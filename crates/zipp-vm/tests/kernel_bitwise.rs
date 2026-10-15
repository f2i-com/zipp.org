//! Array callback kernels (`map` / `filter` / `reduce` run a callback body
//! inline over the array) admit bitwise ops and shifts: `x => (x & 3) === 0`,
//! `x => x >>> 1`, `(acc, x) => acc ^ x`, `x => ~x`. Each operand is ToInt32'd
//! as the interpreter does; a double truncates, and NaN, the infinities,
//! |x| >= 2^63 and every non-number bail that element to the ordinary
//! per-element path, which finishes the array. The expected lines are Node
//! 24's.

const SOURCE: &str = r#"
"use strict";
const out = [];
const ints = [];
for (let i = 0; i < 2000; i++) ints.push(i * 7 - 3000);
const doubles = [];
for (let i = 0; i < 2000; i++) doubles.push(i * 1.75 - 1500.5);
const big = [2147483647, 2147483648, 4294967295, 4294967296, -2147483649, 5e9, -5e9, 1e18, -1e18];
// Values ToInt32 cannot take natively bail to the ordinary path mid-array.
const odd = [5, 6.5, NaN, 7, Infinity, -Infinity, 1e21, -0, 9, true, null, undefined, "12", 13];
const sum = (a) => a.reduce((s, v) => s + (Object.is(v, -0) ? 0.25 : v), 0);
for (const [name, a] of [["ints", ints], ["doubles", doubles], ["big", big], ["odd", odd]]) {
  out.push(name);
  out.push(a.filter((x) => (x & 3) === 0).length);
  out.push(sum(a.map((x) => x | 0)));
  out.push(sum(a.map((x) => x >>> 1)));
  out.push(sum(a.map((x) => x >> 3)));
  out.push(sum(a.map((x) => x << 5)));
  out.push(sum(a.map((x) => x >>> 0)));
  out.push(sum(a.map((x) => ~x)));
  out.push(sum(a.map((x, i) => (x ^ i) & 255)));
  out.push(a.reduce((acc, x) => acc ^ x, 0));
  out.push(a.reduce((acc, x) => (acc + (x & 15)) | 0, 7));
  out.push(a.filter((x) => (x >>> 31) === 1).length);
  // Shift counts outside 0..31 mask to five bits.
  out.push(sum(a.map((x) => x << 33)));
  out.push(sum(a.map((x) => x >>> 32)));
}
// A shift by a register count, and a filter whose predicate is not a Bool.
out.push(sum([1, 2, 3, 40, 41].map((x) => 1 << x)));
out.push([0, 1, 2, 3, 4, 5].filter((x) => x & 1).join(","));
console.log(out.join(" "));
"#;

fn node_output(source: &str) -> Vec<String> {
    let out = std::process::Command::new("node")
        .arg("-e")
        .arg(source)
        .output()
        .expect("node on PATH");
    assert!(out.status.success(), "node failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout)
        .expect("node output is UTF-8")
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn kernel_bitwise_matches_node() {
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    assert_eq!(out.output, node_output(SOURCE));
}

/// The same program in fresh children: the interpreter alone, the JIT
/// compiling everything at once, and under GC stress.
#[test]
fn kernel_bitwise_all_modes() {
    let exe = std::env::current_exe().expect("test exe path");
    for (name, key, value) in [
        ("nojit", "ZIPP_NOJIT", "1"),
        ("eager", "ZIPP_JIT_THRESHOLD", "1"),
        ("gc", "ZIPP_GC_STRESS", "1"),
    ] {
        let out = std::process::Command::new(&exe)
            .args(["kernel_bitwise_matches_node", "--exact"])
            .env(key, value)
            .output()
            .expect("spawn mode child");
        assert!(
            out.status.success(),
            "{name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
