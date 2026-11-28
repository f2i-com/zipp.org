#![allow(unused_imports)]
use super::*;
use crate::bytecode::{Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PromiseState, PropAttr, ReactionPair, Reactions,
};
use crate::value::Value;

/// A compiled callback an array builtin calls over one reused register
/// window (see `Vm::native_cb_entry`), with what each call rechecks.
#[derive(Clone, Copy)]
#[cfg_attr(not(all(feature = "jit", target_arch = "x86_64")), allow(dead_code))]
pub(crate) struct NativeCb {
    /// The body's native entry when the builtin started.
    entry: *const u8,
    /// The window's size: the callee's register count.
    pub(crate) callee_regs: usize,
    param_count: usize,
    /// The `this` OrdinaryCallBindThis gives the callee, the same each call.
    this: Value,
    fid: u32,
    /// The registers reset to `undefined` before each call: the body's
    /// may-read-before-write set (a Tier C body's, as the native cross-call
    /// fills it), or `u64::MAX` for every register past the arguments.
    uninit_mask: u64,
    /// The global slot and value a self-calling body assumed (`try_run_jit`).
    self_binding: Option<(u32, u64)>,
    /// A closure with captures: the base of its upvalue list, installed as
    /// the activation's around each call, or 0. Read once: the list is fixed
    /// at the closure's creation and the closure, the builtin's callback, is
    /// rooted for the whole builtin, so the base cannot move under it. It was
    /// a heap lookup per element.
    upvals_raw: u64,
    /// The JIT's evictions when `entry` was taken (`Jit::retired_count`):
    /// after one, `entry` may be a parked body its function has replaced.
    retired: usize,
}

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) const NATIVE_CB_CACHE_SLOTS: usize = 64;

/// Direct-mapped cache entry for [`Vm::native_cb_entry`]: caches per-`fid`
/// eligibility, native entry, int-body entry, register/param counts, and
/// uninit mask, guarded by `Jit::state_epoch` and `Vm::global_route_epoch`.
/// Avoids re-scanning `proto.code` (`proto_has_handler_ops`) and re-probing
/// `Jit::compiled` / `Jit::cross_entries` on every `Array.prototype.{map,filter,
/// forEach,some,every,find,findIndex,reduce}` call.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[derive(Clone, Copy)]
pub(crate) struct NativeCbCacheEntry {
    fid: u32,
    jit_epoch: u32,
    global_epoch: u32,
    entry: usize,
    int_entry: usize,
    callee_regs: u16,
    param_count: u16,
    uninit_mask: u64,
    self_binding: Option<(u32, u64)>,
    int_self_binding: Option<(u32, u64)>,
    retired: usize,
    /// bit 0: is_strict, bit 1: has_cross, bit 2: ineligible
    flags: u8,
}

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
impl NativeCbCacheEntry {
    pub(crate) const EMPTY: Self = Self {
        fid: u32::MAX,
        jit_epoch: 0,
        global_epoch: 0,
        entry: 0,
        int_entry: 0,
        callee_regs: 1,
        param_count: 0,
        uninit_mask: u64::MAX,
        self_binding: None,
        int_self_binding: None,
        retired: 0,
        flags: 0,
    };
}

impl<'p> Vm<'p> {
    /// Whether `idx`'s prototype chain reaches one of the error prototypes — i.e.
    /// it's a real error instance (created via `new TypeError` or an internal
    /// throw), as opposed to a plain object that merely has a `name` property.
    pub(crate) fn is_error_instance(&self, idx: u32) -> bool {
        if self.error_protos[0] == 0 {
            return false;
        }
        // proto_of chain: direct error instances + `Object.create(XError.prototype)`.
        let mut cur = idx;
        for _ in 0..64 {
            match self.proto_of.get_copied(cur) {
                Some(p) if p.is_heap() => {
                    let pi = p.heap_index();
                    if self.error_protos.contains(&pi) {
                        return true;
                    }
                    cur = pi;
                }
                _ => break,
            }
        }
        // class `extends` chain: a `class X extends Error` instance has no
        // proto_of entry (its prototype is reached via map.class). Walk the class
        // chain and check whether any class's parent is a built-in error ctor.
        if let HeapObj::Object(m) = self.heap.get(idx) {
            let mut c = m.class;
            for _ in 0..64 {
                let ci = match c {
                    Some(ci) => ci,
                    None => break,
                };
                match self.heap.get(ci) {
                    HeapObj::Class(cd) => match cd.parent {
                        Some(par) if self.error_ctors.contains(&par) => return true,
                        Some(par) => c = Some(par),
                        None => break,
                    },
                    _ => break,
                }
            }
        }
        false
    }

