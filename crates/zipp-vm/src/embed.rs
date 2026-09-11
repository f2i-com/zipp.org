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

pub use crate::vm::host_api::{
    FingerprintBudget, HostCallError, HostValue, HostValueBudget, ResourceUsage, Symbol,
    SymbolScope, DEFAULT_HOST_VALUE_MAX_NODES, DEFAULT_HOST_VALUE_MAX_STRING_BYTES,
};
pub use crate::vm::host_api::{HostCallCtx, HostCtx};
/// The execution-trace row and its opcode contract. Full `instrument` builds
/// expose the `ScriptState::start_trace` control API; the artifact-internal
/// `meter-only` profile deliberately does not.
#[cfg(feature = "instrument")]
pub use crate::vm::instrument::{op, TraceStep};
pub use crate::vm::ConsoleStream;

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
/// `Vm<'p>` borrows its `Program`, which makes the pair self-referential. It
/// used to be expressed by leaking the `Program` to `&'static` and, in builds
/// that allow `unsafe`, reclaiming it in `Drop`; the `safe-sandbox` profile
/// could not reclaim it at all, so every engine a WASM instance created left
/// its compiled program behind for the instance's lifetime (the 11 September
/// 2026 audit's ZIPP-06). [`OwnedVm`] now holds both in one allocation with
/// the borrow expressed safely (`self_cell`): the `Vm` is built over the boxed
/// `Program`, is reachable only through this struct, and is dropped before
/// the `Program` — in every profile, with no `unsafe` in this crate.
///
/// Not `Send`: the VM holds raw pointers into its own heap and register file
/// (the JIT pins them), so a `ScriptState` stays on the thread that built it.
pub struct ScriptState {
    /// `Option` so a torn-down state is expressible; `None` only after an
    /// explicit teardown.
    vm: Option<OwnedVm>,
    /// Key for [`Self::fingerprint_slot`]. See [`Self::set_fingerprint_seed`].
    fp_seed: u64,
}

self_cell::self_cell!(
    /// The compiled program and the VM borrowing it, dropped in that order.
    /// The pair lives in one heap allocation, so moving a `ScriptState` never
    /// moves the `Vm` (native code keeps deriving VM-relative addresses from
    /// the live VM argument regardless).
    struct OwnedVm {
        owner: Box<Program>,
        #[covariant]
        dependent: Vm,
    }
);

/// Parse and compile `src` as a classic script, returning a VM ready to run it.
///
/// `Err` is a compile-time failure (a syntax error, or syntax the compiler does
/// not accept). A runtime throw is reported later, by [`ScriptState::run_init`].
///
/// The goal is SCRIPT, not oxc's module default: module mode would make the
/// whole program strict and silently disable Annex B.3.3 hoisting, sloppy-mode
/// semantics and HTML-comment syntax — all of which real page scripts rely on.
pub fn compile_script(src: &str) -> Result<ScriptState, String> {
    compile_script_with_options(src, &CompileOptions::default())
}

/// Which grammar a script is compiled under. The engine's parser has two
/// script goals, and which one applies used to be decided only by a
/// process-wide switch ([`crate::set_pure_script_goal`]) that every embedder
/// in the process shared (the 11 September 2026 audit's ZIPP-24). An
/// embedder now states its goal per compilation; the process switch remains
/// the default for callers that do not, which keeps the CLI and the
/// conformance runner exactly as they were.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScriptGoal {
    /// Whatever the process-wide switch says: the compatibility grammar
    /// unless [`crate::set_pure_script_goal`] turned the pure goal on.
    #[default]
    Inherit,
    /// The CommonJS-shaped compatibility grammar: sloppy unless directed, and
    /// top-level `return` is legal, as it is in a Node-wrapped file.
    Compat,
    /// The pure ECMAScript Script goal: top-level `return` is a SyntaxError.
    Pure,
}

impl ScriptGoal {
    fn allow_return(self) -> bool {
        match self {
            ScriptGoal::Inherit => !crate::front::pure_script_goal(),
            ScriptGoal::Compat => true,
            ScriptGoal::Pure => false,
        }
    }
}

/// Per-compilation policy for [`compile_script_with_options`] and
/// [`compile_script_with_preamble`]. JavaScript strict mode is not an option
/// here: it is the guest's own decision, made by its directive prologue.
#[derive(Debug, Clone, Copy, Default)]
pub struct CompileOptions {
    pub goal: ScriptGoal,
}

/// [`compile_script`] under an explicit grammar goal.
pub fn compile_script_with_options(
    src: &str,
    options: &CompileOptions,
) -> Result<ScriptState, String> {
    let ast = crate::front::parse_script_with(src, options.goal.allow_return(), false)?;
    let program = crate::compile::compile_main_program(&ast, src)?;
    Ok(ScriptState::from_program(program))
}

/// Compile `preamble` followed by `source` as ONE program — the two share a
/// global environment, so the preamble's bindings are ordinary globals the
/// guest reaches by name and the host reaches by slot — while keeping the
/// guest's own directive prologue in force.
///
/// Plain text concatenation loses that prologue: executable preamble
/// statements precede the guest's `"use strict"`, so it is no longer a
/// directive and an intended strict program silently runs sloppy (undeclared
/// assignments succeed, a strict function's `this` becomes the global object,
/// duplicate-parameter early errors vanish — the 11 September 2026 audit's
/// ZIPP-01). Here the guest's prologue is read by the parser's own
/// directive-prologue production on the guest text alone and the combined
/// program is then parsed under that strictness. The preamble is written to
/// be strict-clean, so it runs the same either way; the guest is exactly as
/// strict as it asked to be, and only then.
///
/// Source positions are those of the concatenation, exactly as before: a
/// host still corrects a compile error's line by the number of preamble
/// lines. A guest hashbang is honoured even though it is no longer the first
/// bytes of the program (it is turned into a line comment of the same
/// length), and a guest BOM is whitespace.
pub fn compile_script_with_preamble(
    preamble: &str,
    source: &str,
    options: &CompileOptions,
) -> Result<ScriptState, String> {
    let allow_return = options.goal.allow_return();
    let strict = crate::front::script_prologue_is_strict(source, allow_return);
    let mut full = String::with_capacity(preamble.len() + 1 + source.len());
    full.push_str(preamble);
    full.push('\n');
    let guest_start = full.len();
    full.push_str(source);
    if source.starts_with("#!") {
        // Same byte length, same line count: positions stay those of the
        // concatenation.
        full.replace_range(guest_start..guest_start + 2, "//");
    }
    let ast = crate::front::parse_script_with(&full, allow_return, strict)?;
    let program = crate::compile::compile_main_program(&ast, &full)?;
    Ok(ScriptState::from_program(program))
}

impl ScriptState {
    /// Take ownership of a compiled program and boot a VM over it.
    fn from_program(program: Program) -> ScriptState {
        let cell = OwnedVm::new(Box::new(program), |program| {
            let mut vm = Vm::new(program);
            // No test262 host object for embedded code. `$262.agent.start()`
            // spawns a detached OS thread running its own VM — outside any
            // budget, abort flag, trace or timeout the embedder set — and
            // `createRealm`/`evalScript`/`detachArrayBuffer` are equally not
            // things a page script or a sandboxed job should reach. This API
            // is the untrusted-code path; it does not get the harness.
            // `zipp js` and the test262 runner still do.
            vm.host_262 = false;
            vm
        });
        ScriptState {
            vm: Some(cell),
            // Unkeyed until a host supplies randomness. Documented on
            // set_fingerprint_seed: a fixed start is solvable.
            fp_seed: 0,
        }
    }

