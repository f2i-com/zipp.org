"""Writes crates/zipp-vm/tests/python_torch_core.rs: every expected value is
computed here by CPython 3.11 + PyTorch 2.11 and written into the Rust test
as a literal; the embedded Python program recomputes each case under Zipp.

    python -P crates/zipp-vm/tests/fixtures/torch_core/gen_tests.py

(`-P` keeps this directory off sys.path.) The checkpoints it reads were
saved by PyTorch 2.11: ../torch_optim/torch_globals.pt, and noncontig.pt
here, which `--fixtures` rewrites (make_noncontig below)."""
import math
import sys
import textwrap
import torch

assert torch.__version__.startswith("2.11"), torch.__version__

NORM = r'''
import math
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
'''

exec(NORM)


def lit(v):
    if isinstance(v, float):
        if math.isnan(v):
            return 'float("nan")'
        if math.isinf(v):
            return 'float("inf")' if v > 0 else '-float("inf")'
        return repr(v)
    if isinstance(v, list):
        return "[" + ", ".join(lit(x) for x in v) + "]"
    return repr(v)


TESTS = []


def test(name, doc, setup, cases):
    TESTS.append((name, doc, textwrap.dedent(setup).strip("\n"), cases))


# ---- bug 1: in-place writes through aliases -----------------------------------------------
test("inplace_ops_write_through_data_detach_and_view_aliases",
     "In-place arithmetic writes into the tensor's storage, so `.data`, `detach()` and reshape views see it (manual SGD via `w.data.add_`), with PyTorch's dtype rule for the result.",
     '''
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
     ''',
     [("aliases", "aliases()", 1e-6), ("manual_sgd", "manual_sgd()", 1e-6), ("iops", "iops()", 1e-6),
      ("int_iadd_float", "err(lambda: torch.tensor([1, 2]).__iadd__(1.5), \"can't be cast\")", 0),
      ("f32_iadd_f64", "(lambda a: (a.__iadd__(torch.tensor([1., 2.], dtype=torch.float64)), a)[1])(torch.tensor([1., 2.]))", 0),
      ("view_nonleaf", "view_nonleaf()", 1e-6), ("nonleaf_ops", "nonleaf_ops()", 1e-4)])

# ---- bug 2: saved-tensor version checks; overwrites of leaves ------------------------------
test("inplace_writes_to_saved_tensors_and_leaves_raise",
     "Writing in place into a tensor backward still needs raises PyTorch's version error at backward; zero_/fill_/copy_/uniform_/setitem on a leaf that requires grad raise outside no_grad; `.data` writes stay invisible to the check.",
     '''
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
     ''',
     [(n, "err(lambda: saved(%r), 'modified by an inplace operation')" % n, 0) for n in ["mulself", "exp_out", "mulsave", "zero_saved", "setitem_saved", "matmul_saved", "nograd_leaf"]]
     + [("data_write", "saved('data_write')", 1e-6), ("add_unsaved", "saved('add_unsaved')", 1e-6), ("relu_input", "saved('relu_input')", 1e-6)]
     + [("leaf_" + n, "leaf(%s)" % f, 0) for n, f in [("zero_", "lambda t: t.zero_()"), ("fill_", "lambda t: t.fill_(2)"), ("copy_", "lambda t: t.copy_(torch.ones(2))"), ("uniform_", "lambda t: t.uniform_()"), ("setitem", "lambda t: t.__setitem__(0, 5.0)"), ("add_", "lambda t: t.add_(1)")]]
     + [("view_of_leaf", "err(lambda: torch.ones(4, requires_grad=True).view(2, 2).mul_(2), 'view of a leaf Variable')", 0),
        ("leaf_under_no_grad", "(lambda t: (torch.no_grad().__enter__(), t.zero_(), t.fill_(3), t.__setitem__(0, 4.0), torch.set_grad_enabled(True), t)[-1])(torch.ones(2, requires_grad=True))", 0)])

# ---- bug 3: float32 reductions accumulate in double --------------------------------------
test("float32_reductions_accumulate_in_double_precision",
     "float32 sum/mean/prod (whole and per dim) accumulate in float64 and round once: within 1e-6 of the exact value where float32 accumulation drifted to 0.100958.",
     "",
     [("mean_1e6", "torch.full((10 ** 6,), 0.1).mean()", 2e-7), ("sum_1e5", "torch.full((100000,), 0.1).sum()", 2e-7),
      ("mse", "((torch.full((1000, 1000), 0.3) - torch.full((1000, 1000), 0.2)) ** 2).mean()", 1e-6),
      ("sum_dim0", "torch.full((10000, 3), 0.1).sum(0)", 2e-7), ("sum_dim1", "torch.full((3, 10000), 0.1).sum(1)", 2e-7),
      ("mean_dims", "torch.full((20, 500, 2), 0.1).mean((0, 1))", 2e-7), ("prod", "torch.full((1000,), 1.001).prod()", 1e-5)])

# ---- bug 4: var/std argument order ----------------------------------------------------------
test("var_and_std_take_dim_unbiased_keepdim_and_correction",
     "var/std take (dim, unbiased, keepdim) positionally, a lone bool as `unbiased`, and correction=; float32 is computed in double as PyTorch does.",
     "x = torch.tensor([[1.0, 2.0, 4.0], [3.0, 5.0, 9.0]])",
     [("var_dim_unbiased", "x.var(1, False)", 1e-6), ("std_fn", "torch.std(x, 0, False)", 1e-6), ("var_bool", "x.var(False)", 1e-6),
      ("var_keepdim", "x.var(1, True, True)", 1e-6), ("std_correction", "x.std(correction=0)", 1e-6), ("var_correction2", "x.var(dim=1, correction=2)", 1e-6),
      ("var_fn_keepdim", "torch.var(x, 1, False, True)", 1e-6), ("std_mean", "torch.std_mean(x, 1)", 1e-6), ("var_mean", "torch.var_mean(x, dim=0, correction=0)", 1e-6),
      ("std_default", "x.std(0)", 1e-6), ("var_default", "x.var()", 1e-6)])

# ---- bug 5: create_graph -----------------------------------------------------------------
test("create_graph_records_differentiable_gradients",
     "backward/autograd.grad with create_graph=True return gradients with history (a gradient penalty differentiates through them); second derivatives of the common ops match PyTorch; backward accepts retain_graph and inputs.",
     '''
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
     ''',
     [("penalty", "penalty()", 1e-5), ("backward_create", "backward_create()", 1e-5), ("backward_inputs", "backward_inputs()", 1e-6)]
     + [("d2_" + n, "second(%s)" % f, 1e-4) for n, f in [("exp", "torch.exp"), ("tanh", "torch.tanh"), ("sigmoid", "torch.sigmoid"), ("sin", "torch.sin"), ("log", "torch.log"), ("sqrt", "torch.sqrt"), ("pow3", "lambda t: t ** 3"), ("recip", "lambda t: 1 / t"), ("softmax", "lambda t: torch.softmax(t, 0)"), ("gelu", "F.gelu"), ("silu", "F.silu"), ("norm", "lambda t: t.norm() * t"), ("mm", "lambda t: (t.reshape(1, 3) @ t.reshape(3, 1)).reshape(1) * t"), ("slice", "lambda t: t[1:] ** 2"), ("index", "lambda t: t[[0, 2]] ** 3"), ("log_softmax", "lambda t: torch.log_softmax(t, 0) * t"), ("mean", "lambda t: (t - t.mean()) ** 2"), ("max_dim", "lambda t: t.reshape(1, 3).max(1).values ** 2"), ("where", "lambda t: torch.where(t > 1, t ** 2, t ** 3)"), ("cat", "lambda t: torch.cat([t, t ** 2]) ** 2"), ("cumsum", "lambda t: t.cumsum(0) ** 2")]])

