//! In-process memory diagnosis DLL for the benchmark harness (Windows x64, measurement builds only).
//!
//! Load it at the very start of `main` (`LoadLibraryW` + call the exported `memdiag_start`) with
//! `RG_MEMDIAG_OUT=<folder>` set; create `<folder>/va.trigger` to make it dump. See `../README.md`.
//!
//! Hooks ntdll!NtAllocateVirtualMemory(/Ex), NtFreeVirtualMemory and the win32u D3DKMT syscalls,
//! records calls with their call stacks, and on a trigger file dumps the event log, symbolized
//! frames and a HeapWalk summary.
#![allow(unsafe_op_in_unsafe_fn, static_mut_refs, clippy::missing_safety_doc)]

use core::ffi::c_void;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::PathBuf;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

type Handle = *mut c_void;

#[repr(C)]
struct Mbi {
    base: *mut c_void,
    alloc_base: *mut c_void,
    alloc_protect: u32,
    partition: u16,
    region_size: usize,
    state: u32,
    protect: u32,
    typ: u32,
}

#[repr(C)]
struct HeapEntry {
    data: *mut c_void,
    cb_data: u32,
    cb_overhead: u8,
    region_index: u8,
    flags: u16,
    u0: u32,
    u1: u32,
    u2: *mut c_void,
    u3: *mut c_void,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetModuleHandleA(name: *const u8) -> Handle;
    fn LoadLibraryA(name: *const u8) -> Handle;
    fn GetProcAddress(h: Handle, name: *const u8) -> *mut c_void;
    fn VirtualAlloc(addr: *mut c_void, size: usize, typ: u32, prot: u32) -> *mut c_void;
    fn VirtualProtect(addr: *mut c_void, size: usize, newp: u32, old: *mut u32) -> i32;
    fn VirtualQuery(addr: *const c_void, mbi: *mut Mbi, len: usize) -> usize;
    fn FlushInstructionCache(h: Handle, addr: *const c_void, size: usize) -> i32;
    fn GetCurrentProcess() -> Handle;
    fn GetCurrentThreadId() -> u32;
    fn GetCurrentProcessId() -> u32;
    fn GetProcessId(h: Handle) -> u32;
    fn GetProcessHeaps(n: u32, heaps: *mut Handle) -> u32;
    fn GetProcessHeap() -> Handle;
    fn HeapWalk(h: Handle, entry: *mut HeapEntry) -> i32;
    fn HeapLock(h: Handle) -> i32;
    fn HeapUnlock(h: Handle) -> i32;
    fn GetModuleFileNameW(h: Handle, buf: *mut u16, n: u32) -> u32;
}

#[link(name = "ntdll")]
unsafe extern "system" {
    fn RtlCaptureStackBackTrace(
        skip: u32,
        count: u32,
        frames: *mut *mut c_void,
        hash: *mut u32,
    ) -> u16;
    fn NtQueryInformationProcess(
        h: Handle,
        class: u32,
        buf: *mut c_void,
        len: u32,
        ret: *mut u32,
    ) -> i32;
}

const NFRAMES: usize = 40;
const NSTACKS: usize = 1 << 16;
const MAXEV: usize = 1 << 21;

