import { copyFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { expect, openLibraryDb, test } from "../support/gallery";

// Three more duplicate groups of two, so the page has four. The rows share a
// fixture photo's bytes, so their thumbnails render.
async function moreGroups(libraryRoot: string): Promise<void> {
  const db = openLibraryDb(libraryRoot);
  const { path: source } = db
    .prepare("SELECT path FROM file_hashes WHERE lower(ext) = 'jpg' ORDER BY path LIMIT 1")
    .get() as { path: string };
  const add = db.prepare("INSERT INTO file_hashes (path, hash, size_bytes, ext, mime) VALUES (?, ?, 100, 'jpg', 'image/jpeg')");
  for (let g = 0; g < 3; g++) {
    for (const copy of ["a", "b"]) {
      const path = join(libraryRoot, `kopya-${g}-${copy}.jpg`);
      await copyFile(source, path);
      add.run(path, `${g + 1}`.padStart(64, "d"));
    }
  }
  db.close();
}

test("Duplicates pages its groups and keeps images lazy when they open", async ({ page, isolatedGallery: gallery }) => {
  await moreGroups(gallery.libraryRoot);
  await writeFile(
    join(gallery.libraryRoot, ".videre", "gallery.json"),
    JSON.stringify({ routes: { duplicates: { pageSize: 2 } } })
  );
  const groups = page.locator("#groups-container .group");
  await page.goto(`${gallery.baseURL}/duplicates`);
  await expect(groups).toHaveCount(2);
  await expect(page.locator("#more-btn")).toHaveText("Show more (2 remaining)");
  await page.locator("#more-btn").click();
  await expect(groups).toHaveCount(4);

  // Opening one group, then all of them, leaves every image to load when it
  // nears the screen: making them eager requested every thumbnail at once.
  await groups.first().locator(".group-header").click();
  await page.getByRole("button", { name: "Expand all" }).click();
  await expect(groups.first()).toHaveClass(/open/);
  const loading = await page
    .locator("#groups-container .group img")
    .evaluateAll((imgs) => imgs.map((i) => (i as HTMLImageElement).loading));
  expect(loading.length).toBeGreaterThan(0);
  expect(loading.every((l) => l === "lazy")).toBe(true);
});
