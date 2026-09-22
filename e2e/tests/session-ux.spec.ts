import { expect, test } from "../support/gallery";

// The fixture library has no embeddings, so the semantic search box hides
// itself and the Similar buttons never render.

test("nav shows the videre logo, a Library link, and no search box without embeddings", async ({ page, gallery }) => {
  await page.goto(gallery.baseURL);
  await expect(page.locator(".secnav-brand svg")).toBeVisible();
  // The brand links home but is not one of the highlighted section buttons.
  await expect(page.locator(".secnav a:not(.secnav-brand)").first()).toHaveText("Library");
  await expect(page.locator(".secnav-search")).toBeHidden();
});

test("the home header carries Library/Date/count lines and sections drop it", async ({ page, gallery }) => {
  await page.goto(gallery.baseURL);
  const header = page.locator(".header");
  await expect(header).toBeVisible();
  await expect(header).toContainText("Library:");
  await expect(header).toContainText("Date:");
  await expect(header).toContainText("Files/Embedding Count:");
  // The count moved into the header, so the grid head no longer titles itself.
  await expect(page.locator(".gallery-head")).not.toContainText("All files");

  for (const route of ["/duplicates", "/date", "/map"]) {
    await page.goto(`${gallery.baseURL}${route}`);
    await expect(page.locator(".header")).toHaveCount(0);
  }
});

test("the Show more button uses the primary style", async ({ page, gallery }) => {
  await page.goto(gallery.baseURL);
  // Present in the DOM even when the small fixture library needs no paging.
  await expect(page.locator("#gallery-more")).toHaveClass(/primary/);
});

test("the date breadcrumb roots at All Dates linking to /date", async ({ page, gallery }) => {
  await page.goto(`${gallery.baseURL}/date/2021`);
  const root = page.locator("#dateBreadcrumb a", { hasText: "All Dates" });
  await expect(root).toHaveAttribute("href", "/date");
});

test("the lightbox has both rotate directions and CCW posts dir=ccw", async ({ page, gallery }) => {
  await page.goto(gallery.baseURL);
  await page.locator("#gallery [data-lb-type='image']").first().click();
  await expect(page.locator("#lb")).toHaveClass(/on/);
  await expect(page.locator("#lb-rotate-ccw")).toBeVisible();
  await expect(page.locator("#lb-rotate")).toBeVisible();

  const ccw = page.waitForResponse((r) => {
    const url = new URL(r.url());
    return /\/rotate$/.test(url.pathname) && url.searchParams.get("dir") === "ccw" && r.request().method() === "POST";
  });
  await page.locator("#lb-rotate-ccw").click();
  expect((await ccw).status()).toBe(200);
});

test("the like heart toggles red and persists", async ({ page, gallery }) => {
  await page.goto(gallery.baseURL);
  await page.locator("#gallery [data-lb-type='image']").first().click();
  const heart = page.locator(".lb-like");
  await expect(heart).toBeVisible();
  await expect(heart).not.toHaveClass(/liked/);

  const patch = page.waitForResponse(
    (r) => r.request().method() === "PATCH" && /\/api\/files\//.test(r.url())
  );
  await heart.click();
  expect((await patch).status()).toBe(200);
  await expect(heart).toHaveClass(/liked/);
  await expect(heart.locator("svg")).toHaveCSS("fill", "rgb(239, 68, 68)");

  // Persisted to the same mark `videre mark --like` sets.
  const hash = await heart.getAttribute("data-hash");
  const response = await page.request.get(`${gallery.baseURL}/api/files?hashes=${hash}`);
  const body = await response.json();
  expect(body.files[0].liked).toBe(true);
});

test("the lightbox wheel does not zoom when fit and pans once zoomed", async ({ page, gallery }) => {
  await page.goto(gallery.baseURL);
  await page.locator("#gallery [data-lb-type='image']").first().click();
  const img = page.locator("#lb-img");
  await expect(img).toBeVisible();

  // Events are dispatched on the image element directly so the handlers are
  // exercised regardless of the top-right controls overlapping the tiny (16x12)
  // fixture image.
  const wheelOverImage = (deltaX: number, deltaY: number) =>
    img.evaluate((el, [dx, dy]) => {
      const r = el.getBoundingClientRect();
      el.dispatchEvent(new WheelEvent("wheel", {
        deltaX: dx, deltaY: dy,
        clientX: r.left + r.width / 2, clientY: r.top + r.height / 2,
        bubbles: true, cancelable: true
      }));
    }, [deltaX, deltaY]);
  const tapImage = () =>
    img.evaluate((el) => {
      const r = el.getBoundingClientRect();
      const opts = { clientX: r.left + r.width / 2, clientY: r.top + r.height / 2, bubbles: true, pointerId: 1 };
      el.dispatchEvent(new PointerEvent("pointerdown", opts));
      el.dispatchEvent(new PointerEvent("pointerup", opts));
    });

  // Wheel while fit-to-screen must not zoom: the transform stays cleared.
  await wheelOverImage(0, -200);
  await expect(img).toHaveCSS("transform", "none");

  // A tap zooms to 1:1 (scale 2.5); the stage marks itself zooming.
  await tapImage();
  await expect(page.locator(".lb-stage")).toHaveClass(/zooming/);
  expect(await img.evaluate((el) => getComputedStyle(el).transform)).toMatch(/^matrix\(2\.5,/);

  // Wheel while zoomed must still not change the zoom level: it pans instead
  // (the tiny fixture image has no room to pan, so the scale simply holds).
  await wheelOverImage(0, -200);
  expect(await img.evaluate((el) => getComputedStyle(el).transform)).toMatch(/^matrix\(2\.5,/);
});