    /// Read a DATA property from `idx` walking the `proto_of` chain (no getters,
    /// no class methods) — used by the read-only `display`/ToString path for error
    /// instances, where `name`/`message` may be inherited from the prototype.
    pub(crate) fn read_data_prop(&self, idx: u32, key: &str) -> Option<Value> {
        let mut cur = idx;
        for _ in 0..64 {
            if let HeapObj::Object(m) = self.heap.get(cur) {
                if let Some(v) = m.get(key) {
                    return Some(v);
                }
            }
            match self.proto_of.get_copied(cur) {
                Some(p) if p.is_heap() => cur = p.heap_index(),
                _ => return None,
            }
        }
        None
    }

    /// Methods on a number receiver: `toFixed`, `toString`. Returns `Ok(None)`
    /// for an unrecognised name (the caller then treats it as a missing property
    /// → TypeError, matching JS).
    pub(crate) fn number_method(
        &mut self,
        recv: Value,
        name: &str,
        args: &[Value],
    ) -> Result<Option<Value>, Thrown> {
        // thisNumberValue brand check: a Number primitive uses its value; the
        // Number.prototype object itself has [[NumberData]] = +0; anything else
        // (a String/object via `Number.prototype.toString.call(x)`) is a TypeError.
        let n = if recv.is_number() {
            recv.as_f64()
        } else if recv.is_heap() && recv.heap_index() == self.num_proto {
            0.0
        } else {
            return Err(Thrown::from_static(
                "TypeError: Number.prototype method called on a non-Number",
            ));
        };
        let nv = if recv.is_number() {
            recv
        } else {
            Value::num(n)
        };
        // ToIntegerOrInfinity(ToNumber(arg)): coerce (valueOf/string), truncate;
        // NaN -> 0. Absent arg behaves as `undefined` -> NaN -> 0.
        let int_arg = |vm: &mut Self, i: usize| -> Result<f64, Thrown> {
            // ToIntegerOrInfinity goes through ToNumber, so a BigInt/Symbol digits
            // argument is a TypeError (not silently coerced).
            let raw = vm.to_number_strict(args.get(i).copied().unwrap_or(Value::UNDEFINED))?;
            Ok(if raw.is_nan() { 0.0 } else { raw.trunc() })
        };
        match name {
            "toFixed" => {
                let d = int_arg(self, 0)?;
                if d < 0.0 || d > 100.0 {
                    return Err(Thrown::from_static(
                        "RangeError: toFixed() digits argument must be between 0 and 100",
                    ));
                }
                Ok(Some(self.alloc_str(to_fixed(n, d as usize))))
            }
            "toString" => {
                // An absent/undefined radix defaults to 10; otherwise it is
                // ToIntegerOrInfinity(ToNumber(radix)) and must be 2..36.
                let arg = args.first().copied().unwrap_or(Value::UNDEFINED);
                if arg == Value::UNDEFINED {
                    return Ok(Some(self.alloc_str(self.display(nv))));
                }
                let r = int_arg(self, 0)? as i64;
                if !(2..=36).contains(&r) {
                    return Err(Thrown::from_static(
                        "RangeError: toString() radix must be between 2 and 36",
                    ));
                }
                if r == 10 {
                    Ok(Some(self.alloc_str(self.display(nv))))
                } else {
                    Ok(Some(self.alloc_str(num_to_radix(n, r as u32))))
                }
            }
            "valueOf" => Ok(Some(nv)),
            // ECMA-402 Number.prototype.toLocaleString: Construct an
            // Intl.NumberFormat from (locales, options) and FormatNumeric through
            // it — so grouping, style and the digit options apply here too, and a
            // bad option throws exactly as the constructor would.
            #[cfg(feature = "wasm-lite")]
            "toLocaleString" => Ok(Some(self.alloc_str(self.display(nv)))),
            #[cfg(not(feature = "wasm-lite"))]
            "toLocaleString" => {
                let locales = args.first().copied().unwrap_or(Value::UNDEFINED);
                let options = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let nf = self.make_intl(native::INTL_NUMBERFORMAT, locales, options)?;
                let resolved = match self.heap.get(nf.heap_index()) {
                    HeapObj::Intl { resolved, .. } => *resolved,
                    _ => return Ok(Some(self.alloc_str(self.display(nv)))),
                };
                Ok(Some(self.intl_number_format(resolved, nv)?))
            }
            "toExponential" => {
                let arg = args.first().copied().unwrap_or(Value::UNDEFINED);
                let digits = if arg == Value::UNDEFINED {
                    None
                } else {
                    let d = int_arg(self, 0)?; // ToNumber side effect (valueOf) first
                                               // A NaN/Infinity receiver returns "NaN"/"Infinity" BEFORE the
                                               // RangeError on the digits argument (spec steps 4 & 7).
                    if !n.is_finite() {
                        return Ok(Some(self.alloc_str(fmt_exponential(n, None))));
                    }
                    if d < 0.0 || d > 100.0 {
                        return Err(Thrown::from_static(
                            "RangeError: toExponential() argument must be between 0 and 100",
                        ));
                    }
                    Some(d as usize)
                };
                Ok(Some(self.alloc_str(fmt_exponential(n, digits))))
            }
            "toPrecision" => {
                let arg = args.first().copied().unwrap_or(Value::UNDEFINED);
                if arg == Value::UNDEFINED {
                    return Ok(Some(self.alloc_str(self.display(nv))));
                }
                let p = int_arg(self, 0)?; // ToNumber side effect (valueOf) first
                                           // A NaN/Infinity receiver returns "NaN"/"Infinity" BEFORE the
                                           // RangeError on the precision argument (spec steps 4 & 7).
                if !n.is_finite() {
                    return Ok(Some(self.alloc_str(self.display(nv))));
                }
                if p < 1.0 || p > 100.0 {
                    return Err(Thrown::from_static(
                        "RangeError: toPrecision() argument must be between 1 and 100",
                    ));
                }
                Ok(Some(self.alloc_str(fmt_precision(n, p as usize))))
            }
            _ => Ok(None),
        }
    }

