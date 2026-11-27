"""Turns a screenshot of tdesktop's styled login QR (rounded modules and finder patterns, logo in
the middle) into a plain black-and-white QR image a standard decoder reads.

tdesktop draws the code inside a white rounded box. We find the dark modules' bounding box, take
the module size from the top-left finder pattern (7 modules wide), round the module count to a
valid QR size (21 + 4k), sample every module centre, and draw square modules with a quiet zone.
The logo's modules come out wrong; the code's error correction absorbs them.
"""

from __future__ import annotations

import numpy as np
from PIL import Image


def clean_qr(img: Image.Image, scale: int = 8) -> Image.Image | None:
    g = np.asarray(img.convert("L"), dtype=np.int16)
    h, w = g.shape
    # The white box: the largest block of near-white pixels.
    white = g > 225
    # The box's white margins are rows and columns that are white almost end to end; the first
    # and last of them bound the box (text lines below it are never that white).
    rs, cs = white.sum(axis=1), white.sum(axis=0)
    rows = np.nonzero(rs > 0.8 * rs.max())[0]
    cols = np.nonzero(cs > 0.8 * cs.max())[0]
    if rows.size < 2 or cols.size < 2:
        return None
    by0, by1, bx0, bx1 = rows[0], rows[-1], cols[0], cols[-1]
    box = g[by0:by1 + 1, bx0:bx1 + 1]
    dark = box < 110
    # Ignore the dark page showing through the box's rounded corners.
    m = max(3, (bx1 - bx0) // 25)
    dark[:m, :] = False
    dark[-m:, :] = False
    dark[:, :m] = False
    dark[:, -m:] = False
    dr = np.nonzero(dark.sum(axis=1) > 0)[0]
    dc = np.nonzero(dark.sum(axis=0) > 0)[0]
    if dr.size == 0 or dc.size == 0:
        return None
    y0, y1, x0, x1 = dr[0], dr[-1], dc[0], dc[-1]
    size = min(x1 - x0 + 1, y1 - y0 + 1)
    # Width of the top-left finder pattern along a row through its middle: dark, light, dark
    # core, light, dark in a 1:1:3:1:1 ratio = 7 modules. Rounded corners do not matter there.
    module = 0.0
    for y in range(y0, y0 + size // 4):
        row = dark[y, x0:x1 + 1]
        runs: list[int] = []
        cur, n_ = bool(row[0]), 0
        for v in row:
            if bool(v) == cur:
                n_ += 1
            else:
                runs.append(n_)
                cur, n_ = bool(v), 1
            if len(runs) >= 5:
                break
        if len(runs) >= 5 and row[0]:
            d1, l1, core, l2, d2 = runs[:5]
            unit = (d1 + l1 + core + l2 + d2) / 7.0
            if all(abs(r - unit) < unit * 0.6 for r in (d1, l1, l2, d2)) and abs(core - 3 * unit) < unit * 1.2:
                module = max(module, unit)
    if module <= 0:
        return None
    n = round(size / module)
    n = 21 + 4 * max(0, round((n - 21) / 4))
    step = size / n
    out = np.zeros((n, n), dtype=bool)
    for i in range(n):
        for j in range(n):
            cy = int(y0 + (i + 0.5) * step)
            cx = int(x0 + (j + 0.5) * step)
            patch = box[max(cy - 1, 0):cy + 2, max(cx - 1, 0):cx + 2]
            out[i, j] = patch.mean() < 128
    # tdesktop rounds the finder patterns, so their corner modules sample as light: redraw the
    # fixed parts of every QR (three finders with separators, timing lines) exactly.
    def finder(r0: int, c0: int) -> None:
        for i in range(-1, 8):
            for j in range(-1, 8):
                r, c = r0 + i, c0 + j
                if 0 <= r < n and 0 <= c < n:
                    ring = i in (0, 6) or j in (0, 6)
                    core = 2 <= i <= 4 and 2 <= j <= 4
                    out[r, c] = (0 <= i <= 6 and 0 <= j <= 6) and (ring or core)

    finder(0, 0)
    finder(0, n - 7)
    finder(n - 7, 0)
    for k in range(8, n - 8):
        out[6, k] = out[k, 6] = k % 2 == 0
    q = 4
    full = np.ones((n + 2 * q, n + 2 * q), dtype=np.uint8) * 255
    full[q:q + n, q:q + n] = np.where(out, 0, 255)
    return Image.fromarray(full, "L").resize(((n + 2 * q) * scale, (n + 2 * q) * scale), Image.NEAREST)
