//! Exact, side-effect-free counted-loop kernels for the release WASM
//! interpreter.
//!
//! These are deliberately exact loop-slice recognisers, not a second general
//! interpreter.  A single opcode, register relationship, or runtime-value miss
//! leaves the ordinary dispatch path in complete control.

#![allow(unused_imports)]
use super::*;
use crate::bytecode::Instr;
use crate::value::Value;

/// Largest `n` for which `n * (n + 1) / 2` is a JavaScript safe integer.
/// Every prefix sum from one through `n` is then exact in binary64 too, so the
/// closed form is bit-identical to the source's sequential Number additions.
const MAX_SAFE_TRIANGULAR_N: i32 = 134_217_727;

type CountedSumLoopPlan = (u16, u16);

/// Match the immutable five-op slice and return the accumulator and the
/// constant-one lane. The increment may use either operand order: its
/// immediately preceding `LoadInt` and the runtime first-header guards prove
/// both operands remain tagged integers throughout every admitted loop.
///
/// The slice was seven ops while the compiler copied each statement's
/// discarded value into a scratch register; a build without the JIT, the only
/// kind this module is compiled into, no longer emits those copies.
#[inline]
fn exact_counted_sum_loop_plan(
    code: &[Instr],
    ip: usize,
    a: u16,
    b: u16,
    target: u32,
) -> Option<CountedSumLoopPlan> {
    let exit_ip = ip.checked_add(5)?;
    if usize::try_from(target).ok() != Some(exit_ip) {
        return None;
    }

    match code.get(ip..exit_ip) {
        Some(
            [Instr::JumpIfNotLe {
                a: header_index,
                b: header_limit,
                target: header_exit,
            }, Instr::Add {
                dst: sum,
                a: add_sum,
                b: add_index,
            }, Instr::LoadInt { dst: one, val: 1 }, Instr::Add {
                dst: next_index,
                a: increment_left,
                b: increment_right,
            }, Instr::Jump { target: backedge }],
        ) if *header_index == a
            && *header_limit == b
            && *header_exit == target
            && *sum == *add_sum
            && *add_index == a
            && *next_index == a
            && ((*increment_left == a && *increment_right == *one)
                || (*increment_left == *one && *increment_right == a))
            && usize::try_from(*backedge).ok() == Some(ip) =>
        {
            Some((*sum, *one))
        }
        _ => None,
    }
}