/// win32u syscalls that take one pointer argument (the D3DKMT_* struct).
const KMT_NAMES: &[&str] = &[
    "NtGdiDdDDICreateAllocation",
    "NtGdiDdDDILock",
    "NtGdiDdDDILock2",
    "NtGdiDdDDIEscape",
    "NtGdiDdDDICreateDevice",
    "NtGdiDdDDICreateContext",
    "NtGdiDdDDICreateContextVirtual",
    "NtGdiDdDDIOpenAdapterFromLuid",
    "NtGdiDdDDIOpenAdapterFromHdc",
    "NtGdiDdDDIOpenAdapterFromDeviceName",
    "NtGdiDdDDIEnumAdapters2",
    "NtGdiDdDDIEnumAdapters3",
    "NtGdiDdDDIMapGpuVirtualAddress",
    "NtGdiDdDDIReserveGpuVirtualAddress",
    "NtGdiDdDDIMakeResident",
    "NtGdiDdDDICreatePagingQueue",
    "NtGdiDdDDICreateHwQueue",
    "NtGdiDdDDICreateHwContext",
    "NtGdiDdDDIQueryAdapterInfo",
    "NtGdiDdDDIOpenResource",
    "NtGdiDdDDIOpenResourceFromNtHandle",
    "NtGdiDdDDICreateSynchronizationObject",
    "NtGdiDdDDIPresent",
    "NtGdiDdDDISubmitCommand",
    "NtGdiDdDDIRender",
    "NtGdiDdDDIUpdateGpuVirtualAddress",
    "NtGdiDdDDISubmitCommandToHwQueue",
    "NtGdiDdDDIQueryResourceInfo",
    "NtGdiDdDDIDestroyAllocation2",
    "NtGdiDdDDIDestroyAllocation",
    "NtGdiDdDDIUnlock2",
    "NtGdiDdDDIUnlock",
    "NtGdiDdDDIDestroyDevice",
    "NtGdiDdDDIDestroyContext",
    "NtGdiDdDDICloseAdapter",
    "NtGdiDdDDIFreeGpuVirtualAddress",
    "NtGdiDdDDIEvict",
    "NtGdiDdDDIOfferAllocations",
    "NtGdiDdDDIReclaimAllocations2",
    "NtGdiDdDDIOpenSyncObjectFromNtHandle2",
    "NtGdiDdDDICreateKeyedMutex2",
    "NtGdiDdDDIWaitForSynchronizationObjectFromCpu",
    "NtGdiDdDDISetVidPnSourceOwner",
    "NtGdiDdDDIGetDeviceState",
    "NtGdiDdDDIQueryVideoMemoryInfo",
    "NtGdiDdDDIPresentMultiPlaneOverlay3",
    "NtGdiDdDDIOpenKeyedMutex2",
    "NtGdiDdDDISetAllocationPriority",
    "NtGdiDdDDIInvalidateCache",
    "NtGdiDdDDIRegisterTrimNotification",
];

#[repr(C)]
#[derive(Clone, Copy)]
struct Event {
    kind: u32, // 1 alloc, 2 alloc_ex, 3 free, 100+id D3DKMT call with a private-bytes delta
    typ: u32,
    prot: u32,
    stack: u32,
    base: u64,
    size: u64,
    tid: u32,
    pad: u32,
}

#[repr(C)]
struct Stack {
    hash: u32,
    n: u32,
    frames: [usize; NFRAMES],
}

static mut EVENTS: *mut Event = null_mut();
static mut STACKS: *mut Stack = null_mut();
static mut TRAMP: *mut u8 = null_mut();
static mut TRAMP_W32: *mut u8 = null_mut();
static mut ORIG_ALLOC: usize = 0;
static mut ORIG_ALLOC_EX: usize = 0;
static mut ORIG_FREE: usize = 0;
static mut ORIG_KMT: [usize; 64] = [0; 64];
static KMT_CALLS: [AtomicUsize; 64] = [const { AtomicUsize::new(0) }; 64];
static EV_COUNT: AtomicUsize = AtomicUsize::new(0);
static STACK_LOCK: AtomicBool = AtomicBool::new(false);
static RECORDING: AtomicBool = AtomicBool::new(false);
static STACK_OVERFLOW: AtomicUsize = AtomicUsize::new(0);

#[inline(never)]
unsafe fn record(kind: u32, base: u64, size: u64, typ: u32, prot: u32) {
    let mut frames = [null_mut::<c_void>(); NFRAMES];
    let mut hash = 0u32;
    let n = RtlCaptureStackBackTrace(2, NFRAMES as u32, frames.as_mut_ptr(), &mut hash) as usize;
    let n = n.max(1);
    let mut idx = (hash as usize).wrapping_mul(0x9E37_79B1) & (NSTACKS - 1);
    let mut sid = u32::MAX;
    while STACK_LOCK.swap(true, Ordering::Acquire) {
        core::hint::spin_loop();
    }
    for _ in 0..4096 {
        let s = &mut *STACKS.add(idx);
        if s.n == 0 {
            s.hash = hash;
            s.n = n as u32;
            for i in 0..n {
                s.frames[i] = frames[i] as usize;
            }
            sid = idx as u32;
            break;
        }
        if s.hash == hash && s.n as usize == n && (0..n).all(|i| s.frames[i] == frames[i] as usize)
        {
            sid = idx as u32;
            break;
        }
        idx = (idx + 1) & (NSTACKS - 1);
    }
    STACK_LOCK.store(false, Ordering::Release);
    if sid == u32::MAX {
        STACK_OVERFLOW.fetch_add(1, Ordering::Relaxed);
    }
    let i = EV_COUNT.fetch_add(1, Ordering::Relaxed);
    if i < MAXEV {
        *EVENTS.add(i) = Event {
            kind,
            typ,
            prot,
            stack: sid,
            base,
            size,
            tid: GetCurrentThreadId(),
            pad: 0,
        };
    }
}

