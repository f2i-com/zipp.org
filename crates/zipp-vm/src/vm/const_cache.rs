//! Bounded interpreter cache for immutable string constants.
//!
//! Bytecode stores a string literal as a sentinel into its owning function's
//! immutable `string_constants` pool. Historically every interpreted
//! `LoadConst` cloned and heap-allocated that text again. A hot interpreted
//! helper therefore allocated the same literal once per call, even though a
//! JavaScript String is an immutable primitive and representation identity is
//! unobservable.
//!
//! The cache is deliberately narrower than general string interning:
//!
//! * the key is the unified `(func_id, constant-slot)` pair, so main code,
//!   loader modules and monotonically-installed eval functions cannot collide;
//! * only short literals are retained, and for `eval` / `new Function` code
//!   only a bounded number of slots/functions (past the first few thousand
//!   slots, only name-sized ones); the main program's and loader modules'
//!   literals are bounded by their source;
//! * any function containing a bytecode op licensed to mutate a unique string
//!   buffer is rejected wholesale. Such an op relies on a literal load being a
//!   fresh representation; the cache is a hidden alias, so sharing there would
//!   be unsound. All uncertain/new shapes keep the historical allocation path;
//! * cached Values are rooted by `Vm::mark_roots` and allocated through the
//!   ordinary heap, so GC stress and the embedder's heap high-water accounting
//!   see the real slot. The Rust-side tables/text duplication are capped below.

#![allow(unused_imports)]
use super::*;

/// At most this many primitive representations are retained per VM.
pub(super) const CONST_STRING_CACHE_MAX_ENTRIES: usize = 1 << 15;
/// Avoid duplicating arbitrarily large source literals into a permanent cache.
pub(super) const CONST_STRING_CACHE_MAX_BYTES: usize = 256;
/// Past this many entries only name-sized literals (at most
/// [`CONST_STRING_CACHE_NAME_BYTES`]) are cached. A large program (the
/// Python frontend's torch library executes some 8-16K string-constant
/// slots, nearly all attribute, key and op names) keeps one representation
/// per slot instead of allocating the name on every load past the first
/// few thousand; the entry caps bound retained string payload to
/// 4096 * 256 B + (32768 - 4096) * 32 B, under 2 MiB.
pub(super) const CONST_STRING_CACHE_WIDE_ENTRIES: usize = 4096;
/// See [`CONST_STRING_CACHE_WIDE_ENTRIES`].
pub(super) const CONST_STRING_CACHE_NAME_BYTES: usize = 32;
/// Functions whose cached slots are also kept in the dense mirror
/// (`Vm::const_string_dense`) whatever code they are; a runtime-installed
/// `eval` / `new Function` function past it keeps the map alone.
pub(super) const CONST_DENSE_FUNCS: usize = 4096;
/// Dynamic code installs monotonically increasing function ids. Bound the
/// accompanying eligibility memo too; beyond it we fail closed to fresh loads.
#[cfg(feature = "jit")]
const CONST_STRING_CACHE_MAX_FUNCTIONS: usize = 4096;

#[inline]
fn slot_key(func_id: u32, const_idx: u32) -> u64 {
    ((func_id as u64) << 32) | const_idx as u64
}

/// A cached literal is a hidden second reference to its heap string. Reject a
/// whole function if any compiler-licensed op may mutate an accumulator in
/// place. `StrConcatChain` is intentionally absent: its bytecode contract makes
/// the first link a plain `Add` and mutates only that fresh result, never a
/// `LoadConst` operand.
///
/// That is the rule where compiled code appends through a native cursor,
/// which a frozen accumulator sends back to the interpreter. A build without
/// the JIT freezes each literal it caches instead (`resolve_const_slot`):
/// every in-place grower declines a frozen accumulator and builds a fresh
/// string, so `let out = "seed"` copies once at the loop's first append,
/// where it was copied at the load, and `s += "ab" + i` stops making and
/// freeing an "ab" on every pass (a fifth of that loop).
#[cfg(feature = "jit")]
#[inline]
fn function_literals_shareable(f: &crate::bytecode::FuncProto) -> bool {
    !f.code.iter().any(|instr| {
        matches!(
            instr,
            Instr::StrAppendInPlace { .. }
                | Instr::StrAppendIndex { .. }
                | Instr::AddRightPair { in_place: true, .. }
        )
    })
}

