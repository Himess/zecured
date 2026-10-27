// A static export writes each page's segment payload as `<page>/__next.<segment>/__PAGE__.txt`, while the router's
// prefetches ask for `<page>/__next.<segment>.__PAGE__.txt` (and fall back to index.txt on a 404). A copy at the
// second name spares a static host those 404s. Used by build-replay.mjs, and by scripts/deploy-vercel.sh for the
// live page's export.
//   node scripts/alias-segments.mjs [dir]   (default: out/)
import { copyFileSync, existsSync, readdirSync, statSync } from "node:fs";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

export function aliasSegments(dir) {
  for (const name of readdirSync(dir)) {
    const path = join(dir, name);
    if (!statSync(path).isDirectory()) continue;
    const page = join(path, "__PAGE__.txt");
    if (name.startsWith("__next.") && existsSync(page)) copyFileSync(page, join(dir, `${name}.__PAGE__.txt`));
    if (name !== "_next") aliasSegments(path);
  }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  aliasSegments(resolve(process.argv[2] ?? "out"));
  console.log("Segment payloads aliased.");
}
