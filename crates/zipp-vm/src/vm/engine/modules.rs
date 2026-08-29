// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;
use std::io::Read as _;

const MODULE_NOT_FOUND: &str = "TypeError: module not found";

// Confined loaders are used for hostile input. Keep the graph independently
// bounded even when every individual source is below `module_max_bytes`.
// These limits deliberately do not affect the unrestricted compatibility
// loader used by the regular CLI and test262.
const CONFINED_MODULE_COUNT_LIMIT: usize = 256;
const CONFINED_MODULE_TOTAL_BYTES_LIMIT: u64 = 64 * 1024 * 1024;
const CONFINED_MODULE_RECURSION_LIMIT: u32 = 64;

fn lexical_module_path(raw_path: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut path = std::path::PathBuf::new();
    for component in raw_path.components() {
        match component {
            std::path::Component::Prefix(prefix) => path.push(prefix.as_os_str()),
            std::path::Component::RootDir => path.push(component.as_os_str()),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !path.pop() {
                    return None;
                }
            }
            std::path::Component::Normal(part) => path.push(part),
        }
    }
    Some(path)
}

fn lexically_confined(root: &std::path::Path, raw_path: &std::path::Path) -> bool {
    lexical_module_path(raw_path).is_some_and(|path| path.starts_with(root))
}

fn canonical_module_path(
    root: Option<&std::path::Path>,
    raw_path: &std::path::Path,
) -> Result<std::path::PathBuf, Thrown> {
    // Reject lexical escapes before touching the filesystem. Besides avoiding
    // an existence oracle, this prevents a Windows UNC/prefix escape from
    // triggering network resolution during canonicalization.
    if let Some(root) = root {
        if !lexically_confined(root, raw_path) {
            return Err(Thrown(MODULE_NOT_FOUND.into()));
        }
    }
    let path = std::fs::canonicalize(raw_path).map_err(|_| Thrown(MODULE_NOT_FOUND.into()))?;
    if root.is_some_and(|root| !path.starts_with(root)) {
        return Err(Thrown(MODULE_NOT_FOUND.into()));
    }
    Ok(path)
}

#[cfg(all(test, target_os = "windows"))]
mod lexical_tests {
    use super::lexically_confined;

    #[test]
    fn windows_prefixes_and_parent_escapes_are_rejected_without_io() {
        let root = std::path::Path::new(r"C:\sandbox");

        assert!(lexically_confined(
            root,
            std::path::Path::new(r"C:\sandbox\dir\..\ok.mjs")
        ));
        assert!(!lexically_confined(
            root,
            std::path::Path::new(r"C:\sandbox\..\secret.mjs")
        ));
        assert!(!lexically_confined(
            root,
            std::path::Path::new(r"C:\sandbox-other\secret.mjs")
        ));
        assert!(!lexically_confined(
            root,
            std::path::Path::new(r"D:\sandbox\secret.mjs")
        ));
        assert!(!lexically_confined(
            root,
            std::path::Path::new(r"C:relative\secret.mjs")
        ));
        assert!(!lexically_confined(
            root,
            std::path::Path::new(r"\\server\share\secret.mjs")
        ));
    }
}

impl<'p> Vm<'p> {
    pub(crate) fn resolve_module_path(
        &self,
        raw_path: &std::path::Path,
    ) -> Result<std::path::PathBuf, Thrown> {
        canonical_module_path(self.module_root.as_deref(), raw_path)
    }

    /// Run one recursive loader operation under the confined graph-depth cap.
    /// The closure shape makes decrementing unconditional on every ordinary
    /// error return; unrestricted loading does not touch the counter.
    fn with_confined_module_depth<T>(
        &mut self,
        f: impl FnOnce(&mut Self) -> Result<T, Thrown>,
    ) -> Result<T, Thrown> {
        if self.module_root.is_none() {
            return f(self);
        }
        if self.module_load_depth >= CONFINED_MODULE_RECURSION_LIMIT {
            return Err(Thrown(format!(
                "RangeError: sandbox module recursion limit of {CONFINED_MODULE_RECURSION_LIMIT} exceeded"
            )));
        }
        self.module_load_depth += 1;
        let result = f(self);
        self.module_load_depth -= 1;
        result
    }

    fn read_module_bytes(&mut self, path: &std::path::Path) -> Result<Vec<u8>, Thrown> {
        let Some(limit) = self.module_max_bytes else {
            return std::fs::read(path).map_err(|_| Thrown(MODULE_NOT_FOUND.into()));
        };
        let already_seen = self.module_read_bytes.contains_key(path);
        if !already_seen && self.module_read_bytes.len() >= CONFINED_MODULE_COUNT_LIMIT {
            return Err(Thrown(format!(
                "RangeError: sandbox module count limit of {CONFINED_MODULE_COUNT_LIMIT} exceeded"
            )));
        }
        let meta = std::fs::metadata(path).map_err(|_| Thrown(MODULE_NOT_FOUND.into()))?;
        if meta.len() > limit {
            return Err(Thrown(format!(
                "RangeError: module exceeds the sandbox size limit of {limit} bytes"
            )));
        }
        let file = std::fs::File::open(path).map_err(|_| Thrown(MODULE_NOT_FOUND.into()))?;
        let mut bytes = Vec::new();
        file.take(limit + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| Thrown(MODULE_NOT_FOUND.into()))?;
        if bytes.len() as u64 > limit {
            return Err(Thrown(format!(
                "RangeError: module exceeds the sandbox size limit of {limit} bytes"
            )));
        }

        // Charge a canonical path once at its observed high-water mark. This
        // closes both the many-small-files attack and a mutate-between-reads
        // variant without double-charging ordinary repeated/typed reads.
        let observed = bytes.len() as u64;
        let previous = self.module_read_bytes.get(path).copied().unwrap_or(0);
        let growth = observed.saturating_sub(previous);
        let new_total = self
            .module_total_bytes
            .checked_add(growth)
            .filter(|&n| n <= CONFINED_MODULE_TOTAL_BYTES_LIMIT)
            .ok_or_else(|| {
                Thrown(format!(
                    "RangeError: sandbox aggregate module size limit of {CONFINED_MODULE_TOTAL_BYTES_LIMIT} bytes exceeded"
                ))
            })?;
        if !already_seen || observed > previous {
            self.module_read_bytes
                .insert(path.to_path_buf(), observed.max(previous));
            self.module_total_bytes = new_total;
        }
        Ok(bytes)
    }

    fn read_module_text(&mut self, path: &std::path::Path) -> Result<String, Thrown> {
        String::from_utf8(self.read_module_bytes(path)?)
            .map_err(|_| Thrown(MODULE_NOT_FOUND.into()))
    }

    /// Allocate a fresh live global slot from the eval pool (UNINITIALIZED),
    /// for module-loader bookkeeping (canonical namespace/source binding slots).
    pub(crate) fn alloc_module_shared_slot(&mut self) -> Result<u32, Thrown> {
        let cap = self.program.global_count + (FIELD_POOL + EVAL_POOL) as u32;
        if self.eval_global_next >= cap {
            return Err(Thrown(
                "EvalError: too many distinct globals introduced by eval".into(),
            ));
        }
        let s = self.eval_global_next;
        self.eval_global_next += 1;
        self.globals[s as usize] = Value::UNINITIALIZED;
        self.bump_global_gen(s);
        Ok(s)
    }

    /// The CANONICAL live slot of `canon`'s namespace binding, created on
    /// demand (the VALUE fills in when the namespace is registered/imported).
    /// One slot per module ⇒ slot identity == the spec's (module, ~namespace~)
    /// ResolvedBinding identity, which the `export *` ambiguity check compares.
    pub(crate) fn module_ns_slot(&mut self, canon: &std::path::Path) -> Result<u32, Thrown> {
        if let Some(&s) = self.module_ns_slots.get(canon) {
            return Ok(s);
        }
        let s = self.alloc_module_shared_slot()?;
        self.module_ns_slots.insert(canon.to_path_buf(), s);
        Ok(s)
    }

    /// The CANONICAL live slot of `key`'s ModuleSource binding (`import
    /// source`), creating the %AbstractModuleSource%-prototype-linked source
    /// object on first request. `key` is the canonical target path, or the
    /// synthetic `<module source>` host-module key (test262: every
    /// `<module source>` request resolves to ONE shared module record).
    pub(crate) fn module_source_slot(&mut self, key: &std::path::Path) -> Result<u32, Thrown> {
        if let Some(&s) = self.module_source_slots.get(key) {
            return Ok(s);
        }
        let s = self.alloc_module_shared_slot()?;
        let idx = self
            .heap
            .alloc(HeapObj::Object(Box::new(crate::heap::ObjMap::new())));
        if self.abstractmodulesource_proto != 0 {
            self.proto_of
                .insert(idx, Value::heap(self.abstractmodulesource_proto));
        }
        self.globals[s as usize] = Value::heap(idx);
        self.bump_global_gen(s);
        self.module_source_slots.insert(key.to_path_buf(), s);
        Ok(s)
    }

    /// OWN exports (exported name → live slot) of a PREPARED module program,
    /// in source order; registers the namespace's name→slot map and the
    /// in-flight own-exports map (cycle resolution reads both).
    pub(crate) fn register_module_own(
        &mut self,
        ns_idx: u32,
        path: &std::path::Path,
        exports: &[(String, String)],
        names: &[String],
        gmap: &[u32],
    ) -> Vec<(String, u32)> {
        let mut full: Vec<(String, u32)> = Vec::with_capacity(exports.len());
        let mut own_map: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
        for (exported, local) in exports {
            if let Some(i) = names.iter().position(|n| n == local) {
                full.push((exported.clone(), gmap[i]));
                own_map.insert(exported.clone(), gmap[i]);
            }
        }
        self.module_namespaces.insert(ns_idx, own_map.clone());
        self.heap.pin_mirror_dict(ns_idx);
        self.module_own.insert(path.to_path_buf(), own_map);
        full
    }

