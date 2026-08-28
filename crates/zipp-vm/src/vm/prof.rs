//! `ZIPP_PROF=1` — a sampling profiler that attributes wall time to ENGINE
//! SUBSYSTEMS.
//!
//! §6 has listed this as missing since B3 — *"There is no way to attribute
//! engine time to a source construct, which is precisely how the two reverted
//! epics happened. A sampling profiler behind `ZIPP_PROF=1` would pay for itself
//! immediately and is a prerequisite for honest work on B3/B6."* B84 is the
//! argument made concrete: an `ObjMap` recycle pool measured −35% on object
//! construction, passed the whole gate, and still regressed `json-large` by
//! +2.9% for a reason that TWO hypotheses failed to explain (memory retention
//! was measured and refuted at +1.8% RSS). Without attribution the only honest
//! move was to revert a real win.
//!
//! ## How it works, and why it is not a stack sampler
//!
//! A native stack sampler on Windows means `SuspendThread` + `StackWalk64` +
//! `dbghelp` symbolication, a new dependency, and a deadlock hazard the moment
//! the sampler allocates while the engine thread holds the allocator lock —
//! against a release binary built with fat LTO, where most of the frames it
//! would name have been inlined away.
//!
//! So this samples a PHASE TAG instead. The engine publishes which subsystem it
//! is inside via one relaxed atomic store, a sampler thread reads that tag on a
//! fixed interval, and the histogram of tags is a time breakdown. It is coarser
//! than a stack sampler and it cannot lie about inlining, because the tags are
//! placed by hand at subsystem boundaries rather than recovered from frames.
//!
//! ## Cost when off
//!
//! [`enter`] is a cached `AtomicU8` load and a branch; with `ZIPP_PROF` unset it
//! performs no store and allocates no thread. The guard is `#[inline]` and its
//! `Drop` is a no-op in that case, so an untagged build and a tagged one differ
//! by a predictable branch on paths that already cost tens of nanoseconds.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};

/// Subsystem tags.
///
/// `Interp` is the RESTING state, and its display name is `interp/untagged` for
/// a reason learned the hard way: it means "no tag was active", NOT "the
/// interpreter was running bytecode". Any native engine work reached through a
/// path nobody tagged lands here and reads as interpretation.
///
/// That is not hypothetical. `JSON.stringify(v)` with one argument is FUSED by
/// the compiler into `Instr::JsonStringify` and never reaches `call_native`, so
/// tagging only the native arm left a stringify-only workload reporting **100%
/// `interp`** — and made `json-large` look 40% interpreted when 24 points of
/// that were stringify. Before drawing "this row is not compiling" from a large
/// `interp` share, check that the work in it is actually tagged.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum Phase {
    Interp = 0,
    Gc = 1,
    RegexExec = 2,
    JsonParse = 3,
    JsonStringify = 4,
    StringOps = 5,
    PropSlow = 6,
    #[cfg(all(feature = "jit", any(target_arch = "x86_64", target_arch = "aarch64")))]
    JitCompile = 7,
    /// Executing NATIVE compiled code (a Tier A/C function body or an OSR
    /// region). Distinct from `Interp` because the two have opposite fixes:
    /// time in `Interp` means not enough code is COMPILED, time in `Jit` means
    /// compiled code is SLOW (M4's register file and per-op boxing).
    #[cfg(all(feature = "jit", any(target_arch = "x86_64", target_arch = "aarch64")))]
    Jit = 8,
    /// Promise / microtask machinery: the event-loop drain and everything it
    /// runs that is not user JS. `async-promise-chain` reported **79.1% `interp`**
    /// before this existed, and almost none of it was interpreting bytecode.
    Microtask = 9,
    /// Native compiled code running on the MEMORY-backed register path
    /// (`region_mem`): every intermediate goes through `[rbx + dreg(r)]` and is
    /// re-boxed at each step. Split out from `Jit` because B92 measured the two
    /// tiers ~4x apart on the SAME loop, so "99.7% jit-native" was compatible
    /// with a row being entirely in the slow tier and said nothing either way.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    JitMem = 10,
}

const N_PHASES: usize = 11;

const NAMES: [&str; N_PHASES] = [
    "interp/untagged",
    "gc",
    "regex-exec",
    "json-parse",
    "json-stringify",
    "string-ops",
    "prop-slow",
    "jit-compile",
    "jit-fast",
    "microtask",
    "jit-mem",
];

