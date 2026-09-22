import { expect, test } from "../support/gallery";

test("the events overview lists event cards", async ({ page, gallery }) => {
  await page.goto(`${gallery.baseURL}/events`);
  await expect(page.locator(".secnav a[href='/events']")).toHaveClass(/on/);
  await expect(page.locator("#dateBreadcrumb")).toHaveText("Events");
  await expect(page.locator("#dateGrid .event-card").first()).toBeVisible();
});

test("drilling into an event shows its photos under an All Events crumb", async ({ page, gallery }) => {
  await page.goto(`${gallery.baseURL}/events`);
  const link = page.locator("#dateGrid .event-card a.date-card-link").first();
  await expect(link).toBeVisible();
  // The thumbnail sits above the card link and opens the lightbox, so click
  // the card's caption area, below it.
  const box = await link.boundingBox();
  await link.click({ position: { x: 10, y: box!.height - 6 } });

  await expect(page).toHaveURL(/\/events\/\d{8}T\d{6}-[0-9a-f]{1,8}$/);
  const root = page.locator("#dateBreadcrumb a", { hasText: "All Events" });
  await expect(root).toHaveAttribute("href", "/events");
  await expect(page.locator("#dateGrid [data-lb-url]").first()).toBeVisible();
});
