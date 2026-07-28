# zipp-regress — our fork of regress 0.11.1

The ECMAScript regex engine. This is a FORK, not a vendored copy waiting to be
dropped: `zipp-vm` calls `Regex::from_unicode_byteopt` (`src/api.rs`), which
does not exist upstream, so the crates.io crate does not compile against this
code at all. On top of that it carries six correctness patches for bugs test262
hits (`built-ins/RegExp/regexp-modifiers`, `named-groups`, `property-escapes`,
and the `staging/sm/RegExp` unicode-flag cluster).

## Why not a different crate

There is no substitute. ECMAScript regexes require backreferences and
lookbehind, which rules out `regex` (rust-lang) — a finite-automata engine that
deliberately supports neither in order to guarantee linear time. `fancy-regex`
adds both but implements its own dialect, not ES semantics: no `/v` unicode
sets, different property-escape handling, and none of the `lastIndex`/sticky
protocol. `regress` is the only Rust crate written to the ECMAScript grammar,
which is why it was chosen. Replacing it means writing an engine — that is
Stage 5A of PERF_ROADMAP.md, and it is also where the regex benchmark's ~10.7x
gap has to be closed eventually.

## Relationship to upstream

- **Upstream**: `regress` 0.11.1 (https://github.com/ridiculousfish/regress),
  Apache-2.0/MIT, copied from the crates.io registry.
- **Wiring**: a plain path dependency from `zipp-vm` under the package name
  `zipp-regress` (the *lib* is still `regress`, so `regress::` paths in the
  engine are untouched). It used to be wired through `[patch.crates-io]`, which
  pretended the dependency was the upstream crate; it is not.
- **Upstream's own test corpus was removed** (`pcre_tests.rs`, `tests.rs`,
  `unicode_property_escapes.rs`, `unicodesets.rs` — 1.2 MB): it is
  PCRE/upstream-shaped coverage, and the 2,063 RegExp files in test262 exercise
  the ES semantics this engine actually has to satisfy. The smaller targeted
  suites (escape/pattern/replacement/syntax-error/anchored) are kept.
- **Re-upgrading**: re-apply the patches below (or drop whichever has been
  upstreamed) and regenerate `unicodetables_unknown.rs`.

All patches are intended to be upstreamable.

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

## Patch B4: `[UnicodeMode]` is a real grammar parameter for `\u`

`try_escape_unicode_sequence` applied the whole RegExpUnicodeEscapeSequence
grammar whatever the flags were, but three of its productions are
`[+UnicodeMode]`-only (22.2.1). Without `u`/`v` the only one that exists is
`u Hex4Digits`, so `/\u{2}/` is the identity escape `u` followed by the
quantifier `{2}` — it matches `"uu"` — and a lead escape followed by a trail
escape is two independent code units matched over UCS-2 input, not one astral
character. regress read both the braced form and the lead+trail pair
unconditionally, so those patterns compiled to a single astral `Char` that a
code-unit subject can never match.

- `src/parse.rs`: `try_escape_unicode_sequence` takes the parameter; the
  CharacterEscape call site passes `flags.unicode_mode()`, while the two
  RegExpIdentifier call sites pass `true` — a group NAME's production is
  `\ RegExpUnicodeEscapeSequence[+UnicodeMode]` regardless of the flags.

## Patch B5: non-unicode Canonicalize, and `v` canonicalizes like `u`

Two halves of one defect: regress canonicalized non-unicode `i` with a raw
`toUppercase`, and treated `v` as non-unicode for case.

- `src/unicode.rs`: `fold_code_point` implements Canonicalize step 9 — a code
  unit >= 128 whose uppercase mapping lands below 128 canonicalizes to itself,
  so `/s/i` must not match U+017F nor `/k/i` U+212A. Every consumer had to
  learn the same rule or it reintroduced the matches the guard removed:
  `unfold_uppercase_char` (the optimizer's `CharSet` lowering) builds its
  equivalence class through `fold_code_point` rather than the raw
  `TO_UPPERCASE` table, and `add_icase_code_points` — which pre-closes a
  BRACKET so the matcher can test raw membership — takes the mode and walks
  `TO_UPPERCASE` with the guard instead of `FOLDS` unconditionally (`/[s]/i`
  and `/[a-z]/i` matched U+017F).
- `src/api.rs`: new `Flags::unicode_mode()` (HasEitherUnicodeFlag). `v` picks a
  different *grammar* from `u` but the same *character model*, so every case
  decision asks it: input construction (`api.rs`, `classicalbacktrack.rs`),
  `fold_if_icase` (`parse.rs`), the optimizer walk, and the
  `WordBoundaryUnicodeICase` choice (`emit.rs`).

## Patch B6: `\W` inside a character class is complemented AFTER the case closure

WordCharacters (22.2.2.6.2) already contains the ignoreCase extras — under
`iu`, U+017F and U+212A canonicalize into `\w` — so `\W` is the complement of
the *closed* set. `make_bracket_class` (the standalone `\W` atom) had this
right; `add_class_atom` (`\W` inside `[...]`) inverted first and left the
closure to the `]` handler, which put the extras back and made `/[^\W]/iu`
reject U+017F.

- `src/parse.rs`: `add_class_atom` takes the effective `icase` plus the mode
  (`make_bracket_class` grew the mode too, for B5) and uses
  `make_bracket_class`'s order: positive set, closure, then invert.

Still wrong, and NOT covered by these patches: the `v`-grammar
`consume_class_set_expression` path builds `\W` the same inverted-then-closed
way this patch fixed for the `[...]` grammar, so `/[^\W]/vi` and `/[\W]/vi`
still answer the wrong way round for U+017F and U+212A. Separately, `\u`
followed by something that is not four hex digits is still accepted as an
IdentityEscape under `v` (`/\u/v` should be a SyntaxError) — the surrounding
arms test `flags.unicode` where they mean `flags.unicode_mode()`.

## Patch B7: unicode-mode backreferences compare per code point, not per code unit

`Utf16Input::subrange_eq` (the non-`i` backreference fast path) compared raw
UTF-16 unit slices, so a backreference whose captured text ended in a lone
lead surrogate matched the lead half of a surrogate pair at the match
position — `/foo(.+)bar\1/u.exec("foo\uD834bar\uD834\uDC00")` matched, where
ES 22.2.2.9 reads *characters* (code points) and the pair is a single code
point, so the match must fail (staging/sm/RegExp/unicode-back-reference.js).

- `src/indexing.rs`: in unicode mode `subrange_eq` walks both the reference
  range and the input with `cursor::next` (code-point-wise, like
  `backref_icase` already did); the unit-slice comparison is kept for the
  non-unicode path.
