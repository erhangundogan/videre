import { expect, test } from "../support/gallery";

// The gallery fixture holds first.jpg, second.jpg and clip.mp4 and no
// embeddings: a query's filters work without vectors, its words would not.

function gridNames(page: import("@playwright/test").Page) {
  return page.locator("#gallery [data-lb-type]");
}

test("a query in the nav box narrows the Library grid and says N of M", async ({ page, gallery }) => {
  await page.goto(gallery.baseURL);
  await expect(gridNames(page)).toHaveCount(3);

  await page.locator("#nav-search").fill("type:video");
  await page.locator("#nav-search").press("Enter");
  await expect(page).toHaveURL(/\?q=type%3Avideo/);
  await expect(page.locator("#query-status")).toContainText("1 of 3");
  await expect(gridNames(page)).toHaveCount(1);
  await expect(page.locator("#gallery [data-lb-type='video']")).toHaveCount(1);
  // The box keeps what was asked, as a chip that can be edited.
  await expect(page.locator(".qbox .qchip")).toHaveCount(1);
  await expect(page.locator(".qbox .qchip")).toContainText("video");
});

test("NOT and a reload keep the query", async ({ page, gallery }) => {
  await page.goto(`${gallery.baseURL}/?q=${encodeURIComponent("-type:video")}`);
  await expect(page.locator("#query-status")).toContainText("2 of 3");
  await page.reload();
  await expect(page.locator("#query-status")).toContainText("2 of 3");
  await expect(gridNames(page)).toHaveCount(2);
});

test("a bad query says why and where instead of showing everything", async ({ page, gallery }) => {
  await page.goto(`${gallery.baseURL}/?q=${encodeURIComponent("kişi:özgür")}`);
  const status = page.locator("#query-status");
  await expect(status).toHaveClass(/query-error/);
  await expect(status).toContainText("unknown key");
  await expect(gridNames(page)).toHaveCount(0);

  await page.goto(`${gallery.baseURL}/?q=${encodeURIComponent("(type:video")}`);
  await expect(status).toContainText("character 12");
});

test("clearing the query shows the whole library again", async ({ page, gallery }) => {
  await page.goto(`${gallery.baseURL}/?q=${encodeURIComponent("type:video")}`);
  await expect(gridNames(page)).toHaveCount(1);
  await page.locator("#query-status a", { hasText: "clear" }).click();
  await expect(gridNames(page)).toHaveCount(3);
  await expect(page.locator("#query-status")).toHaveCount(0);
});
