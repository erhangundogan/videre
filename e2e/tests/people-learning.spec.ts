import { DatabaseSync } from "node:sqlite";
import { join } from "node:path";
import type { Page } from "@playwright/test";
import { expect, test, type GallerySession } from "../support/gallery";

// Each test gets its own library (isolatedGallery): teaching writes journal
// rows that no cleanup could take back out of a shared one.

async function setLearning(page: Page, gallery: GallerySession, faces: object): Promise<void> {
  const r = await page.request.patch(`${gallery.baseURL}/api/settings`, {
    headers: { "content-type": "application/merge-patch+json" },
    data: JSON.stringify({ faces })
  });
  expect(r.ok()).toBeTruthy();
}

// One named face for Ayşe and one unassigned face, on a scanned photo, both
// with a real two-dimensional f16 embedding (1, 0) so teaching can extract
// membership features.
function seedFaces(gallery: GallerySession): void {
  const db = new DatabaseSync(join(gallery.libraryRoot, ".videre", "hashes.db"));
  db.exec("PRAGMA busy_timeout = 5000");
  const { hash } = db.prepare("SELECT hash FROM file_hashes WHERE path LIKE '%first.jpg'").get() as { hash: string };
  db.prepare("INSERT OR IGNORE INTO people (name, full_name) VALUES ('ayse', 'Ayşe')").run();
  db.prepare(
    `INSERT INTO faces (hash, bbox, embedding, person_label, confirmed)
     VALUES (?, '0,0,50,50', X'003C0000', 'ayse', 1), (?, '60,0,50,50', X'003C0000', NULL, 0)`
  ).run(hash, hash);
  db.close();
}

function unassignedFaceId(gallery: GallerySession): number {
  const db = new DatabaseSync(join(gallery.libraryRoot, ".videre", "hashes.db"), { readOnly: true });
  const row = db.prepare("SELECT id FROM faces WHERE person_label IS NULL").get() as { id: number };
  db.close();
  return row.id;
}

test("learning without updates asks its questions but shows no status strip", async ({
  page,
  isolatedGallery: gallery
}) => {
  await setLearning(page, gallery, { learning: true, learningUpdates: false });
  const calls: string[] = [];
  page.on("request", (r) => {
    if (r.url().includes("/api/face-learning/")) calls.push(new URL(r.url()).pathname);
  });
  await page.goto(`${gallery.baseURL}/people`);
  await expect.poll(() => calls).toContain("/api/face-learning/questions");
  await page.waitForTimeout(300);
  await expect(page.locator("#learning-strip")).toBeHidden();
  expect(calls, "the status strip is not polled").not.toContain("/api/face-learning/status");
});

test("a teaching action appears in the person's learning history", async ({ page, isolatedGallery: gallery }) => {
  seedFaces(gallery);
  await setLearning(page, gallery, { learning: true, learningUpdates: true });

  const face = unassignedFaceId(gallery);
  const r = await page.request.put(`${gallery.baseURL}/api/people/ayse/faces`, {
    data: { face_ids: [face] }
  });
  expect(r.ok(), await r.text()).toBeTruthy();
  const ack = await r.json();
  expect(ack.event_ids.length).toBeGreaterThan(0);

  await page.goto(`${gallery.baseURL}/people/person/ayse`);
  const history = page.locator("#learning-history");
  await expect(history).toBeVisible();
  const entry = history.locator(".learning-event").first();
  await expect(entry.locator("summary")).toContainText("assign_face");
  await entry.locator("summary").click();
  await expect(entry.locator(".learning-event-proof a")).toHaveAttribute(
    "href",
    `/api/face-learning/events/${ack.event_ids[0]}`
  );
});

test("with learning off the person page shows no history and asks nothing", async ({
  page,
  isolatedGallery: gallery
}) => {
  seedFaces(gallery);
  const calls: string[] = [];
  page.on("request", (r) => {
    if (r.url().includes("/api/face-learning/")) calls.push(r.url());
  });
  await page.goto(`${gallery.baseURL}/people/person/ayse`);
  await expect(page.locator(".person-face, .face-card, img").first()).toBeVisible();
  await page.waitForTimeout(300);
  await expect(page.locator("#learning-history")).toBeHidden();
  expect(calls).toHaveLength(0);
});
