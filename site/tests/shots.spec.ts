import { mkdirSync } from "node:fs";
import { resolve } from "node:path";
import { type Page, test } from "@playwright/test";

// `pnpm shots`: the README's images (docs/assets), the social card (site/og.png), the frames of
// docs/assets/loan.gif (scripts/site-gif.sh assembles them) and full pages at four widths for review
// (site/tests/shots, not committed). Reduced motion: every image shows the page at rest.

const repo = resolve(import.meta.dirname, "..", "..");
const assets = resolve(repo, "docs", "assets");
const shots = resolve(import.meta.dirname, "shots");
mkdirSync(assets, { recursive: true });
mkdirSync(resolve(shots, "loan"), { recursive: true });

async function open(page: Page, width: number, height = 900) {
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.setViewportSize({ width, height });
  await page.goto("/");
  await page.waitForLoadState("networkidle");
  await page.evaluate(() => document.fonts.ready);
}

for (const width of [390, 768, 1024, 1440]) {
  test(`full page at ${width} px`, async ({ page }) => {
    await open(page, width);
    await page.screenshot({ path: resolve(shots, `page-${width}.png`), fullPage: true });
  });
}

test("the README's hero, at 1440 px and twice the density", async ({ browser }) => {
  const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, deviceScaleFactor: 2 });
  const page = await context.newPage();
  await open(page, 1440);
  await page.locator("header.hero").screenshot({ path: resolve(assets, "hero.png") });
  await context.close();
});

test("the social card, 1200 by 630", async ({ page }) => {
  await open(page, 1200, 630);
  // The hero without what a card has no use for: the links, the buttons, the caption and the last two ledger rows.
  await page.evaluate(() => {
    const parts = ".nav-r, .hero-actions, .hero-fine, .hero-cap, .lrow.d3, .lrow.d4";
    for (const el of document.querySelectorAll<HTMLElement>(parts)) el.style.display = "none";
  });
  await page.screenshot({ path: resolve(repo, "site", "og.png"), clip: { x: 0, y: 0, width: 1200, height: 630 } });
});

test("the theft timeline", async ({ browser }) => {
  const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, deviceScaleFactor: 2 });
  const page = await context.newPage();
  await open(page, 1440);
  await page.locator("#theft").screenshot({ path: resolve(assets, "theft.png") });
  await context.close();
});

test("the frames of the loan's five steps, then the viewing key", async ({ page }) => {
  await open(page, 1440, 1000);
  const body = page.locator("#loan .wrap");
  const rail = page.locator(".rail button");
  let n = 0;
  const frame = async () => {
    n += 1;
    await body.screenshot({ path: resolve(shots, "loan", `frame-${String(n).padStart(2, "0")}.png`) });
  };
  for (let i = 0; i < 5; i += 1) {
    await rail.nth(i).click();
    await frame();
  }
  await page.locator("#loan-step-5 .vk").click();
  await frame();
  await rail.nth(2).click();
  await frame();
});