    /// The VM, shared. `None` once torn down.
    fn vm(&self) -> Option<&Vm<'_>> {
        self.vm.as_ref().map(OwnedVm::borrow_dependent)
    }

    /// Run `f` over the VM. `None` once torn down. The closure form is what
    /// keeps the program borrow inside the cell; nothing hands a `&mut Vm`
    /// out, so the pair can never be split.
    fn with_vm<R>(&mut self, f: impl for<'a> FnOnce(&mut Vm<'a>) -> R) -> Option<R> {
        self.vm
            .as_mut()
            .map(|cell| cell.with_dependent_mut(|_, vm| f(vm)))
    }
}

/// The error every entry point reports after teardown.
fn torn_down<T>() -> Result<T, String> {
    Err(TORN_DOWN.into())
}

const TORN_DOWN: &str = "zipp: VM has been torn down";

impl ScriptState {
    /// Install the host closure backing `__zippHostCall`. Replaces any previous
    /// one. Until this is called the native throws, so a script cannot reach
    /// host state the embedder has not deliberately exposed.
    pub fn set_host_call(&mut self, host: HostCall) {
        self.with_vm(|vm| vm.host = Some(host));
    }

    /// Install the context-taking host closure backing `__zippHostCall`. It
    /// is served in preference to [`Self::set_host_call`]'s, and receives the
    /// VM as [`HostCtx`] so it can resolve the guest's typed arrays to memory
    /// regions and call guest functions with numbers while it runs.
    pub fn set_host_call_ctx(&mut self, host: HostCallCtx) {
        self.with_vm(|vm| vm.host_ctx = Some(host));
    }

    /// Enable filesystem module loading, confined to one canonical directory.
    ///
    /// Embedded scripts normally have no module loader at all. A host that
    /// deliberately enables imports should use this method rather than an
    /// unrestricted base directory: every static/dynamic, typed, deferred,
    /// source-phase, and re-export load is canonicalized and must remain under
    /// `root`, so `..` components and symlink escapes are rejected. Each file
    /// is also rejected before parsing when it exceeds `max_module_bytes`.
    #[cfg(all(feature = "instrument", not(feature = "wasm-no-fs-loader")))]
    pub fn set_confined_module_loader(
        &mut self,
        base_dir: &std::path::Path,
        root: &std::path::Path,
        max_module_bytes: u64,
    ) -> Result<(), String> {
        let root = root.to_path_buf();
        self.with_vm(|vm| {
            vm.set_module_root(root, max_module_bytes)?;
            let base = vm.resolve_module_path(base_dir).map_err(|e| e.0)?;
            if !base.is_dir() {
                return Err(format!(
                    "module base '{}' is not a directory",
                    base_dir.display()
                ));
            }
            vm.set_module_base_dir(Some(base));
            Ok(())
        })
        .unwrap_or_else(torn_down)
    }

    /// The dedicated WebAssembly artifact has no filesystem module-loader
    /// authority. Keep the embedding method as a fail-closed stub so feature
    /// unification cannot silently turn a caller's loader request into access.
    #[cfg(all(feature = "instrument", feature = "wasm-no-fs-loader"))]
    pub fn set_confined_module_loader(
        &mut self,
        _base_dir: &std::path::Path,
        _root: &std::path::Path,
        _max_module_bytes: u64,
    ) -> Result<(), String> {
        Err("filesystem module loader is disabled in this build".into())
    }

    /// Disable the VM's native code generators for this script state.
    ///
    /// Sandboxed callers should also disable regress' process-global regex JIT
    /// (the CLI safe runner does so before process startup). This method covers
    /// both whole-function and loop-region VM JITs without allocating a trace.
    #[cfg(feature = "instrument")]
    pub fn disable_vm_jit(&mut self) {
        #[cfg(all(feature = "jit", any(target_arch = "x86_64", target_arch = "aarch64")))]
        self.with_vm(|vm| vm.set_jit_enabled(false));
    }

    /// Execute the program's top level and drain the job queue.
    ///
    /// `Err` carries the uncaught throw's message. Output produced *before* the
    /// throw is preserved and still readable via [`Self::take_output`] — the
    /// engine flushes what it managed to print, like a real one.
    pub fn run_init(&mut self) -> Result<JsValue, String> {
        self.with_vm(|vm| match vm.run() {
            Ok(v) => Ok(marshal(vm, v)),
            Err(thrown) => Err(vm.take_host_throw(thrown)),
        })
        .unwrap_or_else(torn_down)
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
        self.with_vm(|vm| match eval_indirect(vm, src) {
            Ok(v) => Ok(marshal(vm, v)),
            Err(thrown) => Err(vm.take_host_throw(thrown)),
        })
        .unwrap_or_else(torn_down)
    }

    /// Call the global function `name` with `args`, returning its result.
    ///
    /// The preferred way to re-enter a live script by name: unlike
    /// [`Self::eval_in_context`] it compiles nothing and splices no values into
    /// source text, so an argument that happens to contain a quote or a
    /// `);` cannot alter the code that runs. The callee is resolved the way a
    /// bare identifier read is — program and eval-created bindings, the
    /// global object and its prototype chain, then the builtins — on every
    /// call, so a reassigned binding is seen, and the dynamic-compilation
    /// allowance is untouched. (Until the 11 September 2026 audit's ZIPP-03
    /// the name was evaluated as a fresh program per call.)
    ///
    /// `name` must be a plain global identifier. Arguments cross as primitives;
    /// pass anything structured as a JSON string and parse it on the JS side.
    /// A name that does not resolve is a `ReferenceError`; one that resolves
    /// to a non-callable is a `TypeError`; a throw inside the call is that
    /// throw's message.
    ///
    /// Microtasks the call schedules are NOT drained here — see
    /// [`Self::run_microtasks`]; [`Self::call_slot`] drains them itself.
    pub fn call_global(&mut self, name: &str, args: &[JsValue]) -> Result<JsValue, String> {
        if !is_identifier(name) {
            return Err(format!("zipp: {name:?} is not a global identifier"));
        }
        self.with_vm(|vm| {
            let callee = vm.host_resolve_global_by_name(name)?;
            if !vm.is_callable(callee) {
                return Err(format!("TypeError: {name} is not a function"));
            }
            let argv: Vec<Value> = args.iter().map(|a| unmarshal(vm, a)).collect();
            match vm.call_value(callee, Value::UNDEFINED, &argv) {
                Ok(v) => Ok(marshal(vm, v)),
                Err(thrown) => Err(vm.take_host_throw(thrown)),
            }
        })
        .unwrap_or_else(torn_down)
    }

    /// Whether `name` resolves to a callable global — lets a host probe for an
    /// optional entry point without paying for a throw, and without
    /// compiling anything: the same lookup [`Self::call_global`] performs.
    ///
    /// `false` covers every way the probe can miss — an unbound name, a
    /// binding in its temporal dead zone, a non-callable value, a prototype
    /// trap that threw. A resource ceiling crossed by such a trap is reported
    /// through [`Self::resource_limit_error`], not through this bool.
    pub fn has_global_function(&mut self, name: &str) -> bool {
        if !is_identifier(name) {
            return false;
        }
        self.with_vm(|vm| match vm.host_resolve_global_by_name(name) {
            Ok(v) => vm.is_callable(v),
            Err(_) => false,
        })
        .unwrap_or(false)
    }

    /// Run queued microtasks (promise reactions, `queueMicrotask`) to
    /// completion.
    ///
    /// `run_init` drains these when the program finishes, but `call_global` and
    /// `eval_in_context` do NOT: they return as soon as the called function
    /// returns, leaving anything it scheduled sitting in the queue.
    ///
    /// That matters for any host that re-enters a live script. A UI framework
    /// typically applies state updates on a microtask, so a click handler that
    /// calls `setState` has only QUEUED the re-render by the time it returns —
    /// without draining, the update never happens and the interaction silently
    /// does nothing.
    pub fn run_microtasks(&mut self) {
        self.with_vm(|vm| vm.drain_microtasks());
    }

