import { DatabaseSync } from "node:sqlite";
import { join } from "node:path";
import { expect, test } from "../support/gallery";

function openDatabase(libraryRoot: string): DatabaseSync {
  return new DatabaseSync(join(libraryRoot, ".videre", "hashes.db"));
}

function seedClusters(libraryRoot: string): void {
  const db = openDatabase(libraryRoot);
  const files = db.prepare("SELECT path FROM file_hashes ORDER BY path").all() as Array<{ path: string }>;
  const first = files.find((file) => file.path.endsWith("first.jpg"));
  const second = files.find((file) => file.path.endsWith("second.jpg"));
  const video = files.find((file) => file.path.endsWith("clip.mp4"));
  expect(first).toBeTruthy();
  expect(second).toBeTruthy();
  expect(video).toBeTruthy();
  db.exec(
    "DELETE FROM location_clusters;" +
    "UPDATE file_hashes SET location_cluster_id = NULL, gps_lat = NULL, gps_lon = NULL;" +
    "INSERT INTO location_clusters " +
      "(id, centroid_lat, centroid_lon, name, photo_count, radius_km, created_at) VALUES " +
      "(1, 52.52, 13.405, 'Berlin', 1, 20.0, CURRENT_TIMESTAMP), " +
      "(2, 35.68, 139.69, 'Tokyo', 2, 20.0, CURRENT_TIMESTAMP);"
  );
  db.prepare(
    "UPDATE file_hashes SET location_cluster_id = 1, gps_lat = 52.52, gps_lon = 13.405 WHERE path = ?"
  ).run(first!.path);
  db.prepare(
    "UPDATE file_hashes SET location_cluster_id = 2, gps_lat = 52.60, gps_lon = 13.405 WHERE path = ?"
  ).run(second!.path);
  db.prepare(
    "UPDATE file_hashes SET location_cluster_id = 2, gps_lat = 35.68, gps_lon = 139.69 WHERE path = ?"
  ).run(video!.path);
  db.close();
}

