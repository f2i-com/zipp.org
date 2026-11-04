//! Numeric constant expressions the compiler folds (`-1`, `4096 * 4`,
//! `1 << 31`, `~2.7`, `0 * -1`) must load exactly the value the runtime
//! computes: `-0` stays `-0`, NaN and the infinities survive, the bitwise and
//! shift forms wrap through ToInt32/ToUint32, and what is not folded (`%`,
//! `**`, string operands, `1 + 2 + "x"`) keeps its runtime meaning. The
//! loops run hot enough for the native tiers. The expected line is Node's.

const SOURCE: &str = r##"
var out = [];
function show(v) { out.push(Object.is(v, -0) ? "-0" : String(v)); }
show(-1); show(-0); show(+0); show(-(-0)); show(0 * -1); show(-0 + 0); show(-0 - 0); show(-0 * 1);
show(1 / -0); show(1 / (0 * -1)); show(0 / 0); show(-(0 / 0)); show(1 / 0); show(-1 / 0);
show(4096 * 4); show(1 << 20); show(1 << 31); show(1 << 32); show(1 << -1); show(-1 >>> 0); show(-1 >>> 28);
show(-8 >> 1); show(-8 >>> 1); show(0x7fffffff + 1); show((0x7fffffff + 1) | 0); show(2147483648 >> 0);
show(~0); show(~-1); show(~2.7); show(~-2.7); show(~(1 << 31)); show(~4294967296); show(~1e21);
show(5 & 3); show(5 | 3); show(5 ^ 3); show(1.5 | 0); show(-1.5 | 0); show(1e21 | 0); show(4294967297 | 0);
show(0.1 + 0.2); show(0.1 * 3); show(1 / 3); show(2 ** 10); show(7 % 3); show(-7 % 3); show(5.5 % 2);
show(1e308 * 10); show(-1e308 * 10); show(1e-320 / 1e10); show(9007199254740992 + 1); show(-(2147483648));
show(-2147483648 | 0); show(- -2147483648); show(-(-2147483648)); show(+-1); show(-+-1);
show(+"3"); show(-"3"); show(~"7"); show(1 + 2 + "x"); show("x" + 1 + 2); show("x" + (1 + 2)); show(1 - -1);
show(1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1 + 1);
var a = [10, 20, 30];
show(a.indexOf(99) === -1); show(a[-1]); a[-1] = 5; show(a[-1]); show(Object.keys(a).join());
switch (-1) { case -1: show("case -1"); break; default: show("default"); }
switch (3) { case 1 + 1: show("two"); break; case 1 + 2: show("three"); break; }
var x = 7; x *= 2 * 3; show(x); x -= -1; show(x); x = x + -1; show(x);
for (var i = 0, n = 0; i < 4 * 4; i++) n += i; show(n);
show(Object.is(-0, -0)); show(Object.is(0 * -1, -0)); show([1, 2, 3].includes(-0));
var m = new Map([[-0, "z"]]); show(m.get(0));
function hot(k) {
  var s = 0, z = 0;
  for (var j = 0; j < 4096 * 4; j += 4) {
    if (((j >> 2) & 1) === -1) s = -1;
    s = (s + (j & (1 << 10) - 1) * -3 + (~0 & j)) | 0;
    z += 1 / (0 * -1) === -1 / 0 ? 1 : 0;
  }
  return [s, z, k * -0.5];
}
var acc = 0, last;
for (var r = 0; r < 60; r++) { last = hot(r); acc = (acc + last[0] + last[1]) | 0; }
show(acc); show(last.join());
console.log(out.join(" "));
"##;

const NODE: &str = "-1 -0 0 0 -0 0 -0 -0 -Infinity -Infinity NaN NaN Infinity -Infinity 16384 1048576 \
-2147483648 1 -2147483648 4294967295 15 -4 2147483644 2147483648 -2147483648 -2147483648 -1 0 -3 1 \
2147483647 -1 559939583 1 7 6 1 -1 -559939584 1 0.30000000000000004 0.30000000000000004 \
0.3333333333333333 1024 1 -1 1.5 Infinity -Infinity 0 9007199254740992 -2147483648 -2147483648 \
2147483648 2147483648 -1 1 3 -3 -8 3x x12 x3 2 12 true undefined 5 0,1,2,-1 case -1 three 42 43 42 120 \
true true false z 1637007360 27279360,4096,-29.5";

#[test]
fn const_fold_parity() {
    let got = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(got.error.is_none(), "unexpected runtime error: {:?}", got.error);
    let got: Vec<String> = got.output.iter().flat_map(|s| s.lines().map(str::to_owned)).collect();
    assert_eq!(got, vec![NODE.to_owned()]);
}

/// The corpus again with folding off, eager JIT, GC stress and the
/// interpreter, each in a fresh process.
#[test]
fn const_fold_all_modes() {
    let exe = std::env::current_exe().expect("test exe path");
    for mode in [
        ("ZIPP_NO_CONST_FOLD", "1"),
        ("ZIPP_JIT_THRESHOLD", "1"),
        ("ZIPP_GC_STRESS", "1"),
        ("ZIPP_NOJIT", "1"),
    ] {
        let out = std::process::Command::new(&exe)
            .args(["const_fold_parity", "--exact"])
            .env(mode.0, mode.1)
            .output()
            .expect("spawn test binary");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{mode:?} failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!stdout.contains("running 0 tests"), "mode filter matched nothing: {stdout}");
    }
}
