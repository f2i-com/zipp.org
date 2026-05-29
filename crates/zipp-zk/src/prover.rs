//! Winterfell 0.13 prover wiring for the single-segment ZIPP trace.
//! Mirrors the chain's `zk-formlogic` prover structure, minus the auxiliary
//! (memory-permutation) segment.

use winterfell::crypto::hashers::Blake3_256;
use winterfell::crypto::{DefaultRandomCoin, MerkleTree};
use winterfell::math::fields::f128::BaseElement;
use winterfell::math::FieldElement;
use winterfell::matrix::ColMatrix;
use winterfell::{
    AuxRandElements, CompositionPoly, CompositionPolyTrace, ConstraintCompositionCoefficients,
    DefaultConstraintCommitment, DefaultConstraintEvaluator, DefaultTraceLde, EvaluationFrame,
    PartitionOptions, Proof, ProofOptions, Prover, StarkDomain, Trace, TraceInfo, TracePolyTable,
};

use crate::air::{PublicInputs, ZippAir};
use crate::{proof_options, WIDTH};

type Blake3 = Blake3_256<BaseElement>;

/// Single-segment execution trace.
pub struct ZippTrace {
    info: TraceInfo,
    main: ColMatrix<BaseElement>,
}

impl ZippTrace {
    fn new(columns: Vec<Vec<BaseElement>>) -> Self {
        let len = columns[0].len();
        Self {
            info: TraceInfo::new(WIDTH, len),
            main: ColMatrix::new(columns),
        }
    }
}

impl Trace for ZippTrace {
    type BaseField = BaseElement;

    fn info(&self) -> &TraceInfo {
        &self.info
    }

    fn main_segment(&self) -> &ColMatrix<Self::BaseField> {
        &self.main
    }

    fn read_main_frame(&self, row_idx: usize, frame: &mut EvaluationFrame<Self::BaseField>) {
        let next = (row_idx + 1) % self.length();
        self.main.read_row_into(row_idx, frame.current_mut());
        self.main.read_row_into(next, frame.next_mut());
    }
}

struct ZippProver {
    options: ProofOptions,
    pub_inputs: PublicInputs,
}

impl Prover for ZippProver {
    type BaseField = BaseElement;
    type Air = ZippAir;
    type Trace = ZippTrace;
    type HashFn = Blake3;
    type VC = MerkleTree<Blake3>;
    type RandomCoin = DefaultRandomCoin<Blake3>;
    type TraceLde<E: FieldElement<BaseField = BaseElement>> =
        DefaultTraceLde<E, Blake3, MerkleTree<Blake3>>;
    type ConstraintEvaluator<'a, E: FieldElement<BaseField = BaseElement>> =
        DefaultConstraintEvaluator<'a, ZippAir, E>;
    type ConstraintCommitment<E: FieldElement<BaseField = BaseElement>> =
        DefaultConstraintCommitment<E, Blake3, MerkleTree<Blake3>>;

    fn get_pub_inputs(&self, _trace: &Self::Trace) -> PublicInputs {
        self.pub_inputs.clone()
    }

    fn options(&self) -> &ProofOptions {
        &self.options
    }

    fn new_trace_lde<E: FieldElement<BaseField = BaseElement>>(
        &self,
        trace_info: &TraceInfo,
        main_trace: &ColMatrix<BaseElement>,
        domain: &StarkDomain<BaseElement>,
        partition_option: PartitionOptions,
    ) -> (Self::TraceLde<E>, TracePolyTable<E>) {
        DefaultTraceLde::new(trace_info, main_trace, domain, partition_option)
    }

    fn new_evaluator<'a, E: FieldElement<BaseField = BaseElement>>(
        &self,
        air: &'a ZippAir,
        aux_rand_elements: Option<AuxRandElements<E>>,
        composition_coefficients: ConstraintCompositionCoefficients<E>,
    ) -> Self::ConstraintEvaluator<'a, E> {
        DefaultConstraintEvaluator::new(air, aux_rand_elements, composition_coefficients)
    }

    fn build_constraint_commitment<E: FieldElement<BaseField = BaseElement>>(
        &self,
        composition_poly_trace: CompositionPolyTrace<E>,
        num_constraint_composition_columns: usize,
        domain: &StarkDomain<BaseElement>,
        partition_options: PartitionOptions,
    ) -> (Self::ConstraintCommitment<E>, CompositionPoly<E>) {
        DefaultConstraintCommitment::new(
            composition_poly_trace,
            num_constraint_composition_columns,
            domain,
            partition_options,
        )
    }
}

pub(crate) fn prove(
    columns: Vec<Vec<BaseElement>>,
    pub_inputs: PublicInputs,
) -> Result<Proof, String> {
    let trace = ZippTrace::new(columns);
    let prover = ZippProver {
        options: proof_options(),
        pub_inputs,
    };
    Prover::prove(&prover, trace).map_err(|e| format!("proof generation failed: {e:?}"))
}
