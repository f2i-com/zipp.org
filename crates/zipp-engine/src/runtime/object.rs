use std::cell::UnsafeCell;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::rc::Rc;
#[cfg(not(target_arch = "riscv32"))]
use std::sync::atomic::{AtomicU64, Ordering};

use indexmap::IndexMap;
use rustc_hash::FxHashMap;

use crate::intern;
use crate::value::Value;

/// A single-threaded cell that wraps `UnsafeCell` with a
/// `borrow()`/`borrow_mut()` API, eliminating `RefCell`'s runtime
/// borrow-checking overhead.
///
/// # Safety
/// The VM is single-threaded (WASM and native single-thread embed
/// only). `borrow_mut` is `unsafe` because it returns `&mut T` from
/// `&self` and performs no runtime aliasing check — calling it twice
/// without first dropping the prior reference, or calling it while a
/// `borrow()` is alive, is undefined behaviour. Use `RefCell` if you
/// need runtime borrow tracking.
pub struct VmCell<T>(UnsafeCell<T>);

impl<T: fmt::Debug> fmt::Debug for VmCell<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("VmCell")
            .field(unsafe { &*self.0.get() })
            .finish()
    }
}

impl<T: Clone> Clone for VmCell<T> {
    fn clone(&self) -> Self {
        Self(UnsafeCell::new(unsafe { &*self.0.get() }.clone()))
    }
}

impl<T> VmCell<T> {
    pub fn new(value: T) -> Self {
        Self(UnsafeCell::new(value))
    }

    /// Read-only borrow.
    ///
    /// # Safety
    /// Returns `&T` from `&self` with no runtime borrow tracking.
    /// The caller must ensure no other reference (mutable or
    /// immutable) is live for the same cell. The signature is
    /// kept safe because `&T` can be safely held alongside other
    /// `&T`s — the unsafe contract is strictly on the
    /// caller-of-`borrow_mut` side.
    #[inline(always)]
    #[allow(clippy::should_implement_trait)] // intentional: VmCell is not std::borrow::Borrow
    pub fn borrow(&self) -> &T {
        // SAFETY: VM is single-threaded; an `&T` co-existing with
        // an outstanding `&mut T` from a previous `borrow_mut` call
        // would itself be UB but that is the caller's contract.
        unsafe { &*self.0.get() }
    }

    /// Mutable borrow with no runtime check.
    ///
    /// # Safety
    /// The caller must ensure that no other reference (`&T` from
    /// `borrow()` or `&mut T` from a previous `borrow_mut`) is live
    /// for this cell when this method returns. Two simultaneous
    /// `&mut` references to the same value are immediate undefined
    /// behaviour, even on single-threaded targets.
    ///
    /// This was previously a *safe* method. The previous signature
    /// let safe code do:
    ///
    /// ```ignore
    /// let cell = VmCell::new(0);
    /// let a = cell.borrow_mut();
    /// let b = cell.borrow_mut(); // aliasing &mut T -- UB
    /// ```
    ///
    /// Marking it `unsafe` forces every call site to acknowledge the
    /// no-aliasing invariant — the same contract as
    /// `UnsafeCell::get`'s post-deref usage but at the cell's API
    /// boundary instead of buried under `unsafe { &mut *cell.0.get() }`.
    #[inline(always)]
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn borrow_mut(&self) -> &mut T {
        // SAFETY: see method docs — caller ensures no aliasing.
        unsafe { &mut *self.0.get() }
    }

    pub fn into_inner(self) -> T {
        self.0.into_inner()
    }
}

#[derive(Clone, Debug)]
pub enum HashKey {
    Sym(u32),
    Int(i64),
    Float(u64),
    Bool(bool),
    Null,
    Undefined,
    Other(Rc<str>),
}

fn canonical_float_bits(bits: u64) -> u64 {
    let value = f64::from_bits(bits);
    if value == 0.0 {
        0.0f64.to_bits()
    } else if value.is_nan() {
        f64::NAN.to_bits()
    } else {
        bits
    }
}

fn float_bits_to_i64_exact(bits: u64) -> Option<i64> {
    let value = f64::from_bits(bits);
    if !value.is_finite() {
        return None;
    }
    if value == 0.0 {
        return Some(0);
    }
    if value.trunc() == value && value >= i64::MIN as f64 && value <= i64::MAX as f64 {
        Some(value as i64)
    } else {
        None
    }
}

impl PartialEq for HashKey {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (HashKey::Sym(a), HashKey::Sym(b)) => a == b,
            (HashKey::Int(a), HashKey::Int(b)) => a == b,
            (HashKey::Float(a), HashKey::Float(b)) => a == b,
            (HashKey::Int(a), HashKey::Float(b)) | (HashKey::Float(b), HashKey::Int(a)) => {
                float_bits_to_i64_exact(*b) == Some(*a)
            }
            (HashKey::Bool(a), HashKey::Bool(b)) => a == b,
            (HashKey::Null, HashKey::Null) => true,
            (HashKey::Undefined, HashKey::Undefined) => true,
            (HashKey::Other(a), HashKey::Other(b)) => a == b,
            _ => false,
        }
    }
}

impl Eq for HashKey {}

impl Hash for HashKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match self {
            HashKey::Sym(id) => {
                0u8.hash(state);
                id.hash(state);
            }
            HashKey::Int(v) => {
                1u8.hash(state);
                v.hash(state);
            }
            HashKey::Float(bits) => {
                if let Some(i) = float_bits_to_i64_exact(*bits) {
                    1u8.hash(state);
                    i.hash(state);
                } else {
                    2u8.hash(state);
                    bits.hash(state);
                }
            }
            HashKey::Bool(v) => {
                3u8.hash(state);
                v.hash(state);
            }
            HashKey::Null => {
                4u8.hash(state);
            }
            HashKey::Undefined => {
                5u8.hash(state);
            }
            HashKey::Other(s) => {
                6u8.hash(state);
                s.hash(state);
            }
        }
    }
}

impl fmt::Display for HashKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HashKey::Sym(id) => write!(f, "{}", intern::resolve(*id)),
            HashKey::Int(v) => write!(f, "{}", v),
            HashKey::Float(bits) => write!(f, "{}", f64::from_bits(*bits)),
            HashKey::Bool(v) => write!(f, "{}", v),
            HashKey::Null => write!(f, "null"),
            HashKey::Undefined => write!(f, "undefined"),
            HashKey::Other(s) => write!(f, "{}", s),
        }
    }
}

impl HashKey {
    pub fn from_string(s: &str) -> Self {
        HashKey::Sym(intern::intern(s))
    }

    pub fn from_owned_string(s: String) -> Self {
        HashKey::Sym(intern::intern(&s))
    }

    pub fn from_sym(id: u32) -> Self {
        HashKey::Sym(id)
    }

    pub fn from_int(v: i64) -> Self {
        HashKey::Int(v)
    }

    pub fn from_float(v: f64) -> Self {
        HashKey::Float(canonical_float_bits(v.to_bits()))
    }

    pub fn from_bool(v: bool) -> Self {
        HashKey::Bool(v)
    }

    pub fn display_key(&self) -> String {
        match self {
            HashKey::Sym(id) => intern::resolve(*id).to_string(),
            HashKey::Int(v) => v.to_string(),
            HashKey::Float(bits) => f64::from_bits(*bits).to_string(),
            HashKey::Bool(v) => v.to_string(),
            HashKey::Null => "null".to_string(),
            HashKey::Undefined => "undefined".to_string(),
            HashKey::Other(s) => s.to_string(),
        }
    }

    pub fn is_numeric_index(&self) -> Option<u32> {
        match self {
            HashKey::Int(v) if *v >= 0 => Some(*v as u32),
            HashKey::Float(bits) => float_bits_to_i64_exact(*bits).and_then(|v| {
                if v >= 0 {
                    u32::try_from(v).ok()
                } else {
                    None
                }
            }),
            HashKey::Sym(id) => intern::resolve(*id).parse::<u32>().ok(),
            _ => None,
        }
    }
}

#[cfg(not(target_arch = "riscv32"))]
static NEXT_OBJECT_ID: AtomicU64 = AtomicU64::new(1);

