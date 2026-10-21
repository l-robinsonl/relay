#!/usr/bin/env python3
"""Draws Relay's application icon.

Two white chevrons on a rounded square in the accent blue. A double
chevron is the universal "forward on" mark, which is what the name means;
a single one would just be a shell prompt. The leading chevron is lighter
and smaller, so the pair reads as a hand-off rather than as decoration.

The mark lives in code rather than as a committed binary so that changing
it is a reviewable diff. Rendering uses 4x supersampling — without it the
diagonals and corner curves stair-step badly at 32px, which is the size
most people see most often.

    python3 scripts/render-icon.py
    npm run tauri icon "$(pwd)/apps/desktop/src-tauri/icons/icon.png"
"""

import math
import pathlib
import struct
import zlib

SUPERSAMPLE = 4

BACKGROUND = (59, 110, 246)
FOREGROUND = (255, 255, 255)

#: Sizes Tauri and the platform installers expect.
SIZES = {
    "32x32.png": 32,
    "128x128.png": 128,
    "128x128@2x.png": 256,
    "icon.png": 1024,
    "Square30x30Logo.png": 30,
    "Square44x44Logo.png": 44,
    "Square71x71Logo.png": 71,
    "Square89x89Logo.png": 89,
    "Square107x107Logo.png": 107,
    "Square142x142Logo.png": 142,
    "Square150x150Logo.png": 150,
    "Square284x284Logo.png": 284,
    "Square310x310Logo.png": 310,
    "StoreLogo.png": 50,
}


def rounded_box_sdf(px: float, py: float, half: float, radius: float) -> float:
    """Signed distance to a rounded square centred on the origin.

    Negative inside. Collapses to the first quadrant, measures against the
    inner box, then rounds the corner by subtracting the radius.
    """
    qx = abs(px) - (half - radius)
    qy = abs(py) - (half - radius)
    outside = math.hypot(max(qx, 0.0), max(qy, 0.0))
    inside = min(max(qx, qy), 0.0)
    return outside + inside - radius


def segment_distance(px, py, ax, ay, bx, by) -> float:
    """Distance from a point to a line segment."""
    vx, vy = bx - ax, by - ay
    wx, wy = px - ax, py - ay
    length_sq = vx * vx + vy * vy
    t = 0.0 if length_sq == 0 else max(0.0, min(1.0, (wx * vx + wy * vy) / length_sq))
    return math.hypot(px - (ax + t * vx), py - (ay + t * vy))


def chevron_distance(nx: float, ny: float, tip_x: float, height: float, reach: float) -> float:
    """Distance to one chevron pointing right, in unit space."""
    tip = (tip_x, 0.0)
    back = tip_x - reach
    return min(
        segment_distance(nx, ny, back, -height, *tip),
        segment_distance(nx, ny, back, height, *tip),
    )


def render(size: int) -> bytes:
    """Returns raw PNG scanlines for the icon at `size`."""
    big = size * SUPERSAMPLE
    half = big / 2
    # A 9% margin keeps the shape clear of the edge, where launchers crop.
    box_half = half * 0.91
    radius = box_half * 0.26

    # The trailing chevron carries the weight; the leading one is smaller
    # and thinner so the eye reads direction rather than a symmetric pair.
    # Shifted a little left of true centre: the pair's visual mass sits
    # behind the leading tip, so geometric centring reads as right-heavy.
    trailing = {"tip_x": -0.010, "height": 0.195, "reach": 0.195, "stroke": 0.053}
    leading = {"tip_x": 0.200, "height": 0.143, "reach": 0.143, "stroke": 0.042}

    out = bytearray()
    for y in range(size):
        out.append(0)  # PNG filter: none
        for x in range(size):
            r = g = b = a = 0
            for sy in range(SUPERSAMPLE):
                for sx in range(SUPERSAMPLE):
                    px = x * SUPERSAMPLE + sx + 0.5 - half
                    py = y * SUPERSAMPLE + sy + 0.5 - half

                    if rounded_box_sdf(px, py, box_half, radius) > 0:
                        continue

                    ux, uy = px / big, py / big
                    on_mark = (
                        chevron_distance(
                            ux, uy, trailing["tip_x"], trailing["height"], trailing["reach"]
                        )
                        <= trailing["stroke"]
                        or chevron_distance(
                            ux, uy, leading["tip_x"], leading["height"], leading["reach"]
                        )
                        <= leading["stroke"]
                    )

                    if on_mark:
                        r += FOREGROUND[0]
                        g += FOREGROUND[1]
                        b += FOREGROUND[2]
                    else:
                        r += BACKGROUND[0]
                        g += BACKGROUND[1]
                        b += BACKGROUND[2]
                    a += 255

            samples = SUPERSAMPLE * SUPERSAMPLE
            if a == 0:
                out.extend((0, 0, 0, 0))
            else:
                # Un-premultiply, so a partly covered edge pixel keeps its
                # colour instead of darkening towards black.
                covered = a / 255.0
                out.extend(
                    (
                        min(255, round(r / covered)),
                        min(255, round(g / covered)),
                        min(255, round(b / covered)),
                        round(a / samples),
                    )
                )
    return bytes(out)


def write_png(path: pathlib.Path, size: int) -> None:
    """Writes a minimal RGBA PNG, with no image library needed."""
    raw = render(size)

    def chunk(tag: bytes, data: bytes) -> bytes:
        body = tag + data
        return (
            struct.pack(">I", len(data))
            + body
            + struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF)
        )

    path.write_bytes(
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">2I5B", size, size, 8, 6, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )


def main() -> None:
    out = pathlib.Path(__file__).resolve().parent.parent / "apps/desktop/src-tauri/icons"
    out.mkdir(parents=True, exist_ok=True)

    for name, size in SIZES.items():
        write_png(out / name, size)

    print(f"rendered {len(SIZES)} icons into {out}")
    print("now run: npm run tauri icon \"$(pwd)/apps/desktop/src-tauri/icons/icon.png\"")


if __name__ == "__main__":
    main()
