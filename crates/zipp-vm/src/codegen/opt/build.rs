//! Tier O's IR and its construction from bytecode.
//!
//! Steps, over the function's reachable blocks in reverse postorder:
//! bytecode liveness per position (frame states name only live registers,
//! phis exist only for live ones), a forward dataflow fixing each live
//! register's representation at every block entry (so a loop's phis know
//! theirs before the loop body is built), then SSA construction: each
//! bytecode op becomes IR values, its operands converted to the
//! representation it computes in, every conversion that can fail a guard
//! with the frame state of the op's own position. A multi-predecessor
//! block gets a phi per live register; each predecessor converts its value
//! to the phi's representation (only widenings: i32 to f64, anything to a
//! boxed value, which cannot fail) before its terminator.
//!
//! A call whose site has one plain user-function callee (`Site`) is
//! inlined when the callee is straight-line code Tier O admits, behind a
//! guard on the callee's function id (and, for a method, the receiver's
//! shape): every guard of the inlined body resumes the interpreter at the
//! call itself, which runs it again whole (the body has no effect to
//! repeat). Any other call is a call-out through the interpreter cache's
//! native helper, with the frame state written into the window first: the
//! callee's arguments, the collector's roots, and where the interpreter
//! goes on if the call throws or declines.

use rustc_hash::FxHashMap;

use crate::bytecode::{BitwiseOp, FuncProto, Instr, MathFn};
use crate::value::Value;

use super::{MathIntrinsicGuard, PropShapePlan};

/// An SSA value: an index into `Func::insts`.
pub(crate) type V = u32;
/// A block: an index into `Func::blocks` (reverse postorder; 0 the entry).
pub(crate) type B = u32;

/// A value's representation (`Void`: a guard with no value).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Rep {
    I32,
    F64,
    Bool,
    Tagged,
    Void,
    /// A raw machine word (an object's values pointer, `ShapeVals`): never
    /// boxed, never in a frame state or a phi.
    Raw,
}

impl Rep {
    /// The representation that holds both: i32 widens to f64, anything else
    /// mixed is boxed.
    fn join(a: Rep, b: Rep) -> Rep {
        match (a, b) {
            (x, y) if x == y => x,
            (Rep::I32, Rep::F64) | (Rep::F64, Rep::I32) => Rep::F64,
            _ => Rep::Tagged,
        }
    }
}

/// A Number comparison.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Cmp {
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
}

/// A call site's one plain user-function callee, from the cross-call plan.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Site {
    Call { fid: u32 },
    /// An own data method: the receiver's shape and the slot holding it.
    Method { shape: u32, slot: u32, fid: u32 },
    /// A method of the receiver's class (`ClassReadLane`): the receiver's
    /// shape (no own property of the key), its class and that class's
    /// version, and the class-prototype epoch fix which method it is.
    ClassMethod {
        recv_shape: u32,
        class: u32,
        ver: u32,
        epoch: u32,
        fid: u32,
    },
    /// A `GetProp`'s getter on the receiver's class, with the guards of
    /// `ClassMethod`.
    ClassGetter {
        recv_shape: u32,
        class: u32,
        ver: u32,
        epoch: u32,
        fid: u32,
    },
}

impl Site {
    pub(crate) fn fid(self) -> u32 {
        match self {
            Site::Call { fid }
            | Site::Method { fid, .. }
            | Site::ClassMethod { fid, .. }
            | Site::ClassGetter { fid, .. } => fid,
        }
    }

    fn method(self) -> bool {
        !matches!(self, Site::Call { .. })
    }
}

/// What a compile knows besides the function: its property and call sites,
/// and the bodies of the callees it may inline (each with its own
/// property plan).
pub(crate) struct Inputs<'a> {
    pub(crate) prop_shape: &'a PropShapePlan,
    pub(crate) sites: &'a FxHashMap<usize, Site>,
    pub(crate) callees: &'a FxHashMap<u32, super::OptCallee<'a>>,
    /// The function's string constants, as the shared strings Tier C loads.
    pub(crate) const_strs: &'a FxHashMap<u32, u64>,
    /// The intrinsic `Math` and its methods, as compiled code proves them.
    pub(crate) math: Option<MathIntrinsicGuard>,
    /// The sites earlier bodies of the function deopted at
    /// (`FrameState::why`): built here without the speculation that failed.
    pub(crate) generic: Option<&'a rustc_hash::FxHashSet<u32>>,
    /// The `New` sites with Tier C's allocation lane (`CrossSite::new_lane`).
    pub(crate) new_lanes: &'a FxHashMap<usize, &'static super::super::NewLanePlan>,
    /// `OptCallees::class_values`, `super_parents`, and `class_epoch`.
    pub(crate) class_values: &'a FxHashMap<u32, u64>,
    pub(crate) super_parents: &'a FxHashMap<u32, (u32, u64)>,
    pub(crate) class_epoch: u32,
    /// `OptCallees::poly`.
    pub(crate) poly: &'a FxHashMap<usize, Vec<(u32, u32)>>,
    /// Constant own-data primitive properties on `const_watched_objs` (`ip -> (oidx, ver, bits)`).
    pub(crate) const_props: &'a FxHashMap<usize, (u32, u32, u64)>,
    /// Constant enum-object switch jump-table plans keyed by chain start ip.
    pub(crate) enum_switches: &'a FxHashMap<usize, super::super::EnumSwitchPlan>,
    /// Global slots not directly routable at compile time.
    pub(crate) slow_global_loads: &'a rustc_hash::FxHashSet<u32>,
    /// Tier C per-IP arithmetic type feedback (`ARITH_FB_DOUBLE` / `ARITH_FB_OTHER`).
    pub(crate) arith_fb: Option<&'a [u8]>,
    /// Statically proven plain closure plans (`CallableMakePlan::plain_closures`).
    pub(crate) plain_closures: &'a FxHashMap<usize, super::super::PlainClosurePlan>,
    /// Helper addresses + compile-time `atoms` snapshot (`HeapHelpers::is_atom_bits`).
    pub(crate) heap: &'a super::super::HeapHelpers,
}

/// The registers of a call Tier C's emitted cross-call lanes make ahead of
/// the call-out's helper (`Func::cross` holds their plans).
#[derive(Clone, Copy, Debug)]
pub(crate) enum CallLane {
    None,
    /// `Call` (no `this`) or `CallWithThis`: the site's arms.
    Plain {
        callee: u16,
        this: Option<u16>,
        arg_base: u16,
        dst: u16,
    },
    /// `CallMethod`: a dedicated helper for its name (`intr`), then the
    /// own-method lane, class-method arms, and inline megamorphic lookup.
    Method {
        obj: u16,
        obj_v: V,
        arg_base: u16,
        arg0_v: Option<V>,
        dst: u16,
        intr: Option<MethodIntr>,
        mega_ok: bool,
    },
    /// `GetProp` read by its slow helper: Tier C's inline proto/class/cache
    /// fast paths first (`Func::ic_sites`), receivers of any kind, chains
    /// included. `obj_v` is flushed to `[rbx + 8*obj]` before the probe; full
    /// frame state is deferred until probe decline / getter invocation.
    Prop {
        obj: u16,
        obj_v: V,
        dst: u16,
        name: u32,
        is_call: bool,
        is_apply: bool,
        str_char_code_at: bool,
        str_slot_ptr: u64,
        arr_slot_ptr: u64,
        kind_idx: Option<u8>,
    },
    /// `SetProp` written via Tier C's inline cache probe + `mega_set_prop_stub`
    /// first (`Func::ic_sites`), falling back to `jit_set_prop_slow`.
    /// `obj_v`/`val_v` are flushed before the probe; full frame state is
    /// deferred until probe decline.
    SetProp {
        obj: u16,
        obj_v: V,
        val: u16,
        val_v: V,
        name: u32,
    },
}

/// A method a `CallMethod` makes through one of Tier C's dedicated pure
/// helpers by its name and arity, before the general route: the helper
/// declines, before any effect, a receiver it is not for (a user `push`).
#[derive(Clone, Copy, Debug)]
pub(crate) enum MethodIntr {
    CharCodeAt,
    IndexOf,
    Push,
    /// `substring` / `slice`, the helper's mode bits.
    Substring(i32),
    Get,
    Has,
    /// Map/Set `set`, `clear`, `add`, `delete`: the helper's op, and argc.
    Mutate(i32, u16),
    Upper,
    Pop,
    /// Dedicated 1-arg string helper (`charAt`, `at`, etc.) via `str_unit_helper`.
    StrUnit(u64),
    /// String/Array builtin via `jit_str_method` (`builtin_lane_packed`).
    StrArrBuiltin(u32),
    /// Dense Array mutator via `jit_array_mutate` (`shift`/`unshift`/`splice`/`reverse`/`fill`).
    ArrMutate(u8, u16),
    /// `fn.call(thisArg, ...)` via `fn_call_dyn_stub` -> `cross3_dyn_stub`.
    FnCall(u16),
    /// `fn.apply(thisArg, argArray)` via `fn_apply_arr_stub`/`fn_apply_dyn_stub` -> `cross3_dyn_stub`.
    FnApply(u16),
}

impl MethodIntr {
    /// True when the helper never allocates, triggers GC, throws, or runs JS,
    /// so only `obj` and `arg0` need to be flushed to the window before calling
    /// it and full `write_state` can be deferred until decline.
    #[inline]
    pub(crate) fn is_pure_no_gc(self) -> bool {
        matches!(
            self,
            MethodIntr::CharCodeAt
                | MethodIntr::IndexOf
                | MethodIntr::Get
                | MethodIntr::Has
                | MethodIntr::Pop
        )
    }
}

/// `MethodIntr` for a method `key` called with `argc` arguments, as Tier C
/// picks its lanes (`proto_mem`'s `CallMethod`).
fn method_intr(key: &str, argc: u16) -> Option<MethodIntr> {
    let one_arg_substring = super::super::substring1_intrinsic_enabled();
    if let Some(m) = match (key, argc) {
        ("charCodeAt", 1) => Some(MethodIntr::CharCodeAt),
        ("indexOf", 1) => Some(MethodIntr::IndexOf),
        ("push", 1) => Some(MethodIntr::Push),
        ("substring" | "slice", 2) | ("substring" | "slice", 1) if argc == 2 || one_arg_substring => {
            Some(MethodIntr::Substring((key == "slice") as i32 | (((argc == 1) as i32) << 1)))
        }
        ("get", 1) => Some(MethodIntr::Get),
        ("has", 1) => Some(MethodIntr::Has),
        ("set", 2) | ("clear", 0) | ("add", 1) | ("delete", 1)
            if super::super::proto_mem::tierc_coll_mutate_enabled() =>
        {
            let op = match key {
                "set" => 0,
                "clear" => 1,
                "add" => 2,
                _ => 3,
            };
            Some(MethodIntr::Mutate(op, argc))
        }
        ("toUpperCase", 0) if super::super::proto_mem::tierc_string_upper_enabled() => Some(MethodIntr::Upper),
        ("pop", 0) => Some(MethodIntr::Pop),
        ("call", n) if n <= 256 => Some(MethodIntr::FnCall(n)),
        ("apply", n) if n <= 256 => Some(MethodIntr::FnApply(n)),
        _ => None,
    } {
        return Some(m);
    }
    if argc == 1 {
        if let Some(h) = crate::vm::str_unit_helper(key) {
            return Some(MethodIntr::StrUnit(h as u64));
        }
    }
    if let Some(packed) = crate::vm::builtin_lane_packed(key, argc) {
        return Some(MethodIntr::StrArrBuiltin(packed));
    }
    if let Some(op) = crate::vm::arr_mutate_op(key, argc) {
        return Some(MethodIntr::ArrMutate(op, argc));
    }
    None
}

/// A helper's address, for a call-out.
fn helper(f: *const ()) -> u64 {
    f as u64
}

#[derive(Clone, Debug)]
pub(crate) enum Op {
    ConstI32(i32),
    ConstF64(f64),
    ConstBool(bool),
    ConstTagged(u64),
    /// The window's register at entry: `this` or a parameter.
    Param(u16),
    /// Inputs in the block's `preds` order.
    Phi(Vec<V>),
    I32ToF64(V),
    /// A Number unboxed (an Int converted); anything else fails the guard.
    TaggedToF64(V),
    /// ToInt32 of a double; NaN, the infinities and |x| >= 2^63 fail.
    F64ToI32(V),
    /// ToInt32 of a boxed Number (an Int as it is), as `F64ToI32`;
    /// anything but a Number fails.
    TaggedToI32(V),
    /// `a >>> k` for a constant `k` in 1..=31, which always fits an i32.
    UshrI(V, u8),
    /// Any representation boxed (a double by `Value::num`'s rule).
    Box(V),
    /// A boxed value's truthiness (a heap value's through `jit_truthy`).
    TaggedTruthy(V),
    F64Truthy(V),
    I32Truthy(V),
    AddF(V, V),
    SubF(V, V),
    /// `a + b` / `a - b` / `a + k` on i32s; an overflow fails, unless the
    /// result is observed only through ToInt32 (`wrap`, Tier C's
    /// `trunc_only_arith_ips` proof), where it wraps.
    AddI(V, V, bool),
    SubI(V, V, bool),
    AddIK(V, i32, bool),
    MulF(V, V),
    DivF(V, V),
    NegF(V),
    /// `&`, `|`, `^`, `<<`, `>>` on i32s.
    Bit(BitwiseOp, V, V),
    /// `a >>> b`, a Number.
    UshrF(V, V),
    CmpF(Cmp, V, V),
    /// A compare of two i32s.
    CmpI(Cmp, V, V),
    /// `===` (true) or `!==` of two booleans.
    CmpB(bool, V, V),
    NotB(V),
    /// `===` (true) or `!==` of two boxed values; two different heap values
    /// (strings may be equal) fail.
    #[allow(dead_code)]
    StrictEqT(bool, V, V),
    /// An own data property read behind the receiver's shape; anything else
    /// fails, or with `slow` (a site that deopted before: its position and
    /// `jit_get_prop_slow`'s operand) takes the `GetProp` as a call-out.
    GetPropShape {
        obj: V,
        shape: u32,
        slot: u32,
        slow: Option<(u32, u64)>,
    },
    /// An object literal of a known plan and shape (Tier C's baked form):
    /// a ready object filled from the window the frame state fills
    /// (`emit_ready_literal_of`), else `jit_finalize_object_thin`.
    Literal {
        plan_ptr: u64,
        shape: u32,
        val_base: u16,
        count: u16,
        dst: u16,
        ip: u32,
    },
    /// An array or object literal at `ip` through its allocating helper
    /// (`jit_new_array`, `jit_finalize_object`: `vm`, window, `a`, `b`),
    /// which reads its values from the window the frame state fills.
    AllocOut { helper: u64, ip: u32, a: u64, b: u64 },
    /// `obj.length` with no shape plan: a dense Array's through its mirror,
    /// else a string's or an Array's through `quick_len`, else the `GetProp`
    /// at `ip` as a call-out.
    Length { obj: V, ip: u32, packed: u64 },
    /// Register-based flat-ASCII `str.charCodeAt(idx)`: inline zero-FFI byte
    /// load via `hot_mirror`/`upvals_mirror`/`this_mirror`, cold pure helper
    /// `jit_char_code_at` (ropes/UTF-16/OOB -> NaN), deopt on non-string /
    /// negative index / overridden `%String.prototype%.charCodeAt`.
    CharCodeAt { obj: V, idx: V },
    /// An own data property read for receivers of several shapes (the
    /// cache's ways, `ways`), else the `GetProp` at `ip` as a call-out
    /// (`jit_get_prop_slow`, `packed`) when `slow` is `Some`, or a deopt when
    /// `slow` is `None` (inlined callee).
    GetPropPoly {
        obj: V,
        ways: Vec<(u32, u32)>,
        slow: Option<(u32, u64)>,
    },
    /// `a + b`, the whole operator (`jit_concat`: strings, coercions), with
    /// the frame state written first; a boxed result.
    AddT(V, V),
    /// `===` (true) or `!==` of two boxed values, two heap values compared
    /// by `jit_strict_eq`.
    StrictEqH(bool, V, V),
    /// Exact 64-bit identity compare against a non-number, non-heap constant
    /// (`undefined`, `null`, `true`, `false`): never calls a stub.
    StrictEqBits(bool, V, u64),
    /// Pure boolean test: true iff `v` is NOT a heap value OR its `Heap::atoms`
    /// bit is set (so identity comparison against any interned string atom is
    /// exact). CSE'd across `node.type === "..."` switch chains.
    IsAtomOrPrim(V),
    /// `a === atom_bits` (or `!==`) against a compile-time interned string atom,
    /// using precomputed `atom_ok = IsAtomOrPrim(a)` to skip `eq_stub` in 5
    /// register-only instructions whenever `a` is an atom or non-heap primitive.
    StrictEqAtom {
        eq: bool,
        a: V,
        atom_bits: u64,
        atom_ok: V,
    },
    /// A relational compare of two boxed values: Numbers inline, two
    /// strings through `jit_str_relational`; anything else fails.
    CmpT(Cmp, V, V),
    /// `<upvalue idx> = v`; a const or TDZ cell fails.
    UpvalSet(u16, V),
    /// A class value fixed while `Vm::mi_class_epoch` is `epoch`; a class
    /// evaluated since fails.
    ClassConst { bits: u64, epoch: u32 },
    /// `obj[key]` (`key` an i32, a double or boxed): a present element of a
    /// dense Array through its mirror, else `keyed_get_stub` / `jit_get_index`,
    /// else (when `!inlined`) the whole `GetIndex` at `ip` as a call-out
    /// (`jit_opt_index_full`, `packed`), or (when `inlined`) a deopt to `call_fs`.
    /// `ta`: the typed-array record the op's receiver register keeps
    /// (`Func::ta_records`), tried after the mirror.
    GetIndex {
        obj: V,
        key: V,
        ip: u32,
        packed: u64,
        ta: Option<u8>,
        inlined: bool,
    },
    /// `obj[key] = val`, as `GetIndex`: a present element overwritten, else
    /// `jit_set_index`, else the whole `SetIndex`.
    SetIndex {
        obj: V,
        key: V,
        val: V,
        ip: u32,
        packed: u64,
        ta: Option<u8>,
    },
    /// `new` at `ip` (registers `callee`, `arg_base`, `dst`) whose
    /// constructor only stores its arguments: Tier C's allocation lane over
    /// the window the frame state fills, else `jit_new_ic` as a call-out.
    NewLane {
        lane: &'static super::super::NewLanePlan,
        callee: u16,
        arg_base: u16,
        dst: u16,
        ip: u32,
        packed: u64,
        argc: u16,
    },
    /// `obj`'s values pointer behind its shape (`Raw`); another shape fails.
    /// Made once per object and block (`Builder::shape_memo`) while nothing
    /// that may reshape it runs (`Op::may_reshape`).
    ShapeVals { obj: V, shape: u32 },
    /// The value in `slot` of a values pointer.
    LoadSlot { vals: V, slot: u32 },
    /// `val` into `slot` of `obj`'s values pointer, the nursery barrier
    /// after a heap value.
    StoreSlot { vals: V, obj: V, val: V, slot: u32 },
    /// `GetIterator` / `IterPrime` through their pure helpers (`vm`, the
    /// value); a decline deopts.
    IterPure { helper: u64, src: V },
    /// `IterNext` at `ip` over the window the frame state fills: a dense
    /// Array's step through its mirror, else `jit_iter_next` (`packed`,
    /// the cursor register `idx`), which write the value, done and cursor
    /// registers there (`WindowLoad` reads them back).
    IterStep {
        ip: u32,
        packed: u64,
        value_dst: u16,
        done_dst: u16,
        iter: u16,
        idx: u16,
    },
    /// The window's register, as a call-out left it.
    WindowLoad(u16),
    /// A total helper of the frame's handler stack (`jit_push_finally`,
    /// `jit_push_catch`, `jit_pop_finally`), `vm` and `packed`.
    Handler { helper: u64, packed: u64 },
    /// `EndFinally` at `ip` with the frame state (every register) written:
    /// a normal completion goes on; a `break` routes on and leaves the body
    /// at the ip `jit_end_finally_route` names; a return or throw leaves it
    /// at `ip` for the interpreter.
    EndFinally { kind: u16, val: u16, ip: u32 },
    /// `IterCloseFinally` at `ip`: a normal or jump completion closes
    /// nothing; any other leaves the body at `ip`.
    IterCloseFin { kind: u16, ip: u32 },
    /// The way out of a body with a `finally` or `catch` to route through
    /// (every register written): `JumpFinally` routing (`target`, `floor`),
    /// `Throw` of `v`, or a return of `v` that a `finally` intercepts
    /// (`jit_return_finally`; none, and the return goes on).
    JumpFinally { target: u32, floor: u16 },
    Throw { v: V, ip: u32 },
    ReturnFinally { v: V },
    /// A global's slot; a binding in its TDZ fails.
    LoadGlobal(u32),
    /// Non-directly-routable `LoadGlobal` via `jit_load_global_slow`; plain bail on decline.
    LoadGlobalSlow { idx: u32, ip: u32 },
    /// Non-directly-routable `LoadGlobalOrUndefined` via `jit_load_global_or_undef`; plain bail on decline.
    LoadGlobalOrUndefSlow { idx: u32, ip: u32 },
    /// Guard that `obj` is `Value::heap(oidx)` and `Heap::versions[oidx] == ver`
    /// (for objects in `const_watched_objs`); else deopts.
    GuardConstObj { obj: V, oidx: u32, ver: u32 },
    /// `v` is a function of id `fid`; anything else fails.
    GuardFid { v: V, fid: u32 },
    /// `obj` is an instance of `class` of shape `recv_shape`, the class at
    /// version `ver` and the class prototypes at `epoch`; else fails.
    GuardClass {
        obj: V,
        recv_shape: u32,
        class: u32,
        ver: u32,
        epoch: u32,
    },
    /// The op at `ip` through a native helper of the call protocol (`vm`,
    /// window, `(func_id << 32) | ip`, `packed`, `argc`), the frame state
    /// written first (`Inst::fs`): a call, `new`, a class value, a property
    /// access the caches do not answer here. Its result boxed (`Void`: none).
    CallOut {
        helper: u64,
        ip: u32,
        packed: u64,
        argc: u16,
        lane: CallLane,
    },
    /// `obj.<key> = val` into the own data slot the receiver's shape has, the
    /// nursery barrier after a heap value; any other receiver takes the
    /// `SetProp` at `ip` through `jit_set_prop_slow` (`packed`), as a
    /// call-out.
    SetPropShape {
        obj: V,
        val: V,
        shape: u32,
        slot: u32,
        ip: u32,
        packed: u64,
    },
    /// The running closure's captured variable; one in its TDZ, or no
    /// activation to read it from, fails.
    UpvalGet(u16),
    /// `globals[idx] = v`; a routed global or one in its TDZ fails.
    StoreGlobal(u32, V),
    /// `Math`'s method `op` is the intrinsic: through `recv` (the receiver a
    /// `Math.<op>` read was made on) or the global `gidx` (a bare `MathOp`);
    /// anything else fails.
    MathGuard { recv: Option<V>, gidx: u32, op: MathFn },
    /// `v` is `Math`'s intrinsic `op`; anything else fails.
    GuardCallee { v: V, op: MathFn },
    /// `Math.<op>(x)` in one instruction (`sqrt`, `abs`, `floor`, `ceil`,
    /// `trunc`).
    MathF1(MathFn, V),
    /// `Math.<op>` through its pure helpers.
    MathCall1(MathFn, V),
    MathCall2(MathFn, V, V),
    /// `Math.imul` on ToInt32s.
    ImulI(V, V),
    /// `a % b` on Numbers (fmod).
    ModF(V, V),
    /// `~a` on i32.
    BitNotI(V),
    /// `a == b` / `a != b` via per-function `emit_loose_eq_stub`; bail/deopt on decline.
    LooseEq { is_ne: bool, a: V, b: V, ip: u32, inlined: bool },
    /// `a == null` / `a != null` / `a == undefined` / `a != undefined` inline
    /// (zero calls, preserves `shape_memo`, branch-fused into `Term::Branch`).
    IsNullish { is_ne: bool, a: V, ip: u32, inlined: bool },
    /// `Array.isArray(val)` via per-function `emit_is_array_stub`; bail/deopt on decline.
    IsArray { val: V, callee: V, this_v: V, ip: u32, inlined: bool },
    /// `val instanceof ctor`: probe `emit_instanceof_pair_stub`, else `jit_instanceof_ic`.
    InstanceOf { val: V, ctor: V, ip: u32, packed: u64 },
    /// `typeof a` via `heap.typeof_str` (allocates; frame state written first).
    TypeOf(V),
    /// Fused `typeof a === <code_str>` (`neg` flips result); pure.
    TypeOfIs { a: V, code: u8, neg: bool },
    /// Fused `typeof a === typeof b` (`neg` flips result); pure.
    TypeOfSame { a: V, b: V, neg: bool },
    /// `LoadGlobalOrUndefined`: global slot, or `undefined` if UNINITIALIZED.
    LoadGlobalOrUndef(u32),
    /// Active callee value via `jit_load_callee`; plain bail on decline.
    LoadCallee { ip: u32 },
    /// `TailCall` depth check: plain bail at `ip` when `call_depth >= native_call_cap`.
    TailCallGuard { ip: u32 },
    /// `CheckCoercible`: plain bail (or deopt when `inlined`) when `src` is null or undefined.
    CheckCoercible { src: V, ip: u32, inlined: bool },
    /// Unconditional cold handoff to interpreter at `ip` (writes state, no deopt count).
    BailOnly { ip: u32 },
    /// `StrConcatChain` via `jit_concat_chain_fast` / `jit_concat_chain_suffix_fast`
    /// (frame state written first).
    ConcatChain {
        a: V,
        b: V,
        next_leaf: Option<u16>,
        hint: u32,
        ip: u32,
    },
    /// Inline `cell_vals_mirror[cell_idx]`; plain bail on UNINITIALIZED (TDZ).
    CellGet { cell: V, ip: u32 },
    /// Cell store: inline mirror + nursery barrier when `!checked`, else `jit_cell_set_tdz_checked`.
    CellSet { cell: V, src: V, checked: bool, ip: u32 },
    /// `MakeCell` / `MakeCellTdz` / `MakeCellFnName`: inline `ReadyCells` pop
    /// (when `!fn_name`), falling back to `jit_make_cell`.
    MakeCell {
        reg: u16,
        init: Option<V>,
        tdz: bool,
        fn_name: bool,
        ip: u32,
    },
    /// `MakeClosure` / `MakeArrow`: inline `ReadyClosures` pop when
    /// `Func::plain_closures` has a plan for `ip`, else `jit_make_closure`.
    MakeClosure {
        dst: u16,
        ip: u32,
        captures: Vec<(u16, V)>,
    },
    /// `ThisCheck`: inline zero-FFI check when `this_tdz` and `super_this` are
    /// both empty, else `jit_this_check_ic`.
    ThisCheck { src: V, ip: u32 },
    /// `SuperCtorFetch`: inline `proto_of[home_idx]` / baked `parent_bits` under
    /// `mi_class_epoch`, else `jit_super_ctor_fetch`.
    SuperCtorFetch {
        home_class_id: u32,
        baked: Option<(u32, u64)>,
        epoch: u32,
    },
    /// `ToPropKey`: inline non-nullish `obj` + primitive/flat-string `src` pass-through,
    /// else `jit_cold_op_ic` (cold_op 14).
    ToPropKey { obj: V, src: V, ip: u32 },
    /// `ToConcatKey`: inline primitive/flat-string `src` pass-through,
    /// else `jit_cold_op_ic` (cold_op 17).
    ToConcatKey { src: V, ip: u32 },
    /// `InitDataPropDyn`: `jit_init_data_prop_dyn` fast path, else `jit_cold_op_ic` (cold_op 15).
    InitDataPropDyn { obj_reg: u16, obj: V, key: V, val: V, ip: u32 },
    /// `MakeFunc` via `jit_make_func(vm, (func_id<<32)|ip)`.
    MakeFunc { ip: u32 },
    /// `NewObject` via `jit_new_object(vm, hint)`.
    NewObject { hint: u16, ip: u32 },
    /// `NewPlannedObject` via `jit_new_planned_object(vm, (func_id<<32)|plan)`.
    NewPlannedObject { plan: u32, ip: u32 },
    /// `AppendDataProp` via `jit_append_data_prop(vm, obj, (func_id<<32)|name, val)`.
    AppendDataProp { obj: V, name: u32, val: V, ip: u32 },
    /// `Object.keys(obj)` via `jit_object_keys`.
    ObjectKeys { obj: V, callee: V, this_v: V, ip: u32 },
    /// `key in obj` (non-brand) via `heap.has_property`; plain bail/deopt on decline.
    HasProp { key: V, obj: V, ip: u32, inlined: bool },
    /// `LenOf` via `heap.len_of` (pure, total).
    LenOf { obj: V },
    /// `ForInKeys` via `heap.forin_keys` (allocates; frame state written first).
    ForInKeys { obj: V, ip: u32 },
    /// `ForInLive` via `heap.forin_live` (may run Proxy trap; frame state written first).
    ForInLive { obj: V, key: V },
    /// `RegExpLitProbe` via `jit_regexp_lit_probe`; plain bail on decline.
    RegExpLitProbe { pattern: u32, flags: u32, ip: u32 },
    /// `RegExpMethod`: direct helper lane then `jit_call_with_this_ic`.
    RegExpMethod {
        op: crate::bytecode::RegExpMethod,
        callee: u16,
        this_v: u16,
        arg_base: u16,
        argc: u16,
        ip: u32,
        packed: u64,
    },
    /// `RegExpLitTest`: direct `jit_regexp_lit_test` then `jit_regexp_lit_test_ic`.
    RegExpLitTest {
        arg: u16,
        pattern: u32,
        flags: u32,
        ip: u32,
        packed: u64,
    },
    /// `IterToArray` probe (`ITER_PROBE`) via `jit_iter_probe` (pure).
    IterProbe,
}

