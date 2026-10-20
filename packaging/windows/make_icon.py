"""packaging/macos/AppIcon.icns에서 Windows exe 아이콘 krill.ico(16·32·48·256, PNG 항목)를 만든다.

macOS 전용(iconutil·sips). 실행: python3 packaging/windows/make_icon.py
krill-app/build.rs가 Windows 타깃에서 이 파일을 리소스 ID 1로 임베드한다(gpui가 창 클래스 아이콘으로 읽는다).
"""

import pathlib
import struct
import subprocess
import tempfile

HERE = pathlib.Path(__file__).resolve().parent
ICNS = HERE.parent / "macos" / "AppIcon.icns"
OUT = HERE / "krill.ico"
SIZES = (16, 32, 48, 256)


def main():
    with tempfile.TemporaryDirectory() as tmp:
        iconset = pathlib.Path(tmp) / "AppIcon.iconset"
        subprocess.run(["iconutil", "-c", "iconset", str(ICNS), "-o", str(iconset)], check=True)
        src = iconset / "icon_512x512.png"
        images = []
        for size in SIZES:
            png = pathlib.Path(tmp) / f"{size}.png"
            subprocess.run(["sips", "-z", str(size), str(size), str(src), "--out", str(png)],
                           check=True, capture_output=True)
            images.append((size, png.read_bytes()))

    # ICONDIR + ICONDIRENTRY[n] + PNG 데이터. 256은 너비·높이 바이트를 0으로 적는다.
    header = struct.pack("<HHH", 0, 1, len(images))
    offset = len(header) + 16 * len(images)
    entries, blobs = b"", b""
    for size, data in images:
        dim = 0 if size >= 256 else size
        entries += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(data), offset)
        offset += len(data)
        blobs += data
    OUT.write_bytes(header + entries + blobs)
    print(f"wrote {OUT} ({', '.join(map(str, SIZES))})")


if __name__ == "__main__":
    main()
