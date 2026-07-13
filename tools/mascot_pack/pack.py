#!/usr/bin/env python3
"""mascot_pack: pack PNG frames into animated WebP clips for the Zed agent mascot.

Subcommands:
  placeholder --out DIR          Generate the placeholder pixel-cat clips for all
                                 8 states plus static.png and manifest.json.
  pack FRAMESDIR --fps N --out FILE.webp
                                 Pack a directory of `<state>_<index>.png` frames
                                 into one animated WebP.
  slice STRIP.png --frames N --state NAME --out DIR
                                 Slice a horizontal film-strip into N equal frames,
                                 validating alpha and uniform dimensions.

All clips are written with loop=0 (infinite loop) — including one-shot states.
One-shot playback semantics (play once, then return to a base state) are
implemented by the Rust MascotController via `MascotState::clip_duration()`
timers; gpui's img() element has no play/pause API, so the runtime simply swaps
the clip when the timer fires. Baking loop counts into the file would desync
from that logic, so we don't.
"""

import argparse
import json
import sys
from pathlib import Path

from PIL import Image

GRID = 24
MASTER = 512

PALETTE = {
    "B": (176, 168, 158, 255),  # warm-gray body
    "D": (124, 114, 104, 255),  # darker ears / tail tip
    "C": (91, 146, 229, 255),  # accent collar
    "E": (48, 42, 38, 255),  # eyes / mouth
    "H": (232, 106, 136, 255),  # heart (petted)
    "Z": (158, 158, 170, 255),  # drifting z (sleeping)
}

# state -> (frame_count, fps, loops)
STATES = {
    "idle": (10, 8, True),
    "listening": (8, 8, True),
    "thinking": (10, 8, True),
    "happy": (8, 10, False),
    "concerned": (6, 8, False),
    "sleeping": (8, 4, True),
    "stretch": (8, 10, False),
    "petted": (10, 10, False),
    "blink": (4, 10, False),
    "tail_flick": (6, 10, False),
    "look": (8, 8, False),
    "groom": (10, 8, False),
    "walk": (6, 10, True),
}


# --- pixel-grid helpers -----------------------------------------------------


def new_grid():
    return [["." for _ in range(GRID)] for _ in range(GRID)]


def put(grid, x, y, ch):
    if 0 <= x < GRID and 0 <= y < GRID:
        grid[y][x] = ch


def rect(grid, x0, y0, x1, y1, ch):
    for y in range(y0, y1 + 1):
        for x in range(x0, x1 + 1):
            put(grid, x, y, ch)


def render(grid):
    """24x24 char grid -> 512x512 RGBA image (nearest-neighbor upscale)."""
    image = Image.new("RGBA", (GRID, GRID), (0, 0, 0, 0))
    for y, row in enumerate(grid):
        for x, ch in enumerate(row):
            if ch != ".":
                image.putpixel((x, y), PALETTE[ch])
    return image.resize((MASTER, MASTER), Image.NEAREST)


# --- cat drawing ------------------------------------------------------------


def draw_ears(grid, top, style):
    if style == "up":
        rect(grid, 5, top + 1, 7, top + 1, "D")
        put(grid, 6, top, "D")
        rect(grid, 12, top + 1, 14, top + 1, "D")
        put(grid, 13, top, "D")
    elif style == "perked":
        rect(grid, 5, top, 7, top + 1, "D")
        put(grid, 6, top - 1, "D")
        rect(grid, 12, top, 14, top + 1, "D")
        put(grid, 13, top - 1, "D")
    elif style == "back":
        rect(grid, 4, top + 2, 5, top + 2, "D")
        rect(grid, 14, top + 2, 15, top + 2, "D")
    else:
        raise ValueError(f"unknown ear style: {style}")


