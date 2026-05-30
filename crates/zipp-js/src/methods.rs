//! Built-in methods on the primitive/array "wrapper" types.
//!
//! v0 special-cases these in member access (no prototype chain yet — that's a
//! later tier). Each is a [`NativeFn`] `(interp, this, args) -> result`. This is
//! a core set; coverage grows toward Test262 over time.

use crate::interp::{EvalResult, Interp};
use crate::value::{JsValue, NativeFn, Obj, ObjData, Object};

fn as_array(this: &JsValue) -> Option<Obj> {
    if let JsValue::Object(o) = this {
        if matches!(o.borrow().data, ObjData::Array(_)) {
            return Some(o.clone());
        }
    }
    None
}

/// Snapshot an array's elements (so a callback can safely mutate the original).
fn snapshot(o: &Obj) -> Vec<JsValue> {
    if let ObjData::Array(v) = &o.borrow().data {
        v.clone()
    } else {
        Vec::new()
    }
}

fn arg(args: &[JsValue], i: usize) -> JsValue {
    args.get(i).cloned().unwrap_or(JsValue::Undefined)
}

// ToIntegerOrInfinity for a fromIndex argument: NaN/undefined -> 0, else
// truncate toward zero (±Infinity pass through).
fn to_integer_or_zero(v: &JsValue) -> f64 {
    let n = v.to_number();
    if n.is_nan() {
        0.0
    } else {
        n.trunc()
    }
}

/// Normalize a (possibly negative / out-of-range) index against `len`.
fn rel_index(v: &JsValue, len: usize, default: usize) -> usize {
    if matches!(v, JsValue::Undefined) {
        return default;
    }
    let n = v.to_number();
    if n.is_nan() {
        return 0;
    }
    let l = len as f64;
    let i = if n < 0.0 { (l + n).max(0.0) } else { n.min(l) };
    i as usize
}

// ───────────────────────── array methods ─────────────────────────

pub fn array_method(name: &str) -> Option<NativeFn> {
    Some(match name {
        "push" => arr_push,
        "pop" => arr_pop,
        "shift" => arr_shift,
        "unshift" => arr_unshift,
        "slice" => arr_slice,
        "concat" => arr_concat,
        "join" => arr_join,
        "indexOf" => arr_index_of,
        "lastIndexOf" => arr_last_index_of,
        "includes" => arr_includes,
        "find" => arr_find,
        "findIndex" => arr_find_index,
        "map" => arr_map,
        "filter" => arr_filter,
        "reduce" => arr_reduce,
        "forEach" => arr_for_each,
        "some" => arr_some,
        "every" => arr_every,
        "reverse" => arr_reverse,
        "fill" => arr_fill,
        "flat" => arr_flat,
        "flatMap" => arr_flat_map,
        "reduceRight" => arr_reduce_right,
        "at" => arr_at,
        "sort" => arr_sort,
        _ => return None,
    })
}

fn arr_push(it: &Interp, this: &JsValue, args: &[JsValue]) -> EvalResult<JsValue> {
    let o = as_array(this).ok_or_else(|| it.type_error("push called on non-array"))?;
    let mut b = o.borrow_mut();
    if let ObjData::Array(items) = &mut b.data {
        items.extend_from_slice(args);
        Ok(JsValue::Num(items.len() as f64))
    } else {
        unreachable!()
    }
}

fn arr_pop(it: &Interp, this: &JsValue, _a: &[JsValue]) -> EvalResult<JsValue> {
    let o = as_array(this).ok_or_else(|| it.type_error("pop called on non-array"))?;
    let mut b = o.borrow_mut();
    if let ObjData::Array(items) = &mut b.data {
        Ok(items.pop().unwrap_or(JsValue::Undefined))
    } else {
        unreachable!()
    }
}

