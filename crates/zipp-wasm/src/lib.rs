//! A persistent zipp VM for browser hosts, over wasm-bindgen.
//!
//! The engine keeps one script alive across many re-entries: the host compiles
//! it once, then reads and writes its top-level bindings by slot, calls its
//! functions, and delivers events — the shape a UI runtime needs, as opposed to
//! `zipp js file.js`'s run-once-and-exit.
//!
//! Everything the script can reach outside itself is defined in `preamble.js`
//! as ordinary JavaScript, and reaches the host through exactly two channels:
//!
//! - `__zippHostCall(kind, ...args)` — SYNCHRONOUS, strings in and one string
//!   out. `db` and `localStorage` use it, because scripts call
//!   `db.query(...)` mid-expression and cannot await.
//! - a queue drained by [`Engine::drainPendingHostCalls`] — ASYNCHRONOUS, for
//!   `host.call(kind, args, cb)`, whose callback the host resolves later.
//!
//! The split is not stylistic: a synchronous bridge cannot await (although its
//! trusted host adapter can still perform arbitrary synchronous host work), and
//! an asynchronous one cannot be read inline.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use wasm_bindgen::prelude::*;
use zipp_vm::embed::{
    compile_script_with_preamble, CompileOptions, ConsoleStream, FingerprintBudget, HostCallError,
    HostCtx, HostValue, HostValueBudget, ScriptGoal, ScriptState, SymbolScope,
    DEFAULT_HOST_VALUE_MAX_NODES, DEFAULT_HOST_VALUE_MAX_STRING_BYTES,
};

// js-sys's stable Array::is_array/Object::keys/Array indexing bindings do not
// catch JavaScript exceptions. A revoked Proxy, or an ownKeys/length/index trap,
// can therefore unwind through WebAssembly while wasm-bindgen is holding the
// exported Engine's mutable WasmRefCell borrow. Rust destructors do not run on
// that path, leaving the Engine permanently "recursively borrowed". Host values
// are hostile boundary data, so use catch-enabled bindings for every operation
// that can invoke a Proxy trap.
#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = Array, js_name = isArray, catch)]
    fn try_array_is_array(value: &JsValue) -> Result<bool, JsValue>;

    #[wasm_bindgen(js_namespace = Object, js_name = keys, catch)]
    fn try_object_keys(value: &JsValue) -> Result<js_sys::Array, JsValue>;
}

const PREAMBLE: &str = include_str!("preamble.js");
const EVAL_PREFIX: &str = "JSON.stringify((function () { return (";
const EVAL_SUFFIX: &str = "); })())";
// `evalInContextRich` marshals the value itself; only the expression wrapper
// remains. It shares `evalInContext`'s per-expression ceiling, which is
// derived from the longer JSON wrapper, so the rich form is never the more
// permissive one.
const RICH_EVAL_PREFIX: &str = "(function () { return (";
const RICH_EVAL_SUFFIX: &str = "); })()";

// Sized for applications, not for snippets. Every one of these was small
// enough that ordinary media work hit it as a wall rather than as a guard:
// a Game Boy cartridge would not fit in a buffer, an 8-second audio frame
// would not fit in a string, base64 of either would not run in one call.
//
// The bound that matters is MAX_APPROX_HEAP_BYTES, which is charged against
// everything and stops a runaway bundle no matter which shape it allocates.
// The individual ceilings only ever refused ONE absurd request; they are now
// set where an absurd request actually begins.
//
// These are lifetime limits for one Engine. They are deliberately fixed at
// the embedding boundary: a guest must not be able to raise its own ceiling,
// and every browser host gets the same fail-closed defaults.
const MAX_INITIAL_SOURCE_BYTES: usize = 16 * 1024 * 1024;
const MAX_DYNAMIC_CODE_SOURCE_BYTES: usize = 64 * 1024;
// `evalInContext` adds a fixed host-controlled wrapper before it enters the
// same VM-wide dynamic compiler gate as guest `eval`/`Function`/ShadowRealm.
const MAX_EVAL_SOURCE_BYTES: usize =
    MAX_DYNAMIC_CODE_SOURCE_BYTES - EVAL_PREFIX.len() - EVAL_SUFFIX.len();
const MAX_EVAL_RETAINED_SOURCE_BYTES: usize = 1024 * 1024;
const MAX_EVAL_CALLS: u32 = 256;
// Sized for a guest that generates code, not for one that calls `eval` a
// handful of times. The previous ceilings -- 256 compiles and 1 MB of source
// for the life of the engine -- were the snippet-shaped bounds the comment
// above warns about. An emulator that compiles its hot paths into JavaScript,
// which is a real workload and one of the few ways a guest gets fast, hit them
// after 158 compiled traces and stopped dead: a spent dynamic-code allowance is
// terminal, not an exception the guest can catch and back off from.
//
// Retained source is the bound that matters here, because dynamic functions are
// leaked deliberately to keep stable addresses and are never reclaimed. 16 MB is
// where an absurd request begins: it is fixed for the life of one Engine, a
// guest still cannot raise it, and it stays far below MAX_APPROX_HEAP_BYTES,
// which remains the bound that catches a runaway bundle whatever shape it
// allocates.
const MAX_DYNAMIC_CODE_RETAINED_SOURCE_BYTES: usize = 16 * 1024 * 1024;
const MAX_DYNAMIC_CODE_CALLS: usize = 16384;
const MAX_DYNAMIC_CODE_FUNCTIONS: usize = 16384;
const MAX_DYNAMIC_CODE_CLASSES: usize = 1024;
const MAX_LIFETIME_STEPS: u64 = 50_000_000;
// The most a host may ask for through `setInstructionBudget`: forty times the
// default, which is the same order as the native embedders' budgets. A host
// can align with a sibling runtime; it cannot switch the fuse off.
const MAX_INSTRUCTION_BUDGET_STEPS: u64 = 2_000_000_000;
const MAX_APPROX_HEAP_BYTES: usize = 512 * 1024 * 1024;
// Whatever a script prints has to fit in one bounded `takeOutput()` — an array
// root plus one node per line — or the host is left holding buffered output it
// can never retrieve. That is a bound on the number of LINES, and this constant
// only bounds bytes, so the two are tied together by charging every line the
// cost of its own entry (see OUTPUT_LINE_OVERHEAD_BYTES in vm/instrument.rs).
// At 8 bytes a line this admits at most 1,048,576 of them, comfortably inside
// DEFAULT_HOST_VALUE_MAX_NODES.
//
// Before that charge existed an empty line cost a single byte, so a stream of
// them reached neither guard and grew until the instance trapped.
const MAX_LIFETIME_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
const MAX_SYNC_BRIDGE_KIND_BYTES: usize = 64;
const MAX_SYNC_BRIDGE_ARGS: usize = 16;
const MAX_SYNC_BRIDGE_BYTES: usize = 32 * 1024 * 1024;
const MAX_SYNC_CAPABILITY_ENTRIES: u32 = 32;
// The asynchronous queue, on the engine's side of the boundary (the preamble
// keeps guest-visible copies of the queue and pending counts, which are
// bookkeeping, not authority). One drain hands over at most
// MAX_HOST_CALL_DRAIN_REQUESTS requests and MAX_HOST_CALL_DRAIN_STRING_BYTES
// of string payload; what does not fit stays queued for the next drain. A
// single request that does not fit that allowance even on its own can never
// cross and is rejected with explicit settlement instead. The drain moves in
// chunks of MAX_HOST_CALL_DRAIN_CHUNK, halving on a conversion failure, so a
// bad request costs a bounded number of retries rather than a fresh walk of
// the whole queue.
const MAX_HOST_CALL_DRAIN_REQUESTS: u32 = 4096;
/// The preamble's guest-side queue bounds, repeated here so the profile can
/// report them; a unit test holds them to `preamble.js`.
const PREAMBLE_HOST_CALL_QUEUE_MAX: u32 = 4096;
const PREAMBLE_HOST_CALL_PENDING_MAX: u32 = 65536;
const PREAMBLE_HOST_CALL_REQUEST_MAX_UNITS: u32 = 4_194_304;
const MAX_HOST_CALL_DRAIN_CHUNK: u32 = 256;
const MAX_HOST_CALL_DRAIN_STRING_BYTES: usize = 32 * 1024 * 1024;
/// The largest request id the guest's counter can hand over exactly (the
/// largest safe integer). Ids are
/// JavaScript Numbers on both sides now; they used to cross as `u32`, so the
/// 2^32nd request could never be completed (the 11 September 2026 audit's
/// ZIPP-13).
const MAX_HOST_CALL_ID: f64 = 9_007_199_254_740_991.0;
// The `accel.make` binding spec is guest text: bound its size, its entry
// count and its identifier lengths before any of it is parsed or resolved.
const MAX_ACCEL_SPEC_BYTES: usize = 8 * 1024;
const MAX_ACCEL_SPEC_ENTRIES: usize = 64;
const MAX_ACCEL_SPEC_NAME_BYTES: usize = 64;
/// Compiled-function and trace-slot identifiers the accelerator exchanges
/// are non-negative safe integers; a parse that merely yields some `f64` is
/// not an identifier.
const MAX_ACCEL_ID: f64 = 9_007_199_254_740_991.0;

/// The grammar every guest is compiled under. Stated here rather than
/// inherited from the process (the 11 September 2026 audit's ZIPP-24): a
/// guest is a CommonJS-shaped script — sloppy unless its own prologue says
/// otherwise, top-level `return` legal — and `zippProfile()` reports exactly
/// that.
const GUEST_COMPILE_OPTIONS: CompileOptions = CompileOptions {
    goal: ScriptGoal::Compat,
};

/// Preamble bindings the host may address by slot even though it did not
/// declare them. `window` in particular is a two-way channel: hosts stash keys
/// on it and read them back, so it needs a stable index.
const EXPOSED_PREAMBLE: &[&str] = &["window", "navigator", "host"];

/// Top-level bindings declared by `preamble.js`. Keeping this manifest beside
/// the embedded source lets initialization filter plumbing names without a
/// second `compile_script(PREAMBLE)` probe. Under `safe-sandbox` compiled
/// Programs have stable addresses for the WASM instance lifetime, so avoiding
/// that redundant probe also avoids one permanent compiler allocation per
/// Engine. A unit test below compiles the preamble and keeps this list exact.
const PREAMBLE_BINDINGS: &[&str] = &[
    "__zEvents",
    "__zHostQueue",
    "__zHostCbs",
    "__zHostPending",
    "__zHostId",
    "__zHostQueueMax",
    "__zHostPendingMax",
    "__zHostRequestMaxUnits",
    "window",
    "navigator",
    "localStorage",
    "db",
    "host",
    "accel",
    "__zListenerTypes",
    "__zDispatchEvent",
    "__zPeekHostCalls",
    "__zCommitHostCalls",
    "__zRejectHostCall",
    "__zResolveHostCall",
    "__zCancelHostCall",
];

/// Route Rust panics to `console.error` with a message instead of a bare
/// `unreachable` trap — without this a panic in wasm is undiagnosable.
#[wasm_bindgen]
pub fn zipp_install_panic_hook() {
    console_error_panic_hook::set_once();
}

/// Runs when the module is instantiated, before the host can call anything
/// else. The clock install is not optional: wasm32 has no clock, and `Vm::new`
/// reads one, so a VM constructed before this ran would trap.
#[wasm_bindgen(start)]
pub fn zipp_start() {
    console_error_panic_hook::set_once();
    zipp_vm::install_clock(js_sys::Date::now, mono_now);
}

/// `performance.now()` where the host has one, else the wall clock — coarser
/// and not strictly monotonic, but never absent.
///
/// The method is invoked WITH the Performance object as its receiver. It used
/// to be called with an undefined receiver, which a receiver-strict host
/// (Node, and browsers' `Performance.prototype.now`) rejects with "Illegal
/// invocation"; the error was swallowed and every reading silently came from
/// `Date.now()` instead, on hosts that had a perfectly good monotonic clock
/// (the 6 September 2026 audit's Z07).
fn mono_now() -> f64 {
    js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str("performance"))
        .ok()
        .filter(|p| !p.is_undefined() && !p.is_null())
        .and_then(|performance| {
            js_sys::Reflect::get(&performance, &JsValue::from_str("now"))
                .ok()
                .filter(JsValue::is_function)
                .map(JsValue::unchecked_into::<js_sys::Function>)
                .and_then(|now| now.call0(&performance).ok())
        })
        .and_then(|v| v.as_f64())
        .unwrap_or_else(js_sys::Date::now)
}

/// The JS objects the host installs for the synchronous bridges. Shared with
/// the host-call closure, which outlives any single method call.
#[derive(Default)]
struct Bridges {
    db: Option<js_sys::Object>,
    local_storage: Option<js_sys::Object>,
    clipboard: Option<js_sys::Object>,
    /// The accelerator: compiles guest-generated numeric functions with the
    /// host's own engine and runs them over views of the guest's typed
    /// arrays. See `accel` in the preamble and [`Engine::set_accel_bridge`].
    accel: Option<js_sys::Object>,
    /// Exact synchronous operations this Engine was explicitly granted. A
    /// bridge handle and authority are deliberately separate: merely
    /// installing a host object must not expose all of its methods to a guest.
    allowed_sync_operations: HashSet<String>,
}

/// Compile-time-known preamble helpers, resolved to slots once after init so
/// the hot paths never look a name up.
#[derive(Default)]
struct Helpers {
    listener_types: Option<u32>,
    dispatch_event: Option<u32>,
    peek_host_calls: Option<u32>,
    commit_host_calls: Option<u32>,
    reject_host_call: Option<u32>,
    resolve_host_call: Option<u32>,
    cancel_host_call: Option<u32>,
}