def draw_eyes(grid, y, style):
    if style == "open":
        put(grid, 7, y, "E")
        put(grid, 12, y, "E")
    elif style == "up":
        put(grid, 7, y - 1, "E")
        put(grid, 12, y - 1, "E")
    elif style == "closed":
        rect(grid, 6, y, 8, y, "E")
        rect(grid, 11, y, 13, y, "E")
    elif style == "squint":
        put(grid, 6, y, "E")
        put(grid, 7, y, "E")
        put(grid, 12, y, "E")
        put(grid, 13, y, "E")
    elif style == "drift_left":
        put(grid, 6, y, "E")
        put(grid, 11, y, "E")
    elif style == "drift_right":
        put(grid, 8, y, "E")
        put(grid, 13, y, "E")
    else:
        raise ValueError(f"unknown eye style: {style}")


def draw_z(grid, x, y):
    rect(grid, x, y, x + 2, y, "Z")
    put(grid, x + 1, y + 1, "Z")
    rect(grid, x, y + 2, x + 2, y + 2, "Z")


def draw_heart(grid, x, y):
    put(grid, x, y, "H")
    put(grid, x + 2, y, "H")
    rect(grid, x, y + 1, x + 2, y + 1, "H")
    put(grid, x + 1, y + 2, "H")


def draw_cat(dy=0, ears="up", eyes="open", tail_x=0, extend=0):
    """Sitting cat, 3/4 front. dy: vertical head/body squash (breathing/bounce),
    tail_x: tail-tip sway (-1..1), extend: body elongation to the right (stretch)."""
    grid = new_grid()
    top = 3 + dy
    # head
    rect(grid, 4, top + 2, 15, top + 8, "B")
    draw_ears(grid, top, ears)
    draw_eyes(grid, top + 5, eyes)
    # tiny mouth
    put(grid, 9, top + 7, "E")
    put(grid, 10, top + 7, "E")
    # collar (single accent)
    rect(grid, 6, top + 9, 13, top + 9, "C")
    # body
    rect(grid, 5, top + 10, 14 + extend, 20, "B")
    # paws
    rect(grid, 6, 21, 7, 21, "B")
    rect(grid, 12 + extend, 21, 13 + extend, 21, "B")
    # tail, rising up-right with a dark tip
    put(grid, 15 + extend, 19, "B")
    put(grid, 16 + extend, 18, "B")
    put(grid, 17 + extend + tail_x, 17, "B")
    put(grid, 17 + extend + tail_x, 16, "D")
    put(grid, 17 + extend + tail_x, 15, "D")
    return grid


def draw_sleeping_cat(breath=0, z_frame=0):
    """Curled-up cat with a drifting z."""
    grid = new_grid()
    rect(grid, 5, 13 - breath, 17, 13 - breath, "B")
    rect(grid, 4, 14 - breath, 18, 20, "B")
    # ear nubs
    put(grid, 6, 12 - breath, "D")
    put(grid, 8, 12 - breath, "D")
    # closed eyes
    rect(grid, 6, 16, 7, 16, "E")
    rect(grid, 9, 16, 10, 16, "E")
    # tail wrapped along the front
    rect(grid, 11, 20, 17, 20, "D")
    # drifting z
    draw_z(grid, 19 + (z_frame % 2), 9 - z_frame)
    return grid


# --- per-state frame generation ----------------------------------------------


def frames_idle():
    dys = [0, 0, 0, 1, 1, 1, 1, 0, 0, 0]
    tails = [0, 0, 0, 0, 1, 1, 0, 0, 0, 0]
    frames = []
    for i in range(10):
        eyes = "closed" if i == 8 else "open"
        frames.append(draw_cat(dy=dys[i], ears="up", eyes=eyes, tail_x=tails[i]))
    return frames


def frames_listening():
    dys = [0, 0, 1, 1, 1, 1, 0, 0]
    return [draw_cat(dy=dys[i], ears="perked", eyes="up") for i in range(8)]


def frames_thinking():
    tails = [0, 1, 1, 0, -1, -1, 0, 1, 1, 0]
    frames = []
    for i in range(10):
        eyes = "drift_left" if i < 5 else "drift_right"
        frames.append(draw_cat(ears="up", eyes=eyes, tail_x=tails[i]))
    return frames


def frames_happy():
    dys = [0, -1, -2, -2, -1, 0, 0, 0]
    frames = []
    for i in range(8):
        ears = "perked" if i % 2 == 1 else "up"
        frames.append(draw_cat(dy=dys[i], ears=ears, eyes="squint", tail_x=i % 2))
    return frames


