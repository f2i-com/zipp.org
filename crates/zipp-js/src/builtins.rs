//! The global environment: `console`, `Math`, `JSON`, `Object`, `Array`,
//! `Number`, and the global functions (`parseInt`, `parseFloat`, `isNaN`, …).
//!
//! A core set; grows toward full ECMAScript coverage over time.

use crate::interp::{EvalResult, Interp};
use crate::value::{JsValue, NativeFn, ObjData, Object};

fn nf(name: &str, f: NativeFn) -> JsValue {
    JsValue::Object(Object::native(name, f))
}

/// `args[i]` ToNumber, or NaN if absent.
fn n(a: &[JsValue], i: usize) -> f64 {
    a.get(i).map(|v| v.to_number()).unwrap_or(f64::NAN)
}

/// Install all globals into the interpreter's global scope.
pub fn install(it: &Interp) {
    let g = &it.global;
    let decl = |name: &str, v: JsValue| g.borrow_mut().declare(name, v);

    // value globals
    decl("undefined", JsValue::Undefined);
    decl("NaN", JsValue::Num(f64::NAN));
    decl("Infinity", JsValue::Num(f64::INFINITY));

    // console
    let console = Object::plain();
    {
        let mut c = console.borrow_mut();
        c.set("log", nf("log", console_log));
        c.set("error", nf("error", console_log));
        c.set("warn", nf("warn", console_log));
        c.set("info", nf("info", console_log));
        c.set("debug", nf("debug", console_log));
    }
    decl("console", JsValue::Object(console));

    // Math
    let math = Object::plain();
    {
        let mut m = math.borrow_mut();
        m.set("PI", JsValue::Num(std::f64::consts::PI));
        m.set("E", JsValue::Num(std::f64::consts::E));
        m.set("LN2", JsValue::Num(std::f64::consts::LN_2));
        m.set("LN10", JsValue::Num(std::f64::consts::LN_10));
        m.set("SQRT2", JsValue::Num(std::f64::consts::SQRT_2));
        m.set("abs", nf("abs", |_, _, a| Ok(JsValue::Num(n(a, 0).abs()))));
        m.set("floor", nf("floor", |_, _, a| Ok(JsValue::Num(n(a, 0).floor()))));
        m.set("ceil", nf("ceil", |_, _, a| Ok(JsValue::Num(n(a, 0).ceil()))));
        m.set("round", nf("round", |_, _, a| Ok(JsValue::Num(js_round(n(a, 0))))));
        m.set("trunc", nf("trunc", |_, _, a| Ok(JsValue::Num(n(a, 0).trunc()))));
        m.set("sign", nf("sign", |_, _, a| Ok(JsValue::Num(js_sign(n(a, 0))))));
        m.set("sqrt", nf("sqrt", |_, _, a| Ok(JsValue::Num(n(a, 0).sqrt()))));
        m.set("cbrt", nf("cbrt", |_, _, a| Ok(JsValue::Num(n(a, 0).cbrt()))));
        m.set("pow", nf("pow", |_, _, a| {
            let (base, exp) = (n(a, 0), n(a, 1));
            // V8's Math.pow delegates to std::pow, where pow(1, y) == 1 for any
            // y including NaN. Rust's powf returns NaN for 1.powf(NaN) on some
            // platforms (e.g. Windows), so pin the base==1 case. (The `**`
            // operator stays spec-correct -> NaN, matching V8.)
            let r = if base == 1.0 { 1.0 } else { base.powf(exp) };
            Ok(JsValue::Num(r))
        }));
        m.set("exp", nf("exp", |_, _, a| Ok(JsValue::Num(n(a, 0).exp()))));
        m.set("log", nf("log", |_, _, a| Ok(JsValue::Num(n(a, 0).ln()))));
        m.set("log2", nf("log2", |_, _, a| Ok(JsValue::Num(n(a, 0).log2()))));
        m.set("log10", nf("log10", |_, _, a| Ok(JsValue::Num(n(a, 0).log10()))));
        m.set("sin", nf("sin", |_, _, a| Ok(JsValue::Num(n(a, 0).sin()))));
        m.set("cos", nf("cos", |_, _, a| Ok(JsValue::Num(n(a, 0).cos()))));
        m.set("tan", nf("tan", |_, _, a| Ok(JsValue::Num(n(a, 0).tan()))));
        m.set("atan", nf("atan", |_, _, a| Ok(JsValue::Num(n(a, 0).atan()))));
        m.set("atan2", nf("atan2", |_, _, a| Ok(JsValue::Num(n(a, 0).atan2(n(a, 1))))));
        m.set("hypot", nf("hypot", |_, _, a| Ok(JsValue::Num(a.iter().map(|v| v.to_number().powi(2)).sum::<f64>().sqrt()))));
        m.set("min", nf("min", math_min));
        m.set("max", nf("max", math_max));
        m.set("random", nf("random", math_random));
    }
    decl("Math", JsValue::Object(math));

    // JSON
    let json = Object::plain();
    {
        let mut j = json.borrow_mut();
        j.set("stringify", nf("stringify", crate::json::stringify));
        j.set("parse", nf("parse", crate::json::parse));
    }
    decl("JSON", JsValue::Object(json));

    // Object (static methods; also a no-op constructor returning {})
    let object = Object::native("Object", obj_ctor);
    {
        let mut o = object.borrow_mut();
        o.set("keys", nf("keys", obj_keys));
        o.set("values", nf("values", obj_values));
        o.set("entries", nf("entries", obj_entries));
        o.set("assign", nf("assign", obj_assign));
        o.set("freeze", nf("freeze", |_, _, a| Ok(a.first().cloned().unwrap_or(JsValue::Undefined))));
        o.set("create", nf("create", |_, _, _| Ok(JsValue::Object(Object::plain()))));
    }
    decl("Object", JsValue::Object(object));

    // Array (static methods)
    let array = Object::native("Array", array_ctor);
    {
        let mut a = array.borrow_mut();
        a.set("isArray", nf("isArray", |_, _, a| Ok(JsValue::Bool(matches!(a.first(), Some(JsValue::Object(o)) if matches!(o.borrow().data, ObjData::Array(_)))))));
        a.set("from", nf("from", array_from));
        a.set("of", nf("of", |_, _, a| Ok(JsValue::Object(Object::array(a.to_vec())))));
    }
    decl("Array", JsValue::Object(array));

    // Number (static)
    let number = Object::native("Number", |_, _, a| Ok(JsValue::Num(a.first().map(|v| v.to_number()).unwrap_or(0.0))));
    {
        let mut nm = number.borrow_mut();
        nm.set("isInteger", nf("isInteger", |_, _, a| Ok(JsValue::Bool(matches!(a.first(), Some(JsValue::Num(x)) if x.is_finite() && x.fract() == 0.0)))));
        nm.set("isFinite", nf("isFinite", |_, _, a| Ok(JsValue::Bool(matches!(a.first(), Some(JsValue::Num(x)) if x.is_finite())))));
        nm.set("isNaN", nf("isNaN", |_, _, a| Ok(JsValue::Bool(matches!(a.first(), Some(JsValue::Num(x)) if x.is_nan())))));
        nm.set("parseFloat", nf("parseFloat", global_parse_float));
        nm.set("parseInt", nf("parseInt", global_parse_int));
        nm.set("MAX_SAFE_INTEGER", JsValue::Num(9007199254740991.0));
        nm.set("MIN_SAFE_INTEGER", JsValue::Num(-9007199254740991.0));
        nm.set("EPSILON", JsValue::Num(f64::EPSILON));
        nm.set("MAX_VALUE", JsValue::Num(f64::MAX));
        // JS Number.MIN_VALUE is the smallest positive *denormal* (5e-324 =
        // f64::from_bits(1)), NOT Rust's f64::MIN_POSITIVE (smallest *normal*).
        nm.set("MIN_VALUE", JsValue::Num(f64::from_bits(1)));
        nm.set("POSITIVE_INFINITY", JsValue::Num(f64::INFINITY));
        nm.set("NEGATIVE_INFINITY", JsValue::Num(f64::NEG_INFINITY));
        nm.set("NaN", JsValue::Num(f64::NAN));
    }
    decl("Number", JsValue::Object(number));

    // String (callable + fromCharCode)
    let string = Object::native("String", |it, _, a| {
        Ok(JsValue::str(match a.first() {
            Some(v) => it.to_string(v)?, // user toString dispatch for objects
            None => String::new(),
        }))
    });
    string.borrow_mut().set("fromCharCode", nf("fromCharCode", str_from_char_code));
    decl("String", JsValue::Object(string));

    // Boolean (callable)
    decl("Boolean", nf("Boolean", |_, _, a| Ok(JsValue::Bool(a.first().map(|v| v.truthy()).unwrap_or(false)))));

    // Error hierarchy: `Error` plus the standard subclasses, with linked
    // prototypes so `new TypeError(m) instanceof Error` is true. Each prototype
    // is registered in `it.error_protos` so engine-thrown errors share it.
    let error_proto = Object::plain();
    {
        let mut b = error_proto.borrow_mut();
        b.set("name", JsValue::str("Error"));
        b.set("message", JsValue::str(""));
        b.set("toString", nf("toString", error_to_string));
    }
    let error_ctor = Object::native("Error", error_ctor_fn);
    error_ctor.borrow_mut().set("prototype", JsValue::Object(error_proto.clone()));
    it.error_protos.borrow_mut().insert("Error".into(), error_proto.clone());
    decl("Error", JsValue::Object(error_ctor));
    for sub in ["TypeError", "RangeError", "SyntaxError", "ReferenceError", "EvalError", "URIError"] {
        let proto = Object::plain();
        {
            let mut b = proto.borrow_mut();
            b.set("name", JsValue::str(sub));
            b.set("message", JsValue::str(""));
            b.proto = Some(error_proto.clone()); // subclass.prototype.[[Proto]] = Error.prototype
        }
        let ctor = Object::native(sub, error_ctor_fn);
        ctor.borrow_mut().set("prototype", JsValue::Object(proto.clone()));
        it.error_protos.borrow_mut().insert(sub.to_string(), proto.clone());
        decl(sub, JsValue::Object(ctor));
    }

    // global functions
    decl("parseInt", nf("parseInt", global_parse_int));
    decl("parseFloat", nf("parseFloat", global_parse_float));
    decl("isNaN", nf("isNaN", |_, _, a| Ok(JsValue::Bool(n(a, 0).is_nan()))));
    decl("isFinite", nf("isFinite", |_, _, a| Ok(JsValue::Bool(n(a, 0).is_finite()))));
}

