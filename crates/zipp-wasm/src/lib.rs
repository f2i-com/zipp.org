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
use zipp_vm::embed::{compile_script, HostValue, HostValueBudget, ScriptState, SymbolScope};

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

// These are lifetime limits for one Engine. They are deliberately fixed at
// the embedding boundary: a guest must not be able to raise its own ceiling,
// and every browser host gets the same fail-closed defaults.
const MAX_INITIAL_SOURCE_BYTES: usize = 2 * 1024 * 1024;
const MAX_DYNAMIC_CODE_SOURCE_BYTES: usize = 64 * 1024;
// `evalInContext` adds a fixed host-controlled wrapper before it enters the
// same VM-wide dynamic compiler gate as guest `eval`/`Function`/ShadowRealm.
const MAX_EVAL_SOURCE_BYTES: usize =
    MAX_DYNAMIC_CODE_SOURCE_BYTES - EVAL_PREFIX.len() - EVAL_SUFFIX.len();
const MAX_EVAL_RETAINED_SOURCE_BYTES: usize = 1024 * 1024;
const MAX_EVAL_CALLS: u32 = 256;
const MAX_DYNAMIC_CODE_RETAINED_SOURCE_BYTES: usize = 1024 * 1024;
const MAX_DYNAMIC_CODE_CALLS: usize = 256;
const MAX_DYNAMIC_CODE_FUNCTIONS: usize = 4096;
const MAX_DYNAMIC_CODE_CLASSES: usize = 1024;
const MAX_LIFETIME_STEPS: u64 = 50_000_000;
const MAX_APPROX_HEAP_BYTES: usize = 128 * 1024 * 1024;
// Keep the byte ceiling below the 100,000-node host conversion cap: even an
// adversarial stream of empty lines then fits in one bounded `takeOutput()`
// result (array root + one node per line) without destructive marshal failure.
const MAX_LIFETIME_OUTPUT_BYTES: usize = 96 * 1024;
const MAX_SYNC_BRIDGE_KIND_BYTES: usize = 64;
const MAX_SYNC_BRIDGE_ARGS: usize = 16;
const MAX_SYNC_BRIDGE_BYTES: usize = 1024 * 1024;
const MAX_SYNC_CAPABILITY_ENTRIES: u32 = 32;

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
    "__zHostId",
    "window",
    "navigator",
    "localStorage",
    "db",
    "host",
    "__zListenerTypes",
    "__zDispatchEvent",
    "__zDrainHostCalls",
    "__zResolveHostCall",
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
fn mono_now() -> f64 {
    js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str("performance"))
        .ok()
        .filter(|p| !p.is_undefined() && !p.is_null())
        .and_then(|p| js_sys::Reflect::get(&p, &JsValue::from_str("now")).ok())
        .filter(JsValue::is_function)
        .map(JsValue::unchecked_into::<js_sys::Function>)
        .and_then(|f| f.call0(&JsValue::UNDEFINED).ok())
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
    drain_host_calls: Option<u32>,
    resolve_host_call: Option<u32>,
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
}

#[wasm_bindgen]
impl Engine {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Engine {
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
            let mut full = String::with_capacity(PREAMBLE.len() + 1 + source.len());
            full.push_str(PREAMBLE);
            full.push('\n');
            full.push_str(source);
            let mut st = compile_script(&full).map_err(|e| JsValue::from_str(&e))?;

