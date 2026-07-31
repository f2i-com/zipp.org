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

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};

/// Subsystem tags. `Interp` is the resting state: time attributed to it is time
/// in the dispatch loop or in compiled regions, i.e. running JavaScript rather
/// than sitting in an engine service.
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
    Alloc = 7,
    JitCompile = 8,
    Sort = 9,
    /// Executing NATIVE compiled code (a Tier A/C function body or an OSR
    /// region). Distinct from `Interp` because the two have opposite fixes:
    /// time in `Interp` means not enough code is COMPILED, time in `Jit` means
    /// compiled code is SLOW (M4's register file and per-op boxing).
    Jit = 10,
}

const N_PHASES: usize = 11;

const NAMES: [&str; N_PHASES] = [
    "interp",
    "gc",
    "regex-exec",
    "json-parse",
    "json-stringify",
    "string-ops",
    "prop-slow",
    "alloc",
    "jit-compile",
    "sort",
    "jit-native",
];

static CURRENT: AtomicU8 = AtomicU8::new(Phase::Interp as u8);
static COUNTS: [AtomicU64; N_PHASES] = [
    AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0),
    AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0),
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
        _ => {
            let v = std::env::var_os("ZIPP_PROF").is_some() as u8;
            ON.store(v, Ordering::Relaxed);
            if v == 1 {
                start();
            }
            v == 1
        }
    }
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
            let pct = if total == 0 { 0.0 } else { c as f64 * 100.0 / total as f64 };
            (NAMES[i], c, pct)
        })
        .filter(|(_, c, _)| *c > 0)
        .collect();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    (v, total)
}
