#!/usr/bin/env python3
"""Export boxy pixel TTF outlines to a bitmap atlas plus Rust metrics.

Default usage regenerates Toodle's PeanutMoney font assets:

    python3 tools/export_pixel_font.py

Example for another ASCII pixel font:

    python3 tools/export_pixel_font.py \
        --font OtherPixel.ttf \
        --atlas fonts/other_pixel_ascii.png \
        --metrics src/other_pixel_font.rs \
        --const-prefix OTHER_PIXEL
"""

import argparse
import math
import re
import struct
import zlib
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
DEFAULT_FONT = ROOT / "PeanutMoney.ttf"
DEFAULT_ATLAS = ROOT / "fonts" / "peanut_money_ascii.png"
DEFAULT_METRICS = ROOT / "src" / "peanut_money_font.rs"


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


def cmap_dict(cmap):
    """Decode a cmap format 4 subtable into a {codepoint: glyph_id} map once,
    so per-codepoint lookups during a full-Unicode export are O(1) instead of
    a linear segment scan per character."""
    seg_count = u16(cmap, 6) // 2
    end_offset = 14
    start_offset = end_offset + seg_count * 2 + 2
    delta_offset = start_offset + seg_count * 2
    range_offset = delta_offset + seg_count * 2

    mapping = {}
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
                mapping[codepoint] = glyph
    return mapping


def glyph_offsets(font):
    head = table(font, "head")
    maxp = table(font, "maxp")
    loca = table(font, "loca")
    glyph_count = u16(maxp, 4)
    loca_format = i16(head, 50)
    if loca_format == 0:
        return [u16(loca, index * 2) * 2 for index in range(glyph_count + 1)]
    return [struct.unpack_from(">I", loca, index * 4)[0] for index in range(glyph_count + 1)]


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


def glyph_metric(font, glyph, units_per_pixel):
    advance, lsb = glyph_metric_units(font, glyph)
    return round(advance / units_per_pixel), round(lsb / units_per_pixel)


def decode_simple_glyph_units(data):
    contour_count = i16(data, 0)
    if contour_count == 0:
        return [], (0, 0, 0, 0)
    if contour_count < 0:
        raise ValueError("compound glyphs are not supported by this pixel-font exporter")

    bbox = tuple(i16(data, 2 + index * 2) for index in range(4))
    ends = [u16(data, 10 + index * 2) for index in range(contour_count)]
    instruction_len = u16(data, 10 + contour_count * 2)
    pos = 12 + contour_count * 2 + instruction_len
    point_count = ends[-1] + 1

    flags = []
    while len(flags) < point_count:
        flag = data[pos]
        pos += 1
        flags.append(flag)
        if flag & 0x08:
            repeat = data[pos]
            pos += 1
            flags.extend([flag] * repeat)

    xs = []
    x = 0
    for flag in flags:
        if flag & 0x02:
            delta = data[pos]
            pos += 1
            x += delta if flag & 0x10 else -delta
        elif not flag & 0x10:
            x += i16(data, pos)
            pos += 2
        xs.append(x)

    ys = []
    y = 0
    for flag in flags:
        if flag & 0x04:
            delta = data[pos]
            pos += 1
            y += delta if flag & 0x20 else -delta
        elif not flag & 0x20:
            y += i16(data, pos)
            pos += 2
        ys.append(y)

    contours = []
    start = 0
    for end in ends:
        contours.append(list(zip(xs[start : end + 1], ys[start : end + 1])))
        start = end + 1

    return contours, bbox


def decode_simple_glyph(data, units_per_pixel):
    contours, bbox = decode_simple_glyph_units(data)
    return [
        [(x / units_per_pixel, y / units_per_pixel) for x, y in contour]
        for contour in contours
    ], bbox


def infer_units_per_pixel(font, cmap, glyf, offsets, codepoints):
    hhea = table(font, "hhea")
    values = [abs(i16(hhea, 4)), abs(i16(hhea, 6))]
    for codepoint in codepoints:
        glyph = cmap.get(codepoint, 0)
        advance, lsb = glyph_metric_units(font, glyph)
        values.extend([abs(advance), abs(lsb)])

        start, end = offsets[glyph], offsets[glyph + 1]
        if start == end:
            continue
        contours, bbox = decode_simple_glyph_units(glyf[start:end])
        values.extend(abs(value) for value in bbox)
        for contour in contours:
            for x, y in contour:
                values.extend([abs(x), abs(y)])

    unit = 0
    for value in values:
        if value != 0:
            unit = value if unit == 0 else math.gcd(unit, value)
    if unit <= 0:
        raise ValueError("could not infer units per pixel; pass --units-per-pixel")
    return unit


