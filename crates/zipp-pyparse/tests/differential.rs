//! Differential test: ZIPP's front end (lexer, parser, converter) against
//! the RustPython 0.4 parser it replaces (`crates/rustpython-parser-fork`).
//! Both parse the same source; they must build equal ASTs (every node and
//! range) or fail with the same message at the same offset, except where
//! the new front end accepts Python 3.12/3.13 syntax the old one rejected
//! (PEP 701 f-strings, PEP 696 defaults): those are checked against
//! CPython's own parser (`cpython.rs`).
//!
//! - `repository_sources`: every Python file ZIPP ships or tests and the raw
//!   string literals of ZIPP's Python test programs.
//! - `invalid_snippets`: syntax errors, compared message and location.
//! - `mutated_sources`: token deletions, duplications, swaps and
//!   replacements of those sources.
//! - `long_chains`: 20,000-link chains of every iterative form.
//! - `cpython_lib_sources` (ignored; `CPYTHON_LIB` or `py -3.13`/`python3`).
//! - `throughput` (ignored; `--release --ignored --nocapture`).

#[path = "common/snippets.rs"]
mod snippets;

use rustpython_parser::{ast, Mode};
use std::path::{Path, PathBuf};
use std::time::Instant;
use zipp_pyparse::Mode as NewMode;

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
                    .chain(std::iter::repeat_n('#', hashes))
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

fn modes(mode: Mode) -> NewMode {
    match mode {
        Mode::Module => NewMode::Module,
        Mode::Interactive => NewMode::Interactive,
        Mode::Expression => NewMode::Expression,
    }
}

/// How the two front ends compare on one source.
#[derive(Debug, Default)]
pub struct Outcome {
    /// An unexplained difference.
    differ: Option<String>,
    /// The old parser rejected what the new one accepts (after the
    /// neutralizations below): CPython must accept it, with our tree.
    new_accepts: Option<String>,
    /// Both accept with different trees: CPython must build ours.
    tree_differs: Option<String>,
    /// F-string literals PEP 701 delimits differently from the classic
    /// lexer (nested quotes, multi-line fields), replaced by plain strings
    /// of the same length before comparing.
    retokenized: usize,
    /// F-string literals the old parser rejected and PEP 701 accepts,
    /// likewise replaced: CPython must accept each.
    pep701_valid: Vec<String>,
    /// F-string literals the new front end rejects where the old parser was
    /// lenient (format-spec escapes kept raw, `{=x}`): CPython must reject
    /// each.
    spec_escapes: Vec<String>,
    /// A PEP 696 type parameter default the old parser stopped at.
    pep696: bool,
    /// The retokenized literals themselves (CPython must build our tree for
    /// each it accepts).
    retokenized_literals: Vec<String>,
    /// For a new-only acceptance: the program with its f-strings
    /// neutralized, which both front ends accept alike. Where CPython
    /// rejects that too, the rejection is a leniency both share (their
    /// grammar is RustPython's; ZIPP's compiler checks more afterwards).
    agreed: Option<String>,
}

/// A program only the new front end accepts: label, source, and the
/// neutralized program both accept (see `Outcome::agreed`).
type Accepted = (String, String, Option<String>);

