//! An `await` of an already-fulfilled promise or a primitive, in an async
//! activation resumed by its own job while the microtask queue is empty,
//! continues in place in the interpreter's `Await` arm (async_runtime.rs,
//! `await_continue_in_place`). The resume job it stands for would have been
//! the next job to run, so every interleaving must be what Node prints:
//! settled promises and primitives (numbers, strings) awaited alone, a job
//! queued from inside the loop, a `then` on a settled promise registered
//! before the await, a rejection caught mid-loop, an async call awaited in a
//! loop, a generator and array callbacks between awaits, mapped `arguments`
//! and `this` across awaits, objects with no `then`, the budget boundary with
//! a job queued past it, and `Promise.all` awaited.
//!
//! s11 and s12 attack the mark itself: a `then` getter that starts an async
//! generator or an async function while the activation is suspended. s12's
//! getter is the generator's own bound `next`, which takes no frame, so the
//! generator's body runs at the frame depth the activation just left; its
//! `await 1` must suspend (a first version continued it in place, and the
//! job it queued then ran before the activation resumed). Node 24.19.0's
//! output.

const SRC: &str = include_str!("await_in_place.js");

const EXPECTED: &str = "-- s1\ns1 2015890\nret undefined\n\
-- s2\ns2 job 0\ns2 after 0\ns2 after 1\ns2 job 2\ns2 after 2\ns2 after 3\ns2 job 4\ns2 after 4\ns2 after 5\nret undefined\n\
-- s3\ns3 a\ns3 then x\ns3 b\ns3 c\nret r3\n\
-- s4\ns4 caught e2\ns4 8\nret undefined\n\
-- s5\ns5 r 1\ns5 r 3\ns5 r 5\ns5 r 7\ns5 16\nret undefined\n\
-- s6\ns6 21\nret undefined\n\
-- s7\ns7 9 2 2\ns7 this 3\nret undefined\n\
-- s8\ns8 true true true\nret undefined\n\
-- s9\ns9 job at 1500\ns9 2500\nret undefined\n\
-- s10\ns10 0,1,2,3,4,5,6,7,8,9\ns10b 1,2,3\nret undefined\n\
-- s11\ngetter1\nag1 start\ngetter1 end\nag1 after await 1\ns11 awaited true\ngetter2\naf start\ngetter2 end\nag1 post-await job\naf after\ns11 awaited2 true\nag4 start\nag1 next 1\ns11 queued job\nag4 after await 1\ns11 awaited4 true\ns11 before await o5 alone\nag5 start\nag4 post-await job\nag5 after await 1\ns11 awaited5 true\ngetter3\nag3 start\ngetter3 end\nag5 post-await job\nag3 after await 1\nret [object Object]\nag3 post-await job\n\
-- s12\nag6 start\nag6 after await 1\ns12 awaited true\nag6 post-await job\nag7 start\nag7 after await 1\nret [object Object]\nag7 post-await job";

#[test]
fn await_in_place_child() {
    let Some(mode) = std::env::var_os("ZIPP_AWAIT_IN_PLACE_CHILD") else {
        return;
    };
    let out = zipp_vm::run(SRC).expect("source compiles");
    assert!(out.error.is_none(), "{:?}", out.error);
    assert_eq!(out.output.join("\n"), EXPECTED);
    let hits = zipp_vm::async_inline_await_stats();
    if mode == "off" {
        assert_eq!(hits, 0, "the off switch must suspend at every await");
    } else {
        // s1 awaits 4,000 primitives and s9 2,500: the trampoline alone
        // continued none of them (it took a promise only).
        assert!(hits > 8_000, "settled awaits did not continue in place: {hits}");
    }
}

#[test]
fn await_in_place_matches_node_in_every_mode() {
    if std::env::var_os("ZIPP_AWAIT_IN_PLACE_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for mode in ["on", "off", "gc", "nojit"] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "await_in_place_child", "--nocapture"])
            .env("ZIPP_AWAIT_IN_PLACE_CHILD", mode)
            .env("ZIPP_ASYNCSTATS", "1");
        match mode {
            "off" => {
                cmd.env("ZIPP_NO_ASYNC_SETTLED_TRAMPOLINE", "1");
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
