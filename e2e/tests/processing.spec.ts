import { expect, test } from "../support/gallery";

// The nav's "still processing" note shows only while `videre watch` reports
// outstanding work (GET /api/processing, covered by the Rust route tests), and
// opens a list of what each stage still has to do.

test("the nav says what watch is still processing and hides at zero", async ({ page, gallery }) => {
  let body = { watch: true, stages: [{ stage: "faces", outstanding: 12 }, { stage: "locations", outstanding: 3 }] };
  await page.route("**/api/processing", (route) => route.fulfill({ json: body }));
  await page.goto(`${gallery.baseURL}/`);

  const note = page.locator("#secnav-processing-btn");
  await expect(note).toHaveText("12 still processing");
  await note.click();
  const list = page.locator("#secnav-processing-list");
  await expect(list).toBeVisible();
  await expect(list.locator("li")).toHaveText(["12 faces to detect", "3 photos to place"]);

  // Outside click closes the list.
  await page.locator("body").click({ position: { x: 5, y: 400 } });
  await expect(list).toBeHidden();

  // Nothing left: the note goes away on the next poll.
  body = { watch: true, stages: [] };
  await page.evaluate(() => document.dispatchEvent(new Event("visibilitychange")));
  await expect(page.locator("#secnav-processing")).toBeHidden();
});

test("no note while no watcher runs", async ({ page, gallery }) => {
  await page.goto(`${gallery.baseURL}/`);
  // The real endpoint: this test library has no watcher.
  await page.waitForResponse("**/api/processing");
  await expect(page.locator("#secnav-processing")).toBeHidden();
});
