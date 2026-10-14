#!/usr/bin/env python3
"""Zecured watchdog (review G2, G3): checks that every service is up, the header relay keeps up with
Zcash, and every operational balance stays above its floor, and sends an alarm when one stops
holding (and again when it recovers).

Standard library only; run every few minutes by infra/systemd/zecured-watchdog.timer. All settings
come from the environment (infra/watchdog/watchdog.env.example), so thresholds, endpoints and the
alarm channel change without a code change (G1).

Alarms never carry a user address, viewing key, salt or IP: they name a check, our own operational
accounts and numbers, nothing about any vault.
"""

import json
import os
import sys
import time
import urllib.error
import urllib.request

TIMEOUT = 15


def env(name, default=None):
    value = os.environ.get(name, "").strip()
    return value if value else default


# Some public RPC endpoints refuse Python's default user agent.
USER_AGENT = "zecured-watchdog/1"


def http_json(url, body=None):
    data = None if body is None else json.dumps(body).encode()
    headers = {"user-agent": USER_AGENT}
    if body is not None:
        headers["content-type"] = "application/json"
    req = urllib.request.Request(url, data=data, headers=headers)
    with urllib.request.urlopen(req, timeout=TIMEOUT) as r:
        return json.loads(r.read())


def rpc(method, params):
    out = http_json(env("BASE_RPC_URL", "https://sepolia.base.org"),
                    {"jsonrpc": "2.0", "id": 1, "method": method, "params": params})
    if "error" in out:
        raise RuntimeError(f"{method}: {out['error'].get('message', 'error')}")
    return out["result"]


def call(to, data):
    return rpc("eth_call", [{"to": to, "data": data}, "latest"])


def word(address):
    return address.lower().removeprefix("0x").rjust(64, "0")


def eth_balance(address):
    return int(rpc("eth_getBalance", [address, "latest"]), 16) / 1e18


def named_list(value):
    """"name:0xabc,other:0xdef" -> [("name", "0xabc"), ("other", "0xdef")]."""
    out = []
    for part in (value or "").split(","):
        if ":" in part:
            name, address = part.split(":", 1)
            out.append((name.strip(), address.strip()))
    return out


def checks():
    """Yields (check id, ok, message) for every configured check."""
    backend = env("BACKEND_URL", "http://127.0.0.1:8080")
    status = None
    try:
        status = http_json(f"{backend}/status")
        yield "backend", True, "backend answers"
    except Exception as e:  # noqa: BLE001 - any failure is the alarm
        yield "backend", False, f"backend /status fails: {type(e).__name__}"

    if status is not None:
        for node in status.get("nodes", []):
            mode = node.get("mode")
            index = node.get("index")
            # Every node matters: opening a vault needs all five (DKG), payouts need three.
            yield f"node-{index}", mode == "normal", f"signer node {index}: {mode}"

    relay = env("RELAY")
    if relay and status is not None and status.get("zcashTip"):
        try:
            tip = call(relay, "0xab725954")  # bestChainTip()
            relay_height = int(tip[2 + 64:2 + 128], 16)
            lag = int(status["zcashTip"]) - relay_height
            limit = int(env("MAX_RELAY_LAG_BLOCKS", "40"))
            yield "relay-lag", lag <= limit, f"header relay {lag} blocks behind Zcash (alarm above {limit})"
        except Exception as e:  # noqa: BLE001
            yield "relay-lag", False, f"header relay unreadable: {type(e).__name__}"

    floor = float(env("MIN_ETH", "0.01"))
    for name, address in named_list(env("WATCH_ETH")):
        try:
            limit = float(env(f"MIN_ETH_{name.upper()}", str(floor)))
            balance = eth_balance(address)
            yield f"eth-{name}", balance >= limit, f"{name} has {balance:.4f} ETH (floor {limit})"
        except Exception as e:  # noqa: BLE001
            yield f"eth-{name}", False, f"{name} balance unreadable: {type(e).__name__}"

    entry_point, paymaster = env("ENTRYPOINT"), env("PAYMASTER")
    if entry_point and paymaster:
        try:
            deposit = int(call(entry_point, "0x70a08231" + word(paymaster)), 16) / 1e18  # balanceOf
            limit = float(env("MIN_PAYMASTER_DEPOSIT_ETH", "0.02"))
            yield "paymaster", deposit >= limit, f"paymaster deposit {deposit:.4f} ETH (floor {limit})"
        except Exception as e:  # noqa: BLE001
            yield "paymaster", False, f"paymaster deposit unreadable: {type(e).__name__}"

    usdc = env("USDC")
    for name, address in named_list(env("WATCH_USDC")):
        try:
            units = int(call(usdc, "0x70a08231" + word(address)), 16) / 1e6
            limit = float(env(f"MIN_USDC_{name.upper()}", "100"))
            yield f"usdc-{name}", units >= limit, f"{name} has {units:.2f} USDC (floor {limit})"
        except Exception as e:  # noqa: BLE001
            yield f"usdc-{name}", False, f"{name} USDC unreadable: {type(e).__name__}"

    bond = env("SIGNER_BOND")
    if bond:
        try:
            bonded = int(call(bond, "0xcd6563e2"), 16) == 1  # fullyBonded()
            yield "bonds", bonded, "all five signer bonds in place" if bonded else "a signer bond is missing: no new vault can be activated"
        except Exception as e:  # noqa: BLE001
            yield "bonds", False, f"signer bonds unreadable: {type(e).__name__}"


def send(text):
    url = env("ALERT_WEBHOOK_URL")
    prefix = env("ALERT_PREFIX", "zecured")
    line = f"[{prefix}] {text}"
    if not url:
        print(line)
        return
    fmt = env("ALERT_FORMAT", "slack")
    if fmt == "ntfy":
        req = urllib.request.Request(url, data=line.encode(), headers={"Title": prefix, "user-agent": USER_AGENT})
    else:
        key = "content" if fmt == "discord" else "text"
        req = urllib.request.Request(url, data=json.dumps({key: line}).encode(),
                                     headers={"content-type": "application/json", "user-agent": USER_AGENT})
    try:
        urllib.request.urlopen(req, timeout=TIMEOUT).read()
    except urllib.error.URLError as e:
        print(f"alarm not delivered ({type(e).__name__}): {line}", file=sys.stderr)


def main():
    state_path = env("WATCHDOG_STATE", "/var/lib/zecured/watchdog.json")
    try:
        with open(state_path) as f:
            state = json.load(f)
    except (OSError, ValueError):
        state = {}
    repeat = float(env("REPEAT_HOURS", "6")) * 3600
    now = time.time()
    failing = 0
    for check, ok, message in checks():
        was = state.get(check)
        if ok:
            if was and was.get("failing"):
                send(f"RECOVERED {message}")
            state[check] = {"failing": False}
        else:
            failing += 1
            if not (was and was.get("failing")) or now - was.get("alerted", 0) >= repeat:
                send(f"ALARM {message}")
                state[check] = {"failing": True, "alerted": now}
        print(("ok    " if ok else "FAIL  ") + message)
    os.makedirs(os.path.dirname(state_path) or ".", exist_ok=True)
    with open(state_path, "w") as f:
        json.dump(state, f)
    return 1 if failing else 0


if __name__ == "__main__":
    sys.exit(main())