fn arr_shift(it: &Interp, this: &JsValue, _a: &[JsValue]) -> EvalResult<JsValue> {
    let o = as_array(this).ok_or_else(|| it.type_error("shift called on non-array"))?;
    let mut b = o.borrow_mut();
    if let ObjData::Array(items) = &mut b.data {
        if items.is_empty() {
            Ok(JsValue::Undefined)
        } else {
            Ok(items.remove(0))
        }
    } else {
        unreachable!()
    }
}

fn arr_unshift(it: &Interp, this: &JsValue, args: &[JsValue]) -> EvalResult<JsValue> {
    let o = as_array(this).ok_or_else(|| it.type_error("unshift called on non-array"))?;
    let mut b = o.borrow_mut();
    if let ObjData::Array(items) = &mut b.data {
        for (i, v) in args.iter().enumerate() {
            items.insert(i, v.clone());
        }
        Ok(JsValue::Num(items.len() as f64))
    } else {
        unreachable!()
    }
}

fn arr_slice(it: &Interp, this: &JsValue, args: &[JsValue]) -> EvalResult<JsValue> {
    let o = as_array(this).ok_or_else(|| it.type_error("slice called on non-array"))?;
    let items = snapshot(&o);
    let start = rel_index(&arg(args, 0), items.len(), 0);
    let end = rel_index(args.get(1).unwrap_or(&JsValue::Undefined), items.len(), items.len());
    let out = if end > start { items[start..end].to_vec() } else { Vec::new() };
    Ok(JsValue::Object(Object::array(out)))
}

fn arr_concat(it: &Interp, this: &JsValue, args: &[JsValue]) -> EvalResult<JsValue> {
    let o = as_array(this).ok_or_else(|| it.type_error("concat called on non-array"))?;
    let mut out = snapshot(&o);
    for a in args {
        if let Some(other) = as_array(a) {
            out.extend(snapshot(&other));
        } else {
            out.push(a.clone());
        }
    }
    Ok(JsValue::Object(Object::array(out)))
}

fn arr_join(it: &Interp, this: &JsValue, args: &[JsValue]) -> EvalResult<JsValue> {
    let o = as_array(this).ok_or_else(|| it.type_error("join called on non-array"))?;
    let sep = match args.first() {
        None | Some(JsValue::Undefined) => ",".to_string(),
        Some(v) => v.to_js_string(),
    };
    // Route each element through ToString so a user-defined toString() runs;
    // snapshot first so the array borrow is released before re-entering user code.
    let items = snapshot(&o);
    let mut parts: Vec<String> = Vec::with_capacity(items.len());
    for v in &items {
        parts.push(match v {
            JsValue::Undefined | JsValue::Null => String::new(),
            other => it.to_string(other)?,
        });
    }
    Ok(JsValue::str(parts.join(&sep)))
}

fn arr_index_of(it: &Interp, this: &JsValue, args: &[JsValue]) -> EvalResult<JsValue> {
    let o = as_array(this).ok_or_else(|| it.type_error("indexOf called on non-array"))?;
    let target = arg(args, 0);
    let items = snapshot(&o);
    let len = items.len() as f64;
    let n = args.get(1).map_or(0.0, to_integer_or_zero);
    if n >= len {
        return Ok(JsValue::Num(-1.0));
    }
    let start = if n >= 0.0 { n } else { (len + n).max(0.0) } as usize;
    for (i, v) in items.iter().enumerate().skip(start) {
        if v.strict_eq(&target) {
            return Ok(JsValue::Num(i as f64));
        }
    }
    Ok(JsValue::Num(-1.0))
}

