//! Native x86-64 JIT (dynasm) for hot integer functions.
//!
//! This is the first JIT stage, built correctness-first. It compiles a
//! `FuncProto` to native code **only** when the whole function is expressible
//! as pure integer register computation with no calls (a "leaf int function"):
//! a hot numeric loop is the canonical case. Anything outside that set causes
//! the function to be REJECTED at compile time (it keeps running on the
//! interpreter), so a compiled function can never contain an op it doesn't
//! understand.
//!
//! ## Why this can't produce a wrong answer
//!
//! * Compile-time gating: `can_compile` walks the bytecode and refuses any op
//!   that isn't in the integer subset, or any `Call`. So the native code only
//!   ever runs ops it fully implements.
//! * Runtime type guard → bail: every arithmetic/compare op first checks that
//!   its operands are tagged `Int`. If not (a value became a double, string,
//!   etc.), the native code stops and returns a BAIL signal carrying the
//!   instruction index, and the interpreter resumes at exactly that ip with the
//!   register file already consistent (native code and interpreter share the
//!   same `regs` window). No silent fallthrough — the old engine's bug.
//! * Overflow → bail: integer add/sub/mul use the overflow flag; on overflow
//!   the op bails so the interpreter redoes it in the f64 domain. We NEVER
//!   truncate or wrap silently.
//!
//! ## ABI
//!
//! `extern "win64" fn(regs: *mut u64, bail_ip: *mut u32) -> u64`
//! * `rcx = regs` — pointer to this frame's register window (`Value` bits).
//! * `rdx = bail_ip` — out-param. Native writes `u32::MAX` here on a normal
//!   `Return` (and returns the result Value bits in rax), or the instruction
//!   index to resume at on a bail (rax is then ignored).
//!
//! Only `feature = "jit"` + `target_arch = "x86_64"` compiles this; other
//! configs fall back to the pure interpreter.

#![cfg(all(feature = "jit", target_arch = "x86_64"))]

use std::mem;

use dynasmrt::{dynasm, DynasmApi, DynasmLabelApi, ExecutableBuffer};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::bytecode::{FuncProto, Instr, MathFn};
use crate::value::Value;

/// Number of interpreter calls before a function is offered to the JIT.
pub const JIT_THRESHOLD: u32 = 8;

/// Number of times a loop back-edge must fire before the loop region is offered
/// to the OSR (on-stack-replacement) compiler. Low so hot loops promote fast.
pub const OSR_THRESHOLD: u32 = 8;

/// How many times a compiled region may "deopt" (a native run that resumes
/// INSIDE the region — a type guard bailed — rather than exiting cleanly) before
/// it is evicted and blacklisted. Prevents a livelock where the interpreter
/// re-enters native every back-edge only for it to bail at the same guard.
/// Bails tolerated before a region is evicted.
///
/// Was 4, which is a LIFETIME total and far too tight: a single call running a
/// 3M-iteration loop over a Float64Array containing four fractional values bails
/// four times and is then blacklisted for the rest of the process — measured as
/// 4.00 ns/op -> 18.00 ns/op, a 4.5x cliff at a 0.00003% event rate, and three of
/// the ten benches hit it. A failed entry costs one guard check and an immediate
/// exit, so tolerating more of them is cheap; a region that really is wrong (it
/// bails on EVERY entry) still evicts, just after 64 cheap attempts instead of 4.
pub const OSR_DEOPT_LIMIT: u32 = 64;

/// Clean region exits that forgive one accumulated deopt. Makes `OSR_DEOPT_LIMIT`
/// a RATE ("bails without good runs between them") instead of a lifetime total.
///
/// One, not more: a clean exit means the region ran its loop all the way to the
/// loop's own exit, which is a whole unit of productive work — very different
/// from a bail. Note this alone does NOT rescue the case above, where a single
/// long-running loop bails several times and only ever cleanly exits once at the
/// very end; that is what the raised limit is for. The decay covers the other
/// shape: a short loop inside a function called many times.
pub const DEOPT_DECAY_RUNS: u32 = 1;

/// Sentinel in `bail_ip` meaning the native code completed via `Return` (the
/// result is in the returned `u64`). Any other value is the ip to resume at.
pub const NO_BAIL: u32 = u32::MAX;

/// Tag bits for an `Int` value, matching `value.rs` (`0x7FF9 << 48`). A boxed
/// i32 is `INT_TAG | (i32 as u32 as u64)`.
const INT_TAG: u64 = 0x7FF9_0000_0000_0000;
/// Top-16 pattern that identifies an Int (the high 16 bits of `INT_TAG`).
const INT_TAG_HI: u32 = 0x7FF9;

/// NaN-box tag range for the polymorphic region `===` (mirror of value.rs):
/// a Value's high-16 in `[TAG_LO, TAG_HI]` is a tagged form (Int/Bool/Null/
/// Undefined/Heap); anything else is a double. `TAG_HEAP_HI` is the Heap tag.
const TAG_LO: u32 = 0x7FF9;
const TAG_HI: u32 = 0x7FFD;
const TAG_HEAP_HI: u32 = 0x7FFD;
/// Low 48 bits of a Value = a heap index (when the tag is Heap).
const PAYLOAD_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;
/// First non-interned heap index: indices `< 129` are the interned single-ASCII
/// chars (0..128) + the empty string (128); user objects start here. A heap
/// value `>=` this needs full `strict_eq` (the region bits-compare bails on it).
const USER_OBJ_START: i32 = 129;

/// Canonical NaN bits (`Value::num` canonicalises every NaN to this pattern so
/// raw TypedArray bytes can never alias a NaN-box tag). Mirror of value.rs QNAN.
const QNAN_BITS: u64 = 0x7FF8_0000_0000_0000;

/// The internal array-HOLE sentinel bits — `Value::HOLE` = `TAG_UNDEFINED | 2`
/// = `QNAN | (4 << 48) | 2`. A pinned dense-Array element load compares against
/// this to route an absent index to the generic helper (prototype walk), never
/// returning the sentinel to user code. Mirror of value.rs `Value::HOLE`.
const ARR_HOLE_BITS: u64 = 0x7FFC_0000_0000_0002;

/// `Value::TRUE` bits — `TAG_BOOL | 1` = `QNAN | (2 << 48) | 1`. The pinned
/// dense-Array `HasProp` (`i in arr`) inline writes this for an in-range,
/// non-HOLE element (an own property → unconditionally present). Mirror of
/// value.rs `Value::TRUE`.
const BOOL_TRUE_BITS: u64 = 0x7FFA_0000_0000_0001;

/// Where a pinned TypedArray's live Value is RE-READ from at region entry and
/// after every user-code helper: a global slot (`g` never stored in the region)
/// or a frame register (never written in the region). The static choice is only
/// a HINT — every fast-path access re-checks object identity at runtime.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TaPinSrc {
    Global(u32),
    Reg(u16),
}

/// One pinned TypedArray: its live-Value source and the element kind the inline
/// code was specialised for (the snapshot helper re-validates the kind).
/// `kind == DV_PIN_KIND` pins a DataView instead (snapshot: `base = data +
/// byteOffset`, `len = byteLength`) for the whitelisted `get*` methods.
#[derive(Clone, Copy)]
pub struct TaPin {
    pub src: TaPinSrc,
    pub kind: u8,
}

/// `TaPin::kind` marker for a pinned DataView receiver (not a TA element kind).
pub const DV_PIN_KIND: u8 = 255;

/// `TaPin::kind` marker for a pinned flat-ASCII STRING receiver (not a TA element
/// kind). Snapshot: `base = bytes.as_ptr()`, `len = units` (== byte len for ASCII),
/// for inlining `str.charCodeAt(i)` as a direct byte load. A non-ASCII / rope /
/// non-string snapshots all-zero (the per-access identity guard then misses and the
/// access takes the generic `jit_char_code_at` helper — full flatten/surrogate
/// semantics). ASCII-only is the correctness gate: byte i == UTF-16 unit i.
pub const STR_PIN_KIND: u8 = 254;

/// `TaPin::kind` marker for a pinned dense `HeapObj::Array` receiver (not a TA
/// element kind). Snapshot: `base = items.as_ptr()` (the `Vec<Value>` storage),
/// `len = items.len()`, for inlining `arr[i]` (`GetIndex`) and `i in arr`
/// (`HasProp`) as a direct `Value` load. ⚠️ Unlike a TypedArray's fixed buffer,
/// an Array's `Vec` REALLOCATES on growth (push / SetIndex extend / splice /
/// length=), so the pinned base is re-derived (`emit_refetch_ta`) after every
/// op that can grow/replace it — exactly the same discipline as a detach/resize
/// for a TA. A snapshot DECLINES (all-zero → identity guard misses → generic
/// helper) when the array carries an `arr_props` overlay (a defineProperty'd /
/// sparse-overlay index, whose value/accessor lives off the dense Vec) or is a
/// mapped-`arguments` object (a live index reads the formal's register). The
/// inline answers only the in-range, non-HOLE case; a HOLE / out-of-range /
/// non-int key routes to the generic `jit_get_index` / `jit_has_property`
/// helper (full prototype-walk / `in` semantics) — never a new answer.
pub const ARR_PIN_KIND: u8 = 253;

