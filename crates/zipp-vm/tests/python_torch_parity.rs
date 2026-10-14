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


/// torch.randn / normal_ / torch.normal draw what PyTorch 2.11 draws from the
/// same seed, bit for bit: the scalar Box-Muller path (fewer than 16
/// elements, its second value cached in the generator and carried by
/// get_rng_state), the blocked float32 path (normal_fill_AVX2's arithmetic)
/// and the blocked float64 one, with mean and std applied as PyTorch applies
/// them; the uniform, randint and complex draws around them stay in step.
#[test]
fn normal_draws_match_pytorch_bit_for_bit() {
    let source = r#"
import torch
from torch import nn


def h(t):
    return " ".join(float(v).hex() for v in t.reshape(-1).tolist())


for seed in (0, 7):
    for n in (3, 16, 21):
        torch.manual_seed(seed)
        print("randn", seed, n, h(torch.randn(n)))
    torch.manual_seed(seed)
    print("float64", seed, h(torch.randn(5, dtype=torch.float64)), h(torch.randn(17, dtype=torch.float64)))
    torch.manual_seed(seed)
    print("normal_", seed, h(torch.empty(4).normal_(0.5, 2.0)), h(torch.empty(18).normal_(1e3, 7.7)))
    torch.manual_seed(seed)
    print("half", seed, h(torch.randn(5, dtype=torch.float16)), h(torch.randn(3, dtype=torch.bfloat16)))
    torch.manual_seed(seed)
    print("complex", seed, h(torch.view_as_real(torch.randn(3, dtype=torch.complex64))))
    torch.manual_seed(seed)
    print("mixed", seed, h(torch.rand(1)), h(torch.randn(2)), h(torch.rand(2, dtype=torch.float64)), torch.randint(0, 1000, (3,)).tolist())
print("normal", h(torch.normal(2.0, 3.0, size=(4,), generator=torch.Generator().manual_seed(4))))
torch.manual_seed(5)
torch.randn(5)
state = torch.get_rng_state()
a = torch.randn(3)
torch.set_rng_state(state)
print("state keeps the cached normal", torch.equal(a, torch.randn(3)))
torch.manual_seed(0)
print("embedding", h(nn.Embedding(3, 2).weight.detach()))
"#;
    let expected = "\
randn 0 3 0x1.8a7eb80000000p+0 -0x1.2c78a00000000p-2 -0x1.16e2920000000p+1
randn 0 16 -0x1.20370a0000000p+0 -0x1.2701140000000p+0 -0x1.0097ac0000000p-2 -0x1.bc4aba0000000p-2 0x1.b28a2a0000000p-1 0x1.624f060000000p-1 -0x1.4398da0000000p-2 -0x1.0ebf820000000p+1 0x1.4a02700000000p-2 -0x1.4369e80000000p+0 0x1.6661fe0000000p-2 0x1.3b87760000000p-2 0x1.eadeee0000000p-4 0x1.3cd7260000000p+0 0x1.1de51c0000000p+0 -0x1.fa6cf80000000p-3
randn 0 21 -0x1.20370a0000000p+0 -0x1.2701140000000p+0 -0x1.0097ac0000000p-2 -0x1.bc4aba0000000p-2 0x1.b28a2a0000000p-1 -0x1.8e1ab60000000p+0 -0x1.5d8d940000000p-2 0x1.da5e9c0000000p+0 0x1.df54aa0000000p-2 -0x1.42febe0000000p-3 0x1.7193b60000000p+0 0x1.106f420000000p-2 0x1.54e68a0000000p-3 0x1.961a000000000p+0 0x1.e481380000000p-1 -0x1.aff6660000000p-1 0x1.dd18600000000p-1 0x1.424e6e0000000p+0 0x1.00a3340000000p+1 0x1.b836740000000p-5 0x1.3c71ec0000000p-1
float64 0 0x1.8a7eb8902be3ap+0 -0x1.2c78a087c6feap-2 -0x1.16e2920a70ee5p+1 0x1.23096ca3e350dp-1 -0x1.15a341987e136p+0 -0x1.3f174ac92d21fp-1 -0x1.a9389d082941dp-1 0x1.e8e503666016dp-3 0x1.779107c3069d9p-1 -0x1.5266d8f957969p+0 0x1.50977a45ee462p+0 -0x1.7ccc83941008cp+0 -0x1.6d59bf3607db7p-1 0x1.0a3d43c8a26fap-1 -0x1.c8318cc163de5p-1 0x1.5986ed8e63e98p-1 0x1.0bff5e8d733a2p+0 0x1.3bff1c51bd9a4p-3 -0x1.7ff1c81c48b11p-1 -0x1.72602283cd6ebp-1 0x1.61c56572427d3p-4 -0x1.bf14644b163a6p+0
normal_ 0 0x1.ca7eb80000000p+1 -0x1.63c5040000000p-4 -0x1.edc5240000000p+1 0x1.a3096c0000000p+0 0x1.f2a1000000000p+9 0x1.f5aeee0000000p+9 0x1.f98ee00000000p+9 0x1.f506380000000p+9 0x1.f4a40e0000000p+9 0x1.f75dca0000000p+9 0x1.f372980000000p+9 0x1.f392000000000p+9 0x1.f1a3400000000p+9 0x1.f41f240000000p+9 0x1.fbb81c0000000p+9 0x1.f434f60000000p+9 0x1.f661280000000p+9 0x1.f269240000000p+9 0x1.f0c30c0000000p+9 0x1.eb15500000000p+9 0x1.f39b2a0000000p+9 0x1.f70d080000000p+9
half 0 0x1.8a80000000000p+0 -0x1.2c80000000000p-2 -0x1.1700000000000p+1 0x1.2300000000000p-1 -0x1.15c0000000000p+0 -0x1.6600000000000p+0 0x1.9e00000000000p-2 0x1.ae00000000000p-1
complex 0 0x1.16f33a0000000p+0 -0x1.a8ee320000000p-3 -0x1.8a672c0000000p+0 0x1.9b96940000000p-2 -0x1.88a3980000000p-1 -0x1.fa587a0000000p-1
mixed 0 0x1.fc2ab00000000p-2 0x1.a86b140000000p-3 0x1.1458720000000p-2 0x1.297dc5bebb618p-4 0x1.5f59db1d29024p-3 [683, 101, 866]
randn 7 3 -0x1.2ca2e80000000p-3 0x1.92811c0000000p-1 0x1.e4c5ce0000000p-1
randn 7 16 -0x1.a3e8ac0000000p-1 0x1.9520560000000p-2 0x1.cc3dbc0000000p-1 -0x1.636e700000000p+0 -0x1.5602020000000p-3 0x1.23fe5a0000000p-2 -0x1.483d260000000p-1 -0x1.c98d340000000p-1 0x1.da63d80000000p-1 -0x1.122ea80000000p-1 -0x1.28e36e0000000p+0 -0x1.d7336c0000000p-2 0x1.6ac5a20000000p-1 0x1.0343f00000000p+0 0x1.d7da500000000p-3 0x1.1715100000000p+0
randn 7 21 -0x1.a3e8ac0000000p-1 0x1.9520560000000p-2 0x1.cc3dbc0000000p-1 -0x1.636e700000000p+0 -0x1.5602020000000p-3 0x1.906bd40000000p-1 0x1.0a02f60000000p+0 -0x1.72f3120000000p-1 -0x1.b2a4240000000p-1 -0x1.3788b00000000p+0 -0x1.4bdd4a0000000p-4 0x1.5c8a4a0000000p+0 -0x1.4d9f9c0000000p+0 0x1.5923ba0000000p-1 -0x1.52c6ca0000000p+0 -0x1.5c19d20000000p+0 -0x1.bbc0500000000p-1 -0x1.20c57c0000000p-1 -0x1.073a2a0000000p+1 -0x1.6d48ac0000000p-2 0x1.1877f60000000p+0
float64 7 -0x1.2ca2e8774a1c0p-3 0x1.92811cc01ae83p-1 0x1.e4c5cd4ba2dcfp-1 -0x1.1d45a657cf2b6p+0 0x1.b0d79f26af9afp+0 0x1.33d449b0b2b5bp-1 0x1.749d011ade7a0p-1 -0x1.502a937321611p+0 -0x1.4fdf47365b832p-2 -0x1.5a27b878c9061p+0 0x1.cb8c2a005a32ep+0 0x1.9c0eb4528ebc7p-3 0x1.2aac16838dbc7p+1 -0x1.a4fefd811d104p-4 0x1.cf6ff3d1fa92fp-2 -0x1.aaf04bea1dd97p+0 -0x1.9647bfa50381ep+0 0x1.03dfab338259cp+1 0x1.51bdf321a248ap-1 0x1.04a1282b8276cp-1 0x1.e2a6e12548db6p-1 -0x1.39e619594ca49p+0
normal_ 7 0x1.a6ba300000000p-3 0x1.09408e0000000p+1 0x1.3262e60000000p+1 -0x1.ba8b4c0000000p+0 0x1.f3521a0000000p+9 0x1.fabcf80000000p+9 0x1.f3b0260000000p+9 0x1.f93de20000000p+9 0x1.eefb8c0000000p+9 0x1.f456380000000p+9 0x1.eeaafc0000000p+9 0x1.f5f0500000000p+9 0x1.f3adac0000000p+9 0x1.f3e8dc0000000p+9 0x1.ec15260000000p+9 0x1.f2a06a0000000p+9 0x1.f837ce0000000p+9 0x1.f5db680000000p+9 0x1.f962c60000000p+9 0x1.f9699c0000000p+9 0x1.ee9aee0000000p+9 0x1.f8fea80000000p+9
half 7 -0x1.2cc0000000000p-3 0x1.9280000000000p-1 0x1.e4c0000000000p-1 -0x1.1d40000000000p+0 0x1.b0c0000000000p+0 -0x1.ca00000000000p-1 -0x1.6c00000000000p-2 0x1.3c00000000000p+0
complex 7 -0x1.a929fe0000000p-4 0x1.1c9d120000000p-1 0x1.56c93a0000000p-1 -0x1.936f860000000p-1 0x1.3210d20000000p+0 -0x1.43f65a0000000p-1
mixed 7 0x1.11e15e0000000p-1 -0x1.4b3a000000000p-1 -0x1.a6e5720000000p-2 0x1.9a1a7726a072ep-2 0x1.3667fa5d855c0p-5 [761, 158, 631]
normal -0x1.686d140000000p+1 0x1.5946460000000p+1 0x1.17070c0000000p+3 0x1.22ae300000000p+2
state keeps the cached normal True
embedding 0x1.8a7eb80000000p+0 -0x1.2c78a00000000p-2 -0x1.16e2920000000p+1 0x1.23096c0000000p-1 -0x1.15a3420000000p+0 -0x1.660a5a0000000p+0";
    assert_lines(source, expected);
}

