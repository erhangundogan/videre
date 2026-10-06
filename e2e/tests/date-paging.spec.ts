import { writeFile } from "node:fs/promises";
import { join } from "node:path";
import { expect, seedTrips, test } from "../support/gallery";

// A day used to load its first 500 files and no more: a Takeout import put
// 3,518 on one day and the rest could not be reached. A day now loads
// routes.date.pageSize files at a time and Show more loads the rest.
test("a day loads a page at a time and Show more reaches every file", async ({ page, isolatedGallery: gallery }) => {
  seedTrips(gallery.libraryRoot);
  await writeFile(
    join(gallery.libraryRoot, ".videre", "gallery.json"),
    JSON.stringify({ routes: { date: { pageSize: 5 }, files: { view: "list" } } })
  );
  const tiles = page.locator("#dateGrid [data-lb-url]");
  const more = page.locator("#date-more");

  await page.goto(`${gallery.baseURL}/date/2020/03/12`);
  // Twelve dated on the day, and one undated file whose mtime falls on it.
  await expect(page.locator(".date-period-count")).toHaveText("13 items");
  await expect(tiles).toHaveCount(5);
  await expect(more).toHaveText("Show more (8 remaining)");
  await more.click();
  await expect(tiles).toHaveCount(10);
  await more.click();
  await expect(tiles).toHaveCount(13);
  await expect(more).toBeHidden();
  const hashes = await page.locator("#dateGrid .card").evaluateAll((cards) =>
    cards.map((c) => (c as HTMLElement).dataset.hash)
  );
  expect(new Set(hashes).size).toBe(13);

  // Another day starts again from its own first page.
  await page.goto(`${gallery.baseURL}/date/2020/01/01`);
  await expect(page.locator(".date-period-count")).toHaveText("13 items");
  await expect(tiles).toHaveCount(5);
  await expect(more).toHaveText("Show more (8 remaining)");

  // The year overview's cards are not files: no button there.
  await page.goto(`${gallery.baseURL}/date`);
  await expect(page.locator("#dateGrid .date-card").first()).toBeVisible();
  await expect(more).toBeHidden();
});

test("the lightbox steps past the last loaded file of a day", async ({ page, isolatedGallery: gallery }) => {
  seedTrips(gallery.libraryRoot);
  await writeFile(
    join(gallery.libraryRoot, ".videre", "gallery.json"),
    JSON.stringify({ routes: { date: { pageSize: 5 }, files: { view: "list" } } })
  );
  const tiles = page.locator("#dateGrid [data-lb-url]");
  await page.goto(`${gallery.baseURL}/date/2020/03/12`);
  await expect(tiles).toHaveCount(5);
  await tiles.nth(4).click();
  await expect(page.locator("#lb-next")).toBeVisible();
  await page.locator("#lb-next").click();
  await expect(tiles).toHaveCount(10);
});
