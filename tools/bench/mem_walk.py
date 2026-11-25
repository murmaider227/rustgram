"""VMMap-style breakdown of a running process's committed memory (read-only, Windows x64).

Walks the address space with VirtualQueryEx and classifies every committed region:
thread stacks (via each thread's TEB), NT heaps (segments and large "virtual" blocks read from the
PEB heap list), image pages per module, mapped views (file-backed vs pagefile-backed) and the
remaining private VirtualAlloc regions. QueryWorkingSetEx adds the resident part of each class.

The target is only read (PROCESS_QUERY_INFORMATION | PROCESS_VM_READ); nothing is written to it.

Usage: python -I mem_walk.py --pid <pid> [--out-json <file>] [--top 40]
"""

from __future__ import annotations

import argparse
import ctypes
import json
import sys
from collections import defaultdict
from ctypes import wintypes as wt
from typing import Any

k32 = ctypes.WinDLL("kernel32", use_last_error=True)
ntdll = ctypes.WinDLL("ntdll")
psapi = ctypes.WinDLL("psapi", use_last_error=True)

PROCESS_QUERY_INFORMATION = 0x0400
PROCESS_VM_READ = 0x0010
THREAD_QUERY_INFORMATION = 0x0040
MEM_COMMIT = 0x1000
MEM_RESERVE = 0x2000
MEM_PRIVATE = 0x20000
MEM_MAPPED = 0x40000
MEM_IMAGE = 0x1000000
PAGE = 4096
MIB = 1048576.0
WRITABLE = {0x04, 0x08, 0x40, 0x80}  # READWRITE, WRITECOPY, EXECUTE_READWRITE, EXECUTE_WRITECOPY
EXECUTABLE = {0x10, 0x20, 0x40, 0x80}


class MBI(ctypes.Structure):
    _fields_ = [
        ("BaseAddress", ctypes.c_ulonglong),
        ("AllocationBase", ctypes.c_ulonglong),
        ("AllocationProtect", wt.DWORD),
        ("PartitionId", wt.WORD),
        ("RegionSize", ctypes.c_ulonglong),
        ("State", wt.DWORD),
        ("Protect", wt.DWORD),
        ("Type", wt.DWORD),
    ]


class PMC_EX(ctypes.Structure):
    _fields_ = [
        ("cb", wt.DWORD),
        ("PageFaultCount", wt.DWORD),
        ("PeakWorkingSetSize", ctypes.c_size_t),
        ("WorkingSetSize", ctypes.c_size_t),
        ("QuotaPeakPagedPoolUsage", ctypes.c_size_t),
        ("QuotaPagedPoolUsage", ctypes.c_size_t),
        ("QuotaPeakNonPagedPoolUsage", ctypes.c_size_t),
        ("QuotaNonPagedPoolUsage", ctypes.c_size_t),
        ("PagefileUsage", ctypes.c_size_t),
        ("PeakPagefileUsage", ctypes.c_size_t),
        ("PrivateUsage", ctypes.c_size_t),
    ]


class THREADENTRY32(ctypes.Structure):
    _fields_ = [
        ("dwSize", wt.DWORD),
        ("cntUsage", wt.DWORD),
        ("th32ThreadID", wt.DWORD),
        ("th32OwnerProcessID", wt.DWORD),
        ("tpBasePri", wt.LONG),
        ("tpDeltaPri", wt.LONG),
        ("dwFlags", wt.DWORD),
    ]


class WSEX(ctypes.Structure):
    _fields_ = [("VirtualAddress", ctypes.c_ulonglong), ("VirtualAttributes", ctypes.c_ulonglong)]


