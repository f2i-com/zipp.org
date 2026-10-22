// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

impl<'p> Vm<'p> {
    /// IsExtensible(O): a Proxy delegates to its trap; an ordinary object reads its
    /// `extensible` flag; an exotic object reads the arr_props side-table flag
    /// (default extensible). Used by the Proxy trap-invariant checks.
    pub(crate) fn is_extensible(&mut self, obj: Value) -> Result<bool, Thrown> {
        if let Some(b) = self.proxy_is_extensible(obj)? {
            return Ok(b);
        }
        if !obj.is_heap() {
            return Ok(false);
        }
        Ok(match self.heap.get(obj.heap_index()) {
            HeapObj::Object(m) => m.extensible,
            _ => self
                .arr_props
                .get(&obj.heap_index())
                .map_or(true, |m| m.extensible),
        })
    }

    /// The per-index override for an array element, if `defineProperty` gave index
    /// `i` non-default attributes or an accessor. Stored in the `arr_props` side
    /// table keyed by the canonical decimal string; when present it is authoritative
    /// for that index (the dense slot is a placeholder kept only so `length` counts
    /// it). Returns `(attrs, stored)` — `stored` is the value, or the getter for an
    /// accessor. Cheap miss: arrays with no side table return `None` after one
    /// slot-table probe, and one whose table names no element (a RegExp match
    /// result's `index`/`input`/`groups`) returns before spelling the key and
    /// scanning for it.
    pub(crate) fn array_index_override(&self, arr_idx: u32, i: usize) -> Option<(PropAttr, Value)> {
        let m = self.arr_props.get(&arr_idx)?;
        if !m.has_element_key() {
            return None;
        }
        let p = m.element_pos(i)?;
        Some((m.attr_at(p), m.val_at(p)))
    }

    /// Can anything outside the dense `Vec` shadow, hide, or constrain an ELEMENT
    /// (or `length`) of the array at `idx`? The precise form of the
    /// `arr_props.contains_key(&idx)` test that the dense fast paths grew up on.
    ///
    /// A RegExp match result answers `false` here and `true` there: it always
    /// carries a side table, because that is where `index`/`input`/`groups` live,
    /// but none of those four names can shadow an element. Every `exec` result in
    /// the program was falling off `map`/`filter`/`indexOf`/`slice`/`for-of`/
    /// `JSON.stringify` and — worst — deopting every JIT'd `m[i]`, for metadata
    /// that has nothing to do with its elements.
    ///
    /// Conservative direction is `true`. Callers that also care about NAMED
    /// properties must keep asking [`Self::arr_props`] directly.
    #[inline]
    pub(crate) fn array_elements_overlaid(&self, idx: u32) -> bool {
        self.arr_props
            .get(&idx)
            .is_some_and(|m| m.overlays_elements())
    }

    /// The JS `length` of the array at `arr_idx`: the dense element count, unless
    /// the array is SPARSE — its virtual length (always larger) is then recorded
    /// in the `array_js_len` side table. Result is `0..=2^32-1`.
    pub(crate) fn js_array_len(&self, arr_idx: u32) -> usize {
        if let Some(&n) = self.array_js_len.get(&arr_idx) {
            return n as usize;
        }
        match self.heap.get(arr_idx) {
            HeapObj::Array(items) => items.len(),
            _ => 0,
        }
    }

    /// Record that array index `i` now exists as an own property (a sparse write
    /// past the dense prefix): grow the virtual JS length to `i + 1` when that
    /// exceeds the current length. `i` is a valid array index (< 2^32-1), so the
    /// new length fits u32.
    pub(crate) fn array_grow_js_len(&mut self, arr_idx: u32, i: usize) {
        if i + 1 > self.js_array_len(arr_idx) {
            self.array_js_len.insert(arr_idx, (i + 1) as u32);
        }
    }

    /// Whether a NEW element at index `i`, at or past the dense length
    /// `dense_len`, goes to the sparse overlay rather than the dense Vec: past
    /// the eager-materialization cap, or more than
    /// [`crate::vm::MAX_DENSE_GAP`] holes past the dense end. Growing the Vec
    /// over a gap costs 8 bytes a hole, so 64 elements 64K apart
    /// (`a[e * 65536] = e`) used to build a 32 MB Vec of holes.
    #[inline]
    pub(crate) fn array_index_goes_sparse(i: usize, dense_len: usize) -> bool {
        i >= crate::vm::MAX_DENSE_ARRAY_LEN || i - dense_len > crate::vm::MAX_DENSE_GAP
    }

    /// [`Self::array_index_goes_sparse`] for a write about to add index `i` to
    /// the array at `arr_idx`, with one exception: once the elements would
    /// fill at least half of a Vec reaching `i`, the write grows the Vec
    /// instead, and [`Self::array_dense_grew`] brings the overlay in. An array
    /// keyed by ids that start high (`byId[5000 + k] = …`) would otherwise
    /// stay in the string-keyed overlay for good.
    pub(crate) fn array_new_index_sparse(&self, arr_idx: u32, i: usize, dense_len: usize) -> bool {
        if !Self::array_index_goes_sparse(i, dense_len) {
            return false;
        }
        if i >= crate::vm::MAX_DENSE_ARRAY_LEN {
            return true;
        }
        let overlay = self.arr_props.get(&arr_idx).map_or(0, |m| m.live_len());
        (dense_len + overlay + 1) * 2 <= i + 1
    }

