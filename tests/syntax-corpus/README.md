# Real-program syntax corpus

Five reductions of shipped applications, kept here so a change to the hardened
profile's parse-shape limits cannot silently reject working code again.

That is not hypothetical. zipp v0.0.1 introduced `MAX_SAFE_SYNTAX_RECURSION`,
`MAX_SAFE_SYNTAX_CHAIN` and `MAX_SAFE_AST_NESTING` at 48 / 16 / 32 and broke two
shipped applications with them. The only thing standing between that change and
its users was a wasm smoke test that ran `console.log("zipp-web-release-smoke")`
— a source three tokens deep, which no nesting limit above zero can reject.

Every file here is a reduction of code that shipped, not an invented shape, and
each keeps the comment explaining why it is written at the depth it is. Their
provenance and the grammar shape each one contributes:

| file | reduced from | shape |
| --- | --- | --- |
| `else-if-dispatch.js` | an adventure game's hotspot dispatcher | a 26-arm `else if` ladder, one nesting level per arm |
| `state-size-sum.js` | a Game Boy emulator's save-state sizer | a 22-operand additive chain, and a member chain |
| `emulator-step.js` | the same emulator's sprite scanline renderer | hoisted branches around pixel loops, plus a `switch` decoder |
| `ui-layout.js` | a declarative screen builder | object/array literals nested as deep as the layout tree |
| `card-game.js` | a poker table's betting round | rule-shaped loop nesting, and a 17-operand weighted sum |

Four of the five are rejected outright by 48 / 16 / 32. The fifth,
`emulator-step.js`, is the near miss: it parsed under those limits with
single-digit headroom, and it is here because a limit set just above the corpus
is a limit that rejects the next sprite mode someone adds.

Two consumers read this directory, and both must keep passing:

- `crates/zipp-vm/tests/real_program_corpus.rs`, under `cargo test`, parses and
  compiles every file through the hardened native profile.
- `crates/zipp-wasm/tests/node/syntax-corpus.cjs`, under the release workflow's
  wasm job, parses every file through the actual shipped WebAssembly artifact
  with its 1 MiB linker stack — the configuration the limits are calibrated for.

Sources are `.js` rather than the host's own `.logic` extension because they are
ordinary ECMAScript; nothing here depends on the application that shipped them.

When adding a file, reduce something real and record where it came from. A
synthetic shape belongs in `crates/zipp-vm/tests/syntax_nesting_limits.rs`,
which is the other half of this pair: this directory proves the limits are not
too low, that one proves they are not too high.
