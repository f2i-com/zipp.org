//! Interned identifiers: a [`Sym`] is an index; equal names share one.

use rustc_hash::FxHashMap;
use std::borrow::Cow;

/// An interned name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Sym(pub u32);

/// Names of a module: slices of its source, or (for the dotted names of
/// imports) strings the parser built.
#[derive(Default)]
pub struct Interner<'s> {
    map: FxHashMap<&'s str, Sym>,
    owned: FxHashMap<Box<str>, Sym>,
    names: Vec<Cow<'s, str>>,
}

/// Names interned before any source, at fixed indices.
pub const MATCH: Sym = Sym(0);
pub const CASE: Sym = Sym(1);
pub const TYPE: Sym = Sym(2);
pub const UNDERSCORE: Sym = Sym(3);

impl<'s> Interner<'s> {
    pub fn new() -> Self {
        let mut interner = Interner::default();
        for name in ["match", "case", "type", "_"] {
            interner.intern(name);
        }
        interner
    }

    /// Intern a slice of the source.
    #[inline]
    pub fn intern(&mut self, name: &'s str) -> Sym {
        if let Some(&sym) = self.map.get(name) {
            return sym;
        }
        let sym = Sym(self.names.len() as u32);
        self.names.push(Cow::Borrowed(name));
        self.map.insert(name, sym);
        sym
    }

    /// Intern a name built elsewhere.
    pub fn intern_owned(&mut self, name: String) -> Sym {
        if let Some(&sym) = self.map.get(name.as_str()) {
            return sym;
        }
        if let Some(&sym) = self.owned.get(name.as_str()) {
            return sym;
        }
        let sym = Sym(self.names.len() as u32);
        self.owned.insert(name.clone().into_boxed_str(), sym);
        self.names.push(Cow::Owned(name));
        sym
    }

    #[inline]
    pub fn get(&self, sym: Sym) -> &str {
        &self.names[sym.0 as usize]
    }

    pub fn len(&self) -> usize {
        self.names.len()
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }
}
