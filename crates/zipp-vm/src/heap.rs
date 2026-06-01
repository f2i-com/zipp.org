//! Heap object storage.
//!
//! Heap values are referenced by a `u32` index packed into a [`crate::value::Value`].
//! Reference semantics fall out naturally: copying a `Value` copies the index,
//! so `let b = a` makes `a` and `b` alias the same heap slot, and a mutation
//! through either is visible through both — exactly JS object/array semantics.
//!
//! v1 does not reclaim memory (programs are short-lived per `eval`); a real GC
//! slots in here later without touching the value representation. Objects use a
//! simple insertion-ordered property list, which preserves JS string-key
//! enumeration order and is correct (if not yet fast — shapes/inline-caches are
//! a later tier).

use crate::value::Value;
use std::borrow::Cow;

/// A JS object: insertion-ordered string-keyed properties.
#[derive(Clone, Debug, Default)]
pub struct ObjMap {
    pub keys: Vec<String>,
    pub vals: Vec<Value>,
    /// Per-property attributes, parallel to `keys`/`vals` (a property descriptor's
    /// writable/enumerable/configurable + accessor get/set). For a DATA property
    /// `vals[i]` is the value; for an ACCESSOR `vals[i]` is the getter and
    /// `attrs[i].setter` the setter.
    pub attrs: Vec<PropAttr>,
    /// Heap index of the class this object is an instance of (`new C()`), used
    /// for prototype-style method lookup and `instanceof`. `None` for a plain
    /// object literal. Own properties (the fields) live in `keys`/`vals`;
    /// methods are resolved through the class, so they stay non-enumerable.
    pub class: Option<u32>,
    /// `[[Extensible]]`: whether new own properties may be added. Cleared by
    /// `Object.preventExtensions`/`seal`/`freeze`. Default `true`.
    pub extensible: bool,
    /// True for the built-in constructor globals (Object/Array/Map/…), which are
    /// modelled as objects but are callable constructors: `typeof` reports
    /// "function" and they satisfy IsConstructor. False for ordinary objects and
    /// the namespace globals (Reflect/Math/JSON).
    pub is_ctor: bool,
}

/// One property's attributes — the ECMAScript property-descriptor flags plus an
/// accessor pair. A data property uses `writable` and the parallel `vals` entry;
/// an accessor (`accessor == true`) uses `vals[i]` as the getter and `setter`.
#[derive(Clone, Copy, Debug)]
pub struct PropAttr {
    pub writable: bool,
    pub enumerable: bool,
    pub configurable: bool,
    pub accessor: bool,
    /// The setter function for an accessor property (`UNDEFINED` if none / data).
    pub setter: Value,
}

impl PropAttr {
    /// The default attributes for an ordinary created property (`obj.x = v`,
    /// object literals): a writable, enumerable, configurable data property.
    pub fn data() -> PropAttr {
        PropAttr { writable: true, enumerable: true, configurable: true, accessor: false, setter: Value::UNDEFINED }
    }
}

impl ObjMap {
    pub fn new() -> ObjMap {
        ObjMap { keys: Vec::new(), vals: Vec::new(), attrs: Vec::new(), class: None, extensible: true, is_ctor: false }
    }

    /// `Object.isSealed`: not extensible and every own property non-configurable.
    pub fn is_sealed(&self) -> bool {
        !self.extensible && self.attrs.iter().all(|a| !a.configurable)
    }

    /// `Object.isFrozen`: sealed and every own data property non-writable.
    pub fn is_frozen(&self) -> bool {
        !self.extensible && self.attrs.iter().all(|a| !a.configurable && (a.accessor || !a.writable))
    }

    /// `Object.seal`: clear extensibility and make every own property non-configurable.
    pub fn seal(&mut self) {
        self.extensible = false;
        for a in &mut self.attrs {
            a.configurable = false;
        }
    }

    /// `Object.freeze`: seal, and make every own data property non-writable too.
    pub fn freeze(&mut self) {
        self.extensible = false;
        for a in &mut self.attrs {
            a.configurable = false;
            if !a.accessor {
                a.writable = false;
            }
        }
    }

