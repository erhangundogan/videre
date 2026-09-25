import { join } from "node:path";
import { readFile } from "node:fs/promises";
import { expect, test } from "../support/gallery";

// A 512-dim f16 unit vector along `axis`, as the faces table stores embeddings:
// little-endian, 1.0 = 0x3C00.
function unitEmbedding(axis: number): string {
  let hex = "";
  for (let i = 0; i < 512; i++) hex += i === axis ? "003C" : "0000";
  return hex;
}

type Db = { exec(sql: string): void; prepare(sql: string): { run(): void; all(): unknown[] }; close(): void };

async function openLibrary(libraryRoot: string): Promise<Db | undefined> {
  const { DatabaseSync } = await import("node:sqlite").catch(() => ({ DatabaseSync: undefined }));
  if (!DatabaseSync) return undefined;
  const db = new DatabaseSync(join(libraryRoot, ".videre", "hashes.db")) as unknown as Db;
  // The running server writes to this database too, so wait briefly instead of
  // failing on a transient write lock.
  db.exec("PRAGMA busy_timeout = 5000");
  return db;
}

test("learning updates are hidden by default and shown on request", async ({ page, gallery }) => {
  const statusCalls: string[] = [];
  page.on("request", (r) => {
    if (r.url().includes("/api/face-learning/status")) statusCalls.push(r.url());
  });
  await page.goto(`${gallery.baseURL}/people`);
  const select = page.locator("#learning-updates-select");
  await expect(select).toHaveValue("hide");
  await page.waitForTimeout(500);
  await expect(page.locator("#learning-strip")).toBeHidden();
  expect(statusCalls, "no status polling while updates are hidden").toHaveLength(0);

  await select.selectOption("show");
  await expect(page.locator("#learning-strip")).toBeVisible();
  expect(statusCalls.length).toBeGreaterThan(0);

  // Saved for the library: a reload keeps it.
  await expect
    .poll(async () => (await (await page.request.get(`${gallery.baseURL}/api/settings`)).json()).effective.routes.people.learningUpdates)
    .toBe("show");
  await page.reload();
  await expect(page.locator("#learning-updates-select")).toHaveValue("show");
});

test("recluster previews without writing and applies from the toolbar", async ({ page, gallery }) => {
  const db = await openLibrary(gallery.libraryRoot);
  test.skip(!db, "node:sqlite is unavailable on this Node");
  // Four unnamed singletons: three identical faces, which group, and one apart.
  db!.prepare(
    `INSERT INTO faces (hash, bbox, embedding, blur) VALUES
       ('r1', '0,0,100,100', X'${unitEmbedding(0)}', 500.0),
       ('r2', '0,0,100,100', X'${unitEmbedding(0)}', 500.0),
       ('r3', '0,0,100,100', X'${unitEmbedding(0)}', 500.0),
       ('r4', '0,0,100,100', X'${unitEmbedding(1)}', 500.0)`
  ).run();
  const grouped = () =>
    (db!.prepare("SELECT count(*) AS n FROM faces WHERE hash LIKE 'r%' AND cluster_id IS NOT NULL").all()[0] as {
      n: number;
    }).n;

  try {
    await page.goto(`${gallery.baseURL}/people`);
    const row = page.locator("#recluster-row");
    await expect(row).toBeHidden();
    await page.locator("#recluster-toggle").click();
    await expect(row).toBeVisible();
    await expect(page.locator('#recluster-row input[data-param="eps"]')).toHaveValue("0.6");
    await expect(page.locator("#recluster-learning")).toContainText("Learning:");

    await page.locator("#recluster-preview").click();
    await expect(page.locator("#recluster-result")).toContainText("Preview: 1 group from 3 of 4 unnamed faces");
    expect(grouped(), "preview writes nothing").toBe(0);

    await page.locator('#recluster-row input[data-param="min_cluster_size"]').fill("2");
    await page.locator("#recluster-apply").click();
    await expect(page.locator("#recluster-result")).toContainText("Applied: 1 group from 3 of 4 unnamed faces");
    expect(grouped()).toBe(3);
    await expect(page.locator(".cluster-card")).toHaveCount(1);

    // The value that differs from the default is saved for faces and watch.
    const saved = JSON.parse(await readFile(join(gallery.libraryRoot, ".videre", "gallery.json"), "utf8"));
    expect(saved.faces.clustering).toEqual({ min_cluster_size: 2 });

    // The row stays open across a reload and shows the saved value. Its open
    // state is saved after a short quiet period, so wait for it first.
    await expect
      .poll(async () => (await (await page.request.get(`${gallery.baseURL}/api/settings`)).json()).effective.routes.people.reclusterOpen)
      .toBe(true);
    await page.reload();
    await expect(row).toBeVisible();
    await expect(page.locator('#recluster-row input[data-param="min_cluster_size"]')).toHaveValue("2");
  } finally {
    db!.prepare("DELETE FROM faces WHERE hash LIKE 'r%'").run();
    db!.close();
  }
});
