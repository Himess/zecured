#!/usr/bin/env bash
# Renders the systemd units in infra/systemd/ as a second, independent stack (the mainnet beta, docs/MAINNET.md § 5):
# units named zecured-<stack>-*, configuration in /etc/zecured-<stack>/, state in /var/lib/zecured-<stack>/, and the
# five nodes on 127.0.0.1:<node port prefix>1..5. Everything else a stack needs (its backend, bundler and bot ports, its
# network.json, keys and endpoints) lives in that stack's environment files. See infra/README.md, "A second stack".
#
#   sudo infra/stack.sh mainnet 920     # writes /etc/systemd/system/zecured-mainnet-*.{service,timer}
set -euo pipefail

stack=${1:?usage: stack.sh <stack name> <node port prefix, e.g. 920>}
prefix=${2:?usage: stack.sh <stack name> <node port prefix, e.g. 920>}
[[ $stack =~ ^[a-z][a-z0-9]{1,15}$ ]] || { echo "stack name: lowercase letters and digits" >&2; exit 1; }
[[ $prefix =~ ^[1-9][0-9]{2}$ ]] && (( prefix >= 103 && prefix != 910 )) ||
  { echo "node port prefix: three digits from 103 (ports 1031 and up), not the public stack's 910" >&2; exit 1; }

src=$(cd "$(dirname "$0")/systemd" && pwd)
out=${UNIT_DIR:-/etc/systemd/system}
for unit in "$src"/zecured-*.service "$src"/zecured-*.timer; do
  name=$(basename "$unit")
  [[ $name == zecured-app-onion.service ]] && continue  # one onion service per host, in front of the public stack
  target="$out/zecured-$stack-${name#zecured-}"
  sed -e "s|/etc/zecured/|/etc/zecured-$stack/|g" \
      -e "s|/var/lib/zecured\([/ ]\)|/var/lib/zecured-$stack\1|g" \
      -e "s|/var/lib/zecured$|/var/lib/zecured-$stack|" \
      -e "s|127\.0\.0\.1:910%i|127.0.0.1:$prefix%i|g" \
      -e "s|zecured-\([a-z-]*@\{0,1\}[0-9]*\)\.service|zecured-$stack-\1.service|g" \
      -e "s|zecured-watchdog\.service|zecured-$stack-watchdog.service|g" \
      -e "/^Description=/ s|Zecured |Zecured ($stack) |" \
      "$unit" > "$target"
  echo "wrote $target"
done
