//! What keeps a run with Node's globals going: a timeout that was cleared
//! or `unref()`'d does not (its native timeout is cancelled, or left out of
//! the loop's reckoning), as `AbortSignal.timeout`'s pending timer does not;
//! an unref'd timeout still runs while something else keeps the loop going,
//! and `ref()` after `unref()` counts again. Node exits such a program at
//! once; zipp used to wait out every cleared or unref'd timer.

const PROGRAM: &str = r#"
const log = [];
clearTimeout(setTimeout(() => log.push("cleared fired"), 4000));
setTimeout(() => log.push("unref never"), 4000).unref();
const u = setTimeout(() => log.push("unref then ref"), 30); u.unref(); u.ref();
setTimeout(() => log.push("unref but loop alive"), 10).unref();
let n = 0; const iv = setInterval(() => { log.push("iv" + n); if (++n === 3) clearInterval(iv); }, 5);
const r = setTimeout(() => log.push("refreshed"), 20); setTimeout(() => r.refresh(), 10);
AbortSignal.timeout(4000).addEventListener("abort", () => log.push("abort never"));
const ac = AbortSignal.timeout(15); ac.onabort = () => log.push("timeout " + ac.reason.name);
setTimeout(() => {}, 40);
process.on("exit", () => console.log(log.sort().join(",")));
"#;

#[test]
fn cleared_and_unrefd_timers_do_not_keep_the_run_going() {
    zipp_vm::set_node_runtime(true, vec!["zipp".into(), "app.js".into()]);
    let start = std::time::Instant::now();
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    let elapsed = start.elapsed();
    assert_eq!(out.error, None, "{:?}", out.errput);
    let lines: Vec<&str> = out.output.iter().flat_map(|l| l.lines()).collect();
    assert_eq!(
        lines,
        ["iv0,iv1,iv2,refreshed,timeout TimeoutError,unref but loop alive,unref then ref"]
    );
    assert!(elapsed < std::time::Duration::from_millis(2500), "{elapsed:?}");
}
