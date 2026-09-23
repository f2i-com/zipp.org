//! The bundled `torch.distributions`, checked against CPU PyTorch 2.11 /
//! CPython 3.11. Expected values come from `fixtures/torch_distributions/gen.py`
//! (run with the CPython that has PyTorch installed): it writes
//! `dist_expected.json` from `dist_cases.py` and `api_expected.txt` from
//! `dist_api.py`. Sampling streams differ from PyTorch's, so
//! `dist_sampling.py` checks samples by their moments and rsample gradients
//! by reparameterisation identities.
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
