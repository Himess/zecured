// REPLAY (DEMO): checks the static export in out/ before it is published (pages.yml, ci.yml). Every asset a page or
// payload names exists; every page has its payloads, and each segment payload the alias build-replay.mjs adds;
// every page carries one Content-Security-Policy that keeps its connections same-origin; no page's first scripts
// carry the live account-abstraction stack (it loads on the first UserOp, which a replay never sends); and each
// page's first-load JavaScript stays within its budget.
//   NEXT_PUBLIC_BASE_PATH=/zecured/app node scripts/check-export.mjs     (the base path the export was built with;
//   from Git Bash on Windows, prefix MSYS_NO_PATHCONV=1, or MSYS rewrites the path)
import { existsSync, readFileSync, readdirSync, statSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";
import { gzipSync } from "node:zlib";

const out = fileURLToPath(new URL("../out", import.meta.url));
const base = (process.env.NEXT_PUBLIC_BASE_PATH ?? "").replace(/\/$/, "");
/** First-load JavaScript per page, gzip. */
const BUDGET = 450 * 1024;
/** Strings only the live account-abstraction stack (viem's bundler and paymaster clients) contains. */
const LIVE_ONLY = ["eth_sendUserOperation", "pm_getPaymasterStubData"];

if (!existsSync(out)) {
  console.error("No out/: build the replay first (node scripts/build-replay.mjs).");
  process.exit(1);
}
const problems = [];
const rel = (f) => relative(out, f).replaceAll("\\", "/");

function walk(dir) {
  return readdirSync(dir).flatMap((name) => {
    const path = join(dir, name);
    return statSync(path).isDirectory() ? walk(path) : [path];
  });
}
const files = walk(out);
const pages = files.filter((f) => f.endsWith(".html"));

// 1. Every asset named by a page or a payload exists.
const asset = new RegExp(`${base.replace(/[.*+?^${}()|[\]\\/]/g, "\\$&")}/_next/[^"'\\s)\\\\]+`, "g");
let named = 0;
for (const f of files.filter((f) => f.endsWith(".html") || f.endsWith(".txt"))) {
  for (const [ref] of readFileSync(f, "utf8").matchAll(asset)) {
    named += 1;
    const path = ref.slice(base.length + 1).split(/[?#]/)[0];
    if (!existsSync(join(out, path))) problems.push(`${rel(f)} names ${ref}, which is not in the export`);
  }
}
// A check that found nothing to check fails: a wrong base path would otherwise pass, on 0 KB everywhere.
if (named === 0) problems.push(`nothing names an asset under ${base}/_next/: was out/ built with NEXT_PUBLIC_BASE_PATH=${base || '""'}?`);

// 2. Payloads: each page's index.txt, and each segment payload under the name the router asks for.
// (404/index.html is a copy of 404.html, which has none.)
for (const page of pages.filter((f) => f.endsWith("index.html") && rel(f) !== "404/index.html")) {
  if (!existsSync(join(dirname(page), "index.txt"))) problems.push(`${rel(page)} has no index.txt`);
}
for (const f of files.filter((f) => /__next\.[^/\\]+[/\\]__PAGE__\.txt$/.test(f))) {
  const segment = dirname(f);
  const alias = `${segment}.__PAGE__.txt`;
  if (!existsSync(alias)) problems.push(`${rel(f)} has no alias ${rel(alias)}`);
}

// 3 to 5, per page.
const decode = (s) => s.replaceAll("&#x27;", "'").replaceAll("&quot;", '"').replaceAll("&amp;", "&");
const sizes = [];
for (const page of pages) {
  const text = readFileSync(page, "utf8");
  const csp = [...text.matchAll(/<meta http-equiv="Content-Security-Policy" content="([^"]*)"/g)].map((m) => decode(m[1]));
  if (csp.length !== 1) problems.push(`${rel(page)} has ${csp.length} Content-Security-Policy tags, not 1`);
  else if (!/(^|;)\s*connect-src 'self'\s*(;|$)/.test(csp[0])) problems.push(`${rel(page)}: its policy does not keep connections same-origin`);

  const scripts = [...text.matchAll(/<script[^>]+src="([^"]+)"/g)].map((m) => m[1]).filter((src) => src.startsWith(`${base}/_next/`));
  if (scripts.length === 0) problems.push(`${rel(page)}: no script under ${base}/_next/ (another base path, or a page this check cannot read)`);
  let gzip = 0;
  for (const src of scripts) {
    const path = join(out, src.slice(base.length + 1));
    if (!existsSync(path)) continue; // reported by 1.
    const js = readFileSync(path);
    gzip += gzipSync(js, { level: 9 }).length;
    const code = js.toString("utf8");
    for (const s of LIVE_ONLY) if (code.includes(s)) problems.push(`${rel(page)} loads ${s} up front (${src.split("/").pop()})`);
  }
  sizes.push([rel(page), gzip]);
  if (gzip > BUDGET) problems.push(`${rel(page)}: ${Math.round(gzip / 1024)} KB of JavaScript (gzip) up front, over ${BUDGET / 1024} KB`);
}

for (const [page, gzip] of sizes.sort((a, b) => b[1] - a[1])) console.log(`${String(Math.round(gzip / 1024)).padStart(4)} KB  ${page}`);
if (problems.length) {
  console.error(`\n${problems.length} problem(s) in the export:\n${problems.map((p) => `  - ${p}`).join("\n")}`);
  process.exit(1);
}
console.log(`\nThe export checks out: ${pages.length} pages, ${files.length} files.`);
