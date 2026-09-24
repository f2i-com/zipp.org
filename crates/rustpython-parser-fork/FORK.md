# RustPython parser maintenance patch

Upstream: `rustpython-parser` 0.4.0, MIT, source commit
`8dd2aea26778d8d6917770f8e32bea1b9cdc0ae8` in
https://github.com/RustPython/Parser (the published crates.io parser directory).
`LICENSE` is copied from that exact upstream commit. The lexer, the AST
integration (`rustpython-ast` 0.4), string and f-string parsing and the
public API are upstream's; the parser itself is ZIPP's (below).

Role now: ZIPP's Python frontend parses with its own front end,
`crates/zipp-pyparse`, and converts that to the `rustpython_ast` nodes the
emitter consumes. `zipp-vm` still depends on this crate for three things: its
`ast` re-export, `text_size`, and the lexer of the `minify_check` test. In
shipped builds the linker drops the lexer and parser. The crate is also the
reference that `zipp-pyparse`'s differential tests compare against. Once
the emitter reads the arena AST directly, the dependency can go.

## The parser: hand-written, replacing the generated LR parser

Upstream compiles `src/python.lalrpop` with LALRPOP into `src/python.rs`
(2.3 MB of LR tables and actions; about 666 KB of the Python WebAssembly
build). ZIPP parses with `src/descent/` instead: recursive descent for
statements and patterns, precedence climbing for the binary operators. It
consumes the same token stream (the lexer and soft-keyword pass produce
upstream's tokens)
and builds the same AST, node for node and range for range, with the same
errors. `parse_tokens`, `Parse` and every other public entry point route to
it; nothing in `zipp-vm` changed.

- `mod.rs`: the token source (a small lookahead buffer, plus a checkpoint
  log used by the one backtracking point), errors, entry.
- `expr.rs`: expressions, calls, comprehensions, parameter lists.
- `stmt.rs`: statements and blocks; `pattern.rs`: `match` and patterns.

What it keeps from the grammar, deliberately, including its quirks: the
ranges LALRPOP's `@L`/`@R` gave (a parenthesized operand's parentheses are
inside its parent's range, a named expression ends at its value's own end,
a multi-subject `match` tuple spans the whole statement, the leading
`with` items without `as` share one range, ...), what it accepts beyond
CPython (`f(x for x in y, z)`, `def f(**)`, `(x): int` as a simple
annotation, `[*a for a in b]`), and its errors:

- A syntax error is `UnrecognizedToken` at the first token that cannot
  continue a valid program, `Eof` at the end of the last token, or the
  indentation error / "expected an indented block" where only an indent
  could follow. (LALRPOP's `expected` token list is not reproduced beyond
  that indent case: it is a table dump no caller reads; `ParseErrorType`'s
  `Display`, which ZIPP reports, is identical.)
- The grammar's semantic checks (duplicate or misordered parameters and
  arguments, bare `*`, starred and double-starred parenthesized
  expressions, `_` as an `as` target, f-string and string-concatenation
  errors) run where the grammar's actions ran: when the rule is reduced,
  which the LR parser only does once the next token is in the rule's
  lookahead set. `descent::Follow` holds those sets, so a semantic error
  followed by an invalid token reports what the LR parser reported, and a
  lexical error in that position wins, as it did.
- `with (`: parenthesized items (`with (a as b, c):`) and a parenthesized
  expression (`with (a, b) as c:`) share a prefix the LR parser resolved with
  its lookahead; the parser tries the items, and on failure re-reads the
  tokens as an expression, reporting whichever failure got further.

Right-nested chains (`- - x`, `not not x`, `a ** b ** c`, `lambda: lambda:`,
`a if b else c if d else e`) and postfix and operator chains are read
iteratively: like the LR parser, a million-link chain costs no stack, which
ZIPP's frontend relies on (it bounds nesting itself afterwards). Brackets,
blocks and lambda defaults recurse, about 2.4 KB of stack per level in a
release build; past 1000 levels the parser stops with a "too many nested
expressions or blocks" error rather than overflow (ZIPP's frontend already
stops at 200 brackets and 100 indentation levels).

Measured on this repository's bundled Python library (50 files, 1.79 MB,
release, Windows x86-64, best of three): parsing a pre-lexed token stream
takes 26 ms instead of 60 ms (2.3x; 69 instead of 30 MB/s; net of the 8 ms
the `throughput` test spends cloning the stream, so it prints 34 and 68), lexing and
parsing 56 ms instead of 90 ms (1.6x) with the same lexer, and instead of
about 102 ms with the lexer as it was (the `match` keywords and the
soft-keyword buffer below took it from about 41 to 37 ms). The lexer is now
two thirds of the time. In the WebAssembly build of ZIPP with Python, the
switch took about 700 KB off the raw module (6.8%) and 56 KB off the Brotli
one (2.7%): the LR tables compressed well.

