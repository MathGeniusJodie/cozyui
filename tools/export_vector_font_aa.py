#!/usr/bin/env python3
"""Export a vector TTF to an antialiased bitmap atlas using ImageMagick.

Unlike export_vector_font.py (which thresholds to pure black/white), this keeps
edge antialiasing by quantizing every rendered pixel to one of 4 levels: black,
two shades of grey, and white. Output is split into fixed codepoint blocks the
same way export_pixel_font.py does, so coverage can span the whole of Unicode
(emoji included) without baking one enormous, mostly-empty PNG.

Export every emoji NotoEmoji covers, at 14x14, bold weight:

    python3 tools/export_vector_font_aa.py \
        --font NotoEmoji-VariableFont_wght.ttf \
        --weight 700 \
        --cell-size 14 \
        --emoji-data /tmp/emoji-data.txt \
        --atlas fonts/noto_emoji_14_aa.png \
        --metrics src/noto_emoji_14_font.rs \
        --const-prefix NOTO_EMOJI_14
"""

import argparse
import re
import struct
import subprocess
import tempfile
import zlib
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
DEFAULT_FONT = ROOT / "NotoEmoji-VariableFont_wght.ttf"
DEFAULT_ATLAS = ROOT / "fonts" / "noto_emoji_14_aa.png"
DEFAULT_METRICS = ROOT / "src" / "noto_emoji_14_font.rs"

# 4-level palette: black, dark grey, light grey, white.
LEVELS = (0, 85, 170, 255)


def u16(data, offset):
    return struct.unpack_from(">H", data, offset)[0]


def i16(data, offset):
    return struct.unpack_from(">h", data, offset)[0]


def u32(data, offset):
    return struct.unpack_from(">I", data, offset)[0]


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


def cmap_coverage(font, font_path):
    """Return the set of codepoints the font has a (non-.notdef) glyph for,
    reading a format 12 subtable when present (emoji live above the BMP, which
    format 4 cannot encode) and otherwise format 4."""
    cmap = table(font, "cmap")
    count = u16(cmap, 2)
    best = None  # (priority, format, offset)
    for index in range(count):
        platform, encoding, offset = struct.unpack_from(">HHI", cmap, 4 + index * 8)
        fmt = u16(cmap, offset)
        if fmt not in (4, 12):
            continue
        # Prefer format 12 (full Unicode), and the Windows tables within a format.
        priority = (1 if fmt == 12 else 0, 1 if platform == 3 else 0)
        if best is None or priority > best[0]:
            best = (priority, fmt, offset)
    if best is None:
        raise ValueError(f"{font_path} has no cmap format 4 or 12 table")

    _, fmt, offset = best
    return _cmap_format_12(cmap, offset) if fmt == 12 else _cmap_format_4(cmap, offset)


def _cmap_format_4(cmap, base):
    seg_count = u16(cmap, base + 6) // 2
    end_offset = base + 14
    start_offset = end_offset + seg_count * 2 + 2
    delta_offset = start_offset + seg_count * 2
    range_offset = delta_offset + seg_count * 2

    covered = set()
    for index in range(seg_count):
        end_code = u16(cmap, end_offset + index * 2)
        start_code = u16(cmap, start_offset + index * 2)
        if start_code == 0xFFFF:
            continue
        delta = i16(cmap, delta_offset + index * 2)
        glyph_range_offset = u16(cmap, range_offset + index * 2)
        for codepoint in range(start_code, end_code + 1):
            if glyph_range_offset == 0:
                glyph = (codepoint + delta) & 0xFFFF
            else:
                glyph_offset = (
                    range_offset
                    + index * 2
                    + glyph_range_offset
                    + (codepoint - start_code) * 2
                )
                raw = u16(cmap, glyph_offset)
                glyph = 0 if raw == 0 else (raw + delta) & 0xFFFF
            if glyph != 0:
                covered.add(codepoint)
    return covered


def _cmap_format_12(cmap, base):
    group_count = u32(cmap, base + 12)
    covered = set()
    for index in range(group_count):
        group = base + 16 + index * 12
        start_char = u32(cmap, group)
        end_char = u32(cmap, group + 4)
        start_glyph = u32(cmap, group + 8)
        for offset, codepoint in enumerate(range(start_char, end_char + 1)):
            if start_glyph + offset != 0:
                covered.add(codepoint)
    return covered


def emoji_codepoints(path):
    """Single codepoints carrying the Emoji property from a Unicode
    emoji-data.txt (UTS #51). Multi-codepoint sequences are out of scope: each
    atlas cell addresses exactly one `char`."""
    codes = set()
    for line in Path(path).read_text().splitlines():
        line = line.split("#", 1)[0].strip()
        if not line:
            continue
        field, _, prop = (part.strip() for part in line.partition(";"))
        if prop != "Emoji":
            continue
        if ".." in field:
            lo, hi = field.split("..")
            codes.update(range(int(lo, 16), int(hi, 16) + 1))
        else:
            codes.add(int(field, 16))
    return codes


