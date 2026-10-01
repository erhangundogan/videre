import { DatabaseSync } from "node:sqlite";
import { join } from "node:path";
import { expect, seedFaces, test } from "../support/gallery";

// The gallery fixture: first.jpg and second.jpg (one image) and clip.mp4.

function applied(page: import("@playwright/test").Page): string | null {
  return new URL(page.url()).searchParams.get("q");
}

function panel(page: import("@playwright/test").Page) {
  return page.locator("#qbox-options");
}

test("the caret opens one column per key, filled from the library", async ({ page, gallery }) => {
  await page.goto(gallery.baseURL);
  await expect(panel(page)).toBeHidden();
  await page.locator("#qbox-caret").click();
  await expect(panel(page)).toBeVisible();
  for (const title of ["People", "Date", "Place", "Tags", "Rating and marks", "Type", "Category", "Has"]) {
    await expect(panel(page).locator(".qopt-col h3", { hasText: title })).toHaveCount(1);
  }
  const type = panel(page).locator(".qopt-group[data-key=type]");
  await expect(type.locator(".qopt-val")).toHaveText(["image", "video"]);
});

test("choosing values writes chips, and Apply runs the query", async ({ page, gallery }) => {
  await page.goto(gallery.baseURL);
  await page.locator("#qbox-caret").click();
  const ext = panel(page).locator(".qopt-group[data-key=ext]");
  await ext.locator(".qopt-val", { hasText: "jpg" }).click();
  await ext.locator(".qopt-val", { hasText: "mp4" }).click();
  // Two values of a key a file has one of: either.
  await expect(page.locator(".qbox .qchip")).toHaveCount(1);
  await expect(page.locator(".qbox .qchip .qchip-mode")).toHaveText("any");
  await expect(ext.locator(".qopt-val[aria-pressed=true]")).toHaveCount(2);

  // Choosing again takes a value back out.
  await ext.locator(".qopt-val", { hasText: "mp4" }).click();
  await expect(ext.locator(".qopt-val[aria-pressed=true]")).toHaveCount(1);

  await panel(page).locator("#qbox-apply").click();
  await expect.poll(() => applied(page)).toBe("ext:jpg");
  await expect(page.locator("#query-status")).toContainText("2 of 3");
});

test("the date range writes after: and before:", async ({ page, gallery }) => {
  await page.goto(gallery.baseURL);
  await page.locator("#qbox-caret").click();
  await panel(page).locator("#qopt-after").fill("2001-02-03");
  await panel(page).locator("#qopt-before").fill("2030-01-01");
  await panel(page).locator("#qbox-apply").click();
  await expect.poll(() => applied(page)).toBe("after:2001-02-03 before:2030-01-01");
});

test("the panel shows what the query already holds, and people by name", async ({ page, isolatedGallery: gallery }) => {
  const db = new DatabaseSync(join(gallery.libraryRoot, ".videre", "hashes.db"), { readOnly: true });
  const { hash } = db.prepare("SELECT hash FROM file_hashes WHERE path LIKE '%/clip.mp4'").get() as { hash: string };
  db.close();
  seedFaces(gallery.libraryRoot, [{ hash, vector: [1], label: "ayse" }]);

  await page.goto(`${gallery.baseURL}/?q=${encodeURIComponent("type:video")}`);
  await page.locator("#qbox-caret").click();
  await expect(panel(page).locator(".qopt-group[data-key=type] .qopt-val[aria-pressed=true]")).toHaveText(["video"]);
  const people = panel(page).locator(".qopt-group[data-key=person] .qopt-val");
  await expect(people).toHaveCount(1);
  await expect(people.first().locator("img")).toHaveCount(1);
  await page.keyboard.press("Escape");
  await expect(panel(page)).toBeHidden();
});