    /// `Boolean.prototype.toString`/`valueOf` on a boolean value.
    pub(crate) fn boolean_method(&mut self, recv: Value, name: &str) -> Result<Value, Thrown> {
        // thisBooleanValue: the receiver must be a boolean primitive (or a Boolean
        // wrapper, normally already unwrapped by the caller). Anything else is a
        // TypeError — `Boolean.prototype.toString.call(5)` must throw, not coerce.
        let b = if recv.is_bool() {
            recv
        } else if let Some(HeapObj::Boxed { kind: 2, value }) =
            recv.is_heap().then(|| self.heap.get(recv.heap_index()))
        {
            *value
        } else if recv.is_heap() && self.bool_proto != 0 && recv.heap_index() == self.bool_proto {
            // %Boolean.prototype% is itself a Boolean object whose [[BooleanData]]
            // is false — so `Boolean.prototype.valueOf()` is `false`, not a throw.
            Value::bool(false)
        } else {
            return Err(Thrown::join3("TypeError: Boolean.prototype.", &*name, " requires that 'this' be a Boolean"));
        };
        Ok(match name {
            "toString" => self.alloc_str(
                if b == Value::bool(true) {
                    "true"
                } else {
                    "false"
                }
                .to_string(),
            ),
            "valueOf" => b,
            _ => Value::UNDEFINED,
        })
    }

