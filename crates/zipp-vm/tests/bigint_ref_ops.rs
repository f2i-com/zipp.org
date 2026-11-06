//! BigInt operators where an operand is beyond i128, computed from the operands' heap
//! slots (`Vm::bigint_big_op`) and ordered there (`Vm::bigint_ref_cmp`): every operator
//! against Big/Big, Big/small and small/Big pairs of both signs, the i128 boundaries,
//! zero divisors, shifts by in-range and negative counts, comparisons, results that
//! demote into the i128 range, the size cap and the mixing error. Run interpreted, at
//! the default thresholds and with every function compiled at once; the expected line
//! is Node 24's.

const SOURCE: &str = r##"// BigInt operators where an operand is beyond i128, computed from the
// operands' heap slots: every operator against Big/Big, Big/small and
// small/Big pairs of both signs, the i128 boundaries, zero divisors, shifts
// by in-range and negative counts, comparisons, and results that demote back
// into the i128 range.
"use strict";
const B127 = 1n << 127n;
const vals = [0n, 1n, -1n, 3n, -7n, 4000000000n, -4000000000n, (1n << 63n) - 1n, -(1n << 63n), B127 - 1n, -B127, B127, -B127 - 1n,
  B127 + 12345n, -(B127 + 12345n), (1n << 200n) + 987654321n, -((1n << 200n) + 987654321n), (1n << 256n) - 1n, -(1n << 256n),
  0xffffffffn, (1n << 130n), -(1n << 130n) + 1n];
const out = [];
let h = 0;
function mix(s) { for (let i = 0; i < s.length; i++) h = Math.imul(h ^ s.charCodeAt(i), 16777619); }
function run() {
  h = 2166136261 | 0;
  for (const a of vals) for (const b of vals) {
    mix(String(a + b)); mix(String(a - b)); mix(String(a * b));
    mix(String(a & b)); mix(String(a | b)); mix(String(a ^ b));
    if (b !== 0n) { mix(String(a / b)); mix(String(a % b)); }
    else { try { a / b; mix("no"); } catch (e) { mix(e.constructor.name); } try { a % b; mix("no"); } catch (e) { mix(e.constructor.name); } }
    mix(String(a < b) + (a > b) + (a <= b) + (a >= b) + (a === b) + (a == b));
  }
  for (const a of vals) for (const c of [0n, 1n, 31n, 32n, 64n, 127n, 128n, 200n, -1n, -32n, -300n]) {
    mix(String(a << c)); mix(String(a >> c));
  }
  return h >>> 0;
}
let first = 0;
for (let r = 0; r < 40; r++) { const x = run(); if (r === 0) first = x; else if (x !== first) { out.push("DRIFT " + r); break; } }
out.push(first);
// Spot values, demotions, the size cap and the mixing error.
out.push(String((1n << 200n) - (1n << 200n) + 5n), String((B127 + 1n) - 2n), String(-B127 * -1n), String((B127 * 2n) / 2n),
  String(((1n << 300n) + 7n) % (1n << 150n)), String(-((1n << 300n) + 7n) % (1n << 150n)), String((-(1n << 200n)) >> 199n),
  String((-(1n << 200n) - 1n) >> 400n), String((1n << 200n) >> 1000n), String(-B127 / -1n), String((1n << 129n) & -1n));
try { (1n << 200n) << (1n << 31n); out.push("no"); } catch (e) { out.push(e.constructor.name); }
try { (1n << 200n) + 1; out.push("no"); } catch (e) { out.push(e.constructor.name); }
let big = (1n << 200n) + 3n, acc = 0n;
for (let i = 0; i < 2000; i++) { acc = (acc + big * BigInt(i)) % ((1n << 190n) + 1n); acc ^= big >> BigInt(i & 63); }
out.push(String(acc));
console.log(out.join("|"));
"##;

const EXPECTED: &str = "599500102|5|170141183460469231731687303715884105727|170141183460469231731687303715884105728|170141183460469231731687303715884105728|7|-7|-2|-1|0|170141183460469231731687303715884105728|680564733841876926926749214863536422912|RangeError|TypeError|1520235576538961921716052114453600003397588214269416579567";

#[test]
fn bigint_ref_ops_worker() {
    if std::env::var_os("ZIPP_BIGINT_REF_OPS_WORKER").is_none() {
        return;
    }
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    assert_eq!(out.output.join("\n"), EXPECTED);
}

#[test]
fn bigint_ref_ops_match_node() {
    if std::env::var_os("ZIPP_BIGINT_REF_OPS_WORKER").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test executable");
    for (mode, var) in [
        ("default", None),
        ("interpreter", Some("ZIPP_NOJIT")),
        ("compile-at-once", Some("ZIPP_JIT_THRESHOLD")),
    ] {
        let mut command = std::process::Command::new(&exe);
        command
            .args(["bigint_ref_ops_worker", "--exact", "--nocapture"])
            .env("ZIPP_BIGINT_REF_OPS_WORKER", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_JIT_THRESHOLD");
        if let Some(v) = var {
            command.env(v, "1");
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
