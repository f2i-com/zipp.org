//! The dynamic JavaScript value model.
//!
//! v0 uses a tagged enum (correctness first). NaN-boxing / tagged pointers are a
//! later perf optimization. Objects, arrays and functions are all `Object`s
//! behind an `Rc<RefCell<…>>`; reference semantics + sharing fall out of that.
//! (Ref-counting leaks reference cycles — a real tracing GC is a later tier.)

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::rc::Rc;

use crate::ast::FuncDef;
use crate::env::Scope;
use crate::interp::{EvalResult, Interp};

/// A heap object handle (object, array, or function).
pub type Obj = Rc<RefCell<Object>>;

/// A native (Rust-implemented) function: `(interp, this, args) -> result`.
pub type NativeFn = fn(&Interp, &JsValue, &[JsValue]) -> EvalResult<JsValue>;

#[derive(Clone)]
pub enum JsValue {
    Undefined,
    Null,
    Bool(bool),
    Num(f64),
    Str(Rc<str>),
    Object(Obj),
}

/// A heap object: ordered named properties + a payload distinguishing plain
/// objects, arrays, and functions. (No prototype chain yet — methods on arrays/
/// strings are special-cased in [`crate::interp`]; real prototypes are a later
/// tier.)
pub struct Object {
    /// Named own properties, kept in insertion order via the `order` list.
    pub props: BTreeMap<String, JsValue>,
    pub order: Vec<String>,
    /// Accessor (getter/setter) properties, keyed by name. A key here takes
    /// precedence over `props` during get/set.
    pub accessors: BTreeMap<String, Accessor>,
    /// The prototype link (`[[Prototype]]`); `None` ends the chain.
    pub proto: Option<Obj>,
    pub data: ObjData,
}

/// A getter/setter pair (each a callable `JsValue`, or `None`).
#[derive(Clone, Default)]
pub struct Accessor {
    pub get: Option<JsValue>,
    pub set: Option<JsValue>,
}

pub enum ObjData {
    Plain,
    Array(Vec<JsValue>),
    Function {
        def: Rc<FuncDef>,
        scope: Rc<RefCell<Scope>>,
    },
    Native {
        name: String,
        f: NativeFn,
    },
}

impl Object {
    pub fn plain() -> Obj {
        Rc::new(RefCell::new(Object {
            props: BTreeMap::new(),
            order: Vec::new(),
            accessors: BTreeMap::new(),
            proto: None,
            data: ObjData::Plain,
        }))
    }
    pub fn array(items: Vec<JsValue>) -> Obj {
        Rc::new(RefCell::new(Object {
            props: BTreeMap::new(),
            order: Vec::new(),
            accessors: BTreeMap::new(),
            proto: None,
            data: ObjData::Array(items),
        }))
    }
    pub fn function(def: Rc<FuncDef>, scope: Rc<RefCell<Scope>>) -> Obj {
        Rc::new(RefCell::new(Object {
            props: BTreeMap::new(),
            order: Vec::new(),
            accessors: BTreeMap::new(),
            proto: None,
            data: ObjData::Function { def, scope },
        }))
    }
    pub fn native(name: &str, f: NativeFn) -> Obj {
        Rc::new(RefCell::new(Object {
            props: BTreeMap::new(),
            order: Vec::new(),
            accessors: BTreeMap::new(),
            proto: None,
            data: ObjData::Native { name: name.into(), f },
        }))
    }
    /// Set an own named property (preserving insertion order).
    pub fn set(&mut self, key: &str, v: JsValue) {
        if self.props.insert(key.to_string(), v).is_none() {
            self.order.push(key.to_string());
        }
    }
    /// Install/extend an accessor (getter and/or setter) for `key`.
    pub fn define_accessor(&mut self, key: &str, get: Option<JsValue>, set: Option<JsValue>) {
        if !self.accessors.contains_key(key) && !self.props.contains_key(key) {
            self.order.push(key.to_string());
        }
        let a = self.accessors.entry(key.to_string()).or_default();
        if get.is_some() {
            a.get = get;
        }
        if set.is_some() {
            a.set = set;
        }
    }
    pub fn is_callable(&self) -> bool {
        matches!(self.data, ObjData::Function { .. } | ObjData::Native { .. })
    }
}

impl JsValue {
    pub fn str(s: impl Into<Rc<str>>) -> JsValue {
        JsValue::Str(s.into())
    }