    /// Recursively load + LINK a MODULE file for a dynamic `import()`, returning its
    /// (fully-linked) Module Namespace exotic. Cached by canonical path so a re-import
    /// (or a cycle) yields the SAME namespace. Steps: (1) cache hit → return; (2) read
    /// + compile as a module (strict); a real `import` decl / `export * as ns` → reject
    /// (unlinkable); (3) run the body in its own per-module env → OWN export live slots;
    /// (4) mark in-progress (module_own) and resolve `export … from` / `export *`
    /// re-exports by recursively loading the dependency and pointing this namespace's
    /// names at the dependency's live slots; (5) build + cache the namespace. A parse/
    /// compile error, a missing dependency, or a throw during evaluation propagates as
    /// `Err`. The given `path` is canonicalized; relative re-export specifiers resolve
    /// against the module's own directory.
    ///
    /// This entry runs the load as a FRESH Evaluate() invocation — a dynamic
    /// `import()`, a deferred-namespace trigger, the entry load. It gets a
    /// DFS stack segment of its own: nothing an outer body left on the
    /// stack counts as EVALUATING for its cycle bookkeeping. A static
    /// request edge of a link in flight goes through `import_module_dep`.
    pub(crate) fn import_module(
        &mut self,
        raw_path: &std::path::Path,
        mtype: Option<&str>,
    ) -> Result<Value, Thrown> {
        self.require_external_code_enabled()?;
        self.with_fresh_dfs_segment(|vm| {
            vm.with_confined_module_depth(|vm| vm.import_module_inner(raw_path, mtype))
        })
    }

    /// `import_module` for a static request edge taken by the link in
    /// flight (InnerModuleEvaluation step 11): the same stack segment, so a
    /// dependency that reaches back into an ancestor lowers the requester's
    /// [[DFSAncestorIndex]] (see `module_dfs_edge`).
    pub(crate) fn import_module_dep(
        &mut self,
        raw_path: &std::path::Path,
        mtype: Option<&str>,
    ) -> Result<Value, Thrown> {
        self.require_external_code_enabled()?;
        self.with_confined_module_depth(|vm| vm.import_module_inner(raw_path, mtype))
    }

    /// Run `f` as a fresh Evaluate() invocation: modules pushed on the DFS
    /// stack by an enclosing evaluation are not "on this stack" inside it.
    pub(crate) fn with_fresh_dfs_segment<T>(&mut self, f: impl FnOnce(&mut Self) -> T) -> T {
        let saved = self.module_dfs_seg_base;
        self.module_dfs_seg_base = self.module_dfs_next;
        let r = f(self);
        self.module_dfs_seg_base = saved;
        r
    }

