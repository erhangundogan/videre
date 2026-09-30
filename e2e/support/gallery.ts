import { once } from "node:events";
import { copyFileSync } from "node:fs";
import { access, copyFile, mkdir, mkdtemp, rm, utimes, writeFile } from "node:fs/promises";
import { DatabaseSync } from "node:sqlite";
import { get, request } from "node:http";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { spawn, type ChildProcess } from "node:child_process";
import { expect, test as base } from "@playwright/test";

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const FIXTURES = join(REPO_ROOT, "crates/videre/tests/fixtures");
const READY_TIMEOUT_MS = 15_000;
const STOP_TIMEOUT_MS = 3_000;
const MAX_LOG_BYTES = 16_000;

export type GallerySession = {
  baseURL: string;
  libraryRoot: string;
  stderr: () => string;
};

type ManagedGallery = GallerySession & {
  child: ChildProcess;
  stdout: () => string;
};

function delay(milliseconds: number): Promise<void> {
  return new Promise((resolveDelay) => setTimeout(resolveDelay, milliseconds));
}

function logBuffer(stream: NodeJS.ReadableStream): () => string {
  let text = "";
  stream.setEncoding("utf8");
  stream.on("data", (chunk: string) => {
    text = `${text}${chunk}`.slice(-MAX_LOG_BYTES);
  });
  return () => text;
}

async function binaryPath(): Promise<string> {
  const binary = process.env.VIDERE_E2E_BIN ?? join(REPO_ROOT, "target/debug/videre");
  try {
    await access(binary);
  } catch {
    throw new Error(`videre E2E binary not found at ${binary}; run make build-dev first`);
  }
  return binary;
}

async function run(binary: string, args: string[], env?: NodeJS.ProcessEnv): Promise<void> {
  // stdin is a pipe in a spawned child, and `videre mark` reads targets from
  // stdin whenever stdin is not a terminal, blocking forever on an open pipe.
  // /dev/null stdin reads as EOF immediately, so the --path flags win.
  const child = spawn(binary, args, { stdio: ["ignore", "pipe", "pipe"], env });
  const stdout = logBuffer(child.stdout);
  const stderr = logBuffer(child.stderr);
  const [code] = await once(child, "exit") as [number | null];
  if (code !== 0) {
    throw new Error(`videre ${args.join(" ")} exited ${code}\nstdout:\n${stdout()}\nstderr:\n${stderr()}`);
  }
}

async function freePort(): Promise<number> {
  return new Promise((resolvePort, reject) => {
    const listener = createServer();
    listener.once("error", reject);
    listener.listen(0, "127.0.0.1", () => {
      const address = listener.address();
      if (!address || typeof address === "string") {
        listener.close();
        reject(new Error("could not reserve a loopback port for gallery"));
        return;
      }
      listener.close((error) => error ? reject(error) : resolvePort(address.port));
    });
  });
}

async function respondsOk(url: string): Promise<boolean> {
  return new Promise((resolveResponse) => {
    const request = get(url, (response) => {
      response.resume();
      resolveResponse(response.statusCode === 200);
    });
    request.setTimeout(1_000, () => request.destroy());
    request.once("error", () => resolveResponse(false));
  });
}

async function requestShutdown(baseURL: string): Promise<void> {
  await new Promise<void>((resolveRequest) => {
    const shutdown = request(`${baseURL}/api/quit`, { method: "POST" }, (response) => {
      response.resume();
      resolveRequest();
    });
    shutdown.setTimeout(1_000, () => shutdown.destroy());
    shutdown.once("error", resolveRequest);
    shutdown.end();
  });
}

async function waitForReady(session: ManagedGallery): Promise<void> {
  const deadline = Date.now() + READY_TIMEOUT_MS;
  const endpoint = `${session.baseURL}/api/files?view=all&limit=1`;
  while (Date.now() < deadline) {
    if (await respondsOk(endpoint)) return;
    if (session.child.exitCode !== null) {
      throw new Error(`gallery exited ${session.child.exitCode} before readiness\nstdout:\n${session.stdout()}\nstderr:\n${session.stderr()}`);
    }
    await delay(100);
  }
  throw new Error(`gallery did not become ready at ${endpoint}\nstdout:\n${session.stdout()}\nstderr:\n${session.stderr()}`);
}

async function waitForExit(child: ChildProcess): Promise<void> {
  if (child.exitCode !== null) return;
  await once(child, "exit");
}

