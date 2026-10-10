//! Assignments and updates of local bindings: the value they yield, the
//! value nobody reads, and the bindings that refuse the store.

use zipp_vm::embed;

fn run(source: &str) -> Vec<String> {
    let mut state = embed::compile_script(source).expect("source compiles");
    state.run_init().expect("source runs");
    state.take_output()
}

fn value(body: &str) -> String {
    let out = run(&format!("function w(n) {{ {body} }}\nconsole.log(String(w(5)));"));
    assert_eq!(out.len(), 1, "{body}: {out:?}");
    out.into_iter().next().unwrap()
}

#[test]
fn statements_on_plain_locals_compute_what_they_did() {
    assert_eq!(value("let s = 0; for (let i = 0; i < n; i++) s += i; return s;"), "10");
    assert_eq!(value("let s = 0; for (let i = n; i > 0; --i) { s += i; s -= 1; } return s;"), "10");
    assert_eq!(value("let s = 1; let i = 0; while (i < n) { s *= 2; i += 1; } return s;"), "32");
    assert_eq!(value("let a = 7; a %= 4; a <<= 3; a |= 1; a ^= 2; a >>= 1; a **= 2; return a;"), "169");
    assert_eq!(value("let a = 'x'; a += n; a += a; return a;"), "x5x5");
    assert_eq!(value("let a = 1, b = 2; a = b; b = a + n; a++; b--; return a + ':' + b;"), "3:6");
    assert_eq!(value("var v = 1; v++; v += 2; v = v * n; return v;"), "20");
    assert_eq!(value("n++; n += 1; n = n + 1; return n;"), "8");
}

#[test]
fn a_value_that_is_read_is_still_the_right_one() {
    assert_eq!(value("let x = 1; let y = (x += 2); x += 10; return x + ':' + y;"), "13:3");
    assert_eq!(value("let i = 3; let old = i++; let now = ++i; return old + ':' + now + ':' + i;"), "3:5:5");
    assert_eq!(value("let x = 1; x = x++; return x;"), "1");
    assert_eq!(value("let x = 1; x = ++x + x++; return x;"), "4");
    assert_eq!(value("let x = 2; x += (x = 10, 5); return x;"), "7");
    assert_eq!(value("let x = 2; x += x++; return x;"), "4");
    assert_eq!(value("let s = ''; for (let i = 0; i < 3; i++) s += (i += 1); return s;"), "13");
    assert_eq!(value("let a = 1, b; b = a = n; a += 1; return a + ':' + b;"), "6:5");
    assert_eq!(value("let i = 0, s = 0; while ((i += 2) < 9) s += i; return s + ':' + i;"), "20:10");
}

#[test]
fn a_right_hand_side_that_reads_its_own_target_reads_the_old_value() {
    assert_eq!(value("let e = 3; e = { v: e }; return e.v;"), "3");
    assert_eq!(value("let e = 'a'; e = `${e}${e}b`; return e;"), "aab");
    assert_eq!(value("let e = [1]; e = [e.length, e[0] + n]; return e.join();"), "1,6");
    assert_eq!(value("let f = 1; f = function () { return typeof f; }; return f();"), "function");
    assert_eq!(value("let f; f = () => 1; return f.name;"), "f");
}

#[test]
fn bindings_that_are_not_plain_registers_keep_their_rules() {
    // A captured loop variable is a cell, fresh for each iteration.
    assert_eq!(
        value("const fs = []; for (let i = 0; i < 3; i++) fs.push(() => i); return fs.map(f => f()).join();"),
        "0,1,2"
    );
    assert_eq!(value("let c = 0; const bump = () => { c++; c += 2; }; bump(); bump(); return c;"), "6");
    // A constant refuses the store after evaluating the right-hand side.
    assert_eq!(
        value("const k = 1; let seen = 0; try { k += (seen = 1); } catch (e) { return e.name + seen + k; } return 'no';"),
        "TypeError11"
    );
    assert_eq!(value("const k = 1; try { k++; } catch (e) { return e.name + k; } return 'no';"), "TypeError1");
    assert_eq!(value("const k = 1; try { --k; } catch (e) { return e.name + k; } return 'no';"), "TypeError1");
    assert_eq!(
        value("const k = '5'; let r; try { r = k++; } catch (e) { return e.name + typeof k + k + r; } return 'no';"),
        "TypeErrorstring5undefined"
    );
    assert_eq!(
        value("const k = '5'; try { k--; } catch (e) { return e.name + typeof k + k; } return 'no';"),
        "TypeErrorstring5"
    );
    // A function expression's own name is read-only and silently so in sloppy code.
    assert_eq!(value("return (function f() { f = 1; f++; f += 1; return typeof f; })();"), "function");
    assert_eq!(
        value("return (function f() { 'use strict'; try { f = 1; } catch (e) { return e.name; } return 'no'; })();"),
        "TypeError"
    );
    assert_eq!(value("return (function f() { var r = f++; return typeof f + ':' + r; })();"), "function:NaN");
    assert_eq!(value("return (function f() { var r = --f; return typeof f + ':' + r; })();"), "function:NaN");
    assert_eq!(
        value("return (function f() { 'use strict'; try { f++; } catch (e) { return e.name + typeof f; } return 'no'; })();"),
        "TypeErrorfunction"
    );
    // `arguments` aliases the formals in sloppy code.
    assert_eq!(value("arguments[0] = 9; n += 1; return n + ':' + arguments[0];"), "10:10");
    assert_eq!(value("n = 2; n++; return arguments[0];"), "3");
    // A `with` object shadows the local.
    assert_eq!(
        value("let x = 1; const o = { x: 10 }; with (o) { x += n; x++; x = x * 2; } return x + ':' + o.x;"),
        "1:32"
    );
    // A global, and an undeclared name in sloppy code.
    let out = run("var g = 1; g += 2; g++; h = 5; h += g; h--; console.log(g + ':' + h);");
    assert_eq!(out, ["4:8"]);
}

#[test]
fn a_temporal_dead_zone_still_throws() {
    assert_eq!(value("try { t += 1; } catch (e) { return e.name; } let t = 0; return t;"), "ReferenceError");
    assert_eq!(value("try { t++; } catch (e) { return e.name; } let t = 0; return t;"), "ReferenceError");
    assert_eq!(value("try { t = 1; } catch (e) { return e.name; } let t = 0; return t;"), "ReferenceError");
    let out = run(
        "function d(a = (b = 5), b) { return a + ':' + b; }\n\
         try { d(); console.log('no'); } catch (e) { console.log(e.name); }",
    );
    assert_eq!(out, ["ReferenceError"]);
}

#[test]
fn coercions_run_once_and_in_order() {
    assert_eq!(
        value(
            "let log = ''; let o = { valueOf() { log += 'v'; return 4; } };\
             let x = o; x++; let y = o; y += 1; let z = o; z = z * 2; return log + x + y + z;"
        ),
        "vvv558"
    );
    assert_eq!(value("let b = 2n; b++; b += 3n; b *= 2n; return b;"), "12");
    assert_eq!(value("let u; u++; let s = 'a'; s++; return u + ':' + s;"), "NaN:NaN");
}

#[test]
fn an_eval_program_completes_with_the_statement_value() {
    let out = run("var x = 1; console.log(eval('x += 4')); console.log(eval('x++')); console.log(eval('var q = 1; q += 2; q++')); console.log(x);");
    assert_eq!(out, ["5", "5", "3", "6"]);
    let out = run("function f() { let y = 1; return eval('y += 4; y++') + ':' + y; } console.log(f());");
    assert_eq!(out, ["5:6"]);
}
