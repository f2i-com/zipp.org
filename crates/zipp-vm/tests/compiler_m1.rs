use std::fmt::Write;
use zipp_vm::{compile_to_text, run};

fn output(src: &str) -> Vec<String> {
    let outcome = run(src).expect("source compiles");
    assert!(
        outcome.error.is_none(),
        "unexpected runtime error: {:?}",
        outcome.error
    );
    outcome.output
}

#[test]
fn global_indexes_preserve_declarations_exports_and_builtin_shadowing() {
    assert_eq!(
        output(
            r#"
            var duplicate;
            var duplicate = 7;
            var TypeError = function (message) { this.tag = "user:" + message; };
            console.log(duplicate, new TypeError("ok").tag);
            with ({}) { console.log(delete duplicate); }
            "#,
        ),
        ["7 user:ok", "false"]
    );

    compile_to_text(
        r#"
        import { source as local } from "./dependency.mjs";
        export const named = local;
        export { local as renamed };
        export default () => named;
        "#,
        true,
    )
    .expect("module imports and exports compile");
}

#[test]
fn expression_arrow_analysis_preserves_lexical_semantics() {
    assert_eq!(
        output(
            r#"
            class Base { value() { return 10; } }
            class Child extends Base {
              constructor(x) { super(); this.x = x; }
              make(a = 2) {
                const captured = 3;
                return (b = a) =>
                  (() => this.x + super.value() + arguments[0] + captured + b)();
              }
            }
            const exact = (q = 1) => q + 2;
            console.log(new Child(4).make(5)(), exact.toString());
            const asyncArrow = async (x = 7) => (await Promise.resolve(x)) + 1;
            asyncArrow().then(v => console.log(v));
            "#,
        ),
        ["27 (q = 1) => q + 2", "8"]
    );

    assert!(run("function* g() { const bad = () => yield 1; }").is_err());
    assert!(run("const bad = () => await 1;").is_err());
}

#[test]
fn expression_arrows_preserve_direct_eval_scope() {
    assert_eq!(
        output(
            r#"
            const fromParam = (x) => eval("x");
            const fromDefault = (x, y = eval("x")) => y;
            function make() {
              let captured = 17;
              return () => eval("captured");
            }
            console.log(fromParam(9), fromDefault(11), make()());
            "#,
        ),
        ["9 11 17"]
    );
}

#[test]
fn expression_arrows_keep_enclosing_with_bindings() {
    assert_eq!(
        output(
            r#"
            var direct, nested;
            var value = "outer";
            with ({ value: "inner" }) {
              direct = () => value;
              nested = () => () => value;
            }
            console.log(direct(), nested()());
            "#,
        ),
        ["inner inner"]
    );
}

#[test]
#[ignore = "explicit large-source compiler smoke test"]
fn generated_global_and_module_sources_compile() {
    for count in [3_000, 6_000, 12_000, 24_000] {
        let mut source = String::with_capacity(count * 36);
        for i in 0..count {
            writeln!(source, "function generated_{i}() {{ return {i}; }}").unwrap();
        }
        drop(zipp_vm::embed::compile_script(&source).expect("generated script compiles"));
    }

    let mut module = String::with_capacity(20_000 * 32);
    for i in 0..20_000 {
        writeln!(module, "export const exported_{i} = {i};").unwrap();
    }
    let outcome =
        zipp_vm::run_module_with_base(&module, None).expect("20k-export module compiles and runs");
    assert!(outcome.error.is_none(), "{:?}", outcome.error);
}

#[test]
#[ignore = "explicit u32 global-slot end-to-end boundary test"]
fn global_function_slot_above_u16_preserves_self_recursion() {
    let mut source = String::with_capacity(2_500_000);
    for i in 0..=u16::MAX {
        writeln!(source, "function padding_{i}() {{}}").unwrap();
    }
    source.push_str(
        r#"
        function boundary(n) {
          return n ? boundary(n - 1) + 1 : 0;
        }
        console.log(boundary(64));
        "#,
    );
    assert_eq!(output(&source), ["64"]);
}
