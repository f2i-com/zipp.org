# RustPython parser maintenance patch

Upstream: `rustpython-parser` 0.4.0, MIT, source commit
`8dd2aea26778d8d6917770f8e32bea1b9cdc0ae8` in
https://github.com/RustPython/Parser (the published crates.io parser directory).
`LICENSE` is copied from that exact upstream commit. The generated grammar,
AST integration and parser implementation are unchanged.

ZIPP changes `Cargo.toml`, the Unicode imports/helper in `src/lexer.rs`, the
`\N{...}` name lookup in `src/string.rs` (with `src/unicode_names.rs`), and
explicit lifetime spelling in `src/parser.rs` and `src/gen/parse.rs`:

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

Validation: `crates/zipp-vm/tests/python_frontend.rs` covers Unicode identifiers,
emoji and invalid starts through the real frontend; the existing full Python,
project and Torch suites cover the unchanged parser integration. Remove this fork
when a compatible published upstream parser uses maintained Unicode dependencies.
