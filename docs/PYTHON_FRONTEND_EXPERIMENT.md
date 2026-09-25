# Python on Zipp: scope and integration contract

Status: Zipp's own Python 3 implementation, integrated and validated on 13
September 2026 (Rust 1.92.0, Windows x86-64 native CLI and the wasm32
build). Python source is parsed by Zipp's own front end,
`crates/zipp-pyparse` (a byte-oriented lexer, an arena AST and a
recursive-descent parser for Python 3.13 syntax: PEP 701 f-strings, PEP 695
type parameters, `except*`; see its README), which lays each module out as a
tree in a bump arena. No RustPython code remains: the tree keeps the shape of
the RustPython 0.4 AST the compiler was written against, and syntax errors
keep that parser's messages. The tree is lowered by Zipp's own compiler straight to Zipp
register bytecode; Python's object model lives in a fixed JavaScript runtime
that Zipp compiles once per program. Guest Python is never translated to
JavaScript source and never sees the host's JavaScript globals.

Validation is differential: `tests/python_corpus/*.py` are run through the
local CPython and through Zipp, and stdout must match byte for byte. The
recorded CPython outputs are committed next to them and pinned by
`crates/zipp-vm/tests/python_corpus.rs`, so CI needs no CPython. On the
integration date every corpus program matched.

## What runs