/// GRU's hidden update is PyTorch's CPU form, (h - n) * z + n, not the
/// algebraically equal (1 - z) * n + z * h, which rounds differently. The
/// gates here come from biases alone (zero weights), so every input to the
/// update is the same float32 value in both; GRUCell and a one-step nn.GRU
/// then print PyTorch's bytes.
#[test]
fn gru_updates_the_hidden_state_as_pytorch_does() {
    let source = r#"
import torch

H = 8
cell = torch.nn.GRUCell(1, H)
gru = torch.nn.GRU(1, H)
bias = torch.tensor([0.0] * H + [0.75, -1.5, 0.25, 0.5, -0.5, 1.25, -2.25, -1.0] + [0.3, -0.7, 1.1, -0.2, 0.9, -1.3, 0.05, 0.6])
with torch.no_grad():
    for w in (cell.weight_ih, cell.weight_hh, cell.bias_hh, gru.weight_ih_l0, gru.weight_hh_l0, gru.bias_hh_l0):
        w.zero_()
    cell.bias_ih.copy_(bias)
    gru.bias_ih_l0.copy_(bias)
x = torch.zeros(3, 1)
h = torch.tensor([[1.0, -2.0, 0.5, 3.0, -0.25, 1.5, -1.0, 0.75],
                  [0.1, 0.2, -0.3, 0.4, -0.5, 0.6, -0.7, 0.8],
                  [7.0, -3.5, 2.25, -0.125, 5.5, -6.0, 0.375, 4.0]])
print(" ".join(v.hex() for v in cell(x, h).reshape(-1).tolist()))
out, hn = gru(x.unsqueeze(0), h.unsqueeze(0))
print(" ".join(v.hex() for v in out.reshape(-1).tolist()))
"#;
    let line = "0x1.8b97360000000p-1 -0x1.b7ca7c0000000p-1 0x1.435c8e0000000p-1 0x1.caf8ee0000000p+0 0x1.67eaaa0000000p-2 0x1.f2b5e60000000p-1 -0x1.9addce0000000p-5 0x1.304ad00000000p-1 0x1.4a801c0000000p-3 -0x1.d49d1c0000000p-2 0x1.7460500000000p-3 0x1.654eb80000000p-3 0x1.0744280000000p-2 0x1.190fbc0000000p-2 -0x1.61123c0000000p-6 0x1.372d5a0000000p-1 0x1.3640e20000000p+2 -0x1.21f2680000000p+0 0x1.9d89300000000p+0 -0x1.37f5fa0000000p-3 0x1.42dc080000000p+1 -0x1.36c3e00000000p+2 0x1.4b93300000000p-4 0x1.77e7c80000000p+0";
    assert_lines(source, &format!("{line}\n{line}"));
}