    fn import_module_inner(
        &mut self,
        raw_path: &std::path::Path,
        mtype: Option<&str>,
    ) -> Result<Value, Thrown> {
        let path = self.resolve_module_path(raw_path)?;
        // A typed import is a DISTINCT module record from the same file's JS
        // module (e.g. a module importing ITSELF with { type: "text" }):
        // cached under (path, type), checked before the JS cache.
        if let Some(t) = mtype {
            if let Some(&ns) = self.typed_module_cache.get(&(path.clone(), t.to_string())) {
                return Ok(ns);
            }
        }
        if mtype.is_none() {
            if let Some(&ns) = self.module_cache.get(&path) {
                // EVALUATING-ASYNC / EVALUATED: Evaluate step 2.a redirects
                // the module to its recorded [[CycleRoot]], and
                // InnerModuleEvaluation step 2.b returns that root's
                // [[EvaluationError]] — a member of an errored component
                // re-throws the component's error even when ITS OWN body
                // finished cleanly before a peer rejected. A requester on
                // the evaluation stack fails with it (Evaluate step 9).
                if let Some(e) = self.module_cycle_error(&path) {
                    self.module_dfs_fail(e);
                    let msg = self.throw_message(e);
                    self.pending_throw = Some(e);
                    return Err(Thrown(msg));
                }
                // A cached module that SUSPENDED (top-level await) — or whose
                // component's root did — settles later importers from the
                // ROOT's still-pending promise (InnerModuleEvaluation
                // 11.c.iv/v wait on requiredModule.[[CycleRoot]]), never from
                // the incomplete namespace directly. NOT for a SELF-import
                // from the module's own (still executing) body: chaining its
                // import promise on its own body promise is a deadlock cycle
                // — the spec resolves a self-import against the in-progress
                // record directly.
                if !self.executing_modules.contains(&path) {
                    if let Some(bp) = self.module_pending_promise(&path) {
                        self.pending_module_body = Some(bp);
                    }
                }
                return Ok(ns);
            }
        }
        // A module that already FAILED evaluation re-throws the SAME error on
        // every later import (its abrupt completion is permanent); a
        // requester on the evaluation stack fails with it (Evaluate step 9).
        if let Some(&e) = self.module_errors.get(&path) {
            self.module_dfs_fail(e);
            let msg = self.throw_message(e);
            self.pending_throw = Some(e);
            return Err(Thrown(msg));
        }
        // A typed import ({type:'json'|'text'|'bytes'}) builds a synthetic
        // namespace with a single `default` export; unknown types reject.
        if let Some(t) = mtype {
            let val = match t {
                // CreateBytesModule: the raw file bytes as a Uint8Array over
                // an IMMUTABLE ArrayBuffer (read as binary — a PNG fixture is
                // not UTF-8).
                "bytes" => {
                    let bytes = self.read_module_bytes(&path)?;
                    let buf = self.heap.alloc(HeapObj::ArrayBuffer {
                        data: bytes.into(),
                        detached: false,
                    });
                    if self.arraybuffer_proto != 0 {
                        self.proto_of
                            .insert(buf, Value::heap(self.arraybuffer_proto));
                    }
                    self.immutable_buffers.insert(buf);
                    // kind 1 = Uint8Array (TA_KINDS); view over the whole buffer.
                    self.build_typed_array(1, &[Value::heap(buf)])?
                }
                "json" | "text" => {
                    let text = self.read_module_text(&path)?;
                    match t {
                        "json" => self.json_parse(text.as_bytes())?,
                        _ => self.alloc_str(text),
                    }
                }
                _ => return Err(Thrown(format!("TypeError: unsupported module type '{t}'"))),
            };
            // Keep the globals backing allocation immovable. Native code pins
            // its base for an activation and JIT property ways may point at a
            // live module slot, so even a cold typed-module import must draw
            // from the preallocated eval pool rather than `Vec::push`.
            let slot = self.alloc_module_shared_slot()?;
            self.globals[slot as usize] = val;
            self.bump_global_gen(slot);
            let ns_idx = self.alloc_empty_namespace();
            self.populate_module_namespace(ns_idx, &[("default".to_string(), slot)]);
            self.typed_module_cache
                .insert((path.clone(), t.to_string()), Value::heap(ns_idx));
            return Ok(Value::heap(ns_idx));
        }
        let code = self.read_module_text(&path)?;
        let ast = crate::front::parse_module(&code).map_err(Thrown)?;
        let prog = match crate::compile::compile_eval(
            &ast,
            &code,
            true,
            false,
            None,
            false,
            std::collections::HashSet::new(),
            true,
            false,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            false,
            false,
        ) {
            Ok(p) => p,
            // Top-level await in an IMPORTED module needs the async-module
            // evaluation pipeline (not built yet): surface a host TypeError —
            // a SyntaxError would misreport a spec-VALID module as malformed.
            Err(e)
                if e.contains("only valid inside an async function")
                    || e.contains("only valid in an async function") =>
            {
                return Err(Thrown(
                    "TypeError: top-level await is not supported in imported modules yet".into(),
                ));
            }
            Err(e) => return Err(Thrown(format!("SyntaxError: {e}"))),
        };
        let exports = prog.module_exports.clone();
        let names = prog.global_names.clone();
        let reexports = prog.module_reexports.clone();
        let star_reexports = prog.module_star_reexports.clone();
        let ns_reexports = prog.module_ns_reexports.clone();
        let imports = prog.module_imports.clone();
        let dir = path.parent().map(|p| p.to_path_buf());
        // STATIC imports resolve BEFORE this module's body prepares/runs
        // (dependencies instantiate + evaluate first, per the link order):
        // Named/Default locals alias the dependency's live export slot;
        // Namespace locals get the namespace value written post-prepare;
        // SideEffect just evaluates the dependency. Resolution failures are
        // link-time SyntaxErrors. A SELF-import (the dominant test262 cycle)
        // aliases the module's OWN exported local by COMPILE slot — resolved
        // to the live slot inside prepare's second pass. Other in-flight
        // cycles are guarded (no recursion blowup).
        let mut import_aliases: std::collections::HashMap<u32, u32> =
            std::collections::HashMap::new();
        let mut self_aliases: std::collections::HashMap<u32, u32> =
            std::collections::HashMap::new();
        let mut ns_writes: Vec<(u32, Value)> = Vec::new();
        // Own (exported name -> compile slot), for self-import aliasing.
        let own_cslot = |name: &str| -> Option<u32> {
            exports
                .iter()
                .find(|(e, _)| e == name)
                .and_then(|(_, local)| names.iter().position(|n| n == local).map(|i| i as u32))
        };
        // Pre-resolved canonical target of an import specifier. The
        // self/in-flight classification below is STABLE for the whole
        // dependency loop: `module_loading` holds exactly the in-flight
        // ancestors throughout it.
        let module_root = self.module_root.clone();
        let canon_of = |dir: Option<&std::path::Path>, spec: &str| -> std::path::PathBuf {
            let raw = match dir {
                Some(d) => d.join(spec),
                None => std::path::PathBuf::from(spec),
            };
            canonical_module_path(module_root.as_deref(), &raw).unwrap_or(raw)
        };
        use crate::bytecode::ImportName as IN;
        // EARLY-PREPARE eligibility: every Named/Default import resolves
        // WITHOUT loading a dependency (i.e. is a self-import of an own
        // export). Such a module's environment instantiates BEFORE its
        // dependencies evaluate — the spec links the whole cycle before any
        // evaluation — so a cycle dependency that calls one of our hoisted
        // functions mid-evaluation (verify-dfs) finds the real binding.
        // Modules with external Named/Default imports keep the
        // resolve-then-prepare order (their live-slot aliases need the
        // dependency loaded first); cycle members calling THEIR hoisted
        // functions before they prepare stay a known limit.
        let early_prepare = imports.iter().all(|e| match &e.import {
            IN::Named(n) => {
                e.mtype.is_none()
                    && canon_of(dir.as_deref(), &e.specifier) == path
                    && own_cslot(n).is_some()
            }
            IN::Default => {
                e.mtype.is_none()
                    && canon_of(dir.as_deref(), &e.specifier) == path
                    && own_cslot("default").is_some()
            }
            _ => true,
        });
        // Aliases resolvable BEFORE any dependency loads: a SELF-import of an
        // own export aliases the module's own compile slot (prepare's second
        // pass maps both onto ONE live slot); `import * as ns` and
        // `import source x` locals alias the target's CANONICAL shared slot
        // (slot identity == spec binding identity; the VALUES fill in below).
        for e in &imports {
            match &e.import {
                IN::Named(n)
                    if e.mtype.is_none() && canon_of(dir.as_deref(), &e.specifier) == path =>
                {
                    if let Some(c) = own_cslot(n) {
                        self_aliases.insert(e.local_slot, c);
                    }
                }
                IN::Default
                    if e.mtype.is_none() && canon_of(dir.as_deref(), &e.specifier) == path =>
                {
                    if let Some(c) = own_cslot("default") {
                        self_aliases.insert(e.local_slot, c);
                    }
                }
                IN::Namespace if e.mtype.is_none() => {
                    let canon = canon_of(dir.as_deref(), &e.specifier);
                    let slot = self.module_ns_slot(&canon)?;
                    import_aliases.insert(e.local_slot, slot);
                }
                IN::Source => {
                    let key = if e.specifier == "<module source>" {
                        std::path::PathBuf::from("<module source>")
                    } else {
                        canon_of(dir.as_deref(), &e.specifier)
                    };
                    let slot = self.module_source_slot(&key)?;
                    import_aliases.insert(e.local_slot, slot);
                }
                _ => {}
            }
        }
        // PRE-REGISTER this module BEFORE any dependency loads (a CYCLIC
        // re-export back into this module — a dependency doing
        // `export { x } from './me'` while we are mid-load — must resolve to
        // the real binding instead of re-evaluating this module as a second
        // instance). EARLY mode prepares the whole environment now; LATE mode
        // pre-allocates live slots for the declared exports (prepare reuses
        // them). An export whose local is itself an IMPORT binding resolves
        // through its dependency; its map entry is patched after prepare.
        let import_locals: std::collections::HashSet<u32> = imports
            .iter()
            .filter(|e| e.local_slot != u32::MAX)
            .map(|e| e.local_slot)
            .collect();
        let decl_set: std::collections::HashSet<u32> =
            prog.module_decl_globals.iter().copied().collect();
        let cap = self.program.global_count + (FIELD_POOL + EVAL_POOL) as u32;
        let mut prealloc: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
        let mut own_pre: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
        if !early_prepare {
            for (exported, local) in &exports {
                if let Some(i) = names.iter().position(|n| n == local) {
                    let c = i as u32;
                    if import_locals.contains(&c) || !decl_set.contains(&c) {
                        continue;
                    }
                    if let Some(&live) = prealloc.get(&c) {
                        own_pre.insert(exported.clone(), live);
                        continue;
                    }
                    if self.eval_global_next >= cap {
                        return Err(Thrown(
                            "EvalError: too many distinct globals introduced by eval".into(),
                        ));
                    }
                    let live = self.eval_global_next;
                    self.eval_global_next += 1;
                    self.globals[live as usize] = Value::UNINITIALIZED;
                    self.bump_global_gen(live);
                    prealloc.insert(c, live);
                    own_pre.insert(exported.clone(), live);
                }
            }
        }
        let mut prog_opt = Some(prog);
        let ns_idx = self.alloc_empty_namespace();
        self.module_cache.insert(path.clone(), Value::heap(ns_idx));
        self.module_pending_reexports.insert(
            path.clone(),
            (
                reexports.clone(),
                star_reexports.clone(),
                ns_reexports.clone(),
                dir.clone(),
            ),
        );
        // EARLY mode: instantiate the environment (per-module slots, function/
        // class install, hoisting) NOW — cycle dependencies see the real
        // bindings; LATE mode registers the pre-allocated own-export slots.
        let mut gmap_base: Option<(Vec<u32>, u32)> = None;
        let mut full: Vec<(String, u32)> = Vec::new();
        if early_prepare {
            let prog = prog_opt.take().expect("module program");
            let prepared = self.prepare_eval_program(
                prog,
                true,
                None,
                false,
                None,
                if import_aliases.is_empty() {
                    None
                } else {
                    Some(&import_aliases)
                },
                if self_aliases.is_empty() {
                    None
                } else {
                    Some(&self_aliases)
                },
                None,
            );
            match prepared {
                Ok((gmap, base_func)) => {
                    let end = (self.main_func_count + self.eval_funcs.len()) as u32;
                    self.module_func_ranges.push((base_func, end, ns_idx));
                    full = self.register_module_own(ns_idx, &path, &exports, &names, &gmap);
                    gmap_base = Some((gmap, base_func));
                }
                Err(e) => {
                    self.module_cache.remove(&path);
                    self.module_namespaces.remove(&ns_idx);
                    self.module_pending_reexports.remove(&path);
                    return Err(e);
                }
            }
        } else {
            self.module_namespaces.insert(ns_idx, own_pre.clone());
            self.heap.pin_mirror_dict(ns_idx);
            self.module_own.insert(path.clone(), own_pre);
        }
        // Dependencies that suspend at top-level await get collected past this
        // mark (nested links use their own marks via the same discipline).
        let lp_mark = self.link_pending_deps.len();
        self.module_loading.insert(path.clone());
        // InnerModuleEvaluation steps 5-10: EVALUATING, on the DFS stack.
        self.module_dfs_enter(&path, lp_mark);
        let import_res = (|| -> Result<(), Thrown> {
            // LOADING phase: every requested module (including phase-import
            // requests) must resolve to a readable file BEFORE linking starts.
            // A failure here is a HOST error (TypeError) — it takes precedence
            // over any link-time SyntaxError a sibling request would raise.
            for e in &imports {
                // The synthetic `<module source>` host module has no file.
                if matches!(e.import, IN::Source) && e.specifier == "<module source>" {
                    continue;
                }
                let dep_raw = match dir.as_deref() {
                    Some(d) => d.join(&e.specifier),
                    None => std::path::PathBuf::from(&e.specifier),
                };
                let confined = self.module_root.is_some();
                let dep_canon = self.resolve_module_path(&dep_raw).map_err(|err| {
                    if confined {
                        err
                    } else {
                        Thrown(format!(
                            "TypeError: Failed to resolve module specifier '{}'",
                            e.specifier
                        ))
                    }
                })?;
                if dep_canon == path {
                    continue; // self-reference: already loaded
                }
            }
            for e in &imports {
                // The synthetic `<module source>` host module has no file to
                // canonicalize (`resolve_module_path` would report it as
                // missing): its (module, ~source~) binding — the shared slot
                // holding the %AbstractModuleSource%-linked object — was
                // created in the pre-pass, and the record never loads, links
                // or evaluates (InitializeEnvironment 7.c binds the local
                // straight to [[ModuleSource]]). Nothing further to do.
                if matches!(e.import, IN::Source) && e.specifier == "<module source>" {
                    continue;
                }
                let dep_raw = match dir.as_deref() {
                    Some(d) => d.join(&e.specifier),
                    None => std::path::PathBuf::from(&e.specifier),
                };
                let dep_canon = self.resolve_module_path(&dep_raw)?;
                // A TYPED import (json/text) is a DISTINCT module record even
                // for the importing file itself — never self/in-flight.
                let is_self = dep_canon == path && e.mtype.is_none();
                let in_flight =
                    !is_self && e.mtype.is_none() && self.module_loading.contains(&dep_canon);
                match &e.import {
                    IN::LoadOnly => {
                        // A phase import's dep is LOADED (recursively — its own
                        // requests resolve eagerly per LoadRequestedModules)
                        // but never linked or evaluated.
                        if !is_self && !in_flight {
                            let mut seen = std::collections::HashSet::new();
                            self.prescan_module_requests(&dep_canon, &mut seen)?;
                        }
                    }
                    IN::Source => {
                        // The synthetic `<module source>` host module was
                        // skipped above. A REAL target is a Source Text
                        // Module Record, and SourceTextModule.GetModuleSource
                        // throws a SyntaxError at link time — only host-defined
                        // module types carry a [[ModuleSource]] (source-phase
                        // imports proposal). The target's request graph still
                        // resolves FIRST: loading errors precede link errors.
                        if !is_self && !in_flight {
                            let mut seen = std::collections::HashSet::new();
                            self.prescan_module_requests(&dep_canon, &mut seen)?;
                        }
                        return Err(Thrown(format!(
                            "SyntaxError: The requested module '{}' does not provide a module source",
                            e.specifier
                        )));
                    }
                    IN::DeferNamespace => {
                        let ns = self.deferred_namespace_for(&dep_raw, e.mtype.as_deref())?;
                        ns_writes.push((e.local_slot, ns));
                    }
                    IN::SideEffect => {
                        if !is_self && !in_flight {
                            self.import_module_sync(&dep_raw, e.mtype.as_deref())?;
                        }
                    }
                    IN::Named(n) => {
                        if is_self {
                            match own_cslot(n) {
                                // An own-export self-import was aliased in
                                // the pre-pass.
                                Some(_) => {}
                                None => {
                                    // Not an own local: an INDIRECT export of
                                    // ourselves (`export {x as n} from dep`) —
                                    // walk the pending re-export chain.
                                    let mut seen = std::collections::HashSet::new();
                                    match self.resolve_pending_export(&path, n, &mut seen)? {
                                        Some(slot) => {
                                            import_aliases.insert(e.local_slot, slot);
                                        }
                                        None => {
                                            return Err(Thrown(format!(
                                                "SyntaxError: The requested module '{}' does not provide an export named '{n}'",
                                                e.specifier
                                            )))
                                        }
                                    }
                                }
                            }
                        } else if in_flight {
                            // The dependency is mid-load (a cycle): resolve
                            // through its registered bindings + pending
                            // re-exports instead of evaluating it again.
                            let mut seen = std::collections::HashSet::new();
                            match self.resolve_pending_export(&dep_canon, n, &mut seen)? {
                                Some(slot) => {
                                    import_aliases.insert(e.local_slot, slot);
                                }
                                None => {
                                    return Err(Thrown(format!(
                                        "SyntaxError: The requested module '{}' does not provide an export named '{n}'",
                                        e.specifier
                                    )))
                                }
                            }
                        } else {
                            match self.resolve_export(&dep_raw, n, e.mtype.as_deref())? {
                                Some(slot) => {
                                    import_aliases.insert(e.local_slot, slot);
                                }
                                None => {
                                    return Err(Thrown(format!(
                                        "SyntaxError: The requested module '{}' does not provide an export named '{n}'",
                                        e.specifier
                                    )))
                                }
                            }
                        }
                    }
                    IN::Default => {
                        if is_self {
                            match own_cslot("default") {
                                // Aliased in the pre-pass.
                                Some(_) => {}
                                None => {
                                    let mut seen = std::collections::HashSet::new();
                                    match self.resolve_pending_export(&path, "default", &mut seen)? {
                                        Some(slot) => {
                                            import_aliases.insert(e.local_slot, slot);
                                        }
                                        None => {
                                            return Err(Thrown(format!(
                                                "SyntaxError: The requested module '{}' does not provide an export named 'default'",
                                                e.specifier
                                            )))
                                        }
                                    }
                                }
                            }
                        } else if in_flight {
                            let mut seen = std::collections::HashSet::new();
                            match self.resolve_pending_export(&dep_canon, "default", &mut seen)? {
                                Some(slot) => {
                                    import_aliases.insert(e.local_slot, slot);
                                }
                                None => {
                                    return Err(Thrown(format!(
                                        "SyntaxError: The requested module '{}' does not provide an export named 'default'",
                                        e.specifier
                                    )))
                                }
                            }
                        } else {
                            match self.resolve_export(&dep_raw, "default", e.mtype.as_deref())? {
                                Some(slot) => {
                                    import_aliases.insert(e.local_slot, slot);
                                }
                                None => {
                                    return Err(Thrown(format!(
                                        "SyntaxError: The requested module '{}' does not provide an export named 'default'",
                                        e.specifier
                                    )))
                                }
                            }
                        }
                    }
                    IN::Namespace => {
                        if let Some(t) = e.mtype.as_deref() {
                            // A TYPED namespace import is a DISTINCT module
                            // record (even of the importing file itself):
                            // per-local value write after prepare.
                            let ns = self.import_module_sync(&dep_raw, Some(t))?;
                            ns_writes.push((e.local_slot, ns));
                        } else {
                            // The local aliases the dependency's CANONICAL
                            // namespace slot (pre-pass); here we ensure the
                            // slot's VALUE: a self/in-flight target's
                            // namespace is already pre-registered in the
                            // cache, anything else imports (evaluates) now.
                            let slot = self.module_ns_slot(&dep_canon)?;
                            let nsv = if is_self || in_flight {
                                self.module_cache.get(&dep_canon).copied()
                            } else {
                                Some(self.import_module_sync(&dep_raw, None)?)
                            };
                            if let Some(v) = nsv {
                                self.globals[slot as usize] = v;
                                self.bump_global_gen(slot);
                            }
                        }
                    }
                }
                // InnerModuleEvaluation step 11.c for an EVALUATION-phase
                // request of a JS module record: cycle bookkeeping, the
                // requested component's recorded error, its pending async
                // root. A typed request is a distinct synthetic record and a
                // phase request is not an evaluation edge.
                if e.mtype.is_none()
                    && matches!(
                        e.import,
                        IN::SideEffect | IN::Named(_) | IN::Default | IN::Namespace
                    )
                {
                    self.module_dfs_edge(&dep_canon)?;
                }
            }
            Ok(())
        })();
        self.module_loading.remove(&path);
        let cleanup_on_err = |vm: &mut Self| {
            vm.module_dfs_unwind(&path);
            vm.module_cache.remove(&path);
            vm.module_namespaces.remove(&ns_idx);
            vm.module_own.remove(&path);
            vm.module_pending_reexports.remove(&path);
            vm.link_pending_deps.truncate(lp_mark);
        };
        if let Err(e) = import_res {
            cleanup_on_err(self);
            return Err(e);
        }
        // PREPARE the module's environment (declared globals → fresh per-module slots,
        // install funcs/classes, hoist) WITHOUT running the body yet — unless the
        // EARLY path already did, before the dependency loop. `gmap[i]` is the
        // live slot for compile-time global slot `i`. The post-prepare
        // registration REFRESHES the pre-registered maps with the final own
        // exports (an export whose local is an import binding now has its real
        // aliased slot; the namespace itself was registered before the
        // dependency loop, so cyclic re-exports already resolved against the
        // pre-allocated slots).
        let (gmap, base_func) = match gmap_base {
            Some(p) => p,
            None => {
                let prog = prog_opt.take().expect("module program");
                let prepared = self.prepare_eval_program(
                    prog,
                    true,
                    None,
                    false,
                    None,
                    if import_aliases.is_empty() {
                        None
                    } else {
                        Some(&import_aliases)
                    },
                    if self_aliases.is_empty() {
                        None
                    } else {
                        Some(&self_aliases)
                    },
                    if prealloc.is_empty() {
                        None
                    } else {
                        Some(&prealloc)
                    },
                );
                match prepared {
                    Ok((gmap, base_func)) => {
                        let end = (self.main_func_count + self.eval_funcs.len()) as u32;
                        self.module_func_ranges.push((base_func, end, ns_idx));
                        full = self.register_module_own(ns_idx, &path, &exports, &names, &gmap);
                        (gmap, base_func)
                    }
                    Err(e) => {
                        cleanup_on_err(self);
                        return Err(e);
                    }
                }
            }
        };
        // Typed/deferred namespace import locals are initialized PRIOR to
        // evaluation (plain namespace locals alias their canonical shared
        // slot, written during the dependency loop).
        for (cslot, ns) in ns_writes {
            let live = gmap[cslot as usize] as usize;
            self.globals[live] = ns;
            self.bump_global_gen(live as u32);
        }
        // Re-exports LINK (and their dependencies evaluate) BEFORE this body
        // runs — the namespace must be complete at evaluation start (a TDZ
        // read of an indirect export during the body must find its binding).
        let linked = self.link_module_reexports(
            full,
            &reexports,
            &star_reexports,
            &ns_reexports,
            dir.as_deref(),
        );
        // The link is over: no further static edge originates here.
        self.module_link_pop(&path);
        let linked = match linked {
            Ok((full2, ambiguous)) => {
                // EARLY ObjMap fill: reflection DURING the body (Object.keys,
                // defineProperty of @@toStringTag, descriptors) sees the real
                // key set + tag; snapshot values refresh after execution.
                self.populate_module_namespace(ns_idx, &full2);
                if !ambiguous.is_empty() {
                    self.module_ambiguous.insert(ns_idx, ambiguous.clone());
                }
                // Dependencies that SUSPENDED at top-level await: defer this
                // body until every one settles (spec AsyncEvaluating). The
                // capability promise stands in as OUR body promise — importers
                // up the chain wait on it transitively.
                let pending_deps: Vec<Value> = self.link_pending_deps.split_off(lp_mark);
                if !pending_deps.is_empty() {
                    let cap = self.alloc_promise();
                    self.deferred_mods.insert(
                        cap,
                        DeferredModuleExec {
                            remaining: pending_deps.len(),
                            base_func,
                            ns_idx,
                            full2: full2.clone(),
                        },
                    );
                    for bp in pending_deps {
                        let _gc = self.gc_lock_guard();
                        let ok_t =
                            Value::heap(self.heap.alloc(HeapObj::Native(native::MODULE_DEP_OK)));
                        let on_ok = Value::heap(self.heap.alloc(HeapObj::Bound {
                            target: ok_t,
                            this: Value::num(cap as f64),
                            args: Vec::new(),
                        }));
                        let fail_t =
                            Value::heap(self.heap.alloc(HeapObj::Native(native::MODULE_DEP_FAIL)));
                        let on_fail = Value::heap(self.heap.alloc(HeapObj::Bound {
                            target: fail_t,
                            this: Value::num(cap as f64),
                            args: Vec::new(),
                        }));
                        self.then_internal(bp.heap_index(), on_ok, on_fail, None);
                    }
                    self.pending_module_body = Some(Value::heap(cap));
                    self.module_body_promise
                        .insert(path.clone(), (Value::heap(cap), true));
                    // EVALUATING-ASYNC: InnerModuleEvaluation step 16.
                    self.module_dfs_close(&path);
                    return {
                        self.module_own.remove(&path);
                        self.module_pending_reexports.remove(&path);
                        Ok(Value::heap(ns_idx))
                    };
                }
                self.executing_modules.insert(path.clone());
                self.pending_module_body_marker = true;
                let exec = self.execute_eval_program(
                    base_func,
                    // Module code's top-level `this` is UNDEFINED (never
                    // the global object).
                    Some(Value::UNDEFINED),
                    None,
                    Value::UNDEFINED,
                    None,
                    None,
                    None,
                );
                self.executing_modules.remove(&path);
                match exec {
                    Ok(v) => {
                        // The body is an ASYNC activation: the result is its
                        // body promise. A no-await body settles synchronously;
                        // a REJECTED body is the module's thrown error; a
                        // PENDING one suspended at top-level await — the
                        // namespace is linked, the promise is published for
                        // the importer to settle from (stage-1 TLA).
                        let st = if v.is_heap() {
                            match self.heap.get(v.heap_index()) {
                                HeapObj::Promise { state, result, .. } => Some((*state, *result)),
                                _ => None,
                            }
                        } else {
                            None
                        };
                        match st {
                            Some((crate::heap::PromiseState::Rejected, r)) => {
                                if let HeapObj::Promise { handled, .. } =
                                    self.heap.get_mut(v.heap_index())
                                {
                                    *handled = true; // the loader consumes it
                                }
                                let msg = self.throw_message(r);
                                self.module_errors.insert(path.clone(), r);
                                // Evaluate step 9: every module still on
                                // this stack — the requesters above and the
                                // finished members of the unclosed component
                                // — is EVALUATED with this error.
                                self.module_dfs_fail(r);
                                self.pending_throw = Some(r);
                                Err(Thrown(msg))
                            }
                            Some((crate::heap::PromiseState::Pending, _)) => {
                                self.pending_module_body = Some(v);
                                // Register for late importers ONLY when the
                                // suspension is a REAL top-level await. A body
                                // can also suspend at its own dynamic-import
                                // ops — chaining a self-import on that body
                                // promise is a deadlock cycle (the resumption
                                // depends on the import it would wait for).
                                if self.module_has_tla(&path) {
                                    self.module_body_promise.insert(path.clone(), (v, false));
                                }
                                Ok((full2, ambiguous))
                            }
                            _ => Ok((full2, ambiguous)),
                        }
                    }
                    Err(e) => Err(e),
                }
            }
            Err(e) => Err(e),
        };
        self.module_own.remove(&path);
        self.module_pending_reexports.remove(&path);
        match linked {
            Ok((full, ambiguous)) => {
                self.populate_module_namespace(ns_idx, &full);
                if !ambiguous.is_empty() {
                    self.module_ambiguous.insert(ns_idx, ambiguous);
                }
                // EVALUATED / EVALUATING-ASYNC: InnerModuleEvaluation step 16.
                self.module_dfs_close(&path);
                Ok(Value::heap(ns_idx))
            }
            Err(e) => {
                // The module threw / a dependency failed: discard the half-built entry
                // so a later import re-evaluates rather than seeing a partial namespace.
                self.module_dfs_unwind(&path);
                self.module_cache.remove(&path);
                self.module_namespaces.remove(&ns_idx);
                Err(e)
            }
        }
    }