| Area | Supported |
| --- | --- |
| Numbers | arbitrary-precision `int`, `float` (Python `repr`/formatting rules, half-to-even rounding), `bool`, `complex` (imaginary literals; the constructor with CPython's string parsing, keywords and the `__complex__`/`__float__`/`__index__` protocols; subclassing; CPython 3.13's arithmetic, `**` algorithm, equality with int/float, hash, `repr` and `format` specs); `+ - * / // % ** << >> & \| ^ ~`, chained comparisons, `divmod`, `round`, `pow` with modulus |
| Strings | full `str` method set, `%` formatting, `str.format`, f-strings with conversions and nested format specs, `bytes` (utf-8/ascii/latin-1 encode/decode), code-point indexing, `\N{...}` escapes for the common character names (see below) |
| Containers | `list`, `tuple`, `dict` (insertion-ordered, `__missing__`), `set`, `frozenset`, `range`, slices with steps and slice assignment/deletion, comprehensions (list/set/dict/generator), starred unpacking, `del` |
| Functions | defaults, keyword and keyword-only arguments, positional-only, `*args`/`**kwargs`, `*`/`**` at call sites, closures with `nonlocal`/`global`, lambdas, decorators, `__name__`/`__doc__`/`__defaults__`, generators (`yield`, `yield from`, `send`, `throw`, `close`, return value via `StopIteration.value`) |
| Classes | single and multiple inheritance (C3 MRO), `super()` (zero- and two-argument), `__init__`/`__new__`, instance and class attributes, `property` with setters/deleters, `classmethod`, `staticmethod`, descriptors, `__getattr__`/`__setattr__`/`__delattr__`, `__init_subclass__`, `__class_getitem__`, `__slots__` (accepted), subclassing `list`/`dict`/`tuple`/`set`/exceptions, every operator, comparison, container, iteration, call, context-manager and conversion dunder |
| Exceptions | the builtin hierarchy, `try`/`except`/`else`/`finally`, `raise ... from`, `__cause__`/`__context__`, bare `raise`, `with` (and multi-item `with`), `assert`, `SystemExit` |
| Statements | `if`/`elif`/`else`, `while`/`for` with `else`, `break`/`continue` through `try`/`finally`, `pass`, annotations (`__annotations__`), walrus `:=`, `global`/`nonlocal`, `import`/`from ... import` (including inside functions) |
| Modules | one module per `.py` file, packages by folder (`pkg/__init__.py` or a namespace folder; `import a.b.c`, `from pkg import submodule`), `__name__ == "__main__"`, cycles resolved like CPython, `import x as y`, `from x import *`, `__all__`; only the modules reachable from the entry through imports are compiled. A module that does not exist raises `ModuleNotFoundError` when the import runs, so `try: import numpy` / `except ImportError:` guards work |
| Builtins | `print` (sep/end/file), `len`, `range`, `enumerate`, `zip` (strict), `map`, `filter`, `reversed`, `sorted`/`list.sort` (stable, key, reverse), `min`/`max` (key, default), `sum`, `any`/`all`, `abs`, `round`, `divmod`, `pow`, `isinstance`/`issubclass`, `hasattr`/`getattr`/`setattr`/`delattr`, `id`, `hash`, `callable`, `chr`/`ord`, `bin`/`oct`/`hex`, `format`, `repr`/`ascii`, `iter`/`next`, `type`, `object`, `dir`, `vars`, `globals`, `exit` |
| Built-in modules | `math`, `cmath` (every function, with CPython's special-value tables, branch cuts and errors; `isclose`; the constants), `random` (CPython's Mersenne Twister with CPython's seeding, so seeded streams match; `Random`/`SystemRandom` instances, `getstate`/`setstate`), `time`, `sys`, `os`/`os.path` (a virtual empty filesystem), `io` (`StringIO`), `json`, `string`, `textwrap`, `copy` (including dict/list/set subclasses and the `__copy__`/`__deepcopy__`/`__reduce_ex__`/`__getstate__` hooks), `operator`, `itertools`, `functools` (`reduce`, `partial`, `lru_cache`/`cache`, `wraps`, `total_ordering`, `cmp_to_key`), `collections` (`Counter`, `defaultdict`, `deque`, `namedtuple`, `OrderedDict`, `ChainMap`), `heapq`, `bisect`, `statistics`, `re` (JavaScript-backed: groups, named groups and backreferences, inline and scoped flags, Unicode classes, `pos`/`endpos`, `sub` with templates or callables, `split`), `dataclasses` (`dataclass`, `field`, `asdict`, `astuple`, `replace`, `fields`; `order`, `frozen`), `enum` (`Enum`, `IntEnum`, `Flag`, `IntFlag`, `auto`), `typing` (generic aliases, `NamedTuple`, `TypedDict`), `abc` (`ABC` with abstract-method enforcement), `contextlib` (`contextmanager`, `suppress`, `closing`, `nullcontext`, `ExitStack`), `struct` (`pack`/`unpack`/`calcsize`/`iter_unpack`/`Struct` for the standard codes and byte orders), `hashlib` (`md5`, `sha1`, `sha256`, `sha512`), `platform`, `importlib` (`import_module` over the project), `io` (`StringIO`, `BytesIO`), `__future__` (`annotations`: PEP 563 string annotations, plus `X | None` union types) |
| Files | a virtual filesystem holding the project folder's files (the CLI and the playground load them; 8 MiB per file, and 64 MiB in total from the CLI or 16 MiB through the WebAssembly module the playground uses): `open()` in text and binary modes with `read`/`readline`/`readlines`/`write`/`seek`/`tell`/`truncate`, iteration and context managers; `os.listdir`/`makedirs`/`remove`/`rename`/`rmdir`/`getcwd`, `os.path`, `pathlib.Path` (`read_text`, `write_bytes`, `glob`, `rglob`, `mkdir`, ...); `sys.argv` from the host. Written files are reported back to the host (`__zipp_py_vfs_changed`), which the CLI copies to disk and the playground shows in its tree |
| Bundled Python-source modules | `zipp_gpu` (`crates/zipp-vm/src/frontend/python/lib/shared/zipp_gpu.py`): float32 compute graphs (`Graph`, `Tensor`, `submit`, `program`, `to_json`, `execute_locally`) and prepared sessions (`Graph.prepare` -> `Session` with `run`, `run_steps`, `download`, `dispose`: the step validated once, weights and optimizer state carried on the device, several steps per request); `pickle` (reads protocols 0-5, writes protocol 2, with `persistent_load`/`find_class`), `zipfile` (stored entries, zip64 reads, modes `a` and `x`), `pathlib`, `argparse`, `inspect` (signatures, including bound methods and `functools.partial`; `*args`/`**kwargs` and the positional-only `/` are missing, since the runtime's code records omit them), `pytest` (`raises`, `approx`, `mark.parametrize`/`skip`/`skipif`, `fixture`, `tmp_path`, `main`; a `test_*.py` entry runs its tests automatically and a failure exits non-zero); and a `torch` subset (below). Each is registered by name and compiled the first time it is imported (once per process; `crates/zipp-vm/src/frontend/python/lib/modules.txt` lists them), so a program pays nothing for the modules it never imports and `importlib.import_module` reaches any of them. `zipp_gpu.py` also runs under CPython, which is how the corpus checks it |
| `torch` subset | `crates/zipp-vm/src/frontend/python/lib/torch*.py` over the runtime's `_zipp_tensor` kernels (contiguous tensors on typed-array storages): dtypes, creation (`tensor`, `zeros`, `ones`, `full`, `arange`, `eye`, `randn`, `rand`, `randint`, `multinomial`, `randperm`), indexing and slicing (basic, advanced, boolean), shape ops (`view`, `reshape`, `permute`, `transpose`, `cat`, `stack`, `roll`, `unsqueeze`, `expand`, ...), elementwise and reduction ops with broadcasting (including `sin`, `cos` and `rsqrt`, and `repeat_interleave`, which together with `outer`/`cat`/`narrow` are what a rotary, grouped-query, RMSNorm decoder needs), `matmul`/`@`/`einsum`, `softmax`/`log_softmax`, comparisons, in-place ops, reverse-mode autograd (`backward`, `autograd.grad`, `no_grad`, `requires_grad`), `nn` (`Module`, `Parameter`, `Sequential`, `ModuleList`, `Linear`, `Embedding`, `Conv1d` with stride 1, `GRUCell`, `LSTMCell`, `LayerNorm`, `Dropout`, activations, the common losses), `nn.functional`, `nn.init`, `nn.utils.clip_grad_norm_`, `optim` (`SGD`, `Adam`, `AdamW`, `RMSprop`, `lr_scheduler.StepLR`), `manual_seed`/`Generator` (seeded and deterministic; sample streams are not guaranteed to match PyTorch's), `save`/`load` of state dicts and tensors in PyTorch's zip checkpoint format (checkpoints written by PyTorch load, and the ones written here load in PyTorch). Everything runs on CPU kernels inside the engine: eager execution is CPU-only; experimental `torch.compile` records supported GPU inference and dense training steps (relu/gelu/sigmoid/tanh, softmax, MSE or fused cross-entropy, SGD with momentum, Adam, AdamW) with asynchronous completion, and `compiled.prepare(x, target)` records a step once as a device-resident session (`step`/`steps` feed only the batch, weights and optimizer state carry on the device, `sync` writes them back into the model and `optimizer.state`, `dispose` frees them; at most 15 parameter tensors with Adam under the protocol's 64 outputs) (see [Torch compatibility](TORCH_COMPATIBILITY.md)) |
| Host requests | a program can hand its embedder plain-data work (`kind`, `payload`, a callback); the wasm engine exposes them through `takeHostRequests()` and answers through `pythonCall("__zipp_py_deliver", [id, reply])`. `zipp_gpu.Graph.submit` raises `gpu.execute`; `Graph.prepare` and its `Session` raise `gpu.session.create` / `run` / `download` / `dispose`, and the runtime's `_zipp_gpu.request` admits exactly those kinds. Compiled without a host (`zipp py`), `submit` and sessions evaluate with the library's own float32 reference implementation (under Zipp on the tensor kernels, carried tensors staying as storage between steps), so CPython and Zipp agree (`tests/python_corpus/ml_gpu_session.py`, `ml_gpu_prepared.py`) |
| Object model | metaclasses (`metaclass=`, inherited metaclasses, metaclass `__call__`/`__new__`, `type(name, bases, ns)`), `__init_subclass__` with class keywords, `__set_name__`, `__class_getitem__`, `__slots__` (attribute restriction), user descriptors (`__get__`/`__set__`), a live `obj.__dict__` that is a real `dict` (assignable; non-string keys are refused), function `__annotations__` |
| `match` | every pattern kind: literals and value patterns, singletons, captures and wildcards, `as`, `\|` alternatives, sequence patterns with a star, mapping patterns with `**rest`, class patterns with positional (`__match_args__`, self-matching builtins, named tuples, dataclasses) and keyword sub-patterns, guards |
| Tracebacks | an uncaught exception prints the full call chain, outermost frame first, with file, line and function names (long recursions are collapsed like CPython's `[Previous line repeated N more times]`), plus the chained cause or context |
| Host `ui` module | buffered drawing commands (`canvas`, `clear`, `rect`, `circle`, `line`, `text`, `font`, `button`) and an input snapshot (`mouse`, `clicked`, `key`, `width`, `height`) for a host that renders them; see the wasm README and `crates/zipp-wasm/playground/` |

## What does not run yet

- `async`/`await`, type-parameter syntax, `except*`.
- `complex`: `cmath` and `**` with a non-integral or large exponent take their
  transcendental functions from the engine's libm, so a result can differ from
  CPython's in the last bit. `abs()` of a complex, and the hypot that `**` and
  `cmath` use, is correctly rounded; CPython on Windows uses the C runtime's
  `hypot`, which is not, so there about 1 value in 12 differs by an ulp. There
  is no `numbers` module to register with.
- `\N{...}` knows a bundled table of names rather than all of Unicode
  (Latin-1, Greek letters, punctuation, currency, arrows, mathematical
  operators, box drawing, symbols, dingbats, common emoji, the control and
  format character aliases, and `CJK UNIFIED IDEOGRAPH-XXXX`), matched
  case-insensitively as CPython does. Any other name is a SyntaxError that
  says so; spell the character or use `\u`/`\U`.
- Sockets, processes, threads, and any file outside the project folder:
  the filesystem a program sees is the virtual one its host loaded (the
  CLI copies written files back under the project folder when the run
  finishes; the wasm engine only reports them). `input()` raises `EOFError`.
- Parts of `torch`: the blocked sparse layouts (`torch.sparse_bsr`/
  `sparse_bsc`), FX graph-mode quantization and some quantized modules,
  multi-process `torch.distributed` (one process, world size 1, works), CUDA
  devices (the GPU is reached through `torch.compile`; see
  [Torch compatibility](TORCH_COMPATIBILITY.md)) and full PyTorch compiler
  support are missing, and float64 accumulations differ from PyTorch's
  float32 kernels in the last bits.
- `__getattribute__` overrides, weak references, `__del__`.
- `int` values are limited by the engine's BigInt size cap (2^30 bits; 2^20
  in the hardened profile). Any operation whose result would exceed it,
  including `**` and `<<` (checked before computing), raises
  `OverflowError: Maximum BigInt size exceeded`.