fn arr_last_index_of(it: &Interp, this: &JsValue, args: &[JsValue]) -> EvalResult<JsValue> {
    let o = as_array(this).ok_or_else(|| it.type_error("lastIndexOf called on non-array"))?;
    let target = arg(args, 0);
    let items = snapshot(&o);
    let len = items.len();
    if len == 0 {
        return Ok(JsValue::Num(-1.0));
    }
    // default fromIndex is the last element; negatives count from the end
    let n = args.get(1).map_or((len as f64) - 1.0, to_integer_or_zero);
    let k = if n >= 0.0 {
        n.min((len - 1) as f64) as isize
    } else {
        (len as f64 + n) as isize
    };
    if k < 0 {
        return Ok(JsValue::Num(-1.0));
    }
    for i in (0..=(k as usize)).rev() {
        if items[i].strict_eq(&target) {
            return Ok(JsValue::Num(i as f64));
        }
    }
    Ok(JsValue::Num(-1.0))
}

fn arr_includes(it: &Interp, this: &JsValue, args: &[JsValue]) -> EvalResult<JsValue> {
    let o = as_array(this).ok_or_else(|| it.type_error("includes called on non-array"))?;
    let target = arg(args, 0);
    let items = snapshot(&o);
    let len = items.len() as f64;
    let n = args.get(1).map_or(0.0, to_integer_or_zero);
    if n >= len {
        return Ok(JsValue::Bool(false));
    }
    let start = if n >= 0.0 { n } else { (len + n).max(0.0) } as usize;
    // SameValueZero: === plus NaN matching NaN.
    let found = items[start..].iter().any(|v| {
        v.strict_eq(&target)
            || matches!((v, &target), (JsValue::Num(a), JsValue::Num(b)) if a.is_nan() && b.is_nan())
    });
    Ok(JsValue::Bool(found))
}

fn arr_map(it: &Interp, this: &JsValue, args: &[JsValue]) -> EvalResult<JsValue> {
    let o = as_array(this).ok_or_else(|| it.type_error("map called on non-array"))?;
    let cb = arg(args, 0);
    let items = snapshot(&o);
    let mut out = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        out.push(it.call(&cb, &JsValue::Undefined, &[item.clone(), JsValue::Num(i as f64), this.clone()])?);
    }
    Ok(JsValue::Object(Object::array(out)))
}

fn arr_filter(it: &Interp, this: &JsValue, args: &[JsValue]) -> EvalResult<JsValue> {
    let o = as_array(this).ok_or_else(|| it.type_error("filter called on non-array"))?;
    let cb = arg(args, 0);
    let items = snapshot(&o);
    let mut out = Vec::new();
    for (i, item) in items.iter().enumerate() {
        if it.call(&cb, &JsValue::Undefined, &[item.clone(), JsValue::Num(i as f64), this.clone()])?.truthy() {
            out.push(item.clone());
        }
    }
    Ok(JsValue::Object(Object::array(out)))
}

fn arr_reduce(it: &Interp, this: &JsValue, args: &[JsValue]) -> EvalResult<JsValue> {
    let o = as_array(this).ok_or_else(|| it.type_error("reduce called on non-array"))?;
    let cb = arg(args, 0);
    let items = snapshot(&o);
    let mut idx = 0;
    let mut acc = if args.len() >= 2 {
        arg(args, 1)
    } else if items.is_empty() {
        return Err(it.type_error("Reduce of empty array with no initial value"));
    } else {
        idx = 1;
        items[0].clone()
    };
    while idx < items.len() {
        acc = it.call(&cb, &JsValue::Undefined, &[acc, items[idx].clone(), JsValue::Num(idx as f64), this.clone()])?;
        idx += 1;
    }
    Ok(acc)
}

fn arr_for_each(it: &Interp, this: &JsValue, args: &[JsValue]) -> EvalResult<JsValue> {
    let o = as_array(this).ok_or_else(|| it.type_error("forEach called on non-array"))?;
    let cb = arg(args, 0);
    for (i, item) in snapshot(&o).iter().enumerate() {
        it.call(&cb, &JsValue::Undefined, &[item.clone(), JsValue::Num(i as f64), this.clone()])?;
    }
    Ok(JsValue::Undefined)
}

