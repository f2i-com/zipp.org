//! Embedding API — a persistent VM for hosts that outlive a single script.
//!
//! [`crate::run`] is the batch front end: parse, execute, hand back the output,
//! drop everything. That is the right shape for `zipp js file.js`, and the wrong
//! shape for a host that has to keep talking to the script — a browser, say,
//! which runs a page's JS, renders the result, and then needs the SAME global
//! context alive minutes later to invoke a click handler and observe what the
//! handler changed.
//!
//! This module keeps the VM alive:
//!
//! ```ignore
//! let mut st = zipp_vm::embed::compile_script(source)?;
//! st.set_host_call(Box::new(|kind, args| match kind {
//!     "http.get" => Ok(blocking_get(&args[0])),
//!     _ => Err(format!("TypeError: unknown host call {kind}")),
//! }));
//! st.run_init()?;                                    // top-level execution
//! let html = st.eval_in_context("render()")?;        // later, same globals
//! ```
//!
//! Nothing here is new engine machinery. `run_init` is [`crate::vm::Vm::run`],
//! `eval_in_context` is the `$262.evalScript` pipeline (script-goal
//! GlobalDeclarationInstantiation: top-level `var`, `function`, `let`, `const`
//! and `class` all bind persistent realm globals, so successive evals see each
//! other's declarations), and `call_global` is the ordinary internal call path.
//! The one genuinely new capability is [`ScriptState::set_host_call`] — the
//! `__zippHostCall` native, which is the only channel from JS into
//! embedder-supplied Rust and is inert unless a host is installed.

use crate::bytecode::Program;
use crate::value::Value;
use crate::vm::Vm;

pub use crate::vm::host_api::{HostValue, Symbol, SymbolScope};

/// The embedder's side of `__zippHostCall(kind, ...args)`. Arguments arrive
/// already stringified and the reply is a string, so anything structured
/// crosses as JSON — both sides have a JSON implementation and neither needs to
/// know the other's value representation. `Err` becomes a JS throw the script
/// can `try`/`catch`; include the `TypeError: ` / `Error: ` prefix you want the
/// script to see.
pub type HostCall = Box<dyn FnMut(&str, &[String]) -> Result<String, String>>;

/// A JS value marshalled out of the VM. Deliberately shallow: the engine's
/// `Value` is a NaN-boxed heap index whose meaning depends on the live VM, so
/// handing one to an embedder would be handing out a dangling reference the
/// moment the VM moves on. Primitives cross by value; everything else crosses
/// as its `ToString`, and structured data should cross as JSON via
/// `eval_in_context("JSON.stringify(...)")`.
#[derive(Debug, Clone, PartialEq)]
pub enum JsValue {
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    /// An object, array or function, rendered via `ToString`.
    Object(String),
}

impl JsValue {
    /// The string payload of a `String` result, or `None` for anything else.
    /// The common embedder shape: evaluate an expression documented to produce
    /// a string and take it, treating any other type as a failure rather than
    /// silently coercing.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            JsValue::String(s) => Some(s),
            _ => None,
        }
    }

    /// Truthiness, per JS `ToBoolean`. `Object(_)` is always true (every object
    /// is truthy, and `document.all` is not something this engine models).
    pub fn truthy(&self) -> bool {
        match self {
            JsValue::Undefined | JsValue::Null => false,
            JsValue::Bool(b) => *b,
            JsValue::Number(n) => *n != 0.0 && !n.is_nan(),
            JsValue::String(s) => !s.is_empty(),
            JsValue::Object(_) => true,
        }
    }
}