# ---- bug 6: float arange -----------------------------------------------------------------
test("float_arange_has_ceil_length_and_exact_elements",
     "Float arange has ceil((end - start) / step) elements, element i = start + i * step (no accumulated rounding).",
     "",
     [("len_0_1", "torch.arange(0, 1, 0.1).shape", 0), ("values_0_09", "torch.arange(0, 0.9, 0.3)", 1e-7), ("len_0_10", "torch.arange(0, 10, 0.01).shape", 0),
      ("half_steps", "torch.arange(1, 2.5, 0.5)", 0), ("negative", "torch.arange(-1, 1, 0.25)", 0), ("last_below_end", "torch.arange(0, 1, 0.1)[-1].item() < 1", 0),
      ("int_default", "torch.arange(5)", 0), ("desc", "torch.arange(10, 0, -3)", 0), ("tensor_end", "torch.arange(torch.tensor(4))", 0), ("float64", "torch.arange(3, dtype=torch.float64)", 0)])

# ---- bug 7: uint8/bool min and max -----------------------------------------------------------
test("uint8_and_bool_min_max_start_from_the_data",
     "min/max/amax/argmin over uint8 and bool tensors (the reduction no longer starts from an Infinity a Uint8Array cannot hold).",
     "img = torch.tensor([[3, 7], [9, 200]], dtype=torch.uint8)",
     [("min", "img.min()", 0), ("min_dim", "img.min(1)", 0), ("max", "img.max()", 0), ("max_dim0", "img.max(0)", 0), ("amax", "img.amax(1)", 0),
      ("argmin", "img.argmin(1)", 0), ("bool_min", "torch.tensor([True, True]).min()", 0), ("bool_max", "torch.tensor([False, True]).max()", 0), ("bool_min_dim", "torch.tensor([True, False]).min(0)", 0)])

# ---- bug 8: zero gradients where PyTorch masks ------------------------------------------------
test("norm_std_and_pow_gradients_are_zero_not_nan_at_zero",
     "The 2-norm and p-norms of a zero vector, std of constant data, x**0 at 0 and 0**e give PyTorch's zero (masked) gradients, not NaN.",
     '''
     def grad(f, *vals):
         xs = [torch.tensor(v, requires_grad=True) for v in vals]
         f(*xs).sum().backward()
         return [x.grad for x in xs]
     ''',
     [("norm0", "grad(lambda x: x.norm(), [0.0, 0.0])", 1e-6), ("std_const", "grad(lambda y: y.std(), [1.0, 1.0])", 1e-6),
      ("pow0_at0", "grad(lambda z: z ** 0, [0.0, 2.0])", 1e-6), ("exponent_at_base0", "grad(lambda e: torch.tensor([0.0, 2.0, 0.0]) ** e, [1.0, 2.0, -1.0])", 1e-5),
      ("scalar_base0", "grad(lambda e: 0 ** e, [1.0, 0.0, -1.0])", 1e-6), ("scalar_base2", "grad(lambda e: 2 ** e, [1.0, 0.0, -1.0])", 1e-5),
      ("tensor_exp0", "grad(lambda z: z ** torch.tensor([0.0, 0.0]), [0.0, 2.0])", 1e-6), ("norm_rows", "grad(lambda m: m.norm(dim=1), [[0.0, 0.0], [3.0, 4.0]])", 1e-6),
      ("norm_p1", "grad(lambda x: x.norm(p=1), [0.0, 0.0])", 1e-6), ("norm_p3", "grad(lambda x: x.norm(p=3), [0.0, 0.0])", 1e-6), ("norm_inf", "grad(lambda x: x.norm(p=float('inf')), [0.0, 1.0])", 1e-6),
      ("norm_p3_values", "grad(lambda x: x.norm(p=3), [1.0, -2.0, 3.0])", 1e-5), ("std_rows", "grad(lambda x: x.std(1), [[1.0, 1.0], [2.0, 3.0]])", 1e-5)])

# ---- bug 9: .grad owns its storage ---------------------------------------------------------
test("grad_buffers_do_not_share_storage",
     "A leaf's .grad is its own tensor on first assignment (not shared with another leaf or the caller's `gradient`), and later backwards accumulate into it in place.",
     '''
     def shared():
         a = torch.tensor([1.0, 2.0], requires_grad=True); b = torch.tensor([3.0, 4.0], requires_grad=True)
         (a + b).sum().backward()
         a.grad.zero_()
         g = torch.tensor([1.0, 1.0]); c = torch.tensor([1.0, 2.0], requires_grad=True)
         (c + 0).backward(g); c.grad.zero_()
         d = torch.tensor([1.0, 2.0], requires_grad=True)
         (d * 2).sum().backward(); first = d.grad; (d * 3).sum().backward()
         return b.grad, g, first, first is d.grad
     ''',
     [("shared", "shared()", 0)])

# ---- bug 10: integer bitwise ops --------------------------------------------------------------
test("integer_bitwise_operators_are_bitwise",
     "&, |, ^, ~, << and >> on integer tensors are bitwise (logical on bool); float operands are refused.",
     "a, b = torch.tensor([6, 5]), torch.tensor([3, 1])",
     [("and", "a & b", 0), ("or_scalar", "a | 1", 0), ("invert", "~torch.tensor([6, 0])", 0), ("xor", "a ^ b", 0), ("bool_and", "torch.tensor([True, False]) & torch.tensor([True, True])", 0),
      ("bool_invert", "~torch.tensor([True, False])", 0), ("uint8_invert", "~torch.tensor([6, 0], dtype=torch.uint8)", 0), ("wide", "torch.tensor([1 << 40]) & torch.tensor([(1 << 40) | 5])", 0),
      ("rshift", "torch.tensor([-8]) >> 1", 0), ("lshift", "torch.tensor([3]) << 2", 0), ("int32", "torch.tensor([6, 5], dtype=torch.int32) & 3", 0), ("rand", "3 & a", 0),
      ("float_refused", "err(lambda: torch.tensor([1.0]) & torch.tensor([1.0]), 'not implemented')", 0)])

# ---- bug 11: bool lists index as masks -------------------------------------------------------
test("bool_lists_index_and_assign_as_masks",
     "A list of Python bools indexes (and assigns) as a mask, like a bool tensor.",
     '''
     m = torch.arange(12).reshape(3, 4)
     def assign():
         m2 = torch.zeros(3, 4); m2[[True, False, True]] = 1.0
         m3 = torch.arange(12.0).reshape(3, 4); m3[torch.tensor([False, True, False])] = -1
         return m2, m3
     ''',
     [("rows", "m[[True, False, True]]", 0), ("cols", "m[:, [True, False, False, True]]", 0), ("assign", "assign()", 0), ("mask2d", "m[m > 8]", 0), ("scalar_mask", "m[torch.tensor(True)].shape", 0)])

