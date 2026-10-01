import { expect, seedPlace, seedTrips, test } from "../support/gallery";

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
  await expect(page.locator(".secnav a", { hasText: "Map" })).toHaveAttribute("href", `/map?q=${q("type:image")}`);
  await expect(page.locator(".secnav a", { hasText: "Events" })).toHaveAttribute("href", `/events?q=${q("type:image")}`);
  await expect(page.locator(".secnav a", { hasText: "People" })).toHaveAttribute("href", "/people");
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

test("Map asks for the places of matching files and keeps the query in its URLs", async ({ page, isolatedGallery: gallery }) => {
  seedPlace(gallery.libraryRoot);
  const asked = page.waitForRequest((r) => r.url().includes("/api/location-clusters"));
  await page.goto(`${gallery.baseURL}/map/location/berlin?radius=20&q=${q("type:image")}`);
  expect(new URL((await asked).url()).searchParams.get("q")).toBe("type:image");
  await expect(page.locator("#query-status")).toContainText("of 3");
  await expect.poll(() => new URL(page.url()).searchParams.get("q")).toBe("type:image");
  await expect(page).toHaveURL(/\/map\/location\/berlin\?/);
});

test("Map says so when no place has a matching file", async ({ page, isolatedGallery: gallery }) => {
  seedPlace(gallery.libraryRoot);
  await page.goto(`${gallery.baseURL}/map?q=${q("ext:png")}`);
  await expect(page.locator("#query-status")).toContainText("0 of 3");
  await expect(page.getByText("No places have a matching file.")).toBeVisible();
});

test("Events keeps trips with a matching member and narrows them", async ({ page, isolatedGallery: gallery }) => {
  seedTrips(gallery.libraryRoot);
  await page.goto(`${gallery.baseURL}/events?q=${q("type:video")}`);
  const cards = page.locator("#dateGrid .event-card");
  await expect(cards).toHaveCount(1);
  await expect(cards.first()).toContainText("1 file");
  const link = cards.first().locator("a.date-card-link");
  await expect(link).toHaveAttribute("href", /\?q=type%3Avideo$/);
  await link.dispatchEvent("click");
  await expect(page).toHaveURL(/\/events\/[^?]+\?q=type%3Avideo$/);
  await expect(page.locator("#dateBreadcrumb a").first()).toHaveAttribute("href", `/events?q=${q("type:video")}`);
  await expect(page.locator(".date-period-count")).toContainText("1 item");

  await page.goto(`${gallery.baseURL}/events?q=${q("ext:png")}`);
  await expect(page.locator("#dateGrid")).toContainText("No trip has a matching file.");
});
