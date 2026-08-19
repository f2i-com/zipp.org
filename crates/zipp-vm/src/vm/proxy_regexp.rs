#![allow(unused_imports)]
use super::*;
use crate::bytecode::{InstanceCtor, Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PropAttr, PromiseState, ReactionPair, Reactions,
};
use crate::value::Value;

/// The prototype/constructor half of `regexp_matchall_fast_ok`, resolved to
/// SLOT INDICES (B68 item 2). The full gate re-found `flags`/`exec`/
/// `constructor`/`@@match` on the ~20-key %RegExp.prototype% plus `@@species`
/// on %RegExp% with hashed `pos()` scans on EVERY `matchAll()` call; the
/// slots cannot move without a version bump, so once resolved the warm
/// re-proof is version compares plus direct slot reads. (The five instance
/// probes stay uncached — in the pristine case they short-circuit behind a
/// single `arr_props` miss.)
///
/// Guarded exactly like [`super::async_runtime::PromisePristineSlots`]: the
/// heap's index-parallel `versions` array proves the slot indices still name
/// their keys (key add/delete, `defineProperty`, `Heap::replace` and GC slot
/// reuse all bump). What a version does NOT guard — a plain in-place
/// `vals[i] = v` data write bumps nothing (B67/B110) — is never trusted from
/// the cache: the accessor bit and the value identity at each slot are
/// re-read on every call, with each pinned native's own version standing in
/// for a `heap.get` (only `Heap::replace`/GC reuse can change a `Native`,
/// and both bump). On any mismatch the full gate re-runs and re-resolves:
/// conservative fallback, never a wrong answer.
#[derive(Clone, Copy)]
pub(crate) struct MatchallFastSlots {
    /// `heap.versions[regexp_proto]` / `heap.versions[regexp_ctor]` at fill.
    proto_version: u32,
    ctor_version: u32,
    /// `(slot, value heap index, value version)` for the four pinned
    /// intrinsics: `flags` (accessor) / `exec` / `@@match` on the prototype,
    /// `@@species` (accessor) on %RegExp%.
    flags: (u32, u32, u32),
    exec: (u32, u32, u32),
    matchsym: (u32, u32, u32),
    species: (u32, u32, u32),
    /// `constructor`'s slot — its target is the `regexp_ctor` anchor itself,
    /// re-compared by identity per call, so nothing else needs pinning.
    ctor_slot: u32,
}

/// `ZIPP_NO_FASTOK_MEMO=1` makes `regexp_matchall_fast_ok_cached` run the
/// original nine-probe gate on every call, bypassing the slot memo entirely —
/// the rollback switch and one side of a one-binary A/B (`tools/bench.py
/// --ab-env`). Same idiom as `ZIPP_NO_PROMISE_SLOT_CACHE`.
#[inline]
fn fastok_memo_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let v = std::env::var_os("ZIPP_NO_FASTOK_MEMO").is_none() as u8;
            ON.store(v, Ordering::Relaxed);
            v == 1
        }
    }
}

/// `ZIPP_NO_MATCHALL_STEP=1` disables the fused %RegExpStringIterator% STEP
/// (B118): every step runs the full observable protocol re-proof again — the
/// rollback switch and one side of a one-binary A/B, same idiom as
/// `ZIPP_NO_FASTOK_MEMO`.
#[inline]
fn matchall_step_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let v = std::env::var_os("ZIPP_NO_MATCHALL_STEP").is_none() as u8;
            ON.store(v, Ordering::Relaxed);
            v == 1
        }
    }
}

/// `ZIPP_NO_SLIM_EXEC=1` disables the B124 slim per-call exec: the fused
/// matchAll step goes back through the full `regexp_exec_impl_prebits`
/// protocol (duplicate lastIndex read + ToInteger, per-step flatten/is_ascii/
/// str_units heap.gets, per-step twin probe, result-array empty-match probe),
/// and the pristine exec's flag decode goes back to the four `contains`
/// scans. The rollback switch and one side of a one-binary A/B
/// (`tools/bench.py --ab-env`), same idiom as `ZIPP_NO_MATCHALL_STEP`.
#[inline]
fn slim_exec_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let v = std::env::var_os("ZIPP_NO_SLIM_EXEC").is_none() as u8;
            ON.store(v, Ordering::Relaxed);
            v == 1
        }
    }
}

/// Flag-bit layout of the `regexp_string_iters` record's `u8` (computed ONCE
/// at iterator creation): `global`/`fullUnicode` are what
/// CreateRegExpStringIterator captures per spec; the rest exist so the fused
/// step (B118) never re-derives them from the matcher's flags string.
/// `ITFB_FUSED` is only set by the pristine-clone creation arm, whose matcher
/// is ENGINE-INTERNAL (no user reference can ever exist), over a flat-ASCII
/// subject with a numeric `lastIndex`.
pub(crate) const ITFB_GLOBAL: u8 = 1 << 0;
pub(crate) const ITFB_UNICODE: u8 = 1 << 1;
pub(crate) const ITFB_FUSED: u8 = 1 << 2;
pub(crate) const ITFB_STICKY: u8 = 1 << 3;
pub(crate) const ITFB_INDICES: u8 = 1 << 4;

impl<'p> Vm<'p> {
    /// `new Proxy(target, handler)` — both must be objects.
    pub(crate) fn make_proxy(&mut self, target: Value, handler: Value) -> Result<Value, Thrown> {
        if !self.is_object_value(target) || !self.is_object_value(handler) {
            return Err(Thrown(
                "TypeError: Cannot create proxy with a non-object as target or handler".into(),
            ));
        }
        Ok(Value::heap(self.heap.alloc(HeapObj::Proxy { target, handler, revoked: false })))
    }

    pub(crate) fn proxy_parts(&self, idx: u32) -> Option<(Value, Value, bool)> {
        match self.heap.get(idx) {
            HeapObj::Proxy { target, handler, revoked } => Some((*target, *handler, *revoked)),
            _ => None,
        }
    }