# ---- bug 12: NaN ---------------------------------------------------------------------------
test("nan_propagates_through_relu_argmax_and_sort",
     "relu(nan) is nan (its gradient passes, as threshold_backward), argmax/argmin/max(dim) answer the first NaN, and sort/topk/argsort treat NaN as the largest value.",
     '''
     x = torch.tensor([1.0, float("nan"), -1.0])
     def relu_grad():
         n = torch.tensor([float("nan"), 2.0, -1.0], requires_grad=True)
         torch.relu(n).sum().backward()
         return n.grad
     ''',
     [("relu", "torch.relu(x)", 0), ("relu_grad", "relu_grad()", 0), ("argmax", "x.argmax()", 0), ("argmin", "x.argmin()", 0), ("max", "x.max()", 0),
      ("max_dim", "torch.max(x, 0)", 0), ("min_dim", "torch.min(x, 0)", 0), ("argmax_rows", "torch.tensor([[1.0, float('nan')], [2.0, 0.0]]).argmax(1)", 0),
      ("sort", "torch.sort(torch.tensor([3.0, 1.0, float('nan'), 2.5, -2.5, 0.5, -0.5, 1.5]))", 0), ("sort_desc", "torch.sort(torch.tensor([3.0, float('nan'), 1.0, float('nan'), 2.0]), descending=True)", 0),
      ("topk", "torch.topk(torch.tensor([3.0, float('nan'), 1.0, 2.0]), 2)", 0), ("argsort", "torch.argsort(torch.tensor([2.0, float('nan'), 1.0]))", 0)])

# ---- bug 13: integer powers --------------------------------------------------------------------
test("integer_powers_stay_integer",
     "An integer tensor raised to an integer power stays integer (negative exponents truncate: 2**-1 is 0), an int32 exponent keeps int32, and a negative Python-int exponent is refused.",
     "",
     [("neg_exp", "2 ** torch.tensor([-1, -7, 0, 3])", 0), ("bases", "torch.tensor([1, -1, 0, 2, -2]) ** torch.tensor([-3, -3, -1, -1, -1])", 0),
      ("int32", "torch.tensor([2], dtype=torch.int32) ** torch.tensor([3], dtype=torch.int32)", 0), ("square", "torch.tensor([2]) ** 2", 0),
      ("float_exp", "torch.tensor([2]) ** 2.0", 0), ("tensor_float_exp", "torch.tensor([2]) ** torch.tensor(0.5)", 1e-6), ("uint8", "torch.tensor([3], dtype=torch.uint8) ** 2", 0),
      ("float_base", "2.5 ** torch.tensor([2])", 0), ("neg_scalar", "err(lambda: torch.tensor([2]) ** -1, 'negative integer powers')", 0)])

# ---- bug 14: type promotion ---------------------------------------------------------------------
PROMO = [
    "x32 * torch.tensor(2.0, dtype=torch.float64)", "xi32 + 1", "xi32 + 1.5", "xi32 * torch.tensor(2)", "xi32 * torch.tensor(2.0)", "xi + torch.tensor(1.5, dtype=torch.float64)",
    "xb + 1", "xb + 1.5", "xb + xb", "xb * torch.tensor(3)", "xu8 + 10", "xu8 - 251", "xu8 * 2", "xu8 + xi32", "xi32 + xi", "xi / 2", "xi32 / xi32", "x32 + xi",
    "x32.double() + x32", "torch.tensor(1) + torch.tensor(2.5)", "torch.tensor(1, dtype=torch.int32) + torch.tensor(2)", "torch.where(cond, xi32, 5)", "torch.where(cond, xi32, 5.0)",
    "torch.where(cond, x32, torch.tensor(1.0, dtype=torch.float64))", "torch.where(cond, 1, 0)", "torch.where(cond, 1.0, 0)", "xi32 == 2", "xi32 < 1.5", "x32 == 1.5",
    "xi32 // 2", "xi32 % 2", "xi // 2.0", "xi.clamp(0.5, 1.5)", "torch.maximum(xi32, xi)", "torch.tensor([1, 2], dtype=torch.uint8) * torch.tensor([3.0])", "xb.sum()",
    "xi32.sum()", "xu8.sum()", "xi32.cumsum(0)", "torch.tensor([True, True]).cumsum(0)", "xi32.prod()", "torch.tensor([0.1]) == 0.1",
]
test("scalars_and_0d_tensors_promote_by_category",
     "PyTorch's result_type: a Python scalar or 0-d tensor only raises the result's category (float32 * 0-d float64 is float32, int32 + 1 is int32, where(cond, int32, 5) is int32); uint8 arithmetic wraps.",
     '''
     x32 = torch.tensor([1.5, 2.0]); xi32 = torch.tensor([1, 2], dtype=torch.int32); xu8 = torch.tensor([250], dtype=torch.uint8)
     xb = torch.tensor([True, False]); xi = torch.tensor([1, 2]); cond = torch.tensor([True, False])
     ''',
     [("p%02d" % i, e, 1e-6) for i, e in enumerate(PROMO)])

# ---- bug 15: grad-mode contexts -----------------------------------------------------------------
test("grad_mode_contexts_and_decorators",
     "set_grad_enabled works as a function, a context manager and a decorator; inference_mode and bare @torch.no_grad work; leaving a block restores the mode.",
     '''
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
     ''',
     [("modes", "modes()", 0)])

# ---- bug 16: 1-d @ 1-d backward ---------------------------------------------------------------------
test("vector_matmul_gradients",
     "Gradients of 1-d @ 1-d (a 0-d result), matrix @ vector, vector @ matrix and batched @ vector.",
     '''
     def grads():
         a = torch.tensor([1.0, 2.0], requires_grad=True); b = torch.tensor([3.0, 4.0], requires_grad=True)
         (a @ b).backward()
         m = torch.tensor([[1.0, 2.0], [3.0, 4.0]], requires_grad=True); v = torch.tensor([1.0, -1.0], requires_grad=True)
         (m @ v).sum().backward()
         v2 = torch.tensor([2.0, 1.0], requires_grad=True); (v2 @ m).sum().backward()
         bt = torch.ones(3, 2, 2, requires_grad=True); (bt @ v).sum().backward()
         return a.grad, b.grad, m.grad, v.grad, v2.grad, bt.grad
     ''',
     [("grads", "grads()", 1e-6)])

# ---- bug 17: __format__ --------------------------------------------------------------------------
test("zero_dim_tensors_take_format_specs",
     "f'{loss:.4f}' formats a 0-d tensor's value; other tensors take only an empty spec, as PyTorch.",
     "",
     [("f3", "f'{torch.tensor(1.23456):.3f}'", 0), ("d", "f'{torch.tensor(3):d}'", 0), ("width", "f'{torch.tensor(2.5):>8.2f}'", 0), ("plain", "f'{torch.tensor([1.5])}'", 0),
      ("format_call", "'{}'.format(torch.tensor(7))", 0), ("vector_spec", "err(lambda: f'{torch.tensor([1.5]):.2f}')", 0)])

# ---- bug 18: setitem broadcast ----------------------------------------------------------------------
test("setitem_values_drop_leading_ones",
     "An assigned value broadcasts after dropping leading 1-dims (t[0] = ones(1, 4) into a (2, 4) tensor).",
     '''
     def put():
         t = torch.zeros(2, 4); t[0] = torch.ones(1, 4); t[1, :] = torch.full((1, 1, 4), 2.0)
         u = torch.zeros(3, 2); u[:, 0] = torch.tensor([[1.0, 2.0, 3.0]])
         return t, u
     ''',
     [("put", "put()", 0)])

