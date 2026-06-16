#!/usr/bin/env python3
"""Export a vector TTF to a monochrome bitmap atlas using ImageMagick.

Example:

    python3 tools/export_vector_font.py \
        --font RozhaOne-Regular.ttf \
        --pixel-size 48 \
        --atlas fonts/rozha_one_48_ascii.png \
        --metrics src/rozha_one_48_font.rs \
        --const-prefix ROZHA_ONE_48 \
        --threshold 35
"""

import argparse
import math
import re
import struct
import subprocess
import zlib
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
DEFAULT_FONT = ROOT / "RozhaOne-Regular.ttf"
DEFAULT_ATLAS = ROOT / "fonts" / "rozha_one_48_ascii.png"
DEFAULT_METRICS = ROOT / "src" / "rozha_one_48_font.rs"


def u16(data, offset):
    return struct.unpack_from(">H", data, offset)[0]


def i16(data, offset):
    return struct.unpack_from(">h", data, offset)[0]


def table(font, tag):
    count = u16(font, 4)
    for index in range(count):
        offset = 12 + index * 16
        if font[offset : offset + 4].decode("latin1") == tag:
            table_offset, table_len = struct.unpack_from(">II", font, offset + 8)
            return font[table_offset : table_offset + table_len]
    raise ValueError(f"missing TTF table: {tag}")


def parse_codepoint(value):
    value = value.strip()
    if value.startswith(("0x", "0X")):
        return int(value, 16)
    if value.startswith(("U+", "u+")):
        return int(value[2:], 16)
    if len(value) == 1:
        return ord(value)
    return int(value, 10)


def const_name(value):
    name = re.sub(r"[^A-Za-z0-9]+", "_", value).strip("_").upper()
    if not name:
        raise ValueError("constant prefix must contain at least one letter or number")
    if name[0].isdigit():
        name = f"FONT_{name}"
    return name


def repo_relative(path):
    try:
        return path.resolve().relative_to(ROOT).as_posix()
    except ValueError:
        return path.as_posix()


def cmap_format_4(font, font_path):
    cmap = table(font, "cmap")
    count = u16(cmap, 2)
    fallback = None
    for index in range(count):
        platform, encoding, offset = struct.unpack_from(">HHI", cmap, 4 + index * 8)
        subtable = cmap[offset:]
        if u16(subtable, 0) != 4:
            continue
        fallback = subtable
        if platform == 3 and encoding == 1:
            return subtable
    if fallback is None:
        raise ValueError(f"{font_path} has no cmap format 4 table")
    return fallback


def glyph_id(cmap, codepoint):
    seg_count = u16(cmap, 6) // 2
    end_offset = 14
    start_offset = end_offset + seg_count * 2 + 2
    delta_offset = start_offset + seg_count * 2
    range_offset = delta_offset + seg_count * 2

    for index in range(seg_count):
        end_code = u16(cmap, end_offset + index * 2)
        start_code = u16(cmap, start_offset + index * 2)
        if not start_code <= codepoint <= end_code:
            continue

        delta = i16(cmap, delta_offset + index * 2)
        glyph_range_offset = u16(cmap, range_offset + index * 2)
        if glyph_range_offset == 0:
            return (codepoint + delta) & 0xFFFF

        glyph_offset = range_offset + index * 2 + glyph_range_offset + (codepoint - start_code) * 2
        glyph = u16(cmap, glyph_offset)
        return 0 if glyph == 0 else (glyph + delta) & 0xFFFF

    return 0


def glyph_metric_units(font, glyph):
    hhea = table(font, "hhea")
    hmtx = table(font, "hmtx")
    metric_count = u16(hhea, 34)
    if glyph < metric_count:
        advance, lsb = struct.unpack_from(">Hh", hmtx, glyph * 4)
    else:
        advance = u16(hmtx, (metric_count - 1) * 4)
        lsb = i16(hmtx, metric_count * 4 + (glyph - metric_count) * 2)
    return advance, lsb


def write_rgb_png(path, width, height, pixels):
    def chunk(kind, payload):
        body = kind + payload
        return (
            struct.pack(">I", len(payload))
            + body
            + struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF)
        )

    rows = bytearray()
    for y in range(height):
        rows.append(0)
        for value in pixels[y * width : (y + 1) * width]:
            rows.extend((value, value, value))

    png = b"\x89PNG\r\n\x1a\n"
    png += chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
    png += chunk(b"IDAT", zlib.compress(bytes(rows), 9))
    png += chunk(b"IEND", b"")
    path.write_bytes(png)


def render_glyph(font_path, pixel_size, threshold, cell_w, cell_h, x_origin, baseline, ch):
    result = subprocess.run(
        [
            "magick",
            "-size",
            f"{cell_w}x{cell_h}",
            "xc:black",
            "-font",
            str(font_path),
            "-pointsize",
            str(pixel_size),
            "-fill",
            "white",
            "-annotate",
            f"+{x_origin}+{baseline}",
            ch,
            "-threshold",
            f"{threshold}%",
            "-depth",
            "8",
            "gray:-",
        ],
        check=True,
        stdout=subprocess.PIPE,
    )
    pixels = result.stdout
    expected = cell_w * cell_h
    if len(pixels) != expected:
        raise ValueError(f"ImageMagick returned {len(pixels)} bytes, expected {expected}")
    return pixels


