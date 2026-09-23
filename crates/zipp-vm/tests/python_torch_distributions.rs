//! The bundled `torch.distributions`, checked against CPU PyTorch 2.11 /
//! CPython 3.11. Expected values come from `fixtures/torch_distributions/gen.py`
//! (run with the CPython that has PyTorch installed): it writes
//! `dist_expected.json`/`dist_expected2.json` from `dist_cases.py`/
//! `dist_cases2.py` and `api_expected.txt`/`api_expected2.txt` from
//! `dist_api.py`/`dist_api2.py`. Sampling streams differ from PyTorch's, so
//! `dist_sampling.py` and `dist_sampling2.py` check samples by their moments
//! and rsample gradients by reparameterisation identities.
#![cfg(feature = "python")]
use zipp_vm::frontend::compile_python_program;

fn run(source: &str) -> Result<Vec<String>, String> {
    let source = source.to_owned();
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(move || {
            let modules = vec![("main".to_owned(), source)];
            let mut compiled = compile_python_program("main", &modules, &[], &[], false)?;
            let state = compiled.state_mut();
            state.set_limits(20_000_000_000, None);
            state.run_init()?;
            Ok(state.take_output())
        })
        .expect("spawn")
        .join()
        .expect("test thread")
}

fn lines(text: &str) -> Vec<String> {
    text.lines().map(str::to_owned).collect()
}

/// A program's output as lines (one print may span several).
fn output_lines(out: Vec<String>) -> Vec<String> {
    lines(&out.join("\n"))
}

/// Every distribution's log_prob, entropy, cdf/icdf, mean/mode/variance/
/// stddev and the gradients of log_prob, entropy, cdf and icdf with respect
/// to the parameters (float64 within 1e-12 relative, float32 within 2e-5),
/// the transforms and their log-Jacobians, every registered KL pair with
/// its gradients, expand, perplexity, the exponential-family entropy and
/// the implicit Gamma reparameterisation gradient.
#[test]
fn distributions_match_pytorch_values_and_gradients() {
    let source = format!(
        "{}\n{}\nimport json\nexpected = json.loads(r'''{}''')\nbad = compare(results(), expected, 1e-12)\nprint(\"compared\", len(expected), len(bad))\nfor b in bad:\n    print(b)\n",
        include_str!("fixtures/torch_distributions/dist_cases.py"),
        include_str!("fixtures/torch_distributions/compare.py"),
        include_str!("fixtures/torch_distributions/dist_expected.json"),
    );
    assert_eq!(output_lines(run(&source).unwrap()), ["compared 459 0"]);
}

/// Batch/event shapes, has_rsample, supports and their checks, sample
/// dtypes, reprs, constraint objects, argument and sample validation
/// messages (and turning validation off), expand, enumerate_support, lazy
/// probs/logits, the KL registry's dispatch to subclasses and register_kl,
/// and transform identities, caching and shapes print what PyTorch prints.
#[test]
fn distributions_api_matches_pytorch() {
    let out = output_lines(run(include_str!("fixtures/torch_distributions/dist_api.py")).unwrap());
    assert_eq!(out, lines(include_str!("fixtures/torch_distributions/api_expected.txt")));
}

/// Samples from every distribution have the right moments (within six
/// standard errors), and rsample gradients satisfy the reparameterisation
/// identities (location-scale families exactly, the Gamma family through
/// E[dx/da] = dE[x]/da); the manual seed fixes the stream.
#[test]
fn distributions_sample_with_the_right_moments_and_gradients() {
    let out = output_lines(run(include_str!("fixtures/torch_distributions/dist_sampling.py")).unwrap());
    let failed: Vec<&String> = out.iter().filter(|line| !line.starts_with("ok ")).collect();
    assert!(failed.is_empty(), "{failed:#?}");
    assert_eq!(out.len(), 78);
}

/// The second set: Gumbel, Pareto, Weibull, Kumaraswamy, ContinuousBernoulli,
/// FisherSnedecor, GeneralizedPareto, InverseGamma, LogisticNormal, VonMises,
/// NegativeBinomial, Wishart and LKJCholesky (values and gradients of
/// log_prob, entropy, cdf/icdf, the moments), the CorrCholesky, LowerCholesky,
/// PositiveDefinite, Cat, Stack and CumulativeDistribution transforms, and
/// the KL pairs PyTorch registers beyond the first set (with gradients).
#[test]
fn more_distributions_match_pytorch_values_and_gradients() {
    let source = format!(
        "{}\n{}\nimport json\nexpected = json.loads(r'''{}''')\nbad = compare(results2(), expected, 1e-12)\nprint(\"compared\", len(expected), len(bad))\nfor b in bad:\n    print(b)\n",
        include_str!("fixtures/torch_distributions/dist_cases2.py"),
        include_str!("fixtures/torch_distributions/compare.py"),
        include_str!("fixtures/torch_distributions/dist_expected2.json"),
    );
    assert_eq!(output_lines(run(&source).unwrap()), ["compared 297 0"]);
}

/// The second set's shapes, flags, supports, reprs, argument and sample
/// validation, expand, what PyTorch leaves unimplemented, the new transforms
/// and registry entries, the Bregman KL, the package's `__all__`, and the
/// constraints/transforms/kl submodules re-exporting the same objects.
#[test]
fn more_distributions_api_matches_pytorch() {
    let out = output_lines(run(include_str!("fixtures/torch_distributions/dist_api2.py")).unwrap());
    assert_eq!(out, lines(include_str!("fixtures/torch_distributions/api_expected2.txt")));
}

/// Samples from the second set have the right moments and rsample gradients
/// (exact pathwise identities where they exist), VonMises and LKJCholesky
/// match their circular and correlation moments, and Poisson samples at
/// rates up to 1e6 (transformed rejection) have the right mean and variance.
#[test]
fn more_distributions_sample_with_the_right_moments_and_gradients() {
    let out = output_lines(run(include_str!("fixtures/torch_distributions/dist_sampling2.py")).unwrap());
    let failed: Vec<&String> = out.iter().filter(|line| !line.starts_with("ok ")).collect();
    assert!(failed.is_empty(), "{failed:#?}");
    assert_eq!(out.len(), 69);
}