impl Op {
    /// Whether this constant is rematerialized at its uses (`Loc::Imm`) instead
    /// of occupying a register or stack slot from function entry.
    #[inline(always)]
    pub(crate) fn is_imm(&self) -> bool {
        matches!(self, Op::ConstI32(_) | Op::ConstBool(_) | Op::ConstTagged(_))
    }

    /// Whether the op calls native code on its hot path, clobbering caller-saved
    /// allocator registers (`r8`, `r9`, `xmm4`, `xmm5`): a value live across it
    /// needs a callee-saved register.
    ///
    /// Excluded because their hot paths never clobber `r8`/`r9`/`xmm4`/`xmm5`
    /// and their out-of-line cold fallbacks save/restore those volatile regs
    /// into the frame's fixed `vol_save` slot:
    /// `StrictEqAtom`, `TypeOfIs`, `StoreSlot`, `CellSet { checked: false }`,
    /// `TaggedTruthy`, `GetPropShape`, `GetPropPoly`, `SetPropShape`, `Length`,
    /// `GetIndex`, `SetIndex`, `AddT` (non-`safe-sandbox`), `UpvalSet`, `ModF`,
    /// `CmpT`, `ThisCheck`, `SuperCtorFetch { baked: Some(_) }`, `ToPropKey`,
    /// `ToConcatKey`, and `CharCodeAt`.
    pub(crate) fn calls(&self) -> bool {
        #[cfg(feature = "safe-sandbox")]
        if matches!(self, Op::AddT(..)) {
            return true;
        }
        matches!(
            self,
            Op::CallOut { .. }
                | Op::MathCall1(..)
                | Op::MathCall2(..)
                | Op::StrictEqH(..)
                | Op::NewLane { .. }
                | Op::AllocOut { .. }
                | Op::Literal { .. }
                | Op::IterPure { .. }
                | Op::IterStep { .. }
                | Op::Handler { .. }
                | Op::EndFinally { .. }
                | Op::IterCloseFin { .. }
                | Op::JumpFinally { .. }
                | Op::Throw { .. }
                | Op::ReturnFinally { .. }
                | Op::LooseEq { .. }
                | Op::IsArray { .. }
                | Op::InstanceOf { .. }
                | Op::TypeOf(..)
                | Op::TypeOfSame { .. }
                | Op::LoadCallee { .. }
                | Op::LoadGlobalSlow { .. }
                | Op::LoadGlobalOrUndefSlow { .. }
                | Op::ConcatChain { .. }
                | Op::CellSet { checked: true, .. }
                | Op::MakeCell { .. }
                | Op::MakeClosure { .. }
                | Op::SuperCtorFetch { baked: None, .. }
                | Op::InitDataPropDyn { .. }
                | Op::MakeFunc { .. }
                | Op::NewObject { .. }
                | Op::NewPlannedObject { .. }
                | Op::AppendDataProp { .. }
                | Op::ObjectKeys { .. }
                | Op::HasProp { .. }
                | Op::LenOf { .. }
                | Op::ForInKeys { .. }
                | Op::ForInLive { .. }
                | Op::RegExpLitProbe { .. }
                | Op::RegExpMethod { .. }
                | Op::RegExpLitTest { .. }
                | Op::IterProbe
        )
    }

    /// Whether the op may run user code, allocate/GC, or mutate an object's
    /// shape/properties (beyond in-place `StoreSlot`), so shape/const-obj/global
    /// caches must be invalidated. Independent of `Op::calls()` so cold-only
    /// mutating ops stay soundly tracked even when their hot paths preserve
    /// volatile registers.
    pub(crate) fn may_reshape(&self) -> bool {
        matches!(
            self,
            Op::CallOut { .. }
                | Op::SetPropShape { .. }
                | Op::AddT(..)
                | Op::UpvalSet(..)
                | Op::GetPropShape { slow: Some(_), .. }
                | Op::GetPropPoly { slow: Some(_), .. }
                | Op::Length { .. }
                | Op::GetIndex { .. }
                | Op::SetIndex { .. }
                | Op::NewLane { .. }
                | Op::AllocOut { .. }
                | Op::Literal { .. }
                | Op::IterPure { .. }
                | Op::IterStep { .. }
                | Op::Handler { .. }
                | Op::EndFinally { .. }
                | Op::IterCloseFin { .. }
                | Op::JumpFinally { .. }
                | Op::Throw { .. }
                | Op::ReturnFinally { .. }
                | Op::InstanceOf { .. }
                | Op::TypeOf(..)
                | Op::ConcatChain { .. }
                | Op::MakeCell { .. }
                | Op::MakeClosure { .. }
                | Op::ToPropKey { .. }
                | Op::ToConcatKey { .. }
                | Op::InitDataPropDyn { .. }
                | Op::MakeFunc { .. }
                | Op::NewObject { .. }
                | Op::NewPlannedObject { .. }
                | Op::AppendDataProp { .. }
                | Op::ObjectKeys { .. }
                | Op::ForInKeys { .. }
                | Op::ForInLive { .. }
                | Op::RegExpMethod { .. }
                | Op::RegExpLitTest { .. }
        )
    }

    /// Whether the op writes its frame state after a hot-path native call (a
    /// pure helper first, then the call-out or a deopt): the state's values
    /// must live through that call in callee-saved registers.
    pub(crate) fn state_after_call(&self) -> bool {
        matches!(
            self,
            Op::IterPure { .. }
                | Op::CallOut {
                    lane: CallLane::Prop { .. } | CallLane::SetProp { .. },
                    ..
                }
        ) || matches!(
            self,
            Op::CallOut {
                lane: CallLane::Method { intr: Some(m), .. },
                ..
            } if m.is_pure_no_gc()
        ) || matches!(
            self,
            Op::LooseEq { .. }
                | Op::IsArray { .. }
                | Op::InstanceOf { .. }
                | Op::LoadCallee { .. }
                | Op::LoadGlobalSlow { .. }
                | Op::LoadGlobalOrUndefSlow { .. }
                | Op::CellSet { checked: true, .. }
                | Op::MakeCell { .. }
                | Op::MakeClosure { .. }
                | Op::HasProp { .. }
                | Op::RegExpLitProbe { .. }
        )
    }
}

impl Op {
    /// Visit each value this op reads without allocating (phi inputs are not
    /// visited: they are read at the end of each predecessor).
    #[inline]
    pub(crate) fn for_each_operand(&self, mut f: impl FnMut(V)) {
        match *self {
            Op::ConstI32(_)
            | Op::ConstF64(_)
            | Op::ConstBool(_)
            | Op::ConstTagged(_)
            | Op::Param(_)
            | Op::Phi(_)
            | Op::LoadGlobal(_)
            | Op::LoadGlobalOrUndef(_)
            | Op::LoadGlobalSlow { .. }
            | Op::LoadGlobalOrUndefSlow { .. }
            | Op::LoadCallee { .. }
            | Op::TailCallGuard { .. }
            | Op::BailOnly { .. }
            | Op::MakeFunc { .. }
            | Op::NewObject { .. }
            | Op::NewPlannedObject { .. }
            | Op::SuperCtorFetch { .. }
            | Op::RegExpLitProbe { .. }
            | Op::RegExpMethod { .. }
            | Op::RegExpLitTest { .. }
            | Op::IterProbe
            | Op::CallOut {
                lane: CallLane::None | CallLane::Plain { .. },
                ..
            }
            | Op::NewLane { .. }
            | Op::ClassConst { .. }
            | Op::AllocOut { .. }
            | Op::Literal { .. }
            | Op::IterStep { .. }
            | Op::WindowLoad(_)
            | Op::Handler { .. }
            | Op::EndFinally { .. }
            | Op::IterCloseFin { .. }
            | Op::JumpFinally { .. }
            | Op::UpvalGet(_)
            | Op::MathGuard { recv: None, .. } => {}
            Op::CallOut {
                lane: CallLane::Method { obj_v, arg0_v, .. },
                ..
            } => {
                f(obj_v);
                if let Some(a0) = arg0_v {
                    f(a0);
                }
            }
            Op::MakeCell { init, .. } => {
                if let Some(iv) = init {
                    f(iv);
                }
            }
            Op::MakeClosure { ref captures, .. } => {
                for &(_, u) in captures {
                    f(u);
                }
            }
            Op::CallOut {
                lane: CallLane::Prop { obj_v: a, .. },
                ..
            }
            | Op::StrictEqBits(_, a, _)
            | Op::IsAtomOrPrim(a)
            | Op::IsNullish { a, .. }
            | Op::MathGuard { recv: Some(a), .. }
            | Op::StoreGlobal(_, a)
            | Op::GuardCallee { v: a, .. }
            | Op::MathF1(_, a)
            | Op::MathCall1(_, a)
            | Op::BitNotI(a)
            | Op::TypeOf(a)
            | Op::TypeOfIs { a, .. }
            | Op::CheckCoercible { src: a, .. }
            | Op::ThisCheck { src: a, .. }
            | Op::ToConcatKey { src: a, .. }
            | Op::CellGet { cell: a, .. }
            | Op::LenOf { obj: a }
            | Op::ForInKeys { obj: a, .. }
            | Op::UpvalSet(_, a)
            | Op::I32ToF64(a)
            | Op::TaggedToF64(a)
            | Op::F64ToI32(a)
            | Op::TaggedToI32(a)
            | Op::UshrI(a, _)
            | Op::AddIK(a, _, _)
            | Op::Box(a)
            | Op::TaggedTruthy(a)
            | Op::F64Truthy(a)
            | Op::I32Truthy(a)
            | Op::NegF(a)
            | Op::NotB(a)
            | Op::GetPropShape { obj: a, .. }
            | Op::ShapeVals { obj: a, .. }
            | Op::IterPure { src: a, .. }
            | Op::Throw { v: a, .. }
            | Op::ReturnFinally { v: a }
            | Op::LoadSlot { vals: a, .. }
            | Op::GetPropPoly { obj: a, .. }
            | Op::Length { obj: a, .. }
            | Op::GuardFid { v: a, .. }
            | Op::GuardConstObj { obj: a, .. }
            | Op::GuardClass { obj: a, .. } => f(a),
            Op::CallOut {
                lane: CallLane::SetProp { obj_v: a, val_v: b, .. },
                ..
            }
            | Op::StrictEqAtom { a, atom_ok: b, .. }
            | Op::SetPropShape { obj: a, val: b, .. }
            | Op::MathCall2(_, a, b)
            | Op::ImulI(a, b)
            | Op::ModF(a, b)
            | Op::AddT(a, b)
            | Op::StrictEqH(_, a, b)
            | Op::LooseEq { a, b, .. }
            | Op::InstanceOf { val: a, ctor: b, .. }
            | Op::TypeOfSame { a, b, .. }
            | Op::ConcatChain { a, b, .. }
            | Op::CellSet { cell: a, src: b, .. }
            | Op::AppendDataProp { obj: a, val: b, .. }
            | Op::ToPropKey { obj: a, src: b, .. }
            | Op::HasProp { key: a, obj: b, .. }
            | Op::ForInLive { obj: a, key: b }
            | Op::GetIndex { obj: a, key: b, .. }
            | Op::CharCodeAt { obj: a, idx: b }
            | Op::AddF(a, b)
            | Op::SubF(a, b)
            | Op::AddI(a, b, _)
            | Op::SubI(a, b, _)
            | Op::MulF(a, b)
            | Op::DivF(a, b)
            | Op::Bit(_, a, b)
            | Op::UshrF(a, b)
            | Op::CmpF(_, a, b)
            | Op::CmpI(_, a, b)
            | Op::CmpT(_, a, b)
            | Op::CmpB(_, a, b)
            | Op::StrictEqT(_, a, b) => {
                f(a);
                f(b);
            }
            Op::IsArray { val: a, callee: b, this_v: c, .. }
            | Op::ObjectKeys { obj: a, callee: b, this_v: c, .. }
            | Op::StoreSlot { vals: a, obj: b, val: c, .. }
            | Op::InitDataPropDyn { obj: a, key: b, val: c, .. }
            | Op::SetIndex { obj: a, key: b, val: c, .. } => {
                f(a);
                f(b);
                f(c);
            }
        }
    }

    /// Rewrite every SSA operand (including `Op::Phi` inputs) through `f`.
    pub(crate) fn map_operands(&mut self, mut f: impl FnMut(V) -> V) {
        match self {
            Op::ConstI32(_)
            | Op::ConstF64(_)
            | Op::ConstBool(_)
            | Op::ConstTagged(_)
            | Op::Param(_)
            | Op::LoadGlobal(_)
            | Op::LoadGlobalOrUndef(_)
            | Op::LoadGlobalSlow { .. }
            | Op::LoadGlobalOrUndefSlow { .. }
            | Op::LoadCallee { .. }
            | Op::TailCallGuard { .. }
            | Op::BailOnly { .. }
            | Op::MakeFunc { .. }
            | Op::NewObject { .. }
            | Op::NewPlannedObject { .. }
            | Op::SuperCtorFetch { .. }
            | Op::RegExpLitProbe { .. }
            | Op::RegExpMethod { .. }
            | Op::RegExpLitTest { .. }
            | Op::IterProbe
            | Op::CallOut {
                lane: CallLane::None | CallLane::Plain { .. },
                ..
            }
            | Op::NewLane { .. }
            | Op::ClassConst { .. }
            | Op::AllocOut { .. }
            | Op::Literal { .. }
            | Op::IterStep { .. }
            | Op::WindowLoad(_)
            | Op::Handler { .. }
            | Op::EndFinally { .. }
            | Op::IterCloseFin { .. }
            | Op::JumpFinally { .. }
            | Op::UpvalGet(_)
            | Op::MathGuard { recv: None, .. } => {}
            Op::Phi(ins) => {
                for u in ins.iter_mut() {
                    *u = f(*u);
                }
            }
            Op::CallOut {
                lane: CallLane::Method { obj_v, arg0_v, .. },
                ..
            } => {
                *obj_v = f(*obj_v);
                if let Some(a0) = arg0_v {
                    *a0 = f(*a0);
                }
            }
            Op::MakeCell { init, .. } => {
                if let Some(iv) = init {
                    *iv = f(*iv);
                }
            }
            Op::MakeClosure { captures, .. } => {
                for (_, u) in captures.iter_mut() {
                    *u = f(*u);
                }
            }
            Op::CallOut {
                lane: CallLane::Prop { obj_v: a, .. },
                ..
            }
            | Op::StrictEqBits(_, a, _)
            | Op::IsAtomOrPrim(a)
            | Op::IsNullish { a, .. }
            | Op::MathGuard { recv: Some(a), .. }
            | Op::StoreGlobal(_, a)
            | Op::GuardCallee { v: a, .. }
            | Op::MathF1(_, a)
            | Op::MathCall1(_, a)
            | Op::BitNotI(a)
            | Op::TypeOf(a)
            | Op::TypeOfIs { a, .. }
            | Op::CheckCoercible { src: a, .. }
            | Op::ThisCheck { src: a, .. }
            | Op::ToConcatKey { src: a, .. }
            | Op::CellGet { cell: a, .. }
            | Op::LenOf { obj: a }
            | Op::ForInKeys { obj: a, .. }
            | Op::UpvalSet(_, a)
            | Op::I32ToF64(a)
            | Op::TaggedToF64(a)
            | Op::F64ToI32(a)
            | Op::TaggedToI32(a)
            | Op::UshrI(a, _)
            | Op::AddIK(a, _, _)
            | Op::Box(a)
            | Op::TaggedTruthy(a)
            | Op::F64Truthy(a)
            | Op::I32Truthy(a)
            | Op::NegF(a)
            | Op::NotB(a)
            | Op::GetPropShape { obj: a, .. }
            | Op::ShapeVals { obj: a, .. }
            | Op::IterPure { src: a, .. }
            | Op::Throw { v: a, .. }
            | Op::ReturnFinally { v: a }
            | Op::LoadSlot { vals: a, .. }
            | Op::GetPropPoly { obj: a, .. }
            | Op::Length { obj: a, .. }
            | Op::GuardFid { v: a, .. }
            | Op::GuardConstObj { obj: a, .. }
            | Op::GuardClass { obj: a, .. } => {
                *a = f(*a);
            }
            Op::CallOut {
                lane: CallLane::SetProp { obj_v: a, val_v: b, .. },
                ..
            }
            | Op::StrictEqAtom { a, atom_ok: b, .. }
            | Op::SetPropShape { obj: a, val: b, .. }
            | Op::MathCall2(_, a, b)
            | Op::ImulI(a, b)
            | Op::ModF(a, b)
            | Op::AddT(a, b)
            | Op::StrictEqH(_, a, b)
            | Op::LooseEq { a, b, .. }
            | Op::InstanceOf { val: a, ctor: b, .. }
            | Op::TypeOfSame { a, b, .. }
            | Op::ConcatChain { a, b, .. }
            | Op::CellSet { cell: a, src: b, .. }
            | Op::AppendDataProp { obj: a, val: b, .. }
            | Op::ToPropKey { obj: a, src: b, .. }
            | Op::HasProp { key: a, obj: b, .. }
            | Op::ForInLive { obj: a, key: b }
            | Op::GetIndex { obj: a, key: b, .. }
            | Op::CharCodeAt { obj: a, idx: b }
            | Op::AddF(a, b)
            | Op::SubF(a, b)
            | Op::AddI(a, b, _)
            | Op::SubI(a, b, _)
            | Op::MulF(a, b)
            | Op::DivF(a, b)
            | Op::Bit(_, a, b)
            | Op::UshrF(a, b)
            | Op::CmpF(_, a, b)
            | Op::CmpI(_, a, b)
            | Op::CmpT(_, a, b)
            | Op::CmpB(_, a, b)
            | Op::StrictEqT(_, a, b) => {
                *a = f(*a);
                *b = f(*b);
            }
            Op::IsArray { val: a, callee: b, this_v: c, .. }
            | Op::ObjectKeys { obj: a, callee: b, this_v: c, .. }
            | Op::StoreSlot { vals: a, obj: b, val: c, .. }
            | Op::InitDataPropDyn { obj: a, key: b, val: c, .. }
            | Op::SetIndex { obj: a, key: b, val: c, .. } => {
                *a = f(*a);
                *b = f(*b);
                *c = f(*c);
            }
        }
    }
}

pub(crate) struct Inst {
    pub(crate) op: Op,
    pub(crate) rep: Rep,
    /// The frame state a failed guard of this op deopts with (a call-out's:
    /// the one written before the call).
    pub(crate) fs: Option<u32>,
    pub(crate) block: B,
}

pub(crate) enum Term {
    Jump(B),
    /// On a Bool: `t` when true.
    Branch { cond: V, t: B, f: B },
    /// Constant-enum integer jump table: dispatches `val` against `min_val..=min_val+span`.
    Switch {
        val: V,
        min_val: i32,
        targets: Vec<Option<B>>,
        default: B,
    },
    /// A boxed value.
    Return(V),
    /// The block's last op left the body (`JumpFinally`, `Throw`).
    Exit,
}

impl Term {
    pub(crate) fn succs(&self) -> Vec<B> {
        match *self {
            Term::Jump(s) => vec![s],
            Term::Branch { t, f, .. } => {
                if t == f {
                    vec![t]
                } else {
                    vec![t, f]
                }
            }
            Term::Switch {
                ref targets,
                default,
                ..
            } => {
                let mut out = Vec::with_capacity(targets.len() + 1);
                out.push(default);
                for &opt in targets {
                    if let Some(tb) = opt {
                        if !out.contains(&tb) {
                            out.push(tb);
                        }
                    }
                }
                out
            }
            Term::Return(_) | Term::Exit => vec![],
        }
    }
}

pub(crate) struct Block {
    /// The phis, then the block's other values, in order.
    pub(crate) insts: Vec<V>,
    pub(crate) term: Term,
    pub(crate) preds: Vec<B>,
    /// Entry `(ip, live_regs)` for loop headers, used by LICM to synthesize
    /// preheader `FrameState`s for hoisted guards.
    pub(crate) entry_state: Option<(u32, Vec<(u16, V)>)>,
}

/// What a failed guard writes back: the position to resume at and, for
/// each VM register live there, the value it holds.
pub(crate) struct FrameState {
    pub(crate) ip: u32,
    /// The site a deopt here blames (`Jit::note_opt_deopt`): the op's
    /// position, an inlined body's call, or `ENTRY_SITE`.
    pub(crate) why: u32,
    pub(crate) regs: Vec<(u16, V)>,
}

/// The deopt site of the parameters unboxed at entry.
pub(crate) const ENTRY_SITE: u32 = u32::MAX;
/// In `Inputs::generic`: every site is generic (`Jit::note_opt_deopt`).
pub(crate) const ALL_SITES: u32 = u32::MAX - 1;

/// Whether `generic` (`Inputs::generic`) makes the site `ip` generic.
fn is_generic(generic: Option<&rustc_hash::FxHashSet<u32>>, ip: u32) -> bool {
    generic.is_some_and(|g| g.contains(&ip) || g.contains(&ALL_SITES))
}

pub(crate) struct Func {
    pub(crate) func_id: u32,
    pub(crate) insts: Vec<Inst>,
    pub(crate) blocks: Vec<Block>,
    pub(crate) states: Vec<FrameState>,
    /// Calls inlined, for the log.
    pub(crate) inlined: u32,
    /// The intrinsic `Math` the `MathGuard`s prove.
    pub(crate) math: Option<MathIntrinsicGuard>,
    /// The nursery barrier may test generation bytes (`emit_heap_store_barrier`).
    pub(crate) gen_lane: bool,
    /// This body's number among the function's Tier O bodies, which its
    /// deopts report: a parked one still running (an array builtin's
    /// callback) must not count against its successor (`Jit::opt_gen`).
    pub(crate) body_gen: u32,
    /// Typed-array records in the frame, Tier C's (`emit_ta_cache_get`):
    /// one per receiver register of the most element accesses, at most
    /// `MAX_TA_RECORDS`.
    pub(crate) ta_records: u8,
    /// The call sites with Tier C's emitted cross-call lanes (`CallLane`),
    /// the helpers they call, and the function's register count.
    pub(crate) cross: FxHashMap<u32, super::super::CrossCallSitePlan>,
    pub(crate) heap: Option<super::super::HeapHelpers>,
    pub(crate) reg_count: u16,
    /// The `CallWithThis` sites whose callee may be an intrinsic Tier C
    /// calls directly (`captured_builtin_lane`): Map / Set lookups and
    /// writes, `push`, `charCodeAt` and the string methods.
    pub(crate) captured: FxHashMap<u32, super::super::inline::CapturedLane>,
    /// Each `GetProp`'s inline cache site, as Tier C numbers them (the k-th
    /// `GetProp` / `SetProp` of the body takes the k-th reserved site), and
    /// whether its ways are shape-keyed (`opt::ic_identity_keyed`).
    pub(crate) ic_sites: FxHashMap<u32, (u32, bool)>,
    /// Class-method inline read guards (`ClassReadPlan`), keyed by `ip`.
    pub(crate) class_read: FxHashMap<u32, super::super::ClassReadLane>,
    /// Per-bytecode-register flag: true for `0..=param_count` never written in
    /// the function, so `Op::Param(r)` can spill to `Loc::Param(r)` (`[rbx+8*r]`).
    pub(crate) readonly_param: Vec<bool>,
    /// Exact callee function IDs actually inlined into this body.
    pub(crate) inlined_callees: Vec<u32>,
    /// Statically proven plain closure plans (`ReadyClosures`), keyed by `ip`.
    pub(crate) plain_closures: FxHashMap<u32, super::super::PlainClosurePlan>,
}

/// The typed-array records a body keeps, as Tier C's `ta_cache_regs`.
const MAX_TA_RECORDS: usize = 4;

/// The registers whose typed arrays a body keeps a record of: the receivers
/// of its element accesses, the most used first.
fn ta_regs(code: &[Instr]) -> Vec<u16> {
    let mut counts: Vec<(u16, u32)> = Vec::new();
    for i in code {
        if let Instr::GetIndex { obj, .. } | Instr::SetIndex { obj, .. } = *i {
            match counts.iter_mut().find(|(r, _)| *r == obj) {
                Some((_, n)) => *n += 1,
                None => counts.push((obj, 1)),
            }
        }
    }
    crate::cold_sort::sort_by(&mut counts, |a, b| b.1.cmp(&a.1));
    counts.into_iter().take(MAX_TA_RECORDS).map(|(r, _)| r).collect()
}

/// Bodies larger than this are not worth the compile (and keep the bitsets
/// small).
const MAX_OPS: usize = 4096;
const MAX_REGS: usize = 1024;
/// The largest callee inlined.
const MAX_INLINE_OPS: usize = 64;

/// What decides the ops a function admits: its plans, and (`top`) the
/// top-level function's call sites and string constants. An inlined callee
/// has no `top`: no call, no helper that reads the window, no effect.
struct Admit<'a> {
    p: &'a FuncProto,
    plan: &'a PropShapePlan,
    top: Option<&'a FxHashMap<usize, Site>>,
    /// Rooted string constants for this function (top-level or inlined callee).
    strs: &'a FxHashMap<u32, u64>,
    /// Polymorphic own-data ways for `GetProp` sites in this function.
    poly: &'a FxHashMap<usize, Vec<(u32, u32)>>,
    /// Constant own-data primitive properties on `const_watched_objs`.
    const_props: &'a FxHashMap<usize, (u32, u32, u64)>,
    /// Monomorphic plain `Call` sites admitted for depth-2 inlining inside a callee.
    callee_sites: Option<&'a FxHashMap<usize, u32>>,
    /// The intrinsic `Math`, when known (`Inputs::math`).
    math: Option<&'a MathIntrinsicGuard>,
    /// `Math.<op>` reads that feed a `MathOp`, by position (`math_reads`).
    math_reads: &'a FxHashMap<usize, MathFn>,
    /// All local registers converted to capture cells in `p.code`; kept live
    /// into every `MakeClosure` / `MakeArrow` so `write_state` flushes them
    /// before `jit_make_closure` reads `ParentLocal` sources from the window.
    cell_regs: &'a [u16],
}