    /// Resolve a module's re-exports into `full` (the export name→slot list). Split out
    /// of `import_module` so the in-progress (`module_own`) marker is cleaned up by the
    /// caller on every exit path. A name an `export {x} from` dependency doesn't export
    /// (incl. a circular chain that never grounds) is a link-time SyntaxError per
    /// ResolveExport. `export *` copies the dependency's exports except `default`; a
    /// name supplied by TWO different star sources is AMBIGUOUS — excluded from the
    /// namespace and a SyntaxError to resolve by name (the second tuple element).
    pub(crate) fn link_module_reexports(
        &mut self,
        mut full: Vec<(String, u32)>,
        reexports: &[(String, String, String)],
        star_reexports: &[String],
        ns_reexports: &[(String, String)],
        dir: Option<&std::path::Path>,
    ) -> Result<(Vec<(String, u32)>, std::collections::HashSet<String>), Thrown> {
        // `export * as name from`: import the dependency and export its
        // NAMESPACE object under `name` through the dependency's CANONICAL
        // namespace slot (`self.globals` is a GC root) — the same slot every
        // `import * as` of that module aliases, so the binding identity
        // matches the spec's (module, ~namespace~) ResolvedBinding. Cycles
        // resolve through the loader's pre-registered cache entry.
        for (exported, spec) in ns_reexports {
            let dep = match dir {
                Some(d) => d.join(spec),
                None => std::path::PathBuf::from(spec),
            };
            let canon = self.resolve_module_path(&dep)?;
            let slot = self.module_ns_slot(&canon)?;
            let ns = self.import_module_sync(&dep, None)?;
            self.module_dfs_edge(&canon)?;
            self.globals[slot as usize] = ns;
            self.bump_global_gen(slot);
            full.push((exported.clone(), slot));
        }
        for (exported, imported, spec) in reexports {
            let dep = match dir {
                Some(d) => d.join(spec),
                None => std::path::PathBuf::from(spec),
            };
            if let Some(slot) = self.resolve_export(&dep, imported, None)? {
                full.push((exported.clone(), slot));
            } else {
                return Err(Thrown(format!(
                    "SyntaxError: The requested module '{spec}' does not provide an export named '{imported}'"
                )));
            }
            // `resolve_export` answers from the namespace maps without
            // re-entering the loader: the evaluation edge is taken here.
            let canon = self.resolve_module_path(&dep)?;
            self.module_dfs_edge(&canon)?;
        }
        let own: std::collections::HashSet<String> = full.iter().map(|(n, _)| n.clone()).collect();
        let mut star_seen: std::collections::HashMap<String, u32> =
            std::collections::HashMap::new();
        let mut ambiguous: std::collections::HashSet<String> = std::collections::HashSet::new();
        for spec in star_reexports {
            let dep = match dir {
                Some(d) => d.join(spec),
                None => std::path::PathBuf::from(spec),
            };
            for (name, slot) in self.all_exports(&dep)? {
                if own.contains(&name) {
                    continue; // a local/indirect export shadows star names
                }
                match star_seen.get(&name) {
                    Some(&prev) if prev != slot => {
                        ambiguous.insert(name);
                    }
                    Some(_) => {}
                    None => {
                        star_seen.insert(name.clone(), slot);
                        full.push((name, slot));
                    }
                }
            }
            let canon = self.resolve_module_path(&dep)?;
            self.module_dfs_edge(&canon)?;
        }
        if !ambiguous.is_empty() {
            full.retain(|(n, _)| !ambiguous.contains(n));
        }
        Ok((full, ambiguous))
    }