def point_in_polygon(x, y, polygon):
    inside = False
    px, py = polygon[-1]
    for nx, ny in polygon:
        crosses = (ny > y) != (py > y)
        if crosses:
            edge_x = (px - nx) * (y - ny) / (py - ny) + nx
            if x < edge_x:
                inside = not inside
        px, py = nx, ny
    return inside


def glyph_covers_pixel(contours, x, y):
    return sum(point_in_polygon(x, y, contour) for contour in contours) % 2 == 1


def write_rgb_png(path, width, height, pixels):
    def chunk(kind, payload):
        body = kind + payload
        return struct.pack(">I", len(payload)) + body + struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF)

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


def build_arg_parser():
    parser = argparse.ArgumentParser(
        description="Export boxy pixel TTF glyphs to an RGB bitmap atlas and Rust metrics."
    )
    parser.add_argument(
        "--font",
        type=Path,
        action="append",
        dest="fonts",
        help="TTF file to export; repeat to merge several fonts into one, "
        "where each codepoint is taken from the first --font that has a glyph "
        "for it (priority decreases with each later --font)",
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
        default="PEANUT_MONEY",
        help="Rust constant prefix, e.g. PEANUT_MONEY or TINY_FONT",
    )
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
    parser.add_argument(
        "--block",
        type=int,
        default=128,
        help="codepoints per output PNG; blocks with no ink are skipped",
    )
    parser.add_argument(
        "--units-per-pixel",
        type=int,
        help="font units per output pixel; defaults to auto-detected outline grid",
    )
    return parser


