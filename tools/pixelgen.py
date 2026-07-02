#!/usr/bin/env python3
"""Generate cozyui-style pixel art from a text prompt.

Calls black-forest-labs/flux.2-pro via OpenRouter, passing wavey.png,
puter_o_lc.png and assets/lamp_on.png as style references, then downscales
the result to the requested pixel grid and quantizes it to the na16 palette.

Usage:
    export OPENROUTER_API_KEY=sk-or-...
    tools/pixelgen.py "a cactus in a clay pot" --size 32
    tools/pixelgen.py "a steaming coffee mug" --size 16 -o mug.png
"""

import argparse
import base64
import io
import json
import os
import re
import sys
import urllib.request
from pathlib import Path

import numpy as np
from PIL import Image, ImageChops

REPO_ROOT = Path(__file__).resolve().parent.parent

REFERENCE_IMAGES = [
    REPO_ROOT / "wavey.png",
    REPO_ROOT / "puter_o_lc.png",
    REPO_ROOT / "assets" / "lamp_on.png",
]

# na16 palette, extracted from na16-1x.png
NA16 = [
    "#1f0e1c", "#3e2137", "#584563", "#70377f",
    "#17434b", "#34859d", "#7ec4c1", "#8c8fae",
    "#647d34", "#c0c741", "#f5edba", "#d79b7d",
    "#9a6348", "#9d303b", "#d26471", "#e4943a",
]

API_URL = "https://openrouter.ai/api/v1/chat/completions"
MODEL = "black-forest-labs/flux.2-pro"


def build_prompt(subject: str, size: int) -> str:
    return (
        f"Pixel art sprite of: {subject}.\n"
        f"Design it for a tiny {size}x{size} pixel grid, so keep the shapes "
        f"bold and simple with clean single-pixel outlines and no fine detail "
        f"that would be lost at {size}x{size} resolution. "
        f"Use only colors from this 16-color palette: {', '.join(NA16)}. "
        "Match the cozy, warm, hand-crafted pixel art style of the attached "
        "reference images (they are sprites from the same application). "
        "Flat colors, no gradients, no anti-aliasing, no dithering. "
        "Center the subject on a plain solid background using the darkest "
        "palette color (#1f0e1c) so it can be keyed out."
    )


def encode_image(path: Path) -> dict:
    data = base64.b64encode(path.read_bytes()).decode("ascii")
    return {
        "type": "image_url",
        "image_url": {"url": f"data:image/png;base64,{data}"},
    }


def call_openrouter(api_key: str, prompt: str) -> bytes:
    content = [{"type": "text", "text": prompt}]
    content += [encode_image(p) for p in REFERENCE_IMAGES]
    body = json.dumps({
        "model": MODEL,
        "modalities": ["image"],
        "messages": [{"role": "user", "content": content}],
    }).encode()

    req = urllib.request.Request(
        API_URL,
        data=body,
        headers={
            "Authorization": f"Bearer {api_key}",
            "Content-Type": "application/json",
        },
    )
    try:
        with urllib.request.urlopen(req, timeout=300) as resp:
            reply = json.load(resp)
    except urllib.error.HTTPError as err:
        detail = err.read().decode(errors="replace")
        try:
            detail = json.loads(detail)["error"]["message"]
        except (json.JSONDecodeError, KeyError):
            pass
        sys.exit(f"error: OpenRouter returned HTTP {err.code}: {detail}")

    message = reply["choices"][0]["message"]
    images = message.get("images") or []
    if not images:
        text = message.get("content") or "(no content)"
        sys.exit(f"error: model returned no image. Response text: {text}")

    url = images[0]["image_url"]["url"]
    if not url.startswith("data:"):
        with urllib.request.urlopen(url, timeout=60) as resp:
            return resp.read()
    return base64.b64decode(url.split(",", 1)[1])


