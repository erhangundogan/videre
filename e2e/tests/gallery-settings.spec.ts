import { readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { expect, test, type GallerySession } from "../support/gallery";

async function settings(page: import("@playwright/test").Page, gallery: GallerySession) {
  return (await page.request.get(`${gallery.baseURL}/api/settings`)).json();
}

test("the people layout choice survives a reload", async ({ page, isolatedGallery: gallery }) => {
  // The layout toolbar only shows once there are faces to lay out, so give
  // this library one. Its own gallery, so the face cannot reach the shared
  // library's "No faces detected yet" test.
  const { DatabaseSync } = await import("node:sqlite").catch(() => ({ DatabaseSync: undefined }));
  test.skip(!DatabaseSync, "node:sqlite is unavailable on this Node");
  const db = new DatabaseSync!(join(gallery.libraryRoot, ".videre", "hashes.db"));
  db.exec("PRAGMA busy_timeout = 5000");
  db.exec(`INSERT INTO faces (hash, bbox, embedding, blur)
           VALUES ('layout-face', '0,0,100,100', X'${"003C".padEnd(2048, "0")}', 500.0)`);
  db.close();

  await page.goto(`${gallery.baseURL}/people`);
  await expect(page.locator("body")).toHaveClass(/sidebar-mode/);

  await page.locator("#layout-select").selectOption("top");
  await expect(page.locator("body")).not.toHaveClass(/sidebar-mode/);
  await expect.poll(async () => (await settings(page, gallery)).effective.routes.people.align).toBe("top");

  await page.reload();
  await expect(page.locator("body")).not.toHaveClass(/sidebar-mode/);
  await expect(page.locator("#layout-select")).toHaveValue("top");
});

test("the last page visited is saved, and the settings page never is", async ({ page, gallery }) => {
  await page.goto(`${gallery.baseURL}/date/2021`);
  await expect.poll(async () => (await settings(page, gallery)).effective.resume.route).toBe("/date/2021");

  await page.goto(`${gallery.baseURL}/settings`);
  await expect(page.getByRole("heading", { name: "Settings" })).toBeVisible();
  // Give a stray save time to land before checking nothing changed.
  await page.waitForTimeout(600);
  expect((await settings(page, gallery)).effective.resume.route).toBe("/date/2021");
});

test("the ... menu opens, closes on Escape, and leads to Settings", async ({ page, gallery }) => {
  await page.goto(gallery.baseURL);
  const more = page.locator("#secnav-more");
  const menu = page.locator("#secnav-menu-list");

  await more.click();
  await expect(menu).toBeVisible();
  await expect(more).toHaveAttribute("aria-expanded", "true");
  await page.keyboard.press("Escape");
  await expect(menu).toBeHidden();
  await expect(more).toBeFocused();

  await more.click();
  await page.getByRole("menuitem", { name: "Settings" }).click();
  await expect(page).toHaveURL(`${gallery.baseURL}/settings`);
  await expect(page.locator("#settings-path")).toContainText(".videre/gallery.json");
});

test("export, reset and import round-trip the library's choices", async ({ page, gallery }) => {
  await page.goto(gallery.baseURL);
  await page.locator(".view-mode-select").first().selectOption("list");
  await expect.poll(async () => (await settings(page, gallery)).effective.routes.files.view).toBe("list");

  await page.goto(`${gallery.baseURL}/settings`);
  const download = page.waitForEvent("download");
  await page.locator("#settings-export").click();
  const file = await (await download).path();
  const exported = JSON.parse(await readFile(file, "utf8"));
  expect(exported).toEqual({ routes: { files: { view: "list" } } });

  page.once("dialog", (dialog) => dialog.accept());
  await page.locator("#settings-reset").click();
  await expect(page.locator("#settings-status")).toHaveText("Reset to defaults.");
  expect((await settings(page, gallery)).effective.routes.files.view).toBe("tile");

  await page.locator("#settings-import").setInputFiles(file);
  await expect(page.locator("#settings-status")).toHaveText("Imported.");
  await page.goto(gallery.baseURL);
  await expect(page.locator(".view-mode-select").first()).toHaveValue("list");
});

test("page sizes are edited on the settings page, in range only", async ({ page, gallery }) => {
  const path = join(gallery.libraryRoot, ".videre", "gallery.json");
  await page.goto(`${gallery.baseURL}/settings`);
  const date = page.locator('input[data-setting="routes.date.pageSize"]');
  await expect(date).toHaveValue("200");
  await expect(page.locator('input[data-setting="routes.search.pageSize"]')).toHaveValue("96");
  await expect(page.locator('input[data-setting="routes.duplicates.pageSize"]')).toHaveValue("100");

  await date.fill("3");
  await date.press("Enter");
  await expect(page.locator("#settings-status")).toHaveText("Saved.");
  expect(JSON.parse(await readFile(path, "utf8"))).toEqual({ routes: { date: { pageSize: 3 } } });

  await date.fill("501");
  await date.press("Enter");
  await expect(page.locator("#settings-status")).toHaveText("Date: a whole number from 1 to 500.");
  await expect(date).toHaveValue("3");
  expect(JSON.parse(await readFile(path, "utf8"))).toEqual({ routes: { date: { pageSize: 3 } } });

  page.once("dialog", (dialog) => dialog.accept());
  await page.locator("#settings-reset").click();
  await expect(page.locator("#settings-status")).toHaveText("Reset to defaults.");
  await expect(date).toHaveValue("200");
});

test("a settings value that looks like markup cannot swallow the page", async ({ page, gallery }) => {
  // An undeclared key survives into the inlined settings, so an imported file
  // can carry any string. `<!--<script>` inside a script element used to keep
  // the HTML parser in the script and blank everything after it.
  await writeFile(
    join(gallery.libraryRoot, ".videre", "gallery.json"),
    JSON.stringify({ mine: "<!--<script>", routes: { files: { view: "</script><b>x" } } })
  );
  await page.goto(gallery.baseURL);
  await expect(page.locator("#gallery [data-lb-url]").first()).toBeVisible();
  await expect(page.locator("#secnav-more")).toBeVisible();
  expect(await page.evaluate(() => (window as unknown as { VIDERE_SETTINGS: { mine: string } }).VIDERE_SETTINGS.mine))
    .toBe("<!--<script>");
});

test("a fractional page size falls back to the default instead of emptying the grid", async ({ page, gallery }) => {
  await writeFile(
    join(gallery.libraryRoot, ".videre", "gallery.json"),
    JSON.stringify({ routes: { files: { pageSize: 1.5 } } })
  );
  await page.goto(gallery.baseURL);
  await expect(page.locator("#gallery [data-lb-url]").first()).toBeVisible();
});

test("the tile row height comes from gallery.json, and an out-of-range one is ignored", async ({ page, gallery }) => {
  const path = join(gallery.libraryRoot, ".videre", "gallery.json");
  const firstTileHeight = async (tile: object) => {
    await writeFile(path, JSON.stringify({ routes: { files: { view: "tile", tile } } }));
    await page.goto(gallery.baseURL);
    const first = page.locator("#gallery .tile").first();
    await expect(first).toBeVisible();
    return (await first.boundingBox())!.height;
  };
  const short = await firstTileHeight({ rowHeight: 100 });
  const tall = await firstTileHeight({ rowHeight: 400 });
  expect(tall).toBeGreaterThan(short * 2);
  // 5 is below the 80px floor, so the default 280 applies.
  const fallback = await firstTileHeight({ rowHeight: 5 });
  const byDefault = await firstTileHeight({});
  expect(fallback).toBeCloseTo(byDefault, 0);
});

test("an unreadable settings file shows a banner and is never overwritten", async ({ page, gallery }) => {
  const path = join(gallery.libraryRoot, ".videre", "gallery.json");
  await writeFile(path, "{oops");

  await page.goto(gallery.baseURL);
  await expect(page.locator(".settings-banner")).toContainText("not valid JSON");
  await page.locator(".view-mode-select").first().selectOption("list");
  await page.waitForTimeout(600);
  expect(await readFile(path, "utf8")).toBe("{oops");
});
