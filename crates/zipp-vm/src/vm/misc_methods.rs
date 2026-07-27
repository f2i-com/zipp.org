#![allow(unused_imports)]
use super::*;
use crate::bytecode::{InstanceCtor, Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PropAttr, PromiseState, Reaction,
};
use crate::value::Value;

impl<'p> Vm<'p> {
    /// If `idx` is an Error-like object — an object whose `name` is one of the
    /// engine's error kinds — return that name, else `None`.
    pub(crate) fn error_name(&self, idx: u32) -> Option<String> {
        let map = match self.heap.get(idx) {
            HeapObj::Object(m) => m,
            _ => return None,
        };
        let nv = map.get("name")?;
        let name = self.display(nv);
        native::ERROR_NAMES.contains(&name.as_str()).then_some(name)
    }

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
            match self.proto_of.get(&cur) {
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
            match self.proto_of.get(&cur) {
                Some(p) if p.is_heap() => cur = p.heap_index(),
                _ => return None,
            }
        }
        None
    }

    /// `Error.prototype.toString` semantics for the read-only `display` path:
    /// "name: message", dropping the separator when either part is empty.
    pub(crate) fn error_display_string(&self, idx: u32) -> String {
        let name =
            self.read_data_prop(idx, "name").map(|v| self.display(v)).unwrap_or_else(|| "Error".into());
        let msg = self.read_data_prop(idx, "message").map(|v| self.display(v)).unwrap_or_default();
        if name.is_empty() {
            msg
        } else if msg.is_empty() {
            name
        } else {
            format!("{name}: {msg}")
        }
    }

    /// Methods on a number receiver: `toFixed`, `toString`. Returns `Ok(None)`
    /// for an unrecognised name (the caller then treats it as a missing property
    /// → TypeError, matching JS).
    pub(crate) fn number_method(&mut self, recv: Value, name: &str, args: &[Value]) -> Result<Option<Value>, Thrown> {
        // thisNumberValue brand check: a Number primitive uses its value; the
        // Number.prototype object itself has [[NumberData]] = +0; anything else
        // (a String/object via `Number.prototype.toString.call(x)`) is a TypeError.
        let n = if recv.is_number() {
            recv.as_f64()
        } else if recv.is_heap() && recv.heap_index() == self.num_proto {
            0.0
        } else {
            return Err(Thrown(
                "TypeError: Number.prototype method called on a non-Number".into(),
            ));
        };
        let nv = if recv.is_number() { recv } else { Value::num(n) };
        // ToIntegerOrInfinity(ToNumber(arg)): coerce (valueOf/string), truncate;
        // NaN -> 0. Absent arg behaves as `undefined` -> NaN -> 0.
        let mut int_arg = |vm: &mut Self, i: usize| -> Result<f64, Thrown> {
            // ToIntegerOrInfinity goes through ToNumber, so a BigInt/Symbol digits
            // argument is a TypeError (not silently coerced).
            let raw = vm.to_number_strict(args.get(i).copied().unwrap_or(Value::UNDEFINED))?;
            Ok(if raw.is_nan() { 0.0 } else { raw.trunc() })
        };
        match name {
            "toFixed" => {
                let d = int_arg(self, 0)?;
                if d < 0.0 || d > 100.0 {
                    return Err(Thrown(
                        "RangeError: toFixed() digits argument must be between 0 and 100".into(),
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
                    return Err(Thrown(
                        "RangeError: toString() radix must be between 2 and 36".into(),
                    ));
                }
                if r == 10 {
                    Ok(Some(self.alloc_str(self.display(nv))))
                } else {
                    Ok(Some(self.alloc_str(num_to_radix(n, r as u32))))
                }
            }
            "valueOf" => Ok(Some(nv)),
            // No Intl: toLocaleString() behaves like the default base-10 toString().
            "toLocaleString" => Ok(Some(self.alloc_str(self.display(nv)))),
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
                        return Err(Thrown(
                            "RangeError: toExponential() argument must be between 0 and 100".into(),
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
                    return Err(Thrown(
                        "RangeError: toPrecision() argument must be between 1 and 100".into(),
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
            "toString" => {
                self.alloc_str(if b == Value::bool(true) { "true" } else { "false" }.to_string())
            }
            "valueOf" => b,
            _ => Value::UNDEFINED,
        })
    }

    /// Resolve `cb` to the native entry of a COMPILED, non-capturing JIT function
    /// for the array-builtin fast path (`map`/`filter`/`forEach`/`reduce`).
    /// Returns `(entry, callee_reg_count, param_count)` or `None` if `cb` must go
    /// through the interpreter `call_value` (not a plain function, a capturing
    /// closure, JIT disabled, inside a deopted self-call continuation, or not
    /// JIT-compilable). Compiles `cb` on first use if eligible — array builtins
    /// call the same callback many times, so we don't wait for the call-count
    /// threshold; an ineligible proto is blacklisted by `compile` and returns
    /// `None` cheaply thereafter.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn native_cb_entry(&mut self, cb: Value) -> Option<(*const u8, usize, usize)> {
        // Mirror the interpreter's JIT-entry guard: respect ZIPP_NOJIT and never
        // enter native code from a deopted self-call continuation (livelock).
        if !self.jit_fused_ok() || self.jit_recurse_depth != 0 || !cb.is_heap() {
            return None;
        }
        let (fid, ups) = self.heap.as_callable(cb.heap_index())?;
        // A capturing closure reads upvalue cells (heap) — outside the leaf-int JIT.
        if !ups.is_empty() {
            return None;
        }
        // A callback that materialises `arguments` needs the interpreter's call
        // setup (the JIT window never builds the arguments object) — same guard
        // as the fused-kernel paths.
        if self.func(fid as usize).arguments_reg.is_some() {
            return None;
        }
        if self.jit.get(fid).is_none() {
            let proto: *const crate::bytecode::FuncProto =
                self.func(fid as usize);
            // SAFETY: program functions are immutable during execution; the raw
            // ptr dodges the self.jit (&mut) vs self.program (&) borrow conflict.
            let proto_ref = unsafe { &*proto };
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
            self.jit.compile(
                fid,
                proto_ref,
                jit_self_call_at as usize,
                self_val,
                jit_globals_base as usize,
                heap_helper_addrs,
                &const_strs,
                &leaf_plan,
            );
        }
        let entry = self.jit.get(fid)?.entry();
        let proto = self.func(fid as usize);
        Some((entry, (proto.reg_count as usize).max(1), proto.param_count as usize))
    }

    #[cfg(not(all(feature = "jit", target_arch = "x86_64")))]
    pub(crate) fn native_cb_entry(&mut self, _cb: Value) -> Option<(*const u8, usize, usize)> {
        None
    }

    /// Invoke a compiled callback natively over the reused window at `win`
    /// (`regs[win..win+callee_regs]`), writing `this`=undefined + the first
    /// `param_count` args. On a native deopt (bail), re-runs the element through
    /// the interpreter `call_value` — which nests its frame ABOVE this window
    /// (base = `regs.len()`) and pops back, leaving the window intact for the
    /// next element. This is the fast path that skips the per-element frame push
    /// + `run_loop` re-entry + callee re-resolution that `call_value` incurs.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn invoke_cb_windowed(
        &mut self,
        entry: *const u8,
        win: usize,
        param_count: usize,
        cb: Value,
        args: &[Value],
        this_val: Value,
    ) -> Result<Value, Thrown> {
        // An arrow callback ignores the supplied thisArg and uses its lexical `this`.
        let this_val = self.arrow_captured_this(cb).unwrap_or(this_val);
        self.regs[win] = this_val; // reg 0 = this (thisArg, or arrow's lexical this)
        let n = args.len().min(param_count);
        for i in 0..n {
            self.regs[win + 1 + i] = args[i];
        }
        let regs_ptr = unsafe { self.regs.as_mut_ptr().add(win) } as *mut u64;
        let vm_ptr = self as *mut Vm as *mut core::ffi::c_void;
        // SAFETY: `entry` is a valid compiled win64 fn (regs, bail_out, vm)->bits
        // (from JitFn::entry); the window has callee_regs ≥ param_count+1 valid
        // slots; `vm_ptr` is valid for the call. A self-recursive callee routes
        // through `jit_self_call` which is capacity-pinned (no regs realloc).
        let f: extern "win64" fn(*mut u64, *mut u32, *mut core::ffi::c_void) -> u64 =
            unsafe { core::mem::transmute(entry) };
        let mut bail: u32 = crate::codegen::NO_BAIL;
        let bits = f(regs_ptr, &mut bail as *mut u32, vm_ptr);
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
        // Plain deopt (non-int operand / overflow): re-run this element on the
        // interpreter, which nests its frame above the reused window.
        self.call_value(cb, this_val, args)
    }

    /// One per-element callback invocation: native fast path when `native` is
    /// set, else the interpreter `call_value`.
    #[inline]
    pub(crate) fn run_cb_elem(
        &mut self,
        native: Option<(*const u8, usize, usize)>,
        win: usize,
        cb: Value,
        args: &[Value],
        this_val: Value,
    ) -> Result<Value, Thrown> {
        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
        if let Some((entry, _callee_regs, param_count)) = native {
            return self.invoke_cb_windowed(entry, win, param_count, cb, args, this_val);
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
        // Minimal mantissa: the shortest round-trip (Rust's default) is correct.
        None => {
            let raw = format!("{a:e}");
            let epos = raw.find('e').unwrap();
            (raw[..epos].to_string(), raw[epos + 1..].parse::<i32>().unwrap_or(0))
        }
        Some(d) => round_exp_half_away(a, d),
    };
    format!("{sign}{mant}e{}{}", if exp >= 0 { "+" } else { "-" }, exp.abs())
}

/// Round a POSITIVE finite `a` to `d` fractional mantissa digits in exponential
/// form, rounding half-AWAY-from-zero, returning ("D.FFFF", exponent). Works on
/// the exact decimal digits (formatted at high precision, so the (d+1)th digit is
/// exact) to avoid the f64 round-trip drift a `*10^d` reconstruction would cause.
fn round_exp_half_away(a: f64, d: usize) -> (String, i32) {
    // High-precision digits — exact for the first d+1 fractional places.
    let guard = d + 25;
    let raw = format!("{a:.guard$e}");
    let epos = raw.find('e').unwrap();
    let mantissa = &raw[..epos]; // "D.FFFF…"
    let mut exp: i32 = raw[epos + 1..].parse().unwrap_or(0);
    // Digit string without the '.': 1 leading (integer) digit + `guard` fractional.
    let mut ds: Vec<u8> = mantissa.bytes().filter(|&b| b != b'.').map(|b| b - b'0').collect();
    // For a positive value, nearest-rounding (half-away) rounds the kept prefix up
    // iff the first dropped digit is >= 5 (the >= folds the exact-tie case to up).
    let keep = 1 + d;
    let round_up = ds.get(keep).is_some_and(|&dig| dig >= 5);
    ds.truncate(keep);
    if round_up {
        let mut i = ds.len();
        loop {
            if i == 0 {
                ds.insert(0, 1); // carry past the leading digit: 9.. -> 10..
                break;
            }
            i -= 1;
            if ds[i] == 9 {
                ds[i] = 0;
            } else {
                ds[i] += 1;
                break;
            }
        }
    }
    // A carry that grew the integer part to two digits (e.g. 9.99 -> 10.0)
    // renormalises: drop the extra trailing digit and bump the exponent.
    if ds.len() > 1 + d {
        ds.truncate(1 + d);
        exp += 1;
    }
    let int_digit = (b'0' + ds[0]) as char;
    let mant = if d == 0 {
        int_digit.to_string()
    } else {
        let frac: String = ds[1..].iter().map(|&x| (b'0' + x) as char).collect();
        format!("{int_digit}.{frac}")
    };
    (mant, exp)
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
        return if p == 1 { "0".to_string() } else { format!("0.{}", "0".repeat(p - 1)) };
    }
    let neg = n < 0.0;
    let a = n.abs();
    // Round to p significant figures via exponential form, then read the exponent.
    let exp_str = format!("{a:.*e}", p - 1);
    let epos = exp_str.find('e').unwrap();
    let exp: i32 = exp_str[epos + 1..].parse().unwrap_or(0);
    let body = if exp < -6 || exp >= p as i32 {
        // The mantissa substring is ALREADY rounded to p significant figures by
        // the `{:.*e}` formatter — use it directly. Reparsing it into an f64 and
        // reformatting re-introduces a rounding error (1.2345e27 → 1.2344999…e27).
        let m = &exp_str[..epos];
        let sign = if exp < 0 { "-" } else { "+" };
        format!("{m}e{sign}{}", exp.abs())
    } else {
        let frac = (p as i32 - 1 - exp).max(0) as usize;
        format!("{a:.frac$}")
    };
    if neg {
        format!("-{body}")
    } else {
        body
    }
}
