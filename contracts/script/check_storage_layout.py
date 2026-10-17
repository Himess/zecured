#!/usr/bin/env python3
"""The Registry runs ZecuredSpendLogic's code with delegatecall on its own storage (T-11), so both contracts must
lay their storage out identically: same variables, slots, offsets and types, in the same order. Both inherit exactly
ZecuredRegistryStorage; this check catches any drift (a variable declared in only one of them, a reordered struct).
It also prints the shared layout (white-box tests such as test/halmos/PayoutRules.t.sol write storage by slot).

    python3 script/check_storage_layout.py        # from contracts/, after forge build
"""

import json
import subprocess
import sys


def layout(contract):
    out = subprocess.run(
        ["forge", "inspect", contract, "storageLayout", "--json"],
        capture_output=True,
        encoding="utf-8",
        errors="replace",
        check=True,
    ).stdout
    data = json.loads(out)
    types = data["types"] or {}
    return [(s["label"], s["slot"], s["offset"], types[s["type"]]["label"]) for s in data["storage"]]


def main():
    registry, logic = layout("ZecuredRegistry"), layout("ZecuredSpendLogic")
    if registry != logic:
        print("storage layouts differ between ZecuredRegistry and ZecuredSpendLogic:")
        for i in range(max(len(registry), len(logic))):
            a = registry[i] if i < len(registry) else None
            b = logic[i] if i < len(logic) else None
            if a != b:
                print(f"  #{i}: Registry {a} | SpendLogic {b}")
        sys.exit(1)
    for label, slot, offset, kind in registry:
        print(f"  slot {slot:>3} offset {offset:>2}  {label}: {kind}")
    print(f"ZecuredRegistry and ZecuredSpendLogic share one storage layout ({len(registry)} variables)")


if __name__ == "__main__":
    main()
