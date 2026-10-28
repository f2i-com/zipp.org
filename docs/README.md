# ZIPP documentation

ZIPP, the Zippy Interpreter for Polyglot Programming, is a portable polyglot
runtime: one virtual machine for JavaScript and Python today. The
[root README](../README.md) is the short introduction and download guide.
Everything else:

| Document | What it covers |
|---|---|
| [`ARCHITECTURE.md`](ARCHITECTURE.md) | How ZIPP works: one VM, two languages, from source text to machine code |
| [`REFERENCE.md`](REFERENCE.md) | The technical reference: build and workspace, command-line goals, conformance, frontend design, runtime and object model, tiers, embedding |
| [Embed kit](../crates/zipp-wasm/embed-kit/README.md) | Loading the WebAssembly engines and sandboxing programs: permissions, limits, Workers |
| [WebAssembly engines](../crates/zipp-wasm/README.md) | The engines' API, variants, host channels and resource limits |
| [`PROFILES.md`](PROFILES.md) | Which build to use: the JIT CLI, the hardened native runner, the WebAssembly engines |
| [`PERFORMANCE.md`](PERFORMANCE.md) | The canonical benchmark captures against Node, Bun, Deno and QuickJS, with the gaps |
| [`CORRECTNESS.md`](CORRECTNESS.md) | Test262 and language coverage |
| [`GPU.md`](GPU.md) | Torch on the browser GPU, the GPU lab, GPU graphs from JavaScript and Python |
| [`NODE.md`](NODE.md) | Running Node programs: Node's globals, npm packages, CommonJS, the core modules |
| [`python/RUNNING.md`](python/RUNNING.md) | Running Python projects |
| [`python/FRONTEND.md`](python/FRONTEND.md) | Python on ZIPP: scope, integration contract, compiler and runtime |
| [`python/TORCH_COMPATIBILITY.md`](python/TORCH_COMPATIBILITY.md) | The supported `torch` surface |
| [`LANGUAGES.md`](LANGUAGES.md) | How the languages share one engine: the base bytecode, what a language overrides, adding a language |
| [`LANGUAGE_INTEROP.md`](LANGUAGE_INTEROP.md) | Opt-in Python / JavaScript interoperability |
| [`DEVELOPMENT.md`](DEVELOPMENT.md) | Building, local verification, measuring a change, working rules |
| [`PERF_ROADMAP.md`](PERF_ROADMAP.md) | The current performance board and open targets |
| [`releases/`](releases/) | Release notes, one file per version |
| [`../bench/README.md`](../bench/README.md) | Benchmark suites, publication rules, the PGO boundary |
| [`../SECURITY.md`](../SECURITY.md) | Security model and reporting |
| [`archive/`](archive/README.md) | Dated history: the [previous README](archive/README-through-0.0.25.md), session logs, the experiment ledger, validation records, audits, design studies |

`assets/` holds the images and video the README and landing page use.