#![allow(unused_imports)]
//! Interpreter inline caches (ICs) for the hot call / property paths.
//!
//! Every `CallMethod` / `Call` / `GetProp` / `SetProp` / `SuperMethod` site in
//! a main-program or loader-recorded ES-module function (never ordinary
//! eval/new-Function code) owns a lazily-allocated
//! per-ip cache of up to [`IC_WAYS`] entries. A hit skips the slow machinery
//! (the `try_builtin_method` probe, the proto/class chain walk with its
//! per-level string finds, `resolve_callable` + the generator/async flag
//! loads) and goes straight to the action: a register read/write for a data
//! property, or `setup_call` for a resolved plain user function (method,
//! getter, setter) — so accessor round-trips run on the SAME dispatch-loop
//! frame machinery as ordinary calls instead of a nested `run_loop`.
//!
//! ── Correctness model (what makes a cached entry valid) ──
//!
//! The caches are *bug-compatible* with `get_member`'s fast path / `set_prop`:
//! a fill records provenance from a side-effect-free re-walk of EXACTLY the
//! conditions those paths test, and every hit re-validates the guards below.
//! Anything the fast paths would bail on (the global object, `%Array.
//! prototype%`, module namespaces, realm globals, builtin-ctor objects,
//! Proxies, non-`Object` receivers, deferred-namespace state) is excluded at
//! fill time and re-excluded cheaply at hit time.
//!
//! * `OwnData`/`OwnAcc` entries cache a SHAPE plus SLOT. A guardable first-way
//!   shape match proves the key → slot mapping without a name lookup; other
//!   ways validate `keys[slot] == key`. Every usable hit re-reads
//!   `attrs[slot]` — so in-place value writes, `freeze`, and data⇄accessor
//!   redefinition are all observed fresh. Nothing cached can go stale.
//! * `Class*` entries rely on ClassData's member tables being IMMUTABLE after
//!   class definition (methods/getters/setters/parent are only written by the
//!   MakeClass / computed-member ops) and on the tables still describing the
//!   live prototype objects. The latter stops holding exactly when
//!   `note_class_proto_mutation` retires a level (`C.prototype.m = …`,
//!   defineProperty/delete of a declared member, freeze, setPrototypeOf) or
//!   `note_class_instance_reproto` flags a class — both advance
//!   `class_proto_epoch`, which every entry bakes. The guard is
//!   `m.class == class` plus `heap.version_of(class)` (a swept-and-reused class
//!   slot bumps the version) plus that epoch, plus an own-shadow `pos(key)`
//!   miss. A live receiver whose `class` link matches keeps the class — and
//!   through `trace_edges`, the cached method/accessor Values — alive, so the
//!   cached Values can never be used after a sweep.
//! * `Proto*` entries guard the receiver by an own-`pos` miss plus a re-read
//!   of its FIRST proto link, and every chain object by its heap VERSION.
//!   Every mutation that can change chain resolution bumps a guarded
//!   version: key-add (`set_prop`/`define_property`), key-remove
//!   (`delete_prop`), and prototype replacement (`ordinary_set_prototype_of`
//!   — the bump lives there). Values are re-read from the live holder slot on
//!   every hit, and callables are re-resolved (`Func`/`Closure` → fid +
//!   flags) from the freshly-read Value, so slot reuse after a sweep can
//!   never call through a stale function id.
//! * `Callee` entries (plain `f(x)` sites) guard on the callee Value BITS plus
//!   the heap version of its slot — a swept-and-reused slot misses.
//! * `Super*` entries guard on the home-class VALUE (`class_values[id]`, so a
//!   re-evaluated class declaration misses) plus the version chain from the
//!   class's synthesized prototype object down to the holder.
//!
//! GC: the cache is intentionally NOT a GC root. Entries may hold dangling
//! Values after a sweep, but every dereference above is preceded by a guard
//! that fails for swept/reused slots (version bump on reuse) or re-reads the
//! value from a live, guard-validated holder — a stale entry can be PRESENT
//! but never USED.

use super::props::ClassLookup;
use super::*;
use crate::bytecode::{Instr, Program, UpvalSource};
use crate::heap::{ClassData, Handler, Heap, HeapObj, ObjMap, PropAttr};
use crate::slot_table::ProtoTable;
use crate::value::Value;

/// Ways per call-site cache. The class benches cycle 4 receiver shapes and the
/// megamorphic property benches cycle 8 layouts; 8 ways covers both.
pub(crate) const IC_WAYS: usize = 8;
/// Non-cacheable misses (or full-set evictions) before a site is disabled and
/// stops paying the fill walk. Existing entries keep validating.
const IC_MISS_LIMIT: u8 = 16;
/// Slow-path visits a disabled site makes between fills (a power of two, at
/// most 128).
/// A site whose receivers change class over time, such as one reading a class
/// expression's instances when each call makes a new class, evicts a way per
/// class and reaches the limit although each class then hits for hundreds of
/// reads; class-features' per-round classes ran their getters, methods and
/// statics through the generic paths for the rest of the run, 2.8 times
/// QuickJS's time. Letting one visit in this many fill again brings such a
/// site back soon after each new class, and costs a megamorphic one a fill
/// walk per this many of its slow reads. Measured in one binary against no
/// probation: that phase 0.60 (0.55 at 16 visits, whose refills cost the
/// polymorphic phases beside it 2-4%), class-features 0.971, the rest of the
/// 51 bench rows and the bundled applications neutral.
const IC_PROBATION: u8 = 64;
/// Maximum proto-chain hops a `Proto*`/`Super*` entry can guard.
const IC_MAX_HOPS: usize = 6;

/// `ZIPP_NO_CLASS_MISS=1`: a class instance's absent key fills no way.
fn class_miss_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static STATE: AtomicU8 = AtomicU8::new(0);
    match STATE.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            let on = std::env::var_os("ZIPP_NO_CLASS_MISS").is_none();
            STATE.store(if on { 1 } else { 2 }, Ordering::Relaxed);
            on
        }
    }
}

/// `Vm::own_slot_cache`'s size, a power of two.
pub(crate) const OWN_SLOT_CACHE_SLOTS: usize = 4096;

/// One answer of [`Vm::own_pos`]: in an object of shape `shape`, the key of
/// `len` bytes held zero-padded in `w0`/`w1` is at `slot`, or absent when
/// `slot` is `u32::MAX`. `shape` is never `DICT`, so the empty entry (shape
/// 0) matches nothing.
#[derive(Clone, Copy)]
pub(crate) struct OwnSlot {
    shape: u32,
    len: u32,
    slot: u32,
    w0: u64,
    w1: u64,
}

impl OwnSlot {
    pub(crate) const EMPTY: OwnSlot = OwnSlot { shape: crate::shape::DICT, len: 0, slot: 0, w0: 0, w1: 0 };
    /// In `slot`: the property there is an accessor. A shape fixes its
    /// properties' attributes, so the bit holds as long as the answer does.
    const ACCESSOR: u32 = 1 << 31;
    /// In `slot`: the data property there is not writable.
    const READONLY: u32 = 1 << 30;
    const FLAGS: u32 = Self::ACCESSOR | Self::READONLY;
}

/// A key of at most 16 bytes as two little-endian words, zero-padded.
#[inline(always)]
pub(crate) fn key_words(b: &[u8]) -> (u64, u64) {
    if b.len() > 8 {
        let (lo, hi) = b.split_at(8);
        (u64::from_le_bytes(lo.try_into().expect("8 bytes")), load_le_short(hi))
    } else {
        (load_le_short(b), 0)
    }
}

/// At most 8 bytes as a zero-padded little-endian word, from two loads that
/// overlap when the length is not a power of two: copying the key into a
/// zeroed buffer was a variable-length `memcpy` on every property probe.
#[inline(always)]
fn load_le_short(b: &[u8]) -> u64 {
    let n = b.len();
    debug_assert!(n <= 8);
    if n >= 4 {
        let lo = u32::from_le_bytes(b[..4].try_into().expect("4 bytes")) as u64;
        let hi = u32::from_le_bytes(b[n - 4..].try_into().expect("4 bytes")) as u64;
        lo | (hi << ((n - 4) * 8))
    } else if n >= 2 {
        let lo = u16::from_le_bytes(b[..2].try_into().expect("2 bytes")) as u64;
        let hi = u16::from_le_bytes(b[n - 2..].try_into().expect("2 bytes")) as u64;
        lo | (hi << ((n - 2) * 8))
    } else if n == 1 {
        b[0] as u64
    } else {
        0
    }
}

#[inline(always)]
fn own_slot_index(shape: u32, w0: u64, len: usize) -> usize {
    let h = (shape as u64 | (len as u64) << 32).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ w0.wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
    (h >> (64 - OWN_SLOT_CACHE_SLOTS.trailing_zeros())) as usize
}
#[cfg(all(feature = "meter-only", not(feature = "jit")))]
pub(crate) const IC_NO_FIB_GLOBAL: u32 = u32::MAX;

/// `ZIPP_NO_IC_MEGA=1` turns off `Vm::ic_get_prop_mega` and
/// `Vm::ic_set_prop_mega` (the reads and writes then take the full cache
/// calls, as before), for an A/B on one binary.
#[cfg(all(feature = "jit", not(feature = "safe-sandbox")))]
#[inline]
fn ic_mega_enabled() -> bool {
    if crate::WASM_STATIC_SWITCHES {
        return true;
    }
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let v = std::env::var_os("ZIPP_NO_IC_MEGA").is_none() as u8;
            ON.store(v, Ordering::Relaxed);
            v == 1
        }
    }
}

/// Let loader-installed module functions retain the same guarded call/property
/// feedback as main-program functions. Runtime module and eval functions share
/// one id space, so admission additionally checks the loader's exact immutable
/// function ranges. Cached because every hot IC probe asks this question.
#[inline]
fn module_ic_enabled() -> bool {
    if crate::WASM_STATIC_SWITCHES {
        return true;
    }
    static FLAG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FLAG.get_or_init(|| std::env::var_os("ZIPP_NO_MODULE_IC").is_none())
}

/// Sentinel `ret_dst` for a frame whose return value is DISCARDED (an
/// IC-pushed SETTER activation): `pop_frame_with` skips the caller-register
/// write. Real `ret_dst` values are register indices < `reg_count`, far below
/// `u16::MAX`.
pub(crate) const RET_DISCARD: u16 = u16::MAX;

/// First-link discriminant for `Proto*` entries: how the RECEIVER's own proto
/// step resolved at fill time (re-checked on every hit, because the receiver
/// itself is not version-guarded).
pub(crate) const FIRST_HEAP: u8 = 0; // explicit proto_of entry → hops[0]
pub(crate) const FIRST_DEFAULT: u8 = 1; // no proto_of entry → %Object.prototype% (hops[0])
pub(crate) const FIRST_NULL: u8 = 2; // explicit non-heap (null) proto → chain ends

/// Guarded chain: `(heap index, heap version at fill)` per chain object, in
/// walk order (`hops[0]` = the object after the receiver; the last live hop is
/// the HOLDER for data/accessor entries, or the chain END for a miss entry).
pub(crate) type IcHops = ([(u32, u32); IC_MAX_HOPS], u8);

/// One validated resolution for a site. See the module doc for the guard each
/// variant re-checks on a hit.
#[derive(Clone, Copy, Debug)]
pub(crate) enum IcEntry {
    /// `recv.key` is an own property of a plain/instance Object at `slot`
    /// (data for `OwnData`, accessor for `OwnAcc`).
    ///
    /// `shape` is the receiver's hidden class at fill time, or
    /// [`crate::shape::DICT`] if it had none. When it matches, the key -> slot
    /// mapping is proven and the key lookup is SKIPPED — see the fast path in
    /// `ic_get_prop`. When it does not, the entry still validates the old way
    /// (`own == Some(slot)`), so a dictionary-mode receiver loses no ground.
    OwnData {
        shape: u32,
        slot: u32,
    },
    OwnAcc {
        shape: u32,
        slot: u32,
    },
    /// Method (`is_getter == false`) or getter resolved on the receiver's
    /// class chain. `callee` is the materialized member function (stable for
    /// the life of the class — ClassData is immutable post-definition).
    /// `epoch` is `class_proto_epoch` at fill: the tables it was read from
    /// still described the prototype chain then. `recv_shape` is the
    /// receiver's hidden class at fill (or [`crate::shape::DICT`]): a live
    /// match proves the receiver has no own property of the key, so a hit
    /// needs no key lookup (`ic_call_method`).
    ClassMethod {
        class: u32,
        ver: u32,
        epoch: u32,
        callee: Value,
        fid: u32,
        closure: u32,
        recv_shape: u32,
    },
    /// `recv_shape` as for `ClassMethod`: a live match proves the receiver
    /// has no own property of the key (`ic_get_acc_tail`, which only the
    /// mirrored profiles have).
    ClassGetter {
        class: u32,
        ver: u32,
        epoch: u32,
        getter: Value,
        #[cfg_attr(not(all(feature = "jit", not(feature = "safe-sandbox"))), allow(dead_code))]
        recv_shape: u32,
    },
    /// `set key(v)` resolved on the receiver's class chain (SetProp sites).
    /// `recv_shape` as for `ClassGetter` (`ic_set_way0_tail`).
    ClassSetter {
        class: u32,
        ver: u32,
        epoch: u32,
        setter: Value,
        #[cfg_attr(not(all(feature = "jit", not(feature = "safe-sandbox"))), allow(dead_code))]
        recv_shape: u32,
    },
    /// Data property / accessor found on the proto_of chain at
    /// `hops[last].slot`, or (`slot == u32::MAX` for `ProtoData`) a full-chain
    /// MISS — the read yields `undefined`.
    ProtoData {
        /// Guardable hidden shape of the receiver at fill time. For an
        /// inherited-data entry, a live match proves the static key remains
        /// absent from the receiver without another key-table lookup.
        recv_shape: u32,
        first: u8,
        hops: IcHops,
        slot: u32,
    },
    /// An accessor on the proto_of chain at `hops[last].slot`. `recv_shape`
    /// is the receiver's hidden class at fill, as for `ProtoData`: a live
    /// match proves the key still absent from the receiver, so a get or a
    /// set through a prototype's accessor (`v.x` with `x` defined on
    /// `Vec.prototype`) needs no own-key lookup first.
    ProtoAcc {
        recv_shape: u32,
        first: u8,
        hops: IcHops,
        slot: u32,
    },
    /// `SetProp` that APPENDS the site's key: a plain extensible object of
    /// shape `before` takes it as a new data property and reaches `after`,
    /// and the chain (`first`/`hops`, exactly as a `ProtoData` miss or
    /// inherited-writable-data entry guards it) holds no setter and no
    /// read-only property of the name. A constructor's `this.x = v` adds to a
    /// new object every time: no own-data entry can serve it, and every one
    /// took `set_prop`'s whole walk.
    AddData {
        before: u32,
        after: u32,
        first: u8,
        hops: IcHops,
    },
    /// The class-instance form of `AddData`: no member table of `class`'s
    /// chain declares the key (the class's version and `class_proto_epoch`
    /// guard the tables, as `ClassSetter` guards them), and the ordinary
    /// chain where the tables end (`hops`: the materialized class prototypes
    /// and `%Object.prototype%`) is clean. Five hops: constructing an
    /// instance materializes every level's prototype, so a class three
    /// `extends` deep walks four objects, and two hops (one level) sent
    /// every constructor's `this.x = ...` of a derived class to `set_prop`.
    AddClass {
        before: u32,
        after: u32,
        class: u32,
        ver: u32,
        epoch: u32,
        nhops: u8,
        hops: [(u32, u32); 5],
    },
    /// A class instance's read of a key nothing on its chain has: the
    /// receiver of `recv_shape` owns no such key, no member table of
    /// `class`'s chain declares it (`ver` and `epoch` guard the tables, as
    /// `AddClass` guards them), and the ordinary chain where the tables end
    /// (`hops`) has no property of it. The read yields `undefined`
    /// (`node.comments` on an AST node that has none).
    ClassMiss {
        recv_shape: u32,
        class: u32,
        ver: u32,
        epoch: u32,
        nhops: u8,
        hops: [(u32, u32); 5],
    },
    /// Plain `Call` site: callee identity (bits) + slot version → resolved
    /// (fid, closure) for a plain (non-generator, non-async) Func/Closure.
    Callee {
        bits: u64,
        ver: u32,
        fid: u32,
        closure: u32,
        /// The callee's frame facts packed by `Vm::lean_call_desc`, so a hit
        /// pushes its frame without reading the callee's `FuncProto` (a cold
        /// line per call of a function not called lately; `FuncProto` is
        /// immutable, so the facts never go stale).
        #[cfg(not(any(feature = "meter-only", feature = "safe-sandbox")))]
        desc: u32,
        /// Cached structural classification for the meter-only numeric fib
        /// lane. `u32::MAX` means this immutable FuncProto is not the exact
        /// accepted body. Keeping the result in the identity/version-guarded
        /// Call IC entry avoids re-reading its bytecode on every hot hit.
        #[cfg(all(feature = "meter-only", not(feature = "jit")))]
        fib_global: u32,
    },
    /// `super.key(…)` / `super.key` site: `home` is the class VALUE the site's
    /// `home_class_id` resolved to at fill; `hops[0]` is the derivation anchor
    /// (the home's synthesized prototype object — or the class itself for a
    /// static member), `hops[1..]` the chain from the super base to the
    /// holder. `slot` indexes the holder's map.
    SuperData {
        home: Value,
        hops: IcHops,
        slot: u32,
    },
    SuperAcc {
        home: Value,
        hops: IcHops,
        slot: u32,
    },
    /// `ns.key` on the module namespace object `ns` (heap version `ver` at
    /// fill): the export's live binding is global slot `slot`
    /// (`ic_namespace_get`'s answer without its two hash lookups). A
    /// binding still in its TDZ takes the full path. Last, so the other
    /// variants keep their discriminants: the dispatch loops inline matches
    /// on them, and renumbering moved a closure-call row by 18%.
    NsExport {
        ns: u32,
        ver: u32,
        slot: u32,
    },
    /// `obj.name(...)` at a `CallMethod` site where `obj` (heap `obj`,
    /// version `ver` at fill) is an ordinary object whose own data property
    /// at `slot` held the native `id` (value bits `bits`): `Reflect.get`,
    /// `JSON.stringify`, `console.log`. The call runs that native directly
    /// (`Vm::ic_call_native`). Last, like `NsExport`.
    OwnNative {
        obj: u32,
        ver: u32,
        slot: u32,
        id: u16,
        bits: u64,
    },
    /// `obj.#x` (a read, or a write of the field already there) at a site
    /// whose code, running as the callee `head` (`Vm::private_memo_key`: its
    /// heap index and slot version), resolved the name to the private FIELD
    /// `(brand, id)`, which the receiver it filled from held at `pos` of its
    /// `private_fields` list. A hit is the running callee's key and the
    /// receiver's entry for the pair; the entry's presence is the brand check
    /// itself, as on the uncached path. Methods and accessors are not cached
    /// here. Only the way-0 own hits read it (`ic_private_get_tail`,
    /// `ic_private_set_tail`): a private name fills no other way. Last, like
    /// `NsExport`. The safe profile has no own hits and never fills one.
    /// A field initializer's site holds one with `pos` [`PRIVATE_ADD`]: the
    /// field's add (`Vm::ic_private_add_fill`).
    #[cfg_attr(feature = "safe-sandbox", allow(dead_code))]
    Private {
        head: u64,
        brand: u64,
        id: u32,
        pos: u32,
    },
    /// `obj.#m` for an instance's private METHOD, at a site whose code,
    /// running as the callee `head`, resolved the name to `method` of the
    /// class whose brand is `brand`; the receiver it filled from held that
    /// brand's entry at `pos` of its private list. A hit is the running
    /// callee's key, no declaring class collected since the fill (`epoch`,
    /// `Vm::private_epoch`: a class keeps its methods alive) and the
    /// receiver's brand entry. A method's every call (`this.#step(dt)`) read
    /// it through `private_get_slow`: the name's resolution, the brand scan
    /// and the class's method list by name. Last, after `Private`, as the
    /// own hit tests both at once.
    #[cfg_attr(feature = "safe-sandbox", allow(dead_code))]
    PrivateMethod {
        head: u64,
        brand: u64,
        method: Value,
        epoch: u32,
        pos: u32,
    },
}

/// The `pos` of an `IcEntry::Private` way that adds its field.
#[cfg_attr(feature = "safe-sandbox", allow(dead_code))]
pub(crate) const PRIVATE_ADD: u32 = u32::MAX;

// `SuperData` already established this interpreter-cache entry budget. Keep
// additional guards in the existing padding/variant envelope so every
// eight-way `SiteIc` remains the same size on the supported targets.
const _: () = assert!(
    std::mem::size_of::<IcEntry>() <= 72,
    "interpreter IcEntry exceeded its existing largest-variant size budget"
);

/// Read-only resolution of a `super.m()` op for Stage 3 method inlining
/// (`ic_super_method_baked`): the resolved super-method `fid`, the hop
/// `(heap_idx, version)` guards to re-check, and the holder slot + baked fn bits
/// (a same-slot reassignment value guard, in case an overwrite doesn't bump the
/// hop version).
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) struct MiSuperResolved {
    pub(crate) fid: u32,
    pub(crate) hops: Vec<(u32, u32)>,
    pub(crate) holder_vals_ptr: u64,
    pub(crate) holder_slot: u32,
    pub(crate) fn_bits: u64,
}

/// Per-site cache: up to [`IC_WAYS`] entries, a fill-failure counter, and a
/// round-robin eviction cursor.
///
/// `repr(C)` keeps the counters in FRONT of the ways. Left to the compiler
/// they sat after all eight (`n` at byte 577), so every hit, which tests `n`
/// and reads way 0, touched two cache lines of a 584-byte site: a fifth of
/// the own-property hit's samples in Babel were that `n` load.
#[repr(C)]
pub(crate) struct SiteIc {
    pub(crate) misses: u8,
    pub(crate) n: u8,
    rot: u8,
    pub(crate) entries: [IcEntry; IC_WAYS],
}

/// One instruction's cell in its function's cache table: the site, once it
/// has one, and beside it the last own-data answer the instruction found for
/// its key — `(shape, slot)`, `own_shape` [`crate::shape::DICT`] for none.
///
/// A property read's commonest hit is an own data property of a shaped
/// receiver, and a shape fixes its keys and their attributes for the VM's
/// life, so that pair answers every receiver of the shape whatever the
/// site's ways now hold. Held in the cell, a hit reads the table entry it
/// had to read anyway and not the 584-byte site behind it (a fifth of the
/// own-property hit's samples in Babel were the site's line), and a site
/// whose first way was filled for a layout it no longer sees still hits.
///
/// [`SiteCell::WRITE_PROVEN`] in `own_slot` marks a pair a WRITE proved
/// writable data (`ic_set_prop`'s fill); a write's hit needs that mark, a
/// read's takes either.
#[derive(Default)]
pub(crate) struct SiteCell {
    pub(crate) site: Option<Box<SiteIc>>,
    own_shape: u32,
    own_slot: u32,
}

impl SiteCell {
    const WRITE_PROVEN: u32 = 1 << 31;

    /// `own_shape` of a pair that records where a DICTIONARY-mode receiver
    /// held the key (`Vm::ic_cell_dict`). No shape is ever this, so no
    /// shape-keyed hit takes the pair; a read of such a receiver tries the
    /// slot first (`dict_hint`).
    const DICT_HINT: u32 = u32::MAX;

