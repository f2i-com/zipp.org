//! Tests for `py_table`: the table against a reference model, and the
//! native hash, equality and set order against the Python runtime itself
//! (programs run through the frontend; the same values built natively).

use super::*;
use crate::heap::{JsStr, ObjMap};

/// xorshift64*: deterministic test randomness.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// A heap with stand-ins for the runtime's `tuple` and `frozenset` types.
struct Env {
    heap: Heap,
    kinds: PyKinds,
}

impl Env {
    fn new() -> Env {
        let mut heap = Heap::new();
        let t = Value::heap(heap.alloc(HeapObj::Object(Box::new(ObjMap::new()))));
        let f = Value::heap(heap.alloc(HeapObj::Object(Box::new(ObjMap::new()))));
        Env {
            heap,
            kinds: PyKinds { tuple: t, frozenset: f },
        }
    }
    fn int(&mut self, digits: &str) -> Value {
        let n: NumBig = digits.parse().expect("int literal");
        match i128::try_from(&n) {
            Ok(v) => Value::heap(self.heap.alloc(HeapObj::BigInt(v))),
            Err(_) => Value::heap(self.heap.alloc(HeapObj::BigIntBig(Box::new(n)))),
        }
    }
    fn str_units(&mut self, units: &[u16]) -> Value {
        let mut bytes = Vec::new();
        for r in char::decode_utf16(units.iter().copied()) {
            let cp = match r {
                Ok(c) => c as u32,
                Err(e) => e.unpaired_surrogate() as u32,
            };
            // WTF-8: a lone surrogate encodes like any 3-byte code point.
            if cp < 0x80 {
                bytes.push(cp as u8);
            } else if cp < 0x800 {
                bytes.extend([0xC0 | (cp >> 6) as u8, 0x80 | (cp & 0x3F) as u8]);
            } else if cp < 0x10000 {
                bytes.extend([0xE0 | (cp >> 12) as u8, 0x80 | ((cp >> 6) & 0x3F) as u8, 0x80 | (cp & 0x3F) as u8]);
            } else {
                bytes.extend([
                    0xF0 | (cp >> 18) as u8,
                    0x80 | ((cp >> 12) & 0x3F) as u8,
                    0x80 | ((cp >> 6) & 0x3F) as u8,
                    0x80 | (cp & 0x3F) as u8,
                ]);
            }
        }
        Value::heap(self.heap.alloc_js(JsStr::from_wtf8(bytes)))
    }
    fn record(&mut self, cls: Value, field: &str, payload: Value) -> Value {
        let mut m = ObjMap::new();
        m.set("cls", cls);
        m.set(field, payload);
        Value::heap(self.heap.alloc(HeapObj::Object(Box::new(m))))
    }
    fn tuple(&mut self, items: Vec<Value>) -> Value {
        let arr = Value::heap(self.heap.alloc(HeapObj::Array(items)));
        self.record(self.kinds.tuple, "items", arr)
    }
    /// A frozenset as the runtime stores one today: a Map of bucket arrays.
    fn frozen(&mut self, items: Vec<Value>) -> Value {
        let mut keys = Vec::new();
        let mut vals = Vec::new();
        for (i, x) in items.into_iter().enumerate() {
            keys.push(Value::int(i as i32));
            vals.push(Value::heap(self.heap.alloc(HeapObj::Array(vec![x]))));
        }
        let map = Value::heap(self.heap.alloc(HeapObj::Map { keys, vals }));
        self.record(self.kinds.frozenset, "map", map)
    }
    fn hash(&self, v: Value) -> Option<i64> {
        native_hash(&self.heap, &self.kinds, v, &mut 0)
    }
    fn eq(&self, a: Value, b: Value) -> Option<bool> {
        native_eq(&self.heap, &self.kinds, a, b, &mut 0)
    }
}

/// A Python value, rendered both as Python source and as engine values.
#[derive(Clone, Debug)]
enum Spec {
    Int(String),
    Float(f64),
    Str(Vec<u16>),
    Bool(bool),
    None,
    Tuple(Vec<Spec>),
    Frozen(Vec<Spec>),
}

fn int(s: &str) -> Spec {
    Spec::Int(s.to_string())
}
fn st(s: &str) -> Spec {
    Spec::Str(s.encode_utf16().collect())
}

