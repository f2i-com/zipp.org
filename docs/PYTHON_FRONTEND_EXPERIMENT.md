# Python on Zipp: scope and integration contract

Status: Zipp's own Python 3 implementation, integrated and validated on 13
September 2026 (Rust 1.92.0, Windows x86-64 native CLI and the wasm32
build). Python source is parsed with the RustPython *parser* (a parser only,
no second interpreter) and lowered by Zipp's own compiler straight to Zipp
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
| Numbers | arbitrary-precision `int`, `float` (Python `repr`/formatting rules, half-to-even rounding), `bool`; `+ - * / // % ** << >> & \| ^ ~`, chained comparisons, `divmod`, `round`, `pow` with modulus |
| Strings | full `str` method set, `%` formatting, `str.format`, f-strings with conversions and nested format specs, `bytes` (utf-8/ascii/latin-1 encode/decode), code-point indexing |
| Containers | `list`, `tuple`, `dict` (insertion-ordered, `__missing__`), `set`, `frozenset`, `range`, slices with steps and slice assignment/deletion, comprehensions (list/set/dict/generator), starred unpacking, `del` |
| Functions | defaults, keyword and keyword-only arguments, positional-only, `*args`/`**kwargs`, `*`/`**` at call sites, closures with `nonlocal`/`global`, lambdas, decorators, `__name__`/`__doc__`/`__defaults__`, generators (`yield`, `yield from`, `send`, `throw`, `close`, return value via `StopIteration.value`) |
| Classes | single and multiple inheritance (C3 MRO), `super()` (zero- and two-argument), `__init__`/`__new__`, instance and class attributes, `property` with setters/deleters, `classmethod`, `staticmethod`, descriptors, `__getattr__`/`__setattr__`/`__delattr__`, `__init_subclass__`, `__class_getitem__`, `__slots__` (accepted), subclassing `list`/`dict`/`tuple`/`set`/exceptions, every operator, comparison, container, iteration, call, context-manager and conversion dunder |
| Exceptions | the builtin hierarchy, `try`/`except`/`else`/`finally`, `raise ... from`, `__cause__`/`__context__`, bare `raise`, `with` (and multi-item `with`), `assert`, `SystemExit` |
| Statements | `if`/`elif`/`else`, `while`/`for` with `else`, `break`/`continue` through `try`/`finally`, `pass`, annotations (`__annotations__`), walrus `:=`, `global`/`nonlocal`, `import`/`from ... import` (including inside functions) |
| Modules | one module per `.py` file, `__name__ == "__main__"`, cycles resolved like CPython, `import x as y`, `from x import *`, `__all__` |
| Builtins | `print` (sep/end/file), `len`, `range`, `enumerate`, `zip` (strict), `map`, `filter`, `reversed`, `sorted`/`list.sort` (stable, key, reverse), `min`/`max` (key, default), `sum`, `any`/`all`, `abs`, `round`, `divmod`, `pow`, `isinstance`/`issubclass`, `hasattr`/`getattr`/`setattr`/`delattr`, `id`, `hash`, `callable`, `chr`/`ord`, `bin`/`oct`/`hex`, `format`, `repr`/`ascii`, `iter`/`next`, `type`, `object`, `dir`, `vars`, `globals`, `exit` |
| Built-in modules | `math`, `random` (seedable, deterministic), `time`, `sys`, `os`/`os.path` (a virtual empty filesystem), `io` (`StringIO`), `json`, `string`, `textwrap`, `copy`, `operator`, `itertools`, `functools` (`reduce`, `partial`, `lru_cache`/`cache`, `wraps`, `total_ordering`, `cmp_to_key`), `collections` (`Counter`, `defaultdict`, `deque`, `namedtuple`, `OrderedDict`, `ChainMap`), `heapq`, `bisect`, `statistics`, `re` (a JavaScript-backed subset with groups, named groups, `sub` with callables), `dataclasses` (`dataclass`, `field`, `asdict`, `astuple`, `replace`, `fields`; `order`, `frozen`), `enum` (`Enum`, `IntEnum`, `auto`), `typing` (names only), `abc`, `__future__` |
| Host `ui` module | buffered drawing commands (`canvas`, `clear`, `rect`, `circle`, `line`, `text`, `font`, `button`) and an input snapshot (`mouse`, `clicked`, `key`, `width`, `height`) for a host that renders them; see the wasm README and `crates/zipp-wasm/playground/` |

## What does not run yet

- `async`/`await`, `match` statements, type-parameter syntax, `except*`,
  complex numbers.
- Real files, sockets, processes, threads: `open()` and the `os` calls that
  need a filesystem raise `OSError`; `input()` raises `EOFError`. The sandbox
  has no filesystem by design.
- Metaclasses beyond `type` (a `metaclass=` keyword is called like `type` but
  custom metaclass machinery is not modelled), `__getattribute__` overrides,
  weak references, `__del__`, `__set_name__`.
- Tracebacks are one frame: an uncaught exception reports its type, message
  and the file/line of the raising statement, not the full call chain.
- `str` formatting of `float` uses Python's rules for `repr`, `f`, `e`, `g`,
  `%`; a few exotic spec combinations (`=` alignment with `0` padding of
  strings, `n` locale forms) are approximations.
- Performance: every operator and call goes through the runtime's dispatch;
  ints are BigInts. It is a correct interpreter first; dedicated bytecodes
  for the hot Python operations are the next step.

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

`compile_source(source, Frontend::Python { .. })` and
`compile_python_project(entry, &modules)` return a language-labelled handle
owning an ordinary `ScriptState`. The WASM `Engine` exposes
`initSource(source, "python")`, `initPythonProject(files, entry)`,
`pythonCall`, `pythonHas`, `takeUi` and `setPythonInput`; the JavaScript
global-slot, `callFunction` and `evalInContext` methods reject Python states.
Every host-boundary limit of a JavaScript state applies (initial source size,
instruction budget, heap, output, dynamic-code gates), plus the frontend's
compile-time caps (1 MiB per module, 2^20 tokens, 200 nested brackets, 100
indentation levels, 96 nested expressions, 8192 functions per program).

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
cargo test -p zipp-vm --features python --lib
cargo test -p zipp-cli
cargo run -p zipp-cli -- py examples/python/project
cd crates/zipp-wasm && ./build-variants.sh all && node tests/node/python-frontend.cjs
cd crates/zipp-wasm && node playground/smoke.cjs
```
