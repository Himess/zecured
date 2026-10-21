#!/usr/bin/env bash
# O-3 staging drill: the operator messages through zecured-ops, end to end, against a running
# deployment (the local fork: scripts/local-fork.sh start, then scripts/signer-network.sh init).
#
#   scripts/ops-drill.sh           # dry run: prepare, sign with the local nodes' keys, collect, check
#                                  # against Base and simulate; nothing is sent
#   scripts/ops-drill.sh --send    # local fork only: also sends the pause and then the unpause
#
# Stages: a pause signed by three signers (two are refused first), an unpause, a Swap version
# scheduled by all five (a drill version, never sent), the removal of the deployment's last
# working deposit version (five signatures, R-23; never sent), and, given a bond-paid claim
# (DRILL_REIMBURSE_CLAIM and DRILL_REIMBURSE_COMMITMENT), its Reimburse destination (five, R-6).
# Each stage prints its duration.
#
# The five signers' keys stay in their identity files (signer/data/node-<i>/identity.json):
# zecured-ops reads them itself and never prints them. --send pays the gas with DEPLOYER_PRIVATE_KEY
# (repo-root .env), any funded key: the signatures authorize the calls, not the sender.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DATA="${ZECURED_SIGNER_DATA:-$ROOT/signer/data}"
export ZECURED_NETWORK_CONFIG="${ZECURED_NETWORK_CONFIG:-$DATA/network.json}"
SEND=0
case "${1:-}" in
  "") ;;
  --send) SEND=1 ;;
  *) echo "usage: $0 [--send]" >&2; exit 2 ;;
esac

[[ -f "$ZECURED_NETWORK_CONFIG" ]] || { echo "no network config: $ZECURED_NETWORK_CONFIG" >&2; exit 1; }
for i in 1 2 3 4 5; do
  [[ -f "$DATA/node-$i/identity.json" ]] || { echo "no identity file for node $i under $DATA" >&2; exit 1; }
done

if [[ $SEND == 1 ]]; then
  # Sending changes the deployment's state: only ever on a local fork. The URL is checked, never
  # printed (a provider URL may carry an API key).
  python3 -I -c 'import json, sys
url = json.load(open(sys.argv[1]))["base_rpc_url"]
sys.exit(0 if url.startswith(("http://127.0.0.1", "http://localhost")) else 1)' "$ZECURED_NETWORK_CONFIG" \
    || { echo "--send runs against a local fork only (the network config's Base RPC is not local)" >&2; exit 1; }
  if [[ -f "$ROOT/.env" ]]; then
    set -a
    # shellcheck disable=SC1091
    source "$ROOT/.env"
    set +a
  fi
  : "${DEPLOYER_PRIVATE_KEY:?set in .env: it pays the gas of the drill transactions}"
fi

(cd "$ROOT" && cargo build -q -p zecured-ops)
OPS="$ROOT/target/debug/zecured-ops"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

stage() { echo; echo "== $1"; STAGE_START=$SECONDS; }
done_in() { echo "   ($((SECONDS - STAGE_START)) s)"; }

# sign_with REQUEST NAME NODE... : one signature file per node, NAME.<i>.json.
sign_with() {
  local request=$1 name=$2
  shift 2
  for i in "$@"; do
    "$OPS" sign "$request" --identity "$DATA/node-$i/identity.json" --out "$WORK/$name.$i.json" --yes > /dev/null
  done
}

# submit BUNDLE [send]: simulate, and send too when asked for and allowed.
submit() {
  if [[ ${2:-} == send && $SEND == 1 ]]; then
    "$OPS" submit "$1" --sender-key-env DEPLOYER_PRIVATE_KEY
  else
    "$OPS" submit "$1" --dry-run
  fi
}

stage "PauseMinting, three of five (T-03)"
"$OPS" prepare pause --until +1h --ttl 3600 --out "$WORK/pause.json"
sign_with "$WORK/pause.json" pause 1 2 3
"$OPS" collect "$WORK/pause.json" "$WORK/pause.1.json" "$WORK/pause.2.json" --out "$WORK/pause.two.json" > /dev/null
if "$OPS" submit "$WORK/pause.two.json" --dry-run > /dev/null 2>&1; then
  echo "FAIL: two signatures were accepted" >&2
  exit 1
