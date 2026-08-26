#!/usr/bin/env python3
# Minimal solid-color PNG encoder (stdlib only: zlib+struct) so the Tauri
# app has real icon files without pulling in an image-processing dependency
# just to make a placeholder square.
import struct
import sys
import zlib


def write_png(path, size, rgba):
    def chunk(tag, data):
        return (
            struct.pack(">I", len(data))
            + tag
            + data
            + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)
        )

    row = bytes(rgba) * size
    raw = b"".join(b"\x00" + row for _ in range(size))
    sig = b"\x89PNG\r\n\x1a\n"
    ihdr = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)
    with open(path, "wb") as f:
        f.write(sig)
        f.write(chunk(b"IHDR", ihdr))
        f.write(chunk(b"IDAT", zlib.compress(raw, 9)))
        f.write(chunk(b"IEND", b""))


if __name__ == "__main__":
    out_dir = sys.argv[1]
    # LocalSync brand-ish blue square, fully opaque.
    color = (0x2B, 0x6C, 0xB0, 0xFF)
    for name, size in [
        ("32x32.png", 32),
        ("128x128.png", 128),
        ("128x128@2x.png", 256),
        ("icon.png", 512),
    ]:
        write_png(f"{out_dir}/{name}", size, color)
    print("icons written to", out_dir)
