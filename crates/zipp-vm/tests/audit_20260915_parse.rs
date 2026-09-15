//! Parser, lexer and cover-grammar fixes from the 15 September 2026 audit
//! (track E-parse). Each test names the finding it pins.
//!
//! Most of these are parse-time decisions, so they are checked through
//! `parse_to_text` (does the front end accept the text?) and through `eval`
//! (does the guest see the right error class?). The ones with a runtime VALUE
//! — numeric literal rounding, lone surrogates in eval'd source, the ASI cases
//! that used to turn a function into NaN — run in clean child processes under
//! the default, interpreter and forced-JIT modes, as `call_order_default.rs`
//! does. The deep-nesting probes run in a child too, on a large-stack thread,
//! so a regression that restores the native stack overflow fails one test
//! instead of aborting the harness.

use std::process::Command;

fn rejects(source: &str) {
    assert!(
        zipp_vm::parse_to_text(source, false).is_err(),
        "invalid script was accepted: {source}"
    );
}

fn rejects_module(source: &str) {
    assert!(
        zipp_vm::parse_to_text(source, true).is_err(),
        "invalid module was accepted: {source}"
    );
}

fn parses(source: &str) {
    zipp_vm::parse_to_text(source, false)
        .unwrap_or_else(|error| panic!("valid script rejected: {source}: {error}"));
}

fn parses_module(source: &str) {
    zipp_vm::parse_to_text(source, true)
        .unwrap_or_else(|error| panic!("valid module rejected: {source}: {error}"));
}

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

// ---------------------------------------------------------------------------
// R003: a bare arrow is an AssignmentExpression, never an operand.
// ---------------------------------------------------------------------------

#[test]
fn r003_bare_arrow_takes_no_operator_on_the_same_line() {
    for source in [
        "x = () => {} + 1;",
        "x = y || () => 1;",
        "x = y && () => 1;",
        "x = y ?? () => 1;",
        "x = (a, b) => {} ? 1 : 2;",
        "x = 1 + () => 1;",
        "x = a ** () => 1;",
        "x = () => {} instanceof Object;",
        "x = () => {} in {};",
        "x = () => {} ?? 1;",
        "!() => 1;",
        "typeof () => 1;",
        "void () => {};",
        "delete () => 1;",
        "-() => 1;",
        "new () => {};",
        "new () => {}.x;",
    ] {
        rejects(source);
    }
    // Parenthesized, the arrow is a PrimaryExpression and any operator applies;
    // as a conditional branch it is an AssignmentExpression and fine.
    for source in [
        "x = (() => {}) + 1;",
        "x = y || (() => 1);",
        "x = !(() => 1);",
        "x = new (function () {})();",
        "x = a ? () => 1 : () => 2;",
        "x = () => {}, y = 1;",
        "f(() => {}, () => 1);",
    ] {
        parses(source);
    }
}

// ---------------------------------------------------------------------------
// R005: a named default-exported declaration binds its name in the module.
// ---------------------------------------------------------------------------

#[test]
fn r005_named_default_export_declares_its_binding() {
    for source in [
        "export default function App() {}\nexport { App as NamedApp };",
        "export default class Foo {}\nexport { Foo };",
        "export default async function g() {}\nexport { g };",
        "export default function* h() {}\nexport { h as hh };",
        "export default function () {}\nexport const x = 1;",
    ] {
        parses_module(source);
    }
    for source in [
        "import f from 'x'; export default function f() {}",
        "import C from 'x'; export default class C {}",
        "export default function f() {} let f;",
        "let f; export default function f() {}",
        "export default class C {} class C {}",
        "export default async function g() {} var g;",
    ] {
        rejects_module(source);
    }
}

