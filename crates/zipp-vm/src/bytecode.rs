//! Register bytecode.
//!
//! Each function compiles to a flat `Vec<Instr>` over a fixed register file
//! (`reg_count` slots, indexed `u16`). Registers — not a value stack — are the
//! addressing mode, because that is the form a register-allocating JIT maps
//! directly onto machine registers. Operands are register indices, small
//! immediates, or constant-pool indices.
//!
//! The instruction set is deliberately small and regular; it favours
//! three-address arithmetic so a value can stay in one place across a basic
//! block (and, later, across a call).

use crate::value::Value;

/// A register index within a function's frame.
pub type Reg = u16;

/// One bytecode instruction. Kept as a fieldful enum (not packed bytes) for v1:
/// the dispatch cost of a wide enum is negligible next to correctness clarity,
/// and the JIT will consume this same structured form rather than re-decoding
/// bytes.
#[derive(Clone, Debug)]
pub enum Instr {
    /// `dst = <constant pool[idx]>`
    LoadConst { dst: Reg, idx: u32 },
    /// `dst = <small integer immediate>`
    LoadInt { dst: Reg, val: i32 },
    /// `dst = undefined`
    LoadUndefined { dst: Reg },
    /// `dst = null`
    LoadNull { dst: Reg },
    /// `dst = true|false`
    LoadBool { dst: Reg, val: bool },
    /// `dst = src`
    Move { dst: Reg, src: Reg },

    /// `dst = globals[idx]`. Throws ReferenceError if the slot holds the
    /// never-declared sentinel (`Value::UNINITIALIZED`) — i.e. the name was
    /// referenced but never bound (`x` where no `var`/`let`/`function`/builtin/
    /// assignment ever defined it).
    LoadGlobal { dst: Reg, idx: u32 },
    /// `dst = globals[idx]`, but the never-declared sentinel reads as `undefined`
    /// instead of throwing. Emitted for `typeof <ident>`, where an unbound name
    /// must yield "undefined" rather than a ReferenceError.
    LoadGlobalOrUndefined { dst: Reg, idx: u32 },
    /// `globals[idx] = src`
    StoreGlobal { idx: u32, src: Reg },

    /// A clock read: `performance.now()` (`epoch = false`, fractional ms since
    /// VM start) or `Date.now()` (`epoch = true`, integer ms since the Unix
    /// epoch). Both yield an f64 `Value`. Recognised at compile time so the
    /// common timing idiom works without a real global object model.
    Now { dst: Reg, epoch: bool },

    // ── arithmetic (generic: operands may be any number) ──
    Add { dst: Reg, a: Reg, b: Reg },
    Sub { dst: Reg, a: Reg, b: Reg },
    Mul { dst: Reg, a: Reg, b: Reg },
    Div { dst: Reg, a: Reg, b: Reg },
    Mod { dst: Reg, a: Reg, b: Reg },
    Neg { dst: Reg, a: Reg },
    /// `dst = +a` — unary plus: coerce `a` to a number (ToNumber).
    ToNum { dst: Reg, a: Reg },
    /// `dst = a <bitop> b` — bitwise/shift with JS int32 coercion of the operands
    /// (`>>>` coerces to uint32 and may yield a value above i32::MAX).
    Bitwise { dst: Reg, a: Reg, b: Reg, op: BitwiseOp },
    /// `dst = a ** b` — exponentiation (f64 semantics).
    Pow { dst: Reg, a: Reg, b: Reg },
    /// `dst = ~a` — bitwise NOT (int32 coercion).
    BitNot { dst: Reg, a: Reg },

    /// `dst = a + <int immediate>` — the canonical `i + 1`, `n - 1` shape.
    AddInt { dst: Reg, a: Reg, imm: i32 },

    /// `dst = a + b` — SEMANTICALLY IDENTICAL to `Add` (same operator, same
    /// coercion). A pure JIT routing hint emitted by a compile pass for the
    /// `s = s + x` string-accumulator shape: it routes the op to the helper-call
    /// (memory) OSR region instead of the numeric region, so a hot `s += …` loop
    /// JITs its control flow natively and calls a concat helper per step rather
    /// than running fully interpreted. Because the semantics equal `Add`, a
    /// mis-applied hint can only change performance, never results.
    StrConcat { dst: Reg, a: Reg, b: Reg },

    /// `dst = a + b`, computed by appending `b`'s string form into `a`'s buffer
    /// IN PLACE when `a` is a uniquely-owned mutable string (else a fresh string).
    /// Emitted by the compile pass ONLY for a string accumulator it has PROVEN
    /// linear — `a` (a global) is built solely by this loop, never aliased during
    /// the build (no other read/store, no calls/heap ops in the loop, loop not
    /// nested, top-level so it runs once). Under that proof the in-place mutation
    /// is unobservable, turning a 1M-element `s += …` build from ~1M rope-node
    /// allocations into amortized buffer growth. Unlike `StrConcat`/`Add`, this is
    /// NOT semantics-preserving in general (it mutates) — correct ONLY under the
    /// linearity proof the emitter guarantees.
    StrAppendInPlace { dst: Reg, a: Reg, b: Reg },