    /// `set_index`'s store of a NEW index `i`, at or past the dense end of the
    /// (non-arguments) array at `arr_idx`. Past the eager-materialization cap,
    /// or too far past the dense end ([`Self::array_new_index_sparse`]), the
    /// element goes to the SPARSE overlay: arr_props under its canonical index
    /// key (default data attributes), the JS length to the virtual-length
    /// side table — the Vec is never resized over a long run of holes.
    /// Otherwise the Vec grows to take it. Out of line so the in-range store
    /// it splits from stays small.
    #[inline(never)]
    pub(crate) fn array_index_grow(
        &mut self,
        arr_idx: u32,
        i: usize,
        val: Value,
    ) -> Result<(), Thrown> {
        let dense_len = match self.heap.get(arr_idx) {
            HeapObj::Array(items) => items.len(),
            _ => return Ok(()),
        };
        if self.array_new_index_sparse(arr_idx, i, dense_len) {
            let mut buf = [0u8; 20];
            self.arr_props
                .entry(arr_idx)
                .or_insert_with(ObjMap::new_side_table)
                .set(crate::heap::index_key(&mut buf, i), val);
            self.array_grow_js_len(arr_idx, i);
            self.heap.bump_version(arr_idx);
            return Ok(());
        }
        if let HeapObj::Array(items) = self.heap.get_mut(arr_idx) {
            if i == items.len() {
                items.push(val);
            } else {
                // Slots between the old length and the new index are HOLES
                // (absent), not present undefineds.
                items.resize(i + 1, Value::HOLE);
                items[i] = val;
            }
        }
        self.array_dense_grew(arr_idx, dense_len);
        Ok(())
    }

    /// Bookkeeping after the dense Vec of the (non-arguments) array at
    /// `arr_idx` grew from `old_len` past its old end. Cheap unless the array
    /// is virtual or has element overrides.
    ///
    /// * Plain overlay elements within [`crate::vm::MAX_DENSE_GAP`] of the
    ///   Vec's end, and those continuing them within a gap of each other, move
    ///   into the Vec — where they would be had they been written in ascending
    ///   order. An array filled in reverse densifies
    ///   ([`Self::array_new_index_sparse`]) and gets its dense store and every
    ///   fast path back.
    ///
    ///   Finding them must not scan the overlay on every append. An overlay
    ///   element only ever enters that window as the Vec grows (a write inside
    ///   the window goes to the Vec), so a growth probes just the indices it
    ///   brought into the window — one, for an append — and only a hit, or a
    ///   jump of more than a gap (a densify, a `length` extension), pays the
    ///   full pass. A hit skips it when the overlay is too spread out for the
    ///   chain to reach its end: moving elements in one at a time costs a
    ///   table rebuild each, and while any stay past the end the array is
    ///   overlaid regardless.
    /// * A virtual length the Vec now covers is dropped: every element left in
    ///   the overlay lies below the Vec's end, over a HOLE placeholder, which
    ///   is the representation of a defineProperty'd element.
    pub(crate) fn array_dense_grew(&mut self, arr_idx: u32, old_len: usize) {
        // An ordinary array — no virtual length, no side table — has nothing
        // to settle.
        if self.array_js_len.is_empty() && !self.arr_props.contains_key(&arr_idx) {
            return;
        }
        let dense_len = match self.heap.get(arr_idx) {
            HeapObj::Array(items) if !self.arguments_objs.contains_key(&arr_idx) => items.len(),
            _ => return,
        };
        let gap = crate::vm::MAX_DENSE_GAP;
        let plain = |a: PropAttr| !a.accessor && a.writable && a.enumerable && a.configurable;
        let end = match self.arr_props.get(&arr_idx) {
            Some(m) if m.has_element_key() && m.extensible && !m.sealed && !m.frozen => {
                let jump = dense_len.saturating_sub(old_len) > gap + 1;
                let entered = || {
                    (old_len + gap + 1..=dense_len + gap)
                        .any(|k| m.element_pos(k).is_some_and(|p| plain(m.attr_at(p))))
                };
                let reachable = || {
                    self.js_array_len(arr_idx).saturating_sub(dense_len)
                        <= m.live_len().saturating_mul(gap + 1)
                };
                if !(jump || (entered() && reachable())) {
                    None
                } else {
                    // Where the Vec would end had the plain elements past it been
                    // written in ascending order.
                    let mut past: Vec<usize> = m
                        .iter()
                        .filter_map(|(k, _, a)| {
                            canonical_index_str(k).filter(|n| *n >= dense_len && plain(a))
                        })
                        .collect();
                    past.sort_unstable();
                    let mut end = dense_len;
                    for n in past {
                        if Self::array_index_goes_sparse(n, end) {
                            break;
                        }
                        end = n + 1;
                    }
                    Some(end)
                }
            }
            _ => None,
        };
        if let Some(end) = end {
            let moved = match self.arr_props.get_mut(&arr_idx) {
                Some(m) => m.remove_elements_where(|n, a| n < end && plain(a)),
                None => Vec::new(),
            };
            if !moved.is_empty() {
                // Nursery barrier: the side table is rescanned as a root every
                // minor, an old array's Vec is not — the young values moving
                // across need the holder carded (the store that brought us here
                // was barriered for ITS value only).
                self.heap.write_barrier(arr_idx);
                if let HeapObj::Array(items) = self.heap.get_mut(arr_idx) {
                    if end > items.len() {
                        items.resize(end, Value::HOLE);
                    }
                    for (n, v, _) in moved {
                        items[n] = v;
                    }
                }
                self.heap.bump_version(arr_idx);
            }
        }
        if let Some(&n) = self.array_js_len.get(&arr_idx) {
            let dense_len = match self.heap.get(arr_idx) {
                HeapObj::Array(items) => items.len(),
                _ => return,
            };
            if n as usize <= dense_len {
                self.array_js_len.remove(&arr_idx);
            }
        }
        self.array_drop_empty_side_table(arr_idx);
    }

