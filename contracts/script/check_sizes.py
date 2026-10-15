#!/usr/bin/env python3
"""Fails when a contract under src/ comes within MARGIN bytes of the EIP-170 runtime limit (24,576 bytes) or of the
EIP-3860 initcode limit (49,152 bytes). Prints every contract's sizes either way.

Usage, from contracts/: python3 script/check_sizes.py [margin in bytes]   (default: $SIZE_MARGIN, else 0)
"""

import json
import os
import re
import subprocess
import sys
from pathlib import Path


def main() -> int:
    margin = int(sys.argv[1] if len(sys.argv) > 1 else os.environ.get("SIZE_MARGIN", "0"))
    out = subprocess.run(
        ["forge", "build", "--sizes", "--json", "--skip", "test", "--skip", "script"],
        check=True,
        capture_output=True,
        encoding="utf-8",
        errors="replace",
    ).stdout
    sizes = json.loads(out)

    # Every concrete contract declared under src/ (interfaces, libraries and abstract contracts have no bytecode).
    names = sorted(
        {
            m.group(1)
            for path in Path("src").rglob("*.sol")
            for m in re.finditer(r"(?m)^contract (\w+)", path.read_text(encoding="utf-8"))
        }
    )

    status = 0
    print(f"{'contract':<26} {'runtime':>10} {'margin':>10} {'initcode':>10} {'margin':>10}")
    for name in names:
        entry = sizes.get(name)
        if entry is None:
            print(f"check_sizes: no size reported for {name}", file=sys.stderr)
            status = 1
            continue
        print(
            f"{name:<26} {entry['runtime_size']:>10} {entry['runtime_margin']:>10} "
            f"{entry['init_size']:>10} {entry['init_margin']:>10}"
        )
        if entry["runtime_margin"] < margin or entry["init_margin"] < margin:
            print(f"check_sizes: {name} is within {margin} bytes of a size limit", file=sys.stderr)
            status = 1
    return status


if __name__ == "__main__":
    sys.exit(main())
