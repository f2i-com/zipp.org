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

#[test]
fn compiled_inference_records_graphs_and_preserves_cpu_torch() {
    let output = run(r#"
import torch
import torch.nn as nn
model = nn.Sequential(nn.Linear(2, 3), nn.ReLU(), nn.Linear(3, 1))
with torch.no_grad():
    for parameter in model.parameters():
        parameter.fill_(0.5)
x = torch.tensor([[1.0, 2.0], [3.0, 4.0]])
expected = model(x).tolist()
pending = torch.compile(model)(x)
print(pending.shape)
def done(value):
    print(value.tolist() == expected, pending.backend)
pending.submit(done)
@torch.compile
def square_sum(a):
    return (a * a + 1.0).sum()
square_sum(x).submit(lambda value: print(value.item()))
for action in [lambda: torch.tensor([1.0], device='cuda'), lambda: x.to('gpu'), lambda: model.to('cuda'), lambda: torch.device('cuda')]:
    try:
        action()
    except RuntimeError:
        print('unsupported device rejected')
try:
    pending.backward()
except NotImplementedError:
    print('inference only')
"#).unwrap();
    assert_eq!(output[0], "torch.Size([2, 1])");
    assert!(output[1].starts_with("True "));
    assert_eq!(output[2], "34.0");
    assert_eq!(&output[3..7], ["unsupported device rejected"; 4]);
    assert_eq!(output[7], "inference only");
}
