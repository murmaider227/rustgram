"""Generates the fixture's media files (photos, video, GIF, voice, round video, file, stickers).

Everything is synthetic (Pillow drawings, FFmpeg test sources), so no real photos or voices
end up on the test accounts.

    python -I make_media.py --out <dir> [--ffmpeg <ffmpeg.exe>]
"""

from __future__ import annotations

import argparse
import shutil
import subprocess
import sys
from pathlib import Path

from PIL import Image, ImageDraw


def gradient(w: int, h: int, a: tuple[int, int, int], b: tuple[int, int, int], label: str) -> Image.Image:
    img = Image.new("RGB", (w, h))
    px = img.load()
    for y in range(h):
        t = y / max(1, h - 1)
        row = tuple(int(a[i] + (b[i] - a[i]) * t) for i in range(3))
        for x in range(w):
            px[x, y] = row
    d = ImageDraw.Draw(img)
    for i in range(0, w + h, 80):
        d.line([(i, 0), (i - h, h)], fill=(255, 255, 255), width=3)
    d.rectangle([w // 10, h // 10, w // 10 + 18 * len(label) + 40, h // 10 + 60], fill=(0, 0, 0))
    d.text((w // 10 + 20, h // 10 + 20), label, fill=(255, 255, 255))
    return img


def sticker(color: tuple[int, int, int], shape: str) -> Image.Image:
    img = Image.new("RGBA", (512, 512), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    if shape == "circle":
        d.ellipse([40, 40, 472, 472], fill=color + (255,), outline=(255, 255, 255, 255), width=16)
        d.ellipse([150, 170, 210, 230], fill=(0, 0, 0, 255))
        d.ellipse([302, 170, 362, 230], fill=(0, 0, 0, 255))
        d.arc([140, 220, 372, 400], 20, 160, fill=(0, 0, 0, 255), width=18)
    else:
        d.polygon([(256, 30), (480, 470), (32, 470)], fill=color + (255,), outline=(255, 255, 255, 255))
    return img


def ffmpeg(exe: str, args: list[str]) -> None:
    subprocess.run([exe, "-y", "-loglevel", "error", *args], check=True)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True)
    ap.add_argument("--ffmpeg", default=shutil.which("ffmpeg") or "ffmpeg")
    a = ap.parse_args()
    out = Path(a.out)
    out.mkdir(parents=True, exist_ok=True)

    gradient(1280, 960, (30, 90, 160), (200, 220, 240), "album 1").save(out / "album1.jpg", quality=88)
    gradient(1280, 960, (160, 60, 30), (250, 210, 120), "album 2").save(out / "album2.jpg", quality=88)
    gradient(960, 1280, (40, 120, 60), (200, 240, 190), "album 3").save(out / "album3.jpg", quality=88)
    gradient(600, 1800, (80, 40, 120), (240, 200, 250), "tall photo").save(out / "tall.jpg", quality=88)
    gradient(1280, 720, (20, 20, 60), (90, 160, 220), "channel photo").save(out / "channel.jpg", quality=88)
    gradient(1080, 1920, (200, 80, 120), (250, 230, 160), "walk story").save(out / "story.jpg", quality=88)
    sticker((250, 200, 40), "circle").save(out / "sticker1.png")
    sticker((60, 170, 250), "triangle").save(out / "sticker2.png")
    (out / "walk-notes.txt").write_text(
        "Walk notes\n\n" + "\n".join(f"{i}. everyday scenario check" for i in range(1, 41)) + "\n",
        encoding="utf-8",
    )

    ffmpeg(a.ffmpeg, ["-f", "lavfi", "-i", "testsrc2=size=640x360:rate=30:duration=6",
                      "-f", "lavfi", "-i", "sine=frequency=440:duration=6",
                      "-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest",
                      "-movflags", "+faststart", str(out / "video.mp4")])
    ffmpeg(a.ffmpeg, ["-f", "lavfi", "-i", "mandelbrot=size=480x270:rate=25",
                      "-t", "3", "-an", "-c:v", "libx264", "-pix_fmt", "yuv420p",
                      "-movflags", "+faststart", str(out / "gif.mp4")])
    ffmpeg(a.ffmpeg, ["-f", "lavfi", "-i", "sine=frequency=330:duration=4",
                      "-c:a", "libopus", "-b:a", "32k", str(out / "voice.ogg")])
    ffmpeg(a.ffmpeg, ["-f", "lavfi", "-i", "testsrc=size=240x240:rate=30:duration=4",
                      "-f", "lavfi", "-i", "sine=frequency=550:duration=4",
                      "-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest",
                      "-movflags", "+faststart", str(out / "round.mp4")])
    for f in sorted(out.iterdir()):
        print(f"{f.name}\t{f.stat().st_size}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
