import { copyFileSync } from "node:fs";
import { DatabaseSync } from "node:sqlite";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { expect, test } from "../support/gallery";

const FIXTURES = resolve(dirname(fileURLToPath(import.meta.url)), "../../crates/videre/tests/fixtures");

function seedTripsWithDatabaseSync(root: string): void {
  const db = new DatabaseSync(join(root, ".videre", "hashes.db"));
  db.exec("PRAGMA busy_timeout = 5000");
  const add = db.prepare("INSERT INTO file_hashes " +
    "(path,hash,size_bytes,ext,mime,exif_date,modified_at,gps_lat,gps_lon) " +
    "VALUES (?,?,?,?,?,?,?,?,?)");
  const put = (n: number, ext: string, date: string | null,
               lat: number | null, lon: number | null) => {
    const path = join(root, `events-${n}.${ext}`);
    copyFileSync(join(FIXTURES, ext === "mp4" ? "red_1s.mp4" : "tiny.jpg"), path);
    add.run(path, n.toString(16).padStart(64, "0"), 100, ext,
      ext === "mp4" ? "video/mp4" : "image/jpeg", date,
      "2020-03-12T11:00:00+00:00", lat, lon);
  };
  for (let i = 0; i < 13; i++) put(i + 1, "jpg",
    `2020-01-01T08:${String(i).padStart(2, "0")}:00`, 52.52, 13.405);
  put(101, "jpg", "2020-03-12T10:00:00", 47.4979, 19.0402);
  put(102, "jpg", "2020-03-12T11:00:00", 47.4980, 19.0410);
  put(103, "jpg", "2020-03-12T12:00:00", 47.4981, 19.0420);
  for (let i = 0; i < 8; i++) put(104 + i, "jpg",
    `2020-03-12T10:${String(10 + i * 5).padStart(2, "0")}:00`, null, null);
  put(112, "mp4", "2020-03-12T11:40:00", null, null);
  put(113, "jpg", null, null, null);
  db.close();
}

test("Events explains empty travel evidence, then shows one exact Budapest trip", async ({ page, isolatedGallery: gallery }) => {
  await page.goto(`${gallery.baseURL}/events`);
  await expect(page.locator(".secnav a[href='/events']")).toHaveClass(/on/);
  await expect(page.locator("#dateBreadcrumb")).toHaveText("Events");
  await expect(page.locator("#dateGrid .empty-state")).toContainText("location-supported photos");

  seedTripsWithDatabaseSync(gallery.libraryRoot);
  await page.reload();
  const cards = page.locator("#dateGrid .event-card");
  await expect(cards).toHaveCount(1);
  await expect(cards.first()).toContainText("Budapest, March 2020 Trip");
  await expect(cards.first()).toContainText("12 files");
  const key = `20200312T100000-${(101).toString(16).padStart(64, "0")}`;
  await expect(cards.first().locator("a.date-card-link")).toHaveAttribute("href", `/events/${key}`);
  await cards.first().locator("a.date-card-link").dispatchEvent("click");
  await expect(page).toHaveURL(`${gallery.baseURL}/events/${key}`);
  await expect(page.locator("#dateBreadcrumb")).toContainText("Budapest, March 2020 Trip");
  await expect(page.locator("#dateGrid [data-lb-url]")).toHaveCount(12);

  const response = await page.request.get(`${gallery.baseURL}/api/events/${key}/files`);
  expect(response.status()).toBe(200);
  const payload = await response.json() as { files: Array<{ hash: string }> };
  expect(payload.files.map((f) => f.hash).sort()).toEqual(
    Array.from({ length: 12 }, (_, i) => (101 + i).toString(16).padStart(64, "0")).sort()
  );
});