impl Vm<'_> {
    /// Whether `func_id` is the main program's or a loader module's: code
    /// whose functions and constants are bounded by its source, unlike an
    /// `eval` / `new Function` stream's, so the caches' function caps need
    /// not bound it.
    fn const_func_source_bounded(&self, func_id: u32) -> bool {
        (func_id as usize) < self.main_func_count || self.loader_module_func(func_id)
    }

    #[cfg(not(feature = "jit"))]
    #[inline(always)]
    fn const_string_func_shareable(&mut self, _func_id: u32) -> bool {
        true
    }

    #[cfg(feature = "jit")]
    #[inline]
    fn const_string_func_shareable(&mut self, func_id: u32) -> bool {
        if let Some(&safe) = self.const_string_cache_funcs.get(&func_id) {
            return safe;
        }
        // Keep the VM-internal side table resource-bounded even under an eval
        // stream producing infinitely many distinct functions. Failure to memo
        // is also failure to cache, so an attacker cannot induce a repeated
        // whole-body scan at the bound. Code bounded by its source is not
        // capped: a bundle's literals past its 4096th function were loaded as
        // a fresh string every time.
        if self.const_string_cache_funcs.len() >= CONST_STRING_CACHE_MAX_FUNCTIONS
            && !self.const_func_source_bounded(func_id)
        {
            return false;
        }
        let safe = function_literals_shareable(self.func(func_id as usize));
        self.const_string_cache_funcs.insert(func_id, safe);
        safe
    }

    /// The cached representation of a string constant slot, if it has one:
    /// rooted (`Vm::mark_roots`) for the VM's life, so its bits name it for
    /// good.
    #[cfg(feature = "python")]
    #[inline]
    pub(crate) fn const_slot_cached(&self, func_id: u32, const_idx: u32) -> Option<Value> {
        self.const_string_cache.get(&slot_key(func_id, const_idx)).copied()
    }

    /// Resolve one bytecode constant slot, memoizing eligible string literals.
    /// Every rejection delegates verbatim to `resolve_const`, including lone-
    /// surrogate/WTF-8 decoding and the canonical `typeof` handles.
    #[inline]
    pub(crate) fn resolve_const_slot(&mut self, func_id: u32, const_idx: u32) -> Value {
        let raw = self.func(func_id as usize).constants[const_idx as usize];
        // A constant that is not a string is its own value. Answer it here:
        // `resolve_const` makes the same test and returns the same bits, but
        // it is a call, and it was 4% of a loop that adds a float constant.
        if !raw.is_heap() || (raw.heap_index() & crate::vm::helpers_misc::STRING_CONST_BIT) == 0 {
            return raw;
        }
        if !self.const_string_cache_enabled {
            return self.resolve_const(func_id, raw);
        }
        if let Some(&v) = self
            .const_string_dense
            .get(func_id as usize)
            .and_then(|f| f.get(const_idx as usize))
        {
            if !v.is_undefined() {
                return v;
            }
        }

        let string_idx = (raw.heap_index() & !crate::vm::helpers_misc::STRING_CONST_BIT) as usize;
        let source_bytes = self.func(func_id as usize).string_constants[string_idx].len();
        if source_bytes > CONST_STRING_CACHE_MAX_BYTES {
            return self.resolve_const(func_id, raw);
        }

        let key = slot_key(func_id, const_idx);
        if let Some(&cached) = self.const_string_cache.get(&key) {
            return cached;
        }
        // The entry caps bound what runtime code can make the cache retain;
        // a source-bounded function's literals are bounded by that source.
        let len = self.const_string_cache.len();
        let capped = len >= CONST_STRING_CACHE_MAX_ENTRIES
            || (len >= CONST_STRING_CACHE_WIDE_ENTRIES && source_bytes > CONST_STRING_CACHE_NAME_BYTES);
        if (capped && !self.const_func_source_bounded(func_id)) || !self.const_string_func_shareable(func_id) {
            return self.resolve_const(func_id, raw);
        }

        let resolved = self.resolve_const(func_id, raw);
        debug_assert!(
            resolved.is_heap(),
            "a string constant resolves to a heap value"
        );
        // A Python program's names are one heap string per text (`vm::py_rt`).
        #[cfg(feature = "python")]
        let resolved = {
            let text = self.func(func_id as usize).string_constants[string_idx].as_str();
            self.py_intern(text, resolved)
        };
        // Held by this cache and by every load it serves: no accumulator's
        // own (see `function_literals_shareable`).
        #[cfg(not(feature = "jit"))]
        self.heap.freeze_shared_str(resolved.heap_index());
        self.const_string_cache.insert(key, resolved);
        let f = func_id as usize;
        // Only `eval` / `new Function` code takes the cap. A bundle is one
        // program of thousands of functions, and every string load past the
        // first 4096 of them hashed into the map.
        if f < CONST_DENSE_FUNCS || self.const_func_source_bounded(func_id) {
            if self.const_string_dense.len() <= f {
                self.const_string_dense.resize(f + 1, Vec::new());
            }
            let n = self.func(f).constants.len();
            let slots = &mut self.const_string_dense[f];
            if slots.len() < n {
                slots.resize(n, Value::UNDEFINED);
            }
            if let Some(slot) = slots.get_mut(const_idx as usize) {
                *slot = resolved;
            }
        }
        resolved
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vm(source: &str) -> Vm<'static> {
        let ast = crate::front::parse_script(source).expect("source parses");
        let program = Box::leak(Box::new(
            crate::compile::compile_program(&ast, source).expect("source compiles"),
        ));
        let mut vm = Vm::new(program);
        vm.run().expect("program runs");
        vm
    }

    fn literal_slot(vm: &Vm<'_>, text: &str) -> (u32, u32) {
        for func_id in 0..vm.funcs.len() as u32 {
            let f = vm.func(func_id as usize);
            for (const_idx, &raw) in f.constants.iter().enumerate() {
                if !raw.is_heap()
                    || (raw.heap_index() & crate::vm::helpers_misc::STRING_CONST_BIT) == 0
                {
                    continue;
                }
                let string_idx =
                    (raw.heap_index() & !crate::vm::helpers_misc::STRING_CONST_BIT) as usize;
                if f.string_constants[string_idx] == text {
                    return (func_id, const_idx as u32);
                }
            }
        }
        panic!("literal slot not found: {text:?}");
    }

    #[test]
    fn eligible_slot_reuses_one_primitive_representation_and_off_switch_does_not() {
        let mut vm = vm(r#"function value() { return "cache-me-please"; }"#);
        let (func_id, const_idx) = literal_slot(&vm, "cache-me-please");
        vm.const_string_cache.clear();
        vm.const_string_dense.clear();

        let first = vm.resolve_const_slot(func_id, const_idx);
        let second = vm.resolve_const_slot(func_id, const_idx);
        assert_eq!(first, second);
        assert_eq!(vm.const_string_cache.len(), 1);

        vm.const_string_cache.clear();
        vm.const_string_dense.clear();
        vm.const_string_cache_enabled = false;
        let fresh_a = vm.resolve_const_slot(func_id, const_idx);
        let fresh_b = vm.resolve_const_slot(func_id, const_idx);
        assert_ne!(fresh_a, fresh_b);
        assert!(vm.const_string_cache.is_empty());
    }

    #[test]
    fn cached_literal_is_an_explicit_major_gc_root() {
        let mut vm = vm(r#"function value() { return "cache-root-check"; }"#);
        let (func_id, const_idx) = literal_slot(&vm, "cache-root-check");
        vm.const_string_cache.clear();
        vm.const_string_dense.clear();
        vm.regs.clear();

        let cached = vm.resolve_const_slot(func_id, const_idx);
        let slot = cached.heap_index();
        vm.heap.set_nursery(false);
        vm.gc_stress = true;
        vm.maybe_gc();

        assert!(!vm.heap.free_indices().contains(&slot));
        assert_eq!(vm.display(cached), "cache-root-check");
        assert_eq!(
            vm.const_string_cache.get(&slot_key(func_id, const_idx)),
            Some(&cached)
        );
    }

    #[cfg(feature = "jit")]
    #[test]
    fn unique_buffer_functions_keep_fresh_literals_across_calls() {
        let vm = vm(r#"
            function build(n) {
                let out = "seed";
                for (let i = 0; i < n; i++) out += i;
                return out;
            }
            console.log(build(2));
            console.log(build(1));
            "#);
        assert_eq!(vm.output, ["seed01", "seed0"]);

        let (func_id, const_idx) = literal_slot(&vm, "seed");
        assert!(vm.func(func_id as usize).code.iter().any(|instr| matches!(
            instr,
            Instr::StrAppendInPlace { .. }
                | Instr::StrAppendIndex { .. }
                | Instr::AddRightPair { in_place: true, .. }
        )));
        assert_eq!(vm.const_string_cache_funcs.get(&func_id), Some(&false));
        assert!(!vm
            .const_string_cache
            .contains_key(&slot_key(func_id, const_idx)));
    }

    #[cfg(not(feature = "jit"))]
    #[test]
    fn an_appending_function_shares_frozen_literals_and_grows_none_of_them() {
        let mut vm = vm(r#"
            function build(n) {
                let out = "seed";
                for (let i = 0; i < n; i++) out += "ab" + i;
                return out;
            }
            console.log(build(2));
            console.log(build(1));
            console.log(build(3));
            "#);
        assert_eq!(vm.output, ["seedab0ab1", "seedab0", "seedab0ab1ab2"]);

        for text in ["seed", "ab"] {
            let (func_id, const_idx) = literal_slot(&vm, text);
            let cached = *vm
                .const_string_cache
                .get(&slot_key(func_id, const_idx))
                .unwrap_or_else(|| panic!("{text:?} is cached"));
            assert!(
                matches!(vm.heap.get(cached.heap_index()), crate::heap::HeapObj::Str(s) if s.frozen()),
                "{text:?} is frozen"
            );
            assert_eq!(vm.display(cached), text, "the literal is what it was");
            assert_eq!(vm.resolve_const_slot(func_id, const_idx), cached);
        }
    }

    #[test]
    fn entry_and_literal_size_bounds_fail_to_the_exact_fresh_path() {
        let long = "x".repeat(CONST_STRING_CACHE_MAX_BYTES + 1);
        // The entry cap binds runtime-installed code: `short_value` is eval's.
        let source = format!(
            "function long_value() {{ return {long:?}; }}\n\
             function main_value() {{ return \"bounded-main\"; }}\n\
             eval('function short_value() {{ return \"bounded-short\"; }}');"
        );
        let mut vm = vm(&source);
        let (long_func, long_const) = literal_slot(&vm, &long);
        let long_a = vm.resolve_const_slot(long_func, long_const);
        let long_b = vm.resolve_const_slot(long_func, long_const);
        assert_ne!(long_a, long_b);
        assert!(!vm
            .const_string_cache
            .contains_key(&slot_key(long_func, long_const)));

        vm.const_string_cache.clear();
        vm.const_string_dense.clear();
        for n in 0..CONST_STRING_CACHE_MAX_ENTRIES as u64 {
            vm.const_string_cache
                .insert(u64::MAX - n, Value::heap(crate::heap::INTERN_EMPTY));
        }
        let (short_func, short_const) = literal_slot(&vm, "bounded-short");
        assert!(short_func as usize >= vm.main_func_count, "eval installed it");
        let before = vm.const_string_cache.len();
        let short = vm.resolve_const_slot(short_func, short_const);
        assert_eq!(vm.display(short), "bounded-short");
        assert_eq!(vm.const_string_cache.len(), before);
        assert!(!vm
            .const_string_cache
            .contains_key(&slot_key(short_func, short_const)));

        // The main program's literals are bounded by its source: still cached.
        let (main_func, main_const) = literal_slot(&vm, "bounded-main");
        let main_a = vm.resolve_const_slot(main_func, main_const);
        assert_eq!(vm.resolve_const_slot(main_func, main_const), main_a);
        assert!(vm
            .const_string_cache
            .contains_key(&slot_key(main_func, main_const)));
    }

    #[test]
    fn past_the_wide_entries_only_name_sized_literals_are_cached() {
        let name = "n".repeat(CONST_STRING_CACHE_NAME_BYTES);
        let wide = "w".repeat(CONST_STRING_CACHE_NAME_BYTES + 1);
        // Runtime-installed code, which the caps bind.
        let source = format!(
            "eval('function name_value() {{ return {name:?}; }}\\n\
             function wide_value() {{ return {wide:?}; }}');"
        );
        let mut vm = vm(&source);
        vm.const_string_cache.clear();
        vm.const_string_dense.clear();
        for n in 0..CONST_STRING_CACHE_WIDE_ENTRIES as u64 {
            vm.const_string_cache
                .insert(u64::MAX - n, Value::heap(crate::heap::INTERN_EMPTY));
        }
        let (wide_func, wide_const) = literal_slot(&vm, &wide);
        let wide_a = vm.resolve_const_slot(wide_func, wide_const);
        let wide_b = vm.resolve_const_slot(wide_func, wide_const);
        assert_eq!(vm.display(wide_a), wide);
        assert_ne!(wide_a, wide_b);
        assert!(!vm
            .const_string_cache
            .contains_key(&slot_key(wide_func, wide_const)));
        let (name_func, name_const) = literal_slot(&vm, &name);
        let name_a = vm.resolve_const_slot(name_func, name_const);
        let name_b = vm.resolve_const_slot(name_func, name_const);
        assert_eq!(vm.display(name_a), name);
        assert_eq!(name_a, name_b);
        assert!(vm
            .const_string_cache
            .contains_key(&slot_key(name_func, name_const)));
    }

    #[test]
    fn unified_function_id_is_part_of_the_key() {
        let mut vm = vm(r#"
            function left() { return "left-function-literal"; }
            function right() { return "right-function-literal"; }
            "#);
        vm.const_string_cache.clear();
        vm.const_string_dense.clear();
        let (left_func, left_const) = literal_slot(&vm, "left-function-literal");
        let (right_func, right_const) = literal_slot(&vm, "right-function-literal");
        assert_ne!(left_func, right_func);

        let left = vm.resolve_const_slot(left_func, left_const);
        let right = vm.resolve_const_slot(right_func, right_const);
        assert_eq!(vm.display(left), "left-function-literal");
        assert_eq!(vm.display(right), "right-function-literal");
        assert!(vm
            .const_string_cache
            .contains_key(&slot_key(left_func, left_const)));
        assert!(vm
            .const_string_cache
            .contains_key(&slot_key(right_func, right_const)));
    }
}
