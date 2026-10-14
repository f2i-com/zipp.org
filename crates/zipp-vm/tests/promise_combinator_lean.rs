//! The intrinsic combinators skip what they cannot be seen doing: building
//! the result capability through `new Promise(executor)` (async_runtime.rs,
//! `intrinsic_promise_capability`), GetPromiseResolve's Get of
//! `%Promise%.resolve` when it is a data property, GetIterator's Get of
//! `@@iterator` on a plain array, the call of the capability's own resolve
//! function, and the `then` lookup when a promise resolves to a plain array
//! whose prototypes have no `then` (`array_then_absent`). Every step below
//! makes one of those observable or patched (a replaced or getter `resolve`,
//! a non-callable one, an own or patched or getter `@@iterator`, a patched
//! `next`, `then` on Array.prototype or a getter on Object.prototype, an
//! array with its own `then`, a subclass, `Promise.all.call` with a custom
//! constructor), and the rest pin ordering, resolving functions fired twice,
//! the sibling combinators, holes and an await loop. Node 24.19.0's output,
//! with the lean paths on, off, under ZIPP_GC_STRESS=1 and ZIPP_NOJIT=1.

const SRC: &str = include_str!("promise_combinator_lean.js");

const EXPECTED: &str = "-- plain\nplain [1,2,3]\n\
-- empty\nempty [] true\n\
-- ordering\nord job 1\nord all [1,2]\nord job 2\nord job 3\n\
-- resolve patched\nresolve patched [1,2,3] calls 3\n\
-- resolve getter\nresolve getter [1,2] gets 1\n\
-- resolve non-callable\nnoncallable rejected TypeError\n\
-- iterator patched on array\nown iter [7,8]\n\
-- iterator patched on prototype\nproto iter [4,5] calls 1\n\
-- iterator getter on prototype\niter getter [6] gets 1\n\
-- next patched\nnext patched [1,2] calls 3\n\
-- array then on Array.prototype\narray then called\narray then result from then\n\
-- then on Object.prototype\nobject then getter [3] gets>0 true\n\
-- async returns array after then removed\nasync array [1,2]\n\
-- resolve with array having own then\nown then\nown then result own\n\
-- subclass all\nP ctor\nP ctor\nP ctor\nP ctor\nP ctor\nsubclass instance true\nP ctor\nP ctor\nsubclass all [1,2]\n\
-- all.call custom ctor\ncustom ret true\ncustom resolve [1,2]\n\
-- withResolvers and resolve twice\nwithResolvers 1\n\
-- new Promise resolve then reject\nnp a\nnq x\n\
-- allSettled any race\nallSettled [{status:fulfilled,value:1},{status:rejected,reason:2}]\nany 2\nrace 3\n\
-- all rejects\nall rejected boom\n\
-- holes and array-likes\nholes [1,undefined,3]\n\
-- array with own props\nown props [1,2]\n\
-- resolved pairs many\npairs sum 19900\n\
-- await Promise.all in loop\nloop all 3675";

#[test]
fn combinator_lean_child() {
    if std::env::var_os("ZIPP_COMBINATOR_LEAN_CHILD").is_none() {
        return;
    }
    let out = zipp_vm::run(SRC).expect("source compiles");
    assert!(out.error.is_none(), "{:?}", out.error);
    assert_eq!(out.output.join("\n"), EXPECTED);
}

#[test]
fn combinator_lean_matches_node_in_every_mode() {
    if std::env::var_os("ZIPP_COMBINATOR_LEAN_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for mode in ["on", "off", "gc", "nojit"] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "combinator_lean_child", "--nocapture"])
            .env("ZIPP_COMBINATOR_LEAN_CHILD", "1");
        match mode {
            "off" => {
                cmd.env("ZIPP_NO_COMBINATOR_LEAN", "1")
                    .env("ZIPP_NO_PROMISE_CAPABILITY_DIRECT", "1");
            }
            "gc" => {
                cmd.env("ZIPP_GC_STRESS", "1");
            }
            "nojit" => {
                cmd.env("ZIPP_NOJIT", "1");
            }
            _ => {}
        }
        let out = cmd.output().expect("re-run test binary");
        assert!(
            out.status.success(),
            "mode {mode} failed:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
