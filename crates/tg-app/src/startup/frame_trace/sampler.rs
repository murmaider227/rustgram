//! UI-thread stack sampler for `RUSTGRAM_FRAME_TRACE=sample` (Windows x64 only).
//!
//! Every millisecond while a script section runs, a helper thread suspends the UI thread, copies
//! its registers and the used part of its stack, resumes it, and unwinds the copy with
//! `RtlVirtualUnwind`. Nothing that can take a lock (heap, loader) runs while the UI thread is
//! suspended. Output: `<out>.stacks` records `[u64 t_ns][u32 depth][u64; depth]` (little endian,
//! nanoseconds on the same clock as the frame lines) and `<out>.modules` lines
//! `<base hex>\t<size hex>\t<path>` written when sampling stops.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

/// Handle to the running sampler thread.
pub struct Sampler {
    #[cfg(all(windows, target_arch = "x86_64"))]
    inner: imp::Running,
}

impl Sampler {
    /// Starts sampling the calling thread. Must be called on the UI thread.
    #[cfg(all(windows, target_arch = "x86_64"))]
    pub fn start(out: &Path, epoch: Instant, active: Arc<AtomicBool>) -> std::io::Result<Self> {
        Ok(Self {
            inner: imp::Running::start(out, epoch, active)?,
        })
    }

    /// Stack sampling is only implemented for Windows x64.
    #[cfg(not(all(windows, target_arch = "x86_64")))]
    pub fn start(_out: &Path, _epoch: Instant, _active: Arc<AtomicBool>) -> std::io::Result<Self> {
        Err(std::io::Error::other("stack sampling needs Windows x64"))
    }

    /// Stops the thread and writes the module list.
    pub fn stop(self) {
        #[cfg(all(windows, target_arch = "x86_64"))]
        self.inner.stop();
    }
}