def frames_concerned():
    dys = [0, 0, 1, 1, 1, 1]
    frames = []
    for i in range(6):
        eyes = "drift_left" if i >= 3 else "open"
        frames.append(draw_cat(dy=dys[i], ears="back", eyes=eyes))
    return frames


def frames_sleeping():
    breaths = [0, 0, 0, 0, 1, 1, 1, 1]
    return [draw_sleeping_cat(breath=breaths[i], z_frame=i) for i in range(8)]


def frames_stretch():
    extends = [0, 1, 2, 3, 4, 4, 2, 1]
    dys = [0, 0, 1, 1, 1, 1, 0, 0]
    frames = []
    for i in range(8):
        eyes = "closed" if 3 <= i <= 5 else "open"
        frames.append(
            draw_cat(dy=dys[i], ears="perked", eyes=eyes, tail_x=1, extend=extends[i])
        )
    return frames


def frames_petted():
    frames = []
    for i in range(10):
        ears = "perked" if i % 2 == 1 else "up"
        eyes = "squint" if i < 2 else "closed"
        grid = draw_cat(ears=ears, eyes=eyes, tail_x=i % 2)
        draw_heart(grid, 17, 10 - i)
        frames.append(grid)
    return frames


def frames_blink():
    eyes = ["open", "closed", "closed", "open"]
    return [draw_cat(eyes=eyes[i]) for i in range(4)]


def frames_tail_flick():
    tails = [0, 1, 1, 0, -1, 0]
    return [draw_cat(tail_x=tails[i]) for i in range(6)]


def frames_look():
    eyes = ["drift_left", "drift_left", "drift_left",
            "drift_right", "drift_right", "drift_right",
            "up", "open"]
    return [draw_cat(eyes=eye) for eye in eyes]


def frames_groom():
    frames = []
    for i in range(10):
        licking = i % 2 == 1
        grid = draw_cat(eyes="closed" if licking else "open")
        if licking:
            # front paw raised toward the cheek
            rect(grid, 5, 12, 6, 13, "B")
        frames.append(grid)
    return frames


def frames_walk():
    frames = []
    for i in range(6):
        phase = i % 2
        grid = draw_cat(dy=phase, tail_x=1 - phase)
        # stride: erase the sitting paws, redraw offset so the pairs alternate
        rect(grid, 6, 21, 7, 21, ".")
        rect(grid, 12, 21, 13, 21, ".")
        rect(grid, 6 - phase, 21, 7 - phase, 21, "B")
        rect(grid, 12 + phase, 21, 13 + phase, 21, "B")
        frames.append(grid)
    return frames


FRAME_GENERATORS = {
    "idle": frames_idle,
    "listening": frames_listening,
    "thinking": frames_thinking,
    "happy": frames_happy,
    "concerned": frames_concerned,
    "sleeping": frames_sleeping,
    "stretch": frames_stretch,
    "petted": frames_petted,
    "blink": frames_blink,
    "tail_flick": frames_tail_flick,
    "look": frames_look,
    "groom": frames_groom,
    "walk": frames_walk,
}


# --- packing / slicing --------------------------------------------------------


def save_webp(images, fps, out_path):
    duration_ms = round(1000 / fps)
    images[0].save(
        out_path,
        format="WEBP",
        save_all=True,
        append_images=images[1:],
        duration=duration_ms,
        loop=0,
        lossless=True,
    )


def validate_alpha(image, label):
    if image.mode != "RGBA":
        raise SystemExit(
            f"{label}: expected RGBA (real alpha channel), got mode {image.mode}"
        )
    alpha = image.getchannel("A")
    fully_transparent = alpha.histogram()[0]
    total = image.width * image.height
    share = fully_transparent / total
    if share < 0.05:
        raise SystemExit(
            f"{label}: only {share:.1%} fully-transparent pixels (need >= 5%). "
            "Background is probably a baked-in checkerboard, not real transparency."
        )


