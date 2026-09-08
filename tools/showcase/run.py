#!/usr/bin/env python3
"""Shoots the scene list against a running game and writes docs/gallery.

Builds nothing: point --bin at a viewer that is already built. Every shot runs
in its own process with a private XDG_CACHE_HOME, so the chunk cache the user's
own viewer shares is never touched, and with DWARF_EYE_HUD=off so the picture
shows the world rather than the instrument.

    run.py --bin target/release/dwarf-eye            # shoot everything, write the page
    run.py --bin ... --check                         # re-shoot the baselines and diff

Nothing about a scene lives here: scenes.toml is the whole list.
"""

import argparse
import os
import re
import subprocess
import sys
import tempfile
import time
import tomllib
from datetime import date
from pathlib import Path

from PIL import Image, ImageChops, ImageStat

ROOT = Path(__file__).resolve().parents[2]
DEFAULT_SCENES = Path(__file__).resolve().parent / "scenes.toml"
DEFAULT_OUT = ROOT / "docs" / "gallery"
WIDTH = 1280
COLOURS = 256
SCENE_LINE = re.compile(r"scene: world (?P<world>.+?) date (?P<date>.+?)\s{2,}\d")


def load(path):
    with open(path, "rb") as f:
        doc = tomllib.load(f)
    scenes = doc.get("scene", [])
    for scene in scenes:
        for key in ("id", "title", "caption"):
            if key not in scene:
                sys.exit(f"{path}: a scene is missing {key}")
    ids = [s["id"] for s in scenes]
    if len(set(ids)) != len(ids):
        sys.exit(f"{path}: two scenes share an id")
    return doc.get("group", []), scenes


def command(scene, secs, out):
    """The env line that reproduces one shot by hand, exactly as the runner runs it."""
    env = dict(scene.get("env", {}))
    env["DWARF_EYE_HUD"] = "off"
    whole = int(secs) if float(secs).is_integer() else secs
    env["DWARF_EYE_SHOT"] = f"{out}:{whole}"
    return " ".join(f"{k}={v}" for k, v in env.items()) + " ./target/release/dwarf-eye"


def shoot(binary, cache, scene, secs, raw):
    """Runs one viewer to one screenshot. Returns its log, or None if it failed."""
    env = dict(os.environ)
    env.pop("DWARF_EYE_CACHE", None)
    env.update(
        XDG_CACHE_HOME=str(cache),
        RUST_LOG="warn,dwarf_eye=info",
        DWARF_EYE_HUD="off",
        DWARF_EYE_SHOT=f"{raw}:{secs}",
    )
    for key, value in scene.get("env", {}).items():
        env[key] = str(value)

    began = time.monotonic()
    try:
        done = subprocess.run(
            [str(binary)],
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            timeout=secs + 90,
            text=True,
        )
    except subprocess.TimeoutExpired:
        print(f"  {scene['id']}: the viewer never exited", file=sys.stderr)
        return None
    took = time.monotonic() - began
    if not raw.exists():
        print(f"  {scene['id']}: no screenshot after {took:.0f}s", file=sys.stderr)
        sys.stderr.write("\n".join(done.stdout.splitlines()[-15:]) + "\n")
        return None
    print(f"  {scene['id']}: {took:.0f}s")
    return done.stdout


def finish(raw, target, colours):
    """Downscales to 1280 wide and writes an indexed PNG."""
    image = Image.open(raw).convert("RGB")
    if image.width > WIDTH:
        height = round(image.height * WIDTH / image.width)
        image = image.resize((WIDTH, height), Image.LANCZOS)
    if colours:
        image = image.quantize(colors=colours, method=Image.Quantize.MEDIANCUT)
    target.parent.mkdir(parents=True, exist_ok=True)
    image.save(target, optimize=True)


def empty(path):
    """True when the frame is one flat colour: the map never reached the GPU."""
    small = Image.open(path).convert("RGB").resize((32, 18))
    data = small.tobytes()
    levels = [sum(data[i:i + 3]) / 3 for i in range(0, len(data), 3)]
    mean = sum(levels) / len(levels)
    return (sum((v - mean) ** 2 for v in levels) / len(levels)) ** 0.5 < 2.0


def difference(a, b):
    """Mean absolute difference per channel, 0-255.

    The window manager does not always hand the viewer the same window, so a
    fresh shot is scaled to the committed one rather than failing over a few
    rows of pixels.
    """
    one = Image.open(a).convert("RGB")
    two = Image.open(b).convert("RGB")
    if one.size != two.size:
        two = two.resize(one.size, Image.LANCZOS)
    stat = ImageStat.Stat(ImageChops.difference(one, two))
    return sum(stat.mean) / len(stat.mean)


def remembered(readme, world, when):
    """Keeps the header's world and date when this run did not learn them."""
    if world != "unknown" or not readme.exists():
        return world, when
    was = dict(
        (row[1].strip(), row[2].strip())
        for row in (line.split("|") for line in readme.read_text().splitlines())
        if len(row) > 3
    )
    return was.get("World", world), was.get("Game date", when)


