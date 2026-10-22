# The showcase page

`site/` is Zecured's showcase page: `index.html`, `styles.css`, `main.js`, `fonts/`, `favicon.svg` and `og.png`.
It has no build step and loads nothing from anyone else: no analytics, no third-party fonts, scripts, images or
embeds. Its Content-Security-Policy (a meta tag, since GitHub Pages sets no headers) allows only the page's own
origin: `default-src 'none'; style-src 'self'; script-src 'self'; font-src 'self'; img-src 'self'; media-src 'self';
base-uri 'none'; form-action 'none'`, so no inline style or script either.

The page is not a deployment of the system (DECISIONS D82): it holds no keys, calls no backend and reads no chain.
It can be opened at any time, without anything else running.

## Look at it

```bash
python3 -m http.server 8090 --directory site
```

then open http://localhost:8090. Without JavaScript the page is complete: the five steps of "Follow one loan" and
both comparisons show one after the other, and the controls that need the script are not offered.

## The design

The page is the seam between two chains.

- **The seam.** Zcash is on the left: dark, soft corners, encrypted values blurred, no rules. Base is on the right:
  light, sharp corners, ruled ledger rows. On a desktop the seam is vertical; at 760 px and below it is horizontal.
- **Gold** `#F4B728` marks only the π seal (a proof crossing) and the primary button.
- **Red** marks only what leaks: text `#9A1F15` on `#FBE1DC`, underline `#B42318`.
- **Martian Mono** is only for chain data: hashes, heights, amounts, commands. Never for labels.
- **Not used:** uppercase labels, meta text joined with " · ", link texts ending in "→", grids of identical
  shadowed cards.
- **Motion.** The hero has one moment on load: the seal leaves the pool, crosses the seam, the stamp ring opens and
  the ledger rows appear. Everything else moves only in answer to the reader (the steps, the viewing key, the
  comparison). `prefers-reduced-motion` stops all of it.

| Token | Value | Use |
|---|---|---|
| iron | `#0D1611` | Zcash's ground |
| iron-2 | `#142019` | a soft surface on the dark |
| iron-line | `#24352B` | a thin division on the dark |
| iron-edge | `#3E5146` | a button's edge on the dark |
| pool-text | `#E4ECE6` | text on the dark (15.3:1) |
| pool-soft | `#C9D6CC` | body text on the dark (12.3:1) |
| pool-muted | `#93A89A` | secondary text on the dark (7.3:1) |
| ledger | `#EEF2F6` | Base's ground |
| ledger-line | `#CFD8E2` | a ledger row's rule |
| ink | `#0F1B2A` | text on the light (15.4:1) |
| ink-soft | `#33455A` | body text on the light |
| ink-muted | `#4C5D70` | secondary text on the light (6.0:1) |
| seal | `#F4B728` | proofs and the primary button; its text is ink (9.6:1) |
| bad | `#F26B5E` | a compromised signer's dot (6.2:1 on the dark) |

Archivo (variable: width 62 to 125 %, weight 100 to 900) sets the text: body 17/1.55 (16 at 760 px and below),
lead 18 to 21 px, h3 24 px at 118 % width, h2 `clamp(34px, 4vw, 58px)` at 125 % and weight 800, h1
`clamp(40px, 6.4vw, 100px)` at 125 %, weight 800, line height 0.94. Martian Mono sets chain data at 12.5 to 13 px.

## Its content

Every number on the page comes from a recorded run, and each has its source:

| On the page | Source |
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
| Close from collateral: block 4,488,453; 337,500 and 252,500 zat | TESTING § 2, the app's run |
| The swap: order `0x21660b10` and its times | TESTING § 2, v4 |
| The theft demonstration: `43ea5e91`, block 4,523,730; 64 s; 00:04:40, 00:06:47, 00:16:53; 3 → 12.65 USDC | TESTING § 2 |
| Vault `0x59d5e63e`, tag `0xe43d283f`, payoutRef `0xcdf694fd`, blocks 4,490,621 and 4,490,655 | TESTING § 2, 2026-10-12 |
| The mainnet parameters and NU7's date | MAINNET § 1, 2 and 5 |

When a run changes a number, change it on the page too. The page's "Real" and "Mock, or run by us" lists say what
the README's "What is real and what is a mock" says; when one changes, so does the other.

## The two placeholders

- **`[DEMO VIDEO]`** becomes the video, self-hosted in `site/media/`: `demo.mp4` (H.264, 1080p, CRF about 26, at
  most 60 MB; GitHub Pages allows 100 MB a file) with `preload="none"` and a poster, `demo-poster.jpg`, and English
  captions, `<track kind="captions" srclang="en" src="media/demo.en.vtt" default>`. Next to it, a plain link to the
  same video on YouTube (a link, not an embed). The policy's `media-src 'self'` and `img-src 'self'` already allow
  them.
- **`[LIVE RUN]`** becomes the judge deployment's run: Basescan links for the Base Sepolia transactions, short
  txids and blocks for the Zcash side. The app's evidence file (`/evidence`) takes the same run.

## Tests

```bash
cd site/tests
pnpm install
pnpm exec playwright install chromium
pnpm test
```

They run in CI (the `site` job). They check that every request goes to the page's own origin and nothing fails or
breaks the policy; no accessibility violation (axe-core), also with the viewing key on, the last step and the other
comparison shown; everything readable without the script; the steps, the viewing key and the comparison by keyboard
(arrows, Home and End on the step rail); that the viewing key never clears the market maker's output; a link to a
step (`#loan-step-3`) opening it; no animation under `prefers-reduced-motion`; no horizontal scroll at 390, 768,
1024 and 1440 px; and a target for every link inside the page.

`pnpm shots` writes the README's images (`docs/assets/hero.png`, `docs/assets/theft.png`), the social card
(`site/og.png`, 1200 by 630) and the frames of `docs/assets/loan.gif`, which `scripts/site-gif.sh` assembles (needs
ffmpeg), plus full pages at the four widths for review (`site/tests/shots/`, not committed).
`docs/assets/seam.svg` is drawn by hand.

## Fonts

`scripts/site-fonts.py` builds `site/fonts/archivo.woff2` and `site/fonts/martian-mono.woff2` (and the app's copies,
`app/app/fonts/`) from Google Fonts' OFL sources, `ofl/archivo/Archivo[wdth,wght].ttf` and
`ofl/martianmono/MartianMono[wdth,wght].ttf`: variable, subset to Latin and the punctuation the pages use, without
hinting. The licence is `site/fonts/OFL.txt`.

## Publishing

`.github/workflows/pages.yml` publishes everything in `site/` except `tests/` to GitHub Pages, at
https://himess.github.io/zecured/; the page's canonical and social-card URLs name that address. The tests' server
(`site/tests/serve.mjs`) serves the same files, so a file the page needs and the tests find is a file Pages
publishes. The workflow runs only by hand (`workflow_dispatch`). Pages on a private repository needs a paid plan, so the first publication waits for
the repository to become public. Turning Pages on and running the workflow are the project owner's decision at
the time. A custom domain (a `CNAME`) is optional, and so is serving the page as an onion service too
(`infra/onion`).