#[test]
fn r005_default_exported_function_is_importable_under_its_local_name() {
    let dir = std::env::temp_dir().join(format!(
        "zipp-audit-parse-r005-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    std::fs::write(
        dir.join("lib.mjs"),
        "export default function App() { return 'app'; }\nexport { App as NamedApp };\n",
    )
    .expect("write lib");
    std::fs::write(
        dir.join("main.mjs"),
        "import App, { NamedApp } from './lib.mjs';\nconsole.log(App(), NamedApp(), App === NamedApp);\n",
    )
    .expect("write main");
    // The file runner routes an entry with static imports through the module
    // loader, which links them before evaluation.
    let out = zipp_vm::run_module_file(&dir.join("main.mjs"), None).expect("module compiles");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.error.is_none(), "module error: {:?}", out.error);
    assert_eq!(out.output, ["app app true"]);
}

// ---------------------------------------------------------------------------
// R008: a CoverInitializedName in an expression position is final.
// ---------------------------------------------------------------------------

#[test]
fn r008_cover_initialized_name_is_not_discarded_by_an_enclosing_pattern() {
    for source in [
        "({a = {b=1}} = {});",
        "[a = {b=1}] = [];",
        "({[{a=1}]: x} = {});",
        "({a: b = {c=1}} = {});",
        "(a = {b=1}) => 1;",
        "([a = {b=1}]) => 1;",
        "async ({a = {b=1}}) => 1;",
        "for ([a = {b=1}] of []);",
        "for (var x = {a = 1};;) break;",
        "for (let x = 1, y = {a = 1};;) break;",
        "for (var x = {a = 1} in {});",
        "[class { [{b=1}]() {} }.x] = [];",
        "[class extends {b=1} {}.x] = [];",
        "class C extends {b = 1} {}",
        "var a, b = 7; ({a = {b = 1}} = {});",
        "x = {a = 1};",
        "x = y = {a = 1};",
    ] {
        rejects(source);
    }
    // Every literal an enclosing `=` actually refines keeps its initializer.
    for source in [
        "({a = 1} = {});",
        "[{a = 1}] = [];",
        "({a: {b = 1}} = {});",
        "[a = 1] = [];",
        "({a = 1}) => a;",
        "for ({a = 1} of []);",
        "({a = 1} = {}) => 1;",
        "[{a = 1} = {}] = [];",
        "(a = ({b = 1} = {})) => a;",
        "x = {a: y} = {a: 2};",
        "var q = [{a = 1}] = [{}];",
        "for ([{a = 1}] of [[{}]]);",
    ] {
        parses(source);
    }
    // The initializer used to be silently dropped: `a` became `{b: b}`.
    assert_eq!(
        run_ok(
            "var r; try { eval('var a, b = 7; ({a = {b = 1}} = {});'); r = 'accepted'; } \
             catch (e) { r = e.name; } console.log(r);"
        ),
        ["SyntaxError"]
    );
}

// ---------------------------------------------------------------------------
// R009: `[In]` returns inside initializers, computed keys and `new a[…]`.
// ---------------------------------------------------------------------------

#[test]
fn r009_in_is_restored_inside_for_head_initializers_and_keys() {
    for source in [
        "for (var [x = 'a' in {}] = [];;) break;",
        "for (var {x = 'a' in {}} = {};;) break;",
        "for (let {['x' in {}]: y} = {}; ;) break;",
        "for (var {['x' in {}]: y} = {}; ;) break;",
        "var a = {'true': function(){}, 'false': function(){}}; for (var x = new a['b' in {}];;) break;",
    ] {
        parses(source);
    }
    // The top level of the head is still `[~In]`.
    rejects("for (let x = 'a' in {};;) break;");
}

// ---------------------------------------------------------------------------
// R010: an all-octal digit run is a LegacyOctalIntegerLiteral, full stop.
// ---------------------------------------------------------------------------

