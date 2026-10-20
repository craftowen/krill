#!/usr/bin/env python3
"""assets/icon.svg에서 PWA PNG 아이콘과 .gz 짝을 다시 만든다(macOS sips + 표준 라이브러리).

    python3 crates/krilld/src/mobile/make_icons.py

SVG 타일이 꽉 찬 남색이라 알파는 전부 255여야 하고, RGB PNG로 다시 써서 불투명하게 만든다.
마스커블은 마크만 축소해 안전 영역(지름 80% 원) 안에 넣고 그 조건을 검사한다.
"""
import gzip
import pathlib
import struct
import subprocess
import tempfile
import zlib

ASSETS = pathlib.Path(__file__).resolve().parent / "assets"
TILE = (0x08, 0x09, 0x0E)
MASKABLE_MARK_SCALE = 0.8
ICONS = [("icon-192.png", 192, 1), ("icon-512.png", 512, 1),
         ("icon-maskable-512.png", 512, MASKABLE_MARK_SCALE), ("apple-touch-icon.png", 180, 1)]


def render(svg, size):
    with tempfile.TemporaryDirectory() as tmp:
        src, out = pathlib.Path(tmp, "i.svg"), pathlib.Path(tmp, "i.png")
        src.write_text(svg)
        subprocess.run(["sips", "-s", "format", "png", "-z", str(size), str(size), str(src), "--out", str(out)],
                       check=True, capture_output=True)
        return out.read_bytes()


def decode_rgba(png):
    pos, idat, head = 8, b"", None
    while pos < len(png):
        n, kind = struct.unpack(">I4s", png[pos:pos + 8])
        body = png[pos + 8:pos + 8 + n]
        if kind == b"IHDR":
            head = struct.unpack(">IIBBBBB", body)
        elif kind == b"IDAT":
            idat += body
        pos += 12 + n
    w, h, depth, color, _, _, interlace = head
    assert (depth, color, interlace) == (8, 6, 0), head
    raw, stride, rows, prev = zlib.decompress(idat), w * 4, [], bytearray(w * 4)
    for y in range(h):
        f, line = raw[y * (stride + 1)], bytearray(raw[y * (stride + 1) + 1:(y + 1) * (stride + 1)])
        for i in range(stride):
            a, b = line[i - 4] if i >= 4 else 0, prev[i]
            c = prev[i - 4] if i >= 4 else 0
            p = a + b - c
            pred = [0, a, b, (a + b) // 2,
                    a if abs(p - a) <= min(abs(p - b), abs(p - c)) else b if abs(p - b) <= abs(p - c) else c][f]
            line[i] = (line[i] + pred) & 0xFF
        rows.append(line)
        prev = line
    return w, h, rows


def encode_rgb(w, h, rows):
    def chunk(kind, body):
        return struct.pack(">I", len(body)) + kind + body + struct.pack(">I", zlib.crc32(kind + body))
    raw = b"".join(b"\0" + bytes(r[i] for i in range(len(r)) if i % 4 != 3) for r in rows)
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b""))


def main():
    svg = (ASSETS / "icon.svg").read_text()
    for name, size, scale in ICONS:
        s = svg.replace("<svg ", f'<svg width="{size}" height="{size}" ', 1)
        if scale != 1:
            s = s.replace('<g transform="', f'<g transform="translate(96 96) scale({scale}) translate(-96 -96) ', 1)
        w, h, rows = decode_rgba(render(s, size))
        assert (w, h) == (size, size) and all(r[i] == 255 for r in rows for i in range(3, len(r), 4)), name
        if scale != 1:  # 타일색과 다른 픽셀(마크)이 중심에서 반지름 40% 안에 있어야 한다.
            far = max(((x + .5 - w / 2) ** 2 + (y + .5 - h / 2) ** 2) ** .5 for y, r in enumerate(rows)
                      for x in range(w) if max(abs(r[x * 4 + k] - TILE[k]) for k in range(3)) > 8)
            assert far <= 0.4 * w, (name, far)
            print(f"{name}: mark radius {far / w:.3f} of size (safe ≤ 0.400)")
        (ASSETS / name).write_bytes(encode_rgb(w, h, rows))
    for name in [n for n, *_ in ICONS] + ["manifest.json", "index.html"]:
        (ASSETS / (name + ".gz")).write_bytes(gzip.compress((ASSETS / name).read_bytes(), 9, mtime=0))


if __name__ == "__main__":
    main()
