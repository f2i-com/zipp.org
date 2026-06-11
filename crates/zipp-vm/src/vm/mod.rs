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
    /// The function VALUE being invoked in this activation (the object the caller
    /// actually called), or `UNDEFINED` when unknown/irrelevant (top-level script,
    /// eval, generator/async resume). `LoadCallee` returns it so a named function
    /// expression's own name has the SAME identity as the outer reference (rather
    /// than a freshly-allocated `Func`); falls back to the closure/fresh-Func when
    /// `UNDEFINED`.
    callee: Value,
    /// The `new.target` value for this activation: the constructor when entered
    /// via `new` / `Reflect.construct` / `super(...)`, else `undefined` (a plain
    /// call, `.call`/`.apply`, a method, a tagged template, …). Read by the
    /// `LoadNewTarget` op; consumed from `pending_new_target` at frame setup.
    new_target: Value,
    /// A sloppy direct eval's dynamic variable environment for this
    /// activation (heap index of a HeapObj::EvalScope), or u32::MAX. Created
    /// lazily by the DirectEval op for function-context evals.
    eval_scope: u32,
    /// Whether a `super(...)` has completed in THIS activation. A second
    /// `super()` in the same constructor frame throws a ReferenceError (after
    /// running the parent ctor — spec evaluates the SuperCall fully, then
    /// BindThisValue throws on re-initialization).
    super_done: bool,
    /// Heap index of THIS activation's `arguments` object, or u32::MAX. A
    /// MAPPED arguments object's [[ParameterMap]] (see `ArgsMap`) aliases the
    /// param registers only while `frames[frame_idx].args_obj` still equals
    /// its own heap index — the liveness proof (each activation allocates a
    /// fresh object, so a recycled frame slot can never spoof it).
    args_obj: u32,
}

/// [[ParameterMap]] bookkeeping for a MAPPED arguments object (sloppy callee
/// with a simple parameter list). While the creating frame is live, a still-
/// mapped index i reads/writes the formal's register `regs[base + 1 + i]`
/// (through its cell when the param is captured); the dense element store
/// remains the descriptor/escape store. `None` in `arguments_objs` = an
/// UNMAPPED arguments object (strict, or non-simple params): pure snapshot.
pub(crate) struct ArgsMap {
    /// Expected position of the creating activation in `frames`.
    frame_idx: usize,
    /// The creating frame's register-window base (revalidated with args_obj).
    base: usize,
    /// Indices `0..mapped_count` started mapped: min(param_count, argc).
    mapped_count: usize,
    /// Bit i set = formal i SEVERED from the map (delete / accessor redefine /
    /// writable:false redefine) — permanently back to ordinary semantics.
    unmapped: u64,
}

/// One in-flight `AsyncDisposableStack.prototype.disposeAsync`: the spec's
/// DisposeResources loop with an Await after EVERY disposer call. `remaining`
/// is popped from the END (LIFO); a `Value::NULL` entry is the marker a
/// nullish `use()` records (contributes the needs-await tick, calls nothing).
pub(crate) struct DisposeAsyncState {
    remaining: Vec<Value>,
    /// The pending throw completion: a single error stays as-is; each later
    /// error wraps it as SuppressedError{error: new, suppressed: prior}.
    error_chain: Option<Value>,
    /// Whether any real disposer ran (its result got a real Await).
    has_awaited: bool,
    /// Whether a nullish-resource marker was seen (spec still performs one
    /// Await(undefined) before resolving when nothing else awaited).
    needs_await: bool,
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
    /// Resume an async generator suspended at an async `yield*` (AsyncYieldDelegate)
    /// because the consumer called `.return(v)` — the loop delegates to the inner
    /// iterator's `return`. Only meaningful for an async generator at a delegate point.
    Return(Value),
}