impl Spec {
    fn py(&self) -> String {
        match self {
            Spec::Int(d) => format!("({d})"),
            Spec::Float(f) if f.is_nan() => "float('nan')".into(),
            Spec::Float(f) if f.is_infinite() => format!("float('{}inf')", if *f < 0.0 { "-" } else { "" }),
            Spec::Float(f) => format!("({f:?})"),
            Spec::Str(units) => {
                let mut parts = Vec::new();
                let mut plain = String::new();
                for &u in units {
                    let c = u as u32;
                    if (0x20..0x7f).contains(&c) && c != b'\'' as u32 && c != b'\\' as u32 {
                        plain.push(c as u8 as char);
                    } else {
                        if !plain.is_empty() {
                            parts.push(format!("'{plain}'"));
                            plain.clear();
                        }
                        // One code unit at a time: the runtime's str is UTF-16.
                        parts.push(format!("chr({c})"));
                    }
                }
                if !plain.is_empty() || parts.is_empty() {
                    parts.push(format!("'{plain}'"));
                }
                format!("({})", parts.join(" + "))
            }
            Spec::Bool(b) => (if *b { "True" } else { "False" }).into(),
            Spec::None => "None".into(),
            Spec::Tuple(items) => {
                let inner: Vec<String> = items.iter().map(Spec::py).collect();
                if inner.is_empty() {
                    "()".into()
                } else {
                    format!("({},)", inner.join(", "))
                }
            }
            Spec::Frozen(items) => {
                let inner: Vec<String> = items.iter().map(Spec::py).collect();
                format!("frozenset([{}])", inner.join(", "))
            }
        }
    }
    /// Python's repr, for the values the order tests use.
    fn repr(&self) -> String {
        match self {
            Spec::Int(d) => d.clone(),
            Spec::Float(f) if f.is_nan() => "nan".into(),
            Spec::Float(f) if f.is_infinite() => (if *f < 0.0 { "-inf" } else { "inf" }).into(),
            Spec::Float(f) => format!("{f:?}"),
            Spec::Str(u) => format!("'{}'", String::from_utf16(u).expect("ascii")),
            Spec::Bool(b) => (if *b { "True" } else { "False" }).into(),
            Spec::None => "None".into(),
            Spec::Tuple(items) => {
                let inner: Vec<String> = items.iter().map(Spec::repr).collect();
                if inner.len() == 1 {
                    format!("({},)", inner[0])
                } else {
                    format!("({})", inner.join(", "))
                }
            }
            Spec::Frozen(_) => unreachable!("not used in order tests"),
        }
    }
    fn build(&self, env: &mut Env) -> Value {
        match self {
            Spec::Int(d) => env.int(d),
            Spec::Float(f) => Value::num(*f),
            Spec::Str(u) => env.str_units(u),
            Spec::Bool(b) => Value::bool(*b),
            Spec::None => Value::NULL,
            Spec::Tuple(items) => {
                let vs = items.iter().map(|s| s.build(env)).collect();
                env.tuple(vs)
            }
            Spec::Frozen(items) => {
                let vs = items.iter().map(|s| s.build(env)).collect();
                env.frozen(vs)
            }
        }
    }
}

/// Run a Python program through the frontend; its printed lines.
fn run_python(src: String) -> Vec<String> {
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(move || {
            let modules = vec![("main".to_owned(), src)];
            let mut compiled =
                crate::frontend::compile_python_program("main", &modules, &[], &[], false).expect("compile");
            let state = compiled.state_mut();
            state.set_limits(20_000_000_000, None);
            let outcome = state.run_init();
            let out = state.take_output();
            outcome.unwrap_or_else(|e| panic!("python run failed: {e}\n{}", out.join("\n")));
            out.join("\n").lines().map(str::to_owned).collect()
        })
        .expect("spawn")
        .join()
        .expect("python thread")
}

// ---- hashing ----------------------------------------------------------------------------------

#[test]
fn hashes_match_cpython_where_cpython_is_deterministic() {
    let mut env = Env::new();
    // (value, CPython 3.13's hash)
    let cases: Vec<(Spec, i64)> = vec![
        (int("0"), 0),
        (int("-1"), -2),
        (int("-2"), -2),
        (int("2305843009213693951"), 0),
        (int("2305843009213693952"), 1),
        (int("-2305843009213693952"), -2),
        (int("340282366920938463463374607431768211456"), 64),
        (Spec::Float(1.5), 1152921504606846977),
        (Spec::Float(-0.0), 0),
        (Spec::Float(f64::INFINITY), 314159),
        (Spec::Float(f64::NEG_INFINITY), -314159),
        (Spec::None, 4238894112),
        (Spec::Bool(true), 1),
        (Spec::Tuple(vec![int("1"), int("2")]), -3550055125485641917),
        (Spec::Tuple(vec![]), 5740354900026072187),
    ];
    for (spec, want) in cases {
        let v = spec.build(&mut env);
        assert_eq!(env.hash(v), Some(want), "{spec:?}");
    }
}

#[test]
fn str_hash_matches_runtime_values() {
    let mut env = Env::new();
    // Printed by ZIPP's `hash()` (runtime `strHash`).
    for (units, want) in [
        ("abc".encode_utf16().collect::<Vec<_>>(), 4828709191810278i64),
        (vec![], 6384804303800156),
        ("\u{e9}\u{1F600}".encode_utf16().collect(), 5848404964838959),
    ] {
        let v = env.str_units(&units);
        assert_eq!(env.hash(v), Some(want));
    }
    let items = vec![env.int("1"), env.int("2"), env.str_units(&[b'a' as u16])];
    let f = env.frozen(items);
    assert_eq!(env.hash(f), Some(7057156128390738));
}