    /// Resolve a single exported `name` from the module at `raw_path` to its live
    /// global slot (for `export … from` linking). Consults the namespace cache, then
    /// the in-progress own-exports map (cycle break), else recursively loads it.
    /// `Ok(None)` if the module doesn't export `name`.
    pub(crate) fn resolve_export(
        &mut self,
        raw_path: &std::path::Path,
        name: &str,
        mtype: Option<&str>,
    ) -> Result<Option<u32>, Thrown> {
        let dep = self.resolve_module_path(raw_path)?;
        // A TYPED request (json/text) is its own module record: resolve via
        // the typed loader, never the (possibly in-flight) JS module's
        // registries — a file may import ITSELF as text.
        if mtype.is_some() {
            let ns = self.import_module_sync(&dep, mtype)?;
            return Ok(self
                .module_namespaces
                .get(&ns.heap_index())
                .and_then(|m| m.get(name).copied()));
        }
        let ambiguous_check = |vm: &Self, ns_idx: u32| -> Result<(), Thrown> {
            if vm
                .module_ambiguous
                .get(&ns_idx)
                .is_some_and(|s| s.contains(name))
            {
                return Err(Thrown(format!(
                    "SyntaxError: The requested module contains conflicting star exports for name '{name}'"
                )));
            }
            Ok(())
        };
        if let Some(&ns) = self.module_cache.get(&dep) {
            ambiguous_check(self, ns.heap_index())?;
            if let Some(slot) = self
                .module_namespaces
                .get(&ns.heap_index())
                .and_then(|m| m.get(name).copied())
            {
                return Ok(Some(slot));
            }
            // IN-FLIGHT module (pre-registered, link incomplete): its
            // `export … from` entries haven't resolved into the namespace —
            // walk them statically (spec ResolveExport through a cycle; a
            // chain that never grounds yields None → the requester's
            // SyntaxError).
            if self.module_pending_reexports.contains_key(&dep) {
                let mut seen = std::collections::HashSet::new();
                return self.resolve_pending_export(&dep, name, &mut seen);
            }
            return Ok(None);
        }
        if let Some(m) = self.module_own.get(&dep) {
            return Ok(m.get(name).copied());
        }
        let ns = self.import_module_sync(&dep, mtype)?;
        ambiguous_check(self, ns.heap_index())?;
        Ok(self
            .module_namespaces
            .get(&ns.heap_index())
            .and_then(|m| m.get(name).copied()))
    }

    /// The DEFERRED namespace singleton for the module at `raw_path`
    /// (`import defer * as ns` / `import.defer()`): the module's graph LOADS
    /// now (resolution/parse failures surface at link time) but evaluation
    /// waits for the first triggering access. The object starts as a sealed
    /// null-proto carrier of just @@toStringTag = "Deferred Module"; a
    /// trigger evaluates the module and copies the real namespace's bindings
    /// into it (the spec's [[Deferred]] namespace is a distinct object from
    /// the eager one, with its own tag).
    pub(crate) fn deferred_namespace_for(
        &mut self,
        raw_path: &std::path::Path,
        mtype: Option<&str>,
    ) -> Result<Value, Thrown> {
        self.require_external_code_enabled()?;
        let confined = self.module_root.is_some();
        let path = self.resolve_module_path(raw_path).map_err(|err| {
            if confined {
                err
            } else {
                Thrown(format!(
                    "TypeError: Failed to resolve module specifier '{}'",
                    raw_path.display()
                ))
            }
        })?;
        // Keyed by (path, type): the same file imported with and without
        // `with { type: "json" }` is two different modules, so one cache entry
        // must not answer for both.
        let key = (path.clone(), mtype.map(str::to_string));
        if let Some(&ns) = self.deferred_ns_cache.get(&key) {
            return Ok(ns);
        }
        // A non-JS module type (`with { type: "json" }`, and the bytes/text
        // forms) has no module requests and no top-level await by construction,
        // so neither the prescan nor the eager-async sweep below applies — and
        // both of them PARSE the file as JavaScript to find out, which turns a
        // deferred JSON import into `SyntaxError: expected ';'` on the JSON's
        // own opening brace. Skip straight to the deferred namespace object;
        // the trigger re-imports through `import_module`, which knows how to
        // read the type.
        let non_js = matches!(mtype, Some("json") | Some("text") | Some("bytes"));
        if !non_js {
            let mut seen = std::collections::HashSet::new();
            self.prescan_module_requests(&path, &mut seen)?;
            // GatherAsynchronousTransitiveDependencies: the proposal evaluates
            // a deferred graph's ASYNC subgraphs EAGERLY at the importer's
            // link (so a later trigger can stay synchronous). Each async
            // dependency found is a static edge of the importer
            // (InnerModuleEvaluation 11.b.ii): imported through the link in
            // flight, whose body then waits for it; sync-only parts of the
            // graph stay deferred. DFS request order (deterministic —
            // evaluation logs are observable).
            let mut gseen = std::collections::HashSet::new();
            let mut stack = vec![path.clone()];
            while let Some(m) = stack.pop() {
                if !gseen.insert(m.clone()) {
                    continue;
                }
                // Step 6: EVALUATING — on an evaluation stack (linking,
                // executing, or a finished member of a component whose root
                // has not closed). The walk stops HERE: its requests belong
                // to that evaluation, not to this importer.
                if self.module_dfs.contains_key(&m) {
                    continue;
                }
                if self.module_cache.contains_key(&m) || self.module_errors.contains_key(&m) {
                    // EVALUATING-ASYNC / EVALUATED. Step 6 also stops at an
                    // EVALUATED component (IsModuleSCCEvaluated: its ROOT
                    // settled — an errored component counts, its error
                    // surfaces at the trigger). A component whose root is
                    // still pending is an additional async dependency of the
                    // importer, reached through this member: the cache-hit
                    // path publishes the root's promise for the link. A
                    // HasTLA member ends the walk (step 7); a sync member's
                    // requests are walked on (step 8).
                    if self.module_pending_promise(&m).is_none() {
                        continue;
                    }
                    self.import_module_sync(&m, None)?;
                    self.module_dfs_edge(&m)?;
                    if self.module_has_tla(&m) {
                        continue;
                    }
                } else if self.module_has_tla(&m) {
                    // Step 7: an unevaluated async module is gathered — it
                    // evaluates now, its own dependencies with it.
                    self.import_module_sync(&m, None)?;
                    self.module_dfs_edge(&m)?;
                    continue;
                }
                // Step 8: the EVALUATION and DEFER requests; a SOURCE request
                // is never evaluated. Reverse so the explicit stack pops
                // requests in source order.
                if let Ok(reqs) = self.module_requests(&m) {
                    for (r, phase) in reqs.iter().rev() {
                        if *phase != ModuleRequestPhase::Source {
                            stack.push(r.clone());
                        }
                    }
                }
            }
        }
        let _gc = self.gc_lock_guard();
        let tag = self.alloc_str("Deferred Module".to_string());
        let mut m = crate::heap::ObjMap::new();
        m.define(
            "@@toStringTag",
            tag,
            crate::heap::PropAttr {
                writable: false,
                enumerable: false,
                configurable: false,
                accessor: false,
                setter: Value::UNDEFINED,
            },
        );
        m.extensible = false;
        let idx = self.heap.alloc(HeapObj::Object(Box::new(m)));
        self.proto_of.insert(idx, Value::NULL);
        self.deferred_ns_cache.insert(key, Value::heap(idx));
        self.deferred_ns_state
            .insert(idx, (path, mtype.map(str::to_string)));
        Ok(Value::heap(idx))
    }

