# complex64/complex128 parity cases: values, gradients (PyTorch's convention
# for a real loss), printing, dtype rules and error messages. Runs unchanged
# under PyTorch 2.11 (gen.py writes complex_expected.json and
# text_expected.txt from it) and under Zipp (python_torch_complex.rs).
#
# Zipp's Python has no complex literals or complex scalars yet, so every
# complex value here is built with torch.complex/torch.polar and read back
# through torch.view_as_real.
import math

import torch

D = torch.float64


def vals(n, seed, scale=1.0):
    return [scale * (math.sin(0.9 * i + seed) + 0.3 * math.cos(2.3 * i + 2 * seed)) for i in range(n)]


def cplx(shape, seed, dtype=torch.complex128):
    n = 1
    for s in shape:
        n *= s
    rdt = torch.float64 if dtype == torch.complex128 else torch.float32
    re = torch.tensor(vals(n, seed), dtype=D).reshape(tuple(shape)).to(rdt)
    im = torch.tensor(vals(n, seed + 0.77), dtype=D).reshape(tuple(shape)).to(rdt)
    return torch.complex(re, im)


def real_t(shape, seed, dtype=D):
    n = 1
    for s in shape:
        n *= s
    return torch.tensor(vals(n, seed), dtype=D).reshape(tuple(shape)).to(dtype)


def flat(x):
    if isinstance(x, (tuple, list)):
        out = []
        for t in x:
            out.extend(flat(t))
        return out
    if isinstance(x, torch.Tensor):
        x = x.detach()
        if x.is_complex():
            x = torch.view_as_real(x.resolve_conj())
        vals_ = x.reshape(-1).tolist()
    else:
        vals_ = [x]
    out = []
    for v in vals_:
        v = float(v)
        if v != v:
            v = 1234.5
        elif v == math.inf:
            v = 9999.0
        elif v == -math.inf:
            v = -9999.0
        out.append(v)
    return out


def weights(shape, seed, dtype):
    n = 1
    for s in shape:
        n *= s
    return torch.tensor([math.cos(0.37 * k + seed) for k in range(n)], dtype=D).reshape(tuple(shape)).to(dtype)


def wsum(t, seed):
    # A real loss of a complex (or real) tensor: weighted sums of its parts.
    if t.is_complex():
        r = torch.view_as_real(t.resolve_conj())
        return (r * weights(r.shape, seed, r.dtype)).sum()
    return (t * weights(t.shape, seed, t.dtype)).sum()


def grads(loss_fn, *inputs):
    xs = [x.detach().clone().requires_grad_(True) for x in inputs]
    loss = loss_fn(*xs)
    gs = torch.autograd.grad(loss, xs, allow_unused=True)
    gs = [torch.zeros_like(x) if g is None else g for x, g in zip(xs, gs)]
    return [float(loss.detach())] + flat(list(gs))


UNARY = ["exp", "log", "sqrt", "sin", "cos", "tan", "tanh", "sinh", "cosh", "sigmoid", "reciprocal", "rsqrt",
         "log2", "log10", "log1p", "expm1", "square", "neg", "conj", "abs", "angle", "sgn"]


def unary(name, x):
    if name == "sigmoid":
        return torch.sigmoid(x)
    if name == "sgn":
        return torch.sgn(x)
    return getattr(x, name)()


