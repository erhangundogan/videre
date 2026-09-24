import { join } from "node:path";
import { expect, test } from "../support/gallery";

test("renders grid thumbnails at 480px", async ({ page, gallery }) => {
  await page.goto(gallery.baseURL);
  const img = page.locator("#gallery img.thumb").first();
  await expect(img).toBeVisible();
  await expect(img).toHaveAttribute("src", /size=480/);

  const src = await img.getAttribute("src");
  const response = await page.request.get(new URL(src!, gallery.baseURL).toString());
  expect(response.status()).toBe(200);
  expect(response.headers()["content-type"]).toMatch(/^image\//);
});

test("shows the item count on a drilled-down day page", async ({ page, gallery }) => {
  await page.goto(`${gallery.baseURL}/date/2021/08/10`);
  await expect(page.locator("#dateBreadcrumb")).toContainText("2021-08-10");
  // Two seeded copies of the same image share one content hash, and the
  // date views show one row per hash, so one item.
  await expect(page.locator("#dateBreadcrumb .date-period-count")).toHaveText("1 item");
});

test("shows the item count on a drilled-down year page", async ({ page, gallery }) => {
  await page.goto(`${gallery.baseURL}/date/2021`);
  await expect(page.locator("#dateGrid [data-lb-url]").first()).toBeVisible();
  await expect(page.locator("#dateBreadcrumb .date-period-count")).toHaveText("1 item");
});

test("toggles fullscreen from the lightbox and hides the toggle for videos", async ({ page, gallery }) => {
  await page.goto(gallery.baseURL);

  // Photos: the toggle fullscreens the whole lightbox, so the arrows and the
  // info panel stay usable at screen size.
  await page.locator("#gallery [data-lb-type='image']").first().click();
  await expect(page.locator("#lb")).toHaveClass(/on/);
  const fsButton = page.locator("#lb-fs");
  await expect(fsButton).toBeVisible();

  await fsButton.click();
  await expect.poll(() => page.evaluate(() => document.fullscreenElement?.id ?? null)).toBe("lb");
  await expect(fsButton).toHaveAttribute("aria-label", "Exit fullscreen");

  await fsButton.click();
  await expect.poll(() => page.evaluate(() => document.fullscreenElement)).toBeNull();
  await expect(fsButton).toHaveAttribute("aria-label", "Fullscreen");

  // Videos carry fullscreen in their own controls; a second button on the
  // same player would be noise.
  await page.keyboard.press("Escape");
  await expect(page.locator("#lb")).not.toHaveClass(/on/);
  await page.locator("#gallery [data-lb-type='video']").first().click();
  await expect(page.locator("#lb")).toHaveClass(/on/);
  await expect(fsButton).toBeHidden();
});

test("focuses the person-name input when New Person is clicked", async ({ page, gallery }) => {
  // The People page needs face rows, and E2E never runs the face models.
  // Seed one unassigned cluster straight into the library database: two
  // faces sharing a cluster id on the already-scanned image, which is the
  // state `videre faces` would have produced.
  const { DatabaseSync } = await import("node:sqlite").catch(() => {
    return { DatabaseSync: undefined };
  });
  test.skip(!DatabaseSync, "node:sqlite is unavailable on this Node");
  const db = new DatabaseSync!(join(gallery.libraryRoot, ".videre", "hashes.db"));
  // The running gallery server writes to this database too (background
  // training, lazily created tables), so wait briefly instead of failing on
  // a transient write lock.
  db.exec("PRAGMA busy_timeout = 5000");
  db.prepare(
    "INSERT INTO faces (hash, bbox, embedding, cluster_id) VALUES ('h1', '0,0,50,50', X'0000', 7), ('h1', '60,0,50,50', X'0000', 7)"
  ).run();

  // The library is shared by every test in this worker, so the seeded faces
  // are removed afterwards: left behind, they hide the "No faces detected
  // yet" state from whichever People test the worker runs next.
  try {
    await page.goto(`${gallery.baseURL}/people`);
    const card = page.locator(".cluster-card").first();
    await expect(card).toBeVisible();

    await card.locator(".new-person-btn").click();
    await expect(page.locator(".np-input").first()).toBeFocused();
  } finally {
    db.prepare("DELETE FROM faces WHERE hash = 'h1'").run();
    db.close();
  }
});

test("identity question controls stay hidden without a trained profile", async ({
  page,
  gallery,
}) => {
  await page.goto(`${gallery.baseURL}/people`);
  const card = page.locator("#question-card");
  await expect(card).toBeHidden();
  await expect(page.locator("#q-yes")).toBeHidden();
  await expect(page.locator("#q-no")).toBeHidden();
  await expect(page.locator("#q-skip")).toBeHidden();
});