/// A wide spread of values: hash parity with the runtime's `hash()`.
fn hash_specs() -> Vec<Spec> {
    let mut v: Vec<Spec> = [
        "0", "1", "-1", "-2", "2", "255", "1023", "1024", "-256", "-257", "2147483648", "9007199254740991",
        "9007199254740992", "9007199254740993", "-9007199254740993", "2305843009213693950", "2305843009213693951",
        "2305843009213693952", "4611686018427387904", "9223372036854775808", "18446744073709551616",
        "-18446744073709551616", "170141183460469231731687303715884105727", "170141183460469231731687303715884105728",
        "-170141183460469231731687303715884105728", "11529215046068469755", "11529215046068469756",
        "-6917529027641081853", "1000000000000000000000000000000",
        "515377520732011331036461129765621272702107522001",
        "-1797010299914431210413179829509605039731475627537851106401",
    ]
    .iter()
    .map(|s| int(s))
    .collect();
    for f in [
        0.0, -0.0, 1.0, -1.0, 1.5, -1.5, 0.1, 1e-300, 5e-324, -5e-324, 1e300, f64::MAX, f64::INFINITY,
        f64::NEG_INFINITY, f64::NAN, 9007199254740992.0, 2305843009213693952.0, 4.611686018427388e18, 1e22, 123456.789,
        -2.5e-8, 3.0e21, 1e21, 1e20, 0.5, 2.0f64.powi(-1074 + 60),
    ] {
        v.push(Spec::Float(f));
    }
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for _ in 0..60 {
        let f = f64::from_bits(rng.next());
        if f.is_finite() {
            v.push(Spec::Float(f));
        }
        let digits = (rng.below(40) + 1) as usize;
        let mut d: String = (0..digits).map(|_| char::from(b'0' + rng.below(10) as u8)).collect();
        d = d.trim_start_matches('0').to_string();
        if d.is_empty() {
            d.push('0');
        }
        if rng.below(2) == 0 && d != "0" {
            d.insert(0, '-');
        }
        v.push(Spec::Int(d));
    }
    v.extend([Spec::Bool(true), Spec::Bool(false), Spec::None]);
    for s in ["", "a", "abc", "\u{e9}", "\u{65e5}\u{672c}\u{8a9e}", "\u{1F600}", "a\u{1F600}b", "\0", "\0abc", "it's", "\\"] {
        v.push(st(s));
    }
    v.push(Spec::Str(vec![0xD800]));
    v.push(Spec::Str(vec![0xDC00, b'x' as u16]));
    v.push(Spec::Str(vec![b'a' as u16, 0xDBFF, 0xDBFF, 0xDFFF]));
    v.push(Spec::Str((0..1000u16).map(|i| 32 + i % 90).collect()));
    for _ in 0..30 {
        let n = rng.below(12) as usize;
        let units: Vec<u16> = (0..n)
            .map(|_| match rng.below(4) {
                0 => 0xD800 + rng.below(0x800) as u16,
                1 => 0x80 + rng.below(0xD700) as u16,
                _ => 0x20 + rng.below(0x5F) as u16,
            })
            .collect();
        v.push(Spec::Str(units));
    }
    let t12 = Spec::Tuple(vec![int("1"), int("2")]);
    v.extend([
        Spec::Tuple(vec![]),
        Spec::Tuple(vec![int("1")]),
        t12.clone(),
        Spec::Tuple(vec![Spec::Float(1.0), int("2")]),
        Spec::Tuple(vec![st("a"), Spec::Tuple(vec![int("1"), Spec::Tuple(vec![int("2"), Spec::None])])]),
        Spec::Tuple(vec![Spec::Float(f64::NAN)]),
        Spec::Tuple(vec![int("18446744073709551616"), int("-1")]),
        Spec::Tuple(vec![Spec::Bool(true), Spec::Bool(false)]),
        Spec::Tuple(vec![Spec::Frozen(vec![int("1"), st("b")]), st("\u{1F600}")]),
        Spec::Frozen(vec![]),
        Spec::Frozen(vec![int("1"), int("2"), int("3")]),
        Spec::Frozen(vec![st("a"), int("1"), Spec::Float(2.5)]),
        Spec::Frozen(vec![t12, st("x")]),
        Spec::Frozen(vec![Spec::Frozen(vec![int("1")])]),
        Spec::Frozen(vec![int("18446744073709551616"), Spec::Float(0.5), Spec::Float(f64::NAN), Spec::None]),
        Spec::Frozen(vec![st("\u{e000}"), st("\u{1F600}"), st("\u{ffff}"), st("z")]),
        Spec::Frozen(vec![Spec::Float(1e300), Spec::Float(1e-7), int("-9007199254740993"), Spec::Bool(false)]),
    ]);
    v
}

#[test]
fn native_hash_equals_the_runtime_hash() {
    let specs = hash_specs();
    let mut src = String::from("vals = [\n");
    for s in &specs {
        src.push_str(&format!("    {},\n", s.py()));
    }
    src.push_str("]\nfor v in vals:\n    print(hash(v))\n");
    let lines = run_python(src);
    assert_eq!(lines.len(), specs.len(), "{lines:?}");
    let mut env = Env::new();
    let mut bad = Vec::new();
    for (s, line) in specs.iter().zip(&lines) {
        let v = s.build(&mut env);
        let want: i64 = line.trim().parse().unwrap_or_else(|_| panic!("hash line {line:?}"));
        let got = env.hash(v);
        if got != Some(want) {
            bad.push(format!("{} -> runtime {want}, native {got:?}", s.py()));
        }
    }
    assert!(bad.is_empty(), "{} mismatches:\n{}", bad.len(), bad.join("\n"));
}

// ---- equality -------------------------------------------------------------------------------

#[test]
fn native_eq_follows_the_runtime_eq() {
    let mut env = Env::new();
    let one = env.int("1");
    let big = env.int("18446744073709551616");
    let big2 = env.int("18446744073709551616");
    let a = env.str_units(&[97]);
    let a2 = env.str_units(&[97]);
    let t1 = env.tuple(vec![one, Value::num(2.0)]);
    let two = env.int("2");
    let t2 = env.tuple(vec![Value::num(1.0), two]);
    let tn = env.tuple(vec![Value::num(f64::NAN)]);
    let tn2 = env.tuple(vec![Value::num(f64::NAN)]);
    let f1 = env.frozen(vec![one, a]);
    let f2 = env.frozen(vec![a2, Value::TRUE]);
    let f3 = env.frozen(vec![a2, two]);
    let cases = [
        (one, Value::num(1.0), true),
        (one, Value::TRUE, true),
        (Value::num(1.0), Value::TRUE, true),
        (Value::num(0.0), Value::num(-0.0), true),
        (Value::num(f64::NAN), Value::num(f64::NAN), false),
        (big, big2, true),
        (big, Value::num(18446744073709551616.0), true),
        (big, Value::num(18446744073709551615.0), true), // the same double
        (a, a2, true),
        (a, one, false),
        (Value::NULL, Value::NULL, true),
        (Value::NULL, Value::FALSE, false),
        (t1, t2, true),
        (tn, tn, true), // identity
        (tn, tn2, false),
        (f1, f2, true),
        (f1, f3, false),
        (t1, f1, false),
    ];
    for (x, y, want) in cases {
        assert_eq!(env.eq(x, y), Some(want), "{x:?} == {y:?}");
        assert_eq!(env.eq(y, x), Some(want), "{y:?} == {x:?}");
    }
    let other = Value::heap(env.heap.alloc(HeapObj::Object(Box::new(ObjMap::new()))));
    assert_eq!(env.eq(other, one), None);
    assert_eq!(env.eq(other, other), Some(true));
    assert_eq!(env.hash(other), None);
    let with_other = env.tuple(vec![other]);
    assert_eq!(env.hash(with_other), None);
}

