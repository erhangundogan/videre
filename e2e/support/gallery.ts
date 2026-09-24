import { once } from "node:events";
import { access, copyFile, mkdir, mkdtemp, rm, utimes, writeFile } from "node:fs/promises";
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

async function startGalleryWith(seed: SeedFile[]): Promise<ManagedGallery> {
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
    for (const file of seed) {
      if (file.mark) {
        await run(binary, ["--library", libraryRoot, "mark", ...file.mark], env);
      }
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

// Save List as the library's view, for specs that count list-view `.card`s.
// Written to the settings file directly, the way a hand edit would be, so the
// next page load picks it up.
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
  { gallery: GallerySession; sortedGallery: GallerySession; isolatedGallery: GallerySession },
  { galleryServer: GallerySession; sortedGalleryServer: GallerySession }
>({
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
