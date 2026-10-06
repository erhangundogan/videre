import { access, readFile } from "node:fs/promises";
import { join } from "node:path";
import { expect, openLibraryDb, test } from "../support/gallery";

// A Google Photos creation beside its original, next to the library's own
// exact pair (first.jpg and second.jpg share bytes).
function addCreation(libraryRoot: string): void {
  const db = openLibraryDb(libraryRoot);
  const add = db.prepare(
    "INSERT INTO file_hashes (path, hash, size_bytes, ext, mime) VALUES (?, ?, 100, 'jpg', 'image/jpeg')"
  );
  add.run(join(libraryRoot, "Çiçek.jpg"), "c".repeat(64));
  add.run(join(libraryRoot, "Çiçek-EFFECTS.jpg"), "e".repeat(64));
  db.close();
}

async function exists(path: string): Promise<boolean> {
  return access(path).then(
    () => true,
    () => false
  );
}

test("Kinds picks the groups the Duplicates page shows, and is remembered", async ({
  page,
  isolatedGallery: gallery
}) => {
  addCreation(gallery.libraryRoot);
  await page.goto(`${gallery.baseURL}/duplicates`);
  const groups = page.locator("#groups-container .group");
  await expect(groups).toHaveCount(2);
  await expect(page.locator("#groups-container")).toContainText("made in Google Photos");

  const kinds = page.locator("#dup-kinds");
  await expect(kinds.locator("input:checked")).toHaveCount(3);
  await kinds.getByLabel("Exact").uncheck();
  await expect(groups).toHaveCount(1);
  await expect(kinds.getByLabel("Exact")).not.toBeChecked();
  await expect(page.locator("#groups-container")).not.toContainText("first.jpg");

  const saved = JSON.parse(await readFile(join(gallery.libraryRoot, ".videre", "gallery.json"), "utf8"));
  expect(saved.routes.duplicates.kinds).toEqual(["resized", "creation"]);
});

test("Trash copies moves one group's copies and keeps its first file", async ({
  page,
  isolatedGallery: gallery
}) => {
  await page.goto(`${gallery.baseURL}/duplicates`);
  const group = page.locator("#groups-container .group").first();
  await expect(group).toContainText("2 copies");
  await expect(page.locator("#dup-actions")).toHaveText("Trash all copies (1)");

  await group.getByRole("button", { name: "Trash copies" }).click();
  const dialog = page.locator("dialog.sel-confirm");
  await expect(dialog).toContainText("Move 1 copy to the Trash?");
  await dialog.locator("[data-no]").click();
  await expect(group).toBeVisible();

  await group.getByRole("button", { name: "Trash copies" }).click();
  await page.locator("dialog.sel-confirm [data-yes]").click();
  await expect(page.locator(".empty-state")).toContainText("No duplicates");
  const left = await Promise.all(
    ["first.jpg", "second.jpg"].map((name) => exists(join(gallery.libraryRoot, name)))
  );
  expect(left.filter(Boolean)).toHaveLength(1);
});
