//! Differential test: the hand-written parser against the LALRPOP-generated
//! one it replaced (`reference::parse_tokens`, compiled only with the
//! `reference-parser` feature). Both parse the same token stream; they must
//! build equal ASTs (every node and range) or fail with the same error at the
//! same offset.
//!
//! - `repository_sources`: every Python file ZIPP ships or tests (the corpus,
//!   the bundled library, the model plugins, the benchmarks) and the raw
//!   string literals of ZIPP's Python test programs.
//! - `invalid_snippets`: syntax errors, compared message and location.
//! - `mutated_tokens`: token deletions, duplications, swaps and replacements
//!   of the repository sources, compared the same way.
//! - `cpython_lib` (ignored; `--ignored`): CPython's own `Lib/**/*.py`, from
//!   `CPYTHON_LIB` or the `py -3.13` / `python3` on `PATH`.
//! - `throughput` (ignored; run with `--release --ignored --nocapture`).
//!
//! The lexer's soft-keyword pass is compared with upstream's too (it no
//! longer uses `itertools`), on the same sources.
//!
//! Errors are compared as displayed (the text ZIPP reports) plus offset; the
//! `expected` token list of `UnrecognizedToken` is LALRPOP's table dump and
//! only its "expected an indented block" meaning is kept by the new parser.

use rustpython_parser::{
    ast, lexer::LexResult, reference, text_size::TextSize, Mode, ParseError, ParseErrorType,
};
use std::path::{Path, PathBuf};
use std::time::Instant;

#[path = "reference/soft_keywords_multipeek.rs"]
#[allow(dead_code, clippy::all)]
mod reference_soft_keywords;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn collect_py(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        if path.is_dir() {
            if name != "node_modules" && name != "__pycache__" && !name.starts_with('.') {
                collect_py(&path, out);
            }
        } else if name.ends_with(".py") {
            out.push(path);
        }
    }
}

fn read_source(path: &Path) -> Option<String> {
    let text = String::from_utf8(std::fs::read(path).ok()?).ok()?;
    Some(text.strip_prefix('\u{feff}').unwrap_or(&text).to_owned())
}

/// Raw string literals (`r"..."`, `r#"..."#`, ...) in a Rust source file.
fn raw_strings(rust: &str) -> Vec<String> {
    let bytes = rust.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'r'
            && (i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_'))
        {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j] == b'#' {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'"' {
                let hashes = j - i - 1;
                let close: String = std::iter::once('"')
                    .chain(std::iter::repeat('#').take(hashes))
                    .collect();
                if let Some(end) = rust[j + 1..].find(&close) {
                    out.push(rust[j + 1..j + 1 + end].to_owned());
                    i = j + 1 + end + close.len();
                    continue;
                }
            }
        }
        i += 1;
    }
    out
}

/// The tokens up to and including the first lexical error (after which the
/// lexer may repeat the error forever).
fn lex(source: &str, mode: Mode) -> Vec<LexResult> {
    let mut out = Vec::new();
    for item in rustpython_parser::lexer::lex(source, mode) {
        let error = item.is_err();
        out.push(item);
        if error {
            break;
        }
    }
    out
}

/// The token stream upstream's soft-keyword pass (through
/// `itertools::MultiPeek`) makes of `source`, up to its first error.
fn reference_lex(source: &str, mode: Mode) -> Vec<LexResult> {
    let lexer = rustpython_parser::lexer::Lexer::new(source.chars(), TextSize::default());
    let mut out = Vec::new();
    for item in reference_soft_keywords::SoftKeywordTransformer::new(lexer, mode) {
        let error = item.is_err();
        out.push(item);
        if error {
            break;
        }
    }
    out
}

/// `None` if the crate's lexer makes the same tokens as upstream's.
fn compare_lexers(source: &str, mode: Mode) -> Option<String> {
    let new = lex(source, mode);
    let old = reference_lex(source, mode);
    if new == old {
        return None;
    }
    let at = new
        .iter()
        .zip(&old)
        .position(|(a, b)| a != b)
        .unwrap_or(new.len().min(old.len()));
    Some(format!(
        "token streams differ at token {at}: old {:?}, new {:?}",
        old.get(at),
        new.get(at)
    ))
}

/// A comparable rendering of a parse error.
fn error_key(error: &ParseError) -> (String, TextSize) {
    let normalized = match &error.error {
        ParseErrorType::UnrecognizedToken(tok, expected) => ParseErrorType::UnrecognizedToken(
            tok.clone(),
            expected.clone().filter(|expected| expected == "Indent"),
        ),
        other => other.clone(),
    };
    (format!("{normalized:?} / {normalized}"), error.offset)
}

