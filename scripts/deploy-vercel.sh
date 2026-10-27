#!/usr/bin/env bash
# Publishes the site and the app's replay (DEMO) to Vercel, at the root of a vercel.app address: the same files
# .github/workflows/pages.yml publishes to GitHub Pages, built for an empty base path. Static files only: nothing of
# ours runs there (D82), and no custom domain. Needs a Vercel CLI logged in to the project owner's account.
# Run from the repository root, in Git Bash or any POSIX shell:
#   bash scripts/deploy-vercel.sh DIR               builds into DIR (a new folder), then deploys it to production
#   bash scripts/deploy-vercel.sh --build-only DIR  builds into DIR, deploys nothing
# ZECURED_SITE_URL is the address the canonical and social-card links name (default https://zecured.vercel.app).
set -euo pipefail

build_only=0
if [ "${1:-}" = "--build-only" ]; then build_only=1; shift; fi
out="${1:?give a new folder to build into}"
url="${ZECURED_SITE_URL:-https://zecured.vercel.app}"
root="$(cd "$(dirname "$0")/.." && pwd)"
if [ -e "$out" ]; then echo "$out exists; give a new folder" >&2; exit 1; fi

# The site's files without its tooling (pages.yml's first step, with an empty base path).
mkdir -p "$out"
tar -C "$root/site" --exclude=./dev -cf - . | tar -C "$out" -xf -
for f in index.html docs/index.html 404.html; do sed -i "s#https://himess.github.io/zecured#${url}#g" "$out/$f"; done
sed -i "s#{{BASE}}##g" "$out/404.html"
printf 'User-agent: *\nAllow: /\nDisallow: /app/\nSitemap: %s/sitemap.xml\n' "$url" > "$out/robots.txt"
printf '<?xml version="1.0" encoding="UTF-8"?>\n<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9"><url><loc>%s/</loc></url><url><loc>%s/docs/</loc></url></urlset>\n' "$url" "$url" > "$out/sitemap.xml"

# The app's replay (DEMO), a static export served at /app/. (Git Bash would pass /app on as a Windows path.)
(cd "$root/app" && export MSYS2_ENV_CONV_EXCL=NEXT_PUBLIC_BASE_PATH NEXT_PUBLIC_BASE_PATH=/app &&
  node scripts/build-replay.mjs && node scripts/check-export.mjs)
cp -R "$root/app/out" "$out/app"

# Headers, which GitHub Pages cannot send. Every page carries its own CSP in a meta tag; a header adds what a meta tag
# cannot (frame-ancestors). The replay's hashed assets never change.
cat > "$out/vercel.json" <<'EOF'
{
  "trailingSlash": true,
  "headers": [
    {
      "source": "/(.*)",
      "headers": [
        { "key": "Content-Security-Policy", "value": "frame-ancestors 'none'" },
        { "key": "X-Frame-Options", "value": "DENY" },
        { "key": "X-Content-Type-Options", "value": "nosniff" },
        { "key": "Referrer-Policy", "value": "no-referrer" }
      ]
    },
    {
      "source": "/app/_next/static/(.*)",
      "headers": [{ "key": "Cache-Control", "value": "public, max-age=31536000, immutable" }]
    }
  ]
}
EOF
if [ "$build_only" = 1 ]; then echo "Built into $out"; exit 0; fi

# The project is "zecured", created on the first run; --prod puts this deployment at its production address.
vercel project add zecured >/dev/null 2>&1 || true
vercel link --yes --project zecured --cwd "$out" >/dev/null
vercel deploy --prod --yes --cwd "$out"
