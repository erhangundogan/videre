import type { Page } from "@playwright/test";
import { expect, preferListView, test, type GallerySession } from "../support/gallery";

// The search library stores vectors for çiçek (the example), bahçe (close to
// it) and deniz (far). Similar ranks those for real. A text query needs the
// SigLIP model, so those tests answer /api/search?q= themselves and check
// what the page does with the ranking.

async function hashOf(page: Page, gallery: GallerySession, name: string): Promise<string> {
  const body = await (await page.request.get(`${gallery.baseURL}/api/files?limit=50`)).json();
  const file = (body.files as { path: string; hash: string }[]).find((f) => f.path.endsWith(`/${name}`));
  if (!file) throw new Error(`${name} is not in the library`);
  return file.hash;
}

function resultNames(page: Page) {
  return page.locator("#results .rcard:not(.query) .rname");
}

test("with stored vectors the nav search box is offered", async ({ page, searchGallery }) => {
  await page.goto(searchGallery.baseURL);
  await expect(page.locator(".secnav-search")).toBeVisible();
});

test("the nav search box is offered in every section, not only Library and Map", async ({
  page,
  searchGallery
}) => {
  for (const path of ["/", "/date", "/duplicates", "/events", "/map"]) {
    await page.goto(`${searchGallery.baseURL}${path}`);
    await expect(page.locator(".secnav-search"), path).toBeVisible();
  }
});

test("Similar works on the grid under the map", async ({ page, searchGallery }) => {
  await preferListView(searchGallery);
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  await page.goto(`${searchGallery.baseURL}/map`);
  const card = page.locator("#gallery .card", { hasText: "çiçek.jpg" });
  await card.locator(".similar-btn").click();
  await expect(page.locator("#results .results-head h2")).toHaveText("Similar images");
  await expect(resultNames(page)).toHaveText(["bahçe.jpg", "deniz.jpg"]);
  expect(errors).toEqual([]);
});

test("a page with nowhere to show results offers no Similar button", async ({ page, searchGallery }) => {
  await preferListView(searchGallery);
  await page.goto(`${searchGallery.baseURL}/date/2021`);
  // Present, not visible: a large preview may still be rendering.
  await expect(page.locator("#dateGrid [data-lb-url]")).not.toHaveCount(0);
  await expect(page.locator(".similar-btn")).toHaveCount(0);
});

test("Similar ranks the closest file first and leaves out the example", async ({ page, searchGallery }) => {
  await preferListView(searchGallery);
  await page.goto(searchGallery.baseURL);
  const card = page.locator("#gallery .card", { hasText: "çiçek.jpg" });
  await card.locator(".similar-btn").click();

  const results = page.locator("#results");
  await expect(results.locator(".results-head h2")).toHaveText("Similar images");
  await expect(results.locator(".rcard.query .rname")).toHaveText("query: çiçek.jpg");
  await expect(resultNames(page)).toHaveText(["bahçe.jpg", "deniz.jpg"]);

  await results.locator(".results-head button", { hasText: "Close" }).click();
  await expect(results).toBeHidden();
});

test("the nav search runs a text query and shows what it ranked", async ({ page, searchGallery }) => {
  const bahce = await hashOf(page, searchGallery, "bahçe.jpg");
  let asked = "";
  await page.route("**/api/search?q=*", async (route) => {
    asked = new URL(route.request().url()).searchParams.get("q") ?? "";
    await route.fulfill({ json: { total: 1, results: [{ hash: bahce, score: 0.42 }] } });
  });

  await page.goto(`${searchGallery.baseURL}/map`);
  const input = page.locator("#nav-search");
  await input.fill("kırmızı çiçek");
  await input.press("Enter");

  // A search from another section lands on the Files page, which runs it.
  await expect(page).toHaveURL(/\/\?q=/);
  await expect(page.locator("#results .results-head h2")).toHaveText("Results for “kırmızı çiçek”");
  await expect(resultNames(page)).toHaveText(["bahçe.jpg"]);
  await expect(page.locator("#results .rcard .score")).toHaveText("0.420");
  expect(asked).toBe("kırmızı çiçek");
  await expect(input).toHaveValue("kırmızı çiçek");
});

test("a text query that ranks nothing says so", async ({ page, searchGallery }) => {
  await page.route("**/api/search?q=*", (route) => route.fulfill({ json: { total: 0, results: [] } }));
  await page.goto(`${searchGallery.baseURL}/?q=${encodeURIComponent("deniz feneri")}`);
  await expect(page.locator("#results .results-head h2")).toHaveText("No matches for “deniz feneri”");
  await expect(page.locator("#results .rcard")).toHaveCount(0);
});

test("a failed search says so and closes", async ({ page, searchGallery }) => {
  await page.route("**/api/search?q=*", (route) => route.fulfill({ status: 500, body: "" }));
  await page.goto(`${searchGallery.baseURL}/?q=g%C3%BCne%C5%9F`);
  const results = page.locator("#results");
  await expect(results.locator(".results-head h2")).toHaveText("Search failed");
  await results.locator(".results-head button", { hasText: "Close" }).click();
  await expect(results).toBeHidden();
});