test.describe("map clusters", () => {
  // These specs assert the interaction contract, which both renderers honor.
  // Force the canvas fallback so they run deterministically without depending
  // on headless WebGL; the MapLibre path has its own gated specs below.
  test.beforeEach(async ({ page, gallery }) => {
    seedClusters(gallery.libraryRoot);
    await page.addInitScript(() => {
      (window as unknown as { __VIDERE_FORCE_CANVAS_MAP__: boolean }).__VIDERE_FORCE_CANVAS_MAP__ = true;
    });
  });

  test("world view shows continent markers, not clusters", async ({ page, gallery }) => {
    await page.goto(`${gallery.baseURL}/map`);
    await expect(page.locator(".map-marker[data-tier='continent']")).toHaveCount(2);
    await expect(page.locator(".map-marker[data-tier='cluster']")).toHaveCount(0);
  });

  test("zooming in reveals the cluster markers", async ({ page, gallery }) => {
    await page.goto(`${gallery.baseURL}/map`);
    await page.locator("#map-zoom-in").click();
    await page.locator("#map-zoom-in").click();
    await expect(page.locator(".map-marker[data-tier='continent']")).toHaveCount(0);
    await expect(page.locator(".map-marker[data-name='Berlin']")).toBeVisible();
    await expect(page.locator(".map-marker[data-name='Tokyo']")).toBeVisible();
  });

  test("clicking a cluster enters an addressable proximity drill-down", async ({ page, gallery }) => {
    await page.goto(`${gallery.baseURL}/map`);
    await page.locator("#map-zoom-in").click();
    await page.locator("#map-zoom-in").click();
    await page.locator(".map-marker[data-name='Berlin']").click();

    await expect(page).toHaveURL(/\/map\/location\/berlin\?radius=20(?:\.0)?$/);
    await expect(page.locator("#map-breadcrumb")).toContainText("Map > Berlin");
    await expect(page.locator(".map-marker[data-name='Berlin']")).toHaveClass(/active/);
    await expect(page.locator("#gallery .card")).toHaveCount(2);

    await page.locator("#map-clear").click();
    await expect(page.locator("#gallery .card")).toHaveCount(3);
  });

  test("direct location URL lands in the same selected state", async ({ page, gallery }) => {
    await page.goto(`${gallery.baseURL}/map/location/berlin?radius=20`);
    await expect(page.locator("#map-breadcrumb")).toContainText("Map > Berlin");
    await expect(page.locator(".map-marker[data-name='Berlin']")).toHaveClass(/active/);
    await expect(page.locator("#gallery .card")).toHaveCount(2);
  });

  test("changing radius replaces history and grows or shrinks the grid", async ({ page, gallery }) => {
    await page.goto(`${gallery.baseURL}/map/location/berlin?radius=20`);
    const before = await page.evaluate(() => history.length);

    await page.locator("#map-radius").fill("5");
    await page.locator("#map-radius").dispatchEvent("change");
    await expect(page).toHaveURL(/radius=5$/);
    await expect(page.locator("#map-plot-wrap")).toHaveAttribute("data-radius", "5");
    await expect(page.locator("#gallery .card")).toHaveCount(1);
    expect(await page.evaluate(() => history.length)).toBe(before);

    await page.locator("#map-radius").fill("20");
    await page.locator("#map-radius").dispatchEvent("change");
    await expect(page.locator("#map-plot-wrap")).toHaveAttribute("data-radius", "20");
    await expect(page.locator("#gallery .card")).toHaveCount(2);
  });

  test("Clear returns to the unselected map", async ({ page, gallery }) => {
    await page.goto(`${gallery.baseURL}/map/location/berlin?radius=20`);
    await page.locator("#map-clear").click();

    await expect(page).toHaveURL(`${gallery.baseURL}/map`);
    await expect(page.locator("#map-selection-row")).toBeHidden();
    await expect(page.locator("#map-plot-wrap")).not.toHaveAttribute("data-radius", /.+/);
    await expect(page.locator("#gallery .card")).toHaveCount(3);
  });

  test("Escape clears a selection when the lightbox is closed", async ({ page, gallery }) => {
    await page.goto(`${gallery.baseURL}/map/location/berlin?radius=20`);
    await page.keyboard.press("Escape");

    await expect(page).toHaveURL(`${gallery.baseURL}/map`);
    await expect(page.locator("#map-selection-row")).toBeHidden();
    await expect(page.locator("#gallery .card")).toHaveCount(3);
  });

  test("Escape closes an open lightbox without clearing the selection", async ({ page, gallery }) => {
    await page.goto(`${gallery.baseURL}/map/location/berlin?radius=20`);
    await page.locator("#gallery [data-lb-url]").first().click();
    await expect(page.locator("#lb")).toHaveClass(/on/);
    await page.keyboard.press("Escape");

    await expect(page.locator("#lb")).not.toHaveClass(/on/);
    await expect(page).toHaveURL(/\/map\/location\/berlin\?radius=20$/);
    await expect(page.locator("#map-selection-row")).toBeVisible();
    await expect(page.locator("#gallery .card")).toHaveCount(2);
  });

  test("zooming back to the world clears the selection", async ({ page, gallery }) => {
    await page.goto(`${gallery.baseURL}/map/location/berlin?radius=20`);
    for (let click = 0; click < 8; click++) await page.locator("#map-zoom-out").click();

    await expect(page).toHaveURL(`${gallery.baseURL}/map`);
    await expect(page.locator("#map-selection-row")).toBeHidden();
    await expect(page.locator(".map-marker[data-tier='continent']")).toHaveCount(2);
    await expect(page.locator("#gallery .card")).toHaveCount(3);
  });

  test("unknown location keeps the map working without a selection", async ({ page, gallery }) => {
    const response = await page.goto(`${gallery.baseURL}/map/location/not-a-place`);
    expect(response?.status()).toBe(200);
    await expect(page.locator("#map-selection-row")).toBeHidden();
    await expect(page.locator(".map-marker.active")).toHaveCount(0);
    await expect(page.locator("#gallery .card")).toHaveCount(3);
    await expect(page.locator("#map-selection-status")).toHaveText("Unknown location");
    await expect(page.locator("#map-selection-status")).toBeVisible();
  });

  test("the List/Tile toggle works under the map", async ({ page, gallery }) => {
    await page.goto(`${gallery.baseURL}/map`);
    const viewMode = page.locator(".view-mode-select").first();
    await viewMode.selectOption("tile");
    await expect(page.locator("#gallery")).toHaveClass(/tile-mode/);
    await viewMode.selectOption("list");
    await expect(page.locator("#gallery")).not.toHaveClass(/tile-mode/);
  });
});

