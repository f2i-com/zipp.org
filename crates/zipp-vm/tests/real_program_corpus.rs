//! The half of the parse-limit gate that proves the limits are not too LOW.
//!
//! `syntax_nesting_limits.rs` proves hostile shapes fail closed. Nothing proved
//! the other direction, and v0.0.1 shipped limits that rejected two working
//! applications: the release workflow's only browser check parsed
//! `console.log("zipp-web-release-smoke")`, which no nesting limit above zero
//! can reject. `tests/syntax-corpus/` is real reduced application source; this
//! test compiles all of it, so a future tightening fails here instead of in
//! somebody's shipped app.

#![cfg(feature = "safe-sandbox")]

use std::path::PathBuf;

fn corpus_dir() -> PathBuf {
    // The corpus is at the repository root rather than inside this crate: the
    // wasm host workspace is excluded from this one and reads the same files.
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/syntax-corpus")
}

fn corpus_files() -> Vec<(String, String)> {
    let dir = corpus_dir();
    let mut files: Vec<(String, String)> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .map(|entry| entry.expect("corpus dir entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "js"))
        .map(|path| {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let source = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
            (name, source)
        })
        .collect();
    files.sort();
    assert!(
        files.len() >= 5,
        "the real-program corpus lost files: found {} in {}",
        files.len(),
        dir.display()
    );
    files
}

/// Parse AND compile: the parse-shape limits exist to protect the recursive
/// walks in capture analysis and the bytecode compiler, so a corpus that only
/// parsed would not exercise the consumer the AST validator is guarding.
#[test]
fn real_application_sources_compile_under_the_hardened_profile() {
    for (name, source) in corpus_files() {
        if let Err(error) = zipp_vm::compile_to_text(&source, false) {
            panic!(
                "{name} no longer compiles under the hardened profile: {error}\n\
                 This file is reduced from a shipped application. If a parse-shape \
                 limit was just lowered, it is too low."
            );
        }
    }
}

/// The corpus is a floor, not a ceiling. These are the deepest values measured
/// across the 36 `.logic` files of the 17 real bundles on 2026-08-30, including
/// the host preamble the embedder prepends (23 / 4 / 12 of each budget is spent
/// before the guest's first token). Keeping a stated multiple of them is what
/// stops the next author of a long `else if` ladder from being the test.
///
/// A limit change that trips this assert is not necessarily wrong — but it has
/// to be argued against these numbers, and the constants' own comments carry
/// the measured ceiling it must also stay under.
#[test]
fn limits_keep_stated_headroom_over_measured_real_programs() {
    use zipp_vm::safe_syntax_limits::{
        MAX_SAFE_AST_NESTING, MAX_SAFE_SYNTAX_CHAIN, MAX_SAFE_SYNTAX_RECURSION,
    };

    const DEEPEST_REAL_RECURSION: usize = 51;
    const DEEPEST_REAL_CHAIN: usize = 21;
    const DEEPEST_REAL_AST_NESTING: usize = 41;

    // 3x is the smallest margin worth defending: the file that forced this
    // change needed 52 against a limit of 48, so anything that merely clears
    // today's corpus clears it by one commit.
    for (name, limit, needed) in [
        (
            "MAX_SAFE_SYNTAX_RECURSION",
            MAX_SAFE_SYNTAX_RECURSION,
            DEEPEST_REAL_RECURSION,
        ),
        (
            "MAX_SAFE_SYNTAX_CHAIN",
            MAX_SAFE_SYNTAX_CHAIN,
            DEEPEST_REAL_CHAIN,
        ),
        (
            "MAX_SAFE_AST_NESTING",
            MAX_SAFE_AST_NESTING,
            DEEPEST_REAL_AST_NESTING,
        ),
    ] {
        assert!(
            limit >= 3 * needed,
            "{name} is {limit}; the deepest measured real program needs {needed}"
        );
    }
}