/// Compare the front ends on `source`, replacing (with plain strings of the
/// same length) the f-strings they are known to read differently until they
/// agree or disagree for another reason.
fn compare(source: &str, mode: Mode) -> Outcome {
    let mut outcome = Outcome::default();
    // Only the new front end accepts the program as written: CPython must
    // build the same tree (checked by the caller).
    let original = source;
    let mut source = source.to_owned();
    let literals = fstrings(&source);
    let retokenized: Vec<_> = literals
        .iter()
        .filter(|l| l.2)
        .map(|l| (l.0, l.1))
        .collect();
    if !retokenized.is_empty() {
        if let Once::OldRejects(old, None) = compare_once(original, mode) {
            outcome.new_accepts = Some(format!("old rejected: {:?} at {}", old.0, old.1));
        }
        outcome.retokenized = retokenized.len();
        for (start, end) in retokenized {
            outcome
                .retokenized_literals
                .push(source[start as usize..end as usize].to_owned());
            neutralize(&mut source, start, end);
        }
    }
    for _ in 0..16 {
        match compare_once(&source, mode) {
            Once::Same => {
                if outcome.new_accepts.is_some()
                    && source != original
                    && rustpython_parser::parse(&source, mode, "<test>").is_ok()
                {
                    outcome.agreed = Some(source);
                }
                return outcome;
            }
            Once::TreeDiffers(diff) => {
                outcome.tree_differs = Some(diff);
                return outcome;
            }
            Once::OldRejects(old, new_err) => {
                // An f-string the old parser rejected where the new one
                // accepted it (the new error, if any, is elsewhere).
                if let Some((start, end)) = fstring_containing(&source, old.1) {
                    // The new front end accepts the literal (as Python 3.12
                    // does; CPython checks it below).
                    let literal = &source[start as usize..end as usize];
                    let valid = zipp_pyparse::parse(
                        literal,
                        NewMode::Expression,
                        0,
                        zipp_pyparse::Limits::NONE,
                    )
                    .is_ok();
                    if valid {
                        if outcome.pep701_valid.is_empty() && outcome.new_accepts.is_none() {
                            if let Once::OldRejects(old, None) = compare_once(original, mode) {
                                outcome.new_accepts =
                                    Some(format!("old rejected: {:?} at {}", old.0, old.1));
                            }
                        }
                        outcome
                            .pep701_valid
                            .push(source[start as usize..end as usize].to_owned());
                        neutralize(&mut source, start, end);
                        continue;
                    }
                }
                match new_err {
                    None => {
                        outcome.new_accepts =
                            Some(format!("old rejected: {:?} at {}", old.0, old.1));
                        return outcome;
                    }
                    Some(new) => {
                        if old.0 == "invalid syntax. Got unexpected token '='"
                            && (new.1 > old.1 || new.0.starts_with("type parameter defaults"))
                            && type_param_default(&source, old.1)
                        {
                            outcome.pep696 = true;
                            return outcome;
                        }
                        if let Some(literal) = lenient(&source, new.1, Some(old.1)) {
                            outcome.spec_escapes.push(literal);
                            return outcome;
                        }
                        outcome.differ = Some(format!(
                            "errors differ:\n  old: {:?} at {}\n  new: {:?} at {}",
                            old.0, old.1, new.0, new.1
                        ));
                        return outcome;
                    }
                }
            }
            Once::NewRejects(new) => {
                if let Some(literal) = lenient(&source, new.1, None) {
                    outcome.spec_escapes.push(literal);
                    return outcome;
                }
                outcome.differ = Some(format!(
                    "only the new front end rejects: {:?} at {}",
                    new.0, new.1
                ));
                return outcome;
            }
            Once::Panic(what) => {
                outcome.differ = Some(what);
                return outcome;
            }
        }
    }
    outcome.differ = Some("too many f-string replacements".into());
    outcome
}

enum Once {
    Same,
    TreeDiffers(String),
    /// The old error, and the new one if it rejects too (different).
    OldRejects((String, u32), Option<(String, u32)>),
    NewRejects((String, u32)),
    Panic(String),
}

fn compare_once(source: &str, mode: Mode) -> Once {
    let old = std::panic::catch_unwind(|| rustpython_parser::parse(source, mode, "<test>"));
    let new = std::panic::catch_unwind(|| zipp_pyparse::rustpython::parse(source, modes(mode), 0));
    let (old, new) = match (old, new) {
        (Ok(old), Ok(new)) => (old, new),
        (Err(_), Err(_)) => return Once::Same,
        (Err(_), Ok(_)) => return Once::Panic("only the old parser panics".into()),
        (Ok(_), Err(_)) => return Once::Panic("only the new front end panics".into()),
    };
    match (old, new) {
        (Ok(old), Ok(new)) => {
            if old == new {
                Once::Same
            } else {
                Once::TreeDiffers(first_difference(&old, &new))
            }
        }
        (Err(old), Err(new)) => {
            let old_key = (old.error.to_string(), u32::from(old.offset));
            let new_key = (new.message.clone(), new.offset);
            if old_key == new_key {
                Once::Same
            } else {
                Once::OldRejects(old_key, Some(new_key))
            }
        }
        (Ok(_), Err(new)) => Once::NewRejects((new.message.clone(), new.offset)),
        (Err(old), Ok(_)) => Once::OldRejects((old.error.to_string(), u32::from(old.offset)), None),
    }
}

