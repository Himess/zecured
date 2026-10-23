import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import AxeBuilder from "@axe-core/playwright";
import { expect, type Page, test } from "@playwright/test";
import { build } from "esbuild";
import { sceneBuild } from "./build.mjs";

// The site's own rules: same origin only, every word readable without the script, the docs' steps and
// comparison by keyboard, no motion when asked for none, a working fallback without WebGL, no horizontal scroll.

const WIDTHS = [390, 768, 1024, 1440];
const PAGES = ["/", "/docs/"];

async function open(page: Page, path = "/", width = 1440) {
  await page.setViewportSize({ width, height: 900 });
  await page.goto(path);
  await page.waitForLoadState("load");
  await page.evaluate(() => document.fonts.ready);
}

/** The browser's own notes about the GPU are not the page's problems. */
const browserNoise = (text: string) => text.includes("GL Driver Message") || text.includes("GPU stall");

test("every request stays on the site's own origin, and nothing fails or breaks the policy", async ({ page }) => {
  const requests: string[] = [];
  const problems: string[] = [];
  page.on("request", (r) => requests.push(r.url()));
  page.on("console", (m) => {
    if ((m.type() === "error" || m.type() === "warning") && !browserNoise(m.text())) problems.push(m.text());
  });
  page.on("pageerror", (e) => problems.push(e.message));
  page.on("requestfailed", (r) => problems.push(`failed: ${r.url()}`));
  for (const width of WIDTHS) await open(page, "/", width);
  await page.waitForFunction(() => document.documentElement.classList.contains("scene-on"));
  // Through every chapter, so the scene and the page load whatever they would.
  for (const id of ["how", "swap", "privacy", "trust", "demo"]) {
    await page.locator(`#${id}`).scrollIntoViewIfNeeded();
    await page.waitForTimeout(150);
  }
  await open(page, "/docs/");
  await page.getByRole("button", { name: "Read with your viewing key" }).first().click();
  for (const name of ["Deposit", "Prove", "Borrow", "Close"]) {
    await page.locator(".rail").getByRole("button", { name: new RegExp(`^Step \\d: ${name}`) }).click();
  }
  await page.getByRole("button", { name: "Zecured", exact: true }).click();
  const origin = new URL(page.url()).origin;
  expect(requests.length).toBeGreaterThan(8);
  expect(requests.filter((u) => new URL(u).origin !== origin)).toEqual([]);
  expect(problems).toEqual([]);
});

test("no accessibility violations (axe) on either page, with the docs' states too", async ({ page }) => {
  await page.emulateMedia({ reducedMotion: "reduce" });
  const scan = async () => {
    const result = await new AxeBuilder({ page }).analyze();
    expect(result.violations.map((v) => `${v.id}: ${v.nodes.map((n) => n.target.join(" ")).join(", ")}`)).toEqual([]);
  };
  for (const width of [390, 1440]) {
    await open(page, "/", width);
    await scan();
    await open(page, "/docs/", width);
    await scan();
    await page.getByRole("button", { name: "Read with your viewing key" }).first().click();
    await page.locator(".rail button").nth(4).click();
    await page.getByRole("button", { name: "Zecured", exact: true }).click();
    await scan();
  }
});

test("without the script every word shows, and the controls that need it are not offered", async ({ browser }) => {
  const context = await browser.newContext({ javaScriptEnabled: false, viewport: { width: 1440, height: 900 } });
  const page = await context.newPage();
  await page.goto("/");
  await expect(page.locator("#hero-h")).toBeVisible();
  for (const title of await page.locator(".title").all()) await expect(title).toBeVisible();
  await expect(page.locator(".stage .still")).toBeVisible();
  await page.goto("/docs/");
  for (let n = 1; n <= 5; n += 1) await expect(page.locator(`#loan-step-${n}`)).toBeVisible();
  await expect(page.locator("#sees-bridge")).toBeVisible();
  await expect(page.locator("#sees-zecured")).toBeVisible();
  await expect(page.locator(".rail")).toBeHidden();
  await expect(page.locator(".vk").first()).toBeHidden();
  await expect(page.locator(".seg")).toBeHidden();
  await context.close();
});

