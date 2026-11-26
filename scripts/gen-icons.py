#!/usr/bin/env python3
"""
Generate Lunacy brand SVG variants and platform icon assets from assets/logo.svg.

Outputs:
  assets/logo-dark.svg                White mark (#fafafa) for dark backgrounds
  assets/logo-light.svg               Dark mark (#18181b) for light backgrounds
  assets/favicon.svg                  Adaptive light/dark browser favicon SVG
  assets/icons/lunacy.svg             Scalable app-icon tile SVG (Linux hicolor)
  assets/icons/lunacy-{16..512}.png   Desktop/Linux PNG icon set
  assets/icons/lunacy-macos-light-256.png  macOS squircle tile (light Dock)
  assets/icons/lunacy-macos-dark-256.png   macOS squircle tile (dark Dock)
  assets/icons/lunacy.ico             Windows multi-res icon (BMP <=128px + PNG 256px)
  assets/icons/lunacy.icns            macOS multi-res icon bundle (Apple squircle)
  assets/icons/favicon.ico            Web multi-res favicon (16/32/48px)
  assets/icons/apple-touch-icon.png   iOS/Safari 180x180 full-bleed tile
  assets/icons/icon-192.png           Web manifest 192x192 icon
  assets/icons/icon-512.png           Web manifest 512x512 icon

Requires: rsvg-convert (librsvg) and ffmpeg (for uncompressed ICO BMP entries).
"""

import os
import re
import struct
import subprocess
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
LOGO_SVG = ROOT / "assets" / "logo.svg"
ICONS_DIR = ROOT / "assets" / "icons"


def extract_inner_svg(svg_text: str) -> str:
    m = re.search(r"<svg[^>]*>(.*)</svg>", svg_text, re.DOTALL)
    if not m:
        raise ValueError("Could not extract inner markup from assets/logo.svg")
    return m.group(1).strip()


def make_desktop_tile_svg(inner_mark: str, *, dark_dock: bool = False) -> str:
    """Rounded-square app icon tile (1024x1024 viewBox) for Windows/Linux/PWA."""
    bg_top = "#202026" if dark_dock else "#17171b"
    bg_bot = "#121216" if dark_dock else "#0a0a0d"
    stroke = "#3d3d46" if dark_dock else "#2b2b31"
    mark = inner_mark.replace('fill="currentColor"', 'fill="#fafafa"')
    # Tile occupies 32..992 (960x960, rx=216) for balanced visual weight on Windows/Linux.
    # Mark scaled 5.6x (128 * 5.6 = 716.8), centered at (512, 512) -> translate(153.6, 153.6).
    return (
        '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1024 1024" fill="none">\n'
        "  <defs>\n"
        '    <linearGradient id="bg" x1="512" y1="32" x2="512" y2="992" gradientUnits="userSpaceOnUse">\n'
        f'      <stop offset="0%" stop-color="{bg_top}"/>\n'
        f'      <stop offset="100%" stop-color="{bg_bot}"/>\n'
        "    </linearGradient>\n"
        "  </defs>\n"
        f'  <rect x="32" y="32" width="960" height="960" rx="216" fill="url(#bg)" stroke="{stroke}" stroke-width="14"/>\n'
        '  <g transform="translate(153.6 153.6) scale(5.6)">\n'
        f"    {mark}\n"
        "  </g>\n"
        "</svg>\n"
    )


def make_macos_tile_svg(inner_mark: str, *, dark_dock: bool = False) -> str:
    """macOS Big Sur+ squircle-proportioned tile (824x824 centered in 1024x1024)."""
    bg_top = "#24242b" if dark_dock else "#18181c"
    bg_bot = "#141418" if dark_dock else "#0a0a0d"
    stroke = "#464652" if dark_dock else "#2d2d34"
    mark = inner_mark.replace('fill="currentColor"', 'fill="#fafafa"')
    # macOS standard icon grid: 824x824 centered at (512, 512) => x=100, y=100, rx=185.
    # Mark scaled 4.8x (128 * 4.8 = 614.4), centered at (512, 512) -> translate(204.8, 204.8).
    return (
        '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1024 1024" fill="none">\n'
        "  <defs>\n"
        '    <linearGradient id="bg" x1="512" y1="100" x2="512" y2="924" gradientUnits="userSpaceOnUse">\n'
        f'      <stop offset="0%" stop-color="{bg_top}"/>\n'
        f'      <stop offset="100%" stop-color="{bg_bot}"/>\n'
        "    </linearGradient>\n"
        "  </defs>\n"
        f'  <rect x="100" y="100" width="824" height="824" rx="185" fill="url(#bg)" stroke="{stroke}" stroke-width="14"/>\n'
        '  <g transform="translate(204.8 204.8) scale(4.8)">\n'
        f"    {mark}\n"
        "  </g>\n"
        "</svg>\n"
    )


