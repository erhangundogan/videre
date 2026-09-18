import { defineConfig, devices } from "@playwright/test";

export default defineConfig({
  testDir: "./tests",
  timeout: 30_000,
  fullyParallel: false,
  forbidOnly: !!process.env.CI,
  retries: 0,
  reporter: [["html", { open: "never" }], ["line"]],
  use: {
    ...devices["Desktop Chrome"],
    headless: true,
    viewport: { width: 1280, height: 900 },
    trace: "retain-on-failure",
    video: "retain-on-failure",
    screenshot: "only-on-failure"
  }
});
