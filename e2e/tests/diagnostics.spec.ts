import { appendFile, mkdir } from "node:fs/promises";
import { join } from "node:path";
import { expect, test, type GallerySession } from "../support/gallery";

// One faces log line as the JSON file layer writes it.
function line(ts: string, level: string, message: string, path = ""): string {
  return (
    JSON.stringify({
      timestamp: ts,
      level,
      fields: { message, kind: "source_unavailable", path },
      spans: [{ name: "run", command: "faces", run: "R1" }]
    }) + "\n"
  );
}

async function seedFacesLog(gallery: GallerySession): Promise<void> {
  const dir = join(gallery.libraryRoot, ".videre", "logs");
  await mkdir(dir, { recursive: true });
  const now = Date.now();
  const at = (minutesAgo: number) => new Date(now - minutesAgo * 60000).toISOString();
  await appendFile(
    join(dir, "faces.log"),
    line(at(30), "WARN", "QuickLook timed out", join(gallery.libraryRoot, "çiçek.HEIC")) +
      line(at(20), "ERROR", "Şehir failed") +
      line(at(10), "WARN", "eski uyarı")
  );
}

test("the menu opens Diagnostics, and each tab shows its sections", async ({ page, gallery }) => {
  await page.goto(gallery.baseURL);
  await page.locator("#secnav-more").click();
  await page.getByRole("menuitem", { name: "Diagnostics" }).click();
  await expect(page).toHaveURL(/\/diagnostics\/status$/);
  await expect(page.getByRole("heading", { name: "Diagnostics" })).toBeVisible();
  for (const title of ["Coverage", "Runs", "Next"]) {
    await expect(page.locator(".diag-section h2", { hasText: title })).toBeVisible();
  }

  await page.locator(".tool-tabs a", { hasText: "Stats" }).click();
  await expect(page).toHaveURL(/\/diagnostics\/stats$/);
  await expect(page.locator(".diag-card").first()).toContainText("Files");
  for (const title of ["By type", "Disk used by videre"]) {
    await expect(page.locator(".diag-section h2", { hasText: title })).toBeVisible();
  }

  await page.locator(".tool-tabs a", { hasText: "Logs" }).click();
  await expect(page).toHaveURL(/\/diagnostics\/logs$/);
  await expect(page.locator("#diag-level")).toHaveValue("warn");
});

test("a recent problem links to its command's log lines", async ({ page, isolatedGallery: gallery }) => {
  await seedFacesLog(gallery);
  await page.goto(`${gallery.baseURL}/diagnostics/status`);
  const problem = page.locator(".diag-section", { hasText: "Recent problems" }).locator("tr", { hasText: "faces" });
  await expect(problem).toContainText("1 error(s), 2 warning(s)");
  await problem.getByRole("link", { name: "Logs" }).click();
  await expect(page).toHaveURL(/\/diagnostics\/logs\?command=faces/);
  await expect(page.locator("#diag-command")).toHaveValue("faces");
  await expect(page.locator("#diag-rows tr")).toHaveCount(3);
  await expect(page.locator("#diag-rows tr").first()).toContainText("eski uyarı");
});

test("the log filters narrow the lines and survive a reload", async ({ page, isolatedGallery: gallery }) => {
  await seedFacesLog(gallery);
  await page.goto(`${gallery.baseURL}/diagnostics/logs`);
  const rows = page.locator("#diag-rows tr");
  await expect(rows.filter({ hasText: "faces" })).toHaveCount(3);

  await page.locator("#diag-level").selectOption("error");
  await expect(rows).toHaveCount(1);
  await expect(rows.first()).toContainText("Şehir failed");

  await page.locator("#diag-level").selectOption("warn");
  await page.locator("#diag-q").fill("ÇİÇEK");
  await expect(rows).toHaveCount(1);
  await expect(rows.first()).toContainText("QuickLook timed out");
  await expect(rows.first().locator("a")).toHaveAttribute("href", /^\/\?q=path/);

  await page.reload();
  await expect(page.locator("#diag-q")).toHaveValue("ÇİÇEK");
  await expect(rows).toHaveCount(1);
});

test("Status reads again every 10 seconds while watch runs, and only then", async ({ page, gallery }) => {
  await page.clock.install();
  let calls = 0;
  let running = true;
  await page.route("**/api/diagnostics/status", async (route) => {
    calls += 1;
    const response = await route.fetch();
    const doc = await response.json();
    doc.report.watch.running = running;
    await route.fulfill({ response, json: doc });
  });
  await page.goto(`${gallery.baseURL}/diagnostics/status`);
  await expect(page.locator(".diag-head")).toContainText("watch running");
  expect(calls).toBe(1);

  await page.clock.runFor(10_000);
  await expect.poll(() => calls).toBe(2);

  // Once watch has stopped, the page stops asking.
  running = false;
  await page.clock.runFor(10_000);
  await expect.poll(() => calls).toBe(3);
  await expect(page.locator(".diag-head")).toContainText("watch not running");
  await page.clock.runFor(30_000);
  expect(calls).toBe(3);
});
