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

    /// `dst = globals[idx]`
    LoadGlobal { dst: Reg, idx: u32 },
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
    /// `dst = <the class value for classes[class_id]>` — materialize a class.
    /// `parent` is the register holding the superclass value (`extends P`), or
    /// `None`; the new class links to it for inherited lookup + instanceof.
    MakeClass { dst: Reg, class_id: u32, parent: Option<Reg> },
    /// `dst = yield val` — suspend the current generator, handing `val` out as
    /// the yielded value. On resume the value passed to `.next(v)` lands in `dst`.
    Yield { dst: Reg, val: Reg },
    /// `dst = await val` — suspend the async activation on the awaited value's
    /// promise; on resume the settled value lands in `dst` (a rejection is thrown
    /// in at this point so an enclosing `try`/`catch` sees it).
    Await { dst: Reg, val: Reg },
    /// for-of step: advance over `iter` (array/string/Map/Set positionally with
    /// the cursor in `idx`, or a generator via `.next()` ignoring `idx`). Writes
    /// the next element to `value_dst` and a bool to `done_dst`. Throws if `iter`
    /// is not iterable.
    IterNext { value_dst: Reg, done_dst: Reg, iter: Reg, idx: Reg },
    /// `super(args…)`: run the lexical superclass's constructor contribution on
    /// the current `this` (reg 0). `parent_class_id` identifies the superclass.
    SuperCtor { parent_class_id: u32, arg_base: Reg, argc: u16 },
    /// `dst = super.<name>(args…)`: call the named method found from the lexical
    /// superclass up its chain, with `this` = the current frame's `this` (reg 0).
    SuperMethod { dst: Reg, parent_class_id: u32, name: u32, arg_base: Reg, argc: u16 },
    /// `dst = new callee(args…)` — construct an instance. `callee` must be a
    /// class value; builds an object, installs the methods, runs the ctor.
    New { dst: Reg, callee: Reg, arg_base: Reg, argc: u16 },
    /// `dst = Array(args…)` / `new Array(args…)`: a single numeric arg makes an
    /// array of that length (holes → undefined); otherwise an array of the args.
    ArrayCtor { dst: Reg, arg_base: Reg, argc: u16 },
    /// `dst = new Map(src?)` — build a Map from an optional iterable of [k,v]
    /// entries (`src` register, or `None` for an empty map).
    NewMap { dst: Reg, src: Option<Reg> },
    /// `dst = new Set(src?)` — build a Set from an optional iterable of values.
    NewSet { dst: Reg, src: Option<Reg> },
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
    /// materialised into a register.
    JumpIfNotLt { a: Reg, b: Reg, target: u32 },
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
    /// `dst = obj.<string_constants[name]>` — static property read
    /// (also resolves `.length` for arrays/strings).
    GetProp { dst: Reg, obj: Reg, name: u32 },
    /// `obj.<string_constants[name]> = val` — static property write.
    SetProp { obj: Reg, name: u32, val: Reg },

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
    /// Register receiving the rest parameter array (`function f(a, ...rest)`),
    /// or `None`. The VM gathers args beyond `param_count` into a fresh array
    /// and stores it here at call setup. Always `param_count + 1` when present.
    pub rest_reg: Option<u16>,
    /// True for a `function*` generator body: calling it builds a suspended
    /// Generator object instead of running, and it is never whole-function JITed.
    pub is_generator: bool,
    /// True for an `async function` body: calling it builds an AsyncState, runs
    /// to the first await, and returns a Promise; never whole-function JITed.
    pub is_async: bool,
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
}

/// Where a closure's upvalue is sourced from, evaluated in the defining frame.
/// A builtin `Math` function, resolved at compile time from `Math.<name>(…)`.
#[derive(Clone, Copy, Debug)]
pub enum MathFn {
    Abs, Floor, Ceil, Round, Trunc, Sign, Sqrt, Cbrt,
    Exp, Log, Log2, Log10,
    Sin, Cos, Tan, Asin, Acos, Atan,
    Pow, Atan2,
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
            "sin" => Sin, "cos" => Cos, "tan" => Tan,
            "asin" => Asin, "acos" => Acos, "atan" => Atan,
            "pow" => Pow, "atan2" => Atan2,
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
}