    /// Drop the array's side table once it holds nothing — no key, default
    /// integrity flags — so the dense fast paths that test for any side table
    /// at all (the JIT's) take the array again. Not while a virtual length is
    /// left: those same tests are what keep an append from running past it.
    pub(crate) fn array_drop_empty_side_table(&mut self, arr_idx: u32) {
        let empty = self
            .arr_props
            .get(&arr_idx)
            .is_some_and(|m| m.is_empty() && m.extensible && !m.sealed && !m.frozen);
        if empty && !self.array_js_len.contains_key(&arr_idx) {
            self.arr_props.remove(&arr_idx);
        }
    }

    /// Whether Array `[[DefineOwnProperty]]` rejects a plain data write of index
    /// `i` on the array at `arr_idx` — the integrity-level half of `set_index`,
    /// shared so `OrdinarySet`'s boolean and the assignment path cannot drift
    /// (`Reflect.set` used to answer `true` for a write it correctly dropped).
    ///
    /// * FROZEN — every element is non-writable, so any index is rejected.
    /// * Non-extensible (sealed, frozen or `preventExtensions`) — an ABSENT
    ///   index (past the length, or a hole below it) would add an own property.
    /// * Non-writable `length` — an index at or past it would grow `length`.
    ///
    /// `false` for anything that is not a real array, and for an index carrying
    /// a `defineProperty` override (its own attributes govern, upstream).
    pub(crate) fn array_index_write_rejected(&self, arr_idx: u32, i: usize) -> bool {
        let HeapObj::Array(items) = self.heap.get(arr_idx) else {
            return false;
        };
        let present = i < items.len() && !items[i].is_hole();
        let m = self.arr_props.get(&arr_idx);
        if m.is_some_and(|m| m.frozen) {
            return true;
        }
        !present
            && (m.is_some_and(|m| !m.extensible)
                // Growing the JS length needs a writable `length`; an index below
                // a sparse array's VIRTUAL length doesn't grow it.
                || (self.array_length_nonwritable.contains(&arr_idx)
                    && i >= self.js_array_len(arr_idx)))
    }

    /// ArraySetLength truncation blocker: the highest NON-configurable own array
    /// index `>= new_len` (it survives the shrink and the final length becomes
    /// blocker + 1). A defineProperty'd override in `arr_props` can be
    /// non-configurable, so a scan of its keys covers those — including
    /// sparse-overlay indices far past the dense prefix (never walk the integer
    /// RANGE here: a virtual length can be 2^32-1). A SEALED (or frozen) array
    /// makes every element non-configurable through its flag alone, so there
    /// the highest present dense element blocks too.
    pub(crate) fn array_shrink_blocker(&self, arr_idx: u32, new_len: usize) -> Option<usize> {
        let m = self.arr_props.get(&arr_idx)?;
        let sealed = m.sealed || m.frozen;
        let keyed = m
            .slot_keys()
            .filter_map(|(i, k)| {
                // A spec array index is < 2^32-1: "4294967295"/"4294967296"
                // are ORDINARY named properties — they never block a shrink.
                canonical_index_str(k).filter(|ki| {
                    *ki < 4_294_967_295 && *ki >= new_len && (sealed || !m.attr_at(i).configurable)
                })
            })
            .max();
        if !sealed {
            return keyed;
        }
        let dense = match self.heap.get(arr_idx) {
            HeapObj::Array(items) if items.len() > new_len => items[new_len..]
                .iter()
                .rposition(|v| !v.is_hole())
                .map(|p| new_len + p),
            _ => None,
        };
        keyed.max(dense)
    }

    /// Apply a VALIDATED new `length` to a real array: drop the (configurable)
    /// arr_props index entries `>= n`, resize the dense store, and keep the
    /// virtual-length side table consistent (entry present iff the JS length
    /// exceeds the dense element count). The caller has already run the ToUint32
    /// coercion, the writability check, and resolved any non-configurable
    /// blocker into `n`.
    pub(crate) fn array_apply_length(&mut self, arr_idx: u32, n: usize) {
        if let Some(m) = self.arr_props.get_mut(&arr_idx) {
            // Only true array indices (< 2^32-1) are swept by truncation;
            // "4294967295"/"4294967296" are ordinary named props and stay. One
            // pass: a sparse array can shed tens of thousands of elements here.
            if m.has_element_key() {
                m.remove_elements_where(|i, _| i >= n);
            }
        }
        let dense_len = match self.heap.get(arr_idx) {
            HeapObj::Array(items) => items.len(),
            _ => 0,
        };
        // A SHRINK of a virtual array that stays more than a gap past its
        // dense prefix keeps the length virtual: materializing holes up to the
        // new length is the allocation the sparse overlay exists to avoid.
        let virtual_shrink =
            n > dense_len + crate::vm::MAX_DENSE_GAP && n < self.js_array_len(arr_idx);
        if n <= dense_len || (n <= crate::vm::MAX_DENSE_ARRAY_LEN && !virtual_shrink) {
            // Materialized: truncate, or extend with HOLES (absent elements).
            if let HeapObj::Array(items) = self.heap.get_mut(arr_idx) {
                items.resize(n, Value::HOLE);
            }
            self.array_js_len.remove(&arr_idx);
            // Overlay elements the extended Vec now covers move into it.
            if n > dense_len && self.arr_props.contains_key(&arr_idx) {
                self.array_dense_grew(arr_idx, dense_len);
            }
        } else {
            // Keep the dense prefix; the JS length lives in the side table.
            self.array_js_len.insert(arr_idx, n as u32);
        }
        self.array_drop_empty_side_table(arr_idx);
        // `bump_version` also advances B244's saturating Array-snapshot epoch,
        // including the side-table-only branch where the dense Vec did not
        // move. A post-cross-call snapshot must therefore recheck virtual len.
        self.heap.bump_version(arr_idx);
    }