/// `TaPin::kind` for a dense Array **observed to hold only Int-tagged elements**
/// at OSR compile time. Identical to `ARR_PIN_KIND` in layout, snapshotting and
/// every memory-path use — the separate kind exists ONLY so the INTEGER tier can
/// admit `arr[i]` (`region_is_int`), unboxing the element straight into an i64
/// home instead of demoting the whole loop to the boxed memory path. That demotion
/// is what made `for (i…) s += a[i]` — the single most common hot loop in JS —
/// run at 12 ns/element against V8's 0.5 ns.
///
/// The observation is a HEURISTIC (a bounded prefix+stride sample, and the array
/// can hold a double a moment later); it decides only whether the integer tier is
/// worth ATTEMPTING. Soundness rests entirely on the per-access guard, which
/// re-checks the Int tag of the actual element loaded and deopts on any miss —
/// so a double, a HOLE, a heap value or a shrunk array is always correct, merely
/// slow. Sampling keeps a known-double array from compiling INT and then
/// deopt-thrashing to eviction.
pub const ARR_INT_PIN_KIND: u8 = 252;

/// Both dense-Array pin kinds — same snapshot and same memory-path treatment.
pub fn is_arr_pin(kind: u8) -> bool {
    kind == ARR_PIN_KIND || kind == ARR_INT_PIN_KIND
}

/// Compile-time plan for inline TypedArray element access in a memory-path
/// region: the pins (each gets a 32-byte stack snapshot slot `{obj_bits, base,
/// len}` filled by `jit_ta_snapshot`) and, per GetIndex/SetIndex ip, which pin
/// it should guard against. Built in dispatch.rs from LIVE VM state at OSR
/// compile time; empty for non-TA regions.
#[derive(Default)]
pub struct TaPinPlan {
    pub pins: Vec<TaPin>,
    pub access: FxHashMap<usize, u8>,
}

/// Q4 leaf-call inlining (v1). One inlinable monomorphic PLAIN-LEAF callee for a
/// `Call` site in a memory-path region. Built in dispatch.rs from LIVE VM state
/// (the resolved Callee IC entry) at OSR compile time; the emitted code guards
/// the callee register's identity against `callee_bits` and, on a hit, runs the
/// callee body INLINE (no frame push, no `setup_call`, no `run_loop`) over a
/// scratch register window carved at offset `reg_window` above the caller frame.
/// A guard MISS (callee reassigned / a different callee shape appears) falls
/// through to the UNCHANGED `emit_region_call_ic` helper — a pure prefix that
/// never deopts/evicts the region, so polymorphic / unknown callees degrade to
/// today's per-call path.
///
/// SOUNDNESS: the body is verified `callee_leaf_ok` — a straight-line sequence of
/// region-admissible value ops + global reads/writes, NO nested call / heap alloc
/// / closure-cell / upvalue op, exactly one trailing `Return`/`ReturnUndefined`,
/// and NO deopt-capable op after any effect (so an inlined-op bail — which
/// resumes the interpreter AT THE CALL IP and re-runs the whole call — can never
/// double-apply a `StoreGlobal`/`SetProp`). The body touches only the scratch
/// window (its own regs, offset by `reg_window`) and globals (`r12`); a pure leaf
/// runs no GC safepoint, so the carved window needs no zero-fill and the pinned
/// pointers (r12/r13/r14) stay valid across it.
pub struct LeafInlinePlan {
    /// Guard: the caller's callee register must hold exactly these Value bits.
    pub callee_bits: u64,
    /// Guard: the callee heap slot's live VERSION must still equal this baked
    /// value. Heap Value bits are pure `TAG_HEAP|idx`; the parallel `versions[]`
    /// array is the only ABA discriminator. A GC'd + reused slot keeps identical
    /// bits but bumps its version, so the bits-only guard would pass and run the
    /// STALE old callee body. The emitter checks `versions[idx] == callee_ver`
    /// AFTER the bits compare — restoring the exact `(bits, version)` tuple the
    /// interpreter's `ic_call` checks.
    pub callee_ver: u32,
    /// Offset (in registers) of the carved callee scratch window above the
    /// caller frame's window: callee reg `r` lives at caller reg `reg_window+r`.
    pub reg_window: u16,
    /// Callee register-window size (validated to fit the reserved capacity).
    pub callee_reg_count: u16,
    /// The Value bits to write into the callee window's `this` (reg 0).
    ///
    /// The inline site is a plain `Call`, so `thisArg` is undefined — and
    /// `OrdinaryCallBindThis` then does one of two things depending on the
    /// CALLEE: a strict function keeps `undefined`, a sloppy one substitutes its
    /// realm's global object. Baking the answer here is what lets an ordinary
    /// sloppy `function f(a,b){ return a+b; }` inline at all. Hard-coding
    /// `undefined` meant declining every non-strict callee, which measured 26.7ns
    /// per call against 3.6ns for the equivalent method — the single commonest
    /// call shape in the language taking the slowest path.
    pub this_bits: u64,
    /// Callee formal parameter count (args 0..min(argc,param_count) are copied
    /// into scratch regs `reg_window+1 ..` ; reg `reg_window+0` is `this`).
    pub param_count: u16,
    /// The callee body ops to emit inline (with their OWN register numbers).
    pub body: Vec<Instr>,
    /// Resolved numeric-constant bits the body's `LoadConst` ops reference,
    /// keyed by the constant index (the callee's own constant pool index).
    pub consts: FxHashMap<u32, u64>,
    /// Baked CELL for each upvalue index the body READS, as Value bits.
    ///
    /// An inlined body has no frame of its own, so the interpreter's
    /// `jit_upval_get` — which resolves the running closure from the TOP frame —
    /// would read the CALLER's closure. It cannot be used here. The identity
    /// guard above pins the exact closure instance, and a closure's upvalue cells
    /// are fixed for its lifetime, so each cell is resolved once at plan time and
    /// read through `jit_cell_get` at runtime (the cell's CONTENTS still change,
    /// which is why it is a load and not a constant). Empty for a plain function
    /// or a closure with no captures.
    pub upvals: FxHashMap<u16, u64>,
    /// `jit_cell_get` / `jit_cell_set` — carried in the plan rather than as more
    /// positional parameters to `emit_inline_leaf_call`, whose argument order is
    /// documented as load-bearing.
    pub cell_get: usize,
    pub cell_set: usize,
    /// `jit_get_prop_leaf`, and the CALLEE's func id so the emitter can bake
    /// `(callee_fid << 32) | name_idx` for a body `GetProp` — the body's operands
    /// carry the callee's own numbering, so the caller's id would resolve the wrong
    /// string constant. Carried here rather than as more positional parameters, for
    /// the reason above.
    pub prop_get: usize,
    pub callee_fid: u32,
    /// Nested (wrapper) inline: body index of the spliced-in `Call` → the guard
    /// that must hold for the spliced body to be the right one. See
    /// `callee_leaf_ok_one_call`.
    pub nested: FxHashMap<usize, NestedGuard>,
}

/// Identity guard for a nested inline. Same `(bits, version)` tuple the outer
/// call guards, checked against the register the WRAPPER loaded its callee into.
/// A miss jumps to the outer fallback, which re-runs the whole outer call — sound
/// because the admitted nested call precedes any committed effect.
#[derive(Clone, Copy)]
pub struct NestedGuard {
    /// The wrapper's OWN callee register number (the emitter maps it into the
    /// scratch window, like every other body operand).
    pub callee_reg: u16,
    pub bits: u64,
    pub ver: u32,
}

impl LeafInlinePlan {
    /// Boxed bits for a body `LoadConst` constant (numeric — `callee_leaf_ok`
    /// rejected any non-numeric constant, and the planner pre-resolved them).
    fn const_bits(&self, idx: u32) -> u64 {
        self.consts.get(&idx).copied().unwrap_or(0)
    }
}