    /// Resolve `cb` to the native entry of a COMPILED, non-capturing JIT function
    /// for the array-builtin fast path (`map`/`filter`/`forEach`/`reduce`).
    /// Returns the callback's [`NativeCb`] handle or `None` if `cb`
    /// must go through the interpreter `call_value` (not a plain function,
    /// JIT disabled, inside a deopted self-call continuation, a `this` that
    /// must be bound per call, or not JIT-compilable). Compiles a
    /// non-capturing `cb` on first use if eligible — array builtins call the
    /// same callback many times, so we don't wait for the call-count
    /// threshold; an ineligible proto is blacklisted by `compile` and returns
    /// `None` cheaply thereafter.
    ///
    /// A capturing closure is taken once its body has a frame-free native
    /// entry of its own (compiled by its own heat, with the entry the native
    /// cross-call uses): `invoke_cb_windowed` installs its captures as that
    /// call does. Until then, and for a closure made under direct `eval` or
    /// while another realm exists, it goes through `call_value`.
    ///
    /// `this` is what OrdinaryCallBindThis gives the callee for `this_arg`, the
    /// same for every element: an arrow's lexical `this`, a strict callee's
    /// `this_arg`, and for a sloppy one the global object when `this_arg` is
    /// nullish. The native window used to hold the raw `this_arg`, so a sloppy
    /// compiled callback saw `this === undefined`. A sloppy callee given a
    /// primitive needs a FRESH wrapper per call, which `call_value` builds.
    /// Resolved here once, not per element: a sort comparator runs n·log n
    /// times, and a per-call binding measured ~2ns on each.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn native_cb_entry(&mut self, cb: Value, this_arg: Value) -> Option<NativeCb> {
        // Mirror the interpreter's JIT-entry guard: respect ZIPP_NOJIT and never
        // enter native code from a deopted self-call continuation (livelock).
        if !self.jit_fused_ok() || self.jit_recurse_depth != 0 || !cb.is_heap() {
            return None;
        }
        // The window has no frame of its own, and the helpers a native body
        // calls rely on one lying under it (a frame call records the caller's
        // resume point in the top frame). The code that called the builtin
        // has one, except when the builtin itself is what the host runs: a
        // bound `forEach` as a promise reaction, whose compiled callback then
        // called a function, indexed an empty frame stack and panicked. An
        // int-tier body calls no helper but its own self-call, which pushes
        // the frames it needs, so one already compiled runs without (a
        // promise reaction's callback the drain calls, `drain_microtasks`).
        let (fid, ups) = self.heap.as_callable(cb.heap_index())?;
        let frameless = self.frames.is_empty();
        // A capturing closure reads upvalue cells (heap) — outside the
        // leaf-int JIT this function compiles with. It needs the body its own
        // calls compiled, and the conditions of a frame-free native call (the
        // ones an emitted cross-call site tests before it enters one).
        let capturing = ups.is_some_and(|u| !u.is_empty());
        let upvals_raw = if capturing {
            self.heap.upvals_mirror_of(cb.heap_index())
        } else {
            0
        };
        if capturing
            && (!self.closure_eval_scope.is_empty()
                || !self.obj_realm.is_empty()
                || !self.realm_global_objs.is_empty())
        {
            return None;
        }
        let jit_epoch = self.jit.state_epoch();
        let cache_slot = (fid as usize) & (NATIVE_CB_CACHE_SLOTS - 1);
        let cached = self.native_cb_cache[cache_slot];
        if cached.fid == fid
            && cached.jit_epoch == jit_epoch
            && cached.global_epoch == self.global_route_epoch
        {
            if cached.flags & 4 != 0 {
                return None;
            }
            let (entry_addr, self_binding, uninit_mask) = if frameless {
                if capturing || cached.int_entry == 0 {
                    return None;
                }
                (cached.int_entry, cached.int_self_binding, u64::MAX)
            } else {
                if (capturing && cached.flags & 2 == 0) || cached.entry == 0 {
                    return None;
                }
                (cached.entry, cached.self_binding, cached.uninit_mask)
            };
            let is_strict = cached.flags & 1 != 0;
            let this = if let Some(lexical) = self.arrow_captured_this(cb) {
                lexical
            } else if is_strict || self.global_this == 0 || self.is_object_value(this_arg) {
                this_arg
            } else if this_arg.is_nullish() {
                Value::heap(self.callee_this_global(cb))
            } else {
                return None;
            };
            return Some(NativeCb {
                entry: entry_addr as *const u8,
                callee_regs: cached.callee_regs as usize,
                param_count: cached.param_count as usize,
                this,
                fid,
                uninit_mask,
                self_binding,
                upvals_raw,
                retired: cached.retired,
            });
        }
        if frameless && !(!capturing && self.jit.int_body(fid).is_some()) {
            return None;
        }
        // A callback that materialises `arguments` needs the interpreter's call
        // setup (the JIT window never builds the arguments object) — same guard
        // as the fused-kernel paths. An async or generator callback returns the
        // Promise / generator object only that call setup creates; running its
        // compiled body would hand back the raw completion value instead.
        let is_strict = {
            let proto = self.func(fid as usize);
            // A rest parameter's array is built by call setup too (and the
            // window below drops the arguments past the parameters). A body
            // with handler ops (`try`, `finally`) pushes its handlers on the
            // top frame, which under this window is the caller's: a throw in
            // the callback unwound to the caller's handler stack, so a
            // `finally` never ran, a `catch` was missed, and the caller
            // resumed at the callback's handler target (a hang, a stray
            // TypeError, an index past the caller's code). Such a body takes
            // only frame-backed entries, as a cross call's does.
            if proto.arguments_reg.is_some()
                || proto.rest_reg.is_some()
                || proto.is_async
                || proto.is_generator
                || crate::codegen::proto_has_handler_ops(proto)
            {
                self.native_cb_cache[cache_slot] = NativeCbCacheEntry {
                    fid,
                    jit_epoch,
                    global_epoch: self.global_route_epoch,
                    flags: 4,
                    ..NativeCbCacheEntry::EMPTY
                };
                return None;
            }
            proto.is_strict
        };
        let this = if let Some(lexical) = self.arrow_captured_this(cb) {
            lexical
        } else if is_strict || self.global_this == 0 || self.is_object_value(this_arg) {
            this_arg
        } else if this_arg.is_nullish() {
            Value::heap(self.callee_this_global(cb))
        } else {
            return None;
        };
        // A callback whose compile failed for good stays interpreted: trying
        // again built every plan and declined on each call of the builtin —
        // `xs.map(f)` with an uncompilable `f`, once per `map`.
        if self.jit.is_blacklisted(fid) {
            self.native_cb_cache[cache_slot] = NativeCbCacheEntry {
                fid,
                jit_epoch,
                global_epoch: self.global_route_epoch,
                flags: 4,
                ..NativeCbCacheEntry::EMPTY
            };
            return None;
        }
        if self.jit.get(fid).is_none() {
            let proto: *const crate::bytecode::FuncProto = self.func(fid as usize);
            // SAFETY: program functions are immutable during execution; the raw
            // ptr dodges the self.jit (&mut) vs self.program (&) borrow conflict.
            let proto_ref = unsafe { &*proto };
            // ── globals that are NOT slot bindings ── the Tier A/C gate, the region
            // gate and the leaf-inline planner all refuse a body whose direct global
            // access would not observe what the interpreter observes. This path — a
            // Tier A compile of an array-builtin CALLBACK — had no such check at all,
            // so a callback reading an own-property-backed global got the
            // UNINITIALIZED sentinel handed back as `undefined`.
            //
            // It must come BEFORE `jit.compile`: compile blacklists on failure and
            // FN_DEAD is sticky, so declining afterwards would kill the callback for
            // the life of the process instead of for this call.
            if matches!(proto_ref.code.first(), Some(Instr::LazyCompile)) {
                return None;
            }
            let metered = self.jit.metered();
            let mut slow_global_loads = rustc_hash::FxHashSet::default();
            for ins in &proto_ref.code {
                match *ins {
                    Instr::LoadGlobal { idx, .. } | Instr::LoadGlobalOrUndefined { idx, .. } => {
                        if !self.global_slot_directly_routable(idx) {
                            if metered {
                                return None;
                            }
                            slow_global_loads.insert(idx);
                        }
                    }
                    Instr::StoreGlobal { idx, .. }
                    | Instr::StoreGlobalStrict { idx, .. }
                    | Instr::StoreGlobalResolved { idx, .. } => {
                        if !self.global_store_slot_directly_routable(idx) {
                            return None;
                        }
                    }
                    _ => {}
                }
            }
            let self_val = proto_ref
                .name_global
                .and_then(|s| self.globals.get(s as usize).copied())
                .unwrap_or(Value::UNDEFINED)
                .bits();
            let mut heap_helper_addrs = self.jit_heap_helper_addrs();
            let const_strs = self.jit_build_const_strs(fid);
            heap_helper_addrs.atoms_raw = self.heap.atoms_raw;
            heap_helper_addrs.atoms_words = self.heap.atoms_words;
            let tierc_leaf_on = {
                static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
                *ON.get_or_init(|| crate::env_var_os("ZIPP_NO_TIERC_LEAF").is_none())
            };
            let mut leaf_plan = if tierc_leaf_on {
                self.build_leaf_inline_plan(fid, 0, (proto_ref.code.len() - 1) as u32)
            } else {
                rustc_hash::FxHashMap::default()
            };
            // Callback compilation happens outside a live caller bytecode
            // frame/site, so there is no receiver exemplar to bake.
            let method_plan = rustc_hash::FxHashMap::default();
            let (mut cross_plan, cross_pending, cross_baked) = self.build_cross_call_plan(fid, None, true);
            self.plan_new_lanes(fid, &mut cross_plan, &mut leaf_plan);
            self.jit.note_cross_pending(fid, &cross_pending);
            self.jit.note_cross_baked(fid, &cross_baked);
            let callable_make = self.build_callable_make_plan(fid);
            let prop_shape = self.build_prop_shape_plan(fid);
            let class_read = self.build_class_read_plan(fid);
            if !self.jit.metered() {
                let direct_on = crate::codegen::direct_ic_miss_enabled();
                for (ip, instr) in proto_ref.code.iter().enumerate() {
                    let is_set = match instr {
                        Instr::GetProp { .. } => false,
                        Instr::SetProp { .. } => true,
                        _ => continue,
                    };
                    if direct_on && self.ic_site_is_rotating_own(fid, ip, is_set) {
                        self.jit.preseed_direct_miss_site(fid, ip as u32);
                    } else if self.ic_site_has_accessor(fid, ip) {
                        self.jit.preseed_acc_site(fid, ip as u32);
                    }
                }
            }
            let prop_key_ids = self.build_prop_key_ids(fid);
            let enum_switches = self.build_enum_switch_plan(fid);
            self.jit.compile(
                fid,
                proto_ref,
                jit_self_call_at as *const () as usize,
                self_val,
                jit_globals_base as *const () as usize,
                heap_helper_addrs,
                &const_strs,
                &leaf_plan,
                &method_plan,
                &cross_plan,
                &[],
                &rustc_hash::FxHashMap::default(),
                &callable_make,
                &prop_shape,
                &class_read,
                &crate::codegen::OptCallees::default(),
                &prop_key_ids,
                &enum_switches,
                &slow_global_loads,
                self.global_route_epoch,
            );
        }
        let jit_epoch = self.jit.state_epoch();
        // The direct global routes the body was compiled with, as
        // `try_run_jit` rechecks them at an entry (and each call below
        // rechecks them again: the callback can change them itself).
        if self.global_route_epoch != 0 && !self.jit_globals_still_routable(fid) {
            self.native_cb_cache[cache_slot] = NativeCbCacheEntry {
                fid,
                jit_epoch,
                global_epoch: self.global_route_epoch,
                flags: 4,
                ..NativeCbCacheEntry::EMPTY
            };
            return None;
        }
        let cross = self.jit.cross_entry(fid);
        let has_cross = cross.is_some();
        let (main_entry, main_sb, main_int) = match self.jit.get(fid) {
            Some(b) => (b.entry() as usize, b.self_binding(), b.int_tier()),
            None => (0, None, false),
        };
        let (int_entry, int_sb) = match self.jit.int_body(fid) {
            Some(b) => (b.entry() as usize, b.self_binding()),
            None => (0, None),
        };
        let uninit_mask = match cross {
            Some((_, mask, _, _)) if !main_int => mask,
            _ => u64::MAX,
        };
        let retired = self.jit.retired_count();
        let (callee_regs, param_count) = {
            let proto = self.func(fid as usize);
            ((proto.reg_count as usize).max(1), proto.param_count as usize)
        };
        self.native_cb_cache[cache_slot] = NativeCbCacheEntry {
            fid,
            jit_epoch,
            global_epoch: self.global_route_epoch,
            entry: main_entry,
            int_entry,
            callee_regs: callee_regs as u16,
            param_count: param_count as u16,
            uninit_mask,
            self_binding: main_sb,
            int_self_binding: int_sb,
            retired,
            flags: u8::from(is_strict) | (u8::from(has_cross) << 1),
        };
        // A capturing closure needs a Tier-C cross entry (int-tier does not
        // load upvalues). Checked after compilation so hot capturing HOF
        // callbacks compile on first use instead of staying interpreted.
        if capturing && !has_cross {
            return None;
        }
        let upvals_raw = if capturing {
            self.heap.upvals_mirror_of(cb.heap_index())
        } else {
            upvals_raw
        };
        // With no frame under it, the int-tier body (see above).
        let (entry_addr, self_binding, mask) = if frameless {
            if int_entry == 0 {
                return None;
            }
            (int_entry, int_sb, u64::MAX)
        } else {
            if main_entry == 0 {
                return None;
            }
            (main_entry, main_sb, uninit_mask)
        };
        Some(NativeCb {
            entry: entry_addr as *const u8,
            callee_regs,
            param_count,
            this,
            fid,
            uninit_mask: mask,
            self_binding,
            upvals_raw,
            retired,
        })
    }

    #[cfg(not(all(feature = "jit", target_arch = "x86_64")))]
    pub(crate) fn native_cb_entry(&mut self, _cb: Value, _this_arg: Value) -> Option<NativeCb> {
        None
    }

    /// Invoke a compiled callback natively over the reused window at `win`
    /// (`regs[win..win+callee_regs]`), writing the `this` `native_cb_entry`
    /// bound + the first `param_count` args. On a native deopt (bail), re-runs
    /// the element through the interpreter `call_value` — which nests its frame
    /// ABOVE this window (base = `regs.len()`) and pops back, leaving the window
    /// intact for the next element. This is the fast path that skips the
    /// per-element frame push + `run_loop` re-entry + callee re-resolution that
    /// `call_value` incurs.
    ///
    /// Each call is a fresh activation, and the window is not: what a frame
    /// push and `try_run_jit` establish per call is established here per
    /// call.
    ///
    /// * The registers the body can read before it writes them hold
    ///   `undefined`. The window used to keep the previous element's values,
    ///   so `function (x) { var t; if (x > 2) t = 1; return t; }` mapped over
    ///   `[5, 1]` returned `[1, 1]`.
    /// * The body's direct global routes are still valid, and the entry is
    ///   still the body's (with its self-binding intact). A callback compiled
    ///   while `g` was a plain global kept reading the slot after
    ///   `delete globalThis.g`, and returned NaN where it must throw.
    /// * A capturing closure's cells are installed as its activation, as the
    ///   native cross-call installs them.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn invoke_cb_windowed(
        &mut self,
        native: &NativeCb,
        win: usize,
        cb: Value,
        args: &[Value],
    ) -> Result<Value, Thrown> {
        // `this_val` is the bound `this` from `native_cb_entry` (an arrow's
        // lexical one included); `call_value` below rebinds it identically.
        let NativeCb {
            entry,
            callee_regs,
            param_count,
            this: this_val,
            fid,
            ..
        } = *native;
        if self.global_route_epoch != 0 && !self.jit_globals_still_routable(fid) {
            return self.call_value(cb, this_val, args);
        }
        // Still compiled (an evicted body is parked, never freed, so `entry`
        // stays callable; a function sent back to the interpreter must not be
        // entered again), by the body `entry` belongs to (one compiled again
        // after an eviction is another body: the parked one's deopts would
        // count against it), under the binding its self-calls assumed.
        let live = self.jit.fn_state(fid) == crate::codegen::FN_COMPILED
            && (self.jit.retired_count() == native.retired || self.jit.entry_is_current(fid, entry))
            && native.self_binding.is_none_or(|(slot, expect)| {
                self.globals.get(slot as usize).map(|v| v.bits()) == Some(expect)
            });
        if !live {
            return self.call_value(cb, this_val, args);
        }
        let n = args.len().min(param_count);
        if native.uninit_mask != u64::MAX {
            let mut m = native.uninit_mask;
            while m != 0 {
                let r = m.trailing_zeros() as usize;
                if r < callee_regs {
                    self.regs[win + r] = Value::UNDEFINED;
                }
                m &= m - 1;
            }
            for r in (1 + n)..(1 + param_count).min(callee_regs) {
                self.regs[win + r] = Value::UNDEFINED;
            }
        } else {
            for r in (1 + n)..callee_regs {
                self.regs[win + r] = Value::UNDEFINED;
            }
        }
        self.regs[win] = this_val; // reg 0 = this
        for i in 0..n {
            self.regs[win + 1 + i] = args[i];
        }
        // A capturing closure's cells (see `NativeCb::upvals_raw`).
        let upvals_raw = native.upvals_raw;
        let regs_ptr = unsafe { self.regs.as_mut_ptr().add(win) } as *mut u64;
        let vm_ptr = self as *mut Vm as *mut core::ffi::c_void;
        // SAFETY: `entry` is a valid compiled win64 fn (regs, bail_out, vm)->bits
        // (from JitFn::entry); the window has callee_regs ≥ param_count+1 valid
        // slots; `vm_ptr` is valid for the call. A self-recursive callee routes
        // through `jit_self_call` which is capacity-pinned (no regs realloc).
        let f: extern "win64" fn(*mut u64, *mut u32, *mut core::ffi::c_void) -> u64 =
            unsafe { core::mem::transmute(entry) };
        // The native entry is a Rust-stack re-entry exactly like a nested
        // `run_loop`, but it never passes through one: a callback that recurses
        // through the same array builtin (`function f(){ [1].forEach(f) }`, a
        // `visit` over a cyclic graph) nested native frames with no budget until
        // the OS stack overflowed and the process aborted. Charge it against the
        // same re-entry cap so it throws the interpreter's catchable RangeError.
        if self.reentry_exhausted() {
            return Err(Thrown::from_static(
                "RangeError: Maximum call stack size exceeded",
            ));
        }
        // A full activation stack declines before native entry.
        let activation = if upvals_raw != 0 {
            match self.enter_tierc_activation(cb.heap_index(), cb.heap_index(), true, upvals_raw) {
                Some(token) => Some(token),
                None => return self.call_value(cb, this_val, args),
            }
        } else {
            None
        };
        let mut bail: u32 = crate::codegen::NO_BAIL;
        self.run_loop_depth += 1;
        let route = self.route_run_begin();
        let bits = f(regs_ptr, &mut bail as *mut u32, vm_ptr);
        self.route_run_end(route);
        self.run_loop_depth -= 1;
        if let Some(token) = activation {
            self.leave_tierc_activation(token);
        }
        if bail == crate::codegen::NO_BAIL {
            return Ok(Value::from_bits(bits));
        }
        // A deopt that left `pending_throw` set means a native self-recursive
        // callee already THREW (e.g. a recursive callback hit the RangeError
        // frame cap) — UNWIND with that exception. Re-running via call_value
        // would execute the callback a second time and propagate a stale thrown
        // value. Mirrors the try_run_jit ip==0 bail handling.
        if self.pending_throw.is_some() {
            return Err(Thrown(String::new()));
        }
        // A bail at the entry (ip 0) ran nothing: calling the callback on the
        // interpreter is exact. A bail PAST the entry means native code has
        // already executed `[0, bail)` — its side effects included (a global
        // `calls++`, a queued host call) — so the callback must RESUME at
        // `bail` over the same window, exactly as an ordinary frame resumes
        // after `try_run_jit`, never run again from the top. It used to re-run
        // here, and `[1].forEach(v => { calls++; assert._isSameValue(this, u); }, u)`
        // counted two calls (test262 arrow-function/cannot-override-this-with-thisArg).
        if bail == 0 {
            return self.call_value(cb, this_val, args);
        }
        let closure = match self.heap.get(cb.heap_index()) {
            HeapObj::Closure { .. } => cb.heap_index(),
            _ => crate::vm::NO_CLOSURE,
        };
        self.resume_frame_window(cb, fid, closure, win, bail as usize)
    }

    /// The callback handle and reused window for a builtin about to call
    /// `cb` once per element: `native_cb_entry`'s handle with its window made
    /// at the top of the register file, or `None` (and no window) when `cb`
    /// goes through `call_value` or the window would not fit. The caller runs
    /// each element with [`Vm::run_cb_elem`] and truncates the register file
    /// back to the returned length on every exit.
    pub(crate) fn open_cb_window(&mut self, cb: Value, this_arg: Value) -> (Option<NativeCb>, usize) {
        let win = self.regs.len();
        let native = self.native_cb_entry(cb, this_arg).filter(|native| {
            let top = win + native.callee_regs;
            !self.regs_would_overflow(top) && {
                self.regs.resize(top, Value::UNDEFINED);
                true
            }
        });
        (native, win)
    }

    /// One per-element callback invocation: native fast path when `native` is
    /// set (with the `this` it bound), else the interpreter `call_value` with
    /// the raw `this_val`.
    #[inline]
    pub(crate) fn run_cb_elem(
        &mut self,
        native: &Option<NativeCb>,
        win: usize,
        cb: Value,
        args: &[Value],
        this_val: Value,
    ) -> Result<Value, Thrown> {
        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
        if let Some(native) = native {
            let result = self.invoke_cb_windowed(native, win, cb, args);
            // A resumed interpreter frame releases the window when it pops;
            // the next element expects it to be there again.
            if self.regs.len() < win + native.callee_regs {
                self.regs.resize(win + native.callee_regs, Value::UNDEFINED);
            }
            return result;
        }
        let _ = (native, win);
        // In a VM that runs only the interpreter, a leaf callback runs
        // without a frame (`Vm::leaf_callback`).
        #[cfg(not(any(feature = "meter-only", feature = "safe-sandbox")))]
        if let Some(v) = self.leaf_callback(cb, this_val, args) {
            return Ok(v);
        }
        self.call_value(cb, this_val, args)
    }
}

