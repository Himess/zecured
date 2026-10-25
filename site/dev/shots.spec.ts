import { mkdirSync } from "node:fs";
import { resolve } from "node:path";
import { type Page, test } from "@playwright/test";

// `pnpm shots`: the page's still image (site/img/still.jpg, shown before the scene runs and instead of it without
// WebGL or motion), the social card (site/og.png), the README's images (docs/assets), the frames of
// docs/assets/loan.gif (scripts/site-gif.sh assembles them) and full pages for review (site/dev/shots, not
// committed). Reduced motion keeps the text still; the scene is frozen on one frame of the hero.

const repo = resolve(import.meta.dirname, "..", "..");
const assets = resolve(repo, "docs", "assets");
const shots = resolve(import.meta.dirname, "shots");
mkdirSync(assets, { recursive: true });
mkdirSync(resolve(shots, "loan"), { recursive: true });
mkdirSync(resolve(repo, "site", "img"), { recursive: true });

async function open(page: Page, path: string, width: number, height: number) {
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.setViewportSize({ width, height });
  await page.goto(path);
  await page.waitForLoadState("networkidle");
  await page.evaluate(() => document.fonts.ready);
}

/** Puts a frozen frame of the hero's scene behind the page, the same frame every time. */
async function freezeScene(page: Page) {
  await page.evaluate(async () => {
    const stage = document.querySelector(".stage");
    const old = document.getElementById("scene");
    if (!stage || !old) throw new Error("no stage");
    old.remove();
    const canvas = document.createElement("canvas");
    canvas.id = "scene";
    stage.insertBefore(canvas, stage.querySelector(".veil"));
    const { startScene } = await import(new URL("scene.js", document.baseURI).href);
    const scene = startScene(canvas, { tags: document.querySelector(".stage .tags") });
    // Where the hero's text ends, measured as main.js measures it, so the mark stands where the page puts it.
    let edge = 0;
    document.querySelectorAll(".hero-in > .eyebrow, .hero-in > .lead, .display .line > span, .ctas > *").forEach((el) => {
      edge = Math.max(edge, el.getBoundingClientRect().right / window.innerWidth);
    });
    scene.still(3.1, "hero", 0, edge);
    document.documentElement.classList.add("scene-on");
  });
  await page.waitForTimeout(1600);
}

test("the still image of the scene", async ({ page }) => {
  await open(page, "/", 1920, 1080);
  await freezeScene(page);
  await page.evaluate(() => {
    for (const el of document.querySelectorAll<HTMLElement>(".top, main, .foot, .veil, .tags")) el.style.visibility = "hidden";
  });
  await page.screenshot({ path: resolve(repo, "site", "img", "still.jpg"), type: "jpeg", quality: 82 });
});

test("the social card, 1200 by 630", async ({ page }) => {
  await open(page, "/", 1200, 630);
  await freezeScene(page);
  await page.evaluate(() => {
    for (const el of document.querySelectorAll<HTMLElement>(".links, .top .btn, .eyebrow, .lead, .ctas, .cue, .tags")) el.style.visibility = "hidden";
  });
  await page.screenshot({ path: resolve(repo, "site", "og.png"), clip: { x: 0, y: 0, width: 1200, height: 630 } });
});

test("the README's hero, at 1440 px and twice the density", async ({ browser }) => {
  const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, deviceScaleFactor: 2 });
  const page = await context.newPage();
  await open(page, "/", 1440, 900);
  await freezeScene(page);
  await page.screenshot({ path: resolve(assets, "hero.png") });
  await context.close();
});

test("the theft timeline", async ({ browser }) => {
  const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, deviceScaleFactor: 2 });
  const page = await context.newPage();
  await open(page, "/docs/", 1440, 900);
  await page.locator("#theft").screenshot({ path: resolve(assets, "theft.png") });
  await context.close();
});

test("the frames of the loan's five steps, then the viewing key", async ({ page }) => {
  await open(page, "/docs/", 1100, 1300);
  const body = page.locator("#loan-body");
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

for (const [name, path] of [
  ["landing", "/"],
  ["docs", "/docs/"],
] as const) {
  for (const width of [390, 768, 1024, 1440, 1920]) {
    test(`the ${name} page in full at ${width} px`, async ({ page }) => {
      await open(page, path, width, width === 1920 ? 1080 : 900);
      await page.screenshot({ path: resolve(shots, `${name}-${width}.png`), fullPage: true });
    });
  }
}