    /// A TRIGGERING access on a deferred namespace: evaluate the module (and
    /// its eager dependencies) now, then graft the real namespace's live-slot
    /// map and snapshot keys onto the deferred object — keeping ITS
    /// @@toStringTag. No-op for anything that is not an unevaluated deferred
    /// namespace. Call sites gate on `deferred_ns_state` being non-empty.
    pub(crate) fn defer_ns_trigger(&mut self, idx: u32) -> Result<(), Thrown> {
        let Some((path, mtype)) = self.deferred_ns_state.get(&idx).cloned() else {
            return Ok(());
        };
        // ReadyForSyncExecution: the module — or ANYTHING its graph reaches —
        // mid-evaluation/loading (or an unevaluated async subgraph) cannot be
        // completed by a synchronous trigger: TypeError per the proposal,
        // BEFORE evaluating any part of the graph.
        let mut seen = std::collections::HashSet::new();
        if !self.ready_for_sync_execution(&path, &mut seen) {
            return Err(Thrown(
                "TypeError: Cannot synchronously evaluate a deferred module that is currently evaluating"
                    .into(),
            ));
        }
        let real = self.import_module(&path, mtype.as_deref())?;
        // A TLA module's body may still be pending; its bindings update
        // through the shared live slots as it completes.
        self.pending_module_body = None;
        self.defer_ns_adopt(idx, real);
        Ok(())
    }

    /// Graft the REAL namespace's bindings onto deferred namespace `idx`
    /// (live-slot map + snapshot keys), keeping the DEFERRED tag, and mark it
    /// evaluated.
    pub(crate) fn defer_ns_adopt(&mut self, idx: u32, real: Value) {
        self.deferred_ns_state.remove(&idx);
        if real.is_heap() {
            let rid = real.heap_index();
            if let Some(map) = self.module_namespaces.get(&rid).cloned() {
                self.module_namespaces.insert(idx, map);
                self.heap.pin_mirror_dict(idx);
            }
            if let Some(amb) = self.module_ambiguous.get(&rid).cloned() {
                self.module_ambiguous.insert(idx, amb);
            }
            // Copy the snapshot ObjMap (export keys for reflection), then
            // restore the DEFERRED tag.
            let real_map = match self.heap.get(rid) {
                HeapObj::Object(m) => Some(m.clone()),
                _ => None,
            };
            if let Some(mut m) = real_map {
                let _gc = self.gc_lock_guard();
                let tag = self.alloc_str("Deferred Module".to_string());
                m.define(
                    "@@toStringTag",
                    tag,
                    crate::heap::PropAttr {
                        writable: false,
                        enumerable: false,
                        configurable: false,
                        accessor: false,
                        setter: Value::UNDEFINED,
                    },
                );
                m.extensible = false;
                if let HeapObj::Object(slot) = self.heap.get_mut(idx) {
                    *slot = m;
                }
                // Whole-map replacement: invalidate any JIT inline cache that
                // captured the old vals pointer.
                self.heap.bump_version(idx);
            }
        }
    }

    /// Whether the module at canonical `path` uses TOP-LEVEL await (its body
    /// compiles to an activation containing Await ops). Used by import.defer:
    /// the proposal evaluates a deferred graph's ASYNC modules eagerly.
    pub(crate) fn module_has_tla(&mut self, path: &std::path::Path) -> bool {
        let Ok(path) = self.resolve_module_path(path) else {
            return false;
        };
        if let Some(&tla) = self.module_tla_cache.get(&path) {
            return tla;
        }
        let Ok(code) = self.read_module_text(&path) else {
            return false;
        };
        let Ok(ast) = crate::front::parse_module(&code) else {
            return false;
        };
        let Ok(prog) = crate::compile::compile_eval(
            &ast,
            &code,
            true,
            false,
            None,
            false,
            std::collections::HashSet::new(),
            true,
            false,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            false,
            false,
        ) else {
            return false;
        };
        let tla = prog
            .functions
            .first()
            .map(|f| {
                f.code.iter().any(|i| {
                    matches!(
                        i,
                        crate::bytecode::Instr::Await { .. }
                            | crate::bytecode::Instr::ForAwaitNext { .. }
                    )
                })
            })
            .unwrap_or(false);
        self.module_tla_cache.insert(path, tla);
        tla
    }

    /// Whether a [[Get]]/[[Has]]/[[Delete]]/[[DefineOwnProperty]] key
    /// TRIGGERS deferred evaluation: every STRING key except "then" (symbol
    /// keys — "@@"-encoded — and "then" never trigger).
    pub(crate) fn defer_key_triggers(key: &str) -> bool {
        key != "then" && !key.starts_with("@@")
    }

    /// Trigger hook for KEYED operations on a possibly-deferred namespace.
    /// Zero-cost when no unevaluated deferred namespace exists.
    #[inline]
    pub(crate) fn defer_check(&mut self, obj: Value, key: &str) -> Result<(), Thrown> {
        if !self.deferred_ns_state.is_empty()
            && obj.is_heap()
            && Self::defer_key_triggers(key)
            && self.deferred_ns_state.contains_key(&obj.heap_index())
        {
            self.defer_ns_trigger(obj.heap_index())?;
        }
        Ok(())
    }

    /// Trigger hook for [[OwnPropertyKeys]]-class operations (always trigger).
    #[inline]
    pub(crate) fn defer_check_all(&mut self, obj: Value) -> Result<(), Thrown> {
        if !self.deferred_ns_state.is_empty()
            && obj.is_heap()
            && self.deferred_ns_state.contains_key(&obj.heap_index())
        {
            self.defer_ns_trigger(obj.heap_index())?;
        }
        Ok(())
    }

    /// The canonical paths of `path`'s DIRECT module requests (import /
    /// export-from / export-* sources) in source order, each with its
    /// phase, resolving each against the file's dir. Parsed ONCE per
    /// canonical path (module source is immutable for the life of the
    /// loader); every later call is one map probe — the walks that call this
    /// per graph node never re-read or re-parse a file. A file that fails to
    /// read, parse or resolve is not cached: its error surfaces at link.
    pub(crate) fn module_requests(
        &mut self,
        path: &std::path::PathBuf,
    ) -> Result<std::sync::Arc<Vec<(std::path::PathBuf, ModuleRequestPhase)>>, Thrown> {
        let path = self.resolve_module_path(path)?;
        if let Some(cached) = self.module_requests_cache.get(&path) {
            return Ok(cached.clone());
        }
        let confined = self.module_root.is_some();
        let code = self.read_module_text(&path).map_err(|err| {
            if confined || err.0 != MODULE_NOT_FOUND {
                err
            } else {
                Thrown(format!(
                    "TypeError: Failed to resolve module specifier '{}'",
                    path.display()
                ))
            }
        })?;
        let ast = crate::front::parse_module(&code).map_err(Thrown)?;
        let dir = path.parent().map(|p| p.to_path_buf());
        let mut out = Vec::new();
        for s in &ast.body {
            use crate::parse::ast::{ExportDecl, ImportPhase, Stmt};
            let spec: Option<(String, ModuleRequestPhase)> = match s {
                Stmt::Import(d) => Some((
                    d.source.to_lossy_string(),
                    match d.phase {
                        ImportPhase::Evaluation => ModuleRequestPhase::Evaluation,
                        ImportPhase::Defer => ModuleRequestPhase::Defer,
                        ImportPhase::Source => ModuleRequestPhase::Source,
                    },
                )),
                Stmt::Export(e) => match &**e {
                    ExportDecl::Named {
                        source: Some(src), ..
                    } => Some((src.to_lossy_string(), ModuleRequestPhase::Evaluation)),
                    ExportDecl::All { source, .. } => {
                        Some((source.to_lossy_string(), ModuleRequestPhase::Evaluation))
                    }
                    _ => None,
                },
                _ => None,
            };
            if let Some((spec, phase)) = spec {
                // The synthetic `<module source>` host module (source-phase
                // imports) has no file and never evaluates — not a request.
                if spec == "<module source>" {
                    continue;
                }
                let raw = match dir.as_deref() {
                    Some(d) => d.join(&spec),
                    None => std::path::PathBuf::from(&spec),
                };
                let canon = self.resolve_module_path(&raw).map_err(|err| {
                    if confined {
                        err
                    } else {
                        Thrown(format!(
                            "TypeError: Failed to resolve module specifier '{spec}'"
                        ))
                    }
                })?;
                out.push((canon, phase));
            }
        }
        let out = std::sync::Arc::new(out);
        self.module_requests_cache.insert(path, out.clone());
        Ok(out)
    }

    /// InnerModuleEvaluation steps 5-10 for a module entering its link:
    /// status EVALUATING, [[DFSIndex]] = [[DFSAncestorIndex]] = the next
    /// index, pushed on the stack; and it becomes the requester of every
    /// static edge until `module_link_pop`.
    fn module_dfs_enter(&mut self, path: &std::path::PathBuf, lp_mark: usize) {
        let idx = self.module_dfs_next;
        self.module_dfs_next += 1;
        self.module_dfs.insert(path.clone(), (idx, idx));
        self.module_dfs_stack.push(path.clone());
        self.module_link_stack.push((path.clone(), lp_mark));
    }

    /// The link of `path` (dependency loop + re-export link) is over.
    fn module_link_pop(&mut self, path: &std::path::Path) {
        if self
            .module_link_stack
            .last()
            .is_some_and(|(p, _)| p.as_path() == path)
        {
            self.module_link_stack.pop();
        }
    }

    /// `m`'s (index, ancestor index) if it is EVALUATING on the CURRENT
    /// Evaluate()'s stack segment.
    fn module_on_dfs_stack(&self, m: &std::path::Path) -> Option<(u32, u32)> {
        match self.module_dfs.get(m) {
            Some(&(idx, anc)) if idx >= self.module_dfs_seg_base => Some((idx, anc)),
            _ => None,
        }
    }