unsafe fn is_self(ph: Handle) -> bool {
    ph as isize == -1 || GetProcessId(ph) == GetCurrentProcessId()
}

unsafe fn private_usage() -> u64 {
    let mut buf = [0u64; 12];
    if NtQueryInformationProcess(
        -1isize as Handle,
        3,
        buf.as_mut_ptr().cast(),
        96,
        null_mut(),
    ) >= 0
    {
        buf[11]
    } else {
        0
    }
}

type AllocFn =
    unsafe extern "system" fn(Handle, *mut *mut c_void, usize, *mut usize, u32, u32) -> i32;
type AllocExFn = unsafe extern "system" fn(
    Handle,
    *mut *mut c_void,
    *mut usize,
    u32,
    u32,
    *mut c_void,
    u32,
) -> i32;
type FreeFn = unsafe extern "system" fn(Handle, *mut *mut c_void, *mut usize, u32) -> i32;

unsafe extern "system" fn hook_alloc(
    ph: Handle,
    base: *mut *mut c_void,
    zero: usize,
    size: *mut usize,
    typ: u32,
    prot: u32,
) -> i32 {
    let orig: AllocFn = core::mem::transmute(ORIG_ALLOC);
    let st = orig(ph, base, zero, size, typ, prot);
    if st >= 0
        && RECORDING.load(Ordering::Relaxed)
        && !base.is_null()
        && !size.is_null()
        && is_self(ph)
    {
        record(1, *base as u64, *size as u64, typ, prot);
    }
    st
}

unsafe extern "system" fn hook_alloc_ex(
    ph: Handle,
    base: *mut *mut c_void,
    size: *mut usize,
    typ: u32,
    prot: u32,
    ext: *mut c_void,
    ext_count: u32,
) -> i32 {
    let orig: AllocExFn = core::mem::transmute(ORIG_ALLOC_EX);
    let st = orig(ph, base, size, typ, prot, ext, ext_count);
    if st >= 0
        && RECORDING.load(Ordering::Relaxed)
        && !base.is_null()
        && !size.is_null()
        && is_self(ph)
    {
        record(2, *base as u64, *size as u64, typ, prot);
    }
    st
}

unsafe extern "system" fn hook_free(
    ph: Handle,
    base: *mut *mut c_void,
    size: *mut usize,
    free_type: u32,
) -> i32 {
    let orig: FreeFn = core::mem::transmute(ORIG_FREE);
    let st = orig(ph, base, size, free_type);
    if st >= 0
        && RECORDING.load(Ordering::Relaxed)
        && !base.is_null()
        && !size.is_null()
        && is_self(ph)
    {
        record(3, *base as u64, *size as u64, free_type, 0);
    }
    st
}

unsafe extern "system" fn kmt_hook(id: u32, arg: *mut c_void) -> i32 {
    let orig: unsafe extern "system" fn(*mut c_void) -> i32 =
        core::mem::transmute(ORIG_KMT[id as usize]);
    if !RECORDING.load(Ordering::Relaxed) {
        return orig(arg);
    }
    KMT_CALLS[id as usize].fetch_add(1, Ordering::Relaxed);
    let before = private_usage();
    let st = orig(arg);
    let after = private_usage();
    let delta = after as i64 - before as i64;
    if delta.unsigned_abs() >= 65536 {
        let mut out_ptr = 0u64;
        if !arg.is_null() && st >= 0 {
            if id == 2 {
                out_ptr = (arg as *const u8).add(16).cast::<u64>().read_unaligned();
            } else if id == 1 {
                out_ptr = (arg as *const u8).add(24).cast::<u64>().read_unaligned();
            }
        }
        record(100 + id, out_ptr, delta as u64, st as u32, 0);
    }
    st
}