/// Tensor.uniform_(from, to) draws what PyTorch 2.11 draws, bit for bit:
/// float32 computes `x * (to - from) + from` in float with the bounds
/// rounded to float32 (`rand() * (b - a) + a` rounded b - a from the double
/// and differed for (0.1, 0.7) in most elements), float64 in double; the
/// generator stays in step with rand.
#[test]
fn uniform_draws_match_pytorch_bit_for_bit() {
    let source = r#"
import torch


def h(t):
    return " ".join(float(v).hex() for v in t.tolist())


for seed in (0, 7):
    torch.manual_seed(seed)
    for dt in (torch.float32, torch.float64):
        for n, (a, b) in ((5, (0.1, 0.7)), (40, (-0.3, 0.9)), (17, (1e-3, 1.0001)), (6, (-1.0 / 3, 2.0 / 7))):
            t = torch.empty(n, dtype=dt).uniform_(a, b)
            print("uniform", seed, str(dt)[6:], n, h(t))
    g = torch.Generator().manual_seed(seed + 3)
    print("gen", seed, h(torch.empty(20).uniform_(0.25, 0.5, generator=g)), h(torch.rand(3)))
"#;
    let expected = "\
uniform 0 float32 5 0x1.974cce0000000p-2 0x1.1f329e0000000p-1 0x1.3985640000000p-3 0x1.6f0a000000000p-3 0x1.2347d40000000p-2
uniform 0 float32 40 0x1.d7f4b80000000p-2 0x1.2706dc0000000p-2 0x1.8d2cfa0000000p-1 0x1.f959f00000000p-3 0x1.d5c7280000000p-2 0x1.e614c80000000p-4 0x1.74dc480000000p-3 -0x1.17c4200000000p-2 -0x1.8ed3140000000p-4 0x1.af70e00000000p-5 0x1.49f5a80000000p-2 0x1.130c080000000p-1 0x1.51ed500000000p-1 -0x1.b54edc0000000p-4 0x1.3d36900000000p-5 0x1.092e280000000p-1 0x1.98b1fa0000000p-1 0x1.6983440000000p-3 0x1.7f7b3e0000000p-1 0x1.a056800000000p-3 0x1.7436540000000p-2 0x1.afc3260000000p-1 -0x1.06c2bc0000000p-2 -0x1.3e5a3c0000000p-4 0x1.2f4f800000000p-3 0x1.0ed3e00000000p-4 0x1.a305660000000p-1 -0x1.6c2a940000000p-4 0x1.85f1a00000000p-6 -0x1.e82dc80000000p-4 -0x1.0c391c0000000p-2 -0x1.9b99e80000000p-5 0x1.a1ab260000000p-1 0x1.22ada40000000p-1 0x1.2e7dce0000000p-1 0x1.5383240000000p-2 -0x1.f2bc800000000p-8 0x1.9b25a80000000p-2 -0x1.0a76480000000p-2 -0x1.117d4a0000000p-3
uniform 0 float32 17 0x1.f1b2e60000000p-3 0x1.a1a8060000000p-1 0x1.963ea80000000p-1 0x1.1db2b60000000p-2 0x1.ee1b0c0000000p-2 0x1.a3dc9e0000000p-1 0x1.fe8cf60000000p-1 0x1.65cac00000000p-1 0x1.22d5920000000p-1 0x1.abc58a0000000p-1 0x1.a6bc480000000p-3 0x1.2ff1580000000p-1 0x1.cfdb2e0000000p-4 0x1.3c0b820000000p-3 0x1.f09ef60000000p-3 0x1.7402ae0000000p-1 0x1.67245a0000000p-1
uniform 0 float32 6 -0x1.a841ca0000000p-3 0x1.1d7d900000000p-4 0x1.2b3bec0000000p-3 -0x1.018b180000000p-4 -0x1.88ef400000000p-7 0x1.8877a00000000p-5
uniform 0 float64 5 0x1.8e3cd3060492cp-2 0x1.4d0ecb0970e1cp-1 0x1.c5dc7777f43e4p-2 0x1.8dd25438787aep-2 0x1.27bfcbdd6d0e9p-1
uniform 0 float64 40 0x1.21ec6f5b2d3fcp-3 0x1.b4264577584a0p-6 -0x1.004776f3270cap-2 0x1.3264e1e7fb97cp-1 0x1.c2ff94f671bbcp-4 0x1.622bbf7b071c1p-2 0x1.09deacf68274dp-2 0x1.78ed13ad30862p-1 -0x1.00bf0992dae00p-2 0x1.cb1de54535942p-1 0x1.82dd9970df440p-1 0x1.3b2f7a5677f98p-1 0x1.cff949fdee040p-8 -0x1.28d69fe7c7c94p-4 0x1.c5485955651d2p-1 0x1.45632c3170ff2p-1 0x1.12f46609fc56cp-3 0x1.841be3249f1afp-2 0x1.2dcb07f4b4590p-1 -0x1.2db7475f22369p-2 0x1.99eef18af428bp-2 0x1.88dd19c78cd23p-2 -0x1.efbbe5c76df68p-3 -0x1.ab999c13eb7ccp-4 0x1.cbfd55581fab8p-1 0x1.aee64add10321p-2 -0x1.efbf9a3c622d6p-4 0x1.623b1aa46de90p-3 0x1.78e00bf569c20p-1 -0x1.5530cc43500edp-3 0x1.57c5d2a94dd6cp-1 0x1.4b270a7618b6ap-1 0x1.507a9695b4a98p-1 0x1.8123eaadcba1ap-3 0x1.22fe28a989ab2p-1 -0x1.24aea5a1e9c69p-2 0x1.033551f970fc0p-7 -0x1.2d87cfef7b7fcp-4 -0x1.adc36263ab1a2p-4 0x1.2d36b8ddc69c1p-2
uniform 0 float64 17 0x1.3de41fd6146abp-3 0x1.d7b8b2a7aa433p-3 0x1.a519c2ccdfcd6p-2 0x1.7fbd49fe952dbp-2 0x1.784aa03c4bdc0p-3 0x1.9a044f700465ap-2 0x1.09531938fa9fdp-1 0x1.39949eee60ed7p-1 0x1.902f55aa37d1ep-2 0x1.13ef9ff5185c6p-1 0x1.987123d19c520p-3 0x1.b8264128f2278p-2 0x1.b69ec7f4e0099p-1 0x1.d82988a9f32e9p-1 0x1.400a143b3c801p-5 0x1.687814693beadp-1 0x1.69fe21a95f82cp-3
uniform 0 float64 6 -0x1.c8610892ed0fbp-3 -0x1.c620735eb6dfdp-3 0x1.4b729735289e0p-4 0x1.8afa47f82d0f0p-5 -0x1.d5542f870a337p-3 0x1.158f865257d69p-2
gen 0 0x1.01176a0000000p-2 0x1.1b06980000000p-2 0x1.492cf80000000p-2 0x1.06e6840000000p-2 0x1.78bbb80000000p-2 0x1.0f63c80000000p-2 0x1.c599000000000p-2 0x1.be63140000000p-2 0x1.982bfc0000000p-2 0x1.e34a940000000p-2 0x1.7376ca0000000p-2 0x1.ccab6c0000000p-2 0x1.265bf80000000p-2 0x1.66c6a80000000p-2 0x1.0dde8a0000000p-2 0x1.759b940000000p-2 0x1.2cf4780000000p-2 0x1.f2fda60000000p-2 0x1.d8e9e00000000p-2 0x1.dff6940000000p-2 0x1.ad67100000000p-3 0x1.b32ed60000000p-1 0x1.47f4340000000p-2
uniform 7 float32 5 0x1.af0e700000000p-2 0x1.c116e00000000p-3 0x1.fb6b6e0000000p-2 0x1.f9fe4c0000000p-2 0x1.ead1400000000p-3
uniform 7 float32 40 0x1.ae3b200000000p-3 -0x1.a5dc900000000p-5 0x1.d29ebc0000000p-2 0x1.1b66a80000000p-3 0x1.716b560000000p-1 0x1.73ad560000000p-1 0x1.71ca080000000p-2 0x1.6a24600000000p-5 -0x1.ad65d80000000p-5 0x1.df74200000000p-3 0x1.0c94c80000000p-3 0x1.2100720000000p-1 -0x1.b2de1c0000000p-3 0x1.ba51920000000p-1 -0x1.5d80fa0000000p-3 0x1.84d70a0000000p-1 0x1.9103680000000p-3 0x1.379ecc0000000p-1 0x1.1150780000000p-1 0x1.4ceb3c0000000p-2 0x1.a5c3340000000p-2 0x1.82e3ea0000000p-1 0x1.d135d80000000p-2 0x1.3c958c0000000p-1 -0x1.50262e0000000p-3 0x1.744a960000000p-1 0x1.03542a0000000p-1 0x1.ced4080000000p-2 0x1.8813940000000p-2 0x1.2f59740000000p-1 0x1.b3be220000000p-1 0x1.54f8e80000000p-3 -0x1.18b1600000000p-5 0x1.313d300000000p-3 -0x1.0d11340000000p-4 0x1.2d60c20000000p-1 0x1.c4f9000000000p-9 -0x1.4b46600000000p-6 0x1.a2a91e0000000p-1 0x1.b2b6220000000p-1
uniform 7 float32 17 0x1.1db2e00000000p-1 0x1.a7fbd40000000p-2 0x1.be88000000000p-2 0x1.79722e0000000p-1 0x1.1730680000000p-5 0x1.7a0ae80000000p-4 0x1.cc97a80000000p-1 0x1.fcc9a20000000p-1 0x1.e22a3c0000000p-2 0x1.b17a100000000p-4 0x1.0745200000000p-1 0x1.1295ce0000000p-2 0x1.ff946c0000000p-2 0x1.7d78b80000000p-1 0x1.71811c0000000p-1 0x1.c493dc0000000p-2 0x1.1c70d60000000p-1
uniform 7 float32 6 0x1.eefc900000000p-5 -0x1.10d0d00000000p-2 -0x1.07ae380000000p-3 -0x1.7f13800000000p-7 -0x1.9a794c0000000p-3 -0x1.459c200000000p-3
uniform 7 float64 5 0x1.3973f8825337ep-2 0x1.11abb78bbb433p-3 0x1.b662e4c8acd44p-3 0x1.17e37a1883410p-3 0x1.1582de2b89c86p-1
uniform 7 float64 40 0x1.e3f30cd3ac08cp-4 0x1.a77ddd3a47c36p-3 0x1.8f2b72c18a5c6p-1 0x1.ffb6c50697870p-4 0x1.1969635dbc241p-2 -0x1.0f2de146fc4f8p-5 -0x1.6cb8bfadcbb62p-4 0x1.566252ef7be78p-1 0x1.01ab568bd22f8p-3 0x1.82facd2ed3929p-2 0x1.58f658f76d38cp-1 -0x1.181f9fd91e8d2p-2 0x1.37e8c9774edf0p-3 -0x1.897f9c93fea48p-5 0x1.96ab71c6810f2p-1 0x1.0c2dfafad59d2p-1 0x1.9d07a66951f40p-1 0x1.b01a353d096e6p-1 0x1.6a51889bfc83ep-1 0x1.76a5c05849beap-1 0x1.2afb81581a310p-1 0x1.5162efcc02e18p-1 0x1.b761b16eeacc3p-2 0x1.569122f3a8a46p-1 0x1.586bbf537f7a7p-2 0x1.d7c0af0132acdp-2 0x1.53de215a03471p-2 0x1.a17d7d4a44360p-7 0x1.008684664763dp-2 0x1.51ed261d53c53p-2 0x1.55d198d6b0178p-4 0x1.cf824d9a94f7cp-4 0x1.4521e1400dde9p-2 0x1.672ba5c77eef8p-5 -0x1.48297c3b708c0p-5 0x1.04ca7ab5d4130p-4 0x1.ba687aed0517ep-1 0x1.634e026cb3e9cp-4 0x1.80612012e89f5p-2 0x1.ad931e80d5616p-3
uniform 7 float64 17 0x1.08e204f5d664fp-2 0x1.e4deaf544ba0fp-1 0x1.7038da9948472p-1 0x1.80bc1ff2f3038p-1 0x1.fdfd630776c5ep-1 0x1.2be0d908e99e6p-7 0x1.b5def878dbd54p-1 0x1.7c6bc088aae28p-2 0x1.90429b0d65df1p-1 0x1.5958ab8ba41acp-3 0x1.e01a83882d8ecp-3 0x1.86e9044e18600p-2 0x1.3401819b7172dp-2 0x1.0e31fe1df68d1p-2 0x1.81644ddac74c5p-2 0x1.9b74a64579a2cp-2 0x1.772b629ca4074p-2
uniform 7 float64 6 0x1.2108dbfa98ebep-3 0x1.d9ae79b9bb816p-3 -0x1.c902663b7e41cp-4 -0x1.0b85dc7304a1cp-2 0x1.6f6116b3081aep-3 -0x1.46dd7972cdfcap-3
gen 7 0x1.7545080000000p-2 0x1.7b9c7c0000000p-2 0x1.4fffe40000000p-2 0x1.9d72100000000p-2 0x1.36c5400000000p-2 0x1.696d700000000p-2 0x1.b19c7c0000000p-2 0x1.f8249c0000000p-2 0x1.9e27d80000000p-2 0x1.5492dc0000000p-2 0x1.8c45f00000000p-2 0x1.71a79c0000000p-2 0x1.b43f700000000p-2 0x1.8ea7fa0000000p-2 0x1.b224080000000p-2 0x1.fc24480000000p-2 0x1.4ad9000000000p-2 0x1.7b79ea0000000p-2 0x1.9d70280000000p-2 0x1.7f29f60000000p-2 0x1.598e100000000p-4 0x1.9dc7540000000p-2 0x1.f841540000000p-2
";
    assert_lines(source, expected);
}


