// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

/// `ZIPP_NO_ENUM_HOIST=1` restores the per-dense-slot `array_index_override`
/// probe that `object_enum_own` used to pay. See the comment at its call site
/// for why hoisting it is sound; this exists purely so the change is A/B-able
/// and bisectable on one binary.
#[inline]
fn enum_hoist_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let v = std::env::var_os("ZIPP_NO_ENUM_HOIST").is_none() as u8;
            ON.store(v, Ordering::Relaxed);
            v == 1
        }
    }
}

impl<'p> Vm<'p> {
        pub(crate) fn object_enum_own(&mut self, obj: Value, what: EnumWhat) -> Result<Value, Thrown> {
        self.defer_check_all(obj)?;
        // A namespace with a still-uninitialized export throws from the per-key
        // [[GetOwnProperty]] walk (Object.keys/values/entries + for-in).
        self.ns_tdz_check_all(obj)?;
        // A Proxy enumerates via its ownKeys trap, keeping the STRING keys whose
        // [[GetOwnProperty]] (the gopd trap) reports enumerable.
        if let Some(keys) = self.proxy_own_keys(obj)? {
            let mut out: Vec<Value> = Vec::new();
            for k in keys {
                if !(k.is_heap() && self.heap.is_str_like(k.heap_index())) {
                    continue; // Object.keys/values/entries skip Symbol keys
                }
                let ks = self.display(k);
                let desc = match self.proxy_gopd(obj, &ks)? {
                    Some(d) => d,
                    None => Value::UNDEFINED,
                };
                if desc.is_undefined() {
                    continue;
                }
                let en = self.get_prop(desc, "enumerable")?;
                if !self.truthy(en) {
                    continue;
                }
                match what {
                    EnumWhat::Keys => out.push(k),
                    EnumWhat::Values => {
                        let v = self.get_member(obj, &ks, obj)?;
                        out.push(v);
                    }
                    EnumWhat::Entries => {
                        let v = self.get_member(obj, &ks, obj)?;
                        out.push(Value::heap(self.heap.alloc(HeapObj::Array(vec![k, v]))));
                    }
                }
            }
            return Ok(Value::heap(self.heap.alloc(HeapObj::Array(out))));
        }
        // A TypedArray enumerates its integer indices `0..length` (all enumerable),
        // then any enumerable named own prop. Handled before the generic match
        // because reading each element needs `&mut self` (ta_element_get).
        if obj.is_heap() && matches!(self.heap.get(obj.heap_index()), HeapObj::TypedArray { .. }) {
            let idx = obj.heap_index();
            let len = self.ta_len_kind(idx).0;
            let mut pairs: Vec<(String, Value)> = Vec::with_capacity(len);
            for i in 0..len {
                let v = self.ta_element_get(idx, i);
                pairs.push((i.to_string(), v));
            }
            if let Some(m) = self.arr_props.get(&idx) {
                for (i, k) in m.keys.iter().enumerate() {
                    if m.attrs[i].enumerable && !is_hidden_key(k) {
                        pairs.push((k.clone(), m.vals[i]));
                    }
                }
            }
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
            return Ok(Value::heap(self.heap.alloc(HeapObj::Array(out))));
        }
        // An Array enumerates its dense indices `0..length` (skipping any special
        // index defineProperty made non-enumerable), then its enumerable named own
        // props. Handled before the generic match because reading an accessor index
        // or a special value needs `&mut self` (get_index / get_member).
        if obj.is_heap() && matches!(self.heap.get(obj.heap_index()), HeapObj::Array(_)) {
            let idx = obj.heap_index();
            let len = match self.heap.get(idx) {
                HeapObj::Array(items) => items.len(),
                _ => 0,
            };
            // Does ANY overlay key name an index inside the dense range? Asked
            // ONCE, over the overlay's own keys, instead of once per dense slot.
            //
            // `array_index_override` is a `pos()` hash probe on the overlay map,
            // and the loop below used to pay one for every slot in `0..len`. A
            // sparse array makes that quadratic-feeling in the worst way: writing
            // `a[i] = v` on a stride extends the DENSE vector until it stops
            // growing (measured: 1,040,001 slots for an array with 5,000 keys,
            // only 105 of them dense — the rest went to the overlay). Enumerating
            // it then cost 1.04M hash probes to discover 105 elements: 25ms, and
            // node does the whole thing in 0ms. It is the entire fixed cost of a
            // sparse `for…in` / `Object.keys`, independent of key count.
            //
            // When no overlay key parses to an index below `len`, every one of
            // those probes was going to return `None` by construction — which is
            // the overwhelmingly common shape, since the overlay exists precisely
            // to hold the indices that sit ABOVE the dense prefix. A
            // `defineProperty` on a dense index is what puts one below it, and
            // that case keeps the old per-slot probe exactly.
            //
            // `ZIPP_NO_ENUM_HOIST=1` forces the old per-slot probe, so the change
            // can be A/B'd with `tools/bench.py --ab-env` on ONE binary and any
            // behaviour question bisected against it without a rebuild.
            let dense_overlaid = !enum_hoist_enabled()
                || self.arr_props.get(&idx).is_some_and(|m| {
                    m.has_element_key()
                        && m.keys.iter().any(|k| {
                            canonical_index_str(k).is_some_and(|n| n < len)
                        })
                });
            let mut ks: Vec<String> = Vec::new();
            for i in 0..len {
                let overridden =
                    if dense_overlaid { self.array_index_override(idx, i) } else { None };
                // A hole (an absent element) with no defineProperty'd override is not
                // an own property — skip it.
                if overridden.is_none()
                    && matches!(self.heap.get(idx), HeapObj::Array(items) if items[i].is_hole())
                {
                    continue;
                }
                if overridden.map_or(true, |(a, _)| a.enumerable) {
                    ks.push(i.to_string());
                }
            }
            if let Some(m) = self.arr_props.get(&idx) {
                // SPARSE-overlay index keys (>= the dense prefix) continue the
                // ascending index run BEFORE any named key — integer indices sort
                // first in spec own-key order. ("4294967295" and beyond are NOT
                // array indices; they stay named, in insertion order.)
                let mut sparse: Vec<usize> = Vec::new();
                for (j, k) in m.keys.iter().enumerate() {
                    if !m.attrs[j].enumerable || is_hidden_key(k) {
                        continue;
                    }
                    if let Some(n) = canonical_index_str(k).filter(|n| *n < 4_294_967_295) {
                        // An index key < the dense length is already covered by
                        // the dense range above.
                        if n >= len {
                            sparse.push(n);
                        }
                    }
                }
                sparse.sort_unstable();
                ks.extend(sparse.into_iter().map(|n| n.to_string()));
                for (j, k) in m.keys.iter().enumerate() {
                    if !m.attrs[j].enumerable || is_hidden_key(k) {
                        continue;
                    }
                    // Index keys were emitted (dense range / sparse run) above.
                    if canonical_index_str(k).is_some_and(|n| n < 4_294_967_295) {
                        continue;
                    }
                    ks.push(k.clone());
                }
            }
            let mut out: Vec<Value> = Vec::with_capacity(ks.len());
            for k in ks {
                if matches!(what, EnumWhat::Keys) {
                    let kv = self.alloc_str(k);
                    out.push(kv);
                    continue;
                }
                let v = match k.parse::<usize>() {
                    Ok(n) if n.to_string() == k.as_str() => {
                        self.get_index(obj, Value::num(n as f64))?
                    }
                    _ => self.get_member(obj, &k, obj)?,
                };
                match what {
                    EnumWhat::Values => out.push(v),
                    EnumWhat::Entries => {
                        let kv = self.alloc_str(k);
                        out.push(Value::heap(self.heap.alloc(HeapObj::Array(vec![kv, v]))));
                    }
                    EnumWhat::Keys => {}
                }
            }
            return Ok(Value::heap(self.heap.alloc(HeapObj::Array(out))));
        }
        // A String exotic (boxed `new String(s)` or a string primitive): its own
        // ENUMERABLE keys are the character indices `0..length` (the exotic chars;
        // `length` is non-enumerable so it is excluded), then any enumerable assigned
        // own prop on the wrapper. Handled before the generic match because reading a
        // character needs `&mut self` (get_index).
        if let Some((sval, len)) = self.string_exotic_chars(obj) {
            let mut out: Vec<Value> = Vec::with_capacity(len);
            for i in 0..len {
                let kv = self.alloc_str(i.to_string());
                match what {
                    EnumWhat::Keys => out.push(kv),
                    EnumWhat::Values => {
                        let ch = self.get_index(sval, Value::num(i as f64))?;
                        out.push(ch);
                    }
                    EnumWhat::Entries => {
                        let ch = self.get_index(sval, Value::num(i as f64))?;
                        out.push(Value::heap(self.heap.alloc(HeapObj::Array(vec![kv, ch]))));
                    }
                }
            }
            // Enumerable named own props assigned to the wrapper (`s.foo = …`).
            let extra: Vec<String> = match self.arr_props.get(&obj.heap_index()) {
                Some(m) => m
                    .keys
                    .iter()
                    .enumerate()
                    .filter(|(i, k)| m.attrs[*i].enumerable && !is_hidden_key(k))
                    .map(|(_, k)| k.clone())
                    .collect(),
                None => Vec::new(),
            };
            for k in extra {
                let v = self.get_member(obj, &k, obj)?;
                let kv = self.alloc_str(k);
                match what {
                    EnumWhat::Keys => out.push(kv),
                    EnumWhat::Values => out.push(v),
                    EnumWhat::Entries => {
                        out.push(Value::heap(self.heap.alloc(HeapObj::Array(vec![kv, v]))))
                    }
                }
            }
            return Ok(Value::heap(self.heap.alloc(HeapObj::Array(out))));
        }
        // A plain Object: EnumerableOwnPropertyNames — snapshot the ordered own string
        // keys ([[OwnPropertyKeys]]) ONCE, then per key re-read the LIVE descriptor (so
        // a key a prior getter deleted or made non-enumerable is skipped) and, for
        // values/entries, read the value via Get (an accessor's getter runs and its
        // mutations are observed; a thrown getter propagates). Object.keys reads no
        // value, so it never triggers a getter.
        if obj.is_heap() {
            if let HeapObj::Object(m) = self.heap.get(obj.heap_index()) {
                let mut names: Vec<String> = spec_key_order(&m.keys)
                    .into_iter()
                    .map(|i| m.keys[i].clone())
                    .filter(|k| !is_hidden_key(k))
                    .collect();
                // The global object's SCRIPT-declared var/function bindings are
                // enumerable slot-backed own properties (builtins stay
                // non-enumerable and excluded).
                let mut slot_names: std::collections::HashSet<String> =
                    std::collections::HashSet::new();
                if obj.heap_index() == self.global_this && self.global_this != 0 {
                    for (i, n) in self.program.global_names.iter().enumerate() {
                        let iu = i as u32;
                        if (self.program.hoisted_globals.contains(&iu)
                            || self.program.decl_globals.contains(&iu))
                            && !self.globals[i].is_uninitialized()
                            && m.pos(n).is_none()
                            && !names.iter().any(|k| k == n)
                        {
                            names.push(n.clone());
                            slot_names.insert(n.clone());
                        }
                    }
                }
                let mut out: Vec<Value> = Vec::with_capacity(names.len());
                for k in names {
                    let enumerable = slot_names.contains(&k)
                        || match self.heap.get(obj.heap_index()) {
                            HeapObj::Object(m) => {
                                m.pos(&k).map_or(false, |i| m.attrs[i].enumerable)
                            }
                            _ => false,
                        };
                    if !enumerable {
                        continue;
                    }
                    match what {
                        EnumWhat::Keys => {
                            let kv = self.alloc_str(k);
                            out.push(kv);
                        }
                        EnumWhat::Values => {
                            let v = self.get_member(obj, &k, obj)?;
                            out.push(v);
                        }
                        EnumWhat::Entries => {
                            let v = self.get_member(obj, &k, obj)?;
                            let kv = self.alloc_str(k);
                            out.push(Value::heap(self.heap.alloc(HeapObj::Array(vec![kv, v]))));
                        }
                    }
                }
                return Ok(Value::heap(self.heap.alloc(HeapObj::Array(out))));
            }
        }
        // A callable (function / closure / bound / native): enumerate its own keys in
        // canonical [[OwnPropertyKeys]] order — length/name/prototype keep their
        // chronological-first slot even after a defineProperty override (see
        // object_own_property_names) — keeping only the enumerable ones. An intrinsic
        // length/name/prototype is non-enumerable unless a fn_props override made it
        // enumerable, so only fn_props-backed keys can appear; their order follows the
        // canonical key order, NOT raw fn_props insertion order.
        if obj.is_heap()
            && matches!(
                self.heap.get(obj.heap_index()),
                HeapObj::Func(_)
                    | HeapObj::Closure { .. }
                    | HeapObj::Bound { .. }
                    | HeapObj::Native(_) | HeapObj::NativeClosure { .. }
            )
        {
            let idx = obj.heap_index();
            let names_v = self.object_own_property_names(obj)?;
            let names: Vec<String> = self
                .array_snapshot(names_v.heap_index())
                .into_iter()
                .filter_map(|k| {
                    (k.is_heap() && self.heap.is_str_like(k.heap_index())).then(|| self.display(k))
                })
                .collect();
            let mut out: Vec<Value> = Vec::new();
            for k in names {
                let enumerable = self
                    .fn_props
                    .get(&idx)
                    .and_then(|m| m.pos(&k).map(|i| m.attrs[i].enumerable))
                    .unwrap_or(false);
                if !enumerable {
                    continue;
                }
                match what {
                    EnumWhat::Keys => {
                        let kv = self.alloc_str(k);
                        out.push(kv);
                    }
                    EnumWhat::Values => {
                        let v = self.get_member(obj, &k, obj)?;
                        out.push(v);
                    }
                    EnumWhat::Entries => {
                        let v = self.get_member(obj, &k, obj)?;
                        let kv = self.alloc_str(k);
                        out.push(Value::heap(self.heap.alloc(HeapObj::Array(vec![kv, v]))));
                    }
                }
            }
            return Ok(Value::heap(self.heap.alloc(HeapObj::Array(out))));
        }
        // Every REMAINING exotic (Map, Set, Date, Promise, ArrayBuffer, DataView, a
        // generator, a boxed Number/Boolean/Symbol wrapper, …) has no exotic own
        // properties of its own: the only ones it can have are those ASSIGNED to it,
        // and every such kind keeps them in the generic arr_props side table — the
        // store getOwnPropertyNames/getOwnPropertyDescriptor already read. Without
        // this the fallback below reported none, so `var m = new Map(); m.x = 1`
        // was invisible to Object.keys/values/entries, for-in and JSON.stringify
        // while `Object.getOwnPropertyNames(m)` and `m.hasOwnProperty("x")` both
        // saw it. Values go through Get so a defineProperty'd getter runs.
        // (A Class keeps its statics in ClassData, so it stays on the tail path.)
        if obj.is_heap() && !matches!(self.heap.get(obj.heap_index()), HeapObj::Class(_)) {
            let names: Vec<String> = match self.arr_props.get(&obj.heap_index()) {
                Some(m) => spec_key_order(&m.keys)
                    .into_iter()
                    .filter(|&i| m.attrs[i].enumerable && !is_hidden_key(&m.keys[i]))
                    .map(|i| m.keys[i].clone())
                    .collect(),
                None => Vec::new(),
            };
            let mut out: Vec<Value> = Vec::with_capacity(names.len());
            for k in names {
                match what {
                    EnumWhat::Keys => {
                        let kv = self.alloc_str(k);
                        out.push(kv);
                    }
                    EnumWhat::Values => {
                        let v = self.get_member(obj, &k, obj)?;
                        out.push(v);
                    }
                    EnumWhat::Entries => {
                        let v = self.get_member(obj, &k, obj)?;
                        let kv = self.alloc_str(k);
                        out.push(Value::heap(self.heap.alloc(HeapObj::Array(vec![kv, v]))));
                    }
                }
            }
            return Ok(Value::heap(self.heap.alloc(HeapObj::Array(out))));
        }
        let pairs: Vec<(String, Value)> = if obj.is_heap() {
            match self.heap.get(obj.heap_index()) {
                HeapObj::Array(items) => {
                    let mut v: Vec<(String, Value)> =
                        items.iter().enumerate().map(|(i, x)| (i.to_string(), *x)).collect();
                    // Enumerable named own properties (arr.foo / match-result fields).
                    if let Some(m) = self.arr_props.get(&obj.heap_index()) {
                        for (i, k) in m.keys.iter().enumerate() {
                            if m.attrs[i].enumerable && !is_hidden_key(k) {
                                v.push((k.clone(), m.vals[i]));
                            }
                        }
                    }
                    v
                }
                // A function's assigned own properties live in the `fn_props` side
                // table (e.g. `fn.x = 1`); enumerate the enumerable ones (for
                // Object.keys/values/entries + for-in), like the getOwnPropertyNames
                // path already reads them.
                HeapObj::Func(_)
                | HeapObj::Closure { .. }
                | HeapObj::Bound { .. }
                | HeapObj::Native(_) | HeapObj::NativeClosure { .. } => match self.fn_props.get(&obj.heap_index()) {
                    Some(m) => spec_key_order(&m.keys)
                        .into_iter()
                        .filter(|&i| m.attrs[i].enumerable && !is_hidden_key(&m.keys[i]))
                        .map(|i| (m.keys[i].clone(), m.vals[i]))
                        .collect(),
                    None => Vec::new(),
                },
                // A class's own (static) properties live in `ClassData.statics`
                // (static methods are non-enumerable; static fields / `Cls.s = …`
                // assignments are enumerable).
                HeapObj::Class(c) => spec_key_order(&c.statics.keys)
                    .into_iter()
                    .filter(|&i| {
                        c.statics.attrs[i].enumerable
                            && !is_hidden_key(&c.statics.keys[i])
                            && !c.statics.keys[i].starts_with('#')
                    })
                    .map(|i| (c.statics.keys[i].clone(), c.statics.vals[i]))
                    .collect(),
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
        Ok(Value::heap(self.heap.alloc(HeapObj::Array(out))))
    }

    /// EnumerateObjectProperties (for-in): the own + INHERITED enumerable string
    /// keys, walking the [[Prototype]] chain, with shadowing dedup — a key seen at a
    /// nearer level (enumerable OR not) hides the same key on farther prototypes.
    /// Symbol keys are excluded; built-in prototype methods are non-enumerable so
    /// they never appear. Returns a HeapObj::Array of string keys. (Object.keys/
    /// values/entries stay own-only via object_enum_own — only for-in walks up.)
    /// Is `idx` a prototype level that a `for-in` can stop at outright?
    ///
    /// True only when the level is a plain object, every own key is
    /// non-enumerable AND hidden-key-free bookkeeping is unnecessary because the
    /// level TERMINATES the chain (its own prototype is null). Both halves
    /// matter: a level with non-enumerable keys still SHADOWS the same names on
    /// farther prototypes, so stopping early is only sound when there is no
    /// farther prototype.
    ///
    /// Memoised against the heap version, so adding an enumerable property to
    /// `Object.prototype` — which `for-in` must then observe — invalidates it.
    fn for_in_level_barren(&mut self, idx: u32) -> bool {
        let ver = self.heap.version_of(idx);
        if self.for_in_barren.get(&idx) == Some(&ver) {
            return true;
        }
        // Must terminate the chain: an explicit proto entry means more levels.
        match self.proto_of.get(&idx) {
            Some(p) if p.is_heap() => return false,
            Some(_) => {}
            // No entry means the DEFAULT proto, which for a non-%Object.prototype%
            // object is %Object.prototype% itself — i.e. more chain.
            None if idx != self.obj_proto => return false,
            None => {}
        }
        // The exotic carriers keep the general path.
        if (idx == self.global_this && self.global_this != 0)
            || self.module_namespaces.contains_key(&idx)
            || !self.deferred_ns_state.is_empty()
        {
            return false;
        }
        let barren = match self.heap.get(idx) {
            HeapObj::Object(m) => (0..m.len()).all(|i| !m.attr_at(i).enumerable),
            _ => false,
        };
        if barren {
            self.for_in_barren.insert(idx, ver);
        }
        barren
    }

    pub(crate) fn for_in_keys(&mut self, obj: Value) -> Result<Value, Thrown> {
        // `out` holds heap key strings while object_enum_own / object_own_property_names
        // re-enter and allocate — suspend GC for the scope.
        let _gc = self.gc_lock_guard();
        let mut out: Vec<Value> = Vec::new();
        // The shadow set (a nearer level's own key — enumerable or not — hides
        // the same name on farther prototypes) is built LAZILY: the dominant
        // chain shape (own keys, then prototypes whose properties are all
        // non-enumerable) never materialises it. `plain_levels` records the
        // fast levels already walked so the set can be reconstructed exactly
        // when a farther level first wants to emit (or goes exotic).
        let mut seen: Option<std::collections::HashSet<String>> = None;
        let mut plain_levels: Vec<u32> = Vec::new();
        let mut cur = obj;
        for _ in 0..100_000 {
            if !cur.is_heap() {
                break;
            }
            let idx = cur.heap_index();
            // FAST level: an ordinary plain object, whose own keys live in its
            // ObjMap alone — emit straight from the map, one key-string alloc
            // per YIELDED key. Exotic own-key carriers (Proxy / Array /
            // TypedArray / boxed String / Class statics) are other HeapObj
            // variants; the slot-backed global and module namespaces (live
            // bindings, TDZ checks) take the generic path below.
            let plain = matches!(self.heap.get(idx), HeapObj::Object(_))
                && !(idx == self.global_this && self.global_this != 0)
                && !(!self.module_namespaces.is_empty() && self.module_namespaces.contains_key(&idx))
                && !(!self.deferred_ns_state.is_empty() && self.deferred_ns_state.contains_key(&idx));
            if plain {
                // Slots to yield, in spec own-key order, while the map is borrowed.
                let (emit, visible) = match self.heap.get(idx) {
                    HeapObj::Object(m) => {
                        let mut emit: Vec<usize> = Vec::new();
                        let mut visible = false;
                        for i in spec_key_order(&m.keys) {
                            if is_hidden_key(&m.keys[i]) {
                                continue;
                            }
                            visible = true;
                            if m.attrs[i].enumerable {
                                emit.push(i);
                            }
                        }
                        (emit, visible)
                    }
                    _ => (Vec::new(), false),
                };
                if !emit.is_empty() && seen.is_none() && !plain_levels.is_empty() {
                    seen = Some(self.shadow_set_of(&plain_levels));
                }
                for i in emit {
                    let k = match self.heap.get(idx) {
                        HeapObj::Object(m) => match m.keys.get(i) {
                            Some(k) => k.clone(),
                            None => continue,
                        },
                        _ => continue,
                    };
                    if let Some(s) = &mut seen {
                        if !s.insert(k.clone()) {
                            continue;
                        }
                    }
                    let kv = self.alloc_str(k);
                    out.push(kv);
                }
                // With the shadow set live, this level's NON-emitted own keys
                // must still hide farther same-named keys; without it, the
                // level is recorded for a later lazy build instead.
                match &mut seen {
                    Some(s) if visible => {
                        let ks: Vec<String> = match self.heap.get(idx) {
                            HeapObj::Object(m) => m
                                .keys
                                .iter()
                                .filter(|k| !is_hidden_key(k))
                                .cloned()
                                .collect(),
                            _ => Vec::new(),
                        };
                        s.extend(ks);
                    }
                    Some(_) => {}
                    None => plain_levels.push(idx),
                }
                cur = self.object_get_prototype_of(cur);
                if cur == Value::NULL {
                    break;
                }
                // A terminal, wholly non-enumerable prototype contributes no
                // keys and can shadow nothing (there is nothing beyond it), so
                // the walk can stop. `%Object.prototype%` is that level for
                // almost every object, and re-deriving it per `for-in` was the
                // bulk of the ~151ns fixed cost.
                if cur.is_heap() && self.for_in_level_barren(cur.heap_index()) {
                    break;
                }
                continue;
            }
            // GENERIC level: the trap- and exotic-aware walk (object_enum_own
            // is Proxy-aware: ownKeys trap + per-key gopd check). Shadow
            // bookkeeping becomes eager from here on.
            if seen.is_none() {
                seen = Some(self.shadow_set_of(&plain_levels));
            }
            let enum_keys = self.object_enum_own(cur, EnumWhat::Keys)?;
            let enum_snap = self.array_snapshot(enum_keys.heap_index());
            for k in &enum_snap {
                let ks = self.display(*k);
                if seen.as_mut().is_some_and(|s| s.insert(ks)) {
                    out.push(*k);
                }
            }
            // Record EVERY own string key at this level (incl. non-enumerable) so it
            // shadows the same key on farther prototypes.
            let all_names = self.object_own_property_names(cur)?;
            let all_snap = self.array_snapshot(all_names.heap_index());
            for k in &all_snap {
                let ks = self.display(*k);
                if let Some(s) = &mut seen {
                    s.insert(ks);
                }
            }
            cur = self.object_get_prototype_of(cur);
            if cur == Value::NULL {
                break;
            }
        }
        Ok(Value::heap(self.heap.alloc(HeapObj::Array(out))))
    }

    /// Reconstruct the for-in shadow set from already-walked PLAIN levels:
    /// every visible own key (enumerable or not) of each recorded ObjMap.
    pub(crate) fn shadow_set_of(&self, plain_levels: &[u32]) -> std::collections::HashSet<String> {
        let mut s = std::collections::HashSet::new();
        for &pl in plain_levels {
            if let HeapObj::Object(m) = self.heap.get(pl) {
                for k in &m.keys {
                    if !is_hidden_key(k) {
                        s.insert(k.clone());
                    }
                }
            }
        }
        s
    }

    /// Build a data property descriptor object `{value, writable, enumerable,
    /// configurable}` (for `Object.getOwnPropertyDescriptor`).
    pub(crate) fn make_data_descriptor(&mut self, value: Value, w: bool, e: bool, c: bool) -> Value {
        let mut m = ObjMap::new();
        m.set("value", value);
        m.set("writable", Value::bool(w));
        m.set("enumerable", Value::bool(e));
        m.set("configurable", Value::bool(c));
        Value::heap(self.heap.alloc(HeapObj::Object(Box::new(m))))
    }

    /// Build an accessor descriptor object `{get, set, enumerable, configurable}`.
    pub(crate) fn make_accessor_descriptor(&mut self, get: Value, set: Value, e: bool, c: bool) -> Value {
        let mut m = ObjMap::new();
        m.set("get", get);
        m.set("set", set);
        m.set("enumerable", Value::bool(e));
        m.set("configurable", Value::bool(c));
        Value::heap(self.heap.alloc(HeapObj::Object(Box::new(m))))
    }

}