/// Installs a 5-byte jump on a syscall stub; returns the trampoline that runs the original.
unsafe fn install_in(
    module: Handle,
    tramp_page: *mut *mut u8,
    name: &str,
    slot: usize,
    hook: usize,
    kmt_id: Option<u32>,
    orig_out: *mut usize,
    log: &mut String,
) -> usize {
    let cname = format!("{name}\0");
    let target = GetProcAddress(module, cname.as_ptr()) as *mut u8;
    if target.is_null() {
        log.push_str(&format!("{name}: not found\n"));
        return 0;
    }
    let head = core::slice::from_raw_parts(target, 8);
    if head[..4] != [0x4C, 0x8B, 0xD1, 0xB8] {
        log.push_str(&format!(
            "{name}: unexpected prologue {head:02x?}, not hooked\n"
        ));
        return 0;
    }
    if (*tramp_page).is_null() {
        let start = (target as usize) & !0xFFFF;
        for k in 1..30000usize {
            let cand = start.wrapping_sub(k * 0x10000);
            let p = VirtualAlloc(cand as *mut c_void, 4096, 0x3000, 0x40);
            if !p.is_null() {
                *tramp_page = p as *mut u8;
                break;
            }
        }
        if (*tramp_page).is_null() {
            log.push_str("no trampoline page near module\n");
            return 0;
        }
    }
    let t = (*tramp_page).add(slot * 64);
    // [0..8) original instructions, then jmp [rip+0] -> target+8
    core::ptr::copy_nonoverlapping(target, t, 8);
    t.add(8)
        .copy_from_nonoverlapping([0xFFu8, 0x25, 0, 0, 0, 0].as_ptr(), 6);
    (t.add(14) as *mut u64).write_unaligned(target as u64 + 8);
    // relay at +32: [mov rdx, rcx ; mov ecx, id ;] jmp [rip+0] -> hook
    let relay = t.add(32);
    let mut w = relay;
    if let Some(id) = kmt_id {
        w.copy_from_nonoverlapping([0x48u8, 0x89, 0xCA, 0xB9].as_ptr(), 4);
        (w.add(4) as *mut u32).write_unaligned(id);
        w = w.add(8);
    }
    w.copy_from_nonoverlapping([0xFFu8, 0x25, 0, 0, 0, 0].as_ptr(), 6);
    (w.add(6) as *mut u64).write_unaligned(hook as u64);
    let rel = (relay as isize) - (target as isize + 5);
    if rel > i32::MAX as isize || rel < i32::MIN as isize {
        log.push_str(&format!("{name}: relay out of range\n"));
        return 0;
    }
    *orig_out = t as usize;
    let mut old = 0u32;
    if VirtualProtect(target.cast(), 8, 0x40, &mut old) == 0 {
        *orig_out = 0;
        log.push_str(&format!("{name}: VirtualProtect failed\n"));
        return 0;
    }
    let mut patch = [0xE9u8, 0, 0, 0, 0, 0x90, 0x90, 0x90];
    patch[1..5].copy_from_slice(&(rel as i32).to_le_bytes());
    core::ptr::copy_nonoverlapping(patch.as_ptr(), target, 8);
    let mut tmp = 0u32;
    VirtualProtect(target.cast(), 8, old, &mut tmp);
    FlushInstructionCache(GetCurrentProcess(), target.cast(), 8);
    log.push_str(&format!("{name}: hooked at {:#x}\n", target as usize));
    t as usize
}

fn module_of(addr: usize, cache: &mut BTreeMap<usize, String>) -> (usize, String) {
    unsafe {
        let mut mbi: Mbi = core::mem::zeroed();
        if VirtualQuery(addr as *const c_void, &mut mbi, core::mem::size_of::<Mbi>()) == 0 {
            return (0, String::new());
        }
        let ab = mbi.alloc_base as usize;
        if mbi.typ != 0x100_0000 {
            return (ab, String::new());
        }
        if let Some(n) = cache.get(&ab) {
            return (ab, n.clone());
        }
        let mut buf = [0u16; 600];
        let n = GetModuleFileNameW(ab as Handle, buf.as_mut_ptr(), 600) as usize;
        let full = String::from_utf16_lossy(&buf[..n]);
        let name = full.rsplit(['\\', '/']).next().unwrap_or("").to_string();
        cache.insert(ab, name.clone());
        (ab, name)
    }
}