    /// `typeof` — the JS string tag.
    pub fn type_of(&self) -> &'static str {
        match self {
            JsValue::Undefined => "undefined",
            JsValue::Null => "object", // the famous historical quirk
            JsValue::Bool(_) => "boolean",
            JsValue::Num(_) => "number",
            JsValue::Str(_) => "string",
            JsValue::Object(o) => {
                if o.borrow().is_callable() {
                    "function"
                } else {
                    "object"
                }
            }
        }
    }

    /// ToBoolean (the "truthiness" rules).
    pub fn truthy(&self) -> bool {
        match self {
            JsValue::Undefined | JsValue::Null => false,
            JsValue::Bool(b) => *b,
            JsValue::Num(n) => *n != 0.0 && !n.is_nan(),
            JsValue::Str(s) => !s.is_empty(),
            JsValue::Object(_) => true,
        }
    }

    /// ToNumber (numeric coercion).
    pub fn to_number(&self) -> f64 {
        match self {
            JsValue::Undefined => f64::NAN,
            JsValue::Null => 0.0,
            JsValue::Bool(b) => {
                if *b {
                    1.0
                } else {
                    0.0
                }
            }
            JsValue::Num(n) => *n,
            JsValue::Str(s) => {
                let t = s.trim();
                if t.is_empty() {
                    0.0
                } else if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
                    i64::from_str_radix(hex, 16).map(|x| x as f64).unwrap_or(f64::NAN)
                } else {
                    t.parse::<f64>().unwrap_or(f64::NAN)
                }
            }
            JsValue::Object(_) => self.to_primitive_string().parse::<f64>().unwrap_or(f64::NAN),
        }
    }

    /// ToString (string coercion) — the value as a JS string.
    pub fn to_js_string(&self) -> String {
        match self {
            JsValue::Undefined => "undefined".into(),
            JsValue::Null => "null".into(),
            JsValue::Bool(b) => b.to_string(),
            JsValue::Num(n) => num_to_string(*n),
            JsValue::Str(s) => s.to_string(),
            JsValue::Object(_) => self.to_primitive_string(),
        }
    }

    /// ToPrimitive(string-ish) for objects: arrays join with `,`; functions and
    /// plain objects use the conventional tags. (No user `toString`/`valueOf`
    /// dispatch yet — a later tier with the prototype chain.)
    fn to_primitive_string(&self) -> String {
        let JsValue::Object(o) = self else {
            return self.to_js_string();
        };
        let b = o.borrow();
        match &b.data {
            ObjData::Array(items) => items
                .iter()
                .map(|v| match v {
                    JsValue::Undefined | JsValue::Null => String::new(),
                    other => other.to_js_string(),
                })
                .collect::<Vec<_>>()
                .join(","),
            ObjData::Function { def, .. } => {
                format!("function {}() {{ [native code] }}", def.name.as_deref().unwrap_or(""))
            }
            ObjData::Native { name, .. } => format!("function {name}() {{ [native code] }}"),
            ObjData::Plain => "[object Object]".into(),
        }
    }

    /// Strict equality (`===`).
    pub fn strict_eq(&self, other: &JsValue) -> bool {
        match (self, other) {
            (JsValue::Undefined, JsValue::Undefined) => true,
            (JsValue::Null, JsValue::Null) => true,
            (JsValue::Bool(a), JsValue::Bool(b)) => a == b,
            (JsValue::Num(a), JsValue::Num(b)) => a == b, // NaN != NaN falls out
            (JsValue::Str(a), JsValue::Str(b)) => a == b,
            (JsValue::Object(a), JsValue::Object(b)) => Rc::ptr_eq(a, b),
            _ => false,
        }
    }

    /// Loose equality (`==`) — the abstract-equality coercion rules (the common
    /// cases; BigInt and some object-primitive corners are deferred).
    pub fn loose_eq(&self, other: &JsValue) -> bool {
        use JsValue::*;
        match (self, other) {
            (Undefined | Null, Undefined | Null) => true,
            (Num(_), Num(_)) | (Str(_), Str(_)) | (Bool(_), Bool(_)) | (Object(_), Object(_)) => {
                self.strict_eq(other)
            }
            (Num(_), Str(_)) | (Str(_), Num(_)) => self.to_number() == other.to_number(),
            (Bool(_), _) => JsValue::Num(self.to_number()).loose_eq(other),
            (_, Bool(_)) => self.loose_eq(&JsValue::Num(other.to_number())),
            (Object(_), Num(_) | Str(_)) => JsValue::str(self.to_primitive_string()).loose_eq(other),
            (Num(_) | Str(_), Object(_)) => self.loose_eq(&JsValue::str(other.to_primitive_string())),
            _ => false,
        }
    }

    /// A `console.log`-style display (Node-ish): strings unquoted at top level,
    /// objects/arrays inspected. Good enough for the common cases; full
    /// `util.inspect` fidelity is a later refinement.
    pub fn display(&self) -> String {
        self.inspect(true, &mut Vec::new())
    }

    fn inspect(&self, top: bool, seen: &mut Vec<*const RefCell<Object>>) -> String {
        match self {
            JsValue::Str(s) => {
                if top {
                    s.to_string()
                } else {
                    format!("'{s}'")
                }
            }
            JsValue::Object(o) => {
                let ptr = Rc::as_ptr(o);
                if seen.contains(&ptr) {
                    return "[Circular]".into();
                }
                seen.push(ptr);
                let b = o.borrow();
                let out = match &b.data {
                    ObjData::Array(items) => {
                        let inner = items
                            .iter()
                            .map(|v| v.inspect(false, seen))
                            .collect::<Vec<_>>()
                            .join(", ");
                        if inner.is_empty() {
                            "[]".into()
                        } else {
                            format!("[ {inner} ]")
                        }
                    }
                    ObjData::Function { def, .. } => {
                        format!("[Function: {}]", def.name.as_deref().unwrap_or("anonymous"))
                    }
                    ObjData::Native { name, .. } => format!("[Function: {name}]"),
                    ObjData::Plain => {
                        let mut s = String::new();
                        for (i, k) in b.order.iter().enumerate() {
                            if i > 0 {
                                s.push_str(", ");
                            }
                            let v = b.props.get(k).cloned().unwrap_or(JsValue::Undefined);
                            let _ = write!(s, "{}: {}", ident_or_quoted(k), v.inspect(false, seen));
                        }
                        if s.is_empty() {
                            "{}".into()
                        } else {
                            format!("{{ {s} }}")
                        }
                    }
                };
                seen.pop();
                out
            }
            other => other.to_js_string(),
        }
    }
}

