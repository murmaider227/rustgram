"""Attribute a process's private committed pages to allocation call stacks.

Joins `mem_walk.py --out-json` with the dump written by the `memdiag` hook DLL (events.bin,
stacks.tsv, frames.tsv, install.txt, meta.txt) and prints MiB per (memory type, owner module, phase),
plus the private-bytes delta of each D3DKMT syscall (kernel-side commits made for the GPU driver).

Usage: python -I mem_attribution.py --dir <folder with walk.json and the memdiag dump> [--min-mib 0.5]
"""
import argparse
import bisect
import json
import struct
from collections import defaultdict
from pathlib import Path

_p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
_p.add_argument("--dir", type=Path, required=True)
_p.add_argument("--exe-name", default="rustgram.exe", help="Module whose frames count as application code")
_p.add_argument("--min-mib", type=float, default=0.5)
_args = _p.parse_args()
tag = _args.dir.name
min_mib = _args.min_mib
out = _args.dir
EXE = _args.exe_name.lower()
walk = json.load(open(out / "walk.json"))
MIB = 1048576.0
MEM_COMMIT = 0x1000

diag_ranges = []
for line in (out / "meta.txt").read_text().splitlines():
    if line.startswith(("events_table=", "stacks_table=")):
        a, b = line.split()
        base = int(a.split("=")[1], 16)
        diag_ranges.append((base, base + int(b.split("=")[1])))

stacks = {}
for line in (out / "stacks.tsv").read_text().splitlines():
    sid, fr = line.split("\t")
    stacks[int(sid)] = [int(x, 16) for x in fr.split(",") if x]
frames = {}
for line in (out / "frames.tsv").read_text(encoding="utf-8", errors="replace").splitlines():
    parts = line.split("\t")
    while len(parts) < 5:
        parts.append("")
    frames[int(parts[0], 16)] = (parts[1], parts[2], parts[3], parts[4])

SYS = {"ntdll.dll", "kernelbase.dll", "kernel32.dll", "memdiag.dll", "ucrtbase.dll", "vcruntime140.dll",
       "msvcrt.dll", "win32u.dll", "dxcore.dll", "gdi32.dll", "gdi32full.dll"}
RULES = [
    ("gpu_allocator::vulkan::MemoryBlock::new", "wgpu vulkan: gpu_allocator memory block"),
    ("gpu_allocator::d3d12", "wgpu dx12: gpu_allocator memory block"),
    ("wgpu_hal::gles", "wgpu GL (WGL) adapter probe/instance"),
    ("wgpu_hal::dx12::instance::impl$0::enumerate_adapters", "wgpu dx12 adapter probe (D3D12CreateDevice per adapter)"),
    ("wgpu_hal::dx12::Instance", "wgpu dx12 instance init"),
    ("wgpu_hal::dx12", "wgpu dx12 device/other"),
    ("create_swapchain", "wgpu vulkan: swapchain"),
    ("wgpu_hal::vulkan::swapchain", "wgpu vulkan: swapchain/present"),
    ("wgpu_hal::vulkan::Adapter::open", "wgpu vulkan: device open (vkCreateDevice)"),
    ("wgpu_hal::vulkan::instance::impl$4::enumerate_adapters", "wgpu vulkan: adapter enumerate"),
    ("enumerate_adapters", "wgpu: adapter enumerate (other)"),
    ("wgpu_hal::vulkan::Instance", "wgpu vulkan: instance init"),
    ("wgpu_hal::vulkan::instance", "wgpu vulkan: instance/surface"),
    ("request_device", "wgpu vulkan: device create (other)"),
    ("wgpu_hal::vulkan", "wgpu vulkan: other"),
    ("glutin", "glow/glutin GL context"),
    ("egui_glow", "egui_glow painter"),
    ("egui_wgpu", "egui_wgpu painter"),
    ("wgpu_core", "wgpu core (other)"),
    ("ffmpeg", "ffmpeg"),
    ("tg_", "app code (tg_* crates)"),
    ("eframe::native", "eframe/winit window setup"),
    ("winit::", "eframe/winit window setup"),
]