    // ── comparisons → boolean ──
    Lt { dst: Reg, a: Reg, b: Reg },
    Le { dst: Reg, a: Reg, b: Reg },
    Gt { dst: Reg, a: Reg, b: Reg },
    Ge { dst: Reg, a: Reg, b: Reg },
    /// strict `===`
    Eq { dst: Reg, a: Reg, b: Reg },
    /// strict `!==`
    Ne { dst: Reg, a: Reg, b: Reg },
    /// loose `==` (with type coercion)
    LooseEq { dst: Reg, a: Reg, b: Reg },
    /// loose `!=` (with type coercion)
    LooseNe { dst: Reg, a: Reg, b: Reg },

    Not { dst: Reg, a: Reg },
    /// `dst = typeof a` (a JS type-name string).
    TypeOf { dst: Reg, a: Reg },
    /// `dst = Array.isArray(a)` — true iff `a` is a heap array.
    IsArray { dst: Reg, a: Reg },
    /// `dst = JSON.stringify(val, _, space)` — `space` is the indentation arg
    /// (a number → that many spaces, a string → that string, else compact).
    JsonStringify { dst: Reg, val: Reg, space: Reg },
    /// `dst = JSON.parse(a)` — parse a JSON string; throws SyntaxError on invalid.
    JsonParse { dst: Reg, a: Reg },
    /// Append to array `arr`: when `spread`, append every element of `val` (an
    /// array, or a string's chars); otherwise push `val` as one element. Used to
    /// build array literals / call-arg lists containing `...spread`.
    ArrayAppend { arr: Reg, val: Reg, spread: bool },
    /// `dst = [...src.slice(start)]` — the rest of an array (or a string's chars)
    /// from index `start`. Used by array destructuring's `[a, ...rest]`.
    ArrayRest { dst: Reg, src: Reg, start: u32 },
    /// Copy `src`'s own enumerable keys onto `target` (object literal `{...src}`).
    /// `src` may be an object, array, or string; null/undefined contribute none.
    ObjectSpread { target: Reg, src: Reg },
    /// `dst = { ...src } minus the keys` — object rest in destructuring
    /// (`let {a, ...rest} = src`). The excluded keys are `string_constants
    /// [exclude_start .. exclude_start+exclude_count]` (the sibling properties).
    ObjectRest { dst: Reg, src: Reg, exclude_start: u32, exclude_count: u16 },
    /// Like ObjectRest, but the excluded sibling keys are the `n` runtime values
    /// in registers `keys_base..keys_base+n` (each ToPropertyKey-coerced) — used
    /// when an object-rest pattern has a computed sibling key (`{[k]: a, ...r}`).
    ObjectRestDyn { dst: Reg, src: Reg, keys_base: Reg, n: u16 },
    /// `dst = <the class value for classes[class_id]>` — materialize a class.
    /// `parent` is the register holding the superclass value (`extends P`), or
    /// `None`; the new class links to it for inherited lookup + instanceof.
    MakeClass { dst: Reg, class_id: u32, parent: Option<Reg> },
    /// `dst = yield val` — suspend the current generator, handing `val` out as
    /// the yielded value. On resume the value passed to `.next(v)` lands in `dst`.
    Yield { dst: Reg, val: Reg },
    /// Generator body entry marker — emitted for a (sync) generator right after the
    /// parameter prologue (defaults + destructuring). FunctionDeclarationInstantiation
    /// runs eagerly at call time (so a destructuring throw or default side-effect
    /// happens at the call, not the first `.next()`); the generator is then created
    /// suspended AT this marker, and the first `.next()` resumes just past it to run
    /// the body. Behaves like a valueless `yield` that is consumed during construction.
    GenStart,
    /// `dst = await val` — suspend the async activation on the awaited value's
    /// promise; on resume the settled value lands in `dst` (a rejection is thrown
    /// in at this point so an enclosing `try`/`catch` sees it).
    Await { dst: Reg, val: Reg },
    /// for-of step: advance over `iter` (array/string/Map/Set positionally with
    /// the cursor in `idx`, or a generator via `.next()` ignoring `idx`). Writes
    /// the next element to `value_dst` and a bool to `done_dst`. Throws if `iter`
    /// is not iterable.
    IterNext { value_dst: Reg, done_dst: Reg, iter: Reg, idx: Reg },
    /// IteratorClose: invoke `iter`'s `return()` (if present) — emitted on the
    /// abrupt `break` exit of a `for-of` so a not-yet-exhausted iterator is closed.
    IterClose { iter: Reg },
    /// Resolve `src`'s ASYNC iterator into `dst`: `src[@@asyncIterator]()` if
    /// present, else `src[@@iterator]()` (a sync iterable used by `for await`),
    /// else pass `src` through (async generators / built-ins iterate directly).
    GetAsyncIterator { dst: Reg, src: Reg },
    /// `for await` step: writes the next RESULT to `dst` — a Promise (async
    /// iterator / async generator), or a `{value, done}` object (sync iterable,
    /// positional via the `idx` cursor). The loop then `await`s `dst`, so a sync
    /// `{value,done}` passes straight through and an async one suspends.
    ForAwaitNext { dst: Reg, iter: Reg, idx: Reg },
    /// `super(args…)`: run the lexical superclass's constructor contribution on
    /// the current `this` (reg 0). `home_class_id` is the class the method belongs
    /// to; its runtime `ClassData.parent` is the superclass to invoke (so an
    /// `extends <arbitrary expression>` parent resolves dynamically).
    SuperCtor { home_class_id: u32, arg_base: Reg, argc: u16 },
    /// `super(...args_array)`: like SuperCtor but spreads the elements of the
    /// array in `args` (`super(...xs)` in a derived constructor).
    SuperCtorSpread { home_class_id: u32, args: Reg },
    /// `dst = super.<name>(args…)`: call the named method found from the lexical
    /// superclass up its chain, with `this` = the current frame's `this` (reg 0).
    SuperMethod { dst: Reg, home_class_id: u32, name: u32, arg_base: Reg, argc: u16 },
    /// `dst = super.<name>`: read an inherited property (method value, or a getter
    /// invoked with `this` = the current frame's `this`) via the superclass's
    /// prototype.
    SuperGet { dst: Reg, home_class_id: u32, name: u32 },
    /// `dst = super[key]`: computed form of SuperGet (`key` is a register).
    SuperGetComputed { dst: Reg, home_class_id: u32, key: Reg },
    /// `dst = super[key](args…)`: computed form of SuperMethod.
    SuperMethodComputed { dst: Reg, home_class_id: u32, key: Reg, arg_base: Reg, argc: u16 },
    /// `super.<name> = val`: if the superclass prototype chain has an inherited
    /// setter, invoke it with `this` = the current receiver; otherwise create an
    /// own property on the receiver.
    SuperSet { home_class_id: u32, name: u32, val: Reg },
    /// `super[key] = val`: computed form of SuperSet (`key` is a register).
    SuperSetComputed { home_class_id: u32, key: Reg, val: Reg },
    /// `dst = new callee(args…)` — construct an instance. `callee` must be a
    /// class value; builds an object, installs the methods, runs the ctor.
    New { dst: Reg, callee: Reg, arg_base: Reg, argc: u16 },
    /// Append a computed instance-field KEY (already evaluated into `key`) to the
    /// class value `class`'s `computed_field_keys`, at class-definition time and
    /// in source order. Read later by `FieldInit`.
    PushFieldKey { class: Reg, key: Reg },
    /// `this[key] = val` for the `key_index`-th computed instance field, run in a
    /// constructor: looks up the key from the instance's class `computed_field_keys`
    /// (eval-once at class definition) and assigns. `this` is reg 0.
    FieldInit { key_index: u16, val: Reg },
    /// `dst = Array(args…)` / `new Array(args…)`: a single numeric arg makes an
    /// array of that length (holes → undefined); otherwise an array of the args.
    ArrayCtor { dst: Reg, arg_base: Reg, argc: u16 },
    /// `dst = new Map(src?)` — build a Map from an optional iterable of [k,v]
    /// entries (`src` register, or `None` for an empty map).
    NewMap { dst: Reg, src: Option<Reg> },
    /// `dst = new Set(src?)` — build a Set from an optional iterable of values.
    NewSet { dst: Reg, src: Option<Reg> },
    /// `dst = new WeakMap(src?)` / `new WeakSet(src?)` — like NewMap/NewSet but a
    /// distinct WeakMap/WeakSet type, and keys/values must be objects.
    NewWeakMap { dst: Reg, src: Option<Reg> },
    NewWeakSet { dst: Reg, src: Option<Reg> },
    /// `dst = new WeakRef(target)` — target must be an object.
    NewWeakRef { dst: Reg, target: Reg },
    /// `dst = new String/Number/Boolean(arg?)` — a boxed primitive wrapper.
    /// `kind` 0=String/1=Number/2=Boolean; `arg` is the (optional) argument register.
    NewBox { dst: Reg, kind: u8, arg: Option<Reg> },
    /// `dst = new FinalizationRegistry(cleanupCallback)` — callback must be callable.
    NewFinalizationRegistry { dst: Reg, cleanup: Reg },
    /// `dst = new Promise(executor)` — alloc a pending promise, call `executor`
    /// with its (resolve, reject) functions; a throwing executor rejects it.
    NewPromise { dst: Reg, executor: Reg },
    /// `dst = callee(...args_array)` — call `callee` (a function value) spreading
    /// the elements of the array in `args` as the arguments (`this` = undefined).
    CallSpread { dst: Reg, callee: Reg, args: Reg },
    /// `dst = obj[name](...args_array)` — method call spreading the elements of
    /// the array in `args` (`this` = obj). Handles builtin methods (e.g.
    /// `arr.push(...xs)`) and user methods alike.
    CallMethodSpread { dst: Reg, obj: Reg, name: u32, args: Reg },
    /// `dst = new callee(...args_array)` — construct `callee` spreading the
    /// elements of the array in `args` as the arguments.
    NewSpread { dst: Reg, callee: Reg, args: Reg },
    /// `dst = Math.<op>(args…)` — a builtin Math function over `argc` contiguous
    /// argument registers starting at `arg_base`.
    MathOp { dst: Reg, op: MathFn, arg_base: Reg, argc: u16 },
    /// `dst = <Number|parseInt|parseFloat>(args…)` — a builtin global function.
    GlobalFn { dst: Reg, op: GlobalFn, arg_base: Reg, argc: u16 },
    /// `dst = <static builtin>(args…)` — a constructor-namespace static method
    /// over `argc` contiguous arg registers (Object.assign, Array.of,
    /// String.fromCharCode, Number.isInteger/isNaN/isFinite/isSafeInteger).
    StaticFn { dst: Reg, op: StaticFn, arg_base: Reg, argc: u16 },
    /// `dst = Array.from(src[, mapfn])`. `mapfn` is a function value, or
    /// undefined-in-register when absent (the compiler loads undefined there).
    ArrayFrom { dst: Reg, src: Reg, mapfn: Reg },
    /// `dst = Math.<op>(...arr)` — a variadic Math reduction (max/min/hypot)
    /// applied to the elements of the array in `args`.
    MathSpread { dst: Reg, op: MathFn, args: Reg },
    /// `dst = (val instanceof <ctor>)` for a built-in constructor (Array, Object,
    /// Function, Error and its subclasses). User constructors are out of scope.
    InstanceOf { dst: Reg, val: Reg, ctor: InstanceCtor },
    /// `dst = (val instanceof ctor)` where `ctor` is a runtime class value: true
    /// when `val` is an instance whose class is `ctor`.
    InstanceOfDyn { dst: Reg, val: Reg, ctor: Reg },
    /// `dst = (key in obj)` — true when `obj` has the property `key` (own or, for
    /// a class instance, inherited; array indices / `length`; Map/Set `size`).
    HasProp { dst: Reg, key: Reg, obj: Reg },