#[test]
fn r010_legacy_octal_takes_no_fraction_or_exponent() {
    for source in ["x = 07.5;", "x = 07e1;", "x = 07..x;", "x = 00.5;", "x = 07_1;"] {
        rejects(source);
    }
    assert_eq!(
        run_ok("console.log(07.toString(), 07 .x, 010.valueOf(), 08.5, 09.5, 019e1);"),
        ["7 undefined 8 8.5 9.5 190"]
    );
}

// ---------------------------------------------------------------------------
// R011: arrow parameters admit neither parenthesized names nor pattern rest.
// ---------------------------------------------------------------------------

#[test]
fn r011_arrow_parameters_reject_parenthesized_names_and_pattern_rest() {
    for source in [
        "((a)) => 1;",
        "(a, (b)) => 1;",
        "([(a)]) => 1;",
        "({a: (b)}) => 1;",
        "({...{a}}) => 1;",
        "({...[a]}) => 1;",
        "async ({...{a}}) => 1;",
        "async ((a)) => 1;",
        "((a) = 1) => 1;",
        "([(a) = 1]) => 1;",
        "({a: (b) = 1}) => 1;",
        "([...(a)]) => 1;",
        "(...(a)) => 1;",
        "({...(a)}) => 1;",
    ] {
        rejects(source);
    }
    for source in [
        "(a) => 1;",
        "({...a}) => 1;",
        "([...[a]]) => 1;",
        "[(a)] = [1];",
        "({a: (b)} = {});",
        "(a = [(b)] = [1]) => 1;",
        "(a, b = (c)) => 1;",
        "({a, b: [c] = (d)} = {}) => 1;",
        "async ((a));",
        "((a), b);",
    ] {
        parses(source);
    }
}

// ---------------------------------------------------------------------------
// R012: `delete` of a private reference at the end of an optional chain.
// ---------------------------------------------------------------------------

#[test]
fn r012_private_delete_early_error_covers_optional_chains() {
    for body in ["delete this?.#x", "delete this?.a.#x", "delete (this?.#x)", "delete this.#x"] {
        rejects(&format!("class C {{ #x; m() {{ {body}; }} }}"));
    }
    parses("class C { #x; m() { delete this?.#x.y; delete this?.a; } }");
}

// ---------------------------------------------------------------------------
// R013: dynamic-function parameters are function code.
// ---------------------------------------------------------------------------

#[test]
fn r013_new_target_is_legal_in_dynamic_function_parameters() {
    assert_eq!(
        run_ok(
            r#"
            var GF = Object.getPrototypeOf(function* () {}).constructor;
            var AF = Object.getPrototypeOf(async function () {}).constructor;
            var f = new Function('a = new.target', 'return a');
            console.log(typeof f, f() === undefined, typeof new f(),
                typeof GF('a = new.target', 'return a'),
                typeof AF('a = new.target', 'return a'),
                Function('a = () => new.target', 'return a()')());
            try { Function('a = new.target.x.', 'return a'); } catch (e) { console.log(e.name); }
            "#
        ),
        ["function true function function function undefined", "SyntaxError"]
    );
}

// ---------------------------------------------------------------------------
// R014: `-->` right after a byte-order mark is at the start of the input.
// ---------------------------------------------------------------------------

#[test]
fn r014_html_close_comment_after_a_byte_order_mark() {
    for source in [
        "\u{FEFF}--> comment\nx = 1;",
        "\u{FEFF}/*x*/--> comment\nx = 1;",
        "\u{FEFF} --> comment\nx = 1;",
    ] {
        parses(source);
    }
    // Not after a token on the same line, and never in a module.
    rejects("x = 1 \u{FEFF}--> 2;");
    rejects_module("\u{FEFF}--> comment\n");
    assert_eq!(
        run_ok(
            "var r = []; for (var s of ['\\uFEFF--> c', '\\uFEFF/*x*/--> c', '\\uFEFF --> c']) \
             { try { (0, eval)(s); r.push('ok'); } catch (e) { r.push(e.name); } } \
             console.log(r.join(' '));"
        ),
        ["ok ok ok"]
    );
}