def describe(sid):
    if sid < 0 or sid not in stacks:
        return ("(kernel-side or before hook)", "-")
    owner_mod = ""
    chain = []
    for a in stacks[sid]:
        mod, rva, sym, loc = frames.get(a, ("", "", "", ""))
        low = mod.lower()
        if not owner_mod and low not in SYS:
            owner_mod = mod or "(non-image code)"
        if low == EXE:
            chain.append(sym)
    text = " | ".join(chain)
    phase = "no Rust frame (driver/system thread)" if not chain else "rust: other"
    for needle, label in RULES:
        if needle in text:
            phase = label
            break
    return (owner_mod or "(system only)", phase)


pages, page_cat, page_res = [], [], []
diag_commit = 0
for base, ab, size, state, prot, typ, resident, cat, rtag in walk["regions"]:
    if state != MEM_COMMIT or typ != 0x20000:
        continue
    if any(lo <= base < hi for lo, hi in diag_ranges):
        diag_commit += size
        continue
    frac = resident / size if size else 0
    for i in range(size // 4096):
        pages.append((base >> 12) + i)
        page_cat.append(cat.replace("private: ", ""))
        page_res.append(frac)
order = sorted(range(len(pages)), key=lambda i: pages[i])
pages = [pages[i] for i in order]
page_cat = [page_cat[i] for i in order]
page_res = [page_res[i] for i in order]
owner = [-1] * len(pages)

events = list(struct.iter_unpack("<IIIIQQII", (out / "events.bin").read_bytes()))
for kind, typ, prot, sid, base, size, tid, _pad in reversed(events):
    if kind >= 3 or not (typ & 0x1000):
        continue
    lo = bisect.bisect_left(pages, base >> 12)
    hi = bisect.bisect_left(pages, (base + size + 4095) >> 12)
    for i in range(lo, hi):
        if owner[i] == -1:
            owner[i] = sid

agg = defaultdict(lambda: [0.0, 0.0])
for i in range(len(pages)):
    mod, phase = describe(owner[i])
    k = (page_cat[i], mod, phase)
    agg[k][0] += 4096
    agg[k][1] += 4096 * page_res[i]

print(f"[{tag}] PrivateUsage {walk['private_usage']/MIB:.1f} MiB incl. diag tables {diag_commit/MIB:.1f} -> "
      f"net {(walk['private_usage']-diag_commit)/MIB:.1f} MiB; WorkingSet {walk['working_set']/MIB:.1f} MiB; events {len(events)}")
cats = walk["categories"]
print("memory types (commit / resident MiB):")
for name, c in sorted(cats.items(), key=lambda kv: -kv[1].get("commit", 0)):
    cm = c.get("commit", 0)
    if name == "private: other VirtualAlloc":
        cm -= diag_commit
    print(f"  {name:50s} {cm/MIB:8.1f} {c.get('resident',0)/MIB:8.1f}")
print(f"\n{'type':18s} {'owner module':30s} {'phase':58s} {'commit':>7s} {'resid':>7s}")
for (cat, mod, phase), (c, r) in sorted(agg.items(), key=lambda kv: -kv[1][0]):
    if c < min_mib * MIB:
        continue
    print(f"{cat[:18]:18s} {mod[:30]:30s} {phase[:58]:58s} {c/MIB:7.1f} {r/MIB:7.1f}")

inst = [l.split(":")[0] for l in (out / "install.txt").read_text().splitlines() if l.startswith("NtGdiDdDDI")]
kmt = defaultdict(lambda: [0, 0])
for kind, typ, prot, sid, base, size, tid, _pad in events:
    if kind >= 100:
        delta = size if size < (1 << 63) else size - (1 << 64)
        name = inst[kind - 100] if kind - 100 < len(inst) else str(kind)
        mod, phase = describe(sid)
        kmt[(name.replace("NtGdiDdDDI", "D3DKMT"), mod, phase)][0] += delta
        kmt[(name.replace("NtGdiDdDDI", "D3DKMT"), mod, phase)][1] += 1
if kmt:
    print(f"\nkernel-side commits (private-bytes delta across D3DKMT syscalls):")
    print(f"{'syscall':26s} {'owner module':18s} {'phase':58s} {'MiB':>7s} {'calls':>6s}")
    for (name, mod, phase), (d, n) in sorted(kmt.items(), key=lambda kv: -abs(kv[1][0])):
        if abs(d) < min_mib * MIB:
            continue
        print(f"{name[:26]:26s} {mod[:18]:18s} {phase[:58]:58s} {d/MIB:7.1f} {n:6d}")