    // ── control flow (targets are instruction indices) ──
    Jump { target: u32 },
    /// Jump if `cond` is falsy.
    JumpIfFalse { cond: Reg, target: u32 },
    /// Jump if `cond` is truthy.
    JumpIfTrue { cond: Reg, target: u32 },

    /// Fused compare-and-branch: `if !(a < b) goto target`. Keeps the common
    /// loop/recursion guard in one instruction so the boolean never has to be
    /// materialised into a register. RESERVED: fully handled by the interpreter
    /// and the JIT, but the compiler does not emit it yet (a planned peephole).
    #[allow(dead_code)]
    JumpIfNotLt { a: Reg, b: Reg, target: u32 },
    #[allow(dead_code)]
    JumpIfNotLe { a: Reg, b: Reg, target: u32 },

    // ── reference types ──
    /// `dst = <function object for functions[func_id]>`. Capture-free: used for
    /// functions that reference no enclosing variables.
    MakeFunc { dst: Reg, func_id: u32 },
    /// `dst = <closure over functions[func_id]>` capturing upvalue cells named
    /// by `functions[func_id].upvalues`. Each upvalue source is resolved in the
    /// CURRENT (defining) frame: either a local register that holds a cell, or
    /// one of the current frame's own upvalues (for nested-of-nested capture).
    MakeClosure { dst: Reg, func_id: u32 },