/// A compiled script plus the live VM running it.
#[wasm_bindgen]
pub struct Engine {
    state: Option<ScriptState>,
    /// Script symbol name → global slot. Preamble names are excluded.
    slots: Vec<(String, u32, SymbolScope)>,
    helpers: Helpers,
    bridges: Rc<RefCell<Bridges>>,
    /// Number of lines the preamble adds, so a host can correct the line
    /// numbers in a compile error back to its own source.
    preamble_lines: u32,
    /// `evalInContext` installs stable-address definitions that are not broadly
    /// reclaimed on Engine disposal. Track both calls and exact wrapper-source
    /// bytes so one Engine has a strict contribution bound; hosts recycle the
    /// Worker/WASM instance to reclaim those definitions between tenants.
    eval_calls: u32,
    eval_retained_source_bytes: usize,
    /// Disposal is terminal: a disposed engine cannot acquire new bridges or
    /// be initialized with another tenant's script.
    disposed: bool,
    /// Set before compilation/top-level execution starts. `state` is populated
    /// only after successful initialization, so it cannot itself freeze bridge
    /// handles and grants against a callback during top-level execution.
    host_configuration_frozen: bool,
    /// The instruction allowance this engine runs under: the default, or what
    /// the host last asked for through `setInstructionBudget`. Held here as
    /// well as in the recorder so a request made BEFORE `initScript` governs
    /// top-level execution, and so a renewal restores the size the host chose
    /// rather than the default.
    instruction_budget: u64,
    /// The fingerprint key the host asked for, kept here so a request made
    /// BEFORE `initScript` — the natural place, next to the other host
    /// configuration — is applied when the state exists rather than silently
    /// dropped (the 11 September 2026 audit's ZIPP-07).
    fingerprint_seed: Option<u64>,
}

#[wasm_bindgen]
impl Engine {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Engine {
        INSTANCE_USAGE.with(|c| {
            let mut u = c.get();
            u.engines_created += 1;
            c.set(u);
        });
        Engine {
            state: None,
            slots: Vec::new(),
            helpers: Helpers::default(),
            bridges: Rc::new(RefCell::new(Bridges::default())),
            preamble_lines: PREAMBLE.lines().count() as u32,
            eval_calls: 0,
            eval_retained_source_bytes: 0,
            disposed: false,
            host_configuration_frozen: false,
            instruction_budget: MAX_LIFETIME_STEPS,
            fingerprint_seed: None,
        }
    }

    /// Lines the preamble prepends to the host's source.
    #[wasm_bindgen(getter, js_name = preambleLines)]
    pub fn preamble_lines(&self) -> u32 {
        self.preamble_lines
    }

    /// Install the object backing `db.*`. Its methods are called synchronously
    /// from inside VM execution, so they must not await. Installing a bridge
    /// does not grant any operation; call `setSyncHostCapabilities` separately.
    #[wasm_bindgen(js_name = setDbBridge)]
    pub fn set_db_bridge(&mut self, bridge: JsValue) -> Result<(), JsValue> {
        self.ensure_host_configuration_open()?;
        self.bridges.borrow_mut().db = Some(require_bridge(bridge, "db")?);
        Ok(())
    }

    /// Install the object backing `localStorage.*`. This never provides the
    /// clipboard bridge, even when the object happens to have clipboard-like
    /// methods.
    #[wasm_bindgen(js_name = setLocalStorageBridge)]
    pub fn set_local_storage_bridge(&mut self, bridge: JsValue) -> Result<(), JsValue> {
        self.ensure_host_configuration_open()?;
        self.bridges.borrow_mut().local_storage = Some(require_bridge(bridge, "localStorage")?);
        Ok(())
    }

    /// Install the object backing `navigator.clipboard.*`. Clipboard authority
    /// is intentionally separate from local storage authority.
    #[wasm_bindgen(js_name = setClipboardBridge)]
    pub fn set_clipboard_bridge(&mut self, bridge: JsValue) -> Result<(), JsValue> {
        self.ensure_host_configuration_open()?;
        self.bridges.borrow_mut().clipboard = Some(require_bridge(bridge, "clipboard")?);
        Ok(())
    }

    /// Install the object backing `accel.*`: a host that compiles guest-
    /// generated functions with its own engine. Its methods receive what
    /// the guest passed, except that `make` sees every `g:NAME` entry of the
    /// spec resolved to `r:address:length:kind` -- the region of engine
    /// memory holding that global's typed array, pinned for the VM's
    /// lifetime -- and `state` receives the region of the named array as
    /// three numbers. During `run` the host may call [`accelGuestCall`] to
    /// run a guest function by name with numbers.
    #[wasm_bindgen(js_name = setAccelBridge)]
    pub fn set_accel_bridge(&mut self, bridge: JsValue) -> Result<(), JsValue> {
        self.ensure_host_configuration_open()?;
        self.bridges.borrow_mut().accel = Some(require_bridge(bridge, "accel")?);
        Ok(())
    }

    /// Replace the exact allowlist for synchronous guest-to-host operations.
    /// The list is fixed before initialization so guest execution cannot race
    /// or influence a later authority upgrade. Unknown operation names reject
    /// the complete update rather than being silently ignored.
    #[wasm_bindgen(js_name = setSyncHostCapabilities)]
    pub fn set_sync_host_capabilities(&mut self, operations: JsValue) -> Result<(), JsValue> {
        self.ensure_host_configuration_open()?;
        if !checked_is_array(&operations, "synchronous host capabilities").map_err(to_js_error)? {
            return Err(JsValue::from_str(
                "TypeError: synchronous host capabilities must be an array",
            ));
        }
        let len = checked_array_length(&operations, "synchronous host capabilities")
            .map_err(to_js_error)?;
        if len > MAX_SYNC_CAPABILITY_ENTRIES {
            return Err(JsValue::from_str(
                "RangeError: too many synchronous host capability entries",
            ));
        }
        let mut allowed = HashSet::with_capacity(len as usize);
        for index in 0..len {
            let operation = checked_array_get(&operations, index, "synchronous host capabilities")
                .map_err(to_js_error)?;
            let Some(operation) = operation.as_string() else {
                return Err(JsValue::from_str(
                    "TypeError: synchronous host capability names must be strings",
                ));
            };
            if !is_allowed_sync_host_call(&operation) {
                return Err(JsValue::from_str(&format!(
                    "TypeError: unknown synchronous host capability '{operation}'"
                )));
            }
            allowed.insert(operation);
        }
        self.bridges.borrow_mut().allowed_sync_operations = allowed;
        Ok(())
    }

    /// Compile `source` behind the preamble, run its top level, and return the
    /// symbol map as `{ name: { index, scope } }`.
    ///
    /// Bridges should be installed first — a script's top level (and its
    /// `_init`) commonly reads `localStorage` or queries `db`.
    #[wasm_bindgen(js_name = initScript)]
    pub fn init_script(&mut self, source: &str) -> Result<JsValue, JsValue> {
        self.ensure_live()?;
        if self.state.is_some() {
            self.terminate();
            return Err(JsValue::from_str(
                "zipp: repeated initialization disposed this engine",
            ));
        }
        // Freeze authority before compilation and before any guest top-level
        // code can invoke a synchronous host bridge.
        self.host_configuration_frozen = true;
        // Reject before allocating the combined preamble+guest buffer or
        // entering the parser. Source size is a compile-time resource, so the
        // VM's execution/heap recorder cannot protect this path for us.
        if source.len() > MAX_INITIAL_SOURCE_BYTES {
            self.terminate();
            return Err(JsValue::from_str(&format!(
                "RangeError: initial script source exceeds the {MAX_INITIAL_SOURCE_BYTES}-byte limit"
            )));
        }
        // A failed first initialization must not leave partial symbol/helper
        // state that a later tenant can observe.
        self.slots.clear();
        self.helpers = Helpers::default();

        let result: Result<JsValue, JsValue> = (|| {
            // One program, two sources: the preamble's bindings are globals
            // the guest reaches by name, and the guest's OWN directive
            // prologue stays in force even though preamble statements now
            // precede it (the 11 September 2026 audit's ZIPP-01).
            let mut st = compile_script_with_preamble(PREAMBLE, source, &GUEST_COMPILE_OPTIONS)
                .map_err(|e| JsValue::from_str(&e))?;
            if let Some(seed) = self.fingerprint_seed {
                st.set_fingerprint_seed(seed);
            }

            // Attach all execution limits before the first guest instruction.
            // The Cargo dependency disables native JIT features; the explicit
            // runtime switch also keeps this true in native workspace builds
            // where Cargo feature unification may enable zipp-vm's JIT.
            // Top-level execution runs under the allowance the host chose
            // before initialization, or the default; see setInstructionBudget.
            st.set_limits(self.instruction_budget, None);
            st.set_dynamic_code_limits(
                MAX_DYNAMIC_CODE_SOURCE_BYTES,
                MAX_DYNAMIC_CODE_RETAINED_SOURCE_BYTES,
                MAX_DYNAMIC_CODE_CALLS,
                MAX_DYNAMIC_CODE_FUNCTIONS,
                MAX_DYNAMIC_CODE_CLASSES,
            );
            st.set_heap_limit(MAX_APPROX_HEAP_BYTES);
            st.set_output_limit(MAX_LIFETIME_OUTPUT_BYTES);
            st.disable_vm_jit();

            let bridges = Rc::clone(&self.bridges);
            st.set_host_call_ctx(Box::new(move |ctx, kind, args| {
                host_dispatch_ctx(&bridges, ctx, kind, args)
            }));

            let init = st.run_init();
            if let Some(error) = st.resource_limit_error() {
                return Err(JsValue::from_str(error));
            }
            init.map_err(|e| JsValue::from_str(&e))?;

            let mut slots = Vec::new();
            let mut exposed = Vec::new();
            for s in st.symbols() {
                // Preamble names are engine plumbing, with one exception: the host
                // needs a slot for the bridge objects it also writes to (it syncs
                // `window.__foo` keys both ways), so those stay visible. Hosts are
                // expected to exclude them from what they treat as script state.
                if PREAMBLE_BINDINGS.contains(&s.name.as_str())
                    && !EXPOSED_PREAMBLE.contains(&s.name.as_str())
                {
                    continue;
                }
                let scope = match s.scope {
                    SymbolScope::Function => "function",
                    SymbolScope::Variable => "variable",
                };
                exposed.push((
                    s.name.clone(),
                    HostValue::Object(vec![
                        ("index".into(), HostValue::Number(s.index as f64)),
                        ("scope".into(), HostValue::String(scope.into())),
                    ]),
                ));
                slots.push((s.name, s.index, s.scope));
            }

            let find = |n: &str| {
                st.symbols()
                    .into_iter()
                    .find(|s| s.name == n)
                    .map(|s| s.index)
            };
            let helpers = Helpers {
                listener_types: find("__zListenerTypes"),
                dispatch_event: find("__zDispatchEvent"),
                peek_host_calls: find("__zPeekHostCalls"),
                commit_host_calls: find("__zCommitHostCalls"),
                reject_host_call: find("__zRejectHostCall"),
                resolve_host_call: find("__zResolveHostCall"),
                cancel_host_call: find("__zCancelHostCall"),
            };
            let out = to_js(&HostValue::Object(exposed)).map_err(to_js_error)?;
            self.slots = slots;
            self.helpers = helpers;
            self.state = Some(st);
            Ok(out)
        })();
        if result.is_err() {
            self.terminate();
        }
        result
    }

    /// Read the global in `index`. Values that cannot cross as data (functions,
    /// classes, `Map`, `Date`, …) read as `null`.
    #[wasm_bindgen(js_name = getGlobalByIndex)]
    pub fn get_global_by_index(&mut self, index: u32) -> Result<JsValue, JsValue> {
        self.ensure_live()?;
        let Some(st) = self.state.as_mut() else {
            return Ok(JsValue::UNDEFINED);
        };
        let value = st.try_get_slot(index).map_err(to_js_error)?;
        to_js(&value).map_err(to_js_error)
    }

    /// Write the global in `index`. A slot currently holding a function or
    /// class is left alone, so a host that reads all globals and writes them
    /// back cannot destroy the script's own functions.
    #[wasm_bindgen(js_name = setGlobalByIndex)]
    pub fn set_global_by_index(&mut self, index: u32, value: JsValue) -> Result<(), JsValue> {
        self.ensure_live()?;
        let value = from_js(&value).map_err(to_js_error)?;
        if let Some(st) = self.state.as_mut() {
            st.set_slot(index, &value);
        }
        // Host writes allocate VM objects without executing a bytecode
        // instruction, so the periodic in-loop heap poll cannot see them.
        self.finish_execution(Ok(()))
    }

    /// Read many globals in one boundary crossing.
    #[wasm_bindgen(js_name = getGlobalsBatch)]
    pub fn get_globals_batch(&mut self, indices: JsValue) -> Result<JsValue, JsValue> {
        self.ensure_live()?;
        let mut budget = HostValueBudget::default();
        let indices = index_list(&indices, &mut budget).map_err(to_js_error)?;
        let out = js_sys::Array::new();
        if let Some(st) = self.state.as_mut() {
            budget.charge_node().map_err(to_js_error)?;
            budget.ensure_nodes(indices.len()).map_err(to_js_error)?;
            for i in indices {
                let value = st.try_get_slot(i).map_err(to_js_error)?;
                out.push(&to_js_bounded(&value, &mut budget).map_err(to_js_error)?);
            }
        }
        Ok(out.into())
    }

    /// Restore this engine's instruction budget.
    ///
    /// The budget is a lifetime total, which bounds a runaway script but also
    /// puts a fuse on every long-running embedder: an interactive application
    /// is tens of thousands of small calls, and 50M instructions is minutes of
    /// ordinary use. Call this BEFORE a re-entry and the bound becomes
    /// per-re-entry instead — no single call can run unbounded, which is the
    /// property a browser host actually needs, while the application lives as
    /// long as its host keeps calling it.
    ///
    /// Host-only, and that is the whole design: this is a method on the Engine
    /// binding, unreachable from guest code, so a guest still cannot raise its
    /// own ceiling. Returns false once a budget has actually been spent —
    /// exhaustion stays sticky and a torn-down engine stays torn down.
    #[wasm_bindgen(js_name = renewInstructionBudget)]
    pub fn renew_instruction_budget(&mut self) -> bool {
        match self.state.as_mut() {
            Some(st) => st.renew_step_budget(self.instruction_budget),
            None => false,
        }
    }