/// Parse `tokens` with both parsers; `None` if they agree.
fn compare_tokens(tokens: &[LexResult], mode: Mode) -> Option<String> {
    let catch = |f: &dyn Fn() -> Result<ast::Mod, ParseError>| {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
    };
    let old = catch(&|| reference::parse_tokens(tokens.iter().cloned(), mode, "<test>"));
    let new = catch(&|| rustpython_parser::parse_tokens(tokens.iter().cloned(), mode, "<test>"));
    let (old, new) = match (old, new) {
        (Ok(old), Ok(new)) => (old, new),
        (Err(_), Err(_)) => return None,
        (Err(_), Ok(_)) => return Some("only the reference parser panics".to_owned()),
        (Ok(_), Err(_)) => return Some("only the new parser panics".to_owned()),
    };
    match (old, new) {
        (Ok(old), Ok(new)) => {
            if old == new {
                None
            } else {
                Some(first_difference(&old, &new))
            }
        }
        (Err(old), Err(new)) => {
            let (old, new) = (error_key(&old), error_key(&new));
            (old != new).then(|| format!("errors differ:\n  old: {old:?}\n  new: {new:?}"))
        }
        (Ok(_), Err(new)) => Some(format!(
            "only the new parser rejects: {:?}",
            error_key(&new)
        )),
        (Err(old), Ok(_)) => Some(format!(
            "only the new parser accepts; old: {:?}",
            error_key(&old)
        )),
    }
}

fn first_difference(old: &ast::Mod, new: &ast::Mod) -> String {
    let old = format!("{old:#?}");
    let new = format!("{new:#?}");
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();
    let at = old_lines
        .iter()
        .zip(&new_lines)
        .position(|(a, b)| a != b)
        .unwrap_or(old_lines.len().min(new_lines.len()));
    let from = at.saturating_sub(12);
    format!(
        "ASTs differ at Debug line {at}:\n--- old\n{}\n--- new\n{}",
        old_lines[from..(at + 3).min(old_lines.len())].join("\n"),
        new_lines[from..(at + 3).min(new_lines.len())].join("\n"),
    )
}

fn compare_source(source: &str, mode: Mode) -> Option<String> {
    compare_tokens(&lex(source, mode), mode)
}