/// A compiled program plus the live VM executing it.
///
/// `Vm<'p>` borrows its `Program`, which makes the pair self-referential and so
/// not expressible in safe Rust. The `Program` is therefore heap-allocated and
/// leaked to `&'static` for the VM to borrow, and reclaimed in `Drop` after the
/// VM is torn down. The invariant that makes this sound: the `&'static Program`
/// is handed to exactly one `Vm`, that `Vm` is owned by this struct and never
/// escapes it, and `Drop` drops the `Vm` before freeing the `Program`.
///
/// Not `Send`: the VM holds raw pointers into its own heap and register file
/// (the JIT pins them), so a `ScriptState` stays on the thread that built it.
pub struct ScriptState {
    /// `Option` purely so `Drop` can run the VM's destructor at a chosen moment
    /// — before the `Program` it borrows is freed.
    vm: Option<Vm<'static>>,
    /// Owning pointer to the leaked `Program`. Freed in `Drop`, after `vm`.
    program: *mut Program,
}

/// Parse and compile `src` as a classic script, returning a VM ready to run it.
///
/// `Err` is a compile-time failure (a syntax error, or syntax the compiler does
/// not accept). A runtime throw is reported later, by [`ScriptState::run_init`].
///
/// The goal is SCRIPT, not oxc's module default: module mode would make the
/// whole program strict and silently disable Annex B.3.3 hoisting, sloppy-mode
/// semantics and HTML-comment syntax — all of which real page scripts rely on.
pub fn compile_script(src: &str) -> Result<ScriptState, String> {
    let program = compile_program_source(src)?;
    // Leak, hand the `&'static` to the VM, and keep the raw pointer so `Drop`
    // can reclaim it. See the invariant on `ScriptState`.
    let leaked: &'static mut Program = Box::leak(Box::new(program));
    let program = leaked as *mut Program;
    // SAFETY: `leaked` is a live, uniquely-owned allocation; reborrowing it as
    // shared for the VM is fine because `program` is only dereferenced again in
    // `Drop`, after the VM (the sole holder of the shared borrow) is gone.
    let vm = Vm::new(unsafe { &*program });
    Ok(ScriptState { vm: Some(vm), program })
}

/// Parse + compile, applying the same Annex B call-assignment-target parse
/// retry as [`crate::run`]: in sloppy code `f() = 1` and friends must parse and
/// throw a ReferenceError at runtime, but oxc's AST cannot represent a call as
/// an assignment target and fatal-errors instead. A page script that trips this
/// should behave the same whether it is run through `run` or embedded.
fn compile_program_source(src: &str) -> Result<Program, String> {
    let allocator = oxc_allocator::Allocator::default();
    let ret = oxc_parser::Parser::new(&allocator, src, oxc_span::SourceType::cjs()).parse();
    if !ret.errors.is_empty() {
        if let Some(fixed) = crate::annexb_call_target_rewrite(src) {
            return compile_program_source(&fixed);
        }
        return Err(format!("SyntaxError: {}", ret.errors[0]));
    }
    crate::compile::compile_program(&ret.program, src)
}

impl ScriptState {
    /// Install the host closure backing `__zippHostCall`. Replaces any previous
    /// one. Until this is called the native throws, so a script cannot reach
    /// host state the embedder has not deliberately exposed.
    pub fn set_host_call(&mut self, host: HostCall) {
        if let Some(vm) = self.vm.as_mut() {
            vm.host = Some(host);
        }
    }

    /// Execute the program's top level and drain the job queue.
    ///
    /// `Err` carries the uncaught throw's message. Output produced *before* the
    /// throw is preserved and still readable via [`Self::take_output`] — the
    /// engine flushes what it managed to print, like a real one.
    pub fn run_init(&mut self) -> Result<JsValue, String> {
        let vm = self.vm.as_mut().ok_or("zipp: VM has been torn down")?;
        match vm.run() {
            Ok(v) => Ok(marshal(vm, v)),
            Err(thrown) => Err(thrown.0),
        }
    }