// ---- the table against a reference model ----------------------------------------------------------

/// Keys are ints; hash `k % 5` forces collisions; equality is identity.
fn khash(k: i32) -> i64 {
    (k.rem_euclid(5)) as i64
}

fn find(t: &PyTable, k: i32, steps: &mut u64) -> Lookup {
    let key = Value::int(k);
    t.lookup(khash(k), &mut |s| Some(s == key), steps).expect("decidable")
}

fn check_invariants(t: &PyTable) {
    assert!(t.keys.last().is_none_or(|k| !k.is_hole()));
    assert_eq!(t.len(), t.keys.iter().filter(|k| !k.is_hole()).count());
    assert_eq!(t.hashes.len(), t.keys.len());
    assert_eq!(t.vals.len(), if t.set { 0 } else { t.keys.len() });
    assert!(t.stamps.is_empty() || t.stamps.len() == t.keys.len());
    if t.indices.is_empty() {
        assert!(t.keys.len() <= SMALL);
    } else {
        assert!(t.indices.len().is_power_of_two());
        let used = t.indices.iter().filter(|&&x| x != EMPTY).count();
        assert_eq!(used, t.fill as usize);
        assert!((t.fill as usize) <= t.usable());
        assert!(t.keys.len() <= t.usable());
        for (i, k) in t.keys.iter().enumerate() {
            if !k.is_hole() {
                assert!(t.slot_of(t.hashes[i], i).is_some(), "entry {i} unreachable");
            }
        }
    }
}

#[test]
fn dict_matches_a_reference_model() {
    let mut rng = Rng(0x1234_5678_9ABC_DEF1);
    for round in 0..40 {
        let mut t = PyTable::new_dict();
        let mut model: Vec<(i32, i32)> = Vec::new();
        let span = [4, 12, 40, 400, 5000][round % 5];
        let ops = span * 6;
        let mut steps = 0u64;
        for op in 0..ops {
            let k = rng.below(span as u64) as i32 - (span / 4);
            let before = t.version();
            match rng.below(10) {
                0..=5 => {
                    let v = op;
                    match find(&t, k, &mut steps) {
                        Lookup::Found(i) => {
                            assert!(t.set_value(i, Value::int(v)));
                            assert_eq!(t.version(), before, "a replacement keeps the version");
                            model.iter_mut().find(|e| e.0 == k).expect("model has it").1 = v;
                        }
                        Lookup::Absent(hint) => {
                            t.insert(hint, khash(k), Value::int(k), Value::int(v), None, &mut steps).expect("insert");
                            assert_ne!(t.version(), before);
                            assert!(model.iter().all(|e| e.0 != k));
                            model.push((k, v));
                        }
                    }
                }
                6..=8 => {
                    let found = find(&t, k, &mut steps);
                    let pos = model.iter().position(|e| e.0 == k);
                    match (found, pos) {
                        (Lookup::Found(i), Some(p)) => {
                            let (key, val) = t.remove_at(i).expect("live");
                            assert_eq!((key, val), (Value::int(k), Value::int(model[p].1)));
                            assert_ne!(t.version(), before);
                            model.remove(p);
                        }
                        (Lookup::Absent(_), None) => assert_eq!(t.version(), before),
                        other => panic!("lookup {k}: {other:?}"),
                    }
                }
                _ => {
                    if rng.below(40) == 0 {
                        t.clear();
                        model.clear();
                    } else {
                        let got = t.pop_last();
                        let want = model.pop();
                        assert_eq!(got, want.map(|(k, v)| (Value::int(k), Value::int(v))));
                    }
                }
            }
            if op % 97 == 0 || span < 50 {
                check_invariants(&t);
            }
            assert_eq!(t.len(), model.len());
        }
        check_invariants(&t);
        let got: Vec<(Value, Value)> = t.entries().map(|(_, k, v)| (k, v)).collect();
        let want: Vec<(Value, Value)> = model.iter().map(|&(k, v)| (Value::int(k), Value::int(v))).collect();
        assert_eq!(got, want, "round {round}");
        let heap = Heap::new();
        let copy = t.copy(&heap, &mut steps);
        check_invariants(&copy);
        assert_eq!(copy.entries().map(|(_, k, v)| (k, v)).collect::<Vec<_>>(), want);
        for &(k, v) in &model {
            let Lookup::Found(i) = find(&copy, k, &mut steps) else { panic!("copy lost {k}") };
            assert_eq!(copy.value_at(i), Some(Value::int(v)));
        }
    }
}

/// Keys are ints; the runtime would file `k` in bucket `k / 3`; a bucket's
/// keys share a hash, and so do some unrelated buckets.
fn bucket(k: i32) -> i32 {
    k.div_euclid(3)
}
fn bhash(k: i32) -> i64 {
    bucket(k).rem_euclid(5) as i64
}