    /// Box the value currently in `reg` into a fresh heap Cell and write the
    /// cell reference back into `reg`. Emitted for a captured local/param so
    /// later reads/writes go through the shared cell.
    MakeCell { reg: Reg },
    /// `dst = *<cell in reg>` — read a captured local's cell.
    CellGet { dst: Reg, cell: Reg },
    /// `*<cell in reg> = src` — write a captured local's cell.
    CellSet { cell: Reg, src: Reg },
    /// `dst = *<upvalue[idx]>` — read one of this closure's captured cells.
    UpvalGet { dst: Reg, idx: u16 },
    /// `*<upvalue[idx]> = src` — write one of this closure's captured cells.
    UpvalSet { idx: u16, src: Reg },
    /// `dst = [reg[arg_base], …, reg[arg_base+argc-1]]` — array literal.
    NewArray { dst: Reg, arg_base: Reg, argc: u16 },
    /// `dst = {}` — empty object (populated by following SetProp/SetIndex).
    NewObject { dst: Reg },
    /// `dst = ToObject(src)` — `Object(x)` / `new Object(x)`: primitives box
    /// (string/number/boolean/symbol/bigint wrappers), null/undefined → a fresh
    /// object, and an existing object is returned unchanged.
    ToObject { dst: Reg, src: Reg },
    /// RequireObjectCoercible(src): throw a TypeError if `src` is null/undefined,
    /// otherwise a no-op. Emitted for an EMPTY object destructuring pattern
    /// (`var {} = x`) — a non-empty pattern already throws via member access.
    CheckCoercible { src: Reg },
    /// `dst = new <Error subtype>(arg?, opts?)` — a proto-linked error instance.
    /// `kind` indexes the canonical error list (0=Error, 1=TypeError, …,
    /// 7=AggregateError); `arg` (when present) is coerced to the `message` string.
    /// `opts` (when present) is the options object — if it has a `cause`, a
    /// non-enumerable own `cause` is installed (ES2022 InstallErrorCause).
    NewError { dst: Reg, kind: u8, arg: Option<Reg>, opts: Option<Reg> },
    /// `dst = Symbol(desc?)` — a fresh unique Symbol primitive. `desc` (when present)
    /// is coerced to a string description (undefined → no description).
    MakeSymbol { dst: Reg, desc: Option<Reg> },
    /// `dst = <BigInt literal>` (`123n`) — allocate a BigInt with the given value.
    LoadBigInt { dst: Reg, value: i128 },
    /// `dst = BigInt(arg)` — convert a number/string/boolean/BigInt to a BigInt
    /// (non-integer number → RangeError; symbol/null/undefined → TypeError).
    BigIntFrom { dst: Reg, arg: Reg },
    /// `dst = new RegExp(pattern, flags)` — compile a regex (`/pat/flags` literal
    /// and the constructor both lower here). `pattern`/`flags` are string regs;
    /// a bad pattern throws SyntaxError.
    NewRegExp { dst: Reg, pattern: Reg, flags: Reg },
    /// `dst = <array of obj's own enumerable string keys>` — drives `for-in`.
    /// For an array, the keys are the index strings "0".."len-1".
    ObjectKeys { dst: Reg, obj: Reg },
    /// `dst = Object.values(obj)` — array of the object's own values (or array
    /// elements).
    ObjectValues { dst: Reg, obj: Reg },
    /// `dst = Object.entries(obj)` — array of `[key, value]` pair arrays.
    ObjectEntries { dst: Reg, obj: Reg },
    /// `dst = <length of array/string in obj>` (0 for anything else). Used by
    /// the `for-of` desugaring's bound check.
    LenOf { dst: Reg, obj: Reg },
    /// `dst = obj[key]` — computed member read (array element or object prop).
    GetIndex { dst: Reg, obj: Reg, key: Reg },
    /// `obj[key] = val` — computed member write.
    SetIndex { obj: Reg, key: Reg, val: Reg },
    /// `dst = ToPropertyKey(src)` for a read-modify-write of `obj[src]` (`o[k] += v`,
    /// `o[k]++`): coerce the computed key to a property key ONCE (invoking its
    /// `toString`/`valueOf`/@@toPrimitive) so the load and the store reuse it. `obj`
    /// is RequireObjectCoercible-checked FIRST — a null/undefined base throws a
    /// TypeError BEFORE the key's coercion runs (matching `obj[k]`'s evaluation order).
    ToPropKey { dst: Reg, obj: Reg, src: Reg },
    /// Define an accessor property in an object literal: `{ get key(){…} }` or
    /// `{ set key(v){…} }`. `func` is the getter/setter function; `is_setter`
    /// picks the half. Merges with an existing accessor for the same key (so a
    /// get + set pair on one key becomes a single get/set accessor).
    DefineAccessor { obj: Reg, key: Reg, func: Reg, is_setter: bool },
    /// `dst = obj.<string_constants[name]>` — static property read
    /// (also resolves `.length` for arrays/strings).
    GetProp { dst: Reg, obj: Reg, name: u32 },
    /// `obj.<string_constants[name]> = val` — static property write.
    SetProp { obj: Reg, name: u32, val: Reg },
    /// `dst = delete obj.<string_constants[name]>` — remove an own property;
    /// `dst` is the boolean result (true unless the property is non-deletable).
    /// In strict mode a false result throws a TypeError instead.
    DeleteProp { dst: Reg, obj: Reg, name: u32, strict: bool },
    /// `dst = delete obj[key]` — computed property delete (strict: throw on false).
    DeleteIndex { dst: Reg, obj: Reg, key: Reg, strict: bool },

