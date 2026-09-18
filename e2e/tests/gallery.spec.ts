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

  const rawResponse = page.waitForResponse((response) => {
    const url = new URL(response.url());
    return /\/api\/files\/[^/]+\/raw$/.test(url.pathname) && url.searchParams.get("size") === "1200";
  });
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

test("opens a scanned MP4 in the lightbox", async ({ page, gallery }) => {
  await page.goto(gallery.baseURL);
  const video = page.locator("#gallery [data-lb-type='video']");
  await expect(video).toBeVisible();

  await video.click();

  await expect(page.locator("#lb")).toHaveClass(/on/);
  await expect(page.locator("#lb-vid")).toBeVisible();
  await expect(page.locator("#lb-vid")).toHaveAttribute("src", /\/api\/files\/[^/]+\/raw$/);
  const mediaUrl = await page.locator("#lb-vid").getAttribute("src");
  expect(mediaUrl).not.toBeNull();
  const response = await page.request.get(new URL(mediaUrl!, gallery.baseURL).toString());
  expect(response.status()).toBeGreaterThanOrEqual(200);
  expect(response.status()).toBeLessThan(300);
  expect(response.headers()["content-type"]).toMatch(/^video\/mp4/);
});