    pub fn pos(&self, key: &str) -> Option<usize> {
        self.keys.iter().position(|k| k == key)
    }

    /// The raw stored value for `key` (a data value, or an accessor's getter).
    /// Callers that must honour accessors check `attrs[i].accessor` first.
    pub fn get(&self, key: &str) -> Option<Value> {
        self.pos(key).map(|i| self.vals[i])
    }

    /// Set `key = val` as a DATA property. Returns `true` if a NEW key was
    /// appended (which may have reallocated `vals`), `false` if an existing slot
    /// was overwritten. New keys get default data attributes; existing keys keep
    /// their attributes (only the value changes). The JIT inline cache uses the
    /// return to bump the object's version on a key-add.
    pub fn set(&mut self, key: &str, val: Value) -> bool {
        if let Some(i) = self.pos(key) {
            self.vals[i] = val;
            false
        } else {
            self.keys.push(key.to_string());
            self.vals.push(val);
            self.attrs.push(PropAttr::data());
            true
        }
    }

    /// Define `key` with explicit attributes (`Object.defineProperty`, or a method
    /// with non-default enumerability). Overwrites any existing slot. Returns
    /// `true` if a new key was appended.
    pub fn define(&mut self, key: &str, val: Value, attr: PropAttr) -> bool {
        if let Some(i) = self.pos(key) {
            self.vals[i] = val;
            self.attrs[i] = attr;
            false
        } else {
            self.keys.push(key.to_string());
            self.vals.push(val);
            self.attrs.push(attr);
            true
        }
    }

    /// Remove `key`'s own property; returns whether it existed. Shifts later
    /// slots, so the caller MUST bump the object's version (a JIT inline cache
    /// may have recorded a now-stale slot index for another key).
    pub fn remove(&mut self, key: &str) -> bool {
        if let Some(i) = self.pos(key) {
            self.keys.remove(i);
            self.vals.remove(i);
            self.attrs.remove(i);
            true
        } else {
            false
        }
    }
}

/// A flat (contiguous) JS string with cached metadata so `.length` and indexing
/// are O(1). `char_len` is the Unicode-scalar count (the engine measures
/// `.length` in scalars throughout); `ascii` flags the common all-ASCII case,
/// where the i-th character is the i-th byte — O(1) random access. Non-ASCII
/// strings fall back to an O(i) `chars().nth(i)` walk (correct, just slower).
#[derive(Clone, Debug)]
pub struct JsStr {
    pub bytes: String,
    pub char_len: usize,
    pub ascii: bool,
}

impl JsStr {
    pub fn new(bytes: String) -> JsStr {
        let ascii = bytes.is_ascii();
        let char_len = if ascii { bytes.len() } else { bytes.chars().count() };
        JsStr { bytes, char_len, ascii }
    }
}

/// A generator's execution state. `Suspended(ip)` parks at the bytecode index of
/// the `Yield` that paused it (resume re-decodes that op to deliver the sent
/// value into its `dst`, then continues at `ip + 1`); `ip == 0` is the
/// not-yet-started state (the first `next()` runs from the top).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GenState {
    Suspended(usize),
    Running,
    Completed,
}

/// An active `try` handler in a frame, innermost last. A `Catch` lands a thrown
/// value in `reg` and jumps to `target`. A `Finally` is visited on EVERY exit
/// from its protected region — throw, `return`, or normal completion — running
/// the finally block (at `target`) with a completion record deposited into
/// `kind_reg` (0 normal, 1 return, 2 throw) and `val_reg` (the return value /
/// thrown reason), which `EndFinally` then resumes.
#[derive(Clone, Copy, Debug)]
pub enum Handler {
    Catch { target: u32, reg: u16 },
    Finally { target: u32, kind_reg: u16, val_reg: u16 },
}

/// A Promise's settlement state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromiseState {
    Pending,
    Fulfilled,
    Rejected,
}

