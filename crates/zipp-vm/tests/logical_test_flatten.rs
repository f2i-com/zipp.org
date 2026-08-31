//! `&&` used only as a branch test can branch after each falsy operand instead
//! of materialising the intermediate JavaScript values.  This both removes
//! dispatches and exposes nested `<` / `<=` operands to the existing fused
//! compare-jump lowering.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

fn text_of(src: &str) -> String {
    zipp_vm::compile_to_text(src, false).expect("source compiles")
}

#[test]
fn nested_compare_in_and_test_uses_control_form() {
    let text = text_of("function f(ok, i, n) { while (ok && i < n && i <= 9) i++; return i; }");
    assert!(
        text.contains("JumpIfNotLt"),
        "nested `<` should fuse:\n{text}"
    );
    assert!(
        text.contains("JumpIfNotLe"),
        "nested `<=` should fuse:\n{text}"
    );
    assert_eq!(
        text.matches("JumpIfFalse").count(),
        1,
        "only the plain `ok` operand needs JumpIfFalse:\n{text}"
    );
}

#[test]
fn and_test_preserves_short_circuit_values_and_order() {
    let out = run_ok(
        r#"
        var log = [];
        function mark(name, value) { log.push(name); return value; }
        if (mark("a", 0) && mark("x", 1)) log.push("bad");
        if (mark("b", "yes") && mark("c", 2) && mark("d", true)) log.push("taken");
        var i = 0;
        while (mark("w" + i, i < 2) && mark("body" + i, true)) i++;
        console.log(log.join(","));
        console.log(i);
        "#,
    );
    assert_eq!(out, ["a,b,c,d,taken,w0,body0,w1,body1,w2", "2"]);
}

#[test]
fn and_in_value_position_is_unchanged() {
    let out = run_ok(
        r#"
        function f(a, b) { return a && b; }
        console.log(f(0, 7), f("left", "right"), typeof f("left", 3));
        var x = "kept";
        var y = x && 42;
        console.log(x, y);
        "#,
    );
    assert_eq!(out, ["0 right number", "kept 42"]);
}

#[test]
fn condition_expression_and_test_selects_one_arm() {
    let out = run_ok(
        r#"
        var n = 0;
        function hit(v) { n++; return v; }
        console.log((hit(true) && hit(1 < 2)) ? "yes" : "no", n);
        n = 0;
        console.log((hit(false) && hit(true)) ? "bad" : "short", n);
        "#,
    );
    assert_eq!(out, ["yes 2", "short 1"]);
}