def cmd_placeholder(args):
    out_dir = Path(args.out)
    out_dir.mkdir(parents=True, exist_ok=True)
    manifest = {}
    for state, (frame_count, fps, loops) in STATES.items():
        grids = FRAME_GENERATORS[state]()
        assert len(grids) == frame_count, (state, len(grids), frame_count)
        images = [render(grid) for grid in grids]
        if state == "walk":
            # gpui img() has no flip API, so ship both directions pre-mirrored
            for name, direction_images in [
                ("walk_left", images),
                ("walk_right", [image.transpose(Image.FLIP_LEFT_RIGHT) for image in images]),
            ]:
                clip_path = out_dir / f"{name}.webp"
                save_webp(direction_images, fps, clip_path)
                manifest[name] = {"frames": frame_count, "fps": fps, "loop": loops}
                print(f"wrote {clip_path} ({frame_count} frames @ {fps}fps)")
        else:
            clip_path = out_dir / f"{state}.webp"
            save_webp(images, fps, clip_path)
            manifest[state] = {"frames": frame_count, "fps": fps, "loop": loops}
            print(f"wrote {clip_path} ({frame_count} frames @ {fps}fps)")
    static_path = out_dir / "static.png"
    render(draw_cat()).save(static_path, format="PNG")
    print(f"wrote {static_path}")
    manifest_path = out_dir / "manifest.json"
    manifest_path.write_text(json.dumps(manifest, indent=2) + "\n")
    print(f"wrote {manifest_path}")


def cmd_pack(args):
    frames_dir = Path(args.framesdir)
    paths = sorted(frames_dir.glob("*.png"))
    if not paths:
        raise SystemExit(f"no .png frames found in {frames_dir}")
    images = []
    for path in paths:
        image = Image.open(path)
        validate_alpha(image, str(path))
        images.append(image)
    first_size = images[0].size
    for path, image in zip(paths, images):
        if image.size != first_size:
            raise SystemExit(
                f"{path}: frame size {image.size} != first frame size {first_size}"
            )
    save_webp(images, args.fps, args.out)
    print(f"wrote {args.out} ({len(images)} frames @ {args.fps}fps)")


def cmd_slice(args):
    strip_path = Path(args.strip)
    strip = Image.open(strip_path)
    validate_alpha(strip, str(strip_path))
    if strip.width % args.frames != 0:
        raise SystemExit(
            f"{strip_path}: width {strip.width} is not divisible by "
            f"{args.frames} frames — frames are not uniform"
        )
    frame_width = strip.width // args.frames
    state = args.state or strip_path.stem
    out_dir = Path(args.out)
    out_dir.mkdir(parents=True, exist_ok=True)
    for i in range(args.frames):
        frame = strip.crop((i * frame_width, 0, (i + 1) * frame_width, strip.height))
        frame_path = out_dir / f"{state}_{i:02d}.png"
        frame.save(frame_path, format="PNG")
        print(f"wrote {frame_path}")


def main(argv=None):
    parser = argparse.ArgumentParser(prog="pack.py", description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)

    p_placeholder = sub.add_parser(
        "placeholder", help="generate placeholder pixel-cat clips"
    )
    p_placeholder.add_argument("--out", required=True, help="output directory")
    p_placeholder.set_defaults(func=cmd_placeholder)

    p_pack = sub.add_parser("pack", help="pack PNG frames into an animated WebP")
    p_pack.add_argument("framesdir", help="directory of <state>_<index>.png frames")
    p_pack.add_argument("--fps", type=int, required=True)
    p_pack.add_argument("--out", required=True, help="output .webp path")
    p_pack.set_defaults(func=cmd_pack)

    p_slice = sub.add_parser("slice", help="slice a horizontal film-strip into frames")
    p_slice.add_argument("strip", help="film-strip PNG")
    p_slice.add_argument("--frames", type=int, required=True)
    p_slice.add_argument("--state", help="state name (default: strip filename stem)")
    p_slice.add_argument("--out", required=True, help="output directory")
    p_slice.set_defaults(func=cmd_slice)

    args = parser.parse_args(argv)
    args.func(args)


if __name__ == "__main__":
    main()
