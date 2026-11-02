"""Dump a vtable from the KCD2 IDB and decompile selected slots.

Usage:
  python re/scripts/vtable_probe.py <idb> <vtable-rva-hex> <slot> [<slot> ...]

Example (IConsole / CXConsole):
  set IDADIR=C:\Program Files\IDA Pro
  python re\scripts\vtable_probe.py WHGame_V1.5.6.dll.i64 0x3DCF850 0 26 27 21 22

The first slot dump also prints the RTTI class name derived from the
complete-object-locator pointer stored immediately before the vtable, so the
table can be attributed to a class instead of being taken on faith.
"""

from __future__ import annotations

import os
import struct
import sys

IMAGE_BASE = 0x180000000


def main(argv: list[str]) -> int:
    if len(argv) < 4:
        print(__doc__)
        return 2
    idb = argv[1]
    vtable_rva = int(argv[2], 16)
    slots = [int(value, 0) for value in argv[3:]]

    os.environ.setdefault("IDADIR", r"C:\Program Files\IDA Pro")
    import idapro  # noqa: PLC0415  (heavy, imported after the env is set)

    print(f"opening {idb} ...")
    if idapro.open_database(idb, True) != 0:
        print("open_database failed")
        return 1

    import ida_bytes  # noqa: PLC0415
    import ida_funcs  # noqa: PLC0415
    import ida_hexrays  # noqa: PLC0415
    import idc  # noqa: PLC0415

    def class_name(table_rva: int) -> str:
        """Read the COL -> TypeDescriptor -> mangled name for a vtable."""
        col_ptr = ida_bytes.get_qword(IMAGE_BASE + table_rva - 8)
        col_rva = col_ptr - IMAGE_BASE
        if not (0 < col_rva < 0x8000000):
            return "<no COL>"
        descriptor_rva = ida_bytes.get_dword(IMAGE_BASE + col_rva + 0x0C)
        if not (0 < descriptor_rva < 0x8000000):
            return "<no descriptor>"
        raw = ida_bytes.get_bytes(IMAGE_BASE + descriptor_rva + 0x10, 128) or b""
        name = raw.split(b"\x00", 1)[0]
        return name.decode("ascii", "replace")

    print(f"vtable 0x{vtable_rva:X} class={class_name(vtable_rva)}")

    for slot in slots:
        entry = ida_bytes.get_qword(IMAGE_BASE + vtable_rva + slot * 8)
        entry_rva = entry - IMAGE_BASE
        func = ida_funcs.get_func(entry)
        name = idc.get_func_name(entry) if func else "<not a function>"
        size = (func.end_ea - func.start_ea) if func else 0
        print(f"\n=== slot {slot}: va=0x{entry:X} rva=0x{entry_rva:X} {name} size=0x{size:X} ===")
        if not func:
            continue
        try:
            code = ida_hexrays.decompile(entry)
        except Exception as error:  # noqa: BLE001 - report, never abort the dump
            print(f"decompile failed: {error}")
            continue
        text = str(code)
        print(text[:2500] if len(text) > 2500 else text)

    idapro.close_database(False)
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