# ---- bug 19: sort/topk return types -------------------------------------------------------------------
test("sort_and_topk_return_named_values_and_indices",
     "sort, topk (and kthvalue, median, mode, cummax, aminmax) results carry .values/.indices (or min/max) fields.",
     "",
     [("sort", "(lambda s: (s.values, s.indices))(torch.sort(torch.tensor([3.0, 1.0, 2.0])))", 0), ("topk", "(lambda s: (s.values, s.indices))(torch.tensor([3.0, 1.0, 2.0]).topk(2))", 0),
      ("method_sort", "torch.tensor([[3, 1], [0, 5]]).sort(1, descending=True).indices", 0), ("smallest", "torch.topk(torch.tensor([3.0, 1.0, 2.0]), 2, largest=False).values", 0),
      ("aminmax", "(lambda s: (s.min, s.max))(torch.aminmax(torch.tensor([[1.0, 5.0], [3.0, 0.0]]), dim=0))", 0), ("unpack", "(lambda v_i: v_i[1])(torch.sort(torch.tensor([2, 1])))", 0)])

# ---- bug 20: einsum, empty dims, 0-d dims, diag, printing ------------------------------------------------
test("einsum_empty_dims_and_zero_dim_reductions",
     "einsum with a repeated index or '...', cumsum/chunk of an empty dim, dim=0 reductions of a 0-d tensor, and diag of a vector holding inf.",
     "",
     [("einsum_trace", "torch.einsum('ii->', torch.tensor([[1.0, 2.0], [3.0, 4.0]]))", 1e-6), ("einsum_diag", "torch.einsum('ii->i', torch.tensor([[1.0, 2.0], [3.0, 4.0]]))", 0),
      ("einsum_ellipsis", "torch.einsum('...ij,...jk->...ik', torch.ones(2, 2, 3), torch.ones(2, 3, 2))", 0), ("einsum_implicit", "torch.einsum('bij,bjk', torch.ones(2, 2, 3), torch.ones(2, 3, 2))", 0),
      ("einsum_ellipsis_sum", "torch.einsum('...i->...', torch.ones(2, 3))", 0), ("cumsum_empty", "torch.cumsum(torch.zeros(0, 3), 0)", 0),
      ("chunk_empty", "[c.shape for c in torch.chunk(torch.zeros(0, 3), 2)]", 0), ("sum0d", "torch.tensor(5.0).sum(0)", 0), ("max0d", "tuple(torch.tensor(5.0).max(0))", 0),
      ("argmax0d", "torch.tensor(5).argmax(0)", 0), ("mean0d", "torch.tensor(5.0).mean(-1)", 0), ("diag_inf", "torch.diag(torch.tensor([1.0, float('inf')]))", 0),
      ("diag_offset", "torch.diag(torch.tensor([1.0, 2.0]), 1)", 0), ("diag_of_matrix", "torch.diag(torch.tensor([[1.0, 2.0], [3.0, 4.0]]), -1)", 0)])

PRINTS = [
    "torch.tensor([1.0, 2.0, 3.0])", "torch.tensor([0.5, 1.25, -3.0])", "torch.tensor([1e-5, 1.0])", "torch.tensor([1e9, 1.0])", "torch.tensor([1.0, 2000.0])",
    "torch.tensor([0.0001, 5.0])", "torch.tensor([[1.0, 22.5], [-3.0, 4.0]])", "torch.tensor([[7.0, 11.0], [9.0, 13.0]])", "torch.tensor([float('nan'), 1.0, float('inf'), -float('inf')])",
    "torch.tensor(3.5)", "torch.tensor(1e-8)", "torch.tensor([1, -20, 300])", "torch.tensor([True, False])", "torch.tensor([1.5, 2.5], dtype=torch.float64)",
    "torch.tensor([1, 2], dtype=torch.int32)", "torch.zeros(0)", "torch.zeros(2, 0, dtype=torch.int64)", "torch.arange(2000)", "torch.arange(2000.0) / 7",
    "torch.arange(1200).reshape(40, 30)", "torch.arange(24.0).reshape(2, 3, 4)", "torch.arange(30.0)", "torch.tensor([1.0, 2.0], requires_grad=True) * 2",
    "torch.arange(3000.0).reshape(3, 1000)", "torch.tensor([[1e-10, 1.0], [1.0, 2.0]])",
]
test("printing_follows_pytorch_formatting_and_summarises_large_tensors",
     "repr/str follow torch/_tensor_str.py: one layout per tensor, scientific notation for tiny or wide-ranging values, column padding, '...' summaries beyond 1000 elements, set_printoptions; str(device) is 'cpu'.",
     '''
     def opts():
         torch.set_printoptions(precision=2)
         a = str(torch.tensor([1.23456, 2.5]))
         torch.set_printoptions(precision=4, sci_mode=False)
         b = str(torch.tensor([1e-6, 1.0]))
         torch.set_printoptions(sci_mode=None, threshold=5, edgeitems=2)
         c = str(torch.arange(10))
         torch.set_printoptions(profile="default")
         return a, b, c, str(torch.arange(10))
     ''',
     [("r%02d" % i, "repr(%s)" % e, 0) for i, e in enumerate(PRINTS)]
     + [("options", "opts()", 0), ("device", "(str(torch.device('cpu')), repr(torch.device('cpu')), torch.device('cpu') == 'cpu')", 0)])

# ---- bug 21: add/sub alpha ------------------------------------------------------------------------------
test("add_and_sub_take_alpha",
     "torch.add/sub scale only when alpha != 1: a compiled training step using torch.add(graph, parameter) sends the gradient to the parameter, not to an eager copy (values from PyTorch's eager step).",
     '''
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
     ''',
     [("compiled_add", "captured('add')", 1e-5), ("compiled_sub", "captured('sub')", 1e-5), ("plain", "torch.add(a, b)", 0), ("alpha2", "torch.add(a, b, alpha=2)", 0), ("sub_half", "torch.sub(a, b, alpha=0.5)", 0), ("scalar_alpha", "a.add(1, alpha=3)", 0),
      ("rsub", "torch.rsub(a, 5)", 0), ("inplace_alpha", "(lambda t: (t.add_(b, alpha=-0.5), t)[1])(a.clone())", 0)])

test("torch_functions_dispatch_graph_tensors",
     "torch.sqrt/rsqrt/abs/square/div/pow/reshape/flatten/unsqueeze/squeeze/permute/transpose/t and F.silu called on a compiled step's graph tensors record graph operations: the compiled training step equals PyTorch's eager step.",
     '''
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
     ''',
     [(n, "run(%s)" % f, 1e-5) for n, f in [
         ("sqrt", "lambda m, x, y: F.cross_entropy(torch.sqrt(m(x).square() + 1.0), y)"),
         ("rsqrt_abs", "lambda m, x, y: F.cross_entropy(torch.rsqrt(torch.abs(m(x)) + 1.0), y)"),
         ("div_pow", "lambda m, x, y: F.cross_entropy(torch.div(torch.pow(m(x), 2), 2.0), y)"),
         ("shapes", "lambda m, x, y: F.cross_entropy(torch.t(torch.transpose(torch.reshape(torch.flatten(torch.unsqueeze(m(x), 0)), (2, 2)), 0, 1)), y)"),
         ("permute_squeeze", "lambda m, x, y: F.cross_entropy(torch.squeeze(torch.permute(m(x).unsqueeze(0), (0, 1, 2)), 0), y)"),
         ("silu", "lambda m, x, y: F.cross_entropy(F.silu(m(x)), y)")]])