/// A queued microtask (the whole event loop). `Reaction` runs a promise reaction
/// — `callback` (a JS fn, a native BoundResolver, or undefined for pass-through)
/// applied to the settled `arg`, settling `dependent`. `AsyncResume` resumes a
/// suspended async activation.
pub(crate) enum Microtask {
    Reaction { callback: Value, arg: Value, dependent: u32, kind: ReactionKind, finally: bool },
    AsyncResume { activation: u32, input: Resume },
    /// PromiseResolveThenableJob: resolving `promise` with a thenable defers
    /// `then.call(thenable, resolveFn, rejectFn)` to this microtask (spec ordering).
    ThenableJob { thenable: Value, then: Value, promise: u32 },
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

/// A module whose dependencies are still evaluating (top-level await):
/// everything needed to execute its body once they settle. Holds NO heap
/// Values (slots/ids only) — the namespace itself is rooted via module_cache.
pub(crate) struct DeferredModuleExec {
    pub remaining: usize,
    pub base_func: u32,
    pub ns_idx: u32,
    pub full2: Vec<(String, u32)>,
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
    /// Every builtin global NAME → its heap value, recorded at setup regardless of
    /// whether the running program referenced it. Lets `eval`'d code resolve
    /// builtins the main program never named (`eval("new RangeError()")`,
    /// `eval("Object.keys(x)")`) instead of seeing the never-declared sentinel.
    /// Values are permanent roots (traced in gc, never pruned).
    builtin_globals: std::collections::HashMap<String, u32>,
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
    /// One-shot `new.target` for the NEXT frame entered: `construct` /
    /// `Reflect.construct` / `super(...)` set it to the constructor right before
    /// invoking the body; the frame-setup path consumes it into `Frame::new_target`
    /// and resets it to `undefined`, so ordinary calls observe `new.target` undefined.
    pending_new_target: Value,
    /// Set by a `Yield` op to hand a generator's yielded value (+ the yield's
    /// bytecode ip, for the resume point) back to `generator_method`, which
    /// `.take()`s it to distinguish a suspension from a normal return.
    pending_yield: Option<(Value, usize)>,
    /// Set by the `Yield`/`GenStart` ops to the suspending frame's active `try`
    /// handlers, so a SYNC generator's `generator_method` can park them in the
    /// `HeapObj::Generator` and restore them on resume (enabling `gen.throw(e)`
    /// to land in an enclosing `try`/`catch`). The ASYNC `drive_async_gen`
    /// consumer ignores this — async-gen suspension semantics are unchanged.
    pending_yield_handlers: Vec<Handler>,
    /// The suspending frame's dynamic EvalScope (or u32::MAX), captured with
    /// `pending_yield`; the parker stamps it on the generator so a resume can
    /// restore it (param-prologue evals outlive the GenStart suspension).
    pending_yield_eval_scope: u32,
    /// Set by an `Await` op (the awaited value + the Await's ip + the activation's
    /// live `try` handlers); `drive_async` `.take()`s it to suspend the async
    /// activation, mirroring `pending_yield`. Unlike generators, async activations
    /// PRESERVE handlers across a suspension so `try { await p } catch` works.
    pending_await: Option<(Value, usize, Vec<Handler>)>,
    /// Scratch slot for NewPromiseCapability: the capturing executor (CAP_EXECUTOR)
    /// writes its (resolve, reject) arguments here during `new C(executor)`, which
    /// `new_promise_capability` `.take()`s immediately after construction. `Some`
    /// while a capture is in flight (a second executor call → TypeError).
    cap_capture: Option<(Value, Value)>,
    /// FIFO microtask queue — the entire event loop (no timers/IO exist). Drained
    /// to empty by `drain_microtasks` after the main script returns; a microtask
    /// may enqueue more, which run in the same drain.
    microtasks: std::collections::VecDeque<Microtask>,
    /// The `.raw` array of a tagged-template strings object, keyed by the cooked
    /// array's heap index. Arrays don't carry named properties here, so a
    /// template object's `raw` lives in this side table (read by `get_prop`).
    template_raws: std::collections::HashMap<u32, Value>,
    /// GetTemplateObject memoization: the canonical frozen tagged-template object
    /// per source call site, keyed by (function id, per-function site index). The
    /// cached objects are permanent GC roots (they live as long as the realm).
    template_cache: std::collections::HashMap<(u32, u32), Value>,
    /// Lazy %RegExpStringIterator% state, keyed by the iterator's heap index:
    /// (matcher regexp heap idx, subject string, flag bits — bit0 global, bit1
    /// fullUnicode (`u`/`v`, captured at creation per CreateRegExpStringIterator),
    /// done latch). Its `next()` drives RegExpExec (honouring a user `exec`) one
    /// match at a time, rather than matchAll eagerly collecting every match up
    /// front.
    regexp_string_iters: std::collections::HashMap<u32, (u32, Value, u8, bool)>,
    /// RegExp heap idx → EXACT WTF-8 bytes of its [[OriginalSource]], present
    /// ONLY when the pattern holds lone surrogates (the struct's `source:
    /// String` field is the LOSSY view — U+FFFD per surrogate). The `source`
    /// getter / `toString` / re-compile paths check this first so a
    /// lone-surrogate pattern round-trips exactly. Holds no `Value`s (pure
    /// bytes, idx-keyed): GC only needs the sweep-time retain in gc.rs.
    regexp_exact_source: std::collections::HashMap<u32, Vec<u8>>,
    /// Monotone counter minting a fresh private brand per class evaluation (1 = first;
    /// 0 = unbranded).
    next_private_brand: u64,
    /// A class method/getter/setter VALUE (and the class VALUE itself, for ctor /
    /// field-init / static blocks) heap index → the ORDERED lexical private-brand
    /// CHAIN of its class body: own brand first, then each lexically ENCLOSING
    /// class's brand. A private access threaded with lexical DEPTH d checks the
    /// receiver against `chain[d]` — the SPECIFIC declaring class's brand. Pruned by GC.
    method_brand: std::collections::HashMap<u32, Vec<u64>>,
    /// Extra private brands installed on a specific INSTANCE that its `map.class`
    /// chain does not cover — namely a constructor RETURN-OVERRIDE object, which
    /// receives a class's private fields/brand without becoming an instanceof that
    /// class. Checked by `instance_has_brand` alongside the class chain. Pruned by GC.
    instance_brand: std::collections::HashMap<u32, Vec<u64>>,
    /// Private NAMES (with the "#" prefix) declared by the class owning each brand.
    /// Lets a private access resolve "#x" to the innermost brand in the lexical
    /// chain whose class actually declares it (precise + shadow-aware) instead of
    /// accepting ANY chain brand. A name absent here falls back to the lenient
    /// any-brand check. Keyed by brand; pruned by GC to brands still referenced.
    brand_private_names: std::collections::HashMap<u64, Vec<(String, u8)>>,
    /// Brand -> heap index of the class VALUE that minted it (the DECLARING
    /// class), so a private access resolves members from the declaring class
    /// rather than the receiver's chain (kind-aware: shadowed/missing
    /// getter/setter errors). NOT a GC root: pruned when the class dies (no
    /// live accessor can then need it).
    brand_owner: std::collections::HashMap<u64, u32>,
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
    /// Length-tracking DataViews (created on a resizable/growable buffer with no
    /// explicit byteLength): heap idx set. Their byteLength follows the buffer's
    /// current size, and byteLength/byteOffset throw (IsViewOutOfBounds) once the
    /// offset exceeds the shrunk buffer.
    dv_tracking: std::collections::HashSet<u32>,
    /// Callables expose `name`/`length` as synthesized own properties (computed
    /// from the proto, not stored). They're `configurable: true`, so `delete
    /// fn.name` must make them vanish — recorded here as `(heap_idx, 0=name |
    /// 1=length)`. Empty in normal programs; only `delete` on these keys fills it.
    deleted_callable_intrinsics: std::collections::HashSet<(u32, u8)>,
    /// Names of built-in globals (`Number`, `Date`, …) removed via
    /// `delete globalThis.X`. They live in `builtin_globals`/`globals`, not as own
    /// `global_this` entries, so deletion is recorded here and `global_by_name`
    /// gates on it — making get/has-own/descriptor all agree the property is gone.
    /// Empty in normal programs; cleared for a name when it's re-defined.
    deleted_globals: std::collections::HashSet<String>,
    /// Heap indices of arrays whose `length` was made non-writable via
    /// `Object.defineProperty(arr, "length", { writable: false })`. An array's
    /// length lives in the dense Vec with no per-array attribute state, so the
    /// (rare) non-writable flag is recorded here — read by the length descriptor,
    /// `arr.length = n`, and the push/pop/shift/unshift mutators.
    array_length_nonwritable: std::collections::HashSet<u32>,
    /// Set once an integer-indexed own property is defined on `Array.prototype` or
    /// `Object.prototype` (the prototypes in every plain array's chain). While false
    /// — the overwhelmingly common case — `a[i] = v` for an absent index takes the
    /// fast own-write path with NO prototype walk; once true, set_index performs the
    /// full OrdinarySet (a prototype setter at `i` runs, a non-writable one rejects).
    array_proto_has_index: bool,
    /// Heap indices of derived-class instances whose `super(...)` has run during
    /// construction (set by the SuperCtor ops, read+cleared in `construct`). A
    /// derived constructor that returns `undefined` without `super()` having run
    /// throws a ReferenceError. Transient — entries live only across one
    /// in-flight `construct`.
    super_called: std::collections::HashSet<u32>,
    /// Heap indices of derived-class instances whose `this` is still in the
    /// constructor TDZ (between derived-ctor entry and its `super()`
    /// completing). The `ThisCheck` op throws a ReferenceError while the
    /// instance is here. Transient — inserted by `construct`/`run_class_ctor`,
    /// removed on `super()` completion and cleared on every `construct` exit.
    this_tdz: std::collections::HashSet<u32>,
    /// The produced `this` of a completed `super(...)` whose parent ctor
    /// RETURN-OVERRODE the instance, keyed by the pre-allocated instance's heap
    /// index. A derived ctor that then returns `undefined` must yield THIS
    /// value as the construction result (reg 0 was rebound, but the original
    /// instance is what `construct` holds). Read+cleared by `construct` /
    /// `run_class_ctor`; transient like `super_called`.
    super_this: std::collections::HashMap<u32, Value>,
    /// Private FIELD storage — spec [[PrivateElements]] of kind "field":
    /// instance heap idx -> ((brand, name) -> value). Keyed by the
    /// per-evaluation brand so same-named fields of sibling classes or other
    /// evaluations never collide; invisible to reflection, proxy traps and
    /// property enumeration by construction. Values are GC roots; entries
    /// pruned when the instance dies.
    private_fields: std::collections::HashMap<u32, std::collections::HashMap<(u64, String), Value>>,
    /// Heap index of the canonical %eval% native — the DirectEval op's runtime
    /// identity check (a REBOUND global `eval` gets an ordinary call).
    eval_fn_idx: u32,
    /// EvalScope stamps for closures created in frames that carry one (so
    /// arrows/functions made during or after the eval still see its
    /// bindings). Keyed by the closure value's heap index; pruned at GC.
    closure_eval_scope: std::collections::HashMap<u32, u32>,
    /// Memo per func id: 0 unknown, 1 no, 2 yes — "code contains a sloppy
    /// function-context DirectEval". Drives EAGER EvalScope creation at
    /// closure-stamp time, so closures made before the eval call share the
    /// scope the eval later populates.
    sloppy_eval_memo: Vec<u8>,
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
    /// %Array.prototype%'s own `length` (it is an Array EXOTIC object):
    /// tracks integer-index definitions on it; writable, non-enumerable,
    /// non-configurable.
    arr_proto_len: u32,
    /// The `Array` constructor (the `%Array%` intrinsic). 0 until setup. Used by
    /// ArraySpeciesCreate to take the fast dense path when the resolved species is
    /// just `%Array%` itself.
    array_ctor: u32,
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
    /// The `WeakRef` / `FinalizationRegistry` constructor objects, so the
    /// value-form `[[Construct]]` (Reflect.construct / subclassing) can build an
    /// instance and honour `newTarget.prototype`.
    weakref_ctor: u32,
    finreg_ctor: u32,
    /// The `WeakMap` / `WeakSet` constructor objects, so the value-form
    /// `[[Construct]]` (Reflect.construct / subclassing) builds an instance,
    /// honours `newTarget.prototype`, and AddEntriesFromIterable via the adder.
    weakmap_ctor: u32,
    weakset_ctor: u32,
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
    /// Heap indices of ArrayBuffers that are immutable (ES2026): their `immutable`
    /// getter is true, they are never resizable/detachable, and writes through a
    /// TypedArray view throw a TypeError.
    immutable_buffers: std::collections::HashSet<u32>,
    /// Heap indices of objects that have the [[ErrorData]] internal slot (real
    /// Error instances — built-in error ctors, internal throws, and
    /// `class X extends Error` instances). Distinguishes a true error from a
    /// plain object that merely inherits Error.prototype (which `is_error_instance`
    /// cannot, being prototype-chain based); drives `Error.prototype.stack`'s
    /// getter. Pruned on GC sweep.
    error_data: std::collections::HashSet<u32>,
    /// Set by the module loader just before executing a MODULE BODY: the
    /// next alloc_async (the body's own activation) marks its result promise
    /// in module_body_results. Consumed (mem::take) at alloc_async entry.
    pending_module_body_marker: bool,
    /// Result promises of MODULE BODY activations: they settle with
    /// UNDEFINED, never the body's completion value — spec Evaluate()
    /// resolves the capability with undefined, so a thenable completion
    /// value (e.g. a promise-valued last statement) must NOT be adopted
    /// (adoption makes a completed module look suspended and deadlocks its
    /// importers). Entries are removed at settle; pruned on GC sweep.
    module_body_results: std::collections::HashSet<u32>,
    /// Body promises of modules whose evaluation SUSPENDED (top-level await,
    /// or deps still pending): canonical path → the promise importers settle
    /// from. A LATER import of the same module (its namespace is already
    /// cached) re-publishes this as pending_module_body so the new importer
    /// chains on the SAME TopLevelCapability instead of resolving with the
    /// incomplete namespace. Entries persist after settlement.
    /// The bool: true = registered via the DEPS-PENDING (capability) path —
    /// an ANCESTOR whose body waits on other suspended modules (a cycle
    /// root); false = a direct top-level-await suspension. Late importers of
    /// a TLA module inside a cycle wait on the cycle ROOT's capability.
    module_body_promise: std::collections::HashMap<std::path::PathBuf, (Value, bool)>,
    /// Typed-module cache: (canonical path, type attribute) → namespace for
    /// `with { type: "json" | "text" }` imports — DISTINCT records from the
    /// same file's JS module. Values are GC roots like module_cache.
    typed_module_cache: std::collections::HashMap<(std::path::PathBuf, String), Value>,
    /// The function VALUE being invoked for the generator/async allocation in
    /// flight (set by the call site just before alloc_generator /
    /// alloc_async / alloc_async_generator; consumed at their entry).
    pending_gen_callee: Value,
    /// Generator / AsyncGenerator / AsyncState heap index → the function
    /// value that created it. Resumes bind it as Frame.callee so LoadCallee
    /// (a named function expression's self-name) keeps the caller-visible
    /// identity across suspensions. Values are GC roots; keys pruned on sweep.
    gen_callee: std::collections::HashMap<u32, Value>,
    /// Heap indices of IMMUTABLE cells — named function expressions' own-name
    /// bindings (MakeCellFnName). UpvalSet/StoreUpvalDyn writes through one
    /// no-op in sloppy code and throw TypeError in strict code. Pruned on GC
    /// sweep (the cells themselves are ordinary traced heap objects).
    fn_name_cells: std::collections::HashSet<u32>,
    /// `$262.evalScript` flag: the NEXT eval-program instantiation uses SCRIPT
    /// GlobalDeclarationInstantiation semantics (non-configurable brandNew
    /// bindings, lexical-collision SyntaxErrors, realm-persistent lexicals)
    /// instead of EvalDeclarationInstantiation. Consumed (mem::take) by
    /// prepare_eval_program.
    eval_script_gdi: bool,
    /// Realm slots bound as LEXICALS (let/const/class) by `$262.evalScript`
    /// scripts: collision SyntaxErrors + invisible to global-object property
    /// reflection (the main program's own lexicals live in
    /// program.lexical_globals).
    eval_lexical_globals: std::collections::HashSet<u32>,
    /// The `const` subset of eval_lexical_globals: a write to an INITIALIZED
    /// one throws TypeError (the initializing write sees UNINITIALIZED).
    eval_const_globals: std::collections::HashSet<u32>,
    /// Realm slots declared var/function by `$262.evalScript` scripts —
    /// HasVarDeclaration for later scripts' lexical-collision checks.
    eval_var_globals: std::collections::HashSet<u32>,
    /// `arguments` exotic objects (Array-backed): heap index → the live
    /// [[ParameterMap]] for a MAPPED one (sloppy + simple params), or `None`
    /// for an unmapped one (strict / non-simple). Presence alone drives the
    /// `[object Arguments]` toString tag and the Array-exotic carve-outs
    /// (ordinary `length`, no Vec growth). Pruned on GC sweep.
    arguments_objs: std::collections::HashMap<u32, Option<ArgsMap>>,
    /// Directory the running script was loaded from, used to resolve a dynamic
    /// `import(specifier)` against the filesystem (relative + bare specifiers).
    /// `None` when running from a string (eval/embedding) — then `import()` has no
    /// host loader and rejects.
    module_base_dir: Option<std::path::PathBuf>,
    /// Dynamic-import namespace cache: resolved module path → its namespace value.
    /// A module is evaluated at most once, so re-importing the same path returns the
    /// SAME namespace object (identity). Values are GC roots (modules persist).
    module_cache: std::collections::HashMap<std::path::PathBuf, Value>,
    /// Module Namespace exotic objects: namespace heap index → its (export name →
    /// global slot) map. Each module's exported bindings live in PER-MODULE fresh
    /// global slots (see run_eval_program module mode), so a namespace's [[Get]]
    /// reads the LIVE binding without colliding with another module's same-named
    /// export. [[Set]] is a no-op. Pruned on GC sweep.
    module_namespaces: std::collections::HashMap<u32, std::collections::HashMap<String, u32>>,
    /// Modules CURRENTLY being linked (cycle detection): resolved path → its OWN
    /// export name→live-slot map (re-exports not yet resolved). Present only during
    /// `import_module`; a re-export back into an in-progress module (a self/mutual
    /// cycle) resolves against this own-exports snapshot instead of recursing. Holds
    /// only slot indices into `globals` (a GC root), so it needs no separate rooting.
    module_own: std::collections::HashMap<std::path::PathBuf, std::collections::HashMap<String, u32>>,
    /// Per-namespace AMBIGUOUS export names (a name supplied by two different
    /// `export *` sources): excluded from the namespace; resolving one by name
    /// through `export {x} from` / `import {x}` is a SyntaxError.
    module_ambiguous: std::collections::HashMap<u32, std::collections::HashSet<String>>,
    /// The lazily-created `import.meta` object (one per Vm; host-defined,
    /// ordinary extensible null-proto). 0 = not yet allocated.
    import_meta: u32,
    /// AgentCanSuspend: false when launched with ZIPP_CAN_BLOCK=0 (the
    /// test262 CanBlockIsFalse harness mode) — Atomics.wait then throws.
    can_block: bool,
    /// The body promise of the most recent `import_module` whose top-level
    /// await SUSPENDED (still Pending on return). Taken by the dynamic-import
    /// site (which settles its promise from it) or by a static importer
    /// (which rejects — stage-1 TLA). GC ROOT while set.
    pending_module_body: Option<Value>,
    /// Body/capability promises of STATIC dependencies that suspended at
    /// top-level await, collected during the CURRENT import_module's link
    /// (mark/split_off discipline keeps nested links separate). GC ROOTS.
    link_pending_deps: Vec<Value>,
    /// Registered Atomics.waitAsync waiters: (buffer heap idx, byte address,
    /// pending promise idx, optional deadline, global-registry waiter id).
    /// `notify` (local or via the cross-thread mailbox) resolves matching
    /// entries "ok"; a due deadline resolves "timed-out" in the event loop —
    /// unless its registry entry is already gone (a notify won the race).
    /// Buffer + promise indices are GC ROOTS; the id needs no rooting.
    async_waiters: Vec<(u32, usize, u32, Option<std::time::Instant>, u64)>,
    /// `$262.agent.setTimeout` macrotasks: (due, callback). Callback Values
    /// are GC ROOTS.
    timer_queue: Vec<(std::time::Instant, Value)>,
    /// VM construction time — the `$262.agent.monotonicNow()` epoch.
    vm_start: std::time::Instant,
    /// Cross-agent state for the `$262.agent` worker subsystem (report FIFO +
    /// one handle per started agent). `None` until the first `agent.start` —
    /// a run that never starts an agent pays nothing; each worker Vm holds a
    /// clone of the same Arc.
    agent_shared: Option<std::sync::Arc<agents::AgentShared>>,
    /// Whether this Vm is the CLI entry agent (`Main`) or an `agent.start`
    /// worker thread's Vm (`Worker`).
    agent_role: agents::AgentRole,
    /// Worker-side `$262.agent.receiveBroadcast` callback, invoked as
    /// `cb(sab, id)` for each broadcast retrieved. GC ROOT (gc.rs).
    broadcast_cb: Value,
    /// This Vm's cross-thread `Atomics.waitAsync` wake channel: a `notify`
    /// in ANY agent pushes a woken waiter's id here and signals the condvar;
    /// the event loop sleeps on that condvar and drains the ids first each
    /// iteration. Created unconditionally so a remote notify never races a
    /// lazy init; holds no Values (nothing to root).
    mailbox: std::sync::Arc<agents::Mailbox>,
    /// The ShadowRealm whose code is CURRENTLY evaluating (heap index of the
    /// realm object), if any — global-name resolution inside `evaluate` binds
    /// non-builtin names to that realm's own slot table.
    active_realm: Option<u32>,
    /// Per-ShadowRealm global bindings: realm object idx → name → live global
    /// slot. Slot values live in `globals` (rooted wholesale); the map is
    /// pruned when its realm object dies.
    realm_globals: std::collections::HashMap<u32, std::collections::HashMap<String, u32>>,
    /// Canonical paths of modules whose BODY is currently executing — a
    /// deferred-namespace trigger targeting one is a TypeError (you cannot
    /// synchronously evaluate a module that is already mid-evaluation).
    executing_modules: std::collections::HashSet<std::path::PathBuf>,
    /// Modules whose evaluation completed ABRUPTLY: the thrown error is
    /// permanent — every later import re-throws it without re-running the
    /// body. Values are GC ROOTS.
    module_errors: std::collections::HashMap<std::path::PathBuf, Value>,
    /// Per-module DEFERRED namespace singleton (`import defer * as ns`):
    /// canonical path → the deferred namespace object. Values are GC ROOTS.
    deferred_ns_cache: std::collections::HashMap<std::path::PathBuf, Value>,
    /// Deferred namespaces NOT YET evaluated: object idx → module path.
    /// Removed when a triggering access evaluates the module.
    deferred_ns_state: std::collections::HashMap<u32, std::path::PathBuf>,
    /// Async modules waiting on pending dependencies: capability-promise
    /// index → the state needed to run the body once the last dependency
    /// settles. Keys are GC roots (the capability promise must survive).
    deferred_mods: std::collections::HashMap<u32, DeferredModuleExec>,
    /// Canonical paths of modules whose IMPORT RESOLUTION is in flight —
    /// guards static-import cycles (self-imports alias instead).
    module_loading: std::collections::HashSet<std::path::PathBuf>,
    /// Static `export … from` (exported, imported, specifier) + `export *`
    /// specifiers + base dir of modules whose LINK is in flight: a cyclic
    /// resolve_export back into one walks these statically (spec
    /// ResolveExport through a cycle) instead of failing or re-evaluating.
    module_pending_reexports: std::collections::HashMap<
        std::path::PathBuf,
        (Vec<(String, String, String)>, Vec<String>, Option<std::path::PathBuf>),
    >,
    /// `[[HomeObject]]` for OBJECT-LITERAL methods/accessors (and arrows nested in
    /// them), keyed by the closure's heap index → the object the method was defined
    /// in. `super.x` in such a method resolves via GetPrototypeOf(home). Class
    /// methods use the compile-time `home_class_id` path instead. Values are GC roots;
    /// dead closure keys are pruned on sweep.
    closure_home: std::collections::HashMap<u32, Value>,
    /// The lazily-compiled `Array.fromAsync` JS polyfill (an async function value).
    /// Compiled on first call via `do_eval`, then cached + GC-rooted. `None` until
    /// the first `Array.fromAsync(...)` invocation.
    from_async_fn: Option<Value>,
    /// The lazily-compiled `%AsyncIteratorPrototype%[@@asyncDispose]` JS polyfill
    /// (an async function). Compiled on first call via `do_eval`, cached + GC-rooted.
    async_dispose_fn: Option<Value>,
    /// `DisposableStack` ctor + prototype. An instance is a plain Object linked to
    /// `disposablestack_proto`; its disposer stack + disposed flag live in
    /// `dispose_stacks` (the disposers are zero-arg callable thunks, GC-traced).
    disposablestack_ctor: u32,
    disposablestack_proto: u32,
    dispose_stacks: std::collections::HashMap<u32, (Vec<Value>, bool)>,
    /// Per-block `using`-declaration resource scopes, keyed by a monotonic id
    /// (`using_next_id`) that the `OpenUsingScope` op hands back in a register.
    /// Each value is the scope's list of disposers (a @@dispose method bound to its
    /// resource value), pushed by `RegisterDisposable` and drained LIFO by
    /// `DisposeScope` on block exit. The disposers are GC roots (see gc.rs); the
    /// entry is removed when its `DisposeScope` runs.
    using_resources: std::collections::HashMap<u32, Vec<Value>>,
    using_next_id: u32,
    /// `AsyncDisposableStack` ctor + prototype, and the set of dispose-stack
    /// instances that are ASYNC (their `use` prefers @@asyncDispose and their
    /// disposal goes through `disposeAsync`, which returns a Promise).
    asyncdisposablestack_ctor: u32,
    asyncdisposablestack_proto: u32,
    async_stacks: std::collections::HashSet<u32>,
    /// In-flight `AsyncDisposableStack.prototype.disposeAsync` drivers, keyed
    /// by the capability promise heap index. Each disposer's result is awaited
    /// before the next runs (the DISPOSE_ASYNC_STEP reactions re-enter
    /// `dispose_async_drive`). GC: keys + held Values are roots (gc.rs).
    dispose_async_state: std::collections::HashMap<u32, DisposeAsyncState>,
    /// `SuppressedError` ctor + prototype (ES2026). Unlike the 8 standard errors,
    /// it carries `error` + `suppressed` own properties; its prototype chains to
    /// %Error.prototype% so `instanceof Error` holds.
    suppressederror_ctor: u32,
    suppressederror_proto: u32,
    /// `ShadowRealm` ctor + prototype, and the set of branded instances. Note the
    /// realm is NOT truly isolated (evaluate reuses the shared global eval path);
    /// isolation-specific tests won't pass, but structure + evaluate of a string
    /// to a primitive + the argument/return TypeError checks do.
    shadowrealm_ctor: u32,
    shadowrealm_proto: u32,
    shadow_realms: std::collections::HashSet<u32>,
    /// `$262.createRealm()` realm registry. `realms[r]` maps a MAIN-realm intrinsic
    /// prototype heap index to realm `r`'s corresponding prototype (realm 0 = the
    /// main realm, an empty map). `obj_realm` tags a heap index (a realm's
    /// constructor, its prototype, or an object created in it) with its realm id —
    /// `GetFunctionRealm`. Used so `Reflect.construct(C, args, newTargetFromRealmR)`
    /// with a non-object `newTarget.prototype` falls back to realm R's `%C.prototype%`.
    realms: Vec<std::collections::HashMap<u32, u32>>,
    obj_realm: std::collections::HashMap<u32, u32>,
    /// A realm constructor's heap index → the MAIN-realm constructor it mirrors, so
    /// `new other.Array()` / `other.Symbol('x')` route to the real construction /
    /// call behaviour (with the realm's prototype + realm tag).
    realm_ctor_main: std::collections::HashMap<u32, u32>,
    /// An explicit `fn.prototype = value` assignment, for ANY value — including a
    /// NON-object (undefined/null/primitive) which the `prototypes` map (heap-only)
    /// can't hold. Consulted FIRST by the `.prototype` read / `prototype_of` /
    /// getOwnPropertyDescriptor, so a function whose prototype was set to a
    /// non-object reports it (and OrdinaryCreateFromConstructor then falls back).
    fn_proto_override: std::collections::HashMap<u32, Value>,
    /// Heap indices of `[[IsHTMLDDA]]` exotic objects (`$262.IsHTMLDDA`, the
    /// `document.all` emulation): `typeof` is "undefined", loose `== null`/`==
    /// undefined` is true, ToBoolean is false, and calling it returns undefined —
    /// yet it is otherwise an ordinary object (and `Object.is`/`===` do NOT treat
    /// it as undefined).
    is_htmldda: std::collections::HashSet<u32>,
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
    /// `%AsyncGeneratorPrototype%` — async-generator instances: next/return/throw
    /// (returning Promises) + @@asyncIterator + @@toStringTag, chaining to
    /// %AsyncIteratorPrototype%.
    asyncgen_proto: u32,
    /// The default `Array.prototype[Symbol.iterator]` (the `values` function), so
    /// array destructuring can fast-path plain arrays yet still honour a replaced
    /// `Array.prototype[Symbol.iterator]` (drain via the iterator protocol).
    default_array_iter: Value,
    /// The pristine %ArrayIteratorPrototype%.next (see default_array_iter).
    default_array_iter_next: Value,
    /// The single canonical %ThrowTypeError% intrinsic — shared by
    /// Function.prototype.{caller,arguments} and a strict (unmapped) arguments
    /// object's `callee` poison-pill, so all references compare `===`.
    throw_type_error: Value,
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
mod agents;
mod helpers_misc;
mod helpers_datetime;
mod helpers_numeric;
mod helpers_json;
pub(crate) mod helpers_num2;
mod gc;

pub(crate) use helpers_misc::*;
pub(crate) use helpers_datetime::*;
pub(crate) use helpers_numeric::*;
pub(crate) use helpers_json::*;
pub(crate) use helpers_num2::*;