    /// Evaluate `src` in the running script's global context and return its
    /// completion value.
    ///
    /// These are exactly INDIRECT `eval` semantics — what a page gets from
    /// `(0, eval)("…")` — so the behaviour is the engine's ordinary, spec-tested
    /// one rather than something bespoke to embedding:
    ///
    /// - the completion value is the last evaluated expression, so
    ///   `eval_in_context("1 + 1")` is `2`;
    /// - top-level `var` and `function` declarations bind persistent globals,
    ///   visible to every later call and to [`Self::call_global`];
    /// - top-level `let`, `const` and `class` are scoped to this evaluation and
    ///   do NOT persist. That is correct JS, not a limitation of this API; use
    ///   `var`/`function`, or assign to a global, when you need persistence.
    ///
    /// Cost note: each call parses and compiles fresh, and the compiled
    /// functions are interned for the VM's lifetime (the JIT holds raw pointers
    /// into them, so they cannot be freed while the VM lives). Evaluating a
    /// large source string on a hot path — once per mouse-move, say — grows
    /// memory without bound. Prefer defining a function once and calling it
    /// with [`Self::call_global`].
    pub fn eval_in_context(&mut self, src: &str) -> Result<JsValue, String> {
        let vm = self.vm.as_mut().ok_or("zipp: VM has been torn down")?;
        match eval_indirect(vm, src) {
            Ok(v) => Ok(marshal(vm, v)),
            Err(thrown) => Err(thrown.0),
        }
    }

    /// Call the global function `name` with `args`, returning its result.
    ///
    /// The preferred way to re-enter a live script: unlike
    /// [`Self::eval_in_context`] it compiles nothing and splices no values into
    /// source text, so an argument that happens to contain a quote or a
    /// `);` cannot alter the code that runs.
    ///
    /// `name` must be a plain global identifier. Arguments cross as primitives;
    /// pass anything structured as a JSON string and parse it on the JS side.
    pub fn call_global(&mut self, name: &str, args: &[JsValue]) -> Result<JsValue, String> {
        if !is_identifier(name) {
            return Err(format!("zipp: {name:?} is not a global identifier"));
        }
        let vm = self.vm.as_mut().ok_or("zipp: VM has been torn down")?;
        // Resolve the callee by name through the eval pipeline (which knows how
        // to reach both program slots and builtins), then call it directly.
        let callee = eval_indirect(vm, name).map_err(|t| t.0)?;
        let argv: Vec<Value> = args.iter().map(|a| unmarshal(vm, a)).collect();
        match vm.call_value(callee, Value::UNDEFINED, &argv) {
            Ok(v) => Ok(marshal(vm, v)),
            Err(thrown) => Err(thrown.0),
        }
    }

    /// Whether `name` resolves to a callable global — lets a host probe for an
    /// optional entry point without paying for a throw.
    pub fn has_global_function(&mut self, name: &str) -> bool {
        if !is_identifier(name) {
            return false;
        }
        matches!(
            self.eval_in_context(&format!("typeof {name} === \"function\"")),
            Ok(JsValue::Bool(true))
        )
    }

    /// Take the `console.log`/`info`/`debug` lines produced so far, clearing the
    /// buffer. Un-drained output accumulates for the VM's lifetime, so a
    /// long-lived embedder should drain (or discard) periodically.
    pub fn take_output(&mut self) -> Vec<String> {
        self.vm.as_mut().map(|vm| std::mem::take(&mut vm.output)).unwrap_or_default()
    }

    /// Take the `console.error`/`console.warn` lines produced so far.
    pub fn take_errput(&mut self) -> Vec<String> {
        self.vm.as_mut().map(|vm| std::mem::take(&mut vm.errput)).unwrap_or_default()
    }

    // ---- Rich-value API (see `crate::vm::host_api`) -----------------------
    //
    // The methods above address the script by NAME and marshal shallowly, which
    // is the right shape for a host that runs a script and reads a result. A
    // host that keeps live state in sync with the script wants the opposite:
    // stable addressing and structured values. These do that.

    /// The program's top-level bindings, each with a stable slot index. Call
    /// after [`Self::run_init`] so function declarations have been hoisted.
    pub fn symbols(&self) -> Vec<Symbol> {
        self.vm.as_ref().map(|vm| vm.host_symbols()).unwrap_or_default()
    }

