//! A constructor whose body only stores its arguments and constants into
//! `this` — `constructor(x, y) { this.x = x; this.y = y; }`, `function
//! Node(v) { this.v = v; this.next = null; }`, the commonest constructor
//! there is — answered from compiled code without running the body.
//!
//! A compiled `new` of such a constructor made an empty instance, ran the
//! body as a call (frame-free or on a frame) whose `SetProp`s each went
//! through the store miss helper's add way, and grew the instance's value
//! vector on the way: a call, two helper calls and three allocations for an
//! object a literal builds in one. When each store is provably an ordinary
//! append — no level of the chain a fresh instance would have (the class's
//! member tables, then plain objects; or the function's `prototype`, then
//! plain objects) holds a setter, a read-only property or an exotic link
//! for any stored name — the instance is born finished instead, the way an
//! object literal is (`alloc_finalized`: the shape every append would have
//! reached, that shape's shared key list, the values), and then given its
//! class or its [[Prototype]]. The proof is the add way's, made once and
//! kept with the versions it read; a `new` whose chain has moved proves
//! again, and one that cannot be proved makes the instance and runs the
//! body as before. Only a class with no instance fields comes here (a field
//! initializer is user code that runs on the instance first).

#![allow(unused_imports)]
use super::*;
use crate::bytecode::StaticKeyPlan;

/// Where one store's value comes from.
#[derive(Clone, Copy, Debug)]
pub(crate) enum StoreSrc {
    /// The `i`th argument (`undefined` past the call's count).
    Arg(u16),
    /// A constant that is not a heap value (a number, a boolean, `null`,
    /// `undefined`).
    Bits(u64),
}

/// What the scan found for one constructor body, and the proof of the chain
/// it last answered for.
#[derive(Default)]
pub(crate) struct CtorStore {
    /// `None`: the body is not store-only.
    keys: Option<std::rc::Rc<[(String, StoreSrc)]>>,
    proof: Option<std::rc::Rc<StoreProof>>,
    /// The last receiver kind the proof failed for — its class (or
    /// [[Prototype]]), the class's version and `class_proto_epoch` — so a
    /// class that cannot be proved (one with instance fields, a setter on
    /// its chain) does not walk its chain at every `new`. A chain that later
    /// clears only stays on the body's path until one of those moves.
    declined: Option<(u32, u32, u32, u32)>,
}

/// The add way's proof for every store of a body into a fresh instance of
/// one class (or with one [[Prototype]]): valid while the class's version,
/// `class_proto_epoch` and the version of each chain level read are
/// unchanged.
#[derive(Debug)]
pub(crate) struct StoreProof {
    /// The instance's class, `u32::MAX` for a plain function's instance.
    class: u32,
    /// A plain instance's [[Prototype]] (`u32::MAX` for none).
    proto: u32,
    class_ver: u32,
    epoch: u32,
    hops: Vec<(u32, u32)>,
    /// The shape after the last store and its shared key list.
    shape: u32,
    plan: StaticKeyPlan,
}

/// The most stores a body may make here: the shared key lists stop at 16.
const STORE_MAX: usize = 16;

/// `ZIPP_NO_CTOR_STORE=1` runs every constructor's body again (the
/// same-binary A/B).
fn ctor_store_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let v = std::env::var_os("ZIPP_NO_CTOR_STORE").is_none() as u8;
            ON.store(v, Ordering::Relaxed);
            v == 1
        }
    }
}

/// A name a computed or exotic write could intercept: the store miss
/// helper's exclusions (`jit_set_prop_miss`), plus the empty name and an
/// escaped well-known symbol.
fn store_key_ok(key: &str) -> bool {
    !(key.is_empty()
        || key == "__proto__"
        || key == "caller"
        || key == "arguments"
        || key.starts_with("@@")
        || key
            .as_bytes()
            .first()
            .is_some_and(|b| b.is_ascii_digit() || *b == b'-' || *b == b'#'))
}

