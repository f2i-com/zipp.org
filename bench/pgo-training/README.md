# PGO training corpus

These programs exist only to train the release profile. Exact scored files are
never executed during training. `tools/pgo.sh` copies the ordered corpus,
scored provenance inputs, validators, and expected-output manifest into an
exclusive read-only staging tree. It validates and executes those staged bytes,
not mutable checkout paths.

A publication build additionally requires the invoking checkout to be an exact
clean `HEAD`. Both Cargo stages run from one private detached clone of that
commit whose source tree is made read-only; the original checkout is rechecked
before either profile or binary publication. `--validate-only` deliberately
supports a dirty development tree, but it cannot publish or attest a build.

`tools/pgo_corpus.py` rejects publication-tree paths, byte-identical copies,
module/dynamic-code dependencies, and normalized structural clones. It compares
token 10-grams and contiguous runs across whole programs, functions, class and
object blocks, and overlapping windows. An absolute long-run limit prevents an
exact fragment from being hidden by unrelated padding. Training source spelling
is deliberately fail-closed: ASCII with LF line endings only, with raw Unicode
escapes, all template literals, Annex-B HTML comments, and hashbangs denied.
Regex/division slash contexts that the conservative scanner cannot prove are
also denied before scanning any possible regex payload, and private names such
as `#return` are tokenized atomically rather than as keywords. This closes
alternate-identifier, BOM/Unicode-whitespace, comment-terminator, and
tokenizer-ambiguity evasions without claiming to implement a full second
JavaScript parser.

Literal-sensitive checks complement the normalized structure comparison.
Integer-valued decimal, exponent, and base spellings are canonicalized before
comparison; distinctive shared values above the 16-bit range are rejected
apart from a narrow class of ordinary round sizes, powers of two, and masks.
Ordered shift/value tuples, cooked strings of at least eight characters
(`"use strict"` excepted), and regular-expression bodies of at least four
characters must also be disjoint from scored inputs. This keeps number and
literal normalization from hiding a copied data or reducer kernel.

Do not add, translate, rename, resize, or mechanically derive a workload from a
scored benchmark. New training programs need independent code, data, topology,
and checksums. The current mechanism-level inputs include a fan-out/fan-in async
DAG, a quoted-CSV scanner whose boundaries are tuple records, and a separate
URI-template expander. They replace the former serial-promise and combined text
pipeline shapes, which were too close to scored control flow even though their
bytes differed. The text/data, CSV, and URI programs use independent checksum
families rather than the FNV-1a loop shared by several scored programs; the
validator rejects the FNV-1a constants and shared distinctive numeric kernels
from every training input.

Every training input must be self-contained, bounded, and deterministic. The
manifest declares exact stdout and stderr for all seven inputs. The six legacy
training micros were removed because they mechanically duplicated public micro
benches; every tracked JavaScript or module benchmark outside this directory is
now a scored provenance input.
`tools/pgo_training.py`
enforces those bytes, a 30-second timeout, and bounded output. It launches the
binary with the hidden `--pgo-training` host-policy flag, which leaves the main
program and JITs unchanged but rejects eval/Function-family compilation,
ShadowRealm evaluation and imports, `$262.evalScript`, `$262.agent.start`, and
filesystem module loading before secondary source is parsed or read. This flag
is a PGO provenance boundary, not a sandbox for hostile main source.

The instrumented run writes into an exclusive profile directory. The runner
requires exactly one new regular `.profraw` file per ordered input, records its
hash, rejects extras or mutations, and the script merges only that enumerated
set. Profile snapshots and the final binary are copied through exclusive sibling
temporaries and atomically published after regular-file/reparse and digest
checks. The spelling, similarity, runner, staging, manifest, and publication
policies plus their implementation bytes are bound into the PGO recipe hash.
Independent publication verification reads those recipe bytes and the complete
source-snapshot digest from the same clean commit's Git blobs. It therefore
matches the private LF snapshot even when a legitimate Windows checkout has
materialized other text files with CRLF.
The build identity also binds the selected Cargo, rustc, rust-lld, `cl.exe`, and
`lib.exe` driver bytes and identities. This is not a byte-complete MSVC/Windows
SDK identity: compiler backend DLLs, headers, and import libraries are scoped by
the validated developer-environment paths recorded by the recipe.

The filesystem controls address accidental replacement, ordinary build races,
and symlink/reparse clobber hazards: private unpredictable directories, lexical
`lstat`/identity/hash rechecks, explicit profile enumeration, and atomic replace
all fail closed. A hostile process already running as the builder account is
outside this boundary; it could also alter the compiler, tools, or process
memory and therefore requires an isolated build account or host.

This is an auditable anti-leakage boundary, not a claim that training and scored
workloads are statistically independent. Code review remains required whenever
a mechanism or training input changes.

Some runtime shortcuts recognise one publication function by literal source or
an equally narrow bytecode shape. This corpus deliberately does not clone those
functions merely to make their PGO counters hot. Such a shortcut remains
unprofiled here until it is generalised into a reusable engine mechanism.