def header(world, when):
    commit = subprocess.run(
        ["git", "-C", str(ROOT), "rev-parse", "--short", "HEAD"],
        capture_output=True, text=True,
    ).stdout.strip() or "unknown"
    lines = [
        "# Gallery",
        "",
        "Every frame here is the renderer against a live Dwarf Fortress, shot by",
        "`make showcase` from the list in `tools/showcase/scenes.toml`. Nothing is",
        "posed by hand: each scene is a set of environment knobs and a camera aim.",
        "",
        f"| World | {world} |",
        "|---|---|",
        f"| Game date | {when} |",
        f"| Commit | `{commit}` |",
        f"| Shot | {date.today().isoformat()} |",
        "",
        "The game's own clock and weather are never touched. `DWARF_EYE_HOUR` pins",
        "the hour the view is lit at, `DWARF_EYE_CLOUDS` and `DWARF_EYE_WEATHER` the",
        "sky, and every shot runs against a private cache so the user's own viewer",
        "keeps its chunks.",
        "",
    ]
    return "\n".join(lines)


KNOBS = """
## The knobs

Every one of these is read from the environment, so any frame below can be
reproduced without a rebuild.

### Framing and capture

| | |
|---|---|
| `DWARF_EYE_SHOT=path[:seconds]` | save one screenshot after the delay, then exit |
| `DWARF_EYE_VIEW=yaw,pitch` | aim the camera in degrees; yaw 0 looks north, pitch is negative downward |
| `DWARF_EYE_CAM=n` | scale how far back and above the player the camera starts |
| `DWARF_EYE_HUD=off` | leave the overlay out of the picture |
| `DWARF_EYE_Z_OFFSET=n` | start a cut plane n levels from the player; negative looks into the rock |
| `DWARF_EYE_WALK=1` | start in walk sync, standing in the character's tile |
| `DWARF_EYE_WALK_DRIVE=bearing,seconds;…` | walk a scripted route — compass degrees, 0 north, an empty bearing to stand still |

### Sky, light and weather

| | |
|---|---|
| `DWARF_EYE_HOUR=hh[:mm]` | pin the hour the view is lit at; the game's clock stays where the player left it |
| `DWARF_EYE_CLOUDS=cumulus=0.5,fog=0.4` | force a sky by kind: `cumulus`, `stratus`, `cirrus`, `fog` |
| `DWARF_EYE_WEATHER=clear\\|rain\\|snow` | the three skies the `1` `2` `3` keys ask the game for, without asking the game |
| `DWARF_EYE_GODRAYS=density=..,strength=..,g=..,dim=..,steps=..\\|off` | override the light shafts |
| `DWARF_EYE_WIND=x,z` | wind in tiles per second |
| `DWARF_EYE_CLOUD_TUNE=sigma=..,detail=..,gain=..,ambient=..,haze=..,cirrus=..,shadow=..,steps=..` | the cloud shading knobs |
| `DWARF_EYE_EV100=n` | pin the exposure |
| `DWARF_EYE_CANOPY_SKY=n` | how much of the sky's fill a crown keeps |
| `DWARF_EYE_LEAF_LIGHT=n` | how much light a leaf passes from behind |

### Geometry and texture

| | |
|---|---|
| `DWARF_EYE_PLANTS=billboard` | put standing plants back on crossed sprites |
| `DWARF_EYE_PLANT_VOXELS=n` | sub-voxels per plant tile edge (default 2) |
| `DWARF_EYE_GRID=n` | sub-voxels per tile edge for sprite geometry (default 12, 4–32) |
| `DWARF_EYE_TEXELS=n` | texture density per tile |
| `DWARF_EYE_NO_MIPS=1` | the bare nearest-sampled atlas, for comparison |
| `DWARF_EYE_HORIZON_TRANSPOSE=1` | flip the region sample order, for testing |
| `DWARF_EYE_TREE_LOG=1` | log every tree grown |

### Data

| | |
|---|---|
| `DWARF_EYE_CACHE=dir` | the chunk cache root (otherwise `$XDG_CACHE_HOME/dwarf-eye`) |
| `DWARF_EYE_LIVE=1` | let the live walk test run; it **moves the adventurer** |
| `DWARF_EYE_SETTLE=n` | seconds that test waits for the map before setting off |
"""