/// The stores of a body that is nothing else: arguments or non-heap
/// constants moved into registers, `SetProp`s of them into `this` (register
/// 0) under distinct names, and a final `return`. Anything else in the body
/// (a read, a call, a branch, a default or rest parameter, `arguments`) and
/// the scan finds nothing.
fn scan_store_body(p: &crate::bytecode::FuncProto) -> Option<Vec<(String, StoreSrc)>> {
    if p.is_generator
        || p.is_async
        || p.lexical_this
        || p.rest_reg.is_some()
        || p.arguments_reg.is_some()
    {
        return None;
    }
    let mut src: Vec<Option<StoreSrc>> = vec![None; p.reg_count as usize];
    for i in 0..p.param_count {
        *src.get_mut(i as usize + 1)? = Some(StoreSrc::Arg(i));
    }
    let mut keys: Vec<(String, StoreSrc)> = Vec::new();
    let last = p.code.len().checked_sub(1)?;
    for (pc, ins) in p.code.iter().enumerate() {
        let (dst, v) = match *ins {
            Instr::Move { dst, src: s } => (dst, *src.get(s as usize)?),
            Instr::LoadInt { dst, val } => (dst, Some(StoreSrc::Bits(Value::int(val).bits()))),
            Instr::LoadNull { dst } => (dst, Some(StoreSrc::Bits(Value::NULL.bits()))),
            Instr::LoadUndefined { dst } => (dst, Some(StoreSrc::Bits(Value::UNDEFINED.bits()))),
            Instr::LoadBool { dst, val } => (dst, Some(StoreSrc::Bits(Value::bool(val).bits()))),
            Instr::LoadConst { dst, idx } => {
                let c = *p.constants.get(idx as usize)?;
                if c.is_heap() {
                    return None; // a string (an interning sentinel) or a heap constant
                }
                (dst, Some(StoreSrc::Bits(c.bits())))
            }
            Instr::SetProp { obj: 0, name, val, .. } => {
                let key = p.string_constants.get(name as usize)?;
                let v = (*src.get(val as usize)?)?;
                if !store_key_ok(key) || keys.len() == STORE_MAX || keys.iter().any(|(k, _)| k == key) {
                    return None;
                }
                keys.push((key.clone(), v));
                continue;
            }
            Instr::ReturnUndefined if pc == last && !keys.is_empty() => return Some(keys),
            _ => return None,
        };
        if dst == 0 {
            return None;
        }
        *src.get_mut(dst as usize)? = Some(v?);
    }
    None
}

/// No heap index is ever `u32::MAX`, so a lookup of it in `proto_of` misses,
/// as one for an instance not yet made does.
const NO_INSTANCE: u32 = u32::MAX;

impl<'p> Vm<'p> {
    /// `new` of the store-only constructor body `fid` from compiled code,
    /// for an instance of the class `class` (a base class with no instance
    /// fields) or, for a plain function (`class` = `u32::MAX`), with the
    /// [[Prototype]] `proto`: the finished instance, its stores made from
    /// the arguments in the caller's window (`argc` of them at `arg_base`).
    /// `None`: nothing was done and the caller makes the instance and runs
    /// the body. Never on a metered run (the body's steps are charged).
    pub(crate) fn ctor_store_new(
        &mut self,
        fid: u32,
        class: u32,
        proto: Value,
        base: usize,
        arg_base: u16,
        argc: u16,
    ) -> Option<Value> {
        #[cfg(feature = "instrument")]
        if self.jit.metered() {
            return None;
        }
        if !ctor_store_enabled() {
            return None;
        }
        let (keys, proof) = match self.ctor_stores.get(&fid) {
            Some(e) => (e.keys.clone(), e.proof.clone()),
            None => {
                let keys: Option<std::rc::Rc<[(String, StoreSrc)]>> =
                    scan_store_body(self.func(fid as usize)).map(std::rc::Rc::from);
                self.ctor_stores.insert(fid, CtorStore { keys: keys.clone(), ..CtorStore::default() });
                (keys, None)
            }
        };
        let keys = keys?;
        // A plain function's instance always gets its [[Prototype]] recorded
        // (a non-object `prototype` falls back to %Object.prototype% first).
        let proto_idx = if class != u32::MAX {
            u32::MAX
        } else if proto.is_heap() {
            proto.heap_index()
        } else {
            return None;
        };
        let proof = match proof {
            Some(p) if self.store_proof_holds(&p, class, proto_idx) => p,
            _ => {
                let class_ver = if class != u32::MAX { self.heap.version_of(class) } else { 0 };
                let kind = (class, proto_idx, class_ver, self.class_proto_epoch);
                if self.ctor_stores.get(&fid).is_some_and(|e| e.declined == Some(kind)) {
                    return None;
                }
                let Some(p) = self.prove_stores(class, proto_idx, &keys) else {
                    if let Some(e) = self.ctor_stores.get_mut(&fid) {
                        e.declined = Some(kind);
                    }
                    return None;
                };
                let p = std::rc::Rc::new(p);
                if let Some(e) = self.ctor_stores.get_mut(&fid) {
                    e.proof = Some(p.clone());
                }
                p
            }
        };
        // The construction the body's call was: a safe point, before the
        // allocation (the arguments are in the caller's window, a root).
        self.maybe_gc();
        let mut vals = [Value::UNDEFINED; STORE_MAX];
        for (i, (_, src)) in keys.iter().enumerate() {
            vals[i] = match *src {
                StoreSrc::Arg(a) if a < argc => self.get(base, arg_base + a),
                StoreSrc::Arg(_) => Value::UNDEFINED,
                StoreSrc::Bits(b) => Value::from_bits(b),
            };
        }
        let idx = self.heap.alloc_finalized(&proof.plan, &vals[..keys.len()], proof.shape);
        if class != u32::MAX {
            if let HeapObj::Object(m) = self.heap.get_mut(idx) {
                m.class = Some(class);
            }
        } else if proto.is_heap() {
            self.proto_of.insert(idx, proto);
        }
        Some(Value::heap(idx))
    }