test("the scene draws, and the page's tags follow it", async ({ page }) => {
  await open(page);
  await page.waitForFunction(() => document.documentElement.classList.contains("scene-on"));
  await page.waitForTimeout(800);
  // A frame of the scene is no flat colour: a flat frame would compress to a few kilobytes.
  const shot = await page.locator("#scene").screenshot();
  expect(shot.length).toBeGreaterThan(60_000);
  // In the deposit chapter, the vault's tag comes in where the vault is.
  await page.evaluate(() => {
    const el = document.getElementById("how")!;
    window.scrollTo({ top: el.offsetTop + el.offsetHeight * 0.55 - window.innerHeight / 2, behavior: "instant" });
  });
  await expect.poll(async () => Number(await page.locator('[data-tag="vault"]').evaluate((el) => getComputedStyle(el).opacity))).toBeGreaterThan(0.6);
});

test("without WebGL the page keeps its still image and says nothing is wrong", async ({ browser }) => {
  const context = await browser.newContext({ viewport: { width: 1440, height: 900 } });
  await context.addInitScript(() => {
    const original = HTMLCanvasElement.prototype.getContext;
    HTMLCanvasElement.prototype.getContext = function (this: HTMLCanvasElement, kind: string, ...rest: unknown[]) {
      if (kind.startsWith("webgl")) return null;
      return (original as (...a: unknown[]) => unknown).call(this, kind, ...rest);
    } as typeof original;
  });
  const page = await context.newPage();
  const problems: string[] = [];
  page.on("pageerror", (e) => problems.push(e.message));
  page.on("console", (m) => {
    if (m.type() === "error" && !browserNoise(m.text())) problems.push(m.text());
  });
  const requests: string[] = [];
  page.on("request", (r) => requests.push(new URL(r.url()).pathname));
  await page.goto("/");
  await expect(page.locator("html")).toHaveClass(/no-scene/);
  await expect(page.locator(".stage .still")).toHaveCSS("opacity", "1");
  expect(requests).not.toContain("/scene.js");
  expect(problems).toEqual([]);
  await context.close();
});

test("no motion when the reader asks for none", async ({ page }) => {
  await page.emulateMedia({ reducedMotion: "reduce" });
  const requests: string[] = [];
  page.on("request", (r) => requests.push(new URL(r.url()).pathname));
  await open(page);
  await expect(page.locator("html")).toHaveClass(/no-scene/);
  expect(requests).not.toContain("/scene.js");
  expect(await page.evaluate(() => document.getAnimations().length)).toBe(0);
  await expect(page.locator(".lead")).toHaveCSS("opacity", "1");
  await open(page, "/docs/");
  expect(await page.evaluate(() => document.getAnimations().length)).toBe(0);
});

test("the docs' steps work by keyboard, and only one shows at a time", async ({ page }) => {
  await open(page, "/docs/");
  const rail = page.locator(".rail button");
  const steps = page.locator(".loan-step");
  await rail.first().focus();
  await page.keyboard.press("ArrowRight");
  await expect(rail.nth(1)).toBeFocused();
  await expect(rail.nth(1)).toHaveAttribute("aria-pressed", "true");
  await expect(steps.nth(1)).toBeVisible();
  await expect(steps.nth(0)).toBeHidden();
  await page.keyboard.press("End");
  await expect(rail.nth(4)).toHaveAttribute("aria-pressed", "true");
  await page.keyboard.press("Home");
  await expect(rail.nth(0)).toHaveAttribute("aria-pressed", "true");
  const next = page.locator("#loan-step-1 [data-go='1']");
  await next.focus();
  await page.keyboard.press("Enter");
  await expect(steps.nth(1)).toBeVisible();
  await expect(page.locator("#loan-step-2 [data-go='1']")).toBeFocused();
  await expect(page.locator("#loan-live")).toHaveText(/Step 2 of 5/);
  await page.keyboard.press("Space");
  await page.keyboard.press("Space");
  await page.keyboard.press("Space");
  await expect(steps.nth(4)).toBeVisible();
  await expect(page.locator("#loan-step-5 [data-go='1']")).toBeDisabled();
  await expect(page.locator("#loan-step-5 [data-go='-1']")).toBeFocused();
  expect(await steps.evaluateAll((all) => all.filter((s) => !(s as HTMLElement).hidden).length)).toBe(1);
});