/// JS number → string (integers without a decimal, `Infinity`/`NaN`, etc.).
/// JS `Number.prototype.toString()` (base 10) — the ECMAScript Number::toString
/// algorithm. Rust's `{:e}` gives the shortest round-trip mantissa+exponent
/// (`d[.ddd]eEXP`, value = mantissa × 10^EXP), which supplies the spec's digit
/// string `s` (k digits) and decimal-point position `point = EXP + 1`.
pub fn num_to_string(n: f64) -> String {
    if n.is_nan() {
        return "NaN".into();
    }
    if n == 0.0 {
        return "0".into(); // also handles -0 → "0"
    }
    if n.is_infinite() {
        return if n < 0.0 { "-Infinity".into() } else { "Infinity".into() };
    }
    let neg = n < 0.0;
    let m = n.abs();
    let e = format!("{m:e}"); // e.g. "1e21", "1.5e-10", "2.555e2"
    let (mant, exp_str) = e.split_once('e').expect("{:e} always contains 'e'");
    let exp: i32 = exp_str.parse().expect("valid exponent from {:e}");
    let digits: String = mant.chars().filter(|c| *c != '.').collect();
    let s = digits.as_str();
    let k = s.len() as i32; // number of significant digits
    let point = exp + 1; // spec `n`: digit-string position of the decimal point

    let body = if k <= point && point <= 21 {
        // integer, possibly with trailing zeros: e.g. 1e20 → "1" + 20 zeros
        format!("{}{}", s, "0".repeat((point - k) as usize))
    } else if 0 < point && point <= 21 {
        // decimal point falls inside the digits: e.g. 255.5 → "255" "." "5"
        format!("{}.{}", &s[..point as usize], &s[point as usize..])
    } else if -6 < point && point <= 0 {
        // small fraction: e.g. 0.001 → "0." + 2 zeros + "1"
        format!("0.{}{}", "0".repeat((-point) as usize), s)
    } else {
        // exponential: d[.rest]e±(point-1)
        let exp_part = point - 1;
        let sign = if exp_part >= 0 { "+" } else { "-" };
        let mag = exp_part.unsigned_abs();
        if k == 1 {
            format!("{s}e{sign}{mag}")
        } else {
            format!("{}.{}e{sign}{mag}", &s[..1], &s[1..])
        }
    };
    if neg {
        format!("-{body}")
    } else {
        body
    }
}

fn ident_or_quoted(k: &str) -> String {
    let is_ident = !k.is_empty()
        && k.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '$')
        && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$');
    if is_ident {
        k.to_string()
    } else {
        format!("'{k}'")
    }
}