/// Which Promise combinator a `Combinator` is tracking.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CombKind {
    /// `Promise.all` — fulfil with all values, or reject on the first rejection.
    All,
    /// `Promise.allSettled` — fulfil with `{status, value|reason}` records.
    AllSettled,
    /// `Promise.race` — settle as the first input settles.
    Race,
    /// `Promise.any` — first fulfilment, or an AggregateError if all reject.
    Any,
}

/// A registered reaction on a pending promise: when it settles, `callback` runs
/// (as a microtask) and its outcome settles `dependent`. A `callback` of
/// `undefined` is a pass-through (the value/reason forwards to `dependent`).
#[derive(Clone, Debug)]
pub struct Reaction {
    pub callback: Value,
    pub dependent: u32,
    /// A `.finally(cb)` reaction: run `callback` (no args) for its side effect,
    /// then forward the ORIGINAL value/reason (a throw in `callback` overrides).
    pub finally: bool,
    /// An `await` reaction: `dependent` is the suspended async ACTIVATION's heap
    /// index, resumed (value or thrown rejection) instead of running a callback.
    pub is_async: bool,
}

/// Boxed payload of a [`HeapObj::Class`] (see that variant's docs). Kept behind a
/// `Box` so the rarely-allocated class object — 8 fields incl. a `String`, three
/// `Vec`s and an `ObjMap` — does not inflate `size_of::<HeapObj>()` for the hot,
/// tiny variants (`Cons`/`Str`/`Array`/`Object`) that pay it on every alloc.
#[derive(Clone, Debug)]
pub struct ClassData {
    pub name: String,
    pub ctor: Option<u32>,
    /// Whether `ctor` is an explicit constructor (its body calls `super`
    /// itself) vs. a fields-only proto (the `new` path runs the parent ctor).
    pub has_explicit_ctor: bool,
    pub methods: Vec<(String, Value)>,
    /// `get x()` accessors, invoked with `this` = instance on property read.
    pub getters: Vec<(String, Value)>,
    /// `set x(v)` accessors, invoked with `this` = instance on property write.
    pub setters: Vec<(String, Value)>,
    /// Static members — own properties of the class value (`C.method`,
    /// `C.field`). Methods start here; static fields are added by SetProp.
    pub statics: ObjMap,
    /// `static get`/`set` accessors, invoked with `this` = the class value on
    /// read/write of a static property.
    pub static_getters: Vec<(String, Value)>,
    pub static_setters: Vec<(String, Value)>,
    /// Heap index of the superclass value (`class C extends P`), for
    /// inherited method/getter lookup and `instanceof` up the chain.
    pub parent: Option<u32>,
    /// Computed instance-field keys (`[expr] = v`), evaluated ONCE at class
    /// definition (in source order) and read per-instance by the `FieldInit` op
    /// during construction. Empty for classes with no computed instance fields.
    pub computed_field_keys: Vec<Value>,
}

/// Boxed payload of a [`HeapObj::AsyncState`] (see that variant's docs). Boxed for
/// the same reason as [`ClassData`]: it carries two `Vec`s and so is one of the
/// largest variants, but is allocated only when an `async` function suspends.
#[derive(Clone, Debug)]
pub struct AsyncStateData {
    pub func: u32,
    pub closure: u32,
    pub state: GenState,
    pub regs: Vec<Value>,
    pub result: u32,
    pub handlers: Vec<Handler>,
}

/// Payload of [`HeapObj::AsyncGenerator`] (an `async function*` activation). Like
/// a generator (suspend/resume on `yield`) AND an async activation (suspend on
/// `await`), so it carries the saved window + handlers like both. `queue` holds
/// the result promises of `.next()`/`.return()`/`.throw()` calls awaiting the
/// next yield/return (FIFO) — each `.next()` returns a Promise.
#[derive(Clone, Debug)]
pub struct AsyncGenState {
    pub func: u32,
    pub closure: u32,
    pub state: GenState,
    pub regs: Vec<Value>,
    pub handlers: Vec<Handler>,
    pub queue: Vec<u32>,
}