    /// Reconstruct a property KEY as a Value (a Symbol for an `@@`-encoded key,
    /// else a string) — so a Proxy trap / Reflect receives the real key.
    pub(crate) fn key_to_value(&mut self, key: &str) -> Value {
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
    pub(crate) fn proxy_trap(&mut self, handler: Value, name: &str) -> Result<Option<Value>, Thrown> {
        let t = self.get_prop(handler, name)?;
        if t.is_undefined() || t.is_null() {
            Ok(None)
        } else if self.is_callable(t) {
            Ok(Some(t))
        } else {
            Err(Thrown(format!("TypeError: proxy handler's {name} trap is not a function")))
        }
    }

    pub(crate) fn set_regexp_last_index(&mut self, idx: u32, n: usize) {
        if let HeapObj::RegExp { last_index, .. } = self.heap.get_mut(idx) {
            *last_index = Value::num(n as f64);
        }
    }

    /// Whether a RegExp's struct-backed `lastIndex` is writable. A
    /// `defineProperty` records the cleared flag in `arr_props` — but so does
    /// `Object.freeze(re)`, which runs DefinePropertyOrThrow over every own key
    /// and `lastIndex` is the only one a RegExp has. Because the slot lives in
    /// the struct rather than in the side table, freeze left no per-key entry
    /// behind and the flag read as writable: a frozen global regex silently
    /// advanced `lastIndex` instead of throwing.
    pub(crate) fn regexp_last_index_writable(&self, idx: u32) -> bool {
        self.arr_props.get(&idx).map_or(true, |m| {
            !m.frozen && m.pos("lastIndex").map_or(true, |i| m.attrs[i].writable)
        })
    }

    /// True when `String.prototype.replace`'s internal regex fast path is
    /// UNOBSERVABLE for instance `re`: its [[Prototype]] is exactly
    /// %RegExp.prototype%, it has no own exec/flags/@@replace overrides, and
    /// the prototype's `exec` / `@@replace` are still the intrinsic natives.
    /// Anything else (a subclass instance, a patched prototype) must run the
    /// full observable @@replace protocol.
    pub(crate) fn regexp_replace_fast_ok(&self, re: u32) -> bool {
        match self.proto_of.get(&re) {
            None => {}
            Some(p) if p.is_heap() && p.heap_index() == self.regexp_proto => {}
            _ => return false,
        }
        if self.arr_props.get(&re).is_some_and(|m| {
            m.pos("exec").is_some() || m.pos("flags").is_some() || m.pos("@@replace").is_some()
        }) {
            return false;
        }
        match self.heap.get(self.regexp_proto) {
            HeapObj::Object(m) => {
                let intrinsic = |k: &str, id: u16| {
                    m.pos(k).is_some_and(|i| {
                        !m.attrs[i].accessor
                            && m.vals[i].is_heap()
                            && matches!(self.heap.get(m.vals[i].heap_index()),
                                        HeapObj::Native(n) if *n == id)
                    })
                };
                if !(intrinsic("exec", native::REGEXP_EXEC)
                    && intrinsic("@@replace", native::REGEXP_SYM_REPLACE))
                {
                    return false;
                }
            }
            _ => return false,
        }
        // The fast path starts matching at 0 and does not write `lastIndex`
        // back, which is only unobservable for a NON-sticky pattern whose
        // `lastIndex` is already 0. `@@replace` reads `lastIndex` for a sticky
        // regex and, when global, sets it to 0 before matching and leaves it
        // there — `"aaaa".replace(/a/g, "b")` must end with `lastIndex === 0`,
        // and a sticky `re` with `lastIndex === 5` must resume at 5.
        let (flags, last_index) = match self.heap.get(re) {
            HeapObj::RegExp { flags, last_index, .. } => (flags.clone(), *last_index),
            _ => return false,
        };
        if flags.contains('y') {
            return false;
        }
        if !(last_index.is_number() && last_index.as_f64() == 0.0) {
            return false;
        }
        // `@@replace` step 8.b is `Set(rx, "lastIndex", 0, true)` for a GLOBAL
        // regex. The fast path skips it because `lastIndex` is already 0 — which
        // is unobservable only while the property is writable; on a frozen regex
        // that Set is a TypeError, and the fast path swallowed it.
        if flags.contains('g') && !self.regexp_last_index_writable(re) {
            return false;
        }
        // `@@replace` also reads `global` and `unicode` off the INSTANCE, so a
        // patched accessor on the prototype is observable.
        match self.heap.get(self.regexp_proto) {
            HeapObj::Object(m) => ["global", "unicode", "flags", "source"].iter().all(|k| {
                m.pos(k).is_none_or(|i| {
                    let a = m.attrs[i];
                    a.accessor
                        && m.vals[i].is_heap()
                        && matches!(self.heap.get(m.vals[i].heap_index()), HeapObj::Native(_))
                })
            }),
            _ => false,
        }
    }

    /// UNOBSERVABLE to build `@@matchAll`'s matcher by direct clone for instance
    /// `re`: [[Prototype]] is exactly %RegExp.prototype%, no own
    /// `flags`/`constructor`/`lastIndex`-shadowing overrides, the prototype's
    /// `flags` accessor and `exec` are still intrinsic, `constructor` is still
    /// the %RegExp% intrinsic, and `RegExp[@@species]` is still the default
    /// accessor.
    ///
    /// The spec path is expensive because it is fully observable: Get(R,"flags")
    /// through the accessor, ToString it, Get(R,"constructor"), Get(C,"@@species"),
    /// then Construct(C, «R, flags») — which reparses/relooks-up the pattern —
    /// plus Get(R,"lastIndex"). That is ~6 property lookups, 3 string allocations
    /// and a full RegExp construction, measured at 1.2us per `matchAll` call
    /// before a single match is attempted (node: 47ns). When every one of those
    /// steps is guaranteed to return the intrinsic, the whole sequence is
    /// equivalent to cloning the compiled regex.
    pub(crate) fn regexp_matchall_fast_ok(&self, re: u32) -> bool {
        match self.proto_of.get(&re) {
            None => {}
            Some(p) if p.is_heap() && p.heap_index() == self.regexp_proto => {}
            _ => return false,
        }
        if self.arr_props.get(&re).is_some_and(|m| {
            m.pos("flags").is_some()
                || m.pos("constructor").is_some()
                || m.pos("exec").is_some()
                || m.pos("@@matchAll").is_some()
                // `@@match` is observable from this path even though the clone
                // never matches with it: the spec builds the matcher via
                // Construct(C, «R, flags»), and the RegExp constructor's step 1
                // is IsRegExp(pattern) — a Get of `@@match`. Cloning skips the
                // construction and so skips that Get.
                || m.pos("@@match").is_some()
        }) {
            return false;
        }
        if self.regexp_ctor == 0 {
            return false;
        }
        // %RegExp.prototype%: `flags` still the intrinsic accessor, `exec` still
        // the intrinsic native, `constructor` still %RegExp%.
        let proto_ok = match self.heap.get(self.regexp_proto) {
            HeapObj::Object(m) => {
                let flags_ok = m.pos("flags").is_some_and(|i| {
                    m.attrs[i].accessor
                        && m.vals[i].is_heap()
                        && matches!(self.heap.get(m.vals[i].heap_index()),
                                    HeapObj::Native(n) if *n == native::REGEXP_GET_FLAGS)
                });
                let exec_ok = m.pos("exec").is_some_and(|i| {
                    !m.attrs[i].accessor
                        && m.vals[i].is_heap()
                        && matches!(self.heap.get(m.vals[i].heap_index()),
                                    HeapObj::Native(n) if *n == native::REGEXP_EXEC)
                });
                let ctor_ok = m.pos("constructor").is_some_and(|i| {
                    !m.attrs[i].accessor
                        && m.vals[i].is_heap()
                        && m.vals[i].heap_index() == self.regexp_ctor
                });
                // `@@match` still the intrinsic data property. Replacing it with
                // a GETTER makes the construction the fast path elides observable
                // (see the own-prop check above), and a plain replacement value
                // changes what IsRegExp answers inside the RegExp constructor.
                let match_ok = m.pos("@@match").is_some_and(|i| {
                    !m.attrs[i].accessor
                        && m.vals[i].is_heap()
                        && matches!(self.heap.get(m.vals[i].heap_index()),
                                    HeapObj::Native(n) if *n == native::REGEXP_SYM_MATCH)
                });
                flags_ok && exec_ok && ctor_ok && match_ok
            }
            _ => false,
        };
        if !proto_ok {
            return false;
        }
        // %RegExp%[@@species] still the default accessor (never replaced).
        match self.heap.get(self.regexp_ctor) {
            HeapObj::Object(m) => m.pos("@@species").is_some_and(|i| {
                m.attrs[i].accessor
                    && m.vals[i].is_heap()
                    && matches!(self.heap.get(m.vals[i].heap_index()),
                                HeapObj::Native(n) if *n == native::SPECIES_GET)
            }),
            _ => false,
        }
    }

    /// `regexp_matchall_fast_ok` answered from the resolved slots when they
    /// are warm (version compares + slot reads — see [`MatchallFastSlots`]);
    /// a cold or invalidated memo re-runs the prototype/constructor half of
    /// the full gate once and re-resolves. The instance half (proto identity,
    /// own-shadow probes, the ctor anchor) is instance-specific and cheap, so
    /// it runs uncached per call, read-for-read the gate's opening.
    pub(crate) fn regexp_matchall_fast_ok_cached(&mut self, re: u32) -> bool {
        if !fastok_memo_enabled() {
            return self.regexp_matchall_fast_ok(re);
        }
        match self.proto_of.get(&re) {
            None => {}
            Some(p) if p.is_heap() && p.heap_index() == self.regexp_proto => {}
            _ => return false,
        }
        if self.arr_props.get(&re).is_some_and(|m| {
            m.pos("flags").is_some()
                || m.pos("constructor").is_some()
                || m.pos("exec").is_some()
                || m.pos("@@matchAll").is_some()
                || m.pos("@@match").is_some()
        }) {
            return false;
        }
        if self.regexp_ctor == 0 {
            return false;
        }
        if self.matchall_fast_from_slots() {
            return true;
        }
        // Cold memo, or a guarded version moved: run the shared half of the
        // full gate and capture the slots for the next call. `None` (not
        // pristine) leaves every call on the full re-proof — exactly the
        // pre-memo behavior, and those calls take the observable protocol
        // anyway.
        let slots = self.matchall_fast_resolve_slots();
        self.matchall_fast_slots = slots;
        slots.is_some()
    }

    /// Answer the shared pristine question from the resolved slots: `true`
    /// only when every guard holds. Any mismatch — a moved version, a slot no
    /// longer naming its key, an in-place overwrite, a flipped accessor bit —
    /// declines to the full re-proof rather than reasoning about it (unlike
    /// the promise cache there is no fast `false` here: a DIFFERENT value in
    /// a slot could still be an equivalent intrinsic identity).
    #[inline]
    fn matchall_fast_from_slots(&self) -> bool {
        let Some(c) = self.matchall_fast_slots else { return false };
        if self.heap.version_of(self.regexp_proto) != c.proto_version
            || self.heap.version_of(self.regexp_ctor) != c.ctor_version
        {
            return false;
        }
        // Belt-and-braces key checks as in the promise cache: the versions
        // say the layout is unchanged; verify the slots still name their keys
        // anyway, so an un-bumped structural change could only ever cost a
        // re-proof, never a wrong answer.
        let pinned = |m: &ObjMap, key: &str, accessor: bool, (slot, idx, ver): (u32, u32, u32)| {
            let s = slot as usize;
            m.keys.get(s).is_some_and(|k| k == key)
                && m.attrs[s].accessor == accessor
                && m.vals[s].is_heap()
                && m.vals[s].heap_index() == idx
                // The same object, un-replaced since fill proved it the right
                // `Native` — no `heap.get` needed.
                && self.heap.version_of(idx) == ver
        };
        let m = match self.heap.get(self.regexp_proto) {
            HeapObj::Object(m) => m,
            // Unreachable under a matching version (`Heap::replace` bumps).
            _ => return false,
        };
        if !(pinned(m, "flags", true, c.flags)
            && pinned(m, "exec", false, c.exec)
            && pinned(m, "@@match", false, c.matchsym))
        {
            return false;
        }
        let cs = c.ctor_slot as usize;
        if m.keys.get(cs).map_or(true, |k| k != "constructor")
            || m.attrs[cs].accessor
            || !m.vals[cs].is_heap()
            || m.vals[cs].heap_index() != self.regexp_ctor
        {
            return false;
        }
        match self.heap.get(self.regexp_ctor) {
            HeapObj::Object(mc) => pinned(mc, "@@species", true, c.species),
            _ => false,
        }
    }

    /// Run the shared (prototype/constructor) half of the full gate —
    /// read-for-read the same checks as `regexp_matchall_fast_ok` past its
    /// instance probes — and, when it holds, capture the slot indices plus
    /// the version of every object the proof read. `Some` is "pristine, and
    /// how to re-check it warm"; `None` is "not pristine".
    fn matchall_fast_resolve_slots(&self) -> Option<MatchallFastSlots> {
        let pin = |m: &ObjMap, key: &str, accessor: bool, id: u16| {
            let i = m.pos(key)?;
            let v = m.vals[i];
            (m.attrs[i].accessor == accessor
                && v.is_heap()
                && matches!(self.heap.get(v.heap_index()), HeapObj::Native(n) if *n == id))
                .then(|| (i as u32, v.heap_index(), self.heap.version_of(v.heap_index())))
        };
        let m = match self.heap.get(self.regexp_proto) {
            HeapObj::Object(m) => m,
            _ => return None,
        };
        let flags = pin(m, "flags", true, native::REGEXP_GET_FLAGS)?;
        let exec = pin(m, "exec", false, native::REGEXP_EXEC)?;
        let matchsym = pin(m, "@@match", false, native::REGEXP_SYM_MATCH)?;
        let ctor_slot = m.pos("constructor").filter(|&i| {
            !m.attrs[i].accessor
                && m.vals[i].is_heap()
                && m.vals[i].heap_index() == self.regexp_ctor
        })? as u32;
        let mc = match self.heap.get(self.regexp_ctor) {
            HeapObj::Object(mc) => mc,
            _ => return None,
        };
        let species = pin(mc, "@@species", true, native::SPECIES_GET)?;
        Some(MatchallFastSlots {
            proto_version: self.heap.version_of(self.regexp_proto),
            ctor_version: self.heap.version_of(self.regexp_ctor),
            flags,
            exec,
            matchsym,
            species,
            ctor_slot,
        })
    }

    /// The heap index if `v` is a RegExp, else None.
    pub(crate) fn as_regexp(&self, v: Value) -> Option<u32> {
        if v.is_heap() && matches!(self.heap.get(v.heap_index()), HeapObj::RegExp { .. }) {
            Some(v.heap_index())
        } else {
            None
        }
    }

    /// Coerce a `String.prototype.match`/`search` argument to a RegExp: a RegExp
    /// passes through; anything else becomes `new RegExp(arg)`.
    pub(crate) fn to_regexp_arg(&mut self, v: Value) -> Result<u32, Thrown> {
        if let Some(i) = self.as_regexp(v) {
            return Ok(i);
        }
        let p = if v.is_undefined() { self.alloc_str(String::new()) } else { v };
        Ok(self.build_regexp(p, Value::UNDEFINED)?.heap_index())
    }

    /// Expand a `String.prototype.replace` string template against a match: `$&`
    /// (whole), `` $` ``/`$'` (pre/post), `$N`/`$NN` (group), `$<name>` (named), `$$`.
    pub(crate) fn expand_replacement(
        &self,
        tmpl: &str,
        whole: &str,
        groups: &[Option<String>],
        named: &[(String, Option<String>)],
        named_defined: bool,
        pre: &str,
        post: &str,
        limit: usize,
    ) -> Result<String, Thrown> {
        // `limit` caps the output in BYTES: a `$1`-heavy template applied to a
        // huge capture would otherwise build an unbounded string (hang / OOM —
        // staging/sm/String/replace-math.js). Same 2^28 bound as "repeat".
        let mut out = String::with_capacity(tmpl.len().min(limit));
        macro_rules! push {
            ($s:expr) => {{
                let s: &str = $s;
                if s.len() > limit - out.len() {
                    return Err(Thrown("RangeError: Invalid string length".into()));
                }
                out.push_str(s);
            }};
        }
        let bytes = tmpl.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'$' && i + 1 < bytes.len() {
                let c = bytes[i + 1];
                match c {
                    b'$' => {
                        push!("$");
                        i += 2;
                    }
                    b'&' => {
                        push!(whole);
                        i += 2;
                    }
                    b'`' => {
                        push!(pre);
                        i += 2;
                    }
                    b'\'' => {
                        push!(post);
                        i += 2;
                    }
                    b'<' => {
                        // `$<name>` substitutes the named capture (or "" if absent)
                        // when named captures are present; otherwise (no groups
                        // object / namedCaptures undefined) "$<" is a literal.
                        if !named_defined {
                            push!("$");
                            i += 1;
                        } else if let Some(end) = tmpl[i + 2..].find('>') {
                            let name = &tmpl[i + 2..i + 2 + end];
                            if let Some((_, Some(g))) = named.iter().find(|(n, _)| n == name) {
                                push!(g);
                            }
                            i += 2 + end + 1;
                        } else {
                            push!("$");
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
                                push!(g);
                            }
                            i += 3;
                        } else if d1 >= 1 && d1 <= groups.len() {
                            if let Some(g) = &groups[d1 - 1] {
                                push!(g);
                            }
                            i += 2;
                        } else {
                            push!("$");
                            i += 1;
                        }
                    }
                    _ => {
                        push!("$");
                        i += 1;
                    }
                }
            } else {
                // copy one UTF-8 char
                let ch = tmpl[i..].chars().next().unwrap();
                let mut b = [0u8; 4];
                push!(ch.encode_utf8(&mut b));
                i += ch.len_utf8();
            }
        }
        Ok(out)
    }

    /// RegExp instance property reads: `lastIndex`, `source` (empty → "(?:)"),
    /// `flags`, and the per-flag booleans; methods delegate to RegExp.prototype.
    /// EscapeRegExpPattern: render `source` so it round-trips between two `/`
    /// delimiters — escape a bare `/` and the line terminators, pass `\x` pairs
    /// through verbatim, and map the empty pattern to `(?:)`.
    pub(crate) fn escaped_source(&self, source: &str) -> String {
        if source.is_empty() {
            return "(?:)".to_string();
        }
        let chars: Vec<char> = source.chars().collect();
        let mut out = String::new();
        let mut i = 0;
        // A `/` inside a character class needs no escape — RegularExpressionClassChar
        // admits it literally — and escaping it there made `new RegExp("[/]").source`
        // report `[\/]`. An unescaped `[` opens the class and the next unescaped `]`
        // closes it (classes do not nest for this purpose: `/[[]/]/` really does end
        // its class at the first `]`).
        let mut in_class = false;
        while i < chars.len() {
            let c = chars[i];
            if c == '\\' && i + 1 < chars.len() {
                // An escape pair passes through UNCHANGED — except that the
                // escaped character may itself be a raw LineTerminator, and
                // EscapeRegExpPattern's whole job is that `eval("/" + source +
                // "/")` re-parses. Emitting `\` + a literal LF produced an
                // unterminated regular expression.
                out.push('\\');
                match chars[i + 1] {
                    '\n' => out.push_str("n"),
                    '\r' => out.push_str("r"),
                    '\u{2028}' => out.push_str("u2028"),
                    '\u{2029}' => out.push_str("u2029"),
                    other => out.push(other),
                }
                i += 2;
                continue;
            }
            match c {
                '[' => {
                    in_class = true;
                    out.push(c);
                }
                ']' => {
                    in_class = false;
                    out.push(c);
                }
                '/' if !in_class => out.push_str("\\/"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\u{2028}' => out.push_str("\\u2028"),
                '\u{2029}' => out.push_str("\\u2029"),
                _ => out.push(c),
            }
            i += 1;
        }
        out
    }

    /// WTF-8 twin of [`escaped_source`], for a pattern holding lone surrogates
    /// (`regexp_exact_source` side table): operates on code points over the
    /// exact bytes — same escapes (`/`, line terminators, `\x` pairs verbatim,
    /// empty → `(?:)`) — and returns WTF-8 bytes for the WTF-8 string
    /// constructor. A lone surrogate passes through as itself (the spec's
    /// EscapeRegExpPattern leaves it untouched), which `escaped_source` could
    /// never produce from its lossy `&str` view.
    pub(crate) fn escaped_source_wtf8(&self, bytes: &[u8]) -> Vec<u8> {
        if bytes.is_empty() {
            return b"(?:)".to_vec();
        }
        let cps: Vec<u32> = crate::heap::wtf8_code_points(bytes).collect();
        let mut out: Vec<u8> = Vec::with_capacity(bytes.len() + 4);
        let mut i = 0;
        // Same character-class rule as `escaped_source`: `/` is literal inside `[…]`.
        let mut in_class = false;
        while i < cps.len() {
            let c = cps[i];
            if c == u32::from('\\') && i + 1 < cps.len() {
                crate::heap::wtf8_push_cp(&mut out, c);
                crate::heap::wtf8_push_cp(&mut out, cps[i + 1]);
                i += 2;
                continue;
            }
            match c {
                0x5B => {
                    in_class = true;
                    out.push(b'[');
                }
                0x5D => {
                    in_class = false;
                    out.push(b']');
                }
                0x2F if !in_class => out.extend_from_slice(b"\\/"),
                0x0A => out.extend_from_slice(b"\\n"),
                0x0D => out.extend_from_slice(b"\\r"),
                0x2028 => out.extend_from_slice(b"\\u2028"),
                0x2029 => out.extend_from_slice(b"\\u2029"),
                _ => crate::heap::wtf8_push_cp(&mut out, c),
            }
            i += 1;
        }
        out
    }

    /// The `source` string Value for the RegExp at `idx` whose lossy escaped
    /// source is `src`: exact-WTF-8 when the side table has the pattern's
    /// exact bytes (lone surrogates round-trip), else the plain lossy string.
    pub(crate) fn regexp_source_value(&mut self, idx: u32, src: &str) -> Value {
        if let Some(b) = self.regexp_exact_source.get(&idx) {
            let esc = self.escaped_source_wtf8(b);
            let js = crate::heap::JsStr::from_wtf8(esc);
            return Value::heap(self.heap.alloc_js(js));
        }
        let s = self.escaped_source(src);
        self.alloc_str(s)
    }

    /// RegExp.prototype[Symbol.search] core: reset lastIndex to 0, exec, restore
    /// lastIndex, return the match index or -1. Shared by String.prototype.search.
    pub(crate) fn regexp_search_impl(&mut self, rx: Value, input: Value) -> Result<Value, Thrown> {
        // @@search (22.2.6.12) is spec-generic over any Object `rx`: save lastIndex
        // (Get), zero it (Set) unless already 0, RegExpExec, restore it if exec
        // changed it — all via the observable get/set_prop protocol, honouring a
        // user lastIndex getter/setter and a custom `exec`.
        let prev = self.get_prop(rx, "lastIndex")?;
        let zero = Value::int(0);
        if !self.same_value(prev, zero) {
            self.set_prop(rx, "lastIndex", zero, true)?;
        }
        let result = self.regexp_exec_abstract(rx.heap_index(), input)?;
        let cur = self.get_prop(rx, "lastIndex")?;
        if !self.same_value(cur, prev) {
            self.set_prop(rx, "lastIndex", prev, true)?;
        }
        if result == Value::NULL {
            return Ok(Value::int(-1));
        }
        self.get_prop(result, "index")
    }

    /// RegExp.prototype[Symbol.replace] (ES 22.2.6.11) — the OBSERVABLE protocol:
    /// generic over any Object `rx`, honouring a user `exec`/`flags`/`lastIndex`,
    /// reading each result's `0`/`length`/`index`/group-N/`groups` via Get, and
    /// building the replacement from THOSE values. Reuses `regexp_exec_abstract` so a
    /// user `exec` governs the matches. All positions (`index`, lastIndex, slice
    /// bounds, the replacer's offset argument) are UTF-16 unit indices.
    pub(crate) fn regexp_symbol_replace(
        &mut self,
        rx: Value,
        string: Value,
        replace_value: Value,
    ) -> Result<Value, Thrown> {
        // ToString(string) — IDENTITY for a string value (exact WTF-8).
        let s_val = self.to_str_value(string)?;
        // Encode ONCE; every position below indexes this unit buffer.
        let u16s: Vec<u16> = self.value_units(s_val);
        let length_s = u16s.len();
        // `s_val` and `results` live in Rust locals across exec/replacer
        // re-entries — hold GC off for the whole protocol.
        let _gc = self.gc_lock_guard();
        let functional = self.is_callable(replace_value);
        let replace_str = if functional { String::new() } else { self.to_js_string(replace_value)? };
        // flags / global / fullUnicode are observable (Get, ToString).
        let flags_v = self.get_prop(rx, "flags")?;
        let flags = self.to_js_string(flags_v)?;
        let global = flags.contains('g');
        // fullUnicode (`u`/`v`) selects code-point AdvanceStringIndex.
        let full_unicode = flags.contains('u') || flags.contains('v');
        if global {
            self.set_prop(rx, "lastIndex", Value::int(0), true)?;
        }
        // Collect all exec results through the exec protocol (honouring user `exec`).
        let mut results: Vec<Value> = Vec::new();
        let mut guard = 0u32;
        loop {
            guard += 1;
            if guard > 5_000_000 {
                break;
            }
            let result = self.regexp_exec_abstract(rx.heap_index(), s_val)?;
            if result == Value::NULL {
                break;
            }
            results.push(result);
            if !global {
                break;
            }
            // An empty match advances lastIndex so the loop makes progress.
            let match0 = self.get_prop(result, "0")?;
            if self.to_js_string(match0)?.is_empty() {
                let li_v = self.get_prop(rx, "lastIndex")?;
                // ToLength: clamp to 2^53-1 BEFORE the advance.
                let this_index =
                    self.to_integer_or_zero(li_v)?.clamp(0, (1i64 << 53) - 1) as usize;
                let next = advance_string_index(&u16s, this_index, full_unicode);
                self.set_prop(rx, "lastIndex", Value::num(next as f64), true)?;
            }
        }
        // Build the accumulated result (WTF-8 — subject slices stay exact),
        // reading each match's fields via Get.
        let mut accumulated: Vec<u8> = Vec::new();
        let mut next_pos: usize = 0;
        for result in results {
            let len_v = self.get_prop(result, "length")?;
            let n_captures = (self.to_integer_or_zero(len_v)?.max(0) as usize).saturating_sub(1);
            let matched_v = self.get_prop(result, "0")?;
            // ToString(Get(result,"0")) — IDENTITY for a string value; its UNIT
            // length determines how far this match consumes the subject.
            let matched_val = self.to_str_value(matched_v)?;
            let match_len = self.heap.str_units(matched_val.heap_index()).unwrap_or(0);
            let pos_v = self.get_prop(result, "index")?;
            let position = self.to_integer_or_zero(pos_v)?.clamp(0, length_s as i64) as usize;
            let mut captures: Vec<Option<String>> = Vec::with_capacity(n_captures);
            for n in 1..=n_captures {
                let cap_v = self.get_prop(result, &n.to_string())?;
                captures.push(if cap_v == Value::UNDEFINED {
                    None
                } else {
                    Some(self.to_js_string(cap_v)?)
                });
            }
            let named_v = self.get_prop(result, "groups")?;
            let named_defined = named_v != Value::UNDEFINED;
            // Replacement bytes (WTF-8): the functional path appends a returned
            // string's EXACT bytes; the template path expands over lossy views.
            let replacement: Vec<u8> = if functional {
                let mut argv: Vec<Value> = Vec::with_capacity(n_captures + 4);
                argv.push(matched_val);
                for c in &captures {
                    argv.push(match c {
                        Some(g) => self.alloc_str(g.clone()),
                        None => Value::UNDEFINED,
                    });
                }
                argv.push(Value::num(position as f64));
                argv.push(s_val);
                if named_defined {
                    argv.push(named_v);
                }
                let r = self.call_value(replace_value, Value::UNDEFINED, &argv)?;
                let rv = self.to_str_value(r)?;
                self.heap
                    .str_wtf8_cow(rv.heap_index())
                    .map(|c| c.into_owned())
                    .unwrap_or_default()
            } else {
                // GetSubstitution: read the named-capture group object's own props.
                // Step l.i.1 — when `groups` is not undefined it is ToObject'd, so a
                // primitive (e.g. a string `groups`) is boxed and its properties
                // (`$<length>` etc.) become readable; ToObject(null) throws.
                let named_list: Vec<(String, Option<String>)> = if named_defined {
                    // ToObject(namedCaptures): null throws a TypeError (the public
                    // Object(null) would return {}, but this is the internal op).
                    self.require_object_coercible(named_v)?;
                    let obj = self.to_object(named_v)?;
                    // GetSubstitution reads EXACTLY the template's `$<name>`
                    // groups via Get — through the PROTOTYPE chain, so an
                    // inherited group property resolves (groups-object-subclass)
                    // and a missing one substitutes the empty string.
                    let mut v: Vec<(String, Option<String>)> = Vec::new();
                    let mut rest = replace_str.as_str();
                    while let Some(p) = rest.find("$<") {
                        rest = &rest[p + 2..];
                        let Some(e) = rest.find('>') else { break };
                        let name = rest[..e].to_string();
                        rest = &rest[e + 1..];
                        if !v.iter().any(|(n, _)| *n == name) {
                            let val = self.get_prop(obj, &name)?;
                            let sv = if val == Value::UNDEFINED {
                                None
                            } else {
                                Some(self.to_js_string(val)?)
                            };
                            v.push((name, sv));
                        }
                    }
                    v
                } else {
                    Vec::new()
                };
                let matched_lossy = self
                    .heap
                    .str_cow(matched_val.heap_index())
                    .map(|c| c.into_owned())
                    .unwrap_or_default();
                let pre = String::from_utf16_lossy(&u16s[..position]);
                let post_start = (position + match_len).min(length_s);
                let post = String::from_utf16_lossy(&u16s[post_start..]);
                self.expand_replacement(
                    &replace_str,
                    &matched_lossy,
                    &captures,
                    &named_list,
                    named_defined,
                    &pre,
                    &post,
                    (1usize << 28).saturating_sub(accumulated.len()),
                )?
                .into_bytes()
            };
            if position >= next_pos {
                push_units(&mut accumulated, &u16s[next_pos..position]);
                if replacement.len() > (1usize << 28).saturating_sub(accumulated.len()) {
                    return Err(Thrown("RangeError: Invalid string length".into()));
                }
                crate::heap::wtf8_push(&mut accumulated, &replacement);
                next_pos = position + match_len;
            }
        }
        if next_pos < length_s {
            push_units(&mut accumulated, &u16s[next_pos..]);
        }
        Ok(Value::heap(self.heap.alloc_js(crate::heap::JsStr::from_wtf8(accumulated))))
    }

    /// RegExpExec (ES 22.2.7.1): the exec PROTOCOL. When the regex has a callable
    /// own/inherited `exec` (honouring a user override), call it with the subject
    /// string and require an Object-or-null result; otherwise fall back to the
    /// builtin RegExpBuiltinExec. The `@@match`/`@@search` (non-global) cores route
    /// through this so a custom `re.exec` governs the result.
    pub(crate) fn regexp_exec_abstract(&mut self, re: u32, input: Value) -> Result<Value, Thrown> {
        // PLAIN regexp (a REAL RegExp whose intrinsic `exec` is reached
        // through %RegExp.prototype%): the Get(R,"exec") is unobservable and
        // the call dispatch is the intrinsic — run RegExpBuiltinExec
        // directly. (`re` may be any object here — the protocol is generic —
        // so the real-RegExp check guards the arr_props own-props model.)
        if matches!(self.heap.get(re), HeapObj::RegExp { .. }) && self.regexp_exec_fast_ok(re) {
            return self.regexp_exec(re, input);
        }
        let re_v = Value::heap(re);
        let exec = self.get_prop(re_v, "exec")?;
        if self.is_callable(exec) {
            // ToString(S) — IDENTITY for a string value (exact WTF-8; a lossy
            // copy would strip lone surrogates before exec ever sees them).
            let s = self.to_str_value(input)?;
            let r = self.call_value(exec, re_v, &[s])?;
            let is_object = r.is_heap()
                && !matches!(
                    self.heap.get(r.heap_index()),
                    HeapObj::Str(_) | HeapObj::Cons { .. } | HeapObj::Symbol { .. } | HeapObj::BigInt(_) | HeapObj::BigIntBig(_)
                );
            if r != Value::NULL && !is_object {
                return Err(Thrown(
                    "TypeError: RegExp exec method returned something other than an Object or null"
                        .into(),
                ));
            }
            return Ok(r);
        }
        self.regexp_exec(re, input)
    }

    /// RegExp.prototype[Symbol.match] core: a non-global regex returns the exec
    /// result (array or null); a global regex returns the array of matched
    /// substrings (or null) and resets lastIndex. Shared by String.match.
    pub(crate) fn regexp_match_impl(&mut self, re: u32, input: Value) -> Result<Value, Thrown> {
        // OBSERVABLE @@match (22.2.6.8), generic over any Object `rx`: read
        // ToString(Get(rx,"flags")); a non-global match is just RegExpExec; a global
        // match loops RegExpExec (honouring a user `exec`) collecting ToString(Get(
        // result,"0")), resets lastIndex first, and advances past an empty match.
        let rx = Value::heap(re);
        let flags_v = self.get_prop(rx, "flags")?;
        let flags = self.to_js_string(flags_v)?;
        if !flags.contains('g') {
            return self.regexp_exec_abstract(re, input);
        }
        // fullUnicode (`u`/`v`) selects code-point AdvanceStringIndex.
        let full_unicode = flags.contains('u') || flags.contains('v');
        // ToString(string) — IDENTITY for a string value (exact WTF-8).
        let s_val = self.to_str_value(input)?;
        // Unit buffer for the empty-match AdvanceStringIndex step.
        let u16s: Vec<u16> = self.value_units(s_val);
        // `s_val`/`elems` live in Rust locals across exec re-entries.
        let _gc = self.gc_lock_guard();
        self.set_prop(rx, "lastIndex", Value::int(0), false)?;
        let mut elems: Vec<Value> = Vec::new();
        let mut guard = 0u32;
        loop {
            guard += 1;
            if guard > 5_000_000 {
                break;
            }
            let result = self.regexp_exec_abstract(re, s_val)?;
            if result == Value::NULL {
                break;
            }
            let m0 = self.get_prop(result, "0")?;
            // ToString(Get(result,"0")) — IDENTITY for a string value, so a
            // lone-surrogate match survives into the result array.
            let m0_val = self.to_str_value(m0)?;
            let is_empty = self.heap.str_units(m0_val.heap_index()) == Some(0);
            elems.push(m0_val);
            if is_empty {
                let li_v = self.get_prop(rx, "lastIndex")?;
                // ToLength: clamp to 2^53-1 BEFORE the advance.
                let this_index =
                    self.to_integer_or_zero(li_v)?.clamp(0, (1i64 << 53) - 1) as usize;
                let next = advance_string_index(&u16s, this_index, full_unicode);
                self.set_prop(rx, "lastIndex", Value::num(next as f64), true)?;
            }
        }
        if elems.is_empty() {
            return Ok(Value::NULL);
        }
        Ok(Value::heap(self.heap.alloc(HeapObj::Array(elems))))
    }

    /// RegExp.prototype[Symbol.split] core (simplified — no capture groups in the
    /// output yet). Shared by String.prototype.split for a regex separator.
    pub(crate) fn regexp_split_impl(
        &mut self,
        re: u32,
        input: Value,
        limit: Value,
    ) -> Result<Value, Thrown> {
        // OBSERVABLE @@split (22.2.6.14), generic over any Object `rx`:
        // SpeciesConstructor(rx, %RegExp%) builds a sticky (`y`) splitter, then a
        // loop calls RegExpExec (honouring a user `exec`) reading lastIndex/length/
        // captures via Get. Positions p/q/e are UTF-16 unit indices; the no-match
        // advance is spec AdvanceStringIndex (+2 over an astral pair in `u`/`v`).
        let rx = Value::heap(re);
        // ToString(string) — IDENTITY for a string value (exact WTF-8).
        let s_val = self.to_str_value(input)?;
        let u16s: Vec<u16> = self.value_units(s_val);
        let size = u16s.len();
        // `s_val`/`a` live in Rust locals across construct/exec re-entries.
        let _gc = self.gc_lock_guard();
        // SpeciesConstructor(rx, %RegExp%).
        let default_ctor = Value::heap(self.regexp_ctor);
        let c = {
            let ctor = self.get_prop(rx, "constructor")?;
            if ctor == Value::UNDEFINED {
                default_ctor
            } else if !self.is_object_value(ctor) {
                // SpeciesConstructor step 5: a defined-but-non-object constructor
                // (false / "string" / 86 / null) is a TypeError, before @@species.
                return Err(Thrown(
                    "TypeError: Symbol.split constructor property is not an object".into(),
                ));
            } else {
                let sp = self.get_prop(ctor, "@@species")?;
                if sp == Value::UNDEFINED || sp == Value::NULL {
                    default_ctor
                } else if self.is_constructor(sp) {
                    sp
                } else {
                    return Err(Thrown(
                        "TypeError: Symbol.split species constructor is not a constructor".into(),
                    ));
                }
            }
        };
        // flags (observable) + force the sticky `y` flag on the splitter copy.
        let flags_v = self.get_prop(rx, "flags")?;
        let flags = self.to_js_string(flags_v)?;
        // unicodeMatching (`u`/`v`) selects code-point AdvanceStringIndex.
        let unicode_matching = flags.contains('u') || flags.contains('v');
        let new_flags = if flags.contains('y') { flags } else { format!("{flags}y") };
        let new_flags_v = self.alloc_str(new_flags);
        let splitter = self.construct(c, &[rx, new_flags_v])?;
        let lim: u64 = if limit == Value::UNDEFINED {
            u32::MAX as u64
        } else {
            to_uint32(self.to_number_coerce(limit)?) as u64
        };
        let mut a: Vec<Value> = Vec::new();
        if lim == 0 {
            return Ok(Value::heap(self.heap.alloc(HeapObj::Array(a))));
        }
        // Empty input: one exec; if it matches, the result is empty, else [S].
        if size == 0 {
            let z = self.regexp_exec_abstract(splitter.heap_index(), s_val)?;
            if z == Value::NULL {
                a.push(s_val);
            }
            return Ok(Value::heap(self.heap.alloc(HeapObj::Array(a))));
        }
        let mut p: usize = 0;
        let mut q: usize = 0;
        let mut guard = 0u32;
        while q < size {
            guard += 1;
            if guard > 5_000_000 {
                break;
            }
            self.set_prop(splitter, "lastIndex", Value::num(q as f64), true)?;
            let z = self.regexp_exec_abstract(splitter.heap_index(), s_val)?;
            if z == Value::NULL {
                q = advance_string_index(&u16s, q, unicode_matching);
                continue;
            }
            // e = min(ToLength(Get(splitter,"lastIndex")), size).
            let li_v = self.get_prop(splitter, "lastIndex")?;
            let e = (self.to_integer_or_zero(li_v)?.max(0) as usize).min(size);
            if e == p {
                q = advance_string_index(&u16s, q, unicode_matching);
                continue;
            }
            let t = self.units_value(&u16s[p..q]);
            a.push(t);
            if a.len() as u64 == lim {
                return Ok(Value::heap(self.heap.alloc(HeapObj::Array(a))));
            }
            p = e;
            // Each capturing group (1..n) is emitted between the pieces.
            let zlen_v = self.get_prop(z, "length")?;
            let n_captures = (self.to_integer_or_zero(zlen_v)?.max(0) as usize).saturating_sub(1);
            for i in 1..=n_captures {
                let cap = self.get_prop(z, &i.to_string())?;
                a.push(cap);
                if a.len() as u64 == lim {
                    return Ok(Value::heap(self.heap.alloc(HeapObj::Array(a))));
                }
            }
            q = p;
        }
        let tail = self.units_value(&u16s[p..]);
        a.push(tail);
        Ok(Value::heap(self.heap.alloc(HeapObj::Array(a))))
    }

    pub(crate) fn regexp_get_prop(
        &mut self,
        idx: u32,
        source: &str,
        flags: &str,
        last_index: Value,
        key: &str,
        eff_proto: u32,
    ) -> Result<Value, Thrown> {
        Ok(match key {
            "lastIndex" => last_index,
            // Exact-WTF-8 when the side table has the pattern's exact bytes
            // (lone surrogates round-trip), else the lossy escaped source.
            "source" => self.regexp_source_value(idx, source),
            "flags" => {
                let s = canonical_flags(flags);
                self.alloc_str(s)
            }
            "global" => Value::bool(flags.contains('g')),
            "ignoreCase" => Value::bool(flags.contains('i')),
            "multiline" => Value::bool(flags.contains('m')),
            "dotAll" => Value::bool(flags.contains('s')),
            "unicode" => Value::bool(flags.contains('u')),
            "unicodeSets" => Value::bool(flags.contains('v')),
            "sticky" => Value::bool(flags.contains('y')),
            "hasIndices" => Value::bool(flags.contains('d')),
            // A subclass instance resolves through ITS prototype (proto_of)
            // so class [Symbol.replace]/exec overrides shadow the builtin.
            _ => self.proto_member(eff_proto, key),
        })
    }

    /// `RegExp.prototype.exec(input)`: returns the match-result Array (group 0 +
    /// captures, with `.index`/`.input`/`.groups` in the side table) or `null`.
    /// Advances `lastIndex` for a global/sticky regex.
    ///
    /// Matching runs over the subject's UTF-16 CODE UNITS (regress
    /// `find_from_utf16` for `u`/`v` regexes — code-point elements — and
    /// `find_from_ucs2` otherwise — each unit is an element, so `/./` matches
    /// one surrogate half). Every position regress reports (match range,
    /// capture ranges) is a unit index, identical to JS string indexing
    /// engine-wide: `lastIndex` seeds the search directly and `.index` /
    /// `indices` / `lastIndex` writes take the ranges verbatim.
    ///
    /// ASCII FAST PATH: an all-ASCII subject (the `JsStr::is_ascii` flag) is
    /// matched in place over its bytes with regress `find_from_ascii` — no
    /// per-exec `Vec<u16>` encode. Byte offsets == unit offsets for ASCII, so
    /// every reported range is a valid unit index verbatim. This is
    /// semantically identical to the UCS-2/UTF-16 run: regress folds pattern
    /// chars and closes bracket sets at COMPILE time (full Unicode folding),
    /// so a non-ASCII `CharICase` insn can never match an ASCII element on
    /// either backend, and runtime folding only ever compares two SUBJECT
    /// chars (backrefs) — both ASCII here, where ASCII and Unicode simple
    /// folding agree.
    pub(crate) fn regexp_exec(&mut self, re_idx: u32, input_v: Value) -> Result<Value, Thrown> {
        self.regexp_exec_impl(re_idx, input_v, true)
    }

    /// Read one of a pristine match-result Array's standard named properties
    /// without constructing its ordinary `ObjMap` representation.
    #[inline]
    pub(crate) fn regexp_result_prop(&self, idx: u32, key: &str) -> Option<Value> {
        // Reject unrelated Array names before touching the side table. This
        // helper sits on the generic Array named-read path, so `length` and
        // method reads must not acquire an extra indexed lookup merely because
        // some RegExp result exists elsewhere in the VM.
        let slot = match key {
            "index" => 0,
            "input" => 1,
            "groups" => 2,
            "indices" => 3,
            _ => return None,
        };
        let p = self.regexp_result_props.get(&idx)?;
        if slot == 3 && p.values[3] == Value::UNDEFINED {
            None
        } else {
            Some(p.values[slot])
        }
    }

    /// Convert a compact pristine match-result record into `arr_props` before
    /// an operation that can observe or change descriptors, key order,
    /// deletion, or integrity state.
    ///
    /// The defensive merge handles an element overlay installed by an internal
    /// Array path before materialisation. Standard result names were created
    /// first, so they are pushed first; a later explicit entry with the same key
    /// overwrites that slot while retaining its original insertion order.
    pub(crate) fn materialize_regexp_result_props(&mut self, idx: u32) {
        if self.regexp_result_props.is_empty() {
            return;
        }
        let Some(p) = self.regexp_result_props.remove(&idx) else {
            return;
        };
        rxstats::count_materialized();
        let old = self.arr_props.remove(&idx);
        let has_indices = p.values[3] != Value::UNDEFINED;
        let mut m = ObjMap::side_table_with_capacity(
            3 + has_indices as usize + old.as_ref().map_or(0, ObjMap::len),
        );
        m.push_data("index".to_string(), p.values[0]);
        m.push_data("input".to_string(), p.values[1]);
        m.push_data("groups".to_string(), p.values[2]);
        if has_indices {
            m.push_data("indices".to_string(), p.values[3]);
        }
        if let Some(old) = old {
            for (key, value, attr) in old.iter() {
                m.define(key, value, attr);
            }
            m.class = old.class;
            m.is_ctor = old.is_ctor;
            m.is_raw_json = old.is_raw_json;
            if old.frozen {
                m.freeze();
            } else if old.sealed {
                m.seal();
            } else {
                m.extensible = old.extensible;
            }
        }
        self.arr_props.insert(idx, m);
    }

    /// Promote only when an operation targets one of the compact properties.
    /// Unrelated additions can coexist in `arr_props`; the eventual full
    /// materialisation merges them after the earlier-created standard names.
    #[inline]
    pub(crate) fn materialize_regexp_result_prop_for_key(&mut self, idx: u32, key: &str) {
        if matches!(key, "index" | "input" | "groups" | "indices") {
            self.materialize_regexp_result_props(idx);
        }
    }

    /// `regexp_exec` with `build = false` for `RegExp.prototype.test`: the
    /// IDENTICAL protocol (lastIndex Get/ToLength + the stateful Sets, in spec
    /// order), but the unobservable match-result materialization (array +
    /// capture strings + groups/indices objects) is skipped — returns
    /// `Value::TRUE` instead of the array. `Value::NULL` still means no match.
    pub(crate) fn regexp_exec_impl(
        &mut self,
        re_idx: u32,
        input_v: Value,
        build: bool,
    ) -> Result<Value, Thrown> {
        self.regexp_exec_impl_prebits(re_idx, input_v, build, None)
    }

    /// `regexp_exec_impl` with the four flag-derived bits pre-decoded
    /// (`ITFB_*` layout). Callers passing `Some` must guarantee the bits still
    /// describe [[OriginalFlags]] at match time — which only holds when
    /// `lastIndex` is a plain number (no `valueOf` re-entry can `compile()`
    /// new flags between the ToLength and the flags read) and the regex's
    /// flags cannot have changed since the bits were captured. The fused
    /// matchAll step's matcher qualifies: it is engine-internal, so no user
    /// reference exists to `compile()` it.
    fn regexp_exec_impl_prebits(
        &mut self,
        re_idx: u32,
        input_v: Value,
        build: bool,
        prebits: Option<u8>,
    ) -> Result<Value, Thrown> {
        let _prof = crate::vm::prof::enter(crate::vm::prof::Phase::RegexExec);
        // ToString(string) — IDENTITY for a string value (exact WTF-8 content:
        // a lone-surrogate subject keeps its surrogate rather than decaying to
        // U+FFFD, so `/\uD800/` can match it).
        let input_val = self.to_str_value(input_v)?;
        // `input_val` + the result pieces below live in Rust locals across a
        // possible `lastIndex.valueOf` re-entry — hold GC off until we return.
        let _gc = self.gc_lock_guard();
        // Get(R,"lastIndex") — on a real RegExp this can never run user code:
        // `lastIndex` is a non-configurable own DATA property whose value's
        // source of truth is the heap slot (defineProperty writes the value
        // through; only attrs live in arr_props) — so read the slot directly.
        let li_v = match self.heap.get(re_idx) {
            HeapObj::RegExp { last_index, .. } => *last_index,
            _ => {
                return Err(Thrown(
                    "TypeError: RegExp.prototype.exec called on a non-RegExp".into(),
                ))
            }
        };
        // ToLength(Get(R,"lastIndex")) is RegExpBuiltinExec step 4 and it
        // PRECEDES the [[OriginalFlags]] / [[RegExpMatcher]] reads of steps
        // 5-11: a `lastIndex.valueOf` may call `R.compile(pattern, flags)`,
        // which replaces both, and the run must use what it left behind. The
        // flag bits used to be fused into the same heap.get as the slot, so a
        // recompile that added `g` never updated lastIndex and one that dropped
        // `y` still clobbered it.
        let li = self.to_integer_or_zero(li_v)?.clamp(0, (1i64 << 53) - 1) as usize;
        let (global, sticky, has_indices, unicode) = match prebits {
            // Pre-decoded at iterator creation (B118): `lastIndex` was a
            // number (checked by the caller), so no user code ran above and
            // the flags are what they were when the bits were captured.
            Some(b) => {
                debug_assert!(li_v.is_number());
                (
                    b & ITFB_GLOBAL != 0,
                    b & ITFB_STICKY != 0,
                    b & ITFB_INDICES != 0,
                    b & ITFB_UNICODE != 0,
                )
            }
            None => match self.heap.get(re_idx) {
                HeapObj::RegExp { flags, .. } => {
                    if slim_exec_enabled() {
                        // B124: one pass over the ≤8-byte flag string instead
                        // of four `contains` scans. Same heap.get, same spec
                        // position (AFTER ToLength — a `lastIndex.valueOf`
                        // may `compile()`, and this reads what it left).
                        let (mut g, mut y, mut d, mut u) = (false, false, false, false);
                        for b in flags.bytes() {
                            match b {
                                b'g' => g = true,
                                b'y' => y = true,
                                b'd' => d = true,
                                b'u' | b'v' => u = true,
                                _ => {}
                            }
                        }
                        (g, y, d, u)
                    } else {
                        (
                            flags.contains('g'),
                            flags.contains('y'),
                            flags.contains('d'),
                            flags.contains('u') || flags.contains('v'),
                        )
                    }
                }
                _ => {
                    return Err(Thrown(
                        "TypeError: RegExp.prototype.exec called on a non-RegExp".into(),
                    ))
                }
            },
        };
        let stateful = global || sticky;
        // Step 9: a non-global, non-sticky regex always searches from 0.
        let start = if stateful { li } else { 0 };
        // ASCII subjects match in place over the heap bytes (offsets == unit
        // indices); anything else encodes the subject ONCE per exec.
        // `lastIndex` is already a unit index engine-wide, so it is the
        // search start with no conversion either way.
        let s_idx = input_val.heap_index();
        // B124: ONE subject heap.get serves the flat-check, the ascii bit and
        // (for the ascii case, where units == bytes) the unit length, instead
        // of an unconditional `flatten` (its own get + tag check) plus a
        // second `str_units` get. `flatten` now runs only when the get
        // actually sees a rope — Cons→Str is irreversible, so the re-read
        // after it is a `Str` by construction. `ZIPP_NO_SLIM_EXEC=1` restores
        // the split reads; both compute identical values on every input.
        let (is_ascii, ascii_units) = if slim_exec_enabled() {
            match self.heap.get(s_idx) {
                HeapObj::Str(js) => (js.is_ascii(), js.as_bytes().len()),
                _ => {
                    self.heap.flatten(s_idx);
                    match self.heap.get(s_idx) {
                        HeapObj::Str(js) => (js.is_ascii(), js.as_bytes().len()),
                        _ => (false, 0),
                    }
                }
            }
        } else {
            self.heap.flatten(s_idx);
            (matches!(self.heap.get(s_idx), HeapObj::Str(js) if js.is_ascii()), 0)
        };
        let u16s: Vec<u16> = if is_ascii { Vec::new() } else { self.value_units(input_val) };
        let subj_units = if is_ascii {
            if slim_exec_enabled() {
                ascii_units
            } else {
                self.heap.str_units(s_idx).unwrap_or(0)
            }
        } else {
            u16s.len()
        };
        let found = if start > subj_units {
            None
        } else if is_ascii {
            self.ensure_regexp_ascii_twin(re_idx);
            // Both the subject string and the regex/twin are shared borrows of
            // `self.heap` — they coexist. Prefer the byte-optimized twin; fall
            // back to the base program when the twin compile failed.
            let subj: &str = match self.heap.get(s_idx) {
                HeapObj::Str(js) => js.as_str_wf(),
                _ => "",
            };
            match self.heap.get(re_idx) {
                HeapObj::RegExp { ascii_twin: Some(Some(twin)), .. } => {
                    twin.find_from_ascii(subj, start).next()
                }
                HeapObj::RegExp { regex, .. } => regex.find_from_ascii(subj, start).next(),
                _ => None,
            }
        } else {
            match self.heap.get(re_idx) {
                HeapObj::RegExp { regex, .. } => {
                    if unicode {
                        regex.find_from_utf16(&u16s, start).next()
                    } else {
                        regex.find_from_ucs2(&u16s, start).next()
                    }
                }
                _ => None,
            }
        };
        // Sticky: the match must begin exactly at the search start.
        let found = found.filter(|m| !(sticky && m.start() != start));
        let m = match found {
            Some(m) => m,
            None => {
                if stateful {
                    // RegExpBuiltinExec Set(R,"lastIndex",0,true): a non-writable
                    // lastIndex makes a failed global/sticky exec throw.
                    self.regexp_write_last_index(re_idx, Value::int(0))?;
                }
                return Ok(Value::NULL);
            }
        };
        let (mstart, mend) = (m.start(), m.end());
        if stateful {
            // RegExpBuiltinExec Set(R,"lastIndex",e,true) — spec step 15, BEFORE
            // the (unobservable) result construction; throws if non-writable.
            self.regexp_write_last_index(re_idx, Value::num(mend as f64))?;
        }
        // A unit-range slice of the subject: a byte slice of the heap string
        // for an ASCII subject, else a slice of the encoded unit buffer.
        let mk = |vm: &mut Self, r: std::ops::Range<usize>| -> Value {
            if is_ascii {
                vm.ascii_slice_value(s_idx, r)
            } else {
                vm.units_value(&u16s[r])
            }
        };
        // Annex B legacy RegExp statics (RegExp.input/$_, lastMatch/$&,
        // lastParen/$+, leftContext/$`, rightContext/$', $1–$9): refreshed by
        // EVERY successful RegExpBuiltinExec — `exec`, `test`, and the String /
        // RegExp methods that funnel through this builtin.
        // Slots 2..=13 (lastParen, leftContext, rightContext, $1..$9) are all
        // SLICES OF THE SUBJECT, and `ascii_slice_value` copies: `as_bytes()[r]
        // .to_vec()`, an `is_ascii` rescan in `from_wtf8`, and a heap slot. So the
        // eager form copied leftContext + rightContext — together ~87% of the
        // subject — on EVERY successful match, `test` included (the `!build`
        // early-out is below), plus one slice per capture that the result array
        // then sliced again. Virtually no program reads `RegExp.leftContext`.
        //
        // Defer them: root the subject and keep unit RANGES, and materialise all
        // THIRTEEN on the first legacy-static getter read (see
        // `Vm::regexp_last_materialise`). Only slot 0 stays eager — `input_val` is
        // already a Value.
        //
        // Slot 1 (lastMatch) was eager on the stated grounds that `whole` "is
        // computed for the result array regardless". That holds for `exec` and NOT
        // for `test`, which returns a boolean: every successful `.test()` was paying
        // one `ascii_slice_value` — a malloc, a memcpy of the matched span, an
        // `is_ascii` rescan of those same bytes, and a heap slot — for a string
        // nothing ever read. On `regex-log-scan`'s anchored phase the match IS the
        // whole ~112-byte line, ~90k times.
        //
        // MEASURED (tools/bench.py --ab-env against the same binary, 21 paired
        // reps): ablating this block entirely was -8.65% on regex-log-scan
        // [-8.86, -7.77], 2015ms -> 1844ms. That ablation is the ceiling this is
        // aiming at, and it is reached whenever the statics go unread.
        //
        // Only the ASCII subject defers, because a non-ASCII slice reads the
        // locally-decoded `u16s` buffer that does not outlive this call; that path
        // is byte-for-byte what it always was. The length bound keeps the `as u32`
        // range casts below from truncating silently — unreachable in practice (a
        // 4GB flat string), and a wrong slice is exactly what it would produce.
        if is_ascii && subj_units <= u32::MAX as usize {
            // ranges[i] is slot 1+i: lastMatch, lastParen, leftContext,
            // rightContext, $1..$9.
            let mut ranges: [Option<(u32, u32)>; 13] = [None; 13];
            ranges[0] = Some((mstart as u32, mend as u32));
            // lastParen: the LAST participating capture, "" when none did.
            ranges[1] = m.captures.iter().rev().find_map(|c| c.clone()).map(|r| (r.start as u32, r.end as u32));
            ranges[2] = Some((0, mstart as u32));
            ranges[3] = Some((mend as u32, subj_units as u32));
            for i in 0..9 {
                ranges[4 + i] =
                    m.captures.get(i).and_then(|c| c.clone()).map(|r| (r.start as u32, r.end as u32));
            }
            // `regexp_last_lazy` being `Some` is what routes slots >= 1
            // through materialisation first, so the 13 tail slots are
            // placeholders the getter never returns — when the record is
            // already 14 wide only slot 0 needs storing (B118: the per-step
            // clear+resize wrote 14 slots per successful exec; any stale tail
            // value is overwritten by `regexp_last_materialise` before a
            // getter can see it, and is at worst a 13-value GC root).
            if self.regexp_last.len() == 14 {
                self.regexp_last[0] = input_val;
            } else {
                self.regexp_last.clear();
                self.regexp_last.push(input_val);
                self.regexp_last.resize(14, Value::UNDEFINED);
            }
            self.regexp_last_lazy = Some(RegexpLastLazy { subj: input_val, subj_idx: s_idx, ranges });
        } else {
            // A non-ASCII subject cannot defer: the slices read the locally-decoded
            // `u16s` buffer, which does not outlive this call.
            let empty = self.alloc_str(String::new());
            let mut rec = Vec::with_capacity(14);
            rec.push(input_val);
            let whole_units = mk(self, mstart..mend);
            rec.push(whole_units);
            rec.push(match m.captures.iter().rev().find_map(|c| c.clone()) {
                Some(r) => mk(self, r),
                None => empty,
            });
            rec.push(mk(self, 0..mstart));
            rec.push(mk(self, mend..subj_units));
            for i in 0..9 {
                rec.push(match m.captures.get(i).and_then(|c| c.clone()) {
                    Some(r) => mk(self, r),
                    None => empty,
                });
            }
            self.regexp_last = rec;
            self.regexp_last_lazy = None;
        }
        if !build {
            // `test`: nothing below is reachable, and with slot 1 deferred there is
            // no longer any string to build here at all.
            return Ok(Value::TRUE);
        }
        let whole = mk(self, mstart..mend);
        let mut elems = Vec::with_capacity(1 + m.captures.len());
        elems.push(whole);
        for cap in &m.captures {
            let v = match cap {
                Some(r) => mk(self, r.clone()),
                None => Value::UNDEFINED,
            };
            elems.push(v);
        }
        let named: Vec<(String, Option<std::ops::Range<usize>>)> =
            m.named_groups().map(|(n, r)| (n.to_string(), r)).collect();
        let groups = if named.is_empty() {
            Value::UNDEFINED
        } else {
            let mut gm = ObjMap::with_capacity(named.len());
            for (name, r) in &named {
                let v = match r {
                    Some(r) => mk(self, r.clone()),
                    None => Value::UNDEFINED,
                };
                gm.set(name, v);
            }
            let gidx = self.heap.alloc(HeapObj::Object(Box::new(gm)));
            // The groups object is OrdinaryObjectCreate(null) — no prototype.
            self.proto_of.insert(gidx, Value::NULL);
            Value::heap(gidx)
        };
        let arr_idx = self.heap.alloc(HeapObj::Array(elems));
        let index_v = Value::num(mstart as f64);
        let input_sv = input_val;
        // index/input/groups are real own data properties of the result array
        // (writable, enumerable, configurable) so reflection sees them.
        let attr = PropAttr {
            writable: true,
            enumerable: true,
            configurable: true,
            accessor: false,
            setter: Value::UNDEFINED,
        };
        // `/d` (hasIndices): an `indices` array of [start,end] unit ranges for
        // the whole match + each capture group, with `.groups` for named groups.
        let indices_v = if has_indices {
            let mk = |vm: &mut Self, r: &std::ops::Range<usize>| -> Value {
                let s = Value::num(r.start as f64);
                let e = Value::num(r.end as f64);
                Value::heap(vm.heap.alloc(HeapObj::Array(vec![s, e])))
            };
            let mut idx_elems = vec![mk(self, &(mstart..mend))];
            for cap in &m.captures {
                idx_elems.push(match cap {
                    Some(r) => mk(self, r),
                    None => Value::UNDEFINED,
                });
            }
            let idx_groups = if named.is_empty() {
                Value::UNDEFINED
            } else {
                let mut gm = ObjMap::new();
                for (name, r) in &named {
                    let v = match r {
                        Some(r) => mk(self, r),
                        None => Value::UNDEFINED,
                    };
                    gm.set(name, v);
                }
                let gidx = self.heap.alloc(HeapObj::Object(Box::new(gm)));
                self.proto_of.insert(gidx, Value::NULL);
                Value::heap(gidx)
            };
            let indices_arr = self.heap.alloc(HeapObj::Array(idx_elems));
            self.arr_props.entry(indices_arr).or_insert_with(ObjMap::new_side_table).define(
                "groups",
                idx_groups,
                attr,
            );
            Value::heap(indices_arr)
        } else {
            Value::UNDEFINED
        };
        // `arr_idx` is a fresh heap slot. GC prunes both property tables against
        // the mark bits before a slot can be recycled, so neither can carry a
        // stale entry from the previous occupant.
        // Keep the pristine standard fields in one fixed record: this removes
        // the per-result ObjMap, three key-string allocations, and three Vec
        // allocations. Mutation/reflection materialises the exact ordinary
        // data properties lazily; direct reads and presence checks stay compact.
        debug_assert!(!self.arr_props.contains_key(&arr_idx));
        debug_assert!(!self.regexp_result_props.contains_key(&arr_idx));
        self.regexp_result_props.insert(
            arr_idx,
            RegexpResultProps {
                values: [index_v, input_sv, groups, indices_v],
            },
        );
        rxstats::count_compact();
        if !match_variant_enabled() {
            // Off-switch: reproduce the eager representation (an ordinary
            // `ObjMap` in `arr_props`) so the compact form is A/B-able.
            self.materialize_regexp_result_props(arr_idx);
        }
        Ok(Value::heap(arr_idx))
    }

    /// The SLIM per-call exec for the fused matchAll step (B124): the same
    /// RegExpBuiltinExec `regexp_exec_impl_prebits` performs, minus every
    /// protocol step the `ITFB_FUSED` creation proof already paid for. What
    /// is elided, and why each elision is sound:
    ///
    ///  - ToString(subject): the iterator record's subject IS a string Value
    ///    (identity conversion — nothing to do).
    ///  - the lastIndex re-read + ToInteger: the caller just read the slot
    ///    for its `is_number` guard and passes the Value through; the inline
    ///    truncation below is exactly `to_integer_or_zero` on the numeric
    ///    domain (and only engine-written numbers ever reach this slot).
    ///  - the per-step flag decode: `fbits` was captured at creation and the
    ///    matcher is engine-internal — no user reference exists to
    ///    `compile()` new flags (the `Some(prebits)` soundness argument).
    ///  - flatten + `is_ascii` + `str_units` heap.gets: `ITFB_FUSED` encodes
    ///    "flat-ASCII subject", proven at creation and stable for the
    ///    record's life (strings are immutable, Cons→Str flattening is
    ///    irreversible, heap slots don't move); the unit length IS the byte
    ///    length of the one subject borrow the matcher needs anyway.
    ///  - the per-step `ensure_regexp_ascii_twin` probe: the twin field is
    ///    monotonic (only ever set, never cleared — clearing needs a user
    ///    `compile()`, impossible here), so the matcher heap.get the search
    ///    performs anyway doubles as the twin check; the build runs at most
    ///    once, cold, then every later step sees `Some`.
    ///  - `regexp_write_last_index`'s `arr_props` probe: the engine-internal
    ///    matcher can never gain a `lastIndex` attribute entry or a freeze
    ///    marker (both need a user reference), so the throwing form is
    ///    unreachable — the heap slot is written directly (debug-asserted).
    ///    The write-through itself is REQUIRED even though no user reads the
    ///    matcher: a mid-iteration `RegExp.prototype.exec` swap fails the
    ///    per-step memo and the fallback resumes from this heap slot.
    ///  - the caller's result-array empty-match probe: the search knew
    ///    `mstart == mend`; it is returned instead of re-derived from
    ///    element 0.
    ///
    /// The Annex-B statics deferral and the result build are VERBATIM the
    /// shared impl's — per-step statics stay observable (`RegExp.$1` after a
    /// matchAll iteration) and the result array is byte-identical. The
    /// `prof::enter` mark and the `gc_lock_guard` are kept (~2ns each; the
    /// guard's removal is a separate ablation — it is provably safe today
    /// but a landmine if a future edit re-enters the interpreter here).
    ///
    /// Nothing in here can throw (the two throwing steps of the full impl —
    /// ToLength on an object and the non-writable lastIndex Set — are the
    /// elided ones), so the return is a plain pair: `(NULL, None)` for no
    /// match, else the result array plus `Some(match end)` iff the match was
    /// EMPTY — the caller's AdvanceStringIndex trigger.
    fn regexp_exec_fused_slim(
        &mut self,
        re_idx: u32,
        input_val: Value,
        fbits: u8,
        li_v: Value,
    ) -> (Value, Option<usize>) {
        let _prof = crate::vm::prof::enter(crate::vm::prof::Phase::RegexExec);
        // The result pieces below live in Rust locals — hold GC off until we
        // return, exactly as the shared impl does.
        let _gc = self.gc_lock_guard();
        debug_assert!(li_v.is_number(), "the fused-step guard admits numbers only");
        // ToLength on an engine-written number: truncate toward zero, floor
        // at 0, cap 2^53-1 — `to_integer_or_zero` + clamp without the
        // observable-valueOf valve (unreachable for a number).
        let li = {
            let d = li_v.as_f64().trunc();
            let d = if d.is_nan() { 0.0 } else { d };
            d.max(0.0).min(((1u64 << 53) - 1) as f64) as usize
        };
        let global = fbits & ITFB_GLOBAL != 0;
        let sticky = fbits & ITFB_STICKY != 0;
        let has_indices = fbits & ITFB_INDICES != 0;
        let stateful = global || sticky;
        // Step 9: a non-global, non-sticky regex always searches from 0
        // (unreachable today — ITFB_FUSED implies `g` — but kept parallel).
        let start = if stateful { li } else { 0 };
        let s_idx = input_val.heap_index();
        // The matcher fetch the search needs anyway doubles as the twin
        // probe; `built_twin` bounds the cold build at one attempt so a
        // (impossible today) non-RegExp slot cannot loop.
        let mut built_twin = false;
        let (found, subj_units) = loop {
            let subj: &str = match self.heap.get(s_idx) {
                HeapObj::Str(js) => {
                    debug_assert!(js.is_ascii(), "ITFB_FUSED encodes a flat-ASCII subject");
                    js.as_str_wf()
                }
                _ => "",
            };
            // ASCII: the unit length IS the byte length — no `str_units` get.
            let subj_units = subj.len();
            if start > subj_units {
                break (None, subj_units);
            }
            match self.heap.get(re_idx) {
                HeapObj::RegExp { ascii_twin: Some(Some(twin)), .. } => {
                    break (twin.find_from_ascii(subj, start).next(), subj_units);
                }
                // Twin compile failed once: the base program is byte-safe too.
                HeapObj::RegExp { ascii_twin: Some(None), regex, .. } => {
                    break (regex.find_from_ascii(subj, start).next(), subj_units);
                }
                HeapObj::RegExp { ascii_twin: None, .. } if !built_twin => {}
                HeapObj::RegExp { regex, .. } => {
                    break (regex.find_from_ascii(subj, start).next(), subj_units);
                }
                _ => break (None, subj_units),
            }
            // Cold, at most once per matcher: build (or record the failure
            // of) the byte-optimized twin, then re-enter with it in place —
            // `ascii_twin` is monotonic, so the next pass takes a `Some` arm.
            built_twin = true;
            self.ensure_regexp_ascii_twin(re_idx);
        };
        // Sticky: the match must begin exactly at the search start.
        let found = found.filter(|m| !(sticky && m.start() != start));
        let m = match found {
            Some(m) => m,
            None => {
                if stateful {
                    // Set(R,"lastIndex",0,true) — the direct form of
                    // `regexp_write_last_index`'s fast path (see the doc
                    // above for why the slow form is unreachable).
                    debug_assert!(self.arr_props.get(&re_idx).is_none());
                    if let HeapObj::RegExp { last_index, .. } = self.heap.get_mut(re_idx) {
                        *last_index = Value::int(0);
                    }
                }
                return (Value::NULL, None);
            }
        };
        let (mstart, mend) = (m.start(), m.end());
        if stateful {
            // Set(R,"lastIndex",e,true) — spec step 15, BEFORE the result
            // construction; direct write, same argument as above.
            debug_assert!(self.arr_props.get(&re_idx).is_none());
            if let HeapObj::RegExp { last_index, .. } = self.heap.get_mut(re_idx) {
                *last_index = Value::num(mend as f64);
            }
        }
        // Annex-B legacy statics: refreshed by EVERY successful
        // RegExpBuiltinExec, fused steps included — `RegExp.$1`/`lastMatch`
        // after a matchAll iteration are observable global state. VERBATIM
        // the shared impl's ASCII deferral arm (the subject here is always
        // ASCII); the length bound keeps the `as u32` casts exact.
        if subj_units <= u32::MAX as usize {
            let mut ranges: [Option<(u32, u32)>; 13] = [None; 13];
            ranges[0] = Some((mstart as u32, mend as u32));
            ranges[1] = m
                .captures
                .iter()
                .rev()
                .find_map(|c| c.clone())
                .map(|r| (r.start as u32, r.end as u32));
            ranges[2] = Some((0, mstart as u32));
            ranges[3] = Some((mend as u32, subj_units as u32));
            for i in 0..9 {
                ranges[4 + i] =
                    m.captures.get(i).and_then(|c| c.clone()).map(|r| (r.start as u32, r.end as u32));
            }
            if self.regexp_last.len() == 14 {
                self.regexp_last[0] = input_val;
            } else {
                self.regexp_last.clear();
                self.regexp_last.push(input_val);
                self.regexp_last.resize(14, Value::UNDEFINED);
            }
            self.regexp_last_lazy =
                Some(RegexpLastLazy { subj: input_val, subj_idx: s_idx, ranges });
        } else {
            // A >4GB flat subject cannot defer through u32 ranges: the shared
            // impl's eager arm, specialised to its ASCII half.
            let empty = self.alloc_str(String::new());
            let mut rec = Vec::with_capacity(14);
            rec.push(input_val);
            let whole_units = self.ascii_slice_value(s_idx, mstart..mend);
            rec.push(whole_units);
            rec.push(match m.captures.iter().rev().find_map(|c| c.clone()) {
                Some(r) => self.ascii_slice_value(s_idx, r),
                None => empty,
            });
            rec.push(self.ascii_slice_value(s_idx, 0..mstart));
            rec.push(self.ascii_slice_value(s_idx, mend..subj_units));
            for i in 0..9 {
                rec.push(match m.captures.get(i).and_then(|c| c.clone()) {
                    Some(r) => self.ascii_slice_value(s_idx, r),
                    None => empty,
                });
            }
            self.regexp_last = rec;
            self.regexp_last_lazy = None;
        }
        // The result build — VERBATIM the shared impl's, with `mk` collapsed
        // to its ASCII arm (the subject is proven ASCII).
        let whole = self.ascii_slice_value(s_idx, mstart..mend);
        let mut elems = Vec::with_capacity(1 + m.captures.len());
        elems.push(whole);
        for cap in &m.captures {
            let v = match cap {
                Some(r) => self.ascii_slice_value(s_idx, r.clone()),
                None => Value::UNDEFINED,
            };
            elems.push(v);
        }
        let named: Vec<(String, Option<std::ops::Range<usize>>)> =
            m.named_groups().map(|(n, r)| (n.to_string(), r)).collect();
        let groups = if named.is_empty() {
            Value::UNDEFINED
        } else {
            let mut gm = ObjMap::with_capacity(named.len());
            for (name, r) in &named {
                let v = match r {
                    Some(r) => self.ascii_slice_value(s_idx, r.clone()),
                    None => Value::UNDEFINED,
                };
                gm.set(name, v);
            }
            let gidx = self.heap.alloc(HeapObj::Object(Box::new(gm)));
            // The groups object is OrdinaryObjectCreate(null) — no prototype.
            self.proto_of.insert(gidx, Value::NULL);
            Value::heap(gidx)
        };
        let arr_idx = self.heap.alloc(HeapObj::Array(elems));
        let index_v = Value::num(mstart as f64);
        // index/input/groups are real own data properties of the result array
        // (writable, enumerable, configurable) so reflection sees them.
        let attr = PropAttr {
            writable: true,
            enumerable: true,
            configurable: true,
            accessor: false,
            setter: Value::UNDEFINED,
        };
        // `/d` (hasIndices): an `indices` array of [start,end] unit ranges for
        // the whole match + each capture group, with `.groups` for named groups.
        let indices_v = if has_indices {
            let mk = |vm: &mut Self, r: &std::ops::Range<usize>| -> Value {
                let s = Value::num(r.start as f64);
                let e = Value::num(r.end as f64);
                Value::heap(vm.heap.alloc(HeapObj::Array(vec![s, e])))
            };
            let mut idx_elems = vec![mk(self, &(mstart..mend))];
            for cap in &m.captures {
                idx_elems.push(match cap {
                    Some(r) => mk(self, r),
                    None => Value::UNDEFINED,
                });
            }
            let idx_groups = if named.is_empty() {
                Value::UNDEFINED
            } else {
                let mut gm = ObjMap::new();
                for (name, r) in &named {
                    let v = match r {
                        Some(r) => mk(self, r),
                        None => Value::UNDEFINED,
                    };
                    gm.set(name, v);
                }
                let gidx = self.heap.alloc(HeapObj::Object(Box::new(gm)));
                self.proto_of.insert(gidx, Value::NULL);
                Value::heap(gidx)
            };
            let indices_arr = self.heap.alloc(HeapObj::Array(idx_elems));
            self.arr_props.entry(indices_arr).or_insert_with(ObjMap::new_side_table).define(
                "groups",
                idx_groups,
                attr,
            );
            Value::heap(indices_arr)
        } else {
            Value::UNDEFINED
        };
        // Fresh slot; both side tables are pruned against the mark bits
        // before a slot can be recycled (same argument as the shared impl).
        debug_assert!(!self.arr_props.contains_key(&arr_idx));
        debug_assert!(!self.regexp_result_props.contains_key(&arr_idx));
        self.regexp_result_props.insert(
            arr_idx,
            RegexpResultProps {
                values: [index_v, input_val, groups, indices_v],
            },
        );
        rxstats::count_compact();
        if !match_variant_enabled() {
            // Off-switch: reproduce the eager representation (an ordinary
            // `ObjMap` in `arr_props`) so the compact form is A/B-able.
            self.materialize_regexp_result_props(arr_idx);
        }
        (Value::heap(arr_idx), (mstart == mend).then_some(mend))
    }

    /// Allocate the string for a byte-range slice of the (all-ASCII, flat)
    /// subject string at `s_idx` — for ASCII, byte offsets are unit offsets.
    /// Materialise the deferred Annex B legacy statics (slots 2..=13 — lastParen,
    /// leftContext, rightContext, `$1`..`$9`) into `regexp_last`, if a successful
    /// ASCII match left them as ranges.
    ///
    /// All twelve are done at once and the record cleared, so the cost is paid
    /// once per match no matter how many statics are read. A read of these is rare
    /// enough that splitting it per slot would only add a bitmask to the hot
    /// producer. `None` ranges are the empty string, exactly as the eager form
    /// pushed `empty`.
    ///
    /// Callers: `REGEXP_LEGACY_GET` for any slot >= 2. Slots 0/1 never defer.
    pub(crate) fn regexp_last_materialise(&mut self) {
        // COPY the record out and clear it only AFTER the slicing. `take()`ing it
        // up front would unroot `subj` for the duration — `ascii_slice_value`
        // allocates, and an allocation that trips `gc_requested` must not be able
        // to reach a collection while the only reference to the subject is a local.
        // `regexp_last[0]` usually roots it too, but not always: `RegExp.input = x`
        // overwrites slot 0 while the ranges still point at the old subject.
        let Some(lazy) = self.regexp_last_lazy.as_ref() else {
            return;
        };
        let subj_idx = lazy.subj_idx;
        let ranges = lazy.ranges;
        if self.regexp_last.len() < 14 {
            // A `RegExp.input = x` write with no prior match resizes to 14; this
            // only guards the impossible ordering rather than indexing blind.
            self.regexp_last.resize(14, Value::UNDEFINED);
        }
        for (i, r) in ranges.iter().enumerate() {
            self.regexp_last[1 + i] = match *r {
                Some((s, e)) => self.ascii_slice_value(subj_idx, s as usize..e as usize),
                None => self.alloc_str(String::new()),
            };
        }
        self.regexp_last_lazy = None;
    }

    pub(crate) fn ascii_slice_value(&mut self, s_idx: u32, r: std::ops::Range<usize>) -> Value {
        // W11 (B124): a slice of a KNOWN-ASCII subject is ascii by
        // construction — `from_ascii` skips `from_wtf8`'s linear rescan
        // (~1.8M slices/run on regex-log-scan). Non-ascii subjects keep the
        // full canonicalizing path.
        fn ascii_slice_fast() -> bool {
            use std::sync::atomic::{AtomicU8, Ordering};
            static STATE: AtomicU8 = AtomicU8::new(0);
            match STATE.load(Ordering::Relaxed) {
                1 => true,
                2 => false,
                _ => {
                    let on = std::env::var_os("ZIPP_NO_ASCII_SLICE_FAST").is_none();
                    STATE.store(if on { 1 } else { 2 }, Ordering::Relaxed);
                    on
                }
            }
        }
        let (bytes, subject_ascii): (Vec<u8>, bool) = match self.heap.get(s_idx) {
            HeapObj::Str(js) => (js.as_bytes()[r].to_vec(), js.is_ascii() && ascii_slice_fast()),
            _ => (Vec::new(), false),
        };
        let js = if subject_ascii {
            crate::heap::JsStr::from_ascii(bytes)
        } else {
            crate::heap::JsStr::from_wtf8(bytes)
        };
        Value::heap(self.heap.alloc_js(js))
    }

    /// `Set(R, "lastIndex", v, true)` on a real RegExp. Fast path writes the
    /// heap slot directly when nothing can make the Set observable or fail:
    /// no arr_props entry for the object (so no attr override and no freeze
    /// marker) or one without a `lastIndex` key and not frozen. Otherwise the
    /// full set_prop runs (a non-writable lastIndex must throw).
    pub(crate) fn regexp_write_last_index(&mut self, re_idx: u32, v: Value) -> Result<(), Thrown> {
        let fast = match self.arr_props.get(&re_idx) {
            None => true,
            Some(m) => !m.frozen && m.pos("lastIndex").is_none(),
        };
        if fast {
            if let HeapObj::RegExp { last_index, .. } = self.heap.get_mut(re_idx) {
                *last_index = v;
            }
            Ok(())
        } else {
            self.set_prop(Value::heap(re_idx), "lastIndex", v, true)?;
            Ok(())
        }
    }

    /// True when the RegExpExec PROTOCOL's `Get(R, "exec")` is UNOBSERVABLE
    /// and yields the intrinsic for instance `re`: its [[Prototype]] is
    /// exactly %RegExp.prototype%, it has no own `exec`, and the prototype's
    /// `exec` is still the intrinsic native data property. The drivers
    /// (@@match/@@replace/@@split/matchAll/exec_abstract) may then call
    /// `regexp_exec` directly, skipping the Get + generic call dispatch.
    /// The eight per-flag accessor names, in the canonical order
    /// `get RegExp.prototype.flags` reads them.
    const FLAG_ACCESSORS: [(&'static str, u16); 8] = [
        ("hasIndices", native::REGEXP_GET_HASINDICES),
        ("global", native::REGEXP_GET_GLOBAL),
        ("ignoreCase", native::REGEXP_GET_IGNORECASE),
        ("multiline", native::REGEXP_GET_MULTILINE),
        ("dotAll", native::REGEXP_GET_DOTALL),
        ("unicode", native::REGEXP_GET_UNICODE),
        ("unicodeSets", native::REGEXP_GET_UNICODESETS),
        ("sticky", native::REGEXP_GET_STICKY),
    ];

    /// Canonical flag characters, index-parallel to [`Vm::FLAG_ACCESSORS`].
    const FLAG_CHARS: [char; 8] = ['d', 'g', 'i', 'm', 's', 'u', 'v', 'y'];

    /// `Some(flags)` when reading `receiver.flags` provably reduces to the internal
    /// flag string: `re` is a real RegExp, `receiver` IS that object (not a
    /// `Reflect.get` with a foreign receiver), its `[[Prototype]]` is
    /// %RegExp.prototype%, it shadows none of the eight flag names, and each of those
    /// eight on the prototype is still its intrinsic ACCESSOR.
    ///
    /// The internal string is stored **as the program wrote it**, NOT canonicalised —
    /// `new RegExp("a", "ig")` keeps `"ig"` — so the result is rebuilt in canonical
    /// `dgimsuvy` order by membership test, exactly as the eight reads would. Returning
    /// the raw field was the first version of this and it was a conformance regression
    /// (`"ig"` where node says `"gi"`); `tests/regexp_flags_fast_path.rs` diffs the two
    /// paths over all 192 legal flag combinations and both spellings of each, which is
    /// what caught it.
    ///
    /// Eight `contains` scans of a ≤8-byte string replace eight full property
    /// traversals, so the shortcut still stands.
    pub(crate) fn regexp_pristine_flags(&self, re: u32, receiver: Value) -> Option<String> {
        if !receiver.is_heap() || receiver.heap_index() != re {
            return None;
        }
        let flags = match self.heap.get(re) {
            HeapObj::RegExp { flags, .. } => flags.clone(),
            _ => return None,
        };
        match self.proto_of.get(&re) {
            None => {}
            Some(p) if p.is_heap() && p.heap_index() == self.regexp_proto => {}
            _ => return None,
        }
        // An own shadow of any flag name (or of `flags` itself) makes the reads
        // observable again.
        if let Some(m) = self.arr_props.get(&re) {
            if m.pos("flags").is_some()
                || Self::FLAG_ACCESSORS.iter().any(|(n, _)| m.pos(n).is_some())
            {
                return None;
            }
        }
        let proto = match self.heap.get(self.regexp_proto) {
            HeapObj::Object(m) => m,
            _ => return None,
        };
        let mut out = String::with_capacity(8);
        for (i, (name, want)) in Self::FLAG_ACCESSORS.iter().enumerate() {
            let ok = proto.pos(name).is_some_and(|p| {
                proto.attrs[p].accessor
                    && proto.vals[p].is_heap()
                    && matches!(self.heap.get(proto.vals[p].heap_index()),
                                HeapObj::Native(n) if n == want)
            });
            if !ok {
                return None;
            }
            // Canonical `dgimsuvy` order, by membership — the stored string is in
            // SOURCE order, so it cannot be returned as-is.
            let ch = Self::FLAG_CHARS[i];
            if flags.as_bytes().contains(&(ch as u8)) {
                out.push(ch);
            }
        }
        Some(out)
    }

    /// True when `re.<name>` provably resolves to the intrinsic native `want`, so the
    /// receiver-kind builtin fast path may serve it inline instead of going through
    /// `get_prop` + `call_value`.
    ///
    /// This is the OVERRIDE-SAFE guard the other receiver-kind arms lack. B68 measured
    /// that `String.prototype.indexOf = f; "abc".indexOf("b")` still answers `1` in zipp
    /// against node's override, because those arms bind a builtin from its NAME alone;
    /// RegExp was correct only because it had no arm at all. So an arm may be added here
    /// ONLY behind a check that the name still reaches the intrinsic — all three ways it
    /// could stop doing so:
    ///
    /// * the instance's `[[Prototype]]` is no longer %RegExp.prototype% (a subclass, or
    ///   `setPrototypeOf`);
    /// * the instance has an OWN `name` shadowing the prototype;
    /// * %RegExp.prototype%'s `name` slot no longer holds the intrinsic native — it was
    ///   reassigned, deleted, or turned into an accessor.
    ///
    /// Deliberately NOT cached behind a version: B67 established that `ObjMap::set`
    /// bumps the heap version only when a key is ADDED (`if added`), so a plain
    /// `RegExp.prototype.test = f` overwrite would leave a version-keyed cache stale and
    /// silently reinstate the bug this guard exists to prevent. The uncached form is
    /// affordable — B68's ablation put the near-identical `regexp_exec_fast_ok` at ~7% of
    /// the call, while the generic path this skips is the bulk of it.
    pub(crate) fn regexp_method_is_intrinsic(&self, re: u32, name: &str, want: u16) -> bool {
        match self.proto_of.get(&re) {
            None => {}
            Some(p) if p.is_heap() && p.heap_index() == self.regexp_proto => {}
            _ => return false,
        }
        if self.arr_props.get(&re).is_some_and(|m| m.pos(name).is_some()) {
            return false;
        }
        match self.heap.get(self.regexp_proto) {
            HeapObj::Object(m) => m.pos(name).is_some_and(|i| {
                !m.attrs[i].accessor
                    && m.vals[i].is_heap()
                    && matches!(self.heap.get(m.vals[i].heap_index()),
                                HeapObj::Native(n) if *n == want)
            }),
            _ => false,
        }
    }

    pub(crate) fn regexp_exec_fast_ok(&self, re: u32) -> bool {
        match self.proto_of.get(&re) {
            None => {}
            Some(p) if p.is_heap() && p.heap_index() == self.regexp_proto => {}
            _ => return false,
        }
        if self.arr_props.get(&re).is_some_and(|m| m.pos("exec").is_some()) {
            return false;
        }
        match self.heap.get(self.regexp_proto) {
            HeapObj::Object(m) => m.pos("exec").is_some_and(|i| {
                !m.attrs[i].accessor
                    && m.vals[i].is_heap()
                    && matches!(self.heap.get(m.vals[i].heap_index()),
                                HeapObj::Native(n) if *n == native::REGEXP_EXEC)
            }),
            _ => false,
        }
    }

    /// Ensure the BYTE-OPTIMIZED twin compile (`HeapObj::RegExp::ascii_twin`)
    /// for the RegExp at `re_idx` exists — built once, lazily, from the SAME
    /// pattern characters and flags as the heap regex (mirrors
    /// `build_regexp`, incl. the exact-bytes lone-surrogate form). A failed
    /// compile is recorded as `Some(None)` so it isn't retried; callers fall
    /// back to `find_from_ascii` on the unoptimized program (also byte-safe).
    fn ensure_regexp_ascii_twin(&mut self, re_idx: u32) {
        let (source, flags) = match self.heap.get(re_idx) {
            // Already computed (twin or recorded failure): nothing to do.
            HeapObj::RegExp { ascii_twin: Some(_), .. } => return,
            HeapObj::RegExp { source, flags, .. } => (source.clone(), flags.clone()),
            _ => return,
        };
        let rflags: String = flags.chars().filter(|c| "imsuv".contains(*c)).collect();
        let unicode_mode = flags.contains('u') || flags.contains('v');
        let compile_cps: Vec<u32> = match (self.regexp_exact_source.get(&re_idx), unicode_mode) {
            (Some(b), true) => crate::heap::wtf8_code_points(b).collect(),
            (Some(b), false) => nonunicode_pattern_chars(
                &crate::heap::wtf8_units_iter(b).collect::<Vec<u16>>(),
            ),
            (None, true) => source.chars().map(u32::from).collect(),
            (None, false) => nonunicode_pattern_chars(
                &source.encode_utf16().collect::<Vec<u16>>(),
            ),
        };
        // Through the byteopt half of the compile cache (species clones of
        // the same pattern share one twin too).
        let cache_key = self
            .regexp_exact_source
            .get(&re_idx)
            .is_none()
            // The cache key owns its text, so the shared source is materialised
            // here — once per twin build, never per match.
            .then(|| (source.to_string(), rflags.clone(), true));
        let twin: Option<std::sync::Arc<regress::Regex>> =
            match cache_key.as_ref().and_then(|k| self.regex_compile_cache.get(k)) {
                Some(rc) => Some(rc.clone()),
                None => {
                    let compiled =
                        regress::Regex::from_unicode_byteopt(compile_cps.iter().copied(), rflags.as_str())
                            .ok()
                            .map(std::sync::Arc::new);
                    if let (Some(k), Some(rc)) = (cache_key, compiled.as_ref()) {
                        if self.regex_compile_cache.len() >= 512 {
                            self.regex_compile_cache.clear();
                        }
                        self.regex_compile_cache.insert(k, rc.clone());
                    }
                    compiled
                }
            };
        if let HeapObj::RegExp { ascii_twin, .. } = self.heap.get_mut(re_idx) {
            *ascii_twin = Some(twin);
        }
    }

    /// The string's UTF-16 code units — EXACT: an astral scalar yields its two
    /// halves and a lone surrogate its own 0xD800–0xDFFF value (which is what
    /// lets a `\uD800` pattern match a real lone-surrogate subject). `v` must
    /// be a string value (callers come through `to_str_value`).
    pub(crate) fn value_units(&mut self, v: Value) -> Vec<u16> {
        if !v.is_heap() {
            return Vec::new();
        }
        let idx = v.heap_index();
        self.heap.flatten(idx);
        match self.heap.get(idx) {
            HeapObj::Str(js) if js.is_ascii() => js.as_bytes().iter().map(|&b| b as u16).collect(),
            HeapObj::Str(js) => js.units_iter().collect(),
            _ => Vec::new(),
        }
    }

    /// Allocate the string for a unit-slice of a subject — built as WTF-8 so
    /// lone surrogates round-trip exactly and a covered (high, low) pair
    /// recombines into its astral scalar (canonical form).
    pub(crate) fn units_value(&mut self, units: &[u16]) -> Value {
        let mut out: Vec<u8> = Vec::with_capacity(units.len() * 3);
        push_units(&mut out, units);
        Value::heap(self.heap.alloc_js(crate::heap::JsStr::from_wtf8(out)))
    }

    /// One %RegExpStringIterator%.next step for the iterator at `it_idx`:
    /// `Some((value, done))` when it IS a lazy regexp-string iterator (else
    /// `None` — not one). Runs ONE RegExpExec (via the abstract protocol,
    /// honouring a user `exec`). A null result, or the single match of a
    /// non-global regex, latches done; a global empty match advances
    /// lastIndex (spec AdvanceStringIndex: +1 unit, +2 over an astral
    /// surrogate pair when the iterator's fullUnicode bit is set) so the
    /// next step makes progress. Shared by the ITER_NEXT native (which wraps
    /// the pair in a `{value, done}` object) and the `IterNext` for-of fast
    /// path (which consumes the pair directly — the result object an
    /// intrinsic `next` would build is engine-internal and its `done`/`value`
    /// Gets are unobservable).
    pub(crate) fn regexp_string_iter_step(
        &mut self,
        it_idx: u32,
    ) -> Option<Result<(Value, bool), Thrown>> {
        let &(regexp, string, fbits, done) = self.regexp_string_iters.get(&it_idx)?;
        if fbits & ITFB_FUSED != 0 && !done && matchall_step_enabled() {
            if let Some(r) = self.regexp_string_iter_step_fused(it_idx, regexp, string, fbits) {
                return Some(r);
            }
        }
        Some(self.regexp_string_iter_step_inner(it_idx, regexp, string, fbits, done))
    }

    /// The fused pristine matchAll STEP (B118). Reaching here requires the
    /// `ITFB_FUSED` bit, which only the pristine-clone creation arm sets: the
    /// matcher is an ENGINE-INTERNAL clone no user reference exists to (so
    /// its own shape, prototype link, flags and `lastIndex` writability were
    /// proven once, at creation, and cannot change), and the subject is a
    /// flat-ASCII string (immutable, so the bit stays true).
    ///
    /// What CAN change mid-iteration is the shared %RegExp.prototype% — a
    /// replaced `exec` must be honoured per STEP. That is exactly the memo
    /// `matchall_fast_from_slots` version-guards (its `exec` pin re-reads the
    /// slot's value identity every call, which is what catches the
    /// no-version-bump in-place `RegExp.prototype.exec = f` write — B67).
    /// Any mismatch returns `None` and the caller runs the full observable
    /// step; a stale memo is refreshed by the next `matchAll()` call, never
    /// here (a per-step re-resolve would put the nine-probe gate back on the
    /// hot path for permanently-polluted programs).
    ///
    /// With the guards holding, the step is: one RegExpBuiltinExec with the
    /// flag bits pre-decoded from the iterator record (no per-step
    /// `flags.contains` scans, no exec-protocol re-derivation), the dense
    /// element-0 empty-match probe, and the +1 AdvanceStringIndex an ASCII
    /// subject admits (no surrogate pairs to skip).
    fn regexp_string_iter_step_fused(
        &mut self,
        it_idx: u32,
        regexp: u32,
        string: Value,
        fbits: u8,
    ) -> Option<Result<(Value, bool), Thrown>> {
        if !self.matchall_fast_from_slots() {
            rxstats::count_step_full();
            return None;
        }
        // The matcher's `lastIndex` only ever holds the numbers this path and
        // the builtin exec write, but the bit costs nothing to re-check and
        // turns "engine invariant" into "guard". The Value is EXTRACTED here
        // (B124): the slim entry takes it as a parameter instead of paying a
        // second heap.get + ToInteger for the identical slot.
        let li_v = match self.heap.get(regexp) {
            HeapObj::RegExp { last_index, .. } if last_index.is_number() => *last_index,
            _ => {
                rxstats::count_step_full();
                return None;
            }
        };
        if slim_exec_enabled() {
            // B124 slim entry: one infallible call returns the result array
            // plus the empty-match fact the probe below re-derived from the
            // just-built array's element 0.
            let (r, empty_end) = self.regexp_exec_fused_slim(regexp, string, fbits, li_v);
            rxstats::count_step_fused();
            if r == Value::NULL {
                if let Some(e) = self.regexp_string_iters.get_mut(&it_idx) {
                    e.3 = true;
                }
                return Some(Ok((Value::UNDEFINED, true)));
            }
            // The old path probed the just-built array's element 0 for
            // emptiness; the fold to the returned flag is observation-free
            // ONLY because element 0 is exactly subject[mstart..mend] — the
            // assertion anchors that equivalence.
            debug_assert_eq!(
                empty_end.is_some(),
                match self.heap.get(r.heap_index()) {
                    HeapObj::Array(items) => matches!(
                        items.first(),
                        Some(v) if v.is_heap() && self.heap.str_units(v.heap_index()) == Some(0)
                    ),
                    _ => false,
                },
                "slim empty-match flag must agree with the element-0 probe"
            );
            if let Some(end) = empty_end {
                // `lastIndex` was just written by the exec (== the match
                // end); ASCII subject ⇒ the advance is exactly +1.
                self.set_regexp_last_index(regexp, end + 1);
            }
            return Some(Ok((r, false)));
        }
        // ZIPP_NO_SLIM_EXEC=1: the pre-B124 step, byte-for-byte.
        let r = match self.regexp_exec_impl_prebits(regexp, string, true, Some(fbits)) {
            Ok(r) => r,
            Err(t) => return Some(Err(t)),
        };
        rxstats::count_step_fused();
        if r == Value::NULL {
            if let Some(e) = self.regexp_string_iters.get_mut(&it_idx) {
                e.3 = true;
            }
            return Some(Ok((Value::UNDEFINED, true)));
        }
        // Empty match ⇒ AdvanceStringIndex. Element 0 is the string the
        // builtin exec just built (pristine builder, dense store, no user
        // code ran since) — read it directly.
        let empty = match self.heap.get(r.heap_index()) {
            HeapObj::Array(items) => matches!(
                items.first(),
                Some(v) if v.is_heap() && self.heap.str_units(v.heap_index()) == Some(0)
            ),
            _ => false,
        };
        if empty {
            // `lastIndex` was just written by the exec above (a number, ==
            // the match end); ASCII subject ⇒ the advance is exactly +1.
            let cur = match self.heap.get(regexp) {
                HeapObj::RegExp { last_index, .. } => last_index.as_f64().max(0.0) as usize,
                _ => 0,
            };
            self.set_regexp_last_index(regexp, cur + 1);
        }
        Some(Ok((r, false)))
    }

    fn regexp_string_iter_step_inner(
        &mut self,
        it_idx: u32,
        regexp: u32,
        string: Value,
        fbits: u8,
        done: bool,
    ) -> Result<(Value, bool), Thrown> {
        let global = fbits & 1 != 0;
        let full_unicode = fbits & 2 != 0;
        let (value, ret_done, latch) = if done {
            (Value::UNDEFINED, true, true)
        } else {
            // Captured BEFORE the exec: `regexp_exec_fast_ok` proves the result
            // array is the one RegExpBuiltinExec builds, so element 0 can be read
            // from the dense store below. A user `exec` can return anything, and
            // could also install one between iterations, so it is re-checked
            // every step rather than cached on the iterator.
            let pristine_exec = matches!(self.heap.get(regexp), HeapObj::RegExp { .. })
                && self.regexp_exec_fast_ok(regexp);
            // `regexp_exec_abstract` opens by re-proving exactly
            // `pristine_exec` to pick the builtin — when it already holds,
            // call the builtin directly instead of proving it twice.
            let r = if pristine_exec {
                self.regexp_exec(regexp, string)?
            } else {
                self.regexp_exec_abstract(regexp, string)?
            };
            if r == Value::NULL {
                (Value::UNDEFINED, true, true)
            } else if !global {
                (r, false, true)
            } else {
                // Was the match EMPTY? Only then does lastIndex need advancing.
                //
                // The generic `get_index` path must still account for arguments
                // mapping, element overlays, holes, and a custom prototype. When
                // we built the array ourselves (real RegExp, pristine `exec`, so
                // no user code could have replaced element 0 with a getter), read
                // `items[0]` straight out of the dense store instead.
                let fast0 = pristine_exec.then(|| match self.heap.get(r.heap_index()) {
                    HeapObj::Array(items) => items.first().copied(),
                    _ => None,
                });
                let m0 = match fast0 {
                    Some(Some(v)) => v,
                    _ => self.get_index(r, Value::int(0))?,
                };
                // ToString(Get(match,"0")) — IDENTITY for a string value (no
                // copy); only a non-string coerces.
                let m0v = self.to_str_value(m0)?;
                if self.heap.str_units(m0v.heap_index()) == Some(0) {
                    let cur_v = self.get_prop(Value::heap(regexp), "lastIndex")?;
                    // ToLength(Get(R,"lastIndex")) — a throwing
                    // lastIndex.valueOf must propagate, not be swallowed; the
                    // 2^53-1 clamp applies BEFORE the advance.
                    let cur = self.to_integer_or_zero(cur_v)?.clamp(0, (1i64 << 53) - 1) as usize;
                    let next = self.advance_index_on_value(string, cur, full_unicode);
                    self.set_regexp_last_index(regexp, next);
                }
                (r, false, false)
            }
        };
        if latch != done {
            if let Some(e) = self.regexp_string_iters.get_mut(&it_idx) {
                e.3 = latch;
            }
        }
        Ok((value, ret_done))
    }

    /// AdvanceStringIndex reading the units from heap string `s` (for the lazy
    /// matchAll driver, which doesn't keep an encoded unit buffer around).
    pub(crate) fn advance_index_on_value(&mut self, s: Value, index: usize, unicode: bool) -> usize {
        if unicode && s.is_heap() {
            self.heap.flatten(s.heap_index());
            if let HeapObj::Str(js) = self.heap.get(s.heap_index()) {
                if let (Some(hi), Some(lo)) = (js.unit_at(index), js.unit_at(index + 1)) {
                    if (0xD800..=0xDBFF).contains(&hi) && (0xDC00..=0xDFFF).contains(&lo) {
                        return index + 2;
                    }
                }
            }
        }
        index + 1
    }

    /// Regex-backed `String.prototype.replace`/`replaceAll`. `repl` is a function
    /// (called `(match, ...groups, offset, input)`) or a template string (`$&`/`$N`/…).
    /// `s_idx` is the receiver string's heap index. All positions are UTF-16
    /// unit indices; the output is assembled as WTF-8 so the subject's lone
    /// surrogates (and a functional replacer's) round-trip exactly.
    pub(crate) fn regex_replace(
        &mut self,
        s_idx: u32,
        re: u32,
        repl: Value,
        global: bool,
    ) -> Result<Value, Thrown> {
        // ASCII subject: match in place over the bytes (offsets == unit
        // indices), no Vec<u16> encode — see `regexp_exec` for why the ASCII
        // backend is semantically identical here.
        self.heap.flatten(s_idx);
        if matches!(self.heap.get(s_idx), HeapObj::Str(js) if js.is_ascii()) {
            return self.regex_replace_ascii(s_idx, re, repl, global);
        }
        // Encode the subject ONCE; every regress range below indexes into it.
        let u16s: Vec<u16> = self.value_units(Value::heap(s_idx));
        let matches: Vec<regress::Match> = match self.heap.get(re) {
            HeapObj::RegExp { regex, flags, .. } => {
                let unicode = flags.contains('u') || flags.contains('v');
                match (unicode, global) {
                    (true, true) => regex.find_from_utf16(&u16s, 0).collect(),
                    (true, false) => regex.find_from_utf16(&u16s, 0).next().into_iter().collect(),
                    (false, true) => regex.find_from_ucs2(&u16s, 0).collect(),
                    (false, false) => regex.find_from_ucs2(&u16s, 0).next().into_iter().collect(),
                }
            }
            _ => Vec::new(),
        };
        // IsCallable(replaceValue) — the full predicate, not just a compiled
        // Func/Closure: a bound function, a native, a class, or a Proxy of any
        // of them is a functional replacer too, and testing only the two
        // compiled shapes ToString'd it into a literal template instead.
        let callable = self.is_callable(repl);
        let repl_str = if callable { String::new() } else { self.to_js_string(repl)? };
        // No match ⇒ the result is the subject unchanged (T0.4): return it as-is,
        // after the observable `ToString(replaceValue)` above, skipping the full
        // subject copy/rebuild. Strings are immutable, so the same heap value is
        // observably identical to a fresh copy.
        if matches.is_empty() {
            return Ok(Value::heap(s_idx));
        }
        let mut out: Vec<u8> = Vec::new();
        let mut last = 0usize;
        for m in &matches {
            let (st, en) = (m.start(), m.end());
            if st < last {
                continue;
            }
            push_units(&mut out, &u16s[last..st]);
            if callable {
                let whole = self.units_value(&u16s[m.range()]);
                let mut argv = vec![whole];
                for cap in &m.captures {
                    argv.push(match cap {
                        Some(r) => self.units_value(&u16s[r.clone()]),
                        None => Value::UNDEFINED,
                    });
                }
                argv.push(Value::num(st as f64));
                argv.push(Value::heap(s_idx));
                // RegExp.prototype[@@replace] step 14.k.iv: when the regex has named
                // capture groups, a `groups` object (OrdinaryObjectCreate(null)) is
                // the FINAL replacer argument. (Mirrors the exec/array path above.)
                let named: Vec<(String, Option<std::ops::Range<usize>>)> =
                    m.named_groups().map(|(n, r)| (n.to_string(), r)).collect();
                if !named.is_empty() {
                    let mut gm = ObjMap::new();
                    for (name, r) in &named {
                        let v = match r {
                            Some(r) => self.units_value(&u16s[r.clone()]),
                            None => Value::UNDEFINED,
                        };
                        gm.set(name, v);
                    }
                    let gidx = self.heap.alloc(HeapObj::Object(Box::new(gm)));
                    self.proto_of.insert(gidx, Value::NULL);
                    argv.push(Value::heap(gidx));
                }
                let r = self.call_value(repl, Value::UNDEFINED, &argv)?;
                // ToString(result) — exact bytes (a returned lone-surrogate
                // string keeps its surrogate; `wtf8_push` canonicalizes the seam).
                let rv = self.to_str_value(r)?;
                let bytes = self
                    .heap
                    .str_wtf8_cow(rv.heap_index())
                    .map(|c| c.into_owned())
                    .unwrap_or_default();
                if bytes.len() > (1usize << 28).saturating_sub(out.len()) {
                    return Err(Thrown("RangeError: Invalid string length".into()));
                }
                crate::heap::wtf8_push(&mut out, &bytes);
            } else {
                // GetSubstitution over LOSSY views (the template + captures come
                // through ToString); positions stay unit-exact either way.
                let whole = String::from_utf16_lossy(&u16s[m.range()]);
                let groups: Vec<Option<String>> = m
                    .captures
                    .iter()
                    .map(|c| c.as_ref().map(|r| String::from_utf16_lossy(&u16s[r.clone()])))
                    .collect();
                let named: Vec<(String, Option<String>)> = m
                    .named_groups()
                    .map(|(n, r)| (n.to_string(), r.map(|r| String::from_utf16_lossy(&u16s[r]))))
                    .collect();
                let rep = self.expand_replacement(
                    &repl_str,
                    &whole,
                    &groups,
                    &named,
                    !named.is_empty(),
                    &String::from_utf16_lossy(&u16s[..st]),
                    &String::from_utf16_lossy(&u16s[en..]),
                    (1usize << 28).saturating_sub(out.len()),
                )?;
                crate::heap::wtf8_push(&mut out, rep.as_bytes());
            }
            last = en;
        }
        push_units(&mut out, &u16s[last..]);
        Ok(Value::heap(self.heap.alloc_js(crate::heap::JsStr::from_wtf8(out))))
    }

    /// `regex_replace` for an all-ASCII subject: regress `find_from_ascii`
    /// over the heap bytes in place (byte offsets == unit offsets), output
    /// assembled from byte slices. Functional replacements still append their
    /// EXACT WTF-8 bytes (a replacer may return lone surrogates), so the
    /// output buffer stays WTF-8.
    fn regex_replace_ascii(
        &mut self,
        s_idx: u32,
        re: u32,
        repl: Value,
        global: bool,
    ) -> Result<Value, Thrown> {
        self.ensure_regexp_ascii_twin(re);
        let matches: Vec<regress::Match> = {
            let subj: &str = match self.heap.get(s_idx) {
                HeapObj::Str(js) => js.as_str_wf(),
                _ => "",
            };
            let regex: Option<&regress::Regex> = match self.heap.get(re) {
                HeapObj::RegExp { ascii_twin: Some(Some(twin)), .. } => Some(twin),
                HeapObj::RegExp { regex, .. } => Some(regex),
                _ => None,
            };
            match regex {
                Some(regex) => {
                    if global {
                        regex.find_from_ascii(subj, 0).collect()
                    } else {
                        regex.find_from_ascii(subj, 0).next().into_iter().collect()
                    }
                }
                None => Vec::new(),
            }
        };
        // IsCallable(replaceValue) — the full predicate, not just a compiled
        // Func/Closure: a bound function, a native, a class, or a Proxy of any
        // of them is a functional replacer too, and testing only the two
        // compiled shapes ToString'd it into a literal template instead.
        let callable = self.is_callable(repl);
        let repl_str = if callable { String::new() } else { self.to_js_string(repl)? };
        // No match ⇒ the result is the subject unchanged (T0.4): return it as-is,
        // after the observable `ToString(replaceValue)`, skipping the subject
        // memcpy + rebuild. (~46% of the regex bench's section-3 lines have no
        // `//` and hit this.)
        if matches.is_empty() {
            return Ok(Value::heap(s_idx));
        }
        // Own the subject (one memcpy) so the heap allocs below can't
        // invalidate the borrow; ASCII ⇒ valid UTF-8, sliceable as &str.
        let subject: String = match self.heap.get(s_idx) {
            HeapObj::Str(js) => js.as_str_wf().to_string(),
            _ => String::new(),
        };
        let mut out: Vec<u8> = Vec::with_capacity(subject.len() + 16);
        let mut last = 0usize;
        for m in &matches {
            let (st, en) = (m.start(), m.end());
            if st < last {
                continue;
            }
            out.extend_from_slice(subject[last..st].as_bytes());
            if callable {
                let whole = self.alloc_str(subject[m.range()].to_string());
                let mut argv = vec![whole];
                for cap in &m.captures {
                    argv.push(match cap {
                        Some(r) => self.alloc_str(subject[r.clone()].to_string()),
                        None => Value::UNDEFINED,
                    });
                }
                argv.push(Value::num(st as f64));
                argv.push(Value::heap(s_idx));
                // RegExp.prototype[@@replace] step 14.k.iv: a `groups` object
                // (OrdinaryObjectCreate(null)) as the FINAL replacer argument
                // when the regex has named capture groups.
                let named: Vec<(String, Option<std::ops::Range<usize>>)> =
                    m.named_groups().map(|(n, r)| (n.to_string(), r)).collect();
                if !named.is_empty() {
                    let mut gm = ObjMap::new();
                    for (name, r) in &named {
                        let v = match r {
                            Some(r) => self.alloc_str(subject[r.clone()].to_string()),
                            None => Value::UNDEFINED,
                        };
                        gm.set(name, v);
                    }
                    let gidx = self.heap.alloc(HeapObj::Object(Box::new(gm)));
                    self.proto_of.insert(gidx, Value::NULL);
                    argv.push(Value::heap(gidx));
                }
                let r = self.call_value(repl, Value::UNDEFINED, &argv)?;
                // ToString(result) — exact bytes (a returned lone-surrogate
                // string keeps its surrogate; `wtf8_push` canonicalizes the seam).
                let rv = self.to_str_value(r)?;
                let bytes = self
                    .heap
                    .str_wtf8_cow(rv.heap_index())
                    .map(|c| c.into_owned())
                    .unwrap_or_default();
                if bytes.len() > (1usize << 28).saturating_sub(out.len()) {
                    return Err(Thrown("RangeError: Invalid string length".into()));
                }
                crate::heap::wtf8_push(&mut out, &bytes);
            } else {
                // GetSubstitution directly over &str slices of the subject.
                let groups: Vec<Option<String>> = m
                    .captures
                    .iter()
                    .map(|c| c.as_ref().map(|r| subject[r.clone()].to_string()))
                    .collect();
                let named: Vec<(String, Option<String>)> = m
                    .named_groups()
                    .map(|(n, r)| (n.to_string(), r.map(|r| subject[r].to_string())))
                    .collect();
                let rep = self.expand_replacement(
                    &repl_str,
                    &subject[m.range()],
                    &groups,
                    &named,
                    !named.is_empty(),
                    &subject[..st],
                    &subject[en..],
                    (1usize << 28).saturating_sub(out.len()),
                )?;
                crate::heap::wtf8_push(&mut out, rep.as_bytes());
            }
            last = en;
        }
        out.extend_from_slice(subject[last..].as_bytes());
        Ok(Value::heap(self.heap.alloc_js(crate::heap::JsStr::from_wtf8(out))))
    }

    // ── TypedArrays / ArrayBuffer / DataView ──

}

/// The pattern characters fed to the regress parser for a NON-`u`/`v` regex,
/// from the pattern's UTF-16 units. The spec grammar reads such a pattern per
/// CODE UNIT — an astral literal is its two surrogate halves, each its own
/// pattern character (so it matches over UCS-2 subject units) — EXCEPT inside
/// RegExpIdentifierName (a group name `(?<name>…)` / `\k<name>`), where
/// surrogate pairs recombine into code points. Tracks character-class nesting
/// so a literal `(?<` / `\k<` inside `[...]` stays plain units.
pub(crate) fn nonunicode_pattern_chars(units: &[u16]) -> Vec<u32> {
    let mut out: Vec<u32> = Vec::with_capacity(units.len());
    let mut i = 0usize;
    let mut in_class = false;
    let mut in_name = false;
    while i < units.len() {
        let u = units[i] as u32;
        if in_name {
            if u == '>' as u32 {
                in_name = false;
                out.push(u);
                i += 1;
            } else if (0xD800..=0xDBFF).contains(&units[i])
                && i + 1 < units.len()
                && (0xDC00..=0xDFFF).contains(&units[i + 1])
            {
                let (hi, lo) = (units[i] as u32, units[i + 1] as u32);
                out.push(0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00));
                i += 2;
            } else {
                out.push(u);
                i += 1;
            }
            continue;
        }
        match u {
            // An escape: copy `\` + the next unit verbatim (so `\[`/`\]` can't
            // flip the class state), EXCEPT `\k<` outside a class, which opens
            // a group-name reference.
            0x5C => {
                out.push(u);
                if i + 1 < units.len() {
                    let n = units[i + 1] as u32;
                    out.push(n);
                    if !in_class
                        && n == 'k' as u32
                        && i + 2 < units.len()
                        && units[i + 2] == '<' as u16
                    {
                        out.push('<' as u32);
                        in_name = true;
                        i += 3;
                        continue;
                    }
                    i += 2;
                } else {
                    i += 1;
                }
            }
            0x5B if !in_class => {
                in_class = true;
                out.push(u);
                i += 1;
            }
            0x5D if in_class => {
                in_class = false;
                out.push(u);
                i += 1;
            }
            // `(?<` not followed by `=`/`!` (lookbehinds) opens a group name.
            0x28 if !in_class
                && i + 2 < units.len()
                && units[i + 1] == '?' as u16
                && units[i + 2] == '<' as u16
                && units.get(i + 3).map_or(true, |&n| n != '=' as u16 && n != '!' as u16) =>
            {
                out.extend_from_slice(&['(' as u32, '?' as u32, '<' as u32]);
                in_name = true;
                i += 3;
            }
            _ => {
                out.push(u);
                i += 1;
            }
        }
    }
    out
}

