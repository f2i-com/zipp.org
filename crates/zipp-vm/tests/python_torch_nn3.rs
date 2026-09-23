//! A third set of the bundled `torch.nn` against PyTorch 2.11: grid_sample
//! and affine_grid, CTC loss, fractional max pooling, convolution padding
//! modes, SyncBatchNorm, the DataParallel/DistributedDataParallel wrappers,
//! second derivatives through convolutions, uninitialized-parameter errors
//! and dropout's kept values.
//!
//! Regenerate the fixtures with CPU PyTorch:
//! `python crates/zipp-vm/tests/fixtures/torch_nn3/gen.py`.
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
            state.set_limits(2_000_000_000, None);
            state.run_init()?;
            Ok(state.take_output())
        })
        .expect("spawn")
        .join()
        .expect("test thread")
}

/// Runs one parity group with torch_nn's runner; every case must match PyTorch.
fn parity(group: &str, cases: &str) {
    let source = format!(
        "{}\n{}\nrun_cases('{group}')\n",
        include_str!("fixtures/torch_nn/check.py"),
        cases
    );
    let out = run(&source).unwrap_or_else(|e| panic!("parity {group}: {e}"));
    let last = out.last().cloned().unwrap_or_default();
    let total = cases.matches("\n    {\"name\":").count();
    assert_eq!(
        last,
        format!("parity {group}: {total}/{total} passed"),
        "\n{}",
        out.join("\n")
    );
}

#[test]
fn grid_sample_and_affine_grid_match_pytorch() {
    // 2-D bilinear/nearest/bicubic and 3-D bilinear/nearest sampling with
    // every padding mode, with and without align_corners (values, input
    // and grid gradients), affine_grid in 2-D and 3-D, and both composed.
    parity("grid", include_str!("fixtures/torch_nn3/parity_grid.py"));
}

#[test]
fn ctc_loss_matches_pytorch() {
    // Padded and concatenated targets, tuple/list/tensor lengths, every
    // reduction, blank, empty and infeasible targets, zero_infinity, the
    // gradient on raw (unnormalized) log-probabilities, unbatched input,
    // float64 and nn.CTCLoss.
    parity("ctc", include_str!("fixtures/torch_nn3/parity_ctc.py"));
}

#[test]
fn fractional_pooling_padding_modes_and_sync_batchnorm_match_pytorch() {
    // Fractional max pooling (2-D and 3-D, output_size/output_ratio,
    // indices, explicit _random_samples), Conv1d/2d/3d with reflect,
    // replicate and circular padding, SyncBatchNorm and
    // convert_sync_batchnorm.
    parity("pool", include_str!("fixtures/torch_nn3/parity_pool.py"));
}

#[test]
fn second_derivatives_through_convolutions_match_pytorch() {
    // create_graph=True through conv1d/2d/3d and conv_transpose1d/2d/3d:
    // input and weight gradient penalties, a Hessian-vector product and a
    // third derivative.
    parity("dbl", include_str!("fixtures/torch_nn3/parity_dbl.py"));
}

#[test]
fn parallel_wrappers_match_pytorch() {
    // DataParallel and DistributedDataParallel load PyTorch's `module.`
    // state dicts and forward (with kwargs) to the wrapped module.
    parity("parallel", include_str!("fixtures/torch_nn3/parity_parallel.py"));
}

#[test]
fn wrapper_errors_lazy_parameters_and_dropout_match_pytorch() {
    let out = run(include_str!("fixtures/torch_nn3/api.py"))
        .unwrap()
        .join("\n");
    let out: Vec<&str> = out.lines().collect();
    let expected: Vec<&str> = include_str!("fixtures/torch_nn3/api_expected.txt")
        .lines()
        .collect();
    for (i, (got, want)) in out.iter().zip(expected.iter()).enumerate() {
        assert_eq!(got, want, "api line {}", i + 1);
    }
    assert_eq!(out.len(), expected.len(), "\n{}", out.join("\n"));
}
