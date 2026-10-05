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

// A library with thousands of unclustered faces used to request every crop
// at once; the browser queued them, and an assignment waited behind the
// queue (tens of seconds when crops had to be cut from photos on a slow
// drive). Only the crops near the screen are requested now.
test("people pages request only the face crops near the screen", async ({
  page,
  isolatedGallery: gallery
}) => {
  const { DatabaseSync } = await import("node:sqlite").catch(() => ({ DatabaseSync: undefined }));
  test.skip(!DatabaseSync, "node:sqlite is unavailable on this Node");
  const db = new DatabaseSync!(`${gallery.libraryRoot}/.videre/hashes.db`);
  db.exec("PRAGMA busy_timeout = 5000");
  const { hash } = db
    .prepare("SELECT hash FROM file_hashes WHERE lower(ext) IN ('jpg','jpeg') ORDER BY path LIMIT 1")
    .get() as { hash: string };
  db.prepare("INSERT OR IGNORE INTO people (name, full_name) VALUES ('ayse', 'Ayşe')").run();
  const single = db.prepare("INSERT INTO faces (hash, bbox, embedding, blur) VALUES (?, ?, ?, 500.0)");
  const named = db.prepare(
    "INSERT INTO faces (hash, bbox, embedding, person_label, confirmed, blur) VALUES (?, ?, ?, 'ayse', 1, 500.0)"
  );
  for (let i = 0; i < 300; i++) {
    single.run(hash, `${i % 20},0,10,10`, embedding(i % 512));
    named.run(hash, `0,${i % 20},10,10`, embedding(i % 512));
  }
  db.close();

  const crops = (path: string) =>
    new Promise<number>(async (resolve) => {
      let count = 0;
      const seen = (r: { url(): string }) => {
        if (/\/api\/faces\/\d+\/image$/.test(r.url())) count++;
      };
      page.on("request", seen);
      await page.goto(`${gallery.baseURL}${path}`);
      await page.waitForLoadState("networkidle");
      page.off("request", seen);
      resolve(count);
    });

  const onPeople = await crops("/people");
  await expect(page.locator("img.face-img")).not.toHaveCount(0);
  // The one face a learning question shows is on screen; it loads at once.
  expect(await page.locator("img.face-img:not([loading=lazy]):not(.q-face)").count()).toBe(0);
  expect(onPeople, "the 300 unclustered faces are not all requested").toBeLessThan(200);

  const onPerson = await crops("/people/person/ayse");
  await expect(page.locator("#face-count")).toHaveText("300 face(s)");
  expect(await page.locator("img.face-img:not([loading=lazy])").count()).toBe(0);
  expect(onPerson, "the person's 300 faces are not all requested").toBeLessThan(200);
});