// ---------------------------------------------------------------------------
// R015: bare `super`, `for (let.x of …)`, duplicate import-attribute keys.
// ---------------------------------------------------------------------------

#[test]
fn r015_minor_early_errors() {
    for body in ["(super).x", "super + 1", "super`t`", "(super)"] {
        rejects(&format!("({{ m() {{ {body}; }} }});"));
    }
    rejects("class C extends Object { constructor() { new super(); } }");
    rejects("class C extends Object { constructor() { new super; } }");
    parses("class C extends Object { constructor() { super(); new super.x(); super.y; super[0]; } }");

    rejects("for (let.x of []);");
    rejects("async function f() { for await (let.x of []); }");
    parses("for (let.x in {});");
    parses("for ((let).x of []);");
    parses("for (let x of []);");

    for source in [
        "import x from 'a' with { type: 'json', type: 'json' };",
        "import x from 'a' with { type: 'json', 'type': 'json' };",
        "export * from 'a' with { type: 'json', \"type\": 'json' };",
        "export { y } from 'a' with { a: 'x', a: 'y' };",
    ] {
        rejects_module(source);
    }
    parses_module("import x from 'a' with { type: 'json', other: 'json' };");
}

// ---------------------------------------------------------------------------
// Runtime values, checked in every execution mode (R003, R004, R006).
// ---------------------------------------------------------------------------

/// Each line prints the observed value next to node v24's.
const VALUE_PROBES: &str = r#"
  var lines = [];
  function log(label, value) { lines.push(label + "=" + value); }
  function hex(s) {
    var out = [];
    for (var i = 0; i < s.length; i++) out.push(s.charCodeAt(i).toString(16));
    return out.join(",");
  }

  // R003: ASI after a parenthesized arrow with a block body.
  log("asi-minus", typeof eval("var f1 = () => {}\n-1\nf1"));
  log("asi-plus", eval("(function () { var f = (x) => {}\n+'s'\nreturn typeof f; })()"));
  log("asi-regex", eval("var g = (evt) => {\n  return evt\n}\n/^x/.test('x')"));
  log("asi-template", typeof eval("var h = () => {}\n`t`\nh"));
  // A regex literal starting with `=`: an arrow is never an assignment
  // target, so the `/=` is a regex, not a compound assignment.
  log("asi-regex-eq", eval("var i1 = () => {}\n/=/.test('=')"));
  log("asi-regex-eq-flags", eval("var i2 = (a) => {}\n/=a/g.exec('=a')[0]"));
  log("compound-divide", eval("var n = 1; n /= 2; n"));

  // R004: non-decimal literals are correctly rounded, and wide legacy octal
  // literals are not 0.
  log("hex64", 0x2000000000000101);
  log("bin54", 0b1000000000000000000000000000000000000000000000000000011);
  log("octal-wide", 0777777777777777777777777);
  log("octal-2^64", 02000000000000000000000);
  log("octal-prefix", 0o2000000000000000000000);
  log("hex-round-down", 0x1fffffffffffff81);
  log("hex-huge", 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF);
  log("eval-literal", eval("0x2000000000000101") + "," + Function("return 0777777777777777777777777")());
  log("number-hex", Number("0x2000000000000101"));
  log("number-bin", Number("0b1000000000000000000000000000000000000000000000000000011"));
  log("parseint-16", parseInt("2000000000000101", 16));
  log("parseint-10", parseInt("123456789012345678901234567890"));
  log("parseint-2", parseInt("11111111111111111111111111111111111111111111111111111111111111111", 2));
  log("parseint-short", parseInt("ff", 16) + "," + parseInt("-0x1f") + "," + parseInt("777", 8));

  // R006: a raw lone surrogate in eval/Function source reaches the value.
  var L = String.fromCharCode(0xD800);
  log("eval-string", hex(eval("'" + L + "'")));
  log("eval-string-mid", hex(eval('"a' + L + 'b"')));
  log("eval-template", hex(eval("`" + L + "`")));
  log("eval-template-raw", hex(eval("(function (s) { return s.raw[0]; })`" + L + "x`")));
  log("eval-template-sub", hex(eval("`a${1}" + L + "`")));
  log("eval-escaped", hex(eval("'\\" + L + "'")));
  log("eval-regex", hex(eval("/" + L + "/").source));
  log("function-string", hex(Function("return '" + L + "'")()));
  log("function-regex", hex(Function("return /" + L + "/")().source));
  log("function-param", hex(Function("a = '" + L + "'", "return a")()));
  log("pair-intact", hex(eval("'\uD83D\uDE00'")));

  console.log(lines.join(";"));
