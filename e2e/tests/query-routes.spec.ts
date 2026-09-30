import { expect, test } from "../support/gallery";

// The gallery fixture: first.jpg and second.jpg are the same image, so the
// date view (one row per content hash) holds that image and clip.mp4.

const q = (s: string) => encodeURIComponent(s);

test("the Date tree counts only matching files and keeps the query on its links", async ({ page, gallery }) => {
  await page.goto(`${gallery.baseURL}/date?q=${q("type:image")}`);
  await expect(page.locator("#query-status")).toContainText("1 of 2");
  const card = page.locator("#dateGrid .date-card").first();
  await expect(card).toContainText("1 files");
  await expect(card.locator("a.date-card-link")).toHaveAttribute("href", /\?q=type%3Aimage$/);

  await card.locator("a.date-card-link").dispatchEvent("click");
  await expect(page).toHaveURL(/\/date\/\d{4}\?q=type%3Aimage$/);
  await expect(page.locator("#query-status")).toContainText("1 of 2");
  await expect(page.locator("#dateBreadcrumb a").first()).toHaveAttribute("href", `/date?q=${q("type:image")}`);
});

test("a period with no match is not shown", async ({ page, gallery }) => {
  await page.goto(`${gallery.baseURL}/date?q=${q("type:image -type:image")}`);
  await expect(page.locator("#query-status")).toContainText("0 of 2");
  await expect(page.locator("#dateGrid .date-card")).toHaveCount(0);
  await expect(page.locator("#dateGrid")).toContainText("No dated files match");
});

test("a bad query on Date says why instead of showing every date", async ({ page, gallery }) => {
  await page.goto(`${gallery.baseURL}/date?q=${q("kişi:özgür")}`);
  await expect(page.locator("#query-status")).toHaveClass(/query-error/);
  await expect(page.locator("#query-status")).toContainText("unknown key");
  await expect(page.locator("#dateGrid .date-card")).toHaveCount(0);
});

test("the nav carries the query to the pages that honour it", async ({ page, gallery }) => {
  await page.goto(`${gallery.baseURL}/?q=${q("type:image")}`);
  await expect(page.locator(".secnav a", { hasText: "Date" })).toHaveAttribute("href", `/date?q=${q("type:image")}`);
  await expect(page.locator(".secnav a", { hasText: "Library" })).toHaveAttribute("href", `/?q=${q("type:image")}`);
  await expect(page.locator(".secnav a", { hasText: "Map" })).toHaveAttribute("href", "/map");
});

test("the box applies a query on the page it is on, when that page honours one", async ({ page, gallery }) => {
  await page.goto(`${gallery.baseURL}/date`);
  const input = page.locator("#nav-search");
  await input.fill("type:video");
  await expect(page.locator("#qbox-suggest [role=option]").first()).toContainText("video");
  await input.press("Enter");
  await input.press("Enter");
  await expect(page).toHaveURL(/\/date\?q=type%3Avideo$/);
  await expect(page.locator("#query-status")).toContainText("1 of 2");
});
