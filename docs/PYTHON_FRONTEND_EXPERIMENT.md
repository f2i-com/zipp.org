# Python frontend experiment: scope and integration contract

Status: experimental, integrated and build-validated on 13 September 2026
(Rust 1.92.0, Windows x86-64 native CLI and both wasm32 variants). The
`frontend::` unit tests, the `python_frontend` integration tests, the full
`zipp-vm` unit suite with the feature on, the CLI suite, and the wasm crate's
unit tests pass; `examples/python/*.py` produce the CPython reference output
through `zipp py`. The subset below is still a subset: nothing here claims
CPython compatibility, and the security notes at the end still apply.

Where it lives: `crates/zipp-vm/src/frontend/` (language selection and the
`compile_source` API), `frontend/python/mod.rs` (the AST-to-bytecode emitter)
and `frontend/python/runtime.js` (the fixed JS helper bootstrap). The CLI's
`py`/`run`/`--lang` commands are `crates/zipp-cli/src/frontend_cli.rs`; the
wasm entry point is `Engine.initSource`. Cargo features: `zipp-vm/python`
(off by default), `zipp-cli/python` (on by default), `zipp-wasm/python` (off by
default; see the wasm README's build variants).

## Scope matrix

| Construct | Current source behavior |
| --- | --- |
| `int`, `str`, `bool`, `None` | Supported representations; integers use shared ZIPP BigInt storage with explicit prototype size limits |
| Floats, `/`, `**`, complex, bytes, f-strings | Rejected at compile time |
| `+ - * // %`, unary `+ - not` | Python-oriented semantic helpers for the supported value types |
| `and`, `or` | Short circuit and return an operand, not a coerced bool |
| Equality, ordering, membership | Numeric/string/sequence/range subset; Unicode ordering by codepoint |
| `is`, `is not` | Singleton/object identity only; non-singleton scalar identity raises NotImplementedError |
| Lists and tuples | Distinct wrappers; nested values, negative indexes, sequence concatenation/repetition |
| List mutation | Index stores and `.append`; name-target `+=` preserves list aliases |
| Augmented assignment | Name targets only; `+= -= //= %=`. Other forms reject, including `*=` |
| `list += iterable` | Only list-to-list currently; tuple/string/range RHS is outside this subset |
| Tuple/list assignment | Full outer unpack cardinality check before writes; no starred targets |
| `if`, `elif`, `else`, conditional expression | Direct bytecode branches |
| `while`, `for`, `break`, `continue`, loop `else` | Direct bytecode control flow plus private iteration helpers |
| Functions | Module-scope definitions, positional arguments, recursion, first-class function wrappers, implicit None return |
| Calls | Direct VM `Call` instructions after a helper arity check, so Python frames live on the engine's explicit frame stack: deep recursion is bounded by the VM's own limit (`RangeError`, catchable by the host) rather than native stack |
| Python function local binding | Assigned-name prepass, parameter locals, UnboundLocalError sentinel |
| Nested functions/closures | Rejected; no closure-cell lowering yet |
| Defaults, annotations, keywords, varargs, decorators | Rejected rather than silently ignored |
| `print`, `len`, `bool`, `str`, `abs`, `range`, `list`, `tuple`, `min`, `max`, `int` | Limited builtin implementations |
| Builtin defaults | `bool`, `str`, `list`, `tuple` require one argument in this milestone, unlike full Python |
| Exceptions | Named runtime errors and `assert`; no try/except/finally/raise or Python traceback mapping |
| Dictionaries, sets, slices, comprehensions | Rejected |
| Classes, descriptors, protocols, generators, async | Rejected |
| Projects and imports | A program is a project of modules (one per `.py` file) with an entry module; `import m`, `import m as n`, `from m import a as b` resolve project modules and the built-in `ui` module at compile time. Each module has its own namespace (`m.x` reads it). A module runs once on first import; cycles resolve partially, as in CPython. No packages, relative imports, `import *`, imports inside functions, stdlib or pip |
| `ui` module | Buffered drawing commands (`canvas`, `clear`, `rect`, `circle`, `line`, `text`, `font`, `button`) and an input snapshot (`mouse`, `clicked`, `key`, `width`, `height`) for a host that renders them; see the wasm README and `crates/zipp-wasm/playground/` |
| Host hooks | `__zipp_py_has/call/take_ui/set_input` globals let an embedder call entry-module functions (`draw`, `update`, `on_click`, `on_key`) and drain the `ui` buffer by slot; guest Python cannot name them |
| `min`, `max`, `int`, `list.pop` | Two-argument `min`/`max`, `int()` of ints, bools and decimal strings, and `pop()` of the last element |
| Python evaluation mode / REPL | Module execution only; no Python eval-in-context support |
| Python-to-JS/host interop | Not implemented; only captured print output crosses this prototype's boundary |

String rendering is a simple diagnostic representation, not a complete CPython
repr formatter. It always uses single quotes and does not implement every control
character/quote-selection rule. Functions render as `<function NAME>` without an
address. Range `len` is represented by the mathematical BigInt length and is not
limited by CPython's platform `sys.maxsize`; that compatibility decision remains
open. Object layouts and scalar identity are not CPython's layouts.

## Language selection

The compiler always receives an explicit `Frontend`. CLI detection is separate:

```text
explicit selection > recognized filename extension > recognized shebang > leading directive > error
```

Extensions: `.js`, `.cjs`, `.mjs`, `.py`, `.pyw`. `.mjs` routes through the existing
CLI module command. Bare source such as `x = 1` never silently defaults to Python
or JavaScript. There is no trial execution or fallback after a parse error.

Directives are restricted to the first eight leading comment lines, for example
`# zipp:language=python`. A directive inside code or a string does not select a
language. Shebang handling is intentionally conservative, not a full shell/env
parser. Existing `zipp js` and WASM `initScript()` stay JavaScript.

## Bytecode design

The runtime seed initializes a private name Map and frozen helper object, then
calls an empty placeholder function. After compiling that fixed seed with ZIPP's
JS frontend, the Python compiler replaces only the entry FuncProto's body and
appends Python functions. It retains the seed Program's global table, function
IDs and entry name_global metadata, without relocating existing JS jumps.

Modules cost nothing at the ABI: every module-level binding is stored under the
key `"module.name"` in the runtime's single name Map, and the emitter bakes that
key into each load/store (falling back to the bare builtin name on a load), so
two modules' `x` never meet. Each module's top level is compiled as a
parameterless function; the entry body registers them all, names the entry
module, and imports it. `import` at runtime returns the module object from the
table or runs the registered init once (inserting the object first, so a cycle
sees a partially initialised module rather than recursing).

Every Python string literal uses a pending-string Value in the **value constant
pool**. GetProp names use the separate string_constants index. These are different
index spaces and must not be substituted for each other.

Register allocation is monotonically increasing within a function, with checked
u16 bounds. No register reuse or typed specialization is attempted in v1.
Arguments are evaluated left-to-right, then copied to a contiguous call/array
register block. Python name loads never use a guest-controlled JS global slot.

Python helper calls cost considerably more than specialized bytecodes might.
No throughput claim is made. The purpose is to test frontend separation before
optimizing or changing the VM instruction ABI.

## Embedding and WASM

`compile_source(source, Frontend::Python { mode: PythonMode::Module })` returns a
language-labelled handle owning an ordinary ScriptState. Rust embedders can still
obtain the low-level state; that is a trusted-host API, not a Python interop layer.
They must not assume JS globals/eval APIs automatically become Python-aware.

The proposed WASM patch adds `Engine.initSource(source, "python")`.
`initScript(source)` delegates to `initSource(source, "javascript")`.
Python compiles independently of `preamble.js`. It keeps the existing initialization
instruction budget, heap/output caps, bridge configuration freezing and error
termination flow. Additional compiler/helper caps do not replace those limits.

For Python engines, existing JS global-slot access, callFunction, evalInContext
and evalInContextRich are explicitly rejected. Internal JS helper symbols are not
returned as Python globals. Event/host-queue hooks have no Python bindings in this
milestone. SoftN integration is **not complete** simply because initSource exists.

The old `zippProfile()` remains a profile of the established JavaScript guest
configuration; it is not Python capability discovery. The later capability API
should report languages, supported subsets, parser version and per-language host
bindings. Do not present the old profile as a Python conformance claim.

Do not manually edit a generated wasm-bindgen JS/type wrapper to advertise this
API. Rebuild the standalone WASM package to generate matching exports. Existing
precompiled packages in a copied checkout do not acquire this frontend.

## Limits and sandbox status

Prototype source limit: 64 KiB. Token limit: 8,192. Logical-line token limit: 256.
Bracket/indent nesting limit: 32. Emitter depth: 64. Function count including seed
helpers: 256. Per-function emitted instruction count: 65,536. Registers are checked
below the reserved maximum u16 index.

Helper allocation caps: 65,536 sequence items, 1,048,576 UTF-16 string units,
262,144 integer bits. These are coarse development limits, not Python language
semantics. Some operations allocate temporary results before a limit check.
The token preflight invokes the external lexer; these caps are not a proof of
bounded parser stack usage, allocation behavior, or compile-time CPU use.

This is **not a security-reviewed new sandbox frontend**. Do not deploy it for
hostile multi-tenant code. Keep the existing safe build profiles and process/Worker
isolation. Never enable filesystem, networking or host capabilities implicitly to
make a language test pass. Compile and run in disposable processes/Workers with
host-controlled deadlines during validation. Recycle the whole WASM instance
between independent jobs until lifecycle/heap behavior has been measured.

## Validation commands

```text
cargo test -p zipp-vm --features python --lib frontend::
cargo test -p zipp-vm --features python --test python_frontend
cargo test -p zipp-vm --features python --lib
cargo test -p zipp-cli
cargo test -p zipp-vm --features python --test python_project
cargo run -p zipp-cli -- py examples/python/fibonacci.py
cargo run -p zipp-cli -- run examples/python/semantics.py
cargo run -p zipp-cli -- py examples/python/project        # a folder: main.py + modules
cd crates/zipp-wasm && ./build-variants.sh && node tests/node/python-frontend.cjs
cd crates/zipp-wasm && node playground/smoke.cjs            # the playground in a real browser
```

All of these passed on the integration date (see Status). Interpreter-only
native builds (`--no-default-features --features python`) and JS regressions
with the feature off remain part of the ordinary matrix.

Audit pending-string constants, function IDs, jump/register bounds, local name
classification, left-to-right evaluation, chained comparison short-circuiting,
unpacking atomicity, recursion cleanup and cross-engine isolation. Add debug/GC
stress tests and parser fuzzing before expanding syntax.
