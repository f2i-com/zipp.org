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
//! The split is not stylistic: a synchronous bridge cannot do IO, and an
//! asynchronous one cannot be read inline.

use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::prelude::*;
use zipp_vm::embed::{compile_script, HostValue, HostValueBudget, ScriptState, SymbolScope};

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

/// Preamble bindings the host may address by slot even though it did not
/// declare them. `window` in particular is a two-way channel: hosts stash keys
/// on it and read them back, so it needs a stable index.
const EXPOSED_PREAMBLE: &[&str] = &["window", "navigator", "host"];

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
        .and_then(|f| f.dyn_into::<js_sys::Function>().ok())
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
        }
    }

    /// Lines the preamble prepends to the host's source.
    #[wasm_bindgen(getter, js_name = preambleLines)]
    pub fn preamble_lines(&self) -> u32 {
        self.preamble_lines
    }

    /// Install the object backing `db.*`. Its methods are called synchronously
    /// from inside VM execution, so they must not await.
    #[wasm_bindgen(js_name = setDbBridge)]
    pub fn set_db_bridge(&mut self, bridge: JsValue) -> Result<(), JsValue> {
        self.ensure_live()?;
        self.bridges.borrow_mut().db = bridge.dyn_into::<js_sys::Object>().ok();
        Ok(())
    }

    /// Install the object backing `localStorage.*`.
    #[wasm_bindgen(js_name = setLocalStorageBridge)]
    pub fn set_local_storage_bridge(&mut self, bridge: JsValue) -> Result<(), JsValue> {
        self.ensure_live()?;
        self.bridges.borrow_mut().local_storage = bridge.dyn_into::<js_sys::Object>().ok();
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

            // Everything the preamble declares is engine plumbing, not script
            // state; the host must not try to sync it into its own UI.
            let preamble_names: std::collections::HashSet<String> = {
                let mut probe = compile_script(PREAMBLE).map_err(|e| JsValue::from_str(&e))?;
                probe.run_init().map_err(|e| JsValue::from_str(&e))?;
                probe.symbols().into_iter().map(|s| s.name).collect()
            };

            let mut slots = Vec::new();
            let mut exposed = Vec::new();
            for s in st.symbols() {
                // Preamble names are engine plumbing, with one exception: the host
                // needs a slot for the bridge objects it also writes to (it syncs
                // `window.__foo` keys both ways), so those stay visible. Hosts are
                // expected to exclude them from what they treat as script state.
                if preamble_names.contains(&s.name) && !EXPOSED_PREAMBLE.contains(&s.name.as_str())
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

    /// Write many globals in one boundary crossing.
    #[wasm_bindgen(js_name = setGlobalsBatch)]
    pub fn set_globals_batch(&mut self, indices: JsValue, values: JsValue) -> Result<(), JsValue> {
        self.ensure_live()?;
        let mut budget = HostValueBudget::default();
        let idx = index_list(&indices, &mut budget).map_err(to_js_error)?;
        let vals = require_array(&values, "values").map_err(to_js_error)?;
        budget.charge_node().map_err(to_js_error)?;
        budget.ensure_nodes(idx.len()).map_err(to_js_error)?;
        let seen = js_sys::WeakSet::<js_sys::Object>::new_typed();
        let mut converted = Vec::with_capacity(idx.len());
        for (n, i) in idx.into_iter().enumerate() {
            let value =
                from_js_bounded(&vals.get(n as u32), 0, &seen, &mut budget).map_err(to_js_error)?;
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
    matches!(
        kind,
        "db.query"
            | "db.get"
            | "db.create"
            | "db.update"
            | "db.delete"
            | "db.hardDelete"
            | "db.startSync"
            | "db.stopSync"
            | "db.getSyncStatus"
            | "db.getSavedSyncRoom"
            | "ls.getItem"
            | "ls.setItem"
            | "ls.removeItem"
            | "ls.clear"
            | "nav.clipboardWrite"
            | "nav.clipboardRead"
    )
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
    if !is_allowed_sync_host_call(kind) {
        return Err(format!("TypeError: unknown host call '{kind}'"));
    }
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

    let arg = |i: usize| args.get(i).cloned().unwrap_or_default();

    // Clone the handle out before calling: the bridge method runs arbitrary JS,
    // and holding the RefCell borrow across it would panic if it re-entered.
    let (target, method) = match kind.split_once('.') {
        Some(("db", m)) => (bridges.borrow().db.clone(), m),
        Some(("ls", m)) => (bridges.borrow().local_storage.clone(), m),
        Some(("nav", m)) => (bridges.borrow().local_storage.clone(), m),
        _ => return Err(format!("TypeError: unknown host call '{kind}'")),
    };
    let Some(target) = target else {
        // No bridge installed: reads answer empty rather than throwing, so a
        // script that merely probes storage still runs.
        return Ok(match kind {
            "db.query" => "[]".into(),
            "db.get" | "db.getSavedSyncRoom" => "null".into(),
            "db.create" | "db.update" => "{}".into(),
            "db.getSyncStatus" => r#"{"connected":false,"peers":0,"room":"","peerId":""}"#.into(),
            _ => "null".into(),
        });
    };

    let f = js_sys::Reflect::get(&target, &JsValue::from_str(method))
        .ok()
        .and_then(|f| f.dyn_into::<js_sys::Function>().ok())
        .ok_or_else(|| format!("TypeError: host bridge has no method '{kind}'"))?;

    // Per-kind argument shapes: which arguments are JSON and which are plain.
    let call = match kind {
        "db.query" => {
            let opts = js_sys::JSON::parse(&arg(1)).unwrap_or(JsValue::UNDEFINED);
            if opts.is_null() || opts.is_undefined() {
                f.call1(&target, &JsValue::from_str(&arg(0)))
            } else {
                f.call2(&target, &JsValue::from_str(&arg(0)), &opts)
            }
        }
        "db.create" | "db.update" => {
            let data = js_sys::JSON::parse(&arg(1)).unwrap_or(JsValue::UNDEFINED);
            f.call2(&target, &JsValue::from_str(&arg(0)), &data)
        }
        "db.get" | "db.hardDelete" => f.call2(
            &target,
            &JsValue::from_str(&arg(0)),
            &JsValue::from_str(&arg(1)),
        ),
        "ls.setItem" => f.call2(
            &target,
            &JsValue::from_str(&arg(0)),
            &JsValue::from_str(&arg(1)),
        ),
        "db.getSavedSyncRoom" | "ls.clear" | "nav.clipboardRead" => f.call0(&target),
        _ => f.call1(&target, &JsValue::from_str(&arg(0))),
    };

    let ret = call.map_err(|e| {
        // A bridge that threw becomes a catchable JS throw inside the script,
        // carrying the host's own message.
        let msg = e
            .dyn_ref::<js_sys::Error>()
            .map(|er| String::from(er.message()))
            .or_else(|| e.as_string())
            .unwrap_or_else(|| format!("host bridge '{kind}' failed"));
        format!("Error: {msg}")
    })?;

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
        Err(_) => "null".into(),
    };
    if reply.len() > MAX_SYNC_BRIDGE_BYTES {
        return Err(format!(
            "RangeError: host bridge reply exceeds the {MAX_SYNC_BRIDGE_BYTES}-byte limit"
        ));
    }
    Ok(reply)
}

fn to_js_error(error: String) -> JsValue {
    JsValue::from_str(&error)
}

fn require_array<'a>(v: &'a JsValue, label: &str) -> Result<&'a js_sys::Array, String> {
    if !js_sys::Array::is_array(v) {
        return Err(format!("TypeError: {label} must be an array"));
    }
    Ok(v.unchecked_ref())
}