def results():
    R = {}
    for dt, tag in ((torch.complex128, "c128"), (torch.complex64, "c64")):
        z = cplx((2, 3), 0.3, dt)
        w = cplx((2, 3), 1.1, dt)
        r = real_t((2, 3), 2.0, torch.float64 if dt == torch.complex128 else torch.float32)
        R[tag + "_make"] = flat([z, torch.polar(r.abs() + 0.5, r), torch.view_as_complex(torch.stack([r, r * 2], -1))])
        R[tag + "_arith"] = flat([z + w, z - w, z * w, z / w, z + r, r - z, z * r, r / z, z * 2.5, 1.5 - z, z / 3.0, 2.0 / z,
                                  -z, z + w[0], z * w[:, :1]])
        R[tag + "_pow"] = flat([z ** 2, z ** 3, z ** 0.5, z ** -1, z ** -2, z ** -0.5, z ** 1.5, z ** w, z ** r, torch.pow(z, 0)])
        for name in UNARY:
            R[tag + "_u_" + name] = flat(unary(name, z))
        R[tag + "_reduce"] = flat([z.sum(), z.sum(0), z.sum(1, keepdim=True), z.mean(), z.mean(1), z.prod(), z.prod(0),
                                   z.cumsum(1), z.cumsum(0), z.cumprod(1)])
        m = cplx((3, 4), 0.6, dt)
        v = cplx((4,), 1.4, dt)
        b = cplx((2, 3, 4), 2.2, dt)
        R[tag + "_matmul"] = flat([z @ m, torch.mm(z, m), m @ v, torch.mv(m, v), torch.dot(v, v), torch.vdot(v, v),
                                   torch.bmm(b, b.transpose(1, 2)), torch.matmul(b, v), v @ m.T, torch.outer(v, v)])
        R[tag + "_shape"] = flat([z.T, z.reshape(3, 2), z.permute(1, 0), z.flip(0), z.roll(1, 1), torch.cat([z, w], 1),
                                  torch.stack([z, w]), z[1], z[:, 1:], z[torch.tensor([1, 0])], z[z.real > 0], z.unsqueeze(1),
                                  torch.tril(torch.cat([z, w])), torch.diagonal(z), z[:1].expand(3, 3), z.repeat(1, 2),
                                  torch.where(r > 0, z, w), torch.where(r > 0, z, 0.0), z.masked_fill(r < 0, 0.0)])
        s = z.clone()
        s[0, 1] = w[1, 2]
        s[1] = 4.0
        s2 = z.clone()
        s2[:, 0] = r[:, 0]
        s3 = z.clone()
        s3.real = r
        s4 = z.clone()
        s4.imag = r
        R[tag + "_set"] = flat([s, s2, s3, s4, z.clone().mul_(w), z.clone().add_(1.0), z.clone().zero_(), z.clone().fill_(2.0)])
        R[tag + "_parts"] = flat([z.real, z.imag, torch.real(z), torch.imag(z), torch.view_as_real(z), z.conj(), z.conj_physical(),
                                  z.resolve_conj(), z.angle(), z.abs(), torch.isreal(torch.complex(r, r * (r > 0))),
                                  z.mH, z.adjoint(), torch.view_as_real(z).sum(-1)])
        R[tag + "_compare"] = flat([(z == w).to(D), (z == z).to(D), (z != w).to(D), torch.isclose(z, z + 1e-9).to(D),
                                    float(torch.allclose(z, z)), float(torch.allclose(z, w)), float(torch.equal(z, z.clone())),
                                    torch.isnan(z / torch.zeros_like(z)).to(D), torch.isfinite(z).to(D)])
        R[tag + "_convert"] = flat([z.to(torch.complex128), z.to(torch.complex64), r.to(dt), z.bool().to(D),
                                    torch.tensor([1.5, -2.0], dtype=dt), torch.zeros(2, dtype=dt), torch.ones(2, dtype=dt),
                                    torch.full((2,), 3.5, dtype=dt), torch.zeros_like(z), torch.ones_like(z), z.cfloat(), z.cdouble()])
        R[tag + "_norms"] = flat([torch.linalg.vector_norm(z), torch.linalg.vector_norm(z, 1, dim=1), torch.linalg.matrix_norm(z), z.norm(),
                                  torch.linalg.norm(z, dim=0)])
        R[tag + "_g_norms"] = grads(lambda a: torch.linalg.vector_norm(a) + torch.linalg.matrix_norm(a) + torch.linalg.vector_norm(a, 3, dim=0).sum(), z)
        if tag == "c128":
            # (complex64 solves differ at PyTorch's float32 LAPACK accuracy)
            R[tag + "_solve"] = flat([torch.linalg.solve(m[:, :3], z.T), torch.linalg.inv(m[:, :3])])
            R[tag + "_g_solve"] = grads(lambda a, c: wsum(torch.linalg.solve(c[:, :3], a.T), 20) + wsum(torch.linalg.inv(c[:, :3]), 21), z, m)
        # gradients (a real loss of complex intermediates)
        R[tag + "_g_mul_conj"] = grads(lambda a: (a * a.conj()).real.sum(), z)
        R[tag + "_g_abs"] = grads(lambda a: a.abs().sum(), z)
        R[tag + "_g_angle"] = grads(lambda a: wsum(a.angle(), 3), z)
        R[tag + "_g_sgn"] = grads(lambda a: wsum(torch.sgn(a), 4), z)
        R[tag + "_g_arith"] = grads(lambda a, c: wsum(a * c + a / c - c / (a + 2.0) + a * 3.0, 5), z, w)
        R[tag + "_g_mixed"] = grads(lambda a, x: wsum(a * x + x / a + (x - a) * a, 6), z, r)
        R[tag + "_g_make"] = grads(lambda x, y: wsum(torch.complex(x, y) ** 2 + torch.polar(x.abs() + 1.0, y), 7), r, r * 0.5 + 0.1)
        for name in UNARY:
            if name in ("abs", "angle", "sgn", "conj", "neg"):
                continue
            R[tag + "_g_u_" + name] = grads(lambda a, name=name: wsum(unary(name, a), 8), z)
        R[tag + "_g_pow"] = grads(lambda a, c: wsum(a ** 2 + a ** 0.5 + a ** c + a ** 1.5, 9), z, w)
        R[tag + "_g_matmul"] = grads(lambda a, c: wsum(a @ c, 10), z, m)
        R[tag + "_g_bmm"] = grads(lambda a: wsum(torch.bmm(a, a.transpose(1, 2).conj()), 11), b)
        R[tag + "_g_reduce"] = grads(lambda a: wsum(a.sum(0) * a.mean(1).sum() + a.prod(1).sum() + a.cumsum(1), 12), z)
        R[tag + "_g_cumprod"] = grads(lambda a: wsum(a.cumprod(1) + a.cumprod(0), 18), z)
        R[tag + "_g_einsum"] = grads(lambda a, c: wsum(torch.einsum("ij,jk->ik", a, c).sum(1) + torch.einsum("ij,ij->i", a, a), 19), z, m)
        R[tag + "_g_shape"] = grads(lambda a: wsum(torch.cat([a.T.reshape(2, 3), a.flip(1)], 0)[1:3] + a[torch.tensor([1, 0])], 13), z)
        R[tag + "_g_parts"] = grads(lambda a: wsum(a.real * a.imag + torch.view_as_real(a).sum() + a.conj().imag, 14), z)
        R[tag + "_g_vac"] = grads(lambda x: wsum(torch.view_as_complex(x) ** 2, 15), torch.view_as_real(z).contiguous())
        R[tag + "_g_to"] = grads(lambda x: wsum(x.to(dt) * z, 16), r)
        R[tag + "_g_where"] = grads(lambda a: wsum(torch.where(r > 0, a, a * a), 17), z)
        R[tag + "_g_fft_like"] = grads(lambda a: (a.exp() * w).abs().pow(2).sum() + (a @ m).abs().sum(), z)
    return R


