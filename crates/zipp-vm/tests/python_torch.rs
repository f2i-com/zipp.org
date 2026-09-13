//! The bundled `torch` subset: tensors, autograd, `nn`, `optim` and a
//! checkpoint round trip through the virtual filesystem, all on the
//! runtime's CPU kernels.
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
            state.set_limits(400_000_000, None);
            state.run_init()?;
            Ok(state.take_output())
        })
        .expect("spawn")
        .join()
        .expect("test thread")
}

#[test]
fn tensors_autograd_modules_and_checkpoints() {
    let source = r#"
import torch
import torch.nn.functional as F
from torch import nn

x = torch.tensor([[1.0, 2.0], [3.0, 4.0]])
print(x.shape, x.dtype, x.sum().item(), x.mean(1).tolist(), (x @ x.T).tolist(), x.argmax(1).tolist())
a = torch.tensor([1.0, 2.0, 3.0], requires_grad=True)
b = torch.tensor([4.0, 5.0, 6.0], requires_grad=True)
loss = (a * b).sum() + (a ** 2).mean() + torch.tanh(a).sum()
loss.backward()
print([round(v, 4) for v in a.grad.tolist()], b.grad.tolist())

torch.manual_seed(0)
model = nn.Sequential(nn.Linear(1, 8), nn.Tanh(), nn.Linear(8, 1))
opt = torch.optim.Adam(model.parameters(), lr=0.05)
xs = torch.linspace(-1.0, 1.0, 16).unsqueeze(1)
ys = 2.0 * xs
first = None
for step in range(60):
    opt.zero_grad()
    out = model(xs)
    l = F.mse_loss(out, ys)
    l.backward()
    opt.step()
    if first is None:
        first = l.item()
print("trained", first > 0.5, l.item() < first / 10)

torch.save({"model": model.state_dict(), "step": 60}, "ck.pt")
loaded = torch.load("ck.pt")
same = all(torch.equal(loaded["model"][k], v) for k, v in model.state_dict().items())
print("checkpoint", loaded["step"], same, sorted(loaded["model"].keys())[:2])

cell = nn.GRUCell(4, 6)
h = cell(torch.zeros(2, 4), torch.zeros(2, 6))
print(h.shape, F.softmax(torch.tensor([1.0, 2.0, 3.0]), 0).sum().item())
"#;
    let out = run(source).unwrap();
    assert_eq!(
        out,
        vec![
            "torch.Size([2, 2]) torch.float32 10.0 [1.5, 3.5] [[5.0, 11.0], [11.0, 25.0]] [1, 1]",
            "[5.0866, 6.404, 8.0099] [1.0, 2.0, 3.0]",
            "trained True True",
            "checkpoint 60 True ['0.bias', '0.weight']",
            "torch.Size([2, 6]) 1.0",
        ]
    );
}