            // Attach all execution limits before the first guest instruction.
            // The Cargo dependency disables native JIT features; the explicit
            // runtime switch also keeps this true in native workspace builds
            // where Cargo feature unification may enable zipp-vm's JIT.
            st.set_limits(MAX_LIFETIME_STEPS, None);
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
            st.set_host_call(Box::new(move |kind, args| {
                host_dispatch(&bridges, kind, args)
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
                drain_host_calls: find("__zDrainHostCalls"),
                resolve_host_call: find("__zResolveHostCall"),
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
            Some(st) => st.renew_step_budget(MAX_LIFETIME_STEPS),
            None => false,
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
    #[wasm_bindgen(js_name = setFingerprintSeed)]
    pub fn set_fingerprint_seed(&mut self, lo: u32, hi: u32) {
        if let Some(st) = self.state.as_mut() {
            st.set_fingerprint_seed(((hi as u64) << 32) | lo as u64);
        }
    }

    /// Fingerprint many globals in one boundary crossing.
    ///
    /// One number per index: equal numbers mean `getGlobalsBatch` would
    /// return an equal value, so a host can skip reading the ones that have not
    /// moved. `NaN` means "unknown, read it", which is what a value too large
    /// to walk reports — the fallback is always the old always-read behaviour.
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
            for i in indices {
                let cell = match st.fingerprint_slot(i) {
                    Some(h) => (h & ((1u64 << 53) - 1)) as f64,
                    None => f64::NAN,
                };
                out.push(&JsValue::from_f64(cell));
            }
        }
        Ok(out.into())
    }

    /// Write many globals in one boundary crossing.
    #[wasm_bindgen(js_name = setGlobalsBatch)]
    pub fn set_globals_batch(&mut self, indices: JsValue, values: JsValue) -> Result<(), JsValue> {
        self.ensure_live()?;
        let mut budget = HostValueBudget::default();
        let idx = index_list(&indices, &mut budget).map_err(to_js_error)?;
        require_array(&values, "values").map_err(to_js_error)?;
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

    /// Evaluate `expr` in the script's global context and return its value.
    ///
    /// Each call compiles fresh and installs stable-address definitions, so this
    /// is for one-off host queries — never a per-frame path. Use
    /// [`Engine::callFunction`] there.
    #[wasm_bindgen(js_name = evalInContext)]
    pub fn eval_in_context(&mut self, expr: &str) -> Result<JsValue, JsValue> {
        self.ensure_live()?;
        if self.state.is_none() {
            return Err(JsValue::from_str("zipp: not initialized"));
        }
        if expr.len() > MAX_EVAL_SOURCE_BYTES {
            self.terminate();
            return Err(JsValue::from_str(&format!(
                "RangeError: evalInContext source exceeds the {MAX_EVAL_SOURCE_BYTES}-byte per-call limit"
            )));
        }
        if self.eval_calls >= MAX_EVAL_CALLS {
            self.terminate();
            return Err(JsValue::from_str(&format!(
                "RangeError: evalInContext exceeded its {MAX_EVAL_CALLS}-call lifetime limit"
            )));
        }
        let wrapped_len = EVAL_PREFIX
            .len()
            .checked_add(expr.len())
            .and_then(|n| n.checked_add(EVAL_SUFFIX.len()))
            .ok_or_else(|| JsValue::from_str("RangeError: evalInContext source size overflow"))?;
        let retained = self
            .eval_retained_source_bytes
            .checked_add(wrapped_len)
            .ok_or_else(|| {
                JsValue::from_str("RangeError: evalInContext retained source size overflow")
            })?;
        if retained > MAX_EVAL_RETAINED_SOURCE_BYTES {
            self.terminate();
            return Err(JsValue::from_str(&format!(
                "RangeError: evalInContext exceeded its {MAX_EVAL_RETAINED_SOURCE_BYTES}-byte retained-source lifetime limit"
            )));
        }
        self.eval_calls += 1;
        self.eval_retained_source_bytes = retained;

        // Route the result through JSON so structured values survive; the
        // shallow `eval_in_context` marshaller would render them as ToString.
        let mut wrapped = String::with_capacity(wrapped_len);
        wrapped.push_str(EVAL_PREFIX);
        wrapped.push_str(expr);
        wrapped.push_str(EVAL_SUFFIX);
        let result = self
            .state
            .as_mut()
            .expect("initialization checked above")
            .eval_in_context(&wrapped);
        let value = self.finish_execution(result)?;
        match value.as_str() {
            Some(s) => {
                let mut budget = HostValueBudget::default();
                budget.charge_node().map_err(to_js_error)?;
                budget.charge_string(s).map_err(to_js_error)?;
                let parsed = js_sys::JSON::parse(s).unwrap_or(JsValue::UNDEFINED);
                let value = from_js(&parsed).map_err(to_js_error)?;
                to_js(&value).map_err(to_js_error)
            }
            // `JSON.stringify` yields undefined for a function or undefined.
            None => Ok(JsValue::UNDEFINED),
        }
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

    /// Take the `host.call(...)` requests the script queued during the last
    /// re-entry, as `[{ id, kind, args }]`.
    #[wasm_bindgen(js_name = drainPendingHostCalls)]
    pub fn drain_pending_host_calls(&mut self) -> Result<JsValue, JsValue> {
        self.ensure_live()?;
        let (Some(slot), Some(st)) = (self.helpers.drain_host_calls, self.state.as_mut()) else {
            return Ok(js_sys::Array::new().into());
        };
        let result = st.call_slot(slot, &[]);
        let value = self.finish_execution(result)?;
        to_js(&value).map_err(to_js_error)
    }

    /// Invoke the callback the script passed to `host.call` for `call_id`.
    #[wasm_bindgen(js_name = resolveHostCallback)]
    pub fn resolve_host_callback(&mut self, call_id: u32, result: JsValue) -> Result<(), JsValue> {
        self.ensure_live()?;
        let (Some(slot), Some(st)) = (self.helpers.resolve_host_call, self.state.as_mut()) else {
            return Ok(());
        };
        let mut budget = HostValueBudget::default();
        budget.charge_node().map_err(to_js_error)?;
        let seen = js_sys::WeakSet::<js_sys::Object>::new_typed();
        let result = from_js_bounded(&result, 0, &seen, &mut budget).map_err(to_js_error)?;
        let args = [HostValue::Number(call_id as f64), result];
        let result = st.call_slot(slot, &args).map(|_| ());
        self.finish_execution(result)
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

    /// Drain `console.log`/`info`/`debug` output produced so far.
    #[wasm_bindgen(js_name = takeOutput)]
    pub fn take_output(&mut self) -> Result<JsValue, JsValue> {
        self.ensure_live()?;
        let mut lines = Vec::new();
        if let Some(st) = self.state.as_mut() {
            for line in st.take_output() {
                lines.push(HostValue::String(line));
            }
            for line in st.take_errput() {
                lines.push(HostValue::String(line));
            }
        }
        to_js(&HostValue::Array(lines)).map_err(to_js_error)
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

    fn terminate(&mut self) {
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
        _ => return None,
    })
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
        compile_script, host_dispatch, is_allowed_sync_host_call, sync_host_call_arity, Bridges,
        MAX_SYNC_BRIDGE_ARGS, MAX_SYNC_BRIDGE_BYTES, PREAMBLE, PREAMBLE_BINDINGS,
    };
    use std::cell::RefCell;
    use std::collections::HashSet;
    use std::rc::Rc;

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