    /// `Object.defineProperties(obj, props)` — define each own enumerable key of
    /// `props` as a descriptor on `obj`.
    pub(crate) fn object_define_properties(
        &mut self,
        obj: Value,
        props: Value,
    ) -> Result<(), Thrown> {
        // ObjectDefineProperties: props = ToObject(Properties) — null/undefined
        // throw (to_object boxes them like Object(), so guard first); other
        // primitives box (a String's index chars then fail ToPropertyDescriptor).
        self.require_object_coercible(props)?;
        let props = self.to_object(props)?;
        let pidx = props.heap_index();
        // ObjectDefineProperties step 3 is `props.[[OwnPropertyKeys]]()`, so the
        // walk is in OWN-KEY order — array indices ascending, then the string
        // keys in insertion order, then the symbol keys — NOT the raw insertion
        // order, in which zipp interleaves its `@@`-prefixed symbol keys with
        // the string ones. The difference is observable as the order of the
        // [[DefineOwnProperty]] calls on a Proxy target.
        let enum_keys = |m: &ObjMap| -> Vec<String> {
            let ordered = spec_key_order_map(&m);
            let live = |i: &usize| m.attr_at(*i).enumerable;
            ordered
                .iter()
                .filter(|i| live(i) && !is_hidden_key(m.key_at(**i)))
                .chain(
                    ordered
                        .iter()
                        .filter(|i| live(i) && is_hidden_key(m.key_at(**i))),
                )
                .map(|i| m.key_at(*i).to_owned())
                .collect()
        };
        // A PROXY descriptor bag follows the spec protocol exactly:
        // props.[[OwnPropertyKeys]]() (the ownKeys trap, or the target's
        // ordinary integer-first key order when absent), then per key
        // [[GetOwnProperty]] (the gopd trap — an undefined or non-enumerable
        // result skips the key), then Get + ToPropertyDescriptor, with ALL
        // descriptors collected/validated BEFORE any define lands on `obj`
        // (defineProperties/proxy-no-ownkeys-returned-keys-order).
        if self.proxy_parts(pidx).is_some() {
            // `keys`/`pending` hold Values across user trap calls — hold GC off.
            let _gc = self.gc_lock_guard();
            let keys_arr = self.object_own_keys(props)?;
            let keys = self.array_snapshot(keys_arr.heap_index());
            let mut pending: Vec<(String, Value)> = Vec::new();
            for kv in keys {
                let k = self.key_of(kv);
                let desc = self.proxy_gopd(props, &k)?.unwrap_or(Value::UNDEFINED);
                if desc == Value::UNDEFINED {
                    continue;
                }
                let en = self.get_prop(desc, "enumerable")?;
                if !self.truthy(en) {
                    continue;
                }
                let desc_obj = self.get_prop(props, &k)?;
                // ToPropertyDescriptor (validation), its getters run once.
                let record = self.descriptor_record(desc_obj)?;
                pending.push((k, record));
            }
            for (k, d) in pending {
                self.object_define_property(obj, &k, d)?;
            }
            return Ok(());
        }
        // OwnPropertyKeys(ToObject(props)) filtered to enumerable. The descriptor
        // bag may be any object — a function (own props in fn_props) or an exotic
        // object (arr_props) — not only a plain Object.
        let keys: Vec<String> = match self.heap.get(pidx) {
            HeapObj::Object(m) => enum_keys(m),
            HeapObj::Func(_)
            | HeapObj::Closure { .. }
            | HeapObj::Bound { .. }
            | HeapObj::Wrapped { .. }
            | HeapObj::Native(_)
            | HeapObj::NativeClosure { .. }
            | HeapObj::BoundResolver { .. }
            | HeapObj::CombinatorResolver { .. } => {
                self.fn_props.get(&pidx).map(enum_keys).unwrap_or_default()
            }
            // A boxed String (e.g. `Object.create(O, "abc")` → ToObject) exposes its
            // char indices as own enumerable keys; reading each yields a char string,
            // which then fails ToPropertyDescriptor (a non-object) → TypeError.
            HeapObj::Boxed { kind: 0, value } => {
                let v = *value;
                let n = self.heap.str_units(v.heap_index()).unwrap_or(0);
                let mut ks: Vec<String> = (0..n).map(|i| i.to_string()).collect();
                // Plus any own enumerable properties added to the String wrapper.
                if let Some(m) = self.arr_props.get(&pidx) {
                    ks.extend(enum_keys(m));
                }
                ks
            }
            _ => self.arr_props.get(&pidx).map(enum_keys).unwrap_or_default(),
        };
        // Same two-phase protocol as the Proxy branch: every descriptor is read
        // and validated (ToPropertyDescriptor) BEFORE any define lands, so a
        // later invalid or throwing descriptor leaves `obj` untouched. A key an
        // earlier getter deleted (or made non-enumerable) is skipped — the
        // spec re-reads [[GetOwnProperty]] per key.
        let _gc = self.gc_lock_guard(); // `pending` holds descriptor Values
        let mut pending: Vec<(String, Value)> = Vec::with_capacity(keys.len());
        for k in keys {
            if !self.own_enumerable_now(pidx, &k) {
                continue;
            }
            let desc = self.get_prop(props, &k)?;
            let record = self.descriptor_record(desc)?;
            pending.push((k, record));
        }
        for (k, desc) in pending {
            self.object_define_property(obj, &k, desc)?;
        }
        Ok(())
    }

