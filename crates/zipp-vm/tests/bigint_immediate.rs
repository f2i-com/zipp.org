//! Small BigInts are immediates (value.rs): every BigInt in the signed 47-bit
//! range is a NaN-boxed value, never a heap object, and a value outside it is
//! a heap BigInt. These pin that the representation is invisible: operators,
//! coercions, keys, typed arrays, formatting and the built-ins answer as Node
//! does, and results crossing the immediate range's edges (in both
//! directions) equal the i128 arithmetic the test computes itself.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    out.output
}

/// Node 24's output for `SEMANTICS`.
const SEMANTICS_EXPECTED: &str = r#"bigint bigint bigint 5n -3n 1208925819614629174706176n 1125899906842624n
2n -15n 8n -1n 2n -5n 125n 40n 2n 5n -3n -8n -6n
true true true true true true true true true
5 -3 101 -3 {"x":1} 5
json TypeError
mix TypeError
unary+ TypeError
5 7 42n 123n -1n 255n
one big false 2 true 0
true object [object BigInt] true
-7n 4611686018427387904n bigint
9n 9n
t f true true or and
4999950000n
15511210043330985984000000n
-2,5,10
2 TypeError
k 5 true
12,345,678 123
sw big
70368744177663n 70368744177664n -70368744177664n -70368744177665n true true
-9223372036854775808n 18446744073709551615n 10000000000n"#;

const SEMANTICS: &str = r#"
const out = [];
const p = (...a) => out.push(a.map(x => typeof x === "bigint" ? x + "n" : String(x)).join(" "));
let a = 5n, b = -3n, big = 2n ** 80n, mid = 2n ** 50n;
p(typeof a, typeof big, typeof mid, a, b, big, mid);
p(a + b, a * b, a - b, a / b, a % b, -a, a ** 3n, a << 3n, a >> 1n, a & b, a | b, a ^ b, ~a);
p(a === 5n, a == 5, a == "5", 5n === 5n, Object.is(0n, -0n), a !== b, a < 6, a > 4.5, 1n < 1.5);
p(String(a), `${b}`, a.toString(2), b.toString(16), JSON.stringify({x: 1}), a.toLocaleString());
try { JSON.stringify({a}); } catch (e) { p("json", e.constructor.name); }
try { a + 1; } catch (e) { p("mix", e.constructor.name); }
try { +a; } catch (e) { p("unary+", e.constructor.name); }
p(Number(a), parseInt("7"), BigInt(42), BigInt("123"), BigInt.asIntN(8, 255n), BigInt.asUintN(8, -1n));
const m = new Map([[1n, "one"], [2n ** 60n, "big"]]);
p(m.get(1n), m.get(2n ** 60n), m.has(1), new Set([1n, 1n, 2n]).size, [1n, 2n].includes(2n), [3n].indexOf(3n));
p(Object(a) instanceof BigInt, typeof Object(a), Object.prototype.toString.call(a), a.constructor === BigInt);
const ta = new BigInt64Array(2); ta[0] = -7n; ta[1] = 2n ** 62n; p(ta[0], ta[1], typeof ta[0]);
const dv = new DataView(new ArrayBuffer(8)); dv.setBigUint64(0, 9n); p(dv.getBigUint64(0), dv.getBigInt64(0));
p(a ? "t" : "f", 0n ? "t" : "f", !0n, !!7n, 0n || "or", 3n && "and");
let s = 0n; for (let i = 0n; i < 100000n; i++) s += i; p(s);
let f = 1n; for (let i = 1n; i <= 25n; i++) f *= i; p(f);
p([5n, -2n, 10n].sort((x, y) => (x < y ? -1 : x > y ? 1 : 0)).join(","));
p(Math.max(1, 2), (() => { try { Math.max(1n); } catch (e) { return e.constructor.name; } })());
const o = {}; o[5n] = "k"; p(o["5"], Object.keys(o).join(), 5n in {5: 1});
p(new Intl.NumberFormat("en-US").format(12345678n), (123n).toLocaleString("de-DE"));
switch (3n) { case 3: p("sw num"); break; case 3n: p("sw big"); break; }
let x = 2n ** 46n - 1n; p(x, x + 1n, -x - 1n, -x - 2n, (x + 1n) - 1n === x, (-x - 2n) + 1n === -x - 1n);
p(BigInt.asIntN(64, 2n ** 63n), BigInt.asUintN(64, -1n), 10n ** 20n / 10n ** 10n);
out.join("\n");
"#;

