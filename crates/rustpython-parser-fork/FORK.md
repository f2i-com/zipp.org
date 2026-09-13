# RustPython parser maintenance patch

Upstream: `rustpython-parser` 0.4.0, MIT, source commit
`8dd2aea26778d8d6917770f8e32bea1b9cdc0ae8` in
https://github.com/RustPython/Parser (the published crates.io parser directory).
`LICENSE` is copied from that exact upstream commit. The generated grammar,
AST integration and parser implementation are unchanged.

ZIPP changes `Cargo.toml`, the Unicode imports/helper in `src/lexer.rs`, and
explicit lifetime spelling in `src/parser.rs` and `src/gen/parse.rs`:

- Replace `unic-ucd-ident` with `unicode-ident` 1.0.24 for XID_Start/XID_Continue.
- Replace `unic-emoji-char` with the Emoji_Presentation statuses from
  `unicode-properties` 0.1.4, with only its `emoji` feature enabled.
- Spell lexer iterator lifetimes explicitly (`Chars<'_>`) for current Rust's
  warnings-as-errors builds; this does not change parser behavior.
- Both maintained tables use Unicode 17.0. Newly assigned identifier/emoji
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