def page(groups, scenes, world, when, out, secs):
    order = [g["id"] for g in groups]
    titles = {g["id"]: g.get("title", g["id"]) for g in groups}
    blurbs = {g["id"]: g.get("blurb", "") for g in groups}
    for scene in scenes:
        group = scene.get("group", "other")
        if group not in order:
            order.append(group)
            titles.setdefault(group, group.replace("-", " ").capitalize())

    text = [header(world, when), KNOBS.strip(), ""]
    for group in order:
        members = [s for s in scenes if s.get("group", "other") == group]
        if not members:
            continue
        text.append(f"## {titles[group]}")
        text.append("")
        if blurbs.get(group):
            text.append(blurbs[group])
            text.append("")
        for scene in members:
            image = f"{scene['id']}.png"
            if not (out / image).exists():
                continue
            text.append(f"### {scene['title']}")
            text.append("")
            text.append(f"![{scene['title']}]({image})")
            text.append("")
            text.append(scene["caption"])
            text.append("")
            text.append("```sh")
            text.append(command(scene, scene.get("secs", secs), image))
            text.append("```")
            text.append("")
    text.append("Re-shoot the lot with `make showcase`; `make showcase-check` re-shoots the")
    text.append("scenes marked `baseline` and reports how far each has drifted.")
    text.append("")
    return "\n".join(text)


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--bin", type=Path, default=Path("target/release/dwarf-eye"),
                    help="a viewer that is already built")
    ap.add_argument("--scenes", type=Path, default=DEFAULT_SCENES)
    ap.add_argument("--out", type=Path, default=DEFAULT_OUT)
    ap.add_argument("--cache", type=Path, default=None, help="the private XDG_CACHE_HOME")
    ap.add_argument("--secs", type=float, default=25.0, help="default shot delay")
    ap.add_argument("--colours", type=int, default=COLOURS, help="palette size, 0 for truecolour")
    ap.add_argument("--only", default=None, help="comma-separated scene ids")
    ap.add_argument("--check", action="store_true", help="re-shoot the baselines and diff")
    ap.add_argument("--page-only", action="store_true", help="rewrite the page, shoot nothing")
    ap.add_argument("--threshold", type=float, default=6.0, help="mean pixel difference allowed")
    args = ap.parse_args()

    if not args.bin.exists() and not args.page_only:
        sys.exit(f"no viewer at {args.bin}: build it first")

    groups, listed = load(args.scenes)
    scenes = listed
    if args.only:
        wanted = set(args.only.split(","))
        scenes = [s for s in scenes if s["id"] in wanted]
    if args.check:
        scenes = [s for s in scenes if s.get("baseline")]
    if args.page_only:
        scenes = []
    elif not scenes:
        sys.exit("no scenes to shoot")

    cache = args.cache or Path(
        os.environ.get("XDG_CACHE_HOME", Path.home() / ".cache")
    ) / "dwarf-eye-showcase"
    cache.mkdir(parents=True, exist_ok=True)

    world, when = "unknown", "unknown"
    failed, drift = [], []
    with tempfile.TemporaryDirectory(prefix="dwarf-eye-showcase-") as tmp:
        tmp = Path(tmp)
        print(f"{len(scenes)} scene(s), cache {cache}")
        for scene in scenes:
            raw = tmp / f"{scene['id']}-raw.png"
            target = (tmp / f"{scene['id']}.png") if args.check else (args.out / f"{scene['id']}.png")
            # A shot that never landed, or that caught bare sky, is the game
            # busy or between maps rather than the scene. Try again before
            # giving up: an unattended run is worth two more minutes.
            log = None
            for attempt in range(3):
                raw.unlink(missing_ok=True)
                log = shoot(args.bin, cache, scene, scene.get("secs", args.secs), raw)
                if log is None:
                    continue
                finish(raw, target, args.colours)
                if not empty(target):
                    break
                print(f"  {scene['id']}: no map in the frame, shooting again")
                log = None
            if log is None:
                failed.append(scene["id"])
                continue
            found = SCENE_LINE.search(log)
            if found:
                world, when = found["world"], found["date"]
            if args.check:
                committed = args.out / f"{scene['id']}.png"
                if not committed.exists():
                    print(f"  {scene['id']}: nothing committed to compare with")
                    failed.append(scene["id"])
                    continue
                mad = difference(committed, target)
                drift.append((scene["id"], mad))

        if args.check:
            print("\nmean absolute pixel difference against docs/gallery:")
            for name, mad in drift:
                verdict = "ok" if mad <= args.threshold else "DRIFTED"
                print(f"  {name:20s} {mad:6.2f}  {verdict}")
            over = [n for n, m in drift if m > args.threshold]
            if failed or over:
                sys.exit(f"showcase-check failed (threshold {args.threshold})")
            print(f"\nall {len(drift)} baseline(s) within {args.threshold}")
            return

    if failed:
        print(f"\nfailed: {', '.join(failed)}", file=sys.stderr)
    readme = args.out / "README.md"
    # A partial re-shoot still writes the whole page, from every scene in the
    # list that has an image; the header keeps what the last full run recorded.
    world, when = remembered(readme, world, when)
    readme.write_text(page(groups, listed, world, when, args.out, args.secs))
    total = sum(f.stat().st_size for f in args.out.glob("*.png"))
    print(f"\n{readme} written; {total / 1e6:.1f} MB of images")
    if failed:
        sys.exit(1)


if __name__ == "__main__":
    main()