    /// Call `callee` with `argc` arguments staged in registers
    /// `[arg_base, arg_base+argc)`. Result lands in `dst`.
    Call { dst: Reg, callee: Reg, arg_base: Reg, argc: u16 },

    /// `dst = obj.<string_constants[name]>(args…)` — method call with `this`
    /// bound to `obj`. Arguments occupy `[arg_base, arg_base+argc)`.
    CallMethod { dst: Reg, obj: Reg, name: u32, arg_base: Reg, argc: u16 },
    /// `dst = obj[key](args…)` — computed method call: resolve the method by the
    /// runtime `key`, then call it with `this` bound to `obj`.
    CallMethodComputed { dst: Reg, obj: Reg, key: Reg, arg_base: Reg, argc: u16 },

    /// Throw the value in `src`. Unwinds to the nearest enclosing catch handler
    /// (in this or a caller frame), or aborts the program if none.
    Throw { src: Reg },
    /// Push a try-handler: on a throw before the matching `PopHandler`, control
    /// jumps to `catch_target` with the thrown value placed in `catch_reg`.
    PushHandler { catch_target: u32, catch_reg: Reg },
    /// Pop the most recent try-handler (reached when the try block completes
    /// without throwing).
    PopHandler,
    /// Push a `finally` handler. It is visited on EVERY exit from the protected
    /// region — throw (via unwind), `return` (via the Return op), or normal
    /// completion — running the finally block at `target` with a completion record
    /// deposited into `kind_reg` (0 normal, 1 return, 2 throw) and `val_reg` (the
    /// return value / thrown reason).
    PushFinally { target: u32, kind_reg: Reg, val_reg: Reg },
    /// Pop the most recent `finally` handler (the normal-completion path, just
    /// before falling into the finally block).
    PopFinally,
    /// End of a `finally` block: resume the completion in `kind_reg`/`val_reg` —
    /// re-leave a pending `return` (chaining through any outer finally), re-raise a
    /// pending throw, or fall through on normal completion.
    EndFinally { kind_reg: Reg, val_reg: Reg },

