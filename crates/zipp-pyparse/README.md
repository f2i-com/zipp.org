# zipp-pyparse

ZIPP's own Python front end: a byte-oriented lexer, a compact arena AST, a
recursive-descent parser for Python 3.13 syntax, and the tree ZIPP's
compiler walks. ZIPP's Python frontend (`crates/zipp-vm/src/frontend/python`)
parses every module with it. It was written for ZIPP and contains no
RustPython code.

```rust
use zipp_pyparse::{parse, tree, Limits, Mode};
let module = parse("print(f'{x=}')\n", Mode::Module, 0, Limits::NONE)?;
let bump = tree::Bump::new();
let body = tree::build(&module, &bump)?; // what the emitter walks
```

## Layout

| File | What it is |
| --- | --- |
| `src/lexer.rs` | Bytes in, `Vec<Token>` out. Tokens are 16-byte spans with a kind; there is no allocation per token. Names are interned (`intern.rs`, with borrowed `&str` keys). Numbers and strings stay source text until the parser needs them. Indentation, the limits and the soft-keyword pass also live here. |
| `src/token.rs` | Token kinds, string prefixes and keywords. |
| `src/ast.rs` | The arena AST. Nodes are `u32` ids into per-kind tables, and lists are `(start, len)` ranges into side tables. Names are interned `Sym`s, and ranges are `u32` byte offsets with a line table. Number constants keep their text, and string constants point into one string arena. |
| `src/parser/` | The parser. `expr.rs`, `stmt.rs` and `pattern.rs` handle the grammar. `fstring.rs` builds string runs and PEP 701 f-strings. `compat.rs` reproduces the classic f-string parser's errors. |
| `src/tree.rs`, `src/tree/build.rs` | The tree the emitter walks. |
| `src/strings.rs` | Escape decoding: str, bytes and f-string literal parts. |
| `src/unicode_names.rs` | `\N{...}` lookups: a word-coded table (see below) plus the algorithmic `CJK UNIFIED IDEOGRAPH-XXXX` names. |

The emitter's tree is laid out in a `bumpalo` arena. Its nodes are `Copy`
and borrowed: children are `&'a Expr<'a>`, lists are `Seq`s, and names and
string constants are `&'a str`. The whole tree is freed at once, however
deep it is. It keeps the shape of the RustPython 0.4 AST the emitter was
written against: the same enum and struct names, the same field names and
the same node ranges. So the emitter's code was ported by changing one
import. `tree::build` walks expressions with an explicit stack.

The parser recurses only for real nesting: brackets, blocks and nested
f-strings. That recursion is bounded at 1000 levels, and f-strings nest at
most 149 deep, as in CPython. Right-nested chains are parsed iteratively,
as are unary operators, `not`, `**`, attribute, call and subscript trailers,
conditional expressions and lambdas. A 100,000-link chain parses and builds
its tree on a 256 KB stack.

## Compatibility

The front end accepts what the RustPython 0.4 parser, which ZIPP used
before, accepted. It builds the same tree with the same ranges, and it
rejects the same programs with the same message at the same offset.
Differential tests against that parser proved this over:

- the repository's sources;
- the CPython 3.11, 3.12 and 3.13 `Lib` directories (38,742 files);
- syntax-error snippets;
- over three million mutated programs.

That parser has since been retired. `tests/golden/` now records its error
messages, and the conformance tests check against CPython instead.

On top of that, the front end accepts Python 3.12/3.13 syntax the old
parser lacked:

- **PEP 701 f-strings.** This covers nested quotes of the same kind,
  multi-line and commented replacement fields, backslashes, `f"{*a}"`, and
  format specs nested two deep. An f-string that PEP 701 cannot tokenize but
  the classic string rules can delimit is rejected with the old parser's
  message.
- **PEP 696 type parameter defaults** (`def f[T = int]`). As with bounds,
  ZIPP's compiler accepts them on functions and ignores them.

Deliberate differences from CPython, which the conformance tests allow for:

- Identifiers are not NFKC-normalized.
- Lone surrogates in strings read as U+FFFD.
- The `\N{...}` table knows only the common names (2,745 names and aliases);
  an unknown name is an error saying so.
- Parameters and keyword arguments are checked for duplicates in the parser,
  where CPython checks them in its compiler.
- The RustPython grammar is kept in two places: tabs after spaces in
  indentation are rejected, and `type` is a keyword only at the start of a
  line.

## Tests

```sh
cargo test --release -p zipp-pyparse -- --include-ignored --skip throughput
```

- **`tests/basics.rs`** checks tokens, trees, errors, limits and the
  `\N{...}` table. It also checks that deep chains and deep nesting work on
  small stacks.
- **`tests/conformance.rs`** checks against CPython 3.12+ (`py -3.13`,
  `python3`, or `PYPARSE_PYTHON`) and against the goldens. Wherever CPython
  accepts a program, ZIPP must accept it and build CPython's `ast` tree
  (compared by `tests/common/cpython_check.py`). This covers:
  - every Python source in the repository;
  - a mutation fuzzer (`PYPARSE_FUZZ_SEED`, `PYPARSE_FUZZ_SCALE`);
  - with `--include-ignored`, every file of the interpreter's own `Lib`
    (or `CPYTHON_LIB`).
- **`snippets_golden` and `mutations_golden`** compare syntax-error snippets
  in all three modes, and fixed mutations of them, with `tests/golden/`. Each
  golden records the error, or a fingerprint of the tree with every node,
  value and range. After a deliberate change, `PYPARSE_BLESS=1` rewrites the
  goldens; review their diff.
- **`throughput`** is a benchmark: `cargo test --release -p zipp-pyparse
  --test conformance -- --ignored throughput --nocapture`.

## Performance

These timings are for ZIPP's embedded Python library (50 files, 1.8 MB) in a
release build on x86-64 (`throughput`, best of 40 rounds):

| Stage | Time | Before (the descent parser) |
| --- | --- | --- |
| Lexing | 9 ms | 22 ms |
| Lexing and parsing to the arena AST | 17 ms | |
| Lexing, parsing and building the tree (what ZIPP runs) | 24 ms | 57 ms (RustPython AST) |

## Regenerating the name table

```sh
py -3.13 gen_unicode_names.py > src/unicode_names_table.rs
cargo fmt -p zipp-pyparse
```

The table lists the distinct words of the names once (1,828 words,
13.4 KB). Each name is then a code-point delta and its word indices, all as
varints (22 KB). That is 35.5 KB in all, or 15.4 KB after Brotli. The
fork's `HEX NAME` text table held the same names in 70.7 KB, or 17.6 KB
after Brotli. A lookup scans the table once, and only runs when a program
spells `\N{`.

## Provenance and licence

Apache-2.0, like the rest of ZIPP. The crate was written for ZIPP and
contains no code from RustPython, CPython or any other Python
implementation. It does share some things with them on purpose, for
compatibility:

- The node and field names of `tree` follow the RustPython 0.4 AST (MIT),
  which in turn follows CPython's `ast` module.
- Syntax errors keep the RustPython 0.4 parser's messages and offsets.

These are an interface ZIPP's compiler and its users already depended on,
not copied code, so they carry no licence obligation. This note records
where they come from.

The `\N{...}` name table (`src/unicode_names_table.rs`) is data derived from
the Unicode Character Database, read through CPython's `unicodedata`. Unicode
data is Copyright (c) Unicode, Inc. and is used under the Unicode License v3;
the licence text is in `LICENSE-UNICODE` at the repository root. The table
file carries the same notice.