test("the basemap tile endpoint answers absent or a byte range", async ({ page, gallery }) => {
  // MapLibre reads the archive with a Range request. Absent is tolerated (the
  // fixture never downloads); a served archive answers 200/206.
  const response = await page.request.get(`${gallery.baseURL}/tiles/basemap.pmtiles`, {
    headers: { range: "bytes=0-15" }
  });
  expect([200, 206, 404]).toContain(response.status());
});

test("the vendored map libraries are served on their own route", async ({ page, gallery }) => {
  const js = await page.request.get(`${gallery.baseURL}/vendor/maplibre-gl.js`);
  expect(js.status()).toBe(200);
  expect(js.headers()["cache-control"]).toContain("immutable");
  const pmtiles = await page.request.get(`${gallery.baseURL}/vendor/pmtiles.js`);
  expect(pmtiles.status()).toBe(200);
  const unknown = await page.request.get(`${gallery.baseURL}/vendor/nope.js`);
  expect(unknown.status()).toBe(404);
});

test("the map renders MapLibre with attribution when WebGL is available", async ({ page, gallery }) => {
  seedClusters(gallery.libraryRoot);
  await page.goto(`${gallery.baseURL}/map`);

  // Gate on the same feature-detect the page uses: a headless runner without
  // working WebGL skips to nothing rather than flaking on a renderer it cannot
  // run. The interaction contract is covered by the canvas specs above.
  const webgl = await page.evaluate(() => {
    try {
      const probe = document.createElement("canvas");
      return !!(probe.getContext("webgl2") || probe.getContext("webgl"));
    } catch {
      return false;
    }
  });
  test.skip(!webgl, "no working WebGL in this browser");

  await expect(page.locator("#map-gl canvas.maplibregl-canvas")).toBeVisible({ timeout: 15_000 });
  await expect(page.locator("#map-attribution")).toContainText("OpenStreetMap");
  await page.waitForFunction(
    () => (window as unknown as { maplibreInitialized?: boolean }).maplibreInitialized === true,
    null,
    { timeout: 15_000 }
  );
  // The grid loads independent of the renderer, so the two Berlin/Tokyo files
  // are present even under MapLibre.
  await expect(page.locator("#gallery .card")).toHaveCount(3);
});

test("a library that never clustered shows the empty state with a working grid", async ({ page, gallery }) => {
  const db = openDatabase(gallery.libraryRoot);
  db.exec("DELETE FROM location_clusters; UPDATE file_hashes SET location_cluster_id = NULL;");
  db.close();

  await page.addInitScript(() => {
    (window as unknown as { __VIDERE_FORCE_CANVAS_MAP__: boolean }).__VIDERE_FORCE_CANVAS_MAP__ = true;
  });
  await page.goto(`${gallery.baseURL}/map`);
  await expect(page.locator("#map-empty")).toBeVisible();
  await expect(page.locator("#gallery [data-lb-url]").first()).toBeVisible();
});