/// A heap-allocated object.
#[derive(Clone, Debug)]
pub enum HeapObj {
    /// An owned, contiguous JS string (with cached length / ASCII metadata).
    Str(JsStr),
    /// A lazily-concatenated string ("rope" / cons-string, as in V8). `left` and
    /// `right` are heap indices of string-like objects (flat `Str` or nested
    /// `Cons`); `len` is the total character count, so `.length` is O(1) without
    /// materializing. `+` builds one in O(1) instead of copying both operands;
    /// it is flattened to a contiguous `Str` in place on first content access
    /// (indexing, methods, comparison). JS strings are immutable here
    /// (`set_index` no-ops on them), so the structural sharing is sound.
    Cons { left: u32, right: u32, len: usize },
    /// A plain function: index into `Program::functions`. No captured state.
    Func(u32),
    /// A closure: a function id plus captured upvalue cells (indices of `Cell`
    /// heap objects). Captured variables are boxed into cells so mutation is
    /// shared between the closure and its defining scope.
    Closure { func: u32, upvalues: Vec<u32> },
    /// A boxed mutable variable cell (an upvalue's storage).
    Cell(Value),
    /// A bound function (`fn.bind(thisArg, ...boundArgs)`): calling it invokes
    /// `target` with `this` fixed to `this` and `args` prepended to the call args.
    Bound { target: Value, this: Value, args: Vec<Value> },
    /// A built-in (native) function value, identified by a small id (see the
    /// `native` ids in vm.rs). Callable as a first-class value — this is what backs
    /// `Object.defineProperty`, `Array.isArray`, `Object.prototype.hasOwnProperty`,
    /// `Function.prototype.call`, etc. when accessed as values (not just called).
    Native(u16),
    /// A dense array.
    Array(Vec<Value>),
    /// A plain object.
    Object(ObjMap),
    /// A JS Promise. `result` holds the fulfillment value / rejection reason
    /// (undefined while Pending); `fulfill`/`reject` are reactions registered
    /// while Pending (drained as microtasks on settle). `handled` tracks whether
    /// a rejection handler was attached (for optional unhandled-rejection report).
    Promise {
        state: PromiseState,
        result: Value,
        fulfill: Vec<Reaction>,
        reject: Vec<Reaction>,
        handled: bool,
    },
    /// A native `resolve`/`reject` function bound to a promise — the pair handed
    /// to a `new Promise(executor)`. Calling it settles `promise`.
    BoundResolver { promise: u32, is_reject: bool },
    /// A `Date`: milliseconds since the Unix epoch (NaN = Invalid Date). The
    /// engine treats all component getters/setters as UTC (a documented
    /// simplification — node uses the host time zone for the non-UTC ones).
    Date(f64),
    /// Shared state for a Promise combinator (`all`/`allSettled`/`race`/`any`).
    /// `results` collects per-input outcomes (sized to the input count);
    /// `remaining` counts inputs still outstanding; `result` is the combinator's
    /// own promise (settled when the combinator's condition is met).
    Combinator { kind: CombKind, results: Vec<Value>, remaining: u32, result: u32 },
    /// A native reaction that performs one combinator step when its subscribed
    /// input settles — identifying the `combinator` and the input's `index`.
    CombinatorResolver { combinator: u32, index: u32 },
    /// A suspended generator (`function*`). Owns a DETACHED register window (off
    /// the contiguous live `regs` Vec, so the JIT's pinned-capacity invariant
    /// holds while parked); `func`/`closure` re-create the frame on resume, and
    /// `state` carries the resume ip / completion. v1 does not preserve `try`
    /// handlers across a yield.
    Generator { func: u32, closure: u32, state: GenState, regs: Vec<Value> },
    /// An `async function*` activation — see [`AsyncGenState`]. Its `.next()`
    /// returns a Promise; the body may both `yield` and `await`.
    AsyncGenerator(Box<AsyncGenState>),
    /// A suspended `async function` activation — like Generator (detached window
    /// resumed at each `await`) but it also owns its `result` Promise's heap index
    /// and PRESERVES `try` handlers across an await (so `try { await p } catch`
    /// works). `handlers` are (catch_target, catch_reg) pairs.
    AsyncState(Box<AsyncStateData>),
    /// A JS `Map`: insertion-ordered (key, value) entries with SameValueZero key
    /// equality. Parallel `keys`/`vals` Vecs (small Maps dominate; linear scan).
    Map { keys: Vec<Value>, vals: Vec<Value> },
    /// A JS `Set`: insertion-ordered unique values (SameValueZero equality).
    Set(Vec<Value>),
    /// A JS `WeakMap`: like `Map` but keys must be objects and there is no
    /// iteration/size (a distinct type so the [[WeakMapData]] brand check works —
    /// `WeakMap.prototype.set.call(aMap)` must throw). No GC, so refs stay strong.
    WeakMap { keys: Vec<Value>, vals: Vec<Value> },
    /// A JS `WeakSet`: like `Set` but values must be objects, no iteration/size.
    WeakSet(Vec<Value>),
    /// A JS `WeakRef`: a weak reference to an object. No GC, so `deref()` always
    /// returns the (still-live) target.
    WeakRef(Value),
    /// A JS `FinalizationRegistry`: holds a cleanup callback and the live
    /// unregister tokens. No GC, so cleanup never fires (spec-permitted); only
    /// `register`/`unregister` are observable. `tokens` tracks unregister tokens.
    FinalizationRegistry { cleanup: Value, tokens: Vec<Value> },
    /// A boxed primitive wrapper (`new String(x)`/`new Number(x)`/`new Boolean(x)`,
    /// or `Object(primitive)`). `kind` 0=String/1=Number/2=Boolean; `value` is the
    /// wrapped primitive ([[PrimitiveValue]]). `typeof` is "object"; valueOf returns
    /// the value; the kind's prototype provides the methods.
    Boxed { kind: u8, value: Value },
    /// A JS `RegExp`. `regex` is the compiled `regress` engine (ECMAScript regex);
    /// `source` is the pattern text, `flags` the JS flag string (`"gi"`); `last_index`
    /// is the mutable `lastIndex` (a char offset, for `g`/`y` stateful matching).
    RegExp { regex: Box<regress::Regex>, source: String, flags: String, last_index: usize },
    /// A JS `BigInt` primitive. Stored as `i128` (covers the common test262
    /// magnitudes; true arbitrary precision is a later refinement). Compared by
    /// VALUE (`1n === 1n`), not identity; `typeof` is "bigint".
    BigInt(i128),
    /// A JS `Symbol` primitive. Identity is the heap index (so `===` and use as a
    /// property key dedupe correctly). `desc` is the description (a string Value or
    /// UNDEFINED). `prop_key` is the internal string under which the symbol is
    /// stored as an object property — `"@@iterator"` etc. for the well-known
    /// symbols (matching the engine's existing iterator-key convention) and
    /// `"@@sym:N"` for user symbols. `typeof` is "symbol".
    Symbol { desc: Value, prop_key: String },
    /// A built-in iterator (Array/Map/Set `entries()`/`keys()`/`values()` and the
    /// default `@@iterator`). A snapshot of the values to yield plus a cursor;
    /// `proto` is its prototype heap index (%ArrayIteratorPrototype% etc., distinct
    /// per collection). `.next()` yields `items[index]` then advances.
    Iterator { items: Vec<Value>, index: usize, proto: u32 },
    /// A class value (`class C {…}`). Fields live in the boxed [`ClassData`]:
    /// `ctor` is the func id that runs instance field initializers then the user
    /// constructor (or `None`); `methods` maps each instance method name to its
    /// func id. `new C(args)` builds a plain object, links it to its class for
    /// method lookup, and runs the ctor with `this` = the new object.
    Class(Box<ClassData>),
}