# ---- bug 22: 0-d tensor slice bounds ------------------------------------------------------------------
test("zero_dim_tensors_bound_slices",
     "Slice bounds may be 0-d integer tensors (`data[i:i + block]` with `i` from torch.randint), in reads and writes.",
     '''
     d = torch.arange(20)
     def write():
         e = torch.zeros(10); e[torch.tensor(1):torch.tensor(3)] = 1.0
         return e
     ''',
     [("loop", "[d[i:i + 4] for i in torch.tensor([3, 7])]", 0), ("step", "d[torch.tensor(2):torch.tensor(10):torch.tensor(3)]", 0), ("write", "write()", 0)])

# ---- bug 23: strided checkpoints ------------------------------------------------------------------------
test("pytorch_checkpoints_with_strided_tensors_load",
     "torch.load gathers a checkpoint tensor through its storage offset and strides (a transpose, a column, a stepped slice, an expand with stride 0, an offset block, a permute) and resolves PyTorch's weights-only globals (Size, device, Parameter), all saved by PyTorch 2.11; Parameters, dtypes and devices saved here load back as themselves.",
     '''
     from torch import nn
     def described(d):
         return [[k, type(v).__name__, v if isinstance(v, torch.Tensor) else str(v), getattr(v, "requires_grad", None)] for k, v in sorted(d.items())]
     def roundtrip():
         torch.save({"p": nn.Parameter(torch.tensor([1.5, -2.0])), "q": nn.Parameter(torch.ones(2), requires_grad=False), "dt": torch.float64, "b": torch.bool, "dev": torch.device("cpu"), "t": torch.arange(3)}, "roundtrip.pt")
         return described(torch.load("roundtrip.pt"))
     ''',
     [("noncontig", "torch.load('noncontig.pt')", 0), ("torch_globals", "described(torch.load('torch_globals.pt'))", 0), ("roundtrip", "roundtrip()", 0)])

# ---- bug 24: generator state ------------------------------------------------------------------------------
test("generator_state_captures_the_stream_position",
     "Generator.get_state/set_state and torch.get/set_rng_state capture the stream position (not only the seed); torch.seed reseeds; randperm (forward Fisher-Yates), randint (one word below a 2**28 range, random64 above) and random64 follow PyTorch's MT19937 stream.",
     '''
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
     ''',
     [("state", "state()", 0), ("randint_ranges", "ranges()", 0), ("random64", "seed64()", 0),
      ("randperm_stream", "(lambda g: (torch.randperm(10, generator=g), torch.randint(0, 10, (5,), generator=g), torch.randperm(7, generator=g), torch.randint(0, 1000, (3,), generator=g)))(torch.Generator().manual_seed(0))", 0)])

# ---- bug 25: torch.autograd, grad_fn, hooks ---------------------------------------------------------------
test("autograd_module_grad_fn_and_hooks",
     "torch.autograd is the module (Function, grad, backward); Tensor.grad_fn names the recorded op; register_hook sees and may replace a gradient; Function supports several outputs, needs_input_grad and version checks on saved tensors.",
     '''
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
     ''',
     [("module", "module()", 1e-6), ("names", "names()", 0), ("hooks", "hooks()", 0), ("saved_version", "saved_version()", 0), ("grad_api", "grad_api()", 1e-6)])

# ---- APIs --------------------------------------------------------------------------------------------------
API_SETUP = '''
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
'''
API_TENSOR = [
    "a.add(b)", "a.add(b, alpha=2)", "a.sub(b, alpha=3)", "a.mul(b)", "a.div(b)", "a.div(b, rounding_mode='floor')", "a.div(b, rounding_mode='trunc')", "torch.tensor([7, -7]).div(2, rounding_mode='trunc')",
    "torch.tensor([7, -7]).div(2, rounding_mode='floor')", "torch.tensor([7, -7]).div(2)", "a.matmul(m)", "a.mm(m)", "torch.ones(2, 2, 3).bmm(torch.ones(2, 3, 1))", "v.dot(w)", "a.t()",
    "(lambda x: (x.t_(), x)[1])(torch.tensor([[1.0, 2.0, 3.0]]))", "a.flip(0)", "a.flip(0, 1)", "a.fliplr()", "a.flipud()", "a.swapaxes(0, 1)", "torch.ones(2, 3, 4).movedim(0, -1).shape",
    "torch.ones(2, 3, 4).movedim((0, 1), (2, 0)).shape", "torch.arange(12).unflatten(0, (3, 4))", "torch.tensor([1, 2]).type_as(a)", "a.masked_fill(a > 2, 0.0)", "a.masked_fill(torch.tensor([True, False, True]), -1)",
    "torch.tensor([1, 2]).masked_fill(torch.tensor([True, False]), 3.7)", "a.new_zeros(2)", "a.new_ones((1, 2))", "a.new_full((2,), 5)", "a.new_tensor([1, 2])", "a.clamp_min(0)", "a.clamp_max(0)",
    "a.clamp(min=torch.tensor([0.0, 0.0, 0.0]), max=torch.tensor([2.0, 4.0, 1.0]))", "torch.clamp(a, max=torch.tensor(1.0))", "grad_of(lambda x, lo, hi: torch.clamp(x, lo, hi), [1.0, -2.0, 3.0, 0.5], [0.0, 0.0, 0.0, 0.5], [2.0, 1.0, 2.0, 1.0])",
]
API_INPLACE = ["exp_", "log_", "sqrt_", "neg_", "abs_", "sigmoid_", "tanh_", "relu_", "floor_", "round_", "ceil_", "trunc_", "frac_", "reciprocal_", "sin_", "square_"]
API_INPLACE_ARGS = ["inplace('pow_', 2)", "inplace('addcmul_', torch.ones(3), torch.full((3,), 2.0), value=0.5)", "inplace('addcdiv_', torch.ones(3), torch.full((3,), 4.0), value=2)",
                    "inplace('lerp_', torch.zeros(3), 0.25)", "inplace('clamp_min_', 2.0)", "inplace('masked_fill_', torch.tensor([True, False, True]), 9.0)", "inplace('unsqueeze_', 0).shape",
                    "(lambda t: (t.bernoulli_(0.5), 0.3 < t.mean().item() < 0.7 and set(t.tolist()) <= {0.0, 1.0})[1])(torch.zeros(1000))",
                    "(lambda t: (t.exponential_(2.0), 0.3 < t.mean().item() < 0.7 and t.min().item() >= 0)[1])(torch.zeros(1000))",
                    "(lambda t: (t.random_(0, 5), t.min().item() >= 0 and t.max().item() <= 4)[1])(torch.zeros(200, dtype=torch.int64))"]
