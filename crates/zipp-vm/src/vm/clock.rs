//! The engine's two clocks, behind one platform boundary — and one host
//! override.
//!
//! The VM reads time in exactly two ways: a MONOTONIC one (for
//! `performance.now()`, `setTimeout` deadlines and `Atomics.wait` timeouts)
//! and a WALL-CLOCK one ([`now_epoch_ms`] / [`now_epoch_ns`], for `Date` and
//! `Temporal`). Both are overridable: [`install`] hands the engine one host
//! function per clock, and from then on every `Date.now()`, `new Date()`,
//! `Temporal.Now` and `performance.now()` read — on EVERY target — goes
//! through them. That is what a host running untrusted JS needs to make time
//! deterministic (a blockchain does): the compiler intrinsifies the literal
//! `Date.now()` / `new Date(...)` / `performance.now()` shapes to clock
//! opcodes without consulting the JS bindings, so shadowing `Date` from
//! script does not move the engine's clock — installing one does.
//!
//! With nothing installed the behavior is exactly what it always was: on
//! native targets both clocks come straight from `std::time`. The override
//! costs one relaxed atomic load and a well-predicted branch per read, which
//! matters because these sit on `Date.now()`.
//!
//! Two reads deliberately keep the REAL clock even when one is installed:
//! `setTimeout` deadlines and `Atomics.wait` timeouts ([`Instant`] on native
//! targets). They back condvar waits, which sleep in real time — a fixed
//! installed clock would leave every deadline permanently in the future and
//! park the event loop forever. A deterministic-time host should treat
//! timers, like I/O, as outside the deterministic envelope.
//!
//! `wasm32-unknown-unknown` has no clock at all: `Instant::now()` and
//! `SystemTime::now()` are `unimplemented!()` stubs that PANIC (std's
//! `sys/time/unsupported.rs`). Since `Vm::new` records a start reading, an
//! un-shimmed engine traps the moment it is constructed, before running a
//! line of JS. So on wasm the host MUST [`install`] before constructing a VM,
//! the engine holds a safe, non-panicking default until it does — and there,
//! with nothing real to fall back to, [`Instant`] rides the installed
//! monotonic clock as well.
//!
//! A hook rather than a `js-sys` dependency: the engine should not have to
//! know that its wasm host is a browser (a WASI or embedder-supplied clock
//! works the same way), and a `[target.wasm32]` dependency on wasm-bindgen
//! would decide that for every future host. `zipp-wasm` installs `Date.now` /
//! `performance.now` from its `#[wasm_bindgen(start)]`, so a browser embed is
//! never running on the fallback.

use core::sync::atomic::{AtomicUsize, Ordering};

/// `setTimeout` deadlines and `Atomics.wait` timeouts: the REAL monotonic
/// clock, even when a host clock is installed (see the module docs).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use std::time::Instant;

#[cfg(target_arch = "wasm32")]
pub(crate) use wasm_instant::Instant;

/// The installed clocks, as `fn() -> f64` pointers stored as `usize` (0 =
/// not installed). `wasm32-unknown-unknown` is single-threaded, and on native
/// targets a racing pair of `install` calls simply resolves last-wins, so a
/// relaxed ordering is all either needs.
static EPOCH_MS: AtomicUsize = AtomicUsize::new(0);
static MONO_MS: AtomicUsize = AtomicUsize::new(0);

/// Install the host's clocks. `epoch_ms` returns milliseconds since the Unix
/// epoch (`Date.now`); `mono_ms` returns a monotonically non-decreasing
/// millisecond count with an arbitrary zero (`performance.now`).
///
/// Honored on EVERY target from the moment of the call — including by the
/// intrinsified `Date.now()` / `new Date()` / `performance.now()` opcodes,
/// which never consult the JS bindings. The exceptions are `setTimeout`
/// deadlines and `Atomics.wait` timeouts, which keep the real monotonic
/// clock (see the module docs). With nothing installed, native targets read
/// `std::time` exactly as before.
///
/// Call BEFORE constructing the first `Vm`: `performance.now()` is relative
/// to a monotonic reading taken in `Vm::new`, so installing between
/// construction and use would measure from a different clock's zero. Safe to
/// call more than once; the last call wins. Required on wasm32, which has no
/// fallback clock at all.
///
/// A stateful clock fits the `fn`-pointer shape through a process-global,
/// which is how a host whose time IS mutable state (a block height, a
/// virtual tick) supplies it:
///
/// ```ignore
/// static VIRTUAL_MS: AtomicU64 = AtomicU64::new(0);
/// zipp_vm::install_clock(
///     || VIRTUAL_MS.load(Ordering::Relaxed) as f64,
///     || VIRTUAL_MS.load(Ordering::Relaxed) as f64,
/// );
/// ```
pub fn install(epoch_ms: fn() -> f64, mono_ms: fn() -> f64) {
    EPOCH_MS.store(epoch_ms as usize, Ordering::Relaxed);
    MONO_MS.store(mono_ms as usize, Ordering::Relaxed);
}

