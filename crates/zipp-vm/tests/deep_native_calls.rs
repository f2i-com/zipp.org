//! Recursion in compiled code: as deep as the stack the host declared, the same
//! answers as interpreted frames past that, and functions calling each other in
//! a cycle settle after a handful of compiles. Node's output.
#![cfg(all(feature = "jit", target_arch = "x86_64"))]

const EXPECTED: &str = r####"1500 true false
2997000
3000 2100
true
4000
1800"####;

const PROGRAM: &str = r####"function down(n) { return n === 0 ? 0 : 1 + down(n - 1); }
function isEven(n) { return n === 0 ? true : isOdd(n - 1); }
function isOdd(n) { return n === 0 ? false : isEven(n - 1); }
function stepA(n, acc) { return n <= 0 ? acc : stepB(n - 1, (acc + n) | 0); }
function stepB(n, acc) { return n <= 0 ? acc : stepC(n - 1, (acc + n) | 0); }
function stepC(n, acc) { return n <= 0 ? acc : stepA(n - 1, (acc + n) | 0); }
var s = 0;
for (var i = 0; i < 300; i++) { down(1500); isEven(1500); stepA(600, i); }
console.log(down(1500), isEven(1500), isEven(1501));
console.log(stepA(2447, 0) + 3 * 1 - (stepA(2447, 0) - 2997000 + 3));
var o = { d(n) { return n === 0 ? 0 : 1 + this.d(n - 1); } };
class K { constructor(v) { this.v = v; } up(k) { return k === 0 ? this.v : new K(this.v + 1).up(k - 1); } }
for (var i = 0; i < 100; i++) { o.d(100); new K(0).up(100); }
console.log(o.d(3000), new K(0).up(2100));
function inf(n) { return inf(n + 1) + 1; }
try { inf(0); } catch (e) { console.log(e instanceof RangeError); }
console.log(down(4000));
function chain(n) { var box = { n: n }; if (n === 0) return 0; return chain(n - 1) + box.n - n + 1; }
console.log(chain(1800));
"####;

const STACK: usize = 256 << 20;

fn child(declare: bool) -> String {
    let out = std::thread::Builder::new()
        .stack_size(STACK)
        .spawn(move || {
            if declare {
                zipp_vm::set_native_stack_bytes(STACK);
            }
            zipp_vm::run(PROGRAM).expect("compiles")
        })
        .expect("thread")
        .join()
        .expect("no panic");
    assert_eq!(out.error, None, "{:?}", out.errput);
    out.output.join("\n")
}

/// A declaration is the declaring thread's: a VM on a thread with the default
/// stack, as an agent's worker has, keeps the small cap and does not overflow.
fn other_thread() -> String {
    zipp_vm::set_native_stack_bytes(STACK);
    let out = std::thread::spawn(|| zipp_vm::run(PROGRAM).expect("compiles"))
        .join()
        .expect("a default stack holds it");
    assert_eq!(out.error, None, "{:?}", out.errput);
    out.output.join("\n")
}

#[test]
fn deep_native_calls_child() {
    match std::env::var("ZIPP_DEEP_CALLS_CHILD").as_deref() {
        Ok("declared") => assert_eq!(child(true), EXPECTED),
        Ok("default") => assert_eq!(child(false), EXPECTED),
        Ok("other") => assert_eq!(other_thread(), EXPECTED),
        _ => {}
    }
}

fn run_mode(kind: &str, env: &[(&str, &str)]) -> String {
    let mut cmd = std::process::Command::new(std::env::current_exe().expect("test binary"));
    cmd.args(["--exact", "deep_native_calls_child", "--nocapture"])
        .env("ZIPP_DEEP_CALLS_CHILD", kind)
        .env_remove("ZIPP_NOJIT")
        .env_remove("ZIPP_JIT_THRESHOLD")
        .env_remove("ZIPP_JITLOG");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("re-run test binary");
    let (stdout, stderr) = (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    );
    assert!(out.status.success(), "{kind} {env:?}:\n{stdout}\n{stderr}");
    stderr
}

#[test]
fn deep_recursion_agrees_on_every_stack_and_tier() {
    if std::env::var_os("ZIPP_DEEP_CALLS_CHILD").is_some() {
        return;
    }
    for kind in ["declared", "default", "other"] {
        run_mode(kind, &[]);
        run_mode(kind, &[("ZIPP_JIT_THRESHOLD", "1")]);
        run_mode(kind, &[("ZIPP_NOJIT", "1")]);
    }
}

/// A declared stack keeps the recursion compiled past the default's 64 levels,
/// which an undeclared one still stops at; and three functions in a cycle reach
/// a settled set of compiles. They evicted one another a thousand times each.
#[test]
fn a_cycle_settles_and_a_declared_stack_goes_deep() {
    if std::env::var_os("ZIPP_DEEP_CALLS_CHILD").is_some() {
        return;
    }
    let declared = run_mode("declared", &[("ZIPP_JITLOG", "1")]);
    let retries = declared.matches("evicted for cross-entry retry").count();
    assert!(retries < 16, "{retries} retries");
    // The program recurses past both caps on purpose.
    assert!(
        !declared.contains("stack full depth=62") && !declared.contains("call cap depth=64"),
        "a declared stack stopped at the default's depth"
    );
    for kind in ["default", "other"] {
        let log = run_mode(kind, &[("ZIPP_JITLOG", "1")]);
        assert!(
            log.contains("stack full depth=62") || log.contains("call cap depth=64"),
            "{kind}: an undeclared stack must keep the small cap"
        );
    }
}