/// `Error(msg)` / `new Error(msg)` (shared by all error subclasses — `name`
/// comes from the prototype). With `new`, `this` is the fresh object; called
/// plainly, a fresh `Error`-proto'd object is created.
fn error_ctor_fn(it: &Interp, this: &JsValue, args: &[JsValue]) -> EvalResult<JsValue> {
    let target = if matches!(this, JsValue::Object(_)) {
        this.clone()
    } else {
        let o = Object::plain();
        if let Some(p) = it.error_protos.borrow().get("Error") {
            o.borrow_mut().proto = Some(p.clone());
        }
        JsValue::Object(o)
    };
    if let Some(m) = args.first() {
        if !matches!(m, JsValue::Undefined) {
            it.set_member(&target, "message", JsValue::str(m.to_js_string()))?;
        }
    }
    let name = it.get_member(&target, "name")?.to_js_string();
    let msg = it.get_member(&target, "message")?.to_js_string();
    let stack = if msg.is_empty() { name } else { format!("{name}: {msg}") };
    it.set_member(&target, "stack", JsValue::str(stack))?;
    // `new` ignores a non-object return, so returning `target` is correct for
    // both `new Error()` (returns target == this) and `Error()` (returns it).
    Ok(target)
}

/// `Error.prototype.toString()` → `name`, or `name: message`.
fn error_to_string(it: &Interp, this: &JsValue, _args: &[JsValue]) -> EvalResult<JsValue> {
    let name = it.get_member(this, "name")?.to_js_string();
    let msg = it.get_member(this, "message")?.to_js_string();
    Ok(JsValue::str(if msg.is_empty() {
        name
    } else if name.is_empty() {
        msg
    } else {
        format!("{name}: {msg}")
    }))
}