/// Heap index of the interned empty string. The 128 single-ASCII-char strings
/// occupy indices `0..128`; the empty string is `128`; user objects start at
/// `129` (see [`Heap::new`]).
pub const INTERN_EMPTY: u32 = 128;

pub struct Heap {
    objs: Vec<HeapObj>,
    /// Per-object version, parallel to `objs` (one `u32` per heap object). Bumped
    /// whenever an object gains a NEW key (which may reallocate its `vals`). The
    /// JIT inline cache reads this (by heap index) to validate a cached
    /// `vals`-pointer: a matching version proves `vals` hasn't reallocated since
    /// the cache was filled. Allocated in lockstep with `objs` so indices align.
    versions: Vec<u32>,
}

impl Default for Heap {
    fn default() -> Self {
        Heap::new()
    }
}

impl Heap {
    pub fn new() -> Heap {
        // Pre-intern the 128 single-ASCII-char strings (indices 0..128) and the
        // empty string (index 128). These are immutable and ubiquitous — every
        // `s[i]` and every `s += <digit>` produces one — so sharing a single
        // heap slot eliminates per-iteration allocation in string loops.
        let mut objs = Vec::with_capacity(160);
        let mut versions = Vec::with_capacity(160);
        for b in 0u8..128 {
            objs.push(HeapObj::Str(JsStr { bytes: (b as char).to_string(), char_len: 1, ascii: true }));
            versions.push(0);
        }
        objs.push(HeapObj::Str(JsStr { bytes: String::new(), char_len: 0, ascii: true }));
        versions.push(0);
        Heap { objs, versions }
    }

