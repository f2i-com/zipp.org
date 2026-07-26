//! The engine's two clocks, behind one platform boundary.
//!
//! The VM reads time in exactly two ways: a MONOTONIC one ([`Instant`], for
//! `performance.now()`, `setTimeout` deadlines and `Atomics.wait` timeouts) and
//! a WALL-CLOCK one ([`now_epoch_ms`] / [`now_epoch_ns`], for `Date` and
//! `Temporal`). On every ordinary target both come straight from `std::time`
//! and this module is a re-export — the native path compiles to exactly the
//! code it replaced, which matters because these sit on `Date.now()`.
//!
//! `wasm32-unknown-unknown` has no clock at all: `Instant::now()` and
//! `SystemTime::now()` are `unimplemented!()` stubs that PANIC (std's
//! `sys/time/unsupported.rs`). Since `Vm::new` records a start instant, an
//! un-shimmed engine traps the moment it is constructed, before running a line
//! of JS. So on wasm the two clocks are function pointers the host installs
//! with [`install`], and the engine holds a safe, non-panicking default until
//! it does.
//!
//! A hook rather than a `js-sys` dependency: the engine should not have to know
//! that its wasm host is a browser (a WASI or embedder-supplied clock works the
//! same way), and a `[target.wasm32]` dependency on wasm-bindgen would decide
//! that for every future host. `zipp-wasm` installs `Date.now` /
//! `performance.now` from its `#[wasm_bindgen(start)]`, so a browser embed is
//! never running on the fallback.

#[cfg(not(target_arch = "wasm32"))]
pub(crate) use std::time::Instant;

/// Milliseconds since the Unix epoch — the value behind `Date.now()`.
#[cfg(not(target_arch = "wasm32"))]
#[inline]
pub(crate) fn now_epoch_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as f64)
        .unwrap_or(0.0)
}

/// Nanoseconds since the Unix epoch — `Temporal.Now`'s resolution.
#[cfg(not(target_arch = "wasm32"))]
#[inline]
pub(crate) fn now_epoch_ns() -> i128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as i128)
        .unwrap_or(0)
}

/// Accepted and ignored: this target has a real clock, and `std::time` is
/// already better than anything a host could hand us. Present on every target
/// so an embedder can call it unconditionally rather than `cfg`-ing its own
/// startup path.
#[cfg(not(target_arch = "wasm32"))]
pub fn install(_epoch_ms: fn() -> f64, _mono_ms: fn() -> f64) {}

#[cfg(target_arch = "wasm32")]
pub(crate) use wasm_clock::{now_epoch_ms, now_epoch_ns, Instant};

#[cfg(target_arch = "wasm32")]
pub use wasm_clock::install;

#[cfg(target_arch = "wasm32")]
mod wasm_clock {
    use core::sync::atomic::{AtomicUsize, Ordering};
    use std::ops::{Add, Sub};
    use std::time::Duration;

    /// The installed clocks, as `fn() -> f64` pointers stored as `usize` (0 =
    /// not installed). `wasm32-unknown-unknown` is single-threaded, so the
    /// ordering only has to keep the store from being reordered past a load.
    static EPOCH_MS: AtomicUsize = AtomicUsize::new(0);
    static MONO_MS: AtomicUsize = AtomicUsize::new(0);

    /// Install the host's clocks. `epoch_ms` returns milliseconds since the
    /// Unix epoch (`Date.now`); `mono_ms` returns a monotonically
    /// non-decreasing millisecond count with an arbitrary zero
    /// (`performance.now`). Safe to call more than once; the last call wins.
    pub fn install(epoch_ms: fn() -> f64, mono_ms: fn() -> f64) {
        EPOCH_MS.store(epoch_ms as usize, Ordering::Relaxed);
        MONO_MS.store(mono_ms as usize, Ordering::Relaxed);
    }

    /// Fallback monotonic source: a counter advanced 1µs per read. Keeps the
    /// `Instant` ordering invariants (never goes backwards, `now() >= earlier`)
    /// so timer and deadline logic stays coherent, without pretending to
    /// measure anything. Only reachable when no host clock is installed.
    static FALLBACK_NS: AtomicUsize = AtomicUsize::new(0);

    #[inline]
    fn mono_ms_now() -> f64 {
        let f = MONO_MS.load(Ordering::Relaxed);
        if f == 0 {
            // SAFETY-free path: no host clock, so advance the counter instead.
            let ns = FALLBACK_NS.fetch_add(1_000, Ordering::Relaxed) as f64;
            return ns / 1.0e6;
        }
        // SAFETY: `f` is non-zero, so it was written by `install` from a
        // `fn() -> f64`, and nothing ever stores any other kind of value here.
        let f: fn() -> f64 = unsafe { core::mem::transmute::<usize, fn() -> f64>(f) };
        f()
    }

    /// `Date.now()`. 0.0 (the Unix epoch) when no host clock is installed —
    /// wrong, but inert: it cannot panic and cannot wander.
    #[inline]
    pub(crate) fn now_epoch_ms() -> f64 {
        let f = EPOCH_MS.load(Ordering::Relaxed);
        if f == 0 {
            return 0.0;
        }
        // SAFETY: as in `mono_ms_now` — non-zero means `install` wrote it.
        let f: fn() -> f64 = unsafe { core::mem::transmute::<usize, fn() -> f64>(f) };
        f()
    }

    #[inline]
    pub(crate) fn now_epoch_ns() -> i128 {
        // `Date.now()` is millisecond-resolution, so this is exact to the ms
        // and zero below it — the same precision a browser gives Temporal.
        (now_epoch_ms() as i128) * 1_000_000
    }

    /// A monotonic point in time, as nanoseconds from an arbitrary zero.
    ///
    /// Integer nanos rather than `f64` millis so the derived `Ord` is a total
    /// order: the timer queue sorts by deadline (`sort_by_key`) and takes
    /// `min()` across pending waiters, neither of which `f64` can do.
    #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
    pub(crate) struct Instant(u64);

    impl Instant {
        #[inline]
        pub(crate) fn now() -> Self {
            Instant((mono_ms_now() * 1.0e6) as u64)
        }

        #[inline]
        pub(crate) fn elapsed(&self) -> Duration {
            Instant::now() - *self
        }
    }

    impl Add<Duration> for Instant {
        type Output = Instant;
        #[inline]
        fn add(self, rhs: Duration) -> Instant {
            Instant(self.0.saturating_add(rhs.as_nanos() as u64))
        }
    }

    impl Sub<Instant> for Instant {
        type Output = Duration;
        /// Saturating, like `std::time::Instant`'s `duration_since` on a later
        /// operand: a negative interval becomes zero rather than wrapping.
        #[inline]
        fn sub(self, rhs: Instant) -> Duration {
            Duration::from_nanos(self.0.saturating_sub(rhs.0))
        }
    }
}
