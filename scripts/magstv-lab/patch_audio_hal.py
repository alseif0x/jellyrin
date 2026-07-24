#!/usr/bin/env python3
"""
Minimal, verifiable patch for the AOSP generic audio HAL used by the emulator
ARM64 system image (android.hardware.audio@2.0-impl.so).

Under QEMU TCG the function
  StreamOut::getPresentationPositionImpl(audio_stream_out*, uint64_t* frames,
                                         TimeSpec* timestamp) -> int
takes a SIGSEGV on its one-time guarded static init, which kills the HAL
service; audioserver then aborts ("HAL server crashed, need to restart") in a
loop and system_server never finishes booting.

We replace the function prologue with a self-contained stub that returns
success with a zeroed position, touching only its own arguments:

    mov  x8, #0
    str  x8, [x1]        ; *frames = 0
    str  x8, [x2]        ; timestamp->tvSec  = 0
    str  x8, [x2, #8]    ; timestamp->tvNSec = 0
    mov  w0, #0          ; return 0 (Result::OK path in the HIDL wrapper)
    ret

No stack, no LR, no PLT references -> no epilogue needed. 24 bytes overwritten
in a 284-byte function; everything after the stub is dead code.
"""
import sys

SRC = "/home/cdmonio/apk-work/android.hardware.audio@2.0-impl.api27.arm64.so"
DST = "/home/cdmonio/apk-work/android.hardware.audio@2.0-impl.api27.arm64.patched.so"

FUNC_VADDR = 0x2AC14      # symbol value from readelf -s
TEXT_ADDR  = 0x201A8      # .text sh_addr
TEXT_OFF   = 0x1A1A8      # .text sh_offset
FILE_OFF   = FUNC_VADDR - (TEXT_ADDR - TEXT_OFF)   # = 0x24C14

# original prologue we expect to see (sanity guard against patching the wrong build)
ORIG_PROLOGUE = bytes([0xFF, 0x83, 0x01, 0xD1])    # sub sp, sp, #96

STUB = bytes([
    0x08, 0x00, 0x80, 0xD2,   # mov  x8, #0
    0x28, 0x00, 0x00, 0xF9,   # str  x8, [x1]
    0x48, 0x00, 0x00, 0xF9,   # str  x8, [x2]
    0x48, 0x04, 0x00, 0xF9,   # str  x8, [x2, #8]
    0x00, 0x00, 0x80, 0x52,   # mov  w0, #0
    0xC0, 0x03, 0x5F, 0xD6,   # ret
])

def main():
    with open(SRC, "rb") as f:
        data = bytearray(f.read())

    have = bytes(data[FILE_OFF:FILE_OFF + 4])
    if have != ORIG_PROLOGUE:
        print(f"ABORT: prologue mismatch at 0x{FILE_OFF:x}: "
              f"{have.hex()} != {ORIG_PROLOGUE.hex()}", file=sys.stderr)
        return 2

    data[FILE_OFF:FILE_OFF + len(STUB)] = STUB

    with open(DST, "wb") as f:
        f.write(data)

    print(f"patched: {DST}")
    print(f"file offset: 0x{FILE_OFF:x}  bytes written: {len(STUB)}")
    return 0

if __name__ == "__main__":
    raise SystemExit(main())