#[cfg(all(windows, target_arch = "x86_64"))]
mod imp {
    use std::fs::File;
    use std::io::{BufWriter, Write};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant};

    use windows::Win32::Foundation::{CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE};
    use windows::Win32::System::Diagnostics::Debug::{
        CONTEXT, CONTEXT_CONTROL_AMD64, CONTEXT_INTEGER_AMD64, GetThreadContext,
        RtlLookupFunctionEntry, RtlVirtualUnwind, UNW_FLAG_NHANDLER,
    };
    use windows::Win32::System::Threading::{
        GetCurrentProcess, GetCurrentThread, GetCurrentThreadStackLimits, ResumeThread,
        SuspendThread,
    };

    const MAX_DEPTH: usize = 96;
    const MAX_STACK_COPY: usize = 2 * 1024 * 1024;

    #[repr(C, align(16))]
    struct AlignedContext(CONTEXT);

    /// Owned duplicate of the UI thread's handle; only the sampler thread uses it.
    struct ThreadHandle(HANDLE);

    // SAFETY: a thread handle is a kernel object reference that any thread may use.
    unsafe impl Send for ThreadHandle {}

    impl Drop for ThreadHandle {
        fn drop(&mut self) {
            // SAFETY: the handle came from `DuplicateHandle` and is closed exactly once.
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    pub struct Running {
        stop: Arc<AtomicBool>,
        join: Option<JoinHandle<()>>,
        modules_path: PathBuf,
    }

    impl Running {
        pub fn start(out: &Path, epoch: Instant, active: Arc<AtomicBool>) -> std::io::Result<Self> {
            let mut target = HANDLE::default();
            // SAFETY: duplicates the current thread's pseudo handle into a real handle owned by
            // `ThreadHandle`, which closes it.
            unsafe {
                DuplicateHandle(
                    GetCurrentProcess(),
                    GetCurrentThread(),
                    GetCurrentProcess(),
                    &mut target,
                    0,
                    false,
                    DUPLICATE_SAME_ACCESS,
                )
            }
            .map_err(std::io::Error::other)?;
            let target = ThreadHandle(target);
            let mut low = 0usize;
            let mut high = 0usize;
            // SAFETY: both pointers are valid for writes.
            unsafe { GetCurrentThreadStackLimits(&mut low, &mut high) };
            let stacks_path = with_suffix(out, "stacks");
            let mut writer = BufWriter::new(File::create(&stacks_path)?);
            let stop = Arc::new(AtomicBool::new(false));
            let stop_thread = Arc::clone(&stop);
            let join = std::thread::Builder::new()
                .name("frame-trace-sampler".to_owned())
                .spawn(move || {
                    let mut stack_copy = vec![0u64; MAX_STACK_COPY / 8];
                    let mut frames = [0u64; MAX_DEPTH];
                    while !stop_thread.load(Ordering::Relaxed) {
                        std::thread::sleep(Duration::from_millis(1));
                        if !active.load(Ordering::Relaxed) {
                            continue;
                        }
                        let t_ns = u64::try_from(epoch.elapsed().as_nanos()).unwrap_or(u64::MAX);
                        let depth = sample(&target, low, high, &mut stack_copy, &mut frames);
                        if depth == 0 {
                            continue;
                        }
                        let _ = writer.write_all(&t_ns.to_le_bytes());
                        let _ = writer.write_all(&(depth as u32).to_le_bytes());
                        for addr in &frames[..depth] {
                            let _ = writer.write_all(&addr.to_le_bytes());
                        }
                    }
                    let _ = writer.flush();
                })?;
            Ok(Self {
                stop,
                join: Some(join),
                modules_path: with_suffix(out, "modules"),
            })
        }

        pub fn stop(mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(join) = self.join.take() {
                let _ = join.join();
            }
            let _ = write_modules(&self.modules_path);
        }
    }

    fn with_suffix(out: &Path, suffix: &str) -> PathBuf {
        let mut s = out.as_os_str().to_owned();
        s.push(".");
        s.push(suffix);
        PathBuf::from(s)
    }

    /// Takes one stack sample of `target`; returns the number of frames written to `frames`.
    fn sample(
        target: &ThreadHandle,
        low: usize,
        high: usize,
        stack_copy: &mut [u64],
        frames: &mut [u64; MAX_DEPTH],
    ) -> usize {
        // SAFETY: CONTEXT is plain data; all-zero is a valid initial value.
        let mut ctx: AlignedContext = unsafe { std::mem::zeroed() };
        ctx.0.ContextFlags = CONTEXT_CONTROL_AMD64 | CONTEXT_INTEGER_AMD64;
        let copy_bytes;
        // SAFETY: the thread stays suspended only for `GetThreadContext` and a plain memory
        // copy of its own committed stack range [rsp, high); neither takes a lock it might hold.
        unsafe {
            if SuspendThread(target.0) == u32::MAX {
                return 0;
            }
            let ok = GetThreadContext(target.0, &mut ctx.0).is_ok();
            let rsp = ctx.0.Rsp as usize;
            copy_bytes = if ok && rsp >= low && rsp < high && rsp.is_multiple_of(8) {
                (high - rsp).min(stack_copy.len() * 8)
            } else {
                0
            };
            if copy_bytes > 0 {
                std::ptr::copy_nonoverlapping(
                    rsp as *const u8,
                    stack_copy.as_mut_ptr().cast::<u8>(),
                    copy_bytes,
                );
            }
            ResumeThread(target.0);
        }
        if copy_bytes == 0 {
            return 0;
        }
        unwind_copy(&mut ctx, stack_copy, copy_bytes, high, frames)
    }

    /// Unwinds a copied stack: pointers into the original stack are moved into the copy first.
    fn unwind_copy(
        ctx: &mut AlignedContext,
        stack_copy: &mut [u64],
        copy_bytes: usize,
        high: usize,
        frames: &mut [u64; MAX_DEPTH],
    ) -> usize {
        let orig_lo = ctx.0.Rsp;
        let orig_hi = orig_lo + copy_bytes as u64;
        let copy_lo = stack_copy.as_ptr() as u64;
        let copy_hi = copy_lo + copy_bytes as u64;
        let relocate = |v: u64| {
            if v >= orig_lo && v < orig_hi {
                v - orig_lo + copy_lo
            } else {
                v
            }
        };
        for word in &mut stack_copy[..copy_bytes / 8] {
            if *word >= orig_lo && *word < high as u64 {
                *word = relocate(*word);
            }
        }
        let c = &mut ctx.0;
        for reg in [
            &mut c.Rax, &mut c.Rbx, &mut c.Rcx, &mut c.Rdx, &mut c.Rsi, &mut c.Rdi, &mut c.Rbp,
            &mut c.Rsp, &mut c.R8, &mut c.R9, &mut c.R10, &mut c.R11, &mut c.R12, &mut c.R13,
            &mut c.R14, &mut c.R15,
        ] {
            *reg = relocate(*reg);
        }
        let mut depth = 0;
        frames[depth] = c.Rip;
        depth += 1;
        while depth < MAX_DEPTH {
            let mut image_base = 0u64;
            // SAFETY: looks up unwind data of a code address; reads only loaded image tables.
            let entry = unsafe { RtlLookupFunctionEntry(c.Rip, &mut image_base, None) };
            if entry.is_null() {
                // Leaf function without unwind data: the return address is at [rsp].
                if c.Rsp < copy_lo || c.Rsp + 8 > copy_hi {
                    break;
                }
                // SAFETY: bounds-checked above; the address lies inside `stack_copy`.
                c.Rip = unsafe { *(c.Rsp as *const u64) };
                c.Rsp += 8;
            } else {
                let mut handler_data = std::ptr::null_mut();
                let mut establisher = 0u64;
                // SAFETY: `c.Rsp` and every stack pointer register point into `stack_copy`
                // (checked on each iteration), so the unwinder reads only the copy.
                unsafe {
                    RtlVirtualUnwind(
                        UNW_FLAG_NHANDLER,
                        image_base,
                        c.Rip,
                        entry,
                        c,
                        &mut handler_data,
                        &mut establisher,
                        None,
                    );
                }
            }
            if c.Rip == 0 || c.Rsp < copy_lo || c.Rsp >= copy_hi {
                break;
            }
            frames[depth] = c.Rip;
            depth += 1;
        }
        depth
    }

    #[repr(C)]
    struct ModuleInfo {
        base: *mut core::ffi::c_void,
        size: u32,
        entry: *mut core::ffi::c_void,
    }

    unsafe extern "system" {
        fn K32EnumProcessModules(
            process: HANDLE,
            modules: *mut isize,
            cb: u32,
            needed: *mut u32,
        ) -> i32;
        fn K32GetModuleInformation(
            process: HANDLE,
            module: isize,
            info: *mut ModuleInfo,
            cb: u32,
        ) -> i32;
        fn K32GetModuleFileNameExW(
            process: HANDLE,
            module: isize,
            name: *mut u16,
            size: u32,
        ) -> u32;
    }

    fn write_modules(path: &Path) -> std::io::Result<()> {
        let mut modules = vec![0isize; 1024];
        let mut needed = 0u32;
        // SAFETY: the buffer is valid for `cb` bytes; the pseudo process handle needs no close.
        let process = unsafe { GetCurrentProcess() };
        let ok = unsafe {
            K32EnumProcessModules(
                process,
                modules.as_mut_ptr(),
                (modules.len() * std::mem::size_of::<isize>()) as u32,
                &mut needed,
            )
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let count = (needed as usize / std::mem::size_of::<isize>()).min(modules.len());
        let mut out = BufWriter::new(File::create(path)?);
        for &module in &modules[..count] {
            let mut info = ModuleInfo {
                base: std::ptr::null_mut(),
                size: 0,
                entry: std::ptr::null_mut(),
            };
            let mut name = [0u16; 1024];
            // SAFETY: valid output buffers with matching sizes.
            let (ok_info, len) = unsafe {
                (
                    K32GetModuleInformation(
                        process,
                        module,
                        &mut info,
                        std::mem::size_of::<ModuleInfo>() as u32,
                    ),
                    K32GetModuleFileNameExW(process, module, name.as_mut_ptr(), name.len() as u32),
                )
            };
            if ok_info == 0 || len == 0 {
                continue;
            }
            let name = String::from_utf16_lossy(&name[..len as usize]);
            writeln!(out, "{:x}\t{:x}\t{}", info.base as usize, info.size, name)?;
        }
        out.flush()
    }
}