/// The f-string literals PEP 701's rules delimit (start, end), and whether
/// the classic lexer delimited them differently.
fn fstrings(source: &str) -> Vec<(u32, u32, bool)> {
    use zipp_pyparse::token::T;
    let lexed = zipp_pyparse::lexer::lex(source, NewMode::Module, 0, zipp_pyparse::Limits::NONE);
    let tokens = &lexed.tokens;
    let mut out = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let tok = tokens[i];
        if tok.kind == T::FStringStart {
            let end = tokens[tok.data as usize].end;
            let classic = zipp_pyparse::lexer::classic_literal_end(source, tok.start as usize);
            out.push((tok.start, end, classic != Some(end as usize)));
            i = tok.data as usize;
        }
        i += 1;
    }
    out
}

fn fstring_containing(source: &str, offset: u32) -> Option<(u32, u32)> {
    fstrings(source)
        .into_iter()
        .find(|l| l.0 <= offset && offset <= l.1)
        .map(|l| (l.0, l.1))
}

/// Replace `source[start..end]` with a plain string literal of the same
/// length (keeping its line breaks).
fn neutralize(source: &mut String, start: u32, end: u32) {
    let (start, end) = (start as usize, end as usize);
    let n = end - start;
    let bytes = &source.as_bytes()[start..end];
    let newline = |b: &u8| matches!(*b, b'\n' | b'\r');
    let mut out: Vec<u8> = bytes
        .iter()
        .map(|b| if newline(b) { *b } else { b' ' })
        .collect();
    if !bytes.iter().any(newline) {
        out[0] = b'"';
        out[n - 1] = b'"';
    } else if n >= 7 && !bytes[..3].iter().any(newline) && !bytes[n - 3..].iter().any(newline) {
        out[..3].copy_from_slice(b"\"\"\"");
        out[n - 3..].copy_from_slice(b"\"\"\"");
    } else {
        out[0] = b'(';
        out[n - 1] = b')';
    }
    source.replace_range(start..end, std::str::from_utf8(&out).unwrap());
}

/// The f-string literal the new front end rejects at `offset` where the
/// old parser accepted it (its error, if any, is after the literal): an
/// escape in a format spec (kept raw by the old parser), or text its
/// classic f-string parser let through (`{=x}`). CPython must reject it.
fn lenient(source: &str, offset: u32, old: Option<u32>) -> Option<String> {
    let (start, end) = fstring_containing(source, offset)?;
    if offset == end || old.is_some_and(|old| old <= end) {
        return None;
    }
    Some(source[start as usize..end as usize].to_owned())
}