    /// ToPropertyDescriptor(`desc`) — running its field getters exactly once —
    /// captured as a fresh plain object holding just the present fields, so
    /// the later [[DefineOwnProperty]] re-reads the record, not user code.
    fn descriptor_record(&mut self, desc: Value) -> Result<Value, Thrown> {
        let (value, get, set, wr, en, cf) = self.read_descriptor(desc)?;
        let mut m = ObjMap::new();
        if let Some(v) = value {
            m.set("value", v);
        }
        if let Some(w) = wr {
            m.set("writable", Value::bool(w));
        }
        if let Some(g) = get {
            m.set("get", g);
        }
        if let Some(s) = set {
            m.set("set", s);
        }
        if let Some(e) = en {
            m.set("enumerable", Value::bool(e));
        }
        if let Some(c) = cf {
            m.set("configurable", Value::bool(c));
        }
        Ok(Value::heap(self.heap.alloc(HeapObj::Object(Box::new(m)))))
    }

    /// Is `key` still an own ENUMERABLE property of the descriptor bag `pidx`
    /// (an ordinary object, a callable, a String wrapper or another exotic —
    /// the same stores `object_define_properties` collected its keys from)?
    fn own_enumerable_now(&self, pidx: u32, key: &str) -> bool {
        let side = |m: Option<&ObjMap>| {
            m.and_then(|m| m.pos(key).map(|i| m.attr_at(i).enumerable))
                .unwrap_or(false)
        };
        match self.heap.get(pidx) {
            HeapObj::Object(m) => side(Some(m)),
            HeapObj::Func(_)
            | HeapObj::Closure { .. }
            | HeapObj::Bound { .. }
            | HeapObj::Wrapped { .. }
            | HeapObj::Native(_)
            | HeapObj::NativeClosure { .. }
            | HeapObj::BoundResolver { .. }
            | HeapObj::CombinatorResolver { .. } => side(self.fn_props.get(&pidx)),
            HeapObj::Boxed { kind: 0, value } => {
                let n = self.heap.str_units(value.heap_index()).unwrap_or(0);
                canonical_index_str(key).is_some_and(|i| i < n) || side(self.arr_props.get(&pidx))
            }
            _ => side(self.arr_props.get(&pidx)),
        }
    }

    /// The `(name, length)` of a callable value (function, closure, or class) for
    /// its `.name`/`.length` properties — `None` for non-callables. A synthetic
    /// proto name (`<arrow>`, `<script>`, …) reads as the empty string (anonymous).
    /// `globalThis.<name>`: the value of the reserved global slot named `name`
    /// (or None if there is no such global). Backs property access on globalThis.
    /// True when `name` is a top-level LEXICAL binding (let/const/class) of
    /// the main script or a $262.evalScript: a realm binding readable as a
    /// bare identifier, but NOT a global-object property.
    pub(crate) fn global_name_is_lexical(&self, name: &str) -> bool {
        if self.program.lexical_globals.is_empty() && self.eval_lexical_globals.is_empty() {
            return false;
        }
        let slot = if let Some(i) = self.main_global_pos(name) {
            i as u32
        } else if let Some(&s) = self.eval_global_map.get(name) {
            s
        } else {
            return false;
        };
        self.program.lexical_globals.contains(&slot) || self.eval_lexical_globals.contains(&slot)
    }

    pub(crate) fn global_by_name(&self, name: &str) -> Option<Value> {
        // A built-in global removed via `delete globalThis.X` reads as absent
        // everywhere (get / has-own / descriptor all consult this).
        if self.deleted_globals.contains(name) {
            return None;
        }
        // A global LEXICAL is not a global-object property: invisible to
        // property reflection (gopd / hasOwnProperty / `globalThis.x` reads),
        // though the bare identifier still reads its slot. A PRE-EXISTING
        // builtin property of the same name (`let Array` shadowing %Array%)
        // remains an own property of the global object, so fall through to the
        // builtin table for those.
        if self.global_name_is_lexical(name) {
            if let Some(&idx) = self.builtin_globals.get(name) {
                return Some(Value::heap(idx));
            }
            return None;
        }
        if let Some(slot) = self.main_global_pos(name) {
            // A never-declared slot reads as "absent"; fall through to the
            // built-in table below rather than exposing the internal sentinel.
            match self.globals.get(slot).copied() {
                Some(v) if v.is_uninitialized() => {}
                Some(v) => return Some(v),
                None => {}
            }
        }
        // Bindings a LATER script introduced live in the eval slot map, not the
        // main program's compile-time name table. A prelude's (`run_with_prelude`)
        // are slot-backed, so without this lookup they are readable as bare
        // identifiers yet invisible to property reflection — and
        // `hasOwnProperty(globalThis, "$DONE")` is precisely the gate test262's
        // asyncHelpers.js uses to decide a test was run with the `async` flag, so
        // every async test failed with "asyncTest called without async flag".
        // An ordinary `$262.evalScript` binding is unaffected: its slot stays
        // UNINITIALIZED and is skipped here, because it is reflected by the real
        // own property that CreateGlobalVarBinding put on the global object.
        if let Some(&slot) = self.eval_global_map.get(name) {
            match self.globals.get(slot as usize).copied() {
                Some(v) if v.is_uninitialized() => {}
                Some(v) => return Some(v),
                None => {}
            }
        }
        // Standard built-in globals (Object, Array, Math, eval, parseInt, …) are
        // own properties of the global object even when the running program never
        // referenced them as a bare identifier (so no compiler slot was reserved).
        if let Some(&idx) = self.builtin_globals.get(name) {
            return Some(Value::heap(idx));
        }
        // The value-properties of the global object (non-heap globals).
        match name {
            "NaN" => Some(Value::num(f64::NAN)),
            "Infinity" => Some(Value::num(f64::INFINITY)),
            "undefined" => Some(Value::UNDEFINED),
            _ => None,
        }
    }

