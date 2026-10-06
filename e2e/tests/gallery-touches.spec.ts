import { expect, seedPlace, test } from "../support/gallery";

test("the x in the query box clears the words and every filter", async ({ page, gallery }) => {
  await page.goto(`${gallery.baseURL}/?q=${encodeURIComponent("type:image ext:jpg")}`);
  const box = page.locator(".gallery-toolbar #qbox");
  await expect(box.locator(".qchip")).toHaveCount(2);
  const clear = box.locator(".qbox-clear");
  await expect(clear).toBeVisible();
  await clear.click();
  await expect(page).toHaveURL(`${gallery.baseURL}/`);
  await expect(box.locator(".qchip")).toHaveCount(0);
  await expect(box.locator(".qbox-clear")).toBeHidden();

  // Words alone: the x shows once something is typed, and empties the box.
  await box.locator("input").fill("deniz");
  await expect(box.locator(".qbox-clear")).toBeVisible();
  await box.locator(".qbox-clear").click();
  await expect(box.locator("input")).toHaveValue("");
});

test("the lightbox copies the file name", async ({ page, context, gallery }) => {
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);
  await page.goto(gallery.baseURL);
  await page.locator("#gallery [data-lb-url]").first().click();
  const name = await page.locator("#lbMeta .lb-fname span").textContent();
  const copy = page.locator("#lbMeta .lb-copy");
  await copy.click();
  await expect(copy).toHaveAttribute("title", "Copied");
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe(name);
});

test("the more menu offers Help, which opens the docs", async ({ page, gallery }) => {
  await page.goto(gallery.baseURL);
  await page.locator("#secnav-more").click();
  const help = page.getByRole("menuitem", { name: "Help" });
  await expect(help).toHaveAttribute("href", "https://docs.videre.sh/commands/gallery/");
  await expect(help).toHaveAttribute("target", "_blank");
});

test("the map's Radius input looks like the toolbar's other controls", async ({ page, isolatedGallery: gallery }) => {
  seedPlace(gallery.libraryRoot);
  await page.goto(`${gallery.baseURL}/map`);
  const styles = await page.evaluate(() => {
    document.getElementById("map-radius-group")!.hidden = false;
    const radius = getComputedStyle(document.getElementById("map-radius")!);
    const label = getComputedStyle(document.querySelector("#map-radius-group label")!);
    const sort = getComputedStyle(document.querySelector(".gallery-toolbar select")!);
    return { bg: radius.backgroundColor, color: radius.color, label: label.color, sortColor: sort.color };
  });
  expect(styles.bg).toBe("rgb(255, 255, 255)");
  expect(styles.color).toBe("rgb(24, 24, 27)");
  expect(styles.label).toBe(styles.sortColor);
});
