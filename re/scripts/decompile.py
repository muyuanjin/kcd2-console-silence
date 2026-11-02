"""Decompile one or more functions from the KCD2 IDB by VA or RVA.

Usage:
  set IDADIR=C:\Program Files\IDA Pro
  python re/scripts/decompile.py <idb> <va-or-rva> [<va-or-rva> ...]

Values below 0x100000000 are treated as RVAs off the 0x180000000 image base.
Output is bounded per function so a wide dump stays readable.
"""

from __future__ import annotations

import os
import sys

IMAGE_BASE = 0x180000000
LIMIT = 4000


def main(argv: list[str]) -> int:
    if len(argv) < 3:
        print(__doc__)
        return 2
    idb = argv[1]
    targets = []
    for raw in argv[2:]:
        value = int(raw, 16)
        targets.append(value if value >= 0x100000000 else IMAGE_BASE + value)

    os.environ.setdefault("IDADIR", r"C:\Program Files\IDA Pro")
    import idapro  # noqa: PLC0415

    if idapro.open_database(idb, True) != 0:
        print("open_database failed")
        return 1
    import ida_funcs  # noqa: PLC0415
    import ida_hexrays  # noqa: PLC0415
    import idc  # noqa: PLC0415

    for address in targets:
        func = ida_funcs.get_func(address)
        if func is None:
            print(f"\n=== 0x{address:X}: not a function ===")
            continue
        name = idc.get_func_name(address)
        print(
            f"\n=== 0x{address:X} (rva 0x{address - IMAGE_BASE:X}) {name} "
            f"size=0x{func.end_ea - func.start_ea:X} ==="
        )
        try:
            text = str(ida_hexrays.decompile(address))
        except Exception as error:  # noqa: BLE001
            print(f"decompile failed: {error}")
            continue
        print(text[:LIMIT] + ("\n... [truncated]" if len(text) > LIMIT else ""))

    idapro.close_database(False)
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
