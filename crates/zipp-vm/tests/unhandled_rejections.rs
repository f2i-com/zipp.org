//! Unhandled promise rejections reach the host (#77): an embedder's sink and
//! rejection hook hear of each one at a microtask checkpoint, and with
//! `set_unhandled_rejections_fatal` (the `zipp` command's mode) the first
//! one ends the program as Node ends it (tests/unhandled_rejections_fatal.rs,
//! a binary of its own because that mode is process-wide). Expected
//! behaviour is Node 23's.

use std::cell::RefCell;
use std::rc::Rc;
use zipp_vm::embed::{compile_script, JsValue, ScriptState};

fn state_with_sink(src: &str) -> (ScriptState, Rc<RefCell<Vec<String>>>) {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let mut st = compile_script(src).expect("compiles");
    let sink = seen.clone();
    st.set_unhandled_rejection_sink(Box::new(move |msg| sink.borrow_mut().push(msg.to_string())));
    (st, seen)
}

#[test]
fn the_sink_hears_each_unhandled_rejection_once() {
    let (mut st, seen) = state_with_sink(
        r#"
        Promise.reject(new Error("first"));
        Promise.reject(42);
        new Promise((_, reject) => reject(new TypeError("third")));
        (async () => { throw new RangeError("async"); })();
        Promise.reject(new Error("caught")).catch(() => {});
        const later = Promise.reject(new Error("caught in a job"));
        Promise.resolve().then(() => later.catch(() => {}));
        Promise.reject(new Error("chained")).then(() => {});
        console.log("end of script");
        "#,
    );
    st.run_init().expect("runs");
    assert_eq!(st.take_output(), ["end of script"]);
    assert_eq!(
        *seen.borrow(),
        ["Error: first", "42", "TypeError: third", "RangeError: async", "Error: chained"]
    );
    st.run_microtasks();
    assert_eq!(seen.borrow().len(), 5, "reported once");
}

#[test]
fn later_checkpoints_report_later_rejections() {
    let (mut st, seen) = state_with_sink("function fail(m) { Promise.reject(new Error(m)); }");
    st.run_init().expect("runs");
    assert!(seen.borrow().is_empty());
    st.eval_in_context("fail('from eval')").expect("runs");
    assert!(seen.borrow().is_empty(), "not before a checkpoint");
    st.run_microtasks();
    assert_eq!(*seen.borrow(), ["Error: from eval"]);
}

#[test]
fn the_hook_gets_both_events_with_the_promise() {
    let mut st = compile_script(
        r#"
        var events = [];
        var keep;
        function onRejection(kind, promise, reason) {
            events.push(kind + ":" + reason.message + ":" + (promise instanceof Promise));
            if (kind === "unhandledrejection") Promise.resolve().then(() => events.push("job ran"));
        }
        "#,
    )
    .expect("compiles");
    st.run_init().expect("runs");
    st.set_promise_rejection_hook("onRejection").expect("installs");
    st.eval_in_context("keep = Promise.reject(new Error('late'))").expect("runs");
    st.run_microtasks();
    assert_eq!(
        st.eval_in_context("events.join()"),
        Ok(JsValue::String("unhandledrejection:late:true,job ran".into()))
    );
    st.eval_in_context("keep.catch(() => {})").expect("runs");
    st.run_microtasks();
    assert_eq!(
        st.eval_in_context("events.slice(2).join()"),
        Ok(JsValue::String("rejectionhandled:late:true".into()))
    );
    st.run_microtasks();
    assert_eq!(st.eval_in_context("events.length"), Ok(JsValue::Number(3.0)));
}

#[test]
fn a_bad_hook_is_refused_and_a_throwing_one_is_reported() {
    let mut st = compile_script("function boom() { throw new Error('hook threw'); }").expect("compiles");
    st.run_init().expect("runs");
    assert!(st.set_promise_rejection_hook("42").is_err());
    assert!(st.set_promise_rejection_hook("nope").is_err());
    st.set_promise_rejection_hook("boom").expect("installs");
    st.eval_in_context("Promise.reject(1)").expect("runs");
    st.run_microtasks();
    assert_eq!(st.take_errput(), ["Uncaught Error: hook threw"]);
}

#[test]
fn without_a_sink_nothing_changes() {
    let mut st = compile_script("Promise.reject(new Error('dropped')); console.log('ran');").expect("compiles");
    assert!(st.run_init().is_ok());
    assert_eq!(st.take_output(), ["ran"]);
    assert!(st.take_errput().is_empty());
}