    /// Read the global in `index` as a structured value.
    pub fn get_slot(&mut self, index: u32) -> HostValue {
        self.vm.as_mut().map(|vm| vm.host_get_slot(index)).unwrap_or(HostValue::Undefined)
    }

    /// Write the global in `index`. `false` means the write was declined
    /// because the slot holds something that cannot be represented as data (a
    /// function, a class, a `Map`, …) — see [`HostValue::Opaque`].
    pub fn set_slot(&mut self, index: u32, value: &HostValue) -> bool {
        self.vm.as_mut().map(|vm| vm.host_set_slot(index, value)).unwrap_or(false)
    }

    /// Call the function in global slot `index` and drain the microtask queue.
    ///
    /// Prefer this to [`Self::call_global`] on any hot path: `call_global`
    /// resolves its callee by compiling the name as a fresh program, and those
    /// compilations are interned for the VM's lifetime.
    pub fn call_slot(&mut self, index: u32, args: &[HostValue]) -> Result<HostValue, String> {
        match self.vm.as_mut() {
            Some(vm) => vm.host_call_slot(index, args),
            None => Err("zipp: VM has been torn down".into()),
        }
    }

    /// Run pending microtasks without calling anything.
    pub fn pump(&mut self) {
        if let Some(vm) = self.vm.as_mut() {
            vm.host_pump();
        }
    }
}

impl Drop for ScriptState {
    fn drop(&mut self) {
        // Order is the whole point: the VM borrows the `Program`, so it must be
        // destroyed first. A manual `Drop` impl runs before field drops, so
        // dropping the VM here — rather than letting the field drop do it —
        // guarantees the borrow is over before the allocation is freed.
        self.vm = None;
        // SAFETY: `program` came from `Box::leak` in `compile_script`, has not
        // been freed (this runs once), and its only borrower is now gone.
        unsafe { drop(Box::from_raw(self.program)) };
    }
}

/// Run `src` as INDIRECT eval in the global scope — the same call the engine
/// makes for `(0, eval)(src)`, spelled out here because `do_eval` is the
/// general entry point and every argument but two is about DIRECT eval
/// (inheriting a caller's `this`, `super`, private brands, parameter scope).
///
/// The two that matter: `direct = false` (no caller environment to inherit)
/// and `var_env_global = true` (top-level `var`/`function` bind realm globals
/// rather than a throwaway scope, which is what makes declarations persist
/// between calls).
fn eval_indirect(vm: &mut Vm<'static>, src: &str) -> Result<Value, crate::vm::Thrown> {
    vm.do_eval(
        src,
        false,            // force_strict: inherit the source's own directive
        false,            // force_new_target_ok: `new.target` is illegal here
        None,             // this_override: use the realm global
        None,             // inherit_super
        false,            // ban_arguments
        false,            // direct
        Value::UNDEFINED, // caller_new_target
        None,             // caller_home_obj
        true,             // var_env_global
        None,             // param_collisions
        Vec::new(),       // lexical_collisions
        None,             // caller_scope
        None,             // eval_scope_idx
        None,             // exact_src: `src` is well-formed UTF-8 by type
    )
}

/// A plain JS identifier — no dots, no call syntax, no operators. Guards the
/// `name`-taking entry points so a caller cannot smuggle an expression in.
fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_alphabetic() || c == '_' || c == '$' => {}
        _ => return false,
    }
    chars.all(|c| c.is_alphanumeric() || c == '_' || c == '$')
}

/// VM `Value` → embedder [`JsValue`]. Needs `&mut Vm` because `ToString` on an
/// object is observable JS (it can call a user `toString`).
fn marshal(vm: &mut Vm<'static>, v: Value) -> JsValue {
    if v.is_undefined() {
        return JsValue::Undefined;
    }
    if v.is_null() {
        return JsValue::Null;
    }
    if v.is_bool() {
        return JsValue::Bool(v.as_bool());
    }
    if v.is_int() {
        return JsValue::Number(v.as_int() as f64);
    }
    if v.is_double() {
        return JsValue::Number(v.as_f64());
    }
    // Heap value: a string comes across as `String`, anything else as its
    // `ToString`. A `toString` that throws yields `Object("")` rather than
    // propagating — marshalling a result must not manufacture a new throw.
    let is_string = vm.type_of(v) == "string";
    let s = vm.to_js_string(v).map(|s| s.to_string()).unwrap_or_default();
    if is_string {
        JsValue::String(s)
    } else {
        JsValue::Object(s)
    }
}