/// Q7 method-call inlining (v1): an in-region `CallMethod` whose receiver (the
/// `obj` reg) is a known class instance with a trivial straight-line method body
/// (own-`this` field reads + numeric arithmetic; NO `super` in v1) is emitted
/// INLINE behind a per-receiver identity+version guard, with a fallback to the
/// unchanged `jit_call_method_ic` helper on ANY miss (a pure prefix — it never
/// deopts/evicts the region, so a non-baked / mutated / reassigned receiver
/// degrades to today's per-call path).
///
/// SOUNDNESS: the guard re-checks the receiver's Value bits AND its live heap
/// slot version (read from r13), exactly the `(bits, version)` tuple the
/// interpreter's IC checks. The version bumps on every own-key add/delete/
/// redefine, freeze/seal, and setPrototypeOf — so a method-name OWN SHADOW (the
/// G3b hazard, which would make `recv.m()` resolve to the own prop, not the
/// class method) AND any `vals` reallocation both MISS the guard. Behind it the
/// baked `vals_ptr` + field `slot`s are therefore valid, and each `this.<field>`
/// is a direct `vals_ptr[slot]` load (no call, no IC slot). The body is a
/// no-`super` subset of `method_body_inlinable_scan`: a straight-line prefix of
/// own-`this` GetProp + numeric ops ending at the first `Return`, with NO side
/// effect — so any arithmetic op's mid-body number-guard bail can re-run the
/// WHOLE call cleanly (nothing committed). A pure body runs no GC safepoint /
/// alloc / call, so r12/r13/r14 stay valid and the scratch window (zero-filled
/// like `setup_call`) needs no re-fetch.
/// One inlined `super.m()` body (Stage 3), emitted over a sub-window above the
/// outer method's window, over the SAME receiver (reg 0 = recv). v1: the super
/// target is itself a NO-`super`, 0-arg trivial method (e.g. `Shape.area(){return
/// this._v+1}`). Resolution is baked from the live `SuperData` IC entry; the hop
/// version guards re-check the prototype chain each call (a `setPrototypeOf` /
/// method reassignment on the chain bumps a hop version → fall to the helper).
pub struct SuperInline {
    /// Class-redefinition guard: a raw pointer to the VM's `mi_class_epoch` (a
    /// scalar field — its address is stable for the run) + the epoch baked at
    /// compile time. A re-executed class declaration swaps
    /// `class_values[home_class_id]` to a new class WITHOUT mutating the old
    /// prototype objects the hop guards watch, so `*epoch_ptr != epoch_val` is
    /// the discriminator that catches it (→ fall to the helper, which resolves
    /// super against the live `class_values[id]`). Mirrors the interpreter's
    /// `ic_super_method` `home == class_values[home_class_id]` check.
    pub epoch_ptr: u64,
    pub epoch_val: u32,
    /// Super-chain hop version guards `(heap_idx, version)`, anchor..holder.
    pub hops: Vec<(u32, u32)>,
    /// Holder's `vals` base + the method's slot + its baked function Value bits:
    /// a same-slot REASSIGNMENT guard (`Shape.prototype.area = fn`). The
    /// interpreter's super path re-reads the holder slot each call, so the inline
    /// must too — re-check `holder_vals_ptr[holder_slot] == fn_bits` (a chain
    /// realloc is already caught by the hop version guards before this deref).
    pub holder_vals_ptr: u64,
    pub holder_slot: u32,
    pub fn_bits: u64,
    /// Super body `this.<field>` -> own data slot on the SAME receiver.
    pub field_slots: FxHashMap<u32, u32>,
    /// Numeric-constant bits for the super body's `LoadConst` ops.
    pub consts: FxHashMap<u32, u64>,
    /// The super body ops (the super method's own register numbers).
    pub body: Vec<Instr>,
    /// Super body register-window size.
    pub callee_reg_count: u16,
    /// Sub-window base for the super body's registers (above the outer window).
    pub win_off: u16,
}

impl SuperInline {
    fn const_bits(&self, idx: u32) -> u64 {
        self.consts.get(&idx).copied().unwrap_or(0)
    }
    fn field_slot(&self, name: u32) -> u32 {
        self.field_slots.get(&name).copied().unwrap_or(0)
    }
}

/// The extra guards a PROTOTYPE-CHAIN method arm needs (B78).
///
/// `build_method_shape` originally admitted two receiver shapes — a class
/// instance, and a plain object holding the function in an OWN slot — and
/// declined everything else. So `Object.create(proto)` and the classic
/// `Ctor.prototype.m = function …` shape, which is most of the JavaScript
/// written before 2015 and most of what transpilers still emit, inlined NEVER:
/// measured 29.5ns/call at ONE receiver against 5.5ns for the identical method
/// on an ES class, and 1.0ns in node. This closes that arm.
///
/// The receiver's own identity+version guard (already emitted for every arm)
/// carries more weight here than it looks: the version bumps on an own-key ADD
/// (so a later `recv.m = …` SHADOW misses) and inside `ordinary_set_prototype_of`
/// (so a re-pointed first link misses). That leaves exactly two things for this
/// struct: the rest of the chain, and the holder slot's value.
pub struct ProtoMethodGuard {
    /// `(heap_idx, version)` for every object from the receiver's prototype down
    /// to the holder — the same hop set, checked the same way, as
    /// [`SuperInline::hops`]. A `setPrototypeOf` or a key add/delete anywhere on
    /// the chain bumps one of these.
    pub hops: Vec<(u32, u32)>,
    /// Holder's `vals` base + the method's slot + its baked function bits.
    /// REQUIRED, not defensive, for the same reason
    /// [`MethodInlineShape::method_slot`] is: `PROTO.m = other` overwrites an
    /// existing slot in place and deliberately does not bump the holder's
    /// version, so the hop guards alone would happily run the OLD body.
    pub holder_vals_ptr: u64,
    pub holder_slot: u32,
    pub fn_bits: u64,
}

/// One receiver "shape" (arm) of a (possibly polymorphic) inlined CallMethod
/// (Stage 4). Each arm guards a specific receiver instance's identity+version and
/// runs that receiver's resolved class method inline; a miss tries the next arm,
/// and all-miss falls to the helper. v1 enumerates the live receiver exemplar
/// plus, when the receiver is `arr[idx]`, the array's dense elements (the bench's
/// `objs[i&3]` shape) — so the ≤4 fixed instances each get an arm.
pub struct MethodInlineShape {
    /// Guard: the receiver reg must hold exactly these Value bits.
    pub recv_bits: u64,
    /// Guard: the receiver heap slot's live version (ABA + own-shadow +
    /// vals-realloc discriminator).
    pub recv_ver: u32,
    /// Baked base pointer of this receiver's ObjMap `vals` (valid behind the
    /// version guard); shared by the outer body AND its inlined super bodies.
    pub vals_ptr: u64,
    /// Per body `GetProp{obj:0,name}`: callee name index -> own DATA slot.
    pub field_slots: FxHashMap<u32, u32>,
    /// Callee register-window size.
    pub callee_reg_count: u16,
    /// Method formal parameter count.
    pub param_count: u16,
    /// The method body ops to emit inline.
    pub body: Vec<Instr>,
    /// Resolved numeric-constant bits for the body's `LoadConst` ops.
    pub consts: FxHashMap<u32, u64>,
    /// Inlined `super.m()` bodies keyed by their `SuperMethod` body index.
    pub supers: FxHashMap<usize, SuperInline>,
    /// For a PLAIN-OBJECT receiver (`{ m() {} }`, the module/callback shape),
    /// the own `vals` slot the method was found at and the callee bits baked
    /// from it — guarded as `vals_ptr[slot] == bits` before the body runs.
    ///
    /// `None` for a class instance, whose method comes from the class rather
    /// than a property and is already covered by the receiver-version guard.
    ///
    /// This guard is REQUIRED, not defensive: `obj.m = other` overwrites an
    /// existing slot in place and deliberately does NOT bump the version (the
    /// ordinary-set fast path keeps the shape unchanged so JIT caches stay
    /// valid), so identity+version alone would happily run the OLD body.
    pub method_slot: Option<(u32, u64)>,
    /// For a receiver that resolves `name` on its PROTOTYPE CHAIN (B78) — no
    /// class, no own slot. Mutually exclusive with `method_slot`.
    pub proto_method: Option<ProtoMethodGuard>,
}

pub struct MethodInlinePlan {
    /// Carved callee scratch-window offset (callee reg `r` -> caller reg
    /// `reg_window + r`).
    pub reg_window: u16,
    /// Highest scratch reg index used across all arms (outer window + super
    /// sub-windows) — the headroom (`jit_regs_fits`) bound.
    pub win_top: u16,
    /// The receiver arms (1 = monomorphic; ≤ JIT_IC_WAYS). A guard tree.
    pub shapes: Vec<MethodInlineShape>,
}

