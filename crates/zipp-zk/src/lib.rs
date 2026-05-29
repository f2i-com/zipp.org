//! ZIPP optional **zk-STARK profile**.
//!
//! Proves a ZIPP VM execution [`zippc::TraceStep`] stream with an
//! application-specific STARK (Winterfell 0.13), the same approach the ZIPP
//! chain's `zk-formlogic` prover takes for FormLogic bytecode — but for ZIPP's
//! own VM. This is the third profile in PLAN.md (native / WASM-contract /
//! **provable**), and it is entirely optional: drop the crate and the language
//! still runs.
//!
//! ## v0 soundness boundary (honest, like zk-formlogic's own docs)
//!
//! The single-segment AIR constrains the **arithmetic** steps end-to-end:
//! `Const` (dst = imm), `Add` (dst = a + b), `Sub` (dst = a - b), `Mul`
//! (dst = a * b), plus selector booleanity/exclusivity, a monotonic clock, and
//! a boundary assertion binding the final destination to the public result.
//! Control-flow / memory steps are recorded as `Other` and are **not yet**
//! constrained (no PC-integrity or memory-permutation argument). Hardening
//! those — plus 64-bit range checks for field/integer agreement — is the
//! roadmap, mirroring how zk-formlogic grew its 78-column trace.
//!
//! Values are encoded into the field as `x as u64`; the proven constraints hold
//! for non-negative, non-overflowing arithmetic (true for the bundled examples).

mod air;
mod prover;

pub use air::PublicInputs;

use winterfell::math::fields::f128::BaseElement;
use winterfell::math::FieldElement;
use winterfell::crypto::hashers::Blake3_256;
use winterfell::crypto::{DefaultRandomCoin, MerkleTree};
use winterfell::{
    AcceptableOptions, BatchingMethod, FieldExtension, Proof, ProofOptions,
};

use zippc::{OpKind, TraceStep};

type Blake3 = Blake3_256<BaseElement>;

/// Trace width (columns).
pub const WIDTH: usize = 10;

// Column indices.
pub(crate) const COL_CLK: usize = 0;
pub(crate) const COL_SEL_CONST: usize = 1;
pub(crate) const COL_SEL_ADD: usize = 2;
pub(crate) const COL_SEL_SUB: usize = 3;
pub(crate) const COL_SEL_MUL: usize = 4;
pub(crate) const COL_SEL_OTHER: usize = 5;
pub(crate) const COL_A: usize = 6;
pub(crate) const COL_B: usize = 7;
pub(crate) const COL_DST: usize = 8;
pub(crate) const COL_IMM: usize = 9;

/// STARK parameters — shared by prover and verifier (must match exactly).
pub(crate) fn proof_options() -> ProofOptions {
    // 32 queries * log2(8) blowup, quadratic extension over f128.
    ProofOptions::new(
        32,
        8,
        0,
        FieldExtension::Quadratic,
        8,
        31,
        BatchingMethod::Linear,
        BatchingMethod::Linear,
    )
}

fn felt(x: i64) -> BaseElement {
    BaseElement::from(x as u64)
}

/// Build the padded, power-of-two trace columns from a VM trace.
pub(crate) fn build_columns(steps: &[TraceStep]) -> Vec<Vec<BaseElement>> {
    let n = steps.len().next_power_of_two().max(8);
    let last_dst = steps.last().map(|s| s.dst).unwrap_or(0);
    let mut cols = vec![vec![BaseElement::ZERO; n]; WIDTH];

    for i in 0..n {
        // Re-clock to the row index so clk' = clk + 1 holds across padding.
        cols[COL_CLK][i] = BaseElement::from(i as u64);

        let (op, a, b, dst, imm) = if i < steps.len() {
            let s = steps[i];
            (s.op, s.a, s.b, s.dst, s.imm)
        } else {
            // Padding rows: inert `Other` steps that hold the final result.
            (OpKind::Other, 0, 0, last_dst, 0)
        };

        let one = BaseElement::ONE;
        match op {
            OpKind::Const => cols[COL_SEL_CONST][i] = one,
            OpKind::Add => cols[COL_SEL_ADD][i] = one,
            OpKind::Sub => cols[COL_SEL_SUB][i] = one,
            OpKind::Mul => cols[COL_SEL_MUL][i] = one,
            OpKind::Other => cols[COL_SEL_OTHER][i] = one,
        }
        cols[COL_A][i] = felt(a);
        cols[COL_B][i] = felt(b);
        cols[COL_DST][i] = felt(dst);
        cols[COL_IMM][i] = felt(imm);
    }
    cols
}

/// Generate a STARK proof of a ZIPP execution trace.
pub fn prove(trace: &[TraceStep], result: i64) -> Result<(Proof, PublicInputs), String> {
    if trace.is_empty() {
        return Err("cannot prove an empty trace".into());
    }
    let pub_inputs = PublicInputs { result: result as u64 };
    let columns = build_columns(trace);
    let proof = prover::prove(columns, pub_inputs.clone())?;
    Ok((proof, pub_inputs))
}

/// Verify a STARK proof (given its bytes) against the public inputs.
pub fn verify(proof_bytes: &[u8], pub_inputs: &PublicInputs) -> Result<(), String> {
    let proof = Proof::from_bytes(proof_bytes).map_err(|e| format!("invalid proof bytes: {e:?}"))?;
    let acceptable = AcceptableOptions::OptionSet(vec![proof_options()]);
    winterfell::verify::<air::ZippAir, Blake3, DefaultRandomCoin<Blake3>, MerkleTree<Blake3>>(
        proof,
        pub_inputs.clone(),
        &acceptable,
    )
    .map_err(|e| format!("verification failed: {e:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trace_of(src: &str) -> (Vec<TraceStep>, i64) {
        let prog = zippc::compile(src).expect("compile");
        let r = zippc::vm::run(&prog, true).expect("run");
        (r.trace, r.result.as_i64().expect("integer result"))
    }

    // NOTE: run with `cargo test -p zipp-zk --release` — the AIR degree check is
    // a debug_assert that selector-gated constraints trip in debug builds.

    #[test]
    fn proves_and_verifies_real_execution() {
        let (trace, result) = trace_of("fn main(): i64 { let a = 7; let b = 6; return a * b + 3; }");
        assert_eq!(result, 45);
        let (proof, pi) = prove(&trace, result).expect("prove");
        verify(&proof.to_bytes(), &pi).expect("valid proof must verify");
    }

    #[test]
    fn rejects_a_false_result() {
        // The proof must NOT verify against a result the execution didn't produce
        // — i.e. the STARK is meaningful, not a rubber stamp.
        let (trace, result) = trace_of("fn main(): i64 { let a = 7; let b = 6; return a * b + 3; }");
        let (proof, _pi) = prove(&trace, result).expect("prove");
        let lie = PublicInputs { result: (result + 1) as u64 };
        assert!(
            verify(&proof.to_bytes(), &lie).is_err(),
            "verification accepted a false result — soundness broken"
        );
    }
}

