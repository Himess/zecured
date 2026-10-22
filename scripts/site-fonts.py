"""Builds the site's and the app's web fonts from Google Fonts' OFL sources (docs/SITE.md).

    python scripts/site-fonts.py <Archivo[wdth,wght].ttf> <MartianMono[wdth,wght].ttf>

Needs fonttools and brotli (pip install fonttools brotli). The sources are google/fonts'
ofl/archivo/Archivo[wdth,wght].ttf and ofl/martianmono/MartianMono[wdth,wght].ttf. Both stay
variable (width and weight); the subset is Latin and the punctuation the pages use, without
hinting. Writes site/fonts/*.woff2 and copies them into app/app/fonts/, which next/font/local reads.
"""

import shutil
import sys
from pathlib import Path

from fontTools import subset

# Latin, Latin-1, general punctuation (‘ ’ “ ” … and the dashes), arrows, a few math signs. Neither
# source draws ‖, so the pages write || for a concatenation.
UNICODES = (
    "U+0000-00FF,U+0131,U+0152-0153,U+02BB-02BC,U+02C6,U+02DA,U+02DC,U+0304,U+0308,U+0329,"
    "U+03C0,U+2000-206F,U+20AC,U+2122,U+2190-2199,U+2212,U+2215,U+2248,U+2260,U+2264-2265,"
    "U+FEFF,U+FFFD"
)

ROOT = Path(__file__).resolve().parent.parent


def build(source: Path, target: Path) -> None:
    options = subset.Options()
    options.flavor = "woff2"
    options.hinting = False
    options.desubroutinize = True
    options.layout_features = ["*"]
    options.name_IDs = ["*"]
    options.notdef_outline = True
    font = subset.load_font(str(source), options)
    subsetter = subset.Subsetter(options)
    subsetter.populate(unicodes=subset.parse_unicodes(UNICODES))
    subsetter.subset(font)
    target.parent.mkdir(parents=True, exist_ok=True)
    subset.save_font(font, str(target), options)
    print(f"{target.relative_to(ROOT)}: {target.stat().st_size} bytes")


def main() -> None:
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    archivo, martian = Path(sys.argv[1]), Path(sys.argv[2])
    fonts = ROOT / "site" / "fonts"
    outputs = [fonts / "archivo.woff2", fonts / "martian-mono.woff2"]
    build(archivo, outputs[0])
    build(martian, outputs[1])
    app = ROOT / "app" / "app" / "fonts"
    app.mkdir(parents=True, exist_ok=True)
    for out in outputs:
        shutil.copyfile(out, app / out.name)


if __name__ == "__main__":
    main()
