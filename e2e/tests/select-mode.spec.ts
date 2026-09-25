import { expect, preferListView, test } from "../support/gallery";

// The sorted library has three distinct files, so three items to select.
test.beforeEach(async ({ sortedGallery }) => {
  await preferListView(sortedGallery);
});

test("select mode selects items instead of opening the lightbox", async ({ page, sortedGallery }) => {
  await page.goto(sortedGallery.baseURL);
  const cards = page.locator("#gallery .card[data-hash]");
  await expect(cards).toHaveCount(3);

  const toggle = page.locator(".gallery-toolbar .select-toggle").first();
  await toggle.click();
  await expect(toggle).toHaveAttribute("aria-pressed", "true");
  await expect(page.locator(".select-state").first()).toHaveText("Select enabled");

  await cards.nth(0).locator("[data-lb-url]").click();
  await expect(page.locator("#lb")).not.toHaveClass(/on/);
  await expect(cards.nth(0)).toHaveClass(/selected/);
  await expect(page.locator("#file-sel-bar .sel-count")).toHaveText("1 selected");

  // Shift-click selects the run from the last plain click.
  await cards.nth(2).click({ modifiers: ["Shift"] });
  await expect(page.locator("#file-sel-bar .sel-count")).toHaveText("3 selected");
  await expect(page.locator("#gallery .card.selected")).toHaveCount(3);

  // The selection survives a switch to tile view.
  await page.locator(".view-mode-select").first().selectOption("tile");
  await expect(page.locator("#gallery .tile.selected")).toHaveCount(3);

  // A plain click toggles one off; Escape clears the rest.
  await page.locator("#gallery .tile[data-hash]").nth(1).click();
  await expect(page.locator("#file-sel-bar .sel-count")).toHaveText("2 selected");
  await page.keyboard.press("Escape");
  await expect(page.locator("#file-sel-bar")).not.toHaveClass(/on/);

  // Off again: clicks open the lightbox.
  await toggle.click();
  await expect(page.locator(".select-state").first()).toBeHidden();
  await page.locator("#gallery .tile [data-lb-url]").first().click();
  await expect(page.locator("#lb")).toHaveClass(/on/);
});

test("People singletons select through the shared bar, with Shift ranges", async ({ page, gallery }) => {
  const { DatabaseSync } = await import("node:sqlite").catch(() => ({ DatabaseSync: undefined }));
  test.skip(!DatabaseSync, "node:sqlite is unavailable on this Node");
  const db = new DatabaseSync!(`${gallery.libraryRoot}/.videre/hashes.db`);
  db.exec("PRAGMA busy_timeout = 5000");
  db.prepare(
    "INSERT INTO faces (hash, bbox, embedding) VALUES ('p1','0,0,50,50',X'0000'),('p2','0,0,50,50',X'0000'),('p3','0,0,50,50',X'0000')"
  ).run();
  try {
    await page.goto(`${gallery.baseURL}/people`);
    const zones = page.locator(".singleton-card .sel-zone");
    await expect(zones).toHaveCount(3);
    await zones.nth(0).click({ position: { x: 60, y: 60 } });
    await expect(page.locator("#sel-bar .sel-count")).toHaveText("1 selected");
    await expect(page.locator("#sel-bar")).toContainText("New Person");
    await zones.nth(2).click({ position: { x: 60, y: 60 }, modifiers: ["Shift"] });
    await expect(page.locator("#sel-bar .sel-count")).toHaveText("3 selected");
    await expect(page.locator(".singleton-card.selected .sel-check").first()).toBeVisible();
    await page.locator("#sel-bar [data-sel-clear]").click();
    await expect(page.locator("#sel-bar")).not.toHaveClass(/on/);
  } finally {
    db.prepare("DELETE FROM faces WHERE hash IN ('p1','p2','p3')").run();
    db.close();
  }
});