    /// Take the `console.log`/`info`/`debug` lines produced so far, clearing the
    /// buffer. Un-drained output accumulates for the VM's lifetime, so a
    /// long-lived embedder should drain (or discard) periodically.
    pub fn take_output(&mut self) -> Vec<String> {
        self.with_vm(|vm| {
            vm.console_order
                .retain(|stream| *stream == ConsoleStream::Stderr);
            std::mem::take(&mut vm.output)
        })
        .unwrap_or_default()
    }

    /// Take the `console.error`/`console.warn` lines produced so far.
    pub fn take_errput(&mut self) -> Vec<String> {
        self.with_vm(|vm| {
            vm.console_order
                .retain(|stream| *stream == ConsoleStream::Stdout);
            std::mem::take(&mut vm.errput)
        })
        .unwrap_or_default()
    }

    /// Take BOTH console streams in the order the lines were produced, each
    /// tagged with its stream. The separate takes above lose that
    /// chronology by construction (the 11 September 2026 audit's ZIPP-14);
    /// this is the view for a host that shows one combined log. Draining
    /// here empties both buffers; the per-stream takes remain available for
    /// hosts that keep separate consumers.
    pub fn take_console(&mut self) -> Vec<(ConsoleStream, String)> {
        self.with_vm(|vm| {
            let order = std::mem::take(&mut vm.console_order);
            let mut out = std::mem::take(&mut vm.output).into_iter();
            let mut err = std::mem::take(&mut vm.errput).into_iter();
            let mut lines = Vec::with_capacity(order.len());
            for stream in order {
                let line = match stream {
                    ConsoleStream::Stdout => out.next(),
                    ConsoleStream::Stderr => err.next(),
                };
                if let Some(line) = line {
                    lines.push((stream, line));
                }
            }
            // Lines the order vector does not account for (none in practice;
            // a buffer written by a path that bypassed the recorder) still
            // drain.
            lines.extend(out.map(|line| (ConsoleStream::Stdout, line)));
            lines.extend(err.map(|line| (ConsoleStream::Stderr, line)));
            lines
        })
        .unwrap_or_default()
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
        self.vm().map(|vm| vm.host_symbols()).unwrap_or_default()
    }

    /// Read the global in `index` as a structured value.
    pub fn get_slot(&mut self, index: u32) -> HostValue {
        self.try_get_slot(index).unwrap_or(HostValue::Opaque)
    }

    /// Read the global in `index` as a structured value, reporting when its
    /// representation exceeds the host-conversion budget.
    pub fn try_get_slot(&mut self, index: u32) -> Result<HostValue, String> {
        self.with_vm(|vm| vm.host_get_slot(index))
            .unwrap_or_else(torn_down)
    }

    /// Renew the instruction budget without disturbing any other limit.
    ///
    /// The budget exists so a runaway script cannot occupy the host forever.
    /// As a LIFETIME total that also means every long-running embedder dies:
    /// an interactive application is not one computation, it is a few tens of
    /// thousands of small ones, and 50M instructions is minutes of ordinary
    /// use. An emulator reaches it in seconds.
    ///
    /// Renewing per host re-entry keeps the property that matters — no single
    /// re-entry can run unbounded, because this is called BEFORE a call and
    /// never after exhaustion — while letting an application run as long as
    /// its host keeps asking it to. The host decides the cadence; the guest
    /// still cannot raise its own ceiling, which is the actual threat model.
    ///
    /// Deliberately NOT [`Self::set_limits`], which installs a fresh Recorder
    /// and would silently reset `heap_limit` and `output_limit` to unlimited —
    /// they are applied AFTER set_limits during setup, so renewing that way
    /// would quietly drop two ceilings while restoring one.
    ///
    /// Returns false when the budget is already spent: exhaustion is sticky by
    /// design, and a spent engine must stay spent.
    pub fn renew_step_budget(&mut self, max_steps: u64) -> bool {
        self.with_vm(|vm| vm.renew_step_budget(max_steps))
            .unwrap_or(false)
    }

    /// Key the global fingerprints for this engine.
    ///
    /// The digest mixer is a chain of bijections, so an attacker who knows the
    /// starting value can INVERT it and solve for a value landing on any
    /// chosen digest — a constructed collision, not an unlikely one. A host
    /// that skips reads on matching digests would then mirror stale state
    /// while the guest moved on, and a bundle could show one number and act on
    /// another.
    ///
    /// A per-engine key the guest cannot observe removes the ability to solve:
    /// digests are only ever compared against earlier digests from the same
    /// engine, so the key never has to be stable or known to anyone.
    ///
    /// Supply real randomness. Without this the seed is a fixed constant and
    /// the digests are collision-resistant only by accident.
    pub fn set_fingerprint_seed(&mut self, seed: u64) {
        self.fp_seed = seed;
    }

    /// Fingerprint the global in `index` — see
    /// [`Vm::host_fingerprint_slot`]. `None` means "treat it as changed".
    /// Walks under a fresh default [`FingerprintBudget`]; a host digesting
    /// several slots at once should share one through
    /// [`Self::fingerprint_slot_bounded`].
    pub fn fingerprint_slot(&mut self, index: u32) -> Option<u64> {
        let mut budget = FingerprintBudget::default();
        self.fingerprint_slot_bounded(index, &mut budget)
    }

    /// [`Self::fingerprint_slot`] charging the caller's budget, which is what
    /// bounds a batch as a whole: every slot in the batch, duplicates
    /// included, draws on the same allowance, exactly as the batched read
    /// does. The budget's counters report the work done.
    pub fn fingerprint_slot_bounded(
        &mut self,
        index: u32,
        budget: &mut FingerprintBudget,
    ) -> Option<u64> {
        let seed = self.fp_seed;
        self.with_vm(|vm| vm.host_fingerprint_slot(index, seed, budget))?
    }

    /// Write the global in `index`. `false` means the write was declined
    /// because the slot holds something that cannot be represented as data (a
    /// function, a class, a `Map`, …) — see [`HostValue::Opaque`].
    pub fn set_slot(&mut self, index: u32, value: &HostValue) -> bool {
        self.with_vm(|vm| vm.host_set_slot(index, value))
            .unwrap_or(false)
    }

    /// Call the function in global slot `index` and drain the microtask queue.
    ///
    /// Prefer this to [`Self::call_global`] on a hot path: a slot is a direct
    /// index, while a name is looked up through the global environment on
    /// every call (no compilation either way).
    pub fn call_slot(&mut self, index: u32, args: &[HostValue]) -> Result<HostValue, String> {
        self.with_vm(|vm| vm.host_call_slot(index, args))
            .unwrap_or_else(torn_down)
    }

    /// [`Self::call_slot`] with the caller's own conversion budget for the
    /// result and a typed error, so a host staging a transfer across a
    /// boundary can tell "the guest threw" from "the result is too large for
    /// what I can accept" and retry with less. The budget is charged as the
    /// result is walked; clone it first when a failed attempt must not count.
    pub fn call_slot_bounded(
        &mut self,
        index: u32,
        args: &[HostValue],
        budget: &mut HostValueBudget,
    ) -> Result<HostValue, HostCallError> {
        self.with_vm(|vm| vm.host_call_slot_bounded(index, args, budget))
            .unwrap_or_else(|| Err(HostCallError::Thrown(TORN_DOWN.into())))
    }

    /// Cap the approximate resident heap a script may reach.
    ///
    /// Enforced against the same figure [`Self::heap_bytes`] reports, checked
    /// on the schedule the abort flag uses — every few thousand instructions in
    /// the interpreter, and once per entry into compiled code, which is the
    /// only place native execution can be stopped from. A script therefore
    /// overshoots the ceiling by a bounded amount before it is stopped; set the
    /// limit with that headroom in mind rather than at the exact figure a host
    /// cannot afford.
    ///
    /// Exceeding it raises a catchable `RangeError`, like the step budget. Call
    /// after [`Self::set_limits`], which is what allocates the recorder this
    /// lives in; without it this is a no-op. `usize::MAX` means unlimited.
    #[cfg(feature = "instrument")]
    pub fn set_heap_limit(&mut self, bytes: usize) {
        self.with_vm(|vm| {
            if let Some(rec) = vm.instr_rec.as_mut() {
                rec.heap_limit = bytes;
            }
            // Let the slot table refuse to double past the ceiling too.
            vm.set_resident_ceiling(bytes);
        });
    }

