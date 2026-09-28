import { expect, test } from "../support/gallery";

// A 512-dimension f16 unit vector, the shape real face embeddings have.
function embedding(axis: number): Uint8Array {
  const bytes = new Uint8Array(1024);
  bytes[axis * 2 + 1] = 0x3c; // f16 1.0, little-endian
  return bytes;
}

test("a cluster whose photo has two identical copies is assigned, and a rejection says why", async ({
  page,
  isolatedGallery: gallery
}) => {
  const { DatabaseSync } = await import("node:sqlite").catch(() => ({ DatabaseSync: undefined }));
  test.skip(!DatabaseSync, "node:sqlite is unavailable on this Node");
  const db = new DatabaseSync!(`${gallery.libraryRoot}/.videre/hashes.db`);
  db.exec("PRAGMA busy_timeout = 5000");
  const row = db
    .prepare("SELECT hash, path FROM file_hashes WHERE lower(ext) IN ('jpg','jpeg') ORDER BY path LIMIT 1")
    .get() as { hash: string; path: string };
  // The same bytes at a second path: one photo, one set of faces.
  db.prepare("INSERT INTO file_hashes (path, hash, size_bytes, ext) VALUES (?, ?, 1, 'jpg')").run(
    `${row.path}.copy.jpg`,
    row.hash
  );
  const face = db.prepare(
    "INSERT INTO faces (hash, bbox, embedding, cluster_id, blur) VALUES (?, ?, ?, 5, 500.0)"
  );
  face.run(row.hash, "0,0,40,40", embedding(0));
  face.run(row.hash, "50,0,40,40", embedding(0));
  try {
    await page.goto(`${gallery.baseURL}/people/cluster/5`);
    await expect(page.locator("#face-count")).toHaveText("2 face(s)");

    // The page's list goes stale: a third face joins the cluster behind it.
    face.run(row.hash, "0,50,40,40", embedding(0));
    await page.locator("#person-input").fill("Bob");
    await page.getByRole("button", { name: "Assign cluster" }).click();
    await expect(page.locator("#status")).toContainText("cluster 5 has 3 unassigned face(s)");

    // Reloaded, the page lists all three once each, and naming them works.
    await page.reload();
    await expect(page.locator("#face-count")).toHaveText("3 face(s)");
    await page.locator("#person-input").fill("Bob");
    await page.getByRole("button", { name: "Assign cluster" }).click();
    await expect(page.locator("#status")).toHaveText('Assigned 3 face(s) to "Bob"');
  } finally {
    db.close();
  }
});