API_CREATE = [
    "torch.Tensor([1, 2, 3])", "torch.Tensor(2, 3).shape", "torch.Tensor(2, 3).dtype", "torch.FloatTensor([1.5, 2])", "torch.LongTensor([1, 2])", "torch.Tensor().shape", "torch.DoubleTensor(2).dtype",
    "torch.zeros(2).type()", "torch.zeros(2).type('torch.LongTensor').dtype", "torch.ones_like(torch.zeros(2, 3), dtype=torch.int64)", "torch.zeros_like(torch.zeros(2), requires_grad=True).requires_grad",
    "torch.ones_like(torch.zeros(2), requires_grad=True).requires_grad", "torch.randint(10, size=(3,)).shape", "torch.randint(3, 10, (2, 2)).shape", "torch.normal(0.0, 1.0, size=(2, 3)).shape",
    "torch.normal(torch.zeros(3), 1.0).shape", "torch.normal(torch.zeros(3), torch.ones(3)).shape", "torch.finfo(torch.float32).eps", "torch.finfo(torch.float32).max", "torch.finfo(torch.float32).tiny",
    "torch.finfo().eps", "torch.finfo(torch.float64).eps", "torch.iinfo(torch.int64).max", "torch.iinfo(torch.int32).min", "torch.iinfo(torch.uint8).max", "torch.logspace(0, 2, 3)",
    "torch.logspace(0, 3, 4, base=2)", "torch.tensor([torch.tensor(1.0), 2.0])",
]
API_REDUCE = [
    "x.amax(1)", "x.amin(0)", "torch.amax(x, (0, 1))", "torch.logsumexp(x, 1)", "x.logsumexp(0, keepdim=True)", "torch.median(x)", "x.median(1)", "torch.median(torch.tensor([3.0, 1.0, 2.0, 1.0]))",
    "x.cumprod(1)", "torch.nansum(torch.tensor([1.0, float('nan'), 2.0]))", "torch.nanmean(torch.tensor([1.0, float('nan'), 2.0]))", "torch.aminmax(x)", "torch.kthvalue(torch.tensor([3.0, 1.0, 2.0, 5.0]), 2)",
    "x.kthvalue(2, 1)", "torch.mode(torch.tensor([1, 2, 2, 3, 3, 1, 3, 2]))", "x.mode(1)", "torch.cummax(torch.tensor([1.0, 3.0, 3.0, 2.0, 5.0]), 0)", "torch.cummin(torch.tensor([3.0, 1.0, 1.0, 2.0]), 0)",
    "torch.logcumsumexp(torch.tensor([1.0, 2.0, 3.0]), 0)", "torch.diff(torch.tensor([1, 4, 9, 16]))", "torch.diff(x, dim=0)", "torch.diff(torch.tensor([1.0, 3.0, 6.0]), n=2)",
    "torch.diff(torch.tensor([1.0, 3.0]), prepend=torch.tensor([0.0]), append=torch.tensor([10.0]))", "torch.nanmedian(torch.tensor([1.0, float('nan'), 3.0, 2.0]))",
    "torch.unique(u)", "torch.unique(u, return_inverse=True, return_counts=True)", "torch.unique(torch.tensor([[1, 2], [1, 2], [0, 5]]), dim=0, return_counts=True)",
    "torch.bincount(torch.tensor([1, 1, 3]))", "torch.bincount(torch.tensor([1, 1, 3]), weights=torch.tensor([0.5, 1.0, 2.0]))", "torch.bincount(torch.tensor([1]), minlength=4)",
    "grad_of(lambda t: t.amax(1), [[1.0, 5.0, 5.0], [2.0, 0.0, 1.0]])", "grad_of(lambda t: torch.logsumexp(t, 1), [[1.0, 2.0], [3.0, 0.0]])", "grad_of(lambda t: t.median(), [2.0, 1.0, 3.0, 1.0])",
    "grad_of(lambda t: t.cumprod(0), [2.0, 0.0, 3.0, 4.0])", "grad_of(lambda t: torch.cummax(t, 0).values, [1.0, 3.0, 2.0, 5.0])", "grad_of(lambda t: torch.logcumsumexp(t, 0), [2.0, 1.0, 3.0])",
]
API_ELEMENT = [
    "torch.logical_and(p, q)", "torch.logical_or(p, q)", "torch.logical_xor(p, q)", "torch.logical_not(q)", "torch.eq(v, 1.0)", "torch.ne(v, w)", "torch.gt(v, 1)", "torch.ge(v, 2)", "torch.lt(v, w)", "torch.le(v, 1)",
    "torch.isinf(z)", "torch.isnan(z)", "torch.isfinite(z)", "torch.nan_to_num(z)", "torch.nan_to_num(z, nan=1.0, posinf=2.0, neginf=-2.0)",
    "torch.log1p(s)", "torch.expm1(s)", "torch.log2(s.abs())", "torch.log10(s.abs())", "torch.exp2(s)", "torch.erf(s)", "torch.erfinv(y)", "torch.tan(s)", "torch.asin(y)", "torch.acos(y)", "torch.atan(s)",
    "torch.atan2(s, torch.tensor([1.0, -1.0, 0.5, -2.0]))", "torch.sinh(s)", "torch.cosh(s)", "torch.trunc(torch.tensor([1.7, -1.7]))", "torch.frac(torch.tensor([1.7, -1.7]))", "torch.sign(s)", "torch.sgn(s)",
    "torch.rsqrt(s.abs())", "torch.reciprocal(s)", "torch.erf(torch.tensor([3.5, -4.0, 6.5, 0.0]))",
    "torch.remainder(x5, y5)", "torch.fmod(x5, y5)", "torch.floor_divide(x5, y5)", "torch.true_divide(x5, y5)", "torch.fmax(n1, n2)", "torch.fmin(n1, n2)", "torch.maximum(n1, n2)", "torch.minimum(x5, y5)",
    "torch.lerp(torch.zeros(3), torch.tensor([1.0, 2.0, 3.0]), 0.75)", "torch.lerp(torch.zeros(3), torch.ones(3), torch.tensor([0.2, 0.6, 1.0]))",
    "torch.addcmul(torch.ones(2), torch.tensor([1.0, 2.0]), torch.tensor([3.0, 4.0]), value=0.5)", "torch.addcdiv(torch.ones(2), torch.tensor([1.0, 2.0]), torch.tensor([3.0, 4.0]), value=2)",
    "err(lambda: torch.tensor([1, 2]) // torch.tensor([1, 0]))",
    "[grad_of(getattr(torch, name), [0.2, 0.5, 0.7]) for name in ['log1p', 'expm1', 'log2', 'log10', 'exp2', 'erf', 'erfinv', 'tan', 'asin', 'acos', 'atan', 'sinh', 'cosh', 'asinh', 'atanh', 'rsqrt', 'reciprocal', 'erfc']]",
    "grad_of(torch.remainder, [5.0, -5.0, 7.5], [3.0, 3.0, -2.0])", "grad_of(torch.atan2, [1.0, -2.0], [0.5, 3.0])", "grad_of(lambda s_, e: torch.lerp(s_, e, 0.3), [1.0, 2.0], [4.0, -1.0])",
    "grad_of(lambda p_, q_, r_: torch.addcdiv(p_, q_, r_, value=2), [1.0], [2.0], [4.0])", "grad_of(torch.fmax, [1.0, 3.0], [2.0, 1.0])",
]
API_INDEX = [
    "torch.zeros(3, 4).scatter(0, idx, src)", "torch.zeros(3, 4).scatter(1, torch.tensor([[1], [3], [0]]), 5.0)", "torch.scatter_add(torch.zeros(3, 4), 0, idx, src)",
    "torch.zeros(5).index_add(0, torch.tensor([0, 4, 0]), torch.tensor([1.0, 2.0, 3.0]))", "torch.zeros(2, 3).index_add(1, torch.tensor([2, 0]), torch.tensor([[1.0, 2.0], [3.0, 4.0]]))",
    "torch.zeros(2, 3).index_fill(1, torch.tensor([0, 2]), -1.0)", "torch.zeros(3, 2).index_copy(0, torch.tensor([2, 0]), torch.tensor([[1.0, 2.0], [3.0, 4.0]]))",
    "torch.arange(6).reshape(2, 3).index_select(1, torch.tensor([2, 0]))", "torch.masked_select(a, a > 1)", "torch.take(a, torch.tensor([0, 5, 2]))", "torch.take_along_dim(a, torch.tensor([[0], [2]]), 1)",
    "torch.gather(a, 1, torch.tensor([[2, 0], [1, 1]]))", "torch.zeros(2, 2).masked_scatter(torch.tensor([[True, False], [True, True]]), torch.tensor([7.0, 8.0, 9.0, 10.0]))",
    "(lambda t: (t.scatter_(1, torch.tensor([[0], [1]]), torch.ones(2, 1)), t)[1])(torch.zeros(2, 2))", "(lambda t: (t.index_add_(0, torch.tensor([1, 1]), torch.ones(2, 2)), t)[1])(torch.zeros(2, 2))",
    "torch.where(nz)", "torch.nonzero(nz)", "torch.nonzero(nz, as_tuple=True)", "torch.argwhere(nz)", "torch.searchsorted(ss, torch.tensor([3, 6, 9]))", "torch.searchsorted(ss, torch.tensor([3, 6, 9]), right=True)",
    "torch.searchsorted(ss, 4)", "torch.searchsorted(torch.tensor([[1, 3, 5], [2, 4, 6]]), torch.tensor([[3], [3]]))", "torch.bucketize(torch.tensor([[1, 6], [9, 3]]), ss)", "torch.bucketize(torch.tensor([1, 6, 9]), ss, right=True)",
    "grad_of(lambda s_, src_: s_.scatter(0, torch.tensor([[0, 1, 2], [2, 0, 0]]), src_), [[1.0, 2.0, 3.0], [4.0, 5.0, 6.0], [7.0, 8.0, 9.0]], [[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]])",
    "grad_of(lambda s_, src_: s_.scatter_add(1, torch.tensor([[0, 0], [1, 2]]), src_), [[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]], [[1.0, 2.0], [3.0, 4.0]])",
    "grad_of(lambda s_, src_: s_.index_add(0, torch.tensor([1, 1]), src_, alpha=2), [[1.0, 2.0], [3.0, 4.0]], [[1.0, 1.0], [2.0, 2.0]])",
    "grad_of(lambda s_, src_: s_.index_copy(0, torch.tensor([1]), src_), [[1.0, 2.0], [3.0, 4.0]], [[5.0, 6.0]])", "grad_of(lambda s_: torch.take(s_, torch.tensor([0, 0, 3])), [[1.0, 2.0], [3.0, 4.0]])",
    "(lambda t: (t.__setitem__((slice(None), torch.tensor([0, 2])), torch.tensor([1.0, 2.0])), t.__setitem__((0, torch.tensor([0, 2])), 5.0), t.__setitem__((1, [1, 3]), torch.tensor([7.0, 8.0])), t)[-1])(torch.zeros(3, 4))",
    "(lambda u_: (u_.__setitem__((slice(None), 1, [0, 3]), 9.0), u_.__setitem__((Ellipsis, 2), 4.0), u_)[-1])(torch.zeros(2, 3, 4))",
    "(lambda v_: (v_.__setitem__(([0, 2], slice(1, None)), torch.tensor([[1.0, 2.0], [3.0, 4.0]])), v_)[-1])(torch.zeros(4, 3))",
]
API_SHAPE = [
    "g6.tile((2,))", "torch.tile(g6, (2, 1, 2))", "torch.hstack([g6, g6])", "torch.vstack([g6, g6])", "torch.dstack([g6, g6])", "torch.column_stack([torch.tensor([1, 2]), torch.tensor([3, 4])])",
    "torch.tensor_split(torch.arange(7), 3)", "torch.tensor_split(torch.arange(7), [1, 5])", "torch.tensor_split(g6, 2, dim=1)", "torch.broadcast_tensors(torch.ones(2, 1), torch.zeros(3))",
    "torch.broadcast_to(torch.tensor([1, 2]), (2, 2))", "torch.broadcast_shapes((2, 1), (3,))", "torch.meshgrid(torch.tensor([1, 2]), torch.tensor([3, 4, 5]), indexing='ij')",
    "torch.meshgrid(torch.tensor([1, 2]), torch.tensor([3, 4, 5]), indexing='xy')", "torch.rot90(g6)", "torch.rot90(g6, 2)", "torch.rot90(g6, -1)", "torch.diagonal(torch.arange(9).reshape(3, 3), 1)",
    "torch.diagonal(torch.arange(24).reshape(2, 3, 4), 0, 1, 2)", "torch.diag(torch.tensor([1, 2]), -1)", "torch.diag_embed(torch.tensor([[1, 2], [3, 4]]))", "torch.ones(2, 1, 3, 1).squeeze((1, 3)).shape",
    "torch.cartesian_prod(torch.tensor([1, 2]), torch.tensor([3, 4]))", "torch.triu_indices(3, 3)", "torch.tril_indices(3, 4, 1)", "torch.combinations(torch.tensor([1, 2, 3]))",
    "grad_of(lambda t: t.flip(0), [1.0, 2.0, 3.0])", "grad_of(lambda t: torch.rot90(t), [[1.0, 2.0], [3.0, 4.0]])", "grad_of(lambda t: torch.diagonal(t), [[1.0, 2.0], [3.0, 4.0]])",
    "grad_of(lambda t: torch.diag_embed(t), [1.0, 2.0])", "grad_of(lambda t: t[:, [0, 0, 1]], [[1.0, 2.0], [3.0, 4.0]])",
]
API_LINALG = [
    "torch.mv(m.t(), torch.tensor([1.0, 0.0, -1.0]))", "torch.addmm(torch.ones(2, 2), a, m, beta=0.5, alpha=2)", "torch.addmm(torch.tensor([1.0, 2.0]), a, m)",
    "torch.baddbmm(torch.ones(2, 2, 2), torch.ones(2, 2, 3), torch.ones(2, 3, 2), alpha=0.5)", "torch.tensordot(p3, q3, dims=([2, 1], [0, 1]))", "torch.tensordot(torch.ones(2, 3), torch.ones(3, 4), dims=1)",
    "torch.kron(torch.tensor([[1, 2], [3, 4]]), torch.tensor([[0, 1], [1, 0]]))", "torch.cross(torch.tensor([[1.0, 0.0, 0.0]]), torch.tensor([[0.0, 1.0, 0.0]]), dim=1)",
    "torch.trace(torch.arange(9.0).reshape(3, 3))", "torch.outer(torch.tensor([1, 2]), torch.tensor([3, 4]))", "torch.cdist(torch.tensor([[0.0, 0.0], [1.0, 1.0]]), torch.tensor([[1.0, 0.0], [3.0, 4.0], [0.0, 0.0]]))",
    "torch.cdist(torch.tensor([[0.0, 0.0], [1.0, 1.0]]), torch.tensor([[1.0, 0.0]]), p=1)", "torch.dot(torch.tensor([1, 2]), torch.tensor([3, 4]))",
    "grad_of(lambda x_, y_: torch.cdist(x_, y_), [[0.0, 0.0], [1.0, 1.0]], [[1.0, 0.0], [0.0, 0.0]])", "grad_of(lambda x_, y_: torch.kron(x_, y_), [1.0, 2.0], [3.0, 4.0])",
    "grad_of(lambda x_, y_: torch.cross(x_, y_, dim=0), [1.0, 2.0, 3.0], [4.0, 5.0, 6.0])", "grad_of(lambda i_, x_, y_: torch.addmm(i_, x_, y_, beta=2, alpha=3), [1.0], [[1.0, 2.0]], [[3.0], [4.0]])",
    "grad_of(lambda x_, y_: torch.tensordot(x_, y_, dims=1), [[1.0, 2.0]], [[3.0], [4.0]])",
]
test("tensor_methods_and_constructors",
     "Tensor methods (add/sub/mul/div with alpha and rounding_mode, matmul family, flips, movedim, unflatten, type_as, masked_fill, new_*, tensor clamp bounds), the in-place family and the legacy/creation APIs.",
     API_SETUP,
     [("t%02d" % i, e, 1e-5) for i, e in enumerate(API_TENSOR)]
     + [("i_" + n, "inplace(%r)" % n, 1e-5) for n in API_INPLACE]
     + [("ia%02d" % i, e, 1e-5) for i, e in enumerate(API_INPLACE_ARGS)]
     + [("c%02d" % i, e, 1e-6) for i, e in enumerate(API_CREATE)])
