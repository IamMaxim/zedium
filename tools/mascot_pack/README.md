# mascot_pack

Asset pipeline for the Zed agent-panel mascot (spec:
`docs/superpowers/specs/2026-07-09-agent-animations-design.md`, §2.2 / §8).
Turns PNG frames into the animated WebP clips shipped at
`zed/assets/images/mascot/`, and generates the placeholder pixel-cat so every
state is testable before curated AI art lands.

## Setup

On this machine the Homebrew python3.14 has a broken `ensurepip` (pyexpat
symbol error), so use `uv`:

    uv venv .venv
    uv pip install --python .venv/bin/python -r requirements.txt

On machines with a healthy Python, the portable equivalent is:

    python3 -m venv .venv
    .venv/bin/pip install -r requirements.txt

## Commands

    # regenerate the placeholder pixel-cat clips (all 8 states + static.png + manifest.json)
    .venv/bin/python pack.py placeholder --out ../../zed/assets/images/mascot

    # slice an AI-generated horizontal film-strip into frames
    .venv/bin/python pack.py slice strip.png --frames 8 --state stretch --out frames/stretch

    # pack a directory of frames into one clip
    .venv/bin/python pack.py pack frames/stretch --fps 10 --out ../../zed/assets/images/mascot/stretch.webp

## Rules

- Frames are named `<state>_<index>.png` (zero-padded index so lexicographic
  sort equals frame order); masters are 512x512 with a REAL alpha channel.
- `slice` rejects strips without an alpha channel or with fewer than 5%
  fully-transparent pixels (catches baked-in checkerboard backgrounds), and
  strips whose width is not divisible by the frame count.
- Every clip is written lossless with per-frame duration `1000/fps` ms and
  `loop=0` (infinite) — including one-shot states. One-shot timing lives in
  the Rust controller (`MascotState::clip_duration()`); gpui's `img()` has no
  play-once API, so the controller swaps the clip when the timer fires.
- Pillow merges identical consecutive frames when encoding WebP (durations
  are summed), so a 10-frame clip may store fewer physical frames. Total
  cycle time is preserved; verify clips by total duration, not frame count.
- `manifest.json` records `state -> {frames, fps, loop}` for the art
  pipeline. The Rust side hardcodes the same numbers in
  `crates/ui/src/components/mascot_player.rs` (`MascotState::clip_duration`);
  if you change a state's frame count or fps, update BOTH.

## Art pipeline (curated art, later)

1. Generate one canonical character-sheet reference image, curate it.
2. Per state, generate ONE horizontal film-strip (all N frames side by side)
   with the reference attached (`codex exec -i reference.png "..."`) — frames
   born in one image stay consistent.
3. `pack.py slice` the strip, curate frames, `pack.py pack` at the spec fps.
4. Drop the clip into `zed/assets/images/mascot/<state>.webp` (same
   filename = zero code changes) and update `static.png` from the idle pose.
