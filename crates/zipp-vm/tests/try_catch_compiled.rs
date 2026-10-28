//! `try`/`catch` and BigInt literals in hot functions, which Tier C
//! compiles (a handler push on the activation's own frame; a throw inside
//! unwinds to the `catch` through the interpreter): every way control
//! leaves a `try` (return, throw, rethrow, `continue`/`break` in a loop,
//! a throw from deep recursion, a `finally` that returns), destructured
//! and absent catch bindings, non-Error throws, and BigInt shifts. Run at
//! the default thresholds and with every function compiled at once; the
//! expected lines are Node 24's.

const EXPECTED: &[&str] = &[
    r#"36024"#,
    r#"31129205"#,
    r#"0|a0f|outer:TypeError|2|TypeErrortrue|parsed|f|0|bottom0|0|0|-1"#,
    r#"t|3998|bottom18|7998|a7998f|ok3999|R|TypeErrortrue|parsed|f|3999|bottom19"#,
    r#"undefined"#,
];

fn check(threshold: Option<&str>) {
    // The JIT threshold is read once per process, so each setting runs in a
    // child of this test binary.
    let exe = std::env::current_exe().expect("test binary path");
    let mut cmd = std::process::Command::new(&exe);
    cmd.args(["--exact", "try_catch_child", "--nocapture"])
        .env("ZIPP_TRY_CATCH_CHILD", "1");
    if let Some(t) = threshold {
        cmd.env("ZIPP_JIT_THRESHOLD", t);
    }
    let out = cmd.output().expect("re-run test binary");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn try_catch_child() {
    if std::env::var_os("ZIPP_TRY_CATCH_CHILD").is_none() {
        return;
    }
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    let lines: Vec<&str> = out.output.iter().flat_map(|l| l.lines()).collect();
    assert_eq!(lines, EXPECTED);
}

#[test]
fn try_catch_in_compiled_code_runs_as_node_runs_it() {
    if std::env::var_os("ZIPP_TRY_CATCH_CHILD").is_some() {
        return;
    }
    check(None);
    check(Some("1"));
}

const PROGRAM: &str = r####"// try/catch in hot functions, every way control can leave a try.
const out = [];
function thrower(x) { if (x % 7 === 3) throw new RangeError("r" + x); return x * 2; }
function basic(x) { try { return thrower(x); } catch (e) { return e.message; } }
function withFinally(x) { let s = "a"; try { s += thrower(x); } catch (e) { s += "c"; } finally { s += "f"; } return s; }
function nested(x) {
  try {
    try { if (x % 5 === 0) throw new TypeError("inner"); return "ok" + x; }
    catch (e) { if (x % 10 === 0) throw e; return "caught-inner"; }
  } catch (e2) { return "outer:" + e2.constructor.name; }
}
function rethrow(x) { try { return basic(x) + thrower(x + 1); } catch (e) { if (e instanceof RangeError) return "R"; throw e; } }
function inLoop(n) {
  let s = 0;
  for (let i = 0; i < n; i++) {
    try { if (i % 4 === 0) continue; if (i % 9 === 8) break; s += thrower(i); }
    catch (e) { s -= 1; }
  }
  return s;
}
function catchBinding(x) { try { null.f(x); } catch ({ name, message }) { return name + (message.length > 0); } }
function noBinding(x) { try { JSON.parse(x % 3 ? "{" : "{}"); return "parsed"; } catch { return "bad"; } }
function returnInFinally0(x) { try { if (x & 1) throw 1; return "t"; } finally { if (x % 3 === 0) return "f"; } }
function returnInFinally(x) { try { return returnInFinally0(x); } catch (e) { return "esc" + e; } }
function throwNonError(x) { try { throw x % 2 ? { v: x } : x; } catch (e) { return typeof e === "object" ? e.v : e; } }
function deep(x) { if (x === 0) throw new Error("bottom"); return deep(x - 1); }
function catchDeep(x) { try { return deep(x % 20); } catch (e) { return e.message + (x % 20); } }
function bigints(n) { let r = 0n, sh = 0n; for (let i = 0; i < n; i++) { r |= BigInt(i & 0x7f) << sh; sh += 7n; if (sh > 70n) sh = 0n; } return r; }
function bigTry(x) { try { return BigInt(x) * 3n; } catch (e) { return -1n; } }
let acc = [];
for (let i = 0; i < 4000; i++) {
  acc.push(basic(i), withFinally(i), nested(i), rethrow(i), catchBinding(i), noBinding(i), returnInFinally(i), throwNonError(i), catchDeep(i));
  if (i % 500 === 0) acc.push(inLoop(i % 50), String(bigints(i % 30)), String(bigTry(i % 2 ? i : 1.5)));
}
let h = 0;
for (const v of acc) { const s = String(v); for (let k = 0; k < s.length; k++) h = (h * 31 + s.charCodeAt(k)) | 0; }
out.push(acc.length, h, acc.slice(0, 12).join("|"), acc.slice(-12).join("|"));
let thrownOut;
try { rethrow(9); rethrow(5); } catch (e) { thrownOut = e.message; }
out.push(String(thrownOut));
console.log(out.join("\n"));
"####;
