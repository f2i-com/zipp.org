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

/// Sentinel `closure` value for a frame whose callee is a plain (capture-free)
/// function rather than a closure. Real heap indices are always `< u32::MAX`.
const NO_CLOSURE: u32 = u32::MAX;

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
enum EachMode {
    Map,
    Filter,
    ForEach,
}

/// Whether a promise reaction is the fulfill or reject handler.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ReactionKind {
    Fulfill,
    Reject,
}

/// How a suspended async activation is resumed: with an awaited value, or by
/// throwing a rejection into it at the await point.
#[derive(Clone, Copy)]
enum Resume {
    Value(Value),
    Throw(Value),
}

/// A queued microtask (the whole event loop). `Reaction` runs a promise reaction
/// — `callback` (a JS fn, a native BoundResolver, or undefined for pass-through)
/// applied to the settled `arg`, settling `dependent`. `AsyncResume` resumes a
/// suspended async activation.
enum Microtask {
    Reaction { callback: Value, arg: Value, dependent: u32, kind: ReactionKind, finally: bool },
    AsyncResume { activation: u32, input: Resume },
}

/// Native (built-in) function ids — the discriminant carried by `HeapObj::Native`.
/// Each maps to an arm of `Vm::call_native`.
mod native {
    pub const OBJ_DEFINE_PROPERTY: u16 = 1;
    pub const OBJ_DEFINE_PROPERTIES: u16 = 2;
    pub const OBJ_GET_OWN_DESC: u16 = 3;
    pub const OBJ_GET_OWN_NAMES: u16 = 4;
    pub const OBJ_GET_PROTO: u16 = 5;
    pub const OBJ_KEYS: u16 = 6;
    pub const OBJ_VALUES: u16 = 7;
    pub const OBJ_ENTRIES: u16 = 8;
    pub const OBJ_ASSIGN: u16 = 9;
    pub const OBJ_CREATE: u16 = 10;
    pub const PROTO_HAS_OWN: u16 = 11;
    pub const PROTO_PROP_ENUM: u16 = 12;
    pub const PROTO_IS_PROTO_OF: u16 = 13;
    pub const PROTO_VALUE_OF: u16 = 14;
    pub const PROTO_TO_STRING: u16 = 15;
    pub const FN_CALL: u16 = 16;
    pub const FN_APPLY: u16 = 17;
    pub const FN_BIND: u16 = 18;
    pub const ARR_IS_ARRAY: u16 = 19;
    pub const ARR_FROM: u16 = 20;
    pub const ARR_OF: u16 = 21;
    pub const ARR_JOIN: u16 = 22;
    pub const ARR_PUSH: u16 = 23;
    // Promise static methods as first-class values (`Promise.resolve`, …).
    pub const PROMISE_RESOLVE: u16 = 24;
    pub const PROMISE_REJECT: u16 = 25;
    pub const PROMISE_ALL: u16 = 26;
    pub const PROMISE_ALLSETTLED: u16 = 27;
    pub const PROMISE_RACE: u16 = 28;
    pub const PROMISE_ANY: u16 = 29;
    // More Object statics (as first-class values).
    pub const OBJ_IS: u16 = 30;
    pub const OBJ_HAS_OWN: u16 = 31;
    pub const OBJ_FROM_ENTRIES: u16 = 32;
    pub const OBJ_SET_PROTO_OF: u16 = 33;
    pub const OBJ_GET_OWN_SYMBOLS: u16 = 34;
    pub const OBJ_GET_OWN_DESCS: u16 = 35;
    // Integrity traits.
    pub const OBJ_FREEZE: u16 = 36;
    pub const OBJ_IS_FROZEN: u16 = 37;
    pub const OBJ_SEAL: u16 = 38;
    pub const OBJ_IS_SEALED: u16 = 39;
    pub const OBJ_PREVENT_EXT: u16 = 40;
    pub const OBJ_IS_EXT: u16 = 41;
    // ES2024 grouping + promise capability.
    pub const OBJ_GROUP_BY: u16 = 42;
    pub const MAP_GROUP_BY: u16 = 43;
    pub const PROMISE_WITH_RESOLVERS: u16 = 44;
    // Reflect namespace statics.
    pub const REFLECT_APPLY: u16 = 45;
    pub const REFLECT_CONSTRUCT: u16 = 46;
    pub const REFLECT_GET: u16 = 47;
    pub const REFLECT_SET: u16 = 48;
    pub const REFLECT_HAS: u16 = 49;
    pub const REFLECT_DELETE: u16 = 50;
    pub const REFLECT_OWN_KEYS: u16 = 51;
    pub const REFLECT_GET_PROTO: u16 = 52;
    pub const REFLECT_SET_PROTO: u16 = 53;
    pub const REFLECT_DEFINE: u16 = 54;
    pub const REFLECT_GET_OWN_DESC: u16 = 55;
    pub const REFLECT_IS_EXT: u16 = 56;
    pub const REFLECT_PREVENT_EXT: u16 = 57;
    // JSON namespace methods as first-class values.
    pub const JSON_PARSE: u16 = 58;
    pub const JSON_STRINGIFY: u16 = 59;
    pub const MATH_RANDOM: u16 = 60;
    // WeakMap/WeakSet methods (290+, clear of PROTO 64-190 and MATH 256-289).
    pub const WM_GET: u16 = 290;
    pub const WM_SET: u16 = 291;
    pub const WM_HAS: u16 = 292;
    pub const WM_DELETE: u16 = 293;
    pub const WS_ADD: u16 = 294;
    pub const WS_HAS: u16 = 295;
    pub const WS_DELETE: u16 = 296;
    pub const WR_DEREF: u16 = 297;
    pub const FR_REGISTER: u16 = 298;
    pub const FR_UNREGISTER: u16 = 299;
    // Built-in iterator methods.
    pub const ITER_NEXT: u16 = 300;
    pub const ITER_SELF: u16 = 301; // `[Symbol.iterator]()` → returns the iterator
    pub const PROTO_TO_LOCALE_STRING: u16 = 302; // Object.prototype.toLocaleString
    // Number static methods as first-class values (the CALL form is a StaticFn).
    pub const NUM_IS_INTEGER: u16 = 303;
    pub const NUM_IS_NAN: u16 = 304;
    pub const NUM_IS_FINITE: u16 = 305;
    pub const NUM_IS_SAFE_INTEGER: u16 = 306;
    // Global functions as first-class values (the CALL form is a GlobalFn).
    pub const GLOBAL_PARSE_INT: u16 = 307;
    pub const GLOBAL_PARSE_FLOAT: u16 = 308;
    pub const GLOBAL_IS_NAN: u16 = 309;
    pub const GLOBAL_IS_FINITE: u16 = 310;
    // String static methods.
    pub const STR_FROM_CHAR_CODE: u16 = 311;
    pub const STR_FROM_CODE_POINT: u16 = 312;
    pub const STR_RAW: u16 = 313;
    // Date static methods as first-class values (the call form uses Now/DateParse/DateUTC).
    pub const DATE_NOW: u16 = 314;
    pub const DATE_PARSE: u16 = 315;
    pub const DATE_UTC: u16 = 316;
    /// `Error.prototype.toString` — `name`/`message` → "name: message".
    pub const ERROR_TO_STRING: u16 = 317;
    /// Canonical native error names, indexed by error kind (parallels compile.rs
    /// `error_kind_index` and the VM's `error_protos`/`error_ctors`).
    pub const ERROR_NAMES: [&str; 8] = [
        "Error", "TypeError", "RangeError", "SyntaxError",
        "ReferenceError", "EvalError", "URIError", "AggregateError",
    ];
    /// TypedArray element kinds, indexed by `kind`: (ctor name, element byte size,
    /// is_bigint, is_float). Uint8Clamped (index 2) clamps on write.
    pub const TA_KINDS: &[(&str, usize, bool, bool)] = &[
        ("Int8Array", 1, false, false),
        ("Uint8Array", 1, false, false),
        ("Uint8ClampedArray", 1, false, false),
        ("Int16Array", 2, false, false),
        ("Uint16Array", 2, false, false),
        ("Int32Array", 4, false, false),
        ("Uint32Array", 4, false, false),
        ("Float32Array", 4, false, true),
        ("Float64Array", 8, false, true),
        ("BigInt64Array", 8, true, false),
        ("BigUint64Array", 8, true, false),
    ];
    /// `%TypedArray%.prototype` method names, registered as Natives at
    /// `TA_METHOD_BASE + index` so the value form (`TypedArray.prototype.map`,
    /// `.call(...)`, `typeof`) works; the method-call form dispatches directly.
    pub const TA_PROTO_METHODS: &[&str] = &[
        "at", "join", "toString", "indexOf", "lastIndexOf", "includes", "forEach", "map",
        "filter", "find", "findIndex", "findLast", "findLastIndex", "every", "some", "reduce",
        "reduceRight", "fill", "reverse", "slice", "subarray", "sort", "copyWithin", "set",
        "keys", "values", "entries", "@@iterator",
    ];
    pub const TA_METHOD_BASE: u16 = 340;
    /// `DataView.prototype` get/set method names (registered at DV_METHOD_BASE+i).
    pub const DV_PROTO_METHODS: &[&str] = &[
        "getInt8", "getUint8", "getInt16", "getUint16", "getInt32", "getUint32", "getFloat32",
        "getFloat64", "getBigInt64", "getBigUint64", "setInt8", "setUint8", "setInt16",
        "setUint16", "setInt32", "setUint32", "setFloat32", "setFloat64", "setBigInt64",
        "setBigUint64",
    ];
    pub const DV_METHOD_BASE: u16 = 372;
    pub const ARRAYBUFFER_SLICE: u16 = 396;
    pub const PROXY_REVOCABLE: u16 = 397;
    pub const PROXY_REVOKE: u16 = 398;
    /// Temporal.Duration.prototype instance methods (dispatched by name via
    /// `temporal_method`), at TEMPORAL_M_BASE + index.
    pub const TEMPORAL_DURATION_METHODS: &[&str] =
        &["with", "negated", "abs", "toString", "toJSON", "valueOf"];
    pub const TEMPORAL_M_BASE: u16 = 400;
    pub const TEMPORAL_DURATION_FROM: u16 = 410;
    pub const TEMPORAL_DURATION_COMPARE: u16 = 411;
    /// Temporal.PlainDate.prototype methods at PD_M_BASE + index.
    pub const PLAINDATE_METHODS: &[&str] = &[
        "with", "add", "subtract", "until", "since", "equals", "toString", "toJSON", "valueOf",
        "getISOFields", "toPlainDateTime",
    ];
    pub const PD_M_BASE: u16 = 420;
    pub const PLAINDATE_FROM: u16 = 448;
    pub const PLAINDATE_COMPARE: u16 = 449;
    /// Temporal.PlainTime.prototype methods at PT_M_BASE + index.
    pub const PLAINTIME_METHODS: &[&str] = &[
        "with", "add", "subtract", "until", "since", "round", "equals", "toString", "toJSON",
        "valueOf", "getISOFields",
    ];
    pub const PT_M_BASE: u16 = 450;
    pub const PLAINTIME_FROM: u16 = 470;
    pub const PLAINTIME_COMPARE: u16 = 471;
    /// Temporal.PlainDateTime.prototype methods at PDT_M_BASE + index.
    pub const PLAINDATETIME_METHODS: &[&str] = &[
        "with", "add", "subtract", "until", "since", "equals", "toString", "toJSON", "valueOf",
        "toPlainDate", "toPlainTime", "getISOFields",
    ];
    pub const PDT_M_BASE: u16 = 472;
    pub const PLAINDATETIME_FROM: u16 = 490;
    pub const PLAINDATETIME_COMPARE: u16 = 491;
    /// Temporal.Instant.prototype methods at INST_M_BASE + index.
    pub const INSTANT_METHODS: &[&str] = &[
        "add", "subtract", "until", "since", "round", "equals", "toString", "toJSON", "valueOf",
    ];
    pub const INST_M_BASE: u16 = 492;
    pub const INST_FROM: u16 = 505;
    pub const INST_FROM_EPOCH_MS: u16 = 506;
    pub const INST_FROM_EPOCH_NS: u16 = 507;
    pub const INST_FROM_EPOCH_SEC: u16 = 508;
    pub const INST_FROM_EPOCH_US: u16 = 509;
    pub const INST_COMPARE: u16 = 510;
    /// Temporal.PlainYearMonth.prototype methods at PYM_M_BASE + index.
    pub const PLAINYEARMONTH_METHODS: &[&str] = &[
        "with", "add", "subtract", "until", "since", "equals", "toString", "toJSON", "valueOf",
        "toPlainDate", "getISOFields",
    ];
    pub const PYM_M_BASE: u16 = 512;
    pub const PLAINYEARMONTH_FROM: u16 = 524;
    pub const PLAINYEARMONTH_COMPARE: u16 = 525;
    /// Temporal.PlainMonthDay.prototype methods at PMD_M_BASE + index.
    pub const PLAINMONTHDAY_METHODS: &[&str] = &[
        "with", "equals", "toString", "toJSON", "valueOf", "toPlainDate", "getISOFields",
    ];
    pub const PMD_M_BASE: u16 = 528;
    pub const PLAINMONTHDAY_FROM: u16 = 536;
    /// Field names of a Temporal.Duration, in slot order.
    pub const DURATION_FIELDS: [&str; 10] = [
        "years", "months", "weeks", "days", "hours", "minutes", "seconds",
        "milliseconds", "microseconds", "nanoseconds",
    ];
    // RegExp.prototype methods.
    pub const REGEXP_TEST: u16 = 326;
    pub const REGEXP_EXEC: u16 = 327;
    pub const REGEXP_TO_STRING: u16 = 328;
    // BigInt: statics + BigInt.prototype methods.
    pub const BIGINT_AS_INTN: u16 = 322;
    pub const BIGINT_AS_UINTN: u16 = 323;
    pub const BIGINT_TO_STRING: u16 = 324;
    pub const BIGINT_VALUE_OF: u16 = 325;
    // Symbol: the static methods + Symbol.prototype methods as first-class values.
    pub const SYMBOL_FOR: u16 = 318;
    pub const SYMBOL_KEY_FOR: u16 = 319;
    pub const SYMBOL_TO_STRING: u16 = 320; // Symbol.prototype.toString
    pub const SYMBOL_VALUE_OF: u16 = 321; // Symbol.prototype.valueOf
    /// The well-known symbols, as `(JS property name on `Symbol`, internal prop_key)`.
    /// The prop_key is the string the symbol uses as an object key — `@@iterator`
    /// etc. match the engine's existing iterator convention so iteration is unchanged.
    pub const WELL_KNOWN_SYMBOLS: &[(&str, &str)] = &[
        ("iterator", "@@iterator"),
        ("asyncIterator", "@@asyncIterator"),
        ("toPrimitive", "@@toPrimitive"),
        ("toStringTag", "@@toStringTag"),
        ("hasInstance", "@@hasInstance"),
        ("isConcatSpreadable", "@@isConcatSpreadable"),
        ("species", "@@species"),
        ("match", "@@match"),
        ("matchAll", "@@matchAll"),
        ("replace", "@@replace"),
        ("search", "@@search"),
        ("split", "@@split"),
        ("unscopables", "@@unscopables"),
        ("dispose", "@@dispose"),
        ("asyncDispose", "@@asyncDispose"),
    ];
    // Math methods as first-class values: id = MATH_METHOD_BASE + index into
    // MATH_METHODS, each carrying its MathFn + spec `length`. Base is well above the
    // PROTO_METHODS id range (64 + ~127) to avoid collision.
    pub const MATH_METHOD_BASE: u16 = 256;
    pub const MATH_METHODS: &[(&str, crate::bytecode::MathFn, u8)] = {
        use crate::bytecode::MathFn as F;
        &[
            ("abs", F::Abs, 1), ("floor", F::Floor, 1), ("ceil", F::Ceil, 1),
            ("round", F::Round, 1), ("trunc", F::Trunc, 1), ("sign", F::Sign, 1),
            ("sqrt", F::Sqrt, 1), ("cbrt", F::Cbrt, 1), ("exp", F::Exp, 1),
            ("log", F::Log, 1), ("log2", F::Log2, 1), ("log10", F::Log10, 1),
            ("expm1", F::Expm1, 1), ("log1p", F::Log1p, 1), ("sin", F::Sin, 1),
            ("cos", F::Cos, 1), ("tan", F::Tan, 1), ("asin", F::Asin, 1),
            ("acos", F::Acos, 1), ("atan", F::Atan, 1), ("sinh", F::Sinh, 1),
            ("cosh", F::Cosh, 1), ("tanh", F::Tanh, 1), ("asinh", F::Asinh, 1),
            ("acosh", F::Acosh, 1), ("atanh", F::Atanh, 1), ("clz32", F::Clz32, 1),
            ("fround", F::Fround, 1), ("pow", F::Pow, 2), ("atan2", F::Atan2, 2),
            ("imul", F::Imul, 2), ("min", F::Min, 2), ("max", F::Max, 2),
            ("hypot", F::Hypot, 2),
        ]
    };

    pub fn math_method(id: u16) -> Option<(&'static str, crate::bytecode::MathFn, u8)> {
        id.checked_sub(MATH_METHOD_BASE)
            .and_then(|i| MATH_METHODS.get(i as usize).copied())
    }

    /// First native id for a prototype method (`Array.prototype.map` etc.). Method
    /// `PROTO_METHODS[i]` has native id `PROTO_METHOD_BASE + i`, so these are
    /// first-class callable VALUES (`Array.prototype.map.call(arr, fn)`).
    pub const PROTO_METHOD_BASE: u16 = 64;

    /// Prototype methods exposed as values, paired with their receiver kind
    /// (0 = Array.prototype, 1 = String.prototype). Only methods that
    /// `array_method`/`string_method` actually implement are listed, so a `.call`
    /// through the value behaves identically to a direct `arr.method()` call.
    pub const PROTO_METHODS: &[(&str, u8, u8)] = &[
        // (name, kind, spec `length`). join/push already on arr_proto via ARR_*.
        // Array.prototype.
        ("at", 0, 1), ("concat", 0, 1), ("every", 0, 1), ("fill", 0, 1), ("filter", 0, 1),
        ("find", 0, 1), ("findIndex", 0, 1), ("findLast", 0, 1), ("findLastIndex", 0, 1),
        ("flat", 0, 0), ("flatMap", 0, 1), ("forEach", 0, 1), ("includes", 0, 1),
        ("indexOf", 0, 1), ("lastIndexOf", 0, 1), ("map", 0, 1), ("pop", 0, 0), ("reduce", 0, 1),
        ("reduceRight", 0, 1), ("reverse", 0, 0), ("shift", 0, 0), ("slice", 0, 2),
        ("some", 0, 1), ("sort", 0, 1), ("splice", 0, 2), ("toReversed", 0, 0),
        ("toSorted", 0, 1), ("toSpliced", 0, 2), ("toString", 0, 0), ("with", 0, 2),
        ("copyWithin", 0, 2), ("entries", 0, 0), ("keys", 0, 0), ("values", 0, 0),
        ("toLocaleString", 0, 0),
        // String.prototype.
        ("at", 1, 1), ("charAt", 1, 1), ("charCodeAt", 1, 1), ("codePointAt", 1, 1),
        ("endsWith", 1, 1), ("includes", 1, 1), ("indexOf", 1, 1), ("padEnd", 1, 1),
        ("padStart", 1, 1), ("repeat", 1, 1), ("replace", 1, 2), ("replaceAll", 1, 2),
        ("slice", 1, 2), ("split", 1, 2), ("startsWith", 1, 1), ("substring", 1, 2),
        ("toLowerCase", 1, 0), ("toUpperCase", 1, 0), ("trim", 1, 0), ("trimEnd", 1, 0),
        ("trimStart", 1, 0), ("concat", 1, 1), ("substr", 1, 2), ("localeCompare", 1, 1),
        ("normalize", 1, 0), ("isWellFormed", 1, 0), ("toWellFormed", 1, 0),
        ("valueOf", 1, 0), ("toString", 1, 0),
        // Number.prototype (kind 2 → number_method, receiver is a number value).
        ("toFixed", 2, 1), ("toString", 2, 1), ("valueOf", 2, 0), ("toLocaleString", 2, 0),
        // Set.prototype (kind 3 → set_method on the Set receiver).
        ("add", 3, 1), ("clear", 3, 0), ("delete", 3, 1), ("entries", 3, 0), ("forEach", 3, 1),
        ("has", 3, 1), ("keys", 3, 0), ("values", 3, 0), ("union", 3, 1), ("intersection", 3, 1),
        ("difference", 3, 1), ("symmetricDifference", 3, 1), ("isSubsetOf", 3, 1),
        ("isSupersetOf", 3, 1), ("isDisjointFrom", 3, 1),
        // Map.prototype (kind 4 → map_method on the Map receiver).
        ("clear", 4, 0), ("delete", 4, 1), ("entries", 4, 0), ("forEach", 4, 1), ("get", 4, 1),
        ("has", 4, 1), ("keys", 4, 0), ("set", 4, 2), ("values", 4, 0),
        // Boolean.prototype (kind 5 → boolean_method on the boolean value).
        ("toString", 5, 0), ("valueOf", 5, 0),
        // Promise.prototype (kind 7 → promise_method on the Promise receiver).
        ("then", 7, 2), ("catch", 7, 1), ("finally", 7, 1),
        // Date.prototype (kind 6 → date_method on the Date receiver). Getters length 0;
        // setters per spec (setHours=4, setMinutes/setFullYear=3, setMonth/setSeconds=2, …).
        ("getDate", 6, 0), ("getDay", 6, 0), ("getFullYear", 6, 0), ("getHours", 6, 0),
        ("getMilliseconds", 6, 0), ("getMinutes", 6, 0), ("getMonth", 6, 0), ("getSeconds", 6, 0),
        ("getTime", 6, 0), ("getTimezoneOffset", 6, 0), ("getUTCDate", 6, 0), ("getUTCDay", 6, 0),
        ("getUTCFullYear", 6, 0), ("getUTCHours", 6, 0), ("getUTCMilliseconds", 6, 0),
        ("getUTCMinutes", 6, 0), ("getUTCMonth", 6, 0), ("getUTCSeconds", 6, 0), ("setDate", 6, 1),
        ("setFullYear", 6, 3), ("setHours", 6, 4), ("setMilliseconds", 6, 1), ("setMinutes", 6, 3),
        ("setMonth", 6, 2), ("setSeconds", 6, 2), ("setTime", 6, 1), ("setUTCDate", 6, 1),
        ("setUTCFullYear", 6, 3), ("setUTCHours", 6, 4), ("setUTCMilliseconds", 6, 1),
        ("setUTCMinutes", 6, 3), ("setUTCMonth", 6, 2), ("setUTCSeconds", 6, 2), ("toDateString", 6, 0),
        ("toISOString", 6, 0), ("toJSON", 6, 1), ("toLocaleDateString", 6, 0), ("toLocaleString", 6, 0),
        ("toLocaleTimeString", 6, 0), ("toString", 6, 0), ("toTimeString", 6, 0), ("toUTCString", 6, 0),
        ("toGMTString", 6, 0), ("getYear", 6, 0), ("setYear", 6, 1),
        ("valueOf", 6, 0),
    ];

    /// `(name, kind)` for a prototype-method native id, if it is one.
    pub fn proto_method(id: u16) -> Option<(&'static str, u8, u8)> {
        id.checked_sub(PROTO_METHOD_BASE)
            .and_then(|i| PROTO_METHODS.get(i as usize).copied())
    }

    /// The spec `name` and `length` of a static/namespace native (Object.*,
    /// Reflect.*, Function.prototype.call, …) so it exposes real own `name`/
    /// `length` properties like any function. (Proto methods use `proto_method`.)
    pub fn static_name_length(id: u16) -> Option<(&'static str, u8)> {
        // %TypedArray%.prototype method natives.
        if (TA_METHOD_BASE..TA_METHOD_BASE + TA_PROTO_METHODS.len() as u16).contains(&id) {
            let m = TA_PROTO_METHODS[(id - TA_METHOD_BASE) as usize];
            let len: u8 = match m {
                "reverse" | "keys" | "values" | "entries" | "toString" | "@@iterator" => 0,
                "slice" | "subarray" | "copyWithin" => 2,
                _ => 1,
            };
            let name = if m == "@@iterator" { "[Symbol.iterator]" } else { m };
            return Some((name, len));
        }
        // DataView.prototype get*/set* natives (get* length 1, set* length 2).
        if (DV_METHOD_BASE..DV_METHOD_BASE + DV_PROTO_METHODS.len() as u16).contains(&id) {
            let m = DV_PROTO_METHODS[(id - DV_METHOD_BASE) as usize];
            return Some((m, if m.starts_with("set") { 2 } else { 1 }));
        }
        if id == ARRAYBUFFER_SLICE {
            return Some(("slice", 2));
        }
        Some(match id {
            OBJ_DEFINE_PROPERTY => ("defineProperty", 3),
            OBJ_DEFINE_PROPERTIES => ("defineProperties", 2),
            OBJ_GET_OWN_DESC => ("getOwnPropertyDescriptor", 2),
            OBJ_GET_OWN_NAMES => ("getOwnPropertyNames", 1),
            OBJ_GET_PROTO => ("getPrototypeOf", 1),
            OBJ_KEYS => ("keys", 1),
            OBJ_VALUES => ("values", 1),
            OBJ_ENTRIES => ("entries", 1),
            OBJ_ASSIGN => ("assign", 2),
            OBJ_CREATE => ("create", 2),
            PROTO_HAS_OWN => ("hasOwnProperty", 1),
            PROTO_PROP_ENUM => ("propertyIsEnumerable", 1),
            PROTO_IS_PROTO_OF => ("isPrototypeOf", 1),
            PROTO_VALUE_OF => ("valueOf", 0),
            PROTO_TO_STRING => ("toString", 0),
            ERROR_TO_STRING => ("toString", 0),
            SYMBOL_FOR => ("for", 1),
            SYMBOL_KEY_FOR => ("keyFor", 1),
            SYMBOL_TO_STRING => ("toString", 0),
            SYMBOL_VALUE_OF => ("valueOf", 0),
            BIGINT_TO_STRING => ("toString", 0),
            BIGINT_VALUE_OF => ("valueOf", 0),
            BIGINT_AS_INTN => ("asIntN", 2),
            BIGINT_AS_UINTN => ("asUintN", 2),
            REGEXP_TEST => ("test", 1),
            REGEXP_EXEC => ("exec", 1),
            REGEXP_TO_STRING => ("toString", 0),
            FN_CALL => ("call", 1),
            FN_APPLY => ("apply", 2),
            FN_BIND => ("bind", 1),
            ARR_IS_ARRAY => ("isArray", 1),
            ARR_FROM => ("from", 1),
            ARR_OF => ("of", 0),
            ARR_JOIN => ("join", 1),
            ARR_PUSH => ("push", 1),
            PROMISE_RESOLVE => ("resolve", 1),
            PROMISE_REJECT => ("reject", 1),
            PROMISE_ALL => ("all", 1),
            PROMISE_ALLSETTLED => ("allSettled", 1),
            PROMISE_RACE => ("race", 1),
            PROMISE_ANY => ("any", 1),
            OBJ_IS => ("is", 2),
            OBJ_HAS_OWN => ("hasOwn", 2),
            OBJ_FROM_ENTRIES => ("fromEntries", 1),
            OBJ_SET_PROTO_OF => ("setPrototypeOf", 2),
            OBJ_GET_OWN_SYMBOLS => ("getOwnPropertySymbols", 1),
            OBJ_GET_OWN_DESCS => ("getOwnPropertyDescriptors", 1),
            OBJ_FREEZE => ("freeze", 1),
            OBJ_IS_FROZEN => ("isFrozen", 1),
            OBJ_SEAL => ("seal", 1),
            OBJ_IS_SEALED => ("isSealed", 1),
            OBJ_PREVENT_EXT => ("preventExtensions", 1),
            OBJ_IS_EXT => ("isExtensible", 1),
            OBJ_GROUP_BY => ("groupBy", 2),
            MAP_GROUP_BY => ("groupBy", 2),
            PROMISE_WITH_RESOLVERS => ("withResolvers", 0),
            REFLECT_APPLY => ("apply", 3),
            REFLECT_CONSTRUCT => ("construct", 2),
            REFLECT_GET => ("get", 2),
            REFLECT_SET => ("set", 3),
            REFLECT_HAS => ("has", 2),
            REFLECT_DELETE => ("deleteProperty", 2),
            REFLECT_OWN_KEYS => ("ownKeys", 1),
            REFLECT_GET_PROTO => ("getPrototypeOf", 1),
            REFLECT_SET_PROTO => ("setPrototypeOf", 2),
            REFLECT_DEFINE => ("defineProperty", 3),
            REFLECT_GET_OWN_DESC => ("getOwnPropertyDescriptor", 2),
            REFLECT_IS_EXT => ("isExtensible", 1),
            REFLECT_PREVENT_EXT => ("preventExtensions", 1),
            JSON_PARSE => ("parse", 2),
            JSON_STRINGIFY => ("stringify", 3),
            MATH_RANDOM => ("random", 0),
            WM_GET => ("get", 1),
            WM_SET => ("set", 2),
            WM_HAS => ("has", 1),
            WM_DELETE => ("delete", 1),
            WS_ADD => ("add", 1),
            WS_HAS => ("has", 1),
            WS_DELETE => ("delete", 1),
            WR_DEREF => ("deref", 0),
            FR_REGISTER => ("register", 2),
            FR_UNREGISTER => ("unregister", 1),
            ITER_NEXT => ("next", 0),
            ITER_SELF => ("[Symbol.iterator]", 0),
            PROTO_TO_LOCALE_STRING => ("toLocaleString", 0),
            NUM_IS_INTEGER => ("isInteger", 1),
            NUM_IS_NAN => ("isNaN", 1),
            NUM_IS_FINITE => ("isFinite", 1),
            NUM_IS_SAFE_INTEGER => ("isSafeInteger", 1),
            GLOBAL_PARSE_INT => ("parseInt", 2),
            GLOBAL_PARSE_FLOAT => ("parseFloat", 1),
            GLOBAL_IS_NAN => ("isNaN", 1),
            GLOBAL_IS_FINITE => ("isFinite", 1),
            STR_FROM_CHAR_CODE => ("fromCharCode", 1),
            STR_FROM_CODE_POINT => ("fromCodePoint", 1),
            STR_RAW => ("raw", 1),
            DATE_NOW => ("now", 0),
            DATE_PARSE => ("parse", 1),
            DATE_UTC => ("UTC", 7),
            _ => return None,
        })
    }
}

/// What `object_enum_own` collects.
#[derive(Clone, Copy)]
enum EnumWhat {
    Keys,
    Values,
    Entries,
}

pub struct Vm<'p> {
    program: &'p Program,
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
    /// Extra own properties of a regex match-result Array (`.index`, `.input`,
    /// `.groups`), keyed by the result array's heap index — our `Array` is a plain
    /// `Vec` with no slot for named properties, so they live in this side table
    /// (mirroring `template_raws`). Tuple is (index, input, groups).
    regexp_match_extras: std::collections::HashMap<u32, (Value, Value, Value)>,
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
    /// Monotonic counter giving each `Symbol()` a unique internal property key
    /// (`@@sym:N`), so distinct symbols never collide as object keys.
    symbol_counter: u64,
    /// The `Symbol.for` global registry: registry key string → the shared Symbol.
    symbol_registry: std::collections::HashMap<String, Value>,
    /// Internal prop_key (`@@iterator`, `@@sym:N`, …) → the Symbol value, so a
    /// symbol-keyed own property can be reflected back to its Symbol by
    /// `Object.getOwnPropertySymbols`.
    symbol_keys: std::collections::HashMap<String, Value>,
    /// `%ArrayIteratorPrototype%` — the prototype of Array entries/keys/values
    /// iterators (and the default array `@@iterator`). 0 until set up.
    array_iter_proto: u32,
    /// `%MapIteratorPrototype%` / `%SetIteratorPrototype%` — distinct prototypes so
    /// `getPrototypeOf(map.entries())` differs from a Set/Array iterator's.
    map_iter_proto: u32,
    set_iter_proto: u32,
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
}

/// A thrown JS value rendered to a message (v1 throws are strings/RangeError).
#[derive(Debug)]
pub struct Thrown(pub String);

impl<'p> Vm<'p> {
    pub fn new(program: &'p Program) -> Vm<'p> {
        let mut heap = Heap::new();
        // Pre-load string constants of every function into the heap so
        // `LoadConst` of a string resolves to a stable heap index. We rewrite
        // string-constant slots to carry their heap index as an Int payload
        // marker is avoided — instead the compiler emits heap Values directly
        // (see `intern_strings`).
        // `global_count` real slots, plus a fixed POOL of extra slots the JIT uses
        // as scratch "field globals" for object scalar-replacement (SROA): a
        // field-promoted region's GetProp/SetProp are rewritten to Load/StoreGlobal
        // on pool slots, and the interpreter syncs object.field ↔ pool slot around
        // the native run. Sized once here so the globals Vec never reallocates at
        // runtime (the JIT pins its base pointer).
        let mut globals = vec![Value::UNDEFINED; program.global_count as usize + FIELD_POOL];
        // Real global slots start as the never-declared sentinel: a LoadGlobal of
        // one throws ReferenceError unless a builtin (setup_globals), a hoisted
        // function, a top-level `var` (hoisted to undefined just below), or a
        // StoreGlobal writes it first. The JIT scratch pool (past global_count)
        // stays undefined.
        for slot in globals.iter_mut().take(program.global_count as usize) {
            *slot = Value::UNINITIALIZED;
        }
        for &slot in &program.hoisted_globals {
            if (slot as usize) < globals.len() {
                globals[slot as usize] = Value::UNDEFINED;
            }
        }
        let _ = &mut heap;
        Vm {
            program,
            class_values: vec![None; program.classes.len()],
            heap,
            globals,
            regs: Vec::new(),
            frames: Vec::new(),
            output: Vec::new(),
            errput: Vec::new(),
            start: std::time::Instant::now(),
            pending_throw: None,
            pending_yield: None,
            pending_await: None,
            microtasks: std::collections::VecDeque::new(),
            template_raws: std::collections::HashMap::new(),
            prototypes: std::collections::HashMap::new(),
            proto_of: std::collections::HashMap::new(),
            fn_props: std::collections::HashMap::new(),
            deleted_callable_intrinsics: std::collections::HashSet::new(),
            obj_proto: 0,
            fn_proto: 0,
            arr_proto: 0,
            str_proto: 0,
            map_proto: 0,
            set_proto: 0,
            date_proto: 0,
            promise_proto: 0,
            num_proto: 0,
            bool_proto: 0,
            weakmap_proto: 0,
            weakset_proto: 0,
            weakref_proto: 0,
            finreg_proto: 0,
            error_protos: [0; 8],
            error_ctors: [0; 8],
            symbol_proto: 0,
            symbol_ctor: 0,
            bigint_proto: 0,
            bigint_ctor: 0,
            regexp_proto: 0,
            regexp_ctor: 0,
            regexp_match_extras: std::collections::HashMap::new(),
            ta_base_ctor: 0,
            ta_base_proto: 0,
            ta_ctors: [0; 11],
            ta_protos: [0; 11],
            arraybuffer_ctor: 0,
            arraybuffer_proto: 0,
            dataview_ctor: 0,
            dataview_proto: 0,
            proxy_ctor: 0,
            temporal_ns: 0,
            duration_ctor: 0,
            duration_proto: 0,
            plaindate_ctor: 0,
            plaindate_proto: 0,
            plaintime_ctor: 0,
            plaintime_proto: 0,
            plaindatetime_ctor: 0,
            plaindatetime_proto: 0,
            instant_ctor: 0,
            instant_proto: 0,
            plainyearmonth_ctor: 0,
            plainyearmonth_proto: 0,
            plainmonthday_ctor: 0,
            plainmonthday_proto: 0,
            symbol_counter: 0,
            symbol_registry: std::collections::HashMap::new(),
            symbol_keys: std::collections::HashMap::new(),
            array_iter_proto: 0,
            map_iter_proto: 0,
            set_iter_proto: 0,
            global_this: 0,
            rng_state: 0x9E37_79B9_7F4A_7C15, // fixed seed (golden-ratio constant)
            #[cfg(all(feature = "jit", target_arch = "x86_64"))]
            jit: crate::codegen::Jit::new(),
            #[cfg(all(feature = "jit", target_arch = "x86_64"))]
            jit_enabled: std::env::var_os("ZIPP_NOJIT").is_none(),
            #[cfg(all(feature = "jit", target_arch = "x86_64"))]
            jit_recurse_depth: 0,
            #[cfg(all(feature = "jit", target_arch = "x86_64"))]
            reg_capacity: 0,
            #[cfg(all(feature = "jit", target_arch = "x86_64"))]
            regs_hw: 0,
        }
    }

    /// Force the JIT on/off (overrides the `ZIPP_NOJIT` default). Used by the
    /// test suite to run a program both ways and assert the outputs match.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    #[allow(dead_code)] // used by the differential test harness (run_nojit)
    pub(crate) fn set_jit_enabled(&mut self, on: bool) {
        self.jit_enabled = on;
    }

    /// Would growing `self.regs` to `needed` slots exceed the pinned capacity?
    /// (Interpreter-only builds: never — there is no pinned native pointer to
    /// protect, so the Vec may grow/reallocate freely.)
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    #[inline]
    fn regs_would_overflow(&self, needed: usize) -> bool {
        self.reg_capacity != 0 && needed > self.reg_capacity
    }
    #[cfg(not(all(feature = "jit", target_arch = "x86_64")))]
    #[inline]
    fn regs_would_overflow(&self, _needed: usize) -> bool {
        false
    }

    /// The native self-recursive-call implementation behind the `jit_self_call`
    /// FFI trampoline. Runs `self`-recursion natively on a fresh register window
    /// appended to `self.regs`. Returns result Value bits, or
    /// `codegen::SELF_CALL_DEOPT` to make the native caller bail to the interp.
    ///
    /// Register-stability invariant: `self.regs` has reserved capacity for the
    /// whole recursion (`reserve_jit_regs`), so appending the callee window
    /// NEVER reallocates — the native CALLER's window pointer (`rbx`) therefore
    /// stays valid across this call. We `truncate` back to the caller's length
    /// before returning so the register file is exactly as the caller left it.
    ///
    /// NOTE: superseded by the inline native→native fast path + `jit_self_call_at`
    /// (the codegen now calls its own entry directly, no per-call Rust). Retained
    /// for reference / potential reuse; not on any hot path.
    #[allow(dead_code)]
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    fn jit_self_call_impl(&mut self, func_id: u32, args: *const u64, argc: usize) -> u64 {
        // Depth guard: deopt (not crash) past the native recursion budget; the
        // interpreter path then enforces MAX_FRAMES / throws RangeError.
        if self.jit_recurse_depth >= JIT_SELF_RECURSE_MAX {
            return crate::codegen::SELF_CALL_DEOPT;
        }
        // Native entry via the one-entry self-call cache (skips the HashMap
        // lookup on the hot recursive path — it always targets the same func_id).
        let entry = match self.jit.self_call_entry(func_id) {
            Some(e) => e,
            None => return crate::codegen::SELF_CALL_DEOPT,
        };
        let proto = &self.program.functions[func_id as usize];
        let reg_count = (proto.reg_count as usize).max(1);
        let params = proto.param_count as usize;

        // Fresh window appended to regs. Reserved capacity guarantees no realloc.
        let new_base = self.regs.len();
        let needed = new_base + reg_count;
        if needed > self.regs.capacity() {
            // Out of reserved headroom — deopt rather than risk a realloc that
            // would invalidate the caller's live `rbx`.
            return crate::codegen::SELF_CALL_DEOPT;
        }
        if needed > self.regs_hw {
            // New ground: zero-fill the freshly exposed slots and advance the mark.
            self.regs.resize(needed, Value::UNDEFINED);
            self.regs_hw = needed;
        } else {
            // Window lies within already-initialized memory (a previous recursion
            // reached at least this deep). Slots hold valid Value bits (stale, but
            // the compiled code writes before it reads), so skip the zero-fill —
            // this is the hot path for all but the deepest recursive call.
            // SAFETY: needed ≤ regs_hw ≤ a prior len ≤ capacity; [0..regs_hw] was
            // initialized by an earlier resize and the buffer is pinned, so these
            // slots are live, valid `Value`s.
            unsafe {
                self.regs.set_len(needed);
            }
        }
        // reg 0 = `this` (undefined for a plain self-call); params at 1..
        self.regs[new_base] = Value::UNDEFINED;
        let n = argc.min(params);
        for i in 0..n {
            // SAFETY: args points to `argc` valid Value bits (the caller's reg
            // window); n ≤ argc.
            self.regs[new_base + 1 + i] = Value::from_bits(unsafe { *args.add(i) });
        }

        self.jit_recurse_depth += 1;
        let regs_ptr = unsafe { self.regs.as_mut_ptr().add(new_base) } as *mut u64;
        let vm_ptr = self as *mut Vm as *mut core::ffi::c_void;
        // Call the cached native entry directly (same win64 ABI as JitFn::run).
        // SAFETY: `entry` is this function's compiled win64 code (stable across
        // HashMap rehashes); the window has `reg_count` valid slots; vm is valid.
        let (bits, bail) = unsafe {
            let f: extern "win64" fn(*mut u64, *mut u32, *mut core::ffi::c_void) -> u64 =
                core::mem::transmute(entry);
            let mut bail: u32 = crate::codegen::NO_BAIL;
            let r = f(regs_ptr, &mut bail as *mut u32, vm_ptr);
            (r, bail)
        };

        let result_bits = if bail == crate::codegen::NO_BAIL {
            bits
        } else {
            // The native callee bailed mid-body: finish this activation on the
            // interpreter over the SAME window via a transient frame. The frame
            // base is `new_base` into self.regs (stable — reserved capacity).
            self.frames.push(Frame {
                func: func_id,
                base: new_base,
                ip: bail as usize,
                ret_dst: 0,
                closure: NO_CLOSURE,
                handlers: Vec::new(),
            });
            let stop = self.frames.len() - 1;
            match self.run_loop(stop) {
                Ok(v) => v.bits(),
                // A throw inside the recursion: there is no JS-level way to
                // surface it through the native ABI here, so deopt the whole
                // self-call. pending_throw stays set; the interpreter caller
                // (the original top-level run_loop) re-raises it. We restore
                // regs and return the sentinel.
                Err(_) => {
                    self.jit_recurse_depth -= 1;
                    self.regs.truncate(new_base);
                    return crate::codegen::SELF_CALL_DEOPT;
                }
            }
        };

        self.jit_recurse_depth -= 1;
        self.regs.truncate(new_base);
        result_bits
    }

    /// Slow/finish path for the JIT's inline native→native self-call. Called
    /// when the inline fast path can't complete a recursive call purely natively:
    /// either the native depth limit was hit, or the callee bailed mid-body. The
    /// caller passes its window base EXPLICITLY (`caller_base_ptr`, the native
    /// `rbx`) because the fast path tracks windows by raw pointer, not
    /// `self.regs.len()`. Runs the activation on the interpreter over a transient
    /// frame at the callee window, holding `jit_recurse_depth` ELEVATED for the
    /// duration so the dispatch JIT-entry gate (`== 0`) stays closed and the
    /// recursion can't re-enter native and livelock — frames then accumulate
    /// monotonically to `MAX_FRAMES` → catchable RangeError. Returns the result
    /// bits, or `SELF_CALL_DEOPT` if the activation threw (the throw is left in
    /// `pending_throw`; the native chain unwinds and the top-level interpreter
    /// re-raises it).
    ///
    /// # Safety
    /// `caller_base_ptr` is the caller's window base within `self.regs`; `args`
    /// points to `argc` valid `Value` bits.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    fn jit_self_call_at_impl(
        &mut self,
        func_id: u32,
        caller_base_ptr: *const u64,
        args: *const u64,
        argc: usize,
    ) -> u64 {
        let proto = &self.program.functions[func_id as usize];
        let reg_count = (proto.reg_count as usize).max(1);
        let params = proto.param_count as usize;

        // Caller window base as a slot index (the fast path placed it by raw
        // pointer); the callee window sits contiguously above it.
        let regs_base = self.regs.as_ptr() as *const u64;
        // SAFETY: caller_base_ptr lies within self.regs' (non-reallocating) buffer.
        let caller_base =
            unsafe { (caller_base_ptr).offset_from(regs_base) } as usize;
        let new_base = caller_base + reg_count;
        let needed = new_base + reg_count;
        if self.regs_would_overflow(needed) {
            // Out of reserved register headroom (very deep): treat as stack
            // overflow — throw so the interpreter surfaces a catchable RangeError.
            let e =
                self.alloc_error_from_message("RangeError: Maximum call stack size exceeded");
            self.pending_throw = Some(e);
            return crate::codegen::SELF_CALL_DEOPT;
        }

        // RESYNC self.regs.len() to span the callee window so the transient
        // interpreter frame + MAX_FRAMES accounting are consistent. Save the
        // entry length and restore it on the way out (the native caller doesn't
        // use `len`, but the eventual return to the dispatch loop expects it
        // unchanged).
        let saved_len = self.regs.len();
        // CRITICAL: grow `len` with `set_len`, NOT `resize`. The native fast path
        // advanced the register windows by raw pointer WITHOUT touching
        // `self.regs.len()`, so on entry here `len` (≈ the warmup top) is far below
        // the live native windows, which occupy slots up to `new_base`. A
        // `resize(needed, UNDEFINED)` would ZERO-FILL `[len, needed)` — overwriting
        // every parked native frame's registers with `undefined` and corrupting the
        // recursion (this was the bug that capped JIT recursion below the
        // interpreter). The native windows hold valid `Value`s already (each native
        // frame defs its registers before reading — the same def-before-use
        // invariant the leaf JIT relies on), and the buffer is pinned to capacity
        // by `reserve_jit_regs`, so simply exposing them via `set_len` is correct.
        // Bounds: `needed ≤ capacity` (guarded above by `regs_would_overflow`).
        // SAFETY: `needed ≤ capacity`; slots `[0, needed)` are live `Value`s —
        // `[0, len)` from the interpreter, `[len, new_base+reg_count)` written by
        // the native frames whose windows we're spanning.
        unsafe { self.regs.set_len(needed); }
        if needed > self.regs_hw {
            self.regs_hw = needed;
        }
        self.regs[new_base] = Value::UNDEFINED;
        let n = argc.min(params);
        for i in 0..n {
            self.regs[new_base + 1 + i] = Value::from_bits(unsafe { *args.add(i) });
        }

        // Run this activation on the interpreter via a transient frame. Depth is
        // held ELEVATED across the whole run (we only restore it after), so any
        // self-call inside stays interpreted (the dispatch gate sees depth != 0)
        // and the recursion can't re-enter native → no livelock; frames grow to
        // MAX_FRAMES → RangeError on runaway.
        self.jit_recurse_depth += 1;
        self.frames.push(Frame {
            func: func_id,
            base: new_base,
            ip: 0,
            ret_dst: 0,
            closure: NO_CLOSURE,
            handlers: Vec::new(),
        });
        let stop = self.frames.len() - 1;
        let r = self.run_loop(stop);
        self.jit_recurse_depth -= 1;
        // SAFETY: restore the entry length (allocation unchanged, slots valid).
        unsafe { self.regs.set_len(saved_len); }
        match r {
            Ok(v) => v.bits(),
            // Threw (e.g. RangeError): leave it in pending_throw and signal the
            // native caller to unwind; the top-level interpreter re-raises it.
            Err(_) => crate::codegen::SELF_CALL_DEOPT,
        }
    }

    /// Reserve enough register-file capacity that a full JIT self-recursion
    /// never reallocates `self.regs` (which would dangle native window
    /// pointers). Called before entering the top-level run.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    fn reserve_jit_regs(&mut self) {
        // The register file must NEVER reallocate while a native JIT frame holds
        // a raw pointer into it (the caller's window pointer lives in a callee-
        // saved register across the self-call helper). A realloc there dangles
        // it → memory corruption. So reserve the absolute worst case up front:
        // every possible frame (`MAX_FRAMES`) holding the largest window. Then
        // no growth site can ever exceed capacity (each is also guarded), so the
        // Vec is pinned for the VM's lifetime.
        //
        // Cost is bounded: capacity is clamped so the reserve can't exceed
        // ~256 MiB even for a pathological max_window; if the cap is hit, deep
        // recursion simply throws RangeError sooner (a `reg_capacity` field
        // records the real limit so the growth guards agree).
        let max_window = self
            .program
            .functions
            .iter()
            .map(|f| (f.reg_count as usize).max(1))
            .max()
            .unwrap_or(1);
        const MAX_REGS_BYTES: usize = 256 * 1024 * 1024; // 256 MiB ceiling
        let worst_case = max_window.saturating_mul(MAX_FRAMES);
        let capped = worst_case.min(MAX_REGS_BYTES / std::mem::size_of::<Value>());
        let target = self.regs.len() + capped;
        self.regs.reserve(target - self.regs.len());
        // Record the pinned capacity: growth sites must not exceed it (else the
        // Vec would realloc). Use the ACTUAL capacity Rust gave us (≥ requested).
        self.reg_capacity = self.regs.capacity();
    }

    /// Allocate a string on the heap and return its boxed Value.
    pub fn alloc_str(&mut self, s: String) -> Value {
        Value::heap(self.heap.alloc_str(s))
    }

    /// Run the top-level function (id 0) to completion.
    pub fn run(&mut self) -> Result<Value, Thrown> {
        // Inject the built-in global objects (Object/Array/Function + their
        // prototypes) into their reserved slots BEFORE hoisting, so a user
        // declaration of the same name shadows the builtin.
        self.setup_globals();
        // Materialise function objects for every top-level function into the
        // globals that the compiler reserved for them. The compiler records,
        // per function, the global slot its name binds to (or u32::MAX if it is
        // an anonymous/nested function not hoisted to a global).
        self.hoist_functions();

        let top = &self.program.functions[0];
        let base = 0usize;
        let top_regs = top.reg_count as usize;
        self.regs.resize(top_regs, Value::UNDEFINED);
        // Reserve register-file capacity up front so JIT self-recursion can
        // append callee windows without reallocating `self.regs` (which would
        // dangle the native code's window pointer). Must happen while regs holds
        // only the top frame so the reservation math is relative to a known base.
        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
        self.reserve_jit_regs();
        self.frames.push(Frame { func: 0, base, ip: 0, ret_dst: 0, closure: NO_CLOSURE, handlers: Vec::new() });
        // Run until the top-level frame returns (frames drains back to 0), then
        // run the event loop: drain queued microtasks (promise reactions, async
        // resumes) to empty. Drains even on a main throw (matches node ordering),
        // then returns the original result.
        let main = self.run_loop(0);
        self.drain_microtasks();
        main
    }

    /// Invoke a callable `Value` with `this` and `args`, running it to
    /// completion, and return its result. Used by builtin methods that take
    /// callbacks (`map`/`filter`/`reduce`/`sort`). The callee executes on the
    /// explicit frame stack like any other call; we run a nested dispatch loop
    /// that returns when the callee's frame pops back to the current depth.
    ///
    /// Note: this re-enters `run_loop` on the native stack, so deeply *nested
    /// callbacks* use native recursion. Ordinary JS recursion (a function
    /// calling itself) does NOT — it stays on the frame stack. The frame cap
    /// still bounds total depth.
    fn call_value(&mut self, callee: Value, this: Value, args: &[Value]) -> Result<Value, Thrown> {
        // A callable Proxy: `apply` trap (or call the target).
        if callee.is_heap() {
            if let Some((target, handler, revoked)) = self.proxy_parts(callee.heap_index()) {
                if revoked {
                    return Err(Thrown("TypeError: Cannot perform 'apply' on a revoked proxy".into()));
                }
                return match self.proxy_trap(handler, "apply")? {
                    Some(trap) => {
                        let arr = Value::heap(self.heap.alloc(HeapObj::Array(args.to_vec())));
                        self.call_value(trap, handler, &[target, this, arr])
                    }
                    None => self.call_value(target, this, args),
                };
            }
        }
        // A bound function: invoke its target with the fixed `this` and the bound
        // arguments prepended (handles bind-of-bind by recursing).
        if callee.is_heap() {
            if let HeapObj::Bound { target, this: bthis, args: bargs } = self.heap.get(callee.heap_index()) {
                let (t, th) = (*target, *bthis);
                let mut all = bargs.clone();
                all.extend_from_slice(args);
                return self.call_value(t, th, &all);
            }
            if let HeapObj::Native(id) = self.heap.get(callee.heap_index()) {
                let id = *id;
                return self.call_native(id, this, args);
            }
        }
        // A native resolve/reject function settles its bound promise.
        if callee.is_heap() {
            if let HeapObj::BoundResolver { promise, is_reject } = self.heap.get(callee.heap_index()) {
                let (p, isr) = (*promise, *is_reject);
                let arg = args.first().copied().unwrap_or(Value::UNDEFINED);
                if isr {
                    self.reject(p, arg);
                } else {
                    self.resolve(p, arg);
                }
                return Ok(Value::UNDEFINED);
            }
        }
        // %Function.prototype% is itself a callable that returns undefined.
        if callee.is_heap() && self.fn_proto != 0 && callee.heap_index() == self.fn_proto {
            return Ok(Value::UNDEFINED);
        }
        let (func_id, closure) = self.resolve_callable(callee)?;
        let (is_gen, is_async) = {
            let p = &self.program.functions[func_id as usize];
            (p.is_generator, p.is_async)
        };
        // An `async function*` builds a suspended AsyncGenerator (an async
        // iterator); it doesn't run until `.next()`.
        if is_gen && is_async {
            return Ok(self.alloc_async_generator(func_id, closure, this, args));
        }
        // Calling a generator function builds a suspended Generator, not a frame.
        if is_gen {
            return Ok(self.alloc_generator(func_id, closure, this, args));
        }
        // Calling an async function runs synchronously up to the first `await`,
        // then returns its result Promise.
        if is_async {
            return Ok(self.alloc_async(func_id, closure, this, args));
        }
        if self.frames.len() >= MAX_FRAMES {
            return Err(Thrown("RangeError: Maximum call stack size exceeded".into()));
        }
        let proto = &self.program.functions[func_id as usize];
        let callee_regs = (proto.reg_count as usize).max(1);
        let callee_params = proto.param_count as usize;

        let new_base = self.regs.len();
        // Never grow past the pinned capacity (would realloc and dangle a live
        // native window pointer) — throw a catchable RangeError instead.
        if self.regs_would_overflow(new_base + callee_regs) {
            return Err(Thrown("RangeError: Maximum call stack size exceeded".into()));
        }
        self.regs.resize(new_base + callee_regs, Value::UNDEFINED);
        self.regs[new_base] = this; // reg 0 = this
        let n = args.len().min(callee_params);
        for i in 0..n {
            self.regs[new_base + 1 + i] = args[i];
        }
        // Rest parameter: gather any args beyond the fixed params into an array.
        if let Some(rreg) = proto.rest_reg {
            let extra: Vec<Value> = args.get(callee_params..).unwrap_or(&[]).to_vec();
            let arr = Value::heap(self.heap.alloc(HeapObj::Array(extra)));
            self.regs[new_base + rreg as usize] = arr;
        }

        let stop_depth = self.frames.len();
        self.frames.push(Frame { func: func_id, base: new_base, ip: 0, ret_dst: 0, closure, handlers: Vec::new() });
        self.run_loop(stop_depth)
    }

    /// Bind each named top-level function to its reserved global slot as a
    /// heap function object, so `Call` of a global resolves correctly. The
    /// compiler marks function-name globals; here we fill them.
    fn hoist_functions(&mut self) {
        for (id, f) in self.program.functions.iter().enumerate() {
            if let Some(slot) = function_global_slot(f) {
                let v = Value::heap(self.heap.alloc(HeapObj::Func(id as u32)));
                if (slot as usize) < self.globals.len() {
                    self.globals[slot as usize] = v;
                }
            }
        }
    }

    /// Drives execution from the current frame until the frame that was current
    /// on entry returns (frames drops to `stop_depth`), catching thrown values
    /// at `try` handlers along the way. `run()` passes 0 (drain everything);
    /// `call_value` passes the pre-call depth (run one nested call).
    ///
    /// On a throw, [`Self::dispatch_body`] returns `Err`; we look up the thrown
    /// value and unwind to the nearest handler at or above `stop_depth`. If one
    /// exists, execution resumes at its catch target; otherwise the throw
    /// propagates out (with `pending_throw` left set so an enclosing `run_loop`
    /// — e.g. the caller of a builtin callback — can still catch it).
    fn run_loop(&mut self, stop_depth: usize) -> Result<Value, Thrown> {
        loop {
            match self.dispatch_body(stop_depth) {
                Ok(v) => return Ok(v),
                Err(t) => {
                    let tv = match self.pending_throw {
                        Some(v) => v,
                        None => {
                            // Internal error (TypeError/RangeError/…) with no
                            // explicit thrown value: synthesise a real Error
                            // object so `catch (e)` sees `e.name`/`e.message` and
                            // `e instanceof TypeError`, matching JS.
                            let v = self.alloc_error_from_message(&t.0);
                            self.pending_throw = Some(v);
                            v
                        }
                    };
                    if self.unwind_to_handler(tv, stop_depth) {
                        self.pending_throw = None; // caught — resume at catch
                        continue;
                    }
                    // Uncaught here; propagate. If the carried message is empty
                    // (e.g. a JIT-bail unwind that signalled via pending_throw
                    // with no text), recompute it from the thrown value so the
                    // top-level report shows the real error, not "".
                    if t.0.is_empty() {
                        return Err(Thrown(self.throw_message(tv)));
                    }
                    return Err(t); // pending_throw stays set for an outer catch
                }
            }
        }
    }

    /// Pop frames from the top down to (but not below) `stop_depth`, looking for
    /// a `try` handler. A `Catch` deposits `tv` in its register and resumes at the
    /// catch target. A `Finally` deposits a throw completion (kind 2 + the reason)
    /// into its registers and resumes at the finally target — `EndFinally`
    /// re-throws after the finally runs. Either way execution resumes (`true`). If
    /// the boundary is reached with no handler, return `false` (propagate).
    fn unwind_to_handler(&mut self, tv: Value, stop_depth: usize) -> bool {
        while self.frames.len() > stop_depth {
            let top = self.frames.len() - 1;
            if let Some(h) = self.frames[top].handlers.pop() {
                let base = self.frames[top].base;
                match h {
                    Handler::Catch { target, reg } => {
                        self.regs[base + reg as usize] = tv;
                        self.frames[top].ip = target as usize;
                    }
                    Handler::Finally { target, kind_reg, val_reg } => {
                        self.regs[base + kind_reg as usize] = Value::int(2); // throw
                        self.regs[base + val_reg as usize] = tv;
                        self.frames[top].ip = target as usize;
                    }
                }
                return true;
            }
            // No handler in this frame: discard it and its register window.
            let f = self.frames.pop().unwrap();
            self.regs.truncate(f.base);
        }
        false
    }

    /// On a non-throw leave of the top frame (`return`, and later break/continue),
    /// run any pending `finally` first. Discards `Catch` handlers we are exiting;
    /// on the innermost `Finally`, deposits the completion (`kind` 1=return + the
    /// `value`) into its registers and returns its target so the caller resumes
    /// there (`EndFinally` later re-leaves). Returns `None` when no finally is
    /// pending — the caller performs the real leave (pop the frame).
    fn route_through_finally(&mut self, kind: i32, value: Value) -> Option<u32> {
        let top = self.frames.len() - 1;
        let base = self.frames[top].base;
        while let Some(h) = self.frames[top].handlers.last().copied() {
            match h {
                Handler::Finally { target, kind_reg, val_reg } => {
                    self.frames[top].handlers.pop();
                    self.regs[base + kind_reg as usize] = Value::int(kind);
                    self.regs[base + val_reg as usize] = value;
                    return Some(target);
                }
                Handler::Catch { .. } => {
                    self.frames[top].handlers.pop();
                }
            }
        }
        None
    }

    /// The inner execution loop: runs ops in the current frame until a frame
    /// transition (a call pushes / a return pops) or a throw. Returns the value
    /// when the `stop_depth` frame returns, or `Err` to begin unwinding.
    fn dispatch_body(&mut self, stop_depth: usize) -> Result<Value, Thrown> {
        loop {
            // Snapshot the current frame's coordinates. `ip` is advanced as a
            // local and written back only on frame transitions / loops.
            let frame_idx = self.frames.len() - 1;
            let func_id = self.frames[frame_idx].func;
            let base = self.frames[frame_idx].base;
            let mut ip = self.frames[frame_idx].ip;
            let cur_closure = self.frames[frame_idx].closure;
            let code: *const Vec<Instr> = &self.program.functions[func_id as usize].code;
            // SAFETY: `code` borrows immutable program data that outlives the
            // loop; we never mutate program functions during execution.
            let code: &Vec<Instr> = unsafe { &*code };

            // ── JIT tier ──
            // On fresh frame entry (ip == 0), if this function has compiled
            // native code, run it over the frame's register window. The native
            // code shares `self.regs`, so on a bail the interpreter resumes with
            // consistent state. Only entered at ip==0: a bail sets `ip` to the
            // resume point and falls into the interpreter for the rest of this
            // activation (never re-enters native mid-function). We also count
            // entries here and compile on crossing the threshold.
            // Only enter native code from a NON-recursive interpreter context
            // (`jit_recurse_depth == 0`). Once a native self-call has deopted and
            // we're finishing it on the interpreter, re-entering the JIT for the
            // continuation would livelock: native recurses 256, deopts, the
            // interpreter re-enters native, recurses 256, deopts… forever,
            // because the per-call native depth counter resets each return and
            // interpreter frames never reach MAX_FRAMES. Staying interpreted in
            // that subtree lets frames accumulate monotonically → RangeError.
            #[cfg(all(feature = "jit", target_arch = "x86_64"))]
            if ip == 0
                && self.jit_enabled
                && self.jit_recurse_depth == 0
                && !self.program.functions[func_id as usize].is_generator
                && !self.program.functions[func_id as usize].is_async
            {
                if let Some((result, bail)) = self.try_run_jit(func_id, base) {
                    if bail == crate::codegen::NO_BAIL {
                        // Native code returned: behave like a `Return`.
                        if self.pop_frame_with(result, stop_depth) {
                            return Ok(result);
                        }
                        continue; // re-enter outer loop with caller frame
                    }
                    // A bail can mean two things:
                    // (a) a normal deopt (non-int operand, overflow): resume the
                    //     interpreter at the recorded ip with consistent regs.
                    // (b) a self-recursive call threw (e.g. RangeError) and the
                    //     helper signalled deopt with `pending_throw` set — the
                    //     whole native chain must UNWIND, not resume. Detect (b)
                    //     by the pending throw and return Err so `run_loop`
                    //     dispatches it to the nearest handler / propagates it.
                    if self.pending_throw.is_some() {
                        // Persist ip for coherence, then unwind. The message is
                        // recomputed by run_loop from pending_throw.
                        let top = self.frames.len() - 1;
                        self.frames[top].ip = bail as usize;
                        return Err(Thrown(String::new()));
                    }
                    // (a): resume the interpreter at the recorded ip.
                    ip = bail as usize;
                } else if self.jit.record_and_should_compile(func_id) {
                    let proto: *const crate::bytecode::FuncProto =
                        &self.program.functions[func_id as usize];
                    // SAFETY: program functions are immutable during execution.
                    let proto_ref = unsafe { &*proto };
                    // The self-function's current global Value (a heap Func),
                    // stable since hoist_functions ran at startup. Embedded so a
                    // JIT'd `LoadGlobal(self_slot)` stores the REAL function (not
                    // a placeholder) — required for a deopted self-Call to
                    // resolve the callee correctly in the interpreter.
                    let self_val = proto_ref
                        .name_global
                        .and_then(|s| self.globals.get(s as usize).copied())
                        .unwrap_or(Value::UNDEFINED)
                        .bits();
                    self.jit.compile(
                        func_id,
                        proto_ref,
                        jit_self_call_at as usize,
                        self_val,
                    );
                }
            }

            // Inner loop: execute within the current frame until a call pushes
            // a new frame or a return pops this one.
            loop {
                let instr = &code[ip];
                match *instr {
                    Instr::LoadConst { dst, idx } => {
                        let v = self.program.functions[func_id as usize].constants[idx as usize];
                        // String constants are stored with a sentinel; resolve
                        // to a freshly-interned heap string the first time.
                        let resolved = self.resolve_const(func_id, v);
                        self.set(base, dst, resolved);
                        ip += 1;
                    }
                    Instr::LoadInt { dst, val } => {
                        self.set(base, dst, Value::int(val));
                        ip += 1;
                    }
                    Instr::LoadUndefined { dst } => {
                        self.set(base, dst, Value::UNDEFINED);
                        ip += 1;
                    }
                    Instr::LoadNull { dst } => {
                        self.set(base, dst, Value::NULL);
                        ip += 1;
                    }
                    Instr::LoadBool { dst, val } => {
                        self.set(base, dst, Value::bool(val));
                        ip += 1;
                    }
                    Instr::Move { dst, src } => {
                        let v = self.get(base, src);
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::LoadGlobal { dst, idx } => {
                        let v = self.globals[idx as usize];
                        if v.is_uninitialized() {
                            // Referenced but never declared → ReferenceError.
                            let name = self
                                .program
                                .global_names
                                .get(idx as usize)
                                .map(|s| s.as_str())
                                .unwrap_or("?");
                            return Err(Thrown(format!("ReferenceError: {name} is not defined")));
                        }
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::LoadGlobalOrUndefined { dst, idx } => {
                        let v = self.globals[idx as usize];
                        let v = if v.is_uninitialized() { Value::UNDEFINED } else { v };
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::StoreGlobal { idx, src } => {
                        let v = self.get(base, src);
                        self.globals[idx as usize] = v;
                        ip += 1;
                    }
                    Instr::Now { dst, epoch } => {
                        let ms = if epoch {
                            // Date.now(): integer ms since the Unix epoch.
                            std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_millis() as f64)
                                .unwrap_or(0.0)
                        } else {
                            // performance.now(): fractional ms since VM start.
                            self.start.elapsed().as_secs_f64() * 1000.0
                        };
                        self.set(base, dst, Value::num(ms));
                        ip += 1;
                    }

                    Instr::Add { dst, a, b } => {
                        let r = self.add(base, a, b)?;
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    // Identical to `Add` — a JIT routing hint only (see bytecode).
                    Instr::StrConcat { dst, a, b } => {
                        let r = self.add(base, a, b)?;
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    // In-place string append (emitter proved `a` uniquely owned).
                    Instr::StrAppendInPlace { dst, a, b } => {
                        let av = self.get(base, a);
                        let bv = self.get(base, b);
                        let r = self.str_append_inplace(av, bv);
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::Sub { dst, a, b } => {
                        let va = self.get(base, a);
                        let vb = self.get(base, b);
                        let r = if va.is_int() && vb.is_int() {
                            match va.as_int().checked_sub(vb.as_int()) {
                                Some(v) => Value::int(v),
                                None => Value::num(va.as_int() as f64 - vb.as_int() as f64),
                            }
                        } else if let Some(bv) = self.bigint_binop(BigOp::Sub, va, vb)? {
                            bv
                        } else {
                            Value::num(self.to_number(va)? - self.to_number(vb)?)
                        };
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::Mul { dst, a, b } => {
                        let va = self.get(base, a);
                        let vb = self.get(base, b);
                        let r = if va.is_int() && vb.is_int() {
                            match va.as_int().checked_mul(vb.as_int()) {
                                Some(v) => Value::int(v),
                                None => Value::num(va.as_int() as f64 * vb.as_int() as f64),
                            }
                        } else if let Some(bv) = self.bigint_binop(BigOp::Mul, va, vb)? {
                            bv
                        } else {
                            Value::num(self.to_number(va)? * self.to_number(vb)?)
                        };
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::Div { dst, a, b } => {
                        let va = self.get(base, a);
                        let vb = self.get(base, b);
                        let r = if let Some(bv) = self.bigint_binop(BigOp::Div, va, vb)? {
                            bv
                        } else {
                            Value::num(self.to_number(va)? / self.to_number(vb)?)
                        };
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::Mod { dst, a, b } => {
                        let va = self.get(base, a);
                        let vb = self.get(base, b);
                        let r = if let Some(bv) = self.bigint_binop(BigOp::Mod, va, vb)? {
                            bv
                        } else {
                            Value::num(self.to_number(va)? % self.to_number(vb)?)
                        };
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::ToNum { dst, a } => {
                        let va = self.get(base, a);
                        // `+x`: numbers pass through (keep Int tag); `+bigint` throws
                        // (unary plus is not defined on BigInt); else ToNumber.
                        let r = if va.is_number() {
                            va
                        } else if self.bigint_value(va).is_some() {
                            return Err(Thrown("TypeError: Cannot convert a BigInt value to a number".into()));
                        } else {
                            Value::num(self.to_number(va)?)
                        };
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::Neg { dst, a } => {
                        let va = self.get(base, a);
                        let r = if va.is_int() {
                            match va.as_int().checked_neg() {
                                Some(v) => Value::int(v),
                                None => Value::num(-(va.as_int() as f64)),
                            }
                        } else if let Some(n) = self.bigint_value(va) {
                            self.make_bigint(n.wrapping_neg())
                        } else {
                            Value::num(-self.to_number(va)?)
                        };
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::Bitwise { dst, a, b, op } => {
                        use crate::bytecode::BitwiseOp as B;
                        let va = self.get(base, a);
                        let vb = self.get(base, b);
                        // BigInt bitwise: &/|/^/<</>> on two BigInts; `>>>` is not
                        // defined for BigInt (TypeError); mixing → TypeError.
                        if self.bigint_value(va).is_some() || self.bigint_value(vb).is_some() {
                            let bop = match op {
                                B::And => BigOp::And,
                                B::Or => BigOp::Or,
                                B::Xor => BigOp::Xor,
                                B::Shl => BigOp::Shl,
                                B::Shr => BigOp::Shr,
                                B::Ushr => {
                                    return Err(Thrown(
                                        "TypeError: BigInts have no unsigned right shift, use >> instead"
                                            .into(),
                                    ))
                                }
                            };
                            if let Some(bv) = self.bigint_binop(bop, va, vb)? {
                                self.set(base, dst, bv);
                                ip += 1;
                                continue;
                            }
                        }
                        let x = to_int32(self.to_number(va)?);
                        // Shift counts use the low 5 bits per the JS spec.
                        let r = match op {
                            B::And => Value::int(x & to_int32(self.to_number(vb)?)),
                            B::Or => Value::int(x | to_int32(self.to_number(vb)?)),
                            B::Xor => Value::int(x ^ to_int32(self.to_number(vb)?)),
                            B::Shl => {
                                let s = to_uint32(self.to_number(vb)?) & 31;
                                Value::int(x.wrapping_shl(s))
                            }
                            B::Shr => {
                                let s = to_uint32(self.to_number(vb)?) & 31;
                                Value::int(x >> s)
                            }
                            B::Ushr => {
                                let s = to_uint32(self.to_number(vb)?) & 31;
                                let u = to_uint32(self.to_number(va)?) >> s;
                                // u32 may exceed i32::MAX → keep numeric range.
                                if u <= i32::MAX as u32 {
                                    Value::int(u as i32)
                                } else {
                                    Value::num(u as f64)
                                }
                            }
                        };
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::Pow { dst, a, b } => {
                        let va = self.get(base, a);
                        let vb = self.get(base, b);
                        let r = if let Some(bv) = self.bigint_binop(BigOp::Pow, va, vb)? {
                            bv
                        } else {
                            Value::num(self.to_number(va)?.powf(self.to_number(vb)?))
                        };
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::BitNot { dst, a } => {
                        let va = self.get(base, a);
                        if let Some(n) = self.bigint_value(va) {
                            let r = self.make_bigint(!n);
                            self.set(base, dst, r);
                        } else {
                            let r = !to_int32(self.to_number(va)?);
                            self.set(base, dst, Value::int(r));
                        }
                        ip += 1;
                    }
                    Instr::AddInt { dst, a, imm } => {
                        let va = self.get(base, a);
                        let r = if va.is_int() {
                            match va.as_int().checked_add(imm) {
                                Some(v) => Value::int(v),
                                None => Value::num(va.as_int() as f64 + imm as f64),
                            }
                        } else {
                            Value::num(self.to_number(va)? + imm as f64)
                        };
                        self.set(base, dst, r);
                        ip += 1;
                    }

                    Instr::Lt { dst, a, b } => {
                        let r = self.cmp_lt(base, a, b)?;
                        self.set(base, dst, Value::bool(r));
                        ip += 1;
                    }
                    Instr::Le { dst, a, b } => {
                        let r = self.cmp_le(base, a, b)?;
                        self.set(base, dst, Value::bool(r));
                        ip += 1;
                    }
                    Instr::Gt { dst, a, b } => {
                        let r = self.cmp_lt(base, b, a)?;
                        self.set(base, dst, Value::bool(r));
                        ip += 1;
                    }
                    Instr::Ge { dst, a, b } => {
                        let r = self.cmp_le(base, b, a)?;
                        self.set(base, dst, Value::bool(r));
                        ip += 1;
                    }
                    Instr::LooseEq { dst, a, b } => {
                        let va = self.get(base, a);
                        let vb = self.get(base, b);
                        let r = self.loose_eq(va, vb)?;
                        self.set(base, dst, Value::bool(r));
                        ip += 1;
                    }
                    Instr::LooseNe { dst, a, b } => {
                        let va = self.get(base, a);
                        let vb = self.get(base, b);
                        let r = self.loose_eq(va, vb)?;
                        self.set(base, dst, Value::bool(!r));
                        ip += 1;
                    }
                    Instr::Eq { dst, a, b } => {
                        let r = self.strict_eq(base, a, b);
                        self.set(base, dst, Value::bool(r));
                        ip += 1;
                    }
                    Instr::Ne { dst, a, b } => {
                        let r = self.strict_eq(base, a, b);
                        self.set(base, dst, Value::bool(!r));
                        ip += 1;
                    }
                    Instr::Not { dst, a } => {
                        let va = self.get(base, a);
                        let t = self.truthy(va);
                        self.set(base, dst, Value::bool(!t));
                        ip += 1;
                    }
                    Instr::TypeOf { dst, a } => {
                        let va = self.get(base, a);
                        let t = self.type_of(va);
                        let v = self.alloc_str(t.to_string());
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::IsArray { dst, a } => {
                        let v = self.get(base, a);
                        let is_arr = v.is_heap()
                            && matches!(self.heap.get(v.heap_index()), HeapObj::Array(_));
                        self.set(base, dst, Value::bool(is_arr));
                        ip += 1;
                    }
                    Instr::JsonStringify { dst, val, space } => {
                        let v = self.get(base, val);
                        let indent = self.json_indent(self.get(base, space));
                        // `JSON.stringify(undefined)` (and of a function) is undefined.
                        let result = match self.json_value(v, &indent, 0) {
                            Some(s) => self.alloc_str(s),
                            None => Value::UNDEFINED,
                        };
                        self.set(base, dst, result);
                        ip += 1;
                    }
                    Instr::JsonParse { dst, a } => {
                        let s = self.display(self.get(base, a)); // ToString of the arg
                        let v = self.json_parse(&s)?; // propagates SyntaxError as a throw
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::ArrayAppend { arr, val, spread } => {
                        let aidx = self.get(base, arr).heap_index();
                        let vv = self.get(base, val);
                        if spread {
                            // A generator or a custom iterable (object) is drained
                            // via the iterator protocol (iterate_to_vec also errors
                            // for a plain, non-iterable object, as a spread should).
                            if vv.is_heap()
                                && matches!(
                                    self.heap.get(vv.heap_index()),
                                    HeapObj::Generator { .. }
                                        | HeapObj::Object(_)
                                        | HeapObj::Iterator { .. }
                                        | HeapObj::TypedArray { .. }
                                )
                            {
                                let elems = self.iterate_to_vec(vv)?;
                                if let HeapObj::Array(dst_items) = self.heap.get_mut(aidx) {
                                    dst_items.extend(elems);
                                }
                                ip += 1;
                                continue;
                            }
                            // Materialize the spread source's elements (array/set →
                            // elements; string → chars; map → [k,v] entries) WITHOUT
                            // holding a heap borrow across the fresh allocations.
                            let mut chars: Option<Vec<char>> = None;
                            let mut map_pairs: Option<Vec<(Value, Value)>> = None;
                            if vv.is_heap() {
                                match self.heap.get(vv.heap_index()) {
                                    HeapObj::Array(items) => {
                                        let elems = items.clone();
                                        if let HeapObj::Array(d) = self.heap.get_mut(aidx) {
                                            d.extend(elems);
                                        }
                                    }
                                    HeapObj::Set(items) => {
                                        let elems = items.clone();
                                        if let HeapObj::Array(d) = self.heap.get_mut(aidx) {
                                            d.extend(elems);
                                        }
                                    }
                                    HeapObj::Str(_) | HeapObj::Cons { .. } => {
                                        chars = Some(self.heap.str_cow(vv.heap_index()).unwrap().chars().collect());
                                    }
                                    HeapObj::Map { keys, vals } => {
                                        map_pairs = Some(keys.iter().copied().zip(vals.iter().copied()).collect());
                                    }
                                    _ => return Err(Thrown("TypeError: spread value is not iterable".into())),
                                }
                            } else {
                                return Err(Thrown("TypeError: spread value is not iterable".into()));
                            }
                            if let Some(chars) = chars {
                                let elems: Vec<Value> =
                                    chars.into_iter().map(|c| self.alloc_str(c.to_string())).collect();
                                if let HeapObj::Array(dst_items) = self.heap.get_mut(aidx) {
                                    dst_items.extend(elems);
                                }
                            }
                            if let Some(pairs) = map_pairs {
                                let elems: Vec<Value> = pairs
                                    .into_iter()
                                    .map(|(k, v)| Value::heap(self.heap.alloc(HeapObj::Array(vec![k, v]))))
                                    .collect();
                                if let HeapObj::Array(dst_items) = self.heap.get_mut(aidx) {
                                    dst_items.extend(elems);
                                }
                            }
                        } else if let HeapObj::Array(dst_items) = self.heap.get_mut(aidx) {
                            dst_items.push(vv);
                        }
                        ip += 1;
                    }
                    Instr::ArrayRest { dst, src, start } => {
                        let sv = self.get(base, src);
                        let mut elems = self.iterate_to_vec(sv)?;
                        let start = (start as usize).min(elems.len());
                        let rest = elems.split_off(start);
                        let arr = Value::heap(self.heap.alloc(HeapObj::Array(rest)));
                        self.set(base, dst, arr);
                        ip += 1;
                    }
                    Instr::ObjectSpread { target, src } => {
                        let t = self.get(base, target);
                        let s = self.get(base, src);
                        self.object_assign(&[t, s])?; // mutates target in place
                        ip += 1;
                    }
                    Instr::ObjectRest { dst, src, exclude_start, exclude_count } => {
                        let s = self.get(base, src);
                        let prog: &'p Program = self.program;
                        let consts = &prog.functions[func_id as usize].string_constants;
                        let excluded =
                            &consts[exclude_start as usize..exclude_start as usize + exclude_count as usize];
                        // Copy src's own keys except the destructured siblings.
                        let pairs: Vec<(String, Value)> = if s.is_heap() {
                            match self.heap.get(s.heap_index()) {
                                HeapObj::Object(map) => map
                                    .keys
                                    .iter()
                                    .cloned()
                                    .zip(map.vals.iter().copied())
                                    .filter(|(k, _)| !excluded.iter().any(|e| e == k))
                                    .collect(),
                                _ => Vec::new(),
                            }
                        } else {
                            Vec::new()
                        };
                        let mut m = ObjMap::new();
                        for (k, v) in pairs {
                            m.set(&k, v);
                        }
                        let v = Value::heap(self.heap.alloc(HeapObj::Object(m)));
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::MakeClass { dst, class_id, parent } => {
                        let cd = self.program.classes[class_id as usize].clone();
                        let parent_idx = parent.and_then(|p| {
                            let pv = self.get(base, p);
                            pv.is_heap().then(|| pv.heap_index())
                        });
                        // Materialize each method as a Func value once; instances
                        // share these (no per-access alloc, no per-instance copy).
                        let mk = |heap: &mut Heap, defs: &[(String, u32)]| -> Vec<(String, Value)> {
                            defs.iter()
                                .map(|(n, fid)| {
                                    (n.clone(), Value::heap(heap.alloc(HeapObj::Func(*fid))))
                                })
                                .collect()
                        };
                        let methods = mk(&mut self.heap, &cd.methods);
                        let getters = mk(&mut self.heap, &cd.getters);
                        let setters = mk(&mut self.heap, &cd.setters);
                        let static_getters = mk(&mut self.heap, &cd.static_getters);
                        let static_setters = mk(&mut self.heap, &cd.static_setters);
                        let mut statics = ObjMap::new();
                        // Static methods are non-enumerable (writable + configurable),
                        // like instance methods. Static *fields* are added later via
                        // SetProp and stay enumerable, as ES requires.
                        let method_attr = PropAttr {
                            writable: true,
                            enumerable: false,
                            configurable: true,
                            accessor: false,
                            setter: Value::UNDEFINED,
                        };
                        for (n, fid) in &cd.statics {
                            let fv = Value::heap(self.heap.alloc(HeapObj::Func(*fid)));
                            statics.define(n, fv, method_attr);
                        }
                        let v = Value::heap(self.heap.alloc(HeapObj::Class(Box::new(ClassData {
                            name: cd.name,
                            ctor: cd.ctor,
                            has_explicit_ctor: cd.has_explicit_ctor,
                            methods,
                            getters,
                            setters,
                            statics,
                            static_getters,
                            static_setters,
                            parent: parent_idx,
                            computed_field_keys: Vec::new(),
                        }))));
                        // Remember it so `super` in a derived class can reach it.
                        self.class_values[class_id as usize] = Some(v);
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::ClassAddMember { class, key, func, kind } => {
                        let cv = self.get(base, class);
                        let k = self.get(base, key);
                        let kstr = self.display(k);
                        let fv = Value::heap(self.heap.alloc(HeapObj::Func(func)));
                        if let HeapObj::Class(c) = self.heap.get_mut(cv.heap_index()) {
                            if kind == 3 {
                                // Static method — non-enumerable (like a named one).
                                let attr = PropAttr {
                                    writable: true,
                                    enumerable: false,
                                    configurable: true,
                                    accessor: false,
                                    setter: Value::UNDEFINED,
                                };
                                c.statics.define(&kstr, fv, attr);
                            } else {
                                // kind: 1=getter 2=setter 4=static getter 5=static
                                // setter, else instance method.
                                let list = match kind {
                                    1 => &mut c.getters,
                                    2 => &mut c.setters,
                                    4 => &mut c.static_getters,
                                    5 => &mut c.static_setters,
                                    _ => &mut c.methods,
                                };
                                // Replace a same-key member, else append.
                                if let Some(slot) = list.iter_mut().find(|(n, _)| *n == kstr) {
                                    slot.1 = fv;
                                } else {
                                    list.push((kstr, fv));
                                }
                            }
                        }
                        ip += 1;
                    }
                    Instr::New { dst, callee, arg_base, argc } => {
                        let cv = self.get(base, callee);
                        let mut args: Vec<Value> = Vec::with_capacity(argc as usize);
                        for i in 0..argc {
                            args.push(self.get(base, arg_base + i));
                        }
                        let result = self.construct(cv, &args)?;
                        self.set(base, dst, result);
                        ip += 1;
                    }
                    Instr::NewSpread { dst, callee, args } => {
                        let cv = self.get(base, callee);
                        let args_v = self.get(base, args);
                        let arg_vec = self.array_snapshot(args_v.heap_index());
                        let result = self.construct(cv, &arg_vec)?;
                        self.set(base, dst, result);
                        ip += 1;
                    }
                    Instr::PushFieldKey { class, key } => {
                        let cv = self.get(base, class);
                        let kv = self.get(base, key);
                        if let HeapObj::Class(c) = self.heap.get_mut(cv.heap_index()) {
                            c.computed_field_keys.push(kv);
                        }
                        ip += 1;
                    }
                    Instr::FieldInit { key_index, val } => {
                        let this = self.get(base, 0);
                        let v = self.get(base, val);
                        // The computed key was evaluated once at class definition and
                        // stored on this instance's class.
                        let key = match self.heap.get(this.heap_index()) {
                            HeapObj::Object(m) => m.class.and_then(|cidx| {
                                match self.heap.get(cidx) {
                                    HeapObj::Class(c) => c.computed_field_keys.get(key_index as usize).copied(),
                                    _ => None,
                                }
                            }),
                            _ => None,
                        };
                        if let Some(key) = key {
                            self.set_index(this, key, v)?;
                        }
                        ip += 1;
                    }
                    Instr::SuperCtor { home_class_id, arg_base, argc } => {
                        let parent = self.super_parent(home_class_id)
                            .ok_or_else(|| Thrown("TypeError: superclass is not a constructor".into()))?;
                        let this = self.get(base, 0);
                        let mut args: Vec<Value> = Vec::with_capacity(argc as usize);
                        for i in 0..argc {
                            args.push(self.get(base, arg_base + i));
                        }
                        self.run_class_ctor(parent, this, &args)?;
                        ip += 1;
                    }
                    Instr::SuperCtorSpread { home_class_id, args } => {
                        let parent = self.super_parent(home_class_id)
                            .ok_or_else(|| Thrown("TypeError: superclass is not a constructor".into()))?;
                        let this = self.get(base, 0);
                        let args_v = self.get(base, args);
                        let arg_vec = self.array_snapshot(args_v.heap_index());
                        self.run_class_ctor(parent, this, &arg_vec)?;
                        ip += 1;
                    }
                    Instr::SuperMethod { dst, home_class_id, name, arg_base, argc } => {
                        let prog: &'p Program = self.program;
                        let key: &'p str =
                            &prog.functions[func_id as usize].string_constants[name as usize];
                        let parent = self.super_parent(home_class_id)
                            .ok_or_else(|| Thrown("TypeError: bad super reference".into()))?;
                        // Find the method up the parent's class chain.
                        let mut method = None;
                        let mut cur = parent.is_heap().then(|| parent.heap_index());
                        while let Some(cidx) = cur {
                            match self.heap.get(cidx) {
                                HeapObj::Class(c) => {
                                    if let Some((_, v)) = c.methods.iter().find(|(k, _)| k == key) {
                                        method = Some(*v);
                                        break;
                                    }
                                    cur = c.parent;
                                }
                                _ => break,
                            }
                        }
                        let m = method.ok_or_else(|| {
                            Thrown(format!("TypeError: super.{key} is not a function"))
                        })?;
                        let this = self.get(base, 0);
                        let mut args: Vec<Value> = Vec::with_capacity(argc as usize);
                        for i in 0..argc {
                            args.push(self.get(base, arg_base + i));
                        }
                        let r = self.call_value(m, this, &args)?;
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::ArrayCtor { dst, arg_base, argc } => {
                        let arr = if argc == 1 && self.get(base, arg_base).is_number() {
                            // `Array(n)` → n empty slots (undefined).
                            let n = self.get(base, arg_base).as_f64();
                            if n < 0.0 || n.fract() != 0.0 || n > u32::MAX as f64 {
                                return Err(Thrown("RangeError: Invalid array length".into()));
                            }
                            vec![Value::UNDEFINED; n as usize]
                        } else {
                            (0..argc).map(|i| self.get(base, arg_base + i)).collect()
                        };
                        let v = Value::heap(self.heap.alloc(HeapObj::Array(arr)));
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::NewMap { dst, src } => {
                        let (mut keys, mut vals): (Vec<Value>, Vec<Value>) = (Vec::new(), Vec::new());
                        if let Some(s) = src {
                            let sv = self.get(base, s);
                            if !sv.is_nullish() {
                                // Each iterated entry is a [key, value]-indexable.
                                for e in self.iterate_to_vec(sv)? {
                                    let k = normalize_zero(self.get_index(e, Value::int(0))?);
                                    let v = self.get_index(e, Value::int(1))?;
                                    match keys.iter().position(|kk| self.same_value_zero(*kk, k)) {
                                        Some(i) => vals[i] = v,
                                        None => {
                                            keys.push(k);
                                            vals.push(v);
                                        }
                                    }
                                }
                            }
                        }
                        let m = Value::heap(self.heap.alloc(HeapObj::Map { keys, vals }));
                        self.set(base, dst, m);
                        ip += 1;
                    }
                    Instr::NewSet { dst, src } => {
                        let mut items: Vec<Value> = Vec::new();
                        if let Some(s) = src {
                            let sv = self.get(base, s);
                            if !sv.is_nullish() {
                                for e in self.iterate_to_vec(sv)? {
                                    let v = normalize_zero(e);
                                    if !items.iter().any(|x| self.same_value_zero(*x, v)) {
                                        items.push(v);
                                    }
                                }
                            }
                        }
                        let s = Value::heap(self.heap.alloc(HeapObj::Set(items)));
                        self.set(base, dst, s);
                        ip += 1;
                    }
                    Instr::NewWeakMap { dst, src } => {
                        let (mut keys, mut vals): (Vec<Value>, Vec<Value>) = (Vec::new(), Vec::new());
                        if let Some(s) = src {
                            let sv = self.get(base, s);
                            if !sv.is_nullish() {
                                for e in self.iterate_to_vec(sv)? {
                                    let k = self.get_index(e, Value::int(0))?;
                                    let v = self.get_index(e, Value::int(1))?;
                                    if !self.is_object_value(k) {
                                        return Err(Thrown(
                                            "TypeError: Invalid value used as weak map key".into(),
                                        ));
                                    }
                                    match keys.iter().position(|kk| self.same_value_zero(*kk, k)) {
                                        Some(i) => vals[i] = v,
                                        None => {
                                            keys.push(k);
                                            vals.push(v);
                                        }
                                    }
                                }
                            }
                        }
                        let m = Value::heap(self.heap.alloc(HeapObj::WeakMap { keys, vals }));
                        self.set(base, dst, m);
                        ip += 1;
                    }
                    Instr::NewWeakSet { dst, src } => {
                        let mut items: Vec<Value> = Vec::new();
                        if let Some(s) = src {
                            let sv = self.get(base, s);
                            if !sv.is_nullish() {
                                for e in self.iterate_to_vec(sv)? {
                                    if !self.is_object_value(e) {
                                        return Err(Thrown(
                                            "TypeError: Invalid value used in weak set".into(),
                                        ));
                                    }
                                    if !items.iter().any(|x| self.same_value_zero(*x, e)) {
                                        items.push(e);
                                    }
                                }
                            }
                        }
                        let s = Value::heap(self.heap.alloc(HeapObj::WeakSet(items)));
                        self.set(base, dst, s);
                        ip += 1;
                    }
                    Instr::NewWeakRef { dst, target } => {
                        let t = self.get(base, target);
                        if !self.is_object_value(t) {
                            return Err(Thrown(
                                "TypeError: WeakRef: target must be an object".into(),
                            ));
                        }
                        let wr = Value::heap(self.heap.alloc(HeapObj::WeakRef(t)));
                        self.set(base, dst, wr);
                        ip += 1;
                    }
                    Instr::NewBox { dst, kind, arg } => {
                        let value = match kind {
                            0 => {
                                // String box: ToString(arg) (no arg -> "").
                                let s = match arg {
                                    Some(a) => self.to_js_string(self.get(base, a))?,
                                    None => String::new(),
                                };
                                self.alloc_str(s)
                            }
                            1 => {
                                // Number box: ToNumber(arg) (no arg -> +0).
                                let n = match arg {
                                    Some(a) => self.to_number(self.get(base, a))?,
                                    None => 0.0,
                                };
                                Value::num(n)
                            }
                            _ => {
                                // Boolean box: ToBoolean(arg) (no arg -> false).
                                Value::bool(arg.map(|a| self.truthy(self.get(base, a))).unwrap_or(false))
                            }
                        };
                        let b = Value::heap(self.heap.alloc(HeapObj::Boxed { kind, value }));
                        self.set(base, dst, b);
                        ip += 1;
                    }
                    Instr::NewFinalizationRegistry { dst, cleanup } => {
                        let cb = self.get(base, cleanup);
                        if self.type_of(cb) != "function" {
                            return Err(Thrown(
                                "TypeError: FinalizationRegistry: cleanup callback must be callable".into(),
                            ));
                        }
                        let fr = Value::heap(
                            self.heap.alloc(HeapObj::FinalizationRegistry { cleanup: cb, tokens: Vec::new() }),
                        );
                        self.set(base, dst, fr);
                        ip += 1;
                    }
                    Instr::NewPromise { dst, executor } => {
                        let exec = self.get(base, executor);
                        let p = self.alloc_promise();
                        let res = Value::heap(
                            self.heap.alloc(HeapObj::BoundResolver { promise: p, is_reject: false }),
                        );
                        let rej = Value::heap(
                            self.heap.alloc(HeapObj::BoundResolver { promise: p, is_reject: true }),
                        );
                        // A throwing executor rejects the promise.
                        if self.call_value(exec, Value::UNDEFINED, &[res, rej]).is_err() {
                            let reason = self.pending_throw.take().unwrap_or(Value::UNDEFINED);
                            self.reject(p, reason);
                        }
                        self.set(base, dst, Value::heap(p));
                        ip += 1;
                    }
                    Instr::CallSpread { dst, callee, args } => {
                        let callee_v = self.get(base, callee);
                        let args_v = self.get(base, args);
                        let arg_vec = self.array_snapshot(args_v.heap_index());
                        let result = self.call_value(callee_v, Value::UNDEFINED, &arg_vec)?;
                        self.set(base, dst, result);
                        ip += 1;
                    }
                    Instr::CallMethodSpread { dst, obj, name, args } => {
                        let recv = self.get(base, obj);
                        let prog: &'p Program = self.program;
                        let key: &'p str =
                            &prog.functions[func_id as usize].string_constants[name as usize];
                        let args_v = self.get(base, args);
                        let arg_vec = self.array_snapshot(args_v.heap_index());
                        // Builtin (array/string/number) method, else a user method
                        // resolved off the receiver and called with `this = recv`.
                        let result = match self.dispatch_builtin_method(recv, key, &arg_vec)? {
                            Some(r) => r,
                            None => {
                                let prop = self.get_prop(recv, key)?;
                                self.call_value(prop, recv, &arg_vec)?
                            }
                        };
                        self.set(base, dst, result);
                        ip += 1;
                    }
                    Instr::MathOp { dst, op, arg_base, argc } => {
                        let r = self.eval_math(op, base, arg_base, argc)?;
                        self.set(base, dst, Value::num(r));
                        ip += 1;
                    }
                    Instr::GlobalFn { dst, op, arg_base, argc } => {
                        use crate::bytecode::GlobalFn as G;
                        let a0 = if argc >= 1 { self.get(base, arg_base) } else { Value::UNDEFINED };
                        let v = match op {
                            G::Number => {
                                if argc == 0 { Value::num(0.0) } else { Value::num(self.to_number(a0)?) }
                            }
                            G::String => {
                                if argc == 0 {
                                    self.alloc_str(String::new())
                                } else {
                                    let s = self.display(a0);
                                    self.alloc_str(s)
                                }
                            }
                            G::Boolean => Value::bool(argc >= 1 && self.truthy(a0)),
                            G::ParseInt => {
                                let s = self.display(a0);
                                let radix = if argc >= 2 {
                                    self.to_number(self.get(base, arg_base + 1))? as i32
                                } else {
                                    0
                                };
                                Value::num(parse_int(&s, radix))
                            }
                            G::ParseFloat => Value::num(parse_float(&self.display(a0))),
                            // isNaN/isFinite coerce and never throw for the values
                            // in this subset; treat any coercion failure as NaN.
                            G::IsNaN => {
                                Value::bool(self.to_number(a0).unwrap_or(f64::NAN).is_nan())
                            }
                            G::IsFinite => {
                                Value::bool(self.to_number(a0).unwrap_or(f64::NAN).is_finite())
                            }
                        };
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::InstanceOf { dst, val, ctor } => {
                        let v = self.get(base, val);
                        let r = self.eval_instanceof(v, ctor);
                        self.set(base, dst, Value::bool(r));
                        ip += 1;
                    }
                    Instr::HasProp { dst, key, obj } => {
                        let k = self.get(base, key);
                        let o = self.get(base, obj);
                        // Proxy `has` trap (or fall through to the target).
                        let r = if let Some((target, handler, revoked)) =
                            o.is_heap().then(|| self.proxy_parts(o.heap_index())).flatten()
                        {
                            if revoked {
                                return Err(Thrown("TypeError: Cannot perform 'has' on a revoked proxy".into()));
                            }
                            match self.proxy_trap(handler, "has")? {
                                Some(trap) => {
                                    let ks = self.key_of(k);
                                    let kv = self.key_to_value(&ks);
                                    let res = self.call_value(trap, handler, &[target, kv])?;
                                    self.truthy(res)
                                }
                                None => self.has_property(target, k),
                            }
                        } else {
                            self.has_property(o, k)
                        };
                        self.set(base, dst, Value::bool(r));
                        ip += 1;
                    }
                    Instr::InstanceOfDyn { dst, val, ctor } => {
                        let v = self.get(base, val);
                        let c = self.get(base, ctor);
                        // A class uses its `extends` chain; a constructor FUNCTION
                        // checks whether `F.prototype` is in `v`'s prototype chain.
                        let kind = if c.is_heap() {
                            match self.heap.get(c.heap_index()) {
                                HeapObj::Class(_) => 1u8,
                                HeapObj::Func(_) | HeapObj::Closure { .. } | HeapObj::Bound { .. } => 2,
                                // Built-in constructor globals (Map/Set/Date/WeakMap/…)
                                // are objects but constructable: use prototype-chain check.
                                HeapObj::Object(m) if m.is_ctor => 2,
                                _ => 0,
                            }
                        } else {
                            0
                        };
                        let r = match kind {
                            1 => v.is_heap() && self.instance_of_class(v, c.heap_index()),
                            2 => self.instanceof_via_proto(v, c),
                            _ => false,
                        };
                        self.set(base, dst, Value::bool(r));
                        ip += 1;
                    }
                    Instr::StaticFn { dst, op, arg_base, argc } => {
                        use crate::bytecode::StaticFn as S;
                        let mut args: Vec<Value> = Vec::with_capacity(argc as usize);
                        for i in 0..argc {
                            args.push(self.get(base, arg_base + i));
                        }
                        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
                        let v = match op {
                            S::ArrayOf => Value::heap(self.heap.alloc(HeapObj::Array(args))),
                            S::NumberIsInteger => Value::bool(num_is_integer(a0)),
                            S::NumberIsNaN => Value::bool(a0.is_double() && a0.as_f64().is_nan()),
                            S::NumberIsFinite => Value::bool(num_is_finite(a0)),
                            S::NumberIsSafeInteger => Value::bool(num_is_safe_integer(a0)),
                            S::StringFromCharCode => {
                                let s: String = args
                                    .iter()
                                    .map(|&v| {
                                        // ToUint16 of each code unit.
                                        let u = to_uint32(self.to_number(v).unwrap_or(0.0)) as u16;
                                        char::from_u32(u as u32).unwrap_or('\u{FFFD}')
                                    })
                                    .collect();
                                self.alloc_str(s)
                            }
                            S::ObjectAssign => self.object_assign(&args)?,
                            S::ObjectFromEntries => {
                                let entries = self.iterate_to_vec(a0)?;
                                let mut map = ObjMap::new();
                                for e in entries {
                                    let kv = self.get_index(e, Value::int(0))?;
                                    let k = self.display(kv);
                                    let v = self.get_index(e, Value::int(1))?;
                                    map.set(&k, v);
                                }
                                Value::heap(self.heap.alloc(HeapObj::Object(map)))
                            }
                            S::PromiseResolve => {
                                // Promise.resolve(p) of an existing Promise is identity.
                                if a0.is_heap()
                                    && matches!(self.heap.get(a0.heap_index()), HeapObj::Promise { .. })
                                {
                                    a0
                                } else {
                                    let p = self.alloc_promise();
                                    self.resolve(p, a0);
                                    Value::heap(p)
                                }
                            }
                            S::PromiseReject => {
                                let p = self.alloc_promise();
                                self.reject(p, a0);
                                Value::heap(p)
                            }
                            S::PromiseAll => self.promise_combine(crate::heap::CombKind::All, a0)?,
                            S::PromiseAllSettled => {
                                self.promise_combine(crate::heap::CombKind::AllSettled, a0)?
                            }
                            S::PromiseRace => self.promise_combine(crate::heap::CombKind::Race, a0)?,
                            S::PromiseAny => self.promise_combine(crate::heap::CombKind::Any, a0)?,
                            S::ObjectDefineProperty => {
                                let key = self.key_of(args.get(1).copied().unwrap_or(Value::UNDEFINED));
                                let desc = args.get(2).copied().unwrap_or(Value::UNDEFINED);
                                self.object_define_property(a0, &key, desc)?;
                                a0
                            }
                            S::ObjectDefineProperties => {
                                let props = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                                self.object_define_properties(a0, props)?;
                                a0
                            }
                            S::ObjectGetOwnPropertyDescriptor => {
                                let key = self.key_of(args.get(1).copied().unwrap_or(Value::UNDEFINED));
                                self.object_get_own_property_descriptor(a0, &key)
                            }
                            S::ObjectGetOwnPropertyNames => self.object_own_property_names(a0),
                            S::ObjectGetPrototypeOf => self.object_get_prototype_of(a0),
                            S::ObjectCreate => {
                                let o = Value::heap(self.heap.alloc(HeapObj::Object(ObjMap::new())));
                                if a0 != Value::UNDEFINED {
                                    self.proto_of.insert(o.heap_index(), a0);
                                }
                                if let Some(props) = args.get(1).copied() {
                                    if props != Value::UNDEFINED {
                                        self.object_define_properties(o, props)?;
                                    }
                                }
                                o
                            }
                        };
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::ArrayFrom { dst, src, mapfn } => {
                        let sv = self.get(base, src);
                        let fnv = self.get(base, mapfn);
                        let out = self.array_from(sv, fnv)?;
                        self.set(base, dst, out);
                        ip += 1;
                    }
                    Instr::MathSpread { dst, op, args } => {
                        use crate::bytecode::MathFn as M;
                        let av = self.get(base, args);
                        let elems = self.array_snapshot(av.heap_index());
                        let nums: Vec<f64> =
                            elems.iter().map(|&v| self.to_number(v)).collect::<Result<_, _>>()?;
                        let r = match op {
                            M::Max => nums.iter().fold(f64::NEG_INFINITY, |a, &b| {
                                if a.is_nan() || b.is_nan() { f64::NAN } else { a.max(b) }
                            }),
                            M::Min => nums.iter().fold(f64::INFINITY, |a, &b| {
                                if a.is_nan() || b.is_nan() { f64::NAN } else { a.min(b) }
                            }),
                            M::Hypot => nums.iter().map(|&v| v * v).sum::<f64>().sqrt(),
                            // A non-variadic Math fn spread is unusual; apply to elem 0.
                            _ => self.eval_math_one(op, nums.first().copied().unwrap_or(f64::NAN)),
                        };
                        self.set(base, dst, Value::num(r));
                        ip += 1;
                    }

                    Instr::Jump { target } => {
                        let t = target as usize;
                        // ── OSR tier ── a backward jump is a loop back-edge. After
                        // the region heats up, compile `[target, ip]` (the loop
                        // body, headed at `target`) and run it natively; the
                        // native code returns the ip to resume at (a clean loop
                        // exit or a guard bail). Gated like the function JIT:
                        // enabled, and not inside a native self-recursion.
                        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
                        if self.jit_enabled && self.jit_recurse_depth == 0 && t < ip {
                            if let Some(resume) = self.try_run_osr(func_id, t as u32, base) {
                                ip = resume;
                                continue;
                            }
                            if self.jit.record_region(func_id, t as u32) {
                                let proto: *const crate::bytecode::FuncProto =
                                    &self.program.functions[func_id as usize];
                                // SAFETY: program functions are immutable during run.
                                let proto_ref = unsafe { &*proto };
                                self.jit.compile_region(
                                    func_id,
                                    proto_ref,
                                    t as u32,
                                    ip as u32,
                                    jit_globals_base as usize,
                                    crate::codegen::HeapHelperAddrs {
                                        get_prop_miss: jit_get_prop_miss as usize,
                                        set_prop_miss: jit_set_prop_miss as usize,
                                        versions_base: jit_heap_versions_base as usize,
                                        ic_base: jit_ic_base as usize,
                                        get_index: jit_get_index as usize,
                                        set_index: jit_set_index as usize,
                                        array_push: jit_array_push as usize,
                                        char_code_at: jit_char_code_at as usize,
                                        concat: jit_concat as usize,
                                        str_append: jit_str_append as usize,
                                    },
                                    self.program.global_count, // field-global pool base
                                    FIELD_POOL as u32,
                                );
                                if let Some(resume) = self.try_run_osr(func_id, t as u32, base) {
                                    ip = resume;
                                    continue;
                                }
                            }
                        }
                        ip = t;
                    }
                    Instr::JumpIfFalse { cond, target } => {
                        let v = self.get(base, cond);
                        if !self.truthy(v) {
                            ip = target as usize;
                        } else {
                            ip += 1;
                        }
                    }
                    Instr::JumpIfTrue { cond, target } => {
                        let v = self.get(base, cond);
                        if self.truthy(v) {
                            ip = target as usize;
                        } else {
                            ip += 1;
                        }
                    }
                    Instr::JumpIfNotLt { a, b, target } => {
                        let r = self.cmp_lt(base, a, b)?;
                        if !r {
                            ip = target as usize;
                        } else {
                            ip += 1;
                        }
                    }
                    Instr::JumpIfNotLe { a, b, target } => {
                        let r = self.cmp_le(base, a, b)?;
                        if !r {
                            ip = target as usize;
                        } else {
                            ip += 1;
                        }
                    }

                    Instr::Print { arg_base, argc, to_stderr } => {
                        let mut parts = Vec::with_capacity(argc as usize);
                        for i in 0..argc {
                            let v = self.get(base, arg_base + i);
                            parts.push(self.inspect(v));
                        }
                        let line = parts.join(" ");
                        if to_stderr {
                            self.errput.push(line);
                        } else {
                            self.output.push(line);
                        }
                        ip += 1;
                    }

                    Instr::MakeFunc { dst, func_id } => {
                        let v = Value::heap(self.heap.alloc(HeapObj::Func(func_id)));
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::ObjectKeys { dst, obj } => {
                        let o = self.get(base, obj);
                        // Collect the raw key strings first (immutable heap
                        // borrow), then intern them (mutable) — can't hold both.
                        let key_strs: Vec<String> = if o.is_heap() {
                            match self.heap.get(o.heap_index()) {
                                // Only OWN ENUMERABLE keys (skip non-enumerable
                                // and private "#" names).
                                HeapObj::Object(map) => map
                                    .keys
                                    .iter()
                                    .zip(map.attrs.iter())
                                    .filter(|(k, a)| a.enumerable && !is_hidden_key(k))
                                    .map(|(k, _)| k.clone())
                                    .collect(),
                                HeapObj::Array(items) => {
                                    (0..items.len()).map(|i| i.to_string()).collect()
                                }
                                _ => Vec::new(),
                            }
                        } else {
                            Vec::new()
                        };
                        let keys: Vec<Value> =
                            key_strs.into_iter().map(|k| self.alloc_str(k)).collect();
                        let v = Value::heap(self.heap.alloc(HeapObj::Array(keys)));
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::ObjectValues { dst, obj } => {
                        let o = self.get(base, obj);
                        let vals: Vec<Value> = if o.is_heap() {
                            match self.heap.get(o.heap_index()) {
                                HeapObj::Object(map) => map
                                    .vals
                                    .iter()
                                    .zip(map.attrs.iter())
                                    .filter(|(_, a)| a.enumerable)
                                    .map(|(v, _)| *v)
                                    .collect(),
                                HeapObj::Array(items) => items.clone(),
                                _ => Vec::new(),
                            }
                        } else {
                            Vec::new()
                        };
                        let v = Value::heap(self.heap.alloc(HeapObj::Array(vals)));
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::ObjectEntries { dst, obj } => {
                        let o = self.get(base, obj);
                        // Snapshot (key string, value) pairs under the immutable
                        // borrow, then build `[key, value]` arrays (which allocate).
                        let pairs: Vec<(String, Value)> = if o.is_heap() {
                            match self.heap.get(o.heap_index()) {
                                HeapObj::Object(map) => map
                                    .keys
                                    .iter()
                                    .cloned()
                                    .zip(map.vals.iter().copied())
                                    .zip(map.attrs.iter())
                                    .filter(|(_, a)| a.enumerable)
                                    .map(|(kv, _)| kv)
                                    .collect(),
                                HeapObj::Array(items) => {
                                    items.iter().enumerate().map(|(i, v)| (i.to_string(), *v)).collect()
                                }
                                _ => Vec::new(),
                            }
                        } else {
                            Vec::new()
                        };
                        let mut entries = Vec::with_capacity(pairs.len());
                        for (k, val) in pairs {
                            let ks = self.alloc_str(k);
                            let inner = self.heap.alloc(HeapObj::Array(vec![ks, val]));
                            entries.push(Value::heap(inner));
                        }
                        let v = Value::heap(self.heap.alloc(HeapObj::Array(entries)));
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::LenOf { dst, obj } => {
                        let o = self.get(base, obj);
                        let v = if o.is_heap() {
                            match self.heap.get(o.heap_index()) {
                                HeapObj::Array(items) => len_value(items.len()),
                                HeapObj::Str(s) => len_value(s.char_len),
                                HeapObj::Cons { len, .. } => len_value(*len),
                                // for-of over a Map/Set iterates `size` slots.
                                HeapObj::Map { keys, .. } => len_value(keys.len()),
                                HeapObj::Set(items) => len_value(items.len()),
                                _ => Value::int(0),
                            }
                        } else {
                            Value::int(0)
                        };
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::MakeClosure { dst, func_id } => {
                        // Capture each upvalue's cell index, resolved in THIS
                        // (defining) frame: a ParentLocal source reads the cell
                        // index from a local register (the local was boxed via
                        // MakeCell); a ParentUpval source forwards one of this
                        // frame's own captured cells.
                        let sources = &self.program.functions[func_id as usize].upvalues;
                        let mut cells = Vec::with_capacity(sources.len());
                        for src in sources {
                            let cell = match *src {
                                UpvalSource::ParentLocal(reg) => {
                                    self.get(base, reg).heap_index()
                                }
                                UpvalSource::ParentUpval(idx) => {
                                    self.closure_upvalue(cur_closure, idx)
                                }
                            };
                            cells.push(cell);
                        }
                        let v = Value::heap(
                            self.heap.alloc(HeapObj::Closure { func: func_id, upvalues: cells }),
                        );
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::MakeCell { reg } => {
                        let v = self.get(base, reg);
                        let cell = self.heap.alloc(HeapObj::Cell(v));
                        self.set(base, reg, Value::heap(cell));
                        ip += 1;
                    }
                    Instr::CellGet { dst, cell } => {
                        let cell_idx = self.get(base, cell).heap_index();
                        let v = self.heap.cell_get(cell_idx);
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::CellSet { cell, src } => {
                        let cell_idx = self.get(base, cell).heap_index();
                        let v = self.get(base, src);
                        self.heap.cell_set(cell_idx, v);
                        ip += 1;
                    }
                    Instr::UpvalGet { dst, idx } => {
                        let cell = self.closure_upvalue(cur_closure, idx);
                        let v = self.heap.cell_get(cell);
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::UpvalSet { idx, src } => {
                        let cell = self.closure_upvalue(cur_closure, idx);
                        let v = self.get(base, src);
                        self.heap.cell_set(cell, v);
                        ip += 1;
                    }
                    Instr::NewArray { dst, arg_base, argc } => {
                        let mut items = Vec::with_capacity(argc as usize);
                        for i in 0..argc {
                            items.push(self.get(base, arg_base + i));
                        }
                        let v = Value::heap(self.heap.alloc(HeapObj::Array(items)));
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::NewObject { dst } => {
                        let v = Value::heap(self.heap.alloc(HeapObj::Object(ObjMap::new())));
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::NewError { dst, kind, arg } => {
                        let msg = arg.map(|r| self.get(base, r));
                        let v = self.make_error(kind, msg);
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::MakeSymbol { dst, desc } => {
                        // `Symbol(desc)`: description is ToString(desc) unless absent/undefined.
                        let d = match desc {
                            Some(r) => {
                                let v = self.get(base, r);
                                if v == Value::UNDEFINED {
                                    Value::UNDEFINED
                                } else {
                                    let s = self.to_js_string(v)?;
                                    self.alloc_str(s)
                                }
                            }
                            None => Value::UNDEFINED,
                        };
                        let sym = self.make_symbol(d);
                        self.set(base, dst, sym);
                        ip += 1;
                    }
                    Instr::LoadBigInt { dst, value } => {
                        let v = Value::heap(self.heap.alloc(HeapObj::BigInt(value)));
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::BigIntFrom { dst, arg } => {
                        let a = self.get(base, arg);
                        let n = self.to_bigint(a)?;
                        let v = self.make_bigint(n);
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::NewRegExp { dst, pattern, flags } => {
                        let p = self.get(base, pattern);
                        let f = self.get(base, flags);
                        let v = self.build_regexp(p, f)?;
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::GetIndex { dst, obj, key } => {
                        let o = self.get(base, obj);
                        let k = self.get(base, key);
                        let r = self.get_index(o, k)?;
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::SetIndex { obj, key, val } => {
                        let o = self.get(base, obj);
                        let k = self.get(base, key);
                        let v = self.get(base, val);
                        self.set_index(o, k, v)?;
                        ip += 1;
                    }
                    Instr::GetProp { dst, obj, name } => {
                        let o = self.get(base, obj);
                        let key = self.program.functions[func_id as usize]
                            .string_constants[name as usize]
                            .clone();
                        let r = self.get_prop(o, &key)?;
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::SetProp { obj, name, val } => {
                        let o = self.get(base, obj);
                        let v = self.get(base, val);
                        let key = self.program.functions[func_id as usize]
                            .string_constants[name as usize]
                            .clone();
                        self.set_prop(o, &key, v)?;
                        ip += 1;
                    }
                    Instr::DeleteProp { dst, obj, name } => {
                        let o = self.get(base, obj);
                        let key = self.program.functions[func_id as usize]
                            .string_constants[name as usize]
                            .clone();
                        let r = self.delete_property(o, &key)?;
                        self.set(base, dst, r);
                        ip += 1;
                    }
                    Instr::DeleteIndex { dst, obj, key } => {
                        let o = self.get(base, obj);
                        let k = self.get(base, key);
                        let ks = self.key_of(k); // ToPropertyKey (symbol → its prop_key)
                        let r = self.delete_property(o, &ks)?;
                        self.set(base, dst, r);
                        ip += 1;
                    }

                    Instr::Call { dst, callee, arg_base, argc } => {
                        let callee_v = self.get(base, callee);
                        // A callable Proxy: route through call_value (apply trap).
                        if callee_v.is_heap()
                            && matches!(self.heap.get(callee_v.heap_index()), HeapObj::Proxy { .. })
                        {
                            let argv: Vec<Value> =
                                (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                            let r = self.call_value(callee_v, Value::UNDEFINED, &argv)?;
                            self.set(base, dst, r);
                            ip += 1;
                            continue;
                        }
                        // A native resolve/reject function (from `new Promise`).
                        if callee_v.is_heap() {
                            if let HeapObj::BoundResolver { promise, is_reject } =
                                self.heap.get(callee_v.heap_index())
                            {
                                let (p, isr) = (*promise, *is_reject);
                                let arg = if argc >= 1 {
                                    self.get(base, arg_base)
                                } else {
                                    Value::UNDEFINED
                                };
                                if isr {
                                    self.reject(p, arg);
                                } else {
                                    self.resolve(p, arg);
                                }
                                self.set(base, dst, Value::UNDEFINED);
                                ip += 1;
                                continue;
                            }
                            // A bound or native function: run via call_value (fixes
                            // `this`/prepends bound args, or dispatches the builtin).
                            // %Function.prototype% is also a callable (returns undefined).
                            if matches!(
                                self.heap.get(callee_v.heap_index()),
                                HeapObj::Bound { .. } | HeapObj::Native(_)
                            ) || (self.fn_proto != 0 && callee_v.heap_index() == self.fn_proto)
                            {
                                let argv: Vec<Value> =
                                    (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                                let r = self.call_value(callee_v, Value::UNDEFINED, &argv)?;
                                self.set(base, dst, r);
                                ip += 1;
                                continue;
                            }
                        }
                        let (fid, closure) = self.resolve_callable(callee_v)?;
                        // An `async function*` returns an AsyncGenerator (checked
                        // before the plain-generator/async cases since it is both).
                        if self.program.functions[fid as usize].is_generator
                            && self.program.functions[fid as usize].is_async
                        {
                            let argv: Vec<Value> =
                                (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                            let ag = self.alloc_async_generator(fid, closure, Value::UNDEFINED, &argv);
                            self.set(base, dst, ag);
                            ip += 1;
                            continue;
                        }
                        // A generator function returns a Generator object, unrun.
                        if self.program.functions[fid as usize].is_generator {
                            let argv: Vec<Value> =
                                (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                            let g = self.alloc_generator(fid, closure, Value::UNDEFINED, &argv);
                            self.set(base, dst, g);
                            ip += 1;
                            continue;
                        }
                        // An async function runs to its first `await` then returns
                        // its result Promise.
                        if self.program.functions[fid as usize].is_async {
                            let argv: Vec<Value> =
                                (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                            let p = self.alloc_async(fid, closure, Value::UNDEFINED, &argv);
                            self.set(base, dst, p);
                            ip += 1;
                            continue;
                        }
                        self.setup_call(
                            fid,
                            closure,
                            Value::UNDEFINED,
                            base,
                            arg_base,
                            argc,
                            dst,
                            ip + 1,
                        )?;
                        break;
                    }

                    Instr::CallMethod { dst, obj, name, arg_base, argc } => {
                        let recv = self.get(base, obj);
                        // `program` outlives the VM, so borrow the method name
                        // with the program's lifetime (NOT self's) — avoids
                        // cloning the name string on every method call (a heap
                        // alloc per `a.push(i)` / `a.map(cb)` etc.).
                        let prog: &'p Program = self.program;
                        let key: &'p str =
                            &prog.functions[func_id as usize].string_constants[name as usize];
                        // Hot fast path: `arr.push(x)` — the most common
                        // per-element array idiom. Append directly, skipping the
                        // try_builtin_method → dispatch_builtin_method → array_method
                        // layering (and the args-gather), then return the new length.
                        if argc == 1 && key == "push" && recv.is_heap() {
                            let v = self.get(base, arg_base);
                            let len = if let HeapObj::Array(items) =
                                self.heap.get_mut(recv.heap_index())
                            {
                                items.push(v);
                                Some(items.len() as i32)
                            } else {
                                None
                            };
                            if let Some(len) = len {
                                self.set(base, dst, Value::int(len));
                                ip += 1;
                                continue;
                            }
                        }
                        // Builtin methods (array/string) execute inline and
                        // produce a result without pushing a frame.
                        if let Some(result) = self.try_builtin_method(recv, key, base, arg_base, argc)? {
                            self.set(base, dst, result);
                            ip += 1;
                            continue;
                        }
                        // Otherwise the property must resolve to a function; call it
                        // with `this = recv`.
                        let prop = self.get_prop(recv, key)?;
                        // A native or bound method value (e.g. inherited from a
                        // prototype) is invoked via call_value with this = recv.
                        if prop.is_heap()
                            && (matches!(
                                self.heap.get(prop.heap_index()),
                                HeapObj::Native(_) | HeapObj::Bound { .. } | HeapObj::BoundResolver { .. }
                            ) || (self.fn_proto != 0 && prop.heap_index() == self.fn_proto))
                        {
                            let argv: Vec<Value> =
                                (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                            let r = self.call_value(prop, recv, &argv)?;
                            self.set(base, dst, r);
                            ip += 1;
                            continue;
                        }
                        let (fid, closure) = self.resolve_callable(prop)?;
                        // An `async function*` method returns an AsyncGenerator.
                        if self.program.functions[fid as usize].is_generator
                            && self.program.functions[fid as usize].is_async
                        {
                            let argv: Vec<Value> =
                                (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                            let ag = self.alloc_async_generator(fid, closure, recv, &argv);
                            self.set(base, dst, ag);
                            ip += 1;
                            continue;
                        }
                        // A generator method returns a Generator object, unrun.
                        if self.program.functions[fid as usize].is_generator {
                            let argv: Vec<Value> =
                                (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                            let g = self.alloc_generator(fid, closure, recv, &argv);
                            self.set(base, dst, g);
                            ip += 1;
                            continue;
                        }
                        // An async method runs to its first `await` then returns
                        // its result Promise.
                        if self.program.functions[fid as usize].is_async {
                            let argv: Vec<Value> =
                                (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                            let p = self.alloc_async(fid, closure, recv, &argv);
                            self.set(base, dst, p);
                            ip += 1;
                            continue;
                        }
                        self.setup_call(fid, closure, recv, base, arg_base, argc, dst, ip + 1)?;
                        break;
                    }

                    Instr::CallMethodComputed { dst, obj, key, arg_base, argc } => {
                        let recv = self.get(base, obj);
                        let k = self.get(base, key);
                        // `obj["push"](x)` etc: a builtin array/string method first.
                        let kstr = self.display(k);
                        if let Some(result) =
                            self.try_builtin_method(recv, &kstr, base, arg_base, argc)?
                        {
                            self.set(base, dst, result);
                            ip += 1;
                            continue;
                        }
                        // Else resolve the method off the receiver (own/inherited)
                        // and call it with `this = recv`.
                        let method = self.get_index(recv, k)?;
                        // A native / bound / resolver method value runs via call_value.
                        if method.is_heap()
                            && (matches!(
                                self.heap.get(method.heap_index()),
                                HeapObj::Native(_) | HeapObj::Bound { .. } | HeapObj::BoundResolver { .. }
                            ) || (self.fn_proto != 0 && method.heap_index() == self.fn_proto))
                        {
                            let argv: Vec<Value> =
                                (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                            let r = self.call_value(method, recv, &argv)?;
                            self.set(base, dst, r);
                            ip += 1;
                            continue;
                        }
                        let (fid, closure) = self.resolve_callable(method)?;
                        if self.program.functions[fid as usize].is_generator
                            && self.program.functions[fid as usize].is_async
                        {
                            let argv: Vec<Value> =
                                (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                            let ag = self.alloc_async_generator(fid, closure, recv, &argv);
                            self.set(base, dst, ag);
                            ip += 1;
                            continue;
                        }
                        if self.program.functions[fid as usize].is_generator {
                            let argv: Vec<Value> =
                                (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                            let g = self.alloc_generator(fid, closure, recv, &argv);
                            self.set(base, dst, g);
                            ip += 1;
                            continue;
                        }
                        if self.program.functions[fid as usize].is_async {
                            let argv: Vec<Value> =
                                (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                            let p = self.alloc_async(fid, closure, recv, &argv);
                            self.set(base, dst, p);
                            ip += 1;
                            continue;
                        }
                        self.setup_call(fid, closure, recv, base, arg_base, argc, dst, ip + 1)?;
                        break;
                    }

                    Instr::Throw { src } => {
                        let v = self.get(base, src);
                        let msg = self.throw_message(v);
                        // Persist ip so the (unused) frame state is coherent,
                        // then signal unwinding via pending_throw + Err.
                        let top = self.frames.len() - 1;
                        self.frames[top].ip = ip;
                        self.pending_throw = Some(v);
                        return Err(Thrown(msg));
                    }
                    Instr::PushHandler { catch_target, catch_reg } => {
                        let top = self.frames.len() - 1;
                        self.frames[top]
                            .handlers
                            .push(Handler::Catch { target: catch_target, reg: catch_reg });
                        ip += 1;
                    }
                    Instr::PopHandler => {
                        let top = self.frames.len() - 1;
                        self.frames[top].handlers.pop();
                        ip += 1;
                    }
                    Instr::PushFinally { target, kind_reg, val_reg } => {
                        let top = self.frames.len() - 1;
                        self.frames[top]
                            .handlers
                            .push(Handler::Finally { target, kind_reg, val_reg });
                        ip += 1;
                    }
                    Instr::PopFinally => {
                        let top = self.frames.len() - 1;
                        self.frames[top].handlers.pop();
                        ip += 1;
                    }
                    Instr::EndFinally { kind_reg, val_reg } => {
                        // Resume the completion deposited when this finally was
                        // entered: 1 = return (re-leave through any outer finally,
                        // else return), 2 = throw (re-raise), else 0 = normal.
                        match self.regs[base + kind_reg as usize].as_int() {
                            1 => {
                                let v = self.regs[base + val_reg as usize];
                                if let Some(target) = self.route_through_finally(1, v) {
                                    ip = target as usize;
                                    continue;
                                }
                                if self.pop_frame_with(v, stop_depth) {
                                    return Ok(v);
                                }
                                break;
                            }
                            2 => {
                                let v = self.regs[base + val_reg as usize];
                                let top = self.frames.len() - 1;
                                self.frames[top].ip = ip;
                                self.pending_throw = Some(v);
                                return Err(Thrown(self.throw_message(v)));
                            }
                            _ => {
                                ip += 1;
                            }
                        }
                    }
                    Instr::SetRaw { arr, raw } => {
                        let a = self.get(base, arr);
                        let r = self.get(base, raw);
                        if a.is_heap() {
                            self.template_raws.insert(a.heap_index(), r);
                        }
                        ip += 1;
                    }
                    Instr::GetIterator { dst, src } => {
                        let s = self.get(base, src);
                        let it = self.get_iterator(s)?;
                        self.set(base, dst, it);
                        ip += 1;
                    }
                    Instr::GetAsyncIterator { dst, src } => {
                        let s = self.get(base, src);
                        let it = self.get_async_iterator(s)?;
                        self.set(base, dst, it);
                        ip += 1;
                    }
                    Instr::IterToArray { dst, src, count } => {
                        let s = self.get(base, src);
                        let a = self.iter_to_array(s, count)?;
                        self.set(base, dst, a);
                        ip += 1;
                    }
                    Instr::Random { dst } => {
                        // xorshift64* → a uniform double in [0, 1) (top 53 bits).
                        let mut x = self.rng_state;
                        x ^= x >> 12;
                        x ^= x << 25;
                        x ^= x >> 27;
                        self.rng_state = x;
                        let r = x.wrapping_mul(0x2545_F491_4F6C_DD1D);
                        let f = (r >> 11) as f64 / (1u64 << 53) as f64;
                        self.set(base, dst, Value::num(f));
                        ip += 1;
                    }
                    Instr::DateNew { dst, arg_base, argc } => {
                        let args: Vec<Value> =
                            (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                        let ms = self.date_new_ms(&args)?;
                        let v = Value::heap(self.heap.alloc(HeapObj::Date(ms)));
                        self.set(base, dst, v);
                        ip += 1;
                    }
                    Instr::DateUTC { dst, arg_base, argc } => {
                        let args: Vec<Value> =
                            (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                        let ms = self.date_utc_ms(&args)?;
                        self.set(base, dst, Value::num(ms));
                        ip += 1;
                    }
                    Instr::DateParse { dst, src } => {
                        let s = self.get(base, src);
                        let str = self.display(s);
                        self.set(base, dst, Value::num(parse_date(&str)));
                        ip += 1;
                    }
                    Instr::Return { src } => {
                        let v = self.regs[base + src as usize];
                        // Run any pending `finally` in this frame first.
                        if let Some(target) = self.route_through_finally(1, v) {
                            ip = target as usize;
                            continue;
                        }
                        if self.pop_frame_with(v, stop_depth) {
                            return Ok(v);
                        }
                        break;
                    }
                    Instr::ReturnUndefined => {
                        if let Some(target) = self.route_through_finally(1, Value::UNDEFINED) {
                            ip = target as usize;
                            continue;
                        }
                        if self.pop_frame_with(Value::UNDEFINED, stop_depth) {
                            return Ok(Value::UNDEFINED);
                        }
                        break;
                    }
                    Instr::Yield { val, .. } => {
                        // Suspend the generator: pop the frame ENTRY but leave its
                        // register window live at the top of `self.regs` so the
                        // resumer (generator_method) can copy it back into the heap
                        // Generator. The generator frame is always the top (and the
                        // run_loop's stop frame) at a yield, so popping returns to
                        // the resumer. `pending_yield` carries the value + this ip.
                        let v = self.get(base, val);
                        self.frames.pop();
                        self.pending_yield = Some((v, ip));
                        return Ok(v);
                    }
                    Instr::Await { val, .. } => {
                        // Suspend the async activation: pop the frame ENTRY but
                        // leave its register window live at the top of `self.regs`
                        // for `drive_async` to park into the heap AsyncState. Unlike
                        // a generator yield, we CAPTURE the frame's `try` handlers
                        // (carried in `pending_await`) so they can be restored on
                        // resume — letting `try { await p } catch (e)` see a
                        // rejection thrown back in at the await point. The async
                        // frame is always the top (and the run_loop stop frame) at
                        // an await, so popping returns to `drive_async`.
                        let v = self.get(base, val);
                        let f = self.frames.pop().unwrap();
                        self.pending_await = Some((v, ip, f.handlers));
                        return Ok(v);
                    }
                    Instr::IterNext { value_dst, done_dst, iter, idx } => {
                        let it = self.get(base, iter);
                        if !it.is_heap() {
                            return Err(Thrown(format!(
                                "TypeError: {} is not iterable",
                                self.display(it)
                            )));
                        }
                        // A generator is driven by `.next()`; the cursor is unused.
                        if matches!(self.heap.get(it.heap_index()), HeapObj::Generator { .. }) {
                            let res = self
                                .generator_method(it.heap_index(), "next", &[])?
                                .unwrap_or(Value::UNDEFINED);
                            let done = self.get_prop(res, "done")?;
                            let val = self.get_prop(res, "value")?;
                            self.set(base, value_dst, val);
                            self.set(base, done_dst, done);
                            ip += 1;
                            continue;
                        }
                        // A user iterator object (`@@iterator` already resolved by
                        // GetIterator): pull the next result via `.next()`. Lazy —
                        // a `break` simply stops calling it.
                        if matches!(self.heap.get(it.heap_index()), HeapObj::Object(_) | HeapObj::Iterator { .. }) {
                            let next = self.get_prop(it, "next")?;
                            if self.is_callable(next) {
                                let res = self.call_value(next, it, &[])?;
                                let done = self.get_prop(res, "done")?;
                                let val = self.get_prop(res, "value")?;
                                self.set(base, value_dst, val);
                                self.set(base, done_dst, done);
                                ip += 1;
                                continue;
                            }
                        }
                        // Array/Set element, string char, or Map [k,v] at the cursor.
                        let cursor = array_index(self.get(base, idx)).unwrap_or(0);
                        let len = match self.heap.get(it.heap_index()) {
                            HeapObj::Array(items) => items.len(),
                            HeapObj::Set(items) => items.len(),
                            HeapObj::Str(s) => s.char_len,
                            HeapObj::Cons { len, .. } => *len,
                            HeapObj::Map { keys, .. } => keys.len(),
                            HeapObj::TypedArray { length, .. } => *length,
                            _ => {
                                return Err(Thrown(format!(
                                    "TypeError: {} is not iterable",
                                    self.display(it)
                                )))
                            }
                        };
                        if cursor < len {
                            let val = self.get_index(it, Value::int(cursor as i32))?;
                            self.set(base, value_dst, val);
                            self.set(base, done_dst, Value::bool(false));
                            self.set(base, idx, Value::int((cursor + 1) as i32));
                        } else {
                            self.set(base, done_dst, Value::bool(true));
                        }
                        ip += 1;
                    }
                    Instr::ForAwaitNext { dst, iter, idx } => {
                        let it = self.get(base, iter);
                        if !it.is_heap() {
                            return Err(Thrown(format!(
                                "TypeError: {} is not iterable",
                                self.display(it)
                            )));
                        }
                        let result = match self.heap.get(it.heap_index()) {
                            // Async iterator: `.next()` returns a Promise the loop awaits.
                            HeapObj::AsyncGenerator(_) => self
                                .async_generator_method(it.heap_index(), "next", &[])
                                .unwrap_or(Value::UNDEFINED),
                            // Sync generator: `.next()` returns {value,done} (awaited = no-op tick).
                            HeapObj::Generator { .. } => self
                                .generator_method(it.heap_index(), "next", &[])?
                                .unwrap_or(Value::UNDEFINED),
                            // A user iterator object (sync or async) with `.next()`.
                            HeapObj::Object(_) => {
                                let next = self.get_prop(it, "next")?;
                                if self.is_callable(next) {
                                    self.call_value(next, it, &[])?
                                } else {
                                    return Err(Thrown(format!(
                                        "TypeError: {} is not iterable",
                                        self.display(it)
                                    )));
                                }
                            }
                            // Array/Set element, string char, Map [k,v] — positional,
                            // wrapped in a {value, done} the loop awaits (a tick).
                            _ => {
                                let cursor = array_index(self.get(base, idx)).unwrap_or(0);
                                let len = match self.heap.get(it.heap_index()) {
                                    HeapObj::Array(items) => items.len(),
                                    HeapObj::Set(items) => items.len(),
                                    HeapObj::Str(s) => s.char_len,
                                    HeapObj::Cons { len, .. } => *len,
                                    HeapObj::Map { keys, .. } => keys.len(),
                                    _ => {
                                        return Err(Thrown(format!(
                                            "TypeError: {} is not iterable",
                                            self.display(it)
                                        )))
                                    }
                                };
                                if cursor < len {
                                    let val = self.get_index(it, Value::int(cursor as i32))?;
                                    self.set(base, idx, Value::int((cursor + 1) as i32));
                                    self.iter_result(val, false)
                                } else {
                                    self.iter_result(Value::UNDEFINED, true)
                                }
                            }
                        };
                        self.set(base, dst, result);
                        ip += 1;
                    }
                }
            }
        }
    }

    /// If `func_id` has compiled native code, run it over the register window
    /// at `base` and return `(result_bits_as_Value, bail_ip)`. `None` if there
    /// is no compiled code for this function.
    ///
    /// The native code reads/writes `self.regs[base..]` directly via a raw
    /// pointer taken here and used ONLY for the duration of the call — nothing
    /// in between can resize `self.regs` (the JIT subset issues no calls/allocs).
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    fn try_run_jit(&mut self, func_id: u32, base: usize) -> Option<(Value, u32)> {
        let jitfn = self.jit.get(func_id)? as *const crate::codegen::JitFn;
        // SAFETY: `jitfn` points into self.jit.compiled (stable for the call).
        // `regs_ptr` is valid for the frame's reg_count slots. A self-call op
        // routes through `jit_self_call` (passed the `vm` pointer below) which
        // may resize self.regs for the recursive frame — but it RESTORES regs to
        // this length before returning, and the native code re-reads its window
        // base from the callee-saved register only relative to `regs_ptr`, which
        // stays valid because jit_self_call uses a SEPARATE save/restore of the
        // regs Vec around the recursion (see its safety note).
        let regs_ptr = unsafe { self.regs.as_mut_ptr().add(base) } as *mut u64;
        let vm_ptr = self as *mut Vm as *mut core::ffi::c_void;
        let (bits, bail) = unsafe { (*jitfn).run(regs_ptr, vm_ptr) };
        Some((Value::from_bits(bits), bail))
    }

    /// Run the compiled OSR region for the loop headed at `entry_ip` (in
    /// `func_id`) over the frame's register window at `base`, returning the ip to
    /// resume interpreting at. `None` if no region is compiled for this header.
    ///
    /// The region's native code reads/writes `self.regs[base..]` and
    /// `self.globals` directly (the latter via a base pointer it fetches in its
    /// prologue). The numeric region issues NO calls that push frames or grow
    /// `self.regs`/`self.globals`, so the raw pointers stay valid for the call.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    fn try_run_osr(&mut self, func_id: u32, entry_ip: u32, base: usize) -> Option<usize> {
        let region = self.jit.get_region(func_id, entry_ip)? as *const crate::codegen::Region;
        // Object scalar-replacement (SROA): clone the sync plan so no region
        // borrow is held while the sync mutates globals/heap below.
        let field_plan = unsafe { (*region).field_plan().cloned() };

        // ── pre-run sync ── load the promoted object's fields into the scratch
        // pool globals the native code reads as ordinary globals.
        if let Some(ref p) = field_plan {
            let obj = self.globals[p.obj_global as usize];
            for &(name_idx, slot) in &p.fields {
                let key = self.program.functions[p.func_id as usize].string_constants
                    [name_idx as usize]
                    .clone();
                let v = self.get_prop(obj, &key).unwrap_or(Value::UNDEFINED);
                self.globals[slot as usize] = v;
            }
        }

        let regs_ptr = unsafe { self.regs.as_mut_ptr().add(base) } as *mut u64;
        let vm_ptr = self as *mut Vm as *mut core::ffi::c_void;
        // SAFETY: `region` is stable for the call (we don't mutate self.jit until
        // after); regs/globals do not move during a region run.
        let resume = unsafe { (*region).run(regs_ptr, vm_ptr) };

        // ── post-run sync ── flush the pool globals back to the object's fields,
        // so the interpreter (which resumes on the ORIGINAL bytecode, reading the
        // object) sees consistent values. Runs on EVERY exit (clean or bail).
        if let Some(ref p) = field_plan {
            let obj = self.globals[p.obj_global as usize];
            for &(name_idx, slot) in &p.fields {
                let key = self.program.functions[p.func_id as usize].string_constants
                    [name_idx as usize]
                    .clone();
                let v = self.globals[slot as usize];
                let _ = self.set_prop(obj, &key, v);
            }
        }
        // Bookkeeping: a resume INSIDE the region is a deopt; evict if chronic.
        self.jit.note_region_resume(func_id, entry_ip, resume);
        Some(resume as usize)
    }

    /// Pop the current frame. If this returns control to `stop_depth` (the
    /// frame the active `run_loop` was asked to run), report `true` so the loop
    /// returns `ret`. Otherwise deliver `ret` into the caller's `ret_dst` and
    /// report `false` to keep executing the caller.
    #[inline]
    fn pop_frame_with(&mut self, ret: Value, stop_depth: usize) -> bool {
        let finished = self.frames.pop().expect("frame underflow");
        // Shrink the register file back to the caller's window top.
        self.regs.truncate(finished.base);
        if self.frames.len() == stop_depth {
            return true;
        }
        let caller_base = self.frames.last().unwrap().base;
        self.regs[caller_base + finished.ret_dst as usize] = ret;
        false
    }

    /// Render a thrown value for the UNCAUGHT-throw message (the `Outcome.error`
    /// string). An Error-like object (`{message,…}` or one with a `.message`)
    /// prints `name: message`; otherwise the value's string form. Catchable
    /// throws bind the real `Value`, so this is only the top-level report.
    fn throw_message(&self, v: Value) -> String {
        if v.is_heap() {
            if let HeapObj::Object(map) = self.heap.get(v.heap_index()) {
                let name = map.get("name").map(|n| self.display(n));
                let msg = map.get("message").map(|m| self.display(m));
                return match (name, msg) {
                    (Some(n), Some(m)) => format!("{n}: {m}"),
                    (None, Some(m)) => format!("Error: {m}"),
                    _ => self.display(v),
                };
            }
        }
        format!("Uncaught {}", self.display(v))
    }

    // ── register access ──
    //
    // Unchecked: the compiler allocates `reg_count` registers per function and
    // never emits a register index ≥ `reg_count` (it tracks a `max_reg`
    // high-water mark), and every frame resizes `self.regs` to
    // `base + reg_count` on entry — so `base + r` is always in bounds. We index
    // `self.regs` freshly each call (no cached pointer), so a reallocation of
    // the register Vec by a re-entrant call/alloc is handled correctly. The
    // `debug_assert!` turns any compiler bug into a loud test failure in debug
    // builds while release elides the bounds check.
    #[inline(always)]
    fn get(&self, base: usize, r: u16) -> Value {
        debug_assert!((base + r as usize) < self.regs.len(), "reg read out of bounds");
        unsafe { *self.regs.get_unchecked(base + r as usize) }
    }
    #[inline(always)]
    fn set(&mut self, base: usize, r: u16, v: Value) {
        debug_assert!((base + r as usize) < self.regs.len(), "reg write out of bounds");
        unsafe {
            *self.regs.get_unchecked_mut(base + r as usize) = v;
        }
    }

    // ── call setup ──

    /// Resolve a value to a callable function id, or throw a TypeError.
    /// The cell heap-index captured at upvalue slot `idx` of the closure heap
    /// object `closure`. Panics only on a miscompiled program (an UpvalGet in a
    /// frame with no closure, or an out-of-range slot), which the compiler must
    /// not emit.
    #[inline]
    fn closure_upvalue(&self, closure: u32, idx: u16) -> u32 {
        match self.heap.get(closure) {
            HeapObj::Closure { upvalues, .. } => upvalues[idx as usize],
            _ => panic!("UpvalGet/Set in a frame without a closure"),
        }
    }

    /// Resolve a value to `(func_id, closure_heap_idx)`. `closure_heap_idx` is
    /// the value's heap index when it is a `Closure` (so the frame can reach its
    /// captured cells), or `NO_CLOSURE` for a plain `Func`.
    fn resolve_callable(&self, v: Value) -> Result<(u32, u32), Thrown> {
        if v.is_heap() {
            let idx = v.heap_index();
            match self.heap.get(idx) {
                HeapObj::Func(id) => return Ok((*id, NO_CLOSURE)),
                HeapObj::Closure { func, .. } => return Ok((*func, idx)),
                _ => {}
            }
        }
        Err(Thrown(format!("TypeError: {} is not a function", self.display(v))))
    }

    /// Push a new frame for `func_id`, binding `this_val` to register 0 and the
    /// `argc` arguments (staged at `caller_base + arg_base ..`) into registers
    /// `1..`. Records the caller's resume ip and result register.
    #[allow(clippy::too_many_arguments)]
    fn setup_call(
        &mut self,
        func_id: u32,
        closure: u32,
        this_val: Value,
        caller_base: usize,
        arg_base: u16,
        argc: u16,
        dst: u16,
        caller_ip_next: usize,
    ) -> Result<(), Thrown> {
        if self.frames.len() >= MAX_FRAMES {
            return Err(Thrown("RangeError: Maximum call stack size exceeded".into()));
        }
        let proto = &self.program.functions[func_id as usize];
        let callee_regs = (proto.reg_count as usize).max(1);
        let callee_params = proto.param_count as usize;

        let new_base = self.regs.len();
        // Never grow past the pinned capacity (would realloc and dangle a live
        // native window pointer) — throw a catchable RangeError instead.
        if self.regs_would_overflow(new_base + callee_regs) {
            return Err(Thrown("RangeError: Maximum call stack size exceeded".into()));
        }
        self.regs.resize(new_base + callee_regs, Value::UNDEFINED);

        // Register 0 = `this`; parameters at registers 1..1+param_count.
        self.regs[new_base] = this_val;
        let n = (argc as usize).min(callee_params);
        for i in 0..n {
            let v = self.regs[caller_base + arg_base as usize + i];
            self.regs[new_base + 1 + i] = v;
        }
        // Rest parameter: collect args beyond the fixed params into a fresh array.
        if let Some(rreg) = self.program.functions[func_id as usize].rest_reg {
            let extra: Vec<Value> = ((arg_base as usize + callee_params)
                ..(arg_base as usize + argc as usize))
                .map(|i| self.regs[caller_base + i])
                .collect();
            let arr = Value::heap(self.heap.alloc(HeapObj::Array(extra)));
            self.regs[new_base + rreg as usize] = arr;
        }
        // `arguments`: an array of ALL actual args (a function that references it).
        if let Some(areg) = self.program.functions[func_id as usize].arguments_reg {
            let argsv: Vec<Value> = (0..argc as usize)
                .map(|i| self.regs[caller_base + arg_base as usize + i])
                .collect();
            let arr = Value::heap(self.heap.alloc(HeapObj::Array(argsv)));
            self.regs[new_base + areg as usize] = arr;
        }

        let last = self.frames.len() - 1;
        self.frames[last].ip = caller_ip_next;
        self.frames.push(Frame { func: func_id, base: new_base, ip: 0, ret_dst: dst, closure, handlers: Vec::new() });
        Ok(())
    }

    /// Calling a `function*` does NOT run its body — it allocates a suspended
    /// Generator whose DETACHED register window holds `this` + the bound args
    /// (incl. a rest array). Resumed later by `generator_method`.
    fn alloc_generator(&mut self, func_id: u32, closure: u32, this: Value, args: &[Value]) -> Value {
        let proto = &self.program.functions[func_id as usize];
        let reg_count = (proto.reg_count as usize).max(1);
        let param_count = proto.param_count as usize;
        let rest_reg = proto.rest_reg;
        let mut regs = vec![Value::UNDEFINED; reg_count];
        regs[0] = this;
        let n = args.len().min(param_count);
        regs[1..1 + n].copy_from_slice(&args[..n]);
        if let Some(rr) = rest_reg {
            let extra: Vec<Value> = args.get(param_count..).unwrap_or(&[]).to_vec();
            regs[rr as usize] = Value::heap(self.heap.alloc(HeapObj::Array(extra)));
        }
        Value::heap(self.heap.alloc(HeapObj::Generator {
            func: func_id,
            closure,
            state: GenState::Suspended(0),
            regs,
        }))
    }

    /// Resume / query a generator (`gen.next(v)` / `gen.return(v)` / `gen.throw(e)`).
    /// Returns an iterator-result object `{value, done}` (or propagates a throw).
    fn generator_method(
        &mut self,
        idx: u32,
        name: &str,
        args: &[Value],
    ) -> Result<Option<Value>, Thrown> {
        let arg0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        let (state, fid, closure) = match self.heap.get(idx) {
            HeapObj::Generator { state, func, closure, .. } => (*state, *func, *closure),
            _ => return Ok(None),
        };
        match name {
            "return" => {
                // Complete the generator (v1 does not run finally blocks).
                if let HeapObj::Generator { state, regs, .. } = self.heap.get_mut(idx) {
                    *state = GenState::Completed;
                    regs.clear();
                }
                Ok(Some(self.iter_result(arg0, true)))
            }
            "throw" => {
                if matches!(state, GenState::Completed) {
                    return Err(Thrown(self.throw_message(arg0)));
                }
                // v1: complete the generator and surface the throw at the call
                // site (no resume into a `try` inside the body).
                if let HeapObj::Generator { state, regs, .. } = self.heap.get_mut(idx) {
                    *state = GenState::Completed;
                    regs.clear();
                }
                self.pending_throw = Some(arg0);
                Err(Thrown(self.throw_message(arg0)))
            }
            "next" => {
                let resume_ip = match state {
                    GenState::Completed => return Ok(Some(self.iter_result(Value::UNDEFINED, true))),
                    GenState::Running => {
                        return Err(Thrown("TypeError: generator is already running".into()))
                    }
                    GenState::Suspended(ip) => ip,
                };
                // Take the saved window out of the heap object and splice it onto
                // the top of the live register file.
                let saved = match self.heap.get_mut(idx) {
                    HeapObj::Generator { state, regs, .. } => {
                        *state = GenState::Running;
                        std::mem::take(regs)
                    }
                    _ => return Ok(None),
                };
                let reg_count = saved.len();
                let new_base = self.regs.len();
                if self.regs_would_overflow(new_base + reg_count) {
                    if let HeapObj::Generator { state, regs, .. } = self.heap.get_mut(idx) {
                        *state = GenState::Suspended(resume_ip);
                        *regs = saved;
                    }
                    return Err(Thrown("RangeError: Maximum call stack size exceeded".into()));
                }
                self.regs.extend_from_slice(&saved);
                if new_base + reg_count > self.regs_hw {
                    self.regs_hw = new_base + reg_count;
                }
                // First next() runs from ip 0; a later one resumes after the Yield,
                // delivering the sent value into the yield expression's dst.
                let ip = if resume_ip == 0 {
                    0
                } else {
                    if let Instr::Yield { dst, .. } =
                        self.program.functions[fid as usize].code[resume_ip]
                    {
                        self.regs[new_base + dst as usize] = arg0;
                    }
                    resume_ip + 1
                };
                let stop = self.frames.len();
                self.frames.push(Frame {
                    func: fid,
                    base: new_base,
                    ip,
                    ret_dst: 0,
                    closure,
                    handlers: Vec::new(),
                });
                let outcome = self.run_loop(stop);
                if let Some((y, yield_ip)) = self.pending_yield.take() {
                    // Suspended: the window is still live at [new_base..]; park it.
                    let back = self.regs.split_off(new_base);
                    if let HeapObj::Generator { state, regs, .. } = self.heap.get_mut(idx) {
                        *state = GenState::Suspended(yield_ip);
                        *regs = back;
                    }
                    return Ok(Some(self.iter_result(y, false)));
                }
                match outcome {
                    Ok(ret) => {
                        // Returned / fell off the end (pop_frame_with already truncated).
                        if let HeapObj::Generator { state, regs, .. } = self.heap.get_mut(idx) {
                            *state = GenState::Completed;
                            regs.clear();
                        }
                        Ok(Some(self.iter_result(ret, true)))
                    }
                    Err(t) => {
                        self.regs.truncate(new_base);
                        if let HeapObj::Generator { state, regs, .. } = self.heap.get_mut(idx) {
                            *state = GenState::Completed;
                            regs.clear();
                        }
                        Err(t)
                    }
                }
            }
            _ => Ok(None),
        }
    }

    /// Build an iterator-result object `{ value, done }` (insertion order matches
    /// the spec / node).
    fn iter_result(&mut self, value: Value, done: bool) -> Value {
        let mut map = ObjMap::new();
        map.set("value", value);
        map.set("done", Value::bool(done));
        Value::heap(self.heap.alloc(HeapObj::Object(map)))
    }

    // ── promises / microtasks ──

    fn alloc_promise(&mut self) -> u32 {
        self.heap.alloc(HeapObj::Promise {
            state: PromiseState::Pending,
            result: Value::UNDEFINED,
            fulfill: Vec::new(),
            reject: Vec::new(),
            handled: false,
        })
    }

    /// Settle a pending promise (no-op if already settled — the one-shot guard
    /// covers double-resolve / resolve-then-reject / race losers), scheduling its
    /// matching reactions as microtasks.
    fn settle(&mut self, p: u32, state: PromiseState, val: Value) {
        let reactions = match self.heap.get_mut(p) {
            HeapObj::Promise { state: s, result, fulfill, reject, .. } => {
                if *s != PromiseState::Pending {
                    return;
                }
                *s = state;
                *result = val;
                match state {
                    PromiseState::Fulfilled => std::mem::take(fulfill),
                    PromiseState::Rejected => std::mem::take(reject),
                    PromiseState::Pending => return,
                }
            }
            _ => return,
        };
        let kind = if state == PromiseState::Fulfilled {
            ReactionKind::Fulfill
        } else {
            ReactionKind::Reject
        };
        for r in reactions {
            if r.is_async {
                // `dependent` is a suspended async activation; resume it with the
                // value (fulfill) or by throwing the reason in (reject).
                let input = match kind {
                    ReactionKind::Fulfill => Resume::Value(val),
                    ReactionKind::Reject => Resume::Throw(val),
                };
                self.microtasks
                    .push_back(Microtask::AsyncResume { activation: r.dependent, input });
            } else {
                self.microtasks.push_back(Microtask::Reaction {
                    callback: r.callback,
                    arg: val,
                    dependent: r.dependent,
                    kind,
                    finally: r.finally,
                });
            }
        }
    }

    /// JS `[[Resolve]]`: a thenable/Promise value is ADOPTED (p forwards when it
    /// settles); a self-resolution rejects with a TypeError; else fulfill.
    fn resolve(&mut self, p: u32, value: Value) {
        if value.is_heap() {
            if value.heap_index() == p {
                let e = self.alloc_error_from_message("TypeError: Chaining cycle detected for promise");
                self.reject(p, e);
                return;
            }
            if matches!(self.heap.get(value.heap_index()), HeapObj::Promise { .. }) {
                let inner = value.heap_index();
                self.then_internal(inner, Value::UNDEFINED, Value::UNDEFINED, Some(p));
                return;
            }
        }
        self.settle(p, PromiseState::Fulfilled, value);
    }

    fn reject(&mut self, p: u32, reason: Value) {
        self.settle(p, PromiseState::Rejected, reason);
    }

    /// Register reactions on `p` (creating/reusing the dependent promise `into`),
    /// or schedule a microtask immediately if `p` is already settled. Returns the
    /// dependent promise's heap index. The basis of `.then`/`.catch`/`.finally`
    /// and of internal promise adoption.
    fn then_internal(&mut self, p: u32, on_f: Value, on_r: Value, into: Option<u32>) -> u32 {
        let dep = into.unwrap_or_else(|| self.alloc_promise());
        let (state, result) = match self.heap.get(p) {
            HeapObj::Promise { state, result, .. } => (*state, *result),
            _ => return dep,
        };
        match state {
            PromiseState::Pending => {
                if let HeapObj::Promise { fulfill, reject, handled, .. } = self.heap.get_mut(p) {
                    fulfill.push(Reaction { callback: on_f, dependent: dep, finally: false, is_async: false });
                    reject.push(Reaction { callback: on_r, dependent: dep, finally: false, is_async: false });
                    if !on_r.is_undefined() {
                        *handled = true;
                    }
                }
            }
            PromiseState::Fulfilled => {
                self.microtasks.push_back(Microtask::Reaction {
                    callback: on_f,
                    arg: result,
                    dependent: dep,
                    kind: ReactionKind::Fulfill,
                    finally: false,
                });
            }
            PromiseState::Rejected => {
                if let HeapObj::Promise { handled, .. } = self.heap.get_mut(p) {
                    *handled = true;
                }
                self.microtasks.push_back(Microtask::Reaction {
                    callback: on_r,
                    arg: result,
                    dependent: dep,
                    kind: ReactionKind::Reject,
                    finally: false,
                });
            }
        }
        dep
    }

    /// `p.finally(cb)`: register a finally reaction on both settle paths (or
    /// schedule immediately if already settled). Returns the dependent promise.
    fn finally_internal(&mut self, p: u32, cb: Value) -> u32 {
        let dep = self.alloc_promise();
        let (state, result) = match self.heap.get(p) {
            HeapObj::Promise { state, result, .. } => (*state, *result),
            _ => return dep,
        };
        match state {
            PromiseState::Pending => {
                if let HeapObj::Promise { fulfill, reject, .. } = self.heap.get_mut(p) {
                    fulfill.push(Reaction { callback: cb, dependent: dep, finally: true, is_async: false });
                    reject.push(Reaction { callback: cb, dependent: dep, finally: true, is_async: false });
                }
            }
            PromiseState::Fulfilled => self.microtasks.push_back(Microtask::Reaction {
                callback: cb,
                arg: result,
                dependent: dep,
                kind: ReactionKind::Fulfill,
                finally: true,
            }),
            PromiseState::Rejected => self.microtasks.push_back(Microtask::Reaction {
                callback: cb,
                arg: result,
                dependent: dep,
                kind: ReactionKind::Reject,
                finally: true,
            }),
        }
        dep
    }

    // ── async functions ──

    /// Build a suspended `async function` activation and run it synchronously up
    /// to its first `await` (or to completion / a throw). Returns the activation's
    /// result Promise — the value an `async` call evaluates to.
    fn alloc_async(&mut self, func_id: u32, closure: u32, this: Value, args: &[Value]) -> Value {
        let proto = &self.program.functions[func_id as usize];
        let reg_count = (proto.reg_count as usize).max(1);
        let param_count = proto.param_count as usize;
        let rest_reg = proto.rest_reg;
        let mut regs = vec![Value::UNDEFINED; reg_count];
        regs[0] = this;
        let n = args.len().min(param_count);
        regs[1..1 + n].copy_from_slice(&args[..n]);
        if let Some(rr) = rest_reg {
            let extra: Vec<Value> = args.get(param_count..).unwrap_or(&[]).to_vec();
            regs[rr as usize] = Value::heap(self.heap.alloc(HeapObj::Array(extra)));
        }
        let result = self.alloc_promise();
        let idx = self.heap.alloc(HeapObj::AsyncState(Box::new(AsyncStateData {
            func: func_id,
            closure,
            state: GenState::Suspended(0),
            regs,
            result,
            handlers: Vec::new(),
        })));
        // Run from the top until the first await suspends it (or it finishes —
        // settling `result` either way).
        self.drive_async(idx, Resume::Value(Value::UNDEFINED));
        Value::heap(result)
    }

    /// Calling an `async function*` builds a suspended AsyncGenerator (an async
    /// iterator). It does NOT run until the first `.next()`.
    fn alloc_async_generator(&mut self, func_id: u32, closure: u32, this: Value, args: &[Value]) -> Value {
        let proto = &self.program.functions[func_id as usize];
        let reg_count = (proto.reg_count as usize).max(1);
        let param_count = proto.param_count as usize;
        let rest_reg = proto.rest_reg;
        let mut regs = vec![Value::UNDEFINED; reg_count];
        regs[0] = this;
        let n = args.len().min(param_count);
        regs[1..1 + n].copy_from_slice(&args[..n]);
        if let Some(rr) = rest_reg {
            let extra: Vec<Value> = args.get(param_count..).unwrap_or(&[]).to_vec();
            regs[rr as usize] = Value::heap(self.heap.alloc(HeapObj::Array(extra)));
        }
        Value::heap(self.heap.alloc(HeapObj::AsyncGenerator(Box::new(AsyncGenState {
            func: func_id,
            closure,
            state: GenState::Suspended(0),
            regs,
            handlers: Vec::new(),
            queue: Vec::new(),
        }))))
    }

    /// `.next()`/`.return()`/`.throw()` on an async generator. Each returns a
    /// Promise that settles when the body next yields/returns/throws. The result
    /// promise is queued; the driver services the queue FIFO.
    fn async_generator_method(&mut self, idx: u32, name: &str, args: &[Value]) -> Option<Value> {
        let arg0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        let p = self.alloc_promise();
        match name {
            "next" => {
                if let HeapObj::AsyncGenerator(g) = self.heap.get_mut(idx) {
                    g.queue.push(p);
                }
                // Only kick the driver if the generator is idle at a yield (or not
                // started, or completed-to-drain). If it's awaiting a promise or
                // already running, the in-flight resume services the queue when it
                // next yields — resuming now would deliver the wrong value.
                if self.async_gen_should_drive(idx) {
                    self.drive_async_gen(idx, Resume::Value(arg0));
                }
            }
            "return" => {
                // Force completion: settle with { value: arg, done: true }. (v1
                // does not resume `finally` blocks inside the body.)
                if let HeapObj::AsyncGenerator(g) = self.heap.get_mut(idx) {
                    g.state = GenState::Completed;
                    g.regs.clear();
                    g.handlers.clear();
                }
                let r = self.iter_result(arg0, true);
                self.resolve(p, r);
            }
            "throw" => {
                if let HeapObj::AsyncGenerator(g) = self.heap.get_mut(idx) {
                    g.state = GenState::Completed;
                    g.regs.clear();
                    g.handlers.clear();
                }
                self.reject(p, arg0);
            }
            _ => return None,
        }
        Some(Value::heap(p))
    }

    /// Whether a fresh `.next()` should immediately drive the async generator: yes
    /// if it's suspended at a `yield` (or hasn't started, or has completed — to
    /// drain the queued promise as done); NO if it's awaiting a promise or already
    /// running (the in-flight resume will service the queue at its next yield).
    fn async_gen_should_drive(&self, idx: u32) -> bool {
        match self.heap.get(idx) {
            HeapObj::AsyncGenerator(g) => match g.state {
                GenState::Completed => true,
                GenState::Running => false,
                GenState::Suspended(ip) => {
                    ip == 0
                        || matches!(
                            self.program.functions[g.func as usize].code.get(ip),
                            Some(Instr::Yield { .. })
                        )
                }
            },
            _ => false,
        }
    }

    /// Resolve every still-queued `.next()` promise with `{ value: undefined,
    /// done: true }` — called once the async generator has completed.
    fn async_gen_drain_done(&mut self, idx: u32) {
        loop {
            let p = match self.heap.get_mut(idx) {
                HeapObj::AsyncGenerator(g) if !g.queue.is_empty() => g.queue.remove(0),
                _ => break,
            };
            let r = self.iter_result(Value::UNDEFINED, true);
            self.resolve(p, r);
        }
    }

    /// Advance an async generator: run its body until the next `yield` (resolve
    /// the front queued promise with `{value, done:false}`), `await` (park +
    /// subscribe, the promise stays pending), or return/throw (settle + drain).
    /// `input` delivers the `.next()` argument or a settled awaited value/throw.
    fn drive_async_gen(&mut self, idx: u32, input: Resume) {
        let (state, fid, closure) = match self.heap.get(idx) {
            HeapObj::AsyncGenerator(g) => (g.state, g.func, g.closure),
            _ => return,
        };
        let resume_ip = match state {
            GenState::Completed => return self.async_gen_drain_done(idx),
            GenState::Running => return, // re-entrant; will resume when current settles
            GenState::Suspended(ip) => ip,
        };
        // Nothing queued ⇒ idle until a `.next()` arrives.
        if matches!(self.heap.get(idx), HeapObj::AsyncGenerator(g) if g.queue.is_empty()) {
            return;
        }
        let (saved, saved_handlers) = match self.heap.get_mut(idx) {
            HeapObj::AsyncGenerator(g) => {
                g.state = GenState::Running;
                (std::mem::take(&mut g.regs), std::mem::take(&mut g.handlers))
            }
            _ => return,
        };
        let reg_count = saved.len();
        let new_base = self.regs.len();
        if self.regs_would_overflow(new_base + reg_count) {
            if let HeapObj::AsyncGenerator(g) = self.heap.get_mut(idx) {
                g.state = GenState::Completed;
                g.regs.clear();
            }
            let e = self.alloc_error_from_message("RangeError: Maximum call stack size exceeded");
            if let HeapObj::AsyncGenerator(g) = self.heap.get_mut(idx) {
                if !g.queue.is_empty() {
                    let p = g.queue.remove(0);
                    self.reject(p, e);
                }
            }
            self.async_gen_drain_done(idx);
            return;
        }
        self.regs.extend_from_slice(&saved);
        if new_base + reg_count > self.regs_hw {
            self.regs_hw = new_base + reg_count;
        }
        let stop = self.frames.len();
        self.frames.push(Frame {
            func: fid,
            base: new_base,
            ip: 0,
            ret_dst: 0,
            closure,
            handlers: saved_handlers,
        });
        // Resume after the suspending op, delivering the sent/awaited value. The
        // op at `resume_ip` is a Yield (resumed by `.next(v)`) or Await (resumed
        // by a settled promise) — both write the value into the op's `dst`.
        let outcome = if resume_ip == 0 {
            self.run_loop(stop)
        } else {
            match input {
                Resume::Value(v) => {
                    let dst = match self.program.functions[fid as usize].code[resume_ip] {
                        Instr::Yield { dst, .. } => Some(dst),
                        Instr::Await { dst, .. } => Some(dst),
                        _ => None,
                    };
                    if let Some(d) = dst {
                        self.regs[new_base + d as usize] = v;
                    }
                    self.frames[stop].ip = resume_ip + 1;
                    self.run_loop(stop)
                }
                Resume::Throw(e) => {
                    self.pending_throw = Some(e);
                    if self.unwind_to_handler(e, stop) {
                        self.pending_throw = None;
                        self.run_loop(stop)
                    } else {
                        Err(Thrown(String::new()))
                    }
                }
            }
        };
        // Yielded a value → resolve the front queued promise with {value, done:false}.
        if let Some((y, yield_ip)) = self.pending_yield.take() {
            let back = self.regs.split_off(new_base);
            let front = match self.heap.get_mut(idx) {
                HeapObj::AsyncGenerator(g) => {
                    g.state = GenState::Suspended(yield_ip);
                    g.regs = back;
                    (!g.queue.is_empty()).then(|| g.queue.remove(0))
                }
                _ => None,
            };
            if let Some(p) = front {
                let r = self.iter_result(y, false);
                self.resolve(p, r);
            }
            // More `.next()` calls already queued → service the next one now.
            if matches!(self.heap.get(idx), HeapObj::AsyncGenerator(g) if !g.queue.is_empty()) {
                self.drive_async_gen(idx, Resume::Value(Value::UNDEFINED));
            }
            return;
        }
        // Awaited → park and subscribe; the front promise stays pending.
        if let Some((awaited, await_ip, handlers)) = self.pending_await.take() {
            let back = self.regs.split_off(new_base);
            if let HeapObj::AsyncGenerator(g) = self.heap.get_mut(idx) {
                g.state = GenState::Suspended(await_ip);
                g.regs = back;
                g.handlers = handlers;
            }
            let p = self.to_promise(awaited);
            self.settle_subscribe(p, idx);
            return;
        }
        // Returned / fell off the end, or threw.
        match outcome {
            Ok(ret) => {
                let front = match self.heap.get_mut(idx) {
                    HeapObj::AsyncGenerator(g) => {
                        g.state = GenState::Completed;
                        g.regs.clear();
                        g.handlers.clear();
                        (!g.queue.is_empty()).then(|| g.queue.remove(0))
                    }
                    _ => None,
                };
                if let Some(p) = front {
                    let r = self.iter_result(ret, true);
                    self.resolve(p, r);
                }
                self.async_gen_drain_done(idx);
            }
            Err(t) => {
                self.regs.truncate(new_base);
                let reason = self.pending_throw.take().unwrap_or_else(|| {
                    self.alloc_error_from_message(&t.0)
                });
                let front = match self.heap.get_mut(idx) {
                    HeapObj::AsyncGenerator(g) => {
                        g.state = GenState::Completed;
                        g.regs.clear();
                        g.handlers.clear();
                        (!g.queue.is_empty()).then(|| g.queue.remove(0))
                    }
                    _ => None,
                };
                if let Some(p) = front {
                    self.reject(p, reason);
                }
                self.async_gen_drain_done(idx);
            }
        }
    }

    /// `Promise.resolve` as an internal helper: a Promise passes through (identity
    /// preserved); any other value is wrapped in a fulfilled promise. The basis of
    /// awaiting a non-promise (`await 5` still yields a microtask tick).
    fn to_promise(&mut self, v: Value) -> u32 {
        if v.is_heap() {
            if matches!(self.heap.get(v.heap_index()), HeapObj::Promise { .. }) {
                return v.heap_index();
            }
        }
        let p = self.alloc_promise();
        self.resolve(p, v);
        p
    }

    /// Subscribe a suspended async `activation` to promise `p`: when `p` settles,
    /// the activation resumes with the value, or has the reason thrown back in at
    /// the await point. If `p` is already settled, schedule the resume as a
    /// microtask (so `await` always yields to the queue, per spec).
    fn settle_subscribe(&mut self, p: u32, activation: u32) {
        let (state, result) = match self.heap.get(p) {
            HeapObj::Promise { state, result, .. } => (*state, *result),
            _ => {
                self.microtasks.push_back(Microtask::AsyncResume {
                    activation,
                    input: Resume::Value(Value::UNDEFINED),
                });
                return;
            }
        };
        match state {
            PromiseState::Pending => {
                if let HeapObj::Promise { fulfill, reject, handled, .. } = self.heap.get_mut(p) {
                    fulfill.push(Reaction {
                        callback: Value::UNDEFINED,
                        dependent: activation,
                        finally: false,
                        is_async: true,
                    });
                    reject.push(Reaction {
                        callback: Value::UNDEFINED,
                        dependent: activation,
                        finally: false,
                        is_async: true,
                    });
                    *handled = true; // an `await` consumes the rejection
                }
            }
            PromiseState::Fulfilled => self.microtasks.push_back(Microtask::AsyncResume {
                activation,
                input: Resume::Value(result),
            }),
            PromiseState::Rejected => {
                if let HeapObj::Promise { handled, .. } = self.heap.get_mut(p) {
                    *handled = true;
                }
                self.microtasks.push_back(Microtask::AsyncResume {
                    activation,
                    input: Resume::Throw(result),
                });
            }
        }
    }

    // ── Promise combinators ──

    /// `Promise.all/allSettled/race/any(iterable)`. Coerces each input to a
    /// promise and subscribes a native combinator reaction; the shared
    /// `Combinator` state settles the returned promise per the combinator's rule.
    fn promise_combine(&mut self, kind: crate::heap::CombKind, iterable: Value) -> Result<Value, Thrown> {
        use crate::heap::CombKind;
        // GetIterator / iteration abrupt completion → a REJECTED promise, not a
        // synchronous throw (IfAbruptRejectPromise): `Promise.all(1)` rejects with
        // a TypeError rather than throwing out of the call.
        let inputs = match self.iterate_to_vec(iterable) {
            Ok(v) => v,
            Err(Thrown(msg)) => {
                let result = self.alloc_promise();
                let err = self.alloc_error_from_message(&msg);
                self.reject(result, err);
                return Ok(Value::heap(result));
            }
        };
        let total = inputs.len() as u32;
        let result = self.alloc_promise();
        if total == 0 {
            // Empty-iterable terminal cases (race stays pending forever).
            match kind {
                CombKind::All | CombKind::AllSettled => {
                    let arr = Value::heap(self.heap.alloc(HeapObj::Array(Vec::new())));
                    self.resolve(result, arr);
                }
                CombKind::Any => {
                    let e = self.alloc_aggregate_error(Vec::new());
                    self.reject(result, e);
                }
                CombKind::Race => {}
            }
            return Ok(Value::heap(result));
        }
        let comb = self.heap.alloc(HeapObj::Combinator {
            kind,
            results: vec![Value::UNDEFINED; total as usize],
            remaining: total,
            result,
        });
        for (i, inp) in inputs.into_iter().enumerate() {
            let p = self.to_promise(inp);
            let resolver = Value::heap(self.heap.alloc(HeapObj::CombinatorResolver {
                combinator: comb,
                index: i as u32,
            }));
            // Both settle paths route to the resolver (it dispatches on the kind).
            self.then_internal(p, resolver, resolver, None);
        }
        Ok(Value::heap(result))
    }

    /// Perform one combinator step: the input at `index` settled (`kind`) with
    /// `value`. Updates the shared state and settles the combinator's promise
    /// when its rule is met (the one-shot `settle` guard absorbs later inputs).
    fn combinator_step(&mut self, comb: u32, index: u32, kind: ReactionKind, value: Value) {
        use crate::heap::CombKind;
        let (ckind, result) = match self.heap.get(comb) {
            HeapObj::Combinator { kind, result, .. } => (*kind, *result),
            _ => return,
        };
        match (ckind, kind) {
            (CombKind::Race, ReactionKind::Fulfill) => self.resolve(result, value),
            (CombKind::Race, ReactionKind::Reject) => self.reject(result, value),
            (CombKind::All, ReactionKind::Reject) => self.reject(result, value),
            (CombKind::Any, ReactionKind::Fulfill) => self.resolve(result, value),
            (CombKind::All, ReactionKind::Fulfill)
            | (CombKind::Any, ReactionKind::Reject)
            | (CombKind::AllSettled, _) => {
                // Record the per-input outcome and decrement the outstanding count.
                let stored = if ckind == CombKind::AllSettled {
                    self.make_settled_record(kind, value)
                } else {
                    value
                };
                let done = if let HeapObj::Combinator { results, remaining, .. } =
                    self.heap.get_mut(comb)
                {
                    results[index as usize] = stored;
                    *remaining -= 1;
                    *remaining == 0
                } else {
                    false
                };
                if done {
                    let collected = match self.heap.get(comb) {
                        HeapObj::Combinator { results, .. } => results.clone(),
                        _ => Vec::new(),
                    };
                    match ckind {
                        CombKind::Any => {
                            // All inputs rejected → AggregateError of the reasons.
                            let e = self.alloc_aggregate_error(collected);
                            self.reject(result, e);
                        }
                        _ => {
                            let arr = Value::heap(self.heap.alloc(HeapObj::Array(collected)));
                            self.resolve(result, arr);
                        }
                    }
                }
            }
        }
    }

    /// Build a `Promise.allSettled` record: `{status:'fulfilled', value}` or
    /// `{status:'rejected', reason}`.
    fn make_settled_record(&mut self, kind: ReactionKind, value: Value) -> Value {
        let mut map = ObjMap::new();
        match kind {
            ReactionKind::Fulfill => {
                let s = self.alloc_str("fulfilled".to_string());
                map.set("status", s);
                map.set("value", value);
            }
            ReactionKind::Reject => {
                let s = self.alloc_str("rejected".to_string());
                map.set("status", s);
                map.set("reason", value);
            }
        }
        Value::heap(self.heap.alloc(HeapObj::Object(map)))
    }

    /// Build an `AggregateError`-like object `{name, message, errors}` for a
    /// failed `Promise.any`.
    fn alloc_aggregate_error(&mut self, errors: Vec<Value>) -> Value {
        let mut map = ObjMap::new();
        let name = self.alloc_str("AggregateError".to_string());
        map.set("name", name);
        let msg = self.alloc_str("All promises were rejected".to_string());
        map.set("message", msg);
        let errs = Value::heap(self.heap.alloc(HeapObj::Array(errors)));
        map.set("errors", errs);
        Value::heap(self.heap.alloc(HeapObj::Object(map)))
    }

    /// Resume (or start) a suspended async activation `idx` with `input` — the
    /// awaited value (fulfill) or the reason to throw in at the await point
    /// (reject). Runs until the next `await` (re-parks the window + subscribes to
    /// the awaited promise), a normal return (resolves the result Promise), or an
    /// uncaught throw (rejects it). Mirrors `generator_method`'s resume path, but
    /// restores the activation's `try` handlers so a rejection can be caught.
    fn drive_async(&mut self, idx: u32, input: Resume) {
        let (state, fid, closure, result) = match self.heap.get(idx) {
            HeapObj::AsyncState(a) => (a.state, a.func, a.closure, a.result),
            _ => return,
        };
        let resume_ip = match state {
            GenState::Completed | GenState::Running => return,
            GenState::Suspended(ip) => ip,
        };
        // Detach the saved window + handlers, then splice the window onto the top
        // of the live register file.
        let (saved, saved_handlers) = match self.heap.get_mut(idx) {
            HeapObj::AsyncState(a) => {
                a.state = GenState::Running;
                (std::mem::take(&mut a.regs), std::mem::take(&mut a.handlers))
            }
            _ => return,
        };
        let reg_count = saved.len();
        let new_base = self.regs.len();
        if self.regs_would_overflow(new_base + reg_count) {
            // Can't make progress — abandon the activation and reject its result.
            if let HeapObj::AsyncState(a) = self.heap.get_mut(idx) {
                a.state = GenState::Completed;
                a.regs.clear();
                a.handlers.clear();
            }
            let e = self.alloc_error_from_message("RangeError: Maximum call stack size exceeded");
            self.reject(result, e);
            return;
        }
        self.regs.extend_from_slice(&saved);
        if new_base + reg_count > self.regs_hw {
            self.regs_hw = new_base + reg_count;
        }
        let stop = self.frames.len();
        self.frames.push(Frame {
            func: fid,
            base: new_base,
            ip: 0,
            ret_dst: 0,
            closure,
            handlers: saved_handlers,
        });
        // Position the resume point and deliver the awaited value / rejection.
        let outcome = if resume_ip == 0 {
            self.run_loop(stop)
        } else {
            match input {
                Resume::Value(v) => {
                    if let Instr::Await { dst, .. } =
                        self.program.functions[fid as usize].code[resume_ip]
                    {
                        self.regs[new_base + dst as usize] = v;
                    }
                    self.frames[stop].ip = resume_ip + 1;
                    self.run_loop(stop)
                }
                Resume::Throw(e) => {
                    // Throw the rejection in at the await point: unwind to a
                    // handler within this activation (down to `stop`). If caught,
                    // resume at the catch; otherwise it propagates out as the
                    // function's rejection (pending_throw stays set for the Err
                    // arm below).
                    self.pending_throw = Some(e);
                    if self.unwind_to_handler(e, stop) {
                        self.pending_throw = None;
                        self.run_loop(stop)
                    } else {
                        Err(Thrown(String::new()))
                    }
                }
            }
        };
        // Suspended again at an await?
        if let Some((awaited, await_ip, handlers)) = self.pending_await.take() {
            let back = self.regs.split_off(new_base);
            if let HeapObj::AsyncState(a) = self.heap.get_mut(idx) {
                a.state = GenState::Suspended(await_ip);
                a.regs = back;
                a.handlers = handlers;
            }
            let p = self.to_promise(awaited);
            self.settle_subscribe(p, idx);
            return;
        }
        // Otherwise the activation finished — settle `result`.
        match outcome {
            Ok(ret) => {
                if let HeapObj::AsyncState(a) = self.heap.get_mut(idx) {
                    a.state = GenState::Completed;
                    a.regs.clear();
                    a.handlers.clear();
                }
                self.resolve(result, ret);
            }
            Err(_) => {
                let e = match self.pending_throw.take() {
                    Some(v) => v,
                    None => self.alloc_error_from_message("Error"),
                };
                // The unwind already truncated the window; keep regs consistent.
                self.regs.truncate(new_base);
                if let HeapObj::AsyncState(a) = self.heap.get_mut(idx) {
                    a.state = GenState::Completed;
                    a.regs.clear();
                    a.handlers.clear();
                }
                self.reject(result, e);
            }
        }
    }

    /// Run one microtask. A reaction's callback may be a JS function (re-enters
    /// the VM; a throw REJECTS the dependent, never unwinds the drain), a native
    /// BoundResolver, or undefined (pass-through). `AsyncResume` resumes an async
    /// activation (Stage 2).
    fn run_microtask(&mut self, t: Microtask) {
        match t {
            Microtask::Reaction { callback, arg, dependent, kind, finally } => {
                if finally {
                    // Run cb (no args) for its side effect, then forward the
                    // original value/reason — unless cb itself throws.
                    if !callback.is_undefined() {
                        if let Err(_) = self.call_value(callback, Value::UNDEFINED, &[]) {
                            let r = self.pending_throw.take().unwrap_or(Value::UNDEFINED);
                            self.reject(dependent, r);
                            return;
                        }
                    }
                    match kind {
                        ReactionKind::Fulfill => self.resolve(dependent, arg),
                        ReactionKind::Reject => self.reject(dependent, arg),
                    }
                    return;
                }
                if callback.is_undefined() {
                    match kind {
                        ReactionKind::Fulfill => self.resolve(dependent, arg),
                        ReactionKind::Reject => self.reject(dependent, arg),
                    }
                    return;
                }
                if callback.is_heap() {
                    if let HeapObj::BoundResolver { promise, is_reject } =
                        self.heap.get(callback.heap_index())
                    {
                        let (pr, isr) = (*promise, *is_reject);
                        if isr {
                            self.reject(pr, arg);
                        } else {
                            self.resolve(pr, arg);
                        }
                        return;
                    }
                    // A combinator reaction (Promise.all/allSettled/race/any).
                    if let HeapObj::CombinatorResolver { combinator, index } =
                        self.heap.get(callback.heap_index())
                    {
                        let (c, i) = (*combinator, *index);
                        self.combinator_step(c, i, kind, arg);
                        return;
                    }
                }
                match self.call_value(callback, Value::UNDEFINED, &[arg]) {
                    Ok(ret) => self.resolve(dependent, ret),
                    Err(_) => {
                        let r = self.pending_throw.take().unwrap_or(Value::UNDEFINED);
                        self.reject(dependent, r);
                    }
                }
            }
            // Resumes a suspended async activation with the settled value (or by
            // throwing the rejection reason in at the await point). An async
            // generator routes to its own driver.
            Microtask::AsyncResume { activation, input } => {
                if matches!(self.heap.get(activation), HeapObj::AsyncGenerator(_)) {
                    self.drive_async_gen(activation, input);
                } else {
                    self.drive_async(activation, input);
                }
            }
        }
    }

    /// Drain the microtask queue to empty (FIFO; tasks enqueued during the drain
    /// run in the same drain). The whole event loop.
    fn drain_microtasks(&mut self) {
        while let Some(t) = self.microtasks.pop_front() {
            self.run_microtask(t);
        }
    }

    // ── property / index access ──

    fn get_index(&mut self, obj: Value, key: Value) -> Result<Value, Thrown> {
        // A rope must be materialized before random access; no-op (one tag
        // check) for arrays, objects, and already-flat strings.
        if obj.is_heap() {
            self.heap.flatten(obj.heap_index());
        }
        if !obj.is_heap() {
            // null/undefined throw; a number/boolean primitive resolves method-as-value
            // through its prototype (`(5)["toFixed"]`, `true["toString"]`).
            if obj.is_nullish() {
                return Err(Thrown(format!(
                    "TypeError: cannot read property of {}",
                    self.display(obj)
                )));
            }
            let k = self.key_of(key);
            return self.get_prop(obj, &k);
        }
        // A boxed String indexes its wrapped string (chars / length); a boxed
        // Number/Boolean has no index, so computed access goes through the prototype.
        if let HeapObj::Boxed { kind, value } = self.heap.get(obj.heap_index()) {
            let (k, v) = (*kind, *value);
            if k == 0 {
                return self.get_index(v, key);
            }
            let key_s = self.key_of(key);
            return self.get_prop(obj, &key_s);
        }
        // Object / callable / class index access is property access: delegate to
        // `get_prop` so a computed key reaches inherited methods/getters (e.g. a
        // class instance's `obj[Symbol.iterator]`), a callable's `fn["name"]`, and
        // static members (`C["m"]`) — not just own data properties. The built-in
        // instance types (Date/Promise/Weak*) have no integer-index meaning, so all
        // their computed access delegates here too.
        // A TypedArray: a canonical numeric index reads the element; everything
        // else (length/byteLength/methods) delegates to get_prop.
        if matches!(self.heap.get(obj.heap_index()), HeapObj::TypedArray { .. }) {
            if let Some(i) = array_index(key) {
                return Ok(self.ta_element_get(obj.heap_index(), i));
            }
            let k = self.key_of(key);
            return self.get_prop(obj, &k);
        }
        if matches!(
            self.heap.get(obj.heap_index()),
            HeapObj::Object(_)
                | HeapObj::Func(_)
                | HeapObj::Closure { .. }
                | HeapObj::Class(_)
                | HeapObj::Bound { .. }
                | HeapObj::Native(_)
                | HeapObj::Iterator { .. }
                | HeapObj::Date(_)
                | HeapObj::Promise { .. }
                | HeapObj::WeakMap { .. }
                | HeapObj::WeakSet(_)
                | HeapObj::WeakRef(_)
                | HeapObj::FinalizationRegistry { .. }
                | HeapObj::RegExp { .. }
                | HeapObj::Symbol { .. }
                | HeapObj::BigInt(_)
                | HeapObj::ArrayBuffer { .. }
                | HeapObj::DataView { .. }
                | HeapObj::Proxy { .. }
        ) {
            let k = self.key_of(key);
            return self.get_prop(obj, &k);
        }
        match self.heap.get(obj.heap_index()) {
            HeapObj::Array(items) => {
                // Numeric key (incl. an integral double like 1.0 — the JIT region
                // produces f64 indices): direct element access, else undefined.
                if let Some(i) = array_index(key) {
                    if i < items.len() {
                        return Ok(items[i]);
                    }
                    return Ok(Value::UNDEFINED);
                }
                // Non-int key on an array: "length", else resolve via the prototype
                // (a computed method name / `@@iterator`, mirroring dot access).
                let k = self.key_of(key);
                if k == "length" {
                    return Ok(len_value(items.len()));
                }
                self.get_prop(obj, &k)
            }
            HeapObj::Object(map) => {
                let k = self.key_of(key);
                Ok(map.get(&k).unwrap_or(Value::UNDEFINED))
            }
            HeapObj::Str(s) => {
                // Numeric key (incl. an integral double — a JIT region produces
                // f64 indices, and a deopted string index must agree): char at i.
                if let Some(i) = array_index(key) {
                    // A single ASCII char is interned at heap index == its byte
                    // (see Heap::new), so return that slot DIRECTLY — no temporary
                    // 1-char String + re-intern per access (that alloc dominated
                    // `s[i]` scans). O(1) for ASCII (i-th char == i-th byte); a
                    // multi-byte string walks scalars (O(i), correct).
                    if s.ascii {
                        return Ok(match s.bytes.as_bytes().get(i) {
                            Some(&b) => Value::heap(b as u32),
                            None => Value::UNDEFINED,
                        });
                    }
                    match s.bytes.chars().nth(i) {
                        Some(ch) if (ch as u32) < 128 => return Ok(Value::heap(ch as u32)),
                        Some(ch) => {
                            let cs = ch.to_string();
                            return Ok(self.alloc_str(cs));
                        }
                        None => return Ok(Value::UNDEFINED),
                    }
                }
                // Non-numeric key: `s["length"]`, else resolve via String.prototype
                // (a computed method name / `@@iterator`), mirroring dot access.
                let char_len = s.char_len;
                let k = self.key_of(key);
                if k == "length" {
                    return Ok(len_value(char_len));
                }
                self.get_prop(obj, &k)
            }
            // Positional access drives for-of / spread over a Map (the i-th
            // [key, value] entry) and a Set (the i-th value). Insertion order.
            HeapObj::Map { keys, vals } => {
                if let Some(i) = array_index(key) {
                    if i < keys.len() {
                        let (k, v) = (keys[i], vals[i]);
                        return Ok(Value::heap(self.heap.alloc(HeapObj::Array(vec![k, v]))));
                    }
                    return Ok(Value::UNDEFINED);
                }
                // Non-numeric key (`map[Symbol.iterator]`, `map["set"]`): via prototype.
                let k = self.key_of(key);
                self.get_prop(obj, &k)
            }
            HeapObj::Set(items) => {
                if let Some(i) = array_index(key) {
                    if i < items.len() {
                        return Ok(items[i]);
                    }
                    return Ok(Value::UNDEFINED);
                }
                let k = self.key_of(key);
                self.get_prop(obj, &k)
            }
            _ => Ok(Value::UNDEFINED),
        }
    }

    fn set_index(&mut self, obj: Value, key: Value, val: Value) -> Result<(), Thrown> {
        if !obj.is_heap() {
            return Err(Thrown("TypeError: cannot set property of non-object".into()));
        }
        let idx = obj.heap_index();
        // A TypedArray: a canonical numeric index writes the element (coerced +
        // out-of-bounds is a silent no-op); other keys go to set_prop.
        if matches!(self.heap.get(idx), HeapObj::TypedArray { .. }) {
            if let Some(i) = array_index(key) {
                return self.ta_element_set(idx, i, val);
            }
            let k = self.key_of(key);
            return self.set_prop(obj, &k, val);
        }
        // Callable / class computed assignment (`fn["x"] = v`, `C["s"] = v`) is
        // property assignment: route through `set_prop` (honours non-writable
        // `name`/`length`, static setters, function own props).
        if matches!(
            self.heap.get(idx),
            HeapObj::Func(_)
                | HeapObj::Closure { .. }
                | HeapObj::Class(_)
                | HeapObj::Bound { .. }
                | HeapObj::Native(_)
                | HeapObj::Proxy { .. }
        ) {
            let k = self.key_of(key);
            return self.set_prop(obj, &k, val);
        }
        match self.heap.get_mut(idx) {
            HeapObj::Array(items) => {
                // Numeric key (incl. an integral double — the JIT region produces
                // f64 indices): store, growing with `undefined` holes past the end.
                if let Some(i) = array_index(key) {
                    if i >= items.len() {
                        items.resize(i + 1, Value::UNDEFINED);
                    }
                    items[i] = val;
                }
                // Non-numeric / negative / fractional key: no-op in this subset.
                Ok(())
            }
            HeapObj::Object(_) => {
                let k = self.key_of(key);
                let mut added = false;
                if let HeapObj::Object(map) = self.heap.get_mut(idx) {
                    added = map.set(&k, val);
                }
                if added {
                    self.heap.bump_version(idx);
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// `new Date(...)` → epoch ms. 0 args = now; 1 number = ms (time-clipped);
    /// 1 Date = copy; 1 string = parsed; ≥2 = UTC components (month0-based).
    fn date_new_ms(&self, args: &[Value]) -> Result<f64, Thrown> {
        match args.len() {
            0 => Ok(std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as f64)
                .unwrap_or(0.0)),
            1 => {
                let a = args[0];
                if a.is_heap() {
                    if let HeapObj::Date(ms) = self.heap.get(a.heap_index()) {
                        return Ok(*ms);
                    }
                    if matches!(self.heap.get(a.heap_index()), HeapObj::Str(_) | HeapObj::Cons { .. }) {
                        let s = self.heap.str_cow(a.heap_index()).unwrap().into_owned();
                        return Ok(parse_date(&s));
                    }
                }
                Ok(time_clip(self.to_number(a)?))
            }
            _ => {
                let mut comp = [0i64, 0, 1, 0, 0, 0, 0]; // y, mo0, day, h, mi, s, ms
                for (i, &v) in args.iter().enumerate().take(7) {
                    let n = self.to_number(v)?;
                    if n.is_nan() {
                        return Ok(f64::NAN);
                    }
                    comp[i] = n as i64;
                }
                comp[0] = legacy_year(comp[0]);
                Ok(time_clip(ms_from_utc(comp[0], comp[1], comp[2], comp[3], comp[4], comp[5], comp[6])))
            }
        }
    }

    /// `Date.UTC(year, month0, …)` → epoch ms (NaN with no args / a NaN field).
    fn date_utc_ms(&self, args: &[Value]) -> Result<f64, Thrown> {
        if args.is_empty() {
            return Ok(f64::NAN);
        }
        let mut comp = [0i64, 0, 1, 0, 0, 0, 0];
        for (i, &v) in args.iter().enumerate().take(7) {
            let n = self.to_number(v)?;
            if n.is_nan() {
                return Ok(f64::NAN);
            }
            comp[i] = n as i64;
        }
        comp[0] = legacy_year(comp[0]);
        Ok(time_clip(ms_from_utc(comp[0], comp[1], comp[2], comp[3], comp[4], comp[5], comp[6])))
    }

    /// Dispatch a method on a `Date` receiver (`idx` is its heap index). All
    /// getters/setters are UTC. Returns `Ok(None)` if `name` isn't a Date method.
    fn date_method(&mut self, idx: u32, name: &str, args: &[Value]) -> Result<Option<Value>, Thrown> {
        let ms = match self.heap.get(idx) {
            HeapObj::Date(m) => *m,
            _ => return Ok(None),
        };
        let p = date_parts(ms); // (year, month0, day, hour, min, sec, ms, weekday)
        let field = |v: i64| if ms.is_nan() { Value::num(f64::NAN) } else { Value::num(v as f64) };
        let r = match name {
            "getTime" | "valueOf" => Value::num(ms),
            "getFullYear" | "getUTCFullYear" => field(p.0),
            "getMonth" | "getUTCMonth" => field(p.1),
            "getDate" | "getUTCDate" => field(p.2),
            "getHours" | "getUTCHours" => field(p.3),
            "getMinutes" | "getUTCMinutes" => field(p.4),
            "getSeconds" | "getUTCSeconds" => field(p.5),
            "getMilliseconds" | "getUTCMilliseconds" => field(p.6),
            "getDay" | "getUTCDay" => field(p.7),
            "getTimezoneOffset" => Value::num(if ms.is_nan() { f64::NAN } else { 0.0 }),
            "toISOString" => {
                if ms.is_nan() {
                    return Err(Thrown("RangeError: Invalid time value".into()));
                }
                self.alloc_str(date_to_iso(ms))
            }
            "toJSON" => {
                if ms.is_nan() {
                    Value::NULL
                } else {
                    self.alloc_str(date_to_iso(ms))
                }
            }
            // Simplified: ISO (node's local/tz-formatted strings aren't matched).
            // toGMTString is a legacy (Annex B) alias of toUTCString.
            "toString" | "toUTCString" | "toGMTString" | "toDateString" | "toTimeString"
            | "toLocaleString" | "toLocaleDateString" | "toLocaleTimeString" => {
                if ms.is_nan() {
                    self.alloc_str("Invalid Date".to_string())
                } else {
                    self.alloc_str(date_to_iso(ms))
                }
            }
            // Legacy (Annex B): getYear = full year - 1900; setYear maps 0..99 to 19xx.
            "getYear" => field(p.0 - 1900),
            "setYear" => {
                let y = match args.first() {
                    Some(&v) => self.to_number(v)?,
                    None => f64::NAN,
                };
                if y.is_nan() {
                    if let HeapObj::Date(m) = self.heap.get_mut(idx) {
                        *m = f64::NAN;
                    }
                    Value::num(f64::NAN)
                } else {
                    let yi = y as i64;
                    let full = if (0..=99).contains(&yi) { 1900 + yi } else { yi };
                    self.date_set(idx, &p, &[Value::num(full as f64)], 0)?
                }
            }
            "setTime" => {
                let n = match args.first() {
                    Some(&v) => time_clip(self.to_number(v)?),
                    None => f64::NAN,
                };
                if let HeapObj::Date(m) = self.heap.get_mut(idx) {
                    *m = n;
                }
                Value::num(n)
            }
            "setFullYear" | "setUTCFullYear" => self.date_set(idx, &p, args, 0)?,
            "setMonth" | "setUTCMonth" => self.date_set(idx, &p, args, 1)?,
            "setDate" | "setUTCDate" => self.date_set(idx, &p, args, 2)?,
            "setHours" | "setUTCHours" => self.date_set(idx, &p, args, 3)?,
            "setMinutes" | "setUTCMinutes" => self.date_set(idx, &p, args, 4)?,
            "setSeconds" | "setUTCSeconds" => self.date_set(idx, &p, args, 5)?,
            "setMilliseconds" | "setUTCMilliseconds" => self.date_set(idx, &p, args, 6)?,
            _ => return Ok(None),
        };
        Ok(Some(r))
    }

    /// A Date setter starting at component `start` (0=year … 6=ms): overwrite that
    /// field and the following ones from `args`, recompute, store, return the new ms.
    fn date_set(
        &mut self,
        idx: u32,
        p: &(i64, i64, i64, i64, i64, i64, i64, i64),
        args: &[Value],
        start: usize,
    ) -> Result<Value, Thrown> {
        let mut comp = [p.0, p.1, p.2, p.3, p.4, p.5, p.6];
        let mut any_nan = false;
        for (i, &v) in args.iter().enumerate() {
            if start + i >= 7 {
                break;
            }
            let n = self.to_number(v)?;
            if n.is_nan() {
                any_nan = true;
            }
            comp[start + i] = n as i64;
        }
        let ms = if any_nan {
            f64::NAN
        } else {
            time_clip(ms_from_utc(comp[0], comp[1], comp[2], comp[3], comp[4], comp[5], comp[6]))
        };
        if let HeapObj::Date(m) = self.heap.get_mut(idx) {
            *m = ms;
        }
        Ok(Value::num(ms))
    }

    /// The `.prototype` object of a function/class value — lazily created and
    /// cached so it has stable identity (`C.prototype === C.prototype`). A class's
    /// prototype carries its OWN methods plus a `constructor` back-reference; a
    /// plain function's prototype just has `constructor`. `None` for non-callables
    /// (a plain object / array / instance has no `.prototype`).
    fn prototype_of(&mut self, obj: Value) -> Option<Value> {
        if !obj.is_heap() {
            return None;
        }
        let idx = obj.heap_index();
        // A built-in constructor global (Map/Set/Date/…) keeps its .prototype as an
        // own property; return it so `x instanceof Map` (instanceof_via_proto) works.
        if let HeapObj::Object(m) = self.heap.get(idx) {
            if m.is_ctor {
                return m.get("prototype");
            }
        }
        if !matches!(
            self.heap.get(idx),
            HeapObj::Func(_) | HeapObj::Closure { .. } | HeapObj::Class(_)
        ) {
            return None;
        }
        if let Some(&p) = self.prototypes.get(&idx) {
            return Some(Value::heap(p));
        }
        // Collect own methods first (ends the immutable heap borrow before alloc).
        let methods: Vec<(String, Value)> = match self.heap.get(idx) {
            HeapObj::Class(c) => c.methods.iter().map(|(k, v)| (k.clone(), *v)).collect(),
            _ => Vec::new(),
        };
        // Methods and the constructor back-reference are NON-enumerable
        // (writable + configurable), matching ES `class`/function semantics that
        // test262's verifyProperty checks.
        let nonenum =
            PropAttr { writable: true, enumerable: false, configurable: true, accessor: false, setter: Value::UNDEFINED };
        let mut map = ObjMap::new();
        for (k, v) in &methods {
            map.define(k, *v, nonenum);
        }
        map.define("constructor", obj, nonenum);
        let p = self.heap.alloc(HeapObj::Object(map));
        self.prototypes.insert(idx, p);
        Some(Value::heap(p))
    }

    /// Build the built-in global object graph (Object/Array/Function + their
    /// prototypes, with methods as native function VALUES) and inject it into the
    /// global slots the compiler reserved for those free identifiers. Makes
    /// `Array.isArray`, `Object.defineProperty`, `Function.prototype.call`, etc.
    /// usable as first-class values (what the test262 harness binds).
    fn setup_globals(&mut self) {
        use native::*;
        // A built-in method property: a native function, non-enumerable but
        // writable + configurable (matching built-in method descriptors).
        let method_attr = PropAttr {
            writable: true,
            enumerable: false,
            configurable: true,
            accessor: false,
            setter: Value::UNDEFINED,
        };
        let proto_attr = PropAttr {
            writable: false,
            enumerable: false,
            configurable: false,
            accessor: false,
            setter: Value::UNDEFINED,
        };
        let mut build = |vm: &mut Self, methods: &[(&str, u16)], protolink: Option<u32>| -> u32 {
            let mut m = ObjMap::new();
            for &(name, id) in methods {
                let nv = Value::heap(vm.heap.alloc(HeapObj::Native(id)));
                m.define(name, nv, method_attr);
            }
            if let Some(p) = protolink {
                m.define("prototype", Value::heap(p), proto_attr);
                // A global built WITH a .prototype is a constructor (Object/Array/Map/…);
                // a namespace (Reflect/Math/JSON, protolink None) is not.
                m.is_ctor = true;
            }
            vm.heap.alloc(HeapObj::Object(m))
        };
        // Prototypes.
        self.obj_proto = build(
            self,
            &[
                ("hasOwnProperty", PROTO_HAS_OWN),
                ("propertyIsEnumerable", PROTO_PROP_ENUM),
                ("isPrototypeOf", PROTO_IS_PROTO_OF),
                ("valueOf", PROTO_VALUE_OF),
                ("toString", PROTO_TO_STRING),
                ("toLocaleString", PROTO_TO_LOCALE_STRING),
            ],
            None,
        );
        self.fn_proto = build(
            self,
            &[("call", FN_CALL), ("apply", FN_APPLY), ("bind", FN_BIND)],
            None,
        );
        // Build the Array.prototype / String.prototype method lists from the
        // PROTO_METHODS table (id = PROTO_METHOD_BASE + index), so methods are
        // first-class values (`Array.prototype.map.call(arr, fn)`).
        let mut arr_methods: Vec<(&str, u16)> = vec![("join", ARR_JOIN), ("push", ARR_PUSH)];
        let mut str_methods: Vec<(&str, u16)> = Vec::new();
        let mut num_methods: Vec<(&str, u16)> = Vec::new();
        let mut set_methods: Vec<(&str, u16)> = Vec::new();
        let mut map_methods: Vec<(&str, u16)> = Vec::new();
        let mut bool_methods: Vec<(&str, u16)> = Vec::new();
        let mut date_methods: Vec<(&str, u16)> = Vec::new();
        let mut promise_methods: Vec<(&str, u16)> = Vec::new();
        for (i, &(name, kind, _len)) in native::PROTO_METHODS.iter().enumerate() {
            let id = native::PROTO_METHOD_BASE + i as u16;
            match kind {
                0 => arr_methods.push((name, id)),
                1 => str_methods.push((name, id)),
                2 => num_methods.push((name, id)),
                3 => set_methods.push((name, id)),
                4 => map_methods.push((name, id)),
                5 => bool_methods.push((name, id)),
                6 => date_methods.push((name, id)),
                _ => promise_methods.push((name, id)), // kind 7
            }
        }
        self.arr_proto = build(self, &arr_methods, None);
        self.str_proto = build(self, &str_methods, None);
        let str_proto = self.str_proto;
        let num_proto = build(self, &num_methods, None);
        let set_proto = build(self, &set_methods, None);
        let map_proto = build(self, &map_methods, None);
        let bool_proto = build(self, &bool_methods, None);
        let date_proto = build(self, &date_methods, None);
        let promise_proto = build(self, &promise_methods, None);
        // Store the proto indices so Map/Set/Date/Promise instances can delegate
        // method-as-value access to them (get_prop), mirroring arr_proto/str_proto.
        self.set_proto = set_proto;
        self.map_proto = map_proto;
        self.date_proto = date_proto;
        self.promise_proto = promise_proto;
        self.num_proto = num_proto;
        self.bool_proto = bool_proto;
        // Constructors.
        let obj_proto = self.obj_proto;
        let arr_proto = self.arr_proto;
        let fn_proto = self.fn_proto;
        let object_ctor = build(
            self,
            &[
                ("defineProperty", OBJ_DEFINE_PROPERTY),
                ("defineProperties", OBJ_DEFINE_PROPERTIES),
                ("getOwnPropertyDescriptor", OBJ_GET_OWN_DESC),
                ("getOwnPropertyNames", OBJ_GET_OWN_NAMES),
                ("getPrototypeOf", OBJ_GET_PROTO),
                ("keys", OBJ_KEYS),
                ("values", OBJ_VALUES),
                ("entries", OBJ_ENTRIES),
                ("assign", OBJ_ASSIGN),
                ("create", OBJ_CREATE),
                ("is", OBJ_IS),
                ("hasOwn", OBJ_HAS_OWN),
                ("fromEntries", OBJ_FROM_ENTRIES),
                ("setPrototypeOf", OBJ_SET_PROTO_OF),
                ("getOwnPropertySymbols", OBJ_GET_OWN_SYMBOLS),
                ("getOwnPropertyDescriptors", OBJ_GET_OWN_DESCS),
                ("freeze", OBJ_FREEZE),
                ("isFrozen", OBJ_IS_FROZEN),
                ("seal", OBJ_SEAL),
                ("isSealed", OBJ_IS_SEALED),
                ("preventExtensions", OBJ_PREVENT_EXT),
                ("isExtensible", OBJ_IS_EXT),
                ("groupBy", OBJ_GROUP_BY),
            ],
            Some(obj_proto),
        );
        let array_ctor = build(self, &[("isArray", ARR_IS_ARRAY), ("from", ARR_FROM), ("of", ARR_OF)], Some(arr_proto));
        let function_ctor = build(self, &[], Some(fn_proto));
        let string_ctor = build(
            self,
            &[
                ("fromCharCode", STR_FROM_CHAR_CODE),
                ("fromCodePoint", STR_FROM_CODE_POINT),
                ("raw", STR_RAW),
            ],
            Some(str_proto),
        );
        // `Number`: the numeric constants (non-writable/enumerable/configurable per
        // spec) + Number.prototype. `Number(x)` / `Number.isInteger(x)` etc. are
        // call-site lowered (GlobalFn), so only the value-level shape is built here.
        let number_ctor = {
            let mut m = ObjMap::new();
            let consts: &[(&str, f64)] = &[
                ("MAX_SAFE_INTEGER", 9007199254740991.0),
                ("MIN_SAFE_INTEGER", -9007199254740991.0),
                ("MAX_VALUE", f64::MAX),
                ("MIN_VALUE", 5e-324),
                ("EPSILON", f64::EPSILON),
                ("POSITIVE_INFINITY", f64::INFINITY),
                ("NEGATIVE_INFINITY", f64::NEG_INFINITY),
                ("NaN", f64::NAN),
            ];
            for &(n, v) in consts {
                m.define(n, Value::num(v), proto_attr);
            }
            // Static methods as first-class values (the call form is StaticFn/GlobalFn).
            for &(name, id) in &[
                ("isInteger", NUM_IS_INTEGER),
                ("isNaN", NUM_IS_NAN),
                ("isFinite", NUM_IS_FINITE),
                ("isSafeInteger", NUM_IS_SAFE_INTEGER),
                ("parseInt", GLOBAL_PARSE_INT),
                ("parseFloat", GLOBAL_PARSE_FLOAT),
            ] {
                let nv = Value::heap(self.heap.alloc(HeapObj::Native(id)));
                m.define(name, nv, method_attr);
            }
            m.define("prototype", Value::heap(num_proto), proto_attr);
            m.is_ctor = true; // Number is a constructor (typeof "function").
            self.heap.alloc(HeapObj::Object(m))
        };
        // Set / Map / Boolean / Date globals: their .prototype (construction is
        // compile-lowered to NewSet / NewMap / DateNew; value-level shape here).
        let set_ctor = build(self, &[], Some(set_proto));
        let map_ctor = build(self, &[("groupBy", MAP_GROUP_BY)], Some(map_proto));
        let boolean_ctor = build(self, &[], Some(bool_proto));
        let date_ctor = build(
            self,
            &[("now", DATE_NOW), ("parse", DATE_PARSE), ("UTC", DATE_UTC)],
            Some(date_proto),
        );
        // Promise global: static combinators + Promise.prototype. `new Promise`
        // is compile-lowered to NewPromise.
        let promise_ctor = build(
            self,
            &[
                ("resolve", PROMISE_RESOLVE),
                ("reject", PROMISE_REJECT),
                ("all", PROMISE_ALL),
                ("allSettled", PROMISE_ALLSETTLED),
                ("race", PROMISE_RACE),
                ("any", PROMISE_ANY),
                // NOTE: withResolvers is implemented (PROMISE_WITH_RESOLVERS handler)
                // but NOT exposed: without Promise-subclassing it can't validate
                // `this` is a constructor, so the ctx-non-ctor/ctx-non-object tests
                // (which passed via property-access-on-undefined) net-regress. Re-expose
                // once `this`-as-constructor / NewPromiseCapability(C) is modelled.
            ],
            Some(promise_proto),
        );
        // `Reflect`: a namespace object (no .prototype) of static methods that
        // mostly delegate to the existing property machinery.
        let reflect_ctor = build(
            self,
            &[
                ("apply", REFLECT_APPLY),
                ("construct", REFLECT_CONSTRUCT),
                ("get", REFLECT_GET),
                ("set", REFLECT_SET),
                ("has", REFLECT_HAS),
                ("deleteProperty", REFLECT_DELETE),
                ("ownKeys", REFLECT_OWN_KEYS),
                ("getPrototypeOf", REFLECT_GET_PROTO),
                ("setPrototypeOf", REFLECT_SET_PROTO),
                ("defineProperty", REFLECT_DEFINE),
                ("getOwnPropertyDescriptor", REFLECT_GET_OWN_DESC),
                ("isExtensible", REFLECT_IS_EXT),
                ("preventExtensions", REFLECT_PREVENT_EXT),
            ],
            None,
        );
        // `WeakMap`/`WeakSet`: distinct prototypes (get/set/has/delete, add/has/delete
        // — deliberately NO size/keys/values/iteration). Construction is compile-lowered
        // to NewWeakMap/NewWeakSet.
        let weakmap_proto = build(
            self,
            &[("get", WM_GET), ("set", WM_SET), ("has", WM_HAS), ("delete", WM_DELETE)],
            None,
        );
        let weakset_proto = build(self, &[("add", WS_ADD), ("has", WS_HAS), ("delete", WS_DELETE)], None);
        let weakref_proto = build(self, &[("deref", WR_DEREF)], None);
        let finreg_proto = build(self, &[("register", FR_REGISTER), ("unregister", FR_UNREGISTER)], None);
        // %ArrayIteratorPrototype% (next + @@iterator). Array entries/keys/values
        // iterators delegate here.
        let array_iter_proto = build(self, &[("next", ITER_NEXT), ("@@iterator", ITER_SELF)], None);
        self.array_iter_proto = array_iter_proto;
        // Distinct %MapIteratorPrototype% / %SetIteratorPrototype% (same natives,
        // different identity so getPrototypeOf discriminates them).
        self.map_iter_proto = build(self, &[("next", ITER_NEXT), ("@@iterator", ITER_SELF)], None);
        self.set_iter_proto = build(self, &[("next", ITER_NEXT), ("@@iterator", ITER_SELF)], None);
        // Default @@iterator: Map → entries, Set → values (alias to the same fn).
        let map_entries = match self.heap.get(map_proto) {
            HeapObj::Object(m) => m.get("entries"),
            _ => None,
        };
        let set_values = match self.heap.get(set_proto) {
            HeapObj::Object(m) => m.get("values"),
            _ => None,
        };
        let iter_attr = PropAttr {
            writable: true,
            enumerable: false,
            configurable: true,
            accessor: false,
            setter: Value::UNDEFINED,
        };
        if let Some(v) = map_entries {
            if let HeapObj::Object(m) = self.heap.get_mut(map_proto) {
                m.define("@@iterator", v, iter_attr);
            }
        }
        if let Some(v) = set_values {
            if let HeapObj::Object(m) = self.heap.get_mut(set_proto) {
                m.define("@@iterator", v, iter_attr);
            }
        }
        // `Array.prototype[Symbol.iterator]` IS `Array.prototype.values` (same fn).
        let values_fn = match self.heap.get(self.arr_proto) {
            HeapObj::Object(m) => m.get("values"),
            _ => None,
        };
        if let Some(vf) = values_fn {
            let attr = PropAttr {
                writable: true,
                enumerable: false,
                configurable: true,
                accessor: false,
                setter: Value::UNDEFINED,
            };
            if let HeapObj::Object(m) = self.heap.get_mut(self.arr_proto) {
                m.define("@@iterator", vf, attr);
            }
        }
        self.weakmap_proto = weakmap_proto;
        self.weakset_proto = weakset_proto;
        self.weakref_proto = weakref_proto;
        self.finreg_proto = finreg_proto;
        let weakmap_ctor = build(self, &[], Some(weakmap_proto));
        let weakset_ctor = build(self, &[], Some(weakset_proto));
        let weakref_ctor = build(self, &[], Some(weakref_proto));
        let finreg_ctor = build(self, &[], Some(finreg_proto));
        // Error hierarchy: `Error` + the 7 native subtypes. Each is a constructor
        // VALUE (is_ctor object with a `.prototype`) whose prototype carries own
        // `name`/`message`/`constructor` (+ `Error.prototype.toString`). Every error
        // instance — `new TypeError(x)` AND internal VM throws — links here via
        // `proto_of`, so `e.constructor === TypeError`, `e.name`, `e.toString()`,
        // and `e instanceof <ctor value>` all resolve through the chain.
        {
            // Error.prototype (chains to Object.prototype) carries toString.
            let err_proto = build(self, &[("toString", ERROR_TO_STRING)], None);
            self.proto_of.insert(err_proto, Value::heap(obj_proto));
            self.error_protos[0] = err_proto;
            // Subtype prototypes chain to Error.prototype.
            for k in 1..8usize {
                let p = build(self, &[], None);
                self.proto_of.insert(p, Value::heap(err_proto));
                self.error_protos[k] = p;
            }
            // Constructor function values (is_ctor, with a non-writable `.prototype`).
            for k in 0..8usize {
                let proto = self.error_protos[k];
                self.error_ctors[k] = build(self, &[], Some(proto));
            }
            // `Object.getPrototypeOf(TypeError) === Error`; `Error` → Function.prototype.
            let err_ctor = self.error_ctors[0];
            self.proto_of.insert(err_ctor, Value::heap(fn_proto));
            for k in 1..8usize {
                let c = self.error_ctors[k];
                self.proto_of.insert(c, Value::heap(err_ctor));
            }
            // Each prototype's own name/message/constructor (writable, non-enum,
            // configurable — matching the spec's Error.prototype descriptors).
            for k in 0..8usize {
                let name_v = self.alloc_str(native::ERROR_NAMES[k].to_string());
                let empty_v = self.alloc_str(String::new());
                let ctor_v = Value::heap(self.error_ctors[k]);
                let proto = self.error_protos[k];
                if let HeapObj::Object(m) = self.heap.get_mut(proto) {
                    m.define("name", name_v, method_attr);
                    m.define("message", empty_v, method_attr);
                    m.define("constructor", ctor_v, method_attr);
                }
            }
        }
        // `Symbol`: a callable-but-NOT-constructable function object (typeof
        // "function" via the type_of special case; `new Symbol()` throws because
        // it's not is_ctor). The well-known symbols (iterator/toPrimitive/…) are
        // real Symbol VALUES whose property-key form is the engine's `@@`-prefixed
        // key, so symbol-keyed access and iteration use one unified mechanism.
        {
            let symbol_proto = build(
                self,
                &[("toString", SYMBOL_TO_STRING), ("valueOf", SYMBOL_VALUE_OF)],
                None,
            );
            self.proto_of.insert(symbol_proto, Value::heap(obj_proto));
            self.symbol_proto = symbol_proto;
            let fn_attr = PropAttr {
                writable: false,
                enumerable: false,
                configurable: true,
                accessor: false,
                setter: Value::UNDEFINED,
            };
            let for_v = Value::heap(self.heap.alloc(HeapObj::Native(SYMBOL_FOR)));
            let keyfor_v = Value::heap(self.heap.alloc(HeapObj::Native(SYMBOL_KEY_FOR)));
            let name_v = self.alloc_str("Symbol".to_string());
            let mut m = ObjMap::new();
            m.define("prototype", Value::heap(symbol_proto), proto_attr);
            m.define("for", for_v, method_attr);
            m.define("keyFor", keyfor_v, method_attr);
            m.define("name", name_v, fn_attr);
            m.define("length", Value::num(0.0), fn_attr);
            let symbol_ctor = self.heap.alloc(HeapObj::Object(m));
            self.symbol_ctor = symbol_ctor;
            // Symbol.prototype.constructor === Symbol.
            if let HeapObj::Object(p) = self.heap.get_mut(symbol_proto) {
                p.define("constructor", Value::heap(symbol_ctor), method_attr);
            }
            // Well-known symbols: real symbols (non-writable/enum/configurable own
            // props of Symbol), each with its fixed `@@`-prefixed key + description.
            for &(jsname, prop_key) in native::WELL_KNOWN_SYMBOLS {
                let desc = self.alloc_str(format!("Symbol.{jsname}"));
                let sym = self.make_named_symbol(desc, prop_key);
                if let HeapObj::Object(mm) = self.heap.get_mut(symbol_ctor) {
                    mm.define(jsname, sym, proto_attr);
                }
            }
        }
        // `BigInt`: callable-but-NOT-constructable (typeof "function"; new BigInt()
        // throws). BigInt(x) converts (compile-lowered to BigIntFrom); asIntN/asUintN
        // are statics; toString/valueOf on BigInt.prototype.
        {
            let bigint_proto = build(
                self,
                &[("toString", BIGINT_TO_STRING), ("valueOf", BIGINT_VALUE_OF)],
                None,
            );
            self.proto_of.insert(bigint_proto, Value::heap(obj_proto));
            self.bigint_proto = bigint_proto;
            let fn_attr = PropAttr {
                writable: false,
                enumerable: false,
                configurable: true,
                accessor: false,
                setter: Value::UNDEFINED,
            };
            let asintn = Value::heap(self.heap.alloc(HeapObj::Native(BIGINT_AS_INTN)));
            let asuintn = Value::heap(self.heap.alloc(HeapObj::Native(BIGINT_AS_UINTN)));
            let name_v = self.alloc_str("BigInt".to_string());
            let mut m = ObjMap::new();
            m.define("prototype", Value::heap(bigint_proto), proto_attr);
            m.define("asIntN", asintn, method_attr);
            m.define("asUintN", asuintn, method_attr);
            m.define("name", name_v, fn_attr);
            m.define("length", Value::num(1.0), fn_attr);
            let bigint_ctor = self.heap.alloc(HeapObj::Object(m));
            self.bigint_ctor = bigint_ctor;
            if let HeapObj::Object(p) = self.heap.get_mut(bigint_proto) {
                p.define("constructor", Value::heap(bigint_ctor), method_attr);
            }
        }
        // `RegExp` (constructable; `new RegExp`/`/x/` literals lower to NewRegExp).
        // Instance accessors (source/flags/lastIndex/…) are computed in get_prop;
        // the prototype carries test/exec/toString.
        {
            let regexp_proto = build(
                self,
                &[("test", REGEXP_TEST), ("exec", REGEXP_EXEC), ("toString", REGEXP_TO_STRING)],
                None,
            );
            self.proto_of.insert(regexp_proto, Value::heap(obj_proto));
            self.regexp_proto = regexp_proto;
            let regexp_ctor = build(self, &[], Some(regexp_proto));
            self.regexp_ctor = regexp_ctor;
            if let HeapObj::Object(p) = self.heap.get_mut(regexp_proto) {
                p.define("constructor", Value::heap(regexp_ctor), method_attr);
            }
        }
        // TypedArrays: the %TypedArray% abstract base (its prototype holds the shared
        // methods), the 11 concrete kinds inheriting from it, plus ArrayBuffer and
        // DataView. `Object.getPrototypeOf(Int8Array) === %TypedArray%` and
        // `Int8Array.prototype.__proto__ === %TypedArray%.prototype`.
        {
            let fn_attr = PropAttr {
                writable: false,
                enumerable: false,
                configurable: true,
                accessor: false,
                setter: Value::UNDEFINED,
            };
            let ta_methods: Vec<(&str, u16)> = native::TA_PROTO_METHODS
                .iter()
                .enumerate()
                .map(|(i, &n)| (n, native::TA_METHOD_BASE + i as u16))
                .collect();
            let ta_base_proto = build(self, &ta_methods, None);
            self.proto_of.insert(ta_base_proto, Value::heap(obj_proto));
            self.ta_base_proto = ta_base_proto;
            let ta_base_ctor = build(self, &[], Some(ta_base_proto));
            self.ta_base_ctor = ta_base_ctor;
            self.proto_of.insert(ta_base_ctor, Value::heap(fn_proto));
            let tname = self.alloc_str("TypedArray".to_string());
            if let HeapObj::Object(m) = self.heap.get_mut(ta_base_ctor) {
                m.define("name", tname, fn_attr);
                m.define("length", Value::num(0.0), fn_attr);
            }
            if let HeapObj::Object(m) = self.heap.get_mut(ta_base_proto) {
                m.define("constructor", Value::heap(ta_base_ctor), method_attr);
            }
            for k in 0..native::TA_KINDS.len() {
                let size = native::TA_KINDS[k].1;
                let proto = build(self, &[], None);
                self.proto_of.insert(proto, Value::heap(ta_base_proto));
                self.ta_protos[k] = proto;
                let ctor = build(self, &[], Some(proto));
                self.proto_of.insert(ctor, Value::heap(ta_base_ctor));
                self.ta_ctors[k] = ctor;
                if let HeapObj::Object(m) = self.heap.get_mut(proto) {
                    m.define("constructor", Value::heap(ctor), method_attr);
                    m.define("BYTES_PER_ELEMENT", Value::num(size as f64), proto_attr);
                }
                if let HeapObj::Object(m) = self.heap.get_mut(ctor) {
                    m.define("BYTES_PER_ELEMENT", Value::num(size as f64), proto_attr);
                }
            }
            let arraybuffer_proto = build(self, &[("slice", ARRAYBUFFER_SLICE)], None);
            self.proto_of.insert(arraybuffer_proto, Value::heap(obj_proto));
            self.arraybuffer_proto = arraybuffer_proto;
            let arraybuffer_ctor = build(self, &[], Some(arraybuffer_proto));
            self.arraybuffer_ctor = arraybuffer_ctor;
            if let HeapObj::Object(m) = self.heap.get_mut(arraybuffer_proto) {
                m.define("constructor", Value::heap(arraybuffer_ctor), method_attr);
            }
            let dv_methods: Vec<(&str, u16)> = native::DV_PROTO_METHODS
                .iter()
                .enumerate()
                .map(|(i, &n)| (n, native::DV_METHOD_BASE + i as u16))
                .collect();
            let dataview_proto = build(self, &dv_methods, None);
            self.proto_of.insert(dataview_proto, Value::heap(obj_proto));
            self.dataview_proto = dataview_proto;
            // `Proxy`: a constructor with no `.prototype`; `Proxy.revocable` static.
            let revocable = Value::heap(self.heap.alloc(HeapObj::Native(PROXY_REVOCABLE)));
            let mut pm = ObjMap::new();
            pm.define("revocable", revocable, method_attr);
            pm.is_ctor = true;
            self.proxy_ctor = self.heap.alloc(HeapObj::Object(pm));
            // `Temporal` namespace + `Temporal.Duration`.
            let dur_methods: Vec<(&str, u16)> = native::TEMPORAL_DURATION_METHODS
                .iter()
                .enumerate()
                .map(|(i, &n)| (n, native::TEMPORAL_M_BASE + i as u16))
                .collect();
            let duration_proto = build(self, &dur_methods, None);
            self.proto_of.insert(duration_proto, Value::heap(obj_proto));
            self.duration_proto = duration_proto;
            let dfrom = Value::heap(self.heap.alloc(HeapObj::Native(TEMPORAL_DURATION_FROM)));
            let dcompare = Value::heap(self.heap.alloc(HeapObj::Native(TEMPORAL_DURATION_COMPARE)));
            let dname = self.alloc_str("Duration".to_string());
            let dtag = self.alloc_str("Temporal.Duration".to_string());
            let mut dm = ObjMap::new();
            dm.define("prototype", Value::heap(duration_proto), proto_attr);
            dm.define("from", dfrom, method_attr);
            dm.define("compare", dcompare, method_attr);
            dm.define("name", dname, fn_attr);
            dm.define("length", Value::num(0.0), fn_attr);
            dm.is_ctor = true;
            let duration_ctor = self.heap.alloc(HeapObj::Object(dm));
            self.duration_ctor = duration_ctor;
            if let HeapObj::Object(p) = self.heap.get_mut(duration_proto) {
                p.define("constructor", Value::heap(duration_ctor), method_attr);
                p.define("@@toStringTag", dtag, fn_attr);
            }
            // Temporal.PlainDate
            let pd_methods: Vec<(&str, u16)> = native::PLAINDATE_METHODS
                .iter()
                .enumerate()
                .map(|(i, &n)| (n, native::PD_M_BASE + i as u16))
                .collect();
            let plaindate_proto = build(self, &pd_methods, None);
            self.proto_of.insert(plaindate_proto, Value::heap(obj_proto));
            self.plaindate_proto = plaindate_proto;
            let pdfrom = Value::heap(self.heap.alloc(HeapObj::Native(PLAINDATE_FROM)));
            let pdcompare = Value::heap(self.heap.alloc(HeapObj::Native(PLAINDATE_COMPARE)));
            let pdname = self.alloc_str("PlainDate".to_string());
            let pdtag = self.alloc_str("Temporal.PlainDate".to_string());
            let mut pdm = ObjMap::new();
            pdm.define("prototype", Value::heap(plaindate_proto), proto_attr);
            pdm.define("from", pdfrom, method_attr);
            pdm.define("compare", pdcompare, method_attr);
            pdm.define("name", pdname, fn_attr);
            pdm.define("length", Value::num(3.0), fn_attr);
            pdm.is_ctor = true;
            let plaindate_ctor = self.heap.alloc(HeapObj::Object(pdm));
            self.plaindate_ctor = plaindate_ctor;
            if let HeapObj::Object(p) = self.heap.get_mut(plaindate_proto) {
                p.define("constructor", Value::heap(plaindate_ctor), method_attr);
                p.define("@@toStringTag", pdtag, fn_attr);
            }
            // Temporal.PlainTime
            let pt_methods: Vec<(&str, u16)> = native::PLAINTIME_METHODS
                .iter()
                .enumerate()
                .map(|(i, &n)| (n, native::PT_M_BASE + i as u16))
                .collect();
            let plaintime_proto = build(self, &pt_methods, None);
            self.proto_of.insert(plaintime_proto, Value::heap(obj_proto));
            self.plaintime_proto = plaintime_proto;
            let ptfrom = Value::heap(self.heap.alloc(HeapObj::Native(PLAINTIME_FROM)));
            let ptcompare = Value::heap(self.heap.alloc(HeapObj::Native(PLAINTIME_COMPARE)));
            let ptname = self.alloc_str("PlainTime".to_string());
            let pttag = self.alloc_str("Temporal.PlainTime".to_string());
            let mut ptm = ObjMap::new();
            ptm.define("prototype", Value::heap(plaintime_proto), proto_attr);
            ptm.define("from", ptfrom, method_attr);
            ptm.define("compare", ptcompare, method_attr);
            ptm.define("name", ptname, fn_attr);
            ptm.define("length", Value::num(0.0), fn_attr);
            ptm.is_ctor = true;
            let plaintime_ctor = self.heap.alloc(HeapObj::Object(ptm));
            self.plaintime_ctor = plaintime_ctor;
            if let HeapObj::Object(p) = self.heap.get_mut(plaintime_proto) {
                p.define("constructor", Value::heap(plaintime_ctor), method_attr);
                p.define("@@toStringTag", pttag, fn_attr);
            }
            // Temporal.PlainDateTime
            let pdt_methods: Vec<(&str, u16)> = native::PLAINDATETIME_METHODS
                .iter()
                .enumerate()
                .map(|(i, &n)| (n, native::PDT_M_BASE + i as u16))
                .collect();
            let plaindatetime_proto = build(self, &pdt_methods, None);
            self.proto_of.insert(plaindatetime_proto, Value::heap(obj_proto));
            self.plaindatetime_proto = plaindatetime_proto;
            let pdtfrom = Value::heap(self.heap.alloc(HeapObj::Native(PLAINDATETIME_FROM)));
            let pdtcompare = Value::heap(self.heap.alloc(HeapObj::Native(PLAINDATETIME_COMPARE)));
            let pdtname = self.alloc_str("PlainDateTime".to_string());
            let pdttag = self.alloc_str("Temporal.PlainDateTime".to_string());
            let mut pdtm = ObjMap::new();
            pdtm.define("prototype", Value::heap(plaindatetime_proto), proto_attr);
            pdtm.define("from", pdtfrom, method_attr);
            pdtm.define("compare", pdtcompare, method_attr);
            pdtm.define("name", pdtname, fn_attr);
            pdtm.define("length", Value::num(3.0), fn_attr);
            pdtm.is_ctor = true;
            let plaindatetime_ctor = self.heap.alloc(HeapObj::Object(pdtm));
            self.plaindatetime_ctor = plaindatetime_ctor;
            if let HeapObj::Object(p) = self.heap.get_mut(plaindatetime_proto) {
                p.define("constructor", Value::heap(plaindatetime_ctor), method_attr);
                p.define("@@toStringTag", pdttag, fn_attr);
            }
            // Temporal.Instant
            let inst_methods: Vec<(&str, u16)> = native::INSTANT_METHODS
                .iter()
                .enumerate()
                .map(|(i, &n)| (n, native::INST_M_BASE + i as u16))
                .collect();
            let instant_proto = build(self, &inst_methods, None);
            self.proto_of.insert(instant_proto, Value::heap(obj_proto));
            self.instant_proto = instant_proto;
            let iname = self.alloc_str("Instant".to_string());
            let itag = self.alloc_str("Temporal.Instant".to_string());
            let mut im = ObjMap::new();
            im.define("prototype", Value::heap(instant_proto), proto_attr);
            for (n, id) in [
                ("from", INST_FROM),
                ("fromEpochMilliseconds", INST_FROM_EPOCH_MS),
                ("fromEpochNanoseconds", INST_FROM_EPOCH_NS),
                ("fromEpochSeconds", INST_FROM_EPOCH_SEC),
                ("fromEpochMicroseconds", INST_FROM_EPOCH_US),
                ("compare", INST_COMPARE),
            ] {
                let v = Value::heap(self.heap.alloc(HeapObj::Native(id)));
                im.define(n, v, method_attr);
            }
            im.define("name", iname, fn_attr);
            im.define("length", Value::num(1.0), fn_attr);
            im.is_ctor = true;
            let instant_ctor = self.heap.alloc(HeapObj::Object(im));
            self.instant_ctor = instant_ctor;
            if let HeapObj::Object(p) = self.heap.get_mut(instant_proto) {
                p.define("constructor", Value::heap(instant_ctor), method_attr);
                p.define("@@toStringTag", itag, fn_attr);
            }
            // Temporal.PlainYearMonth
            let pym_methods: Vec<(&str, u16)> = native::PLAINYEARMONTH_METHODS
                .iter()
                .enumerate()
                .map(|(i, &n)| (n, native::PYM_M_BASE + i as u16))
                .collect();
            let plainyearmonth_proto = build(self, &pym_methods, None);
            self.proto_of.insert(plainyearmonth_proto, Value::heap(obj_proto));
            self.plainyearmonth_proto = plainyearmonth_proto;
            let pymfrom = Value::heap(self.heap.alloc(HeapObj::Native(PLAINYEARMONTH_FROM)));
            let pymcompare = Value::heap(self.heap.alloc(HeapObj::Native(PLAINYEARMONTH_COMPARE)));
            let pymname = self.alloc_str("PlainYearMonth".to_string());
            let pymtag = self.alloc_str("Temporal.PlainYearMonth".to_string());
            let mut pymm = ObjMap::new();
            pymm.define("prototype", Value::heap(plainyearmonth_proto), proto_attr);
            pymm.define("from", pymfrom, method_attr);
            pymm.define("compare", pymcompare, method_attr);
            pymm.define("name", pymname, fn_attr);
            pymm.define("length", Value::num(2.0), fn_attr);
            pymm.is_ctor = true;
            let plainyearmonth_ctor = self.heap.alloc(HeapObj::Object(pymm));
            self.plainyearmonth_ctor = plainyearmonth_ctor;
            if let HeapObj::Object(p) = self.heap.get_mut(plainyearmonth_proto) {
                p.define("constructor", Value::heap(plainyearmonth_ctor), method_attr);
                p.define("@@toStringTag", pymtag, fn_attr);
            }
            // Temporal.PlainMonthDay
            let pmd_methods: Vec<(&str, u16)> = native::PLAINMONTHDAY_METHODS
                .iter()
                .enumerate()
                .map(|(i, &n)| (n, native::PMD_M_BASE + i as u16))
                .collect();
            let plainmonthday_proto = build(self, &pmd_methods, None);
            self.proto_of.insert(plainmonthday_proto, Value::heap(obj_proto));
            self.plainmonthday_proto = plainmonthday_proto;
            let pmdfrom = Value::heap(self.heap.alloc(HeapObj::Native(PLAINMONTHDAY_FROM)));
            let pmdname = self.alloc_str("PlainMonthDay".to_string());
            let pmdtag = self.alloc_str("Temporal.PlainMonthDay".to_string());
            let mut pmdm = ObjMap::new();
            pmdm.define("prototype", Value::heap(plainmonthday_proto), proto_attr);
            pmdm.define("from", pmdfrom, method_attr);
            pmdm.define("name", pmdname, fn_attr);
            pmdm.define("length", Value::num(2.0), fn_attr);
            pmdm.is_ctor = true;
            let plainmonthday_ctor = self.heap.alloc(HeapObj::Object(pmdm));
            self.plainmonthday_ctor = plainmonthday_ctor;
            if let HeapObj::Object(p) = self.heap.get_mut(plainmonthday_proto) {
                p.define("constructor", Value::heap(plainmonthday_ctor), method_attr);
                p.define("@@toStringTag", pmdtag, fn_attr);
            }
            let mut tn = ObjMap::new();
            tn.define("Duration", Value::heap(duration_ctor), method_attr);
            tn.define("PlainDate", Value::heap(plaindate_ctor), method_attr);
            tn.define("PlainTime", Value::heap(plaintime_ctor), method_attr);
            tn.define("PlainDateTime", Value::heap(plaindatetime_ctor), method_attr);
            tn.define("Instant", Value::heap(instant_ctor), method_attr);
            tn.define("PlainYearMonth", Value::heap(plainyearmonth_ctor), method_attr);
            tn.define("PlainMonthDay", Value::heap(plainmonthday_ctor), method_attr);
            self.temporal_ns = self.heap.alloc(HeapObj::Object(tn));
            let dataview_ctor = build(self, &[], Some(dataview_proto));
            self.dataview_ctor = dataview_ctor;
            if let HeapObj::Object(m) = self.heap.get_mut(dataview_proto) {
                m.define("constructor", Value::heap(dataview_ctor), method_attr);
            }
        }
        // Wire each built-in prototype's `constructor` back to its constructor
        // (`Array.prototype.constructor === Array`, `p.constructor === Promise`,
        // `(5).constructor === Number`, …) — a fundamental invariant assertions
        // rely on. Writable, non-enumerable, configurable (the spec descriptor).
        for (proto, ctor) in [
            (self.obj_proto, object_ctor),
            (self.arr_proto, array_ctor),
            (self.fn_proto, function_ctor),
            (self.str_proto, string_ctor),
            (self.num_proto, number_ctor),
            (self.bool_proto, boolean_ctor),
            (self.set_proto, set_ctor),
            (self.map_proto, map_ctor),
            (self.date_proto, date_ctor),
            (self.promise_proto, promise_ctor),
            (self.weakmap_proto, weakmap_ctor),
            (self.weakset_proto, weakset_ctor),
            (self.weakref_proto, weakref_ctor),
            (self.finreg_proto, finreg_ctor),
        ] {
            if proto != 0 {
                let cv = Value::heap(ctor);
                if let HeapObj::Object(m) = self.heap.get_mut(proto) {
                    m.define("constructor", cv, method_attr);
                }
            }
        }
        // `JSON`: a namespace object. The direct `JSON.parse(x)`/`stringify(x)` call
        // forms are compile-lowered to ops; these back the value form + reflection.
        let json_ctor = build(self, &[("parse", JSON_PARSE), ("stringify", JSON_STRINGIFY)], None);
        // `Math`: a namespace object — the 8 constants (non-w/e/c) + the methods as
        // first-class values + `random`. Direct `Math.abs(x)` is compile-lowered to
        // MathOp; this backs the value form + reflection.
        let math_ctor = {
            let mut methods: Vec<(&str, u16)> = native::MATH_METHODS
                .iter()
                .enumerate()
                .map(|(i, &(name, _, _))| (name, native::MATH_METHOD_BASE + i as u16))
                .collect();
            methods.push(("random", MATH_RANDOM));
            let idx = build(self, &methods, None);
            let consts: &[(&str, f64)] = &[
                ("E", std::f64::consts::E),
                ("LN10", std::f64::consts::LN_10),
                ("LN2", std::f64::consts::LN_2),
                ("LOG10E", std::f64::consts::LOG10_E),
                ("LOG2E", std::f64::consts::LOG2_E),
                ("PI", std::f64::consts::PI),
                ("SQRT1_2", std::f64::consts::FRAC_1_SQRT_2),
                ("SQRT2", std::f64::consts::SQRT_2),
            ];
            if let HeapObj::Object(m) = self.heap.get_mut(idx) {
                for &(n, v) in consts {
                    m.define(n, Value::num(v), proto_attr);
                }
            }
            idx
        };
        // Bare global functions as first-class values (the call form is GlobalFn).
        let parse_int_fn = self.heap.alloc(HeapObj::Native(GLOBAL_PARSE_INT));
        let parse_float_fn = self.heap.alloc(HeapObj::Native(GLOBAL_PARSE_FLOAT));
        let is_nan_fn = self.heap.alloc(HeapObj::Native(GLOBAL_IS_NAN));
        let is_finite_fn = self.heap.alloc(HeapObj::Native(GLOBAL_IS_FINITE));
        // `globalThis`: an empty Object whose property access is routed to the
        // global slots by name (see get_prop/set_prop/has_own_property).
        let global_this = self.heap.alloc(HeapObj::Object(ObjMap::new()));
        self.global_this = global_this;
        // Inject into the reserved global slots (collect first to end the program
        // borrow before mutating `self.globals`).
        let mut sets: Vec<(usize, u32)> = Vec::new();
        for (slot, name) in self.program.global_names.iter().enumerate() {
            let v = match name.as_str() {
                "Object" => Some(object_ctor),
                "Array" => Some(array_ctor),
                "Function" => Some(function_ctor),
                "String" => Some(string_ctor),
                "Number" => Some(number_ctor),
                "Set" => Some(set_ctor),
                "Map" => Some(map_ctor),
                "Boolean" => Some(boolean_ctor),
                "Date" => Some(date_ctor),
                "Promise" => Some(promise_ctor),
                "Reflect" => Some(reflect_ctor),
                "JSON" => Some(json_ctor),
                "Math" => Some(math_ctor),
                "WeakMap" => Some(weakmap_ctor),
                "WeakSet" => Some(weakset_ctor),
                "WeakRef" => Some(weakref_ctor),
                "FinalizationRegistry" => Some(finreg_ctor),
                "Error" => Some(self.error_ctors[0]),
                "TypeError" => Some(self.error_ctors[1]),
                "RangeError" => Some(self.error_ctors[2]),
                "SyntaxError" => Some(self.error_ctors[3]),
                "ReferenceError" => Some(self.error_ctors[4]),
                "EvalError" => Some(self.error_ctors[5]),
                "URIError" => Some(self.error_ctors[6]),
                "AggregateError" => Some(self.error_ctors[7]),
                "Symbol" => Some(self.symbol_ctor),
                "BigInt" => Some(self.bigint_ctor),
                "RegExp" => Some(self.regexp_ctor),
                "ArrayBuffer" => Some(self.arraybuffer_ctor),
                "DataView" => Some(self.dataview_ctor),
                "Proxy" => Some(self.proxy_ctor),
                "Temporal" => Some(self.temporal_ns),
                "parseInt" => Some(parse_int_fn),
                "parseFloat" => Some(parse_float_fn),
                "isNaN" => Some(is_nan_fn),
                "isFinite" => Some(is_finite_fn),
                "globalThis" => Some(global_this),
                // The 11 TypedArray constructors (Int8Array … BigUint64Array).
                _ => native::TA_KINDS
                    .iter()
                    .position(|t| t.0 == name.as_str())
                    .map(|k| self.ta_ctors[k]),
            };
            if let Some(v) = v {
                // Constructor globals expose own `name`/`length` like any function
                // ({writable:false, enumerable:false, configurable:true}). Namespaces
                // (Reflect/Math/JSON, is_ctor==false) don't.
                if matches!(self.heap.get(v), HeapObj::Object(m) if m.is_ctor) {
                    let len = match name.as_str() {
                        "Date" => 7.0,
                        "Map" | "Set" | "WeakMap" | "WeakSet" => 0.0,
                        "AggregateError" => 2.0, // (errors, message?)
                        "RegExp" => 2.0,         // (pattern, flags)
                        "Proxy" => 2.0,          // (target, handler)
                        // TypedArray ctors take (length | buffer, byteOffset, length).
                        n if native::TA_KINDS.iter().any(|t| t.0 == n) => 3.0,
                        _ => 1.0, // Object/Array/Function/String/Number/Boolean/Promise/Error+subtypes/ArrayBuffer/DataView
                    };
                    let nm = self.alloc_str(name.clone());
                    let fn_attr = PropAttr {
                        writable: false,
                        enumerable: false,
                        configurable: true,
                        accessor: false,
                        setter: Value::UNDEFINED,
                    };
                    if let HeapObj::Object(m) = self.heap.get_mut(v) {
                        m.define("length", Value::num(len), fn_attr);
                        m.define("name", nm, fn_attr);
                    }
                }
                sets.push((slot, v));
            }
        }
        for (slot, v) in sets {
            if slot < self.globals.len() {
                self.globals[slot] = Value::heap(v);
            }
        }
    }

    /// Invoke a native (built-in) function by id with `this` and `args`. Backs
    /// first-class builtin values (`Object.defineProperty`, `Array.isArray`,
    /// `Object.prototype.hasOwnProperty`, `Function.prototype.call`, …).
    fn call_native(&mut self, id: u16, this: Value, args: &[Value]) -> Result<Value, Thrown> {
        use native::*;
        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        let a1 = args.get(1).copied().unwrap_or(Value::UNDEFINED);
        Ok(match id {
            OBJ_DEFINE_PROPERTY => {
                let key = self.key_of(a1);
                self.object_define_property(a0, &key, args.get(2).copied().unwrap_or(Value::UNDEFINED))?;
                a0
            }
            OBJ_DEFINE_PROPERTIES => {
                self.object_define_properties(a0, a1)?;
                a0
            }
            OBJ_GET_OWN_DESC => {
                let key = self.key_of(a1);
                self.object_get_own_property_descriptor(a0, &key)
            }
            OBJ_GET_OWN_NAMES => self.object_own_property_names(a0),
            OBJ_GET_PROTO => self.object_get_prototype_of(a0),
            OBJ_KEYS => self.object_enum_own(a0, EnumWhat::Keys),
            OBJ_VALUES => self.object_enum_own(a0, EnumWhat::Values),
            OBJ_ENTRIES => self.object_enum_own(a0, EnumWhat::Entries),
            OBJ_ASSIGN => self.object_assign(args)?,
            OBJ_CREATE => {
                let o = Value::heap(self.heap.alloc(HeapObj::Object(ObjMap::new())));
                if a0 != Value::UNDEFINED {
                    self.proto_of.insert(o.heap_index(), a0);
                }
                if a1 != Value::UNDEFINED {
                    self.object_define_properties(o, a1)?;
                }
                o
            }
            PROTO_HAS_OWN => Value::bool(self.has_own_property(this, &self.key_of(a0))),
            PROTO_PROP_ENUM => Value::bool(self.own_is_enumerable(this, &self.key_of(a0))),
            PROTO_IS_PROTO_OF => Value::bool(self.is_prototype_of(this, a0)),
            PROTO_VALUE_OF => this,
            PROTO_TO_STRING => {
                let tag = self.object_to_string_tag(this)?;
                self.alloc_str(format!("[object {tag}]"))
            }
            ERROR_TO_STRING => {
                // `name` (default "Error") + ": " + `message` (default ""), dropping
                // the separator when either part is empty.
                let nv = self.get_prop(this, "name")?;
                let name =
                    if nv == Value::UNDEFINED { "Error".to_string() } else { self.to_js_string(nv)? };
                let mv = self.get_prop(this, "message")?;
                let msg = if mv == Value::UNDEFINED { String::new() } else { self.to_js_string(mv)? };
                let s = if name.is_empty() {
                    msg
                } else if msg.is_empty() {
                    name
                } else {
                    format!("{name}: {msg}")
                };
                self.alloc_str(s)
            }
            SYMBOL_TO_STRING => {
                // `Symbol.prototype.toString` → "Symbol(description)".
                let desc = match this.is_heap().then(|| self.heap.get(this.heap_index())) {
                    Some(HeapObj::Symbol { desc, .. }) => *desc,
                    _ => {
                        return Err(Thrown(
                            "TypeError: Symbol.prototype.toString requires that 'this' be a Symbol"
                                .into(),
                        ))
                    }
                };
                let d = if desc == Value::UNDEFINED { String::new() } else { self.display(desc) };
                self.alloc_str(format!("Symbol({d})"))
            }
            SYMBOL_VALUE_OF => {
                // `Symbol.prototype.valueOf` → the Symbol primitive itself.
                if matches!(
                    this.is_heap().then(|| self.heap.get(this.heap_index())),
                    Some(HeapObj::Symbol { .. })
                ) {
                    this
                } else {
                    return Err(Thrown(
                        "TypeError: Symbol.prototype.valueOf requires that 'this' be a Symbol".into(),
                    ));
                }
            }
            SYMBOL_FOR => {
                // `Symbol.for(key)`: shared registry symbol for the ToString(key).
                let key = self.to_js_string(a0)?;
                if let Some(&sym) = self.symbol_registry.get(&key) {
                    sym
                } else {
                    let desc = self.alloc_str(key.clone());
                    let prop_key = format!("@@for:{key}");
                    let sym = self.make_named_symbol(desc, &prop_key);
                    self.symbol_registry.insert(key, sym);
                    sym
                }
            }
            SYMBOL_KEY_FOR => {
                // `Symbol.keyFor(sym)`: the registry key for a registered symbol, else undefined.
                if !matches!(
                    a0.is_heap().then(|| self.heap.get(a0.heap_index())),
                    Some(HeapObj::Symbol { .. })
                ) {
                    return Err(Thrown(
                        "TypeError: Symbol.keyFor requires that the argument be a Symbol".into(),
                    ));
                }
                let key =
                    self.symbol_registry.iter().find(|(_, v)| v.bits() == a0.bits()).map(|(k, _)| k.clone());
                match key {
                    Some(k) => self.alloc_str(k),
                    None => Value::UNDEFINED,
                }
            }
            BIGINT_TO_STRING => {
                let n = match self.bigint_value(this) {
                    Some(n) => n,
                    None => {
                        return Err(Thrown(
                            "TypeError: BigInt.prototype.toString requires that 'this' be a BigInt".into(),
                        ))
                    }
                };
                let radix = if a0 == Value::UNDEFINED { 10 } else { self.to_number(a0)? as i64 };
                if !(2..=36).contains(&radix) {
                    return Err(Thrown("RangeError: toString() radix must be between 2 and 36".into()));
                }
                self.alloc_str(bigint_to_radix(n, radix as u32))
            }
            BIGINT_VALUE_OF => {
                if self.bigint_value(this).is_some() {
                    this
                } else {
                    return Err(Thrown(
                        "TypeError: BigInt.prototype.valueOf requires that 'this' be a BigInt".into(),
                    ));
                }
            }
            BIGINT_AS_INTN => {
                let bits = self.to_number(a0)?;
                if !bits.is_finite() || bits < 0.0 {
                    return Err(Thrown("RangeError: Invalid bits for BigInt.asIntN".into()));
                }
                let x = self.to_bigint(a1)?;
                self.make_bigint(bigint_as_intn(bits as u32, x))
            }
            BIGINT_AS_UINTN => {
                let bits = self.to_number(a0)?;
                if !bits.is_finite() || bits < 0.0 {
                    return Err(Thrown("RangeError: Invalid bits for BigInt.asUintN".into()));
                }
                let x = self.to_bigint(a1)?;
                self.make_bigint(bigint_as_uintn(bits as u32, x))
            }
            REGEXP_EXEC => {
                if !matches!(
                    this.is_heap().then(|| self.heap.get(this.heap_index())),
                    Some(HeapObj::RegExp { .. })
                ) {
                    return Err(Thrown(
                        "TypeError: RegExp.prototype.exec called on a non-RegExp".into(),
                    ));
                }
                self.regexp_exec(this.heap_index(), a0)?
            }
            REGEXP_TEST => {
                if !matches!(
                    this.is_heap().then(|| self.heap.get(this.heap_index())),
                    Some(HeapObj::RegExp { .. })
                ) {
                    return Err(Thrown(
                        "TypeError: RegExp.prototype.test called on a non-RegExp".into(),
                    ));
                }
                let r = self.regexp_exec(this.heap_index(), a0)?;
                Value::bool(r != Value::NULL)
            }
            REGEXP_TO_STRING => {
                let (src, flg) = match this.is_heap().then(|| self.heap.get(this.heap_index())) {
                    Some(HeapObj::RegExp { source, flags, .. }) => (
                        if source.is_empty() { "(?:)".to_string() } else { source.clone() },
                        flags.clone(),
                    ),
                    _ => {
                        let s = self.get_prop(this, "source")?;
                        let f = self.get_prop(this, "flags")?;
                        (self.to_js_string(s)?, self.to_js_string(f)?)
                    }
                };
                self.alloc_str(format!("/{src}/{flg}"))
            }
            FN_CALL => {
                let rest: &[Value] = if args.len() > 1 { &args[1..] } else { &[] };
                self.call_value(this, a0, rest)?
            }
            FN_APPLY => {
                let callargs = if a1.is_heap() { self.iterate_to_vec(a1)? } else { Vec::new() };
                self.call_value(this, a0, &callargs)?
            }
            FN_BIND => {
                let bound: Vec<Value> = if args.len() > 1 { args[1..].to_vec() } else { Vec::new() };
                Value::heap(self.heap.alloc(HeapObj::Bound { target: this, this: a0, args: bound }))
            }
            ARR_IS_ARRAY => {
                Value::bool(a0.is_heap() && matches!(self.heap.get(a0.heap_index()), HeapObj::Array(_)))
            }
            ARR_FROM => self.array_from(a0, a1)?,
            ARR_OF => Value::heap(self.heap.alloc(HeapObj::Array(args.to_vec()))),
            // `Array.prototype.{join,push}` as values: `this` is the receiver array.
            // join is generic over array-likes (array_method materializes a
            // non-array receiver); push mutates, so it still requires a real array.
            ARR_JOIN => {
                if this.is_heap() {
                    self.array_method(this.heap_index(), "join", args)?.unwrap_or(Value::UNDEFINED)
                } else {
                    Value::UNDEFINED
                }
            }
            ARR_PUSH => {
                if this.is_heap() && matches!(self.heap.get(this.heap_index()), HeapObj::Array(_)) {
                    self.array_method(this.heap_index(), "push", args)?.unwrap_or(Value::UNDEFINED)
                } else {
                    Value::UNDEFINED
                }
            }
            // More Object statics as values.
            OBJ_IS => {
                let a = args.first().copied().unwrap_or(Value::UNDEFINED);
                let b = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                Value::bool(self.same_value(a, b))
            }
            OBJ_HAS_OWN => {
                let o = args.first().copied().unwrap_or(Value::UNDEFINED);
                let k = self.display(args.get(1).copied().unwrap_or(Value::UNDEFINED));
                Value::bool(self.has_own_property(o, &k))
            }
            OBJ_SET_PROTO_OF => {
                let o = args.first().copied().unwrap_or(Value::UNDEFINED);
                let proto = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                if o.is_heap() {
                    self.proto_of.insert(o.heap_index(), proto);
                }
                o
            }
            OBJ_GET_OWN_SYMBOLS => {
                // Own symbol-keyed properties: the `@@`-prefixed own keys, mapped
                // back to their Symbol values via the prop_key registry.
                let mut syms: Vec<Value> = Vec::new();
                if a0.is_heap() {
                    if let HeapObj::Object(m) = self.heap.get(a0.heap_index()) {
                        let keys: Vec<String> =
                            m.keys.iter().filter(|k| k.starts_with("@@")).cloned().collect();
                        for k in keys {
                            if let Some(&sym) = self.symbol_keys.get(&k) {
                                syms.push(sym);
                            }
                        }
                    }
                }
                Value::heap(self.heap.alloc(HeapObj::Array(syms)))
            }
            OBJ_FROM_ENTRIES => {
                let src = args.first().copied().unwrap_or(Value::UNDEFINED);
                let entries = if src.is_heap() { self.iterate_to_vec(src)? } else { Vec::new() };
                let mut map = ObjMap::new();
                for e in entries {
                    let k = self.get_index(e, Value::int(0))?;
                    let v = self.get_index(e, Value::int(1))?;
                    let ks = self.display(k);
                    map.set(&ks, v);
                }
                Value::heap(self.heap.alloc(HeapObj::Object(map)))
            }
            OBJ_GET_OWN_DESCS => {
                let o = args.first().copied().unwrap_or(Value::UNDEFINED);
                let names = self.object_own_property_names(o);
                let keys: Vec<Value> = match self.heap.get(names.heap_index()) {
                    HeapObj::Array(items) => items.clone(),
                    _ => Vec::new(),
                };
                let mut map = ObjMap::new();
                for kv in keys {
                    let ks = self.display(kv);
                    let desc = self.object_get_own_property_descriptor(o, &ks);
                    map.set(&ks, desc);
                }
                Value::heap(self.heap.alloc(HeapObj::Object(map)))
            }
            // Integrity traits. Non-object arguments pass through unchanged
            // (freeze/seal/preventExtensions) or report as already-locked
            // (isFrozen/isSealed -> true, isExtensible -> false), per ES2015+.
            OBJ_FREEZE => {
                let o = args.first().copied().unwrap_or(Value::UNDEFINED);
                if o.is_heap() {
                    if let HeapObj::Object(m) = self.heap.get_mut(o.heap_index()) {
                        m.freeze();
                    }
                }
                o
            }
            OBJ_SEAL => {
                let o = args.first().copied().unwrap_or(Value::UNDEFINED);
                if o.is_heap() {
                    if let HeapObj::Object(m) = self.heap.get_mut(o.heap_index()) {
                        m.seal();
                    }
                }
                o
            }
            OBJ_PREVENT_EXT => {
                let o = args.first().copied().unwrap_or(Value::UNDEFINED);
                if o.is_heap() {
                    if let HeapObj::Object(m) = self.heap.get_mut(o.heap_index()) {
                        m.extensible = false;
                    }
                }
                o
            }
            OBJ_IS_FROZEN => {
                let o = args.first().copied().unwrap_or(Value::UNDEFINED);
                let frozen = match o.is_heap().then(|| self.heap.get(o.heap_index())) {
                    Some(HeapObj::Object(m)) => m.is_frozen(),
                    _ => true,
                };
                Value::bool(frozen)
            }
            OBJ_IS_SEALED => {
                let o = args.first().copied().unwrap_or(Value::UNDEFINED);
                let sealed = match o.is_heap().then(|| self.heap.get(o.heap_index())) {
                    Some(HeapObj::Object(m)) => m.is_sealed(),
                    _ => true,
                };
                Value::bool(sealed)
            }
            OBJ_IS_EXT => {
                let o = args.first().copied().unwrap_or(Value::UNDEFINED);
                let ext = match o.is_heap().then(|| self.heap.get(o.heap_index())) {
                    Some(HeapObj::Object(m)) => m.extensible,
                    _ => false,
                };
                Value::bool(ext)
            }
            // Object.groupBy(items, cb) -> null-proto object of arrays keyed by cb's
            // (string) return; Map.groupBy -> a Map keyed by cb's value (SameValueZero).
            OBJ_GROUP_BY | MAP_GROUP_BY => {
                let src = args.first().copied().unwrap_or(Value::UNDEFINED);
                let cb = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                if !(cb.is_heap() && self.heap.as_callable(cb.heap_index()).is_some()) {
                    return Err(Thrown("TypeError: groupBy callback is not callable".into()));
                }
                if !src.is_heap() {
                    return Err(Thrown("TypeError: groupBy items is not iterable".into()));
                }
                let items = self.iterate_to_vec(src)?;
                if id == OBJ_GROUP_BY {
                    let mut map = ObjMap::new();
                    for (i, item) in items.into_iter().enumerate() {
                        let key = self.call_value(cb, Value::UNDEFINED, &[item, Value::int(i as i32)])?;
                        let ks = self.display(key);
                        match map.get(&ks) {
                            Some(arr) => {
                                if let HeapObj::Array(a) = self.heap.get_mut(arr.heap_index()) {
                                    a.push(item);
                                }
                            }
                            None => {
                                let arr = Value::heap(self.heap.alloc(HeapObj::Array(vec![item])));
                                map.set(&ks, arr);
                            }
                        }
                    }
                    let result = self.heap.alloc(HeapObj::Object(map));
                    self.proto_of.insert(result, Value::NULL); // null prototype per spec
                    Value::heap(result)
                } else {
                    let mut keys: Vec<Value> = Vec::new();
                    let mut vals: Vec<Value> = Vec::new();
                    for (i, item) in items.into_iter().enumerate() {
                        let mut key = self.call_value(cb, Value::UNDEFINED, &[item, Value::int(i as i32)])?;
                        if key.is_number() && key.as_f64() == 0.0 {
                            key = Value::int(0); // Map normalizes -0 to +0
                        }
                        match keys.iter().position(|k| self.same_value_zero(*k, key)) {
                            Some(pos) => {
                                if let HeapObj::Array(a) = self.heap.get_mut(vals[pos].heap_index()) {
                                    a.push(item);
                                }
                            }
                            None => {
                                keys.push(key);
                                vals.push(Value::heap(self.heap.alloc(HeapObj::Array(vec![item]))));
                            }
                        }
                    }
                    Value::heap(self.heap.alloc(HeapObj::Map { keys, vals }))
                }
            }
            // Promise.withResolvers() -> { promise, resolve, reject }.
            PROMISE_WITH_RESOLVERS => {
                let p = self.alloc_promise();
                let resolve = Value::heap(
                    self.heap.alloc(HeapObj::BoundResolver { promise: p, is_reject: false }),
                );
                let reject = Value::heap(
                    self.heap.alloc(HeapObj::BoundResolver { promise: p, is_reject: true }),
                );
                let mut map = ObjMap::new();
                map.set("promise", Value::heap(p));
                map.set("resolve", resolve);
                map.set("reject", reject);
                Value::heap(self.heap.alloc(HeapObj::Object(map)))
            }
            // Reflect namespace. apply/construct accept any callable target; the
            // property-reflecting methods require Type(target) === Object (else TypeError).
            REFLECT_APPLY => {
                let target = a0;
                let this_arg = a1;
                let args_list = args.get(2).copied().unwrap_or(Value::UNDEFINED);
                let arg_vec =
                    if args_list.is_heap() { self.array_snapshot(args_list.heap_index()) } else { Vec::new() };
                self.call_value(target, this_arg, &arg_vec)?
            }
            REFLECT_CONSTRUCT => {
                let target = a0;
                if !self.is_constructor(target) {
                    return Err(Thrown("TypeError: Reflect.construct target is not a constructor".into()));
                }
                // An explicit newTarget (3rd arg) must also be a constructor. We
                // don't model newTarget-driven prototype selection, but the throw is
                // what test262's isConstructor relies on.
                if let Some(nt) = args.get(2) {
                    if !self.is_constructor(*nt) {
                        return Err(Thrown(
                            "TypeError: Reflect.construct newTarget is not a constructor".into(),
                        ));
                    }
                }
                let arg_vec = if a1.is_heap() { self.array_snapshot(a1.heap_index()) } else { Vec::new() };
                self.construct(target, &arg_vec)?
            }
            REFLECT_GET => {
                if !self.is_object_value(a0) {
                    return Err(Thrown("TypeError: Reflect.get called on non-object".into()));
                }
                self.get_index(a0, a1)?
            }
            REFLECT_SET => {
                if !self.is_object_value(a0) {
                    return Err(Thrown("TypeError: Reflect.set called on non-object".into()));
                }
                let value = args.get(2).copied().unwrap_or(Value::UNDEFINED);
                let key = self.key_of(a1);
                // success = not blocked by a non-writable own data property, an
                // accessor without a setter, or a new key on a non-extensible object.
                let ok = match self.heap.get(a0.heap_index()) {
                    HeapObj::Object(m) => match m.pos(&key) {
                        Some(i) => {
                            if m.attrs[i].accessor {
                                m.attrs[i].setter != Value::UNDEFINED
                            } else {
                                m.attrs[i].writable
                            }
                        }
                        None => m.extensible,
                    },
                    _ => true,
                };
                if ok {
                    self.set_index(a0, a1, value)?;
                }
                Value::bool(ok)
            }
            REFLECT_HAS => {
                if !self.is_object_value(a0) {
                    return Err(Thrown("TypeError: Reflect.has called on non-object".into()));
                }
                Value::bool(self.has_property(a0, a1))
            }
            REFLECT_DELETE => {
                if !self.is_object_value(a0) {
                    return Err(Thrown("TypeError: Reflect.deleteProperty called on non-object".into()));
                }
                let key = self.key_of(a1);
                self.delete_prop(a0, &key)
            }
            REFLECT_OWN_KEYS => {
                if !self.is_object_value(a0) {
                    return Err(Thrown("TypeError: Reflect.ownKeys called on non-object".into()));
                }
                self.object_own_property_names(a0)
            }
            REFLECT_GET_PROTO => {
                if !self.is_object_value(a0) {
                    return Err(Thrown("TypeError: Reflect.getPrototypeOf called on non-object".into()));
                }
                self.object_get_prototype_of(a0)
            }
            REFLECT_SET_PROTO => {
                if !self.is_object_value(a0) {
                    return Err(Thrown("TypeError: Reflect.setPrototypeOf called on non-object".into()));
                }
                if a1 != Value::NULL && !self.is_object_value(a1) {
                    return Err(Thrown(
                        "TypeError: Reflect.setPrototypeOf prototype must be an object or null".into(),
                    ));
                }
                self.proto_of.insert(a0.heap_index(), a1);
                Value::bool(true)
            }
            REFLECT_DEFINE => {
                if !self.is_object_value(a0) {
                    return Err(Thrown("TypeError: Reflect.defineProperty called on non-object".into()));
                }
                let desc = args.get(2).copied().unwrap_or(Value::UNDEFINED);
                if !self.is_object_value(desc) {
                    return Err(Thrown("TypeError: Property description must be an object".into()));
                }
                let key = self.key_of(a1);
                // Reflect.defineProperty returns false (not throw) when the definition
                // is rejected (non-configurable redefine, non-extensible new key).
                match self.object_define_property(a0, &key, desc) {
                    Ok(()) => Value::bool(true),
                    Err(_) => Value::bool(false),
                }
            }
            REFLECT_GET_OWN_DESC => {
                if !self.is_object_value(a0) {
                    return Err(Thrown(
                        "TypeError: Reflect.getOwnPropertyDescriptor called on non-object".into(),
                    ));
                }
                let key = self.key_of(a1);
                self.object_get_own_property_descriptor(a0, &key)
            }
            REFLECT_IS_EXT => {
                if !self.is_object_value(a0) {
                    return Err(Thrown("TypeError: Reflect.isExtensible called on non-object".into()));
                }
                let ext = match self.heap.get(a0.heap_index()) {
                    HeapObj::Object(m) => m.extensible,
                    _ => true,
                };
                Value::bool(ext)
            }
            REFLECT_PREVENT_EXT => {
                if !self.is_object_value(a0) {
                    return Err(Thrown("TypeError: Reflect.preventExtensions called on non-object".into()));
                }
                if let HeapObj::Object(m) = self.heap.get_mut(a0.heap_index()) {
                    m.extensible = false;
                }
                Value::bool(true)
            }
            // JSON namespace methods as values (`JSON.parse`/`JSON.stringify`).
            // (The direct `JSON.parse(x)` call form is compile-lowered to a JSON op;
            // these back the value form + reflection.)
            JSON_PARSE => {
                let s = self.display(a0);
                self.json_parse(&s)?
            }
            JSON_STRINGIFY => {
                let space = args.get(2).copied().unwrap_or(Value::UNDEFINED);
                let indent = self.json_indent(space);
                match self.json_value(a0, &indent, 0) {
                    Some(s) => self.alloc_str(s),
                    None => Value::UNDEFINED,
                }
            }
            // `Math.random` as a value (the call form uses the Random op). xorshift64*.
            MATH_RANDOM => {
                let mut x = self.rng_state;
                x ^= x >> 12;
                x ^= x << 25;
                x ^= x >> 27;
                self.rng_state = x;
                let r = x.wrapping_mul(0x2545_F491_4F6C_DD1D);
                Value::num((r >> 11) as f64 / (1u64 << 53) as f64)
            }
            // WeakMap/WeakSet methods (brand-checked + object-key validated inside).
            WM_GET => self.weakmap_method(this, "get", args)?,
            WM_SET => self.weakmap_method(this, "set", args)?,
            WM_HAS => self.weakmap_method(this, "has", args)?,
            WM_DELETE => self.weakmap_method(this, "delete", args)?,
            WS_ADD => self.weakset_method(this, "add", args)?,
            WS_HAS => self.weakset_method(this, "has", args)?,
            WS_DELETE => self.weakset_method(this, "delete", args)?,
            WR_DEREF => {
                match this.is_heap().then(|| self.heap.get(this.heap_index())) {
                    Some(HeapObj::WeakRef(t)) => *t, // no GC → target always live
                    _ => {
                        return Err(Thrown(
                            "TypeError: WeakRef.prototype.deref called on incompatible receiver".into(),
                        ))
                    }
                }
            }
            FR_REGISTER => self.finreg_method(this, "register", args)?,
            FR_UNREGISTER => self.finreg_method(this, "unregister", args)?,
            ITER_NEXT => {
                let (val, done) = match this.is_heap().then(|| self.heap.get_mut(this.heap_index())) {
                    Some(HeapObj::Iterator { items, index, .. }) => {
                        if *index < items.len() {
                            let v = items[*index];
                            *index += 1;
                            (v, false)
                        } else {
                            (Value::UNDEFINED, true)
                        }
                    }
                    _ => {
                        return Err(Thrown(
                            "TypeError: Iterator.prototype.next called on incompatible receiver".into(),
                        ))
                    }
                };
                let mut m = ObjMap::new();
                m.set("value", val);
                m.set("done", Value::bool(done));
                Value::heap(self.heap.alloc(HeapObj::Object(m)))
            }
            ITER_SELF => this, // `iter[Symbol.iterator]()` returns the iterator itself
            // Number static methods as values (no coercion, per spec).
            NUM_IS_INTEGER => Value::bool(num_is_integer(a0)),
            NUM_IS_NAN => Value::bool(a0.is_double() && a0.as_f64().is_nan()),
            NUM_IS_FINITE => Value::bool(num_is_finite(a0)),
            NUM_IS_SAFE_INTEGER => Value::bool(num_is_safe_integer(a0)),
            // Global functions as values.
            GLOBAL_PARSE_INT => {
                let s = self.display(a0);
                let radix = if args.len() >= 2 { self.to_number(a1)? as i32 } else { 0 };
                Value::num(parse_int(&s, radix))
            }
            GLOBAL_PARSE_FLOAT => Value::num(parse_float(&self.display(a0))),
            GLOBAL_IS_NAN => Value::bool(self.to_number(a0).unwrap_or(f64::NAN).is_nan()),
            GLOBAL_IS_FINITE => Value::bool(self.to_number(a0).unwrap_or(f64::NAN).is_finite()),
            // String static methods.
            STR_FROM_CHAR_CODE => {
                let mut s = String::new();
                for &v in args {
                    let u = to_uint32(self.to_number(v).unwrap_or(0.0)) as u16;
                    s.push(char::from_u32(u as u32).unwrap_or('\u{FFFD}'));
                }
                self.alloc_str(s)
            }
            STR_FROM_CODE_POINT => {
                let mut s = String::new();
                for &v in args {
                    let n = self.to_number(v)?;
                    if !n.is_finite() || n < 0.0 || n > 0x10FFFF as f64 || n.fract() != 0.0 {
                        return Err(Thrown(format!("RangeError: Invalid code point {n}")));
                    }
                    // A lone-surrogate code point can't be a Rust char → replacement.
                    s.push(char::from_u32(n as u32).unwrap_or('\u{FFFD}'));
                }
                self.alloc_str(s)
            }
            // Date static methods as values.
            DATE_NOW => Value::num(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as f64)
                    .unwrap_or(0.0),
            ),
            DATE_PARSE => Value::num(parse_date(&self.display(a0))),
            DATE_UTC => Value::num(self.date_utc_ms(args)?),
            STR_RAW => {
                // String.raw(template, ...subs): interleave template.raw[i] with subs[i].
                let raw = self.get_prop(a0, "raw")?;
                if !raw.is_heap() {
                    return Ok(self.alloc_str(String::new()));
                }
                let len_v = self.get_prop(raw, "length")?;
                let n = self.to_number(len_v)?;
                let raw_len = if n.is_finite() && n > 0.0 { n as usize } else { 0 };
                let subs = args.get(1..).unwrap_or(&[]);
                let mut out = String::new();
                for i in 0..raw_len {
                    let seg = self.get_index(raw, Value::int(i as i32))?;
                    out.push_str(&self.display(seg));
                    if i + 1 == raw_len {
                        break;
                    }
                    if let Some(sub) = subs.get(i) {
                        out.push_str(&self.display(*sub));
                    }
                }
                self.alloc_str(out)
            }
            // Object.prototype.toLocaleString() → this.toString().
            PROTO_TO_LOCALE_STRING => {
                let ts = self.get_prop(this, "toString")?;
                if self.is_callable(ts) {
                    self.call_value(ts, this, &[])?
                } else {
                    return Err(Thrown("TypeError: toString is not callable".into()));
                }
            }
            // `Math.<op>` as a value (`Math.abs`, `Math.max`, …). The direct call
            // form is compile-lowered to MathOp; these back the value form.
            _ if native::math_method(id).is_some() => {
                let (_, op, _) = native::math_method(id).unwrap();
                Value::num(self.eval_math_args(op, args)?)
            }
            // Promise static methods invoked as values (`Promise.resolve`, …).
            PROMISE_RESOLVE => {
                let p = self.to_promise(args.first().copied().unwrap_or(Value::UNDEFINED));
                Value::heap(p)
            }
            PROMISE_REJECT => {
                let p = self.alloc_promise();
                self.reject(p, args.first().copied().unwrap_or(Value::UNDEFINED));
                Value::heap(p)
            }
            PROMISE_ALL => self.promise_combine(crate::heap::CombKind::All, args.first().copied().unwrap_or(Value::UNDEFINED))?,
            PROMISE_ALLSETTLED => self.promise_combine(crate::heap::CombKind::AllSettled, args.first().copied().unwrap_or(Value::UNDEFINED))?,
            PROMISE_RACE => self.promise_combine(crate::heap::CombKind::Race, args.first().copied().unwrap_or(Value::UNDEFINED))?,
            PROMISE_ANY => self.promise_combine(crate::heap::CombKind::Any, args.first().copied().unwrap_or(Value::UNDEFINED))?,
            // `%TypedArray%.prototype.<m>` invoked as a value (`.map.call(ta, …)`).
            _ if (TA_METHOD_BASE..TA_METHOD_BASE + TA_PROTO_METHODS.len() as u16).contains(&id) => {
                let m = TA_PROTO_METHODS[(id - TA_METHOD_BASE) as usize];
                if !matches!(this.is_heap().then(|| self.heap.get(this.heap_index())), Some(HeapObj::TypedArray { .. })) {
                    return Err(Thrown(format!(
                        "TypeError: TypedArray.prototype.{m} called on incompatible receiver"
                    )));
                }
                self.typed_array_method(this.heap_index(), m, args)?.unwrap_or(Value::UNDEFINED)
            }
            _ if (DV_METHOD_BASE..DV_METHOD_BASE + DV_PROTO_METHODS.len() as u16).contains(&id) => {
                let m = DV_PROTO_METHODS[(id - DV_METHOD_BASE) as usize];
                if !matches!(this.is_heap().then(|| self.heap.get(this.heap_index())), Some(HeapObj::DataView { .. })) {
                    return Err(Thrown(format!(
                        "TypeError: DataView.prototype.{m} called on incompatible receiver"
                    )));
                }
                self.dataview_method(this.heap_index(), m, args)?.unwrap_or(Value::UNDEFINED)
            }
            ARRAYBUFFER_SLICE => {
                if !matches!(this.is_heap().then(|| self.heap.get(this.heap_index())), Some(HeapObj::ArrayBuffer { .. })) {
                    return Err(Thrown(
                        "TypeError: ArrayBuffer.prototype.slice called on incompatible receiver".into(),
                    ));
                }
                self.arraybuffer_method(this.heap_index(), "slice", args)?.unwrap_or(Value::UNDEFINED)
            }
            PROXY_REVOCABLE => {
                // Proxy.revocable(target, handler) → { proxy, revoke }.
                let p = self.make_proxy(a0, a1)?;
                let revoke_fn = self.heap.alloc(HeapObj::Native(PROXY_REVOKE));
                let revoke = Value::heap(self.heap.alloc(HeapObj::Bound {
                    target: Value::heap(revoke_fn),
                    this: p,
                    args: Vec::new(),
                }));
                let mut m = ObjMap::new();
                m.set("proxy", p);
                m.set("revoke", revoke);
                Value::heap(self.heap.alloc(HeapObj::Object(m)))
            }
            PROXY_REVOKE => {
                if this.is_heap() {
                    if let HeapObj::Proxy { revoked, .. } = self.heap.get_mut(this.heap_index()) {
                        *revoked = true;
                    }
                }
                Value::UNDEFINED
            }
            _ if (TEMPORAL_M_BASE..TEMPORAL_M_BASE + TEMPORAL_DURATION_METHODS.len() as u16)
                .contains(&id) =>
            {
                let m = TEMPORAL_DURATION_METHODS[(id - TEMPORAL_M_BASE) as usize];
                if !matches!(
                    this.is_heap().then(|| self.heap.get(this.heap_index())),
                    Some(HeapObj::Temporal { kind: 0, .. })
                ) {
                    return Err(Thrown(format!(
                        "TypeError: Temporal.Duration.prototype.{m} called on incompatible receiver"
                    )));
                }
                self.temporal_method(this.heap_index(), m, args)?.unwrap_or(Value::UNDEFINED)
            }
            TEMPORAL_DURATION_FROM => {
                let f = self.to_duration(a0)?;
                self.make_duration(f)
            }
            TEMPORAL_DURATION_COMPARE => {
                let fa = self.to_duration(a0)?;
                let fb = self.to_duration(a1)?;
                // Approximate total (24h days, 7-day weeks; y/mo need relativeTo).
                let tot = |f: &[i64; 10]| -> i128 {
                    ((f[2] * 7 + f[3]) as i128) * 86_400_000_000_000
                        + (f[4] as i128) * 3_600_000_000_000
                        + (f[5] as i128) * 60_000_000_000
                        + (f[6] as i128) * 1_000_000_000
                        + (f[7] as i128) * 1_000_000
                        + (f[8] as i128) * 1_000
                        + (f[9] as i128)
                };
                let (a, b) = (tot(&fa), tot(&fb));
                Value::num(if a < b { -1.0 } else if a > b { 1.0 } else { 0.0 })
            }
            _ if (PD_M_BASE..PD_M_BASE + PLAINDATE_METHODS.len() as u16).contains(&id) => {
                let m = PLAINDATE_METHODS[(id - PD_M_BASE) as usize];
                if !matches!(
                    this.is_heap().then(|| self.heap.get(this.heap_index())),
                    Some(HeapObj::Temporal { kind: 1, .. })
                ) {
                    return Err(Thrown(format!(
                        "TypeError: Temporal.PlainDate.prototype.{m} called on incompatible receiver"
                    )));
                }
                self.temporal_method(this.heap_index(), m, args)?.unwrap_or(Value::UNDEFINED)
            }
            PLAINDATE_FROM => {
                let (y, m, d) = self.to_plain_date(a0)?;
                self.make_plain_date(y, m, d)?
            }
            PLAINDATE_COMPARE => {
                let a = self.to_plain_date(a0)?;
                let b = self.to_plain_date(a1)?;
                let ea = iso_to_epoch_days(a.0, a.1, a.2);
                let eb = iso_to_epoch_days(b.0, b.1, b.2);
                Value::num(if ea < eb { -1.0 } else if ea > eb { 1.0 } else { 0.0 })
            }
            _ if (PT_M_BASE..PT_M_BASE + PLAINTIME_METHODS.len() as u16).contains(&id) => {
                let m = PLAINTIME_METHODS[(id - PT_M_BASE) as usize];
                if !matches!(
                    this.is_heap().then(|| self.heap.get(this.heap_index())),
                    Some(HeapObj::Temporal { kind: 2, .. })
                ) {
                    return Err(Thrown(format!(
                        "TypeError: Temporal.PlainTime.prototype.{m} called on incompatible receiver"
                    )));
                }
                self.temporal_method(this.heap_index(), m, args)?.unwrap_or(Value::UNDEFINED)
            }
            PLAINTIME_FROM => {
                let f = self.to_plain_time(a0)?;
                self.make_plain_time(f)?
            }
            PLAINTIME_COMPARE => {
                let a = self.to_plain_time(a0)?;
                let b = self.to_plain_time(a1)?;
                let (ta, tb) = (time_to_ns(&a), time_to_ns(&b));
                Value::num(if ta < tb { -1.0 } else if ta > tb { 1.0 } else { 0.0 })
            }
            _ if (PDT_M_BASE..PDT_M_BASE + PLAINDATETIME_METHODS.len() as u16).contains(&id) => {
                let m = PLAINDATETIME_METHODS[(id - PDT_M_BASE) as usize];
                if !matches!(
                    this.is_heap().then(|| self.heap.get(this.heap_index())),
                    Some(HeapObj::Temporal { kind: 3, .. })
                ) {
                    return Err(Thrown(format!(
                        "TypeError: Temporal.PlainDateTime.prototype.{m} called on incompatible receiver"
                    )));
                }
                self.temporal_method(this.heap_index(), m, args)?.unwrap_or(Value::UNDEFINED)
            }
            PLAINDATETIME_FROM => {
                let f = self.to_plain_date_time(a0)?;
                self.make_plain_date_time(f)?
            }
            PLAINDATETIME_COMPARE => {
                let a = self.to_plain_date_time(a0)?;
                let b = self.to_plain_date_time(a1)?;
                let an = iso_to_epoch_days(a[0], a[1], a[2]) as i128 * 86_400_000_000_000
                    + time_to_ns(&[a[3], a[4], a[5], a[6], a[7], a[8]]);
                let bn = iso_to_epoch_days(b[0], b[1], b[2]) as i128 * 86_400_000_000_000
                    + time_to_ns(&[b[3], b[4], b[5], b[6], b[7], b[8]]);
                Value::num(if an < bn { -1.0 } else if an > bn { 1.0 } else { 0.0 })
            }
            _ if (INST_M_BASE..INST_M_BASE + INSTANT_METHODS.len() as u16).contains(&id) => {
                let m = INSTANT_METHODS[(id - INST_M_BASE) as usize];
                if !matches!(
                    this.is_heap().then(|| self.heap.get(this.heap_index())),
                    Some(HeapObj::Temporal { kind: 4, .. })
                ) {
                    return Err(Thrown(format!(
                        "TypeError: Temporal.Instant.prototype.{m} called on incompatible receiver"
                    )));
                }
                self.temporal_method(this.heap_index(), m, args)?.unwrap_or(Value::UNDEFINED)
            }
            INST_FROM => {
                let ns = self.to_instant_ns(a0)?;
                self.make_instant(ns)?
            }
            INST_FROM_EPOCH_MS => {
                let ns = (self.to_number(a0)? as i128) * 1_000_000;
                self.make_instant(ns)?
            }
            INST_FROM_EPOCH_SEC => {
                let ns = (self.to_number(a0)? as i128) * 1_000_000_000;
                self.make_instant(ns)?
            }
            INST_FROM_EPOCH_NS => {
                let ns = self.to_bigint(a0)?;
                self.make_instant(ns)?
            }
            INST_FROM_EPOCH_US => {
                let ns = self.to_bigint(a0)? * 1_000;
                self.make_instant(ns)?
            }
            INST_COMPARE => {
                let a = self.to_instant_ns(a0)?;
                let b = self.to_instant_ns(a1)?;
                Value::num(if a < b { -1.0 } else if a > b { 1.0 } else { 0.0 })
            }
            _ if (PYM_M_BASE..PYM_M_BASE + PLAINYEARMONTH_METHODS.len() as u16).contains(&id) => {
                let m = PLAINYEARMONTH_METHODS[(id - PYM_M_BASE) as usize];
                if !matches!(
                    this.is_heap().then(|| self.heap.get(this.heap_index())),
                    Some(HeapObj::Temporal { kind: 5, .. })
                ) {
                    return Err(Thrown(format!(
                        "TypeError: Temporal.PlainYearMonth.prototype.{m} called on incompatible receiver"
                    )));
                }
                self.temporal_method(this.heap_index(), m, args)?.unwrap_or(Value::UNDEFINED)
            }
            PLAINYEARMONTH_FROM => {
                let (y, m, rd) = self.to_plain_year_month(a0)?;
                self.make_plain_year_month(y, m, rd)?
            }
            PLAINYEARMONTH_COMPARE => {
                let a = self.to_plain_year_month(a0)?;
                let b = self.to_plain_year_month(a1)?;
                let ka = a.0 * 12 + a.1;
                let kb = b.0 * 12 + b.1;
                Value::num(if ka < kb { -1.0 } else if ka > kb { 1.0 } else { 0.0 })
            }
            _ if (PMD_M_BASE..PMD_M_BASE + PLAINMONTHDAY_METHODS.len() as u16).contains(&id) => {
                let m = PLAINMONTHDAY_METHODS[(id - PMD_M_BASE) as usize];
                if !matches!(
                    this.is_heap().then(|| self.heap.get(this.heap_index())),
                    Some(HeapObj::Temporal { kind: 6, .. })
                ) {
                    return Err(Thrown(format!(
                        "TypeError: Temporal.PlainMonthDay.prototype.{m} called on incompatible receiver"
                    )));
                }
                self.temporal_method(this.heap_index(), m, args)?.unwrap_or(Value::UNDEFINED)
            }
            PLAINMONTHDAY_FROM => {
                let (ry, m, d) = self.to_plain_month_day(a0)?;
                self.make_plain_month_day(m, d, ry)?
            }
            // `Array.prototype.<m>` / `String.prototype.<m>` invoked as a value
            // (`.call`/`.apply`/`.bind` or `m()`): dispatch on the `this` receiver.
            _ if native::proto_method(id).is_some() => {
                let (m, kind, _len) = native::proto_method(id).unwrap();
                // A boxed primitive receiver unwraps to its [[PrimitiveValue]] so the
                // method runs on the primitive (`new Number(5).toFixed(2)`).
                let this = match this.is_heap().then(|| self.heap.get(this.heap_index())) {
                    Some(HeapObj::Boxed { value, .. }) => *value,
                    _ => this,
                };
                // Number/Boolean receivers are primitive values; the rest are heap.
                if kind == 2 {
                    self.number_method(this, m, args)?.unwrap_or(Value::UNDEFINED)
                } else if kind == 5 {
                    self.boolean_method(this, m)
                } else if kind == 1 {
                    // String methods are generic: RequireObjectCoercible(this) then
                    // ToString(this), so `String.prototype.slice.call(123, …)` works.
                    let s_idx = if this.is_heap() && self.heap.is_str_like(this.heap_index()) {
                        this.heap_index()
                    } else if this == Value::UNDEFINED || this == Value::NULL {
                        return Err(Thrown(format!(
                            "TypeError: String.prototype.{m} called on null or undefined"
                        )));
                    } else {
                        let s = self.to_js_string(this)?;
                        self.alloc_str(s).heap_index()
                    };
                    self.string_method(s_idx, m, args)?.unwrap_or(Value::UNDEFINED)
                } else if !this.is_heap() {
                    return Err(Thrown(format!(
                        "TypeError: prototype method {m} called on {}",
                        self.display(this)
                    )));
                } else {
                    let r = match kind {
                        0 => self.array_method(this.heap_index(), m, args)?,
                        1 => self.string_method(this.heap_index(), m, args)?,
                        3 => self.set_method(this.heap_index(), m, args)?,
                        4 => self.map_method(this.heap_index(), m, args)?,
                        6 => self.date_method(this.heap_index(), m, args)?,
                        _ => self.promise_method(this.heap_index(), m, args)?, // kind 7
                    };
                    r.unwrap_or(Value::UNDEFINED)
                }
            }
            _ => Value::UNDEFINED,
        })
    }

    /// Own ENUMERABLE keys / values / [k,v] entries of `obj` as an array (the
    /// shared core of `Object.keys`/`values`/`entries`).
    fn object_enum_own(&mut self, obj: Value, what: EnumWhat) -> Value {
        let pairs: Vec<(String, Value)> = if obj.is_heap() {
            match self.heap.get(obj.heap_index()) {
                HeapObj::Object(m) => m
                    .keys
                    .iter()
                    .cloned()
                    .zip(m.vals.iter().copied())
                    .zip(m.attrs.iter())
                    .filter(|((k, _), a)| a.enumerable && !is_hidden_key(k))
                    .map(|(kv, _)| kv)
                    .collect(),
                HeapObj::Array(items) => {
                    items.iter().enumerate().map(|(i, v)| (i.to_string(), *v)).collect()
                }
                _ => Vec::new(),
            }
        } else {
            Vec::new()
        };
        let out: Vec<Value> = pairs
            .into_iter()
            .map(|(k, v)| match what {
                EnumWhat::Keys => self.alloc_str(k),
                EnumWhat::Values => v,
                EnumWhat::Entries => {
                    let ks = self.alloc_str(k);
                    Value::heap(self.heap.alloc(HeapObj::Array(vec![ks, v])))
                }
            })
            .collect();
        Value::heap(self.heap.alloc(HeapObj::Array(out)))
    }

    /// Build a data property descriptor object `{value, writable, enumerable,
    /// configurable}` (for `Object.getOwnPropertyDescriptor`).
    fn make_data_descriptor(&mut self, value: Value, w: bool, e: bool, c: bool) -> Value {
        let mut m = ObjMap::new();
        m.set("value", value);
        m.set("writable", Value::bool(w));
        m.set("enumerable", Value::bool(e));
        m.set("configurable", Value::bool(c));
        Value::heap(self.heap.alloc(HeapObj::Object(m)))
    }

    /// Build an accessor descriptor object `{get, set, enumerable, configurable}`.
    fn make_accessor_descriptor(&mut self, get: Value, set: Value, e: bool, c: bool) -> Value {
        let mut m = ObjMap::new();
        m.set("get", get);
        m.set("set", set);
        m.set("enumerable", Value::bool(e));
        m.set("configurable", Value::bool(c));
        Value::heap(self.heap.alloc(HeapObj::Object(m)))
    }

    /// `Object.getOwnPropertyDescriptor(obj, key)` — the property's descriptor, or
    /// undefined for a missing own property / non-object.
    fn object_get_own_property_descriptor(&mut self, obj: Value, key: &str) -> Value {
        if !obj.is_heap() || is_private_key(key) {
            return Value::UNDEFINED; // private names aren't reflectable
        }
        let idx = obj.heap_index();
        // A callable's `name`/`length`: non-writable, non-enumerable, configurable.
        if (key == "name" || key == "length") && self.callable_has_intrinsic(obj, key) {
            if let Some(v) = self.callable_intrinsic_value(obj, key) {
                return self.make_data_descriptor(v, false, false, true);
            }
        }
        let own = match self.heap.get(idx) {
            HeapObj::Object(m) => m.pos(key).map(|i| (m.attrs[i], m.vals[i])),
            HeapObj::Array(items) => {
                if key == "length" {
                    let len = len_value(items.len());
                    return self.make_data_descriptor(len, true, false, false);
                }
                match key.parse::<usize>() {
                    Ok(i) if i < items.len() => {
                        let v = items[i];
                        return self.make_data_descriptor(v, true, true, true);
                    }
                    _ => return Value::UNDEFINED,
                }
            }
            // Class static members: data props, plus `static get`/`set` rendered
            // as an accessor descriptor (raw = getter, attr.setter = setter).
            HeapObj::Class(c) => {
                if let Some(i) = c.statics.pos(key) {
                    Some((c.statics.attrs[i], c.statics.vals[i]))
                } else if let Some((_, g)) = c.static_getters.iter().find(|(n, _)| n == key) {
                    let setter = c
                        .static_setters
                        .iter()
                        .find(|(n, _)| n == key)
                        .map(|(_, s)| *s)
                        .unwrap_or(Value::UNDEFINED);
                    let attr = PropAttr {
                        writable: false,
                        enumerable: false,
                        configurable: true,
                        accessor: true,
                        setter,
                    };
                    Some((attr, *g))
                } else if let Some((_, s)) = c.static_setters.iter().find(|(n, _)| n == key) {
                    let attr = PropAttr {
                        writable: false,
                        enumerable: false,
                        configurable: true,
                        accessor: true,
                        setter: *s,
                    };
                    Some((attr, Value::UNDEFINED))
                } else {
                    None
                }
            }
            // A function's assigned own properties (`fn.x = y`).
            HeapObj::Func(_) | HeapObj::Closure { .. } | HeapObj::Bound { .. } | HeapObj::Native(_) => {
                self.fn_props.get(&idx).and_then(|m| m.pos(key).map(|i| (m.attrs[i], m.vals[i])))
            }
            _ => None,
        };
        match own {
            Some((a, raw)) if a.accessor => {
                self.make_accessor_descriptor(raw, a.setter, a.enumerable, a.configurable)
            }
            Some((a, raw)) => self.make_data_descriptor(raw, a.writable, a.enumerable, a.configurable),
            None => Value::UNDEFINED,
        }
    }

    /// `Object.getOwnPropertyNames(obj)` — all own string keys (enumerable or not).
    fn object_own_property_names(&mut self, obj: Value) -> Value {
        // Collect the key strings under the (immutable) heap borrow, then allocate
        // the result strings afterwards (alloc needs `&mut self`).
        let mut keys: Vec<String> = Vec::new();
        if obj.is_heap() {
            let idx = obj.heap_index();
            // `length`, then `name` — the spec order for ordinary callables.
            let has_length = self.callable_has_intrinsic(obj, "length");
            let has_name = self.callable_has_intrinsic(obj, "name");
            match self.heap.get(idx) {
                // Private names (stored as "#x") are not reflectable own properties.
                HeapObj::Object(m) => {
                    keys.extend(m.keys.iter().filter(|k| !is_hidden_key(k)).cloned())
                }
                HeapObj::Array(items) => {
                    for i in 0..items.len() {
                        keys.push(i.to_string());
                    }
                    keys.push("length".to_string());
                }
                HeapObj::Class(c) => {
                    if has_length {
                        keys.push("length".to_string());
                    }
                    if has_name {
                        keys.push("name".to_string());
                    }
                    keys.extend(c.statics.keys.iter().filter(|k| !is_hidden_key(k)).cloned());
                    for (n, _) in &c.static_getters {
                        if !is_hidden_key(n) && !keys.iter().any(|k| k == n) {
                            keys.push(n.clone());
                        }
                    }
                    for (n, _) in &c.static_setters {
                        if !is_hidden_key(n) && !keys.iter().any(|k| k == n) {
                            keys.push(n.clone());
                        }
                    }
                }
                HeapObj::Func(_) | HeapObj::Closure { .. } | HeapObj::Bound { .. } | HeapObj::Native(_) => {
                    if has_length {
                        keys.push("length".to_string());
                    }
                    if has_name {
                        keys.push("name".to_string());
                    }
                    if let Some(m) = self.fn_props.get(&idx) {
                        keys.extend(m.keys.iter().filter(|k| !is_hidden_key(k)).cloned());
                    }
                }
                _ => {}
            }
        }
        let names: Vec<Value> = keys.into_iter().map(|k| self.alloc_str(k)).collect();
        Value::heap(self.heap.alloc(HeapObj::Array(names)))
    }

    /// `Object.getPrototypeOf(obj)` — the prototype: a class instance's is its
    /// class's `.prototype`; an `Object.create`d object's is the recorded proto;
    /// otherwise `null` (a plain object's real `Object.prototype` isn't modelled).
    fn object_get_prototype_of(&mut self, obj: Value) -> Value {
        if !obj.is_heap() {
            return Value::NULL;
        }
        let idx = obj.heap_index();
        // Proxy `getPrototypeOf` trap (errors degrade to null — this signature is
        // infallible; the throwing path is rare and used internally by instanceof).
        if let Some((target, handler, revoked)) = self.proxy_parts(idx) {
            if revoked {
                return Value::NULL;
            }
            if let Ok(Some(trap)) = self.proxy_trap(handler, "getPrototypeOf") {
                return self.call_value(trap, handler, &[target]).unwrap_or(Value::NULL);
            }
            return self.object_get_prototype_of(target);
        }
        if let Some(&p) = self.proto_of.get(&idx) {
            return p;
        }
        if idx == self.obj_proto {
            return Value::NULL; // Object.prototype's [[Prototype]] is null
        }
        // Built-in instance types delegate to their respective prototype (so
        // `Object.getPrototypeOf(new Map()) === Map.prototype` and `m instanceof Map`).
        let builtin_proto = match self.heap.get(idx) {
            HeapObj::Map { .. } => self.map_proto,
            HeapObj::Set(_) => self.set_proto,
            HeapObj::WeakMap { .. } => self.weakmap_proto,
            HeapObj::WeakSet(_) => self.weakset_proto,
            HeapObj::WeakRef(_) => self.weakref_proto,
            HeapObj::FinalizationRegistry { .. } => self.finreg_proto,
            HeapObj::Iterator { proto, .. } => *proto,
            HeapObj::Boxed { kind, .. } => match kind {
                0 => self.str_proto,
                1 => self.num_proto,
                _ => self.bool_proto,
            },
            HeapObj::Date(_) => self.date_proto,
            HeapObj::Promise { .. } => self.promise_proto,
            _ => 0,
        };
        if builtin_proto != 0 {
            return Value::heap(builtin_proto);
        }
        // kind: 0=plain/instance object, 1=callable, 2=array, 3=other.
        let (class, kind) = match self.heap.get(idx) {
            HeapObj::Object(m) => (m.class, 0u8),
            HeapObj::Func(_) | HeapObj::Closure { .. } | HeapObj::Bound { .. } | HeapObj::Native(_) => {
                (None, 1)
            }
            HeapObj::Array(_) => (None, 2),
            _ => (None, 3),
        };
        match kind {
            0 => {
                if let Some(cidx) = class {
                    if let Some(p) = self.prototype_of(Value::heap(cidx)) {
                        return p;
                    }
                }
                if self.obj_proto != 0 {
                    Value::heap(self.obj_proto)
                } else {
                    Value::NULL
                }
            }
            1 if self.fn_proto != 0 => Value::heap(self.fn_proto),
            2 if self.arr_proto != 0 => Value::heap(self.arr_proto),
            _ => Value::NULL,
        }
    }

    /// Read a property-descriptor object's fields (present-or-absent) for
    /// `Object.defineProperty`. Throws if `desc` is not an object.
    fn read_descriptor(
        &mut self,
        desc: Value,
    ) -> Result<(Option<Value>, Option<Value>, Option<Value>, Option<bool>, Option<bool>, Option<bool>), Thrown>
    {
        if !desc.is_heap() || !matches!(self.heap.get(desc.heap_index()), HeapObj::Object(_)) {
            return Err(Thrown("TypeError: Property description must be an object".into()));
        }
        let idx = desc.heap_index();
        let present = |vm: &Self, k: &str| -> bool {
            matches!(vm.heap.get(idx), HeapObj::Object(m) if m.pos(k).is_some())
        };
        let value = if present(self, "value") { Some(self.get_prop(desc, "value")?) } else { None };
        let get = if present(self, "get") { Some(self.get_prop(desc, "get")?) } else { None };
        let set = if present(self, "set") { Some(self.get_prop(desc, "set")?) } else { None };
        let writable = if present(self, "writable") {
            let v = self.get_prop(desc, "writable")?;
            Some(self.truthy(v))
        } else {
            None
        };
        let enumerable = if present(self, "enumerable") {
            let v = self.get_prop(desc, "enumerable")?;
            Some(self.truthy(v))
        } else {
            None
        };
        let configurable = if present(self, "configurable") {
            let v = self.get_prop(desc, "configurable")?;
            Some(self.truthy(v))
        } else {
            None
        };
        Ok((value, get, set, writable, enumerable, configurable))
    }

    /// `Object.defineProperty(obj, key, descriptor)` — define/redefine an own
    /// property with explicit attributes (unspecified attrs default to false on a
    /// new property; an existing non-configurable property rejects most changes).
    fn object_define_property(&mut self, obj: Value, key: &str, desc: Value) -> Result<(), Thrown> {
        if !obj.is_heap() {
            return Err(Thrown("TypeError: Object.defineProperty called on non-object".into()));
        }
        let idx = obj.heap_index();
        // Array: a numeric-index data descriptor sets the element; `length` resizes.
        // (Index accessors / extra named props aren't modeled — accepted as a no-op
        // so the definition doesn't abort the program, matching common test setup.)
        if let HeapObj::Array(_) = self.heap.get(idx) {
            let (value, get, set, ..) = self.read_descriptor(desc)?;
            if let Ok(i) = key.parse::<usize>() {
                if get.is_none() && set.is_none() {
                    let v = value.unwrap_or(Value::UNDEFINED);
                    if let HeapObj::Array(items) = self.heap.get_mut(idx) {
                        if i >= items.len() {
                            items.resize(i + 1, Value::UNDEFINED);
                        }
                        items[i] = v;
                    }
                    self.heap.bump_version(idx);
                }
                return Ok(());
            }
            if key == "length" {
                if let Some(v) = value {
                    let n = self.to_number(v)?;
                    if !(n >= 0.0 && n.fract() == 0.0 && n < 4_294_967_296.0) {
                        return Err(Thrown("RangeError: Invalid array length".into()));
                    }
                    if let HeapObj::Array(items) = self.heap.get_mut(idx) {
                        items.resize(n as usize, Value::UNDEFINED);
                    }
                    self.heap.bump_version(idx);
                }
            }
            return Ok(());
        }
        // TypedArray: a numeric-index data descriptor writes the element.
        if let HeapObj::TypedArray { .. } = self.heap.get(idx) {
            let (value, get, set, ..) = self.read_descriptor(desc)?;
            if get.is_none() && set.is_none() {
                if let Ok(i) = key.parse::<usize>() {
                    self.ta_element_set(idx, i, value.unwrap_or(Value::UNDEFINED))?;
                }
            }
            return Ok(());
        }
        // 0 = plain object, 1 = class (own props live in `statics`), 2 = callable
        // (own props live in `fn_props`).
        let target = match self.heap.get(idx) {
            HeapObj::Object(_) => 0u8,
            HeapObj::Class(_) => 1,
            HeapObj::Func(_) | HeapObj::Closure { .. } | HeapObj::Bound { .. } | HeapObj::Native(_) => 2,
            _ => return Err(Thrown("TypeError: Object.defineProperty called on non-object".into())),
        };
        // A callable's/class's `name`/`length`/`prototype` are synthesized; accept
        // the call but don't shadow them (full redefinition isn't modelled).
        if target != 0 && matches!(key, "name" | "length" | "prototype") {
            return Ok(());
        }
        let (value, get, set, d_wr, d_en, d_cf) = self.read_descriptor(desc)?;
        let existing = match self.heap.get(idx) {
            HeapObj::Object(m) => m.pos(key).map(|i| (m.attrs[i], m.vals[i])),
            HeapObj::Class(c) => c.statics.pos(key).map(|i| (c.statics.attrs[i], c.statics.vals[i])),
            _ => self.fn_props.get(&idx).and_then(|m| m.pos(key).map(|i| (m.attrs[i], m.vals[i]))),
        };
        let is_accessor = get.is_some() || set.is_some();
        // Start from the existing attrs (redefine) or all-false (new property).
        let (mut wr, mut en, mut cf) = match existing {
            Some((a, _)) => (a.writable, a.enumerable, a.configurable),
            None => (false, false, false),
        };
        if let Some(b) = d_wr {
            wr = b;
        }
        if let Some(b) = d_en {
            en = b;
        }
        if let Some(b) = d_cf {
            cf = b;
        }
        // A non-configurable existing property rejects illegal redefinitions.
        if let Some((a, oldv)) = existing {
            if !a.configurable {
                let make_cfg = d_cf == Some(true);
                let change_enum = d_en.is_some_and(|b| b != a.enumerable);
                let change_kind = is_accessor != a.accessor;
                let make_writable = !a.writable && d_wr == Some(true);
                let change_frozen_value =
                    !a.accessor && !a.writable && value.is_some_and(|v| v != oldv);
                if make_cfg || change_enum || change_kind || make_writable || change_frozen_value {
                    return Err(Thrown(format!("TypeError: Cannot redefine property: {key}")));
                }
            }
        }
        // Defining a brand-new property requires the object to be extensible.
        if existing.is_none() {
            let extensible = match self.heap.get(idx) {
                HeapObj::Object(m) => m.extensible,
                _ => true,
            };
            if !extensible {
                return Err(Thrown(format!(
                    "TypeError: Cannot define property {key}, object is not extensible"
                )));
            }
        }
        let attr = PropAttr {
            writable: wr,
            enumerable: en,
            configurable: cf,
            accessor: is_accessor,
            setter: set.unwrap_or(Value::UNDEFINED),
        };
        let stored = if is_accessor {
            get.unwrap_or(Value::UNDEFINED)
        } else {
            value.or(existing.map(|(_, v)| v)).unwrap_or(Value::UNDEFINED)
        };
        match target {
            0 => {
                if let HeapObj::Object(m) = self.heap.get_mut(idx) {
                    m.define(key, stored, attr);
                }
            }
            1 => {
                if let HeapObj::Class(c) = self.heap.get_mut(idx) {
                    c.statics.define(key, stored, attr);
                }
            }
            _ => {
                self.fn_props.entry(idx).or_insert_with(ObjMap::new).define(key, stored, attr);
            }
        }
        self.heap.bump_version(idx);
        Ok(())
    }

    /// `Object.defineProperties(obj, props)` — define each own enumerable key of
    /// `props` as a descriptor on `obj`.
    fn object_define_properties(&mut self, obj: Value, props: Value) -> Result<(), Thrown> {
        if !props.is_heap() {
            return Ok(());
        }
        let keys: Vec<String> = match self.heap.get(props.heap_index()) {
            HeapObj::Object(m) => m
                .keys
                .iter()
                .zip(m.attrs.iter())
                .filter(|(_, a)| a.enumerable)
                .map(|(k, _)| k.clone())
                .collect(),
            _ => Vec::new(),
        };
        for k in keys {
            let desc = self.get_prop(props, &k)?;
            self.object_define_property(obj, &k, desc)?;
        }
        Ok(())
    }

    /// The `(name, length)` of a callable value (function, closure, or class) for
    /// its `.name`/`.length` properties — `None` for non-callables. A synthetic
    /// proto name (`<arrow>`, `<script>`, …) reads as the empty string (anonymous).
    /// `globalThis.<name>`: the value of the reserved global slot named `name`
    /// (or None if there is no such global). Backs property access on globalThis.
    fn global_by_name(&self, name: &str) -> Option<Value> {
        let slot = self.program.global_names.iter().position(|n| n == name)?;
        // A never-declared slot reads as "absent" for `globalThis.x` (→ undefined),
        // not the internal sentinel.
        match self.globals.get(slot).copied() {
            Some(v) if v.is_uninitialized() => None,
            other => other,
        }
    }

    /// Look up `key` on a built-in prototype object (`arr_proto`/`str_proto`),
    /// returning the method value (or undefined). Lets primitive array/string
    /// values expose their methods as first-class values.
    fn proto_member(&self, proto: u32, key: &str) -> Value {
        if proto != 0 {
            if let HeapObj::Object(m) = self.heap.get(proto) {
                if let Some(v) = m.get(key) {
                    return v;
                }
            }
        }
        // The type prototypes (Array/String/Number/Map/…) inherit from
        // Object.prototype, so a method-as-value miss falls back there:
        // `[].hasOwnProperty`, `(5).isPrototypeOf`, etc.
        if self.obj_proto != 0 && proto != self.obj_proto {
            if let HeapObj::Object(m) = self.heap.get(self.obj_proto) {
                if let Some(v) = m.get(key) {
                    return v;
                }
            }
        }
        Value::UNDEFINED
    }

    fn callable_name_length(&self, obj: Value) -> Option<(String, i32)> {
        let clean = |n: &str| -> String {
            if n.starts_with('<') { String::new() } else { n.to_string() }
        };
        match self.heap.get(obj.heap_index()) {
            HeapObj::Func(fid) => {
                let p = &self.program.functions[*fid as usize];
                Some((clean(&p.name), p.param_count as i32))
            }
            HeapObj::Closure { func, .. } => {
                let p = &self.program.functions[*func as usize];
                Some((clean(&p.name), p.param_count as i32))
            }
            HeapObj::Class(c) => {
                let len = c
                    .ctor
                    .map(|f| self.program.functions[f as usize].param_count as i32)
                    .unwrap_or(0);
                Some((clean(&c.name), len))
            }
            // A native value's `name`/`length`: a prototype method
            // (`Array.prototype.map.name === "map"`, length 1) or a static/namespace
            // method (`Object.keys.name === "keys"`, `Reflect.get.length === 2`).
            HeapObj::Native(id) => native::proto_method(*id)
                .map(|(n, _, l)| (n.to_string(), l as i32))
                .or_else(|| native::math_method(*id).map(|(n, _, l)| (n.to_string(), l as i32)))
                .or_else(|| native::static_name_length(*id).map(|(n, l)| (n.to_string(), l as i32))),
            _ => None,
        }
    }

    /// Does this callable expose `key` (`name`/`length`) as an own property right
    /// now? True for any named callable unless that intrinsic was `delete`d.
    fn callable_has_intrinsic(&self, obj: Value, key: &str) -> bool {
        let bit = match key {
            "name" => 0u8,
            "length" => 1u8,
            _ => return false,
        };
        if !obj.is_heap() || self.deleted_callable_intrinsics.contains(&(obj.heap_index(), bit)) {
            return false;
        }
        self.callable_name_length(obj).is_some()
    }

    /// The current value of a callable's `name`/`length` own property (allocating
    /// the name string), or None if absent/deleted.
    fn callable_intrinsic_value(&mut self, obj: Value, key: &str) -> Option<Value> {
        if !self.callable_has_intrinsic(obj, key) {
            return None;
        }
        let (nm, len) = self.callable_name_length(obj)?;
        Some(if key == "name" { self.alloc_str(nm) } else { Value::int(len) })
    }

    fn get_prop(&mut self, obj: Value, key: &str) -> Result<Value, Thrown> {
        // Proxy `get` trap (or fall through to the target).
        if obj.is_heap() {
            if let Some((target, handler, revoked)) = self.proxy_parts(obj.heap_index()) {
                if revoked {
                    return Err(Thrown("TypeError: Cannot perform 'get' on a revoked proxy".into()));
                }
                return match self.proxy_trap(handler, "get")? {
                    Some(trap) => {
                        let kv = self.key_to_value(key);
                        self.call_value(trap, handler, &[target, kv, obj])
                    }
                    None => self.get_prop(target, key),
                };
            }
        }
        if !obj.is_heap() {
            // Reading a property of null/undefined throws a TypeError (matches
            // JS); other primitives (number/bool) have no own props here → undef.
            if obj.is_nullish() {
                return Err(Thrown(format!(
                    "TypeError: Cannot read properties of {} (reading '{key}')",
                    if obj.is_null() { "null" } else { "undefined" }
                )));
            }
            // A number/boolean PRIMITIVE delegates method-as-value access to
            // Number/Boolean.prototype (`(5).toFixed`, `true.valueOf`).
            if obj.is_number() {
                return Ok(self.proto_member(self.num_proto, key));
            }
            if obj.is_bool() {
                return Ok(self.proto_member(self.bool_proto, key));
            }
            return Ok(Value::UNDEFINED);
        }
        // A function's / class's `.name` and `.length` — synthesized own data
        // properties (configurable, so a prior `delete` suppresses them).
        if key == "name" || key == "length" {
            if let Some(v) = self.callable_intrinsic_value(obj, key) {
                return Ok(v);
            }
        }
        // A function's / class's `.prototype` (a lazily-created, stable object).
        if key == "prototype" {
            if let Some(p) = self.prototype_of(obj) {
                return Ok(p);
            }
        }
        // A RegExp's accessor-like own properties (source/flags/lastIndex + the
        // flag booleans) and its match-result Array's `.index`/`.input`/`.groups`.
        // Cloned out of the heap borrow before any allocation.
        if let HeapObj::RegExp { source, flags, last_index, .. } = self.heap.get(obj.heap_index()) {
            let (s, f, li) = (source.clone(), flags.clone(), *last_index);
            return self.regexp_get_prop(&s, &f, li, key);
        }
        if let Some(&(index, input, groups)) = self.regexp_match_extras.get(&obj.heap_index()) {
            match key {
                "index" => return Ok(index),
                "input" => return Ok(input),
                "groups" => return Ok(groups),
                _ => {}
            }
        }
        // TypedArray / ArrayBuffer / DataView instance properties.
        if let HeapObj::TypedArray { buffer, kind, byte_offset, length } = self.heap.get(obj.heap_index()) {
            let (buffer, kind, byte_offset, length) = (*buffer, *kind, *byte_offset, *length);
            let size = native::TA_KINDS[kind as usize].1;
            // A canonical numeric string index reads the element.
            if let Ok(i) = key.parse::<usize>() {
                return Ok(self.ta_element_get(obj.heap_index(), i));
            }
            return Ok(match key {
                "length" => Value::num(length as f64),
                "byteLength" => Value::num((length * size) as f64),
                "byteOffset" => Value::num(byte_offset as f64),
                "BYTES_PER_ELEMENT" => Value::num(size as f64),
                "buffer" => Value::heap(buffer),
                "@@toStringTag" => self.alloc_str(native::TA_KINDS[kind as usize].0.to_string()),
                _ => self.proto_member(self.ta_protos[kind as usize], key),
            });
        }
        if let HeapObj::ArrayBuffer { data, .. } = self.heap.get(obj.heap_index()) {
            let len = data.len();
            return Ok(match key {
                "byteLength" => Value::num(len as f64),
                _ => self.proto_member(self.arraybuffer_proto, key),
            });
        }
        if let HeapObj::DataView { buffer, byte_offset, byte_length } = self.heap.get(obj.heap_index()) {
            let (buffer, byte_offset, byte_length) = (*buffer, *byte_offset, *byte_length);
            return Ok(match key {
                "byteLength" => Value::num(byte_length as f64),
                "byteOffset" => Value::num(byte_offset as f64),
                "buffer" => Value::heap(buffer),
                _ => self.proto_member(self.dataview_proto, key),
            });
        }
        // Temporal.Duration: field getters + sign/blank; methods via the prototype.
        if let HeapObj::Temporal { kind: 0, .. } = self.heap.get(obj.heap_index()) {
            let f = self.duration_fields(obj.heap_index()).unwrap_or([0; 10]);
            if let Some(i) = native::DURATION_FIELDS.iter().position(|n| *n == key) {
                return Ok(Value::num(f[i] as f64));
            }
            return Ok(match key {
                "sign" => Value::num(Self::duration_sign(&f) as f64),
                "blank" => Value::bool(f.iter().all(|&x| x == 0)),
                _ => self.proto_member(self.duration_proto, key),
            });
        }
        // Temporal.PlainDate getters; methods via the prototype.
        if let HeapObj::Temporal { kind: 1, .. } = self.heap.get(obj.heap_index()) {
            let (y, m, d) = self.plain_date_fields(obj.heap_index()).unwrap_or((0, 0, 0));
            return Ok(match key {
                "year" => Value::num(y as f64),
                "month" => Value::num(m as f64),
                "day" => Value::num(d as f64),
                "dayOfWeek" => Value::num(iso_day_of_week(y, m, d) as f64),
                "dayOfYear" => {
                    Value::num((iso_to_epoch_days(y, m, d) - iso_to_epoch_days(y, 1, 1) + 1) as f64)
                }
                "weekOfYear" => Value::num(iso_week_of_year(y, m, d) as f64),
                "daysInMonth" => Value::num(days_in_month(y, m) as f64),
                "daysInYear" => Value::num(if is_leap_year(y) { 366.0 } else { 365.0 }),
                "daysInWeek" => Value::num(7.0),
                "monthsInYear" => Value::num(12.0),
                "inLeapYear" => Value::bool(is_leap_year(y)),
                "monthCode" => self.alloc_str(format!("M{m:02}")),
                "calendarId" => self.alloc_str("iso8601".to_string()),
                _ => self.proto_member(self.plaindate_proto, key),
            });
        }
        // Temporal.PlainTime getters; methods via the prototype.
        if let HeapObj::Temporal { kind: 2, .. } = self.heap.get(obj.heap_index()) {
            let f = self.plain_time_fields(obj.heap_index()).unwrap_or([0; 6]);
            return Ok(match key {
                "hour" => Value::num(f[0] as f64),
                "minute" => Value::num(f[1] as f64),
                "second" => Value::num(f[2] as f64),
                "millisecond" => Value::num(f[3] as f64),
                "microsecond" => Value::num(f[4] as f64),
                "nanosecond" => Value::num(f[5] as f64),
                "calendarId" => self.alloc_str("iso8601".to_string()),
                _ => self.proto_member(self.plaintime_proto, key),
            });
        }
        // Temporal.PlainDateTime getters (date + time); methods via the prototype.
        if let HeapObj::Temporal { kind: 3, .. } = self.heap.get(obj.heap_index()) {
            let f = self.pdt_fields(obj.heap_index()).unwrap_or([0; 9]);
            let (y, m, d) = (f[0], f[1], f[2]);
            return Ok(match key {
                "year" => Value::num(y as f64),
                "month" => Value::num(m as f64),
                "day" => Value::num(d as f64),
                "hour" => Value::num(f[3] as f64),
                "minute" => Value::num(f[4] as f64),
                "second" => Value::num(f[5] as f64),
                "millisecond" => Value::num(f[6] as f64),
                "microsecond" => Value::num(f[7] as f64),
                "nanosecond" => Value::num(f[8] as f64),
                "dayOfWeek" => Value::num(iso_day_of_week(y, m, d) as f64),
                "dayOfYear" => {
                    Value::num((iso_to_epoch_days(y, m, d) - iso_to_epoch_days(y, 1, 1) + 1) as f64)
                }
                "weekOfYear" => Value::num(iso_week_of_year(y, m, d) as f64),
                "daysInMonth" => Value::num(days_in_month(y, m) as f64),
                "daysInYear" => Value::num(if is_leap_year(y) { 366.0 } else { 365.0 }),
                "daysInWeek" => Value::num(7.0),
                "monthsInYear" => Value::num(12.0),
                "inLeapYear" => Value::bool(is_leap_year(y)),
                "monthCode" => self.alloc_str(format!("M{m:02}")),
                "calendarId" => self.alloc_str("iso8601".to_string()),
                _ => self.proto_member(self.plaindatetime_proto, key),
            });
        }
        // Temporal.Instant getters; methods via the prototype.
        if let HeapObj::Temporal { kind: 4, .. } = self.heap.get(obj.heap_index()) {
            let ns = self.instant_ns(obj.heap_index()).unwrap_or(0);
            return Ok(match key {
                "epochMilliseconds" => Value::num((ns / 1_000_000) as f64),
                "epochNanoseconds" => self.make_bigint(ns),
                "epochSeconds" => Value::num((ns / 1_000_000_000) as f64),
                "epochMicroseconds" => self.make_bigint(ns / 1_000),
                _ => self.proto_member(self.instant_proto, key),
            });
        }
        // Temporal.PlainYearMonth getters; methods via the prototype.
        if let HeapObj::Temporal { kind: 5, fields } = self.heap.get(obj.heap_index()) {
            let (y, m) = (fields[0], fields[1]);
            return Ok(match key {
                "year" => Value::num(y as f64),
                "month" => Value::num(m as f64),
                "monthCode" => self.alloc_str(format!("M{m:02}")),
                "daysInMonth" => Value::num(days_in_month(y, m) as f64),
                "daysInYear" => Value::num(if is_leap_year(y) { 366.0 } else { 365.0 }),
                "monthsInYear" => Value::num(12.0),
                "inLeapYear" => Value::bool(is_leap_year(y)),
                "era" | "eraYear" => Value::UNDEFINED,
                "calendarId" => self.alloc_str("iso8601".to_string()),
                _ => self.proto_member(self.plainyearmonth_proto, key),
            });
        }
        // Temporal.PlainMonthDay getters; methods via the prototype.
        if let HeapObj::Temporal { kind: 6, fields } = self.heap.get(obj.heap_index()) {
            let (m, d) = (fields[1], fields[2]);
            return Ok(match key {
                "monthCode" => self.alloc_str(format!("M{m:02}")),
                "day" => Value::num(d as f64),
                "calendarId" => self.alloc_str("iso8601".to_string()),
                _ => self.proto_member(self.plainmonthday_proto, key),
            });
        }
        // Own data/accessor property on a plain object. Extracted BEFORE the type
        // match so an accessor's getter can be invoked outside the heap borrow.
        let own = match self.heap.get(obj.heap_index()) {
            HeapObj::Object(m) => m.pos(key).map(|i| (m.attrs[i], m.vals[i])),
            _ => None,
        };
        if let Some((a, raw)) = own {
            if a.accessor {
                // `raw` is the getter (UNDEFINED ⇒ no getter ⇒ read is undefined).
                return if raw == Value::UNDEFINED { Ok(Value::UNDEFINED) } else { self.call_value(raw, obj, &[]) };
            }
            return Ok(raw);
        }
        match self.heap.get(obj.heap_index()) {
            HeapObj::Array(items) => {
                if key == "length" {
                    Ok(len_value(items.len()))
                } else if key == "raw" {
                    // A tagged-template strings array's `.raw` (side table).
                    Ok(self.template_raws.get(&obj.heap_index()).copied().unwrap_or(Value::UNDEFINED))
                } else {
                    // A method as a VALUE (`arr.map`, `arr.slice`, …) → Array.prototype.
                    Ok(self.proto_member(self.arr_proto, key))
                }
            }
            HeapObj::Str(s) => {
                if key == "length" {
                    Ok(len_value(s.char_len))
                } else {
                    Ok(self.proto_member(self.str_proto, key))
                }
            }
            HeapObj::Cons { len, .. } => {
                if key == "length" {
                    Ok(len_value(*len))
                } else {
                    Ok(self.proto_member(self.str_proto, key))
                }
            }
            HeapObj::Object(map) => {
                if let Some(v) = map.get(key) {
                    return Ok(v);
                }
                // `globalThis.X` → the reserved global slot named X.
                if obj.heap_index() == self.global_this && self.global_this != 0 {
                    if let Some(v) = self.global_by_name(key) {
                        return Ok(v);
                    }
                }
                // Own-property miss: walk the class chain for an inherited method
                // (return its func) or getter (invoke it with this = obj).
                let class = map.class;
                let (mut method, mut getter) = (None, None);
                let mut cur = class;
                while let Some(cidx) = cur {
                    match self.heap.get(cidx) {
                        HeapObj::Class(c) => {
                            if let Some((_, v)) = c.methods.iter().find(|(k, _)| k == key) {
                                method = Some(*v);
                                break;
                            }
                            if let Some((_, v)) = c.getters.iter().find(|(k, _)| k == key) {
                                getter = Some(*v);
                                break;
                            }
                            cur = c.parent;
                        }
                        _ => break,
                    }
                }
                if let Some(m) = method {
                    return Ok(m);
                }
                if let Some(g) = getter {
                    return self.call_value(g, obj, &[]);
                }
                // Own + class miss: delegate up the prototype chain — an explicit
                // `Object.create` proto, else a class instance's `C.prototype`
                // (carries `constructor` + inherited methods, and itself chains to
                // Object.prototype), else the base Object.prototype.
                let proto = if let Some(&p) = self.proto_of.get(&obj.heap_index()) {
                    p.is_heap().then_some(p)
                } else if let Some(cidx) = class {
                    self.prototype_of(Value::heap(cidx))
                } else if self.obj_proto != 0 && obj.heap_index() != self.obj_proto {
                    Some(Value::heap(self.obj_proto))
                } else {
                    None
                };
                match proto {
                    Some(p) => self.get_prop(p, key),
                    None => Ok(Value::UNDEFINED),
                }
            }
            // Static members are own properties of the class value; statics are
            // inherited, so walk the `extends` chain (`C.method`, `Sub.parentStatic`).
            // A `static get name()` is invoked with `this` = the class value.
            HeapObj::Class(c) => {
                if let Some(v) = c.statics.get(key) {
                    return Ok(v);
                }
                if let Some((_, g)) = c.static_getters.iter().find(|(k, _)| k == key) {
                    let g = *g;
                    return self.call_value(g, obj, &[]);
                }
                let mut cur = c.parent;
                while let Some(pidx) = cur {
                    match self.heap.get(pidx) {
                        HeapObj::Class(pc) => {
                            if let Some(v) = pc.statics.get(key) {
                                return Ok(v);
                            }
                            if let Some((_, g)) = pc.static_getters.iter().find(|(k, _)| k == key) {
                                let g = *g;
                                return self.call_value(g, obj, &[]);
                            }
                            cur = pc.parent;
                        }
                        _ => break,
                    }
                }
                Ok(Value::UNDEFINED)
            }
            // `map.size` / `set.size` — an accessor property, not a method.
            HeapObj::Map { keys, .. } if key == "size" => Ok(len_value(keys.len())),
            HeapObj::Set(items) if key == "size" => Ok(len_value(items.len())),
            // A method as a VALUE on a Map/Set/Date/Promise instance
            // (`new Map().set`, `d.getHours`) → the corresponding prototype.
            HeapObj::Map { .. } => Ok(self.proto_member(self.map_proto, key)),
            HeapObj::Set(_) => Ok(self.proto_member(self.set_proto, key)),
            HeapObj::WeakMap { .. } => Ok(self.proto_member(self.weakmap_proto, key)),
            HeapObj::WeakSet(_) => Ok(self.proto_member(self.weakset_proto, key)),
            HeapObj::WeakRef(_) => Ok(self.proto_member(self.weakref_proto, key)),
            HeapObj::FinalizationRegistry { .. } => Ok(self.proto_member(self.finreg_proto, key)),
            HeapObj::Iterator { proto, .. } => {
                let p = *proto;
                Ok(self.proto_member(p, key))
            }
            // A boxed primitive: `length` (String box) reads the wrapped string;
            // everything else resolves through the wrapped type's prototype.
            HeapObj::Boxed { kind, value } => {
                let (k, v) = (*kind, *value);
                if k == 0 && key == "length" {
                    return self.get_prop(v, "length");
                }
                let proto = match k {
                    0 => self.str_proto,
                    1 => self.num_proto,
                    _ => self.bool_proto,
                };
                Ok(self.proto_member(proto, key))
            }
            HeapObj::Date(_) => Ok(self.proto_member(self.date_proto, key)),
            HeapObj::Promise { .. } => Ok(self.proto_member(self.promise_proto, key)),
            // A Symbol: `.description` reads the wrapped description; methods
            // (toString/valueOf/constructor) resolve through Symbol.prototype.
            HeapObj::Symbol { desc, .. } => {
                if key == "description" {
                    return Ok(*desc);
                }
                Ok(self.proto_member(self.symbol_proto, key))
            }
            // A BigInt: methods (toString/valueOf/constructor) via BigInt.prototype.
            HeapObj::BigInt(_) => Ok(self.proto_member(self.bigint_proto, key)),
            // Functions / natives / bound functions: own props set on them
            // (`assert.sameValue`), then Function.prototype (`call`/`apply`/`bind`).
            _ if matches!(
                self.heap.get(obj.heap_index()),
                HeapObj::Func(_) | HeapObj::Closure { .. } | HeapObj::Bound { .. } | HeapObj::Native(_)
            ) =>
            {
                if let Some(m) = self.fn_props.get(&obj.heap_index()) {
                    if let Some(v) = m.get(key) {
                        return Ok(v);
                    }
                }
                if self.fn_proto != 0 {
                    if let HeapObj::Object(m) = self.heap.get(self.fn_proto) {
                        if let Some(v) = m.get(key) {
                            return Ok(v);
                        }
                    }
                }
                Ok(Value::UNDEFINED)
            }
            _ => Ok(Value::UNDEFINED),
        }
    }

    /// Evaluate a `Math.<fn>` call over `argc` argument registers (coerced to
    /// numbers). Mirrors JS semantics where they differ from Rust's f64 methods:
    /// `round` is half-up (so −2.5 → −2, not −3); `sign` preserves ±0 and maps
    /// NaN→NaN; `min`/`max` are NaN-sticky (any NaN arg ⇒ NaN).
    fn eval_math(&self, op: crate::bytecode::MathFn, base: usize, arg_base: u16, argc: u16) -> Result<f64, Thrown> {
        use crate::bytecode::MathFn as M;
        let arg = |i: u16| -> Result<f64, Thrown> {
            if i < argc {
                self.to_number(self.get(base, arg_base + i))
            } else {
                Ok(f64::NAN)
            }
        };
        Ok(match op {
            M::Min | M::Max | M::Hypot => {
                let mut acc = match op {
                    M::Min => f64::INFINITY,
                    M::Max => f64::NEG_INFINITY,
                    _ => 0.0, // Hypot: sum of squares
                };
                for i in 0..argc {
                    let v = arg(i)?;
                    acc = match op {
                        M::Min => {
                            if v.is_nan() || acc.is_nan() { f64::NAN } else { acc.min(v) }
                        }
                        M::Max => {
                            if v.is_nan() || acc.is_nan() { f64::NAN } else { acc.max(v) }
                        }
                        _ => acc + v * v,
                    };
                }
                if matches!(op, M::Hypot) { acc.sqrt() } else { acc }
            }
            M::Pow => arg(0)?.powf(arg(1)?),
            M::Atan2 => arg(0)?.atan2(arg(1)?),
            // Math.imul(a,b): ToUint32 multiply, result as signed int32.
            M::Imul => (to_uint32(arg(0)?).wrapping_mul(to_uint32(arg(1)?)) as i32) as f64,
            _ => math_unary(op, arg(0)?),
        })
    }

    /// `Math.<op>` reduced to a single f64 result (used by the `MathSpread`
    /// fallback for an unusual non-variadic spread like `Math.abs(...arr)`).
    fn eval_math_one(&self, op: crate::bytecode::MathFn, x: f64) -> f64 {
        math_unary(op, x)
    }

    /// Evaluate a Math method over an argument SLICE (the value-form `Math.abs`
    /// invoked as a native), mirroring `eval_math`'s register-based variant.
    fn eval_math_args(&self, op: crate::bytecode::MathFn, args: &[Value]) -> Result<f64, Thrown> {
        use crate::bytecode::MathFn as M;
        let arg = |i: usize| -> Result<f64, Thrown> {
            match args.get(i) {
                Some(v) => self.to_number(*v),
                None => Ok(f64::NAN),
            }
        };
        Ok(match op {
            M::Min | M::Max | M::Hypot => {
                let mut acc = match op {
                    M::Min => f64::INFINITY,
                    M::Max => f64::NEG_INFINITY,
                    _ => 0.0,
                };
                for i in 0..args.len() {
                    let v = arg(i)?;
                    acc = match op {
                        M::Min => if v.is_nan() || acc.is_nan() { f64::NAN } else { acc.min(v) },
                        M::Max => if v.is_nan() || acc.is_nan() { f64::NAN } else { acc.max(v) },
                        _ => acc + v * v,
                    };
                }
                if matches!(op, M::Hypot) { acc.sqrt() } else { acc }
            }
            M::Pow => arg(0)?.powf(arg(1)?),
            M::Atan2 => arg(0)?.atan2(arg(1)?),
            M::Imul => (to_uint32(arg(0)?).wrapping_mul(to_uint32(arg(1)?)) as i32) as f64,
            _ => math_unary(op, arg(0)?),
        })
    }

    /// The per-level indent string for `JSON.stringify`'s `space` argument: a
    /// number → that many spaces (clamped 0..10); a string → its first 10 chars;
    /// anything else → empty (compact output).
    fn json_indent(&self, space: Value) -> String {
        if space.is_number() {
            let n = space.as_f64();
            let n = if n.is_finite() && n > 0.0 { (n as usize).min(10) } else { 0 };
            " ".repeat(n)
        } else if space.is_heap() {
            match self.heap.str_cow(space.heap_index()) {
                Some(s) => s.chars().take(10).collect(),
                None => String::new(),
            }
        } else {
            String::new()
        }
    }

    /// Serialize `v` to JSON (`None` ⇒ omit: undefined / function). `indent` is
    /// the per-level pad (empty ⇒ compact); `depth` is the current nesting.
    fn json_value(&self, v: Value, indent: &str, depth: usize) -> Option<String> {
        if depth > 512 {
            return None; // guard against pathological / circular structures
        }
        if v.is_undefined() {
            return None;
        }
        if v.is_null() {
            return Some("null".to_string());
        }
        if v.is_bool() {
            return Some(if v.as_bool() { "true" } else { "false" }.to_string());
        }
        if v.is_number() {
            let n = v.as_f64();
            return Some(if n.is_finite() { fmt_f64(n) } else { "null".to_string() });
        }
        if !v.is_heap() {
            return None;
        }
        match self.heap.get(v.heap_index()) {
            HeapObj::Str(_) | HeapObj::Cons { .. } => {
                let s = self.heap.str_cow(v.heap_index()).unwrap();
                Some(json_quote(&s))
            }
            HeapObj::Func(_) | HeapObj::Closure { .. } => None, // functions are omitted
            HeapObj::Symbol { .. } => None,                    // symbols are omitted by JSON
            HeapObj::Array(items) => {
                let items = items.clone(); // release the heap borrow before recursing
                if items.is_empty() {
                    return Some("[]".to_string());
                }
                // A missing element value serializes as null inside an array.
                let parts: Vec<String> = items
                    .iter()
                    .map(|e| self.json_value(*e, indent, depth + 1).unwrap_or_else(|| "null".to_string()))
                    .collect();
                Some(wrap_json(&parts, '[', ']', indent, depth))
            }
            HeapObj::Object(map) => {
                let keys = map.keys.clone();
                let vals = map.vals.clone();
                let sep = if indent.is_empty() { ":" } else { ": " };
                let mut parts = Vec::new();
                for (k, val) in keys.iter().zip(vals.iter()) {
                    // Symbol-keyed (and private) properties are skipped by JSON.
                    if is_hidden_key(k) {
                        continue;
                    }
                    if let Some(vs) = self.json_value(*val, indent, depth + 1) {
                        parts.push(format!("{}{}{}", json_quote(k), sep, vs));
                    }
                }
                if parts.is_empty() {
                    return Some("{}".to_string());
                }
                Some(wrap_json(&parts, '{', '}', indent, depth))
            }
            // A Map/Set/Generator has no enumerable own properties, so
            // JSON.stringify renders it as an empty object (not omitted).
            HeapObj::Map { .. } | HeapObj::Set(_) | HeapObj::Generator { .. } => Some("{}".into()),
            _ => None,
        }
    }

    /// Parse a JSON string into a Value, or throw SyntaxError. Recursive-descent
    /// over the byte string (structure tokens are ASCII; string content is
    /// flushed as UTF-8 slices). Allocates heap objects/arrays/strings.
    fn json_parse(&mut self, src: &str) -> Result<Value, Thrown> {
        let mut i = 0;
        json_skip_ws(src.as_bytes(), &mut i);
        let v = self.json_parse_value(src, &mut i)?;
        json_skip_ws(src.as_bytes(), &mut i);
        if i != src.len() {
            return Err(Thrown("SyntaxError: Unexpected non-whitespace character after JSON".into()));
        }
        Ok(v)
    }

    fn json_parse_value(&mut self, src: &str, i: &mut usize) -> Result<Value, Thrown> {
        let b = src.as_bytes();
        match b.get(*i).copied() {
            Some(b'{') => self.json_parse_object(src, i),
            Some(b'[') => self.json_parse_array(src, i),
            Some(b'"') => {
                let s = json_parse_string(src, i)?;
                Ok(self.alloc_str(s))
            }
            Some(b't') => {
                json_expect(b, i, "true")?;
                Ok(Value::bool(true))
            }
            Some(b'f') => {
                json_expect(b, i, "false")?;
                Ok(Value::bool(false))
            }
            Some(b'n') => {
                json_expect(b, i, "null")?;
                Ok(Value::NULL)
            }
            Some(c) if c == b'-' || c.is_ascii_digit() => json_parse_number(b, i),
            _ => Err(Thrown("SyntaxError: Unexpected token in JSON".into())),
        }
    }

    fn json_parse_array(&mut self, src: &str, i: &mut usize) -> Result<Value, Thrown> {
        let b = src.as_bytes();
        *i += 1; // '['
        let mut items = Vec::new();
        json_skip_ws(b, i);
        if b.get(*i) == Some(&b']') {
            *i += 1;
            return Ok(Value::heap(self.heap.alloc(HeapObj::Array(items))));
        }
        loop {
            json_skip_ws(b, i);
            let v = self.json_parse_value(src, i)?;
            items.push(v);
            json_skip_ws(b, i);
            match b.get(*i) {
                Some(b',') => *i += 1,
                Some(b']') => {
                    *i += 1;
                    break;
                }
                _ => return Err(Thrown("SyntaxError: Expected ',' or ']' in JSON array".into())),
            }
        }
        Ok(Value::heap(self.heap.alloc(HeapObj::Array(items))))
    }

    fn json_parse_object(&mut self, src: &str, i: &mut usize) -> Result<Value, Thrown> {
        let b = src.as_bytes();
        *i += 1; // '{'
        let mut pairs: Vec<(String, Value)> = Vec::new();
        json_skip_ws(b, i);
        if b.get(*i) != Some(&b'}') {
            loop {
                json_skip_ws(b, i);
                if b.get(*i) != Some(&b'"') {
                    return Err(Thrown("SyntaxError: Expected property name string in JSON".into()));
                }
                let key = json_parse_string(src, i)?;
                json_skip_ws(b, i);
                if b.get(*i) != Some(&b':') {
                    return Err(Thrown("SyntaxError: Expected ':' in JSON object".into()));
                }
                *i += 1;
                json_skip_ws(b, i);
                let val = self.json_parse_value(src, i)?;
                pairs.push((key, val));
                json_skip_ws(b, i);
                match b.get(*i) {
                    Some(b',') => *i += 1,
                    Some(b'}') => break,
                    _ => return Err(Thrown("SyntaxError: Expected ',' or '}' in JSON object".into())),
                }
            }
        }
        *i += 1; // '}'
        let mut map = crate::heap::ObjMap::new();
        for (k, v) in pairs {
            map.set(&k, v);
        }
        Ok(Value::heap(self.heap.alloc(HeapObj::Object(map))))
    }

    /// JS `typeof` type-name. `null` is `"object"` (a historic quirk); functions
    /// and closures are `"function"`; arrays and objects are `"object"`.
    fn type_of(&self, v: Value) -> &'static str {
        if v.is_int() || v.is_double() {
            "number"
        } else if v.is_bool() {
            "boolean"
        } else if v.is_undefined() {
            "undefined"
        } else if v.is_null() {
            "object"
        } else if v.is_heap() {
            match self.heap.get(v.heap_index()) {
                HeapObj::Str(_) | HeapObj::Cons { .. } => "string",
                // A class is callable (with `new`), so `typeof C === "function"`.
                // Native builtins and bound functions are callable too.
                HeapObj::Func(_)
                | HeapObj::Closure { .. }
                | HeapObj::Class(_)
                | HeapObj::Native(_)
                | HeapObj::Bound { .. }
                | HeapObj::BoundResolver { .. } => "function",
                HeapObj::Cell(inner) => self.type_of(*inner), // see through an upvalue cell
                HeapObj::Proxy { target, .. } => self.type_of(*target), // typeof = target's
                HeapObj::Symbol { .. } => "symbol",
                HeapObj::BigInt(_) => "bigint",
                // The built-in constructor globals (Object/Array/Map/…) are callable.
                HeapObj::Object(m) if m.is_ctor => "function",
                // %Function.prototype% is itself a (no-op) callable function.
                HeapObj::Object(_) if v.heap_index() == self.fn_proto && self.fn_proto != 0 => "function",
                // `Symbol` is callable (typeof "function") but NOT a constructor
                // (so `new Symbol()` throws and IsConstructor is false).
                HeapObj::Object(_) if v.heap_index() == self.symbol_ctor && self.symbol_ctor != 0 => "function",
                HeapObj::Object(_) if v.heap_index() == self.bigint_ctor && self.bigint_ctor != 0 => "function",
                _ => "object", // Array, ordinary Object, namespace globals
            }
        } else {
            "undefined"
        }
    }

    /// `delete obj[key]` — remove an own property, returning the boolean result.
    /// Without property descriptors every own property is configurable, so this
    /// yields `true` (matching `delete` on a missing property / non-object too).
    /// An array element delete leaves a hole (reads as `undefined`), length kept.
    /// `delete obj[key]` honoring a Proxy `deleteProperty` trap (Result-returning
    /// so the trap can throw); else delegates to `delete_prop`.
    fn delete_property(&mut self, obj: Value, key: &str) -> Result<Value, Thrown> {
        if obj.is_heap() {
            if let Some((target, handler, revoked)) = self.proxy_parts(obj.heap_index()) {
                if revoked {
                    return Err(Thrown("TypeError: Cannot perform 'deleteProperty' on a revoked proxy".into()));
                }
                return match self.proxy_trap(handler, "deleteProperty")? {
                    Some(trap) => {
                        let kv = self.key_to_value(key);
                        let r = self.call_value(trap, handler, &[target, kv])?;
                        Ok(Value::bool(self.truthy(r)))
                    }
                    None => Ok(self.delete_prop(target, key)),
                };
            }
        }
        Ok(self.delete_prop(obj, key))
    }

    fn delete_prop(&mut self, obj: Value, key: &str) -> Value {
        if !obj.is_heap() {
            return Value::bool(true);
        }
        let idx = obj.heap_index();
        // A non-configurable own property cannot be deleted (`delete` yields false).
        if let HeapObj::Object(m) = self.heap.get(idx) {
            if let Some(i) = m.pos(key) {
                if !m.attrs[i].configurable {
                    return Value::bool(false);
                }
            }
        }
        // A callable's `name`/`length` are configurable: record the deletion so
        // the synthesized property stops appearing (own-property queries + reads).
        if (key == "name" || key == "length") && self.callable_has_intrinsic(obj, key) {
            self.deleted_callable_intrinsics
                .insert((idx, if key == "name" { 0 } else { 1 }));
            return Value::bool(true);
        }
        let removed = match self.heap.get_mut(idx) {
            HeapObj::Object(map) => map.remove(key),
            HeapObj::Array(items) => {
                if let Ok(i) = key.parse::<usize>() {
                    if i < items.len() {
                        items[i] = Value::UNDEFINED;
                    }
                }
                false // array slot stays (a hole); no version bump needed
            }
            HeapObj::Class(c) => c.statics.remove(key),
            // A function's assigned own property (`delete fn.x`).
            _ => self.fn_props.get_mut(&idx).map_or(false, |m| m.remove(key)),
        };
        if removed {
            self.heap.bump_version(idx); // a key was removed → slots shifted (IC)
        }
        Value::bool(true)
    }

    fn set_prop(&mut self, obj: Value, key: &str, val: Value) -> Result<(), Thrown> {
        if !obj.is_heap() {
            return Err(Thrown("TypeError: cannot set property of non-object".into()));
        }
        // Proxy `set` trap (or fall through to the target).
        if let Some((target, handler, revoked)) = self.proxy_parts(obj.heap_index()) {
            if revoked {
                return Err(Thrown("TypeError: Cannot perform 'set' on a revoked proxy".into()));
            }
            return match self.proxy_trap(handler, "set")? {
                Some(trap) => {
                    let kv = self.key_to_value(key);
                    self.call_value(trap, handler, &[target, kv, val, obj])?;
                    Ok(())
                }
                None => self.set_prop(target, key, val),
            };
        }
        let idx = obj.heap_index();
        // `re.lastIndex = n` — the only writable own property of a RegExp.
        if key == "lastIndex" && matches!(self.heap.get(idx), HeapObj::RegExp { .. }) {
            let n = self.to_number(val)?;
            let li = if n.is_finite() && n >= 0.0 { n as usize } else { 0 };
            self.set_regexp_last_index(idx, li);
            return Ok(());
        }
        // `arr.length = n` truncates (n < len) or extends-with-holes (n > len) a
        // dense array — a very common idiom (`arr.length = 0` clears it). Per JS,
        // n must be a non-negative integer < 2^32, else a RangeError.
        if key == "length" && matches!(self.heap.get(idx), HeapObj::Array(_)) {
            let n = self.to_number(val)?;
            if !(n >= 0.0 && n.fract() == 0.0 && n < 4_294_967_296.0) {
                return Err(Thrown("RangeError: Invalid array length".into()));
            }
            if let HeapObj::Array(items) = self.heap.get_mut(idx) {
                items.resize(n as usize, Value::UNDEFINED);
            }
            self.heap.bump_version(idx);
            return Ok(());
        }
        // A callable's `name`/`length` are non-writable: assignment is a sloppy
        // no-op while the synthesized intrinsic is present. (Once `delete`d it
        // falls through and becomes an ordinary assigned property.)
        if (key == "name" || key == "length") && self.callable_has_intrinsic(obj, key) {
            return Ok(());
        }
        // An OWN property's descriptor governs assignment: an accessor invokes its
        // setter; a non-writable data property silently ignores the write (sloppy).
        let own_attr = match self.heap.get(idx) {
            HeapObj::Object(m) => m.pos(key).map(|i| m.attrs[i]),
            _ => None,
        };
        if let Some(a) = own_attr {
            if a.accessor {
                if a.setter != Value::UNDEFINED {
                    self.call_value(a.setter, obj, &[val])?;
                }
                return Ok(()); // accessor with no setter ⇒ no-op (sloppy)
            }
            if !a.writable {
                return Ok(()); // non-writable own data property ⇒ no-op (sloppy)
            }
            // writable own data property → fall through to overwrite its value.
        }
        // A class instance with an inherited `set x(v)` accessor: assigning a
        // property that is NOT an own data property invokes the setter (own data
        // properties shadow an inherited accessor, per JS [[Set]]).
        if let HeapObj::Object(map) = self.heap.get(idx) {
            if map.class.is_some() && map.get(key).is_none() {
                if let Some(setter) = self.lookup_setter(map.class, key) {
                    self.call_value(setter, obj, &[val])?;
                    return Ok(());
                }
            }
        }
        // A function value's own property (`fn.x = …`, e.g. `assert.sameValue`)
        // lives in a side table (functions carry no inline property map).
        if matches!(
            self.heap.get(idx),
            HeapObj::Func(_) | HeapObj::Closure { .. } | HeapObj::Bound { .. } | HeapObj::Native(_)
        ) {
            // Reassigning `fn.prototype = obj` redirects what `new fn()` / the
            // `.prototype` getter see (the lazily-cached prototype object).
            if key == "prototype" && val.is_heap() {
                self.prototypes.insert(idx, val.heap_index());
            } else {
                self.fn_props.entry(idx).or_insert_with(ObjMap::new).set(key, val);
            }
            return Ok(());
        }
        // A `static set name(v)` (or getter-only accessor) on the class chain
        // intercepts the write before it becomes a static data property.
        if matches!(self.heap.get(idx), HeapObj::Class(_)) {
            match self.lookup_static_accessor(Some(idx), key) {
                Some(Some(setter)) => {
                    self.call_value(setter, obj, &[val])?;
                    return Ok(());
                }
                Some(None) => return Ok(()), // getter-only ⇒ sloppy no-op
                None => {}                    // fall through to a data write
            }
        }
        let mut added = false;
        match self.heap.get_mut(idx) {
            HeapObj::Object(map) => {
                // A non-extensible object rejects NEW own properties (sloppy no-op);
                // existing writable data properties still accept writes.
                if map.pos(key).is_none() && !map.extensible {
                    return Ok(());
                }
                added = map.set(key, val);
            }
            // Static-member assignment on a class value (`C.x = …`).
            HeapObj::Class(c) => {
                c.statics.set(key, val);
            }
            _ => {}
        }
        if added {
            self.heap.bump_version(idx); // invalidate any JIT inline cache (vals realloc)
        }
        Ok(())
    }

    /// Walk a class chain for a `set key(v)` accessor, returning the setter fn.
    fn lookup_setter(&self, class: Option<u32>, key: &str) -> Option<Value> {
        let mut cur = class;
        while let Some(cidx) = cur {
            match self.heap.get(cidx) {
                HeapObj::Class(c) => {
                    if let Some((_, v)) = c.setters.iter().find(|(k, _)| k == key) {
                        return Some(*v);
                    }
                    cur = c.parent;
                }
                _ => break,
            }
        }
        None
    }

    /// Resolve a static-property write against the class chain starting at heap
    /// index `start`. The first chain level that owns the key decides:
    ///   `Some(Some(setter))` → invoke `setter`;
    ///   `Some(None)`         → a getter-only accessor shadows the write (no-op);
    ///   `None`               → no accessor shadows it → write a static data prop.
    fn lookup_static_accessor(&self, start: Option<u32>, key: &str) -> Option<Option<Value>> {
        let mut cur = start;
        while let Some(cidx) = cur {
            match self.heap.get(cidx) {
                HeapObj::Class(c) => {
                    if let Some((_, s)) = c.static_setters.iter().find(|(k, _)| k == key) {
                        return Some(Some(*s));
                    }
                    if c.static_getters.iter().any(|(k, _)| k == key) {
                        return Some(None); // accessor with no setter ⇒ sloppy no-op
                    }
                    if c.statics.get(key).is_some() {
                        return None; // own data property shadows inherited accessors
                    }
                    cur = c.parent;
                }
                _ => break,
            }
        }
        None
    }

    /// Try a builtin method on an array or string receiver. Returns
    /// `Ok(Some(result))` when `name` is a recognised builtin, `Ok(None)` when
    /// it isn't (the caller then treats it as a user-defined method/property).
    ///
    /// Dispatch is split by receiver type into focused helpers so each stays
    /// readable. Methods that take a JS callback (`map`/`filter`/`reduce`/
    /// `sort`) clone the element snapshot out of the heap BEFORE invoking the
    /// callback, because a callback can mutate the same array (which would
    /// reallocate its `Vec` and invalidate any borrow held across the call).
    fn try_builtin_method(
        &mut self,
        recv: Value,
        name: &str,
        base: usize,
        arg_base: u16,
        argc: u16,
    ) -> Result<Option<Value>, Thrown> {
        // Gather args into a stack buffer for the common small-arity case (1-2
        // args for push/map/filter/…), avoiding a heap Vec alloc per call; only
        // a rare >8-arg call falls back to the heap.
        let mut stackbuf = [Value::UNDEFINED; 8];
        let heapbuf: Vec<Value>;
        let n = arg_base as usize;
        let args: &[Value] = if argc as usize <= stackbuf.len() {
            for i in 0..argc as usize {
                stackbuf[i] = self.regs[base + n + i];
            }
            &stackbuf[..argc as usize]
        } else {
            heapbuf = (0..argc as usize).map(|i| self.regs[base + n + i]).collect();
            &heapbuf
        };
        self.dispatch_builtin_method(recv, name, args)
    }

    /// Dispatch a builtin method on `recv` with an already-materialized args
    /// slice. Shared by `try_builtin_method` (args gathered from registers) and
    /// the spread method-call path (args taken from an array). `Ok(None)` means
    /// no builtin matched the receiver kind.
    fn dispatch_builtin_method(
        &mut self,
        recv: Value,
        name: &str,
        args: &[Value],
    ) -> Result<Option<Value>, Thrown> {
        // Number receivers (Int or double) support a small method set.
        if recv.is_number() {
            return self.number_method(recv, name, args);
        }
        if !recv.is_heap() {
            return Ok(None);
        }
        let idx = recv.heap_index();
        // Temporal receivers route to their own dispatch (so valueOf throws and
        // toString gives the ISO string, not the generic Object behavior).
        if matches!(self.heap.get(idx), HeapObj::Temporal { .. }) {
            return self.temporal_method(idx, name, args);
        }
        // ── Function.prototype.call / apply / bind (callable receivers) ──
        if self.is_callable(recv) {
            match name {
                "call" => {
                    let this = args.first().copied().unwrap_or(Value::UNDEFINED);
                    let rest: &[Value] = if args.len() > 1 { &args[1..] } else { &[] };
                    return Ok(Some(self.call_value(recv, this, rest)?));
                }
                "apply" => {
                    let this = args.first().copied().unwrap_or(Value::UNDEFINED);
                    let arr = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                    let callargs = if arr.is_heap() { self.iterate_to_vec(arr)? } else { Vec::new() };
                    return Ok(Some(self.call_value(recv, this, &callargs)?));
                }
                "bind" => {
                    let this = args.first().copied().unwrap_or(Value::UNDEFINED);
                    let bound: Vec<Value> = if args.len() > 1 { args[1..].to_vec() } else { Vec::new() };
                    let b = self.heap.alloc(HeapObj::Bound { target: recv, this, args: bound });
                    return Ok(Some(Value::heap(b)));
                }
                _ => {}
            }
        }
        // ── Boxed primitive: dispatch on the wrapped value (so new Number(5).
        // toFixed(), new String("x").charAt(), and valueOf/toString unwrap) — this
        // must precede the generic Object.prototype valueOf/toString below.
        if let HeapObj::Boxed { kind, value } = self.heap.get(idx) {
            let (k, v) = (*kind, *value);
            return match k {
                0 => self.string_method(v.heap_index(), name, args),
                1 => self.number_method(v, name, args),
                _ => match name {
                    "toString" | "valueOf" => Ok(Some(self.boolean_method(v, name))),
                    _ => Ok(None),
                },
            };
        }
        // ── Object.prototype methods (available on every object) ──
        match name {
            "hasOwnProperty" => {
                let key = self.key_of(args.first().copied().unwrap_or(Value::UNDEFINED));
                return Ok(Some(Value::bool(self.has_own_property(recv, &key))));
            }
            "propertyIsEnumerable" => {
                let key = self.key_of(args.first().copied().unwrap_or(Value::UNDEFINED));
                return Ok(Some(Value::bool(self.own_is_enumerable(recv, &key))));
            }
            "isPrototypeOf" => {
                let target = args.first().copied().unwrap_or(Value::UNDEFINED);
                return Ok(Some(Value::bool(self.is_prototype_of(recv, target))));
            }
            "valueOf" => return Ok(Some(recv)), // default valueOf returns the object
            "toString" => {
                // Generic `Object.prototype.toString` for a plain object; arrays /
                // numbers / dates etc. have their own toString in the type dispatch.
                if matches!(self.heap.get(idx), HeapObj::Object(_)) {
                    // An error instance inherits Error.prototype.toString ("name: message").
                    if self.is_error_instance(idx) {
                        return self.call_native(native::ERROR_TO_STRING, recv, args).map(Some);
                    }
                    return Ok(Some(self.alloc_str("[object Object]".to_string())));
                }
            }
            _ => {}
        }
        match self.heap.get(idx) {
            HeapObj::Array(_) => self.array_method(idx, name, args),
            HeapObj::Str(_) | HeapObj::Cons { .. } => self.string_method(idx, name, args),
            HeapObj::Map { .. } => self.map_method(idx, name, args),
            HeapObj::Set(_) => self.set_method(idx, name, args),
            HeapObj::Generator { .. } => self.generator_method(idx, name, args),
            HeapObj::AsyncGenerator(_) => Ok(self.async_generator_method(idx, name, args)),
            HeapObj::Promise { .. } => self.promise_method(idx, name, args),
            HeapObj::Date(_) => self.date_method(idx, name, args),
            HeapObj::TypedArray { .. } => self.typed_array_method(idx, name, args),
            HeapObj::DataView { .. } => self.dataview_method(idx, name, args),
            HeapObj::ArrayBuffer { .. } => self.arraybuffer_method(idx, name, args),
            _ => Ok(None),
        }
    }

    /// Infallible ToNumber (Symbol/etc. → NaN) — for index/length args in the
    /// TypedArray/DataView methods, where a closure can't propagate `?`.
    fn value_num(&self, v: Value) -> f64 {
        self.to_number(v).unwrap_or(f64::NAN)
    }

    /// `Object.prototype.toString`'s tag: the builtin tag (Array/Function/Error/…),
    /// overridden by a string `@@toStringTag` if present. (`[object <tag>]`.)
    fn object_to_string_tag(&mut self, this: Value) -> Result<String, Thrown> {
        if this.is_undefined() {
            return Ok("Undefined".to_string());
        }
        if this.is_null() {
            return Ok("Null".to_string());
        }
        let builtin = if this.is_heap() {
            match self.heap.get(this.heap_index()) {
                HeapObj::Str(_) | HeapObj::Cons { .. } => "String",
                HeapObj::Array(_) => "Array",
                HeapObj::Func(_) | HeapObj::Closure { .. } | HeapObj::Native(_) | HeapObj::Bound { .. } => {
                    "Function"
                }
                HeapObj::Boxed { kind: 0, .. } => "String",
                HeapObj::Boxed { kind: 1, .. } => "Number",
                HeapObj::Boxed { kind: 2, .. } => "Boolean",
                _ if self.error_name(this.heap_index()).is_some() => "Error",
                _ => "Object",
            }
        } else if this.is_number() {
            "Number"
        } else if this.is_bool() {
            "Boolean"
        } else {
            "Object"
        };
        // A string @@toStringTag overrides the builtin tag.
        if this.is_heap() {
            let tag = self.get_prop(this, "@@toStringTag")?;
            if tag.is_heap() && self.heap.is_str_like(tag.heap_index()) {
                return Ok(self.display(tag));
            }
        }
        Ok(builtin.to_string())
    }

    fn ta_len_kind(&self, idx: u32) -> (usize, u8) {
        match self.heap.get(idx) {
            HeapObj::TypedArray { length, kind, .. } => (*length, *kind),
            _ => (0, 0),
        }
    }
    /// Snapshot a TypedArray's elements as Values (numbers / BigInts).
    fn ta_snapshot(&mut self, idx: u32) -> Vec<Value> {
        let len = self.ta_len_kind(idx).0;
        (0..len).map(|i| self.ta_element_get(idx, i)).collect()
    }
    /// Build a fresh TypedArray of `kind` from element Values (coerced/encoded).
    fn ta_build_from(&mut self, kind: u8, vals: &[Value]) -> Result<Value, Thrown> {
        let size = native::TA_KINDS[kind as usize].1;
        let buf = self.alloc_array_buffer(vals.len() * size);
        let ta = self.alloc_typed_array(buf, kind, 0, vals.len());
        for (i, v) in vals.iter().enumerate() {
            self.ta_element_set(ta.heap_index(), i, *v)?;
        }
        Ok(ta)
    }

    /// `%TypedArray%.prototype` methods (most mirror Array.prototype, but map/filter/
    /// slice/etc. return TypedArrays and `sort` is numeric by default). `idx` is the
    /// receiver TypedArray's heap index.
    fn typed_array_method(&mut self, idx: u32, name: &str, args: &[Value]) -> Result<Option<Value>, Thrown> {
        if !matches!(self.heap.get(idx), HeapObj::TypedArray { .. }) {
            return Ok(None);
        }
        let (len, kind) = self.ta_len_kind(idx);
        let recv = Value::heap(idx);
        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        let a1 = args.get(1).copied().unwrap_or(Value::UNDEFINED);
        // Resolve a relative index (negative = from end) into [0,len].
        let rel = |v: Value, def: usize, this: &Self| -> usize {
            if v == Value::UNDEFINED {
                return def;
            }
            let n = this.value_num(v);
            if n.is_nan() {
                0
            } else if n < 0.0 {
                ((len as f64 + n).max(0.0)) as usize
            } else {
                (n as usize).min(len)
            }
        };
        match name {
            "at" => {
                let n = self.value_num(a0);
                let i = if n < 0.0 { len as f64 + n } else { n };
                Ok(Some(if i >= 0.0 && (i as usize) < len {
                    self.ta_element_get(idx, i as usize)
                } else {
                    Value::UNDEFINED
                }))
            }
            "join" => {
                let sep = if a0 == Value::UNDEFINED { ",".to_string() } else { self.to_js_string(a0)? };
                let parts: Vec<String> = (0..len).map(|i| self.ta_elem_string(idx, i)).collect();
                Ok(Some(self.alloc_str(parts.join(&sep))))
            }
            "toString" => {
                let parts: Vec<String> = (0..len).map(|i| self.ta_elem_string(idx, i)).collect();
                Ok(Some(self.alloc_str(parts.join(","))))
            }
            "indexOf" | "lastIndexOf" | "includes" => {
                let snap = self.ta_snapshot(idx);
                let mut found: i64 = -1;
                if name == "lastIndexOf" {
                    for i in (0..snap.len()).rev() {
                        if self.values_strict_eq(snap[i], a0) {
                            found = i as i64;
                            break;
                        }
                    }
                } else {
                    for (i, &e) in snap.iter().enumerate() {
                        let eq = if name == "includes" {
                            self.same_value_zero(e, a0)
                        } else {
                            self.values_strict_eq(e, a0)
                        };
                        if eq {
                            found = i as i64;
                            break;
                        }
                    }
                }
                Ok(Some(if name == "includes" {
                    Value::bool(found >= 0)
                } else {
                    Value::num(found as f64)
                }))
            }
            "forEach" | "map" | "filter" | "find" | "findIndex" | "findLast" | "findLastIndex"
            | "every" | "some" => {
                if !self.is_callable(a0) {
                    return Err(Thrown(format!("TypeError: {name} callback is not a function")));
                }
                let snap = self.ta_snapshot(idx);
                let mut mapped: Vec<Value> = Vec::new();
                let order: Vec<usize> = if name == "findLast" || name == "findLastIndex" {
                    (0..snap.len()).rev().collect()
                } else {
                    (0..snap.len()).collect()
                };
                for &i in &order {
                    let e = snap[i];
                    let r = self.call_value(a0, a1, &[e, Value::num(i as f64), recv])?;
                    match name {
                        "forEach" => {}
                        "map" => mapped.push(r),
                        "filter" => {
                            if self.truthy(r) {
                                mapped.push(e);
                            }
                        }
                        "find" => {
                            if self.truthy(r) {
                                return Ok(Some(e));
                            }
                        }
                        "findLast" => {
                            if self.truthy(r) {
                                return Ok(Some(e));
                            }
                        }
                        "findIndex" | "findLastIndex" => {
                            if self.truthy(r) {
                                return Ok(Some(Value::num(i as f64)));
                            }
                        }
                        "every" => {
                            if !self.truthy(r) {
                                return Ok(Some(Value::bool(false)));
                            }
                        }
                        "some" => {
                            if self.truthy(r) {
                                return Ok(Some(Value::bool(true)));
                            }
                        }
                        _ => {}
                    }
                }
                Ok(Some(match name {
                    "map" => self.ta_build_from(kind, &mapped)?,
                    "filter" => self.ta_build_from(kind, &mapped)?,
                    "find" | "findLast" => Value::UNDEFINED,
                    "findIndex" | "findLastIndex" => Value::num(-1.0),
                    "every" => Value::bool(true),
                    "some" => Value::bool(false),
                    _ => Value::UNDEFINED, // forEach
                }))
            }
            "reduce" | "reduceRight" => {
                if !self.is_callable(a0) {
                    return Err(Thrown(format!("TypeError: {name} callback is not a function")));
                }
                let snap = self.ta_snapshot(idx);
                let order: Vec<usize> = if name == "reduceRight" {
                    (0..snap.len()).rev().collect()
                } else {
                    (0..snap.len()).collect()
                };
                let mut acc;
                let mut start = 0;
                if args.len() >= 2 {
                    acc = a1;
                } else {
                    if order.is_empty() {
                        return Err(Thrown("TypeError: Reduce of empty array with no initial value".into()));
                    }
                    acc = snap[order[0]];
                    start = 1;
                }
                for &i in &order[start..] {
                    acc = self.call_value(a0, Value::UNDEFINED, &[acc, snap[i], Value::num(i as f64), recv])?;
                }
                Ok(Some(acc))
            }
            "fill" => {
                let start = rel(a1, 0, self);
                let end = rel(args.get(2).copied().unwrap_or(Value::UNDEFINED), len, self);
                for i in start..end {
                    self.ta_element_set(idx, i, a0)?;
                }
                Ok(Some(recv))
            }
            "reverse" => {
                let mut snap = self.ta_snapshot(idx);
                snap.reverse();
                for (i, v) in snap.into_iter().enumerate() {
                    self.ta_element_set(idx, i, v)?;
                }
                Ok(Some(recv))
            }
            "slice" => {
                let start = rel(a0, 0, self);
                let end = rel(a1, len, self);
                let vals: Vec<Value> = (start..end.max(start)).map(|i| self.ta_element_get(idx, i)).collect();
                Ok(Some(self.ta_build_from(kind, &vals)?))
            }
            "subarray" => {
                let start = rel(a0, 0, self);
                let end = rel(a1, len, self);
                let (buffer, byte_offset) = match self.heap.get(idx) {
                    HeapObj::TypedArray { buffer, byte_offset, .. } => (*buffer, *byte_offset),
                    _ => return Ok(None),
                };
                let size = native::TA_KINDS[kind as usize].1;
                let new_len = end.saturating_sub(start);
                Ok(Some(self.alloc_typed_array(buffer, kind, byte_offset + start * size, new_len)))
            }
            "sort" => {
                let cmp = a0;
                let mut snap = self.ta_snapshot(idx);
                if self.is_callable(cmp) {
                    // Comparator sort (stable insertion to allow VM re-entry).
                    let n = snap.len();
                    for i in 1..n {
                        let mut j = i;
                        while j > 0 {
                            let r = self.call_value(cmp, Value::UNDEFINED, &[snap[j - 1], snap[j]])?;
                            if self.value_num(r) > 0.0 {
                                snap.swap(j - 1, j);
                                j -= 1;
                            } else {
                                break;
                            }
                        }
                    }
                } else {
                    snap.sort_by(|a, b| {
                        let (x, y) = (self.value_num(*a), self.value_num(*b));
                        x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal)
                    });
                }
                for (i, v) in snap.into_iter().enumerate() {
                    self.ta_element_set(idx, i, v)?;
                }
                Ok(Some(recv))
            }
            "copyWithin" => {
                let target = rel(a0, 0, self);
                let start = rel(a1, 0, self);
                let end = rel(args.get(2).copied().unwrap_or(Value::UNDEFINED), len, self);
                let src: Vec<Value> = (start..end.max(start)).map(|i| self.ta_element_get(idx, i)).collect();
                for (k, v) in src.into_iter().enumerate() {
                    if target + k < len {
                        self.ta_element_set(idx, target + k, v)?;
                    }
                }
                Ok(Some(recv))
            }
            "set" => {
                let offset = if a1 == Value::UNDEFINED { 0 } else { self.value_num(a1) as usize };
                let src = self.iterate_or_arraylike(a0)?;
                for (k, v) in src.into_iter().enumerate() {
                    self.ta_element_set(idx, offset + k, v)?;
                }
                Ok(Some(Value::UNDEFINED))
            }
            "keys" => {
                let items: Vec<Value> = (0..len).map(|i| Value::num(i as f64)).collect();
                Ok(Some(self.make_iterator(items, self.array_iter_proto)))
            }
            "values" | "@@iterator" => {
                let items = self.ta_snapshot(idx);
                Ok(Some(self.make_iterator(items, self.array_iter_proto)))
            }
            "entries" => {
                let mut items = Vec::with_capacity(len);
                for i in 0..len {
                    let e = self.ta_element_get(idx, i);
                    items.push(Value::heap(self.heap.alloc(HeapObj::Array(vec![Value::num(i as f64), e]))));
                }
                Ok(Some(self.make_iterator(items, self.array_iter_proto)))
            }
            _ => Ok(None),
        }
    }

    /// Array-like or iterable → Vec of element Values (for `TypedArray.prototype.set`
    /// and TypedArray construction).
    fn iterate_or_arraylike(&mut self, v: Value) -> Result<Vec<Value>, Thrown> {
        if let Some(ta) = self.as_typed_array(v) {
            return Ok(self.ta_snapshot(ta));
        }
        if v.is_heap() {
            match self.heap.get(v.heap_index()) {
                HeapObj::Array(_)
                | HeapObj::Set(_)
                | HeapObj::Map { .. }
                | HeapObj::Str(_)
                | HeapObj::Cons { .. }
                | HeapObj::Generator { .. }
                | HeapObj::Iterator { .. } => return self.iterate_to_vec(v),
                _ => {}
            }
        }
        // Array-like object: read length + indices 0..length.
        let lv = self.get_prop(v, "length")?;
        let n = self.value_num(lv);
        let n = if n.is_finite() && n > 0.0 { n as usize } else { 0 };
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            out.push(self.get_index(v, Value::num(i as f64))?);
        }
        Ok(out)
    }

    /// `DataView.prototype.get/setInt8 … getFloat64` (+ `byteLength`/`byteOffset`/
    /// `buffer` are getters in get_prop). `name` is e.g. "getInt32".
    fn dataview_method(&mut self, idx: u32, name: &str, args: &[Value]) -> Result<Option<Value>, Thrown> {
        let (buffer, byte_offset, byte_length) = match self.heap.get(idx) {
            HeapObj::DataView { buffer, byte_offset, byte_length } => (*buffer, *byte_offset, *byte_length),
            _ => return Ok(None),
        };
        let (op, ty) = if let Some(t) = name.strip_prefix("get") {
            (0u8, t)
        } else if let Some(t) = name.strip_prefix("set") {
            (1u8, t)
        } else {
            return Ok(None);
        };
        // Element kind index for the suffix (Int8..Float64 / BigInt64 / BigUint64).
        let kind = match ty {
            "Int8" => 0,
            "Uint8" => 1,
            "Int16" => 3,
            "Uint16" => 4,
            "Int32" => 5,
            "Uint32" => 6,
            "Float32" => 7,
            "Float64" => 8,
            "BigInt64" => 9,
            "BigUint64" => 10,
            _ => return Ok(None),
        };
        let size = native::TA_KINDS[kind as usize].1;
        let pos = self.value_num(args.first().copied().unwrap_or(Value::UNDEFINED)) as usize;
        // get(pos, littleEndian?) / set(pos, value, littleEndian?)
        let little_endian = if op == 0 {
            self.truthy(args.get(1).copied().unwrap_or(Value::UNDEFINED))
        } else {
            self.truthy(args.get(2).copied().unwrap_or(Value::UNDEFINED))
        };
        if pos + size > byte_length {
            return Err(Thrown("RangeError: Offset is outside the bounds of the DataView".into()));
        }
        let abs = byte_offset + pos;
        if op == 0 {
            // read
            let mut b = [0u8; 8];
            {
                let data = match self.heap.get(buffer) {
                    HeapObj::ArrayBuffer { data, .. } => data,
                    _ => return Ok(Some(Value::UNDEFINED)),
                };
                if abs + size > data.len() {
                    return Err(Thrown("RangeError: DataView out of bounds".into()));
                }
                b[..size].copy_from_slice(&data[abs..abs + size]);
            }
            if !little_endian {
                b[..size].reverse();
            }
            Ok(Some(match kind {
                0 => Value::num(b[0] as i8 as f64),
                1 => Value::num(b[0] as f64),
                3 => Value::num(i16::from_le_bytes([b[0], b[1]]) as f64),
                4 => Value::num(u16::from_le_bytes([b[0], b[1]]) as f64),
                5 => Value::num(i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64),
                6 => Value::num(u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64),
                7 => Value::num(f32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64),
                8 => Value::num(f64::from_le_bytes(b)),
                9 => self.make_bigint(i64::from_le_bytes(b) as i128),
                _ => self.make_bigint(u64::from_le_bytes(b) as i128),
            }))
        } else {
            // write
            let v = args.get(1).copied().unwrap_or(Value::UNDEFINED);
            let mut bytes = if kind >= 9 {
                let n = self.to_bigint(v)?;
                if kind == 9 {
                    (n as i64).to_le_bytes()
                } else {
                    (n as u64).to_le_bytes()
                }
            } else {
                let f = self.to_number(v)?;
                ta_encode(kind, f)
            };
            if !little_endian {
                bytes[..size].reverse();
            }
            if let HeapObj::ArrayBuffer { data, .. } = self.heap.get_mut(buffer) {
                if abs + size <= data.len() {
                    data[abs..abs + size].copy_from_slice(&bytes[..size]);
                }
            }
            Ok(Some(Value::UNDEFINED))
        }
    }

    /// `ArrayBuffer.prototype.slice(begin?, end?)` → a new ArrayBuffer copy.
    fn arraybuffer_method(&mut self, idx: u32, name: &str, args: &[Value]) -> Result<Option<Value>, Thrown> {
        let len = self.array_buffer_len(idx);
        match name {
            "slice" => {
                let rel = |v: Value, def: usize, this: &Self| -> usize {
                    if v == Value::UNDEFINED {
                        return def;
                    }
                    let n = this.value_num(v);
                    if n < 0.0 { ((len as f64 + n).max(0.0)) as usize } else { (n as usize).min(len) }
                };
                let start = rel(args.first().copied().unwrap_or(Value::UNDEFINED), 0, self);
                let end = rel(args.get(1).copied().unwrap_or(Value::UNDEFINED), len, self);
                let slice: Vec<u8> = match self.heap.get(idx) {
                    HeapObj::ArrayBuffer { data, .. } => data[start..end.max(start)].to_vec(),
                    _ => Vec::new(),
                };
                let new_idx = self.alloc_array_buffer(slice.len());
                if let HeapObj::ArrayBuffer { data, .. } = self.heap.get_mut(new_idx) {
                    data.copy_from_slice(&slice);
                }
                Ok(Some(Value::heap(new_idx)))
            }
            _ => Ok(None),
        }
    }

    /// `Promise.prototype.then/catch/finally`. Returns a NEW dependent promise.
    /// All handlers run as microtasks (never synchronously). `idx` is the
    /// receiver promise's heap index.
    fn promise_method(&mut self, idx: u32, name: &str, args: &[Value]) -> Result<Option<Value>, Thrown> {
        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        match name {
            "then" => {
                let on_r = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let dep = self.then_internal(idx, a0, on_r, None);
                Ok(Some(Value::heap(dep)))
            }
            "catch" => {
                let dep = self.then_internal(idx, Value::UNDEFINED, a0, None);
                Ok(Some(Value::heap(dep)))
            }
            "finally" => {
                // `cb` runs (no args) on both settle paths; the original value /
                // reason forwards (FinallyReaction handles the value pass-through).
                let dep = self.finally_internal(idx, a0);
                Ok(Some(Value::heap(dep)))
            }
            _ => Ok(None),
        }
    }

    /// `Map.prototype.*`. `idx` is the Map's heap index. Returns `Ok(None)` for an
    /// unknown method (→ TypeError at the call site). `forEach` snapshots the
    /// entries before invoking the callback (which may mutate the map).
    fn map_method(&mut self, idx: u32, name: &str, args: &[Value]) -> Result<Option<Value>, Thrown> {
        // Brand check: `Map.prototype.<m>.call(x)` requires x to have [[MapData]].
        if !matches!(self.heap.get(idx), HeapObj::Map { .. }) {
            return Err(Thrown(format!("TypeError: Map.prototype.{name} called on incompatible receiver")));
        }
        let recv = Value::heap(idx);
        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        match name {
            "get" => {
                let v = match self.heap.get(idx) {
                    HeapObj::Map { keys, vals } => keys
                        .iter()
                        .position(|k| self.same_value_zero(*k, a0))
                        .map(|i| vals[i]),
                    _ => None,
                };
                Ok(Some(v.unwrap_or(Value::UNDEFINED)))
            }
            "has" => {
                let found = match self.heap.get(idx) {
                    HeapObj::Map { keys, .. } => keys.iter().any(|k| self.same_value_zero(*k, a0)),
                    _ => false,
                };
                Ok(Some(Value::bool(found)))
            }
            "set" => {
                let key = normalize_zero(a0);
                let val = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let pos = match self.heap.get(idx) {
                    HeapObj::Map { keys, .. } => keys.iter().position(|k| self.same_value_zero(*k, key)),
                    _ => None,
                };
                if let HeapObj::Map { keys, vals } = self.heap.get_mut(idx) {
                    match pos {
                        Some(i) => vals[i] = val, // update in place, keep position
                        None => {
                            keys.push(key);
                            vals.push(val);
                        }
                    }
                }
                Ok(Some(recv)) // chainable
            }
            "delete" => {
                let pos = match self.heap.get(idx) {
                    HeapObj::Map { keys, .. } => keys.iter().position(|k| self.same_value_zero(*k, a0)),
                    _ => None,
                };
                if let (Some(i), HeapObj::Map { keys, vals }) = (pos, self.heap.get_mut(idx)) {
                    keys.remove(i);
                    vals.remove(i);
                    return Ok(Some(Value::bool(true)));
                }
                Ok(Some(Value::bool(false)))
            }
            "clear" => {
                if let HeapObj::Map { keys, vals } = self.heap.get_mut(idx) {
                    keys.clear();
                    vals.clear();
                }
                Ok(Some(Value::UNDEFINED))
            }
            "forEach" => {
                let cb = a0;
                let this_arg = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let (ks, vs) = match self.heap.get(idx) {
                    HeapObj::Map { keys, vals } => (keys.clone(), vals.clone()),
                    _ => (Vec::new(), Vec::new()),
                };
                for (k, v) in ks.into_iter().zip(vs) {
                    // callback(value, key, map)
                    self.call_value(cb, this_arg, &[v, k, recv])?;
                }
                Ok(Some(Value::UNDEFINED))
            }
            // Real iterators over %MapIteratorPrototype% (snapshot semantics).
            "keys" => {
                let v = match self.heap.get(idx) {
                    HeapObj::Map { keys, .. } => keys.clone(),
                    _ => Vec::new(),
                };
                Ok(Some(self.make_iterator(v, self.map_iter_proto)))
            }
            "values" => {
                let v = match self.heap.get(idx) {
                    HeapObj::Map { vals, .. } => vals.clone(),
                    _ => Vec::new(),
                };
                Ok(Some(self.make_iterator(v, self.map_iter_proto)))
            }
            "entries" => {
                let pairs: Vec<(Value, Value)> = match self.heap.get(idx) {
                    HeapObj::Map { keys, vals } => {
                        keys.iter().copied().zip(vals.iter().copied()).collect()
                    }
                    _ => Vec::new(),
                };
                let entries: Vec<Value> = pairs
                    .into_iter()
                    .map(|(k, v)| Value::heap(self.heap.alloc(HeapObj::Array(vec![k, v]))))
                    .collect();
                Ok(Some(self.make_iterator(entries, self.map_iter_proto)))
            }
            _ => Ok(None),
        }
    }

    /// `WeakMap.prototype.{get,set,has,delete}`. Brand-checked (the receiver must be
    /// a WeakMap, so `WeakMap.prototype.set.call(aMap)` throws) and keys must be
    /// objects. No GC, so entries are held strongly (unobservable without GC).
    fn weakmap_method(&mut self, this: Value, name: &str, args: &[Value]) -> Result<Value, Thrown> {
        if !this.is_heap() || !matches!(self.heap.get(this.heap_index()), HeapObj::WeakMap { .. }) {
            return Err(Thrown(format!(
                "TypeError: WeakMap.prototype.{name} called on incompatible receiver"
            )));
        }
        let idx = this.heap_index();
        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        match name {
            "get" => {
                let v = match self.heap.get(idx) {
                    HeapObj::WeakMap { keys, vals } => {
                        keys.iter().position(|k| self.same_value_zero(*k, a0)).map(|i| vals[i])
                    }
                    _ => None,
                };
                Ok(v.unwrap_or(Value::UNDEFINED))
            }
            "has" => {
                let found = match self.heap.get(idx) {
                    HeapObj::WeakMap { keys, .. } => keys.iter().any(|k| self.same_value_zero(*k, a0)),
                    _ => false,
                };
                Ok(Value::bool(found))
            }
            "set" => {
                if !self.is_object_value(a0) {
                    return Err(Thrown("TypeError: Invalid value used as weak map key".into()));
                }
                let val = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let pos = match self.heap.get(idx) {
                    HeapObj::WeakMap { keys, .. } => keys.iter().position(|k| self.same_value_zero(*k, a0)),
                    _ => None,
                };
                if let HeapObj::WeakMap { keys, vals } = self.heap.get_mut(idx) {
                    match pos {
                        Some(i) => vals[i] = val,
                        None => {
                            keys.push(a0);
                            vals.push(val);
                        }
                    }
                }
                Ok(this) // chainable
            }
            "delete" => {
                let pos = match self.heap.get(idx) {
                    HeapObj::WeakMap { keys, .. } => keys.iter().position(|k| self.same_value_zero(*k, a0)),
                    _ => None,
                };
                if let (Some(i), HeapObj::WeakMap { keys, vals }) = (pos, self.heap.get_mut(idx)) {
                    keys.remove(i);
                    vals.remove(i);
                    return Ok(Value::bool(true));
                }
                Ok(Value::bool(false))
            }
            _ => Ok(Value::UNDEFINED),
        }
    }

    /// `WeakSet.prototype.{add,has,delete}`. Brand-checked; values must be objects.
    fn weakset_method(&mut self, this: Value, name: &str, args: &[Value]) -> Result<Value, Thrown> {
        if !this.is_heap() || !matches!(self.heap.get(this.heap_index()), HeapObj::WeakSet(_)) {
            return Err(Thrown(format!(
                "TypeError: WeakSet.prototype.{name} called on incompatible receiver"
            )));
        }
        let idx = this.heap_index();
        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        match name {
            "has" => {
                let found = match self.heap.get(idx) {
                    HeapObj::WeakSet(items) => items.iter().any(|v| self.same_value_zero(*v, a0)),
                    _ => false,
                };
                Ok(Value::bool(found))
            }
            "add" => {
                if !self.is_object_value(a0) {
                    return Err(Thrown("TypeError: Invalid value used in weak set".into()));
                }
                let present = match self.heap.get(idx) {
                    HeapObj::WeakSet(items) => items.iter().any(|v| self.same_value_zero(*v, a0)),
                    _ => true,
                };
                if !present {
                    if let HeapObj::WeakSet(items) = self.heap.get_mut(idx) {
                        items.push(a0);
                    }
                }
                Ok(this) // chainable
            }
            "delete" => {
                let pos = match self.heap.get(idx) {
                    HeapObj::WeakSet(items) => items.iter().position(|v| self.same_value_zero(*v, a0)),
                    _ => None,
                };
                if let (Some(i), HeapObj::WeakSet(items)) = (pos, self.heap.get_mut(idx)) {
                    items.remove(i);
                    return Ok(Value::bool(true));
                }
                Ok(Value::bool(false))
            }
            _ => Ok(Value::UNDEFINED),
        }
    }

    /// `FinalizationRegistry.prototype.{register,unregister}`. No GC, so cleanup
    /// never fires; only the register/unregister bookkeeping (+ arg validation) is
    /// observable. `tokens` tracks live unregister tokens for `unregister`.
    fn finreg_method(&mut self, this: Value, name: &str, args: &[Value]) -> Result<Value, Thrown> {
        if !this.is_heap() || !matches!(self.heap.get(this.heap_index()), HeapObj::FinalizationRegistry { .. }) {
            return Err(Thrown(format!(
                "TypeError: FinalizationRegistry.prototype.{name} called on incompatible receiver"
            )));
        }
        let idx = this.heap_index();
        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        match name {
            "register" => {
                let held = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let token = args.get(2).copied().unwrap_or(Value::UNDEFINED);
                if !self.is_object_value(a0) {
                    return Err(Thrown("TypeError: FinalizationRegistry.register: target must be an object".into()));
                }
                if self.same_value(a0, held) {
                    return Err(Thrown(
                        "TypeError: FinalizationRegistry.register: target and held value must not be the same".into(),
                    ));
                }
                if token != Value::UNDEFINED && !self.is_object_value(token) {
                    return Err(Thrown(
                        "TypeError: FinalizationRegistry.register: unregister token must be an object".into(),
                    ));
                }
                if self.is_object_value(token) {
                    if let HeapObj::FinalizationRegistry { tokens, .. } = self.heap.get_mut(idx) {
                        tokens.push(token);
                    }
                }
                Ok(Value::UNDEFINED)
            }
            "unregister" => {
                if !self.is_object_value(a0) {
                    return Err(Thrown(
                        "TypeError: FinalizationRegistry.unregister: token must be an object".into(),
                    ));
                }
                let mut removed = false;
                if let HeapObj::FinalizationRegistry { tokens, .. } = self.heap.get_mut(idx) {
                    let before = tokens.len();
                    tokens.retain(|t| *t != a0); // object identity = Value bit-equality
                    removed = tokens.len() != before;
                }
                Ok(Value::bool(removed))
            }
            _ => Ok(Value::UNDEFINED),
        }
    }

    /// `Set.prototype.*`. `idx` is the Set's heap index. `keys`/`values`/`entries`
    /// return arrays (the iterator approximation).
    fn set_method(&mut self, idx: u32, name: &str, args: &[Value]) -> Result<Option<Value>, Thrown> {
        // Brand check: `Set.prototype.<m>.call(x)` requires x to have [[SetData]].
        if !matches!(self.heap.get(idx), HeapObj::Set(_)) {
            return Err(Thrown(format!("TypeError: Set.prototype.{name} called on incompatible receiver")));
        }
        let recv = Value::heap(idx);
        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        match name {
            "has" => {
                let found = match self.heap.get(idx) {
                    HeapObj::Set(items) => items.iter().any(|v| self.same_value_zero(*v, a0)),
                    _ => false,
                };
                Ok(Some(Value::bool(found)))
            }
            "add" => {
                let val = normalize_zero(a0);
                let present = match self.heap.get(idx) {
                    HeapObj::Set(items) => items.iter().any(|v| self.same_value_zero(*v, val)),
                    _ => true,
                };
                if !present {
                    if let HeapObj::Set(items) = self.heap.get_mut(idx) {
                        items.push(val);
                    }
                }
                Ok(Some(recv)) // chainable
            }
            "delete" => {
                let pos = match self.heap.get(idx) {
                    HeapObj::Set(items) => items.iter().position(|v| self.same_value_zero(*v, a0)),
                    _ => None,
                };
                if let (Some(i), HeapObj::Set(items)) = (pos, self.heap.get_mut(idx)) {
                    items.remove(i);
                    return Ok(Some(Value::bool(true)));
                }
                Ok(Some(Value::bool(false)))
            }
            "clear" => {
                if let HeapObj::Set(items) = self.heap.get_mut(idx) {
                    items.clear();
                }
                Ok(Some(Value::UNDEFINED))
            }
            "forEach" => {
                let cb = a0;
                let this_arg = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let items = match self.heap.get(idx) {
                    HeapObj::Set(items) => items.clone(),
                    _ => Vec::new(),
                };
                for v in items {
                    // callback(value, value, set) — value passed twice, mirroring Map.
                    self.call_value(cb, this_arg, &[v, v, recv])?;
                }
                Ok(Some(Value::UNDEFINED))
            }
            // keys() === values() for a Set; both yield the values (real iterator).
            "keys" | "values" => {
                let v = match self.heap.get(idx) {
                    HeapObj::Set(items) => items.clone(),
                    _ => Vec::new(),
                };
                Ok(Some(self.make_iterator(v, self.set_iter_proto)))
            }
            "entries" => {
                let items = match self.heap.get(idx) {
                    HeapObj::Set(items) => items.clone(),
                    _ => Vec::new(),
                };
                let entries: Vec<Value> = items
                    .into_iter()
                    .map(|v| Value::heap(self.heap.alloc(HeapObj::Array(vec![v, v]))))
                    .collect();
                Ok(Some(self.make_iterator(entries, self.set_iter_proto)))
            }
            // ES2025 set methods. `other` must be set-like; the common (and tested)
            // case is a real Set, whose elements we read directly.
            "union" | "intersection" | "difference" | "symmetricDifference"
            | "isSubsetOf" | "isSupersetOf" | "isDisjointFrom" => {
                let this_items = match self.heap.get(idx) {
                    HeapObj::Set(items) => items.clone(),
                    _ => Vec::new(),
                };
                let other_items = match a0.is_heap().then(|| self.heap.get(a0.heap_index())) {
                    Some(HeapObj::Set(items)) => items.clone(),
                    _ => return Err(Thrown("TypeError: Set method argument is not a Set".into())),
                };
                let has = |hay: &[Value], v: Value, vm: &Self| hay.iter().any(|x| vm.same_value_zero(*x, v));
                let result = match name {
                    "union" => {
                        let mut r = this_items.clone();
                        for &v in &other_items {
                            if !has(&r, v, self) {
                                r.push(v);
                            }
                        }
                        Value::heap(self.heap.alloc(HeapObj::Set(r)))
                    }
                    "intersection" => {
                        let r: Vec<Value> =
                            this_items.iter().copied().filter(|&v| has(&other_items, v, self)).collect();
                        Value::heap(self.heap.alloc(HeapObj::Set(r)))
                    }
                    "difference" => {
                        let r: Vec<Value> =
                            this_items.iter().copied().filter(|&v| !has(&other_items, v, self)).collect();
                        Value::heap(self.heap.alloc(HeapObj::Set(r)))
                    }
                    "symmetricDifference" => {
                        let mut r: Vec<Value> =
                            this_items.iter().copied().filter(|&v| !has(&other_items, v, self)).collect();
                        for &v in &other_items {
                            if !has(&this_items, v, self) && !has(&r, v, self) {
                                r.push(v);
                            }
                        }
                        Value::heap(self.heap.alloc(HeapObj::Set(r)))
                    }
                    "isSubsetOf" => Value::bool(this_items.iter().all(|&v| has(&other_items, v, self))),
                    "isSupersetOf" => Value::bool(other_items.iter().all(|&v| has(&this_items, v, self))),
                    _ => Value::bool(!this_items.iter().any(|&v| has(&other_items, v, self))), // isDisjointFrom
                };
                Ok(Some(result))
            }
            _ => Ok(None),
        }
    }

    /// `key in obj` — does `obj` have the property `key`? Own object keys, a
    /// class instance's inherited methods/getters, array indices / `length`,
    /// Map/Set `size`, and class static members. `in` on a primitive throws
    /// in JS; here it's `false` (rare).
    fn has_property(&self, obj: Value, key: Value) -> bool {
        if !obj.is_heap() {
            return false;
        }
        let idx = obj.heap_index();
        match self.heap.get(idx) {
            HeapObj::Object(map) => {
                let k = self.key_of(key);
                if map.get(&k).is_some() {
                    return true;
                }
                // Inherited method/getter through the class chain.
                let mut cur = map.class;
                while let Some(cidx) = cur {
                    match self.heap.get(cidx) {
                        HeapObj::Class(c) => {
                            if c.methods.iter().any(|(n, _)| *n == k)
                                || c.getters.iter().any(|(n, _)| *n == k)
                            {
                                return true;
                            }
                            cur = c.parent;
                        }
                        _ => break,
                    }
                }
                false
            }
            HeapObj::Array(items) => match array_index(key) {
                Some(i) => i < items.len(),
                None => self.display(key) == "length",
            },
            HeapObj::Str(s) => match array_index(key) {
                Some(i) => i < s.char_len,
                None => self.display(key) == "length",
            },
            HeapObj::Cons { len, .. } => match array_index(key) {
                Some(i) => i < *len,
                None => self.display(key) == "length",
            },
            HeapObj::Map { .. } | HeapObj::Set(_) => self.display(key) == "size",
            // Static members (data + `static get`/`set` accessors) are own
            // properties of the class value and are inherited up the chain.
            HeapObj::Class(_) => {
                let k = self.key_of(key);
                let mut cur = Some(idx);
                while let Some(cidx) = cur {
                    match self.heap.get(cidx) {
                        HeapObj::Class(c) => {
                            if c.statics.get(&k).is_some()
                                || c.static_getters.iter().any(|(n, _)| *n == k)
                                || c.static_setters.iter().any(|(n, _)| *n == k)
                            {
                                return true;
                            }
                            cur = c.parent;
                        }
                        _ => break,
                    }
                }
                false
            }
            _ => false,
        }
    }

    /// `val instanceof <built-in ctor>`. With no user prototype chain the result
    /// is structural: by heap kind for Array/Object/Function, and by the `name`
    /// field for the Error family (any error subtype satisfies `instanceof
    /// Error`). Primitives are never an instance of anything.
    fn eval_instanceof(&self, val: Value, ctor: InstanceCtor) -> bool {
        use InstanceCtor as C;
        if !val.is_heap() {
            return false;
        }
        let idx = val.heap_index();
        match ctor {
            C::Array => matches!(self.heap.get(idx), HeapObj::Array(_)),
            C::Function => {
                matches!(self.heap.get(idx), HeapObj::Func(_) | HeapObj::Closure { .. })
            }
            // Every non-primitive (array, object, function, error) is an Object.
            C::Object => matches!(
                self.heap.get(idx),
                HeapObj::Array(_) | HeapObj::Object(_) | HeapObj::Func(_) | HeapObj::Closure { .. }
            ),
            C::Error => self.error_name(idx).is_some(),
            C::TypeError => self.error_name(idx).as_deref() == Some("TypeError"),
            C::RangeError => self.error_name(idx).as_deref() == Some("RangeError"),
            C::SyntaxError => self.error_name(idx).as_deref() == Some("SyntaxError"),
            C::ReferenceError => self.error_name(idx).as_deref() == Some("ReferenceError"),
            C::EvalError => self.error_name(idx).as_deref() == Some("EvalError"),
            C::UriError => self.error_name(idx).as_deref() == Some("URIError"),
            C::AggregateError => self.error_name(idx).as_deref() == Some("AggregateError"),
        }
    }

    /// Build an Error object from an internal throw message. A message like
    /// `"TypeError: cannot read …"` splits into `name="TypeError"` and
    /// `message="cannot read …"`; anything else becomes a generic `Error` whose
    /// message is the whole text. Mirrors the `{name, message}` shape the
    /// compiler emits for `new TypeError(…)`, so both catch paths are uniform.
    fn alloc_error_from_message(&mut self, raw: &str) -> Value {
        // Internal errors are formatted "Name: message"; recover the kind so the
        // synthesised object links to the right prototype (and `e instanceof X`,
        // `e.constructor` work). Anything unrecognised is a base `Error`.
        let (kind, message) = match raw.split_once(": ") {
            Some((pre, rest)) => match native::ERROR_NAMES.iter().position(|&n| n == pre) {
                Some(i) => (i as u8, rest.to_string()),
                None => (0, raw.to_string()),
            },
            None => (0, raw.to_string()),
        };
        let msg_v = self.alloc_str(message);
        self.make_error(kind, Some(msg_v))
    }

    /// Allocate a proto-linked error instance of the given kind (0=Error … 7=
    /// AggregateError). `name` is set own (so the structural `instanceof`/`error_name`
    /// path keeps working); `message` is set own only when supplied and not
    /// `undefined` (else inherited as "" from the prototype). The prototype link
    /// gives `.constructor`, `.toString`, and value-`instanceof` resolution.
    fn make_error(&mut self, kind: u8, msg: Option<Value>) -> Value {
        let k = (kind as usize).min(7);
        let name_v = self.alloc_str(native::ERROR_NAMES[k].to_string());
        let msg_idx = match msg {
            Some(m) if m != Value::UNDEFINED => Some(self.to_str_idx(m)),
            _ => None,
        };
        let mut map = ObjMap::new();
        map.set("name", name_v);
        if let Some(mi) = msg_idx {
            map.set("message", Value::heap(mi));
        }
        let obj = self.heap.alloc(HeapObj::Object(map));
        let p = self.error_protos[k];
        if p != 0 {
            self.proto_of.insert(obj, Value::heap(p));
        }
        Value::heap(obj)
    }

    /// Allocate a fresh unique `Symbol` with description `desc` (a string Value or
    /// UNDEFINED) and a unique internal prop_key (`@@sym:N`). Recorded in
    /// `symbol_keys` so the symbol can be reflected from an own property key.
    fn make_symbol(&mut self, desc: Value) -> Value {
        self.symbol_counter += 1;
        let prop_key = format!("@@sym:{}", self.symbol_counter);
        let v = Value::heap(self.heap.alloc(HeapObj::Symbol { desc, prop_key: prop_key.clone() }));
        self.symbol_keys.insert(prop_key, v);
        v
    }

    /// Allocate a symbol with a FIXED prop_key (well-known `@@iterator` etc., or a
    /// `Symbol.for` registry key) — so distinct call sites share the same key.
    fn make_named_symbol(&mut self, desc: Value, prop_key: &str) -> Value {
        let v = Value::heap(
            self.heap.alloc(HeapObj::Symbol { desc, prop_key: prop_key.to_string() }),
        );
        self.symbol_keys.insert(prop_key.to_string(), v);
        v
    }

    /// Coerce a Value used as a PROPERTY KEY to its string form: a Symbol → its
    /// internal `prop_key` (`@@iterator` / `@@sym:N`), anything else → `display`.
    fn key_of(&self, key: Value) -> String {
        if key.is_heap() {
            if let HeapObj::Symbol { prop_key, .. } = self.heap.get(key.heap_index()) {
                return prop_key.clone();
            }
        }
        self.display(key)
    }

    /// Allocate a BigInt value.
    fn make_bigint(&mut self, v: i128) -> Value {
        Value::heap(self.heap.alloc(HeapObj::BigInt(v)))
    }

    /// The i128 of a BigInt value, else None.
    fn bigint_value(&self, v: Value) -> Option<i128> {
        if v.is_heap() {
            if let HeapObj::BigInt(n) = self.heap.get(v.heap_index()) {
                return Some(*n);
            }
        }
        None
    }

    /// `ToBigInt(v)` (used by `BigInt(x)`, asIntN/asUintN, and `==`). A non-integer
    /// number → RangeError; symbol/null/undefined/object → TypeError; a bad numeric
    /// string → SyntaxError.
    fn to_bigint(&mut self, v: Value) -> Result<i128, Thrown> {
        if let Some(n) = self.bigint_value(v) {
            return Ok(n);
        }
        if v.is_bool() {
            return Ok(if v.as_bool() { 1 } else { 0 });
        }
        if v.is_int() {
            return Ok(v.as_int() as i128);
        }
        if v.is_double() {
            let d = v.as_f64();
            if !d.is_finite() || d.fract() != 0.0 {
                return Err(Thrown(
                    "RangeError: The number is not a safe integer and cannot be converted to a BigInt"
                        .into(),
                ));
            }
            return Ok(d as i128);
        }
        if v.is_heap() && self.heap.is_str_like(v.heap_index()) {
            let s = self.heap.str_cow(v.heap_index()).unwrap().into_owned();
            let t = s.trim();
            if t.is_empty() {
                return Ok(0);
            }
            return parse_bigint_str(t)
                .ok_or_else(|| Thrown(format!("SyntaxError: Cannot convert {t} to a BigInt")));
        }
        Err(Thrown("TypeError: Cannot convert this value to a BigInt".into()))
    }

    /// Build a RegExp from a pattern value + flags value (`/x/g`, `new RegExp(p,f)`).
    /// A RegExp pattern contributes its source (+ its flags when none are given);
    /// else ToString. Validates flags + compiles via `regress` (bad → SyntaxError).
    fn build_regexp(&mut self, p: Value, f: Value) -> Result<Value, Thrown> {
        let (source, inherited) = if p.is_heap() {
            if let HeapObj::RegExp { source, flags, .. } = self.heap.get(p.heap_index()) {
                (source.clone(), Some(flags.clone()))
            } else {
                (self.to_js_string(p)?, None)
            }
        } else if p.is_undefined() {
            (String::new(), None)
        } else {
            (self.to_js_string(p)?, None)
        };
        let flags = if f.is_undefined() {
            inherited.unwrap_or_default()
        } else {
            self.to_js_string(f)?
        };
        // Validate: only g/i/m/s/u/y/d/v, each at most once.
        let mut seen = std::collections::HashSet::new();
        for c in flags.chars() {
            if !"gimsuyvd".contains(c) || !seen.insert(c) {
                return Err(Thrown(format!(
                    "SyntaxError: Invalid flags supplied to RegExp constructor '{flags}'"
                )));
            }
        }
        // The matching flags `regress` understands (g/y/d are JS-level state).
        let mut rflags = String::new();
        for c in flags.chars() {
            match c {
                'i' | 'm' | 's' => rflags.push(c),
                'u' | 'v' if !rflags.contains('u') => rflags.push('u'),
                _ => {}
            }
        }
        let regex = regress::Regex::with_flags(&source, rflags.as_str())
            .map_err(|e| Thrown(format!("SyntaxError: Invalid regular expression: /{source}/: {e}")))?;
        let idx = self
            .heap
            .alloc(HeapObj::RegExp { regex: Box::new(regex), source, flags, last_index: 0 });
        if self.regexp_proto != 0 {
            self.proto_of.insert(idx, Value::heap(self.regexp_proto));
        }
        Ok(Value::heap(idx))
    }

    // ── Temporal.Duration ──

    fn make_duration(&mut self, f: [i64; 10]) -> Value {
        let idx = self.heap.alloc(HeapObj::Temporal { kind: 0, fields: f.to_vec() });
        if self.duration_proto != 0 {
            self.proto_of.insert(idx, Value::heap(self.duration_proto));
        }
        Value::heap(idx)
    }

    fn duration_fields(&self, idx: u32) -> Option<[i64; 10]> {
        match self.heap.get(idx) {
            HeapObj::Temporal { kind: 0, fields } => {
                let mut f = [0i64; 10];
                for (i, s) in f.iter_mut().enumerate() {
                    *s = *fields.get(i).unwrap_or(&0);
                }
                Some(f)
            }
            _ => None,
        }
    }

    /// All non-zero fields must share a sign (else RangeError).
    fn validate_duration(&self, f: &[i64; 10]) -> Result<(), Thrown> {
        let mut sign = 0i64;
        for &x in f {
            let s = x.signum();
            if s != 0 {
                if sign == 0 {
                    sign = s;
                } else if s != sign {
                    return Err(Thrown(
                        "RangeError: mixed-sign values not allowed as duration fields".into(),
                    ));
                }
            }
        }
        Ok(())
    }

    /// `new Temporal.Duration(y, mo, w, d, h, mi, s, ms, us, ns)` — integer fields.
    fn build_duration(&mut self, args: &[Value]) -> Result<Value, Thrown> {
        let mut f = [0i64; 10];
        for (i, slot) in f.iter_mut().enumerate() {
            let v = args.get(i).copied().unwrap_or(Value::UNDEFINED);
            if v != Value::UNDEFINED {
                let n = self.to_number(v)?;
                if !n.is_finite() || n.fract() != 0.0 {
                    return Err(Thrown(
                        "RangeError: Temporal.Duration fields must be integers".into(),
                    ));
                }
                *slot = n as i64;
            }
        }
        self.validate_duration(&f)?;
        Ok(self.make_duration(f))
    }

    /// ToTemporalDuration: a Duration clones; an object reads its duration fields;
    /// a string parses an ISO-8601 duration.
    fn to_duration(&mut self, v: Value) -> Result<[i64; 10], Thrown> {
        if let Some(idx) = (v.is_heap()).then(|| v.heap_index()) {
            if let Some(f) = self.duration_fields(idx) {
                return Ok(f);
            }
            if self.heap.is_str_like(idx) {
                let s = self.heap.str_cow(idx).unwrap().into_owned();
                return parse_iso_duration(&s)
                    .ok_or_else(|| Thrown(format!("RangeError: invalid duration string '{s}'")));
            }
            if matches!(self.heap.get(idx), HeapObj::Object(_)) {
                let mut f = [0i64; 10];
                let mut any = false;
                for (i, name) in native::DURATION_FIELDS.iter().enumerate() {
                    let pv = self.get_prop(v, name)?;
                    if pv != Value::UNDEFINED {
                        any = true;
                        let n = self.to_number(pv)?;
                        if !n.is_finite() || n.fract() != 0.0 {
                            return Err(Thrown(
                                "RangeError: Temporal.Duration fields must be integers".into(),
                            ));
                        }
                        f[i] = n as i64;
                    }
                }
                if !any {
                    return Err(Thrown(
                        "TypeError: object is not a valid Temporal.Duration-like".into(),
                    ));
                }
                self.validate_duration(&f)?;
                return Ok(f);
            }
        }
        Err(Thrown("TypeError: cannot convert value to a Temporal.Duration".into()))
    }

    fn duration_sign(f: &[i64; 10]) -> i64 {
        f.iter().map(|x| x.signum()).find(|&s| s != 0).unwrap_or(0)
    }

    /// Dispatch a Temporal instance method to the per-kind handler.
    fn temporal_method(&mut self, idx: u32, name: &str, args: &[Value]) -> Result<Option<Value>, Thrown> {
        match self.heap.get(idx) {
            HeapObj::Temporal { kind: 0, .. } => self.duration_method(idx, name, args),
            HeapObj::Temporal { kind: 1, .. } => self.plain_date_method(idx, name, args),
            HeapObj::Temporal { kind: 2, .. } => self.plain_time_method(idx, name, args),
            HeapObj::Temporal { kind: 3, .. } => self.plain_date_time_method(idx, name, args),
            HeapObj::Temporal { kind: 4, .. } => self.instant_method(idx, name, args),
            HeapObj::Temporal { kind: 5, .. } => self.plain_year_month_method(idx, name, args),
            HeapObj::Temporal { kind: 6, .. } => self.plain_month_day_method(idx, name, args),
            _ => Ok(None),
        }
    }

    /// `Temporal.Duration.prototype` methods + getters not handled inline.
    fn duration_method(&mut self, idx: u32, name: &str, args: &[Value]) -> Result<Option<Value>, Thrown> {
        let f = match self.duration_fields(idx) {
            Some(f) => f,
            None => return Ok(None),
        };
        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        match name {
            "negated" => Ok(Some(self.make_duration(f.map(|x| -x)))),
            "abs" => Ok(Some(self.make_duration(f.map(|x| x.abs())))),
            "with" => {
                // Override the supplied fields (a plain partial-duration object).
                let mut nf = f;
                let mut any = false;
                for (i, name) in native::DURATION_FIELDS.iter().enumerate() {
                    let pv = self.get_prop(a0, name)?;
                    if pv != Value::UNDEFINED {
                        any = true;
                        let n = self.to_number(pv)?;
                        if !n.is_finite() || n.fract() != 0.0 {
                            return Err(Thrown("RangeError: Duration fields must be integers".into()));
                        }
                        nf[i] = n as i64;
                    }
                }
                if !any {
                    return Err(Thrown("TypeError: with() requires a partial Duration object".into()));
                }
                self.validate_duration(&nf)?;
                Ok(Some(self.make_duration(nf)))
            }
            "toString" | "toJSON" => Ok(Some(self.alloc_str(duration_to_string(&f)))),
            "valueOf" => {
                Err(Thrown("TypeError: Called Temporal.Duration.prototype.valueOf".into()))
            }
            _ => Ok(None),
        }
    }

    // ── Temporal.PlainDate ──

    fn make_plain_date(&mut self, y: i64, m: i64, d: i64) -> Result<Value, Thrown> {
        if !(1..=12).contains(&m) || d < 1 || d > days_in_month(y, m) || !(-271821..=275760).contains(&y) {
            return Err(Thrown("RangeError: invalid ISO date".into()));
        }
        let idx = self.heap.alloc(HeapObj::Temporal { kind: 1, fields: vec![y, m, d] });
        if self.plaindate_proto != 0 {
            self.proto_of.insert(idx, Value::heap(self.plaindate_proto));
        }
        Ok(Value::heap(idx))
    }

    fn plain_date_fields(&self, idx: u32) -> Option<(i64, i64, i64)> {
        match self.heap.get(idx) {
            HeapObj::Temporal { kind: 1, fields } => Some((fields[0], fields[1], fields[2])),
            _ => None,
        }
    }

    /// ToTemporalDate: a PlainDate clones; a string parses; an object reads year/
    /// month/day (PlainDateTime also has these — accepted).
    fn to_plain_date(&mut self, v: Value) -> Result<(i64, i64, i64), Thrown> {
        if v.is_heap() {
            if let Some(t) = self.plain_date_fields(v.heap_index()) {
                return Ok(t);
            }
            if self.heap.is_str_like(v.heap_index()) {
                let s = self.heap.str_cow(v.heap_index()).unwrap().into_owned();
                return parse_iso_date(&s)
                    .ok_or_else(|| Thrown(format!("RangeError: invalid date string '{s}'")));
            }
            if matches!(self.heap.get(v.heap_index()), HeapObj::Object(_)) {
                let yv = self.get_prop(v, "year")?;
                let mv = self.get_prop(v, "month")?;
                let dv = self.get_prop(v, "day")?;
                if yv == Value::UNDEFINED || mv == Value::UNDEFINED || dv == Value::UNDEFINED {
                    return Err(Thrown("TypeError: PlainDate-like requires year, month, day".into()));
                }
                let (y, m, d) =
                    (self.to_number(yv)? as i64, self.to_number(mv)? as i64, self.to_number(dv)? as i64);
                if !(1..=12).contains(&m) || d < 1 || d > days_in_month(y, m) {
                    return Err(Thrown("RangeError: invalid date fields".into()));
                }
                return Ok((y, m, d));
            }
        }
        Err(Thrown("TypeError: cannot convert value to a Temporal.PlainDate".into()))
    }

    /// `date ± duration` (date units constrain day; time units fold to whole days).
    fn date_add(&self, y: i64, m: i64, d: i64, dur: &[i64; 10], sign: i64) -> (i64, i64, i64) {
        let total_months = (y + dur[0] * sign) * 12 + (m - 1) + dur[1] * sign;
        let ny = total_months.div_euclid(12);
        let nm = total_months.rem_euclid(12) + 1;
        let nd = d.min(days_in_month(ny, nm));
        let time_ns = (dur[4] as i128) * 3_600_000_000_000
            + (dur[5] as i128) * 60_000_000_000
            + (dur[6] as i128) * 1_000_000_000
            + (dur[7] as i128) * 1_000_000
            + (dur[8] as i128) * 1_000
            + (dur[9] as i128);
        let extra_days = (time_ns / 86_400_000_000_000) as i64;
        let ed = iso_to_epoch_days(ny, nm, nd) + (dur[2] * 7 + dur[3] + extra_days) * sign;
        epoch_days_to_iso(ed)
    }

    fn plain_date_method(&mut self, idx: u32, name: &str, args: &[Value]) -> Result<Option<Value>, Thrown> {
        let (y, m, d) = match self.plain_date_fields(idx) {
            Some(t) => t,
            None => return Ok(None),
        };
        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        match name {
            "toString" | "toJSON" => Ok(Some(self.alloc_str(iso_date_string(y, m, d)))),
            "valueOf" => Err(Thrown("TypeError: Called Temporal.PlainDate.prototype.valueOf".into())),
            "equals" => {
                let other = self.to_plain_date(a0)?;
                Ok(Some(Value::bool((y, m, d) == other)))
            }
            "with" => {
                let ny = self.opt_int_field(a0, "year")?.unwrap_or(y);
                let nm = self.opt_int_field(a0, "month")?.unwrap_or(m);
                let nd = self.opt_int_field(a0, "day")?.unwrap_or(d);
                Ok(Some(self.make_plain_date(ny, nm, nd)?))
            }
            "add" | "subtract" => {
                let dur = self.to_duration(a0)?;
                let sign = if name == "add" { 1 } else { -1 };
                let (ny, nm, nd) = self.date_add(y, m, d, &dur, sign);
                Ok(Some(self.make_plain_date(ny, nm, nd)?))
            }
            "until" | "since" => {
                let other = self.to_plain_date(a0)?;
                let from = iso_to_epoch_days(y, m, d);
                let to = iso_to_epoch_days(other.0, other.1, other.2);
                let days = if name == "until" { to - from } else { from - to };
                let mut f = [0i64; 10];
                f[3] = days;
                Ok(Some(self.make_duration(f)))
            }
            "getISOFields" => {
                let cal = self.alloc_str("iso8601".to_string());
                let mut o = ObjMap::new();
                o.set("isoYear", Value::num(y as f64));
                o.set("isoMonth", Value::num(m as f64));
                o.set("isoDay", Value::num(d as f64));
                o.set("calendar", cal);
                Ok(Some(Value::heap(self.heap.alloc(HeapObj::Object(o)))))
            }
            _ => Ok(None),
        }
    }

    /// Read an optional integer field from an options/with object (None if absent).
    fn opt_int_field(&mut self, obj: Value, key: &str) -> Result<Option<i64>, Thrown> {
        let v = self.get_prop(obj, key)?;
        if v == Value::UNDEFINED {
            Ok(None)
        } else {
            Ok(Some(self.to_number(v)? as i64))
        }
    }

    // ── Temporal.PlainTime ──

    fn make_plain_time(&mut self, f: [i64; 6]) -> Result<Value, Thrown> {
        if !(0..24).contains(&f[0])
            || !(0..60).contains(&f[1])
            || !(0..60).contains(&f[2])
            || !(0..1000).contains(&f[3])
            || !(0..1000).contains(&f[4])
            || !(0..1000).contains(&f[5])
        {
            return Err(Thrown("RangeError: invalid time value".into()));
        }
        let idx = self.heap.alloc(HeapObj::Temporal { kind: 2, fields: f.to_vec() });
        if self.plaintime_proto != 0 {
            self.proto_of.insert(idx, Value::heap(self.plaintime_proto));
        }
        Ok(Value::heap(idx))
    }

    fn plain_time_fields(&self, idx: u32) -> Option<[i64; 6]> {
        match self.heap.get(idx) {
            HeapObj::Temporal { kind: 2, fields } => {
                let mut f = [0i64; 6];
                for (i, s) in f.iter_mut().enumerate() {
                    *s = *fields.get(i).unwrap_or(&0);
                }
                Some(f)
            }
            _ => None,
        }
    }

    fn to_plain_time(&mut self, v: Value) -> Result<[i64; 6], Thrown> {
        if v.is_heap() {
            if let Some(f) = self.plain_time_fields(v.heap_index()) {
                return Ok(f);
            }
            if self.heap.is_str_like(v.heap_index()) {
                let s = self.heap.str_cow(v.heap_index()).unwrap().into_owned();
                return parse_iso_time(&s)
                    .ok_or_else(|| Thrown(format!("RangeError: invalid time string '{s}'")));
            }
            if matches!(self.heap.get(v.heap_index()), HeapObj::Object(_)) {
                let names =
                    ["hour", "minute", "second", "millisecond", "microsecond", "nanosecond"];
                let mut f = [0i64; 6];
                for (i, nm) in names.iter().enumerate() {
                    if let Some(x) = self.opt_int_field(v, nm)? {
                        f[i] = x;
                    }
                }
                return Ok(f);
            }
        }
        Err(Thrown("TypeError: cannot convert value to a Temporal.PlainTime".into()))
    }

    fn plain_time_method(&mut self, idx: u32, name: &str, args: &[Value]) -> Result<Option<Value>, Thrown> {
        let f = match self.plain_time_fields(idx) {
            Some(f) => f,
            None => return Ok(None),
        };
        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        match name {
            "toString" | "toJSON" => Ok(Some(self.alloc_str(time_string(&f)))),
            "valueOf" => Err(Thrown("TypeError: Called Temporal.PlainTime.prototype.valueOf".into())),
            "equals" => {
                let o = self.to_plain_time(a0)?;
                Ok(Some(Value::bool(f == o)))
            }
            "with" => {
                let names =
                    ["hour", "minute", "second", "millisecond", "microsecond", "nanosecond"];
                let mut nf = f;
                let mut any = false;
                for (i, nm) in names.iter().enumerate() {
                    if let Some(x) = self.opt_int_field(a0, nm)? {
                        nf[i] = x;
                        any = true;
                    }
                }
                if !any {
                    return Err(Thrown("TypeError: with() requires a partial time object".into()));
                }
                Ok(Some(self.make_plain_time(nf)?))
            }
            "add" | "subtract" => {
                let dur = self.to_duration(a0)?;
                let sign: i128 = if name == "add" { 1 } else { -1 };
                let dur_ns = ((dur[4] as i128) * 3_600_000_000_000
                    + (dur[5] as i128) * 60_000_000_000
                    + (dur[6] as i128) * 1_000_000_000
                    + (dur[7] as i128) * 1_000_000
                    + (dur[8] as i128) * 1_000
                    + (dur[9] as i128))
                    * sign;
                let total = (time_to_ns(&f) + dur_ns).rem_euclid(86_400_000_000_000);
                Ok(Some(self.make_plain_time(ns_to_time(total))?))
            }
            "until" | "since" => {
                let o = self.to_plain_time(a0)?;
                let diff = if name == "until" {
                    time_to_ns(&o) - time_to_ns(&f)
                } else {
                    time_to_ns(&f) - time_to_ns(&o)
                };
                let t = ns_to_time(diff.abs());
                let mut df = [0i64; 10];
                df[4..10].copy_from_slice(&t);
                if diff < 0 {
                    for x in df.iter_mut() {
                        *x = -*x;
                    }
                }
                Ok(Some(self.make_duration(df)))
            }
            "getISOFields" => {
                let cal = self.alloc_str("iso8601".to_string());
                let mut o = ObjMap::new();
                let names = [
                    "isoHour",
                    "isoMinute",
                    "isoSecond",
                    "isoMillisecond",
                    "isoMicrosecond",
                    "isoNanosecond",
                ];
                for (i, nm) in names.iter().enumerate() {
                    o.set(nm, Value::num(f[i] as f64));
                }
                o.set("calendar", cal);
                Ok(Some(Value::heap(self.heap.alloc(HeapObj::Object(o)))))
            }
            _ => Ok(None),
        }
    }

    // ── Temporal.PlainDateTime ──

    fn make_plain_date_time(&mut self, f: [i64; 9]) -> Result<Value, Thrown> {
        if !(1..=12).contains(&f[1])
            || f[2] < 1
            || f[2] > days_in_month(f[0], f[1])
            || !(-271821..=275760).contains(&f[0])
            || !(0..24).contains(&f[3])
            || !(0..60).contains(&f[4])
            || !(0..60).contains(&f[5])
            || !(0..1000).contains(&f[6])
            || !(0..1000).contains(&f[7])
            || !(0..1000).contains(&f[8])
        {
            return Err(Thrown("RangeError: invalid PlainDateTime value".into()));
        }
        let idx = self.heap.alloc(HeapObj::Temporal { kind: 3, fields: f.to_vec() });
        if self.plaindatetime_proto != 0 {
            self.proto_of.insert(idx, Value::heap(self.plaindatetime_proto));
        }
        Ok(Value::heap(idx))
    }

    fn pdt_fields(&self, idx: u32) -> Option<[i64; 9]> {
        match self.heap.get(idx) {
            HeapObj::Temporal { kind: 3, fields } => {
                let mut f = [0i64; 9];
                for (i, s) in f.iter_mut().enumerate() {
                    *s = *fields.get(i).unwrap_or(&0);
                }
                Some(f)
            }
            _ => None,
        }
    }

    fn to_plain_date_time(&mut self, v: Value) -> Result<[i64; 9], Thrown> {
        if v.is_heap() {
            if let Some(f) = self.pdt_fields(v.heap_index()) {
                return Ok(f);
            }
            if let Some((y, m, d)) = self.plain_date_fields(v.heap_index()) {
                return Ok([y, m, d, 0, 0, 0, 0, 0, 0]);
            }
            if self.heap.is_str_like(v.heap_index()) {
                let s = self.heap.str_cow(v.heap_index()).unwrap().into_owned();
                return parse_iso_datetime(&s)
                    .ok_or_else(|| Thrown(format!("RangeError: invalid datetime string '{s}'")));
            }
            if matches!(self.heap.get(v.heap_index()), HeapObj::Object(_)) {
                let names = [
                    "year", "month", "day", "hour", "minute", "second", "millisecond",
                    "microsecond", "nanosecond",
                ];
                let mut f = [0i64; 9];
                let mut have_date = [false; 3];
                for (i, nm) in names.iter().enumerate() {
                    if let Some(x) = self.opt_int_field(v, nm)? {
                        f[i] = x;
                        if i < 3 {
                            have_date[i] = true;
                        }
                    }
                }
                if !have_date.iter().all(|&b| b) {
                    return Err(Thrown("TypeError: PlainDateTime-like requires year, month, day".into()));
                }
                return Ok(f);
            }
        }
        Err(Thrown("TypeError: cannot convert value to a Temporal.PlainDateTime".into()))
    }

    fn plain_date_time_method(&mut self, idx: u32, name: &str, args: &[Value]) -> Result<Option<Value>, Thrown> {
        let f = match self.pdt_fields(idx) {
            Some(f) => f,
            None => return Ok(None),
        };
        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        let date = [f[0], f[1], f[2]];
        let time = [f[3], f[4], f[5], f[6], f[7], f[8]];
        match name {
            "toString" | "toJSON" => {
                let s = format!("{}T{}", iso_date_string(date[0], date[1], date[2]), time_string(&time));
                Ok(Some(self.alloc_str(s)))
            }
            "valueOf" => {
                Err(Thrown("TypeError: Called Temporal.PlainDateTime.prototype.valueOf".into()))
            }
            "equals" => {
                let o = self.to_plain_date_time(a0)?;
                Ok(Some(Value::bool(f == o)))
            }
            "toPlainDate" => Ok(Some(self.make_plain_date(date[0], date[1], date[2])?)),
            "toPlainTime" => Ok(Some(self.make_plain_time(time)?)),
            "with" => {
                let names = [
                    "year", "month", "day", "hour", "minute", "second", "millisecond",
                    "microsecond", "nanosecond",
                ];
                let mut nf = f;
                let mut any = false;
                for (i, nm) in names.iter().enumerate() {
                    if let Some(x) = self.opt_int_field(a0, nm)? {
                        nf[i] = x;
                        any = true;
                    }
                }
                if !any {
                    return Err(Thrown("TypeError: with() requires a partial object".into()));
                }
                Ok(Some(self.make_plain_date_time(nf)?))
            }
            "add" | "subtract" => {
                let dur = self.to_duration(a0)?;
                let sign: i64 = if name == "add" { 1 } else { -1 };
                // Time part with day carry.
                let tns = time_to_ns(&time)
                    + ((dur[4] as i128) * 3_600_000_000_000
                        + (dur[5] as i128) * 60_000_000_000
                        + (dur[6] as i128) * 1_000_000_000
                        + (dur[7] as i128) * 1_000_000
                        + (dur[8] as i128) * 1_000
                        + (dur[9] as i128))
                        * sign as i128;
                let carry = tns.div_euclid(86_400_000_000_000) as i64;
                let nt = ns_to_time(tns.rem_euclid(86_400_000_000_000));
                // Date part: years/months constrain, then weeks/days + carry.
                let tm = (date[0] + dur[0] * sign) * 12 + (date[1] - 1) + dur[1] * sign;
                let ny0 = tm.div_euclid(12);
                let nmo = tm.rem_euclid(12) + 1;
                let nd0 = date[2].min(days_in_month(ny0, nmo));
                let ed = iso_to_epoch_days(ny0, nmo, nd0) + (dur[2] * 7 + dur[3]) * sign + carry;
                let (ny, nm, nd) = epoch_days_to_iso(ed);
                Ok(Some(self.make_plain_date_time([
                    ny, nm, nd, nt[0], nt[1], nt[2], nt[3], nt[4], nt[5],
                ])?))
            }
            "until" | "since" => {
                let o = self.to_plain_date_time(a0)?;
                let a_ns = iso_to_epoch_days(f[0], f[1], f[2]) as i128 * 86_400_000_000_000
                    + time_to_ns(&time);
                let b_ns = iso_to_epoch_days(o[0], o[1], o[2]) as i128 * 86_400_000_000_000
                    + time_to_ns(&[o[3], o[4], o[5], o[6], o[7], o[8]]);
                let diff = if name == "until" { b_ns - a_ns } else { a_ns - b_ns };
                let days = (diff.abs() / 86_400_000_000_000) as i64;
                let t = ns_to_time(diff.abs() % 86_400_000_000_000);
                let mut df = [0i64; 10];
                df[3] = days;
                df[4..10].copy_from_slice(&t);
                if diff < 0 {
                    for x in df.iter_mut() {
                        *x = -*x;
                    }
                }
                Ok(Some(self.make_duration(df)))
            }
            "getISOFields" => {
                let cal = self.alloc_str("iso8601".to_string());
                let names = [
                    "isoYear",
                    "isoMonth",
                    "isoDay",
                    "isoHour",
                    "isoMinute",
                    "isoSecond",
                    "isoMillisecond",
                    "isoMicrosecond",
                    "isoNanosecond",
                ];
                let mut o = ObjMap::new();
                for (i, nm) in names.iter().enumerate() {
                    o.set(nm, Value::num(f[i] as f64));
                }
                o.set("calendar", cal);
                Ok(Some(Value::heap(self.heap.alloc(HeapObj::Object(o)))))
            }
            _ => Ok(None),
        }
    }

    // ── Temporal.Instant ──

    fn make_instant(&mut self, ns: i128) -> Result<Value, Thrown> {
        if ns.abs() > 8_640_000_000_000_000_000_000 {
            return Err(Thrown("RangeError: Instant outside the supported range".into()));
        }
        let hi = (ns >> 64) as i64;
        let lo = ns as i64;
        let idx = self.heap.alloc(HeapObj::Temporal { kind: 4, fields: vec![hi, lo] });
        if self.instant_proto != 0 {
            self.proto_of.insert(idx, Value::heap(self.instant_proto));
        }
        Ok(Value::heap(idx))
    }

    fn instant_ns(&self, idx: u32) -> Option<i128> {
        match self.heap.get(idx) {
            HeapObj::Temporal { kind: 4, fields } => {
                Some(((fields[0] as i128) << 64) | ((fields[1] as u64) as i128))
            }
            _ => None,
        }
    }

    fn to_instant_ns(&mut self, v: Value) -> Result<i128, Thrown> {
        if v.is_heap() {
            if let Some(ns) = self.instant_ns(v.heap_index()) {
                return Ok(ns);
            }
            if self.heap.is_str_like(v.heap_index()) {
                let s = self.heap.str_cow(v.heap_index()).unwrap().into_owned();
                return instant_str_to_ns(&s)
                    .ok_or_else(|| Thrown(format!("RangeError: invalid instant string '{s}'")));
            }
        }
        Err(Thrown("TypeError: cannot convert value to a Temporal.Instant".into()))
    }

    fn instant_method(&mut self, idx: u32, name: &str, args: &[Value]) -> Result<Option<Value>, Thrown> {
        let ns = match self.instant_ns(idx) {
            Some(n) => n,
            None => return Ok(None),
        };
        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        match name {
            "toString" | "toJSON" => Ok(Some(self.alloc_str(instant_to_string(ns)))),
            "valueOf" => Err(Thrown("TypeError: Called Temporal.Instant.prototype.valueOf".into())),
            "equals" => {
                let o = self.to_instant_ns(a0)?;
                Ok(Some(Value::bool(ns == o)))
            }
            "add" | "subtract" => {
                let dur = self.to_duration(a0)?;
                if dur[0] != 0 || dur[1] != 0 || dur[2] != 0 || dur[3] != 0 {
                    return Err(Thrown(
                        "RangeError: Instant arithmetic does not accept calendar (date) units".into(),
                    ));
                }
                let sign: i128 = if name == "add" { 1 } else { -1 };
                let dns = ((dur[4] as i128) * 3_600_000_000_000
                    + (dur[5] as i128) * 60_000_000_000
                    + (dur[6] as i128) * 1_000_000_000
                    + (dur[7] as i128) * 1_000_000
                    + (dur[8] as i128) * 1_000
                    + (dur[9] as i128))
                    * sign;
                Ok(Some(self.make_instant(ns + dns)?))
            }
            "until" | "since" => {
                let o = self.to_instant_ns(a0)?;
                let diff = if name == "until" { o - ns } else { ns - o };
                let s = (diff / 1_000_000_000) as i64;
                let sub = (diff % 1_000_000_000) as i64;
                let mut df = [0i64; 10];
                df[6] = s;
                df[7] = sub / 1_000_000;
                df[8] = (sub / 1_000) % 1_000;
                df[9] = sub % 1_000;
                Ok(Some(self.make_duration(df)))
            }
            _ => Ok(None),
        }
    }

    // ── Temporal.PlainYearMonth ──

    fn make_plain_year_month(&mut self, y: i64, m: i64, ref_day: i64) -> Result<Value, Thrown> {
        if !(1..=12).contains(&m) || !(-271821..=275760).contains(&y) {
            return Err(Thrown("RangeError: invalid year-month value".into()));
        }
        let idx = self.heap.alloc(HeapObj::Temporal { kind: 5, fields: vec![y, m, ref_day] });
        if self.plainyearmonth_proto != 0 {
            self.proto_of.insert(idx, Value::heap(self.plainyearmonth_proto));
        }
        Ok(Value::heap(idx))
    }

    fn pym_fields(&self, idx: u32) -> Option<(i64, i64, i64)> {
        match self.heap.get(idx) {
            HeapObj::Temporal { kind: 5, fields } => {
                Some((fields[0], fields[1], *fields.get(2).unwrap_or(&1)))
            }
            _ => None,
        }
    }

    fn to_plain_year_month(&mut self, v: Value) -> Result<(i64, i64, i64), Thrown> {
        if v.is_heap() {
            if let Some(t) = self.pym_fields(v.heap_index()) {
                return Ok(t);
            }
            if self.heap.is_str_like(v.heap_index()) {
                let s = self.heap.str_cow(v.heap_index()).unwrap().into_owned();
                return parse_iso_year_month(&s)
                    .ok_or_else(|| Thrown(format!("RangeError: invalid year-month string '{s}'")));
            }
            if matches!(self.heap.get(v.heap_index()), HeapObj::Object(_)) {
                let yv = self.get_prop(v, "year")?;
                let m = self.read_month_field(v)?;
                if yv == Value::UNDEFINED || m.is_none() {
                    return Err(Thrown(
                        "TypeError: PlainYearMonth-like requires year and month".into(),
                    ));
                }
                let y = self.to_number(yv)? as i64;
                let m = m.unwrap();
                if !(1..=12).contains(&m) {
                    return Err(Thrown("RangeError: month out of range".into()));
                }
                return Ok((y, m, 1));
            }
        }
        Err(Thrown("TypeError: cannot convert value to a Temporal.PlainYearMonth".into()))
    }

    /// Read month from an object: monthCode ("M06") takes precedence over `month`.
    fn read_month_field(&mut self, obj: Value) -> Result<Option<i64>, Thrown> {
        let mc = self.get_prop(obj, "monthCode")?;
        if mc != Value::UNDEFINED {
            let s = self.to_js_string(mc)?;
            return Ok(parse_month_code(&s));
        }
        self.opt_int_field(obj, "month")
    }

    fn plain_year_month_method(
        &mut self,
        idx: u32,
        name: &str,
        args: &[Value],
    ) -> Result<Option<Value>, Thrown> {
        let (y, m, _ref) = match self.pym_fields(idx) {
            Some(t) => t,
            None => return Ok(None),
        };
        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        match name {
            "toString" | "toJSON" => Ok(Some(self.alloc_str(year_month_string(y, m)))),
            "valueOf" => {
                Err(Thrown("TypeError: Called Temporal.PlainYearMonth.prototype.valueOf".into()))
            }
            "equals" => {
                let o = self.to_plain_year_month(a0)?;
                Ok(Some(Value::bool((y, m) == (o.0, o.1))))
            }
            "with" => {
                let ny = self.opt_int_field(a0, "year")?.unwrap_or(y);
                let nm = self.read_month_field(a0)?.unwrap_or(m);
                Ok(Some(self.make_plain_year_month(ny, nm, 1)?))
            }
            "add" | "subtract" => {
                let dur = self.to_duration(a0)?;
                let sign = if name == "add" { 1 } else { -1 };
                let op_sign = sign * Self::duration_sign(&dur);
                // Reference day per spec: start of month for non-negative ops, end of
                // month for negative — so day/week units don't spill into a wrong month.
                let ref_day = if op_sign < 0 { days_in_month(y, m) } else { 1 };
                let (ny, nm, _nd) = self.date_add(y, m, ref_day, &dur, sign);
                Ok(Some(self.make_plain_year_month(ny, nm, 1)?))
            }
            "until" | "since" => {
                let o = self.to_plain_year_month(a0)?;
                let from = y * 12 + (m - 1);
                let to = o.0 * 12 + (o.1 - 1);
                let diff = if name == "until" { to - from } else { from - to };
                let mut f = [0i64; 10];
                f[0] = diff / 12;
                f[1] = diff % 12;
                Ok(Some(self.make_duration(f)))
            }
            "toPlainDate" => {
                let day = self.opt_int_field(a0, "day")?.ok_or_else(|| {
                    Thrown("TypeError: toPlainDate requires a day".into())
                })?;
                Ok(Some(self.make_plain_date(y, m, day)?))
            }
            "getISOFields" => {
                let cal = self.alloc_str("iso8601".to_string());
                let mut o = ObjMap::new();
                o.set("isoYear", Value::num(y as f64));
                o.set("isoMonth", Value::num(m as f64));
                o.set("isoDay", Value::num(_ref as f64));
                o.set("calendar", cal);
                Ok(Some(Value::heap(self.heap.alloc(HeapObj::Object(o)))))
            }
            _ => Ok(None),
        }
    }

    // ── Temporal.PlainMonthDay ──

    fn make_plain_month_day(&mut self, m: i64, d: i64, ref_year: i64) -> Result<Value, Thrown> {
        if !(1..=12).contains(&m) || d < 1 || d > days_in_month(ref_year, m) {
            return Err(Thrown("RangeError: invalid month-day value".into()));
        }
        let idx = self.heap.alloc(HeapObj::Temporal { kind: 6, fields: vec![ref_year, m, d] });
        if self.plainmonthday_proto != 0 {
            self.proto_of.insert(idx, Value::heap(self.plainmonthday_proto));
        }
        Ok(Value::heap(idx))
    }

    fn pmd_fields(&self, idx: u32) -> Option<(i64, i64, i64)> {
        match self.heap.get(idx) {
            HeapObj::Temporal { kind: 6, fields } => {
                Some((fields[0], fields[1], fields[2]))
            }
            _ => None,
        }
    }

    fn to_plain_month_day(&mut self, v: Value) -> Result<(i64, i64, i64), Thrown> {
        if v.is_heap() {
            if let Some(t) = self.pmd_fields(v.heap_index()) {
                return Ok(t);
            }
            if self.heap.is_str_like(v.heap_index()) {
                let s = self.heap.str_cow(v.heap_index()).unwrap().into_owned();
                return parse_iso_month_day(&s)
                    .ok_or_else(|| Thrown(format!("RangeError: invalid month-day string '{s}'")));
            }
            if matches!(self.heap.get(v.heap_index()), HeapObj::Object(_)) {
                let m = self.read_month_field(v)?;
                let dv = self.get_prop(v, "day")?;
                if m.is_none() || dv == Value::UNDEFINED {
                    return Err(Thrown(
                        "TypeError: PlainMonthDay-like requires month and day".into(),
                    ));
                }
                let m = m.unwrap();
                let d = self.to_number(dv)? as i64;
                if !(1..=12).contains(&m) || d < 1 || d > days_in_month(1972, m) {
                    return Err(Thrown("RangeError: month-day out of range".into()));
                }
                return Ok((1972, m, d));
            }
        }
        Err(Thrown("TypeError: cannot convert value to a Temporal.PlainMonthDay".into()))
    }

    fn plain_month_day_method(
        &mut self,
        idx: u32,
        name: &str,
        args: &[Value],
    ) -> Result<Option<Value>, Thrown> {
        let (ry, m, d) = match self.pmd_fields(idx) {
            Some(t) => t,
            None => return Ok(None),
        };
        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        match name {
            "toString" | "toJSON" => Ok(Some(self.alloc_str(format!("{m:02}-{d:02}")))),
            "valueOf" => {
                Err(Thrown("TypeError: Called Temporal.PlainMonthDay.prototype.valueOf".into()))
            }
            "equals" => {
                let o = self.to_plain_month_day(a0)?;
                Ok(Some(Value::bool((ry, m, d) == o)))
            }
            "with" => {
                let nm = self.read_month_field(a0)?.unwrap_or(m);
                let nd = self.opt_int_field(a0, "day")?.unwrap_or(d);
                Ok(Some(self.make_plain_month_day(nm, nd, ry)?))
            }
            "toPlainDate" => {
                let year = self.opt_int_field(a0, "year")?.ok_or_else(|| {
                    Thrown("TypeError: toPlainDate requires a year".into())
                })?;
                Ok(Some(self.make_plain_date(year, m, d)?))
            }
            "getISOFields" => {
                let cal = self.alloc_str("iso8601".to_string());
                let mut o = ObjMap::new();
                o.set("isoYear", Value::num(ry as f64));
                o.set("isoMonth", Value::num(m as f64));
                o.set("isoDay", Value::num(d as f64));
                o.set("calendar", cal);
                Ok(Some(Value::heap(self.heap.alloc(HeapObj::Object(o)))))
            }
            _ => Ok(None),
        }
    }

    /// `new Proxy(target, handler)` — both must be objects.
    fn make_proxy(&mut self, target: Value, handler: Value) -> Result<Value, Thrown> {
        if !self.is_object_value(target) || !self.is_object_value(handler) {
            return Err(Thrown(
                "TypeError: Cannot create proxy with a non-object as target or handler".into(),
            ));
        }
        Ok(Value::heap(self.heap.alloc(HeapObj::Proxy { target, handler, revoked: false })))
    }

    fn proxy_parts(&self, idx: u32) -> Option<(Value, Value, bool)> {
        match self.heap.get(idx) {
            HeapObj::Proxy { target, handler, revoked } => Some((*target, *handler, *revoked)),
            _ => None,
        }
    }

    /// Reconstruct a property KEY as a Value (a Symbol for an `@@`-encoded key,
    /// else a string) — so a Proxy trap / Reflect receives the real key.
    fn key_to_value(&mut self, key: &str) -> Value {
        if key.starts_with("@@") {
            if let Some(&sym) = self.symbol_keys.get(key) {
                return sym;
            }
        }
        self.alloc_str(key.to_string())
    }

    /// Look up a Proxy handler trap by name; `Ok(Some(fn))` if it's callable,
    /// `Ok(None)` to fall through to the target. A non-callable non-undefined trap
    /// is a TypeError. (`revoked` is checked by the caller.)
    fn proxy_trap(&mut self, handler: Value, name: &str) -> Result<Option<Value>, Thrown> {
        let t = self.get_prop(handler, name)?;
        if t.is_undefined() || t.is_null() {
            Ok(None)
        } else if self.is_callable(t) {
            Ok(Some(t))
        } else {
            Err(Thrown(format!("TypeError: proxy handler's {name} trap is not a function")))
        }
    }

    fn set_regexp_last_index(&mut self, idx: u32, n: usize) {
        if let HeapObj::RegExp { last_index, .. } = self.heap.get_mut(idx) {
            *last_index = n;
        }
    }

    /// The heap index if `v` is a RegExp, else None.
    fn as_regexp(&self, v: Value) -> Option<u32> {
        if v.is_heap() && matches!(self.heap.get(v.heap_index()), HeapObj::RegExp { .. }) {
            Some(v.heap_index())
        } else {
            None
        }
    }

    /// Coerce a `String.prototype.match`/`search` argument to a RegExp: a RegExp
    /// passes through; anything else becomes `new RegExp(arg)`.
    fn to_regexp_arg(&mut self, v: Value) -> Result<u32, Thrown> {
        if let Some(i) = self.as_regexp(v) {
            return Ok(i);
        }
        let p = if v.is_undefined() { self.alloc_str(String::new()) } else { v };
        Ok(self.build_regexp(p, Value::UNDEFINED)?.heap_index())
    }

    /// Expand a `String.prototype.replace` string template against a match: `$&`
    /// (whole), `` $` ``/`$'` (pre/post), `$N`/`$NN` (group), `$<name>` (named), `$$`.
    fn expand_replacement(
        &self,
        tmpl: &str,
        whole: &str,
        groups: &[Option<String>],
        named: &[(String, Option<String>)],
        pre: &str,
        post: &str,
    ) -> String {
        let mut out = String::with_capacity(tmpl.len());
        let bytes = tmpl.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'$' && i + 1 < bytes.len() {
                let c = bytes[i + 1];
                match c {
                    b'$' => {
                        out.push('$');
                        i += 2;
                    }
                    b'&' => {
                        out.push_str(whole);
                        i += 2;
                    }
                    b'`' => {
                        out.push_str(pre);
                        i += 2;
                    }
                    b'\'' => {
                        out.push_str(post);
                        i += 2;
                    }
                    b'<' => {
                        if let Some(end) = tmpl[i + 2..].find('>') {
                            let name = &tmpl[i + 2..i + 2 + end];
                            if let Some((_, Some(g))) = named.iter().find(|(n, _)| n == name) {
                                out.push_str(g);
                            }
                            i += 2 + end + 1;
                        } else {
                            out.push('$');
                            i += 1;
                        }
                    }
                    b'0'..=b'9' => {
                        // One or two digits; prefer the two-digit group if valid.
                        let d1 = (c - b'0') as usize;
                        let two = if i + 2 < bytes.len() && bytes[i + 2].is_ascii_digit() {
                            Some(d1 * 10 + (bytes[i + 2] - b'0') as usize)
                        } else {
                            None
                        };
                        if let Some(n) = two.filter(|&n| n >= 1 && n <= groups.len()) {
                            if let Some(g) = &groups[n - 1] {
                                out.push_str(g);
                            }
                            i += 3;
                        } else if d1 >= 1 && d1 <= groups.len() {
                            if let Some(g) = &groups[d1 - 1] {
                                out.push_str(g);
                            }
                            i += 2;
                        } else {
                            out.push('$');
                            i += 1;
                        }
                    }
                    _ => {
                        out.push('$');
                        i += 1;
                    }
                }
            } else {
                // copy one UTF-8 char
                let ch = tmpl[i..].chars().next().unwrap();
                out.push(ch);
                i += ch.len_utf8();
            }
        }
        out
    }

    /// RegExp instance property reads: `lastIndex`, `source` (empty → "(?:)"),
    /// `flags`, and the per-flag booleans; methods delegate to RegExp.prototype.
    fn regexp_get_prop(
        &mut self,
        source: &str,
        flags: &str,
        last_index: usize,
        key: &str,
    ) -> Result<Value, Thrown> {
        Ok(match key {
            "lastIndex" => Value::num(last_index as f64),
            "source" => {
                let s = if source.is_empty() { "(?:)".to_string() } else { source.to_string() };
                self.alloc_str(s)
            }
            "flags" => self.alloc_str(flags.to_string()),
            "global" => Value::bool(flags.contains('g')),
            "ignoreCase" => Value::bool(flags.contains('i')),
            "multiline" => Value::bool(flags.contains('m')),
            "dotAll" => Value::bool(flags.contains('s')),
            "unicode" => Value::bool(flags.contains('u')),
            "unicodeSets" => Value::bool(flags.contains('v')),
            "sticky" => Value::bool(flags.contains('y')),
            "hasIndices" => Value::bool(flags.contains('d')),
            _ => self.proto_member(self.regexp_proto, key),
        })
    }

    /// `RegExp.prototype.exec(input)`: returns the match-result Array (group 0 +
    /// captures, with `.index`/`.input`/`.groups` in the side table) or `null`.
    /// Advances `lastIndex` for a global/sticky regex.
    fn regexp_exec(&mut self, re_idx: u32, input_v: Value) -> Result<Value, Thrown> {
        let input = self.to_js_string(input_v)?;
        let (global, sticky, start_char) = match self.heap.get(re_idx) {
            HeapObj::RegExp { flags, last_index, .. } => {
                (flags.contains('g'), flags.contains('y'), *last_index)
            }
            _ => {
                return Err(Thrown(
                    "TypeError: RegExp.prototype.exec called on a non-RegExp".into(),
                ))
            }
        };
        let stateful = global || sticky;
        let start = if stateful { start_char } else { 0 };
        let byte_start = char_to_byte(&input, start);
        let found = if start > input.chars().count() {
            None
        } else {
            match self.heap.get(re_idx) {
                HeapObj::RegExp { regex, .. } => regex.find_from(&input, byte_start).next(),
                _ => None,
            }
        };
        // Sticky: the match must begin exactly at the search start.
        let found = found.filter(|m| !(sticky && m.start() != byte_start));
        let m = match found {
            Some(m) => m,
            None => {
                if stateful {
                    self.set_regexp_last_index(re_idx, 0);
                }
                return Ok(Value::NULL);
            }
        };
        let (mstart, mend) = (m.start(), m.end());
        let whole = self.alloc_str(input[m.range()].to_string());
        let mut elems = vec![whole];
        let caps = m.captures.clone();
        for cap in &caps {
            let v = match cap {
                Some(r) => self.alloc_str(input[r.clone()].to_string()),
                None => Value::UNDEFINED,
            };
            elems.push(v);
        }
        let named: Vec<(String, Option<std::ops::Range<usize>>)> =
            m.named_groups().map(|(n, r)| (n.to_string(), r)).collect();
        let groups = if named.is_empty() {
            Value::UNDEFINED
        } else {
            let mut gm = ObjMap::new();
            for (name, r) in &named {
                let v = match r {
                    Some(r) => self.alloc_str(input[r.clone()].to_string()),
                    None => Value::UNDEFINED,
                };
                gm.set(name, v);
            }
            Value::heap(self.heap.alloc(HeapObj::Object(gm)))
        };
        let arr_idx = self.heap.alloc(HeapObj::Array(elems));
        let index_v = Value::num(byte_to_char(&input, mstart) as f64);
        let input_sv = self.alloc_str(input.clone());
        self.regexp_match_extras.insert(arr_idx, (index_v, input_sv, groups));
        if stateful {
            self.set_regexp_last_index(re_idx, byte_to_char(&input, mend));
        }
        Ok(Value::heap(arr_idx))
    }

    /// Regex-backed `String.prototype.replace`/`replaceAll`. `repl` is a function
    /// (called `(match, ...groups, offset, input)`) or a template string (`$&`/`$N`/…).
    fn regex_replace(&mut self, s: &str, re: u32, repl: Value, global: bool) -> Result<String, Thrown> {
        let matches: Vec<regress::Match> = match self.heap.get(re) {
            HeapObj::RegExp { regex, .. } => {
                if global {
                    regex.find_iter(s).collect()
                } else {
                    regex.find(s).into_iter().collect()
                }
            }
            _ => Vec::new(),
        };
        let callable = repl.is_heap() && self.heap.as_callable(repl.heap_index()).is_some();
        let repl_str = if callable { String::new() } else { self.to_js_string(repl)? };
        let mut out = String::new();
        let mut last = 0usize;
        for m in &matches {
            let (st, en) = (m.start(), m.end());
            if st < last {
                continue;
            }
            out.push_str(&s[last..st]);
            let whole = s[m.range()].to_string();
            if callable {
                let mut argv = vec![self.alloc_str(whole)];
                for cap in &m.captures {
                    argv.push(match cap {
                        Some(r) => self.alloc_str(s[r.clone()].to_string()),
                        None => Value::UNDEFINED,
                    });
                }
                argv.push(Value::num(byte_to_char(s, st) as f64));
                argv.push(self.alloc_str(s.to_string()));
                let r = self.call_value(repl, Value::UNDEFINED, &argv)?;
                let rs = self.to_js_string(r)?;
                out.push_str(&rs);
            } else {
                let groups: Vec<Option<String>> =
                    m.captures.iter().map(|c| c.as_ref().map(|r| s[r.clone()].to_string())).collect();
                let named: Vec<(String, Option<String>)> = m
                    .named_groups()
                    .map(|(n, r)| (n.to_string(), r.map(|r| s[r].to_string())))
                    .collect();
                let rep =
                    self.expand_replacement(&repl_str, &whole, &groups, &named, &s[..st], &s[en..]);
                out.push_str(&rep);
            }
            last = en;
        }
        out.push_str(&s[last..]);
        Ok(out)
    }

    // ── TypedArrays / ArrayBuffer / DataView ──

    fn as_array_buffer(&self, v: Value) -> Option<u32> {
        (v.is_heap() && matches!(self.heap.get(v.heap_index()), HeapObj::ArrayBuffer { .. }))
            .then(|| v.heap_index())
    }
    fn as_typed_array(&self, v: Value) -> Option<u32> {
        (v.is_heap() && matches!(self.heap.get(v.heap_index()), HeapObj::TypedArray { .. }))
            .then(|| v.heap_index())
    }
    fn array_buffer_len(&self, idx: u32) -> usize {
        match self.heap.get(idx) {
            HeapObj::ArrayBuffer { data, .. } => data.len(),
            _ => 0,
        }
    }
    fn alloc_array_buffer(&mut self, byte_len: usize) -> u32 {
        let idx = self.heap.alloc(HeapObj::ArrayBuffer { data: vec![0u8; byte_len], detached: false });
        if self.arraybuffer_proto != 0 {
            self.proto_of.insert(idx, Value::heap(self.arraybuffer_proto));
        }
        idx
    }
    /// Allocate a TypedArray view over `buffer`, linked to that kind's prototype.
    fn alloc_typed_array(&mut self, buffer: u32, kind: u8, byte_offset: usize, length: usize) -> Value {
        let idx = self.heap.alloc(HeapObj::TypedArray { buffer, kind, byte_offset, length });
        let p = self.ta_protos[kind as usize];
        if p != 0 {
            self.proto_of.insert(idx, Value::heap(p));
        }
        Value::heap(idx)
    }

    /// Read element `i` of a TypedArray as a Value (number, or BigInt for the
    /// 64-bit BigInt kinds). Out-of-bounds → undefined.
    fn ta_element_get(&mut self, ta_idx: u32, i: usize) -> Value {
        let (kind, bytes) = {
            let (buffer, kind, byte_offset, length) = match self.heap.get(ta_idx) {
                HeapObj::TypedArray { buffer, kind, byte_offset, length } => {
                    (*buffer, *kind, *byte_offset, *length)
                }
                _ => return Value::UNDEFINED,
            };
            if i >= length {
                return Value::UNDEFINED;
            }
            let size = native::TA_KINDS[kind as usize].1;
            let data = match self.heap.get(buffer) {
                HeapObj::ArrayBuffer { data, detached } if !*detached => data,
                _ => return Value::UNDEFINED,
            };
            let off = byte_offset + i * size;
            if off + size > data.len() {
                return Value::UNDEFINED;
            }
            let mut b = [0u8; 8];
            b[..size].copy_from_slice(&data[off..off + size]);
            (kind, b)
        };
        match kind {
            0 => Value::num(bytes[0] as i8 as f64),
            1 | 2 => Value::num(bytes[0] as f64),
            3 => Value::num(i16::from_le_bytes([bytes[0], bytes[1]]) as f64),
            4 => Value::num(u16::from_le_bytes([bytes[0], bytes[1]]) as f64),
            5 => Value::num(i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as f64),
            6 => Value::num(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as f64),
            7 => Value::num(f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as f64),
            8 => Value::num(f64::from_le_bytes(bytes)),
            9 => self.make_bigint(i64::from_le_bytes(bytes) as i128),
            _ => self.make_bigint(u64::from_le_bytes(bytes) as i128),
        }
    }

    /// A TypedArray element as its display string (read-only, no allocation) —
    /// for `display`/`inspect` (ToString of a TypedArray is the comma-join).
    fn ta_elem_string(&self, ta_idx: u32, i: usize) -> String {
        let (buffer, kind, byte_offset, length) = match self.heap.get(ta_idx) {
            HeapObj::TypedArray { buffer, kind, byte_offset, length } => {
                (*buffer, *kind, *byte_offset, *length)
            }
            _ => return String::new(),
        };
        if i >= length {
            return "undefined".to_string();
        }
        let size = native::TA_KINDS[kind as usize].1;
        let data = match self.heap.get(buffer) {
            HeapObj::ArrayBuffer { data, .. } => data,
            _ => return String::new(),
        };
        let off = byte_offset + i * size;
        if off + size > data.len() {
            return "undefined".to_string();
        }
        let b = &data[off..off + size];
        match kind {
            0 => (b[0] as i8).to_string(),
            1 | 2 => b[0].to_string(),
            3 => i16::from_le_bytes([b[0], b[1]]).to_string(),
            4 => u16::from_le_bytes([b[0], b[1]]).to_string(),
            5 => i32::from_le_bytes([b[0], b[1], b[2], b[3]]).to_string(),
            6 => u32::from_le_bytes([b[0], b[1], b[2], b[3]]).to_string(),
            7 => fmt_f64(f32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64),
            8 => fmt_f64(f64::from_le_bytes(b.try_into().unwrap())),
            9 => i64::from_le_bytes(b.try_into().unwrap()).to_string(),
            _ => u64::from_le_bytes(b.try_into().unwrap()).to_string(),
        }
    }

    /// Write `v` to element `i` of a TypedArray (ToNumber/ToBigInt then encode per
    /// the element kind). Out-of-bounds → silent no-op (after coercion).
    fn ta_element_set(&mut self, ta_idx: u32, i: usize, v: Value) -> Result<(), Thrown> {
        let (buffer, kind, byte_offset, length) = match self.heap.get(ta_idx) {
            HeapObj::TypedArray { buffer, kind, byte_offset, length } => {
                (*buffer, *kind, *byte_offset, *length)
            }
            _ => return Ok(()),
        };
        let size = native::TA_KINDS[kind as usize].1;
        let is_bigint = native::TA_KINDS[kind as usize].2;
        // Coerce BEFORE borrowing the buffer mutably (coercion can run user code).
        let bytes: [u8; 8] = if is_bigint {
            let n = self.to_bigint(v)?;
            if kind == 9 {
                let mut o = [0u8; 8];
                o.copy_from_slice(&(n as i64).to_le_bytes());
                o
            } else {
                let mut o = [0u8; 8];
                o.copy_from_slice(&(n as u64).to_le_bytes());
                o
            }
        } else {
            let f = self.to_number(v)?;
            ta_encode(kind, f)
        };
        if i >= length {
            return Ok(());
        }
        if let HeapObj::ArrayBuffer { data, detached } = self.heap.get_mut(buffer) {
            if *detached {
                return Ok(());
            }
            let off = byte_offset + i * size;
            if off + size <= data.len() {
                data[off..off + size].copy_from_slice(&bytes[..size]);
            }
        }
        Ok(())
    }

    /// `new ArrayBuffer(byteLength)`.
    fn build_array_buffer(&mut self, args: &[Value]) -> Result<Value, Thrown> {
        let n = match args.first() {
            Some(&v) if v != Value::UNDEFINED => self.to_number(v)?,
            _ => 0.0,
        };
        if !n.is_finite() || n < 0.0 || n.fract() != 0.0 {
            return Err(Thrown("RangeError: Invalid ArrayBuffer length".into()));
        }
        Ok(Value::heap(self.alloc_array_buffer(n as usize)))
    }

    /// `new <TA>(length | buffer[,off[,len]] | typedArray | array/iterable)`.
    fn build_typed_array(&mut self, kind: u8, args: &[Value]) -> Result<Value, Thrown> {
        let size = native::TA_KINDS[kind as usize].1;
        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        // new TA(buffer, byteOffset?, length?)
        if let Some(buf) = self.as_array_buffer(a0) {
            let byte_offset = match args.get(1) {
                Some(&v) if v != Value::UNDEFINED => self.to_number(v)? as usize,
                _ => 0,
            };
            let buf_len = self.array_buffer_len(buf);
            let length = match args.get(2) {
                Some(&v) if v != Value::UNDEFINED => self.to_number(v)? as usize,
                _ => {
                    if buf_len < byte_offset || (buf_len - byte_offset) % size != 0 {
                        return Err(Thrown("RangeError: byte length not a multiple of element size".into()));
                    }
                    (buf_len - byte_offset) / size
                }
            };
            if byte_offset % size != 0 || byte_offset + length * size > buf_len {
                return Err(Thrown("RangeError: invalid TypedArray length/offset".into()));
            }
            return Ok(self.alloc_typed_array(buf, kind, byte_offset, length));
        }
        // new TA(typedArray) / new TA(array | iterable) → copy element-by-element.
        if a0.is_heap() && !a0.is_uninitialized() {
            let src: Vec<Value> = if let Some(src_ta) = self.as_typed_array(a0) {
                let len = match self.heap.get(src_ta) {
                    HeapObj::TypedArray { length, .. } => *length,
                    _ => 0,
                };
                (0..len).map(|i| self.ta_element_get(src_ta, i)).collect()
            } else {
                self.iterate_to_vec(a0)?
            };
            let len = src.len();
            let buf = self.alloc_array_buffer(len * size);
            let ta = self.alloc_typed_array(buf, kind, 0, len);
            for (i, v) in src.into_iter().enumerate() {
                self.ta_element_set(ta.heap_index(), i, v)?;
            }
            return Ok(ta);
        }
        // new TA(length)
        let length = if a0 == Value::UNDEFINED {
            0
        } else {
            let n = self.to_number(a0)?;
            if !n.is_finite() || n < 0.0 || n.fract() != 0.0 {
                return Err(Thrown("RangeError: invalid typed array length".into()));
            }
            n as usize
        };
        let buf = self.alloc_array_buffer(length * size);
        Ok(self.alloc_typed_array(buf, kind, 0, length))
    }

    /// `new DataView(buffer, byteOffset?, byteLength?)`.
    fn build_data_view(&mut self, args: &[Value]) -> Result<Value, Thrown> {
        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        let buf = self
            .as_array_buffer(a0)
            .ok_or_else(|| Thrown("TypeError: DataView requires an ArrayBuffer".into()))?;
        let buf_len = self.array_buffer_len(buf);
        let byte_offset = match args.get(1) {
            Some(&v) if v != Value::UNDEFINED => self.to_number(v)? as usize,
            _ => 0,
        };
        let byte_length = match args.get(2) {
            Some(&v) if v != Value::UNDEFINED => self.to_number(v)? as usize,
            _ => buf_len.saturating_sub(byte_offset),
        };
        if byte_offset + byte_length > buf_len {
            return Err(Thrown("RangeError: invalid DataView offset/length".into()));
        }
        let idx = self.heap.alloc(HeapObj::DataView { buffer: buf, byte_offset, byte_length });
        if self.dataview_proto != 0 {
            self.proto_of.insert(idx, Value::heap(self.dataview_proto));
        }
        Ok(Value::heap(idx))
    }

    /// A binary arithmetic/bitwise op where at least one operand might be a BigInt.
    /// `Ok(None)` ⇒ neither is a BigInt (caller does its numeric path); `Ok(Some)`
    /// ⇒ both BigInt (result); `Err` ⇒ exactly one BigInt (mixing TypeError) or a
    /// BigInt-specific RangeError (÷0, negative exponent).
    fn bigint_binop(&mut self, op: BigOp, va: Value, vb: Value) -> Result<Option<Value>, Thrown> {
        let (a, b) = (self.bigint_value(va), self.bigint_value(vb));
        if a.is_none() && b.is_none() {
            return Ok(None);
        }
        let (a, b) = match (a, b) {
            (Some(a), Some(b)) => (a, b),
            _ => {
                return Err(Thrown(
                    "TypeError: Cannot mix BigInt and other types, use explicit conversions".into(),
                ))
            }
        };
        let r = match op {
            BigOp::Add => a.wrapping_add(b),
            BigOp::Sub => a.wrapping_sub(b),
            BigOp::Mul => a.wrapping_mul(b),
            BigOp::Div | BigOp::Mod if b == 0 => {
                return Err(Thrown("RangeError: Division by zero".into()))
            }
            BigOp::Div => a.wrapping_div(b),
            BigOp::Mod => a.wrapping_rem(b),
            BigOp::Pow if b < 0 => {
                return Err(Thrown("RangeError: Exponent must be non-negative".into()))
            }
            BigOp::Pow => a.wrapping_pow(b.min(u32::MAX as i128) as u32),
            BigOp::And => a & b,
            BigOp::Or => a | b,
            BigOp::Xor => a ^ b,
            BigOp::Shl => a.wrapping_shl(b as u32),
            BigOp::Shr => a.wrapping_shr(b as u32),
        };
        Ok(Some(self.make_bigint(r)))
    }

    /// `new <class>(args)`: build a plain object, install the class's methods as
    /// own Func properties, then run the constructor (if any) with `this` = the
    /// new object. A constructor that returns an object/array replaces the
    /// instance (JS semantics); otherwise the instance is returned.
    fn construct(&mut self, cv: Value, args: &[Value]) -> Result<Value, Thrown> {
        if !cv.is_heap() {
            return Err(Thrown("TypeError: value is not a constructor".into()));
        }
        // A built-in error constructor used as a VALUE (`var E = TypeError; new E()`,
        // `Reflect.construct(RangeError, [msg])`). Mirrors the compile-lowered
        // `new TypeError(msg)` path. AggregateError takes the message as arg[1].
        if let Some(k) = self.error_ctors.iter().position(|&c| c == cv.heap_index()) {
            let msg = if k == 7 { args.get(1).copied() } else { args.first().copied() };
            return Ok(self.make_error(k as u8, msg));
        }
        // ArrayBuffer / DataView / TypedArray constructors used as values.
        let ci = cv.heap_index();
        if ci == self.arraybuffer_ctor && ci != 0 {
            return self.build_array_buffer(args);
        }
        if ci == self.dataview_ctor && ci != 0 {
            return self.build_data_view(args);
        }
        if let Some(k) = self.ta_ctors.iter().position(|&c| c == ci && ci != 0) {
            return self.build_typed_array(k as u8, args);
        }
        if ci == self.ta_base_ctor && ci != 0 {
            return Err(Thrown("TypeError: Abstract class TypedArray not directly constructable".into()));
        }
        if ci == self.proxy_ctor && ci != 0 {
            return self.make_proxy(
                args.first().copied().unwrap_or(Value::UNDEFINED),
                args.get(1).copied().unwrap_or(Value::UNDEFINED),
            );
        }
        if ci == self.duration_ctor && ci != 0 {
            return self.build_duration(args);
        }
        if ci == self.plaindate_ctor && ci != 0 {
            let y = self.to_number(args.first().copied().unwrap_or(Value::UNDEFINED))? as i64;
            let m = self.to_number(args.get(1).copied().unwrap_or(Value::UNDEFINED))? as i64;
            let d = self.to_number(args.get(2).copied().unwrap_or(Value::UNDEFINED))? as i64;
            return self.make_plain_date(y, m, d);
        }
        if ci == self.plaintime_ctor && ci != 0 {
            let mut f = [0i64; 6];
            for (i, slot) in f.iter_mut().enumerate() {
                let v = args.get(i).copied().unwrap_or(Value::UNDEFINED);
                if v != Value::UNDEFINED {
                    *slot = self.to_number(v)? as i64;
                }
            }
            return self.make_plain_time(f);
        }
        if ci == self.plaindatetime_ctor && ci != 0 {
            // year/month/day required (omitted → 0 → RangeError); time fields default 0.
            let mut f = [0i64; 9];
            for (i, slot) in f.iter_mut().enumerate() {
                let v = args.get(i).copied().unwrap_or(Value::UNDEFINED);
                if v != Value::UNDEFINED {
                    *slot = self.to_number(v)? as i64;
                }
            }
            return self.make_plain_date_time(f);
        }
        if ci == self.instant_ctor && ci != 0 {
            let ns = self.to_bigint(args.first().copied().unwrap_or(Value::UNDEFINED))?;
            return self.make_instant(ns);
        }
        if ci == self.plainyearmonth_ctor && ci != 0 {
            // (year, month, calendar?, referenceISODay=1)
            let y = self.to_number(args.first().copied().unwrap_or(Value::UNDEFINED))? as i64;
            let m = self.to_number(args.get(1).copied().unwrap_or(Value::UNDEFINED))? as i64;
            let rd = match args.get(3).copied() {
                Some(v) if v != Value::UNDEFINED => self.to_number(v)? as i64,
                _ => 1,
            };
            return self.make_plain_year_month(y, m, rd);
        }
        if ci == self.plainmonthday_ctor && ci != 0 {
            // (month, day, calendar?, referenceISOYear=1972)
            let m = self.to_number(args.first().copied().unwrap_or(Value::UNDEFINED))? as i64;
            let d = self.to_number(args.get(1).copied().unwrap_or(Value::UNDEFINED))? as i64;
            let ry = match args.get(3).copied() {
                Some(v) if v != Value::UNDEFINED => self.to_number(v)? as i64,
                _ => 1972,
            };
            return self.make_plain_month_day(m, d, ry);
        }
        // Constructing through a Proxy: `construct` trap (or construct the target).
        if let Some((target, handler, revoked)) = self.proxy_parts(ci) {
            if revoked {
                return Err(Thrown("TypeError: Cannot perform 'construct' on a revoked proxy".into()));
            }
            return match self.proxy_trap(handler, "construct")? {
                Some(trap) => {
                    let arr = Value::heap(self.heap.alloc(HeapObj::Array(args.to_vec())));
                    self.call_value(trap, handler, &[target, arr, cv])
                }
                None => self.construct(target, args),
            };
        }
        // Constructor FUNCTION (`new F()`, the pre-class OOP idiom): make an object
        // whose [[Prototype]] is `F.prototype` (so its methods + `constructor`
        // resolve), run `F` with `this` = that object, and use F's return value if
        // it returns an object (else the new object).
        if matches!(
            self.heap.get(cv.heap_index()),
            HeapObj::Func(_) | HeapObj::Closure { .. }
        ) {
            let proto = self.prototype_of(cv).unwrap_or(Value::UNDEFINED);
            let obj = Value::heap(self.heap.alloc(HeapObj::Object(ObjMap::new())));
            if proto.is_heap() {
                self.proto_of.insert(obj.heap_index(), proto);
            }
            let ret = self.call_value(cv, obj, args)?;
            if ret.is_heap()
                && matches!(self.heap.get(ret.heap_index()), HeapObj::Object(_) | HeapObj::Array(_))
            {
                return Ok(ret);
            }
            return Ok(obj);
        }
        let (ctor, has_explicit, parent) = match self.heap.get(cv.heap_index()) {
            HeapObj::Class(c) => (c.ctor, c.has_explicit_ctor, c.parent),
            _ => return Err(Thrown("TypeError: value is not a constructor".into())),
        };
        // The instance links to its class for method lookup + instanceof; its own
        // keys hold only the fields (so enumeration / JSON stay method-free).
        let mut map = ObjMap::new();
        map.class = Some(cv.heap_index());
        let obj = Value::heap(self.heap.alloc(HeapObj::Object(map)));
        if has_explicit {
            // The explicit constructor runs its own `super(...)`; a ctor that
            // returns an object/array replaces the instance.
            if let Some(fid) = ctor {
                let f = Value::heap(self.heap.alloc(HeapObj::Func(fid)));
                let ret = self.call_value(f, obj, args)?;
                if ret.is_heap()
                    && matches!(self.heap.get(ret.heap_index()), HeapObj::Object(_) | HeapObj::Array(_))
                {
                    return Ok(ret);
                }
            }
        } else {
            // No own constructor: run the parent's ctor (implicit `super(...args)`)
            // then this class's field initializers.
            if let Some(pidx) = parent {
                self.run_class_ctor(Value::heap(pidx), obj, args)?;
            }
            if let Some(fid) = ctor {
                let f = Value::heap(self.heap.alloc(HeapObj::Func(fid)));
                self.call_value(f, obj, &[])?;
            }
        }
        Ok(obj)
    }

    /// `v instanceof F` for a constructor FUNCTION `F`: true iff `F.prototype` is
    /// somewhere in `v`'s prototype chain.
    fn instanceof_via_proto(&mut self, v: Value, ctor: Value) -> bool {
        let target = match self.prototype_of(ctor) {
            Some(p) => p,
            None => return false,
        };
        let mut cur = self.object_get_prototype_of(v);
        for _ in 0..10_000 {
            if !cur.is_heap() {
                return false;
            }
            if cur == target {
                return true;
            }
            cur = self.object_get_prototype_of(cur);
        }
        false
    }

    /// True iff `v` is an object whose class chain includes the class at heap
    /// index `class_idx` (`v instanceof C`, walking `extends` links).
    fn instance_of_class(&self, v: Value, class_idx: u32) -> bool {
        if !v.is_heap() {
            return false;
        }
        let mut cur = match self.heap.get(v.heap_index()) {
            HeapObj::Object(m) => m.class,
            _ => None,
        };
        while let Some(cidx) = cur {
            if cidx == class_idx {
                return true;
            }
            cur = match self.heap.get(cidx) {
                HeapObj::Class(c) => c.parent,
                _ => None,
            };
        }
        false
    }

    /// The superclass value for a `super` reference inside a method of class
    /// `home_class_id`: that class's runtime `ClassData.parent` (linked by
    /// MakeClass from the evaluated `extends` expression), or None.
    fn super_parent(&self, home_class_id: u32) -> Option<Value> {
        let home = (*self.class_values.get(home_class_id as usize)?)?;
        match self.heap.get(home.heap_index()) {
            HeapObj::Class(c) => c.parent.map(Value::heap),
            _ => None,
        }
    }

    /// Run a class's constructor contribution on an existing instance `obj` —
    /// for `super(...)` and the implicit-super chain. An explicit ctor runs its
    /// own `super`; an implicit one runs the parent chain then its fields.
    fn run_class_ctor(&mut self, cval: Value, obj: Value, args: &[Value]) -> Result<(), Thrown> {
        if !cval.is_heap() {
            return Ok(());
        }
        let (ctor, has_explicit, parent) = match self.heap.get(cval.heap_index()) {
            HeapObj::Class(c) => (c.ctor, c.has_explicit_ctor, c.parent),
            _ => return Ok(()),
        };
        if has_explicit {
            if let Some(fid) = ctor {
                let f = Value::heap(self.heap.alloc(HeapObj::Func(fid)));
                self.call_value(f, obj, args)?;
            }
        } else {
            if let Some(pidx) = parent {
                self.run_class_ctor(Value::heap(pidx), obj, args)?;
            }
            if let Some(fid) = ctor {
                let f = Value::heap(self.heap.alloc(HeapObj::Func(fid)));
                self.call_value(f, obj, &[])?;
            }
        }
        Ok(())
    }

    /// `Object.assign(target, ...sources)`: copy each source's own enumerable
    /// keys (object keys, or an array's index strings) onto `target`; returns
    /// `target`. Primitive (incl. null/undefined) sources contribute nothing.
    fn object_assign(&mut self, args: &[Value]) -> Result<Value, Thrown> {
        let target = args.first().copied().unwrap_or(Value::UNDEFINED);
        if !target.is_heap() || !matches!(self.heap.get(target.heap_index()), HeapObj::Object(_)) {
            return Err(Thrown("TypeError: Object.assign target must be an object".into()));
        }
        let tidx = target.heap_index();
        let mut added_any = false;
        for &src in &args[1..] {
            if !src.is_heap() {
                continue;
            }
            // Gather (key, val) pairs under the immutable borrow, then write.
            // (A string source spreads as index→char, like an array.)
            let str_chars: Option<Vec<char>> = match self.heap.get(src.heap_index()) {
                HeapObj::Str(_) | HeapObj::Cons { .. } => {
                    Some(self.heap.str_cow(src.heap_index()).unwrap().chars().collect())
                }
                _ => None,
            };
            let pairs: Vec<(String, Value)> = if let Some(chars) = str_chars {
                chars
                    .into_iter()
                    .enumerate()
                    .map(|(i, c)| (i.to_string(), self.alloc_str(c.to_string())))
                    .collect()
            } else {
                match self.heap.get(src.heap_index()) {
                    HeapObj::Object(map) => {
                        map.keys.iter().cloned().zip(map.vals.iter().copied()).collect()
                    }
                    HeapObj::Array(items) => {
                        items.iter().enumerate().map(|(i, &v)| (i.to_string(), v)).collect()
                    }
                    _ => Vec::new(),
                }
            };
            for (k, v) in pairs {
                if let HeapObj::Object(map) = self.heap.get_mut(tidx) {
                    added_any |= map.set(&k, v);
                }
            }
        }
        if added_any {
            self.heap.bump_version(tidx);
        }
        Ok(target)
    }

    /// `Array.from(src[, mapFn])`: build an array from an array, a string's
    /// chars, or an array-like (`{length, 0:…}`), applying `mapFn(value, index)`
    /// when it is a function.
    /// Materialize a value's iteration elements: an array or set → its items, a
    /// string → its chars (as 1-char strings), a map → fresh `[key, value]` entry
    /// arrays. Throws a TypeError for a non-iterable. Allocations happen after the
    /// heap borrow is released (two phases).
    /// Whether `v` is a user-callable value (function or closure).
    fn is_callable(&self, v: Value) -> bool {
        v.is_heap()
            && matches!(
                self.heap.get(v.heap_index()),
                HeapObj::Func(_) | HeapObj::Closure { .. } | HeapObj::Bound { .. } | HeapObj::Native(_)
            )
    }

    /// `obj.hasOwnProperty(key)` — own data/accessor property, array index/length,
    /// or string index/length.
    fn has_own_property(&self, obj: Value, key: &str) -> bool {
        if !obj.is_heap() || is_private_key(key) {
            return false; // private names aren't reflectable own properties
        }
        match self.heap.get(obj.heap_index()) {
            HeapObj::Object(m) => {
                m.pos(key).is_some()
                    // globalThis own properties are the reserved global bindings.
                    || (obj.heap_index() == self.global_this
                        && self.global_this != 0
                        && self.global_by_name(key).is_some())
            }
            HeapObj::Array(items) => {
                key == "length" || key.parse::<usize>().map_or(false, |i| i < items.len())
            }
            HeapObj::Str(s) => {
                key == "length" || key.parse::<usize>().map_or(false, |i| i < s.char_len)
            }
            HeapObj::Cons { len, .. } => {
                key == "length" || key.parse::<usize>().map_or(false, |i| i < *len)
            }
            // A class value: own statics (data + `static get`/`set`) + name/length.
            HeapObj::Class(c) => {
                c.statics.pos(key).is_some()
                    || c.static_getters.iter().any(|(n, _)| n == key)
                    || c.static_setters.iter().any(|(n, _)| n == key)
                    || self.callable_has_intrinsic(obj, key)
            }
            // Functions/closures/etc.: assigned own props (`fn.x`) + name/length.
            _ => {
                self.fn_props.get(&obj.heap_index()).map_or(false, |m| m.pos(key).is_some())
                    || self.callable_has_intrinsic(obj, key)
            }
        }
    }

    /// `obj.propertyIsEnumerable(key)` — true if `key` is an own enumerable
    /// property. Array indices are enumerable; `length` is not.
    fn own_is_enumerable(&self, obj: Value, key: &str) -> bool {
        if !obj.is_heap() || is_private_key(key) {
            return false;
        }
        match self.heap.get(obj.heap_index()) {
            HeapObj::Object(m) => m.pos(key).map_or(false, |i| m.attrs[i].enumerable),
            HeapObj::Array(items) => key.parse::<usize>().map_or(false, |i| i < items.len()),
            _ => false,
        }
    }

    /// `proto.isPrototypeOf(obj)` — is `proto` anywhere in `obj`'s prototype chain?
    fn is_prototype_of(&mut self, proto: Value, obj: Value) -> bool {
        let mut cur = self.object_get_prototype_of(obj);
        for _ in 0..10_000 {
            if !cur.is_heap() {
                return false;
            }
            if cur == proto {
                return true;
            }
            cur = self.object_get_prototype_of(cur);
        }
        false
    }

    /// Resolve an iterable's iterator: a plain object with a `@@iterator` method
    /// (a custom iterable) yields `obj[@@iterator]()`; everything else (arrays,
    /// strings, Map/Set, generators) iterates directly and passes through.
    fn get_iterator(&mut self, v: Value) -> Result<Value, Thrown> {
        if v.is_heap() && matches!(self.heap.get(v.heap_index()), HeapObj::Object(_)) {
            let m = self.get_prop(v, "@@iterator")?;
            if self.is_callable(m) {
                return self.call_value(m, v, &[]);
            }
        }
        Ok(v)
    }

    /// `for await`: resolve the ASYNC iterator. An async generator is its own
    /// iterator; a plain object uses `@@asyncIterator` (an async iterable) or, as
    /// the spec's async-from-sync fallback, `@@iterator`; everything else (arrays,
    /// strings, Map/Set, sync generators) passes through (ForAwaitNext drives it).
    fn get_async_iterator(&mut self, v: Value) -> Result<Value, Thrown> {
        if v.is_heap() && matches!(self.heap.get(v.heap_index()), HeapObj::Object(_)) {
            let am = self.get_prop(v, "@@asyncIterator")?;
            if self.is_callable(am) {
                return self.call_value(am, v, &[]);
            }
            let sm = self.get_prop(v, "@@iterator")?;
            if self.is_callable(sm) {
                return self.call_value(sm, v, &[]);
            }
        }
        Ok(v)
    }

    /// Normalize a destructuring source to a positionally-indexable value: a
    /// generator or a custom iterable (object with `@@iterator`) is drained into a
    /// fresh array — LAZILY, at most `max` elements (so `let [a,b] = infinite`
    /// pulls 2, not forever); everything else (arrays/strings/Map/Set, or a
    /// non-iterable) passes through unchanged.
    fn iter_to_array(&mut self, v: Value, max: u32) -> Result<Value, Thrown> {
        if !v.is_heap() {
            return Ok(v);
        }
        let drain = match self.heap.get(v.heap_index()) {
            HeapObj::Generator { .. } => true,
            HeapObj::Object(_) => {
                let it = self.get_prop(v, "@@iterator")?;
                self.is_callable(it)
            }
            _ => false,
        };
        if !drain {
            return Ok(v);
        }
        let iter = self.get_iterator(v)?; // generator → itself; iterable → its iterator
        let lim = max as usize;
        let mut out = Vec::new();
        while out.len() < lim {
            let res = if matches!(self.heap.get(iter.heap_index()), HeapObj::Generator { .. }) {
                self.generator_method(iter.heap_index(), "next", &[])?
                    .unwrap_or(Value::UNDEFINED)
            } else {
                let next = self.get_prop(iter, "next")?;
                if !self.is_callable(next) {
                    break;
                }
                self.call_value(next, iter, &[])?
            };
            let done = self.get_prop(res, "done")?;
            if self.truthy(done) {
                break;
            }
            out.push(self.get_prop(res, "value")?);
        }
        Ok(Value::heap(self.heap.alloc(HeapObj::Array(out))))
    }

    fn iterate_to_vec(&mut self, v: Value) -> Result<Vec<Value>, Thrown> {
        // A TypedArray iterates positionally over its elements.
        if let Some(ta) = self.as_typed_array(v) {
            let n = match self.heap.get(ta) {
                HeapObj::TypedArray { length, .. } => *length,
                _ => 0,
            };
            return Ok((0..n).map(|i| self.ta_element_get(ta, i)).collect());
        }
        let v = self.get_iterator(v)?;
        // A generator is drained eagerly via repeated next() (spread / Array.from
        // produce a buffer; an infinite generator hangs here, matching V8).
        if v.is_heap() && matches!(self.heap.get(v.heap_index()), HeapObj::Generator { .. }) {
            let gidx = v.heap_index();
            let mut out = Vec::new();
            loop {
                let res = self
                    .generator_method(gidx, "next", &[])?
                    .unwrap_or(Value::UNDEFINED);
                let done = self.get_prop(res, "done")?;
                if self.truthy(done) {
                    break;
                }
                out.push(self.get_prop(res, "value")?);
            }
            return Ok(out);
        }
        // A user iterator object (one with a `next()` method) or a built-in
        // Iterator: drain it.
        if v.is_heap() && matches!(self.heap.get(v.heap_index()), HeapObj::Object(_) | HeapObj::Iterator { .. }) {
            let next = self.get_prop(v, "next")?;
            if self.is_callable(next) {
                let mut out = Vec::new();
                loop {
                    let res = self.call_value(next, v, &[])?;
                    let done = self.get_prop(res, "done")?;
                    if self.truthy(done) {
                        break;
                    }
                    out.push(self.get_prop(res, "value")?);
                }
                return Ok(out);
            }
        }
        enum Plan {
            Vals(Vec<Value>),
            Chars(Vec<char>),
            Pairs(Vec<(Value, Value)>),
        }
        let plan = if v.is_heap() {
            match self.heap.get(v.heap_index()) {
                HeapObj::Array(items) => Plan::Vals(items.clone()),
                HeapObj::Set(items) => Plan::Vals(items.clone()),
                HeapObj::Str(_) | HeapObj::Cons { .. } => {
                    Plan::Chars(self.heap.str_cow(v.heap_index()).unwrap().chars().collect())
                }
                HeapObj::Map { keys, vals } => {
                    Plan::Pairs(keys.iter().copied().zip(vals.iter().copied()).collect())
                }
                _ => return Err(Thrown(format!("TypeError: {} is not iterable", self.display(v)))),
            }
        } else {
            return Err(Thrown(format!("TypeError: {} is not iterable", self.display(v))));
        };
        Ok(match plan {
            Plan::Vals(v) => v,
            Plan::Chars(cs) => cs.into_iter().map(|c| self.alloc_str(c.to_string())).collect(),
            Plan::Pairs(ps) => ps
                .into_iter()
                .map(|(k, v)| Value::heap(self.heap.alloc(HeapObj::Array(vec![k, v]))))
                .collect(),
        })
    }

    fn array_from(&mut self, src: Value, mapfn: Value) -> Result<Value, Thrown> {
        // Classify the source under a short-lived borrow, then materialize its
        // elements (the object/array-like path needs &mut self for get_prop).
        enum Kind {
            Iterable,
            Obj,
            Other,
        }
        let mut elems: Vec<Value> = Vec::new();
        let kind = if src.is_heap() {
            match self.heap.get(src.heap_index()) {
                HeapObj::Array(_)
                | HeapObj::Str(_)
                | HeapObj::Cons { .. }
                | HeapObj::Set(_)
                | HeapObj::Map { .. }
                | HeapObj::TypedArray { .. }
                | HeapObj::Generator { .. } => Kind::Iterable,
                HeapObj::Object(_) => Kind::Obj,
                _ => Kind::Other,
            }
        } else {
            Kind::Other
        };
        match kind {
            Kind::Iterable => elems = self.iterate_to_vec(src)?,
            Kind::Obj => {
                // A custom iterable object (`@@iterator`) → iterate it; otherwise
                // treat it as array-like (read `length`, then indices 0..length).
                let it = self.get_prop(src, "@@iterator")?;
                if self.is_callable(it) {
                    elems = self.iterate_to_vec(src)?;
                } else {
                    let len = self.get_prop(src, "length")?;
                    let n = if len.is_number() && len.as_f64() >= 0.0 {
                        len.as_f64() as usize
                    } else {
                        0
                    };
                    for i in 0..n {
                        elems.push(self.get_index(src, Value::int(i as i32))?);
                    }
                }
            }
            Kind::Other => {}
        }
        // Apply the map callback, if given.
        let has_map = mapfn.is_heap()
            && matches!(
                self.heap.get(mapfn.heap_index()),
                HeapObj::Func(_) | HeapObj::Closure { .. }
            );
        if has_map {
            for (i, slot) in elems.iter_mut().enumerate() {
                let args = [*slot, Value::int(i as i32)];
                *slot = self.call_value(mapfn, Value::UNDEFINED, &args)?;
            }
        }
        Ok(Value::heap(self.heap.alloc(HeapObj::Array(elems))))
    }

    /// If `idx` is an Error-like object — an object whose `name` is one of the
    /// engine's error kinds — return that name, else `None`.
    fn error_name(&self, idx: u32) -> Option<String> {
        let map = match self.heap.get(idx) {
            HeapObj::Object(m) => m,
            _ => return None,
        };
        let nv = map.get("name")?;
        let name = self.display(nv);
        native::ERROR_NAMES.contains(&name.as_str()).then_some(name)
    }

    /// Whether `idx`'s prototype chain reaches one of the error prototypes — i.e.
    /// it's a real error instance (created via `new TypeError` or an internal
    /// throw), as opposed to a plain object that merely has a `name` property.
    fn is_error_instance(&self, idx: u32) -> bool {
        if self.error_protos[0] == 0 {
            return false;
        }
        let mut cur = idx;
        for _ in 0..64 {
            match self.proto_of.get(&cur) {
                Some(p) if p.is_heap() => {
                    let pi = p.heap_index();
                    if self.error_protos.contains(&pi) {
                        return true;
                    }
                    cur = pi;
                }
                _ => return false,
            }
        }
        false
    }

    /// Read a DATA property from `idx` walking the `proto_of` chain (no getters,
    /// no class methods) — used by the read-only `display`/ToString path for error
    /// instances, where `name`/`message` may be inherited from the prototype.
    fn read_data_prop(&self, idx: u32, key: &str) -> Option<Value> {
        let mut cur = idx;
        for _ in 0..64 {
            if let HeapObj::Object(m) = self.heap.get(cur) {
                if let Some(v) = m.get(key) {
                    return Some(v);
                }
            }
            match self.proto_of.get(&cur) {
                Some(p) if p.is_heap() => cur = p.heap_index(),
                _ => return None,
            }
        }
        None
    }

    /// `Error.prototype.toString` semantics for the read-only `display` path:
    /// "name: message", dropping the separator when either part is empty.
    fn error_display_string(&self, idx: u32) -> String {
        let name =
            self.read_data_prop(idx, "name").map(|v| self.display(v)).unwrap_or_else(|| "Error".into());
        let msg = self.read_data_prop(idx, "message").map(|v| self.display(v)).unwrap_or_default();
        if name.is_empty() {
            msg
        } else if msg.is_empty() {
            name
        } else {
            format!("{name}: {msg}")
        }
    }

    /// Methods on a number receiver: `toFixed`, `toString`. Returns `Ok(None)`
    /// for an unrecognised name (the caller then treats it as a missing property
    /// → TypeError, matching JS).
    fn number_method(&mut self, recv: Value, name: &str, args: &[Value]) -> Result<Option<Value>, Thrown> {
        let n = recv.as_f64();
        match name {
            "toFixed" => {
                let d = args.first().map(|a| a.as_f64()).unwrap_or(0.0);
                if !d.is_finite() || d < 0.0 || d > 100.0 {
                    return Err(Thrown(
                        "RangeError: toFixed() digits argument must be between 0 and 100".into(),
                    ));
                }
                Ok(Some(self.alloc_str(to_fixed(n, d as usize))))
            }
            "toString" => {
                // An absent/undefined radix defaults to 10; otherwise it must be 2..36.
                let arg = args.first().copied().unwrap_or(Value::UNDEFINED);
                if arg == Value::UNDEFINED {
                    return Ok(Some(self.alloc_str(self.display(recv))));
                }
                let rf = arg.as_f64();
                let r = if rf.is_nan() { 0i64 } else { rf.trunc() as i64 };
                if !(2..=36).contains(&r) {
                    return Err(Thrown(
                        "RangeError: toString() radix must be between 2 and 36".into(),
                    ));
                }
                if r == 10 {
                    Ok(Some(self.alloc_str(self.display(recv))))
                } else {
                    Ok(Some(self.alloc_str(num_to_radix(n, r as u32))))
                }
            }
            "valueOf" => Ok(Some(recv)),
            // No Intl: toLocaleString() behaves like the default base-10 toString().
            "toLocaleString" => Ok(Some(self.alloc_str(self.display(recv)))),
            _ => Ok(None),
        }
    }

    /// `Boolean.prototype.toString`/`valueOf` on a boolean value.
    fn boolean_method(&mut self, recv: Value, name: &str) -> Value {
        match name {
            "toString" => self.alloc_str(if recv == Value::bool(true) { "true" } else { "false" }.to_string()),
            "valueOf" => recv,
            _ => Value::UNDEFINED,
        }
    }

    /// Resolve `cb` to the native entry of a COMPILED, non-capturing JIT function
    /// for the array-builtin fast path (`map`/`filter`/`forEach`/`reduce`).
    /// Returns `(entry, callee_reg_count, param_count)` or `None` if `cb` must go
    /// through the interpreter `call_value` (not a plain function, a capturing
    /// closure, JIT disabled, inside a deopted self-call continuation, or not
    /// JIT-compilable). Compiles `cb` on first use if eligible — array builtins
    /// call the same callback many times, so we don't wait for the call-count
    /// threshold; an ineligible proto is blacklisted by `compile` and returns
    /// `None` cheaply thereafter.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    fn native_cb_entry(&mut self, cb: Value) -> Option<(*const u8, usize, usize)> {
        // Mirror the interpreter's JIT-entry guard: respect ZIPP_NOJIT and never
        // enter native code from a deopted self-call continuation (livelock).
        if !self.jit_enabled || self.jit_recurse_depth != 0 || !cb.is_heap() {
            return None;
        }
        let (fid, ups) = self.heap.as_callable(cb.heap_index())?;
        // A capturing closure reads upvalue cells (heap) — outside the leaf-int JIT.
        if !ups.is_empty() {
            return None;
        }
        if self.jit.get(fid).is_none() {
            let proto: *const crate::bytecode::FuncProto =
                &self.program.functions[fid as usize];
            // SAFETY: program functions are immutable during execution; the raw
            // ptr dodges the self.jit (&mut) vs self.program (&) borrow conflict.
            let proto_ref = unsafe { &*proto };
            let self_val = proto_ref
                .name_global
                .and_then(|s| self.globals.get(s as usize).copied())
                .unwrap_or(Value::UNDEFINED)
                .bits();
            self.jit.compile(fid, proto_ref, jit_self_call_at as usize, self_val);
        }
        let entry = self.jit.get(fid)?.entry();
        let proto = &self.program.functions[fid as usize];
        Some((entry, (proto.reg_count as usize).max(1), proto.param_count as usize))
    }

    #[cfg(not(all(feature = "jit", target_arch = "x86_64")))]
    fn native_cb_entry(&mut self, _cb: Value) -> Option<(*const u8, usize, usize)> {
        None
    }

    /// Invoke a compiled callback natively over the reused window at `win`
    /// (`regs[win..win+callee_regs]`), writing `this`=undefined + the first
    /// `param_count` args. On a native deopt (bail), re-runs the element through
    /// the interpreter `call_value` — which nests its frame ABOVE this window
    /// (base = `regs.len()`) and pops back, leaving the window intact for the
    /// next element. This is the fast path that skips the per-element frame push
    /// + `run_loop` re-entry + callee re-resolution that `call_value` incurs.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    fn invoke_cb_windowed(
        &mut self,
        entry: *const u8,
        win: usize,
        param_count: usize,
        cb: Value,
        args: &[Value],
    ) -> Result<Value, Thrown> {
        self.regs[win] = Value::UNDEFINED; // reg 0 = this
        let n = args.len().min(param_count);
        for i in 0..n {
            self.regs[win + 1 + i] = args[i];
        }
        let regs_ptr = unsafe { self.regs.as_mut_ptr().add(win) } as *mut u64;
        let vm_ptr = self as *mut Vm as *mut core::ffi::c_void;
        // SAFETY: `entry` is a valid compiled win64 fn (regs, bail_out, vm)->bits
        // (from JitFn::entry); the window has callee_regs ≥ param_count+1 valid
        // slots; `vm_ptr` is valid for the call. A self-recursive callee routes
        // through `jit_self_call` which is capacity-pinned (no regs realloc).
        let f: extern "win64" fn(*mut u64, *mut u32, *mut core::ffi::c_void) -> u64 =
            unsafe { core::mem::transmute(entry) };
        let mut bail: u32 = crate::codegen::NO_BAIL;
        let bits = f(regs_ptr, &mut bail as *mut u32, vm_ptr);
        if bail == crate::codegen::NO_BAIL {
            return Ok(Value::from_bits(bits));
        }
        // A deopt that left `pending_throw` set means a native self-recursive
        // callee already THREW (e.g. a recursive callback hit the RangeError
        // frame cap) — UNWIND with that exception. Re-running via call_value
        // would execute the callback a second time and propagate a stale thrown
        // value. Mirrors the try_run_jit ip==0 bail handling.
        if self.pending_throw.is_some() {
            return Err(Thrown(String::new()));
        }
        // Plain deopt (non-int operand / overflow): re-run this element on the
        // interpreter, which nests its frame above the reused window.
        self.call_value(cb, Value::UNDEFINED, args)
    }

    /// One per-element callback invocation: native fast path when `native` is
    /// set, else the interpreter `call_value`.
    #[inline]
    fn run_cb_elem(
        &mut self,
        native: Option<(*const u8, usize, usize)>,
        win: usize,
        cb: Value,
        args: &[Value],
    ) -> Result<Value, Thrown> {
        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
        if let Some((entry, _callee_regs, param_count)) = native {
            return self.invoke_cb_windowed(entry, win, param_count, cb, args);
        }
        let _ = (native, win);
        self.call_value(cb, Value::UNDEFINED, args)
    }

    /// Shared driver for `map`/`filter`/`forEach` (callback args = [element,
    /// index]). Uses the native callback fast path when the callback is a
    /// compiled non-capturing function: a single reused register window, a direct
    /// native call per element. Falls back to `call_value` per element otherwise.
    /// The window is always released (truncate) before returning — including on a
    /// callback error — so a thrown callback never leaks register slots.
    fn array_each(&mut self, idx: u32, cb: Value, mode: EachMode) -> Result<Option<Value>, Thrown> {
        let snapshot = self.array_snapshot(idx);
        let collect = matches!(mode, EachMode::Map | EachMode::Filter);
        let mut out: Vec<Value> =
            if collect { Vec::with_capacity(snapshot.len()) } else { Vec::new() };

        // Fused native map kernel: inline the callback into a native loop over
        // the snapshot for the leading run of integer elements — eliminating the
        // per-element call boundary (the gap to V8, which inlines callbacks). Map
        // only (dense, ordered store). On a type-guard bail the kernel returns
        // the index it reached, having written results `[0, start)`; the
        // per-element loop below finishes `[start, len)` correctly (handling
        // doubles/strings/etc.), so a mixed array can never give a wrong answer.
        let mut start = 0usize;
        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
        if matches!(mode, EachMode::Map)
            && self.jit_enabled
            && self.jit_recurse_depth == 0
            && cb.is_heap()
            && snapshot.len() <= i32::MAX as usize
        {
            if let Some((fid, ups)) = self.heap.as_callable(cb.heap_index()) {
                if ups.is_empty() {
                    let proto: *const crate::bytecode::FuncProto =
                        &self.program.functions[fid as usize];
                    // SAFETY: program functions are immutable during execution;
                    // the raw ptr dodges the self.jit (&mut) vs self.program (&)
                    // borrow conflict (same pattern as native_cb_entry).
                    let proto_ref = unsafe { &*proto };
                    let min_window = if proto_ref.param_count >= 2 { 3 } else { 2 };
                    let reg_count = (proto_ref.reg_count as usize).max(min_window);
                    if let Some(entry) = self.jit.map_kernel(fid, proto_ref) {
                        let win = self.regs.len();
                        if !self.regs_would_overflow(win + reg_count) {
                            self.regs.resize(win + reg_count, Value::UNDEFINED);
                            let len = snapshot.len();
                            let window_ptr =
                                unsafe { self.regs.as_mut_ptr().add(win) } as *mut u64;
                            let snap_ptr = snapshot.as_ptr() as *const u64;
                            let out_ptr = out.as_mut_ptr() as *mut u64;
                            // SAFETY: `entry` is a valid win64 map kernel; the
                            // window holds `reg_count` slots; `out` has capacity
                            // `len` ≥ the returned count; the kernel is call-free
                            // so none of these pointers move during the call.
                            let kernel: extern "win64" fn(
                                *mut u64,
                                *const u64,
                                usize,
                                *mut u64,
                            ) -> usize = unsafe { core::mem::transmute(entry) };
                            let processed = kernel(window_ptr, snap_ptr, len, out_ptr);
                            // The kernel wrote `out[0..processed]` densely.
                            unsafe { out.set_len(processed) };
                            self.regs.truncate(win);
                            start = processed;
                        }
                    }
                }
            }
        }

        // Fused native filter kernel: inline the predicate over the snapshot for
        // the leading numeric run, compacting kept elements into `out`. The
        // predicate result must be a Bool (a comparison); a non-Bool result bails
        // that element to the per-element tail (which evaluates JS truthiness).
        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
        if matches!(mode, EachMode::Filter)
            && self.jit_enabled
            && self.jit_recurse_depth == 0
            && cb.is_heap()
            && snapshot.len() <= i32::MAX as usize
        {
            if let Some((fid, ups)) = self.heap.as_callable(cb.heap_index()) {
                if ups.is_empty() {
                    let proto: *const crate::bytecode::FuncProto =
                        &self.program.functions[fid as usize];
                    // SAFETY: as the map branch above.
                    let proto_ref = unsafe { &*proto };
                    let min_window = if proto_ref.param_count >= 2 { 3 } else { 2 };
                    let reg_count = (proto_ref.reg_count as usize).max(min_window);
                    if let Some(entry) = self.jit.filter_kernel(fid, proto_ref) {
                        let win = self.regs.len();
                        if !self.regs_would_overflow(win + reg_count) {
                            self.regs.resize(win + reg_count, Value::UNDEFINED);
                            let len = snapshot.len();
                            let window_ptr =
                                unsafe { self.regs.as_mut_ptr().add(win) } as *mut u64;
                            let snap_ptr = snapshot.as_ptr() as *const u64;
                            let out_ptr = out.as_mut_ptr() as *mut u64;
                            let mut kept: usize = 0;
                            // SAFETY: valid win64 filter kernel; window has
                            // reg_count slots; `out` capacity `len` ≥ kept; the
                            // kernel is call-free so the pointers don't move.
                            let kernel: extern "win64" fn(
                                *mut u64,
                                *const u64,
                                usize,
                                *mut u64,
                                *mut usize,
                            ) -> usize = unsafe { core::mem::transmute(entry) };
                            let scanned =
                                kernel(window_ptr, snap_ptr, len, out_ptr, &mut kept as *mut usize);
                            // The kernel wrote `kept` elements into `out[0..kept]`.
                            unsafe { out.set_len(kept) };
                            self.regs.truncate(win);
                            start = scanned;
                        }
                    }
                }
            }
        }

        // Per-element path for `[start, len)` — the whole array when no kernel
        // ran, or just the tail after a kernel bail (or nothing if it completed).
        let run_tail = start < snapshot.len();
        let mut native = if run_tail { self.native_cb_entry(cb) } else { None };
        let win = self.regs.len();
        if let Some((_, callee_regs, _)) = native {
            if self.regs_would_overflow(win + callee_regs) {
                native = None; // can't fit a window → interpreter path
            } else {
                self.regs.resize(win + callee_regs, Value::UNDEFINED);
            }
        }

        let mut err = None;
        for i in start..snapshot.len() {
            let v = snapshot[i];
            let args = [v, Value::int(i as i32)];
            match self.run_cb_elem(native, win, cb, &args) {
                Ok(r) => match mode {
                    EachMode::Map => out.push(r),
                    EachMode::Filter => {
                        if self.truthy(r) {
                            out.push(v);
                        }
                    }
                    EachMode::ForEach => {}
                },
                Err(e) => {
                    err = Some(e);
                    break;
                }
            }
        }
        if native.is_some() {
            self.regs.truncate(win); // release the reused window (success or error)
        }
        if let Some(e) = err {
            return Err(e);
        }
        match mode {
            EachMode::ForEach => Ok(Some(Value::UNDEFINED)),
            _ => Ok(Some(Value::heap(self.heap.alloc(HeapObj::Array(out))))),
        }
    }

    /// Allocate a built-in iterator over a snapshot of `items` with prototype `proto`.
    fn make_iterator(&mut self, items: Vec<Value>, proto: u32) -> Value {
        Value::heap(self.heap.alloc(HeapObj::Iterator { items, index: 0, proto }))
    }

    fn array_method(&mut self, idx: u32, name: &str, args: &[Value]) -> Result<Option<Value>, Thrown> {
        let arg0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        // Generic array methods accept an array-like `this`
        // (`Array.prototype.map.call({length:2, 0:'a', 1:'b'}, cb)`, or on a string).
        // For a non-array receiver, snapshot its `length` + indexed elements into a
        // temp array and run the (read-only) method against that. Mutating methods
        // still require a real array (they fall through to their HeapObj::Array arms).
        if !matches!(self.heap.get(idx), HeapObj::Array(_))
            && matches!(
                name,
                "map" | "filter" | "forEach" | "every" | "some" | "reduce" | "reduceRight"
                    | "find" | "findIndex" | "findLast" | "findLastIndex" | "indexOf"
                    | "lastIndexOf" | "includes" | "join" | "toString" | "slice" | "at"
                    | "concat" | "flat" | "flatMap" | "with" | "toReversed" | "toSorted"
                    | "toSpliced" | "entries" | "keys" | "values" | "toLocaleString"
            )
        {
            let elems = self.array_like_read(idx);
            let tmp = self.heap.alloc(HeapObj::Array(elems));
            return self.array_method(tmp, name, args);
        }
        match name {
            "push" => {
                let mut last = Value::UNDEFINED;
                if let HeapObj::Array(items) = self.heap.get_mut(idx) {
                    for a in args {
                        items.push(*a);
                    }
                    last = Value::int(items.len() as i32);
                }
                Ok(Some(last))
            }
            "pop" => {
                if let HeapObj::Array(items) = self.heap.get_mut(idx) {
                    return Ok(Some(items.pop().unwrap_or(Value::UNDEFINED)));
                }
                Ok(Some(Value::UNDEFINED))
            }
            "shift" => {
                if let HeapObj::Array(items) = self.heap.get_mut(idx) {
                    if items.is_empty() {
                        return Ok(Some(Value::UNDEFINED));
                    }
                    return Ok(Some(items.remove(0)));
                }
                Ok(Some(Value::UNDEFINED))
            }
            // `Array.prototype.toString()` is `join()` with the default "," sep.
            "join" | "toString" => {
                let sep = if name == "toString" || args.is_empty() {
                    ",".to_string()
                } else {
                    self.display(arg0)
                };
                let snapshot = self.array_snapshot(idx);
                let parts: Vec<String> = snapshot
                    .iter()
                    .map(|v| if v.is_nullish() { String::new() } else { self.display(*v) })
                    .collect();
                Ok(Some(self.alloc_str(parts.join(&sep))))
            }
            "at" => {
                // Negative index counts from the end; out of range → undefined.
                let len = match self.heap.get(idx) {
                    HeapObj::Array(items) => items.len(),
                    _ => 0,
                };
                let i = arg0.is_number().then(|| arg0.as_f64()).unwrap_or(0.0) as i64;
                let abs = if i < 0 { i + len as i64 } else { i };
                let v = if abs >= 0 && (abs as usize) < len {
                    match self.heap.get(idx) {
                        HeapObj::Array(items) => items[abs as usize],
                        _ => Value::UNDEFINED,
                    }
                } else {
                    Value::UNDEFINED
                };
                Ok(Some(v))
            }
            "indexOf" => {
                let snapshot = self.array_snapshot(idx);
                let pos = snapshot.iter().position(|v| self.values_strict_eq(*v, arg0));
                Ok(Some(Value::int(pos.map(|p| p as i32).unwrap_or(-1))))
            }
            "includes" => {
                let snapshot = self.array_snapshot(idx);
                let found = snapshot.iter().any(|v| self.values_strict_eq(*v, arg0));
                Ok(Some(Value::bool(found)))
            }
            "lastIndexOf" => {
                let snapshot = self.array_snapshot(idx);
                let pos = snapshot.iter().rposition(|v| self.values_strict_eq(*v, arg0));
                Ok(Some(Value::int(pos.map(|p| p as i32).unwrap_or(-1))))
            }
            "reverse" => {
                if let HeapObj::Array(items) = self.heap.get_mut(idx) {
                    items.reverse();
                }
                Ok(Some(Value::heap(idx))) // reverses in place, returns the array
            }
            "concat" => {
                // New array = this ++ each arg, spreading array args one level.
                let mut out = self.array_snapshot(idx);
                for a in args {
                    if a.is_heap() && matches!(self.heap.get(a.heap_index()), HeapObj::Array(_)) {
                        out.extend(self.array_snapshot(a.heap_index()));
                    } else {
                        out.push(*a);
                    }
                }
                Ok(Some(Value::heap(self.heap.alloc(HeapObj::Array(out)))))
            }
            "flat" => {
                let depth = if args.is_empty() {
                    1
                } else {
                    let d = arg0.as_f64();
                    if d.is_infinite() && d > 0.0 {
                        i32::MAX
                    } else if d.is_finite() && d >= 0.0 {
                        d as i32
                    } else {
                        0
                    }
                };
                let snapshot = self.array_snapshot(idx);
                let out = self.flatten_array(&snapshot, depth);
                Ok(Some(Value::heap(self.heap.alloc(HeapObj::Array(out)))))
            }
            "fill" => {
                let val = arg0;
                let len = match self.heap.get(idx) {
                    HeapObj::Array(items) => items.len() as i32,
                    _ => 0,
                };
                let start = norm_index(if args.len() >= 2 { args[1].as_f64() as i32 } else { 0 }, len);
                let end = norm_index(if args.len() >= 3 { args[2].as_f64() as i32 } else { len }, len);
                if let HeapObj::Array(items) = self.heap.get_mut(idx) {
                    for i in start..end {
                        items[i as usize] = val;
                    }
                }
                Ok(Some(Value::heap(idx)))
            }
            "slice" => {
                let snapshot = self.array_snapshot(idx);
                let len = snapshot.len() as i32;
                let start = norm_index(if args.is_empty() { 0 } else { arg0.as_f64() as i32 }, len);
                let end = if args.len() < 2 {
                    len
                } else {
                    norm_index(args[1].as_f64() as i32, len)
                };
                let slice: Vec<Value> = if start < end {
                    snapshot[start as usize..end as usize].to_vec()
                } else {
                    Vec::new()
                };
                Ok(Some(Value::heap(self.heap.alloc(HeapObj::Array(slice)))))
            }
            "map" => self.array_each(idx, arg0, EachMode::Map),
            "filter" => self.array_each(idx, arg0, EachMode::Filter),
            "forEach" => self.array_each(idx, arg0, EachMode::ForEach),
            // Short-circuiting callback searches. They stop at the first match, so
            // they use call_value directly (the all-elements array_each driver
            // doesn't fit); the callback receives (element, index).
            "find" | "findIndex" | "some" | "every" => {
                let cb = arg0;
                let snapshot = self.array_snapshot(idx);
                for (i, v) in snapshot.iter().enumerate() {
                    let r = self.call_value(cb, Value::UNDEFINED, &[*v, Value::int(i as i32)])?;
                    let t = self.truthy(r);
                    match name {
                        "find" if t => return Ok(Some(*v)),
                        "findIndex" if t => return Ok(Some(Value::int(i as i32))),
                        "some" if t => return Ok(Some(Value::bool(true))),
                        "every" if !t => return Ok(Some(Value::bool(false))),
                        _ => {}
                    }
                }
                Ok(Some(match name {
                    "find" => Value::UNDEFINED,
                    "findIndex" => Value::int(-1),
                    "some" => Value::bool(false),
                    _ => Value::bool(true), // every: all matched (or empty)
                }))
            }
            "reduce" => {
                let cb = arg0;
                let snapshot = self.array_snapshot(idx);
                let has_init = args.len() >= 2;
                // Seed + first index to process: with an initial value, start at
                // element 0; otherwise the first element seeds and we start at 1.
                let mut start = if has_init { 0 } else { 1 };
                let mut acc = if has_init {
                    args[1]
                } else if !snapshot.is_empty() {
                    snapshot[0]
                } else {
                    return Err(Thrown(
                        "TypeError: Reduce of empty array with no initial value".into(),
                    ));
                };

                // Fused native reduce kernel: inline the `(acc, element)`
                // callback into a native loop over the leading numeric run — no
                // per-element call. On a guard bail it returns the index reached
                // and the accumulated value (via the in/out acc pointer); the
                // per-element tail below finishes `[start, len)` correctly.
                #[cfg(all(feature = "jit", target_arch = "x86_64"))]
                if self.jit_enabled
                    && self.jit_recurse_depth == 0
                    && cb.is_heap()
                    && start < snapshot.len()
                {
                    if let Some((fid, ups)) = self.heap.as_callable(cb.heap_index()) {
                        if ups.is_empty() {
                            let proto: *const crate::bytecode::FuncProto =
                                &self.program.functions[fid as usize];
                            // SAFETY: immutable program functions; raw ptr dodges
                            // the jit-vs-program borrow conflict (as elsewhere).
                            let proto_ref = unsafe { &*proto };
                            let reg_count = (proto_ref.reg_count as usize).max(3);
                            if let Some(entry) = self.jit.reduce_kernel(fid, proto_ref) {
                                let win = self.regs.len();
                                if !self.regs_would_overflow(win + reg_count) {
                                    self.regs.resize(win + reg_count, Value::UNDEFINED);
                                    let count = snapshot.len() - start;
                                    let window_ptr =
                                        unsafe { self.regs.as_mut_ptr().add(win) } as *mut u64;
                                    let snap_ptr =
                                        unsafe { snapshot.as_ptr().add(start) } as *const u64;
                                    let mut acc_bits = acc.bits();
                                    // SAFETY: valid win64 reduce kernel; window has
                                    // reg_count slots; acc_bits is a live u64;
                                    // call-free ⇒ none of these pointers move.
                                    let kernel: extern "win64" fn(
                                        *mut u64,
                                        *const u64,
                                        usize,
                                        *mut u64,
                                    ) -> usize = unsafe { core::mem::transmute(entry) };
                                    let processed =
                                        kernel(window_ptr, snap_ptr, count, &mut acc_bits as *mut u64);
                                    acc = Value::from_bits(acc_bits);
                                    self.regs.truncate(win);
                                    start += processed;
                                }
                            }
                        }
                    }
                }

                // Per-element tail: the whole array if no kernel ran, or just the
                // remainder after a kernel bail (nothing if it completed).
                let run_tail = start < snapshot.len();
                let mut native = if run_tail { self.native_cb_entry(cb) } else { None };
                let win = self.regs.len();
                if let Some((_, callee_regs, _)) = native {
                    if self.regs_would_overflow(win + callee_regs) {
                        native = None;
                    } else {
                        self.regs.resize(win + callee_regs, Value::UNDEFINED);
                    }
                }
                let mut err = None;
                for i in start..snapshot.len() {
                    let cbargs = [acc, snapshot[i], Value::int(i as i32)];
                    match self.run_cb_elem(native, win, cb, &cbargs) {
                        Ok(r) => acc = r,
                        Err(e) => {
                            err = Some(e);
                            break;
                        }
                    }
                }
                if native.is_some() {
                    self.regs.truncate(win);
                }
                if let Some(e) = err {
                    return Err(e);
                }
                Ok(Some(acc))
            }
            "sort" => {
                let cmp = arg0;
                let mut snapshot = self.array_snapshot(idx);
                if cmp.is_heap() && self.heap.as_callable(cmp.heap_index()).is_some() {
                    // Comparator sort: stable O(n log n) bottom-up merge sort,
                    // re-entering the VM for each comparison.
                    self.comparator_sort(&mut snapshot, cmp)?;
                } else {
                    // Default sort: by string coercion (JS spec default).
                    snapshot.sort_by(|a, b| self.display(*a).cmp(&self.display(*b)));
                }
                if let HeapObj::Array(items) = self.heap.get_mut(idx) {
                    *items = snapshot;
                }
                Ok(Some(Value::heap(idx)))
            }
            "reduceRight" => {
                let cb = arg0;
                let snapshot = self.array_snapshot(idx);
                let mut i = snapshot.len();
                let mut acc = if args.len() >= 2 {
                    args[1]
                } else if i > 0 {
                    i -= 1;
                    snapshot[i]
                } else {
                    return Err(Thrown(
                        "TypeError: Reduce of empty array with no initial value".into(),
                    ));
                };
                while i > 0 {
                    i -= 1;
                    acc = self.call_value(cb, Value::UNDEFINED, &[acc, snapshot[i], Value::int(i as i32)])?;
                }
                Ok(Some(acc))
            }
            "flatMap" => {
                // map(cb) then flatten one level (array results spliced in).
                let cb = arg0;
                let snapshot = self.array_snapshot(idx);
                let mut out: Vec<Value> = Vec::new();
                for (i, v) in snapshot.iter().enumerate() {
                    let r = self.call_value(cb, Value::UNDEFINED, &[*v, Value::int(i as i32)])?;
                    if r.is_heap() {
                        if let HeapObj::Array(items) = self.heap.get(r.heap_index()) {
                            out.extend(items.iter().copied());
                            continue;
                        }
                    }
                    out.push(r);
                }
                Ok(Some(Value::heap(self.heap.alloc(HeapObj::Array(out)))))
            }
            "findLast" | "findLastIndex" => {
                let cb = arg0;
                let snapshot = self.array_snapshot(idx);
                for i in (0..snapshot.len()).rev() {
                    let v = snapshot[i];
                    let r = self.call_value(cb, Value::UNDEFINED, &[v, Value::int(i as i32)])?;
                    if self.truthy(r) {
                        return Ok(Some(if name == "findLast" {
                            v
                        } else {
                            Value::int(i as i32)
                        }));
                    }
                }
                Ok(Some(if name == "findLast" { Value::UNDEFINED } else { Value::int(-1) }))
            }
            "toSorted" => {
                // Like sort() but returns a NEW array; the receiver is unchanged.
                let cmp = arg0;
                let mut snapshot = self.array_snapshot(idx);
                if cmp.is_heap() && self.heap.as_callable(cmp.heap_index()).is_some() {
                    self.comparator_sort(&mut snapshot, cmp)?;
                } else {
                    snapshot.sort_by(|a, b| self.display(*a).cmp(&self.display(*b)));
                }
                Ok(Some(Value::heap(self.heap.alloc(HeapObj::Array(snapshot)))))
            }
            "toReversed" => {
                let mut snapshot = self.array_snapshot(idx);
                snapshot.reverse();
                Ok(Some(Value::heap(self.heap.alloc(HeapObj::Array(snapshot)))))
            }
            "splice" => {
                // splice(start, deleteCount?, ...items): mutate in place, return
                // the removed elements (start may be negative).
                let len = match self.heap.get(idx) {
                    HeapObj::Array(items) => items.len(),
                    _ => 0,
                };
                let s = if arg0.is_number() { arg0.as_f64() as i64 } else { 0 };
                let start = if s < 0 { (len as i64 + s).max(0) as usize } else { (s as usize).min(len) };
                let del = if args.len() < 2 {
                    len - start
                } else {
                    let d = if args[1].is_number() { args[1].as_f64() as i64 } else { 0 };
                    (d.max(0) as usize).min(len - start)
                };
                let insert: Vec<Value> = args.get(2..).unwrap_or(&[]).to_vec();
                let removed: Vec<Value> = match self.heap.get_mut(idx) {
                    HeapObj::Array(items) => items.splice(start..start + del, insert).collect(),
                    _ => Vec::new(),
                };
                self.heap.bump_version(idx); // length/contents changed
                Ok(Some(Value::heap(self.heap.alloc(HeapObj::Array(removed)))))
            }
            // Array iterators (real iterator objects with .next(), proto =
            // %ArrayIteratorPrototype%). values() is also the default @@iterator.
            "values" => {
                let items = self.array_snapshot(idx);
                Ok(Some(self.make_iterator(items, self.array_iter_proto)))
            }
            "keys" => {
                let len = match self.heap.get(idx) {
                    HeapObj::Array(items) => items.len(),
                    _ => 0,
                };
                let items: Vec<Value> = (0..len).map(|i| Value::int(i as i32)).collect();
                Ok(Some(self.make_iterator(items, self.array_iter_proto)))
            }
            "entries" => {
                let snap = self.array_snapshot(idx);
                let items: Vec<Value> = snap
                    .into_iter()
                    .enumerate()
                    .map(|(i, v)| Value::heap(self.heap.alloc(HeapObj::Array(vec![Value::int(i as i32), v]))))
                    .collect();
                Ok(Some(self.make_iterator(items, self.array_iter_proto)))
            }
            "toLocaleString" => {
                // Join each element's own toLocaleString() with ","; nullish → "".
                let snapshot = self.array_snapshot(idx);
                let mut parts: Vec<String> = Vec::with_capacity(snapshot.len());
                for v in snapshot {
                    if v.is_nullish() {
                        parts.push(String::new());
                    } else {
                        let f = self.get_prop(v, "toLocaleString")?;
                        let s = if self.is_callable(f) {
                            let r = self.call_value(f, v, &[])?;
                            self.display(r)
                        } else {
                            self.display(v)
                        };
                        parts.push(s);
                    }
                }
                Ok(Some(self.alloc_str(parts.join(","))))
            }
            "with" => {
                // with(index, value): a COPY with one index replaced. The index is
                // relative (negative from the end) and NOT clamped — an out-of-range
                // index throws a RangeError.
                let mut out = self.array_snapshot(idx);
                let len = out.len() as i64;
                let n = self.to_number(arg0)?;
                let rel = if n.is_nan() { 0 } else { n.trunc() as i64 };
                let actual = if rel >= 0 { rel } else { len + rel };
                if actual < 0 || actual >= len {
                    return Err(Thrown("RangeError: Invalid index".into()));
                }
                out[actual as usize] = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                Ok(Some(Value::heap(self.heap.alloc(HeapObj::Array(out)))))
            }
            "toSpliced" => {
                // Like splice() but returns the modified COPY; receiver unchanged.
                let mut out = self.array_snapshot(idx);
                let len = out.len();
                let s = if arg0.is_number() { arg0.as_f64() as i64 } else { 0 };
                let start = if s < 0 { (len as i64 + s).max(0) as usize } else { (s as usize).min(len) };
                let del = if args.len() < 2 {
                    len - start
                } else {
                    let d = if args[1].is_number() { args[1].as_f64() as i64 } else { 0 };
                    (d.max(0) as usize).min(len - start)
                };
                let insert: Vec<Value> = args.get(2..).unwrap_or(&[]).to_vec();
                out.splice(start..start + del, insert);
                Ok(Some(Value::heap(self.heap.alloc(HeapObj::Array(out)))))
            }
            "copyWithin" => {
                // copyWithin(target, start, end?): copy the [start,end) slice over the
                // run beginning at target, in place. Reads from a snapshot so
                // overlapping ranges behave as if copied from the original.
                let len = match self.heap.get(idx) {
                    HeapObj::Array(items) => items.len() as i32,
                    _ => 0,
                };
                let target = norm_index(if arg0.is_number() { arg0.as_f64() as i32 } else { 0 }, len);
                let start = norm_index(if args.len() >= 2 && args[1].is_number() { args[1].as_f64() as i32 } else { 0 }, len);
                let end = norm_index(if args.len() >= 3 && args[2].is_number() { args[2].as_f64() as i32 } else { len }, len);
                let count = (end - start).min(len - target).max(0);
                if count > 0 {
                    let snapshot = self.array_snapshot(idx);
                    if let HeapObj::Array(items) = self.heap.get_mut(idx) {
                        for k in 0..count {
                            items[(target + k) as usize] = snapshot[(start + k) as usize];
                        }
                    }
                    self.heap.bump_version(idx);
                }
                Ok(Some(Value::heap(idx)))
            }
            _ => Ok(None),
        }
    }

    /// Stable bottom-up merge sort driven by a JS comparator (`cmp(a,b) < 0` ⇒
    /// `a` before `b`). O(n log n) comparisons — vs the old insertion sort's
    /// O(n²), which dominated `Array.sort` for non-trivial sizes. Stable: on a tie
    /// (and on `<= 0`) the LEFT run's element wins, preserving original order. The
    /// comparator re-enters the VM (`call_value`) and may throw (propagated).
    fn comparator_sort(&mut self, items: &mut [Value], cmp: Value) -> Result<(), Thrown> {
        let n = items.len();
        if n < 2 {
            return Ok(());
        }
        // Native-callback fast path: a compiled non-capturing comparator is called
        // directly over one reused register window (skipping a per-comparison frame
        // build + run_loop re-entry). `native = None` falls back to call_value.
        let mut native = self.native_cb_entry(cmp);
        let win = self.regs.len();
        if let Some((_, callee_regs, _)) = native {
            if self.regs_would_overflow(win + callee_regs) {
                native = None;
            } else {
                self.regs.resize(win + callee_regs, Value::UNDEFINED);
            }
        }
        // Ping-pong between two local buffers (not self.regs/heap, so a comparator
        // that re-enters the VM and allocates can't invalidate them).
        let mut a: Vec<Value> = items.to_vec();
        let mut b: Vec<Value> = vec![Value::UNDEFINED; n];
        let mut width = 1;
        let mut err: Option<Thrown> = None;
        'outer: while width < n {
            let mut lo = 0;
            while lo < n {
                let mid = (lo + width).min(n);
                let hi = (lo + 2 * width).min(n);
                // Merge a[lo..mid] and a[mid..hi] into b[lo..hi], stably.
                let (mut l, mut r, mut k) = (lo, mid, lo);
                while l < mid && r < hi {
                    let c = match self.run_cb_elem(native, win, cmp, &[a[l], a[r]]) {
                        Ok(c) => c,
                        Err(e) => {
                            err = Some(e);
                            break 'outer;
                        }
                    };
                    if c.as_f64() <= 0.0 {
                        b[k] = a[l];
                        l += 1;
                    } else {
                        b[k] = a[r];
                        r += 1;
                    }
                    k += 1;
                }
                while l < mid {
                    b[k] = a[l];
                    l += 1;
                    k += 1;
                }
                while r < hi {
                    b[k] = a[r];
                    r += 1;
                    k += 1;
                }
                lo += 2 * width;
            }
            std::mem::swap(&mut a, &mut b);
            width *= 2;
        }
        if native.is_some() {
            self.regs.truncate(win); // release the reused window (success or error)
        }
        if let Some(e) = err {
            return Err(e);
        }
        items.copy_from_slice(&a);
        Ok(())
    }

    /// The i-th char of a flat string by heap index, WITHOUT cloning the string —
    /// O(1) for ASCII (i-th byte), else an O(i) scalar scan. `None` if out of range
    /// or not a flat string. (A full-string clone here would make `charCodeAt(i)`
    /// in a loop O(n²) in the string length — the real cost of these methods.)
    fn heap_char_at(&self, idx: u32, i: usize) -> Option<char> {
        match self.heap.get(idx) {
            HeapObj::Str(js) => {
                if js.ascii {
                    js.bytes.as_bytes().get(i).map(|&b| b as char)
                } else {
                    js.bytes.chars().nth(i)
                }
            }
            _ => None,
        }
    }

    /// Char length of a flat string by heap index — O(1) for ASCII.
    fn heap_char_len(&self, idx: u32) -> usize {
        match self.heap.get(idx) {
            HeapObj::Str(js) => {
                if js.ascii {
                    js.bytes.len()
                } else {
                    js.bytes.chars().count()
                }
            }
            _ => 0,
        }
    }

    fn string_method(&mut self, idx: u32, name: &str, args: &[Value]) -> Result<Option<Value>, Thrown> {
        self.heap.flatten(idx); // materialize a rope receiver before reading it
        let arg0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        // Single-char index methods: read one char directly from the heap with NO
        // full-string clone (the clone below is O(n), so these would be O(n²) in a
        // per-char loop — `s.charCodeAt(i)` scanning is a very common idiom).
        match name {
            "charCodeAt" => {
                let i = arg0.as_f64() as i32;
                let c = if i >= 0 { self.heap_char_at(idx, i as usize) } else { None };
                return Ok(Some(match c {
                    Some(c) => Value::int(c as i32),
                    None => Value::num(f64::NAN),
                }));
            }
            "codePointAt" => {
                let i = arg0.as_f64() as i32;
                let c = if i >= 0 { self.heap_char_at(idx, i as usize) } else { None };
                return Ok(Some(match c {
                    Some(c) => Value::int(c as i32),
                    None => Value::UNDEFINED,
                }));
            }
            "charAt" => {
                let i = arg0.as_f64() as i32;
                let c = if i >= 0 { self.heap_char_at(idx, i as usize) } else { None };
                return Ok(Some(self.alloc_str(c.map(|c| c.to_string()).unwrap_or_default())));
            }
            "at" => {
                let len = self.heap_char_len(idx) as i64;
                let i = if arg0.is_number() { arg0.as_f64() as i64 } else { 0 };
                let abs = if i < 0 { i + len } else { i };
                let c = if abs >= 0 && abs < len { self.heap_char_at(idx, abs as usize) } else { None };
                return Ok(Some(match c {
                    Some(c) => self.alloc_str(c.to_string()),
                    None => Value::UNDEFINED,
                }));
            }
            _ => {}
        }
        // Other methods need an owned String (slice/replace/split/…).
        let (s, ascii) = match self.heap.get(idx) {
            HeapObj::Str(js) => (js.bytes.clone(), js.ascii),
            _ => return Ok(None),
        };
        let char_len = |s: &str| -> usize {
            if ascii {
                s.len()
            } else {
                s.chars().count()
            }
        };
        match name {
            "indexOf" => {
                let needle = self.display(arg0);
                // Optional fromIndex (a char position) to start searching at.
                let from = if args.len() >= 2 && args[1].is_number() {
                    args[1].as_f64().max(0.0) as usize
                } else {
                    0
                };
                let byte_from = s.char_indices().nth(from).map(|(b, _)| b).unwrap_or(s.len());
                let pos = s[byte_from..]
                    .find(&needle)
                    .map(|b| s[..byte_from + b].chars().count() as i32)
                    .unwrap_or(-1);
                Ok(Some(Value::int(pos)))
            }
            "includes" => {
                let needle = self.display(arg0);
                Ok(Some(Value::bool(s.contains(&needle))))
            }
            "toUpperCase" => Ok(Some(self.alloc_str(s.to_uppercase()))),
            "toLowerCase" => Ok(Some(self.alloc_str(s.to_lowercase()))),
            "slice" | "substring" => {
                let len = char_len(&s) as i32;
                let start = norm_index(if args.is_empty() { 0 } else { arg0.as_f64() as i32 }, len);
                let end = if args.len() < 2 { len } else { norm_index(args[1].as_f64() as i32, len) };
                let out: String = if start < end {
                    s.chars().skip(start as usize).take((end - start) as usize).collect()
                } else {
                    String::new()
                };
                Ok(Some(self.alloc_str(out)))
            }
            "repeat" => {
                let n = arg0.as_f64();
                if n < 0.0 || !n.is_finite() {
                    return Err(Thrown("RangeError: Invalid count value".into()));
                }
                // Bound the result (an unbounded build would hang / OOM) — a too-long
                // string is a RangeError per spec. (Empty string repeats to "" for any
                // count, so its length is always 0 — no bound needed.)
                if n * (s.len() as f64) > (1u64 << 28) as f64 {
                    return Err(Thrown("RangeError: Invalid string length".into()));
                }
                Ok(Some(self.alloc_str(s.repeat(n as usize))))
            }
            "search" => {
                let re = self.to_regexp_arg(arg0)?;
                let found = match self.heap.get(re) {
                    HeapObj::RegExp { regex, .. } => regex.find(&s),
                    _ => None,
                };
                Ok(Some(match found {
                    Some(m) => Value::num(byte_to_char(&s, m.start()) as f64),
                    None => Value::int(-1),
                }))
            }
            "match" => {
                let re = self.to_regexp_arg(arg0)?;
                let global =
                    matches!(self.heap.get(re), HeapObj::RegExp { flags, .. } if flags.contains('g'));
                if global {
                    let strs: Vec<String> = match self.heap.get(re) {
                        HeapObj::RegExp { regex, .. } => {
                            regex.find_iter(&s).map(|m| s[m.range()].to_string()).collect()
                        }
                        _ => Vec::new(),
                    };
                    self.set_regexp_last_index(re, 0);
                    if strs.is_empty() {
                        return Ok(Some(Value::NULL));
                    }
                    let elems: Vec<Value> = strs.into_iter().map(|m| self.alloc_str(m)).collect();
                    Ok(Some(Value::heap(self.heap.alloc(HeapObj::Array(elems)))))
                } else {
                    let r = self.regexp_exec(re, Value::heap(idx))?;
                    Ok(Some(r))
                }
            }
            "split" if self.as_regexp(arg0).is_some() => {
                let re = self.as_regexp(arg0).unwrap();
                let limit = match args.get(1) {
                    Some(&v) if v != Value::UNDEFINED => self.to_number(v)? as usize,
                    _ => usize::MAX,
                };
                let spans: Vec<(usize, usize)> = match self.heap.get(re) {
                    HeapObj::RegExp { regex, .. } => {
                        regex.find_iter(&s).map(|m| (m.start(), m.end())).collect()
                    }
                    _ => Vec::new(),
                };
                let mut parts: Vec<Value> = Vec::new();
                let mut last = 0usize;
                for (st, en) in spans {
                    if parts.len() >= limit {
                        break;
                    }
                    if st < last || (st == en && st == last) {
                        continue; // skip overlapping / empty-at-cursor matches
                    }
                    parts.push(self.alloc_str(s[last..st].to_string()));
                    last = en;
                }
                if parts.len() < limit {
                    parts.push(self.alloc_str(s[last..].to_string()));
                }
                Ok(Some(Value::heap(self.heap.alloc(HeapObj::Array(parts)))))
            }
            "replace" if self.as_regexp(arg0).is_some() => {
                let re = self.as_regexp(arg0).unwrap();
                let global =
                    matches!(self.heap.get(re), HeapObj::RegExp { flags, .. } if flags.contains('g'));
                let repl = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let out = self.regex_replace(&s, re, repl, global)?;
                Ok(Some(self.alloc_str(out)))
            }
            "replaceAll" if self.as_regexp(arg0).is_some() => {
                let re = self.as_regexp(arg0).unwrap();
                let global =
                    matches!(self.heap.get(re), HeapObj::RegExp { flags, .. } if flags.contains('g'));
                if !global {
                    return Err(Thrown(
                        "TypeError: replaceAll must be called with a global RegExp".into(),
                    ));
                }
                let repl = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let out = self.regex_replace(&s, re, repl, true)?;
                Ok(Some(self.alloc_str(out)))
            }
            "split" => {
                let sep = self.display(arg0);
                let parts: Vec<Value> = if args.is_empty() {
                    vec![self.alloc_str(s.clone())]
                } else if sep.is_empty() {
                    s.chars().map(|c| self.alloc_str(c.to_string())).collect()
                } else {
                    s.split(&sep).map(|p| self.alloc_str(p.to_string())).collect()
                };
                Ok(Some(Value::heap(self.heap.alloc(HeapObj::Array(parts)))))
            }
            "trim" => Ok(Some(self.alloc_str(s.trim().to_string()))),
            "trimStart" => Ok(Some(self.alloc_str(s.trim_start().to_string()))),
            "trimEnd" => Ok(Some(self.alloc_str(s.trim_end().to_string()))),
            "startsWith" => Ok(Some(Value::bool(s.starts_with(&self.display(arg0))))),
            "endsWith" => Ok(Some(Value::bool(s.ends_with(&self.display(arg0))))),
            "concat" => {
                let mut out = s.clone();
                for a in args {
                    out.push_str(&self.display(*a));
                }
                Ok(Some(self.alloc_str(out)))
            }
            "substr" => {
                // Legacy substr(start, length); negative start counts from the end.
                let chars: Vec<char> = s.chars().collect();
                let len = chars.len() as i64;
                let sr = if args.is_empty() { 0.0 } else { arg0.as_f64() };
                let mut start = if sr.is_nan() { 0 } else { sr as i64 };
                if start < 0 {
                    start = (len + start).max(0);
                }
                let start = start.min(len) as usize;
                let avail = chars.len() - start;
                let count = if args.len() < 2 || args[1] == Value::UNDEFINED {
                    avail
                } else {
                    let c = args[1].as_f64();
                    if c.is_nan() || c < 0.0 { 0 } else { (c as usize).min(avail) }
                };
                let sub: String = chars[start..start + count].iter().collect();
                Ok(Some(self.alloc_str(sub)))
            }
            "localeCompare" => {
                // No Intl: a code-unit ordinal comparison (the default approximation).
                let other = self.display(arg0);
                let ord = match s.as_str().cmp(other.as_str()) {
                    std::cmp::Ordering::Less => -1,
                    std::cmp::Ordering::Equal => 0,
                    std::cmp::Ordering::Greater => 1,
                };
                Ok(Some(Value::int(ord)))
            }
            "normalize" => {
                // Validate the form; engine strings are already normalized for ASCII
                // (full Unicode normalization isn't modelled).
                let form = if args.is_empty() || arg0 == Value::UNDEFINED {
                    "NFC".to_string()
                } else {
                    self.display(arg0)
                };
                if !matches!(form.as_str(), "NFC" | "NFD" | "NFKC" | "NFKD") {
                    return Err(Thrown(
                        "RangeError: The normalization form should be one of NFC, NFD, NFKC, NFKD.".into(),
                    ));
                }
                Ok(Some(self.alloc_str(s.clone())))
            }
            // Engine strings are valid UTF-8 (no lone surrogates), so always well-formed.
            "isWellFormed" => Ok(Some(Value::bool(true))),
            "toWellFormed" => Ok(Some(self.alloc_str(s.clone()))),
            // String.prototype.valueOf/toString return the string primitive itself
            // (used by a boxed String's valueOf/toString after unwrapping).
            "valueOf" | "toString" => Ok(Some(Value::heap(idx))),
            "padStart" | "padEnd" => {
                let cur = char_len(&s);
                let t = arg0.as_f64();
                let target = if t.is_finite() && t > 0.0 { t as usize } else { 0 };
                if target as u64 > (1u64 << 28) {
                    return Err(Thrown("RangeError: Invalid string length".into()));
                }
                if cur >= target {
                    return Ok(Some(self.alloc_str(s.clone())));
                }
                let pad = if args.len() >= 2 { self.display(args[1]) } else { " ".to_string() };
                let padchars: Vec<char> = pad.chars().collect();
                if padchars.is_empty() {
                    return Ok(Some(self.alloc_str(s.clone())));
                }
                let mut padding = String::new();
                for k in 0..(target - cur) {
                    padding.push(padchars[k % padchars.len()]);
                }
                let out = if name == "padStart" {
                    format!("{padding}{s}")
                } else {
                    format!("{s}{padding}")
                };
                Ok(Some(self.alloc_str(out)))
            }
            "replace" => {
                // String search: replaces only the FIRST occurrence (JS semantics).
                let search = self.display(arg0);
                let repl = if args.len() >= 2 { self.display(args[1]) } else { "undefined".to_string() };
                let out = match s.find(&search) {
                    Some(pos) => {
                        let mut r = String::with_capacity(s.len() + repl.len());
                        r.push_str(&s[..pos]);
                        r.push_str(&repl);
                        r.push_str(&s[pos + search.len()..]);
                        r
                    }
                    None => s.clone(),
                };
                Ok(Some(self.alloc_str(out)))
            }
            "replaceAll" => {
                let search = self.display(arg0);
                let repl = if args.len() >= 2 { self.display(args[1]) } else { "undefined".to_string() };
                Ok(Some(self.alloc_str(s.replace(&search, &repl))))
            }
            _ => Ok(None),
        }
    }

    /// Clone an array's current elements out of the heap. Used before invoking
    /// callbacks so a heap reallocation during the call can't dangle a borrow.
    /// Read an array-like receiver's elements (`this.length` coerced via ToLength,
    /// then `this[0 .. length]`) into a Vec — backs the generic Array.prototype
    /// methods invoked via `.call(arrayLike, …)` on a non-array (object or string).
    fn array_like_read(&mut self, idx: u32) -> Vec<Value> {
        let this = Value::heap(idx);
        if let HeapObj::Array(items) = self.heap.get(idx) {
            return items.clone();
        }
        let len = self
            .get_prop(this, "length")
            .ok()
            .and_then(|v| self.to_number(v).ok())
            .unwrap_or(0.0);
        let len = if len.is_finite() && len > 0.0 { (len as usize).min(1 << 26) } else { 0 };
        let mut out = Vec::with_capacity(len.min(4096));
        for i in 0..len {
            out.push(self.get_index(this, Value::int(i as i32)).unwrap_or(Value::UNDEFINED));
        }
        out
    }

    fn array_snapshot(&self, idx: u32) -> Vec<Value> {
        match self.heap.get(idx) {
            HeapObj::Array(items) => items.clone(),
            _ => Vec::new(),
        }
    }

    /// Recursively flatten nested arrays up to `depth` levels (for `Array.flat`).
    /// Each nested array is cloned out before recursing (releases the heap borrow).
    fn flatten_array(&self, items: &[Value], depth: i32) -> Vec<Value> {
        let mut out = Vec::new();
        for v in items {
            let nested: Option<Vec<Value>> = if depth > 0 && v.is_heap() {
                match self.heap.get(v.heap_index()) {
                    HeapObj::Array(a) => Some(a.clone()),
                    _ => None,
                }
            } else {
                None
            };
            match nested {
                Some(a) => out.extend(self.flatten_array(&a, depth - 1)),
                None => out.push(*v),
            }
        }
        out
    }

    /// Strict equality between two raw values (no register indirection). Mirrors
    /// `strict_eq` but takes values directly, for builtin use.
    /// SameValueZero — Map/Set key & element equality. Like `===` but NaN equals
    /// NaN (so NaN is a usable key and all NaNs dedupe). +0/-0 are equal here too
    /// (matching `===`); the store side normalizes -0 → +0. Strings compare by
    /// value, objects by reference identity, and there is no type coercion.
    /// Whether `v` is a JS Object (Type(v) === Object): a heap value that is not a
    /// primitive string (`Str`/`Cons`). Used by Reflect, which throws on non-objects.
    fn is_object_value(&self, v: Value) -> bool {
        v.is_heap() && !self.heap.is_str_like(v.heap_index())
    }

    /// `ToString(v)` as a Rust String, honouring a user `toString`/`valueOf` on an
    /// object (ToPrimitive with the string hint). Primitives and engine strings use
    /// `display`; a plain object with only the built-in (native) toString also falls
    /// back to `display` (which already yields "[object Object]" / the array join).
    fn to_js_string(&mut self, v: Value) -> Result<String, Thrown> {
        if !v.is_heap() || self.heap.is_str_like(v.heap_index()) {
            return Ok(self.display(v));
        }
        // ToString of a Symbol is a TypeError (use `.toString()` / `String(sym)`
        // explicitly instead — but even `String(sym)` routes through the dedicated
        // path, not this coercion).
        if matches!(self.heap.get(v.heap_index()), HeapObj::Symbol { .. }) {
            return Err(Thrown("TypeError: Cannot convert a Symbol value to a string".into()));
        }
        for name in ["toString", "valueOf"] {
            let f = self.get_prop(v, name)?;
            if f.is_heap() && self.heap.as_callable(f.heap_index()).is_some() {
                let r = self.call_value(f, v, &[])?;
                if !r.is_heap() || self.heap.is_str_like(r.heap_index()) {
                    return Ok(self.display(r));
                }
            }
        }
        Ok(self.display(v))
    }

    /// Whether `v` has a `[[Construct]]` slot — i.e. `new v` / `Reflect.construct`
    /// is valid. Plain functions and classes qualify; native methods, bound values,
    /// and non-callables do not. (test262's `isConstructor` helper probes this via
    /// `Reflect.construct(fn, [], v)`, so getting it right matters across the suite.)
    fn is_constructor(&self, v: Value) -> bool {
        if !v.is_heap() {
            return false;
        }
        match self.heap.get(v.heap_index()) {
            HeapObj::Func(_) | HeapObj::Closure { .. } | HeapObj::Class(_) => true,
            // The built-in constructor globals (Object/Array/Map/…) are constructors.
            HeapObj::Object(m) => m.is_ctor,
            _ => false,
        }
    }

    /// JS `SameValue` (Object.is): like SameValueZero but +0 and -0 are distinct.
    fn same_value(&self, a: Value, b: Value) -> bool {
        if a.is_number() && b.is_number() {
            let (x, y) = (a.as_f64(), b.as_f64());
            if x == 0.0 && y == 0.0 {
                return x.is_sign_negative() == y.is_sign_negative();
            }
            if x.is_nan() && y.is_nan() {
                return true;
            }
            return x == y;
        }
        self.same_value_zero(a, b)
    }

    fn same_value_zero(&self, a: Value, b: Value) -> bool {
        if a.is_number() && b.is_number() {
            let (na, nb) = (a.as_f64(), b.as_f64());
            return na == nb || (na.is_nan() && nb.is_nan());
        }
        self.values_strict_eq(a, b)
    }

    fn values_strict_eq(&self, a: Value, b: Value) -> bool {
        if a.bits() == b.bits() {
            if a.is_double() && a.as_f64().is_nan() {
                return false;
            }
            return true;
        }
        if a.is_number() && b.is_number() {
            return a.as_f64() == b.as_f64();
        }
        if a.is_heap() && b.is_heap() {
            let (ai, bi) = (a.heap_index(), b.heap_index());
            if self.heap.is_str_like(ai) && self.heap.is_str_like(bi) {
                return self.heap.str_eq(ai, bi);
            }
            if let (HeapObj::BigInt(x), HeapObj::BigInt(y)) = (self.heap.get(ai), self.heap.get(bi)) {
                return x == y;
            }
        }
        false
    }

    /// JS loose equality `==` (the Abstract Equality Comparison). Same-type
    /// compares like `===`; cross-type coerces per spec: null == undefined;
    /// number vs string coerces the string to a number; boolean coerces to a
    /// number; an object vs a primitive coerces the object to its primitive
    /// (here: string coercion, since we have no valueOf). NaN is never equal.
    fn loose_eq(&self, a: Value, b: Value) -> Result<bool, Thrown> {
        // BigInt loose equality compares mathematical values across types
        // (`1n == 1`, `1n == "1"`, `1n == true`), so handle it before the generic
        // same-tag/heap shortcuts (two distinct 1n allocations aren't bit-equal).
        let (ab, bb) = (self.bigint_value(a), self.bigint_value(b));
        if ab.is_some() || bb.is_some() {
            return Ok(match (ab, bb) {
                (Some(x), Some(y)) => x == y,
                (Some(x), None) => self.bigint_loose_eq_other(x, b),
                (None, Some(y)) => self.bigint_loose_eq_other(y, a),
                _ => false,
            });
        }
        // Same NaN-box tag class → strict semantics already cover it.
        if (a.is_number() && b.is_number())
            || (a.is_bool() && b.is_bool())
            || (a.is_heap() && b.is_heap())
        {
            return Ok(self.values_strict_eq(a, b));
        }
        // null == undefined (and each with itself), but not with anything else.
        if a.is_nullish() || b.is_nullish() {
            return Ok(a.is_nullish() && b.is_nullish());
        }
        // From here neither side is null/undefined. Coerce toward numbers,
        // except string-vs-string (handled above via the heap case) and
        // string-vs-heapobject which JS compares by string.
        // boolean → number, then retry.
        if a.is_bool() {
            return self.loose_eq(Value::num(if a.as_bool() { 1.0 } else { 0.0 }), b);
        }
        if b.is_bool() {
            return self.loose_eq(a, Value::num(if b.as_bool() { 1.0 } else { 0.0 }));
        }
        // number vs string: coerce string to number.
        // string vs object / number vs object: coerce via to_number (objects
        // become NaN here, matching `1 == {}` → false; `"[object Object]"`
        // string comparisons aren't reached because both-heap is handled above).
        let an = self.to_number(a)?;
        let bn = self.to_number(b)?;
        Ok(an == bn)
    }

    /// `BigInt x == <non-BigInt other>`: compare mathematical values. Number must
    /// be a finite integer; a string is parsed as a BigInt; boolean → 0/1; an
    /// object/symbol/null/undefined is never loosely equal to a BigInt here.
    fn bigint_loose_eq_other(&self, x: i128, other: Value) -> bool {
        if other.is_bool() {
            return x == if other.as_bool() { 1 } else { 0 };
        }
        if other.is_number() {
            let n = other.as_f64();
            return n.is_finite() && n.fract() == 0.0 && (x as f64) == n;
        }
        if other.is_heap() && self.heap.is_str_like(other.heap_index()) {
            if let Some(s) = self.heap.str_cow(other.heap_index()) {
                let t = s.trim();
                if t.is_empty() {
                    return x == 0;
                }
                return parse_bigint_str(t).is_some_and(|y| y == x);
            }
        }
        false
    }

    // ── arithmetic / coercion helpers ──

    #[inline]
    fn add(&mut self, base: usize, a: u16, b: u16) -> Result<Value, Thrown> {
        let va = self.get(base, a);
        let vb = self.get(base, b);
        self.add_values(va, vb)
    }

    /// The `+` operator on two already-fetched Values (shared by the interpreter's
    /// `Add`/`StrConcat` and the JIT's `jit_concat` helper).
    #[inline]
    /// ToPrimitive a boxed primitive (`new Number(5)` → 5) for use in operators;
    /// a non-box passes through. (Our boxes' valueOf returns the wrapped value, so
    /// this is ToPrimitive with the default/number hint.)
    fn unwrap_boxed(&self, v: Value) -> Value {
        if v.is_heap() {
            if let HeapObj::Boxed { value, .. } = self.heap.get(v.heap_index()) {
                return *value;
            }
        }
        v
    }

    pub(crate) fn add_values(&mut self, va: Value, vb: Value) -> Result<Value, Thrown> {
        let (va, vb) = (self.unwrap_boxed(va), self.unwrap_boxed(vb));
        // A Symbol operand can't be added: ToString (string side) and ToNumber
        // (numeric side) both throw for a Symbol.
        for v in [va, vb] {
            if v.is_heap() && matches!(self.heap.get(v.heap_index()), HeapObj::Symbol { .. }) {
                return Err(Thrown("TypeError: Cannot convert a Symbol value to a string".into()));
            }
        }
        // Fast path: int + int with overflow check.
        if va.is_int() && vb.is_int() {
            return Ok(match va.as_int().checked_add(vb.as_int()) {
                Some(v) => Value::int(v),
                None => Value::num(va.as_int() as f64 + vb.as_int() as f64),
            });
        }
        // BigInt `+`: both BigInt → addition; BigInt + string → concatenation
        // (ToString the BigInt); BigInt + anything-else → mixing TypeError.
        let (ab, bb) = (self.bigint_value(va), self.bigint_value(vb));
        if ab.is_some() || bb.is_some() {
            if let (Some(x), Some(y)) = (ab, bb) {
                return Ok(self.make_bigint(x.wrapping_add(y)));
            }
            let other = if ab.is_some() { vb } else { va };
            let other_is_str = other.is_heap() && self.heap.is_str_like(other.heap_index());
            if !other_is_str {
                return Err(Thrown(
                    "TypeError: Cannot mix BigInt and other types, use explicit conversions".into(),
                ));
            }
            // else: fall through to string concatenation (to_str_idx → decimal).
        }
        // If either side is a heap value, JS `+` is string concatenation (arrays
        // and objects coerce to a string primitive, and string+anything joins).
        // Build a rope (cons-string) in O(1) — children point at existing flat
        // strings / ropes, so a `s += x` loop is O(n) overall, not O(n²).
        if va.is_heap() || vb.is_heap() {
            let li = self.to_str_idx(va);
            let ri = self.to_str_idx(vb);
            let llen = self.heap.str_char_len(li).unwrap_or(0);
            let rlen = self.heap.str_char_len(ri).unwrap_or(0);
            return Ok(Value::heap(self.heap.alloc_cons(li, ri, llen + rlen)));
        }
        Ok(Value::num(self.to_number(va)? + self.to_number(vb)?))
    }

    /// `acc + val` as a string append that MUTATES `acc`'s buffer in place when
    /// `acc` is a uniquely-owned, non-interned flat string (`Str` at a user heap
    /// index). Otherwise — `acc` is the interned `""`/single-char (first append),
    /// a rope, or not a string — it allocates a FRESH non-interned flat string
    /// `display(acc) + display(val)` (never interned, so the NEXT append mutates
    /// it). Correctness rests on the emitter's linearity proof: the only reference
    /// to the mutated buffer is the accumulator itself, so the mutation is
    /// unobservable. Returns the (possibly unchanged) accumulator Value.
    pub(crate) fn str_append_inplace(&mut self, acc: Value, val: Value) -> Value {
        let mutable = acc.is_heap()
            && acc.heap_index() > crate::heap::INTERN_EMPTY
            && matches!(self.heap.get(acc.heap_index()), HeapObj::Str(_));
        // Fast path: appending a single decimal digit (the `s += i%10` shape) —
        // no temporary allocation for the value's string form.
        if mutable && val.is_int() {
            let n = val.as_int();
            if (0..=9).contains(&n) {
                if let HeapObj::Str(js) = self.heap.get_mut(acc.heap_index()) {
                    js.bytes.push((b'0' + n as u8) as char);
                    js.char_len += 1;
                    return acc;
                }
            }
        }
        // General: materialise `val`'s string form (same coercion as `+`).
        let ri = self.to_str_idx(val);
        let add: String = self.heap.str_cow(ri).map(|c| c.into_owned()).unwrap_or_default();
        if mutable {
            if let HeapObj::Str(js) = self.heap.get_mut(acc.heap_index()) {
                let cl = add.chars().count();
                let asc = add.is_ascii();
                js.bytes.push_str(&add);
                js.char_len += cl;
                js.ascii &= asc;
                return acc;
            }
        }
        // Fresh buffer (first append / interned / rope acc): flatten acc + add into
        // a NON-interned `Str` (bypass `alloc_str`'s interning so it's mutable next).
        let li = self.to_str_idx(acc);
        let mut s: String =
            self.heap.str_cow(li).map(|c| c.into_owned()).unwrap_or_default();
        s.push_str(&add);
        Value::heap(self.heap.alloc(HeapObj::Str(crate::heap::JsStr::new(s))))
    }

    /// Heap index of a string-like object representing `v`: `v`'s own index when
    /// it is already a string (flat or rope), else a freshly allocated flat
    /// string from `v`'s string coercion. Used to build rope children.
    fn to_str_idx(&mut self, v: Value) -> u32 {
        if v.is_heap() && self.heap.is_str_like(v.heap_index()) {
            return v.heap_index();
        }
        // A single-digit int is a 1-char ASCII string, already interned at its
        // byte — return that slot directly (no temporary `String` alloc). This is
        // the hot `s += (i % 10)` digit-concat case.
        if v.is_int() {
            let n = v.as_int();
            if (0..=9).contains(&n) {
                return (b'0' as i32 + n) as u32;
            }
        }
        let s = self.display(v);
        self.heap.alloc_str(s)
    }

    #[inline]
    fn cmp_lt(&mut self, base: usize, a: u16, b: u16) -> Result<bool, Thrown> {
        let va = self.get(base, a);
        let vb = self.get(base, b);
        if va.is_int() && vb.is_int() {
            return Ok(va.as_int() < vb.as_int());
        }
        if let Some(o) = self.str_relational(va, vb) {
            return Ok(o.is_lt());
        }
        Ok(self.to_number(va)? < self.to_number(vb)?)
    }
    #[inline]
    fn cmp_le(&mut self, base: usize, a: u16, b: u16) -> Result<bool, Thrown> {
        let va = self.get(base, a);
        let vb = self.get(base, b);
        if va.is_int() && vb.is_int() {
            return Ok(va.as_int() <= vb.as_int());
        }
        if let Some(o) = self.str_relational(va, vb) {
            return Ok(o.is_le());
        }
        Ok(self.to_number(va)? <= self.to_number(vb)?)
    }

    /// JS relational comparison of two STRING operands is lexicographic (by code
    /// unit) — not numeric. Returns the `Ordering` when both are string-like, else
    /// `None` (the caller falls back to numeric comparison). Mirrors the engine's
    /// code-point ordering (≈ UTF-16 for the BMP; astral chars are a known edge).
    fn str_relational(&self, va: Value, vb: Value) -> Option<std::cmp::Ordering> {
        if va.is_heap()
            && vb.is_heap()
            && self.heap.is_str_like(va.heap_index())
            && self.heap.is_str_like(vb.heap_index())
        {
            let sa = self.heap.str_cow(va.heap_index())?;
            let sb = self.heap.str_cow(vb.heap_index())?;
            return Some(sa.as_ref().cmp(sb.as_ref()));
        }
        None
    }

    fn strict_eq(&self, base: usize, a: u16, b: u16) -> bool {
        let va = self.get(base, a);
        let vb = self.get(base, b);
        // Same bits → equal (covers int, bool, null, undefined, same heap idx).
        if va.bits() == vb.bits() {
            // NaN !== NaN even with identical bits.
            if va.is_double() && va.as_f64().is_nan() {
                return false;
            }
            return true;
        }
        // Numeric cross-representation (int vs double) compares by value.
        if va.is_number() && vb.is_number() {
            return va.as_f64() == vb.as_f64();
        }
        // Distinct heap strings with equal contents are `===` equal.
        if va.is_heap() && vb.is_heap() {
            let (ai, bi) = (va.heap_index(), vb.heap_index());
            // Two DISTINCT interned single-ASCII-char slots (idx < INTERN_EMPTY,
            // see Heap::new) are different chars — bits already differ here, so
            // they can't be equal; skip the content compare. This is the hot
            // `s[i] === 'x'` char-check in scanners/lexers.
            if ai < crate::heap::INTERN_EMPTY && bi < crate::heap::INTERN_EMPTY {
                return false;
            }
            if self.heap.is_str_like(ai) && self.heap.is_str_like(bi) {
                return self.heap.str_eq(ai, bi);
            }
            // BigInt === BigInt compares by value (1n === 1n), not heap identity.
            if let (HeapObj::BigInt(x), HeapObj::BigInt(y)) =
                (self.heap.get(ai), self.heap.get(bi))
            {
                return x == y;
            }
        }
        false
    }

    #[inline]
    fn truthy(&self, v: Value) -> bool {
        if let Some(t) = v.truthy_primitive() {
            return t;
        }
        // Heap: empty string is falsy; 0n is falsy; everything else truthy.
        if let Some(empty) = self.heap.str_is_empty(v.heap_index()) {
            return !empty;
        }
        if let HeapObj::BigInt(n) = self.heap.get(v.heap_index()) {
            return *n != 0;
        }
        true
    }

    fn to_number(&self, v: Value) -> Result<f64, Thrown> {
        if v.is_number() {
            return Ok(v.as_f64());
        }
        if v.is_bool() {
            return Ok(if v.as_bool() { 1.0 } else { 0.0 });
        }
        if v.is_null() {
            return Ok(0.0);
        }
        if v.is_undefined() {
            return Ok(f64::NAN);
        }
        // A Date coerces to its epoch ms (so `d2 - d1`, `+d`, `d1 < d2` work).
        if let HeapObj::Date(ms) = self.heap.get(v.heap_index()) {
            return Ok(*ms);
        }
        // A boxed primitive coerces to its wrapped value's number (ToPrimitive).
        if let HeapObj::Boxed { value, .. } = self.heap.get(v.heap_index()) {
            return self.to_number(*value);
        }
        // ToNumber of a Symbol is a TypeError.
        if matches!(self.heap.get(v.heap_index()), HeapObj::Symbol { .. }) {
            return Err(Thrown("TypeError: Cannot convert a Symbol value to a number".into()));
        }
        // A BigInt's numeric value (for `Number(1n)` and relational comparison;
        // arithmetic mixing is rejected earlier by `bigint_binop`).
        if let HeapObj::BigInt(n) = self.heap.get(v.heap_index()) {
            return Ok(*n as f64);
        }
        if let Some(s) = self.heap.str_cow(v.heap_index()) {
            let t = s.trim();
            if t.is_empty() {
                return Ok(0.0);
            }
            return Ok(t.parse::<f64>().unwrap_or(f64::NAN));
        }
        Ok(f64::NAN)
    }

    /// String COERCION (`String(v)`, `'' + v`, property keys). Arrays join with
    /// commas; objects become `[object Object]` — JS `toString` semantics.
    fn display(&self, v: Value) -> String {
        if v.is_int() {
            v.as_int().to_string()
        } else if v.is_double() {
            fmt_f64(v.as_f64())
        } else if v.is_bool() {
            v.as_bool().to_string()
        } else if v.is_null() {
            "null".into()
        } else if v.is_undefined() {
            "undefined".into()
        } else if v.is_heap() {
            match self.heap.get(v.heap_index()) {
                HeapObj::Proxy { target, .. } => return self.display(*target),
                HeapObj::Temporal { kind: 0, fields } => {
                    let mut f = [0i64; 10];
                    for (i, s) in f.iter_mut().enumerate() {
                        *s = *fields.get(i).unwrap_or(&0);
                    }
                    duration_to_string(&f)
                }
                HeapObj::Temporal { kind: 1, fields } => {
                    iso_date_string(fields[0], fields[1], fields[2])
                }
                HeapObj::Temporal { kind: 2, fields } => {
                    let mut f = [0i64; 6];
                    for (i, s) in f.iter_mut().enumerate() {
                        *s = *fields.get(i).unwrap_or(&0);
                    }
                    time_string(&f)
                }
                HeapObj::Temporal { kind: 3, fields } => {
                    let g = |i: usize| *fields.get(i).unwrap_or(&0);
                    format!(
                        "{}T{}",
                        iso_date_string(g(0), g(1), g(2)),
                        time_string(&[g(3), g(4), g(5), g(6), g(7), g(8)])
                    )
                }
                HeapObj::Temporal { kind: 4, fields } => {
                    let ns = ((fields[0] as i128) << 64) | ((fields[1] as u64) as i128);
                    instant_to_string(ns)
                }
                HeapObj::Temporal { kind: 5, fields } => year_month_string(fields[0], fields[1]),
                HeapObj::Temporal { kind: 6, fields } => format!("{:02}-{:02}", fields[1], fields[2]),
                HeapObj::Temporal { .. } => "[object Temporal]".into(),
                HeapObj::Str(s) => s.bytes.clone(),
                HeapObj::Cons { .. } => {
                    let mut out = String::new();
                    self.heap.write_str(v.heap_index(), &mut out);
                    out
                }
                HeapObj::Func(_) | HeapObj::Closure { .. } | HeapObj::Bound { .. } | HeapObj::Native(_) => {
                    "function".into()
                }
                HeapObj::Cell(inner) => self.display(*inner),
                HeapObj::Array(items) => items
                    .iter()
                    .map(|e| if e.is_nullish() { String::new() } else { self.display(*e) })
                    .collect::<Vec<_>>()
                    .join(","),
                HeapObj::Object(_) => {
                    // Error instances ToString to "name: message" (Error.prototype.toString).
                    if self.is_error_instance(v.heap_index()) {
                        self.error_display_string(v.heap_index())
                    } else {
                        "[object Object]".into()
                    }
                }
                HeapObj::Class(c) => format!("class {} {{ }}", c.name),
                HeapObj::Map { .. } => "[object Map]".into(),
                HeapObj::Set(_) => "[object Set]".into(),
                HeapObj::WeakMap { .. } => "[object WeakMap]".into(),
                HeapObj::WeakSet(_) => "[object WeakSet]".into(),
                HeapObj::WeakRef(_) => "[object WeakRef]".into(),
                HeapObj::FinalizationRegistry { .. } => "[object FinalizationRegistry]".into(),
                HeapObj::Iterator { .. } => "[object Array Iterator]".into(),
                // A boxed primitive stringifies as its wrapped value (ToString).
                HeapObj::Boxed { value, .. } => self.display(*value),
                // ToString of a Symbol actually throws (see `to_js_string`); this
                // infallible debug form is "Symbol(desc)".
                HeapObj::Symbol { desc, .. } => {
                    let d = if *desc == Value::UNDEFINED { String::new() } else { self.display(*desc) };
                    format!("Symbol({d})")
                }
                // ToString(BigInt) is the decimal digits with NO "n" (String(1n) === "1").
                HeapObj::BigInt(n) => n.to_string(),
                HeapObj::RegExp { source, flags, .. } => {
                    let s = if source.is_empty() { "(?:)" } else { source };
                    format!("/{s}/{flags}")
                }
                // ToString of a TypedArray is the comma-joined elements (like Array).
                HeapObj::TypedArray { length, .. } => {
                    let n = *length;
                    let idx = v.heap_index();
                    (0..n).map(|i| self.ta_elem_string(idx, i)).collect::<Vec<_>>().join(",")
                }
                HeapObj::ArrayBuffer { .. } => "[object ArrayBuffer]".into(),
                HeapObj::DataView { .. } => "[object DataView]".into(),
                HeapObj::Generator { .. } => "[object Generator]".into(),
                HeapObj::AsyncGenerator(_) => "[object AsyncGenerator]".into(),
                HeapObj::Promise { .. } => "[object Promise]".into(),
                HeapObj::BoundResolver { .. } => "function".into(),
                // Internal: never user-visible (an async call yields its Promise).
                HeapObj::AsyncState(_) => "[object Promise]".into(),
                HeapObj::Combinator { .. } | HeapObj::CombinatorResolver { .. } => {
                    "[object Object]".into()
                }
                // `String(date)` / `"" + date` → the date string (ISO here).
                HeapObj::Date(ms) => {
                    if ms.is_nan() {
                        "Invalid Date".into()
                    } else {
                        date_to_iso(*ms)
                    }
                }
            }
        } else {
            "undefined".into()
        }
    }

    /// INSPECT (`console.log` rendering). Strings are quoted only when nested;
    /// arrays/objects use node's spaced bracket style (`[ 1, 2, 3 ]`,
    /// `{ a: 1 }`).
    fn inspect(&self, v: Value) -> String {
        if v.is_heap() {
            match self.heap.get(v.heap_index()) {
                HeapObj::Str(s) => return s.bytes.clone(), // top-level strings unquoted
                HeapObj::Cons { .. } => {
                    let mut out = String::new();
                    self.heap.write_str(v.heap_index(), &mut out);
                    return out;
                }
                _ => return self.inspect_nested(v),
            }
        }
        self.display(v)
    }

    /// `console.log` label for a function value: `[Function: name]`, or
    /// `[Function (anonymous)]` for an arrow / unnamed expression (synthetic
    /// names start with `<`). Class methods are stored as `Class.method`; show
    /// just the method part, as node does.
    fn func_label(&self, fid: u32) -> String {
        let name = &self.program.functions[fid as usize].name;
        if name.is_empty() || name.starts_with('<') {
            "[Function (anonymous)]".into()
        } else {
            let short = name.rsplit('.').next().unwrap_or(name);
            format!("[Function: {short}]")
        }
    }

    fn inspect_nested(&self, v: Value) -> String {
        if !v.is_heap() {
            return self.display(v);
        }
        match self.heap.get(v.heap_index()) {
            HeapObj::Proxy { target, .. } => {
                let t = *target;
                return self.inspect_nested(t);
            }
            HeapObj::Temporal { kind: 0, fields } => {
                let mut f = [0i64; 10];
                for (i, s) in f.iter_mut().enumerate() {
                    *s = *fields.get(i).unwrap_or(&0);
                }
                format!("Temporal.Duration <{}>", duration_to_string(&f))
            }
            HeapObj::Temporal { kind: 1, fields } => {
                format!("Temporal.PlainDate <{}>", iso_date_string(fields[0], fields[1], fields[2]))
            }
            HeapObj::Temporal { kind: 2, fields } => {
                let mut f = [0i64; 6];
                for (i, s) in f.iter_mut().enumerate() {
                    *s = *fields.get(i).unwrap_or(&0);
                }
                format!("Temporal.PlainTime <{}>", time_string(&f))
            }
            HeapObj::Temporal { kind: 3, fields } => {
                let g = |i: usize| *fields.get(i).unwrap_or(&0);
                format!(
                    "Temporal.PlainDateTime <{}T{}>",
                    iso_date_string(g(0), g(1), g(2)),
                    time_string(&[g(3), g(4), g(5), g(6), g(7), g(8)])
                )
            }
            HeapObj::Temporal { kind: 4, fields } => {
                let ns = ((fields[0] as i128) << 64) | ((fields[1] as u64) as i128);
                format!("Temporal.Instant <{}>", instant_to_string(ns))
            }
            HeapObj::Temporal { kind: 5, fields } => {
                format!("Temporal.PlainYearMonth <{}>", year_month_string(fields[0], fields[1]))
            }
            HeapObj::Temporal { kind: 6, fields } => {
                format!("Temporal.PlainMonthDay <{:02}-{:02}>", fields[1], fields[2])
            }
            HeapObj::Temporal { .. } => "[object Temporal]".into(),
            HeapObj::Str(s) => format!("'{}'", s.bytes),
            HeapObj::Cons { .. } => {
                let mut out = String::new();
                self.heap.write_str(v.heap_index(), &mut out);
                format!("'{out}'")
            }
            HeapObj::Func(id) => self.func_label(*id),
            HeapObj::Closure { func, .. } => self.func_label(*func),
            HeapObj::Bound { .. } => "[Function: bound]".into(),
            HeapObj::Native(_) => "[Function (native)]".into(),
            HeapObj::Cell(inner) => self.inspect_nested(*inner),
            HeapObj::Array(items) => {
                if items.is_empty() {
                    return "[]".into();
                }
                let parts: Vec<String> = items.iter().map(|e| self.inspect_nested(*e)).collect();
                format!("[ {} ]", parts.join(", "))
            }
            HeapObj::Object(map) => {
                // A class instance prints with its constructor name (`Pt { … }`).
                let prefix = match map.class {
                    Some(cidx) => match self.heap.get(cidx) {
                        HeapObj::Class(c) => format!("{} ", c.name),
                        _ => String::new(),
                    },
                    None => String::new(),
                };
                if map.keys.is_empty() {
                    return format!("{prefix}{{}}");
                }
                let parts: Vec<String> = map
                    .keys
                    .iter()
                    .zip(map.vals.iter())
                    .map(|(k, val)| format!("{k}: {}", self.inspect_nested(*val)))
                    .collect();
                format!("{prefix}{{ {} }}", parts.join(", "))
            }
            HeapObj::Class(c) => format!("[class {}]", c.name),
            HeapObj::Map { keys, vals } => {
                if keys.is_empty() {
                    return "Map(0) {}".into();
                }
                let parts: Vec<String> = keys
                    .iter()
                    .zip(vals.iter())
                    .map(|(k, v)| format!("{} => {}", self.inspect_nested(*k), self.inspect_nested(*v)))
                    .collect();
                format!("Map({}) {{ {} }}", keys.len(), parts.join(", "))
            }
            HeapObj::Set(items) => {
                if items.is_empty() {
                    return "Set(0) {}".into();
                }
                let parts: Vec<String> = items.iter().map(|v| self.inspect_nested(*v)).collect();
                format!("Set({}) {{ {} }}", items.len(), parts.join(", "))
            }
            HeapObj::WeakMap { .. } => "WeakMap { <items unknown> }".into(),
            HeapObj::WeakSet(_) => "WeakSet { <items unknown> }".into(),
            HeapObj::WeakRef(_) => "WeakRef {}".into(),
            HeapObj::FinalizationRegistry { .. } => "FinalizationRegistry {}".into(),
            HeapObj::Iterator { .. } => "Object [Array Iterator] {}".into(),
            HeapObj::Boxed { kind, value } => {
                let inner = self.inspect_nested(*value);
                match kind {
                    0 => format!("[String: {inner}]"),
                    1 => format!("[Number: {inner}]"),
                    _ => format!("[Boolean: {inner}]"),
                }
            }
            HeapObj::Symbol { desc, .. } => {
                let d = if *desc == Value::UNDEFINED { String::new() } else { self.display(*desc) };
                format!("Symbol({d})")
            }
            // console.log shows BigInt with the `n` suffix (1n), unlike ToString.
            HeapObj::BigInt(n) => format!("{n}n"),
            HeapObj::RegExp { source, flags, .. } => {
                let s = if source.is_empty() { "(?:)" } else { source };
                format!("/{s}/{flags}")
            }
            HeapObj::TypedArray { kind, length, .. } => {
                let (name, n, idx) = (native::TA_KINDS[*kind as usize].0, *length, v.heap_index());
                let parts: Vec<String> = (0..n).map(|i| self.ta_elem_string(idx, i)).collect();
                if n == 0 {
                    format!("{name}(0) []")
                } else {
                    format!("{name}({n}) [ {} ]", parts.join(", "))
                }
            }
            HeapObj::ArrayBuffer { data, .. } => format!("ArrayBuffer {{ byteLength: {} }}", data.len()),
            HeapObj::DataView { byte_length, .. } => {
                format!("DataView {{ byteLength: {byte_length} }}")
            }
            HeapObj::Generator { .. } => "Object [Generator] {}".into(),
            HeapObj::AsyncGenerator(_) => "Object [AsyncGenerator] {}".into(),
            HeapObj::Promise { state, result, .. } => match state {
                crate::heap::PromiseState::Pending => "Promise { <pending> }".into(),
                crate::heap::PromiseState::Fulfilled => {
                    format!("Promise {{ {} }}", self.inspect_nested(*result))
                }
                crate::heap::PromiseState::Rejected => {
                    format!("Promise {{ <rejected> {} }}", self.inspect_nested(*result))
                }
            },
            HeapObj::BoundResolver { .. } => "[Function (anonymous)]".into(),
            // Internal: never user-visible (an async call yields its Promise).
            HeapObj::AsyncState(_) => "Promise { <pending> }".into(),
            HeapObj::Combinator { .. } | HeapObj::CombinatorResolver { .. } => "[object Object]".into(),
            // node renders a Date in console.log as its ISO string (unquoted).
            HeapObj::Date(ms) => {
                if ms.is_nan() {
                    "Invalid Date".into()
                } else {
                    date_to_iso(*ms)
                }
            }
        }
    }

    /// Resolve a constant slot: most are plain Values; string constants are
    /// stored as a sentinel index into the function's `string_constants` and
    /// interned to a heap string on first use.
    #[inline]
    fn resolve_const(&mut self, func_id: u32, v: Value) -> Value {
        // String constants are encoded as `Value::heap(STRING_CONST_BIT | i)`.
        if v.is_heap() && (v.heap_index() & STRING_CONST_BIT) != 0 {
            let si = (v.heap_index() & !STRING_CONST_BIT) as usize;
            let s = self.program.functions[func_id as usize].string_constants[si].clone();
            return self.alloc_str(s);
        }
        v
    }
}

/// High bit of a heap index marks a "string constant pending interning" slot
/// in a `LoadConst` Value (see `resolve_const`). Real heap indices never set
/// this bit (the heap would need 2^31 objects).
pub const STRING_CONST_BIT: u32 = 0x8000_0000;

/// Per-function: which global slot the function's name binds to, if any. The
/// compiler stores it in `param_count`'s sibling — but to keep `FuncProto`
/// simple we encode it via a convention: a function whose name is hoisted to a
/// global has that slot recorded in a side table. For v1 the compiler sets it
/// through `FuncProto`-adjacent metadata; we read it here.
fn function_global_slot(f: &crate::bytecode::FuncProto) -> Option<u16> {
    f.name_global
}

/// Maximum native self-recursion depth before the JIT self-call helper deopts
/// to the interpreter (which continues on its EXPLICIT frame stack and enforces
/// MAX_FRAMES → catchable RangeError). This MUST stay well below what the native
/// Rust stack can hold, because each native self-call nests
/// `jit_self_call → JitFn::run → call helper → jit_self_call_impl → JitFn::run`
/// on the OS stack. 256 levels is safe on a default stack and is plenty to keep
/// realistic recursion (fib, etc.) native; deeper legal recursion transparently
/// continues on the interpreter (correct, just not JIT-accelerated past 256),
/// and runaway recursion deopts → interpreter → RangeError, never a segfault.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
const JIT_SELF_RECURSE_MAX: u32 = 256;

/// Public mirror of `JIT_SELF_RECURSE_MAX` for codegen's inline depth guard (the
/// native fast path compares `vm.jit_recurse_depth` against this before a direct
/// recursive call), kept identical so the inline guard and the slow path agree.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) const JIT_SELF_RECURSE_MAX_PUB: u32 = JIT_SELF_RECURSE_MAX;

/// Byte offset of `jit_recurse_depth` within `Vm`, for the JIT's inline
/// native→native self-call: the compiled code reads/bumps the counter directly
/// through the `vm` pointer (rdi) rather than crossing into Rust per recursive
/// call. Computed at compile time (verified to match the live field address
/// during bring-up).
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) const JIT_RECURSE_DEPTH_OFFSET: usize =
    core::mem::offset_of!(Vm<'static>, jit_recurse_depth);

/// Win64 helper for the slow/finish path of the JIT's inline native→native
/// self-call (see `jit_self_call_at_impl`). The native fast path tracks register
/// windows by raw pointer, so it passes its window base EXPLICITLY in
/// `caller_base_ptr` (the native `rbx`). `packed` carries `func_id` in the low 24
/// bits and `argc` in the high 8. Returns the result bits or `SELF_CALL_DEOPT`
/// (the activation threw — `pending_throw` is set, the native chain unwinds, and
/// the top-level interpreter re-raises it). ABI: rcx=vm, rdx=caller_base_ptr,
/// r8=args_ptr, r9=packed.
///
/// # Safety
/// `vm` is a valid `*mut Vm`; `caller_base_ptr` is the caller's window base
/// within `vm.regs`; `args` points to `argc` valid `Value` bits.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_self_call_at(
    vm: *mut core::ffi::c_void,
    caller_base_ptr: *const u64,
    args: *const u64,
    packed: u32,
) -> u64 {
    let func_id = packed & 0x00FF_FFFF;
    let argc = (packed >> 24) as usize;
    // Catch Rust panics at the FFI boundary (UB to unwind across `extern`).
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        let vm = &mut *(vm as *mut Vm);
        vm.jit_self_call_at_impl(func_id, caller_base_ptr, args, argc)
    }));
    match r {
        Ok(bits) => bits,
        Err(_) => crate::codegen::SELF_CALL_DEOPT,
    }
}

/// Win64 helper: the INLINE-CACHE MISS path for a JIT'd `GetProp`. The native
/// fast path (identity + version check, direct `vals[slot]` read) only calls this
/// when its cache misses. Looks up `obj.<key>`, and on the fast-path-eligible case
/// (a plain Object that HAS the key) fills inline-cache slot `site` with
/// `(obj_bits, vals.as_ptr(), version, slot)` so subsequent accesses are call-free.
/// Returns the property bits, or `SELF_CALL_DEOPT` (non-Object → interpreter
/// re-executes at this ip; arrays/strings/`.length`/null/undefined handled there).
/// A missing key on an Object returns `undefined` WITHOUT caching (rare).
/// `packed = (func_id << 32) | name_idx`.
///
/// # Safety
/// `vm` is a valid `*mut Vm`.
/// Win64 helper for a JIT'd dense-array element read `a[i]` (`GetIndex`).
/// Returns the element's Value bits; `undefined` bits for an in-bounds-checks-fail
/// (negative or `>= len`) index, matching JS `a[oob] === undefined`; or
/// `SELF_CALL_DEOPT` for a non-array receiver or a non-int key (string indexing,
/// `arr["foo"]`, etc.) so the interpreter re-executes this op. Read-only — no
/// caching needed (a dense array's element address is a direct `vals[i]`).
///
/// # Safety
/// `vm` is a valid `*mut Vm`.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_get_index(
    vm: *mut core::ffi::c_void,
    arr_bits: u64,
    key_bits: u64,
) -> u64 {
    let arr = Value::from_bits(arr_bits);
    let key = Value::from_bits(key_bits);
    // Only a numeric key on a heap object is handled here; a string/other key
    // (or non-heap receiver) deopts so the interpreter applies full semantics.
    if !arr.is_heap() || !key.is_number() {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    // SAFETY: read-only view; the running region holds no conflicting borrow.
    let vm = unsafe { &*(vm as *const Vm) };
    match vm.heap.get(arr.heap_index()) {
        HeapObj::Array(items) => match array_index(key) {
            // In range → the element; out of range / negative / non-integral →
            // undefined (matches JS and the interpreter's get_index).
            Some(i) if i < items.len() => items[i].bits(),
            _ => Value::UNDEFINED.bits(),
        },
        // Flat ASCII string `s[i]`: mirror the interpreter's get_index Str path
        // EXACTLY (vm.rs `get_index`, the `js.ascii` branch). The i-th char is
        // the i-th byte, and a single ASCII char is interned at heap index ==
        // its byte (Heap::new), so the result is that interned slot. In range →
        // that slot; out of range → undefined. Only the O(1)-and-identical
        // flat-ASCII case is handled; a non-ASCII string (char-walk) or a rope
        // `Cons` (must flatten first, a &mut op) deopts to the interpreter. A
        // negative/fractional/non-integer key (`array_index` → None) also defers
        // (the interpreter handles `s["length"]`, methods, etc.).
        HeapObj::Str(s) if s.ascii => match array_index(key) {
            Some(i) => match s.bytes.as_bytes().get(i) {
                Some(&b) => Value::heap(b as u32).bits(),
                None => Value::UNDEFINED.bits(),
            },
            None => crate::codegen::SELF_CALL_DEOPT,
        },
        _ => crate::codegen::SELF_CALL_DEOPT, // non-ASCII str / rope / other → interpreter
    }
}

/// Win64 helper for a JIT'd dense-array element write `a[i] = v` (`SetIndex`).
/// Stores in place when `i < len`, grows the array with `undefined` holes when
/// `i >= len` (matching JS and the interpreter's set_index). Returns `0` on
/// success, or `SELF_CALL_DEOPT` for a non-array receiver / negative / fractional
/// / non-numeric key (the interpreter then applies its no-op fallback). Reads the
/// live array fresh each call — no cached pointer, so a grow that reallocates is
/// safe (the region pins only the register file, never array storage).
///
/// # Safety
/// `vm` is a valid `*mut Vm`.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_set_index(
    vm: *mut core::ffi::c_void,
    arr_bits: u64,
    key_bits: u64,
    val_bits: u64,
) -> u64 {
    let arr = Value::from_bits(arr_bits);
    let key = Value::from_bits(key_bits);
    if !arr.is_heap() || !key.is_number() {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    let i = match array_index(key) {
        Some(i) => i,
        None => return crate::codegen::SELF_CALL_DEOPT, // negative/fractional → interpreter
    };
    // SAFETY: exclusive view; the running region holds no conflicting borrow and
    // pins only the register file (not the array's Vec, which may reallocate).
    let vm = unsafe { &mut *(vm as *mut Vm) };
    match vm.heap.get_mut(arr.heap_index()) {
        HeapObj::Array(items) => {
            let len = items.len();
            if i < len {
                items[i] = Value::from_bits(val_bits); // in-range store
            } else if i == len {
                items.push(Value::from_bits(val_bits)); // append (grow by one)
            } else {
                // A sparse write (i > len) would resize-with-holes — possibly a
                // huge allocation. Deopt so the INTERPRETER does the resize: its
                // panic on a giant/failed allocation unwinds through normal Rust,
                // not across this `extern "win64"` boundary (which would be UB).
                return crate::codegen::SELF_CALL_DEOPT;
            }
            0
        }
        _ => crate::codegen::SELF_CALL_DEOPT, // non-array → interpreter
    }
}

/// Win64 helper for a JIT'd `arr.push(x)` in a region. Appends and returns the
/// new length (Int bits), or `SELF_CALL_DEOPT` for a non-array receiver (the
/// interpreter then resolves the real method). Pins only the register file; the
/// array's Vec may reallocate — safe, no cached pointer.
///
/// # Safety
/// `vm` is a valid `*mut Vm`.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_array_push(
    vm: *mut core::ffi::c_void,
    arr_bits: u64,
    val_bits: u64,
) -> u64 {
    let arr = Value::from_bits(arr_bits);
    if !arr.is_heap() {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    // SAFETY: exclusive view; pins only the register file, not the array's Vec.
    let vm = unsafe { &mut *(vm as *mut Vm) };
    match vm.heap.get_mut(arr.heap_index()) {
        HeapObj::Array(items) => {
            items.push(Value::from_bits(val_bits));
            Value::int(items.len() as i32).bits()
        }
        _ => crate::codegen::SELF_CALL_DEOPT,
    }
}

/// Win64 helper for a JIT'd `str.charCodeAt(i)` in a region. Returns the UTF
/// scalar value (Int bits), NaN bits for an out-of-range index, or
/// `SELF_CALL_DEOPT` for a non-int index / non-flat-string receiver (a rope or
/// non-string → the interpreter, which flattens). O(1) for ASCII.
///
/// # Safety
/// `vm` is a valid `*mut Vm`.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_char_code_at(
    vm: *mut core::ffi::c_void,
    str_bits: u64,
    i_bits: u64,
) -> u64 {
    let sv = Value::from_bits(str_bits);
    let iv = Value::from_bits(i_bits);
    if !sv.is_heap() || !iv.is_number() {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    let i = match array_index(iv) {
        Some(i) => i,
        None => return crate::codegen::SELF_CALL_DEOPT, // negative/fractional
    };
    // SAFETY: read-only view; the running region holds no conflicting borrow.
    let vm = unsafe { &*(vm as *const Vm) };
    match vm.heap.get(sv.heap_index()) {
        HeapObj::Str(js) => {
            let ch = if js.ascii {
                js.bytes.as_bytes().get(i).map(|&b| b as char)
            } else {
                js.bytes.chars().nth(i)
            };
            match ch {
                Some(c) => Value::int(c as i32).bits(),
                None => Value::num(f64::NAN).bits(),
            }
        }
        _ => crate::codegen::SELF_CALL_DEOPT, // rope/non-string → interpreter
    }
}

/// `dst = a + b` for the OSR region's `StrConcat` op: the `+` operator (rope
/// concat or numeric add) on two boxed Values, returning the result bits. A
/// throwing coercion (only possible for exotic operands a `StrConcat` hint
/// shouldn't target) returns `SELF_CALL_DEOPT` so the region bails and the
/// interpreter redoes it (raising the throw properly).
///
/// # Safety
/// `vm` is a valid `*mut Vm`.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_concat(
    vm: *mut core::ffi::c_void,
    a_bits: u64,
    b_bits: u64,
) -> u64 {
    let a = Value::from_bits(a_bits);
    let b = Value::from_bits(b_bits);
    // SAFETY: exclusive view to allocate the rope node; the running region holds
    // no conflicting borrow (it touches only the reg file / globals base, and the
    // heap grows in a separate field).
    let vm = unsafe { &mut *(vm as *mut Vm) };
    match vm.add_values(a, b) {
        Ok(v) => v.bits(),
        Err(_) => crate::codegen::SELF_CALL_DEOPT,
    }
}

/// `dst = a + b` for the OSR region's `StrAppendInPlace` op: appends into `a`'s
/// buffer in place when uniquely owned (see `str_append_inplace`). Never deopts
/// (string append doesn't throw); always returns the result bits.
///
/// # Safety
/// `vm` is a valid `*mut Vm`.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_str_append(
    vm: *mut core::ffi::c_void,
    a_bits: u64,
    b_bits: u64,
) -> u64 {
    let a = Value::from_bits(a_bits);
    let b = Value::from_bits(b_bits);
    // SAFETY: exclusive view to mutate/allocate the string; the running region
    // holds no conflicting borrow (reg file / globals base only).
    let vm = unsafe { &mut *(vm as *mut Vm) };
    vm.str_append_inplace(a, b).bits()
}

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_get_prop_miss(
    vm: *mut core::ffi::c_void,
    obj_bits: u64,
    site_idx: u32,
    packed: u64,
) -> u64 {
    let obj = Value::from_bits(obj_bits);
    if !obj.is_heap() {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    // SAFETY: exclusive view (updates the IC table); the running region holds no
    // conflicting borrow (the IC table and the region live in different fields).
    let vm = unsafe { &mut *(vm as *mut Vm) };
    let func_id = (packed >> 32) as u32;
    let name_idx = packed as u32;
    let idx = obj.heap_index();
    let prog = vm.program; // &'p Program, independent of `vm`'s borrow
    let key = &prog.functions[func_id as usize].string_constants[name_idx as usize];
    let (val, vals_ptr, slot) = match vm.heap.get(idx) {
        HeapObj::Object(map) => match map.keys.iter().position(|k| k == key) {
            Some(s) => (map.vals[s], map.vals.as_ptr() as u64, s as u32),
            // Missing own key: a class instance may resolve it as a method, so
            // defer to the interpreter; a plain object yields undefined.
            None if map.class.is_some() => return crate::codegen::SELF_CALL_DEOPT,
            None => return Value::UNDEFINED.bits(),
        },
        // `arr.length` / `str.length` in a region: return the length WITHOUT
        // caching — it's derived from the container's element count, not a fixed
        // slot, so a stale cache would be wrong after the container grows. The IC
        // entry stays unset, so this site simply misses (helper call) each time —
        // cheap, and it lets a `for (i < a.length) a[i]` loop run as a region
        // instead of bailing on the first `.length` access.
        HeapObj::Array(items) if key == "length" => return len_value(items.len()).bits(),
        HeapObj::Str(s) if key == "length" => return len_value(s.char_len).bits(),
        HeapObj::Cons { len, .. } if key == "length" => return len_value(*len).bits(),
        _ => return crate::codegen::SELF_CALL_DEOPT, // other array/string props → interpreter
    };
    let version = vm.heap.version_of(idx);
    vm.jit.set_ic(site_idx, obj_bits, vals_ptr, version, slot);
    val.bits()
}

/// Win64 helper: the INLINE-CACHE MISS path for a JIT'd `SetProp`. Performs
/// `obj.<key> = val`, then (for a plain Object) fills inline-cache slot `site` so
/// later writes are call-free. Returns `0` (success — incl. a heap non-Object,
/// which no-ops, matching the interpreter) or `SELF_CALL_DEOPT` (null/undefined →
/// the interpreter throws). `packed = (func_id << 32) | name_idx`; `site_idx` is
/// the 5th argument (passed on the stack by the caller).
///
/// # Safety
/// `vm` is a valid `*mut Vm`.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_set_prop_miss(
    vm: *mut core::ffi::c_void,
    obj_bits: u64,
    val_bits: u64,
    packed: u64,
    site_idx: u32,
) -> u64 {
    let obj = Value::from_bits(obj_bits);
    if !obj.is_heap() {
        return crate::codegen::SELF_CALL_DEOPT;
    }
    let vm = unsafe { &mut *(vm as *mut Vm) };
    let func_id = (packed >> 32) as u32;
    let name_idx = packed as u32;
    let idx = obj.heap_index();
    let prog = vm.program;
    let key = &prog.functions[func_id as usize].string_constants[name_idx as usize];
    let (added, vals_ptr, slot) = match vm.heap.get_mut(idx) {
        HeapObj::Object(map) => {
            let added = map.set(key, Value::from_bits(val_bits));
            // Position AFTER the set (existing key: unchanged; new key: appended).
            let s = map.keys.iter().position(|k| k == key).unwrap() as u32;
            (added, map.vals.as_ptr() as u64, s)
        }
        // `arr.length = n` truncates/grows — deopt so the interpreter's set_prop
        // applies it (no-op here would diverge from the interpreter).
        HeapObj::Array(_) if key == "length" => return crate::codegen::SELF_CALL_DEOPT,
        _ => return 0, // other heap non-Object props: silent no-op (matches interpreter)
    };
    if added {
        vm.heap.bump_version(idx);
    }
    let version = vm.heap.version_of(idx);
    vm.jit.set_ic(site_idx, obj_bits, vals_ptr, version, slot);
    0
}

/// Win64 helper: base pointer of the heap's per-object version array, pinned by a
/// heap-op region's prologue. Stable for the run (a region never allocates a heap
/// object, so the array doesn't reallocate).
///
/// # Safety
/// `vm` is a valid `*mut Vm`.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_heap_versions_base(vm: *mut core::ffi::c_void) -> *const u32 {
    let vm = unsafe { &*(vm as *const Vm) };
    vm.heap.versions_ptr()
}

/// Win64 helper: base pointer of the JIT inline-cache table, pinned by a heap-op
/// region's prologue. Stable for the run (the table grows only at compile time,
/// and a `*_miss` only updates an existing slot — never grows it).
///
/// # Safety
/// `vm` is a valid `*mut Vm`.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_ic_base(vm: *mut core::ffi::c_void) -> *const core::ffi::c_void {
    let vm = unsafe { &*(vm as *const Vm) };
    vm.jit.ic_base_ptr() as *const core::ffi::c_void
}

/// Win64 helper: the base pointer of `vm.globals`, fetched once by an OSR loop
/// region's prologue and pinned in a callee-saved register for direct
/// `LoadGlobal`/`StoreGlobal`. Sound because `globals` is allocated once at VM
/// construction (`global_count` slots) and never reallocates at runtime.
///
/// # Safety
/// `vm` is a valid `*mut Vm` that outlives the region run.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) extern "win64" fn jit_globals_base(vm: *mut core::ffi::c_void) -> *mut u64 {
    let vm = unsafe { &mut *(vm as *mut Vm) };
    vm.globals.as_mut_ptr() as *mut u64
}

/// Normalise a (possibly negative) slice index into `[0, len]`. Negative
/// indices count from the end; out-of-range clamps. Matches JS slice/substring.
fn norm_index(i: i32, len: i32) -> i32 {
    let v = if i < 0 { len + i } else { i };
    v.clamp(0, len)
}

/// A `.length` / array-length result as a JS Number. An `Int` when it fits in
/// i32 (the overwhelmingly common case), otherwise a double — so a length beyond
/// 2^31 (cheap to reach now that ropes concatenate lazily without flattening)
/// reports its true magnitude instead of wrapping negative through `as i32`.
/// Integers up to 2^53 are exact in f64, matching JS.
#[inline]
/// A class private name is stored internally as the property "#name". Such keys
/// are NOT reflectable own properties (hidden from getOwnPropertyNames, keys,
/// for-in, hasOwnProperty, getOwnPropertyDescriptor) even though field/method
/// access reads them directly.
fn is_private_key(k: &str) -> bool {
    k.starts_with('#')
}

/// BigInt binary operations (see `bigint_binop`).
#[derive(Clone, Copy)]
#[allow(dead_code)] // `Add` is handled inline in `add_values` (string-concat fallthrough)
enum BigOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Pow,
    And,
    Or,
    Xor,
    Shl,
    Shr,
}

/// Parse a BigInt string: optional sign + decimal, or a `0x`/`0o`/`0b` prefix.
/// `None` ⇒ not a valid BigInt literal (→ SyntaxError at the call site).
fn parse_bigint_str(s: &str) -> Option<i128> {
    let s = s.trim();
    let (neg, body) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let v: i128 = if let Some(h) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
        i128::from_str_radix(h, 16).ok()?
    } else if let Some(o) = body.strip_prefix("0o").or_else(|| body.strip_prefix("0O")) {
        i128::from_str_radix(o, 8).ok()?
    } else if let Some(b) = body.strip_prefix("0b").or_else(|| body.strip_prefix("0B")) {
        i128::from_str_radix(b, 2).ok()?
    } else {
        body.parse::<i128>().ok()?
    };
    Some(if neg { -v } else { v })
}

fn is_leap_year(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}
fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap_year(y) {
                29
            } else {
                28
            }
        }
        _ => 0,
    }
}
/// Days since 1970-01-01 for an ISO date (Howard Hinnant's days_from_civil).
fn iso_to_epoch_days(y: i64, m: i64, d: i64) -> i64 {
    let yy = if m <= 2 { y - 1 } else { y };
    let era = (if yy >= 0 { yy } else { yy - 399 }) / 400;
    let yoe = yy - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}
fn epoch_days_to_iso(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}
/// ISO-8601 week-of-year (weeks belong to the year holding their Thursday).
fn iso_week_of_year(y: i64, m: i64, d: i64) -> i64 {
    let doy = iso_to_epoch_days(y, m, d) - iso_to_epoch_days(y, 1, 1) + 1;
    let dow = iso_day_of_week(y, m, d);
    let week = (doy - dow + 10) / 7;
    if week < 1 {
        return iso_week_of_year(y - 1, 12, 31);
    }
    if week == 53 {
        let jan1 = iso_day_of_week(y, 1, 1);
        let has53 = jan1 == 4 || (is_leap_year(y) && jan1 == 3);
        if !has53 {
            return 1;
        }
    }
    week
}

/// Nanoseconds-since-midnight for a [h,mi,s,ms,us,ns] time.
fn time_to_ns(f: &[i64; 6]) -> i128 {
    (f[0] as i128) * 3_600_000_000_000
        + (f[1] as i128) * 60_000_000_000
        + (f[2] as i128) * 1_000_000_000
        + (f[3] as i128) * 1_000_000
        + (f[4] as i128) * 1_000
        + (f[5] as i128)
}
/// Decompose nanoseconds-since-midnight into [h,mi,s,ms,us,ns].
fn ns_to_time(mut ns: i128) -> [i64; 6] {
    let h = (ns / 3_600_000_000_000) as i64;
    ns %= 3_600_000_000_000;
    let mi = (ns / 60_000_000_000) as i64;
    ns %= 60_000_000_000;
    let s = (ns / 1_000_000_000) as i64;
    ns %= 1_000_000_000;
    let ms = (ns / 1_000_000) as i64;
    ns %= 1_000_000;
    let us = (ns / 1_000) as i64;
    let nss = (ns % 1_000) as i64;
    [h, mi, s, ms, us, nss]
}
/// "HH:MM:SS" with a trimmed fractional-seconds part when sub-second fields exist.
fn time_string(f: &[i64; 6]) -> String {
    let sub = f[3] * 1_000_000 + f[4] * 1_000 + f[5];
    let base = format!("{:02}:{:02}:{:02}", f[0], f[1], f[2]);
    if sub == 0 {
        base
    } else {
        let frac = format!("{sub:09}");
        format!("{base}.{}", frac.trim_end_matches('0'))
    }
}
/// Parse "HH:MM[:SS[.fff]]" (separators optional) → [h,mi,s,ms,us,ns].
fn parse_iso_time(s: &str) -> Option<[i64; 6]> {
    let s = s.trim();
    // Allow a leading "T".
    let s = s.strip_prefix(['T', 't']).unwrap_or(s);
    let digits: Vec<char> = s.chars().collect();
    let take2 = |i: usize| -> Option<i64> {
        if i + 1 < digits.len() && digits[i].is_ascii_digit() && digits[i + 1].is_ascii_digit() {
            format!("{}{}", digits[i], digits[i + 1]).parse().ok()
        } else {
            None
        }
    };
    let h = take2(0)?;
    // minute after optional ':'
    let mut i = 2;
    if digits.get(i) == Some(&':') {
        i += 1;
    }
    let mi = take2(i).unwrap_or(0);
    i += 2;
    let mut sec = 0i64;
    let mut sub = [0i64; 3];
    if digits.get(i) == Some(&':') || digits.get(i).is_some_and(|c| c.is_ascii_digit()) {
        if digits.get(i) == Some(&':') {
            i += 1;
        }
        sec = take2(i).unwrap_or(0);
        i += 2;
        if digits.get(i) == Some(&'.') || digits.get(i) == Some(&',') {
            i += 1;
            let mut fr = String::new();
            while let Some(c) = digits.get(i) {
                if c.is_ascii_digit() {
                    fr.push(*c);
                    i += 1;
                } else {
                    break;
                }
            }
            while fr.len() < 9 {
                fr.push('0');
            }
            fr.truncate(9);
            let ns: i64 = fr.parse().ok()?;
            sub = [ns / 1_000_000, (ns / 1_000) % 1_000, ns % 1_000];
        }
    }
    if !(0..24).contains(&h) || !(0..60).contains(&mi) || !(0..60).contains(&sec) {
        return None;
    }
    Some([h, mi, sec, sub[0], sub[1], sub[2]])
}

/// Parse "YYYY-MM-DD[THH:MM:SS.fff]" → [y,mo,d,h,mi,s,ms,us,ns] (time defaults 0).
fn parse_iso_datetime(s: &str) -> Option<[i64; 9]> {
    let s = s.trim();
    let (date_s, time_s) = match s.find(['T', 't']) {
        Some(i) => (&s[..i], Some(&s[i + 1..])),
        None => match s.find(' ') {
            Some(i) => (&s[..i], Some(&s[i + 1..])),
            None => (s, None),
        },
    };
    let (y, mo, d) = parse_iso_date(date_s)?;
    let t = match time_s {
        Some(ts) if !ts.is_empty() => parse_iso_time(ts)?,
        _ => [0; 6],
    };
    Some([y, mo, d, t[0], t[1], t[2], t[3], t[4], t[5]])
}

/// Nanoseconds in a day.
const DAY_NS: i128 = 86_400_000_000_000;

/// Epoch-nanoseconds → "YYYY-MM-DDTHH:MM:SSZ" (UTC).
fn instant_to_string(ns: i128) -> String {
    let days = ns.div_euclid(DAY_NS) as i64;
    let rem = ns.rem_euclid(DAY_NS);
    let (y, m, d) = epoch_days_to_iso(days);
    let t = ns_to_time(rem);
    // Instant.toString always shows whole seconds (sub-second only if present).
    let base = format!("{:02}:{:02}:{:02}", t[0], t[1], t[2]);
    let sub = t[3] * 1_000_000 + t[4] * 1_000 + t[5];
    let time = if sub == 0 {
        base
    } else {
        let frac = format!("{sub:09}");
        format!("{base}.{}", frac.trim_end_matches('0'))
    };
    format!("{}T{}Z", iso_date_string(y, m, d), time)
}

/// Parse "+HH:MM"/"-HH:MM"/"+HHMM"/"Z" UTC offset → nanoseconds (Z → 0).
fn parse_offset_ns(s: &str) -> Option<i128> {
    if matches!(s, "Z" | "z") {
        return Some(0);
    }
    let sign: i128 = match s.as_bytes().first() {
        Some(b'+') => 1,
        Some(b'-') => -1,
        _ => return None,
    };
    let body = &s[1..];
    let digits: String = body.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() < 2 {
        return None;
    }
    let h: i128 = digits[..2].parse().ok()?;
    let mi: i128 = if digits.len() >= 4 { digits[2..4].parse().ok()? } else { 0 };
    Some(sign * (h * 3_600_000_000_000 + mi * 60_000_000_000))
}

/// Parse an ISO instant string ("…Z" or "…±HH:MM") → epoch nanoseconds (UTC).
fn instant_str_to_ns(s: &str) -> Option<i128> {
    let s = s.trim();
    let tpos = s.find(['T', 't'])?;
    let after_t = &s[tpos + 1..];
    // Locate the offset/Z that ends the time part.
    let (dt_part, off): (&str, Option<&str>) = if let Some(z) = after_t.find(['Z', 'z']) {
        (&s[..tpos + 1 + z], Some("Z"))
    } else if let Some(rel) = after_t.find('+') {
        (&s[..tpos + 1 + rel], Some(&after_t[rel..]))
    } else if let Some(rel) = after_t.find('-') {
        (&s[..tpos + 1 + rel], Some(&after_t[rel..]))
    } else {
        return None; // an Instant string must carry a UTC designator
    };
    let dt = parse_iso_datetime(dt_part)?;
    let mut ns = (iso_to_epoch_days(dt[0], dt[1], dt[2]) as i128) * DAY_NS
        + time_to_ns(&[dt[3], dt[4], dt[5], dt[6], dt[7], dt[8]]);
    ns -= parse_offset_ns(off?)?;
    Some(ns)
}

/// "YYYY-MM-DD" (expanded ±YYYYYY for years outside 0..9999).
fn iso_date_string(y: i64, m: i64, d: i64) -> String {
    let ys = if (0..=9999).contains(&y) {
        format!("{y:04}")
    } else {
        format!("{y:+07}")
    };
    format!("{ys}-{m:02}-{d:02}")
}

/// "YYYY-MM" (expanded-year aware) — Temporal.PlainYearMonth serialization.
fn year_month_string(y: i64, m: i64) -> String {
    let ys = if (0..=9999).contains(&y) {
        format!("{y:04}")
    } else {
        format!("{y:+07}")
    };
    format!("{ys}-{m:02}")
}

/// Parse a month code like "M06" (ISO calendars have no leap months) → 1..=12.
fn parse_month_code(s: &str) -> Option<i64> {
    let body = s.strip_prefix('M')?;
    let body = body.strip_suffix('L').unwrap_or(body);
    let n = body.parse::<i64>().ok()?;
    (1..=12).contains(&n).then_some(n)
}

/// Parse "YYYY-MM" (or a fuller ISO date) → (year, month, referenceISODay).
fn parse_iso_year_month(s: &str) -> Option<(i64, i64, i64)> {
    let s = s.trim();
    if let Some((y, m, d)) = parse_iso_date(s) {
        return Some((y, m, d));
    }
    let bytes = s.as_bytes();
    let (sign, rest) = match bytes.first() {
        Some(b'-') | Some(b'+') => (if bytes[0] == b'-' { -1i64 } else { 1 }, &s[1..]),
        _ => (1, s),
    };
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    let ylen = if digits.len() >= 6 { 6 } else { 4 };
    if digits.len() < ylen {
        return None;
    }
    let y = sign * rest[..ylen].parse::<i64>().ok()?;
    let after = &rest[ylen..];
    let after = after.strip_prefix('-').unwrap_or(after);
    if after.len() < 2 {
        return None;
    }
    let m = after[..2].parse::<i64>().ok()?;
    if !(1..=12).contains(&m) {
        return None;
    }
    Some((y, m, 1))
}

/// Parse "MM-DD" / "--MM-DD" (or a fuller ISO date) → (referenceISOYear, month, day).
fn parse_iso_month_day(s: &str) -> Option<(i64, i64, i64)> {
    let s = s.trim();
    if let Some((y, m, d)) = parse_iso_date(s) {
        return Some((y, m, d));
    }
    let body = s.strip_prefix("--").unwrap_or(s);
    if body.len() < 4 {
        return None;
    }
    let m = body.get(..2)?.parse::<i64>().ok()?;
    let after = &body[2..];
    let after = after.strip_prefix('-').unwrap_or(after);
    if after.len() < 2 {
        return None;
    }
    let d = after[..2].parse::<i64>().ok()?;
    if !(1..=12).contains(&m) || d < 1 || d > days_in_month(1972, m) {
        return None;
    }
    Some((1972, m, d))
}

/// ISO day-of-week: Monday=1 … Sunday=7.
fn iso_day_of_week(y: i64, m: i64, d: i64) -> i64 {
    let ed = iso_to_epoch_days(y, m, d);
    (((ed % 7) + 3) % 7 + 7) % 7 + 1
}
/// Parse "YYYY-MM-DD" (optionally with time/zone/calendar suffix) → (y,m,d).
fn parse_iso_date(s: &str) -> Option<(i64, i64, i64)> {
    let s = s.trim();
    // Optional leading sign for expanded years (±YYYYYY).
    let bytes = s.as_bytes();
    let (sign, rest) = match bytes.first() {
        Some(b'-') | Some(b'+') => (if bytes[0] == b'-' { -1i64 } else { 1 }, &s[1..]),
        _ => (1, s),
    };
    // Year: 4 digits (or 6 for expanded). Then "-MM-DD" (separators optional).
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    let ylen = if digits.len() >= 6 { 6 } else { 4 };
    if digits.len() < ylen {
        return None;
    }
    let y = sign * rest[..ylen].parse::<i64>().ok()?;
    let after = &rest[ylen..];
    let after = after.strip_prefix('-').unwrap_or(after);
    if after.len() < 2 {
        return None;
    }
    let m = after[..2].parse::<i64>().ok()?;
    let after = &after[2..];
    let after = after.strip_prefix('-').unwrap_or(after);
    if after.len() < 2 {
        return None;
    }
    let d = after[..2].parse::<i64>().ok()?;
    if !(1..=12).contains(&m) || d < 1 || d > days_in_month(y, m) {
        return None;
    }
    Some((y, m, d))
}

/// ISO-8601 serialization of a Temporal.Duration (`P1Y2M3DT4H5.5S`). ms/us/ns
/// fold into fractional seconds. All-zero → "PT0S".
fn duration_to_string(f: &[i64; 10]) -> String {
    let sign = f.iter().map(|x| x.signum()).find(|&s| s != 0).unwrap_or(0);
    let a: Vec<i128> = f.iter().map(|x| (*x as i128).abs()).collect();
    let (y, mo, w, d, h, mi) = (a[0], a[1], a[2], a[3], a[4], a[5]);
    let total_ns = a[6] * 1_000_000_000 + a[7] * 1_000_000 + a[8] * 1_000 + a[9];
    let whole_s = total_ns / 1_000_000_000;
    let frac_ns = (total_ns % 1_000_000_000) as u64;
    let mut out = String::new();
    if sign < 0 {
        out.push('-');
    }
    out.push('P');
    if y != 0 {
        out.push_str(&format!("{y}Y"));
    }
    if mo != 0 {
        out.push_str(&format!("{mo}M"));
    }
    if w != 0 {
        out.push_str(&format!("{w}W"));
    }
    if d != 0 {
        out.push_str(&format!("{d}D"));
    }
    let has_time = h != 0 || mi != 0 || whole_s != 0 || frac_ns != 0;
    if has_time {
        out.push('T');
        if h != 0 {
            out.push_str(&format!("{h}H"));
        }
        if mi != 0 {
            out.push_str(&format!("{mi}M"));
        }
        if whole_s != 0 || frac_ns != 0 {
            if frac_ns == 0 {
                out.push_str(&format!("{whole_s}S"));
            } else {
                let frac = format!("{frac_ns:09}");
                out.push_str(&format!("{whole_s}.{}S", frac.trim_end_matches('0')));
            }
        }
    }
    if out == "P" || out == "-P" {
        return "PT0S".to_string();
    }
    out
}

/// Parse an ISO-8601 duration string into `[y,mo,w,d,h,mi,s,ms,us,ns]`. Handles
/// integer date/time units and a fractional seconds field. `None` if malformed.
fn parse_iso_duration(s: &str) -> Option<[i64; 10]> {
    let s = s.trim();
    let (sign, rest) = match s.strip_prefix('-') {
        Some(r) => (-1i64, r),
        None => (1, s.strip_prefix('+').unwrap_or(s)),
    };
    let rest = rest.strip_prefix(['P', 'p'])?;
    let mut f = [0i64; 10];
    let (date_s, time_s) = match rest.find(['T', 't']) {
        Some(i) => (&rest[..i], &rest[i + 1..]),
        None => (rest, ""),
    };
    let mut saw = false;
    // Date units Y/M/W/D.
    let mut num = String::new();
    for c in date_s.chars() {
        if c.is_ascii_digit() {
            num.push(c);
        } else {
            let n: i64 = num.parse().ok()?;
            num.clear();
            let slot = match c {
                'Y' | 'y' => 0,
                'M' => 1,
                'W' | 'w' => 2,
                'D' | 'd' => 3,
                _ => return None,
            };
            f[slot] = n;
            saw = true;
        }
    }
    if !num.is_empty() {
        return None;
    }
    // Time units H/M/S (S may have a fraction → ms/us/ns).
    if !time_s.is_empty() {
        let mut num = String::new();
        let mut frac = String::new();
        let mut in_frac = false;
        for c in time_s.chars() {
            if c.is_ascii_digit() {
                if in_frac {
                    frac.push(c);
                } else {
                    num.push(c);
                }
            } else if c == '.' || c == ',' {
                in_frac = true;
            } else {
                let n: i64 = num.parse().ok()?;
                let slot = match c {
                    'H' | 'h' => 4,
                    'M' => 5,
                    'S' | 's' => 6,
                    _ => return None,
                };
                f[slot] = n;
                saw = true;
                if !frac.is_empty() {
                    if !matches!(c, 'S' | 's') {
                        return None; // only seconds-fraction supported
                    }
                    let mut fr = frac.clone();
                    while fr.len() < 9 {
                        fr.push('0');
                    }
                    fr.truncate(9);
                    let ns: i64 = fr.parse().ok()?;
                    f[7] = ns / 1_000_000;
                    f[8] = (ns / 1_000) % 1_000;
                    f[9] = ns % 1_000;
                }
                num.clear();
                frac.clear();
                in_frac = false;
            }
        }
        if !num.is_empty() || in_frac {
            return None;
        }
    }
    if !saw {
        return None;
    }
    if sign < 0 {
        for x in f.iter_mut() {
            *x = -*x;
        }
    }
    Some(f)
}

/// Encode `f` (already ToNumber'd) into a TypedArray element's little-endian
/// bytes per the element `kind` (JS ToInt8/ToUint8/clamp/… modular reduction;
/// Rust's `as` saturates, so reduce via `rem_euclid` first). BigInt kinds are
/// encoded by the caller.
fn ta_encode(kind: u8, f: f64) -> [u8; 8] {
    let mut out = [0u8; 8];
    match kind {
        0 | 1 => out[0] = to_uint_modular(f, 8) as u8,
        2 => out[0] = clamp_u8(f),
        3 | 4 => out[..2].copy_from_slice(&(to_uint_modular(f, 16) as u16).to_le_bytes()),
        5 | 6 => out[..4].copy_from_slice(&(to_uint_modular(f, 32) as u32).to_le_bytes()),
        7 => out[..4].copy_from_slice(&(f as f32).to_le_bytes()),
        8 => out.copy_from_slice(&f.to_le_bytes()),
        _ => {}
    }
    out
}

/// JS ToUintN modular reduction (the low `bits` bits of trunc(f)), NaN/±∞ → 0.
fn to_uint_modular(f: f64, bits: u32) -> u64 {
    if !f.is_finite() {
        return 0;
    }
    let m = 2f64.powi(bits as i32);
    f.trunc().rem_euclid(m) as u64
}

/// JS ToUint8Clamp: clamp to [0,255] with round-half-to-even.
fn clamp_u8(f: f64) -> u8 {
    if f.is_nan() || f <= 0.0 {
        return 0;
    }
    if f >= 255.0 {
        return 255;
    }
    let fl = f.floor();
    let diff = f - fl;
    let r = if diff < 0.5 {
        fl
    } else if diff > 0.5 {
        fl + 1.0
    } else if (fl as u64) % 2 == 0 {
        fl
    } else {
        fl + 1.0
    };
    r as u8
}

/// Convert a byte offset into `s` to a char offset (regress reports byte offsets;
/// our string indexing is char-based). Identity for ASCII.
fn byte_to_char(s: &str, byte: usize) -> usize {
    let b = byte.min(s.len());
    s[..b].chars().count()
}

/// Convert a char offset into `s` to a byte offset (for seeking regress).
fn char_to_byte(s: &str, ch: usize) -> usize {
    s.char_indices().nth(ch).map(|(b, _)| b).unwrap_or(s.len())
}

/// Format an i128 BigInt in the given radix (2..=36), lowercase digits.
fn bigint_to_radix(n: i128, radix: u32) -> String {
    if radix == 10 {
        return n.to_string();
    }
    if n == 0 {
        return "0".to_string();
    }
    let neg = n < 0;
    let mut m = (n as i128).unsigned_abs();
    let r = radix as u128;
    let mut digits = Vec::new();
    while m > 0 {
        let d = (m % r) as u32;
        digits.push(std::char::from_digit(d, radix).unwrap());
        m /= r;
    }
    if neg {
        digits.push('-');
    }
    digits.iter().rev().collect()
}

/// `BigInt.asUintN(bits, x)`: x mod 2^bits as a non-negative value (i128-limited).
fn bigint_as_uintn(bits: u32, x: i128) -> i128 {
    if bits == 0 {
        return 0;
    }
    if bits >= 127 {
        return x; // beyond the i128 representable mask — pass through (approx)
    }
    x & ((1i128 << bits) - 1)
}

/// `BigInt.asIntN(bits, x)`: x mod 2^bits as a signed bits-bit value.
fn bigint_as_intn(bits: u32, x: i128) -> i128 {
    if bits == 0 {
        return 0;
    }
    if bits >= 127 {
        return x;
    }
    let m = x & ((1i128 << bits) - 1);
    let half = 1i128 << (bits - 1);
    if m >= half {
        m - (1i128 << bits)
    } else {
        m
    }
}

#[inline]
/// A key hidden from STRING enumeration (for-in, Object.keys/values/entries,
/// getOwnPropertyNames, JSON): a private name (`#name`) or a symbol's internal
/// key (`@@iterator`, `@@sym:N`). Symbol keys are still reachable by
/// getOwnPropertyDescriptor and surfaced by getOwnPropertySymbols.
fn is_hidden_key(k: &str) -> bool {
    k.starts_with('#') || k.starts_with("@@")
}

fn len_value(n: usize) -> Value {
    if n <= i32::MAX as usize {
        Value::int(n as i32)
    } else {
        Value::num(n as f64)
    }
}

/// JS `parseInt(s, radix)`: skip leading whitespace, an optional sign, an
/// optional `0x` prefix (radix 16), then digits in `radix` (default 10); stop at
/// the first invalid digit. `NaN` if no digits parse. `radix == 0` means "auto".
fn parse_int(s: &str, radix: i32) -> f64 {
    let b = s.trim_start().as_bytes();
    let mut i = 0;
    let mut sign = 1.0;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        if b[i] == b'-' {
            sign = -1.0;
        }
        i += 1;
    }
    let mut radix = radix;
    if (radix == 16 || radix == 0)
        && i + 1 < b.len()
        && b[i] == b'0'
        && (b[i + 1] == b'x' || b[i + 1] == b'X')
    {
        i += 2;
        radix = 16;
    }
    if radix == 0 {
        radix = 10;
    }
    if !(2..=36).contains(&radix) {
        return f64::NAN;
    }
    let start = i;
    let mut val = 0.0;
    while i < b.len() {
        let d = match b[i] {
            c @ b'0'..=b'9' => (c - b'0') as i32,
            c @ b'a'..=b'z' => (c - b'a' + 10) as i32,
            c @ b'A'..=b'Z' => (c - b'A' + 10) as i32,
            _ => break,
        };
        if d >= radix {
            break;
        }
        val = val * radix as f64 + d as f64;
        i += 1;
    }
    if i == start {
        f64::NAN
    } else {
        sign * val
    }
}

/// JS `parseFloat(s)`: skip leading whitespace, then parse the longest leading
/// decimal-float prefix (sign, digits, `.`, exponent, or `Infinity`). `NaN` if
/// none.
fn parse_float(s: &str) -> f64 {
    let t = s.trim_start();
    let b = t.as_bytes();
    let mut end = 0;
    if end < b.len() && (b[end] == b'+' || b[end] == b'-') {
        end += 1;
    }
    if t[end..].starts_with("Infinity") {
        return if t.starts_with('-') { f64::NEG_INFINITY } else { f64::INFINITY };
    }
    let mut saw_digit = false;
    while end < b.len() && b[end].is_ascii_digit() {
        end += 1;
        saw_digit = true;
    }
    if end < b.len() && b[end] == b'.' {
        end += 1;
        while end < b.len() && b[end].is_ascii_digit() {
            end += 1;
            saw_digit = true;
        }
    }
    if !saw_digit {
        return f64::NAN;
    }
    // Optional exponent — only consumed if it has at least one digit.
    if end < b.len() && (b[end] == b'e' || b[end] == b'E') {
        let mut e = end + 1;
        if e < b.len() && (b[e] == b'+' || b[e] == b'-') {
            e += 1;
        }
        let exp_start = e;
        while e < b.len() && b[e].is_ascii_digit() {
            e += 1;
        }
        if e > exp_start {
            end = e;
        }
    }
    t[..end].parse::<f64>().unwrap_or(f64::NAN)
}

/// A non-negative array index from a numeric key, coercing an integral double
/// the way JS does (`a[1.0]` is `a[1]`). `None` for a negative, non-integral, or
/// non-numeric key (those address no dense element → `undefined`). The JIT region
/// computes loop counters as f64, so `a[i]` arrives here with a double key.
#[inline]
fn array_index(key: Value) -> Option<usize> {
    if key.is_int() {
        let i = key.as_int();
        (i >= 0).then_some(i as usize)
    } else if key.is_double() {
        let d = key.as_f64();
        // Reject negatives, fractions, and absurdly large indices (≥ 2^32).
        if d >= 0.0 && d.fract() == 0.0 && d < 4_294_967_296.0 {
            Some(d as usize)
        } else {
            None
        }
    } else {
        None
    }
}

/// Quote a string as a JSON string literal (escaping per the JSON spec).
fn json_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{0008}' => out.push_str("\\b"),
            '\u{000c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn json_skip_ws(b: &[u8], i: &mut usize) {
    while matches!(b.get(*i), Some(b' ' | b'\t' | b'\n' | b'\r')) {
        *i += 1;
    }
}

/// Match a literal `word` (true/false/null) at `*i`, advancing past it.
fn json_expect(b: &[u8], i: &mut usize, word: &str) -> Result<(), Thrown> {
    if b[*i..].starts_with(word.as_bytes()) {
        *i += word.len();
        Ok(())
    } else {
        Err(Thrown("SyntaxError: Unexpected token in JSON".into()))
    }
}

/// Read exactly 4 hex digits at `pos` as a code unit.
fn json_hex4(b: &[u8], pos: usize) -> Result<u32, Thrown> {
    if pos + 4 > b.len() {
        return Err(Thrown("SyntaxError: Bad unicode escape in JSON".into()));
    }
    let mut v = 0u32;
    for k in 0..4 {
        let d = match b[pos + k] {
            c @ b'0'..=b'9' => (c - b'0') as u32,
            c @ b'a'..=b'f' => (c - b'a' + 10) as u32,
            c @ b'A'..=b'F' => (c - b'A' + 10) as u32,
            _ => return Err(Thrown("SyntaxError: Bad unicode escape in JSON".into())),
        };
        v = v * 16 + d;
    }
    Ok(v)
}

/// Parse a JSON string literal starting at the opening `"` (index `*i`), applying
/// escapes (incl. `\uXXXX` and surrogate pairs). Plain content is flushed as UTF-8
/// slices so multi-byte characters survive intact.
fn json_parse_string(src: &str, i: &mut usize) -> Result<String, Thrown> {
    let b = src.as_bytes();
    *i += 1; // opening quote
    let mut out = String::new();
    let mut run = *i;
    loop {
        match b.get(*i).copied() {
            None => return Err(Thrown("SyntaxError: Unterminated string in JSON".into())),
            Some(b'"') => {
                out.push_str(&src[run..*i]);
                *i += 1;
                return Ok(out);
            }
            Some(b'\\') => {
                out.push_str(&src[run..*i]); // flush the plain run before the escape
                *i += 1;
                match b.get(*i).copied() {
                    Some(b'"') => out.push('"'),
                    Some(b'\\') => out.push('\\'),
                    Some(b'/') => out.push('/'),
                    Some(b'n') => out.push('\n'),
                    Some(b'r') => out.push('\r'),
                    Some(b't') => out.push('\t'),
                    Some(b'b') => out.push('\u{0008}'),
                    Some(b'f') => out.push('\u{000c}'),
                    Some(b'u') => {
                        let cp = json_hex4(b, *i + 1)?;
                        *i += 4; // past the 4 hex (now at the last one)
                        let ch = if (0xD800..=0xDBFF).contains(&cp) {
                            // High surrogate: combine with a following \uXXXX low.
                            if b.get(*i + 1) == Some(&b'\\') && b.get(*i + 2) == Some(&b'u') {
                                let lo = json_hex4(b, *i + 3)?;
                                if (0xDC00..=0xDFFF).contains(&lo) {
                                    *i += 6;
                                    let c = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                                    char::from_u32(c).unwrap_or('\u{FFFD}')
                                } else {
                                    '\u{FFFD}'
                                }
                            } else {
                                '\u{FFFD}'
                            }
                        } else {
                            char::from_u32(cp).unwrap_or('\u{FFFD}')
                        };
                        out.push(ch);
                    }
                    _ => return Err(Thrown("SyntaxError: Invalid escape in JSON string".into())),
                }
                *i += 1;
                run = *i;
            }
            // A raw control character (< 0x20) is invalid in a JSON string — it
            // must be escaped (`\n`, `	`, …). (Matches the spec / node.)
            Some(c) if c < 0x20 => {
                return Err(Thrown("SyntaxError: Bad control character in string literal in JSON".into()));
            }
            Some(_) => *i += 1, // plain byte (ASCII or UTF-8 continuation) — sliced later
        }
    }
}

/// Parse a JSON number token at `*i`.
fn json_parse_number(b: &[u8], i: &mut usize) -> Result<Value, Thrown> {
    let start = *i;
    if b.get(*i) == Some(&b'-') {
        *i += 1;
    }
    while matches!(b.get(*i), Some(c) if c.is_ascii_digit()) {
        *i += 1;
    }
    if b.get(*i) == Some(&b'.') {
        *i += 1;
        while matches!(b.get(*i), Some(c) if c.is_ascii_digit()) {
            *i += 1;
        }
    }
    if matches!(b.get(*i), Some(b'e' | b'E')) {
        *i += 1;
        if matches!(b.get(*i), Some(b'+' | b'-')) {
            *i += 1;
        }
        while matches!(b.get(*i), Some(c) if c.is_ascii_digit()) {
            *i += 1;
        }
    }
    match std::str::from_utf8(&b[start..*i]).unwrap_or("").parse::<f64>() {
        Ok(n) => Ok(Value::num(n)),
        Err(_) => Err(Thrown("SyntaxError: Invalid number in JSON".into())),
    }
}

/// Wrap JSON `parts` in `open`/`close`, compact when `indent` is empty, else
/// one element per line indented `depth+1` deep with the closing bracket at `depth`.
fn wrap_json(parts: &[String], open: char, close: char, indent: &str, depth: usize) -> String {
    if indent.is_empty() {
        return format!("{}{}{}", open, parts.join(","), close);
    }
    let pad = indent.repeat(depth + 1);
    let pad_close = indent.repeat(depth);
    let sep = format!(",\n{pad}");
    format!("{open}\n{pad}{}\n{pad_close}{close}", parts.join(&sep))
}

/// A single-argument `Math.<op>` computation, matching JS where it diverges
/// from Rust (`round` half-up; `sign` preserves ±0 and maps NaN→NaN). The
/// variadic/binary ops never reach here with the real call paths; they fall
/// back to operating on the one value provided.
fn math_unary(op: crate::bytecode::MathFn, x: f64) -> f64 {
    use crate::bytecode::MathFn as M;
    match op {
        M::Abs => x.abs(),
        M::Floor => x.floor(),
        M::Ceil => x.ceil(),
        M::Round => (x + 0.5).floor(),
        M::Trunc => x.trunc(),
        M::Sign => {
            if x.is_nan() {
                f64::NAN
            } else if x > 0.0 {
                1.0
            } else if x < 0.0 {
                -1.0
            } else {
                x
            }
        }
        M::Sqrt => x.sqrt(),
        M::Cbrt => x.cbrt(),
        M::Exp => x.exp(),
        M::Log => x.ln(),
        M::Log2 => x.log2(),
        M::Log10 => x.log10(),
        M::Sin => x.sin(),
        M::Cos => x.cos(),
        M::Tan => x.tan(),
        M::Asin => x.asin(),
        M::Acos => x.acos(),
        M::Atan => x.atan(),
        M::Expm1 => x.exp_m1(),
        M::Log1p => x.ln_1p(),
        M::Sinh => x.sinh(),
        M::Cosh => x.cosh(),
        M::Tanh => x.tanh(),
        M::Asinh => x.asinh(),
        M::Acosh => x.acosh(),
        M::Atanh => x.atanh(),
        // Math.clz32: leading zeros of ToUint32(x). Math.fround: round to f32.
        M::Clz32 => to_uint32(x).leading_zeros() as f64,
        M::Fround => x as f32 as f64,
        // Pow/Atan2/Imul/Min/Max/Hypot aren't unary; degrade gracefully.
        M::Min | M::Max => x,
        M::Hypot => x.abs(),
        M::Pow | M::Atan2 | M::Imul => f64::NAN,
    }
}

/// `Number.isInteger`: a number with no fractional part (no coercion).
fn num_is_integer(v: Value) -> bool {
    if v.is_int() {
        true
    } else if v.is_double() {
        let n = v.as_f64();
        n.is_finite() && n.fract() == 0.0
    } else {
        false
    }
}

/// `Number.isFinite`: a finite number (no coercion).
fn num_is_finite(v: Value) -> bool {
    v.is_int() || (v.is_double() && v.as_f64().is_finite())
}

/// `Number.isSafeInteger`: an integer within ±(2^53 − 1).
fn num_is_safe_integer(v: Value) -> bool {
    num_is_integer(v) && {
        let n = if v.is_int() { v.as_int() as f64 } else { v.as_f64() };
        n.abs() <= 9_007_199_254_740_991.0
    }
}

/// `Number.prototype.toString(radix)` for `radix` in 2..=36. Renders the integer
/// part in the given base (matching JS for whole numbers; a fractional part is
/// truncated — full fractional-radix rendering is out of the subset). NaN and
/// ±Infinity render via the canonical path (handled by the caller for radix 10).
// ── Date helpers (proleptic Gregorian, UTC; Howard Hinnant's algorithms) ──

/// Days since 1970-01-01 for (year, month 1..=12, day) — `day` may be out of
/// [1,31] and is carried linearly (so day 0 = the prior day), matching JS's
/// field normalization.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// (year, month 1..=12, day) from days since 1970-01-01.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Break epoch ms into UTC parts: (year, month0, day, hour, min, sec, ms,
/// weekday 0=Sun..6=Sat). Uses floored division so negative ms work.
fn date_parts(ms: f64) -> (i64, i64, i64, i64, i64, i64, i64, i64) {
    let total = ms.floor() as i64;
    let day = total.div_euclid(86_400_000);
    let rem = total.rem_euclid(86_400_000);
    let (y, m, d) = civil_from_days(day);
    let h = rem / 3_600_000;
    let mi = (rem / 60_000) % 60;
    let s = (rem / 1000) % 60;
    let mss = rem % 1000;
    let wd = (day.rem_euclid(7) + 4) % 7; // 1970-01-01 was a Thursday (4)
    (y, m - 1, d, h, mi, s, mss, wd)
}

/// Epoch ms from UTC components (month0-based; out-of-range fields normalized
/// like JS). NOTE: the legacy 2-digit-year→19xx mapping is applied by the numeric
/// CONSTRUCTORS (`Date.UTC`, `new Date(y,m,…)`), NOT here — ISO string parsing
/// must take the year literally (year 1 = 1, not 1901).
fn ms_from_utc(y: i64, mo0: i64, d: i64, h: i64, mi: i64, s: i64, ms: i64) -> f64 {
    let year = y + mo0.div_euclid(12);
    let month = mo0.rem_euclid(12); // 0-based → 1-based below
    let days = days_from_civil(year, month + 1, d);
    days as f64 * 86_400_000.0
        + h as f64 * 3_600_000.0
        + mi as f64 * 60_000.0
        + s as f64 * 1000.0
        + ms as f64
}

/// The legacy 2-digit-year mapping for the numeric Date constructors: 0..=99 →
/// 1900+y (so `Date.UTC(99,…)` is 1999). Years ≥100 (and negative) pass through.
fn legacy_year(y: i64) -> i64 {
    if (0..=99).contains(&y) {
        1900 + y
    } else {
        y
    }
}

/// JS TimeClip: NaN if non-finite or |t| > 8.64e15 (±100M days); else truncate
/// toward zero to an integer millisecond.
fn time_clip(n: f64) -> f64 {
    if !n.is_finite() || n.abs() > 8.64e15 {
        f64::NAN
    } else {
        n.trunc()
    }
}

/// `toISOString` form: `YYYY-MM-DDTHH:mm:ss.sssZ` (±YYYYYY outside 0..=9999).
fn date_to_iso(ms: f64) -> String {
    let (y, mo0, d, h, mi, s, mss, _) = date_parts(ms);
    if (0..=9999).contains(&y) {
        format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z", y, mo0 + 1, d, h, mi, s, mss)
    } else {
        let sign = if y < 0 { '-' } else { '+' };
        format!("{}{:06}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z", sign, y.abs(), mo0 + 1, d, h, mi, s, mss)
    }
}

/// Parse the ISO-8601 subset JS accepts (`YYYY`, `YYYY-MM`, `YYYY-MM-DD`,
/// optionally `THH:mm[:ss[.sss]]` and a trailing `Z`). Treated as UTC. Returns
/// NaN if unrecognised.
fn parse_date(s: &str) -> f64 {
    let s = s.trim();
    let (date, time) = match s.split_once(['T', ' ']) {
        Some((d, t)) => (d, Some(t)),
        None => (s, None),
    };
    let dp: Vec<&str> = date.split('-').collect();
    // A leading '-' (negative year) splits into an empty first field; reject.
    if dp.is_empty() || dp[0].is_empty() {
        return f64::NAN;
    }
    let parse = |x: &str| x.parse::<i64>().ok();
    let year = match parse(dp[0]) {
        Some(y) => y,
        None => return f64::NAN,
    };
    let mo = if dp.len() > 1 { match parse(dp[1]) { Some(v) => v, None => return f64::NAN } } else { 1 };
    let day = if dp.len() > 2 { match parse(dp[2]) { Some(v) => v, None => return f64::NAN } } else { 1 };
    let (mut h, mut mi, mut sec, mut msec) = (0i64, 0i64, 0i64, 0i64);
    if let Some(t) = time {
        let t = t.trim_end_matches('Z');
        // Drop a timezone offset (we treat everything as UTC).
        let t = t.split(['+']).next().unwrap_or(t);
        let (hms, frac) = match t.split_once('.') {
            Some((a, b)) => (a, Some(b)),
            None => (t, None),
        };
        let tp: Vec<&str> = hms.split(':').collect();
        if !tp.is_empty() {
            h = parse(tp[0]).unwrap_or(0);
        }
        if tp.len() > 1 {
            mi = parse(tp[1]).unwrap_or(0);
        }
        if tp.len() > 2 {
            sec = parse(tp[2]).unwrap_or(0);
        }
        if let Some(f) = frac {
            // First 3 digits = milliseconds.
            let f3: String = f.chars().take(3).chain(std::iter::repeat('0')).take(3).collect();
            msec = f3.parse::<i64>().unwrap_or(0);
        }
    }
    // mo here is 1-based from the string; ms_from_utc wants 0-based.
    ms_from_utc(year, mo - 1, day, h, mi, sec, msec)
}

/// `Number.prototype.toFixed(f)`. JS rounds half AWAY from zero — `(0.5).toFixed(0)`
/// is "1", `(2.5).toFixed(0)` is "3" — whereas Rust's `{:.*}` formatter rounds
/// half-to-even. We round the EXACT decimal of the f64 (not `x*10^f`, whose
/// product error would mis-round e.g. `0.15` whose true value is `0.14999…`):
/// format with guard digits to expose the exact value, then round the decimal
/// string half-up at `f` places. Huge magnitudes (≥1e21) defer to the default
/// rendering (JS switches to exponential there too).
fn to_fixed(n: f64, f: usize) -> String {
    if n.is_nan() {
        return "NaN".into();
    }
    if n.is_infinite() {
        return if n > 0.0 { "Infinity".into() } else { "-Infinity".into() };
    }
    if n.abs() >= 1e21 {
        return format!("{n}");
    }
    let neg = n.is_sign_negative();
    // Exact decimal of |n| with 30 guard digits past `f`; the digit at index `f`
    // (first dropped) decides the rounding, and the formatter computes it exactly.
    let s = format!("{:.*}", f + 30, n.abs());
    let dot = s.find('.').unwrap();
    let int_part = &s[..dot];
    let frac = s[dot + 1..].as_bytes();
    let round_up = frac[f] >= b'5';
    // Digits we keep (integer + first `f` fractional), as a mutable byte buffer.
    let mut digits: Vec<u8> = int_part.bytes().chain(frac[..f].iter().copied()).collect();
    if round_up {
        let mut i = digits.len();
        loop {
            if i == 0 {
                digits.insert(0, b'1'); // carried past the most-significant digit
                break;
            }
            i -= 1;
            if digits[i] == b'9' {
                digits[i] = b'0';
            } else {
                digits[i] += 1;
                break;
            }
        }
    }
    // Place the decimal point `f` digits from the right.
    let mut out = String::from_utf8(digits).unwrap();
    if f > 0 {
        let point = out.len() - f;
        out.insert(point, '.');
    }
    if neg {
        out.insert(0, '-');
    }
    out
}

fn num_to_radix(n: f64, radix: u32) -> String {
    if n.is_nan() {
        return "NaN".into();
    }
    if n.is_infinite() {
        return if n > 0.0 { "Infinity".into() } else { "-Infinity".into() };
    }
    let neg = n < 0.0;
    let mut int = n.abs().trunc() as u64;
    if int == 0 {
        return "0".into();
    }
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut buf = Vec::new();
    while int > 0 {
        buf.push(DIGITS[(int % radix as u64) as usize]);
        int /= radix as u64;
    }
    if neg {
        buf.push(b'-');
    }
    buf.reverse();
    String::from_utf8(buf).unwrap()
}

/// Normalize a Map key / Set element: `-0` becomes `+0` (SameValueZero treats
/// them equal, and iteration must yield `+0`). Everything else is unchanged.
fn normalize_zero(v: Value) -> Value {
    if v.is_double() && v.as_f64() == 0.0 {
        Value::num(0.0)
    } else {
        v
    }
}

/// JS `ToInt32`: truncate toward zero, take modulo 2^32, interpret as signed.
/// NaN/±Infinity → 0. Used by the bitwise operators.
fn to_int32(n: f64) -> i32 {
    to_uint32(n) as i32
}

/// JS `ToUint32`: truncate toward zero, take modulo 2^32 as an unsigned value.
fn to_uint32(n: f64) -> u32 {
    if !n.is_finite() || n == 0.0 {
        return 0;
    }
    // rem_euclid keeps the result in [0, 2^32); `as u32` then wraps exactly.
    let m = n.trunc().rem_euclid(4_294_967_296.0);
    m as u32
}

fn fmt_f64(n: f64) -> String {
    if n.is_nan() {
        return "NaN".into();
    }
    if n.is_infinite() {
        return if n > 0.0 { "Infinity".into() } else { "-Infinity".into() };
    }
    if n == 0.0 {
        return "0".into();
    }
    // Integer-valued doubles print without a decimal point (JS semantics). Use
    // Rust's shortest-round-trip f64 Display (matches JS Number→String, which
    // prints the shortest decimal that round-trips, e.g. 4660046610375530000 not
    // ...496) — NOT `n as i64`, which prints excess digits the f64 can't
    // distinguish and overflows for whole doubles above i64::MAX.
    if n.fract() == 0.0 && n.abs() < 1e21 {
        return format!("{n}");
    }
    let mut s = format!("{n}");
    if s.contains('e') {
        // JS uses e+/e- exponent formatting; Rust already does e.g. 1e21.
        s = s.replace('e', "e+").replace("e+-", "e-");
    }
    s
}