fn dump_heaps(out: &PathBuf) {
    let mut text = String::new();
    unsafe {
        let mut heaps = [null_mut::<c_void>(); 256];
        let n = GetProcessHeaps(256, heaps.as_mut_ptr()) as usize;
        let default = GetProcessHeap();
        for (hi, &h) in heaps.iter().enumerate().take(n.min(256)) {
            let mut busy_hist = [0u64; 48];
            let mut busy_cnt_hist = [0u64; 48];
            let (mut busy, mut busy_n, mut free, mut free_n, mut overhead) =
                (0u64, 0u64, 0u64, 0u64, 0u64);
            let (mut committed, mut uncommitted, mut regions) = (0u64, 0u64, 0u64);
            let mut big: Vec<(usize, u32)> = Vec::with_capacity(8192);
            let mut e: HeapEntry = core::mem::zeroed();
            if HeapLock(h) == 0 {
                text.push_str(&format!("heap {hi} {:#x}: HeapLock failed\n", h as usize));
                continue;
            }
            while HeapWalk(h, &mut e) != 0 {
                if e.flags & 0x1 != 0 {
                    regions += 1;
                    committed += e.u0 as u64;
                    uncommitted += e.u1 as u64;
                } else if e.flags & 0x2 != 0 {
                    // uncommitted range
                } else if e.flags & 0x4 != 0 {
                    busy += e.cb_data as u64;
                    busy_n += 1;
                    overhead += e.cb_overhead as u64;
                    let b = (64 - (e.cb_data.max(1) as u64).leading_zeros()) as usize;
                    busy_hist[b.min(47)] += e.cb_data as u64;
                    busy_cnt_hist[b.min(47)] += 1;
                    if e.cb_data >= 65536 && big.len() < big.capacity() {
                        big.push((e.data as usize, e.cb_data));
                    }
                } else {
                    free += e.cb_data as u64;
                    free_n += 1;
                }
            }
            HeapUnlock(h);
            text.push_str(&format!(
                "heap {hi} base={:#x} default={} regions={regions} region_committed={committed} region_uncommitted={uncommitted} busy_bytes={busy} busy_blocks={busy_n} overhead={overhead} free_bytes={free} free_blocks={free_n}\n",
                h as usize,
                h == default
            ));
            for b in 0..48 {
                if busy_cnt_hist[b] > 0 {
                    text.push_str(&format!(
                        "  size<2^{b}: blocks={} bytes={}\n",
                        busy_cnt_hist[b], busy_hist[b]
                    ));
                }
            }
            big.sort_by(|a, b| b.1.cmp(&a.1));
            for (addr, sz) in big.iter().take(400) {
                text.push_str(&format!("  big {addr:#x} {sz}\n"));
            }
        }
    }
    let _ = std::fs::write(out.join("heaps.txt"), text);
}

