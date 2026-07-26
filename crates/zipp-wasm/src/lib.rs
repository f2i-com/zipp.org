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
use zipp_vm::embed::{compile_script, HostValue, ScriptState, SymbolScope};

const PREAMBLE: &str = include_str!("preamble.js");

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
    pub fn set_db_bridge(&mut self, bridge: JsValue) {
        self.bridges.borrow_mut().db = bridge.dyn_into::<js_sys::Object>().ok();
    }

    /// Install the object backing `localStorage.*`.
    #[wasm_bindgen(js_name = setLocalStorageBridge)]
    pub fn set_local_storage_bridge(&mut self, bridge: JsValue) {
        self.bridges.borrow_mut().local_storage = bridge.dyn_into::<js_sys::Object>().ok();
    }

    /// Compile `source` behind the preamble, run its top level, and return the
    /// symbol map as `{ name: { index, scope } }`.
    ///
    /// Bridges should be installed first — a script's top level (and its
    /// `_init`) commonly reads `localStorage` or queries `db`.
    #[wasm_bindgen(js_name = initScript)]
    pub fn init_script(&mut self, source: &str) -> Result<JsValue, JsValue> {
        let full = format!("{PREAMBLE}\n{source}");
        let mut st = compile_script(&full).map_err(|e| JsValue::from_str(&e))?;

        let bridges = Rc::clone(&self.bridges);
        st.set_host_call(Box::new(move |kind, args| host_dispatch(&bridges, kind, args)));

        st.run_init().map_err(|e| JsValue::from_str(&e))?;

        // Everything the preamble declares is engine plumbing, not script
        // state; the host must not try to sync it into its own UI.
        let preamble_names: std::collections::HashSet<String> = {
            let mut probe = compile_script(PREAMBLE).map_err(|e| JsValue::from_str(&e))?;
            probe.run_init().map_err(|e| JsValue::from_str(&e))?;
            probe.symbols().into_iter().map(|s| s.name).collect()
        };

        self.slots.clear();
        let out = js_sys::Object::new();
        for s in st.symbols() {
            // Preamble names are engine plumbing, with one exception: the host
            // needs a slot for the bridge objects it also writes to (it syncs
            // `window.__foo` keys both ways), so those stay visible. Hosts are
            // expected to exclude them from what they treat as script state.
            if preamble_names.contains(&s.name) && !EXPOSED_PREAMBLE.contains(&s.name.as_str()) {
                continue;
            }
            let entry = js_sys::Object::new();
            let _ = js_sys::Reflect::set(&entry, &"index".into(), &JsValue::from_f64(s.index as f64));
            let _ = js_sys::Reflect::set(
                &entry,
                &"scope".into(),
                &JsValue::from_str(match s.scope {
                    SymbolScope::Function => "function",
                    SymbolScope::Variable => "variable",
                }),
            );
            let _ = js_sys::Reflect::set(&out, &JsValue::from_str(&s.name), &entry);
            self.slots.push((s.name, s.index, s.scope));
        }

        let find = |n: &str| st.symbols().into_iter().find(|s| s.name == n).map(|s| s.index);
        self.helpers = Helpers {
            listener_types: find("__zListenerTypes"),
            dispatch_event: find("__zDispatchEvent"),
            drain_host_calls: find("__zDrainHostCalls"),
            resolve_host_call: find("__zResolveHostCall"),
        };
        self.state = Some(st);
        Ok(out.into())
    }

    /// Read the global in `index`. Values that cannot cross as data (functions,
    /// classes, `Map`, `Date`, …) read as `null`.
    #[wasm_bindgen(js_name = getGlobalByIndex)]
    pub fn get_global_by_index(&mut self, index: u32) -> JsValue {
        match self.state.as_mut() {
            Some(st) => to_js(&st.get_slot(index)),
            None => JsValue::UNDEFINED,
        }
    }

    /// Write the global in `index`. A slot currently holding a function or
    /// class is left alone, so a host that reads all globals and writes them
    /// back cannot destroy the script's own functions.
    #[wasm_bindgen(js_name = setGlobalByIndex)]
    pub fn set_global_by_index(&mut self, index: u32, value: JsValue) {
        if let Some(st) = self.state.as_mut() {
            st.set_slot(index, &from_js(&value, 0));
        }
    }

    /// Read many globals in one boundary crossing.
    #[wasm_bindgen(js_name = getGlobalsBatch)]
    pub fn get_globals_batch(&mut self, indices: JsValue) -> JsValue {
        let out = js_sys::Array::new();
        if let Some(st) = self.state.as_mut() {
            for i in index_list(&indices) {
                out.push(&to_js(&st.get_slot(i)));
            }
        }
        out.into()
    }

    /// Write many globals in one boundary crossing.
    #[wasm_bindgen(js_name = setGlobalsBatch)]
    pub fn set_globals_batch(&mut self, indices: JsValue, values: JsValue) {
        let Some(st) = self.state.as_mut() else { return };
        let idx = index_list(&indices);
        let vals = js_sys::Array::from(&values);
        for (n, i) in idx.into_iter().enumerate() {
            st.set_slot(i, &from_js(&vals.get(n as u32), 0));
        }
    }

    /// Call the top-level function `name`. Microtasks are drained before this
    /// returns, so promise callbacks the call scheduled have already run.
    #[wasm_bindgen(js_name = callFunction)]
    pub fn call_function(&mut self, name: &str, args: JsValue) -> Result<JsValue, JsValue> {
        let slot = self
            .slots
            .iter()
            .find(|(n, _, _)| n == name)
            .map(|(_, i, _)| *i)
            .ok_or_else(|| JsValue::from_str(&format!("zipp: no such function '{name}'")))?;
        let argv: Vec<HostValue> = if args.is_undefined() || args.is_null() {
            Vec::new()
        } else {
            js_sys::Array::from(&args).iter().map(|a| from_js(&a, 0)).collect()
        };
        let st = self.state.as_mut().ok_or_else(|| JsValue::from_str("zipp: not initialized"))?;
        st.call_slot(slot, &argv).map(|v| to_js(&v)).map_err(|e| JsValue::from_str(&e))
    }

    /// Evaluate `expr` in the script's global context and return its value.
    ///
    /// Each call compiles fresh and the compilation is interned for the VM's
    /// lifetime, so this is for one-off host queries — never a per-frame path.
    /// Use [`Engine::callFunction`] there.
    #[wasm_bindgen(js_name = evalInContext)]
    pub fn eval_in_context(&mut self, expr: &str) -> Result<JsValue, JsValue> {
        let st = self.state.as_mut().ok_or_else(|| JsValue::from_str("zipp: not initialized"))?;
        // Route the result through JSON so structured values survive; the
        // shallow `eval_in_context` marshaller would render them as ToString.
        let wrapped = format!("JSON.stringify((function () {{ return ({expr}); }})())");
        match st.eval_in_context(&wrapped) {
            Ok(v) => match v.as_str() {
                Some(s) => js_sys::JSON::parse(s).or(Ok(JsValue::UNDEFINED)),
                // `JSON.stringify` yields undefined for a function or undefined.
                None => Ok(JsValue::UNDEFINED),
            },
            Err(e) => Err(JsValue::from_str(&e)),
        }
    }

    /// Event types the script has registered listeners for, e.g. `["keydown"]`.
    #[wasm_bindgen(js_name = getEventListenerTypes)]
    pub fn get_event_listener_types(&mut self) -> JsValue {
        let out = js_sys::Array::new();
        let (Some(slot), Some(st)) = (self.helpers.listener_types, self.state.as_mut()) else {
            return out.into();
        };
        if let Ok(HostValue::Array(items)) = st.call_slot(slot, &[]) {
            for it in items {
                if let HostValue::String(s) = it {
                    out.push(&JsValue::from_str(&s));
                }
            }
        }
        out.into()
    }

    /// Deliver `event` to every listener registered for `type`, returning how
    /// many ran. The event object is given a no-op `preventDefault` if the host
    /// did not supply one, since scripts call it unconditionally.
    #[wasm_bindgen(js_name = dispatchEvent)]
    pub fn dispatch_event(&mut self, event_type: &str, event: JsValue) -> Result<u32, JsValue> {
        let (Some(slot), Some(st)) = (self.helpers.dispatch_event, self.state.as_mut()) else {
            return Ok(0);
        };
        let args = [HostValue::String(event_type.to_string()), from_js(&event, 0)];
        match st.call_slot(slot, &args) {
            Ok(HostValue::Number(n)) => Ok(n as u32),
            Ok(_) => Ok(0),
            Err(e) => Err(JsValue::from_str(&e)),
        }
    }

    /// Take the `host.call(...)` requests the script queued during the last
    /// re-entry, as `[{ id, kind, args }]`.
    #[wasm_bindgen(js_name = drainPendingHostCalls)]
    pub fn drain_pending_host_calls(&mut self) -> JsValue {
        let out = js_sys::Array::new();
        let (Some(slot), Some(st)) = (self.helpers.drain_host_calls, self.state.as_mut()) else {
            return out.into();
        };
        if let Ok(v) = st.call_slot(slot, &[]) {
            if let HostValue::Array(items) = v {
                for it in items {
                    out.push(&to_js(&it));
                }
            }
        }
        out.into()
    }

    /// Invoke the callback the script passed to `host.call` for `call_id`.
    #[wasm_bindgen(js_name = resolveHostCallback)]
    pub fn resolve_host_callback(&mut self, call_id: u32, result: JsValue) -> Result<(), JsValue> {
        let (Some(slot), Some(st)) = (self.helpers.resolve_host_call, self.state.as_mut()) else {
            return Ok(());
        };
        let args = [HostValue::Number(call_id as f64), from_js(&result, 0)];
        st.call_slot(slot, &args).map(|_| ()).map_err(|e| JsValue::from_str(&e))
    }

    /// Run pending microtasks without calling into the script.
    #[wasm_bindgen]
    pub fn pump(&mut self) {
        if let Some(st) = self.state.as_mut() {
            st.pump();
        }
    }

    /// Drain `console.log`/`info`/`debug` output produced so far.
    #[wasm_bindgen(js_name = takeOutput)]
    pub fn take_output(&mut self) -> JsValue {
        let out = js_sys::Array::new();
        if let Some(st) = self.state.as_mut() {
            for line in st.take_output() {
                out.push(&JsValue::from_str(&line));
            }
            for line in st.take_errput() {
                out.push(&JsValue::from_str(&line));
            }
        }
        out.into()
    }

    /// Tear the VM down. The engine is unusable afterwards.
    #[wasm_bindgen]
    pub fn dispose(&mut self) {
        self.state = None;
        self.slots.clear();
    }
}

