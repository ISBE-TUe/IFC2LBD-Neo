#!/usr/bin/env python3
"""
Post-process wasm-bindgen output for wasm64 targets.

On wasm64, wasm-bindgen 0.2.126 generates JS where:
  - __wbindgen_add_to_stack_pointer returns f64 but some callers pass
    it to functions expecting i64 (BigInt).  These need BigInt().
  - __wbindgen_export (malloc/realloc) returns f64, which is correct for
    f64 ABI params.  Do NOT wrap these in BigInt.
  - getDataView operations with retptr (now BigInt) need Number().

The wasm64 ABI uses f64 for pointer params in most functions, but i64
for retptr (stack pointer manipulation).  Only wrap retptr in BigInt.
"""
import sys
from pathlib import Path

def patch_file(path: str):
    p = Path(path)
    s = p.read_text()
    original = s

    # 1. add_to_stack_pointer(-16) returns f64, but convertIfcToSink etc.
    #    expect i64 (first param = retptr).  Wrap in BigInt().
    s = s.replace(
        "wasm.__wbindgen_add_to_stack_pointer(-16)",
        "BigInt(wasm.__wbindgen_add_to_stack_pointer(-16))",
    )

    # 2. getDataView operations with retptr (now BigInt) → use Number()
    s = s.replace("getDataViewMemory0().getInt32(retptr + 4 * 0, true)",
                  "getDataViewMemory0().getInt32(Number(retptr) + 4 * 0, true)")
    s = s.replace("getDataViewMemory0().getInt32(retptr + 4 * 1, true)",
                  "getDataViewMemory0().getInt32(Number(retptr) + 4 * 1, true)")
    s = s.replace("getDataViewMemory0().getInt32(retptr + 4 * 2, true)",
                  "getDataViewMemory0().getInt32(Number(retptr) + 4 * 2, true)")
    s = s.replace("getDataViewMemory0().getInt32(retptr + 4 * 3, true)",
                  "getDataViewMemory0().getInt32(Number(retptr) + 4 * 3, true)")
    s = s.replace("getDataViewMemory0().getInt64(retptr + 8 * 1, true)",
                  "getDataViewMemory0().getInt64(Number(retptr) + 8 * 1, true)")
    s = s.replace("getDataViewMemory0().getInt64(retptr + 8 * 0, true)",
                  "getDataViewMemory0().getInt64(Number(retptr) + 8 * 0, true)")
    s = s.replace("getDataViewMemory0().setInt32(retptr + 4 * 1,",
                  "getDataViewMemory0().setInt32(Number(retptr) + 4 * 1,")
    s = s.replace("getDataViewMemory0().setInt32(retptr + 4 * 0,",
                  "getDataViewMemory0().setInt32(Number(retptr) + 4 * 0,")
    s = s.replace("getDataViewMemory0().setFloat64(retptr + 8 * 1,",
                  "getDataViewMemory0().setFloat64(Number(retptr) + 8 * 1,")
    s = s.replace("getDataViewMemory0().setFloat64(retptr + 8 * 0,",
                  "getDataViewMemory0().setFloat64(Number(retptr) + 8 * 0,")
    # Also handle generic patterns
    s = s.replace("getDataViewMemory0().setFloat64(arg0 + 8 * 1,",
                  "getDataViewMemory0().setFloat64(Number(arg0) + 8 * 1,")
    s = s.replace("getDataViewMemory0().setFloat64(arg0 + 8 * 0,",
                  "getDataViewMemory0().setFloat64(Number(arg0) + 8 * 0,")
    s = s.replace("getDataViewMemory0().setInt32(arg0 + 4 * 1,",
                  "getDataViewMemory0().setInt32(Number(arg0) + 4 * 1,")
    s = s.replace("getDataViewMemory0().setInt32(arg0 + 4 * 0,",
                  "getDataViewMemory0().setInt32(Number(arg0) + 4 * 0,")
    s = s.replace("getDataViewMemory0().setBigInt64(arg0 + 8 * 1,",
                  "getDataViewMemory0().setBigInt64(Number(arg0) + 8 * 1,")
    s = s.replace("getDataViewMemory0().setBigInt64(arg0 + 8 * 0,",
                  "getDataViewMemory0().setBigInt64(Number(arg0) + 8 * 0,")
    s = s.replace("getDataViewMemory0().setBigInt64(retptr + 8 * 1,",
                  "getDataViewMemory0().setBigInt64(Number(retptr) + 8 * 1,")
    s = s.replace("getDataViewMemory0().setBigInt64(retptr + 8 * 0,",
                  "getDataViewMemory0().setBigInt64(Number(retptr) + 8 * 0,")

    if s != original:
        p.write_text(s)
        print(f"  Patched: {path}")
        return True
    else:
        print(f"  No changes: {path}")
        return False

if __name__ == "__main__":
    for f in sys.argv[1:]:
        patch_file(f)
