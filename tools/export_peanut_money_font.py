#!/usr/bin/env python3
import struct
import zlib
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
FONT = ROOT / "PeanutMoney.ttf"
ATLAS = ROOT / "assets" / "peanut_money_ascii.png"
METRICS = ROOT / "src" / "peanut_money_font.rs"
FIRST = 32
LAST = 126
COLS = 16

# PeanutMoney's outlines are pixel boxes on a 64 font-unit grid.
UNITS_PER_PIXEL = 64


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


def cmap_format_4(font):
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
        raise ValueError("PeanutMoney.ttf has no cmap format 4 table")
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


def glyph_offsets(font):
    head = table(font, "head")
    maxp = table(font, "maxp")
    loca = table(font, "loca")
    glyph_count = u16(maxp, 4)
    loca_format = i16(head, 50)
    if loca_format == 0:
        return [u16(loca, index * 2) * 2 for index in range(glyph_count + 1)]
    return [struct.unpack_from(">I", loca, index * 4)[0] for index in range(glyph_count + 1)]


def glyph_metric(font, glyph):
    hhea = table(font, "hhea")
    hmtx = table(font, "hmtx")
    metric_count = u16(hhea, 34)
    if glyph < metric_count:
        advance, lsb = struct.unpack_from(">Hh", hmtx, glyph * 4)
    else:
        advance = u16(hmtx, (metric_count - 1) * 4)
        lsb = i16(hmtx, metric_count * 4 + (glyph - metric_count) * 2)
    return round(advance / UNITS_PER_PIXEL), round(lsb / UNITS_PER_PIXEL)


def decode_simple_glyph(data):
    contour_count = i16(data, 0)
    if contour_count <= 0:
        return [], (0, 0, 0, 0)

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
        xs.append(x / UNITS_PER_PIXEL)

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
        ys.append(y / UNITS_PER_PIXEL)

    contours = []
    start = 0
    for end in ends:
        contours.append(list(zip(xs[start : end + 1], ys[start : end + 1])))
        start = end + 1

    return contours, bbox


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


def main():
    font = FONT.read_bytes()
    cmap = cmap_format_4(font)
    glyf = table(font, "glyf")
    offsets = glyph_offsets(font)
    hhea = table(font, "hhea")

    ascent = round(i16(hhea, 4) / UNITS_PER_PIXEL)
    descent = round(-i16(hhea, 6) / UNITS_PER_PIXEL)
    x_origin = 0

    glyphs = {}
    advances = [0] * 128
    max_w = 0
    for code in range(FIRST, LAST + 1):
        glyph = glyph_id(cmap, code)
        start, end = offsets[glyph], offsets[glyph + 1]
        advance, lsb = glyph_metric(font, glyph)
        advances[code] = advance
        max_w = max(max_w, advance)

        contours = []
        if start != end:
            contours, bbox = decode_simple_glyph(glyf[start:end])
            x_min = round(bbox[0] / UNITS_PER_PIXEL)
            x_origin = max(x_origin, -x_min)
        glyphs[code] = contours

    cell_w = max_w + x_origin
    cell_h = ascent + descent
    rows = (128 + COLS - 1) // COLS
    atlas_w = COLS * cell_w
    atlas_h = rows * cell_h
    pixels = bytearray(atlas_w * atlas_h)

    for code, contours in glyphs.items():
        if not contours:
            continue
        cell_x = (code % COLS) * cell_w
        cell_y = (code // COLS) * cell_h
        for y in range(cell_h):
            font_y = ascent - y - 0.5
            for x in range(cell_w):
                font_x = x - x_origin + 0.5
                if glyph_covers_pixel(contours, font_x, font_y):
                    pixels[(cell_y + y) * atlas_w + cell_x + x] = 255

    write_rgb_png(ATLAS, atlas_w, atlas_h, pixels)

    metrics_body = ", ".join(str(width) for width in advances)
    METRICS.write_text(
        "\n".join(
            [
                "// Generated by tools/export_peanut_money_font.py from PeanutMoney.ttf.",
                f"pub(crate) const PEANUT_MONEY_ATLAS_PATH: &str = \"assets/{ATLAS.name}\";",
                f"pub(crate) const PEANUT_MONEY_CELL_W: usize = {cell_w};",
                f"pub(crate) const PEANUT_MONEY_CELL_H: usize = {cell_h};",
                f"pub(crate) const PEANUT_MONEY_COLS: usize = {COLS};",
                f"pub(crate) const PEANUT_MONEY_X_ORIGIN: usize = {x_origin};",
                f"pub(crate) const PEANUT_MONEY_ADVANCE: [u8; 128] = [{metrics_body}];",
                "",
            ]
        )
    )


if __name__ == "__main__":
    main()