- `time.sleep` blocks in a standalone run but is a no-op under an embedding
  host (the wasm engine), so the host's worker is never held; `perf_counter`,
  `monotonic` and `process_time` read the engine's monotonic clock.
- `enum`: `IntEnum`/`IntFlag` members are not `int` instances, and data-type
  mixins such as `class S(str, Enum)` do not build their members through
  the mixin type (a user `__init__` does receive each member's value).
- `re` runs on the engine's JavaScript regex with Python's syntax, flags and
  `\w`/`\d`/`\s` sets translated; `\W`/`\S` inside a character class are
  approximations, and `Pattern.match(s, pos)` that fails still scans to the
  end of the string (the engine's sticky match is not anchored yet).
- Iteration order of sets follows insertion order, except that a set of
  small non-negative ints iterates ascending as CPython's hash table does;
  a set of strings can print in a different order from CPython (whose
  string hashes are randomised per process).
- A `float` is an unboxed number, so it has no identity of its own. CPython's
  containers compare identity before equality, which lets a NaN find *itself*
  (`x = float("nan"); x in [x]` is True there); here that is False. Distinct
  NaNs behave as CPython does: never equal, and separate dict and set keys.
- `str` formatting of `float` uses Python's rules for `repr`, `f`, `e`, `g`,
  `%`; a few exotic spec combinations (`=` alignment with `0` padding of
  strings, `n` locale forms) are approximations.
- Performance: Python compiles to the engine's register bytecode plus 27
  fused Python-only instructions (arithmetic and compare-and-branch, class
  guards, instance-dict, global, method and class-attribute reads through
  per-VM, per-site caches keyed by class version, attribute and
  method lookup, subscripts, `len`, `isinstance`, unpacking, sequence
  displays, raise and handler entry, the generator loop step) and native
  helpers for json, the common `str` methods, `str %`, `hash`, `heapq`,
  `bisect`, generator steps and iterator steps; every one defers to the
  JavaScript runtime whenever it cannot answer exactly. On the command line
  (`zipp py`/`zipp run`, no instruction budget) hot Python loops (1024
  iterations) also compile to native code: float and small-int arithmetic,
  comparisons and `range` counters run inline, and every other fused
  instruction calls the interpreter's own step, so results are identical.
  Attribute reads and writes on an instance whose attributes are in
  layout-mode storage also run inline, guarded by the class's version and the
  storage's layout; any change runs the ordinary step (a
  153-program corpus is byte-identical JIT on/off). Loops over generators also compile (each
  generator step runs on a nested interpreter loop, as a call from compiled
  code does), and `try`/`except`/`finally` inside a hot loop compiles with its
  normal completion inline. Loops that spend most of their time in calls, and
  whole functions (`ZIPP_PY_TIERC=1` compiles them, measured slower on
  call-heavy code), stay interpreted; the tier is x86-64 only; embedders with
  an instruction budget, sandbox and WebAssembly keep the JIT off. Measured
  against CPython 3.13 over the 36 programs of `tools/python_bench.py`
  (geomean of work time): about 3x slower interpreted and 1.75x with the
  JIT, from `range_loop`, `int_arith`, `float_arith`, `tuple_swap` and
  `global_read` (faster than CPython with the JIT) to instance creation and
  list comprehensions (about 5-7x). Ints within ±2^46 are unboxed
  immediates in the value word (larger ones are heap BigInts), and dicts, sets and instance attributes live in native tables
  (`vm/py_table.rs`, with hidden-class layouts in `vm/py_table_layout.rs`). `ZIPP_PY_PROF=1` prints fused-op hit and slow-path counts, runtime
  helper call counts and the allocation mix at exit.
- Errors: uncaught exceptions print CPython's traceback (file paths relative
  to the project, no caret lines) and exit with status 1; `e.__traceback__`,
  `sys.exc_info()[2]` and the `traceback` module give CPython's frames and
  lines, with CPython's "Did you mean" suggestions; `f.__defaults__` and
  `f.__kwdefaults__` can be assigned.