def build_arg_parser():
    parser = argparse.ArgumentParser(
        description="Export vector TTF glyphs to a thresholded bitmap atlas and Rust metrics."
    )
    parser.add_argument("--font", type=Path, default=DEFAULT_FONT, help="TTF file to export")
    parser.add_argument("--atlas", type=Path, default=DEFAULT_ATLAS, help="output PNG atlas path")
    parser.add_argument(
        "--metrics",
        type=Path,
        default=DEFAULT_METRICS,
        help="output Rust constants path",
    )
    parser.add_argument(
        "--const-prefix",
        default="ROZHA_ONE_48",
        help="Rust constant prefix, e.g. ROZHA_ONE_48",
    )
    parser.add_argument("--pixel-size", type=int, default=48, help="font pixel size")
    parser.add_argument(
        "--threshold",
        type=int,
        default=35,
        help="ImageMagick threshold percentage; lower values keep more edge pixels",
    )
    parser.add_argument("--padding", type=int, default=8, help="cell padding in pixels")
    parser.add_argument(
        "--first",
        default="32",
        help="first codepoint to export, decimal, hex, U+NNNN, or a single character",
    )
    parser.add_argument(
        "--last",
        default="126",
        help="last codepoint to export, decimal, hex, U+NNNN, or a single character",
    )
    parser.add_argument("--cols", type=int, default=16, help="atlas columns")
    return parser


def main():
    args = build_arg_parser().parse_args()
    font_path = args.font.resolve()
    atlas_path = args.atlas.resolve()
    metrics_path = args.metrics.resolve()
    prefix = const_name(args.const_prefix)
    first = parse_codepoint(args.first)
    last = parse_codepoint(args.last)
    if first > last:
        raise ValueError("--first must be less than or equal to --last")
    if args.cols <= 0:
        raise ValueError("--cols must be positive")
    if args.pixel_size <= 0:
        raise ValueError("--pixel-size must be positive")
    if not 0 <= args.threshold <= 100:
        raise ValueError("--threshold must be between 0 and 100")
    if args.padding < 0:
        raise ValueError("--padding must be non-negative")

    font = font_path.read_bytes()
    head = table(font, "head")
    hhea = table(font, "hhea")
    cmap = cmap_format_4(font, font_path)
    units_per_em = u16(head, 18)
    scale = args.pixel_size / units_per_em

    codepoints = list(range(first, last + 1))
    table_len = max(last + 1, 128)
    advances = [0] * table_len
    x_origin = args.padding
    max_w = 0
    for code in codepoints:
        glyph = glyph_id(cmap, code)
        advance_units, lsb_units = glyph_metric_units(font, glyph)
        advance = math.ceil(advance_units * scale)
        if advance > 255:
            raise ValueError(f"advance for U+{code:04X} is {advance}; FontSpec stores u8 advances")
        advances[code] = advance
        max_w = max(max_w, advance)
        x_origin = max(x_origin, args.padding + math.ceil(max(0, -lsb_units) * scale))

    ascent = math.ceil(i16(hhea, 4) * scale)
    descent = math.ceil(-i16(hhea, 6) * scale)
    cell_w = max_w + x_origin + args.padding
    cell_h = ascent + descent + args.padding * 2
    baseline = args.padding + ascent
    rows = (table_len + args.cols - 1) // args.cols
    atlas_w = args.cols * cell_w
    atlas_h = rows * cell_h
    atlas = bytearray(atlas_w * atlas_h)

    for code in codepoints:
        glyph_pixels = render_glyph(
            font_path,
            args.pixel_size,
            args.threshold,
            cell_w,
            cell_h,
            x_origin,
            baseline,
            chr(code),
        )
        cell_x = (code % args.cols) * cell_w
        cell_y = (code // args.cols) * cell_h
        for y in range(cell_h):
            src_offset = y * cell_w
            dest_offset = (cell_y + y) * atlas_w + cell_x
            atlas[dest_offset : dest_offset + cell_w] = glyph_pixels[src_offset : src_offset + cell_w]

    atlas_path.parent.mkdir(parents=True, exist_ok=True)
    metrics_path.parent.mkdir(parents=True, exist_ok=True)
    write_rgb_png(atlas_path, atlas_w, atlas_h, atlas)

    metrics_body = ", ".join(str(width) for width in advances)
    spec_name = f"{prefix}_SPEC"
    metrics_path.write_text(
        "\n".join(
            [
                f"// Generated by tools/export_vector_font.py from {font_path.name}.",
                f"// pixel_size={args.pixel_size}, range=U+{first:04X}..U+{last:04X}",
                "use crate::text::{FontAtlas, FontSpec};",
                "",
                f"pub const {prefix}_ATLASES: &[FontAtlas] = &[FontAtlas {{",
                "    first_codepoint: 0x0000,",
                f"    path: concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/{repo_relative(atlas_path)}\"),",
                "}];",
                f"pub const {prefix}_BLOCK: usize = {table_len};",
                f"pub const {prefix}_CELL_W: usize = {cell_w};",
                f"pub const {prefix}_CELL_H: usize = {cell_h};",
                f"pub const {prefix}_COLS: usize = {args.cols};",
                f"pub const {prefix}_X_ORIGIN: usize = {x_origin};",
                "#[rustfmt::skip]",
                f"pub const {prefix}_ADVANCE: [u8; {table_len}] = [{metrics_body}];",
                "",
                f"pub const {spec_name}: FontSpec = FontSpec {{",
                f"    atlases: {prefix}_ATLASES,",
                f"    block: {prefix}_BLOCK,",
                f"    cell_w: {prefix}_CELL_W,",
                f"    cell_h: {prefix}_CELL_H,",
                f"    cols: {prefix}_COLS,",
                f"    x_origin: {prefix}_X_ORIGIN,",
                f"    advance: &{prefix}_ADVANCE,",
                "};",
                "",
            ]
        )
    )


if __name__ == "__main__":
    main()