"#;

const VALUE_EXPECTED: &str = "asi-minus=function;\
asi-plus=function;\
asi-regex=true;\
asi-template=function;\
asi-regex-eq=true;\
asi-regex-eq-flags==a;\
compound-divide=0.5;\
hex64=2305843009213694500;\
bin54=18014398509481988;\
octal-wide=4.722366482869645e+21;\
octal-2^64=18446744073709552000;\
octal-prefix=18446744073709552000;\
hex-round-down=2305843009213694000;\
hex-huge=6.277101735386681e+57;\
eval-literal=2305843009213694500,4.722366482869645e+21;\
number-hex=2305843009213694500;\
number-bin=18014398509481988;\
parseint-16=2305843009213694500;\
parseint-10=1.2345678901234568e+29;\
parseint-2=36893488147419103000;\
parseint-short=255,-31,511;\
eval-string=d800;\
eval-string-mid=61,d800,62;\
eval-template=d800;\
eval-template-raw=d800,78;\
eval-template-sub=61,31,d800;\
eval-escaped=d800;\
eval-regex=d800;\
function-string=d800;\
function-regex=d800;\
function-param=d800;\
pair-intact=d83d,de00";

#[test]
fn value_probes_child() {
    if std::env::var_os("ZIPP_AUDIT_PARSE_VALUE_CHILD").is_none() {
        return;
    }
    assert_eq!(run_ok(VALUE_PROBES), [VALUE_EXPECTED]);
}