## Language selection

The compiler always receives an explicit `Frontend`. CLI detection is
separate:

```text
explicit selection > recognized filename extension > recognized shebang > leading directive > error
```

Extensions: `.js`, `.cjs`, `.mjs`, `.py`, `.pyw`. `.mjs` routes through the
existing CLI module command. Bare source such as `x = 1` never silently
defaults to Python or JavaScript. `zipp py FILE` forces Python; `zipp py DIR`
runs a project whose entry is `main.py`.

## Design

`crates/zipp-vm/src/frontend/python/`:

- `symtable.rs` — CPython's scoping rules: locals, cells, free variables,
  globals, class-body namespaces, the implicit `__class__` cell for
  `super()`, and PEP 572 walrus targets inside comprehensions.
- `emitter.rs`, `stmts.rs`, `exprs.rs` — the compiler. Every code object is a
  `FuncProto` with a fixed ABI: register 0 is the Python function object,
  register 1 the array of bound positional values the runtime's `bind`
  produced. Locals are registers, captured locals are cell objects, free
  variables come off `this.cells`, module globals are a `Map` on
  `this.globals`. Control flow, calls and name access are VM instructions;
  `try`/`finally` and `with` use the VM's own handler and finally machinery,
  so `return`/`break` inside them behave. Everything else is a call into the
  runtime helper object.
