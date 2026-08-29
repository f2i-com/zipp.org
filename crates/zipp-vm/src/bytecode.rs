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

/// "No register" for an operand slot that an op may leave unused. Today only
/// `MathOp` uses it: `callee == NO_REG` marks the BARE form (see the op).
pub const NO_REG: Reg = Reg::MAX;

/// A bare `MathOp` whose `Math` global slot could not be encoded in `this_v`
/// (an eval/module re-index landed above the field's range): the op resolves
/// the slot by NAME at execution instead. Correct, slow, and never emitted by
/// the compiler itself -- only the re-index pass produces it.
pub const BARE_MATH_BY_NAME: Reg = Reg::MAX - 1;

/// Engine-private prefix carried by the Array returned from `ForInKeys`.
///
/// The snapshot is never exposed to JavaScript. Slots 0..6 hold the receiver,
/// the two default-prototype anchors, and each object's matching shape version;
/// actual enumerable key strings begin at this offset. Keeping the guard beside
/// the snapshot makes nested and re-entrant for-in loops independent without a
/// VM side table (and the Array keeps every Value here alive through GC).
pub const FORIN_SNAPSHOT_PREFIX: usize = 6;

/// `ClassAddMember.kind` flag: leave the ToPropertyKey'd key back in the `key`
/// register. An auto-accessor installs a get/set PAIR from one
/// ClassElementName, so the second instruction must reuse the first's coerced
/// key — re-coercing would run a `toString`/`@@toPrimitive` key a second time.
pub const KEY_WRITEBACK: u8 = 0x80;

/// One bytecode instruction. Kept as a fieldful enum (not packed bytes) for v1:
/// the dispatch cost of a wide enum is negligible next to correctness clarity,
/// and the JIT will consume this same structured form rather than re-decoding
/// bytes.
#[derive(Clone, Debug)]
pub enum Instr {
    /// `dst = <constant pool[idx]>`
    LoadConst {
        dst: Reg,
        idx: u32,
    },
    /// `dst = <small integer immediate>`
    LoadInt {
        dst: Reg,
        val: i32,
    },
    /// `dst = undefined`
    LoadUndefined {
        dst: Reg,
    },
    /// `dst = new.target` — the current activation's `new.target` (the constructor
    /// when entered via `new`/`Reflect.construct`/`super(...)`, else `undefined`).
    LoadNewTarget {
        dst: Reg,
    },

    /// `dst = <the function currently executing>` — materialises the running
    /// frame's own function value. Used to bind a named function expression's name
    /// to itself inside its body (`(function f(){ … f … })`), so self-reference and
    /// nested closures over the name resolve.
    LoadCallee {
        dst: Reg,
    },
    /// `dst = class_values[class_id]` — the inner immutable class-name binding
    /// visible to method/ctor/static-block bodies (and arrows inside them). A
    /// ReferenceError (TDZ) if the class value is not yet initialized.
    LoadClassValue {
        dst: Reg,
        class_id: u32,
    },
    /// `dst = <array hole>` — the internal HOLE sentinel, used only to fill an
    /// elided array-literal element (`[1,,3]`) before NewArray/ArrayAppend copies
    /// it into the array. Must never be observed outside that copy.
    LoadHole {
        dst: Reg,
    },
    /// `dst = null`
    LoadNull {
        dst: Reg,
    },
    /// `dst = true|false`
    LoadBool {
        dst: Reg,
        val: bool,
    },
    /// `dst = src`
    Move {
        dst: Reg,
        src: Reg,
    },

    /// `dst = globals[idx]`. Throws ReferenceError if the slot holds the
    /// never-declared sentinel (`Value::UNINITIALIZED`) — i.e. the name was
    /// referenced but never bound (`x` where no `var`/`let`/`function`/builtin/
    /// assignment ever defined it).
    LoadGlobal {
        dst: Reg,
        idx: u32,
    },
    /// `dst = globals[idx]`, but the never-declared sentinel reads as `undefined`
    /// instead of throwing. Emitted for `typeof <ident>`, where an unbound name
    /// must yield "undefined" rather than a ReferenceError.
    /// `dst = (typeof a) === TYPEOF_NAMES[code]` (`neg` flips it) — the fused
    /// form of `TypeOf` + `Eq`-against-a-string-literal, which is how `typeof`
    /// is almost always consumed. The unfused pair allocates a heap string per
    /// evaluation and then content-compares it; this compares the `&'static
    /// str` the classifier returns and allocates nothing. `code` indexes
    /// [`TYPEOF_NAMES`]; a literal that is not a possible `typeof` result
    /// compiles with code 255, which matches nothing (the comparison is a
    /// constant, but the operand is still evaluated for its effects).
    TypeOfIs {
        dst: Reg,
        a: Reg,
        code: u8,
        neg: bool,
    },
    LoadGlobalOrUndefined {
        dst: Reg,
        idx: u32,
    },
    /// Dynamic-first variants for code in a contains-direct-eval function (or
    /// an eval program): consult the activation's EvalScope (frame field or
    /// closure stamp) for the slot's NAME before the ordinary global slot.
    LoadGlobalDyn {
        dst: Reg,
        idx: u32,
    },
    LoadGlobalOrUndefinedDyn {
        dst: Reg,
        idx: u32,
    },
    StoreGlobalDyn {
        idx: u32,
        src: Reg,
    },
    /// Does the activation's EvalScope bind this slot's NAME right now?
    /// An assignment in a sloppy direct-eval zone snapshots its target
    /// reference with this BEFORE the RHS runs: a `var` the RHS's eval
    /// introduces is what later READS see, but not what this assignment
    /// writes (PutValue uses the already-resolved reference).
    EvalScopeHas {
        dst: Reg,
        idx: u32,
    },
    /// Write the activation's EXISTING EvalScope binding for the slot's name
    /// (the EvalScopeHas-true arm; falls back to the global slot if gone).
    EvalScopeSet {
        idx: u32,
        src: Reg,
    },
    /// `globals[idx] = src`
    StoreGlobal {
        idx: u32,
        src: Reg,
    },
    /// `globals[idx] = src`, but in STRICT mode: assignment to an unresolvable
    /// reference (a never-declared global slot, still UNINITIALIZED) throws a
    /// ReferenceError instead of creating the global (sloppy `StoreGlobal` creates).
    StoreGlobalStrict {
        idx: u32,
        src: Reg,
    },
    /// `globals[idx] = src` for a reference the SAME expression already read
    /// (`x++`, `x += 1`, `x ||= v`). GetValue having succeeded proves the
    /// reference is resolvable, so PutValue may not throw "is not defined";
    /// an uninitialized slot whose name is an own property of the global object
    /// writes through that property instead. Emitted only where a
    /// `load_binding` of the same binding precedes the store.
    StoreGlobalResolved {
        idx: u32,
        src: Reg,
    },

    /// A clock read: `performance.now()` (`epoch = false`, fractional ms since
    /// VM start) or `Date.now()` (`epoch = true`, integer ms since the Unix
    /// epoch). Both yield an f64 `Value`. Recognised at compile time so the
    /// common timing idiom works without a real global object model.
    Now {
        dst: Reg,
        epoch: bool,
    },