#[test]
fn value_probes_match_node_in_every_mode() {
    if std::env::var_os("ZIPP_AUDIT_PARSE_VALUE_CHILD").is_some()
        || std::env::var_os("ZIPP_AUDIT_PARSE_DEEP_CHILD").is_some()
    {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, env) in [
        ("default", None),
        ("interpreter", Some(("ZIPP_NOJIT", "1"))),
        ("forced-jit", Some(("ZIPP_JIT_THRESHOLD", "1"))),
    ] {
        let mut cmd = Command::new(&exe);
        cmd.args(["--exact", "value_probes_child", "--nocapture"])
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_JIT_THRESHOLD")
            .env("ZIPP_AUDIT_PARSE_VALUE_CHILD", "1");
        if let Some((key, value)) = env {
            cmd.env(key, value);
        }
        let out = cmd.output().expect("spawn mode child");
        assert!(
            out.status.success(),
            "value probes failed in {mode} mode:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

// ---------------------------------------------------------------------------
// R288 / R002 / R303: deep nesting in the default profile is a catchable
// RangeError, never a native stack overflow.
// ---------------------------------------------------------------------------

#[cfg(not(feature = "safe-sandbox"))]
mod default_profile_nesting {
    use super::*;
    use zipp_vm::native_syntax_limits::{
        MAX_NATIVE_AST_NESTING, MAX_NATIVE_SYNTAX_CHAIN, MAX_NATIVE_SYNTAX_RECURSION,
        MAX_NATIVE_TREE_DEPTH_BOUND,
    };

    /// Runs `source` on a thread with a large stack: this is an unoptimized
    /// test binary, whose frames cost several times the release build's, and
    /// the CLI gives its own interpreter thread 256 MiB.
    fn run_on_big_stack(source: String) -> Result<zipp_vm::Outcome, String> {
        std::thread::Builder::new()
            .name("audit-parse-deep".into())
            .stack_size(1 << 30)
            .spawn(move || zipp_vm::run(&source))
            .expect("spawn deep-nesting probe thread")
            .join()
            .expect("deep nesting must fail closed instead of overflowing the stack")
    }

    fn deep_probes() -> String {
        // Depths are derived from the limits, so moving a limit cannot leave a
        // probe silently under it.
        let over_recursion = MAX_NATIVE_SYNTAX_RECURSION + 1;
        let over_chain = MAX_NATIVE_SYNTAX_CHAIN * 4;
        // Every tier stays under its own limit while the tree does not: a
        // parenthesized member chain nested `levels` deep.
        let links = 20usize;
        let levels = MAX_NATIVE_AST_NESTING / links + 1;
        assert!(levels * 3 < MAX_NATIVE_SYNTAX_RECURSION && links < MAX_NATIVE_SYNTAX_CHAIN);
        // The same shape with every tier as long as the chain limit allows: a
        // spine of 13 million levels if nothing stopped it, which the walk
        // after the parse would reject only for its recursive drop to overflow
        // the stack. It must stop at the parser's running height bound.
        let tier_links = MAX_NATIVE_SYNTAX_CHAIN - 1;
        let tier_levels = 400usize;
        assert!(tier_levels * tier_links > 50 * MAX_NATIVE_TREE_DEPTH_BOUND);
        format!(
            r#"
            var results = [];
            function probe(label, run) {{
              try {{ run(); results.push(label + ":accepted"); }}
              catch (e) {{ results.push(label + ":" + e.name + (e instanceof RangeError ? "" : ":" + e.message)); }}
            }}
            var R = {over_recursion}, C = {over_chain};
            probe("eval-parens-40k", function () {{ eval("(".repeat(40000) + "1" + ")".repeat(40000)); }});
            probe("eval-parens", function () {{ eval("(".repeat(R) + "1" + ")".repeat(R)); }});
            probe("function-parens-50k", function () {{ Function("return " + "(".repeat(50000) + "1" + ")".repeat(50000)); }});
            probe("eval-arrays-100k", function () {{ eval("[".repeat(100000) + "]".repeat(100000)); }});
            probe("function-arrays-100k", function () {{ Function("return " + "[".repeat(100000) + "]".repeat(100000)); }});
            probe("eval-blocks-100k", function () {{ eval("{{".repeat(100000) + "}}".repeat(100000)); }});
            probe("eval-functions", function () {{ eval("function f(){{".repeat(R) + "}}".repeat(R)); }});
            probe("eval-heritage-100k", function () {{ eval("(" + "class extends ".repeat(100000) + "Object" + " {{}}".repeat(100000) + ")"); }});
            probe("function-params-100k", function () {{ Function("a = " + "(".repeat(100000) + "1" + ")".repeat(100000), "return a"); }});
            probe("eval-member-chain", function () {{ eval("var o = {{}}; o" + ".x".repeat(C)); }});
            probe("eval-binary-chain", function () {{ eval("1" + "+1".repeat(C)); }});
            probe("eval-logical-chain", function () {{ eval("var t = 1; t" + "&&t".repeat(C)); }});
            probe("eval-decorator-chain", function () {{ eval("@a" + ".b".repeat(C) + " class Q {{}}"); }});
            var composite = "o";
            for (var i = 0; i < {levels}; i++) composite = "(" + composite + ".x".repeat({links}) + ")";
            probe("eval-composite", function () {{ eval("var o = {{}}; " + composite); }});
            var stacked = "o", tier = ".x".repeat({tier_links});
            for (var i = 0; i < {tier_levels}; i++) stacked = "(" + stacked + tier + ")";
            try {{ eval("var o = {{}}; " + stacked); results.push("eval-stacked-tiers:accepted"); }}
            catch (e) {{ results.push("eval-stacked-tiers:" + e.name + ":" + /syntax tree is too deep/.test(e.message)); }}
            console.log(results.join("\n"));
            console.log("after");
            "#
        )
    }

    const DEEP_EXPECTED: &[&str] = &[
        "eval-parens-40k:RangeError",
        "eval-parens:RangeError",
        "function-parens-50k:RangeError",
        "eval-arrays-100k:RangeError",
        "function-arrays-100k:RangeError",
        "eval-blocks-100k:RangeError",
        "eval-functions:RangeError",
        "eval-heritage-100k:RangeError",
        "function-params-100k:RangeError",
        "eval-member-chain:RangeError",
        "eval-binary-chain:RangeError",
        "eval-logical-chain:RangeError",
        "eval-decorator-chain:RangeError",
        "eval-composite:RangeError",
        "eval-stacked-tiers:RangeError:true",
    ];

    #[test]
    fn deep_nesting_child() {
        if std::env::var_os("ZIPP_AUDIT_PARSE_DEEP_CHILD").is_none() {
            return;
        }
        let out = run_on_big_stack(deep_probes()).expect("probe program compiles");
        assert!(out.error.is_none(), "uncaught: {:?}", out.error);
        let mut expected: Vec<String> = DEEP_EXPECTED.iter().map(|s| s.to_string()).collect();
        expected.push("after".into());
        let got: Vec<String> = out
            .output
            .iter()
            .flat_map(|line| line.split('\n').map(str::to_string))
            .collect();
        assert_eq!(got, expected);

        // A script FILE that nests too deeply is rejected the same way, as a
        // RangeError from the compile step rather than an abort.
        let file = format!("console.log({}1{});", "(".repeat(40_000), ")".repeat(40_000));
        let error = match run_on_big_stack(file) {
            Err(error) => error,
            Ok(outcome) => outcome.error.expect("a 40,000-deep script must not run"),
        };
        assert!(error.contains("RangeError"), "{error}");
    }

    #[test]
    fn deep_nesting_fails_closed_in_every_mode() {
        if std::env::var_os("ZIPP_AUDIT_PARSE_DEEP_CHILD").is_some()
            || std::env::var_os("ZIPP_AUDIT_PARSE_VALUE_CHILD").is_some()
        {
            return;
        }
        let exe = std::env::current_exe().expect("test binary path");
        for (mode, env) in [("default", None), ("interpreter", Some(("ZIPP_NOJIT", "1")))] {
            let mut cmd = Command::new(&exe);
            cmd.args([
                "--exact",
                "default_profile_nesting::deep_nesting_child",
                "--nocapture",
            ])
            .env_remove("ZIPP_NOJIT")
            .env("ZIPP_AUDIT_PARSE_DEEP_CHILD", "1");
            if let Some((key, value)) = env {
                cmd.env(key, value);
            }
            let out = cmd.output().expect("spawn deep-nesting child");
            assert!(
                out.status.success(),
                "deep-nesting probes escaped in {mode} mode:\n--- stdout ---\n{}\n--- stderr ---\n{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }

    /// The other direction: every depth node v24 accepts (bisected per shape
    /// with `Function(source)` on 2026-09-15) still compiles and runs.
    #[test]
    fn depths_node_accepts_still_compile() {
        let shapes: Vec<(&str, String)> = vec![
            ("parens", format!("x = {}1{};", "(".repeat(1773), ")".repeat(1773))),
            ("arrays", format!("x = {}{};", "[".repeat(2388), "]".repeat(2388))),
            ("objects", format!("x = {}1{};", "({a:".repeat(757), "})".repeat(757))),
            ("functions", format!("{}{}", "function f(){".repeat(1070), "}".repeat(1070))),
            ("blocks", format!("{}{}", "{".repeat(2834), "}".repeat(2834))),
            ("assignments", format!("{}1;", "x=".repeat(3267))),
            ("arrows", format!("x = {}1;", "()=>".repeat(926))),
            ("templates", format!("x = {}1{};", "`${".repeat(2069), "}`".repeat(2069))),
            ("members", format!("x = {{}}; x{};", "?.x".repeat(5665))),
            ("calls", format!("var o = {{ m() {{ return o; }} }}; o{};", ".m()".repeat(3896))),
            ("unary", format!("x = {}1;", "!".repeat(8000))),
            ("else-if", {
                let mut s = String::from("var x = 3; if (x === 0) {}");
                for arm in 1..4153 {
                    s.push_str(&format!(" else if (x === {arm}) {{}}"));
                }
                s
            }),
            ("sum", format!("x = 1{};", "+1".repeat(20_000))),
        ];
        for (name, source) in shapes {
            let outcome = run_on_big_stack(format!("var x; {source}"))
                .unwrap_or_else(|error| panic!("{name}: node-accepted depth rejected: {error}"));
            assert!(outcome.error.is_none(), "{name}: {:?}", outcome.error);
        }
    }

    /// Chains in SIBLING subtrees build a shallow tree, however many links
    /// they hold in total, so the parser's running height bound must not add
    /// them up: the `&&` arms of an `||` chain and the decorators of one
    /// class each once summed past `MAX_NATIVE_TREE_DEPTH_BOUND` here.
    #[test]
    fn sibling_chains_do_not_count_as_depth() {
        let arms = 500usize;
        let arm = format!("t{}", "&&t".repeat(arms - 1));
        let decorator_links = MAX_NATIVE_AST_NESTING - 1_000;
        let decorators = MAX_NATIVE_TREE_DEPTH_BOUND / decorator_links + 2;
        assert!(arms * arms > MAX_NATIVE_TREE_DEPTH_BOUND);
        for (name, source) in [
            ("or-of-and", format!("var t = 1, r = {};", vec![arm; arms].join("||"))),
            (
                "decorators",
                format!("{}class Q {{}}", format!("@d{} ", ".x".repeat(decorator_links)).repeat(decorators)),
            ),
        ] {
            let result = std::thread::Builder::new()
                .stack_size(1 << 30)
                .spawn(move || zipp_vm::compile_to_text(&source, false))
                .expect("spawn")
                .join()
                .expect("a shallow tree compiles without overflowing");
            result.unwrap_or_else(|error| panic!("{name}: shallow sibling chains rejected: {error}"));
        }
    }

    /// The real-application corpus the hardened profile is calibrated against
    /// compiles under the default profile's limits too.
    #[test]
    fn real_program_corpus_compiles_under_the_default_limits() {
        let dir =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/syntax-corpus");
        let mut count = 0;
        for entry in std::fs::read_dir(&dir).expect("syntax corpus") {
            let path = entry.expect("corpus entry").path();
            if path.extension().is_some_and(|ext| ext == "js") {
                let source = std::fs::read_to_string(&path).expect("read corpus file");
                zipp_vm::compile_to_text(&source, false)
                    .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
                count += 1;
            }
        }
        assert!(count >= 5, "syntax corpus lost files");
    }
}

/// The hardened profile's guard now also covers the two recursion paths that
/// bypassed it: class heritage and decorator member chains.
#[cfg(feature = "safe-sandbox")]
#[test]
fn hardened_profile_bounds_heritage_and_decorator_chains() {
    for source in [
        format!("x = {}Object{};", "class extends ".repeat(2_000), " {}".repeat(2_000)),
        format!("@a{} class C {{}}", ".b".repeat(2_000)),
    ] {
        let error = std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(move || zipp_vm::compile_to_text(&source, false))
            .expect("spawn")
            .join()
            .expect("must fail closed")
            .expect_err("hostile nesting must be rejected");
        assert!(error.contains("sandbox limit"), "{error}");
    }
}
