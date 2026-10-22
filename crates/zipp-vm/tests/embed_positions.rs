//! Positions an embedder's script reports when the engine compiled it
//! behind a preamble, inside an expression wrapper, or under a host's own
//! offsets: an error's `stack` and a syntax error count from the host's text,
//! as `zipp js` and Node count a file, and frames of the engine's plumbing are
//! named `zipp:preamble` or left out. Expected positions are Node 24's for
//! the same source (`vm.Script` with `filename`, `lineOffset` and
//! `columnOffset` for the offsets).

use zipp_vm::embed::{
    compile_script, compile_script_with_origin, compile_script_with_preamble,
    compile_script_with_preamble_and_origin, CompileOptions, JsValue, ScriptOrigin, ScriptState,
};

/// A preamble shaped like the browser engine's: comment-free lines, each
/// ending in a newline, and a helper that calls back into the guest.
const PREAMBLE: &str = "var __zState = 0;\nfunction __zDispatch(cb) {\n  return cb();\n}\n";

/// Seven lines of app, as the report's example has them: positions 2:10,
/// 4:13, 6:9 and 8:7 as `zipp js` and Node give them.
const APP: &str = "function f() {
  return new Error('x').stack;
}
console.log(f());
function g() {
  throw new TypeError('y');
}
try { __zDispatch(g); } catch (e) { console.log(e.stack); }
";

fn output(mut st: ScriptState) -> Vec<String> {
    st.run_init().expect("runs");
    st.take_output()
}

fn origin(name: &str, line_offset: i32, column_offset: i32) -> ScriptOrigin {
    ScriptOrigin { name: Some(name.into()), line_offset, column_offset }
}

#[test]
fn a_preamble_does_not_move_the_guests_lines() {
    let st = compile_script_with_preamble_and_origin(
        PREAMBLE,
        APP,
        &CompileOptions::default(),
        &origin("app.js", 0, 0),
    )
    .expect("compiles");
    assert_eq!(
        output(st),
        [
            "Error: x\n    at f (app.js:2:10)\n    at app.js:4:13",
            "TypeError: y\n    at g (app.js:6:9)\n    at __zDispatch (zipp:preamble:3:10)\n    at app.js:8:7",
        ]
    );
}

#[test]
fn the_same_app_without_a_preamble_reports_the_same_lines() {
    // What `zipp js app.js` prints, the preamble's helper defined in the app.
    let app = format!("{APP}function __zDispatch(cb) {{ return cb(); }}\n");
    let mut st = compile_script(&app).expect("compiles");
    st.set_script_name("app.js");
    assert_eq!(
        output(st),
        [
            "Error: x\n    at f (app.js:2:10)\n    at app.js:4:13",
            "TypeError: y\n    at g (app.js:6:9)\n    at __zDispatch (app.js:9:35)\n    at app.js:8:7",
        ]
    );
}

#[test]
fn an_unnamed_guest_behind_a_preamble_is_anonymous_from_its_line_1() {
    let st = compile_script_with_preamble(PREAMBLE, APP, &CompileOptions::default())
        .expect("compiles");
    assert_eq!(
        output(st)[0],
        "Error: x\n    at f (<anonymous>:2:10)\n    at <anonymous>:4:13"
    );
}

#[test]
fn a_preamble_function_that_throws_itself_is_named_as_the_preambles() {
    // The engine raises the TypeError in `__zCall`'s own frame, which records
    // no position; it is still the preamble's frame.
    let pre = "function __zCall(o) {\n  o.f();\n  return null.x;\n}\n";
    let app = "try { __zCall({ f() {} }); } catch (e) { console.log(e.stack); }\n";
    let st = compile_script_with_preamble_and_origin(
        pre,
        app,
        &CompileOptions::default(),
        &origin("app.js", 0, 0),
    )
    .expect("compiles");
    let out = output(st);
    assert!(
        out[0].starts_with("TypeError: ") && out[0].ends_with("\n    at __zCall (zipp:preamble)\n    at app.js:1:7"),
        "{out:?}"
    );
}

#[test]
fn host_offsets_move_lines_and_the_first_lines_columns_as_vm_script_does() {
    let st = compile_script_with_origin(
        "console.log(new Error('z').stack);\nconsole.log(f());\nfunction f() { return new Error('w').stack; }\n",
        &CompileOptions::default(),
        &origin("x.js", 10, 5),
    )
    .expect("compiles");
    assert_eq!(
        output(st),
        [
            "Error: z\n    at x.js:11:18",
            "Error: w\n    at f (x.js:13:23)\n    at x.js:12:13",
        ]
    );
}

#[test]
fn a_position_moved_before_line_1_has_none() {
    // A host that put two lines of its own ahead of the code it was given:
    // its own frames fall before line 1 and print without a position. (Node
    // leaves out line 0 that way, but prints `host.js:-1:26` for line -1.)
    let st = compile_script_with_origin(
        "function shim() { return new Error('s').stack; }\nfunction mine() { return shim(); }\nconsole.log(mine());\n",
        &CompileOptions::default(),
        &origin("host.js", -2, 0),
    )
    .expect("compiles");
    assert_eq!(output(st), ["Error: s\n    at shim (host.js)\n    at mine (host.js)\n    at host.js:1:13"]);
}