    /// Cap combined buffered console output, counted as UTF-8 plus one newline
    /// per line. Requires [`Self::set_limits`] first; without it this is a no-op.
    #[cfg(feature = "instrument")]
    pub fn set_output_limit(&mut self, bytes: usize) {
        self.with_vm(|vm| {
            if let Some(rec) = vm.instr_rec.as_mut() {
                rec.output_limit = bytes;
                rec.output_used = 0;
                rec.output_exhausted = false;
            }
        });
    }

    /// Payload-aware resident heap estimate, in bytes.
    ///
    /// This includes retained capacities for the object table, strings, arrays,
    /// ArrayBuffers, property maps, collections, suspended activations, GC side
    /// tables, and Arc-deduplicated compiled regular-expression programs. It
    /// remains an estimate: allocator metadata and opaque dependency internals
    /// are not all introspectable. Hosts requiring a hard memory boundary must
    /// additionally cap a worker process/container or WebAssembly linear memory.
    pub fn heap_bytes(&self) -> usize {
        self.vm().map_or(0, |vm| vm.heap_bytes())
    }

    /// Run pending microtasks without calling anything.
    pub fn pump(&mut self) {
        self.with_vm(|vm| vm.host_pump());
    }

    /// Evaluate `src` in the script's global context and marshal its
    /// completion value as a structured [`HostValue`] — the rich-value
    /// counterpart of [`Self::eval_in_context`]'s shallow `ToString`
    /// marshalling, with a slot read's cycle, depth, opaque and budget rules
    /// (nothing is stringified; a cycle reads as `Null`, a function as
    /// `Opaque`). Declarations persist exactly as with `eval_in_context`, and
    /// microtasks are drained afterwards as [`Self::call_slot`] drains them.
    /// The same cost note applies: each call compiles and retains a program.
    pub fn eval_in_context_rich(
        &mut self,
        src: &str,
        budget: &mut HostValueBudget,
    ) -> Result<HostValue, HostCallError> {
        self.with_vm(|vm| vm.host_eval_rich(src, budget))
            .unwrap_or_else(|| Err(HostCallError::Thrown(TORN_DOWN.into())))
    }

    /// What this VM currently retains and has spent — see [`ResourceUsage`].
    /// Cheap (no walk), so a host may read it between every re-entry.
    pub fn resource_usage(&self) -> ResourceUsage {
        self.vm()
            .map(|vm| vm.host_resource_usage())
            .unwrap_or_default()
    }

    // ---- Untrusted-code controls (`instrument` feature) -------------------
    //
    // A host running code it did not write needs to bound it and, for some
    // hosts, to record what it did. See `crate::vm::instrument`.

    /// Bound this VM: at most `max_steps` bytecode instructions, and stop early
    /// when `abort` becomes true.
    ///
    /// Exceeding either surfaces as an uncaught `RangeError` — an `Err` from
    /// [`Self::run_init`] / [`Self::eval_in_context`] / [`Self::call_slot`] —
    /// which the script cannot `catch` its way past.
    ///
    /// The JIT stays on. Compiled code charges the budget itself, once per
    /// basic block, by that block's exact instruction count — the same number
    /// the interpreter would have counted, so the limit means one thing whether
    /// or not the code got hot enough to compile.
    ///
    /// Call this before [`Self::run_init`]. Calling it again replaces the limits
    /// and discards everything compiled so far, since code emitted for a
    /// different budget carries the wrong charge.
    ///
    /// `abort` is polled every few thousand instructions rather than every one:
    /// an atomic load per instruction is a real cost for a flag that is almost
    /// never set.
    ///
    /// The artifact-internal `meter-only` profile is the exception: it requires
    /// `abort` to be `None` and omits cooperative polling. Its zipp-wasm host
    /// enforces the wall-clock deadline by terminating the surrounding Worker.
    ///
    /// Note what this does NOT generally bound: a single native builtin. A
    /// pathological `JSON.parse` is ONE instruction, so wall-clock safety still
    /// needs the abort flag driven by a timer on another thread. The
    /// `safe-sandbox` profile additionally meters classical regex work and caps
    /// its transient backtrack stack; ordinary `instrument` builds retain the
    /// unmetered regex engine for compatibility and speed.
    #[cfg(feature = "instrument")]
    pub fn set_limits(
        &mut self,
        max_steps: u64,
        abort: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    ) {
        #[cfg(feature = "meter-only")]
        debug_assert!(
            abort.is_none(),
            "meter-only relies on Worker termination and does not poll an abort flag"
        );
        self.with_vm(|vm| {
            let mut rec = crate::vm::instrument::Recorder::new();
            rec.set_step_limit(max_steps);
            rec.abort = abort;
            vm.set_instrumentation(rec);
        });
    }

    /// Bound runtime compilation for this script state.
    ///
    /// The source/call gate covers every path through the VM's common dynamic
    /// compiler: direct/indirect `eval`, `Function` constructors,
    /// `ShadowRealm`, and [`Self::eval_in_context`]. Source bytes and call
    /// attempts are charged before parsing, including failed parses. The
    /// function/class limits cap concrete stable-address definitions retained
    /// by successful dynamic compilations or confined modules, and are checked
    /// before those definitions are installed.
    ///
    /// Requires [`Self::set_limits`] first; without an instrumentation recorder
    /// this is a no-op. Limits are lifetime totals for the recorder and a limit
    /// violation is reported through [`Self::resource_limit_error`]. Stable
    /// function/class definitions are not broadly reclaimed when a
    /// `ScriptState` is dropped, so these caps bound one state's contribution;
    /// a multi-tenant host should recycle its process/WASM instance to reclaim
    /// them between tenants.
    #[cfg(feature = "instrument")]
    pub fn set_dynamic_code_limits(
        &mut self,
        per_source_bytes: usize,
        lifetime_source_bytes: usize,
        calls: usize,
        functions: usize,
        classes: usize,
    ) {
        self.with_vm(|vm| {
            vm.set_dynamic_code_limits(
                per_source_bytes,
                lifetime_source_bytes,
                calls,
                functions,
                classes,
            );
        });
    }

    /// Start recording an execution trace, stopping at `max_steps` rows.
    ///
    /// Requires [`Self::set_limits`] first (that is what attaches the recorder);
    /// without it this is a no-op.
    ///
    /// **This switches the JIT off for the VM's lifetime.** Unlike the budget,
    /// which compiled code can charge itself, a trace has to be a complete
    /// row-per-instruction record — native code produces no rows, so a JIT'd hot
    /// loop would be missing from the trace entirely while the program still
    /// returned the right answer.
    ///
    /// Recording costs roughly 64 bytes and a few register reads per
    /// instruction, so switch it on immediately before the code you want to
    /// prove and take it back with [`Self::finish_trace`] straight after.
    #[cfg(all(feature = "instrument", not(feature = "meter-only")))]
    pub fn start_trace(&mut self, max_steps: usize) {
        self.with_vm(|vm| {
            if vm.instr_rec.is_none() {
                return;
            }
            // A trace must be a complete record, and native code produces no
            // rows.
            vm.enter_trace_mode();
            if let Some(rec) = vm.instr_rec.as_mut() {
                rec.start_trace(max_steps);
            }
        });
    }

    /// Stop recording and take the trace, appending a terminal halt row that
    /// carries `result`.
    ///
    /// `None` means the trace is not usable for proving — it hit the row cap, or
    /// is too short — and the caller should fall back to whatever unproven
    /// receipt it offers. It is never a PARTIAL trace: a truncated recording is
    /// discarded rather than returned, because a trace missing its tail attests
    /// to an execution that did not happen.
    #[cfg(all(feature = "instrument", not(feature = "meter-only")))]
    pub fn finish_trace(&mut self, result: u64) -> Option<Vec<TraceStep>> {
        self.with_vm(|vm| vm.instr_rec.as_mut().and_then(|rec| rec.finish(result)))
            .flatten()
    }