/// Win64 addresses of the heap helpers (vm.rs), passed from the interpreter into
/// `Jit::compile_region`. The inline-cache base site index is assigned inside
/// `compile_region` (not here), then bundled into `HeapHelpers` for codegen.
#[derive(Clone, Copy)]
pub struct HeapHelperAddrs {
    pub get_prop_miss: usize,
    pub set_prop_miss: usize,
    pub versions_base: usize,
    pub ic_base: usize,
    /// Helper for a dense-array element read `a[i]` (`GetIndex`); returns the
    /// element bits, `undefined` for out-of-range, or the deopt sentinel.
    pub get_index: usize,
    /// Helper for a dense-array element write `a[i] = v` (`SetIndex`); returns 0
    /// on success (storing/growing) or the deopt sentinel.
    pub set_index: usize,
    /// Helper for `arr.push(x)` — returns the new length, or the deopt sentinel.
    pub array_push: usize,
    /// Helper for `str.charCodeAt(i)` — returns the char code / NaN, or deopt.
    pub char_code_at: usize,
    /// Helper for `a + b` (`StrConcat`) — returns the result bits, or deopt.
    pub concat: usize,
    /// Helper for in-place `a + b` (`StrAppendInPlace`) — returns the result bits.
    pub str_append: usize,
    /// Helper for a generic `obj.m(args…)` (`CallMethod`) in a region: consults
    /// the interpreter's per-site inline cache and frame-calls the resolved
    /// plain user function to completion. Returns the result bits,
    /// `SELF_CALL_DEOPT` (IC miss / megamorphic / depth limit → re-execute the
    /// op in the interpreter), or `CALL_THREW` (exception pending → the region
    /// exits and the interpreter unwinds WITHOUT re-executing the call).
    pub call_method_ic: usize,
    /// Helper for a generic `f(args…)` (`Call`) in a region (same protocol).
    pub call_ic: usize,
    /// Helper for a `GetProp` the miss helper routed `PROP_VIA_IC` (accessor /
    /// class-instance receiver): interpreter-IC resolution + getter frame
    /// call. Returns the value bits / `SELF_CALL_DEOPT` / `CALL_THREW`.
    pub get_prop_slow: usize,
    /// The `SetProp` sibling of `get_prop_slow` (setter frame call; 0 = done).
    pub set_prop_slow: usize,
    /// Helper for a region `===`/`!==` whose operands are non-interned heap
    /// values: full `strict_eq` (read-only; returns 0/1, never deopts).
    pub strict_eq: usize,
    /// Helper for a region truthiness test on a non-Int/Bool value (`!x`,
    /// `JumpIfFalse/True` conditions): full `truthy` (read-only; 0/1).
    pub truthy: usize,
    /// Helper that (re)derives a pinned TypedArray's `{obj_bits, base, len}`
    /// snapshot into a region stack slot (no alloc, no user code).
    pub ta_snapshot: usize,
    /// Helper for a Uint8Clamped store of a DOUBLE value: round-half-even
    /// clamp + byte store (pure — no vm, no alloc).
    pub ta_clamp_store: usize,
    /// Helper for a whitelisted `dv.get*(pos[, le])` DataView read (no alloc,
    /// no user code; anything unusual returns the deopt sentinel).
    pub dv_get: usize,
    /// Helper for a UNARY `Math.<op>` over an already-numeric arg (pure: the
    /// region guards the arg numeric and passes its raw f64 bits). Returns the
    /// result's f64 bits. No vm, no alloc, no user code.
    pub math_unary: usize,
    /// Helper for a TWO-ARG `Math.<op>` (Pow/Atan2/Imul/Min/Max/Hypot) over
    /// already-numeric args (pure, same constraints). Returns f64 bits.
    pub math_two: usize,
    /// Helper for `CellGet` / `UpvalGet` reading a captured-local cell: a pure
    /// heap LOAD of the cell's inner Value (no alloc, no user code). Returns the
    /// inner Value bits, or `SELF_CALL_DEOPT` for a still-uninitialized (TDZ)
    /// cell so the region bails and the interpreter throws.
    pub cell_get: usize,
    /// Helper for `UpvalGet` (resolves the running closure's k-th upvalue cell,
    /// then loads it). Same purity/TDZ contract as `cell_get`.
    pub upval_get: usize,
    /// Intrinsic for `s.indexOf(t)` (ASCII/ASCII, 1 arg); deopts otherwise.
    pub str_index_of: usize,
    /// Intrinsic for `s.substring(a,b)` / `s.slice(a,b)` (ASCII, Int args).
    pub str_substring: usize,
    /// Intrinsic for `Map.get`/`Map.has`/`Set.has` (op selects which).
    pub coll_lookup: usize,
    /// Helpers for `CellSet` / `UpvalSet` — a captured-cell WRITE. A plain heap
    /// store: no TDZ check, no alloc, no user code, so no pinned refetch.
    pub cell_set: usize,
    pub upval_set: usize,
    /// Helper for `GetIndexConcat` (`obj["name" + i]`) — the own-DATA fast path
    /// only; a miss or an exotic receiver deopts to the interpreter.
    pub get_index_concat: usize,
    /// Helper for `ForInLive` (per-iteration for-in liveness). Delegates to the
    /// shared `Vm::forin_live`, so it matches the interpreter byte-for-byte.
    /// May allocate transiently (key re-derivation) — GC-guarded internally.
    /// Returns the BOOL Value bits.
    pub forin_live: usize,
    /// Helper for `HasProp` (the `in` operator, brand=false) over a non-Proxy
    /// chain: read-only `Vm::has_property_jit`. Returns the BOOL Value bits, or
    /// `SELF_CALL_DEOPT` when the op needs user code / a throw (interpreter only).
    pub has_property: usize,
    /// Helper for the Q4 leaf-inline ENTRY headroom check (`jit_regs_fits`):
    /// returns 1 when a carved callee scratch window fits the pinned register
    /// file. Called once per OSR entry when the region has any inlined call.
    pub regs_fits: usize,
    /// Tier C `TypeOf` — `vm.type_of(v)` interned/alloc'd to a heap string;
    /// returns the result Value bits (a heap string; compared by content via
    /// the `strict_eq` slow path). Allocates ⇒ post-call refetch when has_prop.
    pub typeof_str: usize,
    pub typeof_is: usize,
    pub static_fn: usize,
    pub to_concat_key: usize,
    pub set_index_concat: usize,
    /// Tier C `IsArray` — `Array.isArray(v)`; returns Bool bits, or the deopt
    /// sentinel for the rare throwing case (revoked Proxy).
    pub is_array: usize,
    /// Tier C `LenOf` — for-in key-snapshot / array / string length; returns the
    /// length Value bits (pure, total).
    pub len_of: usize,
    /// Tier C `ForInKeys` — materialises the for-in key snapshot Array (nullish →
    /// empty). ALLOCATES; returns the Array bits / SELF_CALL_DEOPT / CALL_THREW.
    pub forin_keys: usize,
}

impl HeapHelperAddrs {
    /// Bundle these helper addresses with the COMPILING function's id and the
    /// reserved inline-cache base into the codegen-internal `HeapHelpers`. Used
    /// by both the OSR region path and Tier C (whole-function mem path).
    fn to_heap_helpers(&self, func_id: u32, ic_base_idx: u32) -> HeapHelpers {
        HeapHelpers {
            func_id,
            get_prop_miss: self.get_prop_miss,
            set_prop_miss: self.set_prop_miss,
            versions_base: self.versions_base,
            ic_base: self.ic_base,
            get_index: self.get_index,
            set_index: self.set_index,
            array_push: self.array_push,
            char_code_at: self.char_code_at,
            concat: self.concat,
            str_append: self.str_append,
            call_method_ic: self.call_method_ic,
            call_ic: self.call_ic,
            get_prop_slow: self.get_prop_slow,
            set_prop_slow: self.set_prop_slow,
            strict_eq: self.strict_eq,
            truthy: self.truthy,
            ta_snapshot: self.ta_snapshot,
            ta_clamp_store: self.ta_clamp_store,
            dv_get: self.dv_get,
            math_unary: self.math_unary,
            math_two: self.math_two,
            cell_get: self.cell_get,
            cell_set: self.cell_set,
            upval_set: self.upval_set,
            get_index_concat: self.get_index_concat,
            upval_get: self.upval_get,
            str_index_of: self.str_index_of,
            str_substring: self.str_substring,
            coll_lookup: self.coll_lookup,
            forin_live: self.forin_live,
            has_property: self.has_property,
            regs_fits: self.regs_fits,
            typeof_str: self.typeof_str,
            typeof_is: self.typeof_is,
            static_fn: self.static_fn,
            to_concat_key: self.to_concat_key,
            set_index_concat: self.set_index_concat,
            is_array: self.is_array,
            len_of: self.len_of,
            forin_keys: self.forin_keys,
            ic_base_idx,
        }
    }
}

/// Is the Tier C whole-function memory-path JIT enabled? ON by default (validated
/// against the full test262 sweep — JIT flag-on 48466/1 with zero new failures —
/// and bench ALL_CORRECT); opt out with `ZIPP_NO_FNJIT_MEM` to fall back to the
/// prior tiers only. (`ZIPP_NOJIT` disables the whole JIT upstream of here.) Read
/// once per `Jit::compile` call (compiles are rare relative to execution).
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
fn fnjit_mem_enabled() -> bool {
    std::env::var_os("ZIPP_NO_FNJIT_MEM").is_none()
}

