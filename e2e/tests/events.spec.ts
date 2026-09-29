import { expect, seedTrips, test } from "../support/gallery";

test("Events explains empty travel evidence, then shows one exact Budapest trip", async ({ page, isolatedGallery: gallery }) => {
  await page.goto(`${gallery.baseURL}/events`);
  await expect(page.locator(".secnav a[href='/events']")).toHaveClass(/on/);
  const empty = page.locator("#page-empty");
  await expect(empty).toContainText("location-supported photos");
  // Placed like every other empty state: centred, 64 px under the nav, with
  // the toolbar and the events layout (breadcrumb, grid) hidden.
  const box = await empty.boundingBox();
  const width = page.viewportSize()!.width;
  expect(Math.abs(box!.x - (width - box!.x - box!.width))).toBeLessThanOrEqual(2);
  const nav = await page.locator(".secnav").boundingBox();
  expect(Math.round(box!.y - (nav!.y + nav!.height))).toBe(64);
  await expect(page.locator(".gallery-toolbar").first()).toBeHidden();
  await expect(page.locator("#dateBreadcrumb")).toBeHidden();

  seedTrips(gallery.libraryRoot);
  await page.reload();
  const cards = page.locator("#dateGrid .event-card");
  await expect(cards).toHaveCount(1);
  await expect(cards.first()).toContainText("Budapest, March 2020 Trip");
  await expect(cards.first()).toContainText("12 files");
  const key = `20200312T100000-${(101).toString(16).padStart(64, "0")}`;
  await expect(cards.first().locator("a.date-card-link")).toHaveAttribute("href", `/events/${key}`);
  await cards.first().locator("a.date-card-link").dispatchEvent("click");
  await expect(page).toHaveURL(`${gallery.baseURL}/events/${key}`);
  await expect(page.locator("#dateBreadcrumb")).toContainText("Budapest, March 2020 Trip");
  await expect(page.locator("#dateGrid [data-lb-url]")).toHaveCount(12);

  const response = await page.request.get(`${gallery.baseURL}/api/events/${key}/files`);
  expect(response.status()).toBe(200);
  const payload = await response.json() as { files: Array<{ hash: string }> };
  expect(payload.files.map((f) => f.hash).sort()).toEqual(
    Array.from({ length: 12 }, (_, i) => (101 + i).toString(16).padStart(64, "0")).sort()
  );
});