    // ── arithmetic (generic: operands may be any number) ──
    Add {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    Sub {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    Mul {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    Div {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    Mod {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    Neg {
        dst: Reg,
        a: Reg,
    },
    /// `dst = +a` — unary plus: coerce `a` to a number (ToNumber).
    ToNum {
        dst: Reg,
        a: Reg,
    },
    /// `dst = a <bitop> b` — bitwise/shift with JS int32 coercion of the operands
    /// (`>>>` coerces to uint32 and may yield a value above i32::MAX).
    Bitwise {
        dst: Reg,
        a: Reg,
        b: Reg,
        op: BitwiseOp,
    },
    /// `dst = a ** b` — exponentiation (f64 semantics).
    Pow {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    /// `dst = ~a` — bitwise NOT (int32 coercion).
    BitNot {
        dst: Reg,
        a: Reg,
    },

    /// `dst = a + <int immediate>` — the canonical `i + 1`, `n - 1` shape.
    /// `upd` distinguishes the two sources with DIFFERENT BigInt semantics:
    /// an UpdateExpression (`++`/`--`, upd=true) is ToNumeric and keeps a
    /// BigInt operand a BigInt (`n++` adds 1n), while the binary `x ± lit`
    /// fast path (upd=false) must throw the spec's mixing TypeError for a
    /// BigInt operand (`1n - 1` throws). Identical for Number operands (the
    /// JIT, which only handles numbers, ignores the flag).
    AddInt {
        dst: Reg,
        a: Reg,
        imm: i32,
        upd: bool,
    },

    /// `dst = a + b` — SEMANTICALLY IDENTICAL to `Add` (same operator, same
    /// coercion). A pure JIT routing hint emitted by a compile pass for the
    /// `s = s + x` string-accumulator shape: it routes the op to the helper-call
    /// (memory) OSR region instead of the numeric region, so a hot `s += …` loop
    /// JITs its control flow natively and calls a concat helper per step rather
    /// than running fully interpreted. Because the semantics equal `Add`, a
    /// mis-applied hint can only change performance, never results.
    StrConcat {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    /// `dst = a + (b + c)` with exact pairwise `+` semantics. The compiler
    /// emits this only for an identifier `+=` whose RHS is a single `b + c`
    /// pair with a definitely-string `b`. Flat ASCII
    /// `string + (string + {string,int})` is materialised directly into one
    /// result buffer; every other shape delegates to the two ordinary Adds in
    /// their original order (inner first, then outer).
    AddRightPair {
        dst: Reg,
        a: Reg,
        b: Reg,
        c: Reg,
        in_place: bool,
    },

    /// Exact literal-prefix `+` used by the benchmark's `pad2` leaves:
    /// `zero=true` is `"0" + src`, `zero=false` is `"" + src`. A tagged Int
    /// compatible with that branch returns the pinned `"00".."99"` primitive
    /// string; every other value executes the ordinary `+` with the literal.
    /// The omitted left expression is a side-effect-free String literal, so
    /// operand evaluation and ToPrimitive order are unchanged.
    Pad2Concat {
        dst: Reg,
        src: Reg,
        zero: bool,
    },

    /// Exact whole-conditional pad2 shape:
    /// `src < 10 ? "0" + src : "" + src`. The compiler emits this only when
    /// all three `src` references are the same stable plain local (or a strict
    /// parameter), so re-entry during relational coercion cannot change the
    /// second binding read. Tagged Ints 0..99 return the pinned two-digit slot;
    /// every other value executes the original relational comparison followed
    /// by the selected ordinary literal-prefix `+`, including both coercions.
    Pad2Conditional {
        dst: Reg,
        src: Reg,
    },

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
    StrAppendInPlace {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    /// Exact fusion of an adjacent `scratch = obj[key]` followed by the
    /// compiler-proven-linear `dst = a + scratch`. The compiler additionally
    /// proves `scratch` cannot be observed anywhere else and that control
    /// cannot enter at the removed append. Runtime may therefore read an
    /// in-range byte directly from a flat ASCII string and append it without
    /// materialising the indexed character; every other shape executes the
    /// original GetIndex then StrAppendInPlace/Add fallback in that order.
    /// `scratch` remains available to root the indexed Value on the slow path.
    StrAppendIndex {
        dst: Reg,
        a: Reg,
        obj: Reg,
        key: Reg,
        scratch: Reg,
    },

    /// `dst = a + b` — SEMANTICALLY IDENTICAL to `Add` for EVERY operand pair
    /// (numeric folding, BigInt rules and the mixing TypeError, Symbol
    /// TypeError, object ToPrimitive order and side effects, WTF-8 seam
    /// canonicalization). Emitted ONLY by the W11 (B124) n-ary concat-chain
    /// fusion in `FnCompiler::binary`/the template arm, for links 2.. of a
    /// flattened `L1+L2+…+Ln` spine: `acc = Add(L1,L2)` then per leaf
    /// `StrConcatChain{dst:acc, a:acc, b:leaf}` and a final `Move` to the
    /// caller's dst. The extra licence over `Add`: `a` is ALWAYS the dead,
    /// freshly-heap-allocated result of the immediately preceding chain link
    /// (an unnamed compiler temp — never a param, never Moved/stored/read
    /// elsewhere, unreachable from JS), so when it is a non-interned flat
    /// `Str` the runtime may grow its buffer IN PLACE instead of allocating.
    /// The first link stays a plain `Add` so a `LoadConst`'d (JIT-shared,
    /// pre-interned) string is never the in-place accumulator. Runtime =
    /// `Vm::add_values_chain`, shared verbatim by the interpreter arm, the
    /// MEM-region helper and the Tier-C helper (`jit_concat_chain`).
    StrConcatChain {
        dst: Reg,
        a: Reg,
        b: Reg,
    },

    // ── comparisons → boolean ──
    Lt {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    Le {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    Gt {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    Ge {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    /// strict `===`
    Eq {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    /// strict `!==`
    Ne {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    /// loose `==` (with type coercion)
    LooseEq {
        dst: Reg,
        a: Reg,
        b: Reg,
    },
    /// loose `!=` (with type coercion)
    LooseNe {
        dst: Reg,
        a: Reg,
        b: Reg,
    },

    Not {
        dst: Reg,
        a: Reg,
    },
    /// `dst = ToString(a)` — the string-hint coercion (ToPrimitive with the string
    /// hint: `@@toPrimitive("string")` / `toString` / `valueOf`). Used for template-
    /// literal substitutions, which `ToString` each `${…}` rather than going through
    /// `+` (whose default hint tries `valueOf` first — wrong for e.g. a Temporal
    /// value, whose `valueOf` throws but whose `toString` works).
    ToStr {
        dst: Reg,
        a: Reg,
    },
    /// `dst = typeof a` (a JS type-name string).
    TypeOf {
        dst: Reg,
        a: Reg,
    },
    /// Guarded `Array.isArray(a)`. `callee`/`this_v` snapshot the live member
    /// reference before `a` was evaluated; an intrinsic-identity miss performs
    /// an ordinary call with those values.
    IsArray {
        dst: Reg,
        a: Reg,
        callee: Reg,
        this_v: Reg,
    },
    /// `dst = JSON.stringify(val, _, space)` — `space` is the indentation arg
    /// (a number → that many spaces, a string → that string, else compact).
    JsonStringify {
        dst: Reg,
        val: Reg,
        space: Reg,
        callee: Reg,
        this_v: Reg,
    },
    /// `dst = JSON.parse(a)` — parse a JSON string; throws SyntaxError on invalid.
    JsonParse {
        dst: Reg,
        a: Reg,
        callee: Reg,
        this_v: Reg,
    },
    /// Append to array `arr`: when `spread`, append every element of `val` (an
    /// array, or a string's chars); otherwise push `val` as one element. Used to
    /// build array literals / call-arg lists containing `...spread`.
    ArrayAppend {
        arr: Reg,
        val: Reg,
        spread: bool,
    },
    /// `dst = [...src.slice(start)]` — the rest of an array (or a string's chars)
    /// from index `start`. Used by array destructuring's `[a, ...rest]`.
    ArrayRest {
        dst: Reg,
        src: Reg,
        start: u32,
    },
    /// Copy `src`'s own enumerable keys onto `target` (object literal `{...src}`).
    /// `src` may be an object, array, or string; null/undefined contribute none.
    ObjectSpread {
        target: Reg,
        src: Reg,
    },
    /// `dst = { ...src } minus the keys` — object rest in destructuring
    /// (`let {a, ...rest} = src`). The excluded keys are `string_constants
    /// [exclude_start .. exclude_start+exclude_count]` (the sibling properties).
    ObjectRest {
        dst: Reg,
        src: Reg,
        exclude_start: u32,
        exclude_count: u16,
    },
    /// Like ObjectRest, but the excluded sibling keys are the `n` runtime values
    /// in registers `keys_base..keys_base+n` (each ToPropertyKey-coerced) — used
    /// when an object-rest pattern has a computed sibling key (`{[k]: a, ...r}`).
    ObjectRestDyn {
        dst: Reg,
        src: Reg,
        keys_base: Reg,
        n: u16,
    },
    /// Record the evaluated ClassElementName of decorated element `elem` into the
    /// class's `DecState::keys`, so `context.name` is the very String/Symbol the
    /// key expression produced. Emitted only for a COMPUTED decorated key (a
    /// static one is already in `DecElemDef::name`); the key is ToPropertyKey'd
    /// by the member/field op that consumes the same register, so this op only
    /// stores what it is given.
    DecKey {
        class: Reg,
        elem: u16,
        key: Reg,
        class_id: u32,
    },
    /// Apply decorated element `elem`'s decorators — the `argc` values in
    /// `[arg_base, arg_base+argc)`, in REVERSE list order (`@a @b m(){}` calls
    /// `b` first and passes its result to `a`) — and write the results back:
    /// a method/getter/setter's replacement onto the class, a field's or
    /// auto-accessor's returned initializer into `DecState::field_inits[elem]`.
    DecElem {
        class: Reg,
        elem: u16,
        arg_base: Reg,
        argc: u16,
        class_id: u32,
    },
    /// Apply the class's own decorators (the `argc` values in `[arg_base,
    /// arg_base+argc)`, reverse order) to the class in `class`, leaving the
    /// possibly-replaced value in `class`. Runs after every element is decorated
    /// and BEFORE the static field initializers, so `@dec class C { static x = 1 }`
    /// initializes `x` on the class the decorator returned.
    DecClass {
        class: Reg,
        arg_base: Reg,
        argc: u16,
    },
    /// Run one of the class's `addInitializer` lists with `this` set to the
    /// receiver. `which`: 0 = instance METHOD/getter/setter list (receiver =
    /// reg 0, the new object; emitted at the head of the constructor's / field
    /// thunk's element initialization), 1 = static method/getter/setter list
    /// (receiver = the class in `recv`, after class decoration and before the
    /// static field initializers), 2 = class (receiver = the class, the very last
    /// step of ClassDefinitionEvaluation), 3 = the PER-ELEMENT list of decorated
    /// element `elem` (a field or auto-accessor), run immediately after that one
    /// element has been defined — `elem` is ignored for 0/1/2.
    ///
    /// `class_id` names the class whose `DecState` holds the list; for the
    /// instance form it is resolved through `class_values` exactly as `FieldInit`
    /// resolves a computed field key.
    DecInits {
        class_id: u32,
        which: u8,
        elem: u16,
        recv: Reg,
    },
    /// Pipe a decorated field's initial value through the initializer chain the
    /// element's decorators returned: `val = init(val)` with `this` = `recv`, in
    /// chain order (innermost decorator first). A no-op when no decorator
    /// returned an initializer, which is why the undecorated field path never
    /// grows a branch.
    DecField {
        class_id: u32,
        elem: u16,
        val: Reg,
        recv: Reg,
    },
    /// `dst = <the class value for classes[class_id]>` — materialize a class.
    /// `parent` is the register holding the superclass value (`extends P`), or
    /// `None`; the new class links to it for inherited lookup + instanceof.
    MakeClass {
        dst: Reg,
        class_id: u32,
        parent: Option<Reg>,
    },
    /// Throw a ReferenceError if the value in `src` is a derived-class instance
    /// whose `this` is still in the constructor TDZ (its `super()` has not yet
    /// completed). Emitted before `this` reads and super-property references in
    /// a derived class constructor (and in arrows lexically inside one).
    ThisCheck {
        src: Reg,
    },
    /// `dst = yield val` — suspend the current generator, handing `val` out as
    /// the yielded value. On resume the value passed to `.next(v)` lands in `dst`.
    Yield {
        dst: Reg,
        val: Reg,
    },
    /// Suspension point for an ASYNC `yield*` delegation step: hands `val` out like
    /// `Yield`. On resume the driver delivers the resume MODE into `mode_dst`
    /// (0 = next, 2 = return) and the value into `val_dst`, so the yield* loop forwards
    /// next/return to the inner iterator. A `.throw()` instead unwinds to the loop's
    /// surrounding handler (it does not resume here). Distinct op so the async-gen
    /// driver recognises a delegating suspension.
    AsyncYieldDelegate {
        mode_dst: Reg,
        val_dst: Reg,
        val: Reg,
    },
    /// `RequireObject(val)` — throw a TypeError if `val` is not an Object. Used after
    /// awaiting an async iterator's `next()` result (the iterator-result must be an
    /// Object per the spec; a non-object result is a TypeError, NOT a thenable).
    RequireObject {
        val: Reg,
    },
    /// One async `yield*` THROW-delegation step: `dst = iter.throw(exc)`. Looks up the
    /// inner iterator's `throw`; if it is absent (undefined/null) or not callable,
    /// throws a TypeError (the spec's "iterator has no throw" case). Otherwise calls
    /// it with `exc` and writes the (to-be-awaited) result into `dst`.
    AsyncIterThrowStep {
        dst: Reg,
        iter: Reg,
        exc: Reg,
    },
    /// One async `yield*` NEXT-delegation step: `dst = next_fn.call(iter, sent)`. Like
    /// `ForAwaitNext` but uses the `next` method CACHED once at GetIterator time
    /// (`next_fn`) — the spec's IteratorRecord.[[NextMethod]] is not re-read each step —
    /// and forwards `sent` (the value passed to the OUTER generator's `.next(v)`) so a
    /// delegated iterator observes it (and `arguments.length === 1`). A built-in
    /// generator/positional source ignores `next_fn` (cursor via `idx`).
    AsyncIterNextStep {
        dst: Reg,
        iter: Reg,
        idx: Reg,
        sent: Reg,
        next_fn: Reg,
    },
    /// One async `yield*` RETURN-delegation step. Looks up the inner iterator's
    /// `return`: if absent (undefined/null), sets `has_dst` to false (the yield* loop
    /// then makes the OUTER generator return `ret`); otherwise calls it with `ret`,
    /// sets `has_dst` true and writes the (to-be-awaited) result into `dst`.
    AsyncIterReturnStep {
        dst: Reg,
        has_dst: Reg,
        iter: Reg,
        ret: Reg,
    },
    /// `yield*` suspension point: yield `val` like `Yield`, but on resume DELIVER
    /// the resume MODE (0 = next, 1 = throw, 2 = return) into `mode_dst` and the
    /// resume value into `val_dst` — instead of `Yield`'s in-body throw/return
    /// injection — so the `yield*` loop can forward it to the inner iterator's
    /// next/throw/return. Only emitted for a SYNC `yield*` (see `IterDelegate`).
    YieldDelegate {
        mode_dst: Reg,
        val_dst: Reg,
        val: Reg,
    },
    /// One step of `yield*` delegation: drive `iter` per the resume `mode` (0 next /
    /// 1 throw / 2 return) with argument `sent`, applying the spec's missing-method
    /// rules (throw with no `throw` → IteratorClose + TypeError; return with no
    /// `return` → the generator returns `sent`). Writes the result value to
    /// `value_dst` and two booleans: `done_dst` true ⇒ the `yield*` expression
    /// completes with `value_dst`; `ret_dst` true ⇒ the generator must RETURN
    /// `value_dst`. Both false ⇒ yield `value_dst` (via `YieldDelegate`).
    IterDelegate {
        value_dst: Reg,
        done_dst: Reg,
        ret_dst: Reg,
        iter: Reg,
        mode: Reg,
        sent: Reg,
    },
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
    Await {
        dst: Reg,
        val: Reg,
    },
    /// for-of step: advance over `iter` (array/string/Map/Set positionally with
    /// the cursor in `idx`, or a generator via `.next()` ignoring `idx`). Writes
    /// the next element to `value_dst` and a bool to `done_dst`. Throws if `iter`
    /// is not iterable.
    /// `next`: register holding the PROLOGUE-cached `next` method (from
    /// `IterPrime`) — a mid-loop redefinition of `iterator.next` is not
    /// observed (spec: the iterator record snapshots it). `Reg::MAX` = no
    /// cache (destructuring sites): a user-object iterator's `next` is
    /// re-fetched per step there.
    IterNext {
        value_dst: Reg,
        done_dst: Reg,
        iter: Reg,
        idx: Reg,
        next: Reg,
    },
    /// Cache a USER-OBJECT iterator's `next` method into `dst` for the loop's
    /// `IterNext` steps (one observable Get, per GetIterator). Built-in fast
    /// iterables (array/string/Map/Set/generator) skip the get — `dst` is
    /// undefined and the cursor fast path is unaffected.
    IterPrime {
        dst: Reg,
        iter: Reg,
    },
    /// IteratorClose: invoke `iter`'s `return()` (if present) — emitted on the
    /// abrupt `break` exit of a `for-of` so a not-yet-exhausted iterator is closed.
    IterClose {
        iter: Reg,
    },
    /// IteratorClose in an ERROR context: invoke `iter`'s `return()` (if present)
    /// but SWALLOW any error it throws and skip the result-is-object check, so a
    /// throw/return out of a `for-of` body closes the iterator while preserving the
    /// original abrupt completion (the spec's `Completion(...)` discard of the close result).
    IterCloseQuiet {
        iter: Reg,
    },
    /// The body of a `for-of`'s close handler, keyed on the completion record the
    /// finally machinery deposited in `kind_reg`: a THROW completion (2) closes
    /// quietly (the original error wins), a RETURN completion (1) closes for real
    /// so a throwing `return()` replaces the return value, and a normal (0) or
    /// break/continue (3) completion closes NOTHING — the loop's own break block
    /// already does that, and a `continue` must leave the iterator open. Running
    /// the close from a `finally` rather than inline before `Return` is what puts
    /// it AFTER the body's own handlers (7.4.11 runs it as the for-of statement's
    /// completion, outside any `try` in the body) and what makes `gen.return()` at
    /// a `yield` inside the loop close the loop's iterator at all.
    IterCloseFinally {
        iter: Reg,
        kind_reg: Reg,
    },
    /// Resolve `src`'s ASYNC iterator into `dst`: `src[@@asyncIterator]()` if
    /// present, else `src[@@iterator]()` (a sync iterable used by `for await`),
    /// else pass `src` through (async generators / built-ins iterate directly).
    GetAsyncIterator {
        dst: Reg,
        src: Reg,
        sync_dst: Reg,
    },
    /// `for await` step: writes the next RESULT to `dst` — a Promise (async
    /// iterator / async generator), or a `{value, done}` object (sync iterable,
    /// positional via the `idx` cursor). The loop then `await`s `dst`, so a sync
    /// `{value,done}` passes straight through and an async one suspends.
    ForAwaitNext {
        dst: Reg,
        iter: Reg,
        idx: Reg,
    },
    /// `dst = GetSuperConstructor()` for a `super(...)` call — fetched BEFORE the
    /// argument list is evaluated (spec SuperCall order: GetNewTarget,
    /// GetSuperConstructor, ArgumentListEvaluation, THEN IsConstructor check).
    /// No constructor-ness check here; SuperCtor/SuperCtorSpread do that after
    /// their args ran (call-proto-not-ctor, staging/sm/class/superCallOrder).
    SuperCtorFetch {
        dst: Reg,
        home_class_id: u32,
    },
    /// `super(args…)`: run the fetched superclass constructor (`ctor` — the
    /// register a SuperCtorFetch filled before the args were evaluated) on the
    /// current `this` (reg 0). IsConstructor is checked HERE, after the args.
    SuperCtor {
        ctor: Reg,
        home_class_id: u32,
        arg_base: Reg,
        argc: u16,
    },
    /// `super(...args_array)`: like SuperCtor but spreads the elements of the
    /// array in `args` (`super(...xs)` in a derived constructor).
    SuperCtorSpread {
        ctor: Reg,
        home_class_id: u32,
        args: Reg,
    },
    /// `dst = GetSuperBase()` — the home object's LIVE [[Prototype]] for a
    /// `super` property reference in a method of class `home_class_id`. Captured
    /// into a register at MakeSuperPropertyReference time (BEFORE the call's
    /// arguments / the assignment's RHS run — they may retarget the home
    /// object's prototype, staging/sm/class/superPropOrdering); the
    /// SuperMethod/SuperSet consumers read it from `base` instead of
    /// re-resolving. May be UNDEFINED/null — consumers RequireObjectCoercible.
    SuperBase {
        dst: Reg,
        home_class_id: u32,
    },
    /// `dst = super.<name>(args…)`: call the named method found from the
    /// captured super base (`base`, a SuperBase register) up its chain, with
    /// `this` = the current frame's `this` (reg 0).
    #[allow(dead_code)] // sealed legacy op: source calls now capture GetValue before arguments
    SuperMethod {
        dst: Reg,
        base: Reg,
        home_class_id: u32,
        name: u32,
        arg_base: Reg,
        argc: u16,
    },
    /// `dst = super.<name>`: read an inherited property (method value, or a getter
    /// invoked with `this` = the current frame's `this`) via the superclass's
    /// prototype.
    SuperGet {
        dst: Reg,
        home_class_id: u32,
        name: u32,
    },
    /// `dst = super[key]`: computed form of SuperGet (`key` is a register).
    SuperGetComputed {
        dst: Reg,
        home_class_id: u32,
        key: Reg,
    },
    /// Call/tag reference form of `super.<name>`. Unlike `SuperGet`, the
    /// compile-site receiver and static/instance home selection are explicit;
    /// inline static-field initializers do not share the enclosing frame's
    /// reg-0 `this` or its FuncProto-level `super_static` flag.
    SuperGetRef {
        dst: Reg,
        home_class_id: u32,
        name: u32,
        receiver: Reg,
        is_static: bool,
    },
    /// Computed call/tag-reference form of `SuperGetRef`.
    SuperGetRefComputed {
        dst: Reg,
        home_class_id: u32,
        key: Reg,
        receiver: Reg,
        is_static: bool,
    },
    /// `dst = super[key](args…)`: computed form of SuperMethod.
    #[allow(dead_code)]
    // retained only while its interpreter/codegen support is retired safely
    SuperMethodComputed {
        dst: Reg,
        base: Reg,
        home_class_id: u32,
        key: Reg,
        arg_base: Reg,
        argc: u16,
    },
    /// `super.<name> = val`: if the captured super base's (`base`) prototype
    /// chain has an inherited setter, invoke it with `this` = the current
    /// receiver; otherwise create an own property on the receiver.
    SuperSet {
        base: Reg,
        home_class_id: u32,
        name: u32,
        val: Reg,
    },
    /// `super[key] = val`: computed form of SuperSet (`key` is a register).
    SuperSetComputed {
        base: Reg,
        home_class_id: u32,
        key: Reg,
        val: Reg,
    },
    /// Set the `[[HomeObject]]` of an OBJECT-LITERAL method/accessor closure `method`
    /// to `home` (the object being built), so `super.x` inside it resolves via
    /// GetPrototypeOf(home). Emitted by the object-literal codegen for concise
    /// methods and get/set accessors.
    SetHomeObject {
        method: Reg,
        home: Reg,
    },
    /// `dst = super.name` inside an OBJECT method: resolve on GetPrototypeOf the
    /// executing closure's `[[HomeObject]]`, with `this` = the current receiver.
    SuperGetObj {
        dst: Reg,
        name: u32,
    },
    /// `dst = super[key]` inside an object method (computed form).
    SuperGetObjComputed {
        dst: Reg,
        key: Reg,
    },
    /// `super.name = val` inside an object method.
    SuperSetObj {
        name: u32,
        val: Reg,
    },
    /// `super[key] = val` inside an object method (computed form).
    SuperSetObjComputed {
        key: Reg,
        val: Reg,
    },
    /// `dst = super.name(args…)` inside an OBJECT method: resolve `name` on
    /// GetPrototypeOf the executing closure's [[HomeObject]], call it with
    /// `this` = the current receiver.
    #[allow(dead_code)] // sealed legacy op; `SuperGetObj` + `CallWithThis` replaced it
    SuperMethodObj {
        dst: Reg,
        name: u32,
        arg_base: Reg,
        argc: u16,
    },
    /// `dst = super[key](args…)` inside an object method (computed form).
    #[allow(dead_code)] // sealed legacy op; exact captured callable dispatch supersedes it
    SuperMethodObjComputed {
        dst: Reg,
        key: Reg,
        arg_base: Reg,
        argc: u16,
    },
    /// `dst = new callee(args…)` — construct an instance. `callee` must be a
    /// class value; builds an object, installs the methods, runs the ctor.
    New {
        dst: Reg,
        callee: Reg,
        arg_base: Reg,
        argc: u16,
    },
    /// Append a computed instance-field KEY (already evaluated into `key`) to the
    /// class value `class`'s `computed_field_keys`, at class-definition time and
    /// in source order. Read later by `FieldInit`.
    PushFieldKey {
        class: Reg,
        key: Reg,
    },
    /// `this[key] = val` for the `key_index`-th computed instance field, run in a
    /// constructor: looks up the key from `class_id`'s runtime class value's
    /// `computed_field_keys` (eval-once at class definition) and assigns —
    /// resolved by the EXECUTING class, not `this`'s class, because a parent
    /// constructor's return-override makes `this` a foreign object.
    /// `class_id == u32::MAX` falls back to `this`'s class. `this` is reg 0.
    FieldInit {
        key_index: u16,
        val: Reg,
        class_id: u32,
    },
    /// AsyncFromSyncIteratorContinuation(step): from a SYNC iterator's raw
    /// `{value, done}` result, build the spec's capability promise that
    /// resolves to `{ value: await value, done }`. Runs in ordinary dispatch
    /// context, synchronously inside the next() turn — the value's observable
    /// constructor read (PromiseResolve) happens BEFORE any job runs, and the
    /// unwrap costs exactly one reaction job (the spec's extra hop). `iter` is
    /// the SYNC iterator (the record's [[Iterator]]): with done == false it is
    /// closed when PromiseResolve aborts or the awaited value rejects
    /// (closeOnRejection — spec steps 7 and 13), before the capability rejects.
    AsyncFromSyncStep {
        dst: Reg,
        step: Reg,
        iter: Reg,
    },
    /// `dst = Array(args…)` / `new Array(args…)`: a single numeric arg makes an
    /// array of that length (holes → undefined); otherwise an array of the args.
    /// A syntactic user call/construct carries the exact `callee` captured before
    /// argument evaluation and takes this fast path only while it is the main
    /// realm's intrinsic Array constructor. `None` is reserved for internal array
    /// creation sites which are not observable calls.
    ArrayCtor {
        dst: Reg,
        callee: Option<Reg>,
        arg_base: Reg,
        argc: u16,
        is_construct: bool,
    },
    /// `dst = new Map(src?)` — build a Map from an optional iterable of [k,v]
    /// entries (`src` register, or `None` for an empty map).
    #[allow(dead_code)] // internal legacy op; syntactic `new Map` uses generic New
    NewMap {
        dst: Reg,
        src: Option<Reg>,
    },
    /// `dst = new Set(src?)` — build a Set from an optional iterable of values.
    #[allow(dead_code)] // internal legacy op; syntactic `new Set` uses generic New
    NewSet {
        dst: Reg,
        src: Option<Reg>,
    },
    /// `dst = new WeakMap(src?)` / `new WeakSet(src?)` — like NewMap/NewSet but a
    /// distinct WeakMap/WeakSet type, and keys/values must be objects.
    #[allow(dead_code)]
    NewWeakMap {
        dst: Reg,
        src: Option<Reg>,
    },
    #[allow(dead_code)]
    NewWeakSet {
        dst: Reg,
        src: Option<Reg>,
    },
    /// `dst = new WeakRef(target)` — target must be an object.
    #[allow(dead_code)]
    NewWeakRef {
        dst: Reg,
        target: Reg,
    },
    /// `dst = new String/Number/Boolean(arg?)` — a boxed primitive wrapper.
    /// `kind` 0=String/1=Number/2=Boolean; `arg` is the (optional) argument register.
    #[allow(dead_code)]
    NewBox {
        dst: Reg,
        kind: u8,
        arg: Option<Reg>,
    },
    /// `dst = new FinalizationRegistry(cleanupCallback)` — callback must be callable.
    #[allow(dead_code)]
    NewFinalizationRegistry {
        dst: Reg,
        cleanup: Reg,
    },
    /// `dst = new Promise(executor)` — alloc a pending promise, call `executor`
    /// with its (resolve, reject) functions; a throwing executor rejects it.
    #[allow(dead_code)]
    NewPromise {
        dst: Reg,
        executor: Reg,
    },
    /// `dst = callee(...args_array)` — call `callee` (a function value) spreading
    /// the elements of the array in `args` as the arguments (`this` = undefined).
    CallSpread {
        dst: Reg,
        callee: Reg,
        args: Reg,
    },
    /// Call the already-captured `callee` with the already-captured receiver,
    /// spreading `args`. Used for namespace statics so their property Get
    /// precedes spread-argument iteration.
    CallWithThisSpread {
        dst: Reg,
        callee: Reg,
        this_v: Reg,
        args: Reg,
    },
    /// `dst = obj[name](...args_array)` — method call spreading the elements of
    /// the array in `args` (`this` = obj). Handles builtin methods (e.g.
    /// `arr.push(...xs)`) and user methods alike.
    #[allow(dead_code)] // sealed legacy op: its property Get occurred after spread iteration
    CallMethodSpread {
        dst: Reg,
        obj: Reg,
        name: u32,
        args: Reg,
    },
    /// `dst = obj[key](...args_array)` — computed-member method call spreading the
    /// elements of `args` (`this` = obj). The computed-key analogue of
    /// `CallMethodSpread` (binds `this`, unlike `CallSpread` on the GET result).
    #[allow(dead_code)] // sealed legacy op: use captured GetIndex + CallWithThisSpread
    CallMethodComputedSpread {
        dst: Reg,
        obj: Reg,
        key: Reg,
        args: Reg,
    },
    /// `dst = super.name(...args_array)` — super method call spreading the elements
    /// of `args` (`this` = the current receiver). The spread analogue of SuperMethod.
    #[allow(dead_code)] // sealed legacy op: use SuperGetRef + CallWithThisSpread
    SuperMethodSpread {
        dst: Reg,
        home_class_id: u32,
        name: u32,
        args: Reg,
    },
    /// `super[key](...args_array)`: computed form of SuperMethodSpread.
    #[allow(dead_code)] // sealed legacy op: use SuperGetRefComputed + CallWithThisSpread
    SuperMethodComputedSpread {
        dst: Reg,
        home_class_id: u32,
        key: Reg,
        args: Reg,
    },
    /// `dst = new callee(...args_array)` — construct `callee` spreading the
    /// elements of the array in `args` as the arguments.
    NewSpread {
        dst: Reg,
        callee: Reg,
        args: Reg,
    },
    /// `dst = Math.<op>(args…)` — a builtin Math function over `argc` contiguous
    /// argument registers starting at `arg_base`.
    ///
    /// Two forms. CAPTURED: `callee`/`this_v` hold the `Math.<op>` callable and
    /// the `Math` receiver read BEFORE the arguments (`LoadGlobal Math; GetProp
    /// <op>`), the exact EvaluateCall order; the op runs the intrinsic only if
    /// both still name the main realm's pristine intrinsic, else it ordinary-
    /// calls the captured pair. BARE (`callee == NO_REG`, `this_v` = the
    /// `LoadGlobal` index of `Math`): nothing is captured because every
    /// argument is order-transparent (`arg_order_transparent`), so validating
    /// the LIVE global slot and the LIVE `Math.<op>` own data slot at execution
    /// (`math_bare_is_intrinsic`; the JIT's `emit_math_identity_guard`) is
    /// indistinguishable from the pre-argument Get; a miss performs that Get
    /// on the live global and ordinary-calls the result. The bare form is the
    /// pre-hardening register layout — no pair in the loop body — with the
    /// hardening's semantics (a replaced method, a rebound `Math`, an accessor
    /// or a deleted slot are all observed).
    MathOp {
        dst: Reg,
        op: MathFn,
        callee: Reg,
        this_v: Reg,
        arg_base: Reg,
        argc: u16,
    },
    /// `dst = <Number|parseInt|parseFloat>(args…)` — a builtin global function.
    /// `callee` is the exact value captured before argument evaluation. The
    /// specialised path is guarded by identity with the main-realm intrinsic;
    /// a miss ordinary-calls this value with the complete argument list.
    GlobalFn {
        dst: Reg,
        op: GlobalFn,
        callee: Reg,
        arg_base: Reg,
        argc: u16,
    },
    /// `dst = <static builtin>(args…)` — a guarded constructor-namespace static
    /// call over `argc` contiguous arg registers. `callee` and `this_v` are the
    /// live method value and namespace receiver captured before argument
    /// evaluation. The runtime takes the specialised path only when both still
    /// identify the current realm's intrinsic; otherwise it performs an ordinary
    /// call with these already-captured values. Keeping the reference in the
    /// bytecode is required for `Object.assign(sideEffect())`-style calls where
    /// an argument replaces the method after EvaluateCall has resolved it.
    StaticFn {
        dst: Reg,
        op: StaticFn,
        callee: Reg,
        this_v: Reg,
        arg_base: Reg,
        argc: u16,
    },
    /// `dst = Array.from(src[, mapfn])`. `mapfn` is a function value, or
    /// undefined-in-register when absent (the compiler loads undefined there).
    ArrayFrom {
        dst: Reg,
        src: Reg,
        mapfn: Reg,
        callee: Reg,
        this_v: Reg,
        argc: u16,
    },
    /// `dst = Math.<op>(...arr)` — a variadic Math reduction (max/min/hypot)
    /// applied to the elements of the array in `args`.
    MathSpread {
        dst: Reg,
        op: MathFn,
        callee: Reg,
        this_v: Reg,
        args: Reg,
    },
    /// `dst = (val instanceof ctor)` where `ctor` is a runtime class value: true
    /// when `val` is an instance whose class is `ctor`.
    InstanceOfDyn {
        dst: Reg,
        val: Reg,
        ctor: Reg,
    },
    /// `dst = (key in obj)` — true when `obj` has the property `key` (own or, for
    /// a class instance, inherited; array indices / `length`; Map/Set `size`).
    /// `brand` is set ONLY for the ergonomic private brand check `#x in obj` (whose
    /// key is the reserved `#x` string): it bypasses the private-key reflection
    /// filter that a regular `in` applies, so `#x in obj` sees the private element
    /// while `'#x' in obj` does not.
    HasProp {
        dst: Reg,
        key: Reg,
        obj: Reg,
        brand: bool,
    },
    /// `dst = bool` — a `with`-statement binding probe: true iff `obj` has the
    /// property `name` (own or inherited, [[HasProperty]]) AND it is not blocked
    /// by `obj[@@unscopables]` (an own/inherited unscopables object whose `name`
    /// entry is truthy hides the binding). Drives `with`-body identifier
    /// resolution: a true result routes the read/write to `obj`, false falls
    /// back to the next with-object or the lexical/global binding.
    WithHas {
        dst: Reg,
        obj: Reg,
        name: u32,
    },
    /// GetBindingValue for a `with` binding AFTER `WithHas` reported it: the
    /// read RE-checks [[HasProperty]] (the WithHas @@unscopables getter may
    /// have DELETED the property) — a miss yields undefined for a sloppy
    /// reference site and a ReferenceError for a strict one (8.1.1.2.6).
    WithGet {
        dst: Reg,
        obj: Reg,
        name: u32,
        strict: bool,
    },
    /// SetMutableBinding counterpart of `WithGet`: re-HasProperty, a strict
    /// reference site throws on a vanished binding, a sloppy one re-creates
    /// the property via the ordinary [[Set]].
    WithSet {
        obj: Reg,
        name: u32,
        val: Reg,
        strict: bool,
    },

    // ── control flow (targets are instruction indices) ──
    Jump {
        target: u32,
    },
    /// Jump if `cond` is falsy.
    JumpIfFalse {
        cond: Reg,
        target: u32,
    },
    /// Jump if `cond` is truthy.
    JumpIfTrue {
        cond: Reg,
        target: u32,
    },

    /// Fused compare-and-branch: `if !(a < b) goto target`. Keeps the common
    /// loop/recursion guard in one instruction so the boolean never has to be
    /// materialised into a register. Emitted for a bare `<`/`<=` branch test
    /// whose value nothing else consumes (`emit_test_jump`); only `<`/`<=` —
    /// the op carries no operand-swap flag, so a fused `>`/`>=` would reorder
    /// the two ToPrimitive coercions. `ZIPP_NO_FUSED_CMPJUMP=1` restores the
    /// unfused `Lt`/`Le` + `JumpIfFalse` pair.
    JumpIfNotLt {
        a: Reg,
        b: Reg,
        target: u32,
    },
    JumpIfNotLe {
        a: Reg,
        b: Reg,
        target: u32,
    },

    // ── reference types ──
    /// `dst = <function object for functions[func_id]>`. Capture-free: used for
    /// functions that reference no enclosing variables.
    MakeFunc {
        dst: Reg,
        func_id: u32,
    },
    /// `dst = <closure over functions[func_id]>` capturing upvalue cells named
    /// by `functions[func_id].upvalues`. Each upvalue source is resolved in the
    /// CURRENT (defining) frame: either a local register that holds a cell, or
    /// one of the current frame's own upvalues (for nested-of-nested capture).
    MakeClosure {
        dst: Reg,
        func_id: u32,
    },

    /// Like `MakeClosure`, but for an ARROW function: the closure also captures
    /// the defining frame's effective `this` from register `this_reg` (the
    /// `this_override.unwrap_or(0)` at the definition site) into the closure, so
    /// a later call binds it lexically (see `FuncProto::lexical_this`). Always
    /// used for arrows (even with no upvalues) since `MakeFunc` has no `this` slot.
    MakeArrow {
        dst: Reg,
        func_id: u32,
        this_reg: Reg,
    },

    /// Box the value currently in `reg` into a fresh heap Cell and write the
    /// cell reference back into `reg`. Emitted for a captured local/param so
    /// later reads/writes go through the shared cell.
    MakeCell {
        reg: Reg,
    },
    /// Like `MakeCell` but the fresh Cell holds the UNINITIALIZED (TDZ) sentinel
    /// regardless of `reg`'s value. Emitted at function entry for a captured
    /// lexical (`let`/`const`/`class`) that a forward-referenced function may
    /// capture: a `CellGet`/`UpvalGet` before the binding's textual declaration
    /// runs (its TDZ) then throws a ReferenceError instead of reading undefined.
    MakeCellTdz {
        reg: Reg,
    },
    /// Like `MakeCell` but the cell is tagged IMMUTABLE — a named function
    /// expression's own-name binding. A nested closure's / eval's write
    /// through it (UpvalSet / StoreUpvalDyn) is a silent no-op in sloppy code
    /// and a TypeError in strict code.
    MakeCellFnName {
        reg: Reg,
    },
    /// Tag the cell already in `reg` IMMUTABLE — a `const`/`using` binding that a
    /// nested closure captures. Emitted after the declaration's initializing
    /// store (a per-iteration loop binding re-tags its fresh cell each turn),
    /// because the declaring function's own writes are rejected at compile time
    /// and only writes THROUGH the closure (UpvalSet / StoreUpvalDyn) need the
    /// runtime check. Unlike `MakeCellFnName`, the write always throws a
    /// TypeError — sloppy code does not get a silent no-op for `const`.
    MarkCellConst {
        reg: Reg,
    },
    /// `dst = *<cell in reg>` — read a captured local's cell.
    CellGet {
        dst: Reg,
        cell: Reg,
    },
    /// `*<cell in reg> = src` — write a captured local's cell.
    CellSet {
        cell: Reg,
        src: Reg,
    },
    /// Like `CellSet` but the target is a lexical (`let`/`const`) cell that may
    /// still be in its TDZ: writing while the cell holds UNINITIALIZED throws a
    /// ReferenceError (an ASSIGNMENT before the declaration runs). The
    /// declaration's own initializing store uses plain `CellSet`.
    CellSetChecked {
        cell: Reg,
        src: Reg,
    },
    /// `dst = *<upvalue[idx]>` — read one of this closure's captured cells.
    UpvalGet {
        dst: Reg,
        idx: u16,
    },
    /// `*<upvalue[idx]> = src` — write one of this closure's captured cells.
    UpvalSet {
        idx: u16,
        src: Reg,
    },
    /// EvalScope-first upvalue access for a sloppy contains-direct-eval
    /// function: the eval may have introduced a function-scoped `var`
    /// SHADOWING the captured name — that binding (looked up via the `name`
    /// global-slot handle) wins over the captured cell.
    LoadUpvalDyn {
        dst: Reg,
        idx: u16,
        name: u32,
    },
    StoreUpvalDyn {
        idx: u16,
        src: Reg,
        name: u32,
    },
    /// `dst = [reg[arg_base], …, reg[arg_base+argc-1]]` — array literal.
    NewArray {
        dst: Reg,
        arg_base: Reg,
        argc: u16,
    },
    /// `dst = {}` — empty object (populated by following SetProp/SetIndex).
    /// `{}` — a fresh ordinary object. `hint` is the number of static data
    /// properties the literal will append, so the property vectors can be sized
    /// once instead of regrowing (0 when unknown). Purely an allocation hint; it
    /// changes nothing observable.
    NewObject {
        dst: Reg,
        hint: u16,
    },
    /// A fresh ordinary object whose following `AppendDataProp` sequence has a
    /// compiler-proved, immutable key list. `plan` indexes
    /// `FuncProto::static_key_plans`; the runtime initially exposes an empty
    /// prefix and advances it once per matching append. This is only a storage
    /// optimisation: malformed/mismatching sequences materialise ordinary
    /// owned keys before continuing.
    NewPlannedObject {
        dst: Reg,
        plan: u16,
    },
    /// `dst = ToObject(src)` — `Object(x)` / `new Object(x)`: primitives box
    /// (string/number/boolean/symbol/bigint wrappers), null/undefined → a fresh
    /// object, and an existing object is returned unchanged.
    ToObject {
        dst: Reg,
        src: Reg,
    },
    /// RequireObjectCoercible(src): throw a TypeError if `src` is null/undefined,
    /// otherwise a no-op. Emitted for an EMPTY object destructuring pattern
    /// (`var {} = x`) — a non-empty pattern already throws via member access.
    CheckCoercible {
        src: Reg,
    },
    /// `dst = new <Error subtype>(arg?, opts?)` — a proto-linked error instance.
    /// `kind` indexes the canonical error list (0=Error, 1=TypeError, …,
    /// 7=AggregateError); `arg` (when present) is coerced to the `message` string.
    /// `opts` (when present) is the options object — if it has a `cause`, a
    /// non-enumerable own `cause` is installed (ES2022 InstallErrorCause).
    /// `errors` (AggregateError only) is the first argument — IterableToList'd into
    /// a non-enumerable own `errors` array AFTER the message is coerced.
    NewError {
        dst: Reg,
        kind: u8,
        arg: Option<Reg>,
        opts: Option<Reg>,
        errors: Option<Reg>,
    },
    /// `dst = Symbol(desc?)` — a fresh unique Symbol primitive. `desc` (when present)
    /// is coerced to a string description (undefined → no description).
    #[allow(dead_code)] // internal legacy op; syntactic Symbol uses generic Call
    MakeSymbol {
        dst: Reg,
        desc: Option<Reg>,
    },
    /// `dst = <BigInt literal>` (`123n`) — allocate a BigInt with the given value.
    LoadBigInt {
        dst: Reg,
        value: i128,
    },
    /// `dst = <BigInt literal beyond i128>` — allocate an arbitrary-precision
    /// BigInt from the function's `bigint_consts[idx]` (parsed at compile time).
    LoadBigIntBig {
        dst: Reg,
        idx: u32,
    },
    /// `dst = BigInt(arg)` — convert a number/string/boolean/BigInt to a BigInt
    /// (non-integer number → RangeError; symbol/null/undefined → TypeError).
    #[allow(dead_code)] // internal legacy op; syntactic BigInt uses generic Call
    BigIntFrom {
        dst: Reg,
        arg: Reg,
    },
    /// `dst = new RegExp(pattern, flags)` — compile a regex (`/pat/flags` literal
    /// and the constructor both lower here). `pattern`/`flags` are string regs;
    /// a bad pattern throws SyntaxError. `is_construct` is false ONLY for a
    /// `RegExp(...)` call WITHOUT `new`: then a RegExp pattern with no flags whose
    /// `constructor` is RegExp is returned unchanged (RegExp ctor step 2.b).
    NewRegExp {
        dst: Reg,
        pattern: Reg,
        flags: Reg,
        is_construct: bool,
    },
    /// `dst = <array of obj's own enumerable string keys>` — Object.keys backing.
    /// For an array, the keys are the index strings "0".."len-1".
    ObjectKeys {
        dst: Reg,
        obj: Reg,
        callee: Reg,
        this_v: Reg,
    },
    /// `dst = <for-in key list>` — own + INHERITED enumerable string keys, walking
    /// the [[Prototype]] chain with shadowing dedup (vs `ObjectKeys`, own-only).
    /// null/undefined receiver → empty (for-in over nullish does not throw).
    ForInKeys {
        dst: Reg,
        obj: Reg,
    },
    /// `dst = <is the snapshotted for-in key still present?>` — `obj` is the
    /// engine-private `ForInKeys` Array whose prefix carries the receiver and
    /// optional version guard. A key deleted (or otherwise removed) after the
    /// snapshot but before its visit is skipped. Uses the NON-observable
    /// own+prototype presence check; a Proxy receiver stays snapshot-only (its
    /// `has` trap must NOT fire — the spec'd protocol is ownKeys + per-key gopd,
    /// already applied by the snapshot), as does a primitive.
    ForInLive {
        dst: Reg,
        obj: Reg,
        key: Reg,
    },
    /// `dst = Object.values(obj)` — array of the object's own values (or array
    /// elements).
    ObjectValues {
        dst: Reg,
        obj: Reg,
        callee: Reg,
        this_v: Reg,
    },
    /// `dst = Object.entries(obj)` — array of `[key, value]` pair arrays.
    ObjectEntries {
        dst: Reg,
        obj: Reg,
        callee: Reg,
        this_v: Reg,
    },
    /// `dst = <length of array/string in obj>` (0 for anything else). Used by
    /// the `for-of` desugaring's bound check.
    LenOf {
        dst: Reg,
        obj: Reg,
    },
    /// `dst = obj[key]` — computed member read (array element or object prop).
    GetIndex {
        dst: Reg,
        obj: Reg,
        key: Reg,
    },
    /// `obj[key] = val` — computed member write.
    SetIndex {
        obj: Reg,
        key: Reg,
        val: Reg,
    },
    /// `dst = obj[<string-const `name`> + key]` — fused computed read for the
    /// `obj["prefix" + i]` map-key idiom. `name` indexes `string_constants` (the
    /// literal prefix). When `key` is an int and `obj` is a plain object, the key
    /// is assembled into a reusable scratch buffer and looked up by `&str` — NO
    /// throwaway heap string is allocated for the concat (the dominant cost of
    /// dictionary-churn loops). Any other operand shape falls back to a real
    /// `prefix + key` concat + `GetIndex` (semantically identical).
    GetIndexConcat {
        dst: Reg,
        obj: Reg,
        name: u32,
        key: Reg,
    },
    /// `obj[<string-const `name`> + key] = val` — the `SetIndex` twin of
    /// `GetIndexConcat` (same no-alloc fast path; falls back to concat + `SetIndex`).
    SetIndexConcat {
        obj: Reg,
        name: u32,
        key: Reg,
        val: Reg,
    },
    /// `dst = the key half of `"name" + src`, coerced to a PRIMITIVE` — the
    /// evaluation-order shim that makes the fused WRITE (`SetIndexConcat` from
    /// a plain `obj["name" + e] = v`) sound. The `+`'s observable coercion
    /// (`ToPrimitive(src, default)`, and the Symbol TypeError of the ensuing
    /// ToString) must run where the `+` sits — BEFORE the RHS evaluates — while
    /// the concatenation itself is deferred to the store. Numbers, strings and
    /// the other primitives pass through UNCHANGED (their concat runs no user
    /// code, so deferring it is unobservable, and an Int key keeps
    /// `set_index_concat`'s allocation-free scratch path); a non-string heap
    /// value runs the real protocol here.
    ToConcatKey {
        dst: Reg,
        src: Reg,
    },
    /// `dst = delete obj[<string-const `name`> + key]` — the `DeleteIndex` twin of
    /// `GetIndexConcat`: an int key on a plain object deletes by `&str` (no concat
    /// alloc — these allocs dominate dictionary-churn delete loops via GC
    /// pressure); other shapes fall back to concat + `DeleteIndex`.
    DeleteIndexConcat {
        dst: Reg,
        obj: Reg,
        name: u32,
        key: Reg,
        strict: bool,
    },
    /// `dst = import(spec)` — dynamic import. ToString the specifier and return a
    /// Promise. With no host module loader, a successfully-coerced specifier rejects
    /// with a TypeError; if the specifier's coercion throws, the Promise rejects
    /// with that thrown value (import() never throws synchronously). options/phase
    /// (import attributes) — `opts` is the evaluated 2nd argument (a non-object,
    /// non-undefined options rejects with a TypeError). `phase`: 0 = normal
    /// `import(x)`, 1 = `import.defer(x)`, 2 = `import.source(x)` (source phase is
    /// not available for a SourceTextModule → rejects with a SyntaxError).
    ImportCall {
        dst: Reg,
        spec: Reg,
        phase: u8,
        opts: Option<Reg>,
    },
    /// Define a STATIC class field with a computed key: ToPropertyKey(key) once,
    /// throw a TypeError if it is "prototype" (a static field may not be named
    /// `prototype` — it is a non-configurable own property of the constructor),
    /// then write the field on the class. Unlike `SetIndex`, the prototype check
    /// is unconditional (class bodies are strict regardless of the enclosing code).
    ClassStaticField {
        class: Reg,
        key: Reg,
        val: Reg,
    },
    /// `dst = ToPropertyKey(src)` for a read-modify-write of `obj[src]` (`o[k] += v`,
    /// `o[k]++`): coerce the computed key to a property key ONCE (invoking its
    /// `toString`/`valueOf`/@@toPrimitive) so the load and the store reuse it. `obj`
    /// is RequireObjectCoercible-checked FIRST — a null/undefined base throws a
    /// TypeError BEFORE the key's coercion runs (matching `obj[k]`'s evaluation order).
    ToPropKey {
        dst: Reg,
        obj: Reg,
        src: Reg,
    },
    /// Define an accessor property in an object literal: `{ get key(){…} }` or
    /// `{ set key(v){…} }`. `func` is the getter/setter function; `is_setter`
    /// picks the half. Merges with an existing accessor for the same key (so a
    /// get + set pair on one key becomes a single get/set accessor).
    DefineAccessor {
        obj: Reg,
        key: Reg,
        func: Reg,
        is_setter: bool,
    },
    /// SetFunctionName(func, key, prefix) for an object-literal accessor / computed
    /// member whose name is only known at runtime: name = prefix + key-as-name
    /// (a Symbol key → "[description]" or "", else ToString(key)); `prefix` is
    /// 0=none, 1="get ", 2="set ". Written as a non-writable/non-enumerable/
    /// configurable own `name` (overriding the synthesized intrinsic).
    SetFnNameFromKey {
        func: Reg,
        key: Reg,
        prefix: u8,
    },
    /// `dst = obj.<string_constants[name]>` — static property read
    /// (also resolves `.length` for arrays/strings).
    GetProp {
        dst: Reg,
        obj: Reg,
        name: u32,
    },
    /// `obj.<string_constants[name]> = val` — static property write.
    /// `strict` forces PutValue strictness even when the enclosing FUNCTION is
    /// sloppy: the ClassTail regions (heritage, computed keys, static field
    /// initializers) are strict code compiled INLINE into the enclosing
    /// function, whose proto-level `is_strict` flag cannot see it. Strict
    /// protos keep `strict: false` here (the runtime ORs the func flag in).
    SetProp {
        obj: Reg,
        name: u32,
        val: Reg,
        strict: bool,
    },
    /// `obj.#name = val` — a PRIVATE field/element write (PrivateSet). Unlike
    /// SetProp it brand-checks first: if the private element is NOT present on
    /// `obj` (PrivateFieldFind/PrivateElementFind empty), throw a TypeError. Used
    /// for user `this.#x = v` (incl. as a destructuring target), NOT for the
    /// field-initializer add (that is AddPrivateField).
    SetPrivate {
        obj: Reg,
        name: u32,
        val: Reg,
    },
    /// Define an own writable/enumerable/configurable data property directly on a
    /// fresh object — CreateDataProperty, NOT [[Set]]: used for object-literal data
    /// properties, which must ignore any inherited accessor / non-writable property.
    InitDataProp {
        obj: Reg,
        name: u32,
        val: Reg,
    },
    /// `{ __proto__: v }` — the object-literal proto SPECIAL FORM (B.3.1): a direct
    /// `object.[[SetPrototypeOf]](v)` for an Object/null `v`, a no-op otherwise.
    /// Deliberately NOT a [[Set]] of the key: the literal form is "not influenced
    /// by Object.prototype" — it still works after `delete
    /// Object.prototype.__proto__` and ignores a replacement `set __proto__`
    /// accessor (staging/sm/extensions/mutable-proto-special-form.js). The target
    /// is the freshly built literal, so the change can never be rejected.
    SetLiteralProto {
        obj: Reg,
        val: Reg,
    },
    /// `InitDataProp` for a key the COMPILER has proven is new: the Nth distinct
    /// static key of a literal with no spread, computed key, accessor or
    /// `__proto__:` before it. Appends without the existence probe that
    /// `ObjMap::define` performs, which is what made building a literal O(n^2)
    /// in its key count. No version bump is needed (nor done by `InitDataProp`):
    /// the object was created by this literal's earlier `NewObject` or
    /// `NewPlannedObject`, so no inline cache can hold a slot for it yet.
    AppendDataProp {
        obj: Reg,
        name: u32,
        val: Reg,
    },
    /// Allocate AND fully populate an all-static object literal in one step:
    /// `dst = { plan.keys[0]: reg[val_base], …, plan.keys[count-1]:
    /// reg[val_base+count-1] }`. The compiler stages every field value into the
    /// contiguous block first (the `NewArray` discipline), so the values are
    /// GC-rooted registers and no partially-initialized object can ever be
    /// observed — the object exists only after every field is written. `count`
    /// is redundant with `plan.len()` and revalidated at runtime so the operand
    /// table can enumerate the block without proto access; a mismatch is a
    /// fail-closed InternalError. Metering charges `1 + count` steps — the
    /// exact historical `NewPlannedObject` + count×`AppendDataProp` cost.
    FinalizeObject {
        dst: Reg,
        plan: u16,
        val_base: Reg,
        count: u16,
    },
    /// CreateDataProperty with a COMPUTED key (already ToPropertyKey'd via
    /// `ToPropKey`) on an object literal: an ordinary own data property even
    /// for "__proto__" — only the textual `__proto__:` colon form sets the
    /// prototype.
    InitDataPropDyn {
        obj: Reg,
        key: Reg,
        val: Reg,
    },
    /// `dst = delete obj.<string_constants[name]>` — remove an own property;
    /// `dst` is the boolean result (true unless the property is non-deletable).
    /// In strict mode a false result throws a TypeError instead.
    DeleteProp {
        dst: Reg,
        obj: Reg,
        name: u32,
        strict: bool,
    },
    /// `dst = delete obj[key]` — computed property delete (strict: throw on false).
    DeleteIndex {
        dst: Reg,
        obj: Reg,
        key: Reg,
        strict: bool,
    },
    /// `dst = delete <global identifier>` (sloppy only). A DECLARED global —
    /// the program's `hoisted_globals`/`decl_globals`/`lexical_globals` slot
    /// lists — is non-configurable: yields `false`, binding untouched.
    /// Anything else (an implicitly-created `x = 1` global, a builtin, an
    /// eval-introduced var) is removed — the slot returns to the
    /// `UNINITIALIZED` never-declared sentinel — and yields `true`.
    DeleteGlobal {
        dst: Reg,
        slot: u32,
    },

    /// Proper-tail-call prefix (strict `return f(args)` in an unprotected
    /// context): when the callee is a PLAIN function/closure, REUSE the
    /// current frame (constant stack for tail recursion) and never fall
    /// through; any other callee falls through to the ordinary Call+Return
    /// the compiler emits right after.
    TailCall {
        callee: Reg,
        arg_base: Reg,
        argc: u16,
    },
    /// `TailCall` with an explicit `this` (register `this_v`): the tail-position
    /// form of a `with`-resolved identifier call (this = the with-object).
    /// Falls through to the `CallWithThis` emitted right after for non-plain
    /// callees.
    TailCallWithThis {
        callee: Reg,
        this_v: Reg,
        arg_base: Reg,
        argc: u16,
    },
    /// Call `callee` with `argc` arguments staged in registers
    /// `[arg_base, arg_base+argc)`. Result lands in `dst`.
    Call {
        dst: Reg,
        callee: Reg,
        arg_base: Reg,
        argc: u16,
    },
    /// Like `Call` but with an explicit `this` value (register `this_v`):
    /// a `with`-resolved identifier call binds `this` to the with-object
    /// (spec WithBaseObject) — the callee value was already fetched by the
    /// `WithGet` protocol, so no further property read happens here.
    CallWithThis {
        dst: Reg,
        callee: Reg,
        this_v: Reg,
        arg_base: Reg,
        argc: u16,
    },
    /// Captured-reference call for the four RegExp-heavy method spellings.
    /// `callee` and `this_v` were resolved before the argument list exactly as
    /// for `CallWithThis`. The VM may enter the direct intrinsic lane only when
    /// `callee` is the exact setup-time main-realm function for `op`; every
    /// miss ordinary-calls that captured pair with the complete argument list.
    RegExpMethod {
        dst: Reg,
        op: RegExpMethod,
        callee: Reg,
        this_v: Reg,
        arg_base: Reg,
        argc: u16,
    },

    /// `dst = eval(arg)` — a DIRECT eval call (the callee is the unshadowed global
    /// `eval` identifier) emitted only from STRICT-mode code, so the evaluated
    /// string inherits strict mode (early errors fire). `new_target_ok` carries the
    /// caller's new.target validity (true inside a function/method/field initializer)
    /// so the eval may contain `new.target`. Indirect/sloppy eval still goes through
    /// the ordinary `Call` → `GLOBAL_EVAL` native path.
    /// `home_class`/`super_static`: the CALLER's compile-time class home (so
    /// `super.x` inside the eval'd code resolves against the same class) —
    /// u32::MAX when the eval site has no class context.
    /// `import.meta` (module code): the per-program host meta object,
    /// lazily allocated by the VM (ordinary, extensible, null prototype).
    ImportMeta {
        dst: Reg,
    },

    /// A call whose syntactic callee is the IdentifierReference `eval`.
    /// `callee` and `this_v` are the exact reference captured before argument
    /// evaluation; the runtime applies direct-eval semantics iff `callee` is
    /// this realm's %eval%.  Otherwise it performs an ordinary call with the
    /// complete argument list and captured receiver (`undefined`, except for a
    /// `with` object environment's WithBaseObject).
    ///
    /// Normally the arguments occupy `[arg_base, arg_base + argc)`.  With
    /// `args_array`, `arg_base` instead names the materialized spread-argument
    /// array and `argc` is zero; this preserves a dynamically-sized complete
    /// argument list without re-reading the callee after spread iteration.
    ///
    /// `tail`: the call sits in a proper-tail-call RETURN position — when the
    /// captured callee is not %eval% (an ordinary call), the frame is reused
    /// like `TailCall`.
    /// `derived_ctor`: the call site is inside a DERIVED-class constructor, so
    /// `super(...)` in the eval'd code is legal (PerformEval inherits the
    /// caller's this-binding status along with its lexical environment).
    /// `class_name_ok`: nothing at this site shadows `home_class`'s inner NAME
    /// binding, so the eval'd code sees it too. Decided by the compiler because
    /// only it knows the caller's scope chain — a method-body `let C` shadows
    /// the class name, an enclosing function's `C` does not.
    DirectEval {
        dst: Reg,
        callee: Reg,
        this_v: Reg,
        arg_base: Reg,
        argc: u16,
        args_array: bool,
        new_target_ok: bool,
        this_reg: Reg,
        home_class: u32,
        super_static: bool,
        derived_ctor: bool,
        class_name_ok: bool,
        ban_arguments: bool,
        strict_caller: bool,
        super_home_obj: bool,
        var_env_is_global: bool,
        site: u16,
        tail: bool,
    },

    /// ResolveBinding for a strict `name = …` on a global the program does not
    /// declare, probed BEFORE the RHS evaluates (the compiler emits this ahead
    /// of the RHS and stores with StoreGlobalResolved after it). Does NOT
    /// throw: an unresolvable probe is recorded in
    /// `strict_unresolvable_globals` and the store raises the ReferenceError —
    /// PutValue runs after the RHS, so an RHS exception must win
    /// (`seen = act()` where act() throws propagates the act() throw —
    /// staging/sm/Proxy/getPrototypeOf). Resolvable means: the slot is live, a
    /// same-named own property of the global object backs it (an eval-created
    /// var), it is a builtin, or the global object's PROTO CHAIN has it. A
    /// property the RHS itself creates must NOT resolve the reference
    /// (`undeclared = (this.undeclared = 5)` still throws).
    CheckGlobalResolvable {
        idx: u32,
    },

    /// CreateDataPropertyOrThrow for a class FIELD initializer: an own
    /// {writable, enumerable, configurable} data property on the receiver —
    /// never consults prototype setters; a Proxy receiver's defineProperty
    /// trap fires.
    DefineField {
        obj: Reg,
        name: u32,
        val: Reg,
    },

    /// `dst = obj.<string_constants[name]>(args…)` — method call with `this`
    /// bound to `obj`. Arguments occupy `[arg_base, arg_base+argc)`.
    ///
    /// The property Get happens HERE, after the arguments were evaluated, so
    /// the compiler emits it only when every argument is order-transparent
    /// (`FnCompiler::arg_order_transparent`: cannot run user code, throw, or
    /// write a binding) — then no observer can tell this from EvaluateCall's
    /// Get-before-arguments order. Any other argument shape lowers to
    /// `GetProp` + `CallWithThis`, which performs the Get first.
    CallMethod {
        dst: Reg,
        obj: Reg,
        name: u32,
        arg_base: Reg,
        argc: u16,
    },
    /// `dst = obj[key](args…)` — computed method call: resolve the method by the
    /// runtime `key`, then call it with `this` bound to `obj`.
    #[allow(dead_code)] // sealed legacy op: use captured GetIndex + CallWithThis
    CallMethodComputed {
        dst: Reg,
        obj: Reg,
        key: Reg,
        arg_base: Reg,
        argc: u16,
    },

    /// Throw the value in `src`. Unwinds to the nearest enclosing catch handler
    /// (in this or a caller frame), or aborts the program if none.
    Throw {
        src: Reg,
    },
    /// Push a try-handler: on a throw before the matching `PopHandler`, control
    /// jumps to `catch_target` with the thrown value placed in `catch_reg`.
    PushHandler {
        catch_target: u32,
        catch_reg: Reg,
    },
    /// Pop the most recent try-handler (reached when the try block completes
    /// without throwing).
    PopHandler,
    /// Push a `finally` handler. It is visited on EVERY exit from the protected
    /// region — throw (via unwind), `return` (via the Return op), or normal
    /// completion — running the finally block at `target` with a completion record
    /// deposited into `kind_reg` (0 normal, 1 return, 2 throw) and `val_reg` (the
    /// return value / thrown reason).
    PushFinally {
        target: u32,
        kind_reg: Reg,
        val_reg: Reg,
    },
    /// Pop the most recent `finally` handler (the normal-completion path, just
    /// before falling into the finally block).
    PopFinally,
    /// End of a `finally` block: resume the completion in `kind_reg`/`val_reg` —
    /// re-leave a pending `return` (chaining through any outer finally), re-raise a
    /// pending throw, resume a pending `break`/`continue` jump (chaining through any
    /// intervening finally), or fall through on normal completion.
    EndFinally {
        kind_reg: Reg,
        val_reg: Reg,
    },

    // ── Explicit Resource Management (`using` declarations) ──
    /// Open a fresh per-block sync-disposal scope: allocate an empty disposer list
    /// in `using_resources` and store its id (as an int Value) in `dst`. The id
    /// lives in a register so it is saved/restored with the frame (generator-safe).
    OpenUsingScope {
        dst: Reg,
    },
    /// Register a `using` resource (CreateDisposableResource, sync hint): `val` is
    /// the already-bound initializer value; `scope` holds the enclosing scope id.
    /// null/undefined → add nothing; a non-object → TypeError; an object whose
    /// @@dispose is absent/null/non-callable → TypeError; otherwise push a
    /// disposer (the @@dispose method bound to the value) onto the scope's list.
    RegisterDisposable {
        scope: Reg,
        val: Reg,
    },
    /// Finally-body op: take the `scope` id's disposer list, run it LIFO building a
    /// SuppressedError chain merged with the incoming completion in `kind_reg`/
    /// `val_reg` (kind&3==2 ⇒ already a throw); rewrite kind_reg/val_reg so the
    /// following `EndFinally` re-raises the merged completion.
    DisposeScope {
        scope: Reg,
        kind_reg: Reg,
        val_reg: Reg,
    },
    /// Register an `await using` resource (CreateDisposableResource, ASYNC hint):
    /// like `RegisterDisposable` but the dispose method is `@@asyncDispose` (read
    /// FIRST, read once), falling back to `@@dispose` only when `@@asyncDispose` is
    /// nullish; both nullish/non-callable on a non-null object → TypeError. A
    /// null/undefined value still pushes an INERT record (a plain `undefined`) — so
    /// the disposal still performs one `Await` (the spec's async asymmetry), unlike
    /// sync where nullish adds nothing.
    RegisterAsyncDisposable {
        scope: Reg,
        val: Reg,
    },
    /// Async-disposal step: pop the LAST (LIFO) entry of `scope`'s disposer list. If
    /// the list is empty, set `done` true. An inert `undefined` entry → `res` =
    /// undefined (nothing called). A real disposer (a bound `@@asyncDispose`/
    /// `@@dispose`) is CALLED here (may throw synchronously) and its result lands in
    /// `res` for the caller to `Await`. The compiler emits this under a handler that
    /// spans the following `Await`, so a sync throw or an awaited rejection both
    /// route to the merge step.
    AsyncDisposeNext {
        scope: Reg,
        res: Reg,
        done: Reg,
    },
    /// Merge a disposer error `err` into the completion in `kind_reg`/`val_reg`
    /// (DisposeResources error chaining): if already a throw (kind&3==2), wrap as
    /// `SuppressedError{error: err, suppressed: val}`; else set the completion to a
    /// throw of `err`. Used by the async-disposal loop's catch arm (the sync path
    /// does this inside `DisposeScope`'s native helper).
    MergeDispose {
        kind_reg: Reg,
        val_reg: Reg,
        err: Reg,
    },
    /// A `break`/`continue` that exits one or more `try` blocks: route through every
    /// intervening `finally` (running each, popping any intervening `catch`) before
    /// landing at `target`. `floor` is the handler-stack depth at the target
    /// loop/switch — routing pops handlers until the stack is back to `floor`. Only
    /// emitted when there ARE handlers to unwind; a plain `break`/`continue` uses
    /// `Jump` (so JIT-eligible loops stay eligible).
    JumpFinally {
        target: u32,
        floor: u16,
    },

    /// Finalize a tagged-template object: define `raw` as a frozen `.raw` own
    /// property of the cooked array `arr` and freeze both arrays.
    SetRaw {
        arr: Reg,
        raw: Reg,
    },
    /// Load the cached tagged-template object for this call SITE (keyed by the
    /// current function id + `site`), or `undefined` on the first evaluation —
    /// GetTemplateObject memoizes one canonical frozen object per source location.
    TemplateGetCached {
        dst: Reg,
        site: u32,
    },
    /// Memoize the freshly-built tagged-template object `src` for this call site.
    TemplateSetCached {
        site: u32,
        src: Reg,
    },
    /// Install a method on a class value at runtime under a COMPUTED key
    /// (`class C { [expr]() {} }`). `class` holds the class value, `key` the
    /// evaluated key, `func` the method's function id, `kind` selects 0=method /
    /// 1=getter / 2=setter / 3=static method / 4=static getter / 5=static
    /// setter, optionally OR'd with [`KEY_WRITEBACK`].
    ClassAddMember {
        class: Reg,
        key: Reg,
        func: u32,
        kind: u8,
    },
    /// `new Date(...)` → a Date. 0 args = now; 1 number = epoch ms; 1 string =
    /// parsed; ≥2 = (year, month0, day, h, m, s, ms) interpreted as UTC.
    #[allow(dead_code)] // internal legacy op; syntactic `new Date` uses generic New
    DateNew {
        dst: Reg,
        arg_base: Reg,
        argc: u16,
    },
    /// `Date.UTC(year, month0, …)` → epoch ms (a number, not a Date).
    #[allow(dead_code)]
    // retained for internal bytecode compatibility; calls are reference-captured
    DateUTC {
        dst: Reg,
        arg_base: Reg,
        argc: u16,
    },
    /// `Date.parse(str)` → epoch ms (NaN if unparseable).
    #[allow(dead_code)]
    // retained for internal bytecode compatibility; calls are reference-captured
    DateParse {
        dst: Reg,
        src: Reg,
    },
    /// Resolve the iterator of `src` for a `for-of`: if `src` has a `@@iterator`
    /// method (a custom iterable) call it (this = src) → the iterator object; else
    /// pass `src` through (arrays/strings/Map/Set/generators iterate directly).
    GetIterator {
        dst: Reg,
        src: Reg,
    },
    /// Like `GetIterator` but ALWAYS returns a real iterator OBJECT (invokes
    /// `src[@@iterator]()`, never the raw positional fast-path) — for `yield*`
    /// delegation, which calls `.next`/`.throw`/`.return` on the iterator.
    GetIteratorObj {
        dst: Reg,
        src: Reg,
    },
    /// Normalize `src` for array destructuring (`let [a,b] = src`): a generator or
    /// custom iterable is drained (LAZILY, ≤ `count` elements — `u32::MAX` when the
    /// pattern has a `...rest`) into a fresh array; arrays/strings/Map/Set (and
    /// anything else) pass through, since positional `GetIndex` already handles
    /// them. Bounding keeps `let [a,b] = infiniteIterator` from looping forever.
    IterToArray {
        dst: Reg,
        src: Reg,
        count: u32,
    },

    /// Return `src` from the current function.
    Return {
        src: Reg,
    },
    /// Return undefined.
    ReturnUndefined,

    /// `console.log`-style print of `argc` values starting at `arg_base`.
    /// A dedicated opcode keeps the v1 stdlib trivial; later this becomes an
    /// ordinary builtin call. `to_stderr` is set for `console.error`/`warn`
    /// (which write to stderr in node), clear for `log`/`info`/`debug`.
    Print {
        arg_base: Reg,
        argc: u16,
        to_stderr: bool,
    },
}

/// A compiled function: its code, register-file size, parameter count, and the
/// constant pool it references.
/// What a static `import` binds locally.
#[derive(Clone, Debug)]
pub enum ImportName {
    /// `import { x } from` / `import { x as y } from` — the EXPORTED name.
    Named(String),
    /// `import d from`.
    Default,
    /// `import * as ns from`.
    Namespace,
    /// `import './m'` — evaluate for side effects only.
    SideEffect,
    /// A bindingless phase import: the request is LOADED (an unresolvable
    /// specifier is a host error) but never linked/evaluated.
    LoadOnly,
    /// `import source x from` — the local binds the target module's
    /// ModuleSource object (%AbstractModuleSource%-prototype-linked); the
    /// target is loaded but never linked/evaluated.
    Source,
    /// `import defer * as ns from` — the local binds the module's DEFERRED
    /// namespace: the graph loads at link time but evaluates on first
    /// (triggering) namespace access.
    DeferNamespace,
}

/// One static import binding (or side-effect import) of a module.
#[derive(Clone, Debug)]
pub struct ImportEntry {
    /// Compile-time global slot of the LOCAL binding (u32::MAX for SideEffect).
    pub local_slot: u32,
    pub import: ImportName,
    pub specifier: String,
    /// The `type` import attribute (`with { type: "json" }`), driving the
    /// loader's synthetic JSON/text module semantics. None = ordinary module.
    pub mtype: Option<String>,
}

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
    /// True when this function has NO [[Construct]] slot independent of its
    /// generator/async kind — an arrow function or a concise method (object or
    /// class method/getter/setter). `new` on it (and `class extends` it) is a
    /// TypeError. Generators/async are already non-constructable via the flags
    /// above; this covers the remaining cases. Plain function declarations/
    /// expressions are constructable (false).
    pub non_constructable: bool,
    /// True for an arrow function: it captures `this` (and `super`/`arguments`/
    /// `new.target`) LEXICALLY from its defining scope rather than receiving its
    /// own. The arrow value is always a `Closure` carrying the captured `this`;
    /// at every call entry reg 0 is rebound to that captured value, ignoring any
    /// `this` supplied by the caller (`.call`/`.apply`/`bind`/a method receiver/
    /// an array-method thisArg). Also suppresses OrdinaryCallBindThis.
    pub lexical_this: bool,
    /// True for a STATIC class element body — a static method/getter/setter or a
    /// `static { … }` block (and an arrow lexically inside one). `super.x` /
    /// `super[x]` there resolves against the class's [[Prototype]] (the PARENT
    /// CLASS) rather than the class prototype's [[Prototype]]. Threaded like the
    /// super home-class id through nested arrows; false everywhere else.
    pub super_static: bool,
    /// True when this function runs in strict mode (own `"use strict"` directive,
    /// a strict enclosing scope, a class body, or module code). Strict functions
    /// receive `this` exactly as passed; sloppy functions called with a nullish
    /// `this` substitute the global object (OrdinaryCallBindThis, ThisMode global).
    pub is_strict: bool,
    /// True when the formal parameter list is SIMPLE — every parameter a plain
    /// identifier, no defaults, no rest, no destructuring. A SLOPPY function
    /// with simple parameters gets a MAPPED arguments object ([[ParameterMap]]
    /// aliasing between `arguments[i]` and the formal parameters).
    pub simple_params: bool,
    pub constants: Vec<Value>,
    /// Heap-string constants referenced by `LoadConst` need their text; this
    /// parallels `constants` for the string case (resolved at load time).
    pub string_constants: Vec<String>,
    /// Compiler-prepared key lists for allocation-surviving object literals.
    /// The backing `Arc` is immutable after compilation and can be shared by
    /// every object created at the same literal site without joining the GC
    /// graph or a process-global attacker-fillable interner.
    pub static_key_plans: Vec<StaticKeyPlan>,
    /// BigInt literal constants BEYOND i128 (`LoadBigIntBig` indexes here),
    /// parsed once at compile time. In-range literals stay inline in
    /// `LoadBigInt`; this pool is empty for virtually every function.
    pub bigint_consts: Vec<num_bigint::BigInt>,
    /// `string_constants` indices whose text is the oxc LONE-SURROGATE marker
    /// form (`\u{FFFD}XXXX` per lone surrogate, `\u{FFFD}fffd` for a literal
    /// U+FFFD — the parser's lossless encoding for `'\uD800'`-style literals).
    /// `resolve_const` decodes these to real WTF-8 lone surrogates at intern
    /// time. Sorted (push order is ascending); almost always empty.
    pub wtf8_consts: Vec<u32>,
    /// If this function's name is hoisted to a global binding, the slot index;
    /// the VM materialises a function object into that global at startup.
    pub name_global: Option<u32>,
    /// Upvalues this function captures, in order. Index `i` of a `UpvalGet`/
    /// `UpvalSet` refers to `upvalues[i]`. Each entry says where the DEFINING
    /// frame finds the cell to capture: a local register holding a cell, or one
    /// of the defining frame's own upvalues (nested-of-nested capture).
    pub upvalues: Vec<UpvalSource>,
    /// Per direct-eval CALL SITE in this function: the visible caller bindings
    /// (name, kind, idx) — kind 0 = a boxed local CELL in register idx, kind 1
    /// = an eval root's own upvalue (forwarded caller scope). The eval program
    /// is built as a CLOSURE over these cells. The second tuple element is the
    /// PARAM-SCOPE collision list when the site sits in a parameter default
    /// (EvalDeclarationInstantiation: a sloppy eval declaring one of these
    /// names — the parameters or the implicit `arguments` — is a SyntaxError).
    /// Indexed by the DirectEval instr's `site` (u16::MAX = no map).
    /// The third element: the LEXICAL (`let`/`const`/`class`) caller binding
    /// names visible at the call site — a sloppy eval's var/function name
    /// colliding with one is a SyntaxError (EvalDeclarationInstantiation).
    pub eval_sites: Vec<(Vec<(String, u8, u16)>, Option<Vec<String>>, Vec<String>)>,
    /// Exact source text of this function (sliced from the program source by the
    /// function node's span), used by `Function.prototype.toString`. Empty for
    /// the synthetic top-level script body and for placeholders, in which case
    /// `toString` falls back to the native-function form.
    pub source: String,
}

/// Immutable property names shared by all objects created at one eligible
/// object-literal bytecode site. Equality/serialization must be by key text,
/// never by `Arc` identity.
#[derive(Debug)]
struct StaticKeyPlanData {
    keys: Vec<String>,
    /// Cached once at compilation/deserialization boundary so hot allocation
    /// does not rescan up to 256 strings. Invalid hand-built plans fail closed.
    runtime_valid: bool,
    /// Whether any key names an array element (canonical "0".."4294967294") —
    /// precomputed so the one-step `FinalizeObject` allocation keeps
    /// `ObjMap::has_element_key` exact without rescanning keys per object.
    has_element_key: bool,
}

#[derive(Clone, Debug)]
pub struct StaticKeyPlan(std::sync::Arc<StaticKeyPlanData>);

/// Retained-plan ceilings. A single compiler accepts only a modest number of
/// profitable literal sites; a live VM additionally bounds the aggregate from
/// separately-created eval/module Compilers before their FuncProtos are leaked.
pub(crate) const STATIC_KEY_PLAN_COMPILER_MAX_SITES: usize = 256;
pub(crate) const STATIC_KEY_PLAN_VM_MAX_SITES: usize = 4_096;
pub(crate) const STATIC_KEY_PLAN_MAX_RETAINED_BYTES: usize = 8 * 1024 * 1024;

/// One process-wide comparator latch shared by compilation and runtime. When
/// disabled, the compiler emits legacy NewObject bytecode and retains no plan
/// metadata; runtime checking remains for precompiled Programs.
#[inline]
pub(crate) fn static_key_plans_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static STATE: AtomicU8 = AtomicU8::new(0);
    match STATE.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            let on = std::env::var_os("ZIPP_NO_STATIC_KEY_PLANS").is_none();
            STATE.store(if on { 1 } else { 2 }, Ordering::Relaxed);
            on
        }
    }
}

/// Same-binary comparator for the B209 `SetHomeObject` elision: when disabled
/// (`ZIPP_NO_HOME_ELIDE=1`), every concise method/accessor gets its
/// [[HomeObject]] wired as before, super-free or not. Compile-time only.
#[inline]
pub(crate) fn home_elide_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static STATE: AtomicU8 = AtomicU8::new(0);
    match STATE.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            let on = std::env::var_os("ZIPP_NO_HOME_ELIDE").is_none();
            STATE.store(if on { 1 } else { 2 }, Ordering::Relaxed);
            on
        }
    }
}

/// Same-binary comparator for the one-step `FinalizeObject` literal lowering.
/// When disabled, the compiler emits the historical `NewPlannedObject` +
/// per-field `AppendDataProp` sequence (B167's shipped baseline); runtime
/// handling of already-compiled `FinalizeObject` bytecode is unchanged.
#[inline]
pub(crate) fn object_finalize_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static STATE: AtomicU8 = AtomicU8::new(0);
    match STATE.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            let on = std::env::var_os("ZIPP_NO_OBJECT_FINALIZE").is_none();
            STATE.store(if on { 1 } else { 2 }, Ordering::Relaxed);
            on
        }
    }
}

/// Conservative retained heap charge for one plan. This deliberately prices
/// more than key text:
///
/// * 96 bytes covers the outer plan-Vec slot/slack plus the Arc allocation,
///   strong/weak counters, inner Vec header, and allocator metadata;
/// * every key pays its 24-byte String record plus 24 bytes of allocator/slack;
/// * every cloned payload pays at least one 16-byte allocation block, rounded
///   up to a 16-byte boundary.
///
/// The accounting is fixed rather than `size_of`-derived so it remains
/// conservative on 32-bit targets too.
pub(crate) fn static_key_plan_retained_charge(keys: &[String]) -> Option<usize> {
    let mut charge = 96usize;
    for key in keys {
        let payload = key.len().max(1).checked_add(15)? & !15usize;
        charge = charge.checked_add(48)?.checked_add(payload)?;
    }
    Some(charge)
}

/// Actual plan allocations retained by a set of FuncProtos. Counting the
/// pools, rather than NewPlannedObject instructions, also charges unused
/// entries supplied by a hand-built Program. Invalid metadata declines before
/// walking key payloads, so an oversize hand-built plan cannot amplify the VM
/// admission pass.
pub(crate) fn static_key_plan_usage(functions: &[FuncProto]) -> Option<(usize, usize)> {
    let mut sites = 0usize;
    let mut bytes = 0usize;
    for plan in functions.iter().flat_map(|func| &func.static_key_plans) {
        if !plan.runtime_valid() {
            return None;
        }
        sites = sites.checked_add(1)?;
        bytes = bytes.checked_add(static_key_plan_retained_charge(plan.keys())?)?;
    }
    Some((sites, bytes))
}

/// Stack slots a `FinalizeObject` build stages its values in — the baked
/// JIT helper, the interpreter arm, and the compiler's admission cap
/// (`OBJECT_FINALIZE_MAX_FIELDS` is defined FROM this) are one number, so
/// no compiled program can exceed the buffer. The runtime checks that quote
/// it exist for a hand-built `Program`: a 17-key plan under the old `> 256`
/// guard indexed past a 16-slot buffer.
pub(crate) const FINALIZE_STAGE_SLOTS: usize = 16;

impl StaticKeyPlan {
    pub(crate) fn new(keys: Vec<String>) -> Self {
        let runtime_valid = if keys.len() > 256 {
            false
        } else {
            // RandomState's keyed hash avoids the quadratic common-prefix and
            // collision amplification of a prefix scan on hand-built or
            // deserialized plans. Compiler plans are already proven unique;
            // this is the bounded runtime trust-boundary validation.
            let mut seen = std::collections::HashSet::with_capacity(keys.len());
            keys.iter().all(|key| seen.insert(key.as_str()))
        };
        let has_element_key = keys.iter().any(|key| crate::heap::key_names_element(key));
        Self(std::sync::Arc::new(StaticKeyPlanData {
            keys,
            runtime_valid,
            has_element_key,
        }))
    }

    /// B239: do these two handles name the SAME plan? Plans are immutable and
    /// shared by `Arc`, so pointer equality is a sound (if incomplete) answer
    /// to "are these key sequences identical", and the only one cheap enough
    /// to ask on every object construction.
    #[cfg(not(feature = "safe-sandbox"))]
    #[inline]
    pub(crate) fn ptr_eq(a: &Self, b: &Self) -> bool {
        std::sync::Arc::ptr_eq(&a.0, &b.0)
    }

    /// Precomputed "any key names an array element" bit — see the field doc.
    #[inline]
    pub(crate) fn has_element_key(&self) -> bool {
        self.0.has_element_key
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.0.keys.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.0.keys.is_empty()
    }

    #[inline]
    pub fn keys(&self) -> &[String] {
        self.0.keys.as_slice()
    }

    #[inline]
    pub(crate) fn runtime_valid(&self) -> bool {
        self.0.runtime_valid
    }
}

#[cfg(test)]
mod static_key_plan_layout_tests {
    use super::*;

    /// `static_key_plans` adds one empty Vec (24 bytes) to every function even
    /// when the optimization never applies. Keep that startup/many-function
    /// cost, plus the retained-charge assumptions for non-empty plans, explicit.
    #[cfg(target_pointer_width = "64")]
    #[test]
    fn func_proto_and_plan_metadata_layouts_are_pinned() {
        assert_eq!(std::mem::size_of::<StaticKeyPlan>(), 8);
        assert_eq!(std::mem::size_of::<StaticKeyPlanData>(), 32);
        assert_eq!(std::mem::size_of::<FuncProto>(), 272);
    }
}

/// Where a closure's upvalue is sourced from, evaluated in the defining frame.
/// A builtin `Math` function, resolved at compile time from `Math.<name>(…)`.
/// `#[repr(u8)]`: the JIT passes `op as u32` to the pure `jit_math_unary` /
/// `jit_math_two` win64 helpers, which `transmute` the discriminant back. This
/// is sound ONLY because every variant is fieldless and the declaration order is
/// fixed — DO NOT reorder or remove variants without updating both helpers.
#[derive(Clone, Copy, Debug)]
#[repr(u8)]
pub enum MathFn {
    Abs,
    Floor,
    Ceil,
    Round,
    Trunc,
    Sign,
    Sqrt,
    Cbrt,
    Exp,
    Log,
    Log2,
    Log10,
    Expm1,
    Log1p,
    Sin,
    Cos,
    Tan,
    Asin,
    Acos,
    Atan,
    Sinh,
    Cosh,
    Tanh,
    Asinh,
    Acosh,
    Atanh,
    Clz32,
    Fround,
    Pow,
    Atan2,
    Imul,
    Min,
    Max,
    Hypot,
}

/// Number of `MathFn` variants — the discriminant range `0..MATH_FN_COUNT`
/// indexes the per-op tables (`MathIntrinsicGuard`). Kept in step with
/// `native::MATH_METHODS`, which the guard builder asserts.
pub const MATH_FN_COUNT: usize = 34;

impl MathFn {
    /// Map a `Math.<name>` method to its function, if supported.
    pub fn from_name(name: &str) -> Option<MathFn> {
        use MathFn::*;
        Some(match name {
            "abs" => Abs,
            "floor" => Floor,
            "ceil" => Ceil,
            "round" => Round,
            "trunc" => Trunc,
            "sign" => Sign,
            "sqrt" => Sqrt,
            "cbrt" => Cbrt,
            "exp" => Exp,
            "log" => Log,
            "log2" => Log2,
            "log10" => Log10,
            "expm1" => Expm1,
            "log1p" => Log1p,
            "sin" => Sin,
            "cos" => Cos,
            "tan" => Tan,
            "asin" => Asin,
            "acos" => Acos,
            "atan" => Atan,
            "sinh" => Sinh,
            "cosh" => Cosh,
            "tanh" => Tanh,
            "asinh" => Asinh,
            "acosh" => Acosh,
            "atanh" => Atanh,
            "clz32" => Clz32,
            "fround" => Fround,
            "pow" => Pow,
            "atan2" => Atan2,
            "imul" => Imul,
            "min" => Min,
            "max" => Max,
            "hypot" => Hypot,
            _ => return None,
        })
    }
}

/// A builtin global function, resolved at compile time from a bare call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum GlobalFn {
    Number,
    String,
    Boolean,
    ParseInt,
    ParseFloat,
    IsNaN,
    IsFinite,
}

/// Hot method spellings whose captured calls have a guarded RegExp-specialized
/// implementation. The spelling is only a compile-time hint: runtime identity
/// decides whether the direct implementation or exact ordinary-call fallback
/// runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum RegExpMethod {
    Test,
    Exec,
    MatchAll,
    Replace,
}

impl RegExpMethod {
    pub const COUNT: usize = 4;

    #[inline]
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "test" => Self::Test,
            "exec" => Self::Exec,
            "matchAll" => Self::MatchAll,
            "replace" => Self::Replace,
            _ => return None,
        })
    }

    #[inline]
    pub const fn index(self) -> usize {
        self as usize
    }
}