- `runtime/core.js` — types, MRO, the attribute/descriptor protocol,
  exceptions, functions and argument binding, generators, modules,
  `with`, `super`, class creation.
- `runtime/types.js` — numbers, comparison, hashing (dicts and sets are
  bucketed by a canonical key; user `__hash__`/`__eq__` honoured), the
  containers, indexing/slicing, `str`/`repr`.
- `runtime/builtins.js` — the builtin functions, type constructors and the
  method tables of the builtin types; `runtime/stdlib.js` — string formatting
  and the built-in modules; `runtime/entry.js` — the host hooks and the
  program entry.

A Python call is a direct VM `Call`, so Python recursion lives on the VM's
explicit frame stack and ends in a catchable `RecursionError`. Calls that
start *inside* the runtime (a dunder method invoked by an operator, a key
function passed to `sorted`, `__init__` from a constructor) re-enter the
interpreter natively, one level per nesting.

## Embedding and WASM

`compile_source(source, Frontend::Python { .. })`,
`compile_python_project(entry, &modules)` and the general
`compile_python_program(entry, &modules, &files, &argv, hosted)` (dotted
module names, the virtual filesystem's files as root-relative paths and
bytes, `sys.argv[1:]`) return a language-labelled handle owning an ordinary
`ScriptState`. The WASM `Engine` exposes `initSource(source, "python")`,
`initPythonProject(files, entry, argv)` (paths to text or `{base64}`
contents), `pythonCall`, `pythonHas`, `takeUi`, `setPythonInput`,
`takeHostRequests`, and the written-file report through
`pythonCall("__zipp_py_vfs_changed", [])`; the JavaScript global-slot,
`callFunction` and `evalInContext` methods reject Python states.

