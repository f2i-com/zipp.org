//! Explicit-frame register virtual machine.
//!
//! The defining choice: **JS recursion does not use the native Rust stack**.
//! Every activation is a frame in `frames: Vec<Frame>` over one flat register
//! file `regs: Vec<Value>`. A call pushes a frame and continues the same
//! dispatch loop; a return pops it. Consequences:
//!
//! * Deep recursion is bounded by a counter, not by the OS stack — it throws a
//!   catchable `RangeError` instead of segfaulting (a real bug in the old
//!   engine's JIT path).
//! * There is exactly one hot loop to optimise, and registers are explicit —
//!   the shape a register-allocating JIT consumes directly. Keeping unboxed
//!   `i32` live across a call boundary (where V8 wins and the old engine lost)
//!   becomes a property of *this* loop's frame model rather than something
//!   bolted on.
//!
//! Arithmetic has typed-`i32` fast paths inline; anything else falls to the
//! generic `f64` path. v1 is an interpreter — it will be slower than the old
//! JIT'd engine and than V8; the point is a clean substrate that a JIT can
//! later make faster.

#![allow(unused_imports)]
use crate::bytecode::{InstanceCtor, Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap, PropAttr, PromiseState, Reaction,
};
use crate::value::Value;

/// Hard cap on simultaneous JS frames. Throws a catchable RangeError rather
/// than growing unbounded. 100k is far beyond any non-pathological recursion
/// and the flat register file makes each frame cheap.
const MAX_FRAMES: usize = 100_000;

/// Extra global slots reserved past `global_count` as JIT scratch "field globals"
/// for object scalar-replacement (SROA). A field-promoted region uses pool slots
/// `[global_count, global_count + n_fields)`; regions reuse the pool (synced per
/// native run, never concurrent), so this caps fields-per-region, not total.
const FIELD_POOL: usize = 64;

/// Extra global slots reserved past the JIT field pool for globals *created or
/// first referenced inside `eval`* (sloppy `x = 1`, `var x`, hoisted function
/// declarations, and reads of builtins the main program never named). Sized once
/// at startup so the globals Vec never reallocates at runtime (the JIT pins its
/// base pointer); `eval` draws from this pool by name and throws once it is
/// exhausted rather than growing the Vec.
const EVAL_POOL: usize = 1024;

/// Sentinel `closure` value for a frame whose callee is a plain (capture-free)
/// function rather than a closure. Real heap indices are always `< u32::MAX`.
const NO_CLOSURE: u32 = u32::MAX;

/// Largest length zipp will EAGERLY materialize for a dense array (`Vec<Value>`).
/// The spec allows up to 2^32-1, but a dense Vec of that many `Value`s would be
/// 32 GB; real engines store such arrays sparsely. Until zipp has sparse arrays,
/// a `new Array(n)` / `arr.length = n` / defineProperty('length') / large-index
/// assignment / array-like materialization beyond this cap throws a RangeError
/// instead of OOMing the host. 2^22 elements ≈ 32 MB per array — far larger than
/// any realistic program needs, while keeping a 12-way-parallel test262 run (each
/// process possibly building several arrays) comfortably bounded.
pub(crate) const MAX_DENSE_ARRAY_LEN: usize = 1 << 20;

/// An active `try` handler within a frame.
/// One activation record.
struct Frame {
    func: u32,
    /// Base index into `regs` of this frame's register window.
    base: usize,
    /// Instruction pointer within the function's code.
    ip: usize,
    /// Register in the *caller's* window that receives this call's result.
    ret_dst: u16,
    /// Heap index of the `Closure` object this frame is executing, or
    /// `NO_CLOSURE` for a plain function. `UpvalGet`/`UpvalSet` read the
    /// closure's captured cell indices through it.
    closure: u32,
    /// Active `try` handlers in this frame, innermost last. A `Throw` (or a
    /// thrown error bubbling up from a builtin call) unwinds to the innermost
    /// handler here, else propagates to the caller frame.
    handlers: Vec<Handler>,
}