/// The error backward raises for a saved tensor written in place reads as
/// PyTorch 2.11's: a saved output named by the node that saved it (not by
/// the in-place op that wrote it since), a saved input by its history now,
/// a leaf by nothing, and PyTorch's hint for anomaly detection being off.
#[test]
fn inplace_errors_on_saved_tensors_read_as_pytorchs() {
    let source = r#"
import torch


def run(name, build):
    try:
        build().sum().backward()
        print(name, "no error")
    except RuntimeError as e:
        print(name, str(e))


def relu_out():
    x = torch.tensor([1.0, -2.0, 3.0], requires_grad=True)
    z = torch.relu(x * 2)
    z.relu_()
    return z


def exp_out():
    x = torch.tensor([1.0, -2.0, 3.0], requires_grad=True)
    z = (x * 2).exp()
    z.exp_()
    return z


def sigmoid_out_then_mul():
    x = torch.tensor([1.0, -2.0, 3.0], requires_grad=True)
    z = torch.sigmoid(x * 2)
    z.mul_(2)
    return z


def sqrt_out_then_add():
    x = torch.tensor([1.0, 4.0, 9.0], requires_grad=True)
    z = (x + 0).sqrt()
    z.add_(1)
    return z


def tanh_on_saved_input():
    x = torch.tensor([1.0, -2.0, 3.0], requires_grad=True)
    y = x * 2
    z = y * y
    y.tanh_()
    return z


def add_on_saved_input():
    w = torch.tensor([1.0, 2.0], requires_grad=True)
    v = w * 1
    u = v * v
    v.add_(1)
    return u


def leaf_without_grad():
    a = torch.tensor([1.0, 2.0])
    b = torch.tensor([3.0, 4.0], requires_grad=True)
    c = a * b
    a.add_(1)
    return c


def leaf_under_no_grad():
    w = torch.tensor([1.0, 2.0], requires_grad=True)
    u = w * w
    with torch.no_grad():
        w.add_(1)
    return u


def exp_chain():
    x = torch.tensor([1.0, 2.0], requires_grad=True)
    y = x.exp()
    z = y.exp()
    y.mul_(2)
    return z


def double_tensor():
    x = torch.tensor([1.0, 2.0], dtype=torch.float64, requires_grad=True)
    z = torch.sigmoid(x)
    z.add_(1)
    return z


for f in (relu_out, exp_out, sigmoid_out_then_mul, sqrt_out_then_add, tanh_on_saved_input, add_on_saved_input, leaf_without_grad, leaf_under_no_grad, exp_chain, double_tensor):
    run(f.__name__, f)
"#;
    let expected = "\
relu_out one of the variables needed for gradient computation has been modified by an inplace operation: [torch.FloatTensor [3]], which is output 0 of ReluBackward0, is at version 1; expected version 0 instead. Hint: enable anomaly detection to find the operation that failed to compute its gradient, with torch.autograd.set_detect_anomaly(True, check_nan=False).
exp_out one of the variables needed for gradient computation has been modified by an inplace operation: [torch.FloatTensor [3]], which is output 0 of ExpBackward0, is at version 1; expected version 0 instead. Hint: enable anomaly detection to find the operation that failed to compute its gradient, with torch.autograd.set_detect_anomaly(True, check_nan=False).
sigmoid_out_then_mul one of the variables needed for gradient computation has been modified by an inplace operation: [torch.FloatTensor [3]], which is output 0 of SigmoidBackward0, is at version 1; expected version 0 instead. Hint: enable anomaly detection to find the operation that failed to compute its gradient, with torch.autograd.set_detect_anomaly(True, check_nan=False).
sqrt_out_then_add one of the variables needed for gradient computation has been modified by an inplace operation: [torch.FloatTensor [3]], which is output 0 of SqrtBackward0, is at version 1; expected version 0 instead. Hint: enable anomaly detection to find the operation that failed to compute its gradient, with torch.autograd.set_detect_anomaly(True, check_nan=False).
tanh_on_saved_input one of the variables needed for gradient computation has been modified by an inplace operation: [torch.FloatTensor [3]], which is output 0 of TanhBackward0, is at version 1; expected version 0 instead. Hint: enable anomaly detection to find the operation that failed to compute its gradient, with torch.autograd.set_detect_anomaly(True, check_nan=False).
add_on_saved_input one of the variables needed for gradient computation has been modified by an inplace operation: [torch.FloatTensor [2]], which is output 0 of AddBackward0, is at version 1; expected version 0 instead. Hint: enable anomaly detection to find the operation that failed to compute its gradient, with torch.autograd.set_detect_anomaly(True, check_nan=False).
leaf_without_grad one of the variables needed for gradient computation has been modified by an inplace operation: [torch.FloatTensor [2]] is at version 1; expected version 0 instead. Hint: enable anomaly detection to find the operation that failed to compute its gradient, with torch.autograd.set_detect_anomaly(True, check_nan=False).
leaf_under_no_grad one of the variables needed for gradient computation has been modified by an inplace operation: [torch.FloatTensor [2]] is at version 1; expected version 0 instead. Hint: enable anomaly detection to find the operation that failed to compute its gradient, with torch.autograd.set_detect_anomaly(True, check_nan=False).
exp_chain one of the variables needed for gradient computation has been modified by an inplace operation: [torch.FloatTensor [2]], which is output 0 of ExpBackward0, is at version 1; expected version 0 instead. Hint: enable anomaly detection to find the operation that failed to compute its gradient, with torch.autograd.set_detect_anomaly(True, check_nan=False).
double_tensor one of the variables needed for gradient computation has been modified by an inplace operation: [torch.DoubleTensor [2]], which is output 0 of SigmoidBackward0, is at version 1; expected version 0 instead. Hint: enable anomaly detection to find the operation that failed to compute its gradient, with torch.autograd.set_detect_anomaly(True, check_nan=False).
";
    assert_lines(source, expected);
}