/// Append `units` onto WTF-8 buffer `out` — exact (`wtf8_push_cp`
/// canonicalizes an adjacent (high, low) pair back into its astral scalar).
pub(crate) fn push_units(out: &mut Vec<u8>, units: &[u16]) {
    for &u in units {
        crate::heap::wtf8_push_cp(out, u as u32);
    }
}

/// AdvanceStringIndex (ES 22.2.7.3): +1 code UNIT, or +2 when `unicode`
/// (the `u`/`v` flags) and `index` sits on a high surrogate directly followed
/// by a low surrogate (one astral code point).
pub(crate) fn advance_string_index(units: &[u16], index: usize, unicode: bool) -> usize {
    if unicode
        && index + 1 < units.len()
        && (0xD800..=0xDBFF).contains(&units[index])
        && (0xDC00..=0xDFFF).contains(&units[index + 1])
    {
        index + 2
    } else {
        index + 1
    }
}

/// `ZIPP_NO_MATCH_VARIANT=1` restores the eager `arr_props` `ObjMap` for every
/// match result: the compact record is built and then immediately materialised,
/// so the old representation (and its cost) is reproduced exactly. Exists so
/// the compact form is A/B-able and bisectable on one binary, same as
/// `ZIPP_NO_ENUM_HOIST`.
#[inline]
pub(crate) fn match_variant_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let v = std::env::var_os("ZIPP_NO_MATCH_VARIANT").is_none() as u8;
            ON.store(v, Ordering::Relaxed);
            v == 1
        }
    }
}

