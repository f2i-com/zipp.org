//! # zipp-vm — dynamic JavaScript engine v2
//!
//! A clean-sheet engine built around one architectural bet: an **explicit-frame
//! register VM**. JS recursion lives in a `Vec` of frames over a flat register
//! file, never the native Rust stack. That gives two things the previous engine
//! lacked by construction:
//!
//! 1. **Bounded, catchable recursion** — deep recursion throws a `RangeError`
//!    instead of overflowing the native stack (a real correctness gap before).
//! 2. **A JIT-ready substrate** — registers are explicit and a value can stay
//!    in one place across a basic block (and, in a later JIT tier, across a
//!    call), which is exactly the property V8 exploits and the old engine could
//!    not preserve through recursion.
//!
//! Pipeline: `oxc` parse → [`compile`] (AST → register bytecode) → [`vm`]
//! (explicit-frame interpreter). This is the interpreter milestone: correct and
//! clean, but NOT yet faster than the old JIT'd engine or V8 — a from-scratch
//! engine starts at zero and earns speed back tier by tier. Numbers are
//! reported honestly at each step.

mod bytecode;
mod capture;
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
mod codegen;
mod compile;
/// Persistent-VM embedding API, for hosts that keep a script alive across many
/// re-entries rather than running it once (see the module docs).
pub mod embed;
mod front;
pub use front::set_pure_script_goal;
mod heap;
mod slot_table;
/// Hand-written front end (lexer/AST/parser) being built to replace
/// `oxc_parser` — see the module docs for why. Not yet wired in.
mod parse;
mod shape;

/// Transition-tree diagnostics: `(nodes, max fan-out, total edges)`. Behind
/// `ZIPP_SHAPESTATS=1` in the CLI — fan-out is what decides whether a linear
/// scan of a node's outgoing edges is the right structure.
pub fn shape_stats() -> (usize, usize, usize) {
    shape::stats()
}

/// Whether this build actually has the native codegen tiers, i.e. the `jit`
/// feature AND an x86-64 target. Exported for `zipp --version`: the feature lives
/// on THIS crate, so a `cfg!` in the CLI would always read false and quietly
/// report every build as interpreter-only.
pub fn jit_enabled() -> bool {
    cfg!(all(feature = "jit", target_arch = "x86_64"))
}
pub mod value;
mod vm;


pub use value::Value;

/// Install the host's clocks — required on wasm32, ignored everywhere else.
///
/// wasm32 has no clock of its own: `std`'s `Instant::now`/`SystemTime::now`
/// there are stubs that PANIC, and `Vm::new` reads one, so on that target an
/// embedder must call this before constructing a VM or the engine traps before
/// running a line of JS. On every other target `std::time` is already the best
/// available source and this is a no-op, so a host may call it unconditionally.
///
/// `epoch_ms` is milliseconds since the Unix epoch (`Date.now`); `mono_ms` is a
/// monotonically non-decreasing millisecond count with an arbitrary zero
/// (`performance.now`). See `vm::clock` for what the engine does without them.
pub use vm::clock::install as install_clock;

/// Result of running a program: console output plus an optional uncaught-throw
/// message (output produced before the throw is preserved, like a real engine
/// flushing stdout before reporting the error).
pub struct Outcome {
    pub output: Vec<String>,
    /// Lines from `console.error`/`console.warn` (the caller writes these to
    /// stderr, matching node).
    pub errput: Vec<String>,
    pub error: Option<String>,
}

/// Parse + run JavaScript source. `Err` is a compile-time failure (parse error
/// or unsupported syntax); a runtime uncaught throw is reported via
/// [`Outcome::error`] alongside any output produced before it.
pub fn run(src: &str) -> Result<Outcome, String> {
    run_with_base(src, None)
}

/// Compile `src` and return a canonical text form of the resulting `Program`,
/// WITHOUT running it.
///
/// This is the comparison medium for swapping in a second front end: if two
/// front ends produce the same string for every file in a corpus, they compile
/// to the same program, and the swap cannot change behaviour. It is also the
/// only way to inspect bytecode without executing the script — `ZIPP_VM_DUMP`
/// prints as a side effect of running, which is useless for a program that
/// loops or throws.
///
/// The canonical form is `{:#?}`, deliberately: it covers every field of
/// `Program` by construction, so a field added later cannot silently escape
/// the comparison the way a hand-written formatter would let it.
pub fn compile_to_text(src: &str, module: bool) -> Result<String, String> {
    let program = compile_only(src, module)?;
    Ok(format!("{program:#?}"))
}

/// Parse with OUR parser and return the AST's canonical text form.
///
/// The gate for the hand-written front end: this and `lower_to_text` should
/// agree for every file in a corpus, because the bridge's output is already
/// known to compile byte-identically.
pub fn parse_to_text(src: &str, module: bool) -> Result<String, String> {
    let opts = if module {
        parse::parser::ParseOptions::module()
    } else {
        // The engine's scripts are CommonJS-shaped (`SourceType::cjs()` today):
        // node wraps a file in a function, so top-level `return` is legal and
        // real packages use it. `ParseOptions::script()` stays spec-strict for
        // callers that want the pure Script goal.
        parse::parser::ParseOptions {
            allow_return: true,
            ..parse::parser::ParseOptions::script()
        }
    };
    match parse::stmt::parse(src, opts) {
        Ok(p) => Ok(format!("{p:#?}")),
        Err(e) => Err(format!("{} (at {})", e.msg, e.pos)),
    }
}

/// Parse + compile, no VM. Shares `run_with_base`'s Annex B parse-retry so the
/// dump reflects what would actually run.
fn compile_only(src: &str, module: bool) -> Result<bytecode::Program, String> {
    let ast =
        if module { front::parse_module(src)? } else { front::parse_script(src)? };
    compile::compile_program(&ast, src)
}

/// Like [`run`], but `base_dir` is the directory the script was loaded from, used
/// to resolve a dynamic `import(specifier)` against the filesystem. `None` (the
/// `run` default) means no host module loader, so `import()` rejects.
pub fn run_with_base(src: &str, base_dir: Option<std::path::PathBuf>) -> Result<Outcome, String> {
    // Annex B call assignment targets (`f() = 1` in sloppy code) need no
    // source-rewrite-and-reparse any more: the parser produces `Target::Call`
    // directly and the compiler emits the runtime ReferenceError.
    let ast = front::parse_auto(src)?;
    let program = compile::compile_program(&ast, src)?;
    // Dev aid: `ZIPP_VM_DUMP=1` prints each function's bytecode to stderr before
    // running (so the JIT-able regions can be inspected).
    if std::env::var_os("ZIPP_VM_DUMP").is_some() {
        for (fid, f) in program.functions.iter().enumerate() {
            eprintln!("── fn {fid} (regs={}, params={}) ──", f.reg_count, f.param_count);
            for (ip, instr) in f.code.iter().enumerate() {
                eprintln!("  {ip:4}  {instr:?}");
            }
        }
    }
    let mut vm = vm::Vm::new(&program);
    vm.set_module_base_dir(base_dir);
    match vm.run() {
        Ok(_) => Ok(Outcome { output: vm.output, errput: vm.errput, error: None }),
        Err(thrown) => Ok(Outcome {
            output: std::mem::take(&mut vm.output),
            errput: std::mem::take(&mut vm.errput),
            error: Some(thrown.0),
        }),
    }
}

/// Run `harness` and then `src` as TWO SEPARATE SCRIPTS in one realm — the
/// script-goal analogue of [`run_module_file`]'s harness prelude.
///
/// This exists because concatenating them is not equivalent. `INTERPRETING.md`
/// evaluates each `includes:` file in the realm *prior to* the test, and applies
/// the strict-mode directive as "the initial character sequence of the file" —
/// the TEST file. Gluing harness and test into one source and putting
/// `"use strict";` on top instead makes the HARNESS strict, and any harness
/// helper that uses a **direct** `eval` inherits that strictness, so the mode
/// under test leaks into code that is supposed to stay sloppy. That is not a
/// hypothetical: it accounted for 19 of this engine's test262 failures, and V8
/// fails the very same tests when handed the concatenated bytes.
///
/// The harness runs first as a realm script (its `var`/function declarations
/// become realm globals the test can see), then the test runs as its own script
/// through the same path `$262.evalScript` uses, so its directive prologue —
/// and only its own — governs it. The event loop is drained afterwards, and
/// drained even when the test threw, matching [`run_with_base`].
pub fn run_with_harness(
    src: &str,
    harness: &str,
    base_dir: Option<std::path::PathBuf>,
) -> Result<Outcome, String> {
    // `src` — the TEST — is compiled as the main program, so it keeps ordinary
    // script semantics in full; the harness is the prelude. See
    // `Vm::run_with_prelude` for why it is this way round and not the other.
    let ast = front::parse_auto(src)?;
    let program = compile::compile_program(&ast, src)?;
    let mut vm = vm::Vm::new(&program);
    vm.set_module_base_dir(base_dir);
    match vm.run_with_prelude(Some(harness)) {
        Ok(_) => Ok(Outcome { output: vm.output, errput: vm.errput, error: None }),
        Err(thrown) => Ok(Outcome {
            output: std::mem::take(&mut vm.output),
            errput: std::mem::take(&mut vm.errput),
            error: Some(thrown.0),
        }),
    }
}

/// Run `src` as an ES MODULE entry: the top level is an async context (top-level
/// `await`), declarations are module-scoped, and the event loop drains to
/// completion. `base_dir` resolves relative imports. Like [`run_with_base`] but
/// for `flags:[module]` test262 tests and `.mjs` entry points.
/// Run the module FILE at `path`: an entry with STATIC imports routes through
/// the module loader (dependencies link before evaluation); one without keeps
/// the async-capable direct path (top-level await works there).
pub fn run_module_file(
    path: &std::path::Path,
    harness_path: Option<&str>,
) -> Result<Outcome, String> {
    let src = std::fs::read_to_string(path).map_err(|e| format!("read error: {e}"))?;
    let base_dir = path.parent().map(|p| p.to_path_buf());
    let harness = match harness_path {
        Some(h) => Some(std::fs::read_to_string(h).map_err(|e| format!("read error: {e}"))?),
        None => None,
    };
    let entry_ast = front::parse_module(&src)?;
    compile::compile_module(&entry_ast, &src)?;
    // The harness (if any) runs as a realm SCRIPT — its vars become realm
    // globals every module can reference — then the entry loads through the
    // module loader: imports link before evaluation and the module's own
    // declarations stay MODULE-scoped (never globalThis properties). An entry
    // using top-level await falls back to the direct async-capable path (the
    // loader's eval pipeline can't suspend yet); nothing of the entry has run
    // when that compile-time rejection surfaces.
    let host_src = harness.clone().unwrap_or_default();
    // The harness runs as a realm SCRIPT (its declarations become realm
    // globals), so it parses script-first — which is also what it compiled as
    // before the front-end swap, oxc's module-flavoured default notwithstanding.
    let host_ast = front::parse_auto(&host_src)?;
    let host = compile::compile_program(&host_ast, &host_src)?;
    let mut vm = vm::Vm::new(&host);
    vm.set_module_base_dir(base_dir.clone());
    if let Err(thrown) = vm.run() {
        return Ok(Outcome {
            output: std::mem::take(&mut vm.output),
            errput: std::mem::take(&mut vm.errput),
            error: Some(thrown.0),
        });
    }
    match vm.run_module_entry(path) {
        Ok(_) => Ok(Outcome { output: vm.output, errput: vm.errput, error: None }),
        Err(thrown) if thrown.0.contains("top-level await is not supported") => {
            // ENTRY top-level await: rerun on a fresh Vm via the direct
            // async-capable module path, with the harness prepended (the
            // single-module concatenation).
            let combined;
            let text: &str = match &harness {
                Some(h) => {
                    // A hashbang is only valid at position 0 — strip it (it is
                    // a comment) before prepending the harness.
                    let body = if src.starts_with("#!") {
                        src.split_once('\n').map(|(_, rest)| rest).unwrap_or("")
                    } else {
                        src.as_str()
                    };
                    combined = format!("{h}\n{body}");
                    combined.as_str()
                }
                None => src.as_str(),
            };
            let ast2 = front::parse_module(text)?;
            let program2 = compile::compile_module(&ast2, text)?;
            let mut vm = vm::Vm::new(&program2);
            vm.set_module_base_dir(base_dir);
            match vm.run_module() {
                Ok(_) => Ok(Outcome { output: vm.output, errput: vm.errput, error: None }),
                Err(thrown) => Ok(Outcome {
                    output: std::mem::take(&mut vm.output),
                    errput: std::mem::take(&mut vm.errput),
                    error: Some(thrown.0),
                }),
            }
        }
        Err(thrown) => Ok(Outcome {
            output: std::mem::take(&mut vm.output),
            errput: std::mem::take(&mut vm.errput),
            error: Some(thrown.0),
        }),
    }
}

