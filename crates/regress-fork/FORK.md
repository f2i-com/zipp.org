# zipp-regress — our fork of regress 0.11.1

The ECMAScript regex engine. This is a FORK, not a vendored copy waiting to be
dropped: `zipp-vm` calls `Regex::from_unicode_byteopt` (`src/api.rs`), which
does not exist upstream, so the crates.io crate does not compile against this
code at all. On top of that it carries seven correctness patches for bugs test262
hits (`built-ins/RegExp/regexp-modifiers`, `named-groups`, `property-escapes`,
and the `staging/sm/RegExp` unicode-flag cluster), plus the 15 September 2026
audit patches B8-B11 (a backtracking crash, the anchored sticky attempt,
lookbehind group names, and `[UnicodeMode]` grammar under `v`), and B12 (`v`
class set expressions).

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
Current priorities live in [`docs/PERF_ROADMAP.md`](../../docs/PERF_ROADMAP.md).

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

## Patch B12: `v`-mode class set expressions

QuickJS's `test_builtin.js` regexp block and probes against QuickJS and V8
found the `v` class grammar computing the wrong sets. Where V8 and QuickJS
disagree the spec decides: V8 keeps `A` out of `/[\q{BC|A}]/vi` and lets `B`
into `/[^\q{a|b}]/vi`, and QuickJS accepts `/[^[\q{ab}--\q{ab}]]/v`.

- `src/unicode.rs`, `src/parse.rs`: under `v` with ignoreCase every operand is
  simple case folded before `&&` and `--` run (MaybeSimpleCaseFolding), and a
  complement (`[^…]`, `\P{…}`, `\W`, `\D`, `\S`) is taken within the code
  points that fold to themselves (CharacterComplement over AllCharacters). The
  operators ran on raw sets, so `/[[a-c]&&B]/vi` matched nothing and
  `/[\w--k]/vi` matched `K`.
- `src/parse.rs`: a nested `[^…]` is complemented. The code called
  `inverted()` and dropped the result, so `/[[^b]]/v` was `[b]`.
- `src/parse.rs`: `ClassSet::strings` is MayContainStrings, syntactic as the
  spec defines it, and is what gates `[^…]`; the old check rejected any string
  property under a negated class, `/[^\p{RGI_Emoji}&&a]/v` included, and let
  `/[^\q{ab}]/v` through. One-code-point strings join the code points before a
  complement or a bracket (`/[^\q{a}]/v` is `[^a]`), `\q{}` and `\q{a|}` keep
  the empty string, and strings are tried longest first (`/[\q{a|ab}]/v`
  matches all of `"ab"`).
- `src/parse.rs`: a lone `&` after the first operand (`/[a&b]/v`) no longer
  drops that operand.

Tests: `tests/class_set_operations.rs`. Of the differential fuzzer's 306
recorded disagreements with Node (`fuzz/corpus.js`), 201 now agree and none
moved further away; their ZIPP lines are re-recorded.

## Compact Unicode property storage

The property tables retain their generated Interval literals but encode them
as gap/width varints during Rust const evaluation. Regex compilation decodes
directly into the CodePointSet allocation that previously copied the table;
matching still uses the same representation. ID_Start and ID_Continue stay
directly indexed for group-name parsing. No Unicode properties are removed.

After regenerating `unicodetables.rs` / `unicodetables_unknown.rs`, run
`python tools/pack_regex_tables.py` from the repository root. The generated
tests compare every decoded interval against its original literals.

## Compact UTF-16 matcher for ZIPP Lite

The optional `compact-utf16` feature shares one backtracker instantiation for
UTF-16 code-point and UCS-2 code-unit input. ASCII retains its specialized
executor. A surrogate mask selects the iteration mode without changing Unicode
case folding: a caller of the UTF-16 API still decodes pairs without a `u` flag,
and the UCS-2 API still reads individual units even with Unicode folding.
Subinputs and captured backreferences preserve that distinction.

In this feature only, mandatory, greedy and lazy single-character loops share
an outlined scanner. It bounds attempts by the remaining step allowance before
reading input, then charges exactly the attempted characters, including a final
mismatch. These matchers cannot re-enter the budget or allocate backtracking
state. A loop reaching exactly zero remaining steps does not signal exhaustion
until another attempt is requested. Used steps are derived from the initial allowance minus the remainder, removing
a redundant per-operation counter update. Backtracking and memory limits are
unchanged. Zero/one mandatory characters retain direct fast paths.

