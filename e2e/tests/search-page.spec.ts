import { readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import type { Page } from "@playwright/test";
import { expect, openLibraryDb, preferListView, test, type GallerySession } from "../support/gallery";

// The search library stores vectors for çiçek (the example), bahçe (close to
// it) and deniz (far), so Similar ranks for real without a model. A text
// query needs SigLIP, so that test answers /api/search?q= itself.

async function hashOf(page: Page, gallery: GallerySession, name: string): Promise<string> {
  const body = await (await page.request.get(`${gallery.baseURL}/api/files?limit=50`)).json();
  const file = (body.files as { path: string; hash: string }[]).find((f) => f.path.endsWith(`/${name}`));
  if (!file) throw new Error(`${name} is not in the library`);
  return file.hash;
}

function rankedNames(page: Page) {
  return page.locator("#gallery .card .card-meta[title]");
}

test("Similar opens as a page that a bookmark reopens", async ({ page, searchGallery }) => {
  await preferListView(searchGallery);
  await page.goto(searchGallery.baseURL);
  await page.locator("#gallery .card", { hasText: "çiçek.jpg" }).locator(".similar-btn").click();
  const open = page.locator("#results .results-head a", { hasText: "Open as a page" });
  await expect(open).toBeVisible();
  const hash = await hashOf(page, searchGallery, "çiçek.jpg");
  await expect(open).toHaveAttribute("href", `/search?like=${hash}`);

  await open.click();
  await expect(page).toHaveURL(`${searchGallery.baseURL}/search?like=${hash}`);
  await expect(page.locator("#search-head")).toContainText("Similar to çiçek.jpg");
  await expect(rankedNames(page)).toHaveText(["bahçe.jpg", "deniz.jpg"]);
  await expect(page.locator("#gallery .card .score").first()).toBeVisible();

  await page.reload();
  await expect(rankedNames(page)).toHaveText(["bahçe.jpg", "deniz.jpg"]);
});

test("a search page shows more of its ranking, a page at a time", async ({ page, searchGallery }) => {
  await preferListView(searchGallery);
  const path = join(searchGallery.libraryRoot, ".videre", "gallery.json");
  const stored = JSON.parse(await readFile(path, "utf8"));
  stored.routes.search = { pageSize: 1 };
  await writeFile(path, JSON.stringify(stored));
  const hash = await hashOf(page, searchGallery, "çiçek.jpg");
  const more = page.locator("#gallery-more");

  await page.goto(`${searchGallery.baseURL}/search?like=${hash}`);
  await expect(rankedNames(page)).toHaveText(["bahçe.jpg"]);
  await expect(more).toHaveText("Show more");
  await more.click();
  await expect(rankedNames(page)).toHaveText(["bahçe.jpg", "deniz.jpg"]);
  await expect(more).toBeHidden();
  await expect(page.locator("#search-head .results-count")).toHaveText("2 results");
});

test("a search page ranks within its query's filters", async ({ page, searchGallery }) => {
  await preferListView(searchGallery);
  const hash = await hashOf(page, searchGallery, "çiçek.jpg");
  const deniz = await hashOf(page, searchGallery, "deniz.jpg");
  const db = openLibraryDb(searchGallery.libraryRoot);
  db.exec(`CREATE TABLE IF NOT EXISTS photo_tags (hash TEXT NOT NULL, tag TEXT NOT NULL, PRIMARY KEY (hash, tag));
           DELETE FROM photo_tags; INSERT INTO photo_tags VALUES ('${deniz}', 'deniz');`);
  db.close();
  await page.goto(`${searchGallery.baseURL}/search?like=${hash}&q=${encodeURIComponent("tag:deniz")}`);
  await expect(rankedNames(page)).toHaveText(["deniz.jpg"]);
  await expect(page.locator(".qbox .qchip")).toContainText("deniz");
});

test("a text search has its own page too", async ({ page, searchGallery }) => {
  await preferListView(searchGallery);
  const bahce = await hashOf(page, searchGallery, "bahçe.jpg");
  await page.route((url) => url.pathname === "/api/search", (route) =>
    route.fulfill({ json: { total: 1, results: [{ hash: bahce, score: 0.42 }] } }));
  await page.goto(`${searchGallery.baseURL}/search?q=${encodeURIComponent("kırmızı çiçek")}`);
  await expect(page.locator("#search-head")).toContainText("Results for “kırmızı çiçek”");
  await expect(rankedNames(page)).toHaveText(["bahçe.jpg"]);
  await expect(page.locator("#gallery .card .score")).toHaveText("0.420");
});