/// The host function installed in `slot`, or `None`.
#[inline]
fn installed(slot: &AtomicUsize) -> Option<fn() -> f64> {
    let f = slot.load(Ordering::Relaxed);
    if f == 0 {
        return None;
    }
    // SAFETY: `f` is non-zero, so it was written by `install` from a
    // `fn() -> f64`, and nothing ever stores any other kind of value here.
    Some(unsafe { core::mem::transmute::<usize, fn() -> f64>(f) })
}

/// Milliseconds since the Unix epoch — the value behind `Date.now()`.
#[inline]
pub(crate) fn now_epoch_ms() -> f64 {
    if let Some(f) = installed(&EPOCH_MS) {
        return f();
    }
    platform_epoch_ms()
}

/// Nanoseconds since the Unix epoch — `Temporal.Now`'s resolution.
#[inline]
pub(crate) fn now_epoch_ns() -> i128 {
    if installed(&EPOCH_MS).is_some() {
        // `Date.now()` is millisecond-resolution, so this is exact to the ms
        // and zero below it — the same precision a browser gives Temporal.
        return (now_epoch_ms() as i128) * 1_000_000;
    }
    platform_epoch_ns()
}

/// Milliseconds on the monotonic clock, from an arbitrary zero —
/// `performance.now()`'s source. With an installed clock this is the host's
/// own reading; without one it is real monotonic time.
#[inline]
pub(crate) fn now_mono_ms() -> f64 {
    if let Some(f) = installed(&MONO_MS) {
        return f();
    }
    platform_mono_ms()
}

// ── platform defaults: the no-clock-installed path ────────────────────────

#[cfg(not(target_arch = "wasm32"))]
#[inline]
fn platform_epoch_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as f64)
        .unwrap_or(0.0)
}

#[cfg(not(target_arch = "wasm32"))]
#[inline]
fn platform_epoch_ns() -> i128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as i128)
        .unwrap_or(0)
}

#[cfg(not(target_arch = "wasm32"))]
#[inline]
fn platform_mono_ms() -> f64 {
    // Rebased onto a process-lifetime zero so the f64 keeps sub-millisecond
    // resolution no matter how long the host has been up — a raw
    // since-boot microsecond count stops being exact in f64 after a while.
    static BASE: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    BASE.get_or_init(std::time::Instant::now)
        .elapsed()
        .as_secs_f64()
        * 1000.0
}

/// The wasm wall-clock default: 0.0 (the Unix epoch) — wrong, but inert: it
/// cannot panic and cannot wander. Only reachable with no host clock
/// installed.
#[cfg(target_arch = "wasm32")]
#[inline]
fn platform_epoch_ms() -> f64 {
    0.0
}

#[cfg(target_arch = "wasm32")]
#[inline]
fn platform_epoch_ns() -> i128 {
    0
}

/// The wasm monotonic default: a counter advanced 1µs per read. Keeps the
/// `Instant` ordering invariants (never goes backwards, `now() >= earlier`)
/// so timer and deadline logic stays coherent, without pretending to measure
/// anything. Only reachable with no host clock installed.
#[cfg(target_arch = "wasm32")]
fn platform_mono_ms() -> f64 {
    static FALLBACK_NS: AtomicUsize = AtomicUsize::new(0);
    FALLBACK_NS.fetch_add(1_000, Ordering::Relaxed) as f64 / 1.0e6
}

#[cfg(target_arch = "wasm32")]
mod wasm_instant {
    use std::ops::{Add, Sub};
    use std::time::Duration;

    /// A monotonic point in time, as nanoseconds from an arbitrary zero.
    ///
    /// Integer nanos rather than `f64` millis so the derived `Ord` is a total
    /// order: the timer queue sorts by deadline (`sort_by_key`) and takes
    /// `min()` across pending waiters, neither of which `f64` can do. Unlike
    /// the native `Instant` this rides the installed monotonic clock — wasm
    /// has no other.
    #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
    pub(crate) struct Instant(u64);

    impl Instant {
        #[inline]
        pub(crate) fn now() -> Self {
            Instant((super::now_mono_ms() * 1.0e6) as u64)
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
