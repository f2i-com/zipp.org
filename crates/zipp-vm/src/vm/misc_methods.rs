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
    /// A closure with captures: its cells are installed around each call.
    capturing: bool,
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
            return Err(Thrown(format!(
                "TypeError: Boolean.prototype.{name} requires that 'this' be a Boolean"
            )));
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
        if frameless && !(ups.is_empty() && self.jit.int_body(fid).is_some()) {
            return None;
        }
        // A capturing closure reads upvalue cells (heap) — outside the
        // leaf-int JIT this function compiles with. It needs the body its own
        // calls compiled, and the conditions of a frame-free native call (the
        // ones an emitted cross-call site tests before it enters one).
        let capturing = !ups.is_empty();
        if capturing
            && (self.jit.cross_entry(fid).is_none()
                || !self.closure_eval_scope.is_empty()
                || !self.obj_realm.is_empty()
                || !self.realm_global_objs.is_empty())
        {
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
            if !proto_ref.code.iter().all(|ins| match *ins {
                Instr::LoadGlobal { idx, .. } | Instr::LoadGlobalOrUndefined { idx, .. } => {
                    self.global_slot_directly_routable(idx)
                }
                Instr::StoreGlobal { idx, .. }
                | Instr::StoreGlobalStrict { idx, .. }
                | Instr::StoreGlobalResolved { idx, .. } => {
                    self.global_store_slot_directly_routable(idx)
                }
                _ => true,
            }) {
                return None;
            }
            let self_val = proto_ref
                .name_global
                .and_then(|s| self.globals.get(s as usize).copied())
                .unwrap_or(Value::UNDEFINED)
                .bits();
            let heap_helper_addrs = self.jit_heap_helper_addrs();
            let const_strs = self.jit_build_const_strs(fid);
            let leaf_plan = if std::env::var_os("ZIPP_NO_TIERC_LEAF").is_none() {
                self.build_leaf_inline_plan(fid, 0, (proto_ref.code.len() - 1) as u32)
            } else {
                rustc_hash::FxHashMap::default()
            };
            // Callback compilation happens outside a live caller bytecode
            // frame/site, so there is no receiver exemplar to bake.
            let method_plan = rustc_hash::FxHashMap::default();
            let (cross_plan, cross_pending, _cross_baked) = self.build_cross_call_plan(fid, None, true);
            self.jit.note_cross_pending(fid, &cross_pending);
            let plain_makefunc = self.build_plain_makefunc_plan(fid);
            let prop_shape = self.build_prop_shape_plan(fid);
            let class_read = self.build_class_read_plan(fid);
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
                &plain_makefunc,
                &prop_shape,
                &class_read,
            );
        }
        // The direct global routes the body was compiled with, as
        // `try_run_jit` rechecks them at an entry (and each call below
        // rechecks them again: the callback can change them itself).
        if self.global_route_epoch != 0 && !self.jit_globals_still_routable(fid) {
            return None;
        }
        // With no frame under it, the int-tier body (see above).
        let body = if frameless {
            self.jit.int_body(fid)?
        } else {
            self.jit.get(fid)?
        };
        let int_body = body.int_tier();
        let (entry, self_binding) = (body.entry(), body.self_binding());
        let uninit_mask = match self.jit.cross_entry(fid) {
            Some((_, mask, _, _)) if !int_body => mask,
            _ => u64::MAX,
        };
        let proto = self.func(fid as usize);
        Some(NativeCb {
            entry,
            callee_regs: (proto.reg_count as usize).max(1),
            param_count: proto.param_count as usize,
            this,
            fid,
            uninit_mask,
            self_binding,
            capturing,
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
        // entered again), under the binding its self-calls assumed.
        let live = self.jit.fn_state(fid) == crate::codegen::FN_COMPILED
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
        // A capturing closure's cells, read from the closure each call (its
        // list never changes, but this is the pointer native code follows).
        let upvals_raw = if native.capturing {
            match self.heap.get(cb.heap_index()) {
                HeapObj::Closure { upvalues, .. } if !upvalues.is_empty() => {
                    upvalues.as_ptr() as u64
                }
                _ => return self.call_value(cb, this_val, args),
            }
        } else {
            0u64
        };
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
        if self.run_loop_depth >= MAX_RUN_LOOP_DEPTH {
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
        let bits = f(regs_ptr, &mut bail as *mut u32, vm_ptr);
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
                digits
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