#[test]
fn small_bigints_answer_as_node_does() {
    let src = format!("{SEMANTICS}\nconsole.log(out.join(\"\\n\"));");
    let got = run_ok(&src).join("\n");
    assert_eq!(got.replace('\r', ""), SEMANTICS_EXPECTED);
}

/// The values around the immediate range's two edges, and some far away.
fn edge_values() -> Vec<i128> {
    let lim = 1i128 << 46;
    let mut v = vec![0, 1, -1, 2, -2, 7, -7, 1 << 32, -(1 << 32), 1 << 62, -(1 << 62), 1 << 100];
    for d in -3..=3 {
        v.push(lim + d);
        v.push(-lim + d);
    }
    v
}

#[test]
fn arithmetic_across_the_immediate_edges_matches_i128() {
    let vals = edge_values();
    let list = vals.iter().map(|v| format!("{v}n")).collect::<Vec<_>>().join(", ");
    let src = format!(
        r#"
const vs = [{list}];
for (const a of vs) for (const b of vs) {{
  const r = [a + b, a - b, a * b, a & b, a | b, a ^ b, a < b, a === b, a == b];
  if (b !== 0n) r.push(a / b, a % b);
  console.log(r.join(" "));
  // Canonical: a value equals its own re-parse and keys a Map once.
  const s = a + b;
  if (s !== BigInt(String(s)) || new Map([[s, 1], [BigInt(String(s)), 2]]).size !== 1) {{
    console.log("NOT CANONICAL " + s);
  }}
}}
"#
    );
    let got = run_ok(&src);
    let mut want = Vec::new();
    for &a in &vals {
        for &b in &vals {
            let mut r = vec![
                (a + b).to_string(),
                (a - b).to_string(),
                a.checked_mul(b).map_or_else(|| "?".into(), |p| p.to_string()),
                (a & b).to_string(),
                (a | b).to_string(),
                (a ^ b).to_string(),
                (a < b).to_string(),
                (a == b).to_string(),
                (a == b).to_string(),
            ];
            if b != 0 {
                r.push((a / b).to_string());
                r.push((a % b).to_string());
            }
            want.push(r);
        }
    }
    assert_eq!(got.len(), want.len(), "one line per pair: {got:?}");
    for (g, w) in got.iter().zip(&want) {
        let g: Vec<&str> = g.split(' ').collect();
        assert_eq!(g.len(), w.len(), "{g:?} vs {w:?}");
        for (x, y) in g.iter().zip(w) {
            // i128 overflows only for products of the 2^100 value; skip those.
            if y != "?" {
                assert_eq!(x, y, "{g:?} vs {w:?}");
            }
        }
    }
}

#[test]
fn small_bigints_in_every_value_slot() {
    // Array elements, object properties, closures, Map/Set keys and values,
    // JSON revival through a replacer, WeakMap rejection, Symbol-keyed
    // properties and exceptions all carry an immediate as they carry any
    // primitive.
    let out = run_ok(
        r#"
const xs = [1n, -2n, 3n];
const o = { a: 4n, [Symbol.for("k")]: 5n };
const f = ((c) => () => c + 1n)(6n);
const m = new Map([[7n, 8n]]);
const set = new Set([9n, 9n, 10n]);
let caught;
try { throw 11n; } catch (e) { caught = e; }
let weak = "ok";
try { new WeakMap().set(12n, 1); weak = "accepted"; } catch (e) { weak = e.constructor.name; }
const json = JSON.stringify({ v: 13n }, (k, v) => typeof v === "bigint" ? v.toString() : v);
BigInt.prototype.toJSON = function () { return "J" + this.toString(); };
const json2 = JSON.stringify([14n]);
delete BigInt.prototype.toJSON;
console.log([xs.reduce((s, v) => s + v, 0n), o.a + o[Symbol.for("k")], f(), m.get(7n), set.size,
  caught, weak, json, json2, typeof xs[0], xs.indexOf(-2n), Object.is(xs[2], 3n)].join(" "));
"#,
    );
    assert_eq!(out, vec![r#"2 9 7 8 2 11 TypeError {"v":"13"} ["J14"] bigint 1 true"#]);
}
