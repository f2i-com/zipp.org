//! Lexical environments (scope chains).
//!
//! A `Scope` is a frame of variable bindings plus a link to its parent; closures
//! capture an `Rc<RefCell<Scope>>` so they keep their defining environment alive
//! and observe later mutations (capture by reference, like JS).

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::rc::Rc;

use crate::value::JsValue;

/// FxHash — a fast non-cryptographic hash (the variant rustc itself uses). The
/// default `HashMap` uses SipHash, which is DoS-resistant but slow; variable
/// names are short, trusted, and looked up on *every* identifier access, so a
/// cheap hash is a sizeable interpreter win. No external dependency.
#[derive(Default)]
pub struct FxHasher {
    hash: u64,
}

const SEED: u64 = 0x51_7c_c1_b7_27_22_0a_95;

impl Hasher for FxHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let mut h = self.hash;
        for &b in bytes {
            h = (h.rotate_left(5) ^ (b as u64)).wrapping_mul(SEED);
        }
        self.hash = h;
    }
    #[inline]
    fn finish(&self) -> u64 {
        self.hash
    }
}

/// A variable map keyed by name, using the fast FxHash hasher.
pub type VarMap = HashMap<String, JsValue, BuildHasherDefault<FxHasher>>;

/// A frame past this many bindings is promoted from the linear `Vec` to a hashed
/// map. Activation records and blocks almost always hold a handful of bindings,
/// so a linear scan with no hashing wins; only large frames like the global
/// scope (which holds every builtin) pay for the map.
const PROMOTE_AT: usize = 16;

/// Variable storage for one scope frame, sized to the binding count:
/// - `Empty`: zero bindings — a freshly-created activation/block scope. NO heap
///   allocation at all (the common case for a function called with its body not
///   yet executed, and for blocks with no `let`).
/// - `One`: exactly one binding — the dominant activation-record shape (one
///   param, or one `let`). Inline; avoids a `Vec` allocation entirely.
/// - `Small`: a linear-scan `Vec` for a handful of bindings.
/// - `Map`: the FxHash map for the global scope and any frame past `PROMOTE_AT`.
enum VarStore {
    Empty,
    One(Box<str>, JsValue),
    Small(Vec<(Box<str>, JsValue)>),
    Map(VarMap),
}

impl VarStore {
    #[inline]
    fn get(&self, name: &str) -> Option<JsValue> {
        match self {
            VarStore::Empty => None,
            VarStore::One(k, v) => (&**k == name).then(|| v.clone()),
            VarStore::Small(v) => v.iter().find(|(k, _)| &**k == name).map(|(_, val)| val.clone()),
            VarStore::Map(m) => m.get(name).cloned(),
        }
    }

    /// Overwrite an existing binding; returns false if `name` isn't present here.
    fn set_existing(&mut self, name: &str, val: JsValue) -> bool {
        match self {
            VarStore::Empty => false,
            VarStore::One(k, v) => {
                if &**k == name {
                    *v = val;
                    true
                } else {
                    false
                }
            }
            VarStore::Small(v) => {
                if let Some(slot) = v.iter_mut().find(|(k, _)| &**k == name) {
                    slot.1 = val;
                    true
                } else {
                    false
                }
            }
            VarStore::Map(m) => {
                if let Some(slot) = m.get_mut(name) {
                    *slot = val;
                    true
                } else {
                    false
                }
            }
        }
    }

    /// Declare (or redeclare) a binding in this frame.
    fn declare(&mut self, name: &str, val: JsValue) {
        // First, the in-place cases that don't change the variant.
        match self {
            VarStore::Empty => {
                *self = VarStore::One(name.into(), val);
                return;
            }
            VarStore::One(k, v) if &**k == name => {
                *v = val;
                return;
            }
            VarStore::Small(v) => {
                if let Some(slot) = v.iter_mut().find(|(k, _)| &**k == name) {
                    slot.1 = val;
                    return;
                }
                if v.len() < PROMOTE_AT {
                    v.push((name.into(), val));
                    return;
                }
                // else: needs promotion to a Map — handled below.
            }
            VarStore::Map(m) => {
                m.insert(name.to_string(), val);
                return;
            }
            VarStore::One(_, _) => {} // different key — grow to Small, handled below
        }
        // Variant transitions that need ownership of `self`.
        match std::mem::replace(self, VarStore::Empty) {
            VarStore::One(ok, ov) => {
                *self = VarStore::Small(vec![(ok, ov), (name.into(), val)]);
            }
            VarStore::Small(mut v) => {
                let mut m = VarMap::default();
                for (k, val) in v.drain(..) {
                    m.insert(k.into(), val);
                }
                m.insert(name.to_string(), val);
                *self = VarStore::Map(m);
            }
            other => *self = other, // unreachable: the in-place match handled these
        }
    }
}

pub struct Scope {
    vars: VarStore,
    pub parent: Option<Rc<RefCell<Scope>>>,
}

impl Scope {
    pub fn global() -> Rc<RefCell<Scope>> {
        // The global scope holds every builtin, so start it as a map directly.
        Rc::new(RefCell::new(Scope { vars: VarStore::Map(VarMap::default()), parent: None }))
    }

    pub fn child(parent: &Rc<RefCell<Scope>>) -> Rc<RefCell<Scope>> {
        // Child frames start empty — no heap allocation until the first binding,
        // and a one-binding frame (the common activation record) stays inline.
        Rc::new(RefCell::new(Scope { vars: VarStore::Empty, parent: Some(parent.clone()) }))
    }

    /// Declare (or redeclare) a binding in this frame.
    pub fn declare(&mut self, name: &str, v: JsValue) {
        self.vars.declare(name, v);
    }
}

/// Look up `name`, walking up the scope chain. `None` if undeclared.
pub fn get(scope: &Rc<RefCell<Scope>>, name: &str) -> Option<JsValue> {
    let mut cur = scope.clone();
    loop {
        // One borrow per frame: read the var (returning if found) and grab the
        // parent link in the same borrow, so the parent `Rc` is cloned at most
        // once per level.
        let next = {
            let b = cur.borrow();
            if let Some(v) = b.vars.get(name) {
                return Some(v);
            }
            b.parent.clone()
        };
        match next {
            Some(p) => cur = p,
            None => return None,
        }
    }
}

/// Assign to the nearest existing binding named `name`. Returns `false` if no
/// such binding exists (the caller then creates an implicit global, per sloppy
/// mode).
pub fn set(scope: &Rc<RefCell<Scope>>, name: &str, v: JsValue) -> bool {
    let mut cur = scope.clone();
    loop {
        {
            let mut b = cur.borrow_mut();
            if b.vars.set_existing(name, v.clone()) {
                return true;
            }
        }
        let next = cur.borrow().parent.clone();
        match next {
            Some(p) => cur = p,
            None => return false,
        }
    }
}