/// Coerce a JS array of numbers to slot indices.
fn index_list(v: &JsValue, budget: &mut HostValueBudget) -> Result<Vec<u32>, String> {
    if v.is_undefined() || v.is_null() {
        return Ok(Vec::new());
    }
    let values = require_array(v, "indices")?;
    let len = values.length() as usize;
    budget.charge_node()?;
    budget.ensure_nodes(len)?;
    let mut out = Vec::with_capacity(len);
    for i in 0..values.length() {
        budget.charge_node()?;
        if let Some(n) = values.get(i).as_f64() {
            out.push(n as u32);
        }
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
    if js_sys::Array::is_array(v) {
        let object: js_sys::Object = v.clone().unchecked_into();
        if seen.has(&object) {
            return Ok(HostValue::Null);
        }
        seen.add(&object);
        let result = (|| {
            let a: &js_sys::Array = v.unchecked_ref();
            let len = a.length() as usize;
            budget.ensure_nodes(len)?;
            let mut items = Vec::with_capacity(len);
            for i in 0..a.length() {
                items.push(from_js_bounded(&a.get(i), depth + 1, seen, budget)?);
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
            let keys = js_sys::Object::keys(&object);
            budget.ensure_nodes(keys.length() as usize)?;
            let mut pairs = Vec::with_capacity(keys.length() as usize);
            for k in keys.iter() {
                let key: &js_sys::JsString = k.unchecked_ref();
                budget.ensure_string_units(key.length() as usize)?;
                let Some(name) = k.as_string() else { continue };
                budget.charge_string(&name)?;
                let val = js_sys::Reflect::get(&object, &k).unwrap_or(JsValue::UNDEFINED);
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
        host_dispatch, is_allowed_sync_host_call, Bridges, MAX_SYNC_BRIDGE_ARGS,
        MAX_SYNC_BRIDGE_BYTES,
    };
    use std::cell::RefCell;
    use std::rc::Rc;

    #[test]
    fn synchronous_host_call_allowlist_is_exact() {
        for kind in [
            "db.query",
            "db.get",
            "db.create",
            "db.update",
            "db.delete",
            "db.hardDelete",
            "db.startSync",
            "db.stopSync",
            "db.getSyncStatus",
            "db.getSavedSyncRoom",
            "ls.getItem",
            "ls.setItem",
            "ls.removeItem",
            "ls.clear",
            "nav.clipboardWrite",
            "nav.clipboardRead",
        ] {
            assert!(
                is_allowed_sync_host_call(kind),
                "documented kind rejected: {kind}"
            );
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
        let bridges = Rc::new(RefCell::new(Bridges::default()));
        let too_many = vec![String::new(); MAX_SYNC_BRIDGE_ARGS + 1];
        let err = host_dispatch(&bridges, "db.query", &too_many).unwrap_err();
        assert!(err.contains("argument limit"), "got {err:?}");

        let too_large = vec!["x".repeat(MAX_SYNC_BRIDGE_BYTES + 1)];
        let err = host_dispatch(&bridges, "db.query", &too_large).unwrap_err();
        assert!(err.contains("arguments exceed"), "got {err:?}");
    }
}