    /// Attach `raw` (an array) as the `.raw` of a tagged-template strings array
    /// `arr` (arrays can't hold named props, so it lands in a VM side table).
    SetRaw { arr: Reg, raw: Reg },
    /// `Math.random()` → a float in [0, 1) from the VM's PRNG.
    Random { dst: Reg },
    /// Install a method on a class value at runtime under a COMPUTED key
    /// (`class C { [expr]() {} }`). `class` holds the class value, `key` the
    /// evaluated key, `func` the method's function id, `kind` selects 0=method /
    /// 1=getter / 2=setter / 3=static method.
    ClassAddMember { class: Reg, key: Reg, func: u32, kind: u8 },
    /// `new Date(...)` → a Date. 0 args = now; 1 number = epoch ms; 1 string =
    /// parsed; ≥2 = (year, month0, day, h, m, s, ms) interpreted as UTC.
    DateNew { dst: Reg, arg_base: Reg, argc: u16 },
    /// `Date.UTC(year, month0, …)` → epoch ms (a number, not a Date).
    DateUTC { dst: Reg, arg_base: Reg, argc: u16 },
    /// `Date.parse(str)` → epoch ms (NaN if unparseable).
    DateParse { dst: Reg, src: Reg },
    /// Resolve the iterator of `src` for a `for-of`: if `src` has a `@@iterator`
    /// method (a custom iterable) call it (this = src) → the iterator object; else
    /// pass `src` through (arrays/strings/Map/Set/generators iterate directly).
    GetIterator { dst: Reg, src: Reg },
    /// Normalize `src` for array destructuring (`let [a,b] = src`): a generator or
    /// custom iterable is drained (LAZILY, ≤ `count` elements — `u32::MAX` when the
    /// pattern has a `...rest`) into a fresh array; arrays/strings/Map/Set (and
    /// anything else) pass through, since positional `GetIndex` already handles
    /// them. Bounding keeps `let [a,b] = infiniteIterator` from looping forever.
    IterToArray { dst: Reg, src: Reg, count: u32 },

    /// Return `src` from the current function.
    Return { src: Reg },
    /// Return undefined.
    ReturnUndefined,

    /// `console.log`-style print of `argc` values starting at `arg_base`.
    /// A dedicated opcode keeps the v1 stdlib trivial; later this becomes an
    /// ordinary builtin call. `to_stderr` is set for `console.error`/`warn`
    /// (which write to stderr in node), clear for `log`/`info`/`debug`.
    Print { arg_base: Reg, argc: u16, to_stderr: bool },
}

/// A compiled function: its code, register-file size, parameter count, and the
/// constant pool it references.
#[derive(Clone, Debug)]
pub struct FuncProto {
    pub name: String,
    pub code: Vec<Instr>,
    pub reg_count: u16,
    pub param_count: u16,
    /// The function's `.length` property = ExpectedArgumentCount: the number of
    /// formal parameters BEFORE the first one with a default value, excluding the
    /// rest parameter. Distinct from `param_count` (which drives arg binding and
    /// counts every non-rest formal). E.g. `(a, b = 1, c) => …` has param_count 3
    /// but length 1.
    pub length: u16,
    /// Register receiving the rest parameter array (`function f(a, ...rest)`),
    /// or `None`. The VM gathers args beyond `param_count` into a fresh array
    /// and stores it here at call setup. Always `param_count + 1` when present.
    pub rest_reg: Option<u16>,
    /// Register receiving the `arguments` object (an array of all actual args),
    /// built at call setup, when the (non-arrow) function references `arguments`.
    /// `None` if unused.
    pub arguments_reg: Option<u16>,
    /// True for a `function*` generator body: calling it builds a suspended
    /// Generator object instead of running, and it is never whole-function JITed.
    pub is_generator: bool,
    /// True for an `async function` body: calling it builds an AsyncState, runs
    /// to the first await, and returns a Promise; never whole-function JITed.
    pub is_async: bool,
    /// True when this function runs in strict mode (own `"use strict"` directive,
    /// a strict enclosing scope, a class body, or module code). Strict functions
    /// receive `this` exactly as passed; sloppy functions called with a nullish
    /// `this` substitute the global object (OrdinaryCallBindThis, ThisMode global).
    pub is_strict: bool,
    pub constants: Vec<Value>,
    /// Heap-string constants referenced by `LoadConst` need their text; this
    /// parallels `constants` for the string case (resolved at load time).
    pub string_constants: Vec<String>,
    /// If this function's name is hoisted to a global binding, the slot index;
    /// the VM materialises a function object into that global at startup.
    pub name_global: Option<u16>,
    /// Upvalues this function captures, in order. Index `i` of a `UpvalGet`/
    /// `UpvalSet` refers to `upvalues[i]`. Each entry says where the DEFINING
    /// frame finds the cell to capture: a local register holding a cell, or one
    /// of the defining frame's own upvalues (nested-of-nested capture).
    pub upvalues: Vec<UpvalSource>,
    /// Exact source text of this function (sliced from the program source by the
    /// function node's span), used by `Function.prototype.toString`. Empty for
    /// the synthetic top-level script body and for placeholders, in which case
    /// `toString` falls back to the native-function form.
    pub source: String,
}