#[test]
fn set_order_matches_the_runtime_bucket_model() {
    let heap = Heap::new();
    let mut rng = Rng(0xDEAD_BEEF_0BAD_F00D);
    for round in 0..60 {
        let span = [6, 20, 60, 600][round % 4];
        let mut t = PyTable::new_set();
        // The runtime: a Map of buckets in creation order.
        let mut model: Vec<(i32, Vec<i32>)> = Vec::new();
        let mut steps = 0u64;
        for _ in 0..span * 5 {
            let k = rng.below(span as u64) as i32;
            let key = Value::int(k);
            let h = bhash(k);
            let found = t.lookup(h, &mut |s| Some(s == key), &mut steps).expect("decidable");
            if rng.below(3) > 0 {
                if let Lookup::Absent(hint) = found {
                    let mate = t
                        .same_hash(h, &mut steps)
                        .into_iter()
                        .find(|&i| bucket(t.keys[i].as_int()) == bucket(k));
                    t.insert(hint, h, key, Value::UNDEFINED, mate, &mut steps).expect("insert");
                    match model.iter_mut().find(|b| b.0 == bucket(k)) {
                        Some(b) => b.1.push(k),
                        None => model.push((bucket(k), vec![k])),
                    }
                }
            } else if let Lookup::Found(i) = found {
                t.remove_at(i).expect("live");
                let b = model.iter().position(|b| b.0 == bucket(k)).expect("bucket");
                model[b].1.retain(|&x| x != k);
                if model[b].1.is_empty() {
                    model.remove(b);
                }
            }
            check_invariants(&t);
            let got: Vec<i32> = t.visible_order(&heap).into_iter().map(|i| t.keys[i].as_int()).collect();
            let want: Vec<i32> = model.iter().flat_map(|b| b.1.iter().copied()).collect();
            assert_eq!(got, want, "round {round}");
        }
        // A copy keeps the order and the buckets: a later bucket-mate still
        // lands with its bucket.
        let mut c = t.copy(&heap, &mut steps);
        check_invariants(&c);
        let order = |t: &PyTable| -> Vec<i32> { t.visible_order(&heap).into_iter().map(|i| t.keys[i].as_int()).collect() };
        assert_eq!(order(&c), order(&t));
        for k in 0..span {
            let key = Value::int(k);
            let h = bhash(k);
            for tab in [&mut t, &mut c] {
                if let Lookup::Absent(hint) = tab.lookup(h, &mut |s| Some(s == key), &mut steps).unwrap() {
                    let mate = tab.same_hash(h, &mut steps).into_iter().find(|&i| bucket(tab.keys[i].as_int()) == bucket(k));
                    tab.insert(hint, h, key, Value::UNDEFINED, mate, &mut steps).unwrap();
                }
            }
            assert_eq!(order(&c), order(&t), "round {round} after adding {k}");
        }
    }
}

#[test]
fn stale_hints_are_refused_and_probe_reports_candidates() {
    let mut steps = 0;
    let mut heap = Heap::new();
    let obj = Value::heap(heap.alloc(HeapObj::Array(vec![])));
    let obj2 = Value::heap(heap.alloc(HeapObj::Array(vec![])));
    let mut t = PyTable::new_dict();
    let (b, c) = (Value::int(6), Value::int(11)); // all keys: hash 1
    let Probe::Miss(hint) = t.probe(1, obj, &mut steps) else { panic!() };
    t.insert(hint, 1, obj, Value::NULL, None, &mut steps).unwrap();
    assert_eq!(t.insert(hint, 1, obj, Value::NULL, None, &mut steps), Err(InsertError::Stale));
    let Probe::Hit(0) = t.probe(1, obj, &mut steps) else { panic!("identity is found without guest code") };
    // Identity only counts for heap keys (an unboxed NaN has none).
    let Probe::Candidates(cands, hint) = t.probe(1, b, &mut steps) else { panic!() };
    assert_eq!(cands, vec![0]);
    t.insert(hint, 1, b, Value::NULL, None, &mut steps).unwrap();
    let Probe::Candidates(cands, _) = t.probe(1, b, &mut steps) else { panic!("an int key has no identity") };
    assert_eq!(cands, vec![0, 1]);
    let Probe::Candidates(_, hint) = t.probe(1, obj2, &mut steps) else { panic!() };
    t.insert(hint, 1, obj2, Value::NULL, None, &mut steps).unwrap();
    let Probe::Candidates(cands, _) = t.probe(1, obj2, &mut steps) else { panic!("an earlier same-hash key is tried first") };
    assert_eq!(cands, vec![0, 1, 2]);
    let v = t.version();
    assert_eq!(t.at(2, v), Some((obj2, Value::NULL)));
    let Probe::Candidates(_, hint) = t.probe(1, c, &mut steps) else { panic!() };
    t.remove_at(0);
    assert_eq!(t.at(2, v), None, "a delete moves the version");
    assert_eq!(t.insert(hint, 1, c, Value::NULL, None, &mut steps), Err(InsertError::Stale));
    assert!(steps > 0);
}

#[test]
fn cpython_set_size_matches_the_runtime_simulation() {
    let (mut size, mut fill) = (8usize, 0usize);
    for n in 1..=60_000usize {
        fill += 1;
        if fill * 5 >= (size - 1) * 3 {
            let mut next = 8;
            while next <= fill * 4 {
                next *= 2;
            }
            size = next;
        }
        assert_eq!(cpython_set_size(n), size, "n = {n}");
    }
}