def has_table(font, tag):
    count = u16(font, 4)
    return any(
        font[12 + i * 16 : 12 + i * 16 + 4] == tag.encode("latin1") for i in range(count)
    )


def instantiate_weight(font_path, weight, fonttools_cmd, out_dir):
    """ImageMagick's -weight cannot drive a variable font's wght axis, so pin it
    with fonttools' instancer and render the resulting static font instead.
    Returns the original path unchanged when the font has no fvar axis."""
    if not has_table(font_path.read_bytes(), "fvar"):
        return font_path
    out = Path(out_dir) / f"{font_path.stem}-wght{weight}.ttf"
    subprocess.run(
        [*fonttools_cmd.split(), "varLib.instancer", str(font_path),
         f"wght={weight}", "-o", str(out)],
        check=True,
        stdout=subprocess.DEVNULL,
    )
    return out


def quantize(value):
    """Snap an 8-bit grey value to the nearest of the 4 palette levels."""
    index = round(value / 255 * (len(LEVELS) - 1))
    return LEVELS[index]


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


def render_cell(font_path, pointsize, cell_size, ch):
    """Render `ch` centered in a square cell, antialiased, and quantized to the
    4-level palette. Returns `cell_size * cell_size` grey bytes."""
    command = [
        "magick",
        "-size",
        f"{cell_size}x{cell_size}",
        "xc:black",
        "-font",
        str(font_path),
        "-pointsize",
        str(pointsize),
        "-fill",
        "white",
        "-gravity",
        "center",
        "-annotate",
        "+0+0",
        ch,
        "-colorspace",
        "Gray",
        "-depth",
        "8",
        "gray:-",
    ]
    result = subprocess.run(command, check=True, stdout=subprocess.PIPE)
    pixels = result.stdout
    expected = cell_size * cell_size
    if len(pixels) != expected:
        raise ValueError(f"ImageMagick returned {len(pixels)} bytes, expected {expected}")
    return bytes(quantize(value) for value in pixels)


def build_arg_parser():
    parser = argparse.ArgumentParser(
        description="Export vector TTF glyphs to a 4-level antialiased bitmap atlas and Rust metrics."
    )
    parser.add_argument("--font", type=Path, default=DEFAULT_FONT, help="TTF file to export")
    parser.add_argument(
        "--weight",
        type=int,
        default=None,
        help="weight to pin a variable font's wght axis to, e.g. 700 (applied "
        "via fonttools instancing; ignored for static fonts)",
    )
    parser.add_argument(
        "--fonttools",
        default="fonttools",
        help="fonttools command used to instantiate --weight on variable fonts",
    )
    parser.add_argument("--atlas", type=Path, default=DEFAULT_ATLAS, help="output PNG atlas path")
    parser.add_argument(
        "--metrics",
        type=Path,
        default=DEFAULT_METRICS,
        help="output Rust constants path",
    )
    parser.add_argument(
        "--const-prefix",
        default="NOTO_EMOJI_14",
        help="Rust constant prefix, e.g. NOTO_EMOJI_14",
    )
    parser.add_argument(
        "--cell-size",
        type=int,
        default=14,
        help="square cell size in pixels; each glyph is centered in it",
    )
    parser.add_argument(
        "--pointsize",
        type=int,
        default=None,
        help="font pointsize; defaults to --cell-size",
    )
    parser.add_argument(
        "--emoji-data",
        type=Path,
        default=None,
        help="Unicode emoji-data.txt; restricts export to codepoints with the "
        "Emoji property (intersected with what the font covers)",
    )
    parser.add_argument(
        "--first",
        default="0x20",
        help="first codepoint to consider, decimal, hex, U+NNNN, or a single character",
    )
    parser.add_argument(
        "--last",
        default="0x1FAFF",
        help="last codepoint to consider, decimal, hex, U+NNNN, or a single character",
    )
    parser.add_argument("--cols", type=int, default=16, help="atlas columns")
    parser.add_argument(
        "--block",
        type=int,
        default=256,
        help="codepoints per output PNG; blocks with no ink are skipped",
    )
    return parser