test("the viewing key clears what the borrower can read, never the market maker's output", async ({ page }) => {
  await open(page, "/docs/");
  await page.locator(".rail button").nth(4).click();
  const step = page.locator("#loan-step-5");
  const mine = step.locator(".enc.readable");
  const maker = step.locator(".enc:not(.readable)");
  await expect(mine).toHaveAttribute("aria-hidden", "true");
  await expect(step.locator(".lock")).toBeHidden();
  const key = step.getByRole("button", { name: "Read with your viewing key" });
  await key.focus();
  await page.keyboard.press("Enter");
  await expect(key).toHaveAttribute("aria-pressed", "true");
  await expect(mine).toHaveAttribute("aria-hidden", "false");
  await expect(mine).toHaveCSS("filter", "blur(0px)");
  await expect(maker).toHaveAttribute("aria-hidden", "true");
  await expect(maker).toHaveCSS("filter", "blur(6px)");
  await expect(step.locator(".lock")).toBeVisible();
  await expect(page.locator("#loan-step-1 .vk")).toHaveAttribute("aria-pressed", "true");
  await page.keyboard.press("Space");
  await expect(mine).toHaveAttribute("aria-hidden", "true");
});

test("the comparison switches by keyboard", async ({ page }) => {
  await open(page, "/docs/");
  const bridge = page.getByRole("button", { name: "A typical bridge" });
  const ours = page.getByRole("button", { name: "Zecured", exact: true });
  await expect(page.locator("#sees-bridge")).toBeVisible();
  await expect(page.locator("#sees-zecured")).toBeHidden();
  await bridge.focus();
  await page.keyboard.press("ArrowRight");
  await expect(ours).toBeFocused();
  await expect(ours).toHaveAttribute("aria-pressed", "true");
  await expect(page.locator("#sees-zecured")).toBeVisible();
  await expect(page.locator("#sees-bridge")).toBeHidden();
  await expect(page.locator("#sees-zecured mark")).toHaveCount(0);
  await page.keyboard.press("ArrowLeft");
  await expect(page.locator("#sees-bridge mark").first()).toBeVisible();
});

test("a link to a step opens it and scrolls to it", async ({ page }) => {
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.goto("/docs/#loan-step-3");
  await expect(page.locator("#loan-step-3")).toBeVisible();
  await expect(page.locator(".rail button").nth(2)).toHaveAttribute("aria-pressed", "true");
  await expect(page.locator("#loan-step-3")).toBeInViewport();
  // Followed from inside the page, once it has loaded and settled, to a step that was hidden: the browser does
  // not scroll to it by itself.
  await page.waitForLoadState("networkidle");
  await page.evaluate(() => window.scrollTo(0, 0));
  await page.waitForTimeout(250);
  await page.evaluate(() => {
    window.location.hash = "#loan-step-5";
  });
  await expect(page.locator("#loan-step-5")).toBeVisible();
  await expect(page.locator("#loan-step-5")).toBeInViewport();
});

for (const path of PAGES) {
  for (const width of WIDTHS) {
    test(`no horizontal scroll on ${path} at ${width} px`, async ({ page }) => {
      await open(page, path, width);
      const wide = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
      expect(wide).toBeLessThanOrEqual(0);
    });
  }
}

test("every link inside the site has a target", async ({ page }) => {
  const missing: string[] = [];
  await open(page, "/docs/");
  const docsIds = new Set(await page.evaluate(() => Array.from(document.querySelectorAll("[id]"), (el) => el.id)));
  for (const path of PAGES) {
    await open(page, path);
    const result = await page.evaluate(() => ({
      inPage: Array.from(document.querySelectorAll<HTMLAnchorElement>('a[href^="#"]'))
        .map((a) => a.getAttribute("href")!.slice(1))
        .filter((id) => id !== "" && !document.getElementById(id)),
      toDocs: Array.from(document.querySelectorAll<HTMLAnchorElement>('a[href^="docs/#"]'), (a) => a.getAttribute("href")!.slice(6)),
    }));
    missing.push(...result.inPage.map((id) => `${path}#${id}`));
    missing.push(...result.toDocs.filter((id) => !docsIds.has(id)).map((id) => `docs/#${id}`));
  }
  expect(missing).toEqual([]);
});

test("site/scene.js is what site/dev/scene builds to", async () => {
  const out = await build({ ...sceneBuild, write: false });
  const fresh = out.outputFiles[0].text.replace(/\r\n/g, "\n");
  const committed = readFileSync(resolve(import.meta.dirname, "..", "scene.js"), "utf8").replace(/\r\n/g, "\n");
  expect(committed === fresh, "run `pnpm build` in site/dev and commit site/scene.js").toBe(true);
});