    /// Whether the last recording stopped early at the row cap.
    #[cfg(all(feature = "instrument", not(feature = "meter-only")))]
    pub fn trace_truncated(&self) -> bool {
        self.vm()
            .and_then(|vm| vm.instr_rec.as_ref())
            .is_some_and(|r| r.truncated())
    }

    /// Instructions still available under [`Self::set_limits`]; `u64::MAX` when
    /// unlimited or uninstrumented.
    ///
    /// Includes any chunk currently on loan to the native tier, so the figure is
    /// the same whether or not the JIT happened to be running.
    #[cfg(feature = "instrument")]
    pub fn steps_remaining(&self) -> u64 {
        let Some(vm) = self.vm() else {
            return u64::MAX;
        };
        let Some(rec) = vm.instr_rec.as_ref() else {
            return u64::MAX;
        };
        let Some(left) = rec.finite_remaining() else {
            return u64::MAX;
        };
        (left as i64 + vm.jit_steps.max(0)).max(0) as u64
    }

    /// Instructions actually executed since [`Self::set_limits`] attached the
    /// recorder — the consumed half of [`Self::steps_remaining`], and 0 before
    /// limits are set.
    ///
    /// This is the figure a host bills on (blockchain gas metering): what the
    /// script DID, in the same unit `max_steps` caps, counted identically
    /// whether the work ran in the interpreter, was charged per basic block by
    /// compiled code, or ran in an off-loop kernel. With a finite budget,
    /// `steps_used() + steps_remaining() == max_steps` at every observational
    /// point — so a script stopped by its budget reports exactly the cap.
    #[cfg(feature = "instrument")]
    pub fn steps_used(&self) -> u64 {
        let Some(vm) = self.vm() else { return 0 };
        let Some(rec) = vm.instr_rec.as_ref() else {
            return 0;
        };
        rec.steps_used()
    }

