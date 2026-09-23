//! The bundled `torch.nn` against PyTorch 2.11: layers, losses and
//! functional ops (forward, input and parameter gradients, buffers) from
//! generated parity fixtures, and module behaviour (registries, state_dict,
//! containers, repr, hooks, init) compared line by line with PyTorch's output.
//!
//! Regenerate the fixtures with CPU PyTorch:
//! `python crates/zipp-vm/tests/fixtures/torch_nn/gen.py`.
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

/// Runs one parity group; every case must match PyTorch.
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
fn bug_fixes_match_pytorch() {
    // BCE-with-logits at logit 0, cross-entropy/nll ignore_index, weights and
    // N-D input, zero-safe normalize/cosine_similarity, dropout(p=1), the
    // implicit softmax dim, unbatched cells, conv1d stride/'same', ...
    parity("bugs", include_str!("fixtures/torch_nn/parity_bugs.py"));
}

#[test]
fn layers_match_pytorch() {
    // BatchNorm/InstanceNorm/GroupNorm/RMSNorm, pooling, transposed
    // convolution, fold/unfold, interpolate, padding modes, embeddings,
    // attention, transformers, RNN/LSTM/GRU (packed too) and activations.
    parity("layers", include_str!("fixtures/torch_nn/parity_layers.py"));
}

#[test]
fn losses_match_pytorch() {
    parity("losses", include_str!("fixtures/torch_nn/parity_losses.py"));
}

#[test]
fn module_behaviour_matches_pytorch() {
    let out = run(include_str!("fixtures/torch_nn/api.py"))
        .unwrap()
        .join("\n");
    let out: Vec<&str> = out.lines().collect();
    let expected: Vec<&str> = include_str!("fixtures/torch_nn/api_expected.txt")
        .lines()
        .collect();
    for (i, (got, want)) in out.iter().zip(expected.iter()).enumerate() {
        assert_eq!(got, want, "api line {}", i + 1);
    }
    assert_eq!(out.len(), expected.len(), "\n{}", out.join("\n"));
}