#[test]
fn small_nonnegative_int_sets_iterate_ascending() {
    let mut env = Env::new();
    let build = |env: &mut Env, xs: &[&str]| -> Vec<String> {
        let mut t = PyTable::new_set();
        let mut shown = Vec::new();
        for x in xs {
            let v = if let Some(f) = x.strip_prefix('f') { Value::num(f.parse().unwrap()) } else { env.int(x) };
            native_add(&env.heap, &env.kinds, &mut t, v, &mut 0).unwrap();
            shown.push((v, x.to_string()));
        }
        t.visible_order(&env.heap)
            .into_iter()
            .map(|i| shown.iter().find(|s| s.0 == t.keys[i]).unwrap().1.clone())
            .collect()
    };
    assert_eq!(build(&mut env, &["3", "1", "2"]), ["1", "2", "3"]);
    assert_eq!(build(&mut env, &["7", "3", "100"]), ["7", "3", "100"]);
    assert_eq!(build(&mut env, &["2", "f1.0"]), ["2", "f1.0"]);
    assert_eq!(build(&mut env, &["9", "5", "31"]), ["9", "5", "31"]);
    assert_eq!(build(&mut env, &["9", "5", "3", "4", "31"]), ["3", "4", "5", "9", "31"]);
}

// ---- set and dict behavior against the runtime ---------------------------------------------------

/// The value pool the order tests draw from: ints (small, large, beyond
/// 2^61 so as to share a small int's bucket), floats equal to ints, NaN,
/// bools, None, strs and tuples.
fn order_pool() -> Vec<Spec> {
    let mut v: Vec<Spec> = Vec::new();
    for i in 0..12 {
        v.push(int(&i.to_string()));
    }
    for s in ["40", "100", "-1", "-2", "2305843009213693951", "2305843009213693952", "2305843009213693953", "18446744073709551616"] {
        v.push(int(s));
    }
    v.extend([
        Spec::Float(1.0),
        Spec::Float(2.5),
        Spec::Float(-0.0),
        Spec::Float(f64::NAN),
        Spec::Float(f64::INFINITY),
        Spec::Bool(true),
        Spec::Bool(false),
        Spec::None,
        st("a"),
        st("b"),
        st("zz"),
        Spec::Tuple(vec![int("1"), int("2")]),
        Spec::Tuple(vec![Spec::Float(f64::NAN)]),
        Spec::Tuple(vec![int("2305843009213693952")]),
        Spec::Tuple(vec![int("1")]),
    ]);
    v
}

/// Two sets' steps and a dict's (set?, pool index, value) steps.
type Scenario = (Vec<SetStep>, Vec<SetStep>, Vec<(bool, usize, i32)>);

enum SetStep {
    Add(usize),
    Discard(usize),
    Pop,
}