fn console_log(it: &Interp, _this: &JsValue, args: &[JsValue]) -> EvalResult<JsValue> {
    let line = args.iter().map(|a| a.display()).collect::<Vec<_>>().join(" ");
    it.out.borrow_mut().push(line);
    Ok(JsValue::Undefined)
}

fn js_round(x: f64) -> f64 {
    // JS rounds .5 toward +Infinity (Rust's round is half-away-from-zero).
    // NaN/±Inf/integers (incl. -0) pass through unchanged.
    if x.is_nan() || x.is_infinite() || x.fract() == 0.0 {
        return x;
    }
    let r = (x + 0.5).floor();
    // A negative value in [-0.5, 0) rounds to -0, not +0.
    if r == 0.0 && x < 0.0 {
        -0.0
    } else {
        r
    }
}

fn js_sign(x: f64) -> f64 {
    if x.is_nan() {
        f64::NAN
    } else if x > 0.0 {
        1.0
    } else if x < 0.0 {
        -1.0
    } else {
        x // preserves -0 / +0
    }
}

fn math_min(_: &Interp, _: &JsValue, a: &[JsValue]) -> EvalResult<JsValue> {
    let mut m = f64::INFINITY;
    for v in a {
        let x = v.to_number();
        if x.is_nan() {
            return Ok(JsValue::Num(f64::NAN));
        }
        // `<` treats +0 == -0; per spec Math.min prefers -0 over +0.
        if x < m || (x == 0.0 && m == 0.0 && x.is_sign_negative() && m.is_sign_positive()) {
            m = x;
        }
    }
    Ok(JsValue::Num(m))
}