/// Which array higher-order method `array_each` is driving (callback args are
/// `[element, index]` for all three; only the result handling differs).
#[derive(Clone, Copy)]
pub(crate) enum EachMode {
    Map,
    Filter,
    ForEach,
}

/// Whether a promise reaction is the fulfill or reject handler.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReactionKind {
    Fulfill,
    Reject,
}

/// How a suspended async activation is resumed: with an awaited value, or by
/// throwing a rejection into it at the await point.
#[derive(Clone, Copy)]
pub(crate) enum Resume {
    Value(Value),
    Throw(Value),
}

/// A queued microtask (the whole event loop). `Reaction` runs a promise reaction
/// — `callback` (a JS fn, a native BoundResolver, or undefined for pass-through)
/// applied to the settled `arg`, settling `dependent`. `AsyncResume` resumes a
/// suspended async activation.
pub(crate) enum Microtask {
    Reaction { callback: Value, arg: Value, dependent: u32, kind: ReactionKind, finally: bool },
    AsyncResume { activation: u32, input: Resume },
}

/// Native (built-in) function ids — the discriminant carried by `HeapObj::Native`.
/// Each maps to an arm of `Vm::call_native`.

/// What `object_enum_own` collects.
#[derive(Clone, Copy)]
pub(crate) enum EnumWhat {
    Keys,
    Values,
    Entries,
}