fn arr_some(it: &Interp, this: &JsValue, args: &[JsValue]) -> EvalResult<JsValue> {
    let o = as_array(this).ok_or_else(|| it.type_error("some called on non-array"))?;
    let cb = arg(args, 0);
    for (i, item) in snapshot(&o).iter().enumerate() {
        if it.call(&cb, &JsValue::Undefined, &[item.clone(), JsValue::Num(i as f64), this.clone()])?.truthy() {
            return Ok(JsValue::Bool(true));
        }
    }
    Ok(JsValue::Bool(false))
}

fn arr_every(it: &Interp, this: &JsValue, args: &[JsValue]) -> EvalResult<JsValue> {
    let o = as_array(this).ok_or_else(|| it.type_error("every called on non-array"))?;
    let cb = arg(args, 0);
    for (i, item) in snapshot(&o).iter().enumerate() {
        if !it.call(&cb, &JsValue::Undefined, &[item.clone(), JsValue::Num(i as f64), this.clone()])?.truthy() {
            return Ok(JsValue::Bool(false));
        }
    }
    Ok(JsValue::Bool(true))
}

fn arr_find(it: &Interp, this: &JsValue, args: &[JsValue]) -> EvalResult<JsValue> {
    let o = as_array(this).ok_or_else(|| it.type_error("find called on non-array"))?;
    let cb = arg(args, 0);
    for (i, item) in snapshot(&o).iter().enumerate() {
        if it.call(&cb, &JsValue::Undefined, &[item.clone(), JsValue::Num(i as f64), this.clone()])?.truthy() {
            return Ok(item.clone());
        }
    }
    Ok(JsValue::Undefined)
}

fn arr_find_index(it: &Interp, this: &JsValue, args: &[JsValue]) -> EvalResult<JsValue> {
    let o = as_array(this).ok_or_else(|| it.type_error("findIndex called on non-array"))?;
    let cb = arg(args, 0);
    for (i, item) in snapshot(&o).iter().enumerate() {
        if it.call(&cb, &JsValue::Undefined, &[item.clone(), JsValue::Num(i as f64), this.clone()])?.truthy() {
            return Ok(JsValue::Num(i as f64));
        }
    }
    Ok(JsValue::Num(-1.0))
}

fn arr_reverse(it: &Interp, this: &JsValue, _a: &[JsValue]) -> EvalResult<JsValue> {
    let o = as_array(this).ok_or_else(|| it.type_error("reverse called on non-array"))?;
    if let ObjData::Array(items) = &mut o.borrow_mut().data {
        items.reverse();
    }
    Ok(this.clone())
}

fn arr_fill(it: &Interp, this: &JsValue, args: &[JsValue]) -> EvalResult<JsValue> {
    let o = as_array(this).ok_or_else(|| it.type_error("fill called on non-array"))?;
    let v = arg(args, 0);
    let len = snapshot(&o).len();
    let start = rel_index(args.get(1).unwrap_or(&JsValue::Undefined), len, 0);
    let end = rel_index(args.get(2).unwrap_or(&JsValue::Undefined), len, len);
    if let ObjData::Array(items) = &mut o.borrow_mut().data {
        for slot in items.iter_mut().take(end).skip(start) {
            *slot = v.clone();
        }
    }
    Ok(this.clone())
}

fn arr_flat(it: &Interp, this: &JsValue, _a: &[JsValue]) -> EvalResult<JsValue> {
    let o = as_array(this).ok_or_else(|| it.type_error("flat called on non-array"))?;
    let mut out = Vec::new();
    for v in snapshot(&o) {
        if let Some(inner) = as_array(&v) {
            out.extend(snapshot(&inner));
        } else {
            out.push(v);
        }
    }
    Ok(JsValue::Object(Object::array(out)))
}

