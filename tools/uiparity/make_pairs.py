"""Generate side-by-side Official vs Rustgram pair images and distance metrics.

Usage:
  python -I tools/uiparity/make_pairs.py --snap-dir <DIR>
  # or set UIPARITY_SNAP_DIR=<DIR>
"""

from __future__ import annotations

import argparse
import json
import math
import os
import re
import sys
from pathlib import Path
from typing import Any

from PIL import Image, ImageDraw, ImageFont


PAIR_RE = re.compile(r"^(\d{2})_(.+)_(1440x900|390x844)_(official|rustgram)\.png$")


def compute_metrics(img_a: Image.Image, img_b: Image.Image) -> dict[str, Any]:
    """Compute pixel/structural/histogram distance between two RGB images."""
    a = img_a.convert("RGB")
    b = img_b.convert("RGB")
    size_a = a.size
    size_b = b.size
    if b.size != a.size:
        b_cmp = b.resize(a.size, Image.Resampling.BILINEAR)
    else:
        b_cmp = b

    # Downsample to 360px wide for fast pure-Python block/pixel metrics
    scale_w = min(360, a.width)
    scale_h = max(1, int(round(a.height * (scale_w / a.width))))
    asml = a.resize((scale_w, scale_h), Image.Resampling.BILINEAR)
    bsml = b_cmp.resize((scale_w, scale_h), Image.Resampling.BILINEAR)

    pa = list(asml.get_flattened_data() if hasattr(asml, "get_flattened_data") else asml.getdata())
    pb = list(bsml.get_flattened_data() if hasattr(bsml, "get_flattened_data") else bsml.getdata())
    n = len(pa)

    abs_sum = 0.0
    sq_sum = 0.0
    diff_pixels = 0
    lum_a: list[float] = []
    lum_b: list[float] = []
    for (r1, g1, b1), (r2, g2, b2) in zip(pa, pb):
        dr = abs(r1 - r2)
        dg = abs(g1 - g2)
        db = abs(b1 - b2)
        m = (dr + dg + db) / 3.0
        abs_sum += m
        sq_sum += (dr * dr + dg * dg + db * db) / 3.0
        if m > 12.0:
            diff_pixels += 1
        lum_a.append(0.299 * r1 + 0.587 * g1 + 0.114 * b1)
        lum_b.append(0.299 * r2 + 0.587 * g2 + 0.114 * b2)

    mae = abs_sum / n
    rmse = math.sqrt(sq_sum / n)
    diff_ratio = diff_pixels / n

    # Global luminance SSIM approximation over 8x8 blocks
    bw, bh = 8, 8
    ssim_vals: list[float] = []
    c1 = (0.01 * 255.0) ** 2
    c2 = (0.03 * 255.0) ** 2
    for by in range(0, scale_h - bh + 1, bh):
        for bx in range(0, scale_w - bw + 1, bw):
            va: list[float] = []
            vb: list[float] = []
            for y in range(by, by + bh):
                row = y * scale_w
                for x in range(bx, bx + bw):
                    va.append(lum_a[row + x])
                    vb.append(lum_b[row + x])
            k = len(va)
            ma = sum(va) / k
            mb = sum(vb) / k
            vara = sum((x - ma) ** 2 for x in va) / k
            varb = sum((x - mb) ** 2 for x in vb) / k
            cov = sum((x - ma) * (y - mb) for x, y in zip(va, vb)) / k
            num = (2 * ma * mb + c1) * (2 * cov + c2)
            den = (ma * ma + mb * mb + c1) * (vara + varb + c2)
            ssim_vals.append(num / den if den > 0 else 1.0)
    ssim = sum(ssim_vals) / max(1, len(ssim_vals))

    # Color histogram L1 distance (normalized 0..1)
    ha = a.histogram()
    hb = b_cmp.histogram()
    tot_a = float(sum(ha)) or 1.0
    tot_b = float(sum(hb)) or 1.0
    hist_l1 = 0.5 * sum(abs(x / tot_a - y / tot_b) for x, y in zip(ha, hb))

    # Composite distance score (0 = identical, 100 = completely different)
    distance = round(
        0.40 * ((1.0 - max(0.0, min(1.0, ssim))) * 100.0)
        + 0.35 * (mae / 255.0 * 100.0)
        + 0.25 * (hist_l1 * 100.0),
        2,
    )

    return {
        "official_size": list(size_a),
        "rustgram_size": list(size_b),
        "mae": round(mae, 2),
        "rmse": round(rmse, 2),
        "diff_pixel_pct": round(diff_ratio * 100.0, 2),
        "ssim": round(ssim, 4),
        "hist_l1": round(hist_l1, 4),
        "distance": distance,
    }


