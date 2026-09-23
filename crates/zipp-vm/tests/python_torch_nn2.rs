//! More of the bundled `torch.nn` against PyTorch 2.11: 3-D convolution,
//! pooling and resampling, bicubic and antialiased interpolation, spectral
//! norm and the parametrization API, and lazy modules.
//!
//! Regenerate the fixtures with CPU PyTorch:
//! `python crates/zipp-vm/tests/fixtures/torch_nn2/gen.py`.
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
fn conv3d_pool3d_and_3d_resampling_match_pytorch() {
    // Conv3d/ConvTranspose3d (stride, padding incl. 'same', dilation,
    // groups, output_padding, output_size, unbatched), 3-D max/avg/adaptive/
    // Lp pooling and unpooling, 5-D interpolate, BatchNorm3d/InstanceNorm3d,
    // 3-D padding modules and lazy 3-D convolutions.
    parity("conv3d", include_str!("fixtures/torch_nn2/parity_conv3d.py"));
}

#[test]
fn bicubic_and_antialias_match_pytorch() {
    // Bicubic (A=-0.75, border clamping) and antialiased bilinear/bicubic
    // (PIL-style) resampling, up and down, sizes and scale factors, with and
    // without align_corners and recompute_scale_factor.
    parity("resample", include_str!("fixtures/torch_nn2/parity_resample.py"));
}

#[test]
fn norms_parametrizations_and_lazy_modules_match_pytorch() {
    // Legacy and parametrized spectral_norm (PyTorch's u/v buffers loaded),
    // weight_norm, orthogonal, removal, lazy modules after a dry run,
    // multilabel_margin_loss and the in-place activation aliases.
    parity("param", include_str!("fixtures/torch_nn2/parity_param.py"));
}

#[test]
fn lazy_parametrize_and_hook_behaviour_matches_pytorch() {
    let out = run(include_str!("fixtures/torch_nn2/api.py"))
        .unwrap()
        .join("\n");
    let out: Vec<&str> = out.lines().collect();
    let expected: Vec<&str> = include_str!("fixtures/torch_nn2/api_expected.txt")
        .lines()
        .collect();
    for (i, (got, want)) in out.iter().zip(expected.iter()).enumerate() {
        assert_eq!(got, want, "api line {}", i + 1);
    }
    assert_eq!(out.len(), expected.len(), "\n{}", out.join("\n"));
}