test("the bar's actions mark, tag and copy the selection", async ({ page, sortedGallery }) => {
  await page.context().grantPermissions(["clipboard-read", "clipboard-write"]);
  await page.goto(sortedGallery.baseURL);
  await page.locator(".gallery-toolbar .select-toggle").first().click();
  const cards = page.locator("#gallery .card[data-hash]");
  await cards.nth(0).click();
  await cards.nth(1).click({ modifiers: ["Shift"] });
  const bar = page.locator("#file-sel-bar");
  await expect(bar.locator(".sel-count")).toHaveText("2 selected");

  await bar.locator('[data-sel-act="like"]').click();
  await expect(bar.locator(".sel-result")).toHaveText("Liked 2 item(s)");

  await bar.locator(".sel-rate").selectOption("4");
  await expect(bar.locator(".sel-result")).toHaveText("Rated 2 item(s) ★★★★");

  await bar.locator('[data-sel-act="keep"]').click();
  await expect(bar.locator(".sel-result")).toHaveText("Marked 2 item(s) Keep");
  await bar.locator('[data-sel-act="keep"]').click();
  await expect(bar.locator(".sel-result")).toHaveText("Cleared Keep on 2 item(s)");

  await bar.locator(".sel-tag").fill("İstanbul");
  await bar.locator('[data-sel-act="tag"]').click();
  await expect(bar.locator(".sel-result")).toHaveText("Tagged 2 item(s) with İstanbul");
  const tags = await (await page.request.get(`${sortedGallery.baseURL}/api/tags`)).json();
  expect(tags).toEqual([{ tag: "İstanbul", count: 2 }]);
  await bar.locator(".sel-tag").fill("İstanbul");
  await bar.locator('[data-sel-act="untag"]').click();
  await expect(bar.locator(".sel-result")).toHaveText("Untagged 2 item(s) from İstanbul");

  await bar.locator('[data-sel-act="copy"]').click();
  await expect(bar.locator(".sel-result")).toHaveText("Copied 2 path(s)");
  const copied = await page.evaluate(() => navigator.clipboard.readText());
  expect(copied.split("\n")).toHaveLength(2);

  // Restore the fixture's marks (c_clip liked and unrated, a_alpha rated 5 and
  // not liked): the library is shared with the sort specs, which order by them.
  const hashes = await cards.evaluateAll((els) => els.map((e) => (e as HTMLElement).dataset.hash));
  await page.request.post(`${sortedGallery.baseURL}/api/files/marks`, {
    data: { hashes: [hashes[0]], liked: true, rating: 0 }
  });
  await page.request.post(`${sortedGallery.baseURL}/api/files/marks`, {
    data: { hashes: [hashes[1]], liked: false, rating: 5 }
  });
});

test("delete asks first, Cancel keeps everything, confirm moves the item to the Trash", async ({
  page,
  isolatedGallery: gallery
}) => {
  // A library of its own, since this one deletes. first.jpg and second.jpg are
  // one item (same bytes, two cards), so deleting it moves both files.
  await preferListView(gallery);
  await page.goto(gallery.baseURL);
  await page.locator(".gallery-toolbar .select-toggle").first().click();
  const photo = page.locator("#gallery .card[data-hash]").filter({ hasText: /first|second/ }).first();
  await photo.click();
  const bar = page.locator("#file-sel-bar");
  await bar.locator('[data-sel-act="delete"]').click();
  const dialog = page.locator("dialog.sel-confirm");
  await expect(dialog).toContainText("Move 1 item to the Trash?");
  await expect(dialog).toContainText("2 files: 1 photo and 1 extra copy");
  await dialog.locator("[data-no]").click();
  await expect(dialog).toHaveCount(0);
  await expect(page.locator("#gallery .card[data-hash]")).toHaveCount(3);

  await bar.locator('[data-sel-act="delete"]').click();
  await page.locator("dialog.sel-confirm [data-yes]").click();
  await expect(bar).not.toHaveClass(/on/);
  await expect(page.locator("#gallery .card[data-hash]")).toHaveCount(1);
});

test("Clear in the selection bar empties the selection", async ({ page, sortedGallery }) => {
  await page.goto(sortedGallery.baseURL);
  await page.locator(".gallery-toolbar .select-toggle").first().click();
  await page.locator("#gallery .card[data-hash]").first().click();
  await expect(page.locator("#file-sel-bar")).toHaveClass(/on/);
  await page.locator("#file-sel-bar [data-sel-clear]").click();
  await expect(page.locator("#file-sel-bar")).not.toHaveClass(/on/);
  await expect(page.locator("#gallery .card.selected")).toHaveCount(0);
});