pub fn run_module_with_base(src: &str, base_dir: Option<std::path::PathBuf>) -> Result<Outcome, String> {
    let ast = front::parse_module(src)?;
    let program = compile::compile_module(&ast, src)?;
    if std::env::var_os("ZIPP_VM_DUMP").is_some() {
        for (fid, f) in program.functions.iter().enumerate() {
            eprintln!("── fn {fid} (regs={}, params={}) ──", f.reg_count, f.param_count);
            for (ip, instr) in f.code.iter().enumerate() {
                eprintln!("  {ip:4}  {instr:?}");
            }
        }
    }
    let mut vm = vm::Vm::new(&program);
    vm.set_module_base_dir(base_dir);
    match vm.run_module() {
        Ok(_) => Ok(Outcome { output: vm.output, errput: vm.errput, error: None }),
        Err(thrown) => Ok(Outcome {
            output: std::mem::take(&mut vm.output),
            errput: std::mem::take(&mut vm.errput),
            error: Some(thrown.0),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_ok(src: &str) -> Vec<String> {
        let out = run(src).expect("compile");
        assert!(out.error.is_none(), "unexpected throw: {:?}", out.error);
        out.output
    }

    #[test]
    fn console_log_basics() {
        assert_eq!(run_ok("console.log(1, 2, 3)"), vec!["1 2 3"]);
        assert_eq!(run_ok("console.log('hi', true, null)"), vec!["hi true null"]);
    }

    #[test]
    fn arithmetic() {
        assert_eq!(run_ok("console.log(1 + 2 * 3)"), vec!["7"]);
        assert_eq!(run_ok("console.log(10 - 3 - 2)"), vec!["5"]);
        assert_eq!(run_ok("console.log(7 % 3)"), vec!["1"]);
        assert_eq!(run_ok("console.log(7 / 2)"), vec!["3.5"]);
    }

    /// `x = <expr that reads x>` must read x's OLD value. The assignment
    /// compiler builds the RHS straight into x's own register to save a Move,
    /// which is only sound for forms that finish reading before they write.
    /// Object and template literals materialise into the destination and fill
    /// it in afterwards, so in place they would read their own half-built value.
    ///
    /// React's minified `createContext` is exactly this shape —
    /// `e = {_currentValue: e, …}` — so a regression here makes every React
    /// context self-referential and takes the whole library down with it.
    /// A parameter DEFAULT can close over the enclosing scope, so a name it
    /// references has to be boxed like any other captured variable.
    ///
    /// The free-variable scan used to look only at a nested function's BODY. A
    /// name referenced ONLY from a default was therefore never captured, and
    /// the default threw "not defined" at runtime while the identical
    /// reference in the body worked. `function J(r=G){...}` is a stock shape in
    /// minified bundles for a defaulted config argument, so this took out whole
    /// applications.
    /// A name used only inside an OPTIONAL CHAIN is still captured.
    ///
    /// `Expr::Chain` wraps the whole chain, so without an arm in the capture
    /// walkers every name inside `r()?.getItem(k)` was invisible: never boxed,
    /// nothing for a nested function to capture, ReferenceError at runtime.
    /// Under-inclusion in that scan is not a slower lookup, it is a throw.
    /// `var` is a Statement, so it is legal as an unbraced statement body.
    ///
    /// Only `let`/`const`/`class`/`function` are Declarations and barred there.
    /// Rejecting `if (x) var y = 1;` broke every minified bundle, since dropping
    /// the braces is a standard minifier size win.
    #[test]
    fn var_is_legal_as_an_unbraced_statement_body() {
        assert_eq!(run_ok("if(true)var x=1; console.log(x)"), vec!["1"]);
        assert_eq!(run_ok("if(false)var x=1; else var y=2; console.log(y)"), vec!["2"]);
        assert_eq!(run_ok("for(var i=0;i<1;i++)var q=5; console.log(q)"), vec!["5"]);
        assert_eq!(run_ok("for(var k in {a:1})var m=1; console.log(m)"), vec!["1"]);
        assert_eq!(run_ok("for(var v of [1])var n=2; console.log(n)"), vec!["2"]);
        assert_eq!(run_ok("do var d=1; while(false); console.log(d)"), vec!["1"]);
        assert_eq!(run_ok("var i=0; while(i<1){i++} console.log(i)"), vec!["1"]);
        // `var` hoists out of the body, so it is visible afterwards even when
        // the branch never ran.
        assert_eq!(
            run_ok("if(false)var h=1; console.log(typeof h)"),
            vec!["undefined"]
        );
        // A real Declaration in the same position is still an error.
        assert!(run("if(true)class C{}").is_err());
    }

    #[test]
    fn optional_chain_captures_enclosing() {
        assert_eq!(
            run_ok(
                "console.log((function(){ const r=()=>({x:'ok'}), o={f:()=>r()?.x};                  return o.f() })())"
            ),
            vec!["ok"]
        );
        assert_eq!(
            run_ok(
                "console.log((function(){ const r=()=>'ok', o={f:()=>r?.()}; return o.f() })())"
            ),
            vec!["ok"]
        );
        assert_eq!(
            run_ok(
                "console.log((function(){ const k='x', r={x:'ok'}, o={f:()=>r?.[k]};                  return o.f() })())"
            ),
            vec!["ok"]
        );
        // The real shape: a storage adapter reaching a captured accessor
        // through a chain from every method.
        assert_eq!(
            run_ok(
                "var store={_d:{},setItem:function(k,v){this._d[k]=v},getItem:function(k){return this._d[k]||null}};                  function mk(){ const r=()=>store, t={ set:(k,v)=>{r()?.setItem(k,v)},                  get:k=>r()?.getItem(k)??null }; return t }                  var a=mk(); a.set('k','ok'); console.log(a.get('k'))"
            ),
            vec!["ok"]
        );
    }

    #[test]
    fn param_default_captures_enclosing() {
        // Every binding kind, since the scan is what fails, not the kind.
        for kw in ["const", "let", "var"] {
            assert_eq!(
                run_ok(&format!(
                    "console.log((function(){{ {kw} G='ok';                      function i(r=G){{return r}} return i() }})())"
                )),
                vec!["ok"],
                "{kw}"
            );
        }
        // Function expression, arrow, and a default declared before the binding.
        assert_eq!(
            run_ok(
                "console.log((function(){ const G='ok';                  var i=function(r=G){return r}; return i() })())"
            ),
            vec!["ok"]
        );
        assert_eq!(
            run_ok("console.log((function(){ const G='ok'; var i=(r=G)=>r; return i() })())"),
            vec!["ok"]
        );
        assert_eq!(
            run_ok(
                "console.log((function(){ function i(r=G){return r} const G='ok'; return i() })())"
            ),
            vec!["ok"]
        );
        // Through an intervening function, and via a destructuring default.
        assert_eq!(
            run_ok(
                "console.log((function(){ const G='ok'; return (function(){                  function i(r=G){return r} return i() })() })())"
            ),
            vec!["ok"]
        );
        assert_eq!(
            run_ok(
                "console.log((function(){ const G='ok';                  function i({r=G}={}){return r} return i() })())"
            ),
            vec!["ok"]
        );
        // A default referring to an EARLIER PARAMETER is not a capture, and must
        // keep resolving to the parameter rather than leaking outward.
        assert_eq!(
            run_ok(
                "var a='outer'; console.log((function(){                  function i(a,b=a){return b} return i('inner') })())"
            ),
            vec!["inner"]
        );
    }

    #[test]
    fn assign_reads_target() {
        // The two forms that build incrementally.
        assert_eq!(run_ok("let e = 'old'; e = {v: e}; console.log(e.v)"), vec!["old"]);
        assert_eq!(run_ok("let e = 'old'; e = `<${e}>`; console.log(e)"), vec!["<old>"]);
        // Reached through the transparent forms.
        assert_eq!(run_ok("let e = 'old'; e = e ? {v: e} : 0; console.log(e.v)"), vec!["old"]);
        assert_eq!(run_ok("let e = 'old'; e = e && {v: e}; console.log(e.v)"), vec!["old"]);
        assert_eq!(run_ok("let e = 'old'; e = (0, {v: e}); console.log(e.v)"), vec!["old"]);
        assert_eq!(run_ok("let e = 'old'; e = {a: {b: e}}; console.log(e.a.b)"), vec!["old"]);
        assert_eq!(run_ok("let e = 'old'; e = {...{}, v: e}; console.log(e.v)"), vec!["old"]);
        // Forms that were already correct must stay correct (and keep compiling
        // in place — this asserts behaviour, not codegen, but pins the semantics).
        assert_eq!(run_ok("let e = 'old'; e = [e]; console.log(e[0])"), vec!["old"]);
        assert_eq!(run_ok("let e = 1; e = e + 1; console.log(e)"), vec!["2"]);
        assert_eq!(
            run_ok("function f(x){return x} let e = 'old'; e = f(e); console.log(e)"),
            vec!["old"]
        );
        // A parameter is a local too.
        assert_eq!(
            run_ok("function f(e){ e = {v: e}; return e.v } console.log(f('old'))"),
            vec!["old"]
        );
        // `x = x.p = v`: the inner store resolves its base to a register, and
        // for a plain local that IS the destination — no copy is made — so
        // evaluating `v` into it clobbered the object and `.p` landed on `v`.
        assert_eq!(
            run_ok(
                "function f(){ var e = {p: null}, keep = e; e = e.p = 'V'; \
                 return keep.p + ',' + e } console.log(f())"
            ),
            vec!["V,V"]
        );
        // Same through a computed key, a parameter, and a three-deep chain.
        assert_eq!(
            run_ok(
                "function f(e){ var keep = e; e = e['p'] = 'V'; return keep.p } \
                 console.log(f({p: null}))"
            ),
            vec!["V"]
        );
        assert_eq!(
            run_ok(
                "function f(){ var e = {p: null, q: null}, keep = e; e = e.p = e.q = 'V'; \
                 return keep.p + keep.q } console.log(f())"
            ),
            vec!["VV"]
        );
        // React's minified `useState` mount, which is where this surfaced: the
        // queue's `dispatch` must be the bound setter, not stay null. Reading it
        // back is what every re-render does, so a regression makes the SECOND
        // interaction with any stateful control throw.
        assert_eq!(
            run_ok(
                "function K(){ return 'set' } \
                 function mountState(e){ var t = {queue: null}; \
                 t.memoizedState = e; \
                 e = {pending: null, dispatch: null, lastRenderedState: e}; \
                 t.queue = e; \
                 e = e.dispatch = K.bind(null, 0, e); \
                 return [t.queue.dispatch, e] } \
                 var r = mountState(false); \
                 console.log(typeof r[0], typeof r[1], r[0] === r[1])"
            ),
            vec!["function function true"]
        );
        // The whole React shape, end to end.
        assert_eq!(
            run_ok(
                "function createContext(e){ return (e = {_currentValue: e, _currentValue2: e, \
                 Provider: null, Consumer: null}).Provider = {_context: e}, e.Consumer = e, e } \
                 var c = createContext(null); \
                 console.log(c._currentValue === null, c._currentValue2 === null, c.Consumer === c)"
            ),
            vec!["true true true"]
        );
    }

    /// An arrow body is a scope like any other: a `function` declaration hoisted
    /// above a `let`/`const` in the same body must still capture that binding.
    ///
    /// Only function BODIES pre-created cells for their body-level lexicals, so
    /// in an arrow the forward reference found no binding, compiled to a global
    /// load, and failed at runtime with "x is not defined" (note: NOT the TDZ
    /// error, which is what a genuine too-early read reports). Webpack wraps
    /// bundles in `(() => { "use strict"; … })()`, so this hit essentially every
    /// modern minified bundle.
    #[test]
    fn arrow_body_lexicals_are_capturable() {
        // The shape webpack emits.
        assert_eq!(
            run_ok(
                "console.log((() => { \"use strict\"; \
                 function use() { return G('x') } const G = e => '?' + e; return use() })())"
            ),
            vec!["?x"]
        );
        // let, and a chain of declarators referring to each other.
        assert_eq!(
            run_ok(
                "console.log((() => { function use() { return K(['a','b']) } \
                 let q = e => e, K = e => q(e.join('/')); return use() })())"
            ),
            vec!["a/b"]
        );
        // A class declaration is a lexical binding too.
        assert_eq!(
            run_ok(
                "console.log((() => { function make() { return new C().v } \
                 class C { constructor() { this.v = 7 } } return make() })())"
            ),
            vec!["7"]
        );
        // The equivalent function-expression body always worked — keep it working.
        assert_eq!(
            run_ok(
                "console.log((function () { \
                 function use() { return G('x') } const G = e => '?' + e; return use() })())"
            ),
            vec!["?x"]
        );
        // A genuine too-early read still reports TDZ, not "not defined".
        let out = run("(() => { function early() { return L } early(); const L = 1 })()")
            .expect("compiles");
        let err = out.error.expect("throws");
        assert!(err.contains("before initialization"), "got {err:?}");
    }

    #[test]
    fn let_and_reassign() {
        assert_eq!(run_ok("let x = 5; x = x + 1; console.log(x)"), vec!["6"]);
        assert_eq!(run_ok("let x = 1; x += 10; console.log(x)"), vec!["11"]);
    }

    #[test]
    fn if_else() {
        assert_eq!(
            run_ok("let x = 3; if (x > 2) { console.log('big') } else { console.log('small') }"),
            vec!["big"]
        );
    }

    #[test]
    fn while_loop() {
        assert_eq!(
            run_ok("let i = 0; let s = 0; while (i < 5) { s = s + i; i = i + 1 } console.log(s)"),
            vec!["10"]
        );
    }

    /// Run a program with the JIT forced OFF (pure interpreter), for differential
    /// checks against the default JIT-on `run`.
    fn run_nojit(src: &str) -> Vec<String> {
        let ast = front::parse_auto(src).expect("parse");
        let program = compile::compile_program(&ast, src).expect("compile");
        let mut vm = vm::Vm::new(&program);
        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
        vm.set_jit_enabled(false);
        vm.run().expect("run");
        vm.output
    }

    /// Assert JIT-on output == JIT-off output == `expected` for a hot loop. The
    /// loops here run well past OSR_THRESHOLD so the region JIT (int64 path) fires.
    fn assert_jit_matches(src: &str, expected: &[&str]) {
        let on = run_ok(src);
        let off = run_nojit(src);
        let exp: Vec<String> = expected.iter().map(|s| s.to_string()).collect();
        assert_eq!(on, off, "JIT-on != JIT-off for: {src}");
        assert_eq!(on, exp, "wrong result for: {src}");
    }

    #[test]
    fn int_region_positive_sum() {
        // sum 0..999 = 499500 (stays well within i32).
        assert_jit_matches("let s=0; for(let i=0;i<1000;i++){ s+=i; } console.log(s)", &["499500"]);
    }

    #[test]
    fn int_region_subtraction_negative() {
        // 0 - (0+1+...+999) = -499500 (negative i64, signed flush to Int).
        assert_jit_matches("let s=0; for(let i=0;i<1000;i++){ s-=i; } console.log(s)", &["-499500"]);
    }

    // ── B67: the tier facts whose guards live in more than one place ──
    // `tests/jit_tier_parity.rs` runs these at the DEFAULT tier and asserts the
    // right answer. These run each one JIT-on AND JIT-off and assert the two
    // AGREE, which is the property that actually broke: every one of B66's nine
    // divergences was a case where the interpreter was already right.

    #[test]
    fn jit_matches_deleted_global_throws() {
        // Both spellings of the deletion. `delete implicitG` always returned the
        // slot to the sentinel; `delete globalThis.implicitG` never cleared it at
        // all, so this used to disagree with node in BOTH tiers.
        assert_jit_matches(
            "implicitA = 5; function ra() { return implicitA; } \
             for (var i = 0; i < 3000; i++) ra(); \
             delete implicitA; \
             var a; try { a = 'v' + ra(); } catch (e) { a = 'throw'; } \
             implicitB = 6; function rb() { return implicitB; } \
             for (var j = 0; j < 3000; j++) rb(); \
             delete globalThis.implicitB; \
             var b; try { b = 'v' + rb(); } catch (e) { b = 'throw'; } \
             console.log(a + ',' + b)",
            &["throw,throw"],
        );
    }

    #[test]
    fn jit_matches_recreated_global_is_readable_again() {
        // The entry check must DECLINE, not evict: re-creating the binding has to
        // bring the compiled body back on its own.
        assert_jit_matches(
            "impR = 1; function r() { return impR; } \
             var a = 0; for (var i = 0; i < 3000; i++) a = r(); \
             delete impR; \
             var m; try { m = 'v' + r(); } catch (e) { m = 'throw'; } \
             impR = 7; \
             var b = 0; for (var j = 0; j < 3000; j++) b = r(); \
             console.log(a + ',' + m + ',' + b)",
            &["1,throw,7"],
        );
    }

    #[test]
    fn jit_matches_real_own_global_descriptor() {
        // A real own descriptor on the global object IS the binding: the getter
        // runs on a read and the setter on a write, including when the descriptor
        // appears after the loop has already been compiled.
        assert_jit_matches(
            "impG = 1; var seen = 0; \
             function w(n) { var last; for (var i = 0; i < n; i++) { impG = i; last = impG; } return last; } \
             w(3000); \
             Object.defineProperty(globalThis, 'impG', { \
               set: function (v) { seen++; }, get: function () { return 'G'; }, configurable: true }); \
             console.log(w(3000) + ',' + seen)",
            &["G,3000"],
        );
    }

    #[test]
    fn jit_matches_reprototyped_array_prototype() {
        // The indexed-proto protector must see `setPrototypeOf`, not just an
        // integer key being DEFINED on Array.prototype.
        assert_jit_matches(
            "var a = [1, 2]; \
             function read(o, n) { var s; for (var i = 0; i < n; i++) s = o[5]; return s; } \
             read(a, 3000); \
             Object.setPrototypeOf(Array.prototype, { 5: 'M5' }); \
             console.log(read(a, 3000) + ',' + (5 in a))",
            &["M5,true"],
        );
    }

    #[test]
    fn jit_matches_self_recursion_after_rebind() {
        // Tier A's direct `call` to its own entry has no callee guard, so the
        // recursion has to be refused at entry once the name is rebound.
        assert_jit_matches(
            "function fib(n) { if (n < 2) return n; return fib(n - 1) + fib(n - 2); } \
             var orig = fib; var w = 0; \
             for (var i = 0; i < 60; i++) w = orig(18); \
             fib = function (n) { return 0; }; \
             console.log((w > 0 ? 'after:' + orig(18) : 'warmup-failed'))",
            &["after:0"],
        );
    }

    #[test]
    fn jit_matches_global_define_materializes_the_binding() {
        // `defineProperty` on a slot-only binding is a REDEFINE, not a create: a
        // descriptor omitting `value` keeps the binding's value, and a
        // `configurable: true` request on a non-configurable `var` is rejected.
        assert_jit_matches(
            "foo = 1; \
             Object.defineProperty(globalThis, 'foo', { writable: false, configurable: true }); \
             var before = foo; foo = 2; \
             var d = Object.getOwnPropertyDescriptor(globalThis, 'foo'); \
             var thrown = 'none'; \
             var lateV = 0; \
             try { Object.defineProperty(globalThis, 'lateV', { value: 9, configurable: true }); } \
             catch (e) { thrown = e.constructor.name; } \
             console.log(before + ',' + foo + ',' + d.value + ',' + d.enumerable + ',' + thrown)",
            &["1,1,1,true,TypeError"],
        );
    }

    // ── B73: GetProp inside an inlined leaf body ──
    // The inline only exists once the enclosing loop is hot, so JIT-on vs JIT-off is
    // the axis that matters; `tests/leaf_getprop_inline.rs` carries the full matrix.

    #[test]
    fn jit_matches_leaf_getprop_own_and_inherited() {
        assert_jit_matches(
            "function get(o) { return o.v; }              var own = { v: 1 }, inh = Object.create({ v: 2 }), absent = { w: 3 };              var a = 0, b = 0, c = 'x';              for (var i = 0; i < 4000; i++) { a = get(own); b = get(inh); c = get(absent); }              console.log(a + ',' + b + ',' + c)",
            &["1,2,undefined"],
        );
    }

    #[test]
    fn jit_matches_leaf_getprop_defers_accessors_and_proxies() {
        assert_jit_matches(
            "var gc = 0, pc = 0;              var acc = {}; Object.defineProperty(acc, 'v', { get: function () { gc++; return 5; } });              var prox = new Proxy({ v: 6 }, { get: function (t, k) { pc++; return t[k] + 1; } });              function get(o) { return o.v; }              var a = 0, b = 0;              for (var i = 0; i < 4000; i++) { a = get(acc); b = get(prox); }              console.log(a + ',' + b + ',' + (gc === 4000) + ',' + (pc === 4000))",
            &["5,7,true,true"],
        );
    }

    #[test]
    fn jit_matches_leaf_getprop_when_the_shape_changes_mid_loop() {
        assert_jit_matches(
            "function get(o) { return o.v; }              var o = { v: 'data' }, first = '', last = '';              for (var i = 0; i < 4000; i++) {                if (i === 2000) Object.defineProperty(o, 'v', { get: function () { return 'getter'; }, configurable: true });                var r = get(o); if (i === 0) first = r; last = r;              }              console.log(first + ',' + last)",
            &["data,getter"],
        );
    }

    #[test]
    fn heap_obj_slot_stays_small() {
        // Every heap slot is one `HeapObj`, so the enum's size multiplies across
        // the whole heap. Measured: +64 bytes of pure padding cost 7.9% on the
        // bench suite, which is why `Object` and `Combinator` are boxed. A new
        // fat variant would silently undo that, so pin it.
        assert!(
            std::mem::size_of::<crate::heap::HeapObj>() <= 80,
            "HeapObj grew to {} bytes (cap 80) — box the new variant's payload",
            std::mem::size_of::<crate::heap::HeapObj>()
        );
    }

    #[test]
    fn ctor_field_hint_is_capped() {
        // One enormous instance must not teach a permanent reservation; the
        // small instances that follow must still be correct.
        assert_jit_matches(
            "function C(n){ for(var i=0;i<n;i++) this['k'+i]=i; }              var big=new C(3000); var small=[];              for(var j=0;j<200;j++) small.push(Object.keys(new C(2)).length);              console.log(Object.keys(big).length + ',' + small[0] + ',' + small[199])",
            &["3000,2,2"],
        );
    }

    #[test]
    fn arrow_body_lexical_assign_before_decl_is_tdz() {
        // Pre-creating an arrow body's lexical cells (so a hoisted nested
        // function can capture them) made an ASSIGNMENT before the textual
        // declaration resolve to that cell and emit a plain `CellSet`, writing
        // straight through the TDZ. A READ always threw, which is why it looked
        // fine. `block_tdz_cells` is what selects the checked store; the
        // declaration clears it again.
        // Regression: staging/sm/expressions/optional-chain-tdz.js [strict].
        //
        // NOTE the same shape in a `function` body or a bare block still fails
        // to throw in SLOPPY mode — a separate, PRE-EXISTING gap (those only
        // pre-create a cell when the name is captured, so an uncaptured
        // forward-assigned lexical resolves to an implicit global store
        // instead). That is why the `[sloppy]` half of the test262 case above
        // is a long-standing expected failure. Not covered here so this test
        // stays a regression guard for the arrow fix rather than a to-do.
        assert_jit_matches(
            "var out=[];              function t(n,f){ try{ f(); out.push(n+':none'); }catch(e){ out.push(n+':'+e.constructor.name); } }              t('assign', () => { b = 0; let b; });              t('read',   () => { var z = b; let b; });              t('optchain', () => { const N=null; N?.[b]; b = 0; let b; });              console.log(out.join('|'))",
            &["assign:ReferenceError|read:ReferenceError|optchain:ReferenceError"],
        );
    }

    #[test]
    fn arrow_body_lexical_assign_after_decl_is_allowed() {
        // The other side: once the declaration has run, assignment is normal.
        assert_jit_matches(
            "console.log((() => { let b = 1; b = 2; b += 3; return b; })())",
            &["5"],
        );
    }

    #[test]
    fn promise_resolve_uses_the_intrinsic_not_prototype_constructor() {
        // PromiseResolve step 2 returns x unchanged only when Get(x,"constructor")
        // is C, and C here is %Promise% itself. Reading C back out of the mutable
        // `Promise.prototype.constructor` answered a different question and got
        // BOTH patched cases wrong: a plain promise passed through when it must
        // not, and one whose OWN constructor is %Promise% did not when it must.
        assert_jit_matches(
            "var out=[]; var p1=Promise.resolve(1); out.push(Promise.resolve(p1)===p1);              Promise.prototype.constructor=function X(){};              var p2=Promise.resolve(2); out.push(Promise.resolve(p2)===p2);              var p3=Promise.resolve(3); p3.constructor=Promise;              out.push(Promise.resolve(p3)===p3); console.log(out.join(','))",
            &["true,false,true"],
        );
    }

    #[test]
    fn plain_object_method_inline_guards_the_slot() {
        // A method held as an own property of a PLAIN object now inlines. The
        // receiver-version guard is NOT enough on its own: `o.m = other`
        // overwrites the slot in place and deliberately does not bump the
        // version (the ordinary-set fast path keeps the shape stable so JIT
        // caches survive), so the arm also guards the slot's VALUE.
        //
        // Each case runs far past the OSR threshold and then changes something
        // the inline must notice.
        assert_jit_matches(
            "var out=[];              (function(){var o={m:function(){return 1;}},s=0;                for(var i=0;i<200000;i++){ if(i===100000) o.m=function(){return 2;}; s+=o.m(); }                out.push(s);})();              (function(){var o={v:7,m:function(){return this.v;}},s=0;                for(var i=0;i<200000;i++){ if(i===100000) o.v=9; s+=o.m(); } out.push(s);})();              (function(){var k=3,o={m:function(){return k;}},s=0;                for(var i=0;i<200000;i++){ if(i===100000) k=5; s+=o.m(); } out.push(s);})();              (function(){var o={m:function(){return 1;}},s=0,e=0;                for(var i=0;i<100000;i++){ if(i===50000) delete o.m;                  try{ s+=o.m(); }catch(x){ e++; } } out.push(s+'/'+e);})();              console.log(out.join('|'))",
            &["300000|1600000|800000|50000/50000"],
        );
    }

    #[test]
    fn super_getter_inline_invalidates() {
        // A class GETTER whose body reads `super.v` now inlines the parent
        // getter (Stage 6). The arm bakes the holder's slot, so each case here
        // runs far past the OSR threshold to get the arm installed and THEN
        // breaks one of the things the guard set is responsible for:
        //
        //   1. redefine the parent getter          → `mi_class_epoch` is NOT
        //      enough; the holder slot re-read catches it (a getter lives in
        //      `vals[slot]`, which is exactly what the re-read loads).
        //   2. replace the accessor with a DATA property of the same name.
        //   3. `delete` it, so `super.v` becomes undefined (→ NaN).
        //   4. `setPrototypeOf` the derived prototype, swapping the chain the
        //      hop version guards watch.
        //   5. mutate the field the parent getter reads through `this`, which
        //      must be observed because the body is re-run, not memoised.
        assert_jit_matches(
            "var out=[];\
             (function(){class A{constructor(x){this._v=x}get v(){return this._v}}\
              class B extends A{get v(){return super.v*2}}var b=new B(10),s=0;\
              for(var i=0;i<200000;i++){ if(i===100000) Object.defineProperty(A.prototype,'v',{get:function(){return 100},configurable:true}); s+=b.v; }\
              out.push(s);})();\
             (function(){class A{constructor(x){this._v=x}get v(){return this._v}}\
              class B extends A{get v(){return super.v*2}}var b=new B(3),s=0;\
              for(var i=0;i<200000;i++){ if(i===100000) Object.defineProperty(A.prototype,'v',{value:55,writable:true,configurable:true}); s+=b.v; }\
              out.push(s);})();\
             (function(){class A{constructor(x){this._v=x}get v(){return this._v}}\
              class B extends A{get v(){return super.v*2}}var b=new B(4),s=0,n=0;\
              for(var i=0;i<200000;i++){ if(i===100000) delete A.prototype.v; var t=b.v; if(t!==t) n++; else s+=t; }\
              out.push(s+'/'+n);})();\
             (function(){class A{constructor(x){this._v=x}get v(){return this._v}}\
              class B extends A{get v(){return super.v*2}}var b=new B(9),s=0;\
              for(var i=0;i<200000;i++){ if(i===100000) Object.setPrototypeOf(B.prototype,{get v(){return 1000}}); s+=b.v; }\
              out.push(s);})();\
             (function(){class A{constructor(x){this._v=x}get v(){return this._v}}\
              class B extends A{get v(){return super.v+1}}var b=new B(4),s=0;\
              for(var i=0;i<200000;i++){ if(i===100000) b._v=99; s+=b.v; }\
              out.push(s);})();\
             console.log(out.join('|'))",
            &["22000000|11600000|800000/100000|201800000|10500000"],
        );
    }

    #[test]
    fn super_getter_inline_preserves_values_and_effects() {
        // The inlined parent getter must be VALUE-transparent (`-0` survives,
        // a non-number passes through) and must actually RUN — a getter with a
        // side effect is not elidable, and a set-only parent accessor reads as
        // `undefined` rather than calling anything.
        assert_jit_matches(
            "var out=[];\
             (function(){class A{constructor(x){this._v=x}get v(){return this._v}}\
              class B extends A{get v(){return super.v}}var b=new B(0);\
              for(var i=0;i<200000;i++) b.v; b._v=-0; out.push(Object.is(b.v,-0));\
              b._v='s'; out.push(b.v);})();\
             (function(){var n=0;class A{constructor(x){this._v=x}get v(){n++;return this._v}}\
              class B extends A{get v(){return super.v*2}}var b=new B(1);\
              for(var i=0;i<200000;i++) b.v; out.push(n);})();\
             (function(){class A{set v(x){this._v=x}}class B extends A{get v(){return super.v}}\
              var b=new B(); for(var i=0;i<200000;i++) b.v; out.push(String(b.v));})();\
             console.log(out.join('|'))",
            &["true|s|200000|undefined"],
        );
    }

    #[test]
    fn super_setter_inline_invalidates() {
        // A class SETTER whose body is `super.v = x` now inlines the parent
        // setter (Stage 7). The arm's re-check reads `attrs[slot].setter` —
        // NOT `vals[slot]`, which holds the getter half — via the absolute
        // address `ic_super_setter_baked` bakes. Each case runs past the OSR
        // threshold and then attacks one dependency:
        //   1. swap ONLY the setter half in place (`defineProperty` keeping
        //      the getter): no version bump, no realloc — only the value
        //      compare on the baked address catches it.
        //   2. replace the parent accessor with a DATA property, so the write
        //      falls through to CreateDataProperty on the receiver.
        //   3. delete it (strict: still no throw — absent means own create),
        //      exercising the recursion fix in `reflect_set_on_receiver`.
        //   4. `setPrototypeOf` swaps the chain the hop guards watch.
        //   5. the parent setter's side effect must fire on every call.
        // Expectations computed with node (B51: never by hand), and
        // `assert_jit_matches` pins JIT == NOJIT first regardless.
        assert_jit_matches(
            "var out=[];\
             (function(){class A{constructor(){this._v=0}get v(){return this._v}set v(x){this._v=x}}\
              class B extends A{set v(x){super.v=x}get v(){return super.v}}var b=new B(),s=0;\
              for(var i=0;i<200000;i++){ if(i===100000) Object.defineProperty(A.prototype,'v',{get:Object.getOwnPropertyDescriptor(A.prototype,'v').get,set:function(x){this._v=x*3},configurable:true}); b.v=i; s=(s+b.v)|0; }\
              out.push(s);})();\
             (function(){class A{constructor(){this._v=0}get v(){return this._v}set v(x){this._v=x}}\
              class B extends A{set v(x){super.v=x}get v(){return this._v}}var b=new B(),s=0;\
              for(var i=0;i<200000;i++){ if(i===100000) Object.defineProperty(A.prototype,'v',{value:1,writable:true,configurable:true}); b.v=i; s=(s+b.v)|0; }\
              out.push(s+'/'+Object.prototype.hasOwnProperty.call(b,'v')+'/'+b._v);})();\
             (function(){class A{constructor(){this._v=0}set v(x){this._v=x}get v(){return this._v}}\
              class B extends A{set v(x){super.v=x}}var b=new B(),thrown=0;\
              for(var i=0;i<200000;i++){ if(i===100000) delete A.prototype.v; try{ b.v=i; }catch(e){ thrown++; } }\
              out.push(thrown+'/'+Object.prototype.hasOwnProperty.call(b,'v')+'/'+b.v+'/'+b._v);})();\
             (function(){class A{constructor(){this._v=0}set v(x){this._v=x}get v(){return this._v}}\
              class B extends A{set v(x){super.v=x}get v(){return this._w||this._v}}var b=new B(),s=0;\
              for(var i=0;i<200000;i++){ if(i===100000) Object.setPrototypeOf(B.prototype,{set v(x){this._w=x*7}}); b.v=3; }\
              out.push(b._w+'/'+b._v);})();\
             (function(){var n=0;class A{constructor(){this._v=0}set v(x){n++;this._v=x}get v(){return this._v}}\
              class B extends A{set v(x){super.v=x}}var b=new B();\
              for(var i=0;i<200000;i++) b.v=i; out.push(n);})();\
             console.log(out.join('|'))",
            &["-1539807552|-1474936480/true/99999|0/true/199999/99999|21/3|200000"],
        );
    }

    #[test]
    fn super_setter_inline_semantics() {
        // Value identity through the inlined store (`-0`, object identity), a
        // get-only parent (strict TypeError on EVERY call — the arm must not
        // bake), the assignment expression's value being the RHS rather than
        // the setter's return, and — the crash regression — `super.v = x`
        // with NO `v` anywhere on the super chain must CreateDataProperty on
        // the receiver rather than re-entering the receiver's own inherited
        // setter. That re-entry was unbounded recursion, and under
        // `panic = "abort"` it killed the process from two lines of JS
        // (`reflect_set_on_receiver` used a full [[Set]] where the spec has
        // an own define; found by this feature's soundness probe, present at
        // least since e38ebb3).
        assert_jit_matches(
            "var out=[];\
             (function(){class A{constructor(){this._v=1}set v(x){this._v=x}get v(){return this._v}}\
              class B extends A{set v(x){super.v=x}get v(){return super.v}}var b=new B();\
              for(var i=0;i<200000;i++) b.v=i; b.v=-0; out.push(Object.is(b.v,-0)); var o={t:1}; b.v=o; out.push(b.v===o);})();\
             (function(){class A{get v(){return 1}}class B extends A{set v(x){super.v=x}}\
              var b=new B(),t=0; for(var i=0;i<200000;i++){ try{ b.v=i; }catch(e){ t++; } } out.push(t);})();\
             (function(){class A{set v(x){this._v=x}}class B extends A{set v(x){super.v=x}}\
              var b=new B(); for(var i=0;i<200000;i++) b.v=i; out.push((b.v=77));})();\
             (function(){class A{}class B extends A{set v(x){super.v=x}}\
              var b=new B(); for(var i=0;i<200000;i++) b.v=i; b.v=6;\
              var d=Object.getOwnPropertyDescriptor(b,'v');\
              out.push(d.value+'/'+d.writable+'/'+d.enumerable+'/'+d.configurable);})();\
             console.log(out.join('|'))",
            &["true|true|200000|77|6/true/true/true"],
        );
    }

    #[test]
    fn proto_method_inline_matches_across_tiers() {
        // B78: the method inliner now bakes an arm for a receiver whose method
        // is INHERITED (`Object.create(proto)` / `Ctor.prototype.m = fn`),
        // which previously fell to the per-call helper on every iteration —
        // 29.5ns/call at one receiver against 5.5ns for the same method on a
        // class. Each case runs past the OSR threshold and then attacks one
        // guarded fact:
        //   1. reassign `P.m` in place — no version bump anywhere, so ONLY the
        //      `holder_vals_ptr[slot] == fn_bits` compare catches it.
        //   2. add an own SHADOW on the receiver (its version bumps).
        //   3. `setPrototypeOf` the receiver — the reason this arm may guard
        //      the first chain link by the receiver's version alone instead of
        //      re-reading `proto_of` the way `ic_chain_ok` does.
        //   4. an inherited ARROW must decline: inlining drops the captured
        //      `this_val` and would bind reg 0 to the receiver (999 or 3, not
        //      111) — the own-slot arm's `lexical_this` hazard, inherited.
        //   5. an inherited GETTER must decline and run on EVERY call.
        //   6. the pre-ES6 constructor-prototype shape, with an argument.
        // Expectations computed with node (B51: never by hand).
        assert_jit_matches(
            "var out=[];\
             (function(){var P={m:function(){return this.x+1}};var o=Object.create(P);o.x=41;var s=0;\
              for(var i=0;i<200000;i++){ if(i===100000) P.m=function(){return this.x*2}; s=(s+o.m())|0; }\
              out.push(s);})();\
             (function(){var P={m:function(){return 1}};var o=Object.create(P);var s=0;\
              for(var i=0;i<200000;i++){ if(i===100000) o.m=function(){return 7}; s=(s+o.m())|0; }\
              out.push(s);})();\
             (function(){var P={m:function(){return 1}};var Q={m:function(){return 2}};var o=Object.create(P);var s=0;\
              for(var i=0;i<200000;i++){ if(i===100000) Object.setPrototypeOf(o,Q); s=(s+o.m())|0; }\
              out.push(s);})();\
             (function(){var holder={f:111};var P={f:3,m:(function(){return ()=>this.f}).call(holder)};\
              var o=Object.create(P);o.f=999;var s=0;for(var i=0;i<200000;i++) s=o.m(); out.push(s);})();\
             (function(){var n=0;var P={};Object.defineProperty(P,'m',{get:function(){n++;return function(){return 9}}});\
              var o=Object.create(P);var s=0;for(var i=0;i<200000;i++) s=o.m(); out.push(s+'/'+n);})();\
             (function(){function K(i){this.v=i}K.prototype.m=function(a){return (this.v*2+a)|0};\
              var k=new K(21),s=0;for(var i=0;i<200000;i++) s=k.m(i&7); out.push(s);})();\
             console.log(out.join('|'))",
            &["12400000|800000|300000|111|9/200000|49"],
        );
    }

    #[test]
    fn set_index_concat_fusion_order() {
        // `o["k" + e] = v` fuses to ToConcatKey + SetIndexConcat. Case 1 is
        // the wrong answer a previous fusion shipped and reverted (B50): the
        // key's observable coercion must run BEFORE the RHS, where the `+`
        // sits. The rest: valueOf/@@toPrimitive on the key (default hint), a
        // Symbol key throwing before the RHS runs, a throwing toString
        // skipping the RHS, coercion mutating the receiver before the store,
        // string-suffix keys, `__proto__` (runs the inherited setter — node
        // semantics), frozen/non-extensible strict TypeErrors, an inherited
        // setter, new-key attributes and key order, a hot JIT loop hitting
        // existing keys with a mid-loop new-key deopt, and double/negative/
        // huge numeric key formatting. Expectations from node.
        assert_jit_matches(
            "'use strict';var out=[];\
             var log=[];var o1={};\
             function k1(){log.push('key');return {toString:function(){log.push('keyToString');return 'X';}};}\
             function v1(){log.push('val');return 7;}\
             o1['p'+k1()]=v1();\
             out.push(log.join(',')+'='+o1.pX);\
             var o2={};o2['n'+{valueOf:function(){return 42;}}]='v';out.push(Object.keys(o2)[0]);\
             var tp={};tp[Symbol.toPrimitive]=function(h){return 'H'+h;};\
             var o3={};o3['x'+tp]=1;out.push(Object.keys(o3)[0]);\
             var ran4=0,err4='';var o4={};\
             try{o4['s'+Symbol('q')]=(ran4++,5);}catch(e){err4=e.constructor.name;}\
             out.push(err4+'/'+ran4);\
             var ran5=0,o5={};\
             try{o5['t'+{toString:function(){throw new Error('boom');}}]=(ran5++,1);}catch(e){}\
             out.push('ran='+ran5);\
             var o6={};var mut={toString:function(){o6.kA='early';return 'A';}};\
             o6['k'+mut]='late';out.push(o6.kA);\
             var o7={};var suf='zz';for(var i=0;i<300;i++)o7['p'+suf]=i;out.push(o7.pzz);\
             var o8={};o8['__pro'+'to__']={t:1};\
             out.push((Object.getPrototypeOf(o8)===Object.prototype)+'/'+Object.keys(o8).length);\
             var o9=Object.freeze({q1:1});var err9='';\
             try{o9['q'+1]=5;}catch(e){err9=e.constructor.name;}out.push(err9+'/'+o9.q1);\
             var got10=null;var proto10={};\
             Object.defineProperty(proto10,'w3',{set:function(v){got10=v;}});\
             var o10=Object.create(proto10);o10['w'+3]=99;\
             out.push(got10+'/'+Object.prototype.hasOwnProperty.call(o10,'w3'));\
             var o11={z:1};o11['a'+0]=2;var d11=Object.getOwnPropertyDescriptor(o11,'a0');\
             out.push(d11.writable+d11.enumerable+d11.configurable+'/'+JSON.stringify(Object.keys(o11)));\
             var o12={};for(var i=0;i<8;i++)o12['h'+i]=0;\
             for(var i=0;i<300000;i++){o12['h'+(i&7)]=i;if(i===200000)o12['h9']=-1;}\
             out.push(o12.h0+'/'+o12.h7+'/'+o12.h9);\
             var o13=Object.preventExtensions({e0:1});var err13='';\
             try{o13['e'+9]=5;}catch(e){err13=e.constructor.name;}\
             out.push(err13+'/'+(o13.e9===undefined));\
             var o14={};o14['d'+1.5]=1;o14['d'+(-3)]=2;o14['d'+1e21]=3;\
             out.push(Object.keys(o14).join(';'));\
             console.log(out.join('|'))",
            &["key,keyToString,val=7|n42|xHdefault|TypeError/0|ran=0|late|299|false/0|TypeError/1|99/false|3/[\"z\",\"a0\"]|299992/299999/-1|TypeError/true|d1.5;d-3;d1e+21"],
        );
    }

    #[test]
    fn local_accumulator_inplace_aliasing() {
        // `out += x` on a FUNCTION-LOCAL accumulator now rewrites to
        // StrAppendInPlace when the register provably never leaks a second
        // live reference while appends can still run. Every case here is an
        // ALIASING ATTACK on that proof — a false positive mutates a string
        // the user holds:
        //   1. mid-loop escape (each snapshot a distinct prefix), 2. snapshot
        //   read later, 3. `out += out`, 4. closure capture, 5. eval reading
        //   the accumulator, 6. mid-loop reset, 7. two sibling loops,
        //   8. per-outer-iteration re-init, 9. accumulator LIVE ACROSS outer
        //   iterations (enclosed loop must decline), 10. try/catch in body,
        //   11. the hot escapeHtml shape itself run twice, 12. append via a
        //   helper-call result, 13. a generator yielding the accumulator.
        // Expectations from node.
        assert_jit_matches(
            "var out_=[];\
             function esc(n){var snaps=[];var out='';for(var i=0;i<n;i++){out+='x';snaps.push(out);}return snaps;}\
             out_.push(esc(5).join(',')==='x,xx,xxx,xxxx,xxxxx');\
             function snapread(n){var out='';var first=null;for(var i=0;i<n;i++){out+='a';if(i===0)first=out;}return first+'/'+out;}\
             out_.push(snapread(4));\
             function selfapp(n){var out='ab';for(var i=0;i<n;i++){out+=out;}return out.length+':'+out.slice(0,8);}\
             out_.push(selfapp(4));\
             function cap(n){var out='';var f=function(){return out;};for(var i=0;i<n;i++){out+='c';}return f()+'/'+out;}\
             out_.push(cap(3));\
             function ev(n){var out='';var seen='';for(var i=0;i<n;i++){out+='e';seen=eval('out');}return seen+'/'+out;}\
             out_.push(ev(3));\
             function reset(n){var out='';for(var i=0;i<n;i++){out+='r';if(i===2)out='R';}return out;}\
             out_.push(reset(5));\
             function sib(n){var out='';for(var i=0;i<n;i++)out+='1';var mid=out;for(var j=0;j<n;j++)out+='2';return mid+'/'+out;}\
             out_.push(sib(3));\
             function nest(n){var keep=[];for(var o=0;o<n;o++){var out='';for(var i=0;i<=o;i++)out+='n';keep.push(out);}return keep.join(',');}\
             out_.push(nest(3));\
             function nest2(n){var keep=[];var out='';for(var o=0;o<n;o++){for(var i=0;i<2;i++)out+=o;keep.push(out);}return keep.join(',');}\
             out_.push(nest2(3));\
             function tc(n){var out='';for(var i=0;i<n;i++){try{out+='t';}catch(e){}}return out;}\
             out_.push(tc(4));\
             function render(s){var out='';for(var i=0;i<s.length;i++){var c=s.charCodeAt(i);if(c===60)out+='&lt;';else if(c===62)out+='&gt;';else if(c===38)out+='&amp;';else out+=s[i];}return out;}\
             var big='';for(var i=0;i<50000;i++)big+=String.fromCharCode(97+(i%26),i%7===0?60:98);\
             var r1=render(big);var r2=render(big);\
             out_.push(r1.length+'/'+(r1===r2)+'/'+r1.slice(0,12));\
             function up(c){return c.toUpperCase();}\
             function callin(s){var out='';for(var i=0;i<s.length;i++){out+=up(s[i]);}return out;}\
             out_.push(callin('abc'));\
             function* gen(n){var out='';for(var i=0;i<n;i++){out+='g';yield out;}}\
             var g=[];for(var v of gen(3))g.push(v);\
             out_.push(g.join(','));\
             console.log(out_.join('|'))",
            &["true|a/aaaa|32:abababab|ccc/ccc|eee/eee|Rrr|111/111222|n,nn,nnn|00,0011,001122|tttt|121429/true/a&lt;bbcbdbe|ABC|g,gg,ggg"],
        );
    }

    #[test]
    fn static_fn_region_promise_resolve() {
        // `a[j] = Promise.resolve(j)` in a hot loop now compiles (StaticFn was
        // the region's only blocker). The helper's fast path is non-heap
        // arguments ONLY; everything observable must still go through the
        // interpreter: identity for an existing promise (constructor check),
        // thenable adoption running user `then`, and resolution values.
        // Expectations from node.
        assert_jit_matches(
            "var order=[];\
             async function run(){\
               var a=new Array(1000);\
               for(var j=0;j<1000;j++) a[j]=Promise.resolve(j);\
               var s=0; for(var j=0;j<1000;j++) s+=await a[j];\
               order.push('sum='+s);\
               var p0=Promise.resolve(42);\
               order.push('identity='+(Promise.resolve(p0)===p0));\
               var thenable={then:function(res){order.push('thenable');res(7);}};\
               order.push('adopted='+(await Promise.resolve(thenable)));\
               var q=Promise.resolve(1); order.push('chain='+(await q.then(function(x){return x+1;})));\
             }\
             run().then(function(){ console.log(order.join('|')); })",
            &["sum=499500|identity=true|thenable|adopted=7|chain=2"],
        );
    }

    #[test]
    fn typeof_is_fusion_semantics() {
        // `typeof x === "lit"` fuses to TypeOfIs (no heap string, no Eq). Pins:
        // a hot mixed-type JIT loop over all polarities; the undeclared-global
        // non-throwing read surviving the fusion; a never-match literal still
        // evaluating its operand's side effects; null ("object"), bigint,
        // symbol. Expectations from node.
        assert_jit_matches(
            "var out=[];\
             (function(){var arr=[1,'a',true,{},undefined,function(){}]; var c=0;\
              for(var i=0;i<600000;i++){ var v=arr[i%6];\
               if(typeof v==='number') c+=1; else if(typeof v==='string') c+=2;\
               else if(typeof v!=='object') c+=3; else c+=4; }\
              out.push(c);})();\
             (function(){out.push(typeof notDeclaredXyz==='undefined', typeof notDeclaredXyz!=='undefined');})();\
             (function(){var n=0; function eff(){n++;return 1;}\
              for(var i=0;i<50000;i++){ if(typeof eff()==='nonsense') out.push('no'); } out.push(n);})();\
             (function(){out.push(typeof null==='object', typeof 10n==='bigint', typeof Symbol()==='symbol');})();\
             console.log(out.join('|'))",
            &["1600000|true|false|50000|true|true|true"],
        );
    }

    #[test]
    fn topropkey_regalloc_key_semantics() {
        // `x[i] *= v` emits ToPropKey, which the regalloc (f64) tier now
        // compiles as a register copy — sound ONLY because a numeric key is
        // ToPropertyKey's identity. These pin the cases where a copy and the
        // real coercion diverge: a FRACTIONAL key must stay a miss (never
        // truncate to an element), NaN likewise, and `-0` is element 0. The
        // hot f64 read-modify-write is case 1. Expectations from node.
        assert_jit_matches(
            "var out=[];\
             (function(){var x=new Float64Array(64); for(var i=0;i<64;i++) x[i]=i+0.5; var s=0;\
              for(var r=0;r<20000;r++){ for(var i=0;i<64;i++){ x[i]*=1.000001; s+=x[i]; } }\
              out.push(s.toFixed(2));})();\
             (function(){var y=new Float64Array(4); y[1]=2;\
              for(var r=0;r<200000;r++){ y[1.5]=(y[1.5]||0)+1; } out.push(y[1]+'/'+(y[1.5]===undefined));})();\
             (function(){var w=new Float64Array(2); w[0]=5;\
              for(var r=0;r<200000;r++){ w[NaN]=(w[NaN]||0)+1; } out.push(w[0]+'/'+(w[NaN]===undefined));})();\
             (function(){var v=new Float64Array(2); v[0]=3;\
              for(var r=0;r<200000;r++){ v[-0]+=1; } out.push(v[0]);})();\
             console.log(out.join('|'))",
            &["41372364.85|2/true|5/true|200003"],
        );
    }

    // ── INT live-in interval contract ─────────────────────────────────────────
    // `emit_int_entry_load` admits an Int-tagged value OR a double holding an
    // exact integer in [-2^53, 2^53], so `plan_region` must seed the interval
    // analysis with IV_FULL for live-ins. It used to seed IV_I32 (correct only
    // while the load was Int-tag-only), and the analysis then elided 2^53
    // overflow guards — and strength-reduced multiplies to `psllq` — on values
    // that were never i32. `x * 1024` entered with x = 2^53 shifted into the
    // i64 sign bit: -2^63 instead of +2^63.
    //
    // Each of these runs a loop-CARRIED live-in (read before written in the
    // region, so it is a genuine entry-guarded live-in) whose true value is far
    // outside i32, through an operation whose i64 result overflows.

    #[test]
    fn int_live_in_mul_pow2_at_2p53() {
        // 2^53 * 1024 == 2^63, which does not fit i64: the guard must survive.
        assert_jit_matches(
            "var x=9007199254740992,o=0; for(var i=0;i<300;i++){ o=x*1024; x=x-0; } console.log(o)",
            &["9223372036854776000"],
        );
    }

    #[test]
    fn int_live_in_mul_pow2_negative_at_2p53() {
        assert_jit_matches(
            "var x=-9007199254740992,o=0; for(var i=0;i<300;i++){ o=x*1024; x=x-0; } console.log(o)",
            &["-9223372036854776000"],
        );
    }

    #[test]
    fn int_live_in_mul_2048_at_2p53() {
        // 2^53 * 2048 == 2^64 — a full i64 wrap to 0 if the guard is elided.
        assert_jit_matches(
            "var x=9007199254740992,o=0; for(var i=0;i<300;i++){ o=x*2048; x=x-0; } console.log(o)",
            &["18446744073709552000"],
        );
    }

    #[test]
    fn int_live_in_non_pow2_mul_at_2p53() {
        // Not a power of two, so no psllq — exercises the plain guard path.
        assert_jit_matches(
            "var x=9007199254740992,o=0; for(var i=0;i<300;i++){ o=x*3; x=x-0; } console.log(o)",
            &["27021597764222976"],
        );
    }

    #[test]
    fn int_live_in_add_at_2p53() {
        assert_jit_matches(
            "var x=9007199254740992,o=0; for(var i=0;i<300;i++){ o=x+x; x=x-0; } console.log(o)",
            &["18014398509481984"],
        );
    }

    #[test]
    fn int_live_in_roundtrip_identity_at_2p53() {
        // (x + 1) - 1 at 2^53: JS rounds x+1 back to 2^53, so the answer is 2^53
        // and NOT 2^53 - 1. An i64 home computes the unrounded value, which is
        // why the guard has to fire here.
        assert_jit_matches(
            "var x=9007199254740992,o=0; for(var i=0;i<300;i++){ o=(x+1)-1; x=x-0; } console.log(o)",
            &["9007199254740991"],
        );
    }

    #[test]
    fn int_live_in_accumulator_crosses_i32() {
        // The shape the widened entry load exists FOR: a nested loop whose
        // accumulator leaves i32, so the inner region must re-enter with a
        // double live-in rather than deopt to eviction.
        assert_jit_matches(
            "var a=[]; for(var i=0;i<2000;i++)a.push(i*1000);              var s=0; for(var k=0;k<40;k++){ for(var i=0;i<a.length;i++) s+=a[i]; } console.log(s)",
            &["79960000000"],
        );
    }

    #[test]
    fn int_live_in_negative_zero_survives_entry() {
        // -0 cannot live in an i64 home (ucomisd reports -0.0 == +0.0, so the
        // entry round-trip would accept it and box back +0). A zero-iteration
        // inner loop must leave the accumulator as -0.
        assert_jit_matches(
            "var n=0,s=-0; for(var k=0;k<300;k++){ for(var i=0;i<n;i++) s+=1; } console.log(1/s)",
            &["-Infinity"],
        );
    }

    // ── early-exit flush soundness ────────────────────────────────────────────
    // A trip count of exactly OSR_THRESHOLD (8) compiles the region on the final
    // back-edge, so it is ENTERED and then exits with zero body iterations. Every
    // home `flush_exit` writes back must therefore already hold the register's
    // real value at entry — otherwise the flush silently overwrites correct
    // interpreter state with whatever was in the xmm/gpr. Each of these returned
    // a wrong answer (not a deopt) before the entry loads covered def-first regs,
    // bool gprs and def-first globals.

    #[test]
    fn early_exit_flush_def_first_move() {
        // Returned 8 (the loop counter's value, via a unified home) instead of 7.
        assert_jit_matches("var s=999; for(var i=0;i<8;i++){ s=i; } console.log(s)", &["7"]);
    }

    #[test]
    fn early_exit_flush_def_first_arith() {
        // Returned 0 — an xmm home that was never loaded and never written.
        assert_jit_matches("var s=999; for(var i=0;i<8;i++){ s=(i*3)|0|0; } console.log(s)", &["21"]);
    }

    #[test]
    fn early_exit_flush_conditional_def() {
        // The def never runs, so `s` must keep its pre-loop value. Returned 8.
        assert_jit_matches(
            "var s=5; for(var i=0;i<8;i++){ if(i>100){ s=i; } } console.log(s)",
            &["5"],
        );
    }

    #[test]
    fn early_exit_flush_def_first_global() {
        // A def-first GLOBAL flushed an uninitialised xmm as a double, printing a
        // raw bit pattern (4626604192193053000) instead of 14.
        assert_jit_matches(
            "var g=42; (function(){ for(var i=0;i<8;i++){ g=i*2; } })(); console.log(g)",
            &["14"],
        );
    }

    #[test]
    fn early_exit_flush_bool_home() {
        // Bool homes live in gprs the prologue never initialised, so the flush
        // boxed whatever the register happened to hold into a Bool.
        assert_jit_matches(
            "var b='keep'; for(var i=0;i<8;i++){ b=i<100; } console.log(b)",
            &["true"],
        );
    }

    // ── speculative prologue work must be guarded by "does it actually run" ──
    // The prologue materialises hoisted constants and the hoisted `arr.length`
    // BEFORE the body, and elides the body op that would have produced them. If
    // that op sits on a branch the loop never takes, the prologue value is pure
    // invention: it is flushed over the register's real value, and reads inside
    // the region see it too. `runs_every_iteration` is what gates this now.

    #[test]
    fn hoisted_const_on_untaken_branch() {
        // `c`'s only def is inside a branch that never runs; it must stay 3.
        assert_jit_matches(
            "function f(){ let s=0, c=3; for (let i=0;i<200000;i++){ if (i>1e9) { c=7; s+=c; } s+=i; } return c; } console.log(f())",
            &["3"],
        );
    }

    #[test]
    fn hoisted_const_on_untaken_branch_double() {
        // Same shape on the f64 regalloc tier (fractional constants).
        assert_jit_matches(
            "function f(){ let s=0.5, c=3.5; for (let i=0;i<200000;i++){ if (i>1e9) { c=7.5; s+=c; } s+=i; } return c; } console.log(f())",
            &["3.5"],
        );
    }

    #[test]
    fn hoisted_length_on_untaken_branch() {
        // The memory tier hoists `arr.length` straight into the register file.
        assert_jit_matches(
            "var arr=[1,2,3,4,5,6,7]; function f(){ let n=99,s=0; for (let i=0;i<200000;i++){ if (i>1e9) { n=arr.length; } s+=i; } return n; } console.log(f())",
            &["99"],
        );
    }

    #[test]
    fn home_reuse_does_not_clobber_early_locals() {
        // >14 numeric values put the planner on the home-reuse path, where one
        // xmm backed several registers and the exit flush wrote it to ALL their
        // slots — so locals assigned early came back holding a later temp.
        let mut src = String::from("function f(){ let a1=0,a2=0,a3=0,s=0;\n for (let i=0;i<200000;i++){ a1=i; a2=a1+a1; a3=a2+a1;\n let c0=a3+a2;\n");
        for k in 1..=20 {
            src.push_str(&format!("let c{k}=c{}+{};\n", k - 1, k));
        }
        src.push_str(" s+=c20; }\n return a1+' '+a2+' '+a3; } console.log(f())");
        assert_jit_matches(&src, &["199999 399998 599997"]);
    }

    // ── inlining closures that capture variables ─────────────────────────────
    // Leaf inlining used to decline any callee that captured upvalues, so every
    // closure-over-a-variable paid a full call. Reads are baked to their cell;
    // writes are BUFFERED in a scratch register and committed once after the
    // body, which is what keeps a mid-body bail idempotent.

    #[test]
    fn inline_closure_reads_upvalue() {
        assert_jit_matches(
            "function mk(){ var u=3; return function(x){ return (x*u)|0; }; } \
             var c=mk(), s=0; for (var i=0;i<200;i++) s=(s+c(i))|0; console.log(s)",
            &["59700"],
        );
    }

    #[test]
    fn inline_closure_write_is_visible_outside() {
        // The buffered write must be committed to the real cell, not just held in
        // the scratch window — a second closure over the same binding reads it.
        assert_jit_matches(
            "function mk(){ var v=0; return [function(){ v=(v+2)|0; return v; }, function(){ return v; }]; } \
             var p=mk(), a=p[0], b=p[1], s=0; for (var i=0;i<200;i++){ a(); s=(s+b())|0; } console.log(s)",
            &["40200"],
        );
    }

    #[test]
    fn inline_closure_write_before_deopt_capable_ops() {
        // mulberry32: the upvalue write comes FIRST and is followed by Math.imul
        // and shifts, all of which can bail. Buffering is what makes this legal.
        assert_jit_matches(
            "function mk(seed){ var a=seed|0; return function(){ a=(a+0x6D2B79F5)|0; \
             var t=Math.imul(a^(a>>>15),1|a); t=(t+Math.imul(t^(t>>>7),61|t))^t; \
             return ((t^(t>>>14))>>>0)/4294967296; }; } \
             var r=mk(0x10C5CAB), s=0; for (var i=0;i<5000;i++) s+=r(); console.log(s.toFixed(6))",
            &["2483.497902"],
        );
    }

    #[test]
    fn inline_closure_bail_after_write_is_not_double_applied() {
        // A late non-numeric argument bails the inlined body AFTER the upvalue
        // write. Because the write is still buffered, re-running the call from the
        // call ip applies the increment exactly once.
        assert_jit_matches(
            "function mk(){ var n=0; return function(x){ n=(n+1)|0; return (x*2)|0; }; } \
             var c=mk(), s=0; for (var i=0;i<200;i++){ s=(s+c(i<150 ? i : '7'))|0; } console.log(s)",
            &["23050"],
        );
    }

    // ── numeric edge cases the register tiers used to get wrong ──────────────

    #[test]
    fn dce_keeps_a_register_read_after_the_region() {
        // `dead` was "never read IN THE REGION", so the store was skipped and the
        // frame kept the last interpreted value. Returned 7 instead of 39.
        assert_jit_matches(
            "function f(){ for (var i=0;i<40;i++) { var q = i; } return q; } console.log(f())",
            &["39"],
        );
    }

    #[test]
    fn negate_zero_keeps_the_sign() {
        // Negation was `0.0 - x`, and `0.0 - 0.0` is `+0.0` under round-to-nearest.
        assert_jit_matches(
            "var z=0, r=1; for (var i=0;i<20;i++) { r = 1/(-z); } console.log(r)",
            &["-Infinity"],
        );
    }

    #[test]
    fn negate_zero_is_observable_in_an_int_region() {
        // The literal `-0` lowers to `LoadInt 0; Neg`, and an i64 home cannot hold
        // -0 — the int path must bail rather than produce integer 0.
        assert_jit_matches(
            "var v=0; for (var i=0;i<10;i++){ v = Object.is(-0,-0) ? 1 : 2; } console.log(v)",
            &["1"],
        );
    }

    #[test]
    fn mod_of_nan_is_nan_not_an_integer() {
        // `ucomisd` leaves NaN UNORDERED, so the `jne` integer-valued guard fell
        // through and ran idiv on cvttsd2si's i64::MIN. Returned 0.
        assert_jit_matches(
            "var r=0, x=NaN, d=0; for (var i=0;i<20;i++) { d=i/2; r = x % 1; } console.log(r)",
            &["NaN"],
        );
    }

    #[test]
    fn mod_of_nan_by_minus_one_does_not_crash() {
        // Same missing guard, but i64::MIN / -1 overflows the quotient and raised
        // #DE — this aborted the process rather than returning a wrong answer.
        assert_jit_matches(
            "var r=0, x=NaN, d=0; for (var i=0;i<20;i++) { d=i/2; r = x % -1; } console.log(r)",
            &["NaN"],
        );
    }

    #[test]
    fn mod_with_negative_dividend_and_zero_remainder_is_negative_zero() {
        // `-20 % 5` is -0 in JS, which has no integer home: the int/idiv paths
        // must bail instead of boxing Int(0).
        assert_jit_matches(
            "var r=1; for (var i=0;i<20;i++) { r = 1/(-20 % 5); } console.log(r)",
            &["-Infinity"],
        );
    }

    #[test]
    fn early_exit_flush_pinned_string() {
        // Same defect reached through the pinned-string charCodeAt path.
        assert_jit_matches(
            "var s='ab',e=777; for(var m=0;m<8;m++){ e=(s.charCodeAt(0)^0)|0; } console.log(e)",
            &["97"],
        );
    }

    #[test]
    fn int_region_crosses_i32() {
        // sum 0..99999 = 4999950000 > 2^31 — value stays i64 in the loop, flushes
        // as a DOUBLE (since >i32) and must render identically to the interpreter.
        assert_jit_matches("let s=0; for(let i=0;i<100000;i++){ s+=i; } console.log(s)", &["4999950000"]);
    }

    #[test]
    fn int_region_countdown_and_compare() {
        // Decrement with an interior comparison/conditional (exercises bool homes).
        assert_jit_matches(
            "let i=100000; let c=0; while(i>0){ i=i-1; if(i<50000){ c=c+1; } } console.log(c)",
            &["50000"],
        );
    }

    #[test]
    fn int_region_overflow_bail_powers_of_two() {
        // Doubling: s reaches 2^53 then the per-op 2^53 guard bails to the
        // interpreter; results stay exact (powers of two are representable in f64),
        // so JIT-on must equal JIT-off. 2^60's shortest-round-trip form (== node).
        assert_jit_matches(
            "let s=1; for(let i=0;i<60;i++){ s=s+s; } console.log(s)",
            &["1152921504606847000"],
        );
    }

    #[test]
    fn int_region_negative_start_and_loop_var() {
        // Negative live-in and accumulation across zero.
        assert_jit_matches(
            "let s=-1000; for(let i=0;i<2000;i++){ s=s+1; } console.log(s)",
            &["1000"],
        );
    }

    #[test]
    fn int_region_strict_eq_ne() {
        // === and !== as integer comparisons producing bool homes.
        assert_jit_matches(
            "let c=0; for(let i=0;i<1000;i++){ if(i===500){c=c+7;} } console.log(c)",
            &["7"],
        );
        assert_jit_matches(
            "let c=0; for(let i=0;i<1000;i++){ if(i!==0){c=c+1;} } console.log(c)",
            &["999"],
        );
    }

    #[test]
    fn int_region_multi_var_and_bounds() {
        // Several live integer vars + a mix of < and > guards; result spans i32.
        assert_jit_matches(
            "let a=0; let b=1000000; for(let i=0;i<500000;i++){ a=a+2; b=b-1; } console.log(a, b)",
            &["1000000 500000"],
        );
    }

    #[test]
    fn int_region_overflow_nonrepresentable_fibonacci() {
        // Fibonacci grows past 2^53 into integers NOT exactly representable in f64
        // (unlike powers of two), so the int path MUST bail at 2^53 and let the
        // interpreter continue in rounded f64 — JIT-on must equal JIT-off, and
        // both must equal node's value (4660046610375530000 = fib(91) as f64,
        // shortest-round-trip). This is the case the verifier flagged: a value is
        // written, overflows, and must still be flushed correctly.
        assert_jit_matches(
            "let a=0; let b=1; let t=0; for(let i=0;i<90;i++){ t=a+b; a=b; b=t; } console.log(b)",
            &["4660046610375530000"],
        );
    }

    #[test]
    fn heap_region_object_prop_get_set() {
        // GetProp/SetProp in a hot loop (the object.js shape): o.a=i; o.b=o.a+1;
        // s+=o.b. sum of (i+1) for i in 0..999 = sum 1..1000 = 500500.
        assert_jit_matches(
            "let o={a:0,b:0}; let s=0; for(let i=0;i<1000;i++){ o.a=i; o.b=o.a+1; s+=o.b; } console.log(s)",
            &["500500"],
        );
    }

    #[test]
    fn heap_region_object_read_only_and_mul() {
        // Read a stable property each iteration + Mul (forces the double/mem path,
        // not int64). o.k*3 summed: 3*7*2000 = 42000.
        assert_jit_matches(
            "let o={k:7}; let s=0; for(let i=0;i<2000;i++){ s += o.k*3; } console.log(s)",
            &["42000"],
        );
    }

    #[test]
    fn int_region_multiply() {
        // i*i in the int64 path (imul). sum_{i<10000} i^2 = (n-1)n(2n-1)/6, n=10000.
        assert_jit_matches(
            "let s=0; for(let i=0;i<10000;i++){ s += i*i; } console.log(s)",
            &["333283335000"],
        );
    }

    #[test]
    fn int_region_multiply_overflow_bails() {
        // Repeated doubling via multiply crosses 2^53 → the per-op guard bails to
        // the interpreter; powers of two stay exact, so JIT-on == JIT-off == node.
        // 2^60 = 1152921504606847000 (shortest round-trip).
        assert_jit_matches(
            "let p=1; for(let i=0;i<60;i++){ p=p*2; } console.log(p)",
            &["1152921504606847000"],
        );
    }

    #[test]
    fn object_sroa_full_chain_int_mul() {
        // The object.js chain — exercises object scalar-replacement + int64 Mul
        // (o.c = o.b*2). s = sum 2*(i+1) for i in 0..999 = 1001000. (Also covered by
        // heap_region_object_full_chain, but at the scale that triggers SROA.)
        assert_jit_matches(
            "let o={a:0,b:0,c:0}; let s=0; for(let i=0;i<5000;i++){ o.a=i; o.b=o.a+1; o.c=o.b*2; s+=o.c; } console.log(s)",
            &["25005000"],
        );
    }

    #[test]
    fn json_stringify_buffer_append() {
        // The single-buffer SerializeJSONProperty path: omitted object props (incl.
        // their comma) vanish, omitted array elements become null, empty/nested
        // containers and escaping are unchanged. (test262's JSON/stringify suite is
        // the exhaustive guard; this is a fast smoke test.)
        assert_eq!(
            run_ok("console.log(JSON.stringify({a:1,b:undefined,c:[1,undefined,3],d:{},e:function(){}}))"),
            vec![r#"{"a":1,"c":[1,null,3],"d":{}}"#.to_string()]
        );
        assert_eq!(
            run_ok("console.log(JSON.stringify({a:1,b:[2]},null,2))"),
            vec!["{\n  \"a\": 1,\n  \"b\": [\n    2\n  ]\n}".to_string()]
        );
        assert_eq!(
            run_ok(r#"console.log(JSON.stringify({x:"a\nb\t\"q\""}))"#),
            vec![r#"{"x":"a\nb\t\"q\""}"#.to_string()]
        );
    }

    #[test]
    fn region_math_imul_inline() {
        // Math.imul emitted INLINE in a region (native 32-bit imul, no FFI) must
        // be byte-identical to the helper path across overflow, negatives, double
        // truncation, NaN, and a hot accumulating loop. Cross-checked vs node.
        assert_jit_matches(
            "let h=0x811c9dc5|0; for(let i=0;i<100000;i++){ h=Math.imul(h^i,16777619); } \
             let a=0; for(let i=0;i<50000;i++){ a=(a+Math.imul(-1,-1)+Math.imul(0x7FFFFFFF,2)+Math.imul(2.9,5)+Math.imul(NaN,5)+Math.imul(i,-3))|0; } \
             console.log((h>>>0)+' '+a)",
            &["3686109669 545492296"],
        );
    }

    #[test]
    fn fused_computed_index_concat_key() {
        // GetIndexConcat / SetIndexConcat / DeleteIndexConcat: the `obj["k" + i]`
        // map-key idiom must be byte-identical to the unfused path. Covers a plain
        // get/set/delete/re-add cycle (1), numeric-string-prefix keys treated as
        // named props on a plain object (2), an ACCESSOR-named computed key whose
        // getter/setter must still fire via the fallback (3 → calls/hidden), and a
        // non-int float key falling back to a real concat (4). Expected values
        // cross-checked against node.
        assert_jit_matches(
            "let o={},s=0; \
             for(let i=0;i<50;i++) o['k'+i]=i*2; \
             for(let i=0;i<50;i+=2) delete o['k'+i]; \
             for(let i=0;i<50;i+=2) o['k'+i]=i*3; \
             for(let i=0;i<50;i++) s+=(o['k'+i]||0); \
             let n={},s2=0; \
             for(let i=0;i<10;i++){ n[''+i]=i; n['1'+i]=i+100; } \
             for(let i=0;i<10;i++) s2+=n[''+i]+n['1'+i]; \
             let calls=0,hidden=0,a={}; \
             Object.defineProperty(a,'v0',{get(){calls++;return hidden;},set(x){calls++;hidden=x+1;},configurable:true}); \
             for(let i=0;i<5;i++){ a['v'+i]=i; } \
             let s3=a['v'+0]; \
             let f={}; f['p'+1.5]=7; let s4=f['p'+1.5]; \
             console.log(s, s2, s3, calls, hidden, s4, a.v1, a.v4)",
            &["3050 1090 1 2 1 7 1 4"],
        );
    }

    #[test]
    fn sroa_declines_class_accessor() {
        // SROA must NOT scalar-replace a CLASS getter/setter on a stable global:
        // the accessor lives on the prototype, so bypassing it (running the getter
        // / setter only at region boundaries) drops side effects and reads stale
        // values. Each iter runs set(7)→backing=70 and get→70, so s+=70 and
        // calls+=2. (Regression for sroa-accessor-miscompile, the inherited case.)
        assert_jit_matches(
            "let backing=0,calls=0; class B{get v(){calls++;return backing;} set v(x){calls++;backing=x*10;}} \
             let o=new B(); let s=0; for(let i=0;i<5000;i++){ o.v=7; s+=o.v; } console.log(s,calls,backing)",
            &["350000 10000 70"],
        );
    }

    #[test]
    fn sroa_declines_own_defineproperty_accessor() {
        // Same, for an OWN accessor installed by Object.defineProperty on a stable
        // global plain object: it occupies a real own slot but is an accessor, so
        // SROA must decline. (Regression for sroa-accessor-miscompile, own case.)
        assert_jit_matches(
            "let bk=0,c=0; let p={}; \
             Object.defineProperty(p,'v',{get(){c++;return bk;},set(x){c++;bk=x*10;},configurable:true}); \
             let s=0; for(let i=0;i<5000;i++){ p.v=7; s+=p.v; } console.log(s,c,bk)",
            &["350000 10000 70"],
        );
    }

    #[test]
    fn sroa_bails_on_field_redefined_as_accessor() {
        // Deeper guard: a plain DATA field is SROA-compiled in a hot loop, then
        // redefined as an accessor (constant getter, no-op setter), then the SAME
        // loop re-runs. The region's entry-time shape/version guard must detect the
        // change and fall back to the interpreter, honouring the accessor: each
        // iter reads 5, +1=6, set(6) is a no-op, reads 5 → s+=5, so s==25000.
        // (Regression for sroa-accessor-miscompile, the shape-change-after-compile
        // second bug.)
        assert_jit_matches(
            "let g={a:0}; let s=0; function hot(n){ for(let i=0;i<n;i++){ g.a=g.a+1; s+=g.a; } } \
             hot(5000); \
             Object.defineProperty(g,'a',{get(){return 5;},set(x){},configurable:true}); \
             s=0; hot(5000); console.log(s)",
            &["25000"],
        );
    }

    #[test]
    fn regalloc_linear_scan_reuse_many_values() {
        // A loop with far more numeric values (~33) than the 14-home pool, forcing
        // linear-scan home REUSE. Hoisted constants (1..16) must keep permanent
        // homes (a reused home would clobber them — a real bug this guards).
        // s = sum_{i<100000} sum_{k=1..16}(i+k) = 16*sum(i) + 136*100000.
        assert_jit_matches(
            "let s=0; for(let i=0;i<100000;i++){ s += (i+1)+(i+2)+(i+3)+(i+4)+(i+5)+(i+6)+(i+7)+(i+8)+(i+9)+(i+10)+(i+11)+(i+12)+(i+13)+(i+14)+(i+15)+(i+16); } console.log(s)",
            &["80012800000"],
        );
    }

    #[test]
    fn heap_region_setprop_on_array_noops() {
        // Setting a property on an ARRAY is a silent no-op in this engine (only
        // plain Objects store props) — the JIT must match the interpreter (return
        // success, NOT deopt-churn). The loop stays JIT'd; s = sum 0..999 = 499500.
        assert_jit_matches(
            "let a=[]; let s=0; for(let i=0;i<1000;i++){ a.x=i; s+=i; } console.log(s)",
            &["499500"],
        );
    }

    #[test]
    fn heap_region_object_full_chain() {
        // The exact object.js chain at smaller scale: o.a=i; o.b=o.a+1; o.c=o.b*2;
        // s+=o.c. s = sum 2*(i+1) for i in 0..999 = 2*(1+..+1000) = 1001000.
        assert_jit_matches(
            "let o={a:0,b:0,c:0}; let s=0; for(let i=0;i<1000;i++){ o.a=i; o.b=o.a+1; o.c=o.b*2; s+=o.c; } console.log(s)",
            &["1001000"],
        );
    }

    #[test]
    fn large_whole_double_uses_shortest_roundtrip() {
        // JS Number→String prints the shortest decimal that round-trips, and a
        // whole double above i64::MAX must not overflow `as i64`.
        assert_eq!(run_ok("console.log(4660046610375530496)"), vec!["4660046610375530000"]);
        assert_eq!(run_ok("console.log(1e20)"), vec!["100000000000000000000"]);
        assert_eq!(run_ok("console.log(1e19)"), vec!["10000000000000000000"]);
    }

    #[test]
    fn for_loop() {
        assert_eq!(
            run_ok("let s = 0; for (let i = 0; i < 5; i++) { s += i } console.log(s)"),
            vec!["10"]
        );
    }

    #[test]
    fn function_call() {
        assert_eq!(
            run_ok("function add(a, b) { return a + b } console.log(add(3, 4))"),
            vec!["7"]
        );
    }

    #[test]
    fn recursion_fib() {
        assert_eq!(
            run_ok("function fib(n){ return n < 2 ? n : fib(n-1) + fib(n-2) } console.log(fib(10))"),
            vec!["55"]
        );
    }

    #[test]
    fn recursion_is_bounded_not_segfault() {
        // Deeply recursive with no base case reached in bounds → catchable
        // RangeError, NOT a crash.
        let out = run("function r(n){ return r(n+1) } r(0)").expect("compile");
        assert!(out.error.is_some());
        assert!(
            out.error.as_ref().unwrap().contains("Maximum call stack"),
            "expected RangeError, got {:?}",
            out.error
        );
    }

    #[test]
    fn ternary_and_logical() {
        assert_eq!(run_ok("console.log(1 < 2 ? 'a' : 'b')"), vec!["a"]);
        assert_eq!(run_ok("console.log(0 || 'fallback')"), vec!["fallback"]);
        assert_eq!(run_ok("console.log(1 && 2)"), vec!["2"]);
    }

    #[test]
    fn string_concat() {
        assert_eq!(run_ok("console.log('a' + 'b' + 'c')"), vec!["abc"]);
        assert_eq!(run_ok("console.log('n=' + 42)"), vec!["n=42"]);
    }

    // ── Stage 1: reference types ──

    #[test]
    fn array_literal_and_index() {
        assert_eq!(run_ok("let a = [10, 20, 30]; console.log(a[0], a[1], a[2])"), vec!["10 20 30"]);
        assert_eq!(run_ok("let a = [1,2,3]; console.log(a.length)"), vec!["3"]);
        assert_eq!(run_ok("let a = [1,2,3]; a[1] = 99; console.log(a[1])"), vec!["99"]);
    }

    #[test]
    fn a_parenthesized_receiver_still_guards_the_callee_get() {
        // EvaluateCall does `func = GetValue(ref)` BEFORE ArgumentListEvaluation,
        // so `(o.a).m(arg())` must throw on the `.m` get of a nullish `o.a`
        // without ever calling `arg()`.
        //
        // This regressed silently for years because the check was gated on the
        // receiver being one of the parser's member-expression node kinds, and a
        // PARENTHESIZED member was not one of them — so wrapping the receiver in
        // parentheses quietly disabled it. The AST has no parenthesized-expression
        // node, which is what closed it.
        assert_eq!(
            run_ok(
                "var evaluated = false;                  function arg() { evaluated = true; return 1; }                  var o = { a: null };                  try { (o.a).m(arg()); } catch (e) {}                  console.log(evaluated)"
            ),
            vec!["false"]
        );
        // Unparenthesized was always correct; it must stay that way.
        assert_eq!(
            run_ok(
                "var evaluated = false;                  function arg() { evaluated = true; return 1; }                  var o = { a: null };                  try { o.a.m(arg()); } catch (e) {}                  console.log(evaluated)"
            ),
            vec!["false"]
        );
    }

    #[test]
    fn front_parse_script_raises_early_errors() {
        assert!(crate::front::parse_script("let x; let x;").is_err(), "dup lexical");
        assert!(crate::front::parse_auto("let x; let x;").is_err(), "dup via auto");
        assert!(run("let x; let x;").is_err(), "dup via run()");
    }

    #[test]
    fn spreading_an_array_densifies_its_holes() {
        // The array iterator reads each index with Get, so a hole never crosses
        // as a hole: the result is DENSE. Copying the backing store verbatim
        // instead made `[...Array(3)]` sparse, and every hole-skipping method
        // then silently did nothing — `.map` returned three holes, not [0,1,2].
        assert_eq!(run_ok("let a = [...Array(3)]; console.log(0 in a, a.length)"), vec!["true 3"]);
        assert_eq!(
            run_ok("console.log(JSON.stringify([...Array(3)].map((_, i) => i)))"),
            vec!["[0,1,2]"]
        );
        assert_eq!(run_ok("let b = [...[1, , 3]]; console.log(1 in b)"), vec!["true"]);

        // Every construct that materializes an array by iterating it.
        assert_eq!(run_ok("let [, ...r] = [1, , 3]; console.log(0 in r)"), vec!["true"]);
        assert_eq!(run_ok("console.log(1 in [...[...[1, , 3]]])"), vec!["true"]);
        assert_eq!(run_ok("console.log(2 in [9, ...[1, , 3]])"), vec!["true"]);
        assert_eq!(run_ok("console.log(0 in Array.from(Array(2)))"), vec!["true"]);

        // The hole resolves THROUGH the prototype chain, and the consumer stores
        // the result as an own property — so it outlives the prototype entry.
        // Getting this wrong still prints "P" (the lookup just happens later, at
        // stringify time), which is what made the original bug look correct.
        assert_eq!(
            run_ok(
                "Array.prototype[1] = 'P'; let r = [...[1, , 3]]; delete Array.prototype[1]; \
                 console.log(JSON.stringify(r), 1 in r)"
            ),
            vec![r#"[1,"P",3] true"#]
        );

        // A getter on the prototype is user code, and it runs during the spread.
        assert_eq!(
            run_ok(
                "let n = 0; Object.defineProperty(Array.prototype, 0, { configurable: true, \
                 get() { n++; return 'G'; } }); \
                 let r = [...[, 'b']]; delete Array.prototype[0]; \
                 console.log(JSON.stringify(r), n)"
            ),
            vec![r#"["G","b"] 1"#]
        );

        // Holes are still holes where no iteration happens: an array literal
        // keeps them, and `for-of` reads them as undefined without densifying.
        assert_eq!(run_ok("console.log(0 in [, 1])"), vec!["false"]);
        assert_eq!(
            run_ok("let s = ''; for (const v of [1, , 3]) s += (v === undefined ? 'u' : 'v'); console.log(s)"),
            vec!["vuv"]
        );
    }

    #[test]
    fn array_inspect_matches_node() {
        // node renders arrays with spaced brackets.
        assert_eq!(run_ok("console.log([1, 2, 3])"), vec!["[ 1, 2, 3 ]"]);
        assert_eq!(run_ok("console.log([])"), vec!["[]"]);
    }

    #[test]
    fn array_coercion_is_comma_join() {
        assert_eq!(run_ok("console.log('x' + [1,2,3])"), vec!["x1,2,3"]);
        assert_eq!(run_ok("console.log([1,2,3].join('-'))"), vec!["1-2-3"]);
    }

    #[test]
    fn array_push_pop() {
        assert_eq!(
            run_ok("let a = [1]; a.push(2); a.push(3); console.log(a.length, a[2])"),
            vec!["3 3"]
        );
        assert_eq!(run_ok("let a = [1,2,3]; let x = a.pop(); console.log(x, a.length)"), vec!["3 2"]);
    }

    #[test]
    fn object_literal_and_props() {
        assert_eq!(run_ok("let o = {a: 1, b: 2}; console.log(o.a, o.b)"), vec!["1 2"]);
        assert_eq!(run_ok("let o = {}; o.x = 5; console.log(o.x)"), vec!["5"]);
        assert_eq!(run_ok("let o = {a: 1}; o['b'] = 2; console.log(o['a'], o['b'])"), vec!["1 2"]);
    }

    #[test]
    fn object_inspect_matches_node() {
        assert_eq!(run_ok("console.log({a: 1, b: 2})"), vec!["{ a: 1, b: 2 }"]);
        assert_eq!(run_ok("console.log({})"), vec!["{}"]);
    }

    #[test]
    fn object_reference_semantics() {
        // Aliasing: mutating through one binding is visible through the other.
        assert_eq!(run_ok("let a = {n: 1}; let b = a; b.n = 9; console.log(a.n)"), vec!["9"]);
    }

    #[test]
    fn method_call_with_this() {
        assert_eq!(
            run_ok("let o = {x: 10, get() { return this.x }}; console.log(o.get())"),
            vec!["10"]
        );
    }

    #[test]
    fn this_recursive_method() {
        assert_eq!(
            run_ok("let o = {fact(n){ return n <= 1 ? 1 : n * this.fact(n-1) }}; console.log(o.fact(5))"),
            vec!["120"]
        );
    }

    #[test]
    fn function_expression_and_arrow() {
        assert_eq!(run_ok("let f = function(a){ return a*2 }; console.log(f(21))"), vec!["42"]);
        assert_eq!(run_ok("let g = a => a + 1; console.log(g(41))"), vec!["42"]);
        assert_eq!(run_ok("let h = (a, b) => a * b; console.log(h(6, 7))"), vec!["42"]);
    }

    #[test]
    fn nested_arrays_and_objects_inspect() {
        assert_eq!(run_ok("console.log([1, [2, 3], 4])"), vec!["[ 1, [ 2, 3 ], 4 ]"]);
        assert_eq!(run_ok("console.log({a: [1, 2], b: {c: 3}})"), vec!["{ a: [ 1, 2 ], b: { c: 3 } }"]);
    }

    #[test]
    fn array_as_loop_accumulator() {
        assert_eq!(
            run_ok("let a = []; for (let i = 0; i < 4; i++) { a.push(i * i) } console.log(a.join(','))"),
            vec!["0,1,4,9"]
        );
    }

    // ── Stage 2: callback builtins + string methods ──

    #[test]
    fn array_map_filter_reduce() {
        assert_eq!(
            run_ok("console.log([1,2,3,4].map(x => x * 2).join(','))"),
            vec!["2,4,6,8"]
        );
        assert_eq!(
            run_ok("console.log([1,2,3,4,5,6].filter(x => x % 2 === 0).join(','))"),
            vec!["2,4,6"]
        );
        assert_eq!(
            run_ok("console.log([1,2,3,4].reduce((p, c) => p + c, 0))"),
            vec!["10"]
        );
    }

    #[test]
    fn array_pipeline_matches_corpus() {
        // The exact shape of bench/array.js.
        assert_eq!(
            run_ok("let a=[]; for(let i=0;i<10;i++) a.push(i); console.log(a.map(x=>x*2).filter(x=>x%3===0).reduce((p,c)=>p+c,0))"),
            vec!["36"] // map→0,2,4,…,18; filter %3===0→0,6,12,18; sum→36
        );
    }

    #[test]
    fn map_kernel_matches_interpreter() {
        // The fused native map kernel must agree with the interpreter (JIT-off)
        // and node on int-tagged AND double elements, with index, mixed types,
        // overflow, and a tail after a guard bail.
        // Loop-built arrays hold DOUBLES (the build loop ran in the SSE region) —
        // the kernel must process them, not bail at element 0.
        assert_jit_matches(
            "let a=[]; for(let i=0;i<50;i++) a[i]=i; console.log(a.map(x=>x*2).reduce((s,x)=>s+x,0))",
            &["2450"], // 2*(0+..+49) = 2*1225
        );
        // Literal int array (int-tagged elements): full native run.
        assert_jit_matches("console.log([1,2,3,4].map(x=>x*2).join(','))", &["2,4,6,8"]);
        // Two-param callback (element, index).
        assert_jit_matches("console.log([10,20,30].map((x,i)=>x+i).join(','))", &["10,21,32"]);
        // Division (f64).
        assert_jit_matches("console.log([4,6,9].map(x=>x/2).join(','))", &["2,3,4.5"]);
        // Mixed int/double: kernel runs the int prefix, bails at 3.5, the tail
        // (interpreter) finishes — same answer as a full interpreter run.
        assert_jit_matches("console.log([1,2,3.5,4].map(x=>x*2).join(','))", &["2,4,7,8"]);
        // Non-numeric element → bail to the tail, which yields NaN (== node).
        assert_jit_matches("console.log([1,2,'x',4].map(x=>x*2).join(','))", &["2,4,NaN,8"]);
        // Overflow past i32: f64 stays exact (no wrap).
        assert_jit_matches("console.log([2,1000000000,3].map(x=>x*3).join(','))", &["6,3000000000,9"]);
        // Empty array.
        assert_jit_matches("console.log([].map(x=>x*2).length)", &["0"]);
        // Compound arithmetic body.
        assert_jit_matches("console.log([1,2,3,4].map(x=>x*2+1).join(','))", &["3,5,7,9"]);
    }

    #[test]
    fn reduce_kernel_matches_interpreter() {
        // The fused native reduce kernel must agree with the interpreter and node
        // on int/double elements, with/without an initial value, mixed-type and
        // string tails, and 3-param (index) reduces (which fall back).
        assert_jit_matches("console.log([1,2,3,4].reduce((s,x)=>s+x,0))", &["10"]);
        // No initial value: the first element seeds, kernel runs from index 1.
        assert_jit_matches("console.log([1,2,3,4].reduce((s,x)=>s+x))", &["10"]);
        // Single element, no initial value.
        assert_jit_matches("console.log([7].reduce((s,x)=>s+x))", &["7"]);
        // Product (multiplicative accumulator).
        assert_jit_matches("console.log([1,2,3,4,5].reduce((s,x)=>s*x,1))", &["120"]);
        // Loop-built (double) array — the kernel must process doubles.
        assert_jit_matches(
            "let a=[]; for(let i=0;i<100;i++) a[i]=i; console.log(a.reduce((s,x)=>s+x,0))",
            &["4950"],
        );
        // Mixed: kernel runs the numeric prefix, bails at 3.5, tail finishes.
        assert_jit_matches("console.log([1,2,3.5,4].reduce((s,x)=>s+x,0))", &["10.5"]);
        // Non-numeric element → string concatenation in the tail (== node).
        assert_jit_matches("console.log([1,2,'x',4].reduce((s,x)=>s+x,0))", &["3x4"]);
        // Empty array WITH an initial value returns it untouched.
        assert_jit_matches("console.log([].reduce((s,x)=>s+x,42))", &["42"]);
        // 3-param (index) reduce isn't kernel-eligible — must still be correct.
        assert_jit_matches("console.log([5,6,7].reduce((s,x,i)=>s+x*i,0))", &["20"]);
    }

    #[test]
    fn filter_kernel_matches_interpreter() {
        // The fused native filter kernel: a Bool-returning predicate (comparison
        // or `%`-comparison) selects elements; non-Bool predicates and non-number
        // elements fall to the per-element tail (JS truthiness). Must agree with
        // the interpreter and node across int/double, mixed, index, and empty.
        assert_jit_matches("console.log([1,2,3,4,5,6].filter(x=>x%2===0).join(','))", &["2,4,6"]);
        assert_jit_matches("console.log([1,2,3,4,5,6,7,8,9].filter(x=>x%3===0).join(','))", &["3,6,9"]);
        assert_jit_matches("console.log([5,3,8,1,9,2].filter(x=>x>=5).join(','))", &["5,8,9"]);
        assert_jit_matches("console.log([1,2,3,4,5].filter(x=>x<=2).join(','))", &["1,2"]);
        // Loop-built (double) array.
        assert_jit_matches(
            "let a=[]; for(let i=0;i<100;i++) a[i]=i; console.log(a.filter(x=>x%10===0).length)",
            &["10"],
        );
        // Mixed: 3.5 % 2 !== 0 (kept-out), and the run continues past it.
        assert_jit_matches("console.log([1,2,3.5,4,6].filter(x=>x%2===0).join(','))", &["2,4,6"]);
        // Non-number element → predicate bails to the tail ('x' % 2 !== 0).
        assert_jit_matches("console.log([1,2,'x',4].filter(x=>x%2===0).join(','))", &["2,4"]);
        // Non-Bool predicate result (bare value) → tail evaluates JS truthiness.
        assert_jit_matches("console.log([0,1,2,0,3].filter(x=>x).join(','))", &["1,2,3"]);
        // Index predicate (2-param).
        assert_jit_matches("console.log([1,2,3,4,5,6].filter((x,i)=>i%2===0).join(','))", &["1,3,5"]);
        // Empty.
        assert_jit_matches("console.log([].filter(x=>x>0).length)", &["0"]);
    }

    #[test]
    fn array_sort_comparator() {
        assert_eq!(
            run_ok("let a = [3, 1, 4, 1, 5, 9, 2, 6]; a.sort((x, y) => x - y); console.log(a.join(','))"),
            vec!["1,1,2,3,4,5,6,9"]
        );
        // sort returns the same array reference and mutates in place.
        assert_eq!(
            run_ok("let a = [3,1,2]; let b = a.sort((x,y)=>x-y); console.log(a.join(','), a === b)"),
            vec!["1,2,3 true"]
        );
    }

    #[test]
    fn array_sort_default_lexicographic() {
        assert_eq!(
            run_ok("console.log([10, 1, 2, 20].sort().join(','))"),
            vec!["1,10,2,20"]
        );
    }

    #[test]
    fn array_misc_methods() {
        assert_eq!(run_ok("console.log([1,2,3].indexOf(2))"), vec!["1"]);
        assert_eq!(run_ok("console.log([1,2,3].includes(5))"), vec!["false"]);
        assert_eq!(run_ok("console.log([1,2,3,4].slice(1,3).join(','))"), vec!["2,3"]);
        assert_eq!(run_ok("let a=[1,2,3]; console.log(a.shift(), a.join(','))"), vec!["1 2,3"]);
    }

    #[test]
    fn string_indexing_and_methods() {
        assert_eq!(run_ok("let s = 'hello'; console.log(s[0], s[4], s.length)"), vec!["h o 5"]);
        assert_eq!(run_ok("console.log('hello'.toUpperCase())"), vec!["HELLO"]);
        assert_eq!(run_ok("console.log('Hello World'.indexOf('World'))"), vec!["6"]);
        assert_eq!(run_ok("console.log('a,b,c'.split(',').join('-'))"), vec!["a-b-c"]);
        assert_eq!(run_ok("console.log('ab'.repeat(3))"), vec!["ababab"]);
        assert_eq!(run_ok("console.log('hello'.slice(1, 4))"), vec!["ell"]);
    }

    #[test]
    fn string_char_counting_matches_corpus() {
        // The shape of bench/string.js's counting loop.
        assert_eq!(
            run_ok("let s='0123456789'; let c=0; for(let i=0;i<s.length;i++){ if(s[i]==='7') c++; } console.log(c)"),
            vec!["1"]
        );
    }

    // ── Stage 3: closures ──

    #[test]
    fn closure_counter_shares_mutable_state() {
        // The classic: each call mutates the captured `c`, shared across calls.
        assert_eq!(
            run_ok("function counter(){ let c=0; return function(){ c++; return c } } let f=counter(); console.log(f(), f(), f())"),
            vec!["1 2 3"]
        );
    }

    #[test]
    fn closure_captures_parameter() {
        assert_eq!(
            run_ok("function adder(n){ return x => x + n } let a5 = adder(5); console.log(a5(10), a5(20))"),
            vec!["15 25"]
        );
    }

    #[test]
    fn arrow_captures_outer_let() {
        assert_eq!(run_ok("let mul = 3; let f = x => x * mul; console.log(f(10))"), vec!["30"]);
    }

    #[test]
    fn nested_of_nested_capture() {
        // Three levels: innermost captures `a` from the grandparent (ParentUpval
        // re-sourcing) and `b` from the parent (ParentLocal).
        assert_eq!(
            run_ok("function outer(){ let a=1; function mid(){ let b=10; return function(){ return a+b } } return mid() } console.log(outer()())"),
            vec!["11"]
        );
    }

    #[test]
    fn closures_are_independent_instances() {
        // Two counters from the same factory must not share state.
        assert_eq!(
            run_ok("function mk(){ let c=0; return ()=>++c } let a=mk(); let b=mk(); console.log(a(),a(),b(),a())"),
            vec!["1 2 1 3"]
        );
    }

    #[test]
    fn closure_mutates_captured_from_inner() {
        // Writing a captured upvalue from the inner function is visible to a
        // sibling reader closure (shared cell).
        assert_eq!(
            run_ok("function mk(){ let v=0; let set=x=>{v=x}; let get=()=>v; return [set,get] } let p=mk(); p[0](42); console.log(p[1]())"),
            vec!["42"]
        );
    }

    // ── Stage 4: loose equality ──

    #[test]
    fn loose_equality_matches_node() {
        assert_eq!(run_ok("console.log(1 == '1')"), vec!["true"]);
        assert_eq!(run_ok("console.log(null == undefined)"), vec!["true"]);
        assert_eq!(run_ok("console.log(0 == false)"), vec!["true"]);
        assert_eq!(run_ok("console.log('' == 0)"), vec!["true"]);
        assert_eq!(run_ok("console.log('2' == 2)"), vec!["true"]);
        assert_eq!(run_ok("console.log(true == 1)"), vec!["true"]);
        assert_eq!(run_ok("console.log(null == 0)"), vec!["false"]);
        assert_eq!(run_ok("console.log(undefined == 0)"), vec!["false"]);
        assert_eq!(run_ok("console.log(1 != '1')"), vec!["false"]);
        assert_eq!(run_ok("console.log(null != undefined)"), vec!["false"]);
    }

    #[test]
    fn strict_vs_loose_distinct() {
        assert_eq!(run_ok("console.log(1 === '1', 1 == '1')"), vec!["false true"]);
        assert_eq!(run_ok("console.log(null === undefined, null == undefined)"), vec!["false true"]);
    }

    #[test]
    fn nan_and_infinity_globals() {
        assert_eq!(run_ok("console.log(NaN == NaN)"), vec!["false"]);
        assert_eq!(run_ok("let x = 0/0; console.log(x === x)"), vec!["false"]);
        assert_eq!(run_ok("console.log(Infinity > 1e308, -Infinity < 0)"), vec!["true true"]);
    }

    // ── Stage 4: for-of / for-in / do-while / try-catch-throw ──

    #[test]
    fn for_of_array_and_string() {
        assert_eq!(run_ok("let s=0; for (const x of [1,2,3,4]) { s += x } console.log(s)"), vec!["10"]);
        assert_eq!(run_ok("let s=''; for (const c of 'abc') { s = c + s } console.log(s)"), vec!["cba"]);
    }

    #[test]
    fn for_in_object_keys_and_values() {
        assert_eq!(run_ok("let o={a:1,b:2,c:3}; let k=''; for (const key in o) { k += key } console.log(k)"), vec!["abc"]);
        assert_eq!(run_ok("let o={x:10,y:20,z:5}; let s=0; for (const key in o) { s += o[key] } console.log(s)"), vec!["35"]);
    }

    #[test]
    fn do_while_runs_body_first() {
        assert_eq!(run_ok("let i=0,s=0; do { s+=i; i++ } while (i<5); console.log(s)"), vec!["10"]);
        // body runs at least once even when the condition is false initially
        assert_eq!(run_ok("let n=0; do { n++ } while (false); console.log(n)"), vec!["1"]);
    }

    #[test]
    fn try_catch_basic() {
        assert_eq!(run_ok("try { throw 'boom' } catch (e) { console.log('caught', e) }"), vec!["caught boom"]);
        assert_eq!(run_ok("try { throw 42 } catch (e) { console.log(e + 1) }"), vec!["43"]);
    }

    #[test]
    fn try_catch_across_call() {
        assert_eq!(
            run_ok("function f(){ throw 'deep' } try { f() } catch(e){ console.log('got', e) }"),
            vec!["got deep"]
        );
    }

    #[test]
    fn try_catch_finally_order() {
        assert_eq!(
            run_ok("let r=''; try { r+='a'; throw 1; r+='b' } catch(e){ r+='c' } finally { r+='d' } console.log(r)"),
            vec!["acd"]
        );
        // finally also runs on normal completion
        assert_eq!(
            run_ok("let r=''; try { r+='x' } finally { r+='y' } console.log(r)"),
            vec!["xy"]
        );
    }

    #[test]
    fn try_finally_runs_on_all_exits() {
        // `return` inside try runs the finally first (sync function).
        assert_eq!(run_ok("function f(){try{return 'A'}finally{console.log('fin')}} console.log(f())"), vec!["fin", "A"]);
        // Plain `return` in try/finally, no catch.
        assert_eq!(run_ok("function f(){try{return 1}finally{console.log('f')}} console.log(f())"), vec!["f", "1"]);
        // Nested try/finally: both finallys run, innermost first.
        assert_eq!(run_ok("function f(){try{try{return 'v'}finally{console.log('in')}}finally{console.log('out')}} console.log(f())"), vec!["in", "out", "v"]);
        // finally overrides the try's return.
        assert_eq!(run_ok("function f(){try{return 'try'}finally{return 'fin'}} console.log(f())"), vec!["fin"]);
        // finally overrides a throw with a return.
        assert_eq!(run_ok("function f(){try{throw 'x'}finally{return 'saved'}} console.log(f())"), vec!["saved"]);
        // A throw in finally overrides the try's return; caught one level out.
        assert_eq!(run_ok("function f(){try{try{return 'x'}finally{throw 'ft'}}catch(e){return 'caught '+e}} console.log(f())"), vec!["caught ft"]);
        // Throw propagates THROUGH a finally (uncaught locally) across a call.
        assert_eq!(run_ok("function g(){try{throw 'd'}finally{console.log('gfin')}} function f(){try{g()}catch(e){return 'c '+e}} console.log(f())"), vec!["gfin", "c d"]);
        // finally runs every loop iteration; value passes through on normal exit.
        assert_eq!(run_ok("function f(){let s=0; for(let i=0;i<3;i++){try{s+=i}finally{s+=100}} return s} console.log(f())"), vec!["303"]);
        // return in catch, with finally still running.
        assert_eq!(run_ok("function f(){try{throw 'e'}catch(x){return 'c'}finally{console.log('fin')}} console.log(f())"), vec!["fin", "c"]);
    }

    #[test]
    fn error_object_name_and_message() {
        assert_eq!(run_ok("try { throw new Error('boom') } catch (e) { console.log(e.message, e.name) }"), vec!["boom Error"]);
        assert_eq!(run_ok("try { throw new RangeError('neg') } catch (e) { console.log(e.name) }"), vec!["RangeError"]);
    }

    #[test]
    fn property_of_undefined_throws_typeerror() {
        assert_eq!(
            run_ok("let x; try { x = undefined.foo } catch (e) { x = 'caught' } console.log(x)"),
            vec!["caught"]
        );
    }

    #[test]
    fn uncaught_throw_reports_error_with_output_preserved() {
        let out = run("console.log('before'); throw new Error('fail'); console.log('after')").expect("compile");
        assert_eq!(out.output, vec!["before"]);
        assert!(out.error.as_ref().unwrap().contains("fail"), "got {:?}", out.error);
    }

    // ── Stage 5b: native JIT (correctness — same answers as the interpreter) ──

    #[test]
    fn jit_hot_int_leaf_function_correct() {
        // A pure-int leaf function called far past the JIT threshold (8). The
        // result must equal node regardless of whether it ran native or interp.
        assert_eq!(
            run_ok("function sq(x){ return x*x } let s=0; for(let i=0;i<50;i++){ s = s + sq(i) } console.log(s)"),
            vec!["40425"] // sum of i^2 for i in 0..49
        );
    }

    #[test]
    fn jit_multi_op_int_function_correct() {
        // f(a,b,c)=max(0, a*a + 2b - c); summed over i in 0..30 → 455 (node).
        assert_eq!(
            run_ok("function f(a,b,c){ let r=a*a; r=r+b*2; r=r-c; if(r<0) return 0; return r } let t=0; for(let i=0;i<30;i++){ t=t+f(i%7, i%5, i%3) } console.log(t)"),
            vec!["455"]
        );
    }

    #[test]
    fn jit_overflow_bails_to_f64_not_wrap() {
        // i32 multiply that overflows must NOT wrap (the old engine's bug); the
        // JIT bails and the interpreter computes the f64 result, == node.
        assert_eq!(
            run_ok("function big(x){ return x*x } let r=0; for(let i=0;i<20;i++){ r = big(100000) } console.log(r)"),
            vec!["10000000000"] // 100000^2 = 1e10, exceeds i32 → must be exact, not wrapped
        );
    }

    #[test]
    fn jit_type_change_bails_correctly() {
        // A function that's int for many calls then gets a non-int arg must
        // still produce the right answer (the op bails on the non-int operand).
        assert_eq!(
            run_ok("function add1(x){ return x + 1 } let out=''; for(let i=0;i<12;i++){ out = '' + add1(i) } out = '' + add1('s'); console.log(out)"),
            vec!["s1"] // 's' + 1 → 's1' (string concat, via bail)
        );
    }

    #[test]
    fn per_iteration_let_bindings() {
        // A captured `let` loop var gets a FRESH binding per iteration (node 0,1,2),
        // while `var` shares one (3,3,3). Covers for / for-of / for-in.
        assert_eq!(
            run_ok("function mk(){ let xs=[]; for(let i=0;i<3;i++){ xs.push(()=>i) } return xs } let f=mk(); console.log(f.map(g=>g()).join(','))"),
            vec!["0,1,2"]
        );
        assert_eq!(
            run_ok("function mk(){ let xs=[]; for(var j=0;j<3;j++){ xs.push(()=>j) } return xs } console.log(mk().map(g=>g()).join(','))"),
            vec!["3,3,3"]
        );
        // for-of: fresh binding per element.
        assert_eq!(
            run_ok("function mk(){ let xs=[]; for(let x of [10,20,30]){ xs.push(()=>x) } return xs } console.log(mk().map(g=>g()).join(','))"),
            vec!["10,20,30"]
        );
        // for-in: fresh binding per key.
        assert_eq!(
            run_ok("function mk(){ let xs=[]; let o={a:1,b:2}; for(let k in o){ xs.push(()=>k) } return xs } console.log(mk().map(g=>g()).join(','))"),
            vec!["a,b"]
        );
        // Mutation inside the body is visible to THAT iteration's closure.
        assert_eq!(
            run_ok("function mk(){ let xs=[]; for(let i=0;i<3;i++){ i+=10; xs.push(()=>i) } return xs } console.log(mk().map(g=>g()).join(','))"),
            vec!["10"]
        );
        // Nested for-let captures independently.
        assert_eq!(
            run_ok("function mk(){ let xs=[]; for(let a=0;a<2;a++) for(let b=0;b<2;b++) xs.push(()=>a*10+b); return xs } console.log(mk().map(g=>g()).join(','))"),
            vec!["0,1,10,11"]
        );
        // Non-captured loop is unaffected (fast path / hot-loop JIT preserved).
        assert_eq!(run_ok("let s=0; for(let i=0;i<1000;i++) s+=i; console.log(s)"), vec!["499500"]);
    }

    // ── rope strings (cons-strings) + JsStr cached length/index + interning ──

    #[test]
    fn rope_concat_loop_content_and_length() {
        // `s += digit` builds a deep rope; flattening on display + the O(1)
        // cached length must reproduce the eager-concat result exactly.
        assert_jit_matches(
            "let s=''; for(let i=0;i<5;i++){ s += i; } console.log(s, s.length)",
            &["01234 5"],
        );
    }

    #[test]
    fn rope_index_and_methods_after_flatten() {
        // First s[i] flattens the rope; charAt / indexOf / split / toUpperCase
        // must then operate on the flat string correctly.
        assert_eq!(
            run_ok("let s=''; for(let i=0;i<5;i++){ s+=i; } console.log(s.charAt(2), s.indexOf('3'), s.split('').length, s.toUpperCase())"),
            vec!["2 3 5 01234"],
        );
    }

    #[test]
    fn rope_aliasing_is_immutable() {
        // `let t=s; s+=x` must NOT mutate t — ropes share children structurally,
        // and flattening s in place must not corrupt the aliased value t.
        assert_eq!(
            run_ok("let s=''; for(let i=0;i<3;i++){ s+='ab'; } let t=s; s+='Z'; console.log(s, t, s.length, t.length)"),
            vec!["abababZ ababab 7 6"],
        );
    }

    #[test]
    fn rope_strict_eq_against_flat() {
        // A rope and a flat literal with equal content are === equal (str_eq
        // materializes the rope side; flat-vs-flat stays the fast no-alloc path).
        assert_eq!(
            run_ok("let a='he'+'llo'; console.log(a==='hello', a==='hell', ('x'+'y')===('xy'))"),
            vec!["true false true"],
        );
    }

    #[test]
    fn empty_rope_length_and_truthiness() {
        // An empty rope ("" + "") has length 0 and is falsy (str_is_empty is O(1)
        // on Cons via len, and the interned empty string round-trips).
        assert_eq!(
            run_ok("let e=''+''; console.log(e.length, e?1:0, (''+'')==='')"),
            vec!["0 0 true"],
        );
    }

    #[test]
    fn concat_coerces_array_and_object() {
        // Either side heap ⇒ string concatenation; arrays join, objects become
        // [object Object] — coerced to a flat string child of the rope.
        assert_eq!(
            run_ok("console.log([1,2]+[3], {}+'x', 'n='+(1+2))"),
            vec!["1,23 [object Object]x n=3"],
        );
    }

    #[test]
    fn interned_single_chars_index_correctly() {
        // Indexing returns interned single-char strings (shared slots); content
        // and per-index correctness must hold across many accesses.
        assert_jit_matches(
            "let s='abcdefghij'; let c=0; for(let i=0;i<s.length;i++){ if(s[i]==='e'){ c++; } } console.log(c, s[0], s[9])",
            &["1 a j"],
        );
    }

    #[test]
    fn nonascii_length_and_index_unit_count() {
        // Non-ASCII decodes per UTF-16 unit; .length is the cached unit count.
        // 'café' is 4 units (all BMP); index 3 is 'é'.
        assert_eq!(
            run_ok("let s='caf\u{00e9}'; console.log(s.length, s[3], s.charAt(3), (s+s).length)"),
            vec!["4 \u{00e9} \u{00e9} 8"],
        );
    }

    #[test]
    fn astral_utf16_unit_semantics() {
        // An astral char ('𠮷' U+20BB7) is TWO UTF-16 units: .length counts
        // units, charCodeAt yields the surrogate halves, codePointAt the full
        // code point, while for-of/spread still iterate CODE POINTS (one step).
        assert_eq!(
            run_ok("let s='\u{20BB7}a'; console.log(s.length, s.charCodeAt(0), s.charCodeAt(1), s.charCodeAt(2), s.codePointAt(0), s.charAt(2), [...s].length, s.slice(2), s.indexOf('a'))"),
            vec!["3 55362 57271 97 134071 a 2 a 2"],
        );
        assert_eq!(
            run_ok("console.log(String.fromCharCode(0xD842, 0xDFB7) === '\u{20BB7}', '\u{20BB7}'.padStart(4, 'x'))"),
            vec!["true xx\u{20BB7}"],
        );
    }

    #[test]
    fn array_index_region_sum() {
        // `s += a[i]` over a constant bound JITs in the OSR region (GetIndex via
        // helper). The region computes the loop counter as f64, so a[i] indexes
        // with a DOUBLE key — array_index must coerce it. JIT-on == JIT-off.
        assert_jit_matches(
            "let a=[]; for(let i=0;i<100;i++) a.push(i); let s=0; for(let i=0;i<100;i++){ s+=a[i]; } console.log(s)",
            &["4950"],
        );
    }

    #[test]
    fn array_length_bound_index_region() {
        // `for (i < a.length) s += a[i]` — the common array-scan shape. a.length
        // is a GetProp the miss-helper now answers for arrays (uncached), so the
        // whole loop JITs instead of bailing on the first .length access.
        assert_jit_matches(
            "let a=[]; for(let i=0;i<100;i++) a.push(i*2); let s=0; for(let i=0;i<a.length;i++){ s+=a[i]; } console.log(s)",
            &["9900"],
        );
    }

    #[test]
    fn object_length_property_not_confused_with_array_length() {
        // An object with its own `length` property reads that property via the
        // inline-cache slot, never the array element-count path.
        assert_jit_matches(
            "let o={length:7}; let s=0; for(let i=0;i<20000;i++){ s+=o.length; } console.log(s)",
            &["140000"],
        );
    }

    #[test]
    fn switch_statement() {
        assert_eq!(run_ok("let x=2,r=''; switch(x){case 1:r='a';break;case 2:r='b';break;default:r='d';} console.log(r)"), vec!["b"]);
        assert_eq!(run_ok("let x=9,r=''; switch(x){case 1:r='a';break;default:r='d';} console.log(r)"), vec!["d"]);
        // Fall-through (no break) runs subsequent case bodies.
        assert_eq!(run_ok("let r='',x=2; switch(x){case 1:r+='1';case 2:r+='2';case 3:r+='3';break;case 4:r+='4';} console.log(r)"), vec!["23"]);
        assert_eq!(run_ok("function f(x){switch(x){case 1:return 'one';default:return 'other'}} console.log(f(1),f(5))"), vec!["one other"]);
        // `continue` in a switch targets the enclosing LOOP; `break` targets the switch.
        assert_eq!(run_ok("let r=[]; for(let i=0;i<4;i++){ switch(i){case 1:continue;case 3:break;} r.push(i); } console.log(r.join(','))"), vec!["0,2,3"]);
    }

    #[test]
    fn break_and_continue() {
        assert_eq!(run_ok("let s=0; for(let i=0;i<10;i++){ if(i===5) break; s+=i; } console.log(s)"), vec!["10"]);
        assert_eq!(run_ok("let s=0; for(let i=0;i<5;i++){ if(i===2) continue; s+=i; } console.log(s)"), vec!["8"]);
        assert_eq!(run_ok("let s=0,i=0; while(i<100){ i++; if(i>5) break; s+=i; } console.log(s)"), vec!["15"]);
        assert_eq!(run_ok("let s=0; for(const x of [1,2,3,4]){ if(x===3) break; s+=x; } console.log(s)"), vec!["3"]);
        // do-while with both; and a nested loop where the inner break stays inner.
        assert_eq!(run_ok("let s=0,i=0; do{ i++; if(i===3) continue; if(i>5) break; s+=i; }while(i<100); console.log(s)"), vec!["12"]);
        assert_eq!(run_ok("let s=0; for(let i=0;i<5;i++){ for(let j=0;j<5;j++){ if(j===2) break; s+=1; } } console.log(s)"), vec!["10"]);
    }

    #[test]
    fn break_in_hot_loop_jit() {
        // A `break` in a JIT'd loop is a region exit; JIT-on must equal JIT-off.
        assert_jit_matches(
            "let c=0; for(let i=0;i<100000;i++){ if(i>=50000) break; c++; } console.log(c)",
            &["50000"],
        );
    }

    #[test]
    fn optional_chaining() {
        // Member chains short-circuit to undefined at the first nullish base.
        assert_eq!(run_ok("let o={a:{b:7}}; console.log(o?.a?.b, o?.x?.y, o?.a?.b?.c)"), vec!["7 undefined undefined"]);
        assert_eq!(run_ok("let o=null; console.log(o?.a?.b)"), vec!["undefined"]);
        // Optional computed access and optional calls.
        assert_eq!(run_ok("let o={arr:[10,20]}; console.log(o?.arr?.[1], o?.no?.[0])"), vec!["20 undefined"]);
        assert_eq!(run_ok("let o={f:()=>42}; console.log(o?.f(), o?.g?.())"), vec!["42 undefined"]);
        // The short-circuited value is genuine undefined (NaN in arithmetic).
        assert_eq!(run_ok("let u=undefined; console.log(u?.x, (u?.x)+1)"), vec!["undefined NaN"]);
    }

    #[test]
    fn default_parameters() {
        // Applied only when the arg is missing/undefined (null does NOT trigger it).
        assert_eq!(run_ok("function f(x=5){return x} console.log(f(), f(9), f(undefined))"), vec!["5 9 5"]);
        assert_eq!(run_ok("function z(x=1){return x} console.log(z(null), z(0))"), vec!["null 0"]);
        // A later default may reference an earlier parameter.
        assert_eq!(run_ok("function g(a,b=10,c=a+b){return a+','+b+','+c} console.log(g(1), g(1,2))"), vec!["1,10,11 1,2,3"]);
        // Arrow defaults; and a defaulted parameter captured by a closure.
        assert_eq!(run_ok("let h=(x=7)=>x*2; console.log(h(), h(4))"), vec!["14 8"]);
        assert_eq!(run_ok("function cap(n=3){return ()=>n} console.log(cap()(), cap(8)())"), vec!["3 8"]);
    }

    #[test]
    fn array_is_array() {
        assert_eq!(
            run_ok("console.log(Array.isArray([]), Array.isArray([1]), Array.isArray(1), Array.isArray('x'), Array.isArray({}), Array.isArray(null))"),
            vec!["true true false false false false"],
        );
    }

    #[test]
    fn number_parse_globals() {
        assert_eq!(
            run_ok("console.log(Number('42')+1, Number(''), Number(true), Number('abc'), Number())"),
            vec!["43 0 1 NaN 0"],
        );
        assert_eq!(
            run_ok("console.log(parseInt('10px'), parseInt('0xff'), parseInt('11',2), parseInt('-7'), parseInt('abc'))"),
            vec!["10 255 3 -7 NaN"],
        );
        assert_eq!(
            run_ok("console.log(parseFloat('3.14x'), parseFloat('1e3'), parseFloat('-2.5e-1'), parseFloat('abc'))"),
            vec!["3.14 1000 -0.25 NaN"],
        );
    }

    #[test]
    fn method_name_after_numeric_constant() {
        // REGRESSION: a method/property name's index must be into string_constants,
        // not the constant pool — a preceding non-string constant (e.g. 3.5) used
        // to push the name's pool index past string_constants and panic (OOB).
        assert_eq!(run_ok("console.log((3.5).toFixed(2))"), vec!["3.50"]);
        assert_eq!(run_ok("let x=3.14159; console.log(x.toFixed(2))"), vec!["3.14"]);
        assert_eq!(run_ok("let n=9.5; let o={prop:7}; console.log(o.prop, n)"), vec!["7 9.5"]);
        assert_eq!(run_ok("let a=[1.5]; console.log(a[0].toFixed(1))"), vec!["1.5"]);
        // toFixed rounds half AWAY from zero (not Rust's half-to-even), on the
        // EXACT decimal — so float-repr near-ties round the way node does.
        assert_eq!(run_ok("console.log((0.5).toFixed(0), (2.5).toFixed(0), (1.5).toFixed(0))"), vec!["1 3 2"]);
        assert_eq!(run_ok("console.log((0.15).toFixed(1), (1.45).toFixed(1), (2.675).toFixed(2), (8.575).toFixed(2))"), vec!["0.1 1.4 2.67 8.57"]);
        assert_eq!(run_ok("console.log((-0.5).toFixed(0), (-2.5).toFixed(0), (123.456).toFixed(2))"), vec!["-1 -3 123.46"]);
        assert_eq!(run_ok("console.log((999.999).toFixed(2), (0).toFixed(2))"), vec!["1000.00 0.00"]);
    }

    #[test]
    fn object_statics_and_math_constants() {
        assert_eq!(
            run_ok("let o={a:1,b:2,c:3}; console.log(Object.keys(o).join(','), Object.values(o).join(','))"),
            vec!["a,b,c 1,2,3"],
        );
        assert_eq!(
            run_ok("let o={x:10,y:20}; console.log(Object.entries(o).map(e=>e[0]+'='+e[1]).join(','))"),
            vec!["x=10,y=20"],
        );
        assert_eq!(run_ok("console.log(Object.keys([7,8]).join(','), Object.values([7,8]).join(','))"), vec!["0,1 7,8"]);
        assert_eq!(run_ok("console.log(Math.PI.toFixed(4), Math.E.toFixed(4), Math.SQRT2.toFixed(4))"), vec!["3.1416 2.7183 1.4142"]);
    }

    #[test]
    fn json_parse() {
        assert_eq!(
            run_ok("let o=JSON.parse('{\"a\":1,\"b\":[2,3],\"c\":\"hi\"}'); console.log(o.a, o.b[1], o.c)"),
            vec!["1 3 hi"],
        );
        assert_eq!(
            run_ok("console.log(JSON.parse('[1,2.5,-3,1e2,true,false,null]').join(','))"),
            vec!["1,2.5,-3,100,true,false,"],
        );
        // Round-trips with stringify.
        assert_eq!(run_ok("let r=JSON.parse(JSON.stringify({x:[1,{y:2}],z:'a'})); console.log(r.x[1].y, r.z)"), vec!["2 a"]);
        // Invalid JSON throws a (catchable) SyntaxError.
        assert_eq!(run_ok("let e='ok'; try{ JSON.parse('{bad}'); }catch(x){ e='threw'; } console.log(e)"), vec!["threw"]);
        assert_eq!(run_ok("let e='ok'; try{ JSON.parse('[1,2'); }catch(x){ e='threw'; } console.log(e)"), vec!["threw"]);
    }

    #[test]
    fn json_stringify() {
        assert_eq!(run_ok("console.log(JSON.stringify({a:1,b:[2,3]}))"), vec![r#"{"a":1,"b":[2,3]}"#]);
        // undefined/function are omitted in objects but become null in arrays.
        assert_eq!(run_ok("console.log(JSON.stringify([1,undefined,null]))"), vec!["[1,null,null]"]);
        assert_eq!(run_ok("console.log(JSON.stringify({x:undefined,y:1}))"), vec![r#"{"y":1}"#]);
        // Primitives; NaN/Infinity → null; top-level undefined → undefined.
        assert_eq!(run_ok("console.log(JSON.stringify(42), JSON.stringify(NaN), JSON.stringify(undefined))"), vec!["42 null undefined"]);
        // Pretty-print with a numeric `space`.
        assert_eq!(run_ok("console.log(JSON.stringify({a:1}, null, 2))"), vec!["{\n  \"a\": 1\n}"]);
    }

    #[test]
    fn spread_operator() {
        // Array-literal spread: arrays, repeated sources, with plain elements.
        assert_eq!(run_ok("let a=[1,2]; console.log([...a,3,...a].join(','))"), vec!["1,2,3,1,2"]);
        assert_eq!(run_ok("let a=[1,2],b=[3,4]; console.log([0,...a,...b,5].join(','))"), vec!["0,1,2,3,4,5"]);
        assert_eq!(run_ok("console.log([...[]].length, [...[1]].length)"), vec!["0 1"]);
        // Spreading a string yields its characters.
        assert_eq!(run_ok("console.log([...'abc'].join('-'))"), vec!["a-b-c"]);
        // Call spread on a plain function value (declared fn and arrow).
        assert_eq!(run_ok("function sum(a,b,c){return a+b+c} console.log(sum(...[1,2,3]))"), vec!["6"]);
        assert_eq!(run_ok("let g=(a,b)=>a-b; console.log(g(...[10,3]))"), vec!["7"]);
        assert_eq!(run_ok("function f(a,b,c,d){return a+b+c+d} console.log(f(1,...[2,3],4))"), vec!["10"]);
        // Method-call spread: builtin (push/concat) and mixed spread+plain args.
        assert_eq!(run_ok("let a=[3,1,2]; a.push(...[4,5]); console.log(a.join(','))"), vec!["3,1,2,4,5"]);
        assert_eq!(run_ok("let a=[1,2],b=[5,6]; a.push(...b,7); console.log(a.join(','))"), vec!["1,2,5,6,7"]);
        assert_eq!(run_ok("console.log([0].concat(...[[1,2],[3]]).join(','))"), vec!["0,1,2,3"]);
        // Spreading a non-iterable throws a (catchable) TypeError.
        assert_eq!(run_ok("let e='ok'; try{ [...5]; }catch(x){ e='threw'; } console.log(e)"), vec!["threw"]);
    }

    #[test]
    fn destructuring() {
        // Object: shorthand, subset, rename, defaults.
        assert_eq!(run_ok("let {x,y}={x:1,y:2}; console.log(x+y)"), vec!["3"]);
        assert_eq!(run_ok("let {a:p,b:q}={a:10,b:20}; console.log(p,q)"), vec!["10 20"]);
        assert_eq!(run_ok("let {x=5,y=9}={x:1}; console.log(x,y)"), vec!["1 9"]);
        // Array: positional, holes, defaults, rest (incl. shorter-than-pattern).
        assert_eq!(run_ok("let [a,b,c]=[10,20,30]; console.log(a+b+c)"), vec!["60"]);
        assert_eq!(run_ok("let [,b,,d]=[1,2,3,4]; console.log(b,d)"), vec!["2 4"]);
        assert_eq!(run_ok("let [a=1,b=2,c=3]=[10]; console.log(a,b,c)"), vec!["10 2 3"]);
        assert_eq!(run_ok("let [first,...rest]=[1,2,3,4]; console.log(first, rest.join(','))"), vec!["1 2,3,4"]);
        assert_eq!(run_ok("let [a,b,...rest]=[1]; console.log(a,b,rest.length)"), vec!["1 undefined 0"]);
        // A string is iterable for array destructuring.
        assert_eq!(run_ok("let [h,...t]='hello'; console.log(h, t.join(''))"), vec!["h ello"]);
        // Nested patterns, arbitrary depth.
        assert_eq!(run_ok("let {a:{b}}={a:{b:42}}; console.log(b)"), vec!["42"]);
        assert_eq!(run_ok("let [[a,b],[c]]=[[1,2],[3]]; console.log(a,b,c)"), vec!["1 2 3"]);
        assert_eq!(run_ok("let {p:[m,n]}={p:[7,8]}; console.log(m,n)"), vec!["7 8"]);
        // Computed key.
        assert_eq!(run_ok("let k='x'; let {[k]:v}={x:99}; console.log(v)"), vec!["99"]);
        // Object rest: collects the remaining own keys into a new object.
        assert_eq!(run_ok("let {a,...rest}={a:1,b:2,c:3}; console.log(a, JSON.stringify(rest))"), vec![r#"1 {"b":2,"c":3}"#]);
        assert_eq!(run_ok("let {a:x,...rest}={a:1,b:2}; console.log(x, JSON.stringify(rest))"), vec![r#"1 {"b":2}"#]);
        assert_eq!(run_ok("let f=({id,...opts})=>id+':'+JSON.stringify(opts); console.log(f({id:1,a:2,b:3}))"), vec![r#"1:{"a":2,"b":3}"#]);
        // Inside a function; a destructured local captured by a closure.
        assert_eq!(run_ok("function f(o){let {a,b}=o; return a+b} console.log(f({a:3,b:4}))"), vec!["7"]);
        assert_eq!(run_ok("function mk(){let [a,b]=[1,2]; return ()=>a+b} console.log(mk()())"), vec!["3"]);
    }

    #[test]
    fn number_to_radix_and_array_ctor() {
        // Number.toString(radix).
        assert_eq!(run_ok("console.log((255).toString(16), (255).toString(2), (10).toString())"), vec!["ff 11111111 10"]);
        assert_eq!(run_ok("console.log((-42).toString(16), (35).toString(36), (3735928559).toString(16))"), vec!["-2a z deadbeef"]);
        // new Array(n) → n holes; new Array(a,b,…) / Array(...) → the args.
        assert_eq!(run_ok("console.log(new Array(3).length, new Array(3).fill(0).join(','))"), vec!["3 0,0,0"]);
        assert_eq!(run_ok("console.log(new Array(1,2,3).join(','), Array(4,5).join(','))"), vec!["1,2,3 4,5"]);
        assert_eq!(run_ok("console.log(Array(3).fill(7).map((x,i)=>x+i).join(','))"), vec!["7,8,9"]);
        // Invalid length throws a RangeError; new Object()/Object() → {}.
        assert_eq!(run_ok("let e='ok'; try{ new Array(-1); }catch(x){ e='threw'; } console.log(e)"), vec!["threw"]);
        assert_eq!(run_ok("let o=new Object(); o.x=1; console.log(o.x, JSON.stringify(Object()))"), vec!["1 {}"]);
    }

    #[test]
    fn static_builtins() {
        // Array.from over array / string / array-like, with and without a map fn.
        assert_eq!(run_ok("console.log(Array.from([1,2,3],x=>x*2).join(','))"), vec!["2,4,6"]);
        assert_eq!(run_ok("console.log(Array.from({length:3},(_, i)=>i).join(','))"), vec!["0,1,2"]);
        assert_eq!(run_ok("console.log(Array.from('abc').join('-'))"), vec!["a-b-c"]);
        assert_eq!(run_ok("console.log(Array.of(1,2,3).join(','), Array.of(7).length)"), vec!["1,2,3 1"]);
        // Object.assign mutates + returns the target.
        assert_eq!(run_ok("let t={a:1}; let r=Object.assign(t,{a:9,b:2}); console.log(r===t, t.a, t.b)"), vec!["true 9 2"]);
        // String.fromCharCode.
        assert_eq!(run_ok("console.log(String.fromCharCode(72,73,33))"), vec!["HI!"]);
        // Number.isX (no coercion).
        assert_eq!(run_ok("console.log(Number.isInteger(5), Number.isInteger(5.5), Number.isInteger('5'))"), vec!["true false false"]);
        assert_eq!(run_ok("console.log(Number.isSafeInteger(2**53-1), Number.isSafeInteger(2**53))"), vec!["true false"]);
        // Math.max/min spread (incl. mixed plain + spread args).
        assert_eq!(run_ok("let a=[4,2,8,1]; console.log(Math.max(...a), Math.min(...a), Math.max(1,...[5,3],10))"), vec!["8 1 10"]);
        // .at() with negative indexing on arrays and strings.
        assert_eq!(run_ok("console.log([10,20,30].at(-1), [1,2].at(5))"), vec!["30 undefined"]);
        assert_eq!(run_ok("console.log('hello'.at(-1), 'hi'.at(10))"), vec!["o undefined"]);
    }

    #[test]
    fn nullish_and_logical_assign() {
        // ?? keeps the left unless null/undefined (0 and "" are kept).
        assert_eq!(run_ok("console.log(null ?? 5, 0 ?? 9, undefined ?? 'x', '' ?? 'y')"), vec!["5 0 x "]);
        // Logical assignment, short-circuit (RHS not evaluated when skipped).
        assert_eq!(run_ok("let a=0; a||=7; let b=1; b&&=9; console.log(a,b)"), vec!["7 9"]);
        assert_eq!(run_ok("let x=5; x??=10; let y=null; y??=20; console.log(x,y)"), vec!["5 20"]);
        assert_eq!(run_ok("let cnt=0; function f(){cnt++;return 5} let v=1; v||=f(); console.log(v,cnt)"), vec!["1 0"]);
        // Member logical assignment + the counter idiom.
        assert_eq!(run_ok("let o={}; o.a ??= 1; o.a ??= 2; console.log(o.a)"), vec!["1"]);
        assert_eq!(run_ok("let c={}; for(let k of ['a','b','a']){ c[k] ??= 0; c[k]++; } console.log(c.a, c.b)"), vec!["2 1"]);
    }

    #[test]
    fn compound_and_update_assignment() {
        // All arithmetic/bitwise compound operators on a local.
        assert_eq!(run_ok("let a=10; a/=2; a%=3; a**=3; console.log(a)"), vec!["8"]);
        assert_eq!(run_ok("let f=1; f<<=4; f|=1; f&=0xF; f^=2; console.log(f)"), vec!["3"]);
        // Compound + update on members (property and index).
        assert_eq!(run_ok("let o={n:10}; o.n+=5; o.n*=2; console.log(o.n)"), vec!["30"]);
        assert_eq!(run_ok("let a=[1,2,3]; a[0]+=10; a[1]*=3; console.log(a.join(','))"), vec!["11,6,3"]);
        assert_eq!(run_ok("let o={n:5}; let r=[o.n++, o.n, ++o.n]; console.log(r.join(','))"), vec!["5,6,7"]);
        assert_eq!(run_ok("let a=[10,20]; let r=[a[0]++, a[0], --a[1]]; console.log(r.join(','))"), vec!["10,11,19"]);
    }

    #[test]
    fn object_spread_and_computed_keys() {
        assert_eq!(run_ok("let o={a:1,...{b:2,c:3}}; console.log(o.a,o.b,o.c)"), vec!["1 2 3"]);
        // Later properties win over a spread; array source spreads as index keys.
        assert_eq!(run_ok("let base={x:1,y:2}; let o={...base, y:9, z:3}; console.log(o.x,o.y,o.z)"), vec!["1 9 3"]);
        assert_eq!(run_ok("let o={...[10,20]}; console.log(o[0],o[1])"), vec!["10 20"]);
        // null/undefined spread is a no-op.
        assert_eq!(run_ok("let o={...null,...undefined,a:1}; console.log(o.a, Object.keys(o).length)"), vec!["1 1"]);
        // Computed keys, including a template-literal key.
        assert_eq!(run_ok("let k='dyn'; let o={[k]:42,[`a${1}`]:7}; console.log(o.dyn,o.a1)"), vec!["42 7"]);
    }

    #[test]
    fn bitwise_and_exponent() {
        assert_eq!(run_ok("console.log(5 & 3, 5 | 2, 5 ^ 1, ~5)"), vec!["1 7 4 -6"]);
        assert_eq!(run_ok("console.log(1<<4, 256>>2, -8>>1)"), vec!["16 64 -4"]);
        // Unsigned right shift yields a uint32 (can exceed i32::MAX).
        assert_eq!(run_ok("console.log(-1>>>0, (1<<31)>>>0, -1>>>28)"), vec!["4294967295 2147483648 15"]);
        // The canonical (x*31+c)|0 hash idiom.
        assert_eq!(run_ok("let h=0; for(let i=0;i<5;i++) h=(h*31 + i)|0; console.log(h)"), vec!["31810"]);
        // Exponentiation, right-associative.
        assert_eq!(run_ok("console.log(2**10, (-2)**3, 2**3**2, 10**-2)"), vec!["1024 -8 512 0.01"]);
        // Operands coerce via ToInt32 (bool/string/null/undefined/NaN/float).
        assert_eq!(run_ok("console.log(true & 1, '5'|0, null|0, undefined|0, 3.9|0, NaN|0)"), vec!["1 5 0 0 3 0"]);
    }

    #[test]
    fn assignment_destructuring() {
        // The swap idiom + plain array targets.
        assert_eq!(run_ok("let a=1,b=2; [a,b]=[b,a]; console.log(a,b)"), vec!["2 1"]);
        assert_eq!(run_ok("let a,b,c; [a,b,c]=[10,20,30]; console.log(a+b+c)"), vec!["60"]);
        // Rest and defaults in an assignment target.
        assert_eq!(run_ok("let a,r; [a,...r]=[1,2,3,4]; console.log(a, r.join(','))"), vec!["1 2,3,4"]);
        assert_eq!(run_ok("let a,b; [a=5,b=9]=[1]; console.log(a,b)"), vec!["1 9"]);
        // Object assignment destructuring (shorthand, rename, default).
        assert_eq!(run_ok("let x,y; ({x,y}=({x:1,y:2})); console.log(x+y)"), vec!["3"]);
        assert_eq!(run_ok("let p,q; ({a:p,b:q}=({a:7,b:8})); console.log(p,q)"), vec!["7 8"]);
        assert_eq!(run_ok("let x; ({x=42}=({})); console.log(x)"), vec!["42"]);
        // Member targets and nesting.
        assert_eq!(run_ok("let o={}; [o.a,o.b]=[1,2]; console.log(o.a,o.b)"), vec!["1 2"]);
        assert_eq!(run_ok("let a,b,c; [a,[b,c]]=[1,[2,3]]; console.log(a,b,c)"), vec!["1 2 3"]);
        // The assignment expression evaluates to the right-hand side.
        assert_eq!(run_ok("let a,b; let r=([a,b]=[1,2]); console.log(r.join(','))"), vec!["1,2"]);
        // Object rest in an assignment target (own keys minus the siblings).
        assert_eq!(run_ok("let a,rest; ({a,...rest}=({a:1,b:2,c:3})); console.log(a, JSON.stringify(rest))"), vec![r#"1 {"b":2,"c":3}"#]);
        assert_eq!(run_ok("let x,others; ({a:x,...others}=({a:10,p:1,q:2})); console.log(x, JSON.stringify(others))"), vec![r#"10 {"p":1,"q":2}"#]);
        assert_eq!(run_ok("let a,o={}; ({a,...o.bag}=({a:5,m:6})); console.log(a, JSON.stringify(o.bag))"), vec![r#"5 {"m":6}"#]);
    }

    #[test]
    fn labeled_break_continue() {
        // continue label skips to the next iteration of the labeled outer loop.
        assert_eq!(run_ok("let r=[]; outer: for(let i=0;i<3;i++){ for(let j=0;j<3;j++){ if(j===1) continue outer; r.push(i+''+j); } } console.log(r.join(','))"), vec!["00,10,20"]);
        // break label exits the labeled outer loop entirely.
        assert_eq!(run_ok("let r=[]; outer: for(let i=0;i<3;i++){ for(let j=0;j<3;j++){ if(i===1&&j===1) break outer; r.push(i+''+j); } } console.log(r.join(','))"), vec!["00,01,02,10"]);
        // Works over for-of, and with a labeled break inside nested labels.
        assert_eq!(run_ok("let r=[]; loop: for(let x of [1,2,3]){ for(let y of [10,20]){ if(y===20) continue loop; r.push(x*y); } } console.log(r.join(','))"), vec!["10,20,30"]);
        assert_eq!(run_ok("let r=[]; a: for(let i=0;i<2;i++) b: for(let j=0;j<3;j++){ if(j===2) break a; r.push(j); } console.log(r.join(','))"), vec!["0,1"]);
        // A label on a block makes `break label` exit the block.
        assert_eq!(run_ok("let r=[]; blk:{ r.push(1); break blk; r.push(2); } console.log(r.join(','))"), vec!["1"]);
    }

    #[test]
    fn for_of_for_in_capture() {
        // A closure capturing a for-of / for-in loop variable resolves it (was a
        // pre-existing bug: the loop var wasn't detected as captured → not boxed).
        // Within-iteration capture+use matches node exactly.
        assert_eq!(run_ok("let out=[]; for(let x of [1,2,3]){ let g=()=>x*10; out.push(g()); } console.log(out.join(','))"), vec!["10,20,30"]);
        assert_eq!(run_ok("let out=[]; for(let k in {a:1,b:2}){ out.push((()=>k)()); } console.log(out.join(','))"), vec!["a,b"]);
        assert_eq!(run_ok("function f(){let r=[]; for(let v of [10,20]){ r.push((()=>v)()); } return r} console.log(f().join(','))"), vec!["10,20"]);
        // Generator loop var captured within the iteration.
        assert_eq!(run_ok("function* g(){yield 1;yield 2} let o=[]; for(let n of g()){ o.push((()=>n+100)()); } console.log(o.join(','))"), vec!["101,102"]);
    }

    #[test]
    fn for_of_destructuring() {
        assert_eq!(run_ok("let r=[]; for(let [a,b] of [[1,2],[3,4]]) r.push(a+b); console.log(r.join(','))"), vec!["3,7"]);
        // The canonical Object.entries idiom.
        assert_eq!(run_ok("let o={x:1,y:2}; let r=[]; for(let [k,v] of Object.entries(o)) r.push(k+'='+v); console.log(r.join(' '))"), vec!["x=1 y=2"]);
        assert_eq!(run_ok("let r=[]; for(let {n} of [{n:'a'},{n:'b'}]) r.push(n); console.log(r.join(''))"), vec!["ab"]);
        // Rest and defaults in the head.
        assert_eq!(run_ok("let r=[]; for(let [a,...t] of [[1,2,3]]) r.push(a+':'+t.join(',')); console.log(r[0])"), vec!["1:2,3"]);
        assert_eq!(run_ok("let r=[]; for(let {a,b=9} of [{a:1,b:2},{a:3}]) r.push(a+''+b); console.log(r.join(' '))"), vec!["12 39"]);
        // Captured destructured loop var.
        assert_eq!(run_ok("let f; for(let [a,b] of [[1,2]]) f=()=>a+b; console.log(f())"), vec!["3"]);
    }

    #[test]
    fn function_inspect_label() {
        // Named functions / methods show their name; truly anonymous ones don't.
        assert_eq!(run_ok("function foo(){} console.log(foo)"), vec!["[Function: foo]"]);
        assert_eq!(run_ok("console.log([function named(){}, x=>x])"), vec!["[ [Function: named], [Function (anonymous)] ]"]);
        assert_eq!(run_ok("class A{m(){}} console.log(new A().m)"), vec!["[Function: m]"]);
    }

    #[test]
    fn function_name_and_length() {
        // .name: declaration, named expression, class, and inference for an
        // anonymous arrow / function expression bound to a variable.
        assert_eq!(run_ok("function foo(){} console.log(foo.name)"), vec!["foo"]);
        assert_eq!(run_ok("let q=function named(){}; console.log(q.name)"), vec!["named"]);
        assert_eq!(run_ok("const baz=()=>{}; console.log(baz.name)"), vec!["baz"]);
        assert_eq!(run_ok("const bar=function(){}; console.log(bar.name)"), vec!["bar"]);
        assert_eq!(run_ok("class C{} console.log(C.name)"), vec!["C"]);
        // A truly anonymous function (in an array) has an empty name.
        assert_eq!(run_ok("console.log([x=>x][0].name === '')"), vec!["true"]);
        // .length: declared parameter count (rest excluded).
        assert_eq!(run_ok("function f(a,b,c){} console.log(f.length, ((x,y)=>{}).length, (()=>{}).length)"), vec!["3 2 0"]);
        assert_eq!(run_ok("function r(a,...rest){} console.log(r.length)"), vec!["1"]);
        assert_eq!(run_ok("class C{constructor(a,b){}} console.log(C.length)"), vec!["2"]);
    }

    #[test]
    fn promises() {
        // resolve/reject + then/catch; chaining; a throw in then routes to catch.
        assert_eq!(run_ok("Promise.resolve(5).then(v=>console.log('got',v))"), vec!["got 5"]);
        assert_eq!(run_ok("Promise.reject('e').catch(e=>console.log('caught',e))"), vec!["caught e"]);
        assert_eq!(run_ok("Promise.resolve(1).then(v=>v+1).then(v=>console.log(v))"), vec!["2"]);
        assert_eq!(run_ok("Promise.resolve(1).then(v=>{throw 'x'}).catch(e=>console.log('c:'+e))"), vec!["c:x"]);
        // The defining ordering property: reactions run as microtasks AFTER sync.
        assert_eq!(run_ok("console.log('A'); Promise.resolve().then(()=>console.log('C')); console.log('B')"), vec!["A", "B", "C"]);
        // new Promise: resolve, reject, chaining, and adopting a returned promise.
        assert_eq!(run_ok("new Promise(res=>res(42)).then(v=>console.log(v))"), vec!["42"]);
        assert_eq!(run_ok("new Promise((res,rej)=>rej('bad')).catch(e=>console.log('err',e))"), vec!["err bad"]);
        assert_eq!(run_ok("new Promise(r=>r(Promise.resolve(99))).then(v=>console.log(v))"), vec!["99"]);
        // A promise resolved later by a stored resolver.
        assert_eq!(run_ok("let r; let p=new Promise(res=>{r=res}); p.then(v=>console.log('late',v)); r(7)"), vec!["late 7"]);
        // The executor captures an outer variable (regression: capture analysis
        // must descend into `new` arguments to box `v`).
        assert_eq!(run_ok("function delay(v){return new Promise(res=>res(v))} delay(9).then(x=>console.log('d',x))"), vec!["d 9"]);
        // finally runs on both paths and passes the value/reason through.
        assert_eq!(run_ok("Promise.resolve(1).finally(()=>console.log('cleanup')).then(v=>console.log('v',v))"), vec!["cleanup", "v 1"]);
        assert_eq!(run_ok("console.log(typeof Promise.resolve(1))"), vec!["object"]);
    }

    #[test]
    fn promise_combinators() {
        // all: array of values (mixed plain + promise); first rejection wins; empty.
        assert_eq!(run_ok("Promise.all([1,Promise.resolve(2),3]).then(a=>console.log(a.join(',')))"), vec!["1,2,3"]);
        assert_eq!(run_ok("Promise.all([Promise.resolve(1),Promise.reject('x'),Promise.resolve(3)]).catch(e=>console.log('r',e))"), vec!["r x"]);
        assert_eq!(run_ok("Promise.all([]).then(a=>console.log(a.length))"), vec!["0"]);
        // race: first to settle (fulfil or reject).
        assert_eq!(run_ok("Promise.race([Promise.resolve('fast'),Promise.reject('slow')]).then(v=>console.log(v))"), vec!["fast"]);
        assert_eq!(run_ok("Promise.race([Promise.reject('boom'),Promise.resolve('ok')]).catch(e=>console.log('r',e))"), vec!["r boom"]);
        // allSettled: status records on both paths.
        assert_eq!(
            run_ok("Promise.allSettled([Promise.resolve(1),Promise.reject('e')]).then(rs=>console.log(rs.map(r=>r.status+(r.status==='fulfilled'?r.value:r.reason)).join(',')))"),
            vec!["fulfilled1,rejectede"]
        );
        // any: first fulfilment; all-reject → AggregateError; empty → AggregateError.
        assert_eq!(run_ok("Promise.any([Promise.reject('a'),Promise.resolve('win')]).then(v=>console.log(v))"), vec!["win"]);
        assert_eq!(run_ok("Promise.any([Promise.reject('e1'),Promise.reject('e2')]).catch(e=>console.log(e.name,e.errors.join(',')))"), vec!["AggregateError e1,e2"]);
        assert_eq!(run_ok("Promise.any([]).catch(e=>console.log(e.name))"), vec!["AggregateError"]);
        // Integrates with await + destructuring.
        assert_eq!(run_ok("async function f(){let [a,b]=await Promise.all([Promise.resolve(10),Promise.resolve(20)]); return a+b} f().then(v=>console.log(v))"), vec!["30"]);
    }

    #[test]
    fn generators() {
        // Manual next(): values then done; return value reported once.
        assert_eq!(run_ok("function* g(){yield 1;yield 2} let it=g(); console.log(it.next().value,it.next().value,it.next().done)"), vec!["1 2 true"]);
        assert_eq!(run_ok("function* g(){yield 1; return 9} let it=g(); console.log(JSON.stringify(it.next()),JSON.stringify(it.next()),JSON.stringify(it.next()))"), vec![r#"{"value":1,"done":false} {"value":9,"done":true} {"done":true}"#]);
        // Empty generator; value sent into a yield expression.
        assert_eq!(run_ok("function* g(){} console.log(g().next().done)"), vec!["true"]);
        assert_eq!(run_ok("function* g(){let x=yield 1; yield x+10} let it=g(); console.log(it.next().value, it.next(5).value)"), vec!["1 15"]);
        // for-of over a generator: direct call AND via a variable.
        assert_eq!(run_ok("function* g(){yield 1;yield 2;yield 3} let s=0; for(let x of g()) s+=x; console.log(s)"), vec!["6"]);
        assert_eq!(run_ok("function* g(){yield 1;yield 2} let gen=g(); let r=[]; for(let x of gen) r.push(x); console.log(r.join(','))"), vec!["1,2"]);
        // for-of destructuring a generator's elements.
        assert_eq!(run_ok("function* g(){yield [1,2]; yield [3,4]} let r=[]; for(let [a,b] of g()) r.push(a+b); console.log(r.join(','))"), vec!["3,7"]);
        // Infinite generator with break terminates (lazy pull).
        assert_eq!(run_ok("function* nat(){let i=0; while(true) yield i++} let r=[]; for(let x of nat()){ if(x>=4) break; r.push(x); } console.log(r.join(','))"), vec!["0,1,2,3"]);
        // Spread and Array.from drain a finite generator.
        assert_eq!(run_ok("function* g(){yield 1;yield 2;yield 3} console.log([...g()].join('-'), Array.from(g()).length)"), vec!["1-2-3 3"]);
        // A generator using a captured outer variable + a range helper.
        assert_eq!(run_ok("function* range(n){for(let i=0;i<n;i++) yield i*i} console.log([...range(4)].join(','))"), vec!["0,1,4,9"]);
        // typeof and inspect.
        assert_eq!(run_ok("function* g(){} console.log(typeof g, typeof g())"), vec!["function object"]);
    }

    #[test]
    fn generator_methods_and_yield_star() {
        // Object and class generator methods (incl. using `this` and static).
        assert_eq!(run_ok("let o={*gen(){yield 1;yield 2}}; console.log([...o.gen()].join(','))"), vec!["1,2"]);
        assert_eq!(run_ok("class C{*vals(){yield 1;yield 2}} console.log([...new C().vals()].join(','))"), vec!["1,2"]);
        assert_eq!(run_ok("class C{constructor(){this.xs=[10,20,30]} *each(){for(let x of this.xs) yield x}} console.log([...new C().each()].join(','))"), vec!["10,20,30"]);
        assert_eq!(run_ok("class C{static *make(){yield 1;yield 2}} console.log([...C.make()].join(','))"), vec!["1,2"]);
        // yield* delegation over a generator, array, string, and nested.
        assert_eq!(run_ok("function* inner(){yield 1;yield 2} function* outer(){yield* inner(); yield 3} console.log([...outer()].join(','))"), vec!["1,2,3"]);
        assert_eq!(run_ok("function* g(){yield* [1,2,3]; yield* 'ab'} console.log([...g()].join(','))"), vec!["1,2,3,a,b"]);
        assert_eq!(run_ok("function* g(){yield 0; yield* [1,2]; yield 3} console.log([...g()].join(','))"), vec!["0,1,2,3"]);
        assert_eq!(run_ok("function* nest(){yield* (function*(){yield* [1,2]})()} console.log([...nest()].join(','))"), vec!["1,2"]);
    }

    #[test]
    fn async_await() {
        // An async function returns a Promise; its body's `return` fulfills it.
        assert_eq!(run_ok("async function f(){return 1} f().then(v=>console.log('v',v))"), vec!["v 1"]);
        // await a non-promise (still yields a microtask tick) and a real promise.
        assert_eq!(run_ok("async function f(){let x=await 5; return x+10} f().then(v=>console.log(v))"), vec!["15"]);
        assert_eq!(run_ok("async function f(){let x=await Promise.resolve(3); let y=await Promise.resolve(4); return x*y} f().then(v=>console.log(v))"), vec!["12"]);
        // Rejection caught by try/catch around the await.
        assert_eq!(run_ok("async function f(){try{await Promise.reject('boom'); return 'no'}catch(e){return 'caught '+e}} f().then(v=>console.log(v))"), vec!["caught boom"]);
        // Uncaught rejection / a thrown body error reject the returned promise.
        assert_eq!(run_ok("async function f(){await Promise.reject('k')} f().catch(e=>console.log('c',e))"), vec!["c k"]);
        assert_eq!(run_ok("async function f(){throw new Error('x')} f().catch(e=>console.log(e.message))"), vec!["x"]);
        // Ordering: sync runs first; the await suspends and resumes as a microtask.
        assert_eq!(
            run_ok("console.log('start'); async function f(){console.log('before'); await 0; console.log('after')} f(); console.log('end')"),
            vec!["start", "before", "end", "after"]
        );
        // Async calling async + await in a loop, accumulating.
        assert_eq!(
            run_ok("async function dbl(n){return n*2} async function f(){let t=0; for(let i=1;i<=3;i++){t+=await dbl(i)} return t} f().then(v=>console.log(v))"),
            vec!["12"]
        );
        // await a `new Promise` that resolves synchronously with a captured value.
        assert_eq!(
            run_ok("function delay(v){return new Promise(res=>res(v))} async function f(){let a=await delay(10); let b=await delay(20); return a+b} f().then(v=>console.log(v))"),
            vec!["30"]
        );
        // Async arrow.
        assert_eq!(run_ok("const f=async()=>(await Promise.resolve(7))+1; f().then(v=>console.log(v))"), vec!["8"]);
        // typeof of an async call is the Promise object.
        assert_eq!(run_ok("async function f(){} console.log(typeof f())"), vec!["object"]);
        // try/finally around `return await` runs the finally before fulfilling.
        assert_eq!(
            run_ok("async function f(){try{return await Promise.resolve('ok')}finally{console.log('fin')}} f().then(v=>console.log(v))"),
            vec!["fin", "ok"]
        );
        // A rejection thrown in at the await still runs the finally on the way out.
        assert_eq!(
            run_ok("async function f(){try{await Promise.reject('e')}finally{console.log('fin')}} f().catch(e=>console.log('c',e))"),
            vec!["fin", "c e"]
        );
    }

    #[test]
    fn date_basics() {
        // Construct from ms; getTime / toISOString / UTC getters.
        assert_eq!(run_ok("let d=new Date(1577836800000); console.log(d.getTime(), d.toISOString())"), vec!["1577836800000 2020-01-01T00:00:00.000Z"]);
        assert_eq!(run_ok("let d=new Date(Date.UTC(2021,5,15,10,30,45,123)); console.log(d.getUTCFullYear(),d.getUTCMonth(),d.getUTCDate(),d.getUTCHours(),d.getUTCMinutes(),d.getUTCSeconds(),d.getUTCMilliseconds())"), vec!["2021 5 15 10 30 45 123"]);
        // Date.UTC / Date.parse / new Date(string).
        assert_eq!(run_ok("console.log(Date.UTC(2000,0,1))"), vec!["946684800000"]);
        assert_eq!(run_ok("console.log(Date.parse('2020-01-01T00:00:00.000Z'), Date.parse('1970-01-02'))"), vec!["1577836800000 86400000"]);
        assert_eq!(run_ok("console.log(new Date('2020-06-15').toISOString())"), vec!["2020-06-15T00:00:00.000Z"]);
        // Arithmetic (ms diff), comparison, unary + coercion.
        assert_eq!(run_ok("let a=new Date(1000),b=new Date(5000); console.log(b-a, a<b, +a)"), vec!["4000 true 1000"]);
        // Leap day, month overflow, pre-epoch, getUTCDay (2020-01-01 = Wednesday).
        assert_eq!(run_ok("console.log(new Date(Date.UTC(2020,1,29)).toISOString())"), vec!["2020-02-29T00:00:00.000Z"]);
        assert_eq!(run_ok("console.log(new Date(Date.UTC(2020,12,1)).toISOString())"), vec!["2021-01-01T00:00:00.000Z"]);
        assert_eq!(run_ok("console.log(new Date(-86400000).toISOString())"), vec!["1969-12-31T00:00:00.000Z"]);
        assert_eq!(run_ok("console.log(new Date(Date.UTC(2020,0,1)).getUTCDay())"), vec!["3"]);
        // Invalid date; the legacy 2-digit year (constructor only); setFullYear (literal year).
        assert_eq!(run_ok("console.log(new Date('nope').getTime(), String(new Date(NaN)))"), vec!["NaN Invalid Date"]);
        assert_eq!(run_ok("console.log(new Date(Date.UTC(99,0,1)).getUTCFullYear(), Date.parse('0001-01-01T00:00:00.000Z'))"), vec!["1999 -62135596800000"]);
        assert_eq!(run_ok("let d=new Date(0); d.setUTCFullYear(99); console.log(d.getUTCFullYear())"), vec!["99"]);
        // console.log renders a Date as its ISO string (unquoted).
        assert_eq!(run_ok("console.log(new Date(0))"), vec!["1970-01-01T00:00:00.000Z"]);
    }

    #[test]
    fn computed_class_member_keys() {
        // Runtime-computed method / getter / setter / static keys.
        assert_eq!(run_ok("let m='m'+1; class C{[m](){return 7}} console.log(new C().m1())"), vec!["7"]);
        assert_eq!(run_ok("class C{constructor(){this.x=3} ['a'+'b'](){return this.x}} console.log(new C().ab())"), vec!["3"]);
        assert_eq!(run_ok("class C{get [('v'+'al')](){return 42} set [('v'+'al')](z){this.x=z*2} ['get'+'X'](){return this.x}} let c=new C(); c.val=10; console.log(c.val, c.getX())"), vec!["42 20"]);
        assert_eq!(run_ok("class C{static ['s'+'q'](n){return n*n}} console.log(C.sq(5))"), vec!["25"]);
    }

    #[test]
    fn private_class_members() {
        // Private fields read/write/update, a private method, and a private getter.
        assert_eq!(run_ok("class C{#n=0; #step; constructor(s){this.#step=s} inc(){this.#n+=this.#step; return this} get value(){return this.#n} #secret(){return this.#n*2} reveal(){return this.#secret()}} let c=new C(5); c.inc().inc(); console.log(c.value, c.reveal())"), vec!["10 20"]);
        // `this.#n++` update.
        assert_eq!(run_ok("class C{#n=0; bump(){this.#n++; return this.#n}} let c=new C(); console.log(c.bump(), c.bump())"), vec!["1 2"]);
        // A closure capturing `this`/a local that reads a private field.
        assert_eq!(run_ok("class E{#v=7; make(){let s=this; return ()=>s.#v}} console.log(new E().make()())"), vec!["7"]);
        // A private field used inside a method-local closure (map).
        assert_eq!(run_ok("class D{#xs=[1,2,3]; doubled(){return this.#xs.map(x=>x*2)}} console.log(new D().doubled().join(','))"), vec!["2,4,6"]);
    }

    #[test]
    fn computed_method_call_binds_this() {
        // `obj[key](args)` binds `this` to obj (dynamic method dispatch).
        assert_eq!(run_ok("let o={x:10,getX(){return this.x},add(a,b){return this.x+a+b}}; let m='getX'; console.log(o[m](), o['add'](1,2))"), vec!["10 13"]);
        // A dispatch table iterated by name.
        assert_eq!(run_ok("let ops={inc(n){return n+1},dbl(n){return n*2}}; let r=[]; for(let k of ['inc','dbl']) r.push(ops[k](10)); console.log(r.join(','))"), vec!["11,20"]);
        // Computed builtin method on an array.
        assert_eq!(run_ok("let a=[3,1,2]; console.log(a['join']('-'))"), vec!["3-1-2"]);
        // Class instance dynamic method.
        assert_eq!(run_ok("class C{constructor(){this.v=5} double(){return this.v*2}} let c=new C(),n='double'; console.log(c[n]())"), vec!["10"]);
    }

    #[test]
    fn symbol_iterator_custom_iterables() {
        // A custom-iterable object: for-of, spread, and Array.from all drive its
        // `[Symbol.iterator]()`.
        let range = "let range={from:1,to:4,[Symbol.iterator](){let c=this.from,e=this.to;return{next:()=>c<=e?{value:c++,done:false}:{done:true}}}};";
        assert_eq!(run_ok(&format!("{range} let r=[]; for(let x of range) r.push(x); console.log(r.join(','))")), vec!["1,2,3,4"]);
        assert_eq!(run_ok(&format!("{range} console.log([...range].join(','))")), vec!["1,2,3,4"]);
        assert_eq!(run_ok(&format!("{range} console.log(Array.from(range).join(','))")), vec!["1,2,3,4"]);
        // Lazy: a `break` stops pulling from an infinite iterator.
        assert_eq!(
            run_ok("let nat={[Symbol.iterator](){let i=0;return{next:()=>({value:i++,done:false})}}}; let o=[]; for(let n of nat){if(n>=4)break; o.push(n)} console.log(o.join(','))"),
            vec!["0,1,2,3"]
        );
        // A class implementing the protocol (and a closure capturing a method local).
        assert_eq!(
            run_ok("class S{constructor(){this.xs=[1,2,3]} [Symbol.iterator](){let i=this.xs.length,s=this; return{next:()=>i>0?{value:s.xs[--i],done:false}:{done:true}}}} console.log([...new S()].join(','))"),
            vec!["3,2,1"]
        );
        // `obj[Symbol.iterator]` reads the (inherited) method via computed access.
        assert_eq!(run_ok(&format!("{range} console.log(typeof range[Symbol.iterator])")), vec!["function"]);
    }

    #[test]
    fn string_relational_comparison() {
        // `<`/`>`/`<=`/`>=` on two strings compare lexicographically (not numeric).
        assert_eq!(run_ok("console.log('apple'<'banana', 'cherry'<'apple', 'a'<='a', 'b'>'a', 'Z'<'a')"), vec!["true false true true true"]);
        // String vs number falls back to numeric coercion.
        assert_eq!(run_ok("console.log('10'<'9', 10<9, '10'<9)"), vec!["true false false"]);
        // sort with a string comparator, and the default (stringify) sort.
        assert_eq!(run_ok("let s=['banana','apple','cherry','date']; s.sort((x,y)=>x<y?-1:x>y?1:0); console.log(s.join(','))"), vec!["apple,banana,cherry,date"]);
        assert_eq!(run_ok("console.log(['banana','apple','cherry'].sort().join(','))"), vec!["apple,banana,cherry"]);
        // Numeric comparator still goes through the (now native) fast path.
        assert_eq!(run_ok("console.log([5,3,8,1,9,2].sort((a,b)=>a-b).join(','))"), vec!["1,2,3,5,8,9"]);
    }

    #[test]
    fn array_destructure_iterables() {
        // Array destructuring drives the iterator protocol for generators and
        // custom iterables (positional fast path still used for arrays/strings).
        assert_eq!(run_ok("function* g(){yield 1;yield 2;yield 3} let [a,b]=g(); console.log(a,b)"), vec!["1 2"]);
        assert_eq!(run_ok("function* g(){yield 1;yield 2;yield 3} let [a,...r]=g(); console.log(a,r.join(','))"), vec!["1 2,3"]);
        assert_eq!(run_ok("let it={[Symbol.iterator](){let i=0;return{next:()=>({value:i++,done:false})}}}; let [a,b,c]=it; console.log(a,b,c)"), vec!["0 1 2"]); // infinite iterator, bounded pull (no hang)
        assert_eq!(run_ok("let [a,b]=[10,20]; console.log(a,b)"), vec!["10 20"]);          // array fast path
        assert_eq!(run_ok("let [x,y]='hi'; console.log(x,y)"), vec!["h i"]);              // string
        assert_eq!(run_ok("let m=new Map([['k',1]]); let [[k,v]]=m; console.log(k,v)"), vec!["k 1"]); // map entries
    }

    #[test]
    fn map_basics() {
        assert_eq!(run_ok("let m=new Map(); m.set('a',1).set('b',2); console.log(m.get('a'),m.get('b'),m.size,m.has('a'),m.has('z'))"), vec!["1 2 2 true false"]);
        assert_eq!(run_ok("let m=new Map([['x',10],['y',20]]); console.log(m.get('x'),m.get('y'),m.size)"), vec!["10 20 2"]);
        // set on an existing key updates in place (one entry); delete returns bool.
        assert_eq!(run_ok("let m=new Map(); m.set(1,'a'); m.set(1,'b'); console.log(m.get(1),m.size)"), vec!["b 1"]);
        assert_eq!(run_ok("let m=new Map([[1,1]]); console.log(m.delete(1),m.delete(1),m.size)"), vec!["true false 0"]);
        // Iteration: for-of entries, keys/values, forEach(value,key), spread.
        assert_eq!(run_ok("let m=new Map([['a',1],['b',2]]); let r=[]; for(let [k,v] of m) r.push(k+v); console.log(r.join(','))"), vec!["a1,b2"]);
        assert_eq!(run_ok("let m=new Map([['a',1],['b',2]]); console.log([...m.keys()].join(','), [...m.values()].join(','))"), vec!["a,b 1,2"]);
        assert_eq!(run_ok("let m=new Map([['a',1]]); let r=[]; m.forEach((v,k)=>r.push(k+'='+v)); console.log(r.join(','))"), vec!["a=1"]);
        // SameValueZero keys: NaN dedupes, -0/+0 collapse, objects by identity, no coercion.
        assert_eq!(run_ok("let m=new Map(); m.set(NaN,1).set(NaN,2); console.log(m.size,m.get(NaN))"), vec!["1 2"]);
        assert_eq!(run_ok("let m=new Map(); m.set(-0,'z'); console.log(m.get(0), m.has(0))"), vec!["z true"]);
        assert_eq!(run_ok("let m=new Map(); m.set(1,'n'); console.log(m.get('1'))"), vec!["undefined"]);
        // console.log + JSON shape.
        assert_eq!(run_ok("console.log(new Map([['a',1],['b',2]]))"), vec!["Map(2) { 'a' => 1, 'b' => 2 }"]);
        assert_eq!(run_ok("console.log(JSON.stringify({m:new Map([[1,2]])}))"), vec![r#"{"m":{}}"#]);
    }

    #[test]
    fn set_basics() {
        assert_eq!(run_ok("let s=new Set([1,2,2,3]); console.log(s.size, s.has(2), s.has(9))"), vec!["3 true false"]);
        assert_eq!(run_ok("let s=new Set(); s.add(1).add(2).add(1); console.log(s.size, [...s].join(','))"), vec!["2 1,2"]);
        assert_eq!(run_ok("let s=new Set([1,2,3]); console.log(s.delete(2), s.size, [...s].join(','))"), vec!["true 2 1,3"]);
        // Set from a string iterates chars (deduped).
        assert_eq!(run_ok("let s=new Set('aabbc'); console.log(s.size, [...s].join(''))"), vec!["3 abc"]);
        // for-of yields values; forEach; NaN dedupe.
        assert_eq!(run_ok("let r=[]; for(let v of new Set([10,20])) r.push(v); console.log(r.join(','))"), vec!["10,20"]);
        assert_eq!(run_ok("let s=new Set([1,2,3]); let t=0; s.forEach(v=>t+=v); console.log(t)"), vec!["6"]);
        assert_eq!(run_ok("console.log(new Set([NaN,NaN]).size)"), vec!["1"]);
        // The canonical dedupe idiom + console.log.
        assert_eq!(run_ok("console.log([...new Set([3,1,3,2,1])].join(','))"), vec!["3,1,2"]);
        assert_eq!(run_ok("console.log(new Set([1,2]))"), vec!["Set(2) { 1, 2 }"]);
    }

    #[test]
    fn classes() {
        // Constructor + method + this.
        assert_eq!(run_ok("class A{constructor(x){this.x=x} get(){return this.x}} console.log(new A(5).get())"), vec!["5"]);
        // Class fields (with and without initializers) + field mutation.
        assert_eq!(run_ok("class C{count=0; inc(){this.count++; return this.count}} let c=new C(); console.log(c.inc(), c.inc())"), vec!["1 2"]);
        // Method chaining via `return this`; method calling another method.
        assert_eq!(run_ok("class K{constructor(){this.v=0} add(n){this.v+=n;return this} val(){return this.v}} console.log(new K().add(3).add(4).val())"), vec!["7"]);
        assert_eq!(run_ok("class A{constructor(n){this.n=n} d(){return this.n*2} q(){return this.d()*2}} console.log(new A(5).q())"), vec!["20"]);
        // A constructor returning an object replaces the instance.
        assert_eq!(run_ok("class W{constructor(){return {custom:true}}} console.log(new W().custom)"), vec!["true"]);
        // Methods are non-enumerable: keys/JSON show only fields.
        assert_eq!(run_ok("class A{constructor(){this.k=1;this.j=2} m(){}} let a=new A(); console.log(Object.keys(a).join(','), JSON.stringify(a))"), vec![r#"k,j {"k":1,"j":2}"#]);
        // instanceof for user classes; typeof a class is "function".
        assert_eq!(run_ok("class A{} class B{} let a=new A(); console.log(a instanceof A, a instanceof B, typeof A)"), vec!["true false function"]);
        // Arrays of instances; console.log prints the constructor name.
        assert_eq!(run_ok("class Pt{constructor(x){this.x=x}} console.log([new Pt(1),new Pt(2)].map(p=>p.x).join(','))"), vec!["1,2"]);
        assert_eq!(run_ok("class Pt{constructor(x,y){this.x=x;this.y=y}} console.log(new Pt(3,4))"), vec!["Pt { x: 3, y: 4 }"]);
        // Getters: invoked on read (this = instance), not enumerable.
        assert_eq!(run_ok("class C{constructor(){this.items=[1,2,3]} get size(){return this.items.length}} console.log(new C().size)"), vec!["3"]);
        assert_eq!(run_ok("class C{constructor(){this.n=1} get d(){return this.n*2}} let c=new C(); console.log(c.d, Object.keys(c).join(','))"), vec!["2 n"]);
        // Setters: invoked on write; get/set pair; setter-only; inherited; own
        // data property still shadows.
        assert_eq!(run_ok("class T{constructor(c){this._c=c} get c(){return this._c} set c(v){this._c=v*2}} let t=new T(5); console.log(t.c); t.c=10; console.log(t.c)"), vec!["5", "20"]);
        assert_eq!(run_ok("class L{set m(v){this.last='['+v+']'}} let l=new L(); l.m='hi'; console.log(l.last)"), vec!["[hi]"]);
        assert_eq!(run_ok("class B{set v(x){this._v=x*2} get v(){return this._v}} class D extends B{} let d=new D(); d.v=21; console.log(d.v)"), vec!["42"]);
        assert_eq!(run_ok("class P{constructor(){this.x=1}} let p=new P(); p.x=5; p.y=9; console.log(p.x,p.y)"), vec!["5 9"]);
        // Static methods + fields; instances don't see statics.
        assert_eq!(run_ok("class M{static sq(n){return n*n}} console.log(M.sq(5))"), vec!["25"]);
        assert_eq!(run_ok("class Cfg{static V='1.0'; static MAX=100} console.log(Cfg.V, Cfg.MAX)"), vec!["1.0 100"]);
        assert_eq!(run_ok("class C{static n=0; constructor(){C.n++; this.id=C.n}} let a=new C(),b=new C(); console.log(a.id,b.id,C.n)"), vec!["1 2 2"]);
        assert_eq!(run_ok("class A{static s(){return 1}} let a=new A(); console.log(typeof a.s, typeof A.s)"), vec!["undefined function"]);
    }

    #[test]
    fn class_inheritance() {
        // Inherited method; instanceof up the chain.
        assert_eq!(run_ok("class A{m(){return 1}} class B extends A{} let b=new B(); console.log(b.m(), b instanceof A, b instanceof B)"), vec!["1 true true"]);
        // super(args) in a derived constructor; fields after super.
        assert_eq!(run_ok("class A{constructor(x){this.x=x}} class B extends A{constructor(x,y){super(x);this.y=y}} let b=new B(3,4); console.log(b.x,b.y)"), vec!["3 4"]);
        // Implicit super forwards constructor args.
        assert_eq!(run_ok("class A{constructor(n){this.n=n}} class B extends A{} console.log(new B(7).n)"), vec!["7"]);
        // super.method() and override.
        assert_eq!(run_ok("class A{g(){return 'A'}} class B extends A{g(){return 'B->'+super.g()}} console.log(new B().g())"), vec!["B->A"]);
        assert_eq!(run_ok("class Animal{constructor(n){this.name=n} speak(){return this.name+' sound'}} class Dog extends Animal{speak(){return this.name+' barks'}} console.log(new Dog('Rex').speak())"), vec!["Rex barks"]);
        // Inherited fields; 3-level chain.
        assert_eq!(run_ok("class A{x=1} class B extends A{y=2} let b=new B(); console.log(b.x,b.y)"), vec!["1 2"]);
        assert_eq!(run_ok("class A{constructor(){this.t='a'}} class B extends A{} class C extends B{} console.log(new C().t, new C() instanceof A)"), vec!["a true"]);
        // Inherited static method.
        assert_eq!(run_ok("class A{static make(){return 'A'}} class B extends A{} console.log(B.make())"), vec!["A"]);
    }

    #[test]
    fn instanceof_operator() {
        // Built-in collections / functions.
        assert_eq!(run_ok("console.log([] instanceof Array, [] instanceof Object)"), vec!["true true"]);
        assert_eq!(run_ok("console.log(({}) instanceof Object, ({}) instanceof Array)"), vec!["true false"]);
        assert_eq!(run_ok("let f=x=>x; console.log(f instanceof Function, f instanceof Object)"), vec!["true true"]);
        // Primitives are never instances.
        assert_eq!(run_ok("console.log(5 instanceof Object, 's' instanceof Object, null instanceof Object)"), vec!["false false false"]);
        // Error hierarchy: a subtype is also an Error; siblings don't match.
        assert_eq!(run_ok("let e=new TypeError('x'); console.log(e instanceof TypeError, e instanceof Error, e instanceof RangeError)"), vec!["true true false"]);
        // Engine-thrown errors are real Error objects (name/message/instanceof).
        assert_eq!(run_ok("try{null.x}catch(e){console.log(e instanceof TypeError, e.name)}"), vec!["true TypeError"]);
        assert_eq!(run_ok("try{let a=[];a.length=-1}catch(e){console.log(e instanceof RangeError)}"), vec!["true"]);
    }

    #[test]
    fn is_nan_is_finite() {
        assert_eq!(run_ok("console.log(isNaN(NaN), isNaN(5), isNaN('x'), isNaN('12'))"), vec!["true false true false"]);
        assert_eq!(run_ok("console.log(isFinite(5), isFinite(Infinity), isFinite(NaN), isFinite('3'))"), vec!["true false false true"]);
    }

    #[test]
    fn destructuring_parameters() {
        // The common .map(([k,v])=>…) over entries; arrow object-pattern param.
        assert_eq!(run_ok("console.log(Object.entries({a:1,b:2}).map(([k,v])=>k+v).join(','))"), vec!["a1,b2"]);
        assert_eq!(run_ok("let f=({x,y})=>x+y; console.log(f({x:3,y:4}))"), vec!["7"]);
        // Function with mixed array + object pattern params.
        assert_eq!(run_ok("function f([a,b],{c}){return a+b+c} console.log(f([1,2],{c:3}))"), vec!["6"]);
        // Defaults and rest inside a pattern parameter.
        assert_eq!(run_ok("let f=({a,b=10})=>a+b; console.log(f({a:1}), f({a:1,b:2}))"), vec!["11 3"]);
        assert_eq!(run_ok("let f=([a,...rest])=>a+':'+rest.join(','); console.log(f([1,2,3,4]))"), vec!["1:2,3,4"]);
        // forEach with a pattern param; pattern param captured by a closure.
        assert_eq!(run_ok("let r=[]; [[1,2],[3,4]].forEach(([a,b])=>r.push(a+b)); console.log(r.join(','))"), vec!["3,7"]);
        assert_eq!(run_ok("let fns=[[1,2],[3,4]].map(([a,b])=>()=>a+b); console.log(fns[0](),fns[1]())"), vec!["3 7"]);
    }

    #[test]
    fn rest_parameters() {
        // Pure rest, rest after fixed params, empty rest.
        assert_eq!(run_ok("function f(...a){return a.length} console.log(f(1,2,3))"), vec!["3"]);
        assert_eq!(run_ok("function f(a,...b){return a+':'+b.join(',')} console.log(f(1,2,3,4))"), vec!["1:2,3,4"]);
        assert_eq!(run_ok("function f(a,...b){return b.length} console.log(f(1))"), vec!["0"]);
        // Arrow rest.
        assert_eq!(run_ok("let g=(...xs)=>xs.reduce((a,b)=>a+b,0); console.log(g(1,2,3,4))"), vec!["10"]);
        // Rest fed by spread (the two halves compose).
        assert_eq!(run_ok("function f(...a){return a.join(',')} console.log(f(...[1,2,3],4))"), vec!["1,2,3,4"]);
        // Rest array captured by an inner closure (boxed into a cell).
        assert_eq!(run_ok("function f(...a){return ()=>a.length} console.log(f(1,2,3)())"), vec!["3"]);
        // Rest method keeps `this`.
        assert_eq!(run_ok("let o={n:5,f(...xs){return this.n+xs.length}}; console.log(o.f(1,2))"), vec!["7"]);
    }

    #[test]
    fn in_operator_and_more_methods() {
        // `in`: own object keys, array indices/length, class-instance inherited
        // methods, Map/Set size. (Plain-object Object.prototype methods aren't
        // inherited here — no prototype chain.)
        assert_eq!(run_ok("let o={a:1,b:2}; console.log('a' in o, 'c' in o)"), vec!["true false"]);
        assert_eq!(run_ok("console.log(0 in [1,2], 5 in [1,2], 'length' in [])"), vec!["true false true"]);
        assert_eq!(run_ok("class A{m(){}} let a=new A(); a.x=1; console.log('m' in a, 'x' in a, 'y' in a)"), vec!["true true false"]);
        assert_eq!(run_ok("class A{am(){}} class B extends A{} console.log('am' in new B())"), vec!["true"]);
        assert_eq!(run_ok("console.log('size' in new Map())"), vec!["true"]);
        // reduceRight (with and without an initial value).
        assert_eq!(run_ok("console.log([1,2,3].reduceRight((a,b)=>a+'-'+b))"), vec!["3-2-1"]);
        assert_eq!(run_ok("console.log([[0,1],[2,3]].reduceRight((a,b)=>a.concat(b),[]).join(','))"), vec!["2,3,0,1"]);
        // Object.fromEntries from an array of pairs and from a Map.
        assert_eq!(run_ok("console.log(JSON.stringify(Object.fromEntries([['a',1],['b',2]])))"), vec![r#"{"a":1,"b":2}"#]);
        assert_eq!(run_ok("let m=new Map([['x',1]]); console.log(Object.fromEntries(m).x)"), vec!["1"]);
    }

    #[test]
    fn array_string_methods_batch2() {
        // flatMap (map + flatten one level; empty array => filter out).
        assert_eq!(run_ok("console.log([1,2,3].flatMap(x=>[x,x*2]).join(','))"), vec!["1,2,2,4,3,6"]);
        assert_eq!(run_ok("console.log([1,2,3].flatMap(x=>x%2?[x]:[]).join(','))"), vec!["1,3"]);
        // Immutable toSorted / toReversed leave the receiver unchanged.
        assert_eq!(run_ok("let a=[3,1,2]; let b=a.toSorted((x,y)=>x-y); console.log(b.join(','), a.join(','))"), vec!["1,2,3 3,1,2"]);
        assert_eq!(run_ok("let a=[1,2,3]; console.log(a.toReversed().join(','), a.join(','))"), vec!["3,2,1 1,2,3"]);
        // findLast / findLastIndex.
        assert_eq!(run_ok("console.log([1,2,3,4].findLast(x=>x<3), [1,2,3,4].findLastIndex(x=>x<3))"), vec!["2 1"]);
        // splice: remove+insert (returns removed), insert-only, negative start.
        assert_eq!(run_ok("let a=[1,2,3,4,5]; let r=a.splice(1,2,9,9,9); console.log(r.join(','), a.join(','))"), vec!["2,3 1,9,9,9,4,5"]);
        assert_eq!(run_ok("let a=[1,2,3]; a.splice(1,0,'x'); console.log(a.join(','))"), vec!["1,x,2,3"]);
        assert_eq!(run_ok("let a=[1,2,3]; console.log(a.splice(-1).join(','), a.join(','))"), vec!["3 1,2"]);
        // String indexOf honors a start position; codePointAt.
        assert_eq!(run_ok("console.log('abcabc'.indexOf('c',3), 'abcabc'.indexOf('a',1))"), vec!["5 3"]);
        assert_eq!(run_ok("console.log('Hello'.codePointAt(0), 'Hi'.codePointAt(5))"), vec!["72 undefined"]);
    }

    #[test]
    fn array_methods_more() {
        assert_eq!(run_ok("let a=[1,2,3]; a.reverse(); console.log(a.join(','))"), vec!["3,2,1"]);
        assert_eq!(run_ok("console.log([1,2].concat([3,4],5,[6]).join(','))"), vec!["1,2,3,4,5,6"]);
        assert_eq!(run_ok("console.log([1,[2,[3]]].flat().length, [1,[2,[3]]].flat(2).join(','))"), vec!["3 1,2,3"]);
        assert_eq!(run_ok("console.log([1,2,3,4].fill(9,1,3).join(','), [1,2,1].lastIndexOf(1))"), vec!["1,9,9,4 2"]);
    }

    #[test]
    fn array_callback_search_methods() {
        assert_eq!(
            run_ok("let a=[1,2,3,4]; console.log(a.find(x=>x>2), a.findIndex(x=>x>2), a.some(x=>x>3), a.every(x=>x>0))"),
            vec!["3 2 true true"],
        );
        assert_eq!(
            run_ok("let a=[1,2,3]; console.log(a.find(x=>x>9), a.findIndex(x=>x>9), a.some(x=>x>9), a.every(x=>x>1))"),
            vec!["undefined -1 false false"],
        );
        // Empty array: some→false, every→true (vacuous truth).
        assert_eq!(run_ok("console.log([].some(x=>x), [].every(x=>x))"), vec!["false true"]);
    }

    #[test]
    fn string_methods_extra() {
        assert_eq!(run_ok("console.log('  hi  '.trim(), 'abc'.startsWith('ab'), 'abc'.endsWith('bc'))"), vec!["hi true true"]);
        assert_eq!(run_ok("console.log('5'.padStart(3,'0'), '5'.padEnd(3,'-'), 'abc'.padStart(2))"), vec!["005 5-- abc"]);
        // replace = first occurrence; replaceAll = all.
        assert_eq!(run_ok("console.log('aXbXc'.replace('X','-'), 'aXbXc'.replaceAll('X','-'))"), vec!["a-bXc a-b-c"]);
        // charCodeAt/charAt/at/codePointAt — O(1) byte access (no per-call clone),
        // correct for ASCII and multi-byte; out-of-range → NaN/''/undefined.
        assert_eq!(run_ok("let s='hello'; console.log(s.charCodeAt(0), s.charCodeAt(4), s.charCodeAt(9), s.charAt(1), s.charAt(9), s.at(-1), s.codePointAt(2))"), vec!["104 111 NaN e  o 108"]);
        assert_eq!(run_ok("let u='héllo→'; console.log(u.charCodeAt(1), u.charAt(1), u.codePointAt(5), u.at(-1), u.length)"), vec!["233 é 8594 → 6"]);
        // A charCodeAt scan over a built-up string (regression: was O(n²)).
        assert_eq!(run_ok("let s=''; for(let i=0;i<500;i++) s+=(i%10); let c=0; for(let i=0;i<s.length;i++) if(s.charCodeAt(i)===55) c++; console.log(c)"), vec!["50"]);
    }

    #[test]
    fn math_functions() {
        assert_eq!(
            run_ok("console.log(Math.sqrt(16), Math.floor(3.7), Math.ceil(3.2), Math.abs(-5), Math.trunc(-4.7))"),
            vec!["4 3 4 5 -4"],
        );
        // JS Math.round is half-up (≠ Rust's half-away-from-zero for negatives).
        assert_eq!(run_ok("console.log(Math.round(2.5), Math.round(-2.5), Math.round(-2.6))"), vec!["3 -2 -3"]);
        assert_eq!(
            run_ok("console.log(Math.min(3,1,2), Math.max(1,9,2), Math.pow(2,10), Math.hypot(3,4))"),
            vec!["1 9 1024 5"],
        );
        // sign preserves 0 / maps NaN→NaN; min/max are NaN-sticky; empty → ±Infinity.
        assert_eq!(
            run_ok("console.log(Math.sign(-3), Math.sign(0), Math.sign(NaN), Math.max(1,NaN), Math.max(), Math.min())"),
            vec!["-1 0 NaN NaN -Infinity Infinity"],
        );
        // Argument coercion (string → number).
        assert_eq!(run_ok("console.log(Math.sqrt('9'))"), vec!["3"]);
        // Math.random(): always in [0,1); a dice roll lands in range.
        assert_eq!(run_ok("let ok=true; for(let i=0;i<500;i++){let r=Math.random(); if(!(r>=0&&r<1))ok=false} console.log(ok)"), vec!["true"]);
        assert_eq!(run_ok("let d=Math.floor(Math.random()*6)+1; console.log(d>=1&&d<=6)"), vec!["true"]);
    }

    #[test]
    fn template_literals() {
        assert_eq!(run_ok("let x=5; console.log(`val=${x+1}`)"), vec!["val=6"]);
        assert_eq!(run_ok("let a='A',b=2; console.log(`${a}-${b}-${a+b}`)"), vec!["A-2-A2"]);
        assert_eq!(run_ok("let o={n:7}; console.log(`obj ${o.n} arr ${[1,2].length}`)"), vec!["obj 7 arr 2"]);
        assert_eq!(run_ok("console.log(`no interp`)"), vec!["no interp"]);
        assert_eq!(run_ok("let n=10; let f=()=>`n=${n}`; console.log(f())"), vec!["n=10"]);
    }

    #[test]
    fn tagged_templates() {
        // The tag gets the cooked strings array + the interpolated values.
        assert_eq!(run_ok("function t(s,...v){return s.join('|')+'#'+v.join(',')} console.log(t`a${1}b${2}c`)"), vec!["a|b|c#1,2"]);
        // No interpolations: one string, no values.
        assert_eq!(run_ok("function t(s,...v){return s.join('|')+'#'+v.length} console.log(t`hi`)"), vec!["hi#0"]);
        // `.raw` is the un-escaped parts (here `\\n` stays literal).
        assert_eq!(run_ok(r"function t(s){return s.raw[0]} console.log(t`a\nb`)"), vec![r"a\nb"]);
        // String.raw built-in.
        assert_eq!(run_ok(r"console.log(String.raw`a\n${1+1}b`)"), vec![r"a\n2b"]);
        // A member tag binds `this`.
        assert_eq!(run_ok("let o={p:'P',f(s,...v){return this.p+':'+s.join('/')+v.join('')}}; console.log(o.f`x${10}y`)"), vec!["P:x/y10"]);
        // A closure capturing an outer var inside an interpolation.
        assert_eq!(run_ok("function t(s,...v){return v[0]} function mk(n){return ()=>t`${n*2}`} console.log(mk(21)())"), vec!["42"]);
    }

    #[test]
    fn typeof_operator() {
        // null is "object" (the historic quirk); arrays/objects "object";
        // functions/arrows "function"; primitives their type.
        assert_eq!(
            run_ok("let f=()=>1; console.log(typeof 1, typeof 1.5, typeof 'a', typeof true, typeof undefined, typeof null, typeof [], typeof {}, typeof f)"),
            vec!["number number string boolean undefined object object object function"],
        );
        // typeof sees through a captured (cell) variable to its value.
        assert_eq!(
            run_ok("let n=5; let g=()=>typeof n; console.log(g())"),
            vec!["number"],
        );
    }

    #[test]
    fn string_index_double_key_region() {
        // s[k] where k is a region-computed integral double (k = i*0+2) must
        // return the char, not undefined — the get_index Str arm coerces integral
        // doubles like the Array arm. JIT-on must equal JIT-off across the window.
        assert_jit_matches(
            "let s='ABCD'; let k=0; let c=0; for(let i=0;i<2000;i++){ k=i*0+2; if(s[k]==='C') c=c+1; } console.log(c)",
            &["2000"],
        );
    }

    #[test]
    fn array_setindex_sparse_grow_deopts() {
        // A sparse write past the end inside a hot loop deopts the (potentially
        // huge) resize to the interpreter rather than reallocating from native
        // code; the result still matches. a starts len 3; a[10]=i grows it to 11.
        assert_jit_matches(
            "let a=[1,2,3]; let n=0; for(let i=0;i<2000;i++){ a[10]=i; n=n+1; } console.log(a.length, n)",
            &["11 2000"],
        );
    }

    #[test]
    fn array_setindex_region_transform() {
        // In-place transform a[i] = a[i]*2 (GetIndex + SetIndex in one region).
        // JIT-on == JIT-off == 2*(0+..+999) = 999000.
        assert_jit_matches(
            "let a=[]; for(let i=0;i<1000;i++) a.push(i); for(let i=0;i<a.length;i++){ a[i]=a[i]*2; } let s=0; for(let i=0;i<a.length;i++) s+=a[i]; console.log(s)",
            &["999000"],
        );
    }

    #[test]
    fn array_setindex_build_and_grow() {
        // Build loop a[i]=i*i where every write GROWS the array (the helper grows
        // it, like the interpreter); plus a sparse write past the end with holes.
        assert_jit_matches(
            "let a=[]; for(let i=0;i<1000;i++){ a[i]=i*i; } let s=0; for(let i=0;i<1000;i++) s+=a[i]; console.log(a.length, s)",
            &["1000 332833500"],
        );
        assert_eq!(run_ok("let a=[]; a[5]=99; console.log(a.length, a[0], a[5])"), vec!["6 undefined 99"]);
    }

    #[test]
    fn array_length_assignment_in_region() {
        // `a.length = n` truncates a dense array; in a hot loop the result must
        // agree JIT-on == JIT-off == JS. Requires SROA to NOT scalar-promote the
        // special `length` property, and the write to deopt to the truncating
        // interpreter path rather than silently no-op.
        assert_jit_matches(
            "let a=[1,2,3,4,5]; let s=0; for(let i=0;i<20000;i++){ a.length=2; s+=a.length; } console.log(s)",
            &["40000"],
        );
    }

    #[test]
    fn array_length_clear_grow_invalid() {
        // arr.length = 0 clears (a very common idiom); larger extends with holes;
        // a non-integer / negative length throws RangeError.
        assert_eq!(run_ok("let a=[1,2,3]; a.length=0; console.log(a.length, a[0])"), vec!["0 undefined"]);
        assert_eq!(run_ok("let a=[1,2]; a.length=4; console.log(a.length, a[3], a[1])"), vec!["4 undefined 2"]);
        let out = run("let a=[1,2,3]; a.length=-1;").expect("compile");
        assert!(out.error.as_deref().unwrap_or("").contains("Invalid array length"));
    }

    #[test]
    fn array_double_and_oob_index() {
        // Integral double keys coerce (a[1.0]==a[1]); negative / fractional /
        // out-of-range keys are undefined — matching JS and the JIT helper.
        assert_eq!(
            run_ok("let a=[10,20,30]; console.log(a[1.0], a[2], a[5], a[-1], a[1.5])"),
            vec!["20 30 undefined undefined undefined"],
        );
    }

    #[test]
    fn recursive_callback_in_map_native() {
        // A self-recursive callback used in map exercises the native callback
        // fast path invoking a JIT'd self-recursive function (jit_self_call).
        // tri(n)=n+(n-1)+…; tri(3)=6, tri(4)=10, tri(5)=15.
        assert_eq!(
            run_ok("function tri(n){ return n<=0?0:n+tri(n-1); } console.log([3,4,5].map(tri).join(','))"),
            vec!["6,10,15"],
        );
    }

    #[test]
    fn int_function_modulo_jit() {
        // A hot function using `%` compiles via the whole-function JIT (idiv).
        // Negative dividends keep the dividend's sign (JS / interpreter
        // semantics); JIT-on must equal JIT-off and the expected string.
        assert_jit_matches(
            "function f(x){ return x%3; } let o=''; for(let i=-5;i<6;i++){ o += f(i)+','; } console.log(o)",
            &["-2,-1,0,-2,-1,0,1,2,0,1,2,"],
        );
    }

    #[test]
    fn modulo_zero_and_negone_bail() {
        // `% 0` is NaN and `% -1` is 0 — the JIT bails on both (div-by-zero, and
        // the INT_MIN/-1 #DE), so the interpreter produces them; JIT-on==JIT-off.
        assert_jit_matches(
            "function g(x,m){ return x%m; } let o=''; for(let i=0;i<10;i++){ o += g(i,0)+'|'+g(i,-1)+';'; } console.log(o)",
            &["NaN|0;NaN|0;NaN|0;NaN|0;NaN|0;NaN|0;NaN|0;NaN|0;NaN|0;NaN|0;"],
        );
    }

    #[test]
    fn string_bracket_length_matches_dot_length() {
        // s['length'] (computed member) must equal s.length and arr['length'] —
        // the get_index Str arm used to drop non-int keys to undefined.
        assert_eq!(
            run_ok("let s='hello'; let a=[1,2,3]; console.log(s['length'], s.length, a['length'])"),
            vec!["5 5 3"],
        );
    }

    #[test]
    fn huge_array_literal_does_not_corrupt_the_frame() {
        // REGRESSION: `NewArray`'s argc is a u16 and its elements need one
        // CONTIGUOUS register each, so a literal with >= 2^16 elements used to
        // truncate its count (70000 -> 4464) AND wrap `next_reg` back over live
        // registers, silently corrupting unrelated locals. Big literals now take
        // the incremental ArrayAppend path (which needs one scratch register).
        let n = 70_000usize;
        let elems = (0..n).map(|i| i.to_string()).collect::<Vec<_>>().join(",");
        let src = format!(
            "function f(){{ var x = 111; var y = 222; var a = [{elems}]; \
             return [a.length, x, y, a[0], a[{}]]; }} console.log(f().join(','))",
            n - 1
        );
        assert_eq!(run_ok(&src), vec![format!("{n},111,222,0,{}", n - 1)]);
    }

    #[test]
    fn huge_array_literal_preserves_holes() {
        // The incremental path must reproduce NewArray's hole semantics: an
        // elision is an ABSENT element, not a present `undefined`.
        let n = 70_000usize;
        let src = format!(
            "var a = [1,,3{}]; console.log(a.length, 1 in a, 0 in a, a[2])",
            ",4".repeat(n)
        );
        assert_eq!(run_ok(&src), vec![format!("{} false true 3", n + 3)]);
    }

    #[test]
    fn fused_concat_key_in_a_branchy_loop() {
        // Regression for the bug that killed the B9 cold-exit tier: admitting
        // `GetIndexConcat` let regions containing it reach that tier for the
        // first time, and it computed `s` as 0 instead of 3050. Cold exits are
        // gone; this pins the shape that exposed them — a fused computed key
        // read inside a `||` (so the loop body has branches) after a
        // delete/re-add cycle.
        assert_jit_matches(
            "let o={},s=0;              for(let i=0;i<50;i++) o['k'+i]=i*2;              for(let i=0;i<50;i+=2) delete o['k'+i];              for(let i=0;i<50;i+=2) o['k'+i]=i*3;              for(let i=0;i<50;i++) s+=(o['k'+i]||0);              console.log(s)",
            &["3050"],
        );
    }

    #[test]
    fn timers_fire_in_due_order_when_queued_out_of_order() {
        // REGRESSION: the event loop sorted due timers by DEADLINE and then
        // removed through that list in reverse, assuming deadline order implied
        // index order. A timer queued first but due LATER made `Vec::remove`
        // take a stale index and panic -- a hard crash under `panic = "abort"`.
        assert_eq!(
            run_ok(
                "const s=ms=>new Promise(r=>setTimeout(r,ms)); \
                 Promise.race([s(9).then(()=>1), s(5).then(()=>2)]) \
                   .then(v=>console.log('race', v))"
            ),
            vec!["race 2"],
        );
        // Several out-of-order deadlines must still fire earliest-first.
        assert_eq!(
            run_ok(
                "for (const ms of [30, 10, 20]) setTimeout(()=>console.log('t'+ms), ms)"
            ),
            vec!["t10", "t20", "t30"],
        );
        // Equal deadlines keep insertion order (FIFO).
        assert_eq!(
            run_ok("for (const n of [1,2,3]) setTimeout(()=>console.log('n'+n), 5)"),
            vec!["n1", "n2", "n3"],
        );
    }

    #[test]
    fn typedarray_length_in_jit_region_matches_interpreter() {
        // `ta.length` is an INHERITED accessor, so the JIT's direct answer is
        // only valid while the built-in getter is intact. JIT and interpreter
        // must agree in every case: pristine, shadowed by an own property,
        // re-pointed prototype, and detached buffer.
        assert_jit_matches(
            "const t=new Float64Array(2048); let s=0; \
             for(let i=0;i<t.length;i++) s+=t.length; console.log(s)",
            &["4194304"],
        );
        // An own `length` shadows the inherited accessor.
        assert_jit_matches(
            "const t=new Float64Array(8); Object.defineProperty(t,'length',{value:99}); \
             let s=0; for(let i=0;i<200;i++) s+=t.length; console.log(s)",
            &["19800"],
        );
        // A re-pointed prototype. Both tiers currently report the TypedArray's
        // own length (8), not the shadowing `{length: 7}` — the KNOWN DEVIATION
        // documented in vm/props/member.rs. What this case pins is that the JIT
        // and the interpreter AGREE, so fixing the deviation moves both.
        assert_jit_matches(
            "const t=new Float64Array(8); Object.setPrototypeOf(t,{length:7}); \
             let s=0; for(let i=0;i<200;i++) s+=t.length; console.log(s)",
            &["1600"],
        );
        // A detached buffer reports 0.
        assert_jit_matches(
            "const b=new ArrayBuffer(64); const t=new Float64Array(b); \
             let s=0; for(let i=0;i<200;i++){ if(i===100) transfer(b); s+=t.length; } \
             console.log(s); function transfer(x){ x.transfer(); }",
            &["800"],
        );
    }

    #[test]
    fn sparse_array_iteration_covers_the_whole_length() {
        // REGRESSION: elements past MAX_DENSE_ARRAY_LEN (2^20) live in the sparse
        // overlay, and `array_like_iterate` clamped its ascending probe to that
        // constant — so every callback method silently stopped at 1,048,576.
        // `some()` returned false while `find()` found a match on the SAME array.
        let src = "
            const N = 1200000, a = new Array(N);
            for (let i = 0; i < N; i++) a[i] = i;
            a[N + 5] = 99;
            console.log(
                a.some(x => x > 1100000),
                a.filter(x => x > 1100000).length,
                a.map(x => x).length,
                a.reduce(n => n + 1, 0),
            );
        ";
        assert_eq!(run_ok(src), vec!["true 99999 1200006 1200001"]);
    }
}
