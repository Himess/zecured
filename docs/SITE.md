# The site

`site/` is Zecured's public site, two static pages with no server behind them:

- **The landing page** (`index.html`): a short story told by a 3D scene. The scene is Zecured’s mark made solid:
  a disc split by a seam, its dark half Zcash’s shielded pool (notes of ciphertext), its white half Base’s public
  ledger, and a gold core in the seam, the only place anything crosses. As the reader scrolls, the story plays out
  on the mark: a coin paid into a vault its five signers ring, a proof crossing to the ledger, a fresh account
  borrowing from the market, the close, the swap, what a bridge would leak, and the theft and its slashing. The
  text stays short: a line and a sentence a chapter. The page ends on the evidence in numbers, the app, the doors
  into the docs and a closing line with the three ways in (the app, the demo, the code).
- **The docs page** (`docs/index.html`): the details. One loan step by step on both chains (with the borrower’s
  viewing key), what Base sees next to a typical bridge, the swap, who you have to trust and the theft timeline,
  the evidence, the way to mainnet, and how to run it all.
- **The 404 page** (`404.html`): GitHub Pages serves it at any missing path, so its links and stylesheet are
  absolute: `{{BASE}}` stands for the site’s base path, which the publishing workflow writes in (below).

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
  for reduced motion. Otherwise, and until the scene draws its first frame, the page shows a frame of the scene as
  an image: `img/still.jpg`, or `img/still-tall.jpg` on a tall screen (the mark above, the text below). Without
  the scene the chapters are plain sections over that image, and the Prove chapter shows `img/seam.svg`.
- The scroll is the story’s clock. Each section of the page names its chapter (`data-scene`); story time is the
  chapter’s number plus how far through it the reader is, and every object’s state is a function of that time
  alone, so the story plays backwards as well as forwards. Each chapter’s events happen while its text is on
  screen; the swap and the theft, which tell the most, have longer sections.
- The camera follows a track of keyframes in story time, each an orbit around what is being told and the place on
  screen where it should stand. `main.js` measures where each section’s text ends (its right edge, or its top on a
  tall screen), and the scene stands the story in the space the text leaves. Each moment also names the box of
  what it tells (`camera.js`), and the camera centres that box in the free space and steps back until all of it
  is on screen, clear of the header and of the text. The veil darkens the scene up to where the text’s lines end
  on a wide screen (`--text-edge`), and from where the text begins on a tall one. Once, as the page opens, the
  camera swings in a little toward the mark. In the demo section the mark sinks out of the app’s picture.
- A proof is light, not metal: a pale, glowing tile with a π, and a comet’s tail behind it, so it never reads as
  the gold ZEC coins. The signers’ bonds are USDC, blue like the chips they repay a victim with.
- The small tags on the scene (`.tags [data-tag]`, in the page’s HTML) follow the things they name: the scene
  places them each frame, fades them with its story and never lays one over a chapter’s text. They are decorative
  (`aria-hidden`); the text says the same.
- It draws into a multisampled target of its own (the canvas itself has no antialiasing), keeps its frame rate by
  lowering the pixel ratio while frames run slow, then turning off the glow, then the shadows, stops while the tab
  is hidden, and gives the page back its still image if the GPU loses the context.

## The design

- **Ground:** near black with a hint of green (`#040907`), the pool’s teal (`#38a684`, `#5ed3a8` for small marks),
  the ledger’s ice (`#cfe0f2`, `#eef2f6` for light panels).
- **Gold** (`#f4b728`) marks proofs and the primary button, nothing else: chapter numbers, arrows and the scroll
  cue are neutral, and a button’s glow is short.
- **Red** (`#ff5a4e`, and `#9a1f15` on `#fbe1dc` for a leak in text) marks only what leaks or is compromised.
- **Archivo** (variable: width 62 to 125 %, weight 100 to 900) sets the text, headlines wide and heavy. **Martian
  Mono** is for chain data only: hashes, heights, amounts, commands. Neither draws ↗ (external links get a small
  drawn arrow) and Martian Mono has no π (π is set in Archivo).
- **Grid:** the hero’s and the chapters’ text starts where the header’s logo does, at every width; a short laptop
  window (up to 860 px high) gets a tighter first screen so its buttons stay in view.