    /// InnerModuleEvaluation step 16: `path`'s evaluation phase is over (its
    /// body ran, suspended at top-level await, or was deferred behind
    /// pending async dependencies). A ROOT — ancestor index == own index —
    /// closes its strongly connected component: every member above it on
    /// the stack is popped, leaves EVALUATING, and records `path` as its
    /// [[CycleRoot]]. A non-root stays on the stack until its root closes.
    fn module_dfs_close(&mut self, path: &std::path::PathBuf) {
        self.module_link_pop(path);
        let Some(&(idx, anc)) = self.module_dfs.get(path) else {
            return;
        };
        if anc != idx {
            return;
        }
        while let Some(m) = self.module_dfs_stack.pop() {
            self.module_dfs.remove(&m);
            if m == *path {
                break;
            }
            self.module_cycle_root.insert(m, path.clone());
        }
    }

    /// The evaluation of `path` aborted — a link error, or an evaluation
    /// error `module_dfs_fail` has already recorded on the stack: drop it
    /// and everything above it from the stack. No [[CycleRoot]] is recorded
    /// (Evaluate step 9 leaves an errored stack's roots EMPTY; each module
    /// answers with its own recorded error).
    fn module_dfs_unwind(&mut self, path: &std::path::PathBuf) {
        self.module_link_pop(path);
        if let Some(pos) = self.module_dfs_stack.iter().rposition(|m| m == path) {
            for m in self.module_dfs_stack.drain(pos..) {
                self.module_dfs.remove(&m);
            }
        }
    }

    /// Evaluate step 9: an abrupt InnerModuleEvaluation completion makes
    /// EVERY module still on this Evaluate()'s stack EVALUATED with
    /// [[EvaluationError]] = `err` — the requesters up the chain and the
    /// finished members of a component that never closed. A module that
    /// already recorded an error keeps it. A no-op outside a link (a
    /// dynamic import of an errored module has an empty stack).
    pub(crate) fn module_dfs_fail(&mut self, err: Value) {
        let base = self.module_dfs_seg_base;
        for m in self.module_dfs_stack.iter().rev() {
            match self.module_dfs.get(m) {
                Some(&(idx, _)) if idx >= base => {}
                _ => break,
            }
            self.module_errors.entry(m.clone()).or_insert(err);
        }
    }

    /// InnerModuleEvaluation step 11.c for the static request edge from the
    /// link in flight (top of `module_link_stack`) to `dep`, taken AFTER the
    /// request was resolved / evaluated:
    /// - iii. `dep` still EVALUATING on this stack (a back-edge, or a fresh
    ///   dependency that joined an ancestor's component): the requester's
    ///   [[DFSAncestorIndex]] drops to `dep`'s.
    /// - iv. otherwise `dep` stands for its [[CycleRoot]]: a recorded
    ///   [[EvaluationError]] aborts the evaluation (Evaluate step 9).
    /// - v. an async-evaluating `dep` (its root's pending promise) becomes a
    ///   pending async dependency of the requester.
    /// Named / default imports and `export … from` resolve through the
    /// namespace maps without re-entering the loader, so this is the ONLY
    /// place those edges get their evaluation-time bookkeeping. A no-op
    /// outside a link and for a self-request.
    pub(crate) fn module_dfs_edge(&mut self, dep: &std::path::PathBuf) -> Result<(), Thrown> {
        let Some((parent, _)) = self.module_link_stack.last().cloned() else {
            return Ok(());
        };
        if parent == *dep {
            return Ok(());
        }
        if let Some((_, dep_anc)) = self.module_on_dfs_stack(dep) {
            if let Some(p) = self.module_dfs.get_mut(&parent) {
                p.1 = p.1.min(dep_anc);
            }
        } else if let Some(e) = self.module_cycle_error(dep) {
            self.module_dfs_fail(e);
            let msg = self.throw_message(e);
            self.pending_throw = Some(e);
            return Err(Thrown(msg));
        }
        if let Some(bp) = self.module_pending_promise(dep) {
            self.link_collect_pending(bp);
        }
        Ok(())
    }

    /// Record a pending dependency promise for the link in flight — the
    /// importer's body waits for it — once per link frame (the same root
    /// is reached through several members of its component).
    pub(crate) fn link_collect_pending(&mut self, bp: Value) {
        let mark = self.module_link_stack.last().map_or(0, |(_, m)| *m);
        let mark = mark.min(self.link_pending_deps.len());
        if !self.link_pending_deps[mark..].contains(&bp) {
            self.link_pending_deps.push(bp);
        }
    }

    /// `m`'s registered promise if it is still PENDING.
    fn module_registered_pending(&self, m: &std::path::Path) -> Option<Value> {
        let &(bp, _) = self.module_body_promise.get(m)?;
        let pending = bp.is_heap()
            && matches!(
                self.heap.get(bp.heap_index()),
                HeapObj::Promise {
                    state: crate::heap::PromiseState::Pending,
                    ..
                }
            );
        pending.then_some(bp)
    }

    /// The still-pending promise a requester of `m` must wait on, if any:
    /// its recorded [[CycleRoot]]'s while the component is evaluating-async
    /// (InnerModuleEvaluation 11.c.iv/v wait on the root; the root's
    /// completion transitively covers every async member), else `m`'s own
    /// suspended body. Two map probes.
    pub(crate) fn module_pending_promise(&self, m: &std::path::Path) -> Option<Value> {
        if let Some(root) = self.module_cycle_root.get(m) {
            if let Some(bp) = self.module_registered_pending(root) {
                return Some(bp);
            }
        }
        self.module_registered_pending(m)
    }

    /// `m`'s own [[EvaluationError]]: recorded synchronously in
    /// `module_errors`, or — AsyncModuleExecutionRejected step 5 — its
    /// registered body / capability promise having REJECTED after it
    /// suspended, memoised into `module_errors` on first sight (the loader
    /// consumes the rejection).
    fn module_recorded_error(&mut self, m: &std::path::PathBuf) -> Option<Value> {
        if let Some(&e) = self.module_errors.get(m) {
            return Some(e);
        }
        let &(bp, _) = self.module_body_promise.get(m)?;
        if !bp.is_heap() {
            return None;
        }
        let r = match self.heap.get(bp.heap_index()) {
            HeapObj::Promise {
                state: crate::heap::PromiseState::Rejected,
                result,
                ..
            } => *result,
            _ => return None,
        };
        if let HeapObj::Promise { handled, .. } = self.heap.get_mut(bp.heap_index()) {
            *handled = true;
        }
        self.module_errors.insert(m.clone(), r);
        Some(r)
    }

    /// The permanent [[EvaluationError]] a re-import of an already-cached
    /// (EVALUATING-ASYNC / EVALUATED) module must re-throw, or None when it
    /// evaluated — or is still evaluating — cleanly.
    ///
    /// Evaluate step 2.a redirects the module to its recorded [[CycleRoot]]
    /// and InnerModuleEvaluation step 2.b returns the root's error: a
    /// rejection anywhere in a component propagates up
    /// [[AsyncParentModules]] to its root, so a member whose own body
    /// finished cleanly still re-throws the component's error — and the
    /// root's error wins even over the member's OWN later rejection
    /// (AsyncModuleExecutionRejected step 2 leaves an already-errored root's
    /// [[EvaluationError]] in place, and the root's [[TopLevelCapability]]
    /// is what the import settles from). A module with no recorded root
    /// (its own root, or errored synchronously — Evaluate step 9 records no
    /// root) answers for itself. The effective error is memoised under the
    /// member's own path.
    ///
    /// Cost: two `is_empty` checks when nothing ever failed or suspended;
    /// otherwise at most four map probes — the root is RECORDED at DFS
    /// time, never re-derived from the request graph.
    pub(crate) fn module_cycle_error(&mut self, path: &std::path::PathBuf) -> Option<Value> {
        if self.module_errors.is_empty() && self.module_body_promise.is_empty() {
            return None;
        }
        if let Some(&e) = self.module_errors.get(path) {
            return Some(e);
        }
        // EVALUATING (on a stack): InnerModuleEvaluation step 3 — no error yet.
        if self.module_dfs.contains_key(path) {
            return None;
        }
        if let Some(root) = self.module_cycle_root.get(path).cloned() {
            if let Some(e) = self.module_recorded_error(&root) {
                self.module_errors.insert(path.clone(), e);
                return Some(e);
            }
        }
        self.module_recorded_error(path)
    }

    /// The spec's LOADING phase for a module that is never evaluated (a phase
    /// import's target): read + parse it and resolve its requested specifiers
    /// recursively — an unreadable file is a host TypeError, a parse failure a
    /// SyntaxError. Already-cached / already-seen modules are done. Every
    /// phase of request loads (LoadRequestedModules).
    pub(crate) fn prescan_module_requests(
        &mut self,
        path: &std::path::PathBuf,
        seen: &mut std::collections::HashSet<std::path::PathBuf>,
    ) -> Result<(), Thrown> {
        self.with_confined_module_depth(|vm| vm.prescan_module_requests_inner(path, seen))
    }

    fn prescan_module_requests_inner(
        &mut self,
        path: &std::path::PathBuf,
        seen: &mut std::collections::HashSet<std::path::PathBuf>,
    ) -> Result<(), Thrown> {
        if !seen.insert(path.clone()) || self.module_cache.contains_key(path) {
            return Ok(());
        }
        let reqs = self.module_requests(path)?;
        for (canon, _) in reqs.iter() {
            self.prescan_module_requests(canon, seen)?;
        }
        Ok(())
    }

    /// The proposal's ReadyForSyncExecution: can `path`'s graph evaluate
    /// synchronously RIGHT NOW? False when any reachable unevaluated module
    /// is mid-evaluation/loading or contains top-level await.
    pub(crate) fn ready_for_sync_execution(
        &mut self,
        path: &std::path::PathBuf,
        seen: &mut std::collections::HashSet<std::path::PathBuf>,
    ) -> bool {
        self.with_confined_module_depth(|vm| Ok(vm.ready_for_sync_execution_inner(path, seen)))
            .unwrap_or(false)
    }