fn math_max(_: &Interp, _: &JsValue, a: &[JsValue]) -> EvalResult<JsValue> {
    let mut m = f64::NEG_INFINITY;
    for v in a {
        let x = v.to_number();
        if x.is_nan() {
            return Ok(JsValue::Num(f64::NAN));
        }
        // `>` treats +0 == -0; per spec Math.max prefers +0 over -0.
        if x > m || (x == 0.0 && m == 0.0 && x.is_sign_positive() && m.is_sign_negative()) {
            m = x;
        }
    }
    Ok(JsValue::Num(m))
}

// A small deterministic xorshift PRNG (v0: reproducible; real entropy later).
thread_local! {
    static RNG: std::cell::Cell<u64> = const { std::cell::Cell::new(0x2545F4914F6CDD1D) };
}
fn math_random(_: &Interp, _: &JsValue, _: &[JsValue]) -> EvalResult<JsValue> {
    let x = RNG.with(|s| {
        let mut x = s.get();
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        s.set(x);
        x
    });
    // top 53 bits → [0, 1)
    Ok(JsValue::Num((x >> 11) as f64 / (1u64 << 53) as f64))
}

fn obj_ctor(_: &Interp, _: &JsValue, a: &[JsValue]) -> EvalResult<JsValue> {
    match a.first() {
        Some(v @ JsValue::Object(_)) => Ok(v.clone()),
        _ => Ok(JsValue::Object(Object::plain())),
    }
}

fn own_keys(v: &JsValue) -> Vec<String> {
    match v {
        JsValue::Object(o) => {
            let b = o.borrow();
            match &b.data {
                ObjData::Array(items) => (0..items.len()).map(|i| i.to_string()).collect(),
                _ => b.order.clone(),
            }
        }
        _ => Vec::new(),
    }
}

fn obj_keys(_: &Interp, _: &JsValue, a: &[JsValue]) -> EvalResult<JsValue> {
    let keys = own_keys(&a.first().cloned().unwrap_or(JsValue::Undefined));
    Ok(JsValue::Object(Object::array(keys.into_iter().map(JsValue::str).collect())))
}

fn obj_values(it: &Interp, _: &JsValue, a: &[JsValue]) -> EvalResult<JsValue> {
    let v = a.first().cloned().unwrap_or(JsValue::Undefined);
    let mut out = Vec::new();
    for k in own_keys(&v) {
        out.push(it.get_member(&v, &k)?);
    }
    Ok(JsValue::Object(Object::array(out)))
}

fn obj_entries(it: &Interp, _: &JsValue, a: &[JsValue]) -> EvalResult<JsValue> {
    let v = a.first().cloned().unwrap_or(JsValue::Undefined);
    let mut out = Vec::new();
    for k in own_keys(&v) {
        let val = it.get_member(&v, &k)?;
        out.push(JsValue::Object(Object::array(vec![JsValue::str(k), val])));
    }
    Ok(JsValue::Object(Object::array(out)))
}

fn obj_assign(it: &Interp, _: &JsValue, a: &[JsValue]) -> EvalResult<JsValue> {
    let target = a.first().cloned().unwrap_or(JsValue::Undefined);
    for src in a.iter().skip(1) {
        for k in own_keys(src) {
            let v = it.get_member(src, &k)?;
            it.set_member(&target, &k, v)?;
        }
    }
    Ok(target)
}

fn array_ctor(_: &Interp, _: &JsValue, a: &[JsValue]) -> EvalResult<JsValue> {
    // Array(n) → empty array of length n; Array(a, b, …) → [a, b, …].
    if a.len() == 1 {
        if let JsValue::Num(len) = a[0] {
            return Ok(JsValue::Object(Object::array(vec![JsValue::Undefined; len as usize])));
        }
    }
    Ok(JsValue::Object(Object::array(a.to_vec())))
}

