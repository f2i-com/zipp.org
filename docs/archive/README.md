# Documentation archive

This directory preserves dated project history without presenting it as the
current state. Figures here remain useful evidence for the commit and setup
they name; they must not replace the canonical capture or current status in the
current documents (see [`docs/README.md`](../README.md)).

| Archive | Scope | Status |
|---|---|---|
| [`HANDOFF-through-0.0.25.md`](HANDOFF-through-0.0.25.md) | Dated session log, newest first, from 13 September to the 0.0.25 release (15 October 2026) | Historical; its verification commands and working rules continue in [`DEVELOPMENT.md`](../DEVELOPMENT.md) |
| [`HANDOFF-through-B252.md`](HANDOFF-through-B252.md) | Cumulative continuation snapshots through the B252 campaign | Historical; continued by `HANDOFF-through-0.0.25.md` |
| [`PERF_LEDGER-B001-B252.md`](PERF_LEDGER-B001-B252.md) | Full append-only experiment ledger, including refutations and old gates | Historical evidence; current priorities are in [`PERF_ROADMAP.md`](../PERF_ROADMAP.md) |
| [`DOC-through-B252.md`](DOC-through-B252.md) | Previous long-form reference, including volatile performance diagnosis | Historical; replaced by [`REFERENCE.md`](../REFERENCE.md) |
| [`NURSERY_DESIGN.md`](NURSERY_DESIGN.md) | Design study and costing of the generational (nursery) collector | Implemented; the live design is in `heap.rs` and `vm/gc.rs` |
| [`HOSTILE-W30-W39.md`](HOSTILE-W30-W39.md) | Early hostile-corpus instructions and Wave 30/Wave 39 checkpoints | Historical; current suite guide is [`bench/hostile/README.md`](../../bench/hostile/README.md) |
| [`validation/`](validation/) | Dated validation records: CI readiness and the Test262 date-format check (14 September), the engine-size passes, rounds 1 to 16 (29 September to 14 October), and the 0.0.25 release validation | Historical; each release's summary is in [`docs/releases`](../releases/) |
| [`audits/`](audits/) | The 12 September 2026 correctness audit and its follow-up | Historical; their findings are fixed and pinned by the `audit_*` tests |

Superseded benchmark captures are condensed, one row each, into
[`bench/history`](../../bench/history/README.md) rather than kept here.

The repository history remains the ultimate source for earlier versions. New
archive files should state their date or commit range, why they were superseded,
and the current document that replaces them.