/// Where a closure's upvalue is sourced from, evaluated in the defining frame.
/// A builtin `Math` function, resolved at compile time from `Math.<name>(…)`.
#[derive(Clone, Copy, Debug)]
pub enum MathFn {
    Abs, Floor, Ceil, Round, Trunc, Sign, Sqrt, Cbrt,
    Exp, Log, Log2, Log10, Expm1, Log1p,
    Sin, Cos, Tan, Asin, Acos, Atan,
    Sinh, Cosh, Tanh, Asinh, Acosh, Atanh,
    Clz32, Fround,
    Pow, Atan2, Imul,
    Min, Max, Hypot,
}

impl MathFn {
    /// Map a `Math.<name>` method to its function, if supported.
    pub fn from_name(name: &str) -> Option<MathFn> {
        use MathFn::*;
        Some(match name {
            "abs" => Abs, "floor" => Floor, "ceil" => Ceil, "round" => Round,
            "trunc" => Trunc, "sign" => Sign, "sqrt" => Sqrt, "cbrt" => Cbrt,
            "exp" => Exp, "log" => Log, "log2" => Log2, "log10" => Log10,
            "expm1" => Expm1, "log1p" => Log1p,
            "sin" => Sin, "cos" => Cos, "tan" => Tan,
            "asin" => Asin, "acos" => Acos, "atan" => Atan,
            "sinh" => Sinh, "cosh" => Cosh, "tanh" => Tanh,
            "asinh" => Asinh, "acosh" => Acosh, "atanh" => Atanh,
            "clz32" => Clz32, "fround" => Fround,
            "pow" => Pow, "atan2" => Atan2, "imul" => Imul,
            "min" => Min, "max" => Max, "hypot" => Hypot,
            _ => return None,
        })
    }
}

/// A builtin global function, resolved at compile time from a bare call.
#[derive(Clone, Copy, Debug)]
pub enum GlobalFn {
    Number,
    String,
    Boolean,
    ParseInt,
    ParseFloat,
    IsNaN,
    IsFinite,
}

impl GlobalFn {
    pub fn from_name(name: &str) -> Option<GlobalFn> {
        Some(match name {
            "Number" => GlobalFn::Number,
            "String" => GlobalFn::String,
            "Boolean" => GlobalFn::Boolean,
            "parseInt" => GlobalFn::ParseInt,
            "parseFloat" => GlobalFn::ParseFloat,
            "isNaN" => GlobalFn::IsNaN,
            "isFinite" => GlobalFn::IsFinite,
            _ => return None,
        })
    }
}

/// Constructor-namespace static methods that take a flat argument list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StaticFn {
    /// `Object.assign(target, ...sources)` — copy own keys; returns target.
    ObjectAssign,
    /// `Object.fromEntries(iterable)` — build an object from [k,v] entries.
    ObjectFromEntries,
    /// `Promise.resolve(v)` / `Promise.reject(r)`.
    PromiseResolve,
    PromiseReject,
    /// `Promise.all(iterable)` — fulfils with an array of results, or rejects with
    /// the first rejection.
    PromiseAll,
    /// `Promise.allSettled(iterable)` — fulfils with `{status,value|reason}` records.
    PromiseAllSettled,
    /// `Promise.race(iterable)` — settles as the first input to settle.
    PromiseRace,
    /// `Promise.any(iterable)` — fulfils with the first fulfilment, or rejects with
    /// an AggregateError if all reject.
    PromiseAny,
    /// `Array.of(...items)` — a new array of the arguments.
    ArrayOf,
    /// `String.fromCharCode(...codes)` — string from UTF-16 code units.
    StringFromCharCode,
    /// `Number.isInteger(x)` — no coercion.
    NumberIsInteger,
    /// `Number.isNaN(x)` — no coercion.
    NumberIsNaN,
    /// `Number.isFinite(x)` — no coercion.
    NumberIsFinite,
    /// `Number.isSafeInteger(x)` — integer within ±(2^53 − 1).
    NumberIsSafeInteger,
    /// `Object.defineProperty(obj, key, descriptor)` — returns obj.
    ObjectDefineProperty,
    /// `Object.getOwnPropertyDescriptor(obj, key)` — a descriptor object or undefined.
    ObjectGetOwnPropertyDescriptor,
    /// `Object.getOwnPropertyNames(obj)` — array of own string keys (incl. non-enumerable).
    ObjectGetOwnPropertyNames,
    /// `Object.getPrototypeOf(obj)` — the object's prototype (or null).
    ObjectGetPrototypeOf,
    /// `Object.create(proto[, props])` — a new object with the given prototype.
    ObjectCreate,
    /// `Object.defineProperties(obj, descs)` — define multiple properties.
    ObjectDefineProperties,
}

