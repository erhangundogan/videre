import { expect, test } from "../support/gallery";

// Every route of an empty library says so the same way: one centred box, 64 px
// under the nav, with no toolbar, header or layout around it.
for (const [route, heading] of [
  ["/", "No photos yet"],
  ["/duplicates", "No duplicates"],
  ["/date", "No photos yet"],
  ["/events", "No travel trips yet"],
  ["/people", "No faces detected yet"],
  ["/map", "No places yet"]
] as const) {
  test(`an empty library's ${route} shows the shared empty state`, async ({ page, emptyGallery: gallery }) => {
    await page.goto(`${gallery.baseURL}${route}`);
    const box = page.locator(".empty-state");
    await expect(box.getByRole("heading", { name: heading })).toBeVisible();
    await expect(box).toHaveCount(1);

    const rect = (await box.boundingBox())!;
    const width = page.viewportSize()!.width;
    expect(Math.abs(rect.x - (width - rect.x - rect.width))).toBeLessThanOrEqual(2);
    const nav = (await page.locator(".secnav").boundingBox())!;
    expect(Math.round(rect.y - (nav.y + nav.height))).toBe(64);

    // Nothing else on the page is showing: only the nav and the box.
    const shown = await page.evaluate(() =>
      [...document.body.children]
        .filter((el) => !["SCRIPT", "STYLE"].includes(el.tagName))
        .filter((el) => getComputedStyle(el).display !== "none" && (el as HTMLElement).offsetHeight > 0)
        .map((el) => el.className || el.id || el.tagName)
    );
    expect(shown).toEqual(["secnav", "empty-state"]);
  });
}