def text():
    """Printing, dtype properties and error messages, one line each."""
    out = []
    z = cplx((2, 3), 0.3, torch.complex64)
    for t in [z, cplx((3,), 1.0, torch.complex128), cplx((), 0.5, torch.complex64), torch.complex(torch.tensor([1.0, -2.5]), torch.tensor([3.0, -0.0])),
              torch.complex(torch.tensor([1e-5, 200.0]), torch.tensor([1e6, 0.5])), torch.zeros(0, dtype=torch.complex64),
              cplx((2, 3), 0.3, torch.complex64).requires_grad_(), torch.ones(2, 2, dtype=torch.complex128),
              torch.complex(torch.arange(4.0), torch.ones(4))]:
        out.append(repr(t).replace("\n", "|"))
    out.append(str([torch.complex64, torch.complex128, torch.cfloat, torch.cdouble]))
    out.append(str([torch.complex64.is_complex, torch.complex64.is_floating_point, torch.complex64.itemsize, torch.complex128.itemsize,
                    torch.complex64.to_real(), torch.complex128.to_real(), torch.float32.to_complex(), torch.float64.to_complex()]))
    out.append(str([z.is_complex(), torch.is_complex(z), z.is_floating_point(), torch.is_floating_point(z), z.element_size(),
                    z.dtype, z.real.dtype, z.abs().dtype, z.angle().dtype, (z == z).dtype, z.is_conj(), z.conj().resolve_conj().is_conj()]))
    out.append(str([torch.finfo(torch.complex64).eps, torch.finfo(torch.complex128).eps, torch.finfo(torch.complex64).max]))
    f32 = torch.ones(2)
    f64 = torch.ones(2, dtype=torch.float64)
    c64 = torch.ones(2, dtype=torch.complex64)
    c128 = torch.ones(2, dtype=torch.complex128)
    i64 = torch.ones(2, dtype=torch.int64)
    out.append(str([torch.promote_types(torch.complex64, torch.float64), torch.promote_types(torch.float32, torch.complex64),
                    torch.promote_types(torch.int64, torch.complex64), torch.promote_types(torch.complex64, torch.complex128),
                    torch.result_type(c64, torch.tensor(1.0, dtype=torch.float64)), torch.result_type(f64, torch.ones((), dtype=torch.complex64)),
                    torch.result_type(f32, torch.ones((), dtype=torch.complex128)), torch.result_type(i64, torch.ones((), dtype=torch.complex64)),
                    (c64 + f64).dtype, (c64 * 2.0).dtype, (c64 * 2).dtype, (i64 * c64).dtype, (c128 + f32).dtype, (c64 / i64).dtype,
                    torch.can_cast(torch.complex64, torch.float32), torch.can_cast(torch.float64, torch.complex64)]))
    out.append(str([torch.randn(3, dtype=torch.complex64).dtype, torch.randn(2, 2, dtype=torch.cdouble).shape, torch.rand(2, dtype=torch.complex64).dtype,
                    torch.zeros(2, dtype=torch.complex64).dtype, torch.view_as_real(z).shape, torch.view_as_real(z).dtype]))
    for fn in [lambda: torch.view_as_real(torch.ones(2)), lambda: torch.view_as_complex(torch.ones(2, 3)),
               lambda: torch.view_as_complex(torch.ones(2, 2, dtype=torch.int64)), lambda: torch.complex(torch.ones(2), torch.ones(2, dtype=torch.float64)),
               lambda: torch.complex(torch.ones(2, dtype=torch.int64), torch.ones(2, dtype=torch.int64)), lambda: torch.imag(torch.ones(2)),
               lambda: torch.sign(c64), lambda: (c64 * c64).sum().backward(),
               lambda: torch.polar(torch.ones(2), torch.ones(2, dtype=torch.float64))]:
        try:
            fn()
            out.append("no error")
        except Exception as e:
            out.append(type(e).__name__ + ": " + str(e).split("\n")[0])
    # (this message names C++ types, which differ by compiler)
    try:
        torch.ones(2) @ c64
        out.append("no error")
    except RuntimeError as e:
        out.append("RuntimeError " + str("same dtype" in str(e)))
    # grad_fn names along a complex graph
    a = cplx((2,), 0.1).requires_grad_()
    out.append(str([type(x.grad_fn).__name__ for x in [a * a, a.abs(), torch.view_as_real(a), a.conj(), a.exp(), a.real, a.sum()]]))
    return out
