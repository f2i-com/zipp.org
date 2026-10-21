//! `zipp_vm::set_unhandled_rejections_fatal`, the `zipp` command's mode (#77):
//! the first rejection still unhandled at a microtask checkpoint ends the
//! program, as Node 23 ends it. The mode is process-wide, so this binary
//! holds one test that runs every case with it on.

#[test]
fn the_fatal_mode_ends_the_program_as_node_does() {
    zipp_vm::set_unhandled_rejections_fatal(true);
    let run = |src: &str| {
        let out = zipp_vm::run_with_base(src, None).expect("compiles");
        (out.output, out.error)
    };
    let (out, err) = run("Promise.reject(new Error('first')); console.log('end of script');");
    assert_eq!(out, ["end of script"]);
    assert_eq!(err.as_deref(), Some("Uncaught (in promise) Error: first"));
    // No later timer runs.
    let (out, err) = run("Promise.reject(42); setTimeout(() => console.log('timer ran'), 0);");
    assert!(out.is_empty(), "{out:?}");
    assert_eq!(err.as_deref(), Some("Uncaught (in promise) 42"));
    // Unhandled at the end of the first task, though a timer would catch it.
    let (out, err) = run("const p = Promise.reject(new Error('late')); setTimeout(() => p.catch(() => console.log('caught late')), 0);");
    assert!(out.is_empty(), "{out:?}");
    assert_eq!(err.as_deref(), Some("Uncaught (in promise) Error: late"));
    // A rejection in a timer ends the program after that timer.
    let (out, err) = run("setTimeout(() => { Promise.reject(new Error('from timer')); console.log('t1'); }, 0); setTimeout(() => console.log('t2'), 10);");
    assert_eq!(out, ["t1"]);
    assert_eq!(err.as_deref(), Some("Uncaught (in promise) Error: from timer"));
    // Handled within the checkpoint, or awaited: nothing to report.
    let (out, err) = run("const p = Promise.reject(new Error('x')); Promise.resolve().then(() => p.catch(e => console.log('caught', e.message)));");
    assert_eq!((out, err), (vec!["caught x".to_string()], None));
    let (out, err) = run("(async () => { try { await Promise.reject(new Error('y')); } catch (e) { console.log('awaited', e.message); } })();");
    assert_eq!((out, err), (vec!["awaited y".to_string()], None));
    let (out, err) = run("Promise.allSettled([Promise.reject(1)]).then(r => console.log(r[0].status)); Promise.any([Promise.reject(1), Promise.resolve(2)]).then(v => console.log(v));");
    assert_eq!((out, err), (vec!["rejected".to_string(), "2".to_string()], None));
    // The program's own throw comes first.
    let (_, err) = run("Promise.reject(new Error('second')); throw new Error('first');");
    assert_eq!(err.as_deref(), Some("Error: first"));
    // A module's rejected body is its error, not an unhandled rejection.
    let out = zipp_vm::run_module_with_base("await null; throw new Error('module');", None).expect("compiles");
    assert_eq!(out.error.as_deref(), Some("Error: module"));
    zipp_vm::set_unhandled_rejections_fatal(false);
}
