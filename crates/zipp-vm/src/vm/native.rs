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
pub const PROMISE_TRY: u16 = 628;
pub const REGEXP_COMPILE: u16 = 629;
pub const REGEXP_ESCAPE: u16 = 630;
// RegExp.prototype accessor getters (defined on the prototype, not instances).
pub const REGEXP_GET_SOURCE: u16 = 631;
pub const REGEXP_GET_FLAGS: u16 = 632;
pub const REGEXP_GET_GLOBAL: u16 = 633;
pub const REGEXP_GET_IGNORECASE: u16 = 634;
pub const REGEXP_GET_MULTILINE: u16 = 635;
pub const REGEXP_GET_DOTALL: u16 = 636;
pub const REGEXP_GET_UNICODE: u16 = 637;
pub const REGEXP_GET_UNICODESETS: u16 = 638;
pub const REGEXP_GET_STICKY: u16 = 639;
pub const REGEXP_GET_HASINDICES: u16 = 640;
pub const REGEXP_SYM_MATCHALL: u16 = 641;
pub const REGEXP_SYM_SEARCH: u16 = 642;
pub const REGEXP_SYM_MATCH: u16 = 643;
pub const REGEXP_SYM_SPLIT: u16 = 644;
pub const REGEXP_SYM_REPLACE: u16 = 645;
pub const TA_FROM: u16 = 646;
pub const TA_OF: u16 = 647;
// %TypedArray%.prototype accessor getters (buffer/byteLength/byteOffset/length +
// the @@toStringTag), defined on the prototype, not the instance.
pub const TA_GET_BUFFER: u16 = 648;
pub const TA_GET_BYTELENGTH: u16 = 649;
pub const TA_GET_BYTEOFFSET: u16 = 650;
pub const TA_GET_LENGTH: u16 = 651;
pub const TA_GET_TOSTRINGTAG: u16 = 652;
/// `get [Symbol.species]()` — returns the receiver constructor (`this`). Shared
/// by Array/TypedArray/Map/Set/Promise/RegExp/ArrayBuffer.
pub const SPECIES_GET: u16 = 653;
/// `Function.prototype.toString` — returns the function's stored source text, or
/// the `function name() { [native code] }` form for natives/bound functions.
pub const FN_TO_STRING: u16 = 654;
/// `Symbol.prototype[Symbol.toPrimitive]` — returns the Symbol primitive.
pub const SYMBOL_TO_PRIMITIVE: u16 = 655;
/// `get Symbol.prototype.description` — the symbol's description (or undefined).
pub const SYMBOL_DESCRIPTION_GET: u16 = 656;
/// `String.prototype[Symbol.iterator]` — a String Iterator over code points.
pub const STR_ITERATOR: u16 = 657;
/// `JSON.rawJSON(text)` — wrap validated JSON text so `stringify` emits it raw.
pub const JSON_RAW_JSON: u16 = 658;
/// `JSON.isRawJSON(value)` — true for a `JSON.rawJSON` result object.
pub const JSON_IS_RAW_JSON: u16 = 659;
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
// WeakMap ES2025 upsert. NB: 311-314 are NOT free — they are already owned by
// STR_FROM_CHAR_CODE/STR_FROM_CODE_POINT/STR_RAW/DATE_NOW (line 232+), so these
// use genuinely-unused ids above the current max (857).
pub const WM_GET_OR_INSERT: u16 = 858;
pub const WM_GET_OR_INSERT_COMPUTED: u16 = 859;
/// `get Set.prototype.size` / `get Map.prototype.size` — brand-checked accessor
/// getters (so `getOwnPropertyDescriptor(Set.prototype,"size").get` is a real fn).
pub const SET_SIZE_GET: u16 = 860;
pub const MAP_SIZE_GET: u16 = 861;
/// The GetCapabilitiesExecutor for NewPromiseCapability: captures (resolve,
/// reject) into `vm.cap_capture`; throws if called a second time.
pub const CAP_EXECUTOR: u16 = 862;
/// The four global URI codec functions (percent-encode/decode per the Encode/
/// Decode abstract operations).
pub const GLOBAL_ENCODE_URI: u16 = 863;
pub const GLOBAL_ENCODE_URI_COMPONENT: u16 = 864;
pub const GLOBAL_DECODE_URI: u16 = 865;
pub const GLOBAL_DECODE_URI_COMPONENT: u16 = 866;
/// `Function.prototype[Symbol.hasInstance]` — OrdinaryHasInstance as a method.
pub const FN_HAS_INSTANCE: u16 = 867;
/// %ThrowTypeError% — the shared get/set accessor of `Function.prototype`'s
/// restricted `caller`/`arguments` properties; always throws a TypeError.
pub const FN_THROW_TYPE_ERROR: u16 = 868;
/// `Promise.prototype.finally`'s internal anonymous functions. THEN/CATCH are
/// the ThenFinally/CatchFinally wrappers (bound to `[onFinally, C]`): they call
/// onFinally, resolve C with its result, then chain a thunk that passes the
/// original value through / re-throws the original reason. VALUE_THUNK/THROWER
/// are those bound thunks (bound to `[value]` / `[reason]`).
pub const FINALLY_THEN: u16 = 869;
pub const FINALLY_CATCH: u16 = 870;
pub const FINALLY_VALUE_THUNK: u16 = 871;
pub const FINALLY_THROWER: u16 = 872;
pub const WS_ADD: u16 = 294;
pub const WS_HAS: u16 = 295;
pub const WS_DELETE: u16 = 296;
pub const WR_DEREF: u16 = 297;
pub const FR_REGISTER: u16 = 298;
pub const FR_UNREGISTER: u16 = 299;
// Built-in iterator methods.
pub const ITER_NEXT: u16 = 300;
pub const ITER_SELF: u16 = 301; // `[Symbol.iterator]()` → returns the iterator
// ES2025 Iterator Helpers (%Iterator.prototype%).
pub const ITER_MAP: u16 = 600;
pub const ITER_FILTER: u16 = 601;
pub const ITER_TAKE: u16 = 602;
pub const ITER_DROP: u16 = 603;
pub const ITER_FLATMAP: u16 = 604;
pub const ITER_REDUCE: u16 = 605;
pub const ITER_TOARRAY: u16 = 606;
pub const ITER_FOREACH: u16 = 607;
pub const ITER_SOME: u16 = 608;
pub const ITER_EVERY: u16 = 609;
pub const ITER_FIND: u16 = 610;
pub const ITER_HELPER_NEXT: u16 = 611;
pub const ITER_HELPER_RETURN: u16 = 612;
pub const ITER_FROM: u16 = 613;
pub const ITER_TAG_GET: u16 = 614;
pub const ITER_TAG_SET: u16 = 615;
pub const ITER_CTOR_GET: u16 = 616;
pub const ITER_CTOR_SET: u16 = 617;
/// `Iterator.concat(...iterables)` — concatenate iterables into one Iterator
/// Helper (eager @@iterator validation, lazy per-iterable opening).
pub const ITER_CONCAT: u16 = 618;
/// `Iterator.zip(iterables, options)` — zip an iterable of iterables into an
/// iterator of arrays (shortest/longest/strict modes + longest padding).
pub const ITER_ZIP: u16 = 619;
/// `Iterator.zipKeyed(iterables, options)` — like zip but `iterables` is an
/// object of named iterables and the result yields keyed objects.
pub const ITER_ZIPKEYED: u16 = 682;
// The test262 `$262` host object.
pub const DOLLAR262_DETACH: u16 = 620;
pub const DOLLAR262_GC: u16 = 621;
// Object.prototype Annex-B accessor helpers + __proto__.
pub const OBJPROTO_DEFINE_GETTER: u16 = 622;
pub const OBJPROTO_DEFINE_SETTER: u16 = 623;
pub const OBJPROTO_LOOKUP_GETTER: u16 = 624;
pub const OBJPROTO_LOOKUP_SETTER: u16 = 625;
pub const OBJPROTO_PROTO_GET: u16 = 626;
pub const OBJPROTO_PROTO_SET: u16 = 627;
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
/// `eval` — the global function. Reference identity matters (`this === eval`,
/// passed as a thisArg) and calling it parses+runs a code string (indirect eval,
/// global scope). Picked clear of every BASE range (max ~767).
pub const GLOBAL_EVAL: u16 = 800;
/// `SharedArrayBuffer.prototype.grow(newLength)` — like ArrayBuffer.resize but
/// only grows (a growable SAB).
pub const SAB_GROW: u16 = 810;
/// `SharedArrayBuffer.prototype` accessor getters, indexed off this base by
/// `SAB_GETTERS`. Each validates a shared-buffer receiver then reads the value
/// from the instance (`get_member`).
pub const SAB_GETTER_BASE: u16 = 811;
pub const SAB_GETTERS: &[&str] = &["byteLength", "maxByteLength", "growable"];
/// `Atomics.*` methods, indexed off this base by `ATOMICS_METHODS` (name, length).
pub const ATOMICS_BASE: u16 = 820;
pub const ATOMICS_METHODS: &[(&str, u8)] = &[
    ("add", 3),
    ("and", 3),
    ("or", 3),
    ("xor", 3),
    ("sub", 3),
    ("exchange", 3),
    ("compareExchange", 4),
    ("load", 2),
    ("store", 3),
    ("isLockFree", 1),
    ("wait", 4),
    ("waitAsync", 4),
    ("notify", 3),
    ("pause", 0),
];
/// `DisposableStack.prototype` methods + the `disposed` accessor getter.
pub const DISPOSABLE_USE: u16 = 840;
pub const DISPOSABLE_ADOPT: u16 = 841;
pub const DISPOSABLE_DEFER: u16 = 842;
pub const DISPOSABLE_DISPOSE: u16 = 843;
pub const DISPOSABLE_MOVE: u16 = 844;
pub const DISPOSABLE_DISPOSED_GET: u16 = 845;
/// `AsyncDisposableStack.prototype.disposeAsync` — runs disposers, returns a Promise.
pub const DISPOSABLE_DISPOSE_ASYNC: u16 = 846;
/// `%GeneratorPrototype%` next/return/throw (dispatch to `generator_method`).
pub const GEN_NEXT: u16 = 850;
pub const GEN_RETURN: u16 = 851;
pub const GEN_THROW: u16 = 852;
/// `%AsyncGeneratorPrototype%` next/return/throw (dispatch to async_generator_method).
pub const ASYNCGEN_NEXT: u16 = 853;
pub const ASYNCGEN_RETURN: u16 = 854;
pub const ASYNCGEN_THROW: u16 = 855;
/// `ShadowRealm.prototype.evaluate` / `.importValue`.
pub const SHADOWREALM_EVALUATE: u16 = 856;
pub const SHADOWREALM_IMPORTVALUE: u16 = 857;
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
    "keys", "values", "entries", "@@iterator", "toReversed", "toSorted", "with",
    // NB: append-only — ids are TA_METHOD_BASE + index, and DV_METHOD_BASE (372)
    // follows this list; keep new entries before that boundary.
    "toLocaleString",
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
pub const ARRAYBUFFER_RESIZE: u16 = 399;
pub const ARRAYBUFFER_ISVIEW: u16 = 760;
/// Accessor getters for ArrayBuffer / TypedArray / DataView instance properties,
/// registered as real accessor properties on the protos (so getOwnPropertyDescriptor
/// finds a `get` and a brand-checked invocation works). `kind`: 0 = ArrayBuffer,
/// 1 = TypedArray (on %TypedArray%.prototype), 2 = DataView.
pub const BUFFER_GETTER_BASE: u16 = 761;
pub const BUFFER_GETTERS: &[(&str, u8)] = &[
    ("byteLength", 0),
    ("maxByteLength", 0),
    ("resizable", 0),
    ("detached", 0),
    ("byteLength", 2),
    ("byteOffset", 2),
    ("immutable", 0),
    ("buffer", 2),
];
/// `ArrayBuffer.prototype.transferToImmutable` / `sliceToImmutable` (ES2026).
pub const ARRAYBUFFER_TRANSFER_IMMUTABLE: u16 = 815;
pub const ARRAYBUFFER_SLICE_IMMUTABLE: u16 = 816;
/// `ArrayBuffer.prototype.transfer` (preserves resizability) / `transferToFixedLength`.
pub const ARRAYBUFFER_TRANSFER: u16 = 817;
pub const ARRAYBUFFER_TRANSFER_FIXED: u16 = 818;
pub const PROXY_REVOCABLE: u16 = 397;
pub const PROXY_REVOKE: u16 = 398;
/// Temporal.Duration.prototype instance methods (dispatched by name via
/// `temporal_method`), at TEMPORAL_M_BASE + index.
pub const TEMPORAL_DURATION_METHODS: &[&str] =
    &["with", "negated", "abs", "toString", "toJSON", "valueOf", "add", "subtract", "round", "total"];
