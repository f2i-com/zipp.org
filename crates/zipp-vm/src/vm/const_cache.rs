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
//! * only short literals and a bounded number of slots/functions are retained;
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
pub(super) const CONST_STRING_CACHE_MAX_ENTRIES: usize = 4096;
/// Avoid duplicating arbitrarily large source literals into a permanent cache.
/// Together with the entry cap this bounds retained string payload to 1 MiB.
pub(super) const CONST_STRING_CACHE_MAX_BYTES: usize = 256;
/// Dynamic code installs monotonically increasing function ids. Bound the
/// accompanying eligibility memo too; beyond it we fail closed to fresh loads.
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
    #[inline]
    fn const_string_func_shareable(&mut self, func_id: u32) -> bool {
        if let Some(&safe) = self.const_string_cache_funcs.get(&func_id) {
            return safe;
        }
        // Keep the VM-internal side table resource-bounded even under an eval
        // stream producing infinitely many distinct functions. Failure to memo
        // is also failure to cache, so an attacker cannot induce a repeated
        // whole-body scan at the bound.
        if self.const_string_cache_funcs.len() >= CONST_STRING_CACHE_MAX_FUNCTIONS {
            return false;
        }
        let safe = function_literals_shareable(self.func(func_id as usize));
        self.const_string_cache_funcs.insert(func_id, safe);
        safe
    }

    /// Resolve one bytecode constant slot, memoizing eligible string literals.
    /// Every rejection delegates verbatim to `resolve_const`, including lone-
    /// surrogate/WTF-8 decoding and the canonical `typeof` handles.
    #[inline]
    pub(crate) fn resolve_const_slot(&mut self, func_id: u32, const_idx: u32) -> Value {
        let raw = self.func(func_id as usize).constants[const_idx as usize];
        if !raw.is_heap()
            || (raw.heap_index() & crate::vm::helpers_misc::STRING_CONST_BIT) == 0
            || !self.const_string_cache_enabled
        {
            return self.resolve_const(func_id, raw);
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
        if self.const_string_cache.len() >= CONST_STRING_CACHE_MAX_ENTRIES
            || !self.const_string_func_shareable(func_id)
        {
            return self.resolve_const(func_id, raw);
        }

        let resolved = self.resolve_const(func_id, raw);
        debug_assert!(
            resolved.is_heap(),
            "a string constant resolves to a heap value"
        );
        self.const_string_cache.insert(key, resolved);
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
        for func_id in 0..vm.main_func_count as u32 {
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

        let first = vm.resolve_const_slot(func_id, const_idx);
        let second = vm.resolve_const_slot(func_id, const_idx);
        assert_eq!(first, second);
        assert_eq!(vm.const_string_cache.len(), 1);

        vm.const_string_cache.clear();
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

    #[test]
    fn entry_and_literal_size_bounds_fail_to_the_exact_fresh_path() {
        let long = "x".repeat(CONST_STRING_CACHE_MAX_BYTES + 1);
        let source = format!("function long_value() {{ return {long:?}; }}\nfunction short_value() {{ return \"bounded-short\"; }}");
        let mut vm = vm(&source);
        let (long_func, long_const) = literal_slot(&vm, &long);
        let long_a = vm.resolve_const_slot(long_func, long_const);
        let long_b = vm.resolve_const_slot(long_func, long_const);
        assert_ne!(long_a, long_b);
        assert!(!vm
            .const_string_cache
            .contains_key(&slot_key(long_func, long_const)));

        vm.const_string_cache.clear();
        for n in 0..CONST_STRING_CACHE_MAX_ENTRIES as u64 {
            vm.const_string_cache
                .insert(u64::MAX - n, Value::heap(crate::heap::INTERN_EMPTY));
        }
        let (short_func, short_const) = literal_slot(&vm, "bounded-short");
        let before = vm.const_string_cache.len();
        let short = vm.resolve_const_slot(short_func, short_const);
        assert_eq!(vm.display(short), "bounded-short");
        assert_eq!(vm.const_string_cache.len(), before);
        assert!(!vm
            .const_string_cache
            .contains_key(&slot_key(short_func, short_const)));
    }

    #[test]
    fn unified_function_id_is_part_of_the_key() {
        let mut vm = vm(r#"
            function left() { return "left-function-literal"; }
            function right() { return "right-function-literal"; }
            "#);
        vm.const_string_cache.clear();
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
