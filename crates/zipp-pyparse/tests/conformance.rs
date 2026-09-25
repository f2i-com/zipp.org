//! The front end against CPython's own parser and against its recorded
//! behaviour.
//!
//! Where CPython 3.12+ accepts a program, ZIPP must accept it too and build
//! the same tree (compared as CPython's `ast`, see `common/cpython.rs`).
//! Where ZIPP rejects a program, the message and offset are the RustPython
//! 0.4 parser's, which ZIPP's front end replaced and matched exactly (the
//! differential tests that proved it were retired with that parser); the
//! goldens under `tests/golden/` record them, with a fingerprint of each
//! accepted program's tree (every node and range). `PYPARSE_BLESS=1`
//! rewrites the goldens after a deliberate change; review their diff.
//!
//! - `repository_sources`: every Python file ZIPP ships or tests, and the
//!   raw string literals of ZIPP's Python test programs, against CPython.
//! - `snippets_golden`, `mutations_golden`: syntax-error snippets in three
//!   modes, and fixed mutations of them, against the goldens.
//! - `mutated_sources`: token deletions, duplications, swaps and
//!   replacements of the repository's sources: whatever CPython accepts,
//!   ZIPP accepts with the same tree (`PYPARSE_FUZZ_SEED`, `_SCALE`).
//! - `long_chains_on_a_small_stack`: parsing and laying out the tree do not
//!   recurse per link of a chain.
//! - `cpython_trees` (ignored; `CPYTHON_LIB` or the found interpreter's
//!   stdlib): every file of a CPython `Lib` against CPython's tree.
//! - `throughput` (ignored; `--release --ignored --nocapture`).

#[path = "common/snippets.rs"]
mod snippets;

#[path = "common/cpython.rs"]
mod zipp_pyparse_cpython;

use std::path::{Path, PathBuf};
use std::time::Instant;
use zipp_pyparse::tree::{self, Bump};
use zipp_pyparse::{Limits, Mode};

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

/// FNV-1a, 64 bits: a fingerprint stable across Rust versions.
fn fnv(text: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// What the front end makes of `source`: its error, or a fingerprint of the
/// tree the emitter walks (every node, name, value and range).
fn outcome(source: &str, mode: Mode) -> String {
    let bump = Bump::new();
    match zipp_pyparse::parse(source, mode, 0, Limits::NONE)
        .and_then(|module| tree::build(&module, &bump).map(|t| format!("{t:?}")))
    {
        Ok(dump) => format!("ok {:016x}", fnv(&dump)),
        Err(e) => format!("err {:?} at {}", e.message, e.offset),
    }
}

fn report(what: &str, failures: &[String]) {
    if !failures.is_empty() {
        for failure in failures.iter().take(25) {
            eprintln!("\n{failure}");
        }
        panic!("{what}: {} differ", failures.len());
    }
}

/// Compare `lines` with the golden file `name` (or rewrite it when
/// `PYPARSE_BLESS` is set).
fn golden(name: &str, lines: &[String]) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name);
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    if std::env::var_os("PYPARSE_BLESS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e} (PYPARSE_BLESS=1 writes it)", path.display()));
    let expected: Vec<&str> = expected.lines().collect();
    let mut failures = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        match expected.get(i) {
            Some(want) if *want == line => {}
            Some(want) => failures.push(format!("line {}:\n  want {want}\n  got  {line}", i + 1)),
            None => failures.push(format!("line {}: extra {line}", i + 1)),
        }
    }
    if expected.len() > lines.len() {
        failures.push(format!("{} lines missing", expected.len() - lines.len()));
    }
    report(name, &failures);
}

/// Check sources against CPython: whatever it accepts, ZIPP accepts with
/// the same tree; and (unless `lenient`) ZIPP accepts nothing CPython
/// rejects, other than the assignment targets ZIPP's compiler checks.
fn check_with_cpython(what: &str, sources: &[(String, String)], lenient: bool) {
    let result = if lenient {
        zipp_pyparse_cpython::check_where_cpython_accepts(sources)
    } else {
        zipp_pyparse_cpython::check_if_valid(sources)
    };
    match result {
        Some(failures) => report(&format!("{what} (vs CPython)"), &failures),
        None => eprintln!("  (no CPython 3.12+ found: {what} not checked)"),
    }
}

#[test]
fn repository_sources() {
    let sources = repository_python();
    eprintln!("repository sources: {}", sources.len());
    check_with_cpython("repository sources", &sources, false);
}

const MODES: [(Mode, &str); 3] = [
    (Mode::Module, "Module"),
    (Mode::Interactive, "Interactive"),
    (Mode::Expression, "Expression"),
];

fn snippet_in(snippet: &str, mode: Mode) -> &str {
    if mode == Mode::Expression {
        snippet.trim_end()
    } else {
        snippet
    }
}

