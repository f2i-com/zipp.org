//! jit_opt_int's program with every third Tier O guard check failing.
#![cfg(all(feature = "jit", target_arch = "x86_64"))]

const EXPECTED: &str = r####"499500 2149482000 -2149482000 1998120260 -2147480196 1926 4950 3.5 a1 2147483648 -2147483649 2449965000"####;

const PROGRAM: &str = r####"// Tier O's small-integer arithmetic: i32 adds with an overflow guard, wrapping
// adds under | 0, integer compares and switches.
const out = [];
function count(n) { let s = 0; for (let i = 0; i < n; i++) s = s + i; return s; }
function overflow(n) { let s = 2147483000; for (let i = 0; i < n; i++) s = s + i; return s; }
function sub(n) { let s = -2147483000; for (let i = 0; i < n; i++) s = s - i; return s; }
function hash(n) { let h = 0; for (let i = 0; i < n; i++) h = (h + (h << 5) + i) | 0; return h; }
function wrapK(n) { let h = 2147483600; for (let i = 0; i < n; i++) h = (h + 7) | 0; return h; }
function kinds(n) {
  let t = 0;
  for (let i = 0; i < n; i++) {
    switch (i & 7) { case 0: t += 1; break; case 3: t += 30; break; case 5: t -= 2; break; default: t += i & 1; }
    if ((i & 15) < 3) t++;
    if ((i & 15) >= 12) t--;
  }
  return t;
}
let r = [];
for (let k = 0; k < 40; k++) r = [count(1000), overflow(2000), sub(2000), hash(3000), wrapK(500), kinds(500)];
out.push(r.join(" "));
// Mixed: doubles and strings through the same adds after they compiled.
function add(a, b) { return a + b; }
function addLoop(n) { let s = 0; for (let i = 0; i < n; i++) s = add(s, i); return s; }
for (let k = 0; k < 60; k++) addLoop(100);
out.push(addLoop(100), add(1.5, 2), add("a", 1), add(2147483647, 1), add(-2147483648, -1), count(70000));
console.log(out.join(" "));
"####;

#[test]
fn matches_node() {
    // Read once per process, before the first VM: this file's only test.
    std::env::set_var("ZIPP_OPT", "1");
    std::env::set_var("ZIPP_OPT_DEOPT_EVERY", "3");
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    assert_eq!(out.output.join("\n"), EXPECTED);
}