The generated parser is kept, unchanged, as the reference: the test-only
`reference-parser` feature compiles `src/python.rs` (and `lalrpop-util`) as
`reference::parse_tokens`, which also parses f-string replacement fields
with it. Shipped builds contain neither; `build.rs` is gone: it checked the
grammar's hash (nothing regenerates it) and generated the lexer's `phf`
keyword map, now a `match` (`lexer::KEYWORDS` is gone; nothing used it).
The soft-keyword pass peeks through a small buffer of its own instead of
`itertools::MultiPeek`, and the parser pulls tokens through one `dyn
Iterator` instead of being compiled once per token-source type. Together
that drops `lalrpop-util`, `itertools`, `either`, `phf` (with `phf_shared`,
`siphasher`, and at build time `phf_codegen`, `phf_generator`, `rand`,
`rand_core`), `anyhow` and `tiny-keccak` from ZIPP's build.

`tests/differential.rs` compares the two on the same token stream (AST
equality including every range, or equal error text and offset):

- `repository_sources`: every `.py` under `tests/python_corpus`,
  `crates/zipp-vm/src/frontend/python/lib`, `crates/zipp-wasm/model-plugins`
  and `tools/python_bench`, and the raw string literals of ZIPP's Python
  tests (422 sources, 3.1 MB).
- `invalid_snippets`: about 270 programs, mostly syntax errors, in module,
  interactive and expression mode.
- `reduction_lookaheads`: every construct whose grammar action can fail, in
  25 contexts, followed by every kind of token (37,000 streams): the
  `Follow` sets against the LR tables.
- `mutated_tokens`: one to three token deletions, duplications, swaps or
  replacements of all of the above (`PARSER_FUZZ_SEED` / `_SCALE` /
  `_TOKENS` widen it).
- `long_chains`: 20,000-link chains of every iterative form.
- The soft-keyword pass against upstream's (`tests/reference/`), on the
  repository, the snippets and CPython's `Lib`.
- `cpython_lib_sources` (ignored; `CPYTHON_LIB` or the `py -3.13` /
  `python3` on `PATH`): a CPython `Lib` tree.
- `throughput` (ignored): the timings above.

Run it with
`cargo +1.92.0 test --locked --release --no-default-features --features location,num-bigint,reference-parser --test differential`
(add `all-nodes-with-ranges` to compare the optional ranges too, and
`-- --include-ignored` for CPython and the benchmark). At the switch all of
it agreed, with and without `all-nodes-with-ranges`: the repository, the
snippets, 1.3 million mutated streams, and the CPython 3.13 `Lib`, the 3.12
`Lib` with its test suite (2,215 files, 38 MB) and a 3.11 installation with
its site-packages (38,742 files, 541 MB). Both parsers reject the same few
files with the same error: Python 3.12 f-strings (PEP 701), `\N{...}` names
outside the fork's table, and Python 2 or deliberately invalid test data.

## Unicode dependencies

ZIPP changes the Unicode imports/helper in `src/lexer.rs` and the `\N{...}`
name lookup in `src/string.rs` (with `src/unicode_names.rs`):

- Replace `unic-ucd-ident` with `unicode-ident` 1.0.24 for XID_Start/XID_Continue.
- Replace `unic-emoji-char` with the Emoji_Presentation statuses from
  `unicode-properties` 0.1.4, with only its `emoji` feature enabled.
- Replace `unicode_names2` (every Unicode character name: 832 KB of tables,
  about 300 KB Brotli, in the wasm build) with `src/unicode_names.rs`, a
  generated table of the names `\N{...}` escapes commonly spell (Latin-1,
  Greek letters, punctuation, currency, arrows, mathematical operators, box
  drawing, symbols, dingbats and common emoji), the control-character aliases
  and the algorithmic `CJK UNIFIED IDEOGRAPH-XXXX` names. Lookup is
  case-insensitive, as in CPython. Any other name is a SyntaxError naming the
  escape and the limitation. `gen_unicode_names.py` regenerates the table
  from CPython's `unicodedata`; the checked-in table was generated with
  CPython 3.13 (Unicode 15.1).
- Spell lexer iterator lifetimes explicitly (`Chars<'_>`) for current Rust's
  warnings-as-errors builds; this does not change parser behavior.
- Both maintained identifier/emoji tables use Unicode 17.0. Newly assigned identifier/emoji
  characters are consequently accepted. RustPython's existing single-emoji
  identifier extension remains available; text-presentation emoji such as
  copyright are not accidentally accepted as identifiers.

This removes all six unmaintained UNIC packages from ZIPP's resolved dependencies
without audit exceptions. `Cargo.toml.orig` records the original upstream manifest.
ZIPP selects this fork through a direct path dependency, so the isolated WASM
workspace receives the same fix without duplicated Cargo patch configuration.
`LexicalError`, `LexicalErrorType`, `FStringErrorType` and `ParseErrorType`
also derive `Clone` (the parser re-reports a lexical error it has seen).

Validation: `crates/zipp-vm/tests/python_frontend.rs` covers Unicode identifiers,
emoji and invalid starts through the real frontend; the crate's own unit tests
(`cargo test --lib`, snapshot-checked against upstream's output) and the full
Python, project and Torch suites cover the parser. The crate is outside ZIPP's
workspace; its `Cargo.lock` pins the test-only dependencies (`insta`,
`lalrpop-util`) for `--locked` runs.
