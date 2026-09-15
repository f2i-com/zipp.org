# zipp-regress — our fork of regress 0.11.1

The ECMAScript regex engine. This is a FORK, not a vendored copy waiting to be
dropped: `zipp-vm` calls `Regex::from_unicode_byteopt` (`src/api.rs`), which
does not exist upstream, so the crates.io crate does not compile against this
code at all. On top of that it carries seven correctness patches for bugs test262
hits (`built-ins/RegExp/regexp-modifiers`, `named-groups`, `property-escapes`,
and the `staging/sm/RegExp` unicode-flag cluster), plus the 15 September 2026
audit patches B8-B11 (a backtracking crash, the anchored sticky attempt,
lookbehind group names, and `[UnicodeMode]` grammar under `v`).

## Why not a different crate

There is no substitute. ECMAScript regexes require backreferences and
lookbehind, which rules out `regex` (rust-lang) — a finite-automata engine that
deliberately supports neither in order to guarantee linear time. `fancy-regex`
adds both but implements its own dialect, not ES semantics: no `/v` unicode
sets, different property-escape handling, and none of the `lastIndex`/sticky
protocol. `regress` is the only Rust crate written to the ECMAScript grammar,
which is why it was chosen. Replacing it means writing an engine. The former
Stage 5A plan and its ~10.7× regex gap belong to the
[archived performance ledger](../../docs/archive/PERF_LEDGER-B001-B252.md), not
the current baseline: the canonical `regex-log-scan` row is now 1.02× Node.
Current priorities live in [`PERF_ROADMAP.md`](../../PERF_ROADMAP.md).

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
still answer the wrong way round for U+017F and U+212A. (The `flags.unicode`
guards that meant `flags.unicode_mode()` are fixed by B11.)

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

## Patch B8: one-character loop backtracking cannot overshoot its bound

A greedy one-character loop records `min` (its start plus the minimum
iterations) and backtracks `max` toward it with the input's surrogate-aware
`next_left_pos`. When the search started on the trailing half of a surrogate
pair, `min` sits inside the pair: stepping left from just past the pair jumps
two units, over `min`, so the `*max == *min` stop never fired, and the next
step ran off the left end into `rs_unreachable!` — `unreachable_unchecked`
without `prohibit-unsafe`, observed as a native crash from
`/.*x/uy; re.lastIndex = 1; re.test("😀ab")` (a panic, and a WASM trap, with
it). The lookbehind direction mirrors it.

- `src/classicalbacktrack.rs`: `GreedyLoop1Char` and `NonGreedyLoop1Char`
  compare the cursors by order, and a step that would cross the other bound
  (or finds no position) clamps to it, so the bound itself is still tried
  once. Neither arm can reach `rs_unreachable!` any more.
- The VM also maps such a `lastIndex` to the pair's start for `u`/`v`
  regexes, so JavaScript never searches from mid-pair; this patch is what
  keeps a direct caller of `find_from_utf16` memory-safe.

## Patch B9: anchored single-attempt search (`match_at_*`)

The fork had no way to run the backtracker once at a given position, so the
VM's sticky (`y`) exec ran `find_from_*(text, start).next()` and discarded a
match that did not begin at `start`. A failing sticky exec therefore attempted
every later position first, and `RegExp.prototype[@@split]` — a sticky exec per
position — was quadratic in the gaps between separators.

- `src/classicalbacktrack.rs`: `BacktrackExecutor::match_at(offset)` makes the
  one attempt through the same `attempt_at` (native code where compiled) and
  `successful_match` as `next_match_anchored`, honouring the budget.
- `src/api.rs`: `Regex::match_at_{ascii,ucs2,utf16}` and their
  `_with_limits` forms returning `(Option<Match>, MatchUsage)`. The ASCII form
  always uses the classical executor, never the optional linear tier.

## Patch B10: capture-group names are keyed by group id

`Emitter` pushed each group's name in emission order, but `Match::captures`
is indexed by the parser's group id, and a lookbehind's body is emitted in
reverse (`ir::Node::reverse_cats`). Named groups inside a lookbehind therefore
swapped names: `/(?<=(?<int>\d+)\.(?<frac>\d+))USD/` reported `int` as the
fraction. Numbered captures and `\k<name>` (which use ids) were unaffected.

- `src/emit.rs`: the `CaptureGroup` arm stores the name at index `id`.

## Patch B11: `v` is a `[UnicodeMode]` grammar; two escape fixes

B4 made `\u` ask `flags.unicode_mode()`, but the other `[UnicodeMode]`
decisions still tested `flags.unicode`, so under `v` alone the parser took the
Annex B paths: `/a{/v`, `/]/v`, `/(?=a)+/v`, `/\a/v`, `/\1/v`, `/\c/v`,
`/\x1/v` and friends compiled instead of throwing.

- `src/parse.rs`: the term arms (`\c`, lookahead quantifiability, lone `{`,
  lone `* + ? ] { }`), the invalid braced quantifier, and the character-escape
  arms (`\x`, `\u`, legacy octal, identity escape), `\1`-`\9` and `\k` ask
  `unicode_mode()`. The `[...]` class-atom guards are left alone: under `v` the
  class grammar is `consume_class_set_expression`.
- `src/parse.rs`: `\b` inside a `v`-mode class is U+0008 (it produced `b`).
- `src/parse.rs`: an Annex B `\x` not followed by two hex digits is the
  identity escape `x`, and the characters after it are re-read as pattern text
  (`/\x1/` matches `"x1"`); they were consumed, so `/[\x1]/` was an
  "Unbalanced bracket" SyntaxError.