// flatMap(cb, [thisArg]) == map(cb).flat(1): map each element, then flatten the
// results by a single level.
fn arr_flat_map(it: &Interp, this: &JsValue, args: &[JsValue]) -> EvalResult<JsValue> {
    let o = as_array(this).ok_or_else(|| it.type_error("flatMap called on non-array"))?;
    let cb = arg(args, 0);
    let items = snapshot(&o);
    let mut out = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let r = it.call(&cb, &JsValue::Undefined, &[item.clone(), JsValue::Num(i as f64), this.clone()])?;
        if let Some(inner) = as_array(&r) {
            out.extend(snapshot(&inner));
        } else {
            out.push(r);
        }
    }
    Ok(JsValue::Object(Object::array(out)))
}

// reduceRight(cb, [init]): like reduce but walks indices from len-1 down to 0.
fn arr_reduce_right(it: &Interp, this: &JsValue, args: &[JsValue]) -> EvalResult<JsValue> {
    let o = as_array(this).ok_or_else(|| it.type_error("reduceRight called on non-array"))?;
    let cb = arg(args, 0);
    let items = snapshot(&o);
    let mut idx = items.len();
    let mut acc = if args.len() >= 2 {
        arg(args, 1)
    } else if items.is_empty() {
        return Err(it.type_error("Reduce of empty array with no initial value"));
    } else {
        idx -= 1;
        items[idx].clone()
    };
    while idx > 0 {
        idx -= 1;
        acc = it.call(&cb, &JsValue::Undefined, &[acc, items[idx].clone(), JsValue::Num(idx as f64), this.clone()])?;
    }
    Ok(acc)
}

fn arr_at(it: &Interp, this: &JsValue, args: &[JsValue]) -> EvalResult<JsValue> {
    let o = as_array(this).ok_or_else(|| it.type_error("at called on non-array"))?;
    let items = snapshot(&o);
    let n = arg(args, 0).to_number();
    let i = if n < 0.0 { items.len() as f64 + n } else { n };
    if i < 0.0 || i >= items.len() as f64 {
        Ok(JsValue::Undefined)
    } else {
        Ok(items[i as usize].clone())
    }
}

fn arr_sort(it: &Interp, this: &JsValue, args: &[JsValue]) -> EvalResult<JsValue> {
    let o = as_array(this).ok_or_else(|| it.type_error("sort called on non-array"))?;
    let mut items = snapshot(&o);
    let cmp = args.first().cloned();
    // Insertion sort so a JS comparator (which returns EvalResult) can be used.
    for i in 1..items.len() {
        let mut j = i;
        while j > 0 {
            let order = match &cmp {
                Some(f) if matches!(f, JsValue::Object(_)) => {
                    it.call(f, &JsValue::Undefined, &[items[j - 1].clone(), items[j].clone()])?.to_number()
                }
                _ => {
                    // default: compare by string form
                    let a = items[j - 1].to_js_string();
                    let b = items[j].to_js_string();
                    if a > b { 1.0 } else { -1.0 }
                }
            };
            if order > 0.0 {
                items.swap(j - 1, j);
                j -= 1;
            } else {
                break;
            }
        }
    }
    if let ObjData::Array(slot) = &mut o.borrow_mut().data {
        *slot = items;
    }
    Ok(this.clone())
}

// ───────────────────────── string methods ─────────────────────────

fn this_str(this: &JsValue) -> String {
    this.to_js_string()
}

pub fn string_method(name: &str) -> Option<NativeFn> {
    Some(match name {
        "charAt" => str_char_at,
        "charCodeAt" => str_char_code_at,
        "at" => str_at,
        "slice" => str_slice,
        "substring" => str_substring,
        "indexOf" => str_index_of,
        "lastIndexOf" => str_last_index_of,
        "includes" => str_includes,
        "startsWith" => str_starts_with,
        "endsWith" => str_ends_with,
        "toUpperCase" => str_upper,
        "toLowerCase" => str_lower,
        "trim" => str_trim,
        "trimStart" => str_trim_start,
        "trimEnd" => str_trim_end,
        "split" => str_split,
        "repeat" => str_repeat,
        "replace" => str_replace,
        "replaceAll" => str_replace_all,
        "padStart" => str_pad_start,
        "padEnd" => str_pad_end,
        "concat" => str_concat,
        "toString" => str_to_string,
        _ => return None,
    })
}