    /// Look up `key` on a built-in prototype object (`arr_proto`/`str_proto`),
    /// returning the method value (or undefined). Lets primitive array/string
    /// values expose their methods as first-class values.
    /// The effective prototype for an Array's method/inherited-index resolution: a
    /// `class extends Array` instance records its own (subclass) prototype in proto_of
    /// (which chains to %Array.prototype%); a plain array has no entry → %Array.prototype%;
    /// a NULL entry (`Object.setPrototypeOf(arr, null)`) → 0, nothing inherited
    /// (B289 — it used to fall back to %Array.prototype%, so `arr.push` still
    /// resolved after the prototype was removed).
    pub(crate) fn array_eff_proto(&self, idx: u32) -> u32 {
        match self.proto_of.get_copied(idx) {
            Some(p) if p.is_heap() => p.heap_index(),
            Some(_) => 0,
            None => self.arr_proto,
        }
    }

    /// Flag that `Array.prototype` / `Object.prototype` now carries an integer
    /// index, so array index assignment must consult the prototype chain (the
    /// `array_proto_has_index` perf guard, read by `set_index`).
    /// A module-namespace TDZ guard for FALLIBLE reflective entry points:
    /// reading the descriptor/value of an UNINITIALIZED export throws.
    pub(crate) fn ns_tdz_check(&self, obj: Value, key: &str) -> Result<(), Thrown> {
        if !obj.is_heap() {
            return Ok(());
        }
        if let Some(slot) = self
            .module_namespaces
            .get(&obj.heap_index())
            .and_then(|m| m.get(key))
            .copied()
        {
            let live = self
                .globals
                .get(slot as usize)
                .copied()
                .unwrap_or(Value::UNDEFINED);
            if live.is_uninitialized() {
                return Err(Thrown(format!(
                    "ReferenceError: Cannot access '{key}' before initialization"
                )));
            }
        }
        Ok(())
    }

    /// EnumerableOwnProperties / EnumerateObjectProperties over a namespace
    /// call [[GetOwnProperty]] per key — the FIRST uninitialized export (in
    /// sorted ownKeys order) throws.
    pub(crate) fn ns_tdz_check_all(&self, obj: Value) -> Result<(), Thrown> {
        if !obj.is_heap() {
            return Ok(());
        }
        if let Some(m) = self.module_namespaces.get(&obj.heap_index()) {
            let mut keys: Vec<&String> = m.keys().collect();
            crate::cold_sort::sort(&mut keys);
            for k in keys {
                let live = self
                    .globals
                    .get(m[k] as usize)
                    .copied()
                    .unwrap_or(Value::UNDEFINED);
                if live.is_uninitialized() {
                    return Err(Thrown(format!(
                        "ReferenceError: Cannot access '{k}' before initialization"
                    )));
                }
            }
        }
        Ok(())
    }

    /// The ONE way to invalidate the indexed-prototype protector (see the field's
    /// doc comment on `Vm`). Sticky — no caller ever restores validity.
    ///
    /// `#[inline]` and branchless-ish on purpose: the callers sit on
    /// `defineProperty`/`setPrototypeOf`, not on a hot path, but keeping the write
    /// unconditional means no caller can accidentally guard it wrongly.
    #[inline]
    pub(crate) fn invalidate_indexed_proto_protector(&mut self) {
        // Pinned HasProp snapshots now carry the protector's clear state. A
        // native cross-call may otherwise reuse an Array snapshot under B244's
        // coarse epoch after user code installs an indexed prototype property.
        // Dirties only the clear -> invalid transition; the protector is sticky.
        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
        if !self.array_proto_has_index {
            self.heap.dirty_array_snapshots();
        }
        self.array_proto_has_index = true;
    }

    /// True when `obj_idx` is one of the two prototypes that sit in EVERY plain
    /// array's chain, so a mutation to it is protector-relevant.
    #[inline]
    pub(crate) fn is_indexed_proto_anchor(&self, obj_idx: u32) -> bool {
        obj_idx == self.arr_proto || obj_idx == self.obj_proto
    }

    pub(crate) fn note_array_proto_index(&mut self, obj_idx: u32, key: &str) {
        if self.is_indexed_proto_anchor(obj_idx) && canonical_index_str(key).is_some() {
            self.invalidate_indexed_proto_protector();
            // %Array.prototype% is an Array exotic: an index definition on it
            // grows its own `length` (ArraySetLength step for index defines).
            // Only a true array index (< 2^32-1) does: "4294967295" and beyond
            // are ordinary named properties.
            if obj_idx == self.arr_proto {
                if let Some(i) = canonical_index_str(key).filter(|&i| i < 4_294_967_295) {
                    let want = (i as u64 + 1) as u32;
                    if want > self.arr_proto_len {
                        self.arr_proto_len = want;
                    }
                }
            }
        }
    }

