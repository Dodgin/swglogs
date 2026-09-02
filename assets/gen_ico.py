#!/usr/bin/env python3
"""Build assets/swglogs.ico from assets/swglogs-icon-1024.png (std-lib only).

Windows wants an .ico with several sizes: small ones as classic 32-bit DIBs
(16/24/32/48/64) and the big ones PNG-compressed (128/256). Re-run after
changing the source PNG; build.rs embeds the result into swglogs.exe.
"""
import struct
import sys
import zlib
from pathlib import Path

HERE = Path(__file__).resolve().parent
SRC = HERE / "swglogs-icon-1024.png"
OUT = HERE / "swglogs.ico"
DIB_SIZES = (16, 24, 32, 48, 64)
PNG_SIZES = (128, 256)


def read_png(path):
    data = path.read_bytes()
    assert data[:8] == b"\x89PNG\r\n\x1a\n", "not a PNG"
    pos, idat, w = 8, [], None
    while pos < len(data):
        n, typ = struct.unpack(">I4s", data[pos:pos + 8])
        body = data[pos + 8:pos + 8 + n]
        pos += 12 + n
        if typ == b"IHDR":
            w, h, depth, ctype, _, _, interlace = struct.unpack(">IIBBBBB", body)
            assert depth == 8 and ctype in (2, 6) and interlace == 0, \
                f"unsupported PNG (depth {depth}, color type {ctype}, interlace {interlace})"
            bpp = 4 if ctype == 6 else 3
        elif typ == b"IDAT":
            idat.append(body)
    raw = zlib.decompress(b"".join(idat))
    stride = w * bpp
    px = bytearray(w * h * 4)
    prev = bytearray(stride)
    p = 0
    for y in range(h):
        f = raw[p]
        line = bytearray(raw[p + 1:p + 1 + stride])
        p += 1 + stride
        for i in range(stride):
            a = line[i - bpp] if i >= bpp else 0
            b = prev[i]
            c = prev[i - bpp] if i >= bpp else 0
            if f == 1:
                line[i] = (line[i] + a) & 255
            elif f == 2:
                line[i] = (line[i] + b) & 255
            elif f == 3:
                line[i] = (line[i] + ((a + b) >> 1)) & 255
            elif f == 4:
                pa, pb, pc = abs(b - c), abs(a - c), abs(a + b - 2 * c)
                pr = a if pa <= pb and pa <= pc else (b if pb <= pc else c)
                line[i] = (line[i] + pr) & 255
        o = y * w * 4
        if bpp == 4:
            px[o:o + w * 4] = line
        else:
            for x in range(w):
                px[o + x * 4:o + x * 4 + 3] = line[x * 3:x * 3 + 3]
                px[o + x * 4 + 3] = 255
        prev = line
    return w, h, px


def downscale(w, h, px, size):
    """Area-average RGBA (alpha-weighted colour) to size x size."""
    out = bytearray(size * size * 4)
    for ty in range(size):
        y0, y1 = ty * h // size, max((ty + 1) * h // size, ty * h // size + 1)
        for tx in range(size):
            x0, x1 = tx * w // size, max((tx + 1) * w // size, tx * w // size + 1)
            r = g = b = a = 0
            n = 0
            for y in range(y0, y1):
                row = y * w * 4
                for x in range(x0, x1):
                    i = row + x * 4
                    al = px[i + 3]
                    r += px[i] * al
                    g += px[i + 1] * al
                    b += px[i + 2] * al
                    a += al
                    n += 1
            o = (ty * size + tx) * 4
            if a:
                out[o] = r // a
                out[o + 1] = g // a
                out[o + 2] = b // a
            out[o + 3] = a // n
    return out


def encode_png(size, px):
    def chunk(typ, body):
        return struct.pack(">I", len(body)) + typ + body + struct.pack(">I", zlib.crc32(typ + body) & 0xFFFFFFFF)
    rows = b"".join(b"\x00" + bytes(px[y * size * 4:(y + 1) * size * 4]) for y in range(size))
    return (b"\x89PNG\r\n\x1a\n"
            + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(rows, 9))
            + chunk(b"IEND", b""))


def encode_dib(size, px):
    """32-bit BGRA DIB (bottom-up) + 1-bit AND mask, as stored inside .ico."""
    hdr = struct.pack("<IiiHHIIiiII", 40, size, size * 2, 1, 32, 0, size * size * 4, 0, 0, 0, 0)
    xor = bytearray()
    for y in range(size - 1, -1, -1):
        for x in range(size):
            i = (y * size + x) * 4
            xor += bytes((px[i + 2], px[i + 1], px[i], px[i + 3]))
    mask_stride = ((size + 31) // 32) * 4
    mask = bytearray()
    for y in range(size - 1, -1, -1):
        row = bytearray(mask_stride)
        for x in range(size):
            if px[(y * size + x) * 4 + 3] == 0:
                row[x >> 3] |= 0x80 >> (x & 7)
        mask += row
    return hdr + xor + mask


def main():
    w, h, px = read_png(SRC)
    images = []  # (size, bytes)
    for s in DIB_SIZES:
        images.append((s, encode_dib(s, downscale(w, h, px, s))))
    for s in PNG_SIZES:
        images.append((s, encode_png(s, downscale(w, h, px, s))))
    out = struct.pack("<HHH", 0, 1, len(images))
    offset = 6 + 16 * len(images)
    body = b""
    for s, img in images:
        dim = 0 if s >= 256 else s
        out += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(img), offset + len(body))
        body += img
    OUT.write_bytes(out + body)
    print(f"wrote {OUT.name}: {len(images)} images, {len(out) + len(body)} bytes")


if __name__ == "__main__":
    sys.exit(main())