fn chars(s: &str) -> Vec<char> {
    s.chars().collect()
}

fn str_char_at(_it: &Interp, this: &JsValue, args: &[JsValue]) -> EvalResult<JsValue> {
    let c = chars(&this_str(this));
    let i = arg(args, 0).to_number();
    if i < 0.0 || i >= c.len() as f64 {
        Ok(JsValue::str(""))
    } else {
        Ok(JsValue::str(c[i as usize].to_string()))
    }
}

fn str_char_code_at(_it: &Interp, this: &JsValue, args: &[JsValue]) -> EvalResult<JsValue> {
    let c = chars(&this_str(this));
    let i = arg(args, 0).to_number();
    if i < 0.0 || i >= c.len() as f64 {
        Ok(JsValue::Num(f64::NAN))
    } else {
        Ok(JsValue::Num(c[i as usize] as u32 as f64))
    }
}

fn str_at(_it: &Interp, this: &JsValue, args: &[JsValue]) -> EvalResult<JsValue> {
    let c = chars(&this_str(this));
    let n = arg(args, 0).to_number();
    let i = if n < 0.0 { c.len() as f64 + n } else { n };
    if i < 0.0 || i >= c.len() as f64 {
        Ok(JsValue::Undefined)
    } else {
        Ok(JsValue::str(c[i as usize].to_string()))
    }
}

fn str_slice(_it: &Interp, this: &JsValue, args: &[JsValue]) -> EvalResult<JsValue> {
    let c = chars(&this_str(this));
    let start = rel_index(&arg(args, 0), c.len(), 0);
    let end = rel_index(args.get(1).unwrap_or(&JsValue::Undefined), c.len(), c.len());
    let out: String = if end > start { c[start..end].iter().collect() } else { String::new() };
    Ok(JsValue::str(out))
}

fn str_substring(_it: &Interp, this: &JsValue, args: &[JsValue]) -> EvalResult<JsValue> {
    let c = chars(&this_str(this));
    let clamp = |v: &JsValue| -> usize {
        let n = v.to_number();
        if n.is_nan() || n < 0.0 {
            0
        } else {
            (n as usize).min(c.len())
        }
    };
    let mut a = clamp(&arg(args, 0));
    let mut b = if matches!(args.get(1), None | Some(JsValue::Undefined)) {
        c.len()
    } else {
        clamp(&arg(args, 1))
    };
    if a > b {
        std::mem::swap(&mut a, &mut b);
    }
    Ok(JsValue::str(c[a..b].iter().collect::<String>()))
}

fn str_index_of(_it: &Interp, this: &JsValue, args: &[JsValue]) -> EvalResult<JsValue> {
    let s = this_str(this);
    let needle = arg(args, 0).to_js_string();
    // byte->char index conversion for correctness with multibyte
    match s.find(&needle) {
        Some(byte) => Ok(JsValue::Num(s[..byte].chars().count() as f64)),
        None => Ok(JsValue::Num(-1.0)),
    }
}

fn str_last_index_of(_it: &Interp, this: &JsValue, args: &[JsValue]) -> EvalResult<JsValue> {
    let s = this_str(this);
    let needle = arg(args, 0).to_js_string();
    match s.rfind(&needle) {
        Some(byte) => Ok(JsValue::Num(s[..byte].chars().count() as f64)),
        None => Ok(JsValue::Num(-1.0)),
    }
}

fn str_includes(_it: &Interp, this: &JsValue, args: &[JsValue]) -> EvalResult<JsValue> {
    Ok(JsValue::Bool(this_str(this).contains(&arg(args, 0).to_js_string())))
}

