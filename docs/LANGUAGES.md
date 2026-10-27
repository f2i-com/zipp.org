# Languages

Zipp is one engine for several languages. The core is language-neutral in
structure and JavaScript's in meaning: one bytecode, one VM, one heap and
collector, one set of native tiers. A language is a frontend that compiles
its source to that bytecode, plus whatever it adds where its semantics are
not JavaScript's. JavaScript, TypeScript and Python are built in; this page
is how they fit together and how another language would.

## Layout

```text
crates/zipp-vm/src/
  lib.rs, embed.rs          the public API
  bytecode.rs               the instruction set (`Instr`) and `FuncProto`
  value.rs heap.rs shape.rs the value representation, objects, hidden classes
  vm/                       the interpreter, the GC, the inline caches, builtins
  codegen/                  the native tiers (x86-64; codegen_aarch64.rs)
  lang/
    mod.rs                  the `Language` trait and the table of languages
    js/                     JavaScript: parse/, compile/, front.rs, mod.rs
    ts/                     TypeScript: types.rs, expr.rs, decl.rs, class.rs,
                            lower.rs, elide.rs, config.rs, mod.rs
    python/                 Python: parser/ (a crate), frontend/, runtime/, lib/,
                            vm/, jit/, mod.rs
```

The module tree is older than the folders: each language's files keep the
module names the core uses (`crate::compile` is JavaScript's compiler,
`crate::vm::py_ops` is Python's instruction semantics), declared where they
always were with a `#[path]` into `lang/`.

## The base and what a language overrides

The base language is JavaScript. Every instruction of `Instr` outside a
language's own section means what ECMAScript says: `Add` is `+` with its
coercions, `GetProp` is [[Get]] with the prototype chain, `Call` is
[[Call]]. A language uses a base instruction wherever its semantics agree
and emits its own where they differ. That is the override: Python's
frontend emits `Move`, `LoadInt`, `Jump`, `Call`, `Return` and the handler
instructions as they are, and `PyArith`, `PyCompare`, `PyGetAttr`,
`PySetAttr`, `PyGetItem`, `PyGlobal`, `PyCallEntry` and the rest of
`Instr::Py*` where Python's arithmetic, comparison, attribute, item and
global semantics are not JavaScript's.

A language's instructions are executed in its own folder:

- **Semantics.** The main loop sends each of them out of line to the
  language's executor (`Vm::py_step` and `Vm::py_step_ext` in
  `lang/python/vm/`), which holds the instruction's full meaning. An instruction may also have a fast path
  inline in the main loop (an integer `PyArith`, say) that answers the
  common case and falls through to the executor for the rest; the fast path
  must agree with it exactly.
- **Runtime.** Whatever the instructions do not encode lives in the
  language's runtime. Python's is written in JavaScript (`runtime/*.js`) and
  compiled by the JavaScript frontend, so the object model, the builtins and
  the standard library run on the same VM as the program.
- **Native tier.** A language may teach the JIT its instructions
  (`lang/python/jit/`); one it does not teach is simply not compiled, and
  the function runs in the interpreter.

Instructions that only the interpreter sees, the superinstruction copy
(`bytecode::fuse_for_interpreter`) and the lite loop's encoding
(`vm/lite.rs`), are built at run time from what a frontend emitted and
need nothing from a language.

## Adding a language

1. **Its folder.** `lang/<name>/mod.rs` implements `Language` (names, file
   extensions, shebang commands, `compile`, `compile_to_text`) and declares
   the frontend. Add a `LanguageId` variant and the entry in `languages()`;
   detection, `--lang` and `zipp run` then know it.
2. **Its frontend** compiles source to a `Program` of `FuncProto`s using the
   base instructions, and runs on a `ScriptState` like any script.
3. **Its instructions**, only where the base ones mean something else: new
   `Instr` variants in a section of their own, appended after the existing
   sections (an instruction's position is its encoding in the bytecode
   codec and the code cache, so existing ones never move), one main-loop
   arm sending them to the language's executor in `lang/<name>/vm/`, and an
   entry in each exhaustive table the build checks (`remap.rs`,
   `plan_region`'s `instr_uses`; `tests/instr_uses_exhaustive.rs` fails
   until they are there).
4. **Its runtime**, in the language's folder, written in any language the
   engine already runs.
5. **A feature**, if it should be optional: everything but the table entry
   behind it, so a build without the language still recognises its files
   and says why it cannot run them (Python's `python` feature does this).

## Rules every language keeps

- The bytecode a frontend emits must not depend on whether the JIT is on: a
  metered run charges the same steps either way (see
  `tests/str_append_index.rs`). Interpreter-only rewrites belong in the
  superinstruction copy, which a recorder and the meter-only build turn off.
- An inline fast path never runs guest code. Whatever may (a dunder
  method, a getter) goes to the executor or the runtime, and through the
  VM's ordinary call protocol, which owns re-entrancy, the recursion limit
  and the GC's safe points.

## TypeScript

TypeScript is the one language that needs nothing past the parser. It is a
dialect of JavaScript's grammar: the JavaScript parser reads a `.ts`, `.mts`
or `.cts` file with `ParseOptions::typescript` on and calls into `lang/ts`
at each place the grammars part, and the tree that comes out is the one the
same program written in JavaScript gives. There is no transpile step and no
JavaScript text; the compiler, the VM and the JIT run it as they run
JavaScript.

- **Types are read and dropped** (`types.rs`): a recursive descent over
  tsc's type grammar that checks what it reads and builds nothing.
  Annotations, type parameters and arguments, `as`, `satisfies`, `!`,
  `interface`, `type`, `declare`, overload signatures and abstract members
  leave no trace.
- **Constructs with a run-time meaning are lowered** (`lower.rs`) to the
  tree of the JavaScript tsc emits: `enum` (constant-folded, reverse-mapped;
  a `const enum`'s members are inlined where read), `namespace`, constructor
  parameter properties, `import x = …` and `export = …`.
- **Imports used only as types are elided** (`elide.rs`), as tsc elides
  them; a `.ts` module's `export { T } from` of a name its dependency does
  not export (a type) is left out rather than a link error.
- **`tsconfig.json` decides class fields** (`config.rs`): with
  `useDefineForClassFields` (or a target of ES2022 or later) fields are
  defined on the instance; otherwise, and with no config, they are assigned
  in the constructor, which is tsc's default.
- **Modules resolve the way TypeScript names them**: `./x` finds `./x.ts`
  or `./x/index.ts`, and `./x.js` finds `./x.ts`.

The ambiguous prefixes (`(a): T => …` against `(a)`, `f<T>(x)` against
`f < T > (x)`, `<T>x` against `<T>(x) => …`) are settled as tsc settles
them, with the parser's snapshot as the fallback point. `zipp ts file.ts`
(or `zipp file.ts`) runs a file; `zipp ts-sweep <paths>` parses a corpus
and groups the failures. Not supported: `.tsx` (the engine has no JSX) and
`experimentalDecorators`' legacy decorator semantics (decorators are the
standard ones).