impl StaticFn {
    /// Map `Namespace.method` to its static function, if supported.
    pub fn from_name(ns: &str, method: &str) -> Option<StaticFn> {
        Some(match (ns, method) {
            ("Object", "assign") => StaticFn::ObjectAssign,
            ("Object", "fromEntries") => StaticFn::ObjectFromEntries,
            ("Promise", "resolve") => StaticFn::PromiseResolve,
            ("Promise", "reject") => StaticFn::PromiseReject,
            ("Promise", "all") => StaticFn::PromiseAll,
            ("Promise", "allSettled") => StaticFn::PromiseAllSettled,
            ("Promise", "race") => StaticFn::PromiseRace,
            ("Promise", "any") => StaticFn::PromiseAny,
            ("Array", "of") => StaticFn::ArrayOf,
            ("String", "fromCharCode") => StaticFn::StringFromCharCode,
            ("Number", "isInteger") => StaticFn::NumberIsInteger,
            ("Number", "isNaN") => StaticFn::NumberIsNaN,
            ("Number", "isFinite") => StaticFn::NumberIsFinite,
            ("Number", "isSafeInteger") => StaticFn::NumberIsSafeInteger,
            ("Object", "defineProperty") => StaticFn::ObjectDefineProperty,
            ("Object", "getOwnPropertyDescriptor") => StaticFn::ObjectGetOwnPropertyDescriptor,
            ("Object", "getOwnPropertyNames") => StaticFn::ObjectGetOwnPropertyNames,
            ("Object", "getPrototypeOf") => StaticFn::ObjectGetPrototypeOf,
            ("Object", "create") => StaticFn::ObjectCreate,
            ("Object", "defineProperties") => StaticFn::ObjectDefineProperties,
            _ => return None,
        })
    }
}

/// Bitwise/shift operators (operands coerced to int32, or uint32 for `Ushr`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BitwiseOp {
    And,
    Or,
    Xor,
    Shl,
    Shr,
    Ushr,
}

/// A built-in constructor recognised on the right of `instanceof`. The engine
/// has no user-level prototype chain, so `x instanceof C` is decided
/// structurally: by the heap kind of `x`, and (for errors) its `name` field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstanceCtor {
    Array,
    Object,
    Function,
    /// The `Error` base — matches any error subtype.
    Error,
    TypeError,
    RangeError,
    SyntaxError,
    ReferenceError,
    EvalError,
    UriError,
    AggregateError,
}

impl InstanceCtor {
    pub fn from_name(name: &str) -> Option<InstanceCtor> {
        Some(match name {
            "Array" => InstanceCtor::Array,
            "Object" => InstanceCtor::Object,
            "Function" => InstanceCtor::Function,
            "Error" => InstanceCtor::Error,
            "TypeError" => InstanceCtor::TypeError,
            "RangeError" => InstanceCtor::RangeError,
            "SyntaxError" => InstanceCtor::SyntaxError,
            "ReferenceError" => InstanceCtor::ReferenceError,
            "EvalError" => InstanceCtor::EvalError,
            "URIError" => InstanceCtor::UriError,
            "AggregateError" => InstanceCtor::AggregateError,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug)]
pub enum UpvalSource {
    /// Capture the cell currently in the defining frame's register `reg`.
    ParentLocal(Reg),
    /// Capture the defining frame's own upvalue `idx` (re-capture up the chain).
    ParentUpval(u16),
}

/// A whole program: the top-level function plus every nested function, indexed
/// by id. Function id `0` is the top-level script body.
#[derive(Clone, Debug)]
pub struct Program {
    pub functions: Vec<FuncProto>,
    pub global_count: u32,
    pub classes: Vec<ClassDef>,
    /// Global slot names, indexed by slot. Lets the VM populate slots for free
    /// builtin identifiers (`Object`, `Array`, `Function`, …) at startup.
    pub global_names: Vec<String>,
    /// Global slots for top-level `var` declarations — hoisted to `undefined` at
    /// startup so a read before the textual declaration yields `undefined` (not
    /// the never-declared ReferenceError). Other slots start as the uninitialized
    /// sentinel; a write (StoreGlobal/builtin/function) clears it.
    pub hoisted_globals: Vec<u32>,
}

/// A compiled class: the constructor func id (runs field inits + user ctor body),
/// and each instance method's name + func id. Materialized into a `HeapObj::Class`
/// by the `MakeClass` op.
#[derive(Clone, Debug)]
pub struct ClassDef {
    pub name: String,
    /// The constructor proto: an explicit ctor (field inits prepended + body)
    /// when `has_explicit_ctor`, else a fields-only proto (or `None`).
    pub ctor: Option<u32>,
    /// Whether the class declared its own `constructor`. When false, `new` runs
    /// the parent ctor (implicit `super(...args)`) before this class's fields.
    pub has_explicit_ctor: bool,
    pub methods: Vec<(String, u32)>,
    /// `get name()` accessors: invoked (with `this` = instance) on property read.
    pub getters: Vec<(String, u32)>,
    /// `set name(v)` accessors: invoked (with `this` = instance) on property write.
    pub setters: Vec<(String, u32)>,
    /// `static name()` methods: own properties of the class value itself.
    pub statics: Vec<(String, u32)>,
    /// `static get name()` / `static set name(v)` accessors: invoked with
    /// `this` = the class value on read/write of `C.name`.
    pub static_getters: Vec<(String, u32)>,
    pub static_setters: Vec<(String, u32)>,
    /// Exact source text of the whole `class … { … }` (by the class node's span),
    /// returned by `Function.prototype.toString` on the class value.
    pub source: String,
}
