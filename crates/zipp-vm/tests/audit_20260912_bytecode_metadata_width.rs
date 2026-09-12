//! Hostile source must not wrap compiler metadata fields that are narrower
//! than the in-memory AST collections they index.

#[test]
fn call_argument_window_cannot_wrap_register_operands() {
    let mut source = String::from("function sink() {} sink(");
    for i in 0..=u16::MAX as usize {
        if i != 0 {
            source.push(',');
        }
        source.push('0');
    }
    source.push_str(");");

    let error = match zipp_vm::run(&source) {
        Err(error) => error,
        Ok(_) => panic!("the call argument window cannot fit in the register file"),
    };
    assert!(error.contains("registers"), "{error}");
}

#[test]
fn direct_eval_register_exhaustion_returns_an_error() {
    let mut source = String::from("function probe() { let secret = 7, guard = false;");
    for _ in 0..u16::MAX {
        source.push_str("if (guard) { eval(''); }");
    }
    source.push_str("return eval('secret'); }");

    let error = match zipp_vm::run(&source) {
        Err(error) => error,
        Ok(_) => panic!("the DirectEval site index cannot represent this source"),
    };
    assert!(error.contains("registers"), "{error}");
}

#[test]
fn decorator_register_window_cannot_overflow() {
    let mut source = String::from("function outer() {");
    for _ in 0..16_384 {
        source.push_str("@dec ");
    }
    source.push_str("class C {} }");

    let error = match zipp_vm::run(&source) {
        Err(error) => error,
        Ok(_) => panic!("the decorator value/receiver window cannot fit in the register file"),
    };
    assert!(error.contains("registers"), "{error}");
}

#[test]
fn tagged_template_windows_cannot_overflow() {
    let mut source = String::from("function outer(tag) { tag`");
    for _ in 0..32_768 {
        source.push_str("${0}");
    }
    source.push_str("`; }");

    let error = match zipp_vm::run(&source) {
        Err(error) => error,
        Ok(_) => panic!("the tagged-template windows cannot fit in the register file"),
    };
    assert!(error.contains("registers"), "{error}");
}

#[test]
fn large_array_literal_preserves_length_endpoints_and_neighboring_locals() {
    let mut source = String::from("function probe() { let before = 37; let values = [11");
    for _ in 0..u16::MAX {
        source.push_str(",0");
    }
    source.push_str(
        ",99]; let after = 5; console.log(values.length, values[0], values[65536], before + after); } probe();",
    );

    let outcome = zipp_vm::run(&source).unwrap_or_else(|error| panic!("compile failed: {error}"));
    assert_eq!(outcome.error, None);
    assert_eq!(outcome.output, ["65537 11 99 42"]);
}

#[test]
fn computed_class_field_index_cannot_wrap() {
    let mut source = String::from("function outer(k) { class C {");
    for _ in 0..=u16::MAX as usize {
        source.push_str("[k];");
    }
    source.push_str("[k]; } }");

    let error = match zipp_vm::run(&source) {
        Err(error) => error,
        Ok(_) => panic!("the computed-field key index cannot represent this source"),
    };
    assert!(error.contains("computed class fields"), "{error}");
}