pub const TEMPORAL_M_BASE: u16 = 400;
pub const TEMPORAL_DURATION_FROM: u16 = 410;
pub const TEMPORAL_DURATION_COMPARE: u16 = 411;
/// Temporal.PlainDate.prototype methods at PD_M_BASE + index.
pub const PLAINDATE_METHODS: &[&str] = &[
    "with", "add", "subtract", "until", "since", "equals", "toString", "toJSON", "valueOf",
    "getISOFields", "toPlainDateTime", "toZonedDateTime", "withCalendar", "toPlainYearMonth",
    "toPlainMonthDay",
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
    "with", "add", "subtract", "until", "since", "round", "equals", "toString", "toJSON",
    "valueOf", "toPlainDate", "toPlainTime", "getISOFields", "toZonedDateTime", "withCalendar",
    "withPlainDate", "toPlainYearMonth", "toPlainMonthDay",
];
pub const PDT_M_BASE: u16 = 472;
pub const PLAINDATETIME_FROM: u16 = 490;
pub const PLAINDATETIME_COMPARE: u16 = 491;
/// Temporal.Instant.prototype methods at INST_M_BASE + index.
pub const INSTANT_METHODS: &[&str] = &[
    "add", "subtract", "until", "since", "round", "equals", "toString", "toJSON", "valueOf",
    "toZonedDateTimeISO", "toZonedDateTime",
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
/// Temporal.ZonedDateTime.prototype methods at ZDT_M_BASE + index. Instance is
/// HeapObj::Temporal kind 7, fields [epochNs hi, epochNs lo, offsetNanoseconds];
/// the time-zone id is the heap string in `zdt_tz[idx]`.
pub const ZONEDDATETIME_METHODS: &[&str] = &[
    "with", "withPlainTime", "withTimeZone", "withCalendar", "add", "subtract", "until",
    "since", "round", "equals", "toString", "toJSON", "toLocaleString", "valueOf",
    "startOfDay", "toInstant", "toPlainDate", "toPlainTime", "toPlainDateTime", "getISOFields",
];
pub const ZDT_M_BASE: u16 = 660;
pub const ZDT_FROM: u16 = 680;
pub const ZDT_COMPARE: u16 = 681;
/// Temporal.Now namespace methods.
pub const NOW_INSTANT: u16 = 540;
pub const NOW_PLAINDATETIME_ISO: u16 = 541;
pub const NOW_PLAINDATE_ISO: u16 = 542;
pub const NOW_PLAINTIME_ISO: u16 = 543;
pub const NOW_TIMEZONE_ID: u16 = 544;
pub const NOW_ZONEDDATETIME_ISO: u16 = 545;
/// Shared Temporal.<Type>.prototype.toLocaleString (routes to the receiver's
/// toString — no Intl formatting — with a brand check). One id for all types.
pub const TEMPORAL_TO_LOCALE_STRING: u16 = 546;
/// Temporal.ZonedDateTime.prototype.getTimeZoneTransition (standalone id — the
/// ZDT_M_BASE block is full). Offset/UTC zones have no transitions -> null.
pub const ZDT_GET_TZ_TRANSITION: u16 = 547;
/// Temporal.PlainDateTime.prototype.withPlainTime (standalone id — the PDT_M_BASE
/// block is full). Keeps the date, replaces the time (default midnight).
pub const PDT_WITH_PLAIN_TIME: u16 = 548;
/// `Date.prototype[Symbol.toPrimitive]` — OrdinaryToPrimitive with hint "string"
/// for "string"/"default" and "number" for "number"; any other hint is a TypeError.
pub const DATE_TO_PRIMITIVE: u16 = 549;
/// `Date.prototype.toJSON(key)` — generic: ToObject(this), ToPrimitive(number),
/// `null` for a non-finite time value, else Invoke(O, "toISOString"). Not
/// Date-branded (works on any object with a callable `toISOString`).
pub const DATE_TO_JSON: u16 = 550;
/// Intl namespace + per-service method native ids.
pub const INTL_GET_CANONICAL_LOCALES: u16 = 560;
pub const INTL_SUPPORTED_VALUES_OF: u16 = 561;
pub const INTL_RESOLVED_OPTIONS: u16 = 562;
pub const INTL_SUPPORTED_LOCALES_OF: u16 = 563;
pub const INTL_NF_FORMAT: u16 = 564;
pub const INTL_NF_FORMAT_TO_PARTS: u16 = 565;
pub const INTL_DTF_FORMAT: u16 = 566;
pub const INTL_DTF_FORMAT_TO_PARTS: u16 = 567;
pub const INTL_COLLATOR_COMPARE: u16 = 568;
pub const INTL_PLURAL_SELECT: u16 = 569;
pub const INTL_LIST_FORMAT: u16 = 570;
pub const INTL_LIST_FORMAT_TO_PARTS: u16 = 571;
pub const INTL_RTF_FORMAT: u16 = 572;
pub const INTL_RTF_FORMAT_TO_PARTS: u16 = 573;
pub const INTL_DISPLAYNAMES_OF: u16 = 574;
pub const INTL_LOCALE_TOSTRING: u16 = 575;
pub const INTL_LOCALE_MAXIMIZE: u16 = 576;
pub const INTL_LOCALE_MINIMIZE: u16 = 577;
pub const INTL_SEGMENTER_SEGMENT: u16 = 578;
pub const INTL_DURATION_FORMAT: u16 = 579;
pub const INTL_PLURAL_SELECT_RANGE: u16 = 580;
/// Intl.Locale prototype getters at INTL_LOCALE_GET_BASE + index of LOCALE_ACCESSORS.
pub const INTL_LOCALE_GET_BASE: u16 = 581;
pub const LOCALE_ACCESSORS: &[&str] = &[
    "language", "script", "region", "baseName", "calendar", "caseFirst", "collation",
    "hourCycle", "numeric", "numberingSystem",
];
/// `format`/`compare` bound-function getters (spec: these are accessors that
/// return a function bound to the instance).
pub const INTL_NF_FORMAT_GET: u16 = 592;
pub const INTL_DTF_FORMAT_GET: u16 = 593;
pub const INTL_COLLATOR_COMPARE_GET: u16 = 594;
/// Intl service kinds (index into VM.intl_ctors / intl_protos).
pub const INTL_NUMBERFORMAT: u8 = 0;
pub const INTL_DATETIMEFORMAT: u8 = 1;
pub const INTL_COLLATOR: u8 = 2;
pub const INTL_PLURALRULES: u8 = 3;
pub const INTL_LISTFORMAT: u8 = 4;
pub const INTL_RELATIVETIMEFORMAT: u8 = 5;
pub const INTL_SEGMENTER: u8 = 6;
pub const INTL_LOCALE: u8 = 7;
pub const INTL_DISPLAYNAMES: u8 = 8;
pub const INTL_DURATIONFORMAT: u8 = 9;
/// Field names of a Temporal.Duration, in slot order.
pub const DURATION_FIELDS: [&str; 10] = [
    "years", "months", "weeks", "days", "hours", "minutes", "seconds",
    "milliseconds", "microseconds", "nanoseconds",
];

/// Duration fields in the spec's alphabetical read order, paired with their index
/// into the `[y,mo,w,d,h,mi,s,ms,us,ns]` field array (ToTemporalDuration and
/// Duration.prototype.with read property-bag fields alphabetically).
pub const DURATION_FIELDS_ALPHA: [(usize, &str); 10] = [
    (3, "days"),
    (4, "hours"),
    (8, "microseconds"),
    (7, "milliseconds"),
    (5, "minutes"),
    (1, "months"),
    (9, "nanoseconds"),
    (6, "seconds"),
    (2, "weeks"),
    (0, "years"),
];
/// Temporal prototype field getters. Each unique field name has a getter native
/// at TEMPORAL_GETTER_BASE + its index in TEMPORAL_GETTER_FIELDS; the handler
/// brand-checks `this` is a Temporal instance and reads the field. These are
/// registered as accessor properties on the relevant prototypes by setup so
/// that `Object.getOwnPropertyDescriptor(Type.prototype, field).get` is a real
/// function (the value still resolves via the fast get_member path).
pub const TEMPORAL_GETTER_BASE: u16 = 700;
pub const TEMPORAL_GETTER_FIELDS: &[&str] = &[
    "years", "months", "weeks", "days", "hours", "minutes", "seconds", "milliseconds",
    "microseconds", "nanoseconds", "sign", "blank", "year", "month", "day", "dayOfWeek",
    "dayOfYear", "weekOfYear", "daysInWeek", "daysInMonth", "daysInYear", "monthsInYear",
    "inLeapYear", "monthCode", "calendarId", "hour", "minute", "second", "millisecond",
    "microsecond", "nanosecond", "epochMilliseconds", "epochNanoseconds", "epochSeconds",
    "epochMicroseconds", "era", "eraYear", "timeZoneId", "offset", "offsetNanoseconds",
    "hoursInDay",
];
pub const TEMP_G_ZONEDDATETIME: &[&str] = &[
    "calendarId", "timeZoneId", "year", "month", "monthCode", "day", "hour", "minute",
    "second", "millisecond", "microsecond", "nanosecond", "epochSeconds", "epochMilliseconds",
    "epochMicroseconds", "epochNanoseconds", "dayOfWeek", "dayOfYear", "weekOfYear",
    "hoursInDay", "daysInWeek", "daysInMonth", "daysInYear", "monthsInYear", "inLeapYear",
    "offset", "offsetNanoseconds", "era", "eraYear",
];
// Per-prototype getter sets (match the get_member field computations).
pub const TEMP_G_DURATION: &[&str] = &[
    "years", "months", "weeks", "days", "hours", "minutes", "seconds", "milliseconds",
    "microseconds", "nanoseconds", "sign", "blank",
];
pub const TEMP_G_PLAINDATE: &[&str] = &[
    "year", "month", "day", "dayOfWeek", "dayOfYear", "weekOfYear", "daysInMonth",
    "daysInYear", "daysInWeek", "monthsInYear", "inLeapYear", "monthCode", "calendarId",
    "era", "eraYear",
];
pub const TEMP_G_PLAINTIME: &[&str] =
    &["hour", "minute", "second", "millisecond", "microsecond", "nanosecond"];
pub const TEMP_G_PLAINDATETIME: &[&str] = &[
    "year", "month", "day", "hour", "minute", "second", "millisecond", "microsecond",
    "nanosecond", "dayOfWeek", "dayOfYear", "weekOfYear", "daysInMonth", "daysInYear",
    "daysInWeek", "monthsInYear", "inLeapYear", "monthCode", "calendarId", "era", "eraYear",
];
pub const TEMP_G_INSTANT: &[&str] =
    &["epochMilliseconds", "epochNanoseconds", "epochSeconds", "epochMicroseconds"];
pub const TEMP_G_PLAINYEARMONTH: &[&str] = &[
    "year", "month", "monthCode", "daysInMonth", "daysInYear", "monthsInYear", "inLeapYear",
    "era", "eraYear", "calendarId",
];
pub const TEMP_G_PLAINMONTHDAY: &[&str] = &["monthCode", "day", "calendarId"];
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
    ("toLocaleString", 0, 0), ("unshift", 0, 1),
    // String.prototype.
    ("at", 1, 1), ("charAt", 1, 1), ("charCodeAt", 1, 1), ("codePointAt", 1, 1),
    ("endsWith", 1, 1), ("includes", 1, 1), ("indexOf", 1, 1), ("padEnd", 1, 1),
    ("padStart", 1, 1), ("repeat", 1, 1), ("replace", 1, 2), ("replaceAll", 1, 2),
    ("slice", 1, 2), ("split", 1, 2), ("startsWith", 1, 1), ("substring", 1, 2),
    ("toLowerCase", 1, 0), ("toUpperCase", 1, 0), ("trim", 1, 0), ("trimEnd", 1, 0),
    ("trimStart", 1, 0), ("concat", 1, 1), ("substr", 1, 2), ("localeCompare", 1, 1),
    ("normalize", 1, 0), ("isWellFormed", 1, 0), ("toWellFormed", 1, 0),
    ("valueOf", 1, 0), ("toString", 1, 0), ("lastIndexOf", 1, 1),
    ("toLocaleLowerCase", 1, 0), ("toLocaleUpperCase", 1, 0),
    ("match", 1, 1), ("matchAll", 1, 1), ("search", 1, 1),
    // Annex B HTML wrapper methods.
    ("anchor", 1, 1), ("big", 1, 0), ("blink", 1, 0), ("bold", 1, 0), ("fixed", 1, 0),
    ("fontcolor", 1, 1), ("fontsize", 1, 1), ("italics", 1, 0), ("link", 1, 1),
    ("small", 1, 0), ("strike", 1, 0), ("sub", 1, 0), ("sup", 1, 0),
    // Number.prototype (kind 2 → number_method, receiver is a number value).
    ("toFixed", 2, 1), ("toString", 2, 1), ("valueOf", 2, 0), ("toLocaleString", 2, 0),
    ("toExponential", 2, 1), ("toPrecision", 2, 1),
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
    // Map.prototype upsert proposal (kind 4) — appended here (not in the Map
    // block) so existing entries keep their native ids.
    ("getOrInsert", 4, 2), ("getOrInsertComputed", 4, 2),
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
    // Promise.prototype.finally's internal anonymous functions. ThenFinally/
    // CatchFinally must present `length` 1; they are handed to `then` as Bound
    // wrappers over `[onFinally, C]` (2 bound args), and a bound length is
    // `max(0, target.length - boundCount)`, so the underlying native is length 3
    // to yield 1. The value-thunk/thrower are bound over 1 arg and present 0.
    match id {
        FINALLY_THEN | FINALLY_CATCH => return Some(("", 3)),
        FINALLY_VALUE_THUNK | FINALLY_THROWER => return Some(("", 0)),
        _ => {}
    }
    // Atomics.* methods.
    if (ATOMICS_BASE..ATOMICS_BASE + ATOMICS_METHODS.len() as u16).contains(&id) {
        let (name, len) = ATOMICS_METHODS[(id - ATOMICS_BASE) as usize];
        return Some((name, len));
    }
    // %TypedArray%.prototype method natives.
    if (TA_METHOD_BASE..TA_METHOD_BASE + TA_PROTO_METHODS.len() as u16).contains(&id) {
        let m = TA_PROTO_METHODS[(id - TA_METHOD_BASE) as usize];
        let len: u8 = match m {
            "reverse" | "keys" | "values" | "entries" | "toString" | "@@iterator"
            | "toReversed" | "toLocaleString" => 0,
            "slice" | "subarray" | "copyWithin" | "with" => 2,
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
    if id == ARRAYBUFFER_RESIZE {
        return Some(("resize", 1));
    }
    if id == ARRAYBUFFER_ISVIEW {
        return Some(("isView", 1));
    }
    if id == ARRAYBUFFER_TRANSFER_IMMUTABLE {
        return Some(("transferToImmutable", 0));
    }
    if id == ARRAYBUFFER_SLICE_IMMUTABLE {
        return Some(("sliceToImmutable", 2));
    }
    if id == ARRAYBUFFER_TRANSFER {
        return Some(("transfer", 0));
    }
    if id == ARRAYBUFFER_TRANSFER_FIXED {
        return Some(("transferToFixedLength", 0));
    }
    // Temporal.<Type>.prototype method natives: name + length as own properties.
    for (base, methods) in [
        (TEMPORAL_M_BASE, TEMPORAL_DURATION_METHODS),
        (PD_M_BASE, PLAINDATE_METHODS),
        (PT_M_BASE, PLAINTIME_METHODS),
        (PDT_M_BASE, PLAINDATETIME_METHODS),
        (INST_M_BASE, INSTANT_METHODS),
        (PYM_M_BASE, PLAINYEARMONTH_METHODS),
        (PMD_M_BASE, PLAINMONTHDAY_METHODS),
        (ZDT_M_BASE, ZONEDDATETIME_METHODS),
    ] {
        if (base..base + methods.len() as u16).contains(&id) {
            let m = methods[(id - base) as usize];
            let len: u8 = match m {
                "with" | "add" | "subtract" | "until" | "since" | "round" | "equals"
                | "total" | "withPlainTime" | "withTimeZone" | "withCalendar" | "withPlainDate"
                | "toZonedDateTime" | "toZonedDateTimeISO" => 1,
                _ => 0,
            };
            return Some((m, len));
        }
    }
    Some(match id {
        NOW_INSTANT => ("instant", 0),
        NOW_PLAINDATETIME_ISO => ("plainDateTimeISO", 0),
        NOW_PLAINDATE_ISO => ("plainDateISO", 0),
        NOW_PLAINTIME_ISO => ("plainTimeISO", 0),
        NOW_TIMEZONE_ID => ("timeZoneId", 0),
        NOW_ZONEDDATETIME_ISO => ("zonedDateTimeISO", 0),
        TEMPORAL_TO_LOCALE_STRING => ("toLocaleString", 0),
        ZDT_GET_TZ_TRANSITION => ("getTimeZoneTransition", 1),
        PDT_WITH_PLAIN_TIME => ("withPlainTime", 0),
        DATE_TO_PRIMITIVE => ("[Symbol.toPrimitive]", 1),
        DATE_TO_JSON => ("toJSON", 1),
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
        SYMBOL_TO_PRIMITIVE => ("[Symbol.toPrimitive]", 1),
        SYMBOL_DESCRIPTION_GET => ("get description", 0),
        STR_ITERATOR => ("[Symbol.iterator]", 0),
        BIGINT_TO_STRING => ("toString", 0),
        BIGINT_VALUE_OF => ("valueOf", 0),
        BIGINT_AS_INTN => ("asIntN", 2),
        BIGINT_AS_UINTN => ("asUintN", 2),
        REGEXP_TEST => ("test", 1),
        REGEXP_EXEC => ("exec", 1),
        REGEXP_TO_STRING => ("toString", 0),
        REGEXP_COMPILE => ("compile", 2),
        REGEXP_ESCAPE => ("escape", 1),
        REGEXP_GET_SOURCE => ("get source", 0),
        REGEXP_GET_FLAGS => ("get flags", 0),
        REGEXP_GET_GLOBAL => ("get global", 0),
        REGEXP_GET_IGNORECASE => ("get ignoreCase", 0),
        REGEXP_GET_MULTILINE => ("get multiline", 0),
        REGEXP_GET_DOTALL => ("get dotAll", 0),
        REGEXP_GET_UNICODE => ("get unicode", 0),
        REGEXP_GET_UNICODESETS => ("get unicodeSets", 0),
        REGEXP_GET_STICKY => ("get sticky", 0),
        REGEXP_GET_HASINDICES => ("get hasIndices", 0),
        REGEXP_SYM_MATCHALL => ("[Symbol.matchAll]", 1),
        REGEXP_SYM_SEARCH => ("[Symbol.search]", 1),
        REGEXP_SYM_MATCH => ("[Symbol.match]", 1),
        REGEXP_SYM_SPLIT => ("[Symbol.split]", 2),
        REGEXP_SYM_REPLACE => ("[Symbol.replace]", 2),
        TA_FROM => ("from", 1),
        TA_OF => ("of", 0),
        TA_GET_BUFFER => ("get buffer", 0),
        TA_GET_BYTELENGTH => ("get byteLength", 0),
        TA_GET_BYTEOFFSET => ("get byteOffset", 0),
        TA_GET_LENGTH => ("get length", 0),
        TA_GET_TOSTRINGTAG => ("get [Symbol.toStringTag]", 0),
        SPECIES_GET => ("get [Symbol.species]", 0),
        FN_TO_STRING => ("toString", 0),
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
        PROMISE_TRY => ("try", 1),
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
        JSON_RAW_JSON => ("rawJSON", 1),
        JSON_IS_RAW_JSON => ("isRawJSON", 1),
        MATH_RANDOM => ("random", 0),
        WM_GET => ("get", 1),
        WM_SET => ("set", 2),
        WM_GET_OR_INSERT => ("getOrInsert", 2),
        WM_GET_OR_INSERT_COMPUTED => ("getOrInsertComputed", 2),
        SET_SIZE_GET => ("get size", 0),
        MAP_SIZE_GET => ("get size", 0),
        CAP_EXECUTOR => ("", 2),
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
        ITER_MAP => ("map", 1),
        ITER_FILTER => ("filter", 1),
        ITER_TAKE => ("take", 1),
        ITER_DROP => ("drop", 1),
        ITER_FLATMAP => ("flatMap", 1),
        ITER_REDUCE => ("reduce", 1),
        ITER_TOARRAY => ("toArray", 0),
        ITER_FOREACH => ("forEach", 1),
        ITER_SOME => ("some", 1),
        ITER_EVERY => ("every", 1),
        ITER_FIND => ("find", 1),
        ITER_HELPER_NEXT => ("next", 0),
        ITER_HELPER_RETURN => ("return", 0),
        ITER_FROM => ("from", 1),
        ITER_CONCAT => ("concat", 0),
        ITER_ZIP => ("zip", 1),
        ITER_ZIPKEYED => ("zipKeyed", 1),
        OBJPROTO_DEFINE_GETTER => ("__defineGetter__", 2),
        OBJPROTO_DEFINE_SETTER => ("__defineSetter__", 2),
        OBJPROTO_LOOKUP_GETTER => ("__lookupGetter__", 1),
        OBJPROTO_LOOKUP_SETTER => ("__lookupSetter__", 1),
        PROTO_TO_LOCALE_STRING => ("toLocaleString", 0),
        NUM_IS_INTEGER => ("isInteger", 1),
        NUM_IS_NAN => ("isNaN", 1),
        NUM_IS_FINITE => ("isFinite", 1),
        NUM_IS_SAFE_INTEGER => ("isSafeInteger", 1),
        GLOBAL_PARSE_INT => ("parseInt", 2),
        GLOBAL_PARSE_FLOAT => ("parseFloat", 1),
        GLOBAL_ENCODE_URI => ("encodeURI", 1),
        GLOBAL_ENCODE_URI_COMPONENT => ("encodeURIComponent", 1),
        GLOBAL_DECODE_URI => ("decodeURI", 1),
        GLOBAL_DECODE_URI_COMPONENT => ("decodeURIComponent", 1),
        FN_HAS_INSTANCE => ("[Symbol.hasInstance]", 1),
        FN_THROW_TYPE_ERROR => ("", 0),
        GLOBAL_IS_NAN => ("isNaN", 1),
        GLOBAL_IS_FINITE => ("isFinite", 1),
        GLOBAL_EVAL => ("eval", 1),
        SAB_GROW => ("grow", 1),
        DISPOSABLE_USE => ("use", 1),
        DISPOSABLE_ADOPT => ("adopt", 2),
        DISPOSABLE_DEFER => ("defer", 1),
        DISPOSABLE_DISPOSE => ("dispose", 0),
        DISPOSABLE_DISPOSE_ASYNC => ("disposeAsync", 0),
        DISPOSABLE_MOVE => ("move", 0),
        GEN_NEXT => ("next", 1),
        GEN_RETURN => ("return", 1),
        GEN_THROW => ("throw", 1),
        ASYNCGEN_NEXT => ("next", 1),
        ASYNCGEN_RETURN => ("return", 1),
        ASYNCGEN_THROW => ("throw", 1),
        SHADOWREALM_EVALUATE => ("evaluate", 1),
        SHADOWREALM_IMPORTVALUE => ("importValue", 2),
        STR_FROM_CHAR_CODE => ("fromCharCode", 1),
        STR_FROM_CODE_POINT => ("fromCodePoint", 1),
        STR_RAW => ("raw", 1),
        DATE_NOW => ("now", 0),
        DATE_PARSE => ("parse", 1),
        DATE_UTC => ("UTC", 7),
        // Temporal static methods: from (length 1), compare (length 2), and
        // Instant.fromEpoch* (length 1).
        TEMPORAL_DURATION_FROM | PLAINDATE_FROM | PLAINTIME_FROM | PLAINDATETIME_FROM
        | INST_FROM | PLAINYEARMONTH_FROM | PLAINMONTHDAY_FROM | ZDT_FROM => ("from", 1),
        TEMPORAL_DURATION_COMPARE | PLAINDATE_COMPARE | PLAINTIME_COMPARE
        | PLAINDATETIME_COMPARE | INST_COMPARE | PLAINYEARMONTH_COMPARE | ZDT_COMPARE => {
            ("compare", 2)
        }
        INST_FROM_EPOCH_MS => ("fromEpochMilliseconds", 1),
        INST_FROM_EPOCH_NS => ("fromEpochNanoseconds", 1),
        INST_FROM_EPOCH_SEC => ("fromEpochSeconds", 1),
        INST_FROM_EPOCH_US => ("fromEpochMicroseconds", 1),
        _ => return None,
    })
}
