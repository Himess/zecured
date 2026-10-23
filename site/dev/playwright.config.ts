import { defineConfig, devices } from "@playwright/test";

// The site's checks (docs/SITE.md): `pnpm test`. Its images, the README's and the social card: `pnpm shots`,
// which writes site/img/still.jpg, site/og.png, docs/assets/*.png and site/dev/shots/ (not committed).
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
    { name: "site", testMatch: /site\.spec\.ts/ },
    { name: "shots", testMatch: /shots\.spec\.ts/, timeout: 120_000, fullyParallel: false },
  ],
});