Full/native profiles retain their original separate input types and scalar
budgeted loops. ZIPP enables the new feature only through `wasm-lite`.
See [the fourth size audit](../../docs/archive/validation/2026-09-29-engine-size-round4.md)
for sizes, paired performance measurements and the differential probe. Unit tests
cover every individual code unit, surrogate boundaries, forward/backward stepping,
subinputs and scalar-equivalent budget boundaries (including prior exhaustion
and saturating usage counters).

## `ID_Start` / `ID_Continue` for the host's lexer

`regress::is_id_start` and `regress::is_id_continue` (lib.rs) answer from the
tables behind `\p{ID_Start}`, `\p{ID_Continue}` and a named group's name.
ZIPP's JavaScript lexer calls them instead of linking a crate of its own for
the same two properties, so a module carries each property once and the
lexer, the regex properties and group names are on one Unicode version.

## Alternation is a shallow tree, with no limit on alternatives (issue #57)

Upstream builds `a|b|c|…` as a right spine, `Alt(a, Alt(b, Alt(c, …)))`, and
the optimizer's IR walks (`ir::walk_mut`, `contains_capture_groups`, the
start predicate, `Drop`) recurse down it. A spine is as deep as the
alternation is long: a 5,000-word list overflowed a 1 MiB stack, 10,000 words
a 2 MiB one. The hardened profile had already switched to a balanced tree,
but kept a limit of 64 alternatives from before it did, so keyword lists and
tokenizers were SyntaxErrors there.

- `src/parse.rs`: `make_alt` folds each run of `ALT_SPINE_LEAVES` consecutive
  alternatives into a right spine and joins the spines in a balanced tree,
  keeping their order (the leftmost alternative that leads to a match still
  wins). The ordinary profile uses 64, so an alternation of up to 64 is the
  upstream tree, instruction for instruction and step for step; the hardened
  profile uses 1, the fully balanced tree it already had. The limit on
  alternatives is gone in both: the depth is at most 64 + log2(n).
- `src/optimizer.rs`: `propagate_early_fails` asked `contains_capture_groups`
  of every node before looking for anything to replace, walking the whole
  subtree each time, so the pass was quadratic in the depth of the tree
  (20,000 alternatives took 682 ms). It now asks only when it has found an
  always-failing child, with the same answers.
- `tests/compile_limits.rs`: 65 to 50,000 alternatives, their order, capture
  numbering across them, and a 1 MiB stack.

## Unrolling is budgeted, and a program can be capped while it is emitted (issue #58)

`unroll_loops` (upstream) replaces a loop with a minimum of one to five by
that many copies of its body and the loop, and a loop unrolled inside
another is copied again with every copy of the enclosing body. The program
grew by `min + 1` per level of nesting: 20 levels of `(?:…|)+` took 2 s to
compile, 31 levels never finished, and the copies took the loop count past
the 65,535 that the emitter's 16-bit loop ids number. With overflow checks
(the hardened build) that was a panic at `next_loop_id += 1`; without them,
two loops shared one id's iteration state. The empty alternative played no
part: `(?:(?:a)+)+…` doubled the same way, and `{5}` grew five times per
level.

- `src/optimizer.rs`: an `UnrollBudget` per regex. The copies may cost at
  most 1,024 nodes plus five times the regex's own size (a bracket counts its
  intervals too, since the copy clones its set), which every regex without
  nested unrolled loops fits, and may add only the loops the 16-bit ids have
  left. A loop the budget cannot pay for stays a loop: rolled and unrolled
  match the same way. The program is now linear in the pattern.
- `src/emit.rs`: `emit_limited` counts the bytes it emits, by length, the way
  `CompiledRegex::resident_bytes` counts them by capacity, and stops with
  `EmitError::ProgramTooLarge` once over its limit; a loop id that would wrap
  is `EmitError::TooManyLoops` (reported as "Loop count limit exceeded")
  instead of an overflow.
- `src/api.rs`: `CompileLimits`, `CompileError`, `Regex::from_unicode_with_limits`
  and `from_unicode_byteopt_with_limits`. ZIPP passes its compiled-program
  cap, so an oversized program fails as soon as it outgrows the cap rather
  than after it is finished; the count is a lower bound, so nothing the
  VM's own check would accept is refused.