/// One compiled native function plus the buffer backing it.
pub struct JitFn {
    _buf: ExecutableBuffer,
    entry: *const u8,
    /// `(global slot, the Value bits it held at compile time)` when this body
    /// contains a SELF-CALL — Tier A's `emit_self_call`, which emits a direct
    /// `call` to this function's own entry with no callee guard at all.
    ///
    /// That direct call is only equivalent to the bytecode `LoadGlobal(self_slot)
    /// + Call` it replaces while the slot still holds THIS function. Rebind the
    /// name (`fib = function (n) { return 0; }`) and the interpreter's every inner
    /// `fib(n-1)` re-resolves to the new function, collapsing the recursion on the
    /// first hop, while compiled code kept calling itself — B66's second open tier
    /// divergence, and a wrong ANSWER, not a crash.
    ///
    /// `None` for a body with no self-call and for every Tier C / region / kernel
    /// compile, which resolve callees the ordinary way.
    self_binding: Option<(u32, u64)>,
}

impl JitFn {
    /// Raw native entry pointer (for self-recursive calls that re-enter the
    /// same code through the win64 trampoline).
    pub fn entry(&self) -> *const u8 {
        self.entry
    }

    /// The compile-time self-binding assumption, if this body made one.
    #[inline]
    pub fn self_binding(&self) -> Option<(u32, u64)> {
        self.self_binding
    }

    /// Run the native code over `regs`. ABI: `(regs, bail_ip, vm) -> result`.
    /// Returns `(result_bits, bail_ip)`: `bail_ip == NO_BAIL` means a normal
    /// return with `result_bits`; otherwise the interpreter must resume at
    /// `bail_ip` (result_bits is meaningless).
    ///
    /// # Safety
    /// `regs` must point to at least the function's `reg_count` valid `Value`
    /// slots; `vm` must be a valid `*mut Vm`; the buffer outlives the call.
    pub unsafe fn run(&self, regs: *mut u64, vm: *mut core::ffi::c_void) -> (u64, u32) {
        let _prof = crate::vm::prof::enter(crate::vm::prof::Phase::Jit);
        let f: extern "win64" fn(*mut u64, *mut u32, *mut core::ffi::c_void) -> u64 =
            mem::transmute(self.entry);
        let mut bail: u32 = NO_BAIL;
        let r = f(regs, &mut bail as *mut u32, vm);
        (r, bail)
    }
}

/// A compiled loop region (OSR): native code for the bytecode range
/// `[start, end]`, entered at `start` (the loop header). `deopts` counts native
/// runs that bailed back inside the region; past `OSR_DEOPT_LIMIT` the region is
/// evicted. A native run returns the ip to resume interpreting at — a clean loop
/// exit (ip outside `[start,end]`) or a guard bail (ip inside it).
pub struct Region {
    code: JitFn,
    start: u32,
    end: u32,
    deopts: u32,
    /// Successful (non-bailing) exits since the last decay. `deopts` is a RATE
    /// budget, not a lifetime total — see `note_region_resume`.
    ok_runs: u32,
    /// True if compiled by the integer path. On eviction an int region falls
    /// back to the double path (rather than full-blacklisting the loop).
    is_int: bool,
    /// True if compiled by the MEMORY path (`region_mem`) rather than one of the
    /// register-homed paths. Purely diagnostic: `try_run_osr` uses it to pick the
    /// profiler phase, so `ZIPP_PROF=1` can separate `jit-fast` from `jit-mem`.
    is_mem: bool,
    /// Set when this region was object-scalar-replaced (SROA): its GetProp/SetProp
    /// were rewritten to scratch field-globals. The interpreter must sync the
    /// object's fields ↔ the pool slots around each native run.
    field_plan: Option<FieldSyncPlan>,
}

/// How the interpreter syncs a scalar-replaced object around a native region run:
/// for each accessed field, the pool global slot holding it during the run.
#[derive(Clone)]
pub struct FieldSyncPlan {
    /// The global slot holding the promoted object.
    pub obj_global: u32,
    /// `(field name-constant index, pool global slot)` for each accessed field.
    pub fields: Vec<(u32, u32)>,
    /// The function id whose `string_constants` the name indices belong to.
    pub func_id: u32,
    /// Compile-time heap identity of the promoted object: a region-entry guard
    /// bails (→ interpreter) if the global was reassigned to a different object.
    pub obj_idx: u32,
    /// Compile-time heap shape version: a region-entry guard bails if it changed
    /// (a key add/remove/redefine, freeze, or `setPrototypeOf` could have turned
    /// a promoted data field into an accessor / non-writable — see
    /// sroa-accessor-miscompile). Normal stores to existing slots don't bump it.
    pub obj_version: u32,
}

/// Ways per JIT inline-cache site (matches the interpreter's `IC_WAYS`): a
/// region `GetProp`/`SetProp` probes up to this many entries natively before
/// falling to the miss helper, so polymorphic shape-cycling loops stay call-free.
pub const JIT_IC_WAYS: usize = 8;
/// Byte stride of one [`IcEntry`] (`size_of`, kept explicit for the emitter).
pub const JIT_IC_STRIDE: usize = 64;
/// Maximum proto-chain hops one [`IcEntry`] can guard (the interpreter's
/// `IC_MAX_HOPS` is 6; 5 fits the 64-byte entry and covers depth-5 chains —
/// the deepest the real-world benches walk).
pub const JIT_IC_MAX_HOPS: usize = 5;

/// One way of a JIT'd `GetProp`/`SetProp` site's inline cache. `repr(C)` with a
/// fixed layout the native code indexes directly: `obj_bits @0`, `vals_ptr @8`,
/// `version @16`, `slot|nhops<<24 @20`, then `nhops` pairs `(hop_idx @24+8k,
/// hop_ver @28+8k)` (stride 64). The native probe checks `obj_bits` (receiver
/// identity) AND the receiver's live `version` (read from the heap's parallel
/// version array — catches own key add/remove/redefine, freeze, and
/// setPrototypeOf, all of which bump it) — then, for a PROTO-CHAIN entry
/// (`nhops > 0`), the live version of each chain hop down to the holder (whose
/// `vals_ptr`/`slot` the hit reads); any chain mutation bumps a guarded hop.
/// Writes only ever fill OWN entries (`nhops == 0`). On a full match it
/// reads/writes `vals_ptr[slot]` with NO call (slots never move without a
/// version bump). `obj_bits == 0` means empty (no real object Value is 0).
/// `slot` is packed into 24 bits (an ObjMap with 16M+ keys is unreachable —
/// fills exceeding it are skipped defensively).
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct IcEntry {
    pub obj_bits: u64,
    pub vals_ptr: u64,
    pub version: u32,
    pub slot_nhops: u32,
    pub hops: [(u32, u32); JIT_IC_MAX_HOPS],
}

impl IcEntry {
    /// An OWN-property way (receiver == holder; no hop guards). `None` if the
    /// slot doesn't fit the 24-bit packing (never in practice).
    pub fn own(obj_bits: u64, vals_ptr: u64, version: u32, slot: u32) -> Option<IcEntry> {
        if slot > 0x00FF_FFFF {
            return None;
        }
        Some(IcEntry {
            obj_bits,
            vals_ptr,
            version,
            slot_nhops: slot,
            hops: [(0, 0); JIT_IC_MAX_HOPS],
        })
    }

    /// A PROTO-CHAIN way: the holder is `hops.len()` (1..=5) hops from the
    /// receiver; every hop is version-guarded.
    pub fn chain(
        obj_bits: u64,
        vals_ptr: u64,
        version: u32,
        slot: u32,
        hops: &[(u32, u32)],
    ) -> Option<IcEntry> {
        if slot > 0x00FF_FFFF || hops.is_empty() || hops.len() > JIT_IC_MAX_HOPS {
            return None;
        }
        let mut h = [(0u32, 0u32); JIT_IC_MAX_HOPS];
        h[..hops.len()].copy_from_slice(hops);
        Some(IcEntry {
            obj_bits,
            vals_ptr,
            version,
            slot_nhops: slot | ((hops.len() as u32) << 24),
            hops: h,
        })
    }
}

/// Dense function-tier states (see `Jit::fn_state`).
pub const FN_COLD: u8 = 0;
pub const FN_COMPILED: u8 = 1;
pub const FN_DEAD: u8 = 2;