- **Motion:** the scene and the reveals of the landing page answer the scroll. `prefers-reduced-motion` stops all
  of it: no scene, no transitions, the still image.
- **Panels:** where a step happens on both chains, the shielded pool is a dark rounded panel with encrypted values
  blurred, and Base a light, square, ruled one. The landing’s comparison of a bridge and Zecured is two such
  ledger panels: both columns are what Base shows.
- **Other projects' marks:** the scene names ZEC, USDC and Morpho with their owners' own marks, drawn unaltered
  from their brand kits: the Zcash brandmark from the Zcash brand folder ZecHub keeps
  (https://zechub.wiki/zcash-organizations/brand), the USDC token logo from Circle's press kit
  (https://www.circle.com/en/pressroom) and Morpho's symbol from https://brand.morpho.org. They are their owners'
  trademarks, used only to name the asset or the protocol (`site/dev/scene/labels.js`); the Morpho market leaves
  the scene with the close, so no other project’s mark decorates the page’s closing frames.

## Its content

Every number on the site comes from a recorded run, and each has its source:

| On the site | Source |
|---|---|
| 804 contract tests, 6 skipped | TESTING § 1 |
| 4 invariant campaigns, 35 invariants, 12,800 calls each, no reverts | TESTING § 1, INVARIANTS |
| Halmos 22 of 22, for every input of a single call | TESTING § 1 |
| 4 of 4 kinds of real proof (deposit, F1, inclusion, and a payment on a synthetic fixture) pass RISC Zero’s router, in 12 tests (`RealSealsV2.t.sol`) | TESTING § 2b, “Statement v2's seals on-chain” |
| RISC Zero’s live router accepts the real deposit’s seal by `eth_call` (the README’s one-command check) | TESTING § 2b, “The live router, by anyone” |
| 533 on a Base mainnet fork (the v3 suites) | TESTING § 1, MAINNET § 1 |
| Rust 600 (CI on Linux), app 451 | TESTING § 1 |
| GPU: a deposit in 28.1 s, an F1 in 36.9 s (the STARK); about a minute more for the Groth16 wrap (61.5 s on a 15-vCPU x86 machine) | TESTING § 2b, 2026-10-21 and 2026-10-14 |
| Leak scan: 302 identifiers (txids, note commitments, nullifiers, raw receivers, viewing and tag keys), 77 events, 55 calldata, 177 storage words, 0 matches | TESTING § 2 (P-6) and its leak scanner |
| The browser run: 2 min 50 s, 4 min 25 s, 7 min 56 s (MOCK verifier on that fork); 729,131 gas | TESTING § 2 |
| The opening fee: 0.5 %, waived below 20,000 zatoshi | ECONOMICS, “Fees” |
| Close from collateral: block 4,488,453; 337,500 and 252,500 zat | TESTING § 2, the app’s run |
| The swap: order `0x21660b10` (0.01 ZEC for 11.115555 USDC) and its times | TESTING § 2, v4 |
| The swap's liquidity: market makers' own inventory, quotes signed for one taker; orders capped at 200 USDC on testnet, 250 USDC in the mainnet beta | SWAP, “A Buy, step by step”, “Parameters” and “The market maker” |
| The theft demonstration: `43ea5e91`, block 4,523,730; 64 s; 00:04:40, 00:06:47, 00:16:53; 3 → 12.65 USDC at $1,265 | TESTING § 2 |
| All five bonds slashed because FROST hides which three signed | TIER3, F1 |
| Vault `0x59d5e63e`, tag `0xe43d283f`, payoutRef `0xcdf694fd`, blocks 4,490,621 and 4,490,655 | TESTING § 2, 2026-10-12 |
| One signer offline: opening a vault refused within a second | TESTING § 2, “One signer down at opening” |
| Two compromised signers sign a sweep, the other three refuse | TESTING § 2, “Threshold demo” |
| A claim unpaid 24 hours (72 on mainnet) after a signer acknowledged it is paid from the bonds | ECONOMICS, MAINNET § 2 |
| The market’s liquidation LTV, 62.5 % | MAINNET § 1, ECONOMICS |
| A borrow of 4 USDC | TESTING § 2, “Browser, gasless” |
| 0.125 ZEC as a standard size | THREAT_MODEL (L3) |
| One Equihash ASIC out-mines Zcash testnet about 40 times over | TIER3, TIER2, JURY_FAQ |
| The mainnet parameters and NU7’s date | MAINNET § 1, 2 and 5 |
| Lenders on Morpho Blue supply the USDC and earn the interest, in a new market backed by Zcash collateral | SUBMISSION, “What it does” |
| A liquidation pays the liquidator in shielded ZEC with no outgoing viewing key; pre-liquidation is opt-in | README (Borrow), ARCHITECTURE (P3), ECONOMICS (“Pre-liquidation (opt-in)”), TESTING § 2 (`dc7c669a`) |
| The relay and the proofs form a Zcash light client other contracts can use | SUBMISSION, SWAP (its first paragraph) |
| The draft ZIP, Shielded Collateral Vaults | `docs/zip-draft-shielded-collateral-vaults.md` |
| A judge’s questions | JURY_FAQ |
| Known limitations and their designs | MAINNET § 7 |
| Built from 10 October 2026 | the repository’s first commit |
| The app’s replay: a guided replay of the live run of 27 October in the browser (DEMO) | `app/README.md` (“The judges' replay”) |

When a run changes a number, change it on the site too. The docs’ “Real” and “Mock, or run by us” lists say what
the README’s “What is real and what is a mock” says; when one changes, so does the other.

## The video

The landing page’s demo section plays the video of the live run of 27 October and links it on YouTube
(https://youtu.be/iyXiao54KWk); before the video existed it showed a picture of the app (`img/app.webp`, 16:10, at most
150 KB) that opens it. The video lives in `site/media/`:
`demo.mp4` (H.264, 1080p, CRF about 26, `+faststart` so that it plays before it has downloaded, at most about 30 MB:
the repository keeps every version), a poster frame of the app, `demo-poster.jpg` (at most 150 KB), and English
captions, `demo.en.vtt` (the site's file stays without burned-in captions, or drop `default` from the track). Then
replace the whole `<section class="demo" ...>` with this, keeping the one sentence that says what the video was
recorded from (the replay's sample data, or a live run on a given date), and in the hero change the second button to
“Watch the 2-minute demo”. Keep the YouTube link a plain link, never an embed: the policy allows no frames.

```html
<section class="demo demo-video" id="demo" data-scene="demo" data-veil="wide" aria-labelledby="demo-h">
  <div class="demo-copy">
    <h2 id="demo-h" class="title reveal">See it run.</h2>
    <p class="body reveal">Two minutes, recorded in the app’s replay (DEMO, sample data): a loan opened in the browser, its deposit proven on Base, USDC borrowed from a brand-new account, a close from collateral, and a swap into shielded ZEC.</p>
    <!-- a live run: "Two minutes, recorded in our live run of <date> on Zcash testnet and Base Sepolia: ..." -->
    <p class="note reveal">Or click through it yourself: the app runs as a guided replay in your browser, with sample data (DEMO).</p>
    <p class="ctas reveal"><a class="btn btn-gold" href="app/">Replay it in the app</a><a class="btn btn-line" href="https://www.youtube.com/watch?v=VIDEO_ID">Watch on YouTube<svg class="ext" width="12" height="12" viewBox="0 0 12 12" aria-hidden="true" focusable="false"><path d="M3.5 2.5h6v6M9.5 2.5 2.5 9.5" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"/></svg></a></p>
  </div>
  <figure class="frame reveal">
    <video controls preload="none" playsinline width="1920" height="1080" poster="media/demo-poster.jpg" aria-describedby="demo-cap">
      <source src="media/demo.mp4" type="video/mp4">
      <track kind="captions" srclang="en" label="English" src="media/demo.en.vtt" default>
    </video>
    <figcaption id="demo-cap" class="frame-cap">2:00 · narrated · English captions</figcaption>
  </figure>
</section>
```

And in `landing.css`, after `.frame video { ... }`, so that the video takes the section's width and the app's words
in it stay legible (at the side of the copy it would get about 660 px, a third of 1080p):

```css
.demo-video {
  grid-template-columns: minmax(0, 1fr);
}

.demo-video .demo-copy {
  max-width: 44rem;
}

.frame-cap {
  padding: 10px 16px 12px;
  border-top: 1px solid var(--hair);
  color: var(--muted);
  font-size: 13.5px;
}
```

Encode with `ffmpeg -i master.mov -vf scale=1920:-2 -c:v libx264 -preset slow -crf 26 -profile:v high -pix_fmt yuv420p
-movflags +faststart -c:a aac -b:a 128k site/media/demo.mp4`, and the poster with `ffmpeg -ss 2 -i site/media/demo.mp4
-frames:v 1 -q:v 4 site/media/demo-poster.jpg`. The policy’s `media-src 'self'` and `img-src 'self'` already allow
them, and the track meets axe's video-caption rule. If the narration does not say everything the video shows, add a
transcript next to it. If no video is published, take its promises out of the README, SUBMISSION and JURY_FAQ.

The docs’ evidence lists the recorded runs of TESTING § 2. A run on Base Sepolia would be added there once TESTING
records it, with Basescan links for the Base Sepolia transactions and short txids and blocks for the Zcash side.

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
it; no horizontal scroll at 390, 768, 1024, 1440 and 1920 px; a target for every link inside the site; that a
missing page gets the 404 page, styled, with its links; and that `site/scene.js` is what its source builds to. On
a machine short of memory, run them one at a time: `pnpm test -- --workers=1`.

`pnpm shots` writes the site’s images: `site/img/still.jpg` and `site/img/still-tall.jpg` (read from the canvas
itself), the social card `site/og.jpg` (1200 by 630), the README’s `docs/assets/hero.png` and
`docs/assets/theft.png`, and the frames of `docs/assets/loan.gif`, which `scripts/site-gif.sh` assembles (needs
ffmpeg). The scene is frozen on one frame of the hero for them. It also writes both pages in full at those five
widths for review (`site/dev/shots/`, not committed). `docs/assets/seam.svg` (also `site/img/seam.svg`) is drawn by
hand.

## Fonts

`scripts/site-fonts.py` builds `site/fonts/archivo.woff2` and `site/fonts/martian-mono.woff2` (and the app’s
copies, `app/app/fonts/`) from Google Fonts’ OFL sources, `ofl/archivo/Archivo[wdth,wght].ttf` and
`ofl/martianmono/MartianMono[wdth,wght].ttf`: variable, subset to Latin and the punctuation the pages use, without
hinting. The licence is `site/fonts/OFL.txt`.

## Publishing

The site is at https://zecured.vercel.app/, and the same files are on GitHub Pages at https://himess.github.io/zecured/.
No custom domain (decided 2026-10-27).

`.github/workflows/pages.yml` publishes everything in `site/` except its tooling, `site/dev`, to GitHub Pages; the
pages’ canonical and social-card URLs name the github.io address, and the workflow rewrites it, and the 404 page’s
`{{BASE}}`, from what `actions/configure-pages` reports. Beside them, at `app/`, the same workflow builds and
publishes the judges' replay of the app (DEMO, `app/README.md`): its static export, which replays the live run of
27 October in the browser. The site's “Try the app” buttons link there. The tests’ server (`site/dev/serve.mjs`)
serves the same files the same way, so a file the site needs and the tests find is a file Pages publishes. The
workflow runs only by hand (`workflow_dispatch`): the site has been published from `main` since 2026-10-26.

`scripts/deploy-vercel.sh` builds the same files for the root of the vercel.app address (the replay at `/app/`, the
canonical URLs naming that address) and deploys them with the Vercel CLI, from the project owner’s account; it has
served them since 2026-10-27. Vercel also sends the headers Pages cannot: `frame-ancestors 'none'` (which a meta tag
cannot carry), `nosniff`, `no-referrer`, and a year’s cache for the replay’s hashed assets. Both hosts serve static
files only, and nothing of ours runs there (D82). Every change to `site/` or to the app's replay needs a run of the
workflow and of the script, each the project owner’s decision at the time. Serving the site as an onion service too
is optional (`infra/onion`).
