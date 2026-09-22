//! A destructuring declaration's defaults run while its own names are still in
//! their temporal dead zone: `let [a = b, b] = []` reads `b` before it exists
//! and must throw, in a block and in a function body, never read `undefined`
//! or an outer binding of the same name.
fn output(source: &str) -> Vec<String> {
    let result = zipp_vm::run(source).expect("valid program");
    assert!(result.error.is_none(), "{:?}", result.error);
    result.output
}

#[test]
fn a_default_that_reads_a_later_name_of_the_same_pattern_throws() {
    assert_eq!(
        output(
            r#"
        function T(f) {
            try { return JSON.stringify(f()); }
            catch (e) { return e instanceof ReferenceError ? "ReferenceError" : String(e); }
        }
        console.log(T(() => { { let [a = b, b] = []; return [a, b]; } }));
        console.log(T(() => { { let { a = b, b } = { b: 2 }; return [a, b]; } }));
        console.log(T(() => { { let { x = x } = {}; return x; } }));
        console.log(T(function () { let [a = b, b] = []; return [a, b]; }));
        console.log(T(() => { let b0 = "outer"; { let { a = b0, b0 } = {}; return a; } }));
        console.log(T(() => { let q = 3; { let [a = q] = []; return a; } }));
        console.log(T(() => { { let [f = () => g, g = 2] = []; return f(); } }));
    "#
        ),
        [
            "ReferenceError",
            "ReferenceError",
            "ReferenceError",
            "ReferenceError",
            "ReferenceError",
            "3",
            "2"
        ]
    );
}
