#!/usr/bin/env python3
"""Zecured watchdog (review G2, G3, v4 O-7): checks that every service is up, the header relay keeps
up with Zcash, every operational balance stays above its floor (Base Sepolia and Base mainnet ETH,
the loan token, the demo wallet's TAZ, the market maker's inventory) and the coverage cap keeps
room (R-11.3), and sends an alarm when one stops holding (and again when it recovers).

Standard library only; run every few minutes by infra/systemd/zecured-watchdog.timer. All settings
come from the environment (infra/watchdog/watchdog.env.example), so thresholds, endpoints and the
alarm channel change without a code change (G1).

Alarms never carry an address, a key, a viewing key, a salt or an IP: they name a check, our own
operational accounts by name and numbers, nothing about any vault. Endpoint URLs (which may carry
an API key) are never printed either: a failure names only its kind.
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


def rpc(method, params, url=None):
    out = http_json(url or env("BASE_RPC_URL", "https://sepolia.base.org"),
                    {"jsonrpc": "2.0", "id": 1, "method": method, "params": params})
    if "error" in out:
        raise RuntimeError(f"{method}: {out['error'].get('message', 'error')}")
    return out["result"]


def call(to, data, url=None):
    return rpc("eth_call", [{"to": to, "data": data}, "latest"], url)


def word(address):
    return address.lower().removeprefix("0x").rjust(64, "0")


def eth_balance(address, url=None):
    return int(rpc("eth_getBalance", [address, "latest"], url), 16) / 1e18


def eth_checks(prefix, chain, accounts, default_floor, url=None):
    """ETH floors of `accounts` ("name:0xabc,..."), per name `MIN_ETH[_MAINNET]_<NAME>`."""
    floor_var = "MIN_ETH" if prefix == "eth" else "MIN_ETH_MAINNET"
    floor = float(env(floor_var, default_floor))
    for name, address in named_list(accounts):
        check = f"{prefix}-{name}"
        try:
            limit = float(env(f"{floor_var}_{name.upper()}", str(floor)))
            balance = eth_balance(address, url)
            yield check, balance >= limit, f"{name}{chain} has {balance:.4f} ETH (floor {limit})"
        except Exception as e:  # noqa: BLE001 - any failure is the alarm
            yield check, False, f"{name}{chain} balance unreadable: {type(e).__name__}"


def usdc_checks(prefix, chain, token, accounts, url=None):
    """Loan-token (6 decimals) floors of `accounts`, per name `MIN_USDC[_MAINNET]_<NAME>`."""
    floor_var = "MIN_USDC" if prefix == "usdc" else "MIN_USDC_MAINNET"
    for name, address in named_list(accounts):
        check = f"{prefix}-{name}"
        try:
            units = int(call(token, "0x70a08231" + word(address), url), 16) / 1e6  # balanceOf
            limit = float(env(f"{floor_var}_{name.upper()}", "100"))
            yield check, units >= limit, f"{name}{chain} has {units:.2f} USDC (floor {limit})"
        except Exception as e:  # noqa: BLE001
            yield check, False, f"{name}{chain} USDC unreadable: {type(e).__name__}"


def json_field(value, path):
    """`path` ("a.b") of a JSON object, as a number (JSON numbers and decimal strings)."""
    for key in path.split("."):
        value = value[key]
    return float(value)


def json_checks(entries):
    """O-7: numeric fields of status endpoints that must stay above a floor, for balances only a
    service knows (the market maker's inventory): "name|url|field|floor,..." (`field` may be a
    dotted path). The URL is never printed."""
    for entry in (entries or "").split(","):
        parts = [p.strip() for p in entry.split("|")]
        if len(parts) != 4:
            continue
        name, url, field, floor = parts
        check = f"json-{name}"
        try:
            value = json_field(http_json(url), field)
            limit = float(floor)
            yield check, value >= limit, f"{name}: {field} is {number(value)} (floor {number(limit)})"
        except Exception as e:  # noqa: BLE001
            yield check, False, f"{name}: {field} unreadable: {type(e).__name__}"


def number(value):
    """A whole number without an exponent (zatoshi), anything else as it is."""
    return f"{value:.0f}" if value == int(value) else f"{value}"


def cap_fill(registry, intake, limit):
    """R-11.3: how full the coverage cap is: locked and pending zatoshi (the Registry's
    `lockedZatoshi` and the Intake's `pendingTotal` and, since v4, `pendingRefundTotal`) over those
    plus the room the Intake still admits (`capRoom`)."""
    locked = int(call(registry, "0x4271109c"), 16)  # lockedZatoshi()
    pending = int(call(intake, "0x36a1b5a0"), 16)  # pendingTotal()
    try:
        pending += int(call(intake, "0x46236eb9"), 16)  # pendingRefundTotal() (v4)
    except Exception:  # noqa: BLE001 - an Intake from before v4
        pass
    room = int(call(intake, "0xdcbc2d60"), 16)  # capRoom()
    used = locked + pending
    fill = used / (used + room) if used + room else 0.0
    return (
        fill < limit,
        f"coverage cap {fill:.0%} full: {used / 1e8:.4f} ZEC locked or pending, {min(room, 10**18) / 1e8:.4f} ZEC "
        f"of room left (alarm at {limit:.0%}; new vaults wait once it is full)",
    )


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

    yield from eth_checks("eth", "", env("WATCH_ETH"), "0.01")

    # O-7: the mainnet beta's own accounts on Base mainnet, when this watchdog watches both stacks.
    mainnet = env("MAINNET_RPC_URL")
    if mainnet:
        yield from eth_checks("mainnet-eth", " (Base mainnet)", env("WATCH_ETH_MAINNET"), "0.005", mainnet)
        if env("MAINNET_USDC"):
            yield from usdc_checks("mainnet-usdc", " (Base mainnet)", env("MAINNET_USDC"),
                                   env("WATCH_USDC_MAINNET"), mainnet)

    entry_point, paymaster = env("ENTRYPOINT"), env("PAYMASTER")
    if entry_point and paymaster:
        try:
            deposit = int(call(entry_point, "0x70a08231" + word(paymaster)), 16) / 1e18  # balanceOf
            limit = float(env("MIN_PAYMASTER_DEPOSIT_ETH", "0.02"))
            yield "paymaster", deposit >= limit, f"paymaster deposit {deposit:.4f} ETH (floor {limit})"
        except Exception as e:  # noqa: BLE001
            yield "paymaster", False, f"paymaster deposit unreadable: {type(e).__name__}"

    if env("USDC"):
        yield from usdc_checks("usdc", "", env("USDC"), env("WATCH_USDC"))

    # O-7: the demo wallet that pays judges' vaults in TAZ (the backend's /demo/status says when it
    # runs low; it never shows the wallet's address or balance).
    if env("WATCH_DEMO_TAZ") == "1":
        try:
            demo = http_json(env("DEMO_STATUS_URL", f"{backend}/demo/status"))
            if not demo.get("enabled"):
                yield "demo-taz", False, "the demo path is off"
            elif demo.get("lowBalance"):
                yield "demo-taz", False, "the demo wallet runs low on TAZ: top it up"
            else:
                yield "demo-taz", True, "the demo wallet has TAZ"
        except Exception as e:  # noqa: BLE001
            yield "demo-taz", False, f"demo wallet status unreadable: {type(e).__name__}"

    # O-7: balances only a service knows (the market maker's ZEC inventory), by status field.
    yield from json_checks(env("WATCH_JSON"))

    # R-11.3: the coverage cap fills up; new vaults wait (or their owners ask for a refund) once full.
    registry, intake = env("REGISTRY"), env("INTAKE")
    if registry and intake:
        try:
            ok, message = cap_fill(registry, intake, float(env("CAP_FILL_ALARM", "0.70")))
            yield "cap-fill", ok, message
        except Exception as e:  # noqa: BLE001
            yield "cap-fill", False, f"coverage cap unreadable (a stale cap price?): {type(e).__name__}"

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
