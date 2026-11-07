//! A method call whose argument computes over a local (`s.charCodeAt(i + 1)`)
//! fuses its property Get after the arguments only when the local provably
//! holds a number, `undefined`, `null` or a boolean (`capture::numeric_locals`):
//! then the argument runs no user code and cannot throw. Every other local
//! keeps the Get first. The corpus pairs a getter on the method with operands
//! whose `valueOf`/`toString` log, BigInts, destructuring, for-in/of, catch,
//! parameters, closures, eval, `with` and `arguments`; the expected lines are
//! Node's.

const SOURCE: &str = r##"// Method calls whose argument reads a local: the method's Get must come
// before any user code the argument runs, and before a throw it raises.
var out = [];
function log(s) { out.push(s); }
var obj = { get m() { log("get"); return function (a) { log("call " + String(a)); return a; }; } };
var k = { valueOf() { log("valueOf"); return 1; }, toString() { log("toString"); return "K"; } };
function t(name, f) { log("== " + name); try { f(); } catch (e) { log("threw " + e.constructor.name); } }

t("plain number", function () { var i = 0; obj.m(i + 1); });
t("let number", function () { let i = 2; i *= 3; obj.m(i - 1); });
t("undefined local", function () { var i; obj.m(i + 1); });
t("bool local", function () { var b = true; obj.m(b + 1); });
t("later object write", function () { var i = 0; obj.m(i + 1); i = k; obj.m(i + 1); });
t("object init", function () { var i = k; obj.m(i + 1); });
t("plus-assign object", function () { var i = 0; i += k; obj.m(i + 1); });
t("minus-assign object", function () { var i = 5; i -= k; obj.m(i + 1); });
t("string write", function () { var i = 0; i = i + "x"; obj.m(i + 1); });
t("string cond", function (c) { var i = c ? 1 : "s"; obj.m(i + 1); });
t("bigint", function () { var i = 1n; obj.m(i + 1); });
t("bigint mul", function () { var j = 2n; var i = j * 3n; obj.m(i + 1); });
t("bigint unary", function () { var j = 2n; var i = -j; obj.m(i + 1); });
t("destructure", function () { var [i] = [k]; obj.m(i + 1); });
t("destructure obj", function () { var { i } = { i: k }; obj.m(i + 1); });
t("assign destructure", function () { var i = 0; [i] = [k]; obj.m(i + 1); });
t("for-in", function () { for (var i in { a: 1 }) obj.m(i + 1); });
t("for-of", function () { for (var i of [k]) obj.m(i + 1); });
t("catch", function () { try { throw k; } catch (i) { obj.m(i + 1); } });
t("param var", function () { (function (i) { var i; obj.m(i + 1); })(k); });
t("param pattern var", function () { (function ({ i }) { var i; obj.m(i + 1); })({ i: k }); });
t("param default", function () { (function (a = 1) { var i = 0; obj.m(i + a); })(k); });
t("closure write", function () { var i = 0; (function () { i = k; })(); obj.m(i + 1); });
t("arrow write", function () { var i = 0; var f = () => { i = k; }; f(); obj.m(i + 1); });
t("eval write", function () { var i = 0; eval("i = k"); obj.m(i + 1); });
t("with", function () { var i = 0; with ({ i: k }) { obj.m(i + 1); } });
t("arguments alias", function () { (function (i) { arguments[0] = k; obj.m(i + 1); })(0); });
t("fn decl name", function () { var i = 0; function i() {} obj.m(i + 1); });
t("block shadow", function () { let i = 0; { let i = k; obj.m(i + 1); } obj.m(i + 1); });
t("logical assign", function () { var i = 0; i ||= k; obj.m(i + 1); });
t("coalesce assign", function () { var i = null; i ??= 4; obj.m(i + 1); });
t("seq", function () { var i = (k, 3); obj.m(i + 1); });
t("typeof", function () { var i = typeof k; obj.m(i + 1); });
t("template", function () { var i = 3; obj.m(`${i}!`); });
t("unary plus", function () { var i = +k; obj.m(i + 1); });
t("ushr", function () { var i = k >>> 0; obj.m(i + 1); });
t("or zero", function () { var i = k | 0; obj.m(i + 1); });
t("mul object", function () { var i = k * 1; obj.m(i + 1); });
t("add objects", function () { var i = 1 + k; obj.m(i + 1); });
t("cycle", function () { var a = 0, b = 0; for (var n = 0; n < 3; n++) { a = b + 1; b = a + 1; } obj.m(a + b); });
t("cycle tainted", function () { var a = 0, b = 0; for (var n = 0; n < 3; n++) { a = b + 1; b = n < 2 ? a + 1 : k; } obj.m(a + b); });
t("update", function () { var i = 0; i++; ++i; i--; obj.m(i * 2); });
t("generator", function () { function* g() { var i = 1; yield 0; obj.m(i + 1); } var it = g(); it.next(); it.next(); });
t("class method", function () { class C { run() { var i = 4; obj.m(i + 1); } } new C().run(); });
t("arrow body", function () { var f = () => { let i = 7; obj.m(i + 1); }; f(); });
t("loop hot", function () {
  var s = "abcdefghij", sum = 0;
  for (var r = 0; r < 3000; r++) for (var i = 0; i < 9; i++) sum = (sum + s.charCodeAt(i + 1) + s.indexOf("c", i % 3)) | 0;
  var a = [];
  for (var j = 0; j < 2000; j++) a.push(j * 2, -j);
  log("loop " + sum + " " + a.length + " " + a[1999]);
});
t("getter swaps method", function () {
  var calls = 0;
  var o = { get f() { calls++; var c = calls; return function (x) { return c * 100 + x; }; } };
  var acc = 0;
  for (var i = 0; i < 500; i++) acc += o.f(i % 7 + 1);
  log("swaps " + acc + " " + calls);
});
console.log(out.join("\n"));
"##;