/// Set `PYTABLE_DUMP=<path>` to keep the generated Python program.
#[test]
fn set_and_dict_behavior_matches_the_runtime() {
    let pool = order_pool();
    let mut rng = Rng(0x0DDB_A11C_AFE5_1234);
    let mut src = String::from("P = [\n");
    for s in &pool {
        src.push_str(&format!("    {},\n", s.py()));
    }
    src.push_str("]\ndef show(xs):\n    print(repr(list(xs)))\n");
    // Scenarios: each builds sets `a` and `b` step by step, printing after
    // every pop and at the end, then the algebra; each builds a dict too.
    let mut scenarios: Vec<Scenario> = Vec::new();
    for n in 0..600 {
        let width = [4usize, 8, 14, pool.len()][n % 4];
        let base = rng.below((pool.len() - width + 1) as u64) as usize;
        let steps_for = |rng: &mut Rng| -> Vec<SetStep> {
            let len = rng.below(18) as usize;
            (0..len)
                .map(|_| match rng.below(10) {
                    0..=5 => SetStep::Add(base + rng.below(width as u64) as usize),
                    6..=8 => SetStep::Discard(base + rng.below(width as u64) as usize),
                    _ => SetStep::Pop,
                })
                .collect()
        };
        let a = steps_for(&mut rng);
        let b = steps_for(&mut rng);
        let d: Vec<(bool, usize, i32)> = (0..rng.below(14))
            .map(|i| (rng.below(4) > 0, base + rng.below(width as u64) as usize, i as i32))
            .collect();
        scenarios.push((a, b, d));
    }
    // One function per scenario: a function's bytecode is bounded.
    for (n, (a, b, d)) in scenarios.iter().enumerate() {
        let mut body = String::new();
        let program = &mut src;
        let src = &mut body;
        for (name, steps) in [("a", a), ("b", b)] {
            src.push_str(&format!("{name} = set()\n"));
            for s in steps {
                match s {
                    SetStep::Add(i) => src.push_str(&format!("{name}.add(P[{i}])\n")),
                    SetStep::Discard(i) => src.push_str(&format!("{name}.discard(P[{i}])\n")),
                    SetStep::Pop => src.push_str(&format!("print(repr({name}.pop()) if {name} else 'E')\n")),
                }
            }
            src.push_str(&format!("show({name})\n"));
        }
        src.push_str("show(a | b)\nshow(a & b)\nshow(a - b)\nshow(a ^ b)\nprint(a <= b, a == b)\n");
        src.push_str("c = set(a)\nc |= b\nshow(c)\n");
        src.push_str("d = {}\n");
        for &(set, i, v) in d {
            if set {
                src.push_str(&format!("d[P[{i}]] = {v}\n"));
            } else {
                src.push_str(&format!("d.pop(P[{i}], None)\n"));
            }
        }
        src.push_str("show(d.items())\n");
        let indented: String = body.lines().map(|l| format!("    {l}\n")).collect();
        program.push_str(&format!("def sc{n}():\n{indented}sc{n}()\n"));
    }
    if let Some(p) = std::env::var_os("PYTABLE_DUMP") { std::fs::write(p, &src).unwrap(); }
    let lines = run_python(src);

    let mut env = Env::new();
    let vals: Vec<Value> = pool.iter().map(|s| s.build(&mut env)).collect();
    let reprs: Vec<String> = pool.iter().map(Spec::repr).collect();
    let repr_of = |v: Value| -> String {
        let i = vals.iter().position(|&x| x == v).expect("pool value");
        reprs[i].clone()
    };
    let show_set = |t: &PyTable| -> String {
        let items: Vec<String> = t.visible_order(&env.heap).into_iter().map(|i| repr_of(t.keys[i])).collect();
        format!("[{}]", items.join(", "))
    };
    let (h, k) = (&env.heap, &env.kinds);
    let mut out: Vec<String> = Vec::new();
    // The scenario each output line belongs to.
    let mut owner: Vec<usize> = Vec::new();
    let mut steps = 0u64;
    for (n, (a, b, d)) in scenarios.iter().enumerate() {
        let mut sets = Vec::new();
        for script in [a, b] {
            let mut t = PyTable::new_set();
            for s in script {
                match *s {
                    SetStep::Add(i) => {
                        native_add(h, k, &mut t, vals[i], &mut steps).expect("native");
                    }
                    SetStep::Discard(i) => {
                        if let (Some(at), _) = native_lookup(h, k, &t, vals[i], &mut steps).expect("native") {
                            t.remove_at(at);
                        }
                    }
                    SetStep::Pop => match t.first_visible(h) {
                        Some(at) => {
                            // The runtime's `set.pop()` deletes its pick by
                            // equality (`setDel`), which never finds a NaN: the
                            // NaN is returned and stays. (CPython removes it; the
                            // native table's `remove_at` would too.)
                            let key = t.key_at(at).unwrap();
                            if !(key.is_number() && key.as_f64().is_nan()) {
                                t.remove_at(at);
                            }
                            out.push(repr_of(key));
                        }
                        None => out.push("E".into()),
                    },
                }
            }
            out.push(show_set(&t));
            sets.push(t);
        }
        let (sa, sb) = (&sets[0], &sets[1]);
        for op in [SetOp::Union, SetOp::Intersection, SetOp::Difference, SetOp::SymmetricDifference] {
            out.push(show_set(&set_op(h, k, op, sa, sb, &mut steps).expect("native")));
        }
        let py_bool = |b: bool| if b { "True" } else { "False" };
        out.push(format!(
            "{} {}",
            py_bool(is_subset(h, k, sa, sb, &mut steps).unwrap()),
            py_bool(set_eq(h, k, sa, sb, &mut steps).unwrap())
        ));
        let mut c = sa.copy(h, &mut steps);
        union_into(h, k, &mut c, sb, &mut steps).expect("native");
        out.push(show_set(&c));
        let mut dict = PyTable::new_dict();
        for &(set, i, v) in d {
            if set {
                native_set_item(h, k, &mut dict, vals[i], Value::int(v), &mut steps).expect("native");
            } else if let (Some(at), _) = native_lookup(h, k, &dict, vals[i], &mut steps).expect("native") {
                dict.remove_at(at);
            }
        }
        let items: Vec<String> = dict.entries().map(|(_, key, v)| format!("({}, {})", repr_of(key), v.as_int())).collect();
        out.push(format!("[{}]", items.join(", ")));
        owner.resize(out.len(), n);
    }
    assert_eq!(out.len(), lines.len());
    let mut bad = 0;
    for (i, (got, want)) in out.iter().zip(&lines).enumerate() {
        if got != want {
            bad += 1;
            if bad < 20 {
                eprintln!("line {i} (sc{}): native {got}\n         runtime {want}", owner[i]);
            }
        }
    }
    assert_eq!(bad, 0, "{bad} of {} lines differ", lines.len());
    assert!(steps > 0);
}

#[test]
fn dict_update_and_equality() {
    let mut env = Env::new();
    let keys: Vec<Value> = (0..50).map(|i| env.int(&i.to_string())).collect();
    let s = env.str_units(&[120]);
    let (heap, kinds) = (&env.heap, &env.kinds);
    let mut steps = 0;
    let mut a = PyTable::new_dict();
    let mut b = PyTable::new_dict();
    for (i, &key) in keys.iter().enumerate() {
        native_set_item(heap, kinds, &mut a, key, Value::int(i as i32), &mut steps).unwrap();
        if i % 2 == 0 {
            native_set_item(heap, kinds, &mut b, Value::num(i as f64), Value::int(-1), &mut steps).unwrap();
        }
    }
    native_set_item(heap, kinds, &mut b, s, Value::NULL, &mut steps).unwrap();
    let mut c = a.copy(heap, &mut steps);
    update_from(heap, kinds, &mut c, &b, &mut steps).unwrap();
    assert_eq!(c.len(), 51);
    // Keys keep their first spelling (an int, not the float) and place.
    let (_, k0, v0) = c.entries().next().unwrap();
    assert_eq!((k0, v0), (keys[0], Value::int(-1)));
    assert_eq!(c.entries().last().unwrap().1, s);
    assert_eq!(dict_eq(heap, kinds, &a, &a.copy(heap, &mut steps), &mut steps), Some(true));
    assert_eq!(dict_eq(heap, kinds, &a, &c, &mut steps), Some(false));
    // A guest key stops the update where the runtime takes over.
    let other = Value::heap(env.heap.alloc(HeapObj::Object(Box::new(ObjMap::new()))));
    let (heap, kinds) = (&env.heap, &env.kinds);
    let mut g = PyTable::new_dict();
    native_set_item(heap, kinds, &mut g, keys[1], Value::int(7), &mut steps).unwrap();
    let hint = match g.probe(99, other, &mut steps) {
        Probe::Miss(hint) => hint,
        p => panic!("{p:?}"),
    };
    g.insert(hint, 99, other, Value::int(8), None, &mut steps).unwrap();
    native_set_item(heap, kinds, &mut g, keys[2], Value::int(9), &mut steps).unwrap();
    // Without a same-hash key in the destination no equality is needed.
    let mut d = PyTable::new_dict();
    assert_eq!(update_from(heap, kinds, &mut d, &g, &mut steps), Ok(()));
    assert_eq!(d.len(), 3);
    // With one, the guest key's equality is: the update stops there.
    let mut d = PyTable::new_dict();
    let k99 = Value::int(99);
    native_set_item(heap, kinds, &mut d, k99, Value::NULL, &mut steps).unwrap();
    assert_eq!(update_from(heap, kinds, &mut d, &g, &mut steps), Err(Stop::Guest(1)));
    assert_eq!(d.len(), 2);
    // A same-hash guest key makes a native lookup undecidable.
    let mut g2 = PyTable::new_dict();
    let h1 = native_hash(heap, kinds, keys[1], &mut steps).unwrap();
    let Probe::Miss(hint) = g2.probe(h1, other, &mut steps) else { panic!() };
    g2.insert(hint, h1, other, Value::NULL, None, &mut steps).unwrap();
    assert_eq!(native_lookup(heap, kinds, &g2, keys[1], &mut steps), None);
}