#[test]
fn syntax_errors_say_where_they_are() {
    let err = |r: Result<ScriptState, String>| r.err().expect("a syntax error");
    assert_eq!(
        err(compile_script("let a = 1;\nlet b = ;\n")),
        "SyntaxError: unexpected token ';'\n    at <anonymous>:2:9"
    );
    assert_eq!(
        err(compile_script_with_preamble(PREAMBLE, "let a = 1;\nlet b = ;\n", &CompileOptions::default())),
        "SyntaxError: unexpected token ';'\n    at <anonymous>:2:9"
    );
    assert_eq!(
        err(compile_script_with_preamble_and_origin(
            PREAMBLE,
            "let a = 1;\nlet b = ;\n",
            &CompileOptions::default(),
            &origin("app.js", -1, 0),
        )),
        "SyntaxError: unexpected token ';'\n    at app.js:1:9"
    );
    // The end of the input is the end of the guest's text.
    assert_eq!(
        err(compile_script_with_preamble(PREAMBLE, "let a = (1,\n", &CompileOptions::default())),
        "SyntaxError: unexpected end of input\n    at <anonymous>:2:1"
    );
}

#[test]
fn an_evals_syntax_error_message_has_no_position() {
    // V8's message for `eval("let b = ;")` is the message alone.
    let st = compile_script(
        "try { eval('let b = ;'); } catch (e) { console.log(e.message); }\n\
         try { new Function('let b = ;'); } catch (e) { console.log(e.message); }\n",
    )
    .expect("compiles");
    assert_eq!(output(st), ["unexpected token ';'", "unexpected token ';'"]);
}

#[test]
fn a_class_declaration_needs_a_name() {
    let err = |src: &str| compile_script(src).err().unwrap_or_else(|| panic!("{src:?} compiled"));
    assert_eq!(err("class { }\nconsole.log('ran');\n"), "SyntaxError: unexpected token '{'\n    at <anonymous>:1:7");
    assert_eq!(err("class extends Object {}"), "SyntaxError: unexpected token 'extends'\n    at <anonymous>:1:7");
    assert_eq!(err("{ class {} }"), "SyntaxError: unexpected token '{'\n    at <anonymous>:1:9");
    assert_eq!(err("class"), "SyntaxError: unexpected end of input\n    at <anonymous>:1:6");
    // Class expressions and named declarations are as they were.
    let st = compile_script(
        "class A {}\nconst B = class {};\nconst C = class extends A {};\n\
         console.log(typeof A, B.name, new C() instanceof A, (class {}).name);\n",
    )
    .expect("compiles");
    assert_eq!(output(st), ["function B true "]);
}

#[test]
fn modules_keep_export_default_class_and_refuse_a_bare_export_class() {
    assert!(zipp_vm::compile_to_text("export default class {}", true).is_ok());
    assert!(zipp_vm::compile_to_text("export default class extends Object {}", true).is_ok());
    let err = zipp_vm::compile_to_text("export class {}", true).expect_err("an export needs a name");
    assert!(err.starts_with("SyntaxError: unexpected token '{'"), "{err}");
}

/// What the browser engine's `evalInContext` sends: the expression in a
/// JSON-projecting function wrapper.
fn wrapped(expr: &str) -> (String, std::ops::Range<usize>) {
    const PREFIX: &str = "JSON.stringify((function () { return (";
    (format!("{PREFIX}{expr}); }})())"), PREFIX.len()..PREFIX.len() + expr.len())
}

#[test]
fn an_evaluated_expression_counts_from_itself_and_hides_the_wrapper() {
    let mut st = compile_script("function stackOf() { return new Error('s').stack; }\n")
        .expect("compiles");
    st.run_init().expect("runs");
    let (src, guest) = wrapped("stackOf()");
    let got = st.eval_wrapped_in_context(&src, guest).expect("evaluates");
    assert_eq!(
        got,
        JsValue::String(r#""Error: s\n    at stackOf (<anonymous>:1:29)\n    at <anonymous>:1:1""#.into())
    );
    // A later column of the expression's line 1, and its second line.
    let (src, guest) = wrapped("[0, stackOf()][1] +\n stackOf()");
    let got = st.eval_wrapped_in_context(&src, guest).expect("evaluates");
    let JsValue::String(text) = got else { panic!("{got:?}") };
    assert!(text.contains("at <anonymous>:1:5Error: s") && text.ends_with(r#"at <anonymous>:2:2""#), "{text}");
    // The plain form is the unwrapped text's own, wrapper and all.
    let (src, _) = wrapped("stackOf()");
    let got = st.eval_in_context(&src).expect("evaluates");
    let JsValue::String(text) = got else { panic!("{got:?}") };
    assert!(text.contains("at <anonymous>:1:39"), "{text}");
}

#[test]
fn the_rich_form_reports_the_same_frames() {
    let mut st = compile_script("function stackOf() { return new Error('s').stack; }\n")
        .expect("compiles");
    st.run_init().expect("runs");
    const PREFIX: &str = "(function () { return (";
    let expr = "stackOf()";
    let src = format!("{PREFIX}{expr}); }})()");
    let mut budget = zipp_vm::embed::HostValueBudget::default();
    let got = st
        .eval_wrapped_in_context_rich(&src, PREFIX.len()..PREFIX.len() + expr.len(), &mut budget)
        .expect("evaluates");
    assert_eq!(
        got,
        zipp_vm::embed::HostValue::String("Error: s\n    at stackOf (<anonymous>:1:29)\n    at <anonymous>:1:1".into())
    );
}