fn str_starts_with(_it: &Interp, this: &JsValue, args: &[JsValue]) -> EvalResult<JsValue> {
    Ok(JsValue::Bool(this_str(this).starts_with(&arg(args, 0).to_js_string())))
}

fn str_ends_with(_it: &Interp, this: &JsValue, args: &[JsValue]) -> EvalResult<JsValue> {
    Ok(JsValue::Bool(this_str(this).ends_with(&arg(args, 0).to_js_string())))
}

fn str_upper(_it: &Interp, this: &JsValue, _a: &[JsValue]) -> EvalResult<JsValue> {
    Ok(JsValue::str(this_str(this).to_uppercase()))
}

fn str_lower(_it: &Interp, this: &JsValue, _a: &[JsValue]) -> EvalResult<JsValue> {
    Ok(JsValue::str(this_str(this).to_lowercase()))
}

fn str_trim(_it: &Interp, this: &JsValue, _a: &[JsValue]) -> EvalResult<JsValue> {
    Ok(JsValue::str(this_str(this).trim().to_string()))
}

fn str_trim_start(_it: &Interp, this: &JsValue, _a: &[JsValue]) -> EvalResult<JsValue> {
    Ok(JsValue::str(this_str(this).trim_start().to_string()))
}

fn str_trim_end(_it: &Interp, this: &JsValue, _a: &[JsValue]) -> EvalResult<JsValue> {
    Ok(JsValue::str(this_str(this).trim_end().to_string()))
}

fn str_split(_it: &Interp, this: &JsValue, args: &[JsValue]) -> EvalResult<JsValue> {
    let s = this_str(this);
    let out: Vec<JsValue> = match args.first() {
        None | Some(JsValue::Undefined) => vec![JsValue::str(s)],
        Some(sep) => {
            let sep = sep.to_js_string();
            if sep.is_empty() {
                s.chars().map(|c| JsValue::str(c.to_string())).collect()
            } else {
                s.split(&sep).map(JsValue::str).collect()
            }
        }
    };
    Ok(JsValue::Object(Object::array(out)))
}

fn str_repeat(it: &Interp, this: &JsValue, args: &[JsValue]) -> EvalResult<JsValue> {
    let n = arg(args, 0).to_number();
    if n < 0.0 || !n.is_finite() {
        return Err(it.range_error("Invalid count value"));
    }
    Ok(JsValue::str(this_str(this).repeat(n as usize)))
}

fn str_replace(_it: &Interp, this: &JsValue, args: &[JsValue]) -> EvalResult<JsValue> {
    // v0: string pattern only (regex/function replacers deferred), first match.
    let s = this_str(this);
    let pat = arg(args, 0).to_js_string();
    let rep = arg(args, 1).to_js_string();
    Ok(JsValue::str(s.replacen(&pat, &rep, 1)))
}

fn str_replace_all(_it: &Interp, this: &JsValue, args: &[JsValue]) -> EvalResult<JsValue> {
    let s = this_str(this);
    let pat = arg(args, 0).to_js_string();
    let rep = arg(args, 1).to_js_string();
    if pat.is_empty() {
        return Ok(JsValue::str(s));
    }
    Ok(JsValue::str(s.replace(&pat, &rep)))
}

fn str_pad_start(_it: &Interp, this: &JsValue, args: &[JsValue]) -> EvalResult<JsValue> {
    let s = this_str(this);
    let target = arg(args, 0).to_number() as usize;
    let pad = match args.get(1) {
        None | Some(JsValue::Undefined) => " ".to_string(),
        Some(v) => v.to_js_string(),
    };
    Ok(JsValue::str(pad_to(&s, target, &pad, true)))
}