async function stopGallery(session: ManagedGallery): Promise<void> {
  if (session.child.exitCode === null) {
    await requestShutdown(session.baseURL);
    await Promise.race([waitForExit(session.child), delay(STOP_TIMEOUT_MS)]);
  }
  if (session.child.exitCode === null) {
    session.child.kill("SIGKILL");
    await waitForExit(session.child);
  }
  await rm(session.libraryRoot, { recursive: true, force: true });
}

type SeedFile = {
  name: string;
  fixture: string;
  mtime?: string;
  mark?: string[];
};

// `prepare` runs after the scan and marks and before the server starts, for
// state the server reads only at startup (an embeddings database).
async function startGalleryWith(
  seed: SeedFile[],
  prepare?: (libraryRoot: string) => Promise<void>
): Promise<ManagedGallery> {
  const binary = await binaryPath();
  const libraryRoot = await mkdtemp(join(tmpdir(), "videre-e2e-"));
  // Isolate the cache: videre derives its cache (thumbnails and the shared geo
  // cache, including the basemap archive) from HOME/.cache. Pointing HOME at a
  // per-run temp directory keeps the suite off the developer/CI machine cache,
  // so a test never reads a pre-existing basemap or writes one, and results do
  // not depend on the machine's cache state.
  const home = join(libraryRoot, "home");
  await mkdir(home, { recursive: true });
  const env = { ...process.env, HOME: home };
  try {
    for (const file of seed) {
      const target = join(libraryRoot, file.name);
      await copyFile(join(FIXTURES, file.fixture), target);
      if (file.mtime) {
        const when = new Date(file.mtime);
        await utimes(target, when, when);
      }
    }
    await run(binary, ["--library", libraryRoot, "scan", "--silent"], env);
    // The suite checks the gallery alone: without this, every server would
    // start a videre watch beside it (that has its own Rust tests).
    await run(binary, ["--library", libraryRoot, "config", "set", "gallery-starts-watch", "false"], env);
    for (const file of seed) {
      if (file.mark) {
        await run(binary, ["--library", libraryRoot, "mark", ...file.mark], env);
      }
    }
    if (prepare) {
      await prepare(libraryRoot);
    }

    const port = await freePort();
    const child = spawn(binary, ["--library", libraryRoot, "gallery", "--port", String(port)], {
      stdio: "pipe",
      env
    });
    const stderr = logBuffer(child.stderr);
    const stdout = logBuffer(child.stdout);
    const session: ManagedGallery = {
      baseURL: `http://127.0.0.1:${port}`,
      libraryRoot,
      child,
      stdout,
      stderr
    };
    await waitForReady(session);
    return session;
  } catch (error) {
    await rm(libraryRoot, { recursive: true, force: true });
    throw error;
  }
}

async function startGallery(): Promise<ManagedGallery> {
  return startGalleryWith([
    { name: "first.jpg", fixture: "tiny.jpg" },
    { name: "second.jpg", fixture: "tiny.jpg" },
    { name: "clip.mp4", fixture: "red_1s.mp4" }
  ]);
}

// IEEE 754 half precision, little-endian: how videre stores a vector.
export function f16Bytes(values: number[]): Uint8Array {
  const out = new Uint8Array(values.length * 2);
  const f32 = new Float32Array(1);
  const u32 = new Uint32Array(f32.buffer);
  values.forEach((value, i) => {
    f32[0] = value;
    const bits = u32[0];
    const sign = (bits >>> 16) & 0x8000;
    const exponent = ((bits >>> 23) & 0xff) - 127 + 15;
    const mantissa = (bits >>> 13) & 0x3ff;
    const half = value === 0 ? sign : exponent <= 0 ? sign : sign | (exponent << 10) | mantissa;
    out[i * 2] = half & 0xff;
    out[i * 2 + 1] = half >>> 8;
  });
  return out;
}

// Vectors for the search library, by file name: çiçek is the example, bahçe
// is close to it and deniz is far, so a similarity ranking has one right
// answer and no model is needed to produce it.
const SEARCH_VECTORS: Record<string, number[]> = {
  "çiçek.jpg": [1, 0, 0, 0],
  "bahçe.jpg": [0.9, 0.1, 0, 0],
  "deniz.jpg": [0, 1, 0, 0]
};