    /// Set this engine's instruction budget to `steps`, clamped to
    /// `[1, MAX_INSTRUCTION_BUDGET_STEPS]`.
    ///
    /// The default lifetime budget is sized for an interactive host. An
    /// embedder that runs the SAME script on more than one runtime — this
    /// module in the browser, the engine natively or under WASI on a server —
    /// needs the budgets to agree, or an expression can complete on one side
    /// and be cut off on the other. This is the host-side knob for that; the
    /// clamp is the fuse it cannot remove.
    ///
    /// Called BEFORE `initScript`, the allowance governs top-level execution
    /// and `_init` as well: it used to need existing script state, so the one
    /// phase a host most wants to bound — a stranger's top level — always ran
    /// under the default (the 6 September 2026 audit's Z06). Called after,
    /// it renews the running budget to the new size, and every later
    /// `renewInstructionBudget` restores that size rather than the default.
    ///
    /// The value's handling is defined, not incidental: a non-finite number
    /// selects the default; a fraction is truncated; zero and negatives clamp
    /// to one step; anything above the maximum clamps to it. Host-only, like
    /// renewal: a method on the Engine binding, unreachable from guest code.
    /// Setting the budget restores nothing else — heap, output and
    /// dynamic-code ceilings stay where setup left them. Returns false once a
    /// budget has actually been spent, exactly as renewal does, and on a
    /// disposed engine.
    #[wasm_bindgen(js_name = setInstructionBudget)]
    pub fn set_instruction_budget(&mut self, steps: f64) -> bool {
        if self.disposed {
            return false;
        }
        let steps = if steps.is_finite() {
            (steps.max(1.0).min(MAX_INSTRUCTION_BUDGET_STEPS as f64)) as u64
        } else {
            MAX_LIFETIME_STEPS
        };
        self.instruction_budget = steps;
        match self.state.as_mut() {
            Some(st) => st.renew_step_budget(steps),
            // Recorded; applied when initScript attaches the limits.
            None => true,
        }
    }

    /// Key this engine's global fingerprints with host randomness.
    ///
    /// Supply two halves of a 64-bit value from a real random source. The
    /// digest mixer is invertible, so an unkeyed digest can be SOLVED for a
    /// collision — a host skipping reads on matching digests would mirror
    /// stale state while the guest moved on. The key is never exposed to guest
    /// code and never needs to be stable, since digests are only compared with
    /// earlier digests from the same engine.
    ///
    /// May be called before or after `initScript`; a seed set before is
    /// applied at initialization, and the same seed gives the same digests
    /// either way. Changing the seed changes every digest, so a host that
    /// caches digests must discard them when it re-keys.
    #[wasm_bindgen(js_name = setFingerprintSeed)]
    pub fn set_fingerprint_seed(&mut self, lo: u32, hi: u32) {
        let seed = ((hi as u64) << 32) | lo as u64;
        self.fingerprint_seed = Some(seed);
        if let Some(st) = self.state.as_mut() {
            st.set_fingerprint_seed(seed);
        }
    }

    /// Fingerprint many globals in one boundary crossing.
    ///
    /// One number per index: equal numbers mean `getGlobalsBatch` would
    /// return an equal value, so a host can skip reading the ones that have not
    /// moved. `NaN` means "unknown, read it", which is what a value too large
    /// to walk reports — the fallback is always the old always-read behaviour.
    /// The whole batch, duplicate indices included, walks under one work
    /// budget with the same node and string-byte ceilings as a batched read,
    /// so the digest never does more work than the read it stands in for
    /// could, and every element, hole, key and string byte counts.
    ///
    /// Digests are 53-bit so they land exactly in a JS number. At that width a
    /// collision across a UI's worth of state is not a practical concern, and the
    /// cost of one would be a skipped update, not corruption.
    ///
    /// Built with the same Array and from_f64 that getGlobalsBatch uses, rather
    /// than the Float64Array this obviously wants to be. A typed array pulls in
    /// `__wbg_new_with_length` and `__wbg_set_index`, and the host import surface
    /// is audited: check-wasm-memory.cjs pins the exact set of functions this
    /// module may call out to. Widening that list to save an allocation on a
    /// path that runs once per frame is a bad trade — the point of pinning it is
    /// that it only moves deliberately.
    #[wasm_bindgen(js_name = getGlobalsFingerprint)]
    pub fn get_globals_fingerprint(&mut self, indices: JsValue) -> Result<JsValue, JsValue> {
        self.ensure_live()?;
        let mut budget = HostValueBudget::default();
        let indices = index_list(&indices, &mut budget).map_err(to_js_error)?;
        let out = js_sys::Array::new();
        if let Some(st) = self.state.as_mut() {
            let mut work = FingerprintBudget::default();
            for i in indices {
                let cell = match st.fingerprint_slot_bounded(i, &mut work) {
                    Some(h) => (h & ((1u64 << 53) - 1)) as f64,
                    None => f64::NAN,
                };
                out.push(&JsValue::from_f64(cell));
            }
        }
        Ok(out.into())
    }

    /// Write many globals in one boundary crossing.
    ///
    /// Strict arity: `indices` and `values` must have the same length, and an
    /// index may appear only once. Both are checked before any value is
    /// converted or any slot written, so a host-side construction mistake is
    /// a `TypeError` rather than a partial write (a short `values` used to
    /// write `undefined` into the remaining slots, and extra values were
    /// silently ignored — the 11 September 2026 audit's ZIPP-10). A hole in
    /// `values` is an explicit `undefined`. All values are converted before
    /// the first slot is written, as before.
    #[wasm_bindgen(js_name = setGlobalsBatch)]
    pub fn set_globals_batch(&mut self, indices: JsValue, values: JsValue) -> Result<(), JsValue> {
        self.ensure_live()?;
        let mut budget = HostValueBudget::default();
        let idx = index_list(&indices, &mut budget).map_err(to_js_error)?;
        require_array(&values, "values").map_err(to_js_error)?;
        let values_len = checked_array_length(&values, "values").map_err(to_js_error)?;
        if values_len as usize != idx.len() {
            return Err(JsValue::from_str(&format!(
                "TypeError: setGlobalsBatch: indices and values must have the same length ({} indices, {} values)",
                idx.len(),
                values_len
            )));
        }
        let mut distinct = HashSet::with_capacity(idx.len());
        for i in &idx {
            if !distinct.insert(*i) {
                return Err(JsValue::from_str(&format!(
                    "TypeError: setGlobalsBatch: index {i} appears more than once"
                )));
            }
        }
        budget.charge_node().map_err(to_js_error)?;
        budget.ensure_nodes(idx.len()).map_err(to_js_error)?;
        let seen = js_sys::WeakSet::<js_sys::Object>::new_typed();
        let mut converted = Vec::with_capacity(idx.len());
        for (n, i) in idx.into_iter().enumerate() {
            let raw = checked_array_get(&values, n as u32, "values").map_err(to_js_error)?;
            let value = from_js_bounded(&raw, 0, &seen, &mut budget).map_err(to_js_error)?;
            converted.push((i, value));
        }
        if let Some(st) = self.state.as_mut() {
            for (i, value) in converted {
                st.set_slot(i, &value);
            }
        }
        self.finish_execution(Ok(()))
    }

    /// Call the top-level function `name`. Microtasks are drained before this
    /// returns, so promise callbacks the call scheduled have already run.
    #[wasm_bindgen(js_name = callFunction)]
    pub fn call_function(&mut self, name: &str, args: JsValue) -> Result<JsValue, JsValue> {
        self.ensure_live()?;
        let slot = self
            .slots
            .iter()
            .find(|(n, _, _)| n == name)
            .map(|(_, i, _)| *i)
            .ok_or_else(|| JsValue::from_str(&format!("zipp: no such function '{name}'")))?;
        let argv = match from_js(&args).map_err(to_js_error)? {
            HostValue::Undefined | HostValue::Null => Vec::new(),
            HostValue::Array(items) => items,
            _ => {
                return Err(JsValue::from_str(
                    "TypeError: call arguments must be an array",
                ))
            }
        };
        let result = self
            .state
            .as_mut()
            .ok_or_else(|| JsValue::from_str("zipp: not initialized"))?
            .call_slot(slot, &argv);
        let value = self.finish_execution(result)?;
        to_js(&value).map_err(to_js_error)
    }

    /// Evaluate `expr` in the script's global context and return its value
    /// as a JSON PROJECTION: the result is passed through the guest's
    /// `JSON.stringify` and parsed on the host side. That contract differs
    /// from the rich-value one `callFunction` and the slot APIs use, and the
    /// differences are the ones `JSON.stringify` makes — `undefined`, a
    /// function or a symbol result is `undefined`; `NaN` and the infinities
    /// become `null`; `-0` becomes `0`; a BigInt throws; `toJSON` and
    /// getters run; own enumerable data properties cross and accessors are
    /// evaluated; a cycle throws — and a guest that replaced its `JSON`
    /// facilities changes the answer, which is reported as an error rather
    /// than as `undefined`. [`Engine::evalInContextRich`] is the rich-value
    /// form. Polling state should use the slot/batch APIs, which neither
    /// compile nor project.
    ///
    /// Each call compiles fresh and installs stable-address definitions, so this
    /// is for one-off host queries — never a per-frame path. Use
    /// [`Engine::callFunction`] there.
    #[wasm_bindgen(js_name = evalInContext)]
    pub fn eval_in_context(&mut self, expr: &str) -> Result<JsValue, JsValue> {
        // Route the result through JSON so structured values survive; the
        // shallow `eval_in_context` marshaller would render them as ToString.
        let wrapped = self.account_eval("evalInContext", expr, EVAL_PREFIX, EVAL_SUFFIX)?;
        let result = self
            .state
            .as_mut()
            .expect("initialization checked by account_eval")
            .eval_in_context(&wrapped);
        let value = self.finish_execution(result)?;
        match value.as_str() {
            Some(s) => {
                let mut budget = HostValueBudget::default();
                budget.charge_node().map_err(to_js_error)?;
                budget.charge_string(s).map_err(to_js_error)?;
                // The projection is the guest's `JSON.stringify` output. Text
                // that does not parse means the guest replaced that facility;
                // report it instead of answering `undefined` as if the
                // expression had produced nothing.
                let parsed = js_sys::JSON::parse(s).map_err(|_| {
                    JsValue::from_str(
                        "SyntaxError: evalInContext result is not valid JSON (has the guest replaced JSON.stringify?)",
                    )
                })?;
                let value = from_js(&parsed).map_err(to_js_error)?;
                to_js(&value).map_err(to_js_error)
            }
            // `JSON.stringify` yields undefined for a function or undefined.
            None => Ok(JsValue::UNDEFINED),
        }
    }

    /// Evaluate `expr` in the script's global context and return its value
    /// as STRUCTURED DATA, under the same contract as `callFunction` and the
    /// slot reads: `-0`, `NaN` and the infinities cross as themselves, a
    /// function, class, `Map`, `Date`, typed array or proxy reads as `null`,
    /// a cycle reads as `null`, accessors are not invoked, and the result is
    /// bounded by the host-value conversion budget. Nothing is stringified
    /// and the guest's `JSON` facilities are not involved. Microtasks the
    /// evaluation schedules are drained before this returns, as they are for
    /// `callFunction`.
    ///
    /// Shares `evalInContext`'s lifetime ceilings (per-expression bytes,
    /// retained wrapper source, call count) and its cost: each call compiles
    /// and retains a program. One-off host queries only.
    #[wasm_bindgen(js_name = evalInContextRich)]
    pub fn eval_in_context_rich(&mut self, expr: &str) -> Result<JsValue, JsValue> {
        let wrapped = self.account_eval(
            "evalInContextRich",
            expr,
            RICH_EVAL_PREFIX,
            RICH_EVAL_SUFFIX,
        )?;
        let mut budget = HostValueBudget::default();
        let result = self
            .state
            .as_mut()
            .expect("initialization checked by account_eval")
            .eval_in_context_rich(&wrapped, &mut budget);
        // A resource ceiling is terminal whatever the typed error says.
        let result = result.map_err(HostCallError::into_message);
        let value = self.finish_execution(result)?;
        // The JS-side conversion has its own copy of the same budget shape.
        let mut js_budget = HostValueBudget::default();
        to_js_bounded(&value, &mut js_budget).map_err(to_js_error)
    }