/// torch.pow with a Python-number exponent gives PyTorch 2.11's values bit
/// for bit for the exponents its CPU kernel specializes: x**2 as x*x, x**3
/// as (x*x)*x and x**-2 as 1/(x*x), rounded to the dtype at each step,
/// x**0.5 and x**-0.5 through sqrt, x**-1 as 1/x, x**1 a copy (Math.pow
/// rounded x**3 and x**-2 once, and gave +0 for (-0)**0.5): float32 and
/// float64, with signed zeros, subnormals, infinities and NaN (x**0.5 only
/// on values whose root PyTorch's MKL kernel rounds correctly), and the
/// gradients whose backward stays among those exponents (x**2, x**3,
/// x**0.5, x**-1, x**1) and first and second derivatives of x**3 and
/// (x + 1)**2 under create_graph. (The gradients of x**-2 and x**-0.5 take
/// x**-3 and x**-1.5, which both compute with pow, and can differ in the
/// last bit.)
#[test]
fn powers_match_pytorch_bit_for_bit() {
    let source = r#"
import torch

INF = float("inf")
NAN = float("nan")
SPECIAL = [0.0, -0.0, 1.0, -1.0, 2.0, -2.0, 0.5, -0.5, 1e-45, -1e-45, 1e-40, 3e38, -3e38, 1e300, 1e-300, INF, -INF, NAN, 1.1, -1.1, 7.25, 0.1, 1e19, -1e19, 4.0, 0.25, 1e-20, 6.5e4]


def vals(n, seed):
    out = []
    x = seed
    while len(out) < n:
        x = (x * 1103515245 + 12345) % 2147483648
        out.append((x / 2147483648.0 - 0.5) * (10.0 ** ((x >> 7) % 9 - 4)))
    return out


def line(label, t):
    print(label, " ".join(v.hex() for v in t.reshape(-1).tolist()))


for dt in (torch.float32, torch.float64):
    name = str(dt)[6:]
    x = torch.tensor(SPECIAL + vals(12, 11), dtype=dt)
    # (sqrt: PyTorch's CPU kernel is MKL's, not always the correctly rounded
    # root; only values both give exactly)
    roots = torch.tensor([0.0, -0.0, INF, -INF, NAN, -1.0, 4.0, 0.25, 6.25, 1e-40, 1e30, 2.0 ** -126], dtype=dt)
    for e in (2, 3, -2, -0.5, -1, 1, 0, 2.0, 3.0):
        line("%s x**%r" % (name, e), x ** e)
    line("%s x**0.5" % name, roots ** 0.5)
    line("%s pow(x, 0.5)" % name, torch.pow(roots, 0.5))
    line("%s x.pow(3)" % name, x.pow(3))
    m = x.reshape(4, 10)
    line("%s matrix x**3" % name, m ** 3)
    for e in (2, 3, 0.5, -1, 1):
        xg = torch.tensor(vals(16, 5 + e * 2 if e == int(e) else 7), dtype=dt).abs().add(0.25).requires_grad_()
        (xg ** e).sum().backward()
        line("%s grad x**%r" % (name, e), xg.grad)
    xg = torch.tensor(vals(16, 3), dtype=dt).abs().add(0.5).requires_grad_()
    y = (xg ** 3 + (xg + 1.0) ** 2 * 0.5).sum()
    (gx,) = torch.autograd.grad(y, xg, create_graph=True)
    line("%s create_graph grad" % name, gx)
    gx.sum().backward()
    line("%s second derivative" % name, xg.grad)
"#;
    let expected = "float32 x**2 0x0.0p+0 0x0.0p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+2 0x1.0000000000000p+2 0x1.0000000000000p-2 0x1.0000000000000p-2 0x0.0p+0 0x0.0p+0 0x0.0p+0 inf inf inf 0x0.0p+0 inf inf nan 0x1.35c2900000000p+0 0x1.35c2900000000p+0 0x1.a480000000000p+5 0x1.47ae160000000p-7 0x1.2ced320000000p+126 0x1.2ced320000000p+126 0x1.0000000000000p+4 0x1.0000000000000p-4 0x1.16c2000000000p-133 0x1.f7a8c40000000p+31 0x1.ff80040000000p-33 0x1.b5fa3c0000000p+13 0x1.2b8b1c0000000p+1 0x1.25913a0000000p-23 0x1.089a0e0000000p+1 0x1.ac87860000000p-25 0x1.cdbaec0000000p-24 0x1.c326800000000p+16 0x1.e3924e0000000p-5 0x1.38a8760000000p-17 0x1.b4e1f80000000p-24 0x1.6109480000000p+4
float32 x**3 0x0.0p+0 -0x0.0p+0 0x1.0000000000000p+0 -0x1.0000000000000p+0 0x1.0000000000000p+3 -0x1.0000000000000p+3 0x1.0000000000000p-3 -0x1.0000000000000p-3 0x0.0p+0 -0x0.0p+0 0x0.0p+0 inf -inf inf 0x0.0p+0 inf -inf nan 0x1.54bc6c0000000p+0 -0x1.54bc6c0000000p+0 0x1.7d14000000000p+8 0x1.0624de0000000p-10 inf -inf 0x1.0000000000000p+6 0x1.0000000000000p-6 0x0.0p+0 0x1.f38a3a0000000p+47 0x1.ff40120000000p-49 -0x1.9514ee0000000p+20 -0x1.ca3b2e0000000p+1 -0x1.bc96340000000p-35 -0x1.7c70300000000p+1 0x1.880b860000000p-37 0x1.360cec0000000p-35 -0x1.2b74900000000p+25 0x1.d5f46c0000000p-7 0x1.e8a6d80000000p-26 -0x1.1d5cde0000000p-35 -0x1.9e94a20000000p+6
float32 x**-2 inf inf 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p-2 0x1.0000000000000p-2 0x1.0000000000000p+2 0x1.0000000000000p+2 inf inf inf 0x0.0p+0 0x0.0p+0 0x0.0p+0 inf 0x0.0p+0 0x0.0p+0 nan 0x1.a723f60000000p-1 0x1.a723f60000000p-1 0x1.37b4820000000p-6 0x1.8ffffe0000000p+6 0x1.b38fbc0000000p-127 0x1.b38fbc0000000p-127 0x1.0000000000000p-4 0x1.0000000000000p+4 inf 0x1.043d4c0000000p-32 0x1.00400e0000000p+32 0x1.2b443c0000000p-14 0x1.b5929a0000000p-2 0x1.be7af20000000p+22 0x1.ef5b100000000p-2 0x1.31dd5c0000000p+24 0x1.1bdf160000000p+23 0x1.2287460000000p-17 0x1.0f0cc60000000p+4 0x1.a337e80000000p+16 0x1.2c04360000000p+23 0x1.73454a0000000p-5
float32 x**-0.5 inf -inf 0x1.0000000000000p+0 nan 0x1.6a09e60000000p-1 nan 0x1.6a09e60000000p+0 nan 0x1.6a09e60000000p+74 nan 0x1.5af2140000000p+66 0x1.10a55e0000000p-64 nan 0x0.0p+0 inf 0x0.0p+0 nan nan 0x1.e82c400000000p-1 nan 0x1.7c4dd60000000p-2 0x1.94c5840000000p+1 0x1.5bb2340000000p-32 nan 0x1.0000000000000p-1 0x1.0000000000000p+1 0x1.2a05f20000000p+33 0x1.010da80000000p-8 0x1.0010020000000p+8 nan nan nan nan 0x1.0ba59e0000000p+6 0x1.b9ceaa0000000p+5 nan 0x1.03aea80000000p+1 0x1.2198260000000p+4 nan nan
float32 x**-1 inf -inf 0x1.0000000000000p+0 -0x1.0000000000000p+0 0x1.0000000000000p-1 -0x1.0000000000000p-1 0x1.0000000000000p+1 -0x1.0000000000000p+1 inf -inf inf 0x1.225fd00000000p-128 -0x1.225fd00000000p-128 0x0.0p+0 inf 0x0.0p+0 -0x0.0p+0 nan 0x1.d1745c0000000p-1 -0x1.d1745c0000000p-1 0x1.1a7b960000000p-3 0x1.4000000000000p+3 0x1.d83c960000000p-64 -0x1.d83c960000000p-64 0x1.0000000000000p-2 0x1.0000000000000p+2 0x1.5af1d80000000p+66 0x1.021c6c0000000p-16 0x1.0020060000000p+16 -0x1.14ca060000000p-7 -0x1.4eb11a0000000p-1 -0x1.5214ce0000000p+11 -0x1.641afc0000000p-1 0x1.17d2e40000000p+12 0x1.7d3cd80000000p+11 -0x1.81ae920000000p-9 0x1.076ae00000000p+2 0x1.4798e00000000p+8 -0x1.87edda0000000p+11 -0x1.b3fe680000000p-3
float32 x**1 0x0.0p+0 -0x0.0p+0 0x1.0000000000000p+0 -0x1.0000000000000p+0 0x1.0000000000000p+1 -0x1.0000000000000p+1 0x1.0000000000000p-1 -0x1.0000000000000p-1 0x1.0000000000000p-149 -0x1.0000000000000p-149 0x1.16c2000000000p-133 0x1.c363cc0000000p+127 -0x1.c363cc0000000p+127 inf 0x0.0p+0 inf -inf nan 0x1.19999a0000000p+0 -0x1.19999a0000000p+0 0x1.d000000000000p+2 0x1.99999a0000000p-4 0x1.158e460000000p+63 -0x1.158e460000000p+63 0x1.0000000000000p+2 0x1.0000000000000p-2 0x1.79ca100000000p-67 0x1.fbd0000000000p+15 0x1.ffbffe0000000p-17 -0x1.d98b6a0000000p+6 -0x1.879eb80000000p+0 -0x1.83b19a0000000p-12 -0x1.7012200000000p+0 0x1.d468b80000000p-13 0x1.57ce780000000p-12 -0x1.53d8460000000p+8 0x1.f195300000000p-3 0x1.9019e40000000p-9 -0x1.4e6d860000000p-12 -0x1.2ca0ce0000000p+2
float32 x**0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0
float32 x**2.0 0x0.0p+0 0x0.0p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+2 0x1.0000000000000p+2 0x1.0000000000000p-2 0x1.0000000000000p-2 0x0.0p+0 0x0.0p+0 0x0.0p+0 inf inf inf 0x0.0p+0 inf inf nan 0x1.35c2900000000p+0 0x1.35c2900000000p+0 0x1.a480000000000p+5 0x1.47ae160000000p-7 0x1.2ced320000000p+126 0x1.2ced320000000p+126 0x1.0000000000000p+4 0x1.0000000000000p-4 0x1.16c2000000000p-133 0x1.f7a8c40000000p+31 0x1.ff80040000000p-33 0x1.b5fa3c0000000p+13 0x1.2b8b1c0000000p+1 0x1.25913a0000000p-23 0x1.089a0e0000000p+1 0x1.ac87860000000p-25 0x1.cdbaec0000000p-24 0x1.c326800000000p+16 0x1.e3924e0000000p-5 0x1.38a8760000000p-17 0x1.b4e1f80000000p-24 0x1.6109480000000p+4
float32 x**3.0 0x0.0p+0 -0x0.0p+0 0x1.0000000000000p+0 -0x1.0000000000000p+0 0x1.0000000000000p+3 -0x1.0000000000000p+3 0x1.0000000000000p-3 -0x1.0000000000000p-3 0x0.0p+0 -0x0.0p+0 0x0.0p+0 inf -inf inf 0x0.0p+0 inf -inf nan 0x1.54bc6c0000000p+0 -0x1.54bc6c0000000p+0 0x1.7d14000000000p+8 0x1.0624de0000000p-10 inf -inf 0x1.0000000000000p+6 0x1.0000000000000p-6 0x0.0p+0 0x1.f38a3a0000000p+47 0x1.ff40120000000p-49 -0x1.9514ee0000000p+20 -0x1.ca3b2e0000000p+1 -0x1.bc96340000000p-35 -0x1.7c70300000000p+1 0x1.880b860000000p-37 0x1.360cec0000000p-35 -0x1.2b74900000000p+25 0x1.d5f46c0000000p-7 0x1.e8a6d80000000p-26 -0x1.1d5cde0000000p-35 -0x1.9e94a20000000p+6
float32 x**0.5 0x0.0p+0 -0x0.0p+0 inf nan nan nan 0x1.0000000000000p+1 0x1.0000000000000p-1 0x1.4000000000000p+1 0x1.79c9ce0000000p-67 0x1.c6bf520000000p+49 0x1.0000000000000p-63
float32 pow(x, 0.5) 0x0.0p+0 -0x0.0p+0 inf nan nan nan 0x1.0000000000000p+1 0x1.0000000000000p-1 0x1.4000000000000p+1 0x1.79c9ce0000000p-67 0x1.c6bf520000000p+49 0x1.0000000000000p-63
float32 x.pow(3) 0x0.0p+0 -0x0.0p+0 0x1.0000000000000p+0 -0x1.0000000000000p+0 0x1.0000000000000p+3 -0x1.0000000000000p+3 0x1.0000000000000p-3 -0x1.0000000000000p-3 0x0.0p+0 -0x0.0p+0 0x0.0p+0 inf -inf inf 0x0.0p+0 inf -inf nan 0x1.54bc6c0000000p+0 -0x1.54bc6c0000000p+0 0x1.7d14000000000p+8 0x1.0624de0000000p-10 inf -inf 0x1.0000000000000p+6 0x1.0000000000000p-6 0x0.0p+0 0x1.f38a3a0000000p+47 0x1.ff40120000000p-49 -0x1.9514ee0000000p+20 -0x1.ca3b2e0000000p+1 -0x1.bc96340000000p-35 -0x1.7c70300000000p+1 0x1.880b860000000p-37 0x1.360cec0000000p-35 -0x1.2b74900000000p+25 0x1.d5f46c0000000p-7 0x1.e8a6d80000000p-26 -0x1.1d5cde0000000p-35 -0x1.9e94a20000000p+6
float32 matrix x**3 0x0.0p+0 -0x0.0p+0 0x1.0000000000000p+0 -0x1.0000000000000p+0 0x1.0000000000000p+3 -0x1.0000000000000p+3 0x1.0000000000000p-3 -0x1.0000000000000p-3 0x0.0p+0 -0x0.0p+0 0x0.0p+0 inf -inf inf 0x0.0p+0 inf -inf nan 0x1.54bc6c0000000p+0 -0x1.54bc6c0000000p+0 0x1.7d14000000000p+8 0x1.0624de0000000p-10 inf -inf 0x1.0000000000000p+6 0x1.0000000000000p-6 0x0.0p+0 0x1.f38a3a0000000p+47 0x1.ff40120000000p-49 -0x1.9514ee0000000p+20 -0x1.ca3b2e0000000p+1 -0x1.bc96340000000p-35 -0x1.7c70300000000p+1 0x1.880b860000000p-37 0x1.360cec0000000p-35 -0x1.2b74900000000p+25 0x1.d5f46c0000000p-7 0x1.e8a6d80000000p-26 -0x1.1d5cde0000000p-35 -0x1.9e94a20000000p+6
float32 grad x**2 0x1.0020b60000000p-1 0x1.8ef6d20000000p+11 0x1.a481ae0000000p-1 0x1.0002540000000p-1 0x1.feba960000000p+1 0x1.5ce01e0000000p+7 0x1.00028c0000000p-1 0x1.0049e60000000p-1 0x1.445f780000000p-1 0x1.0469740000000p-1 0x1.0044960000000p-1 0x1.43b7240000000p+6 0x1.82fa280000000p+9 0x1.5a1d040000000p+6 0x1.0005500000000p-1 0x1.00000a0000000p-1
float32 grad x**3 0x1.800c000000000p-3 0x1.49df360000000p+15 0x1.3016180000000p+3 0x1.8122fa0000000p-3 0x1.1176f00000000p+3 0x1.80afc00000000p-3 0x1.8102060000000p-3 0x1.52dc5c0000000p+18 0x1.7543600000000p-1 0x1.896f440000000p-3 0x1.80fafe0000000p-3 0x1.25b60a0000000p+6 0x1.800df80000000p-3 0x1.ef6bc00000000p+5 0x1.a4f1860000000p-3 0x1.41c6980000000p+6
float32 grad x**0.5 0x1.ffe6920000000p-1 0x1.fffabc0000000p-1 0x1.ef96780000000p-1 0x1.75e9da0000000p-7 0x1.c2036a0000000p-3 0x1.3e31380000000p-1 0x1.fff37c0000000p-1 0x1.2da92e0000000p-1 0x1.ff6fc00000000p-6 0x1.efb6ee0000000p-1 0x1.fffbe00000000p-1 0x1.eea7380000000p-6 0x1.974f460000000p-6 0x1.7aeb440000000p-4 0x1.c8fa300000000p-3 0x1.2990a80000000p-1
float32 grad x**-1 -0x1.fe4ce60000000p+3 -0x1.9d23d80000000p+3 -0x1.ff3f9c0000000p+3 -0x1.03cf8e0000000p+2 -0x1.bcbdb20000000p+2 -0x1.03eaf20000000p-15 -0x1.e2a3000000000p-18 -0x1.fffd500000000p+3 -0x1.a8dda00000000p+3 -0x1.57101a0000000p+1 -0x1.3f3bde0000000p-4 -0x1.9395320000000p-9 -0x1.cbf6da0000000p+3 -0x1.1ba4400000000p+3 -0x1.ffeee00000000p+3 -0x1.e9949c0000000p+0
float32 grad x**1 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0
float32 create_graph grad 0x1.20368c0000000p+1 0x1.2ecd380000000p+1 0x1.2018140000000p+1 0x1.b56a080000000p+1 0x1.68a66c0000000p+1 0x1.7bf8d20000000p+16 0x1.9846280000000p+18 0x1.2000560000000p+1 0x1.2cbe420000000p+1 0x1.055e8c0000000p+2 0x1.8713c60000000p+5 0x1.fe66ea0000000p+9 0x1.271e7c0000000p+1 0x1.4ece2c0000000p+1 0x1.2002240000000p+1 0x1.34183e0000000p+2
float32 second derivative 0x1.0028e40000000p+2 0x1.0aded40000000p+2 0x1.0012100000000p+2 0x1.5e96340000000p+2 0x1.31ab4e0000000p+2 0x1.0e19900000000p+10 0x1.17fab80000000p+11 0x1.0000400000000p+2 0x1.0962a80000000p+2 0x1.8a8eb80000000p+2 0x1.7fdf380000000p+4 0x1.ba83820000000p+6 0x1.0548e60000000p+2 0x1.20facc0000000p+2 0x1.00019c0000000p+2 0x1.b5ad300000000p+2
float64 x**2 0x0.0p+0 0x0.0p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+2 0x1.0000000000000p+2 0x1.0000000000000p-2 0x1.0000000000000p-2 0x1.04bd984990e6fp-299 0x1.04bd984990e6fp-299 0x1.2f8ac174d6122p-266 0x1.8df463bf3cc39p+255 0x1.8df463bf3cc39p+255 inf 0x0.0p+0 inf inf nan 0x1.35c28f5c28f5dp+0 0x1.35c28f5c28f5dp+0 0x1.a480000000000p+5 0x1.47ae147ae147cp-7 0x1.2ced32a16a1b1p+126 0x1.2ced32a16a1b1p+126 0x1.0000000000000p+4 0x1.0000000000000p-4 0x1.16c262777579cp-133 0x1.f7a8c48000000p+31 0x1.ff80031652c05p-33 0x1.b5fa3df97e3d0p+13 0x1.2b8b1ac0705e4p+1 0x1.25913a043c295p-23 0x1.089a0f4549ee2p+1 0x1.ac878606f6f41p-25 0x1.cdbaed1d635cdp-24 0x1.c3267e80540dfp+16 0x1.e3924da874800p-5 0x1.38a874df1e23dp-17 0x1.b4e1f66198df8p-24 0x1.610946fc6b71bp+4
float64 x**3 0x0.0p+0 -0x0.0p+0 0x1.0000000000000p+0 -0x1.0000000000000p+0 0x1.0000000000000p+3 -0x1.0000000000000p+3 0x1.0000000000000p-3 -0x1.0000000000000p-3 0x1.7424348ca1c9cp-449 -0x1.7424348ca1c9cp-449 0x1.4a8729fc3ddb5p-399 0x1.5ed85722d3c7ep+383 -0x1.5ed85722d3c7ep+383 inf 0x0.0p+0 inf -inf nan 0x1.54bc6a7ef9db4p+0 -0x1.54bc6a7ef9db4p+0 0x1.7d14000000000p+8 0x1.0624dd2f1a9fdp-10 0x1.4643e5ae44d13p+189 -0x1.4643e5ae44d13p+189 0x1.0000000000000p+6 0x1.0000000000000p-6 0x1.9b604aaaca625p-200 0x1.f38a3b2494000p+47 0x1.ff4010a167f58p-49 -0x1.9514f0e721a10p+20 -0x1.ca3b2be03addbp+1 -0x1.bc96352c4e111p-35 -0x1.7c70324fb274ep+1 0x1.880b8625b8565p-37 0x1.360cec9f0d6ccp-35 -0x1.2b748e2591eafp+25 0x1.d5f46bcb55e3fp-7 0x1.e8a6d4fc7f2bap-26 -0x1.1d5cdb7982772p-35 -0x1.9e94a094247f4p+6
float64 x**-2 inf inf 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p-2 0x1.0000000000000p-2 0x1.0000000000000p+2 0x1.0000000000000p+2 0x1.f6b0f092959c7p+298 0x1.f6b0f092959c7p+298 0x1.afcef51f0fb61p+265 0x1.495d39e9e2f2dp-256 0x1.495d39e9e2f2dp-256 0x0.0p+0 inf 0x0.0p+0 0x0.0p+0 nan 0x1.a723f789854a0p-1 0x1.a723f789854a0p-1 0x1.37b4824872744p-6 0x1.8ffffffffffffp+6 0x1.b38fb9daa78e4p-127 0x1.b38fb9daa78e4p-127 0x1.0000000000000p-4 0x1.0000000000000p+4 0x1.d6329f1c35ca5p+132 0x1.043d4bd7e2487p-32 0x1.00400e7811c3ap+32 0x1.2b4439f36642bp-14 0x1.b5929b8b52a63p-2 0x1.be7af22d13021p+22 0x1.ef5b0d4380fd4p-2 0x1.31dd5aff4c424p+24 0x1.1bdf1518308f2p+23 0x1.22874605546a8p-17 0x1.0f0cc56dd28d8p+4 0x1.a337e8f67e0bep+16 0x1.2c043819fcc43p+23 0x1.73454a6addd0cp-5
float64 x**-0.5 inf -inf 0x1.0000000000000p+0 nan 0x1.6a09e667f3bccp-1 nan 0x1.6a09e667f3bccp+0 nan 0x1.ac918bbfa5243p+74 nan 0x1.5af1d78b58c40p+66 0x1.10a55d5f80411p-64 nan 0x1.a2fe76a3f9475p-499 0x1.38d352e5096afp+498 0x0.0p+0 nan nan 0x1.e82c3f9d89e1bp-1 nan 0x1.7c4dd663ebb88p-2 0x1.94c583ada5b52p+1 0x1.5bb233fcb6a90p-32 nan 0x1.0000000000000p-1 0x1.0000000000000p+1 0x1.2a05f20000000p+33 0x1.010da7bb94159p-8 0x1.0010021d8ed8dp+8 nan nan nan nan 0x1.0ba59ead15227p+6 0x1.b9ceab280f79bp+5 nan 0x1.03aea89457917p+1 0x1.219826258e6efp+4 nan nan
float64 x**-1 inf -inf 0x1.0000000000000p+0 -0x1.0000000000000p+0 0x1.0000000000000p-1 -0x1.0000000000000p-1 0x1.0000000000000p+1 -0x1.0000000000000p+1 0x1.66bb7f0435c9fp+149 -0x1.66bb7f0435c9fp+149 0x1.d6329f1c35ca5p+132 0x1.225fd13c6fb43p-128 -0x1.225fd13c6fb43p-128 0x1.56e1fc2f8f359p-997 0x1.7e43c8800759bp+996 0x0.0p+0 -0x0.0p+0 nan 0x1.d1745d1745d17p-1 -0x1.d1745d1745d17p-1 0x1.1a7b9611a7b96p-3 0x1.4000000000000p+3 0x1.d83c94fb6d2acp-64 -0x1.d83c94fb6d2acp-64 0x1.0000000000000p-2 0x1.0000000000000p+2 0x1.5af1d78b58c40p+66 0x1.021c6b811646ap-16 0x1.0020053b6167fp+16 -0x1.14ca04e22d180p-7 -0x1.4eb11a73a27f3p-1 -0x1.5214ce321114cp+11 -0x1.641afb1e08d4bp-1 0x1.17d2e422f78d3p+12 0x1.7d3cd843f61c4p+11 -0x1.81ae929dee1aep-9 0x1.076ae045c1da7p+2 0x1.4798e08e1ddeep+8 -0x1.87eddb132ff81p+11 -0x1.b3fe68c186fbep-3
float64 x**1 0x0.0p+0 -0x0.0p+0 0x1.0000000000000p+0 -0x1.0000000000000p+0 0x1.0000000000000p+1 -0x1.0000000000000p+1 0x1.0000000000000p-1 -0x1.0000000000000p-1 0x1.6d601ad376ab9p-150 -0x1.6d601ad376ab9p-150 0x1.16c262777579cp-133 0x1.c363cbf21f28ap+127 -0x1.c363cbf21f28ap+127 0x1.7e43c8800759cp+996 0x1.56e1fc2f8f359p-997 inf -inf nan 0x1.199999999999ap+0 -0x1.199999999999ap+0 0x1.d000000000000p+2 0x1.999999999999ap-4 0x1.158e460913d00p+63 -0x1.158e460913d00p+63 0x1.0000000000000p+2 0x1.0000000000000p-2 0x1.79ca10c924223p-67 0x1.fbd0000000000p+15 0x1.ffbffd8adaba0p-17 -0x1.d98b6aa0c0000p+6 -0x1.879eb70800000p+0 -0x1.83b19a6666667p-12 -0x1.7012207000000p+0 0x1.d468b820c49bbp-13 0x1.57ce78624dd2fp-12 -0x1.53d84565a0000p+8 0x1.f195300000000p-3 0x1.9019e38f5c290p-9 -0x1.4e6d852f1a9fcp-12 -0x1.2ca0cda500000p+2
float64 x**0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0
float64 x**2.0 0x0.0p+0 0x0.0p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+2 0x1.0000000000000p+2 0x1.0000000000000p-2 0x1.0000000000000p-2 0x1.04bd984990e6fp-299 0x1.04bd984990e6fp-299 0x1.2f8ac174d6122p-266 0x1.8df463bf3cc39p+255 0x1.8df463bf3cc39p+255 inf 0x0.0p+0 inf inf nan 0x1.35c28f5c28f5dp+0 0x1.35c28f5c28f5dp+0 0x1.a480000000000p+5 0x1.47ae147ae147cp-7 0x1.2ced32a16a1b1p+126 0x1.2ced32a16a1b1p+126 0x1.0000000000000p+4 0x1.0000000000000p-4 0x1.16c262777579cp-133 0x1.f7a8c48000000p+31 0x1.ff80031652c05p-33 0x1.b5fa3df97e3d0p+13 0x1.2b8b1ac0705e4p+1 0x1.25913a043c295p-23 0x1.089a0f4549ee2p+1 0x1.ac878606f6f41p-25 0x1.cdbaed1d635cdp-24 0x1.c3267e80540dfp+16 0x1.e3924da874800p-5 0x1.38a874df1e23dp-17 0x1.b4e1f66198df8p-24 0x1.610946fc6b71bp+4
float64 x**3.0 0x0.0p+0 -0x0.0p+0 0x1.0000000000000p+0 -0x1.0000000000000p+0 0x1.0000000000000p+3 -0x1.0000000000000p+3 0x1.0000000000000p-3 -0x1.0000000000000p-3 0x1.7424348ca1c9cp-449 -0x1.7424348ca1c9cp-449 0x1.4a8729fc3ddb5p-399 0x1.5ed85722d3c7ep+383 -0x1.5ed85722d3c7ep+383 inf 0x0.0p+0 inf -inf nan 0x1.54bc6a7ef9db4p+0 -0x1.54bc6a7ef9db4p+0 0x1.7d14000000000p+8 0x1.0624dd2f1a9fdp-10 0x1.4643e5ae44d13p+189 -0x1.4643e5ae44d13p+189 0x1.0000000000000p+6 0x1.0000000000000p-6 0x1.9b604aaaca625p-200 0x1.f38a3b2494000p+47 0x1.ff4010a167f58p-49 -0x1.9514f0e721a10p+20 -0x1.ca3b2be03addbp+1 -0x1.bc96352c4e111p-35 -0x1.7c70324fb274ep+1 0x1.880b8625b8565p-37 0x1.360cec9f0d6ccp-35 -0x1.2b748e2591eafp+25 0x1.d5f46bcb55e3fp-7 0x1.e8a6d4fc7f2bap-26 -0x1.1d5cdb7982772p-35 -0x1.9e94a094247f4p+6
float64 x**0.5 0x0.0p+0 -0x0.0p+0 inf nan nan nan 0x1.0000000000000p+1 0x1.0000000000000p-1 0x1.4000000000000p+1 0x1.79ca10c924223p-67 0x1.c6bf526340000p+49 0x1.0000000000000p-63
float64 pow(x, 0.5) 0x0.0p+0 -0x0.0p+0 inf nan nan nan 0x1.0000000000000p+1 0x1.0000000000000p-1 0x1.4000000000000p+1 0x1.79ca10c924223p-67 0x1.c6bf526340000p+49 0x1.0000000000000p-63
float64 x.pow(3) 0x0.0p+0 -0x0.0p+0 0x1.0000000000000p+0 -0x1.0000000000000p+0 0x1.0000000000000p+3 -0x1.0000000000000p+3 0x1.0000000000000p-3 -0x1.0000000000000p-3 0x1.7424348ca1c9cp-449 -0x1.7424348ca1c9cp-449 0x1.4a8729fc3ddb5p-399 0x1.5ed85722d3c7ep+383 -0x1.5ed85722d3c7ep+383 inf 0x0.0p+0 inf -inf nan 0x1.54bc6a7ef9db4p+0 -0x1.54bc6a7ef9db4p+0 0x1.7d14000000000p+8 0x1.0624dd2f1a9fdp-10 0x1.4643e5ae44d13p+189 -0x1.4643e5ae44d13p+189 0x1.0000000000000p+6 0x1.0000000000000p-6 0x1.9b604aaaca625p-200 0x1.f38a3b2494000p+47 0x1.ff4010a167f58p-49 -0x1.9514f0e721a10p+20 -0x1.ca3b2be03addbp+1 -0x1.bc96352c4e111p-35 -0x1.7c70324fb274ep+1 0x1.880b8625b8565p-37 0x1.360cec9f0d6ccp-35 -0x1.2b748e2591eafp+25 0x1.d5f46bcb55e3fp-7 0x1.e8a6d4fc7f2bap-26 -0x1.1d5cdb7982772p-35 -0x1.9e94a094247f4p+6
float64 matrix x**3 0x0.0p+0 -0x0.0p+0 0x1.0000000000000p+0 -0x1.0000000000000p+0 0x1.0000000000000p+3 -0x1.0000000000000p+3 0x1.0000000000000p-3 -0x1.0000000000000p-3 0x1.7424348ca1c9cp-449 -0x1.7424348ca1c9cp-449 0x1.4a8729fc3ddb5p-399 0x1.5ed85722d3c7ep+383 -0x1.5ed85722d3c7ep+383 inf 0x0.0p+0 inf -inf nan 0x1.54bc6a7ef9db4p+0 -0x1.54bc6a7ef9db4p+0 0x1.7d14000000000p+8 0x1.0624dd2f1a9fdp-10 0x1.4643e5ae44d13p+189 -0x1.4643e5ae44d13p+189 0x1.0000000000000p+6 0x1.0000000000000p-6 0x1.9b604aaaca625p-200 0x1.f38a3b2494000p+47 0x1.ff4010a167f58p-49 -0x1.9514f0e721a10p+20 -0x1.ca3b2be03addbp+1 -0x1.bc96352c4e111p-35 -0x1.7c70324fb274ep+1 0x1.880b8625b8565p-37 0x1.360cec9f0d6ccp-35 -0x1.2b748e2591eafp+25 0x1.d5f46bcb55e3fp-7 0x1.e8a6d4fc7f2bap-26 -0x1.1d5cdb7982772p-35 -0x1.9e94a094247f4p+6
float64 grad x**2 0x1.0020b629126e9p-1 0x1.8ef6d24a08000p+11 0x1.a481ae2000000p-1 0x1.00025476d4c98p-1 0x1.feba960800000p+1 0x1.5ce01dd840000p+7 0x1.00028ca294467p-1 0x1.0049e616020c5p-1 0x1.445f782000000p-1 0x1.046974f2a3d71p-1 0x1.004495cc72b02p-1 0x1.43b7248d40000p+6 0x1.82fa273560000p+9 0x1.5a1d0302c0000p+6 0x1.00055039ce076p-1 0x1.00000902597f6p-1
float64 grad x**3 0x1.800bfe97eb414p-3 0x1.49df370b173dcp+15 0x1.30161661d446bp+3 0x1.8122fc3f07adap-3 0x1.1176f188f772ap+3 0x1.80afbb5b66920p-3 0x1.81020623cff50p-3 0x1.52dc5bfa45267p+18 0x1.7543608f95d80p-1 0x1.896f433ad69e6p-3 0x1.80fafb19126d1p-3 0x1.25b60884c8954p+6 0x1.800dfa02c0f00p-3 0x1.ef6bc1e2e6b42p+5 0x1.a4f1860b8896cp-3 0x1.41c698075b681p+6
float64 grad x**0.5 0x1.ffe6909300f8ap-1 0x1.fffabcf85f9d3p-1 0x1.ef9677c66b2efp-1 0x1.75e9da3b2434bp-7 0x1.c2036b3cefd7dp-3 0x1.3e3139fa7a4f4p-1 0x1.fff37b34674f2p-1 0x1.2da92de0c53cfp-1 0x1.ff6fbf2771f1dp-6 0x1.efb6efaa422fdp-1 0x1.fffbdd0fbbf36p-1 0x1.eea7378178ef5p-6 0x1.974f474d8658ep-6 0x1.7aeb43fa66d87p-4 0x1.c8fa308154efep-3 0x1.2990a89719a0dp-1
float64 grad x**-1 -0x1.fe4ce4550c198p+3 -0x1.9d23d9e219177p+3 -0x1.ff3f9da479ce0p+3 -0x1.03cf8d0222b69p+2 -0x1.bcbdadeccf29ep+2 -0x1.03eaf1e2e3e2ep-15 -0x1.e2a2fe4c83105p-18 -0x1.fffd4f6918e11p+3 -0x1.a8dda25c0355dp+3 -0x1.5710198cbf237p+1 -0x1.3f3bde9401bbap-4 -0x1.939534fd9f5aap-9 -0x1.cbf6d851249c0p+3 -0x1.1ba4409c4b0b3p+3 -0x1.ffeee236aceffp+3 -0x1.e9949b98aa8adp+0
float64 grad x**1 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0 0x1.0000000000000p+0
float64 create_graph grad 0x1.20368a9328382p+1 0x1.2ecd37f6048f0p+1 0x1.201813edd4e6ep+1 0x1.b56a09565db03p+1 0x1.68a66df17527ap+1 0x1.7bf8d233d3fb4p+16 0x1.98462ae0eb05ep+18 0x1.200056133e90ap+1 0x1.2cbe43dba14a0p+1 0x1.055e8ad3ae815p+2 0x1.8713c609eb429p+5 0x1.fe66e92dbcb36p+9 0x1.271e7d774409fp+1 0x1.4ece2a0a196acp+1 0x1.200223c89df0cp+1 0x1.34183e851696ep+2
float64 second derivative 0x1.0028e4aa3d70ap+2 0x1.0aded4aee6666p+2 0x1.00120e4f5db23p+2 0x1.5e9633e300000p+2 0x1.31ab4f0400000p+2 0x1.0e198fb110000p+10 0x1.17fab8b568000p+11 0x1.0000408e66c8bp+2 0x1.0962a7fa66666p+2 0x1.8a8eb8d300000p+2 0x1.7fdf36f400000p+4 0x1.ba83813880000p+6 0x1.0548e73400000p+2 0x1.20facb4600000p+2 0x1.00019ad52ccccp+2 0x1.b5ad302700000p+2
";
    assert_lines(source, expected);
}