fn collect_cell_regs(code: &[Instr]) -> Vec<u16> {
    let mut regs = Vec::new();
    for i in code {
        if let Instr::MakeCell { reg } | Instr::MakeCellTdz { reg } | Instr::MakeCellFnName { reg } = *i {
            if !regs.contains(&reg) {
                regs.push(reg);
            }
        }
    }
    regs
}

/// The `GetProp`s of `code` that read a `Math` method only for a `MathOp`
/// later in the same straight run (its callee and receiver registers
/// untouched between): the read's position, and the op.
fn math_reads(code: &[Instr]) -> FxHashMap<usize, MathFn> {
    let mut out = FxHashMap::default();
    for (ip, i) in code.iter().enumerate() {
        let Instr::MathOp {
            op, callee, this_v, ..
        } = *i
        else {
            continue;
        };
        if callee == crate::bytecode::NO_REG {
            continue;
        }
        for back in (0..ip).rev() {
            let j = &code[back];
            if let Instr::GetProp { dst, obj, .. } = *j {
                if dst == callee && obj == this_v {
                    out.insert(back, op);
                    break;
                }
            }
            if flow(j, back).1.is_some() || dst_of(j).is_some_and(|d| d == callee || d == this_v) {
                break;
            }
        }
    }
    out
}

/// `math_reads` kept to the methods the intrinsic guard can prove (`math`).
fn math_reads_known(code: &[Instr], math: Option<&MathIntrinsicGuard>) -> FxHashMap<usize, MathFn> {
    let Some(g) = math else {
        return FxHashMap::default();
    };
    let mut r = math_reads(code);
    r.retain(|_, op| g.op_callee_bits[*op as usize] != 0 && g.op_slot[*op as usize] != u32::MAX);
    r
}

/// The registers `i` at `ip` reads and the one it writes, for the ops Tier O
/// admits; `None` for any other op.
fn uses_def(i: &Instr, ip: usize, a: &Admit) -> Option<(Vec<u16>, Option<u16>)> {
    let args = |first: u16, base: u16, argc: u16| -> Vec<u16> {
        std::iter::once(first).chain(base..base + argc).collect()
    };
    let p = a.p;
    let top = a.top.is_some();
    Some(match *i {
        Instr::LoadInt { dst, .. } | Instr::LoadBool { dst, .. } | Instr::LoadNull { dst } | Instr::LoadUndefined { dst } => {
            (vec![], Some(dst))
        }
        Instr::LoadConst { dst, idx } => {
            let num = p.constants.get(idx as usize).is_some_and(|c| c.is_number());
            if !num && !a.strs.contains_key(&(idx as u32)) {
                return None;
            }
            (vec![], Some(dst))
        }
        Instr::LoadGlobal { dst, .. } => (vec![], Some(dst)),
        Instr::LoadGlobalOrUndefined { dst, .. } => (vec![], Some(dst)),
        Instr::LoadCallee { dst } if top => (vec![], Some(dst)),
        Instr::UpvalGet { dst, .. } if top => (vec![], Some(dst)),
        Instr::UpvalSet { src, .. } if top => (vec![src], None),
        Instr::MakeCell { reg } | Instr::MakeCellFnName { reg } if top => (vec![reg], Some(reg)),
        Instr::MakeCellTdz { reg } if top => (vec![], Some(reg)),
        Instr::MarkCellConst { reg } if top => (vec![reg], None),
        Instr::MakeFunc { dst, .. } if top => (vec![], Some(dst)),
        Instr::MakeClosure { dst, .. } if top => (a.cell_regs.to_vec(), Some(dst)),
        Instr::MakeArrow { dst, this_reg, .. } if top => {
            let mut u = a.cell_regs.to_vec();
            u.push(this_reg);
            (u, Some(dst))
        }
        Instr::CellGet { dst, cell } if top => (vec![cell], Some(dst)),
        Instr::CellSet { cell, src } | Instr::CellSetChecked { cell, src } if top => (vec![cell, src], None),
        Instr::GetIndex { dst, obj, key } => (vec![obj, key], Some(dst)),
        // A `try`'s handler stack, on the activation's own frame (a body with
        // handler ops is entered frame-backed only).
        Instr::PushFinally { .. } | Instr::PopFinally | Instr::PushHandler { .. } | Instr::PopHandler if top => {
            (vec![], None)
        }
        Instr::EndFinally { kind_reg, val_reg } if top => (vec![kind_reg, val_reg], None),
        Instr::IterCloseFinally { iter, kind_reg } if top => (vec![iter, kind_reg], None),
        Instr::JumpFinally { .. } if top => (vec![], None),
        // A `break`'s close of the iterator (its `return()` is user code).
        Instr::IterClose { iter } | Instr::IterCloseQuiet { iter } if top => (vec![iter], Some(iter)),
        Instr::Throw { src } if top => (vec![src], None),
        Instr::GetIterator { dst, src } if top => (vec![src], Some(dst)),
        Instr::IterPrime { dst, iter } if top => (vec![iter], Some(dst)),
        // Its other two writes (done, the cursor) count as reads too: they
        // stay live a little longer, which only widens the frame states.
        Instr::IterNext {
            value_dst,
            done_dst,
            iter,
            idx,
            next,
        } if top => {
            let mut u = vec![iter, idx, done_dst];
            if next != u16::MAX {
                u.push(next);
            }
            (u, Some(value_dst))
        }
        Instr::NewArray { dst, arg_base, argc } if top => ((arg_base..arg_base + argc).collect(), Some(dst)),
        Instr::NewObject { dst, .. } | Instr::NewPlannedObject { dst, .. } if top => (vec![], Some(dst)),
        Instr::AppendDataProp { obj, val, .. } if top => (vec![obj, val], None),
        Instr::InitDataProp { obj, val, name } if top && name <= 0xFF_FFFF => (vec![obj, val], Some(obj)),
        Instr::ObjectSpread { target, src } if top => (vec![target, src], Some(target)),
        Instr::ObjectRest { dst, src, .. } if top => (vec![src], Some(dst)),
        Instr::ArrayAppend { arr, val, .. } if top => (vec![arr, val], Some(arr)),
        Instr::IterToArray { dst, count, .. } if top && count & !0x7FFF == crate::bytecode::ITER_PROBE => {
            (vec![], Some(dst))
        }
        Instr::FinalizeObject {
            dst, val_base, count, ..
        } if top => ((val_base..val_base + count).collect(), Some(dst)),
        Instr::SetIndex { obj, key, val } if top => (vec![obj, key, val], None),
        Instr::StoreGlobal { src, .. } | Instr::StoreGlobalStrict { src, .. } if top => (vec![src], None),
        Instr::SetProp { obj, val, name, .. } if top && !is_private(p, name) => (vec![obj, val], None),
        Instr::LooseEq { dst, a: x, b: y } | Instr::LooseNe { dst, a: x, b: y } => (vec![x, y], Some(dst)),
        Instr::IsArray {
            dst,
            a: x,
            callee,
            this_v,
        } => (vec![x, callee, this_v], Some(dst)),
        Instr::InstanceOfDyn { dst, val, ctor } if top => (vec![val, ctor], Some(dst)),
        Instr::BitNot { dst, a: x } => (vec![x], Some(dst)),
        Instr::TypeOf { dst, a: x } if top => (vec![x], Some(dst)),
        Instr::TypeOfIs { dst, a: x, .. } => (vec![x], Some(dst)),
        Instr::TypeOfSame { dst, a: x, b: y, .. } => (vec![x, y], Some(dst)),
        Instr::StrConcatChain { dst, a: x, b: y } if top => (vec![x, y], Some(dst)),
        Instr::ToStr { dst, a: x } if top => (vec![x], Some(dst)),
        Instr::CheckCoercible { src } => (vec![src], None),
        Instr::TailCall {
            callee,
            arg_base,
            argc,
        } if top => (args(callee, arg_base, argc), None),
        Instr::StaticFn {
            dst,
            callee,
            this_v,
            arg_base,
            argc,
            ..
        } if top => {
            let mut u = args(callee, arg_base, argc);
            u.push(this_v);
            (u, Some(dst))
        }
        Instr::GlobalFn {
            dst,
            op,
            callee,
            arg_base,
            argc,
        } if top && !(op == crate::bytecode::GlobalFn::String && argc == 1) => {
            (args(callee, arg_base, argc), Some(dst))
        }
        Instr::ObjectKeys {
            dst,
            obj,
            callee,
            this_v,
        }
        | Instr::ObjectValues {
            dst,
            obj,
            callee,
            this_v,
        }
        | Instr::ObjectEntries {
            dst,
            obj,
            callee,
            this_v,
        } if top => (vec![obj, callee, this_v], Some(dst)),
        Instr::HasProp { dst, key, obj, brand } if top || !brand => (vec![key, obj], Some(dst)),
        Instr::LenOf { dst, obj } => (vec![obj], Some(dst)),
        Instr::ForInKeys { dst, obj } if top => (vec![obj], Some(dst)),
        Instr::ForInLive { dst, obj, key } if top => (vec![obj, key], Some(dst)),
        Instr::SuperGet { dst, .. } if top => (vec![0], Some(dst)),
        Instr::ThisCheck { src } if top => (vec![src], Some(src)),
        Instr::SuperCtorFetch { dst, .. } if top => (vec![], Some(dst)),
        Instr::SuperCtor {
            ctor,
            arg_base,
            argc,
            ..
        } if top => {
            let mut u = args(ctor, arg_base, argc);
            u.push(0);
            (u, Some(0))
        }
        Instr::SuperCtorSpread { ctor, args: a_reg, .. } if top => (vec![0, ctor, a_reg], Some(0)),
        Instr::ToPropKey { dst, obj, src } if top => (vec![obj, src], Some(dst)),
        Instr::ToConcatKey { dst, src } if top => (vec![src], Some(dst)),
        Instr::InitDataPropDyn { obj, key, val } if top => (vec![obj, key, val], Some(obj)),
        Instr::ArrayRest { dst, src, .. } if top => (vec![src], Some(dst)),
        Instr::GetIndexConcat { dst, obj, key, .. } if top => (vec![obj, key], Some(dst)),
        Instr::NewRegExp {
            dst, pattern, flags, ..
        } if top => (vec![pattern, flags], Some(dst)),
        Instr::RegExpLitProbe { dst, .. } if top => (vec![], Some(dst)),
        Instr::RegExpLitTest { dst, arg, .. } if top => (vec![arg], Some(dst)),
        Instr::RegExpMethod {
            dst,
            callee,
            this_v,
            arg_base,
            argc,
            ..
        } if top => {
            let mut u = args(callee, arg_base, argc);
            u.push(this_v);
            (u, Some(dst))
        }
        Instr::CallMethodComputed {
            dst,
            obj,
            key,
            arg_base,
            argc,
        } if top => {
            let mut u = args(obj, arg_base, argc);
            u.push(key);
            (u, Some(dst))
        }
        // Cold bail-only ops (hand off cleanly to interpreter without deopt count).
        Instr::DeleteProp { dst, obj, .. } if top => (vec![obj], Some(dst)),
        Instr::DeleteIndex { dst, obj, key, .. } if top => (vec![obj, key], Some(dst)),
        Instr::JsonStringify {
            dst,
            val,
            space,
            callee,
            this_v,
        } if top => (vec![val, space, callee, this_v], Some(dst)),
        Instr::JsonParse {
            dst,
            a: x,
            callee,
            this_v,
        } if top => (vec![x, callee, this_v], Some(dst)),
        Instr::CallWithThisSpread {
            dst,
            callee,
            this_v,
            args: a_reg,
        } if top => (vec![callee, this_v, a_reg], Some(dst)),
        Instr::ArrayFrom {
            dst,
            src,
            mapfn,
            callee,
            this_v,
            ..
        } if top => (vec![src, mapfn, callee, this_v], Some(dst)),
        Instr::Pow { dst, a: x, b: y } if top => (vec![x, y], Some(dst)),
        Instr::New {
            dst,
            callee,
            arg_base,
            argc,
        } if top => (args(callee, arg_base, argc), Some(dst)),
        Instr::LoadClassValue { dst, .. } if top => (vec![], Some(dst)),
        Instr::Mod { dst, a: x, b: y } => (vec![x, y], Some(dst)),
        Instr::MathOp {
            dst,
            op,
            callee,
            this_v,
            arg_base,
            argc,
        } if a
            .math
            .is_some_and(|g| g.op_callee_bits[op as usize] != 0 && g.op_slot[op as usize] != u32::MAX)
            && (1..=2).contains(&argc)
            && super::super::emit_misc::math_op_emittable(op, argc)
            && (callee != crate::bytecode::NO_REG || this_v < crate::bytecode::BARE_MATH_BY_NAME) =>
        {
            let mut u: Vec<u16> = (arg_base..arg_base + argc).collect();
            if callee != crate::bytecode::NO_REG {
                u.push(callee);
                u.push(this_v);
            }
            (u, Some(dst))
        }
        Instr::Move { dst, src } => (vec![src], Some(dst)),
        Instr::Add { dst, a, b }
        | Instr::Sub { dst, a, b }
        | Instr::Mul { dst, a, b }
        | Instr::Div { dst, a, b }
        | Instr::Lt { dst, a, b }
        | Instr::Le { dst, a, b }
        | Instr::Gt { dst, a, b }
        | Instr::Ge { dst, a, b }
        | Instr::Eq { dst, a, b }
        | Instr::Ne { dst, a, b }
        | Instr::Bitwise { dst, a, b, .. } => (vec![a, b], Some(dst)),
        Instr::AddInt { dst, a, .. } | Instr::Neg { dst, a } | Instr::Not { dst, a } => (vec![a], Some(dst)),
        Instr::GetProp { dst, obj, name } => {
            // Constant watched prop, receiver's shape, poly ways, a `Math` method's
            // guard, or (at the top) the slow helper; a private name is the frame's.
            let fused = a.math_reads.contains_key(&ip);
            if !a.const_props.contains_key(&ip)
                && !a.plan.contains_key(&ip)
                && !a.poly.contains_key(&ip)
                && !fused
                && !(top && !is_private(p, name))
            {
                return None;
            }
            (vec![obj], Some(dst))
        }
        Instr::Call {
            dst,
            callee,
            arg_base,
            argc,
        } if top || a.callee_sites.is_some_and(|s| s.contains_key(&ip)) => {
            (args(callee, arg_base, argc), Some(dst))
        }
        Instr::CallWithThis {
            dst,
            callee,
            this_v,
            arg_base,
            argc,
            ..
        } if top => {
            let mut u = args(callee, arg_base, argc);
            u.push(this_v);
            (u, Some(dst))
        }
        Instr::CallMethod {
            dst,
            obj,
            arg_base,
            argc,
            ..
        } if top => {
            (args(obj, arg_base, argc), Some(dst))
        }
        Instr::Jump { .. } | Instr::ReturnUndefined => (vec![], None),
        Instr::JumpIfFalse { cond, .. } | Instr::JumpIfTrue { cond, .. } => (vec![cond], None),
        Instr::JumpIfNotLt { a, b, .. } | Instr::JumpIfNotLe { a, b, .. } => (vec![a, b], None),
        Instr::Return { src } => (vec![src], None),
        _ => return None,
    })
}

/// Whether `Math.<op>(x)` is one instruction (`emit`'s `MathF1`), as Tier
/// C's `emit_math_unary_inline` makes it.
fn math_f1_inline(op: MathFn) -> bool {
    crate::codegen::math_inline_enabled()
        && match op {
            MathFn::Sqrt | MathFn::Abs => true,
            MathFn::Floor | MathFn::Ceil | MathFn::Trunc => std::arch::is_x86_feature_detected!("sse4.1"),
            _ => false,
        }
}

fn is_private(p: &FuncProto, name: u32) -> bool {
    p.string_constants
        .get(name as usize)
        .is_none_or(|k| crate::vm::is_private_key(k))
}

/// Where control goes after `i` at `ip`: its fall-through (if any) and its
/// jump target (if any).
fn flow(i: &Instr, ip: usize) -> (Option<usize>, Option<usize>) {
    match *i {
        Instr::Jump { target } => (None, Some(target as usize)),
        Instr::JumpIfFalse { target, .. }
        | Instr::JumpIfTrue { target, .. }
        | Instr::JumpIfNotLt { target, .. }
        | Instr::JumpIfNotLe { target, .. } => (Some(ip + 1), Some(target as usize)),
        // Bodies leave at these for the interpreter (`Flow::Exit`).
        Instr::Return { .. } | Instr::ReturnUndefined | Instr::JumpFinally { .. } | Instr::Throw { .. } => (None, None),
        _ => (Some(ip + 1), None),
    }
}