async function seedSearchEmbeddings(libraryRoot: string): Promise<void> {
  const { DatabaseSync } = await import("node:sqlite");
  const library = new DatabaseSync(join(libraryRoot, ".videre", "hashes.db"), { readOnly: true });
  const rows = library.prepare("SELECT path, hash FROM file_hashes").all() as { path: string; hash: string }[];
  library.close();
  const model = "google/siglip-base-patch16-224";
  await mkdir(join(libraryRoot, ".videre", "embeddings"), { recursive: true });
  const store = new DatabaseSync(join(libraryRoot, ".videre", "embeddings", "google--siglip-base-patch16-224.db"));
  store.exec(
    "CREATE TABLE IF NOT EXISTS embeddings (hash TEXT PRIMARY KEY, model_id TEXT NOT NULL, embedding BLOB NOT NULL)"
  );
  const insert = store.prepare("INSERT OR REPLACE INTO embeddings VALUES (?, ?, ?)");
  for (const row of rows) {
    const vector = SEARCH_VECTORS[row.path.split("/").pop() ?? ""];
    if (vector) insert.run(row.hash, model, f16Bytes(vector));
  }
  store.close();
}

// Save List as the library's view, for specs that count list-view `.card`s.
// Written to the settings file directly, the way a hand edit would be, so the
// next page load picks it up.
// Seeding for pages that show the shared empty state until the library has
// what they are about: a place for Map, a face for People, trips for Events.
// Each writes straight to the running library's database.
export function openLibraryDb(libraryRoot: string): DatabaseSync {
  const db = new DatabaseSync(join(libraryRoot, ".videre", "hashes.db"));
  db.exec("PRAGMA busy_timeout = 5000");
  return db;
}

// One place, Berlin, holding every file in the library.
export function seedPlace(libraryRoot: string): void {
  const db = openLibraryDb(libraryRoot);
  db.exec(
    "DELETE FROM location_clusters;" +
    "INSERT INTO location_clusters " +
      "(id, centroid_lat, centroid_lon, name, photo_count, radius_km, created_at) " +
      "VALUES (1, 52.52, 13.405, 'Berlin', 1, 20.0, CURRENT_TIMESTAMP);" +
    "UPDATE file_hashes SET location_cluster_id = 1, gps_lat = 52.52, gps_lon = 13.405;"
  );
  db.close();
}

// One unnamed face, with a unit embedding along the first axis.
export function seedFace(libraryRoot: string): void {
  const db = openLibraryDb(libraryRoot);
  db.exec("INSERT INTO faces (hash, bbox, embedding, blur) VALUES " +
    "('seeded-face', '0,0,100,100', X'" + "003C".padEnd(2048, "0") + "', 500.0)");
  db.close();
}

export type SeedFace = { hash: string; vector: number[]; side?: number; blur?: number; label?: string };

// Faces at chosen angles: `vector` fills the first components of a 512-dim
// embedding, so a test decides exactly how alike any two faces are. A `label`
// makes the face confirmed and named.
export function seedFaces(libraryRoot: string, faces: SeedFace[]): void {
  const db = openLibraryDb(libraryRoot);
  const insert = db.prepare(
    "INSERT INTO faces (hash, bbox, embedding, blur, confirmed, person_label) VALUES (?, ?, ?, ?, ?, ?)");
  const person = db.prepare("INSERT OR IGNORE INTO people (name, full_name) VALUES (?, ?)");
  for (const f of faces) {
    if (f.label) person.run(f.label, f.label);
    const v = new Array(512).fill(0);
    f.vector.forEach((x, i) => (v[i] = x));
    const side = f.side ?? 100;
    insert.run(f.hash, `0,0,${side},${side}`, f16Bytes(v), f.blur ?? 500, f.label ? 1 : 0, f.label ?? null);
  }
  db.close();
}

// Files that make exactly one trip, to Budapest in March 2020, against a
// Berlin home base; the rest are undated or unlocated on purpose.
export function seedTrips(root: string): void {
  const db = openLibraryDb(root);
  const add = db.prepare("INSERT INTO file_hashes " +
    "(path,hash,size_bytes,ext,mime,exif_date,modified_at,gps_lat,gps_lon) " +
    "VALUES (?,?,?,?,?,?,?,?,?)");
  const put = (n: number, ext: string, date: string | null,
               lat: number | null, lon: number | null) => {
    const path = join(root, "events-" + n + "." + ext);
    copyFileSync(join(FIXTURES, ext === "mp4" ? "red_1s.mp4" : "tiny.jpg"), path);
    add.run(path, n.toString(16).padStart(64, "0"), 100, ext,
      ext === "mp4" ? "video/mp4" : "image/jpeg", date,
      "2020-03-12T11:00:00+00:00", lat, lon);
  };
  for (let i = 0; i < 13; i++) put(i + 1, "jpg",
    "2020-01-01T08:" + String(i).padStart(2, "0") + ":00", 52.52, 13.405);
  put(101, "jpg", "2020-03-12T10:00:00", 47.4979, 19.0402);
  put(102, "jpg", "2020-03-12T11:00:00", 47.4980, 19.0410);
  put(103, "jpg", "2020-03-12T12:00:00", 47.4981, 19.0420);
  for (let i = 0; i < 8; i++) put(104 + i, "jpg",
    "2020-03-12T10:" + String(10 + i * 5).padStart(2, "0") + ":00", null, null);
  put(112, "mp4", "2020-03-12T11:40:00", null, null);
  put(113, "jpg", null, null, null);
  db.close();
}