    #[inline]
    pub fn alloc(&mut self, obj: HeapObj) -> u32 {
        let idx = self.objs.len() as u32;
        self.objs.push(obj);
        self.versions.push(0);
        idx
    }

    /// Bump object `idx`'s version (call after a key-add reallocates its `vals`).
    ///
    /// The counter is `u32`. A false inline-cache hit would require it to wrap
    /// (2^32 key-adds to a SINGLE object); that is ~36 GB of keys on one object
    /// (OOM long before), and the cache is re-filled on every miss, so it is
    /// practically unreachable. A `u64` would remove even the theoretical edge.
    #[inline]
    pub fn bump_version(&mut self, idx: u32) {
        self.versions[idx as usize] = self.versions[idx as usize].wrapping_add(1);
    }

    /// Base pointer of the parallel version array (for the JIT inline cache). The
    /// array does not reallocate during a native region run (a region never
    /// allocates a heap object), so this stays valid for the run.
    #[inline]
    pub fn versions_ptr(&self) -> *const u32 {
        self.versions.as_ptr()
    }

    /// Current version of object `idx` (for filling an inline-cache entry).
    #[inline]
    pub fn version_of(&self, idx: u32) -> u32 {
        self.versions[idx as usize]
    }

    #[inline]
    pub fn get(&self, idx: u32) -> &HeapObj {
        &self.objs[idx as usize]
    }

    #[inline]
    pub fn get_mut(&mut self, idx: u32) -> &mut HeapObj {
        &mut self.objs[idx as usize]
    }

    #[inline]
    pub fn alloc_str(&mut self, s: String) -> u32 {
        // Reuse the interned slot for the empty string and single-ASCII-char
        // strings instead of allocating (see `Heap::new`). Safe because strings
        // are immutable — nothing ever mutates a heap string in place.
        match s.len() {
            0 => return INTERN_EMPTY,
            1 => {
                let b = s.as_bytes()[0];
                if b < 128 {
                    return b as u32;
                }
            }
            _ => {}
        }
        self.alloc(HeapObj::Str(JsStr::new(s)))
    }

    /// Allocate a rope node over two string-like children (O(1) concatenation).
    #[inline]
    pub fn alloc_cons(&mut self, left: u32, right: u32, len: usize) -> u32 {
        self.alloc(HeapObj::Cons { left, right, len })
    }

    /// Is this heap object a string — flat `Str` or rope `Cons`?
    #[inline]
    pub fn is_str_like(&self, idx: u32) -> bool {
        matches!(self.get(idx), HeapObj::Str(_) | HeapObj::Cons { .. })
    }

    /// Character length of a string-like object — O(1): a rope stores it; a flat
    /// `JsStr` caches it (computed once in `JsStr::new`). `None` if not a string.
    pub fn str_char_len(&self, idx: u32) -> Option<usize> {
        match self.get(idx) {
            HeapObj::Str(s) => Some(s.char_len),
            HeapObj::Cons { len, .. } => Some(*len),
            _ => None,
        }
    }