    /// Return the recorder's typed resource-exhaustion state, if any.
    ///
    /// This deliberately does not inspect a guest exception's text: a script is
    /// allowed to throw the same string as a budget error without impersonating
    /// the recorder. Embedders should check this after each guest entry because
    /// promise/microtask machinery can turn an execution failure into a rejected
    /// promise instead of returning it directly from the original call.
    #[cfg(feature = "instrument")]
    pub fn resource_limit_error(&mut self) -> Option<&'static str> {
        self.with_vm(|vm| vm.instrument_resource_limit_error())?
    }

    /// How many host completion values the VM is holding rooted right now —
    /// always zero between calls. A test hook for the ZA-05 invariant that
    /// every host entry releases its root on every exit; not an API.
    #[doc(hidden)]
    pub fn host_result_roots_for_test(&self) -> usize {
        self.vm().map(Vm::host_result_roots_len).unwrap_or(0)
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
fn eval_indirect(vm: &mut Vm<'_>, src: &str) -> Result<Value, crate::vm::Thrown> {
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
fn marshal(vm: &mut Vm<'_>, v: Value) -> JsValue {
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
    let s = vm
        .to_js_string(v)
        .map(|s| s.to_string())
        .unwrap_or_default();
    if is_string {
        JsValue::String(s)
    } else {
        JsValue::Object(s)
    }
}

/// Embedder [`JsValue`] → VM `Value`. `Object(s)` crosses as the string `s`;
/// there is no way to rebuild an arbitrary object from its `ToString`, and
/// inventing one would be worse than being explicit about the limit.
fn unmarshal(vm: &mut Vm<'_>, v: &JsValue) -> Value {
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

    /// Persistent native code receives the live VM on every entry. Epoch
    /// guards must derive their address from that argument rather than baking
    /// a pointer into the movable `ScriptState` allocation.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    #[test]
    fn moving_hot_state_keeps_route_and_class_epoch_guards_live() {
        let mut first = Vec::with_capacity(1);
        first.push(
            compile_script(
                r#"
                routeState = 1;
                var routeSeen = 0;
                function routeRandom() {
                    routeState = (routeState + 1) | 0;
                    return routeState;
                }
                var routeObj = { random: routeRandom };
                function invokeRoute() { return routeObj.random(); }
                function warmRoute(n) {
                    var out = 0;
                    for (var i = 0; i < n; i++) out = invokeRoute();
                    return out;
                }

                leafState = 1;
                var leafSeen = 0, leafLast = 0;
                function leafStep(x) {
                    leafState = (leafState + x) | 0;
                    return leafState;
                }
                function warmLeaf(n) {
                    var out = 0, kind = "";
                    for (var i = 0; i < n; i++) {
                        kind = (i & 1) ? "odd" : "even";
                        out = leafStep((i & 3) + 1);
                    }
                    return kind === "never" ? -1 : out;
                }

                intLeafState = 1;
                var intLeafSeen = 0, intLeafLast = 0;
                function intLeafStep(x) {
                    intLeafState = (intLeafState + x) | 0;
                }
                function warmIntLeaf(n) {
                    for (var i = 0; i < n; i++) intLeafStep(1);
                    return intLeafState;
                }

                function make(k) {
                    class A {
                        constructor(v) { this._v = v | 0; }
                        area() { return this._v + k; }
                    }
                    class B extends A {
                        area() { return super.area() * 3 + 1; }
                    }
                    return new B(11);
                }
                var superObj = make(1);
                function invokeSuper() { return superObj.area(); }
                function warmSuper(n) {
                    var out = 0;
                    for (var i = 0; i < n; i++) out = invokeSuper();
                    return out;
                }
                function retargetSuper() { make(50); return invokeSuper(); }
            "#,
            )
            .expect("compiles"),
        );
        let st = &mut first[0];
        st.run_init().expect("initializes");
        assert_eq!(
            st.call_global("warmRoute", &[JsValue::Number(5_000.0)]),
            Ok(JsValue::Number(5_001.0))
        );
        assert_eq!(
            st.call_global("warmLeaf", &[JsValue::Number(5_000.0)]),
            Ok(JsValue::Number(12_501.0))
        );
        assert_eq!(
            st.call_global("warmIntLeaf", &[JsValue::Number(5_000.0)]),
            Ok(JsValue::Number(5_001.0))
        );
        assert_eq!(
            st.call_global("warmSuper", &[JsValue::Number(60_000.0)]),
            Ok(JsValue::Number(37.0))
        );

        // Move the state into a separately allocated Vec. The VM itself lives
        // in the self-owned cell's allocation, so it does NOT move with its
        // `ScriptState` (B306 stage two); the guards below are exercised by
        // the live route/epoch changes regardless, and native code keeps
        // deriving VM-relative addresses from the live VM argument.
        let before = first[0].vm().unwrap() as *const Vm<'_> as usize;
        let mut second = Vec::with_capacity(1);
        second.push(first.pop().unwrap());
        let after = second[0].vm().unwrap() as *const Vm<'_> as usize;
        assert_eq!(
            before, after,
            "the VM's address is stable across a state move"
        );
        let st = &mut second[0];

        st.eval_in_context(
            r#"Object.defineProperty(globalThis, "routeState", {
                   configurable: true,
                   get: function () { return 40; },
                   set: function (v) { routeSeen = v; }
               });
               Object.defineProperty(globalThis, "leafState", {
                   configurable: true,
                   get: function () { return 40; },
                   set: function (v) {
                       leafSeen = (leafSeen + 1) | 0;
                       leafLast = v;
                   }
               });
               Object.defineProperty(globalThis, "intLeafState", {
                   configurable: true,
                   get: function () { return 50; },
                   set: function (v) {
                       intLeafSeen = (intLeafSeen + 1) | 0;
                       intLeafLast = v;
                   }
               })"#,
        )
        .expect("installs a live global-object route");
        assert_eq!(
            st.call_global("invokeRoute", &[]),
            Ok(JsValue::Number(40.0))
        );
        assert_eq!(
            st.eval_in_context("routeSeen"),
            Ok(JsValue::Number(41.0)),
            "the moved VM must take the setter route, not a stale raw-slot lane"
        );
        assert_eq!(
            st.call_global("warmLeaf", &[JsValue::Number(8.0)]),
            Ok(JsValue::Number(40.0)),
            "the generic MEM leaf must fall back after its global route changes"
        );
        assert_eq!(
            st.eval_in_context("leafSeen + ',' + leafLast"),
            Ok(JsValue::String("8,44".into())),
            "the moved generic leaf must invoke the live setter exactly once per call"
        );
        assert_eq!(
            st.call_global("warmIntLeaf", &[JsValue::Number(8.0)]),
            Ok(JsValue::Number(50.0)),
            "the INT splice must entry-bail after its global route changes"
        );
        assert_eq!(
            st.eval_in_context("intLeafSeen + ',' + intLeafLast"),
            Ok(JsValue::String("8,51".into())),
            "the moved INT splice must invoke the live setter exactly once per call"
        );
        st.eval_in_context("delete globalThis.leafState; delete globalThis.intLeafState;")
            .expect("removes both live descriptor routes");
        let leaf_err = st
            .call_global("warmLeaf", &[JsValue::Number(4.0)])
            .expect_err("deleting the dynamic global must make its binding absent");
        assert!(
            leaf_err.contains("ReferenceError: leafState is not defined"),
            "generic leaf resumed a stale raw slot after delete: {leaf_err}"
        );
        let int_leaf_err = st
            .call_global("warmIntLeaf", &[JsValue::Number(4.0)])
            .expect_err("deleting the dynamic global must make its binding absent");
        assert!(
            int_leaf_err.contains("ReferenceError: intLeafState is not defined"),
            "INT leaf resumed a stale raw slot after delete: {int_leaf_err}"
        );
        assert_eq!(
            st.eval_in_context("leafSeen + ',' + intLeafSeen"),
            Ok(JsValue::String("8,8".into())),
            "deleted setters must not observe the failed real-call writes"
        );

        // Re-running the factory changes the live class epoch. Zipp's existing
        // per-class-id semantics retarget the first instance too; the important
        // invariant here is that moved native code observes the new epoch and
        // agrees with the interpreter instead of using the stale super chain.
        assert_eq!(
            st.call_global("retargetSuper", &[]),
            Ok(JsValue::Number(184.0))
        );
    }

    /// The name path resolves like a bare identifier read: program slots,
    /// eval-created globals, global-object properties, builtins — reading the
    /// CURRENT binding each time, with TDZ and non-callables reported.
    #[test]
    fn call_global_resolves_every_binding_kind_without_compiling() {
        let mut st = compile_script(
            "var count = 0; function bump() { return ++count; } let lexical = function () { return 'lex'; }; \
             globalThis.own = function () { return 'own'; }; \
             Object.setPrototypeOf(globalThis, { inherited: function () { return 'proto'; } }); \
             function thrower() { throw new Error('boom'); }",
        )
        .expect("compiles");
        st.run_init().expect("runs");
        assert_eq!(st.call_global("bump", &[]), Ok(JsValue::Number(1.0)));
        assert_eq!(
            st.call_global("lexical", &[]),
            Ok(JsValue::String("lex".into()))
        );
        assert_eq!(
            st.call_global("own", &[]),
            Ok(JsValue::String("own".into()))
        );
        assert_eq!(
            st.call_global("inherited", &[]),
            Ok(JsValue::String("proto".into()))
        );
        assert_eq!(
            st.call_global("isNaN", &[JsValue::Number(f64::NAN)]),
            Ok(JsValue::Bool(true))
        );
        assert!(st.has_global_function("bump"));
        assert!(st.has_global_function("lexical"));
        assert!(st.has_global_function("own"));
        assert!(st.has_global_function("inherited"));
        assert!(st.has_global_function("isNaN"));
        assert!(!st.has_global_function("count"), "a number is not callable");
        assert!(!st.has_global_function("absent"));
        assert!(!st.has_global_function("not an identifier"));

        // A reassignment is seen on the next call: nothing caches the value.
        st.eval_in_context("bump = function () { return 99; }; count = 5;")
            .expect("reassigns");
        assert_eq!(st.call_global("bump", &[]), Ok(JsValue::Number(99.0)));
        st.eval_in_context("var later = function () { return 'later'; };")
            .expect("declares through eval");
        assert_eq!(
            st.call_global("later", &[]),
            Ok(JsValue::String("later".into()))
        );

        let missing = st.call_global("absent", &[]).expect_err("unbound name");
        assert_eq!(missing, "ReferenceError: absent is not defined");
        let not_callable = st.call_global("count", &[]).expect_err("a number");
        assert_eq!(not_callable, "TypeError: count is not a function");
        let thrown = st
            .call_global("thrower", &[])
            .expect_err("the callee throws");
        assert!(thrown.contains("boom"), "{thrown}");
        // Still usable afterwards.
        assert_eq!(st.call_global("bump", &[]), Ok(JsValue::Number(99.0)));

        // A deleted builtin is gone by name too.
        st.eval_in_context("delete globalThis.isNaN")
            .expect("deletes");
        assert!(!st.has_global_function("isNaN"));
    }

    /// Two embedders in one process can hold different grammar goals at the
    /// same time; neither the other's option nor the process switch leaks.
    #[test]
    fn grammar_goal_is_per_compilation() {
        let pure = CompileOptions {
            goal: ScriptGoal::Pure,
        };
        let compat = CompileOptions {
            goal: ScriptGoal::Compat,
        };
        let top_level_return = "var x = 1; return x;";
        assert!(compile_script_with_options(top_level_return, &pure).is_err());
        assert!(compile_script_with_options(top_level_return, &compat).is_ok());
        // Interleaved, and again after a pure compile, the compat one still
        // accepts what the pure one refused.
        assert!(compile_script_with_options(top_level_return, &pure).is_err());
        assert!(compile_script_with_options(top_level_return, &compat).is_ok());
        assert!(
            compile_script_with_preamble("var preamble = 1;", top_level_return, &pure).is_err()
        );
        assert!(
            compile_script_with_preamble("var preamble = 1;", top_level_return, &compat).is_ok()
        );
        // Strictness stays the guest's own decision under either goal.
        for options in [&pure, &compat] {
            let err = compile_script_with_preamble(
                "var preamble = 1;",
                "\"use strict\"; function f(a, a) {}",
                options,
            )
            .err()
            .expect("duplicate parameters are a strict early error");
            assert!(err.contains("SyntaxError"), "{err}");
        }
    }

    #[test]
    fn rich_eval_marshals_structures_and_reports_usage() {
        let mut st = compile_script("var log = []; function f() { return 1; }").expect("compiles");
        st.run_init().expect("runs");
        let before = st.resource_usage();
        assert_eq!(before.retained_functions, 0);
        let mut budget = HostValueBudget::default();
        let got = st
            .eval_in_context_rich(
                "var later = function () { return 2; }; \
                 Promise.resolve().then(function () { log.push('ran'); }); \
                 ({ a: [1, -0, NaN], f: f, s: 'x' })",
                &mut budget,
            )
            .expect("evaluates");
        // -0 and NaN cross as themselves, a function as Opaque (NaN is not
        // equal to itself, so the array is checked element-wise).
        let HostValue::Object(pairs) = got else {
            panic!("object expected")
        };
        assert_eq!(pairs.len(), 3);
        assert_eq!(pairs[0].0, "a");
        let HostValue::Array(items) = &pairs[0].1 else {
            panic!("array expected")
        };
        assert!(matches!(items[0], HostValue::Number(n) if n == 1.0));
        assert!(matches!(items[1], HostValue::Number(n) if n == 0.0 && n.is_sign_negative()));
        assert!(matches!(items[2], HostValue::Number(n) if n.is_nan()));
        assert_eq!(pairs[1], ("f".to_string(), HostValue::Opaque));
        assert_eq!(pairs[2], ("s".to_string(), HostValue::String("x".into())));
        // Declarations persist and microtasks were drained.
        assert_eq!(st.call_global("later", &[]), Ok(JsValue::Number(2.0)));
        assert_eq!(st.eval_in_context("log.length"), Ok(JsValue::Number(1.0)));
        assert_eq!(
            st.eval_in_context_rich("cyc = {}; cyc.self = cyc; cyc", &mut budget)
                .expect("evaluates"),
            HostValue::Object(vec![("self".into(), HostValue::Null)])
        );
        let err = st
            .eval_in_context_rich("(function () { throw new Error('boom'); })()", &mut budget)
            .expect_err("a throw is a throw");
        assert!(
            matches!(err, HostCallError::Thrown(ref m) if m.contains("boom")),
            "{err:?}"
        );
        let mut tiny = HostValueBudget::new(2, 1 << 20);
        let err = st
            .eval_in_context_rich("[1, 2, 3, 4]", &mut tiny)
            .expect_err("over budget");
        assert!(matches!(err, HostCallError::Conversion(_)), "{err:?}");

        let after = st.resource_usage();
        assert!(
            after.retained_functions > before.retained_functions,
            "{after:?}"
        );
        assert!(after.heap_bytes > 0);
        assert_eq!(after.console_lines_buffered, 0);
    }

    /// A string holding a lone surrogate crosses as its exact code units in
    /// both directions, digests differently from its U+FFFD rendering, and
    /// well-formed text keeps the `String` form.
    #[test]
    fn strings_with_lone_surrogates_cross_exactly() {
        let mut st = compile_script(
            "var lone = '\\ud800'; var pair = '\\ud83d\\ude00'; var mixed = 'a\\u0000b\\udc00c'; \
             var lengths = null; function measure(v) { lengths = [v.length, isNaN(v.charCodeAt(1)), v === lone]; }",
        )
        .expect("compiles");
        st.run_init().expect("runs");
        let lone = slot_of(&st, "lone");
        assert_eq!(st.get_slot(lone), HostValue::Utf16(vec![0xD800]));
        assert_eq!(
            st.get_slot(slot_of(&st, "pair")),
            HostValue::String("\u{1F600}".into()),
            "a valid pair is ordinary text"
        );
        assert_eq!(
            st.get_slot(slot_of(&st, "mixed")),
            HostValue::Utf16(vec![0x61, 0, 0x62, 0xDC00, 0x63])
        );
        // Inbound: the exact units are recreated, and identity holds.
        let measure = slot_of(&st, "measure");
        st.call_slot(measure, &[HostValue::Utf16(vec![0xD800])])
            .expect("calls");
        assert_eq!(
            st.get_slot(slot_of(&st, "lengths")),
            HostValue::Array(vec![
                HostValue::Number(1.0),
                HostValue::Bool(true),
                HostValue::Bool(true)
            ]),
            "one unit, no second unit, identical to the guest's own"
        );
        // The digest tells a lone surrogate from its replacement character.
        let a = st.fingerprint_slot(lone);
        assert!(st.set_slot(lone, &HostValue::String("\u{FFFD}".into())));
        let b = st.fingerprint_slot(lone);
        assert_ne!(a, b);
    }

    #[test]
    fn eval_declarations_persist_across_calls() {
        let mut st = compile_script("var x = 1;").expect("compiles");
        st.run_init().expect("runs");
        st.eval_in_context("function later() { return x + 41; }")
            .expect("evals");
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
        assert_eq!(
            st.eval_in_context("({a:1})"),
            Ok(JsValue::Object("[object Object]".into()))
        );
        assert_eq!(
            st.eval_in_context("[1,2]"),
            Ok(JsValue::Object("1,2".into()))
        );
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
        assert_eq!(
            caught,
            Ok(JsValue::String("Error: no such host call nope".into()))
        );
    }

    // Reads the pinned region through its raw address, which the
    // `safe-sandbox` profile forbids (`forbid(unsafe_code)`); the region
    // contract is a native-host feature, so the test stays with that build.
    #[cfg(not(feature = "safe-sandbox"))]
    #[test]
    fn host_context_resolves_regions_and_calls_back() {
        let mut st = compile_script(
            "var A = new Int32Array(4); A[2] = 7; \
             function add(a, b) { return a + b; } \
             function go() { return __zippHostCall('probe', 'x'); }",
        )
        .unwrap();
        st.run_init().unwrap();
        st.set_host_call_ctx(Box::new(|ctx, kind, args| {
            assert_eq!(kind, "probe");
            assert_eq!(args, ["x"]);
            let (ptr, len, kind) = ctx.typed_array_region("A")?;
            assert!(ptr != 0);
            assert_eq!((len, kind), (4, 5));
            // The region is the array's own bytes.
            let third = unsafe { *((ptr + 8) as *const i32) };
            assert_eq!(third, 7);
            assert!(ctx.typed_array_region("add").is_err());
            assert!(ctx.typed_array_region("nothing").is_err());
            let r = ctx.call_global_numbers("add", &[2.0, 3.0])?;
            Ok(format!("{r}"))
        }));
        assert_eq!(st.call_global("go", &[]).unwrap().as_str(), Some("5"));
        // Pinned: the buffer refuses to be transferred or resized.
        assert!(st
            .eval_in_context("(function(){ try { A.buffer.transfer(); return 'moved'; } catch (e) { return String(e); } })()")
            .unwrap()
            .as_str()
            .unwrap()
            .contains("pinned"));
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
        assert!(
            compile_script("function (").is_err(),
            "syntax error is a compile failure"
        );

        let mut st = compile_script("throw new Error('boom');").expect("compiles");
        let err = st.run_init().expect_err("uncaught throw is an Err");
        assert!(err.contains("boom"), "got {err:?}");
        // The VM is still usable after an uncaught top-level throw.
        assert_eq!(st.eval_in_context("1 + 1"), Ok(JsValue::Number(2.0)));

        let mut st = compile_script("var x = 1;").expect("compiles");
        st.run_init().expect("runs");
        assert!(
            st.call_global("nope; evil()", &[]).is_err(),
            "non-identifier is rejected"
        );
        assert!(!st.has_global_function("nope"));
        assert!(st.has_global_function("isNaN"));
    }

    /// Resolve a symbol by name, for tests that care about a binding rather
    /// than a slot number.
    fn slot_of(st: &ScriptState, name: &str) -> u32 {
        st.symbols()
            .into_iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("no {name}"))
            .index
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
        assert_eq!(
            by("K"),
            Some(SymbolScope::Function),
            "a class is callable state"
        );
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
            assert_eq!(
                st.get_slot(slot_of(&st, name)),
                HostValue::Opaque,
                "{name} is opaque"
            );
        }
        // A host that read the whole global set and wrote it back must not
        // destroy the function it read as `Opaque`.
        let keep = slot_of(&st, "keep");
        assert!(
            !st.set_slot(keep, &HostValue::Number(1.0)),
            "write to a function slot is declined"
        );
        assert!(!st.set_slot(keep, &HostValue::Opaque));
        assert_eq!(st.eval_in_context("keep()"), Ok(JsValue::Number(42.0)));
    }

    #[test]
    fn a_read_modify_write_cannot_strip_an_object_s_methods() {
        // Exactly what a UI host does to a bridge object: read it, spread it,
        // add a key, write it back. The methods it could not see come back as
        // Null, and must not land as Null.
        let mut st =
            compile_script("var bridge = { greet: function (n) { return 'hi ' + n; }, count: 1 };")
                .expect("compiles");
        st.run_init().expect("runs");
        let slot = slot_of(&st, "bridge");

        let read = st.get_slot(slot);
        assert_eq!(
            read,
            HostValue::Object(vec![
                ("greet".into(), HostValue::Opaque),
                ("count".into(), HostValue::Number(1.0)),
            ])
        );

        // Write back the shape the host would produce from that read.
        assert!(st.set_slot(
            slot,
            &HostValue::Object(vec![
                ("greet".into(), HostValue::Null),
                ("count".into(), HostValue::Number(2.0)),
                ("added".into(), HostValue::String("x".into())),
            ]),
        ));
        assert_eq!(
            st.eval_in_context("bridge.greet('bob') + '/' + bridge.count + '/' + bridge.added"),
            Ok(JsValue::String("hi bob/2/x".into())),
            "the method survived the round trip"
        );

        // A key the host omits entirely is preserved when opaque, dropped when
        // it is data the host chose not to send back.
        assert!(st.set_slot(
            slot,
            &HostValue::Object(vec![("count".into(), HostValue::Number(3.0))])
        ));
        assert_eq!(
            st.eval_in_context("bridge.greet('x')"),
            Ok(JsValue::String("hi x".into()))
        );
        assert_eq!(
            st.eval_in_context("typeof bridge.added"),
            Ok(JsValue::String("undefined".into()))
        );

        // But an explicit value always wins — this protects what the host could
        // not express, never what it deliberately set.
        assert!(st.set_slot(
            slot,
            &HostValue::Object(vec![("greet".into(), HostValue::Number(9.0))])
        ));
        assert_eq!(st.eval_in_context("bridge.greet"), Ok(JsValue::Number(9.0)));
    }

    #[test]
    fn cycles_and_holes_do_not_hang_the_walk() {
        let mut st =
            compile_script("var cyc = { name: 'root' }; cyc.self = cyc; var sparse = [1, , 3];")
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
    fn shared_dag_is_stopped_by_the_host_conversion_budget() {
        let mut st = compile_script("var dag = 0; for (var i = 0; i < 32; i++) dag = [dag, dag];")
            .expect("compiles");
        st.run_init().expect("runs");
        let err = st
            .try_get_slot(slot_of(&st, "dag"))
            .expect_err("the expanded representation must be bounded");
        assert!(err.contains("conversion node limit"), "got {err:?}");
        assert_eq!(
            st.get_slot(slot_of(&st, "dag")),
            HostValue::Opaque,
            "the compatibility getter fails closed"
        );
    }

    #[test]
    fn host_conversion_budget_counts_nodes_and_utf8_bytes_exactly() {
        let mut budget = HostValueBudget::new(2, 4);
        budget.charge_node().expect("first node");
        budget.charge_node().expect("second node");
        assert!(budget.charge_node().unwrap_err().contains("node limit"));

        budget.charge_string("a").expect("one byte");
        budget.charge_string("é").expect("two bytes");
        assert!(budget
            .charge_string("xy")
            .unwrap_err()
            .contains("string limit"));
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
        let got = st
            .call_slot(add, &[arg, HostValue::Number(2.0)])
            .expect("calls");
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
            HostValue::Array(vec![
                HostValue::String("x".into()),
                HostValue::String("x".into())
            ])
        );
        // A throw is an Err, and the VM stays usable afterwards.
        assert!(st
            .call_slot(add, &[HostValue::Null, HostValue::Number(1.0)])
            .is_err());
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

    /// A host that re-enters a script must be able to flush what that re-entry
    /// scheduled. UI frameworks apply state updates on a microtask, so without
    /// this a click handler's `setState` never takes effect.
    #[test]
    fn microtasks_scheduled_by_a_reentry_can_be_drained() {
        let mut st = compile_script(
            "var log = []; function go() { Promise.resolve().then(function(){ log.push('ran') }) }",
        )
        .expect("compiles");
        st.run_init().expect("runs");
        st.call_global("go", &[]).expect("calls");
        // The reaction is queued, not run: call_global returns as soon as `go` does.
        assert_eq!(st.eval_in_context("log.length"), Ok(JsValue::Number(0.0)));
        st.run_microtasks();
        assert_eq!(st.eval_in_context("log.length"), Ok(JsValue::Number(1.0)));
    }

    #[test]
    fn console_output_is_drainable() {
        let mut st = compile_script("console.log('a'); console.error('b');").expect("compiles");
        st.run_init().expect("runs");
        assert_eq!(st.take_output(), vec!["a".to_string()]);
        assert_eq!(st.take_errput(), vec!["b".to_string()]);
        assert!(st.take_output().is_empty(), "draining clears the buffer");
    }

    #[test]
    fn combined_console_view_keeps_chronology() {
        let mut st = compile_script(
            "console.log('first'); console.error('second'); console.log('third'); \
             Promise.resolve().then(function () { console.warn('fourth'); console.info('fifth'); }); \
             function later() { console.log('sixth'); console.error('seventh'); throw new Error('x'); }",
        )
        .expect("compiles");
        st.run_init().expect("runs");
        let tag = |s: ConsoleStream| {
            if s == ConsoleStream::Stderr {
                "err"
            } else {
                "out"
            }
        };
        let got: Vec<String> = st
            .take_console()
            .into_iter()
            .map(|(s, l)| format!("{}:{l}", tag(s)))
            .collect();
        assert_eq!(
            got,
            [
                "out:first",
                "err:second",
                "out:third",
                "err:fourth",
                "out:fifth"
            ]
        );
        assert!(st.take_console().is_empty(), "draining clears both buffers");

        // Output written before a throw is kept, in order.
        assert!(st.call_global("later", &[]).is_err());
        let got: Vec<String> = st
            .take_console()
            .into_iter()
            .map(|(s, l)| format!("{}:{l}", tag(s)))
            .collect();
        assert_eq!(got, ["out:sixth", "err:seventh"]);

        // A per-stream take removes only its own entries from the order.
        st.call_global("later", &[]).ok();
        assert_eq!(st.take_output(), vec!["sixth".to_string()]);
        let got: Vec<String> = st
            .take_console()
            .into_iter()
            .map(|(s, l)| format!("{}:{l}", tag(s)))
            .collect();
        assert_eq!(got, ["err:seventh"]);
    }

    #[cfg(feature = "instrument")]
    #[test]
    fn output_budget_stops_buffering_even_when_caught() {
        let mut st = compile_script(
            "for (let i = 0; i < 1000; i++) { try { console.log('1234567890'); } catch (e) {} }",
        )
        .expect("compiles");
        st.set_limits(1_000_000, None);
        st.set_output_limit(64);

        let err = st.run_init().expect_err("output cap stops the script");
        assert!(err.contains("output budget"), "{err}");
        let retained: usize = st.take_output().iter().map(|line| line.len() + 1).sum();
        assert!(retained <= 64, "retained {retained} bytes");
    }

    #[cfg(feature = "instrument")]
    #[test]
    fn resource_limit_status_is_typed_and_survives_microtask_rejection() {
        let mut spoof =
            compile_script(r#"throw "RangeError: script exceeded its instruction budget";"#)
                .expect("compiles");
        spoof.set_limits(10_000, None);
        let err = spoof.run_init().expect_err("guest throw surfaces");
        assert!(err.contains("instruction budget"), "{err}");
        assert_eq!(
            spoof.resource_limit_error(),
            None,
            "guest-controlled exception text must not impersonate the recorder"
        );

        let mut exhausted = compile_script(
            "function go() { Promise.resolve().then(function () { for (;;) {} }); }",
        )
        .expect("compiles");
        exhausted.set_limits(10_000, None);
        exhausted.run_init().expect("initializes within budget");
        let go = slot_of(&exhausted, "go");
        exhausted
            .call_slot(go, &[])
            .expect("the callback failure becomes a rejected promise");
        let error = exhausted
            .resource_limit_error()
            .expect("recorder status survives promise rejection");
        assert!(error.contains("instruction budget"), "{error}");
    }
}
