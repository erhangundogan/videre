import { expect, preferListView, test } from "../support/gallery";
import type { Page } from "@playwright/test";

// These specs read list-view cards; Tile is the default view, so save List.
test.beforeEach(async ({ sortedGallery }) => {
  await preferListView(sortedGallery);
});

// The first card's filename, in list mode. buildCard emits the filename as
// the card's first .card-meta.
async function firstCard(page: Page): Promise<string> {
  return page.locator("#gallery .card > div.card-meta:first-of-type").first().innerText();
}

async function allCards(page: Page): Promise<string[]> {
  return page.$$eval("#gallery .card > div.card-meta:first-of-type", (els) =>
    els.map((e) => (e.textContent || "").trim())
  );
}

async function chooseSort(page: Page, field: string, desc: boolean): Promise<void> {
  const dir = desc ? "desc" : "asc";
  // The refetch is asynchronous and the stale grid also shows three cards, so
  // settle on the response that carries the final field and direction before
  // reading any order.
  const settled = page.waitForResponse(
    (r) => r.url().includes("/api/files?") && r.url().includes(`sort=${field}&dir=${dir}`)
  );
  await page.locator(".sort-select").first().selectOption(field);
  const btn = page.locator(".sort-dir-btn").first();
  if ((await btn.getAttribute("aria-pressed")) !== (desc ? "true" : "false")) {
    await btn.click();
  }
  await expect(btn).toHaveAttribute("aria-pressed", desc ? "true" : "false");
  await settled;
  await expect(page.locator("#gallery .card")).toHaveCount(3);
}

test("defaults to date descending, not path order", async ({ page, sortedGallery }) => {
  await page.goto(sortedGallery.baseURL);
  await expect(page.locator("#gallery .card").first()).toBeVisible();
  expect(await firstCard(page)).toBe("c_clip.mp4");
  expect(await allCards(page)).toEqual(["c_clip.mp4", "a_alpha.jpg", "b_beta.jpg"]);
});

test("each field puts its own file first", async ({ page, sortedGallery }) => {
  await page.goto(sortedGallery.baseURL);
  await expect(page.locator("#gallery .card").first()).toBeVisible();

  await chooseSort(page, "name", false);
  expect(await allCards(page)).toEqual(["a_alpha.jpg", "b_beta.jpg", "c_clip.mp4"]);
  await chooseSort(page, "name", true);
  expect(await allCards(page)).toEqual(["c_clip.mp4", "b_beta.jpg", "a_alpha.jpg"]);

  await chooseSort(page, "size", true);
  expect(await allCards(page)).toEqual(["b_beta.jpg", "a_alpha.jpg", "c_clip.mp4"]);
  await chooseSort(page, "size", false);
  expect(await allCards(page)).toEqual(["c_clip.mp4", "a_alpha.jpg", "b_beta.jpg"]);

  await chooseSort(page, "rating", true);
  expect(await allCards(page)).toEqual(["a_alpha.jpg", "b_beta.jpg", "c_clip.mp4"]);
  await chooseSort(page, "rating", false);
  expect(await allCards(page)).toEqual(["b_beta.jpg", "a_alpha.jpg", "c_clip.mp4"]);

  await chooseSort(page, "liked", true);
  expect(await allCards(page)).toEqual(["c_clip.mp4", "a_alpha.jpg", "b_beta.jpg"]);
  await chooseSort(page, "liked", false);
  expect(await allCards(page)).toEqual(["a_alpha.jpg", "b_beta.jpg", "c_clip.mp4"]);

  await chooseSort(page, "type", false);
  expect(await allCards(page)).toEqual(["a_alpha.jpg", "b_beta.jpg", "c_clip.mp4"]);
  await chooseSort(page, "type", true);
  expect(await allCards(page)).toEqual(["c_clip.mp4", "a_alpha.jpg", "b_beta.jpg"]);
});

test("the direction button toggles pressed state and order", async ({ page, sortedGallery }) => {
  await page.goto(sortedGallery.baseURL);
  await expect(page.locator("#gallery .card").first()).toBeVisible();
  const btn = page.locator(".sort-dir-btn").first();
  await expect(btn).toHaveAttribute("aria-pressed", "true"); // date desc is the default

  await btn.click();
  await expect(btn).toHaveAttribute("aria-pressed", "false");
  expect(await firstCard(page)).toBe("b_beta.jpg"); // date asc: oldest first

  await btn.click();
  await expect(btn).toHaveAttribute("aria-pressed", "true");
  expect(await firstCard(page)).toBe("c_clip.mp4");
});

test("the sort choice survives a reload", async ({ page, sortedGallery }) => {
  await page.goto(sortedGallery.baseURL);
  await chooseSort(page, "name", true);
  expect(await firstCard(page)).toBe("c_clip.mp4");

  // The choice is saved to the library's settings after a short quiet period.
  await expect.poll(async () =>
    (await (await page.request.get(`${sortedGallery.baseURL}/api/settings`)).json()).effective.routes.files.sort
  ).toEqual({ field: "name", dir: "desc" });
  await page.reload();
  await expect(page.locator(".sort-select").first()).toHaveValue("name");
  await expect(page.locator(".sort-dir-btn").first()).toHaveAttribute("aria-pressed", "true");
  expect(await firstCard(page)).toBe("c_clip.mp4");
});

test("tile mode keeps the same first item as list mode", async ({ page, sortedGallery }) => {
  await page.goto(sortedGallery.baseURL);
  await expect(page.locator("#gallery .card").first()).toBeVisible();
  await chooseSort(page, "liked", true);
  // buildPreview stamps every preview link with data-lb-meta, whose JSON
  // carries the hash; both list cards and tiles go through buildPreview, so
  // one attribute identifies the item in either mode.
  const metaOf = async (locator: ReturnType<Page["locator"]>) => {
    const raw = await locator.first().getAttribute("data-lb-meta");
    return (JSON.parse(raw as string) as { hash: string }).hash;
  };
  const listFirst = await metaOf(page.locator("#gallery .card [data-lb-meta]"));

  await page.locator(".view-mode-select").first().selectOption("tile");
  await expect(page.locator("#gallery")).toHaveClass(/tile-mode/);
  await expect(page.locator("#gallery .tile").first()).toBeVisible();
  expect(await metaOf(page.locator("#gallery .tile [data-lb-meta]"))).toBe(listFirst);
});

test("the date view has the control and the duplicates page does not", async ({
  page,
  sortedGallery
}) => {
  await page.goto(`${sortedGallery.baseURL}/date`);
  await expect(page.locator("#dateGrid .date-card").first()).toBeVisible();
  await expect(page.locator(".gallery-toolbar .sort-select")).toHaveCount(1);

  // The sorted seed has no duplicates, so the duplicates page renders its
  // empty state; either way the new control is absent there. (The page's own
  // Sort by select is id="sort-select", distinct from the class-only control.)
  await page.goto(`${sortedGallery.baseURL}/duplicates`);
  await expect(page.locator(".sort-select")).toHaveCount(0);
});
