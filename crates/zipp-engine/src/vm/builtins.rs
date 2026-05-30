//! The big built-in-function dispatcher — `Math.*`, `JSON.*`, `Array.*`,
//! `String.*`, `Number.*`, `Object.*`, `Map.*`, `Set.*`, `Promise.*`, …
//!
//! The VM resolves calls like `Math.floor(1.5)` into a
//! [`BuiltinFunction`] variant at compile time (see
//! [`crate::runtime::globals::builtin_global_object`]), then hands the
//! variant plus the argument slice to this file for execution. Every
//! built-in implementation lives in the single giant match in
//! [`VM::execute_builtin_function_slice`].
//!
//! Nothing here is "new" — the function used to sit inside `vm/mod.rs`;
//! it's extracted into a sibling submodule purely to keep `mod.rs` under
//! a more manageable line count. `impl VM` in two files is supported
//! because `vm::builtins` is a child of `vm`, so it can see the private
//! helper methods on `VM` that the dispatcher leans on.

use std::rc::Rc;

use crate::object::{
    make_array, make_hash, undefined_object, BuiltinFunction, BuiltinFunctionObject, HashKey,
    HashObject, Object, PromiseObject, PromiseState,
};
use crate::value::{
    json_value_to_vm_value, obj_into_val, obj_to_val, val_inspect, val_to_obj, Heap, Value,
};

use super::{
    epoch_millis_now, unwrap_array, uri_decode, uri_encode, VMError, ERR_ARRAY_SIZE,
    MAX_ARRAY_SIZE, VM,
};

