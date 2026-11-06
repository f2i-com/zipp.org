//! `instanceof` against an ordinary function remembers the function's
//! right-hand side checks and its `prototype` (`Vm::instanceof_memo`),
//! valid while the function's slot version and `Vm::fn_props_gen` stand.
//! Each way those facts change after the memo filled: `prototype`
//! reassigned and restored, an own `Symbol.hasInstance` defined and
//! deleted, the function's own [[Prototype]] swapped, a non-object
//! `prototype` (the operator's TypeError), closures of one code, a receiver
//! and a chain link re-prototyped, a bound function. The expected lines are
//! Node's (v24), with the memo, without it (`ZIPP_NO_INSTANCEOF_MEMO`) and
//! in the interpreter.

const SOURCE: &str = r##"
"use strict";
const out = [];
function count(f, xs, n) { let k = 0; for (let r = 0; r < n; r++) for (const x of xs) if (x instanceof f) k++; return k; }
function A() {} function B() {}
const pa = A.prototype;
const xs = [new A(), new A(), new B(), {}, Object.create(pa)];
out.push("base " + count(A, xs, 2000));
// prototype reassigned after the memo filled
A.prototype = B.prototype;
out.push("reassign " + count(A, xs, 2000));
A.prototype = pa;
out.push("back " + count(A, xs, 2000));
// Symbol.hasInstance defined on the function itself later
Object.defineProperty(A, Symbol.hasInstance, { value: (v) => v === xs[3], configurable: true });
out.push("hasInstance " + count(A, xs, 2000));
delete A[Symbol.hasInstance];
out.push("deleted " + count(A, xs, 2000));
// the function's own [[Prototype]] swapped to an object with Symbol.hasInstance
function C() {}
const cs = [new C(), new A(), {}];
out.push("c0 " + count(C, cs, 2000));
Object.setPrototypeOf(C, { [Symbol.hasInstance](v) { return v === cs[1]; } });
out.push("cproto " + count(C, cs, 2000));
// a non-object prototype after the memo filled: TypeError from the operator
function D() {}
const ds = [new D(), {}];
out.push("d0 " + count(D, ds, 2000));
D.prototype = 42;
try { count(D, ds, 10); out.push("d1 no throw"); } catch (e) { out.push("d1 " + e.constructor.name); }
// closures made per call share code but not identity
function mk() { return function K() {}; }
const K1 = mk(), K2 = mk();
const ks = [new K1(), new K2(), new K1()];
out.push("k " + count(K1, ks, 2000) + " " + count(K2, ks, 2000));
// a receiver re-prototyped after the memo filled
const r = new A();
out.push("r0 " + count(A, [r], 2000));
Object.setPrototypeOf(r, B.prototype);
out.push("r1 " + count(A, [r], 2000) + " " + count(B, [r], 2000));
// a deep chain whose middle link changes
function N0() {} function N1() {} N1.prototype = Object.create(N0.prototype);
function N2() {} N2.prototype = Object.create(N1.prototype);
const n = new N2();
out.push("n0 " + count(N0, [n], 2000));
Object.setPrototypeOf(N1.prototype, Object.prototype);
out.push("n1 " + count(N0, [n], 2000) + " " + count(N1, [n], 2000));
// bound functions resolve to their target
const BA = A.bind(null);
out.push("bound " + count(BA, xs, 1000));
console.log(out.join("\n"));
"##;

const EXPECTED: &str = "base 6000\nreassign 2000\nback 6000\nhasInstance 2000\ndeleted 6000\nc0 2000\ncproto 2000\nd0 2000\nd1 TypeError\nk 4000 2000\nr0 2000\nr1 0 2000\nn0 2000\nn1 0 2000\nbound 3000";

#[test]
fn instanceof_fn_memo_worker() {
    if std::env::var_os("ZIPP_INSTANCEOF_MEMO_WORKER").is_none() {
        return;
    }
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    assert_eq!(out.output.join("\n"), EXPECTED);
}

#[test]
fn instanceof_fn_memo_matches_node() {
    if std::env::var_os("ZIPP_INSTANCEOF_MEMO_WORKER").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test executable");
    for (mode, var) in [("default", None), ("memo-off", Some("ZIPP_NO_INSTANCEOF_MEMO")), ("interpreter", Some("ZIPP_NOJIT"))] {
        let mut command = std::process::Command::new(&exe);
        command
            .args(["instanceof_fn_memo_worker", "--exact", "--nocapture"])
            .env("ZIPP_INSTANCEOF_MEMO_WORKER", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_NO_INSTANCEOF_MEMO");
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