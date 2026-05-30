//! Tier-2 JIT cache: per-function compile state + dispatch lookup.
//!
//! Mirrors the [`DynasmJit`](crate::djit::DynasmJit) shape so the VM
//! call site can treat both tiers symmetrically. Key differences:
//!
//! * Tier-2 compile is more expensive than tier-1 (bytecode → IR →
//!   passes → regalloc → emit), so functions only attempt tier-2
//!   after surviving tier-1 for `TIER2_THRESHOLD` additional calls.
//! * Compile failures are permanent: the function is added to a
//!   blacklist and the VM never retries tier-2 for that key. Tier-1
//!   continues serving its native code.
//! * Tier-2 functions always take the 4-arg `vm_ptr` ABI — the emit
//!   layer's runtime-helper path needs a valid VM pointer even when
//!   the callee body doesn't issue any runtime calls. This matches
//!   tier-1's `execute_ptr_with_vm` shape.

#![cfg(all(feature = "djit", target_arch = "x86_64"))]

use std::collections::{HashMap, HashSet};
use std::mem;

use super::emit::EmittedFunction;

/// Number of times a tier-1-compiled function must be called before
/// the VM attempts tier-2 promotion. Tunable; 100 picked to hide
/// tier-2 compile cost behind real work.
///
/// Any `cfg(test)` caller can override by setting
/// [`Tier2Jit::set_threshold`] before the first tier-2 call so test
/// suites don't need 100+ invocations to exercise promotion.
pub const TIER2_THRESHOLD: u32 = 100;

/// One tier-2-compiled function plus the metadata the dispatcher
/// needs to call it correctly.
pub struct Tier2Function {
    emitted: EmittedFunction,
}

impl Tier2Function {
    pub fn entry_ptr(&self) -> *const u8 {
        self.emitted.entry_ptr()
    }
}

/// Global tier-2 cache. Lives inside the VM and is consulted at
/// every call site before tier-1 dispatch.
pub struct Tier2Jit {
    /// Successfully-compiled functions, keyed by the same
    /// `instructions as usize` pointer tier-1 uses.
    compiled: HashMap<usize, Tier2Function>,
    /// Per-key call counts *after* tier-1 compiled. Counter reaching
    /// `threshold` triggers the compile attempt.
    call_counts: HashMap<usize, u32>,
    /// Keys whose tier-2 compile returned an error. Never retried.
    blacklist: HashSet<usize>,
    /// The most recent compile; held until the next top-level
    /// `run_register` call to avoid mid-recursion execution of a
    /// buffer that was just materialised. Matches the tier-1
    /// deferral scheme.
    deferred: Option<usize>,
    /// Override for [`TIER2_THRESHOLD`], for test harnesses that
    /// want to trigger promotion on a handful of calls.
    threshold: u32,
}

impl Default for Tier2Jit {
    fn default() -> Self {
        Self::new()
    }
}

impl Tier2Jit {
    pub fn new() -> Self {
        Self {
            compiled: HashMap::new(),
            call_counts: HashMap::new(),
            blacklist: HashSet::new(),
            deferred: None,
            threshold: TIER2_THRESHOLD,
        }
    }

    /// Adjust the promotion threshold. Primarily used by tests that
    /// need to exercise the tier-2 path without executing 100+
    /// warm-up calls.
    pub fn set_threshold(&mut self, threshold: u32) {
        self.threshold = threshold;
    }

    /// Is `func_key` compiled and ready to invoke? Returns the raw
    /// entry pointer if so, else `None`. The deferred-key carve-out
    /// suppresses the pointer during the same run_register that
    /// triggered its compile.
    pub fn get_fn_ptr(&self, func_key: usize) -> Option<*const u8> {
        if self.deferred == Some(func_key) {
            return None;
        }
        self.compiled.get(&func_key).map(|f| f.entry_ptr())
    }

    /// Record a call against `func_key` for the promotion counter.
    /// Returns `true` iff the call caused the counter to hit the
    /// threshold — the caller then invokes [`Self::try_compile`].
    ///
    /// Functions already compiled or blacklisted are ignored.
    pub fn record_call(&mut self, func_key: usize) -> bool {
        if self.compiled.contains_key(&func_key) {
            return false;
        }
        if self.blacklist.contains(&func_key) {
            return false;
        }
        let count = self.call_counts.entry(func_key).or_insert(0);
        *count += 1;
        *count == self.threshold
    }

    /// Attempt tier-2 compilation. On success the function becomes
    /// available at the next top-level run_register (via the
    /// deferred-key mechanism). On failure the key is blacklisted
    /// and never retried.
    ///
    /// Returns `true` if compile succeeded. The caller typically
    /// wires `set_deferred(func_key)` on a successful compile so
    /// the current recursion completes on tier-1 / tier-0.
    pub fn try_compile(
        &mut self,
        func_key: usize,
        instructions: &[u8],
        constants: &[crate::object::Object],
        num_bytecode_regs: u16,
        num_parameters: u16,
    ) -> bool {
        if self.compiled.contains_key(&func_key) || self.blacklist.contains(&func_key) {
            return false;
        }
        // Pipeline wants `Rc<Vec<Object>>` so passes can hold the
        // constants table across IR rewrites. Materialise a fresh
        // Rc here — callers pass a borrowed slice to match the
        // dispatcher's existing types and the cost is a one-time
        // Vec clone per successful compile.
        let constants_rc = std::rc::Rc::new(constants.to_vec());
        match super::try_compile(instructions, constants_rc, num_bytecode_regs, num_parameters) {
            Ok(emitted) => {
                self.compiled
                    .insert(func_key, Tier2Function { emitted });
                true
            }
            Err(_) => {
                self.blacklist.insert(func_key);
                false
            }
        }
    }

