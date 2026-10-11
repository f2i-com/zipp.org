//! A string accumulator must never be a string someone else holds.
//!
//! `s += x` in a loop grows `s` in place when the compiler can prove nothing
//! else names it. A literal that spells a `typeof` result ("number",
//! "function", ...) resolves to the engine's one shared copy of that word, so
//! a loop opened with such a literal appended to the word itself: after
//! `var s = "number"; for (...) s += i`, `typeof 1` printed `number012` and
//! compared unequal to a "number" built at run time, in the interpreter, the
//! compiled tiers and the WebAssembly modules alike.

use zipp_vm::embed;

fn run(source: &str) -> Vec<String> {
    let mut state = embed::compile_script(source).expect("source compiles");
    state.run_init().expect("source runs");
    state.take_output()
}

#[test]
fn a_literal_that_spells_a_typeof_result_is_not_the_typeof_result() {
    let out = run(r#"
var NAMES = ["undefined", "object", "boolean", "number", "string", "function", "symbol", "bigint"];
function viaLiteral_undefined(n) { var s = "undefined"; for (var i = 0; i < n; i++) s += i; return s; }
function viaLiteral_object(n) { var s = "object"; for (var i = 0; i < n; i++) s += i; return s; }
function viaLiteral_boolean(n) { var s = "boolean"; for (var i = 0; i < n; i++) s += i; return s; }
function viaLiteral_number(n) { var s = "number"; for (var i = 0; i < n; i++) s += i; return s; }
function viaLiteral_string(n) { var s = "string"; for (var i = 0; i < n; i++) s += i; return s; }
function viaLiteral_function(n) { var s = "function"; for (var i = 0; i < n; i++) s += i; return s; }
function viaLiteral_symbol(n) { var s = "symbol"; for (var i = 0; i < n; i++) s += i; return s; }
function viaLiteral_bigint(n) { var s = "bigint"; for (var i = 0; i < n; i++) s += i; return s; }
function viaTemplate(n) { var s = `function`; for (var i = 0; i < n; i++) s += i; return s; }
function viaTypeof(n) { var s = typeof n; for (var i = 0; i < n; i++) s += i; return s; }
function viaPair(n) { var s = "string"; for (var i = 0; i < n; i++) { s += "ab" + i; } return s; }
function viaIndex(n, a) { var s = "object"; for (var i = 0; i < n; i++) { s += a[i % 3]; } return s; }
var fns = [viaLiteral_undefined, viaLiteral_object, viaLiteral_boolean, viaLiteral_number,
           viaLiteral_string, viaLiteral_function, viaLiteral_symbol, viaLiteral_bigint];
for (var round = 0; round < 3; round++) {
  var row = [];
  for (var k = 0; k < fns.length; k++) row.push(fns[k](3));
  row.push(viaTemplate(3), viaTypeof(3), viaPair(2), viaIndex(4, ["x", "y", "z"]));
  console.log(row.join(" "));
}
var values = [undefined, null, true, 1, "", function () {}, Symbol(), 1n];
var seen = [];
for (var v = 0; v < values.length; v++) {
  var t = typeof values[v];
  // Compared by content against a string built at run time, and printed.
  seen.push(t + ":" + (t === NAMES[v].slice(0, 2) + NAMES[v].slice(2)) + ":" + t.length);
}
console.log(seen.join(" "));
"#);
    let row = "undefined012 object012 boolean012 number012 string012 function012 symbol012 bigint012 \
               function012 number012 stringab0ab1 objectxyzx";
    assert_eq!(
        out,
        [
            row,
            row,
            row,
            "undefined:true:9 object:true:6 boolean:true:7 number:true:6 string:true:6 \
             function:true:8 symbol:true:6 bigint:true:6",
        ]
    );
}