### The CLI's project runs

`zipp py FILE|DIR [ARGS...]` (and `zipp run FILE` for a file detected as
Python by its extension, shebang or directive) runs the script as the entry
of a project:

- **Root.** The current directory when the script is inside it (as
  `python path/to/script.py` sees the tree from where it is run), otherwise
  the script's own folder. A filesystem root or the home folder is where
  scripts are run from rather than a project, so a script below one is
  rooted at its own folder. `zipp py DIR` roots at `DIR` and runs its
  `main.py`; `--bc` takes a file, never a folder.
- **Entry.** Any file name runs, with `__name__ == "__main__"` and
  `__file__`/`sys.argv[0]` its root-relative path: `my-script.py`,
  `2024_report.py`, an extensionless shebang script, or a script under a
  dot-folder or a skipped folder. A file name that is a module name is also
  importable by it. Its folder comes first for bare module names, as
  `sys.path[0]` does: `sub/util.py` shadows a root `util.py` for a script
  in `sub/` (the shadowed file stays readable).
- **Files loaded.** The script's folder first, then the tree breadth-first,
  skipping links and reparse points, every folder whose name starts with a
  dot, and `__pycache__`, `node_modules`, `target`, `venv` and `dist`. Up to
  8 MiB per file and 64 MiB in total, source files first; a `.py` file is
  a module up to 1 MiB, and up to 256 project modules are importable
  (bundled library modules do not count). The walk stops
  after 20,000 entries. Files and folders that are over the limits or
  cannot be read are left out with a note on stderr, not an error.
- **Output.** Console lines are written as the program produces them, with
  stdout and stderr in the order written.
- **Standard input.** `zipp --lang=python -` has no project folder; its
  file writes are discarded with a warning.

### CPU Torch compatibility evidence

The checked-in [Torch regressions](../crates/zipp-vm/tests/python_torch.rs)
exercise learning with Adam, tensor gradients, recurrent cells and checkpoints.
The [Conv2d fixture](../crates/zipp-vm/tests/fixtures/torch_conv2d.py) checks forward
values, input/weight/bias gradients and an optimizer update against PyTorch CPU.
These are bounded compatibility examples, not a claim that arbitrary research
projects or native Torch dependencies work unchanged in every host.
See [Torch compatibility](TORCH_COMPATIBILITY.md) for integer precision,
view/stride differences and the CPU/GPU boundary.