impl Default for Engine {
    fn default() -> Self {
        Engine::new()
    }
}

/// Service one synchronous `__zippHostCall`. Anything structured crosses as
/// JSON; an `Err` becomes a JS throw the script can catch.
fn host_dispatch(
    bridges: &Rc<RefCell<Bridges>>,
    kind: &str,
    args: &[String],
) -> Result<String, String> {
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
            "db.getSyncStatus" => {
                r#"{"connected":false,"peers":0,"room":"","peerId":""}"#.into()
            }
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
        "db.get" | "db.hardDelete" => {
            f.call2(&target, &JsValue::from_str(&arg(0)), &JsValue::from_str(&arg(1)))
        }
        "ls.setItem" => {
            f.call2(&target, &JsValue::from_str(&arg(0)), &JsValue::from_str(&arg(1)))
        }
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
    Ok(js_sys::JSON::stringify(&ret)
        .ok()
        .and_then(|s| s.as_string())
        .unwrap_or_else(|| "null".into()))
}

/// Coerce a JS array-like of numbers to slot indices.
fn index_list(v: &JsValue) -> Vec<u32> {
    if v.is_undefined() || v.is_null() {
        return Vec::new();
    }
    js_sys::Array::from(v).iter().filter_map(|x| x.as_f64()).map(|n| n as u32).collect()
}

