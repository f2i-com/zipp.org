//! The core of the bundled `torch` subset against PyTorch 2.11: in-place
//! aliasing and version checks, autograd (create_graph, hooks, grad_fn),
//! reductions, type promotion, NaN handling, indexing, printing, RNG state,
//! strided checkpoints and the wider tensor API. Every expected value was
//! computed by CPython 3.11 with PyTorch 2.11 and written here as a literal
//! by `fixtures/torch_core/gen_tests.py` (edit the cases there and rerun it);
//! each Python program checks Zipp's result against it and prints
//! `label True`.
#![cfg(feature = "python")]
use zipp_vm::frontend::compile_python_program;

const NORM: &str = r#"import math
import torch

def norm(v):
    if isinstance(v, torch.Tensor):
        flat = v.detach().reshape(-1).tolist()
        return ["T", list(v.shape), str(v.dtype), flat]
    if isinstance(v, torch.dtype):
        return str(v)
    if isinstance(v, torch.Size):
        return list(v)
    if isinstance(v, (list, tuple)):
        return [norm(x) for x in v]
    if isinstance(v, dict):
        return [[k, norm(v[k])] for k in sorted(v)]
    return v

def close(got, want, tol):
    if isinstance(want, list):
        return isinstance(got, list) and len(got) == len(want) and all(close(g, w, tol) for g, w in zip(got, want))
    if isinstance(want, bool) or want is None or isinstance(want, str):
        return got == want
    if isinstance(want, int) and not isinstance(want, bool):
        return (isinstance(got, int) and not isinstance(got, bool) and got == want) or (isinstance(got, float) and got == want and tol > 0)
    if isinstance(want, float):
        if not isinstance(got, (int, float)) or isinstance(got, bool):
            return False
        if math.isnan(want):
            return math.isnan(got)
        if math.isinf(want):
            return got == want
        return abs(got - want) <= tol * max(1.0, abs(want))
    return got == want

def check(label, fn, want, tol=1e-6):
    try:
        got = norm(fn())
    except Exception as e:
        got = ["E", type(e).__name__]
    ok = close(got, want, tol)
    print(label, ok if ok else "False got=%r want=%r" % (got, want))

def err(fn, *words):
    """The exception type, and whether its message holds every word."""
    try:
        fn()
    except Exception as e:
        return [type(e).__name__, all(w in str(e) for w in words)]
    return "no error"
"#;

fn run(body: &str) -> Vec<String> {
    let source = format!("{NORM}\n{body}");
    let files = vec![
        (
            "noncontig.pt".to_owned(),
            include_bytes!("fixtures/torch_core/noncontig.pt").to_vec(),
        ),
        (
            "torch_globals.pt".to_owned(),
            include_bytes!("fixtures/torch_optim/torch_globals.pt").to_vec(),
        ),
    ];
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(move || {
            let modules = vec![("main".to_owned(), source)];
            let mut compiled = compile_python_program("main", &modules, &files, &[], false)?;
            let state = compiled.state_mut();
            state.set_limits(2_000_000_000, None);
            state.run_init()?;
            Ok::<_, String>(state.take_output())
        })
        .expect("spawn")
        .join()
        .expect("test thread")
        .unwrap()
}

fn assert_all_true(out: &[String], count: usize) {
    let failed: Vec<&String> = out.iter().filter(|line| !line.ends_with(" True")).collect();
    assert!(failed.is_empty(), "{failed:#?}");
    assert_eq!(out.len(), count, "{out:#?}");
}

