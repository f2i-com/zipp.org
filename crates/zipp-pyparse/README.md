# zipp-pyparse

ZIPP's own Python front end: a byte-oriented lexer, a compact arena AST and
a recursive-descent parser for Python 3.13 syntax. ZIPP's Python frontend
(`crates/zipp-vm/src/frontend/python`) parses every module with it. Written
for ZIPP (no RustPython code). The grammar handling follows the
hand-written descent parser that replaced LALRPOP in
`crates/rustpython-parser-fork`.

```rust
use zipp_pyparse::{parse, Limits, Mode};
let module = parse("print(f'{x=}')\n", Mode::Module, 0, Limits::NONE)?;
```

## Layout

| File | What it is |
| --- | --- |
| `src/lexer.rs` | Bytes in, `Vec<Token>` out. Tokens are 16-byte spans with a kind; there is no allocation per token. Names are interned (`intern.rs`, borrowed `&str` keys). Numbers and strings stay source text until the parser needs them. Indentation, the limits and the soft-keyword pass also live here. |
| `src/token.rs` | Token kinds, string prefixes and keywords. |
| `src/ast.rs` | The arena AST. Nodes are `u32` ids into per-kind tables, and lists are `(start, len)` ranges into side tables. Names are interned `Sym`s and ranges are `u32` byte offsets, with a line table for `line_col`. Number constants keep their text (`int_digits`, `float_value`), and string constants point into one string arena. |
| `src/parser/` | The parser. `expr.rs`, `stmt.rs` and `pattern.rs` handle the grammar. `fstring.rs` builds string runs and PEP 701 f-strings. `compat.rs` reproduces the classic f-string parser's errors. |
| `src/strings.rs` | Escape decoding: str, bytes and f-string literal parts. |
| `src/unicode_names.rs` | `\N{...}` lookups: a word-coded table (see below) plus the algorithmic `CJK UNIFIED IDEOGRAPH-XXXX` names. |
| `src/rustpython.rs` | Feature `rustpython`: the converter to the `rustpython_ast` nodes the emitter consumes. |

The parser recurses only for real nesting: brackets, blocks and nested
f-strings. That recursion is bounded at 1000 levels, and f-strings nest at
most 149 deep, as in CPython. Right-nested chains are parsed iteratively and
converted with an explicit stack, as are unary operators, `not`, `**`,
attribute, call and subscript trailers, conditional expressions and
lambdas. A 100,000-link chain parses on a 256 KB stack.

## Compatibility

The front end reproduces the RustPython 0.4 parser that ZIPP used before.
It accepts the same programs and builds the same `rustpython_ast` tree,
ranges included. It rejects the same programs with the same message at the
same offset. On top of that, it accepts Python 3.12/3.13 syntax the old
parser lacked:

- **PEP 701 f-strings.** This covers nested quotes of the same kind, multi-line
  and commented replacement fields, backslashes, `f"{*a}"` and format specs
  nested two deep. An f-string that PEP 701 cannot tokenize but the classic
  string rules can delimit is rejected with the old parser's message.
- **PEP 696 type parameter defaults.** These are parsed. The converter
  rejects them ("type parameter defaults (PEP 696) are not supported")
  until the emitter supports them.

Deliberate differences, each checked against CPython:

- Escapes in f-string format specs are decoded. The old parser kept them
  raw.
- The `\N{...}` table covers the common names the fork's table had (2,745
  names and aliases). An unknown name is an error saying so.
- Identifiers are not NFKC-normalized, and lone surrogates in strings read as
  U+FFFD. The old parser behaved the same way.

## Tests

```sh
cargo test -p zipp-pyparse --test basics
cargo test --release -p zipp-pyparse --features rustpython --test differential -- --include-ignored --skip throughput
```

`tests/differential.rs` compares the front end with the descent parser, in
all three modes. The two must build equal `rustpython_ast` trees (ranges
included) or report equal errors (message and offset). The corpora are:

- every Python file in the repository;
- syntax-error snippets;
- a mutation fuzzer (`PYPARSE_FUZZ_SEED`, `PYPARSE_FUZZ_SCALE`);
- a CPython `Lib` (`CPYTHON_LIB`, default the found interpreter's stdlib).

Where only the new front end accepts a program (PEP 701/696 syntax),
CPython 3.12+ must accept it too and build the same tree
(`tests/common/cpython_check.py`). `cpython_trees` goes further: it compares
the tree of every file of a CPython `Lib` with CPython's own `ast`.

`throughput` is a benchmark: `cargo test --release ... -- --ignored
throughput --nocapture`.

## Performance

The benchmark corpus is ZIPP's embedded Python library: 50 files, 1.79 MB,
in a release build on x86-64 (`throughput`).

| Stage | Time | vs the descent parser |
| --- | --- | --- |
| Lexing | 7.8 ms | 2.8x faster (old lexer 21.6 ms) |
| Lexing and parsing to the arena AST | 14.4 ms | 3.9x faster (old lex and parse 57 ms) |
| Plus converting to the RustPython AST | 36 ms | 1.6x faster |

The conversion costs more than lexing and parsing together, because it
allocates every RustPython node. That cost goes away once the emitter reads
the arena AST directly.

## Regenerating the name table

```sh
py -3.13 gen_unicode_names.py > src/unicode_names_table.rs
```

The table lists the distinct words of the names once (1,828 words,
13.4 KB). Each name is then a code-point delta and word indices, all as
varints (22 KB). That is 35.5 KB in all, 15.4 KB after Brotli. The fork's
`HEX NAME` text table held the same names in 70.7 KB, 17.6 KB after Brotli.
A lookup, made only when a program spells `\N{`, scans the table once.
