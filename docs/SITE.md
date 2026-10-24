# The site

`site/` is Zecured's public site, two static pages with no server behind them:

- **The landing page** (`index.html`): a short story told by a 3D scene. The scene is Zecured’s mark made solid:
  a disc split by a seam, its dark half Zcash’s shielded pool (notes of ciphertext), its white half Base’s public
  ledger, and a gold core in the seam, the only place anything crosses. As the reader scrolls, the story plays out
  on the mark: a coin paid into a vault its five signers ring, a proof crossing to the ledger, a fresh account
  borrowing from the market, the close, the swap, what a bridge would leak, and the theft and its slashing. The
  text stays short: a line and a sentence a chapter.
- **The docs page** (`docs/index.html`): the details. One loan step by step on both chains (with the borrower’s
  viewing key), what Base sees next to a typical bridge, the swap, who you have to trust and the theft timeline,
  the evidence, the way to mainnet, and how to run it all.

The first design of the site, a single long page, is kept on the branch `site-v1`; the second, whose scene was
an abstract field of light, on `site-v2`.

The site is not a deployment of the system (DECISIONS D82): it holds no keys, calls no backend and reads no chain.
It loads nothing from anyone else: no analytics, no third-party fonts, scripts, images or embeds. Its
Content-Security-Policy (a meta tag, since GitHub Pages sets no headers) allows only its own origin:
`default-src 'none'; style-src 'self'; script-src 'self'; font-src 'self'; img-src 'self'; media-src 'self';
base-uri 'none'; form-action 'none'`, so no inline style or script either.

## Look at it

```bash
python3 -m http.server 8090 --directory site
```

then open http://localhost:8090 and http://localhost:8090/docs/.

## The scene