/// `ZIPP_RXSTATS=1` — how many match results were CONSTRUCTED in the compact
/// record vs how many were later MATERIALISED into an ordinary `arr_props`
/// `ObjMap` (by mutation/reflection — or by `ZIPP_NO_MATCH_VARIANT=1`, which
/// materialises every one at construction). A workload that only reads
/// `m[i]`/`m.index`/`m.input`/`m.groups` should show near-zero materialisations.
/// Off, this costs one relaxed atomic load per event.
pub(crate) mod rxstats {
    use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};

    static ON: AtomicU8 = AtomicU8::new(2);
    static COMPACT: AtomicU64 = AtomicU64::new(0);
    static MATERIALIZED: AtomicU64 = AtomicU64::new(0);
    static STEP_FUSED: AtomicU64 = AtomicU64::new(0);
    static STEP_FULL: AtomicU64 = AtomicU64::new(0);

    #[inline]
    pub(crate) fn enabled() -> bool {
        match ON.load(Ordering::Relaxed) {
            0 => false,
            1 => true,
            _ => {
                let v = std::env::var_os("ZIPP_RXSTATS").is_some() as u8;
                ON.store(v, Ordering::Relaxed);
                v == 1
            }
        }
    }

    #[inline]
    pub(crate) fn count_compact() {
        if enabled() {
            COMPACT.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline]
    pub(crate) fn count_materialized() {
        if enabled() {
            MATERIALIZED.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// A %RegExpStringIterator% step served by the fused pristine path
    /// (B118): flag bits from the iterator record, no per-step protocol
    /// re-proof beyond the version-guarded slot memo.
    #[inline]
    pub(crate) fn count_step_fused() {
        if enabled() {
            STEP_FUSED.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// A fused-ELIGIBLE step that fell back to the full observable protocol
    /// (memo cold/invalidated, or a guard declined).
    #[inline]
    pub(crate) fn count_step_full() {
        if enabled() {
            STEP_FULL.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// `(compact_constructions, materialized, steps_fused, steps_fallback)`.
    pub fn dump() -> (u64, u64, u64, u64) {
        (
            COMPACT.load(Ordering::Relaxed),
            MATERIALIZED.load(Ordering::Relaxed),
            STEP_FUSED.load(Ordering::Relaxed),
            STEP_FULL.load(Ordering::Relaxed),
        )
    }
}

/// Assemble a RegExp `flags` string in the canonical order `dgimsuvy`,
/// regardless of the order the flags were supplied in.
pub(crate) fn canonical_flags(flags: &str) -> String {
    let mut out = String::new();
    for ch in ['d', 'g', 'i', 'm', 's', 'u', 'v', 'y'] {
        if flags.contains(ch) {
            out.push(ch);
        }
    }
    out
}
