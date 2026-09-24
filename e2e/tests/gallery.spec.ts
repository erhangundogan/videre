import { copyFileSync, rmSync } from "node:fs";
import { dirname, join } from "node:path";
import { DatabaseSync } from "node:sqlite";
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
  // The year overview is rooted by an "All Dates" breadcrumb (there is no
  // "Browse by date" title any more).
  await expect(page.locator("#dateBreadcrumb")).toHaveText("All Dates");
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

test("small media keeps the lightbox controls clear of the metadata", async ({ page, gallery }) => {
  // The fixture is a tiny 64px image; without a minimum stage size the control
  // bar would overlap the metadata panel below.
  await page.goto(gallery.baseURL);
  await page.locator("#gallery [data-lb-type='image']").first().click();
  await expect(page.locator("#lb-img")).toBeVisible();

  const controls = await page.locator(".lb-controls").boundingBox();
  const meta = await page.locator("#lbMeta").boundingBox();
  if (!controls || !meta) throw new Error("missing bounding boxes");
  // The control bar sits entirely above the metadata panel, not over it.
  expect(controls.y + controls.height).toBeLessThanOrEqual(meta.y + 1);
});

test("the lightbox date links to its day view", async ({ page, gallery }) => {
  await page.goto(gallery.baseURL);
  await page.locator("#gallery [data-lb-type='image']").first().click();
  const dateLink = page.locator("#lbMeta a.lb-link[href^='/date/']");
  await expect(dateLink).toBeVisible();
  await expect(dateLink).toHaveAttribute("href", /^\/date\/\d{4}\/\d{2}\/\d{2}$/);
  // Following it lands on that day's view.
  const href = await dateLink.getAttribute("href");
  await page.goto(`${gallery.baseURL}${href}`);
  await expect(page.locator("#dateGrid [data-lb-url]").first()).toBeVisible();
});

test("clicking the lightbox image zooms it and toggles back to fit", async ({ page, gallery }) => {
  await page.goto(gallery.baseURL);
  await page.locator("#gallery [data-lb-type='image']").first().click();
  const img = page.locator("#lb-img");
  await expect(img).toBeVisible();
  await expect(img).toHaveCSS("transform", "none");

  // Toggling zoom is a tap on the image (pointerdown+up with no drag). Driven by
  // a pointer dispatch on the element itself so the 16x12 fixture image, which
  // sits under the top-right controls, is exercised without a coordinate clash.
  const tapImage = () =>
    img.evaluate((el) => {
      const r = el.getBoundingClientRect();
      const opts = { clientX: r.left + r.width / 2, clientY: r.top + r.height / 2, bubbles: true, pointerId: 1 };
      el.dispatchEvent(new PointerEvent("pointerdown", opts));
      el.dispatchEvent(new PointerEvent("pointerup", opts));
    });

  // A tap zooms in: the stage enters its pan viewport and the image is scaled,
  // loading the full-resolution original for detail.
  await tapImage();
  await expect(page.locator(".lb-stage")).toHaveClass(/zooming/);
  await expect(img).not.toHaveCSS("transform", "none");
  await expect(async () => {
    const natural = await img.evaluate((el: HTMLImageElement) => el.naturalWidth);
    expect(natural).toBeGreaterThan(0);
  }).toPass();

  // A second tap returns to fit.
  await tapImage();
  await expect(page.locator(".lb-stage")).not.toHaveClass(/zooming/);
  await expect(img).toHaveCSS("transform", "none");
});

test("rotate is offered for photos, rotates, and is hidden for video", async ({ page, gallery }) => {
  await page.goto(gallery.baseURL);

  // A photo: controls in order rotate-ccw, rotate-cw, fullscreen, close.
  await page.locator("#gallery [data-lb-type='image']").first().click();
  await expect(page.locator("#lb")).toHaveClass(/on/);
  await expect(page.locator("#lb-rotate")).toBeVisible();
  const ids = await page.locator(".lb-controls button").evaluateAll((els) => els.map((e) => e.id));
  expect(ids).toEqual(["lb-rotate-ccw", "lb-rotate", "lb-fs", "lb-close"]);

  // Rotating posts to the endpoint and re-fetches the preview under the
  // photo's new content hash (cache-busted).
  const rotateResponse = page.waitForResponse(
    (r) => /\/api\/files\/[^/]+\/rotate$/.test(new URL(r.url()).pathname) && r.request().method() === "POST"
  );
  const before = await page.locator("#lb-img").getAttribute("src");
  await page.locator("#lb-rotate").click();
  const response = await rotateResponse;
  expect(response.status()).toBe(200);
  const { hash } = await response.json();
  await expect(page.locator("#lb-img")).not.toHaveAttribute("src", before ?? "");
  await expect(page.locator("#lb-img")).toHaveAttribute("src", new RegExp(`/api/files/${hash}/`));
  await expect(page.locator("#lb-img")).toHaveAttribute("src", /[?&]b=\d+/);
  await expect
    .poll(() => page.locator("#lb-img").evaluate((img: HTMLImageElement) => img.naturalWidth))
    .toBeGreaterThan(0);
  await expect(page.locator("#lb-rotate")).toBeEnabled();

  // A video carries no EXIF orientation, so the rotate button is hidden.
  await page.keyboard.press("Escape");
  await page.locator("#gallery [data-lb-type='video']").first().click();
  await expect(page.locator("#lb")).toHaveClass(/on/);
  await expect(page.locator("#lb-rotate")).toBeHidden();
});

