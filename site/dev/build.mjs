// Bundles the landing page's scene (scene/index.js and the parts of three.js it uses) into site/scene.js, one
// minified module the page loads from its own origin. `pnpm build`; the test suite checks the committed file is
// what this builds.
import { resolve } from "node:path";
import { build } from "esbuild";

export const sceneBuild = {
  entryPoints: [resolve(import.meta.dirname, "scene", "index.js")],
  bundle: true,
  format: "esm",
  minify: true,
  target: "es2020",
  legalComments: "eof",
  charset: "utf8",
};

if (process.argv[1] && resolve(process.argv[1]) === resolve(import.meta.filename)) {
  await build({ ...sceneBuild, outfile: resolve(import.meta.dirname, "..", "scene.js"), logLevel: "info" });
}