    /// `Some(true)` if the string-like object is empty (O(1)); `None` if not a
    /// string. Reads the cached/stored length rather than scanning the bytes.
    #[inline]
    pub fn str_is_empty(&self, idx: u32) -> Option<bool> {
        match self.get(idx) {
            HeapObj::Str(s) => Some(s.char_len == 0),
            HeapObj::Cons { len, .. } => Some(*len == 0),
            _ => None,
        }
    }

    /// Append the full character content of a (possibly rope) string to `out`.
    /// Iterative, not recursive: a `s += x` loop builds a left-leaning rope that
    /// can be thousands of nodes deep, which would overflow the stack.
    pub fn write_str(&self, idx: u32, out: &mut String) {
        // Explicit stack; push the right child then the left so the left is
        // popped (appended) first — preserving left-to-right concatenation.
        let mut stack = vec![idx];
        while let Some(n) = stack.pop() {
            match self.get(n) {
                HeapObj::Str(s) => out.push_str(&s.bytes),
                HeapObj::Cons { left, right, .. } => {
                    stack.push(*right);
                    stack.push(*left);
                }
                _ => {}
            }
        }
    }

    /// Borrow a string-like as `&str` without allocating when it is already flat
    /// (the common case); materialize a rope into an owned `String` otherwise.
    /// `None` if `idx` isn't a string.
    pub fn str_cow(&self, idx: u32) -> Option<Cow<'_, str>> {
        match self.get(idx) {
            HeapObj::Str(s) => Some(Cow::Borrowed(s.bytes.as_str())),
            HeapObj::Cons { len, .. } => {
                let mut out = String::with_capacity(*len);
                self.write_str(idx, &mut out);
                Some(Cow::Owned(out))
            }
            _ => None,
        }
    }

    /// Content equality of two string-like objects. Fast (no allocation) when
    /// both are already flat — the common case for a hot `a === b` comparison.
    pub fn str_eq(&self, a: u32, b: u32) -> bool {
        match (self.get(a), self.get(b)) {
            (HeapObj::Str(x), HeapObj::Str(y)) => x.bytes == y.bytes,
            _ => {
                let (mut sa, mut sb) = (String::new(), String::new());
                self.write_str(a, &mut sa);
                self.write_str(b, &mut sb);
                sa == sb
            }
        }
    }

    /// Flatten the rope at `idx` into a contiguous `Str` in place. No-op if it is
    /// already flat (or not a string). The already-flat fast path is a single tag
    /// check, so this is cheap to call unconditionally before content access.
    #[inline]
    pub fn flatten(&mut self, idx: u32) {
        if matches!(self.objs[idx as usize], HeapObj::Cons { .. }) {
            self.flatten_cold(idx);
        }
    }

    #[cold]
    fn flatten_cold(&mut self, idx: u32) {
        let len = match &self.objs[idx as usize] {
            HeapObj::Cons { len, .. } => *len,
            _ => return,
        };
        let mut out = String::with_capacity(len);
        self.write_str(idx, &mut out);
        self.objs[idx as usize] = HeapObj::Str(JsStr::new(out));
    }

    /// Resolve a callable (plain function or closure) to its function id and
    /// upvalue list. Returns `None` for non-callables.
    #[inline]
    pub fn as_callable(&self, idx: u32) -> Option<(u32, &[u32])> {
        match self.get(idx) {
            HeapObj::Func(id) => Some((*id, &[])),
            HeapObj::Closure { func, upvalues } => Some((*func, upvalues.as_slice())),
            _ => None,
        }
    }

    #[inline]
    pub fn cell_get(&self, idx: u32) -> Value {
        match self.get(idx) {
            HeapObj::Cell(v) => *v,
            _ => Value::UNDEFINED,
        }
    }

    #[inline]
    pub fn cell_set(&mut self, idx: u32, v: Value) {
        if let HeapObj::Cell(slot) = self.get_mut(idx) {
            *slot = v;
        }
    }
}
