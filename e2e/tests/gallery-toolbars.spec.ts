import { expect, preferListView, seedPlace, seedTrips, test } from "../support/gallery";
import type { GallerySession } from "../support/gallery";

async function settings(page: import("@playwright/test").Page, gallery: GallerySession) {
  return (await page.request.get(`${gallery.baseURL}/api/settings`)).json();
}

test("the map sorts its files and keeps its own sort choice", async ({ page, sortedGallery }) => {
  await preferListView(sortedGallery);
  // A map with no places shows the empty state, so give it one.
  seedPlace(sortedGallery.libraryRoot);
  await page.goto(`${sortedGallery.baseURL}/map`);
  await expect(page.locator("#gallery .card")).toHaveCount(3);

  const settled = page.waitForResponse(
    (r) => r.url().includes("/api/files?") && r.url().includes("sort=name&dir=desc")
  );
  await page.locator(".gallery-toolbar .sort-select").selectOption("name");
  await settled;
  await expect.poll(async () => (await settings(page, sortedGallery)).effective.routes.map.sort.field).toBe("name");
  // Library and Date keep theirs.
  expect((await settings(page, sortedGallery)).effective.routes.files.sort.field).toBe("date");
});

test("the Events overview sorts events by its own fields", async ({ page, isolatedGallery: gallery }) => {
  // With no trips the page shows the empty state, and no toolbar.
  seedTrips(gallery.libraryRoot);
  await page.goto(`${gallery.baseURL}/events`);
  const select = page.locator(".gallery-toolbar .sort-select");
  await expect(select.locator("option")).toHaveText(["Date", "Files", "Length", "Name"]);

  const events = [
    { key: "a", title: "Zagreb", start: "2020-01-01 10:00:00", end: "2020-01-05 10:00:00", count: 3 },
    { key: "b", title: "Ankara", start: "2021-06-01 10:00:00", end: "2021-06-02 10:00:00", count: 9 },
    { key: "c", title: "İzmir", start: "2019-03-01 10:00:00", end: "2019-03-10 10:00:00", count: 5 }
  ];
  const order = (field: string, dir: string) =>
    page.evaluate(([evs, f, d]) => {
      saveSetting("routes.events.sort", { field: f, dir: d });
      return sortEvents(evs).map((e: { key: string }) => e.key);
    }, [events, field, dir] as const);
  expect(await order("date", "desc")).toEqual(["b", "a", "c"]);
  expect(await order("files", "desc")).toEqual(["b", "c", "a"]);
  expect(await order("length", "desc")).toEqual(["c", "a", "b"]);
  expect(await order("name", "asc")).toEqual(["b", "c", "a"]);

  await select.selectOption("files");
  await expect.poll(async () => (await settings(page, gallery)).effective.routes.events.sort.field).toBe("files");
  expect((await settings(page, gallery)).effective.routes.files.sort.field).toBe("date");
});

test("the Similar and search results sit under the toolbar", async ({ page, gallery }) => {
  await page.goto(gallery.baseURL);
  const next = await page.evaluate(
    () => document.querySelector(".gallery-toolbar[data-files]")?.nextElementSibling?.id
  );
  expect(next).toBe("results");
});

test("a person's page keeps its controls in the toolbar", async ({ page, gallery }) => {
  await page.goto(`${gallery.baseURL}/people/person/Ay%C5%9Fe`);
  const bar = page.locator(".gallery-toolbar");
  await expect(bar).toHaveCount(1);
  for (const id of ["#backLink", "#renameInput", "#removeBtn"]) {
    await expect(bar.locator(id)).toHaveCount(1);
  }
  // The toolbar sits directly under the nav, above the page's content.
  expect(await page.evaluate(() => document.querySelector(".gallery-toolbar + main") !== null)).toBe(true);
});

test("a cluster's page keeps its title, count and actions in the shared toolbar", async ({ page, gallery }) => {
  await page.goto(`${gallery.baseURL}/people/cluster/1`);
  const bar = page.locator(".gallery-toolbar");
  await expect(bar).toHaveCount(1);
  await expect(bar.locator("a")).toContainText("Back to people");
  await expect(bar.locator("strong")).toHaveText("Cluster 1");
  for (const id of ["#face-count", "#person-input"]) {
    await expect(bar.locator(id)).toHaveCount(1);
  }
  await expect(bar.getByRole("button", { name: "Assign cluster" })).toBeVisible();
  await expect(bar.getByRole("button", { name: /Dissolve cluster/ })).toBeVisible();
  expect(await page.evaluate(() => document.querySelector(".gallery-toolbar + main") !== null)).toBe(true);
});

test("toolbar controls look the same on every page", async ({ page, gallery }) => {
  // One shared rule styles a toolbar's plain buttons: the Library's Select,
  // the People Recluster toggle and a cluster's Dissolve all share its border.
  const border = async (url: string, selector: string) => {
    await page.goto(`${gallery.baseURL}${url}`);
    return page.locator(`.gallery-toolbar ${selector}`).first().evaluate((el) => getComputedStyle(el).borderTopColor);
  };
  const library = await border("/", ".select-toggle");
  expect(await border("/people", "#recluster-toggle")).toBe(library);
  expect(await border("/people/cluster/1", "button.danger")).toBe(library);
});

declare function saveSetting(path: string, value: unknown): void;
declare function sortEvents<T>(evs: T[]): T[];