fi
echo "   two signatures: refused, as they must be"
"$OPS" collect "$WORK/pause.json" "$WORK"/pause.[123].json --out "$WORK/pause.bundle.json" > /dev/null
submit "$WORK/pause.bundle.json" send
done_in

stage "UnpauseMinting, three of five (T-03)"
"$OPS" prepare unpause --ttl 3600 --out "$WORK/unpause.json"
sign_with "$WORK/unpause.json" unpause 3 4 5
"$OPS" collect "$WORK/unpause.json" "$WORK"/unpause.[345].json --out "$WORK/unpause.bundle.json" > /dev/null
submit "$WORK/unpause.bundle.json" send
done_in

stage "ScheduleVersion of a Swap guest, all five (T-07a; a drill version, never sent)"
DRILL_IMAGE="0x$(printf 'zecured-ops drill %s' "$(date +%s)" | sha256sum | cut -c1-64)"
"$OPS" prepare schedule-version --kind swap --selector "${DRILL_SELECTOR:-0x73c457ba}" --image-id "$DRILL_IMAGE" \
  --ttl 86400 --out "$WORK/schedule.json"
sign_with "$WORK/schedule.json" schedule 1 2 3 4 5
"$OPS" collect "$WORK/schedule.json" "$WORK"/schedule.[12345].json --out "$WORK/schedule.bundle.json" > /dev/null
submit "$WORK/schedule.bundle.json"
done_in

stage "RemoveVersion of the last working deposit version, all five (R-23; never sent)"
"$OPS" versions
DEPOSIT_VERSION="$("$OPS" versions --json | python3 -I -c 'import json, sys
working = [v for v in json.load(sys.stdin) if v["kind"] == "deposit" and v["working"]]
print(working[0]["versionId"] if working else "")')"
if [[ -z "$DEPOSIT_VERSION" ]]; then
  echo "   skipped: no working deposit version"
else
  "$OPS" prepare remove-version --version-id "$DEPOSIT_VERSION" --ttl 3600 --out "$WORK/remove.json"
  sign_with "$WORK/remove.json" remove 1 2 3 4 5
  "$OPS" collect "$WORK/remove.json" "$WORK"/remove.[123].json --out "$WORK/remove.three.json" > /dev/null
  "$OPS" collect "$WORK/remove.json" "$WORK"/remove.[12345].json --out "$WORK/remove.bundle.json" > /dev/null
  if "$OPS" submit "$WORK/remove.three.json" --dry-run > /dev/null 2>&1; then
    echo "FAIL: three signatures removed the last working deposit version" >&2
    exit 1
  fi
  echo "   three signatures: refused, as they must be"
  submit "$WORK/remove.bundle.json"
fi
done_in

stage "ReimburseDestination of a bond-paid claim, all five (R-6)"
if [[ -n "${DRILL_REIMBURSE_CLAIM:-}" && -n "${DRILL_REIMBURSE_COMMITMENT:-}" ]]; then
  "$OPS" prepare reimburse-destination --claim "$DRILL_REIMBURSE_CLAIM" --commitment "$DRILL_REIMBURSE_COMMITMENT" \
    --ttl 3600 --out "$WORK/reimburse.json"
  sign_with "$WORK/reimburse.json" reimburse 1 2 3 4 5
  "$OPS" collect "$WORK/reimburse.json" "$WORK"/reimburse.[12345].json --out "$WORK/reimburse.bundle.json" > /dev/null
  submit "$WORK/reimburse.bundle.json"
else
  echo "   skipped: set DRILL_REIMBURSE_CLAIM and DRILL_REIMBURSE_COMMITMENT to a claim the bonds paid"
fi
done_in

echo
echo "drill finished$( [[ $SEND == 1 ]] && echo ' (the pause and the unpause were sent)' || echo ' (dry run: nothing sent)')"