fn array_from(it: &Interp, _: &JsValue, a: &[JsValue]) -> EvalResult<JsValue> {
    let src = a.first().cloned().unwrap_or(JsValue::Undefined);
    let mut items: Vec<JsValue> = match &src {
        JsValue::Object(o) if matches!(o.borrow().data, ObjData::Array(_)) => {
            if let ObjData::Array(v) = &o.borrow().data {
                v.clone()
            } else {
                Vec::new()
            }
        }
        JsValue::Str(s) => s.chars().map(|c| JsValue::str(c.to_string())).collect(),
        JsValue::Object(o) => {
            // array-like: { length, 0, 1, … }
            let len = it.get_member(&src, "length")?.to_number();
            let _ = o;
            let mut v = Vec::new();
            let len = if len.is_finite() && len > 0.0 { len as usize } else { 0 };
            for i in 0..len {
                v.push(it.get_member(&src, &i.to_string())?);
            }
            v
        }
        _ => Vec::new(),
    };
    if let Some(cb) = a.get(1) {
        if matches!(cb, JsValue::Object(_)) {
            for (i, item) in items.iter_mut().enumerate() {
                *item = it.call(cb, &JsValue::Undefined, &[item.clone(), JsValue::Num(i as f64)])?;
            }
        }
    }
    Ok(JsValue::Object(Object::array(items)))
}

fn str_from_char_code(_: &Interp, _: &JsValue, a: &[JsValue]) -> EvalResult<JsValue> {
    let s: String = a
        .iter()
        .filter_map(|v| char::from_u32(v.to_number() as u32))
        .collect();
    Ok(JsValue::str(s))
}

fn global_parse_int(_: &Interp, _: &JsValue, a: &[JsValue]) -> EvalResult<JsValue> {
    let s = a.first().map(|v| v.to_js_string()).unwrap_or_default();
    let s = s.trim();
    let radix = a.get(1).map(|v| v.to_number()).filter(|r| r.is_finite()).map(|r| r as u32).unwrap_or(0);
    let (neg, body) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let (radix, body) = if radix == 16 || radix == 0 {
        if let Some(h) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
            (16, h)
        } else {
            (if radix == 0 { 10 } else { radix }, body)
        }
    } else {
        (radix, body)
    };
    if !(2..=36).contains(&radix) {
        return Ok(JsValue::Num(f64::NAN));
    }
    let digits: String = body.chars().take_while(|c| c.to_digit(radix).is_some()).collect();
    if digits.is_empty() {
        return Ok(JsValue::Num(f64::NAN));
    }
    let mut val = 0.0;
    for c in digits.chars() {
        val = val * radix as f64 + c.to_digit(radix).unwrap() as f64;
    }
    Ok(JsValue::Num(if neg { -val } else { val }))
}

fn global_parse_float(_: &Interp, _: &JsValue, a: &[JsValue]) -> EvalResult<JsValue> {
    let s = a.first().map(|v| v.to_js_string()).unwrap_or_default();
    let s = s.trim();
    // longest leading prefix that parses as a float
    let mut end = 0;
    let bytes = s.as_bytes();
    let mut seen_dot = false;
    let mut seen_e = false;
    while end < bytes.len() {
        let c = bytes[end] as char;
        let ok = c.is_ascii_digit()
            || (c == '.' && !seen_dot && !seen_e)
            || ((c == 'e' || c == 'E') && !seen_e && end > 0)
            || ((c == '+' || c == '-') && (end == 0 || matches!(bytes[end - 1] as char, 'e' | 'E')));
        if c == '.' {
            seen_dot = true;
        }
        if c == 'e' || c == 'E' {
            seen_e = true;
        }
        if ok {
            end += 1;
        } else {
            break;
        }
    }
    match s[..end].parse::<f64>() {
        Ok(v) => Ok(JsValue::Num(v)),
        Err(_) => {
            if s.starts_with("Infinity") || s.starts_with("+Infinity") {
                Ok(JsValue::Num(f64::INFINITY))
            } else if s.starts_with("-Infinity") {
                Ok(JsValue::Num(f64::NEG_INFINITY))
            } else {
                Ok(JsValue::Num(f64::NAN))
            }
        }
    }
}
