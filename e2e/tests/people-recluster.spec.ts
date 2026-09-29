import { join } from "node:path";
import { readFile } from "node:fs/promises";
import type { Page } from "@playwright/test";
import { expect, openLibraryDb, seedFace, seedFaces, test } from "../support/gallery";

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

test("face learning is off by default and turned on in gallery.json", async ({ page, isolatedGallery: gallery }) => {
  // With no faces the page shows the empty state, strip included.
  seedFace(gallery.libraryRoot);
  const learningCalls: string[] = [];
  page.on("request", (r) => {
    if (r.url().includes("/api/face-learning/")) learningCalls.push(r.url());
  });
  await page.goto(`${gallery.baseURL}/people`);
  // No learning control in the toolbar, in any state.
  await expect(page.locator(".gallery-toolbar").first()).not.toContainText("Learning");
  await page.waitForTimeout(500);
  await expect(page.locator("#learning-strip")).toBeHidden();
  await page.goto(`${gallery.baseURL}/people/person/nobody`);
  await page.waitForTimeout(300);
  expect(learningCalls, "no learning request while learning is off").toHaveLength(0);

  const r = await page.request.patch(`${gallery.baseURL}/api/settings`, {
    headers: { "content-type": "application/merge-patch+json" },
    data: JSON.stringify({ faces: { learning: true, learningUpdates: true } })
  });
  expect(r.ok()).toBeTruthy();
  await page.goto(`${gallery.baseURL}/people`);
  await expect(page.locator("#learning-strip")).toBeVisible();
  expect(learningCalls.length).toBeGreaterThan(0);
  await expect(page.locator(".gallery-toolbar").first()).not.toContainText("Learning");
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
    // Learning is off by default, so the row says nothing about it.
    await expect(page.locator("#recluster-learning")).toHaveText("");

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

// Another process writing beside the gallery (a `videre watch` cycle) must
// stay visible after the gallery opens the library a second time, as a
// recluster preview does. That second open used to read the database header
// through a plain file handle, whose close dropped the gallery's SQLite
// locks; the other process, closing next, took itself for the last user and
// deleted the WAL under the gallery, which then read a frozen snapshot.
test("another process's writes stay visible after a recluster preview", async ({
  page,
  isolatedGallery: gallery
}) => {
  const faces = async () => (await (await page.request.get(`${gallery.baseURL}/api/faces`)).json()).singletons.length;
  const writer = await openLibrary(gallery.libraryRoot);
  writer!.prepare(
    `INSERT INTO faces (hash, bbox, embedding, blur) VALUES
       ('r1', '0,0,100,100', X'${unitEmbedding(0)}', 500.0),
       ('r2', '0,0,100,100', X'${unitEmbedding(0)}', 500.0),
       ('r3', '0,0,100,100', X'${unitEmbedding(0)}', 500.0),
       ('r4', '0,0,100,100', X'${unitEmbedding(1)}', 500.0)`
  ).run();
  expect(await faces()).toBe(4);

  const preview = await page.request.post(`${gallery.baseURL}/api/faces/recluster/preview`, { data: {} });
  expect(preview.ok()).toBeTruthy();
  writer!.close();

  const next = await openLibrary(gallery.libraryRoot);
  next!.prepare("INSERT INTO faces (hash, bbox, embedding) VALUES ('p1', '0,0,50,50', X'0000')").run();
  next!.close();
  expect(await faces(), "the gallery no longer sees other writers").toBe(5);
});

const SHOWN = ["eps", "merge_sim", "attach_sim", "min_cluster_size"];
const MORE = ["min_face_size", "min_blur", "max_generic_sim", "max_landmark_error"];

async function openRow(page: Page, baseURL: string) {
  await page.goto(`${baseURL}/people`);
  await page.locator("#recluster-toggle").click();
  await expect(page.locator("#recluster-row")).toBeVisible();
  await expect(page.locator('#recluster-row input[data-param="eps"]')).not.toHaveValue("");
}

for (const width of [1280, 375]) {
  test(`recluster fields form two aligned columns at ${width}px`, async ({ page, isolatedGallery: gallery }) => {
    await page.setViewportSize({ width, height: 900 });
    seedFaces(gallery.libraryRoot, [{ hash: "a1", vector: [1] }]);
    await openRow(page, gallery.baseURL);
    const params = (sel: string) =>
      page.locator(sel).evaluateAll((els) => els.map((e) => (e as HTMLElement).dataset.param));
    expect(await params("#recluster-row > .recluster-fields input[data-param]")).toEqual(SHOWN);
    const more = page.locator("details.recluster-more");
    await expect(more).not.toHaveAttribute("open", "");
    await more.locator("summary").click();
    expect(await params("details.recluster-more input[data-param]")).toEqual(MORE);

    const lefts = (sel: string) =>
      page.locator(sel).evaluateAll((els) => els.map((e) => Math.round(e.getBoundingClientRect().left)));
    const inputs = await lefts("#recluster-row input[data-param]");
    const titles = await lefts("#recluster-row .recluster-field label");
    expect(new Set(inputs).size, `inputs start at one x: ${inputs}`).toBe(1);
    expect(new Set(titles).size, `titles start at one x: ${titles}`).toBe(1);
    expect(inputs[0]).toBeGreaterThan(titles[0]);
    // Each info icon sits after its title, on its line.
    const rows = page.locator("#recluster-row .recluster-field");
    await expect(rows).toHaveCount(8);
    for (let i = 0; i < 8; i++) {
      const t = await rows.nth(i).locator("label").boundingBox();
      const b = await rows.nth(i).locator("button.param-info").boundingBox();
      expect(b!.x).toBeGreaterThan(t!.x);
      expect(Math.abs(b!.y + b!.height / 2 - (t!.y + t!.height / 2))).toBeLessThan(6);
    }
    // Actions below the last field, and nothing in the row wider than a phone.
    const lastField = await rows.nth(7).boundingBox();
    const actions = await page.locator(".recluster-actions").boundingBox();
    expect(actions!.y).toBeGreaterThanOrEqual(lastField!.y + lastField!.height);
    const overflow = await page.locator("#recluster-row *").evaluateAll((els) =>
      els.filter((e) => e.getBoundingClientRect().right > window.innerWidth).map((e) => e.outerHTML.slice(0, 60)));
    expect(overflow).toEqual([]);
  });
}

test("each recluster value explains itself by click, tap and keyboard", async ({ page, isolatedGallery: gallery }) => {
  seedFaces(gallery.libraryRoot, [{ hash: "a1", vector: [1] }]);
  await openRow(page, gallery.baseURL);
  await page.locator("details.recluster-more summary").click();
  await expect(page.locator("#recluster-row button.param-info")).toHaveCount(8);
  const epsIcon = page.getByRole("button", { name: "About eps" });
  const epsTip = page.locator("#rc-eps-tip");
  await expect(epsTip).toBeHidden();
  await expect(page.locator("#rc-eps")).toHaveAttribute("aria-describedby", "rc-eps-tip");
  // A click (a tap on a phone) opens and pins it, with the default filled in.
  await epsIcon.click();
  await expect(epsTip).toBeVisible();
  await expect(epsIcon).toHaveAttribute("aria-expanded", "true");
  await expect(epsTip).toContainText("Default 0.6.");
  await page.keyboard.press("Escape");
  await page.mouse.move(1, 1); // hovering the icon shows it too
  await expect(epsTip).toBeHidden();
  await expect(epsIcon).toHaveAttribute("aria-expanded", "false");
  // Keyboard focus shows it without a click.
  await page.locator("#rc-eps").focus();
  await page.keyboard.press("Shift+Tab");
  await expect(epsIcon).toBeFocused();
  await expect(epsTip).toBeVisible();
  // Opening another closes the first; a click elsewhere closes the open one.
  const mergeIcon = page.getByRole("button", { name: "About merge" });
  await mergeIcon.click();
  await expect(page.locator("#rc-merge-tip")).toContainText("Default 0.35.");
  await page.getByRole("button", { name: "About attach" }).click();
  await expect(mergeIcon).toHaveAttribute("aria-expanded", "false");
  await expect(page.locator("#rc-merge-tip")).toBeHidden();
  await page.mouse.click(5, 5);
  await expect(page.locator("#rc-attach-tip")).toBeHidden();
  for (const id of ["rc-min-size", "rc-min-face", "rc-sharpness", "rc-generic", "rc-landmark"]) {
    await expect(page.locator(`#${id}-tip .param-default`)).toHaveText(/^Default [\d.]+\.$/);
  }
});

// A number input reports text that is not a number as an empty value, so the
// empty case covers it; Playwright cannot type letters into one.
test("invalid recluster values are refused and nothing runs", async ({ page, isolatedGallery: gallery }) => {
  seedFaces(gallery.libraryRoot, [{ hash: "a1", vector: [1] }]);
  const posts: string[] = [];
  page.on("request", (r) => {
    if (r.method() === "POST" && r.url().includes("/api/faces/recluster")) posts.push(r.url());
  });
  await openRow(page, gallery.baseURL);
  const line = page.locator("#recluster-result");
  const eps = page.locator("#rc-eps");
  const minSize = page.locator("#rc-min-size");

  await eps.fill("");
  await page.locator("#recluster-preview").click();
  await expect(line).toHaveText("eps: enter a number.");
  await expect(eps).toHaveAttribute("aria-invalid", "true");
  await expect(eps).toBeFocused();

  await eps.fill("3");
  await page.locator("#recluster-apply").click();
  await expect(line).toHaveText("eps: between 0 and 2.");

  await eps.fill("0.6");
  await expect(eps).not.toHaveAttribute("aria-invalid", "true");
  await minSize.fill("0");
  await page.locator("#recluster-preview").click();
  await expect(line).toHaveText("min size: 1 or more.");
  await minSize.fill("2.5");
  await page.locator("#recluster-preview").click();
  await expect(line).toHaveText("min size: a whole number.");
  // A bad value under a closed More opens it, so the marked field is seen.
  await minSize.fill("3");
  await page.locator("#rc-sharpness").evaluate((e) => ((e as HTMLInputElement).value = "-1"));
  await expect(page.locator("details.recluster-more")).not.toHaveAttribute("open", "");
  await page.locator("#recluster-preview").click();
  await expect(line).toHaveText("sharpness: 0 or more.");
  await expect(page.locator("#rc-sharpness")).toBeFocused();
  expect(posts, "nothing was posted").toHaveLength(0);

  // The server's own refusal names the field by its title too.
  await page.locator("#rc-sharpness").fill("80");
  await page.route("**/api/faces/recluster/preview", (route) =>
    route.fulfill({ status: 400, contentType: "application/json", body: JSON.stringify({ field: "merge_sim" }) }));
  await page.locator("#recluster-preview").click();
  await expect(line).toHaveText("merge: out of range.");
});

// Each value moves the grouping the way its description says. Faces sit at
// chosen angles in tight groups: [1] and [0.5, 0.866] are 0.5 alike (cosine
// distance 0.5), [0.6, 0, 0.8] is 0.6 alike to [1], and [0, 1] is unrelated.
// Each test changes one value from a stated baseline and reads Preview.
async function preview(page: Page, values: Record<string, string>) {
  await page.locator("details.recluster-more").evaluate((d) => ((d as HTMLDetailsElement).open = true));
  for (const [param, value] of Object.entries(values)) {
    await page.locator(`#recluster-row input[data-param="${param}"]`).fill(value);
  }
  const line = page.locator("#recluster-result");
  const done = page.waitForResponse((r) => r.url().includes("/api/faces/recluster/preview"));
  await page.locator("#recluster-preview").click();
  await done;
  await expect(line).toContainText("Preview:");
  return (await line.textContent())!;
}

const group = (hash: string, vector: number[], n: number) =>
  Array.from({ length: n }, (_, i) => ({ hash: `${hash}${i}`, vector }));

test("lower eps splits look-alikes", async ({ page, isolatedGallery: gallery }) => {
  seedFaces(gallery.libraryRoot, [...group("a", [1], 3), ...group("b", [0.5, 0.866], 3)]);
  await openRow(page, gallery.baseURL);
  const base = { merge_sim: "0.9", attach_sim: "1", min_cluster_size: "3" };
  expect(await preview(page, { ...base, eps: "0.6" })).toContain("Preview: 1 group from 6 of 6");
  expect(await preview(page, { ...base, eps: "0.3" })).toContain("Preview: 2 groups from 6 of 6");
});

test("lower merge rejoins a person split in two", async ({ page, isolatedGallery: gallery }) => {
  seedFaces(gallery.libraryRoot, [...group("a", [1], 3), ...group("b", [0.5, 0.866], 3)]);
  await openRow(page, gallery.baseURL);
  const base = { eps: "0.3", attach_sim: "1", min_cluster_size: "3" };
  expect(await preview(page, { ...base, merge_sim: "0.9" })).toContain("Preview: 2 groups from 6 of 6");
  expect(await preview(page, { ...base, merge_sim: "0.4" })).toContain("Preview: 1 group from 6 of 6");
});

test("attach pulls a leftover in, and 1 turns it off", async ({ page, isolatedGallery: gallery }) => {
  seedFaces(gallery.libraryRoot, [...group("a", [1], 3), { hash: "loner", vector: [0.6, 0, 0.8] }]);
  await openRow(page, gallery.baseURL);
  const base = { eps: "0.3", merge_sim: "0.9", min_cluster_size: "3" };
  expect(await preview(page, { ...base, attach_sim: "1" })).toContain("from 3 of 4 unnamed faces; 1 left single");
  expect(await preview(page, { ...base, attach_sim: "0.5" })).toContain("from 4 of 4 unnamed faces; 0 left single");
});

test("a larger min size leaves more faces single", async ({ page, isolatedGallery: gallery }) => {
  seedFaces(gallery.libraryRoot, [...group("a", [1], 3), ...group("c", [0, 1], 2)]);
  await openRow(page, gallery.baseURL);
  expect(await preview(page, { min_cluster_size: "2" })).toContain("Preview: 2 groups from 5 of 5");
  expect(await preview(page, { min_cluster_size: "3" })).toContain(
    "Preview: 1 group from 3 of 5 unnamed faces; 2 left single"
  );
});

test("the quality gates in More hold out tiny and blurred faces", async ({ page, isolatedGallery: gallery }) => {
  seedFaces(gallery.libraryRoot, [
    ...group("a", [1], 3),
    { hash: "tiny", vector: [1], side: 40 },
    { hash: "soft", vector: [1], blur: 50 }
  ]);
  await openRow(page, gallery.baseURL);
  const base = { attach_sim: "1" };
  expect(await preview(page, { ...base, min_face_size: "80", min_blur: "80" })).toContain(
    "from 3 of 5 unnamed faces; 2 left single (2 held out by quality)"
  );
  expect(await preview(page, { ...base, min_face_size: "30", min_blur: "80" })).toContain(
    "from 4 of 5 unnamed faces; 1 left single (1 held out by quality)"
  );
  expect(await preview(page, { ...base, min_face_size: "80", min_blur: "40" })).toContain(
    "from 4 of 5 unnamed faces; 1 left single (1 held out by quality)"
  );
});