    /// What this engine currently retains and has spent, as a plain object a
    /// host can read between re-entries (cheap: no walk). The first stage of
    /// the 11 September 2026 audit's ZIPP-06 — measure retained compiled code
    /// before attempting to reclaim it:
    ///
    /// - `heapBytes`: the payload-aware guest heap estimate the heap ceiling
    ///   is enforced against;
    /// - `stepsUsed`: bytecode instructions executed under the current budget;
    /// - `instructionBudget`: the allowance the host chose;
    /// - `evalCalls`, `evalRetainedSourceBytes`: this engine's `evalInContext`
    ///   / `evalInContextRich` accounting;
    /// - `dynamicCodeCalls`, `dynamicCodeSourceBytes`: every dynamic
    ///   compilation attempt (`eval`, `Function`, `ShadowRealm`, host eval)
    ///   and the source bytes charged;
    /// - `retainedFunctions`, `retainedClasses`: stable-address definitions
    ///   retained by successful dynamic compilations — the figure
    ///   `dispose()` does NOT reclaim within one WASM instance, so a host
    ///   recycles the Worker/WASM instance when their sum across tenants
    ///   passes what it accepts;
    /// - `programFunctions`, `programBytecodeBytes`, `programSourceBytes`: the
    ///   size of this engine's own compiled program, which IS freed with the
    ///   engine (B306 stage two);
    /// - `consoleLinesBuffered`, `consoleBytesLifetime`, `pinnedBuffers`.
    ///
    /// These are exact counts, not allocator bytes: compare them with the
    /// WASM instance's `memory.buffer.byteLength` (linear-memory high-water)
    /// and the process's own memory, which this module cannot see.
    #[wasm_bindgen(js_name = resourceUsage)]
    pub fn resource_usage(&mut self) -> Result<JsValue, JsValue> {
        self.ensure_live()?;
        let usage = self
            .state
            .as_ref()
            .map(ScriptState::resource_usage)
            .unwrap_or_default();
        let n = |v: usize| HostValue::Number(v as f64);
        to_js(&HostValue::Object(vec![
            ("heapBytes".into(), n(usage.heap_bytes)),
            (
                "stepsUsed".into(),
                HostValue::Number(usage.steps_used as f64),
            ),
            (
                "instructionBudget".into(),
                HostValue::Number(self.instruction_budget as f64),
            ),
            ("evalCalls".into(), n(self.eval_calls as usize)),
            (
                "evalRetainedSourceBytes".into(),
                n(self.eval_retained_source_bytes),
            ),
            ("dynamicCodeCalls".into(), n(usage.dynamic_code_calls)),
            (
                "dynamicCodeSourceBytes".into(),
                n(usage.dynamic_code_source_bytes),
            ),
            ("retainedFunctions".into(), n(usage.retained_functions)),
            ("retainedClasses".into(), n(usage.retained_classes)),
            (
                "consoleLinesBuffered".into(),
                n(usage.console_lines_buffered),
            ),
            (
                "consoleBytesLifetime".into(),
                n(usage.console_bytes_lifetime),
            ),
            ("pinnedBuffers".into(), n(usage.pinned_buffers)),
            ("programFunctions".into(), n(usage.program_functions)),
            (
                "programBytecodeBytes".into(),
                n(usage.program_bytecode_bytes),
            ),
            ("programSourceBytes".into(), n(usage.program_source_bytes)),
        ]))
        .map_err(to_js_error)
    }

    /// Event types the script has registered listeners for, e.g. `["keydown"]`.
    #[wasm_bindgen(js_name = getEventListenerTypes)]
    pub fn get_event_listener_types(&mut self) -> Result<JsValue, JsValue> {
        self.ensure_live()?;
        let (Some(slot), Some(st)) = (self.helpers.listener_types, self.state.as_mut()) else {
            return Ok(js_sys::Array::new().into());
        };
        let result = st.call_slot(slot, &[]);
        let value = self.finish_execution(result)?;
        let value = match value {
            HostValue::Array(items) => HostValue::Array(
                items
                    .into_iter()
                    .filter(|it| matches!(it, HostValue::String(_)))
                    .collect(),
            ),
            _ => HostValue::Array(Vec::new()),
        };
        to_js(&value).map_err(to_js_error)
    }

    /// Deliver `event` to every listener registered for `type`, returning how
    /// many ran. The event object is given a no-op `preventDefault` if the host
    /// did not supply one, since scripts call it unconditionally.
    #[wasm_bindgen(js_name = dispatchEvent)]
    pub fn dispatch_event(&mut self, event_type: &str, event: JsValue) -> Result<u32, JsValue> {
        self.ensure_live()?;
        let (Some(slot), Some(st)) = (self.helpers.dispatch_event, self.state.as_mut()) else {
            return Ok(0);
        };
        let mut budget = HostValueBudget::default();
        budget.charge_node().map_err(to_js_error)?;
        budget.charge_string(event_type).map_err(to_js_error)?;
        let seen = js_sys::WeakSet::<js_sys::Object>::new_typed();
        let event = from_js_bounded(&event, 0, &seen, &mut budget).map_err(to_js_error)?;
        let args = [HostValue::String(event_type.to_string()), event];
        let result = st.call_slot(slot, &args);
        match self.finish_execution(result)? {
            HostValue::Number(n) => Ok(n as u32),
            _ => Ok(0),
        }
    }

    /// Take the `host.call(...)` requests the script has queued, as
    /// `[{ id, kind, args }]`, oldest first.
    ///
    /// The transfer is transactional. A request leaves the guest queue only
    /// once its host representation exists: the engine reads a bounded
    /// prefix, converts it, and commits exactly that prefix, so a conversion
    /// failure leaves the queue intact and is retried with a smaller prefix.
    /// Every accepted request therefore ends in one of three states —
    /// delivered here exactly once, still queued for the next drain (when the
    /// per-drain request or byte allowance is used up), or, for a single
    /// request too large to cross even on its own, rejected: it is removed and its
    /// callback is invoked with a `RangeError`, so no callback is left
    /// pending for a request the host will never see. A host that wants an
    /// empty queue keeps draining until this returns an empty array.
    ///
    /// Until the 11 September 2026 audit's ZIPP-02 the guest helper emptied
    /// the queue before its return value crossed the converter, so a
    /// conversion failure discarded every queued request while their
    /// callbacks stayed registered forever.
    #[wasm_bindgen(js_name = drainPendingHostCalls)]
    pub fn drain_pending_host_calls(&mut self) -> Result<JsValue, JsValue> {
        self.ensure_live()?;
        let out = js_sys::Array::new();
        let (Some(peek), Some(commit), Some(reject)) = (
            self.helpers.peek_host_calls,
            self.helpers.commit_host_calls,
            self.helpers.reject_host_call,
        ) else {
            return Ok(out.into());
        };
        if self.state.is_none() {
            return Ok(out.into());
        }
        // Two aggregate budgets for the whole drain, one per conversion stage
        // (VM graph to host values, host values to JS), each charged only by
        // committed prefixes.
        let mut walk_budget = HostValueBudget::new(
            DEFAULT_HOST_VALUE_MAX_NODES,
            MAX_HOST_CALL_DRAIN_STRING_BYTES,
        );
        let mut js_budget = HostValueBudget::new(
            DEFAULT_HOST_VALUE_MAX_NODES,
            MAX_HOST_CALL_DRAIN_STRING_BYTES,
        );
        let mut chunk = MAX_HOST_CALL_DRAIN_CHUNK;
        let mut delivered: u32 = 0;
        loop {
            if delivered >= MAX_HOST_CALL_DRAIN_REQUESTS {
                break;
            }
            let want = chunk.min(MAX_HOST_CALL_DRAIN_REQUESTS - delivered);
            let mut attempt_walk = walk_budget.clone();
            let mut attempt_js = js_budget.clone();
            match self.peek_host_calls(peek, want, &mut attempt_walk, &mut attempt_js)? {
                Ok(items) => {
                    // Engine-built, so the audited Reflect bindings suffice
                    // (no new host import for the artifact's pinned surface).
                    let count =
                        checked_array_length(&items, "host call batch").map_err(to_js_error)?;
                    if count == 0 {
                        break;
                    }
                    // Commit BEFORE appending: the guest queue is the source of
                    // truth until the prefix is off it.
                    self.call_helper(commit, &[HostValue::Number(count as f64)])?;
                    for i in 0..count {
                        out.push(
                            &checked_array_get(&items, i, "host call batch")
                                .map_err(to_js_error)?,
                        );
                    }
                    walk_budget = attempt_walk;
                    js_budget = attempt_js;
                    delivered += count;
                    if count < want {
                        break;
                    }
                    chunk = (chunk * 2).min(MAX_HOST_CALL_DRAIN_CHUNK);
                }
                Err(_) if want > 1 => {
                    chunk = (want / 2).max(1);
                }
                Err(reason) => {
                    // One request did not fit under the drain's remaining
                    // allowance. Alone, under a fresh full allowance, it
                    // either fits — then the AGGREGATE is what ran out and
                    // the request stays queued for the next drain — or it
                    // does not, and it can never cross: settle it.
                    let mut solo_walk = HostValueBudget::new(
                        DEFAULT_HOST_VALUE_MAX_NODES,
                        MAX_HOST_CALL_DRAIN_STRING_BYTES,
                    );
                    let mut solo_js = HostValueBudget::new(
                        DEFAULT_HOST_VALUE_MAX_NODES,
                        MAX_HOST_CALL_DRAIN_STRING_BYTES,
                    );
                    if delivered > 0
                        && self
                            .peek_host_calls(peek, 1, &mut solo_walk, &mut solo_js)?
                            .is_ok()
                    {
                        break;
                    }
                    let message = format!(
                        "host.call: request exceeds the {MAX_HOST_CALL_DRAIN_STRING_BYTES}-byte transport limit ({reason})"
                    );
                    let removed = self.call_helper(reject, &[HostValue::String(message)])?;
                    if !matches!(removed, HostValue::Number(n) if n == 1.0) {
                        break;
                    }
                    chunk = MAX_HOST_CALL_DRAIN_CHUNK;
                }
            }
        }
        Ok(out.into())
    }

    /// Invoke the callback the script passed to `host.call` for `call_id`,
    /// returning whether one was pending. `false` means the id is unknown:
    /// never issued, already completed, or cancelled — a late or duplicate
    /// completion is a no-op, not an error.
    ///
    /// The id is the JavaScript Number the guest's counter produced; it is
    /// validated as a finite positive integer up to 2^53 rather than
    /// truncated to 32 bits, so the 2^32nd request can be completed like any
    /// other (the 11 September 2026 audit's ZIPP-13). Guest-issued ids are
    /// bookkeeping, not authorization: a host must still bind each completion
    /// to the Worker/tenant generation that issued the request.
    #[wasm_bindgen(js_name = resolveHostCallback)]
    pub fn resolve_host_callback(
        &mut self,
        call_id: f64,
        result: JsValue,
    ) -> Result<bool, JsValue> {
        self.ensure_live()?;
        let call_id = host_call_id(call_id)?;
        let (Some(slot), Some(st)) = (self.helpers.resolve_host_call, self.state.as_mut()) else {
            return Ok(false);
        };
        let mut budget = HostValueBudget::default();
        budget.charge_node().map_err(to_js_error)?;
        let seen = js_sys::WeakSet::<js_sys::Object>::new_typed();
        let result = from_js_bounded(&result, 0, &seen, &mut budget).map_err(to_js_error)?;
        let args = [HostValue::Number(call_id), result];
        let result = st.call_slot(slot, &args);
        Ok(matches!(
            self.finish_execution(result)?,
            HostValue::Number(n) if n == 1.0
        ))
    }

    /// Release the callback pending for `call_id` WITHOUT invoking it — the
    /// host cancelled or timed the request out. Returns whether one was
    /// pending; a later `resolveHostCallback` for the same id then reports
    /// `false` and runs nothing.
    #[wasm_bindgen(js_name = cancelHostCallback)]
    pub fn cancel_host_callback(&mut self, call_id: f64) -> Result<bool, JsValue> {
        self.ensure_live()?;
        let call_id = host_call_id(call_id)?;
        let (Some(slot), Some(st)) = (self.helpers.cancel_host_call, self.state.as_mut()) else {
            return Ok(false);
        };
        let result = st.call_slot(slot, &[HostValue::Number(call_id)]);
        Ok(matches!(
            self.finish_execution(result)?,
            HostValue::Number(n) if n == 1.0
        ))
    }

    /// Run pending microtasks without calling into the script.
    #[wasm_bindgen]
    pub fn pump(&mut self) -> Result<(), JsValue> {
        self.ensure_live()?;
        if let Some(st) = self.state.as_mut() {
            st.pump();
        }
        self.finish_execution(Ok(()))
    }

    /// Drain every console line produced so far — `log`/`info`/`debug` and
    /// `warn`/`error` alike — in the order they were written. (The two
    /// streams used to be concatenated, stdout first, so interleaved
    /// messages lost their order: the 11 September 2026 audit's ZIPP-14.)
    /// `takeConsole` returns the same lines tagged with their stream.
    #[wasm_bindgen(js_name = takeOutput)]
    pub fn take_output(&mut self) -> Result<JsValue, JsValue> {
        self.ensure_live()?;
        let mut lines = Vec::new();
        if let Some(st) = self.state.as_mut() {
            for (_, line) in st.take_console() {
                lines.push(HostValue::String(line));
            }
        }
        to_js(&HostValue::Array(lines)).map_err(to_js_error)
    }

    /// Drain every console line produced so far, in order, as
    /// `[{ stream: "stdout" | "stderr", text }]`. Draining here empties the
    /// same buffers `takeOutput` drains.
    #[wasm_bindgen(js_name = takeConsole)]
    pub fn take_console(&mut self) -> Result<JsValue, JsValue> {
        self.ensure_live()?;
        let mut records = Vec::new();
        if let Some(st) = self.state.as_mut() {
            for (stream, line) in st.take_console() {
                let stream = match stream {
                    ConsoleStream::Stdout => "stdout",
                    ConsoleStream::Stderr => "stderr",
                };
                records.push(HostValue::Object(vec![
                    ("stream".into(), HostValue::String(stream.into())),
                    ("text".into(), HostValue::String(line)),
                ]));
            }
        }
        to_js(&HostValue::Array(records)).map_err(to_js_error)
    }

    /// Tear the VM down. The engine is unusable afterwards.
    #[wasm_bindgen]
    pub fn dispose(&mut self) {
        self.terminate();
    }
}

impl Default for Engine {
    fn default() -> Self {
        Engine::new()
    }
}

impl Engine {
    fn ensure_live(&self) -> Result<(), JsValue> {
        if self.disposed {
            Err(JsValue::from_str("zipp: engine is disposed"))
        } else {
            Ok(())
        }
    }

    fn ensure_host_configuration_open(&self) -> Result<(), JsValue> {
        self.ensure_live()?;
        if self.host_configuration_frozen {
            Err(JsValue::from_str(
                "zipp: host bridge configuration is immutable after initialization starts",
            ))
        } else {
            Ok(())
        }
    }

