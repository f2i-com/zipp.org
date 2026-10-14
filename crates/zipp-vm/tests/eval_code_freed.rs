//! Code a state evaluates is freed with the state (f2i-com/zipp.org#53).
//!
//! Every function and class `eval_in_context`, `eval` and `new Function`
//! install used to be `Box::leak`ed, so a host that evaluated a library into a
//! state per page grew by the library's compiled size per page, and dropping
//! the state reclaimed none of it. They now live in an arena the state owns.
//! A counting allocator (this test crate's own) shows the live heap returning
//! to its level after each state is dropped, and a repeated `new Function`
//! source compiles once.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use zipp_vm::embed::compile_script;

struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            LIVE.fetch_add(layout.size(), Ordering::Relaxed);
        }
        p
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if !p.is_null() {
            LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
            LIVE.fetch_add(new_size, Ordering::Relaxed);
        }
        p
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

/// The counter is process-wide, so the tests take turns.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn library() -> String {
    let mut lib = String::from("var lib = {};\n");
    for i in 0..400 {
        lib.push_str(&format!(
            "lib.f{i} = function (a, b) {{ if (a > b) {{ return a - b + {i}; }} return [a, b, {i}].join('-'); }};\n"
        ));
    }
    lib.push_str("class Widget { constructor(n) { this.n = n; } get twice() { return this.n * 2; } }\n");
    lib.push_str("var made = new Function('a', 'b', 'return a * b + lib.f3(a, b).length;');\n");
    lib
}

fn one_state(lib: &str) {
    let mut st = compile_script("").expect("empty script compiles");
    st.run_init().expect("empty script runs");
    st.eval_in_context(lib).expect("library evaluates");
    st.eval_in_context("made(2, 3) + new (eval('(class { })'))().constructor.name.length")
        .expect("library runs");
    drop(st);
}

#[test]
fn a_repeated_function_constructor_source_compiles_once() {
    let _turn = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    // A long-lived state that keeps building the same function (a syntax
    // check, a template engine) used to compile and retain a new copy of its
    // code on every call. Counted directly: memory figures here would mostly
    // measure when the collector last ran.
    let mut st = compile_script("").expect("empty script compiles");
    st.run_init().expect("empty script runs");
    st.eval_in_context(
        "var params = Array.from({ length: 200 }, (_, i) => 'a' + i);\n\
         var body = 'return a0 + a199 + arguments.length;';\n\
         function same(n) { var f; for (var i = 0; i < n; i++) f = new Function(...params, body); return f.length; }\n\
         function distinct(n) { var f; for (var i = 0; i < n; i++) f = new Function(...params, body + '//' + i); return f.length; }",
    )
    .expect("setup runs");
    let before = st.runtime_function_count();
    let r = st.eval_in_context("same(2000)").expect("repeated loop runs");
    let same = st.runtime_function_count() - before;
    assert_eq!(r, zipp_vm::embed::JsValue::Number(200.0));
    let before = st.runtime_function_count();
    st.eval_in_context("distinct(50)").expect("distinct loop runs");
    let distinct = st.runtime_function_count() - before;
    // The script that runs the loop, plus one wrapper program and its
    // function for the source: never one per call.
    assert!(same <= 3, "2000 identical `new Function` calls installed {same} functions");
    assert!(distinct >= 100, "50 distinct sources installed only {distinct} functions");
}

#[test]
fn dropping_a_state_frees_the_code_it_evaluated() {
    let _turn = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let lib = library();
    // One state first: process-wide one-time allocations (interned tables,
    // lazily built statics) are not what this measures.
    one_state(&lib);
    one_state(&lib);
    let before = LIVE.load(Ordering::Relaxed);
    for _ in 0..20 {
        one_state(&lib);
    }
    let after = LIVE.load(Ordering::Relaxed);
    let grew = after.saturating_sub(before);
    // A leaking state kept the library's compiled code: several hundred KB
    // each, so megabytes over twenty states.
    assert!(
        grew < 256 * 1024,
        "the live heap grew by {grew} bytes over 20 dropped states"
    );
}