// ---- measurements ---------------------------------------------------------------------------------

/// `cargo test --release --features python --lib py_table::tests::bench -- --ignored --nocapture`
#[test]
#[ignore]
fn bench() {
    use std::collections::HashMap;
    use std::time::Instant;
    const N: usize = 1_000_000;
    let mut env = Env::new();
    let keys: Vec<Value> = (0..N as i32).map(|i| Value::int(i.wrapping_mul(7919))).collect();
    let strs: Vec<Value> = (0..N).map(|i| {
        let s: Vec<u16> = format!("key{i}").encode_utf16().collect();
        env.str_units(&s)
    }).collect();
    let (heap, kinds) = (&env.heap, &env.kinds);
    let mut steps = 0u64;
    let time = |label: &str, f: &mut dyn FnMut() -> u64| {
        let t0 = Instant::now();
        let r = f();
        println!("{label:44} {:8.1} ms  (check {r})", t0.elapsed().as_secs_f64() * 1e3);
    };
    let mut t = PyTable::new_dict();
    time("PyTable int insert (native hash+eq)", &mut || {
        for (i, &k) in keys.iter().enumerate() {
            native_set_item(heap, kinds, &mut t, k, Value::int(i as i32), &mut steps).unwrap();
        }
        t.len() as u64
    });
    time("PyTable int lookup (native hash+eq)", &mut || {
        let mut s = 0u64;
        for &k in &keys {
            s += native_lookup(heap, kinds, &t, k, &mut steps).unwrap().0.unwrap() as u64;
        }
        s
    });
    time("PyTable int lookup (raw: hash known, bits eq)", &mut || {
        let mut s = 0u64;
        for &k in &keys {
            let h = k.as_int() as i64;
            if let Some(Lookup::Found(i)) = t.lookup(h, &mut |x| Some(x == k), &mut steps) {
                s += i as u64;
            }
        }
        s
    });
    time("PyTable iterate", &mut || t.entries().map(|(_, _, v)| v.as_int() as u64).sum());
    let mut m: HashMap<u64, Value> = HashMap::new();
    let mut order: Vec<u64> = Vec::new();
    time("std HashMap int insert (+ order Vec)", &mut || {
        for (i, &k) in keys.iter().enumerate() {
            if m.insert(k.bits(), Value::int(i as i32)).is_none() {
                order.push(k.bits());
            }
        }
        m.len() as u64
    });
    time("std HashMap int lookup", &mut || keys.iter().map(|k| m[&k.bits()].as_int() as u64).sum());
    time("std HashMap iterate (insertion order)", &mut || order.iter().map(|k| m[k].as_int() as u64).sum());
    let mut ts = PyTable::new_dict();
    time("PyTable str insert (native hash+eq)", &mut || {
        for (i, &k) in strs.iter().enumerate() {
            native_set_item(heap, kinds, &mut ts, k, Value::int(i as i32), &mut steps).unwrap();
        }
        ts.len() as u64
    });
    time("PyTable str lookup (native hash+eq)", &mut || {
        let mut s = 0u64;
        for &k in &strs {
            s += native_lookup(heap, kinds, &ts, k, &mut steps).unwrap().0.unwrap() as u64;
        }
        s
    });
    let mut ms: HashMap<Vec<u8>, Value> = HashMap::new();
    time("std HashMap str insert (bytes keys)", &mut || {
        for (i, &k) in strs.iter().enumerate() {
            ms.insert(heap.str_wtf8_cow(k.heap_index()).unwrap().into_owned(), Value::int(i as i32));
        }
        ms.len() as u64
    });
    time("std HashMap str lookup (bytes keys)", &mut || {
        strs.iter().map(|k| ms[heap.str_wtf8_cow(k.heap_index()).unwrap().as_ref()].as_int() as u64).sum()
    });
    let mut set = PyTable::new_set();
    time("PyTable set add 1e6 then delete half", &mut || {
        for &k in &keys {
            native_add(heap, kinds, &mut set, k, &mut steps).unwrap();
        }
        for &k in keys.iter().step_by(2) {
            let (i, _) = native_lookup(heap, kinds, &set, k, &mut steps).unwrap();
            set.remove_at(i.unwrap());
        }
        set.len() as u64
    });
    println!("steps charged: {steps}; PyTable payload bytes (1e6 int dict): {}", t.payload_bytes());
}