/// Whether the `=` at `offset` follows a type parameter (in the `[...]`
/// after `def name`, `class name` or `type name`).
fn type_param_default(source: &str, offset: u32) -> bool {
    use zipp_pyparse::token::T;
    let lexed = zipp_pyparse::lexer::lex(source, NewMode::Module, 0, zipp_pyparse::Limits::NONE);
    let tokens = &lexed.tokens;
    let Some(eq) = tokens
        .iter()
        .position(|t| t.start == offset && t.kind == T::Equal)
    else {
        return false;
    };
    let text = |t: &zipp_pyparse::token::Token| &source[t.start as usize..t.end as usize];
    let mut depth = 0i32;
    let mut i = eq;
    while i > 0 {
        i -= 1;
        match tokens[i].kind {
            T::Rsqb | T::Rpar | T::Rbrace => depth += 1,
            T::Lpar | T::Lbrace | T::Lsqb if depth > 0 => depth -= 1,
            T::Lsqb => {
                return i >= 2
                    && tokens[i - 1].kind == T::Name
                    && (matches!(tokens[i - 2].kind, T::Def | T::Class | T::Type)
                        || (tokens[i - 2].kind == T::Name && text(&tokens[i - 2]) == "type"));
            }
            T::Lpar | T::Lbrace | T::Newline | T::Indent | T::Dedent => return false,
            _ => {}
        }
    }
    false
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

/// Collect differences; justified ones are checked against CPython.
fn run(label: &str, sources: &[(String, String)], mode: Mode) -> (Vec<String>, Vec<Accepted>) {
    let mut failures = Vec::new();
    let mut accepted = Vec::new();
    let mut trees = Vec::new();
    let mut valid = Vec::new();
    let mut invalid = Vec::new();
    let (mut retokenized, mut pep696) = (0, 0);
    let mut retokenized_literals = Vec::new();
    for (name, source) in sources {
        let name = &format!("{name} #"); // unambiguous prefix for the report
        let outcome = compare(source, mode);
        for literal in outcome.retokenized_literals {
            retokenized_literals.push((format!("{name}: {literal:?}"), literal));
        }
        retokenized += outcome.retokenized;
        pep696 += outcome.pep696 as usize;
        for literal in outcome.pep701_valid {
            valid.push((format!("{name}: {literal:?}"), literal));
        }
        for literal in outcome.spec_escapes {
            invalid.push((format!("{name}: {literal:?}"), literal));
        }
        if let Some(diff) = outcome.differ {
            failures.push(format!("{name}: {diff}"));
            if let Some(dir) = std::env::var_os("PYPARSE_DUMP") {
                let file = std::path::Path::new(&dir).join(format!("case{}.py", failures.len()));
                let _ = std::fs::write(file, source);
            }
        }
        if let Some(why) = outcome.new_accepts {
            accepted.push((format!("{name} ({why})"), source.clone(), outcome.agreed));
        }
        if let Some(diff) = outcome.tree_differs {
            eprintln!("{name}: {diff}");
            trees.push((name.clone(), source.clone()));
        }
    }
    eprintln!(
        "{label}: {} compared, {} differences; justified: {} accepted only by the new front end, \
         {} trees where CPython sides with the new one, {} f-strings only PEP 701 accepts, {} \
         f-strings PEP 701 delimits differently, {} f-strings the old parser let through, {} PEP 696 defaults",
        sources.len(),
        failures.len(),
        accepted.len(),
        trees.len(),
        valid.len(),
        retokenized,
        invalid.len(),
        pep696
    );
    if let Some(f) = zipp_pyparse_cpython::check(&trees) {
        failures.extend(f);
    }
    if let Some(f) = zipp_pyparse_cpython::check(&valid) {
        failures.extend(f);
    }
    if let Some(f) = zipp_pyparse_cpython::check_if_valid(&retokenized_literals) {
        failures.extend(f);
    }
    if let Some(f) = zipp_pyparse_cpython::check_rejects(&invalid) {
        failures.extend(f);
    }
    (failures, accepted)
}

fn report(what: &str, failures: &[String]) {
    if !failures.is_empty() {
        for failure in failures.iter().take(25) {
            eprintln!("\n{failure}");
        }
        panic!("{what}: {} differ", failures.len());
    }
}

/// The new-only acceptances must be Python CPython accepts too, with the
/// same tree (when a CPython is available; see `cpython.rs`).
fn check_with_cpython(what: &str, accepted: &[Accepted]) {
    if accepted.is_empty() {
        return;
    }
    for (name, _, _) in accepted.iter().take(10) {
        eprintln!("  new only: {name}");
    }
    let sources: Vec<(String, String)> = accepted
        .iter()
        .map(|(label, source, _)| (label.clone(), source.clone()))
        .collect();
    let Some(failures) = zipp_pyparse_cpython::check(&sources) else {
        eprintln!("  (no CPython found: the new-only acceptances were not checked)");
        return;
    };
    // A rejection CPython also makes of the program both front ends accept
    // (its f-strings neutralized) is not about what only the new one reads.
    let mut real = Vec::new();
    let mut shared = 0;
    for failure in failures {
        let agreed = accepted.iter().find_map(|(label, _, agreed)| {
            let prefix = format!("{label}: CPython rejects");
            failure
                .starts_with(&prefix)
                .then_some(agreed.as_ref())
                .flatten()
        });
        if let Some(agreed) = agreed {
            let probe = [("agreed".to_owned(), agreed.clone())];
            if zipp_pyparse_cpython::check_rejects(&probe).is_some_and(|f| f.is_empty()) {
                shared += 1;
                continue;
            }
        }
        real.push(failure);
    }
    if shared > 0 {
        eprintln!("  {shared} rejected by CPython for a leniency both front ends share");
    }
    report(&format!("{what} (vs CPython)"), &real);
}

#[path = "common/cpython.rs"]
mod zipp_pyparse_cpython;

#[test]
fn repository_sources() {
    let sources = repository_python();
    assert!(sources.len() > 200, "found only {} sources", sources.len());
    let (failures, accepted) = run("repository sources", &sources, Mode::Module);
    report("repository sources", &failures);
    check_with_cpython("repository sources", &accepted);
}

#[test]
fn invalid_snippets() {
    let mut failures = Vec::new();
    let mut accepted = Vec::new();
    for (mode, name) in [
        (Mode::Module, "Module"),
        (Mode::Interactive, "Interactive"),
        (Mode::Expression, "Expression"),
    ] {
        let sources: Vec<(String, String)> = snippets::SNIPPETS
            .iter()
            .map(|s| {
                let s = if mode == Mode::Expression {
                    s.trim_end()
                } else {
                    s
                };
                (format!("{s:?} ({name})"), s.to_owned())
            })
            .collect();
        let (f, a) = run(&format!("snippets {name}"), &sources, mode);
        failures.extend(f);
        if mode == Mode::Module {
            accepted.extend(a);
        }
    }
    report("snippets", &failures);
    check_with_cpython("snippets", &accepted);
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

const PIECES: &[&str] = &[
    "(",
    ")",
    "[",
    "]",
    "{",
    "}",
    ":",
    ",",
    ";",
    "=",
    ":=",
    "*",
    "**",
    ".",
    "-",
    "|",
    "@",
    "->",
    "\n",
    "\n    ",
    "    ",
    "x",
    "_",
    "1",
    "0x",
    "1.5",
    "2j",
    "'s'",
    "b'b'",
    "f'{x}'",
    "f\"{x!r:>{y}}\"",
    "f'{'",
    "f'{*a}'",
    "lambda",
    "yield",
    "await",
    "async",
    "if",
    "else",
    "for",
    "in",
    "not",
    "is",
    "as",
    "with",
    "def",
    "class",
    "return",
    "match",
    "case",
    "type",
    "from",
    "import",
    "except",
    "/",
    "\\\n",
    "#",
    "'",
    "\"",
    "\"\"\"",
    "!",
    "$",
    "\t",
    "u'x'",
    "rb'\\x'",
    "'\\N{DASH}'",
    "==",
    "<",
    "and",
    "or",
    "global",
    "del",
    "try",
    "finally",
    "raise",
    "while",
    "elif",
    "pass",
];

/// Mutate `source` at a token boundary the new lexer found.
fn mutate(source: &str, rng: &mut Rng) -> String {
    let lexed = zipp_pyparse::lexer::lex(source, NewMode::Module, 0, zipp_pyparse::Limits::NONE);
    let spans: Vec<(usize, usize)> = lexed
        .tokens
        .iter()
        .filter(|t| t.end > t.start)
        .map(|t| (t.start as usize, t.end as usize))
        .collect();
    if spans.is_empty() {
        return format!("{}{source}", PIECES[rng.below(PIECES.len())]);
    }
    let (start, end) = spans[rng.below(spans.len())];
    let text = &source[start..end];
    match rng.below(4) {
        0 => format!("{}{}", &source[..start], &source[end..]),
        1 => format!("{}{text}{text}{}", &source[..start], &source[end..]),
        2 => {
            let (s2, e2) = spans[rng.below(spans.len())];
            if s2 >= end {
                format!(
                    "{}{}{}{}{}",
                    &source[..start],
                    &source[s2..e2],
                    &source[end..s2],
                    text,
                    &source[e2..]
                )
            } else {
                format!(
                    "{}{}{}",
                    &source[..start],
                    PIECES[rng.below(PIECES.len())],
                    &source[end..]
                )
            }
        }
        _ => format!(
            "{}{}{}",
            &source[..start],
            PIECES[rng.below(PIECES.len())],
            &source[end..]
        ),
    }
}

#[test]
fn mutated_sources() {
    let seed: u64 = std::env::var("PYPARSE_FUZZ_SEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(7);
    let scale: usize = std::env::var("PYPARSE_FUZZ_SCALE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    let budget: usize = std::env::var("PYPARSE_FUZZ_BYTES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(40_000_000);
    let mut rng = Rng(seed.wrapping_mul(2).wrapping_add(1));
    let mut sources = repository_python();
    for snippet in snippets::SNIPPETS {
        sources.push((format!("snippet {snippet:?}"), snippet.to_string()));
    }
    let mut cases = Vec::new();
    let mut spent = 0;
    for (label, source) in &sources {
        let rounds = (20_000 / source.len().max(1) * 4).clamp(2, 40) * scale;
        for round in 0..rounds {
            if spent > budget {
                break;
            }
            let mut mutated = mutate(source, &mut rng);
            for _ in 0..rng.below(3) {
                mutated = mutate(&mutated, &mut rng);
            }
            spent += mutated.len();
            cases.push((format!("{label} (mutation {round})"), mutated));
        }
    }
    let (failures, accepted) = run("mutations", &cases, Mode::Module);
    let shown: Vec<String> = failures
        .iter()
        .map(|f| {
            let name = f.split(':').next().unwrap_or("");
            let source = cases
                .iter()
                .find(|(l, _)| f.starts_with(l.as_str()))
                .map(|c| &c.1);
            match source {
                Some(s) if s.len() < 300 => format!("{f}\n  source: {s:?}"),
                Some(s) => {
                    // The text around the first offset the report names.
                    let at: usize = f
                        .split(" at ")
                        .nth(1)
                        .and_then(|t| t.split_whitespace().next())
                        .and_then(|t| t.parse().ok())
                        .unwrap_or(0);
                    let mut lo = at.saturating_sub(150).min(s.len());
                    while !s.is_char_boundary(lo) {
                        lo -= 1;
                    }
                    let mut hi = (at + 150).min(s.len());
                    while !s.is_char_boundary(hi) {
                        hi += 1;
                    }
                    let _ = name;
                    format!("{f}\n  around {lo}: {:?}", &s[lo..hi])
                }
                None => f.clone(),
            }
        })
        .collect();
    report("mutations", &shown);
    check_with_cpython("mutations", &accepted);
}

/// `PYPARSE_ONE=file cargo test ... one -- --ignored --nocapture`: both
/// front ends' results for one file.
#[test]
#[ignore = "debugging aid"]
fn one() {
    let Ok(path) = std::env::var("PYPARSE_ONE") else {
        return; // nothing to compare (as in CI's `--include-ignored`)
    };
    let source = std::fs::read_to_string(path).unwrap();
    let old = rustpython_parser::parse(&source, Mode::Module, "<test>");
    let new = zipp_pyparse::rustpython::parse(&source, NewMode::Module, 0);
    match (&old, &new) {
        (Ok(o), Ok(n)) if o == n => eprintln!("same AST"),
        (Ok(o), Ok(n)) => eprintln!("{}", first_difference(o, n)),
        (Err(o), Err(n)) => eprintln!(
            "old: {} at {:?}\nnew: {} at {}",
            o.error, o.offset, n.message, n.offset
        ),
        (Err(o), Ok(_)) => eprintln!("old: {} at {:?}\nnew: ok", o.error, o.offset),
        (Ok(_), Err(n)) => eprintln!("old: ok\nnew: {} at {}", n.message, n.offset),
    }
}

#[test]
fn long_chains() {
    std::thread::Builder::new()
        .stack_size(1 << 30)
        .spawn(|| {
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
            let sources: Vec<(String, String)> = cases
                .iter()
                .map(|(p, u, s)| {
                    (
                        format!("{u:?} x 20000"),
                        format!("{p}{}{s}\n", u.repeat(20_000)),
                    )
                })
                .collect();
            let (failures, _) = run("long chains", &sources, Mode::Module);
            report("long chains", &failures);
        })
        .unwrap()
        .join()
        .unwrap();
}

/// The converter and parser do not recurse per link of a chain.
#[test]
fn long_chains_on_a_small_stack() {
    for (prefix, unit, suffix) in [
        ("x = ", "-", "1"),
        ("x = ", "not ", "1"),
        ("x = a", ".b", ""),
        ("x = f", "()", ""),
        ("x = ", "lambda: ", "1"),
        ("x = ", "a if b else ", "c"),
        ("x = ", "a ** -", "1"),
        ("x = 1", " + 1", ""),
    ] {
        let source = format!("{prefix}{}{suffix}\n", unit.repeat(100_000));
        let ok = std::thread::Builder::new()
            .stack_size(512 << 10)
            .spawn(move || {
                let result =
                    zipp_pyparse::rustpython::parse_module(&source, zipp_pyparse::Limits::NONE);
                let ok = result.is_ok();
                std::mem::forget(result);
                ok
            })
            .unwrap()
            .join()
            .unwrap_or_else(|_| panic!("{unit:?} overflowed"));
        assert!(ok, "{unit:?}");
    }
}

fn cpython_lib() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CPYTHON_LIB") {
        return Some(PathBuf::from(dir));
    }
    zipp_pyparse_cpython::stdlib()
}

#[test]
#[ignore = "needs a CPython installation; run with --ignored"]
fn cpython_lib_sources() {
    let dir = cpython_lib().expect("no CPython Lib found (set CPYTHON_LIB)");
    let mut files = Vec::new();
    collect_py(&dir, &mut files);
    let sources: Vec<(String, String)> = files
        .iter()
        .filter_map(|p| Some((p.display().to_string(), read_source(p)?)))
        .collect();
    let bytes: usize = sources.iter().map(|s| s.1.len()).sum();
    eprintln!(
        "CPython Lib {}: {} files, {bytes} bytes",
        dir.display(),
        sources.len()
    );
    let (failures, accepted) = run("CPython Lib", &sources, Mode::Module);
    report("CPython Lib", &failures);
    check_with_cpython("CPython Lib", &accepted);
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
    let sources: Vec<String> = files.iter().filter_map(|p| read_source(p)).collect();
    let bytes: usize = sources.iter().map(String::len).sum();
    let rounds: usize = std::env::var("PYPARSE_BENCH_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(30);
    let megabytes = (bytes * rounds) as f64 / 1e6;
    let run = |name: &str, f: &mut dyn FnMut()| {
        f();
        let start = Instant::now();
        for _ in 0..rounds {
            f();
        }
        let seconds = start.elapsed().as_secs_f64();
        eprintln!(
            "{name:<40} {:>8.1} ms/round {:>8.1} MB/s",
            seconds * 1e3 / rounds as f64,
            megabytes / seconds
        );
    };
    eprintln!("{} files, {bytes} bytes, {rounds} rounds", sources.len());
    run("old: lex only", &mut || {
        for s in &sources {
            std::hint::black_box(rustpython_parser::lexer::lex(s, Mode::Module).count());
        }
    });
    run("old: lex + parse (descent, rustpython AST)", &mut || {
        for s in &sources {
            std::hint::black_box(rustpython_parser::parse(s, Mode::Module, "").unwrap());
        }
    });
    run("new: lex only", &mut || {
        for s in &sources {
            std::hint::black_box(zipp_pyparse::lexer::lex(
                s,
                NewMode::Module,
                0,
                zipp_pyparse::Limits::NONE,
            ));
        }
    });
    run("new: lex + parse (arena AST)", &mut || {
        for s in &sources {
            std::hint::black_box(
                zipp_pyparse::parse(s, NewMode::Module, 0, zipp_pyparse::Limits::NONE).unwrap(),
            );
        }
    });
    run("new: lex + parse + convert (rustpython AST)", &mut || {
        for s in &sources {
            std::hint::black_box(
                zipp_pyparse::rustpython::parse_module(s, zipp_pyparse::Limits::NONE).unwrap(),
            );
        }
    });
}

/// Every file of a CPython `Lib` against CPython's own tree (not just the
/// ones the old parser rejected): what the new front end builds is Python's
/// AST, up to the RustPython-compatible readings `cpython_check.py` folds.
#[test]
#[ignore = "needs a CPython installation; run with --ignored"]
fn cpython_trees() {
    let dir = cpython_lib().expect("no CPython Lib found (set CPYTHON_LIB)");
    let mut files = Vec::new();
    collect_py(&dir, &mut files);
    let sources: Vec<(String, String)> = files
        .iter()
        .filter_map(|p| Some((p.display().to_string(), read_source(p)?)))
        .collect();
    let mut failures = Vec::new();
    for chunk in sources.chunks(200) {
        match zipp_pyparse_cpython::check_if_valid(chunk) {
            Some(f) => failures.extend(f),
            None => panic!("no CPython"),
        }
    }
    eprintln!(
        "CPython trees: {} files, {} differ",
        sources.len(),
        failures.len()
    );
    report("CPython trees", &failures);
}