static CURRENT: AtomicU8 = AtomicU8::new(Phase::Interp as u8);
static COUNTS: [AtomicU64; N_PHASES] = [
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
];
static SAMPLES: AtomicU64 = AtomicU64::new(0);
static RUNNING: AtomicBool = AtomicBool::new(false);
static ON: AtomicU8 = AtomicU8::new(2);

/// Sampling interval. 200µs over a ~500ms benchmark row is ~2,500 samples —
/// enough that a 2% subsystem is ~50 samples and visible above noise, while the
/// sampler thread itself stays under a tenth of a percent of one core.
const INTERVAL: std::time::Duration = std::time::Duration::from_micros(200);

#[inline]
pub(crate) fn enabled() -> bool {
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => init(),
    }
}

/// One-time env latch, OUTLINED: `enabled()` sits on per-call JIT hot paths
/// (every cross-call helper takes a phase guard), and inlining the env read +
/// sampler spawn into every caller bloated exactly those paths — the W7
/// census priced the guard at ~1-2ns/call of the ~19ns cross-call residual,
/// most of it recovered by keeping the hot side to one load + compare.
#[cold]
fn init() -> bool {
    let v = (std::env::var_os("ZIPP_PROF").is_some()
        || std::env::var_os("ZIPP_PROF_PC").is_some()) as u8;
    ON.store(v, Ordering::Relaxed);
    if v == 1 {
        // B237: the FIRST thread to enter a phase is the engine thread, which
        // is the one worth sampling. Capture it here, before the sampler runs.
        pc::arm_current_thread();
        start();
    }
    v == 1
}

/// Launch the sampler thread once. It is a daemon: the process exits without
/// joining it, and [`dump`] reads the counters directly.
fn start() {
    if RUNNING.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::Builder::new()
        .name("zipp-prof".into())
        .spawn(|| loop {
            std::thread::sleep(INTERVAL);
            let p = CURRENT.load(Ordering::Relaxed) as usize;
            if p < N_PHASES {
                COUNTS[p].fetch_add(1, Ordering::Relaxed);
                SAMPLES.fetch_add(1, Ordering::Relaxed);
            }
            pc::sample();
        })
        .ok();
}

/// RAII phase tag. Restores the PREVIOUS phase on drop rather than resetting to
/// `Interp`, so nesting (a regex exec that allocates, a GC triggered inside
/// JSON parse) attributes to the innermost tag and unwinds correctly.
pub(crate) struct Guard(u8, bool);

#[inline]
pub(crate) fn enter(p: Phase) -> Guard {
    if !enabled() {
        return Guard(0, false);
    }
    let prev = CURRENT.swap(p as u8, Ordering::Relaxed);
    Guard(prev, true)
}

impl Drop for Guard {
    #[inline]
    fn drop(&mut self) {
        if self.1 {
            CURRENT.store(self.0, Ordering::Relaxed);
        }
    }
}