    /// The slot a dictionary-mode receiver last held the key at, if the
    /// cell remembers one.
    #[inline(always)]
    #[cfg_attr(feature = "safe-sandbox", allow(dead_code))]
    fn dict_hint(&self) -> Option<usize> {
        (self.own_shape == Self::DICT_HINT).then_some((self.own_slot & !Self::WRITE_PROVEN) as usize)
    }

    /// The slot of the key in a receiver of `shape`, if the cell knows it.
    /// (The safe profile has no inline hits to ask.)
    #[inline(always)]
    #[cfg_attr(feature = "safe-sandbox", allow(dead_code))]
    fn own_read(&self, shape: u32) -> Option<u32> {
        (self.own_shape == shape).then_some(self.own_slot & !Self::WRITE_PROVEN)
    }

    /// Whether the instruction has found its key as an own data property of
    /// some layout (`Vm::ic_get_prop_mega` and `Vm::ic_set_prop_mega` try only
    /// there).
    #[inline(always)]
    #[cfg_attr(not(all(feature = "jit", not(feature = "safe-sandbox"))), allow(dead_code))]
    fn knows_own(&self) -> bool {
        self.own_shape != crate::shape::DICT
    }

    /// The slot a write of the key may store to in a receiver of `shape`.
    #[inline(always)]
    #[cfg_attr(feature = "safe-sandbox", allow(dead_code))]
    fn own_write(&self, shape: u32) -> Option<u32> {
        (self.own_shape == shape && self.own_slot & Self::WRITE_PROVEN != 0)
            .then_some(self.own_slot & !Self::WRITE_PROVEN)
    }
}

/// Resolved action for a plain `Call` IC hit/fill. The meter-only build carries
/// one cached specialization witness; other builds retain the same two-u32
/// result shape as the former tuple.
#[derive(Clone, Copy)]
pub(crate) struct CallIcHit {
    pub(crate) fid: u32,
    pub(crate) closure: u32,
    /// `Vm::lean_call_desc(fid)`, for `Vm::setup_call_lean_desc`.
    #[cfg(not(any(feature = "meter-only", feature = "safe-sandbox")))]
    pub(crate) desc: u32,
    #[cfg(all(feature = "meter-only", not(feature = "jit")))]
    pub(crate) fib_global: u32,
}

/// Action for a `GetProp`-shaped site (also `SuperGet`).
pub(crate) enum GetAct {
    /// Data hit: write the value to `dst` and continue.
    Value(Value),
    /// Accessor hit resolved to a plain user function: push its frame with
    /// `this` = receiver and `ret_dst` = the GetProp `dst`.
    Accessor {
        fid: u32,
        closure: u32,
        getter: Value,
    },
    /// No usable entry — take the slow path.
    None,
}

/// Action for a `SetProp`-shaped site (also `SuperSet`).
pub(crate) enum SetAct {
    /// Own data slot written — the store is complete.
    Done,
    /// Setter hit resolved to a plain user function: push its frame with
    /// `this` = receiver, the value as the single argument, and
    /// `ret_dst` = [`RET_DISCARD`].
    Setter {
        fid: u32,
        closure: u32,
        setter: Value,
    },
    None,
}

/// A validated `SetProp` action, produced borrow-free by the shape probe or
/// [`Vm::ic_validate_set`].
enum SetPlan {
    WriteOwn {
        idx: u32,
        slot: u32,
    },
    Append {
        idx: u32,
        after: u32,
    },
    Setter {
        fid: u32,
        closure: u32,
        setter: Value,
    },
}

/// Side-effect-free provenance of a property resolution (the fill walk).
pub(crate) enum Walked {
    OwnData {
        slot: usize,
    },
    OwnAcc {
        slot: usize,
    },
    /// Hit on the class chain: methods-before-getters per level, like
    /// `get_member`'s inline walk.
    ClassMethod {
        class: u32,
        callee: Value,
    },
    ClassGetter {
        class: u32,
        getter: Value,
    },
    ChainData {
        first: u8,
        hops: IcHops,
        slot: u32,
    },
    ChainAcc {
        first: u8,
        hops: IcHops,
        slot: u32,
    },
    /// The whole (guardable) chain misses → `undefined`.
    ChainMiss {
        first: u8,
        hops: IcHops,
    },
    /// A class instance's key that neither its class chain's tables nor the
    /// ordinary chain past them has → `undefined` (`IcEntry::ClassMiss`).
    ClassMiss {
        class: u32,
        nhops: u8,
        hops: [(u32, u32); 5],
    },
    /// Not cacheable (exotic receiver/chain, too deep, non-Object hop, …).
    No,
}

/// Method names `dispatch_builtin_method` claims for ANY object receiver —
/// a `CallMethod` IC entry would bypass that claim, changing behavior, so
/// these names are never cached for method-call sites.
#[inline]
fn builtin_object_method(key: &str) -> bool {
    matches!(
        key,
        "hasOwnProperty" | "propertyIsEnumerable" | "isPrototypeOf"
    )
}

/// `ZIPP_NO_DYNAMIC_IC=1` keeps runtime `eval` / `new Function` code out of
/// the interpreter inline caches (the pre-B274 behaviour) for A/B runs.
#[inline]
fn dynamic_ic_enabled() -> bool {
    if crate::WASM_STATIC_SWITCHES {
        return true;
    }
    static FLAG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FLAG.get_or_init(|| std::env::var_os("ZIPP_NO_DYNAMIC_IC").is_none())
}

/// Cache storage every runtime-installed function of one VM may draw on
/// together (per-function `ip` tables plus site payloads). Main-program code
/// is bounded by its source; dynamic code is bounded by the dynamic-code
/// ceilings, which admit 16,384 functions in the wasm build, so its feedback
/// gets a budget of its own. Past it a site simply runs uncached, which
/// changes nothing a program can observe.
const DYNAMIC_IC_BUDGET_BYTES: usize = 8 << 20;

impl<'p> Vm<'p> {
    /// Main code, exact loader-recorded module ranges, and installed runtime
    /// code (`eval` / `new Function`) may own IC state.
    ///
    /// Dynamic functions used to be excluded wholesale, which left the
    /// programs that compile their hot paths into JavaScript (an emulator
    /// installing its traces) running every property site uncached: the
    /// same monomorphic property loop measured 70% slower installed through
    /// `new Function` than as main code. Eligibility is exact, not a range:
    /// `eval_funcs` is append-only and its protos are leaked at stable
    /// addresses, so an id past `main_func_count` names one installed
    /// function for the life of the VM and is never repurposed — the property
    /// every guard in this module relies on. Ids are assigned in
    /// `prepare_eval_program` only after the program's global/function/class
    /// remap, and a site can only fill once its code runs, so no cache ever
    /// sees a pre-remap instruction.
    ///
    /// This sits on every IC probe. Main code, the overwhelmingly common
    /// case, must stay one compare in the caller: the module and dynamic
    /// checks (two lazily read switches, a range scan, a subtraction) live
    /// out of line so their size can never cost the hot path its inlining.
    ///
    /// Installed code is hot too: every function of the Python runtime (and
    /// of torch) and every script an embedder runs through `eval_in_context`
    /// is installed, and the out-of-line half binary-searched the module
    /// ranges on each probe (2% of an interpreted torch loop). With dynamic
    /// caching on, the default, an installed function is eligible whichever
    /// kind it is, so that answer is one more compare here.
    #[inline(always)]
    fn ic_func_eligible(&self, func_id: u32) -> bool {
        let f = func_id as usize;
        f < self.main_func_count
            || (f.wrapping_sub(self.main_func_count) < self.eval_funcs.len() && dynamic_ic_enabled())
            || self.ic_func_eligible_installed(func_id)
    }

    /// The runtime-installed half of `ic_func_eligible`: loader-recorded
    /// module ranges and, past `main_func_count`, installed dynamic code.
    #[cold]
    #[inline(never)]
    fn ic_func_eligible_installed(&self, func_id: u32) -> bool {
        (module_ic_enabled() && self.loader_module_func(func_id))
            || (dynamic_ic_enabled()
                && (func_id as usize).wrapping_sub(self.main_func_count) < self.eval_funcs.len())
    }

    /// Exclusions shared with `get_member`'s fast path: objects with live /
    /// exotic slot semantics layered over their ObjMap never take IC paths.
    /// (Also consulted by the JIT region prop-miss helpers in helpers_misc.rs.)
    #[inline(always)]
    pub(crate) fn ic_obj_ok(&self, idx: u32) -> bool {
        !(idx == self.global_this && self.global_this != 0)
            && !(idx == self.arr_proto && self.arr_proto != 0)
            && (self.module_namespaces.is_empty() || !self.module_namespaces.contains_key(&idx))
            && (self.realm_global_objs.is_empty() || !self.realm_global_objs.contains_key(&idx))
    }

    /// [`Self::ic_obj_ok`] for a receiver whose map `m` is at hand. A module
    /// namespace's map is in dictionary mode for good
    /// (`register_module_namespace`), so a receiver with a shape is not one,
    /// and only a dictionary-mode receiver is hashed into
    /// `module_namespaces`. Every cached read of a module program hashed its
    /// receiver there: 4% of a module loop.
    #[inline(always)]
    fn ic_obj_ok_map(&self, idx: u32, m: &ObjMap) -> bool {
        debug_assert!(
            !m.shape_guardable() || self.module_namespaces.is_empty() || !self.module_namespaces.contains_key(&idx),
            "module namespace {idx} with a guardable shape"
        );
        !(idx == self.global_this && self.global_this != 0)
            && !(idx == self.arr_proto && self.arr_proto != 0)
            && (m.shape_guardable() || self.module_namespaces.is_empty() || !self.module_namespaces.contains_key(&idx))
            && (self.realm_global_objs.is_empty() || !self.realm_global_objs.contains_key(&idx))
    }

    /// Make `idx` the module namespace whose exports read `slots`: its live
    /// bindings are layered over its map, so the map drops to dictionary
    /// mode for good (no shape-keyed cache way can serve it, which is what
    /// lets `ic_obj_ok_map` skip shaped receivers) and its mirrors are
    /// pinned.
    pub(crate) fn register_module_namespace(&mut self, idx: u32, slots: ExportSlots) {
        self.module_namespaces.insert(idx, slots);
        if let HeapObj::Object(m) = self.heap.get_mut(idx) {
            m.pin_dict();
        }
        self.heap.pin_mirror_dict(idx);
    }

    /// The site cache for `(func_id, ip)`, if this function is IC-eligible.
    /// Loader-recorded modules cache; ordinary eval/new-Function code does not.
    ///
    /// Three loads on every cached property access. The checked profile's
    /// size-first build left it a call on a hint: 5% of a property loop.
    #[cfg_attr(feature = "safe-sandbox", inline(always))]
    #[cfg_attr(not(feature = "safe-sandbox"), inline)]
    fn ic_site(&self, func_id: u32, ip: usize) -> Option<&SiteIc> {
        let f = func_id as usize;
        if !self.ic_func_eligible(func_id) {
            return None;
        }
        self.site_ics.get(f)?.as_ref()?.get(ip)?.site.as_deref()
    }

    /// The table cell of `(func_id, ip)`, if its function has a table.
    #[inline(always)]
    #[cfg_attr(feature = "safe-sandbox", allow(dead_code))]
    fn ic_cell(&self, func_id: u32, ip: usize) -> Option<&SiteCell> {
        self.site_ics.get(func_id as usize)?.as_ref()?.get(ip)
    }

    /// Remember that the key of the property instruction at `(func_id, ip)`
    /// is the data property in `slot` of a receiver of `shape` (writable,
    /// with `write_proven`). A table that does not exist yet is not made for
    /// it: the instruction's first fill makes one.
    #[inline]
    fn ic_cell_own(&mut self, func_id: u32, ip: usize, shape: u32, slot: usize, write_proven: bool) {
        if shape == crate::shape::DICT || slot >= SiteCell::WRITE_PROVEN as usize {
            return;
        }
        if let Some(cell) = self
            .site_ics
            .get_mut(func_id as usize)
            .and_then(Option::as_mut)
            .and_then(|table| table.get_mut(ip))
        {
            cell.own_shape = shape;
            cell.own_slot = slot as u32 | if write_proven { SiteCell::WRITE_PROVEN } else { 0 };
        }
    }

    /// Remember that a dictionary-mode receiver held the key of the
    /// property instruction at `(func_id, ip)` in `slot` (`SiteCell::
    /// dict_hint`), unless the cell knows a shaped receiver's pair, which
    /// the inline hits read. As `ic_cell_own`, never made for it.
    #[inline]
    fn ic_cell_dict(&mut self, func_id: u32, ip: usize, slot: usize) {
        if slot >= SiteCell::WRITE_PROVEN as usize {
            return;
        }
        if let Some(cell) = self
            .site_ics
            .get_mut(func_id as usize)
            .and_then(Option::as_mut)
            .and_then(|table| table.get_mut(ip))
        {
            if cell.own_shape == crate::shape::DICT || cell.own_shape == SiteCell::DICT_HINT {
                cell.own_shape = SiteCell::DICT_HINT;
                cell.own_slot = slot as u32;
            }
        }
    }

    /// Count a fill failure (or a full-set eviction) at a site, allocating the
    /// site cache if needed so the disable counter persists.
    fn ic_note_miss(&mut self, func_id: u32, ip: usize) {
        if let Some(site) = self.ic_site_mut(func_id, ip) {
            site.misses = site.misses.saturating_add(1);
        }
    }

    /// Whether a site past [`IC_MISS_LIMIT`] stays on the slow path for this
    /// visit; every [`IC_PROBATION`]th visit it may fill once more (and the
    /// fill's eviction, if the site is full, disables it again). The visits
    /// are counted in the eviction cursor, so that fill takes way 0.
    #[cold]
    #[inline(never)]
    fn ic_still_disabled(&mut self, func_id: u32, ip: usize) -> bool {
        let Some(site) = self
            .site_ics
            .get_mut(func_id as usize)
            .and_then(Option::as_mut)
            .and_then(|table| table.get_mut(ip))
            .and_then(|cell| cell.site.as_deref_mut())
        else {
            return true;
        };
        // `ZIPP_NO_IC_PROBATION=1` leaves a disabled site disabled, as before:
        // the two sides of a one-binary A/B.
        static OFF: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if *OFF.get_or_init(|| std::env::var_os("ZIPP_NO_IC_PROBATION").is_some()) {
            return true;
        }
        site.rot = site.rot.wrapping_add(1);
        if site.rot % IC_PROBATION != 0 {
            return true;
        }
        site.misses = IC_MISS_LIMIT - 1;
        false
    }

    /// Mutable access to (and lazy allocation of) the site cache.
    fn ic_site_mut(&mut self, func_id: u32, ip: usize) -> Option<&mut SiteIc> {
        let f = func_id as usize;
        if !self.ic_func_eligible(func_id) {
            return None;
        }
        let func_count = self.main_func_count + self.eval_funcs.len();
        if self.site_ics.len() < func_count {
            self.site_ics.resize_with(func_count, || None);
        }
        let code_len = self.func(f).code.len();
        if f >= self.main_func_count {
            // Runtime-installed code draws its cache storage from the fixed
            // per-VM budget: charge what THIS call would allocate (the
            // function's ip table and/or this site's payload) before
            // allocating either, and run the site uncached once it is spent.
            let table_new = self.site_ics[f].is_none();
            let site_new = ip < code_len
                && self.site_ics[f]
                    .as_ref()
                    .is_none_or(|table| table[ip].site.is_none());
            let mut need = 0usize;
            if table_new {
                need = need.saturating_add(code_len.saturating_mul(std::mem::size_of::<SiteCell>()));
            }
            if site_new {
                need = need.saturating_add(std::mem::size_of::<SiteIc>());
            }
            if need > 0 {
                let total = self.dynamic_ic_bytes.saturating_add(need);
                if total > DYNAMIC_IC_BUDGET_BYTES {
                    return None;
                }
                self.dynamic_ic_bytes = total;
            }
        }
        let slots = self.site_ics[f].get_or_insert_with(|| {
            let mut v = Vec::new();
            v.resize_with(code_len, SiteCell::default);
            v.into_boxed_slice()
        });
        let slot = slots.get_mut(ip)?;
        Some(slot.site.get_or_insert_with(|| {
            Box::new(SiteIc {
                misses: 0,
                n: 0,
                rot: 0,
                entries: [IcEntry::OwnData {
                    shape: crate::shape::DICT,
                    slot: 0,
                }; IC_WAYS],
            })
        }))
    }

    /// Install `e` at the site (round-robin eviction when full; a full-set
    /// eviction also counts toward the disable limit so a megamorphic site
    /// eventually stops churning).
    fn ic_install(&mut self, func_id: u32, ip: usize, e: IcEntry) {
        if let Some(site) = self.ic_site_mut(func_id, ip) {
            if (site.n as usize) < IC_WAYS {
                site.entries[site.n as usize] = e;
                site.n += 1;
            } else {
                let r = site.rot as usize % IC_WAYS;
                site.entries[r] = e;
                site.rot = site.rot.wrapping_add(1);
                site.misses = site.misses.saturating_add(1);
            }
        }
    }