/// The repository's Python sources: (label, source).
fn repository_python() -> Vec<(String, String)> {
    let root = repo_root();
    let mut files = Vec::new();
    for dir in [
        "tests/python_corpus",
        "crates/zipp-vm/src/frontend/python/lib",
        "crates/zipp-wasm/model-plugins",
        "tools/python_bench",
    ] {
        collect_py(&root.join(dir), &mut files);
    }
    let mut out: Vec<(String, String)> = files
        .iter()
        .filter_map(|path| Some((path.display().to_string(), read_source(path)?)))
        .collect();
    // The Python programs embedded in ZIPP's Python tests.
    let tests = root.join("crates/zipp-vm/tests");
    let mut rust_files: Vec<PathBuf> = std::fs::read_dir(&tests)
        .map(|entries| entries.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    rust_files.sort();
    for path in rust_files {
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        if !(name.starts_with("python") || name.contains("py_")) || !name.ends_with(".rs") {
            continue;
        }
        let Some(rust) = read_source(&path) else {
            continue;
        };
        for (i, literal) in raw_strings(&rust).into_iter().enumerate() {
            out.push((format!("{name}#raw{i}"), literal));
        }
    }
    out
}

fn report(what: &str, checked: usize, failures: &[String]) {
    eprintln!("{what}: {checked} compared, {} differences", failures.len());
    if !failures.is_empty() {
        for failure in failures.iter().take(25) {
            eprintln!("\n{failure}");
        }
        panic!("{what}: {} of {checked} differ", failures.len());
    }
}

#[test]
fn repository_sources() {
    let sources = repository_python();
    assert!(sources.len() > 200, "found only {} sources", sources.len());
    let mut failures = Vec::new();
    let mut bytes = 0;
    for (label, source) in &sources {
        bytes += source.len();
        if let Some(diff) = compare_lexers(source, Mode::Module) {
            failures.push(format!("{label}: {diff}"));
        }
        if let Some(diff) = compare_source(source, Mode::Module) {
            failures.push(format!("{label}: {diff}"));
        }
    }
    eprintln!("{} bytes", bytes);
    report("repository sources", sources.len(), &failures);
}

/// Invalid (and a few borderline valid) programs.
const SNIPPETS: &[&str] = &[
    // Unexpected tokens and bad statements.
    "x = = 1\n",
    "x = \n",
    "print 'hello'\n",
    "a b\n",
    "1 +\n",
    "def\n",
    "def f\n",
    "def f(:\n",
    "def f():\npass\n",
    "def f():\n",
    "def f(): return\n  x\n",
    "if x\n  pass\n",
    "if x:\n",
    "if x:\npass\n",
    "if x: pass\nelse\n  pass\n",
    "while True print(1)\n",
    "for x in : pass\n",
    "for in y: pass\n",
    "for x y: pass\n",
    "class A(:\n  pass\n",
    "class A\n  pass\n",
    "x = (1, 2\n",
    "x = [1, 2\n",
    "x = {1: 2\n",
    "x = 1)\n",
    "x = 1]\n",
    "f(a b)\n",
    "f(a=1, b)\n",
    "f(**a, *b)\n",
    "f(a=1, a=2)\n",
    "f(a for a in b, c)\n",
    "f(x=)\n",
    "f(=1)\n",
    "f(a.b=1)\n",
    "lambda: yield\n",
    "lambda x, x: 0\n",
    "lambda *: 0\n",
    "lambda ,: 0\n",
    "def f(a, a): pass\n",
    "def f(a=1, b): pass\n",
    "def f(*): pass\n",
    "def f(*, =1): pass\n",
    "def f(*,): pass\n",
    "def f(a=1, b, *): pass\n",
    "lambda a=1, b, *: 0\n",
    "lambda *, : 0\n",
    "def f(*, **k): pass\n",
    "def f(**k, a): pass\n",
    "def f(/): pass\n",
    "def f(a, /, b, /): pass\n",
    "def f(,): pass\n",
    "def f(a, *a): pass\n",
    "def f(a=1, /, b): pass\n",
    "def f(*a, *b): pass\n",
    "def f() -> : pass\n",
    "def f(a, a) x: pass\n",
    "async x\n",
    "async def f(): await\n",
    "await await x\n",
    "x := 1\n",
    "(x := 1) = 2\n",
    "[x := 1, y]\n",
    "f(x := 1 = 2)\n",
    "(*a)\n",
    "(**a)\n",
    "print(*a for a in b)\n",
    "{*a: 1}\n",
    "{a: *b}\n",
    "{**a for a in b}\n",
    "{*a for a in b}\n",
    "{a: b, c}\n",
    "{a, b: c}\n",
    "[*a for a in b]\n",
    "(*a for a in b)\n",
    "a[1:*b]\n",
    "a[x:=1:2]\n",
    "a[]\n",
    "a.1\n",
    "a.\n",
    "del\n",
    "del a if b else c\n",
    "del not a\n",
    "import\n",
    "import a,\n",
    "import a.\n",
    "from import x\n",
    "from a import\n",
    "from a import b,\n",
    "from a import ()\n",
    "from . import (a, b,)\n",
    "from .. import *\n",
    "global\n",
    "global a,\n",
    "assert\n",
    "raise from x\n",
    "return *a, b\n",
    "yield = 1\n",
    "x = yield = 1\n",
    "x: int = 1 = 2\n",
    "x, y: int\n",
    "*a: int\n",
    "a, b += 1\n",
    "a + 1 = 2\n",
    "f() = 1\n",
    "x if y\n",
    "x if y else\n",
    "a if b if c else d else e\n",
    "a or lambda: b\n",
    "not lambda: x\n",
    "a == not b\n",
    "a not b\n",
    "a is not not b\n",
    "a < b < c\n",
    "-not a\n",
    "a ** -b ** c\n",
    "[x for x in y if a else b]\n",
    "[x for x in lambda: y]\n",
    "[x if c for x in y]\n",
    "[x async for x in y]\n",
    "[x for x, in y]\n",
    "[x for x, not in y]\n",
    "try:\n  pass\n",
    "try:\n  pass\nexcept A, B:\n  pass\n",
    "try:\n  pass\nexcept* A:\n  pass\nexcept B:\n  pass\n",
    "try:\n  pass\nexcept A:\n  pass\nexcept* B:\n  pass\n",
    "try:\n  pass\nexcept*:\n  pass\n",
    "try:\n  pass\nelse:\n  pass\n",
    "try:\n  pass\nfinally:\n  pass\nexcept:\n  pass\n",
    "with a, : pass\n",
    "with (a, b) as c: pass\n",
    "with (a as b) + c: pass\n",
    "with (a, *b) as c: pass\n",
    "with (a, *b): pass\n",
    "with (yield x): pass\n",
    "with (): pass\n",
    "with (a as b, c as d,): pass\n",
    "with (a, b), c: pass\n",
    "with (a) c: pass\n",
    "with (a, f(x=1, x=2)): pass\n",
    "with (a as b c): pass\n",
    "with (a := b): pass\n",
    "with (x for x in y): pass\n",
    "with a as b, (c as d): pass\n",
    "@\ndef f(): pass\n",
    "@d\nx = 1\n",
    "@d\nasync with x: pass\n",
    "@d def f(): pass\n",
    "class A(x=1, x=2): pass\n",
    "class A[]: pass\n",
    "def f[T,](): pass\n",
    "def f[*T: int](): pass\n",
    "type X = \n",
    "type X[T] = list[T]\n",
    "type = 1\n",
    "type X\n",
    "match x:\n  case 1:\n    pass\n",
    "match x:\ncase 1: pass\n",
    "match x:\n  pass\n",
    "match x:\n  case 1 + 2 + 3: pass\n",
    "match x:\n  case -x: pass\n",
    "match x:\n  case 1 - x: pass\n",
    "match x:\n  case a as _: pass\n",
    "match x:\n  case {a: 1}: pass\n",
    "match x:\n  case {**r, 'a': 1}: pass\n",
    "match x:\n  case {, }: pass\n",
    "match x:\n  case C(a=1, b): pass\n",
    "match x:\n  case C(a=1, 2): pass\n",
    "match x:\n  case (a b): pass\n",
    "match x:\n  case [,]: pass\n",
    "match x:\n  case *a | b: pass\n",
    "match x, :\n  case a: pass\n",
    "match x,, :\n  case a: pass\n",
    "match x, y:\n  case a, b if a: pass\n  case _: pass\n",
    "match x:\n  case a.b.c(d, e=f): pass\n  case {1: a, 'b': c, **d}: pass\n  case [a, *_]: pass\n",
    "match x:\n  case (a): pass\n  case (): pass\n  case (a,): pass\n  case (a, b,): pass\n",
    "match x:\n  case None | True | False | -1 | 1.5 | 2j | -1 + 2j | 'a' 'b': pass\n",
    "match x:\n  case b'a' 'b': pass\n",
    "match x:\n  case b'a' 'b' x: pass\n",
    "match x:\n  case {b'a' 'b': 1}: pass\n",
    "match x:\n  case {b'a' 'b' 1}: pass\n",
    "match x:\n  case a as _ b: pass\n",
    "match x:\n  case (a as _): pass\n",
    "with b'a' 'b': pass\n",
    "with b'a' 'b'\n",
    "with (*a)\n",
    "with (**a): pass\n",
    "with (*a) as b: pass\n",
    "f'{a!x}' except\n",
    "(*a) as b\n",
    "(**a)\n",
    "def f(a, a) x: pass\n",
    "def f(a, a) -> x: pass\n",
    "lambda a, a: 0 except\n",
    "  x = 1\n",
    "x = 1\n    y = 2\n",
    "if x:\n    a\n  b\n",
    "if x:\n\ta\n        b\n",
    "x = 'unterminated\n",
    "x = \"\"\"never closed\n",
    "x = 1 $ 2\n",
    "x = 0x\n",
    "x = 1_\n",
    "f'{'\n",
    "f'{a!x}'\n",
    "f'{*a}'\n",
    "f'{a b}'\n",
    "f'{}'\n",
    "f'{a:{b:{c}}}'\n",
    "f'{x=}' f'{y!r:>10}'\n",
    "f'{lambda: 1}'\n",
    "f'{yield}'\n",
    "f'{(x := 1)}'\n",
    "'\\N{NOT A REAL NAME}'\n",
    "x = (\n",
    "x = [1,\n  2,\n",
    "(",
    ")",
    "@",
    "",
    "\n\n\n",
    "pass;;\n",
    "pass; pass;\n",
    "a = b = c = d\n",
    "x = *a, *b\n",
    "for *a, b in c: pass\n",
    "print(a, *b, c=1, **d, e=2)\n",
    "def f(a, /, b=1, *c, d, e=2, **f): pass\n",
    "def f(**): pass\n",
    "def f(a, **): pass\n",
    "def f(*, a): pass\n",
    "lambda a, /, b=1, *c, d, **e: 0\n",
    "x[1:2, ::3, ...]\n",
    "x[a, b:c, *d]\n",
    "x[1,]\n",
    "(yield)\n",
    "(a, *b, c)\n",
    "a if b else lambda: c if d else e\n",
    "not not a == b and c or d\n",
    "a @ b @= c\n",
    "global a, b\nnonlocal c\n",
    "if a:\n  pass\nelif b:\n  pass\nelif c:\n  pass\nelse:\n  pass\n",
    "async def f():\n  async with a as b, c: pass\n  async for x in y: pass\n  await z\n",
    "class A[T: int, *Ts, **P](B, metaclass=M): pass\n",
    "x = (a for b in c for d in e if f if g)\n",
    "print(f'{a}' 'b' f'{c!r:>{d}}')\n",
    "b'a' b'b'\n",
    "b'a' 'b'\n",
    "u'a' 'b'\n",
    "x = 1 if True else 2, 3\n",
    "match = 1\ncase = 2\ntype = 3\nprint(match, case, type)\n",
    "match(x)\n",
    "match x:\n  case _:\n    pass\nmatch = 3\n",
    "type X[T: (int, str)] = dict[str, T]\n",
    "lambda: (yield)\n",
    "x = {**a, 'b': 1, **c}\n",
    "x = {1, 2, *a}\n",
    "(a := 1, b := 2)\n",
    "a[b := 1]\n",
    "f(a := 1)\n",
    "while (x := f()): pass\n",
    "if (a := 1) and (b := 2): pass\n",
    "assert x, 'msg'\n",
    "raise E from None\n",
    "del a, b[1], c.d, (e, f), [g]\n",
    "with a as (b, c), d as [e]: pass\n",
    "try:\n  pass\nexcept* (A, B) as e:\n  pass\nelse:\n  pass\nfinally:\n  pass\n",
    "for x in 1, 2, 3: pass\n",
    "return\n",
    "x = yield from y\n",
    "x = yield a, b\n",
    "def f(): yield\n",
    "a = 1; b = 2\n",
    "x = (\n  1 +\n  2\n)\n",
    "if True:\n  pass\n\n\n  # comment\n  pass\n",
    "x = \\\n  1\n",
];

#[test]
fn invalid_snippets() {
    let mut failures = Vec::new();
    let verbose = std::env::var_os("PARSER_DIFF_VERBOSE").is_some();
    for snippet in SNIPPETS {
        if verbose {
            eprintln!("snippet {snippet:?}");
        }
        for (mode, name) in [(Mode::Module, "Module"), (Mode::Interactive, "Interactive")] {
            if let Some(diff) = compare_lexers(snippet, mode) {
                failures.push(format!("{snippet:?} ({name}): {diff}"));
            }
            if let Some(diff) = compare_source(snippet, mode) {
                failures.push(format!("{snippet:?} ({name}): {diff}"));
            }
        }
        // As an expression, where that is not trivially the same error.
        if let Some(diff) = compare_source(snippet.trim_end(), Mode::Expression) {
            failures.push(format!("{snippet:?} (Expression): {diff}"));
        }
    }
    report("snippets", SNIPPETS.len() * 3, &failures);
}

/// A small deterministic generator.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// Every kind of token (a few of them with more than one value).
fn replacement_tokens() -> Vec<rustpython_parser::Tok> {
    use rustpython_parser::{StringKind, Tok};
    let string = |value: &str, kind| Tok::String {
        value: value.into(),
        kind,
        triple_quoted: false,
    };
    vec![
        Tok::Name { name: "x".into() },
        Tok::Name { name: "_".into() },
        Tok::Int { value: 1u8.into() },
        Tok::Float { value: 1.5 },
        Tok::Complex {
            real: 0.0,
            imag: 2.0,
        },
        string("s", StringKind::String),
        string("s", StringKind::Unicode),
        string("s", StringKind::RawString),
        string("b", StringKind::Bytes),
        string("b", StringKind::RawBytes),
        string("{x}", StringKind::FString),
        string("{x!r:>{y}}", StringKind::RawFString),
        string("{", StringKind::FString),
        string("{*a}", StringKind::FString),
        string("{lambda: 1}", StringKind::FString),
        Tok::Newline,
        Tok::Indent,
        Tok::Dedent,
        Tok::EndOfFile,
        Tok::StartModule,
        Tok::StartInteractive,
        Tok::StartExpression,
        Tok::Lpar,
        Tok::Rpar,
        Tok::Lsqb,
        Tok::Rsqb,
        Tok::Colon,
        Tok::Comma,
        Tok::Semi,
        Tok::Plus,
        Tok::Minus,
        Tok::Star,
        Tok::Slash,
        Tok::Vbar,
        Tok::Amper,
        Tok::Less,
        Tok::Greater,
        Tok::Equal,
        Tok::Dot,
        Tok::Percent,
        Tok::Lbrace,
        Tok::Rbrace,
        Tok::EqEqual,
        Tok::NotEqual,
        Tok::LessEqual,
        Tok::GreaterEqual,
        Tok::Tilde,
        Tok::CircumFlex,
        Tok::LeftShift,
        Tok::RightShift,
        Tok::DoubleStar,
        Tok::DoubleStarEqual,
        Tok::PlusEqual,
        Tok::MinusEqual,
        Tok::StarEqual,
        Tok::SlashEqual,
        Tok::PercentEqual,
        Tok::AmperEqual,
        Tok::VbarEqual,
        Tok::CircumflexEqual,
        Tok::LeftShiftEqual,
        Tok::RightShiftEqual,
        Tok::DoubleSlash,
        Tok::DoubleSlashEqual,
        Tok::ColonEqual,
        Tok::At,
        Tok::AtEqual,
        Tok::Rarrow,
        Tok::Ellipsis,
        Tok::False,
        Tok::None,
        Tok::True,
        Tok::And,
        Tok::As,
        Tok::Assert,
        Tok::Async,
        Tok::Await,
        Tok::Break,
        Tok::Class,
        Tok::Continue,
        Tok::Def,
        Tok::Del,
        Tok::Elif,
        Tok::Else,
        Tok::Except,
        Tok::Finally,
        Tok::For,
        Tok::From,
        Tok::Global,
        Tok::If,
        Tok::Import,
        Tok::In,
        Tok::Is,
        Tok::Lambda,
        Tok::Nonlocal,
        Tok::Not,
        Tok::Or,
        Tok::Pass,
        Tok::Raise,
        Tok::Return,
        Tok::Try,
        Tok::While,
        Tok::Match,
        Tok::Type,
        Tok::Case,
        Tok::With,
        Tok::Yield,
    ]
}

/// Mutate one token of `tokens` (kept up to its first error).
fn mutate(tokens: &[LexResult], rng: &mut Rng, pool: &[rustpython_parser::Tok]) -> Vec<LexResult> {
    let mut out: Vec<LexResult> = tokens.to_vec();
    if out.is_empty() {
        return out;
    }
    let i = rng.below(out.len());
    match rng.below(4) {
        0 => {
            let _ = out.remove(i);
        }
        1 => {
            let token = out[i].clone();
            out.insert(i, token);
        }
        // Swap the tokens but keep the positions in order.
        2 if i + 1 < out.len() => {
            if let (Ok(a), Ok(b)) = (out[i].clone(), out[i + 1].clone()) {
                out[i] = Ok((b.0, a.1));
                out[i + 1] = Ok((a.0, b.1));
            }
        }
        _ => {
            if let Ok((_, range)) = &out[i] {
                let range = *range;
                out[i] = Ok((pool[rng.below(pool.len())].clone(), range));
            }
        }
    }
    out
}

#[test]
fn mutated_tokens() {
    let pool = replacement_tokens();
    let seed = std::env::var("PARSER_FUZZ_SEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0x9E37_79B9_7F4A_7C15u64);
    let scale: usize = std::env::var("PARSER_FUZZ_SCALE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    let mut rng = Rng(seed.wrapping_mul(2).wrapping_add(1));
    let mut failures = Vec::new();
    let mut checked = 0;
    let mut sources = repository_python();
    for snippet in SNIPPETS {
        sources.push((format!("snippet {snippet:?}"), snippet.to_string()));
    }
    let budget: usize = std::env::var("PARSER_FUZZ_TOKENS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(40_000_000);
    let mut spent = 0usize;
    for (label, source) in &sources {
        let tokens = lex(source, Mode::Module);
        // Small files get many mutations; big ones a few.
        let rounds = (20_000 / tokens.len().max(1)).clamp(2, 40) * scale;
        for round in 0..rounds {
            if spent > budget {
                break;
            }
            // One to three mutations.
            let mut mutated = mutate(&tokens, &mut rng, &pool);
            for _ in 0..rng.below(3) {
                mutated = mutate(&mutated, &mut rng, &pool);
            }
            spent += mutated.len();
            checked += 1;
            if let Some(diff) = compare_tokens(&mutated, Mode::Module) {
                let shown = if mutated.len() <= 40 {
                    let toks: Vec<String> = mutated
                        .iter()
                        .map(|t| match t {
                            Ok((tok, range)) => format!("{tok}@{}", u32::from(range.start())),
                            Err(e) => format!("<{:?}>", e.error),
                        })
                        .collect();
                    format!(
                        "
  tokens: {}",
                        toks.join(" ")
                    )
                } else {
                    String::new()
                };
                failures.push(format!("{label} (mutation {round}): {diff}{shown}"));
            }
        }
    }
    report("mutations", checked, &failures);
}

fn cpython_lib() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CPYTHON_LIB") {
        return Some(PathBuf::from(dir));
    }
    for (cmd, args) in [
        ("py", &["-3.13"][..]),
        ("python3", &[][..]),
        ("python", &[][..]),
    ] {
        let output = std::process::Command::new(cmd)
            .args(args)
            .args([
                "-c",
                "import os, sysconfig; print(sysconfig.get_paths()['stdlib'])",
            ])
            .output();
        if let Ok(output) = output {
            if output.status.success() {
                let dir = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
                if dir.is_dir() {
                    return Some(dir);
                }
            }
        }
    }
    None
}

#[test]
#[ignore = "needs a CPython installation; run with --ignored"]
fn cpython_lib_sources() {
    let dir = cpython_lib().expect("no CPython Lib found (set CPYTHON_LIB)");
    let mut files = Vec::new();
    collect_py(&dir, &mut files);
    let mut failures = Vec::new();
    let (mut accepted, mut rejected, mut bytes, mut checked) = (0, 0, 0, 0);
    for path in &files {
        let Some(source) = read_source(path) else {
            continue;
        };
        bytes += source.len();
        checked += 1;
        if let Some(diff) = compare_lexers(&source, Mode::Module) {
            failures.push(format!("{}: {diff}", path.display()));
        }
        let tokens = lex(&source, Mode::Module);
        match compare_tokens(&tokens, Mode::Module) {
            Some(diff) => failures.push(format!("{}: {diff}", path.display())),
            None => match rustpython_parser::parse_tokens(tokens, Mode::Module, "<test>") {
                Ok(_) => accepted += 1,
                Err(error) => {
                    rejected += 1;
                    eprintln!(
                        "  both reject {}: {} at {:?}",
                        path.display(),
                        error.error,
                        error.offset
                    );
                }
            },
        }
    }
    eprintln!(
        "CPython Lib {}: {checked} files, {bytes} bytes; both accept {accepted}, both reject {rejected}",
        dir.display()
    );
    report("CPython Lib", checked, &failures);
}

#[test]
#[ignore = "benchmark; run with --release --ignored --nocapture"]
fn throughput() {
    let root = repo_root();
    let mut files = Vec::new();
    collect_py(
        &root.join("crates/zipp-vm/src/frontend/python/lib"),
        &mut files,
    );
    let sources: Vec<String> = files.iter().filter_map(|path| read_source(path)).collect();
    let bytes: usize = sources.iter().map(String::len).sum();
    let token_streams: Vec<Vec<LexResult>> = sources
        .iter()
        .map(|source| lex(source, Mode::Module))
        .collect();
    let rounds = std::env::var("PARSER_BENCH_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(50);
    let megabytes = (bytes * rounds) as f64 / 1e6;
    let run = |name: &str, f: &mut dyn FnMut()| {
        f(); // warm up
        let start = Instant::now();
        for _ in 0..rounds {
            f();
        }
        let seconds = start.elapsed().as_secs_f64();
        eprintln!(
            "{name:<34} {:>8.1} ms/round {:>8.1} MB/s",
            seconds * 1e3 / rounds as f64,
            megabytes / seconds
        );
    };
    eprintln!("{} files, {bytes} bytes, {rounds} rounds", sources.len());
    run("lex only", &mut || {
        for source in &sources {
            std::hint::black_box(lex(source, Mode::Module));
        }
    });
    run("lex without the soft-keyword pass", &mut || {
        for source in &sources {
            let lexer = rustpython_parser::lexer::Lexer::new(source.chars(), TextSize::default());
            std::hint::black_box(lexer.count());
        }
    });
    run("lex with the soft-keyword pass", &mut || {
        for source in &sources {
            std::hint::black_box(rustpython_parser::lexer::lex(source, Mode::Module).count());
        }
    });
    run("clone pre-lexed tokens only", &mut || {
        for tokens in &token_streams {
            std::hint::black_box(tokens.to_vec());
        }
    });
    run("parse pre-lexed tokens, LALRPOP", &mut || {
        for tokens in &token_streams {
            std::hint::black_box(
                reference::parse_tokens(tokens.iter().cloned(), Mode::Module, "").unwrap(),
            );
        }
    });
    run("parse pre-lexed tokens, descent", &mut || {
        for tokens in &token_streams {
            std::hint::black_box(
                rustpython_parser::parse_tokens(tokens.iter().cloned(), Mode::Module, "").unwrap(),
            );
        }
    });
    run("lex + parse, LALRPOP", &mut || {
        for source in &sources {
            std::hint::black_box(
                reference::parse_tokens(
                    rustpython_parser::lexer::lex(source, Mode::Module),
                    Mode::Module,
                    "",
                )
                .unwrap(),
            );
        }
    });
    run("lex + parse, descent", &mut || {
        for source in &sources {
            std::hint::black_box(
                rustpython_parser::parse_tokens(
                    rustpython_parser::lexer::lex(source, Mode::Module),
                    Mode::Module,
                    "",
                )
                .unwrap(),
            );
        }
    });
}

/// Long right-nested and postfix chains (the new parser reads them
/// iteratively; the old one kept its stack on the heap). Compared on a
/// large stack, since comparing and dropping the trees recurse.
#[test]
fn long_chains() {
    std::thread::Builder::new()
        .stack_size(1 << 30)
        .spawn(|| {
            let mut failures = Vec::new();
            let cases = [
                ("x = ", "-", "1"),
                ("x = ", "not ", "1"),
                ("x = a", ".b", ""),
                ("x = f", "(1)", ""),
                ("x = ", "lambda a, b=1: ", "1"),
                ("x = ", "a if b else ", "c"),
                ("x = ", "lambda: a if b else ", "c"),
                ("x = ", "a ** -~", "1"),
                ("x = ", "await a ** ", "1"),
                ("x = 1", " - 1", ""),
                ("x = 1", " < 1", ""),
                ("x = 1", " and 1 or 1", ""),
                ("x = ", "lambda a, a: ", "1"),
                ("x = ", "lambda: ", ")"),
            ];
            for (prefix, unit, suffix) in cases {
                let source = format!("{prefix}{}{suffix}\n", unit.repeat(20_000));
                if let Some(diff) = compare_source(&source, Mode::Module) {
                    failures.push(format!("{unit:?} x 20000: {diff}"));
                }
            }
            report("long chains", cases.len(), &failures);
        })
        .unwrap()
        .join()
        .unwrap();
}

/// Every construct whose grammar action can fail, in several contexts,
/// followed by every kind of token: the new parser reports the semantic
/// error exactly when the LR parser reduced the rule (the next token is in
/// its lookahead set), and the syntax error at that token otherwise.
#[test]
fn reduction_lookaheads() {
    let constructs = [
        "f'{'",
        "f'{*a}'",
        "b'a' 'b'",
        "(*a)",
        "(**a)",
        "lambda a, a: 0",
        "lambda *: 0",
        "f(a=1, a=2)",
        "f(**a, b)",
        "'s'",
        "(a)",
        "def f(a, a)",
        "def f[T](a, *)",
        "class C(a=1, a=1)",
    ];
    let contexts: &[(&str, &str)] = &[
        ("", ""),
        ("x = ", ""),
        ("x = y + ", ""),
        ("(", ""),
        ("[", ""),
        ("{", ""),
        ("{a: ", ""),
        ("f(", ""),
        ("a[", ""),
        ("with ", ""),
        ("with a, ", ""),
        ("with (", ""),
        ("for x in ", ""),
        ("[x for x in ", ""),
        ("lambda: ", ""),
        ("x if ", ""),
        ("return ", ""),
        ("del ", ""),
        ("assert ", ""),
        ("async def f():\n  await ", ""),
        ("match x:\n  case ", ""),
        ("match x:\n  case {", ""),
        ("match x:\n  case C(", ""),
        ("match x:\n  case [", ""),
        ("match x:\n  case (", ""),
    ];
    let pattern_constructs = [
        "b'a' 'b'",
        "f'{x}'",
        "a as _",
        "{b'a' 'b': 1}",
        "C(b'a' 'b')",
    ];
    let mut failures = Vec::new();
    let mut checked = 0;
    let followers = replacement_tokens();
    for (prefix, _) in contexts {
        let in_pattern = prefix.contains("case");
        let list: &[&str] = if in_pattern {
            &pattern_constructs
        } else {
            &constructs
        };
        for construct in list {
            let source = format!("{prefix}{construct}");
            let mut tokens = lex(&source, Mode::Module);
            // Drop what the lexer adds at the end (the newline, closing
            // dedents), keeping any lexical error.
            while matches!(
                tokens.last(),
                Some(Ok((
                    rustpython_parser::Tok::Newline | rustpython_parser::Tok::Dedent,
                    _
                )))
            ) {
                tokens.pop();
            }
            if tokens.iter().any(Result::is_err) {
                continue;
            }
            let end = source.len() as u32 + 1;
            for follower in &followers {
                for tail in [false, true] {
                    let mut stream = tokens.clone();
                    let at =
                        rustpython_parser::text_size::TextRange::new(end.into(), (end + 1).into());
                    stream.push(Ok((follower.clone(), at)));
                    if tail {
                        let after =
                            rustpython_parser::text_size::TextRange::empty((end + 2).into());
                        stream.push(Ok((rustpython_parser::Tok::Newline, after)));
                    }
                    checked += 1;
                    if let Some(diff) = compare_tokens(&stream, Mode::Module) {
                        failures.push(format!(
                            "{source:?} + {follower}{}: {diff}",
                            if tail { " + Newline" } else { "" }
                        ));
                    }
                }
            }
        }
    }
    report("reduction lookaheads", checked, &failures);
}
