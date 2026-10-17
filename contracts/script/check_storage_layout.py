#!/usr/bin/env python3
"""Fails unless ZecuredRegistry and ZecuredSpendLogic have exactly the same storage layout.

The Registry runs the spend logic's code by delegatecall on its own storage (T-11), so every slot must mean the same in
both: each inherits ZecuredRegistryStorage and must declare no storage of its own. This compares the two layouts that
`forge inspect` reports, item by item (label, slot, offset and type), and prints them either way.

Usage, from contracts/: python3 script/check_storage_layout.py
"""

import json
import subprocess
import sys

CONTRACTS = ("src/ZecuredRegistry.sol:ZecuredRegistry", "src/registry/ZecuredSpendLogic.sol:ZecuredSpendLogic")


def layout(contract: str) -> list[tuple[str, str, int, str]]:
    out = subprocess.run(
        ["forge", "inspect", contract, "storageLayout", "--json"],
        check=True,
        capture_output=True,
        encoding="utf-8",
        errors="replace",
    ).stdout
    items = json.loads(out)["storage"]
    return [(i["label"], i["slot"], i["offset"], i["type"]) for i in items]


def main() -> int:
    registry, logic = (layout(c) for c in CONTRACTS)
    print(f"{'slot':>5} {'offset':>6}  label (type)")
    for label, slot, offset, kind in registry:
        print(f"{slot:>5} {offset:>6}  {label} ({kind})")
    if registry != logic:
        print("check_storage_layout: ZecuredRegistry and ZecuredSpendLogic differ:", file=sys.stderr)
        for a, b in zip(registry, logic):
            if a != b:
                print(f"  registry {a}\n  logic    {b}", file=sys.stderr)
        if len(registry) != len(logic):
            print(f"  {len(registry)} items against {len(logic)}", file=sys.stderr)
        return 1
    print(f"check_storage_layout: the {len(registry)} storage items are identical")
    return 0


if __name__ == "__main__":
    sys.exit(main())