- `tests/compile_limits.rs`: program size linear in the depth for seven
  nested families, the 31-deep pattern from quickjs-ng#1221, loops that match
  empty against Node, programs unrolled in full and part-way against
  unoptimized ones, loop ids, and the limit.

## Removed: the PikeVM backend

Upstream ships a second, PikeVM-style executor (`src/pikevm.rs`, feature
`backend-pikevm`, on by default). Nothing in ZIPP ran it: the engine calls
the backtracking executor only, and LTO already dropped the PikeVM from
every artifact. Its one use was the crate's own tests, which ran each case
on both executors. It is gone, with its feature; the differential fuzzer
against Node (`fuzz/`) is the independent check now, and the patches above
that mention `src/pikevm.rs` changed a file that no longer exists.

## Right-to-left instructions: one matcher per input type

Upstream instantiates the backtracking matcher for each input type AND each
direction (`try_at_pos<Dir>`, `run_lookaround<Dir>`, every one-character
loop and matcher type under it), because a lookbehind's body runs right to
left. Here the direction is part of the instruction instead: inside a
lookbehind the emitter wraps every instruction whose effect depends on the
direction (consuming ones, backreferences, capture-group ends, one-character
loops) in `Insn::Back`. `try_at_pos` runs left to right and hands a wrapped
instruction to `back_step`, which runs it right to left; a right-to-left
one-character loop tests its body through `scm::Dyn`, one matcher for every
body type. Each instruction still costs one step: the step counts in
`tests/step_counts.rs` are those of the per-direction matcher.

## Lazy one-character loops read ahead only as far as the match needs

Upstream ran a lazy one-character loop (`.*?`, `\w+?`) by scanning its
whole run to the maximum on entry and backtracking through it one position
at a time. The scan made `/<b>(.*?)<\/b>/g` quadratic over text without a
line terminator: each match read the rest of the subject. The loop now
takes its minimum, and each backtrack into it (`NonGreedyLoop1Char`,
holding the position reached and the iterations left) tests the body at
one more position. The regex JIT's lazy loops do the same. The positions
tried, and so every result, are unchanged; the steps charged are not (see
`tests/step_counts.rs`).

## Property names are tables, not `match`es

The generator wrote each kind of property (binary properties, General
Category values, Script values, properties of strings) as an enum, a
function mapping a name to it with a `match` over string literals, and a
function mapping it to its table with another `match`. The name `match`es
compiled to comparisons several times the size of the names (the Script
one alone was 7.7 KB of Lite's code). Each kind is now a byte string of its
values' names (each name a length byte and the name, a zero byte after each
value) and an array of tables in the same order;
`unicode::lookup_property_name` finds the value. After regenerating
`unicodetables.rs` and running `tools/pack_regex_tables.py`, run
`python crates/regress-fork/tools/pack_property_names.py`, then
`python crates/regress-fork/tools/derive_property_tables.py` (below), and
rustfmt the file.

## Derived and packed identifier tables

Nine property tables are unions of others: the General_Category groups (L,
LC, M, N, P, S, Z, C) of their categories, and Assigned the complement of
Unassigned. They are stored as `derived_intervals!`, the tables they are
made of and whether to complement, and built the first time a regex uses
them (with `std`, kept in a `OnceLock` per table: merging the parts again
for every compile made `\p{L}` compile 1.4 times slower); their interval
literals stay in the source for the round-trip test and for the
interval count the sandbox's expansion limit charges. ID_Start and
ID_Continue are packed like every other table: `unicode::is_id_start_cp`
and `is_id_continue_cp` (behind `regress::is_id_start` for the engine's
lexer, and a named group's name) decode them once per process and binary
search the result. `tools/derive_property_tables.py` makes both changes to
the generator's output, after `pack_property_names.py`.

## Searches build their executor in place

`MatchAttempter::new`/`new_with_limits`, `BacktrackExecutor::new`/
`new_with_limits`, and the ASCII and UTF-8 executors' `Executor::new` and
`with_limits` are `#[inline(always)]`. Out of line, each returned its value
(the attempter is about 100 bytes) through a stack temporary that the caller
then copied into place with 16-byte loads over the callee's 8-byte stores,
which the CPU cannot forward from the store buffer: a stall on every search,
the largest single cost of a one-character `re.test(ch)`. Inlining only the
attempter's constructors moved the copy up a level and was slower. With all
of them inlined, `re.test(ch)` on a shared regex takes 5.7% fewer cycles (a
fresh literal each time 2%) and the expr-compiler benchmark 1%.