type TileMeta = { hash: string; path?: string };

async function tileMetas(page: import("@playwright/test").Page, scope: string): Promise<TileMeta[]> {
  return page
    .locator(`${scope} [data-lb-meta]`)
    .evaluateAll((els) => els.map((el) => JSON.parse((el as HTMLElement).dataset.lbMeta || "{}")));
}

async function rotateOpenPhoto(page: import("@playwright/test").Page) {
  const rotateResponse = page.waitForResponse(
    (r) => /\/api\/files\/[^/]+\/rotate$/.test(new URL(r.url()).pathname) && r.request().method() === "POST"
  );
  await page.locator("#lb-rotate").click();
  const response = await rotateResponse;
  expect(response.status()).toBe(200);
  await expect(page.locator("#lb-rotate")).toBeEnabled();
  return (await response.json()) as { hash: string; path: string };
}

test("rotating one of two identical photos leaves the other copy's item alone", async ({ page, gallery }) => {
  // The library is shared by every test in this worker and others turn
  // first.jpg, so the pair is made here from second.jpg's current bytes and
  // row, and removed again afterwards (the map tests count every row).
  const db = new DatabaseSync(join(gallery.libraryRoot, ".videre", "hashes.db"));
  const source = db
    .prepare("SELECT path, hash, ext, mime, size_bytes, exif_date FROM file_hashes WHERE path LIKE '%/second.jpg'")
    .get() as { path: string; hash: string; ext: string; mime: string; size_bytes: number; exif_date: string };
  const twins = ["twin-a.jpg", "twin-b.jpg"].map((name) => join(dirname(source.path), name));
  try {
    for (const twin of twins) {
      copyFileSync(source.path, twin);
      db.prepare(
        "INSERT INTO file_hashes (path, hash, ext, mime, size_bytes, exif_date) VALUES (?, ?, ?, ?, ?, ?)"
      ).run(twin, source.hash, source.ext, source.mime, source.size_bytes, source.exif_date);
    }

    await page.goto(gallery.baseURL);
    const clicked = page.locator(`#gallery [data-lb-type='image'][data-lb-meta*="twin-a.jpg"]`).first();
    await clicked.click();
    const turned = await rotateOpenPhoto(page);
    expect(turned.path, "the copy that was clicked is the one that turns").toBe(twins[0]);
    await expect(page.locator("#lb-img")).toHaveAttribute("src", new RegExp(`/api/files/${turned.hash}/`));

    const after = await tileMetas(page, "#gallery");
    expect(after.find((m) => m.path === twins[0])!.hash).toBe(turned.hash);
    expect(after.find((m) => m.path === twins[1])!.hash, "the copy that did not turn keeps its hash").toBe(
      source.hash
    );
  } finally {
    for (const twin of twins) {
      db.prepare("DELETE FROM file_hashes WHERE path = ?").run(twin);
      rmSync(twin, { force: true });
    }
    db.close();
  }
});

test("a date page keeps the rotated photo's new hash across a layout switch", async ({ page, gallery }) => {
  await page.goto(`${gallery.baseURL}/date/2021/08/10`);
  await page.locator("#dateGrid [data-lb-type='image']").first().click();
  const turned = await rotateOpenPhoto(page);
  await page.keyboard.press("Escape");

  // Switching layout rebuilds the items from the page's retained rows.
  await page.locator(".view-mode-select").first().selectOption("tile");
  await expect(page.locator("#dateGrid")).toHaveClass(/tile-mode/);
  const item = (await tileMetas(page, "#dateGrid")).find((m) => m.path === turned.path);
  expect(item!.hash).toBe(turned.hash);
  await expect(
    page.locator(`#dateGrid [data-lb-url*="/api/files/${turned.hash}/"]`).first()
  ).toBeAttached();
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

test("face learning strip renders on the labeling route", async ({ page, gallery }) => {
  await page.goto(`${gallery.baseURL}/people`);
  const strip = page.locator("#learning-strip");
  await expect(strip).toBeVisible();
  await expect(strip).toHaveAttribute(
    "data-learning-status",
    /current|stale|training|failed/
  );
  // No profile has ever been trained in this library, so no question may
  // promise itself into existence.
  await expect(page.locator("#question-card")).toBeHidden();
});
