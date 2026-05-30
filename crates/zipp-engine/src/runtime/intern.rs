//! Thread-local string interner.
//!
//! Each thread gets its own [`Interner`] instance. Symbol IDs are unique
//! within a thread but may differ across threads — that's fine because a
//! Zipp [`crate::vm::VM`] is always owned by a single thread.
//!
//! This used to be `static mut INTERNER_PTR: *mut Interner` accessed from
//! any thread under an implicit "only one thread uses it at a time" rule.
//! That rule held in production (one VM per thread) but broke under
//! `cargo test`'s parallel runner, where multiple tests would race on
//! `intern()` and produce intermittent `Rc` UB / deadlocks. Putting the
//! interner behind a `thread_local!` removes the cross-thread aliasing
//! entirely.
//!
//! The `Rc<str>`-based representation is intentional: the entire VM uses
//! `Rc` (not `Arc`), so nothing crosses threads and the refcount stays
//! in a single CPU cache line. Moving to `Arc` would be a whole-crate
//! refactor for no measurable benefit in the single-threaded VM.

use rustc_hash::FxHashMap;
use std::cell::UnsafeCell;
use std::rc::Rc;

/// Hard upper bound on how many unique symbols a single thread's
/// interner will hold before the VM refuses to keep running. Without a
/// cap, a script that generates unique property names in a loop
/// (`obj['k' + i] = 1` ad infinitum) can grow the per-thread interner
/// without bound — the entries never leave, so `max_heap_bytes` can't
/// rein them in.
///
/// 1 M symbols is comfortably more than any realistic script creates;
/// at ~60 bytes/entry for `Rc<str>` + `FxHashMap` bookkeeping the cap
/// corresponds to roughly 60 MiB of intern-table memory, same ballpark
/// as the default `max_heap_bytes`.
pub const MAX_INTERNED_SYMBOLS: usize = 1_000_000;

pub struct Interner {
    strings: Vec<Rc<str>>,
    ids: FxHashMap<Rc<str>, u32>,
    /// Pointer-keyed fast lookup for `intern_rc`. Only stores pointers
    /// whose underlying allocation we own (i.e. a `Rc<str>` is also in
    /// `strings`/`ids`). This guarantees the pointer stays valid forever
    /// and can never be reused by a different allocation — the earlier
    /// version cached caller-supplied pointers which could get freed
    /// and recycled, producing stale entries that mapped one content to
    /// another's symbol ID.
    ptr_ids: FxHashMap<usize, u32>,
    /// Direct-mapped 1-cycle cache fronting `ptr_ids`. Hot loops that
    /// repeatedly intern the same canonical `Rc<str>` (interpreter
    /// Map.set/get with heap-string keys, hash key normalisation, etc.)
    /// hit here and skip the FxHashMap probe entirely. Slot stores
    /// `(ptr, sym_id)`; sentinel pointer 0 means empty.
    ptr_cache: Box<[(usize, u32); PTR_CACHE_SIZE]>,
}

const PTR_CACHE_SIZE: usize = 256;

impl Interner {
    fn new() -> Self {
        Self {
            strings: Vec::with_capacity(4096),
            ids: FxHashMap::with_capacity_and_hasher(4096, Default::default()),
            ptr_ids: FxHashMap::with_capacity_and_hasher(4096, Default::default()),
            ptr_cache: Box::new([(0usize, 0u32); PTR_CACHE_SIZE]),
        }
    }