/// Where the interpreter may resume a body with handler ops, besides its
/// own control flow: every `finally` and `catch` (a throw, a return routed
/// through one) and every `break` target a `finally` routes on to. Liveness
/// makes them successors of every op, so every frame state holds what they
/// read.
fn resume_points(code: &[Instr]) -> Vec<usize> {
    let mut v: Vec<usize> = code
        .iter()
        .filter_map(|i| match *i {
            Instr::PushFinally { target, .. } | Instr::JumpFinally { target, .. } => Some(target as usize),
            Instr::PushHandler { catch_target, .. } => Some(catch_target as usize),
            _ => None,
        })
        .filter(|&t| t < code.len())
        .collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// The representation `i` makes its destination, given the registers'
/// (`reps`); a call's is `call` (an inlined callee's result, else boxed).
/// `over`: the representation the op's site was given (`addt`, `ushr_i`).
fn result_rep(i: &Instr, reps: &[Rep], p: &FuncProto, call: Rep, over: Option<Rep>) -> Option<Rep> {
    if let Some(r) = over {
        return Some(r);
    }
    Some(match *i {
        Instr::LoadInt { .. } | Instr::BitNot { .. } => Rep::I32,
        Instr::LoadConst { idx, .. } => {
            let c = p.constants[idx as usize];
            if c.is_int() {
                Rep::I32
            } else if c.is_number() {
                Rep::F64
            } else {
                Rep::Tagged
            }
        }
        Instr::UpvalGet { .. }
        | Instr::New { .. }
        | Instr::LoadClassValue { .. }
        | Instr::GetIndex { .. }
        | Instr::NewArray { .. }
        | Instr::NewObject { .. }
        | Instr::NewPlannedObject { .. }
        | Instr::InitDataProp { .. }
        | Instr::ObjectSpread { .. }
        | Instr::ObjectRest { .. }
        | Instr::ArrayAppend { .. }
        | Instr::IterToArray { .. }
        | Instr::FinalizeObject { .. }
        | Instr::GetIterator { .. }
        | Instr::IterPrime { .. }
        | Instr::IterNext { .. }
        | Instr::IterClose { .. }
        | Instr::IterCloseQuiet { .. }
        | Instr::MakeCell { .. }
        | Instr::MakeCellTdz { .. }
        | Instr::MakeCellFnName { .. }
        | Instr::MakeFunc { .. }
        | Instr::MakeClosure { .. }
        | Instr::MakeArrow { .. }
        | Instr::CellGet { .. }
        | Instr::LoadCallee { .. }
        | Instr::TypeOf { .. }
        | Instr::StrConcatChain { .. }
        | Instr::ToStr { .. }
        | Instr::InstanceOfDyn { .. }
        | Instr::StaticFn { .. }
        | Instr::GlobalFn { .. }
        | Instr::ObjectKeys { .. }
        | Instr::ObjectValues { .. }
        | Instr::ObjectEntries { .. }
        | Instr::HasProp { brand: true, .. }
        | Instr::LenOf { .. }
        | Instr::ForInKeys { .. }
        | Instr::ForInLive { .. }
        | Instr::SuperGet { .. }
        | Instr::ThisCheck { .. }
        | Instr::SuperCtorFetch { .. }
        | Instr::SuperCtor { .. }
        | Instr::SuperCtorSpread { .. }
        | Instr::ToPropKey { .. }
        | Instr::ToConcatKey { .. }
        | Instr::InitDataPropDyn { .. }
        | Instr::ArrayRest { .. }
        | Instr::GetIndexConcat { .. }
        | Instr::NewRegExp { .. }
        | Instr::RegExpLitProbe { .. }
        | Instr::RegExpLitTest { .. }
        | Instr::RegExpMethod { .. }
        | Instr::CallMethodComputed { .. }
        | Instr::DeleteProp { .. }
        | Instr::DeleteIndex { .. }
        | Instr::JsonStringify { .. }
        | Instr::JsonParse { .. }
        | Instr::CallWithThisSpread { .. }
        | Instr::ArrayFrom { .. }
        | Instr::Pow { .. } => Rep::Tagged,
        Instr::Mod { .. } => Rep::F64,
        Instr::MathOp { op, .. } => {
            if matches!(op, MathFn::Imul) {
                Rep::I32
            } else {
                Rep::F64
            }
        }
        Instr::LoadBool { .. }
        | Instr::Lt { .. }
        | Instr::Le { .. }
        | Instr::Gt { .. }
        | Instr::Ge { .. }
        | Instr::Eq { .. }
        | Instr::Ne { .. }
        | Instr::LooseEq { .. }
        | Instr::LooseNe { .. }
        | Instr::IsArray { .. }
        | Instr::TypeOfIs { .. }
        | Instr::TypeOfSame { .. }
        | Instr::HasProp { brand: false, .. }
        | Instr::Not { .. } => Rep::Bool,
        Instr::LoadNull { .. }
        | Instr::LoadUndefined { .. }
        | Instr::LoadGlobal { .. }
        | Instr::LoadGlobalOrUndefined { .. } => Rep::Tagged,
        // A getter inlined returns its body's.
        Instr::GetProp { .. } => call,
        Instr::Move { src, .. } => reps[src as usize],
        // Two i32s add and subtract as an i32 behind an overflow guard
        // (`AddI`); a site that overflowed before is given `F64` (`over`).
        Instr::Add { a, b, .. } | Instr::Sub { a, b, .. } if reps[a as usize] == Rep::I32 && reps[b as usize] == Rep::I32 => {
            Rep::I32
        }
        Instr::AddInt { a, .. } if reps[a as usize] == Rep::I32 => Rep::I32,
        Instr::Add { .. } | Instr::Sub { .. } | Instr::Mul { .. } | Instr::Div { .. } | Instr::AddInt { .. } | Instr::Neg { .. } => {
            Rep::F64
        }
        Instr::Bitwise { op, .. } => {
            if matches!(op, BitwiseOp::Ushr) {
                Rep::F64
            } else {
                Rep::I32
            }
        }
        Instr::Call { .. } | Instr::CallMethod { .. } | Instr::CallWithThis { .. } => call,
        _ => return None,
    })
}

/// Representation of a constant primitive folded from a `const_watched_objs` property.
#[inline]
fn const_prop_rep(bits: u64) -> Rep {
    let v = Value::from_bits(bits);
    if v.is_int() {
        Rep::I32
    } else if v.is_number() {
        Rep::F64
    } else if v.is_bool() {
        Rep::Bool
    } else {
        Rep::Tagged
    }
}

/// A callee Tier O inlines at a call site: pure forward-DAG code it admits
/// (no backward jump/loop, no write/effect, no captured variable; up to depth 2
/// nested pure `Call`s), small, with a `this` the inlined body can be given:
/// the receiver of a method call, `undefined` for a strict function's plain
/// call; an arrow or a sloppy plain call that reads its `this` is not.
/// Returns `Some(total_ops)` including transitive nested callees when admissible.
fn inlinable_ops(
    callee: &super::OptCallee,
    fid: u32,
    method: bool,
    math: Option<&MathIntrinsicGuard>,
    callees: &FxHashMap<u32, super::OptCallee>,
    depth: u8,
    stack: &mut Vec<u32>,
) -> Option<usize> {
    if stack.contains(&fid) {
        return None;
    }
    let c = callee.proto;
    let reads = math_reads_known(&c.code, math);
    let allow_nested = depth == 0 && !callee.sites.is_empty();
    let admit = Admit {
        p: c,
        plan: &callee.prop_shape,
        top: None,
        strs: &callee.const_strs,
        poly: &callee.poly,
        const_props: &callee.const_props,
        callee_sites: allow_nested.then_some(&callee.sites),
        math,
        math_reads: &reads,
        cell_regs: &[],
    };
    let n = c.code.len();
    if n == 0
        || c.is_generator
        || c.is_async
        || c.rest_reg.is_some()
        || c.arguments_reg.is_some()
        || !c.eval_sites.is_empty()
        || n > MAX_INLINE_OPS
        || c.reg_count as usize > MAX_INLINE_OPS
        || (c.param_count as usize) >= (c.reg_count as usize).max(1)
        || !matches!(c.code[n - 1], Instr::Return { .. } | Instr::ReturnUndefined)
    {
        return None;
    }
    stack.push(fid);
    let mut total_ops = n;
    let mut reads_this = false;
    let mut ok = true;
    for (ip, i) in c.code.iter().enumerate() {
        let Some((uses, def)) = uses_def(i, ip, &admit) else {
            ok = false;
            break;
        };
        if matches!(*i, Instr::Call { .. }) {
            let Some(&sub_fid) = callee.sites.get(&ip) else {
                ok = false;
                break;
            };
            let Some(sub_callee) = callees.get(&sub_fid) else {
                ok = false;
                break;
            };
            let Some(sub_ops) = inlinable_ops(sub_callee, sub_fid, false, math, callees, depth + 1, stack) else {
                ok = false;
                break;
            };
            total_ops += sub_ops;
            if total_ops > MAX_INLINE_OPS * 2 {
                ok = false;
                break;
            }
        }
        // Forward-only DAG: every branch target strictly after `ip` and in bounds.
        let fwd = match *i {
            Instr::Jump { target }
            | Instr::JumpIfFalse { target, .. }
            | Instr::JumpIfTrue { target, .. }
            | Instr::JumpIfNotLt { target, .. }
            | Instr::JumpIfNotLe { target, .. } => {
                let t = target as usize;
                t > ip && t < n
            }
            _ => true,
        };
        if !fwd || uses.iter().chain(def.iter()).any(|&r| r >= c.reg_count) {
            ok = false;
            break;
        }
        reads_this |= uses.contains(&0);
    }
    stack.pop();
    if ok && (!reads_this || (!c.lexical_this && (method || c.is_strict))) {
        Some(total_ops)
    } else {
        None
    }
}

/// Forward lattice pass over an `inlinable` forward-DAG callee to compute
/// the joined return `Rep` across all reachable `Return` paths.
fn inline_cfg_ret_rep(
    callee: &super::OptCallee,
    entry_reps: Vec<Rep>,
    callees: &FxHashMap<u32, super::OptCallee>,
    depth: u8,
) -> Option<Rep> {
    let c = callee.proto;
    let n = c.code.len();
    let nregs = entry_reps.len();
    let mut state: Vec<Option<Vec<Rep>>> = vec![None; n];
    state[0] = Some(entry_reps);
    let mut ret: Option<Rep> = None;
    let merge = |state: &mut [Option<Vec<Rep>>], dst: usize, src: &[Rep]| {
        match &mut state[dst] {
            None => state[dst] = Some(src.to_vec()),
            Some(cur) => {
                for r in 0..nregs {
                    cur[r] = Rep::join(cur[r], src[r]);
                }
            }
        }
    };
    for ip in 0..n {
        let Some(mut reps) = state[ip].take() else {
            continue;
        };
        let i = &c.code[ip];
        match *i {
            Instr::Return { src } => {
                let r = reps[src as usize];
                ret = Some(ret.map_or(r, |a| Rep::join(a, r)));
            }
            Instr::ReturnUndefined => {
                ret = Some(Rep::Tagged);
            }
            Instr::Jump { target } => {
                merge(&mut state, target as usize, &reps);
            }
            Instr::JumpIfFalse { target, .. }
            | Instr::JumpIfTrue { target, .. }
            | Instr::JumpIfNotLt { target, .. }
            | Instr::JumpIfNotLe { target, .. } => {
                merge(&mut state, target as usize, &reps);
                if ip + 1 < n {
                    merge(&mut state, ip + 1, &reps);
                }
            }
            _ => {
                let call = if let Instr::Call { arg_base, argc, .. } = *i {
                    if depth == 0 {
                        if let Some(sub) = callee.sites.get(&ip).and_then(|fid| callees.get(fid)) {
                            let mut sub_reps = vec![Rep::Tagged; sub.proto.reg_count as usize];
                            for k in 0..(argc as usize).min(sub.proto.param_count as usize) {
                                sub_reps[1 + k] = reps[arg_base as usize + k];
                            }
                            inline_sim(sub, sub_reps.clone(), argc as usize)
                                .map(|(r, _)| r)
                                .or_else(|| inline_cfg_ret_rep(sub, sub_reps, callees, depth + 1))
                                .unwrap_or(Rep::Tagged)
                        } else {
                            Rep::Tagged
                        }
                    } else {
                        Rep::Tagged
                    }
                } else {
                    Rep::Tagged
                };
                let over = match *i {
                    Instr::GetProp { .. } => callee.const_props.get(&ip).map(|&(_, _, bits)| const_prop_rep(bits)),
                    _ => None,
                };
                if let (Some(r), Some(d)) = (result_rep(i, &reps, c, call, over), dst_of(i)) {
                    reps[d as usize] = r;
                }
                if ip + 1 < n {
                    merge(&mut state, ip + 1, &reps);
                }
            }
        }
    }
    ret
}

/// What `inline_sim` knows of a callee register beyond its representation.
#[derive(Clone, Copy, PartialEq)]
enum Known {
    Unknown,
    Num,
    Undef,
    Null,
    Bool(bool),
}

/// The way an inlined callee `c` goes, its registers entering with `reps`
/// and `argc` arguments passed: every branch on it decided by what the
/// arguments are (a default parameter's `x === undefined` of a Number, or
/// of an argument not passed), the ops built in order, the last a return,
/// and the representation it returns. `None`: a branch or nested call the
/// straight-line path does not decide, so `inline_cfg` handles it.
fn inline_sim(callee: &super::OptCallee, mut reps: Vec<Rep>, argc: usize) -> Option<(Rep, Vec<usize>)> {
    let c = callee.proto;
    let n = c.code.len();
    let mut known = vec![Known::Unknown; reps.len()];
    for r in 1..=(c.param_count as usize).min(reps.len().saturating_sub(1)) {
        known[r] = if r > argc {
            Known::Undef
        } else {
            match reps[r] {
                Rep::I32 | Rep::F64 => Known::Num,
                _ => Known::Unknown,
            }
        };
    }
    let mut path = Vec::new();
    let mut ip = 0;
    while ip < n {
        let i = &c.code[ip];
        match *i {
            Instr::Return { src } => {
                path.push(ip);
                return Some((reps[src as usize], path));
            }
            Instr::ReturnUndefined => {
                path.push(ip);
                return Some((Rep::Tagged, path));
            }
            Instr::Call { .. } => return None,
            Instr::Jump { target } => {
                if target as usize <= ip {
                    return None;
                }
                ip = target as usize;
                continue;
            }
            Instr::JumpIfFalse { cond, target } | Instr::JumpIfTrue { cond, target } => {
                let Known::Bool(v) = known[cond as usize] else {
                    return None;
                };
                if target as usize <= ip {
                    return None;
                }
                let on = matches!(*i, Instr::JumpIfTrue { .. });
                ip = if v == on { target as usize } else { ip + 1 };
                continue;
            }
            Instr::JumpIfNotLt { .. } | Instr::JumpIfNotLe { .. } => return None,
            _ => {}
        }
        path.push(ip);
        let over = match *i {
            Instr::GetProp { .. } => callee.const_props.get(&ip).map(|&(_, _, bits)| const_prop_rep(bits)),
            _ => None,
        };
        let r = result_rep(i, &reps, c, Rep::Tagged, over);
        let k = match *i {
            Instr::LoadUndefined { .. } => Known::Undef,
            Instr::LoadNull { .. } => Known::Null,
            Instr::LoadBool { val, .. } => Known::Bool(val),
            Instr::Move { src, .. } => known[src as usize],
            Instr::Eq { a, b, .. } | Instr::Ne { a, b, .. } => {
                let eq = matches!(*i, Instr::Eq { .. });
                let nullish = |k: Known| matches!(k, Known::Undef | Known::Null);
                match (known[a as usize], known[b as usize]) {
                    (Known::Num, k) | (k, Known::Num) if nullish(k) => Known::Bool(!eq),
                    (x, y) if nullish(x) && nullish(y) => Known::Bool((x == y) == eq),
                    _ => Known::Unknown,
                }
            }
            _ => match r {
                Some(Rep::I32 | Rep::F64) => Known::Num,
                _ => Known::Unknown,
            },
        };
        if let (Some(r), Some(d)) = (r, dst_of(i)) {
            reps[d as usize] = r;
            known[d as usize] = k;
        }
        ip += 1;
    }
    None
}

fn dst_of(i: &Instr) -> Option<u16> {
    match *i {
        Instr::LoadInt { dst, .. }
        | Instr::LoadBool { dst, .. }
        | Instr::LoadNull { dst }
        | Instr::LoadUndefined { dst }
        | Instr::LoadConst { dst, .. }
        | Instr::LoadGlobal { dst, .. }
        | Instr::LoadGlobalOrUndefined { dst, .. }
        | Instr::LoadCallee { dst }
        | Instr::Move { dst, .. }
        | Instr::Add { dst, .. }
        | Instr::Sub { dst, .. }
        | Instr::Mul { dst, .. }
        | Instr::Div { dst, .. }
        | Instr::Lt { dst, .. }
        | Instr::Le { dst, .. }
        | Instr::Gt { dst, .. }
        | Instr::Ge { dst, .. }
        | Instr::Eq { dst, .. }
        | Instr::Ne { dst, .. }
        | Instr::LooseEq { dst, .. }
        | Instr::LooseNe { dst, .. }
        | Instr::IsArray { dst, .. }
        | Instr::InstanceOfDyn { dst, .. }
        | Instr::Bitwise { dst, .. }
        | Instr::BitNot { dst, .. }
        | Instr::AddInt { dst, .. }
        | Instr::Neg { dst, .. }
        | Instr::Not { dst, .. }
        | Instr::TypeOf { dst, .. }
        | Instr::TypeOfIs { dst, .. }
        | Instr::TypeOfSame { dst, .. }
        | Instr::StrConcatChain { dst, .. }
        | Instr::ToStr { dst, .. }
        | Instr::GetProp { dst, .. }
        | Instr::Call { dst, .. }
        | Instr::CallMethod { dst, .. }
        | Instr::CallMethodComputed { dst, .. }
        | Instr::CallWithThis { dst, .. }
        | Instr::StaticFn { dst, .. }
        | Instr::GlobalFn { dst, .. }
        | Instr::UpvalGet { dst, .. }
        | Instr::MakeCell { reg: dst }
        | Instr::MakeCellTdz { reg: dst }
        | Instr::MakeCellFnName { reg: dst }
        | Instr::MakeFunc { dst, .. }
        | Instr::MakeClosure { dst, .. }
        | Instr::MakeArrow { dst, .. }
        | Instr::CellGet { dst, .. }
        | Instr::New { dst, .. }
        | Instr::LoadClassValue { dst, .. }
        | Instr::Mod { dst, .. }
        | Instr::MathOp { dst, .. }
        | Instr::GetIndex { dst, .. }
        | Instr::NewArray { dst, .. }
        | Instr::NewObject { dst, .. }
        | Instr::NewPlannedObject { dst, .. }
        | Instr::InitDataProp { obj: dst, .. }
        | Instr::ObjectSpread { target: dst, .. }
        | Instr::ObjectRest { dst, .. }
        | Instr::ArrayAppend { arr: dst, .. }
        | Instr::IterToArray { dst, .. }
        | Instr::FinalizeObject { dst, .. }
        | Instr::GetIterator { dst, .. }
        | Instr::IterPrime { dst, .. }
        | Instr::IterNext { value_dst: dst, .. }
        | Instr::IterClose { iter: dst }
        | Instr::IterCloseQuiet { iter: dst }
        | Instr::ObjectKeys { dst, .. }
        | Instr::ObjectValues { dst, .. }
        | Instr::ObjectEntries { dst, .. }
        | Instr::HasProp { dst, .. }
        | Instr::LenOf { dst, .. }
        | Instr::ForInKeys { dst, .. }
        | Instr::ForInLive { dst, .. }
        | Instr::SuperGet { dst, .. }
        | Instr::ThisCheck { src: dst }
        | Instr::SuperCtorFetch { dst, .. }
        | Instr::ToPropKey { dst, .. }
        | Instr::ToConcatKey { dst, .. }
        | Instr::InitDataPropDyn { obj: dst, .. }
        | Instr::ArrayRest { dst, .. }
        | Instr::GetIndexConcat { dst, .. }
        | Instr::NewRegExp { dst, .. }
        | Instr::RegExpLitProbe { dst, .. }
        | Instr::RegExpLitTest { dst, .. }
        | Instr::RegExpMethod { dst, .. }
        | Instr::DeleteProp { dst, .. }
        | Instr::DeleteIndex { dst, .. }
        | Instr::JsonStringify { dst, .. }
        | Instr::JsonParse { dst, .. }
        | Instr::CallWithThisSpread { dst, .. }
        | Instr::ArrayFrom { dst, .. }
        | Instr::Pow { dst, .. } => Some(dst),
        Instr::SuperCtor { .. } | Instr::SuperCtorSpread { .. } => Some(0),
        _ => None,
    }
}

/// A fixed-width register set.
#[derive(Clone, PartialEq, Eq)]
struct Bits(Vec<u64>);

impl Bits {
    fn new(n: usize) -> Bits {
        Bits(vec![0; n.div_ceil(64).max(1)])
    }
    fn clear_all(&mut self) {
        self.0.fill(0);
    }
    fn get(&self, r: usize) -> bool {
        self.0[r / 64] >> (r % 64) & 1 != 0
    }
    fn set(&mut self, r: usize) {
        self.0[r / 64] |= 1 << (r % 64);
    }
    fn clear(&mut self, r: usize) {
        self.0[r / 64] &= !(1 << (r % 64));
    }
    fn union(&mut self, o: &Bits) -> bool {
        let mut changed = false;
        for (a, b) in self.0.iter_mut().zip(&o.0) {
            let n = *a | *b;
            changed |= n != *a;
            *a = n;
        }
        changed
    }
    #[inline]
    fn for_each_set(&self, mut f: impl FnMut(usize)) {
        for (wi, &w) in self.0.iter().enumerate() {
            let mut m = w;
            let base = wi * 64;
            while m != 0 {
                let tz = m.trailing_zeros() as usize;
                f(base + tz);
                m &= m - 1;
            }
        }
    }
}

/// Where one op sends control (`translate`).
enum Flow {
    Next,
    Jump(usize),
    /// On a Bool, to positions.
    Branch {
        cond: V,
        t: usize,
        f: usize,
    },
    /// The value returned, in its own representation.
    Ret(V),
    /// The op left the body for the interpreter (`Term::Exit`).
    Exit,
}

/// The function a translation reads its constants and plans from.
struct Fn<'a> {
    p: &'a FuncProto,
    /// True for the top-level function being compiled; false for an inlined callee.
    is_top: bool,
    /// Its function id (the top level's; an inlined callee makes no op that
    /// names it).
    func_id: u64,
    /// `trunc_only_arith_ips`: the adds whose result is observed only
    /// through ToInt32, which wrap.
    trunc: Vec<bool>,
    /// The body has handler ops (`proto_has_handler_ops`): a return may
    /// route through a `finally` (`Op::ReturnFinally`).
    handlers: bool,
    plan: &'a PropShapePlan,
    /// The top level's string constants (`Inputs::const_strs`).
    strs: Option<&'a FxHashMap<u32, u64>>,
    math: Option<&'a MathIntrinsicGuard>,
    /// `math_reads_known`.
    math_reads: FxHashMap<usize, MathFn>,
    /// The top level's deopted sites (`Inputs::generic`) and full `+`s.
    generic: Option<&'a rustc_hash::FxHashSet<u32>>,
    addt: Option<&'a rustc_hash::FxHashSet<usize>>,
    ushr_i: Option<&'a rustc_hash::FxHashSet<usize>>,
    new_lanes: Option<&'a FxHashMap<usize, &'static super::super::NewLanePlan>>,
    /// `ta_regs` of the top level.
    ta_regs: &'a [u16],
    /// `Inputs::class_values`, `super_parents`, and `class_epoch` (none in a callee).
    class_values: Option<(&'a FxHashMap<u32, u64>, u32)>,
    super_parents: Option<(&'a FxHashMap<u32, (u32, u64)>, u32)>,
    /// `Inputs::poly` (none in a callee).
    poly: Option<&'a FxHashMap<usize, Vec<(u32, u32)>>>,
    /// Constant own-data primitive properties on `const_watched_objs`.
    const_props: Option<&'a FxHashMap<usize, (u32, u32, u64)>>,
    /// Global slots routed through pure slow helpers (none in a callee).
    slow_global_loads: Option<&'a rustc_hash::FxHashSet<u32>>,
    /// Nested inlining map for depth-1 callees (`ip -> callee_fid`).
    callee_sites: Option<&'a FxHashMap<usize, u32>>,
    /// All inlinable callee bodies (for depth-2 nested inlining).
    callees: &'a FxHashMap<u32, super::OptCallee<'a>>,
    /// Current inline nesting depth (0 = top-level, 1 = depth-1 callee, 2 = leaf).
    inline_depth: u8,
    /// Tier C arithmetic type feedback (none in a callee).
    arith_fb: Option<&'a [u8]>,
    /// Statically proven plain closure plans (none in a callee).
    plain_closures: Option<&'a FxHashMap<usize, super::super::PlainClosurePlan>>,
    /// Per-ip flag: true when the shaped `GetProp`/`SetProp` at `ip` shares
    /// its receiver & shape with another shaped access in the same block (so
    /// `ShapeVals` CSE pays off; single-use sites emit non-deopting `GetPropShape`/
    /// `SetPropShape` instead).
    multi_prop: &'a [bool],
    /// Helper addresses + compile-time `atoms` snapshot.
    heap: &'a super::super::HeapHelpers,
}

impl Fn<'_> {
    /// Whether the top level's op at `ip` deopted in an earlier body.
    fn generic(&self, ip: usize) -> bool {
        is_generic(self.generic, ip as u32)
    }

    /// Whether Tier C's arithmetic feedback saw a double at `ip`.
    fn arith_double(&self, ip: usize) -> bool {
        self.arith_fb
            .and_then(|fb| fb.get(ip).copied())
            .is_some_and(|b| b & super::super::ARITH_FB_DOUBLE != 0)
    }
}

/// Whether Tier O builds a body of `p`'s kind and size at all.
fn body_ok(p: &FuncProto) -> bool {
    let (n, nregs) = (p.code.len(), p.reg_count as usize);
    !(n == 0
        || n > MAX_OPS
        || nregs > MAX_REGS
        || p.is_generator
        || p.is_async
        || p.rest_reg.is_some()
        || p.arguments_reg.is_some()
        || !p.eval_sites.is_empty()
        || (p.param_count as usize) >= nregs.max(1))
}

/// Whether Tier O admits every op of `p` without plans (which only widen
/// it): a Tier C body counts its entries toward Tier O only then
/// (`Jit::opt_tier_up`); one Tier O would decline is left be.
pub(crate) fn admits(p: &FuncProto, const_strs: &FxHashMap<u32, u64>, math: Option<&MathIntrinsicGuard>) -> bool {
    if !body_ok(p) {
        return false;
    }
    let reads = math_reads_known(&p.code, math);
    let cell_regs = collect_cell_regs(&p.code);
    let (plan, sites, empty_poly, empty_const) = (
        PropShapePlan::default(),
        FxHashMap::default(),
        FxHashMap::default(),
        FxHashMap::default(),
    );
    let admit = Admit {
        p,
        plan: &plan,
        top: Some(&sites),
        strs: const_strs,
        poly: &empty_poly,
        const_props: &empty_const,
        callee_sites: None,
        math,
        math_reads: &reads,
        cell_regs: &cell_regs,
    };
    let nregs = p.reg_count as usize;
    p.code.iter().enumerate().all(|(ip, i)| {
        uses_def(i, ip, &admit).is_some_and(|(u, d)| u.iter().chain(d.iter()).all(|&r| (r as usize) < nregs))
    })
}

/// The IR of `proto`, or `None` when Tier O does not admit it.
pub(crate) fn build(p: &FuncProto, func_id: u32, inputs: &Inputs) -> Option<Func> {
    let code = &p.code;
    let n = code.len();
    let nregs = p.reg_count as usize;
    if !body_ok(p) {
        return None;
    }
    let math = inputs.math.as_ref();
    let top_reads = math_reads_known(code, math);
    let cell_regs = collect_cell_regs(code);
    let admit = Admit {
        p,
        plan: inputs.prop_shape,
        top: Some(inputs.sites),
        strs: inputs.const_strs,
        poly: inputs.poly,
        const_props: inputs.const_props,
        callee_sites: None,
        math,
        math_reads: &top_reads,
        cell_regs: &cell_regs,
    };
    let mut ud = Vec::with_capacity(n);
    for (ip, i) in code.iter().enumerate() {
        let Some((uses, def)) = uses_def(i, ip, &admit) else {
            if crate::jitlog_enabled() {
                eprintln!("[jit] Tier O fn{func_id} declines at ip {ip}: {i:?}");
            }
            return None;
        };
        if uses.iter().chain(def.iter()).any(|&r| r as usize >= nregs) {
            return None;
        }
        ud.push((uses, def));
    }
    // Every path ends at a return.
    if !matches!(code[n - 1], Instr::Return { .. } | Instr::ReturnUndefined) {
        return None;
    }
    for (ip, i) in code.iter().enumerate() {
        let (_, t) = flow(i, ip);
        if t.is_some_and(|t| t >= n) {
            return None;
        }
    }
    // The call sites inlined: deterministic ip order with a total inline budget
    // so representation dataflow and SSA translation always agree.
    let mut site_ips: Vec<(usize, Site)> = inputs.sites.iter().map(|(&ip, &s)| (ip, s)).collect();
    site_ips.sort_unstable_by_key(|&(ip, _)| ip);
    let mut inline_at: FxHashMap<usize, (&super::OptCallee, Site)> = FxHashMap::default();
    let mut inline_budget: usize = 0;
    const MAX_INLINE_BUDGET: usize = 256;
    let mut inline_stack = vec![func_id];
    for (ip, site) in site_ips {
        let Some(callee) = inputs.callees.get(&site.fid()) else {
            continue;
        };
        let method = site.method();
        let getter = matches!(site, Site::ClassGetter { .. });
        let ok = match code.get(ip) {
            Some(Instr::Call { .. } | Instr::CallWithThis { .. }) => !method,
            Some(Instr::CallMethod { .. }) => method && !getter,
            Some(Instr::GetProp { .. }) => getter,
            _ => false,
        };
        let deopted = is_generic(inputs.generic, ip as u32);
        if ok && !deopted {
            if let Some(cost) = inlinable_ops(callee, site.fid(), method, math, inputs.callees, 0, &mut inline_stack) {
                if inline_budget + cost <= MAX_INLINE_BUDGET {
                    inline_budget += cost;
                    inline_at.insert(ip, (callee, site));
                }
            }
        }
    }

    let generic = |ip: usize| is_generic(inputs.generic, ip as u32);
    let rpoints = resume_points(code);
    // Helper: build basic blocks, RPO order, predecessors, and liveness given
    // any active enum-switch jump tables (`switches`).
    #[allow(clippy::type_complexity)]
    let build_cfg = |switches: &FxHashMap<usize, &super::super::EnumSwitchPlan>,
                     ud_cur: &[(Vec<u16>, Option<u16>)]|
     -> (
        Vec<bool>,
        Vec<usize>,
        Vec<Vec<usize>>,
        Vec<usize>,
        Vec<usize>,
        Vec<Vec<usize>>,
        Vec<Bits>,
        Vec<Option<Bits>>,
    ) {
        let mut leader = vec![false; n];
        leader[0] = true;
        for (ip, i) in code.iter().enumerate() {
            if let Some(&plan) = switches.get(&ip) {
                let end_ip = ip + 4 * plan.n_cases;
                if end_ip < n {
                    leader[end_ip] = true;
                }
                for &tgt in plan.targets.iter().flatten() {
                    leader[tgt as usize] = true;
                }
                if ip + 1 < n {
                    leader[ip + 1] = true;
                }
                continue;
            }
            let (ft, t) = flow(i, ip);
            if let Some(t) = t {
                leader[t] = true;
                if ip + 1 < n {
                    leader[ip + 1] = true;
                }
            }
            if ft.is_none() && ip + 1 < n {
                leader[ip + 1] = true;
            }
        }
        for &t in &rpoints {
            leader[t] = true;
        }
        let starts: Vec<usize> = (0..n).filter(|&ip| leader[ip]).collect();
        let nb = starts.len();
        let block_of_ip = |ip: usize| starts.partition_point(|&s| s <= ip) - 1;
        let end_of = |b: usize| if b + 1 < nb { starts[b + 1] } else { n };
        let bsuccs: Vec<Vec<usize>> = (0..nb)
            .map(|b| {
                let last = end_of(b) - 1;
                if let Some(&plan) = switches.get(&last) {
                    let mut s = Vec::with_capacity(plan.targets.len() + 1);
                    s.push(block_of_ip(last + 4 * plan.n_cases));
                    for &tgt in plan.targets.iter().flatten() {
                        let tb = block_of_ip(tgt as usize);
                        if !s.contains(&tb) {
                            s.push(tb);
                        }
                    }
                    return s;
                }
                let (ft, t) = flow(&code[last], last);
                let mut s = Vec::new();
                if let Some(f) = ft {
                    if f < n {
                        s.push(block_of_ip(f));
                    }
                }
                if let Some(t) = t {
                    let tb = block_of_ip(t);
                    if !s.contains(&tb) {
                        s.push(tb);
                    }
                }
                s
            })
            .collect();
        let mut order = Vec::new();
        let mut seen = vec![false; nb];
        let mut stack: Vec<(usize, usize)> = vec![(0, 0)];
        seen[0] = true;
        while let Some(&mut (b, ref mut k)) = stack.last_mut() {
            if *k < bsuccs[b].len() {
                let s = bsuccs[b][*k];
                *k += 1;
                if !seen[s] {
                    seen[s] = true;
                    stack.push((s, 0));
                }
            } else {
                order.push(b);
                stack.pop();
            }
        }
        order.reverse();
        let mut rpo_index = vec![usize::MAX; nb];
        for (k, &b) in order.iter().enumerate() {
            rpo_index[b] = k;
        }
        let mut bpreds: Vec<Vec<usize>> = vec![Vec::new(); nb];
        for &b in &order {
            for &s in &bsuccs[b] {
                bpreds[s].push(b);
            }
        }
        let resume_blocks: Vec<usize> = rpoints.iter().map(|&t| block_of_ip(t)).collect();
        let resume_live = |live_in: &[Bits]| -> Bits {
            let mut l = Bits::new(nregs);
            for &rb in &resume_blocks {
                l.union(&live_in[rb]);
            }
            l
        };
        let all_blocks: Vec<usize> = if resume_blocks.is_empty() {
            order.clone()
        } else {
            (0..nb).collect()
        };
        let mut live_in: Vec<Bits> = vec![Bits::new(nregs); nb];
        let mut live_scratch = Bits::new(nregs);
        let has_resume = !resume_blocks.is_empty();
        let mut changed = true;
        while changed {
            changed = false;
            for &b in all_blocks.iter().rev() {
                let extra = has_resume.then(|| resume_live(&live_in));
                live_scratch.clear_all();
                for &s in &bsuccs[b] {
                    live_scratch.union(&live_in[s]);
                }
                for ip in (starts[b]..end_of(b)).rev() {
                    let (uses, def) = &ud_cur[ip];
                    if let Some(d) = def {
                        live_scratch.clear(*d as usize);
                    }
                    for &u in uses {
                        live_scratch.set(u as usize);
                    }
                    if let Some(ref ex) = extra {
                        live_scratch.union(ex);
                    }
                }
                if live_scratch != live_in[b] {
                    live_in[b].0.copy_from_slice(&live_scratch.0);
                    changed = true;
                }
            }
        }
        let extra = has_resume.then(|| resume_live(&live_in));
        let mut live_before: Vec<Option<Bits>> = vec![None; n];
        for &b in &order {
            live_scratch.clear_all();
            for &s in &bsuccs[b] {
                live_scratch.union(&live_in[s]);
            }
            for ip in (starts[b]..end_of(b)).rev() {
                let (uses, def) = &ud_cur[ip];
                if let Some(d) = def {
                    live_scratch.clear(*d as usize);
                }
                for &u in uses {
                    live_scratch.set(u as usize);
                }
                if let Some(ref ex) = extra {
                    live_scratch.union(ex);
                }
                live_before[ip] = Some(live_scratch.clone());
            }
        }
        (leader, starts, bsuccs, order, rpo_index, bpreds, live_in, live_before)
    };

    let empty_switches: FxHashMap<usize, &super::super::EnumSwitchPlan> = FxHashMap::default();
    let (mut leader, mut starts, mut bsuccs, mut order, mut rpo_index, mut bpreds, mut live_in, mut live_before) =
        build_cfg(&empty_switches, &ud);

    // Filter eligible constant-enum switches whose intermediate temp registers
    // (`u_reg`, `c_reg`, `t_reg`) are dead at every case target and default.
    let mut active_switches: FxHashMap<usize, &super::super::EnumSwitchPlan> = FxHashMap::default();
    if !inputs.enum_switches.is_empty() {
        let orig_block_of = |ip: usize| starts.partition_point(|&s| s <= ip) - 1;
        let mut ext_targeted = vec![false; n];
        for (ip, i) in code.iter().enumerate() {
            if let Some(t) = flow(i, ip).1 {
                if t < n {
                    ext_targeted[t] = true;
                }
            }
        }
        for (&start_ip, plan) in inputs.enum_switches {
            let end_ip = start_ip + 4 * plan.n_cases;
            if end_ip >= n
                || generic(start_ip)
                || (plan.upval_idx.is_none() && inputs.slow_global_loads.contains(&plan.global_idx))
                || rpo_index[orig_block_of(start_ip)] == usize::MAX
                || rpoints.iter().any(|&r| r > start_ip && r < end_ip)
                || (start_ip + 1..end_ip).any(|ip| ext_targeted[ip])
            {
                continue;
            }
            let temps_dead_at = |tgt_ip: usize| -> bool {
                let li = &live_in[orig_block_of(tgt_ip)];
                !li.get(plan.c_reg as usize)
                    && !li.get(plan.t_reg as usize)
                    && plan.u_reg.is_none_or(|u| !li.get(u as usize))
            };
            if temps_dead_at(end_ip) && plan.targets.iter().flatten().all(|&t| temps_dead_at(t as usize)) {
                active_switches.insert(start_ip, plan);
            }
        }
        if !active_switches.is_empty() {
            for (&start_ip, &plan) in &active_switches {
                ud[start_ip] = (vec![plan.x], None);
            }
            (leader, starts, bsuccs, order, rpo_index, bpreds, live_in, live_before) =
                build_cfg(&active_switches, &ud);
        }
    }
    let _ = &rpo_index;

    // The `Add`s built as the full `+` (`AddT`, a boxed result): a site that
    // deopted before (`generic`), or one whose operand the same block made a
    // string, a string constant or another such `Add`.
    let mut addt: rustc_hash::FxHashSet<usize> = rustc_hash::FxHashSet::default();
    for (ip, i) in code.iter().enumerate() {
        let Instr::Add { a, b: c, .. } = *i else {
            continue;
        };
        let stringy = |r: u16| {
            for back in (0..ip).rev() {
                let j = &code[back];
                if ud[back].1 == Some(r) {
                    return match *j {
                        Instr::LoadConst { idx, .. } => inputs.const_strs.contains_key(&(idx as u32)),
                        Instr::Add { .. } => addt.contains(&back),
                        _ => false,
                    };
                }
                if leader[back] {
                    return false;
                }
            }
            false
        };
        let fb_other = inputs
            .arith_fb
            .and_then(|fb| fb.get(ip).copied())
            .is_some_and(|b| b & super::super::ARITH_FB_OTHER != 0);
        if generic(ip) || fb_other || stringy(a) || stringy(c) {
            addt.insert(ip);
        }
    }
    // The `>>>`s by a constant 1..=31 the same block loads: an i32.
    let mut ushr_i: rustc_hash::FxHashSet<usize> = rustc_hash::FxHashSet::default();
    for (ip, i) in code.iter().enumerate() {
        let Instr::Bitwise {
            op: BitwiseOp::Ushr,
            b: c,
            ..
        } = *i
        else {
            continue;
        };
        for back in (0..ip).rev() {
            if ud[back].1 == Some(c) {
                let k = match code[back] {
                    Instr::LoadInt { val, .. } => Some(val),
                    Instr::LoadConst { idx, .. } if p.constants[idx as usize].is_int() => {
                        Some(p.constants[idx as usize].as_int())
                    }
                    _ => None,
                };
                if k.is_some_and(|k| k & 31 != 0) {
                    ushr_i.insert(ip);
                }
                break;
            }
            if leader[back] {
                break;
            }
        }
    }
    let block_of_ip = |ip: usize| starts.partition_point(|&s| s <= ip) - 1;
    let nb = starts.len();
    let end_of = |b: usize| if b + 1 < nb { starts[b + 1] } else { n };

    // Parameters (including `r == 0`, `this`) that are never written anywhere
    // in the function already live boxed in `[rbx + 8*r]` for the entire
    // activation: excluding them from `FrameState::regs` eliminates redundant
    // window spills at every call-out and frees callee-saved GPRs.
    let mut written = vec![false; nregs];
    for (ip, ins) in code.iter().enumerate() {
        if let Some(d) = ud[ip].1 {
            written[d as usize] = true;
        }
        match *ins {
            Instr::IterNext { done_dst, idx, .. } => {
                written[done_dst as usize] = true;
                written[idx as usize] = true;
            }
            Instr::PushHandler { catch_reg, .. } => {
                written[catch_reg as usize] = true;
            }
            Instr::PushFinally { kind_reg, val_reg, .. } => {
                written[kind_reg as usize] = true;
                written[val_reg as usize] = true;
            }
            _ => {}
        }
    }
    let mut readonly_param = vec![false; nregs];
    for r in 0..=(p.param_count as usize).min(nregs.saturating_sub(1)) {
        if !written[r] {
            readonly_param[r] = true;
        }
    }

    // ── parameter speculation ── a parameter the body only computes with
    // (read by a Number op, never as an object or call operand, never moved,
    // written only with Numbers) is unboxed once at entry, behind a guard
    // that resumes the call at its first op. Unboxed at each use instead, a
    // loop paid the guard every iteration, and a loop variable seeded with
    // it merged a box with Numbers at its phi and was reboxed every
    // iteration.
    let mut spec = vec![false; nregs];
    for r in 1..=(p.param_count as usize).min(nregs.saturating_sub(1)) {
        let r16 = r as u16;
        let mut numeric_read = false;
        let mut ok = true;
        for (ip, i) in code.iter().enumerate() {
            if ud[ip].0.contains(&r16) {
                match *i {
                    Instr::Add { .. } if addt.contains(&ip) => ok = false,
                    Instr::Add { .. }
                    | Instr::Sub { .. }
                    | Instr::Mul { .. }
                    | Instr::Div { .. }
                    | Instr::Mod { .. }
                    | Instr::Lt { .. }
                    | Instr::Le { .. }
                    | Instr::Gt { .. }
                    | Instr::Ge { .. }
                    | Instr::Bitwise { .. }
                    | Instr::JumpIfNotLt { .. }
                    | Instr::JumpIfNotLe { .. }
                    | Instr::AddInt { .. }
                    | Instr::Neg { .. } => numeric_read = true,
                    Instr::MathOp { callee, this_v, .. }
                        if callee != r16 && (callee == crate::bytecode::NO_REG || this_v != r16) =>
                    {
                        numeric_read = true
                    }
                    // Any representation: a boxed copy, a truthiness, `===`.
                    Instr::Eq { .. }
                    | Instr::Ne { .. }
                    | Instr::Not { .. }
                    | Instr::JumpIfFalse { .. }
                    | Instr::JumpIfTrue { .. }
                    | Instr::Return { .. }
                    | Instr::StoreGlobal { .. }
                    | Instr::StoreGlobalStrict { .. }
                    | Instr::UpvalSet { .. } => {}
                    Instr::SetProp { obj, .. } if obj != r16 => {}
                    Instr::GetIndex { obj, .. } | Instr::SetIndex { obj, .. } if obj != r16 => {}
                    // An object, a callee, a move, an argument.
                    _ => ok = false,
                }
            }
            if let Some(d) = ud[ip].1 {
                let numeric_write = match *i {
                    Instr::LoadConst { idx, .. } => p.constants[idx as usize].is_number(),
                    _ => matches!(
                        i,
                        Instr::Add { .. }
                            | Instr::Sub { .. }
                            | Instr::Mul { .. }
                            | Instr::Div { .. }
                            | Instr::Mod { .. }
                            | Instr::AddInt { .. }
                            | Instr::Neg { .. }
                            | Instr::Bitwise { .. }
                            | Instr::LoadInt { .. }
                            | Instr::MathOp { .. }
                    ),
                };
                if d == r16 && !numeric_write {
                    ok = false;
                }
            }
        }
        spec[r] = ok
            && numeric_read
            && !is_generic(inputs.generic, ENTRY_SITE)
            && live_before[0].as_ref().is_some_and(|l| l.get(r));
    }

    // ── representations ── each live register's at each block entry. An
    // inlined call's is its callee's result's, given its arguments'.
    let call_rep = |ip: usize, reps: &[Rep]| -> Rep {
        let Some(&(callee, site)) = inline_at.get(&ip) else {
            return Rep::Tagged;
        };
        let c = callee.proto;
        // The inlined body's `this` is boxed (the receiver, or undefined).
        let (arg_base, argc) = match &code[ip] {
            Instr::CallMethod { arg_base, argc, .. } if site.method() => (*arg_base, *argc),
            Instr::Call { arg_base, argc, .. } | Instr::CallWithThis { arg_base, argc, .. } if !site.method() => (*arg_base, *argc),
            Instr::GetProp { .. } => (0, 0),
            _ => return Rep::Tagged,
        };
        let mut creps = vec![Rep::Tagged; c.reg_count as usize];
        for k in 0..(argc as usize).min(c.param_count as usize) {
            creps[1 + k] = reps[arg_base as usize + k];
        }
        if let Some((r, _)) = inline_sim(callee, creps.clone(), argc as usize) {
            r
        } else {
            inline_cfg_ret_rep(callee, creps, inputs.callees, 0).unwrap_or(Rep::Tagged)
        }
    };
    // `None`: not yet reached.
    let mut in_rep: Vec<Option<Vec<Rep>>> = vec![None; nb];
    in_rep[0] = Some((0..nregs).map(|r| if spec[r] { Rep::F64 } else { Rep::Tagged }).collect());
    let mut changed = true;
    while changed {
        changed = false;
        for &b in &order {
            let Some(mut reps) = in_rep[b].clone() else {
                continue;
            };
            for ip in starts[b]..end_of(b) {
                if active_switches.contains_key(&ip) {
                    break;
                }
                let call = call_rep(ip, &reps);
                let fb_dbl = inputs
                    .arith_fb
                    .and_then(|fb| fb.get(ip).copied())
                    .is_some_and(|b| b & super::super::ARITH_FB_DOUBLE != 0);
                let over = if addt.contains(&ip) {
                    Some(Rep::Tagged)
                } else if ushr_i.contains(&ip) {
                    Some(Rep::I32)
                } else if !generic(ip)
                    && matches!(code[ip], Instr::GetProp { .. })
                    && inputs.const_props.contains_key(&ip)
                {
                    Some(const_prop_rep(inputs.const_props[&ip].2))
                } else if (generic(ip) || fb_dbl)
                    && matches!(code[ip], Instr::Add { .. } | Instr::Sub { .. } | Instr::AddInt { .. })
                {
                    Some(Rep::F64)
                } else {
                    None
                };
                if let (Some(r), (_, Some(d))) = (result_rep(&code[ip], &reps, p, call, over), &ud[ip]) {
                    reps[*d as usize] = r;
                }
                // `IterNext`'s other writes come back boxed from the window.
                if let Instr::IterNext { done_dst, idx, .. } = code[ip] {
                    reps[done_dst as usize] = Rep::Tagged;
                    reps[idx as usize] = Rep::Tagged;
                }
            }
            for &s in &bsuccs[b] {
                match &mut in_rep[s] {
                    None => {
                        in_rep[s] = Some(reps.clone());
                        changed = true;
                    }
                    Some(cur) => {
                        for r in 0..nregs {
                            if live_in[s].get(r) {
                                let j = Rep::join(cur[r], reps[r]);
                                if j != cur[r] {
                                    cur[r] = j;
                                    changed = true;
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // ── SSA ──
    let mut b = Builder {
        insts: Vec::new(),
        states: Vec::new(),
        blocks: Vec::with_capacity(order.len() + 2),
        cur_block: 0,
        cur_preds: Vec::new(),
        memo: Vec::new(),
        out: Vec::new(),
        consts: Vec::new(),
        inlined: 0,
        inlined_callees: Vec::new(),
        math: inputs.math,
        heap: *inputs.heap,
        shape_memo: Vec::new(),
        atom_memo: Vec::new(),
        const_obj_memo: Vec::new(),
        global_memo: Vec::new(),
    };
    // Identify shaped GetProp/SetProp sites that share `(obj, shape)` with at
    // least one other shaped access in the same basic block before a call/reshape
    // or overwrite of `obj`. Only those sites use deopting `ShapeVals` CSE;
    // isolated sites emit non-deopting `GetPropShape`/`SetPropShape`.
    let mut multi_prop = vec![false; n];
    for &bb in &order {
        let mut seen: Vec<(u16, u32, usize)> = Vec::new();
        for ip in starts[bb]..end_of(bb) {
            match code[ip] {
                Instr::GetProp { obj, .. }
                    if !inputs.poly.contains_key(&ip)
                        && !top_reads.contains_key(&ip)
                        && !inputs.const_props.contains_key(&ip) =>
                {
                    if let Some(&(shape, _)) = inputs.prop_shape.get(&ip) {
                        if let Some(&(_, _, first)) = seen.iter().find(|&&(r, sh, _)| r == obj && sh == shape) {
                            multi_prop[first] = true;
                            multi_prop[ip] = true;
                        } else {
                            seen.push((obj, shape, ip));
                        }
                    } else {
                        seen.clear();
                    }
                }
                Instr::SetProp { obj, .. } => {
                    if let Some(&(shape, _)) = inputs.prop_shape.get(&ip) {
                        if let Some(&(_, _, first)) = seen.iter().find(|&&(r, sh, _)| r == obj && sh == shape) {
                            multi_prop[first] = true;
                            multi_prop[ip] = true;
                        } else {
                            seen.push((obj, shape, ip));
                        }
                    } else {
                        seen.clear();
                    }
                }
                Instr::Call { .. }
                | Instr::CallMethod { .. }
                | Instr::CallWithThis { .. }
                | Instr::CallMethodComputed { .. }
                | Instr::New { .. }
                | Instr::StaticFn { .. }
                | Instr::GlobalFn { .. }
                | Instr::IterNext { .. }
                | Instr::NewObject { .. }
                | Instr::NewArray { .. }
                | Instr::FinalizeObject { .. }
                | Instr::MakeClosure { .. }
                | Instr::MakeArrow { .. }
                | Instr::MakeFunc { .. }
                | Instr::MakeCell { .. }
                | Instr::MakeCellTdz { .. }
                | Instr::MakeCellFnName { .. }
                | Instr::SetIndex { .. }
                | Instr::GetIndex { .. } => {
                    seen.clear();
                }
                _ => {}
            }
            if let Some(d) = ud[ip].1 {
                seen.retain(|&(r, _, _)| r != d);
            }
        }
    }
    let top_ta = ta_regs(code);
    let top = Fn {
        p,
        is_top: true,
        func_id: func_id as u64,
        trunc: super::super::emit_misc::trunc_only_arith_ips(p),
        handlers: super::super::proto_has_handler_ops(p),
        plan: inputs.prop_shape,
        strs: Some(inputs.const_strs),
        math,
        math_reads: top_reads.clone(),
        generic: inputs.generic,
        addt: Some(&addt),
        ushr_i: Some(&ushr_i),
        new_lanes: Some(inputs.new_lanes),
        ta_regs: &top_ta,
        class_values: Some((inputs.class_values, inputs.class_epoch)),
        super_parents: Some((inputs.super_parents, inputs.class_epoch)),
        poly: Some(inputs.poly),
        const_props: Some(inputs.const_props),
        slow_global_loads: Some(inputs.slow_global_loads),
        callee_sites: None,
        callees: inputs.callees,
        inline_depth: 0,
        arith_fb: inputs.arith_fb,
        plain_closures: Some(inputs.plain_closures),
        multi_prop: &multi_prop,
        heap: inputs.heap,
    };
    // A loop may jump back to bytecode ip 0. In that case the bytecode
    // entry is a loop header, not a unique predecessor-free entry block.
    // Split out a synthetic SSA preheader so its first invocation and its
    // back-edges enter through distinct phi inputs.
    let entry_preheader = !bpreds[0].is_empty();
    let mut bb_entry: Vec<B> = vec![B::MAX; nb];
    let mut bb_exit: Vec<B> = vec![B::MAX; nb];
    let mut exit_defs: Vec<Vec<V>> = Vec::with_capacity(order.len() + 2);
    let mut exit_shapes: Vec<Vec<(V, u32, V)>> = Vec::with_capacity(order.len() + 2);
    let mut exit_atoms: Vec<Vec<(V, V)>> = Vec::with_capacity(order.len() + 2);
    let mut exit_const_objs: Vec<Vec<(V, u32, u32)>> = Vec::with_capacity(order.len() + 2);
    let mut exit_globals: Vec<Vec<(u32, V)>> = Vec::with_capacity(order.len() + 2);
    let mut pending_top_preds: Vec<(B, Vec<usize>, bool)> = Vec::with_capacity(order.len());
    let mut entry_states: Vec<(B, (u32, Vec<(u16, V)>))> = Vec::new();
    enum TopTerm {
        Jump(usize),
        Branch { cond: V, t: usize, f: usize },
        Switch {
            val: V,
            min_val: i32,
            targets: Vec<Option<usize>>,
            default: usize,
        },
        Return(V),
        Exit,
    }
    let mut pending_top_terms: Vec<(B, TopTerm)> = Vec::with_capacity(order.len());
    // Phis to fill once every block is built: (ir_block, phi, register).
    let mut phis: Vec<(B, V, u16)> = Vec::new();
    let undefined_bits = Value::UNDEFINED.bits();
    let make_entry = |b: &mut Builder| -> (Vec<V>, Vec<V>) {
        let mut defs: Vec<V> = vec![V::MAX; nregs];
        let mut insts = Vec::new();
        let undef = b.konst(Op::ConstTagged(undefined_bits), Rep::Tagged);
        for (r, d) in defs.iter_mut().enumerate() {
            *d = if r <= p.param_count as usize {
                let v = b.push(Op::Param(r as u16), Rep::Tagged, None);
                insts.push(v);
                v
            } else {
                undef
            };
        }
        // The speculated parameters are unboxed once at the true function
        // entry, not again when a loop returns to bytecode ip 0.
        let entry_live = live_before[0].as_ref().expect("the entry has liveness");
        let unbox: Vec<usize> = (0..nregs).filter(|&r| spec[r]).collect();
        if !unbox.is_empty() {
            let s = b.states.len() as u32;
            b.states.push(FrameState {
                ip: 0,
                why: ENTRY_SITE,
                regs: (0..nregs)
                    .filter(|&r| entry_live.get(r))
                    .map(|r| (r as u16, defs[r]))
                    .collect(),
            });
            for r in unbox {
                let c = b.push(Op::TaggedToF64(defs[r]), Rep::F64, Some(s));
                insts.push(c);
                defs[r] = c;
            }
        }
        (defs, insts)
    };
    if entry_preheader {
        b.cur_block = 0;
        let (defs, insts) = make_entry(&mut b);
        exit_defs.push(defs);
        exit_shapes.push(Vec::new());
        exit_atoms.push(Vec::new());
        exit_const_objs.push(Vec::new());
        exit_globals.push(Vec::new());
        b.blocks.push(Block {
            insts,
            term: Term::Jump(1),
            preds: Vec::new(),
            entry_state: None,
        });
    }
    for (original_k, &bb) in order.iter().enumerate() {
        let entry_id = b.blocks.len() as B;
        bb_entry[bb] = entry_id;
        b.cur_block = entry_id;
        b.cur_preds.clear();
        b.memo.clear();
        b.shape_memo.clear();
        b.atom_memo.clear();
        b.const_obj_memo.clear();
        b.global_memo.clear();
        let has_pre = entry_preheader && original_k == 0;
        pending_top_preds.push((entry_id, bpreds[bb].clone(), has_pre));
        let mut defs: Vec<V> = vec![V::MAX; nregs];
        let mut insts_of_block: Vec<V> = Vec::new();
        if entry_id == 0 {
            debug_assert!(bpreds[bb].is_empty() && !has_pre, "the entry has no predecessor");
            (defs, insts_of_block) = make_entry(&mut b);
        } else if !has_pre && bpreds[bb].len() == 1 && bb_exit[bpreds[bb][0]] != B::MAX {
            let pred_exit = bb_exit[bpreds[bb][0]] as usize;
            defs = exit_defs[pred_exit].clone();
            b.shape_memo = exit_shapes[pred_exit].clone();
            b.atom_memo = exit_atoms[pred_exit].clone();
            b.const_obj_memo = exit_const_objs[pred_exit].clone();
            b.global_memo = exit_globals[pred_exit].clone();
        } else {
            let reps = in_rep[bb]
                .as_ref()
                .expect("a reachable block has entry representations");
            for r in 0..nregs {
                if live_in[bb].get(r) {
                    let phi = b.push(Op::Phi(Vec::new()), reps[r], None);
                    insts_of_block.push(phi);
                    defs[r] = phi;
                    phis.push((entry_id, phi, r as u16));
                }
            }
        }
        b.out = std::mem::take(&mut insts_of_block);
        if bpreds[bb].iter().any(|&p| rpo_index[p] >= original_k) {
            let start_ip = starts[bb];
            if let Some(live) = live_before[start_ip].as_ref() {
                let mut regs = Vec::new();
                live.for_each_set(|r| {
                    if !readonly_param[r] {
                        regs.push((r as u16, defs[r]));
                    }
                });
                entry_states.push((entry_id, (start_ip as u32, regs)));
            }
        }
        let mut term: Option<TopTerm> = None;
        for ip in starts[bb]..end_of(bb) {
            let live = live_before[ip].as_ref().expect("every reachable position has liveness");
            let mut fs = FsCtx {
                ip: ip as u32,
                live: Some(live),
                readonly_param: &readonly_param,
                id: None,
            };
            if let Some(&plan) = active_switches.get(&ip) {
                let s = fs.id(&mut b, &defs);
                let enum_obj = if let Some(up) = plan.upval_idx {
                    b.push_out(Op::UpvalGet(up), Rep::Tagged, Some(s))
                } else {
                    b.load_global(plan.global_idx, s)
                };
                b.guard_const_obj(enum_obj, plan.enum_obj_idx, plan.enum_obj_ver, s);
                let targets: Vec<Option<usize>> = plan
                    .targets
                    .iter()
                    .map(|opt| opt.map(|t| block_of_ip(t as usize)))
                    .collect();
                term = Some(TopTerm::Switch {
                    val: defs[plan.x as usize],
                    min_val: plan.min_val,
                    targets,
                    default: block_of_ip(ip + 4 * plan.n_cases),
                });
                break;
            }
            let inline = inline_at.get(&ip).copied();
            match translate(&mut b, &top, ip, &code[ip], &mut defs, &mut fs, inline)? {
                Flow::Next => {}
                Flow::Jump(t) => term = Some(TopTerm::Jump(block_of_ip(t))),
                Flow::Branch { cond, t, f } => {
                    term = Some(TopTerm::Branch {
                        cond,
                        t: block_of_ip(t),
                        f: block_of_ip(f),
                    })
                }
                Flow::Ret(v) => {
                    let v = b.tagged(v);
                    term = Some(TopTerm::Return(v));
                }
                Flow::Exit => term = Some(TopTerm::Exit),
            }
        }
        let top_term = term.unwrap_or_else(|| TopTerm::Jump(block_of_ip(end_of(bb))));
        let exit_id = b.cur_block;
        debug_assert_eq!(exit_id as usize, b.blocks.len());
        bb_exit[bb] = exit_id;
        pending_top_terms.push((exit_id, top_term));
        b.blocks.push(Block {
            insts: std::mem::take(&mut b.out),
            term: Term::Exit,
            preds: std::mem::take(&mut b.cur_preds),
            entry_state: None,
        });
        if exit_defs.len() < b.blocks.len() {
            exit_defs.resize_with(b.blocks.len(), Vec::new);
            exit_shapes.resize_with(b.blocks.len(), Vec::new);
            exit_atoms.resize_with(b.blocks.len(), Vec::new);
            exit_const_objs.resize_with(b.blocks.len(), Vec::new);
            exit_globals.resize_with(b.blocks.len(), Vec::new);
        }
        exit_defs[exit_id as usize] = defs;
        exit_shapes[exit_id as usize] = std::mem::take(&mut b.shape_memo);
        exit_atoms[exit_id as usize] = std::mem::take(&mut b.atom_memo);
        exit_const_objs[exit_id as usize] = std::mem::take(&mut b.const_obj_memo);
        exit_globals[exit_id as usize] = std::mem::take(&mut b.global_memo);
    }
    for (entry_id, top_preds, has_pre) in pending_top_preds {
        let mut preds = Vec::with_capacity(top_preds.len() + usize::from(has_pre));
        if has_pre {
            preds.push(0);
        }
        for p in top_preds {
            preds.push(bb_exit[p]);
        }
        b.blocks[entry_id as usize].preds = preds;
    }
    for (entry_id, st) in entry_states {
        b.blocks[entry_id as usize].entry_state = Some(st);
    }
    for (exit_id, top_term) in pending_top_terms {
        b.blocks[exit_id as usize].term = match top_term {
            TopTerm::Jump(t) => Term::Jump(bb_entry[t]),
            TopTerm::Branch { cond, t, f } => Term::Branch {
                cond,
                t: bb_entry[t],
                f: bb_entry[f],
            },
            TopTerm::Switch {
                val,
                min_val,
                targets,
                default,
            } => Term::Switch {
                val,
                min_val,
                targets: targets.into_iter().map(|opt| opt.map(|t| bb_entry[t])).collect(),
                default: bb_entry[default],
            },
            TopTerm::Return(v) => Term::Return(v),
            TopTerm::Exit => Term::Exit,
        };
    }
    // ── phis ── each predecessor's value, widened to the phi's
    // representation before the predecessor's terminator.
    for (k, phi, r) in phis {
        let k = k as usize;
        let mut inputs = Vec::with_capacity(b.blocks[k].preds.len());
        for pi in 0..b.blocks[k].preds.len() {
            let pred = b.blocks[k].preds[pi] as usize;
            let v = exit_defs[pred][r as usize];
            debug_assert!(v != V::MAX, "a live register is defined on every path");
            let want = b.insts[phi as usize].rep;
            let have = b.insts[v as usize].rep;
            let v = if have == want {
                v
            } else {
                let op = match (have, want) {
                    (Rep::I32, Rep::F64) => Op::I32ToF64(v),
                    (_, Rep::Tagged) => Op::Box(v),
                    _ => unreachable!("a phi only widens: {have:?} to {want:?}"),
                };
                let c = b.push(op, want, None);
                b.insts[c as usize].block = pred as B;
                b.blocks[pred].insts.push(c);
                c
            };
            inputs.push(v);
        }
        b.insts[phi as usize].op = Op::Phi(inputs);
    }
    // Only non-rematerialized constants (`ConstF64`) open the entry block;
    // `ConstI32`/`ConstBool`/`ConstTagged` are rematerialized at uses (`Loc::Imm`).
    let mut entry: Vec<V> = b
        .consts
        .iter()
        .filter_map(|&(_, _, v)| (!b.insts[v as usize].op.is_imm()).then_some(v))
        .collect();
    entry.append(&mut b.blocks[0].insts);
    b.blocks[0].insts = entry;
    let plain_closures = inputs
        .plain_closures
        .iter()
        .map(|(&ip, p)| (ip as u32, p.clone()))
        .collect();
    Some(Func {
        func_id,
        insts: b.insts,
        blocks: b.blocks,
        states: b.states,
        inlined: b.inlined,
        math: inputs.math,
        gen_lane: false,
        body_gen: 0,
        ta_records: top_ta.len() as u8,
        cross: FxHashMap::default(),
        heap: None,
        reg_count: p.reg_count.max(1),
        captured: FxHashMap::default(),
        ic_sites: FxHashMap::default(),
        class_read: FxHashMap::default(),
        readonly_param,
        inlined_callees: b.inlined_callees,
        plain_closures,
    })
}

/// One op of `f` at `ip` into the current block. `inline`: the call site's
/// callee, inlined. `None` when the op is not one Tier O builds.
fn translate(
    b: &mut Builder,
    f: &Fn,
    ip: usize,
    ins: &Instr,
    defs: &mut [V],
    fs: &mut FsCtx,
    inline: Option<(&super::OptCallee, Site)>,
) -> Option<Flow> {
    let undefined_bits = Value::UNDEFINED.bits();
    match *ins {
        Instr::LoadInt { dst, val } => defs[dst as usize] = b.konst(Op::ConstI32(val), Rep::I32),
        Instr::LoadConst { dst, idx } => {
            let c = f.p.constants[idx as usize];
            defs[dst as usize] = if c.is_int() {
                b.konst(Op::ConstI32(c.as_int()), Rep::I32)
            } else if c.is_number() {
                b.konst(Op::ConstF64(c.as_f64()), Rep::F64)
            } else {
                // A string the VM roots for its life.
                let bits = *f.strs?.get(&(idx as u32))?;
                b.konst(Op::ConstTagged(bits), Rep::Tagged)
            };
        }
        Instr::UpvalGet { dst, idx } => {
            let s = fs.id(b, defs);
            defs[dst as usize] = b.push_out(Op::UpvalGet(idx), Rep::Tagged, Some(s));
        }
        Instr::StoreGlobal { idx, src } | Instr::StoreGlobalStrict { idx, src } => {
            let x = b.tagged(defs[src as usize]);
            let s = fs.id(b, defs);
            b.push_out(Op::StoreGlobal(idx, x), Rep::Void, Some(s));
        }
        Instr::SetProp { obj, name, val, .. } => {
            let s = fs.id(b, defs);
            let packed = ((name as u64) << 32) | ((obj as u64) << 16) | val as u64;
            match f.plan.get(&ip) {
                Some(&(shape, slot)) => {
                    let o = b.tagged(defs[obj as usize]);
                    let x = b.tagged(defs[val as usize]);
                    if !f.generic(ip) && (f.multi_prop.get(ip).copied().unwrap_or(false) || b.has_shape_memo(o, shape)) {
                        let vals = b.shape_vals(o, shape, s);
                        b.push_out(Op::StoreSlot { vals, obj: o, val: x, slot }, Rep::Void, None);
                    } else {
                        b.push_out(
                            Op::SetPropShape {
                                obj: o,
                                val: x,
                                shape,
                                slot,
                                ip: ip as u32,
                                packed,
                            },
                            Rep::Void,
                            Some(s),
                        );
                    }
                }
                None => {
                    let obj_v = defs[obj as usize];
                    let val_v = defs[val as usize];
                    b.push_out(
                        Op::CallOut {
                            helper: helper(crate::vm::jit_set_prop_slow as *const ()),
                            ip: ip as u32,
                            packed,
                            argc: 0,
                            lane: CallLane::SetProp {
                                obj,
                                obj_v,
                                val,
                                val_v,
                                name,
                            },
                        },
                        Rep::Void,
                        Some(s),
                    );
                }
            }
        }
        Instr::New {
            dst,
            callee,
            arg_base,
            argc,
        } => {
            let s = fs.id(b, defs);
            let packed = ((callee as u64) << 16) | arg_base as u64;
            let op = match f.new_lanes.and_then(|m| m.get(&ip)) {
                Some(&lane) => Op::NewLane {
                    lane,
                    callee,
                    arg_base,
                    dst,
                    ip: ip as u32,
                    packed,
                    argc,
                },
                None => Op::CallOut {
                    helper: helper(crate::vm::jit_new_ic as *const ()),
                    ip: ip as u32,
                    packed,
                    argc,
                    lane: CallLane::None,
                },
            };
            defs[dst as usize] = b.push_out(op, Rep::Tagged, Some(s));
        }
        Instr::LoadClassValue { dst, class_id } if !f.generic(ip) && f.class_values.is_some_and(|(m, _)| m.contains_key(&class_id)) => {
            let (m, epoch) = f.class_values.expect("checked");
            let s = fs.id(b, defs);
            defs[dst as usize] = b.push_out(Op::ClassConst { bits: m[&class_id], epoch }, Rep::Tagged, Some(s));
        }
        Instr::LoadClassValue { dst, class_id } => {
            let s = fs.id(b, defs);
            defs[dst as usize] = b.push_out(
                Op::CallOut {
                    helper: helper(crate::vm::jit_load_class_value_ic as *const ()),
                    ip: ip as u32,
                    packed: class_id as u64 | crate::vm::LOAD_CLASS_FROM_TIER_C,
                    argc: 0,
                    lane: CallLane::None,
                },
                Rep::Tagged,
                Some(s),
            );
        }
        Instr::Mod { dst, a, b: c } => {
            let x = b.f64(defs[a as usize], fs, defs);
            let y = b.f64(defs[c as usize], fs, defs);
            defs[dst as usize] = b.emit(Op::ModF(x, y), Rep::F64);
        }
        Instr::MathOp {
            dst,
            op,
            callee,
            this_v,
            arg_base,
            argc,
        } => {
            let g = f.math?;
            let s = fs.id(b, defs);
            if callee == crate::bytecode::NO_REG {
                b.push_out(
                    Op::MathGuard {
                        recv: None,
                        gidx: this_v as u32,
                        op,
                    },
                    Rep::Void,
                    Some(s),
                );
            } else {
                // The callee a fused `Math.<op>` read made is proven already.
                let want = g.op_callee_bits[op as usize];
                let cv = defs[callee as usize];
                if !matches!(b.insts[cv as usize].op, Op::ConstTagged(bits) if bits == want) {
                    let cv = b.tagged(cv);
                    b.push_out(Op::GuardCallee { v: cv, op }, Rep::Void, Some(s));
                }
            }
            let x = defs[arg_base as usize];
            defs[dst as usize] = if argc == 2 {
                let y = defs[arg_base as usize + 1];
                if matches!(op, MathFn::Imul) {
                    let xi = b.i32(x, fs, defs);
                    let yi = b.i32(y, fs, defs);
                    b.emit(Op::ImulI(xi, yi), Rep::I32)
                } else {
                    let xf = b.f64(x, fs, defs);
                    let yf = b.f64(y, fs, defs);
                    b.emit(Op::MathCall2(op, xf, yf), Rep::F64)
                }
            } else {
                let xf = b.f64(x, fs, defs);
                if math_f1_inline(op) {
                    b.emit(Op::MathF1(op, xf), Rep::F64)
                } else {
                    b.emit(Op::MathCall1(op, xf), Rep::F64)
                }
            };
        }
        Instr::LoadBool { dst, val } => defs[dst as usize] = b.konst(Op::ConstBool(val), Rep::Bool),
        Instr::LoadNull { dst } => defs[dst as usize] = b.konst(Op::ConstTagged(Value::NULL.bits()), Rep::Tagged),
        Instr::LoadUndefined { dst } => defs[dst as usize] = b.konst(Op::ConstTagged(undefined_bits), Rep::Tagged),
        Instr::LoadGlobal { dst, idx } => {
            let s = fs.id(b, defs);
            defs[dst as usize] = if f.slow_global_loads.is_some_and(|set| set.contains(&idx)) {
                b.push_out(Op::LoadGlobalSlow { idx, ip: ip as u32 }, Rep::Tagged, Some(s))
            } else {
                b.load_global(idx, s)
            };
        }
        Instr::Move { dst, src } => defs[dst as usize] = defs[src as usize],
        Instr::Add { dst, a, b: c } if f.addt.is_some_and(|s| s.contains(&ip)) => {
            let x = b.tagged(defs[a as usize]);
            let y = b.tagged(defs[c as usize]);
            let s = fs.id(b, defs);
            defs[dst as usize] = b.push_out(Op::AddT(x, y), Rep::Tagged, Some(s));
        }
        Instr::UpvalSet { idx, src } => {
            let x = b.tagged(defs[src as usize]);
            let s = fs.id(b, defs);
            b.push_out(Op::UpvalSet(idx, x), Rep::Void, Some(s));
        }
        Instr::GetIterator { dst, src } | Instr::IterPrime { dst, iter: src } => {
            let x = b.tagged(defs[src as usize]);
            let s = fs.id(b, defs);
            let h = if matches!(*ins, Instr::GetIterator { .. }) {
                crate::vm::jit_get_iterator as *const ()
            } else {
                crate::vm::jit_iter_prime as *const ()
            };
            defs[dst as usize] = b.push_out(
                Op::IterPure {
                    helper: helper(h),
                    src: x,
                },
                Rep::Tagged,
                Some(s),
            );
        }
        Instr::IterNext {
            value_dst,
            done_dst,
            iter,
            idx,
            next,
        } => {
            let s = fs.id(b, defs);
            let packed = (iter as u64) << 48 | (next as u64) << 32 | (value_dst as u64) << 16 | done_dst as u64;
            b.push_out(
                Op::IterStep {
                    ip: ip as u32,
                    packed,
                    value_dst,
                    done_dst,
                    iter,
                    idx,
                },
                Rep::Void,
                Some(s),
            );
            for r in [value_dst, done_dst, idx] {
                defs[r as usize] = b.push_out(Op::WindowLoad(r), Rep::Tagged, None);
            }
        }
        Instr::NewArray { dst, arg_base, argc } => {
            let s = fs.id(b, defs);
            let window = u64::from(f.p.reg_count.max(1));
            defs[dst as usize] = b.push_out(
                Op::AllocOut {
                    helper: helper(crate::vm::jit_new_array as *const ()),
                    ip: ip as u32,
                    a: window << 32 | (arg_base as u64) << 16 | argc as u64,
                    b: 0,
                },
                Rep::Tagged,
                Some(s),
            );
        }
        Instr::FinalizeObject {
            dst,
            plan,
            val_base,
            count,
        } => {
            let s = fs.id(b, defs);
            let window = u64::from(f.p.reg_count.max(1));
            // Tier C's baked form (`proto_mem`'s `FinalizeObject`): the
            // plan's keys folded into the shape it makes.
            let baked = f.p.static_key_plans.get(plan as usize).filter(|pl| {
                super::super::finalize_baked_enabled()
                    && crate::heap::thin_alloc_enabled()
                    && pl.runtime_valid()
                    && pl.len() == count as usize
                    && !pl.has_element_key()
                    && (count as usize) < crate::heap::PROP_INDEX_THRESHOLD
                    && val_base as usize + count as usize <= f.p.reg_count as usize
            });
            if let Some(pl) = baked {
                let data = crate::shape::attr_bits(true, true, true, false);
                let mut shape = crate::shape::EMPTY;
                for key in pl.keys() {
                    shape = crate::shape::add(shape, key, data);
                }
                defs[dst as usize] = b.push_out(
                    Op::Literal {
                        plan_ptr: pl as *const crate::bytecode::StaticKeyPlan as u64,
                        shape,
                        val_base,
                        count,
                        dst,
                        ip: ip as u32,
                    },
                    Rep::Tagged,
                    Some(s),
                );
                return Some(Flow::Next);
            }
            defs[dst as usize] = b.push_out(
                Op::AllocOut {
                    helper: helper(crate::vm::jit_finalize_object as *const ()),
                    ip: ip as u32,
                    a: f.func_id << 32 | plan as u64,
                    b: window << 32 | (val_base as u64) << 16 | count as u64,
                },
                Rep::Tagged,
                Some(s),
            );
        }
        Instr::GetIndex { dst, obj, key } => {
            let o = b.tagged(defs[obj as usize]);
            let k = b.index_key(defs[key as usize]);
            let s = fs.id(b, defs);
            defs[dst as usize] = b.push_out(
                Op::GetIndex {
                    obj: o,
                    key: k,
                    ip: ip as u32,
                    packed: obj as u64 | (key as u64) << 16,
                    ta: if f.is_top {
                        f.ta_regs.iter().position(|&r| r == obj).map(|k| k as u8)
                    } else {
                        None
                    },
                    inlined: !f.is_top,
                },
                Rep::Tagged,
                Some(s),
            );
        }
        Instr::SetIndex { obj, key, val } => {
            let o = b.tagged(defs[obj as usize]);
            let k = b.index_key(defs[key as usize]);
            let x = b.tagged(defs[val as usize]);
            let s = fs.id(b, defs);
            b.push_out(
                Op::SetIndex {
                    obj: o,
                    key: k,
                    val: x,
                    ip: ip as u32,
                    packed: obj as u64 | (key as u64) << 16 | (val as u64) << 32 | 1 << 56,
                    ta: f.ta_regs.iter().position(|&r| r == obj).map(|k| k as u8),
                },
                Rep::Void,
                Some(s),
            );
        }
        Instr::Add { dst, a, b: c } | Instr::Sub { dst, a, b: c }
            if !f.generic(ip) && !f.arith_double(ip) && b.rep(defs[a as usize]) == Rep::I32 && b.rep(defs[c as usize]) == Rep::I32 =>
        {
            // Small-integer speculation: an overflow resumes the interpreter
            // here, and the site is a double's next time (`generic`).
            let (x, y) = (defs[a as usize], defs[c as usize]);
            let wrap = f.trunc.get(ip).copied().unwrap_or(false);
            let op = if matches!(*ins, Instr::Add { .. }) {
                Op::AddI(x, y, wrap)
            } else {
                Op::SubI(x, y, wrap)
            };
            let s = fs.id(b, defs);
            defs[dst as usize] = b.push_out(op, Rep::I32, Some(s));
        }
        Instr::AddInt { dst, a, imm, .. } if !f.generic(ip) && !f.arith_double(ip) && b.rep(defs[a as usize]) == Rep::I32 => {
            let s = fs.id(b, defs);
            let wrap = f.trunc.get(ip).copied().unwrap_or(false);
            defs[dst as usize] = b.push_out(Op::AddIK(defs[a as usize], imm, wrap), Rep::I32, Some(s));
        }
        Instr::Add { dst, a, b: c } | Instr::Sub { dst, a, b: c } | Instr::Mul { dst, a, b: c } | Instr::Div { dst, a, b: c } => {
            let x = b.f64(defs[a as usize], fs, defs);
            let y = b.f64(defs[c as usize], fs, defs);
            let op = match *ins {
                Instr::Add { .. } => Op::AddF(x, y),
                Instr::Sub { .. } => Op::SubF(x, y),
                Instr::Mul { .. } => Op::MulF(x, y),
                _ => Op::DivF(x, y),
            };
            defs[dst as usize] = b.emit(op, Rep::F64);
        }
        Instr::AddInt { dst, a, imm, .. } => {
            let x = b.f64(defs[a as usize], fs, defs);
            let y = b.konst(Op::ConstF64(imm as f64), Rep::F64);
            defs[dst as usize] = b.emit(Op::AddF(x, y), Rep::F64);
        }
        Instr::Neg { dst, a } => {
            let x = b.f64(defs[a as usize], fs, defs);
            defs[dst as usize] = b.emit(Op::NegF(x), Rep::F64);
        }
        Instr::Lt { dst, a, b: c } | Instr::Le { dst, a, b: c } | Instr::Gt { dst, a, b: c } | Instr::Ge { dst, a, b: c } => {
            let cmp = match *ins {
                Instr::Lt { .. } => Cmp::Lt,
                Instr::Le { .. } => Cmp::Le,
                Instr::Gt { .. } => Cmp::Gt,
                _ => Cmp::Ge,
            };
            defs[dst as usize] = b.compare(f, ip, cmp, defs[a as usize], defs[c as usize], fs, defs);
        }
        Instr::Eq { dst, a, b: c } | Instr::Ne { dst, a, b: c } => {
            let eq = matches!(*ins, Instr::Eq { .. });
            let (va, vc) = (defs[a as usize], defs[c as usize]);
            let (ra, rc) = (b.rep(va), b.rep(vc));
            let num = |r: Rep| matches!(r, Rep::I32 | Rep::F64);
            let exact_prim_bits = |b: &Builder, v: V| -> Option<u64> {
                match b.insts[v as usize].op {
                    Op::ConstTagged(bits)
                        if bits == Value::UNDEFINED.bits()
                            || bits == Value::NULL.bits()
                            || bits == Value::TRUE.bits()
                            || bits == Value::FALSE.bits() =>
                    {
                        Some(bits)
                    }
                    Op::ConstBool(flag) => Some(if flag { Value::TRUE.bits() } else { Value::FALSE.bits() }),
                    _ => None,
                }
            };
            let atom_const = |b: &Builder, v: V| -> Option<u64> {
                match b.insts[v as usize].op {
                    Op::ConstTagged(bits) if f.heap.is_atom_bits(bits) => Some(bits),
                    _ => None,
                }
            };
            defs[dst as usize] = if (num(ra) && exact_prim_bits(b, vc).is_some())
                || (num(rc) && exact_prim_bits(b, va).is_some())
                || (num(ra) && atom_const(b, vc).is_some())
                || (num(rc) && atom_const(b, va).is_some())
                || (ra == Rep::Bool && atom_const(b, vc).is_some())
                || (rc == Rep::Bool && atom_const(b, va).is_some())
            {
                // Distinct primitive/string types are never `===`.
                b.konst(Op::ConstBool(!eq), Rep::Bool)
            } else if ra == Rep::I32 && rc == Rep::I32 {
                b.emit(Op::CmpI(if eq { Cmp::Eq } else { Cmp::Ne }, va, vc), Rep::Bool)
            } else if num(ra) && num(rc) {
                let x = b.f64(va, fs, defs);
                let y = b.f64(vc, fs, defs);
                b.emit(Op::CmpF(if eq { Cmp::Eq } else { Cmp::Ne }, x, y), Rep::Bool)
            } else if ra == Rep::Bool && rc == Rep::Bool {
                b.emit(Op::CmpB(eq, va, vc), Rep::Bool)
            } else if let (Some(ba), Some(bc)) = (atom_const(b, va), atom_const(b, vc)) {
                b.konst(Op::ConstBool((ba == bc) == eq), Rep::Bool)
            } else if let Some(bits) = exact_prim_bits(b, vc) {
                let x = b.tagged(va);
                b.emit(Op::StrictEqBits(eq, x, bits), Rep::Bool)
            } else if let Some(bits) = exact_prim_bits(b, va) {
                let y = b.tagged(vc);
                b.emit(Op::StrictEqBits(eq, y, bits), Rep::Bool)
            } else if let Some(atom_bits) = atom_const(b, vc) {
                let x = b.tagged(va);
                let atom_ok = b.is_atom_or_prim(x);
                b.push_out(
                    Op::StrictEqAtom {
                        eq,
                        a: x,
                        atom_bits,
                        atom_ok,
                    },
                    Rep::Bool,
                    None,
                )
            } else if let Some(atom_bits) = atom_const(b, va) {
                let y = b.tagged(vc);
                let atom_ok = b.is_atom_or_prim(y);
                b.push_out(
                    Op::StrictEqAtom {
                        eq,
                        a: y,
                        atom_bits,
                        atom_ok,
                    },
                    Rep::Bool,
                    None,
                )
            } else {
                let x = b.tagged(va);
                let y = b.tagged(vc);
                // Always emit non-deopting `StrictEqH` (backed by per-function
                // `emit_strict_eq_stub` with inline bit/atom/object fast paths)
                // so string equality never triggers deopt evictions.
                b.push_out(Op::StrictEqH(eq, x, y), Rep::Bool, None)
            };
        }
        Instr::Not { dst, a } => {
            let t = b.truthy(defs[a as usize], fs, defs);
            defs[dst as usize] = b.emit(Op::NotB(t), Rep::Bool);
        }
        Instr::Bitwise { dst, a, b: c, op } => {
            let x = b.i32(defs[a as usize], fs, defs);
            let y = b.i32(defs[c as usize], fs, defs);
            let small_shift = match b.insts[y as usize].op {
                Op::ConstI32(k) if k & 31 != 0 => Some((k & 31) as u8),
                _ => None,
            };
            defs[dst as usize] = if let (true, Some(k)) = (f.ushr_i.is_some_and(|s| s.contains(&ip)), small_shift) {
                b.emit(Op::UshrI(x, k), Rep::I32)
            } else if matches!(op, BitwiseOp::Ushr) {
                b.emit(Op::UshrF(x, y), Rep::F64)
            } else {
                b.emit(Op::Bit(op, x, y), Rep::I32)
            };
        }
        Instr::GetProp { dst, obj, name } => {
            let s = fs.id(b, defs);
            defs[dst as usize] = if let Some((
                callee,
                Site::ClassGetter {
                    recv_shape,
                    class,
                    ver,
                    epoch,
                    fid,
                },
            )) = inline
            {
                // The class's getter, inlined behind the class-method guard.
                let o = b.tagged(defs[obj as usize]);
                b.push_out(
                    Op::GuardClass {
                        obj: o,
                        recv_shape,
                        class,
                        ver,
                        epoch,
                    },
                    Rep::Void,
                    Some(s),
                );
                b.inline_call(callee, fid, o, &[], s, f.callees, f.inline_depth + 1)?
            } else if let Some(&op) = f.math_reads.get(&ip) {
                // `Math.<op>` for a `MathOp`: the receiver is the intrinsic
                // `Math` holding its intrinsic method, a constant then.
                let g = f.math?;
                let o = b.tagged(defs[obj as usize]);
                b.push_out(
                    Op::MathGuard {
                        recv: Some(o),
                        gidx: 0,
                        op,
                    },
                    Rep::Void,
                    Some(s),
                );
                b.konst(Op::ConstTagged(g.op_callee_bits[op as usize]), Rep::Tagged)
            } else if let Some(&(oidx, ver, bits)) = f
                .const_props
                .and_then(|m| m.get(&ip))
                .filter(|_| !f.is_top || !f.generic(ip))
            {
                let o = b.tagged(defs[obj as usize]);
                b.guard_const_obj(o, oidx, ver, s);
                let val = Value::from_bits(bits);
                if val.is_int() {
                    b.konst(Op::ConstI32(val.as_int()), Rep::I32)
                } else if val.is_number() {
                    b.konst(Op::ConstF64(val.as_f64()), Rep::F64)
                } else if val.is_bool() {
                    b.konst(Op::ConstBool(val.as_bool()), Rep::Bool)
                } else {
                    b.konst(Op::ConstTagged(bits), Rep::Tagged)
                }
            } else if let Some(&(shape, slot)) = f
                .plan
                .get(&ip)
                // A site whose cache has met several shapes takes them all.
                .filter(|_| !f.poly.is_some_and(|m| m.contains_key(&ip)))
            {
                let o = b.tagged(defs[obj as usize]);
                if !f.is_top
                    || (!f.generic(ip)
                        && (f.multi_prop.get(ip).copied().unwrap_or(false) || b.has_shape_memo(o, shape)))
                {
                    let vals = b.shape_vals(o, shape, s);
                    b.emit(Op::LoadSlot { vals, slot }, Rep::Tagged)
                } else {
                    let slow = Some((ip as u32, ((name as u64) << 32) | obj as u64));
                    b.push_out(Op::GetPropShape { obj: o, shape, slot, slow }, Rep::Tagged, Some(s))
                }
            } else if f.is_top && f.p.string_constants.get(name as usize).is_some_and(|k| k == "length") {
                let o = b.tagged(defs[obj as usize]);
                b.push_out(
                    Op::Length {
                        obj: o,
                        ip: ip as u32,
                        packed: ((name as u64) << 32) | obj as u64,
                    },
                    Rep::Tagged,
                    Some(s),
                )
            } else if let Some(ways) = f.poly.and_then(|m| m.get(&ip)) {
                let o = b.tagged(defs[obj as usize]);
                let slow = f.is_top.then(|| (ip as u32, ((name as u64) << 32) | obj as u64));
                b.push_out(
                    Op::GetPropPoly {
                        obj: o,
                        ways: ways.clone(),
                        slow,
                    },
                    Rep::Tagged,
                    Some(s),
                )
            } else {
                // Admitted only at the top level, where the window is the
                // function's own.
                if !f.is_top {
                    return None;
                }
                let obj_v = defs[obj as usize];
                let prop_name = f.p.string_constants.get(name as usize).map(String::as_str);
                let is_call = prop_name == Some("call");
                let is_apply = prop_name == Some("apply");
                let str_char_code_at = prop_name == Some("charCodeAt");
                let str_slot_ptr = prop_name
                    .filter(|_| !str_char_code_at && f.heap.str_proto_ver != u32::MAX)
                    .and_then(super::super::emit_misc::str_proto_fast_index)
                    .map(|k| f.heap.str_proto_slot_ptrs[k])
                    .unwrap_or(0);
                let arr_slot_ptr = prop_name
                    .filter(|_| f.heap.arr_proto_ver != u32::MAX)
                    .and_then(super::super::emit_misc::arr_proto_fast_index)
                    .map(|k| f.heap.arr_proto_slot_ptrs[k])
                    .unwrap_or(0);
                let kind_idx = prop_name
                    .and_then(super::super::emit_misc::kind_proto_fast_index)
                    .filter(|&k| {
                        super::super::emit_misc::kind_proto_has_arms(
                            k,
                            &f.heap.kind_proto_vers,
                            &f.heap.kind_proto_slot_ptrs,
                        )
                    })
                    .map(|k| k as u8);
                b.push_out(
                    Op::CallOut {
                        helper: helper(crate::vm::jit_opt_get_prop_slow as *const ()),
                        ip: ip as u32,
                        packed: ((name as u64) << 32) | obj as u64,
                        argc: 0,
                        lane: CallLane::Prop {
                            obj,
                            obj_v,
                            dst,
                            name,
                            is_call,
                            is_apply,
                            str_char_code_at,
                            str_slot_ptr,
                            arr_slot_ptr,
                            kind_idx,
                        },
                    },
                    Rep::Tagged,
                    Some(s),
                )
            };
        }
        Instr::Call {
            dst,
            callee,
            arg_base,
            argc,
        } => {
            let s = fs.id(b, defs);
            let nested = if inline.is_none() && !f.is_top && f.inline_depth == 1 {
                f.callee_sites
                    .and_then(|m| m.get(&ip))
                    .and_then(|&sub_fid| f.callees.get(&sub_fid).map(|ci| (ci, sub_fid)))
            } else {
                None
            };
            defs[dst as usize] = match inline {
                Some((callee_info, Site::Call { fid })) => {
                    let cv = b.tagged(defs[callee as usize]);
                    b.push_out(Op::GuardFid { v: cv, fid }, Rep::Void, Some(s));
                    let this = b.konst(Op::ConstTagged(undefined_bits), Rep::Tagged);
                    let args: Vec<V> = (0..argc).map(|k| defs[(arg_base + k) as usize]).collect();
                    b.inline_call(callee_info, fid, this, &args, s, f.callees, f.inline_depth + 1)?
                }
                _ if nested.is_some() => {
                    let (callee_info, fid) = nested.expect("checked");
                    let cv = b.tagged(defs[callee as usize]);
                    b.push_out(Op::GuardFid { v: cv, fid }, Rep::Void, Some(s));
                    let this = b.konst(Op::ConstTagged(undefined_bits), Rep::Tagged);
                    let args: Vec<V> = (0..argc).map(|k| defs[(arg_base + k) as usize]).collect();
                    b.inline_call(callee_info, fid, this, &args, s, f.callees, f.inline_depth + 1)?
                }
                _ => {
                    if !f.is_top {
                        return None;
                    }
                    b.push_out(
                        Op::CallOut {
                            helper: helper(crate::vm::jit_call_ic_cross as *const ()),
                            ip: ip as u32,
                            packed: ((callee as u64) << 16) | arg_base as u64,
                            argc,
                            lane: CallLane::Plain {
                                callee,
                                this: None,
                                arg_base,
                                dst,
                            },
                        },
                        Rep::Tagged,
                        Some(s),
                    )
                }
            };
        }
        Instr::CallMethod {
            dst,
            obj,
            name,
            arg_base,
            argc,
        } => {
            let s = fs.id(b, defs);
            defs[dst as usize] = match inline {
                Some((callee_info, Site::Method { shape, slot, fid })) => {
                    let o = b.tagged(defs[obj as usize]);
                    let m = b.push_out(
                        Op::GetPropShape {
                            obj: o,
                            shape,
                            slot,
                            slow: None,
                        },
                        Rep::Tagged,
                        Some(s),
                    );
                    b.push_out(Op::GuardFid { v: m, fid }, Rep::Void, Some(s));
                    let args: Vec<V> = (0..argc).map(|k| defs[(arg_base + k) as usize]).collect();
                    b.inline_call(callee_info, fid, o, &args, s, f.callees, f.inline_depth + 1)?
                }
                Some((
                    callee_info,
                    Site::ClassMethod {
                        recv_shape,
                        class,
                        ver,
                        epoch,
                        fid,
                    },
                )) => {
                    let o = b.tagged(defs[obj as usize]);
                    b.push_out(
                        Op::GuardClass {
                            obj: o,
                            recv_shape,
                            class,
                            ver,
                            epoch,
                        },
                        Rep::Void,
                        Some(s),
                    );
                    let args: Vec<V> = (0..argc).map(|k| defs[(arg_base + k) as usize]).collect();
                    b.inline_call(callee_info, fid, o, &args, s, f.callees, f.inline_depth + 1)?
                }
                _ if !f.generic(ip)
                    && argc == 1
                    && f.p.string_constants.get(name as usize).is_some_and(|k| k == "charCodeAt") =>
                {
                    let o = b.tagged(defs[obj as usize]);
                    let k = b.index_key(defs[arg_base as usize]);
                    b.push_out(Op::CharCodeAt { obj: o, idx: k }, Rep::Tagged, Some(s))
                }
                _ => b.push_out(
                    Op::CallOut {
                        helper: helper(crate::vm::jit_call_method_ic as *const ()),
                        ip: ip as u32,
                        packed: ((name as u64) << 32) | ((obj as u64) << 16) | arg_base as u64,
                        argc,
                        lane: CallLane::Method {
                            obj,
                            obj_v: defs[obj as usize],
                            arg_base,
                            arg0_v: (argc >= 1).then(|| defs[arg_base as usize]),
                            dst,
                            intr: f
                                .p
                                .string_constants
                                .get(name as usize)
                                .and_then(|k| method_intr(k, argc)),
                            mega_ok: f.p.string_constants.get(name as usize).is_some_and(|k| {
                                let s = k.as_str();
                                !matches!(
                                    s,
                                    "call"
                                        | "apply"
                                        | "bind"
                                        | "hasOwnProperty"
                                        | "propertyIsEnumerable"
                                        | "isPrototypeOf"
                                ) && !crate::vm::is_private_key(s)
                            }),
                        },
                    },
                    Rep::Tagged,
                    Some(s),
                ),
            };
        }
        Instr::CallWithThis {
            dst,
            callee,
            this_v,
            arg_base,
            argc,
            ..
        } => {
            let s = fs.id(b, defs);
            defs[dst as usize] = match inline {
                Some((callee_info, Site::Call { fid })) => {
                    let cv = b.tagged(defs[callee as usize]);
                    b.push_out(Op::GuardFid { v: cv, fid }, Rep::Void, Some(s));
                    let this = b.tagged(defs[this_v as usize]);
                    let args: Vec<V> = (0..argc).map(|k| defs[(arg_base + k) as usize]).collect();
                    b.inline_call(callee_info, fid, this, &args, s, f.callees, f.inline_depth + 1)?
                }
                _ => b.push_out(
                    Op::CallOut {
                        helper: helper(crate::vm::jit_call_with_this_cross as *const ()),
                        ip: ip as u32,
                        packed: ((this_v as u64) << 32) | ((callee as u64) << 16) | arg_base as u64,
                        argc,
                        lane: CallLane::Plain {
                            callee,
                            this: Some(this_v),
                            arg_base,
                            dst,
                        },
                    },
                    Rep::Tagged,
                    Some(s),
                ),
            };
        }
        Instr::Jump { target } => return Some(Flow::Jump(target as usize)),
        Instr::JumpIfFalse { cond, target } | Instr::JumpIfTrue { cond, target } => {
            let t = b.truthy(defs[cond as usize], fs, defs);
            let jump = target as usize;
            return Some(if matches!(*ins, Instr::JumpIfTrue { .. }) {
                Flow::Branch { cond: t, t: jump, f: ip + 1 }
            } else {
                Flow::Branch { cond: t, t: ip + 1, f: jump }
            });
        }
        Instr::JumpIfNotLt { a, b: c, target } | Instr::JumpIfNotLe { a, b: c, target } => {
            let cmp = if matches!(*ins, Instr::JumpIfNotLt { .. }) {
                Cmp::Lt
            } else {
                Cmp::Le
            };
            let t = b.compare(f, ip, cmp, defs[a as usize], defs[c as usize], fs, defs);
            return Some(Flow::Branch {
                cond: t,
                t: ip + 1,
                f: target as usize,
            });
        }
        Instr::Return { .. } | Instr::ReturnUndefined if f.handlers => {
            // A `finally` the return passes through runs in the interpreter.
            let v = match *ins {
                Instr::Return { src } => b.tagged(defs[src as usize]),
                _ => b.konst(Op::ConstTagged(undefined_bits), Rep::Tagged),
            };
            let s = fs.id(b, defs);
            b.push_out(Op::ReturnFinally { v }, Rep::Void, Some(s));
            return Some(Flow::Ret(v));
        }
        Instr::Return { src } => return Some(Flow::Ret(defs[src as usize])),
        Instr::ReturnUndefined => return Some(Flow::Ret(b.konst(Op::ConstTagged(undefined_bits), Rep::Tagged))),
        Instr::PushFinally {
            target,
            kind_reg,
            val_reg,
        } => {
            let packed = (target as u64) << 32 | (kind_reg as u64) << 16 | val_reg as u64;
            b.push_out(
                Op::Handler {
                    helper: helper(crate::vm::jit_push_finally as *const ()),
                    packed,
                },
                Rep::Void,
                None,
            );
        }
        Instr::PushHandler { catch_target, catch_reg } => {
            b.push_out(
                Op::Handler {
                    helper: helper(crate::vm::jit_push_catch as *const ()),
                    packed: (catch_target as u64) << 16 | catch_reg as u64,
                },
                Rep::Void,
                None,
            );
        }
        Instr::PopFinally | Instr::PopHandler => {
            b.push_out(
                Op::Handler {
                    helper: helper(crate::vm::jit_pop_finally as *const ()),
                    packed: 0,
                },
                Rep::Void,
                None,
            );
        }
        Instr::EndFinally { kind_reg, val_reg } => {
            let s = fs.id(b, defs);
            b.push_out(
                Op::EndFinally {
                    kind: kind_reg,
                    val: val_reg,
                    ip: ip as u32,
                },
                Rep::Void,
                Some(s),
            );
        }
        Instr::IterCloseFinally { kind_reg, .. } => {
            let s = fs.id(b, defs);
            b.push_out(
                Op::IterCloseFin {
                    kind: kind_reg,
                    ip: ip as u32,
                },
                Rep::Void,
                Some(s),
            );
        }
        Instr::IterClose { iter } | Instr::IterCloseQuiet { iter } => {
            // `jit_iter_op_ic`'s close (2) or quiet close (3), as Tier C's.
            let op: u64 = if matches!(*ins, Instr::IterClose { .. }) { 2 } else { 3 };
            let s = fs.id(b, defs);
            defs[iter as usize] = b.push_out(
                Op::CallOut {
                    helper: helper(crate::vm::jit_iter_op_ic as *const ()),
                    ip: ip as u32,
                    packed: iter as u64 | op << 56,
                    argc: 0,
                    lane: CallLane::None,
                },
                Rep::Tagged,
                Some(s),
            );
        }
        Instr::JumpFinally { target, floor } => {
            let s = fs.id(b, defs);
            b.push_out(Op::JumpFinally { target, floor }, Rep::Void, Some(s));
            return Some(Flow::Exit);
        }
        Instr::Throw { src } => {
            let v = b.tagged(defs[src as usize]);
            let s = fs.id(b, defs);
            b.push_out(Op::Throw { v, ip: ip as u32 }, Rep::Void, Some(s));
            return Some(Flow::Exit);
        }
        Instr::LoadGlobalOrUndefined { dst, idx } => {
            let s = fs.id(b, defs);
            defs[dst as usize] = if f.slow_global_loads.is_some_and(|set| set.contains(&idx)) {
                b.push_out(Op::LoadGlobalOrUndefSlow { idx, ip: ip as u32 }, Rep::Tagged, Some(s))
            } else {
                b.push_out(Op::LoadGlobalOrUndef(idx), Rep::Tagged, Some(s))
            };
        }
        Instr::LoadCallee { dst } => {
            let s = fs.id(b, defs);
            defs[dst as usize] = b.push_out(Op::LoadCallee { ip: ip as u32 }, Rep::Tagged, Some(s));
        }
        Instr::BitNot { dst, a } => {
            let x = b.i32(defs[a as usize], fs, defs);
            defs[dst as usize] = b.emit(Op::BitNotI(x), Rep::I32);
        }
        Instr::LooseEq { dst, a, b: c } | Instr::LooseNe { dst, a, b: c } => {
            let is_ne = matches!(*ins, Instr::LooseNe { .. });
            let (va, vc) = (defs[a as usize], defs[c as usize]);
            let is_nullish_const = |v: V| {
                matches!(
                    b.insts[v as usize].op,
                    Op::ConstTagged(bits)
                        if bits == Value::NULL.bits() || bits == Value::UNDEFINED.bits()
                )
            };
            let other_if_nullish = if is_nullish_const(vc) {
                Some(va)
            } else if is_nullish_const(va) {
                Some(vc)
            } else {
                None
            };
            defs[dst as usize] = if let Some(other) = other_if_nullish {
                if is_nullish_const(other) {
                    b.konst(Op::ConstBool(!is_ne), Rep::Bool)
                } else if matches!(b.rep(other), Rep::I32 | Rep::F64 | Rep::Bool) {
                    b.konst(Op::ConstBool(is_ne), Rep::Bool)
                } else {
                    let s = fs.id(b, defs);
                    let x = b.tagged(other);
                    b.push_out(
                        Op::IsNullish {
                            is_ne,
                            a: x,
                            ip: ip as u32,
                            inlined: !f.is_top,
                        },
                        Rep::Bool,
                        Some(s),
                    )
                }
            } else {
                let x = b.tagged(va);
                let y = b.tagged(vc);
                let s = fs.id(b, defs);
                b.push_out(
                    Op::LooseEq {
                        is_ne,
                        a: x,
                        b: y,
                        ip: ip as u32,
                        inlined: !f.is_top,
                    },
                    Rep::Bool,
                    Some(s),
                )
            };
        }
        Instr::IsArray {
            dst,
            a,
            callee,
            this_v,
        } => {
            let val = b.tagged(defs[a as usize]);
            let cv = b.tagged(defs[callee as usize]);
            let tv = b.tagged(defs[this_v as usize]);
            let s = fs.id(b, defs);
            defs[dst as usize] = b.push_out(
                Op::IsArray {
                    val,
                    callee: cv,
                    this_v: tv,
                    ip: ip as u32,
                    inlined: !f.is_top,
                },
                Rep::Bool,
                Some(s),
            );
        }
        Instr::InstanceOfDyn { dst, val, ctor } => {
            let v_val = b.tagged(defs[val as usize]);
            let c_val = b.tagged(defs[ctor as usize]);
            let s = fs.id(b, defs);
            let packed = ((ctor as u64) << 16) | val as u64;
            defs[dst as usize] = b.push_out(
                Op::InstanceOf {
                    val: v_val,
                    ctor: c_val,
                    ip: ip as u32,
                    packed,
                },
                Rep::Tagged,
                Some(s),
            );
        }
        Instr::TypeOf { dst, a } => {
            let x = b.tagged(defs[a as usize]);
            let s = fs.id(b, defs);
            defs[dst as usize] = b.push_out(Op::TypeOf(x), Rep::Tagged, Some(s));
        }
        Instr::TypeOfIs { dst, a, code, neg } => {
            let x = b.tagged(defs[a as usize]);
            defs[dst as usize] = b.emit(Op::TypeOfIs { a: x, code, neg }, Rep::Bool);
        }
        Instr::TypeOfSame { dst, a, b: c, neg } => {
            let x = b.tagged(defs[a as usize]);
            let y = b.tagged(defs[c as usize]);
            defs[dst as usize] = b.emit(Op::TypeOfSame { a: x, b: y, neg }, Rep::Bool);
        }
        Instr::StrConcatChain { dst, a, b: c } => {
            let x = b.tagged(defs[a as usize]);
            let y = b.tagged(defs[c as usize]);
            let s = fs.id(b, defs);
            let hint = super::super::region_mem::chain_capacity_hint(
                &f.p.code,
                ip,
                a,
                f.p.code.len() - 1,
            );
            let next_leaf = if crate::heap::concat_suffix_memo_enabled() {
                super::super::region_mem::chain_next_leaf(&f.p.code, ip, a)
            } else {
                None
            };
            defs[dst as usize] = b.push_out(
                Op::ConcatChain {
                    a: x,
                    b: y,
                    next_leaf,
                    hint,
                    ip: ip as u32,
                },
                Rep::Tagged,
                Some(s),
            );
        }
        Instr::TailCall { .. } => {
            let s = fs.id(b, defs);
            b.push_out(Op::TailCallGuard { ip: ip as u32 }, Rep::Void, Some(s));
        }
        Instr::CheckCoercible { src } => {
            let x = b.tagged(defs[src as usize]);
            let s = fs.id(b, defs);
            b.push_out(
                Op::CheckCoercible {
                    src: x,
                    ip: ip as u32,
                    inlined: !f.is_top,
                },
                Rep::Void,
                Some(s),
            );
        }
        Instr::MakeCell { reg } | Instr::MakeCellTdz { reg } | Instr::MakeCellFnName { reg } => {
            let s = fs.id(b, defs);
            let tdz = matches!(*ins, Instr::MakeCellTdz { .. });
            let fn_name = matches!(*ins, Instr::MakeCellFnName { .. });
            let init = (!tdz).then(|| defs[reg as usize]);
            defs[reg as usize] = b.push_out(
                Op::MakeCell {
                    reg,
                    init,
                    tdz,
                    fn_name,
                    ip: ip as u32,
                },
                Rep::Tagged,
                Some(s),
            );
        }
        Instr::MarkCellConst { .. } => {
            let s = fs.id(b, defs);
            let fip = (f.func_id << 32) | ip as u64;
            let window = u64::from(f.p.reg_count.max(1));
            b.push_out(
                Op::AllocOut {
                    helper: helper(crate::vm::jit_make_cell as *const ()),
                    ip: ip as u32,
                    a: fip,
                    b: window,
                },
                Rep::Void,
                Some(s),
            );
        }
        Instr::MakeFunc { dst, .. } => {
            let s = fs.id(b, defs);
            defs[dst as usize] = b.push_out(Op::MakeFunc { ip: ip as u32 }, Rep::Tagged, Some(s));
        }
        Instr::MakeClosure { dst, .. } | Instr::MakeArrow { dst, .. } => {
            let s = fs.id(b, defs);
            let mut captures = Vec::new();
            if let Some(plan) = f.plain_closures.and_then(|m| m.get(&ip)) {
                for src in &plan.upvalues {
                    if let crate::bytecode::UpvalSource::ParentLocal(r) = *src {
                        captures.push((r, defs[r as usize]));
                    }
                }
                if let Some(tr) = plan.this_reg {
                    captures.push((tr, defs[tr as usize]));
                }
            }
            defs[dst as usize] = b.push_out(
                Op::MakeClosure {
                    dst,
                    ip: ip as u32,
                    captures,
                },
                Rep::Tagged,
                Some(s),
            );
        }
        Instr::CellGet { dst, cell } => {
            let c = b.tagged(defs[cell as usize]);
            let s = fs.id(b, defs);
            defs[dst as usize] = b.push_out(Op::CellGet { cell: c, ip: ip as u32 }, Rep::Tagged, Some(s));
        }
        Instr::CellSet { cell, src } | Instr::CellSetChecked { cell, src } => {
            let c = b.tagged(defs[cell as usize]);
            let x = b.tagged(defs[src as usize]);
            let checked = matches!(*ins, Instr::CellSetChecked { .. });
            let s = checked.then(|| fs.id(b, defs));
            b.push_out(
                Op::CellSet {
                    cell: c,
                    src: x,
                    checked,
                    ip: ip as u32,
                },
                Rep::Void,
                s,
            );
        }
        Instr::NewObject { dst, hint } => {
            let s = fs.id(b, defs);
            defs[dst as usize] = b.push_out(Op::NewObject { hint, ip: ip as u32 }, Rep::Tagged, Some(s));
        }
        Instr::NewPlannedObject { dst, plan } => {
            let s = fs.id(b, defs);
            defs[dst as usize] = b.push_out(Op::NewPlannedObject { plan: u32::from(plan), ip: ip as u32 }, Rep::Tagged, Some(s));
        }
        Instr::AppendDataProp { obj, name, val } => {
            let o = b.tagged(defs[obj as usize]);
            let x = b.tagged(defs[val as usize]);
            let s = fs.id(b, defs);
            b.push_out(
                Op::AppendDataProp {
                    obj: o,
                    name,
                    val: x,
                    ip: ip as u32,
                },
                Rep::Void,
                Some(s),
            );
        }
        Instr::InitDataProp { obj, name, val } => {
            let s = fs.id(b, defs);
            defs[obj as usize] = b.push_out(
                Op::CallOut {
                    helper: helper(crate::vm::jit_obj_init_ic as *const ()),
                    ip: ip as u32,
                    packed: obj as u64 | (val as u64) << 16 | (name as u64) << 32 | 1u64 << 56,
                    argc: 0,
                    lane: CallLane::None,
                },
                Rep::Tagged,
                Some(s),
            );
        }
        Instr::ObjectSpread { target, src } => {
            let s = fs.id(b, defs);
            defs[target as usize] = b.push_out(
                Op::CallOut {
                    helper: helper(crate::vm::jit_obj_init_ic as *const ()),
                    ip: ip as u32,
                    packed: target as u64 | (src as u64) << 16,
                    argc: 0,
                    lane: CallLane::None,
                },
                Rep::Tagged,
                Some(s),
            );
        }
        Instr::ObjectRest {
            dst,
            src,
            exclude_start,
            exclude_count,
        } => {
            let s = fs.id(b, defs);
            defs[dst as usize] = b.push_out(
                Op::CallOut {
                    helper: helper(crate::vm::jit_cold_op_ic as *const ()),
                    ip: ip as u32,
                    packed: src as u64 | (exclude_start as u64) << 16 | (exclude_count as u64) << 32,
                    argc: 0,
                    lane: CallLane::None,
                },
                Rep::Tagged,
                Some(s),
            );
        }
        Instr::ArrayAppend { arr, val, spread } => {
            let s = fs.id(b, defs);
            defs[arr as usize] = b.push_out(
                Op::CallOut {
                    helper: helper(crate::vm::jit_array_append_ic as *const ()),
                    ip: ip as u32,
                    packed: arr as u64 | (val as u64) << 16 | (spread as u64) << 32,
                    argc: 0,
                    lane: CallLane::None,
                },
                Rep::Tagged,
                Some(s),
            );
        }
        Instr::IterToArray { dst, .. } => {
            defs[dst as usize] = b.emit(Op::IterProbe, Rep::Tagged);
        }
        Instr::ToStr { dst, a } => {
            let s = fs.id(b, defs);
            defs[dst as usize] = b.push_out(
                Op::CallOut {
                    helper: helper(crate::vm::jit_to_str_ic as *const ()),
                    ip: ip as u32,
                    packed: a as u64,
                    argc: 0,
                    lane: CallLane::None,
                },
                Rep::Tagged,
                Some(s),
            );
        }
        Instr::StaticFn { dst, .. } => {
            let s = fs.id(b, defs);
            defs[dst as usize] = b.push_out(
                Op::CallOut {
                    helper: helper(crate::vm::jit_static_fn_ic as *const ()),
                    ip: ip as u32,
                    packed: 0,
                    argc: 0,
                    lane: CallLane::None,
                },
                Rep::Tagged,
                Some(s),
            );
        }
        Instr::GlobalFn {
            dst,
            op,
            callee,
            arg_base,
            argc,
        } => {
            let s = fs.id(b, defs);
            defs[dst as usize] = b.push_out(
                Op::CallOut {
                    helper: helper(crate::vm::jit_global_fn_ic as *const ()),
                    ip: ip as u32,
                    packed: callee as u64
                        | (arg_base as u64) << 16
                        | (argc as u64) << 32
                        | (op as u8 as u64) << 48,
                    argc: 0,
                    lane: CallLane::None,
                },
                Rep::Tagged,
                Some(s),
            );
        }
        Instr::ObjectKeys {
            dst,
            obj,
            callee,
            this_v,
        } => {
            let o = b.tagged(defs[obj as usize]);
            let cv = b.tagged(defs[callee as usize]);
            let tv = b.tagged(defs[this_v as usize]);
            let s = fs.id(b, defs);
            defs[dst as usize] = b.push_out(
                Op::ObjectKeys {
                    obj: o,
                    callee: cv,
                    this_v: tv,
                    ip: ip as u32,
                },
                Rep::Tagged,
                Some(s),
            );
        }
        Instr::ObjectValues {
            dst,
            obj,
            callee,
            this_v,
        }
        | Instr::ObjectEntries {
            dst,
            obj,
            callee,
            this_v,
        } => {
            let op: u64 = if matches!(*ins, Instr::ObjectValues { .. }) { 1 } else { 2 };
            let s = fs.id(b, defs);
            defs[dst as usize] = b.push_out(
                Op::CallOut {
                    helper: helper(crate::vm::jit_obj_enum_ic as *const ()),
                    ip: ip as u32,
                    packed: obj as u64 | (callee as u64) << 16 | (this_v as u64) << 32 | op << 56,
                    argc: 0,
                    lane: CallLane::None,
                },
                Rep::Tagged,
                Some(s),
            );
        }
        Instr::HasProp {
            dst,
            key,
            obj,
            brand: false,
        } => {
            let k = b.tagged(defs[key as usize]);
            let o = b.tagged(defs[obj as usize]);
            let s = fs.id(b, defs);
            defs[dst as usize] = b.push_out(
                Op::HasProp {
                    key: k,
                    obj: o,
                    ip: ip as u32,
                    inlined: !f.is_top,
                },
                Rep::Bool,
                Some(s),
            );
        }
        Instr::HasProp {
            dst,
            key,
            obj,
            brand: true,
        } => {
            let s = fs.id(b, defs);
            defs[dst as usize] = b.push_out(
                Op::CallOut {
                    helper: helper(crate::vm::jit_private_in_ic as *const ()),
                    ip: ip as u32,
                    packed: obj as u64 | (key as u64) << 16,
                    argc: 0,
                    lane: CallLane::None,
                },
                Rep::Tagged,
                Some(s),
            );
        }
        Instr::LenOf { dst, obj } => {
            let o = b.tagged(defs[obj as usize]);
            defs[dst as usize] = b.emit(Op::LenOf { obj: o }, Rep::Tagged);
        }
        Instr::ForInKeys { dst, obj } => {
            let o = b.tagged(defs[obj as usize]);
            let s = fs.id(b, defs);
            defs[dst as usize] = b.push_out(Op::ForInKeys { obj: o, ip: ip as u32 }, Rep::Tagged, Some(s));
        }
        Instr::ForInLive { dst, obj, key } => {
            let o = b.tagged(defs[obj as usize]);
            let k = b.tagged(defs[key as usize]);
            let s = fs.id(b, defs);
            defs[dst as usize] = b.push_out(Op::ForInLive { obj: o, key: k }, Rep::Tagged, Some(s));
        }
        Instr::SuperGet {
            dst,
            home_class_id,
            name,
        } => {
            let s = fs.id(b, defs);
            defs[dst as usize] = b.push_out(
                Op::CallOut {
                    helper: helper(crate::vm::jit_super_get_ic as *const ()),
                    ip: ip as u32,
                    packed: home_class_id as u64 | (name as u64) << 32,
                    argc: 0,
                    lane: CallLane::None,
                },
                Rep::Tagged,
                Some(s),
            );
        }
        Instr::NewRegExp {
            dst,
            pattern,
            flags,
            is_construct,
        } => {
            let s = fs.id(b, defs);
            defs[dst as usize] = b.push_out(
                Op::CallOut {
                    helper: helper(crate::vm::jit_new_regexp_ic as *const ()),
                    ip: ip as u32,
                    packed: pattern as u64 | (flags as u64) << 16 | (is_construct as u64) << 32,
                    argc: 0,
                    lane: CallLane::None,
                },
                Rep::Tagged,
                Some(s),
            );
        }
        Instr::RegExpLitProbe { dst, pattern, flags } => {
            let s = fs.id(b, defs);
            defs[dst as usize] = b.push_out(
                Op::RegExpLitProbe {
                    pattern,
                    flags,
                    ip: ip as u32,
                },
                Rep::Tagged,
                Some(s),
            );
        }
        Instr::RegExpLitTest {
            dst,
            arg,
            pattern,
            flags,
        } => {
            let s = fs.id(b, defs);
            defs[dst as usize] = b.push_out(
                Op::RegExpLitTest {
                    arg,
                    pattern,
                    flags,
                    ip: ip as u32,
                    packed: arg as u64 | (pattern as u64) << 16 | (flags as u64) << 40,
                },
                Rep::Tagged,
                Some(s),
            );
        }
        Instr::RegExpMethod {
            dst,
            op,
            callee,
            this_v,
            arg_base,
            argc,
        } => {
            let s = fs.id(b, defs);
            defs[dst as usize] = b.push_out(
                Op::RegExpMethod {
                    op,
                    callee,
                    this_v,
                    arg_base,
                    argc,
                    ip: ip as u32,
                    packed: ((this_v as u64) << 32) | ((callee as u64) << 16) | arg_base as u64,
                },
                Rep::Tagged,
                Some(s),
            );
        }
        Instr::CallMethodComputed {
            dst,
            obj,
            key,
            arg_base,
            argc,
        } => {
            let s = fs.id(b, defs);
            defs[dst as usize] = b.push_out(
                Op::CallOut {
                    helper: helper(crate::vm::jit_call_method_computed_dense as *const ()),
                    ip: ip as u32,
                    packed: ((obj as u64) << 32) | ((key as u64) << 16) | arg_base as u64,
                    argc,
                    lane: CallLane::None,
                },
                Rep::Tagged,
                Some(s),
            );
        }
        Instr::ThisCheck { src } => {
            let x = b.tagged(defs[src as usize]);
            let s = fs.id(b, defs);
            defs[src as usize] = b.push_out(Op::ThisCheck { src: x, ip: ip as u32 }, Rep::Tagged, Some(s));
        }
        Instr::SuperCtorFetch { dst, home_class_id } => {
            let (baked, epoch) = match f.super_parents {
                Some((m, ep)) => (m.get(&home_class_id).copied(), ep),
                None => (None, 0),
            };
            defs[dst as usize] = b.push_out(
                Op::SuperCtorFetch {
                    home_class_id,
                    baked,
                    epoch,
                },
                Rep::Tagged,
                None,
            );
        }
        Instr::SuperCtor {
            ctor,
            home_class_id,
            arg_base,
            argc,
        } => {
            let s = fs.id(b, defs);
            let packed = ((home_class_id as u64) << 32) | ((ctor as u64) << 16) | arg_base as u64;
            defs[0] = b.push_out(
                Op::CallOut {
                    helper: helper(crate::vm::jit_super_ctor_ic as *const ()),
                    ip: ip as u32,
                    packed,
                    argc,
                    lane: CallLane::None,
                },
                Rep::Tagged,
                Some(s),
            );
        }
        Instr::SuperCtorSpread { .. } => {
            let s = fs.id(b, defs);
            defs[0] = b.push_out(
                Op::CallOut {
                    helper: helper(crate::vm::jit_cold_op_ic as *const ()),
                    ip: ip as u32,
                    packed: 0,
                    argc: 13,
                    lane: CallLane::None,
                },
                Rep::Tagged,
                Some(s),
            );
        }
        Instr::ToPropKey { dst, obj, src } => {
            let o = b.tagged(defs[obj as usize]);
            let x = b.tagged(defs[src as usize]);
            let s = fs.id(b, defs);
            defs[dst as usize] = b.push_out(Op::ToPropKey { obj: o, src: x, ip: ip as u32 }, Rep::Tagged, Some(s));
        }
        Instr::ToConcatKey { dst, src } => {
            let x = b.tagged(defs[src as usize]);
            let s = fs.id(b, defs);
            defs[dst as usize] = b.push_out(Op::ToConcatKey { src: x, ip: ip as u32 }, Rep::Tagged, Some(s));
        }
        Instr::InitDataPropDyn { obj, key, val } => {
            let o = b.tagged(defs[obj as usize]);
            let k = b.tagged(defs[key as usize]);
            let x = b.tagged(defs[val as usize]);
            let s = fs.id(b, defs);
            defs[obj as usize] = b.push_out(
                Op::InitDataPropDyn {
                    obj_reg: obj,
                    obj: o,
                    key: k,
                    val: x,
                    ip: ip as u32,
                },
                Rep::Tagged,
                Some(s),
            );
        }
        Instr::ArrayFrom { dst, .. }
        | Instr::CallWithThisSpread { dst, .. }
        | Instr::DeleteIndex { dst, .. }
        | Instr::JsonStringify { dst, .. }
        | Instr::JsonParse { dst, .. }
        | Instr::ArrayRest { dst, .. }
        | Instr::GetIndexConcat { dst, .. } => {
            let cold_op: u16 = match *ins {
                Instr::ArrayFrom { .. } => 2,
                Instr::CallWithThisSpread { .. } => 6,
                Instr::DeleteIndex { .. } => 8,
                Instr::JsonStringify { .. } => 9,
                Instr::JsonParse { .. } => 10,
                Instr::ArrayRest { .. } => 16,
                Instr::GetIndexConcat { .. } => 18,
                _ => unreachable!(),
            };
            let s = fs.id(b, defs);
            defs[dst as usize] = b.push_out(
                Op::CallOut {
                    helper: helper(crate::vm::jit_cold_op_ic as *const ()),
                    ip: ip as u32,
                    packed: 0,
                    argc: cold_op,
                    lane: CallLane::None,
                },
                Rep::Tagged,
                Some(s),
            );
        }
        Instr::DeleteProp { dst, .. } | Instr::Pow { dst, .. } => {
            let s = fs.id(b, defs);
            b.push_out(Op::BailOnly { ip: ip as u32 }, Rep::Void, Some(s));
            defs[dst as usize] = b.konst(Op::ConstTagged(undefined_bits), Rep::Tagged);
        }
        _ => return None,
    }
    Some(Flow::Next)
}

/// The frame state of the op being built, made on its first guard (or one
/// given: an inlined body's guards all take its call's).
struct FsCtx<'a> {
    ip: u32,
    live: Option<&'a Bits>,
    readonly_param: &'a [bool],
    id: Option<u32>,
}

impl FsCtx<'_> {
    fn id(&mut self, b: &mut Builder, defs: &[V]) -> u32 {
        if let Some(id) = self.id {
            return id;
        }
        let id = b.states.len() as u32;
        let mut regs = Vec::new();
        if let Some(live) = self.live {
            live.for_each_set(|r| {
                if !self.readonly_param[r] {
                    regs.push((r as u16, defs[r]));
                }
            });
        }
        b.states.push(FrameState {
            ip: self.ip,
            why: self.ip,
            regs,
        });
        self.id = Some(id);
        id
    }
}

struct Builder {
    insts: Vec<Inst>,
    states: Vec<FrameState>,
    blocks: Vec<Block>,
    cur_block: B,
    cur_preds: Vec<B>,
    /// The current block's conversions: (value, representation) -> value.
    memo: Vec<(V, Rep, V)>,
    /// The current block's values, in order.
    out: Vec<V>,
    /// The constants, made once each at the entry: (representation, bits,
    /// value). Made where the bytecode loads them, a loop's rebuilt them
    /// every iteration.
    consts: Vec<(Rep, u64, V)>,
    inlined: u32,
    inlined_callees: Vec<u32>,
    /// The intrinsic `Math` (`Inputs::math`), for inlined callees.
    math: Option<MathIntrinsicGuard>,
    /// Helper addresses + compile-time `atoms` snapshot, for inlined callees.
    heap: super::super::HeapHelpers,
    /// The current block's shape checks: (object, shape, its values
    /// pointer), forgotten at an op that may reshape (`Op::may_reshape`).
    shape_memo: Vec<(V, u32, V)>,
    /// Dominating `IsAtomOrPrim(x)` CSE cache: `(x, atom_ok)`. An SSA value's
    /// atomicity is immutable, so entries survive calls and reshapes.
    atom_memo: Vec<(V, V)>,
    /// The current block's `GuardConstObj(obj, oidx, ver)` checks, cleared on
    /// `Op::may_reshape()`.
    const_obj_memo: Vec<(V, u32, u32)>,
    /// The current block's directly-routable `LoadGlobal(idx)` CSE cache,
    /// cleared on `Op::may_reshape()` and `Op::StoreGlobal(..)`.
    global_memo: Vec<(u32, V)>,
}

impl Builder {
    fn is_atom_or_prim(&mut self, x: V) -> V {
        if let Some(&(_, ok)) = self.atom_memo.iter().find(|&&(v, _)| v == x) {
            return ok;
        }
        let ok = self.emit(Op::IsAtomOrPrim(x), Rep::Bool);
        self.atom_memo.push((x, ok));
        ok
    }

    fn load_global(&mut self, idx: u32, fs: u32) -> V {
        if let Some(&(_, v)) = self.global_memo.iter().find(|&&(i, _)| i == idx) {
            return v;
        }
        let v = self.push_out(Op::LoadGlobal(idx), Rep::Tagged, Some(fs));
        self.global_memo.push((idx, v));
        v
    }

    fn guard_const_obj(&mut self, obj: V, oidx: u32, ver: u32, fs: u32) {
        if self
            .const_obj_memo
            .iter()
            .any(|&(o, i, v)| o == obj && i == oidx && v == ver)
        {
            return;
        }
        self.push_out(Op::GuardConstObj { obj, oidx, ver }, Rep::Void, Some(fs));
        self.const_obj_memo.push((obj, oidx, ver));
    }

    fn has_shape_memo(&self, obj: V, shape: u32) -> bool {
        self.shape_memo.iter().any(|&(o, sh, _)| o == obj && sh == shape)
    }

    /// `obj`'s values pointer behind its shape: one check per object and
    /// block while nothing reshapes it.
    fn shape_vals(&mut self, obj: V, shape: u32, fs: u32) -> V {
        if let Some(&(_, _, v)) = self.shape_memo.iter().find(|&&(o, sh, _)| o == obj && sh == shape) {
            return v;
        }
        let v = self.push_out(Op::ShapeVals { obj, shape }, Rep::Raw, Some(fs));
        self.shape_memo.push((obj, shape, v));
        v
    }

    fn push(&mut self, op: Op, rep: Rep, fs: Option<u32>) -> V {
        if op.may_reshape() {
            self.shape_memo.clear();
            self.const_obj_memo.clear();
            self.global_memo.clear();
        } else if matches!(op, Op::StoreGlobal(..)) {
            self.global_memo.clear();
        }
        let v = self.insts.len() as V;
        self.insts.push(Inst {
            op,
            rep,
            fs,
            block: self.cur_block,
        });
        v
    }

    fn push_out(&mut self, op: Op, rep: Rep, fs: Option<u32>) -> V {
        let v = self.push(op, rep, fs);
        self.out.push(v);
        v
    }

    fn emit(&mut self, op: Op, rep: Rep) -> V {
        self.push_out(op, rep, None)
    }

    /// The constant `op`, made once at the entry (`consts`).
    fn konst(&mut self, op: Op, rep: Rep) -> V {
        let bits = match op {
            Op::ConstI32(c) => c as u32 as u64,
            Op::ConstF64(x) => x.to_bits(),
            Op::ConstBool(b) => u64::from(b),
            Op::ConstTagged(t) => t,
            _ => unreachable!("not a constant: {op:?}"),
        };
        if let Some(&(_, _, v)) = self.consts.iter().find(|&&(r, b, _)| r == rep && b == bits) {
            return v;
        }
        let v = self.insts.len() as V;
        self.insts.push(Inst {
            op,
            rep,
            fs: None,
            block: 0,
        });
        self.consts.push((rep, bits, v));
        v
    }

    fn rep(&self, v: V) -> Rep {
        self.insts[v as usize].rep
    }

    fn memo_get(&self, v: V, r: Rep) -> Option<V> {
        self.memo.iter().find(|&&(a, b, _)| a == v && b == r).map(|&(_, _, c)| c)
    }

    /// The straight-line callee `c` inlined into the current block (see
    /// `inlinable`): `this` and the arguments its registers start with,
    /// every guard the call's frame state `call_fs`. Its result, in its
    /// own representation.
    /// `inline_sim`'s way through `c` for these arguments (their
    /// representations), the call inlined only when there is one.
    fn inline_path(&self, callee: &super::OptCallee, args: &[V]) -> Option<Vec<usize>> {
        let c = callee.proto;
        let mut reps = vec![Rep::Tagged; c.reg_count as usize];
        for (k, &a) in args.iter().take(c.param_count as usize).enumerate() {
            reps[1 + k] = self.rep(a);
        }
        inline_sim(callee, reps, args.len()).map(|(_, path)| path)
    }

    /// Inline `callee` at the current call site: straight-line when `inline_sim`
    /// folds every branch, else multi-block forward-DAG inlining (`inline_cfg`).
    #[allow(clippy::too_many_arguments)]
    fn inline_call(
        &mut self,
        callee: &super::OptCallee,
        callee_fid: u32,
        this: V,
        args: &[V],
        call_fs: u32,
        callees: &FxHashMap<u32, super::OptCallee>,
        depth: u8,
    ) -> Option<V> {
        let res = if let Some(path) = self.inline_path(callee, args) {
            self.inline(callee, this, args, call_fs, &path, callees, depth)
        } else {
            self.inline_cfg(callee, this, args, call_fs, callees, depth)
        };
        if res.is_some() && !self.inlined_callees.contains(&callee_fid) {
            self.inlined_callees.push(callee_fid);
        }
        res
    }

    /// Straight-line inlining along `path` (`inline_path`): every guard takes
    /// `call_fs`. Returns the callee's result in its own representation.
    #[allow(clippy::too_many_arguments)]
    fn inline(
        &mut self,
        callee_info: &super::OptCallee,
        this: V,
        args: &[V],
        call_fs: u32,
        path: &[usize],
        callees: &FxHashMap<u32, super::OptCallee>,
        depth: u8,
    ) -> Option<V> {
        let c = callee_info.proto;
        let undef = self.konst(Op::ConstTagged(Value::UNDEFINED.bits()), Rep::Tagged);
        let mut defs = vec![undef; c.reg_count as usize];
        defs[0] = this;
        for (k, &a) in args.iter().take(c.param_count as usize).enumerate() {
            defs[1 + k] = a;
        }
        let math = self.math;
        let heap = self.heap;
        let callee = Fn {
            p: c,
            is_top: false,
            func_id: u64::MAX,
            trunc: super::super::emit_misc::trunc_only_arith_ips(c),
            handlers: false,
            plan: &callee_info.prop_shape,
            strs: Some(&callee_info.const_strs),
            math: math.as_ref(),
            math_reads: math_reads_known(&c.code, math.as_ref()),
            generic: None,
            addt: None,
            ushr_i: None,
            new_lanes: None,
            ta_regs: &[],
            class_values: None,
            super_parents: None,
            poly: Some(&callee_info.poly),
            const_props: Some(&callee_info.const_props),
            slow_global_loads: None,
            callee_sites: (depth == 1).then_some(&callee_info.sites),
            callees,
            inline_depth: depth,
            arith_fb: None,
            plain_closures: None,
            multi_prop: &[],
            heap: &heap,
        };
        let mut fs = FsCtx {
            ip: 0,
            live: None,
            readonly_param: &[],
            id: Some(call_fs),
        };
        for &ip in path {
            match translate(self, &callee, ip, &c.code[ip], &mut defs, &mut fs, None)? {
                Flow::Next => {}
                Flow::Ret(v) => {
                    self.inlined += 1;
                    return Some(v);
                }
                Flow::Jump(_) | Flow::Branch { .. } | Flow::Exit => unreachable!("the path holds no branch"),
            }
        }
        None
    }

    /// Multi-block forward-DAG inlining for small pure callees with runtime
    /// branches (`if (!node) return false; return node.type === "Identifier";`)
    /// and optional depth-2 nested pure calls (`isAsciiLetter` calling
    /// `isAsciiUpper` / `isAsciiLower`). Uses two-phase block entry/exit
    /// tracking so a callee block that splits via nested `inline_cfg` patches
    /// its predecessor and terminator edges cleanly in RPO.
    #[allow(clippy::too_many_arguments)]
    fn inline_cfg(
        &mut self,
        callee_info: &super::OptCallee,
        this: V,
        args: &[V],
        call_fs: u32,
        callees: &FxHashMap<u32, super::OptCallee>,
        depth: u8,
    ) -> Option<V> {
        let c = callee_info.proto;
        let code = &c.code;
        let n = code.len();
        let nregs = c.reg_count as usize;
        // 1. Callee basic blocks (strictly forward-branching DAG).
        let mut leader = vec![false; n];
        leader[0] = true;
        for (ip, i) in code.iter().enumerate() {
            let (ft, t) = flow(i, ip);
            if let Some(t) = t {
                leader[t] = true;
                if ip + 1 < n {
                    leader[ip + 1] = true;
                }
            }
            if ft.is_none() && ip + 1 < n {
                leader[ip + 1] = true;
            }
        }
        let c_starts: Vec<usize> = (0..n).filter(|&ip| leader[ip]).collect();
        let c_nb = c_starts.len();
        let c_end_of = |cb: usize| if cb + 1 < c_nb { c_starts[cb + 1] } else { n };
        let c_block_of_ip = |ip: usize| c_starts.partition_point(|&s| s <= ip) - 1;
        let mut c_succs: Vec<Vec<usize>> = vec![Vec::new(); c_nb];
        for cb in 0..c_nb {
            let last = c_end_of(cb) - 1;
            let (ft, t) = flow(&code[last], last);
            if let Some(fip) = ft {
                let s = c_block_of_ip(fip);
                if !c_succs[cb].contains(&s) {
                    c_succs[cb].push(s);
                }
            }
            if let Some(tip) = t {
                let s = c_block_of_ip(tip);
                if !c_succs[cb].contains(&s) {
                    c_succs[cb].push(s);
                }
            }
        }
        let mut reach = vec![false; c_nb];
        reach[0] = true;
        for cb in 0..c_nb {
            if reach[cb] {
                for &s in &c_succs[cb] {
                    reach[s] = true;
                }
            }
        }
        let reach_list: Vec<usize> = (0..c_nb).filter(|&cb| reach[cb]).collect();
        let mut c_preds: Vec<Vec<usize>> = vec![Vec::new(); c_nb];
        for &cb in &reach_list {
            for &s in &c_succs[cb] {
                if reach[s] {
                    c_preds[s].push(cb);
                }
            }
        }
        debug_assert!(c_preds[0].is_empty(), "forward-DAG callee entry has no predecessors");
        debug_assert_eq!(self.cur_block as usize, self.blocks.len());

        let undef = self.konst(Op::ConstTagged(Value::UNDEFINED.bits()), Rep::Tagged);
        let math = self.math;
        let heap = self.heap;
        let callee = Fn {
            p: c,
            is_top: false,
            func_id: u64::MAX,
            trunc: super::super::emit_misc::trunc_only_arith_ips(c),
            handlers: false,
            plan: &callee_info.prop_shape,
            strs: Some(&callee_info.const_strs),
            math: math.as_ref(),
            math_reads: math_reads_known(&c.code, math.as_ref()),
            generic: None,
            addt: None,
            ushr_i: None,
            new_lanes: None,
            ta_regs: &[],
            class_values: None,
            super_parents: None,
            poly: Some(&callee_info.poly),
            const_props: Some(&callee_info.const_props),
            slow_global_loads: None,
            callee_sites: (depth == 1).then_some(&callee_info.sites),
            callees,
            inline_depth: depth,
            arith_fb: None,
            plain_closures: None,
            multi_prop: &[],
            heap: &heap,
        };
        let mut fs = FsCtx {
            ip: 0,
            live: None,
            readonly_param: &[],
            id: Some(call_fs),
        };

        enum InlineTerm {
            Jump(usize),
            Branch { cond: V, t: usize, f: usize },
            Ret,
        }
        let mut c_entry_ir: Vec<B> = vec![B::MAX; c_nb];
        let mut c_exit_ir: Vec<B> = vec![B::MAX; c_nb];
        let mut pending_c_terms: Vec<(B, InlineTerm)> = Vec::with_capacity(reach_list.len());
        let mut c_exit_defs: Vec<Vec<V>> = vec![Vec::new(); c_nb];
        let mut c_exit_shapes: Vec<Vec<(V, u32, V)>> = vec![Vec::new(); c_nb];
        let mut c_exit_atoms: Vec<Vec<(V, V)>> = vec![Vec::new(); c_nb];
        let mut c_exit_const_objs: Vec<Vec<(V, u32, u32)>> = vec![Vec::new(); c_nb];
        let mut c_exit_globals: Vec<Vec<(u32, V)>> = vec![Vec::new(); c_nb];
        let mut returns: Vec<(B, V)> = Vec::new();
        for (j, &cb) in reach_list.iter().enumerate() {
            let mut c_defs: Vec<V>;
            if j == 0 {
                c_entry_ir[cb] = self.cur_block;
                c_defs = vec![undef; nregs];
                c_defs[0] = this;
                for (k, &a) in args.iter().take(c.param_count as usize).enumerate() {
                    c_defs[1 + k] = a;
                }
            } else {
                let entry_id = self.blocks.len() as B;
                c_entry_ir[cb] = entry_id;
                self.cur_block = entry_id;
                self.out.clear();
                self.memo.clear();
                self.shape_memo.clear();
                self.atom_memo.clear();
                self.const_obj_memo.clear();
                self.global_memo.clear();
                let preds_cb = &c_preds[cb];
                self.cur_preds = preds_cb.iter().map(|&p| c_exit_ir[p]).collect();
                if preds_cb.len() == 1 {
                    let p = preds_cb[0];
                    c_defs = c_exit_defs[p].clone();
                    self.shape_memo = c_exit_shapes[p].clone();
                    self.atom_memo = c_exit_atoms[p].clone();
                    self.const_obj_memo = c_exit_const_objs[p].clone();
                    self.global_memo = c_exit_globals[p].clone();
                } else {
                    c_defs = vec![undef; nregs];
                    for r in 0..nregs {
                        let first_v = c_exit_defs[preds_cb[0]][r];
                        if preds_cb.iter().all(|&p| c_exit_defs[p][r] == first_v) {
                            c_defs[r] = first_v;
                        } else {
                            let want = preds_cb
                                .iter()
                                .fold(self.rep(first_v), |acc, &p| Rep::join(acc, self.rep(c_exit_defs[p][r])));
                            let mut phi_inputs = Vec::with_capacity(preds_cb.len());
                            for &p in preds_cb {
                                let pv = c_exit_defs[p][r];
                                let have = self.rep(pv);
                                let wv = if have == want {
                                    pv
                                } else {
                                    let op = match (have, want) {
                                        (Rep::I32, Rep::F64) => Op::I32ToF64(pv),
                                        (_, Rep::Tagged) => Op::Box(pv),
                                        _ => unreachable!("inline phi only widens: {have:?} to {want:?}"),
                                    };
                                    let conv = self.push(op, want, None);
                                    let pb = c_exit_ir[p];
                                    self.insts[conv as usize].block = pb;
                                    self.blocks[pb as usize].insts.push(conv);
                                    conv
                                };
                                phi_inputs.push(wv);
                            }
                            let phi = self.push_out(Op::Phi(phi_inputs), want, None);
                            c_defs[r] = phi;
                        }
                    }
                }
            }
            let mut term: Option<InlineTerm> = None;
            for ip in c_starts[cb]..c_end_of(cb) {
                match translate(self, &callee, ip, &code[ip], &mut c_defs, &mut fs, None)? {
                    Flow::Next => {}
                    Flow::Jump(t) => term = Some(InlineTerm::Jump(c_block_of_ip(t))),
                    Flow::Branch { cond, t, f } => {
                        term = Some(InlineTerm::Branch {
                            cond,
                            t: c_block_of_ip(t),
                            f: c_block_of_ip(f),
                        });
                    }
                    Flow::Ret(rv) => {
                        returns.push((self.cur_block, rv));
                        term = Some(InlineTerm::Ret);
                    }
                    Flow::Exit => unreachable!("inlined callee has no exit"),
                }
            }
            let exit_id = self.cur_block;
            debug_assert_eq!(exit_id as usize, self.blocks.len());
            c_exit_ir[cb] = exit_id;
            let iterm = term.unwrap_or_else(|| InlineTerm::Jump(c_block_of_ip(c_end_of(cb))));
            pending_c_terms.push((exit_id, iterm));
            c_exit_defs[cb] = c_defs;
            c_exit_shapes[cb] = std::mem::take(&mut self.shape_memo);
            c_exit_atoms[cb] = std::mem::take(&mut self.atom_memo);
            c_exit_const_objs[cb] = std::mem::take(&mut self.const_obj_memo);
            c_exit_globals[cb] = std::mem::take(&mut self.global_memo);
            self.blocks.push(Block {
                insts: std::mem::take(&mut self.out),
                term: Term::Exit,
                preds: std::mem::take(&mut self.cur_preds),
                entry_state: None,
            });
        }
        let cont_id = self.blocks.len() as B;
        for (exit_id, iterm) in pending_c_terms {
            self.blocks[exit_id as usize].term = match iterm {
                InlineTerm::Jump(t) => Term::Jump(c_entry_ir[t]),
                InlineTerm::Branch { cond, t, f } => {
                    let tb = c_entry_ir[t];
                    let fb = c_entry_ir[f];
                    if tb == fb {
                        Term::Jump(tb)
                    } else {
                        Term::Branch { cond, t: tb, f: fb }
                    }
                }
                InlineTerm::Ret => Term::Jump(cont_id),
            };
        }

        // Open the continuation block for the caller's remaining ops.
        self.cur_block = cont_id;
        self.out.clear();
        self.memo.clear();
        self.shape_memo.clear();
        self.atom_memo.clear();
        self.const_obj_memo.clear();
        self.global_memo.clear();
        self.cur_preds = returns.iter().map(|&(pb, _)| pb).collect();
        if returns.is_empty() {
            return None;
        }
        let ret_v = if returns.len() == 1 {
            returns[0].1
        } else {
            let want = returns
                .iter()
                .fold(self.rep(returns[0].1), |acc, &(_, rv)| Rep::join(acc, self.rep(rv)));
            let mut phi_inputs = Vec::with_capacity(returns.len());
            for &(pb, rv) in &returns {
                let have = self.rep(rv);
                let wv = if have == want {
                    rv
                } else {
                    let op = match (have, want) {
                        (Rep::I32, Rep::F64) => Op::I32ToF64(rv),
                        (_, Rep::Tagged) => Op::Box(rv),
                        _ => unreachable!("inline return phi only widens: {have:?} to {want:?}"),
                    };
                    let conv = self.push(op, want, None);
                    self.insts[conv as usize].block = pb;
                    self.blocks[pb as usize].insts.push(conv);
                    conv
                };
                phi_inputs.push(wv);
            }
            self.push_out(Op::Phi(phi_inputs), want, None)
        };
        self.inlined += 1;
        Some(ret_v)
    }

    /// `x <cmp> y`: on Numbers, unless the site deopted before (`generic`)
    /// with a boxed operand, which then compares strings too (`CmpT`).
    #[allow(clippy::too_many_arguments)]
    fn compare(&mut self, f: &Fn, ip: usize, cmp: Cmp, x: V, y: V, fs: &mut FsCtx, defs: &[V]) -> V {
        if f.generic(ip) && (self.rep(x) == Rep::Tagged || self.rep(y) == Rep::Tagged) {
            let xt = self.tagged(x);
            let yt = self.tagged(y);
            let s = fs.id(self, defs);
            return self.push_out(Op::CmpT(cmp, xt, yt), Rep::Bool, Some(s));
        }
        if self.rep(x) == Rep::I32 && self.rep(y) == Rep::I32 {
            return self.emit(Op::CmpI(cmp, x, y), Rep::Bool);
        }
        let xf = self.f64(x, fs, defs);
        let yf = self.f64(y, fs, defs);
        self.emit(Op::CmpF(cmp, xf, yf), Rep::Bool)
    }

    /// `v` as an element key: a Number as it is (an i32 or a double), any
    /// other value boxed.
    fn index_key(&mut self, v: V) -> V {
        match self.rep(v) {
            Rep::I32 | Rep::F64 => v,
            _ => self.tagged(v),
        }
    }

    /// `v` as an f64; a boxed value is unboxed behind a guard.
    fn f64(&mut self, v: V, fs: &mut FsCtx, defs: &[V]) -> V {
        if let Op::ConstI32(c) = self.insts[v as usize].op {
            return self.konst(Op::ConstF64(c as f64), Rep::F64);
        }
        match self.rep(v) {
            Rep::F64 => v,
            r => {
                if let Some(c) = self.memo_get(v, Rep::F64) {
                    return c;
                }
                let c = match r {
                    Rep::I32 => self.emit(Op::I32ToF64(v), Rep::F64),
                    Rep::Tagged => {
                        let s = fs.id(self, defs);
                        self.push_out(Op::TaggedToF64(v), Rep::F64, Some(s))
                    }
                    // `true + 1` is a Number, but rare: the frame takes it.
                    _ => {
                        let t = self.tagged(v);
                        let s = fs.id(self, defs);
                        self.push_out(Op::TaggedToF64(t), Rep::F64, Some(s))
                    }
                };
                self.memo.push((v, Rep::F64, c));
                c
            }
        }
    }

    /// `v` as an i32 by ToInt32.
    fn i32(&mut self, v: V, fs: &mut FsCtx, defs: &[V]) -> V {
        if self.rep(v) == Rep::I32 {
            return v;
        }
        if let Op::ConstF64(x) = self.insts[v as usize].op {
            let c = Value::num(x);
            if c.is_int() {
                return self.konst(Op::ConstI32(c.as_int()), Rep::I32);
            }
        }
        if let Some(c) = self.memo_get(v, Rep::I32) {
            return c;
        }
        let c = if self.rep(v) == Rep::Tagged && self.memo_get(v, Rep::F64).is_none() {
            // A boxed Int needs no double on its way.
            let s = fs.id(self, defs);
            self.push_out(Op::TaggedToI32(v), Rep::I32, Some(s))
        } else {
            let x = self.f64(v, fs, defs);
            let s = fs.id(self, defs);
            self.push_out(Op::F64ToI32(x), Rep::I32, Some(s))
        };
        self.memo.push((v, Rep::I32, c));
        c
    }

    /// `v`'s truthiness as a Bool.
    fn truthy(&mut self, v: V, fs: &mut FsCtx, defs: &[V]) -> V {
        match self.rep(v) {
            Rep::Bool => v,
            Rep::I32 => self.emit(Op::I32Truthy(v), Rep::Bool),
            Rep::F64 => self.emit(Op::F64Truthy(v), Rep::Bool),
            _ => {
                let s = fs.id(self, defs);
                self.push_out(Op::TaggedTruthy(v), Rep::Bool, Some(s))
            }
        }
    }

    /// `v` boxed.
    fn tagged(&mut self, v: V) -> V {
        if self.rep(v) == Rep::Tagged {
            return v;
        }
        let boxed = match self.insts[v as usize].op {
            Op::ConstI32(c) => Some(Value::int(c)),
            Op::ConstF64(x) => Some(Value::num(x)),
            Op::ConstBool(b) => Some(Value::bool(b)),
            _ => None,
        };
        if let Some(c) = boxed {
            return self.konst(Op::ConstTagged(c.bits()), Rep::Tagged);
        }
        if let Some(c) = self.memo_get(v, Rep::Tagged) {
            return c;
        }
        let c = self.emit(Op::Box(v), Rep::Tagged);
        self.memo.push((v, Rep::Tagged, c));
        c
    }
}
