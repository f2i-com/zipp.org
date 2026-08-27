//! B218: the eager `Promise.all` resolve-element collapse runs element jobs at
//! subscription instead of queueing one each. Job ordering is OBSERVABLE, so
//! the optimisation is only legal if every interleaving below is unchanged.
//!
//! Each case builds a sequence of tags and prints it; every expectation is
//! node-oracled (v24.12.0). The cases are chosen to attack the specific
//! argument the lane rests on — that an empty queue makes the collapsed jobs
//! unobservable, and that the deferred `CombinatorFinish` lands where the
//! spec's settle would have: more unrelated jobs than elements and fewer,
//! attachment in a later job, a pending element mid-list, a NON-empty queue
//! (the lane must switch itself off), nesting, the empty iterable, a thenable
//! whose `then` getter is user code, a rejection, and the sibling combinators.

const SRC: &str = include_str!("combinator_job_order.js");

const EXPECTED: &str = "A:g1|g2|g3|all,B:g1|all,C:g|all,G:empty|g1|one,\
D:g1|release|all,E:g1|g2|all,F:g1|g2|outer,H:thenCalled|g1|all,\
I:g1|g2|caught,J:g1|settled|race|any";

#[test]
fn eager_combinator_preserves_every_observable_job_order() {
    let out = zipp_vm::run(SRC).expect("source compiles");
    assert!(out.error.is_none(), "{:?}", out.error);
    assert_eq!(out.output, vec![EXPECTED.to_string()]);
}
