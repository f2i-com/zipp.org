//! Block functions have a lexical binding as well as the Annex B outer update.
fn output(source: &str) -> Vec<String> {
    let result = zipp_vm::run(source).expect("valid sloppy JavaScript");
    assert!(result.error.is_none(), "{:?}", result.error);
    result.output
}

#[test]
fn arguments_block_function_is_hoisted_and_updates_outer_at_declaration() {
    assert_eq!(
        output(
            r#"
            function simple() {
                const outer = () => typeof arguments;
                console.log(outer());
                {
                    console.log(arguments(), outer());
                    function arguments() { return 7; }
                    console.log(arguments(), outer());
                }
                console.log(arguments());
            }
            function defaults(x = 1) {
                { console.log(arguments()); function arguments() { return 9; } }
                console.log(arguments());
            }
            function rest(...xs) {
                { console.log(arguments()); function arguments() { return 11; } }
                console.log(arguments());
            }
            simple(); defaults(); rest();
        "#
        ),
        [
            "object",
            "7 object",
            "7 function",
            "7",
            "9",
            "9",
            "11",
            "11"
        ]
    );
}

#[test]
fn arguments_formal_parameter_keeps_its_outer_binding() {
    assert_eq!(
        output(
            r#"
            function formal(arguments) {
                { console.log(arguments()); function arguments() { return 13; } }
                console.log(arguments);
            }
            function lexical() {
                { let arguments = 17;
                  { console.log(arguments()); function arguments() { return 19; } }
                  console.log(arguments);
                }
            }
            function rest(...arguments) {
                { console.log(arguments()); function arguments() { return 47; } }
                console.log(arguments[0]);
            }
            function pattern({arguments}) {
                { console.log(arguments()); function arguments() { return 53; } }
                console.log(arguments);
            }
            formal(23); lexical(); rest(59); pattern({arguments: 61});
        "#
        ),
        ["13", "23", "19", "17", "47", "59", "53", "61"]
    );
}

#[test]
fn arrow_block_function_has_its_own_var_without_overwriting_parent_arguments() {
    assert_eq!(
        output(
            r#"
            function parent() {
                const outer = () => typeof arguments;
                (() => {
                    console.log(typeof arguments, outer());
                    { console.log(arguments()); function arguments() { return 29; } }
                    console.log(typeof arguments, outer());
                })();
            }
            parent();
        "#
        ),
        ["undefined object", "29", "function object"]
    );
}

#[test]
fn arrow_annexb_updates_vars_but_preserves_parameters_and_strict_scopes() {
    assert_eq!(
        output(
            r#"
            (() => {
                const read = () => typeof f;
                console.log(read());
                { console.log(f(), read()); function f() { return 31; } }
                console.log(f(), read());
            })();
            (({f}) => {
                { console.log(f()); function f() { return 37; } }
                console.log(f);
            })({f: 41});
            (() => {
                "use strict";
                { console.log(f()); function f() { return 43; } }
                console.log(typeof f);
            })();
        "#
        ),
        [
            "undefined",
            "31 undefined",
            "31 function",
            "37",
            "41",
            "43",
            "undefined"
        ]
    );
}