    /// Whether `p` still proves the stores for a fresh instance of `class`
    /// (or with the [[Prototype]] `proto_idx`).
    fn store_proof_holds(&self, p: &StoreProof, class: u32, proto_idx: u32) -> bool {
        if p.class != class || p.proto != proto_idx || self.class_proto_epoch != p.epoch {
            return false;
        }
        if class != u32::MAX && self.heap.version_of(class) != p.class_ver {
            return false;
        }
        p.hops.iter().all(|&(h, v)| self.heap.version_of(h) == v)
    }

    /// The add way's proof, for each stored name in turn, for a fresh
    /// instance: the chain (`class_add_chain` for a class instance; plain
    /// objects from `proto_idx` otherwise, as the store miss helper walks
    /// them) holds no setter, no read-only property and no exotic level for
    /// any of the names. Then the shape the appends reach and its shared key
    /// list, which must exist (a list past the caps keeps the body).
    fn prove_stores(
        &mut self,
        class: u32,
        proto_idx: u32,
        keys: &[(String, StoreSrc)],
    ) -> Option<StoreProof> {
        if !self.deferred_ns_state.is_empty() {
            return None;
        }
        let mut hops: Vec<(u32, u32)> = Vec::new();
        if class != u32::MAX {
            // Instance fields run on the instance before the body; such a
            // class keeps the body's path. A class's fields are fixed when
            // it is made, so this holds for as long as the proof does.
            if self.class_body_fn(Value::heap(class), true).is_some() {
                return None;
            }
            let mut buf = [(0u32, 0u32); 16];
            for (k, _) in keys {
                let n = self.class_add_chain(NO_INSTANCE, class, k, &mut buf)?;
                hops.extend_from_slice(&buf[..n]);
            }
        } else {
            for (k, _) in keys {
                let mut next = proto_idx;
                for depth in 0.. {
                    if next == u32::MAX {
                        break; // a null prototype: the chain's end
                    }
                    if depth == 64 || !self.ic_obj_ok(next) {
                        return None;
                    }
                    hops.push((next, self.heap.version_of(next)));
                    match self.heap.get(next) {
                        HeapObj::Object(m2) if !m2.is_ctor && m2.class.is_none() => {
                            if let Some(i) = m2.pos(k) {
                                let a = m2.attr_at(i);
                                if a.accessor || !a.writable {
                                    return None;
                                }
                                break; // a writable data property: the own add shadows it
                            }
                        }
                        _ => return None, // an exotic level
                    }
                    next = match self.proto_of.get_copied(next) {
                        Some(p) if p.is_heap() => p.heap_index(),
                        Some(_) => u32::MAX,
                        None if self.obj_proto == 0 || next == self.obj_proto => u32::MAX,
                        None => self.obj_proto,
                    };
                }
            }
        }
        hops.sort_unstable();
        hops.dedup();
        let mut shape = crate::shape::EMPTY;
        for (k, _) in keys {
            shape = crate::shape::add(shape, k, crate::shape::attr_bits(true, true, true, false));
            if shape == crate::shape::DICT {
                return None;
            }
        }
        let plan = crate::vm::helpers_misc::shape_key_plan(self, shape)?;
        if plan.len() != keys.len() {
            return None;
        }
        Some(StoreProof {
            class,
            proto: proto_idx,
            class_ver: if class != u32::MAX { self.heap.version_of(class) } else { 0 },
            epoch: self.class_proto_epoch,
            hops,
            shape,
            plan,
        })
    }
}