/// `Number.prototype.toExponential` formatting → JS form "d.ddde±X" (the exponent
/// always carries a sign; `digits` None = minimal mantissa). Spec (21.1.3.2)
/// differs from Rust's `format!("{:.e}")` in two ways: the sign is "" whenever
/// x = 0 (so -0 has no '-'), and the mantissa rounds half-AWAY-from-zero (Rust
/// uses banker's round-half-to-even).
fn fmt_exponential(n: f64, digits: Option<usize>) -> String {
    if n.is_nan() {
        return "NaN".to_string();
    }
    if n.is_infinite() {
        return if n < 0.0 { "-Infinity" } else { "Infinity" }.to_string();
    }
    // x = 0 (incl. -0): no sign.
    if n == 0.0 {
        return match digits {
            Some(d) if d > 0 => format!("0.{}e+0", "0".repeat(d)),
            _ => "0e+0".to_string(),
        };
    }
    let sign = if n < 0.0 { "-" } else { "" };
    let a = n.abs();
    let (mant, exp) = match digits {
        // Minimal mantissa: the shortest round-trip digits, an exact tie
        // settled to the even candidate as in Number::toString.
        None => {
            let (digits, exp) = crate::vm::helpers_num2::shortest_digits(a);
            let mant = if digits.len() > 1 {
                format!("{}.{}", &digits[..1], &digits[1..])
            } else {
                digits.to_string()
            };
            (mant, exp)
        }
        Some(d) => {
            let (digits, exp) = crate::vm::helpers_num2::sig_digits_half_up(a, d + 1);
            let mant = if d > 0 {
                format!("{}.{}", &digits[..1], &digits[1..])
            } else {
                digits
            };
            (mant, exp)
        }
    };
    format!(
        "{sign}{mant}e{}{}",
        if exp >= 0 { "+" } else { "-" },
        exp.abs()
    )
}

