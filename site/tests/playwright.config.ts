import { defineConfig, devices } from "@playwright/test";

// The showcase page's checks (docs/SITE.md): `pnpm test`. The screenshots for the README and the social card:
// `pnpm shots`, which writes docs/assets/*.png, site/og.png and site/tests/shots/ (not committed).
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
    { name: "shots", testMatch: /shots\.spec\.ts/ },
  ],
});
