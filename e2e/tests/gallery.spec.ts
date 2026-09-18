import { expect, test } from "../support/gallery";

test("loads a scanned local library in Chromium", async ({ page, gallery }) => {
  await page.goto(gallery.baseURL);
  await expect(page.locator("#gallery [data-lb-url]").first()).toBeVisible();
  await expect(page.locator("#gallery")).not.toHaveClass(/tile-mode/);
});

test("persists the tile layout after a reload", async ({ page, gallery }) => {
  await page.goto(gallery.baseURL);
  const viewMode = page.locator(".view-mode-select").first();

  await viewMode.selectOption("tile");
  await expect(page.locator("#gallery")).toHaveClass(/tile-mode/);
  await expect(viewMode).toHaveValue("tile");

  await page.reload();
  await expect(page.locator("#gallery")).toHaveClass(/tile-mode/);
  await expect(page.locator(".view-mode-select").first()).toHaveValue("tile");
});

test("opens an image in the lightbox and navigates to the next item", async ({ page, gallery }) => {
  await page.goto(gallery.baseURL);
  const image = page.locator("#gallery [data-lb-type='image']").first();
  await expect(image).toBeVisible();

  const rawResponse = page.waitForResponse((response) =>
    /\/api\/files\/[^/]+\/raw$/.test(new URL(response.url()).pathname),
  );
  await image.click();
  const response = await rawResponse;

  await expect(page.locator("#lb")).toHaveClass(/on/);
  await expect(page.locator("#lb-img")).toBeVisible();
  expect(response.status()).toBe(200);
  expect(response.headers()["content-type"]).toMatch(/^image\//);

  await page.keyboard.press("ArrowRight");
  await expect(page.locator("#lb")).toHaveClass(/on/);
  await expect(page.locator("#lb-prev")).toBeVisible();
});
