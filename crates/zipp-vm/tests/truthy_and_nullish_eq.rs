//! `==` / `!=` with a nullish side answer without coercion
//! (`loose_eq_nullish`), and truthiness keeps its heap rules. Forty-one
//! values (empty, sliced and concatenated strings, `0n` and a computed zero
//! BigInt, `-0`, `NaN`, boxed primitives, settled and appended objects,
//! functions, classes, a Proxy, a null-prototype object) for truthiness and
//! `== null`, `!= null`, `undefined ==`, loose equality that must still
//! coerce, and a hot `if (o)` over objects each append changes. The expected
//! lines are Node's (v24), in the JIT, the interpreter and the interpreter
//! under GC stress.

const SOURCE: &str = r###"
"use strict";
class K { constructor() { this.a = 1; } }
function F() { this.b = 2; }
const settled = { x: 1 }; settled.x; settled.x;
const values = [
  "", "a", "0", " ", 0, -0, 1, NaN, Infinity, 0n, 1n, -1n, true, false, null, undefined,
  {}, [], [0], settled, new K(), new F(), function () {}, () => 0, class {}, Symbol("s"),
  new Boolean(false), new String(""), new Number(0), Object(0n), new Proxy({}, {}), new Map(),
  /x/, new Date(0), Object.create(null), "".concat(""), "ab".slice(1, 1), String.fromCharCode(),
  ("x" + "y").slice(0, 0), 2n ** 64n - 2n ** 64n, -0n,
];
const out = [];
for (let r = 0; r < 3; r++) {
  let t = "", eqn = "", nen = "", equ = "";
  for (const v of values) {
    t += v ? "1" : "0";
    eqn += v == null ? "1" : "0";
    nen += null != v ? "1" : "0";
    equ += undefined == v ? "1" : "0";
  }
  out.push(t + " " + eqn + " " + nen + " " + equ);
}
// Loose equality that must still coerce once neither side is nullish.
const coerce = [[0, ""], ["0", false], [1, true], [[1], 1], [{ valueOf() { return 3; } }, 3], [1n, 1], ["1", 1n], [null, 0], [undefined, 0], [null, false], [undefined, ""], [NaN, NaN]];
out.push(coerce.map(([a, b]) => (a == b ? 1 : 0) + "" + (b == a ? 1 : 0)).join(","));
// A hot `if (x)` over objects being appended to (mirror cleared each append).
let n = 0;
for (let i = 0; i < 2000; i++) { const o = {}; if (o) n++; o["k" + (i & 7)] = i; if (o) n++; const s = i & 1 ? "" : "s"; if (s) n++; }
out.push(n);
console.log(out.join("\n"));
"###;

const EXPECTED: &str = "01110010101110001111111111111111111000000 00000000000000110000000000000000000000000 11111111111111001111111111111111111111111 00000000000000110000000000000000000000000\n01110010101110001111111111111111111000000 00000000000000110000000000000000000000000 11111111111111001111111111111111111111111 00000000000000110000000000000000000000000\n01110010101110001111111111111111111000000 00000000000000110000000000000000000000000 11111111111111001111111111111111111111111 00000000000000110000000000000000000000000\n11,11,11,11,11,11,11,00,00,00,00,00\n5000";

#[test]
fn truthy_and_nullish_eq_worker() {
    if std::env::var_os("ZIPP_TRUTHY_NULLISH_EQ_WORKER").is_none() {
        return;
    }
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    assert_eq!(out.output.join("\n"), EXPECTED);
}

#[test]
fn truthy_and_nullish_eq_matches_node() {
    if std::env::var_os("ZIPP_TRUTHY_NULLISH_EQ_WORKER").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test executable");
    for (mode, nojit, gc) in [("default", false, false), ("interpreter", true, false), ("interpreter-gc-stress", true, true)] {
        let mut command = std::process::Command::new(&exe);
        command
            .args(["truthy_and_nullish_eq_worker", "--exact", "--nocapture"])
            .env("ZIPP_TRUTHY_NULLISH_EQ_WORKER", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_GC_STRESS");
        if nojit {
            command.env("ZIPP_NOJIT", "1");
        }
        if gc {
            command.env("ZIPP_GC_STRESS", "1");
        }
        let out = command.output().expect("spawn the worker");
        assert!(
            out.status.success(),
            "{mode} diverged from node:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
