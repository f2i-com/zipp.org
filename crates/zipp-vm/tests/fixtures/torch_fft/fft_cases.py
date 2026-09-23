# torch.fft parity cases: every transform's values over lengths made of
# 2, 3 and 5 and of other primes (Bluestein), each norm, n/s and dim, and
# the gradients of real losses through them. Runs unchanged under PyTorch
# 2.11 (gen.py writes fft_expected.json and text_expected.txt) and under
# Zipp (python_torch_fft.rs). Complex values are built with torch.complex
# and read back through torch.view_as_real (Zipp has no complex scalars).
import math

import torch
import torch.fft as F

D = torch.float64


def vals(n, seed):
    return [math.sin(0.9 * i + seed) + 0.3 * math.cos(2.3 * i + 2 * seed) for i in range(n)]


def real_t(shape, seed, dtype=D):
    n = 1
    for s in shape:
        n *= s
    return torch.tensor(vals(n, seed), dtype=D).reshape(tuple(shape)).to(dtype)


def cplx(shape, seed, dtype=torch.complex128):
    rdt = D if dtype == torch.complex128 else torch.float32
    return torch.complex(real_t(shape, seed, rdt), real_t(shape, seed + 0.77, rdt))


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
        return [float(v) for v in x.reshape(-1).tolist()]
    return [float(x)]


def weights(shape, seed, dtype):
    n = 1
    for s in shape:
        n *= s
    return torch.tensor([math.cos(0.37 * k + seed) for k in range(n)], dtype=D).reshape(tuple(shape)).to(dtype)


def wsum(t, seed):
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


SIZES = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 15, 16, 17, 25, 27, 30, 31, 32, 45, 64, 97, 100, 128, 243]