def make_fullbleed_tile_svg(inner_mark: str) -> str:
    """Full-bleed square tile for Apple touch icons (iOS applies its own mask)."""
    mark = inner_mark.replace('fill="currentColor"', 'fill="#fafafa"')
    # Mark scaled 5.9x (128 * 5.9 = 755.2), centered at (512, 512) -> translate(134.4, 134.4).
    return (
        '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1024 1024" fill="none">\n'
        "  <defs>\n"
        '    <linearGradient id="bg" x1="512" y1="0" x2="512" y2="1024" gradientUnits="userSpaceOnUse">\n'
        '      <stop offset="0%" stop-color="#17171b"/>\n'
        '      <stop offset="100%" stop-color="#09090c"/>\n'
        "    </linearGradient>\n"
        "  </defs>\n"
        '  <rect width="1024" height="1024" fill="url(#bg)"/>\n'
        '  <g transform="translate(134.4 134.4) scale(5.9)">\n'
        f"    {mark}\n"
        "  </g>\n"
        "</svg>\n"
    )


def rsvg_png(svg_path: Path, png_path: Path, size: int) -> None:
    subprocess.run(
        ["rsvg-convert", "-w", str(size), "-h", str(size), str(svg_path), "-o", str(png_path)],
        check=True,
    )