test("reductions_sorting_and_counting",
     "amax/amin/logsumexp/median/cumprod/nansum/nanmean/aminmax/kthvalue/mode/cummax/cummin/logcumsumexp/diff/nanmedian/unique/bincount, with gradients.",
     API_SETUP + '''
x = torch.tensor([[1.0, 5.0, 2.0, 5.0], [3.0, -1.0, 3.0, 0.0]])
u = torch.tensor([3, 1, 2, 1, 3])
''',
     [("r%02d" % i, e, 1e-5) for i, e in enumerate(API_REDUCE)])
test("elementwise_math_logic_and_their_gradients",
     "Logical and comparison functions, isinf/isnan/isfinite/nan_to_num, the unary math family (log1p ... erfinv) and binary family (remainder, fmod, floor_divide, fmax/fmin, lerp, addcmul/addcdiv), with gradients.",
     API_SETUP + '''
p, q = torch.tensor([True, False, True]), torch.tensor([1.0, 0.0, 0.0])
z = torch.tensor([1.0, float("inf"), float("nan"), -float("inf"), 0.0])
s = torch.tensor([0.25, 0.5, -0.75, 1.5]); y = torch.tensor([0.1, 0.5, -0.9, 0.99])
x5 = torch.tensor([5.0, -5.0, 7.5, -7.5]); y5 = torch.tensor([3.0, 3.0, -2.0, -2.0])
n1 = torch.tensor([1.0, float("nan"), 3.0]); n2 = torch.tensor([float("nan"), 2.0, 1.0])
''',
     [("e%02d" % i, e, 1e-5) for i, e in enumerate(API_ELEMENT)])
