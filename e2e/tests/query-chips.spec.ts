import { expect, test } from "../support/gallery";

// The gallery fixture holds first.jpg, second.jpg and clip.mp4 and no
// embeddings. Assertions go through the URL and the N-of-M line rather than
// the grid, since a video card depends on the browser's codecs.

function chips(page: import("@playwright/test").Page) {
  return page.locator(".qbox .qchip");
}

function applied(page: import("@playwright/test").Page): string | null {
  return new URL(page.url()).searchParams.get("q");
}

test("keyed terms load as chips and words stay text", async ({ page, gallery }) => {
  await page.goto(`${gallery.baseURL}/?q=${encodeURIComponent("type:image -ext:mp4 gün")}`);
  await expect(chips(page)).toHaveCount(2);
  await expect(chips(page).nth(0)).toContainText("type");
  await expect(chips(page).nth(0)).toContainText("image");
  await expect(chips(page).nth(1)).toHaveClass(/qchip-neg/);
  await expect(page.locator("#nav-search")).toHaveValue("gün");
});

test("suggestions complete a key, then a value, into a chip", async ({ page, gallery }) => {
  await page.goto(gallery.baseURL);
  const input = page.locator("#nav-search");
  await input.pressSequentially("ty");
  const options = page.locator("#qbox-suggest [role=option]");
  await expect(options.first()).toContainText("type:");
  await input.press("Tab");
  await expect(input).toHaveValue("type:");
  await expect(options.first()).toContainText("image");
  await input.press("Enter");
  await expect(chips(page)).toHaveCount(1);
  await expect(input).toHaveValue("");
  await input.press("Enter");
  await expect.poll(() => applied(page)).toBe("type:image");
  await expect(page.locator("#query-status")).toContainText("2 of 3");
});

test("a second value of a key joins its chip, as any for a one-valued key", async ({ page, gallery }) => {
  await page.goto(`${gallery.baseURL}/?q=ext:jpg`);
  const input = page.locator("#nav-search");
  await input.pressSequentially("ext:m");
  await expect(page.locator("#qbox-suggest [role=option]").first()).toContainText("mp4");
  await input.press("Enter");
  await expect(chips(page)).toHaveCount(1);
  await expect(chips(page).locator(".qchip-mode")).toHaveText("any");
  await input.press("Enter");
  await expect.poll(() => applied(page)).toBe("(ext:jpg OR ext:mp4)");
  await expect(page.locator("#query-status")).toContainText("3 of 3");
});

test("a chip's not, any/all and remove apply at once", async ({ page, gallery }) => {
  await page.goto(`${gallery.baseURL}/?q=${encodeURIComponent("ext:jpg ext:mp4")}`);
  await expect(chips(page)).toHaveCount(1);
  await expect(page.locator("#query-status")).toContainText("0 of 3");
  await chips(page).locator(".qchip-mode").click();
  await expect.poll(() => applied(page)).toBe("(ext:jpg OR ext:mp4)");

  await chips(page).locator(".qchip-not").click();
  await expect.poll(() => applied(page)).toBe("-(ext:jpg OR ext:mp4)");
  await expect(page.locator("#query-status")).toContainText("0 of 3");

  await chips(page).locator(".qchip-x").click();
  await expect.poll(() => applied(page) ?? "").toBe("");
  await expect(page.locator("#query-status")).toHaveCount(0);
});

test("what chips cannot show stays one text chip, unchanged", async ({ page, gallery }) => {
  for (const q of ["(type:image OR ext:mp4)", "type:image OR ext:mp4"]) {
    await page.goto(`${gallery.baseURL}/?q=${encodeURIComponent(q)}`);
    await expect(chips(page)).toHaveCount(1);
    await expect(chips(page).first()).toHaveClass(/qchip-text/);
    await expect(chips(page).first()).toContainText(q);
  }
});

test("Backspace in an empty box takes the last chip back for editing", async ({ page, gallery }) => {
  await page.goto(`${gallery.baseURL}/?q=${encodeURIComponent("type:image ext:jpg")}`);
  await expect(chips(page)).toHaveCount(2);
  const input = page.locator("#nav-search");
  await input.click();
  await input.press("Backspace");
  await expect(chips(page)).toHaveCount(1);
  await expect(input).toHaveValue("ext:jpg");
});