    /// OrdinarySet's prototype step for an array index `i` absent as an own property:
    /// walk the array's prototype chain (incl. Object.prototype). A prototype own
    /// ACCESSOR at `i` → invoke its setter with `obj` as receiver, return Ok(true)
    /// (handled, no own property created). A prototype non-writable own DATA prop at
    /// `i` → reject (Ok(true)). Otherwise Ok(false) → the caller creates the own data
    /// property. Only called when `array_proto_has_index` is set.
    pub(crate) fn array_proto_set_step(
        &mut self,
        obj: Value,
        i: usize,
        val: Value,
        strict: bool,
    ) -> Result<bool, Thrown> {
        let key = i.to_string();
        let mut cur = match self.object_get_prototype_of(obj) {
            p if p.is_heap() => p.heap_index(),
            _ => return Ok(false),
        };
        let mut guard = 0u32;
        let mut saw_obj_proto = false;
        while cur != 0 && guard < 64 {
            guard += 1;
            saw_obj_proto |= cur == self.obj_proto;
            // A Proxy chain node: parent.[[Set]](P, V, Receiver) — its `set`
            // trap fires with the ORIGINAL receiver (call-parameters-
            // prototype-index); with no trap, [[Set]] forwards to the proxy's
            // target — continue the walk from there with the same receiver.
            if self.proxy_parts(cur).is_some() {
                match self.proxy_set_bool(Value::heap(cur), &key, val, obj)? {
                    Some(true) => return Ok(true),
                    Some(false) => {
                        self.reject_write(&key, strict)?;
                        return Ok(true);
                    }
                    None => {
                        cur = match self.proxy_parts(cur).map(|(t, _, _)| t) {
                            Some(t) if t.is_heap() => t.heap_index(),
                            _ => 0,
                        };
                        continue;
                    }
                }
            }
            // A TypedArray chain node absorbs an INVALID index silently (report
            // handled — no write, no coercion, no reject); a valid index is a
            // plain writable data prop, so the caller writes the own element.
            if matches!(self.heap.get(cur), HeapObj::TypedArray { .. }) {
                return Ok(self.ta_valid_index(cur, &key).is_none());
            }
            if let Some((attr, raw)) = self.own_member(cur, &key) {
                return self.apply_proto_set(attr, raw, obj, &key, val, strict);
            }
            cur = match self.proto_of.get_copied(cur) {
                Some(p) if p.is_heap() => p.heap_index(),
                _ => 0,
            };
        }
        // Type prototypes may not record proto_of -> Object.prototype explicitly.
        if !saw_obj_proto && self.obj_proto != 0 {
            if let Some((attr, raw)) = self.own_member(self.obj_proto, &key) {
                return self.apply_proto_set(attr, raw, obj, &key, val, strict);
            }
        }
        Ok(false)
    }