def main():
    args = build_arg_parser().parse_args()
    font_paths = [path.resolve() for path in (args.fonts or [DEFAULT_FONT])]
    atlas_path = args.atlas.resolve()
    metrics_path = args.metrics.resolve()
    prefix = const_name(args.const_prefix)
    first = parse_codepoint(args.first)
    last = parse_codepoint(args.last)
    if first > last:
        raise ValueError("--first must be less than or equal to --last")
    if args.cols <= 0:
        raise ValueError("--cols must be positive")
    if args.block <= 0 or args.block % args.cols != 0:
        raise ValueError("--block must be positive and a multiple of --cols")

    # Load each source font once. Listed first means highest priority: when
    # several fonts cover the same codepoint, the earliest one wins. Metrics
    # (units-per-pixel, ascent, descent) are resolved per font so fonts whose
    # outlines use different grids still rasterize onto the same pixel cell.
    codepoints = list(range(first, last + 1))
    sources = []
    for font_path in font_paths:
        font = font_path.read_bytes()
        cmap = cmap_dict(cmap_format_4(font, font_path))
        glyf = table(font, "glyf")
        offsets = glyph_offsets(font)
        hhea = table(font, "hhea")
        units_per_pixel = args.units_per_pixel or infer_units_per_pixel(
            font, cmap, glyf, offsets, range(0x20, 0x7F)
        )
        if units_per_pixel <= 0:
            raise ValueError("--units-per-pixel must be positive")
        sources.append(
            {
                "font": font,
                "glyf": glyf,
                "offsets": offsets,
                "cmap": cmap,
                "units_per_pixel": units_per_pixel,
                "ascent": round(i16(hhea, 4) / units_per_pixel),
                "descent": round(-i16(hhea, 6) / units_per_pixel),
            }
        )

    ascent = max(source["ascent"] for source in sources)
    descent = max(source["descent"] for source in sources)
    x_origin = 0

    glyphs = {}
    table_len = max(last + 1, 128)
    advances = [0] * table_len
    max_w = 0
    for code in codepoints:
        # Take the codepoint from the first (highest-priority) font that has a
        # glyph for it; glyph 0 is .notdef, meaning the font lacks the
        # codepoint, so we fall through to the next font instead of baking the
        # missing-glyph box into the atlas.
        source, glyph = next(
            ((s, g) for s in sources if (g := s["cmap"].get(code))), (None, 0)
        )
        if source is None:
            continue
        font = source["font"]
        glyf = source["glyf"]
        offsets = source["offsets"]
        units_per_pixel = source["units_per_pixel"]
        start, end = offsets[glyph], offsets[glyph + 1]
        advance, lsb = glyph_metric(font, glyph, units_per_pixel)
        advances[code] = advance
        max_w = max(max_w, advance)

        contours = []
        if start != end:
            contours, bbox = decode_simple_glyph(glyf[start:end], units_per_pixel)
            x_min = round(bbox[0] / units_per_pixel)
            x_origin = max(x_origin, -x_min)
        glyphs[code] = contours

    cell_w = max_w + x_origin
    cell_h = ascent + descent
    rows = (table_len + args.cols - 1) // args.cols
    atlas_w = args.cols * cell_w
    atlas_h = rows * cell_h
    pixels = bytearray(atlas_w * atlas_h)

    for code, contours in glyphs.items():
        if not contours:
            continue
        cell_x = (code % args.cols) * cell_w
        cell_y = (code // args.cols) * cell_h
        for y in range(cell_h):
            font_y = ascent - y - 0.5
            for x in range(cell_w):
                font_x = x - x_origin + 0.5
                if glyph_covers_pixel(contours, font_x, font_y):
                    pixels[(cell_y + y) * atlas_w + cell_x + x] = 255

    # Pixel TTFs often declare a taller ascent/descent than any glyph ink
    # actually reaches, which would bake permanently blank rows into every
    # cell. Trim rows that are empty across all glyphs so a draw position
    # means the top of the ink and cell_h reflects the real text height.
    row_has_ink = [
        any(
            pixels[(cell_row * cell_h + y) * atlas_w + x]
            for cell_row in range(rows)
            for x in range(atlas_w)
        )
        for y in range(cell_h)
    ]
    if any(row_has_ink):
        top = row_has_ink.index(True)
        bottom = len(row_has_ink) - 1 - row_has_ink[::-1].index(True)
        if (top, bottom) != (0, cell_h - 1):
            trimmed_h = bottom - top + 1
            trimmed = bytearray(atlas_w * rows * trimmed_h)
            for cell_row in range(rows):
                for y in range(trimmed_h):
                    src = (cell_row * cell_h + top + y) * atlas_w
                    dst = (cell_row * trimmed_h + y) * atlas_w
                    trimmed[dst : dst + atlas_w] = pixels[src : src + atlas_w]
            pixels = trimmed
            cell_h = trimmed_h
            atlas_h = rows * cell_h

    atlas_path.parent.mkdir(parents=True, exist_ok=True)
    metrics_path.parent.mkdir(parents=True, exist_ok=True)

    # Split the atlas into fixed codepoint blocks so the export scales to the
    # whole of Unicode: each PNG holds `args.block` codepoints (16 wide), is
    # named for the codepoint range it covers, and is skipped entirely when no
    # glyph in the block has any ink.
    block_rows = args.block // args.cols
    block_h = block_rows * cell_h
    stem = re.sub(r"_(ascii|unicode)$", "", atlas_path.stem)
    written = []
    first_block = (first // args.block) * args.block
    for block_start in range(first_block, last + 1, args.block):
        block_end = block_start + args.block - 1
        # A block's cell rows are contiguous in `pixels`, so the whole block is
        # one slice (truncated for a short final block, zero-padded by `block`).
        src = (block_start // args.cols) * cell_h * atlas_w
        rows_slice = pixels[src : src + atlas_w * block_h]
        if not any(rows_slice):
            continue
        block = bytearray(atlas_w * block_h)
        block[: len(rows_slice)] = rows_slice
        block_path = atlas_path.with_name(f"{stem}_{block_start:04X}-{block_end:04X}.png")
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
                f"// Generated by tools/export_pixel_font.py from "
                f"{', '.join(path.name for path in font_paths)}.",
                f"// units_per_pixel={units_per_pixel}, range=U+{first:04X}..U+{last:04X}",
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
                f"pub const {prefix}_X_ORIGIN: usize = {x_origin};",
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
