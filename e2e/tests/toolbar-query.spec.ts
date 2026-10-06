import { expect, seedFace, seedPlace, seedTrips, test } from "../support/gallery";

// Search and filter sit in each tab's toolbar, beside Sort, on every page that
// narrows by a query; elsewhere they stay in the nav.
test("search and filter sit in each tab's toolbar", async ({ page, isolatedGallery: gallery }) => {
  seedPlace(gallery.libraryRoot);
  seedFace(gallery.libraryRoot);
  seedTrips(gallery.libraryRoot);
  for (const path of ["/", "/date", "/date/2021", "/events", "/duplicates", "/map", "/people"]) {
    await page.goto(`${gallery.baseURL}${path}`);
    const box = page.locator(".gallery-toolbar .secnav-search");
    await expect(box, path).toBeVisible();
    await expect(page.locator(".secnav .secnav-search"), path).toHaveCount(0);
    await expect(box.getByRole("button", { name: "Filter" }), path).toBeVisible();
  }
  await page.goto(`${gallery.baseURL}/settings`);
  await expect(page.locator(".secnav .secnav-search")).toBeVisible();
});

test("an empty state puts the box back in the nav", async ({ page, emptyGallery: gallery }) => {
  await page.goto(`${gallery.baseURL}/date`);
  await expect(page.locator("#page-empty")).toBeVisible();
  await expect(page.locator(".secnav .secnav-search")).toBeVisible();
});

test("Filter in the toolbar opens the options and Apply filters the tab", async ({ page, gallery }) => {
  await page.goto(`${gallery.baseURL}/date/2021`);
  const filter = page.locator(".gallery-toolbar").getByRole("button", { name: "Filter" });
  await filter.click();
  await expect(page.locator("#qbox-options")).toBeVisible();
  await expect(filter).toHaveAttribute("aria-expanded", "true");
  await page.locator("#qbox-options .qopt-group[data-key=ext] .qopt-val", { hasText: "mp4" }).click();
  await page.locator("#qbox-apply").click();
  await expect(page).toHaveURL(/\/date\/2021\?q=ext%3Amp4/);
  await expect(page.locator(".gallery-toolbar .qchip")).toContainText("mp4");
});

test("Select comes first in a files toolbar, and features are divided", async ({ page, gallery }) => {
  await page.goto(gallery.baseURL);
  const bar = page.locator(".gallery-toolbar[data-files]").first();
  await expect(bar.locator(":scope > :first-child")).toHaveClass(/select-toggle/);
  const dividers = await bar.locator(":scope > *").evaluateAll((els) =>
    els.map((e) => [e.className, getComputedStyle(e).borderLeftStyle])
  );
  // View, Sort and the query box each start with a divider; Select's note
  // belongs to Select and does not.
  expect(dividers.filter(([, style]) => style === "solid").map(([cls]) => cls)).toEqual(
    expect.arrayContaining(["view-toggle", "toolbar-query"])
  );
  expect(dividers.find(([cls]) => cls === "select-state")![1]).toBe("none");
});

test("the toolbar sticks under the nav while the page scrolls", async ({ page, gallery }) => {
  await page.goto(gallery.baseURL);
  await page.evaluate(() => {
    document.body.style.minHeight = "5000px";
    window.scrollTo(0, 2000);
  });
  const nav = await page.locator("nav.secnav").boundingBox();
  const bar = await page.locator(".gallery-toolbar[data-files]").first().boundingBox();
  expect(Math.abs(bar!.y - (nav!.y + nav!.height))).toBeLessThanOrEqual(1);
});

// A box in the middle of a one-line toolbar on a laptop-sized window: the
// panel (up to 920px wide) used to start at the box and run off the right
// edge, and off the bottom on a short window.
test("the Filter panel and suggestions stay inside the window", async ({ page, gallery }) => {
  const [W, H] = [1100, 420];
  await page.setViewportSize({ width: W, height: H });
  await page.goto(`${gallery.baseURL}/date/2021`);
  const inside = async (selector: string) => {
    const box = (await page.locator(selector).boundingBox())!;
    expect(box.x, selector).toBeGreaterThanOrEqual(0);
    expect(box.x + box.width, selector).toBeLessThanOrEqual(W);
    expect(box.y + box.height, selector).toBeLessThanOrEqual(H);
  };
  await page.locator(".gallery-toolbar").getByRole("button", { name: "Filter" }).click();
  await expect(page.locator("#qbox-options .qopt-col").first()).toBeVisible();
  await inside("#qbox-options");
  // A short window scrolls the panel, and Apply stays in view at its foot.
  await inside("#qbox-apply");
  await page.keyboard.press("Escape");
  await page.locator("#nav-search").fill("ty");
  await expect(page.locator("#qbox-suggest")).toBeVisible();
  await inside("#qbox-suggest");
});

test("the query's N of M line sticks under the toolbar", async ({ page, gallery }) => {
  await page.goto(`${gallery.baseURL}/?q=${encodeURIComponent("type:image")}`);
  const status = page.locator("#query-status");
  await expect(status).toContainText("of");
  await page.evaluate(() => {
    document.body.style.minHeight = "5000px";
    window.scrollTo(0, 2000);
  });
  const bar = await page.locator(".gallery-toolbar[data-files]").first().boundingBox();
  const line = await status.boundingBox();
  expect(Math.abs(line!.y - (bar!.y + bar!.height))).toBeLessThanOrEqual(1);
});