    pub(crate) fn apply_proto_set(
        &mut self,
        attr: PropAttr,
        _raw: Value,
        obj: Value,
        key: &str,
        val: Value,
        strict: bool,
    ) -> Result<bool, Thrown> {
        if attr.accessor {
            // For a SET, the setter is `attr.setter` (the `raw` value is the getter).
            if attr.setter == Value::UNDEFINED {
                self.reject_write(key, strict)?;
            } else {
                self.call_value(attr.setter, obj, &[val])?;
            }
            Ok(true)
        } else if !attr.writable {
            self.reject_write(key, strict)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

#[cfg(all(test, feature = "jit", target_arch = "x86_64"))]
mod array_snapshot_epoch_tests {
    use super::*;

    fn array_snapshot(vm: &mut Vm<'_>, idx: u32) -> crate::vm::TaSnap {
        let mut snap = crate::vm::TaSnap {
            obj_bits: u64::MAX,
            base: u64::MAX,
            len: u64::MAX,
            flags: u64::MAX,
        };
        crate::vm::jit_ta_snapshot(
            vm as *mut Vm<'_> as *mut core::ffi::c_void,
            Value::heap(idx).bits(),
            crate::codegen::ARR_INT_PIN_KIND as u32,
            &mut snap,
        );
        snap
    }

    #[test]
    fn side_table_only_virtual_length_changes_dirty_array_snapshots() {
        let source = "var ready = true;";
        let ast = crate::front::parse_script(source).expect("source parses");
        let program = crate::compile::compile_program(&ast, source).expect("source compiles");
        let mut vm = Vm::new(&program);
        let idx = vm.heap.alloc(HeapObj::Array(vec![
            Value::int(1),
            Value::int(2),
            Value::int(3),
            Value::int(4),
        ]));
        let dense = array_snapshot(&mut vm, idx);
        assert_eq!(dense.obj_bits, Value::heap(idx).bits());
        assert_ne!(dense.base, 0);
        assert_eq!(dense.len, 4);

        let before = vm.heap.array_snapshot_epoch;
        vm.array_apply_length(idx, crate::vm::MAX_DENSE_ARRAY_LEN + 1);
        assert_eq!(vm.js_array_len(idx), crate::vm::MAX_DENSE_ARRAY_LEN + 1);
        assert_eq!(
            vm.heap.array_snapshot_epoch,
            before + 1,
            "entering virtual-length storage must invalidate a cached dense snapshot"
        );
        assert!(matches!(vm.heap.get(idx), HeapObj::Array(items) if items.len() == 4));
        let declined = array_snapshot(&mut vm, idx);
        assert_eq!(
            (
                declined.obj_bits,
                declined.base,
                declined.len,
                declined.flags
            ),
            (0, 0, 0, 0),
            "runtime snapshot validation must not publish dense len for a virtual Array"
        );

        let before = vm.heap.array_snapshot_epoch;
        vm.array_apply_length(idx, crate::vm::MAX_DENSE_ARRAY_LEN + 9);
        assert_eq!(vm.heap.array_snapshot_epoch, before + 1);
        assert_eq!(
            vm.js_array_len(idx),
            crate::vm::MAX_DENSE_ARRAY_LEN + 9,
            "retaining virtual storage while changing length must invalidate too"
        );

        let before = vm.heap.array_snapshot_epoch;
        vm.array_apply_length(idx, 2);
        assert!(vm.heap.array_snapshot_epoch > before);
        assert_eq!(vm.js_array_len(idx), 2);
        assert!(!vm.array_js_len.contains_key(&idx));
    }
}

/// Where elements written past an array's end are STORED: the dense Vec, the
/// sparse overlay, and the virtual length. Semantics are compared with Node by
/// the scripts; these pin the representation, which no script can observe.
#[cfg(test)]
mod dense_gap_tests {
    use super::*;

    /// Run `src` and report, per global array name: (dense Vec length, overlay
    /// keys — `None` when the array has no side table at all, which is what
    /// the JIT's dense lanes require — and the virtual length if any).
    fn shapes(src: &str, names: &[&str]) -> Vec<(usize, Option<usize>, Option<u32>)> {
        let ast = crate::front::parse_script(src).expect("source parses");
        let program = crate::compile::compile_program(&ast, src).expect("source compiles");
        let mut vm = Vm::new(&program);
        vm.run().expect("program runs");
        names
            .iter()
            .map(|name| {
                let slot = program
                    .global_names
                    .iter()
                    .position(|candidate| candidate == name)
                    .unwrap_or_else(|| panic!("missing global slot for {name}"));
                let idx = vm.globals[slot].heap_index();
                let dense = match vm.heap.get(idx) {
                    HeapObj::Array(items) => items.len(),
                    _ => panic!("{name} is not an array"),
                };
                let overlay = vm.arr_props.get(&idx).map(|m| m.live_len());
                (dense, overlay, vm.array_js_len.get(&idx).copied())
            })
            .collect()
    }

    #[test]
    fn a_write_far_past_the_end_goes_to_the_overlay() {
        let got = shapes(
            "var a = []; for (var e = 0; e < 64; e++) a[e * 65536] = e;
             var b = []; b[0] = 0; b[1026] = 1;
             var c = []; c[0] = 0; c[1025] = 1;
             var d = []; d['5000'] = 1;
             var f = []; Object.defineProperty(f, 7000, {value: 1, writable: true, enumerable: true, configurable: true});",
            &["a", "b", "c", "d", "f"],
        );
        let far = Some(63 * 65536 + 1);
        assert_eq!(got[0], (1, Some(63), far), "64K gaps must not build a Vec of holes");
        assert_eq!(got[1], (1, Some(1), Some(1027)), "1025 holes is past the widest gap");
        assert_eq!(got[2], (1026, None, None), "1024 holes is the widest");
        assert_eq!(got[3], (0, Some(1), Some(5001)), "a string-keyed index write too");
        assert_eq!(got[4], (0, Some(1), Some(7001)), "so does a default-attribute define");
    }

    #[test]
    fn filling_back_in_returns_the_elements_to_the_vec() {
        let got = shapes(
            "var rev = []; for (var i = 4999; i >= 0; i--) rev[i] = i;
             var odd = []; for (var j = 9999; j >= 0; j -= 2) odd[j] = j;
             var ids = []; for (var k = 5000; k < 16000; k++) ids[k] = k;
             var up = []; up[3000] = 3; for (var m = 0; m < 2000; m++) up[m] = m;
             var ext = []; ext[0] = 0; ext[3000] = 3; ext.length = 4000;",
            &["rev", "odd", "ids", "up", "ext"],
        );
        assert_eq!(got[0], (5000, None, None), "a reverse fill ends dense, table dropped");
        assert_eq!(got[1], (10000, None, None), "strided gaps within the limit too");
        assert_eq!(got[2], (16000, None, None), "ids that start high densify once half full");
        assert_eq!(got[3], (3001, None, None), "the Vec within a gap of an element absorbs it");
        assert_eq!(got[4], (4000, None, None), "a length extension absorbs what it covers");
    }

    #[test]
    fn non_default_elements_stay_in_the_overlay_over_a_placeholder() {
        let got = shapes(
            "var a = []; a[0] = 0;
             Object.defineProperty(a, 2000, {get: function () { return 1; }, enumerable: true, configurable: true});
             a[3000] = 3;
             for (var i = 1; i < 2100; i++) if (i !== 2000) a[i] = i;
             var s = []; s[0] = 0; s[5000] = 5; Object.seal(s); s[5000] = 6;",
            &["a", "s"],
        );
        // The accessor keeps its overlay entry (over a HOLE in the Vec); the
        // plain far element moved in once the Vec reached within a gap of it.
        assert_eq!(got[0], (3001, Some(1), None));
        assert_eq!(got[1], (1, Some(1), Some(5001)), "a sealed array's elements never move");
    }

    #[test]
    fn truncation_and_virtual_shrink() {
        let got = shapes(
            "var a = []; for (var i = 0; i < 20000; i++) a[i * 2000] = i;
             a.length = 10;
             var v = []; v[0] = 0; v[9000] = 9; v.length = 5000;
             var w = []; w[0] = 0; w[9000] = 9; w.length = 5000; for (var k = 1; k < 5100; k++) w[k] = k;",
            &["a", "v", "w"],
        );
        assert_eq!(got[0], (10, None, None), "20,000 overlay elements shed in one pass");
        // The emptied side table stays while the length is virtual (it keeps
        // the JIT's append lane off the array), and goes with it.
        assert_eq!(got[1], (1, Some(0), Some(5000)), "a shrink a gap past the Vec stays virtual");
        assert_eq!(got[2], (5100, None, None), "the Vec reaching the virtual length drops it");
    }
}