const NODE: &str = r##"== plain number
get
call 1
== let number
get
call 5
== undefined local
get
call NaN
== bool local
get
call 2
== later object write
get
call 1
get
valueOf
call 2
== object init
get
valueOf
call 2
== plus-assign object
valueOf
get
call 2
== minus-assign object
valueOf
get
call 5
== string write
get
call 0x1
== string cond
get
call s1
== bigint
get
threw TypeError
== bigint mul
get
threw TypeError
== bigint unary
get
threw TypeError
== destructure
get
valueOf
call 2
== destructure obj
get
valueOf
call 2
== assign destructure
get
valueOf
call 2
== for-in
get
call a1
== for-of
get
valueOf
call 2
== catch
get
valueOf
call 2
== param var
get
valueOf
call 2
== param pattern var
get
valueOf
call 2
== param default
get
valueOf
call 1
== closure write
get
valueOf
call 2
== arrow write
get
valueOf
call 2
== eval write
get
valueOf
call 2
== with
get
valueOf
call 2
== arguments alias
get
valueOf
call 2
== fn decl name
get
call 1
== block shadow
get
valueOf
call 2
get
call 1
== logical assign
get
valueOf
call 2
== coalesce assign
get
call 5
== seq
get
call 4
== typeof
get
call object1
== template
get
call 3!
== unary plus
valueOf
get
call 2
== ushr
valueOf
get
call 2
== or zero
valueOf
get
call 2
== mul object
valueOf
get
call 2
== add objects
valueOf
get
call 3
== cycle
get
call 11
== cycle tainted
get
valueOf
call 6
== update
get
call 2
== generator
get
call 2
== class method
get
call 5
== arrow body
get
call 8
== loop hot
loop 2808000 4000 -999
== getter swaps method
swaps 12526994 500
"##;

fn lines(s: &str) -> Vec<String> {
    s.lines().map(str::to_owned).collect()
}

#[test]
fn numeric_locals_parity() {
    let got = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(got.error.is_none(), "unexpected runtime error: {:?}", got.error);
    let got: Vec<String> = got.output.iter().flat_map(|s| s.lines().map(str::to_owned)).collect();
    assert_eq!(got, lines(NODE.trim_start_matches('\n')));
}

/// The corpus again with the analysis off, eager JIT, GC stress and the
/// interpreter, each in a fresh process.
#[test]
fn numeric_locals_all_modes() {
    let exe = std::env::current_exe().expect("test exe path");
    for mode in [
        ("ZIPP_NO_NUMERIC_LOCALS", "1"),
        ("ZIPP_JIT_THRESHOLD", "1"),
        ("ZIPP_GC_STRESS", "1"),
        ("ZIPP_NOJIT", "1"),
    ] {
        let out = std::process::Command::new(&exe)
            .args(["numeric_locals_parity", "--exact"])
            .env(mode.0, mode.1)
            .output()
            .expect("spawn test binary");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{mode:?} failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!stdout.contains("running 0 tests"), "mode filter matched nothing: {stdout}");
    }
}