//! Lite's subtractive instruction set must not renumber the native Python cache.
#[path = "../build/instr_codec.rs"]
mod instr_codec;

#[test]
fn lite_exclusions_preserve_the_complete_codec_schema() {
    let source = include_str!("../src/bytecode.rs");
    let full_source = source.replace("#[cfg(not(feature = \"wasm-lite\"))]", "");
    let generated = instr_codec::generate(source);
    assert_eq!(generated, instr_codec::generate(&full_source));
    assert!(generated.contains("Instr::PyArith {"));
    assert!(generated.contains("Instr::PyCall {"));
    assert!(generated.contains("fn enc_PyArithOp"));
}

#[test]
fn unrelated_conditional_variants_are_still_rejected() {
    let source = include_str!("../src/bytecode.rs").replacen(
        "#[cfg(not(feature = \"wasm-lite\"))]",
        "#[cfg(feature = \"python\")]",
        1,
    );
    assert!(std::panic::catch_unwind(|| instr_codec::generate(&source)).is_err());
}