pub struct Vm<'p> {
    program: &'p Program,
    /// Functions compiled at runtime by `eval` / `new Function`. Each is a leaked
    /// `Box<FuncProto>` so its address is stable (the whole-function JIT and the
    /// run loop hold raw pointers into FuncProtos, so they must never move). A
    /// unified `func_id` addresses `program.functions` for `id < main_func_count`
    /// and `eval_funcs[id - main_func_count]` beyond it.
    eval_funcs: Vec<&'static crate::bytecode::FuncProto>,
    /// Number of functions in the compile-time `program` (the boundary between
    /// program function ids and runtime `eval_funcs` ids).
    main_func_count: usize,
    /// Classes compiled at runtime by `eval`. Like `eval_funcs`: each is a leaked
    /// `Box<ClassDef>` addressed by a unified class_id (`program.classes` below
    /// `main_class_count`, `eval_classes` beyond). Their func-id members and the
    /// referencing `MakeClass`/`Super*` ops are re-indexed at install time.
    eval_classes: Vec<&'static crate::bytecode::ClassDef>,
    /// Number of classes in the compile-time `program` (boundary between program
    /// and runtime `eval_classes` class ids).
    main_class_count: usize,
    /// Globals introduced by `eval`: maps a global NAME (one not present in the
    /// compile-time `program.global_names`) to the EVAL_POOL slot it was assigned.
    /// Persists across `eval` calls so repeated evals see each other's globals.
    eval_global_map: std::collections::HashMap<String, u32>,
    /// Next free EVAL_POOL slot. Starts at `global_count + FIELD_POOL`; bumped as
    /// new eval globals are assigned, capped at `+ EVAL_POOL`.
    eval_global_next: u32,
    /// Most-recent class value per class_id (filled by `MakeClass`), so a
    /// `super` call can reach its lexical superclass value at runtime.
    class_values: Vec<Option<Value>>,
    heap: Heap,
    globals: Vec<Value>,
    /// One contiguous register file shared by all live frames; each frame owns
    /// the window `[base, base + reg_count)`.
    regs: Vec<Value>,
    frames: Vec<Frame>,
    /// Lines produced by `Print` (console.log/info/debug → stdout), in order.
    pub output: Vec<String>,
    /// Lines produced by `console.error`/`console.warn` (→ stderr in node).
    pub errput: Vec<String>,
    /// VM start instant — the zero point for `performance.now()` (which reports
    /// fractional milliseconds elapsed since the program began).
    start: std::time::Instant,
    /// The JS value currently being thrown, set when a `Throw` (or an internal
    /// error) begins unwinding and cleared when a `catch` handler receives it.
    /// Carrying the real `Value` (not just a message) lets `catch (e)` bind the
    /// exact thrown object/string/number, and survives propagation across
    /// nested `run_loop` invocations (builtin callbacks) until caught.
    pending_throw: Option<Value>,
    /// Set by a `Yield` op to hand a generator's yielded value (+ the yield's
    /// bytecode ip, for the resume point) back to `generator_method`, which
    /// `.take()`s it to distinguish a suspension from a normal return.
    pending_yield: Option<(Value, usize)>,
    /// Set by an `Await` op (the awaited value + the Await's ip + the activation's
    /// live `try` handlers); `drive_async` `.take()`s it to suspend the async
    /// activation, mirroring `pending_yield`. Unlike generators, async activations
    /// PRESERVE handlers across a suspension so `try { await p } catch` works.
    pending_await: Option<(Value, usize, Vec<Handler>)>,
    /// FIFO microtask queue — the entire event loop (no timers/IO exist). Drained
    /// to empty by `drain_microtasks` after the main script returns; a microtask
    /// may enqueue more, which run in the same drain.
    microtasks: std::collections::VecDeque<Microtask>,
    /// The `.raw` array of a tagged-template strings object, keyed by the cooked
    /// array's heap index. Arrays don't carry named properties here, so a
    /// template object's `raw` lives in this side table (read by `get_prop`).
    template_raws: std::collections::HashMap<u32, Value>,
    /// Lazily-created `.prototype` object for a function/class value, keyed by the
    /// callable's heap index. `Fn.prototype` / `Class.prototype` must return a
    /// stable object (identity: `C.prototype === C.prototype`), so it is built on
    /// first access and cached here. For a class it carries the own methods +
    /// `constructor`; for a plain function just `constructor`.
    prototypes: std::collections::HashMap<u32, u32>,
    /// Explicit `[[Prototype]]` recorded for an `Object.create(proto)` object,
    /// keyed by the new object's heap index (read by `Object.getPrototypeOf`).
    proto_of: std::collections::HashMap<u32, Value>,
    /// Own properties set on a function value (`fn.x = y`, e.g. `assert.sameValue`),
    /// keyed by the callable's heap index. Functions can't carry an inline ObjMap,
    /// so their (rare) own props live here.
    fn_props: std::collections::HashMap<u32, ObjMap>,
    /// Non-index string-keyed own properties of an Array (`arr.foo = 1`, and a
    /// regex match-result's `index`/`input`/`groups`), keyed by the array's heap
    /// index. `HeapObj::Array` is a dense `Vec<Value>` with no inline property
    /// map, so its (rare) named own properties live here — exactly mirroring
    /// `fn_props` for callables. Numeric indices + `length` stay in the Vec.
    arr_props: std::collections::HashMap<u32, ObjMap>,
    /// Resizable ArrayBuffers: heap idx → maxByteLength. Presence marks a buffer
    /// as resizable (a side table avoids changing the ArrayBuffer heap variant).
    ab_max: std::collections::HashMap<u32, usize>,
    /// Length-tracking TypedArrays/DataViews (created on a resizable buffer with
    /// no explicit length): heap idx set. Their effective length follows the
    /// buffer's current byte length.
    ta_tracking: std::collections::HashSet<u32>,
    /// Callables expose `name`/`length` as synthesized own properties (computed
    /// from the proto, not stored). They're `configurable: true`, so `delete
    /// fn.name` must make them vanish — recorded here as `(heap_idx, 0=name |
    /// 1=length)`. Empty in normal programs; only `delete` on these keys fills it.
    deleted_callable_intrinsics: std::collections::HashSet<(u32, u8)>,
    /// Heap indices of the built-in prototype objects (`Object.prototype`,
    /// `Function.prototype`, `Array.prototype`), built by `setup_globals`. Used as
    /// the [[Prototype]] for plain objects / functions / arrays so their methods
    /// resolve as values and `getPrototypeOf` returns them. 0 until set up.
    obj_proto: u32,
    fn_proto: u32,
    /// The `Function` constructor object — identified in `construct` /
    /// `call_ctor_as_function` so `new Function(args, body)` / `Function(...)`
    /// compile and return a real function (via `do_eval` of a function literal).
    function_ctor: u32,
    /// The %GeneratorFunction% / %AsyncFunction% / %AsyncGeneratorFunction%
    /// intrinsic constructors and their `.prototype` objects. Not global — reached
    /// via `Object.getPrototypeOf(function*(){}).constructor` etc. A generator/
    /// async/async-generator function's [[Prototype]] is the matching `*_fn_proto`
    /// (so its `.constructor` resolves to the matching ctor); the ctors build new
    /// functions via `do_eval` of `(function*|async ... anonymous(){...})`.
    gen_fn_ctor: u32,
    gen_fn_proto: u32,
    async_fn_ctor: u32,
    async_fn_proto: u32,
    asyncgen_fn_ctor: u32,
    asyncgen_fn_proto: u32,
    arr_proto: u32,
    /// `String.prototype` — primitive string values delegate here for method
    /// access (`"x".charAt`, `"x".slice`, …, as values), 0 until `setup_globals`.
    str_proto: u32,
    /// `Map`/`Set`/`Date`/`Promise`.prototype — instances delegate here for
    /// method access as VALUES (`new Map().set`, `d.getHours`). 0 until set up.
    map_proto: u32,
    set_proto: u32,
    date_proto: u32,
    promise_proto: u32,
    /// `Number`/`Boolean`.prototype — number/boolean PRIMITIVES delegate here for
    /// method-as-value access (`(5).toFixed`, `true.toString`). 0 until set up.
    num_proto: u32,
    bool_proto: u32,
    /// `WeakMap`/`WeakSet`/`WeakRef`.prototype — instances delegate here.
    weakmap_proto: u32,
    weakset_proto: u32,
    weakref_proto: u32,
    finreg_proto: u32,
    /// Error prototypes, indexed by the canonical error kind (0=Error.prototype,
    /// 1=TypeError.prototype, …, 7=AggregateError.prototype). The subtype protos
    /// chain to `error_protos[0]`; every error instance links here via `proto_of`
    /// so `.constructor`/`.name`/`.message`/`.toString`/`instanceof` resolve. 0
    /// until `setup_globals`.
    error_protos: [u32; 8],
    /// The matching error constructor function values (`Error`, `TypeError`, …),
    /// indexed the same way. Stored on each proto as `.constructor` and used by the
    /// runtime `new (TypeError)()` / `Reflect.construct` path.
    error_ctors: [u32; 8],
    /// `Symbol.prototype` heap index (toString/valueOf/description) and the `Symbol`
    /// constructor object heap index (callable, NOT constructable). 0 until setup.
    symbol_proto: u32,
    symbol_ctor: u32,
    /// `BigInt.prototype` and the `BigInt` constructor object (callable, NOT
    /// constructable — like `Symbol`). 0 until setup.
    bigint_proto: u32,
    bigint_ctor: u32,
    /// `RegExp.prototype` and the `RegExp` constructor object. 0 until setup.
    regexp_proto: u32,
    regexp_ctor: u32,
    /// `%RegExpStringIteratorPrototype%` — the prototype of the iterator returned
    /// by `RegExp.prototype[Symbol.matchAll]` / `String.prototype.matchAll`.
    regexp_string_iter_proto: u32,
    /// The `%TypedArray%` intrinsic (abstract base ctor) + its prototype, the 11
    /// concrete TypedArray ctors + their prototypes (indexed by `kind`), and the
    /// `ArrayBuffer`/`DataView` ctors + prototypes. 0 until setup.
    ta_base_ctor: u32,
    ta_base_proto: u32,
    ta_ctors: [u32; 11],
    ta_protos: [u32; 11],
    arraybuffer_ctor: u32,
    arraybuffer_proto: u32,
    dataview_ctor: u32,
    dataview_proto: u32,
    /// `SharedArrayBuffer` ctor + prototype. A SharedArrayBuffer reuses the
    /// `HeapObj::ArrayBuffer` representation; its heap index is recorded in
    /// `shared_buffers` so property/prototype/`instanceof` resolution treats it as
    /// shared (growable instead of resizable, never detached, `SharedArrayBuffer`
    /// toStringTag). 0 until setup.
    sab_ctor: u32,
    sab_proto: u32,
    /// Heap indices of ArrayBuffers that are actually SharedArrayBuffers.
    shared_buffers: std::collections::HashSet<u32>,
    /// `DisposableStack` ctor + prototype. An instance is a plain Object linked to
    /// `disposablestack_proto`; its disposer stack + disposed flag live in
    /// `dispose_stacks` (the disposers are zero-arg callable thunks, GC-traced).
    disposablestack_ctor: u32,
    disposablestack_proto: u32,
    dispose_stacks: std::collections::HashMap<u32, (Vec<Value>, bool)>,
    /// `AsyncDisposableStack` ctor + prototype, and the set of dispose-stack
    /// instances that are ASYNC (their `use` prefers @@asyncDispose and their
    /// disposal goes through `disposeAsync`, which returns a Promise).
    asyncdisposablestack_ctor: u32,
    asyncdisposablestack_proto: u32,
    async_stacks: std::collections::HashSet<u32>,
    /// `SuppressedError` ctor + prototype (ES2026). Unlike the 8 standard errors,
    /// it carries `error` + `suppressed` own properties; its prototype chains to
    /// %Error.prototype% so `instanceof Error` holds.
    suppressederror_ctor: u32,
    suppressederror_proto: u32,
    /// The `Proxy` constructor object (no `.prototype`). 0 until setup.
    proxy_ctor: u32,
    /// The `Temporal` namespace object + `Temporal.Duration`/`PlainDate` ctors/protos.
    temporal_ns: u32,
    duration_ctor: u32,
    duration_proto: u32,
    plaindate_ctor: u32,
    plaindate_proto: u32,
    plaintime_ctor: u32,
    plaintime_proto: u32,
    plaindatetime_ctor: u32,
    plaindatetime_proto: u32,
    instant_ctor: u32,
    instant_proto: u32,
    plainyearmonth_ctor: u32,
    plainyearmonth_proto: u32,
    plainmonthday_ctor: u32,
    plainmonthday_proto: u32,
    zoneddatetime_ctor: u32,
    zoneddatetime_proto: u32,
    /// `Temporal.ZonedDateTime` time-zone id per instance heap index (the id is a
    /// heap string, kept here so it's GC-traced — the `Temporal{fields:Vec<i64>}`
    /// layout can't hold a heap reference). The instance's `fields` are
    /// `[epochNs hi, epochNs lo, offsetNanoseconds]`.
    zdt_tz: std::collections::HashMap<u32, Value>,
    intl_ns: u32,
    intl_ctors: [u32; 10],
    intl_protos: [u32; 10],
    /// Monotonic counter giving each `Symbol()` a unique internal property key
    /// (`@@sym:N`), so distinct symbols never collide as object keys.
    symbol_counter: u64,
    /// The `Symbol.for` global registry: registry key string → the shared Symbol.
    symbol_registry: std::collections::HashMap<String, Value>,
    /// Internal prop_key (`@@iterator`, `@@sym:N`, …) → the Symbol value, so a
    /// symbol-keyed own property can be reflected back to its Symbol by
    /// `Object.getOwnPropertySymbols`.
    symbol_keys: std::collections::HashMap<String, Value>,
    /// `%Iterator.prototype%` (the shared root of all iterator prototypes; holds
    /// the ES2025 helper methods), `%IteratorHelperPrototype%` (next/return for
    /// lazy helpers, chains to the root), and the `Iterator` constructor.
    iterator_proto_root: u32,
    iterator_helper_proto: u32,
    /// `%GeneratorPrototype%` — the prototype of generator instances: next/return/
    /// throw + @@iterator + @@toStringTag, chaining to %Iterator.prototype% so a
    /// generator inherits the iterator-helper methods (`g().map(...)` etc.).
    gen_proto: u32,
    iterator_ctor: u32,
    /// The test262 `$262` host object (0 until set up).
    dollar262: u32,
    /// `%ArrayIteratorPrototype%` — the prototype of Array entries/keys/values
    /// iterators (and the default array `@@iterator`). 0 until set up.
    array_iter_proto: u32,
    /// `%MapIteratorPrototype%` / `%SetIteratorPrototype%` — distinct prototypes so
    /// `getPrototypeOf(map.entries())` differs from a Set/Array iterator's.
    map_iter_proto: u32,
    set_iter_proto: u32,
    string_iter_proto: u32,
    /// The `globalThis` object (an empty Object at this heap index); property
    /// access on it is routed to the global slots by name. 0 until `setup_globals`.
    global_this: u32,
    /// `Math.random()` PRNG state (xorshift64*). Deterministically seeded, so a
    /// program's random sequence is reproducible run-to-run (and JIT-on == off).
    rng_state: u64,
    /// Native JIT tier (x86-64 only, `feature = "jit"`). Compiles hot leaf
    /// integer functions to native code that shares this VM's register window;
    /// any non-int/heap/call op bails back to the interpreter at the exact ip.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    jit: crate::codegen::Jit,
    /// JIT on/off (set from `ZIPP_NOJIT` env var at construction) — lets a
    /// single binary A/B the JIT against the pure interpreter for honest
    /// measurement.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    jit_enabled: bool,
    /// Current native self-recursion depth (guards `jit_self_call`).
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    jit_recurse_depth: u32,
    /// Pinned register-file capacity: `self.regs` is reserved to this at startup
    /// and NEVER allowed to grow past it (every call/recursion site checks),
    /// so the Vec never reallocates while native JIT code holds a raw pointer
    /// into it. 0 until `reserve_jit_regs` runs (interpreter-only builds ignore
    /// it). Exceeding it throws RangeError — a tighter bound than MAX_FRAMES.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    reg_capacity: usize,
    /// High-water mark: the largest `regs.len()` ever reached (and thus
    /// initialized). A native self-call window at or below this can be exposed
    /// with `set_len` instead of a zero-filling `resize` — its slots already hold
    /// valid `Value` bits (stale, but the compiled code defs-before-use). This
    /// avoids re-zeroing the callee window on every recursive call once the
    /// recursion has reached its deepest native level. Backing buffer is pinned
    /// (`reserve_jit_regs`) so initialized slots stay valid for the VM's life.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    regs_hw: usize,
    /// Mark-sweep GC. `gc_floor` is the first collectable slot: everything below
    /// it (the interned strings + all built-ins allocated during setup) is pinned
    /// and never freed. `gc_lock` > 0 disables collection during native built-ins
    /// that hold un-rooted `Vec<Value>` working sets across a callback re-entry
    /// (array iteration, sort, iterate_to_vec, …). `gc_stress` (ZIPP_GC_STRESS)
    /// forces a collection at every safe point to flush out missed roots/edges.
    gc_floor: u32,
    gc_lock: u32,
    gc_stress: bool,
}

/// A thrown JS value rendered to a message (v1 throws are strings/RangeError).
#[derive(Debug)]
pub struct Thrown(pub String);


// submodules (split from the former monolithic vm.rs)
mod engine;
mod dispatch;
mod async_runtime;
mod indexing_date;
mod setup;
mod natives;
mod props;
mod mathjson;
mod access;
mod builtins;
mod values;
mod temporal;
mod intl;
mod iterhelpers;
mod proxy_regexp;
mod typedarray;
mod construct;
mod misc_methods;
mod array_ops;
mod string_ops;
mod coerce;
mod native;
mod helpers_misc;
mod helpers_datetime;
mod helpers_numeric;
mod helpers_json;
mod helpers_num2;
mod gc;

pub(crate) use helpers_misc::*;
pub(crate) use helpers_datetime::*;
pub(crate) use helpers_numeric::*;
pub(crate) use helpers_json::*;
pub(crate) use helpers_num2::*;
