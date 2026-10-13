//! Behaviour pinned to CPU PyTorch 2.11's own output: each program below was
//! run under PyTorch (CPython 3.11) and the lines it printed are the
//! expected ones here, byte for byte.
#![cfg(feature = "python")]
use zipp_vm::frontend::compile_python_program;

fn run(source: &str) -> Vec<String> {
    let source = source.to_owned();
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(move || {
            let modules = vec![("main".to_owned(), source)];
            let mut compiled = compile_python_program("main", &modules, &[], &[], false).expect("compiles");
            let state = compiled.state_mut();
            state.set_limits(2_000_000_000, None);
            state.run_init().expect("runs");
            state.take_output().join("\n").lines().map(str::to_owned).collect()
        })
        .expect("spawn")
        .join()
        .expect("test thread")
}

fn assert_lines(source: &str, expected: &str) {
    let got = run(source);
    let want: Vec<&str> = expected.lines().collect();
    assert_eq!(got.len(), want.len(), "{got:#?}");
    for (g, w) in got.iter().zip(&want) {
        assert_eq!(g, w);
    }
}

/// An optimizer step writes into the parameter's storage, as PyTorch's
/// in-place updates do: a `detach()` alias, `.data`, a view and the tensors
/// `state_dict()` returned before the steps all see the new values.
#[test]
fn optimizers_update_parameters_in_place() {
    let source = r#"
import torch
from torch import nn


def r6(t):
    return " ".join("%.6g" % v for v in t.reshape(-1).tolist())


for name in ("SGD", "Adam", "AdamW", "RMSprop", "Adagrad", "Adamax", "NAdam", "RAdam", "Adadelta"):
    lin = nn.Linear(2, 2)
    with torch.no_grad():
        lin.weight.copy_(torch.tensor([[0.5, -0.25], [0.125, 1.0]]))
        lin.bias.copy_(torch.tensor([0.1, -0.2]))
    alias, data, view = lin.weight.detach(), lin.weight.data, lin.weight.view(4)
    sd = lin.state_dict()
    opt = getattr(torch.optim, name)(lin.parameters(), lr=0.1)
    x = torch.tensor([[1.0, 2.0], [3.0, -1.0]])
    for _ in range(2):
        opt.zero_grad()
        lin(x).pow(2).sum().backward()
        opt.step()
    w = lin.weight.detach()
    print("inplace", name, torch.equal(alias, w), torch.equal(data, w), torch.equal(view.detach(), w.reshape(4)), torch.equal(sd["weight"], w), torch.equal(sd["bias"], lin.bias.detach()), r6(w))
"#;
    let expected = "\
inplace SGD True True True True True 0.878 -0.068 0.114 0.131
inplace Adam True True True True True 0.30194 -0.052474 0.270054 0.800746
inplace AdamW True True True True True 0.301054 -0.0520896 0.269621 0.798855
inplace RMSprop True True True True True 0.35702 -0.147966 0.12858 0.447093
inplace Adagrad True True True True True 0.340802 -0.0928207 0.198688 0.835223
inplace Adamax True True True True True 0.313887 -0.0658648 0.258047 0.807788
inplace NAdam True True True True True 0.328184 -0.0803675 0.20255 0.822214
inplace RAdam True True True True True -0.371579 0.158421 0.223421 -0.343158
inplace Adadelta True True True True True 0.499359 -0.249359 0.12564 0.999359";
    assert_lines(source, expected);
}
