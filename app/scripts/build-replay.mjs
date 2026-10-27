// REPLAY (DEMO): builds the judges' static export of the app (lib/replay), or serves it with
// `--dev`. Cross-platform: sets the replay's variables and runs Next.js with this Node, in the app's
// own directory wherever it is started from; any other argument goes to Next.js (`--port 3001`).
//   node scripts/build-replay.mjs                       -> out/ (basePath from NEXT_PUBLIC_BASE_PATH, default none)
//   NEXT_PUBLIC_BASE_PATH=/zecured/app node scripts/build-replay.mjs
//   node scripts/build-replay.mjs --dev                 -> next dev, replay mode
import { spawnSync } from "node:child_process";
import { createRequire } from "node:module";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { aliasSegments } from "./alias-segments.mjs";

const require = createRequire(import.meta.url);
const next = require.resolve("next/dist/bin/next");
const app = fileURLToPath(new URL("..", import.meta.url));
const dev = process.argv.includes("--dev");
const rest = process.argv.slice(2).filter((a) => a !== "--dev");
const env = { ...process.env, NEXT_PUBLIC_REPLAY: "1", NEXT_PUBLIC_BASE_PATH: process.env.NEXT_PUBLIC_BASE_PATH ?? "" };
const result = spawnSync(process.execPath, [next, dev ? "dev" : "build", ...rest], { stdio: "inherit", env, cwd: app });
if (dev || result.status !== 0) process.exit(result.status ?? 1);

aliasSegments(join(app, "out"));
console.log("Replay exported to out/ (segment payloads aliased).");