/// Per-function JIT state: call counts, compiled code, and a blacklist of
/// functions that aren't eligible (so we don't re-attempt them every tick).
/// The `region_*` maps mirror this for OSR loop regions, keyed by
/// `(func_id, loop_header_ip)`.
#[derive(Default)]
pub struct Jit {
    /// `ZIPP_JIT_THRESHOLD=<n>` replaces BOTH [`JIT_THRESHOLD`] and
    /// [`OSR_THRESHOLD`] for the process; `0` (the `Default`, and the absence of
    /// the variable) means "use the constants". Read once in [`Jit::new`], so
    /// the count paths stay a field compare.
    ///
    /// This exists because the standing gate cannot see a JIT-only bug. §2 runs
    /// test262 under `ZIPP_NOJIT=1` to prove the interpreter, but the region JIT
    /// only compiles hot LOOPS and test262 asserts once, straight-line — so
    /// helpers like `jit_get_index` are never reached by 95,936 executions.
    /// B63 found a real `arr[oob]` prototype-chain divergence there by hand,
    /// while doing something else. With `ZIPP_JIT_THRESHOLD=1` the same suite
    /// becomes a JIT gate at no authoring cost.
    threshold_override: u32,
    counts: FxHashMap<u32, u32>,
    compiled: FxHashMap<u32, JitFn>,
    blacklist: FxHashSet<u32>,
    /// Dense per-func_id tier state ([`FN_COLD`]/[`FN_COMPILED`]/[`FN_DEAD`]),
    /// grown on demand. One array read on EVERY interpreted frame entry
    /// replaces the 2-3 hash probes (`compiled` miss + `blacklist` hit) that
    /// otherwise tax each call to a never-compiled function.
    fn_state: Vec<u8>,
    regions: FxHashMap<(u32, u32), Region>,
    region_counts: FxHashMap<(u32, u32), u32>,
    region_blacklist: FxHashSet<(u32, u32)>,
    /// Loop headers where the INTEGER path was tried and deoptimised; the next
    /// compile for the key skips int and uses the double path instead.
    region_int_blacklist: FxHashSet<(u32, u32)>,
    /// Inline-cache ways for heap-op JIT sites: site `k` owns the contiguous
    /// entries `[k*JIT_IC_WAYS, (k+1)*JIT_IC_WAYS)`. Grows only at compile time
    /// (never during a native run, EXCEPT through a region call helper — after
    /// which the region re-derives its pinned base pointer); a `*_miss` helper
    /// only UPDATES existing ways (no growth).
    ic_table: Vec<IcEntry>,
    /// Round-robin fill cursor per site (parallel to `ic_table` / JIT_IC_WAYS).
    ic_rot: Vec<u8>,
    /// One-entry cache of the most recent self-call target `(func_id, native
    /// entry)`. A self-recursive function (e.g. `fib`) always recurses into the
    /// SAME `func_id`, so this hits on every call and skips the `compiled`
    /// HashMap lookup that otherwise runs ~30M times for `fib(35)`. The cached
    /// entry pointer stays valid even if `compiled` rehashes: it points into the
    /// function's mmap'd `ExecutableBuffer`, which never moves, and a function's
    /// entry is immutable once compiled.
    self_cache: Option<(u32, *const u8)>,
    /// Compiled fused `map` kernels, keyed by callback `func_id`. `None` =
    /// tried and ineligible (so we don't recompile every `map` call). Keyed by
    /// `func_id` alone: a given callback proto has fixed param_count/body.
    map_kernels: FxHashMap<u32, Option<JitFn>>,
    /// Compiled fused `reduce` kernels, keyed by callback `func_id` (as above).
    reduce_kernels: FxHashMap<u32, Option<JitFn>>,
    /// Compiled fused `filter` kernels, keyed by predicate `func_id` (as above).
    filter_kernels: FxHashMap<u32, Option<JitFn>>,
    /// Where compiled code charges the step budget, when this VM is metered.
    /// Set once by `Vm::set_instrumentation`; `None` means every emitter is
    /// byte-for-byte what an uninstrumented build produces.
    meter: Option<meter::Meter>,
    /// Evicted regions parked here instead of being dropped. A region can be
    /// evicted REENTRANTLY (its `jit_call_*_ic` helper runs user code, which can
    /// loop back into the SAME region and deopt it past the limit) while an
    /// outer activation of that region is still executing on the native stack —
    /// dropping the `ExecutableBuffer` then would unmap code we're inside.
    /// Parked regions are freed only when the VM drops. Bounded: each loop key
    /// evicts at most twice (int retry, then blacklist).
    retired: Vec<Region>,
}

impl Jit {
    pub fn new() -> Jit {
        let mut jit = Jit::default();
        // Values below 1 are meaningless (a counter starts at 1, so `== 0` would
        // never fire and would silently disable the tier — the opposite of what
        // anyone setting this wants), so they are ignored.
        jit.threshold_override = std::env::var("ZIPP_JIT_THRESHOLD")
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
            .filter(|n| *n >= 1)
            .unwrap_or(0);
        jit
    }

    /// Interpreter entries before a function is offered to the JIT.
    #[inline]
    fn fn_threshold(&self) -> u32 {
        if self.threshold_override != 0 { self.threshold_override } else { JIT_THRESHOLD }
    }

    /// Back-edges before a loop region is offered to the OSR compiler.
    #[inline]
    fn loop_threshold(&self) -> u32 {
        if self.threshold_override != 0 { self.threshold_override } else { OSR_THRESHOLD }
    }

    /// Point compiled code at a step counter, and throw away everything already
    /// compiled without it.
    ///
    /// The discard is the whole point: code emitted before the VM was metered
    /// contains no charge, so leaving it installed would leave a permanently
    /// unmetered native path — the exact hole this machinery exists to close.
    /// Buffers are PARKED rather than dropped, because a native frame may be
    /// live on the stack (see `retired`).
    pub fn set_meter(&mut self, m: meter::Meter) {
        self.meter = Some(m);
        self.retired.extend(self.regions.drain().map(|(_, r)| r));
        self.compiled.clear();
        self.counts.clear();
        self.region_counts.clear();
        self.fn_state.clear();
        self.self_cache = None;
        self.map_kernels.clear();
        self.reduce_kernels.clear();
        self.filter_kernels.clear();
    }

    /// Whether this VM meters native code. The fused array kernels and the
    /// off-frame method inliner run user work with no VM pointer and no
    /// interpreter loop, so a metered VM declines them rather than leaving an
    /// uncharged native path open.
    pub fn metered(&self) -> bool {
        self.meter.is_some()
    }

    /// Look up compiled native code for `func_id`, if any.
    pub fn get(&self, func_id: u32) -> Option<&JitFn> {
        self.compiled.get(&func_id)
    }

    /// Native entry pointer for a SELF-CALL target, via a one-entry cache that
    /// the hot recursive path hits every time (skipping the `compiled` HashMap
    /// lookup). See `self_cache`. Returns `None` if `func_id` isn't compiled.
    #[inline]
    pub fn self_call_entry(&mut self, func_id: u32) -> Option<*const u8> {
        if let Some((id, entry)) = self.self_cache {
            if id == func_id {
                return Some(entry);
            }
        }
        let entry = self.compiled.get(&func_id)?.entry();
        self.self_cache = Some((func_id, entry));
        Some(entry)
    }

    /// Record an interpreter entry into `func_id`. Returns `true` once it
    /// crosses the threshold and is neither compiled nor blacklisted — the
    /// caller should then attempt `compile`.
    pub fn record_and_should_compile(&mut self, func_id: u32) -> bool {
        if self.compiled.contains_key(&func_id) || self.blacklist.contains(&func_id) {
            return false;
        }
        let c = self.counts.entry(func_id).or_insert(0);
        *c += 1;
        *c == self.fn_threshold()
    }

    /// Dense tier state of `func_id` — the frame-entry fast path.
    #[inline]
    pub fn fn_state(&self, func_id: u32) -> u8 {
        self.fn_state.get(func_id as usize).copied().unwrap_or(FN_COLD)
    }

    fn set_fn_state(&mut self, func_id: u32, s: u8) {
        let i = func_id as usize;
        if self.fn_state.len() <= i {
            self.fn_state.resize(i + 1, FN_COLD);
        }
        self.fn_state[i] = s;
    }