    #[inline(always)]
    fn intern(&mut self, s: &str) -> u32 {
        if let Some(&id) = self.ids.get(s) {
            return id;
        }
        let rc: Rc<str> = Rc::from(s);
        let id = self.strings.len() as u32;
        // Record the pointer for the canonical Rc — strings + ids each
        // hold one Rc reference, so the allocation outlives the interner.
        let ptr = Rc::as_ptr(&rc) as *const () as usize;
        self.strings.push(Rc::clone(&rc));
        self.ids.insert(rc, id);
        self.ptr_ids.insert(ptr, id);
        // Mix in u64 so wasm32's 32-bit `usize` doesn't truncate the
        // golden-ratio hash constant (`0x9E37…7C15` is 64 bits and the
        // literal would otherwise saturate to 0x7F4A7C15 there).
        let slot = ((ptr as u64).wrapping_mul(0x9E3779B97F4A7C15) >> 32) as usize
            & (PTR_CACHE_SIZE - 1);
        unsafe { *self.ptr_cache.get_unchecked_mut(slot) = (ptr, id); }
        id
    }

    #[inline(always)]
    fn intern_rc(&mut self, s: &Rc<str>) -> u32 {
        // Pointer fast path. A hit means the caller is holding a clone
        // of one of our canonical Rcs; the pointer is stable because
        // we also hold a strong reference in `strings`/`ids`.
        let ptr = Rc::as_ptr(s) as *const () as usize;
        // L1: direct-mapped 256-slot cache. Most hot loops repeatedly
        // intern the same handful of canonical Rcs, so a single load +
        // compare per call replaces the FxHashMap probe below.
        let slot = ((ptr as u64).wrapping_mul(0x9E3779B97F4A7C15) >> 32) as usize
            & (PTR_CACHE_SIZE - 1);
        let (cp, cs) = unsafe { *self.ptr_cache.get_unchecked(slot) };
        if cp == ptr {
            return cs;
        }
        // L2: full FxHashMap.
        if let Some(&id) = self.ptr_ids.get(&ptr) {
            // Promote into the L1 cache for next time.
            unsafe { *self.ptr_cache.get_unchecked_mut(slot) = (ptr, id); }
            return id;
        }
        // Content lookup fallback. We do NOT cache the caller's ptr
        // here: if the caller's Rc is a non-canonical one (e.g. created
        // via `Rc::from(&str)` outside the interner), it may be freed
        // after this call, and the memory address could later be reused
        // by a different allocation with different content — causing a
        // stale ptr_ids entry to mis-route future intern_rc calls.
        if let Some(&id) = self.ids.get(&**s) {
            return id;
        }
        // Brand-new entry: we allocate a canonical Rc and its ptr is
        // safe to cache because we store the Rc ourselves.
        let id = self.strings.len() as u32;
        let canonical = Rc::clone(s);
        let canonical_ptr = Rc::as_ptr(&canonical) as *const () as usize;
        self.strings.push(canonical);
        self.ids.insert(Rc::clone(s), id);
        self.ptr_ids.insert(canonical_ptr, id);
        let cslot = ((canonical_ptr as u64).wrapping_mul(0x9E3779B97F4A7C15) >> 32) as usize
            & (PTR_CACHE_SIZE - 1);
        unsafe { *self.ptr_cache.get_unchecked_mut(cslot) = (canonical_ptr, id); }
        id
    }

    fn resolve(&self, id: u32) -> Rc<str> {
        // Defence in depth. Symbol IDs are produced by `intern()` /
        // `intern_rc()` on this same thread and so are always in range,
        // but a future bytecode-loading or compiler bug could ask for
        // an OOB id. Returning an empty string makes the failure mode
        // observable rather than panicking the host (`panic = "abort"`
        // in release would tear down the embedder).
        match self.strings.get(id as usize) {
            Some(rc) => Rc::clone(rc),
            None => Rc::from(""),
        }
    }
}

thread_local! {
    /// Per-thread interner. The `UnsafeCell` lets us skip the `RefCell`
    /// dynamic borrow check — the VM never re-enters `intern()` while
    /// holding a reference to the interner, so the borrow rules are
    /// enforced by construction.
    static INTERNER: UnsafeCell<Interner> = UnsafeCell::new(Interner::new());
}