/// Embedder [`JsValue`] → VM `Value`. `Object(s)` crosses as the string `s`;
/// there is no way to rebuild an arbitrary object from its `ToString`, and
/// inventing one would be worse than being explicit about the limit.
fn unmarshal(vm: &mut Vm<'static>, v: &JsValue) -> Value {
    match v {
        JsValue::Undefined => Value::UNDEFINED,
        JsValue::Null => Value::NULL,
        JsValue::Bool(b) => Value::bool(*b),
        JsValue::Number(n) => Value::num(*n),
        JsValue::String(s) | JsValue::Object(s) => vm.alloc_str(s.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_survives_the_program_and_sees_its_globals() {
        let mut st = compile_script("var counter = 0; function bump() { return ++counter; }")
            .expect("compiles");
        st.run_init().expect("runs");
        // The whole point of the module: the globals are still there afterwards.
        assert_eq!(st.call_global("bump", &[]), Ok(JsValue::Number(1.0)));
        assert_eq!(st.call_global("bump", &[]), Ok(JsValue::Number(2.0)));
        assert_eq!(st.eval_in_context("counter"), Ok(JsValue::Number(2.0)));
    }

    #[test]
    fn eval_declarations_persist_across_calls() {
        let mut st = compile_script("var x = 1;").expect("compiles");
        st.run_init().expect("runs");
        st.eval_in_context("function later() { return x + 41; }").expect("evals");
        // Script-goal GDI, not eval semantics: `later` outlived its eval.
        assert_eq!(st.call_global("later", &[]), Ok(JsValue::Number(42.0)));
    }

    #[test]
    fn marshalling_round_trips_each_type() {
        let mut st = compile_script("function id(v) { return v; }").expect("compiles");
        st.run_init().expect("runs");
        for v in [
            JsValue::Undefined,
            JsValue::Null,
            JsValue::Bool(true),
            JsValue::Number(-1.5),
            JsValue::String("hello".into()),
        ] {
            assert_eq!(st.call_global("id", std::slice::from_ref(&v)), Ok(v));
        }
        // Objects cross outbound as ToString, never as a live reference.
        assert_eq!(st.eval_in_context("({a:1})"), Ok(JsValue::Object("[object Object]".into())));
        assert_eq!(st.eval_in_context("[1,2]"), Ok(JsValue::Object("1,2".into())));
        // …so structured data crosses as JSON.
        assert_eq!(
            st.eval_in_context("JSON.stringify({a:1})"),
            Ok(JsValue::String("{\"a\":1}".into()))
        );
    }

    #[test]
    fn host_call_round_trips_and_can_throw() {
        let mut st = compile_script(
            "function ask(q) { try { return __zippHostCall('echo', q); } \
             catch (e) { return 'caught:' + e.message; } }",
        )
        .expect("compiles");
        st.set_host_call(Box::new(|kind, args| match kind {
            "echo" => Ok(format!("echo:{}", args[0])),
            other => Err(format!("Error: no such host call {other}")),
        }));
        st.run_init().expect("runs");
        assert_eq!(
            st.call_global("ask", &[JsValue::String("hi".into())]),
            Ok(JsValue::String("echo:hi".into()))
        );
        // A host `Err` is an ordinary JS throw the script catches.
        let caught = st.eval_in_context(
            "(function(){ try { __zippHostCall('nope'); return 'unreachable'; } \
             catch (e) { return String(e); } })()",
        );
        assert_eq!(caught, Ok(JsValue::String("Error: no such host call nope".into())));
    }

    #[test]
    fn host_call_without_a_host_throws() {
        let mut st = compile_script("var r = 'unset';").expect("compiles");
        st.run_init().expect("runs");
        // No host installed: the native is inert, so a stock embed cannot be
        // used to reach host state by accident.
        assert!(st.eval_in_context("__zippHostCall('x')").is_err());
    }

    #[test]
    fn errors_are_reported_not_panicked() {
        assert!(compile_script("function (").is_err(), "syntax error is a compile failure");

        let mut st = compile_script("throw new Error('boom');").expect("compiles");
        let err = st.run_init().expect_err("uncaught throw is an Err");
        assert!(err.contains("boom"), "got {err:?}");
        // The VM is still usable after an uncaught top-level throw.
        assert_eq!(st.eval_in_context("1 + 1"), Ok(JsValue::Number(2.0)));

        let mut st = compile_script("var x = 1;").expect("compiles");
        st.run_init().expect("runs");
        assert!(st.call_global("nope; evil()", &[]).is_err(), "non-identifier is rejected");
        assert!(!st.has_global_function("nope"));
        assert!(st.has_global_function("isNaN"));
    }

    /// Resolve a symbol by name, for tests that care about a binding rather
    /// than a slot number.
    fn slot_of(st: &ScriptState, name: &str) -> u32 {
        st.symbols().into_iter().find(|s| s.name == name).unwrap_or_else(|| panic!("no {name}")).index
    }

    #[test]
    fn symbols_report_declarations_not_free_identifiers() {
        let mut st = compile_script(
            "var a = 1; let b = 2; const c = 3; function f() { return Math.max(1, 2); } class K {}",
        )
        .expect("compiles");
        st.run_init().expect("runs");
        let syms = st.symbols();
        let by = |n: &str| syms.iter().find(|s| s.name == n).map(|s| s.scope);
        assert_eq!(by("a"), Some(SymbolScope::Variable));
        assert_eq!(by("b"), Some(SymbolScope::Variable));
        assert_eq!(by("c"), Some(SymbolScope::Variable));
        assert_eq!(by("f"), Some(SymbolScope::Function));
        assert_eq!(by("K"), Some(SymbolScope::Function), "a class is callable state");
        // `Math` is mentioned by the program, so it has a global slot — but it
        // is not a declaration and a host syncing it would be syncing the
        // standard library.
        assert_eq!(by("Math"), None, "free identifiers are not symbols");
        let mut idx: Vec<u32> = syms.iter().map(|s| s.index).collect();
        let n = idx.len();
        idx.dedup();
        assert_eq!(idx.len(), n, "slots are unique");
    }

    #[test]
    fn structured_state_round_trips() {
        let mut st = compile_script(
            "var state = { items: [{ id: 1, tags: ['a', 'b'] }], open: true, note: null };",
        )
        .expect("compiles");
        st.run_init().expect("runs");
        let slot = slot_of(&st, "state");

        let got = st.get_slot(slot);
        let expected = HostValue::Object(vec![
            (
                "items".into(),
                HostValue::Array(vec![HostValue::Object(vec![
                    ("id".into(), HostValue::Number(1.0)),
                    (
                        "tags".into(),
                        HostValue::Array(vec![
                            HostValue::String("a".into()),
                            HostValue::String("b".into()),
                        ]),
                    ),
                ])]),
            ),
            ("open".into(), HostValue::Bool(true)),
            ("note".into(), HostValue::Null),
        ]);
        assert_eq!(got, expected);

        // Write a modified tree back and let the SCRIPT observe it — the real
        // test of `set_slot`, since it proves the rebuilt objects are ordinary
        // engine values and not a parallel representation.
        assert!(st.set_slot(
            slot,
            &HostValue::Object(vec![(
                "items".into(),
                HostValue::Array(vec![
                    HostValue::Object(vec![("id".into(), HostValue::Number(7.0))]),
                    HostValue::Object(vec![("id".into(), HostValue::Number(8.0))]),
                ]),
            )]),
        ));
        assert_eq!(
            st.eval_in_context("state.items.length + ':' + state.items[1].id"),
            Ok(JsValue::String("2:8".into()))
        );
    }

    #[test]
    fn opaque_values_never_cross_and_are_never_clobbered() {
        let mut st = compile_script(
            "function keep() { return 42; } var m = new Map([['k', 1]]); var d = new Date(0);",
        )
        .expect("compiles");
        st.run_init().expect("runs");

        for name in ["keep", "m", "d"] {
            assert_eq!(st.get_slot(slot_of(&st, name)), HostValue::Opaque, "{name} is opaque");
        }
        // A host that read the whole global set and wrote it back must not
        // destroy the function it read as `Opaque`.
        let keep = slot_of(&st, "keep");
        assert!(!st.set_slot(keep, &HostValue::Number(1.0)), "write to a function slot is declined");
        assert!(!st.set_slot(keep, &HostValue::Opaque));
        assert_eq!(st.eval_in_context("keep()"), Ok(JsValue::Number(42.0)));
    }

    #[test]
    fn cycles_and_holes_do_not_hang_the_walk() {
        let mut st = compile_script(
            "var cyc = { name: 'root' }; cyc.self = cyc; var sparse = [1, , 3];",
        )
        .expect("compiles");
        st.run_init().expect("runs");
        // The back-edge becomes Null rather than recursing forever.
        assert_eq!(
            st.get_slot(slot_of(&st, "cyc")),
            HostValue::Object(vec![
                ("name".into(), HostValue::String("root".into())),
                ("self".into(), HostValue::Null),
            ])
        );
        assert_eq!(
            st.get_slot(slot_of(&st, "sparse")),
            HostValue::Array(vec![
                HostValue::Number(1.0),
                HostValue::Undefined,
                HostValue::Number(3.0),
            ])
        );
    }

    #[test]
    fn call_slot_passes_structures_and_sees_mutations() {
        let mut st = compile_script(
            "var log = []; function add(item, n) { for (var i = 0; i < n; i++) log.push(item.id); \
             return { count: log.length, last: item.id }; }",
        )
        .expect("compiles");
        st.run_init().expect("runs");
        let add = slot_of(&st, "add");
        let arg = HostValue::Object(vec![("id".into(), HostValue::String("x".into()))]);
        let got = st.call_slot(add, &[arg, HostValue::Number(2.0)]).expect("calls");
        assert_eq!(
            got,
            HostValue::Object(vec![
                ("count".into(), HostValue::Number(2.0)),
                ("last".into(), HostValue::String("x".into())),
            ])
        );
        // The in-place mutation of a global the call performed is visible.
        assert_eq!(
            st.get_slot(slot_of(&st, "log")),
            HostValue::Array(vec![HostValue::String("x".into()), HostValue::String("x".into())])
        );
        // A throw is an Err, and the VM stays usable afterwards.
        assert!(st.call_slot(add, &[HostValue::Null, HostValue::Number(1.0)]).is_err());
        assert_eq!(st.eval_in_context("1 + 1"), Ok(JsValue::Number(2.0)));
    }

    #[test]
    fn call_slot_drains_microtasks_before_returning() {
        let mut st = compile_script(
            "var done = false; function go() { Promise.resolve().then(function () { done = true; }); }",
        )
        .expect("compiles");
        st.run_init().expect("runs");
        st.call_slot(slot_of(&st, "go"), &[]).expect("calls");
        // Without the drain the continuation would still be queued here.
        assert_eq!(st.get_slot(slot_of(&st, "done")), HostValue::Bool(true));
    }

    #[test]
    fn console_output_is_drainable() {
        let mut st = compile_script("console.log('a'); console.error('b');").expect("compiles");
        st.run_init().expect("runs");
        assert_eq!(st.take_output(), vec!["a".to_string()]);
        assert_eq!(st.take_errput(), vec!["b".to_string()]);
        assert!(st.take_output().is_empty(), "draining clears the buffer");
    }
}