#[test]
fn snippets_golden() {
    let mut lines = Vec::new();
    for (mode, name) in MODES {
        for (i, snippet) in snippets::SNIPPETS.iter().enumerate() {
            let source = snippet_in(snippet, mode);
            lines.push(format!("{name} {i} {source:?}: {}", outcome(source, mode)));
        }
    }
    golden("snippets.txt", &lines);
    let sources: Vec<(String, String)> = snippets::SNIPPETS
        .iter()
        .map(|s| (format!("{s:?}"), s.to_string()))
        .collect();
    check_with_cpython("snippets", &sources, true);
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

/// Mutate `source` at a token boundary.
fn mutate(source: &str, rng: &mut Rng) -> String {
    let lexed = zipp_pyparse::lexer::lex(source, Mode::Module, 0, Limits::NONE);
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

/// Fixed mutations of the snippets (a corpus that changes only with them).
#[test]
fn mutations_golden() {
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    let mut lines = Vec::new();
    let mut accepted = Vec::new();
    for (i, snippet) in snippets::SNIPPETS.iter().enumerate() {
        for round in 0..5 {
            let mut mutated = mutate(snippet, &mut rng);
            for _ in 0..rng.below(3) {
                mutated = mutate(&mutated, &mut rng);
            }
            let result = outcome(&mutated, Mode::Module);
            if result.starts_with("ok") {
                accepted.push((format!("snippet {i} mutation {round}"), mutated.clone()));
            }
            lines.push(format!("{i}.{round} {mutated:?}: {result}"));
        }
    }
    golden("mutations.txt", &lines);
    check_with_cpython("mutated snippets", &accepted, true);
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
    let mut rng = Rng(seed.wrapping_mul(2).wrapping_add(1));
    let mut cases = Vec::new();
    for (label, source) in &repository_python() {
        let rounds = (20_000 / source.len().max(1) * 4).clamp(2, 40) * scale;
        for round in 0..rounds {
            let mut mutated = mutate(source, &mut rng);
            for _ in 0..rng.below(3) {
                mutated = mutate(&mutated, &mut rng);
            }
            // Every mutation parses without a panic and lays out its tree.
            let _ = outcome(&mutated, Mode::Module);
            cases.push((format!("{label} (mutation {round})"), mutated));
        }
    }
    eprintln!("mutations: {}", cases.len());
    // `PYPARSE_SHOW=<label>`: print the mutated source(s) so labelled.
    if let Ok(want) = std::env::var("PYPARSE_SHOW") {
        for (label, source) in cases.iter().filter(|(l, _)| l.ends_with(&want)) {
            eprintln!(
                "{label}:
{source}"
            );
        }
    }
    check_with_cpython("mutations", &cases, true);
}

/// Every name of the `\N{...}` table is CPython's name (or alias) for the
/// same character.
#[test]
fn unicode_names_match_cpython() {
    let Some(mut python) = zipp_pyparse_cpython::python_command() else {
        eprintln!("  (no CPython 3.12+ found: names not checked)");
        return;
    };
    let names: String = zipp_pyparse::unicode_names::all()
        .iter()
        .map(|(name, c)| format!("{:x} {name}\n", *c as u32))
        .collect();
    let path = std::env::temp_dir().join(format!("zipp-pyparse-names-{}.txt", std::process::id()));
    std::fs::write(&path, names).unwrap();
    let script = r#"
import sys, unicodedata
for line in open(sys.argv[1], encoding="utf-8"):
    code, name = line.rstrip("\n").split(" ", 1)
    try:
        ok = unicodedata.lookup(name) == chr(int(code, 16))
    except KeyError:
        ok = False
    if not ok:
        print(name)
"#;
    let output = python.arg("-c").arg(script).arg(&path).output().unwrap();
    let _ = std::fs::remove_file(&path);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let wrong = String::from_utf8_lossy(&output.stdout);
    assert!(
        wrong.trim().is_empty(),
        "names CPython does not know:\n{wrong}"
    );
}

// Parsing and laying out the tree do not recurse per link of a chain.
#[test]
fn long_chains_on_a_small_stack() {
    for (prefix, unit, suffix) in [
        ("x = ", "-", "1"),
        ("x = ", "not ", "1"),
        ("x = a", ".b", ""),
        ("x = f", "()", ""),
        ("x = ", "lambda: ", "1"),
        ("x = ", "lambda a, b=1: ", "1"),
        ("x = ", "a if b else ", "c"),
        ("x = ", "lambda: a if b else ", "c"),
        ("x = ", "a ** -", "1"),
        ("x = ", "await a ** ", "1"),
        ("x = 1", " + 1", ""),
        ("x = 1", " < 1", ""),
        ("x = 1", " and 1 or 1", ""),
    ] {
        let source = format!("{prefix}{}{suffix}\n", unit.repeat(100_000));
        let result = std::thread::Builder::new()
            .stack_size(512 << 10)
            .spawn(move || {
                let bump = Bump::new();
                let module = zipp_pyparse::parse(&source, Mode::Module, 0, Limits::NONE)?;
                tree::build(&module, &bump).map(|body| body.len())
            })
            .unwrap()
            .join()
            .unwrap_or_else(|_| panic!("{unit:?} overflowed"));
        assert_eq!(result, Ok(1), "{unit:?}");
    }
}

fn cpython_lib() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CPYTHON_LIB") {
        return Some(PathBuf::from(dir));
    }
    zipp_pyparse_cpython::stdlib()
}

/// Every file of a CPython `Lib`: what CPython accepts, ZIPP builds as
/// CPython does.
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
            "{name:<34} {:>8.1} ms/round {:>8.1} MB/s",
            seconds * 1e3 / rounds as f64,
            megabytes / seconds
        );
    };
    eprintln!("{} files, {bytes} bytes, {rounds} rounds", sources.len());
    run("lex", &mut || {
        for s in &sources {
            std::hint::black_box(zipp_pyparse::lexer::lex(s, Mode::Module, 0, Limits::NONE));
        }
    });
    run("lex + parse (arena AST)", &mut || {
        for s in &sources {
            std::hint::black_box(zipp_pyparse::parse(s, Mode::Module, 0, Limits::NONE).unwrap());
        }
    });
    run("lex + parse + tree (what ZIPP runs)", &mut || {
        for s in &sources {
            let bump = Bump::new();
            let module = zipp_pyparse::parse(s, Mode::Module, 0, Limits::NONE).unwrap();
            std::hint::black_box(tree::build(&module, &bump).unwrap());
        }
    });
}