export async function preferListView(session: GallerySession): Promise<void> {
  await mkdir(join(session.libraryRoot, ".videre"), { recursive: true });
  await writeFile(
    join(session.libraryRoot, ".videre", "gallery.json"),
    JSON.stringify({ routes: { files: { view: "list" } } })
  );
}

// Libraries are started once per worker (starting one is the slow part), but
// each test starts from default gallery settings: the gallery saves choices
// such as the view, the sort and the last page into the library's
// `.videre/gallery.json`, so without clearing it one test's clicks would
// become the next test's starting state.
async function withDefaultSettings(session: GallerySession): Promise<GallerySession> {
  await rm(join(session.libraryRoot, ".videre", "gallery.json"), { force: true });
  return session;
}

export const test = base.extend<
  {
    gallery: GallerySession;
    sortedGallery: GallerySession;
    isolatedGallery: GallerySession;
    searchGallery: GallerySession;
    emptyGallery: GallerySession;
  },
  { galleryServer: GallerySession; sortedGalleryServer: GallerySession; searchGalleryServer: GallerySession }
>({
  searchGalleryServer: [async ({}, use) => {
    // Three distinct images (marks and vectors are keyed by content) with
    // Turkish names, plus stored vectors, so the search box and Similar are on.
    const session = await startGalleryWith(
      [
        { name: "çiçek.jpg", fixture: "tiny.jpg" },
        { name: "bahçe.jpg", fixture: "ai-generated-couple.jpg" },
        { name: "deniz.jpg", fixture: "sample_with_exif.jpg" }
      ],
      seedSearchEmbeddings
    );
    try {
      await use(session);
    } finally {
      await stopGallery(session);
    }
  }, { scope: "worker" }],
  searchGallery: async ({ searchGalleryServer }, use) => {
    await use(await withDefaultSettings(searchGalleryServer));
  },
  galleryServer: [async ({}, use) => {
    const session = await startGallery();
    try {
      await use(session);
    } finally {
      await stopGallery(session);
    }
  }, { scope: "worker" }],
  sortedGalleryServer: [async ({}, use) => {
    // Path order is a, b, c. Every other sort picks a different first file:
    // the clip has the newest date (mtime 2022), b has no EXIF so its mtime
    // (2021-06) is its date, and a carries the fixture's 2021-08-10 EXIF. The
    // three files must be three distinct images: marks are keyed by the
    // content key, so byte-identical copies would share one mark row. Sizes:
    // b (1.2 MB) > a (3 KB) > c (2 KB). a is rated 5, b is rated 3, c is the
    // liked one and the only video.
    const session = await startGalleryWith([
      { name: "a_alpha.jpg", fixture: "tiny.jpg", mark: ["--path", "a_alpha.jpg", "--rating", "5"] },
      {
        name: "b_beta.jpg",
        fixture: "ai-generated-couple.jpg",
        mtime: "2021-06-01T00:00:00Z",
        mark: ["--path", "b_beta.jpg", "--rating", "3"]
      },
      { name: "c_clip.mp4", fixture: "red_1s.mp4", mtime: "2022-05-01T00:00:00Z", mark: ["--path", "c_clip.mp4", "--like"] }
    ]);
    try {
      await use(session);
    } finally {
      await stopGallery(session);
    }
  }, { scope: "worker" }],
  gallery: async ({ galleryServer }, use) => {
    await use(await withDefaultSettings(galleryServer));
  },
  sortedGallery: async ({ sortedGalleryServer }, use) => {
    await use(await withDefaultSettings(sortedGalleryServer));
  },
  // A scanned library with no files at all, for the empty states.
  emptyGallery: async ({}, use) => {
    const session = await startGalleryWith([]);
    try {
      await use(session);
    } finally {
      await stopGallery(session);
    }
  },
  isolatedGallery: async ({}, use) => {
    const session = await startGallery();
    try {
      await use(session);
    } finally {
      await stopGallery(session);
    }
  }
});

export { expect };