fn dump(out: &PathBuf, install_log: &str) {
    RECORDING.store(false, Ordering::SeqCst);
    let nev = EV_COUNT.load(Ordering::SeqCst).min(MAXEV);
    unsafe {
        let bytes =
            core::slice::from_raw_parts(EVENTS as *const u8, nev * core::mem::size_of::<Event>());
        let _ = std::fs::write(out.join("events.bin"), bytes);
    }
    dump_heaps(out);

    let mut used: BTreeSet<u32> = BTreeSet::new();
    unsafe {
        for i in 0..nev {
            used.insert((*EVENTS.add(i)).stack);
        }
    }
    let mut frames: BTreeSet<usize> = BTreeSet::new();
    let mut stacks_txt = String::new();
    unsafe {
        for &sid in &used {
            if sid == u32::MAX {
                continue;
            }
            let s = &*STACKS.add(sid as usize);
            stacks_txt.push_str(&format!("{sid}\t"));
            for i in 0..s.n as usize {
                frames.insert(s.frames[i]);
                stacks_txt.push_str(&format!("{:x},", s.frames[i]));
            }
            stacks_txt.push('\n');
        }
    }
    let _ = std::fs::write(out.join("stacks.tsv"), stacks_txt);

    let mut cache = BTreeMap::new();
    let mut f = match std::fs::File::create(out.join("frames.tsv")) {
        Ok(f) => std::io::BufWriter::new(f),
        Err(_) => return,
    };
    for &addr in &frames {
        if addr == 0 {
            continue;
        }
        let (mbase, mname) = module_of(addr, &mut cache);
        let mut sym = String::new();
        let mut loc = String::new();
        if !mname.is_empty() {
            backtrace::resolve((addr - 1) as *mut c_void, |s| {
                if sym.is_empty() {
                    if let Some(n) = s.name() {
                        sym = format!("{n:#}");
                    }
                    if let (Some(file), Some(line)) = (s.filename(), s.lineno()) {
                        loc = format!("{}:{}", file.display(), line);
                    }
                } else if let Some(n) = s.name() {
                    sym.push_str(&format!(" <- {n:#}"));
                }
            });
        }
        let _ = writeln!(
            f,
            "{addr:x}\t{mname}\t{:x}\t{}\t{}",
            addr.wrapping_sub(mbase),
            sym.replace(['\t', '\n'], " "),
            loc
        );
    }
    let _ = f.flush();

    let mut meta = unsafe {
        format!(
            "events={}\nevents_recorded={nev}\nstack_overflow={}\nevents_table={:#x} size={}\nstacks_table={:#x} size={}\ntrampoline={:#x}\n",
            EV_COUNT.load(Ordering::SeqCst),
            STACK_OVERFLOW.load(Ordering::SeqCst),
            EVENTS as usize,
            MAXEV * core::mem::size_of::<Event>(),
            STACKS as usize,
            NSTACKS * core::mem::size_of::<Stack>(),
            TRAMP as usize
        )
    };
    for (i, name) in KMT_NAMES.iter().enumerate() {
        let n = KMT_CALLS[i].load(Ordering::Relaxed);
        if n > 0 {
            meta.push_str(&format!("kmt {i} {name} calls={n}\n"));
        }
    }
    meta.push_str(install_log);
    let _ = std::fs::write(out.join("meta.txt"), meta);
    let _ = std::fs::write(out.join("va.done"), b"ok");
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn memdiag_start() {
    let Some(out) = std::env::var_os("RG_MEMDIAG_OUT") else {
        return;
    };
    let out = PathBuf::from(out);
    EVENTS = VirtualAlloc(
        null_mut(),
        MAXEV * core::mem::size_of::<Event>(),
        0x3000,
        0x04,
    ) as *mut Event;
    STACKS = VirtualAlloc(
        null_mut(),
        NSTACKS * core::mem::size_of::<Stack>(),
        0x3000,
        0x04,
    ) as *mut Stack;
    if EVENTS.is_null() || STACKS.is_null() {
        return;
    }
    let mut log = String::new();
    let ntdll = GetModuleHandleA(c"ntdll.dll".as_ptr().cast());
    install_in(
        ntdll,
        &raw mut TRAMP,
        "NtAllocateVirtualMemory",
        0,
        hook_alloc as *const () as usize,
        None,
        &raw mut ORIG_ALLOC,
        &mut log,
    );
    install_in(
        ntdll,
        &raw mut TRAMP,
        "NtAllocateVirtualMemoryEx",
        1,
        hook_alloc_ex as *const () as usize,
        None,
        &raw mut ORIG_ALLOC_EX,
        &mut log,
    );
    install_in(
        ntdll,
        &raw mut TRAMP,
        "NtFreeVirtualMemory",
        2,
        hook_free as *const () as usize,
        None,
        &raw mut ORIG_FREE,
        &mut log,
    );
    if std::env::var_os("RG_MEMDIAG_NO_KMT").is_none() {
        let w32 = LoadLibraryA(c"win32u.dll".as_ptr().cast());
        if !w32.is_null() {
            for (i, name) in KMT_NAMES.iter().enumerate() {
                install_in(
                    w32,
                    &raw mut TRAMP_W32,
                    name,
                    i,
                    kmt_hook as *const () as usize,
                    Some(i as u32),
                    (&raw mut ORIG_KMT).cast::<usize>().add(i),
                    &mut log,
                );
            }
        }
    }
    let _ = std::fs::write(out.join("install.txt"), &log);
    RECORDING.store(true, Ordering::SeqCst);
    std::thread::spawn(move || {
        let trig = out.join("va.trigger");
        while !trig.exists() {
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        dump(&out, &log);
    });
}
