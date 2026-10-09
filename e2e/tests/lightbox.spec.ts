import { mkdir, writeFile } from "node:fs/promises";
import { join } from "node:path";
import type { Page } from "@playwright/test";
import { expect, test, type GallerySession } from "../support/gallery";

// The sorted library's default order is date descending: c_clip.mp4 (2022),
// a_alpha.jpg (2021-08 EXIF), b_beta.jpg (2021-06 mtime).

async function writeSettings(gallery: GallerySession, settings: object): Promise<void> {
  await mkdir(join(gallery.libraryRoot, ".videre"), { recursive: true });
  await writeFile(join(gallery.libraryRoot, ".videre", "gallery.json"), JSON.stringify(settings));
}

function shownName(page: Page) {
  return page.locator("#lbMeta .lb-fname span");
}

async function openFirst(page: Page): Promise<void> {
  const first = page.locator("#gallery [data-lb-url]").first();
  await expect(first).toBeVisible();
  await first.click();
  await expect(page.locator("#lb")).toHaveClass(/on/);
}

test("the arrows and buttons walk the grid and stop at both ends", async ({ page, sortedGallery }) => {
  await page.goto(sortedGallery.baseURL);
  await openFirst(page);
  await expect(shownName(page)).toHaveText("c_clip.mp4");
  await expect(page.locator("#lb-prev")).toBeHidden();
  await expect(page.locator("#lb-next")).toBeVisible();

  // Before the first item there is nothing to step to.
  await page.keyboard.press("ArrowLeft");
  await expect(shownName(page)).toHaveText("c_clip.mp4");

  await page.keyboard.press("ArrowRight");
  await expect(shownName(page)).toHaveText("a_alpha.jpg");
  await expect(page.locator("#lb-prev")).toBeVisible();

  await page.locator("#lb-next").click();
  await expect(shownName(page)).toHaveText("b_beta.jpg");
  await expect(page.locator("#lb-next")).toBeHidden();

  // Past the last item there is nothing either.
  await page.keyboard.press("ArrowRight");
  await expect(shownName(page)).toHaveText("b_beta.jpg");

  await page.keyboard.press("ArrowLeft");
  await expect(shownName(page)).toHaveText("a_alpha.jpg");
  await page.locator("#lb-prev").click();
  await expect(shownName(page)).toHaveText("c_clip.mp4");
  await expect(page.locator("#lb-prev")).toBeHidden();
});

test("stepping from a clip to a photo stops the clip", async ({ page, sortedGallery }) => {
  await page.goto(sortedGallery.baseURL);
  await openFirst(page);
  await expect(page.locator("#lb-vid")).toBeVisible();

  await page.keyboard.press("ArrowRight");
  await expect(page.locator("#lb-img")).toBeVisible();
  await expect(page.locator("#lb-vid")).toBeHidden();
  expect(await page.locator("#lb-vid").evaluate((v: HTMLVideoElement) => v.paused)).toBe(true);
  await expect(page.locator("#lb-vid")).toHaveAttribute("src", "");
});

test("Next past the loaded page loads the next page and carries on", async ({ page, sortedGallery }) => {
  await writeSettings(sortedGallery, { routes: { files: { pageSize: 1 } } });
  await page.goto(sortedGallery.baseURL);
  await expect(page.locator("#gallery [data-lb-url]")).toHaveCount(1);
  await openFirst(page);

  // One tile is loaded, but a page remains, so Next is offered.
  await expect(page.locator("#lb-next")).toBeVisible();
  await page.keyboard.press("ArrowRight");
  await expect(shownName(page)).toHaveText("a_alpha.jpg");
  await expect(page.locator("#gallery [data-lb-url]")).toHaveCount(2);

  await page.locator("#lb-next").click();
  await expect(shownName(page)).toHaveText("b_beta.jpg");
  await expect(page.locator("#gallery [data-lb-url]")).toHaveCount(3);
  await expect(page.locator("#lb-next")).toBeHidden();
});