/// `Number.prototype.toPrecision` formatting (significant digits → fixed or
/// exponential depending on the magnitude).
fn fmt_precision(n: f64, p: usize) -> String {
    if n.is_nan() {
        return "NaN".to_string();
    }
    if n.is_infinite() {
        return if n < 0.0 { "-Infinity" } else { "Infinity" }.to_string();
    }
    if n == 0.0 {
        return if p == 1 {
            "0".to_string()
        } else {
            format!("0.{}", "0".repeat(p - 1))
        };
    }
    let neg = n < 0.0;
    let a = n.abs();
    // Round to p significant figures ONCE, then lay the same digits out in
    // either form. Step 10 picks the LARGER candidate on an exact tie
    // ((2.5).toPrecision(1) is "3", (1.25).toPrecision(2) is "1.3"); Rust's
    // `{:.*e}` / `{:.N}` formatters round such ties to even, so both forms
    // come from `sig_digits_half_up` (which also carries 9.99 → 1.00e+1).
    // Reparsing the digits into an f64 and reformatting would re-introduce a
    // rounding error (1.2345e27 → 1.2344999…e27).
    let (digits, exp) = crate::vm::helpers_num2::sig_digits_half_up(a, p);
    let body = if exp < -6 || exp >= p as i32 {
        let sign = if exp < 0 { "-" } else { "+" };
        if p > 1 {
            format!("{}.{}e{sign}{}", &digits[..1], &digits[1..], exp.abs())
        } else {
            format!("{digits}e{sign}{}", exp.abs())
        }
    } else if exp < 0 {
        format!("0.{}{digits}", "0".repeat((-exp - 1) as usize))
    } else if exp as usize + 1 == p {
        digits
    } else {
        let int_len = exp as usize + 1;
        format!("{}.{}", &digits[..int_len], &digits[int_len..])
    };
    if neg {
        format!("-{body}")
    } else {
        body
    }
}
