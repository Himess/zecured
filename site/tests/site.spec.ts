import AxeBuilder from "@axe-core/playwright";
import { expect, type Page, test } from "@playwright/test";

// S-12 of the presentation work order, and the page's own rules: same origin only, no horizontal scroll, the
// steps and the comparison by keyboard, everything readable without the script, no motion when asked for none.

const WIDTHS = [390, 768, 1024, 1440];

async function open(page: Page, width = 1440) {
  await page.setViewportSize({ width, height: 900 });
  await page.goto("/");
  await page.waitForLoadState("networkidle");
}

test("every request stays on the page's own origin, and nothing fails or breaks the policy", async ({ page }) => {
  const requests: string[] = [];
  const problems: string[] = [];
  page.on("request", (r) => requests.push(r.url()));
  page.on("console", (m) => {
    if (m.type() === "error" || m.type() === "warning") problems.push(m.text());
  });
  page.on("pageerror", (e) => problems.push(e.message));
  page.on("requestfailed", (r) => problems.push(`failed: ${r.url()}`));
  for (const width of WIDTHS) await open(page, width);
  // Exercise everything that could load something more.
  await page.getByRole("button", { name: "Read with your viewing key" }).first().click();
  for (const name of ["Deposit", "Prove", "Borrow", "Close"]) {
    await page.locator(".rail").getByRole("button", { name: new RegExp(`^Step \\d: ${name}`) }).click();
  }
  await page.getByRole("button", { name: "Zecured", exact: true }).click();
  const origin = new URL(page.url()).origin;
  expect(requests.length).toBeGreaterThan(3);
  expect(requests.filter((u) => new URL(u).origin !== origin)).toEqual([]);
  expect(problems).toEqual([]);
});

test("no accessibility violations (axe), with the script's states too", async ({ page }) => {
  for (const width of [390, 1440]) {
    await open(page, width);
    await page.emulateMedia({ reducedMotion: "reduce" });
    const scan = async () => {
      const result = await new AxeBuilder({ page }).analyze();
      expect(result.violations.map((v) => `${v.id}: ${v.nodes.map((n) => n.target.join(" ")).join(", ")}`)).toEqual([]);
    };
    await scan();
    await page.getByRole("button", { name: "Read with your viewing key" }).first().click();
    await page.locator(".rail button").nth(4).click();
    await page.getByRole("button", { name: "Zecured", exact: true }).click();
    await scan();
  }
});

test("without the script every step and both comparisons show", async ({ browser }) => {
  const context = await browser.newContext({ javaScriptEnabled: false, viewport: { width: 1440, height: 900 } });
  const page = await context.newPage();
  await page.goto("/");
  for (let n = 1; n <= 5; n += 1) await expect(page.locator(`#loan-step-${n}`)).toBeVisible();
  await expect(page.locator("#sees-bridge")).toBeVisible();
  await expect(page.locator("#sees-zecured")).toBeVisible();
  // The controls that need the script are not offered.
  await expect(page.locator(".rail")).toBeHidden();
  await expect(page.locator(".vk").first()).toBeHidden();
  await expect(page.locator(".seg")).toBeHidden();
  await context.close();
});

test("the steps work by keyboard, and only one shows at a time", async ({ page }) => {
  await open(page);
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
  // Next and previous, with Enter; the focus stays on the button.
  const next = page.locator("#loan-step-1 [data-go='1']");
  await next.focus();
  await page.keyboard.press("Enter");
  await expect(steps.nth(1)).toBeVisible();
  await expect(page.locator("#loan-step-2 [data-go='1']")).toBeFocused();
  await expect(page.locator("#loan-live")).toHaveText(/Step 2 of 5/);
  // At the ends the buttons are disabled.
  await page.keyboard.press("Space");
  await page.keyboard.press("Space");
  await page.keyboard.press("Space");
  await expect(steps.nth(4)).toBeVisible();
  await expect(page.locator("#loan-step-5 [data-go='1']")).toBeDisabled();
  await expect(page.locator("#loan-step-5 [data-go='-1']")).toBeFocused();
  expect(await steps.evaluateAll((all) => all.filter((s) => !(s as HTMLElement).hidden).length)).toBe(1);
});

test("the viewing key clears what the borrower can read, never the market maker's output", async ({ page }) => {
  await open(page);
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
  // One switch for every step.
  await expect(page.locator("#loan-step-1 .vk")).toHaveAttribute("aria-pressed", "true");
  await page.keyboard.press("Space");
  await expect(mine).toHaveAttribute("aria-hidden", "true");
});

test("the comparison switches by keyboard", async ({ page }) => {
  await open(page);
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
  await expect(page.locator("#sees-zecured .leak")).toHaveCount(0);
  await page.keyboard.press("ArrowLeft");
  await expect(page.locator("#sees-bridge .leak").first()).toBeVisible();
});

test("a link to a step opens it and scrolls to it", async ({ page }) => {
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.goto("/#loan-step-3");
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

test("no motion when the reader asks for none", async ({ page }) => {
  await page.emulateMedia({ reducedMotion: "reduce" });
  await open(page);
  const running = await page.evaluate(() => document.getAnimations().length);
  expect(running).toBe(0);
  await expect(page.locator(".hero .seal")).toHaveCSS("opacity", "1");
  await expect(page.locator(".hero .lrow").first()).toHaveCSS("opacity", "1");
});

for (const width of WIDTHS) {
  test(`no horizontal scroll at ${width} px`, async ({ page }) => {
    await open(page, width);
    for (const n of [0, 4]) {
      await page.locator(".rail button").nth(n).click();
      const wide = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
      expect(wide).toBeLessThanOrEqual(0);
    }
  });
}

test("every link inside the page has a target", async ({ page }) => {
  await open(page);
  const missing = await page.evaluate(() =>
    Array.from(document.querySelectorAll<HTMLAnchorElement>('a[href^="#"]'))
      .map((a) => a.getAttribute("href")!.slice(1))
      .filter((id) => id !== "" && !document.getElementById(id)),
  );
  expect(missing).toEqual([]);
});