impl GlobalFn {
    pub const COUNT: usize = 7;

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

    #[inline]
    pub const fn index(self) -> usize {
        self as usize
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
    /// Slots of top-level FUNCTION/CLASS declarations — like `hoisted_globals`
    /// these are non-configurable, ENUMERABLE global bindings (vs the
    /// configurable, non-enumerable built-ins).
    pub decl_globals: Vec<u32>,
    /// Slots of top-level LEXICAL (`let`/`const`) declarations: a sloppy eval
    /// may not var/function-declare one of these names (SyntaxError per
    /// EvalDeclarationInstantiation), and they are NOT global-object props.
    pub lexical_globals: Vec<u32>,
    /// The `const` subset of `lexical_globals` (plus module import locals):
    /// `$262.evalScript` registers these as realm consts so a LATER script's
    /// assignment throws TypeError (same-program writes are compile errors).
    pub const_globals: Vec<u32>,
    /// For a sloppy FUNCTION-context eval program: the var/function names the
    /// eval declares into the caller's dynamic EvalScope (instead of globals).
    pub eval_dynamic_names: Vec<String>,
    /// For a MODULE program (a fixture loaded by a dynamic `import()`): the
    /// (exported name, local name) pairs. The loader reads each local's top-level
    /// binding after the module runs to build the import's namespace. Empty for
    /// ordinary scripts / eval.
    pub module_exports: Vec<(String, String)>,
    /// `export {imported as exported} from 'spec'` re-exports, as
    /// (exported, imported, specifier). The loader recursively loads `spec` and
    /// points the namespace's `exported` at the dependency's live `imported` slot.
    pub module_reexports: Vec<(String, String, String)>,
    /// `export * from 'spec'` star re-exports, as the specifier. The loader copies
    /// every export of `spec` (except `default`) into this module's namespace.
    pub module_star_reexports: Vec<String>,
    /// `export * as name from './m'` entries: (exported name, specifier). The
    /// loader imports the dependency and exports its NAMESPACE object.
    pub module_ns_reexports: Vec<(String, String)>,
    /// Static `import` declarations, in source order. The loader resolves each
    /// BEFORE the body runs: Named/Default locals ALIAS the dependency's live
    /// export slot (live bindings); Namespace locals receive the namespace
    /// value; SideEffect just evaluates the dependency.
    pub module_imports: Vec<ImportEntry>,
    /// Compile-time global slots DECLARED by a module's top level (var/let/const/
    /// function/class + the synthetic `*default*`). When loading a module these are
    /// remapped to PER-MODULE FRESH slots (not the realm's shared by-name slots) so
    /// two modules' same-named exports don't collide; free/builtin references stay
    /// realm-shared. Empty for ordinary scripts / eval.
    pub module_decl_globals: Vec<u32>,
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
    /// For a DERIVED class with an explicit ctor: a separate fields-only thunk
    /// (instance field initializers), run by the SuperCtor ops right after
    /// `super()` completes (spec InitializeInstanceElements timing). `None`
    /// when there are no instance fields or the ctor carries entry inits.
    pub field_thunk: Option<u32>,
    pub methods: Vec<(String, u32)>,
    /// `get name()` accessors: invoked (with `this` = instance) on property read.
    pub getters: Vec<(String, u32)>,
    /// `set name(v)` accessors: invoked (with `this` = instance) on property write.
    pub setters: Vec<(String, u32)>,
    /// Public INSTANCE prototype keys in SOURCE order, first definition keeping
    /// the position (OrdinaryDefineOwnProperty never moves an existing key).
    /// The three lists above are grouped by kind, so they cannot express the
    /// interleaving `class C { get g(){} m(){} }` requires — `getOwnPropertyNames`
    /// must answer ["constructor","g","m"], not ["constructor","m","g"].
    /// Computed keys park a "\u{1}cm{i}" placeholder here, renamed in place by
    /// `ClassAddMember` once the key value is known.
    pub proto_order: Vec<String>,
    /// `static name()` methods: own properties of the class value itself.
    pub statics: Vec<(String, u32)>,
    /// `static get name()` / `static set name(v)` accessors: invoked with
    /// `this` = the class value on read/write of `C.name`.
    pub static_getters: Vec<(String, u32)>,
    pub static_setters: Vec<(String, u32)>,
    /// Exact source text of the whole `class … { … }` (by the class node's span),
    /// returned by `Function.prototype.toString` on the class value.
    pub source: String,
    /// Names of instance + static FIELDS declared in the class body, including
    /// the "#" prefix for private fields. Used at `MakeClass` to register which
    /// private names this class's brand declares (methods/accessors are already
    /// in the lists above); lets a private access resolve to the precise
    /// declaring class instead of accepting any brand in the lexical chain.
    pub instance_field_names: Vec<String>,
    /// Names of STATIC fields (incl. "#" private ones) — separate from the
    /// instance list because a static private's brand lives on the class
    /// VALUE, not on instances (kind bit 8 at MakeClass registration).
    pub static_field_names: Vec<String>,
    /// The class's decorators, or `None` when it has none — which is every class
    /// in practically all existing code, so the whole decoration path is gated on
    /// one `Option` check rather than on empty `Vec`s.
    pub dec_plan: Option<Box<DecPlan>>,
}

/// The compile-time shape of a decorated class: what is decorated and how many
/// decorators each element carries. The decorator VALUES are not here — they are
/// arbitrary expressions evaluated at class-definition time into registers.
#[derive(Clone, Debug, Default)]
pub struct DecPlan {
    /// `@a @b class C {}` → 2. Their values are evaluated BEFORE the heritage
    /// (DecoratorList of a ClassDeclaration is evaluated before ClassTail) and
    /// applied after every element is decorated and installed.
    pub class_decorators: u32,
    /// Decorated class ELEMENTS in DOCUMENT order (the order their decorator
    /// expressions and keys must be evaluated in).
    pub elements: Vec<DecElemDef>,
}

/// One decorated class element.
#[derive(Clone, Debug)]
pub struct DecElemDef {
    /// 0 = method, 1 = getter, 2 = setter, 3 = field, 4 = auto-accessor.
    pub kind: u8,
    pub is_static: bool,
    pub is_private: bool,
    /// The element's static key, `#`-prefixed for a private name. Empty when the
    /// key is computed — the runtime then reads `DecState::keys[i]`, which the
    /// `DecKey` op filled when the key expression was evaluated.
    pub name: String,
    pub computed: bool,
    /// The element's key was WRITTEN as a computed well-known-symbol
    /// (`[Symbol.iterator]`) and constant-folded to the engine's reserved
    /// `"@@iterator"` spelling. `context.name` must be handed the Symbol, but a
    /// string-literal key that merely spells `"@@iterator"` must not be — the two
    /// are indistinguishable from `name` alone.
    pub sym_key: bool,
    /// For an auto-accessor: the unspellable private backing slot the get/set pair
    /// reads and writes (`#accessor@N`), which is the storage a returned `init`
    /// initializer feeds and what `access.get/set` must touch.
    pub storage: String,
}

/// The eight possible results of JS `typeof`, indexed by `TypeOfIs::code`.
/// Compared by CONTENT against what `Vm::type_of` returns, so the fused op can
/// never diverge from the unfused `TypeOf` — including the `[[IsHTMLDDA]]`
/// exotic (`document.all`), whose `type_of` answers "undefined" and therefore
/// matches code 3 here exactly as the unfused pair would.
pub const TYPEOF_NAMES: [&str; 8] = [
    "number",
    "string",
    "boolean",
    "undefined",
    "object",
    "function",
    "symbol",
    "bigint",
];

/// Compile-time mapping of a string literal to its `TypeOfIs::code`. `None`
/// for any other literal — the comparison can then never be true, which the
/// compiler encodes as code 255 (matches nothing) rather than declining the
/// fusion, so the operand's evaluation (and a `typeof undeclared`'s
/// non-throwing read) is preserved.
pub fn typeof_code(lit: &str) -> Option<u8> {
    TYPEOF_NAMES.iter().position(|&n| n == lit).map(|i| i as u8)
}
