//! The Python tier's per-VM registry and per-site caches (`vm::py_rt`) are
//! per VM: several VMs running the same program (the same function ids and
//! instruction positions) at once, on one thread or on several, each see
//! only their own classes and globals.
#![cfg(feature = "python")]
use zipp_vm::frontend::{compile_source, Frontend, PythonMode};
use zipp_vm::embed::ScriptState;

const CLASSES: &str = include_str!("../../../tests/python_corpus/py_native_s1_classes.py");
const CLASSES_OUT: &str = include_str!("../../../tests/python_corpus/py_native_s1_classes.out");
const GLOBALS: &str = include_str!("../../../tests/python_corpus/py_native_s1_globals.py");
const GLOBALS_OUT: &str = include_str!("../../../tests/python_corpus/py_native_s1_globals.out");

fn lines(expected: &str) -> Vec<String> {
    expected.replace("\r\n", "\n").lines().map(str::to_owned).collect()
}

fn state(source: &str) -> ScriptState {
    let mut state = compile_source(source, Frontend::Python { mode: PythonMode::Module }).expect("compiles").into_state();
    state.set_limits(500_000_000, None);
    state
}

/// Run on a thread with the stack a Python program needs.
fn on_big_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(256 << 20).spawn(f).expect("spawn").join().expect("test thread")
}

#[test]
fn two_vms_on_one_thread_keep_their_own_caches() {
    on_big_stack(|| {
        // The same program twice, and a variant whose classes differ, all
        // alive at once: identical functions at identical positions.
        let variant = CLASSES.replace("return \"p\"", "return \"P\"").replace("kind = \"p\"", "kind = \"k\"");
        let mut a = state(CLASSES);
        let mut b = state(&variant);
        let mut c = state(CLASSES);
        a.run_init().expect("a runs");
        b.run_init().expect("b runs");
        c.run_init().expect("c runs");
        assert_eq!(a.take_output(), lines(CLASSES_OUT));
        assert_eq!(c.take_output(), lines(CLASSES_OUT));
        let b_out = b.take_output();
        assert_ne!(b_out, lines(CLASSES_OUT));
        assert!(b_out.iter().any(|l| l.contains("'P'")), "the variant's own method answered: {b_out:?}");
    });
}

#[test]
fn vms_on_several_threads_at_once() {
    let handles: Vec<_> = (0..4)
        .map(|i| {
            std::thread::Builder::new()
                .stack_size(256 << 20)
                .spawn(move || {
                    let (src, out) = if i % 2 == 0 { (CLASSES, CLASSES_OUT) } else { (GLOBALS, GLOBALS_OUT) };
                    let mut s = state(src);
                    s.run_init().expect("runs");
                    assert_eq!(s.take_output(), lines(out));
                })
                .expect("spawn")
        })
        .collect();
    for h in handles {
        h.join().expect("thread");
    }
}
