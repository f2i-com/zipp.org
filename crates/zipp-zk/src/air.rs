//! AIR constraints for the ZIPP VM execution trace (single-segment, 10 columns).

use winterfell::math::fields::f128::BaseElement;
use winterfell::math::{FieldElement, ToElements};
use winterfell::{
    Air, AirContext, Assertion, EvaluationFrame, ProofOptions, TraceInfo,
    TransitionConstraintDegree,
};

use crate::*;

#[derive(Clone)]
pub struct PublicInputs {
    /// Field-encoded result of `main` (bound to the final trace row).
    pub result: u64,
    /// Hash of the program (bound to all rows via a constant column).
    pub program_hash: u64,
}

impl ToElements<BaseElement> for PublicInputs {
    fn to_elements(&self) -> Vec<BaseElement> {
        vec![
            BaseElement::from(self.result),
            BaseElement::from(self.program_hash),
        ]
    }
}

pub struct ZippAir {
    context: AirContext<BaseElement>,
    result: BaseElement,
    prog_hash: BaseElement,
    trace_len: usize,
}

impl Air for ZippAir {
    type BaseField = BaseElement;
    type PublicInputs = PublicInputs;

    fn new(trace_info: TraceInfo, pub_inputs: PublicInputs, options: ProofOptions) -> Self {
        let d1 = TransitionConstraintDegree::new(1);
        let d2 = TransitionConstraintDegree::new(2);
        let d3 = TransitionConstraintDegree::new(3);
        let degrees = vec![
            d1.clone(), // 0  clk' - clk - 1
            d2.clone(), // 1  sel_const booleanity
            d2.clone(), // 2  sel_add  booleanity
            d2.clone(), // 3  sel_sub  booleanity
            d2.clone(), // 4  sel_mul  booleanity
            d2.clone(), // 5  sel_other booleanity
            d1,         // 6  selector exclusivity (sum - 1)
            d2.clone(), // 7  const: sc*(dst - imm)
            d2.clone(), // 8  add:   sa*(dst - a - b)
            d2,         // 9  sub:   ss*(dst - a + b)
            d3,         // 10 mul:   sm*(dst - a*b)
            TransitionConstraintDegree::new(1), // 11 program hash constancy
        ];
        let trace_len = trace_info.length();
        ZippAir {
            context: AirContext::new(trace_info, degrees, 3, options),
            result: BaseElement::from(pub_inputs.result),
            prog_hash: BaseElement::from(pub_inputs.program_hash),
            trace_len,
        }
    }

    fn context(&self) -> &AirContext<Self::BaseField> {
        &self.context
    }

    fn evaluate_transition<E: FieldElement<BaseField = Self::BaseField>>(
        &self,
        frame: &EvaluationFrame<E>,
        _periodic_values: &[E],
        result: &mut [E],
    ) {
        let cur = frame.current();
        let nxt = frame.next();
        let one = E::ONE;

        let clk = cur[COL_CLK];
        let clk_n = nxt[COL_CLK];
        let sc = cur[COL_SEL_CONST];
        let sa = cur[COL_SEL_ADD];
        let ss = cur[COL_SEL_SUB];
        let sm = cur[COL_SEL_MUL];
        let so = cur[COL_SEL_OTHER];
        let a = cur[COL_A];
        let b = cur[COL_B];
        let dst = cur[COL_DST];
        let imm = cur[COL_IMM];

        // Monotonic clock.
        result[0] = clk_n - clk - one;
        // Selector booleanity.
        result[1] = sc * (sc - one);
        result[2] = sa * (sa - one);
        result[3] = ss * (ss - one);
        result[4] = sm * (sm - one);
        result[5] = so * (so - one);
        // Exactly one selector active per row.
        result[6] = sc + sa + ss + sm + so - one;
        // Per-opcode arithmetic (gated by its selector).
        result[7] = sc * (dst - imm);
        result[8] = sa * (dst - a - b);
        result[9] = ss * (dst - a + b);
        result[10] = sm * (dst - a * b);
        // Program hash is constant across rows.
        result[11] = nxt[COL_PROG_HASH] - cur[COL_PROG_HASH];
    }

    fn get_assertions(&self) -> Vec<Assertion<Self::BaseField>> {
        let last = self.trace_len - 1;
        vec![
            // Clock starts at zero.
            Assertion::single(COL_CLK, 0, BaseElement::ZERO),
            // Final destination equals the public result.
            Assertion::single(COL_DST, last, self.result),
            // Program hash (row 0) equals the public program hash.
            Assertion::single(COL_PROG_HASH, 0, self.prog_hash),
        ]
    }
}