#[cfg(not(target_arch = "riscv32"))]
fn next_object_id() -> u64 {
    NEXT_OBJECT_ID.fetch_add(1, Ordering::Relaxed)
}

// riscv32 (zkVM) is single-threaded — use a simple Cell counter instead of atomics.
#[cfg(target_arch = "riscv32")]
fn next_object_id() -> u64 {
    use std::cell::Cell;
    thread_local! {
        static NEXT_OBJECT_ID: Cell<u64> = Cell::new(1);
    }
    NEXT_OBJECT_ID.with(|c| {
        let id = c.get();
        c.set(id + 1);
        id
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum ObjectType {
    Integer,
    Float,
    Boolean,
    Null,
    Undefined,
    ReturnValue,
    Error,
    Function,
    String,
    Builtin,
    Module,
    Array,
    Hash,
    CompiledFunction,
    Closure,
    Promise,
    Symbol,
    Accessor,
    PropertyDescriptor,
    SuperObject,
    Iterator,
    Cell,
    Regexp,
    Map,
    Set,
    WeakMap,
    WeakSet,
    Class,
    Instance,
    BoundMethod,
    SuperRef,
    Generator,
    ArrayBuffer,
    TypedArray,
    DataView,
    BigInt,
    Proxy,
}

#[derive(Clone, Debug)]
#[repr(u8)]
pub enum Object {
    Integer(i64),
    Float(f64),
    Boolean(bool),
    Null,
    Undefined,
    String(Rc<str>),
    RegExp(Box<RegExpObject>),
    Map(Box<MapObject>),
    Set(Box<SetObject>),
    Array(Rc<VmCell<Vec<Value>>>),
    Hash(Rc<VmCell<HashObject>>),
    CompiledFunction(Box<CompiledFunctionObject>),
    Class(Box<ClassObject>),
    BuiltinFunction(Box<BuiltinFunctionObject>),
    Instance(Box<InstanceObject>),
    BoundMethod(Box<BoundMethodObject>),
    SuperRef(Box<SuperRefObject>),
    /// Promises are shared-mutable: the executor's resolve/reject closures
    /// need to mutate the settlement state across call boundaries, and
    /// `.then` callbacks queued on a pending promise need to see the
    /// resolved value when it eventually lands. Wrapping in `Rc<VmCell<>>`
    /// lets every `Object::Promise` clone observe the same settlement.
    Promise(Rc<VmCell<PromiseObject>>),
    Generator(Rc<VmCell<GeneratorObject>>),
    /// A unique symbol value with an optional description and a unique ID.
    Symbol(u32, Option<Rc<str>>),
    ReturnValue(Box<Object>),
    Error(Box<ErrorObject>),
    /// Rope string: deferred concatenation. Stores two string Values and total length.
    /// Flattened lazily when content is needed (charAt, split, comparison, etc.).
    /// Stored inline (no Box) to eliminate heap allocation per string concat.
    StringRope(StringRopeNode),
    /// Raw byte buffer backing TypedArrays / DataViews.
    /// Multiple views can share the same underlying buffer.
    ArrayBuffer(Rc<VmCell<Vec<u8>>>),
    /// View over an ArrayBuffer with element type + length.
    TypedArray(Box<TypedArrayObject>),
    /// Untyped view over an ArrayBuffer for endian-aware reads/writes.
    DataView(Box<DataViewObject>),
    /// Arbitrary-precision integer. Engine uses i128 as a pragmatic bound;
    /// real BigInt would need a true bignum (skipped for now).
    BigInt(i128),
    /// Proxy wrapping a target with a handler hash. Get/Set/Has traps
    /// fire when the property access path (`indexing.rs`) hits a Proxy
    /// before unwrapping; everything else passes straight through to
    /// the target.
    Proxy(Box<ProxyObject>),
}

#[derive(Clone, Debug)]
pub struct ProxyObject {
    pub target: Box<Object>,
    pub handler: Box<Object>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TypedArrayKind {
    Int8,
    Uint8,
    Uint8Clamped,
    Int16,
    Uint16,
    Int32,
    Uint32,
    Float32,
    Float64,
}

impl TypedArrayKind {
    pub fn bytes_per_element(self) -> usize {
        match self {
            Self::Int8 | Self::Uint8 | Self::Uint8Clamped => 1,
            Self::Int16 | Self::Uint16 => 2,
            Self::Int32 | Self::Uint32 | Self::Float32 => 4,
            Self::Float64 => 8,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Int8 => "Int8Array",
            Self::Uint8 => "Uint8Array",
            Self::Uint8Clamped => "Uint8ClampedArray",
            Self::Int16 => "Int16Array",
            Self::Uint16 => "Uint16Array",
            Self::Int32 => "Int32Array",
            Self::Uint32 => "Uint32Array",
            Self::Float32 => "Float32Array",
            Self::Float64 => "Float64Array",
        }
    }
    /// Read element at `byte_offset` from `buf` and return as f64.
    /// Caller is responsible for bounds-checking before calling.
    pub fn read(self, buf: &[u8], byte_offset: usize) -> f64 {
        let s = byte_offset;
        match self {
            Self::Int8 => buf[s] as i8 as f64,
            Self::Uint8 | Self::Uint8Clamped => buf[s] as f64,
            Self::Int16 => i16::from_le_bytes([buf[s], buf[s + 1]]) as f64,
            Self::Uint16 => u16::from_le_bytes([buf[s], buf[s + 1]]) as f64,
            Self::Int32 => {
                i32::from_le_bytes([buf[s], buf[s + 1], buf[s + 2], buf[s + 3]]) as f64
            }
            Self::Uint32 => {
                u32::from_le_bytes([buf[s], buf[s + 1], buf[s + 2], buf[s + 3]]) as f64
            }
            Self::Float32 => {
                f32::from_le_bytes([buf[s], buf[s + 1], buf[s + 2], buf[s + 3]]) as f64
            }
            Self::Float64 => f64::from_le_bytes([
                buf[s], buf[s + 1], buf[s + 2], buf[s + 3],
                buf[s + 4], buf[s + 5], buf[s + 6], buf[s + 7],
            ]),
        }
    }
    /// Write `value` (an f64) into `buf` at `byte_offset` using this kind's
    /// JS coercion rules (truncation, modulo, clamping).
    pub fn write(self, buf: &mut [u8], byte_offset: usize, value: f64) {
        let s = byte_offset;
        match self {
            Self::Int8 => buf[s] = (value as i64 as i8) as u8,
            Self::Uint8 => buf[s] = (value as i64 as u8),
            Self::Uint8Clamped => {
                let v = if value.is_nan() {
                    0
                } else if value < 0.0 {
                    0
                } else if value > 255.0 {
                    255
                } else {
                    value.round() as u8
                };
                buf[s] = v;
            }
            Self::Int16 => {
                let bytes = (value as i64 as i16).to_le_bytes();
                buf[s..s + 2].copy_from_slice(&bytes);
            }
            Self::Uint16 => {
                let bytes = (value as i64 as u16).to_le_bytes();
                buf[s..s + 2].copy_from_slice(&bytes);
            }
            Self::Int32 => {
                let bytes = (value as i64 as i32).to_le_bytes();
                buf[s..s + 4].copy_from_slice(&bytes);
            }
            Self::Uint32 => {
                let bytes = (value as i64 as u32).to_le_bytes();
                buf[s..s + 4].copy_from_slice(&bytes);
            }
            Self::Float32 => {
                let bytes = (value as f32).to_le_bytes();
                buf[s..s + 4].copy_from_slice(&bytes);
            }
            Self::Float64 => {
                let bytes = value.to_le_bytes();
                buf[s..s + 8].copy_from_slice(&bytes);
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct TypedArrayObject {
    pub buffer: Rc<VmCell<Vec<u8>>>,
    pub byte_offset: usize,
    pub length: usize,
    pub kind: TypedArrayKind,
}

#[derive(Clone, Debug)]
pub struct DataViewObject {
    pub buffer: Rc<VmCell<Vec<u8>>>,
    pub byte_offset: usize,
    pub byte_length: usize,
}

#[derive(Clone, Debug)]
pub struct StringRopeNode {
    pub left: Value,
    pub right: Value,
    pub total_len: usize,
}

#[derive(Clone, Debug)]
pub struct ErrorObject {
    pub name: Rc<str>,
    pub message: Rc<str>,
}

#[derive(Clone, Debug)]
#[repr(u16)]
pub enum BuiltinFunction {
    MathAbs,
    MathFloor,
    MathCeil,
    MathRound,
    MathMin,
    MathMax,
    MathPow,
    MathSqrt,
    MathTrunc,
    MathSign,
    MathRandom,
    MathLog,
    MathLog2,
    MathCbrt,
    MathSin,
    MathCos,
    MathTan,
    MathExp,
    MathLog10,
    MathAtan2,
    MathHypot,
    MathImul,
    MathClz32,
    MathFround,
    MathAcos,
    MathAsin,
    MathAtan,
    MathAcosh,
    MathAsinh,
    MathAtanh,
    MathSinh,
    MathCosh,
    MathTanh,
    MathExpm1,
    MathLog1p,
    StringCtor,
    NumberCtor,
    StringFromCharCode,
    StringCharAt,
    StringSplit,
    StringIncludes,
    StringSlice,
    StringToUpperCase,
    StringToLowerCase,
    StringTrim,
    StringStartsWith,
    StringEndsWith,
    StringIndexOf,
    StringLastIndexOf,
    StringSubstring,
    StringRepeat,
    StringPadStart,
    StringPadEnd,
    StringCharCodeAt,
    StringReplace,
    StringReplaceAll,
    NumberToFixed,
    NumberToPrecision,
    NumberToString,
    ParseInt,
    ParseFloat,
    IsNaN,
    IsFinite,
    NumberIsNaN,
    NumberIsFinite,
    NumberIsInteger,
    NumberIsSafeInteger,
    ObjectKeys,
    ObjectValues,
    ObjectEntries,
    ObjectFromEntries,
    ObjectHasOwn,
    ObjectIs,
    ObjectAssign,
    ObjectFreeze,
    ObjectCreate,
    ObjectDefineProperty,
    ObjectGetPrototypeOf,
    ObjectGetOwnPropertyDescriptor,
    ObjectGetOwnPropertyNames,
    ArrayOf,
    HashHasOwnProperty,
    ObjectPrototypeToString,
    JsonStringify,
    JsonParse,
    PromiseResolve,
    PromiseReject,
    PromiseAll,
    PromiseRace,
    PromiseAllSettled,
    PromiseAny,
    PromiseThen,
    PromiseCatch,
    PromiseFinally,
    ArrayMap,
    ArrayForEach,
    ArrayFlatMap,
    ArrayFlat,
    ArrayReverse,
    ArraySome,
    ArrayEvery,
    ArrayFindIndex,
    ArrayIndexOf,
    ArrayLastIndexOf,
    ArrayPop,
    ArrayPush,
    ArraySort,
    ArrayFilter,
    ArrayReduce,
    ArrayReduceRight,
    ArrayFind,
    ArrayIncludes,
    ArrayJoin,
    ArrayToString,
    ArrayValueOf,
    ArraySlice,
    ArrayAt,
    ArrayToSorted,
    ArrayWith,
    ArrayKeys,
    ArrayValues,
    ArrayEntries,
    ArrayShift,
    ArrayUnshift,
    ArraySplice,
    ArrayConcat,
    ArrayFrom,
    ArrayIsArray,
    ArrayFill,
    ArrayCopyWithin,
    ArrayFindLast,
    ArrayFindLastIndex,
    ArrayToReversed,
    RegExpCtor,
    RegExpTest,
    RegExpExec,
    StringMatch,
    StringMatchAll,
    StringSearch,
    StringConcat,
    StringTrimStart,
    StringTrimEnd,
    StringFromCodePoint,
    MapCtor,
    MapSet,
    MapGet,
    MapHas,
    MapDelete,
    MapClear,
    MapKeys,
    MapValues,
    MapEntries,
    MapForEach,
    SetCtor,
    SetAdd,
    SetHas,
    SetDelete,
    SetClear,
    SetKeys,
    SetValues,
    SetEntries,
    SetForEach,
    SymbolCtor,
    GeneratorNext,
    GeneratorReturn,
    GeneratorThrow,
    LocalStorageGetItem,
    LocalStorageSetItem,
    LocalStorageRemoveItem,
    LocalStorageClear,
    DateNow,
    DateToISOString,
    DateGetTime,
    DateGetHours,
    DateGetMinutes,
    DateGetSeconds,
    DateGetMilliseconds,
    DateGetFullYear,
    DateGetMonth,
    DateGetDate,
    DateGetDay,
    DateToLocaleDateString,
    DateToLocaleTimeString,
    DateToLocaleString,
    DateToString,
    DateValueOf,
    DateSetTime,
    DateSetHours,
    DateSetMinutes,
    DateSetSeconds,
    DateSetMilliseconds,
    DateSetFullYear,
    DateSetMonth,
    DateSetDate,
    DateParse,
    DateUtc,
    DbQuery,
    DbCreate,
    DbUpdate,
    DbDelete,
    DbHardDelete,
    DbGet,
    DbStartSync,
    DbStopSync,
    DbGetSyncStatus,
    DbGetSavedSyncRoom,

    // ── HTTP bridge ──
    HttpGet,
    HttpPost,
    HttpPut,
    HttpDelete,

    // ── FS bridge ──
    FsReadFile,
    FsWriteFile,
    FsAppendFile,
    FsExists,
    FsListDir,
    FsDeleteFile,
    FsMkdir,

    // ── Env bridge ──
    EnvGet,
    EnvKeys,
    EnvLog,

    // ── Draw bridge ──
    DrawRect,
    DrawRoundedRect,
    DrawCircle,
    DrawEllipse,
    DrawLine,
    DrawPath,
    DrawText,
    DrawImage,
    DrawLinearGradient,
    DrawRadialGradient,
    DrawShadow,
    DrawPushClip,
    DrawPopClip,
    DrawPushTransform,
    DrawPopTransform,
    DrawPushOpacity,
    DrawPopOpacity,
    DrawArc,
    DrawMeasureText,
    DrawGetViewportWidth,
    DrawGetViewportHeight,

    // ── Layout bridge ──
    LayoutCreateNode,
    LayoutUpdateStyle,
    LayoutSetChildren,
    LayoutComputeLayout,
    LayoutGetLayout,
    LayoutRemoveNode,
    LayoutClear,

    // ── Input bridge ──
    InputGetMouseX,
    InputGetMouseY,
    InputIsMouseDown,
    InputIsMousePressed,
    InputIsMouseReleased,
    InputGetScrollY,
    InputSetCursor,
    InputGetTextInput,
    InputIsBackspacePressed,
    InputIsEscapePressed,
    InputRequestRedraw,
    InputGetElapsedSecs,
    InputGetPageElapsedSecs,
    InputGetDeltaTime,
    InputGetFocusedInput,
    InputSetFocusedInput,
    InputIsKeyDown,

    // ── Window event bridge ──
    WindowAddEventListener,
    WindowRemoveEventListener,
    EventPreventDefault,
    EventStopPropagation,

    // ── URI encoding ──
    EncodeURIComponent,
    DecodeURIComponent,
    EncodeURI,
    DecodeURI,

    // ── Generic host call bridge ──
    /// host.call(kind, argsArray, callback) — queues an async call to the host.
    HostCall,
    /// host.callSync(kind, argsArray) — synchronous host call, returns JSON result.
    HostCallSync,

    // ── Error constructors ──
    ErrorConstructor,
    TypeErrorConstructor,
    RangeErrorConstructor,
    SyntaxErrorConstructor,
    ReferenceErrorConstructor,

    // ── Typed arrays + ArrayBuffer + DataView ──
    ArrayBufferCtor,
    DataViewCtor,
    Int8ArrayCtor,
    Uint8ArrayCtor,
    Uint8ClampedArrayCtor,
    Int16ArrayCtor,
    Uint16ArrayCtor,
    Int32ArrayCtor,
    Uint32ArrayCtor,
    Float32ArrayCtor,
    Float64ArrayCtor,
    TypedArraySet,
    TypedArraySubarray,
    DataViewGetInt8,
    DataViewSetInt8,
    DataViewGetUint8,
    DataViewSetUint8,
    DataViewGetInt16,
    DataViewSetInt16,
    DataViewGetUint16,
    DataViewSetUint16,
    DataViewGetInt32,
    DataViewSetInt32,
    DataViewGetUint32,
    DataViewSetUint32,
    DataViewGetFloat32,
    DataViewSetFloat32,
    DataViewGetFloat64,
    DataViewSetFloat64,

    // ── BigInt ──
    BigIntCtor,

    // ── Proxy ──
    ProxyCtor,

    // ── Microtask + Promise(executor) ──
    QueueMicrotask,
    PromiseExecutorCtor,
    /// Callable bound to an Rc<VmCell<PromiseObject>> via receiver. When
    /// invoked, transitions the captured promise from Pending to
    /// Fulfilled (PromiseResolveBound) or Rejected (PromiseRejectBound)
    /// and drains any queued .then/.catch handlers into the microtask
    /// queue. No-ops on an already-settled promise (matches V8).
    PromiseResolveBound,
    PromiseRejectBound,
    /// Internal: microtask-queue step that runs a single `.then` /
    /// `.catch` handler and settles the companion promise. Args are
    /// `[chained_promise, value, kind /*0=fulfilled,1=rejected*/,
    /// handler_or_undefined]`. Not exposed as a global.
    PromiseChainStep,

    // ── Reflect ──
    ReflectGet,
    ReflectSet,
    ReflectHas,
    ReflectDeleteProperty,
    ReflectOwnKeys,
    ReflectGetPrototypeOf,
    ReflectSetPrototypeOf,
    ReflectIsExtensible,
    ReflectPreventExtensions,
    ReflectDefineProperty,
    ReflectGetOwnPropertyDescriptor,
    ReflectApply,
    ReflectConstruct,
    StringAt,
    StringCodePointAt,
    StringNormalize,
    StringRaw,
    StructuredClone,
}

#[derive(Clone, Debug)]
pub struct BuiltinFunctionObject {
    pub function: BuiltinFunction,
    pub receiver: Option<Object>,
}

#[derive(Clone, Debug)]
pub struct CompiledFunctionObject {
    pub instructions: Rc<Vec<u8>>,
    pub constants: Rc<Vec<Object>>,
    pub num_locals: usize,
    pub num_parameters: usize,
    pub rest_parameter_index: Option<usize>,
    pub takes_this: bool,
    pub is_async: bool,
    pub is_generator: bool,
    pub num_cache_slots: u16,
    pub max_stack_depth: u16,
    /// Maximum number of registers used by this function.
    pub register_count: u16,
    /// Persistent inline property cache shared across all invocations of this
    /// function.  Entries are `(shape_version, slot_index)` indexed by
    /// `cache_slot`.  `Rc` ensures all clones of this function (including
    /// `BoundMethod` wrappers) share the same warm cache.
    pub inline_cache: Rc<VmCell<Vec<(u32, u32)>>>,
    /// Global slot indices that inner closures need to capture at creation time.
    /// Set by the compiler for functions that contain captured parameters.
    pub closure_captures: Vec<u16>,
    /// Captured global slot values, snapshotted at closure creation time.
    /// Set by the VM's MakeClosure opcode.
    pub captured_values: Vec<(u16, Value)>,
    /// Function properties (e.g., webpack's n.d, n.r, n.o).
    /// In JS, functions are objects and can have arbitrary properties.
    pub properties: Option<Box<rustc_hash::FxHashMap<u32, Value>>>,
}

#[derive(Clone, Debug)]
pub struct PromiseObject {
    /// Current state. A Pending promise holds callback queues; settling
    /// (via resolve/reject) drains them into the microtask queue and
    /// flips the state to Fulfilled/Rejected. Once settled, the state
    /// never changes again.
    pub settled: PromiseState,
    /// Callbacks registered by `.then(onFulfilled, onRejected)` while the
    /// promise was still pending. Drained into the microtask queue on
    /// settle. Parallel: `then_chain[i]` runs if fulfilled, `catch_chain[i]`
    /// runs if rejected. Both vectors are the same length; Value::UNDEFINED
    /// means "no handler for that branch" (the chain forwards the value).
    pub then_chain: Vec<Value>,
    pub catch_chain: Vec<Value>,
    /// Companion promises for each `.then` chain entry — resolved with the
    /// handler's return value (or the propagated settlement if the handler
    /// was UNDEFINED on its branch). Same length as `then_chain`.
    pub chained: Vec<Value>,
}

#[derive(Clone, Debug)]
pub struct RegExpObject {
    pub pattern: String,
    pub flags: String,
}

/// Open-addressing hash table with linear probing. No chain Vec needed.
/// Each slot stores entry_index + 1 (0 = empty). Collisions resolved by probing.
/// Simpler than bucket-chain: one Vec, no chain allocation/growth/zeroing.
#[derive(Clone, Debug)]
pub struct FlatHashTable {
    pub buckets: Vec<u16>,   // u16 slots: halves table size, fits in L1 at 4096 slots
    /// Parallel symbol ID cache: sym_ids[bucket_idx] = symbol u32 for that slot.
    /// Eliminates enum discrimination in get_sym() — just a u32 compare per probe.
    sym_cache: Vec<u32>,
    pub mask: u32,
    pub count: u32,
}

impl Default for FlatHashTable {
    fn default() -> Self {
        Self::new()
    }
}

impl FlatHashTable {
    pub fn new() -> Self {
        // 256 is the balance point empirically: enough slots to skip
        // the 64→256 grow for medium Maps, small enough (1.5 KiB per
        // empty Map) that many-small-Map workloads don't balloon.
        // 4 096 was tried and made no measurable perf difference on
        // the 2 000-entry bench because the grow path is already a
        // narrow hot spot.
        Self {
            buckets: vec![0u16; 256],
            sym_cache: vec![0u32; 256],
            mask: 255,
            count: 0,
        }
    }

    /// Construct with a starting capacity of at least `hint` slots,
    /// rounded up to the next power of two. Useful when the caller
    /// knows they'll be inserting many entries.
    pub fn with_capacity(hint: usize) -> Self {
        let cap = hint.max(64).next_power_of_two();
        // Cap at u16::MAX - 1 because slot indices are u16.
        let cap = cap.min(65_535);
        Self {
            buckets: vec![0u16; cap],
            sym_cache: vec![0u32; cap],
            mask: (cap - 1) as u32,
            count: 0,
        }
    }

    #[inline(always)]
    pub fn hash_key(key: &HashKey) -> u32 {
        match key {
            HashKey::Sym(id) => id.wrapping_mul(2654435769), // fibonacci hashing
            HashKey::Int(v) => (*v as u32).wrapping_mul(2654435769),
            HashKey::Float(bits) => (*bits as u32).wrapping_mul(2654435769),
            HashKey::Bool(b) => *b as u32 | 0x8000_0000,
            HashKey::Null => 0xDEAD_0001,
            HashKey::Undefined => 0xDEAD_0002,
            HashKey::Other(s) => {
                let mut h = 0u32;
                for b in s.as_bytes() { h = h.wrapping_mul(31).wrapping_add(*b as u32); }
                h
            }
        }
    }

    #[inline(always)]
    pub fn get(&self, entries: &[(HashKey, Value)], key: &HashKey) -> Option<usize> {
        let h = Self::hash_key(key);
        let mask = self.mask as usize;
        let mut idx = (h as usize) & mask;
        loop {
            let slot = unsafe { *self.buckets.get_unchecked(idx) };
            if slot == 0 { return None; }
            let i = (slot - 1) as usize;
            if unsafe { &entries.get_unchecked(i).0 } == key { return Some(i); }
            idx = (idx + 1) & mask;
        }
    }

    #[inline(always)]
    pub fn get_sym(&self, _entries: &[(HashKey, Value)], sym_id: u32) -> Option<usize> {
        let h = sym_id.wrapping_mul(2654435769);
        let mask = self.mask as usize;
        let mut idx = (h as usize) & mask;
        loop {
            let slot = unsafe { *self.buckets.get_unchecked(idx) };
            if slot == 0 { return None; }
            // Fast u32 compare via parallel sym_cache — no enum discrimination needed
            if unsafe { *self.sym_cache.get_unchecked(idx) } == sym_id {
                return Some((slot - 1) as usize);
            }
            idx = (idx + 1) & mask;
        }
    }

    #[inline(always)]
    pub fn get_or_insert_sym(&mut self, entries: &[(HashKey, Value)], sym_id: u32, entry_idx: usize) -> Option<usize> {
        let h = sym_id.wrapping_mul(2654435769);
        let mask = self.mask as usize;
        let mut idx = (h as usize) & mask;
        loop {
            let slot = unsafe { *self.buckets.get_unchecked(idx) };
            if slot == 0 {
                if self.count * 4 >= (self.mask + 1) * 3 {
                    self.grow(entries);
                    let new_mask = self.mask as usize;
                    idx = (h as usize) & new_mask;
                    while unsafe { *self.buckets.get_unchecked(idx) } != 0 {
                        idx = (idx + 1) & new_mask;
                    }
                }
                unsafe {
                    *self.buckets.get_unchecked_mut(idx) = (entry_idx + 1) as u16;
                    *self.sym_cache.get_unchecked_mut(idx) = sym_id;
                }
                self.count += 1;
                return None;
            }
            // Fast u32 compare via parallel sym_cache
            if unsafe { *self.sym_cache.get_unchecked(idx) } == sym_id {
                return Some((slot - 1) as usize);
            }
            idx = (idx + 1) & mask;
        }
    }

    #[inline(always)]
    pub fn insert(&mut self, entries: &[(HashKey, Value)], key: &HashKey, entry_idx: usize) {
        if self.count * 4 >= (self.mask + 1) * 3 {
            self.grow(entries);
        }
        let h = Self::hash_key(key);
        let sym_id = if let HashKey::Sym(id) = key { *id } else { 0 };
        let mask = self.mask as usize;
        let mut idx = (h as usize) & mask;
        while unsafe { *self.buckets.get_unchecked(idx) } != 0 {
            idx = (idx + 1) & mask;
        }
        unsafe {
            *self.buckets.get_unchecked_mut(idx) = (entry_idx + 1) as u16;
            *self.sym_cache.get_unchecked_mut(idx) = sym_id;
        }
        self.count += 1;
    }

    fn grow(&mut self, entries: &[(HashKey, Value)]) {
        // Aggressive growth: quadruple when small, double when large.
        // Reduces resize count from 6 to 4 for 2000-entry Maps.
        let old_cap = (self.mask + 1) as usize;
        let new_cap = if old_cap <= 256 { old_cap * 4 } else { old_cap * 2 };
        self.buckets = vec![0u16; new_cap];
        self.sym_cache = vec![0u32; new_cap];
        self.mask = (new_cap - 1) as u32;
        self.count = 0;
        for (i, (key, _)) in entries.iter().enumerate() {
            let h = Self::hash_key(key);
            let sym_id = if let HashKey::Sym(id) = key { *id } else { 0 };
            let mask = self.mask as usize;
            let mut idx = (h as usize) & mask;
            while unsafe { *self.buckets.get_unchecked(idx) } != 0 {
                idx = (idx + 1) & mask;
            }
            unsafe {
                *self.buckets.get_unchecked_mut(idx) = (i + 1) as u16;
                *self.sym_cache.get_unchecked_mut(idx) = sym_id;
            }
            self.count += 1;
        }
    }

    pub fn clear(&mut self) {
        self.buckets.fill(0);
        self.sym_cache.fill(0);
        self.count = 0;
    }

    pub fn remove(&mut self, entries: &[(HashKey, Value)], entry_idx: usize) {
        let key = &entries[entry_idx].0;
        let h = Self::hash_key(key);
        let mask = self.mask as usize;
        let mut idx = (h as usize) & mask;
        loop {
            let slot = self.buckets[idx];
            if slot == 0 { return; }
            if (slot - 1) as usize == entry_idx {
                self.buckets[idx] = 0;
                self.sym_cache[idx] = 0;
                self.count -= 1;
                let mut next = (idx + 1) & mask;
                while self.buckets[next] != 0 {
                    let displaced = self.buckets[next];
                    let displaced_sym = self.sym_cache[next];
                    self.buckets[next] = 0;
                    self.sym_cache[next] = 0;
                    self.count -= 1;
                    let di = (displaced - 1) as usize;
                    let dh = Self::hash_key(&entries[di].0);
                    let mut new_idx = (dh as usize) & mask;
                    while self.buckets[new_idx] != 0 {
                        new_idx = (new_idx + 1) & mask;
                    }
                    self.buckets[new_idx] = displaced;
                    self.sym_cache[new_idx] = displaced_sym;
                    self.count += 1;
                    next = (next + 1) & mask;
                }
                return;
            }
            idx = (idx + 1) & mask;
        }
    }
}

#[derive(Clone, Debug)]
pub struct MapObject {
    pub entries: Rc<VmCell<Vec<(HashKey, Value)>>>,
    pub indices: Rc<VmCell<FlatHashTable>>,
}

impl Default for MapObject {
    fn default() -> Self {
        Self {
            // Pre-allocate entries to avoid early reallocations (0→1→2→4→8→16)
            entries: Rc::new(VmCell::new(Vec::with_capacity(16))),
            indices: Rc::new(VmCell::new(FlatHashTable::new())),
        }
    }
}

#[derive(Clone, Debug)]
pub struct SetObject {
    pub entries: Rc<VmCell<Vec<HashKey>>>,
    pub indices: Rc<VmCell<FxHashMap<HashKey, usize>>>,
}

impl Default for SetObject {
    fn default() -> Self {
        Self {
            entries: Rc::new(VmCell::new(Vec::new())),
            indices: Rc::new(VmCell::new(FxHashMap::default())),
        }
    }
}

#[derive(Clone, Debug)]
pub enum PromiseState {
    Pending,
    Fulfilled(Box<Object>),
    Rejected(Box<Object>),
}

/// Construct a fresh promise wrapper in the given state. The chain
/// vectors start empty — `.then` appends to them while pending, and
/// `resolve`/`reject` drain them at settlement time.
#[inline]
pub fn new_promise_object(state: PromiseState) -> PromiseObject {
    PromiseObject {
        settled: state,
        then_chain: Vec::new(),
        catch_chain: Vec::new(),
        chained: Vec::new(),
    }
}

/// Convenience: wrap `new_promise_object(state)` in an `Object::Promise`.
#[inline]
pub fn new_promise(state: PromiseState) -> Object {
    Object::Promise(Rc::new(VmCell::new(new_promise_object(state))))
}

#[inline]
pub fn new_fulfilled_promise(v: Object) -> Object {
    new_promise(PromiseState::Fulfilled(Box::new(v)))
}

#[inline]
pub fn new_rejected_promise(v: Object) -> Object {
    new_promise(PromiseState::Rejected(Box::new(v)))
}

#[inline]
pub fn new_pending_promise() -> Object {
    new_promise(PromiseState::Pending)
}

/// State of a generator: Created (not yet started), Suspended (yielded),
/// or Completed (returned or threw).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GeneratorState {
    Created,
    Suspended,
    Completed,
}

/// A generator object created by calling a `function*`.
///
/// Holds the compiled function, the saved execution state (IP, locals),
/// and the generator's current state.  The VM resumes execution from the
/// saved IP when `.next()` is called.
#[derive(Clone, Debug)]
pub struct GeneratorObject {
    /// The compiled generator function body.
    pub function: CompiledFunctionObject,
    /// Saved locals (variables) — populated after the first `yield`.
    pub locals: Vec<Object>,
    /// Saved instruction pointer — where to resume.
    pub saved_ip: usize,
    /// Arguments passed to the generator function on initial call.
    pub args: Vec<Value>,
    /// The receiver (`this`) if any.
    pub receiver: Option<Value>,
    /// Current state.
    pub state: GeneratorState,
}

/// One level of super methods/getters/setters for multi-level inheritance chains.
pub type SuperLevel = (
    FxHashMap<String, CompiledFunctionObject>,
    FxHashMap<String, CompiledFunctionObject>,
    FxHashMap<String, CompiledFunctionObject>,
);

#[derive(Clone, Debug)]
pub struct ClassObject {
    pub name: String,
    pub parent_chain: Vec<String>,
    pub constructor: Option<CompiledFunctionObject>,
    pub methods: FxHashMap<String, CompiledFunctionObject>,
    pub static_methods: FxHashMap<String, CompiledFunctionObject>,
    pub getters: FxHashMap<String, CompiledFunctionObject>,
    pub setters: FxHashMap<String, CompiledFunctionObject>,
    pub super_methods: FxHashMap<String, CompiledFunctionObject>,
    pub super_getters: FxHashMap<String, CompiledFunctionObject>,
    pub super_setters: FxHashMap<String, CompiledFunctionObject>,
    /// Chain of ancestor super levels for multi-level inheritance.
    /// `super_constructor_chain[0]` = grandparent methods, `[1]` = great-grandparent, etc.
    pub super_constructor_chain: Vec<SuperLevel>,
    /// Instance field initializers: `(name, initializer_fn)`.
    /// Each initializer is a zero-arg `takes_this=true` function whose return
    /// value is the field's initial value.  Executed in order during `new`.
    pub field_initializers: Vec<(String, CompiledFunctionObject)>,
    /// Static initializers run once at class definition time, in order.
    pub static_initializers: Vec<StaticInitializer>,
    /// Static field values, populated at class-definition time by the
    /// `OpInitClass` / `InitClass` opcode.
    pub static_fields: FxHashMap<String, Object>,
}

/// A static initializer — either a field assignment or a static block.
#[derive(Clone, Debug)]
pub enum StaticInitializer {
    /// `static name = expr;` — thunk returns the value to assign.
    Field {
        name: String,
        thunk: CompiledFunctionObject,
    },
    /// `static { ... }` — thunk is executed for side effects.
    Block { thunk: CompiledFunctionObject },
}

#[derive(Clone, Debug)]
pub struct InstanceObject {
    pub class_name: String,
    pub parent_chain: Vec<String>,
    pub fields: FxHashMap<String, crate::value::Value>,
    pub methods: FxHashMap<String, CompiledFunctionObject>,
    pub getters: FxHashMap<String, CompiledFunctionObject>,
    pub setters: FxHashMap<String, CompiledFunctionObject>,
    pub super_methods: FxHashMap<String, CompiledFunctionObject>,
    pub super_getters: FxHashMap<String, CompiledFunctionObject>,
    pub super_setters: FxHashMap<String, CompiledFunctionObject>,
    /// Remaining ancestor chain for multi-level super() constructor calls.
    pub super_constructor_chain: Vec<SuperLevel>,
}

#[derive(Clone, Debug)]
pub struct BoundMethodObject {
    pub function: CompiledFunctionObject,
    pub receiver: Box<Object>,
}

#[derive(Clone, Debug)]
pub struct SuperRefObject {
    pub receiver: Box<Object>,
    pub methods: FxHashMap<String, CompiledFunctionObject>,
    pub getters: FxHashMap<String, CompiledFunctionObject>,
    pub setters: FxHashMap<String, CompiledFunctionObject>,
    /// Remaining ancestor chain so nested super() calls resolve correctly.
    pub constructor_chain: Vec<SuperLevel>,
}

#[derive(Clone, Debug)]
pub struct HashObject {
    pub id: u64,
    pub shape_version: u32,
    pub pairs: IndexMap<HashKey, Value>,
    pub values: Vec<Value>,
    pub str_slots: FxHashMap<u32, usize>,
    /// True when `values` has been updated via `set_value_at_slot_unchecked`
    /// without syncing the corresponding entries in `pairs`.
    /// Call `sync_pairs_if_dirty()` before reading from `pairs`.
    pub pairs_dirty: bool,
    /// Backing store for heap-type Values created outside the VM's main heap
    /// (e.g., compiler-constructed builtin globals). Migrated to VM heap on
    /// first access via `migrate_local_objects`. Empty for VM-created hashes.
    pub local_objects: Vec<Object>,
    /// Getter accessor functions defined on this object (e.g. `{ get x() { ... } }`).
    pub getters: Option<Box<FxHashMap<String, CompiledFunctionObject>>>,
    /// Setter accessor functions defined on this object (e.g. `{ set x(v) { ... } }`).
    pub setters: Option<Box<FxHashMap<String, CompiledFunctionObject>>>,
    /// True when Object.freeze() has been called on this object.
    pub frozen: bool,
}

impl Default for HashObject {
    fn default() -> Self {
        Self {
            id: next_object_id(),
            shape_version: 1,
            pairs: IndexMap::<HashKey, Value>::new(),
            values: Vec::new(),
            str_slots: FxHashMap::default(),
            pairs_dirty: false,
            local_objects: Vec::new(),
            getters: None,
            setters: None,
            frozen: false,
        }
    }
}

impl HashObject {
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            id: next_object_id(),
            shape_version: 1,
            pairs: IndexMap::<HashKey, Value>::with_capacity(capacity),
            values: Vec::with_capacity(capacity),
            str_slots: {
                let mut m = FxHashMap::default();
                m.reserve(capacity);
                m
            },
            pairs_dirty: false,
            local_objects: Vec::new(),
            getters: None,
            setters: None,
            frozen: false,
        }
    }

    pub fn bump_shape_version(&mut self) {
        self.shape_version = self.shape_version.wrapping_add(1);
        if self.shape_version == 0 {
            self.shape_version = 1;
        }
    }

    /// Bump shape version incorporating the property name, so hashes with
    /// different property orderings get different shape versions even if they
    /// have the same number of properties.
    pub fn bump_shape_version_with_key(&mut self, sym: u32) {
        // Mix the symbol ID into the shape to distinguish {a,b} from {b,a}
        self.shape_version = self.shape_version.wrapping_mul(31).wrapping_add(sym);
        if self.shape_version == 0 {
            self.shape_version = 1;
        }
    }

    /// Insert or update a key. Returns `true` if a new slot was added,
    /// `false` if the value updated an existing slot **or** if the
    /// hash is frozen. The frozen check is centralised here so every
    /// caller (`set_by_str`, the property-store opcode slow path,
    /// `Object.assign` / `Object.fromEntries`, …) gets the no-op
    /// behaviour that strict-mode JS expects without each call site
    /// having to re-check.
    pub fn insert_pair(&mut self, key: HashKey, value: Value) -> bool {
        if self.frozen {
            return false;
        }
        if let HashKey::Sym(id) = &key {
            if let Some(&slot) = self.str_slots.get(id) {
                self.values[slot] = value;
                self.pairs[slot] = value;
                return false;
            }
        }

        if let Some(slot) = self.pairs.get_index_of(&key) {
            self.values[slot] = value;
            self.pairs[slot] = value;
            return false;
        }

        let sym_id = match &key {
            HashKey::Sym(id) => Some(*id),
            _ => None,
        };
        self.pairs.insert(key, value);
        self.values.push(value);
        if let Some(id) = sym_id {
            self.str_slots.insert(id, self.values.len() - 1);
            // Mix property name into shape so {a,b} and {b,a} get different shapes
            self.bump_shape_version_with_key(id);
        } else {
            self.bump_shape_version();
        }
        true // new key was inserted
    }

    /// Remove a key. Returns the removed value or `None` if the key
    /// was absent — and also `None` when the hash is frozen, since
    /// `Object.freeze`'s contract makes deletes silently fail.
    pub fn remove_pair(&mut self, key: &HashKey) -> Option<Value> {
        if self.frozen {
            return None;
        }
        let slot = self.pairs.get_index_of(key)?;

        // Sync before removal since shift_remove returns the value.
        self.sync_pairs_if_dirty();

        let removed_sym = self.pairs.get_index(slot).and_then(|(k, _)| match k {
            HashKey::Sym(id) => Some(*id),
            _ => None,
        });

        let removed = self.pairs.shift_remove(key);
        self.values.remove(slot);

        if let Some(id) = removed_sym {
            self.str_slots.remove(&id);
        }
        for idx in self.str_slots.values_mut() {
            if *idx > slot {
                *idx -= 1;
            }
        }

        if removed.is_some() {
            self.bump_shape_version();
        }
        removed
    }

    #[inline(always)]
    pub fn get_value_at_slot(&self, slot: usize) -> Option<Value> {
        self.values.get(slot).copied()
    }

    /// # Safety
    /// `slot` must be within bounds of the values array.
    #[inline(always)]
    pub unsafe fn get_value_at_slot_unchecked(&self, slot: usize) -> Value {
        *self.values.get_unchecked(slot)
    }

    pub fn ordered_keys_ref(&self) -> Vec<HashKey> {
        self.pairs.keys().cloned().collect()
    }

    /// Get by symbol ID — reads from `values` via `str_slots`.
    #[inline(always)]
    pub fn get_by_sym(&self, sym: u32) -> Option<Value> {
        self.str_slots
            .get(&sym)
            .and_then(|&slot| self.values.get(slot).copied())
    }

    /// Get by string key — interns to u32 then looks up via `str_slots`.
    pub fn get_by_str(&self, s: &str) -> Option<Value> {
        let sym = intern::intern(s);
        self.get_by_sym(sym)
    }

    /// Update/insert by symbol ID while preserving fast slot layout.
    /// Fast path: existing sym → direct slot write (no insert_pair overhead).
    #[inline(always)]
    pub fn set_by_sym(&mut self, sym: u32, value: Value) {
        if self.frozen {
            return;
        }
        // Fast path: update existing slot via str_slots (avoids insert_pair indirection)
        if let Some(&slot) = self.str_slots.get(&sym) {
            self.values[slot] = value;
            self.pairs_dirty = true;
            return;
        }
        // Slow path: new property — full insert
        self.insert_pair(HashKey::Sym(sym), value);
    }

    /// Update/insert by `Rc<str>` key — interns to u32.
    pub fn set_by_str(&mut self, s: Rc<str>, value: Value) {
        let sym = intern::intern_rc(&s);
        self.insert_pair(HashKey::Sym(sym), value);
    }

    /// Write directly to a known slot index. Only valid when the slot was
    /// previously validated via inline cache (same shape_version).
    /// Only updates `values[slot]`; `pairs` is NOT synced here for performance.
    /// # Safety
    /// `slot` must be within bounds. Callers must ensure `sync_pairs_if_dirty()`
    /// is called before reading values from `pairs`.
    #[inline(always)]
    pub unsafe fn set_value_at_slot_unchecked(&mut self, slot: usize, value: Value) {
        *self.values.get_unchecked_mut(slot) = value;
        self.pairs_dirty = true;
    }

    /// Sync stale `pairs` values from `values` vec.
    /// Only needs to be called before operations that READ VALUES from `pairs`.
    #[inline(never)]
    pub fn sync_pairs(&mut self) {
        if !self.pairs_dirty {
            return;
        }
        for (i, (_, v)) in self.pairs.iter_mut().enumerate() {
            if i < self.values.len() {
                *v = self.values[i]; // Value is Copy
            }
        }
        self.pairs_dirty = false;
    }

    /// Conditionally sync pairs if dirty. Inline hint for the fast check.
    #[inline(always)]
    pub fn sync_pairs_if_dirty(&mut self) {
        if self.pairs_dirty {
            self.sync_pairs();
        }
    }

    /// Contains check by string key — interns and checks `str_slots`.
    pub fn contains_str(&self, s: &str) -> bool {
        let sym = intern::intern(s);
        self.str_slots.contains_key(&sym)
    }

    /// Check if this hash has any getter/setter accessors defined.
    #[inline(always)]
    pub fn has_accessors(&self) -> bool {
        self.getters.is_some() || self.setters.is_some()
    }

    /// Look up a getter by property name.
    #[inline]
    pub fn get_getter(&self, name: &str) -> Option<&CompiledFunctionObject> {
        self.getters.as_ref().and_then(|g| g.get(name))
    }

    /// Look up a setter by property name.
    #[inline]
    pub fn get_setter(&self, name: &str) -> Option<&CompiledFunctionObject> {
        self.setters.as_ref().and_then(|s| s.get(name))
    }

    /// Define a getter accessor on this object.
    pub fn define_getter(&mut self, name: String, func: CompiledFunctionObject) {
        self.getters
            .get_or_insert_with(|| Box::new(FxHashMap::default()))
            .insert(name, func);
    }

    /// Define a setter accessor on this object.
    pub fn define_setter(&mut self, name: String, func: CompiledFunctionObject) {
        self.setters
            .get_or_insert_with(|| Box::new(FxHashMap::default()))
            .insert(name, func);
    }

    /// Insert a key-value pair where the value is an Object. Primitives are
    /// converted to Value inline; heap types are stored in `local_objects`
    /// and referenced by index. Use this when no VM heap is available
    /// (e.g., compiler-constructed builtin globals).
    pub fn insert_pair_obj(&mut self, key: HashKey, obj: Object) {
        let val = match &obj {
            Object::Integer(v) => Value::from_i64(*v),
            Object::Float(v) => Value::from_f64(*v),
            Object::Boolean(v) => Value::from_bool(*v),
            Object::Null => Value::NULL,
            Object::Undefined => Value::UNDEFINED,
            _ => {
                let idx = self.local_objects.len() as u32;
                self.local_objects.push(obj);
                Value::from_heap(idx)
            }
        };
        self.insert_pair(key, val);
    }

    /// Migrate `local_objects` to the VM's main heap and remap all Value
    /// heap indices. Called once during VM constant loading. After this,
    /// `local_objects` is empty and all Values reference the VM heap.
    pub fn migrate_local_objects(&mut self, heap: &mut crate::value::Heap) {
        if self.local_objects.is_empty() {
            return;
        }
        // Track actual heap index for each local object (nested migrations may
        // insert additional objects, so a simple base+offset doesn't work).
        let mut index_map = Vec::with_capacity(self.local_objects.len());
        for obj in self.local_objects.drain(..) {
            // Recursively migrate nested hashes before allocating
            if let Object::Hash(ref inner_rc) = obj {
                unsafe { inner_rc.borrow_mut() }.migrate_local_objects(heap);
            }
            let idx = heap.alloc(obj);
            index_map.push(idx);
        }
        for val in self.values.iter_mut() {
            if val.is_heap() {
                *val = Value::from_heap(index_map[val.heap_index() as usize]);
            }
        }
        for (_, val) in self.pairs.iter_mut() {
            if val.is_heap() {
                *val = Value::from_heap(index_map[val.heap_index() as usize]);
            }
        }
    }
}

impl Object {
    pub fn object_type(&self) -> ObjectType {
        match self {
            Object::Integer(_) => ObjectType::Integer,
            Object::Float(_) => ObjectType::Float,
            Object::Boolean(_) => ObjectType::Boolean,
            Object::Null => ObjectType::Null,
            Object::Undefined => ObjectType::Undefined,
            Object::String(_) | Object::StringRope(_) => ObjectType::String,
            Object::RegExp(_) => ObjectType::Regexp,
            Object::Map(_) => ObjectType::Map,
            Object::Set(_) => ObjectType::Set,
            Object::Array(_) => ObjectType::Array,
            Object::Hash(_) => ObjectType::Hash,
            Object::CompiledFunction(_) => ObjectType::CompiledFunction,
            Object::Class(_) => ObjectType::Class,
            Object::BuiltinFunction(_) => ObjectType::Builtin,
            Object::Instance(_) => ObjectType::Instance,
            Object::BoundMethod(_) => ObjectType::BoundMethod,
            Object::SuperRef(_) => ObjectType::SuperRef,
            Object::Promise(_) => ObjectType::Promise,
            Object::Generator(_) => ObjectType::Generator,
            Object::Symbol(_, _) => ObjectType::Symbol,
            Object::ReturnValue(_) => ObjectType::ReturnValue,
            Object::Error(_) => ObjectType::Error,
            Object::ArrayBuffer(_) => ObjectType::ArrayBuffer,
            Object::TypedArray(_) => ObjectType::TypedArray,
            Object::DataView(_) => ObjectType::DataView,
            Object::BigInt(_) => ObjectType::BigInt,
            Object::Proxy(_) => ObjectType::Proxy,
        }
    }

    pub fn inspect(&self) -> String {
        match self {
            Object::Integer(v) => v.to_string(),
            Object::Float(v) => v.to_string(),
            Object::Boolean(v) => v.to_string(),
            Object::Null => "null".to_string(),
            Object::Undefined => "undefined".to_string(),
            Object::String(v) => v.to_string(),
            Object::RegExp(re) => format!("/{}/{}", re.pattern, re.flags),
            Object::Map(_) => "[Map]".to_string(),
            Object::Set(_) => "[Set]".to_string(),
            Object::Array(items) => {
                let borrowed = items.borrow();
                let mut result = String::from("[");
                for (i, x) in borrowed.iter().enumerate() {
                    if i > 0 { result.push_str(", "); }
                    result.push_str(&x.inspect_inline());
                }
                result.push(']');
                result
            }
            Object::Hash(h) => {
                let h = unsafe { h.borrow_mut() };
                h.sync_pairs_if_dirty();
                let mut result = String::from("{");
                for (i, (k, v)) in h.pairs.iter().enumerate() {
                    if i > 0 { result.push_str(", "); }
                    use std::fmt::Write;
                    let _ = write!(result, "{}: {}", k, v.inspect_inline());
                }
                result.push('}');
                result
            }
            Object::CompiledFunction(_) => "[CompiledFunction]".to_string(),
            Object::Class(c) => format!("[Class {}]", c.name),
            Object::BuiltinFunction(_) => "[BuiltinFunction]".to_string(),
            Object::Instance(i) => format!("[Instance {}]", i.class_name),
            Object::BoundMethod(_) => "[BoundMethod]".to_string(),
            Object::SuperRef(_) => "[SuperRef]".to_string(),
            Object::Promise(p) => match &p.borrow().settled {
                PromiseState::Pending => "[Promise pending]".to_string(),
                PromiseState::Fulfilled(v) => format!("[Promise fulfilled {}]", v.inspect()),
                PromiseState::Rejected(v) => format!("[Promise rejected {}]", v.inspect()),
            },
            Object::Generator(_) => "[Generator]".to_string(),
            Object::Symbol(id, desc) => match desc {
                Some(d) => format!("Symbol({})", d),
                None => format!("Symbol({})", id),
            },
            Object::ReturnValue(v) => v.inspect(),
            Object::Error(err) => format!("{}: {}", err.name, err.message),
            Object::StringRope(_) => "[StringRope]".to_string(),
            Object::ArrayBuffer(rc) => format!("[ArrayBuffer {} bytes]", rc.borrow().len()),
            Object::TypedArray(t) => {
                let buf = t.buffer.borrow();
                let mut result = format!("{}[", t.kind.name());
                let bpe = t.kind.bytes_per_element();
                for i in 0..t.length {
                    if i > 0 { result.push_str(", "); }
                    let off = t.byte_offset + i * bpe;
                    if off + bpe <= buf.len() {
                        let v = t.kind.read(&buf, off);
                        if v.is_finite() && v.fract() == 0.0 && !matches!(t.kind, TypedArrayKind::Float32 | TypedArrayKind::Float64) {
                            use std::fmt::Write;
                            let _ = write!(result, "{}", v as i64);
                        } else {
                            use std::fmt::Write;
                            let _ = write!(result, "{}", v);
                        }
                    }
                }
                result.push(']');
                result
            }
            Object::DataView(d) => format!(
                "[DataView byteLength={} byteOffset={}]",
                d.byte_length, d.byte_offset
            ),
            Object::BigInt(v) => format!("{}n", v),
            Object::Proxy(p) => format!("[Proxy {}]", p.target.inspect()),
        }
    }

    /// Flatten a StringRope to Rc<str> using heap access.
    /// For non-rope strings, returns the existing Rc<str>.
    pub fn as_string_with_heap(&self, heap: &crate::value::Heap) -> Option<Rc<str>> {
        match self {
            Object::String(s) => Some(s.clone()),
            Object::StringRope(rope) => Some(Rc::from(flatten_rope(rope, heap).as_str())),
            _ => None,
        }
    }

    /// JavaScript-style ToString conversion for string concatenation.
    /// Differs from inspect() for arrays (no brackets) and objects ("[object Object]").
    /// NOTE: Array items are Value (NaN-boxed) so we use inspect_inline() for them,
    /// which doesn't require heap access. Nested heap arrays will show as "[ref]"
    /// but this is acceptable for the common case.
    pub fn to_js_string(&self) -> String {
        match self {
            Object::Array(items) => {
                let borrowed = items.borrow();
                let mut result = String::new();
                for (i, x) in borrowed.iter().enumerate() {
                    if i > 0 { result.push(','); }
                    if !x.is_undefined() && !x.is_null() {
                        result.push_str(&x.inspect_inline());
                    }
                }
                result
            }
            Object::Hash(_) | Object::Instance(_) => "[object Object]".to_string(),
            Object::CompiledFunction(_) | Object::BuiltinFunction(_) | Object::BoundMethod(_) => {
                "function () { [native code] }".to_string()
            }
            _ => self.inspect(),
        }
    }
}

pub fn true_object() -> Object {
    Object::Boolean(true)
}

pub fn false_object() -> Object {
    Object::Boolean(false)
}

pub fn null_object() -> Object {
    Object::Null
}

pub fn undefined_object() -> Object {
    Object::Undefined
}

/// Flatten a string rope into a contiguous `String`.
/// Uses an iterative stack to avoid deep recursion.
pub fn flatten_rope(rope: &StringRopeNode, heap: &crate::value::Heap) -> String {
    let mut buf = String::with_capacity(rope.total_len);
    let mut work: Vec<crate::value::Value> = Vec::with_capacity(16);
    work.push(rope.right);
    work.push(rope.left);
    while let Some(val) = work.pop() {
        if val.is_inline_str() {
            let (b, len) = val.inline_str_buf();
            buf.push_str(unsafe { std::str::from_utf8_unchecked(&b[..len]) });
        } else if val.is_heap() {
            match heap.get(val.heap_index()) {
                Object::String(s) => buf.push_str(s),
                Object::StringRope(r) => {
                    // Push right first so left is processed first (stack LIFO)
                    work.push(r.right);
                    work.push(r.left);
                }
                _ => {}
            }
        }
    }
    buf
}

/// Get the length of a string Value (handles String, inline str, and StringRope).
pub fn string_val_len(val: crate::value::Value, heap: &crate::value::Heap) -> usize {
    if val.is_inline_str() {
        return val.inline_str_len();
    }
    if val.is_heap() {
        match heap.get(val.heap_index()) {
            Object::String(s) => s.len(),
            Object::StringRope(r) => r.total_len,
            _ => 0,
        }
    } else {
        0
    }
}

/// Create an `Object::Array` with reference semantics (Rc<VmCell<Vec<Object>>>).
pub fn make_array(items: Vec<Value>) -> Object {
    Object::Array(Rc::new(VmCell::new(items)))
}

/// Convenience: create an empty `Object::Array`.
pub fn make_empty_array() -> Object {
    Object::Array(Rc::new(VmCell::new(Vec::new())))
}

/// Create an `Object::Hash` with reference semantics (Rc<VmCell<HashObject>>).
pub fn make_hash(hash: HashObject) -> Object {
    Object::Hash(Rc::new(VmCell::new(hash)))
}

/// Extract `Vec<Object>` from an `Rc<VmCell<Vec<Object>>>`, avoiding clone when refcount is 1.
pub fn unwrap_array(rc: Rc<VmCell<Vec<Value>>>) -> Vec<Value> {
    match Rc::try_unwrap(rc) {
        Ok(cell) => cell.into_inner(),
        Err(rc) => rc.borrow().clone(),
    }
}
