//! Invalid programs must fail in the parser with SyntaxError, rather than fall
//! through to an untyped compiler "unsupported subset" diagnostic. A stricter
//! Test262 runner exposed these gaps in rest/arrow and private-name grammar.

fn syntax_error(source: &str) {
    for strict in [false, true] {
        let source = if strict {
            format!("\"use strict\";\n{source}")
        } else {
            source.to_owned()
        };
        let error = zipp_vm::parse_to_text(&source, false)
            .expect_err(&format!("invalid syntax was accepted: {source}"));
        assert!(error.starts_with("SyntaxError:"), "{source}: {error}");
    }
}

fn parses(source: &str) {
    zipp_vm::parse_to_text(source, false)
        .unwrap_or_else(|error| panic!("valid source rejected: {source}: {error}"));
}

#[test]
fn arrow_rest_binding_cannot_have_a_default_but_nested_bindings_can() {
    for prefix in ["", "async "] {
        for rest in ["x = []", "[x] = []", "{x} = {}"] {
            syntax_error(&format!("({prefix}(...{rest}) => {{}});"));
        }
        for rest in ["x", "[x = 1]", "{x = 1}"] {
            parses(&format!("({prefix}(...{rest}) => {{}});"));
        }
    }
    // A spread argument is an AssignmentExpression, so the same token
    // sequence is valid when the cover head resolves to a call, not an arrow.
    parses("var x; async(...x = []);");
}

#[test]
fn yield_expression_in_arrow_head_is_an_early_error() {
    for parameter in ["x = yield", "x = (yield 1)", "x = yield* []", "[x = yield]"] {
        syntax_error(&format!("function* g() {{ ({parameter}) => {{}}; }}"));
    }
    // Contains(YieldExpression) does not descend into a nested generator body.
    parses("function* g() { (x = function* () { yield 1; }) => {}; }");
    parses("function* g() { var x = (yield 1); return (x); }");
    parses("var yield; (x = yield) => {};");
}

#[test]
fn object_literals_cannot_declare_private_properties_or_methods() {
    for member in [
        "#x: 1",
        "#x() {}",
        "*#x() {}",
        "async #x() {}",
        "async *#x() {}",
        "get #x() {}",
        "set #x(value) {}",
    ] {
        syntax_error(&format!("({{{member}}});"));
        syntax_error(&format!("class C {{ #x; field = {{{member}}}; }}"));
    }
    // Private class elements and private access inside a computed public key
    // remain valid: the restriction belongs to the property name grammar.
    parses("class C { #x = 1; field = {[this.#x]: 2, '#x': 3}; }");
    parses("class C { #x() {} *#g() {} async #a() {} get #v() {} set #v(v) {} }");
}

#[test]
fn object_patterns_cannot_bind_or_assign_private_property_names() {
    for body in [
        "const {#x: value} = this;",
        "let value; ({#x: value} = this);",
        "(({#x: value}) => {})(this);",
        "function f({#x: value}) {}",
    ] {
        syntax_error(&format!("class C {{ #x; method() {{ {body} }} }}"));
    }
    parses("class C { #x = 'key'; method() { const {[this.#x]: value} = {}; } }");
}

#[test]
fn super_property_grammar_does_not_allow_private_identifiers() {
    for expression in ["super.#x", "super.#x()", "new super.#x()", "super.#x = 1"] {
        syntax_error(&format!(
            "class C extends Object {{ #x; method() {{ {expression}; }} }}"
        ));
    }
    parses("class C extends Object { #x; method() { super.x; super['#x']; this.#x; } }");
    parses("class C { #x; method(other) { return other?.#x; } }");
}