test("the lightbox follows the chosen sort", async ({ page, sortedGallery }) => {
  await writeSettings(sortedGallery, { routes: { files: { sort: { field: "name", dir: "asc" } } } });
  await page.goto(sortedGallery.baseURL);
  await openFirst(page);
  await expect(shownName(page)).toHaveText("a_alpha.jpg");
  await page.keyboard.press("ArrowRight");
  await expect(shownName(page)).toHaveText("b_beta.jpg");
  await page.keyboard.press("ArrowRight");
  await expect(shownName(page)).toHaveText("c_clip.mp4");
});

test("the metadata panel names the file and gives its size", async ({ page, sortedGallery }) => {
  await page.goto(sortedGallery.baseURL);
  await openFirst(page);
  await page.keyboard.press("ArrowRight");
  await expect(shownName(page)).toHaveText("a_alpha.jpg");
  const panel = page.locator("#lbMeta");
  await expect(panel).toHaveClass(/on/);
  await expect(panel.locator(".lb-row a.lb-link").first()).toHaveAttribute("href", "/date/2021/08/10");
  await expect(panel).toContainText(/\d+(\.\d+)?\s?(B|KB)/);
});

// c_clip.mp4 is the sorted library's one liked item.
for (const view of ["tile", "list"] as const) {
  test(`a liked item wears a heart in ${view} view, which the lightbox heart changes`, async ({
    page,
    sortedGallery
  }) => {
    await writeSettings(sortedGallery, { routes: { files: { view } } });
    await page.goto(sortedGallery.baseURL);
    const item = (name: string) =>
      page.locator(`#gallery [data-hash]`).filter({
        has: page.locator(`[data-lb-meta*='"name":"${name}"']`)
      });
    await expect(item("c_clip.mp4").locator(".liked-badge")).toHaveCount(1);
    await expect(item("a_alpha.jpg").locator(".liked-badge")).toHaveCount(0);
    await expect(page.locator("#gallery .liked-badge")).toHaveCount(1);

    // The badge takes no clicks: one on it opens the item, it likes nothing.
    await item("c_clip.mp4").locator(".liked-badge").click({ force: true });
    await expect(page.locator("#lb")).toHaveClass(/on/);
    await expect(shownName(page)).toHaveText("c_clip.mp4");
    await expect(page.locator(".lb-like")).toHaveClass(/liked/);
    await page.keyboard.press("Escape");

    // The lightbox heart likes a_alpha and its thumbnail follows; unliking
    // takes the badge away again.
    await item("a_alpha.jpg").locator("[data-lb-url]").click();
    await expect(shownName(page)).toHaveText("a_alpha.jpg");
    await page.locator(".lb-like").click();
    await expect(page.locator(".lb-like")).toHaveClass(/liked/);
    await expect(item("a_alpha.jpg").locator(".liked-badge")).toHaveCount(1);
    await page.locator(".lb-like").click();
    await expect(page.locator(".lb-like")).not.toHaveClass(/liked/);
    await expect(item("a_alpha.jpg").locator(".liked-badge")).toHaveCount(0);
  });
}

// A browser plays few video formats (no Motion JPEG or ProRes in a .mov), and
// the player then sat dead. Served bytes no browser decodes, the lightbox says
// so and offers the original; stepping to the next item clears it.
test("a video the browser cannot play says so and offers the original", async ({ page, sortedGallery }) => {
  await page.goto(sortedGallery.baseURL);
  await expect(page.locator("#gallery [data-lb-url]")).toHaveCount(3);
  // Only the lightbox's request gets the bad bytes; the grid is already drawn.
  await page.route("**/raw**", (route) =>
    route.fulfill({ status: 200, contentType: "video/quicktime", body: Buffer.from("not a video at all") })
  );
  await openFirst(page);
  await expect(shownName(page)).toHaveText("c_clip.mp4");
  const notice = page.locator("#lb-unplayable");
  await expect(notice).toBeVisible();
  await expect(notice).toContainText("cannot play");
  const link = notice.locator("a");
  await expect(link).toHaveAttribute("download", "c_clip.mp4");
  await expect(link).toHaveAttribute("href", /\/raw/);
  await expect(page.locator("#lb-vid")).toBeHidden();

  await page.keyboard.press("ArrowRight");
  await expect(shownName(page)).toHaveText("a_alpha.jpg");
  await expect(notice).toBeHidden();
});