    /// Resource exhaustion is terminal. The status comes from the recorder,
    /// never from exception text a guest can spoof, and is checked even when a
    /// microtask converted the failure into a rejected promise. Ordinary guest
    /// throws remain recoverable and preserve the existing API contract.
    fn finish_execution<T>(&mut self, result: Result<T, String>) -> Result<T, JsValue> {
        let resource_error = self
            .state
            .as_mut()
            .and_then(ScriptState::resource_limit_error);
        if let Some(error) = resource_error {
            let error = JsValue::from_str(error);
            self.terminate();
            return Err(error);
        }
        result.map_err(|error| JsValue::from_str(&error))
    }

    /// Read the first `want` queued requests through the preamble's peek
    /// helper and convert them to a JS array, charging `walk` and `js`. The
    /// outer `Err` is terminal (a resource ceiling, or a guest throw from a
    /// tampered helper); the inner `Err` is a conversion failure carrying the
    /// limit that was crossed, which the drain answers by trying less.
    fn peek_host_calls(
        &mut self,
        peek: u32,
        want: u32,
        walk: &mut HostValueBudget,
        js: &mut HostValueBudget,
    ) -> Result<Result<JsValue, String>, JsValue> {
        let st = self
            .state
            .as_mut()
            .ok_or_else(|| JsValue::from_str("zipp: not initialized"))?;
        let peeked = st.call_slot_bounded(peek, &[HostValue::Number(want as f64)], walk);
        if let Some(error) = st.resource_limit_error() {
            let error = JsValue::from_str(error);
            self.terminate();
            return Err(error);
        }
        let value = match peeked {
            Ok(value) => value,
            Err(HostCallError::Conversion(limit)) => return Ok(Err(limit)),
            Err(HostCallError::Thrown(message)) => return Err(JsValue::from_str(&message)),
        };
        if !matches!(value, HostValue::Array(_)) {
            return Err(JsValue::from_str("zipp: host call queue is not an array"));
        }
        Ok(to_js_bounded(&value, js))
    }

    /// Call a preamble helper by slot, with the usual terminal handling of a
    /// resource ceiling.
    fn call_helper(&mut self, slot: u32, args: &[HostValue]) -> Result<HostValue, JsValue> {
        let st = self
            .state
            .as_mut()
            .ok_or_else(|| JsValue::from_str("zipp: not initialized"))?;
        let result = st.call_slot(slot, args);
        self.finish_execution(result)
    }

    /// The lifetime accounting both eval entry points share: the
    /// per-expression source ceiling, the call count and the retained wrapper
    /// source, checked and charged BEFORE anything is compiled. Returns the
    /// wrapped source to evaluate. A ceiling crossed is terminal.
    fn account_eval(
        &mut self,
        api: &str,
        expr: &str,
        prefix: &str,
        suffix: &str,
    ) -> Result<String, JsValue> {
        self.ensure_live()?;
        if self.state.is_none() {
            return Err(JsValue::from_str("zipp: not initialized"));
        }
        if expr.len() > MAX_EVAL_SOURCE_BYTES {
            self.terminate();
            return Err(JsValue::from_str(&format!(
                "RangeError: {api} source exceeds the {MAX_EVAL_SOURCE_BYTES}-byte per-call limit"
            )));
        }
        if self.eval_calls >= MAX_EVAL_CALLS {
            self.terminate();
            return Err(JsValue::from_str(&format!(
                "RangeError: {api} exceeded its {MAX_EVAL_CALLS}-call lifetime limit"
            )));
        }
        let wrapped_len = prefix
            .len()
            .checked_add(expr.len())
            .and_then(|n| n.checked_add(suffix.len()))
            .ok_or_else(|| JsValue::from_str(&format!("RangeError: {api} source size overflow")))?;
        let retained = self
            .eval_retained_source_bytes
            .checked_add(wrapped_len)
            .ok_or_else(|| {
                JsValue::from_str(&format!("RangeError: {api} retained source size overflow"))
            })?;
        if retained > MAX_EVAL_RETAINED_SOURCE_BYTES {
            self.terminate();
            return Err(JsValue::from_str(&format!(
                "RangeError: {api} exceeded its {MAX_EVAL_RETAINED_SOURCE_BYTES}-byte retained-source lifetime limit"
            )));
        }
        self.eval_calls += 1;
        self.eval_retained_source_bytes = retained;
        let mut wrapped = String::with_capacity(wrapped_len);
        wrapped.push_str(prefix);
        wrapped.push_str(expr);
        wrapped.push_str(suffix);
        Ok(wrapped)
    }

    fn terminate(&mut self) {
        // Account what this engine leaves behind in the instance before the
        // state that knows the figures is dropped.
        if let Some(st) = self.state.as_ref() {
            let usage = st.resource_usage();
            INSTANCE_USAGE.with(|c| {
                let mut u = c.get();
                u.engines_disposed += 1;
                u.retained_functions += usage.retained_functions as u64;
                u.retained_classes += usage.retained_classes as u64;
                u.dynamic_code_calls += usage.dynamic_code_calls as u64;
                u.dynamic_code_source_bytes += usage.dynamic_code_source_bytes as u64;
                c.set(u);
            });
        }
        self.state = None;
        self.slots.clear();
        self.helpers = Helpers::default();
        self.eval_calls = 0;
        self.eval_retained_source_bytes = 0;
        *self.bridges.borrow_mut() = Bridges::default();
        self.disposed = true;
    }
}

/// Service one synchronous `__zippHostCall`. Anything structured crosses as
/// JSON; an `Err` becomes a JS throw the script can catch.
fn is_allowed_sync_host_call(kind: &str) -> bool {
    sync_host_call_arity(kind).is_some()
}

/// Exact wire arity for every synchronous operation. Guest code can call
/// `__zippHostCall` directly, so wrapper arity is not a sufficient boundary.
fn sync_host_call_arity(kind: &str) -> Option<usize> {
    Some(match kind {
        "db.query" | "db.get" | "db.create" | "db.update" | "db.hardDelete" | "ls.setItem" => 2,
        "db.delete" | "db.startSync" | "db.stopSync" | "db.getSyncStatus" | "ls.getItem"
        | "ls.removeItem" | "nav.clipboardWrite" => 1,
        "db.getSavedSyncRoom" | "ls.clear" | "nav.clipboardRead" => 0,
        "accel.compile" | "accel.make" | "accel.run" | "accel.install" => 2,
        "accel.state" => 1,
        _ => return None,
    })
}

thread_local! {
    /// What this WASM instance has accumulated across the engines it has
    /// disposed: the definitions `dispose()` cannot give back, and the
    /// compilation work that produced them. Live engines report their own
    /// figures through `resourceUsage()`; these are the disposed ones', so a
    /// host can decide when to recycle the instance (ZIPP-06, stage 1).
    static INSTANCE_USAGE: std::cell::Cell<InstanceUsage> =
        const { std::cell::Cell::new(InstanceUsage::ZERO) };
    /// The context of the `accel.run` call in progress, for
    /// [`accel_guest_call`]. Set for the duration of the bridge call and
    /// cleared after it; a callback outside that window is refused.
    static ACCEL_CTX: std::cell::Cell<Option<*mut (dyn HostCtx + 'static)>> =
        const { std::cell::Cell::new(None) };
}

#[derive(Clone, Copy)]
struct InstanceUsage {
    engines_created: u64,
    engines_disposed: u64,
    retained_functions: u64,
    retained_classes: u64,
    dynamic_code_calls: u64,
    dynamic_code_source_bytes: u64,
}

impl InstanceUsage {
    const ZERO: InstanceUsage = InstanceUsage {
        engines_created: 0,
        engines_disposed: 0,
        retained_functions: 0,
        retained_classes: 0,
        dynamic_code_calls: 0,
        dynamic_code_source_bytes: 0,
    };
}

/// What this WASM instance has accumulated over every engine it has disposed
/// so far, as JSON-shaped data: `enginesCreated`, `enginesDisposed`, and the
/// disposed engines' summed `retainedFunctions`, `retainedClasses`,
/// `dynamicCodeCalls` and `dynamicCodeSourceBytes`. An engine's compiled
/// preamble-plus-guest program is freed with the engine since B306 stage two
/// (the state owns it; nothing is leaked); the stable-address definitions
/// that successful dynamic compilations install are what still survive
/// `dispose()`. When their total passes
/// what a host accepts, the host recycles the Worker/WASM instance, which is
/// the only reclamation this artifact offers (the 11 September 2026 audit's
/// ZIPP-06, stage 1: measure before reclaiming). Live engines are not
/// included; read each one's `resourceUsage()`.
#[wasm_bindgen(js_name = zippInstanceUsage)]
pub fn zipp_instance_usage() -> Result<JsValue, JsValue> {
    let u = INSTANCE_USAGE.with(|c| c.get());
    let n = |v: u64| HostValue::Number(v as f64);
    to_js(&HostValue::Object(vec![
        ("enginesCreated".into(), n(u.engines_created)),
        ("enginesDisposed".into(), n(u.engines_disposed)),
        ("retainedFunctions".into(), n(u.retained_functions)),
        ("retainedClasses".into(), n(u.retained_classes)),
        ("dynamicCodeCalls".into(), n(u.dynamic_code_calls)),
        (
            "dynamicCodeSourceBytes".into(),
            n(u.dynamic_code_source_bytes),
        ),
    ]))
    .map_err(to_js_error)
}

/// Run a guest function by global name with numbers, from inside a host
/// `accel.run` bridge call and only from there: the engine is re-entered
/// through the context of the call in progress. The guest cannot make a
/// nested host call while it runs.
#[wasm_bindgen(js_name = accelGuestCall)]
pub fn accel_guest_call(name: &str, args: &[f64]) -> Result<f64, JsValue> {
    let raw = ACCEL_CTX
        .with(|c| c.get())
        .ok_or_else(|| JsValue::from_str("Error: accelGuestCall outside accel.run"))?;
    // SAFETY: the pointer was taken from the `&mut dyn HostCtx` that
    // `host_dispatch_ctx` holds across the bridge call, is only dereferenced
    // while that call is in progress (the cell is cleared before it
    // returns), and the holder does not touch its own reference meanwhile.
    let ctx = unsafe { &mut *raw };
    ctx.call_global_numbers(name, args)
        .map_err(|e| JsValue::from_str(&e))
}

/// The spec `accel.make` forwards, held to the PUBLIC binding grammar the
/// preamble documents — `NAME=g:GLOBAL`, `NAME=c:GLOBAL`, `NAME=a:ID`,
/// `NAME=n:NUMBER`, `NAME=t` — with every `g:` entry resolved to
/// `NAME=r:address:length:kind` through the VM. A global that is not a typed
/// array is an error the guest sees.
///
/// `r:` is the engine's own transport form and never one the guest may
/// write. An adapter cannot tell an engine-resolved region from guest text
/// that spells one, so any entry outside the public grammar — an `r:` region
/// above all — is refused here, before the adapter sees the spec, rather than
/// forwarded for the adapter to trust (the 6 September 2026 audit's Z02).
/// Names must be identifiers and unique; a `c:` target is an identifier; an
/// `a:` id is a non-negative safe integer; an `n:` operand is a finite number.
///
/// Two phases, and the order is the point: the WHOLE spec is parsed into a
/// validated form — size, entry count and name lengths bounded, duplicates
/// found through a set — before a single region is resolved, and the regions
/// are then resolved as one transaction. A spec whose later entry is invalid
/// therefore pins nothing and never reaches the adapter; it used to pin the
/// earlier entries' buffers first and check duplicates by scanning a growing
/// list (the 11 September 2026 audit's ZIPP-08).
fn resolve_accel_spec(ctx: &mut dyn HostCtx, spec: &str) -> Result<String, String> {
    fn is_identifier(s: &str) -> bool {
        let mut chars = s.chars();
        s.len() <= MAX_ACCEL_SPEC_NAME_BYTES
            && matches!(chars.next(), Some(c) if c == '_' || c == '$' || c.is_ascii_alphabetic())
            && chars.all(|c| c == '_' || c == '$' || c.is_ascii_alphanumeric())
    }
    fn refuse(entry: &str, why: &str) -> String {
        format!("TypeError: accel.make: {entry:?} {why}")
    }
    enum Binding<'a> {
        /// `g:GLOBAL` — resolved to a region in phase two.
        Region(&'a str),
        /// Forwarded exactly as written.
        Verbatim,
    }
    if spec.is_empty() {
        return Ok(String::new());
    }
    if spec.len() > MAX_ACCEL_SPEC_BYTES {
        return Err(format!(
            "RangeError: accel.make: spec exceeds the {MAX_ACCEL_SPEC_BYTES}-byte limit"
        ));
    }
    // Phase one: parse and validate everything; touch nothing.
    let mut entries: Vec<(&str, Binding)> = Vec::new();
    let mut names: HashSet<&str> = HashSet::new();
    for entry in spec.split(',') {
        if entries.len() >= MAX_ACCEL_SPEC_ENTRIES {
            return Err(format!(
                "RangeError: accel.make: spec exceeds the {MAX_ACCEL_SPEC_ENTRIES}-entry limit"
            ));
        }
        let Some((name, binding)) = entry.split_once('=') else {
            return Err(refuse(entry, "is not NAME=BINDING"));
        };
        if !is_identifier(name) {
            return Err(refuse(entry, "does not name a binding"));
        }
        if !names.insert(name) {
            return Err(refuse(entry, "binds a name twice"));
        }
        let (tag, value) = match binding.split_once(':') {
            Some((tag, value)) => (tag, Some(value)),
            None => (binding, None),
        };
        let binding = match (tag, value) {
            ("g", Some(global)) if is_identifier(global) => Binding::Region(global),
            ("c", Some(global)) if is_identifier(global) => Binding::Verbatim,
            ("a", Some(id)) if is_accel_id_text(id) => Binding::Verbatim,
            ("n", Some(number)) if number.parse::<f64>().is_ok_and(f64::is_finite) => {
                Binding::Verbatim
            }
            ("t", None) => Binding::Verbatim,
            _ => {
                return Err(refuse(
                    entry,
                    "is not a public binding (NAME=g:GLOBAL, NAME=c:GLOBAL, NAME=a:ID, NAME=n:NUMBER or NAME=t)",
                ))
            }
        };
        entries.push((entry, binding));
    }
    // Phase two: every region at once — all pinned, or none.
    let globals: Vec<&str> = entries
        .iter()
        .filter_map(|(_, binding)| match binding {
            Binding::Region(global) => Some(*global),
            Binding::Verbatim => None,
        })
        .collect();
    let mut regions = ctx.typed_array_regions(&globals)?.into_iter();
    let mut out = String::with_capacity(spec.len() + 64);
    for (i, (entry, binding)) in entries.into_iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        match binding {
            Binding::Region(_) => {
                let (ptr, len, kind) = regions
                    .next()
                    .ok_or_else(|| "Error: accel.make: region count mismatch".to_owned())?;
                let name = entry.split_once('=').map(|(n, _)| n).unwrap_or(entry);
                out.push_str(name);
                out.push_str("=r:");
                out.push_str(&ptr.to_string());
                out.push(':');
                out.push_str(&len.to_string());
                out.push(':');
                out.push_str(&kind.to_string());
            }
            Binding::Verbatim => out.push_str(entry),
        }
    }
    Ok(out)
}