/// `(phase name, samples, percent)` sorted by sample count, plus the total.
pub fn dump() -> (Vec<(&'static str, u64, f64)>, u64) {
    let total = SAMPLES.load(Ordering::Relaxed);
    let mut v: Vec<(&'static str, u64, f64)> = (0..N_PHASES)
        .map(|i| {
            let c = COUNTS[i].load(Ordering::Relaxed);
            let pct = if total == 0 {
                0.0
            } else {
                c as f64 * 100.0 / total as f64
            };
            (NAMES[i], c, pct)
        })
        .filter(|(_, c, _)| *c > 0)
        .collect();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    (v, total)
}


/// B237: instruction-pointer sampling.
///
/// The phase sampler above answers "which subsystem", and for a row that is
/// 78% `jit-fast` that is the end of the road -- every remaining hostile gap
/// lives inside emitted code, which carries no phase tags and whose frames a
/// fat-LTO release build has largely inlined away. This mode answers "which
/// emitted body" instead, and it needs neither stack walking nor symbols: the
/// JIT knows the address range of every buffer it emits, so the ranges are
/// registered at install time and a sampled RIP is resolved by binary search
/// afterwards.
///
/// The sampler suspends the engine thread to read its context. Between
/// `SuspendThread` and `ResumeThread` it must not allocate or take a lock the
/// engine might hold -- so the sample buffer is allocated ONCE when the mode
/// arms, the `CONTEXT` lives on the sampler's stack, and the window contains
/// exactly two system calls and one `u64` read.
#[cfg(all(windows, target_arch = "x86_64", not(feature = "safe-sandbox")))]
pub(crate) mod pc {
    use std::sync::atomic::{AtomicUsize, Ordering};

    type Handle = isize;

    extern "system" {
        fn GetCurrentThread() -> Handle;
        fn GetCurrentProcess() -> Handle;
        fn DuplicateHandle(
            src_process: Handle,
            src: Handle,
            dst_process: Handle,
            dst: *mut Handle,
            access: u32,
            inherit: i32,
            options: u32,
        ) -> i32;
        fn SuspendThread(t: Handle) -> u32;
        fn ResumeThread(t: Handle) -> u32;
        fn GetThreadContext(t: Handle, ctx: *mut u8) -> i32;
    }

    #[link(name = "dbghelp")]
    extern "system" {
        fn SymInitialize(process: Handle, search_path: *const u8, invade: i32) -> i32;
        fn SymSetOptions(options: u32) -> u32;
        fn SymFromAddr(process: Handle, addr: u64, disp: *mut u64, sym: *mut u8) -> i32;
    }

    extern "system" {
        fn GetModuleHandleW(name: *const u16) -> usize;
    }

    /// `(base, size)` of the running executable, read from its own PE headers:
    /// `e_lfanew` at 0x3C, then past the 4-byte signature and the 20-byte COFF
    /// header to the optional header, whose `SizeOfImage` sits at 0x38.
    fn main_module() -> Option<(u64, u64)> {
        // SAFETY: reading our own mapped image's headers, which are present
        // for the life of the process.
        unsafe {
            let base = GetModuleHandleW(std::ptr::null());
            if base == 0 {
                return None;
            }
            let p = base as *const u8;
            let pe = std::ptr::read_unaligned(p.add(0x3C) as *const u32) as usize;
            let size = std::ptr::read_unaligned(p.add(pe + 4 + 20 + 0x38) as *const u32) as u64;
            Some((base as u64, size))
        }
    }

    const DUPLICATE_SAME_ACCESS: u32 = 0x0000_0002;
    /// x86-64 `CONTEXT`: 1232 bytes, 16-byte aligned, `ContextFlags` at 0x30
    /// and `Rip` at 0xF8. Stable ABI, and the only two fields this touches.
    const CONTEXT_SIZE: usize = 1232;
    const CONTEXT_FLAGS_OFF: usize = 0x30;
    const CONTEXT_RIP_OFF: usize = 0xF8;
    /// `CONTEXT_AMD64 | CONTEXT_CONTROL` — the control registers only, which is
    /// the cheapest capture that includes `Rip`.
    const CONTEXT_CONTROL: u32 = 0x0010_0001;

    const MAX_SAMPLES: usize = 1 << 20;

    static THREAD: AtomicUsize = AtomicUsize::new(0);
    static BUF: AtomicUsize = AtomicUsize::new(0);
    static COUNT: AtomicUsize = AtomicUsize::new(0);
    static DROPPED: AtomicUsize = AtomicUsize::new(0);

    #[repr(C, align(16))]
    struct Context([u8; CONTEXT_SIZE]);

    /// Is PC mode requested? Separate from the phase profiler so `ZIPP_PROF=1`
    /// alone keeps its old, allocation-free, suspend-free behaviour.
    fn wanted() -> bool {
        std::env::var_os("ZIPP_PROF_PC").is_some()
    }

    /// Duplicate the CALLING thread's handle into one usable from the sampler,
    /// and allocate the sample buffer. Called once, from the engine thread,
    /// before the sampler exists.
    pub(crate) fn arm_current_thread() {
        if !wanted() || THREAD.load(Ordering::Relaxed) != 0 {
            return;
        }
        let mut dup: Handle = 0;
        // SAFETY: pseudo-handles from GetCurrentThread/GetCurrentProcess are
        // valid for the duration of the call; `dup` is a live local.
        let ok = unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                GetCurrentThread(),
                GetCurrentProcess(),
                &mut dup,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            )
        };
        if ok == 0 || dup == 0 {
            return;
        }
        let buf = vec![0u64; MAX_SAMPLES].into_boxed_slice();
        BUF.store(Box::leak(buf).as_mut_ptr() as usize, Ordering::Release);
        THREAD.store(dup as usize, Ordering::Release);
    }

    /// One sample. A no-op unless the mode armed, so the phase sampler's loop
    /// pays a single relaxed load when it is not in use.
    pub(crate) fn sample() {
        let h = THREAD.load(Ordering::Relaxed);
        let buf = BUF.load(Ordering::Relaxed);
        if h == 0 || buf == 0 {
            return;
        }
        let mut ctx = Context([0u8; CONTEXT_SIZE]);
        // SAFETY: `ctx` is a live, correctly aligned CONTEXT-sized buffer, and
        // the suspend window below allocates nothing and takes no lock.
        unsafe {
            std::ptr::write(
                ctx.0.as_mut_ptr().add(CONTEXT_FLAGS_OFF) as *mut u32,
                CONTEXT_CONTROL,
            );
            if SuspendThread(h as Handle) == u32::MAX {
                return;
            }
            let got = GetThreadContext(h as Handle, ctx.0.as_mut_ptr());
            ResumeThread(h as Handle);
            if got == 0 {
                return;
            }
            let rip = std::ptr::read(ctx.0.as_ptr().add(CONTEXT_RIP_OFF) as *const u64);
            let i = COUNT.fetch_add(1, Ordering::Relaxed);
            if i < MAX_SAMPLES {
                std::ptr::write((buf as *mut u64).add(i), rip);
            } else {
                COUNT.store(MAX_SAMPLES, Ordering::Relaxed);
                DROPPED.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// `(start, end, label)` for every emitted body, newest last. Registered by
    /// the JIT at install time; buffers are mmap'd and never move, so a range
    /// stays valid until its body is evicted, and an evicted body's range is
    /// simply never sampled again.
    static RANGES: std::sync::Mutex<Vec<(u64, u64, String)>> = std::sync::Mutex::new(Vec::new());

    /// Record one emitted body's address range. Cheap and only called when PC
    /// mode armed, so an ordinary run never builds the table.
    ///
    /// Only the JIT calls this, so a build without one leaves it unused.
    #[allow(dead_code)]
    pub(crate) fn register(start: u64, len: usize, label: String) {
        if THREAD.load(Ordering::Relaxed) == 0 {
            return;
        }
        if let Ok(mut r) = RANGES.lock() {
            r.push((start, start + len as u64, label));
        }
    }

    /// `SYMBOL_INFO` (x86-64): `SizeOfStruct` 88 at 0x00, `MaxNameLen` at 0x50,
    /// and the NUL-terminated `Name` starting at 0x54.
    const SYMBOL_INFO_SIZE: usize = 88;
    const SYMBOL_MAXNAME_OFF: usize = 0x50;
    const SYMBOL_NAME_OFF: usize = 0x54;
    const SYMBOL_NAME_CAP: usize = 512;
    /// `SYMOPT_UNDNAME | SYMOPT_DEFERRED_LOADS`.
    const SYMOPT: u32 = 0x0000_0002 | 0x0000_0004;

    /// Resolve one address to a symbol name, or `None`.
    ///
    /// Rust symbols come back mangled; the mangling still carries the crate and
    /// module path, which is what makes the profile readable, so no demangler
    /// is pulled in for it.
    fn symbolicate(addr: u64) -> Option<String> {
        let mut buf = [0u8; SYMBOL_INFO_SIZE + SYMBOL_NAME_CAP];
        let mut disp: u64 = 0;
        // SAFETY: `buf` is larger than SYMBOL_INFO plus the name capacity we
        // declare, and dbghelp writes at most that much. Called only after
        // sampling has stopped, on the reporting thread.
        unsafe {
            std::ptr::write(buf.as_mut_ptr() as *mut u32, SYMBOL_INFO_SIZE as u32);
            std::ptr::write(
                buf.as_mut_ptr().add(SYMBOL_MAXNAME_OFF) as *mut u32,
                SYMBOL_NAME_CAP as u32 - 1,
            );
            if SymFromAddr(GetCurrentProcess(), addr, &mut disp, buf.as_mut_ptr()) == 0 {
                return None;
            }
            let name = buf.as_ptr().add(SYMBOL_NAME_OFF);
            let mut n = 0usize;
            while n < SYMBOL_NAME_CAP - 1 && *name.add(n) != 0 {
                n += 1;
            }
            let bytes = std::slice::from_raw_parts(name, n);
            Some(String::from_utf8_lossy(bytes).into_owned())
        }
    }

    /// `(label, samples, percent)` sorted by samples, plus the total.
    ///
    /// Samples outside every registered range are the interpreter, the runtime
    /// helpers, the allocator and the OS — real time, and deliberately kept as
    /// one visible bucket rather than dropped, so the compiled-code shares are
    /// read against the whole run and not against each other.
    pub(crate) fn report() -> (Vec<(String, u64, f64)>, u64) {
        let total = COUNT.load(Ordering::Relaxed).min(MAX_SAMPLES);
        let buf = BUF.load(Ordering::Acquire);
        if buf == 0 || total == 0 {
            return (Vec::new(), 0);
        }
        let mut ranges = match RANGES.lock() {
            Ok(r) => r.clone(),
            Err(_) => return (Vec::new(), 0),
        };
        ranges.sort_by_key(|r| r.0);
        let mut tally: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
        let mut sym_cache: std::collections::HashMap<u64, Option<String>> =
            std::collections::HashMap::new();
        // SAFETY: initialising the symbol handler for our own process, once,
        // after sampling has stopped. A failure just leaves addresses unnamed.
        unsafe {
            SymSetOptions(SYMOPT);
            SymInitialize(GetCurrentProcess(), std::ptr::null(), 1);
        }
        let module = main_module();
        let mut outside = 0u64;
        for i in 0..total {
            // SAFETY: `i < total <= MAX_SAMPLES` and the buffer is leaked.
            let rip = unsafe { std::ptr::read((buf as *const u64).add(i)) };
            let at = ranges.partition_point(|r| r.0 <= rip);
            if at > 0 && rip < ranges[at - 1].1 {
                *tally.entry(ranges[at - 1].2.clone()).or_insert(0) += 1;
                continue;
            }
            // Not in emitted code: a runtime helper, the allocator, the
            // interpreter, or the OS. Name it if the symbols allow.
            let name = match sym_cache.entry(rip) {
                std::collections::hash_map::Entry::Occupied(e) => e.get().clone(),
                std::collections::hash_map::Entry::Vacant(e) => {
                    let n = symbolicate(rip);
                    e.insert(n.clone());
                    n
                }
            };
            match name {
                Some(n) => *tally.entry(n).or_insert(0) += 1,
                None if module.is_some_and(|(b, n)| rip >= b && rip < b + n) => {
                    // Report where in the image it landed. `tools/pcmap.py`
                    // turns these into function names using the linker map.
                    let off = rip - module.unwrap().0;
                    *tally.entry(format!("zipp.exe+0x{off:x}")).or_insert(0) += 1
                }
                // Not in any module: emitted code whose buffer nothing
                // registered, or a helper thunk.
                None => outside += 1,
            }
        }
        let mut rows: Vec<(String, u64, f64)> = tally
            .into_iter()
            .map(|(k, c)| (k, c, c as f64 * 100.0 / total as f64))
            .collect();
        if outside > 0 {
            rows.push((
                "<emitted code, unregistered buffer>".to_string(),
                outside,
                outside as f64 * 100.0 / total as f64,
            ));
        }
        rows.sort_by(|a, b| b.1.cmp(&a.1));
        // `ZIPP_PROF_PC_DUMP=<path>` writes every sample as a module-relative
        // offset (or an absolute address when it is outside the image), one
        // per line, so a whole profile can be bucketed offline rather than
        // through this 25-row console view.
        if let Some(path) = std::env::var_os("ZIPP_PROF_PC_DUMP") {
            let mut out = String::with_capacity(total * 12);
            for i in 0..total {
                // SAFETY: as above, `i < total <= MAX_SAMPLES`.
                let rip = unsafe { std::ptr::read((buf as *const u64).add(i)) };
                match module {
                    Some((b, n)) if rip >= b && rip < b + n => {
                        out.push_str(&format!("{:x}\n", rip - b))
                    }
                    _ => out.push_str(&format!("abs:{rip:x}\n")),
                }
            }
            let _ = std::fs::write(path, out);
        }
        (rows, total as u64)
    }
}

/// PC mode is x86-64 Windows only; everywhere else these are no-ops so the
/// call sites need no `cfg`.
#[cfg(not(all(windows, target_arch = "x86_64", not(feature = "safe-sandbox"))))]
pub(crate) mod pc {
    pub(crate) fn arm_current_thread() {}
    pub(crate) fn sample() {}
    /// Only the JIT registers ranges, so a build without one never calls this.
    #[allow(dead_code)]
    pub(crate) fn register(_start: u64, _len: usize, _label: String) {}
    pub(crate) fn report() -> (Vec<(String, u64, f64)>, u64) {
        (Vec::new(), 0)
    }
}