    /// Attempt to compile `proto` (id `func_id`). On success it becomes
    /// available via `get`; on failure the id is blacklisted and never retried.
    /// `self_call_helper` is the address of the depth-guarded Rust trampoline
    /// the native code invokes for a self-recursive call (see vm.rs).
    pub fn compile(
        &mut self,
        func_id: u32,
        proto: &FuncProto,
        self_call_helper: usize,
        self_val_bits: u64,
        globals_base_helper: usize,
        heap_helpers: HeapHelperAddrs,
        const_strs: &FxHashMap<u32, u64>,
        // Tier-C leaf-inline plan for this whole function (built by the caller from
        // the live ICs; empty = no inlining, e.g. when ZIPP_NO_TIERC_LEAF is set).
        leaf_plan: &FxHashMap<usize, LeafInlinePlan>,
    ) {
        if self.compiled.contains_key(&func_id) || self.blacklist.contains(&func_id) {
            return;
        }
        let meter = self.meter;
        match compile_proto(proto, func_id, self_call_helper, self_val_bits, meter) {
            Some(f) => {
                self.compiled.insert(func_id, f);
                self.set_fn_state(func_id, FN_COMPILED);
                return;
            }
            None => {}
        }
        // ── Tier C (whole-function memory path) ── Tier A declined (not a
        // fib-shaped int self-recursion). Try the call-heavy / recursive-descent
        // path before blacklisting. Gated behind ZIPP_FNJIT_MEM (default-ON;
        // opt out with ZIPP_NO_FNJIT_MEM).
        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
        if std::env::var_os("ZIPP_JITLOG").is_some() && !mem_can_compile(proto, const_strs) {
            eprintln!("[jit] fn{func_id} mem_can_compile=false ({} ops)", proto.code.len());
        }
        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
        if fnjit_mem_enabled() && mem_can_compile(proto, const_strs) {
            // One inline-cache site per GetProp/SetProp in the whole function
            // (0 for the call/arith-only functions of v1). reserve_ic_sites never
            // grows the table after, so the pinned r14 stays valid for a run.
            let n_sites = proto
                .code
                .iter()
                .filter(|i| matches!(i, Instr::GetProp { .. } | Instr::SetProp { .. }))
                .count();
            let ic_base_idx = self.reserve_ic_sites(n_sites);
            let helpers = heap_helpers.to_heap_helpers(func_id, ic_base_idx);
            if let Some(f) =
                compile_proto_mem(proto, func_id, globals_base_helper, helpers, const_strs, leaf_plan, meter)
            {
                if std::env::var_os("ZIPP_JITLOG").is_some() {
                    eprintln!(
                        "[jit] Tier C fn{func_id} compiled (whole-function mem path, leaf_inlines={})",
                        leaf_plan.len()
                    );
                }
                self.compiled.insert(func_id, f);
                self.set_fn_state(func_id, FN_COMPILED);
                return;
            }
        }
        if std::env::var_os("ZIPP_JITLOG").is_some() {
            eprintln!("[jit] fn{func_id} BLACKLISTED (neither Tier A nor Tier C compiled)");
        }
        self.blacklist.insert(func_id);
        self.set_fn_state(func_id, FN_DEAD);
    }

    /// Native entry for the fused `map` kernel of callback `func_id`, compiling
    /// (and caching) it on first request. Returns `None` if the callback isn't
    /// kernel-eligible. The entry pointer is into the kernel's mmap'd buffer
    /// (stable; the cache `Option` owns the buffer for the VM's lifetime).
    pub fn map_kernel(&mut self, func_id: u32, proto: &FuncProto) -> Option<*const u8> {
        if let Some(slot) = self.map_kernels.get(&func_id) {
            return slot.as_ref().map(|f| f.entry());
        }
        let compiled = compile_map_kernel(proto);
        let entry = compiled.as_ref().map(|f| f.entry());
        self.map_kernels.insert(func_id, compiled);
        entry
    }

    /// Native entry for the fused `reduce` kernel of callback `func_id`,
    /// compiling and caching on first request. `None` if ineligible.
    pub fn reduce_kernel(&mut self, func_id: u32, proto: &FuncProto) -> Option<*const u8> {
        if let Some(slot) = self.reduce_kernels.get(&func_id) {
            return slot.as_ref().map(|f| f.entry());
        }
        let compiled = compile_reduce_kernel(proto);
        let entry = compiled.as_ref().map(|f| f.entry());
        self.reduce_kernels.insert(func_id, compiled);
        entry
    }

    /// Native entry for the fused `filter` kernel of predicate `func_id`,
    /// compiling and caching on first request. `None` if ineligible.
    pub fn filter_kernel(&mut self, func_id: u32, proto: &FuncProto) -> Option<*const u8> {
        if let Some(slot) = self.filter_kernels.get(&func_id) {
            return slot.as_ref().map(|f| f.entry());
        }
        let compiled = compile_filter_kernel(proto);
        let entry = compiled.as_ref().map(|f| f.entry());
        self.filter_kernels.insert(func_id, compiled);
        entry
    }

    // ── OSR loop regions ──

    /// Native code for the loop region of `func_id` whose header is `entry_ip`.
    pub fn get_region(&self, func_id: u32, entry_ip: u32) -> Option<&Region> {
        self.regions.get(&(func_id, entry_ip))
    }

    /// Count a back-edge to the loop headed at `entry_ip`. Returns `true` exactly
    /// once, when the count crosses `OSR_THRESHOLD` and the region is neither
    /// compiled nor blacklisted — the caller should then attempt `compile_region`.
    pub fn record_region(&mut self, func_id: u32, entry_ip: u32) -> bool {
        let key = (func_id, entry_ip);
        // Called only after a `get_region` miss on the same back-edge, so
        // `regions` cannot contain the key — only the blacklist gates. This
        // keeps the per-back-edge cost of a permanently-rejected loop to one
        // hash probe (plus the counter bump until it is blacklisted).
        if self.region_blacklist.contains(&key) {
            return false;
        }
        let c = self.region_counts.entry(key).or_insert(0);
        *c += 1;
        *c == self.loop_threshold()
    }

    /// Permanently blacklist the region headed at `entry_ip` (the dispatch-side
    /// call-mix gate found it would lose to the interpreter — e.g. dominated by
    /// always-fallback native-callee sites).
    pub fn blacklist_region(&mut self, func_id: u32, entry_ip: u32) {
        if std::env::var_os("ZIPP_JITLOG").is_some() {
            eprintln!("[jit] region fn{func_id} [{entry_ip}] DECLINED (call-mix gate)");
        }
        self.region_blacklist.insert((func_id, entry_ip));
    }

    /// Undo the threshold trip reported by [`Jit::record_and_should_compile`]:
    /// the caller found the function not YET safe to compile (a global op whose
    /// slot is still uninitialized, so the binding may be an own property of the
    /// global object rather than a slot). Re-arm so a later call re-checks.
    ///
    /// The sibling of [`Jit::region_defer`], and it was missing — see B65.
    pub fn compile_defer(&mut self, func_id: u32) {
        if let Some(c) = self.counts.get_mut(&func_id) {
            *c -= 1;
        }
    }

    /// Undo the threshold trip reported by [`Jit::record_region`]: the caller
    /// found the region not YET safe to compile (an uninitialized global in
    /// the body) but expects it to become safe — re-arm so a later back-edge
    /// re-trips the threshold and re-checks.
    pub fn region_defer(&mut self, func_id: u32, entry_ip: u32) {
        if let Some(c) = self.region_counts.get_mut(&(func_id, entry_ip)) {
            *c -= 1;
        }
    }

    /// Attempt to compile the loop region `[start, end]` of `func_id` (entered at
    /// `start`). `globals_base_helper` is the address of the win64 helper that
    /// returns `vm.globals.as_mut_ptr()` (the region pins it for direct global
    /// access). On failure the region is blacklisted and never retried.
    #[allow(clippy::too_many_arguments)]
    pub fn compile_region(
        &mut self,
        func_id: u32,
        proto: &FuncProto,
        start: u32,
        end: u32,
        globals: &[Value],
        heap: &crate::heap::Heap,
        globals_base_helper: usize,
        heap_helpers: HeapHelperAddrs,
        field_pool_base: u32,
        field_pool_size: u32,
        const_strs: &FxHashMap<u32, u64>,
        ta_plan: &TaPinPlan,
        leaf_plan: &FxHashMap<usize, LeafInlinePlan>,
        method_plan: &FxHashMap<usize, MethodInlinePlan>,
    ) {
        let key = (func_id, start);
        if self.regions.contains_key(&key) || self.region_blacklist.contains(&key) {
            return;
        }
        let meter = self.meter;

        // ── object scalar-replacement (SROA) ── if the region's heap ops all
        // target one non-escaping global object, rewrite them to scratch
        // field-globals and compile the (now purely numeric) region — the loop
        // becomes register-only, like V8. Tried FIRST (beats the IC mem path).
        if !self.region_int_blacklist.contains(&key) {
            if let Some(fp) = plan_field_promotion(proto, start, end, globals, heap) {
                if (fp.fields.len() as u32) <= field_pool_size {
                    let sync_fields: Vec<(u32, u32)> = fp
                        .fields
                        .iter()
                        .enumerate()
                        .map(|(i, &name)| (name, field_pool_base + i as u32))
                        .collect();
                    let rewritten = rewrite_for_field_promotion(proto, start, end, &fp, field_pool_base);
                    let compiled = compile_region_numeric(&rewritten, start, end, globals_base_helper, meter);
                    if std::env::var_os("ZIPP_JITLOG").is_some() {
                        eprintln!(
                            "[jit] SROA region fn{func_id} [{start},{end}] fields={} -> {}",
                            fp.fields.len(),
                            if compiled.is_some() { "compiled" } else { "DECLINED (numeric path)" }
                        );
                    }
                    if let Some((code, is_int)) = compiled {
                        let plan = FieldSyncPlan {
                            obj_global: fp.obj_global,
                            fields: sync_fields,
                            func_id,
                            obj_idx: fp.obj_idx,
                            obj_version: fp.obj_version,
                        };
                        self.regions.insert(
                            key,
                            Region { code, start, end, deopts: 0, ok_runs: 0, is_int, is_mem: false, field_plan: Some(plan) },
                        );
                        return;
                    }
                }
            }
        }

        // Prefer the integer path (i64/paddq — beats the double path on integer
        // loops) unless it already deoptimised for this loop. Fall back to the
        // double/memory path.
        if !self.region_int_blacklist.contains(&key) {
            if let Some(code) =
                compile_region_int(proto, start, end, globals_base_helper, ta_plan, heap_helpers.ta_snapshot, meter)
            {
                if std::env::var_os("ZIPP_JITLOG").is_some() {
                    eprintln!("[jit] INT region fn{func_id} [{start},{end}] compiled");
                }
                self.regions
                    .insert(key, Region { code, start, end, deopts: 0, ok_runs: 0, is_int: true, is_mem: false, field_plan: None });
                return;
            }
        }
        // Reserve one inline-cache slot per GetProp/SetProp in the region (only
        // heap regions take the mem path that uses them; numeric regions have 0).
        let n_sites = proto.code[start as usize..=end as usize]
            .iter()
            .filter(|i| matches!(i, Instr::GetProp { .. } | Instr::SetProp { .. }))
            .count();
        let ic_base_idx = self.reserve_ic_sites(n_sites);
        let helpers = heap_helpers.to_heap_helpers(func_id, ic_base_idx);
        match compile_region(proto, start, end, globals_base_helper, helpers, const_strs, ta_plan, leaf_plan, method_plan, meter) {
            Some((code, is_mem)) => {
                if std::env::var_os("ZIPP_JITLOG").is_some() {
                    let tier = if is_mem { "MEM" } else { "DOUBLE" };
                    eprintln!("[jit] {tier} region fn{func_id} [{start},{end}] compiled");
                }
                self.regions
                    .insert(key, Region { code, start, end, deopts: 0, ok_runs: 0, is_int: false, is_mem, field_plan: None });
            }
            None => {
                if std::env::var_os("ZIPP_JITLOG").is_some() {
                    eprintln!("[jit] region fn{func_id} [{start},{end}] DECLINED (blacklisted)");
                }
                self.region_blacklist.insert(key);
            }
        }
    }

