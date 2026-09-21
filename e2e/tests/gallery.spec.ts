import { expect, test } from "../support/gallery";

test("loads a scanned local library in Chromium", async ({ page, gallery }) => {
  await page.goto(gallery.baseURL);
  await expect(page.locator("#gallery [data-lb-url]").first()).toBeVisible();
  await expect(page.locator("#gallery")).not.toHaveClass(/tile-mode/);
});

test("renders and expands the duplicate-review route", async ({ page, gallery }) => {
  await page.goto(`${gallery.baseURL}/duplicates`);
  await expect(page.locator(".secnav a[href='/duplicates']")).toHaveClass(/on/);
  const group = page.locator("#groups-container .group").first();
  await expect(group).toBeVisible();

  await group.locator(".group-header").click();
  await expect(group.locator(".group-body tr").first()).toBeVisible();
});

test("loads the date drill-down route", async ({ page, gallery }) => {
  await page.goto(`${gallery.baseURL}/date`);
  await expect(page.locator(".secnav a[href='/date']")).toHaveClass(/on/);
  await expect(page.getByRole("heading", { name: "Browse by date" })).toBeVisible();
  await expect(page.locator("#dateGrid .date-card").first()).toBeVisible();
});

test("loads a direct date drill-down URL", async ({ page, gallery }) => {
  await page.goto(`${gallery.baseURL}/date/2021/08/10`);
  await expect(page.locator(".secnav a[href='/date']")).toHaveClass(/on/);
  await expect(page.locator("#dateBreadcrumb")).toContainText("2021-08-10");
  await expect(page.locator("#dateGrid [data-lb-url]").first()).toBeVisible();
});

test("loads the People route without face data", async ({ page, gallery }) => {
  await page.goto(`${gallery.baseURL}/people`);
  await expect(page.locator(".secnav a[href='/people']")).toHaveClass(/on/);
  await expect(page.getByRole("heading", { name: "No faces detected yet" })).toBeVisible();
  await expect(page.getByText("Run videre faces to detect and group them")).toBeVisible();
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

  await page.locator(".view-mode-select").first().selectOption("list");
  await expect(page.locator("#gallery")).not.toHaveClass(/tile-mode/);
  await expect(page.locator("#gallery .card").first()).toBeVisible();
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

test("closes the lightbox with Escape", async ({ page, gallery }) => {
  await page.goto(gallery.baseURL);
  await page.locator("#gallery [data-lb-type='image']").first().click();
  await expect(page.locator("#lb")).toHaveClass(/on/);

  await page.keyboard.press("Escape");

  await expect(page.locator("#lb")).not.toHaveClass(/on/);
  await expect(page.locator("#lb-img")).toHaveAttribute("src", "");
});

test("closes the lightbox from the close button and from a click outside", async ({ page, gallery }) => {
  await page.goto(gallery.baseURL);

  // The close button closes it.
  await page.locator("#gallery [data-lb-type='image']").first().click();
  await expect(page.locator("#lb")).toHaveClass(/on/);
  await page.locator("#lb-close").click();
  await expect(page.locator("#lb")).not.toHaveClass(/on/);

  // Clicking outside the image (on the overlay) also closes it.
  await page.locator("#gallery [data-lb-type='image']").first().click();
  await expect(page.locator("#lb")).toHaveClass(/on/);
  // Click the overlay itself, away from the stage, at the top-left corner.
  await page.locator("#lb").click({ position: { x: 5, y: 5 } });
  await expect(page.locator("#lb")).not.toHaveClass(/on/);
});

test("the fullscreen and close controls sit at the image's top right", async ({ page, gallery }) => {
  await page.goto(gallery.baseURL);
  await page.locator("#gallery [data-lb-type='image']").first().click();
  await expect(page.locator("#lb-img")).toBeVisible();

  const fs = page.locator("#lb-fs");
  const close = page.locator("#lb-close");
  await expect(fs).toBeVisible();
  await expect(close).toBeVisible();

  const stage = await page.locator(".lb-stage").boundingBox();
  const controls = await page.locator(".lb-controls").boundingBox();
  const fsBox = await fs.boundingBox();
  const closeBox = await close.boundingBox();
  if (!stage || !controls || !fsBox || !closeBox) throw new Error("missing bounding boxes");

  // Anchored to the image stage, not the viewport: the controls hug the stage's
  // top-right corner rather than the screen corner.
  expect(controls.x + controls.width).toBeGreaterThan(stage.x + stage.width - 24);
  expect(controls.y).toBeLessThan(stage.y + 24);
  // Fullscreen and close are side by side, fullscreen first.
  expect(fsBox.x).toBeLessThan(closeBox.x);
  expect(Math.abs(fsBox.y - closeBox.y)).toBeLessThan(2);
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
