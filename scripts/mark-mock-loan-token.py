#!/usr/bin/env python3
"""Names a deployment's loan token as the MockUSDC it is (D109), before the backend reads the deployment file.

A Base Sepolia deployment takes its loan token as LOAN_TOKEN: a MockUSDC deployed beforehand, since DeployZecured
mints one only on a local fork. The script then records it as `LoanToken` alone, and the app, which names the token
from `contracts.MockUSDC` (lib/api.ts), would call it "USDC" and list it as real. This adds `contracts.MockUSDC`,
`loanTokenIsMock` and the mock's note, checking first that the token is the market's and that it is a MockUSDC owned by
the deployer (its `symbol()` and `owner()`, read with `cast` when RPC is given).

    scripts/mark-mock-loan-token.py contracts/deployments/base-sepolia.json [--rpc <url>]
"""
import argparse
import json
import subprocess
import sys

NOTE = "MockUSDC is a MOCK (owner-minted; deployed before the script and passed as LOAN_TOKEN, D109)"


def cast(*args: str) -> str:
    return subprocess.run(["cast", *args], check=True, capture_output=True, text=True).stdout.strip()


def main() -> int:
    p = argparse.ArgumentParser()
    p.add_argument("deployment")
    p.add_argument("--rpc", help="check the token's symbol and owner on this RPC first")
    a = p.parse_args()
    with open(a.deployment) as f:
        d = json.load(f)
    c = d["contracts"]
    token = c["LoanToken"]
    if token.lower() != d["market"]["loanToken"].lower():
        sys.exit("the deployment's LoanToken is not its market's loan token")
    if a.rpc:
        symbol = cast("call", token, "symbol()(string)", "--rpc-url", a.rpc).strip('"')
        owner = cast("call", token, "owner()(address)", "--rpc-url", a.rpc)
        if symbol != "mUSDC":
            sys.exit(f"the loan token's symbol is {symbol!r}, not MockUSDC's mUSDC")
        if owner.lower() != d["deployer"].lower():
            sys.exit("the loan token's owner is not the deployer")
    c["MockUSDC"] = token
    d["loanTokenIsMock"] = True
    mocks = d.get("mocks", "")
    if "MockUSDC is a MOCK" not in mocks:
        d["mocks"] = f"{mocks}; {NOTE}" if mocks else NOTE
    with open(a.deployment, "w") as f:
        json.dump(d, f, indent=2)
        f.write("\n")
    print(f"{a.deployment}: contracts.MockUSDC set to the loan token")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
