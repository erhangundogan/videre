import { DatabaseSync } from "node:sqlite";
import { join } from "node:path";
import { expect, test } from "../support/gallery";

function openDatabase(libraryRoot: string): DatabaseSync {
  return new DatabaseSync(join(libraryRoot, ".videre", "hashes.db"));
}

function seedClusters(libraryRoot: string): void {
  const db = openDatabase(libraryRoot);
  const files = db.prepare("SELECT path FROM file_hashes ORDER BY path").all() as Array<{ path: string }>;
  expect(files.length).toBeGreaterThanOrEqual(2);
  db.exec(
    "DELETE FROM location_clusters;" +
    "UPDATE file_hashes SET location_cluster_id = NULL;" +
    "INSERT INTO location_clusters " +
      "(id, centroid_lat, centroid_lon, name, photo_count, radius_km, created_at) VALUES " +
      "(1, 52.52, 13.405, 'Berlin', 1, 20.0, CURRENT_TIMESTAMP), " +
      "(2, 35.68, 139.69, 'Tokyo', 1, 20.0, CURRENT_TIMESTAMP);"
  );
  db.prepare("UPDATE file_hashes SET location_cluster_id = 1 WHERE path = ?").run(files[0].path);
  db.prepare("UPDATE file_hashes SET location_cluster_id = 2 WHERE path = ?").run(files[1].path);
  db.close();
}

test.describe("map clusters", () => {
  test.beforeEach(async ({ gallery }) => {
    seedClusters(gallery.libraryRoot);
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

  test("clicking a cluster filters the grid to its files", async ({ page, gallery }) => {
    await page.goto(`${gallery.baseURL}/map`);
    await page.locator("#map-zoom-in").click();
    await page.locator("#map-zoom-in").click();
    await page.locator(".map-marker[data-name='Berlin']").click();

    await expect(page.locator("#gallery .card")).toHaveCount(1);

    await page.locator("#map-clear").click();
    await expect(page.locator("#gallery .card")).toHaveCount(3);
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

test("a library that never clustered shows the empty state with a working grid", async ({ page, gallery }) => {
  const db = openDatabase(gallery.libraryRoot);
  db.exec("DELETE FROM location_clusters; UPDATE file_hashes SET location_cluster_id = NULL;");
  db.close();

  await page.goto(`${gallery.baseURL}/map`);
  await expect(page.locator("#map-empty")).toBeVisible();
  await expect(page.locator("#gallery [data-lb-url]").first()).toBeVisible();
});