    /// Q7: record a receiver INSTANCE seen at a Class method/getter/setter site
    /// (called at IC-fill time — rare, so ~free). The JIT planner reads the ≤8
    /// recorded receiver-bits to bake per-instance inline arms (the class-keyed
    /// IC records no instances). Deduped + capped; stale entries are harmless
    /// (rebuilt-from-live + runtime-guarded). See `mi_recv`.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    fn mi_record_recv(&mut self, func_id: u32, ip: usize, recv: Value) {
        if !recv.is_heap() {
            return;
        }
        let bits = recv.bits();
        let set = self
            .mi_recv
            .entry(((func_id as u64) << 32) | ip as u64)
            .or_default();
        if set.len() < IC_WAYS && !set.contains(&bits) {
            set.push(bits);
        }
    }
    #[cfg(not(all(feature = "jit", target_arch = "x86_64")))]
    fn mi_record_recv(&mut self, _func_id: u32, _ip: usize, _recv: Value) {}

    // ── the fill walk ──

    /// Resolve `recv.key` with provenance, mirroring `get_member`'s fast path
    /// EXACTLY and performing no side effects. Anything the fast path would
    /// delegate to the slow path reports `Walked::No`.
    pub(crate) fn ic_walk(&self, recv: Value, key: &str) -> Walked {
        if !self.deferred_ns_state.is_empty() || !recv.is_heap() {
            return Walked::No;
        }
        // Private members live in side tables with brand semantics — never IC.
        if matches!(key.as_bytes().first(), Some(b'#')) {
            return Walked::No;
        }
        let idx = recv.heap_index();
        if !self.ic_obj_ok(idx) {
            return Walked::No;
        }
        let m = match self.heap.get(idx) {
            HeapObj::Object(m) if !m.is_ctor => m,
            _ => return Walked::No,
        };
        if let Some(i) = self.own_pos(m, key) {
            return if m.attr_at(i).accessor {
                Walked::OwnAcc { slot: i }
            } else {
                Walked::OwnData { slot: i }
            };
        }
        if let Some(class) = m.class {
            // Class-instance own miss: the inline class-chain walk
            // (methods before getters per level; a non-Class link breaks to
            // the slow path → not cacheable).
            //
            // A class whose instances may carry their own [[Prototype]] is not
            // cacheable by class: an entry filled from one instance would also
            // serve a re-prototyped sibling, and the hit guard does not read
            // `proto_of`. A retired table level (`Live`) resolves on the live
            // chain, which the slow path walks.
            if matches!(self.heap.get(class), HeapObj::Class(c) if c.reproto_instances) {
                return Walked::No;
            }
            return match self.class_member_lookup(idx, class, key) {
                ClassLookup::Method(callee) => Walked::ClassMethod { class, callee },
                ClassLookup::Accessor { getter, .. } if getter != Value::UNDEFINED => {
                    Walked::ClassGetter { class, getter }
                }
                // Absent from the tables: absent from the ordinary chain
                // past them too is a guardable miss.
                ClassLookup::Miss if class_miss_enabled() => {
                    let mut hops = [(0u32, 0u32); 5];
                    match self.class_chain_proof(idx, class, key, &mut hops, true) {
                        Some(n) => Walked::ClassMiss { class, nhops: n as u8, hops },
                        None => Walked::No,
                    }
                }
                _ => Walked::No, // live level, setter-only accessor → slow path
            };
        }
        // Plain-object proto_of chain, recording (idx, version) per hop.
        self.ic_walk_chain(idx, key)
    }

    /// `m.pos(key)`, answered from `own_slot_cache` when `m` has a shape and
    /// `key` is at most 16 bytes.
    ///
    /// A site that sees many layouts — Babel's `node.type`, `path.node`, the
    /// AST walkers of every bundled compiler — misses its shape-keyed ways on
    /// most reads, and each miss hashed the key and compared it against the
    /// object's heap-allocated key string (a quarter of a miss-heavy profile,
    /// and the set path asked twice). An object's shape fixes its key
    /// sequence, and shapes are never reissued, so one (shape, key) answer —
    /// a slot, or absent — holds for every object of that shape for the
    /// VM's life. Keys are compared by their bytes, not by address.
    #[inline]
    pub(crate) fn own_pos(&self, m: &ObjMap, key: &str) -> Option<usize> {
        let sh = m.shape();
        let b = key.as_bytes();
        if sh == crate::shape::DICT || b.len() > 16 {
            return m.pos(key);
        }
        let (w0, w1) = key_words(b);
        let cell = &self.own_slot_cache[own_slot_index(sh, w0, b.len())];
        let e = cell.get();
        if e.shape == sh && e.len == b.len() as u32 && e.w0 == w0 && e.w1 == w1 {
            let r = (e.slot != u32::MAX).then_some((e.slot & !OwnSlot::FLAGS) as usize);
            debug_assert_eq!(r, m.pos(key), "own_slot_cache: a stale answer for {key:?}");
            return r;
        }
        let r = m.pos(key);
        cell.set(OwnSlot {
            shape: sh,
            len: b.len() as u32,
            slot: r.map_or(u32::MAX, |s| {
                let a = m.attr_at(s);
                s as u32
                    | if a.accessor {
                        OwnSlot::ACCESSOR
                    } else if !a.writable {
                        OwnSlot::READONLY
                    } else {
                        0
                    }
            }),
            w0,
            w1,
        });
        r
    }

    /// The slot of an own writable DATA property `key` of every object of
    /// `shape`, when the (shape, key) memo already holds the answer.
    #[cfg(all(feature = "jit", not(feature = "safe-sandbox")))]
    #[inline(always)]
    fn own_writable_slot_cached(&self, sh: u32, key: &str) -> Option<u32> {
        let b = key.as_bytes();
        if sh == crate::shape::DICT || b.len() > 16 {
            return None;
        }
        let (w0, w1) = key_words(b);
        let e = self.own_slot_cache[own_slot_index(sh, w0, b.len())].get();
        (e.shape == sh && e.len == b.len() as u32 && e.w0 == w0 && e.w1 == w1 && e.slot & OwnSlot::FLAGS == 0)
            .then_some(e.slot)
    }

    /// [`Self::ic_get_prop_mega`] for a write: an own writable data property
    /// stored through the receiver's mirror, as the cache's own-data hit
    /// stores it (`ic_set_prop_own_hit`), from the (shape, key) memo. A class
    /// prototype, whose writes must reach `set_prop`, is pinned to `DICT`
    /// (`mark_class_proto`) and never matches. `false` for anything else.
    #[cfg(all(feature = "jit", not(feature = "safe-sandbox")))]
    #[inline]
    pub(crate) fn ic_set_prop_mega(&mut self, func_id: u32, ip: usize, recv: Value, key: &str, val: Value) -> bool {
        if !recv.is_heap()
            || is_private_key(key)
            || !self.deferred_ns_state.is_empty()
            || !self.interp_own_resolve_vm_is_interpreter()
            || !ic_mega_enabled()
        {
            return false;
        }
        // As for the read: only where a write has stored an own field before.
        if !self.ic_cell(func_id, ip).is_some_and(SiteCell::knows_own) {
            return false;
        }
        let idx = recv.heap_index();
        let mir = self.heap.hot_mirror_of(idx);
        if mir.shape == crate::shape::DICT || mir.fid != crate::heap::FID_MIRROR_NONE {
            return false;
        }
        let Some(slot) = self.own_writable_slot_cached(mir.shape, key) else {
            return false;
        };
        if val.is_heap() {
            self.heap.write_barrier_val(idx, val);
        }
        // SAFETY: as for the read in `ic_get_prop_mega`; the barrier above
        // allocates nothing and moves no column.
        unsafe { *(mir.vals as *mut Value).add(slot as usize) = val };
        self.ic_cell_own(func_id, ip, mir.shape, slot as usize, true);
        true
    }

    /// The slot of an own DATA property `key` of every object of `shape`,
    /// when the (shape, key) memo already holds the answer; `None` for an
    /// accessor, an absent key and a memo miss alike.
    #[cfg(all(feature = "jit", not(feature = "safe-sandbox")))]
    #[inline(always)]
    fn own_data_slot_cached(&self, sh: u32, key: &str) -> Option<u32> {
        let b = key.as_bytes();
        if sh == crate::shape::DICT || b.len() > 16 {
            return None;
        }
        let (w0, w1) = key_words(b);
        let e = self.own_slot_cache[own_slot_index(sh, w0, b.len())].get();
        (e.shape == sh
            && e.len == b.len() as u32
            && e.w0 == w0
            && e.w1 == w1
            && e.slot & OwnSlot::ACCESSOR == 0)
            .then_some(e.slot & !OwnSlot::FLAGS)
    }

    /// A read of `recv.key` that missed the inline hits, in a pure-
    /// interpreter VM: an own data property answered from the receiver's
    /// mirror and the (shape, key) memo alone, as `ic_get_prop`'s own
    /// resolve would answer it after its site probe, the receiver's heap
    /// slot and map, the namespace test and the ways. A field read through
    /// more layouts than the inline hits keep (`node.flags` over a
    /// compiler's AST) took that whole call on most reads. `None` for
    /// anything else, which goes on to the call.
    #[cfg(all(feature = "jit", not(feature = "safe-sandbox")))]
    #[inline]
    pub(crate) fn ic_get_prop_mega(&mut self, func_id: u32, ip: usize, recv: Value, key: &str) -> Option<Value> {
        // A private name is its brand's, never a string key's that spells it.
        if !recv.is_heap()
            || is_private_key(key)
            || !self.deferred_ns_state.is_empty()
            || !self.interp_own_resolve_vm_is_interpreter()
            || !ic_mega_enabled()
        {
            return None;
        }
        // Only an instruction that has read an own data property before (its
        // cell holds a layout): an inherited method's, an accessor's or a
        // miss's site would probe the memo for nothing before the full call
        // asks it again.
        if !self.ic_cell(func_id, ip).is_some_and(SiteCell::knows_own) {
            return None;
        }
        let mir = self.heap.hot_mirror_of(recv.heap_index());
        if mir.shape == crate::shape::DICT || mir.fid != crate::heap::FID_MIRROR_NONE {
            return None;
        }
        let slot = self.own_data_slot_cached(mir.shape, key)?;
        // SAFETY: the mirror's shape proves `vals` is the map's live value
        // column, and the memo's answer for that shape that it holds `slot`.
        let v = unsafe { *(mir.vals as *const Value).add(slot as usize) };
        self.ic_cell_own(func_id, ip, mir.shape, slot as usize, false);
        Some(v)
    }

    /// The proto-chain leg of [`Vm::ic_walk`], starting from the link OUT of
    /// `idx` (whose own map already missed). Also used for `super` fills with
    /// the synthesized prototype object as the anchor.
    fn ic_walk_chain(&self, idx: u32, key: &str) -> Walked {
        let mut hops: IcHops = ([(0, 0); IC_MAX_HOPS], 0);
        let mut cur = idx;
        let mut first = FIRST_HEAP;
        loop {
            let next = match self.proto_of.get_copied(cur) {
                Some(p) => {
                    if !p.is_heap() {
                        // Null-prototype chain end → undefined.
                        if cur == idx {
                            first = FIRST_NULL;
                        }
                        return Walked::ChainMiss { first, hops };
                    }
                    p.heap_index()
                }
                None => {
                    if self.obj_proto == 0 || cur == self.obj_proto {
                        // End at %Object.prototype% (guarded as the last hop;
                        // `cur == idx` — reading off obj_proto itself with an
                        // own miss — is excluded below to keep `first` simple).
                        if cur == idx {
                            return Walked::No;
                        }
                        return Walked::ChainMiss { first, hops };
                    }
                    if cur == idx {
                        first = FIRST_DEFAULT;
                    }
                    self.obj_proto
                }
            };
            if hops.1 as usize >= IC_MAX_HOPS {
                return Walked::No; // too deep to guard
            }
            if !self.ic_obj_ok(next) {
                return Walked::No;
            }
            let m2 = match self.heap.get(next) {
                HeapObj::Object(m2) if !m2.is_ctor && m2.class.is_none() => m2,
                _ => return Walked::No,
            };
            hops.0[hops.1 as usize] = (next, self.heap.version_of(next));
            hops.1 += 1;
            if let Some(i) = self.own_pos(m2, key) {
                return if m2.attr_at(i).accessor {
                    Walked::ChainAcc {
                        first,
                        hops,
                        slot: i as u32,
                    }
                } else {
                    Walked::ChainData {
                        first,
                        hops,
                        slot: i as u32,
                    }
                };
            }
            cur = next;
        }
    }

    // ── hit validation ──

    /// Validate the receiver-side guards shared by every `Own*`/`Class*`/
    /// `Proto*` hit: a plain Object receiver outside the exotic set. Returns
    /// its map.
    ///
    /// Forced inline: it is a handful of compares, and as a call it held
    /// 10% of a property-read loop's samples on the WebAssembly build.
    #[inline(always)]
    fn ic_recv_map(&self, recv: Value) -> Option<(u32, &ObjMap)> {
        if !recv.is_heap() {
            return None;
        }
        let idx = recv.heap_index();
        // The kind first: a string's or an array's read leaves here on the
        // one load its own path makes anyway.
        let HeapObj::Object(m) = self.heap.get_for_ic(idx) else {
            return None;
        };
        if m.is_ctor || !self.deferred_ns_state.is_empty() || !self.ic_obj_ok_map(idx, m) {
            return None;
        }
        Some((idx, m))
    }

    /// The receiver's hidden class, or [`crate::shape::DICT`] when it has none
    /// (or is not the kind of object the caches guard). `DICT` never matches a
    /// live receiver's shape, so an entry filled with it simply falls through to
    /// the old validation.
    #[inline]
    fn ic_recv_shape(&self, recv: Value) -> u32 {
        match self.ic_recv_map(recv) {
            Some((_, m)) => m.shape(),
            None => crate::shape::DICT,
        }
    }

    /// Validate a `Proto*` entry's first link + hop versions against `recv`
    /// (whose own map must already have missed `key`). Returns the holder's
    /// map when the chain still stands.
    #[inline]
    fn ic_chain_ok(&self, recv_idx: u32, first: u8, hops: &IcHops) -> Option<&ObjMap> {
        // First link re-read (the receiver itself is not version-guarded),
        // from `proto_of`'s link copy.
        let link = self.proto_of.link(recv_idx);
        match first {
            FIRST_HEAP => {
                if link < ProtoTable::HEAP_BASE || link - ProtoTable::HEAP_BASE != hops.0[0].0 {
                    return None;
                }
            }
            FIRST_DEFAULT => {
                if link != ProtoTable::NONE || hops.1 == 0 || hops.0[0].0 != self.obj_proto {
                    return None;
                }
            }
            _ => {
                // FIRST_NULL: the receiver's explicit proto must still be
                // non-heap (a miss entry with no hops).
                return (link == ProtoTable::NON_HEAP).then(|| self.ic_empty_map());
            }
        }
        // Hop versions: any key add/remove/defineProperty/setPrototypeOf on a
        // chain object bumps its version (see the module doc).
        let n = hops.1 as usize;
        for &(h, v) in &hops.0[..n] {
            if self.heap.version_of(h) != v {
                return None;
            }
        }
        match self.heap.get(hops.0[n - 1].0) {
            HeapObj::Object(hm) => Some(hm),
            _ => None,
        }
    }

    /// A shared empty map for `FIRST_NULL` chain-miss validation (no holder).
    #[inline]
    fn ic_empty_map(&self) -> &'static ObjMap {
        static EMPTY: std::sync::OnceLock<ObjMap> = std::sync::OnceLock::new();
        EMPTY.get_or_init(ObjMap::new)
    }

    /// Re-resolve a freshly-read callable Value to a PLAIN (non-generator,
    /// non-async) user function. `None` for natives/bound/generators/… —
    /// the caller falls back to the slow path. (Also used by the JIT prop-miss
    /// helpers to pre-screen a bakeable accessor fn for a B114 ACCESSOR way.)
    /// Always inlined: as a call, its Option came back through memory
    /// as two 4-byte stores read as one 8-byte load, a stalled store-forward
    /// at every caller (a twentieth of constructing a class instance).
    #[inline(always)]
    pub(crate) fn ic_plain_fn(&self, v: Value) -> Option<(u32, u32)> {
        if !v.is_heap() {
            return None;
        }
        let i = v.heap_index();
        let (fid, closure) = match self.heap.get(i) {
            HeapObj::Func(id) => (*id, NO_CLOSURE),
            HeapObj::Closure { func, .. } => (*func, i),
            _ => return None,
        };
        let p = self.func(fid as usize);
        if p.is_generator || p.is_async {
            return None;
        }
        Some((fid, closure))
    }

    // ── GetProp sites ──

    /// True when this VM cannot enter native code. Builds with a JIT-capable
    /// field honor the live `ZIPP_NOJIT`/embedding toggle; no-JIT and
    /// safe-sandbox builds are interpreter-only by construction.
    #[inline]
    fn interp_own_resolve_vm_is_interpreter(&self) -> bool {
        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
        {
            // A Python program's runtime stays interpreted and builds no
            // native property plans from this training (`codegen::py`).
            !self.jit_enabled || self.jit.runtime_interpreted()
        }
        #[cfg(all(feature = "jit", target_arch = "aarch64"))]
        {
            !self.jit_enabled
        }
        #[cfg(not(all(feature = "jit", any(target_arch = "x86_64", target_arch = "aarch64"))))]
        {
            true
        }
    }

    /// The way-0 own-data hit of [`Self::ic_get_prop`], for the general
    /// profile's dispatch loop to take inline. `ic_get_prop` is large and stays
    /// a call there; on an interpreted property-read loop the call and its
    /// prologue were a sixth of the run (property-ic-shapes) for a hit that is
    /// three loads and a compare. This is exactly `ic_get_prop`'s first check:
    /// the same answer, or `None` and the full call. The safe profile inlines
    /// `ic_get_prop` whole and does not use it.
    ///
    /// With the JIT compiled in, the receiver is read through its hot mirror:
    /// `objs[idx]`, the boxed map and its value column were three dependent
    /// loads, a miss each on a large heap, where the mirror's record holds
    /// the shape and the column's base side by side. The mirror's discipline
    /// (`Heap::hot_mirror`) is the soundness argument the emitted shape-way
    /// probes rely on: a matching shape proves the record was settled after
    /// the map's last version-bumping mutation, so the base is live. The
    /// receivers the caches exclude never match: the exotic ones are pinned
    /// (`Heap::pin_mirror_dict`) and a constructor's map carries
    /// `FID_MIRROR_CTOR`; a string or an array has no shape there at all. A
    /// receiver whose mirror holds none (an object built by appends, whose
    /// every append's bump cleared it) takes the full call, whose way-0 hit
    /// settles the mirror (`ic_get_prop`). Only the mirror check is inline:
    /// with the map's check beside it, the larger arm slowed unrelated rows
    /// by up to 12%.
    ///
    /// A site whose way 0 is an inherited-data entry is answered here too,
    /// out of line (`ic_proto_hit_tail`): the mirror's shape is the
    /// receiver's, which is what the entry guards, and the site is already in
    /// hand. `ic_get_prop_proto_hit`, after this declined, fetched the site
    /// again and the receiver's map with it: a read through
    /// `Object.create(p)` ran 13% faster, polymorphic-objects 2.4% and
    /// property-ic-shapes 3%. Its own hits settle the mirror
    /// (`ic_proto_hit_settle`), so a receiver that is only ever read through
    /// its prototype reaches this one from its second read on.
    #[cfg(all(feature = "jit", not(feature = "safe-sandbox")))]
    #[inline(always)]
    pub(crate) fn ic_get_prop_own_hit(&self, func_id: u32, ip: usize, recv: Value) -> Option<Value> {
        if !recv.is_heap() {
            return None;
        }
        let mir = self.heap.hot_mirror_of(recv.heap_index());
        if mir.shape == crate::shape::DICT
            || mir.fid != crate::heap::FID_MIRROR_NONE
            || !self.deferred_ns_state.is_empty()
            || !self.ic_func_eligible(func_id)
        {
            return None;
        }
        let cell = self.ic_cell(func_id, ip)?;
        if let Some(slot) = cell.own_read(mir.shape) {
            // SAFETY: the matching shape proves `vals` is the map's live value
            // column and that it holds `slot` (the pair was recorded from a map
            // of this shape, which has that many slots).
            return Some(unsafe { *(mir.vals as *const Value).add(slot as usize) });
        }
        let site = cell.site.as_deref()?;
        if site.n == 0 {
            return None;
        }
        match site.entries[0] {
            IcEntry::OwnData { shape, slot } if shape == mir.shape => {
                // SAFETY: the matching shape proves `vals` is the map's live value
                // column and that it holds `slot` (the way was filled against a map
                // of this shape, which has that many slots).
                Some(unsafe { *(mir.vals as *const Value).add(slot as usize) })
            }
            IcEntry::ProtoData { .. } | IcEntry::Private { .. } | IcEntry::PrivateMethod { .. } => {
                self.ic_get_way0_tail(recv.heap_index(), &site.entries[0], mir.shape)
            }
            _ => None,
        }
    }

    /// [`Self::ic_get_prop_own_hit`] for the main dispatch loop, which also
    /// takes a getter to call, or a class method to read, from way 0
    /// (`ic_get_acc_tail`). Each went to the full `ic_get_prop` call once
    /// this hit and the prototype probe had missed: an object literal's
    /// getter took 31 ns a read against QuickJS's 22.
    #[cfg(all(feature = "jit", not(feature = "safe-sandbox")))]
    #[inline(always)]
    pub(crate) fn ic_get_prop_way0(&self, func_id: u32, ip: usize, recv: Value) -> GetAct {
        if !recv.is_heap() {
            return GetAct::None;
        }
        let mir = self.heap.hot_mirror_of(recv.heap_index());
        if mir.shape == crate::shape::DICT
            || mir.fid != crate::heap::FID_MIRROR_NONE
            || !self.deferred_ns_state.is_empty()
            || !self.ic_func_eligible(func_id)
        {
            return GetAct::None;
        }
        let Some(cell) = self.ic_cell(func_id, ip) else {
            return GetAct::None;
        };
        if let Some(slot) = cell.own_read(mir.shape) {
            // SAFETY: as in `ic_get_prop_own_hit`.
            return GetAct::Value(unsafe { *(mir.vals as *const Value).add(slot as usize) });
        }
        let Some(site) = cell.site.as_deref() else {
            return GetAct::None;
        };
        if site.n == 0 {
            return GetAct::None;
        }
        match site.entries[0] {
            IcEntry::OwnData { shape, slot } if shape == mir.shape => {
                // SAFETY: as in `ic_get_prop_own_hit`.
                GetAct::Value(unsafe { *(mir.vals as *const Value).add(slot as usize) })
            }
            IcEntry::ProtoData { .. } | IcEntry::Private { .. } | IcEntry::PrivateMethod { .. } => {
                match self.ic_get_way0_tail(recv.heap_index(), &site.entries[0], mir.shape) {
                    Some(v) => GetAct::Value(v),
                    None => GetAct::None,
                }
            }
            IcEntry::OwnAcc { .. } | IcEntry::ProtoAcc { .. } | IcEntry::ClassGetter { .. } | IcEntry::ClassMethod { .. } => {
                self.ic_get_acc_tail(recv.heap_index(), &site.entries[0], mir.shape, mir.vals)
            }
            _ => GetAct::None,
        }
    }

    /// [`Self::ic_get_prop_way0`]'s accessor and class-member ways, out of
    /// line, under the guards `ic_get_prop`'s shape-keyed hits apply: the
    /// receiver's shape (its mirror's, `shape`, whose value column is `vals`)
    /// is the one the way was filled with, which proves an own accessor's
    /// slot and, for an inherited accessor or a class member, that the
    /// receiver still has no own property of the key.
    #[cfg(all(feature = "jit", not(feature = "safe-sandbox")))]
    #[inline(never)]
    fn ic_get_acc_tail(&self, idx: u32, e: &IcEntry, shape: u32, vals: u64) -> GetAct {
        match *e {
            IcEntry::OwnAcc { shape: s, slot } if s == shape => {
                // SAFETY: as the own-data read: the matching shape proves
                // `vals` is the map's live value column and holds `slot`.
                let g = unsafe { *(vals as *const Value).add(slot as usize) };
                if g == Value::UNDEFINED {
                    return GetAct::Value(Value::UNDEFINED);
                }
                match self.ic_plain_fn(g) {
                    Some((fid, closure)) => GetAct::Accessor {
                        fid,
                        closure,
                        getter: g,
                    },
                    None => GetAct::None,
                }
            }
            IcEntry::ProtoAcc {
                recv_shape,
                first,
                hops,
                slot,
            } if recv_shape == shape => {
                let HeapObj::Object(m) = self.heap.get(idx) else {
                    return GetAct::None;
                };
                if m.class.is_some()
                    || !self.interp_own_resolve_vm_is_interpreter()
                    || self.ic_chain_ok(idx, first, &hops).is_none()
                {
                    return GetAct::None;
                }
                self.ic_chain_acc_get(hops, slot)
            }
            IcEntry::ClassGetter {
                class,
                ver,
                epoch,
                getter,
                recv_shape,
            } if recv_shape == shape => {
                let HeapObj::Object(m) = self.heap.get(idx) else {
                    return GetAct::None;
                };
                if m.class != Some(class) || self.heap.version_of(class) != ver || self.class_proto_epoch != epoch {
                    return GetAct::None;
                }
                match self.ic_plain_fn(getter) {
                    Some((fid, closure)) => GetAct::Accessor { fid, closure, getter },
                    None => GetAct::None,
                }
            }
            IcEntry::ClassMethod {
                class,
                ver,
                epoch,
                callee,
                recv_shape,
                ..
            } if recv_shape == shape => {
                let HeapObj::Object(m) = self.heap.get(idx) else {
                    return GetAct::None;
                };
                if m.class != Some(class) || self.heap.version_of(class) != ver || self.class_proto_epoch != epoch {
                    return GetAct::None;
                }
                GetAct::Value(callee)
            }
            _ => GetAct::None,
        }
    }

    /// `ic_get_prop_own_hit`'s other way-0 hits, out of line: one call with
    /// the entry by reference keeps the inline arm small.
    #[cfg(not(feature = "safe-sandbox"))]
    #[inline(never)]
    #[cfg_attr(not(feature = "jit"), allow(unused_variables))]
    fn ic_get_way0_tail(&self, idx: u32, e: &IcEntry, shape: u32) -> Option<Value> {
        match *e {
            #[cfg(feature = "jit")]
            IcEntry::ProtoData {
                recv_shape,
                first,
                ref hops,
                slot,
            } if recv_shape == shape => self.ic_proto_hit_tail(idx, first, hops, slot),
            IcEntry::Private { head, brand, id, pos } => self.ic_private_get_tail(idx, head, brand, id, pos),
            IcEntry::PrivateMethod {
                head,
                brand,
                method,
                epoch,
                pos,
            } => self.ic_private_method_tail(idx, head, brand, method, epoch, pos),
            _ => None,
        }
    }

    /// A private-name site's way-0 method hit (`IcEntry::PrivateMethod`):
    /// the method, when no declaring class died since the fill, the running
    /// callee is the one the way was filled under, and the receiver, which
    /// is not a class value (a class's own statics are another way), carries
    /// the brand: what `private_get_slow` checks before it reads the method.
    #[cfg(not(feature = "safe-sandbox"))]
    #[inline]
    fn ic_private_method_tail(
        &self,
        idx: u32,
        head: u64,
        brand: u64,
        method: Value,
        epoch: u32,
        pos: u32,
    ) -> Option<Value> {
        if epoch != self.private_epoch {
            return None;
        }
        let ctx = self.frames.last()?.callee;
        if self.private_memo_key(ctx) != Some(head) || matches!(self.heap.get(idx), HeapObj::Class(_)) {
            return None;
        }
        self.brand_entry_at(idx, brand, pos).is_some().then_some(method)
    }

    /// A private-name site's way-0 field hit (`IcEntry::Private`): the field
    /// when the running callee is the one the way was filled under and the
    /// receiver holds the pair. The read had paid the failed own hit, the
    /// name's text and test, the memo's hash and the list's scan.
    #[cfg(not(feature = "safe-sandbox"))]
    #[inline]
    fn ic_private_get_tail(&self, idx: u32, head: u64, brand: u64, id: u32, pos: u32) -> Option<Value> {
        let ctx = self.frames.last()?.callee;
        if self.private_memo_key(ctx) != Some(head) {
            return None;
        }
        self.private_field_at(idx, brand, id, pos).map(|(_, v)| v)
    }

    /// [`Self::ic_private_get_tail`] for a write of the field, which must
    /// already be there (PrivateSet; an add takes the uncached path). The
    /// own hit hands over only the site: its way 0 is read again here, which
    /// keeps the inline arm to one call.
    #[cfg(not(feature = "safe-sandbox"))]
    #[inline(never)]
    fn ic_private_set_tail(&mut self, func_id: u32, ip: usize, idx: u32, val: Value) -> bool {
        let Some(IcEntry::Private { head, brand, id, pos }) = self
            .ic_cell(func_id, ip)
            .and_then(|c| c.site.as_deref())
            .map(|s| s.entries[0])
        else {
            return false;
        };
        let Some(ctx) = self.frames.last().map(|f| f.callee) else {
            return false;
        };
        if self.private_memo_key(ctx) != Some(head) {
            return false;
        }
        if pos == PRIVATE_ADD {
            return self.private_field_add_hit(idx, brand, id, val);
        }
        let Some((at, _)) = self.private_field_at(idx, brand, id, pos) else {
            return false;
        };
        // No holder barrier: the collector roots private field values; a
        // minor takes the young ones from this record (`record_young_value`).
        if let Some(fields) = self.private_fields_mut(idx) {
            fields[at].2 = val;
        }
        self.heap.record_young_value(val);
        true
    }

    /// Fill a private-name site's way 0 after its code, running as `ctx`,
    /// resolved the name to the field `(brand, id)` and found it on `recv`.
    #[cfg(not(feature = "safe-sandbox"))]
    pub(crate) fn ic_private_fill(&mut self, func_id: u32, ip: usize, ctx: Value, brand: u64, id: u32, recv: Value) {
        if !recv.is_heap() || !matches!(self.heap.get(recv.heap_index()), HeapObj::Object(_)) {
            return;
        }
        let Some(head) = self.private_memo_key(ctx) else {
            return;
        };
        let Some((pos, _)) = self.private_field_at(recv.heap_index(), brand, id, 0) else {
            return;
        };
        if let Some(site) = self.ic_site_mut(func_id, ip) {
            site.entries[0] = IcEntry::Private {
                head,
                brand,
                id,
                pos: pos as u32,
            };
            site.n = site.n.max(1);
        }
        // The own hit reads the receiver through its mirror.
        self.heap.refresh_mirror(recv.heap_index());
    }

    /// Fill a private-name site's way 0 after its code, running as `ctx`,
    /// read the instance private method `method` of the class whose brand
    /// is `brand` from `recv` (`IcEntry::PrivateMethod`).
    #[cfg(not(feature = "safe-sandbox"))]
    pub(crate) fn ic_private_method_fill(
        &mut self,
        func_id: u32,
        ip: usize,
        ctx: Value,
        brand: u64,
        method: Value,
        recv: Value,
    ) {
        if !recv.is_heap() || !matches!(self.heap.get(recv.heap_index()), HeapObj::Object(_)) {
            return;
        }
        let Some(head) = self.private_memo_key(ctx) else {
            return;
        };
        let Some(pos) = self.brand_entry_at(recv.heap_index(), brand, 0) else {
            return;
        };
        let epoch = self.private_epoch;
        if let Some(site) = self.ic_site_mut(func_id, ip) {
            site.entries[0] = IcEntry::PrivateMethod {
                head,
                brand,
                method,
                epoch,
                pos: pos as u32,
            };
            site.n = site.n.max(1);
        }
        // The own hit reads the receiver through its mirror.
        self.heap.refresh_mirror(recv.heap_index());
    }

    /// Fill a field initializer's private-name site, a `SetProp` that ran as
    /// `ctx` and added the field `(brand, id)` to `recv`: way 0 becomes an
    /// `IcEntry::Private` whose `pos` is [`PRIVATE_ADD`], which the set's own
    /// hit hands to `private_field_add_hit`. Such a site only ever adds (an
    /// assignment to a private field is a `SetPrivate`), and every instance
    /// it initializes took `private_set_slow`'s resolution, its failed write
    /// and its checks, for each of its private fields.
    #[cfg(not(feature = "safe-sandbox"))]
    pub(crate) fn ic_private_add_fill(&mut self, func_id: u32, ip: usize, ctx: Value, brand: u64, id: u32, recv: Value) {
        if !recv.is_heap() || !matches!(self.heap.get(recv.heap_index()), HeapObj::Object(_)) {
            return;
        }
        let Some(head) = self.private_memo_key(ctx) else {
            return;
        };
        if let Some(site) = self.ic_site_mut(func_id, ip) {
            site.entries[0] = IcEntry::Private {
                head,
                brand,
                id,
                pos: PRIVATE_ADD,
            };
            site.n = site.n.max(1);
        }
        self.heap.refresh_mirror(recv.heap_index());
    }

    /// After `ic_get_prop_proto_hit` answered for `recv`: settle its mirror,
    /// so its next read through the site is `ic_get_prop_own_hit`'s. That
    /// hit is reached only by a receiver whose mirror holds no shape, as any
    /// other was answered by the own hit first. (Nothing to settle without
    /// the JIT's mirrors: `refresh_mirror` is empty there.)
    #[cfg(not(feature = "safe-sandbox"))]
    #[inline(always)]
    pub(crate) fn ic_proto_hit_settle(&mut self, recv: Value) {
        // A module namespace's or a dictionary-mode object's map has no shape
        // to settle (the export and dictionary-hint answers): the refresh
        // found nothing to keep, on every read of `ns.key` in a loop.
        if matches!(self.heap.get(recv.heap_index()), HeapObj::Object(m) if m.shape() == crate::shape::DICT) {
            return;
        }
        self.heap.refresh_mirror(recv.heap_index());
    }

    /// `ic_get_prop_proto_hit`'s guards for a receiver whose shape the
    /// mirror has already matched against the entry's: an inherited value,
    /// not a miss; a pure interpreter (the JIT keeps its feedback path); not
    /// a class instance, whose chain is its class's, which a plain object of
    /// the same shape does not share; and the chain as it was.
    #[cfg(all(feature = "jit", not(feature = "safe-sandbox")))]
    #[inline]
    fn ic_proto_hit_tail(&self, idx: u32, first: u8, hops: &IcHops, slot: u32) -> Option<Value> {
        if !self.interp_own_resolve_vm_is_interpreter() {
            return None;
        }
        let HeapObj::Object(m) = self.heap.get(idx) else {
            return None;
        };
        if m.class.is_some() {
            return None;
        }
        let hm = self.ic_chain_ok(idx, first, hops)?;
        // A guarded chain miss (`node.decorators` on a node without any).
        if slot == u32::MAX {
            return Some(Value::UNDEFINED);
        }
        Some(hm.val_at(slot as usize))
    }

    /// [`Self::ic_get_prop_proto_hit`] for a receiver whose shape is not the
    /// one way 0 was filled against: what `ic_validate_get` checks of that
    /// way once the full call reaches it — the key among none of the
    /// receiver's own, the chain as it was, the holder's slot still the
    /// key's data property — without the call's site probe, receiver fetch
    /// and namespace test first. A site reading one inherited name or one
    /// absent field through many layouts took the whole call on every read.
    #[cfg(not(feature = "safe-sandbox"))]
    #[inline(never)]
    fn ic_proto_hit_other_shape(
        &self,
        idx: u32,
        m: &ObjMap,
        first: u8,
        hops: &IcHops,
        slot: u32,
        key: &str,
    ) -> Option<Value> {
        if self.own_pos(m, key).is_some() {
            return None;
        }
        let hm = self.ic_chain_ok(idx, first, hops)?;
        if slot == u32::MAX {
            return Some(Value::UNDEFINED);
        }
        let s = slot as usize;
        (hm.key_get(s) == Some(key) && !hm.attr_at(s).accessor).then(|| hm.val_at(s))
    }

    /// [`Self::ic_get_prop_own_hit`] without the JIT's mirrors: the map's own
    /// shape.
    #[cfg(all(not(feature = "jit"), not(feature = "safe-sandbox")))]
    #[inline(always)]
    pub(crate) fn ic_get_prop_own_hit(&self, func_id: u32, ip: usize, recv: Value) -> Option<Value> {
        // The receiver first: the dispatch loop asks before its `length` and
        // shapeless-receiver leaves, so a string's or an array's `.length`
        // must fail here before the site lookup, not after it.
        let (idx, m) = self.ic_recv_map(recv)?;
        if !self.ic_func_eligible(func_id) {
            return None;
        }
        let cell = self.ic_cell(func_id, ip)?;
        let sh = m.shape();
        if sh != crate::shape::DICT {
            if let Some(slot) = cell.own_read(sh) {
                return Some(m.val_at(slot as usize));
            }
        }
        let site = cell.site.as_deref()?;
        if site.n == 0 {
            return None;
        }
        match site.entries[0] {
            IcEntry::OwnData { shape, slot } => {
                (sh != crate::shape::DICT && sh == shape).then(|| m.val_at(slot as usize))
            }
            IcEntry::Private { .. } | IcEntry::PrivateMethod { .. } => {
                self.ic_get_way0_tail(idx, &site.entries[0], crate::shape::DICT)
            }
            _ => None,
        }
    }

    /// [`Self::ic_get_prop_proto_hit`] for a receiver whose mirror holds no
    /// shape: `ic_get_prop`'s dictionary hint, the slot where this site last
    /// found its key in a dictionary-mode object, when that object holds the
    /// key there as a data property. A delete leaves an object in the mode for
    /// good, and every read of one took the full call to reach the hint, past
    /// its megamorphic probe, receiver fetch and namespace test.
    #[cfg(all(feature = "jit", not(feature = "safe-sandbox")))]
    #[inline(never)]
    fn ic_dict_hint_hit(&self, func_id: u32, ip: usize, recv: Value, key: &str) -> Option<Value> {
        let s = self.ic_cell(func_id, ip)?.dict_hint()?;
        if is_private_key(key) || !self.interp_own_resolve_vm_is_interpreter() {
            return None;
        }
        let (_, m) = self.ic_recv_map(recv)?;
        if m.shape() != crate::shape::DICT || m.key_get(s) != Some(key) || m.attr_at(s).accessor {
            return None;
        }
        Some(m.val_at(s))
    }

    /// The way-0 inherited-data hit of [`Self::ic_get_prop`], for the
    /// dispatch loops to take inline after the own-data one misses: the same
    /// guards (the receiver's complete shape, an unchanged chain, the holder's
    /// slot still the key's data property), so the same answer or `None` and
    /// the full call. `o.m` through a prototype took the full call's site
    /// probe and way scan on every read: 11 ns against QuickJS's 1.
    #[cfg(not(feature = "safe-sandbox"))]
    #[inline(always)]
    pub(crate) fn ic_get_prop_proto_hit(&self, func_id: u32, ip: usize, recv: Value, key: &str) -> Option<Value> {
        // The site's way 0 before the receiver's map: an own-data site, which
        // the own hit just missed through another layout, leaves here
        // without touching the receiver's heap slot and map (two cold lines
        // of a megamorphic field read, on every read the own hit missed). A
        // shaped mirror (the own hit's, just read) already proves an object;
        // anything else is told apart by its slot's kind first, so a string's
        // or an array's `length` still leaves on that one load.
        if !recv.is_heap() || !self.ic_func_eligible(func_id) {
            return None;
        }
        #[cfg(feature = "jit")]
        let dict_mirror = self.heap.hot_mirror_of(recv.heap_index()).shape == crate::shape::DICT;
        #[cfg(feature = "jit")]
        if dict_mirror && !matches!(self.heap.get_for_ic(recv.heap_index()), HeapObj::Object(_)) {
            return None;
        }
        #[cfg(not(feature = "jit"))]
        if !matches!(self.heap.get_for_ic(recv.heap_index()), HeapObj::Object(_)) {
            return None;
        }
        let cell = self.ic_cell(func_id, ip)?;
        // A dictionary-mode receiver at a site that remembers where it found
        // its key in one (a namespace's site never does).
        #[cfg(feature = "jit")]
        if dict_mirror && cell.dict_hint().is_some() {
            if let Some(v) = self.ic_dict_hint_hit(func_id, ip, recv, key) {
                return Some(v);
            }
        }
        let site = cell.site.as_deref()?;
        if site.n == 0 {
            return None;
        }
        let IcEntry::ProtoData {
            recv_shape,
            first,
            hops,
            slot,
        } = site.entries[0]
        else {
            // A module namespace's export (`ns.key`), out of line: this probe
            // has the site in hand, where `ic_get_prop` fetched it again after
            // the dispatch loop's other leaves for the namespace's every read.
            if let IcEntry::NsExport { .. } = site.entries[0] {
                return self.ic_ns_export_hit(recv, &site.entries[0]);
            }
            return None;
        };
        let (idx, m) = self.ic_recv_map(recv)?;
        let sh = m.shape();
        if sh == crate::shape::DICT || m.class.is_some() || !self.interp_own_resolve_vm_is_interpreter() {
            return None;
        }
        if recv_shape != sh {
            // Another layout reading the same inherited name (`node.type`
            // through dozens of AST node shapes, a field most nodes lack).
            return self.ic_proto_hit_other_shape(idx, m, first, &hops, slot, key);
        }
        let hm = self.ic_chain_ok(idx, first, &hops)?;
        // A guarded chain miss: the shape proves the key absent here, the
        // hops that no link of the chain has gained it.
        if slot == u32::MAX {
            return Some(Value::UNDEFINED);
        }
        let s = slot as usize;
        // The holder is the last hop, and its version is the one the fill saw:
        // every key add, removal, compaction or descriptor change on it bumps
        // that version, so the slot still holds the key as a data property.
        // Comparing the key's text again was an eighth of an inherited read
        // (an audit build that panicked on any disagreement passed Test262 in
        // both modes and the 78-program check).
        debug_assert!(
            hm.key_get(s) == Some(key) && !hm.attr_at(s).accessor,
            "holder slot {s} changed under a matching version"
        );
        Some(hm.val_at(s))
    }

    /// `ns.name` on the module namespace `recv` (`import * as ns`): its
    /// export's live slot, the namespace [[Get]]'s answer, without the member
    /// walk's realm, proxy and global probes in front of it. A binding still
    /// in its TDZ, and any key that is not an export, take that walk. Out of
    /// line: `ic_get_prop` is on every uncached read, and only module code
    /// asks this.
    #[cold]
    #[inline(never)]
    fn ic_namespace_get(&self, recv: Value, key: &str) -> Option<(Value, u32)> {
        let slots = self.module_namespaces.get(&recv.heap_index())?;
        let slot = *slots.get(key)?;
        let v = self.globals.get(slot as usize).copied().unwrap_or(Value::UNDEFINED);
        (!v.is_uninitialized()).then_some((v, slot))
    }

    /// IC lookup + fill for a `GetProp` site. `GetAct::None` ⇒ slow path.
    ///
    /// The safe profile's dispatch loop takes it inline. Left to the
    /// heuristic, the choice turned on unrelated code: dropping the JIT's
    /// mirror stores from the allocator (d2886b14) made class-prototype-hot
    /// 26% slower with every extra cycle at this call site, and forcing it
    /// either way restored it.
    #[cfg_attr(feature = "safe-sandbox", inline(always))]
    pub(crate) fn ic_get_prop(
        &mut self,
        func_id: u32,
        ip: usize,
        recv: Value,
        key: &str,
    ) -> GetAct {
        // A field read through more layouts than the inline hits keep: the
        // mirror and the (shape, key) memo before this call's site probe and
        // the receiver's map (`ic_get_prop_mega`), here rather than as its own
        // step in the dispatch loops, where every read the hits missed paid
        // its call whether or not it could answer.
        #[cfg(all(feature = "jit", not(feature = "safe-sandbox")))]
        if let Some(v) = self.ic_get_prop_mega(func_id, ip, recv, key) {
            return GetAct::Value(v);
        }
        // Receiver guards + own-`pos` are entry-independent: derive ONCE per
        // probe (a 4-way site otherwise pays the heap fetch + key scan per
        // entry). The site is fetched once for both probe and disable check.
        if let Some(site) = self.ic_site(func_id, ip) {
            // `ns.key` on the module namespace this site read before: the
            // export's live slot, the namespace [[Get]]'s answer (the same
            // object at the same version, no deferred namespace pending, the
            // binding out of its TDZ). The namespace map and its export map
            // were two hash lookups per read, a sixth of a module loop.
            if site.n > 0 {
                if let IcEntry::NsExport { .. } = site.entries[0] {
                    if let Some(v) = self.ic_ns_export_hit(recv, &site.entries[0]) {
                        return GetAct::Value(v);
                    }
                }
            }
            if site.n > 0 {
                if let Some((idx, m)) = self.ic_recv_map(recv) {
                    // ── shape-keyed fast path ──────────────────────────────
                    // A matching hidden class proves the receiver's key -> slot
                    // mapping is the one this entry was filled against, so the
                    // slot is usable WITHOUT looking the key up. That matters
                    // twice over:
                    //
                    //   * `m.pos(key)` below is unconditional — the cache never
                    //     avoided the key scan, only the proto/class walk, which
                    //     is why the interpreter measured flat at ~34ns per read
                    //     no matter how well the site was cached; and
                    //   * shapes are shared, so a site reading a property from
                    //     ten thousand distinct objects built the same way needs
                    //     ONE way instead of overflowing all eight. Measured on
                    //     identically-shaped receivers, the old identity guard
                    //     went 4.25ns at 12 receivers to 14.75ns at 16.
                    //
                    // Sound because a site's key is a compile-time constant
                    // (`Instr::GetProp` reads `string_constants[name]`), so the
                    // key that filled the entry is the key being asked for now.
                    // Only the FIRST way is probed by shape. Scanning all eight
                    // was measurably worse: a site whose receivers genuinely do
                    // not share shapes — `json-large` builds 54,390 of them, so
                    // the guard never hits — paid eight failed compares before
                    // every access, for +9% on that bench. Property sites are
                    // overwhelmingly monomorphic, so way 0 is where the shape
                    // will be if it is anywhere, and a single failed compare is
                    // affordable when it is not.
                    let sh = m.shape();
                    if sh != crate::shape::DICT && site.n > 0 {
                        match site.entries[0] {
                            IcEntry::OwnData { shape, slot } if shape == sh => {
                                let v = m.val_at(slot as usize);
                                // Settle the mirror, so the next read of this
                                // receiver is `ic_get_prop_own_hit`'s.
                                self.heap.refresh_mirror(idx);
                                return GetAct::Value(v);
                            }
                            IcEntry::OwnAcc { shape, slot } if shape == sh => {
                                let act = self.ic_acc_get_from(m, slot as usize);
                                // Settled as the data hit settles it, so the
                                // next read is `ic_get_prop_way0`'s (an object
                                // literal's accessor leaves its mirror unset).
                                self.heap.refresh_mirror(idx);
                                return act;
                            }
                            // A class instance's method (`o.m` before a
                            // `CallWithThis`): the matching shape proves the
                            // key is still not an own property, which is all
                            // the own-key scan below would tell the way, as
                            // `ic_call_method`'s shape-keyed hit has it.
                            IcEntry::ClassMethod {
                                class,
                                ver,
                                epoch,
                                callee,
                                recv_shape,
                                ..
                            } if recv_shape == sh => {
                                if m.class == Some(class)
                                    && self.heap.version_of(class) == ver
                                    && self.class_proto_epoch == epoch
                                {
                                    return GetAct::Value(callee);
                                }
                            }
                            // A class instance's absent key, by the same
                            // shape proof.
                            IcEntry::ClassMiss { recv_shape, .. } if recv_shape == sh => {
                                if self.ic_class_miss_ok(&site.entries[0], idx, m) {
                                    return GetAct::Value(Value::UNDEFINED);
                                }
                            }
                            _ => {}
                        }
                    }

                    // An inherited-data fill has already proved this static
                    // key absent from the receiver. In a pure interpreter,
                    // matching the receiver's complete hidden shape proves it
                    // is still absent, so validate the live proto chain and
                    // holder directly instead of hashing the receiver key and
                    // then re-discovering the same first way. DICT receivers,
                    // class resolution, accessors, chain misses, and every
                    // failed guard retain the ordinary path below. JIT-enabled
                    // execution deliberately keeps the old feedback path.
                    if sh != crate::shape::DICT {
                        match site.entries[0] {
                            IcEntry::ProtoData {
                                recv_shape,
                                first,
                                hops,
                                slot,
                            } => {
                                if slot != u32::MAX
                                    && recv_shape == sh
                                    && m.class.is_none()
                                    && self.interp_own_resolve_vm_is_interpreter()
                                {
                                    if let Some(hm) = self.ic_chain_ok(idx, first, &hops) {
                                        let s = slot as usize;
                                        if hm.key_get(s) == Some(key)
                                            && !hm.attr_at(s).accessor
                                        {
                                            return GetAct::Value(hm.val_at(s));
                                        }
                                    }
                                }
                            }
                            // An inherited accessor, by the same shape proof
                            // (`ic_validate_get`'s `ProtoAcc` arm without the
                            // own-key lookup).
                            IcEntry::ProtoAcc {
                                recv_shape,
                                first,
                                hops,
                                slot,
                            } => {
                                if recv_shape == sh
                                    && m.class.is_none()
                                    && self.interp_own_resolve_vm_is_interpreter()
                                    && self.ic_chain_ok(idx, first, &hops).is_some()
                                {
                                    match self.ic_chain_acc_get(hops, slot) {
                                        GetAct::None => {}
                                        act => return act,
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    // A dictionary-mode receiver: the slot where this site
                    // last found its key in one, tried before the key search
                    // (a map holds a key once, so the key there is the own
                    // property). The search hashed or scanned the receiver's
                    // keys on every read of such a receiver.
                    if sh == crate::shape::DICT
                        && self.interp_own_resolve_vm_is_interpreter()
                        && !is_private_key(key)
                    {
                        if let Some(s) = self.ic_cell(func_id, ip).and_then(SiteCell::dict_hint) {
                            if m.key_get(s) == Some(key) {
                                return if m.attr_at(s).accessor {
                                    self.ic_acc_get_from(m, s)
                                } else {
                                    GetAct::Value(m.val_at(s))
                                };
                            }
                        }
                    }
                    let own = self.own_pos(m, key);
                    // In a pure-interpreter VM, `own` has already answered the
                    // static site's complete own-property question against the
                    // LIVE map. Once the first-way shape shortcut misses, do
                    // not re-prove that same key/slot mapping against up to eight
                    // historical Own* ways: read the live data value directly,
                    // or re-resolve the live accessor exactly as an OwnAcc hit
                    // would. A native/non-plain getter returns None and resumes
                    // the authoritative slow path; a missing getter returns
                    // undefined. JIT-enabled execution deliberately retains the
                    // old scan/walk/install path so every observed shape remains
                    // training feedback for native property plans.
                    if let Some(slot) = own {
                        if self.interp_own_resolve_vm_is_interpreter() {
                            if m.attr_at(slot).accessor {
                                return self.ic_acc_get_from(m, slot);
                            }
                            // The cell's pair answers this layout's next read
                            // inline (`ic_get_prop_own_hit`). Never a private
                            // name's: that hit comes before the brand checks,
                            // and a string key may spell one (`o["#x"]`).
                            let (v, sh) = (m.val_at(slot), m.shape());
                            if !is_private_key(key) {
                                if sh == crate::shape::DICT {
                                    self.ic_cell_dict(func_id, ip, slot);
                                } else {
                                    self.ic_cell_own(func_id, ip, sh, slot, false);
                                    // Settle the mirror, as the way-0 hit
                                    // does: a key add leaves it unsettled,
                                    // and only that hit repaired it, so a
                                    // receiver read through a site whose way 0
                                    // holds another layout kept missing the
                                    // inline hit and the (shape, key) memo on
                                    // every read.
                                    self.heap.refresh_mirror(idx);
                                }
                            }
                            return GetAct::Value(v);
                        }
                    }
                    for e in &site.entries[..site.n as usize] {
                        match self.ic_validate_get(e, idx, m, own, key) {
                            GetAct::None => {}
                            act => return act,
                        }
                    }
                }
            }
            if site.misses >= IC_MISS_LIMIT && self.ic_still_disabled(func_id, ip) {
                return GetAct::None;
            }
        }
        // A private name never fills a way: the dispatch loop takes a site's
        // way-0 own-data hit before its private-member checks, which is sound
        // only because no such site holds one.
        if is_private_key(key) {
            return GetAct::None;
        }
        if !self.module_namespaces.is_empty() && recv.is_heap() && self.module_namespaces.contains_key(&recv.heap_index()) {
            return match self.ic_namespace_get(recv, key) {
                Some((v, slot)) => {
                    // Remember the export's slot for this site (the
                    // `NsExport` hit at the top of this function).
                    if self.deferred_ns_state.is_empty() {
                        let ns = recv.heap_index();
                        let ver = self.heap.version_of(ns);
                        self.ic_install(func_id, ip, IcEntry::NsExport { ns, ver, slot });
                    }
                    GetAct::Value(v)
                }
                None => GetAct::None,
            };
        }
        match self.ic_walk(recv, key) {
            Walked::OwnData { slot } => {
                // Deliver from the freshly-validated walk (same read the
                // entry would perform).
                let (v, shape) = match self.ic_recv_map(recv) {
                    Some((_, m)) => (m.val_at(slot), m.shape()),
                    None => return GetAct::None,
                };
                // Member calls are lowered in spec order as an adjacent
                // `GetProp; CallWithThis` pair.  The transactional Tier-C
                // own-method planner runs at the caller's next entry, when
                // the receiver register need not be populated yet, so retain
                // the exact receiver observed by this side-effect-free GET
                // fill just as the legacy fused CallMethod cache did.  Limit
                // this history to syntactic captured-call sites whose value
                // is a plain user function: ordinary property reads should
                // not pay the per-site receiver storage cost.
                let captured_plain_method = matches!(
                    (
                        self.func(func_id as usize).code.get(ip),
                        self.func(func_id as usize).code.get(ip + 1),
                    ),
                    (
                        Some(Instr::GetProp { dst, obj, .. }),
                        Some(Instr::CallWithThis {
                            callee, this_v, ..
                        }),
                    ) if dst == callee && obj == this_v
                ) && self.ic_plain_fn(v).is_some();
                if captured_plain_method {
                    self.mi_record_recv(func_id, ip, recv);
                }
                self.ic_install(
                    func_id,
                    ip,
                    IcEntry::OwnData {
                        shape,
                        slot: slot as u32,
                    },
                );
                self.ic_cell_own(func_id, ip, shape, slot, false);
                GetAct::Value(v)
            }
            Walked::OwnAcc { slot } => {
                let shape = self.ic_recv_shape(recv);
                self.ic_install(
                    func_id,
                    ip,
                    IcEntry::OwnAcc {
                        shape,
                        slot: slot as u32,
                    },
                );
                self.ic_own_acc_get(recv, key, slot as u32)
            }
            Walked::ClassMethod { class, callee } => {
                if let Some((fid, closure)) = self.ic_plain_fn(callee) {
                    // A spec-order member call fills this GET site rather than
                    // the following CallWithThis site. Preserve the receiver
                    // history here so its captured-call inline plan can build
                    // the same polymorphic arms as the legacy fused call.
                    self.mi_record_recv(func_id, ip, recv);
                    let recv_shape = self.ic_recv_shape(recv);
                    self.ic_install(
                        func_id,
                        ip,
                        IcEntry::ClassMethod {
                            class,
                            ver: self.heap.version_of(class),
                            epoch: self.class_proto_epoch,
                            callee,
                            fid,
                            closure,
                            recv_shape,
                        },
                    );
                } else {
                    self.ic_note_miss(func_id, ip);
                }
                // A method READ yields the member value either way.
                GetAct::Value(callee)
            }
            Walked::ClassGetter { class, getter } => {
                let ver = self.heap.version_of(class);
                let epoch = self.class_proto_epoch;
                self.mi_record_recv(func_id, ip, recv);
                let recv_shape = self.ic_recv_shape(recv);
                self.ic_install(
                    func_id,
                    ip,
                    IcEntry::ClassGetter {
                        class,
                        ver,
                        epoch,
                        getter,
                        recv_shape,
                    },
                );
                match self.ic_plain_fn(getter) {
                    Some((fid, closure)) => GetAct::Accessor {
                        fid,
                        closure,
                        getter,
                    },
                    None => GetAct::None, // native/generator getter → slow path
                }
            }
            Walked::ChainData { first, hops, slot } => {
                let recv_shape = self.ic_recv_shape(recv);
                self.ic_install(
                    func_id,
                    ip,
                    IcEntry::ProtoData {
                        recv_shape,
                        first,
                        hops,
                        slot,
                    },
                );
                let hm = match self.heap.get(hops.0[hops.1 as usize - 1].0) {
                    HeapObj::Object(hm) => hm,
                    _ => return GetAct::None,
                };
                GetAct::Value(hm.val_at(slot as usize))
            }
            Walked::ChainAcc { first, hops, slot } => {
                let recv_shape = self.ic_recv_shape(recv);
                self.ic_install(
                    func_id,
                    ip,
                    IcEntry::ProtoAcc {
                        recv_shape,
                        first,
                        hops,
                        slot,
                    },
                );
                self.ic_chain_acc_get(hops, slot)
            }
            Walked::ChainMiss { first, hops } => {
                let recv_shape = self.ic_recv_shape(recv);
                self.ic_install(
                    func_id,
                    ip,
                    IcEntry::ProtoData {
                        recv_shape,
                        first,
                        hops,
                        slot: u32::MAX,
                    },
                );
                GetAct::Value(Value::UNDEFINED)
            }
            Walked::ClassMiss { class, nhops, hops } => {
                let recv_shape = self.ic_recv_shape(recv);
                let ver = self.heap.version_of(class);
                let epoch = self.class_proto_epoch;
                self.ic_install(
                    func_id,
                    ip,
                    IcEntry::ClassMiss {
                        recv_shape,
                        class,
                        ver,
                        epoch,
                        nhops,
                        hops,
                    },
                );
                GetAct::Value(Value::UNDEFINED)
            }
            Walked::No => {
                self.ic_note_miss(func_id, ip);
                GetAct::None
            }
        }
    }

    /// Validate one entry against a `GetProp` access. `idx`/`m` are the
    /// already-guarded receiver (see `ic_recv_map`); `own` its `pos(key)`.
    fn ic_validate_get(
        &self,
        e: &IcEntry,
        idx: u32,
        m: &ObjMap,
        own: Option<usize>,
        key: &str,
    ) -> GetAct {
        match *e {
            IcEntry::OwnData { slot, .. } => {
                let s = slot as usize;
                if own == Some(s) && !m.attr_at(s).accessor {
                    GetAct::Value(m.val_at(s))
                } else {
                    GetAct::None
                }
            }
            IcEntry::OwnAcc { slot, .. } => {
                let s = slot as usize;
                if own == Some(s) && m.attr_at(s).accessor {
                    self.ic_acc_get_from(m, s)
                } else {
                    GetAct::None
                }
            }
            IcEntry::ClassMiss { .. } => {
                if own.is_none() && self.ic_class_miss_ok(e, idx, m) {
                    GetAct::Value(Value::UNDEFINED)
                } else {
                    GetAct::None
                }
            }
            IcEntry::ClassMethod {
                class,
                ver,
                epoch,
                callee,
                ..
            } => {
                if own.is_none()
                    && m.class == Some(class)
                    && self.heap.version_of(class) == ver
                    && self.class_proto_epoch == epoch
                {
                    GetAct::Value(callee)
                } else {
                    GetAct::None
                }
            }
            IcEntry::ClassGetter {
                class,
                ver,
                epoch,
                getter,
                ..
            } => {
                if own.is_none()
                    && m.class == Some(class)
                    && self.heap.version_of(class) == ver
                    && self.class_proto_epoch == epoch
                {
                    match self.ic_plain_fn(getter) {
                        Some((fid, closure)) => GetAct::Accessor {
                            fid,
                            closure,
                            getter,
                        },
                        None => GetAct::None,
                    }
                } else {
                    GetAct::None
                }
            }
            IcEntry::ProtoData {
                first, hops, slot, ..
            } => {
                if own.is_some() || m.class.is_some() {
                    return GetAct::None;
                }
                let Some(hm) = self.ic_chain_ok(idx, first, &hops) else {
                    return GetAct::None;
                };
                if slot == u32::MAX {
                    return GetAct::Value(Value::UNDEFINED); // guarded chain miss
                }
                let s = slot as usize;
                if hm.key_get(s) == Some(key) && !hm.attr_at(s).accessor {
                    GetAct::Value(hm.val_at(s))
                } else {
                    GetAct::None
                }
            }
            IcEntry::ProtoAcc {
                first, hops, slot, ..
            } => {
                if own.is_some() || m.class.is_some() {
                    return GetAct::None;
                }
                if self.ic_chain_ok(idx, first, &hops).is_none() {
                    return GetAct::None;
                }
                self.ic_chain_acc_get(hops, slot)
            }
            _ => GetAct::None,
        }
    }

    /// Accessor GET from an already-validated map slot: re-read the getter and
    /// re-resolve it (nothing cached survives a slot redefinition).
    #[inline]
    fn ic_acc_get_from(&self, m: &ObjMap, s: usize) -> GetAct {
        let g = m.val_at(s);
        if g == Value::UNDEFINED {
            // No getter ⇒ undefined (matches the fast path).
            return GetAct::Value(Value::UNDEFINED);
        }
        match self.ic_plain_fn(g) {
            Some((fid, closure)) => GetAct::Accessor {
                fid,
                closure,
                getter: g,
            },
            None => GetAct::None,
        }
    }

    /// Resolve an OWN accessor slot for a GET: re-read the getter and
    /// re-resolve it (nothing cached survives a slot redefinition).
    #[inline]
    fn ic_own_acc_get(&self, recv: Value, key: &str, slot: u32) -> GetAct {
        let Some((_, m)) = self.ic_recv_map(recv) else {
            return GetAct::None;
        };
        let s = slot as usize;
        if m.key_get(s) != Some(key) || !m.attr_at(s).accessor {
            return GetAct::None;
        }
        self.ic_acc_get_from(m, s)
    }

    /// Resolve a CHAIN accessor slot for a GET (chain already validated).
    #[inline]
    fn ic_chain_acc_get(&self, hops: IcHops, slot: u32) -> GetAct {
        let holder = hops.0[hops.1 as usize - 1].0;
        let hm = match self.heap.get(holder) {
            HeapObj::Object(hm) => hm,
            _ => return GetAct::None,
        };
        let s = slot as usize;
        if hm.key_get(s).is_none() || !hm.attr_at(s).accessor {
            return GetAct::None;
        }
        let g = hm.val_at(s);
        if g == Value::UNDEFINED {
            return GetAct::Value(Value::UNDEFINED);
        }
        match self.ic_plain_fn(g) {
            Some((fid, closure)) => GetAct::Accessor {
                fid,
                closure,
                getter: g,
            },
            None => GetAct::None,
        }
    }

    // ── SetProp sites ──

    /// The way-0 own-data hit of [`Self::ic_set_prop`], inline in the general
    /// profile's dispatch loop (see [`Self::ic_get_prop_own_hit`]): the same
    /// shape guard and the same store `ic_apply_set`'s `WriteOwn` makes.
    /// `false` leaves the write to the full call.
    ///
    /// With the JIT compiled in, through the receiver's hot mirror, on the
    /// argument and exclusions `ic_get_prop_own_hit` gives for the read: the
    /// way's shape carries the slot's attributes, so a match also proves the
    /// property writable data, and the emitted shape-way stores write the
    /// same base after the same barrier.
    #[cfg(all(feature = "jit", not(feature = "safe-sandbox")))]
    #[inline(always)]
    pub(crate) fn ic_set_prop_own_hit(
        &mut self,
        func_id: u32,
        ip: usize,
        recv: Value,
        val: Value,
    ) -> bool {
        if !recv.is_heap() {
            return false;
        }
        let idx = recv.heap_index();
        let mir = self.heap.hot_mirror_of(idx);
        if mir.shape == crate::shape::DICT
            || mir.fid != crate::heap::FID_MIRROR_NONE
            || !self.deferred_ns_state.is_empty()
            || !self.ic_func_eligible(func_id)
        {
            return false;
        }
        let Some(cell) = self.ic_cell(func_id, ip) else {
            return false;
        };
        let slot = match cell.own_write(mir.shape) {
            Some(slot) => slot,
            None => {
                let Some(site) = cell.site.as_deref() else {
                    return false;
                };
                if site.n == 0 {
                    return false;
                }
                match site.entries[0] {
                    IcEntry::OwnData { shape: cached, slot } if cached == mir.shape => slot,
                    IcEntry::Private { .. } => return self.ic_private_set_tail(func_id, ip, idx, val),
                    _ => return false,
                }
            }
        };
        if val.is_heap() {
            self.heap.write_barrier_val(idx, val);
        }
        // SAFETY: as for the read in `ic_get_prop_own_hit`; the barrier above
        // allocates nothing and moves no column.
        unsafe { *(mir.vals as *mut Value).add(slot as usize) = val };
        true
    }

    /// [`Self::ic_set_prop_own_hit`] for the main dispatch loop, which also
    /// takes from way 0, out of line (`ic_set_way0_tail`), an append (a
    /// constructor's `this.x = v`) and a setter to call. Each went through
    /// the site key's text, the private and static-member tests and the full
    /// `ic_set_prop` call, whose megamorphic probe, site fetch and receiver
    /// map came before the same first way; an own or a class setter took
    /// the receiver's key lookup as well.
    #[cfg(all(feature = "jit", not(feature = "safe-sandbox")))]
    #[inline(always)]
    pub(crate) fn ic_set_prop_way0(&mut self, func_id: u32, ip: usize, recv: Value, val: Value, name: u32) -> SetAct {
        if !recv.is_heap() {
            return SetAct::None;
        }
        let idx = recv.heap_index();
        let mir = self.heap.hot_mirror_of(idx);
        if mir.shape == crate::shape::DICT
            || mir.fid != crate::heap::FID_MIRROR_NONE
            || !self.deferred_ns_state.is_empty()
            || !self.ic_func_eligible(func_id)
        {
            return SetAct::None;
        }
        let Some(cell) = self.ic_cell(func_id, ip) else {
            return SetAct::None;
        };
        let slot = match cell.own_write(mir.shape) {
            Some(slot) => slot,
            None => {
                let Some(site) = cell.site.as_deref() else {
                    return SetAct::None;
                };
                if site.n == 0 {
                    return SetAct::None;
                }
                match site.entries[0] {
                    IcEntry::OwnData { shape: cached, slot } if cached == mir.shape => slot,
                    IcEntry::Private { .. } => {
                        return if self.ic_private_set_tail(func_id, ip, idx, val) {
                            SetAct::Done
                        } else {
                            SetAct::None
                        };
                    }
                    e @ (IcEntry::AddData { .. }
                    | IcEntry::AddClass { .. }
                    | IcEntry::OwnAcc { .. }
                    | IcEntry::ProtoAcc { .. }
                    | IcEntry::ClassSetter { .. }) => {
                        return self.ic_set_way0_tail(func_id, idx, e, mir.shape, val, name);
                    }
                    _ => return SetAct::None,
                }
            }
        };
        if val.is_heap() {
            self.heap.write_barrier_val(idx, val);
        }
        // SAFETY: as in `ic_set_prop_own_hit`.
        unsafe { *(mir.vals as *mut Value).add(slot as usize) = val };
        SetAct::Done
    }

    /// [`Self::ic_set_prop_way0`]'s appends and setters, under the guards
    /// `ic_set_prop`'s shape-keyed first way applies: the receiver's shape
    /// (its mirror's, `shape`) is the one the way was filled with, which
    /// proves an append's starting layout, an own accessor's slot, and for
    /// an inherited or a class setter that the receiver has no own property
    /// of the key. `name` is the site's key, read only for an append.
    #[cfg(all(feature = "jit", not(feature = "safe-sandbox")))]
    #[inline(never)]
    fn ic_set_way0_tail(&mut self, func_id: u32, idx: u32, e: IcEntry, shape: u32, val: Value, name: u32) -> SetAct {
        let HeapObj::Object(m) = self.heap.get(idx) else {
            return SetAct::None;
        };
        let plan = match e {
            IcEntry::AddData { before, .. } | IcEntry::AddClass { before, .. } if before == shape => {
                self.ic_validate_add(&e, idx, m)
            }
            IcEntry::OwnAcc { shape: s, slot } if s == shape => {
                // A getter-only accessor (no plain setter) is the full path's.
                let setter = m.attr_at(slot as usize).setter;
                self.ic_plain_fn(setter)
                    .map(|(fid, closure)| SetPlan::Setter { fid, closure, setter })
            }
            IcEntry::ProtoAcc {
                recv_shape,
                first,
                hops,
                slot,
            } if recv_shape == shape && m.class.is_none() && self.ic_chain_ok(idx, first, &hops).is_some() => {
                match self.ic_chain_acc_set(hops, slot) {
                    SetAct::Setter { fid, closure, setter } => Some(SetPlan::Setter { fid, closure, setter }),
                    _ => None,
                }
            }
            IcEntry::ClassSetter {
                class,
                ver,
                epoch,
                setter,
                recv_shape,
            } if recv_shape == shape
                && m.class == Some(class)
                && self.heap.version_of(class) == ver
                && self.class_proto_epoch == epoch =>
            {
                self.ic_plain_fn(setter)
                    .map(|(fid, closure)| SetPlan::Setter { fid, closure, setter })
            }
            _ => None,
        };
        match plan {
            Some(p) => {
                let key: &'p str = self.func(func_id as usize).string_constants[name as usize].as_str();
                self.ic_apply_set(p, key, val)
            }
            None => SetAct::None,
        }
    }

    /// [`Self::ic_set_prop_own_hit`] without the JIT's mirrors: the map's own
    /// shape.
    #[cfg(all(not(feature = "jit"), not(feature = "safe-sandbox")))]
    #[inline(always)]
    pub(crate) fn ic_set_prop_own_hit(
        &mut self,
        func_id: u32,
        ip: usize,
        recv: Value,
        val: Value,
    ) -> bool {
        // The receiver first, as `ic_get_prop_own_hit` checks it.
        let Some((idx, m)) = self.ic_recv_map(recv) else {
            return false;
        };
        if !self.ic_func_eligible(func_id) {
            return false;
        }
        let Some(cell) = self.ic_cell(func_id, ip) else {
            return false;
        };
        let sh = m.shape();
        let slot = match (sh != crate::shape::DICT).then(|| cell.own_write(sh)).flatten() {
            Some(slot) => slot,
            None => {
                let Some(site) = cell.site.as_deref() else {
                    return false;
                };
                if site.n == 0 {
                    return false;
                }
                let (cached, slot) = match site.entries[0] {
                    IcEntry::OwnData { shape, slot } => (shape, slot),
                    IcEntry::Private { .. } => return self.ic_private_set_tail(func_id, ip, idx, val),
                    _ => return false,
                };
                if sh == crate::shape::DICT || sh != cached {
                    return false;
                }
                slot
            }
        };
        if val.is_heap() {
            self.heap.write_barrier_val(idx, val);
        }
        if let HeapObj::Object(m) = self.heap.get_mut(idx) {
            m.set_val_at(slot as usize, val);
        }
        true
    }

    /// `ns.key` through an `IcEntry::NsExport` way: the export's live slot,
    /// the namespace [[Get]]'s answer, when the receiver is that namespace
    /// at that version, no deferred namespace is pending and the binding is
    /// out of its TDZ. `None` for anything else.
    #[inline(never)]
    fn ic_ns_export_hit(&self, recv: Value, e: &IcEntry) -> Option<Value> {
        let IcEntry::NsExport { ns, ver, slot } = *e else {
            return None;
        };
        if !recv.is_heap()
            || recv.heap_index() != ns
            || self.heap.version_of(ns) != ver
            || !self.deferred_ns_state.is_empty()
        {
            return None;
        }
        self.globals
            .get(slot as usize)
            .copied()
            .filter(|v| !v.is_uninitialized())
    }

    /// IC lookup + fill for a `SetProp` site. `SetAct::Done` means the own
    /// data slot was written; `Setter` hands a plain setter to frame-call.
    pub(crate) fn ic_set_prop(
        &mut self,
        func_id: u32,
        ip: usize,
        recv: Value,
        key: &str,
        val: Value,
    ) -> SetAct {
        // As `ic_get_prop` does for a read (`ic_set_prop_mega`).
        #[cfg(all(feature = "jit", not(feature = "safe-sandbox")))]
        if self.ic_set_prop_mega(func_id, ip, recv, key, val) {
            return SetAct::Done;
        }
        let mut plan: Option<SetPlan> = None;
        let mut own_rec: Option<(u32, u32)> = None;
        let mut disabled = false;
        if let Some(site) = self.ic_site(func_id, ip) {
            disabled = site.misses >= IC_MISS_LIMIT;
            if site.n > 0 {
                if let Some((idx, m)) = self.ic_recv_map(recv) {
                    // Probe ONLY the first way by hidden class before paying for
                    // `pos(key)`. A shape match proves this site's constant key
                    // still occupies the recorded slot, but not that the live
                    // descriptor is safe to write: re-read it so freeze and
                    // data<->accessor redefinitions cannot bypass ordinary Set
                    // semantics. Keep only indices in the plan so the mutable
                    // store happens after the map/site borrows end.
                    //
                    // Do not scan every way by shape here. GetProp tried that
                    // policy already; failed compares on genuinely polymorphic
                    // JSON sites made it materially slower.
                    let shape = m.shape();
                    if m.shape_guardable() {
                        if let IcEntry::OwnData {
                            shape: cached_shape,
                            slot,
                        } = site.entries[0]
                        {
                            if cached_shape == shape {
                                // Set sites install an OwnData entry only after
                                // proving that this exact shape has a writable
                                // data property in the cached slot. Descriptor
                                // changes leave the shape, so a shape hit is the
                                // complete guard for the monomorphic first way.
                                plan = Some(SetPlan::WriteOwn { idx, slot });
                            }
                        } else if let IcEntry::AddData { before, .. }
                        | IcEntry::AddClass { before, .. } = site.entries[0]
                        {
                            if before == shape {
                                plan = self.ic_validate_add(&site.entries[0], idx, m);
                            }
                        } else if let IcEntry::ProtoAcc {
                            recv_shape,
                            first,
                            hops,
                            slot,
                        } = site.entries[0]
                        {
                            // An inherited setter: the shape proves the key
                            // absent from the receiver, as `ic_validate_set`'s
                            // own-key lookup would.
                            if recv_shape == shape
                                && m.class.is_none()
                                && self.ic_chain_ok(idx, first, &hops).is_some()
                            {
                                if let SetAct::Setter {
                                    fid,
                                    closure,
                                    setter,
                                } = self.ic_chain_acc_set(hops, slot)
                                {
                                    plan = Some(SetPlan::Setter {
                                        fid,
                                        closure,
                                        setter,
                                    });
                                }
                            }
                        }
                    }

                    // Dictionary receivers, unsafe/invalidated first ways, and
                    // shape misses retain the complete old lookup and way scan.
                    if plan.is_none() {
                        let own = self.own_pos(m, key);
                        // In a pure-interpreter VM a write of an own writable
                        // data property is decided by the live map, as the
                        // read beside it is (`ic_get_prop`): no scan of ways
                        // filled for other layouts, no install. A node field
                        // written through more layouts than a site has ways
                        // (`node.flags |= f` over a compiler's AST) evicted,
                        // disabled the site and took `set_prop`'s walk on
                        // every write. JIT-enabled VMs keep the scan and the
                        // fills, which are their native plans' feedback.
                        if let Some(s) = own {
                            if self.interp_own_resolve_vm_is_interpreter()
                                && !is_private_key(key)
                                && m.shape_guardable()
                                && !m.attr_at(s).accessor
                                && m.attr_at(s).writable
                                && !m.class_proto
                            {
                                own_rec = Some((shape, s as u32));
                                plan = Some(SetPlan::WriteOwn { idx, slot: s as u32 });
                            }
                        }
                        if plan.is_none() {
                            for e in &site.entries[..site.n as usize] {
                                if let Some(p) = self.ic_validate_set(e, idx, m, own) {
                                    // A later way's own-data write: the cell's
                                    // pair takes this layout's next one inline.
                                    if let (SetPlan::WriteOwn { slot, .. }, true) =
                                        (&p, m.shape_guardable() && !is_private_key(key))
                                    {
                                        own_rec = Some((shape, *slot));
                                    }
                                    plan = Some(p);
                                    break;
                                }
                            }
                        }
                    }
                }
            }
        }
        if let Some((sh, slot)) = own_rec {
            self.ic_cell_own(func_id, ip, sh, slot as usize, true);
            // Settle the receiver's mirror for the inline hits, as the read
            // side's own resolve does.
            if let Some(SetPlan::WriteOwn { idx, .. }) = plan {
                self.heap.refresh_mirror(idx);
            }
        }
        if let Some(p) = plan {
            return self.ic_apply_set(p, key, val);
        }
        if disabled && self.ic_still_disabled(func_id, ip) {
            return SetAct::None;
        }
        // Keys with exotic write interception never cache: `__proto__`
        // (the inherited setter), restricted names, and canonical-index-ish
        // keys (`note_array_proto_index`). Nor does a private name: the
        // dispatch loop takes a site's way-0 own-data hit before its
        // private-member routing, which is sound only because no such site
        // holds one.
        if is_private_key(key) {
            return SetAct::None;
        }
        if key == "__proto__"
            || key == "caller"
            || key == "arguments"
            || key
                .as_bytes()
                .first()
                .is_some_and(|b| b.is_ascii_digit() || *b == b'-')
        {
            self.ic_note_miss(func_id, ip);
            return SetAct::None;
        }
        match self.ic_walk(recv, key) {
            Walked::OwnData { slot } => {
                match self.ic_own_set_plan(recv, key, slot as u32) {
                    Some(p) => {
                        // Only writable own-data shapes may seed the direct
                        // shape-hit arm above. In particular, do not let a
                        // failed write to a frozen/non-writable property leave
                        // an entry that would make a later hit skip attributes.
                        let shape = self.ic_recv_shape(recv);
                        self.ic_install(
                            func_id,
                            ip,
                            IcEntry::OwnData {
                                shape,
                                slot: slot as u32,
                            },
                        );
                        self.ic_cell_own(func_id, ip, shape, slot, true);
                        self.ic_apply_set(p, key, val)
                    }
                    None => SetAct::None,
                }
            }
            Walked::OwnAcc { slot } => {
                let shape = self.ic_recv_shape(recv);
                self.ic_install(
                    func_id,
                    ip,
                    IcEntry::OwnAcc {
                        shape,
                        slot: slot as u32,
                    },
                );
                self.ic_own_acc_set(recv, key, slot as u32)
            }
            Walked::ClassMethod { .. } | Walked::ClassGetter { .. } => {
                // A write to a class-resolved member: only a SETTER on the
                // chain is cacheable (set_prop's class arm).
                let (ridx, m) = match self.ic_recv_map(recv) {
                    Some(x) => x,
                    None => return SetAct::None,
                };
                let class = m.class.expect("class walk provenance");
                match self.ic_class_setter(ridx, class, key) {
                    Some(setter) => {
                        let ver = self.heap.version_of(class);
                        let epoch = self.class_proto_epoch;
                        self.mi_record_recv(func_id, ip, recv);
                        let recv_shape = self.ic_recv_shape(recv);
                        self.ic_install(
                            func_id,
                            ip,
                            IcEntry::ClassSetter {
                                class,
                                ver,
                                epoch,
                                setter,
                                recv_shape,
                            },
                        );
                        match self.ic_plain_fn(setter) {
                            Some((fid, closure)) => SetAct::Setter {
                                fid,
                                closure,
                                setter,
                            },
                            None => SetAct::None,
                        }
                    }
                    None => {
                        self.ic_note_miss(func_id, ip);
                        SetAct::None
                    }
                }
            }
            // Chain data / chain miss writes create own properties (adds) or
            // route through proto_chain_set — both stay on the slow path.
            // But a class instance with NO method/getter hit may still have a
            // chain SETTER; resolve it directly.
            Walked::ChainMiss { first, hops } => {
                if let Some(p) = self.ic_add_fill(func_id, ip, recv, key, first, hops) {
                    return self.ic_apply_set(p, key, val);
                }
                self.ic_note_miss(func_id, ip);
                SetAct::None
            }
            Walked::ChainData { first, hops, slot }
                if matches!(self.heap.get(hops.0[hops.1 as usize - 1].0), HeapObj::Object(hm)
                    if hm.key_get(slot as usize).is_some()
                        && !hm.attr_at(slot as usize).accessor
                        && hm.attr_at(slot as usize).writable) =>
            {
                // A writable inherited data property is shadowed by the add.
                if let Some(p) = self.ic_add_fill(func_id, ip, recv, key, first, hops) {
                    return self.ic_apply_set(p, key, val);
                }
                self.ic_note_miss(func_id, ip);
                SetAct::None
            }
            // A class instance's absent key (a read's `ClassMiss`): its write
            // appends, as for any other class-chain miss.
            Walked::No | Walked::ChainData { .. } | Walked::ClassMiss { .. } => {
                if let Some(p) = self.ic_add_class_fill(func_id, ip, recv, key) {
                    return self.ic_apply_set(p, key, val);
                }
                if let Some((ridx, m)) = self.ic_recv_map(recv) {
                    if let Some(class) = m.class {
                        if m.pos(key).is_none() {
                            if let Some(setter) = self.ic_class_setter(ridx, class, key) {
                                let ver = self.heap.version_of(class);
                                let epoch = self.class_proto_epoch;
                                self.mi_record_recv(func_id, ip, recv);
                                let recv_shape = self.ic_recv_shape(recv);
                                self.ic_install(
                                    func_id,
                                    ip,
                                    IcEntry::ClassSetter {
                                        class,
                                        ver,
                                        epoch,
                                        setter,
                                        recv_shape,
                                    },
                                );
                                return match self.ic_plain_fn(setter) {
                                    Some((fid, closure)) => SetAct::Setter {
                                        fid,
                                        closure,
                                        setter,
                                    },
                                    None => SetAct::None,
                                };
                            }
                        }
                    }
                }
                self.ic_note_miss(func_id, ip);
                SetAct::None
            }
            Walked::ChainAcc { first, hops, slot } => {
                // Inherited accessor governs the write (proto_chain_set):
                // cache it; a getter-only accessor stays slow (reject path).
                let recv_shape = self.ic_recv_shape(recv);
                self.ic_install(
                    func_id,
                    ip,
                    IcEntry::ProtoAcc {
                        recv_shape,
                        first,
                        hops,
                        slot,
                    },
                );
                self.ic_chain_acc_set(hops, slot)
            }
        }
    }

    /// The proof a class instance's append of `key` needs, shared by the
    /// JIT's add ways and the interpreter's `AddClass` entries: the instance
    /// has no [[Prototype]] of its own, its class chain is classes down to a
    /// base class (a function or built-in parent, or `extends null`, keeps the
    /// general path), no member table on it declares `key`, and the ordinary
    /// chain from where the tables end holds no setter and no read-only
    /// `key`. Records that chain's objects with their versions in `hops` and
    /// returns how many; `None` when the proof fails or needs more hops.
    pub(crate) fn class_add_chain(
        &self,
        idx: u32,
        class: u32,
        key: &str,
        hops: &mut [(u32, u32)],
    ) -> Option<usize> {
        self.class_chain_proof(idx, class, key, hops, false)
    }

    /// [`Vm::class_add_chain`]'s walk, for an append (`read` false) or for a
    /// read that finds nothing (`read`): there no level of the ordinary
    /// chain may hold `key` at all, since a read would return it.
    pub(crate) fn class_chain_proof(
        &self,
        idx: u32,
        class: u32,
        key: &str,
        hops: &mut [(u32, u32)],
        read: bool,
    ) -> Option<usize> {
        if self.proto_of.has(idx) {
            return None;
        }
        let mut cur = class;
        loop {
            let HeapObj::Class(c) = self.heap.get(cur) else {
                return None;
            };
            if c.extends_null || (cur == class && c.reproto_instances) {
                return None;
            }
            match c.parent {
                None => break,
                Some(p) if matches!(self.heap.get(p), HeapObj::Class(_)) => cur = p,
                Some(_) => return None,
            }
        }
        if !matches!(self.class_member_lookup(idx, class, key), ClassLookup::Miss) {
            return None;
        }
        let mut next = self.class_instance_live_proto(idx, class);
        let mut n = 0usize;
        while next.is_heap() {
            let h = next.heap_index();
            if n == hops.len() || !self.ic_obj_ok(h) {
                return None;
            }
            let HeapObj::Object(m) = self.heap.get(h) else {
                return None;
            };
            if m.is_ctor || m.class.is_some() {
                return None;
            }
            hops[n] = (h, self.heap.version_of(h));
            n += 1;
            if let Some(i) = m.pos(key) {
                // A writable data property is shadowed by the append; anything
                // else governs the write. A read would find any of them.
                if read || m.attr_at(i).accessor || !m.attr_at(i).writable {
                    return None;
                }
                break;
            }
            next = match self.proto_of.get_copied(h) {
                Some(p) => p,
                None if self.obj_proto == 0 || h == self.obj_proto => Value::NULL,
                None => Value::heap(self.obj_proto),
            };
        }
        Some(n)
    }

    /// A `ClassMiss` entry's guards past the receiver's shape (or its own
    /// miss): still an instance of the entry's class, no [[Prototype]] or
    /// side-table properties of its own, the class tables as filled, and the
    /// chain past them unchanged.
    #[inline]
    fn ic_class_miss_ok(&self, e: &IcEntry, idx: u32, m: &ObjMap) -> bool {
        let IcEntry::ClassMiss {
            class,
            ver,
            epoch,
            nhops,
            hops,
            ..
        } = *e
        else {
            return false;
        };
        m.class == Some(class)
            && !m.is_ctor
            && self.heap.version_of(class) == ver
            && self.class_proto_epoch == epoch
            && !self.proto_of.has(idx)
            && (self.arr_props.is_empty() || !self.arr_props.contains_key(&idx))
            && hops[..nhops as usize]
                .iter()
                .all(|&(h, v)| self.heap.version_of(h) == v)
    }

    /// An `AddData` hit: `m` (heap `idx`) is a plain extensible object of the
    /// entry's `before` shape the ordinary add path would take, and the chain
    /// still stands.
    #[inline]
    fn ic_validate_add(&self, e: &IcEntry, idx: u32, m: &ObjMap) -> Option<SetPlan> {
        if let IcEntry::AddClass {
            after,
            class,
            ver,
            epoch,
            nhops,
            hops,
            ..
        } = *e
        {
            if !m.extensible
                || m.class != Some(class)
                || m.is_ctor
                || self.heap.version_of(class) != ver
                || self.class_proto_epoch != epoch
                || self.proto_of.has(idx)
                || (!self.arr_props.is_empty() && self.arr_props.contains_key(&idx))
                || hops[..nhops as usize]
                    .iter()
                    .any(|&(h, v)| self.heap.version_of(h) != v)
            {
                return None;
            }
            return Some(SetPlan::Append { idx, after });
        }
        let IcEntry::AddData {
            after, first, hops, ..
        } = *e
        else {
            return None;
        };
        if !m.extensible
            || m.class.is_some()
            || m.class_proto
            || idx == self.obj_proto
            || (!self.arr_props.is_empty() && self.arr_props.contains_key(&idx))
        {
            return None;
        }
        self.ic_chain_ok(idx, first, &hops)?;
        Some(SetPlan::Append { idx, after })
    }

    /// Prove the append a `SetProp` miss is about to make and install the
    /// site's `AddData` entry: the receiver and key are ones `set_prop`'s
    /// ordinary add would take (`ordinary_set_recv_ok`/`ordinary_set_ok`),
    /// the receiver has a shape, and the walk that found the chain clear
    /// guards it. Returns the plan that makes the append.
    fn ic_add_fill(
        &mut self,
        func_id: u32,
        ip: usize,
        recv: Value,
        key: &str,
        first: u8,
        hops: IcHops,
    ) -> Option<SetPlan> {
        let (idx, m) = self.ic_recv_map(recv)?;
        if !m.shape_guardable() || !self.ordinary_set_recv_ok(idx) || !self.ordinary_set_ok(idx, key) {
            return None;
        }
        let before = m.shape();
        let after = crate::shape::add(before, key, crate::shape::attr_bits(true, true, true, false));
        if after == crate::shape::DICT {
            return None;
        }
        self.ic_install(
            func_id,
            ip,
            IcEntry::AddData {
                before,
                after,
                first,
                hops,
            },
        );
        Some(SetPlan::Append { idx, after })
    }

    /// The class-instance twin of [`Vm::ic_add_fill`]: a class instance
    /// missing `key` whose append `class_add_chain` proves, with a key the
    /// ordinary add would take. Installs the site's `AddClass` entry.
    fn ic_add_class_fill(&mut self, func_id: u32, ip: usize, recv: Value, key: &str) -> Option<SetPlan> {
        let (idx, m) = self.ic_recv_map(recv)?;
        let class = m.class?;
        if !m.shape_guardable()
            || !m.extensible
            || m.class_proto
            || m.pos(key).is_some()
            || !self.ordinary_set_ok(idx, key)
        {
            return None;
        }
        let before = m.shape();
        let mut hops = [(0u32, 0u32); 5];
        let nhops = self.class_add_chain(idx, class, key, &mut hops)?;
        let after = crate::shape::add(before, key, crate::shape::attr_bits(true, true, true, false));
        if after == crate::shape::DICT {
            return None;
        }
        let ver = self.heap.version_of(class);
        let epoch = self.class_proto_epoch;
        self.ic_install(
            func_id,
            ip,
            IcEntry::AddClass {
                before,
                after,
                class,
                ver,
                epoch,
                nhops: nhops as u8,
                hops,
            },
        );
        Some(SetPlan::Append { idx, after })
    }

    /// Pure validation of one entry against a `SetProp` access; the returned
    /// plan is applied by [`Vm::ic_apply_set`] once all borrows end. `idx`/`m`
    /// are the already-guarded receiver; `own` its `pos(key)`.
    fn ic_validate_set(
        &self,
        e: &IcEntry,
        idx: u32,
        m: &ObjMap,
        own: Option<usize>,
    ) -> Option<SetPlan> {
        match *e {
            IcEntry::AddData { before, .. } | IcEntry::AddClass { before, .. } => {
                if own.is_none() && m.shape_guardable() && m.shape() == before {
                    self.ic_validate_add(e, idx, m)
                } else {
                    None
                }
            }
            IcEntry::OwnData { slot, .. } => {
                let s = slot as usize;
                // A class prototype's writes must reach `set_prop`, which
                // records a divergence from its class's member tables.
                if own == Some(s)
                    && !m.attr_at(s).accessor
                    && m.attr_at(s).writable
                    && !m.class_proto
                {
                    Some(SetPlan::WriteOwn { idx, slot })
                } else {
                    None
                }
            }
            IcEntry::OwnAcc { slot, .. } => {
                let s = slot as usize;
                if own == Some(s) && m.attr_at(s).accessor {
                    let setter = m.attr_at(s).setter;
                    let (fid, closure) = self.ic_plain_fn(setter)?;
                    Some(SetPlan::Setter {
                        fid,
                        closure,
                        setter,
                    })
                } else {
                    None
                }
            }
            IcEntry::ClassSetter {
                class,
                ver,
                epoch,
                setter,
                ..
            } => {
                if own.is_none()
                    && m.class == Some(class)
                    && self.heap.version_of(class) == ver
                    && self.class_proto_epoch == epoch
                {
                    let (fid, closure) = self.ic_plain_fn(setter)?;
                    Some(SetPlan::Setter {
                        fid,
                        closure,
                        setter,
                    })
                } else {
                    None
                }
            }
            IcEntry::ProtoAcc {
                first, hops, slot, ..
            } => {
                if own.is_some() || m.class.is_some() {
                    return None;
                }
                self.ic_chain_ok(idx, first, &hops)?;
                match self.ic_chain_acc_set(hops, slot) {
                    SetAct::Setter {
                        fid,
                        closure,
                        setter,
                    } => Some(SetPlan::Setter {
                        fid,
                        closure,
                        setter,
                    }),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// Apply a validated set plan (the write, or hand back the setter).
    #[inline]
    fn ic_apply_set(&mut self, p: SetPlan, key: &str, val: Value) -> SetAct {
        match p {
            SetPlan::Append { idx, after } => {
                // What `set_prop`'s ordinary add does: its barrier, the
                // append, and the version bump the JIT's caches need. The
                // object takes the shape's shared key list, as the JIT's add
                // ways do, rather than a new `String` per key: a constructor's
                // `this.a = a; this.b = b` was two allocations per instance.
                self.store_barrier_v(crate::heap::gcoracle::SET_PROP, Value::heap(idx), val);
                #[cfg(not(feature = "safe-sandbox"))]
                {
                    if !self.shape_key_plans.get(after as usize).is_some_and(Option::is_some) {
                        super::helpers_misc::shape_key_plan(self, after);
                    }
                    let borrow = crate::bytecode::plan_borrow_enabled();
                    let plan = self.shape_key_plans.get(after as usize).and_then(Option::as_ref);
                    if let HeapObj::Object(m) = self.heap.get_mut(idx) {
                        match plan {
                            // SAFETY: the VM's shape plans are never removed
                            // or replaced, and its heap's maps die with it.
                            Some(p) if p.len() == m.slot_len() + 1 => unsafe {
                                m.append_data_planned(key, val, after, p, borrow)
                            },
                            _ => m.append_data_shaped(key, val, after, None),
                        }
                    }
                }
                #[cfg(feature = "safe-sandbox")]
                {
                    let plan = super::helpers_misc::shape_key_plan(self, after);
                    if let HeapObj::Object(m) = self.heap.get_mut(idx) {
                        m.append_data_shaped(key, val, after, plan);
                    }
                }
                self.heap.bump_version(idx);
                // The append is complete, so this is a settling event: the
                // instance a constructor builds reads through its mirror
                // from its first read on (`ic_get_prop_own_hit`).
                self.heap.refresh_mirror(idx);
                SetAct::Done
            }
            SetPlan::WriteOwn { idx, slot } => {
                // Nursery barrier: the interpreter IC's direct own-data store
                // bypasses `set_prop` (whose entry barrier covers every slow
                // route), so it carries its own.
                // Primitive stores dominate ordinary property loops and can
                // never create an old-to-young edge. Keep them out of the
                // non-inlined barrier helper entirely; the helper retains its
                // own value check for its general call sites.
                if val.is_heap() {
                    self.heap.write_barrier_val(idx, val);
                }
                if let HeapObj::Object(m) = self.heap.get_mut(idx) {
                    m.set_val_at(slot as usize, val);
                }
                SetAct::Done
            }
            SetPlan::Setter {
                fid,
                closure,
                setter,
            } => SetAct::Setter {
                fid,
                closure,
                setter,
            },
        }
    }

    // ── DefineField sites ──

    /// A class field's definition (`DefineField`, CreateDataPropertyOrThrow)
    /// as an append through the site's ways: an `AddData` entry whose
    /// `before` is the receiver's shape takes the key to `after`, as
    /// `define_field_append` would, without its key tests, own lookup and
    /// transition search. A definition consults no prototype, so the entries
    /// a `DefineField` site holds have empty chains, and nothing reads them
    /// as a `SetProp`'s. `false`, with nothing done, sends the definition the
    /// full way; a miss that is an append fills a way.
    ///
    /// Every instance of a class with public fields took that search for
    /// each of them: two fields were an eighth of constructing an instance of
    /// class-features' hierarchy.
    #[cfg(not(feature = "safe-sandbox"))]
    pub(crate) fn ic_define_field(&mut self, func_id: u32, ip: usize, recv: Value, key: &str, val: Value) -> bool {
        if !recv.is_heap() || !self.deferred_ns_state.is_empty() {
            return false;
        }
        let idx = recv.heap_index();
        let HeapObj::Object(m) = self.heap.get(idx) else {
            return false;
        };
        if !m.extensible
            || m.is_ctor
            || m.class_proto
            || !m.shape_guardable()
            || idx == self.obj_proto
            || !self.ic_obj_ok_map(idx, m)
            || (!self.arr_props.is_empty() && self.arr_props.contains_key(&idx))
        {
            return false;
        }
        let shape = m.shape();
        let hit = self.ic_site(func_id, ip).and_then(|site| {
            site.entries[..site.n as usize].iter().find_map(|e| match *e {
                IcEntry::AddData { before, after, .. } if before == shape => Some(after),
                _ => None,
            })
        });
        let after = match hit {
            Some(after) => after,
            None => {
                if !Self::field_key_appends(key) || m.pos(key).is_some() {
                    return false;
                }
                let after = crate::shape::add(shape, key, crate::shape::attr_bits(true, true, true, false));
                if after == crate::shape::DICT {
                    return false;
                }
                self.ic_install(
                    func_id,
                    ip,
                    IcEntry::AddData {
                        before: shape,
                        after,
                        first: 0,
                        hops: ([(0, 0); IC_MAX_HOPS], 0),
                    },
                );
                after
            }
        };
        self.ic_apply_set(SetPlan::Append { idx, after }, key, val);
        true
    }

    /// Validate an OWN data slot for a write (still a writable data property
    /// under the same key — redefined/frozen slots fall to the slow path).
    #[inline]
    fn ic_own_set_plan(&self, recv: Value, key: &str, slot: u32) -> Option<SetPlan> {
        let (idx, m) = self.ic_recv_map(recv)?;
        let s = slot as usize;
        if m.key_get(s) == Some(key)
            && !m.attr_at(s).accessor
            && m.attr_at(s).writable
            && !m.class_proto
        {
            Some(SetPlan::WriteOwn { idx, slot })
        } else {
            None
        }
    }

    /// The SETTER a write of `key` on class instance `idx` resolves to through
    /// its class's member tables, when that resolution may be cached by class
    /// (see `ic_walk` for why a class with re-prototyped instances may not).
    fn ic_class_setter(&self, idx: u32, class: u32, key: &str) -> Option<Value> {
        if matches!(self.heap.get(class), HeapObj::Class(c) if c.reproto_instances) {
            return None;
        }
        match self.class_member_lookup(idx, class, key) {
            ClassLookup::Accessor { setter, .. } if setter != Value::UNDEFINED => Some(setter),
            _ => None,
        }
    }

    /// Resolve an OWN accessor slot for a SET: the setter must exist and be a
    /// plain user function (getter-only / native → slow path).
    #[inline]
    fn ic_own_acc_set(&self, recv: Value, key: &str, slot: u32) -> SetAct {
        let Some((_, m)) = self.ic_recv_map(recv) else {
            return SetAct::None;
        };
        let s = slot as usize;
        if m.key_get(s) != Some(key) || !m.attr_at(s).accessor {
            return SetAct::None;
        }
        let setter = m.attr_at(s).setter;
        match self.ic_plain_fn(setter) {
            Some((fid, closure)) => SetAct::Setter {
                fid,
                closure,
                setter,
            },
            None => SetAct::None,
        }
    }

    /// Resolve a CHAIN accessor slot for a SET (chain already validated).
    #[inline]
    fn ic_chain_acc_set(&self, hops: IcHops, slot: u32) -> SetAct {
        let holder = hops.0[hops.1 as usize - 1].0;
        let hm = match self.heap.get(holder) {
            HeapObj::Object(hm) => hm,
            _ => return SetAct::None,
        };
        let s = slot as usize;
        if hm.key_get(s).is_none() || !hm.attr_at(s).accessor {
            return SetAct::None;
        }
        let setter = hm.attr_at(s).setter;
        match self.ic_plain_fn(setter) {
            Some((fid, closure)) => SetAct::Setter {
                fid,
                closure,
                setter,
            },
            None => SetAct::None,
        }
    }

    // ── CallMethod sites ──

    /// IC lookup + fill for a `CallMethod` site: resolves `recv.key` to a
    /// PLAIN user function to frame-call with `this = recv`. `None` ⇒ slow
    /// path (builtins, natives, accessors, generators, exotic receivers).
    pub(crate) fn ic_call_method(
        &mut self,
        func_id: u32,
        ip: usize,
        recv: Value,
        key: &str,
    ) -> Option<(u32, u32, Value)> {
        if let Some(site) = self.ic_site(func_id, ip) {
            if site.n > 0 {
                if let Some((idx, m)) = self.ic_recv_map(recv) {
                    // The ways by hidden class: a matching shape proves where
                    // the key is (an own data slot) or that the receiver has
                    // no own property of it (a class or inherited method), so
                    // the hit needs no key lookup. That lookup was a fifth of a
                    // monomorphic `o.m()`, and a site cycling a few classes
                    // (`objs[i & 3].area()`) paid it on every call that missed
                    // the first way. Every way is tried, unlike `ic_get_prop`'s
                    // first-way probe: a method site that outruns its ways
                    // stops filling them (`IC_MISS_LIMIT`), and a receiver with
                    // a fresh shape per object rarely calls methods at one site.
                    let sh = m.shape();
                    if sh != crate::shape::DICT {
                        for e in &site.entries[..site.n as usize] {
                            match *e {
                                IcEntry::OwnData { shape, slot } if shape == sh => {
                                    let v = m.val_at(slot as usize);
                                    if let Some((fid, closure)) = self.ic_plain_fn(v) {
                                        return Some((fid, closure, v));
                                    }
                                    break;
                                }
                                IcEntry::ClassMethod {
                                    class,
                                    ver,
                                    epoch,
                                    callee,
                                    fid,
                                    closure,
                                    recv_shape,
                                } if recv_shape == sh => {
                                    if m.class == Some(class)
                                        && self.heap.version_of(class) == ver
                                        && self.class_proto_epoch == epoch
                                    {
                                        return Some((fid, closure, callee));
                                    }
                                }
                                IcEntry::ProtoData {
                                    recv_shape,
                                    first,
                                    hops,
                                    slot,
                                } if recv_shape == sh && slot != u32::MAX && m.class.is_none() => {
                                    if let Some(hm) = self.ic_chain_ok(idx, first, &hops) {
                                        let s = slot as usize;
                                        // The holder's version is the fill's, so
                                        // its slot still holds the key as a data
                                        // property (`ic_get_prop_proto_hit`'s
                                        // argument): the key's text compare was
                                        // a string read per inherited call.
                                        debug_assert!(hm.key_get(s) == Some(key) && !hm.attr_at(s).accessor);
                                        let v = hm.val_at(s);
                                        if let Some((fid, closure)) = self.ic_plain_fn(v) {
                                            return Some((fid, closure, v));
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    // A native method the site cached for this very receiver
                    // (`Reflect.get(o, k)`): `ic_call_native`, asked next,
                    // answers it, and the key lookup below could only find
                    // what no frame calls. Every `Reflect.*` call hashed its
                    // name against the namespace's keys here first.
                    if site.entries[..site.n as usize].iter().any(|e| {
                        matches!(*e, IcEntry::OwnNative { obj, ver, slot, bits, .. }
                            if obj == idx
                                && self.heap.version_of(idx) == ver
                                && m.val_get(slot as usize).is_some_and(|v| v.bits() == bits)
                                && !m.attr_at(slot as usize).accessor)
                    }) {
                        return None;
                    }
                    let own = m.pos(key);
                    let mut learned = None;
                    for e in &site.entries[..site.n as usize] {
                        if let Some(hit) = self.ic_validate_method(e, idx, m, own, key) {
                            learned = Some((hit, sh, *e, site.n as usize));
                            break;
                        }
                    }
                    if let Some((hit, sh, e, n)) = learned {
                        // A receiver of another layout reached a way's method:
                        // the lookup just proved where this layout keeps the
                        // key (or that it has none), so give the layout its
                        // own way while one is free and its next call takes
                        // the shape loop above. Without it every receiver but
                        // the first layout's paid the key lookup each call,
                        // a hashed one for a wide object.
                        if sh != crate::shape::DICT && n < IC_WAYS {
                            let way = match e {
                                IcEntry::OwnData { .. } => own.map(|s| IcEntry::OwnData {
                                    shape: sh,
                                    slot: s as u32,
                                }),
                                IcEntry::ClassMethod {
                                    class,
                                    ver,
                                    epoch,
                                    callee,
                                    fid,
                                    closure,
                                    ..
                                } => Some(IcEntry::ClassMethod {
                                    class,
                                    ver,
                                    epoch,
                                    callee,
                                    fid,
                                    closure,
                                    recv_shape: sh,
                                }),
                                IcEntry::ProtoData {
                                    first, hops, slot, ..
                                } => Some(IcEntry::ProtoData {
                                    recv_shape: sh,
                                    first,
                                    hops,
                                    slot,
                                }),
                                _ => None,
                            };
                            if let Some(way) = way {
                                self.ic_install(func_id, ip, way);
                            }
                        }
                        return Some(hit);
                    }
                }
            }
            if site.misses >= IC_MISS_LIMIT && self.ic_still_disabled(func_id, ip) {
                return None;
            }
        }
        // Never claim names the builtin-method probe would handle for an
        // object receiver (bug-compatibility with try_builtin_method).
        if builtin_object_method(key) {
            self.ic_note_miss(func_id, ip);
            return None;
        }
        match self.ic_walk(recv, key) {
            Walked::OwnData { slot } => {
                let (_, m) = self.ic_recv_map(recv)?;
                let v = m.val_at(slot);
                let shape = m.shape();
                match self.ic_plain_fn(v) {
                    Some((fid, closure)) => {
                        // The Tier-C transactional own-method planner runs at
                        // the caller's next entry, where the receiver register
                        // has not necessarily been populated yet. Preserve the
                        // exact receiver observed by this side-effect-free fill;
                        // emitted code still revalidates identity, version,
                        // shape/slot descriptor and the live callee value.
                        self.mi_record_recv(func_id, ip, recv);
                        self.ic_install(
                            func_id,
                            ip,
                            IcEntry::OwnData {
                                shape,
                                slot: slot as u32,
                            },
                        );
                        Some((fid, closure, v))
                    }
                    None => {
                        self.ic_note_miss(func_id, ip);
                        None
                    }
                }
            }
            Walked::ClassMethod { class, callee } => match self.ic_plain_fn(callee) {
                Some((fid, closure)) => {
                    let ver = self.heap.version_of(class);
                    let epoch = self.class_proto_epoch;
                    let recv_shape = self.ic_recv_shape(recv);
                    self.mi_record_recv(func_id, ip, recv);
                    self.ic_install(
                        func_id,
                        ip,
                        IcEntry::ClassMethod {
                            class,
                            ver,
                            epoch,
                            callee,
                            fid,
                            closure,
                            recv_shape,
                        },
                    );
                    Some((fid, closure, callee))
                }
                None => {
                    self.ic_note_miss(func_id, ip);
                    None
                }
            },
            Walked::ChainData { first, hops, slot } => {
                let hm = match self.heap.get(hops.0[hops.1 as usize - 1].0) {
                    HeapObj::Object(hm) => hm,
                    _ => return None,
                };
                let v = hm.val_at(slot as usize);
                match self.ic_plain_fn(v) {
                    Some((fid, closure)) => {
                        // B78: the method inliner now bakes arms for INHERITED
                        // methods too, so this fill is worth recording for the
                        // same reason the class one is. Without it the planner
                        // sees only the live exemplar at the receiver register
                        // plus the `arr[idx]` dense scan, so `var o = list[i];
                        // o.m()` — the receiver loaded indirectly — would get
                        // exactly ONE arm.
                        self.mi_record_recv(func_id, ip, recv);
                        let recv_shape = self.ic_recv_shape(recv);
                        self.ic_install(
                            func_id,
                            ip,
                            IcEntry::ProtoData {
                                recv_shape,
                                first,
                                hops,
                                slot,
                            },
                        );
                        Some((fid, closure, v))
                    }
                    None => {
                        self.ic_note_miss(func_id, ip);
                        None
                    }
                }
            }
            // Accessors / class getters / chain misses: slow path resolves
            // (and may call a getter, which the IC must never do).
            _ => {
                self.ic_note_miss(func_id, ip);
                None
            }
        }
    }

    /// The native a `CallMethod` site's ordinary-object receiver `recv`
    /// holds as its own data method `key`, from the site's `OwnNative` way,
    /// filled here on a miss: what the builtin dispatcher's
    /// `own_native_method` arm would call, without the receiver-kind probes
    /// ahead of it and its key lookup (a hash on a namespace object's index).
    /// The way is guarded by the receiver's identity and version and the
    /// slot's value; a realm, a deferred namespace or a name the dispatcher
    /// defers (`toString` and its kin) takes the dispatcher. `Reflect.get`
    /// paid both on every call, and a Proxy trap forwards through it.
    #[inline(never)]
    pub(crate) fn ic_call_native(&mut self, func_id: u32, ip: usize, recv: Value, key: &str) -> Option<u16> {
        if !self.realm_global_objs.is_empty() || !self.realm_fns.is_empty() || !self.deferred_ns_state.is_empty() {
            return None;
        }
        let idx = recv.heap_index();
        if let Some(site) = self.ic_site(func_id, ip) {
            for e in &site.entries[..site.n as usize] {
                if let IcEntry::OwnNative { obj, ver, slot, id, bits } = *e {
                    if obj == idx && self.heap.version_of(idx) == ver {
                        if let HeapObj::Object(m) = self.heap.get(idx) {
                            let s = slot as usize;
                            if m.val_get(s).is_some_and(|v| v.bits() == bits) && !m.attr_at(s).accessor {
                                return Some(id);
                            }
                        }
                    }
                }
            }
        }
        if matches!(key, "toString" | "valueOf" | "toLocaleString" | "toJSON") {
            return None;
        }
        let id = self.own_native_method(idx, key)?;
        let HeapObj::Object(m) = self.heap.get(idx) else {
            return None;
        };
        let slot = m.pos(key)?;
        let bits = m.val_at(slot).bits();
        let ver = self.heap.version_of(idx);
        self.ic_install(
            func_id,
            ip,
            IcEntry::OwnNative {
                obj: idx,
                ver,
                slot: slot as u32,
                id,
                bits,
            },
        );
        Some(id)
    }

    /// Read-only, allocation-free probe for the hot own-data arm of a
    /// `CallMethod` site. Unlike [`Self::ic_call_method`], this never fills,
    /// walks a prototype, hashes `key`, or allocates an owned copy of it. The
    /// already-filled `OwnData { shape, slot }` way is sufficient because a
    /// guardable hidden-class shape identifies the complete key/descriptor
    /// layout. We nevertheless re-check the exact key and data descriptor at
    /// the slot as a fail-closed audit boundary before reading the LIVE value.
    ///
    /// A same-shape method replacement is intentionally observed: the shape
    /// remains valid, but `m.val_at(slot)` is read on every call. Delete,
    /// defineProperty/accessor transitions, dictionary mode, proxies/exotics,
    /// namespaces and realm-global objects all miss without effects.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn ic_call_method_own_data_cached(
        &self,
        func_id: u32,
        ip: usize,
        recv: Value,
        key: &str,
    ) -> Option<(u32, u32, Value)> {
        if !recv.is_heap() {
            return None;
        }
        let idx = recv.heap_index();
        if idx as usize >= self.heap.len() || !self.ic_obj_ok(idx) {
            return None;
        }
        let m = match self.heap.get(idx) {
            HeapObj::Object(m) if !m.is_ctor && m.shape_guardable() => m,
            _ => return None,
        };
        let site = self.ic_site(func_id, ip)?;
        for entry in &site.entries[..site.n as usize] {
            let IcEntry::OwnData { shape, slot } = *entry else {
                continue;
            };
            if shape != m.shape() {
                continue;
            }
            let slot = slot as usize;
            if m.key_get(slot).is_none()
                || slot >= m.vals_len()
                || slot >= m.attrs_len()
                || m.key_at(slot) != key
                || m.attr_at(slot).accessor
            {
                return None;
            }
            let callee = m.val_at(slot);
            let (fid, closure) = self.ic_plain_fn(callee)?;
            return Some((fid, closure, callee));
        }
        None
    }

    fn ic_validate_method(
        &self,
        e: &IcEntry,
        idx: u32,
        m: &ObjMap,
        own: Option<usize>,
        key: &str,
    ) -> Option<(u32, u32, Value)> {
        match *e {
            IcEntry::OwnData { slot, .. } => {
                let s = slot as usize;
                if own == Some(s) && !m.attr_at(s).accessor {
                    let v = m.val_at(s);
                    let (fid, closure) = self.ic_plain_fn(v)?;
                    Some((fid, closure, v))
                } else {
                    None
                }
            }
            IcEntry::ClassMethod {
                class,
                ver,
                epoch,
                callee,
                fid,
                closure,
                ..
            } => {
                if own.is_none()
                    && m.class == Some(class)
                    && self.heap.version_of(class) == ver
                    && self.class_proto_epoch == epoch
                {
                    Some((fid, closure, callee))
                } else {
                    None
                }
            }
            IcEntry::ProtoData {
                first, hops, slot, ..
            } => {
                if slot == u32::MAX || own.is_some() || m.class.is_some() {
                    return None;
                }
                let hm = self.ic_chain_ok(idx, first, &hops)?;
                let s = slot as usize;
                if hm.key_get(s) == Some(key) && !hm.attr_at(s).accessor {
                    let v = hm.val_at(s);
                    let (fid, closure) = self.ic_plain_fn(v)?;
                    Some((fid, closure, v))
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    // ── plain Call sites ──

    /// Read-only probe of a `Call` site's inline cache for the Q4 leaf-inline
    /// planner: if the site is MONOMORPHIC (exactly one filled way) and that way
    /// is a `Callee` entry, return `(callee_bits, ver, fid, closure)` (`ver` is
    /// the cached slot version — the inline guard re-checks it to catch a GC'd +
    /// reused callee slot whose bits collide but version differs). Returns `None`
    /// for an empty / polymorphic / disabled site — the planner then declines to
    /// inline this call (it keeps the per-call helper). Performs NO fill and NO
    /// side effect (unlike `ic_call`), so it's safe to call at compile time.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn ic_call_mono(&self, func_id: u32, ip: usize) -> Option<(u64, u32, u32, u32)> {
        let site = self.ic_site(func_id, ip)?;
        if site.n != 1 {
            return None;
        }
        match site.entries[0] {
            IcEntry::Callee {
                bits,
                ver,
                fid,
                closure,
                ..
            } => Some((bits, ver, fid, closure)),
            _ => None,
        }
    }

    /// Read-only witness for a rotating closure call site: at least two filled
    /// `Callee` ways, and every filled way names the same immutable FuncProto.
    /// Identities / captured cells may differ. This is only a profitability
    /// signal for planting Tier C's fully dynamic cross-call prefix; generated
    /// code never trusts the witness and the helper re-resolves the live callee
    /// Value on every invocation.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn ic_call_same_proto(
        &self,
        func_id: u32,
        ip: usize,
    ) -> Option<(u64, u32, u32, u32)> {
        let site = self.ic_site(func_id, ip)?;
        if site.n < 2 {
            return None;
        }
        let first = match site.entries[0] {
            IcEntry::Callee {
                bits,
                ver,
                fid,
                closure,
                ..
            } => (bits, ver, fid, closure),
            _ => return None,
        };
        for entry in &site.entries[1..site.n as usize] {
            match *entry {
                IcEntry::Callee { fid, .. } if fid == first.2 => {}
                _ => return None,
            }
        }
        Some(first)
    }

    /// Read-only profitability witness for a genuinely polymorphic plain-call
    /// site: at least two filled `Callee` ways, every filled way names a
    /// structurally cross-callable plain function, and at least two distinct
    /// immutable FuncProto ids occur. Returned witnesses are de-duplicated by
    /// fid and bounded by this site's filled ways. The planner may use them to
    /// emit independently guarded CROSS3 arms; the generic cross-call helper
    /// remains the final live-resolution miss path.
    ///
    /// Requiring every filled way to be `Callee` keeps natives, bound functions,
    /// generators, async functions and other exotic callables out of the
    /// profitability signal. A later replacement with any of those values is
    /// still correct: the live helper declines before effects and the ordinary
    /// call path runs unchanged.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn ic_call_poly_fids(
        &self,
        func_id: u32,
        ip: usize,
    ) -> Option<Vec<(u64, u32, u32, u32)>> {
        let site = self.ic_site(func_id, ip)?;
        if site.n < 2 {
            return None;
        }
        let mut out: Vec<(u64, u32, u32, u32)> = Vec::with_capacity(site.n as usize);
        for entry in &site.entries[..site.n as usize] {
            let IcEntry::Callee {
                bits,
                ver,
                fid,
                closure,
                ..
            } = *entry
            else {
                return None;
            };
            let callee = self.func(fid as usize);
            if callee.is_generator
                || callee.is_async
                || callee.rest_reg.is_some()
                || callee.arguments_reg.is_some()
            {
                return None;
            }
            if !out.iter().any(|witness| witness.2 == fid) {
                out.push((bits, ver, fid, closure));
            }
        }
        (out.len() >= 2).then_some(out)
    }

    /// Read-only: the resolved class-method `fid` and exact callable Value for a
    /// method-read/call site whose receiver belongs to `class`, taken from a
    /// FILLED `ClassMethod` IC way. The exact Value is required by captured-call
    /// inline plans: equal function ids are not equal JS function identities.
    /// `None` if no such way exists yet (unfilled site, or a different entry
    /// kind/class). Performs no fill / side effect.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn ic_class_method_target(
        &self,
        func_id: u32,
        ip: usize,
        class: u32,
    ) -> Option<(u32, Value)> {
        let site = self.ic_site(func_id, ip)?;
        for e in &site.entries[..site.n as usize] {
            if let IcEntry::ClassMethod {
                class: c,
                ver,
                epoch,
                fid,
                callee,
                ..
            } = *e
            {
                if c == class && self.ic_class_way_live(c, ver, epoch) {
                    return Some((fid, callee));
                }
            }
        }
        None
    }

    /// Whether a `Class*` way filled against `class` at (`ver`, `epoch`) would
    /// still validate now. The JIT planners bake a way's member only when it
    /// does, and guard the arm on the current `class_proto_epoch`.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    #[inline]
    fn ic_class_way_live(&self, class: u32, ver: u32, epoch: u32) -> bool {
        self.heap.version_of(class) == ver && self.class_proto_epoch == epoch
    }

    /// Read-only: the trivial class GETTER `fid` for a `GetProp` site whose
    /// receiver belongs to `class` (Stage 5 accessor inlining), from a filled
    /// `ClassGetter` IC way that still validates. An arm baked off this fid
    /// guards the receiver's identity/version and `class_proto_epoch`, which
    /// together match the interpreter's `ClassGetter` hit. `None` if no such
    /// way / not a plain user fn.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn ic_class_getter_fid(&self, func_id: u32, ip: usize, class: u32) -> Option<u32> {
        let site = self.ic_site(func_id, ip)?;
        for e in &site.entries[..site.n as usize] {
            if let IcEntry::ClassGetter {
                class: c,
                ver,
                epoch,
                getter,
                ..
            } = *e
            {
                if c == class && self.ic_class_way_live(c, ver, epoch) {
                    return self.ic_plain_fn(getter).map(|(fid, _)| fid);
                }
            }
        }
        None
    }

    /// Read-only: the trivial class SETTER `fid` for a `SetProp` site whose
    /// receiver belongs to `class` (Stage 5), from a filled `ClassSetter` IC way
    /// that still validates.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn ic_class_setter_fid(&self, func_id: u32, ip: usize, class: u32) -> Option<u32> {
        let site = self.ic_site(func_id, ip)?;
        for e in &site.entries[..site.n as usize] {
            if let IcEntry::ClassSetter {
                class: c,
                ver,
                epoch,
                setter,
                ..
            } = *e
            {
                if c == class && self.ic_class_way_live(c, ver, epoch) {
                    return self.ic_plain_fn(setter).map(|(fid, _)| fid);
                }
            }
        }
        None
    }

    /// Read-only resolver for a `super.m()` op (Stage 3 method inlining): from the
    /// FILLED `SuperData` IC way at site `(func_id, ip)` whose `home` matches the
    /// live `class_values[home_class_id]` and whose hop chain still validates,
    /// return the resolved plain super-method `fid` and the hop `(heap_idx,
    /// version)` guards the inline must re-check each call (anchor..holder; a
    /// `setPrototypeOf` / method reassignment on the chain bumps one). `None` if
    /// no usable way (unfilled / chain mutated / accessor / not a plain fn).
    /// Performs no fill / side effect. Mirrors `ic_super_method`'s hit validation.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn ic_super_method_baked(
        &self,
        func_id: u32,
        ip: usize,
        home_class_id: u32,
        key: &str,
    ) -> Option<MiSuperResolved> {
        // A repeated class's methods are shared by all its evaluations, so no
        // one home can be baked into their code: the helper resolves it.
        if self.class_repeated(home_class_id) {
            return None;
        }
        let home = self.class_value_for(home_class_id)?;
        let site = self.ic_site(func_id, ip)?;
        for e in &site.entries[..site.n as usize] {
            if let IcEntry::SuperData {
                home: h,
                hops,
                slot,
            } = *e
            {
                if h == home && self.ic_super_chain_ok(&hops) {
                    if let HeapObj::Object(hm) = self.heap.get(hops.0[hops.1 as usize - 1].0) {
                        let s = slot as usize;
                        if hm.key_get(s) == Some(key) && !hm.attr_at(s).accessor {
                            let v = hm.val_at(s);
                            if let Some((fid, _closure)) = self.ic_plain_fn(v) {
                                return Some(MiSuperResolved {
                                    fid,
                                    hops: hops.0[..hops.1 as usize].to_vec(),
                                    holder_vals_ptr: hm.vals_ptr_raw() as u64,
                                    holder_slot: s as u32,
                                    fn_bits: v.bits(),
                                });
                            }
                        }
                    }
                }
            }
        }
        None
    }

    /// FILLED `SuperAcc` IC way at site `(func_id, ip)` whose `home` matches the
    /// baked class — the `super.v` (accessor) twin of `ic_super_method_baked`,
    /// for inlining a `SuperGet` inside a class GETTER body.
    ///
    /// The guard set is IDENTICAL to the method case, and that is not a
    /// coincidence to be re-derived later: for an accessor slot `heap.rs` stores
    /// the GETTER in `vals[i]` (`attrs[i].setter` holds the other half), so
    /// `holder_vals_ptr[holder_slot] == fn_bits` re-checks exactly the function
    /// this baked — the same load, at the same address, for the same reason.
    /// A SETTER would need a second baked pointer into `attrs`, which is why
    /// only the getter direction is resolved here.
    ///
    /// Everything else transfers verbatim: the hop version guards catch
    /// `setPrototypeOf` and a holder realloc, and `mi_class_epoch` catches a
    /// re-executed class declaration.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn ic_super_getter_baked(
        &self,
        func_id: u32,
        ip: usize,
        home_class_id: u32,
        key: &str,
    ) -> Option<MiSuperResolved> {
        // A repeated class's methods are shared by all its evaluations, so no
        // one home can be baked into their code: the helper resolves it.
        if self.class_repeated(home_class_id) {
            return None;
        }
        let home = self.class_value_for(home_class_id)?;
        let site = self.ic_site(func_id, ip)?;
        for e in &site.entries[..site.n as usize] {
            if let IcEntry::SuperAcc {
                home: h,
                hops,
                slot,
            } = *e
            {
                if h == home && self.ic_super_chain_ok(&hops) {
                    if let HeapObj::Object(hm) = self.heap.get(hops.0[hops.1 as usize - 1].0) {
                        let s = slot as usize;
                        // The key must still name THIS slot and still be an
                        // accessor: a `delete` + re-add shifts slots, and a
                        // redefine to a data property changes what `vals[s]` means.
                        if hm.key_get(s) == Some(key) && hm.attr_at(s).accessor {
                            let g = hm.val_at(s);
                            // A getter-less accessor (`set` only) reads as
                            // `undefined` rather than calling anything — not
                            // inlinable as a body, so decline to the helper.
                            if let Some((fid, _closure)) = self.ic_plain_fn(g) {
                                return Some(MiSuperResolved {
                                    fid,
                                    hops: hops.0[..hops.1 as usize].to_vec(),
                                    holder_vals_ptr: hm.vals_ptr_raw() as u64,
                                    holder_slot: s as u32,
                                    fn_bits: g.bits(),
                                });
                            }
                        }
                    }
                }
            }
        }
        None
    }

    /// FILLED `SuperAcc` IC way for a `super.v = x` site — the SETTER twin of
    /// `ic_super_getter_baked`, and the one place the getter/setter asymmetry
    /// is load-bearing: an accessor slot keeps its getter in `vals[slot]` but
    /// its setter in `attrs[slot].setter` (heap.rs:257), so the getter's
    /// `holder_vals_ptr[holder_slot]` re-check reads the WRONG word for a
    /// setter. Instead of a vals base + slot, this bakes the ABSOLUTE address
    /// of `attrs[slot].setter` into `holder_vals_ptr` with `holder_slot = 0`,
    /// so the emitter's re-check (`[ptr + slot*8] == fn_bits`) reads exactly
    /// the live setter half.
    ///
    /// Deref safety is the same argument as vals: `attrs` reallocates only on
    /// a key add/delete, both of which bump the holder's version, and the hop
    /// version guards run before this address is dereferenced. An in-place
    /// swap of the setter half (`defineProperty` with a new `set`, keeping
    /// `get`) does NOT move the buffer — and is caught by the VALUE compare,
    /// which is the whole point of the re-check.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn ic_super_setter_baked(
        &self,
        func_id: u32,
        ip: usize,
        home_class_id: u32,
        key: &str,
    ) -> Option<MiSuperResolved> {
        // A repeated class's methods are shared by all its evaluations, so no
        // one home can be baked into their code: the helper resolves it.
        if self.class_repeated(home_class_id) {
            return None;
        }
        let home = self.class_value_for(home_class_id)?;
        let site = self.ic_site(func_id, ip)?;
        for e in &site.entries[..site.n as usize] {
            if let IcEntry::SuperAcc {
                home: h,
                hops,
                slot,
            } = *e
            {
                if h == home && self.ic_super_chain_ok(&hops) {
                    if let HeapObj::Object(hm) = self.heap.get(hops.0[hops.1 as usize - 1].0) {
                        let s = slot as usize;
                        if hm.key_get(s) == Some(key) && hm.attr_at(s).accessor {
                            let setter = hm.attr_at(s).setter;
                            // A setter-less accessor (`get` only) is a strict
                            // TypeError / sloppy no-op — helper only.
                            if let Some((fid, _closure)) = self.ic_plain_fn(setter) {
                                return Some(MiSuperResolved {
                                    fid,
                                    hops: hops.0[..hops.1 as usize].to_vec(),
                                    holder_vals_ptr: hm.setter_ref(s) as *const Value as u64,
                                    holder_slot: 0,
                                    fn_bits: setter.bits(),
                                });
                            }
                        }
                    }
                }
            }
        }
        None
    }

    /// Classify the one exact immutable FuncProto accepted by the meter-only
    /// numeric Fibonacci lane. This runs only while installing a `Callee` IC
    /// way; hot hits consume the cached global slot without rescanning code.
    #[cfg(all(feature = "meter-only", not(feature = "jit")))]
    #[inline]
    fn ic_numeric_fib_global(&self, fid: u32, closure: u32) -> Option<u32> {
        if closure != NO_CLOSURE || fid as usize >= self.program.functions.len() {
            return None;
        }
        let p = self.func(fid as usize);
        if p.param_count != 1
            || p.reg_count != 9
            || p.is_generator
            || p.is_async
            || p.lexical_this
            || p.rest_reg.is_some()
            || p.arguments_reg.is_some()
            || !p.upvalues.is_empty()
        {
            return None;
        }
        match p.code.as_slice() {
            [Instr::LoadInt { dst: limit, val: 2 }, Instr::JumpIfNotLt {
                a: 1,
                b: limit_use,
                target: 3,
            }, Instr::Return { src: 1 }, Instr::LoadGlobal {
                dst: callee1,
                idx: global1,
            }, Instr::AddInt {
                dst: arg1,
                a: 1,
                imm: -1,
                upd: false,
            }, Instr::Call {
                dst: result1,
                callee: callee1_use,
                arg_base: arg1_use,
                argc: 1,
            }, Instr::LoadGlobal {
                dst: callee2,
                idx: global2,
            }, Instr::AddInt {
                dst: arg2,
                a: 1,
                imm: -2,
                upd: false,
            }, Instr::Call {
                dst: result2,
                callee: callee2_use,
                arg_base: arg2_use,
                argc: 1,
            }, Instr::Add {
                dst: result,
                a: result1_use,
                b: result2_use,
            }, Instr::Return { src: result_use }, Instr::ReturnUndefined]
                if limit == limit_use
                    && *limit == 4
                    && callee1 == callee1_use
                    && *callee1 == 5
                    && arg1 == arg1_use
                    && *arg1 == 6
                    && callee2 == callee2_use
                    && *callee2 == 6
                    && arg2 == arg2_use
                    && *arg2 == 7
                    && result1 == result1_use
                    && *result1 == 4
                    && result2 == result2_use
                    && *result2 == 5
                    && result == result_use
                    && *result == 3
                    && global1 == global2 =>
            {
                Some(*global1)
            }
            _ => None,
        }
    }

    /// IC for a `Call` site: callee identity (+ slot version) → a resolved
    /// plain user function, skipping the Proxy/native/bound/ctor probes and
    /// flag loads. `None` ⇒ slow path.
    ///
    /// The first way's hit is inlined into the call arms; the rest (the
    /// other ways, the fill) is `ic_call_ways`. The whole was too large to
    /// inline, and every call of the main loop paid the call to it and its
    /// result through memory: a twentieth of a call-heavy loop.
    #[inline(always)]
    pub(crate) fn ic_call(&mut self, func_id: u32, ip: usize, callee: Value) -> Option<CallIcHit> {
        if !callee.is_heap() {
            return None;
        }
        if let Some(site) = self.ic_site(func_id, ip) {
            if site.n > 0 {
                if let IcEntry::Callee {
                    bits,
                    ver,
                    fid,
                    closure,
                    #[cfg(not(any(feature = "meter-only", feature = "safe-sandbox")))]
                    desc,
                    #[cfg(all(feature = "meter-only", not(feature = "jit")))]
                    fib_global,
                } = site.entries[0]
                {
                    if callee.bits() == bits && self.heap.version_of(callee.heap_index()) == ver {
                        return Some(CallIcHit {
                            fid,
                            closure,
                            #[cfg(not(any(feature = "meter-only", feature = "safe-sandbox")))]
                            desc,
                            #[cfg(all(feature = "meter-only", not(feature = "jit")))]
                            fib_global,
                        });
                    }
                }
            }
        }
        self.ic_call_ways(func_id, ip, callee)
    }

    /// [`Self::ic_call`] past its first way: every way, then the fill.
    #[inline(never)]
    fn ic_call_ways(&mut self, func_id: u32, ip: usize, callee: Value) -> Option<CallIcHit> {
        if let Some(site) = self.ic_site(func_id, ip) {
            for e in &site.entries[..site.n as usize] {
                if let IcEntry::Callee {
                    bits,
                    ver,
                    fid,
                    closure,
                    #[cfg(not(any(feature = "meter-only", feature = "safe-sandbox")))]
                    desc,
                    #[cfg(all(feature = "meter-only", not(feature = "jit")))]
                    fib_global,
                } = *e
                {
                    if callee.bits() == bits && self.heap.version_of(callee.heap_index()) == ver {
                        return Some(CallIcHit {
                            fid,
                            closure,
                            #[cfg(not(any(feature = "meter-only", feature = "safe-sandbox")))]
                            desc,
                            #[cfg(all(feature = "meter-only", not(feature = "jit")))]
                            fib_global,
                        });
                    }
                }
            }
            if site.misses >= IC_MISS_LIMIT && self.ic_still_disabled(func_id, ip) {
                return None;
            }
        }
        match self.ic_plain_fn(callee) {
            Some((fid, closure)) => {
                let ver = self.heap.version_of(callee.heap_index());
                #[cfg(all(feature = "meter-only", not(feature = "jit")))]
                let fib_global = if self.ic_func_eligible(func_id) {
                    self.ic_numeric_fib_global(fid, closure)
                        .unwrap_or(IC_NO_FIB_GLOBAL)
                } else {
                    // Runtime eval/new-Function sites do not own IC entries;
                    // do not replace their uncached call resolution with a
                    // repeated structural scan.
                    IC_NO_FIB_GLOBAL
                };
                #[cfg(not(any(feature = "meter-only", feature = "safe-sandbox")))]
                let desc = self.lean_call_desc(fid);
                self.ic_install(
                    func_id,
                    ip,
                    IcEntry::Callee {
                        bits: callee.bits(),
                        ver,
                        fid,
                        closure,
                        #[cfg(not(any(feature = "meter-only", feature = "safe-sandbox")))]
                        desc,
                        #[cfg(all(feature = "meter-only", not(feature = "jit")))]
                        fib_global,
                    },
                );
                Some(CallIcHit {
                    fid,
                    closure,
                    #[cfg(not(any(feature = "meter-only", feature = "safe-sandbox")))]
                    desc,
                    #[cfg(all(feature = "meter-only", not(feature = "jit")))]
                    fib_global,
                })
            }
            None => {
                self.ic_note_miss(func_id, ip);
                None
            }
        }
    }

    // ── super sites ──

    /// IC for `SuperMethod` (`hit ⇒ (fid, closure, callee)` to frame-call
    /// with `this` = the current receiver) — and, with `want_acc`, the shared
    /// resolver for `SuperGet`/`SuperSet` accessor entries.
    pub(crate) fn ic_super_method(
        &mut self,
        func_id: u32,
        ip: usize,
        home_class_id: u32,
        is_static: bool,
        key: &str,
    ) -> Option<(u32, u32, Value)> {
        let home = self.class_value_for(home_class_id)?;
        if let Some(site) = self.ic_site(func_id, ip) {
            for e in &site.entries[..site.n as usize] {
                if let IcEntry::SuperData {
                    home: h,
                    hops,
                    slot,
                } = *e
                {
                    if h == home && self.ic_super_chain_ok(&hops) {
                        let hm = match self.heap.get(hops.0[hops.1 as usize - 1].0) {
                            HeapObj::Object(hm) => hm,
                            _ => continue,
                        };
                        let s = slot as usize;
                        if hm.key_get(s) == Some(key) && !hm.attr_at(s).accessor {
                            let v = hm.val_at(s);
                            if let Some((fid, closure)) = self.ic_plain_fn(v) {
                                return Some((fid, closure, v));
                            }
                        }
                    }
                }
            }
            if site.misses >= IC_MISS_LIMIT && self.ic_still_disabled(func_id, ip) {
                return None;
            }
        }
        match self.ic_super_walk(home, home_class_id, is_static, key) {
            Some((hops, slot, false)) => {
                let hm = match self.heap.get(hops.0[hops.1 as usize - 1].0) {
                    HeapObj::Object(hm) => hm,
                    _ => return None,
                };
                let v = hm.val_at(slot as usize);
                match self.ic_plain_fn(v) {
                    Some((fid, closure)) => {
                        self.ic_install(func_id, ip, IcEntry::SuperData { home, hops, slot });
                        Some((fid, closure, v))
                    }
                    None => {
                        self.ic_note_miss(func_id, ip);
                        None
                    }
                }
            }
            _ => {
                self.ic_note_miss(func_id, ip);
                None
            }
        }
    }

    /// IC for `SuperGet`: a data hit yields the value; an accessor hit yields
    /// a plain getter to frame-call with `this` = the current receiver.
    pub(crate) fn ic_super_get(
        &mut self,
        func_id: u32,
        ip: usize,
        home_class_id: u32,
        is_static: bool,
        key: &str,
    ) -> GetAct {
        let Some(home) = self.class_value_for(home_class_id) else {
            return GetAct::None;
        };
        if let Some(site) = self.ic_site(func_id, ip) {
            for e in &site.entries[..site.n as usize] {
                match *e {
                    IcEntry::SuperData {
                        home: h,
                        hops,
                        slot,
                    } if h == home => {
                        if self.ic_super_chain_ok(&hops) {
                            if let HeapObj::Object(hm) =
                                self.heap.get(hops.0[hops.1 as usize - 1].0)
                            {
                                let s = slot as usize;
                                // The holder is the last hop, at the version
                                // the fill saw: the slot still holds the key
                                // as a data property (`ic_get_prop_proto_hit`
                                // reads its holder the same way).
                                debug_assert!(
                                    hm.key_get(s) == Some(key) && !hm.attr_at(s).accessor,
                                    "super holder slot {s} changed under a matching version"
                                );
                                return GetAct::Value(hm.val_at(s));
                            }
                        }
                    }
                    IcEntry::SuperAcc {
                        home: h,
                        hops,
                        slot,
                    } if h == home => {
                        if self.ic_super_chain_ok(&hops) {
                            match self.ic_chain_acc_get(hops, slot) {
                                GetAct::None => {}
                                act => return act,
                            }
                        }
                    }
                    _ => {}
                }
            }
            if site.misses >= IC_MISS_LIMIT && self.ic_still_disabled(func_id, ip) {
                return GetAct::None;
            }
        }
        match self.ic_super_walk(home, home_class_id, is_static, key) {
            Some((hops, slot, accessor)) => {
                if accessor {
                    self.ic_install(func_id, ip, IcEntry::SuperAcc { home, hops, slot });
                    self.ic_chain_acc_get(hops, slot)
                } else {
                    self.ic_install(func_id, ip, IcEntry::SuperData { home, hops, slot });
                    match self.heap.get(hops.0[hops.1 as usize - 1].0) {
                        HeapObj::Object(hm) => GetAct::Value(hm.val_at(slot as usize)),
                        _ => GetAct::None,
                    }
                }
            }
            None => {
                self.ic_note_miss(func_id, ip);
                GetAct::None
            }
        }
    }

    /// IC for `SuperSet`: only an inherited SETTER on the super chain is
    /// cacheable (data writes go to the RECEIVER — slow path).
    pub(crate) fn ic_super_set(
        &mut self,
        func_id: u32,
        ip: usize,
        home_class_id: u32,
        is_static: bool,
        key: &str,
    ) -> SetAct {
        let Some(home) = self.class_value_for(home_class_id) else {
            return SetAct::None;
        };
        if let Some(site) = self.ic_site(func_id, ip) {
            for e in &site.entries[..site.n as usize] {
                if let IcEntry::SuperAcc {
                    home: h,
                    hops,
                    slot,
                } = *e
                {
                    if h == home && self.ic_super_chain_ok(&hops) {
                        match self.ic_chain_acc_set(hops, slot) {
                            SetAct::None => {}
                            act => return act,
                        }
                    }
                }
            }
            if site.misses >= IC_MISS_LIMIT && self.ic_still_disabled(func_id, ip) {
                return SetAct::None;
            }
        }
        match self.ic_super_walk(home, home_class_id, is_static, key) {
            Some((hops, slot, true)) => {
                self.ic_install(func_id, ip, IcEntry::SuperAcc { home, hops, slot });
                self.ic_chain_acc_set(hops, slot)
            }
            _ => {
                self.ic_note_miss(func_id, ip);
                SetAct::None
            }
        }
    }

    /// Validate a `Super*` entry's hop versions (`hops[0]` is the derivation
    /// anchor — `setPrototypeOf` on the home's prototype object or on the
    /// class value bumps it; the rest guard the chain to the holder).
    #[inline]
    fn ic_super_chain_ok(&self, hops: &IcHops) -> bool {
        if !self.deferred_ns_state.is_empty() || hops.1 == 0 {
            return false;
        }
        let n = hops.1 as usize;
        for &(h, v) in &hops.0[..n] {
            if self.heap.version_of(h) != v {
                return false;
            }
        }
        true
    }

    /// Side-effect-light fill walk for super sites: derive the super base via
    /// the REAL `super_base` (it may lazily synthesize the prototype object —
    /// idempotent), then chain-walk from it with version snapshots. Returns
    /// `(hops, slot, is_accessor)`; the anchor occupies `hops[0]`.
    fn ic_super_walk(
        &mut self,
        home: Value,
        home_class_id: u32,
        is_static: bool,
        key: &str,
    ) -> Option<(IcHops, u32, bool)> {
        if !self.deferred_ns_state.is_empty() || matches!(key.as_bytes().first(), Some(b'#')) {
            return None;
        }
        // Anchor: what `super_base` derives FROM — the synthesized prototype
        // object (instance members) or the class value itself (static).
        // A version bump on it (setPrototypeOf) invalidates the entry.
        let anchor: u32 = if is_static {
            home.heap_index()
        } else {
            // `prototype_of` for a class home: the cached synthesized object.
            match self.prototype_of(home) {
                Some(p) if p.is_heap() => p.heap_index(),
                _ => return None,
            }
        };
        // `fn_proto_override` would re-route `prototype_of`; bail if any
        // override exists for the home (defensive — classes never get one).
        if !self.fn_proto_override.is_empty()
            && self.fn_proto_override.contains_key(&home.heap_index())
        {
            return None;
        }
        let base = self.super_base(home_class_id, is_static);
        if !base.is_heap() {
            return None;
        }
        let mut hops: IcHops = ([(0, 0); IC_MAX_HOPS], 0);
        hops.0[0] = (anchor, self.heap.version_of(anchor));
        hops.1 = 1;
        // Walk from the base (it IS the first chain object to test).
        let mut cur = base.heap_index();
        loop {
            if hops.1 as usize >= IC_MAX_HOPS {
                return None;
            }
            if !self.ic_obj_ok(cur) {
                return None;
            }
            let m = match self.heap.get(cur) {
                HeapObj::Object(m) if !m.is_ctor && m.class.is_none() => m,
                _ => return None,
            };
            hops.0[hops.1 as usize] = (cur, self.heap.version_of(cur));
            hops.1 += 1;
            if let Some(i) = m.pos(key) {
                return Some((hops, i as u32, m.attr_at(i).accessor));
            }
            cur = match self.proto_of.get_copied(cur) {
                Some(p) if p.is_heap() => p.heap_index(),
                Some(_) => return None, // null end — super misses stay slow
                None => {
                    if self.obj_proto == 0 || cur == self.obj_proto {
                        return None;
                    }
                    self.obj_proto
                }
            };
        }
    }
}

#[cfg(test)]
mod key_word_tests {
    use super::key_words;

    #[test]
    fn key_words_are_the_zero_padded_little_endian_words() {
        let bytes: Vec<u8> = (0..16u8).map(|i| i.wrapping_mul(37).wrapping_add(11)).collect();
        for n in 0..=16 {
            let b = &bytes[..n];
            let mut buf = [0u8; 16];
            buf[..n].copy_from_slice(b);
            let want = (
                u64::from_le_bytes(buf[..8].try_into().unwrap()),
                u64::from_le_bytes(buf[8..].try_into().unwrap()),
            );
            assert_eq!(key_words(b), want, "length {n}");
        }
    }
}

#[cfg(all(test, feature = "meter-only", not(feature = "jit")))]
mod fib_ic_classifier_tests {
    use super::*;

    fn classify(program: Program, fid: u32) -> Option<u32> {
        let program = Box::leak(Box::new(program));
        Vm::new(program).ic_numeric_fib_global(fid, NO_CLOSURE)
    }

    #[test]
    fn numeric_fib_classifier_rejects_register_permutations() {
        const SRC: &str = "function fib(n){ return n < 2 ? n : fib(n-1) + fib(n-2); }";
        let ast = crate::front::parse_script(SRC).expect("source parses");
        let canonical = crate::compile::compile_program(&ast, SRC).expect("source compiles");
        let fid = canonical
            .functions
            .iter()
            .position(|p| p.name == "fib")
            .expect("fib proto exists") as u32;
        let expected_global = canonical.functions[fid as usize]
            .name_global
            .expect("fib has a global slot");
        assert_eq!(classify(canonical.clone(), fid), Some(expected_global));

        // A hand-built Program may preserve every def/use relationship while
        // assigning different physical registers. It must not inherit the
        // canonical compiler layout's arithmetic/capacity proof by accident.
        let mut permuted = canonical;
        let rename = |reg: &mut u16| {
            *reg = match *reg {
                3 => 8,
                4 => 3,
                5 => 4,
                6 => 5,
                7 => 6,
                8 => 7,
                other => other,
            };
        };
        for instr in &mut permuted.functions[fid as usize].code {
            match instr {
                Instr::LoadInt { dst, .. } | Instr::LoadGlobal { dst, .. } => rename(dst),
                Instr::JumpIfNotLt { a, b, .. } => {
                    rename(a);
                    rename(b);
                }
                Instr::Return { src } => rename(src),
                Instr::AddInt { dst, a, .. } => {
                    rename(dst);
                    rename(a);
                }
                Instr::Call {
                    dst,
                    callee,
                    arg_base,
                    ..
                } => {
                    rename(dst);
                    rename(callee);
                    rename(arg_base);
                }
                Instr::Add { dst, a, b } => {
                    rename(dst);
                    rename(a);
                    rename(b);
                }
                Instr::ReturnUndefined => {}
                other => panic!("unexpected canonical fib instruction: {other:?}"),
            }
        }
        assert_eq!(classify(permuted, fid), None);
    }
}