    fn ready_for_sync_execution_inner(
        &mut self,
        path: &std::path::PathBuf,
        seen: &mut std::collections::HashSet<std::path::PathBuf>,
    ) -> bool {
        if !seen.insert(path.clone()) {
            return true;
        }
        // EVALUATING — on an evaluation stack (linking, executing, or a
        // finished member of a component whose root has not closed). BEFORE
        // the cache check: a namespace is pre-registered (cached) while its
        // module is still evaluating / mid-link.
        if self.module_dfs.contains_key(path) {
            return false;
        }
        if self.module_cache.contains_key(path) || self.module_errors.contains_key(path) {
            // EVALUATED / EVALUATING-ASYNC: IsModuleSCCEvaluated — ready once
            // the component's root (and this member) settled; an errored
            // component is EVALUATED (the trigger re-throws its error).
            return self.module_pending_promise(path).is_none();
        }
        if self.module_has_tla(path) {
            return false; // HasTLA, not yet evaluated
        }
        let Ok(reqs) = self.module_requests(path) else {
            return true; // resolution errors surface at evaluation
        };
        for (dep, phase) in reqs.iter() {
            // A SOURCE request is never evaluated.
            if *phase == ModuleRequestPhase::Source {
                continue;
            }
            if !self.ready_for_sync_execution(dep, seen) {
                return false;
            }
        }
        true
    }

    /// ResolveExport through modules whose link is IN FLIGHT: own bindings
    /// resolve directly; `export {x as y} from` / `export *` entries recurse.
    /// `seen` is the spec's resolveSet — a repeated (module, name) request is
    /// a circular chain that never grounds → None.
    pub(crate) fn resolve_pending_export(
        &mut self,
        dep: &std::path::PathBuf,
        name: &str,
        seen: &mut std::collections::HashSet<(std::path::PathBuf, String)>,
    ) -> Result<Option<u32>, Thrown> {
        self.with_confined_module_depth(|vm| vm.resolve_pending_export_inner(dep, name, seen))
    }

    fn resolve_pending_export_inner(
        &mut self,
        dep: &std::path::PathBuf,
        name: &str,
        seen: &mut std::collections::HashSet<(std::path::PathBuf, String)>,
    ) -> Result<Option<u32>, Thrown> {
        if !seen.insert((dep.clone(), name.to_string())) {
            return Ok(None);
        }
        if let Some(slot) = self.module_own.get(dep).and_then(|m| m.get(name).copied()) {
            return Ok(Some(slot));
        }
        let Some((reex, stars, nsreex, pdir)) = self.module_pending_reexports.get(dep).cloned()
        else {
            // Completed (or never in-flight): normal resolution.
            return self.resolve_export(dep, name, None);
        };
        let module_root = self.module_root.clone();
        let join =
            |pdir: Option<&std::path::Path>, spec: &str| -> Result<std::path::PathBuf, Thrown> {
                let raw = match pdir {
                    Some(d) => d.join(spec),
                    None => std::path::PathBuf::from(spec),
                };
                canonical_module_path(module_root.as_deref(), &raw)
            };
        for (exported, imported, spec) in &reex {
            if exported == name {
                let target = join(pdir.as_deref(), spec)?;
                return self.resolve_pending_export(&target, imported, seen);
            }
        }
        // `export * as name from spec`: an INDIRECT export whose binding is
        // the dependency's namespace — its canonical shared slot (created on
        // demand; the value fills in when the dependency links).
        for (exported, spec) in &nsreex {
            if exported == name {
                let target = join(pdir.as_deref(), spec)?;
                return Ok(Some(self.module_ns_slot(&target)?));
            }
        }
        if name != "default" {
            for spec in &stars {
                let target = join(pdir.as_deref(), spec)?;
                if let Some(slot) = self.resolve_pending_export(&target, name, seen)? {
                    return Ok(Some(slot));
                }
            }
        }
        Ok(None)
    }

    /// Enumerate all exports of the module at `raw_path` (excluding `default`) as
    /// (name, live slot), for `export * from`. Uses the cache / in-progress map, else
    /// recursively loads it.
    pub(crate) fn all_exports(
        &mut self,
        raw_path: &std::path::Path,
    ) -> Result<Vec<(String, u32)>, Thrown> {
        let dep = self.resolve_module_path(raw_path)?;
        let collect = |m: &std::collections::HashMap<String, u32>| -> Vec<(String, u32)> {
            m.iter()
                .filter(|(n, _)| n.as_str() != "default")
                .map(|(n, s)| (n.clone(), *s))
                .collect()
        };
        if let Some(&ns) = self.module_cache.get(&dep) {
            return Ok(self
                .module_namespaces
                .get(&ns.heap_index())
                .map(collect)
                .unwrap_or_default());
        }
        if let Some(m) = self.module_own.get(&dep) {
            return Ok(collect(m));
        }
        let ns = self.import_module_sync(&dep, None)?;
        Ok(self
            .module_namespaces
            .get(&ns.heap_index())
            .map(collect)
            .unwrap_or_default())
    }
}

#[cfg(test)]
mod confined_budget_tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

    struct TestDir(std::path::PathBuf);

    impl TestDir {
        fn new() -> Self {
            let id = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("zipp-module-budget-{}-{id}", std::process::id()));
            std::fs::create_dir(&path).expect("create isolated module fixture directory");
            Self(path)
        }

        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn with_confined_vm(test: impl FnOnce(&mut Vm<'_>, &std::path::Path)) {
        let dir = TestDir::new();
        let ast = crate::front::parse_script("").expect("parse empty host");
        let program = crate::compile::compile_program(&ast, "").expect("compile empty host");
        let mut vm = Vm::new(&program);
        vm.set_module_root(dir.path().to_path_buf(), 1024 * 1024)
            .expect("confine loader");
        test(&mut vm, dir.path());
    }

    #[test]
    fn repeated_canonical_reads_charge_only_observed_growth() {
        with_confined_vm(|vm, root| {
            let path = root.join("same.mjs");
            std::fs::write(&path, b"export const x=1;").expect("write fixture");
            let path = std::fs::canonicalize(path).expect("canonical fixture");

            let first = vm.read_module_bytes(&path).expect("first read");
            assert_eq!(vm.module_read_bytes.len(), 1);
            assert_eq!(vm.module_total_bytes, first.len() as u64);

            let second = vm.read_module_bytes(&path).expect("repeated read");
            assert_eq!(second, first);
            assert_eq!(vm.module_read_bytes.len(), 1);
            assert_eq!(vm.module_total_bytes, first.len() as u64);

            std::fs::write(&path, b"export const x=123456;").expect("grow fixture");
            let grown = vm.read_module_bytes(&path).expect("grown read");
            assert!(grown.len() > first.len());
            assert_eq!(vm.module_read_bytes.len(), 1);
            assert_eq!(vm.module_total_bytes, grown.len() as u64);
        });
    }

    #[test]
    fn deferred_typed_and_eager_views_share_the_same_path_charge() {
        with_confined_vm(|vm, root| {
            vm.run().expect("initialize host realm");
            let path = root.join("multi-view.mjs");
            std::fs::write(&path, b"export const value=7;").expect("write fixture");
            let path = std::fs::canonicalize(path).expect("canonical fixture");
            let expected = std::fs::metadata(&path).expect("fixture metadata").len();

            let deferred = vm
                .deferred_namespace_for(&path, None)
                .expect("deferred prescan");
            assert_eq!(vm.module_read_bytes.len(), 1);
            assert_eq!(vm.module_total_bytes, expected);

            vm.defer_ns_trigger(deferred.heap_index())
                .expect("trigger eager evaluation");
            assert_eq!(vm.module_read_bytes.len(), 1);
            assert_eq!(vm.module_total_bytes, expected);

            vm.import_module(&path, Some("text"))
                .expect("typed view of same canonical file");
            assert_eq!(vm.module_read_bytes.len(), 1);
            assert_eq!(vm.module_total_bytes, expected);
        });
    }

    #[test]
    fn distinct_confined_module_count_is_bounded() {
        with_confined_vm(|vm, root| {
            let mut first = None;
            for i in 0..CONFINED_MODULE_COUNT_LIMIT {
                let path = root.join(format!("m{i}.mjs"));
                std::fs::write(&path, []).expect("write fixture");
                let path = std::fs::canonicalize(path).expect("canonical fixture");
                vm.read_module_bytes(&path)
                    .expect("within module count budget");
                first.get_or_insert(path);
            }
            assert_eq!(vm.module_read_bytes.len(), CONFINED_MODULE_COUNT_LIMIT);

            // Cache/prescan/typed paths may reread an existing file after the
            // count is full; that must remain allowed and uncharged.
            vm.read_module_bytes(first.as_ref().expect("first path"))
                .expect("existing path is not double-charged");

            let extra = root.join("too-many.mjs");
            std::fs::write(&extra, []).expect("write extra fixture");
            let extra = std::fs::canonicalize(extra).expect("canonical extra fixture");
            let err = vm
                .read_module_bytes(&extra)
                .expect_err("new path past count cap must fail closed");
            assert!(err.0.contains("module count limit"), "got {err:?}");
        });
    }

    #[test]
    fn aggregate_bytes_are_checked_before_committing_a_new_path() {
        with_confined_vm(|vm, root| {
            let charged = root.join("already-charged.mjs");
            vm.module_read_bytes
                .insert(charged, CONFINED_MODULE_TOTAL_BYTES_LIMIT - 2);
            vm.module_total_bytes = CONFINED_MODULE_TOTAL_BYTES_LIMIT - 2;

            let path = root.join("overflow.mjs");
            std::fs::write(&path, b"abc").expect("write fixture");
            let path = std::fs::canonicalize(path).expect("canonical fixture");
            let err = vm
                .read_module_bytes(&path)
                .expect_err("aggregate overflow must fail closed");
            assert!(err.0.contains("aggregate module size limit"), "got {err:?}");
            assert!(!vm.module_read_bytes.contains_key(&path));
            assert_eq!(vm.module_total_bytes, CONFINED_MODULE_TOTAL_BYTES_LIMIT - 2);
        });
    }

    #[test]
    fn recursive_module_prescan_is_bounded_and_unwinds() {
        with_confined_vm(|vm, root| {
            for i in 0..=CONFINED_MODULE_RECURSION_LIMIT {
                let source = if i == CONFINED_MODULE_RECURSION_LIMIT {
                    String::new()
                } else {
                    format!("import './m{}.mjs';", i + 1)
                };
                std::fs::write(root.join(format!("m{i}.mjs")), source)
                    .expect("write chain fixture");
            }
            let entry = std::fs::canonicalize(root.join("m0.mjs")).expect("canonical entry");
            let mut seen = std::collections::HashSet::new();
            let err = vm
                .prescan_module_requests(&entry, &mut seen)
                .expect_err("deep request graph must fail closed");
            assert!(err.0.contains("module recursion limit"), "got {err:?}");
            assert_eq!(
                vm.module_load_depth, 0,
                "error path must unwind the counter"
            );
        });
    }
}