def results():
    R = {}
    for rdt, cdt, tag in ((D, torch.complex128, "f64"), (torch.float32, torch.complex64, "f32")):
        for n in SIZES:
            x = real_t((n,), 0.3, rdt)
            z = cplx((n,), 1.1, cdt)
            R["%s_n%d" % (tag, n)] = flat([F.fft(z), F.ifft(z), F.fft(x), F.rfft(x), F.irfft(F.rfft(x), n), F.ihfft(x), F.hfft(z[: n // 2 + 1], n)])
        x = real_t((3, 10), 0.5, rdt)
        z = cplx((3, 10), 0.9, cdt)
        for norm in (None, "backward", "ortho", "forward"):
            key = "%s_norm_%s" % (tag, norm)
            R[key] = flat([F.fft(z, norm=norm), F.ifft(z, norm=norm), F.rfft(x, norm=norm), F.irfft(z, norm=norm), F.irfft(z, 7, norm=norm),
                           F.hfft(z, norm=norm), F.ihfft(x, norm=norm), F.fft2(z, norm=norm), F.ifft2(z, norm=norm), F.rfft2(x, norm=norm),
                           F.irfft2(z, norm=norm), F.fftn(z, norm=norm), F.ifftn(z, norm=norm), F.rfftn(x, norm=norm), F.irfftn(z, norm=norm),
                           F.hfft2(z, norm=norm), F.ihfft2(x, norm=norm), F.hfftn(z, norm=norm), F.ihfftn(x, norm=norm)])
        x3 = real_t((2, 3, 5), 0.2, rdt)
        z3 = cplx((2, 3, 5), 0.4, cdt)
        R[tag + "_n_dim"] = flat([F.fft(z3, dim=0), F.fft(z3, dim=1), F.fft(z3, n=8, dim=1), F.fft(z3, n=2), F.ifft(z3, n=7, dim=0),
                                  F.rfft(x3, dim=1), F.rfft(x3, n=9), F.rfft(x3, n=3, dim=0), F.irfft(z3, n=4, dim=1), F.irfft(z3, dim=0),
                                  F.hfft(z3, n=6, dim=1), F.ihfft(x3, dim=0), F.fft2(z3, s=(4, 6)), F.fftn(z3, s=(3,), dim=(1,)),
                                  F.fftn(z3, dim=(0, 2)), F.rfftn(x3, s=(4, 4)), F.rfftn(x3, dim=(0, 1)), F.irfftn(z3, s=(2, 5, 8)),
                                  F.irfft2(z3, s=(3, 9)), F.fftn(x3), F.ifftn(x3, s=(3, 3, 3)), F.fft2(x3, dim=(0, 2)), F.fft(z3, dim=-3)])
        R[tag + "_helpers"] = flat([F.fftfreq(5, dtype=rdt), F.fftfreq(6, 0.1, dtype=rdt), F.rfftfreq(7, 2.0, dtype=rdt), F.rfftfreq(8, dtype=rdt),
                                    F.fftshift(x3), F.ifftshift(x3), F.fftshift(x3, dim=1), F.ifftshift(z3, dim=(0, 2)),
                                    F.ifftshift(F.fftshift(z3))])
        # gradients of real losses through the transforms
        x = real_t((2, 12), 0.7, rdt)
        z = cplx((2, 12), 1.3, cdt)
        zh = cplx((2, 7), 1.9, cdt)
        R[tag + "_g_fft"] = grads(lambda a: wsum(F.fft(a), 1) + wsum(F.ifft(a, n=9), 2) + wsum(F.fft(a, n=16, norm="ortho"), 3), z)
        R[tag + "_g_fft_real"] = grads(lambda a: wsum(F.fft(a), 4) + wsum(F.ifft(a, n=5, norm="forward"), 5), x)
        R[tag + "_g_rfft"] = grads(lambda a: F.rfft(a).abs().sum() + wsum(F.rfft(a, n=7), 6) + wsum(F.rfft(a, n=16, norm="forward"), 7), x)
        R[tag + "_g_irfft"] = grads(lambda a: wsum(F.irfft(a), 8) + wsum(F.irfft(a, n=9), 9) + wsum(F.irfft(a, n=20, norm="ortho"), 10) + F.irfft(a, n=3).pow(2).sum(), zh)
        R[tag + "_g_hfft"] = grads(lambda a: wsum(F.hfft(a), 11) + wsum(F.hfft(a, n=9, norm="ortho"), 12), zh)
        R[tag + "_g_ihfft"] = grads(lambda a: wsum(F.ihfft(a), 13) + wsum(F.ihfft(a, n=15), 14), x)
        R[tag + "_g_2d"] = grads(lambda a, b: wsum(F.fft2(a), 15) + wsum(F.rfft2(b), 16) + wsum(F.irfft2(a, s=(2, 6)), 17) + wsum(F.fftn(b, s=(3, 5)), 18), z, x)
        R[tag + "_g_nd"] = grads(lambda a: wsum(F.irfftn(F.rfftn(a) * 2.0), 19) + wsum(F.ifftn(F.fftn(a), dim=(0,)), 20) + wsum(F.hfftn(F.ihfftn(a)), 21), x)
        R[tag + "_g_loss"] = grads(lambda a: (F.fft(a).abs() ** 2).mean() + F.irfft(F.rfft(a) * F.rfft(a).conj(), n=12).sum(), x)
        R[tag + "_g_shift"] = grads(lambda a: wsum(F.fftshift(F.fft(a)), 22) + wsum(F.ifftshift(a, dim=1), 23), z)
        R[tag + "_g_double"] = grads(lambda a: wsum(torch.autograd.grad(wsum(F.fft(a * a), 24), a, create_graph=True)[0], 25), x)
    # windows, stft and istft
    for dt, tag in ((D, "f64"), (torch.float32, "f32")):
        R[tag + "_windows"] = flat([torch.hann_window(8, dtype=dt), torch.hann_window(7, periodic=False, dtype=dt), torch.hamming_window(9, dtype=dt),
                                    torch.hamming_window(6, False, 0.6, 0.4, dtype=dt), torch.blackman_window(10, dtype=dt), torch.bartlett_window(9, dtype=dt),
                                    torch.bartlett_window(8, False, dtype=dt), torch.hann_window(1, dtype=dt), torch.hann_window(0, dtype=dt)])
        sig = real_t((2, 40), 0.9, dt)
        win = torch.hann_window(16, dtype=dt)
        R[tag + "_stft"] = flat([torch.stft(sig, 16, 4, window=win, return_complex=True), torch.stft(sig[0], 12, return_complex=True),
                                 torch.stft(sig, 16, 5, 12, torch.hann_window(12, dtype=dt), center=False, return_complex=True),
                                 torch.stft(sig, 8, 3, window=torch.hann_window(8, dtype=dt), pad_mode="constant", normalized=True, return_complex=True),
                                 torch.stft(sig, 8, 2, window=torch.hann_window(8, dtype=dt), onesided=False, return_complex=True),
                                 torch.stft(sig, 8, 2, window=torch.hann_window(8, dtype=dt), return_complex=False)])
        spec = torch.stft(sig, 16, 4, window=win, return_complex=True)
        R[tag + "_istft"] = flat([torch.istft(spec, 16, 4, window=win), torch.istft(spec, 16, 4, window=win, length=37), torch.istft(spec, 16, 4, window=win, length=50),
                                  torch.istft(torch.stft(sig, 8, 2, window=torch.hann_window(8, dtype=dt), normalized=True, return_complex=True), 8, 2,
                                              window=torch.hann_window(8, dtype=dt), normalized=True)])
        R[tag + "_g_stft"] = grads(lambda a: torch.stft(a, 16, 4, window=win, return_complex=True).abs().pow(2).sum() + wsum(torch.stft(a[0], 8, 3, return_complex=True), 26), sig)
        R[tag + "_g_istft"] = grads(lambda z: wsum(torch.istft(z, 16, 4, window=win, length=38), 27), spec)
    # a long transform
    long_x = real_t((4099,), 0.1)
    long_z = cplx((3000,), 0.2)
    R["f64_long"] = flat([F.rfft(long_x)[:40], F.fft(long_z)[::97], F.irfft(F.rfft(long_x), 4099)[:40]])
    return R


def text():
    out = []
    x = torch.arange(6.0)
    z = torch.complex(x, x)
    for fn in [lambda: F.fft(x, norm="bad"), lambda: F.fft(x, n=0), lambda: F.fft(x, n=-1), lambda: F.rfft(z), lambda: F.fft(x.half()),
               lambda: F.fft(x.bfloat16()), lambda: F.fft(torch.arange(4)).dtype, lambda: F.fft(x, dim=2), lambda: F.irfft(z).shape,
               lambda: F.irfft(x).dtype, lambda: F.fftn(x.reshape(2, 3), s=(4,), dim=(0, 1)), lambda: F.fftn(x.reshape(2, 3), s=(4, 5)).shape,
               lambda: F.fftn(x.reshape(2, 3), dim=(0, 0)), lambda: F.fft(torch.tensor(1.0)), lambda: F.fft(x.double()).dtype,
               lambda: F.hfft(z).dtype, lambda: F.ihfft(x).dtype, lambda: F.irfft(torch.ones(1, dtype=torch.complex64)),
               lambda: F.rfft(torch.ones(0)), lambda: F.fft2(x).shape, lambda: F.fftn(x.reshape(2, 3), dim=1).shape,
               lambda: F.rfftn(torch.ones(4, 6), s=(3,)).shape, lambda: F.irfftn(torch.ones(4, 6, dtype=torch.complex64)).shape,
               lambda: F.irfft2(torch.ones(4, 6, dtype=torch.complex64), s=(3, 5)).shape, lambda: F.fftfreq(4).dtype,
               lambda: F.rfftfreq(5, dtype=torch.float64).dtype, lambda: F.fftshift(torch.arange(6).reshape(2, 3)).tolist(),
               lambda: F.ifftshift(torch.arange(5)).tolist(), lambda: torch.fft.fft(x).shape]:
        try:
            out.append(repr(fn()))
        except Exception as e:
            out.append(type(e).__name__ + ": " + str(e).split("\n")[0])
    xr = torch.arange(6.0).requires_grad_()
    zr = torch.complex(torch.arange(4.0), torch.ones(4)).requires_grad_()
    out.append(str([type(t.grad_fn).__name__ for t in [F.fft(xr), F.rfft(xr), F.fft(zr), F.irfft(zr), F.hfft(zr), F.ihfft(xr), F.fft2(zr.reshape(2, 2))]]))
    with torch.autocast("cpu", dtype=torch.bfloat16):
        out.append(str([F.fft(x).dtype, F.rfft(x).dtype, F.fft(x.bfloat16()).dtype, F.irfft(z).dtype]))
    out.append(repr(F.fft(torch.tensor([1.0, 2.0, 3.0, 4.0]))))
    out.append(repr(F.rfft(torch.tensor([1.0, 0.0, -1.0, 0.0, 2.0, 1.0]))))
    return out