/// Idempotent initialisation shim — kept for API compatibility with the
/// previous `static mut` implementation. `thread_local!` initialises
/// lazily on first access, so this is effectively a no-op, but callers
/// are free to invoke it at startup.
pub fn ensure_init() {
    INTERNER.with(|_| {});
}

/// Intern a string slice, returning a `u32` symbol ID.
///
/// Identical strings always return the same ID **on the same thread**.
/// IDs are not portable across threads.
#[inline(always)]
pub fn intern(s: &str) -> u32 {
    INTERNER.with(|cell| {
        // SAFETY: `thread_local!` guarantees exclusive access on this
        // thread, and the VM never holds a live borrow across calls.
        unsafe { (*cell.get()).intern(s) }
    })
}

/// Intern an existing `Rc<str>`, returning a `u32` symbol ID.
/// O(1) if already interned.
#[inline(always)]
pub fn intern_rc(s: &Rc<str>) -> u32 {
    INTERNER.with(|cell| unsafe { (*cell.get()).intern_rc(s) })
}

/// Resolve a symbol ID back to its canonical `Rc<str>`.
#[inline(always)]
pub fn resolve(id: u32) -> Rc<str> {
    INTERNER.with(|cell| unsafe { (*cell.get()).resolve(id) })
}

/// Return a raw pointer to the thread-local `Interner`. Intended for hot
/// dispatch loops that pay the `thread_local!` access cost per
/// instruction — the VM can cache this pointer once per `run_register`
/// entry and use the `*_with_ptr` helpers below.
///
/// # Safety
///
/// The pointer is only valid on the thread that created it, and only
/// while that thread's `Interner` is alive (i.e. until thread exit).
/// Callers must not send the pointer across threads.
#[inline(always)]
pub unsafe fn raw_interner_ptr() -> *mut Interner {
    INTERNER.with(|cell| cell.get())
}

/// Intern a string slice via a raw interner pointer obtained from
/// `raw_interner_ptr`. Same behaviour as [`intern`] but skips the
/// `thread_local!` access on the hot path.
///
/// # Safety
///
/// `ptr` must be a valid pointer obtained from `raw_interner_ptr` on
/// the current thread.
#[inline(always)]
pub unsafe fn intern_with_ptr(ptr: *mut Interner, s: &str) -> u32 {
    (*ptr).intern(s)
}

/// Intern a canonical `Rc<str>` via a raw interner pointer. Companion
/// to [`intern_rc`].
///
/// # Safety
///
/// Same as [`intern_with_ptr`].
#[inline(always)]
pub unsafe fn intern_rc_with_ptr(ptr: *mut Interner, s: &Rc<str>) -> u32 {
    (*ptr).intern_rc(s)
}

/// Intern a string slice, returning the canonical `Rc<str>`.
/// Convenience for callers that want the `Rc` back directly
/// (e.g. `Object::String` constants).
#[inline(always)]
pub fn intern_str(s: &str) -> Rc<str> {
    INTERNER.with(|cell| unsafe {
        let i = &mut *cell.get();
        let id = i.intern(s);
        Rc::clone(&i.strings[id as usize])
    })
}

/// Like [`intern_with_ptr`] but also returns the canonical `Rc<str>`.
/// Saves a follow-up `resolve(id)` call (and its thread-local access)
/// when the caller needs both the sym id and the string itself.
///
/// # Safety
///
/// Same as [`intern_with_ptr`].
#[inline(always)]
pub unsafe fn intern_with_ptr_and_rc(ptr: *mut Interner, s: &str) -> (u32, Rc<str>) {
    let i = &mut *ptr;
    let id = i.intern(s);
    (id, Rc::clone(&i.strings[id as usize]))
}

/// Number of symbols in the current thread's interner.
/// Used by [`crate::vm::VM::check_execution_limits`] to enforce
/// [`MAX_INTERNED_SYMBOLS`].
#[inline]
pub fn interned_count() -> usize {
    INTERNER.with(|cell| unsafe { (*cell.get()).strings.len() })
}