def make_side_by_side(
    official_path: Path,
    rustgram_path: Path,
    out_path: Path,
    title: str,
) -> dict[str, Any]:
    a = Image.open(official_path).convert("RGB")
    b = Image.open(rustgram_path).convert("RGB")
    metrics = compute_metrics(a, b)

    header_h = 34
    divider_w = 4
    canvas_w = a.width + divider_w + b.width
    canvas_h = header_h + max(a.height, b.height)
    canvas = Image.new("RGB", (canvas_w, canvas_h), (28, 32, 38))
    draw = ImageDraw.Draw(canvas)

    try:
        font = ImageFont.truetype("segoeui.ttf", 15)
    except Exception:
        font = ImageFont.load_default()

    # Header bar
    draw.rectangle((0, 0, canvas_w, header_h), fill=(24, 28, 34))
    draw.text((12, 8), f"OFFICIAL  —  {title} ({a.width}x{a.height})", fill=(235, 240, 245), font=font)
    draw.text(
        (a.width + divider_w + 12, 8),
        f"RUSTGRAM  —  {title} ({b.width}x{b.height})   |   dist={metrics['distance']}  SSIM={metrics['ssim']}  MAE={metrics['mae']}",
        fill=(235, 240, 245),
        font=font,
    )
    # Divider
    draw.rectangle((a.width, 0, a.width + divider_w, canvas_h), fill=(220, 70, 70))

    canvas.paste(a, (0, header_h))
    canvas.paste(b, (a.width + divider_w, header_h))
    canvas.save(out_path)
    return metrics


def main() -> int:
    ap = argparse.ArgumentParser(description="Build side-by-side Official vs Rustgram pairs.")
    ap.add_argument("--snap-dir", default=os.environ.get("UIPARITY_SNAP_DIR", ""), help="Directory containing screenshots")
    ap.add_argument("--out-json", default="", help="Optional JSON metrics report output path")
    args = ap.parse_args()
    if not args.snap_dir:
        print("Pass --snap-dir or set UIPARITY_SNAP_DIR", file=sys.stderr)
        return 2

    snap_dir = Path(args.snap_dir)
    groups: dict[tuple[str, str, str], dict[str, Path]] = {}
    for p in sorted(snap_dir.glob("*.png")):
        m = PAIR_RE.match(p.name)
        if not m:
            continue
        nn, screen, size, side = m.groups()
        groups.setdefault((nn, screen, size), {})[side] = p

    results: list[dict[str, Any]] = []
    for (nn, screen, size), sides in sorted(groups.items()):
        if "official" not in sides or "rustgram" not in sides:
            missing = "rustgram" if "official" in sides else "official"
            print(f"[SKIP] {nn}_{screen}_{size}: missing {missing}")
            continue
        pair_path = snap_dir / f"{nn}_{screen}_{size}_pair.png"
        metrics = make_side_by_side(
            sides["official"],
            sides["rustgram"],
            pair_path,
            f"{nn} {screen} ({size})",
        )
        entry = {
            "id": nn,
            "screen": screen,
            "size": size,
            "official": str(sides["official"]),
            "rustgram": str(sides["rustgram"]),
            "pair": str(pair_path),
            **metrics,
        }
        results.append(entry)
        print(f"[PAIR] {pair_path.name}  dist={metrics['distance']:5.2f}  ssim={metrics['ssim']:.4f}  mae={metrics['mae']:5.2f}")

    ranked = sorted(results, key=lambda r: r["distance"], reverse=True)
    print("\n--- RANKED BY DISTANCE (WORST FIRST) ---")
    for idx, r in enumerate(ranked, 1):
        print(f"{idx:2d}. {r['id']}_{r['screen']}_{r['size']}: dist={r['distance']:5.2f} (SSIM={r['ssim']:.4f}, MAE={r['mae']:5.2f}, diff={r['diff_pixel_pct']:5.2f}%)")

    out_json = Path(args.out_json) if args.out_json else (snap_dir / "metrics.json")
    out_json.write_text(json.dumps({"pairs": results, "ranked": ranked}, indent=2), encoding="utf-8")
    print(f"\nWrote metrics to {out_json}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
