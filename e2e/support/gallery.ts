import { once } from "node:events";
import { access, copyFile, mkdtemp, rm } from "node:fs/promises";
import { get, request } from "node:http";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
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
  child: ChildProcessWithoutNullStreams;
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

async function run(binary: string, args: string[]): Promise<void> {
  const child = spawn(binary, args, { stdio: "pipe" });
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

async function waitForExit(child: ChildProcessWithoutNullStreams): Promise<void> {
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

async function startGallery(): Promise<ManagedGallery> {
  const binary = await binaryPath();
  const libraryRoot = await mkdtemp(join(tmpdir(), "videre-e2e-"));
  try {
    await copyFile(join(FIXTURES, "tiny.jpg"), join(libraryRoot, "first.jpg"));
    await copyFile(join(FIXTURES, "tiny.jpg"), join(libraryRoot, "second.jpg"));
    await copyFile(join(FIXTURES, "red_1s.mp4"), join(libraryRoot, "clip.mp4"));
    await run(binary, ["--library", libraryRoot, "scan", "--silent"]);

    const port = await freePort();
    const child = spawn(binary, ["--library", libraryRoot, "gallery", "--port", String(port)], {
      stdio: "pipe"
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

export const test = base.extend<{ gallery: GallerySession }>({
  gallery: [async ({}, use) => {
    const session = await startGallery();
    try {
      await use(session);
    } finally {
      await stopGallery(session);
    }
  }, { scope: "worker" }]
});

export { expect };