k32.OpenProcess.restype = wt.HANDLE
k32.OpenProcess.argtypes = [wt.DWORD, wt.BOOL, wt.DWORD]
k32.OpenThread.restype = wt.HANDLE
k32.OpenThread.argtypes = [wt.DWORD, wt.BOOL, wt.DWORD]
k32.CloseHandle.argtypes = [wt.HANDLE]
k32.VirtualQueryEx.restype = ctypes.c_size_t
k32.VirtualQueryEx.argtypes = [wt.HANDLE, ctypes.c_void_p, ctypes.POINTER(MBI), ctypes.c_size_t]
k32.ReadProcessMemory.argtypes = [
    wt.HANDLE,
    ctypes.c_void_p,
    ctypes.c_void_p,
    ctypes.c_size_t,
    ctypes.POINTER(ctypes.c_size_t),
]
k32.CreateToolhelp32Snapshot.restype = wt.HANDLE
k32.CreateToolhelp32Snapshot.argtypes = [wt.DWORD, wt.DWORD]
k32.Thread32First.argtypes = [wt.HANDLE, ctypes.POINTER(THREADENTRY32)]
k32.Thread32Next.argtypes = [wt.HANDLE, ctypes.POINTER(THREADENTRY32)]
k32.LocalFree.argtypes = [ctypes.c_void_p]
psapi.GetMappedFileNameW.argtypes = [wt.HANDLE, ctypes.c_void_p, wt.LPWSTR, wt.DWORD]
psapi.GetProcessMemoryInfo.argtypes = [wt.HANDLE, ctypes.POINTER(PMC_EX), wt.DWORD]
psapi.QueryWorkingSetEx.argtypes = [wt.HANDLE, ctypes.c_void_p, wt.DWORD]
ntdll.NtQueryInformationProcess.argtypes = [
    wt.HANDLE,
    ctypes.c_ulong,
    ctypes.c_void_p,
    ctypes.c_ulong,
    ctypes.POINTER(ctypes.c_ulong),
]
ntdll.NtQueryInformationThread.argtypes = [
    wt.HANDLE,
    ctypes.c_ulong,
    ctypes.c_void_p,
    ctypes.c_ulong,
    ctypes.POINTER(ctypes.c_ulong),
]


def read_mem(h: int, addr: int, size: int) -> bytes | None:
    buf = ctypes.create_string_buffer(size)
    got = ctypes.c_size_t(0)
    if not k32.ReadProcessMemory(h, ctypes.c_void_p(addr), buf, size, ctypes.byref(got)):
        return None
    return buf.raw[: got.value]


def read_u64(h: int, addr: int) -> int | None:
    b = read_mem(h, addr, 8)
    return int.from_bytes(b, "little") if b and len(b) == 8 else None


def read_u32(h: int, addr: int) -> int | None:
    b = read_mem(h, addr, 4)
    return int.from_bytes(b, "little") if b and len(b) == 4 else None


def walk_regions(h: int) -> list[dict[str, Any]]:
    regions = []
    addr = 0
    mbi = MBI()
    while addr < 0x7FFFFFFF0000:
        if not k32.VirtualQueryEx(h, ctypes.c_void_p(addr), ctypes.byref(mbi), ctypes.sizeof(mbi)):
            break
        if mbi.State in (MEM_COMMIT, MEM_RESERVE):
            regions.append(
                {
                    "base": mbi.BaseAddress,
                    "alloc_base": mbi.AllocationBase,
                    "size": mbi.RegionSize,
                    "state": mbi.State,
                    "protect": mbi.Protect & 0xFF,
                    "guard": bool(mbi.Protect & 0x100),
                    "type": mbi.Type,
                }
            )
        addr = mbi.BaseAddress + mbi.RegionSize
    return regions


def mapped_name(h: int, addr: int) -> str:
    buf = ctypes.create_unicode_buffer(1024)
    n = psapi.GetMappedFileNameW(h, ctypes.c_void_p(addr), buf, 1024)
    if not n:
        return ""
    return buf.value.replace("/", "\\").rsplit("\\", 1)[-1]