/// How deep [`from_js`] will follow a host object graph. Matches the engine's
/// own walk limit; a host object deeper than this is not script state.
const MAX_DEPTH: usize = 64;

fn to_js(v: &HostValue) -> JsValue {
    match v {
        HostValue::Undefined => JsValue::UNDEFINED,
        // A function/class/Map/Date reads as null, which the host treats as
        // "not syncable" — matching what it does for its own non-serializables.
        HostValue::Null | HostValue::Opaque => JsValue::NULL,
        HostValue::Bool(b) => JsValue::from_bool(*b),
        HostValue::Number(n) => JsValue::from_f64(*n),
        HostValue::String(s) => JsValue::from_str(s),
        HostValue::Array(items) => {
            let a = js_sys::Array::new_with_length(items.len() as u32);
            for (i, it) in items.iter().enumerate() {
                a.set(i as u32, to_js(it));
            }
            a.into()
        }
        HostValue::Object(pairs) => {
            let o = js_sys::Object::new();
            for (k, val) in pairs {
                let _ = js_sys::Reflect::set(&o, &JsValue::from_str(k), &to_js(val));
            }
            o.into()
        }
    }
}

fn from_js(v: &JsValue, depth: usize) -> HostValue {
    if v.is_undefined() {
        return HostValue::Undefined;
    }
    if v.is_null() {
        return HostValue::Null;
    }
    if let Some(b) = v.as_bool() {
        return HostValue::Bool(b);
    }
    if let Some(n) = v.as_f64() {
        return HostValue::Number(n);
    }
    if let Some(s) = v.as_string() {
        return HostValue::String(s);
    }
    if depth >= MAX_DEPTH {
        return HostValue::Null;
    }
    if js_sys::Array::is_array(v) {
        let a = js_sys::Array::from(v);
        return HostValue::Array(a.iter().map(|x| from_js(&x, depth + 1)).collect());
    }
    if v.is_function() {
        return HostValue::Opaque;
    }
    if v.is_object() {
        let o: &js_sys::Object = v.unchecked_ref();
        let keys = js_sys::Object::keys(o);
        let mut pairs = Vec::with_capacity(keys.length() as usize);
        for k in keys.iter() {
            let Some(name) = k.as_string() else { continue };
            let val = js_sys::Reflect::get(o, &k).unwrap_or(JsValue::UNDEFINED);
            // A method on a host-supplied object is not state; drop it rather
            // than storing a placeholder the script would try to call.
            if val.is_function() {
                continue;
            }
            pairs.push((name, from_js(&val, depth + 1)));
        }
        return HostValue::Object(pairs);
    }
    HostValue::Opaque
}