/// An `a:ID` operand: decimal digits naming a non-negative safe integer.
fn is_accel_id_text(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 16
        && id.bytes().all(|b| b.is_ascii_digit())
        && id.parse::<f64>().is_ok_and(|n| n <= MAX_ACCEL_ID)
}

/// A function or trace-slot identifier as the guest passes it to the
/// synchronous accelerator bridge: a finite, integral, non-negative number in
/// the safe range. A successful generic `f64` parse is not sufficient for an
/// identifier.
fn accel_identifier(kind: &str, text: &str) -> Result<JsValue, String> {
    parse_accel_identifier(kind, text).map(JsValue::from_f64)
}

fn parse_accel_identifier(kind: &str, text: &str) -> Result<f64, String> {
    text.trim()
        .parse::<f64>()
        .ok()
        .filter(|n| n.is_finite() && *n >= 0.0 && n.fract() == 0.0 && *n <= MAX_ACCEL_ID)
        .ok_or_else(|| {
            format!(
                "TypeError: host bridge call '{kind}' expects a non-negative integer identifier"
            )
        })
}

/// Linked WebAssembly linear-memory maximum, in bytes. Set by the linker from
/// `.cargo/config.toml` (and repeated in the release workflow's RUSTFLAGS);
/// stated here so the profile can report it, and checked against the built
/// artifact by `tests/node/check-wasm-memory.cjs`.
const LINKED_MEMORY_MAX_BYTES: u64 = 1024 * 1024 * 1024;

/// The limits and semantics this artifact was built with, as JSON.
///
/// A host used to have only the README's table to go by, and at v0.0.14 four
/// of its rows described an older build (the 6 September 2026 audit's Z04).
/// This is read from the same constants the engine enforces, so it cannot
/// drift; `tests/node/profile-matches-readme.cjs` holds the README to it.
///
/// `profileVersion` 2 adds provenance and policy (the 11 September 2026
/// audit's ZIPP-18): the source revision the release pipeline built from
/// (`source.sha`, `null` in an unlabelled local build), the grammar goal and
/// strict-mode policy guests are compiled under, the string-transport
/// contract, the batch-write arity, the host-call id width, and the
/// host-boundary work limits — value nodes and bytes, the asynchronous
/// queue's drain and per-request ceilings, the fingerprint budget and the
/// accelerator spec bounds. Fields are only ever added; a host should read
/// the ones it knows.
#[wasm_bindgen(js_name = zippProfile)]
pub fn zipp_profile() -> String {
    let source_sha = match option_env!("ZIPP_SOURCE_SHA") {
        Some(sha) if !sha.is_empty() && sha.bytes().all(|b| b.is_ascii_hexdigit()) => {
            format!("\"{sha}\"")
        }
        _ => "null".to_owned(),
    };
    format!(
        concat!(
            "{{",
            "\"engine\":\"zipp-wasm\",",
            "\"version\":\"{version}\",",
            "\"profileVersion\":2,",
            "\"source\":{{\"sha\":{source_sha},\"target\":\"wasm32-unknown-unknown\"}},",
            "\"features\":[\"safe-sandbox\",\"meter-only\",\"wasm-no-fs-loader\",\"wasm-single-agent\"],",
            "\"semantics\":{{",
            "\"callOrder\":\"strict\",",
            "\"parseGoal\":\"script-compat\",",
            "\"topLevelReturn\":true,",
            "\"guestStrictMode\":\"directive-prologue\",",
            "\"stringTransport\":\"utf16\",",
            "\"batchWriteArity\":\"strict\",",
            "\"hostCallIdBits\":53,",
            "\"consoleOutput\":\"chronological\",",
            "\"hostCallDrain\":\"transactional\"",
            "}},",
            "\"limits\":{{",
            "\"initialSourceBytes\":{initial_source},",
            "\"evalExpressionBytes\":{eval_expression},",
            "\"evalRetainedSourceBytes\":{eval_retained},",
            "\"evalCalls\":{eval_calls},",
            "\"dynamicCodeSourceBytes\":{dynamic_source},",
            "\"dynamicCodeRetainedSourceBytes\":{dynamic_retained},",
            "\"dynamicCodeCalls\":{dynamic_calls},",
            "\"dynamicCodeFunctions\":{dynamic_functions},",
            "\"dynamicCodeClasses\":{dynamic_classes},",
            "\"lifetimeSteps\":{lifetime_steps},",
            "\"maxInstructionBudgetSteps\":{max_budget},",
            "\"approxHeapBytes\":{heap},",
            "\"linkedMemoryMaxBytes\":{linked_memory},",
            "\"lifetimeOutputBytes\":{output},",
            "\"syncBridgeKindBytes\":{bridge_kind},",
            "\"syncBridgeArgs\":{bridge_args},",
            "\"syncBridgeBytes\":{bridge_bytes},",
            "\"syncCapabilityEntries\":{capability_entries},",
            "\"hostValueNodes\":{host_value_nodes},",
            "\"hostValueStringBytes\":{host_value_string_bytes},",
            "\"fingerprintNodes\":{host_value_nodes},",
            "\"fingerprintStringBytes\":{host_value_string_bytes},",
            "\"hostCallQueue\":{host_call_queue},",
            "\"hostCallPending\":{host_call_pending},",
            "\"hostCallRequestUnits\":{host_call_request_units},",
            "\"hostCallDrainRequests\":{host_call_drain_requests},",
            "\"hostCallDrainStringBytes\":{host_call_drain_string_bytes},",
            "\"accelSpecBytes\":{accel_spec_bytes},",
            "\"accelSpecEntries\":{accel_spec_entries},",
            "\"accelSpecNameBytes\":{accel_spec_name_bytes}",
            "}}}}"
        ),
        version = env!("CARGO_PKG_VERSION"),
        source_sha = source_sha,
        initial_source = MAX_INITIAL_SOURCE_BYTES,
        eval_expression = MAX_EVAL_SOURCE_BYTES,
        eval_retained = MAX_EVAL_RETAINED_SOURCE_BYTES,
        eval_calls = MAX_EVAL_CALLS,
        dynamic_source = MAX_DYNAMIC_CODE_SOURCE_BYTES,
        dynamic_retained = MAX_DYNAMIC_CODE_RETAINED_SOURCE_BYTES,
        dynamic_calls = MAX_DYNAMIC_CODE_CALLS,
        dynamic_functions = MAX_DYNAMIC_CODE_FUNCTIONS,
        dynamic_classes = MAX_DYNAMIC_CODE_CLASSES,
        lifetime_steps = MAX_LIFETIME_STEPS,
        max_budget = MAX_INSTRUCTION_BUDGET_STEPS,
        heap = MAX_APPROX_HEAP_BYTES,
        linked_memory = LINKED_MEMORY_MAX_BYTES,
        output = MAX_LIFETIME_OUTPUT_BYTES,
        bridge_kind = MAX_SYNC_BRIDGE_KIND_BYTES,
        bridge_args = MAX_SYNC_BRIDGE_ARGS,
        bridge_bytes = MAX_SYNC_BRIDGE_BYTES,
        capability_entries = MAX_SYNC_CAPABILITY_ENTRIES,
        host_value_nodes = DEFAULT_HOST_VALUE_MAX_NODES,
        host_value_string_bytes = DEFAULT_HOST_VALUE_MAX_STRING_BYTES,
        host_call_queue = PREAMBLE_HOST_CALL_QUEUE_MAX,
        host_call_pending = PREAMBLE_HOST_CALL_PENDING_MAX,
        host_call_request_units = PREAMBLE_HOST_CALL_REQUEST_MAX_UNITS,
        host_call_drain_requests = MAX_HOST_CALL_DRAIN_REQUESTS,
        host_call_drain_string_bytes = MAX_HOST_CALL_DRAIN_STRING_BYTES,
        accel_spec_bytes = MAX_ACCEL_SPEC_BYTES,
        accel_spec_entries = MAX_ACCEL_SPEC_ENTRIES,
        accel_spec_name_bytes = MAX_ACCEL_SPEC_NAME_BYTES,
    )
}