def list_threads(h: int, pid: int) -> list[dict[str, Any]]:
    out = []
    snap = k32.CreateToolhelp32Snapshot(0x4, 0)
    te = THREADENTRY32()
    te.dwSize = ctypes.sizeof(te)
    ok = k32.Thread32First(snap, ctypes.byref(te))
    tids = []
    while ok:
        if te.th32OwnerProcessID == pid:
            tids.append(te.th32ThreadID)
        ok = k32.Thread32Next(snap, ctypes.byref(te))
    k32.CloseHandle(snap)
    for tid in tids:
        ht = k32.OpenThread(THREAD_QUERY_INFORMATION, False, tid)
        if not ht:
            continue
        tbi = (ctypes.c_ulonglong * 6)()
        info: dict[str, Any] = {"tid": tid}
        if ntdll.NtQueryInformationThread(ht, 0, tbi, 48, None) == 0:
            teb = tbi[1]
            info["teb"] = teb
            info["stack_base"] = read_u64(h, teb + 0x08)
            info["stack_limit"] = read_u64(h, teb + 0x10)
            info["dealloc_stack"] = read_u64(h, teb + 0x1478)
        start = ctypes.c_ulonglong(0)
        if ntdll.NtQueryInformationThread(ht, 9, ctypes.byref(start), 8, None) == 0:
            info["start_address"] = start.value
        desc = ctypes.c_void_p()
        try:
            if k32.GetThreadDescription(wt.HANDLE(ht), ctypes.byref(desc)) >= 0 and desc.value:
                info["name"] = ctypes.wstring_at(desc.value)
                k32.LocalFree(desc)
        except (AttributeError, OSError):
            pass
        k32.CloseHandle(ht)
        out.append(info)
    return out


def list_heaps(h: int) -> list[dict[str, Any]]:
    """Read the PEB heap list; for NT heaps also the segment list and large virtual blocks."""
    pbi = (ctypes.c_ulonglong * 6)()
    if ntdll.NtQueryInformationProcess(h, 0, pbi, 48, None) != 0:
        return []
    peb = pbi[1]
    default_heap = read_u64(h, peb + 0x30)
    count = read_u32(h, peb + 0xE8) or 0
    arr = read_u64(h, peb + 0xF0) or 0
    heaps = []
    for i in range(min(count, 512)):
        base = read_u64(h, arr + 8 * i)
        if not base:
            continue
        sig = read_u32(h, base + 0x10)
        heap: dict[str, Any] = {
            "index": i,
            "base": base,
            "default": base == default_heap,
            "kind": {0xFFEEFFEE: "nt", 0xDDEEDDEE: "segment"}.get(sig or 0, "unknown"),
            "segments": [],
            "virtual_blocks": [],
        }
        if heap["kind"] == "nt":
            head = base + 0x120
            cur = read_u64(h, head)
            guard = 0
            while cur and cur != head and guard < 4096:
                seg = cur - 0x18
                if read_u32(h, seg + 0x10) != 0xFFEEFFEE:
                    break
                seg_base = read_u64(h, seg + 0x30) or seg
                pages = read_u32(h, seg + 0x38) or 0
                heap["segments"].append({"base": seg_base, "size": pages * PAGE})
                cur = read_u64(h, cur)
                guard += 1
            head = base + 0x110
            cur = read_u64(h, head)
            guard = 0
            while cur and cur != head and guard < 65536:
                commit = read_u64(h, cur + 0x20) or 0
                reserve = read_u64(h, cur + 0x28) or 0
                heap["virtual_blocks"].append({"base": cur, "commit": commit, "reserve": reserve})
                cur = read_u64(h, cur)
                guard += 1
        heaps.append(heap)
    return heaps