fn str_pad_end(_it: &Interp, this: &JsValue, args: &[JsValue]) -> EvalResult<JsValue> {
    let s = this_str(this);
    let target = arg(args, 0).to_number() as usize;
    let pad = match args.get(1) {
        None | Some(JsValue::Undefined) => " ".to_string(),
        Some(v) => v.to_js_string(),
    };
    Ok(JsValue::str(pad_to(&s, target, &pad, false)))
}

fn pad_to(s: &str, target: usize, pad: &str, start: bool) -> String {
    let cur = s.chars().count();
    if cur >= target || pad.is_empty() {
        return s.to_string();
    }
    let need = target - cur;
    let padc: Vec<char> = pad.chars().collect();
    let fill: String = (0..need).map(|i| padc[i % padc.len()]).collect();
    if start {
        format!("{fill}{s}")
    } else {
        format!("{s}{fill}")
    }
}

fn str_concat(_it: &Interp, this: &JsValue, args: &[JsValue]) -> EvalResult<JsValue> {
    let mut s = this_str(this);
    for a in args {
        s.push_str(&a.to_js_string());
    }
    Ok(JsValue::str(s))
}

fn str_to_string(_it: &Interp, this: &JsValue, _a: &[JsValue]) -> EvalResult<JsValue> {
    Ok(JsValue::str(this_str(this)))
}

// ───────────────────────── number methods ─────────────────────────

pub fn number_method(name: &str) -> Option<NativeFn> {
    Some(match name {
        "toFixed" => num_to_fixed,
        "toString" => num_to_string_m,
        _ => return None,
    })
}

fn num_to_fixed(_it: &Interp, this: &JsValue, args: &[JsValue]) -> EvalResult<JsValue> {
    let n = this.to_number();
    if n.is_nan() {
        return Ok(JsValue::str("NaN"));
    }
    let digits = arg(args, 0).to_number();
    let d = if digits.is_finite() && digits >= 0.0 { (digits as usize).min(100) } else { 0 };
    // Per spec, |x| >= 1e21 (and non-finite) falls back to Number->string.
    if n.is_infinite() || n.abs() >= 1e21 {
        return Ok(JsValue::str(crate::value::num_to_string(n)));
    }
    let neg = n < 0.0; // `-0` is not < 0, so it gets no sign (matches node)
    let scale = 10f64.powi(d as i32);
    // Rust's round() is half-away-from-zero; on the positive magnitude that's the
    // spec's "ties toward the larger n" (whereas format!'s {:.N} rounds to even).
    let scaled = (n.abs() * scale).round();
    let int_str = format!("{scaled:.0}"); // integer-valued, no decimals
    let body = if d == 0 {
        int_str
    } else {
        let mut s = int_str;
        if s.len() <= d {
            s = format!("{}{}", "0".repeat(d + 1 - s.len()), s); // ensure one int digit
        }
        let point = s.len() - d;
        format!("{}.{}", &s[..point], &s[point..])
    };
    Ok(JsValue::str(if neg { format!("-{body}") } else { body }))
}

fn num_to_string_m(_it: &Interp, this: &JsValue, args: &[JsValue]) -> EvalResult<JsValue> {
    let n = this.to_number();
    match args.first() {
        None | Some(JsValue::Undefined) => Ok(JsValue::str(crate::value::num_to_string(n))),
        Some(radix) => {
            let r = radix.to_number() as u32;
            if r == 10 || !(2..=36).contains(&r) {
                return Ok(JsValue::str(crate::value::num_to_string(n)));
            }
            Ok(JsValue::str(int_to_radix(n, r)))
        }
    }
}

fn int_to_radix(n: f64, radix: u32) -> String {
    if n == 0.0 {
        return "0".into();
    }
    let neg = n < 0.0;
    let mut x = n.abs().trunc() as u64;
    let digits = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut out = Vec::new();
    while x > 0 {
        out.push(digits[(x % radix as u64) as usize]);
        x /= radix as u64;
    }
    if neg {
        out.push(b'-');
    }
    out.reverse();
    String::from_utf8(out).unwrap()
}