Every host-boundary limit of a JavaScript state applies (initial source size,
instruction budget, heap, output, dynamic-code gates), plus the frontend's
compile-time caps (1 MiB per module, 2^20 tokens, 200 nested brackets, 100
indentation levels, 96 nested expressions, 8192 functions per program).

In the hardened wasm build, a generator resumed by a builtin (`sum(x for
...)`, `"".join(...)`, a `for` over a generator `__iter__`), a `sorted` key
function and similar callbacks each run in a nested interpreter loop, and
the profile caps that nesting at 32 levels (`MAX_RUN_LOOP_DEPTH`, measured
against the 16 MiB shadow stack and V8's machine stack in
`crates/zipp-wasm/tests/node/native-depth.cjs`); deeper nesting is a
catchable `RecursionError`. The runtime keeps its own hot paths (sort keys,
`list.count`/`index`/`remove`, dataclass `__eq__`/`__repr__`, `in` over
sequences) on plain loops so that ordinary user recursion through them does
not spend that budget, and the `torch` subset iterates tensors through an
iterator object rather than a generator for the same reason.

This is **not a security-reviewed sandbox frontend**. Keep the existing safe
build profiles and process/Worker isolation, never enable filesystem,
networking or host capabilities implicitly, and run untrusted programs in
disposable Workers with host-controlled deadlines.

## Validation commands

```text
py -3 tools/python_corpus.py                        # differential run against local CPython
py -3 tools/python_corpus.py --write-expected       # refresh the recorded outputs
cargo test -p zipp-vm --features python --test python_corpus
cargo test -p zipp-vm --features python --test python_frontend
cargo test -p zipp-vm --features python --test python_project
cargo test -p zipp-vm --features python --test python_torch
cargo test -p zipp-vm --features python --lib
cargo test -p zipp-cli
cargo run -p zipp-cli -- py examples/python/project
cargo run -p zipp-cli -- py path/to/lab/tests/test_lab.py      # a folder's test file, with its packages and data
cd crates/zipp-wasm && ./build-variants.sh all && node tests/node/python-frontend.cjs && node tests/node/python-gpu.cjs
cd crates/zipp-wasm && node playground/smoke.cjs               # includes a lab-like folder: subfolders, a binary, arguments, written files
```

## Project file changes and native containment

`__zipp_py_vfs_changed` returns JSON with `version: 1` and a `changes` array.
Each entry is either `{ "path": "file", "base64": "..." }` (including an empty
base64 string for a zero-byte file), or `{ "path": "file", "deleted": true }`.
Rename reports deletion of the old path and a write to the new one. Reading the
hook drains the change set. Hosts must update their parser with the engine;
the old tab-separated protocol is no longer supported.

The CLI checks every change before it writes anything, applies deletions
first, and reports each change it refuses on stderr while the rest still
apply; a refused change fails the run, and when the program itself failed
too, its own error and traceback are still printed (after the refusals) and
decide the exit status. A change is refused when its path
is invalid on the host (Windows stream syntax `:`, for example) or when it
would replace a file that exists on disk but was not loaded into the
program: one in a skipped folder, over the size limits, or unreadable. The
program saw such a file as missing, so writing it back would destroy
contents the program never read; a new file in a skipped folder is written
normally. The virtual filesystem is case-sensitive. On a case-insensitive
disk (Windows, macOS) spellings that differ only in case are one file when
written back: the last write wins, and a case-only rename keeps the file
under its new spelling (the last one written, when there are several).

The native CLI accepts `.py`/`.pyw` case-insensitively for project entry files and
module discovery. It skips symlinks and Windows reparse points while collecting
files and refuses write/delete targets containing them. Every existing path
component is checked; lexical parent traversal, absolute paths and Windows stream
syntax are rejected. Reads are bounded even if a file grows during ingestion.
This is the trusted CLI, not a filesystem sandbox against another process changing
links concurrently. Use process/OS isolation for adversarial filesystem writers.
The browser receives only files supplied by its host and never writes to native disk.