def png_to_ico_dib(png_path: Path, size: int) -> bytes:
    """Convert PNG to 32-bit BGRA ICO DIB (BITMAPINFOHEADER + bottom-up BGRA + AND mask)."""
    raw = subprocess.run(
        ["ffmpeg", "-v", "error", "-i", str(png_path), "-f", "rawvideo", "-pix_fmt", "bgra", "-"],
        check=True,
        capture_output=True,
    ).stdout
    expected = size * size * 4
    if len(raw) != expected:
        raise ValueError(f"Expected {expected} BGRA bytes for {size}x{size}, got {len(raw)}")

    # Bottom-up rows for DIB
    stride = size * 4
    rows = [raw[y * stride : (y + 1) * stride] for y in range(size - 1, -1, -1)]
    pixel_data = b"".join(rows)

    # 1-bit AND mask (row padded to 4 bytes); 1 = transparent (alpha == 0), 0 = opaque
    mask_stride = ((size + 31) // 32) * 4
    mask_rows = bytearray()
    for y in range(size - 1, -1, -1):
        row_bytes = bytearray(mask_stride)
        for x in range(size):
            a = raw[(y * size + x) * 4 + 3]
            if a == 0:
                row_bytes[x // 8] |= 0x80 >> (x % 8)
        mask_rows.extend(row_bytes)

    # BITMAPINFOHEADER (40 bytes): height is 2*size (XOR + AND)
    header = struct.pack(
        "<IIIHHIIIIII",
        40,              # biSize
        size,            # biWidth
        size * 2,        # biHeight
        1,               # biPlanes
        32,              # biBitCount
        0,               # biCompression (BI_RGB)
        len(pixel_data) + len(mask_rows),  # biSizeImage
        0, 0, 0, 0,      # pels/colors
    )
    return header + pixel_data + bytes(mask_rows)


def write_ico(out_path: Path, entries: list[tuple[int, bytes]]) -> None:
    """Write a multi-resolution Windows .ico file."""
    count = len(entries)
    ico_header = struct.pack("<HHH", 0, 1, count)
    offset = 6 + 16 * count
    dir_entries = []
    payloads = []
    for size, data in entries:
        dim = 0 if size >= 256 else size
        dir_entries.append(struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(data), offset))
        payloads.append(data)
        offset += len(data)
    out_path.write_bytes(ico_header + b"".join(dir_entries) + b"".join(payloads))


def write_icns(out_path: Path, chunks: list[tuple[bytes, bytes]]) -> None:
    """Write an Apple .icns icon bundle."""
    body = b"".join(tag + struct.pack(">I", len(data) + 8) + data for tag, data in chunks)
    header = b"icns" + struct.pack(">I", len(body) + 8)
    out_path.write_bytes(header + body)


def main() -> None:
    ICONS_DIR.mkdir(parents=True, exist_ok=True)
    master = LOGO_SVG.read_text(encoding="utf-8")
    inner = extract_inner_svg(master)

    # 1. Fixed dark/light SVGs for README & static surfaces
    (ROOT / "assets" / "logo-dark.svg").write_text(
        master.replace('fill="currentColor"', 'fill="#fafafa"'),
        encoding="utf-8",
    )
    (ROOT / "assets" / "logo-light.svg").write_text(
        master.replace('fill="currentColor"', 'fill="#18181b"'),
        encoding="utf-8",
    )

    # 2. Standalone adaptive browser favicon SVG (scoped class, prefers-color-scheme)
    favicon_svg = (
        '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 128 128" fill="none">\n'
        "  <style>\n"
        "    .lunacy-favicon { color: #18181b; }\n"
        "    @media (prefers-color-scheme: dark) { .lunacy-favicon { color: #fafafa; } }\n"
        "  </style>\n"
        '  <g class="lunacy-favicon">\n'
        f"    {inner}\n"
        "  </g>\n"
        "</svg>\n"
    )
    (ROOT / "assets" / "favicon.svg").write_text(favicon_svg, encoding="utf-8")

    # 3. Scalable desktop app-icon tile SVG
    desktop_tile_svg = make_desktop_tile_svg(inner, dark_dock=False)
    desktop_svg_path = ICONS_DIR / "lunacy.svg"
    desktop_svg_path.write_text(desktop_tile_svg, encoding="utf-8")

    with tempfile.TemporaryDirectory() as tmp:
        tmp_dir = Path(tmp)
        mac_light_svg = tmp_dir / "mac_light.svg"
        mac_dark_svg = tmp_dir / "mac_dark.svg"
        fullbleed_svg = tmp_dir / "fullbleed.svg"

        mac_light_svg.write_text(make_macos_tile_svg(inner, dark_dock=False), encoding="utf-8")
        mac_dark_svg.write_text(make_macos_tile_svg(inner, dark_dock=True), encoding="utf-8")
        fullbleed_svg.write_text(make_fullbleed_tile_svg(inner), encoding="utf-8")

        # 4. Desktop/Linux PNG icon set
        for sz in (16, 32, 48, 64, 128, 256, 512):
            rsvg_png(desktop_svg_path, ICONS_DIR / f"lunacy-{sz}.png", sz)

        # 5. macOS Dock runtime light/dark pair (256px)
        rsvg_png(mac_light_svg, ICONS_DIR / "lunacy-macos-light-256.png", 256)
        rsvg_png(mac_dark_svg, ICONS_DIR / "lunacy-macos-dark-256.png", 256)

        # 6. Web / iOS / PWA PNG icons
        rsvg_png(fullbleed_svg, ICONS_DIR / "apple-touch-icon.png", 180)
        rsvg_png(desktop_svg_path, ICONS_DIR / "icon-192.png", 192)
        rsvg_png(desktop_svg_path, ICONS_DIR / "icon-512.png", 512)

        # 7. Windows multi-res .ico: uncompressed BMP DIB for <=128px (fast LoadImageW), PNG for 256px
        ico_entries: list[tuple[int, bytes]] = []
        for sz in (16, 24, 32, 48, 64, 128):
            p = tmp_dir / f"win-{sz}.png"
            rsvg_png(desktop_svg_path, p, sz)
            ico_entries.append((sz, png_to_ico_dib(p, sz)))
        p256 = ICONS_DIR / "lunacy-256.png"
        ico_entries.append((256, p256.read_bytes()))
        write_ico(ICONS_DIR / "lunacy.ico", ico_entries)

        # 8. Web favicon.ico (16, 32, 48px uncompressed BMP)
        fav_entries: list[tuple[int, bytes]] = []
        for sz in (16, 32, 48):
            p = tmp_dir / f"fav-{sz}.png"
            rsvg_png(desktop_svg_path, p, sz)
            fav_entries.append((sz, png_to_ico_dib(p, sz)))
        write_ico(ICONS_DIR / "favicon.ico", fav_entries)

        # 9. macOS multi-res .icns (Apple squircle grid)
        mac_pngs: dict[int, bytes] = {}
        for sz in (16, 32, 64, 128, 256, 512, 1024):
            p = tmp_dir / f"mac-{sz}.png"
            rsvg_png(mac_light_svg, p, sz)
            mac_pngs[sz] = p.read_bytes()

        icns_chunks = [
            (b"icp4", mac_pngs[16]),    # 16x16
            (b"icp5", mac_pngs[32]),    # 32x32
            (b"icp6", mac_pngs[64]),    # 64x64
            (b"ic07", mac_pngs[128]),   # 128x128
            (b"ic08", mac_pngs[256]),   # 256x256
            (b"ic09", mac_pngs[512]),   # 512x512
            (b"ic10", mac_pngs[1024]),  # 1024x1024 (512@2x)
            (b"ic11", mac_pngs[32]),    # 16x16@2x
            (b"ic12", mac_pngs[64]),    # 32x32@2x
            (b"ic13", mac_pngs[256]),   # 128x128@2x
            (b"ic14", mac_pngs[512]),   # 256x256@2x
        ]
        write_icns(ICONS_DIR / "lunacy.icns", icns_chunks)

    print("Generated all Lunacy brand and platform icon assets.")


if __name__ == "__main__":
    main()