The scene is written with [three.js](https://threejs.org) 0.186.1 (MIT) in `site/dev/scene/`: `world.js` builds
the mark and its lights, `props.js` the things of the story (notes, coins, signers, ledger rows, the market, the
escrow, the bonds), `labels.js` draws the text on them, `story.js` gives each thing its state, `camera.js` the
camera’s, and `index.js` runs it all. esbuild bundles it with the parts of three.js it uses into one minified
module, `site/scene.js` (`pnpm build` in `site/dev`); a test checks that the committed file is what the source
builds to.

- `main.js` loads `scene.js` once the page is up, and only where WebGL 2 is available and the reader has not asked
  for reduced motion. Otherwise, and until the scene draws its first frame, the page shows `img/still.jpg`, one
  frame of the scene.
- The scroll is the story’s clock. Each section of the page names its chapter (`data-scene`); story time is the
  chapter’s number plus how far through it the reader is, and every object’s state is a function of that time
  alone, so the story plays backwards as well as forwards. Each chapter’s events happen while its text is on
  screen.
- The camera follows a track of keyframes in story time, each an orbit around what is being told and the place on
  screen where it should stand. `main.js` measures where each section’s text ends (its right edge, or its top on a
  tall screen), and the scene stands the story in the space the text leaves, stepping back where that space is
  narrow. Once, as the page opens, the camera swings in toward the mark.
- The small tags on the scene (`.tags [data-tag]`, in the page’s HTML) follow the things they name: the scene
  places them each frame and fades them with its story. They are decorative (`aria-hidden`); the text says the same.
- It keeps its frame rate by lowering the pixel ratio while frames run slow, then turning off the glow, then the
  shadows, and it stops while the tab is hidden.

## The design

- **Ground:** near black with a hint of green (`#040907`), the pool’s teal (`#38a684`), the ledger’s ice
  (`#cfe0f2`, `#eef2f6` for light panels).
- **Gold** (`#f4b728`) marks proofs and the primary button.
- **Red** (`#ff5a4e`, and `#9a1f15` on `#fbe1dc` for a leak in text) marks only what leaks or is compromised.
- **Archivo** (variable: width 62 to 125 %, weight 100 to 900) sets the text, headlines wide and heavy. **Martian
  Mono** is for chain data only: hashes, heights, amounts, commands.
- **Motion:** the scene and the reveals of the landing page answer the scroll. `prefers-reduced-motion` stops all
  of it: no scene, no transitions, the still image.
- **Panels in the docs:** where a step happens on both chains, the shielded pool is a dark rounded panel with
  encrypted values blurred, and Base a light, square, ruled one.
- **Other projects' marks:** the scene names ZEC, USDC and Morpho with their owners' own marks, drawn unaltered
  from their brand kits: the Zcash brandmark from the Zcash brand folder ZecHub keeps
  (https://zechub.wiki/zcash-organizations/brand), the USDC token logo from Circle's press kit
  (https://www.circle.com/en/pressroom) and Morpho's symbol from https://brand.morpho.org. They are their owners'
  trademarks, used only to name the asset or the protocol (`site/dev/scene/labels.js`).

## Its content

Every number on the site comes from a recorded run, and each has its source:

| On the site | Source |
|---|---|
| 804 contract tests, 6 skipped | TESTING § 1 |
| 4 invariant campaigns, 35 invariants, 12,800 calls each, no reverts | TESTING § 1, INVARIANTS |
| Halmos 22 of 22 | TESTING § 1 |
| Real Groth16 seals 12 of 12 (`RealSealsV2.t.sol`) | TESTING § 2 |
| 533 on a Base mainnet fork (the v3 suites) | TESTING § 1, MAINNET § 1 |
| Rust 600 (CI on Linux), app 438 | TESTING § 1 |
| GPU: a deposit in 28.1 s, an F1 in 36.9 s | TESTING § 2, 2026-10-21 |
| Leak scan: 302 identifiers, 77 events, 55 calldata, 177 storage words, 0 matches | TESTING § 2, v4.1 |
| The browser run: 2 min 50 s, 4 min 25 s, 7 min 56 s; 729,131 gas | TESTING § 2 |
| Close from collateral: block 4,488,453; 337,500 and 252,500 zat | TESTING § 2, the app’s run |
| The swap: order `0x21660b10` (0.01 ZEC for 11.115555 USDC) and its times | TESTING § 2, v4 |
| The swap's liquidity: market makers' own inventory, quotes signed for one taker; at most 200 USDC an order on the judge deployment, 250 USDC (5 ZEC) in the mainnet beta | SWAP, "A Buy, step by step", "Parameters" and "The market maker" |
| The theft demonstration: `43ea5e91`, block 4,523,730; 64 s; 00:04:40, 00:06:47, 00:16:53; 3 → 12.65 USDC | TESTING § 2 |
| Vault `0x59d5e63e`, tag `0xe43d283f`, payoutRef `0xcdf694fd`, blocks 4,490,621 and 4,490,655 | TESTING § 2, 2026-10-12 |
| The mainnet parameters and NU7’s date | MAINNET § 1, 2 and 5 |

When a run changes a number, change it on the site too. The docs’ “Real” and “Mock, or run by us” lists say what
the README’s “What is real and what is a mock” says; when one changes, so does the other.

## The two placeholders

- **`[DEMO VIDEO]`** (the landing page’s demo section) becomes the video, self-hosted in `site/media/`: `demo.mp4`
  (H.264, 1080p, CRF about 26, at most 60 MB; GitHub Pages allows 100 MB a file) with `preload="none"` and a
  poster, `demo-poster.jpg`, and English captions, `<track kind="captions" srclang="en" src="media/demo.en.vtt"
  default>`. Next to it, a plain link to the same video on YouTube (a link, not an embed). The policy’s
  `media-src 'self'` and `img-src 'self'` already allow them.
- **`[LIVE RUN]`** (the docs’ evidence) becomes the judge deployment’s run: Basescan links for the Base Sepolia
  transactions, short txids and blocks for the Zcash side. The app’s evidence file (`/evidence`) takes the same run.

## Tests

```bash
cd site/dev
pnpm install
pnpm exec playwright install chromium
pnpm test
```

They run in CI (the `site` job), in Chromium, on both pages. They check that every request goes to the site’s own
origin and nothing fails or breaks the policy; no accessibility violation (axe-core), also with the viewing key
on, the last step and the other comparison shown; every word readable without the script; that the scene draws and
its tags follow it; that without WebGL the page keeps its still image, loads no scene and logs no error; no motion
and no scene under `prefers-reduced-motion`; the docs’ steps, viewing key and comparison by keyboard, and that the
viewing key never clears the market maker’s output; a link to a step (`#loan-step-3`) opening it and scrolling to
it; no horizontal scroll at 390, 768, 1024 and 1440 px; a target for every link inside the site; and that
`site/scene.js` is what its source builds to.

`pnpm shots` writes the site’s images: `site/img/still.jpg`, the social card `site/og.png` (1200 by 630), the
README’s `docs/assets/hero.png` and `docs/assets/theft.png`, and the frames of `docs/assets/loan.gif`, which
`scripts/site-gif.sh` assembles (needs ffmpeg). The scene is frozen on one frame of the hero for them. It also writes
both pages in full at the four widths for review (`site/dev/shots/`, not committed). `docs/assets/seam.svg` (also
`site/img/seam.svg`) is drawn by hand.

## Fonts

`scripts/site-fonts.py` builds `site/fonts/archivo.woff2` and `site/fonts/martian-mono.woff2` (and the app’s
copies, `app/app/fonts/`) from Google Fonts’ OFL sources, `ofl/archivo/Archivo[wdth,wght].ttf` and
`ofl/martianmono/MartianMono[wdth,wght].ttf`: variable, subset to Latin and the punctuation the pages use, without
hinting. The licence is `site/fonts/OFL.txt`.

## Publishing

`.github/workflows/pages.yml` publishes everything in `site/` except its tooling, `site/dev`, to GitHub Pages, at
https://himess.github.io/zecured/; the pages’ canonical and social-card URLs name that address. The tests’ server
(`site/dev/serve.mjs`) serves the same files, so a file the site needs and the tests find is a file Pages
publishes. The workflow runs only by hand (`workflow_dispatch`). Pages on a private repository needs a paid plan, so
the first publication waits for the repository to become public. Turning Pages on and running the workflow are the
project owner’s decision at the time. A custom domain (a `CNAME`) is optional, and so is serving the site as an
onion service too (`infra/onion`).