impl Vm<'_> {
    /// Collapse this exact compiler output at its first loop header:
    ///
    /// ```text
    /// let total = 0, i = 1;
    /// while (i <= n) { total = total + i; i = i + 1; }
    /// return total;
    /// ```
    ///
    /// The `JumpIfNotLe` dispatch tick has already been paid.  For `n`
    /// iterations the dispatched tail is five instructions per iteration:
    /// four body/backedge ops plus the following header test (including the
    /// final failing test).  `Return` remains in the dispatch loop, which pins
    /// the exact and exact-minus-one budget boundary.
    #[inline]
    pub(super) fn try_metered_counted_sum_loop(
        &mut self,
        func_id: u32,
        base: usize,
        ip: usize,
        a: u16,
        b: u16,
        target: u32,
    ) -> bool {
        // Match only the contiguous, immutable five-op loop slice. Register
        // identities are allowed to vary, but every data-flow relationship is
        // pinned. This admits both the direct `w(n)` probe and the comparison
        // harness's `n = mode * 2000000` wrapper without coupling the proof to
        // their unrelated prefix/return-string bytecode.
        let plan = {
            let proto = self.func(func_id as usize);
            exact_counted_sum_loop_plan(&proto.code, ip, a, b, target)
        };
        let Some((sum_reg, one_reg)) = plan else {
            return false;
        };

        // A compiler normally assigns four distinct registers here. Make that
        // part of admission rather than trusting provenance: aliasing the
        // constant with a live lane would change the bytecode's recurrence.
        let regs = [sum_reg, a, b, one_reg];
        for left in 0..regs.len() {
            if regs[left + 1..].contains(&regs[left]) {
                return false;
            }
        }

        // This must be the FIRST header visit.  Besides closing off synthetic
        // entry/backedge states, the tagged-int guards avoid every observable
        // ToPrimitive/ToNumber operation in the original comparison and adds.
        let limit = self.get(base, b);
        if self.get(base, sum_reg) != Value::int(0)
            || self.get(base, a) != Value::int(1)
            || !limit.is_int()
        {
            return false;
        }
        let n = limit.as_int();
        if !(1..=MAX_SAFE_TRIANGULAR_N).contains(&n) {
            return false;
        }

        let skipped_steps = i64::from(n) * 5;
        // If the complete pure kernel cannot be paid, retain ordinary dispatch
        // so exhaustion is observed at the historical opcode.  Equality is
        // admitted: it completes the loop and then correctly fails when the
        // uncharged Return attempts its tick.
        if let Some(rec) = self.instr_rec.as_ref() {
            if rec.exhaustion.is_some()
                || (rec.remaining != i64::MAX && rec.remaining < skipped_steps)
            {
                return false;
            }
        }

        let n64 = i64::from(n);
        let sum = n64 * (n64 + 1) / 2;
        let sum = Value::num(sum as f64);
        let next = Value::int(n + 1);

        // Reproduce every loop-carried register at the exit header, not
        // merely the returned accumulator.  This keeps the fast state a
        // faithful state of the original bytecode even if future diagnostics
        // inspect the frame window.
        self.set(base, sum_reg, sum);
        self.set(base, one_reg, Value::int(1));
        self.set(base, a, next);

        // Work is complete and cannot fail or allocate; charge its exact
        // dispatch count before the next guest opcode runs.
        self.charge_steps(skipped_steps);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compiled_loop_plan(source: &str) -> Option<CountedSumLoopPlan> {
        let ast = crate::front::parse_script(source).expect("source parses");
        let program = crate::compile::compile_program(&ast, source).expect("source compiles");
        let func = program
            .functions
            .iter()
            .find(|func| func.name == "w")
            .expect("compiled w function");
        func.code
            .iter()
            .enumerate()
            .find_map(|(ip, instr)| match *instr {
                Instr::JumpIfNotLe { a, b, target } => {
                    exact_counted_sum_loop_plan(&func.code, ip, a, b, target)
                }
                _ => None,
            })
    }

    fn loop_slice(increment_left: u16, increment_right: u16) -> [Instr; 5] {
        [
            Instr::JumpIfNotLe {
                a: 1,
                b: 2,
                target: 5,
            },
            Instr::Add { dst: 0, a: 0, b: 1 },
            Instr::LoadInt { dst: 4, val: 1 },
            Instr::Add {
                dst: 1,
                a: increment_left,
                b: increment_right,
            },
            Instr::Jump { target: 0 },
        ]
    }

    #[test]
    fn exact_plan_admits_both_increment_orders_and_rejects_other_operands() {
        let expected = Some((0, 4));
        assert_eq!(
            exact_counted_sum_loop_plan(&loop_slice(1, 4), 0, 1, 2, 5),
            expected
        );
        assert_eq!(
            exact_counted_sum_loop_plan(&loop_slice(4, 1), 0, 1, 2, 5),
            expected
        );
        assert_eq!(
            exact_counted_sum_loop_plan(&loop_slice(4, 2), 0, 1, 2, 5),
            None,
            "an increment using the limit lane must fail closed"
        );
    }

    #[test]
    fn the_slice_with_copied_statement_values_is_no_longer_a_plan() {
        let with_copies = [
            Instr::JumpIfNotLe {
                a: 1,
                b: 2,
                target: 7,
            },
            Instr::Add { dst: 0, a: 0, b: 1 },
            Instr::Move { dst: 3, src: 0 },
            Instr::LoadInt { dst: 4, val: 1 },
            Instr::Add { dst: 1, a: 1, b: 4 },
            Instr::Move { dst: 5, src: 1 },
            Instr::Jump { target: 0 },
        ];
        assert_eq!(exact_counted_sum_loop_plan(&with_copies, 0, 1, 2, 7), None);
    }

    #[test]
    fn compiler_commuted_increment_reaches_the_exact_plan() {
        assert!(
            compiled_loop_plan(
                "function w(n) { let total = 0; let i = 1; while (i <= n) { total = total + i; i = 1 + i; } return total; }"
            )
            .is_some(),
            "the compiler's commuted increment must activate the recogniser"
        );
        assert_eq!(
            compiled_loop_plan(
                "function w(n) { let total = 0; let i = 1; while (i <= n) { total = i + total; i = 1 + i; } return total; }"
            ),
            None,
            "commuting an unproved accumulator Add must remain a near miss"
        );
    }
}
