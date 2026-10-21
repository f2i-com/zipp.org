//! `switch` over string literals (`Instr::FuseStrSwitch`): one table lookup
//! answers what the cases' compares did, for every kind of discriminant.

use std::process::Command;

const SOURCE: &str = r#"
function kind(t) {
  switch (t) {
    case "BlockStatement":
    case "CatchClause":
      return "block";
    case "Identifier":
      return "id";
    case "Identifier":
      return "dup";
    case "":
      return "empty";
    case "x":
      return "x";
    default:
      return "other";
    case "MemberExpression":
      return "member";
    case "ForStatement":
    case "WhileStatement":
      return "loop";
  }
}
function fall(t) {
  let out = "";
  switch (t) {
    case "a1": out += "a1,";
    case "b2": out += "b2,";
    case "c3": out += "c3,"; break;
    case "d4": out += "d4,";
    default: out += "def,";
    case "e5": out += "e5,";
  }
  return out;
}
function wide(t) {
  switch (t) {
    case "café": return "cafe";
    case "naïve": return "naive";
    case "plain": return "plain";
    case "\ud800": return "lone";
    case "z": return "z";
  }
  return "none";
}
const sym = Symbol("Identifier");
const inputs = [
  "BlockStatement", "CatchClause", "Identifier", "", "x", "MemberExpression",
  "ForStatement", "WhileStatement", "Unknown", "identifier", "Identifie",
  "Identifierr", 0, 1, null, undefined, true, {}, [], sym,
  new String("Identifier"), { toString() { return "Identifier"; } },
  "Ident" + "ifier".slice(0), ["Member", "Expression"].join(""),
];
const out = [];
for (let r = 0; r < 3; r++) {
  for (const t of inputs) out.push(kind(t));
}
for (const t of ["a1", "b2", "c3", "d4", "e5", "zz", 7, "a" + "1"]) out.push(fall(t));
for (const t of ["café", "cafe", "naïve", "plain", "\ud800", "\udc00", "z", "pla" + "in", 5]) out.push(wide(t));
let n = 0;
for (let i = 0; i < 20000; i++) if (kind(inputs[i % inputs.length]) === "other") n++;
out.push(n);
console.log(out.join(" "));
"#;

/// Node 24's output for `SOURCE`.
const EXPECTED: &str = "block block id empty x member loop loop other other other other other other other other other other other other other other id member block block id empty x member loop loop other other other other other other other other other other other other other other id member block block id empty x member loop loop other other other other other other other other other other other other other other id member a1,b2,c3, b2,c3, c3, d4,def,e5, e5, def,e5, def,e5, a1,b2,c3, cafe none naive plain lone none z plain none 11662";

#[test]
fn execution_mode_child() {
    if std::env::var_os("ZIPP_STR_SWITCH_CHILD").is_none() {
        return;
    }
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "runtime error: {:?}", out.error);
    assert_eq!(out.output, vec![EXPECTED.to_string()]);
}

#[test]
fn interpreted_compiled_and_gc_modes_match() {
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, env) in [
        ("nojit", &[("ZIPP_NOJIT", "1")][..]),
        ("nojit_gc", &[("ZIPP_NOJIT", "1"), ("ZIPP_GC_STRESS", "1")][..]),
        ("hot", &[("ZIPP_JIT_THRESHOLD", "1")][..]),
    ] {
        let out = Command::new(&exe)
            .args(["execution_mode_child", "--exact"])
            .env("ZIPP_STR_SWITCH_CHILD", "1")
            .env_remove("ZIPP_JIT_THRESHOLD")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_GC_STRESS")
            .envs(env.iter().copied())
            .output()
            .expect("spawn mode child");
        assert!(
            out.status.success()
                && !String::from_utf8_lossy(&out.stdout).contains("running 0 tests"),
            "{mode} child failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}