/// In-place arithmetic writes into the tensor's storage, so `.data`, `detach()` and reshape views see it (manual SGD via `w.data.add_`), with PyTorch's dtype rule for the result.
#[test]
fn inplace_ops_write_through_data_detach_and_view_aliases() {
    let out = run(r#"
def aliases():
    p = torch.ones(3, requires_grad=True)
    p.data.add_(1); p.data.mul_(3); p.data.sub_(torch.ones(3), alpha=2); p.data.div_(2); p.data.clamp_(0, 1.5)
    q = torch.ones(2); d = q.detach(); d.add_(1)
    x = torch.zeros(4); v = x.view(2, 2); x.add_(1); v.mul_(3)
    y = torch.zeros(2, 3); y.view(-1).add_(1.0)
    z = torch.zeros(4); z.reshape(2, 2).mul_(0).add_(5)
    return p, q, x, v, y, z
def manual_sgd():
    w = torch.tensor([1.0, 2.0], requires_grad=True)
    for _ in range(3):
        (w * w).sum().backward()
        w.data.add_(w.grad, alpha=-0.1)
        w.grad.zero_()
    w2 = torch.tensor([1.0, 2.0], requires_grad=True)
    for _ in range(3):
        (w2 * w2).sum().backward()
        with torch.no_grad():
            w2 -= 0.1 * w2.grad
            w2.grad.zero_()
    return w, w2, w2.is_leaf, w2.requires_grad
def iops():
    a = torch.tensor([1.0, 2.0, 3.0]); b = a; a += 1; a -= torch.tensor([0.5, 0.5, 0.5]); a *= 2; a /= 4
    c = torch.tensor([5, 7]); c //= 2; e = torch.tensor([2.0, 3.0]); e **= 2
    return a, b is a, c, e
def view_nonleaf():
    a = torch.tensor([1., 2., 3., 4.], requires_grad=True); x = a * 1
    x.view(2, 2).mul_(2)
    (x * a).sum().backward()
    return a.grad, type(x.grad_fn).__name__
def nonleaf_ops():
    out = []
    a = torch.tensor([1., 2.], requires_grad=True); y = a * 2; y.pow_(2); y.sum().backward(); out.append(a.grad)
    a = torch.tensor([1., 2.], requires_grad=True); w = torch.tensor([3., 4.], requires_grad=True); y = a * 2; y.mul_(w); y.sum().backward(); out.append((a.grad, w.grad))
    a = torch.tensor([1., 2.], requires_grad=True); y = a * 2; y.sigmoid_(); y.sum().backward(); out.append(a.grad)
    a = torch.tensor([1., 2.], requires_grad=True); b = torch.tensor([5., 6.], requires_grad=True); y = a * 2; y.copy_(b * 3); (y * y).sum().backward(); out.append((a.grad, b.grad))
    a = torch.tensor([1., 2., 3.], requires_grad=True); v = torch.tensor(4.0, requires_grad=True); y = a * 1; y[0] = v; (y * y).sum().backward(); out.append((a.grad, v.grad))
    return out
check('aliases', lambda: aliases(), [['T', [3], 'torch.float32', [1.5, 1.5, 1.5]], ['T', [2], 'torch.float32', [2.0, 2.0]], ['T', [4], 'torch.float32', [3.0, 3.0, 3.0, 3.0]], ['T', [2, 2], 'torch.float32', [3.0, 3.0, 3.0, 3.0]], ['T', [2, 3], 'torch.float32', [1.0, 1.0, 1.0, 1.0, 1.0, 1.0]], ['T', [4], 'torch.float32', [5.0, 5.0, 5.0, 5.0]]], 1e-06)
check('manual_sgd', lambda: manual_sgd(), [['T', [2], 'torch.float32', [0.5119999647140503, 1.0239999294281006]], ['T', [2], 'torch.float32', [0.5119999647140503, 1.0239999294281006]], True, True], 1e-06)
check('iops', lambda: iops(), [['T', [3], 'torch.float32', [0.75, 1.25, 1.75]], True, ['T', [2], 'torch.int64', [2, 3]], ['T', [2], 'torch.float32', [4.0, 9.0]]], 1e-06)
check('int_iadd_float', lambda: err(lambda: torch.tensor([1, 2]).__iadd__(1.5), "can't be cast"), ['RuntimeError', True], 0)
check('f32_iadd_f64', lambda: (lambda a: (a.__iadd__(torch.tensor([1., 2.], dtype=torch.float64)), a)[1])(torch.tensor([1., 2.])), ['T', [2], 'torch.float32', [2.0, 4.0]], 0)
check('view_nonleaf', lambda: view_nonleaf(), [['T', [4], 'torch.float32', [4.0, 8.0, 12.0, 16.0]], 'CopySlices'], 1e-06)
check('nonleaf_ops', lambda: nonleaf_ops(), [['T', [2], 'torch.float32', [8.0, 16.0]], [['T', [2], 'torch.float32', [6.0, 8.0]], ['T', [2], 'torch.float32', [2.0, 4.0]]], ['T', [2], 'torch.float32', [0.20998725295066833, 0.035325467586517334]], [['T', [2], 'torch.float32', [0.0, 0.0]], ['T', [2], 'torch.float32', [90.0, 108.0]]], [['T', [3], 'torch.float32', [0.0, 4.0, 6.0]], ['T', [], 'torch.float32', [8.0]]]], 0.0001)
"#);
    assert_all_true(&out, 7);
}

/// Writing in place into a tensor backward still needs raises PyTorch's version error at backward; zero_/fill_/copy_/uniform_/setitem on a leaf that requires grad raise outside no_grad; `.data` writes stay invisible to the check.
#[test]
fn inplace_writes_to_saved_tensors_and_leaves_raise() {
    let out = run(r#"
def saved(name):
    a = torch.tensor([1., 2.], requires_grad=True)
    if name == "mulself":
        y = a * 2; y.mul_(y); out = y.sum()
    elif name == "exp_out":
        z = (a * 2).exp(); z.add_(1); out = z.sum()
    elif name == "mulsave":
        y = a * 2; z = y * a; y.add_(5); out = z.sum()
    elif name == "zero_saved":
        y = a * 2; z = y * y; y.zero_(); out = z.sum()
    elif name == "setitem_saved":
        t = torch.tensor([3.0, 4.0]); out = (a * t).sum(); t[0] = 100.0
    elif name == "matmul_saved":
        m = torch.ones(2, 2); out = (a @ m).sum(); m.add_(1)
    elif name == "nograd_leaf":
        out = (a * a).sum()
        with torch.no_grad():
            a.add_(1)
    elif name == "data_write":
        out = (a * a).sum(); a.data.add_(1)
    elif name == "add_unsaved":
        y = a * 2; z = y + 1; y.add_(5); out = z.sum()
    elif name == "relu_input":
        y = a * 2; z = y.relu(); y.add_(5); out = z.sum()
    out.backward()
    return a.grad
def leaf(op):
    t = torch.ones(2, requires_grad=True)
    return err(lambda: op(t), "leaf Variable")
check('mulself', lambda: err(lambda: saved('mulself'), 'modified by an inplace operation'), ['RuntimeError', True], 0)
check('exp_out', lambda: err(lambda: saved('exp_out'), 'modified by an inplace operation'), ['RuntimeError', True], 0)
check('mulsave', lambda: err(lambda: saved('mulsave'), 'modified by an inplace operation'), ['RuntimeError', True], 0)
check('zero_saved', lambda: err(lambda: saved('zero_saved'), 'modified by an inplace operation'), ['RuntimeError', True], 0)
check('setitem_saved', lambda: err(lambda: saved('setitem_saved'), 'modified by an inplace operation'), ['RuntimeError', True], 0)
check('matmul_saved', lambda: err(lambda: saved('matmul_saved'), 'modified by an inplace operation'), ['RuntimeError', True], 0)
check('nograd_leaf', lambda: err(lambda: saved('nograd_leaf'), 'modified by an inplace operation'), ['RuntimeError', True], 0)
check('data_write', lambda: saved('data_write'), ['T', [2], 'torch.float32', [4.0, 6.0]], 1e-06)
check('add_unsaved', lambda: saved('add_unsaved'), ['T', [2], 'torch.float32', [2.0, 2.0]], 1e-06)
check('relu_input', lambda: saved('relu_input'), ['T', [2], 'torch.float32', [2.0, 2.0]], 1e-06)
check('leaf_zero_', lambda: leaf(lambda t: t.zero_()), ['RuntimeError', True], 0)
check('leaf_fill_', lambda: leaf(lambda t: t.fill_(2)), ['RuntimeError', True], 0)
check('leaf_copy_', lambda: leaf(lambda t: t.copy_(torch.ones(2))), ['RuntimeError', True], 0)
check('leaf_uniform_', lambda: leaf(lambda t: t.uniform_()), ['RuntimeError', True], 0)
check('leaf_setitem', lambda: leaf(lambda t: t.__setitem__(0, 5.0)), ['RuntimeError', True], 0)
check('leaf_add_', lambda: leaf(lambda t: t.add_(1)), ['RuntimeError', True], 0)
check('view_of_leaf', lambda: err(lambda: torch.ones(4, requires_grad=True).view(2, 2).mul_(2), 'view of a leaf Variable'), ['RuntimeError', True], 0)
check('leaf_under_no_grad', lambda: (lambda t: (torch.no_grad().__enter__(), t.zero_(), t.fill_(3), t.__setitem__(0, 4.0), torch.set_grad_enabled(True), t)[-1])(torch.ones(2, requires_grad=True)), ['T', [2], 'torch.float32', [4.0, 3.0]], 0)
"#);
    assert_all_true(&out, 18);
}

/// float32 sum/mean/prod (whole and per dim) accumulate in float64 and round once: within 1e-6 of the exact value where float32 accumulation drifted to 0.100958.
#[test]
fn float32_reductions_accumulate_in_double_precision() {
    let out = run(r#"
check('mean_1e6', lambda: torch.full((10 ** 6,), 0.1).mean(), ['T', [], 'torch.float32', [0.10000001639127731]], 2e-07)
check('sum_1e5', lambda: torch.full((100000,), 0.1).sum(), ['T', [], 'torch.float32', [10000.0009765625]], 2e-07)
check('mse', lambda: ((torch.full((1000, 1000), 0.3) - torch.full((1000, 1000), 0.2)) ** 2).mean(), ['T', [], 'torch.float32', [0.01000000350177288]], 1e-06)
check('sum_dim0', lambda: torch.full((10000, 3), 0.1).sum(0), ['T', [3], 'torch.float32', [1000.0001831054688, 1000.0001831054688, 1000.0001831054688]], 2e-07)
check('sum_dim1', lambda: torch.full((3, 10000), 0.1).sum(1), ['T', [3], 'torch.float32', [1000.0001220703125, 1000.0001220703125, 1000.0001220703125]], 2e-07)
check('mean_dims', lambda: torch.full((20, 500, 2), 0.1).mean((0, 1)), ['T', [2], 'torch.float32', [0.10000001639127731, 0.10000001639127731]], 2e-07)
check('prod', lambda: torch.full((1000,), 1.001).prod(), ['T', [], 'torch.float32', [2.717038869857788]], 1e-05)
"#);
    assert_all_true(&out, 7);
}

/// var/std take (dim, unbiased, keepdim) positionally, a lone bool as `unbiased`, and correction=; float32 is computed in double as PyTorch does.
#[test]
fn var_and_std_take_dim_unbiased_keepdim_and_correction() {
    let out = run(r#"
x = torch.tensor([[1.0, 2.0, 4.0], [3.0, 5.0, 9.0]])
check('var_dim_unbiased', lambda: x.var(1, False), ['T', [2], 'torch.float32', [1.5555555820465088, 6.222222328186035]], 1e-06)
check('std_fn', lambda: torch.std(x, 0, False), ['T', [3], 'torch.float32', [1.0, 1.5, 2.5]], 1e-06)
check('var_bool', lambda: x.var(False), ['T', [], 'torch.float32', [6.666666507720947]], 1e-06)
check('var_keepdim', lambda: x.var(1, True, True), ['T', [2, 1], 'torch.float32', [2.3333332538604736, 9.333333015441895]], 1e-06)
check('std_correction', lambda: x.std(correction=0), ['T', [], 'torch.float32', [2.58198881149292]], 1e-06)
check('var_correction2', lambda: x.var(dim=1, correction=2), ['T', [2], 'torch.float32', [4.666666507720947, 18.66666603088379]], 1e-06)
check('var_fn_keepdim', lambda: torch.var(x, 1, False, True), ['T', [2, 1], 'torch.float32', [1.5555555820465088, 6.222222328186035]], 1e-06)
check('std_mean', lambda: torch.std_mean(x, 1), [['T', [2], 'torch.float32', [1.5275251865386963, 3.0550503730773926]], ['T', [2], 'torch.float32', [2.3333332538604736, 5.666666507720947]]], 1e-06)
check('var_mean', lambda: torch.var_mean(x, dim=0, correction=0), [['T', [3], 'torch.float32', [1.0, 2.25, 6.25]], ['T', [3], 'torch.float32', [2.0, 3.5, 6.5]]], 1e-06)
check('std_default', lambda: x.std(0), ['T', [3], 'torch.float32', [1.4142135381698608, 2.1213202476501465, 3.535533905029297]], 1e-06)
check('var_default', lambda: x.var(), ['T', [], 'torch.float32', [8.0]], 1e-06)
"#);
    assert_all_true(&out, 11);
}

/// backward/autograd.grad with create_graph=True return gradients with history (a gradient penalty differentiates through them); second derivatives of the common ops match PyTorch; backward accepts retain_graph and inputs.
#[test]
fn create_graph_records_differentiable_gradients() {
    let out = run(r#"
import torch.nn.functional as F
def penalty():
    w = torch.tensor([2.0, -1.0], requires_grad=True)
    x = torch.tensor([1.0, 3.0], requires_grad=True)
    out = (w * x * x).sum()
    (gx,) = torch.autograd.grad(out, x, create_graph=True)
    (out + (gx ** 2).sum()).backward()
    return gx.requires_grad, w.grad, x.grad
def second(f):
    x = torch.tensor([0.5, 1.5, 2.0], requires_grad=True)
    (g,) = torch.autograd.grad(f(x).sum(), x, create_graph=True)
    (h,) = torch.autograd.grad(g.sum(), x)
    return h
def backward_create():
    x = torch.tensor([1.0, 2.0], requires_grad=True)
    (x ** 3).sum().backward(create_graph=True)
    g = x.grad
    g.sum().backward()
    return x.grad
def backward_inputs():
    a = torch.tensor([1.0, 2.0], requires_grad=True); b = torch.tensor([3.0, 4.0], requires_grad=True)
    (a * b).sum().backward(inputs=[a])
    l = (a * b).sum(); l.backward(retain_graph=True); l.backward()
    return a.grad, b.grad
check('penalty', lambda: penalty(), [True, ['T', [2], 'torch.float32', [17.0, -63.0]], ['T', [2], 'torch.float32', [36.0, 18.0]]], 1e-05)
check('backward_create', lambda: backward_create(), ['T', [2], 'torch.float32', [9.0, 24.0]], 1e-05)
check('backward_inputs', lambda: backward_inputs(), [['T', [2], 'torch.float32', [9.0, 12.0]], ['T', [2], 'torch.float32', [2.0, 4.0]]], 1e-06)
check('d2_exp', lambda: second(torch.exp), ['T', [3], 'torch.float32', [1.6487212181091309, 4.481688976287842, 7.389056205749512]], 0.0001)
check('d2_tanh', lambda: second(torch.tanh), ['T', [3], 'torch.float32', [-0.7268619537353516, -0.32713258266448975, -0.13621866703033447]], 0.0001)
check('d2_sigmoid', lambda: second(torch.sigmoid), ['T', [3], 'torch.float32', [-0.05755680054426193, -0.09473022073507309, -0.07996252179145813]], 0.0001)
check('d2_sin', lambda: second(torch.sin), ['T', [3], 'torch.float32', [-0.4794255495071411, -0.9974949955940247, -0.9092974066734314]], 0.0001)
check('d2_log', lambda: second(torch.log), ['T', [3], 'torch.float32', [-4.0, -0.4444444477558136, -0.25]], 0.0001)
check('d2_sqrt', lambda: second(torch.sqrt), ['T', [3], 'torch.float32', [-0.7071067690849304, -0.1360827535390854, -0.0883883461356163]], 0.0001)
check('d2_pow3', lambda: second(lambda t: t ** 3), ['T', [3], 'torch.float32', [3.0, 9.0, 12.0]], 0.0001)
check('d2_recip', lambda: second(lambda t: 1 / t), ['T', [3], 'torch.float32', [16.0, 0.5925926566123962, 0.25]], 0.0001)
check('d2_softmax', lambda: second(lambda t: torch.softmax(t, 0)), ['T', [3], 'torch.float32', [0.0, 0.0, 0.0]], 0.0001)
check('d2_gelu', lambda: second(F.gelu), ['T', [3], 'torch.float32', [0.6161143183708191, -0.0323793888092041, -0.10798193514347076]], 0.0001)
check('d2_silu', lambda: second(F.silu), ['T', [3], 'torch.float32', [0.441228985786438, 0.15619762241840363, 0.050062213093042374]], 0.0001)
check('d2_norm', lambda: second(lambda t: t.norm() * t), ['T', [3], 'torch.float32', [3.2434592247009277, 3.4546611309051514, 3.5602622032165527]], 0.0001)
check('d2_mm', lambda: second(lambda t: (t.reshape(1, 3) @ t.reshape(3, 1)).reshape(1) * t), ['T', [3], 'torch.float32', [19.0, 25.0, 28.0]], 0.0001)
check('d2_slice', lambda: second(lambda t: t[1:] ** 2), ['T', [3], 'torch.float32', [0.0, 2.0, 2.0]], 0.0001)
check('d2_index', lambda: second(lambda t: t[[0, 2]] ** 3), ['T', [3], 'torch.float32', [3.0, 0.0, 12.0]], 0.0001)
check('d2_log_softmax', lambda: second(lambda t: torch.log_softmax(t, 0) * t), ['T', [3], 'torch.float32', [0.6341450810432434, 0.00550311803817749, -0.6396481990814209]], 0.0001)
check('d2_mean', lambda: second(lambda t: (t - t.mean()) ** 2), ['T', [3], 'torch.float32', [0.0, 0.0, 0.0]], 0.0001)
check('d2_max_dim', lambda: second(lambda t: t.reshape(1, 3).max(1).values ** 2), ['T', [3], 'torch.float32', [0.0, 0.0, 2.0]], 0.0001)
check('d2_where', lambda: second(lambda t: torch.where(t > 1, t ** 2, t ** 3)), ['T', [3], 'torch.float32', [3.0, 2.0, 2.0]], 0.0001)
check('d2_cat', lambda: second(lambda t: torch.cat([t, t ** 2]) ** 2), ['T', [3], 'torch.float32', [5.0, 29.0, 50.0]], 0.0001)
check('d2_cumsum', lambda: second(lambda t: t.cumsum(0) ** 2), ['T', [3], 'torch.float32', [12.0, 10.0, 6.0]], 0.0001)
"#);
    assert_all_true(&out, 24);
}

/// Float arange has ceil((end - start) / step) elements, element i = start + i * step (no accumulated rounding).
#[test]
fn float_arange_has_ceil_length_and_exact_elements() {
    let out = run(r#"
check('len_0_1', lambda: torch.arange(0, 1, 0.1).shape, [10], 0)
check('values_0_09', lambda: torch.arange(0, 0.9, 0.3), ['T', [3], 'torch.float32', [0.0, 0.30000001192092896, 0.6000000238418579]], 1e-07)
check('len_0_10', lambda: torch.arange(0, 10, 0.01).shape, [1000], 0)
check('half_steps', lambda: torch.arange(1, 2.5, 0.5), ['T', [3], 'torch.float32', [1.0, 1.5, 2.0]], 0)
check('negative', lambda: torch.arange(-1, 1, 0.25), ['T', [8], 'torch.float32', [-1.0, -0.75, -0.5, -0.25, 0.0, 0.25, 0.5, 0.75]], 0)
check('last_below_end', lambda: torch.arange(0, 1, 0.1)[-1].item() < 1, True, 0)
check('int_default', lambda: torch.arange(5), ['T', [5], 'torch.int64', [0, 1, 2, 3, 4]], 0)
check('desc', lambda: torch.arange(10, 0, -3), ['T', [4], 'torch.int64', [10, 7, 4, 1]], 0)
check('tensor_end', lambda: torch.arange(torch.tensor(4)), ['T', [4], 'torch.int64', [0, 1, 2, 3]], 0)
check('float64', lambda: torch.arange(3, dtype=torch.float64), ['T', [3], 'torch.float64', [0.0, 1.0, 2.0]], 0)
"#);
    assert_all_true(&out, 10);
}

/// min/max/amax/argmin over uint8 and bool tensors (the reduction no longer starts from an Infinity a Uint8Array cannot hold).
#[test]
fn uint8_and_bool_min_max_start_from_the_data() {
    let out = run(r#"
img = torch.tensor([[3, 7], [9, 200]], dtype=torch.uint8)
check('min', lambda: img.min(), ['T', [], 'torch.uint8', [3]], 0)
check('min_dim', lambda: img.min(1), [['T', [2], 'torch.uint8', [3, 9]], ['T', [2], 'torch.int64', [0, 0]]], 0)
check('max', lambda: img.max(), ['T', [], 'torch.uint8', [200]], 0)
check('max_dim0', lambda: img.max(0), [['T', [2], 'torch.uint8', [9, 200]], ['T', [2], 'torch.int64', [1, 1]]], 0)
check('amax', lambda: img.amax(1), ['T', [2], 'torch.uint8', [7, 200]], 0)
check('argmin', lambda: img.argmin(1), ['T', [2], 'torch.int64', [0, 0]], 0)
check('bool_min', lambda: torch.tensor([True, True]).min(), ['T', [], 'torch.bool', [True]], 0)
check('bool_max', lambda: torch.tensor([False, True]).max(), ['T', [], 'torch.bool', [True]], 0)
check('bool_min_dim', lambda: torch.tensor([True, False]).min(0), [['T', [], 'torch.bool', [False]], ['T', [], 'torch.int64', [1]]], 0)
"#);
    assert_all_true(&out, 9);
}

/// The 2-norm and p-norms of a zero vector, std of constant data, x**0 at 0 and 0**e give PyTorch's zero (masked) gradients, not NaN.
#[test]
fn norm_std_and_pow_gradients_are_zero_not_nan_at_zero() {
    let out = run(r#"
def grad(f, *vals):
    xs = [torch.tensor(v, requires_grad=True) for v in vals]
    f(*xs).sum().backward()
    return [x.grad for x in xs]
check('norm0', lambda: grad(lambda x: x.norm(), [0.0, 0.0]), [['T', [2], 'torch.float32', [0.0, 0.0]]], 1e-06)
check('std_const', lambda: grad(lambda y: y.std(), [1.0, 1.0]), [['T', [2], 'torch.float32', [0.0, 0.0]]], 1e-06)
check('pow0_at0', lambda: grad(lambda z: z ** 0, [0.0, 2.0]), [['T', [2], 'torch.float32', [0.0, 0.0]]], 1e-06)
check('exponent_at_base0', lambda: grad(lambda e: torch.tensor([0.0, 2.0, 0.0]) ** e, [1.0, 2.0, -1.0]), [['T', [3], 'torch.float32', [0.0, 2.7725887298583984, -float("inf")]]], 1e-05)
check('scalar_base0', lambda: grad(lambda e: 0 ** e, [1.0, 0.0, -1.0]), [['T', [3], 'torch.float32', [0.0, 0.0, -float("inf")]]], 1e-06)
check('scalar_base2', lambda: grad(lambda e: 2 ** e, [1.0, 0.0, -1.0]), [['T', [3], 'torch.float32', [1.3862943649291992, 0.6931471824645996, 0.3465735912322998]]], 1e-05)
check('tensor_exp0', lambda: grad(lambda z: z ** torch.tensor([0.0, 0.0]), [0.0, 2.0]), [['T', [2], 'torch.float32', [0.0, 0.0]]], 1e-06)
check('norm_rows', lambda: grad(lambda m: m.norm(dim=1), [[0.0, 0.0], [3.0, 4.0]]), [['T', [2, 2], 'torch.float32', [0.0, 0.0, 0.6000000238418579, 0.800000011920929]]], 1e-06)
check('norm_p1', lambda: grad(lambda x: x.norm(p=1), [0.0, 0.0]), [['T', [2], 'torch.float32', [0.0, 0.0]]], 1e-06)
check('norm_p3', lambda: grad(lambda x: x.norm(p=3), [0.0, 0.0]), [['T', [2], 'torch.float32', [0.0, 0.0]]], 1e-06)
check('norm_inf', lambda: grad(lambda x: x.norm(p=float('inf')), [0.0, 1.0]), [['T', [2], 'torch.float32', [0.0, 1.0]]], 1e-06)
check('norm_p3_values', lambda: grad(lambda x: x.norm(p=3), [1.0, -2.0, 3.0]), [['T', [3], 'torch.float32', [0.09172019362449646, -0.36688077449798584, 0.8254817724227905]]], 1e-05)
check('std_rows', lambda: grad(lambda x: x.std(1), [[1.0, 1.0], [2.0, 3.0]]), [['T', [2, 2], 'torch.float32', [0.0, 0.0, -0.7071067690849304, 0.7071067690849304]]], 1e-05)
"#);
    assert_all_true(&out, 13);
}

/// A leaf's .grad is its own tensor on first assignment (not shared with another leaf or the caller's `gradient`), and later backwards accumulate into it in place.
#[test]
fn grad_buffers_do_not_share_storage() {
    let out = run(r#"
def shared():
    a = torch.tensor([1.0, 2.0], requires_grad=True); b = torch.tensor([3.0, 4.0], requires_grad=True)
    (a + b).sum().backward()
    a.grad.zero_()
    g = torch.tensor([1.0, 1.0]); c = torch.tensor([1.0, 2.0], requires_grad=True)
    (c + 0).backward(g); c.grad.zero_()
    d = torch.tensor([1.0, 2.0], requires_grad=True)
    (d * 2).sum().backward(); first = d.grad; (d * 3).sum().backward()
    return b.grad, g, first, first is d.grad
check('shared', lambda: shared(), [['T', [2], 'torch.float32', [1.0, 1.0]], ['T', [2], 'torch.float32', [1.0, 1.0]], ['T', [2], 'torch.float32', [5.0, 5.0]], True], 0)
"#);
    assert_all_true(&out, 1);
}

/// &, |, ^, ~, << and >> on integer tensors are bitwise (logical on bool); float operands are refused.
#[test]
fn integer_bitwise_operators_are_bitwise() {
    let out = run(r#"
a, b = torch.tensor([6, 5]), torch.tensor([3, 1])
check('and', lambda: a & b, ['T', [2], 'torch.int64', [2, 1]], 0)
check('or_scalar', lambda: a | 1, ['T', [2], 'torch.int64', [7, 5]], 0)
check('invert', lambda: ~torch.tensor([6, 0]), ['T', [2], 'torch.int64', [-7, -1]], 0)
check('xor', lambda: a ^ b, ['T', [2], 'torch.int64', [5, 4]], 0)
check('bool_and', lambda: torch.tensor([True, False]) & torch.tensor([True, True]), ['T', [2], 'torch.bool', [True, False]], 0)
check('bool_invert', lambda: ~torch.tensor([True, False]), ['T', [2], 'torch.bool', [False, True]], 0)
check('uint8_invert', lambda: ~torch.tensor([6, 0], dtype=torch.uint8), ['T', [2], 'torch.uint8', [249, 255]], 0)
check('wide', lambda: torch.tensor([1 << 40]) & torch.tensor([(1 << 40) | 5]), ['T', [1], 'torch.int64', [1099511627776]], 0)
check('rshift', lambda: torch.tensor([-8]) >> 1, ['T', [1], 'torch.int64', [-4]], 0)
check('lshift', lambda: torch.tensor([3]) << 2, ['T', [1], 'torch.int64', [12]], 0)
check('int32', lambda: torch.tensor([6, 5], dtype=torch.int32) & 3, ['T', [2], 'torch.int32', [2, 1]], 0)
check('rand', lambda: 3 & a, ['T', [2], 'torch.int64', [2, 1]], 0)
check('float_refused', lambda: err(lambda: torch.tensor([1.0]) & torch.tensor([1.0]), 'not implemented'), ['NotImplementedError', True], 0)
"#);
    assert_all_true(&out, 13);
}

/// A list of Python bools indexes (and assigns) as a mask, like a bool tensor.
#[test]
fn bool_lists_index_and_assign_as_masks() {
    let out = run(r#"
m = torch.arange(12).reshape(3, 4)
def assign():
    m2 = torch.zeros(3, 4); m2[[True, False, True]] = 1.0
    m3 = torch.arange(12.0).reshape(3, 4); m3[torch.tensor([False, True, False])] = -1
    return m2, m3
check('rows', lambda: m[[True, False, True]], ['T', [2, 4], 'torch.int64', [0, 1, 2, 3, 8, 9, 10, 11]], 0)
check('cols', lambda: m[:, [True, False, False, True]], ['T', [3, 2], 'torch.int64', [0, 3, 4, 7, 8, 11]], 0)
check('assign', lambda: assign(), [['T', [3, 4], 'torch.float32', [1.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0]], ['T', [3, 4], 'torch.float32', [0.0, 1.0, 2.0, 3.0, -1.0, -1.0, -1.0, -1.0, 8.0, 9.0, 10.0, 11.0]]], 0)
check('mask2d', lambda: m[m > 8], ['T', [3], 'torch.int64', [9, 10, 11]], 0)
check('scalar_mask', lambda: m[torch.tensor(True)].shape, [1, 3, 4], 0)
"#);
    assert_all_true(&out, 5);
}

/// relu(nan) is nan (its gradient passes, as threshold_backward), argmax/argmin/max(dim) answer the first NaN, and sort/topk/argsort treat NaN as the largest value.
#[test]
fn nan_propagates_through_relu_argmax_and_sort() {
    let out = run(r#"
x = torch.tensor([1.0, float("nan"), -1.0])
def relu_grad():
    n = torch.tensor([float("nan"), 2.0, -1.0], requires_grad=True)
    torch.relu(n).sum().backward()
    return n.grad
check('relu', lambda: torch.relu(x), ['T', [3], 'torch.float32', [1.0, float("nan"), 0.0]], 0)
check('relu_grad', lambda: relu_grad(), ['T', [3], 'torch.float32', [1.0, 1.0, 0.0]], 0)
check('argmax', lambda: x.argmax(), ['T', [], 'torch.int64', [1]], 0)
check('argmin', lambda: x.argmin(), ['T', [], 'torch.int64', [1]], 0)
check('max', lambda: x.max(), ['T', [], 'torch.float32', [float("nan")]], 0)
check('max_dim', lambda: torch.max(x, 0), [['T', [], 'torch.float32', [float("nan")]], ['T', [], 'torch.int64', [1]]], 0)
check('min_dim', lambda: torch.min(x, 0), [['T', [], 'torch.float32', [float("nan")]], ['T', [], 'torch.int64', [1]]], 0)
check('argmax_rows', lambda: torch.tensor([[1.0, float('nan')], [2.0, 0.0]]).argmax(1), ['T', [2], 'torch.int64', [1, 0]], 0)
check('sort', lambda: torch.sort(torch.tensor([3.0, 1.0, float('nan'), 2.5, -2.5, 0.5, -0.5, 1.5])), [['T', [8], 'torch.float32', [-2.5, -0.5, 0.5, 1.0, 1.5, 2.5, 3.0, float("nan")]], ['T', [8], 'torch.int64', [4, 6, 5, 1, 7, 3, 0, 2]]], 0)
check('sort_desc', lambda: torch.sort(torch.tensor([3.0, float('nan'), 1.0, float('nan'), 2.0]), descending=True), [['T', [5], 'torch.float32', [float("nan"), float("nan"), 3.0, 2.0, 1.0]], ['T', [5], 'torch.int64', [1, 3, 0, 4, 2]]], 0)
check('topk', lambda: torch.topk(torch.tensor([3.0, float('nan'), 1.0, 2.0]), 2), [['T', [2], 'torch.float32', [float("nan"), 3.0]], ['T', [2], 'torch.int64', [1, 0]]], 0)
check('argsort', lambda: torch.argsort(torch.tensor([2.0, float('nan'), 1.0])), ['T', [3], 'torch.int64', [2, 0, 1]], 0)
"#);
    assert_all_true(&out, 12);
}

/// An integer tensor raised to an integer power stays integer (negative exponents truncate: 2**-1 is 0), an int32 exponent keeps int32, and a negative Python-int exponent is refused.
#[test]
fn integer_powers_stay_integer() {
    let out = run(r#"
check('neg_exp', lambda: 2 ** torch.tensor([-1, -7, 0, 3]), ['T', [4], 'torch.int64', [0, 0, 1, 8]], 0)
check('bases', lambda: torch.tensor([1, -1, 0, 2, -2]) ** torch.tensor([-3, -3, -1, -1, -1]), ['T', [5], 'torch.int64', [1, -1, 0, 0, 0]], 0)
check('int32', lambda: torch.tensor([2], dtype=torch.int32) ** torch.tensor([3], dtype=torch.int32), ['T', [1], 'torch.int32', [8]], 0)
check('square', lambda: torch.tensor([2]) ** 2, ['T', [1], 'torch.int64', [4]], 0)
check('float_exp', lambda: torch.tensor([2]) ** 2.0, ['T', [1], 'torch.float32', [4.0]], 0)
check('tensor_float_exp', lambda: torch.tensor([2]) ** torch.tensor(0.5), ['T', [1], 'torch.float32', [1.4142135381698608]], 1e-06)
check('uint8', lambda: torch.tensor([3], dtype=torch.uint8) ** 2, ['T', [1], 'torch.uint8', [9]], 0)
check('float_base', lambda: 2.5 ** torch.tensor([2]), ['T', [1], 'torch.float32', [6.25]], 0)
check('neg_scalar', lambda: err(lambda: torch.tensor([2]) ** -1, 'negative integer powers'), ['RuntimeError', True], 0)
"#);
    assert_all_true(&out, 9);
}

/// PyTorch's result_type: a Python scalar or 0-d tensor only raises the result's category (float32 * 0-d float64 is float32, int32 + 1 is int32, where(cond, int32, 5) is int32); uint8 arithmetic wraps.
#[test]
fn scalars_and_0d_tensors_promote_by_category() {
    let out = run(r#"
x32 = torch.tensor([1.5, 2.0]); xi32 = torch.tensor([1, 2], dtype=torch.int32); xu8 = torch.tensor([250], dtype=torch.uint8)
xb = torch.tensor([True, False]); xi = torch.tensor([1, 2]); cond = torch.tensor([True, False])
check('p00', lambda: x32 * torch.tensor(2.0, dtype=torch.float64), ['T', [2], 'torch.float32', [3.0, 4.0]], 1e-06)
check('p01', lambda: xi32 + 1, ['T', [2], 'torch.int32', [2, 3]], 1e-06)
check('p02', lambda: xi32 + 1.5, ['T', [2], 'torch.float32', [2.5, 3.5]], 1e-06)
check('p03', lambda: xi32 * torch.tensor(2), ['T', [2], 'torch.int32', [2, 4]], 1e-06)
check('p04', lambda: xi32 * torch.tensor(2.0), ['T', [2], 'torch.float32', [2.0, 4.0]], 1e-06)
check('p05', lambda: xi + torch.tensor(1.5, dtype=torch.float64), ['T', [2], 'torch.float64', [2.5, 3.5]], 1e-06)
check('p06', lambda: xb + 1, ['T', [2], 'torch.int64', [2, 1]], 1e-06)
check('p07', lambda: xb + 1.5, ['T', [2], 'torch.float32', [2.5, 1.5]], 1e-06)
check('p08', lambda: xb + xb, ['T', [2], 'torch.bool', [True, False]], 1e-06)
check('p09', lambda: xb * torch.tensor(3), ['T', [2], 'torch.int64', [3, 0]], 1e-06)
check('p10', lambda: xu8 + 10, ['T', [1], 'torch.uint8', [4]], 1e-06)
check('p11', lambda: xu8 - 251, ['T', [1], 'torch.uint8', [255]], 1e-06)
check('p12', lambda: xu8 * 2, ['T', [1], 'torch.uint8', [244]], 1e-06)
check('p13', lambda: xu8 + xi32, ['T', [2], 'torch.int32', [251, 252]], 1e-06)
check('p14', lambda: xi32 + xi, ['T', [2], 'torch.int64', [2, 4]], 1e-06)
check('p15', lambda: xi / 2, ['T', [2], 'torch.float32', [0.5, 1.0]], 1e-06)
check('p16', lambda: xi32 / xi32, ['T', [2], 'torch.float32', [1.0, 1.0]], 1e-06)
check('p17', lambda: x32 + xi, ['T', [2], 'torch.float32', [2.5, 4.0]], 1e-06)
check('p18', lambda: x32.double() + x32, ['T', [2], 'torch.float64', [3.0, 4.0]], 1e-06)
check('p19', lambda: torch.tensor(1) + torch.tensor(2.5), ['T', [], 'torch.float32', [3.5]], 1e-06)
check('p20', lambda: torch.tensor(1, dtype=torch.int32) + torch.tensor(2), ['T', [], 'torch.int64', [3]], 1e-06)
check('p21', lambda: torch.where(cond, xi32, 5), ['T', [2], 'torch.int32', [1, 5]], 1e-06)
check('p22', lambda: torch.where(cond, xi32, 5.0), ['T', [2], 'torch.float32', [1.0, 5.0]], 1e-06)
check('p23', lambda: torch.where(cond, x32, torch.tensor(1.0, dtype=torch.float64)), ['T', [2], 'torch.float32', [1.5, 1.0]], 1e-06)
check('p24', lambda: torch.where(cond, 1, 0), ['T', [2], 'torch.int64', [1, 0]], 1e-06)
check('p25', lambda: torch.where(cond, 1.0, 0), ['T', [2], 'torch.float32', [1.0, 0.0]], 1e-06)
check('p26', lambda: xi32 == 2, ['T', [2], 'torch.bool', [False, True]], 1e-06)
check('p27', lambda: xi32 < 1.5, ['T', [2], 'torch.bool', [True, False]], 1e-06)
check('p28', lambda: x32 == 1.5, ['T', [2], 'torch.bool', [True, False]], 1e-06)
check('p29', lambda: xi32 // 2, ['T', [2], 'torch.int32', [0, 1]], 1e-06)
check('p30', lambda: xi32 % 2, ['T', [2], 'torch.int32', [1, 0]], 1e-06)
check('p31', lambda: xi // 2.0, ['T', [2], 'torch.float32', [0.0, 1.0]], 1e-06)
check('p32', lambda: xi.clamp(0.5, 1.5), ['T', [2], 'torch.float32', [1.0, 1.5]], 1e-06)
check('p33', lambda: torch.maximum(xi32, xi), ['T', [2], 'torch.int64', [1, 2]], 1e-06)
check('p34', lambda: torch.tensor([1, 2], dtype=torch.uint8) * torch.tensor([3.0]), ['T', [2], 'torch.float32', [3.0, 6.0]], 1e-06)
check('p35', lambda: xb.sum(), ['T', [], 'torch.int64', [1]], 1e-06)
check('p36', lambda: xi32.sum(), ['T', [], 'torch.int64', [3]], 1e-06)
check('p37', lambda: xu8.sum(), ['T', [], 'torch.int64', [250]], 1e-06)
check('p38', lambda: xi32.cumsum(0), ['T', [2], 'torch.int64', [1, 3]], 1e-06)
check('p39', lambda: torch.tensor([True, True]).cumsum(0), ['T', [2], 'torch.int64', [1, 2]], 1e-06)
check('p40', lambda: xi32.prod(), ['T', [], 'torch.int64', [2]], 1e-06)
check('p41', lambda: torch.tensor([0.1]) == 0.1, ['T', [1], 'torch.bool', [True]], 1e-06)
"#);
    assert_all_true(&out, 42);
}

/// set_grad_enabled works as a function, a context manager and a decorator; inference_mode and bare @torch.no_grad work; leaving a block restores the mode.
#[test]
fn grad_mode_contexts_and_decorators() {
    let out = run(r#"
x = torch.tensor([1.0, 2.0], requires_grad=True)
def modes():
    out = []
    with torch.set_grad_enabled(False):
        out.append((x * 2).requires_grad)
    out.append(torch.is_grad_enabled())
    torch.set_grad_enabled(False); out.append((x * 2).requires_grad); torch.set_grad_enabled(True)
    out.append((x * 2).requires_grad)
    @torch.no_grad
    def f(a):
        return a * 2
    @torch.inference_mode()
    def h(a):
        return a * 2
    @torch.inference_mode
    def h2(a):
        return a * 2
    out += [f(x).requires_grad, f.__name__, h(x).requires_grad, h2(x).requires_grad]
    with torch.inference_mode():
        out.append((x * 2).requires_grad)
    with torch.inference_mode(False):
        out.append((x * 2).requires_grad)
    @torch.set_grad_enabled(False)
    def s(a):
        return a * 2
    out.append(torch.is_grad_enabled())
    out.append(s(x).requires_grad)
    with torch.no_grad():
        with torch.enable_grad():
            out.append((x * 2).requires_grad)
    out.append(torch.is_grad_enabled())
    return out
check('modes', lambda: modes(), [False, True, False, True, False, 'f', False, False, False, True, True, False, True, True], 0)
"#);
    assert_all_true(&out, 1);
}

/// Gradients of 1-d @ 1-d (a 0-d result), matrix @ vector, vector @ matrix and batched @ vector.
#[test]
fn vector_matmul_gradients() {
    let out = run(r#"
def grads():
    a = torch.tensor([1.0, 2.0], requires_grad=True); b = torch.tensor([3.0, 4.0], requires_grad=True)
    (a @ b).backward()
    m = torch.tensor([[1.0, 2.0], [3.0, 4.0]], requires_grad=True); v = torch.tensor([1.0, -1.0], requires_grad=True)
    (m @ v).sum().backward()
    v2 = torch.tensor([2.0, 1.0], requires_grad=True); (v2 @ m).sum().backward()
    bt = torch.ones(3, 2, 2, requires_grad=True); (bt @ v).sum().backward()
    return a.grad, b.grad, m.grad, v.grad, v2.grad, bt.grad
check('grads', lambda: grads(), [['T', [2], 'torch.float32', [3.0, 4.0]], ['T', [2], 'torch.float32', [1.0, 2.0]], ['T', [2, 2], 'torch.float32', [3.0, 1.0, 2.0, 0.0]], ['T', [2], 'torch.float32', [10.0, 12.0]], ['T', [2], 'torch.float32', [3.0, 7.0]], ['T', [3, 2, 2], 'torch.float32', [1.0, -1.0, 1.0, -1.0, 1.0, -1.0, 1.0, -1.0, 1.0, -1.0, 1.0, -1.0]]], 1e-06)
"#);
    assert_all_true(&out, 1);
}

/// f'{loss:.4f}' formats a 0-d tensor's value; other tensors take only an empty spec, as PyTorch.
#[test]
fn zero_dim_tensors_take_format_specs() {
    let out = run(r#"
check('f3', lambda: f'{torch.tensor(1.23456):.3f}', '1.235', 0)
check('d', lambda: f'{torch.tensor(3):d}', '3', 0)
check('width', lambda: f'{torch.tensor(2.5):>8.2f}', '    2.50', 0)
check('plain', lambda: f'{torch.tensor([1.5])}', 'tensor([1.5000])', 0)
check('format_call', lambda: '{}'.format(torch.tensor(7)), '7', 0)
check('vector_spec', lambda: err(lambda: f'{torch.tensor([1.5]):.2f}'), ['TypeError', True], 0)
"#);
    assert_all_true(&out, 6);
}

/// An assigned value broadcasts after dropping leading 1-dims (t[0] = ones(1, 4) into a (2, 4) tensor).
#[test]
fn setitem_values_drop_leading_ones() {
    let out = run(r#"
def put():
    t = torch.zeros(2, 4); t[0] = torch.ones(1, 4); t[1, :] = torch.full((1, 1, 4), 2.0)
    u = torch.zeros(3, 2); u[:, 0] = torch.tensor([[1.0, 2.0, 3.0]])
    return t, u
check('put', lambda: put(), [['T', [2, 4], 'torch.float32', [1.0, 1.0, 1.0, 1.0, 2.0, 2.0, 2.0, 2.0]], ['T', [3, 2], 'torch.float32', [1.0, 0.0, 2.0, 0.0, 3.0, 0.0]]], 0)
"#);
    assert_all_true(&out, 1);
}

/// sort, topk (and kthvalue, median, mode, cummax, aminmax) results carry .values/.indices (or min/max) fields.
#[test]
fn sort_and_topk_return_named_values_and_indices() {
    let out = run(r#"
check('sort', lambda: (lambda s: (s.values, s.indices))(torch.sort(torch.tensor([3.0, 1.0, 2.0]))), [['T', [3], 'torch.float32', [1.0, 2.0, 3.0]], ['T', [3], 'torch.int64', [1, 2, 0]]], 0)
check('topk', lambda: (lambda s: (s.values, s.indices))(torch.tensor([3.0, 1.0, 2.0]).topk(2)), [['T', [2], 'torch.float32', [3.0, 2.0]], ['T', [2], 'torch.int64', [0, 2]]], 0)
check('method_sort', lambda: torch.tensor([[3, 1], [0, 5]]).sort(1, descending=True).indices, ['T', [2, 2], 'torch.int64', [0, 1, 1, 0]], 0)
check('smallest', lambda: torch.topk(torch.tensor([3.0, 1.0, 2.0]), 2, largest=False).values, ['T', [2], 'torch.float32', [1.0, 2.0]], 0)
check('aminmax', lambda: (lambda s: (s.min, s.max))(torch.aminmax(torch.tensor([[1.0, 5.0], [3.0, 0.0]]), dim=0)), [['T', [2], 'torch.float32', [1.0, 0.0]], ['T', [2], 'torch.float32', [3.0, 5.0]]], 0)
check('unpack', lambda: (lambda v_i: v_i[1])(torch.sort(torch.tensor([2, 1]))), ['T', [2], 'torch.int64', [1, 0]], 0)
"#);
    assert_all_true(&out, 6);
}

/// einsum with a repeated index or '...', cumsum/chunk of an empty dim, dim=0 reductions of a 0-d tensor, and diag of a vector holding inf.
#[test]
fn einsum_empty_dims_and_zero_dim_reductions() {
    let out = run(r#"
check('einsum_trace', lambda: torch.einsum('ii->', torch.tensor([[1.0, 2.0], [3.0, 4.0]])), ['T', [], 'torch.float32', [5.0]], 1e-06)
check('einsum_diag', lambda: torch.einsum('ii->i', torch.tensor([[1.0, 2.0], [3.0, 4.0]])), ['T', [2], 'torch.float32', [1.0, 4.0]], 0)
check('einsum_ellipsis', lambda: torch.einsum('...ij,...jk->...ik', torch.ones(2, 2, 3), torch.ones(2, 3, 2)), ['T', [2, 2, 2], 'torch.float32', [3.0, 3.0, 3.0, 3.0, 3.0, 3.0, 3.0, 3.0]], 0)
check('einsum_implicit', lambda: torch.einsum('bij,bjk', torch.ones(2, 2, 3), torch.ones(2, 3, 2)), ['T', [2, 2], 'torch.float32', [6.0, 6.0, 6.0, 6.0]], 0)
check('einsum_ellipsis_sum', lambda: torch.einsum('...i->...', torch.ones(2, 3)), ['T', [2], 'torch.float32', [3.0, 3.0]], 0)
check('cumsum_empty', lambda: torch.cumsum(torch.zeros(0, 3), 0), ['T', [0, 3], 'torch.float32', []], 0)
check('chunk_empty', lambda: [c.shape for c in torch.chunk(torch.zeros(0, 3), 2)], [[0, 3], [0, 3]], 0)
check('sum0d', lambda: torch.tensor(5.0).sum(0), ['T', [], 'torch.float32', [5.0]], 0)
check('max0d', lambda: tuple(torch.tensor(5.0).max(0)), [['T', [], 'torch.float32', [5.0]], ['T', [], 'torch.int64', [0]]], 0)
check('argmax0d', lambda: torch.tensor(5).argmax(0), ['T', [], 'torch.int64', [0]], 0)
check('mean0d', lambda: torch.tensor(5.0).mean(-1), ['T', [], 'torch.float32', [5.0]], 0)
check('diag_inf', lambda: torch.diag(torch.tensor([1.0, float('inf')])), ['T', [2, 2], 'torch.float32', [1.0, 0.0, 0.0, float("inf")]], 0)
check('diag_offset', lambda: torch.diag(torch.tensor([1.0, 2.0]), 1), ['T', [3, 3], 'torch.float32', [0.0, 1.0, 0.0, 0.0, 0.0, 2.0, 0.0, 0.0, 0.0]], 0)
check('diag_of_matrix', lambda: torch.diag(torch.tensor([[1.0, 2.0], [3.0, 4.0]]), -1), ['T', [1], 'torch.float32', [3.0]], 0)
"#);
    assert_all_true(&out, 14);
}

/// repr/str follow torch/_tensor_str.py: one layout per tensor, scientific notation for tiny or wide-ranging values, column padding, '...' summaries beyond 1000 elements, set_printoptions; str(device) is 'cpu'.
#[test]
fn printing_follows_pytorch_formatting_and_summarises_large_tensors() {
    let out = run(r#"
def opts():
    torch.set_printoptions(precision=2)
    a = str(torch.tensor([1.23456, 2.5]))
    torch.set_printoptions(precision=4, sci_mode=False)
    b = str(torch.tensor([1e-6, 1.0]))
    torch.set_printoptions(sci_mode=None, threshold=5, edgeitems=2)
    c = str(torch.arange(10))
    torch.set_printoptions(profile="default")
    return a, b, c, str(torch.arange(10))
check('r00', lambda: repr(torch.tensor([1.0, 2.0, 3.0])), 'tensor([1., 2., 3.])', 0)
check('r01', lambda: repr(torch.tensor([0.5, 1.25, -3.0])), 'tensor([ 0.5000,  1.2500, -3.0000])', 0)
check('r02', lambda: repr(torch.tensor([1e-5, 1.0])), 'tensor([1.0000e-05, 1.0000e+00])', 0)
check('r03', lambda: repr(torch.tensor([1e9, 1.0])), 'tensor([1.0000e+09, 1.0000e+00])', 0)
check('r04', lambda: repr(torch.tensor([1.0, 2000.0])), 'tensor([1.0000e+00, 2.0000e+03])', 0)
check('r05', lambda: repr(torch.tensor([0.0001, 5.0])), 'tensor([1.0000e-04, 5.0000e+00])', 0)
check('r06', lambda: repr(torch.tensor([[1.0, 22.5], [-3.0, 4.0]])), 'tensor([[ 1.0000, 22.5000],\n        [-3.0000,  4.0000]])', 0)
check('r07', lambda: repr(torch.tensor([[7.0, 11.0], [9.0, 13.0]])), 'tensor([[ 7., 11.],\n        [ 9., 13.]])', 0)
check('r08', lambda: repr(torch.tensor([float('nan'), 1.0, float('inf'), -float('inf')])), 'tensor([nan, 1., inf, -inf])', 0)
check('r09', lambda: repr(torch.tensor(3.5)), 'tensor(3.5000)', 0)
check('r10', lambda: repr(torch.tensor(1e-8)), 'tensor(1.0000e-08)', 0)
check('r11', lambda: repr(torch.tensor([1, -20, 300])), 'tensor([  1, -20, 300])', 0)
check('r12', lambda: repr(torch.tensor([True, False])), 'tensor([ True, False])', 0)
check('r13', lambda: repr(torch.tensor([1.5, 2.5], dtype=torch.float64)), 'tensor([1.5000, 2.5000], dtype=torch.float64)', 0)
check('r14', lambda: repr(torch.tensor([1, 2], dtype=torch.int32)), 'tensor([1, 2], dtype=torch.int32)', 0)
check('r15', lambda: repr(torch.zeros(0)), 'tensor([])', 0)
check('r16', lambda: repr(torch.zeros(2, 0, dtype=torch.int64)), 'tensor([], size=(2, 0), dtype=torch.int64)', 0)
check('r17', lambda: repr(torch.arange(2000)), 'tensor([   0,    1,    2,  ..., 1997, 1998, 1999])', 0)
check('r18', lambda: repr(torch.arange(2000.0) / 7), 'tensor([0.0000e+00, 1.4286e-01, 2.8571e-01,  ..., 2.8529e+02, 2.8543e+02,\n        2.8557e+02])', 0)
check('r19', lambda: repr(torch.arange(1200).reshape(40, 30)), 'tensor([[   0,    1,    2,  ...,   27,   28,   29],\n        [  30,   31,   32,  ...,   57,   58,   59],\n        [  60,   61,   62,  ...,   87,   88,   89],\n        ...,\n        [1110, 1111, 1112,  ..., 1137, 1138, 1139],\n        [1140, 1141, 1142,  ..., 1167, 1168, 1169],\n        [1170, 1171, 1172,  ..., 1197, 1198, 1199]])', 0)
check('r20', lambda: repr(torch.arange(24.0).reshape(2, 3, 4)), 'tensor([[[ 0.,  1.,  2.,  3.],\n         [ 4.,  5.,  6.,  7.],\n         [ 8.,  9., 10., 11.]],\n\n        [[12., 13., 14., 15.],\n         [16., 17., 18., 19.],\n         [20., 21., 22., 23.]]])', 0)
check('r21', lambda: repr(torch.arange(30.0)), 'tensor([ 0.,  1.,  2.,  3.,  4.,  5.,  6.,  7.,  8.,  9., 10., 11., 12., 13.,\n        14., 15., 16., 17., 18., 19., 20., 21., 22., 23., 24., 25., 26., 27.,\n        28., 29.])', 0)
check('r22', lambda: repr(torch.tensor([1.0, 2.0], requires_grad=True) * 2), 'tensor([2., 4.], grad_fn=<MulBackward0>)', 0)
check('r23', lambda: repr(torch.arange(3000.0).reshape(3, 1000)), 'tensor([[0.0000e+00, 1.0000e+00, 2.0000e+00,  ..., 9.9700e+02, 9.9800e+02,\n         9.9900e+02],\n        [1.0000e+03, 1.0010e+03, 1.0020e+03,  ..., 1.9970e+03, 1.9980e+03,\n         1.9990e+03],\n        [2.0000e+03, 2.0010e+03, 2.0020e+03,  ..., 2.9970e+03, 2.9980e+03,\n         2.9990e+03]])', 0)
check('r24', lambda: repr(torch.tensor([[1e-10, 1.0], [1.0, 2.0]])), 'tensor([[1.0000e-10, 1.0000e+00],\n        [1.0000e+00, 2.0000e+00]])', 0)
check('options', lambda: opts(), ['tensor([1.23, 2.50])', 'tensor([0.0000, 1.0000])', 'tensor([0, 1,  ..., 8, 9])', 'tensor([0, 1, 2, 3, 4, 5, 6, 7, 8, 9])'], 0)
check('device', lambda: (str(torch.device('cpu')), repr(torch.device('cpu')), torch.device('cpu') == 'cpu'), ['cpu', "device(type='cpu')", False], 0)
"#);
    assert_all_true(&out, 27);
}

/// torch.add/sub scale only when alpha != 1: a compiled training step using torch.add(graph, parameter) sends the gradient to the parameter, not to an eager copy (values from PyTorch's eager step).
#[test]
fn add_and_sub_take_alpha() {
    let out = run(r#"
from torch import nn
a = torch.tensor([1.0, 2.0]); b = torch.tensor([3.0, 4.0])
def captured(kind):
    W = nn.Parameter(torch.tensor([[0.5, -0.2], [0.1, 0.3]]))
    bias = nn.Parameter(torch.tensor([0.1, -0.1]))
    opt = torch.optim.SGD([W, bias], lr=0.1)
    x = torch.tensor([[1.0, 2.0], [3.0, -1.0], [0.5, 0.5]])
    def step(x):
        opt.zero_grad()
        h = x @ W.T
        h = torch.add(h, bias) if kind == "add" else torch.sub(h, bias)
        loss = ((h + bias) ** 2).mean()
        loss.backward()
        opt.step()
        return loss
    if "zipp" in torch.__version__:
        got = []
        torch.compile(step, training=True)(x).submit(got.append)
        loss = got[0]
    else:
        loss = step(x)
    return loss, bias.grad, bias, W
check('compiled_add', lambda: captured('add'), [['T', [], 'torch.float32', [0.6854166984558105]], ['T', [2], 'torch.float32', [1.7000000476837158, 0.20000000298023224]], ['T', [2], 'torch.float32', [-0.07000000029802322, -0.12000000476837158]], ['T', [2, 2], 'torch.float32', [0.2941666543483734, -0.16249999403953552, 0.10333333909511566, 0.2600000202655792]]], 1e-05)
check('compiled_sub', lambda: captured('sub'), [['T', [], 'torch.float32', [0.5754166841506958]], ['T', [2], 'torch.float32', [0.0, 0.0]], ['T', [2], 'torch.float32', [0.10000000149011612, -0.10000000149011612]], ['T', [2, 2], 'torch.float32', [0.3241666555404663, -0.1525000035762787, 0.07333333045244217, 0.25]]], 1e-05)
check('plain', lambda: torch.add(a, b), ['T', [2], 'torch.float32', [4.0, 6.0]], 0)
check('alpha2', lambda: torch.add(a, b, alpha=2), ['T', [2], 'torch.float32', [7.0, 10.0]], 0)
check('sub_half', lambda: torch.sub(a, b, alpha=0.5), ['T', [2], 'torch.float32', [-0.5, 0.0]], 0)
check('scalar_alpha', lambda: a.add(1, alpha=3), ['T', [2], 'torch.float32', [4.0, 5.0]], 0)
check('rsub', lambda: torch.rsub(a, 5), ['T', [2], 'torch.float32', [4.0, 3.0]], 0)
check('inplace_alpha', lambda: (lambda t: (t.add_(b, alpha=-0.5), t)[1])(a.clone()), ['T', [2], 'torch.float32', [-0.5, 0.0]], 0)
"#);
    assert_all_true(&out, 8);
}

/// torch.sqrt/rsqrt/abs/square/div/pow/reshape/flatten/unsqueeze/squeeze/permute/transpose/t and F.silu called on a compiled step's graph tensors record graph operations: the compiled training step equals PyTorch's eager step.
#[test]
fn torch_functions_dispatch_graph_tensors() {
    let out = run(r#"
from torch import nn
import torch.nn.functional as F
def run(body):
    model = nn.Linear(3, 2)
    with torch.no_grad():
        model.weight.copy_(torch.tensor([[0.5, -0.2, 0.1], [0.3, 0.4, -0.6]]))
        model.bias.fill_(0.1)
    optimizer = torch.optim.SGD(model.parameters(), lr=0.1)
    x = torch.tensor([[1.0, 2.0, 0.5], [3.0, -1.0, 0.2]])
    y = torch.tensor([0, 1])
    def step(x, y):
        optimizer.zero_grad()
        loss = body(model, x, y)
        loss.backward()
        optimizer.step()
        return loss
    if "zipp" in torch.__version__:
        got = []
        torch.compile(step, training=True)(x, y).submit(got.append)
        loss = got[0]
    else:
        loss = step(x, y)
    return loss, model.weight, model.bias
check('sqrt', lambda: run(lambda m, x, y: F.cross_entropy(torch.sqrt(m(x).square() + 1.0), y)), [['T', [], 'torch.float32', [1.0761463642120361]], ['T', [2, 3], 'torch.float32', [0.41175124049186707, -0.15422864258289337, 0.09715412557125092, 0.3277003765106201, 0.3456557095050812, -0.6065310835838318]], ['T', [2], 'torch.float32', [0.07525663822889328, 0.09634463489055634]]], 1e-05)
check('rsqrt_abs', lambda: run(lambda m, x, y: F.cross_entropy(torch.rsqrt(torch.abs(m(x)) + 1.0), y)), [['T', [], 'torch.float32', [0.5992650985717773]], ['T', [2, 3], 'torch.float32', [0.4988352656364441, -0.21872290968894958, 0.09637314081192017, 0.2858918011188507, 0.41490089893341064, -0.5990466475486755]], ['T', [2], 'torch.float32', [0.09415142983198166, 0.09821102023124695]]], 1e-05)
check('div_pow', lambda: run(lambda m, x, y: F.cross_entropy(torch.div(torch.pow(m(x), 2), 2.0), y)), [['T', [], 'torch.float32', [1.3162204027175903]], ['T', [2, 3], 'torch.float32', [0.28255900740623474, -0.11024236679077148, 0.08871258050203323, 0.3326435089111328, 0.3269205391407013, -0.6093748807907104]], ['T', [2], 'torch.float32', [0.03245604783296585, 0.09311022609472275]]], 1e-05)
check('shapes', lambda: run(lambda m, x, y: F.cross_entropy(torch.t(torch.transpose(torch.reshape(torch.flatten(torch.unsqueeze(m(x), 0)), (2, 2)), 0, 1)), y)), [['T', [], 'torch.float32', [1.3213154077529907]], ['T', [2, 3], 'torch.float32', [0.4139770269393921, -0.09467446058988571, 0.10850036144256592, 0.38602298498153687, 0.29467445611953735, -0.6085003614425659]], ['T', [2], 'torch.float32', [0.09322602301836014, 0.1067739725112915]]], 1e-05)
check('permute_squeeze', lambda: run(lambda m, x, y: F.cross_entropy(torch.squeeze(torch.permute(m(x).unsqueeze(0), (0, 1, 2)), 0), y)), [['T', [], 'torch.float32', [1.3213154077529907]], ['T', [2, 3], 'torch.float32', [0.4139770269393921, -0.09467446058988571, 0.10850036144256592, 0.38602298498153687, 0.29467445611953735, -0.6085003614425659]], ['T', [2], 'torch.float32', [0.09322602301836014, 0.1067739725112915]]], 1e-05)
check('silu', lambda: run(lambda m, x, y: F.cross_entropy(F.silu(m(x)), y)), [['T', [], 'torch.float32', [1.2454661130905151]], ['T', [2, 3], 'torch.float32', [0.39305710792541504, -0.11907017230987549, 0.10128002613782883, 0.35773810744285583, 0.3157104253768921, -0.6082303524017334]], ['T', [2], 'torch.float32', [0.0772901400923729, 0.10066216439008713]]], 1e-05)
"#);
    assert_all_true(&out, 6);
}

/// Slice bounds may be 0-d integer tensors (`data[i:i + block]` with `i` from torch.randint), in reads and writes.
#[test]
fn zero_dim_tensors_bound_slices() {
    let out = run(r#"
d = torch.arange(20)
def write():
    e = torch.zeros(10); e[torch.tensor(1):torch.tensor(3)] = 1.0
    return e
check('loop', lambda: [d[i:i + 4] for i in torch.tensor([3, 7])], [['T', [4], 'torch.int64', [3, 4, 5, 6]], ['T', [4], 'torch.int64', [7, 8, 9, 10]]], 0)
check('step', lambda: d[torch.tensor(2):torch.tensor(10):torch.tensor(3)], ['T', [3], 'torch.int64', [2, 5, 8]], 0)
check('write', lambda: write(), ['T', [10], 'torch.float32', [0.0, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]], 0)
"#);
    assert_all_true(&out, 3);
}

/// torch.load gathers a checkpoint tensor through its storage offset and strides (a transpose, a column, a stepped slice, an expand with stride 0, an offset block, a permute) and resolves PyTorch's weights-only globals (Size, device, Parameter), all saved by PyTorch 2.11; Parameters, dtypes and devices saved here load back as themselves.
#[test]
fn pytorch_checkpoints_with_strided_tensors_load() {
    let out = run(r#"
from torch import nn
def described(d):
    return [[k, type(v).__name__, v if isinstance(v, torch.Tensor) else str(v), getattr(v, "requires_grad", None)] for k, v in sorted(d.items())]
def roundtrip():
    torch.save({"p": nn.Parameter(torch.tensor([1.5, -2.0])), "q": nn.Parameter(torch.ones(2), requires_grad=False), "dt": torch.float64, "b": torch.bool, "dev": torch.device("cpu"), "t": torch.arange(3)}, "roundtrip.pt")
    return described(torch.load("roundtrip.pt"))
check('noncontig', lambda: torch.load('noncontig.pt'), [['base', ['T', [3, 4], 'torch.float32', [0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0]]], ['col', ['T', [3], 'torch.float32', [1.0, 5.0, 9.0]]], ['expand', ['T', [3, 4], 'torch.float64', [0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0, 2.0, 2.0, 2.0, 2.0]]], ['int_t', ['T', [4, 2], 'torch.int64', [1, 11, 2, 12, 3, 13, 4, 14]]], ['offset', ['T', [2, 2], 'torch.float32', [6.0, 7.0, 10.0, 11.0]]], ['perm', ['T', [4, 2, 3], 'torch.float32', [0.0, 4.0, 8.0, 12.0, 16.0, 20.0, 1.0, 5.0, 9.0, 13.0, 17.0, 21.0, 2.0, 6.0, 10.0, 14.0, 18.0, 22.0, 3.0, 7.0, 11.0, 15.0, 19.0, 23.0]]], ['scalar_view', ['T', [], 'torch.float32', [11.0]]], ['step', ['T', [2, 4], 'torch.float32', [0.0, 1.0, 2.0, 3.0, 8.0, 9.0, 10.0, 11.0]]], ['t', ['T', [4, 3], 'torch.float32', [0.0, 4.0, 8.0, 1.0, 5.0, 9.0, 2.0, 6.0, 10.0, 3.0, 7.0, 11.0]]]], 0)
check('torch_globals', lambda: described(torch.load('torch_globals.pt')), [['dev', 'device', 'cpu', None], ['param', 'Parameter', ['T', [2], 'torch.float32', [1.5, -2.0]], True], ['shape', 'Size', 'torch.Size([2, 3])', None]], 0)
check('roundtrip', lambda: roundtrip(), [['b', 'dtype', 'torch.bool', None], ['dev', 'device', 'cpu', None], ['dt', 'dtype', 'torch.float64', None], ['p', 'Parameter', ['T', [2], 'torch.float32', [1.5, -2.0]], True], ['q', 'Parameter', ['T', [2], 'torch.float32', [1.0, 1.0]], False], ['t', 'Tensor', ['T', [3], 'torch.int64', [0, 1, 2]], False]], 0)
"#);
    assert_all_true(&out, 3);
}

/// Generator.get_state/set_state and torch.get/set_rng_state capture the stream position (not only the seed); torch.seed reseeds; randperm (forward Fisher-Yates), randint (one word below a 2**28 range, random64 above) and random64 follow PyTorch's MT19937 stream.
#[test]
fn generator_state_captures_the_stream_position() {
    let out = run(r#"
def state():
    g = torch.Generator().manual_seed(7)
    torch.rand(3, generator=g)
    st = g.get_state()
    a = torch.rand(4, generator=g)
    g.set_state(st)
    b = torch.rand(4, generator=g)
    torch.manual_seed(3)
    torch.randn(5)
    s2 = torch.get_rng_state()
    c = torch.randn(3)
    torch.set_rng_state(s2)
    d = torch.randn(3)
    s = torch.seed()
    h = torch.Generator().manual_seed(1)
    cl = torch.Generator(); cl.set_state(h.get_state())
    return torch.equal(a, b), torch.equal(c, d), torch.initial_seed() == s, isinstance(s, int), st.dtype, torch.equal(torch.rand(2, generator=h), torch.rand(2, generator=cl)), g.initial_seed()
def ranges():
    out = []
    for r in (10, 1000, 2 ** 28 - 1, 2 ** 28, 2 ** 31, 3 * 2 ** 30, 2 ** 32 + 1, 2 ** 40):
        out.append(torch.randint(0, r, (3,), generator=torch.Generator().manual_seed(5)))
    out.append(torch.randint(-2 ** 40, 2 ** 40, (2,), generator=torch.Generator().manual_seed(5)))
    return out
def seed64():
    g = torch.Generator().manual_seed(9)
    if "zipp" in torch.__version__:
        return [torch._random64(g) % (1 << 63) for _ in range(2)]
    return [torch.empty((), dtype=torch.int64).random_(generator=g).item() for _ in range(2)]
check('state', lambda: state(), [True, True, True, True, 'torch.uint8', True, 7], 0)
check('randint_ranges', lambda: ranges(), [['T', [3], 'torch.int64', [1, 4, 7]], ['T', [3], 'torch.int64', [411, 814, 767]], ['T', [3], 'torch.int64', [148147046, 236996814, 250105852]], ['T', [3], 'torch.int64', [236996814, 80864957, 220060790]], ['T', [3], 'torch.int64', [236996814, 1423042237, 1562238070]], ['T', [3], 'torch.int64', [2384480462, 1423042237, 488496246]], ['T', [3], 'torch.int64', [3578510700, 4125726415, 674386064]], ['T', [3], 'torch.int64', [425438759118, 1030067709629, 989404716150]], ['T', [2], 'torch.int64', [425438759118, 1030067709629]]], 0)
check('random64', lambda: seed64(), [191369442034012508, 34580184003183093], 0)
check('randperm_stream', lambda: (lambda g: (torch.randperm(10, generator=g), torch.randint(0, 10, (5,), generator=g), torch.randperm(7, generator=g), torch.randint(0, 1000, (3,), generator=g)))(torch.Generator().manual_seed(0)), [['T', [10], 'torch.int64', [4, 1, 7, 5, 3, 9, 0, 8, 6, 2]], ['T', [5], 'torch.int64', [3, 1, 6, 6, 9]], ['T', [7], 'torch.int64', [2, 3, 1, 0, 6, 4, 5]], ['T', [3], 'torch.int64', [126, 119, 391]]], 0)
"#);
    assert_all_true(&out, 4);
}

/// torch.autograd is the module (Function, grad, backward); Tensor.grad_fn names the recorded op; register_hook sees and may replace a gradient; Function supports several outputs, needs_input_grad and version checks on saved tensors.
#[test]
fn autograd_module_grad_fn_and_hooks() {
    let out = run(r#"
import torch.autograd as ag
from torch.autograd import Function
class Mul(Function):
    @staticmethod
    def forward(ctx, x, w):
        ctx.save_for_backward(x, w)
        return x * w
    @staticmethod
    def backward(ctx, g):
        x, w = ctx.saved_tensors
        return (g * w if ctx.needs_input_grad[0] else None, g * x if ctx.needs_input_grad[1] else None)
class Split(Function):
    @staticmethod
    def forward(ctx, x):
        return x * 2, x * 3
    @staticmethod
    def backward(ctx, g1, g2):
        return g1 * 2 + g2 * 3
def module():
    w = torch.full((2,), 3.0, requires_grad=True)
    Mul.apply(torch.ones(2), w).sum().backward()
    x = torch.ones(2, requires_grad=True)
    p, q = Split.apply(x)
    (p + q).sum().backward()
    y = Mul.apply(torch.ones(2, requires_grad=True), torch.ones(2))
    return hasattr(torch.autograd, "Function"), torch.autograd is ag, w.grad, x.grad, type(y.grad_fn).__name__, callable(torch.autograd.grad), callable(torch.autograd.backward)
def names():
    x = torch.ones(2, requires_grad=True)
    y = x * 2
    nf = y.grad_fn.next_functions
    return x.grad_fn, y.grad_fn.name(), [type(t.grad_fn).__name__ for t in (y, x.sum(), x.view(2), x.exp(), torch.cat([x, x]), x.double(), x ** 2, x.relu(), x + 1, x - 1, x / 2, x.clone(), x.mean(), x.tanh(), x.sigmoid(), x.abs(), x.sqrt(), x.norm())], type(nf[0][0]).__name__, nf[1][0], repr(y)
def hooks():
    x = torch.tensor([1.0, 2.0], requires_grad=True)
    seen = []
    h = x.register_hook(lambda g: seen.append(g.tolist()))
    y = x * 3
    y.register_hook(lambda g: g * 10)
    y.sum().backward()
    h.remove()
    (x * 1).sum().backward()
    return seen, x.grad, err(lambda: torch.ones(1).register_hook(lambda g: g))
def saved_version():
    class Sq(Function):
        @staticmethod
        def forward(ctx, x):
            ctx.save_for_backward(x)
            return x * x
        @staticmethod
        def backward(ctx, g):
            (x,) = ctx.saved_tensors
            return 2 * x * g
    a = torch.tensor([1.0, 2.0], requires_grad=True)
    y = a * 1
    z = Sq.apply(y)
    y.add_(1)
    return err(lambda: z.sum().backward(), "modified by an inplace operation")
def grad_api():
    x = torch.tensor([1.0, 2.0], requires_grad=True); y = torch.tensor([3.0], requires_grad=True)
    out = [torch.autograd.grad((x * 2).sum(), [x, y], allow_unused=True)]
    out.append(torch.autograd.grad((x * 2).sum(), [x, y], materialize_grads=True))
    out.append(torch.autograd.grad([(x * 2).sum(), (x * x).sum()], x))
    out.append(torch.autograd.grad(x * 3, x, grad_outputs=torch.tensor([1.0, 2.0])))
    out.append(err(lambda: torch.autograd.grad((x * 2).sum(), [x, y]), "appears to not have been used"))
    return out
check('module', lambda: module(), [True, True, ['T', [2], 'torch.float32', [1.0, 1.0]], ['T', [2], 'torch.float32', [5.0, 5.0]], 'MulBackward', True, True], 1e-06)
check('names', lambda: names(), [None, 'MulBackward0', ['MulBackward0', 'SumBackward0', 'ViewBackward0', 'ExpBackward0', 'CatBackward0', 'ToCopyBackward0', 'PowBackward0', 'ReluBackward0', 'AddBackward0', 'SubBackward0', 'DivBackward0', 'CloneBackward0', 'MeanBackward0', 'TanhBackward0', 'SigmoidBackward0', 'AbsBackward0', 'SqrtBackward0', 'LinalgVectorNormBackward0'], 'AccumulateGrad', None, 'tensor([2., 2.], grad_fn=<MulBackward0>)'], 0)
check('hooks', lambda: hooks(), [[[30.0, 30.0]], ['T', [2], 'torch.float32', [31.0, 31.0]], ['RuntimeError', True]], 0)
check('saved_version', lambda: saved_version(), ['RuntimeError', True], 0)
check('grad_api', lambda: grad_api(), [[['T', [2], 'torch.float32', [2.0, 2.0]], None], [['T', [2], 'torch.float32', [2.0, 2.0]], ['T', [1], 'torch.float32', [0.0]]], [['T', [2], 'torch.float32', [4.0, 6.0]]], [['T', [2], 'torch.float32', [3.0, 6.0]]], ['RuntimeError', True]], 1e-06)
"#);
    assert_all_true(&out, 5);
}

/// Tensor methods (add/sub/mul/div with alpha and rounding_mode, matmul family, flips, movedim, unflatten, type_as, masked_fill, new_*, tensor clamp bounds), the in-place family and the legacy/creation APIs.
#[test]
fn tensor_methods_and_constructors() {
    let out = run(r#"
A = [[1.0, -2.0, 3.0], [4.0, 5.0, -6.0]]
a = torch.tensor(A); b = torch.tensor([[0.5, 1.5, -1.0], [2.0, -0.5, 1.0]])
v = torch.tensor([3.0, 1.0, 2.0]); w = torch.tensor([1.0, 0.0, -1.0])
m = torch.tensor([[1.0, 2.0], [3.0, 4.0], [5.0, 6.0]])
def grad_of(f, *vals):
    xs = [torch.tensor(val, requires_grad=True) for val in vals]
    out = f(*xs)
    out = out if out.dim() == 0 else (out * torch.arange(1, out.numel() + 1, dtype=out.dtype).reshape(out.shape)).sum()
    out.backward()
    return [x.grad for x in xs]
def inplace(name, *args, **kwargs):
    t = torch.tensor([1.5, 2.25, 4.0])
    getattr(t, name)(*args, **kwargs)
    return t
check('t00', lambda: a.add(b), ['T', [2, 3], 'torch.float32', [1.5, -0.5, 2.0, 6.0, 4.5, -5.0]], 1e-05)
check('t01', lambda: a.add(b, alpha=2), ['T', [2, 3], 'torch.float32', [2.0, 1.0, 1.0, 8.0, 4.0, -4.0]], 1e-05)
check('t02', lambda: a.sub(b, alpha=3), ['T', [2, 3], 'torch.float32', [-0.5, -6.5, 6.0, -2.0, 6.5, -9.0]], 1e-05)
check('t03', lambda: a.mul(b), ['T', [2, 3], 'torch.float32', [0.5, -3.0, -3.0, 8.0, -2.5, -6.0]], 1e-05)
check('t04', lambda: a.div(b), ['T', [2, 3], 'torch.float32', [2.0, -1.3333333730697632, -3.0, 2.0, -10.0, -6.0]], 1e-05)
check('t05', lambda: a.div(b, rounding_mode='floor'), ['T', [2, 3], 'torch.float32', [2.0, -2.0, -3.0, 2.0, -10.0, -6.0]], 1e-05)
check('t06', lambda: a.div(b, rounding_mode='trunc'), ['T', [2, 3], 'torch.float32', [2.0, -1.0, -3.0, 2.0, -10.0, -6.0]], 1e-05)
check('t07', lambda: torch.tensor([7, -7]).div(2, rounding_mode='trunc'), ['T', [2], 'torch.int64', [3, -3]], 1e-05)
check('t08', lambda: torch.tensor([7, -7]).div(2, rounding_mode='floor'), ['T', [2], 'torch.int64', [3, -4]], 1e-05)
check('t09', lambda: torch.tensor([7, -7]).div(2), ['T', [2], 'torch.float32', [3.5, -3.5]], 1e-05)
check('t10', lambda: a.matmul(m), ['T', [2, 2], 'torch.float32', [10.0, 12.0, -11.0, -8.0]], 1e-05)
check('t11', lambda: a.mm(m), ['T', [2, 2], 'torch.float32', [10.0, 12.0, -11.0, -8.0]], 1e-05)
check('t12', lambda: torch.ones(2, 2, 3).bmm(torch.ones(2, 3, 1)), ['T', [2, 2, 1], 'torch.float32', [3.0, 3.0, 3.0, 3.0]], 1e-05)
check('t13', lambda: v.dot(w), ['T', [], 'torch.float32', [1.0]], 1e-05)
check('t14', lambda: a.t(), ['T', [3, 2], 'torch.float32', [1.0, 4.0, -2.0, 5.0, 3.0, -6.0]], 1e-05)
check('t15', lambda: (lambda x: (x.t_(), x)[1])(torch.tensor([[1.0, 2.0, 3.0]])), ['T', [3, 1], 'torch.float32', [1.0, 2.0, 3.0]], 1e-05)
check('t16', lambda: a.flip(0), ['T', [2, 3], 'torch.float32', [4.0, 5.0, -6.0, 1.0, -2.0, 3.0]], 1e-05)
check('t17', lambda: a.flip(0, 1), ['T', [2, 3], 'torch.float32', [-6.0, 5.0, 4.0, 3.0, -2.0, 1.0]], 1e-05)
check('t18', lambda: a.fliplr(), ['T', [2, 3], 'torch.float32', [3.0, -2.0, 1.0, -6.0, 5.0, 4.0]], 1e-05)
check('t19', lambda: a.flipud(), ['T', [2, 3], 'torch.float32', [4.0, 5.0, -6.0, 1.0, -2.0, 3.0]], 1e-05)
check('t20', lambda: a.swapaxes(0, 1), ['T', [3, 2], 'torch.float32', [1.0, 4.0, -2.0, 5.0, 3.0, -6.0]], 1e-05)
check('t21', lambda: torch.ones(2, 3, 4).movedim(0, -1).shape, [3, 4, 2], 1e-05)
check('t22', lambda: torch.ones(2, 3, 4).movedim((0, 1), (2, 0)).shape, [3, 4, 2], 1e-05)
check('t23', lambda: torch.arange(12).unflatten(0, (3, 4)), ['T', [3, 4], 'torch.int64', [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]], 1e-05)
check('t24', lambda: torch.tensor([1, 2]).type_as(a), ['T', [2], 'torch.float32', [1.0, 2.0]], 1e-05)
check('t25', lambda: a.masked_fill(a > 2, 0.0), ['T', [2, 3], 'torch.float32', [1.0, -2.0, 0.0, 0.0, 0.0, -6.0]], 1e-05)
check('t26', lambda: a.masked_fill(torch.tensor([True, False, True]), -1), ['T', [2, 3], 'torch.float32', [-1.0, -2.0, -1.0, -1.0, 5.0, -1.0]], 1e-05)
check('t27', lambda: torch.tensor([1, 2]).masked_fill(torch.tensor([True, False]), 3.7), ['T', [2], 'torch.int64', [3, 2]], 1e-05)
check('t28', lambda: a.new_zeros(2), ['T', [2], 'torch.float32', [0.0, 0.0]], 1e-05)
check('t29', lambda: a.new_ones((1, 2)), ['T', [1, 2], 'torch.float32', [1.0, 1.0]], 1e-05)
check('t30', lambda: a.new_full((2,), 5), ['T', [2], 'torch.float32', [5.0, 5.0]], 1e-05)
check('t31', lambda: a.new_tensor([1, 2]), ['T', [2], 'torch.float32', [1.0, 2.0]], 1e-05)
check('t32', lambda: a.clamp_min(0), ['T', [2, 3], 'torch.float32', [1.0, 0.0, 3.0, 4.0, 5.0, 0.0]], 1e-05)
check('t33', lambda: a.clamp_max(0), ['T', [2, 3], 'torch.float32', [0.0, -2.0, 0.0, 0.0, 0.0, -6.0]], 1e-05)
check('t34', lambda: a.clamp(min=torch.tensor([0.0, 0.0, 0.0]), max=torch.tensor([2.0, 4.0, 1.0])), ['T', [2, 3], 'torch.float32', [1.0, 0.0, 1.0, 2.0, 4.0, 0.0]], 1e-05)
check('t35', lambda: torch.clamp(a, max=torch.tensor(1.0)), ['T', [2, 3], 'torch.float32', [1.0, -2.0, 1.0, 1.0, 1.0, -6.0]], 1e-05)
check('t36', lambda: grad_of(lambda x, lo, hi: torch.clamp(x, lo, hi), [1.0, -2.0, 3.0, 0.5], [0.0, 0.0, 0.0, 0.5], [2.0, 1.0, 2.0, 1.0]), [['T', [4], 'torch.float32', [1.0, 0.0, 0.0, 4.0]], ['T', [4], 'torch.float32', [0.0, 2.0, 0.0, 0.0]], ['T', [4], 'torch.float32', [0.0, 0.0, 3.0, 0.0]]], 1e-05)
check('i_exp_', lambda: inplace('exp_'), ['T', [3], 'torch.float32', [4.481688976287842, 9.487735748291016, 54.598148345947266]], 1e-05)
check('i_log_', lambda: inplace('log_'), ['T', [3], 'torch.float32', [0.40546512603759766, 0.8109301924705505, 1.3862943649291992]], 1e-05)
check('i_sqrt_', lambda: inplace('sqrt_'), ['T', [3], 'torch.float32', [1.2247449159622192, 1.5, 2.0]], 1e-05)
check('i_neg_', lambda: inplace('neg_'), ['T', [3], 'torch.float32', [-1.5, -2.25, -4.0]], 1e-05)
check('i_abs_', lambda: inplace('abs_'), ['T', [3], 'torch.float32', [1.5, 2.25, 4.0]], 1e-05)
check('i_sigmoid_', lambda: inplace('sigmoid_'), ['T', [3], 'torch.float32', [0.8175744414329529, 0.9046505093574524, 0.9820137619972229]], 1e-05)
check('i_tanh_', lambda: inplace('tanh_'), ['T', [3], 'torch.float32', [0.9051482677459717, 0.9780260920524597, 0.9993293285369873]], 1e-05)
check('i_relu_', lambda: inplace('relu_'), ['T', [3], 'torch.float32', [1.5, 2.25, 4.0]], 1e-05)
check('i_floor_', lambda: inplace('floor_'), ['T', [3], 'torch.float32', [1.0, 2.0, 4.0]], 1e-05)
check('i_round_', lambda: inplace('round_'), ['T', [3], 'torch.float32', [2.0, 2.0, 4.0]], 1e-05)
check('i_ceil_', lambda: inplace('ceil_'), ['T', [3], 'torch.float32', [2.0, 3.0, 4.0]], 1e-05)
check('i_trunc_', lambda: inplace('trunc_'), ['T', [3], 'torch.float32', [1.0, 2.0, 4.0]], 1e-05)
check('i_frac_', lambda: inplace('frac_'), ['T', [3], 'torch.float32', [0.5, 0.25, 0.0]], 1e-05)
check('i_reciprocal_', lambda: inplace('reciprocal_'), ['T', [3], 'torch.float32', [0.6666666865348816, 0.4444444477558136, 0.25]], 1e-05)
check('i_sin_', lambda: inplace('sin_'), ['T', [3], 'torch.float32', [0.9974949955940247, 0.7780731916427612, -0.756802499294281]], 1e-05)
check('i_square_', lambda: inplace('square_'), ['T', [3], 'torch.float32', [2.25, 5.0625, 16.0]], 1e-05)
check('ia00', lambda: inplace('pow_', 2), ['T', [3], 'torch.float32', [2.25, 5.0625, 16.0]], 1e-05)
check('ia01', lambda: inplace('addcmul_', torch.ones(3), torch.full((3,), 2.0), value=0.5), ['T', [3], 'torch.float32', [2.5, 3.25, 5.0]], 1e-05)
check('ia02', lambda: inplace('addcdiv_', torch.ones(3), torch.full((3,), 4.0), value=2), ['T', [3], 'torch.float32', [2.0, 2.75, 4.5]], 1e-05)
check('ia03', lambda: inplace('lerp_', torch.zeros(3), 0.25), ['T', [3], 'torch.float32', [1.125, 1.6875, 3.0]], 1e-05)
check('ia04', lambda: inplace('clamp_min_', 2.0), ['T', [3], 'torch.float32', [2.0, 2.25, 4.0]], 1e-05)
check('ia05', lambda: inplace('masked_fill_', torch.tensor([True, False, True]), 9.0), ['T', [3], 'torch.float32', [9.0, 2.25, 9.0]], 1e-05)
check('ia06', lambda: inplace('unsqueeze_', 0).shape, [1, 3], 1e-05)
check('ia07', lambda: (lambda t: (t.bernoulli_(0.5), 0.3 < t.mean().item() < 0.7 and set(t.tolist()) <= {0.0, 1.0})[1])(torch.zeros(1000)), True, 1e-05)
check('ia08', lambda: (lambda t: (t.exponential_(2.0), 0.3 < t.mean().item() < 0.7 and t.min().item() >= 0)[1])(torch.zeros(1000)), True, 1e-05)
check('ia09', lambda: (lambda t: (t.random_(0, 5), t.min().item() >= 0 and t.max().item() <= 4)[1])(torch.zeros(200, dtype=torch.int64)), True, 1e-05)
check('c00', lambda: torch.Tensor([1, 2, 3]), ['T', [3], 'torch.float32', [1.0, 2.0, 3.0]], 1e-06)
check('c01', lambda: torch.Tensor(2, 3).shape, [2, 3], 1e-06)
check('c02', lambda: torch.Tensor(2, 3).dtype, 'torch.float32', 1e-06)
check('c03', lambda: torch.FloatTensor([1.5, 2]), ['T', [2], 'torch.float32', [1.5, 2.0]], 1e-06)
check('c04', lambda: torch.LongTensor([1, 2]), ['T', [2], 'torch.int64', [1, 2]], 1e-06)
check('c05', lambda: torch.Tensor().shape, [0], 1e-06)
check('c06', lambda: torch.DoubleTensor(2).dtype, 'torch.float64', 1e-06)
check('c07', lambda: torch.zeros(2).type(), 'torch.FloatTensor', 1e-06)
check('c08', lambda: torch.zeros(2).type('torch.LongTensor').dtype, 'torch.int64', 1e-06)
check('c09', lambda: torch.ones_like(torch.zeros(2, 3), dtype=torch.int64), ['T', [2, 3], 'torch.int64', [1, 1, 1, 1, 1, 1]], 1e-06)
check('c10', lambda: torch.zeros_like(torch.zeros(2), requires_grad=True).requires_grad, True, 1e-06)
check('c11', lambda: torch.ones_like(torch.zeros(2), requires_grad=True).requires_grad, True, 1e-06)
check('c12', lambda: torch.randint(10, size=(3,)).shape, [3], 1e-06)
check('c13', lambda: torch.randint(3, 10, (2, 2)).shape, [2, 2], 1e-06)
check('c14', lambda: torch.normal(0.0, 1.0, size=(2, 3)).shape, [2, 3], 1e-06)
check('c15', lambda: torch.normal(torch.zeros(3), 1.0).shape, [3], 1e-06)
check('c16', lambda: torch.normal(torch.zeros(3), torch.ones(3)).shape, [3], 1e-06)
check('c17', lambda: torch.finfo(torch.float32).eps, 1.1920928955078125e-07, 1e-06)
check('c18', lambda: torch.finfo(torch.float32).max, 3.4028234663852886e+38, 1e-06)
check('c19', lambda: torch.finfo(torch.float32).tiny, 1.1754943508222875e-38, 1e-06)
check('c20', lambda: torch.finfo().eps, 1.1920928955078125e-07, 1e-06)
check('c21', lambda: torch.finfo(torch.float64).eps, 2.220446049250313e-16, 1e-06)
check('c22', lambda: torch.iinfo(torch.int64).max, 9223372036854775807, 1e-06)
check('c23', lambda: torch.iinfo(torch.int32).min, -2147483648, 1e-06)
check('c24', lambda: torch.iinfo(torch.uint8).max, 255, 1e-06)
check('c25', lambda: torch.logspace(0, 2, 3), ['T', [3], 'torch.float32', [1.0, 10.0, 100.0]], 1e-06)
check('c26', lambda: torch.logspace(0, 3, 4, base=2), ['T', [4], 'torch.float32', [1.0, 2.0, 4.0, 8.0]], 1e-06)
check('c27', lambda: torch.tensor([torch.tensor(1.0), 2.0]), ['T', [2], 'torch.float32', [1.0, 2.0]], 1e-06)
"#);
    assert_all_true(&out, 91);
}

/// amax/amin/logsumexp/median/cumprod/nansum/nanmean/aminmax/kthvalue/mode/cummax/cummin/logcumsumexp/diff/nanmedian/unique/bincount, with gradients.
#[test]
fn reductions_sorting_and_counting() {
    let out = run(r#"
A = [[1.0, -2.0, 3.0], [4.0, 5.0, -6.0]]
a = torch.tensor(A); b = torch.tensor([[0.5, 1.5, -1.0], [2.0, -0.5, 1.0]])
v = torch.tensor([3.0, 1.0, 2.0]); w = torch.tensor([1.0, 0.0, -1.0])
m = torch.tensor([[1.0, 2.0], [3.0, 4.0], [5.0, 6.0]])
def grad_of(f, *vals):
    xs = [torch.tensor(val, requires_grad=True) for val in vals]
    out = f(*xs)
    out = out if out.dim() == 0 else (out * torch.arange(1, out.numel() + 1, dtype=out.dtype).reshape(out.shape)).sum()
    out.backward()
    return [x.grad for x in xs]
def inplace(name, *args, **kwargs):
    t = torch.tensor([1.5, 2.25, 4.0])
    getattr(t, name)(*args, **kwargs)
    return t

x = torch.tensor([[1.0, 5.0, 2.0, 5.0], [3.0, -1.0, 3.0, 0.0]])
u = torch.tensor([3, 1, 2, 1, 3])
check('r00', lambda: x.amax(1), ['T', [2], 'torch.float32', [5.0, 3.0]], 1e-05)
check('r01', lambda: x.amin(0), ['T', [4], 'torch.float32', [1.0, -1.0, 2.0, 0.0]], 1e-05)
check('r02', lambda: torch.amax(x, (0, 1)), ['T', [], 'torch.float32', [5.0]], 1e-05)
check('r03', lambda: torch.logsumexp(x, 1), ['T', [2], 'torch.float32', [5.7266316413879395, 3.7266316413879395]], 1e-05)
check('r04', lambda: x.logsumexp(0, keepdim=True), ['T', [1, 4], 'torch.float32', [3.1269280910491943, 5.002475738525391, 3.3132617473602295, 5.006715297698975]], 1e-05)
check('r05', lambda: torch.median(x), ['T', [], 'torch.float32', [2.0]], 1e-05)
check('r06', lambda: x.median(1), [['T', [2], 'torch.float32', [2.0, 0.0]], ['T', [2], 'torch.int64', [2, 3]]], 1e-05)
check('r07', lambda: torch.median(torch.tensor([3.0, 1.0, 2.0, 1.0])), ['T', [], 'torch.float32', [1.0]], 1e-05)
check('r08', lambda: x.cumprod(1), ['T', [2, 4], 'torch.float32', [1.0, 5.0, 10.0, 50.0, 3.0, -3.0, -9.0, -0.0]], 1e-05)
check('r09', lambda: torch.nansum(torch.tensor([1.0, float('nan'), 2.0])), ['T', [], 'torch.float32', [3.0]], 1e-05)
check('r10', lambda: torch.nanmean(torch.tensor([1.0, float('nan'), 2.0])), ['T', [], 'torch.float32', [1.5]], 1e-05)
check('r11', lambda: torch.aminmax(x), [['T', [], 'torch.float32', [-1.0]], ['T', [], 'torch.float32', [5.0]]], 1e-05)
check('r12', lambda: torch.kthvalue(torch.tensor([3.0, 1.0, 2.0, 5.0]), 2), [['T', [], 'torch.float32', [2.0]], ['T', [], 'torch.int64', [2]]], 1e-05)
check('r13', lambda: x.kthvalue(2, 1), [['T', [2], 'torch.float32', [2.0, 0.0]], ['T', [2], 'torch.int64', [2, 3]]], 1e-05)
check('r14', lambda: torch.mode(torch.tensor([1, 2, 2, 3, 3, 1, 3, 2])), [['T', [], 'torch.int64', [2]], ['T', [], 'torch.int64', [7]]], 1e-05)
check('r15', lambda: x.mode(1), [['T', [2], 'torch.float32', [5.0, 3.0]], ['T', [2], 'torch.int64', [3, 2]]], 1e-05)
check('r16', lambda: torch.cummax(torch.tensor([1.0, 3.0, 3.0, 2.0, 5.0]), 0), [['T', [5], 'torch.float32', [1.0, 3.0, 3.0, 3.0, 5.0]], ['T', [5], 'torch.int64', [0, 1, 2, 2, 4]]], 1e-05)
check('r17', lambda: torch.cummin(torch.tensor([3.0, 1.0, 1.0, 2.0]), 0), [['T', [4], 'torch.float32', [3.0, 1.0, 1.0, 1.0]], ['T', [4], 'torch.int64', [0, 1, 2, 2]]], 1e-05)
check('r18', lambda: torch.logcumsumexp(torch.tensor([1.0, 2.0, 3.0]), 0), ['T', [3], 'torch.float32', [1.0, 2.3132617473602295, 3.4076058864593506]], 1e-05)
check('r19', lambda: torch.diff(torch.tensor([1, 4, 9, 16])), ['T', [3], 'torch.int64', [3, 5, 7]], 1e-05)
check('r20', lambda: torch.diff(x, dim=0), ['T', [1, 4], 'torch.float32', [2.0, -6.0, 1.0, -5.0]], 1e-05)
check('r21', lambda: torch.diff(torch.tensor([1.0, 3.0, 6.0]), n=2), ['T', [1], 'torch.float32', [1.0]], 1e-05)
check('r22', lambda: torch.diff(torch.tensor([1.0, 3.0]), prepend=torch.tensor([0.0]), append=torch.tensor([10.0])), ['T', [3], 'torch.float32', [1.0, 2.0, 7.0]], 1e-05)
check('r23', lambda: torch.nanmedian(torch.tensor([1.0, float('nan'), 3.0, 2.0])), ['T', [], 'torch.float32', [2.0]], 1e-05)
check('r24', lambda: torch.unique(u), ['T', [3], 'torch.int64', [1, 2, 3]], 1e-05)
check('r25', lambda: torch.unique(u, return_inverse=True, return_counts=True), [['T', [3], 'torch.int64', [1, 2, 3]], ['T', [5], 'torch.int64', [2, 0, 1, 0, 2]], ['T', [3], 'torch.int64', [2, 1, 2]]], 1e-05)
check('r26', lambda: torch.unique(torch.tensor([[1, 2], [1, 2], [0, 5]]), dim=0, return_counts=True), [['T', [2, 2], 'torch.int64', [0, 5, 1, 2]], ['T', [2], 'torch.int64', [1, 2]]], 1e-05)
check('r27', lambda: torch.bincount(torch.tensor([1, 1, 3])), ['T', [4], 'torch.int64', [0, 2, 0, 1]], 1e-05)
check('r28', lambda: torch.bincount(torch.tensor([1, 1, 3]), weights=torch.tensor([0.5, 1.0, 2.0])), ['T', [4], 'torch.float32', [0.0, 1.5, 0.0, 2.0]], 1e-05)
check('r29', lambda: torch.bincount(torch.tensor([1]), minlength=4), ['T', [4], 'torch.int64', [0, 1, 0, 0]], 1e-05)
check('r30', lambda: grad_of(lambda t: t.amax(1), [[1.0, 5.0, 5.0], [2.0, 0.0, 1.0]]), [['T', [2, 3], 'torch.float32', [0.0, 0.5, 0.5, 2.0, 0.0, 0.0]]], 1e-05)
check('r31', lambda: grad_of(lambda t: torch.logsumexp(t, 1), [[1.0, 2.0], [3.0, 0.0]]), [['T', [2, 2], 'torch.float32', [0.26894140243530273, 0.7310585379600525, 1.9051482677459717, 0.09485174715518951]]], 1e-05)
check('r32', lambda: grad_of(lambda t: t.median(), [2.0, 1.0, 3.0, 1.0]), [['T', [4], 'torch.float32', [0.0, 0.5, 0.0, 0.5]]], 1e-05)
check('r33', lambda: grad_of(lambda t: t.cumprod(0), [2.0, 0.0, 3.0, 4.0]), [['T', [4], 'torch.float32', [1.0, 118.0, 0.0, 0.0]]], 1e-05)
check('r34', lambda: grad_of(lambda t: torch.cummax(t, 0).values, [1.0, 3.0, 2.0, 5.0]), [['T', [4], 'torch.float32', [1.0, 5.0, 0.0, 4.0]]], 1e-05)
check('r35', lambda: grad_of(lambda t: torch.logcumsumexp(t, 0), [2.0, 1.0, 3.0]), [['T', [3], 'torch.float32', [3.196302652359009, 0.8079745173454285, 1.995723009109497]]], 1e-05)
"#);
    assert_all_true(&out, 36);
}

/// Logical and comparison functions, isinf/isnan/isfinite/nan_to_num, the unary math family (log1p ... erfinv) and binary family (remainder, fmod, floor_divide, fmax/fmin, lerp, addcmul/addcdiv), with gradients.
#[test]
fn elementwise_math_logic_and_their_gradients() {
    let out = run(r#"
A = [[1.0, -2.0, 3.0], [4.0, 5.0, -6.0]]
a = torch.tensor(A); b = torch.tensor([[0.5, 1.5, -1.0], [2.0, -0.5, 1.0]])
v = torch.tensor([3.0, 1.0, 2.0]); w = torch.tensor([1.0, 0.0, -1.0])
m = torch.tensor([[1.0, 2.0], [3.0, 4.0], [5.0, 6.0]])
def grad_of(f, *vals):
    xs = [torch.tensor(val, requires_grad=True) for val in vals]
    out = f(*xs)
    out = out if out.dim() == 0 else (out * torch.arange(1, out.numel() + 1, dtype=out.dtype).reshape(out.shape)).sum()
    out.backward()
    return [x.grad for x in xs]
def inplace(name, *args, **kwargs):
    t = torch.tensor([1.5, 2.25, 4.0])
    getattr(t, name)(*args, **kwargs)
    return t

p, q = torch.tensor([True, False, True]), torch.tensor([1.0, 0.0, 0.0])
z = torch.tensor([1.0, float("inf"), float("nan"), -float("inf"), 0.0])
s = torch.tensor([0.25, 0.5, -0.75, 1.5]); y = torch.tensor([0.1, 0.5, -0.9, 0.99])
x5 = torch.tensor([5.0, -5.0, 7.5, -7.5]); y5 = torch.tensor([3.0, 3.0, -2.0, -2.0])
n1 = torch.tensor([1.0, float("nan"), 3.0]); n2 = torch.tensor([float("nan"), 2.0, 1.0])
check('e00', lambda: torch.logical_and(p, q), ['T', [3], 'torch.bool', [True, False, False]], 1e-05)
check('e01', lambda: torch.logical_or(p, q), ['T', [3], 'torch.bool', [True, False, True]], 1e-05)
check('e02', lambda: torch.logical_xor(p, q), ['T', [3], 'torch.bool', [False, False, True]], 1e-05)
check('e03', lambda: torch.logical_not(q), ['T', [3], 'torch.bool', [False, True, True]], 1e-05)
check('e04', lambda: torch.eq(v, 1.0), ['T', [3], 'torch.bool', [False, True, False]], 1e-05)
check('e05', lambda: torch.ne(v, w), ['T', [3], 'torch.bool', [True, True, True]], 1e-05)
check('e06', lambda: torch.gt(v, 1), ['T', [3], 'torch.bool', [True, False, True]], 1e-05)
check('e07', lambda: torch.ge(v, 2), ['T', [3], 'torch.bool', [True, False, True]], 1e-05)
check('e08', lambda: torch.lt(v, w), ['T', [3], 'torch.bool', [False, False, False]], 1e-05)
check('e09', lambda: torch.le(v, 1), ['T', [3], 'torch.bool', [False, True, False]], 1e-05)
check('e10', lambda: torch.isinf(z), ['T', [5], 'torch.bool', [False, True, False, True, False]], 1e-05)
check('e11', lambda: torch.isnan(z), ['T', [5], 'torch.bool', [False, False, True, False, False]], 1e-05)
check('e12', lambda: torch.isfinite(z), ['T', [5], 'torch.bool', [True, False, False, False, True]], 1e-05)
check('e13', lambda: torch.nan_to_num(z), ['T', [5], 'torch.float32', [1.0, 3.4028234663852886e+38, 0.0, -3.4028234663852886e+38, 0.0]], 1e-05)
check('e14', lambda: torch.nan_to_num(z, nan=1.0, posinf=2.0, neginf=-2.0), ['T', [5], 'torch.float32', [1.0, 2.0, 1.0, -2.0, 0.0]], 1e-05)
check('e15', lambda: torch.log1p(s), ['T', [4], 'torch.float32', [0.2231435477733612, 0.40546509623527527, -1.3862943649291992, 0.9162907600402832]], 1e-05)
check('e16', lambda: torch.expm1(s), ['T', [4], 'torch.float32', [0.2840254306793213, 0.6487212777137756, -0.5276334285736084, 3.481688976287842]], 1e-05)
check('e17', lambda: torch.log2(s.abs()), ['T', [4], 'torch.float32', [-2.0, -1.0, -0.41503751277923584, 0.5849624872207642]], 1e-05)
check('e18', lambda: torch.log10(s.abs()), ['T', [4], 'torch.float32', [-0.6020600199699402, -0.3010300099849701, -0.1249387338757515, 0.1760912537574768]], 1e-05)
check('e19', lambda: torch.exp2(s), ['T', [4], 'torch.float32', [1.1892070770263672, 1.4142135381698608, 0.5946035385131836, 2.8284270763397217]], 1e-05)
check('e20', lambda: torch.erf(s), ['T', [4], 'torch.float32', [0.27632638812065125, 0.5204998850822449, -0.7111556529998779, 0.9661051630973816]], 1e-05)
check('e21', lambda: torch.erfinv(y), ['T', [4], 'torch.float32', [0.08885598927736282, 0.4769362807273865, -1.1630871295928955, 1.8213865756988525]], 1e-05)
check('e22', lambda: torch.tan(s), ['T', [4], 'torch.float32', [0.25534191727638245, 0.5463024973869324, -0.9315964579582214, 14.101419448852539]], 1e-05)
check('e23', lambda: torch.asin(y), ['T', [4], 'torch.float32', [0.1001674234867096, 0.5235987901687622, -1.1197694540023804, 1.4292569160461426]], 1e-05)
check('e24', lambda: torch.acos(y), ['T', [4], 'torch.float32', [1.4706288576126099, 1.0471975803375244, 2.690565824508667, 0.14153940975666046]], 1e-05)
check('e25', lambda: torch.atan(s), ['T', [4], 'torch.float32', [0.244978666305542, 0.46364760398864746, -0.6435011029243469, 0.9827936887741089]], 1e-05)
check('e26', lambda: torch.atan2(s, torch.tensor([1.0, -1.0, 0.5, -2.0])), ['T', [4], 'torch.float32', [0.244978666305542, 2.677945137023926, -0.9827937483787537, 2.498091459274292]], 1e-05)
check('e27', lambda: torch.sinh(s), ['T', [4], 'torch.float32', [0.25261232256889343, 0.5210952758789062, -0.8223167061805725, 2.129279375076294]], 1e-05)
check('e28', lambda: torch.cosh(s), ['T', [4], 'torch.float32', [1.0314130783081055, 1.1276259422302246, 1.2946833372116089, 2.352409601211548]], 1e-05)
check('e29', lambda: torch.trunc(torch.tensor([1.7, -1.7])), ['T', [2], 'torch.float32', [1.0, -1.0]], 1e-05)
check('e30', lambda: torch.frac(torch.tensor([1.7, -1.7])), ['T', [2], 'torch.float32', [0.7000000476837158, -0.7000000476837158]], 1e-05)
check('e31', lambda: torch.sign(s), ['T', [4], 'torch.float32', [1.0, 1.0, -1.0, 1.0]], 1e-05)
check('e32', lambda: torch.sgn(s), ['T', [4], 'torch.float32', [1.0, 1.0, -1.0, 1.0]], 1e-05)
check('e33', lambda: torch.rsqrt(s.abs()), ['T', [4], 'torch.float32', [2.0, 1.4142135381698608, 1.154700517654419, 0.8164965510368347]], 1e-05)
check('e34', lambda: torch.reciprocal(s), ['T', [4], 'torch.float32', [4.0, 2.0, -1.3333333730697632, 0.6666666865348816]], 1e-05)
check('e35', lambda: torch.erf(torch.tensor([3.5, -4.0, 6.5, 0.0])), ['T', [4], 'torch.float32', [0.9999992847442627, -1.0, 1.0, 0.0]], 1e-05)
check('e36', lambda: torch.remainder(x5, y5), ['T', [4], 'torch.float32', [2.0, 1.0, -0.5, -1.5]], 1e-05)
check('e37', lambda: torch.fmod(x5, y5), ['T', [4], 'torch.float32', [2.0, -2.0, 1.5, -1.5]], 1e-05)
check('e38', lambda: torch.floor_divide(x5, y5), ['T', [4], 'torch.float32', [1.0, -2.0, -4.0, 3.0]], 1e-05)
check('e39', lambda: torch.true_divide(x5, y5), ['T', [4], 'torch.float32', [1.6666666269302368, -1.6666666269302368, -3.75, 3.75]], 1e-05)
check('e40', lambda: torch.fmax(n1, n2), ['T', [3], 'torch.float32', [1.0, 2.0, 3.0]], 1e-05)
check('e41', lambda: torch.fmin(n1, n2), ['T', [3], 'torch.float32', [1.0, 2.0, 1.0]], 1e-05)
check('e42', lambda: torch.maximum(n1, n2), ['T', [3], 'torch.float32', [float("nan"), float("nan"), 3.0]], 1e-05)
check('e43', lambda: torch.minimum(x5, y5), ['T', [4], 'torch.float32', [3.0, -5.0, -2.0, -7.5]], 1e-05)
check('e44', lambda: torch.lerp(torch.zeros(3), torch.tensor([1.0, 2.0, 3.0]), 0.75), ['T', [3], 'torch.float32', [0.75, 1.5, 2.25]], 1e-05)
check('e45', lambda: torch.lerp(torch.zeros(3), torch.ones(3), torch.tensor([0.2, 0.6, 1.0])), ['T', [3], 'torch.float32', [0.20000000298023224, 0.6000000238418579, 1.0]], 1e-05)
check('e46', lambda: torch.addcmul(torch.ones(2), torch.tensor([1.0, 2.0]), torch.tensor([3.0, 4.0]), value=0.5), ['T', [2], 'torch.float32', [2.5, 5.0]], 1e-05)
check('e47', lambda: torch.addcdiv(torch.ones(2), torch.tensor([1.0, 2.0]), torch.tensor([3.0, 4.0]), value=2), ['T', [2], 'torch.float32', [1.6666667461395264, 2.0]], 1e-05)
check('e48', lambda: err(lambda: torch.tensor([1, 2]) // torch.tensor([1, 0])), ['RuntimeError', True], 1e-05)
check('e49', lambda: [grad_of(getattr(torch, name), [0.2, 0.5, 0.7]) for name in ['log1p', 'expm1', 'log2', 'log10', 'exp2', 'erf', 'erfinv', 'tan', 'asin', 'acos', 'atan', 'sinh', 'cosh', 'asinh', 'atanh', 'rsqrt', 'reciprocal', 'erfc']], [[['T', [3], 'torch.float32', [0.8333333134651184, 1.3333333730697632, 1.764705777168274]]], [['T', [3], 'torch.float32', [1.2214027643203735, 3.2974424362182617, 6.041257858276367]]], [['T', [3], 'torch.float32', [7.213475227355957, 5.770780086517334, 6.182978630065918]]], [['T', [3], 'torch.float32', [2.1714723110198975, 1.737177848815918, 1.8612619638442993]]], [['T', [3], 'torch.float32', [0.7962170243263245, 1.9605162143707275, 3.3780627250671387]]], [['T', [3], 'torch.float32', [1.084134817123413, 1.7575652599334717, 2.073824882507324]]], [['T', [3], 'torch.float32', [0.9151294231414795, 2.2251698970794678, 4.5490899085998535]]], [['T', [3], 'torch.float32', [1.0410913228988647, 2.596892833709717, 5.128349304199219]]], [['T', [3], 'torch.float32', [1.0206207036972046, 2.309401035308838, 4.200839996337891]]], [['T', [3], 'torch.float32', [-1.0206207036972046, -2.309401035308838, -4.200839996337891]]], [['T', [3], 'torch.float32', [0.9615384936332703, 1.600000023841858, 2.013422727584839]]], [['T', [3], 'torch.float32', [1.020066738128662, 2.255251884460449, 3.765507221221924]]], [['T', [3], 'torch.float32', [0.20133601129055023, 1.0421905517578125, 2.2757511138916016]]], [['T', [3], 'torch.float32', [0.9805806875228882, 1.7888543605804443, 2.457695722579956]]], [['T', [3], 'torch.float32', [1.0416667461395264, 2.6666667461395264, 5.882352828979492]]], [['T', [3], 'torch.float32', [-5.590169906616211, -2.8284268379211426, -2.561203956604004]]], [['T', [3], 'torch.float32', [-25.0, -8.0, -6.122448921203613]]], [['T', [3], 'torch.float32', [-1.084134817123413, -1.7575652599334717, -2.073824882507324]]]], 1e-05)
check('e50', lambda: grad_of(torch.remainder, [5.0, -5.0, 7.5], [3.0, 3.0, -2.0]), [['T', [3], 'torch.float32', [1.0, 2.0, 3.0]], ['T', [3], 'torch.float32', [-1.0, 4.0, 12.0]]], 1e-05)
check('e51', lambda: grad_of(torch.atan2, [1.0, -2.0], [0.5, 3.0]), [['T', [2], 'torch.float32', [0.4000000059604645, 0.46153849363327026]], ['T', [2], 'torch.float32', [-0.800000011920929, 0.3076923191547394]]], 1e-05)
check('e52', lambda: grad_of(lambda s_, e: torch.lerp(s_, e, 0.3), [1.0, 2.0], [4.0, -1.0]), [['T', [2], 'torch.float32', [0.699999988079071, 1.399999976158142]], ['T', [2], 'torch.float32', [0.30000001192092896, 0.6000000238418579]]], 1e-05)
check('e53', lambda: grad_of(lambda p_, q_, r_: torch.addcdiv(p_, q_, r_, value=2), [1.0], [2.0], [4.0]), [['T', [1], 'torch.float32', [1.0]], ['T', [1], 'torch.float32', [0.5]], ['T', [1], 'torch.float32', [-0.25]]], 1e-05)
check('e54', lambda: grad_of(torch.fmax, [1.0, 3.0], [2.0, 1.0]), [['T', [2], 'torch.float32', [0.0, 2.0]], ['T', [2], 'torch.float32', [1.0, 0.0]]], 1e-05)
"#);
    assert_all_true(&out, 55);
}

/// scatter/scatter_add/index_add/index_fill/index_copy/index_select/masked_select/take/take_along_dim/gather/masked_scatter, one-argument where, nonzero(as_tuple), argwhere, searchsorted/bucketize and mixed basic+advanced assignment, with gradients.
#[test]
fn scatter_gather_index_and_search() {
    let out = run(r#"
A = [[1.0, -2.0, 3.0], [4.0, 5.0, -6.0]]
a = torch.tensor(A); b = torch.tensor([[0.5, 1.5, -1.0], [2.0, -0.5, 1.0]])
v = torch.tensor([3.0, 1.0, 2.0]); w = torch.tensor([1.0, 0.0, -1.0])
m = torch.tensor([[1.0, 2.0], [3.0, 4.0], [5.0, 6.0]])
def grad_of(f, *vals):
    xs = [torch.tensor(val, requires_grad=True) for val in vals]
    out = f(*xs)
    out = out if out.dim() == 0 else (out * torch.arange(1, out.numel() + 1, dtype=out.dtype).reshape(out.shape)).sum()
    out.backward()
    return [x.grad for x in xs]
def inplace(name, *args, **kwargs):
    t = torch.tensor([1.5, 2.25, 4.0])
    getattr(t, name)(*args, **kwargs)
    return t

idx = torch.tensor([[0, 1, 2, 0], [2, 0, 0, 1]])
src = torch.arange(1.0, 9.0).reshape(2, 4)
nz = torch.tensor([[0, 1], [2, 0]])
ss = torch.tensor([1, 3, 5, 7, 9])
check('x00', lambda: torch.zeros(3, 4).scatter(0, idx, src), ['T', [3, 4], 'torch.float32', [1.0, 6.0, 7.0, 4.0, 0.0, 2.0, 0.0, 8.0, 5.0, 0.0, 3.0, 0.0]], 1e-06)
check('x01', lambda: torch.zeros(3, 4).scatter(1, torch.tensor([[1], [3], [0]]), 5.0), ['T', [3, 4], 'torch.float32', [0.0, 5.0, 0.0, 0.0, 0.0, 0.0, 0.0, 5.0, 5.0, 0.0, 0.0, 0.0]], 1e-06)
check('x02', lambda: torch.scatter_add(torch.zeros(3, 4), 0, idx, src), ['T', [3, 4], 'torch.float32', [1.0, 6.0, 7.0, 4.0, 0.0, 2.0, 0.0, 8.0, 5.0, 0.0, 3.0, 0.0]], 1e-06)
check('x03', lambda: torch.zeros(5).index_add(0, torch.tensor([0, 4, 0]), torch.tensor([1.0, 2.0, 3.0])), ['T', [5], 'torch.float32', [4.0, 0.0, 0.0, 0.0, 2.0]], 1e-06)
check('x04', lambda: torch.zeros(2, 3).index_add(1, torch.tensor([2, 0]), torch.tensor([[1.0, 2.0], [3.0, 4.0]])), ['T', [2, 3], 'torch.float32', [2.0, 0.0, 1.0, 4.0, 0.0, 3.0]], 1e-06)
check('x05', lambda: torch.zeros(2, 3).index_fill(1, torch.tensor([0, 2]), -1.0), ['T', [2, 3], 'torch.float32', [-1.0, 0.0, -1.0, -1.0, 0.0, -1.0]], 1e-06)
check('x06', lambda: torch.zeros(3, 2).index_copy(0, torch.tensor([2, 0]), torch.tensor([[1.0, 2.0], [3.0, 4.0]])), ['T', [3, 2], 'torch.float32', [3.0, 4.0, 0.0, 0.0, 1.0, 2.0]], 1e-06)
check('x07', lambda: torch.arange(6).reshape(2, 3).index_select(1, torch.tensor([2, 0])), ['T', [2, 2], 'torch.int64', [2, 0, 5, 3]], 1e-06)
check('x08', lambda: torch.masked_select(a, a > 1), ['T', [3], 'torch.float32', [3.0, 4.0, 5.0]], 1e-06)
check('x09', lambda: torch.take(a, torch.tensor([0, 5, 2])), ['T', [3], 'torch.float32', [1.0, -6.0, 3.0]], 1e-06)
check('x10', lambda: torch.take_along_dim(a, torch.tensor([[0], [2]]), 1), ['T', [2, 1], 'torch.float32', [1.0, -6.0]], 1e-06)
check('x11', lambda: torch.gather(a, 1, torch.tensor([[2, 0], [1, 1]])), ['T', [2, 2], 'torch.float32', [3.0, 1.0, 5.0, 5.0]], 1e-06)
check('x12', lambda: torch.zeros(2, 2).masked_scatter(torch.tensor([[True, False], [True, True]]), torch.tensor([7.0, 8.0, 9.0, 10.0])), ['T', [2, 2], 'torch.float32', [7.0, 0.0, 8.0, 9.0]], 1e-06)
check('x13', lambda: (lambda t: (t.scatter_(1, torch.tensor([[0], [1]]), torch.ones(2, 1)), t)[1])(torch.zeros(2, 2)), ['T', [2, 2], 'torch.float32', [1.0, 0.0, 0.0, 1.0]], 1e-06)
check('x14', lambda: (lambda t: (t.index_add_(0, torch.tensor([1, 1]), torch.ones(2, 2)), t)[1])(torch.zeros(2, 2)), ['T', [2, 2], 'torch.float32', [0.0, 0.0, 2.0, 2.0]], 1e-06)
check('x15', lambda: torch.where(nz), [['T', [2], 'torch.int64', [0, 1]], ['T', [2], 'torch.int64', [1, 0]]], 1e-06)
check('x16', lambda: torch.nonzero(nz), ['T', [2, 2], 'torch.int64', [0, 1, 1, 0]], 1e-06)
check('x17', lambda: torch.nonzero(nz, as_tuple=True), [['T', [2], 'torch.int64', [0, 1]], ['T', [2], 'torch.int64', [1, 0]]], 1e-06)
check('x18', lambda: torch.argwhere(nz), ['T', [2, 2], 'torch.int64', [0, 1, 1, 0]], 1e-06)
check('x19', lambda: torch.searchsorted(ss, torch.tensor([3, 6, 9])), ['T', [3], 'torch.int64', [1, 3, 4]], 1e-06)
check('x20', lambda: torch.searchsorted(ss, torch.tensor([3, 6, 9]), right=True), ['T', [3], 'torch.int64', [2, 3, 5]], 1e-06)
check('x21', lambda: torch.searchsorted(ss, 4), ['T', [], 'torch.int64', [2]], 1e-06)
check('x22', lambda: torch.searchsorted(torch.tensor([[1, 3, 5], [2, 4, 6]]), torch.tensor([[3], [3]])), ['T', [2, 1], 'torch.int64', [1, 1]], 1e-06)
check('x23', lambda: torch.bucketize(torch.tensor([[1, 6], [9, 3]]), ss), ['T', [2, 2], 'torch.int64', [0, 3, 4, 1]], 1e-06)
check('x24', lambda: torch.bucketize(torch.tensor([1, 6, 9]), ss, right=True), ['T', [3], 'torch.int64', [1, 3, 5]], 1e-06)
check('x25', lambda: grad_of(lambda s_, src_: s_.scatter(0, torch.tensor([[0, 1, 2], [2, 0, 0]]), src_), [[1.0, 2.0, 3.0], [4.0, 5.0, 6.0], [7.0, 8.0, 9.0]], [[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]]), [['T', [3, 3], 'torch.float32', [0.0, 0.0, 0.0, 4.0, 0.0, 6.0, 0.0, 8.0, 0.0]], ['T', [2, 3], 'torch.float32', [1.0, 5.0, 9.0, 7.0, 2.0, 3.0]]], 1e-06)
check('x26', lambda: grad_of(lambda s_, src_: s_.scatter_add(1, torch.tensor([[0, 0], [1, 2]]), src_), [[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]], [[1.0, 2.0], [3.0, 4.0]]), [['T', [2, 3], 'torch.float32', [1.0, 2.0, 3.0, 4.0, 5.0, 6.0]], ['T', [2, 2], 'torch.float32', [1.0, 1.0, 5.0, 6.0]]], 1e-06)
check('x27', lambda: grad_of(lambda s_, src_: s_.index_add(0, torch.tensor([1, 1]), src_, alpha=2), [[1.0, 2.0], [3.0, 4.0]], [[1.0, 1.0], [2.0, 2.0]]), [['T', [2, 2], 'torch.float32', [1.0, 2.0, 3.0, 4.0]], ['T', [2, 2], 'torch.float32', [6.0, 8.0, 6.0, 8.0]]], 1e-06)
check('x28', lambda: grad_of(lambda s_, src_: s_.index_copy(0, torch.tensor([1]), src_), [[1.0, 2.0], [3.0, 4.0]], [[5.0, 6.0]]), [['T', [2, 2], 'torch.float32', [1.0, 2.0, 0.0, 0.0]], ['T', [1, 2], 'torch.float32', [3.0, 4.0]]], 1e-06)
check('x29', lambda: grad_of(lambda s_: torch.take(s_, torch.tensor([0, 0, 3])), [[1.0, 2.0], [3.0, 4.0]]), [['T', [2, 2], 'torch.float32', [3.0, 0.0, 0.0, 3.0]]], 1e-06)
check('x30', lambda: (lambda t: (t.__setitem__((slice(None), torch.tensor([0, 2])), torch.tensor([1.0, 2.0])), t.__setitem__((0, torch.tensor([0, 2])), 5.0), t.__setitem__((1, [1, 3]), torch.tensor([7.0, 8.0])), t)[-1])(torch.zeros(3, 4)), ['T', [3, 4], 'torch.float32', [5.0, 0.0, 5.0, 0.0, 1.0, 7.0, 2.0, 8.0, 1.0, 0.0, 2.0, 0.0]], 1e-06)
check('x31', lambda: (lambda u_: (u_.__setitem__((slice(None), 1, [0, 3]), 9.0), u_.__setitem__((Ellipsis, 2), 4.0), u_)[-1])(torch.zeros(2, 3, 4)), ['T', [2, 3, 4], 'torch.float32', [0.0, 0.0, 4.0, 0.0, 9.0, 0.0, 4.0, 9.0, 0.0, 0.0, 4.0, 0.0, 0.0, 0.0, 4.0, 0.0, 9.0, 0.0, 4.0, 9.0, 0.0, 0.0, 4.0, 0.0]], 1e-06)
check('x32', lambda: (lambda v_: (v_.__setitem__(([0, 2], slice(1, None)), torch.tensor([[1.0, 2.0], [3.0, 4.0]])), v_)[-1])(torch.zeros(4, 3)), ['T', [4, 3], 'torch.float32', [0.0, 1.0, 2.0, 0.0, 0.0, 0.0, 0.0, 3.0, 4.0, 0.0, 0.0, 0.0]], 1e-06)
"#);
    assert_all_true(&out, 33);
}

/// tile/hstack/vstack/dstack/column_stack/tensor_split/broadcast_tensors/broadcast_to/meshgrid(indexing)/rot90/diagonal/diag(x, k)/diag_embed/squeeze(tuple)/cartesian_prod/triu_indices/tril_indices/combinations, with gradients.
#[test]
fn shape_construction_ops() {
    let out = run(r#"
A = [[1.0, -2.0, 3.0], [4.0, 5.0, -6.0]]
a = torch.tensor(A); b = torch.tensor([[0.5, 1.5, -1.0], [2.0, -0.5, 1.0]])
v = torch.tensor([3.0, 1.0, 2.0]); w = torch.tensor([1.0, 0.0, -1.0])
m = torch.tensor([[1.0, 2.0], [3.0, 4.0], [5.0, 6.0]])
def grad_of(f, *vals):
    xs = [torch.tensor(val, requires_grad=True) for val in vals]
    out = f(*xs)
    out = out if out.dim() == 0 else (out * torch.arange(1, out.numel() + 1, dtype=out.dtype).reshape(out.shape)).sum()
    out.backward()
    return [x.grad for x in xs]
def inplace(name, *args, **kwargs):
    t = torch.tensor([1.5, 2.25, 4.0])
    getattr(t, name)(*args, **kwargs)
    return t
g6 = torch.arange(6).reshape(2, 3)
check('s00', lambda: g6.tile((2,)), ['T', [2, 6], 'torch.int64', [0, 1, 2, 0, 1, 2, 3, 4, 5, 3, 4, 5]], 1e-06)
check('s01', lambda: torch.tile(g6, (2, 1, 2)), ['T', [2, 2, 6], 'torch.int64', [0, 1, 2, 0, 1, 2, 3, 4, 5, 3, 4, 5, 0, 1, 2, 0, 1, 2, 3, 4, 5, 3, 4, 5]], 1e-06)
check('s02', lambda: torch.hstack([g6, g6]), ['T', [2, 6], 'torch.int64', [0, 1, 2, 0, 1, 2, 3, 4, 5, 3, 4, 5]], 1e-06)
check('s03', lambda: torch.vstack([g6, g6]), ['T', [4, 3], 'torch.int64', [0, 1, 2, 3, 4, 5, 0, 1, 2, 3, 4, 5]], 1e-06)
check('s04', lambda: torch.dstack([g6, g6]), ['T', [2, 3, 2], 'torch.int64', [0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5]], 1e-06)
check('s05', lambda: torch.column_stack([torch.tensor([1, 2]), torch.tensor([3, 4])]), ['T', [2, 2], 'torch.int64', [1, 3, 2, 4]], 1e-06)
check('s06', lambda: torch.tensor_split(torch.arange(7), 3), [['T', [3], 'torch.int64', [0, 1, 2]], ['T', [2], 'torch.int64', [3, 4]], ['T', [2], 'torch.int64', [5, 6]]], 1e-06)
check('s07', lambda: torch.tensor_split(torch.arange(7), [1, 5]), [['T', [1], 'torch.int64', [0]], ['T', [4], 'torch.int64', [1, 2, 3, 4]], ['T', [2], 'torch.int64', [5, 6]]], 1e-06)
check('s08', lambda: torch.tensor_split(g6, 2, dim=1), [['T', [2, 2], 'torch.int64', [0, 1, 3, 4]], ['T', [2, 1], 'torch.int64', [2, 5]]], 1e-06)
check('s09', lambda: torch.broadcast_tensors(torch.ones(2, 1), torch.zeros(3)), [['T', [2, 3], 'torch.float32', [1.0, 1.0, 1.0, 1.0, 1.0, 1.0]], ['T', [2, 3], 'torch.float32', [0.0, 0.0, 0.0, 0.0, 0.0, 0.0]]], 1e-06)
check('s10', lambda: torch.broadcast_to(torch.tensor([1, 2]), (2, 2)), ['T', [2, 2], 'torch.int64', [1, 2, 1, 2]], 1e-06)
check('s11', lambda: torch.broadcast_shapes((2, 1), (3,)), [2, 3], 1e-06)
check('s12', lambda: torch.meshgrid(torch.tensor([1, 2]), torch.tensor([3, 4, 5]), indexing='ij'), [['T', [2, 3], 'torch.int64', [1, 1, 1, 2, 2, 2]], ['T', [2, 3], 'torch.int64', [3, 4, 5, 3, 4, 5]]], 1e-06)
check('s13', lambda: torch.meshgrid(torch.tensor([1, 2]), torch.tensor([3, 4, 5]), indexing='xy'), [['T', [3, 2], 'torch.int64', [1, 2, 1, 2, 1, 2]], ['T', [3, 2], 'torch.int64', [3, 3, 4, 4, 5, 5]]], 1e-06)
check('s14', lambda: torch.rot90(g6), ['T', [3, 2], 'torch.int64', [2, 5, 1, 4, 0, 3]], 1e-06)
check('s15', lambda: torch.rot90(g6, 2), ['T', [2, 3], 'torch.int64', [5, 4, 3, 2, 1, 0]], 1e-06)
check('s16', lambda: torch.rot90(g6, -1), ['T', [3, 2], 'torch.int64', [3, 0, 4, 1, 5, 2]], 1e-06)
check('s17', lambda: torch.diagonal(torch.arange(9).reshape(3, 3), 1), ['T', [2], 'torch.int64', [1, 5]], 1e-06)
check('s18', lambda: torch.diagonal(torch.arange(24).reshape(2, 3, 4), 0, 1, 2), ['T', [2, 3], 'torch.int64', [0, 5, 10, 12, 17, 22]], 1e-06)
check('s19', lambda: torch.diag(torch.tensor([1, 2]), -1), ['T', [3, 3], 'torch.int64', [0, 0, 0, 1, 0, 0, 0, 2, 0]], 1e-06)
check('s20', lambda: torch.diag_embed(torch.tensor([[1, 2], [3, 4]])), ['T', [2, 2, 2], 'torch.int64', [1, 0, 0, 2, 3, 0, 0, 4]], 1e-06)
check('s21', lambda: torch.ones(2, 1, 3, 1).squeeze((1, 3)).shape, [2, 3], 1e-06)
check('s22', lambda: torch.cartesian_prod(torch.tensor([1, 2]), torch.tensor([3, 4])), ['T', [4, 2], 'torch.int64', [1, 3, 1, 4, 2, 3, 2, 4]], 1e-06)
check('s23', lambda: torch.triu_indices(3, 3), ['T', [2, 6], 'torch.int64', [0, 0, 0, 1, 1, 2, 0, 1, 2, 1, 2, 2]], 1e-06)
check('s24', lambda: torch.tril_indices(3, 4, 1), ['T', [2, 9], 'torch.int64', [0, 0, 1, 1, 1, 2, 2, 2, 2, 0, 1, 0, 1, 2, 0, 1, 2, 3]], 1e-06)
check('s25', lambda: torch.combinations(torch.tensor([1, 2, 3])), ['T', [3, 2], 'torch.int64', [1, 2, 1, 3, 2, 3]], 1e-06)
check('s26', lambda: grad_of(lambda t: t.flip(0), [1.0, 2.0, 3.0]), [['T', [3], 'torch.float32', [3.0, 2.0, 1.0]]], 1e-06)
check('s27', lambda: grad_of(lambda t: torch.rot90(t), [[1.0, 2.0], [3.0, 4.0]]), [['T', [2, 2], 'torch.float32', [3.0, 1.0, 4.0, 2.0]]], 1e-06)
check('s28', lambda: grad_of(lambda t: torch.diagonal(t), [[1.0, 2.0], [3.0, 4.0]]), [['T', [2, 2], 'torch.float32', [1.0, 0.0, 0.0, 2.0]]], 1e-06)
check('s29', lambda: grad_of(lambda t: torch.diag_embed(t), [1.0, 2.0]), [['T', [2], 'torch.float32', [1.0, 4.0]]], 1e-06)
check('s30', lambda: grad_of(lambda t: t[:, [0, 0, 1]], [[1.0, 2.0], [3.0, 4.0]]), [['T', [2, 2], 'torch.float32', [3.0, 3.0, 9.0, 6.0]]], 1e-06)
"#);
    assert_all_true(&out, 31);
}

/// mv/addmm/baddbmm/tensordot/kron/cross/trace/outer/cdist/dot, with gradients.
#[test]
fn linear_algebra_ops() {
    let out = run(r#"
A = [[1.0, -2.0, 3.0], [4.0, 5.0, -6.0]]
a = torch.tensor(A); b = torch.tensor([[0.5, 1.5, -1.0], [2.0, -0.5, 1.0]])
v = torch.tensor([3.0, 1.0, 2.0]); w = torch.tensor([1.0, 0.0, -1.0])
m = torch.tensor([[1.0, 2.0], [3.0, 4.0], [5.0, 6.0]])
def grad_of(f, *vals):
    xs = [torch.tensor(val, requires_grad=True) for val in vals]
    out = f(*xs)
    out = out if out.dim() == 0 else (out * torch.arange(1, out.numel() + 1, dtype=out.dtype).reshape(out.shape)).sum()
    out.backward()
    return [x.grad for x in xs]
def inplace(name, *args, **kwargs):
    t = torch.tensor([1.5, 2.25, 4.0])
    getattr(t, name)(*args, **kwargs)
    return t
p3 = torch.arange(24.0).reshape(2, 3, 4); q3 = torch.arange(12.0).reshape(4, 3)
check('l00', lambda: torch.mv(m.t(), torch.tensor([1.0, 0.0, -1.0])), ['T', [2], 'torch.float32', [-4.0, -4.0]], 1e-05)
check('l01', lambda: torch.addmm(torch.ones(2, 2), a, m, beta=0.5, alpha=2), ['T', [2, 2], 'torch.float32', [20.5, 24.5, -21.5, -15.5]], 1e-05)
check('l02', lambda: torch.addmm(torch.tensor([1.0, 2.0]), a, m), ['T', [2, 2], 'torch.float32', [11.0, 14.0, -10.0, -6.0]], 1e-05)
check('l03', lambda: torch.baddbmm(torch.ones(2, 2, 2), torch.ones(2, 2, 3), torch.ones(2, 3, 2), alpha=0.5), ['T', [2, 2, 2], 'torch.float32', [2.5, 2.5, 2.5, 2.5, 2.5, 2.5, 2.5, 2.5]], 1e-05)
check('l04', lambda: torch.tensordot(p3, q3, dims=([2, 1], [0, 1])), ['T', [2], 'torch.float32', [440.0, 1232.0]], 1e-05)
check('l05', lambda: torch.tensordot(torch.ones(2, 3), torch.ones(3, 4), dims=1), ['T', [2, 4], 'torch.float32', [3.0, 3.0, 3.0, 3.0, 3.0, 3.0, 3.0, 3.0]], 1e-05)
check('l06', lambda: torch.kron(torch.tensor([[1, 2], [3, 4]]), torch.tensor([[0, 1], [1, 0]])), ['T', [4, 4], 'torch.int64', [0, 1, 0, 2, 1, 0, 2, 0, 0, 3, 0, 4, 3, 0, 4, 0]], 1e-05)
check('l07', lambda: torch.cross(torch.tensor([[1.0, 0.0, 0.0]]), torch.tensor([[0.0, 1.0, 0.0]]), dim=1), ['T', [1, 3], 'torch.float32', [0.0, 0.0, 1.0]], 1e-05)
check('l08', lambda: torch.trace(torch.arange(9.0).reshape(3, 3)), ['T', [], 'torch.float32', [12.0]], 1e-05)
check('l09', lambda: torch.outer(torch.tensor([1, 2]), torch.tensor([3, 4])), ['T', [2, 2], 'torch.int64', [3, 4, 6, 8]], 1e-05)
check('l10', lambda: torch.cdist(torch.tensor([[0.0, 0.0], [1.0, 1.0]]), torch.tensor([[1.0, 0.0], [3.0, 4.0], [0.0, 0.0]])), ['T', [2, 3], 'torch.float32', [1.0, 5.0, 0.0, 1.0, 3.605551242828369, 1.4142135381698608]], 1e-05)
check('l11', lambda: torch.cdist(torch.tensor([[0.0, 0.0], [1.0, 1.0]]), torch.tensor([[1.0, 0.0]]), p=1), ['T', [2, 1], 'torch.float32', [1.0, 1.0]], 1e-05)
check('l12', lambda: torch.dot(torch.tensor([1, 2]), torch.tensor([3, 4])), ['T', [], 'torch.int64', [11]], 1e-05)
check('l13', lambda: grad_of(lambda x_, y_: torch.cdist(x_, y_), [[0.0, 0.0], [1.0, 1.0]], [[1.0, 0.0], [0.0, 0.0]]), [['T', [2, 2], 'torch.float32', [-1.0, 0.0, 2.8284270763397217, 5.828427314758301]], ['T', [2, 2], 'torch.float32', [1.0, -3.0, -2.8284270763397217, -2.8284270763397217]]], 1e-05)
check('l14', lambda: grad_of(lambda x_, y_: torch.kron(x_, y_), [1.0, 2.0], [3.0, 4.0]), [['T', [2], 'torch.float32', [11.0, 25.0]], ['T', [2], 'torch.float32', [7.0, 10.0]]], 1e-05)
check('l15', lambda: grad_of(lambda x_, y_: torch.cross(x_, y_, dim=0), [1.0, 2.0, 3.0], [4.0, 5.0, 6.0]), [['T', [3], 'torch.float32', [3.0, -6.0, 3.0]], ['T', [3], 'torch.float32', [0.0, 0.0, 0.0]]], 1e-05)
check('l16', lambda: grad_of(lambda i_, x_, y_: torch.addmm(i_, x_, y_, beta=2, alpha=3), [1.0], [[1.0, 2.0]], [[3.0], [4.0]]), [['T', [1], 'torch.float32', [2.0]], ['T', [1, 2], 'torch.float32', [9.0, 12.0]], ['T', [2, 1], 'torch.float32', [3.0, 6.0]]], 1e-05)
check('l17', lambda: grad_of(lambda x_, y_: torch.tensordot(x_, y_, dims=1), [[1.0, 2.0]], [[3.0], [4.0]]), [['T', [1, 2], 'torch.float32', [3.0, 4.0]], ['T', [2, 1], 'torch.float32', [1.0, 2.0]]], 1e-05)
"#);
    assert_all_true(&out, 18);
}