def query_resident(h: int, regions: list[dict[str, Any]]) -> None:
    """Fill region['resident'] and region['resident_private'] (bytes) via QueryWorkingSetEx."""
    batch = 32768
    arr = (WSEX * batch)()
    pending: list[tuple[dict[str, Any], int]] = []

    def flush() -> None:
        n = len(pending)
        if not n:
            return
        for i, (_reg, va) in enumerate(pending):
            arr[i].VirtualAddress = va
            arr[i].VirtualAttributes = 0
        if psapi.QueryWorkingSetEx(h, arr, n * ctypes.sizeof(WSEX)):
            for i, (reg, _va) in enumerate(pending):
                attrs = arr[i].VirtualAttributes
                if attrs & 1:
                    reg["resident"] += PAGE
                    if not (attrs >> 15) & 1:
                        reg["resident_private"] += PAGE
        pending.clear()

    for reg in regions:
        reg["resident"] = 0
        reg["resident_private"] = 0
        if reg["state"] != MEM_COMMIT:
            continue
        for va in range(reg["base"], reg["base"] + reg["size"], PAGE):
            pending.append((reg, va))
            if len(pending) == batch:
                flush()
    flush()


def analyze(pid: int, top: int) -> dict[str, Any]:
    h = k32.OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, False, pid)
    if not h:
        raise OSError(f"OpenProcess({pid}) failed: {ctypes.get_last_error()}")
    try:
        pmc = PMC_EX()
        pmc.cb = ctypes.sizeof(pmc)
        psapi.GetProcessMemoryInfo(h, ctypes.byref(pmc), pmc.cb)
        regions = walk_regions(h)
        threads = list_threads(h, pid)
        heaps = list_heaps(h)
        query_resident(h, regions)

        allocs: dict[int, list[dict[str, Any]]] = defaultdict(list)
        for reg in regions:
            allocs[reg["alloc_base"]].append(reg)

        names: dict[int, str] = {}
        for ab, regs in allocs.items():
            if regs[0]["type"] in (MEM_IMAGE, MEM_MAPPED):
                names[ab] = mapped_name(h, regs[0]["base"])

        image_ranges = sorted(
            (ab, ab + sum(r["size"] for r in regs), names.get(ab, "?"))
            for ab, regs in allocs.items()
            if regs[0]["type"] == MEM_IMAGE
        )

        def module_of(addr: int | None) -> str:
            if not addr:
                return "?"
            for lo, hi, name in image_ranges:
                if lo <= addr < hi:
                    return f"{name}+0x{addr - lo:x}"
            return f"0x{addr:x}"

        def alloc_base_of(addr: int | None) -> int | None:
            if not addr:
                return None
            for reg in regions:
                if reg["base"] <= addr < reg["base"] + reg["size"]:
                    return reg["alloc_base"]
            return None

        tags: dict[int, str] = {}
        for t in threads:
            ab = alloc_base_of(t.get("dealloc_stack")) or alloc_base_of(t.get("stack_limit"))
            if ab is not None:
                tags[ab] = f"stack:{t['tid']}"
                t["stack_alloc_base"] = ab
            teb_ab = alloc_base_of(t.get("teb"))
            if teb_ab is not None and teb_ab not in tags:
                tags[teb_ab] = "teb"
        for hp in heaps:
            label = f"heap:{hp['index']}" + (":default" if hp["default"] else "")
            ab = alloc_base_of(hp["base"])
            if ab is not None:
                tags.setdefault(ab, label)
            for seg in hp["segments"]:
                ab = alloc_base_of(seg["base"])
                if ab is not None:
                    tags.setdefault(ab, label)
            for vb in hp["virtual_blocks"]:
                ab = alloc_base_of(vb["base"])
                if ab is not None:
                    tags.setdefault(ab, label + ":large")

        cats: dict[str, dict[str, float]] = defaultdict(lambda: defaultdict(float))
        image_mods: dict[str, dict[str, float]] = defaultdict(lambda: defaultdict(float))
        heap_stats: dict[str, dict[str, float]] = defaultdict(lambda: defaultdict(float))
        other_private = []
        mapped_views = []
        private_estimate = 0

        for ab, regs in allocs.items():
            typ = regs[0]["type"]
            commit = sum(r["size"] for r in regs if r["state"] == MEM_COMMIT)
            reserve = sum(r["size"] for r in regs)
            resident = sum(r["resident"] for r in regs)
            res_priv = sum(r["resident_private"] for r in regs)
            writable = sum(
                r["size"] for r in regs if r["state"] == MEM_COMMIT and r["protect"] in WRITABLE
            )
            execb = sum(
                r["size"] for r in regs if r["state"] == MEM_COMMIT and r["protect"] in EXECUTABLE
            )
            tag = tags.get(ab, "")
            if typ == MEM_PRIVATE:
                private_estimate += commit
                if tag.startswith("stack"):
                    cat = "private: thread stacks"
                elif tag.startswith("heap"):
                    cat = "private: heaps"
                    hs = heap_stats[tag.replace(":large", "")]
                    hs["commit"] += commit
                    hs["reserve"] += reserve
                    hs["resident"] += resident
                    if tag.endswith(":large"):
                        hs["large_commit"] += commit
                        hs["large_blocks"] += 1
                elif tag == "teb":
                    cat = "private: TEB/PEB"
                else:
                    cat = "private: other VirtualAlloc"
                    other_private.append(
                        {
                            "alloc_base": ab,
                            "commit": commit,
                            "reserve": reserve,
                            "resident": resident,
                            "writable": writable,
                            "executable": execb,
                            "regions": len(regs),
                        }
                    )
            elif typ == MEM_IMAGE:
                cat = "image"
                name = names.get(ab, "?")
                # Writable image pages are copy-on-write: they are charged to the process commit.
                private_estimate += writable
                m = image_mods[name]
                m["commit"] += commit
                m["writable"] += writable
                m["resident"] += resident
                m["resident_private"] += res_priv
                cats["image: writable (charged to private bytes)"]["commit"] += writable
            else:
                name = names.get(ab, "")
                cat = "mapped: file-backed" if name else "mapped: pagefile-backed (shared section)"
                mapped_views.append(
                    {"alloc_base": ab, "name": name, "commit": commit, "resident": resident}
                )
            for r in regs:
                r["category"] = cat
                r["tag"] = tag or names.get(ab, "")
            c = cats[cat]
            c["commit"] += commit
            c["reserve"] += reserve
            c["resident"] += resident
            c["resident_private"] += res_priv
            c["allocations"] += 1

        thread_rows = []
        for t in threads:
            ab = t.get("stack_alloc_base")
            regs = allocs.get(ab, []) if ab is not None else []
            thread_rows.append(
                {
                    "tid": t["tid"],
                    "name": t.get("name", ""),
                    "start": module_of(t.get("start_address")),
                    "stack_reserve": sum(r["size"] for r in regs),
                    "stack_commit": sum(r["size"] for r in regs if r["state"] == MEM_COMMIT),
                    "stack_resident": sum(r["resident"] for r in regs),
                }
            )

        other_private.sort(key=lambda r: -r["commit"])
        mapped_views.sort(key=lambda r: -r["commit"])
        return {
            "pid": pid,
            "private_usage": pmc.PrivateUsage,
            "working_set": pmc.WorkingSetSize,
            "private_estimate": private_estimate,
            "categories": {k: dict(v) for k, v in cats.items()},
            "heaps": [
                {
                    "index": hp["index"],
                    "base": hp["base"],
                    "default": hp["default"],
                    "kind": hp["kind"],
                    "segments": len(hp["segments"]),
                    "virtual_blocks": len(hp["virtual_blocks"]),
                    **{
                        k: v
                        for k, v in heap_stats.get(
                            f"heap:{hp['index']}" + (":default" if hp["default"] else ""), {}
                        ).items()
                    },
                }
                for hp in heaps
            ],
            "threads": thread_rows,
            "image_modules": {k: dict(v) for k, v in image_mods.items()},
            "other_private": other_private[: max(top, 0) or None],
            "other_private_count": len(other_private),
            "mapped_views": mapped_views[: max(top, 0) or None],
            "regions": [
                [
                    r["base"],
                    r["alloc_base"],
                    r["size"],
                    r["state"],
                    r["protect"],
                    r["type"],
                    r["resident"],
                    r.get("category", ""),
                    r.get("tag", ""),
                ]
                for r in regions
            ],
        }
    finally:
        k32.CloseHandle(h)


