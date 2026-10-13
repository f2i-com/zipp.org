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