/// The synchronous dispatch with the VM at hand: `accel.*` is served here,
/// everything else exactly as [`host_dispatch`].
fn host_dispatch_ctx(
    bridges: &Rc<RefCell<Bridges>>,
    ctx: &mut dyn HostCtx,
    kind: &str,
    args: &[String],
) -> Result<String, String> {
    let Some(method) = kind.strip_prefix("accel.") else {
        return host_dispatch(bridges, kind, args);
    };
    let Some(expected_arity) = sync_host_call_arity(kind) else {
        return Err(format!("TypeError: unknown host call '{kind}'"));
    };
    if args.len() != expected_arity {
        return Err(format!(
            "TypeError: host bridge call '{kind}' requires exactly {expected_arity} arguments"
        ));
    }
    let input_bytes = args.iter().try_fold(kind.len(), |total, arg| {
        total
            .checked_add(arg.len())
            .filter(|n| *n <= MAX_SYNC_BRIDGE_BYTES)
    });
    if input_bytes.is_none() {
        return Err(format!(
            "RangeError: host bridge arguments exceed the {MAX_SYNC_BRIDGE_BYTES}-byte limit"
        ));
    }
    let target = {
        let bridges = bridges.borrow();
        if !bridges.allowed_sync_operations.contains(kind) {
            return Err("SecurityError: synchronous host capability denied".into());
        }
        bridges.accel.clone()
    };
    let Some(target) = target else {
        return Err("Error: authorized host bridge is unavailable".into());
    };
    let f = js_sys::Reflect::get(&target, &JsValue::from_str(method))
        .ok()
        .filter(JsValue::is_function)
        .map(JsValue::unchecked_into::<js_sys::Function>)
        .ok_or_else(|| "Error: authorized host bridge is unavailable".to_owned())?;
    let number = |s: &str| -> Result<JsValue, String> {
        s.trim()
            .parse::<f64>()
            .map(JsValue::from_f64)
            .map_err(|_| format!("TypeError: host bridge call '{kind}' expects a number"))
    };
    let call = match method {
        "compile" => f.call2(
            &target,
            &JsValue::from_str(&args[0]),
            &JsValue::from_str(&args[1]),
        ),
        "make" => {
            // Validate the identifier BEFORE the spec resolves and pins.
            let id = accel_identifier(kind, &args[0])?;
            let spec = resolve_accel_spec(ctx, &args[1])?;
            f.call2(&target, &id, &JsValue::from_str(&spec))
        }
        "state" => {
            let (ptr, len, kind) = ctx.typed_array_region(&args[0])?;
            f.call3(
                &target,
                &JsValue::from_f64(ptr as f64),
                &JsValue::from_f64(len as f64),
                &JsValue::from_f64(kind as f64),
            )
        }
        "install" => f.call2(
            &target,
            &accel_identifier(kind, &args[0])?,
            &accel_identifier(kind, &args[1])?,
        ),
        "run" => {
            let id = accel_identifier(kind, &args[0])?;
            let hops = number(&args[1])?;
            let raw: *mut (dyn HostCtx + '_) = ctx;
            // SAFETY: the lifetime is erased only for storage; the pointer is
            // cleared before this function returns, and `ctx` is not used
            // again until then.
            let raw: *mut (dyn HostCtx + 'static) = unsafe { std::mem::transmute(raw) };
            let previous = ACCEL_CTX.with(|c| c.replace(Some(raw)));
            let r = f.call2(&target, &id, &hops);
            ACCEL_CTX.with(|c| c.set(previous));
            r
        }
        _ => return Err(format!("TypeError: unknown host call '{kind}'")),
    };
    let ret = call.map_err(|_| "Error: host bridge call failed".to_owned())?;
    let reply = match js_sys::JSON::stringify(&ret) {
        Ok(serialized) => serialized.as_string().unwrap_or_else(|| "null".to_owned()),
        Err(_) => return Err("Error: host bridge reply is not serializable".into()),
    };
    Ok(reply)
}

fn host_dispatch(
    bridges: &Rc<RefCell<Bridges>>,
    kind: &str,
    args: &[String],
) -> Result<String, String> {
    if kind.len() > MAX_SYNC_BRIDGE_KIND_BYTES {
        return Err(format!(
            "RangeError: host bridge kind exceeds the {MAX_SYNC_BRIDGE_KIND_BYTES}-byte limit"
        ));
    }
    // `kind` is controlled by the guest: it can call `__zippHostCall`
    // directly instead of going through the preamble wrappers. Reject before
    // even selecting a bridge or looking up a property, otherwise a planted
    // getter/method outside the advertised API becomes ambient authority.
    let Some(expected_arity) = sync_host_call_arity(kind) else {
        return Err(format!("TypeError: unknown host call '{kind}'"));
    };
    // A host bridge invocation is one VM instruction even when the strings it
    // passes cause megabytes of JSON parsing or allocation on the host side.
    // Bound the complete argument envelope before selecting/calling a bridge.
    if args.len() > MAX_SYNC_BRIDGE_ARGS {
        return Err(format!(
            "RangeError: host bridge call exceeds the {MAX_SYNC_BRIDGE_ARGS}-argument limit"
        ));
    }
    let input_bytes = args.iter().try_fold(kind.len(), |total, arg| {
        total
            .checked_add(arg.len())
            .filter(|n| *n <= MAX_SYNC_BRIDGE_BYTES)
    });
    if input_bytes.is_none() {
        return Err(format!(
            "RangeError: host bridge arguments exceed the {MAX_SYNC_BRIDGE_BYTES}-byte limit"
        ));
    }
    if args.len() != expected_arity {
        return Err(format!(
            "TypeError: host bridge call '{kind}' requires exactly {expected_arity} arguments"
        ));
    }

    // Clone the handle out before calling: the bridge method runs arbitrary JS,
    // and holding the RefCell borrow across it would panic if it re-entered.
    let (target, method) = {
        let bridges = bridges.borrow();
        if !bridges.allowed_sync_operations.contains(kind) {
            return Err("SecurityError: synchronous host capability denied".into());
        }
        match kind.split_once('.') {
            Some(("db", m)) => (bridges.db.clone(), m),
            Some(("ls", m)) => (bridges.local_storage.clone(), m),
            // Expose the standard Clipboard method names on the dedicated
            // object rather than requiring a second bespoke nav-shaped API.
            Some(("nav", "clipboardWrite")) => (bridges.clipboard.clone(), "writeText"),
            Some(("nav", "clipboardRead")) => (bridges.clipboard.clone(), "readText"),
            _ => return Err(format!("TypeError: unknown host call '{kind}'")),
        }
    };
    let Some(target) = target else {
        return Err("Error: authorized host bridge is unavailable".into());
    };

    // Validate structured guest input before even resolving a host property.
    // Reflect::get may invoke a host getter, so malformed JSON must not reach
    // that point. Keep the parsed value for the actual call.
    let structured_argument = match kind {
        "db.query" | "db.create" | "db.update" => Some(parse_bridge_json(&args[1])?),
        _ => None,
    };

    let f = js_sys::Reflect::get(&target, &JsValue::from_str(method))
        .ok()
        .filter(JsValue::is_function)
        .map(JsValue::unchecked_into::<js_sys::Function>)
        .ok_or_else(|| "Error: authorized host bridge is unavailable".to_owned())?;

    // Per-kind argument shapes: which arguments are JSON and which are plain.
    let call = match kind {
        "db.query" | "db.create" | "db.update" => f.call2(
            &target,
            &JsValue::from_str(&args[0]),
            structured_argument
                .as_ref()
                .expect("structured bridge argument was validated"),
        ),
        "db.get" | "db.hardDelete" => f.call2(
            &target,
            &JsValue::from_str(&args[0]),
            &JsValue::from_str(&args[1]),
        ),
        "ls.setItem" => f.call2(
            &target,
            &JsValue::from_str(&args[0]),
            &JsValue::from_str(&args[1]),
        ),
        "db.getSavedSyncRoom" | "ls.clear" | "nav.clipboardRead" => f.call0(&target),
        _ => f.call1(&target, &JsValue::from_str(&args[0])),
    };

    // Host exception text may contain credentials, internal paths, tenant IDs,
    // or backend details. It remains available to the trusted host at the call
    // site, but the guest receives only a stable opaque failure.
    let ret = call.map_err(|_| "Error: host bridge call failed".to_owned())?;

    // Every reply crosses as JSON, including `undefined` — which `stringify`
    // answers with the JS value `undefined`, NOT a string. Converting that to a
    // Rust `String` unconditionally panics, and a void bridge method
    // (`setItem`, `delete`, `startSync`) hits it on every call.
    let reply = match js_sys::JSON::stringify(&ret) {
        Ok(serialized) => {
            // js-sys types JSON.stringify as JsString even though JavaScript
            // returns `undefined` for a void result. Inspect the underlying
            // value first, then reject a definitely-oversized UTF-16 string
            // before copying it into Rust/WASM memory. The byte check below is
            // still authoritative for non-ASCII text.
            let raw: JsValue = serialized.into();
            if !raw.is_string() {
                "null".into()
            } else {
                let text: &js_sys::JsString = raw.unchecked_ref();
                if text.length() as usize > MAX_SYNC_BRIDGE_BYTES {
                    return Err(format!(
                        "RangeError: host bridge reply exceeds the {MAX_SYNC_BRIDGE_BYTES}-byte limit"
                    ));
                }
                raw.as_string().unwrap_or_else(|| "null".into())
            }
        }
        Err(_) => return Err("Error: host bridge returned an unserializable value".into()),
    };
    if reply.len() > MAX_SYNC_BRIDGE_BYTES {
        return Err(format!(
            "RangeError: host bridge reply exceeds the {MAX_SYNC_BRIDGE_BYTES}-byte limit"
        ));
    }
    Ok(reply)
}

/// A `host.call` request id as the host hands it back: the exact Number the
/// guest's counter produced, or a TypeError.
fn host_call_id(call_id: f64) -> Result<f64, JsValue> {
    if !call_id.is_finite() || call_id < 0.0 || call_id > MAX_HOST_CALL_ID || call_id.fract() != 0.0
    {
        return Err(JsValue::from_str(
            "TypeError: host call id must be a non-negative safe integer",
        ));
    }
    Ok(call_id)
}

fn require_bridge(bridge: JsValue, label: &str) -> Result<js_sys::Object, JsValue> {
    // `dyn_into::<Object>` uses JavaScript `instanceof Object`. A Proxy's
    // getPrototypeOf trap can throw from that non-catch binding and poison the
    // exported Engine borrow before initialization even starts. `typeof`-style
    // wasm-bindgen predicates cannot invoke guest/host JavaScript.
    if bridge.is_null() || (!bridge.is_object() && !bridge.is_function()) {
        return Err(JsValue::from_str(&format!(
            "TypeError: {label} bridge must be a non-null object"
        )));
    }
    Ok(bridge.unchecked_into())
}

fn parse_bridge_json(text: &str) -> Result<JsValue, String> {
    js_sys::JSON::parse(text)
        .map_err(|_| "TypeError: malformed JSON in host bridge argument".to_owned())
}

fn to_js_error(error: String) -> JsValue {
    JsValue::from_str(&error)
}

fn inspection_error(label: &str) -> String {
    format!("TypeError: {label} could not be inspected safely")
}

fn checked_is_array(v: &JsValue, label: &str) -> Result<bool, String> {
    try_array_is_array(v).map_err(|_| inspection_error(label))
}

fn require_array(v: &JsValue, label: &str) -> Result<(), String> {
    if !checked_is_array(v, label)? {
        return Err(format!("TypeError: {label} must be an array"));
    }
    Ok(())
}

fn checked_array_length(v: &JsValue, label: &str) -> Result<u32, String> {
    let raw = js_sys::Reflect::get(v, &JsValue::from_str("length"))
        .map_err(|_| inspection_error(label))?;
    let Some(length) = raw.as_f64() else {
        return Err(inspection_error(label));
    };
    if !length.is_finite() || length < 0.0 || length > u32::MAX as f64 || length.fract() != 0.0 {
        return Err(inspection_error(label));
    }
    Ok(length as u32)
}

fn checked_array_get(v: &JsValue, index: u32, label: &str) -> Result<JsValue, String> {
    js_sys::Reflect::get_u32(v, index).map_err(|_| inspection_error(label))
}

/// Coerce a JS array of numbers to slot indices.
fn index_list(v: &JsValue, budget: &mut HostValueBudget) -> Result<Vec<u32>, String> {
    if v.is_undefined() || v.is_null() {
        return Ok(Vec::new());
    }
    require_array(v, "indices")?;
    let len = checked_array_length(v, "indices")?;
    budget.charge_node()?;
    budget.ensure_nodes(len as usize)?;
    let mut out = Vec::with_capacity(len as usize);
    for i in 0..len {
        budget.charge_node()?;
        let raw = checked_array_get(v, i, "indices")?;
        let Some(index) = raw.as_f64() else {
            return Err(
                "TypeError: indices must contain only finite unsigned 32-bit integers".into(),
            );
        };
        if !index.is_finite() || index < 0.0 || index > u32::MAX as f64 || index.fract() != 0.0 {
            return Err(
                "TypeError: indices must contain only finite unsigned 32-bit integers".into(),
            );
        }
        out.push(index as u32);
    }
    Ok(out)
}

/// How deep [`from_js`] will follow a host object graph. Matches the engine's
/// own walk limit; a host object deeper than this is not script state.
const MAX_DEPTH: usize = 64;

fn to_js(v: &HostValue) -> Result<JsValue, String> {
    let mut budget = HostValueBudget::default();
    to_js_bounded(v, &mut budget)
}

fn to_js_bounded(v: &HostValue, budget: &mut HostValueBudget) -> Result<JsValue, String> {
    budget.charge_node()?;
    match v {
        HostValue::Undefined => Ok(JsValue::UNDEFINED),
        // A function/class/Map/Date reads as null, which the host treats as
        // "not syncable" — matching what it does for its own non-serializables.
        HostValue::Null | HostValue::Opaque => Ok(JsValue::NULL),
        HostValue::Bool(b) => Ok(JsValue::from_bool(*b)),
        HostValue::Number(n) => Ok(JsValue::from_f64(*n)),
        HostValue::String(s) => {
            budget.charge_string(s)?;
            Ok(JsValue::from_str(s))
        }
        // A guest string holding a lone surrogate: rebuilt from its exact
        // code units, so the host sees the string the guest has (the
        // 11 September 2026 audit's ZIPP-11).
        HostValue::Utf16(units) => {
            budget.charge_string_bytes(units.len().saturating_mul(3))?;
            Ok(js_sys::JsString::from_char_code(units).into())
        }
        HostValue::Array(items) => {
            budget.ensure_nodes(items.len())?;
            let a = js_sys::Array::new_with_length(items.len() as u32);
            for (i, it) in items.iter().enumerate() {
                a.set(i as u32, to_js_bounded(it, budget)?);
            }
            Ok(a.into())
        }
        HostValue::Object(pairs) => {
            budget.ensure_nodes(pairs.len())?;
            let o = js_sys::Object::new();
            for (k, val) in pairs {
                budget.charge_string(k)?;
                let value = to_js_bounded(val, budget)?;
                define_own(&o, k, &value)?;
            }
            Ok(o.into())
        }
    }
}

/// Define an ordinary enumerable data property without invoking the legacy
/// `__proto__` setter inherited from `Object.prototype`.
fn define_own(target: &js_sys::Object, key: &str, value: &JsValue) -> Result<(), String> {
    let descriptor = js_sys::Object::new();
    for (name, value) in [
        ("value", value.clone()),
        ("writable", JsValue::TRUE),
        ("enumerable", JsValue::TRUE),
        ("configurable", JsValue::TRUE),
    ] {
        match js_sys::Reflect::set(&descriptor, &JsValue::from_str(name), &value) {
            Ok(true) => {}
            _ => return Err("zipp: failed to construct a host object".into()),
        }
    }
    match js_sys::Reflect::define_property(target, &JsValue::from_str(key), &descriptor) {
        Ok(true) => Ok(()),
        _ => Err("zipp: failed to construct a host object".into()),
    }
}

fn from_js(v: &JsValue) -> Result<HostValue, String> {
    let mut budget = HostValueBudget::default();
    let seen = js_sys::WeakSet::<js_sys::Object>::new_typed();
    from_js_bounded(v, 0, &seen, &mut budget)
}

fn from_js_bounded(
    v: &JsValue,
    depth: usize,
    seen: &js_sys::WeakSet<js_sys::Object>,
    budget: &mut HostValueBudget,
) -> Result<HostValue, String> {
    budget.charge_node()?;
    if v.is_undefined() {
        return Ok(HostValue::Undefined);
    }
    if v.is_null() {
        return Ok(HostValue::Null);
    }
    if let Some(b) = v.as_bool() {
        return Ok(HostValue::Bool(b));
    }
    if let Some(n) = v.as_f64() {
        return Ok(HostValue::Number(n));
    }
    if v.is_string() {
        let value: &js_sys::JsString = v.unchecked_ref();
        budget.ensure_string_units(value.length() as usize)?;
        let s = v.as_string().unwrap_or_default();
        // wasm-bindgen's text decoding replaces a lone surrogate with U+FFFD.
        // A replacement character in the result is the only sign, so only
        // then re-read the exact code units (a string that genuinely holds
        // U+FFFD costs one extra pass and comes back as itself).
        if s.contains('\u{FFFD}') {
            let units: Vec<u16> = value.iter().collect();
            if String::from_utf16(&units).is_err() {
                budget.charge_string_bytes(units.len().saturating_mul(3))?;
                return Ok(HostValue::Utf16(units));
            }
        }
        budget.charge_string(&s)?;
        return Ok(HostValue::String(s));
    }
    if depth >= MAX_DEPTH {
        return Ok(HostValue::Null);
    }
    if checked_is_array(v, "host value")? {
        let object: js_sys::Object = v.clone().unchecked_into();
        if seen.has(&object) {
            return Ok(HostValue::Null);
        }
        seen.add(&object);
        let result = (|| {
            let len = checked_array_length(v, "host array")?;
            budget.ensure_nodes(len as usize)?;
            let mut items = Vec::with_capacity(len as usize);
            for i in 0..len {
                let value = checked_array_get(v, i, "host array")?;
                items.push(from_js_bounded(&value, depth + 1, seen, budget)?);
            }
            Ok(HostValue::Array(items))
        })();
        seen.delete(&object);
        return result;
    }
    if v.is_function() {
        return Ok(HostValue::Opaque);
    }
    if v.is_object() {
        let object: js_sys::Object = v.clone().unchecked_into();
        if seen.has(&object) {
            return Ok(HostValue::Null);
        }
        seen.add(&object);
        let result = (|| {
            let keys: JsValue = try_object_keys(v)
                .map_err(|_| inspection_error("host object"))?
                .into();
            require_array(&keys, "host object keys")?;
            let key_count = checked_array_length(&keys, "host object keys")?;
            budget.ensure_nodes(key_count as usize)?;
            let mut pairs = Vec::with_capacity(key_count as usize);
            for i in 0..key_count {
                let k = checked_array_get(&keys, i, "host object keys")?;
                if !k.is_string() {
                    return Err(inspection_error("host object keys"));
                }
                let key: &js_sys::JsString = k.unchecked_ref();
                budget.ensure_string_units(key.length() as usize)?;
                let Some(name) = k.as_string() else { continue };
                budget.charge_string(&name)?;
                let val = js_sys::Reflect::get(&object, &k)
                    .map_err(|_| inspection_error("host object property"))?;
                // A method on a host-supplied object is not state; drop it
                // rather than storing a placeholder the script would call.
                if val.is_function() {
                    continue;
                }
                pairs.push((name, from_js_bounded(&val, depth + 1, seen, budget)?));
            }
            Ok(HostValue::Object(pairs))
        })();
        seen.delete(&object);
        return result;
    }
    Ok(HostValue::Opaque)
}

#[cfg(test)]
mod tests {
    use super::{
        compile_script_with_preamble, host_dispatch, is_allowed_sync_host_call,
        parse_accel_identifier, resolve_accel_spec, sync_host_call_arity, Bridges, HostCtx,
        HostValue, GUEST_COMPILE_OPTIONS, MAX_ACCEL_SPEC_BYTES, MAX_ACCEL_SPEC_ENTRIES,
        MAX_ACCEL_SPEC_NAME_BYTES, MAX_SYNC_BRIDGE_ARGS, MAX_SYNC_BRIDGE_BYTES, PREAMBLE,
        PREAMBLE_BINDINGS, PREAMBLE_HOST_CALL_PENDING_MAX, PREAMBLE_HOST_CALL_QUEUE_MAX,
        PREAMBLE_HOST_CALL_REQUEST_MAX_UNITS,
    };
    use std::cell::RefCell;
    use std::collections::HashSet;
    use std::rc::Rc;
    use zipp_vm::embed::compile_script;

    #[test]
    fn preamble_binding_manifest_matches_the_compiler() {
        let state = compile_script(PREAMBLE).expect("embedded preamble must compile");
        let actual: HashSet<String> = state
            .symbols()
            .into_iter()
            .map(|symbol| symbol.name)
            .collect();
        let expected: HashSet<String> = PREAMBLE_BINDINGS
            .iter()
            .map(|name| (*name).to_owned())
            .collect();
        assert_eq!(
            PREAMBLE_BINDINGS.len(),
            expected.len(),
            "preamble manifest contains a duplicate"
        );
        assert_eq!(
            actual, expected,
            "update PREAMBLE_BINDINGS with preamble.js"
        );
    }

    /// The guest's `"use strict"` must survive being placed after the
    /// preamble, and the preamble must run identically under it.
    #[test]
    fn guest_directive_prologue_survives_the_preamble() {
        let strict_undeclared = compile_script_with_preamble(
            PREAMBLE,
            "\"use strict\"; auditUndeclared = 1;",
            &GUEST_COMPILE_OPTIONS,
        )
        .expect("compiles")
        .run_init()
        .expect_err("strict code may not assign an undeclared name");
        assert!(
            strict_undeclared.contains("ReferenceError"),
            "{strict_undeclared}"
        );

        let mut sloppy =
            compile_script_with_preamble(PREAMBLE, "auditUndeclared = 1;", &GUEST_COMPILE_OPTIONS)
                .expect("compiles");
        sloppy
            .run_init()
            .expect("sloppy code still creates the global");

        let early = match compile_script_with_preamble(
            PREAMBLE,
            "\"use strict\"; function auditDuplicate(a, a) { return a; }",
            &GUEST_COMPILE_OPTIONS,
        ) {
            Err(error) => error,
            Ok(_) => panic!("duplicate parameters are a strict early error"),
        };
        assert!(early.contains("SyntaxError"), "{early}");

        for (source, expected) in [
            (
                "\"use strict\"; function auditThis() { return this === undefined; }",
                true,
            ),
            ("function auditThis() { return this === undefined; }", false),
            // Escaped text is not a Use Strict Directive.
            (
                "\"use\\x20strict\"; function auditThis() { return this === undefined; }",
                false,
            ),
            // A string followed by an operator is an expression, not a directive.
            (
                "\"use strict\" + 1; function auditThis() { return this === undefined; }",
                false,
            ),
            // Comments, a BOM and a hashbang ahead of the prologue are skipped.
            (
                "// leading comment\n/* block */ \"use strict\"; function auditThis() { return this === undefined; }",
                true,
            ),
            (
                "\u{feff}\"use strict\"; function auditThis() { return this === undefined; }",
                true,
            ),
            (
                "#!/usr/bin/env zipp\n\"use strict\"; function auditThis() { return this === undefined; }",
                true,
            ),
        ] {
            let mut st = compile_script_with_preamble(PREAMBLE, source, &GUEST_COMPILE_OPTIONS)
                .expect("compiles");
            st.run_init().expect("initializes");
            let slot = st
                .symbols()
                .into_iter()
                .find(|s| s.name == "auditThis")
                .expect("guest function has a slot")
                .index;
            assert_eq!(
                st.call_slot(slot, &[]),
                Ok(HostValue::Bool(expected)),
                "{source:?}"
            );
            // Preamble helpers keep working under either mode.
            let peek = st
                .symbols()
                .into_iter()
                .find(|s| s.name == "__zPeekHostCalls")
                .expect("preamble helper has a slot")
                .index;
            assert_eq!(
                st.call_slot(peek, &[HostValue::Number(16.0)]),
                Ok(HostValue::Array(Vec::new()))
            );
        }
    }

    #[test]
    fn preamble_queue_bounds_match_the_profile_constants() {
        for (name, value) in [
            ("__zHostQueueMax", PREAMBLE_HOST_CALL_QUEUE_MAX),
            ("__zHostPendingMax", PREAMBLE_HOST_CALL_PENDING_MAX),
            (
                "__zHostRequestMaxUnits",
                PREAMBLE_HOST_CALL_REQUEST_MAX_UNITS,
            ),
        ] {
            let needle = format!("var {name} = {value};");
            assert!(
                PREAMBLE.contains(&needle),
                "preamble.js does not declare `{needle}`"
            );
        }
    }

    /// A mock accelerator context that records every region request. The
    /// engine's own implementation pins as a transaction; what this pins is
    /// what `resolve_accel_spec` asked for, which must be nothing for a spec
    /// with any invalid entry.
    struct MockCtx {
        regions: Vec<String>,
        calls: usize,
        bad: &'static str,
    }
    impl HostCtx for MockCtx {
        fn typed_array_region(&mut self, name: &str) -> Result<(usize, usize, u8), String> {
            self.typed_array_regions(&[name]).map(|r| r[0])
        }
        fn typed_array_regions(
            &mut self,
            names: &[&str],
        ) -> Result<Vec<(usize, usize, u8)>, String> {
            self.calls += 1;
            if names.iter().any(|n| *n == self.bad) {
                return Err(format!("TypeError: {} is not a typed array", self.bad));
            }
            self.regions.extend(names.iter().map(|n| (*n).to_owned()));
            Ok(names
                .iter()
                .enumerate()
                .map(|(i, _)| (4096 + i * 64, 16, 5))
                .collect())
        }
        fn call_global_numbers(&mut self, _name: &str, _args: &[f64]) -> Result<f64, String> {
            unreachable!("not used by spec resolution")
        }
    }
    fn mock() -> MockCtx {
        MockCtx {
            regions: Vec::new(),
            calls: 0,
            bad: "notAnArray",
        }
    }

    #[test]
    fn accel_spec_is_validated_completely_before_any_region_is_resolved() {
        // A valid region ahead of an invalid entry: nothing is resolved.
        for spec in [
            "a=g:buf,b=r:1:2:3",
            "a=g:buf,b=nope",
            "a=g:buf,a=g:buf",
            "a=g:buf,b=a:1.5",
            "a=g:buf,b=a:99999999999999999999",
            "a=g:buf,b=n:NaN",
            "a=g:buf,b=n:Infinity",
            "a=g:buf,1b=t",
            "a=g:buf,b=g:not an identifier",
            "a=g:buf,b",
        ] {
            let mut ctx = mock();
            assert!(
                resolve_accel_spec(&mut ctx, spec).is_err(),
                "{spec} accepted"
            );
            assert_eq!(ctx.calls, 0, "{spec} reached the region resolver");
            assert!(ctx.regions.is_empty(), "{spec} pinned {:?}", ctx.regions);
        }
        // A later region that fails to resolve pins nothing either: the
        // batch is one transaction.
        let mut ctx = mock();
        assert!(resolve_accel_spec(&mut ctx, "a=g:buf,b=g:notAnArray").is_err());
        assert_eq!(ctx.calls, 1);
        assert!(ctx.regions.is_empty());

        // Too many entries, too long a name, too long a spec: refused before
        // parsing gets far, and the duplicate check is a set rather than a
        // scan.
        let many: Vec<String> = (0..=MAX_ACCEL_SPEC_ENTRIES)
            .map(|i| format!("n{i}=t"))
            .collect();
        let mut ctx = mock();
        assert!(resolve_accel_spec(&mut ctx, &many.join(","))
            .unwrap_err()
            .contains("entry limit"));
        let long_name = format!("{}=t", "x".repeat(MAX_ACCEL_SPEC_NAME_BYTES + 1));
        assert!(resolve_accel_spec(&mut ctx, &long_name).is_err());
        let huge = format!("a=n:{}", "1".repeat(MAX_ACCEL_SPEC_BYTES));
        assert!(resolve_accel_spec(&mut ctx, &huge)
            .unwrap_err()
            .contains("byte limit"));
        assert_eq!(ctx.calls, 0);

        // The valid grammar still resolves, every region in one call, in
        // entry order, with the verbatim entries forwarded exactly.
        let mut ctx = mock();
        let out = resolve_accel_spec(&mut ctx, "a=g:buf,f=c:step,k=a:7,x=n:-2.5e3,tr=t,b=g:other")
            .expect("valid spec");
        assert_eq!(
            out,
            "a=r:4096:16:5,f=c:step,k=a:7,x=n:-2.5e3,tr=t,b=r:4160:16:5"
        );
        assert_eq!(ctx.calls, 1);
        assert_eq!(ctx.regions, ["buf", "other"]);
        assert_eq!(resolve_accel_spec(&mut mock(), "").expect("empty"), "");
    }

    #[test]
    fn accel_identifiers_are_integral_and_in_range() {
        for ok in ["0", "7", " 42 ", "9007199254740991"] {
            assert!(parse_accel_identifier("accel.run", ok).is_ok(), "{ok:?}");
        }
        for bad in [
            "1.5",
            "-1",
            "NaN",
            "Infinity",
            "1e400",
            "9007199254740992",
            "9007199254740993",
            "",
            "x",
        ] {
            assert!(parse_accel_identifier("accel.run", bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn synchronous_host_call_allowlist_is_exact() {
        for (kind, arity) in [
            ("db.query", 2),
            ("db.get", 2),
            ("db.create", 2),
            ("db.update", 2),
            ("db.delete", 1),
            ("db.hardDelete", 2),
            ("db.startSync", 1),
            ("db.stopSync", 1),
            ("db.getSyncStatus", 1),
            ("db.getSavedSyncRoom", 0),
            ("ls.getItem", 1),
            ("ls.setItem", 2),
            ("ls.removeItem", 1),
            ("ls.clear", 0),
            ("nav.clipboardWrite", 1),
            ("nav.clipboardRead", 0),
            ("accel.compile", 2),
            ("accel.make", 2),
            ("accel.run", 2),
            ("accel.install", 2),
            ("accel.state", 1),
        ] {
            assert!(
                is_allowed_sync_host_call(kind),
                "documented kind rejected: {kind}"
            );
            assert_eq!(sync_host_call_arity(kind), Some(arity), "wrong arity");
        }

        for kind in [
            "db.secret",
            "db.__proto__",
            "db.query.extra",
            "db.query ",
            "DB.query",
            "ls.key",
            "nav.share",
            "nav.clipboard",
            "",
        ] {
            assert!(
                !is_allowed_sync_host_call(kind),
                "unexpected kind admitted: {kind}"
            );
        }
    }

    #[test]
    fn synchronous_host_call_envelope_is_bounded_before_dispatch() {
        let mut configured = Bridges::default();
        configured.allowed_sync_operations.insert("db.query".into());
        let bridges = Rc::new(RefCell::new(configured));
        let too_many = vec![String::new(); MAX_SYNC_BRIDGE_ARGS + 1];
        let err = host_dispatch(&bridges, "db.query", &too_many).unwrap_err();
        assert!(err.contains("argument limit"), "got {err:?}");

        // Each argument is individually below the envelope ceiling; only their
        // aggregate (including the kind) is too large.
        let part = "x".repeat(MAX_SYNC_BRIDGE_BYTES / 2);
        let too_large = vec![part.clone(), part];
        let err = host_dispatch(&bridges, "db.query", &too_large).unwrap_err();
        assert!(err.contains("arguments exceed"), "got {err:?}");

        let wrong_arity = vec!["collection".into()];
        let err = host_dispatch(&bridges, "db.query", &wrong_arity).unwrap_err();
        assert!(err.contains("requires exactly 2 arguments"), "got {err:?}");
    }

    #[test]
    fn synchronous_host_calls_are_denied_without_an_engine_grant() {
        let bridges = Rc::new(RefCell::new(Bridges::default()));
        let args = vec!["collection".into(), "null".into()];
        let err = host_dispatch(&bridges, "db.query", &args).unwrap_err();
        assert_eq!(err, "SecurityError: synchronous host capability denied");
    }
}