test("scatter_gather_index_and_search",
     "scatter/scatter_add/index_add/index_fill/index_copy/index_select/masked_select/take/take_along_dim/gather/masked_scatter, one-argument where, nonzero(as_tuple), argwhere, searchsorted/bucketize and mixed basic+advanced assignment, with gradients.",
     API_SETUP + '''
idx = torch.tensor([[0, 1, 2, 0], [2, 0, 0, 1]])
src = torch.arange(1.0, 9.0).reshape(2, 4)
nz = torch.tensor([[0, 1], [2, 0]])
ss = torch.tensor([1, 3, 5, 7, 9])
''',
     [("x%02d" % i, e, 1e-6) for i, e in enumerate(API_INDEX)])
test("shape_construction_ops",
     "tile/hstack/vstack/dstack/column_stack/tensor_split/broadcast_tensors/broadcast_to/meshgrid(indexing)/rot90/diagonal/diag(x, k)/diag_embed/squeeze(tuple)/cartesian_prod/triu_indices/tril_indices/combinations, with gradients.",
     API_SETUP + "g6 = torch.arange(6).reshape(2, 3)\n",
     [("s%02d" % i, e, 1e-6) for i, e in enumerate(API_SHAPE)])
test("linear_algebra_ops",
     "mv/addmm/baddbmm/tensordot/kron/cross/trace/outer/cdist/dot, with gradients.",
     API_SETUP + "p3 = torch.arange(24.0).reshape(2, 3, 4); q3 = torch.arange(12.0).reshape(4, 3)\n",
     [("l%02d" % i, e, 1e-5) for i, e in enumerate(API_LINALG)])


def build(target):
    import os
    import shutil
    import tempfile
    fixtures = os.path.join(os.path.dirname(os.path.abspath(target)), "fixtures")
    work = tempfile.mkdtemp()
    shutil.copy(os.path.join(fixtures, "torch_core", "noncontig.pt"), work)
    shutil.copy(os.path.join(fixtures, "torch_optim", "torch_globals.pt"), work)
    os.chdir(work)
    rust = ['''//! The core of the bundled `torch` subset against PyTorch 2.11: in-place
//! aliasing and version checks, autograd (create_graph, hooks, grad_fn),
//! reductions, type promotion, NaN handling, indexing, printing, RNG state,
//! strided checkpoints and the wider tensor API. Every expected value was
//! computed by CPython 3.11 with PyTorch 2.11 and written here as a literal
//! by `fixtures/torch_core/gen_tests.py` (edit the cases there and rerun it);
//! each Python program checks Zipp's result against it and prints
//! `label True`.
#![cfg(feature = "python")]
use zipp_vm::frontend::compile_python_program;

const NORM: &str = r#"''' + NORM.strip("\n") + '''
"#;

fn run(body: &str) -> Vec<String> {
    let source = format!("{NORM}\\n{body}");
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
''']
    for name, doc, setup, cases in TESTS:
        ns = {}
        exec(NORM, ns)
        exec(setup, ns)
        lines = []
        for label, expr, tol in cases:
            try:
                want = ns["norm"](eval(expr, ns))
            except Exception as e:
                want = ["E", type(e).__name__]
            lines.append("check(%r, lambda: %s, %s, %r)" % (label, expr, lit(want), tol))
        body = (setup + "\n" if setup else "") + "\n".join(lines) + "\n"
        assert '"#' not in body, name
        rust.append('''
/// %s
#[test]
fn %s() {
    let out = run(r#"
%s"#);
    assert_all_true(&out, %d);
}
''' % (doc, name, body, len(cases)))
    open(target, "w", encoding="utf-8", newline="\n").write("".join(rust))


def make_noncontig(path):
    """Tensors PyTorch saves through their storage offset and strides."""
    x = torch.arange(12, dtype=torch.float32).reshape(3, 4)
    v = torch.arange(3, dtype=torch.float64)
    i = torch.arange(20, dtype=torch.int64).reshape(4, 5)
    torch.save({"t": x.t(), "col": x[:, 1], "step": x[::2], "expand": v.unsqueeze(1).expand(3, 4), "offset": x[1:, 2:], "base": x,
                "int_t": i.t()[1:, ::2], "perm": torch.arange(24.).reshape(2, 3, 4).permute(2, 0, 1), "scalar_view": x[2, 3]}, path)


if __name__ == "__main__":
    import os
    here = os.path.dirname(os.path.abspath(__file__))
    if "--fixtures" in sys.argv:
        sys.argv.remove("--fixtures")
        make_noncontig(os.path.join(here, "noncontig.pt"))
    build(sys.argv[1] if len(sys.argv) > 1 else os.path.join(here, "..", "..", "python_torch_core.rs"))
