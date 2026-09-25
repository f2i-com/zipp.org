//! Proof that the comment-stripped sources `build.rs` embeds (see
//! build/minify.rs) are the same programs as the files they came from:
//!
//!   * every Python library module lexes to the same tokens, each at the
//!     same line and column (the lexer emits no comment or
//!     non-logical-newline tokens);
//!   * every runtime JavaScript file compiles to the same bytecode, line
//!     table and constants; only `FuncProto::source` (the text
//!     `Function.prototype.toString` returns, which no Python program can
//!     reach) loses its comments.
//!
//!   cargo test -p zipp-vm --features python --lib minify_check
use zipp_pyparse::{lexer, token::T, Limits, Mode};
use std::path::{Path, PathBuf};

fn pairs(dir: &str, ext: &str) -> Vec<(PathBuf, String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/frontend/python");
    let out = Path::new(env!("OUT_DIR")).join("pysrc");
    let mut found = Vec::new();
    let mut stack = vec![PathBuf::from(dir)];
    while let Some(rel) = stack.pop() {
        for entry in std::fs::read_dir(root.join(&rel)).unwrap() {
            let path = entry.unwrap().path();
            let rel_path = rel.join(path.file_name().unwrap());
            if path.is_dir() {
                stack.push(rel_path);
            } else if path.extension().and_then(|e| e.to_str()) == Some(ext) {
                let original = std::fs::read_to_string(&path).unwrap();
                let stripped = std::fs::read_to_string(out.join(&rel_path)).unwrap();
                found.push((rel_path, original, stripped));
            }
        }
    }
    assert!(!found.is_empty(), "no .{ext} files under {dir}");
    found
}

fn python_tokens(source: &str) -> Vec<(String, usize, usize)> {
    let lines = super::emitter::line_starts(source);
    let lexed = lexer::lex(source, Mode::Module, 0, Limits::NONE);
    assert!(lexed.errors.is_empty(), "bundled module lexes");
    lexed
        .tokens
        .iter()
        .map(|tok| {
            let at = tok.start;
            let line = lines.partition_point(|&s| s <= at).max(1);
            let col = (at - lines[line - 1]) as usize;
            // A NEWLINE token sits where the line's text ends, which a cut
            // trailing comment moves; it has no position the emitter uses.
            let (col, text) = if tok.kind == T::Newline {
                (0, "")
            } else {
                (col, &source[tok.start as usize..tok.end as usize])
            };
            (format!("{:?} {text}", tok.kind), line, col)
        })
        .collect()
}

#[test]
fn minify_check_python_library_tokens_unchanged() {
    for (path, original, stripped) in pairs("lib", "py") {
        assert_eq!(original.lines().count(), stripped.lines().count(), "{}", path.display());
        assert!(
            python_tokens(&original) == python_tokens(&stripped),
            "{}: stripped module lexes differently",
            path.display()
        );
    }
}

#[test]
fn minify_check_runtime_javascript_bytecode_unchanged() {
    for (path, original, stripped) in pairs("runtime", "js") {
        assert_eq!(original.lines().count(), stripped.lines().count(), "{}", path.display());
        let compile = |src: &str| {
            let mut program = crate::compile_only(src, false)
                .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            for f in &mut program.functions {
                f.source.clear();
            }
            format!("{program:?}")
        };
        assert!(
            compile(&original) == compile(&stripped),
            "{}: stripped runtime compiles differently",
            path.display()
        );
    }
}