def print_report(d: dict[str, Any], top: int) -> None:
    def mib(v: float) -> str:
        return f"{v / MIB:9.2f}"

    print(f"pid {d['pid']}: PrivateUsage {mib(d['private_usage'])} MiB, WorkingSet {mib(d['working_set'])} MiB")
    print(
        f"walk estimate of private bytes (private commit + writable image) {mib(d['private_estimate'])} MiB, "
        f"residual {mib(d['private_usage'] - d['private_estimate'])} MiB"
    )
    print(f"\n{'category':52s} {'commit':>9s} {'resident':>9s} {'reserve':>10s} {'allocs':>7s}")
    for name, c in sorted(d["categories"].items(), key=lambda kv: -kv[1].get("commit", 0)):
        print(
            f"{name:52s} {mib(c.get('commit', 0))} {mib(c.get('resident', 0))} "
            f"{c.get('reserve', 0) / MIB:10.2f} {int(c.get('allocations', 0)):7d}"
        )
    print("\nheaps (index, kind, commit MiB, resident MiB, large-block commit MiB / count):")
    for hp in sorted(d["heaps"], key=lambda x: -x.get("commit", 0)):
        print(
            f"  #{hp['index']:<3d} {hp['kind']:8s} {'default' if hp['default'] else '':8s} "
            f"{mib(hp.get('commit', 0))} {mib(hp.get('resident', 0))} "
            f"{mib(hp.get('large_commit', 0))} / {int(hp.get('large_blocks', 0))}  base=0x{hp['base']:x}"
        )
    print("\nthreads (tid, stack commit KiB / reserve MiB, start, name):")
    for t in sorted(d["threads"], key=lambda x: -x["stack_commit"]):
        print(
            f"  {t['tid']:6d} {t['stack_commit'] / 1024:8.0f} / {t['stack_reserve'] / MIB:6.2f}  "
            f"{t['start']:44s} {t['name']}"
        )
    print(f"\ntop image modules by writable (private-charged) bytes:")
    mods = sorted(d["image_modules"].items(), key=lambda kv: -kv[1].get("writable", 0))
    for name, m in mods[:top]:
        print(
            f"  {name:40s} writable {mib(m.get('writable', 0))}  image {mib(m.get('commit', 0))}  "
            f"resident {mib(m.get('resident', 0))}  resident-private {mib(m.get('resident_private', 0))}"
        )
    print(f"\ntop other private VirtualAlloc allocations ({d['other_private_count']} total):")
    for r in d["other_private"][:top]:
        print(
            f"  0x{r['alloc_base']:014x} commit {mib(r['commit'])} resident {mib(r['resident'])} "
            f"reserve {r['reserve'] / MIB:9.2f} exec {mib(r['executable'])} regions {r['regions']}"
        )
    print("\ntop mapped views:")
    for r in d["mapped_views"][: min(top, 15)]:
        print(
            f"  0x{r['alloc_base']:014x} commit {mib(r['commit'])} resident {mib(r['resident'])} "
            f"{r['name'] or '(pagefile-backed)'}"
        )


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--pid", type=int, required=True)
    p.add_argument("--out-json", default=None)
    p.add_argument("--top", type=int, default=40)
    args = p.parse_args()
    data = analyze(args.pid, args.top)
    print_report(data, args.top)
    if args.out_json:
        with open(args.out_json, "w", encoding="utf-8") as f:
            json.dump(data, f)
    return 0


if __name__ == "__main__":
    sys.exit(main())
