import { defineConfig, devices } from "@playwright/test";

// The site's checks (docs/SITE.md): `pnpm test`. Its images, the README's and the social card: `pnpm shots`,
// which writes site/img/still.jpg, site/og.jpg, docs/assets/*.png and site/dev/shots/ (not committed).
export default defineConfig({
  testDir: ".",
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  reporter: [["list"]],
  use: {
    baseURL: "http://127.0.0.1:8091",
    ...devices["Desktop Chrome"],
  },
  webServer: {
    command: "node serve.mjs",
    url: "http://127.0.0.1:8091/",
    reuseExistingServer: !process.env.CI,
    env: { PORT: "8091" },
  },
  projects: [
    // A software renderer draws the scene a few frames a second: the page answers slowly while it runs.
    { name: "site", testMatch: /site\.spec\.ts/, timeout: 90_000 },
    { name: "shots", testMatch: /shots\.spec\.ts/, timeout: 120_000, fullyParallel: false },
  ],
});