def main():
    args = build_arg_parser().parse_args()
    font_path = args.font.resolve()
    atlas_path = args.atlas.resolve()
    metrics_path = args.metrics.resolve()
    prefix = const_name(args.const_prefix)
    first = parse_codepoint(args.first)
    last = parse_codepoint(args.last)
    cell_size = args.cell_size
    pointsize = args.pointsize or cell_size
    if first > last:
        raise ValueError("--first must be less than or equal to --last")
    if args.cols <= 0:
        raise ValueError("--cols must be positive")
    if args.block <= 0 or args.block % args.cols != 0:
        raise ValueError("--block must be positive and a multiple of --cols")
    if cell_size <= 0:
        raise ValueError("--cell-size must be positive")
    if cell_size > 255:
        raise ValueError("--cell-size must fit a u8 advance")

    font = font_path.read_bytes()
    covered = cmap_coverage(font, font_path)
    tmp_dir = tempfile.mkdtemp(prefix="font_aa_")
    render_path = (
        instantiate_weight(font_path, args.weight, args.fonttools, tmp_dir)
        if args.weight is not None
        else font_path
    )
    wanted = set(range(first, last + 1))
    if args.emoji_data is not None:
        wanted &= emoji_codepoints(args.emoji_data)
    codepoints = sorted(covered & wanted)
    if not codepoints:
        raise ValueError("no codepoints to export after intersecting font and filters")

    # Square cells: the glyph is centered, the advance is the cell itself, so
    # emoji tile a uniform grid. The atlas is indexed by absolute codepoint, the
    # same convention font.rs / export_pixel_font.py use.
    cell_w = cell_h = cell_size
    table_len = max(last + 1, 128)
    advances = [0] * table_len
    last_code = codepoints[-1]
    rows = (last_code // args.cols) + 1
    atlas_w = args.cols * cell_w
    atlas_h = rows * cell_h
    pixels = bytearray(atlas_w * atlas_h)

    for code in codepoints:
        cell = render_cell(render_path, pointsize, cell_size, chr(code))
        if not any(cell):
            continue
        advances[code] = cell_w
        cell_x = (code % args.cols) * cell_w
        cell_y = (code // args.cols) * cell_h
        for y in range(cell_h):
            dst = (cell_y + y) * atlas_w + cell_x
            pixels[dst : dst + cell_w] = cell[y * cell_w : (y + 1) * cell_w]

    atlas_path.parent.mkdir(parents=True, exist_ok=True)
    metrics_path.parent.mkdir(parents=True, exist_ok=True)

    # Split into fixed codepoint blocks, naming each PNG for the range it
    # covers and skipping blocks whose cells are all empty.
    block_rows = args.block // args.cols
    block_h = block_rows * cell_h
    stem = re.sub(r"_(ascii|unicode|aa)$", "", atlas_path.stem)
    written = []
    first_block = (first // args.block) * args.block
    for block_start in range(first_block, last_code + 1, args.block):
        block_end = block_start + args.block - 1
        src = (block_start // args.cols) * cell_h * atlas_w
        rows_slice = pixels[src : src + atlas_w * block_h]
        if not any(rows_slice):
            continue
        block = bytearray(atlas_w * block_h)
        block[: len(rows_slice)] = rows_slice
        block_path = atlas_path.with_name(f"{stem}_aa_{block_start:04X}-{block_end:04X}.png")
        write_rgb_png(block_path, atlas_w, block_h, block)
        written.append((block_start, block_path))

    if not written:
        raise ValueError("no glyphs had ink; nothing was exported")

    metrics_body = ", ".join(str(width) for width in advances)
    spec_name = f"{prefix}_SPEC"
    block_list = "\n".join(f"//   {repo_relative(path)}" for _, path in written)
    atlas_entries = "\n".join(
        f"    FontAtlas {{\n"
        f"        first_codepoint: 0x{block_start:04X},\n"
        f"        path: concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/{repo_relative(path)}\"),\n"
        f"    }},"
        for block_start, path in written
    )
    metrics_path.write_text(
        "\n".join(
            [
                f"// Generated by tools/export_vector_font_aa.py from {font_path.name}.",
                f"// cell={cell_size}px, 4-level antialiased, "
                f"{len(codepoints)} glyphs in U+{first:04X}..U+{last:04X}",
                f"// {len(written)} atlas block(s), {args.block} codepoints each:",
                block_list,
                "use crate::text::{FontAtlas, FontSpec};",
                "",
                "#[rustfmt::skip]",
                f"pub const {prefix}_ATLASES: &[FontAtlas] = &[",
                atlas_entries,
                "];",
                f"pub const {prefix}_BLOCK: usize = {args.block};",
                f"pub const {prefix}_CELL_W: usize = {cell_w};",
                f"pub const {prefix}_CELL_H: usize = {cell_h};",
                f"pub const {prefix}_COLS: usize = {args.cols};",
                f"pub const {prefix}_X_ORIGIN: usize = 0;",
                "#[rustfmt::skip]",
                f"pub static {prefix}_ADVANCE: [u8; {table_len}] = [{metrics_body}];",
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