def trim_padding(im: Image.Image, fuzz: int = 32) -> Image.Image:
    """Crop away the flat background border (like `magick -trim -fuzz`),
    then pad the result back to a square with that background color."""
    corners = [im.getpixel(p) for p in
               [(0, 0), (im.width - 1, 0), (0, im.height - 1),
                (im.width - 1, im.height - 1)]]
    bg_color = max(set(corners), key=corners.count)
    diff = ImageChops.difference(im, Image.new("RGB", im.size, bg_color))
    bbox = diff.convert("L").point(lambda v: 255 if v > fuzz else 0).getbbox()
    if bbox:
        im = im.crop(bbox)
    if im.width != im.height:
        side = max(im.size)
        square = Image.new("RGB", (side, side), bg_color)
        square.paste(im, ((side - im.width) // 2, (side - im.height) // 2))
        im = square
    return im


def quantize_to_na16(im: Image.Image, size: int, dither: int) -> Image.Image:
    im = trim_padding(im.convert("RGB"))
    im = im.resize((size, size), Image.LANCZOS)

    arr = np.asarray(im, dtype=np.float32)
    if dither:
        # Checkerboard dither: nudge pixel values up on even (x+y) parity
        # and down on odd, so borderline colors alternate between their two
        # nearest palette entries in a checker pattern.
        ys, xs = np.mgrid[0:size, 0:size]
        checker = np.where((xs + ys) % 2 == 0, float(dither), -float(dither))
        arr = np.clip(arr + checker[:, :, None], 0, 255)

    flat = []
    for color in NA16:
        flat += [int(color[i:i + 2], 16) for i in (1, 3, 5)]
    dithered = Image.fromarray(arr.astype(np.uint8), "RGB")
    pal = Image.new("P", (1, 1))
    pal.putpalette(flat + flat[:3] * (256 - len(NA16)))
    return dithered.quantize(palette=pal, dither=Image.Dither.NONE)


def auto_name(subject: str, size: int) -> str:
    slug = re.sub(r"[^a-z0-9]+", "_", subject.lower()).strip("_")[:40]
    return f"{slug}_{size}.png"


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("prompt", help="what to draw")
    ap.add_argument("--size", type=int, default=32,
                    help="output pixel size: a power of 2 or 1.5x a power "
                         "of 2, e.g. 16, 24, 32, 48 (default: 32)")
    ap.add_argument("-o", "--output", type=Path, default=None,
                    help="output PNG path (default: derived from the prompt)")
    ap.add_argument("--no-keep-raw", dest="keep_raw", action="store_false",
                    help="don't save the raw model output next to the result")
    ap.add_argument("--dither", type=int, default=12, metavar="STRENGTH",
                    help="checkerboard dither strength, 0 disables "
                         "(default: 12)")
    ap.add_argument("--from", dest="from_image", type=Path, metavar="FILE",
                    help="reprocess an existing image (e.g. a saved raw) "
                         "instead of calling the API")
    args = ap.parse_args()

    def is_pow2(n: int) -> bool:
        return n > 0 and n & (n - 1) == 0

    if not (8 <= args.size <= 512
            and (is_pow2(args.size)
                 or (args.size % 3 == 0 and is_pow2(args.size * 2 // 3)))):
        ap.error("--size must be a power of 2 or 1.5x a power of 2 "
                 "(16, 24, 32, 48, ...) between 8 and 512")

    out = args.output or Path(auto_name(args.prompt, args.size))

    if args.from_image:
        raw = args.from_image.read_bytes()
    else:
        api_key = os.environ.get("OPENROUTER_API_KEY")
        if not api_key:
            sys.exit("error: OPENROUTER_API_KEY is not set")
        for path in REFERENCE_IMAGES:
            if not path.exists():
                sys.exit(f"error: reference image missing: {path}")
        print(f"generating {args.size}x{args.size} sprite via {MODEL} ...")
        raw = call_openrouter(api_key, build_prompt(args.prompt, args.size))
        if args.keep_raw:
            raw_path = out.with_name(out.stem + "_raw.png")
            raw_path.write_bytes(raw)
            print(f"wrote {raw_path}")

    im = Image.open(io.BytesIO(raw))
    print(f"received {im.width}x{im.height} image, "
          f"resizing and quantizing to na16 ...")
    quantize_to_na16(im, args.size, args.dither).save(out, optimize=True)
    print(f"wrote {out}")


if __name__ == "__main__":
    main()