    /// Mark a just-compiled function as deferred so the VM skips it
    /// until the next top-level run_register. Called by `record_call`
    /// ↦ `try_compile` flows.
    pub fn set_deferred(&mut self, func_key: usize) {
        self.deferred = Some(func_key);
    }

    /// Clear the deferred marker. Called at the start of each
    /// top-level run_register so previously-compiled code becomes
    /// visible on the next dispatch round.
    pub fn clear_deferred(&mut self) {
        self.deferred = None;
    }

    /// Is `func_key` on the blacklist? Exposed for diagnostics and
    /// tests.
    pub fn is_blacklisted(&self, func_key: usize) -> bool {
        self.blacklist.contains(&func_key)
    }

    /// Add `func_key` to the blacklist and drop any cached compile.
    /// Called by the VM dispatch site when tier-2 code soft-deopts
    /// mid-execution: the speculation failed, so we don't want to
    /// retry the same emit path on the next call.
    pub fn blacklist(&mut self, func_key: usize) {
        self.blacklist.insert(func_key);
        self.compiled.remove(&func_key);
        self.call_counts.remove(&func_key);
    }

    /// Number of tier-2-compiled functions currently cached.
    /// Telemetry only; dispatch uses [`Self::get_fn_ptr`].
    pub fn compiled_count(&self) -> usize {
        self.compiled.len()
    }

    /// Run an emitted tier-2 function. Uniform Win64 ABI:
    /// `(regs, consts, globals, vm_ptr) -> Value::bits()`.
    ///
    /// # Safety
    ///
    /// `fn_ptr` must have come from [`Self::get_fn_ptr`] on this
    /// instance. All four pointers must remain valid for the
    /// duration of the call.
    pub unsafe fn execute_ptr(
        fn_ptr: *const u8,
        regs: *mut u64,
        consts: *const u64,
        globals: *mut u64,
        vm_ptr: *mut u8,
    ) -> u64 {
        let f: extern "win64" fn(*mut u64, *const u64, *mut u64, *mut u8) -> u64 =
            mem::transmute(fn_ptr);
        f(regs, consts, globals, vm_ptr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_cache_starts_empty() {
        let cache = Tier2Jit::new();
        assert_eq!(cache.compiled_count(), 0);
        assert!(cache.get_fn_ptr(0x1234).is_none());
        assert!(!cache.is_blacklisted(0x1234));
    }

    #[test]
    fn record_call_hits_threshold() {
        let mut cache = Tier2Jit::new();
        cache.set_threshold(3);
        assert_eq!(cache.record_call(1), false); // count=1
        assert_eq!(cache.record_call(1), false); // count=2
        assert_eq!(cache.record_call(1), true); // count=3 → threshold
        assert_eq!(cache.record_call(1), false); // past threshold → false
    }

    #[test]
    fn blacklisted_key_stops_recording() {
        let mut cache = Tier2Jit::new();
        cache.set_threshold(2);
        cache.blacklist(7);
        // Already blacklisted → record_call is a no-op
        assert_eq!(cache.record_call(7), false);
        assert_eq!(cache.record_call(7), false);
    }

    #[test]
    fn blacklist_drops_cached_entry() {
        let mut cache = Tier2Jit::new();
        // Record a call + trip threshold would normally compile;
        // here we just confirm blacklist clears state.
        cache.set_threshold(3);
        cache.record_call(42);
        cache.record_call(42);
        cache.blacklist(42);
        assert!(cache.is_blacklisted(42));
        // record_call now returns false because blacklist wins.
        assert_eq!(cache.record_call(42), false);
    }

    #[test]
    fn deferred_hides_pointer() {
        let mut cache = Tier2Jit::new();
        cache.deferred = Some(42);
        // Even if we faked a compiled entry, deferred hides it.
        assert!(cache.get_fn_ptr(42).is_none());
        cache.clear_deferred();
        // Still None because nothing's compiled, but the deferred
        // carve-out doesn't block it anymore.
        assert!(cache.get_fn_ptr(42).is_none());
    }

    #[test]
    fn failed_compile_blacklists_and_never_retries() {
        let mut cache = Tier2Jit::new();
        // Intentionally malformed bytecode — a bare 0xff byte isn't a
        // valid opcode so translate bails out.
        let result = cache.try_compile(
            123,
            &[0xff],
            &[],
            0,
            0,
        );
        assert_eq!(result, false);
        assert!(cache.is_blacklisted(123));
        // Second attempt doesn't try again — function stays
        // blacklisted and try_compile returns false cheaply.
        let result2 = cache.try_compile(
            123,
            &[0xff],
            &[],
            0,
            0,
        );
        assert_eq!(result2, false);
        assert!(cache.is_blacklisted(123));
    }
}
