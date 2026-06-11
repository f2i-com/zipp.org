# Vendored: regress 0.11.1 (patched)

- **Upstream**: `regress` 0.11.1 from crates.io
  (https://github.com/ridiculousfish/regress), copied verbatim from
  `~/.cargo/registry/src/index.crates.io-*/regress-0.11.1/`
  (minus `Cargo.lock`, `Cargo.toml.orig`, and the registry's dangling
  `[profile.release]` section in the normalized `Cargo.toml`).
- **Why vendored**: three upstream correctness bugs hit by test262
  (`built-ins/RegExp/regexp-modifiers`, `named-groups`, `property-escapes`).
  Wired via `[patch.crates-io] regress = { path = "crates/regress-patched" }`
  in the workspace root `Cargo.toml`; zipp-vm keeps depending on
  `regress = { version = "0.11", features = ["utf16"] }`.
- **Re-upgrading**: when bumping regress, re-apply the three patches below (or
  drop whichever has been upstreamed) and regenerate `unicodetables_unknown.rs`.

All patches are intended to be upstreamable. `cargo test -p regress` passes.

## Patch B1: effective (modifier-scoped) ignoreCase for `\p`/`\P`, `\b`/`\B`, and backreferences

regress parses `(?i:...)`/`(?-i:...)` modifier groups by swapping
`self.flags` while parsing the body, and literal `Char`/bracket nodes already
capture the scoped flag. But three constructs consulted the **global**
`flags.icase` at emit/match time, ignoring the scope — and `\p`/`\P` as a
standalone atom applied **no** case folding at all, which was wrong even for a
global `/iu` (`/\p{Lu}/iu` failed to match `'a'`).

- `src/ir.rs`: `Node::WordBoundary` and `Node::BackRef` gain an `icase: bool`
  (the effective flag where the node was parsed); new `Node::NamedBackRef`
  (see B2). `try_duplicate`, walker leaf lists, and `Display` updated.
- `src/parse.rs`:
  - `\b`/`\B` and every `BackRef` construction store `self.flags.icase`.
  - the `\p{...}`/`\P{...}` atom arm closes the class under case folding
    (`unicode::add_icase_code_points`) when the effective `icase` is set.
    In unicode mode the `\P` complement is applied **before** the closure
    (per 22.2.2.9 the input is canonicalized against the complemented set:
    `fold('A')='a'` is in the complement of `Lu`, so `(?i:\P{Lu})` matches
    `'A'`); in unicode-sets mode folding applies first (MaybeSimpleCaseFolding)
    and the `invert` flag is kept on the bracket.
- `src/insn.rs`: `Insn::BackRef` carries `icase`.
- `src/emit.rs`: `WordBoundaryUnicodeICase` is chosen from
  `flags.unicode && node.icase` (the unicode flag cannot be modifier-scoped);
  `Insn::BackRef` is emitted with the node's `icase`.
- `src/classicalbacktrack.rs`, `src/pikevm.rs`: backreference comparison uses
  the instruction's `icase` instead of `re.flags.icase`.
- `src/startpredicate.rs`, `src/optimizer.rs`: pattern updates for the changed
  / new variants only.

## Patch B2: duplicate named-group backreference participation

`\k<name>` where `name` is declared by multiple groups (in distinct
alternatives) was lowered to an alternation of plain backreferences. A
backreference to a non-participating group succeeds with an empty match (ES
22.2.2.9), so the first branch always empty-succeeded and shadowed a later
participating group: `/(?:(?<x>a)|(?<x>b))\k<x>/.exec("bb")` matched `"b"`
instead of `"bb"`.

- `src/ir.rs`: new `Node::NamedBackRef { groups, icase }` (1-based indices).
- `src/parse.rs`: the multi-index `\k<name>` case builds it (single-index
  still builds `Node::BackRef`).
- `src/insn.rs` + `src/emit.rs`: new `Insn::BackRefMulti { groups, icase }`
  (0-based).
- `src/classicalbacktrack.rs`, `src/pikevm.rs`: scan `groups` in order for the
  unique participating one (the parser's duplicate-conflict check guarantees
  at most one), match against its range, or empty-succeed when none
  participated. `Match::named_groups()` dedup already preferred the
  participating range, so no API change.

## Patch B3: `Script=Unknown` / `Script_Extensions=Unknown` (`Zzzz`)

The generated property tables have no entry for the special script value
`Unknown` (alias `Zzzz`, UTS #24: code points not assigned to any script —
unassigned + surrogate + private-use), so `/\p{Script=Unknown}/u` was a
SyntaxError.

- `src/unicodetables_unknown.rs` (new, generated): `SCRIPT_UNKNOWN`, the
  complement of the union of every Script value table in `unicodetables.rs`
  (Unicode 17.0.0; 733 intervals / 954,246 code points — verified identical
  to test262's `Script_-_Unknown.js` expectation set). The union of all
  Script_Extensions tables equals the union of all Script tables, so one
  table serves both properties.
- `src/unicode.rs`: `unicode_property_from_str` maps
  `Script/Script_Extensions = Unknown | Zzzz` to that table before the
  normal table lookup.
- `src/lib.rs`: `mod unicodetables_unknown;`