impl VM {
    pub(super) fn execute_builtin_function_slice(
        &mut self,
        builtin: BuiltinFunctionObject,
        args: &[Value],
    ) -> Result<Value, VMError> {
        match builtin.function {
            BuiltinFunction::MathAbs => {
                let n = args
                    .first()
                    .map(|v| self.to_number_val(*v))
                    .transpose()?
                    .unwrap_or(0.0);
                if n.is_finite() && n.fract() == 0.0 {
                    Ok(Value::from_i64(n.abs() as i64))
                } else {
                    Ok(Value::from_f64(n.abs()))
                }
            }
            BuiltinFunction::MathFloor => {
                let n = args
                    .first()
                    .map(|v| self.to_number_val(*v))
                    .transpose()?
                    .unwrap_or(f64::NAN);
                let out = n.floor();
                if out.is_finite() && out.fract() == 0.0 {
                    Ok(Value::from_i64(out as i64))
                } else {
                    Ok(Value::from_f64(out))
                }
            }
            BuiltinFunction::MathCeil => {
                let n = args
                    .first()
                    .map(|v| self.to_number_val(*v))
                    .transpose()?
                    .unwrap_or(f64::NAN);
                let out = n.ceil();
                if out.is_finite() && out.fract() == 0.0 {
                    Ok(Value::from_i64(out as i64))
                } else {
                    Ok(Value::from_f64(out))
                }
            }
            BuiltinFunction::MathRound => {
                let n = args
                    .first()
                    .map(|v| self.to_number_val(*v))
                    .transpose()?
                    .unwrap_or(f64::NAN);
                let out = n.round();
                if out.is_finite() && out.fract() == 0.0 {
                    Ok(Value::from_i64(out as i64))
                } else {
                    Ok(Value::from_f64(out))
                }
            }
            BuiltinFunction::MathMin => {
                if args.is_empty() {
                    return Ok(Value::from_f64(f64::INFINITY));
                }
                let mut min = f64::INFINITY;
                // `Math.min(...bigArr)` with a 1 M-element array spins
                // through `to_number_val` a million times inside a
                // single builtin call — wall-time / abort-flag can't
                // fire mid-loop without a periodic check.
                for (i, arg) in args.iter().enumerate() {
                    if (i & 0xfff) == 0 {
                        self.check_builtin_callback_limits()?;
                    }
                    let n = self.to_number_val(*arg)?;
                    if n.is_nan() {
                        return Ok(Value::from_f64(f64::NAN));
                    }
                    if n < min {
                        min = n;
                    }
                }
                if min.is_finite() && min.fract() == 0.0 {
                    Ok(Value::from_i64(min as i64))
                } else {
                    Ok(Value::from_f64(min))
                }
            }
            BuiltinFunction::MathMax => {
                if args.is_empty() {
                    return Ok(Value::from_f64(f64::NEG_INFINITY));
                }
                let mut max = f64::NEG_INFINITY;
                for (i, arg) in args.iter().enumerate() {
                    if (i & 0xfff) == 0 {
                        self.check_builtin_callback_limits()?;
                    }
                    let n = self.to_number_val(*arg)?;
                    if n.is_nan() {
                        return Ok(Value::from_f64(f64::NAN));
                    }
                    if n > max {
                        max = n;
                    }
                }
                if max.is_finite() && max.fract() == 0.0 {
                    Ok(Value::from_i64(max as i64))
                } else {
                    Ok(Value::from_f64(max))
                }
            }
            BuiltinFunction::MathPow => {
                let base = args
                    .first()
                    .map(|v| self.to_number_val(*v))
                    .transpose()?
                    .unwrap_or(f64::NAN);
                let exp = args
                    .get(1)
                    .map(|v| self.to_number_val(*v))
                    .transpose()?
                    .unwrap_or(f64::NAN);
                let out = base.powf(exp);
                if out.is_finite() && out.fract() == 0.0 {
                    Ok(Value::from_i64(out as i64))
                } else {
                    Ok(Value::from_f64(out))
                }
            }
            BuiltinFunction::MathSqrt => {
                let n = args
                    .first()
                    .map(|v| self.to_number_val(*v))
                    .transpose()?
                    .unwrap_or(f64::NAN);
                let out = n.sqrt();
                if out.is_finite() && out.fract() == 0.0 {
                    Ok(Value::from_i64(out as i64))
                } else {
                    Ok(Value::from_f64(out))
                }
            }
            BuiltinFunction::MathTrunc => {
                let n = args
                    .first()
                    .map(|v| self.to_number_val(*v))
                    .transpose()?
                    .unwrap_or(f64::NAN);
                let out = n.trunc();
                if out.is_finite() && out.fract() == 0.0 {
                    Ok(Value::from_i64(out as i64))
                } else {
                    Ok(Value::from_f64(out))
                }
            }
            BuiltinFunction::MathSign => {
                let n = args
                    .first()
                    .map(|v| self.to_number_val(*v))
                    .transpose()?
                    .unwrap_or(f64::NAN);
                let out = if n.is_nan() {
                    f64::NAN
                } else if n == 0.0 {
                    // JS Math.sign(0) === 0, Math.sign(-0) === -0
                    n
                } else {
                    n.signum()
                };
                if out.is_finite() && out.fract() == 0.0 {
                    Ok(Value::from_i64(out as i64))
                } else {
                    Ok(Value::from_f64(out))
                }
            }
            BuiltinFunction::MathRandom => {
                #[cfg(target_arch = "wasm32")]
                {
                    Ok(Value::from_f64(js_sys::Math::random()))
                }
                #[cfg(not(target_arch = "wasm32"))]
                {
                    // Xorshift64 PRNG — fast, stateful, uniform distribution
                    let mut s = self.rng_state;
                    s ^= s << 13;
                    s ^= s >> 7;
                    s ^= s << 17;
                    self.rng_state = s;
                    // Convert to [0, 1) by taking upper 53 bits as f64
                    let n = (s >> 11) as f64 / (1u64 << 53) as f64;
                    Ok(Value::from_f64(n))
                }
            }
            BuiltinFunction::MathLog => {
                let n = args
                    .first()
                    .map(|v| self.to_number_val(*v))
                    .transpose()?
                    .unwrap_or(f64::NAN);
                let out = n.ln();
                if out.is_finite() && out.fract() == 0.0 {
                    Ok(Value::from_i64(out as i64))
                } else {
                    Ok(Value::from_f64(out))
                }
            }
            BuiltinFunction::MathLog2 => {
                let n = args
                    .first()
                    .map(|v| self.to_number_val(*v))
                    .transpose()?
                    .unwrap_or(f64::NAN);
                let out = n.log2();
                if out.is_finite() && out.fract() == 0.0 {
                    Ok(Value::from_i64(out as i64))
                } else {
                    Ok(Value::from_f64(out))
                }
            }
            BuiltinFunction::MathCbrt => {
                let n = args
                    .first()
                    .map(|v| self.to_number_val(*v))
                    .transpose()?
                    .unwrap_or(f64::NAN);
                let out = n.cbrt();
                if out.is_finite() && out.fract() == 0.0 {
                    Ok(Value::from_i64(out as i64))
                } else {
                    Ok(Value::from_f64(out))
                }
            }
            BuiltinFunction::MathSin => {
                let n = args
                    .first()
                    .map(|v| self.to_number_val(*v))
                    .transpose()?
                    .unwrap_or(f64::NAN);
                let out = n.sin();
                if out.is_finite() && out.fract() == 0.0 {
                    Ok(Value::from_i64(out as i64))
                } else {
                    Ok(Value::from_f64(out))
                }
            }
            BuiltinFunction::MathCos => {
                let n = args
                    .first()
                    .map(|v| self.to_number_val(*v))
                    .transpose()?
                    .unwrap_or(f64::NAN);
                let out = n.cos();
                if out.is_finite() && out.fract() == 0.0 {
                    Ok(Value::from_i64(out as i64))
                } else {
                    Ok(Value::from_f64(out))
                }
            }
            BuiltinFunction::MathTan => {
                let n = args
                    .first()
                    .map(|v| self.to_number_val(*v))
                    .transpose()?
                    .unwrap_or(f64::NAN);
                let out = n.tan();
                if out.is_finite() && out.fract() == 0.0 {
                    Ok(Value::from_i64(out as i64))
                } else {
                    Ok(Value::from_f64(out))
                }
            }
            BuiltinFunction::MathExp => {
                let n = args
                    .first()
                    .map(|v| self.to_number_val(*v))
                    .transpose()?
                    .unwrap_or(f64::NAN);
                let out = n.exp();
                if out.is_finite() && out.fract() == 0.0 {
                    Ok(Value::from_i64(out as i64))
                } else {
                    Ok(Value::from_f64(out))
                }
            }
            BuiltinFunction::MathLog10 => {
                let n = args
                    .first()
                    .map(|v| self.to_number_val(*v))
                    .transpose()?
                    .unwrap_or(f64::NAN);
                let out = n.log10();
                if out.is_finite() && out.fract() == 0.0 {
                    Ok(Value::from_i64(out as i64))
                } else {
                    Ok(Value::from_f64(out))
                }
            }
            BuiltinFunction::MathAtan2 => {
                let y = args
                    .first()
                    .map(|v| self.to_number_val(*v))
                    .transpose()?
                    .unwrap_or(f64::NAN);
                let x = args
                    .get(1)
                    .map(|v| self.to_number_val(*v))
                    .transpose()?
                    .unwrap_or(f64::NAN);
                let out = y.atan2(x);
                if out.is_finite() && out.fract() == 0.0 {
                    Ok(Value::from_i64(out as i64))
                } else {
                    Ok(Value::from_f64(out))
                }
            }
            BuiltinFunction::MathHypot => {
                if args.is_empty() {
                    return Ok(Value::from_i64(0));
                }
                let mut sum_sq = 0.0;
                for arg in args {
                    let n = self.to_number_val(*arg)?;
                    if n.is_nan() {
                        return Ok(Value::from_f64(f64::NAN));
                    }
                    if n.is_infinite() {
                        return Ok(Value::from_f64(f64::INFINITY));
                    }
                    sum_sq += n * n;
                }
                let out = sum_sq.sqrt();
                if out.is_finite() && out.fract() == 0.0 {
                    Ok(Value::from_i64(out as i64))
                } else {
                    Ok(Value::from_f64(out))
                }
            }
            BuiltinFunction::MathImul => {
                let a = args
                    .first()
                    .map(|v| self.to_i32_val(*v))
                    .transpose()?
                    .unwrap_or(0);
                let b = args
                    .get(1)
                    .map(|v| self.to_i32_val(*v))
                    .transpose()?
                    .unwrap_or(0);
                let out = a.wrapping_mul(b);
                Ok(Value::from_i64(out as i64))
            }
            BuiltinFunction::MathClz32 => {
                let n = args
                    .first()
                    .map(|v| self.to_u32_val(*v))
                    .transpose()?
                    .unwrap_or(0);
                Ok(Value::from_i64(n.leading_zeros() as i64))
            }
            BuiltinFunction::MathFround => {
                let n = args
                    .first()
                    .map(|v| self.to_number_val(*v))
                    .transpose()?
                    .unwrap_or(f64::NAN);
                let out = (n as f32) as f64;
                if out.is_finite() && out.fract() == 0.0 {
                    Ok(Value::from_i64(out as i64))
                } else {
                    Ok(Value::from_f64(out))
                }
            }
            // Trig + hyperbolic + log gaps. All take one number arg, return a
            // number; identical shape to MathSqrt above. Wrapped in a helper
            // closure to keep the unary-math arms readable.
            BuiltinFunction::MathAcos
            | BuiltinFunction::MathAsin
            | BuiltinFunction::MathAtan
            | BuiltinFunction::MathAcosh
            | BuiltinFunction::MathAsinh
            | BuiltinFunction::MathAtanh
            | BuiltinFunction::MathSinh
            | BuiltinFunction::MathCosh
            | BuiltinFunction::MathTanh
            | BuiltinFunction::MathExpm1
            | BuiltinFunction::MathLog1p => {
                let n = args
                    .first()
                    .map(|v| self.to_number_val(*v))
                    .transpose()?
                    .unwrap_or(f64::NAN);
                let out = match builtin.function {
                    BuiltinFunction::MathAcos => n.acos(),
                    BuiltinFunction::MathAsin => n.asin(),
                    BuiltinFunction::MathAtan => n.atan(),
                    BuiltinFunction::MathAcosh => n.acosh(),
                    BuiltinFunction::MathAsinh => n.asinh(),
                    BuiltinFunction::MathAtanh => n.atanh(),
                    BuiltinFunction::MathSinh => n.sinh(),
                    BuiltinFunction::MathCosh => n.cosh(),
                    BuiltinFunction::MathTanh => n.tanh(),
                    BuiltinFunction::MathExpm1 => n.exp_m1(),
                    BuiltinFunction::MathLog1p => n.ln_1p(),
                    _ => unreachable!(),
                };
                if out.is_finite() && out.fract() == 0.0 {
                    Ok(Value::from_i64(out as i64))
                } else {
                    Ok(Value::from_f64(out))
                }
            }
            BuiltinFunction::NumberCtor => {
                let out = args
                    .first()
                    .map(|v| self.to_number_val(*v))
                    .transpose()?
                    .unwrap_or(0.0);
                if out.is_finite() && out.fract() == 0.0 {
                    Ok(Value::from_i64(out as i64))
                } else {
                    Ok(Value::from_f64(out))
                }
            }
            BuiltinFunction::StringCtor => {
                let value = args
                    .first()
                    .map(|v| {
                        let obj = val_to_obj(*v, &self.heap);
                        self.to_js_string(&obj)
                    })
                    .unwrap_or_default();
                Ok(obj_into_val(Object::String(value.into()), &mut self.heap))
            }
            BuiltinFunction::StringFromCharCode => {
                let mut out = String::new();
                for arg in args {
                    let code = self.to_u32_val(*arg)?;
                    let ch = char::from_u32(code).unwrap_or('\u{FFFD}');
                    out.push(ch);
                }
                Ok(obj_into_val(Object::String(out.into()), &mut self.heap))
            }
            BuiltinFunction::StringCharAt => {
                let receiver = builtin.receiver.ok_or_else(|| {
                    VMError::TypeError("String.charAt missing receiver".to_string())
                })?;
                let text = match receiver {
                    Object::String(s) => s,
                    _ => return Ok(Value::UNDEFINED),
                };
                let idx = args
                    .first()
                    .map(|v| self.to_i32_val(*v))
                    .transpose()?
                    .unwrap_or(0);
                if idx < 0 {
                    return Ok(obj_into_val(Object::String(Rc::from("")), &mut self.heap));
                }
                let ch = Self::string_nth_char(&text, idx as usize);
                Ok(obj_into_val(
                    Object::String(ch.map(|c| c.to_string()).unwrap_or_default().into()),
                    &mut self.heap,
                ))
            }
            BuiltinFunction::StringSplit => {
                let receiver = builtin.receiver.ok_or_else(|| {
                    VMError::TypeError("String.split missing receiver".to_string())
                })?;
                let text = match receiver {
                    Object::String(s) => s,
                    _ => return Ok(Value::UNDEFINED),
                };
                let limit: Option<usize> = if args.len() >= 2 {
                    Some(self.to_i32_val(args[1])?.max(0) as usize)
                } else {
                    None
                };

                // Check if separator is a RegExp
                let sep_val = args.first().copied().unwrap_or(Value::UNDEFINED);
                let sep_obj = val_to_obj(sep_val, &self.heap);
                if let Object::RegExp(re) = &sep_obj {
                    let regex = self.build_regex(&re.pattern, &re.flags)?;
                    let mut items: Vec<Value> = Vec::new();
                    let mut last = 0;
                    let max = limit.unwrap_or(usize::MAX);
                    for m in regex.find_iter(&text) {
                        if items.len() >= max { break; }
                        items.push(obj_into_val(
                            Object::String(text[last..m.start()].to_string().into()),
                            &mut self.heap,
                        ));
                        last = m.end();
                    }
                    if items.len() < max {
                        items.push(obj_into_val(
                            Object::String(text[last..].to_string().into()),
                            &mut self.heap,
                        ));
                    }
                    return Ok(obj_into_val(make_array(items), &mut self.heap));
                }

                let sep = match sep_obj {
                    Object::String(s) => s.to_string(),
                    _ => sep_obj.inspect(),
                };
                // User-supplied `limit` silently truncates; MAX_ARRAY_SIZE
                // is a hard cap that errors when there's no user limit.
                // A 10 M-char string `.split("")` without a limit would
                // otherwise build a 10 M-entry `Vec` of single-char heap
                // strings — `MAX_STRING_LENGTH` permits the input and
                // only `max_heap_bytes` eventually catches the output.
                let user_limited = limit.is_some();
                let effective = limit.unwrap_or(MAX_ARRAY_SIZE).min(MAX_ARRAY_SIZE);
                if sep.is_empty() {
                    let mut items: Vec<Value> = Vec::with_capacity(effective.min(16));
                    for c in text.chars() {
                        if items.len() >= effective {
                            if user_limited {
                                break;
                            }
                            return Err(VMError::TypeError(
                                crate::vm::ERR_ARRAY_SIZE.to_string(),
                            ));
                        }
                        items.push(obj_into_val(
                            Object::String(c.to_string().into()),
                            &mut self.heap,
                        ));
                    }
                    return Ok(obj_into_val(make_array(items), &mut self.heap));
                }
                let mut items: Vec<Value> = Vec::with_capacity(effective.min(16));
                for piece in text.split(&sep) {
                    if items.len() >= effective {
                        if user_limited {
                            break;
                        }
                        return Err(VMError::TypeError(
                            crate::vm::ERR_ARRAY_SIZE.to_string(),
                        ));
                    }
                    items.push(obj_into_val(
                        Object::String(piece.to_string().into()),
                        &mut self.heap,
                    ));
                }
                Ok(obj_into_val(make_array(items), &mut self.heap))
            }
            BuiltinFunction::StringIncludes => {
                let receiver = builtin.receiver.ok_or_else(|| {
                    VMError::TypeError("String.includes missing receiver".to_string())
                })?;
                let text = match receiver {
                    Object::String(s) => s,
                    _ => return Ok(Value::from_bool(false)),
                };
                let needle = args
                    .first()
                    .map(|v| val_inspect(*v, &self.heap))
                    .unwrap_or_default();
                Ok(Value::from_bool(text.contains(&needle)))
            }
            BuiltinFunction::StringSlice => {
                let receiver = builtin.receiver.ok_or_else(|| {
                    VMError::TypeError("String.slice missing receiver".to_string())
                })?;
                let text = match receiver {
                    Object::String(s) => s,
                    _ => return Ok(obj_into_val(Object::String(Rc::from("")), &mut self.heap)),
                };
                let chars: Vec<char> = text.chars().collect();
                let len = chars.len() as i32;
                let start = args
                    .first()
                    .map(|v| self.to_i32_val(*v))
                    .transpose()?
                    .unwrap_or(0);
                let end = args
                    .get(1)
                    .map(|v| self.to_i32_val(*v))
                    .transpose()?
                    .unwrap_or(len);
                let (sidx, eidx) = Self::slice_bounds(start, end, len);
                let out: String = chars[sidx as usize..eidx as usize].iter().collect();
                Ok(obj_into_val(Object::String(out.into()), &mut self.heap))
            }
            BuiltinFunction::StringToUpperCase => {
                let receiver = builtin.receiver.ok_or_else(|| {
                    VMError::TypeError("String.toUpperCase missing receiver".to_string())
                })?;
                let text = match receiver {
                    Object::String(s) => s,
                    _ => return Ok(obj_into_val(Object::String(Rc::from("")), &mut self.heap)),
                };
                Ok(obj_into_val(
                    Object::String(text.to_uppercase().into()),
                    &mut self.heap,
                ))
            }
            BuiltinFunction::StringToLowerCase => {
                let receiver = builtin.receiver.ok_or_else(|| {
                    VMError::TypeError("String.toLowerCase missing receiver".to_string())
                })?;
                let text = match receiver {
                    Object::String(s) => s,
                    _ => return Ok(obj_into_val(Object::String(Rc::from("")), &mut self.heap)),
                };
                Ok(obj_into_val(
                    Object::String(text.to_lowercase().into()),
                    &mut self.heap,
                ))
            }
            BuiltinFunction::StringTrim => {
                let receiver = builtin.receiver.ok_or_else(|| {
                    VMError::TypeError("String.trim missing receiver".to_string())
                })?;
                let text = match receiver {
                    Object::String(s) => s,
                    _ => return Ok(obj_into_val(Object::String(Rc::from("")), &mut self.heap)),
                };
                Ok(obj_into_val(
                    Object::String(text.trim().to_string().into()),
                    &mut self.heap,
                ))
            }
            BuiltinFunction::StringStartsWith => {
                let receiver = builtin.receiver.ok_or_else(|| {
                    VMError::TypeError("String.startsWith missing receiver".to_string())
                })?;
                let text = match receiver {
                    Object::String(s) => s,
                    _ => return Ok(Value::from_bool(false)),
                };
                let needle = args
                    .first()
                    .map(|v| val_inspect(*v, &self.heap))
                    .unwrap_or_default();
                let pos = args
                    .get(1)
                    .map(|v| self.to_i32_val(*v))
                    .transpose()?
                    .unwrap_or(0)
                    .max(0) as usize;
                let slice: String = text.chars().skip(pos).collect();
                Ok(Value::from_bool(slice.starts_with(&needle)))
            }
            BuiltinFunction::StringEndsWith => {
                let receiver = builtin.receiver.ok_or_else(|| {
                    VMError::TypeError("String.endsWith missing receiver".to_string())
                })?;
                let text = match receiver {
                    Object::String(s) => s,
                    _ => return Ok(Value::from_bool(false)),
                };
                let needle = args
                    .first()
                    .map(|v| val_inspect(*v, &self.heap))
                    .unwrap_or_default();
                if let Some(end_pos_obj) = args.get(1) {
                    let end_pos = self.to_i32_val(*end_pos_obj)?.max(0) as usize;
                    let truncated: String = text.chars().take(end_pos).collect();
                    Ok(Value::from_bool(truncated.ends_with(&needle)))
                } else {
                    Ok(Value::from_bool(text.ends_with(&needle)))
                }
            }
            BuiltinFunction::StringIndexOf => {
                let receiver = builtin.receiver.ok_or_else(|| {
                    VMError::TypeError("String.indexOf missing receiver".to_string())
                })?;
                let text = match receiver {
                    Object::String(s) => s,
                    _ => return Ok(Value::from_i64(-1)),
                };
                let needle = args
                    .first()
                    .map(|v| val_inspect(*v, &self.heap))
                    .unwrap_or_default();
                let from = args
                    .get(1)
                    .map(|v| self.to_i32_val(*v))
                    .transpose()?
                    .unwrap_or(0)
                    .max(0) as usize;
                if from >= text.len() {
                    return Ok(Value::from_i64(-1));
                }
                let sliced = &text[from..];
                if let Some(pos) = sliced.find(&needle) {
                    Ok(Value::from_i64((from + pos) as i64))
                } else {
                    Ok(Value::from_i64(-1))
                }
            }
            BuiltinFunction::StringLastIndexOf => {
                let receiver = builtin.receiver.ok_or_else(|| {
                    VMError::TypeError("String.lastIndexOf missing receiver".to_string())
                })?;
                let text = match receiver {
                    Object::String(s) => s,
                    _ => return Ok(Value::from_i64(-1)),
                };
                let needle = args
                    .first()
                    .map(|v| val_inspect(*v, &self.heap))
                    .unwrap_or_default();
                if needle.is_empty() {
                    let pos = args
                        .get(1)
                        .map(|v| self.to_i32_val(*v))
                        .transpose()?
                        .unwrap_or(text.len() as i32)
                        .max(0) as usize;
                    return Ok(Value::from_i64(pos.min(text.len()) as i64));
                }

                let default_pos = text.len().saturating_sub(needle.len()) as i32;
                let mut pos = args
                    .get(1)
                    .map(|v| self.to_i32_val(*v))
                    .transpose()?
                    .unwrap_or(default_pos)
                    .max(0) as usize;
                pos = pos.min(text.len().saturating_sub(needle.len()));

                for i in (0..=pos).rev() {
                    if text[i..].starts_with(&needle) {
                        return Ok(Value::from_i64(i as i64));
                    }
                }
                Ok(Value::from_i64(-1))
            }
            BuiltinFunction::StringSubstring => {
                let receiver = builtin.receiver.ok_or_else(|| {
                    VMError::TypeError("String.substring missing receiver".to_string())
                })?;
                let text = match receiver {
                    Object::String(s) => s,
                    _ => return Ok(obj_into_val(Object::String(Rc::from("")), &mut self.heap)),
                };
                let len = Self::string_char_len(&text) as i32;
                let mut start = args
                    .first()
                    .map(|v| self.to_i32_val(*v))
                    .transpose()?
                    .unwrap_or(0)
                    .max(0)
                    .min(len);
                let mut end = args
                    .get(1)
                    .map(|v| self.to_i32_val(*v))
                    .transpose()?
                    .unwrap_or(len)
                    .max(0)
                    .min(len);
                if start > end {
                    std::mem::swap(&mut start, &mut end);
                }
                let chars: Vec<char> = text.chars().collect();
                let out: String = chars[start as usize..end as usize].iter().collect();
                Ok(obj_into_val(Object::String(out.into()), &mut self.heap))
            }
            BuiltinFunction::StringRepeat => {
                let receiver = builtin.receiver.ok_or_else(|| {
                    VMError::TypeError("String.repeat missing receiver".to_string())
                })?;
                let text = match receiver {
                    Object::String(s) => s,
                    _ => return Ok(obj_into_val(Object::String(Rc::from("")), &mut self.heap)),
                };
                let count = args
                    .first()
                    .map(|v| self.to_i32_val(*v))
                    .transpose()?
                    .unwrap_or(0);
                if count < 0 {
                    return Err(VMError::TypeError(
                        "String.repeat count must be non-negative".to_string(),
                    ));
                }
                // Cap the product up front — `text.repeat(count)` itself
                // will happily allocate gigabytes and OOM the host.
                // `saturating_mul` guards against a malicious `count`
                // that would otherwise overflow `usize`.
                let total = (text.len() as usize).saturating_mul(count as usize);
                if total > crate::vm::MAX_STRING_LENGTH {
                    return Err(VMError::TypeError(format!(
                        "String.repeat: result length {} exceeds MAX_STRING_LENGTH ({})",
                        total,
                        crate::vm::MAX_STRING_LENGTH
                    )));
                }
                Ok(obj_into_val(
                    Object::String(text.repeat(count as usize).into()),
                    &mut self.heap,
                ))
            }
            BuiltinFunction::StringPadStart | BuiltinFunction::StringPadEnd => {
                let receiver = builtin.receiver.ok_or_else(|| {
                    VMError::TypeError("String.padStart/padEnd missing receiver".to_string())
                })?;
                let text = match receiver {
                    Object::String(s) => s,
                    _ => return Ok(obj_into_val(Object::String(Rc::from("")), &mut self.heap)),
                };
                let target_len = args
                    .first()
                    .map(|v| self.to_i32_val(*v))
                    .transpose()?
                    .unwrap_or(0)
                    .max(0) as usize;
                // Cap the requested pad width — a script asking for
                // `padStart(2_000_000_000, '·')` would otherwise loop
                // for hours building a 2 GiB pad string.
                if target_len > crate::vm::MAX_STRING_LENGTH {
                    return Err(VMError::TypeError(format!(
                        "String.padStart/padEnd: target length {} exceeds MAX_STRING_LENGTH ({})",
                        target_len,
                        crate::vm::MAX_STRING_LENGTH
                    )));
                }
                let text_len = Self::string_char_len(&text);
                if text_len >= target_len {
                    return Ok(obj_into_val(Object::String(text), &mut self.heap));
                }
                let fill = args
                    .get(1)
                    .map(|v| val_inspect(*v, &self.heap))
                    .unwrap_or_else(|| " ".to_string());
                if fill.is_empty() {
                    return Ok(obj_into_val(Object::String(text), &mut self.heap));
                }

                let needed = target_len - text_len;
                let mut pad = String::new();
                while Self::string_char_len(&pad) < needed {
                    pad.push_str(&fill);
                }
                let pad: String = pad.chars().take(needed).collect();
                if matches!(builtin.function, BuiltinFunction::StringPadStart) {
                    Ok(obj_into_val(
                        Object::String(format!("{}{}", pad, text).into()),
                        &mut self.heap,
                    ))
                } else {
                    Ok(obj_into_val(
                        Object::String(format!("{}{}", text, pad).into()),
                        &mut self.heap,
                    ))
                }
            }
            BuiltinFunction::StringCharCodeAt => {
                let receiver = builtin.receiver.ok_or_else(|| {
                    VMError::TypeError("String.charCodeAt missing receiver".to_string())
                })?;
                let text = match receiver {
                    Object::String(s) => s,
                    _ => return Ok(Value::from_f64(f64::NAN)),
                };
                let idx = args
                    .first()
                    .map(|v| self.to_i32_val(*v))
                    .transpose()?
                    .unwrap_or(0);
                if idx < 0 {
                    return Ok(Value::from_f64(f64::NAN));
                }
                match Self::string_nth_char(&text, idx as usize) {
                    Some(ch) => Ok(Value::from_i64(ch as i64)),
                    None => Ok(Value::from_f64(f64::NAN)),
                }
            }
            BuiltinFunction::StringReplace => {
                let receiver = builtin.receiver.ok_or_else(|| {
                    VMError::TypeError("String.replace missing receiver".to_string())
                })?;
                let text = match receiver {
                    Object::String(s) => s,
                    _ => return Ok(obj_into_val(Object::String(Rc::from("")), &mut self.heap)),
                };
                let replacement_val = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let replacement_value = val_to_obj(replacement_val, &self.heap);
                let replacement_is_fn = matches!(
                    replacement_value,
                    Object::CompiledFunction(_)
                        | Object::BoundMethod(_)
                        | Object::BuiltinFunction(_)
                        | Object::SuperRef(_)
                );
                let arg0 = args.first().map(|v| val_to_obj(*v, &self.heap));
                let out = match arg0.as_ref() {
                    Some(Object::RegExp(re)) => {
                        let regex = self.build_regex(&re.pattern, &re.flags)?;
                        if replacement_is_fn {
                            let mut out = String::new();
                            let mut last = 0usize;
                            let mut cb_args: Vec<Value> = Vec::with_capacity(4);
                            for caps in regex.captures_iter(&text) {
                                // Every callback iteration must honour
                                // wall-time + abort_flag — without this
                                // a pattern like `/./g` on a 10 M-char
                                // input invokes the callback 10 M times
                                // and only the top-of-dispatch batched
                                // check would fire, which doesn't see
                                // abort_flag.
                                self.check_builtin_callback_limits()?;
                                let Some(m) = caps.get(0) else {
                                    continue;
                                };
                                out.push_str(&text[last..m.start()]);
                                cb_args.clear();
                                cb_args.push(obj_into_val(
                                    Object::String(m.as_str().to_string().into()),
                                    &mut self.heap,
                                ));
                                for i in 1..caps.len() {
                                    if let Some(g) = caps.get(i) {
                                        cb_args.push(obj_into_val(
                                            Object::String(g.as_str().to_string().into()),
                                            &mut self.heap,
                                        ));
                                    } else {
                                        cb_args.push(Value::UNDEFINED);
                                    }
                                }
                                cb_args.push(Value::from_i64(m.start() as i64));
                                cb_args.push(obj_into_val(
                                    Object::String(text.clone()),
                                    &mut self.heap,
                                ));
                                let replace_result =
                                    self.call_value_slice(replacement_val, &cb_args)?;
                                let replace_obj = val_to_obj(replace_result, &self.heap);
                                out.push_str(&replace_obj.inspect());
                                if out.len() > crate::vm::MAX_STRING_LENGTH {
                                    return Err(VMError::TypeError(
                                        "String.replace: output exceeds MAX_STRING_LENGTH".into(),
                                    ));
                                }
                                last = m.end();
                                if !re.flags.contains('g') {
                                    break;
                                }
                            }
                            out.push_str(&text[last..]);
                            if out.len() > crate::vm::MAX_STRING_LENGTH {
                                return Err(VMError::TypeError(
                                    "String.replace: output exceeds MAX_STRING_LENGTH".into(),
                                ));
                            }
                            out
                        } else {
                            let replacement_template = replacement_value.inspect();
                            let mut out = String::new();
                            let mut last = 0usize;
                            for caps in regex.captures_iter(&text) {
                                let Some(m) = caps.get(0) else {
                                    continue;
                                };
                                out.push_str(&text[last..m.start()]);
                                let mut groups = Vec::with_capacity(caps.len().saturating_sub(1));
                                for i in 1..caps.len() {
                                    groups.push(caps.get(i).map(|g| g.as_str().to_string()));
                                }
                                let expanded = Self::expand_js_replacement(
                                    &replacement_template,
                                    m.as_str(),
                                    &groups,
                                    &text[..m.start()],
                                    &text[m.end()..],
                                );
                                out.push_str(&expanded);
                                if out.len() > crate::vm::MAX_STRING_LENGTH {
                                    return Err(VMError::TypeError(
                                        "String.replace: output exceeds MAX_STRING_LENGTH".into(),
                                    ));
                                }
                                last = m.end();
                                if !re.flags.contains('g') {
                                    break;
                                }
                            }
                            out.push_str(&text[last..]);
                            out
                        }
                    }
                    Some(pattern) => {
                        let p = pattern.inspect();
                        if replacement_is_fn {
                            if let Some(start) = text.find(&p) {
                                let end = start + p.len();
                                let match_val =
                                    obj_into_val(Object::String(p.clone().into()), &mut self.heap);
                                let text_val =
                                    obj_into_val(Object::String(text.clone()), &mut self.heap);
                                let replacement_result = self.call_value3(
                                    replacement_val,
                                    match_val,
                                    Value::from_i64(start as i64),
                                    text_val,
                                )?;
                                let mut out = String::new();
                                out.push_str(&text[..start]);
                                out.push_str(&val_inspect(replacement_result, &self.heap));
                                out.push_str(&text[end..]);
                                out
                            } else {
                                text.to_string()
                            }
                        } else {
                            let replacement_template = replacement_value.inspect();
                            if let Some(start) = text.find(&p) {
                                let end = start + p.len();
                                let mut out = String::new();
                                out.push_str(&text[..start]);
                                out.push_str(&Self::expand_js_replacement(
                                    &replacement_template,
                                    &p,
                                    &[],
                                    &text[..start],
                                    &text[end..],
                                ));
                                out.push_str(&text[end..]);
                                out
                            } else {
                                text.to_string()
                            }
                        }
                    }
                    None => text.to_string(),
                };
                Ok(obj_into_val(Object::String(out.into()), &mut self.heap))
            }
            BuiltinFunction::StringReplaceAll => {
                let receiver = builtin.receiver.ok_or_else(|| {
                    VMError::TypeError("String.replaceAll missing receiver".to_string())
                })?;
                let text = match receiver {
                    Object::String(s) => s,
                    _ => return Ok(obj_into_val(Object::String(Rc::from("")), &mut self.heap)),
                };
                let replacement_val = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let replacement_value = val_to_obj(replacement_val, &self.heap);
                let replacement_is_fn = matches!(
                    replacement_value,
                    Object::CompiledFunction(_)
                        | Object::BoundMethod(_)
                        | Object::BuiltinFunction(_)
                        | Object::SuperRef(_)
                );

                let arg0 = args.first().map(|v| val_to_obj(*v, &self.heap));
                let out = match arg0.as_ref() {
                    Some(Object::RegExp(re)) => {
                        if !re.flags.contains('g') {
                            return Err(VMError::TypeError(
                                "String.prototype.replaceAll called with a non-global RegExp"
                                    .to_string(),
                            ));
                        }
                        let regex = self.build_regex(&re.pattern, &re.flags)?;
                        if replacement_is_fn {
                            let mut out = String::new();
                            let mut last = 0usize;
                            let mut cb_args: Vec<Value> = Vec::with_capacity(4);
                            for caps in regex.captures_iter(&text) {
                                self.check_builtin_callback_limits()?;
                                let Some(m) = caps.get(0) else {
                                    continue;
                                };
                                out.push_str(&text[last..m.start()]);
                                cb_args.clear();
                                cb_args.push(obj_into_val(
                                    Object::String(m.as_str().to_string().into()),
                                    &mut self.heap,
                                ));
                                for i in 1..caps.len() {
                                    if let Some(g) = caps.get(i) {
                                        cb_args.push(obj_into_val(
                                            Object::String(g.as_str().to_string().into()),
                                            &mut self.heap,
                                        ));
                                    } else {
                                        cb_args.push(Value::UNDEFINED);
                                    }
                                }
                                cb_args.push(Value::from_i64(m.start() as i64));
                                cb_args.push(obj_into_val(
                                    Object::String(text.clone()),
                                    &mut self.heap,
                                ));
                                let replace_result =
                                    self.call_value_slice(replacement_val, &cb_args)?;
                                let replace_obj = val_to_obj(replace_result, &self.heap);
                                out.push_str(&replace_obj.inspect());
                                if out.len() > crate::vm::MAX_STRING_LENGTH {
                                    return Err(VMError::TypeError(
                                        "String.replaceAll: output exceeds MAX_STRING_LENGTH".into(),
                                    ));
                                }
                                last = m.end();
                            }
                            out.push_str(&text[last..]);
                            out
                        } else {
                            let replacement_template = replacement_value.inspect();
                            let mut out = String::new();
                            let mut last = 0usize;
                            for caps in regex.captures_iter(&text) {
                                let Some(m) = caps.get(0) else {
                                    continue;
                                };
                                out.push_str(&text[last..m.start()]);
                                let mut groups = Vec::with_capacity(caps.len().saturating_sub(1));
                                for i in 1..caps.len() {
                                    groups.push(caps.get(i).map(|g| g.as_str().to_string()));
                                }
                                let expanded = Self::expand_js_replacement(
                                    &replacement_template,
                                    m.as_str(),
                                    &groups,
                                    &text[..m.start()],
                                    &text[m.end()..],
                                );
                                out.push_str(&expanded);
                                if out.len() > crate::vm::MAX_STRING_LENGTH {
                                    return Err(VMError::TypeError(
                                        "String.replaceAll: output exceeds MAX_STRING_LENGTH".into(),
                                    ));
                                }
                                last = m.end();
                            }
                            out.push_str(&text[last..]);
                            out
                        }
                    }
                    Some(pattern) => {
                        let p = pattern.inspect();
                        if p.is_empty() {
                            return Ok(obj_into_val(Object::String(text), &mut self.heap));
                        }
                        if replacement_is_fn {
                            let mut out = String::new();
                            let mut cursor = 0usize;
                            while let Some(rel) = text[cursor..].find(&p) {
                                self.check_builtin_callback_limits()?;
                                let start = cursor + rel;
                                let end = start + p.len();
                                out.push_str(&text[cursor..start]);
                                let match_val =
                                    obj_into_val(Object::String(p.clone().into()), &mut self.heap);
                                let text_val =
                                    obj_into_val(Object::String(text.clone()), &mut self.heap);
                                let repl = self.call_value3(
                                    replacement_val,
                                    match_val,
                                    Value::from_i64(start as i64),
                                    text_val,
                                )?;
                                out.push_str(&val_inspect(repl, &self.heap));
                                if out.len() > crate::vm::MAX_STRING_LENGTH {
                                    return Err(VMError::TypeError(
                                        "String.replaceAll: output exceeds MAX_STRING_LENGTH".into(),
                                    ));
                                }
                                cursor = end;
                            }
                            out.push_str(&text[cursor..]);
                            out
                        } else {
                            let replacement_template = replacement_value.inspect();
                            let mut out = String::new();
                            let mut cursor = 0usize;
                            while let Some(rel) = text[cursor..].find(&p) {
                                let start = cursor + rel;
                                let end = start + p.len();
                                out.push_str(&text[cursor..start]);
                                out.push_str(&Self::expand_js_replacement(
                                    &replacement_template,
                                    &p,
                                    &[],
                                    &text[..start],
                                    &text[end..],
                                ));
                                if out.len() > crate::vm::MAX_STRING_LENGTH {
                                    return Err(VMError::TypeError(
                                        "String.replaceAll: output exceeds MAX_STRING_LENGTH".into(),
                                    ));
                                }
                                cursor = end;
                            }
                            out.push_str(&text[cursor..]);
                            out
                        }
                    }
                    None => text.to_string(),
                };
                Ok(obj_into_val(Object::String(out.into()), &mut self.heap))
            }
            BuiltinFunction::NumberToFixed => {
                let receiver = builtin.receiver.ok_or_else(|| {
                    VMError::TypeError("Number.toFixed missing receiver".to_string())
                })?;
                let n = self.to_number(&receiver)?;
                let digits = args
                    .first()
                    .map(|v| self.to_u32_val(*v))
                    .transpose()?
                    .unwrap_or(0);
                // ECMA-262 §21.1.3.3: fractionDigits must be 0..100.
                // Without this clamp `(3.14).toFixed(4e9)` asks `format!`
                // to emit a 4 GiB string of zeros.
                if digits > 100 {
                    return Err(VMError::TypeError(
                        "Number.toFixed: digits out of range (must be 0..=100)".to_string(),
                    ));
                }
                Ok(obj_into_val(
                    Object::String(format!("{:.*}", digits as usize, n).into()),
                    &mut self.heap,
                ))
            }
            BuiltinFunction::NumberToString => {
                let receiver = builtin.receiver.ok_or_else(|| {
                    VMError::TypeError("Number.toString missing receiver".to_string())
                })?;
                let n = self.to_number(&receiver)?;
                let radix = args
                    .first()
                    .map(|v| self.to_u32_val(*v))
                    .transpose()?
                    .unwrap_or(10);
                if !n.is_finite() {
                    return Ok(obj_into_val(
                        Object::String(if n.is_nan() {
                            "NaN".into()
                        } else if n.is_sign_negative() {
                            "-Infinity".into()
                        } else {
                            "Infinity".into()
                        }),
                        &mut self.heap,
                    ));
                }

                if radix == 10 {
                    if n.fract() == 0.0 {
                        Ok(obj_into_val(
                            Object::String((n as i64).to_string().into()),
                            &mut self.heap,
                        ))
                    } else {
                        Ok(obj_into_val(
                            Object::String(n.to_string().into()),
                            &mut self.heap,
                        ))
                    }
                } else if (2..=36).contains(&radix) {
                    Ok(obj_into_val(
                        Object::String(Self::int_to_radix_string(n as i64, radix).into()),
                        &mut self.heap,
                    ))
                } else {
                    Err(VMError::TypeError(
                        "Number.toString radix must be between 2 and 36".to_string(),
                    ))
                }
            }
            BuiltinFunction::ParseInt => {
                let input = args
                    .first()
                    .map(|v| val_inspect(*v, &self.heap))
                    .unwrap_or_default();
                let radix_opt = args.get(1).map(|v| self.to_i32_val(*v)).transpose()?;
                let trimmed = input.trim_start();
                let sign = if trimmed.starts_with('-') {
                    -1i64
                } else {
                    1i64
                };
                let mut body = if trimmed.starts_with('-') || trimmed.starts_with('+') {
                    &trimmed[1..]
                } else {
                    trimmed
                };
                let mut base = radix_opt.unwrap_or(0);
                if base == 0 {
                    if body.starts_with("0x") || body.starts_with("0X") {
                        base = 16;
                        body = &body[2..];
                    } else {
                        base = 10;
                    }
                } else if base == 16 && (body.starts_with("0x") || body.starts_with("0X")) {
                    body = &body[2..];
                }
                if !(2..=36).contains(&base) {
                    return Ok(Value::from_f64(f64::NAN));
                }
                // Bound the digit run scanned. `parseInt("1".repeat(10_000_000))`
                // otherwise spends ~100 ms collecting the entire
                // MAX_STRING_LENGTH input before `i64::from_str_radix`
                // even runs. i64 can hold at most ~19 decimal digits,
                // so a 128-char scan already covers every overflow
                // case the parser can represent exactly.
                const MAX_PARSE_INT_DIGITS: usize = 128;
                let digits: String = body
                    .chars()
                    .take(MAX_PARSE_INT_DIGITS)
                    .take_while(|c| c.is_digit(base as u32))
                    .collect();
                if digits.is_empty() {
                    return Ok(Value::from_f64(f64::NAN));
                }
                match i64::from_str_radix(&digits, base as u32) {
                    Ok(v) => Ok(Value::from_i64(v * sign)),
                    Err(_) => Ok(Value::from_f64(f64::NAN)),
                }
            }
            BuiltinFunction::ParseFloat => {
                let input = args
                    .first()
                    .map(|v| val_inspect(*v, &self.heap))
                    .unwrap_or_default();
                let trimmed = input.trim_start();
                let mut buf = String::new();
                let mut has_digit = false;
                let mut has_dot = false;
                let mut has_exp = false;
                let mut prev_was_exp = false;
                // Same rationale as parseInt — an f64 has at most
                // ~17 significant digits plus an exponent; scanning
                // beyond 128 chars buys no additional precision.
                const MAX_PARSE_FLOAT_CHARS: usize = 128;
                for ch in trimmed.chars().take(MAX_PARSE_FLOAT_CHARS) {
                    if (ch == '+' || ch == '-') && (buf.is_empty() || prev_was_exp) {
                        buf.push(ch);
                        prev_was_exp = false;
                        continue;
                    }
                    if ch.is_ascii_digit() {
                        buf.push(ch);
                        has_digit = true;
                        prev_was_exp = false;
                        continue;
                    }
                    if ch == '.' && !has_dot && !has_exp {
                        buf.push(ch);
                        has_dot = true;
                        prev_was_exp = false;
                        continue;
                    }
                    if (ch == 'e' || ch == 'E') && !has_exp && has_digit {
                        buf.push(ch);
                        has_exp = true;
                        prev_was_exp = true;
                        continue;
                    }
                    break;
                }
                if !has_digit {
                    return Ok(Value::from_f64(f64::NAN));
                }
                match buf.parse::<f64>() {
                    Ok(v) => {
                        if v.fract() == 0.0 {
                            Ok(Value::from_i64(v as i64))
                        } else {
                            Ok(Value::from_f64(v))
                        }
                    }
                    Err(_) => Ok(Value::from_f64(f64::NAN)),
                }
            }
            BuiltinFunction::IsNaN => {
                if args.is_empty() {
                    return Ok(Value::from_bool(true));
                }
                let n = self.to_number_val(args[0]).unwrap_or(f64::NAN);
                Ok(Value::from_bool(n.is_nan()))
            }
            BuiltinFunction::IsFinite => {
                if args.is_empty() {
                    return Ok(Value::from_bool(false));
                }
                let n = self.to_number_val(args[0]).unwrap_or(f64::NAN);
                Ok(Value::from_bool(n.is_finite()))
            }
            BuiltinFunction::NumberIsNaN => {
                let obj = args.first().map(|v| val_to_obj(*v, &self.heap));
                match obj.as_ref() {
                    Some(Object::Float(v)) => Ok(Value::from_bool(v.is_nan())),
                    Some(Object::Integer(_)) => Ok(Value::from_bool(false)),
                    _ => Ok(Value::from_bool(false)),
                }
            }
            BuiltinFunction::NumberIsFinite => {
                let obj = args.first().map(|v| val_to_obj(*v, &self.heap));
                match obj.as_ref() {
                    Some(Object::Integer(_)) => Ok(Value::from_bool(true)),
                    Some(Object::Float(v)) => Ok(Value::from_bool(v.is_finite())),
                    _ => Ok(Value::from_bool(false)),
                }
            }
            BuiltinFunction::NumberIsInteger => {
                let obj = args.first().map(|v| val_to_obj(*v, &self.heap));
                match obj.as_ref() {
                    Some(Object::Integer(_)) => Ok(Value::from_bool(true)),
                    Some(Object::Float(v)) => {
                        Ok(Value::from_bool(v.is_finite() && v.fract() == 0.0))
                    }
                    _ => Ok(Value::from_bool(false)),
                }
            }
            BuiltinFunction::NumberIsSafeInteger => {
                let obj = args.first().map(|v| val_to_obj(*v, &self.heap));
                match obj.as_ref() {
                    Some(Object::Integer(v)) => {
                        Ok(Value::from_bool((*v as f64).abs() <= 9007199254740991.0))
                    }
                    Some(Object::Float(v)) => Ok(Value::from_bool(
                        v.is_finite() && v.fract() == 0.0 && v.abs() <= 9007199254740991.0,
                    )),
                    _ => Ok(Value::from_bool(false)),
                }
            }
            BuiltinFunction::ObjectKeys => {
                let source_val = args.first().copied().unwrap_or(Value::UNDEFINED);
                let source = val_to_obj(source_val, &self.heap);
                Ok(obj_into_val(
                    make_array(self.get_keys_array(source)),
                    &mut self.heap,
                ))
            }
            BuiltinFunction::ObjectValues => {
                let source_val = args.first().copied().unwrap_or(Value::UNDEFINED);
                let source = val_to_obj(source_val, &self.heap);
                let out: Vec<Value> = match source {
                    Object::Hash(hash) => {
                        let hash_b = unsafe { hash.borrow_mut() };
                        hash_b.sync_pairs_if_dirty();
                        self.ordered_hash_keys_js(hash_b)
                            .into_iter()
                            .filter_map(|k| hash_b.pairs.get(&k).copied())
                            .collect()
                    }
                    Object::String(s) => s
                        .chars()
                        .map(|c| obj_into_val(Object::String(c.to_string().into()), &mut self.heap))
                        .collect(),
                    Object::Array(items) => unwrap_array(items),
                    _ => vec![],
                };
                Ok(obj_into_val(make_array(out), &mut self.heap))
            }
            BuiltinFunction::ObjectEntries => {
                let source_val = args.first().copied().unwrap_or(Value::UNDEFINED);
                let source = val_to_obj(source_val, &self.heap);
                let mut out: Vec<Value> = vec![];
                match source {
                    Object::Hash(hash) => {
                        let hash_b = unsafe { hash.borrow_mut() };
                        hash_b.sync_pairs_if_dirty();
                        for k in self.ordered_hash_keys_js(hash_b) {
                            let Some(v) = hash_b.pairs.get(&k).copied() else {
                                continue;
                            };
                            let key_val = obj_into_val(
                                Object::String(k.display_key().into()),
                                &mut self.heap,
                            );
                            let entry = make_array(vec![key_val, v]);
                            out.push(obj_into_val(entry, &mut self.heap));
                        }
                    }
                    Object::Array(items) => {
                        for (i, v) in unwrap_array(items).into_iter().enumerate() {
                            let idx_val =
                                obj_into_val(Object::String(i.to_string().into()), &mut self.heap);
                            let entry = make_array(vec![idx_val, v]);
                            out.push(obj_into_val(entry, &mut self.heap));
                        }
                    }
                    Object::String(s) => {
                        for (i, c) in s.chars().enumerate() {
                            let idx_val =
                                obj_into_val(Object::String(i.to_string().into()), &mut self.heap);
                            let ch_val =
                                obj_into_val(Object::String(c.to_string().into()), &mut self.heap);
                            let entry = make_array(vec![idx_val, ch_val]);
                            out.push(obj_into_val(entry, &mut self.heap));
                        }
                    }
                    _ => {}
                }
                Ok(obj_into_val(make_array(out), &mut self.heap))
            }
            BuiltinFunction::ObjectFromEntries => {
                let source_val = args.first().copied().unwrap_or(Value::UNDEFINED);
                let source = val_to_obj(source_val, &self.heap);
                let mut out = crate::object::HashObject::default();
                if let Object::Array(items) = source {
                    // Wall-time / abort re-check every 1 024 entries.
                    // `Array.from({length:1e6}, …)` → `Object.fromEntries(…)`
                    // could otherwise spin through a million inserts
                    // without hitting any runtime limit — `insert_pair`
                    // grows the IndexMap without bumping heap accounting.
                    for (i, item) in unwrap_array(items).into_iter().enumerate() {
                        if (i & 0x3ff) == 0 {
                            self.check_builtin_callback_limits()?;
                        }
                        if out.pairs.len() >= MAX_ARRAY_SIZE {
                            return Err(VMError::TypeError(
                                "Object.fromEntries: entry count exceeds MAX_ARRAY_SIZE".to_string(),
                            ));
                        }
                        let item_obj = val_to_obj(item, &self.heap);
                        if let Object::Array(entry) = item_obj {
                            let entry = entry.borrow();
                            if entry.len() < 2 {
                                continue;
                            }
                            let key_obj = val_to_obj(entry[0], &self.heap);
                            let key = self.hash_key_from_object(&key_obj);
                            let val = entry[1];
                            out.insert_pair(key, val);
                        }
                    }
                }
                Ok(obj_into_val(make_hash(out), &mut self.heap))
            }
            BuiltinFunction::ObjectHasOwn => {
                if args.len() < 2 {
                    return Ok(Value::from_bool(false));
                }
                let arg0 = val_to_obj(args[0], &self.heap);
                let arg1 = val_to_obj(args[1], &self.heap);
                let has = match &arg0 {
                    Object::Hash(hash) => {
                        let k = self.hash_key_from_object(&arg1);
                        hash.borrow().pairs.contains_key(&k)
                    }
                    Object::Array(items) => {
                        let items = items.borrow();
                        match &arg1 {
                            Object::String(key_str) => {
                                if &**key_str == "length" {
                                    true
                                } else if let Some(idx) = Self::parse_non_negative_usize(key_str) {
                                    idx < items.len()
                                } else {
                                    false
                                }
                            }
                            other => Self::numeric_array_index(other)
                                .map(|idx| idx < items.len())
                                .unwrap_or(false),
                        }
                    }
                    Object::String(s) => {
                        let s_len = Self::string_char_len(s);
                        match &arg1 {
                            Object::String(key_str) => {
                                if &**key_str == "length" {
                                    true
                                } else if let Some(idx) = Self::parse_non_negative_usize(key_str) {
                                    idx < s_len
                                } else {
                                    false
                                }
                            }
                            other => Self::numeric_array_index(other)
                                .map(|idx| idx < s_len)
                                .unwrap_or(false),
                        }
                    }
                    _ => false,
                };
                Ok(Value::from_bool(has))
            }
            BuiltinFunction::ObjectIs => {
                if args.len() < 2 {
                    return Ok(Value::from_bool(true));
                }
                let a = val_to_obj(args[0], &self.heap);
                let b = val_to_obj(args[1], &self.heap);
                Ok(Value::from_bool(self.same_value(&a, &b)))
            }
            BuiltinFunction::ObjectAssign => {
                if args.is_empty() {
                    return Ok(Value::UNDEFINED);
                }

                let target_val = args[0];

                // Collect source entries first (avoids borrow conflicts).
                // Wall-time / abort-flag re-checks run per source and every
                // 1 024 entries; combined entry count capped at
                // MAX_ARRAY_SIZE — `Object.assign({}, s, s, s, s, s, s)`
                // over a 100 K-entry `s` used to commit ~600 K entry
                // tuples unconditionally before `insert_pair` even ran.
                let mut source_entries: Vec<Vec<(HashKey, Value)>> = Vec::new();
                let mut total_entries: usize = 0;
                for source_val in args.iter().skip(1) {
                    self.check_builtin_callback_limits()?;
                    let source = val_to_obj(*source_val, &self.heap);
                    let mut entries = Vec::new();
                    match &source {
                        Object::Hash(hash) => {
                            let hash_b = unsafe { hash.borrow_mut() };
                            hash_b.sync_pairs_if_dirty();
                            for (i, (k, v)) in hash_b.pairs.iter().enumerate() {
                                if (i & 0x3ff) == 0 {
                                    self.check_builtin_callback_limits()?;
                                }
                                if total_entries + entries.len() >= MAX_ARRAY_SIZE {
                                    return Err(VMError::TypeError(
                                        "Object.assign: combined entry count exceeds MAX_ARRAY_SIZE".to_string(),
                                    ));
                                }
                                entries.push((k.clone(), *v));
                            }
                        }
                        Object::Array(items) => {
                            for (i, v) in items.borrow().iter().enumerate() {
                                if (i & 0x3ff) == 0 {
                                    self.check_builtin_callback_limits()?;
                                }
                                if total_entries + entries.len() >= MAX_ARRAY_SIZE {
                                    return Err(VMError::TypeError(
                                        "Object.assign: combined entry count exceeds MAX_ARRAY_SIZE".to_string(),
                                    ));
                                }
                                entries.push((HashKey::from_string(&i.to_string()), *v));
                            }
                        }
                        Object::String(s) => {
                            for (i, ch) in s.chars().enumerate() {
                                if (i & 0x3ff) == 0 {
                                    self.check_builtin_callback_limits()?;
                                }
                                if total_entries + entries.len() >= MAX_ARRAY_SIZE {
                                    return Err(VMError::TypeError(
                                        "Object.assign: combined entry count exceeds MAX_ARRAY_SIZE".to_string(),
                                    ));
                                }
                                let val = obj_into_val(
                                    Object::String(ch.to_string().into()),
                                    &mut self.heap,
                                );
                                entries.push((HashKey::from_string(&i.to_string()), val));
                            }
                        }
                        _ => {}
                    }
                    total_entries = total_entries.saturating_add(entries.len());
                    source_entries.push(entries);
                }

                // Mutate target in-place (or create new if not a hash)
                if target_val.is_heap() {
                    let heap_obj = unsafe {
                        &*self
                            .heap
                            .objects
                            .as_ptr()
                            .add(target_val.heap_index() as usize)
                    };
                    if let Object::Hash(hash_rc) = heap_obj {
                        let target = unsafe { hash_rc.borrow_mut() };
                        for entries in source_entries {
                            for (k, v) in entries {
                                target.insert_pair(k, v);
                            }
                        }
                        return Ok(target_val);
                    }
                }

                // Fallback: create new hash
                let mut target = crate::object::HashObject::default();
                for entries in source_entries {
                    for (k, v) in entries {
                        target.insert_pair(k, v);
                    }
                }
                Ok(obj_into_val(make_hash(target), &mut self.heap))
            }
            BuiltinFunction::ObjectFreeze => {
                let target_val = args.first().copied().unwrap_or(Value::UNDEFINED);
                if target_val.is_heap() {
                    let heap_obj = unsafe {
                        &*self
                            .heap
                            .objects
                            .as_ptr()
                            .add(target_val.heap_index() as usize)
                    };
                    if let Object::Hash(hash_rc) = heap_obj {
                        unsafe { hash_rc.borrow_mut() }.frozen = true;
                    }
                }
                Ok(target_val)
            }
            BuiltinFunction::ObjectCreate => {
                let proto_val = args.first().copied().unwrap_or(Value::UNDEFINED);
                if proto_val.is_null() {
                    return Ok(obj_into_val(make_hash(HashObject::default()), &mut self.heap));
                }
                if proto_val.is_heap() {
                    let heap_idx = proto_val.heap_index() as usize;
                    let proto_obj = unsafe { &*self.heap.objects.as_ptr().add(heap_idx) };
                    if let Object::Hash(h) = proto_obj {
                        let mut new_hash = HashObject::default();
                        let h = h.borrow();
                        for (key, &val) in h.pairs.iter() {
                            new_hash.pairs.insert(key.clone(), val);
                        }
                        new_hash.values = h.values.clone();
                        new_hash.str_slots = h.str_slots.clone();
                        if let Some(ref getters) = h.getters {
                            new_hash.getters = Some(getters.clone());
                        }
                        if let Some(ref setters) = h.setters {
                            new_hash.setters = Some(setters.clone());
                        }
                        return Ok(obj_into_val(make_hash(new_hash), &mut self.heap));
                    }
                }
                Ok(obj_into_val(make_hash(HashObject::default()), &mut self.heap))
            }
            BuiltinFunction::ObjectDefineProperty => {
                // Object.defineProperty(obj, prop, descriptor)
                let obj_val = args.first().copied().unwrap_or(Value::UNDEFINED);
                let prop_val = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let desc_val = args.get(2).copied().unwrap_or(Value::UNDEFINED);

                let prop_name = val_inspect(prop_val, &self.heap);
                // Defence-in-depth: reject property names that would
                // become prototype-poisoning primitives in a future
                // engine that implements a real prototype chain. Our
                // `ObjectGetPrototypeOf` currently returns `null`, so
                // these aren't live today — but a malicious script's
                // defineProperty call should not silently set them.
                if matches!(prop_name.as_str(), "__proto__" | "constructor" | "prototype") {
                    return Err(VMError::TypeError(format!(
                        "Object.defineProperty: refusing to define reserved property {:?}",
                        prop_name
                    )));
                }
                let prop_sym = crate::intern::intern(&prop_name);

                if obj_val.is_heap() {
                    let heap_idx = obj_val.heap_index() as usize;
                    let desc_obj = val_to_obj(desc_val, &self.heap);
                    if let Object::Hash(desc_h) = &desc_obj {
                        let desc_h = desc_h.borrow();
                        let val_sym = crate::intern::intern("value");
                        let get_sym = crate::intern::intern("get");
                        let set_sym = crate::intern::intern("set");

                        // Check for accessor descriptor (get/set)
                        let getter_val = desc_h.get_by_sym(get_sym);
                        let setter_val = desc_h.get_by_sym(set_sym);

                        if getter_val.is_some() || setter_val.is_some() {
                            // Accessor descriptor: install getter/setter
                            let getter_fn = getter_val.and_then(|v| {
                                match val_to_obj(v, &self.heap) {
                                    Object::CompiledFunction(f) => Some(*f),
                                    Object::BoundMethod(bm) => Some(bm.function),
                                    _ => None,
                                }
                            });
                            let setter_fn = setter_val.and_then(|v| {
                                match val_to_obj(v, &self.heap) {
                                    Object::CompiledFunction(f) => Some(*f),
                                    Object::BoundMethod(bm) => Some(bm.function),
                                    _ => None,
                                }
                            });
                            let heap_obj = unsafe { &mut *self.heap.objects.as_mut_ptr().add(heap_idx) };
                            if let Object::Hash(hash_rc) = heap_obj {
                                let hash = unsafe { hash_rc.borrow_mut() };
                                if let Some(gf) = getter_fn {
                                    hash.define_getter(prop_name.clone(), gf);
                                }
                                if let Some(sf) = setter_fn {
                                    hash.define_setter(prop_name.clone(), sf);
                                }
                            }
                        } else {
                            // Data descriptor: store value
                            let value = desc_h.get_by_sym(val_sym);
                            if let Some(val) = value {
                                let heap_obj = unsafe { &mut *self.heap.objects.as_mut_ptr().add(heap_idx) };
                                if let Object::Hash(hash_rc) = heap_obj {
                                    unsafe { hash_rc.borrow_mut() }.set_by_sym(prop_sym, val);
                                } else if let Object::Instance(inst) = heap_obj {
                                    inst.fields.insert(prop_name.clone(), val);
                                }
                            }
                        }
                    }
                }
                Ok(obj_val)
            }
            BuiltinFunction::ObjectGetPrototypeOf => {
                Ok(Value::NULL) // Simplified: return null
            }
            BuiltinFunction::ObjectGetOwnPropertyDescriptor => {
                let obj_val = args.first().copied().unwrap_or(Value::UNDEFINED);
                let prop_val = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let prop_name = val_inspect(prop_val, &self.heap);
                let prop_sym = crate::intern::intern(&prop_name);

                if obj_val.is_heap() {
                    let heap_obj = unsafe { &*self.heap.objects.as_ptr().add(obj_val.heap_index() as usize) };
                    if let Object::Hash(hash_rc) = heap_obj {
                        if let Some(val) = hash_rc.borrow().get_by_sym(prop_sym) {
                            let mut desc = HashObject::default();
                            desc.set_by_sym(crate::intern::intern("value"), val);
                            desc.set_by_sym(crate::intern::intern("writable"), Value::TRUE);
                            desc.set_by_sym(crate::intern::intern("enumerable"), Value::TRUE);
                            desc.set_by_sym(crate::intern::intern("configurable"), Value::TRUE);
                            return Ok(obj_into_val(make_hash(desc), &mut self.heap));
                        }
                    }
                }
                Ok(Value::UNDEFINED)
            }
            BuiltinFunction::ObjectGetOwnPropertyNames => {
                // Delegate to ObjectKeys implementation
                let obj_val = args.first().copied().unwrap_or(Value::UNDEFINED);
                if obj_val.is_heap() {
                    let heap_obj = unsafe { &*self.heap.objects.as_ptr().add(obj_val.heap_index() as usize) };
                    if let Object::Hash(h) = heap_obj {
                        let h = h.borrow();
                        let keys: Vec<Value> = h.pairs.keys()
                            .map(|k| {
                                let name: String = match k {
                                    HashKey::Sym(s) => crate::intern::resolve(*s).to_string(),
                                    HashKey::Int(i) => i.to_string(),
                                    _ => format!("{:?}", k),
                                };
                                obj_into_val(Object::String(name.into()), &mut self.heap)
                            })
                            .collect();
                        return Ok(obj_into_val(make_array(keys), &mut self.heap));
                    }
                }
                Ok(obj_into_val(make_array(vec![]), &mut self.heap))
            }
            BuiltinFunction::ArrayOf => {
                Ok(obj_into_val(make_array(args.to_vec()), &mut self.heap))
            }
            BuiltinFunction::HashHasOwnProperty => {
                let key_str = args
                    .first()
                    .map(|v| {
                        let obj = val_to_obj(*v, &self.heap);
                        match obj {
                            Object::String(s) => s.to_string(),
                            _ => obj.inspect(),
                        }
                    })
                    .unwrap_or_default();
                if let Some(Object::Hash(h)) = &builtin.receiver {
                    let has = h.borrow().contains_str(&key_str);
                    Ok(Value::from_bool(has))
                } else {
                    Ok(Value::FALSE)
                }
            }
            BuiltinFunction::ObjectPrototypeToString => {
                // Object.prototype.toString.call(value)
                // When called via .call(value), the value is in args[0] (the this).
                // When called as a method on receiver, use the receiver.
                let val = if let Some(ref recv) = builtin.receiver {
                    recv.clone()
                } else {
                    args.first()
                        .map(|v| val_to_obj(*v, &self.heap))
                        .unwrap_or(Object::Undefined)
                };
                let tag = match &val {
                    Object::Undefined => "[object Undefined]",
                    Object::Null => "[object Null]",
                    Object::Boolean(_) => "[object Boolean]",
                    Object::Integer(_) | Object::Float(_) => "[object Number]",
                    Object::String(_) => "[object String]",
                    Object::Array(_) => "[object Array]",
                    Object::Hash(_) => "[object Object]",
                    Object::CompiledFunction(_) | Object::BuiltinFunction(_)
                    | Object::BoundMethod(_) => "[object Function]",
                    Object::Instance(_) => "[object Object]",
                    Object::Error(_) => "[object Error]",
                    _ => "[object Object]",
                };
                Ok(obj_into_val(Object::String(tag.into()), &mut self.heap))
            }
            BuiltinFunction::JsonStringify => {
                let value = args.first().copied().unwrap_or(Value::UNDEFINED);
                let value_obj = val_to_obj(value, &self.heap);
                let json = self.object_to_json_value(&value_obj);
                Ok(obj_into_val(
                    Object::String(json.to_string().into()),
                    &mut self.heap,
                ))
            }
            BuiltinFunction::JsonParse => {
                let source = args
                    .first()
                    .map(|v| val_inspect(*v, &self.heap))
                    .unwrap_or_default();
                let parsed = self.json_parse(&source)?;
                Ok(obj_into_val(parsed, &mut self.heap))
            }
            BuiltinFunction::SymbolCtor => {
                static NEXT_SYMBOL_ID: std::sync::atomic::AtomicU32 =
                    std::sync::atomic::AtomicU32::new(100); // IDs < 100 reserved for well-known symbols
                let id = NEXT_SYMBOL_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let desc = args.first().and_then(|v| {
                    if v.is_undefined() {
                        None
                    } else {
                        let obj = val_to_obj(*v, &self.heap);
                        Some(Rc::from(obj.to_js_string().as_str()))
                    }
                });
                let sym = Object::Symbol(id, desc);
                Ok(obj_into_val(sym, &mut self.heap))
            }
            BuiltinFunction::PromiseResolve => {
                let value = args.first().copied().unwrap_or(Value::UNDEFINED);
                let value_obj = val_to_obj(value, &self.heap);
                let promise = crate::object::new_fulfilled_promise(value_obj);
                Ok(obj_into_val(promise, &mut self.heap))
            }
            BuiltinFunction::PromiseReject => {
                let value = args.first().copied().unwrap_or(Value::UNDEFINED);
                let value_obj = val_to_obj(value, &self.heap);
                let promise = crate::object::new_rejected_promise(value_obj);
                Ok(obj_into_val(promise, &mut self.heap))
            }
            // Concurrency methods. We have no microtask queue — every
            // promise this engine ever sees is already-settled — so the
            // semantics collapse to "inspect each input and combine the
            // settled states immediately". This matches V8 for the
            // already-resolved case (which is the only case scripts can
            // actually create today).
            BuiltinFunction::PromiseAll => {
                let arr_val = args.first().copied().unwrap_or(Value::UNDEFINED);
                let arr_obj = val_to_obj(arr_val, &self.heap);
                let items = match arr_obj {
                    Object::Array(a) => unwrap_array(a),
                    _ => {
                        return Err(VMError::TypeError(
                            "Promise.all expects an array".to_string(),
                        ))
                    }
                };
                let mut out_vals: Vec<Value> = Vec::with_capacity(items.len());
                for item in items.iter() {
                    let item_obj = val_to_obj(*item, &self.heap);
                    if let Object::Promise(p) = &item_obj {
                        let state = p.borrow().settled.clone();
                        match state {
                            PromiseState::Fulfilled(v) => {
                                out_vals.push(obj_into_val((*v).clone(), &mut self.heap));
                            }
                            PromiseState::Rejected(v) => {
                                let rej = crate::object::new_rejected_promise((*v).clone());
                                return Ok(obj_into_val(rej, &mut self.heap));
                            }
                            PromiseState::Pending => {
                                // Spec: the aggregate promise stays pending
                                // until every input settles. Engine has no
                                // event loop, so we conservatively treat
                                // pending as `undefined` for now — close
                                // enough for the synchronous-resolve cases
                                // the engine can actually produce today.
                                out_vals.push(Value::UNDEFINED);
                            }
                        }
                    } else {
                        out_vals.push(*item);
                    }
                }
                let arr = make_array(out_vals);
                let arr_v = obj_into_val(arr, &mut self.heap);
                let arr_obj = val_to_obj(arr_v, &self.heap);
                let p = crate::object::new_fulfilled_promise(arr_obj);
                Ok(obj_into_val(p, &mut self.heap))
            }
            BuiltinFunction::PromiseRace => {
                let arr_val = args.first().copied().unwrap_or(Value::UNDEFINED);
                let arr_obj = val_to_obj(arr_val, &self.heap);
                let items = match arr_obj {
                    Object::Array(a) => unwrap_array(a),
                    _ => {
                        return Err(VMError::TypeError(
                            "Promise.race expects an array".to_string(),
                        ))
                    }
                };
                if let Some(first) = items.first() {
                    let item_obj = val_to_obj(*first, &self.heap);
                    if matches!(item_obj, Object::Promise(_)) {
                        return Ok(*first);
                    }
                    let p = crate::object::new_fulfilled_promise(item_obj);
                    return Ok(obj_into_val(p, &mut self.heap));
                }
                // Empty input → forever-pending in spec; engine has no
                // pending state, so collapse to a never-resolving fulfilled
                // undefined (closest representable analogue).
                let p = crate::object::new_fulfilled_promise(Object::Undefined);
                Ok(obj_into_val(p, &mut self.heap))
            }
            BuiltinFunction::PromiseAllSettled => {
                let arr_val = args.first().copied().unwrap_or(Value::UNDEFINED);
                let arr_obj = val_to_obj(arr_val, &self.heap);
                let items = match arr_obj {
                    Object::Array(a) => unwrap_array(a),
                    _ => {
                        return Err(VMError::TypeError(
                            "Promise.allSettled expects an array".to_string(),
                        ))
                    }
                };
                let status_sym = crate::intern::intern("status");
                let value_sym = crate::intern::intern("value");
                let reason_sym = crate::intern::intern("reason");
                let mut results: Vec<Value> = Vec::with_capacity(items.len());
                for item in items.iter() {
                    let item_obj = val_to_obj(*item, &self.heap);
                    // Build values on the real heap first, then assemble the
                    // entry hash. `insert_pair_obj` is for compile-time
                    // construction (uses a private local_objects table) — at
                    // runtime that produces dangling heap indices.
                    let (status_str, payload_sym, payload_val) = match &item_obj {
                        Object::Promise(p) => {
                            let state = p.borrow().settled.clone();
                            match state {
                                PromiseState::Fulfilled(v) => (
                                    "fulfilled",
                                    value_sym,
                                    obj_into_val((*v).clone(), &mut self.heap),
                                ),
                                PromiseState::Rejected(v) => (
                                    "rejected",
                                    reason_sym,
                                    obj_into_val((*v).clone(), &mut self.heap),
                                ),
                                PromiseState::Pending => (
                                    "pending",
                                    value_sym,
                                    Value::UNDEFINED,
                                ),
                            }
                        }
                        _ => (
                            "fulfilled",
                            value_sym,
                            obj_into_val(item_obj.clone(), &mut self.heap),
                        ),
                    };
                    let status_val = obj_into_val(
                        Object::String(Rc::from(status_str)),
                        &mut self.heap,
                    );
                    let mut entry = HashObject::default();
                    entry.insert_pair(HashKey::Sym(status_sym), status_val);
                    entry.insert_pair(HashKey::Sym(payload_sym), payload_val);
                    let entry_val = obj_into_val(make_hash(entry), &mut self.heap);
                    results.push(entry_val);
                }
                let arr_v = obj_into_val(make_array(results), &mut self.heap);
                let arr_obj = val_to_obj(arr_v, &self.heap);
                let p = crate::object::new_fulfilled_promise(arr_obj);
                Ok(obj_into_val(p, &mut self.heap))
            }
            BuiltinFunction::PromiseAny => {
                let arr_val = args.first().copied().unwrap_or(Value::UNDEFINED);
                let arr_obj = val_to_obj(arr_val, &self.heap);
                let items = match arr_obj {
                    Object::Array(a) => unwrap_array(a),
                    _ => {
                        return Err(VMError::TypeError(
                            "Promise.any expects an array".to_string(),
                        ))
                    }
                };
                let mut errors: Vec<Object> = Vec::with_capacity(items.len());
                for item in items.iter() {
                    let item_obj = val_to_obj(*item, &self.heap);
                    if let Object::Promise(p) = &item_obj {
                        let state = p.borrow().settled.clone();
                        match state {
                            PromiseState::Fulfilled(_) => return Ok(*item),
                            PromiseState::Rejected(v) => errors.push((*v).clone()),
                            PromiseState::Pending => {
                                // Pending — skip; if every promise turns out
                                // to be pending we fall through to the
                                // all-rejected AggregateError path.
                            }
                        }
                    } else {
                        // Non-promise treated as fulfilled.
                        return Ok(*item);
                    }
                }
                // All rejected — wrap in AggregateError-like object (we
                // don't have AggregateError as a real type, so use a
                // plain hash with name/message/errors at runtime).
                let err_vals: Vec<Value> = errors
                    .into_iter()
                    .map(|o| obj_into_val(o, &mut self.heap))
                    .collect();
                let errors_arr_val =
                    obj_into_val(make_array(err_vals), &mut self.heap);
                let name_val = obj_into_val(
                    Object::String(Rc::from("AggregateError")),
                    &mut self.heap,
                );
                let msg_val = obj_into_val(
                    Object::String(Rc::from("All promises were rejected")),
                    &mut self.heap,
                );
                let mut hash = HashObject::default();
                hash.insert_pair(HashKey::Sym(crate::intern::intern("name")), name_val);
                hash.insert_pair(HashKey::Sym(crate::intern::intern("message")), msg_val);
                hash.insert_pair(HashKey::Sym(crate::intern::intern("errors")), errors_arr_val);
                let agg_v = obj_into_val(make_hash(hash), &mut self.heap);
                let agg_obj = val_to_obj(agg_v, &self.heap);
                let p = crate::object::new_rejected_promise(agg_obj);
                Ok(obj_into_val(p, &mut self.heap))
            }
            // Instance methods. Two cases:
            //  - settled promise: run the handler synchronously, wrap the
            //    result in a fresh fulfilled promise.
            //  - pending promise: append to the promise's then/catch chain
            //    and return a companion promise that settles when the chain
            //    runs (inside `resolve`/`reject` below).
            BuiltinFunction::PromiseThen => {
                let receiver = builtin.receiver.clone().ok_or_else(|| {
                    VMError::TypeError("Promise.then missing receiver".to_string())
                })?;
                let prom = match receiver {
                    Object::Promise(p) => p,
                    _ => {
                        return Err(VMError::TypeError(
                            "Promise.then called on non-promise".to_string(),
                        ))
                    }
                };
                let on_fulfilled = args.first().copied().unwrap_or(Value::UNDEFINED);
                let on_rejected = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let state = prom.borrow().settled.clone();
                match state {
                    PromiseState::Pending => {
                        // Register handlers + a companion promise for the
                        // caller to wait on.
                        let chained = crate::object::new_pending_promise();
                        let chained_val = obj_into_val(chained, &mut self.heap);
                        let mut p = unsafe { prom.borrow_mut() };
                        p.then_chain.push(on_fulfilled);
                        p.catch_chain.push(on_rejected);
                        p.chained.push(chained_val);
                        Ok(chained_val)
                    }
                    PromiseState::Fulfilled(v) => {
                        let val = obj_into_val((*v).clone(), &mut self.heap);
                        let result = if !on_fulfilled.is_undefined() && !on_fulfilled.is_null() {
                            self.call_value_slice(on_fulfilled, &[val])?
                        } else {
                            val
                        };
                        let result_obj = val_to_obj(result, &self.heap);
                        if matches!(result_obj, Object::Promise(_)) {
                            return Ok(obj_into_val(result_obj, &mut self.heap));
                        }
                        let p = crate::object::new_fulfilled_promise(result_obj);
                        Ok(obj_into_val(p, &mut self.heap))
                    }
                    PromiseState::Rejected(v) => {
                        let val = obj_into_val((*v).clone(), &mut self.heap);
                        if !on_rejected.is_undefined() && !on_rejected.is_null() {
                            let result = self.call_value_slice(on_rejected, &[val])?;
                            let p = crate::object::new_fulfilled_promise(
                                val_to_obj(result, &self.heap),
                            );
                            return Ok(obj_into_val(p, &mut self.heap));
                        }
                        // No reject handler — propagate.
                        let p = crate::object::new_rejected_promise((*v).clone());
                        Ok(obj_into_val(p, &mut self.heap))
                    }
                }
            }
            BuiltinFunction::PromiseCatch => {
                let receiver = builtin.receiver.clone().ok_or_else(|| {
                    VMError::TypeError("Promise.catch missing receiver".to_string())
                })?;
                let prom = match receiver {
                    Object::Promise(p) => p,
                    _ => {
                        return Err(VMError::TypeError(
                            "Promise.catch called on non-promise".to_string(),
                        ))
                    }
                };
                let on_rejected = args.first().copied().unwrap_or(Value::UNDEFINED);
                let state = prom.borrow().settled.clone();
                match state {
                    PromiseState::Pending => {
                        // Same as then(undefined, onRejected).
                        let chained = crate::object::new_pending_promise();
                        let chained_val = obj_into_val(chained, &mut self.heap);
                        let mut p = unsafe { prom.borrow_mut() };
                        p.then_chain.push(Value::UNDEFINED);
                        p.catch_chain.push(on_rejected);
                        p.chained.push(chained_val);
                        Ok(chained_val)
                    }
                    PromiseState::Fulfilled(v) => {
                        let p = crate::object::new_fulfilled_promise((*v).clone());
                        Ok(obj_into_val(p, &mut self.heap))
                    }
                    PromiseState::Rejected(v) => {
                        let val = obj_into_val((*v).clone(), &mut self.heap);
                        if !on_rejected.is_undefined() && !on_rejected.is_null() {
                            let result = self.call_value_slice(on_rejected, &[val])?;
                            let p = crate::object::new_fulfilled_promise(
                                val_to_obj(result, &self.heap),
                            );
                            return Ok(obj_into_val(p, &mut self.heap));
                        }
                        let p = crate::object::new_rejected_promise((*v).clone());
                        Ok(obj_into_val(p, &mut self.heap))
                    }
                }
            }
            BuiltinFunction::PromiseFinally => {
                let receiver = builtin.receiver.clone().ok_or_else(|| {
                    VMError::TypeError("Promise.finally missing receiver".to_string())
                })?;
                let prom = match receiver {
                    Object::Promise(p) => p,
                    _ => {
                        return Err(VMError::TypeError(
                            "Promise.finally called on non-promise".to_string(),
                        ))
                    }
                };
                if let Some(cb) = args.first().copied() {
                    if !cb.is_undefined() && !cb.is_null() {
                        let _ = self.call_value_slice(cb, &[])?;
                    }
                }
                // Pass-through: .finally returns a promise sharing the
                // original's settlement. Since both refer to the same
                // Rc cell, updates propagate.
                Ok(obj_into_val(Object::Promise(prom), &mut self.heap))
            }
            BuiltinFunction::ArrayPop => {
                let receiver = builtin
                    .receiver
                    .ok_or_else(|| VMError::TypeError("Array.pop missing receiver".to_string()))?;
                match receiver {
                    Object::Array(items_rc) => {
                        let borrowed = unsafe { items_rc.borrow_mut() };
                        Ok(borrowed.pop().unwrap_or(Value::UNDEFINED))
                    }
                    _ => Ok(Value::UNDEFINED),
                }
            }
            BuiltinFunction::ArrayPush => {
                let receiver = builtin
                    .receiver
                    .ok_or_else(|| VMError::TypeError("Array.push missing receiver".to_string()))?;
                match receiver {
                    Object::Array(items_rc) => {
                        let borrowed = unsafe { items_rc.borrow_mut() };
                        // Pre-check the combined size. The old code
                        // pushed all args first and only caught the
                        // overflow afterwards — a 2 M-arg push on a
                        // full array would commit ~16 MiB of Values
                        // before the check rejected the call.
                        if borrowed.len().saturating_add(args.len()) > MAX_ARRAY_SIZE {
                            return Err(VMError::TypeError(ERR_ARRAY_SIZE.to_string()));
                        }
                        for arg in args {
                            borrowed.push(*arg);
                        }
                        Ok(Value::from_i64(borrowed.len() as i64))
                    }
                    _ => Ok(Value::UNDEFINED),
                }
            }
            BuiltinFunction::ArrayAt => {
                let receiver = builtin
                    .receiver
                    .ok_or_else(|| VMError::TypeError("Array.at missing receiver".to_string()))?;
                let items = match receiver {
                    Object::Array(items) => unwrap_array(items),
                    _ => return Ok(Value::UNDEFINED),
                };
                let idx = args
                    .first()
                    .map(|v| self.to_i32_val(*v))
                    .transpose()?
                    .unwrap_or(0);
                let real = if idx < 0 {
                    items.len() as i32 + idx
                } else {
                    idx
                };
                if real < 0 || real as usize >= items.len() {
                    Ok(Value::UNDEFINED)
                } else {
                    Ok(items[real as usize])
                }
            }
            BuiltinFunction::ArrayToSorted => {
                let receiver = builtin.receiver.ok_or_else(|| {
                    VMError::TypeError("Array.toSorted missing receiver".to_string())
                })?;
                let mut items = match receiver {
                    Object::Array(items) => unwrap_array(items),
                    _ => return Ok(Value::UNDEFINED),
                };
                items.sort_by_key(|v| val_inspect(*v, &self.heap));
                Ok(obj_into_val(make_array(items), &mut self.heap))
            }
            BuiltinFunction::ArrayWith => {
                let receiver = builtin
                    .receiver
                    .ok_or_else(|| VMError::TypeError("Array.with missing receiver".to_string()))?;
                let mut items = match receiver {
                    Object::Array(items) => unwrap_array(items),
                    _ => return Ok(Value::UNDEFINED),
                };
                if args.len() < 2 {
                    return Ok(obj_into_val(make_array(items), &mut self.heap));
                }
                let idx = self.to_i32_val(args[0])?;
                if idx < 0 || idx as usize >= items.len() {
                    return Ok(obj_into_val(make_array(items), &mut self.heap));
                }
                items[idx as usize] = args[1];
                Ok(obj_into_val(make_array(items), &mut self.heap))
            }
            BuiltinFunction::ArrayMap => {
                let receiver = builtin
                    .receiver
                    .ok_or_else(|| VMError::TypeError("Array.map missing receiver".to_string()))?;
                // Borrow the array, clone items to a local Vec, then drop
                // the borrow so we don't hold it across callback re-entry.
                let items = match receiver {
                    Object::Array(ref rc) => rc.borrow().clone(),
                    _ => return Ok(Value::UNDEFINED),
                };
                let callback = args
                    .first()
                    .cloned()
                    .ok_or_else(|| VMError::TypeError("Array.map requires callback".to_string()))?;
                let source_for_cb = if Self::callback_max_used_args_val(callback, &self.heap) >= 3 {
                    Some(obj_into_val(make_array(items.clone()), &mut self.heap))
                } else {
                    None
                };
                let mut out: Vec<Value> = Vec::with_capacity(items.len());
                for (i, item) in items.into_iter().enumerate() {
                    self.check_builtin_callback_limits()?;
                    let mapped = if let Some(src) = source_for_cb {
                        self.call_value3(callback, item, Value::from_i64(i as i64), src)?
                    } else {
                        self.call_value2(callback, item, Value::from_i64(i as i64))?
                    };
                    out.push(mapped);
                }
                Ok(obj_into_val(make_array(out), &mut self.heap))
            }
            BuiltinFunction::ArrayForEach => {
                let receiver = builtin.receiver.ok_or_else(|| {
                    VMError::TypeError("Array.forEach missing receiver".to_string())
                })?;
                // Borrow the array, clone items to a local Vec, then drop
                // the borrow so we don't hold it across callback re-entry.
                let items = match receiver {
                    Object::Array(ref rc) => rc.borrow().clone(),
                    _ => return Ok(Value::UNDEFINED),
                };
                let callback = args.first().copied().ok_or_else(|| {
                    VMError::TypeError("Array.forEach requires callback".to_string())
                })?;
                let source_for_cb = if Self::callback_max_used_args_val(callback, &self.heap) >= 3 {
                    Some(obj_into_val(make_array(items.clone()), &mut self.heap))
                } else {
                    None
                };
                for (i, item) in items.into_iter().enumerate() {
                    self.check_builtin_callback_limits()?;
                    if let Some(src) = source_for_cb {
                        let _ = self.call_value3(callback, item, Value::from_i64(i as i64), src)?;
                    } else {
                        let _ = self.call_value2(callback, item, Value::from_i64(i as i64))?;
                    }
                }
                Ok(Value::UNDEFINED)
            }
            BuiltinFunction::ArrayFlatMap => {
                let receiver = builtin.receiver.ok_or_else(|| {
                    VMError::TypeError("Array.flatMap missing receiver".to_string())
                })?;
                let items = match receiver {
                    Object::Array(items) => unwrap_array(items),
                    _ => return Ok(obj_into_val(make_array(vec![]), &mut self.heap)),
                };
                let callback = args.first().copied().ok_or_else(|| {
                    VMError::TypeError("Array.flatMap requires callback".to_string())
                })?;
                let source_for_cb = if Self::callback_max_used_args_val(callback, &self.heap) >= 3 {
                    Some(obj_into_val(make_array(items.clone()), &mut self.heap))
                } else {
                    None
                };
                let mut out: Vec<Value> = vec![];
                for (i, item) in items.into_iter().enumerate() {
                    self.check_builtin_callback_limits()?;
                    let mapped = if let Some(src) = source_for_cb {
                        self.call_value3(callback, item, Value::from_i64(i as i64), src)?
                    } else {
                        self.call_value2(callback, item, Value::from_i64(i as i64))?
                    };
                    let mapped_obj = val_to_obj(mapped, &self.heap);
                    match mapped_obj {
                        Object::Array(inner) => out.extend(unwrap_array(inner)),
                        _ => out.push(mapped),
                    }
                }
                Ok(obj_into_val(make_array(out), &mut self.heap))
            }
            BuiltinFunction::ArrayFlat => {
                let receiver = builtin
                    .receiver
                    .ok_or_else(|| VMError::TypeError("Array.flat missing receiver".to_string()))?;
                let items = match receiver {
                    Object::Array(items) => unwrap_array(items),
                    _ => return Ok(obj_into_val(make_array(vec![]), &mut self.heap)),
                };
                // Clamp caller-supplied depth — `flatten()` recurses
                // linearly with it, so `.flat(Number.MAX_SAFE_INTEGER)`
                // is a free stack-overflow primitive. 64 is deeper
                // than any real-world nesting and well below the
                // Windows 1 MiB stack budget even with large frames.
                const MAX_FLAT_DEPTH: i32 = 64;
                let depth = args
                    .first()
                    .map(|v| self.to_i32_val(*v))
                    .transpose()?
                    .unwrap_or(1)
                    .max(0)
                    .min(MAX_FLAT_DEPTH);

                fn flatten(
                    items: Vec<Value>,
                    depth: i32,
                    heap: &Heap,
                    out_cap: &mut usize,
                ) -> Result<Vec<Value>, VMError> {
                    if depth == 0 {
                        return Ok(items);
                    }
                    let mut out = vec![];
                    for item in items {
                        let obj = val_to_obj(item, heap);
                        match obj {
                            Object::Array(inner) => {
                                let nested = flatten(unwrap_array(inner), depth - 1, heap, out_cap)?;
                                if out.len().saturating_add(nested.len()) > *out_cap {
                                    return Err(VMError::TypeError(
                                        crate::vm::ERR_ARRAY_SIZE.to_string(),
                                    ));
                                }
                                out.extend(nested);
                            }
                            _ => {
                                if out.len() >= *out_cap {
                                    return Err(VMError::TypeError(
                                        crate::vm::ERR_ARRAY_SIZE.to_string(),
                                    ));
                                }
                                out.push(item);
                            }
                        }
                    }
                    Ok(out)
                }

                let mut cap = MAX_ARRAY_SIZE;
                let flat = flatten(items, depth, &self.heap, &mut cap)?;
                Ok(obj_into_val(make_array(flat), &mut self.heap))
            }
            BuiltinFunction::ArrayReverse => {
                let receiver = builtin.receiver.ok_or_else(|| {
                    VMError::TypeError("Array.reverse missing receiver".to_string())
                })?;
                match receiver {
                    Object::Array(items_rc) => {
                        unsafe { items_rc.borrow_mut() }.reverse();
                        Ok(obj_into_val(Object::Array(items_rc), &mut self.heap))
                    }
                    _ => Ok(obj_into_val(make_array(vec![]), &mut self.heap)),
                }
            }
            BuiltinFunction::ArraySort => {
                let receiver = builtin
                    .receiver
                    .ok_or_else(|| VMError::TypeError("Array.sort missing receiver".to_string()))?;
                let mut items = match receiver {
                    Object::Array(items) => unwrap_array(items),
                    _ => return Ok(obj_into_val(make_array(vec![]), &mut self.heap)),
                };

                if let Some(compare_fn) = args.first().copied() {
                    // Aborting `sort_by` is fiddly because the closure
                    // can't return a `Result`. Park the first error in a
                    // captured `Option` and short-circuit every
                    // subsequent comparison with `Ordering::Equal` — the
                    // script-facing error is re-raised below.
                    let mut early_err: Option<VMError> = None;
                    items.sort_by(|a, b| {
                        if early_err.is_some() {
                            return std::cmp::Ordering::Equal;
                        }
                        if let Err(e) = self.check_builtin_callback_limits() {
                            early_err = Some(e);
                            return std::cmp::Ordering::Equal;
                        }
                        let out = self.call_value2(compare_fn, *a, *b);
                        match out {
                            Ok(v) => {
                                let n = self.to_number_val(v).unwrap_or(0.0);
                                if n < 0.0 {
                                    std::cmp::Ordering::Less
                                } else if n > 0.0 {
                                    std::cmp::Ordering::Greater
                                } else {
                                    std::cmp::Ordering::Equal
                                }
                            }
                            Err(e) => {
                                early_err = Some(e);
                                std::cmp::Ordering::Equal
                            }
                        }
                    });
                    if let Some(e) = early_err {
                        return Err(e);
                    }
                } else {
                    items.sort_by_key(|v| val_inspect(*v, &self.heap));
                }

                Ok(obj_into_val(make_array(items), &mut self.heap))
            }
            BuiltinFunction::ArrayFilter => {
                let receiver = builtin.receiver.ok_or_else(|| {
                    VMError::TypeError("Array.filter missing receiver".to_string())
                })?;
                // Borrow the array, clone items to a local Vec, then drop
                // the borrow so we don't hold it across callback re-entry.
                let items = match receiver {
                    Object::Array(ref rc) => rc.borrow().clone(),
                    _ => return Ok(obj_into_val(make_array(vec![]), &mut self.heap)),
                };
                let callback = args.first().copied().ok_or_else(|| {
                    VMError::TypeError("Array.filter requires callback".to_string())
                })?;
                let source_for_cb = if Self::callback_max_used_args_val(callback, &self.heap) >= 3 {
                    Some(obj_into_val(make_array(items.clone()), &mut self.heap))
                } else {
                    None
                };
                let mut out: Vec<Value> = vec![];
                for (i, item) in items.into_iter().enumerate() {
                    self.check_builtin_callback_limits()?;
                    let keep = if let Some(ref src) = source_for_cb {
                        self.call_value3(callback, item, Value::from_i64(i as i64), *src)?
                    } else {
                        self.call_value2(callback, item, Value::from_i64(i as i64))?
                    };
                    if self.is_truthy(&val_to_obj(keep, &self.heap)) {
                        out.push(item);
                    }
                }
                Ok(obj_into_val(make_array(out), &mut self.heap))
            }
            BuiltinFunction::ArraySome => {
                let receiver = builtin
                    .receiver
                    .ok_or_else(|| VMError::TypeError("Array.some missing receiver".to_string()))?;
                let items = match receiver {
                    Object::Array(items) => unwrap_array(items),
                    _ => return Ok(Value::from_bool(false)),
                };
                let callback = args.first().copied().ok_or_else(|| {
                    VMError::TypeError("Array.some requires callback".to_string())
                })?;
                let source_for_cb = if Self::callback_max_used_args_val(callback, &self.heap) >= 3 {
                    Some(obj_into_val(make_array(items.clone()), &mut self.heap))
                } else {
                    None
                };
                for (i, item) in items.into_iter().enumerate() {
                    self.check_builtin_callback_limits()?;
                    let ok = if let Some(ref src) = source_for_cb {
                        self.call_value3(callback, item, Value::from_i64(i as i64), *src)?
                    } else {
                        self.call_value2(callback, item, Value::from_i64(i as i64))?
                    };
                    if self.is_truthy(&val_to_obj(ok, &self.heap)) {
                        return Ok(Value::from_bool(true));
                    }
                }
                Ok(Value::from_bool(false))
            }
            BuiltinFunction::ArrayEvery => {
                let receiver = builtin.receiver.ok_or_else(|| {
                    VMError::TypeError("Array.every missing receiver".to_string())
                })?;
                let items = match receiver {
                    Object::Array(items) => unwrap_array(items),
                    _ => return Ok(Value::from_bool(false)),
                };
                let callback = args.first().copied().ok_or_else(|| {
                    VMError::TypeError("Array.every requires callback".to_string())
                })?;
                let source_for_cb = if Self::callback_max_used_args_val(callback, &self.heap) >= 3 {
                    Some(obj_into_val(make_array(items.clone()), &mut self.heap))
                } else {
                    None
                };
                for (i, item) in items.into_iter().enumerate() {
                    self.check_builtin_callback_limits()?;
                    let ok = if let Some(ref src) = source_for_cb {
                        self.call_value3(callback, item, Value::from_i64(i as i64), *src)?
                    } else {
                        self.call_value2(callback, item, Value::from_i64(i as i64))?
                    };
                    if !self.is_truthy(&val_to_obj(ok, &self.heap)) {
                        return Ok(Value::from_bool(false));
                    }
                }
                Ok(Value::from_bool(true))
            }
            BuiltinFunction::ArrayFind => {
                let receiver = builtin
                    .receiver
                    .ok_or_else(|| VMError::TypeError("Array.find missing receiver".to_string()))?;
                let items = match receiver {
                    Object::Array(items) => unwrap_array(items),
                    _ => return Ok(Value::UNDEFINED),
                };
                let callback = args.first().copied().ok_or_else(|| {
                    VMError::TypeError("Array.find requires callback".to_string())
                })?;
                let source_for_cb = if Self::callback_max_used_args_val(callback, &self.heap) >= 3 {
                    Some(obj_into_val(make_array(items.clone()), &mut self.heap))
                } else {
                    None
                };
                for (i, item) in items.into_iter().enumerate() {
                    self.check_builtin_callback_limits()?;
                    let found = if let Some(ref src) = source_for_cb {
                        self.call_value3(callback, item, Value::from_i64(i as i64), *src)?
                    } else {
                        self.call_value2(callback, item, Value::from_i64(i as i64))?
                    };
                    if self.is_truthy(&val_to_obj(found, &self.heap)) {
                        return Ok(item);
                    }
                }
                Ok(Value::UNDEFINED)
            }
            BuiltinFunction::ArrayFindIndex => {
                let receiver = builtin.receiver.ok_or_else(|| {
                    VMError::TypeError("Array.findIndex missing receiver".to_string())
                })?;
                let items = match receiver {
                    Object::Array(items) => unwrap_array(items),
                    _ => return Ok(Value::from_i64(-1)),
                };
                let callback = args.first().copied().ok_or_else(|| {
                    VMError::TypeError("Array.findIndex requires callback".to_string())
                })?;
                let source_for_cb = if Self::callback_max_used_args_val(callback, &self.heap) >= 3 {
                    Some(obj_into_val(make_array(items.clone()), &mut self.heap))
                } else {
                    None
                };
                for (i, item) in items.into_iter().enumerate() {
                    self.check_builtin_callback_limits()?;
                    let found = if let Some(ref src) = source_for_cb {
                        self.call_value3(callback, item, Value::from_i64(i as i64), *src)?
                    } else {
                        self.call_value2(callback, item, Value::from_i64(i as i64))?
                    };
                    if self.is_truthy(&val_to_obj(found, &self.heap)) {
                        return Ok(Value::from_i64(i as i64));
                    }
                }
                Ok(Value::from_i64(-1))
            }
            BuiltinFunction::ArrayFindLast => {
                let receiver = builtin.receiver.ok_or_else(|| {
                    VMError::TypeError("Array.findLast missing receiver".to_string())
                })?;
                let items = match receiver {
                    Object::Array(items) => unwrap_array(items),
                    _ => return Ok(Value::UNDEFINED),
                };
                let callback = args.first().copied().ok_or_else(|| {
                    VMError::TypeError("Array.findLast requires callback".to_string())
                })?;
                for i in (0..items.len()).rev() {
                    self.check_builtin_callback_limits()?;
                    let item = items[i];
                    let found = self.call_value2(callback, item, Value::from_i64(i as i64))?;
                    if self.is_truthy(&val_to_obj(found, &self.heap)) {
                        return Ok(item);
                    }
                }
                Ok(Value::UNDEFINED)
            }
            BuiltinFunction::ArrayFindLastIndex => {
                let receiver = builtin.receiver.ok_or_else(|| {
                    VMError::TypeError("Array.findLastIndex missing receiver".to_string())
                })?;
                let items = match receiver {
                    Object::Array(items) => unwrap_array(items),
                    _ => return Ok(Value::from_i64(-1)),
                };
                let callback = args.first().copied().ok_or_else(|| {
                    VMError::TypeError("Array.findLastIndex requires callback".to_string())
                })?;
                for i in (0..items.len()).rev() {
                    self.check_builtin_callback_limits()?;
                    let item = items[i];
                    let found = self.call_value2(callback, item, Value::from_i64(i as i64))?;
                    if self.is_truthy(&val_to_obj(found, &self.heap)) {
                        return Ok(Value::from_i64(i as i64));
                    }
                }
                Ok(Value::from_i64(-1))
            }
            BuiltinFunction::ArrayToReversed => {
                let receiver = builtin.receiver.ok_or_else(|| {
                    VMError::TypeError("Array.toReversed missing receiver".to_string())
                })?;
                let items = match receiver {
                    Object::Array(items) => unwrap_array(items),
                    _ => return Ok(Value::UNDEFINED),
                };
                let mut reversed = items;
                reversed.reverse();
                Ok(obj_into_val(make_array(reversed), &mut self.heap))
            }
            BuiltinFunction::ArrayIncludes => {
                let receiver = builtin.receiver.ok_or_else(|| {
                    VMError::TypeError("Array.includes missing receiver".to_string())
                })?;
                let items = match receiver {
                    Object::Array(items) => unwrap_array(items),
                    _ => return Ok(Value::from_bool(false)),
                };
                let needle = args.first().copied().unwrap_or(Value::UNDEFINED);
                let needle_obj = val_to_obj(needle, &self.heap);
                let has = items.iter().any(|item| {
                    let item_obj = val_to_obj(*item, &self.heap);
                    Self::same_value_zero(&item_obj, &needle_obj)
                });
                Ok(Value::from_bool(has))
            }
            BuiltinFunction::ArrayIndexOf => {
                let receiver = builtin.receiver.ok_or_else(|| {
                    VMError::TypeError("Array.indexOf missing receiver".to_string())
                })?;
                let items = match receiver {
                    Object::Array(items) => unwrap_array(items),
                    _ => return Ok(Value::from_i64(-1)),
                };
                let needle = args.first().copied().unwrap_or(Value::UNDEFINED);
                let needle_obj = val_to_obj(needle, &self.heap);
                let mut from = args
                    .get(1)
                    .map(|v| self.to_i32_val(*v))
                    .transpose()?
                    .unwrap_or(0);
                if from < 0 {
                    from = (items.len() as i32 + from).max(0);
                }
                for (i, item) in items.iter().enumerate().skip(from as usize) {
                    let item_obj = val_to_obj(*item, &self.heap);
                    if Self::strict_equal(&item_obj, &needle_obj) {
                        return Ok(Value::from_i64(i as i64));
                    }
                }
                Ok(Value::from_i64(-1))
            }
            BuiltinFunction::ArrayLastIndexOf => {
                let receiver = builtin.receiver.ok_or_else(|| {
                    VMError::TypeError("Array.lastIndexOf missing receiver".to_string())
                })?;
                let items = match receiver {
                    Object::Array(items) => unwrap_array(items),
                    _ => return Ok(Value::from_i64(-1)),
                };
                let needle = args.first().copied().unwrap_or(Value::UNDEFINED);
                let needle_obj = val_to_obj(needle, &self.heap);
                let mut from = args
                    .get(1)
                    .map(|v| self.to_i32_val(*v))
                    .transpose()?
                    .unwrap_or(items.len() as i32 - 1);
                if from < 0 {
                    from += items.len() as i32;
                }
                if items.is_empty() {
                    return Ok(Value::from_i64(-1));
                }
                let from = from.clamp(0, items.len() as i32 - 1) as usize;
                for i in (0..=from).rev() {
                    let item_obj = val_to_obj(items[i], &self.heap);
                    if Self::strict_equal(&item_obj, &needle_obj) {
                        return Ok(Value::from_i64(i as i64));
                    }
                }
                Ok(Value::from_i64(-1))
            }
            BuiltinFunction::ArrayJoin => {
                let receiver = builtin
                    .receiver
                    .ok_or_else(|| VMError::TypeError("Array.join missing receiver".to_string()))?;
                let items = match receiver {
                    Object::Array(items) => unwrap_array(items),
                    _ => return Ok(obj_into_val(Object::String(Rc::from("")), &mut self.heap)),
                };
                let sep = args
                    .first()
                    .map(|v| val_inspect(*v, &self.heap))
                    .unwrap_or_else(|| ",".to_string());
                // Incremental join with MAX_STRING_LENGTH cap. Produces
                // a clear TypeError before the allocator thrashes on
                // an `Array(1_000_000).fill("x".repeat(100)).join("")`
                // (~100 MB) or worse.
                let mut out = String::new();
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(&sep);
                    }
                    let obj = val_to_obj(*v, &self.heap);
                    let piece = match &obj {
                        Object::Undefined | Object::Null => String::new(),
                        Object::Array(nested) => {
                            self.array_to_js_string(nested.borrow())
                        }
                        Object::Hash(_) => "[object Object]".to_string(),
                        _ => obj.inspect(),
                    };
                    out.push_str(&piece);
                    if out.len() > crate::vm::MAX_STRING_LENGTH {
                        return Err(VMError::TypeError(
                            crate::vm::ERR_STRING_LEN.to_string(),
                        ));
                    }
                }
                Ok(obj_into_val(
                    Object::String(out.into()),
                    &mut self.heap,
                ))
            }
            BuiltinFunction::ArrayToString => {
                let receiver = builtin.receiver.ok_or_else(|| {
                    VMError::TypeError("Array.toString missing receiver".to_string())
                })?;
                let items = match receiver {
                    Object::Array(items) => unwrap_array(items),
                    _ => return Ok(obj_into_val(Object::String(Rc::from("")), &mut self.heap)),
                };
                Ok(obj_into_val(
                    Object::String(self.array_to_js_string(&items[..]).into()),
                    &mut self.heap,
                ))
            }
            BuiltinFunction::ArrayValueOf => {
                let receiver = builtin.receiver.ok_or_else(|| {
                    VMError::TypeError("Array.valueOf missing receiver".to_string())
                })?;
                match receiver {
                    Object::Array(items) => Ok(obj_into_val(Object::Array(items), &mut self.heap)),
                    _ => Ok(Value::UNDEFINED),
                }
            }
            BuiltinFunction::ArraySlice => {
                let receiver = builtin.receiver.ok_or_else(|| {
                    VMError::TypeError("Array.slice missing receiver".to_string())
                })?;
                let items = match receiver {
                    Object::Array(items) => unwrap_array(items),
                    _ => return Ok(obj_into_val(make_array(vec![]), &mut self.heap)),
                };
                let len = items.len() as i32;
                let start = args
                    .first()
                    .map(|v| self.to_i32_val(*v))
                    .transpose()?
                    .unwrap_or(0);
                let end = args
                    .get(1)
                    .map(|v| self.to_i32_val(*v))
                    .transpose()?
                    .unwrap_or(len);
                let (sidx, eidx) = Self::slice_bounds(start, end, len);
                Ok(obj_into_val(
                    make_array(items[sidx as usize..eidx as usize].to_vec()),
                    &mut self.heap,
                ))
            }
            BuiltinFunction::ArrayReduce => {
                let receiver = builtin.receiver.ok_or_else(|| {
                    VMError::TypeError("Array.reduce missing receiver".to_string())
                })?;
                let items = match receiver {
                    Object::Array(items) => unwrap_array(items),
                    _ => return Ok(Value::UNDEFINED),
                };
                let callback = args.first().copied().ok_or_else(|| {
                    VMError::TypeError("Array.reduce requires callback".to_string())
                })?;
                let source_for_cb = if Self::callback_max_used_args_val(callback, &self.heap) >= 4 {
                    Some(obj_into_val(make_array(items.clone()), &mut self.heap))
                } else {
                    None
                };

                if items.is_empty() && args.get(1).is_none() {
                    return Err(VMError::TypeError(
                        "Reduce of empty array with no initial value".to_string(),
                    ));
                }

                let mut idx = 0usize;
                let mut acc: Value = if let Some(init) = args.get(1) {
                    *init
                } else {
                    idx = 1;
                    items[0]
                };

                while idx < items.len() {
                    self.check_builtin_callback_limits()?;
                    acc = if let Some(src) = source_for_cb {
                        self.call_value4(
                            callback,
                            acc,
                            items[idx],
                            Value::from_i64(idx as i64),
                            src,
                        )?
                    } else {
                        self.call_value3(callback, acc, items[idx], Value::from_i64(idx as i64))?
                    };
                    idx += 1;
                }

                Ok(acc)
            }
            BuiltinFunction::ArrayReduceRight => {
                let receiver = builtin.receiver.ok_or_else(|| {
                    VMError::TypeError("Array.reduceRight missing receiver".to_string())
                })?;
                let items = match receiver {
                    Object::Array(items) => unwrap_array(items),
                    _ => return Ok(Value::UNDEFINED),
                };
                let callback = args.first().copied().ok_or_else(|| {
                    VMError::TypeError("Array.reduceRight requires callback".to_string())
                })?;

                if items.is_empty() && args.get(1).is_none() {
                    return Err(VMError::TypeError(
                        "Reduce of empty array with no initial value".to_string(),
                    ));
                }

                let len = items.len();
                let mut idx = len.wrapping_sub(1);
                let mut acc: Value = if let Some(init) = args.get(1) {
                    *init
                } else {
                    idx = len.wrapping_sub(2);
                    items[len - 1]
                };

                loop {
                    if idx >= len {
                        break;
                    }
                    self.check_builtin_callback_limits()?;
                    acc =
                        self.call_value3(callback, acc, items[idx], Value::from_i64(idx as i64))?;
                    idx = idx.wrapping_sub(1);
                }

                Ok(acc)
            }
            BuiltinFunction::ArrayFrom => {
                let source_val = args.first().copied().unwrap_or(Value::UNDEFINED);
                let source = val_to_obj(source_val, &self.heap);
                let mut out: Vec<Object> = match source {
                    Object::Array(items) => unwrap_array(items)
                        .into_iter()
                        .map(|v| val_to_obj(v, &self.heap))
                        .collect(),
                    Object::String(s) => s
                        .chars()
                        .map(|c| Object::String(c.to_string().into()))
                        .collect(),
                    Object::Hash(hash) => {
                        // Check for Symbol.iterator protocol (@@sym:1)
                        let iter_fn_opt = {
                            let hash_b = unsafe { hash.borrow_mut() };
                            hash_b.sync_pairs_if_dirty();
                            let sym_iter_key = HashKey::Other(Rc::from("@@sym:1"));
                            hash_b.pairs.get(&sym_iter_key).copied()
                        };
                        if let Some(iter_fn_val) = iter_fn_opt {
                            // Call [Symbol.iterator]() to get the iterator object
                            let iterator_val =
                                self.call_value_slice(iter_fn_val, &[source_val])?;
                            // Iterate: call .next() until done. Every
                            // iteration is a host-to-script call, so
                            // the wall-time + abort-flag re-check
                            // needs to fire or a hostile iterator
                            // that just keeps returning `{done:false}`
                            // runs up to MAX_ARRAY_SIZE=1M callbacks
                            // before catching.
                            let mut items = Vec::new();
                            loop {
                                self.check_builtin_callback_limits()?;
                                let next_sym = crate::intern::intern("next");
                                let next_fn =
                                    self.get_property_val(iterator_val, next_sym, 0)?;
                                let result =
                                    self.call_value_slice(next_fn, &[iterator_val])?;
                                let result_obj = val_to_obj(result, &self.heap);
                                match result_obj {
                                    Object::Hash(h) => {
                                        let hb = h.borrow();
                                        let done = hb
                                            .get_by_str("done")
                                            .map(|v| {
                                                let obj = val_to_obj(v, &self.heap);
                                                self.is_truthy(&obj)
                                            })
                                            .unwrap_or(false);
                                        if done {
                                            break;
                                        }
                                        let value = hb
                                            .get_by_str("value")
                                            .map(|v| val_to_obj(v, &self.heap))
                                            .unwrap_or_else(undefined_object);
                                        items.push(value);
                                    }
                                    _ => break,
                                }
                                if items.len() > MAX_ARRAY_SIZE {
                                    return Err(VMError::TypeError(
                                        "Array.from: iterator too large"
                                            .to_string(),
                                    ));
                                }
                            }
                            items
                        } else {
                            let hash_b = unsafe { hash.borrow_mut() };
                            if let Some(length_val) = hash_b.get_by_str("length") {
                                let length_obj = val_to_obj(length_val, &self.heap);
                                let requested = self.to_u32(&length_obj).unwrap_or(0) as usize;
                                // `{length: 2**32}` would otherwise ask for
                                // a 32 GiB `Vec::with_capacity` and OOM the
                                // host before a single push ran. Cap the
                                // requested length at `MAX_ARRAY_SIZE` and
                                // fail cleanly above it — matches the limit
                                // used for real arrays elsewhere.
                                if requested > MAX_ARRAY_SIZE {
                                    return Err(VMError::TypeError(format!(
                                        "Array.from: length {} exceeds MAX_ARRAY_SIZE ({})",
                                        requested, MAX_ARRAY_SIZE
                                    )));
                                }
                                let len = requested;
                                let mut arr = Vec::with_capacity(len);
                                for i in 0..len {
                                    let key = HashKey::from_string(&i.to_string());
                                    arr.push(
                                        hash_b
                                            .pairs
                                            .get(&key)
                                            .map(|v| val_to_obj(*v, &self.heap))
                                            .unwrap_or_else(undefined_object),
                                    );
                                }
                                arr
                            } else {
                                vec![]
                            }
                        }
                    }
                    Object::Set(set_obj) => set_obj
                        .entries
                        .borrow()
                        .iter()
                        .map(|k| self.object_from_hash_key(k))
                        .collect(),
                    Object::Map(map_obj) => map_obj
                        .entries
                        .borrow()
                        .iter()
                        .map(|(k, v)| {
                            let key_val =
                                obj_into_val(self.object_from_hash_key(k), &mut self.heap);
                            make_array(vec![key_val, *v])
                        })
                        .collect(),
                    Object::Generator(gen_rc) => {
                        // Iterate generator by calling .next() until done.
                        // An infinite generator yielding inline i32s
                        // (zero heap alloc per yield) would otherwise
                        // grow `items` unboundedly — the MAX_ARRAY_SIZE
                        // cap matches the iterator-protocol branch above.
                        let mut items = Vec::new();
                        loop {
                            let result = self.execute_generator_next(&gen_rc, Value::UNDEFINED)?;
                            let result_obj = val_to_obj(result, &self.heap);
                            match result_obj {
                                Object::Hash(h) => {
                                    let hb = h.borrow();
                                    let done = hb.get_by_str("done")
                                        .map(|v| {
                                            let obj = val_to_obj(v, &self.heap);
                                            self.is_truthy(&obj)
                                        })
                                        .unwrap_or(false);
                                    if done {
                                        break;
                                    }
                                    let value = hb.get_by_str("value")
                                        .map(|v| val_to_obj(v, &self.heap))
                                        .unwrap_or_else(undefined_object);
                                    if items.len() >= MAX_ARRAY_SIZE {
                                        return Err(VMError::TypeError(
                                            "Array.from: generator yielded more than MAX_ARRAY_SIZE values".to_string(),
                                        ));
                                    }
                                    items.push(value);
                                }
                                _ => break,
                            }
                        }
                        items
                    }
                    _ => vec![],
                };

                if let Some(callback) = args.get(1) {
                    let cb = *callback;
                    let source_arr_vals: Vec<Value> =
                        out.iter().map(|o| obj_to_val(o, &mut self.heap)).collect();
                    let source_arr = obj_into_val(make_array(source_arr_vals), &mut self.heap);
                    let mut mapped = Vec::with_capacity(out.len());
                    for (i, item) in out.into_iter().enumerate() {
                        self.check_builtin_callback_limits()?;
                        let item_val = obj_into_val(item, &mut self.heap);
                        let value =
                            self.call_value3(cb, item_val, Value::from_i64(i as i64), source_arr)?;
                        mapped.push(val_to_obj(value, &self.heap));
                    }
                    out = mapped;
                }

                let result: Vec<Value> = out
                    .into_iter()
                    .map(|o| obj_into_val(o, &mut self.heap))
                    .collect();
                Ok(obj_into_val(make_array(result), &mut self.heap))
            }
            BuiltinFunction::ArrayIsArray => {
                let val = args.first().copied().unwrap_or(Value::UNDEFINED);
                let is_arr = if val.is_heap() {
                    matches!(
                        self.heap.objects.get(val.heap_index() as usize),
                        Some(Object::Array(_))
                    )
                } else {
                    false
                };
                Ok(Value::from_bool(is_arr))
            }
            BuiltinFunction::ArrayFill => {
                let receiver = builtin
                    .receiver
                    .ok_or_else(|| VMError::TypeError("fill requires receiver".to_string()))?;
                match receiver {
                    Object::Array(items_rc) => {
                        let fill_val = args.first().copied().unwrap_or(Value::UNDEFINED);
                        let borrowed = unsafe { items_rc.borrow_mut() };
                        let len = borrowed.len() as i64;
                        let start = if let Some(v) = args.get(1) {
                            let s = self.to_i32_val(*v)? as i64;
                            if s < 0 { (len + s).max(0) as usize } else { (s as usize).min(len as usize) }
                        } else { 0 };
                        let end = if let Some(v) = args.get(2) {
                            let e = self.to_i32_val(*v)? as i64;
                            if e < 0 { (len + e).max(0) as usize } else { (e as usize).min(len as usize) }
                        } else { len as usize };
                        borrowed[start..end].fill(fill_val);
                        let _ = borrowed;
                        Ok(obj_into_val(Object::Array(items_rc), &mut self.heap))
                    }
                    _ => Err(VMError::TypeError("fill called on non-array".to_string())),
                }
            }
            BuiltinFunction::ArrayCopyWithin => {
                let receiver = builtin
                    .receiver
                    .ok_or_else(|| VMError::TypeError("copyWithin requires receiver".to_string()))?;
                let items = match receiver {
                    Object::Array(items) => unwrap_array(items),
                    _ => {
                        return Err(VMError::TypeError(
                            "copyWithin called on non-array".to_string(),
                        ))
                    }
                };
                let len = items.len() as i64;
                let target = {
                    let t = self.to_i32_val(args.first().copied().unwrap_or(Value::UNDEFINED))? as i64;
                    if t < 0 { (len + t).max(0) as usize } else { (t as usize).min(len as usize) }
                };
                let start = if let Some(v) = args.get(1) {
                    let s = self.to_i32_val(*v)? as i64;
                    if s < 0 { (len + s).max(0) as usize } else { (s as usize).min(len as usize) }
                } else {
                    0
                };
                let end = if let Some(v) = args.get(2) {
                    let e = self.to_i32_val(*v)? as i64;
                    if e < 0 { (len + e).max(0) as usize } else { (e as usize).min(len as usize) }
                } else {
                    len as usize
                };
                // Copy elements from [start..end) to [target..)
                let count = (end - start).min(len as usize - target);
                let mut new_items = items;
                // Use a temporary buffer to handle overlapping regions
                let source: Vec<Value> = new_items[start..start + count].to_vec();
                for (i, val) in source.into_iter().enumerate() {
                    new_items[target + i] = val;
                }
                Ok(obj_into_val(make_array(new_items), &mut self.heap))
            }
            BuiltinFunction::ArrayKeys => {
                let receiver = builtin
                    .receiver
                    .ok_or_else(|| VMError::TypeError("Array.keys missing receiver".to_string()))?;
                let items = match receiver {
                    Object::Array(items) => unwrap_array(items),
                    _ => return Ok(obj_into_val(make_array(vec![]), &mut self.heap)),
                };
                let out: Vec<Value> = (0..items.len())
                    .map(|i| Value::from_i64(i as i64))
                    .collect();
                Ok(obj_into_val(make_array(out), &mut self.heap))
            }
            BuiltinFunction::ArrayValues => {
                let receiver = builtin.receiver.ok_or_else(|| {
                    VMError::TypeError("Array.values missing receiver".to_string())
                })?;
                let items = match receiver {
                    Object::Array(items) => items,
                    _ => return Ok(obj_into_val(make_array(vec![]), &mut self.heap)),
                };
                Ok(obj_into_val(Object::Array(items), &mut self.heap))
            }
            BuiltinFunction::ArrayEntries => {
                let receiver = builtin.receiver.ok_or_else(|| {
                    VMError::TypeError("Array.entries missing receiver".to_string())
                })?;
                let items = match receiver {
                    Object::Array(items) => unwrap_array(items),
                    _ => return Ok(obj_into_val(make_array(vec![]), &mut self.heap)),
                };
                let out: Vec<Value> = items
                    .into_iter()
                    .enumerate()
                    .map(|(i, v)| {
                        let entry = make_array(vec![Value::from_i64(i as i64), v]);
                        obj_into_val(entry, &mut self.heap)
                    })
                    .collect();
                Ok(obj_into_val(make_array(out), &mut self.heap))
            }
            BuiltinFunction::ArrayShift => {
                let receiver = builtin.receiver.ok_or_else(|| {
                    VMError::TypeError("Array.shift missing receiver".to_string())
                })?;
                match receiver {
                    Object::Array(items_rc) => {
                        let borrowed = unsafe { items_rc.borrow_mut() };
                        if borrowed.is_empty() {
                            Ok(Value::UNDEFINED)
                        } else {
                            Ok(borrowed.remove(0))
                        }
                    }
                    _ => Ok(Value::UNDEFINED),
                }
            }
            BuiltinFunction::ArrayUnshift => {
                let receiver = builtin.receiver.ok_or_else(|| {
                    VMError::TypeError("Array.unshift missing receiver".to_string())
                })?;
                match receiver {
                    Object::Array(items_rc) => {
                        let borrowed = unsafe { items_rc.borrow_mut() };
                        if borrowed.len().saturating_add(args.len()) > MAX_ARRAY_SIZE {
                            return Err(VMError::TypeError(ERR_ARRAY_SIZE.to_string()));
                        }
                        for (i, arg) in args.iter().enumerate() {
                            borrowed.insert(i, *arg);
                        }
                        Ok(Value::from_i64(borrowed.len() as i64))
                    }
                    _ => Ok(Value::UNDEFINED),
                }
            }
            BuiltinFunction::ArraySplice => {
                let receiver = builtin.receiver.ok_or_else(|| {
                    VMError::TypeError("Array.splice missing receiver".to_string())
                })?;
                match receiver {
                    Object::Array(items_rc) => {
                        let borrowed = unsafe { items_rc.borrow_mut() };
                        let len = borrowed.len() as i64;
                        let start_raw = args
                            .first()
                            .map(|v| self.to_i32_val(*v))
                            .transpose()?
                            .unwrap_or(0) as i64;
                        let start = if start_raw < 0 {
                            (len + start_raw).max(0) as usize
                        } else {
                            (start_raw as usize).min(borrowed.len())
                        };
                        let delete_count = if args.len() >= 2 {
                            self.to_i32_val(args[1])?.max(0) as usize
                        } else {
                            borrowed.len() - start
                        };
                        let delete_count = delete_count.min(borrowed.len() - start);
                        let removed: Vec<Value> = borrowed.drain(start..start + delete_count).collect();
                        let insert_count = args.len().saturating_sub(2);
                        if borrowed.len().saturating_add(insert_count) > MAX_ARRAY_SIZE {
                            return Err(VMError::TypeError(ERR_ARRAY_SIZE.to_string()));
                        }
                        for (i, arg) in args[2..].iter().enumerate() {
                            borrowed.insert(start + i, *arg);
                        }
                        Ok(obj_into_val(make_array(removed), &mut self.heap))
                    }
                    _ => Ok(Value::UNDEFINED),
                }
            }
            BuiltinFunction::ArrayConcat => {
                let receiver = builtin.receiver.ok_or_else(|| {
                    VMError::TypeError("Array.concat missing receiver".to_string())
                })?;
                let mut result = match receiver {
                    Object::Array(items) => unwrap_array(items),
                    _ => return Ok(Value::UNDEFINED),
                };
                for arg in args {
                    let obj = val_to_obj(*arg, &self.heap);
                    match obj {
                        Object::Array(items) => {
                            let extra = unwrap_array(items);
                            if result.len().saturating_add(extra.len()) > MAX_ARRAY_SIZE {
                                return Err(VMError::TypeError(
                                    crate::vm::ERR_ARRAY_SIZE.to_string(),
                                ));
                            }
                            result.extend(extra);
                        }
                        _ => {
                            if result.len() >= MAX_ARRAY_SIZE {
                                return Err(VMError::TypeError(
                                    crate::vm::ERR_ARRAY_SIZE.to_string(),
                                ));
                            }
                            result.push(*arg);
                        }
                    }
                }
                Ok(obj_into_val(make_array(result), &mut self.heap))
            }
            BuiltinFunction::RegExpCtor => {
                let arg0 = args.first().map(|v| val_to_obj(*v, &self.heap));
                let (pattern, inferred_flags) = match arg0.as_ref() {
                    Some(Object::RegExp(re)) => (re.pattern.clone(), re.flags.clone()),
                    Some(v) => (v.inspect(), String::new()),
                    None => (String::new(), String::new()),
                };
                let flags = args
                    .get(1)
                    .map(|v| val_inspect(*v, &self.heap))
                    .unwrap_or(inferred_flags);
                let regexp =
                    Object::RegExp(Box::new(crate::object::RegExpObject { pattern, flags }));
                Ok(obj_into_val(regexp, &mut self.heap))
            }
            BuiltinFunction::RegExpTest => {
                let receiver = builtin.receiver.ok_or_else(|| {
                    VMError::TypeError("RegExp.test missing receiver".to_string())
                })?;
                let re = match receiver {
                    Object::RegExp(re) => *re,
                    _ => return Ok(Value::from_bool(false)),
                };
                let text = args
                    .first()
                    .map(|v| val_inspect(*v, &self.heap))
                    .unwrap_or_default();
                let regex = self.build_regex(&re.pattern, &re.flags)?;
                Ok(Value::from_bool(regex.is_match(&text)))
            }
            BuiltinFunction::RegExpExec => {
                let receiver = builtin.receiver.ok_or_else(|| {
                    VMError::TypeError("RegExp.exec missing receiver".to_string())
                })?;
                let re = match receiver {
                    Object::RegExp(re) => *re,
                    _ => return Ok(Value::NULL),
                };
                let text = args
                    .first()
                    .map(|v| val_inspect(*v, &self.heap))
                    .unwrap_or_default();
                let regex = self.build_regex(&re.pattern, &re.flags)?;
                if let Some(captures) = regex.captures(&text) {
                    let mut result = Vec::new();
                    for i in 0..captures.len() {
                        if let Some(m) = captures.get(i) {
                            result.push(obj_into_val(
                                Object::String(m.as_str().to_string().into()),
                                &mut self.heap,
                            ));
                        } else {
                            result.push(Value::UNDEFINED);
                        }
                    }
                    Ok(obj_into_val(make_array(result), &mut self.heap))
                } else {
                    Ok(Value::NULL)
                }
            }
            BuiltinFunction::StringMatch => {
                let receiver = builtin.receiver.ok_or_else(|| {
                    VMError::TypeError("String.match missing receiver".to_string())
                })?;
                let text = match receiver {
                    Object::String(s) => s,
                    _ => return Ok(Value::NULL),
                };
                let arg0 = args.first().map(|v| val_to_obj(*v, &self.heap));
                let (pattern, flags, is_regex_input) = match arg0.as_ref() {
                    Some(Object::RegExp(re)) => (re.pattern.clone(), re.flags.clone(), true),
                    Some(other) => (other.inspect(), String::new(), false),
                    None => (String::new(), String::new(), false),
                };
                if pattern.is_empty() {
                    let empty_str_val = obj_into_val(Object::String(Rc::from("")), &mut self.heap);
                    return Ok(obj_into_val(
                        make_array(vec![empty_str_val]),
                        &mut self.heap,
                    ));
                }

                let regex = self.build_regex(&pattern, &flags)?;
                if is_regex_input && flags.contains('g') {
                    let matches: Vec<Value> = regex
                        .find_iter(&text)
                        .map(|m| {
                            obj_into_val(
                                Object::String(m.as_str().to_string().into()),
                                &mut self.heap,
                            )
                        })
                        .collect();
                    if matches.is_empty() {
                        Ok(Value::NULL)
                    } else {
                        Ok(obj_into_val(make_array(matches), &mut self.heap))
                    }
                } else if let Some(captures) = regex.captures(&text) {
                    let mut result = Vec::new();
                    for i in 0..captures.len() {
                        if let Some(m) = captures.get(i) {
                            result.push(obj_into_val(
                                Object::String(m.as_str().to_string().into()),
                                &mut self.heap,
                            ));
                        } else {
                            result.push(Value::UNDEFINED);
                        }
                    }
                    Ok(obj_into_val(make_array(result), &mut self.heap))
                } else {
                    Ok(Value::NULL)
                }
            }
            BuiltinFunction::StringMatchAll => {
                let receiver = builtin.receiver.ok_or_else(|| {
                    VMError::TypeError("String.matchAll missing receiver".to_string())
                })?;
                let text = match receiver {
                    Object::String(s) => s,
                    _ => return Ok(obj_into_val(make_array(vec![]), &mut self.heap)),
                };

                let arg0 = args.first().map(|v| val_to_obj(*v, &self.heap));
                let (pattern, flags) = match arg0.as_ref() {
                    Some(Object::RegExp(re)) => {
                        if !re.flags.contains('g') {
                            return Err(VMError::TypeError(
                                "String.prototype.matchAll called with a non-global RegExp"
                                    .to_string(),
                            ));
                        }
                        (re.pattern.clone(), re.flags.clone())
                    }
                    Some(other) => (regex::escape(&other.inspect()), "g".to_string()),
                    None => (String::new(), "g".to_string()),
                };

                let regex = self.build_regex(&pattern, &flags)?;
                let mut out: Vec<Value> = Vec::new();
                for caps in regex.captures_iter(&text) {
                    let mut m: Vec<Value> = Vec::new();
                    for i in 0..caps.len() {
                        match caps.get(i) {
                            Some(g) => m.push(obj_into_val(
                                Object::String(g.as_str().to_string().into()),
                                &mut self.heap,
                            )),
                            None => m.push(Value::UNDEFINED),
                        }
                    }
                    out.push(obj_into_val(make_array(m), &mut self.heap));
                }
                Ok(obj_into_val(make_array(out), &mut self.heap))
            }
            BuiltinFunction::StringSearch => {
                let receiver = builtin.receiver.ok_or_else(|| {
                    VMError::TypeError("String.search missing receiver".to_string())
                })?;
                let text = match receiver {
                    Object::String(s) => s,
                    _ => return Ok(Value::from_i64(-1)),
                };
                let arg0 = args.first().map(|v| val_to_obj(*v, &self.heap));
                let (pattern, flags) = match arg0.as_ref() {
                    Some(Object::RegExp(re)) => (re.pattern.clone(), re.flags.clone()),
                    Some(other) => (other.inspect(), String::new()),
                    None => (String::new(), String::new()),
                };
                let regex = self.build_regex(&pattern, &flags)?;
                if let Some(m) = regex.find(&text) {
                    Ok(Value::from_i64(m.start() as i64))
                } else {
                    Ok(Value::from_i64(-1))
                }
            }
            BuiltinFunction::StringConcat => {
                let receiver = builtin.receiver.ok_or_else(|| {
                    VMError::TypeError("String.concat missing receiver".to_string())
                })?;
                let text = match receiver {
                    Object::String(s) => s,
                    _ => return Ok(obj_into_val(Object::String(Rc::from("")), &mut self.heap)),
                };
                let mut result = text.to_string();
                for arg in args {
                    let s = val_inspect(*arg, &self.heap);
                    result.push_str(&s);
                }
                Ok(obj_into_val(
                    Object::String(Rc::from(result.as_str())),
                    &mut self.heap,
                ))
            }
            BuiltinFunction::StringTrimStart => {
                let receiver = builtin.receiver.ok_or_else(|| {
                    VMError::TypeError("String.trimStart missing receiver".to_string())
                })?;
                let text = match receiver {
                    Object::String(s) => s,
                    _ => return Ok(obj_into_val(Object::String(Rc::from("")), &mut self.heap)),
                };
                Ok(obj_into_val(
                    Object::String(Rc::from(text.trim_start())),
                    &mut self.heap,
                ))
            }
            BuiltinFunction::StringTrimEnd => {
                let receiver = builtin.receiver.ok_or_else(|| {
                    VMError::TypeError("String.trimEnd missing receiver".to_string())
                })?;
                let text = match receiver {
                    Object::String(s) => s,
                    _ => return Ok(obj_into_val(Object::String(Rc::from("")), &mut self.heap)),
                };
                Ok(obj_into_val(
                    Object::String(Rc::from(text.trim_end())),
                    &mut self.heap,
                ))
            }
            BuiltinFunction::StringFromCodePoint => {
                let mut out = String::new();
                for arg in args {
                    let code = self.to_u32_val(*arg)?;
                    let ch = char::from_u32(code).ok_or_else(|| {
                        VMError::TypeError(format!(
                            "Invalid code point: {}",
                            code
                        ))
                    })?;
                    out.push(ch);
                }
                Ok(obj_into_val(Object::String(out.into()), &mut self.heap))
            }
            BuiltinFunction::MapCtor => {
                let map_obj = crate::object::MapObject::default();
                if let Some(source_val) = args.first() {
                    let source = val_to_obj(*source_val, &self.heap);
                    match &source {
                        Object::Array(entries) => {
                            let target_entries = unsafe { map_obj.entries.borrow_mut() };
                            let target_indices = unsafe { map_obj.indices.borrow_mut() };
                            // Re-check wall-time every 1 024 inserts.
                            // Upstream `MAX_ARRAY_SIZE` caps the source
                            // but 1 M inserts is still enough runtime
                            // work to want an abort-flag check.
                            for (i, entry) in entries.borrow().iter().enumerate() {
                                if (i & 0x3ff) == 0 {
                                    self.check_builtin_callback_limits()?;
                                }
                                let entry_obj = val_to_obj(*entry, &self.heap);
                                if let Object::Array(pair) = entry_obj {
                                    let pair = pair.borrow();
                                    if pair.len() >= 2 {
                                        let key_obj = val_to_obj(pair[0], &self.heap);
                                        let key = self.hash_key_from_object(&key_obj);
                                        Self::map_insert_or_replace(
                                            target_entries,
                                            target_indices,
                                            key,
                                            pair[1],
                                        );
                                    }
                                }
                            }
                        }
                        Object::Map(existing) => {
                            let target_entries = unsafe { map_obj.entries.borrow_mut() };
                            let target_indices = unsafe { map_obj.indices.borrow_mut() };
                            for (i, (k, v)) in existing.entries.borrow().iter().enumerate() {
                                if (i & 0x3ff) == 0 {
                                    self.check_builtin_callback_limits()?;
                                }
                                Self::map_insert_or_replace(
                                    target_entries,
                                    target_indices,
                                    k.clone(),
                                    *v,
                                );
                            }
                        }
                        _ => {}
                    }
                }
                Ok(obj_into_val(Object::Map(Box::new(map_obj)), &mut self.heap))
            }
            BuiltinFunction::MapSet => {
                let receiver = builtin
                    .receiver
                    .ok_or_else(|| VMError::TypeError("Map.set missing receiver".to_string()))?;
                let map_obj = match receiver {
                    Object::Map(m) => m,
                    _ => return Ok(Value::UNDEFINED),
                };
                if args.len() >= 2 {
                    // Use hash_key_from_value to skip the Value→Object materialization
                    // (each `Rc::from(s)` allocation showed up hot in the Map bench).
                    let key = self.hash_key_from_value(args[0]);
                    let entries = unsafe { map_obj.entries.borrow_mut() };
                    let indices = unsafe { map_obj.indices.borrow_mut() };
                    Self::map_insert_or_replace(entries, indices, key, args[1]);
                }
                Ok(obj_into_val(Object::Map(map_obj), &mut self.heap))
            }
            BuiltinFunction::MapGet => {
                let receiver = builtin
                    .receiver
                    .ok_or_else(|| VMError::TypeError("Map.get missing receiver".to_string()))?;
                let map_obj = match receiver {
                    Object::Map(m) => m,
                    _ => return Ok(Value::UNDEFINED),
                };
                let key = args.first().map(|v| self.hash_key_from_value(*v));
                if let Some(k) = key {
                    let entries = map_obj.entries.borrow();
                    let indices = map_obj.indices.borrow();
                    Ok(Self::map_get(entries, indices, &k).unwrap_or(Value::UNDEFINED))
                } else {
                    Ok(Value::UNDEFINED)
                }
            }
            BuiltinFunction::MapHas => {
                let receiver = builtin
                    .receiver
                    .ok_or_else(|| VMError::TypeError("Map.has missing receiver".to_string()))?;
                let map_obj = match receiver {
                    Object::Map(m) => m,
                    _ => return Ok(Value::from_bool(false)),
                };
                let key = args.first().map(|v| self.hash_key_from_value(*v));
                Ok(Value::from_bool(
                    key.map(|k| Self::map_contains(map_obj.entries.borrow(), map_obj.indices.borrow(), &k))
                        .unwrap_or(false),
                ))
            }
            BuiltinFunction::MapDelete => {
                let receiver = builtin
                    .receiver
                    .ok_or_else(|| VMError::TypeError("Map.delete missing receiver".to_string()))?;
                let map_obj = match receiver {
                    Object::Map(m) => m,
                    _ => return Ok(Value::from_bool(false)),
                };
                let key = args.first().map(|v| self.hash_key_from_value(*v));
                Ok(Value::from_bool(
                    key.map(|k| {
                        let entries = unsafe { map_obj.entries.borrow_mut() };
                        let indices = unsafe { map_obj.indices.borrow_mut() };
                        Self::map_remove(entries, indices, &k).is_some()
                    })
                    .unwrap_or(false),
                ))
            }
            BuiltinFunction::MapClear => {
                let receiver = builtin
                    .receiver
                    .ok_or_else(|| VMError::TypeError("Map.clear missing receiver".to_string()))?;
                let map_obj = match receiver {
                    Object::Map(m) => m,
                    _ => return Ok(Value::UNDEFINED),
                };
                unsafe { map_obj.entries.borrow_mut() }.clear();
                unsafe { map_obj.indices.borrow_mut() }.clear();
                Ok(Value::UNDEFINED)
            }
            BuiltinFunction::MapKeys => {
                let receiver = builtin
                    .receiver
                    .ok_or_else(|| VMError::TypeError("Map.keys missing receiver".to_string()))?;
                let map_obj = match receiver {
                    Object::Map(m) => m,
                    _ => return Ok(obj_into_val(make_array(vec![]), &mut self.heap)),
                };
                let items: Vec<Value> = map_obj
                    .entries
                    .borrow()
                    .iter()
                    .map(|(k, _)| obj_into_val(self.object_from_hash_key(k), &mut self.heap))
                    .collect();
                Ok(obj_into_val(make_array(items), &mut self.heap))
            }
            BuiltinFunction::MapValues => {
                let receiver = builtin
                    .receiver
                    .ok_or_else(|| VMError::TypeError("Map.values missing receiver".to_string()))?;
                let map_obj = match receiver {
                    Object::Map(m) => m,
                    _ => return Ok(obj_into_val(make_array(vec![]), &mut self.heap)),
                };
                let items: Vec<Value> = map_obj.entries.borrow().iter().map(|(_, v)| *v).collect();
                Ok(obj_into_val(make_array(items), &mut self.heap))
            }
            BuiltinFunction::MapEntries => {
                let receiver = builtin.receiver.ok_or_else(|| {
                    VMError::TypeError("Map.entries missing receiver".to_string())
                })?;
                let map_obj = match receiver {
                    Object::Map(m) => m,
                    _ => return Ok(obj_into_val(make_array(vec![]), &mut self.heap)),
                };
                let items: Vec<Value> = map_obj
                    .entries
                    .borrow()
                    .iter()
                    .map(|(k, v)| {
                        let key_val = obj_into_val(self.object_from_hash_key(k), &mut self.heap);
                        let entry = make_array(vec![key_val, *v]);
                        obj_into_val(entry, &mut self.heap)
                    })
                    .collect();
                Ok(obj_into_val(make_array(items), &mut self.heap))
            }
            BuiltinFunction::MapForEach => {
                let receiver = builtin.receiver.ok_or_else(|| {
                    VMError::TypeError("Map.forEach missing receiver".to_string())
                })?;
                let map_obj = match receiver {
                    Object::Map(m) => m,
                    _ => return Ok(Value::UNDEFINED),
                };
                let callback = args.first().copied().ok_or_else(|| {
                    VMError::TypeError("Map.forEach requires callback".to_string())
                })?;
                let snapshot = map_obj.entries.borrow().clone();
                for (k, v) in snapshot {
                    self.check_builtin_callback_limits()?;
                    let key_val = obj_into_val(self.object_from_hash_key(&k), &mut self.heap);
                    let map_val = obj_into_val(Object::Map(map_obj.clone()), &mut self.heap);
                    let _ = self.call_value3(callback, v, key_val, map_val)?;
                }
                Ok(Value::UNDEFINED)
            }
            BuiltinFunction::SetCtor => {
                let set_obj = crate::object::SetObject::default();
                if let Some(source_val) = args.first() {
                    let source = val_to_obj(*source_val, &self.heap);
                    match &source {
                        Object::Array(entries) => {
                            let target_entries = unsafe { set_obj.entries.borrow_mut() };
                            let target_indices = unsafe { set_obj.indices.borrow_mut() };
                            for (i, entry) in entries.borrow().iter().enumerate() {
                                if (i & 0x3ff) == 0 {
                                    self.check_builtin_callback_limits()?;
                                }
                                let entry_obj = val_to_obj(*entry, &self.heap);
                                Self::set_insert_unique(
                                    target_entries,
                                    target_indices,
                                    self.hash_key_from_object(&entry_obj),
                                );
                            }
                        }
                        Object::Set(existing) => {
                            let target_entries = unsafe { set_obj.entries.borrow_mut() };
                            let target_indices = unsafe { set_obj.indices.borrow_mut() };
                            for (i, key) in existing.entries.borrow().iter().enumerate() {
                                if (i & 0x3ff) == 0 {
                                    self.check_builtin_callback_limits()?;
                                }
                                Self::set_insert_unique(
                                    target_entries,
                                    target_indices,
                                    key.clone(),
                                );
                            }
                        }
                        Object::String(text) => {
                            let target_entries = unsafe { set_obj.entries.borrow_mut() };
                            let target_indices = unsafe { set_obj.indices.borrow_mut() };
                            for ch in text.chars() {
                                Self::set_insert_unique(
                                    target_entries,
                                    target_indices,
                                    self.hash_key_from_object(&Object::String(
                                        ch.to_string().into(),
                                    )),
                                );
                            }
                        }
                        _ => {}
                    }
                }
                Ok(obj_into_val(Object::Set(Box::new(set_obj)), &mut self.heap))
            }
            BuiltinFunction::SetAdd => {
                let receiver = builtin
                    .receiver
                    .ok_or_else(|| VMError::TypeError("Set.add missing receiver".to_string()))?;
                let set_obj = match receiver {
                    Object::Set(s) => s,
                    _ => return Ok(Value::UNDEFINED),
                };
                if let Some(v) = args.first() {
                    let obj = val_to_obj(*v, &self.heap);
                    let entries = unsafe { set_obj.entries.borrow_mut() };
                    let indices = unsafe { set_obj.indices.borrow_mut() };
                    Self::set_insert_unique(
                        entries,
                        indices,
                        self.hash_key_from_object(&obj),
                    );
                }
                Ok(obj_into_val(Object::Set(set_obj), &mut self.heap))
            }
            BuiltinFunction::SetHas => {
                let receiver = builtin
                    .receiver
                    .ok_or_else(|| VMError::TypeError("Set.has missing receiver".to_string()))?;
                let set_obj = match receiver {
                    Object::Set(s) => s,
                    _ => return Ok(Value::from_bool(false)),
                };
                let key = args.first().map(|v| {
                    let obj = val_to_obj(*v, &self.heap);
                    self.hash_key_from_object(&obj)
                });
                Ok(Value::from_bool(
                    key.map(|k| Self::set_contains(set_obj.indices.borrow(), &k))
                        .unwrap_or(false),
                ))
            }
            BuiltinFunction::SetDelete => {
                let receiver = builtin
                    .receiver
                    .ok_or_else(|| VMError::TypeError("Set.delete missing receiver".to_string()))?;
                let set_obj = match receiver {
                    Object::Set(s) => s,
                    _ => return Ok(Value::from_bool(false)),
                };
                let key = args.first().map(|v| {
                    let obj = val_to_obj(*v, &self.heap);
                    self.hash_key_from_object(&obj)
                });
                Ok(Value::from_bool(
                    key.map(|k| {
                        let entries = unsafe { set_obj.entries.borrow_mut() };
                        let indices = unsafe { set_obj.indices.borrow_mut() };
                        Self::set_remove(entries, indices, &k)
                    })
                    .unwrap_or(false),
                ))
            }
            BuiltinFunction::SetClear => {
                let receiver = builtin
                    .receiver
                    .ok_or_else(|| VMError::TypeError("Set.clear missing receiver".to_string()))?;
                let set_obj = match receiver {
                    Object::Set(s) => s,
                    _ => return Ok(Value::UNDEFINED),
                };
                unsafe { set_obj.entries.borrow_mut() }.clear();
                unsafe { set_obj.indices.borrow_mut() }.clear();
                Ok(Value::UNDEFINED)
            }
            BuiltinFunction::SetKeys | BuiltinFunction::SetValues => {
                let receiver = builtin.receiver.ok_or_else(|| {
                    VMError::TypeError("Set.keys/values missing receiver".to_string())
                })?;
                let set_obj = match receiver {
                    Object::Set(s) => s,
                    _ => return Ok(obj_into_val(make_array(vec![]), &mut self.heap)),
                };
                let out: Vec<Value> = set_obj
                    .entries
                    .borrow()
                    .iter()
                    .map(|k| obj_into_val(self.object_from_hash_key(k), &mut self.heap))
                    .collect();
                Ok(obj_into_val(make_array(out), &mut self.heap))
            }
            BuiltinFunction::SetEntries => {
                let receiver = builtin.receiver.ok_or_else(|| {
                    VMError::TypeError("Set.entries missing receiver".to_string())
                })?;
                let set_obj = match receiver {
                    Object::Set(s) => s,
                    _ => return Ok(obj_into_val(make_array(vec![]), &mut self.heap)),
                };
                let out: Vec<Value> = set_obj
                    .entries
                    .borrow()
                    .iter()
                    .map(|k| {
                        let v = obj_into_val(self.object_from_hash_key(k), &mut self.heap);
                        let entry = make_array(vec![v, v]);
                        obj_into_val(entry, &mut self.heap)
                    })
                    .collect();
                Ok(obj_into_val(make_array(out), &mut self.heap))
            }
            BuiltinFunction::SetForEach => {
                let receiver = builtin.receiver.ok_or_else(|| {
                    VMError::TypeError("Set.forEach missing receiver".to_string())
                })?;
                let set_obj = match receiver {
                    Object::Set(s) => s,
                    _ => return Ok(Value::UNDEFINED),
                };
                let callback = args.first().copied().ok_or_else(|| {
                    VMError::TypeError("Set.forEach requires callback".to_string())
                })?;
                let snapshot = set_obj.entries.borrow().clone();
                for k in snapshot {
                    self.check_builtin_callback_limits()?;
                    let v_val = obj_into_val(self.object_from_hash_key(&k), &mut self.heap);
                    let set_val = obj_into_val(Object::Set(set_obj.clone()), &mut self.heap);
                    let _ = self.call_value3(callback, v_val, v_val, set_val)?;
                }
                Ok(Value::UNDEFINED)
            }
            BuiltinFunction::GeneratorNext => {
                let receiver = builtin.receiver.ok_or_else(|| {
                    VMError::TypeError("Generator.next missing receiver".to_string())
                })?;
                let gen_rc = match receiver {
                    Object::Generator(g) => g,
                    _ => return Ok(Value::UNDEFINED),
                };
                let next_arg = args.first().copied().unwrap_or(Value::UNDEFINED);
                self.execute_generator_next(&gen_rc, next_arg)
            }
            BuiltinFunction::GeneratorReturn => {
                let receiver = builtin.receiver.ok_or_else(|| {
                    VMError::TypeError("Generator.return missing receiver".to_string())
                })?;
                let gen_rc = match receiver {
                    Object::Generator(g) => g,
                    _ => return Ok(Value::UNDEFINED),
                };
                let return_value = args.first().copied().unwrap_or(Value::UNDEFINED);
                self.execute_generator_return(&gen_rc, return_value)
            }
            BuiltinFunction::GeneratorThrow => {
                let receiver = builtin.receiver.ok_or_else(|| {
                    VMError::TypeError("Generator.throw missing receiver".to_string())
                })?;
                let gen_rc = match receiver {
                    Object::Generator(g) => g,
                    _ => return Ok(Value::UNDEFINED),
                };
                // For now, just mark the generator as completed and propagate
                // the error.  Full throw-into-generator semantics would require
                // resuming inside a try/catch inside the generator body.
                unsafe { gen_rc.borrow_mut() }.state = crate::object::GeneratorState::Completed;
                let err_val = args.first().copied().unwrap_or(Value::UNDEFINED);
                let err_obj = val_to_obj(err_val, &self.heap);
                let msg = match &err_obj {
                    Object::String(s) => s.to_string(),
                    Object::Error(e) => e.message.to_string(),
                    _ => format!("{:?}", err_obj),
                };
                Err(VMError::TypeError(msg))
            }
            BuiltinFunction::DateNow => {
                let ms = epoch_millis_now();
                Ok(Value::from_f64(ms))
            }
            BuiltinFunction::DateUtc => {
                // Date.UTC(year, month, day=1, hours=0, minutes=0, seconds=0, ms=0)
                let year = args
                    .first()
                    .map(|v| self.to_number_val(*v))
                    .transpose()?
                    .unwrap_or(f64::NAN) as i64;
                let month = args
                    .get(1)
                    .map(|v| self.to_number_val(*v))
                    .transpose()?
                    .unwrap_or(0.0) as i64;
                let day = args
                    .get(2)
                    .map(|v| self.to_number_val(*v))
                    .transpose()?
                    .unwrap_or(1.0) as i64;
                let hours = args
                    .get(3)
                    .map(|v| self.to_number_val(*v))
                    .transpose()?
                    .unwrap_or(0.0) as i64;
                let minutes = args
                    .get(4)
                    .map(|v| self.to_number_val(*v))
                    .transpose()?
                    .unwrap_or(0.0) as i64;
                let seconds = args
                    .get(5)
                    .map(|v| self.to_number_val(*v))
                    .transpose()?
                    .unwrap_or(0.0) as i64;
                let millis = args
                    .get(6)
                    .map(|v| self.to_number_val(*v))
                    .transpose()?
                    .unwrap_or(0.0) as i64;
                // JS months are 0-indexed in this signature.
                let days = Self::ymd_to_days(year, month + 1, day);
                let ms = (days * 86400 + hours * 3600 + minutes * 60 + seconds) * 1000 + millis;
                Ok(Value::from_f64(ms as f64))
            }
            BuiltinFunction::DateParse => {
                // Minimal: accept the same numeric forms the constructor
                // accepts — full ISO 8601 parsing is out of scope here.
                let s = args
                    .first()
                    .map(|v| val_inspect(*v, &self.heap))
                    .unwrap_or_default();
                Ok(Value::from_f64(s.parse::<f64>().unwrap_or(f64::NAN)))
            }
            // ── Date setters ──
            // Each setter mutates the receiver hash's `__time_ms` and
            // returns the new ms value, matching V8's setter contract.
            BuiltinFunction::DateSetTime => {
                let ms = args
                    .first()
                    .map(|v| self.to_number_val(*v))
                    .transpose()?
                    .unwrap_or(f64::NAN);
                Ok(Value::from_f64(Self::store_date_ms(&builtin.receiver, ms)))
            }
            BuiltinFunction::DateSetMilliseconds => {
                let new_ms = args
                    .first()
                    .map(|v| self.to_number_val(*v))
                    .transpose()?
                    .unwrap_or(0.0) as i64;
                let cur_ms = Self::extract_date_ms(&builtin.receiver) as i64;
                let secs = cur_ms / 1000;
                let updated = secs * 1000 + ((new_ms % 1000 + 1000) % 1000);
                Ok(Value::from_f64(Self::store_date_ms(
                    &builtin.receiver,
                    updated as f64,
                )))
            }
            BuiltinFunction::DateSetSeconds => {
                let new_s = args
                    .first()
                    .map(|v| self.to_number_val(*v))
                    .transpose()?
                    .unwrap_or(0.0) as i64;
                let cur_ms = Self::extract_date_ms(&builtin.receiver) as i64;
                let cur_min = cur_ms / 60000;
                let cur_milli = ((cur_ms % 1000) + 1000) % 1000;
                let updated = cur_min * 60000 + new_s * 1000 + cur_milli;
                Ok(Value::from_f64(Self::store_date_ms(
                    &builtin.receiver,
                    updated as f64,
                )))
            }
            BuiltinFunction::DateSetMinutes => {
                let new_min = args
                    .first()
                    .map(|v| self.to_number_val(*v))
                    .transpose()?
                    .unwrap_or(0.0) as i64;
                let cur_ms = Self::extract_date_ms(&builtin.receiver) as i64;
                let cur_hour = cur_ms / 3600000;
                let rem = ((cur_ms % 60000) + 60000) % 60000;
                let updated = cur_hour * 3600000 + new_min * 60000 + rem;
                Ok(Value::from_f64(Self::store_date_ms(
                    &builtin.receiver,
                    updated as f64,
                )))
            }
            BuiltinFunction::DateSetHours => {
                let new_h = args
                    .first()
                    .map(|v| self.to_number_val(*v))
                    .transpose()?
                    .unwrap_or(0.0) as i64;
                let cur_ms = Self::extract_date_ms(&builtin.receiver) as i64;
                let day_part = cur_ms / 86400000 * 86400000;
                let rem = ((cur_ms % 3600000) + 3600000) % 3600000;
                let updated = day_part + new_h * 3600000 + rem;
                Ok(Value::from_f64(Self::store_date_ms(
                    &builtin.receiver,
                    updated as f64,
                )))
            }
            BuiltinFunction::DateSetDate => {
                let new_d = args
                    .first()
                    .map(|v| self.to_number_val(*v))
                    .transpose()?
                    .unwrap_or(1.0) as i64;
                let cur_ms = Self::extract_date_ms(&builtin.receiver) as i64;
                let secs = cur_ms / 1000;
                let cur_days = secs / 86400;
                let (year, month, _day) = Self::days_to_ymd(cur_days);
                let new_days = Self::ymd_to_days(year, month, new_d);
                let updated = new_days * 86400000 + (cur_ms % 86400000 + 86400000) % 86400000;
                Ok(Value::from_f64(Self::store_date_ms(
                    &builtin.receiver,
                    updated as f64,
                )))
            }
            BuiltinFunction::DateSetMonth => {
                let new_m_idx = args
                    .first()
                    .map(|v| self.to_number_val(*v))
                    .transpose()?
                    .unwrap_or(0.0) as i64;
                let cur_ms = Self::extract_date_ms(&builtin.receiver) as i64;
                let secs = cur_ms / 1000;
                let cur_days = secs / 86400;
                let (year, _month, day) = Self::days_to_ymd(cur_days);
                let new_days = Self::ymd_to_days(year, new_m_idx + 1, day);
                let updated = new_days * 86400000 + (cur_ms % 86400000 + 86400000) % 86400000;
                Ok(Value::from_f64(Self::store_date_ms(
                    &builtin.receiver,
                    updated as f64,
                )))
            }
            BuiltinFunction::DateSetFullYear => {
                let new_y = args
                    .first()
                    .map(|v| self.to_number_val(*v))
                    .transpose()?
                    .unwrap_or(f64::NAN) as i64;
                let cur_ms = Self::extract_date_ms(&builtin.receiver) as i64;
                let secs = cur_ms / 1000;
                let cur_days = secs / 86400;
                let (_y, month, day) = Self::days_to_ymd(cur_days);
                let new_days = Self::ymd_to_days(new_y, month, day);
                let updated = new_days * 86400000 + (cur_ms % 86400000 + 86400000) % 86400000;
                Ok(Value::from_f64(Self::store_date_ms(
                    &builtin.receiver,
                    updated as f64,
                )))
            }
            BuiltinFunction::DateToISOString => {
                let ms = Self::extract_date_ms(&builtin.receiver);
                let secs = (ms / 1000.0) as i64;
                let millis = (ms % 1000.0) as u32;
                let (year, month, day) = Self::days_to_ymd(secs / 86400);
                let tod = secs % 86400;
                let iso = format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
                    year, month, day, tod / 3600, (tod % 3600) / 60, tod % 60, millis);
                Ok(obj_into_val(Object::String(iso.into()), &mut self.heap))
            }
            BuiltinFunction::DateGetTime | BuiltinFunction::DateValueOf => {
                let ms = Self::extract_date_ms(&builtin.receiver);
                Ok(Value::from_f64(ms))
            }
            BuiltinFunction::DateGetHours => {
                let ms = Self::extract_date_ms(&builtin.receiver);
                let secs = (ms / 1000.0) as i64;
                let hours = ((secs % 86400) + 86400) % 86400 / 3600;
                Ok(Value::from_i64(hours))
            }
            BuiltinFunction::DateGetMinutes => {
                let ms = Self::extract_date_ms(&builtin.receiver);
                let secs = (ms / 1000.0) as i64;
                let minutes = ((secs % 3600) + 3600) % 3600 / 60;
                Ok(Value::from_i64(minutes))
            }
            BuiltinFunction::DateGetSeconds => {
                let ms = Self::extract_date_ms(&builtin.receiver);
                let secs = (ms / 1000.0) as i64;
                Ok(Value::from_i64(((secs % 60) + 60) % 60))
            }
            BuiltinFunction::DateGetMilliseconds => {
                let ms = Self::extract_date_ms(&builtin.receiver);
                Ok(Value::from_i64(((ms % 1000.0) + 1000.0) as i64 % 1000))
            }
            BuiltinFunction::DateGetFullYear => {
                let ms = Self::extract_date_ms(&builtin.receiver);
                let secs = (ms / 1000.0) as i64;
                let (year, _, _) = Self::days_to_ymd(secs / 86400);
                Ok(Value::from_i64(year))
            }
            BuiltinFunction::DateGetMonth => {
                let ms = Self::extract_date_ms(&builtin.receiver);
                let secs = (ms / 1000.0) as i64;
                let (_, month, _) = Self::days_to_ymd(secs / 86400);
                // JS months are 0-indexed
                Ok(Value::from_i64(month - 1))
            }
            BuiltinFunction::DateGetDate => {
                let ms = Self::extract_date_ms(&builtin.receiver);
                let secs = (ms / 1000.0) as i64;
                let (_, _, day) = Self::days_to_ymd(secs / 86400);
                Ok(Value::from_i64(day))
            }
            BuiltinFunction::DateGetDay => {
                let ms = Self::extract_date_ms(&builtin.receiver);
                let secs = (ms / 1000.0) as i64;
                // Day of week: 0=Sunday. Unix epoch (1970-01-01) was a Thursday (4).
                let days = secs / 86400;
                let dow = ((days % 7 + 4) % 7 + 7) % 7;
                Ok(Value::from_i64(dow))
            }
            BuiltinFunction::DateToLocaleDateString
            | BuiltinFunction::DateToLocaleTimeString
            | BuiltinFunction::DateToLocaleString
            | BuiltinFunction::DateToString => {
                let ms = Self::extract_date_ms(&builtin.receiver);
                let secs = (ms / 1000.0) as i64;
                let millis = (ms % 1000.0) as u32;
                let (year, month, day) = Self::days_to_ymd(secs / 86400);
                let tod = ((secs % 86400) + 86400) % 86400;
                let hours = tod / 3600;
                let minutes = (tod % 3600) / 60;
                let seconds = tod % 60;
                let s = match builtin.function {
                    BuiltinFunction::DateToLocaleDateString => {
                        format!("{}/{}/{}", month, day, year)
                    }
                    BuiltinFunction::DateToLocaleTimeString => {
                        let ampm = if hours < 12 { "AM" } else { "PM" };
                        let h12 = if hours == 0 { 12 } else if hours > 12 { hours - 12 } else { hours };
                        format!("{}:{:02}:{:02} {}", h12, minutes, seconds, ampm)
                    }
                    BuiltinFunction::DateToLocaleString => {
                        let ampm = if hours < 12 { "AM" } else { "PM" };
                        let h12 = if hours == 0 { 12 } else if hours > 12 { hours - 12 } else { hours };
                        format!("{}/{}/{}, {}:{:02}:{:02} {}", month, day, year, h12, minutes, seconds, ampm)
                    }
                    _ => {
                        // DateToString
                        format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
                            year, month, day, hours, minutes, seconds, millis)
                    }
                };
                Ok(obj_into_val(Object::String(s.into()), &mut self.heap))
            }
            BuiltinFunction::LocalStorageGetItem => {
                if let Some(ref storage) = self.local_storage {
                    let key = args
                        .first()
                        .map(|v| val_inspect(*v, &self.heap))
                        .unwrap_or_default();
                    match storage.get_item(&key) {
                        Some(val) => {
                            let s = Object::String(val.into());
                            Ok(obj_into_val(s, &mut self.heap))
                        }
                        None => Ok(Value::NULL),
                    }
                } else {
                    Ok(Value::NULL)
                }
            }
            BuiltinFunction::LocalStorageSetItem => {
                if let Some(ref mut storage) = self.local_storage {
                    let key = args
                        .first()
                        .map(|v| val_inspect(*v, &self.heap))
                        .unwrap_or_default();
                    let value = args
                        .get(1)
                        .map(|v| val_inspect(*v, &self.heap))
                        .unwrap_or_default();
                    storage.set_item(&key, &value);
                }
                Ok(Value::UNDEFINED)
            }
            BuiltinFunction::LocalStorageRemoveItem => {
                if let Some(ref mut storage) = self.local_storage {
                    let key = args
                        .first()
                        .map(|v| val_inspect(*v, &self.heap))
                        .unwrap_or_default();
                    storage.remove_item(&key);
                }
                Ok(Value::UNDEFINED)
            }
            BuiltinFunction::LocalStorageClear => {
                if let Some(ref mut storage) = self.local_storage {
                    storage.clear();
                }
                Ok(Value::UNDEFINED)
            }
            BuiltinFunction::DbQuery => {
                if let Some(ref db) = self.db {
                    let collection = args
                        .first()
                        .map(|v| val_inspect(*v, &self.heap))
                        .unwrap_or_default();
                    match db.query(&collection) {
                        Ok(records) => {
                            let items: Vec<Value> = records
                                .into_iter()
                                .map(|r| {
                                    let obj = self.db_record_to_object(r);
                                    obj_into_val(obj, &mut self.heap)
                                })
                                .collect();
                            Ok(obj_into_val(make_array(items), &mut self.heap))
                        }
                        Err(e) => Err(VMError::TypeError(format!("db.query error: {}", e))),
                    }
                } else {
                    Ok(obj_into_val(make_array(vec![]), &mut self.heap))
                }
            }
            BuiltinFunction::DbCreate => {
                let collection = args
                    .first()
                    .map(|v| val_inspect(*v, &self.heap))
                    .unwrap_or_default();
                let data_json = if let Some(&v) = args.get(1) {
                    let obj = val_to_obj(v, &self.heap);
                    let jv = self.object_to_json_value(&obj);
                    serde_json::to_string(&jv).unwrap_or_else(|_| "{}".to_string())
                } else {
                    "{}".to_string()
                };
                if let Some(ref mut db) = self.db {
                    match db.create(&collection, &data_json) {
                        Ok(record) => {
                            let obj = self.db_record_to_object(record);
                            Ok(obj_into_val(obj, &mut self.heap))
                        }
                        Err(e) => Err(VMError::TypeError(format!("db.create error: {}", e))),
                    }
                } else {
                    Ok(Value::NULL)
                }
            }
            BuiltinFunction::DbUpdate => {
                let id = args
                    .first()
                    .map(|v| val_inspect(*v, &self.heap))
                    .unwrap_or_default();
                let data_json = if let Some(&v) = args.get(1) {
                    let obj = val_to_obj(v, &self.heap);
                    let jv = self.object_to_json_value(&obj);
                    serde_json::to_string(&jv).unwrap_or_else(|_| "{}".to_string())
                } else {
                    "{}".to_string()
                };
                if let Some(ref mut db) = self.db {
                    match db.update(&id, &data_json) {
                        Ok(Some(record)) => {
                            let obj = self.db_record_to_object(record);
                            Ok(obj_into_val(obj, &mut self.heap))
                        }
                        Ok(None) => Ok(Value::NULL),
                        Err(e) => Err(VMError::TypeError(format!("db.update error: {}", e))),
                    }
                } else {
                    Ok(Value::NULL)
                }
            }
            BuiltinFunction::DbDelete => {
                if let Some(ref mut db) = self.db {
                    let id = args
                        .first()
                        .map(|v| val_inspect(*v, &self.heap))
                        .unwrap_or_default();
                    if let Err(e) = db.delete(&id) {
                        return Err(VMError::TypeError(format!("db.delete error: {}", e)));
                    }
                }
                Ok(Value::UNDEFINED)
            }
            BuiltinFunction::DbHardDelete => {
                if let Some(ref mut db) = self.db {
                    let collection = args
                        .first()
                        .map(|v| val_inspect(*v, &self.heap))
                        .unwrap_or_default();
                    let id = args
                        .get(1)
                        .map(|v| val_inspect(*v, &self.heap))
                        .unwrap_or_default();
                    if let Err(e) = db.hard_delete(&collection, &id) {
                        return Err(VMError::TypeError(format!("db.hardDelete error: {}", e)));
                    }
                }
                Ok(Value::UNDEFINED)
            }
            BuiltinFunction::DbGet => {
                if let Some(ref db) = self.db {
                    let collection = args
                        .first()
                        .map(|v| val_inspect(*v, &self.heap))
                        .unwrap_or_default();
                    let id = args
                        .get(1)
                        .map(|v| val_inspect(*v, &self.heap))
                        .unwrap_or_default();
                    match db.get(&collection, &id) {
                        Ok(Some(record)) => {
                            let obj = self.db_record_to_object(record);
                            Ok(obj_into_val(obj, &mut self.heap))
                        }
                        Ok(None) => Ok(Value::NULL),
                        Err(e) => Err(VMError::TypeError(format!("db.get error: {}", e))),
                    }
                } else {
                    Ok(Value::NULL)
                }
            }
            BuiltinFunction::DbStartSync => {
                if let Some(ref mut db) = self.db {
                    let room = args
                        .first()
                        .map(|v| val_inspect(*v, &self.heap))
                        .unwrap_or_default();
                    db.start_sync(&room);
                }
                Ok(Value::UNDEFINED)
            }
            BuiltinFunction::DbStopSync => {
                if let Some(ref mut db) = self.db {
                    let room = args
                        .first()
                        .map(|v| {
                            let s = val_inspect(*v, &self.heap);
                            if s == "undefined" || s == "null" || s.is_empty() {
                                None
                            } else {
                                Some(s)
                            }
                        })
                        .unwrap_or(None);
                    db.stop_sync(room.as_deref());
                }
                Ok(Value::UNDEFINED)
            }
            BuiltinFunction::DbGetSyncStatus => {
                if let Some(ref db) = self.db {
                    let room = args
                        .first()
                        .map(|v| {
                            let s = val_inspect(*v, &self.heap);
                            if s == "undefined" || s == "null" || s.is_empty() {
                                None
                            } else {
                                Some(s)
                            }
                        })
                        .unwrap_or(None);
                    let status = db.get_sync_status(room.as_deref());
                    let mut hash = crate::object::HashObject::default();
                    hash.insert_pair(
                        HashKey::from_string("connected"),
                        Value::from_bool(status.connected),
                    );
                    hash.insert_pair(
                        HashKey::from_string("peers"),
                        Value::from_i64(status.peers as i64),
                    );
                    let room_val = obj_into_val(Object::String(status.room.into()), &mut self.heap);
                    hash.insert_pair(HashKey::from_string("room"), room_val);
                    Ok(obj_into_val(make_hash(hash), &mut self.heap))
                } else {
                    let mut hash = crate::object::HashObject::default();
                    hash.insert_pair(HashKey::from_string("connected"), Value::from_bool(false));
                    hash.insert_pair(HashKey::from_string("peers"), Value::from_i64(0));
                    let room_val = obj_into_val(Object::String("".into()), &mut self.heap);
                    hash.insert_pair(HashKey::from_string("room"), room_val);
                    Ok(obj_into_val(make_hash(hash), &mut self.heap))
                }
            }
            BuiltinFunction::DbGetSavedSyncRoom => {
                if let Some(ref db) = self.db {
                    match db.get_saved_sync_room() {
                        Some(room) => {
                            let s = Object::String(room.into());
                            Ok(obj_into_val(s, &mut self.heap))
                        }
                        None => Ok(Value::NULL),
                    }
                } else {
                    Ok(Value::NULL)
                }
            }

            // ════════════════════════════════════════════════════════════════
            // HTTP bridge
            // ════════════════════════════════════════════════════════════════
            BuiltinFunction::HttpGet => {
                if let Some(ref http) = self.http {
                    let url = args.first().map(|v| val_inspect(*v, &self.heap)).unwrap_or_default();
                    match http.get(&url) {
                        Ok(resp) => {
                            let mut hash = crate::object::HashObject::default();
                            hash.insert_pair(HashKey::from_string("status"), Value::from_i64(resp.status as i64));
                            hash.insert_pair(HashKey::from_string("ok"), Value::from_bool(resp.ok));
                            let body_val = obj_into_val(Object::String(resp.body.into()), &mut self.heap);
                            hash.insert_pair(HashKey::from_string("body"), body_val);
                            Ok(obj_into_val(make_hash(hash), &mut self.heap))
                        }
                        Err(e) => {
                            let mut hash = crate::object::HashObject::default();
                            hash.insert_pair(HashKey::from_string("ok"), Value::from_bool(false));
                            hash.insert_pair(HashKey::from_string("status"), Value::from_i64(0));
                            let err_val = obj_into_val(Object::String(e.into()), &mut self.heap);
                            hash.insert_pair(HashKey::from_string("error"), err_val);
                            let body_val = obj_into_val(Object::String("".into()), &mut self.heap);
                            hash.insert_pair(HashKey::from_string("body"), body_val);
                            Ok(obj_into_val(make_hash(hash), &mut self.heap))
                        }
                    }
                } else {
                    Ok(Value::NULL)
                }
            }
            BuiltinFunction::HttpPost => {
                if let Some(ref http) = self.http {
                    let url = args.first().map(|v| val_inspect(*v, &self.heap)).unwrap_or_default();
                    let body = args.get(1).map(|v| val_inspect(*v, &self.heap)).unwrap_or_default();
                    let ct = args.get(2).map(|v| val_inspect(*v, &self.heap)).unwrap_or_else(|| "application/json".into());
                    match http.post(&url, &body, &ct) {
                        Ok(resp) => {
                            let mut hash = crate::object::HashObject::default();
                            hash.insert_pair(HashKey::from_string("status"), Value::from_i64(resp.status as i64));
                            hash.insert_pair(HashKey::from_string("ok"), Value::from_bool(resp.ok));
                            let body_val = obj_into_val(Object::String(resp.body.into()), &mut self.heap);
                            hash.insert_pair(HashKey::from_string("body"), body_val);
                            Ok(obj_into_val(make_hash(hash), &mut self.heap))
                        }
                        Err(e) => {
                            let mut hash = crate::object::HashObject::default();
                            hash.insert_pair(HashKey::from_string("ok"), Value::from_bool(false));
                            hash.insert_pair(HashKey::from_string("status"), Value::from_i64(0));
                            let err_val = obj_into_val(Object::String(e.into()), &mut self.heap);
                            hash.insert_pair(HashKey::from_string("error"), err_val);
                            let body_val = obj_into_val(Object::String("".into()), &mut self.heap);
                            hash.insert_pair(HashKey::from_string("body"), body_val);
                            Ok(obj_into_val(make_hash(hash), &mut self.heap))
                        }
                    }
                } else {
                    Ok(Value::NULL)
                }
            }
            BuiltinFunction::HttpPut => {
                if let Some(ref http) = self.http {
                    let url = args.first().map(|v| val_inspect(*v, &self.heap)).unwrap_or_default();
                    let body = args.get(1).map(|v| val_inspect(*v, &self.heap)).unwrap_or_default();
                    let ct = args.get(2).map(|v| val_inspect(*v, &self.heap)).unwrap_or_else(|| "application/json".into());
                    match http.put(&url, &body, &ct) {
                        Ok(resp) => {
                            let mut hash = crate::object::HashObject::default();
                            hash.insert_pair(HashKey::from_string("status"), Value::from_i64(resp.status as i64));
                            hash.insert_pair(HashKey::from_string("ok"), Value::from_bool(resp.ok));
                            let body_val = obj_into_val(Object::String(resp.body.into()), &mut self.heap);
                            hash.insert_pair(HashKey::from_string("body"), body_val);
                            Ok(obj_into_val(make_hash(hash), &mut self.heap))
                        }
                        Err(e) => {
                            let mut hash = crate::object::HashObject::default();
                            hash.insert_pair(HashKey::from_string("ok"), Value::from_bool(false));
                            hash.insert_pair(HashKey::from_string("status"), Value::from_i64(0));
                            let err_val = obj_into_val(Object::String(e.into()), &mut self.heap);
                            hash.insert_pair(HashKey::from_string("error"), err_val);
                            let body_val = obj_into_val(Object::String("".into()), &mut self.heap);
                            hash.insert_pair(HashKey::from_string("body"), body_val);
                            Ok(obj_into_val(make_hash(hash), &mut self.heap))
                        }
                    }
                } else {
                    Ok(Value::NULL)
                }
            }
            BuiltinFunction::HttpDelete => {
                if let Some(ref http) = self.http {
                    let url = args.first().map(|v| val_inspect(*v, &self.heap)).unwrap_or_default();
                    match http.delete(&url) {
                        Ok(resp) => {
                            let mut hash = crate::object::HashObject::default();
                            hash.insert_pair(HashKey::from_string("status"), Value::from_i64(resp.status as i64));
                            hash.insert_pair(HashKey::from_string("ok"), Value::from_bool(resp.ok));
                            let body_val = obj_into_val(Object::String(resp.body.into()), &mut self.heap);
                            hash.insert_pair(HashKey::from_string("body"), body_val);
                            Ok(obj_into_val(make_hash(hash), &mut self.heap))
                        }
                        Err(e) => {
                            let mut hash = crate::object::HashObject::default();
                            hash.insert_pair(HashKey::from_string("ok"), Value::from_bool(false));
                            hash.insert_pair(HashKey::from_string("status"), Value::from_i64(0));
                            let err_val = obj_into_val(Object::String(e.into()), &mut self.heap);
                            hash.insert_pair(HashKey::from_string("error"), err_val);
                            let body_val = obj_into_val(Object::String("".into()), &mut self.heap);
                            hash.insert_pair(HashKey::from_string("body"), body_val);
                            Ok(obj_into_val(make_hash(hash), &mut self.heap))
                        }
                    }
                } else {
                    Ok(Value::NULL)
                }
            }

            // ════════════════════════════════════════════════════════════════
            // FS bridge
            // ════════════════════════════════════════════════════════════════
            BuiltinFunction::FsReadFile => {
                if let Some(ref fs) = self.fs {
                    let path = args.first().map(|v| val_inspect(*v, &self.heap)).unwrap_or_default();
                    match fs.read_file(&path) {
                        Ok(content) => Ok(obj_into_val(Object::String(content.into()), &mut self.heap)),
                        Err(_) => Ok(Value::NULL),
                    }
                } else {
                    Ok(Value::NULL)
                }
            }
            BuiltinFunction::FsWriteFile => {
                if let Some(ref mut fs) = self.fs {
                    let path = args.first().map(|v| val_inspect(*v, &self.heap)).unwrap_or_default();
                    let content = args.get(1).map(|v| val_inspect(*v, &self.heap)).unwrap_or_default();
                    match fs.write_file(&path, &content) {
                        Ok(()) => Ok(Value::from_bool(true)),
                        Err(_) => Ok(Value::from_bool(false)),
                    }
                } else {
                    Ok(Value::from_bool(false))
                }
            }
            BuiltinFunction::FsAppendFile => {
                if let Some(ref mut fs) = self.fs {
                    let path = args.first().map(|v| val_inspect(*v, &self.heap)).unwrap_or_default();
                    let content = args.get(1).map(|v| val_inspect(*v, &self.heap)).unwrap_or_default();
                    match fs.append_file(&path, &content) {
                        Ok(()) => Ok(Value::from_bool(true)),
                        Err(_) => Ok(Value::from_bool(false)),
                    }
                } else {
                    Ok(Value::from_bool(false))
                }
            }
            BuiltinFunction::FsExists => {
                if let Some(ref fs) = self.fs {
                    let path = args.first().map(|v| val_inspect(*v, &self.heap)).unwrap_or_default();
                    Ok(Value::from_bool(fs.exists(&path)))
                } else {
                    Ok(Value::from_bool(false))
                }
            }
            BuiltinFunction::FsListDir => {
                if let Some(ref fs) = self.fs {
                    let path = args.first().map(|v| val_inspect(*v, &self.heap)).unwrap_or_default();
                    match fs.list_dir(&path) {
                        Ok(entries) => {
                            let items: Vec<Value> = entries
                                .into_iter()
                                .map(|e| obj_into_val(Object::String(e.into()), &mut self.heap))
                                .collect();
                            Ok(obj_into_val(make_array(items), &mut self.heap))
                        }
                        Err(_) => Ok(obj_into_val(make_array(vec![]), &mut self.heap)),
                    }
                } else {
                    Ok(obj_into_val(make_array(vec![]), &mut self.heap))
                }
            }
            BuiltinFunction::FsDeleteFile => {
                if let Some(ref mut fs) = self.fs {
                    let path = args.first().map(|v| val_inspect(*v, &self.heap)).unwrap_or_default();
                    match fs.delete_file(&path) {
                        Ok(()) => Ok(Value::from_bool(true)),
                        Err(_) => Ok(Value::from_bool(false)),
                    }
                } else {
                    Ok(Value::from_bool(false))
                }
            }
            BuiltinFunction::FsMkdir => {
                if let Some(ref mut fs) = self.fs {
                    let path = args.first().map(|v| val_inspect(*v, &self.heap)).unwrap_or_default();
                    match fs.mkdir(&path) {
                        Ok(()) => Ok(Value::from_bool(true)),
                        Err(_) => Ok(Value::from_bool(false)),
                    }
                } else {
                    Ok(Value::from_bool(false))
                }
            }

            // ════════════════════════════════════════════════════════════════
            // Env bridge
            // ════════════════════════════════════════════════════════════════
            BuiltinFunction::EnvGet => {
                if let Some(ref env) = self.env {
                    let name = args.first().map(|v| val_inspect(*v, &self.heap)).unwrap_or_default();
                    match env.get(&name) {
                        Some(val) => Ok(obj_into_val(Object::String(val.into()), &mut self.heap)),
                        None => Ok(Value::NULL),
                    }
                } else {
                    Ok(Value::NULL)
                }
            }
            BuiltinFunction::EnvKeys => {
                if let Some(ref env) = self.env {
                    let keys = env.keys();
                    let items: Vec<Value> = keys
                        .into_iter()
                        .map(|k| obj_into_val(Object::String(k.into()), &mut self.heap))
                        .collect();
                    Ok(obj_into_val(make_array(items), &mut self.heap))
                } else {
                    Ok(obj_into_val(make_array(vec![]), &mut self.heap))
                }
            }
            BuiltinFunction::ConsoleLog => {
                // console.log(...args): format each arg node-style (space-joined,
                // strings unquoted) and write a line to stdout.
                let line = args
                    .iter()
                    .map(|v| val_inspect(*v, &self.heap))
                    .collect::<Vec<_>>()
                    .join(" ");
                println!("{line}");
                Ok(Value::UNDEFINED)
            }
            BuiltinFunction::EnvLog => {
                if let Some(ref env) = self.env {
                    let level = args.first().map(|v| val_inspect(*v, &self.heap)).unwrap_or_default();
                    let message = args.get(1).map(|v| val_inspect(*v, &self.heap)).unwrap_or_default();
                    env.log(&level, &message);
                }
                Ok(Value::UNDEFINED)
            }

            // ════════════════════════════════════════════════════════════════
            // Draw bridge
            // ════════════════════════════════════════════════════════════════
            BuiltinFunction::DrawRect => {
                if let Some(ref mut d) = self.draw {
                    let x = args.first().map(|v| v.to_number()).unwrap_or(0.0);
                    let y = args.get(1).map(|v| v.to_number()).unwrap_or(0.0);
                    let w = args.get(2).map(|v| v.to_number()).unwrap_or(0.0);
                    let h = args.get(3).map(|v| v.to_number()).unwrap_or(0.0);
                    let fill = args
                        .get(4)
                        .map(|v| val_inspect(*v, &self.heap))
                        .unwrap_or_default();
                    let border_radius = args.get(5).map(|v| v.to_number()).unwrap_or(0.0);
                    let border_width = args.get(6).map(|v| v.to_number()).unwrap_or(0.0);
                    let border_color = args
                        .get(7)
                        .map(|v| val_inspect(*v, &self.heap))
                        .unwrap_or_default();
                    let opacity = args.get(8).map(|v| v.to_number()).unwrap_or(1.0);
                    d.draw_rect(
                        x,
                        y,
                        w,
                        h,
                        &fill,
                        border_radius,
                        border_width,
                        &border_color,
                        opacity,
                    );
                }
                Ok(Value::UNDEFINED)
            }

            BuiltinFunction::DrawRoundedRect => {
                // Calling convention (zipp):
                //   draw.roundedRect(x, y, w, h, fill, borderRadius, borderWidth, borderColor, opacity)
                // This is the same as draw.rect but always uses rounded corners.
                if let Some(ref mut d) = self.draw {
                    let x = args.first().map(|v| v.to_number()).unwrap_or(0.0);
                    let y = args.get(1).map(|v| v.to_number()).unwrap_or(0.0);
                    let w = args.get(2).map(|v| v.to_number()).unwrap_or(0.0);
                    let h = args.get(3).map(|v| v.to_number()).unwrap_or(0.0);
                    let fill = args
                        .get(4)
                        .map(|v| val_inspect(*v, &self.heap))
                        .unwrap_or_default();
                    let border_radius = args.get(5).map(|v| v.to_number()).unwrap_or(0.0);
                    let border_width = args.get(6).map(|v| v.to_number()).unwrap_or(0.0);
                    let border_color = args
                        .get(7)
                        .map(|v| val_inspect(*v, &self.heap))
                        .unwrap_or_default();
                    let opacity = args.get(8).map(|v| v.to_number()).unwrap_or(1.0);
                    d.draw_rect(
                        x,
                        y,
                        w,
                        h,
                        &fill,
                        border_radius,
                        border_width,
                        &border_color,
                        opacity,
                    );
                }
                Ok(Value::UNDEFINED)
            }

            BuiltinFunction::DrawCircle => {
                if let Some(ref mut d) = self.draw {
                    let cx = args.first().map(|v| v.to_number()).unwrap_or(0.0);
                    let cy = args.get(1).map(|v| v.to_number()).unwrap_or(0.0);
                    let r = args.get(2).map(|v| v.to_number()).unwrap_or(0.0);
                    let fill = args
                        .get(3)
                        .map(|v| val_inspect(*v, &self.heap))
                        .unwrap_or_default();
                    let opacity = args.get(4).map(|v| v.to_number()).unwrap_or(1.0);
                    d.draw_circle(cx, cy, r, &fill, opacity);
                }
                Ok(Value::UNDEFINED)
            }

            BuiltinFunction::DrawEllipse => {
                if let Some(ref mut d) = self.draw {
                    let cx = args.first().map(|v| v.to_number()).unwrap_or(0.0);
                    let cy = args.get(1).map(|v| v.to_number()).unwrap_or(0.0);
                    let rx = args.get(2).map(|v| v.to_number()).unwrap_or(0.0);
                    let ry = args.get(3).map(|v| v.to_number()).unwrap_or(0.0);
                    let fill = args
                        .get(4)
                        .map(|v| val_inspect(*v, &self.heap))
                        .unwrap_or_default();
                    let opacity = args.get(5).map(|v| v.to_number()).unwrap_or(1.0);
                    d.draw_ellipse(cx, cy, rx, ry, &fill, opacity);
                }
                Ok(Value::UNDEFINED)
            }

            BuiltinFunction::DrawLine => {
                if let Some(ref mut d) = self.draw {
                    let x1 = args.first().map(|v| v.to_number()).unwrap_or(0.0);
                    let y1 = args.get(1).map(|v| v.to_number()).unwrap_or(0.0);
                    let x2 = args.get(2).map(|v| v.to_number()).unwrap_or(0.0);
                    let y2 = args.get(3).map(|v| v.to_number()).unwrap_or(0.0);
                    let color = args
                        .get(4)
                        .map(|v| val_inspect(*v, &self.heap))
                        .unwrap_or_default();
                    let width = args.get(5).map(|v| v.to_number()).unwrap_or(1.0);
                    d.draw_line(x1, y1, x2, y2, &color, width);
                }
                Ok(Value::UNDEFINED)
            }

            BuiltinFunction::DrawPath => {
                if let Some(ref mut d) = self.draw {
                    let commands = args
                        .first()
                        .map(|v| val_inspect(*v, &self.heap))
                        .unwrap_or_default();
                    let fill = args
                        .get(1)
                        .map(|v| val_inspect(*v, &self.heap))
                        .unwrap_or_default();
                    let stroke = args
                        .get(2)
                        .map(|v| val_inspect(*v, &self.heap))
                        .unwrap_or_default();
                    let stroke_width = args.get(3).map(|v| v.to_number()).unwrap_or(0.0);
                    let opacity = args.get(4).map(|v| v.to_number()).unwrap_or(1.0);
                    d.draw_path(&commands, &fill, &stroke, stroke_width, opacity);
                }
                Ok(Value::UNDEFINED)
            }

            BuiltinFunction::DrawText => {
                // Calling convention (zipp):
                //   draw.text(text, x, y, fontSize, fontWeight, color,
                //             fontFamily, maxWidth, opacity)
                if let Some(ref mut d) = self.draw {
                    let text = args
                        .first()
                        .map(|v| val_inspect(*v, &self.heap))
                        .unwrap_or_default();
                    let x = args.get(1).map(|v| v.to_number()).unwrap_or(0.0);
                    let y = args.get(2).map(|v| v.to_number()).unwrap_or(0.0);
                    let font_size = args.get(3).map(|v| v.to_number()).unwrap_or(14.0);
                    let font_weight = args.get(4).map(|v| v.to_number() as u32).unwrap_or(400);
                    let color = args
                        .get(5)
                        .map(|v| val_inspect(*v, &self.heap))
                        .unwrap_or_default();
                    let font_family = args
                        .get(6)
                        .map(|v| val_inspect(*v, &self.heap))
                        .unwrap_or_default();
                    let max_width = args.get(7).map(|v| v.to_number()).unwrap_or(f64::INFINITY);
                    let opacity = args.get(8).map(|v| v.to_number()).unwrap_or(1.0);

                    // Apply opacity layer if needed
                    if opacity < 1.0 && opacity > 0.0 {
                        d.push_opacity(opacity);
                    }

                    let (w, h) = d.draw_text(
                        &text,
                        x,
                        y,
                        font_size,
                        &color,
                        font_weight,
                        &font_family,
                        max_width,
                        0.0, // letter_spacing (not used by zipp stdlib)
                    );

                    if opacity < 1.0 && opacity > 0.0 {
                        d.pop_opacity();
                    }

                    // Return {width, height} object
                    let mut hash = HashObject::default();
                    hash.insert_pair(HashKey::from_string("width"), Value::from_f64(w));
                    hash.insert_pair(HashKey::from_string("height"), Value::from_f64(h));
                    Ok(obj_into_val(make_hash(hash), &mut self.heap))
                } else {
                    Ok(Value::UNDEFINED)
                }
            }

            BuiltinFunction::DrawImage => {
                if let Some(ref mut d) = self.draw {
                    let src = args
                        .first()
                        .map(|v| val_inspect(*v, &self.heap))
                        .unwrap_or_default();
                    let x = args.get(1).map(|v| v.to_number()).unwrap_or(0.0);
                    let y = args.get(2).map(|v| v.to_number()).unwrap_or(0.0);
                    let w = args.get(3).map(|v| v.to_number()).unwrap_or(0.0);
                    let h = args.get(4).map(|v| v.to_number()).unwrap_or(0.0);
                    let opacity = args.get(5).map(|v| v.to_number()).unwrap_or(1.0);
                    d.draw_image(&src, x, y, w, h, opacity);
                }
                Ok(Value::UNDEFINED)
            }

            BuiltinFunction::DrawLinearGradient => {
                let stops = self.extract_f64_vec(args.get(5).copied());
                if let Some(ref mut d) = self.draw {
                    let x = args.first().map(|v| v.to_number()).unwrap_or(0.0);
                    let y = args.get(1).map(|v| v.to_number()).unwrap_or(0.0);
                    let w = args.get(2).map(|v| v.to_number()).unwrap_or(0.0);
                    let h = args.get(3).map(|v| v.to_number()).unwrap_or(0.0);
                    let angle_deg = args.get(4).map(|v| v.to_number()).unwrap_or(0.0);
                    let border_radius = args.get(6).map(|v| v.to_number()).unwrap_or(0.0);
                    d.draw_linear_gradient(x, y, w, h, angle_deg, &stops, border_radius);
                }
                Ok(Value::UNDEFINED)
            }

            BuiltinFunction::DrawRadialGradient => {
                let stops = self.extract_f64_vec(args.get(4).copied());
                if let Some(ref mut d) = self.draw {
                    let x = args.first().map(|v| v.to_number()).unwrap_or(0.0);
                    let y = args.get(1).map(|v| v.to_number()).unwrap_or(0.0);
                    let w = args.get(2).map(|v| v.to_number()).unwrap_or(0.0);
                    let h = args.get(3).map(|v| v.to_number()).unwrap_or(0.0);
                    let border_radius = args.get(5).map(|v| v.to_number()).unwrap_or(0.0);
                    d.draw_radial_gradient(x, y, w, h, &stops, border_radius);
                }
                Ok(Value::UNDEFINED)
            }

            BuiltinFunction::DrawShadow => {
                if let Some(ref mut d) = self.draw {
                    let x = args.first().map(|v| v.to_number()).unwrap_or(0.0);
                    let y = args.get(1).map(|v| v.to_number()).unwrap_or(0.0);
                    let w = args.get(2).map(|v| v.to_number()).unwrap_or(0.0);
                    let h = args.get(3).map(|v| v.to_number()).unwrap_or(0.0);
                    let blur = args.get(4).map(|v| v.to_number()).unwrap_or(0.0);
                    let spread = args.get(5).map(|v| v.to_number()).unwrap_or(0.0);
                    let color = args
                        .get(6)
                        .map(|v| val_inspect(*v, &self.heap))
                        .unwrap_or_default();
                    let offset_x = args.get(7).map(|v| v.to_number()).unwrap_or(0.0);
                    let offset_y = args.get(8).map(|v| v.to_number()).unwrap_or(0.0);
                    let border_radius = args.get(9).map(|v| v.to_number()).unwrap_or(0.0);
                    d.draw_shadow(
                        x,
                        y,
                        w,
                        h,
                        blur,
                        spread,
                        &color,
                        offset_x,
                        offset_y,
                        border_radius,
                    );
                }
                Ok(Value::UNDEFINED)
            }

            BuiltinFunction::DrawPushClip => {
                if let Some(ref mut d) = self.draw {
                    let x = args.first().map(|v| v.to_number()).unwrap_or(0.0);
                    let y = args.get(1).map(|v| v.to_number()).unwrap_or(0.0);
                    let w = args.get(2).map(|v| v.to_number()).unwrap_or(0.0);
                    let h = args.get(3).map(|v| v.to_number()).unwrap_or(0.0);
                    let border_radius = args.get(4).map(|v| v.to_number()).unwrap_or(0.0);
                    d.push_clip(x, y, w, h, border_radius);
                }
                Ok(Value::UNDEFINED)
            }

            BuiltinFunction::DrawPopClip => {
                if let Some(ref mut d) = self.draw {
                    d.pop_clip();
                }
                Ok(Value::UNDEFINED)
            }

            BuiltinFunction::DrawPushTransform => {
                if let Some(ref mut d) = self.draw {
                    let translate_x = args.first().map(|v| v.to_number()).unwrap_or(0.0);
                    let translate_y = args.get(1).map(|v| v.to_number()).unwrap_or(0.0);
                    let rotate_deg = args.get(2).map(|v| v.to_number()).unwrap_or(0.0);
                    let scale_x = args.get(3).map(|v| v.to_number()).unwrap_or(1.0);
                    let scale_y = args.get(4).map(|v| v.to_number()).unwrap_or(1.0);
                    d.push_transform(translate_x, translate_y, rotate_deg, scale_x, scale_y);
                }
                Ok(Value::UNDEFINED)
            }

            BuiltinFunction::DrawPopTransform => {
                if let Some(ref mut d) = self.draw {
                    d.pop_transform();
                }
                Ok(Value::UNDEFINED)
            }

            BuiltinFunction::DrawPushOpacity => {
                if let Some(ref mut d) = self.draw {
                    let opacity = args.first().map(|v| v.to_number()).unwrap_or(1.0);
                    d.push_opacity(opacity);
                }
                Ok(Value::UNDEFINED)
            }

            BuiltinFunction::DrawPopOpacity => {
                if let Some(ref mut d) = self.draw {
                    d.pop_opacity();
                }
                Ok(Value::UNDEFINED)
            }

            BuiltinFunction::DrawArc => {
                if let Some(ref mut d) = self.draw {
                    let cx = args.first().map(|v| v.to_number()).unwrap_or(0.0);
                    let cy = args.get(1).map(|v| v.to_number()).unwrap_or(0.0);
                    let radius = args.get(2).map(|v| v.to_number()).unwrap_or(0.0);
                    let thickness = args.get(3).map(|v| v.to_number()).unwrap_or(0.0);
                    let start_angle = args.get(4).map(|v| v.to_number()).unwrap_or(0.0);
                    let end_angle = args.get(5).map(|v| v.to_number()).unwrap_or(0.0);
                    let color = args
                        .get(6)
                        .map(|v| val_inspect(*v, &self.heap))
                        .unwrap_or_default();
                    d.draw_arc(cx, cy, radius, thickness, start_angle, end_angle, &color);
                }
                Ok(Value::UNDEFINED)
            }

            BuiltinFunction::DrawMeasureText => {
                if let Some(ref d) = self.draw {
                    let text = args
                        .first()
                        .map(|v| val_inspect(*v, &self.heap))
                        .unwrap_or_default();
                    let font_size = args.get(1).map(|v| v.to_number()).unwrap_or(14.0);
                    let font_weight = args.get(2).map(|v| v.to_number() as u32).unwrap_or(400);
                    let font_family = args
                        .get(3)
                        .map(|v| val_inspect(*v, &self.heap))
                        .unwrap_or_default();
                    let max_width = args.get(4).map(|v| v.to_number()).unwrap_or(f64::INFINITY);
                    let (w, h) =
                        d.measure_text(&text, font_size, font_weight, &font_family, max_width);
                    let mut hash = HashObject::default();
                    hash.insert_pair(HashKey::from_string("width"), Value::from_f64(w));
                    hash.insert_pair(HashKey::from_string("height"), Value::from_f64(h));
                    Ok(obj_into_val(make_hash(hash), &mut self.heap))
                } else {
                    Ok(Value::UNDEFINED)
                }
            }

            BuiltinFunction::DrawGetViewportWidth => {
                if let Some(ref d) = self.draw {
                    Ok(Value::from_f64(d.get_viewport_width()))
                } else {
                    Ok(Value::from_f64(0.0))
                }
            }

            BuiltinFunction::DrawGetViewportHeight => {
                if let Some(ref d) = self.draw {
                    Ok(Value::from_f64(d.get_viewport_height()))
                } else {
                    Ok(Value::from_f64(0.0))
                }
            }

            // ════════════════════════════════════════════════════════════════
            // Layout bridge
            // ════════════════════════════════════════════════════════════════
            BuiltinFunction::LayoutCreateNode => {
                let style = self.extract_layout_style(args.first().copied());
                if let Some(ref mut lay) = self.layout {
                    let id = lay.create_node(style);
                    Ok(Value::from_f64(id as f64))
                } else {
                    Ok(Value::from_f64(0.0))
                }
            }

            BuiltinFunction::LayoutSetChildren => {
                let children = self.extract_u64_vec(args.get(1).copied());
                if let Some(ref mut lay) = self.layout {
                    let parent = args.first().map(|v| v.to_number() as u64).unwrap_or(0);
                    lay.set_children(parent, &children);
                }
                Ok(Value::UNDEFINED)
            }

            BuiltinFunction::LayoutComputeLayout => {
                if let Some(ref mut lay) = self.layout {
                    let root = args.first().map(|v| v.to_number() as u64).unwrap_or(0);
                    let avail_w = args.get(1).map(|v| v.to_number()).unwrap_or(0.0);
                    let avail_h = args.get(2).map(|v| v.to_number()).unwrap_or(0.0);
                    lay.compute_layout(root, avail_w, avail_h);
                }
                Ok(Value::UNDEFINED)
            }

            BuiltinFunction::LayoutGetLayout => {
                if let Some(ref lay) = self.layout {
                    let node = args.first().map(|v| v.to_number() as u64).unwrap_or(0);
                    let (x, y, w, h) = lay.get_layout(node);
                    let mut hash = HashObject::default();
                    hash.insert_pair(HashKey::from_string("x"), Value::from_f64(x));
                    hash.insert_pair(HashKey::from_string("y"), Value::from_f64(y));
                    hash.insert_pair(HashKey::from_string("width"), Value::from_f64(w));
                    hash.insert_pair(HashKey::from_string("height"), Value::from_f64(h));
                    Ok(obj_into_val(make_hash(hash), &mut self.heap))
                } else {
                    Ok(Value::UNDEFINED)
                }
            }

            BuiltinFunction::LayoutRemoveNode => {
                if let Some(ref mut lay) = self.layout {
                    let node = args.first().map(|v| v.to_number() as u64).unwrap_or(0);
                    lay.remove_node(node);
                }
                Ok(Value::UNDEFINED)
            }

            BuiltinFunction::LayoutClear => {
                if let Some(ref mut lay) = self.layout {
                    lay.clear();
                }
                Ok(Value::UNDEFINED)
            }

            BuiltinFunction::LayoutUpdateStyle => {
                // args[0] = node_id (f64), args[1] = style object
                let node_id = args
                    .first()
                    .copied()
                    .unwrap_or(Value::from_f64(0.0))
                    .to_number() as u64;
                let style = self.extract_layout_style(args.get(1).copied());
                if let Some(ref mut lay) = self.layout {
                    lay.update_style(node_id, style);
                }
                Ok(Value::UNDEFINED)
            }

            // ════════════════════════════════════════════════════════════════
            // Input bridge
            // ════════════════════════════════════════════════════════════════
            BuiltinFunction::InputGetMouseX => {
                if let Some(ref inp) = self.input {
                    Ok(Value::from_f64(inp.get_mouse_x()))
                } else {
                    Ok(Value::from_f64(0.0))
                }
            }

            BuiltinFunction::InputGetMouseY => {
                if let Some(ref inp) = self.input {
                    Ok(Value::from_f64(inp.get_mouse_y()))
                } else {
                    Ok(Value::from_f64(0.0))
                }
            }

            BuiltinFunction::InputIsMouseDown => {
                if let Some(ref inp) = self.input {
                    Ok(Value::from_bool(inp.is_mouse_down()))
                } else {
                    Ok(Value::FALSE)
                }
            }

            BuiltinFunction::InputIsMousePressed => {
                if let Some(ref inp) = self.input {
                    Ok(Value::from_bool(inp.is_mouse_pressed()))
                } else {
                    Ok(Value::FALSE)
                }
            }

            BuiltinFunction::InputIsMouseReleased => {
                if let Some(ref inp) = self.input {
                    Ok(Value::from_bool(inp.is_mouse_released()))
                } else {
                    Ok(Value::FALSE)
                }
            }

            BuiltinFunction::InputGetScrollY => {
                if let Some(ref inp) = self.input {
                    Ok(Value::from_f64(inp.get_scroll_y()))
                } else {
                    Ok(Value::from_f64(0.0))
                }
            }

            BuiltinFunction::InputSetCursor => {
                if let Some(ref mut inp) = self.input {
                    let cursor = args
                        .first()
                        .map(|v| val_inspect(*v, &self.heap))
                        .unwrap_or_default();
                    inp.set_cursor(&cursor);
                }
                Ok(Value::UNDEFINED)
            }

            BuiltinFunction::InputGetTextInput => {
                if let Some(ref inp) = self.input {
                    let text = inp.get_text_input();
                    if text.is_empty() {
                        Ok(obj_into_val(Object::String("".into()), &mut self.heap))
                    } else {
                        Ok(obj_into_val(Object::String(text.into()), &mut self.heap))
                    }
                } else {
                    Ok(obj_into_val(Object::String("".into()), &mut self.heap))
                }
            }

            BuiltinFunction::InputIsBackspacePressed => {
                if let Some(ref inp) = self.input {
                    Ok(Value::from_bool(inp.is_backspace_pressed()))
                } else {
                    Ok(Value::FALSE)
                }
            }

            BuiltinFunction::InputIsEscapePressed => {
                if let Some(ref inp) = self.input {
                    Ok(Value::from_bool(inp.is_escape_pressed()))
                } else {
                    Ok(Value::FALSE)
                }
            }

            BuiltinFunction::InputRequestRedraw => {
                if let Some(ref mut inp) = self.input {
                    inp.request_redraw();
                }
                Ok(Value::UNDEFINED)
            }

            BuiltinFunction::InputGetElapsedSecs => {
                if let Some(ref inp) = self.input {
                    Ok(Value::from_f64(inp.get_elapsed_secs()))
                } else {
                    Ok(Value::from_f64(0.0))
                }
            }

            BuiltinFunction::InputGetPageElapsedSecs => {
                if let Some(ref inp) = self.input {
                    Ok(Value::from_f64(inp.get_page_elapsed_secs()))
                } else {
                    Ok(Value::from_f64(0.0))
                }
            }

            BuiltinFunction::InputGetDeltaTime => {
                if let Some(ref inp) = self.input {
                    Ok(Value::from_f64(inp.get_delta_time()))
                } else {
                    Ok(Value::from_f64(0.0))
                }
            }

            BuiltinFunction::InputGetFocusedInput => {
                if let Some(ref inp) = self.input {
                    match inp.get_focused_input() {
                        Some(name) => {
                            let s = Object::String(name.into());
                            Ok(obj_into_val(s, &mut self.heap))
                        }
                        None => Ok(Value::NULL),
                    }
                } else {
                    Ok(Value::NULL)
                }
            }

            BuiltinFunction::InputSetFocusedInput => {
                if let Some(ref mut inp) = self.input {
                    let val = args.first().copied();
                    if let Some(v) = val {
                        if v.is_null() || v.is_undefined() {
                            inp.set_focused_input(None);
                        } else {
                            let name = val_inspect(v, &self.heap);
                            inp.set_focused_input(Some(&name));
                        }
                    } else {
                        inp.set_focused_input(None);
                    }
                }
                Ok(Value::UNDEFINED)
            }

            BuiltinFunction::InputIsKeyDown => {
                if let Some(ref inp) = self.input {
                    let key = args
                        .first()
                        .map(|v| val_inspect(*v, &self.heap))
                        .unwrap_or_default();
                    Ok(Value::from_bool(inp.is_key_down(&key)))
                } else {
                    Ok(Value::FALSE)
                }
            }

            // ── Window event bridge ──
            BuiltinFunction::WindowAddEventListener => {
                // args: (event_type: string, handler: function)
                let event_type = args.first().map(|v| val_inspect(*v, &self.heap)).unwrap_or_default();
                if let Some(&handler_val) = args.get(1) {
                    self.event_listeners
                        .entry(event_type)
                        .or_default()
                        .push(handler_val);
                }
                Ok(Value::UNDEFINED)
            }
            BuiltinFunction::WindowRemoveEventListener => {
                // args: (event_type: string, handler: function)
                let event_type = args.first().map(|v| val_inspect(*v, &self.heap)).unwrap_or_default();
                if let Some(&handler_val) = args.get(1) {
                    if let Some(listeners) = self.event_listeners.get_mut(&event_type) {
                        listeners.retain(|v| *v != handler_val);
                    }
                }
                Ok(Value::UNDEFINED)
            }
            BuiltinFunction::EventPreventDefault | BuiltinFunction::EventStopPropagation => {
                // No-ops in native runtime
                Ok(Value::UNDEFINED)
            }

            // ── URI encoding ──
            BuiltinFunction::EncodeURIComponent => {
                let input = args
                    .first()
                    .map(|v| val_inspect(*v, &self.heap))
                    .unwrap_or_default();
                let encoded = uri_encode(&input, false);
                Ok(obj_into_val(Object::String(encoded.into()), &mut self.heap))
            }
            BuiltinFunction::DecodeURIComponent => {
                let input = args
                    .first()
                    .map(|v| val_inspect(*v, &self.heap))
                    .unwrap_or_default();
                let decoded = uri_decode(&input);
                Ok(obj_into_val(Object::String(decoded.into()), &mut self.heap))
            }
            BuiltinFunction::EncodeURI => {
                let input = args
                    .first()
                    .map(|v| val_inspect(*v, &self.heap))
                    .unwrap_or_default();
                let encoded = uri_encode(&input, true);
                Ok(obj_into_val(Object::String(encoded.into()), &mut self.heap))
            }
            BuiltinFunction::DecodeURI => {
                let input = args
                    .first()
                    .map(|v| val_inspect(*v, &self.heap))
                    .unwrap_or_default();
                let decoded = uri_decode(&input);
                Ok(obj_into_val(Object::String(decoded.into()), &mut self.heap))
            }

            // ── Generic host call bridge ──
            BuiltinFunction::HostCall => {
                // host.call(kind, argsArray, callback)
                let kind = args.first().map(|v| val_inspect(*v, &self.heap)).unwrap_or_default();
                // Second arg is an array of string arguments
                let mut call_args = Vec::new();
                if let Some(&arr_val) = args.get(1) {
                    if arr_val.is_heap() {
                        if let Object::Array(ref items) = self.heap.get(arr_val.heap_index()) {
                            for item in items.borrow().iter() {
                                call_args.push(val_inspect(*item, &self.heap));
                            }
                        }
                    }
                }
                // Third arg is the callback function
                let callback = args.get(2).copied().unwrap_or(Value::UNDEFINED);
                self.queue_host_call(&kind, call_args, callback)?;
                Ok(Value::UNDEFINED)
            }
            BuiltinFunction::HostCallSync => {
                // host.callSync(kind, argsArray) — synchronous host call, returns parsed JSON
                // Limit host calls per execution to prevent thread pool exhaustion
                self.host_call_count += 1;
                if self.host_call_count > 100 {
                    return Err(VMError::TypeError(
                        "Too many host.callSync calls (max 100 per execution)".into(),
                    ));
                }
                let kind = args.first().map(|v| val_inspect(*v, &self.heap)).unwrap_or_default();
                let mut call_args: Vec<String> = Vec::new();
                let mut total_bytes: usize = kind.len();
                if let Some(&arr_val) = args.get(1) {
                    if arr_val.is_heap() {
                        if let Object::Array(ref items) = self.heap.get(arr_val.heap_index()) {
                            for item in items.borrow().iter() {
                                let s = val_inspect(*item, &self.heap);
                                total_bytes = total_bytes.saturating_add(s.len());
                                if total_bytes > Self::MAX_HOST_CALL_ARGS_BYTES {
                                    return Err(VMError::ExecutionTimeout(format!(
                                        "host.callSync argument payload exceeds {}B",
                                        Self::MAX_HOST_CALL_ARGS_BYTES
                                    )));
                                }
                                call_args.push(s);
                            }
                        }
                    } else {
                        // Single string argument
                        let s = val_inspect(arr_val, &self.heap);
                        total_bytes = total_bytes.saturating_add(s.len());
                        if total_bytes > Self::MAX_HOST_CALL_ARGS_BYTES {
                            return Err(VMError::ExecutionTimeout(format!(
                                "host.callSync argument payload exceeds {}B",
                                Self::MAX_HOST_CALL_ARGS_BYTES
                            )));
                        }
                        call_args.push(s);
                    }
                }
                // Re-check wall-time and abort flag before handing control to
                // the host. The handler runs synchronously — no dispatch-loop
                // opcode ticks inside it — so without this a script sitting
                // between its 100-call quota can still pay the host cost of
                // 100 slow handler invocations while ignoring a stale
                // max_wall_time_ms.
                self.check_builtin_callback_limits()?;
                if let Some(ref handler) = self.config.sync_host_call {
                    match handler(&kind, &call_args) {
                        Ok(json_result) => {
                            // Parse JSON result into a VM value
                            match serde_json::from_str::<serde_json::Value>(&json_result) {
                                Ok(val) => Ok(json_value_to_vm_value(val, &mut self.heap)),
                                Err(_) => Ok(obj_into_val(Object::String(Rc::from(json_result.as_str())), &mut self.heap)),
                            }
                        }
                        Err(e) => Err(VMError::TypeError(format!("host.callSync error: {e}"))),
                    }
                } else {
                    Err(VMError::TypeError("host.callSync: no sync handler configured".into()))
                }
            }
            // ── ArrayBuffer / TypedArray / DataView ─────────────────────
            BuiltinFunction::ArrayBufferCtor => {
                let len = args
                    .first()
                    .map(|v| self.to_number_val(*v))
                    .transpose()?
                    .unwrap_or(0.0) as usize;
                let buf = std::rc::Rc::new(crate::object::VmCell::new(vec![0u8; len]));
                Ok(obj_into_val(Object::ArrayBuffer(buf), &mut self.heap))
            }
            BuiltinFunction::Int8ArrayCtor
            | BuiltinFunction::Uint8ArrayCtor
            | BuiltinFunction::Uint8ClampedArrayCtor
            | BuiltinFunction::Int16ArrayCtor
            | BuiltinFunction::Uint16ArrayCtor
            | BuiltinFunction::Int32ArrayCtor
            | BuiltinFunction::Uint32ArrayCtor
            | BuiltinFunction::Float32ArrayCtor
            | BuiltinFunction::Float64ArrayCtor => {
                let kind = match builtin.function {
                    BuiltinFunction::Int8ArrayCtor => crate::object::TypedArrayKind::Int8,
                    BuiltinFunction::Uint8ArrayCtor => crate::object::TypedArrayKind::Uint8,
                    BuiltinFunction::Uint8ClampedArrayCtor => {
                        crate::object::TypedArrayKind::Uint8Clamped
                    }
                    BuiltinFunction::Int16ArrayCtor => crate::object::TypedArrayKind::Int16,
                    BuiltinFunction::Uint16ArrayCtor => crate::object::TypedArrayKind::Uint16,
                    BuiltinFunction::Int32ArrayCtor => crate::object::TypedArrayKind::Int32,
                    BuiltinFunction::Uint32ArrayCtor => crate::object::TypedArrayKind::Uint32,
                    BuiltinFunction::Float32ArrayCtor => crate::object::TypedArrayKind::Float32,
                    BuiltinFunction::Float64ArrayCtor => crate::object::TypedArrayKind::Float64,
                    _ => unreachable!(),
                };
                let bpe = kind.bytes_per_element();
                let arg0 = args.first().copied();
                let arg0_obj = arg0.map(|v| val_to_obj(v, &self.heap));
                let (buffer, byte_offset, length) = match arg0_obj {
                    None => {
                        let buf =
                            std::rc::Rc::new(crate::object::VmCell::new(Vec::<u8>::new()));
                        (buf, 0usize, 0usize)
                    }
                    Some(Object::ArrayBuffer(buf)) => {
                        let byte_offset = args
                            .get(1)
                            .map(|v| self.to_number_val(*v))
                            .transpose()?
                            .unwrap_or(0.0) as usize;
                        let buf_len = buf.borrow().len();
                        let length = match args.get(2) {
                            Some(v) => self.to_number_val(*v)? as usize,
                            None => (buf_len.saturating_sub(byte_offset)) / bpe,
                        };
                        (buf, byte_offset, length)
                    }
                    Some(Object::Array(items)) => {
                        let items = unwrap_array(items);
                        let length = items.len();
                        let mut bytes = vec![0u8; length * bpe];
                        for (i, v) in items.iter().enumerate() {
                            let n = self.to_number_val(*v)?;
                            kind.write(&mut bytes, i * bpe, n);
                        }
                        let buf = std::rc::Rc::new(crate::object::VmCell::new(bytes));
                        (buf, 0usize, length)
                    }
                    Some(Object::TypedArray(t)) => {
                        // Copy from another typed array (potentially different kind)
                        let length = t.length;
                        let mut bytes = vec![0u8; length * bpe];
                        let src_buf = t.buffer.borrow();
                        let src_bpe = t.kind.bytes_per_element();
                        for i in 0..length {
                            let src_off = t.byte_offset + i * src_bpe;
                            let n = if src_off + src_bpe <= src_buf.len() {
                                t.kind.read(&src_buf, src_off)
                            } else {
                                0.0
                            };
                            kind.write(&mut bytes, i * bpe, n);
                        }
                        drop(src_buf);
                        let buf = std::rc::Rc::new(crate::object::VmCell::new(bytes));
                        (buf, 0usize, length)
                    }
                    Some(other) => {
                        // Number argument → length
                        let length = match other {
                            Object::Integer(v) => v.max(0) as usize,
                            Object::Float(v) => v.max(0.0) as usize,
                            _ => 0,
                        };
                        let buf =
                            std::rc::Rc::new(crate::object::VmCell::new(vec![0u8; length * bpe]));
                        (buf, 0usize, length)
                    }
                };
                let ta = Object::TypedArray(Box::new(crate::object::TypedArrayObject {
                    buffer,
                    byte_offset,
                    length,
                    kind,
                }));
                Ok(obj_into_val(ta, &mut self.heap))
            }
            BuiltinFunction::DataViewCtor => {
                let arg0 = args.first().copied().unwrap_or(Value::UNDEFINED);
                let arg0_obj = val_to_obj(arg0, &self.heap);
                let buffer = match arg0_obj {
                    Object::ArrayBuffer(b) => b,
                    _ => {
                        return Err(VMError::TypeError(
                            "DataView requires an ArrayBuffer".to_string(),
                        ))
                    }
                };
                let byte_offset = args
                    .get(1)
                    .map(|v| self.to_number_val(*v))
                    .transpose()?
                    .unwrap_or(0.0) as usize;
                let buf_len = buffer.borrow().len();
                let byte_length = match args.get(2) {
                    Some(v) => self.to_number_val(*v)? as usize,
                    None => buf_len.saturating_sub(byte_offset),
                };
                let dv = Object::DataView(Box::new(crate::object::DataViewObject {
                    buffer,
                    byte_offset,
                    byte_length,
                }));
                Ok(obj_into_val(dv, &mut self.heap))
            }
            BuiltinFunction::TypedArraySet => {
                // ta.set(src, [offset])
                let receiver = builtin.receiver.clone().ok_or_else(|| {
                    VMError::TypeError("TypedArray.set missing receiver".to_string())
                })?;
                let ta = match receiver {
                    Object::TypedArray(t) => t,
                    _ => {
                        return Err(VMError::TypeError(
                            "TypedArray.set called on non-TypedArray".to_string(),
                        ))
                    }
                };
                let offset = args
                    .get(1)
                    .map(|v| self.to_number_val(*v))
                    .transpose()?
                    .unwrap_or(0.0) as usize;
                let src_val = args.first().copied().unwrap_or(Value::UNDEFINED);
                let src_obj = val_to_obj(src_val, &self.heap);
                let bpe = ta.kind.bytes_per_element();
                // Pre-collect numeric values *before* taking the buffer's
                // mutable borrow — to_number_val needs &self.heap and the
                // borrow checker can't prove it doesn't touch the same RefCell.
                match src_obj {
                    Object::Array(items) => {
                        let nums: Vec<f64> = {
                            let items = items.borrow();
                            let mut out = Vec::with_capacity(items.len());
                            for v in items.iter() {
                                out.push(self.to_number_val(*v)?);
                            }
                            out
                        };
                        let mut buf = unsafe { ta.buffer.borrow_mut() };
                        for (i, n) in nums.into_iter().enumerate() {
                            let off = ta.byte_offset + (offset + i) * bpe;
                            if off + bpe > buf.len() {
                                break;
                            }
                            ta.kind.write(&mut buf, off, n);
                        }
                    }
                    Object::TypedArray(src) => {
                        let mut buf = unsafe { ta.buffer.borrow_mut() };
                        let src_buf = src.buffer.borrow();
                        for i in 0..src.length {
                            let dst_off = ta.byte_offset + (offset + i) * bpe;
                            if dst_off + bpe > buf.len() {
                                break;
                            }
                            let src_off = src.byte_offset + i * src.kind.bytes_per_element();
                            let n = src.kind.read(&src_buf, src_off);
                            ta.kind.write(&mut buf, dst_off, n);
                        }
                    }
                    _ => {}
                }
                Ok(Value::UNDEFINED)
            }
            BuiltinFunction::TypedArraySubarray => {
                let receiver = builtin.receiver.clone().ok_or_else(|| {
                    VMError::TypeError("TypedArray.subarray missing receiver".to_string())
                })?;
                let ta = match receiver {
                    Object::TypedArray(t) => t,
                    _ => return Ok(Value::UNDEFINED),
                };
                let begin = args
                    .first()
                    .map(|v| self.to_number_val(*v))
                    .transpose()?
                    .unwrap_or(0.0) as i64;
                let end = args
                    .get(1)
                    .map(|v| self.to_number_val(*v))
                    .transpose()?
                    .unwrap_or(ta.length as f64) as i64;
                let len = ta.length as i64;
                let begin = if begin < 0 { (begin + len).max(0) } else { begin.min(len) };
                let end = if end < 0 { (end + len).max(0) } else { end.min(len) };
                let new_len = (end - begin).max(0) as usize;
                let bpe = ta.kind.bytes_per_element();
                let new_ta = Object::TypedArray(Box::new(crate::object::TypedArrayObject {
                    buffer: ta.buffer.clone(),
                    byte_offset: ta.byte_offset + (begin as usize) * bpe,
                    length: new_len,
                    kind: ta.kind,
                }));
                Ok(obj_into_val(new_ta, &mut self.heap))
            }
            // ── DataView accessors ─────────────────────────────────────
            // Each pair shares one helper match arm. Endianness defaults
            // to big-endian to match the JS spec; pass `true` for LE.
            BuiltinFunction::DataViewGetInt8
            | BuiltinFunction::DataViewGetUint8
            | BuiltinFunction::DataViewGetInt16
            | BuiltinFunction::DataViewGetUint16
            | BuiltinFunction::DataViewGetInt32
            | BuiltinFunction::DataViewGetUint32
            | BuiltinFunction::DataViewGetFloat32
            | BuiltinFunction::DataViewGetFloat64 => {
                let receiver = builtin.receiver.clone().ok_or_else(|| {
                    VMError::TypeError("DataView accessor missing receiver".to_string())
                })?;
                let dv = match receiver {
                    Object::DataView(d) => d,
                    _ => return Ok(Value::UNDEFINED),
                };
                let off = args
                    .first()
                    .map(|v| self.to_number_val(*v))
                    .transpose()?
                    .unwrap_or(0.0) as usize;
                let little = args
                    .get(1)
                    .map(|v| !v.is_undefined() && !v.is_null() && v.is_truthy_full(&self.heap))
                    .unwrap_or(false);
                let abs_off = dv.byte_offset + off;
                let buf = dv.buffer.borrow();
                let val = match builtin.function {
                    BuiltinFunction::DataViewGetInt8 => buf[abs_off] as i8 as f64,
                    BuiltinFunction::DataViewGetUint8 => buf[abs_off] as f64,
                    BuiltinFunction::DataViewGetInt16 => {
                        let bytes = [buf[abs_off], buf[abs_off + 1]];
                        if little { i16::from_le_bytes(bytes) as f64 }
                        else { i16::from_be_bytes(bytes) as f64 }
                    }
                    BuiltinFunction::DataViewGetUint16 => {
                        let bytes = [buf[abs_off], buf[abs_off + 1]];
                        if little { u16::from_le_bytes(bytes) as f64 }
                        else { u16::from_be_bytes(bytes) as f64 }
                    }
                    BuiltinFunction::DataViewGetInt32 => {
                        let bytes = [buf[abs_off], buf[abs_off + 1], buf[abs_off + 2], buf[abs_off + 3]];
                        if little { i32::from_le_bytes(bytes) as f64 }
                        else { i32::from_be_bytes(bytes) as f64 }
                    }
                    BuiltinFunction::DataViewGetUint32 => {
                        let bytes = [buf[abs_off], buf[abs_off + 1], buf[abs_off + 2], buf[abs_off + 3]];
                        if little { u32::from_le_bytes(bytes) as f64 }
                        else { u32::from_be_bytes(bytes) as f64 }
                    }
                    BuiltinFunction::DataViewGetFloat32 => {
                        let bytes = [buf[abs_off], buf[abs_off + 1], buf[abs_off + 2], buf[abs_off + 3]];
                        if little { f32::from_le_bytes(bytes) as f64 }
                        else { f32::from_be_bytes(bytes) as f64 }
                    }
                    BuiltinFunction::DataViewGetFloat64 => {
                        let bytes = [
                            buf[abs_off], buf[abs_off + 1], buf[abs_off + 2], buf[abs_off + 3],
                            buf[abs_off + 4], buf[abs_off + 5], buf[abs_off + 6], buf[abs_off + 7],
                        ];
                        if little { f64::from_le_bytes(bytes) }
                        else { f64::from_be_bytes(bytes) }
                    }
                    _ => unreachable!(),
                };
                if val.is_finite() && val.fract() == 0.0 {
                    Ok(Value::from_i64(val as i64))
                } else {
                    Ok(Value::from_f64(val))
                }
            }
            BuiltinFunction::DataViewSetInt8
            | BuiltinFunction::DataViewSetUint8
            | BuiltinFunction::DataViewSetInt16
            | BuiltinFunction::DataViewSetUint16
            | BuiltinFunction::DataViewSetInt32
            | BuiltinFunction::DataViewSetUint32
            | BuiltinFunction::DataViewSetFloat32
            | BuiltinFunction::DataViewSetFloat64 => {
                let receiver = builtin.receiver.clone().ok_or_else(|| {
                    VMError::TypeError("DataView setter missing receiver".to_string())
                })?;
                let dv = match receiver {
                    Object::DataView(d) => d,
                    _ => return Ok(Value::UNDEFINED),
                };
                let off = args
                    .first()
                    .map(|v| self.to_number_val(*v))
                    .transpose()?
                    .unwrap_or(0.0) as usize;
                let value = args
                    .get(1)
                    .map(|v| self.to_number_val(*v))
                    .transpose()?
                    .unwrap_or(0.0);
                let little = args
                    .get(2)
                    .map(|v| !v.is_undefined() && !v.is_null() && v.is_truthy_full(&self.heap))
                    .unwrap_or(false);
                let abs_off = dv.byte_offset + off;
                let mut buf = unsafe { dv.buffer.borrow_mut() };
                match builtin.function {
                    BuiltinFunction::DataViewSetInt8 => buf[abs_off] = value as i64 as i8 as u8,
                    BuiltinFunction::DataViewSetUint8 => buf[abs_off] = value as i64 as u8,
                    BuiltinFunction::DataViewSetInt16 => {
                        let bytes = (value as i64 as i16);
                        let bytes = if little { bytes.to_le_bytes() } else { bytes.to_be_bytes() };
                        buf[abs_off..abs_off + 2].copy_from_slice(&bytes);
                    }
                    BuiltinFunction::DataViewSetUint16 => {
                        let bytes = (value as i64 as u16);
                        let bytes = if little { bytes.to_le_bytes() } else { bytes.to_be_bytes() };
                        buf[abs_off..abs_off + 2].copy_from_slice(&bytes);
                    }
                    BuiltinFunction::DataViewSetInt32 => {
                        let bytes = (value as i64 as i32);
                        let bytes = if little { bytes.to_le_bytes() } else { bytes.to_be_bytes() };
                        buf[abs_off..abs_off + 4].copy_from_slice(&bytes);
                    }
                    BuiltinFunction::DataViewSetUint32 => {
                        let bytes = (value as i64 as u32);
                        let bytes = if little { bytes.to_le_bytes() } else { bytes.to_be_bytes() };
                        buf[abs_off..abs_off + 4].copy_from_slice(&bytes);
                    }
                    BuiltinFunction::DataViewSetFloat32 => {
                        let bytes = (value as f32);
                        let bytes = if little { bytes.to_le_bytes() } else { bytes.to_be_bytes() };
                        buf[abs_off..abs_off + 4].copy_from_slice(&bytes);
                    }
                    BuiltinFunction::DataViewSetFloat64 => {
                        let bytes = if little { value.to_le_bytes() } else { value.to_be_bytes() };
                        buf[abs_off..abs_off + 8].copy_from_slice(&bytes);
                    }
                    _ => unreachable!(),
                }
                Ok(Value::UNDEFINED)
            }
            // ── Microtask queue ────────────────────────────────────────
            BuiltinFunction::QueueMicrotask => {
                let cb = args.first().copied().unwrap_or(Value::UNDEFINED);
                if !cb.is_undefined() && !cb.is_null() {
                    self.microtask_queue.push_back((cb, Vec::new()));
                }
                Ok(Value::UNDEFINED)
            }
            // `new Promise(executor)` — creates a pending promise backed by
            // an Rc<VmCell>, then calls `executor(resolve, reject)` where
            // both callbacks are BuiltinFunctions bound to the same cell.
            // If the executor calls resolve/reject synchronously, the
            // promise is settled before the constructor returns. If it
            // doesn't (e.g. captures resolve for later), the promise stays
            // pending and any `.then` callbacks registered in the meantime
            // are queued, to be drained when resolve is eventually called.
            BuiltinFunction::PromiseExecutorCtor => {
                let executor = args.first().copied().unwrap_or(Value::UNDEFINED);
                let pending = crate::object::new_pending_promise();
                let prom_rc = match &pending {
                    Object::Promise(rc) => rc.clone(),
                    _ => unreachable!(),
                };
                let promise_val = obj_into_val(pending, &mut self.heap);

                if executor.is_undefined() || executor.is_null() {
                    return Ok(promise_val);
                }

                // Build the resolve/reject callables. The receiver carries
                // the shared Rc so every call sees the same cell as the
                // one we return to the user.
                let resolve_bf = Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::PromiseResolveBound,
                    receiver: Some(Object::Promise(prom_rc.clone())),
                }));
                let reject_bf = Object::BuiltinFunction(Box::new(BuiltinFunctionObject {
                    function: BuiltinFunction::PromiseRejectBound,
                    receiver: Some(Object::Promise(prom_rc.clone())),
                }));
                let resolve_val = obj_into_val(resolve_bf, &mut self.heap);
                let reject_val = obj_into_val(reject_bf, &mut self.heap);
                // Run the executor. Any throw propagates into the promise
                // as a rejection, matching the spec.
                let run = self.call_value_slice(executor, &[resolve_val, reject_val]);
                if let Err(e) = run {
                    let err_obj = Object::Error(Box::new(crate::object::ErrorObject {
                        name: Rc::from("Error"),
                        message: Rc::from(format!("{:?}", e).as_str()),
                    }));
                    self.settle_promise(&prom_rc, PromiseState::Rejected(Box::new(err_obj)))?;
                }
                Ok(promise_val)
            }
            BuiltinFunction::PromiseResolveBound => {
                let receiver = builtin.receiver.clone().ok_or_else(|| {
                    VMError::TypeError("resolve missing receiver".to_string())
                })?;
                let prom_rc = match receiver {
                    Object::Promise(p) => p,
                    _ => return Ok(Value::UNDEFINED),
                };
                let value = args.first().copied().unwrap_or(Value::UNDEFINED);
                // If the incoming value is itself a promise, adopt its
                // state (spec says "follow the thenable").
                let value_obj = val_to_obj(value, &self.heap);
                let state = if let Object::Promise(inner) = &value_obj {
                    // Pull the inner state. If it's pending, register a
                    // chain so when it resolves, we propagate; for now,
                    // if pending, we just stay pending.
                    let inner_state = inner.borrow().settled.clone();
                    match inner_state {
                        PromiseState::Pending => {
                            // Adopt-pending: attach ourselves as a then-
                            // handler on the inner promise.
                            let self_val = obj_into_val(
                                Object::Promise(prom_rc.clone()),
                                &mut self.heap,
                            );
                            let mut inner_mut = unsafe { inner.borrow_mut() };
                            inner_mut.then_chain.push(Value::UNDEFINED);
                            inner_mut.catch_chain.push(Value::UNDEFINED);
                            inner_mut.chained.push(self_val);
                            return Ok(Value::UNDEFINED);
                        }
                        s => s,
                    }
                } else {
                    PromiseState::Fulfilled(Box::new(value_obj))
                };
                self.settle_promise(&prom_rc, state)?;
                Ok(Value::UNDEFINED)
            }
            BuiltinFunction::PromiseChainStep => {
                // Args: [chained_promise, value, kind (0=fulfilled,1=rejected), handler]
                // Runs the handler (if any) with `value`, then settles
                // the chained promise with the handler's return — or the
                // pass-through value if handler is UNDEFINED.
                let chained_val = args.first().copied().unwrap_or(Value::UNDEFINED);
                let value = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let kind = args
                    .get(2)
                    .map(|v| {
                        if v.is_i32() {
                            unsafe { v.as_i32_unchecked() }
                        } else {
                            0
                        }
                    })
                    .unwrap_or(0);
                let handler = args.get(3).copied().unwrap_or(Value::UNDEFINED);
                let chained_obj = val_to_obj(chained_val, &self.heap);
                let chained_rc = match chained_obj {
                    Object::Promise(rc) => rc,
                    _ => return Ok(Value::UNDEFINED),
                };

                if !handler.is_undefined() && !handler.is_null() {
                    // Run the handler. A throw here settles the chained
                    // promise as rejected with the thrown value.
                    match self.call_value_slice(handler, &[value]) {
                        Ok(result) => {
                            // If the handler returns a promise, adopt it.
                            let result_obj = val_to_obj(result, &self.heap);
                            if let Object::Promise(inner) = &result_obj {
                                let inner_state = inner.borrow().settled.clone();
                                match inner_state {
                                    PromiseState::Pending => {
                                        // Adopt-pending: register chained
                                        // as a handler on the inner.
                                        let chained_v = obj_into_val(
                                            Object::Promise(chained_rc.clone()),
                                            &mut self.heap,
                                        );
                                        let mut im = unsafe { inner.borrow_mut() };
                                        im.then_chain.push(Value::UNDEFINED);
                                        im.catch_chain.push(Value::UNDEFINED);
                                        im.chained.push(chained_v);
                                        return Ok(Value::UNDEFINED);
                                    }
                                    s => {
                                        self.settle_promise(&chained_rc, s)?;
                                    }
                                }
                            } else {
                                self.settle_promise(
                                    &chained_rc,
                                    PromiseState::Fulfilled(Box::new(result_obj)),
                                )?;
                            }
                        }
                        Err(e) => {
                            let err_obj = Object::Error(Box::new(crate::object::ErrorObject {
                                name: Rc::from("Error"),
                                message: Rc::from(format!("{:?}", e).as_str()),
                            }));
                            self.settle_promise(
                                &chained_rc,
                                PromiseState::Rejected(Box::new(err_obj)),
                            )?;
                        }
                    }
                } else {
                    // No handler — propagate value in its original kind.
                    let value_obj = val_to_obj(value, &self.heap);
                    let st = if kind == 0 {
                        PromiseState::Fulfilled(Box::new(value_obj))
                    } else {
                        PromiseState::Rejected(Box::new(value_obj))
                    };
                    self.settle_promise(&chained_rc, st)?;
                }
                Ok(Value::UNDEFINED)
            }
            BuiltinFunction::PromiseRejectBound => {
                let receiver = builtin.receiver.clone().ok_or_else(|| {
                    VMError::TypeError("reject missing receiver".to_string())
                })?;
                let prom_rc = match receiver {
                    Object::Promise(p) => p,
                    _ => return Ok(Value::UNDEFINED),
                };
                let value = args.first().copied().unwrap_or(Value::UNDEFINED);
                let value_obj = val_to_obj(value, &self.heap);
                self.settle_promise(
                    &prom_rc,
                    PromiseState::Rejected(Box::new(value_obj)),
                )?;
                Ok(Value::UNDEFINED)
            }
            // ── Proxy ──────────────────────────────────────────────────
            // Construction only — trap dispatch lives in indexing.rs so it
            // sits on the property-access hot path without an extra hop.
            BuiltinFunction::ProxyCtor => {
                let target_val = args.first().copied().unwrap_or(Value::UNDEFINED);
                let handler_val = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let target = val_to_obj(target_val, &self.heap);
                let handler = val_to_obj(handler_val, &self.heap);
                if !matches!(handler, Object::Hash(_)) {
                    return Err(VMError::TypeError(
                        "Proxy handler must be an object".to_string(),
                    ));
                }
                let proxy = Object::Proxy(Box::new(crate::object::ProxyObject {
                    target: Box::new(target),
                    handler: Box::new(handler),
                }));
                Ok(obj_into_val(proxy, &mut self.heap))
            }
            // ── BigInt ──────────────────────────────────────────────────
            BuiltinFunction::BigIntCtor => {
                let v = args.first().copied().unwrap_or(Value::UNDEFINED);
                let val_obj = val_to_obj(v, &self.heap);
                let bi: i128 = match val_obj {
                    Object::Integer(n) => n as i128,
                    Object::Float(f) => f as i128,
                    Object::Boolean(b) => if b { 1 } else { 0 },
                    Object::String(s) => s.parse::<i128>().map_err(|_| {
                        VMError::TypeError(format!("Cannot convert '{}' to BigInt", s))
                    })?,
                    Object::BigInt(n) => n,
                    _ => 0,
                };
                Ok(obj_into_val(Object::BigInt(bi), &mut self.heap))
            }
            // ── Reflect API ─────────────────────────────────────────────
            // Most arms are thin wrappers around the matching Object.* /
            // syntax-level operation. Where an Object.* builtin already
            // exists, we delegate via execute_builtin_function_slice so
            // there's exactly one source of truth for the semantics.
            BuiltinFunction::ReflectGet => {
                let target_val = args.first().copied().unwrap_or(Value::UNDEFINED);
                let key_val = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let target_obj = val_to_obj(target_val, &self.heap);
                let key_obj = val_to_obj(key_val, &self.heap);
                self.execute_index_expression(target_obj, key_obj)?;
                self.pop_val()
            }
            BuiltinFunction::ReflectSet => {
                let target_val = args.first().copied().unwrap_or(Value::UNDEFINED);
                let key_val = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let value_val = args.get(2).copied().unwrap_or(Value::UNDEFINED);
                let target_obj = val_to_obj(target_val, &self.heap);
                let key_obj = val_to_obj(key_val, &self.heap);
                let value_obj = val_to_obj(value_val, &self.heap);
                self.execute_set_index(target_obj, key_obj, value_obj)?;
                // execute_set_index leaves the receiver on the stack —
                // discard it; Reflect.set returns a boolean.
                let _ = self.pop_val();
                Ok(Value::from_bool(true))
            }
            BuiltinFunction::ReflectHas => {
                let target_val = args.first().copied().unwrap_or(Value::UNDEFINED);
                let key_val = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let key_str = val_inspect(key_val, &self.heap);
                let found = match val_to_obj(target_val, &self.heap) {
                    Object::Hash(h) => h.borrow().get_by_str(&key_str).is_some(),
                    Object::Array(a) => {
                        if let Ok(i) = key_str.parse::<usize>() {
                            i < a.borrow().len()
                        } else {
                            false
                        }
                    }
                    Object::Instance(inst) => {
                        inst.fields.contains_key(key_str.as_str())
                            || inst.methods.contains_key(key_str.as_str())
                            || inst.getters.contains_key(key_str.as_str())
                    }
                    _ => false,
                };
                Ok(Value::from_bool(found))
            }
            BuiltinFunction::ReflectDeleteProperty => {
                // Mirrors `delete obj[key]` — only succeeds on Hash today.
                let target_val = args.first().copied().unwrap_or(Value::UNDEFINED);
                let key_val = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let target_obj = val_to_obj(target_val, &self.heap);
                let key_str = val_inspect(key_val, &self.heap);
                let removed = match target_obj {
                    Object::Hash(h) => {
                        let mut hb = unsafe { h.borrow_mut() };
                        if hb.frozen {
                            false
                        } else {
                            let sym = crate::intern::intern(&key_str);
                            hb.remove_pair(&HashKey::Sym(sym)).is_some()
                        }
                    }
                    _ => true,
                };
                Ok(Value::from_bool(removed))
            }
            BuiltinFunction::ReflectOwnKeys => self.execute_builtin_function_slice(
                BuiltinFunctionObject {
                    function: BuiltinFunction::ObjectKeys,
                    receiver: None,
                },
                args,
            ),
            BuiltinFunction::ReflectGetPrototypeOf => self.execute_builtin_function_slice(
                BuiltinFunctionObject {
                    function: BuiltinFunction::ObjectGetPrototypeOf,
                    receiver: None,
                },
                args,
            ),
            BuiltinFunction::ReflectSetPrototypeOf => {
                // Engine doesn't track per-object prototypes outside Class
                // chains. Accept the call so library code that drives Reflect
                // through it doesn't error, but report the no-op truthfully.
                Ok(Value::from_bool(false))
            }
            BuiltinFunction::ReflectIsExtensible => {
                let target_val = args.first().copied().unwrap_or(Value::UNDEFINED);
                let extensible = match val_to_obj(target_val, &self.heap) {
                    Object::Hash(h) => !h.borrow().frozen,
                    _ => true,
                };
                Ok(Value::from_bool(extensible))
            }
            BuiltinFunction::ReflectPreventExtensions => self.execute_builtin_function_slice(
                BuiltinFunctionObject {
                    function: BuiltinFunction::ObjectFreeze,
                    receiver: None,
                },
                args,
            ),
            BuiltinFunction::ReflectDefineProperty => self.execute_builtin_function_slice(
                BuiltinFunctionObject {
                    function: BuiltinFunction::ObjectDefineProperty,
                    receiver: None,
                },
                args,
            ),
            BuiltinFunction::ReflectGetOwnPropertyDescriptor => self
                .execute_builtin_function_slice(
                    BuiltinFunctionObject {
                        function: BuiltinFunction::ObjectGetOwnPropertyDescriptor,
                        receiver: None,
                    },
                    args,
                ),
            BuiltinFunction::ReflectApply => {
                // Reflect.apply(fn, thisArg, argsArray)
                let fn_val = args.first().copied().unwrap_or(Value::UNDEFINED);
                let _this_val = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let arr_val = args.get(2).copied().unwrap_or(Value::UNDEFINED);
                let arr_obj = val_to_obj(arr_val, &self.heap);
                let arg_vec = match arr_obj {
                    Object::Array(a) => unwrap_array(a),
                    _ => Vec::new(),
                };
                // The engine's call_value_slice doesn't accept a custom
                // `this` (functions inherit it through BoundMethod). For
                // ergonomics we just pass the args through; passing this
                // properly would need a temporary BoundMethod allocation.
                self.call_value_slice(fn_val, &arg_vec)
            }
            BuiltinFunction::ReflectConstruct => {
                // Reflect.construct(fn, argsArray) — equivalent to `new fn(...)`.
                let fn_val = args.first().copied().unwrap_or(Value::UNDEFINED);
                let arr_val = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let arr_obj = val_to_obj(arr_val, &self.heap);
                let arg_vec = match arr_obj {
                    Object::Array(a) => unwrap_array(a),
                    _ => Vec::new(),
                };
                // Without a dedicated `new`-style call helper, fall through
                // to a regular call. For a Class this still produces the
                // right Instance because the constructor is registered as
                // the function entry point.
                self.call_value_slice(fn_val, &arg_vec)
            }
            BuiltinFunction::ErrorConstructor
            | BuiltinFunction::TypeErrorConstructor
            | BuiltinFunction::RangeErrorConstructor
            | BuiltinFunction::SyntaxErrorConstructor
            | BuiltinFunction::ReferenceErrorConstructor => {
                let message = args
                    .first()
                    .map(|v| val_inspect(*v, &self.heap))
                    .unwrap_or_default();
                let name = match builtin.function {
                    BuiltinFunction::TypeErrorConstructor => "TypeError",
                    BuiltinFunction::RangeErrorConstructor => "RangeError",
                    BuiltinFunction::SyntaxErrorConstructor => "SyntaxError",
                    BuiltinFunction::ReferenceErrorConstructor => "ReferenceError",
                    _ => "Error",
                };
                let err = Object::Error(Box::new(crate::object::ErrorObject {
                    name: Rc::from(name),
                    message: Rc::from(message.as_str()),
                }));
                Ok(obj_into_val(err, &mut self.heap))
            }
            BuiltinFunction::StringAt => {
                let receiver = builtin.receiver.as_ref();
                let s = match receiver {
                    Some(Object::String(s)) => s.clone(),
                    _ => return Ok(Value::UNDEFINED),
                };
                let idx = args
                    .first()
                    .map(|v| self.to_number_val(*v))
                    .transpose()?
                    .unwrap_or(0.0) as i64;
                let len = s.chars().count() as i64;
                let actual = if idx < 0 { len + idx } else { idx };
                if actual < 0 || actual >= len {
                    return Ok(Value::UNDEFINED);
                }
                if let Some(ch) = s.chars().nth(actual as usize) {
                    Ok(obj_into_val(
                        Object::String(ch.to_string().into()),
                        &mut self.heap,
                    ))
                } else {
                    Ok(Value::UNDEFINED)
                }
            }
            BuiltinFunction::StringCodePointAt => {
                let receiver = builtin.receiver.as_ref();
                let s = match receiver {
                    Some(Object::String(s)) => s.clone(),
                    _ => return Ok(Value::UNDEFINED),
                };
                let idx = args
                    .first()
                    .map(|v| self.to_number_val(*v))
                    .transpose()?
                    .unwrap_or(0.0) as usize;
                if let Some(ch) = s.chars().nth(idx) {
                    Ok(Value::from_i64(ch as i64))
                } else {
                    Ok(Value::UNDEFINED)
                }
            }
            BuiltinFunction::StringRaw => {
                // String.raw(strings, ...values)
                // strings is an object with a .raw property (array of raw strings)
                let strings_val = args.first().copied().unwrap_or(Value::UNDEFINED);
                let strings_obj = val_to_obj(strings_val, &self.heap);
                let raw_parts: Vec<String> = match strings_obj {
                    Object::Hash(h) => {
                        let hb = h.borrow();
                        if let Some(raw_val) = hb.get_by_str("raw") {
                            let raw_obj = val_to_obj(raw_val, &self.heap);
                            match raw_obj {
                                Object::Array(items) => {
                                    items.borrow().iter().map(|v| {
                                        let o = val_to_obj(*v, &self.heap);
                                        match o {
                                            Object::String(s) => s.to_string(),
                                            _ => o.inspect(),
                                        }
                                    }).collect()
                                }
                                _ => vec![],
                            }
                        } else {
                            vec![]
                        }
                    }
                    _ => vec![],
                };
                let mut result = String::new();
                for (i, part) in raw_parts.iter().enumerate() {
                    result.push_str(part);
                    if i + 1 < raw_parts.len() {
                        if let Some(&val) = args.get(i + 1) {
                            let obj = val_to_obj(val, &self.heap);
                            match obj {
                                Object::String(s) => result.push_str(&s),
                                _ => result.push_str(&obj.inspect()),
                            }
                        }
                    }
                }
                Ok(obj_into_val(Object::String(result.into()), &mut self.heap))
            }
            BuiltinFunction::StringNormalize => {
                let receiver = builtin.receiver.as_ref();
                match receiver {
                    Some(Object::String(s)) => {
                        // Basic NFC normalization — for ASCII strings this is identity
                        // Full Unicode normalization would require the `unicode-normalization` crate
                        Ok(obj_into_val(Object::String(s.clone()), &mut self.heap))
                    }
                    _ => Ok(Value::UNDEFINED),
                }
            }
            BuiltinFunction::NumberToPrecision => {
                let receiver = builtin.receiver.ok_or_else(|| {
                    VMError::TypeError("Number.toPrecision missing receiver".to_string())
                })?;
                let n = self.to_number(&receiver)?;
                let precision = args
                    .first()
                    .map(|v| self.to_u32_val(*v))
                    .transpose()?
                    .unwrap_or(0) as usize;
                if precision == 0 {
                    // No argument: just toString
                    return Ok(obj_into_val(
                        Object::String(format!("{}", n).into()),
                        &mut self.heap,
                    ));
                }
                // ECMA-262 §21.1.3.5: precision must be 1..=100.
                // Without this clamp `(1).toPrecision(4e9)` allocates
                // multiple gigabytes in the exponential-format path.
                if precision > 100 {
                    return Err(VMError::TypeError(
                        "Number.toPrecision: precision out of range (must be 1..=100)".to_string(),
                    ));
                }
                let result = Self::format_to_precision(n, precision);
                Ok(obj_into_val(Object::String(result.into()), &mut self.heap))
            }
            BuiltinFunction::StructuredClone => {
                if args.is_empty() {
                    return Ok(Value::UNDEFINED);
                }
                let val = args[0];
                let cloned = self.deep_clone_value(val)?;
                Ok(cloned)
            }
        }
    }
}
