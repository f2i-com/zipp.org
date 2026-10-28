//! A `switch` over string literals in compiled code compares the
//! discriminant's bits with each case's first (`str_case_chains`), and the
//! literals it compares with are the shared ones (`Vm::jit_const_str_bits`).
//! Discriminants that are the literals, strings equal by content only (built,
//! sliced, parsed), repeated case texts, defaults, no match, non-string
//! discriminants, fall-through, a non-literal case mid-run and a hand-written
//! `===` chain. Run interpreted, at the default thresholds, with every
//! function compiled at once, under GC stress, with the bits test off and with
//! fresh literals in compiled code; the expected lines are Node 24's.

const EXPECTED: &str = r####"acc 0
kind:num 3000
kind:var 6000
kind:bin 9000
kind:call 12000
kind:nope 0
built:num 3000
built:var 6000
built:bin 9000
built:call 12000
built:xyz 0
built:nu 0
dup:alpha:first 6000
dup:beta:second 3000
dup:gamma:third 3000
dup:delta:none 3000
fall:one:12 6000
fall:two:2 3000
fall:three:3d 3000
fall:four:d 3000
mixed:left:L 3000
mixed:call:C 6000
mixed:right:R 3000
mixed:up:U 3000
mixed:down:- 3000
mixed:7:- 3000
chain:red 3000
chain:green 12000
chain:blue 9000
chain:pink 0
chain:3 0"####;

#[test]
fn string_switch_bits_child() {
    if std::env::var_os("ZIPP_STRSWITCH_CHILD").is_none() {
        return;
    }
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    let lines: Vec<&str> = out.output.iter().flat_map(|l| l.lines()).collect();
    assert_eq!(lines, EXPECTED.lines().collect::<Vec<_>>());
}

#[test]
fn string_switches_match_node() {
    if std::env::var_os("ZIPP_STRSWITCH_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, env) in [
        ("interpreter", Some(("ZIPP_NOJIT", "1"))),
        ("default-thresholds", None),
        ("compile-at-once", Some(("ZIPP_JIT_THRESHOLD", "1"))),
        ("gc-stress", Some(("ZIPP_GC_STRESS", "1"))),
        ("bits-off", Some(("ZIPP_NO_STR_CASE_BITS", "1"))),
        ("fresh-literals", Some(("ZIPP_NO_JIT_SHARED_CONSTS", "1"))),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "string_switch_bits_child", "--nocapture"])
            .env("ZIPP_STRSWITCH_CHILD", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_JIT_THRESHOLD")
            .env_remove("ZIPP_GC_STRESS");
        if let Some((k, v)) = env {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("re-run test binary");
        assert!(
            out.status.success(),
            "{mode}:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

const PROGRAM: &str = r####"// `switch` over string literals in compiled code: discriminants that are the
// case literals themselves, strings equal by content only (built at run
// time, sliced, from JSON), repeated case texts, a default, no match,
// non-string discriminants (numbers, booleans, undefined, objects, a
// String wrapper), fall-through between cases, a case the run reaches past
// a non-literal case, and `===` chains written out by hand. Each function
// runs often enough to be compiled.
const out = [];
function kind(k) {
  switch (k) {
    case "num": return 1;
    case "var": return 2;
    case "bin": return 3;
    case "call": return 4;
    default: return 0;
  }
}
function dup(k) {
  switch (k) {
    case "alpha": return "first";
    case "beta": return "second";
    case "alpha": return "repeat";
    case "gamma": return "third";
  }
  return "none";
}
function fall(k) {
  let s = "";
  switch (k) {
    case "one": s += "1";
    case "two": s += "2"; break;
    case "three": s += "3";
    default: s += "d";
  }
  return s;
}
const dyn = "ca";
function mixed(k) {
  switch (k) {
    case "left": return "L";
    case dyn + "ll": return "C";
    case "right": return "R";
    case "up": return "U";
  }
  return "-";
}
function chain(k) {
  if (k === "red") return 1;
  if (k === "green") return 2;
  if (k === "blue") return 3;
  return 0;
}
const built = ["n" + "um", "v" + "ar", "bin".slice(0), JSON.parse('"call"'), "xyz", "nu"];
const others = [1, 0, -0, 1.5, NaN, true, false, null, undefined, {}, [], new String("num"), Symbol.iterator];
let acc = 0;
const tally = new Map();
const note = (k, v) => tally.set(k, (tally.get(k) || 0) + (typeof v === "number" ? v : v.length));
for (let i = 0; i < 3000; i++) {
  for (const k of ["num", "var", "bin", "call", "nope"]) note("kind:" + k, kind(k));
  for (const k of built) note("built:" + k, kind(k));
  for (const k of others) acc += kind(k);
  for (const k of ["alpha", "beta", "gamma", "alp" + "ha", "delta"]) note("dup:" + k + ":" + dup(k), 1);
  for (const k of ["one", "two", "three", "four", "o" + "ne"]) note("fall:" + k + ":" + fall(k), 1);
  for (const k of ["left", "call", "c" + "all", "right", "up", "down", 7]) note("mixed:" + String(k) + ":" + mixed(k), 1);
  for (const k of ["red", "green", "gr" + "een", "blue", "pink", 3]) note("chain:" + String(k), chain(k));
}
out.push("acc " + acc);
for (const [k, v] of tally) out.push(k + " " + v);
console.log(out.join("\n"));
"####;
