"""Find a string literal in the IDB and report every referencing function.

Usage:
  set IDADIR=C:\Program Files\IDA Pro
  python re/scripts/find_string.py <idb> "Unsupported color chart dimensions"

For each hit it prints the string VA/RVA, the code xrefs and, for each referencing
function, whether the function (transitively, one level) calls the console print
entry point. The point is to answer "does this message reach the console through
IConsole::PrintLine or through some other writer?" without guessing.
"""

from __future__ import annotations

import os
import sys

IMAGE_BASE = 0x180000000


def main(argv: list[str]) -> int:
    if len(argv) < 3:
        print(__doc__)
        return 2
    idb = argv[1]
    needle = argv[2]

    os.environ.setdefault("IDADIR", r"C:\Program Files\IDA Pro")
    import idapro  # noqa: PLC0415

    if idapro.open_database(idb, True) != 0:
        print("open_database failed")
        return 1
    import ida_bytes  # noqa: PLC0415
    import ida_funcs  # noqa: PLC0415
    import ida_hexrays  # noqa: PLC0415
    import ida_nalt  # noqa: PLC0415
    import ida_xref  # noqa: PLC0415
    import idc  # noqa: PLC0415

    pattern = needle.encode("ascii")
    hits = []
    ea = 0
    while True:
        ea = ida_bytes.find_bytes(pattern, ea)
        if ea == idc.BADADDR or ea is None:
            break
        hits.append(ea)
        ea += len(pattern)
    print(f"string hits: {len(hits)}")
    for hit in hits[:8]:
        print(f"  va=0x{hit:X} rva=0x{hit - IMAGE_BASE:X}")

    for hit in hits[:8]:
        xref = ida_xref.get_first_dref_to(hit)
        seen = set()
        while xref != idc.BADADDR and xref is not None:
            func = ida_funcs.get_func(xref)
            if func is not None and func.start_ea not in seen:
                seen.add(func.start_ea)
                print(
                    f"\n--- xref from 0x{xref:X} in {idc.get_func_name(func.start_ea)} "
                    f"(rva 0x{func.start_ea - IMAGE_BASE:X}) ---"
                )
                try:
                    text = str(ida_hexrays.decompile(func.start_ea))
                    print(text[:1800] + ("\n... [truncated]" if len(text) > 1800 else ""))
                except Exception as error:  # noqa: BLE001
                    print(f"decompile failed: {error}")
            xref = ida_xref.get_next_dref_to(hit, xref)

    idapro.close_database(False)
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
