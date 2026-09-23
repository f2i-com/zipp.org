//! The bundled `torch.amp` checked against CPU PyTorch 2.11:
//! `fixtures/torch_amp/amp_cases.py` runs unchanged under both, and
//! `gen.py` writes PyTorch's results to `amp_expected.json` and
//! `amp_lines_expected.txt`. GradScaler runs with `device="cpu"` there (the
//! machine that generated the fixture has CUDA); the CUDA spellings, which
//! PyTorch disables when CUDA is unavailable, are checked on their own.
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

fn lines(text: &str) -> Vec<String> {
    text.lines().map(str::to_owned).collect()
}

/// Scales, growth trackers, skipped steps, parameters and unscaled
/// gradients over SGD and Adam training runs with inf/NaN losses injected
/// (float64 and float32), unscale_ before gradient clipping, a growth
/// interval of one, two optimizers sharing a scaler, update(new_scale) with
/// a float and a tensor, and the state_dict round trip.
#[test]
fn grad_scaler_matches_pytorch_trajectories() {
    let source = format!(
        "{}\nimport json\nexpected = json.loads(r'''{}''')\n{}",
        include_str!("fixtures/torch_amp/amp_cases.py"),
        include_str!("fixtures/torch_amp/amp_expected.json"),
        r#"
got = results()
bad = []
for name in sorted(expected):
    e, g = expected[name], got[name]
    if len(e) != len(g):
        bad.append("%s: %d values, expected %d" % (name, len(g), len(e)))
        continue
    tol = 1e-6 if name.endswith("f32") else 1e-13
    worst = max([abs(a - b) / max(1.0, abs(b)) for a, b in zip(g, e)] + [0.0])
    if not worst <= tol:
        bad.append("%s: off by %.3g" % (name, worst))
print("compared", len(expected), bad)
"#
    );
    assert_eq!(run(&source).unwrap(), ["compared 7 []"]);
}

/// GradScaler's errors (stage machine, factor checks, empty state dict,
/// closures), a disabled scaler, autocast state and nesting (including a
/// nested `torch.autocast`), the decorator form, and custom_fwd/custom_bwd
/// with and without cast_inputs.
#[test]
fn amp_behaviour_matches_pytorch() {
    let source = format!(
        "{}\nfor line in lines():\n    print(line)\n",
        include_str!("fixtures/torch_amp/amp_cases.py")
    );
    let out = lines(&run(&source).unwrap().join("\n"));
    assert_eq!(out, lines(include_str!("fixtures/torch_amp/amp_lines_expected.txt")));
}

/// Without CUDA, PyTorch disables `GradScaler("cuda")` and
/// `autocast("cuda")` (with a warning): the scaler is the identity and
/// `step()` is a plain `optimizer.step()`. The deprecated
/// `torch.cuda.amp` spellings are the same classes.
#[test]
fn cuda_amp_disables_itself_without_cuda() {
    let source = r#"
import torch
import torch.amp as amp
s = amp.GradScaler()
w = torch.tensor([1.0, 2.0], requires_grad=True)
opt = torch.optim.SGD([w], lr=0.5)
loss = (w * w).sum()
print(s.is_enabled(), s.scale(loss) is loss, s.get_scale(), s.state_dict())
loss.backward()
print(s.step(opt), w.detach().tolist(), s.update(), s.unscale_(opt))
legacy = torch.cuda.amp.GradScaler(init_scale=8.0)
print(isinstance(legacy, amp.GradScaler), legacy.is_enabled())
with amp.autocast("cuda") as ctx:
    print(ctx.enabled, torch.is_autocast_enabled("cuda"), torch.is_autocast_enabled())
with torch.cuda.amp.autocast() as ctx:
    print(isinstance(ctx, amp.autocast), torch.is_autocast_enabled("cuda"))
print(isinstance(torch.autocast("cpu"), amp.autocast), torch.get_autocast_dtype("cpu") == torch.bfloat16)
with torch.autocast("cpu", dtype=torch.bfloat16):
    x = torch.ones(2, 2)
    print(torch.is_autocast_enabled("cpu"), (x @ x).dtype)
"#;
    let out = run(source).unwrap().join("\n");
    assert_eq!(
        lines(&out),
        [
            "False True 1.0 {}",
            "None [0.0, 0.0] None None",
            "True False",
            "False False False",
            "True False",
            "True True",
            "True torch.bfloat16",
        ],
        "{out}"
    );
}

/// Inside `torch.autocast("cpu")` every op takes PyTorch 2.11's CPU policy:
/// the result dtype of ~70 ops (lower-precision, fp32, promote and
/// untouched ops, composites such as einsum/tensordot/multi_dot/pinv and
/// the nn modules built on them) for float32, bfloat16, float16 and float64
/// inputs in bfloat16 and float16 regions, cat's `prioritize` errors, and
/// the autocast state through nesting, `enabled=False`, the decorator,
/// `cache_enabled` and an unsupported dtype.
#[test]
fn cpu_autocast_retypes_ops_as_pytorch() {
    let source = format!(
        "{}
for line in policy_lines() + state_lines():
    print(line)
",
        include_str!("fixtures/torch_amp/autocast_cases.py")
    );
    let out = lines(&run(&source).unwrap().join("
"));
    let want = lines(include_str!("fixtures/torch_amp/autocast_lines_expected.txt"));
    for (got, want) in out.iter().zip(&want) {
        assert_eq!(got, want);
    }
    assert_eq!(out.len(), want.len());
}

/// A training step inside `torch.autocast("cpu")` (bfloat16 and float16):
/// the activations' dtypes, the loss values and the float32 gradients that
/// flow back through the casts agree with PyTorch to the autocast format's
/// precision (Zipp's reduced-precision matmul rounds once where PyTorch's
/// blocked float kernels may round differently).
#[test]
fn cpu_autocast_training_step_matches_pytorch() {
    let source = format!(
        "{}
import json
expected = json.loads(r'''{}''')
{}",
        include_str!("fixtures/torch_amp/autocast_cases.py"),
        include_str!("fixtures/torch_amp/autocast_expected.json"),
        r#"
got = train_values()
bad = []
for name in sorted(expected):
    e, g = expected[name], got[name]
    if name.endswith("dtypes") or len(e) != len(g):
        if e != g:
            bad.append("%s: %r, expected %r" % (name, g, e))
        continue
    if isinstance(e[0], str):
        if e[0] != g[0]:
            bad.append("%s: %s, expected %s" % (name, g[0], e[0]))
        e, g = e[1:], g[1:]
    tol = 0.02 if name.startswith("bfloat16") else 0.004
    worst = max([abs(a - b) / max(1.0, abs(b)) for a, b in zip(g, e)] + [0.0])
    if not worst <= tol:
        bad.append("%s: off by %.3g" % (name, worst))
print("compared", len(expected), bad)
"#
    );
    assert_eq!(run(&source).unwrap(), ["compared 16 []"]);
}