    /// Record that a region run resumed at `resume_ip`. If that ip is inside the
    /// region (a deopt/bail) and the region has now deopted past the limit, evict
    /// and blacklist it (so the interpreter stops re-entering a guard that keeps
    /// failing). Returns whether the region remains installed.
    pub fn note_region_resume(&mut self, func_id: u32, entry_ip: u32, resume_ip: u32) {
        let key = (func_id, entry_ip);
        let (evict, retry) = if let Some(r) = self.regions.get_mut(&key) {
            if resume_ip >= r.start && resume_ip <= r.end {
                r.deopts += 1;
                if std::env::var_os("ZIPP_JITLOG").is_some() {
                    eprintln!("[jit] region fn{} [{}] deopt at ip {}", key.0, key.1, resume_ip);
                }
                // Retry on a SIMPLER path if this was an int region (value grew
                // past 2^53 → double handles it) or a SROA region (a field turned
                // non-numeric → the inline-cache mem path handles any type).
                (r.deopts >= OSR_DEOPT_LIMIT, r.is_int || r.field_plan.is_some())
            } else {
                // A clean exit: the region reached its loop exit instead of
                // bailing. DECAY the deopt budget.
                //
                // `deopts` used to be a lifetime total, so four rare guard
                // misses spread over an entire program permanently blacklisted
                // a loop — measured as a 4.5x cliff (4.00 ns/op -> 18.00 ns/op)
                // triggered by a 0.00003% event rate, and three of the ten
                // benches were hitting it. Decaying on success turns it into
                // "four bails without DEOPT_DECAY_RUNS clean exits between
                // them", so a region that genuinely misbehaves is still evicted
                // just as fast (it never earns a decay), while a hot loop with
                // an occasional cold guard keeps its native code.
                r.ok_runs += 1;
                if r.ok_runs >= DEOPT_DECAY_RUNS {
                    r.ok_runs = 0;
                    r.deopts = r.deopts.saturating_sub(1);
                }
                (false, false)
            }
        } else {
            (false, false)
        };
        if evict {
            if std::env::var_os("ZIPP_JITLOG").is_some() {
                eprintln!("[jit] region fn{} [{}] EVICTED (retry={retry})", key.0, key.1);
            }
            // Park, don't drop: an outer activation of this region may still be
            // running (a call helper re-entered the interpreter, which looped
            // back into the region and deopted it) — see `retired`.
            if let Some(r) = self.regions.remove(&key) {
                self.retired.push(r);
            }
            if retry {
                // Don't blacklist the loop — let it recompile on a more general
                // path (region_int_blacklist also gates the SROA + int attempts).
                // Reset the back-edge counter so iterations re-trigger compile.
                self.region_int_blacklist.insert(key);
                self.region_counts.remove(&key);
            } else {
                self.region_blacklist.insert(key);
            }
        }
    }

    /// Reserve `n` fresh inline-cache sites (one per heap-op site in a region;
    /// `JIT_IC_WAYS` ways each), returning the base global site id. The ways
    /// start empty (`obj_bits == 0` ⇒ always miss on first use).
    pub fn reserve_ic_sites(&mut self, n: usize) -> u32 {
        let _prof = crate::vm::prof::enter(crate::vm::prof::Phase::JitCompile);
        let base = (self.ic_table.len() / JIT_IC_WAYS) as u32;
        self.ic_table
            .resize(self.ic_table.len() + n * JIT_IC_WAYS, IcEntry::default());
        self.ic_rot.resize(self.ic_rot.len() + n, 0);
        base
    }

    /// Base pointer of the inline-cache table (for a region prologue to pin).
    pub fn ic_base_ptr(&self) -> *const IcEntry {
        self.ic_table.as_ptr()
    }

    /// Fill one way of inline-cache site `site` after a miss (called by the
    /// `*_miss` helpers): an existing way for the same receiver identity is
    /// updated in place, else the next empty way, else round-robin eviction.
    /// Never grows the table (the site was reserved at compile time), so a
    /// pinned base pointer in a running region stays valid. `site == u32::MAX`
    /// (the hoisted-`.length` pseudo-site) is ignored.
    /// Has this site evicted a full round of ways?
    ///
    /// `ic_rot` only advances when every way is occupied and one has to be
    /// thrown out, so a non-trivial count means the site has seen more distinct
    /// RECEIVERS than it has ways — the megamorphic-by-identity case. Callers
    /// use it to stop refilling ways that will be evicted before they are hit.
    #[inline]
    pub fn ic_thrashing(&self, site: u32) -> bool {
        self.ic_rot.get(site as usize).is_some_and(|&r| r >= JIT_IC_WAYS as u8)
    }

    pub fn set_ic(&mut self, site: u32, e: IcEntry) {
        let base = (site as usize).wrapping_mul(JIT_IC_WAYS);
        let Some(ways) = self.ic_table.get_mut(base..base + JIT_IC_WAYS) else {
            return;
        };
        if let Some(w) = ways.iter_mut().find(|w| w.obj_bits == e.obj_bits) {
            *w = e;
            return;
        }
        if let Some(w) = ways.iter_mut().find(|w| w.obj_bits == 0) {
            *w = e;
            return;
        }
        let r = &mut self.ic_rot[site as usize];
        ways[*r as usize % JIT_IC_WAYS] = e;
        *r = r.wrapping_add(1);
    }
}

impl Region {
    /// Raw native entry pointer (into the region's mmap'd `ExecutableBuffer`,
    /// which never moves — stable across `regions`-map rehashes and, via
    /// `Jit::retired`, even across an eviction of a still-running region).
    /// The caller invokes it with the `JitFn::run` ABI; the returned bail slot
    /// is the ip to resume interpreting at.
    pub fn entry(&self) -> *const u8 {
        self.code.entry()
    }

    /// The object scalar-replacement sync plan, if this region was field-promoted.
    /// The interpreter syncs the object's fields ↔ the pool globals around `run`.
    pub fn field_plan(&self) -> Option<&FieldSyncPlan> {
        self.field_plan.as_ref()
    }

    /// True if this region runs on the memory-backed path. Diagnostic only.
    pub fn is_mem(&self) -> bool {
        self.is_mem
    }
}

// submodules (split out of the former monolithic codegen.rs)
mod fn_int;
pub(crate) mod meter;
mod self_call;
mod kernels;
mod region_admit;
mod plan;
mod absint;
mod plan_region;
mod regalloc;
mod region_int;
mod emit;
mod inline;
mod region_mem;
mod proto_mem;
mod emit_misc;

pub(crate) use fn_int::*;
pub(crate) use self_call::*;
pub(crate) use kernels::*;
pub(crate) use region_admit::*;
pub(crate) use plan::*;
pub(crate) use absint::*;
pub(crate) use plan_region::*;
pub(crate) use regalloc::*;
pub(crate) use region_int::*;
pub(crate) use emit::*;
pub(crate) use inline::*;
pub(crate) use region_mem::*;
pub(crate) use proto_mem::*;
pub(crate) use emit_misc::*;
