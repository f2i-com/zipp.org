"""torch.fft for Zipp: discrete Fourier transforms over complex64/complex128
tensors (real float32/float64 inputs where PyTorch takes them), with
PyTorch's `n`/`s`, `dim` and `norm` ("backward", "ortho", "forward")
arguments and its gradients.

Each 1-D transform runs `_zipp_tensor.fft` over the rows of the moved dim:
a mixed-radix Cooley-Tukey for lengths made of 2, 3 and 5 and Bluestein's
algorithm for any other length, in double precision, rounded once to the
result dtype (natively, or in the runtime's JavaScript loop when a native
kernel declines). N-dimensional transforms apply the 1-D ones dim by dim.
Every transform is linear, so its gradient is the adjoint transform
(PyTorch's FftC2C/FftR2C/FftC2R backward), itself differentiable.
"""
import math as _math
import torch

_k = torch._k
_int = int
_float = float
_range = range
_len = len


# ---- argument checks ---------------------------------------------------------------------
def _norm_scale(norm, n, forward):
    """The factor a transform of length n scales by: `forward` is the
    direction whose "backward" normalization is none (fft, rfft, hfft)."""
    if norm is None or norm == "backward":
        return 1.0 if forward else 1.0 / n
    if norm == "ortho":
        return 1.0 / _math.sqrt(n)
    if norm == "forward":
        return 1.0 / n if forward else 1.0
    raise RuntimeError('Invalid normalization mode: "%s"' % (norm,))


def _check_n(n):
    if n < 1:
        raise RuntimeError("Invalid number of data points (%d) specified" % n)


def _dim(x, dim):
    rank = x.ndim
    if rank == 0:
        raise IndexError("Dimension specified as %d but tensor has no dimensions" % dim)
    return torch._norm_dim(dim, rank)


def _float_input(x):
    """A real input as float32/float64 (integers and bool take the default
    dtype, as PyTorch promotes them)."""
    dt = x.dtype
    if dt.is_complex:
        return x
    if dt is torch.float16 or dt is torch.bfloat16:
        raise RuntimeError("Unsupported dtype %s" % torch._CAST_NAME[dt.name])
    if not dt.is_floating_point:
        return x.to(torch.get_default_dtype())
    return x


def _complex_input(x):
    x = _float_input(x)
    return x if x.dtype.is_complex else x.to(x.dtype.to_complex())


def _resize(x, dim, n):
    """x truncated or zero-padded to length n along dim (differentiable)."""
    have = x.shape[dim]
    if have == n:
        return x
    if have > n:
        return torch.narrow(x, dim, 0, n)
    shape = list(x.shape)
    shape[dim] = n - have
    return torch.cat([x, torch.zeros(*shape, dtype=x.dtype)], dim)


def _rows(x, dim):
    """x with `dim` moved last (no history), and the order to move it back."""
    last = x.ndim - 1
    if dim == last:
        return x, None
    order = [d for d in _range(x.ndim) if d != dim] + [dim]
    back = [0] * x.ndim
    for i, d in enumerate(order):
        back[d] = i
    return x.permute(*order), back


def _run(x, dim, n, mode, inverse, scale):
    """The raw transform of x (no history): mode 0 complex -> complex,
    1 real -> onesided complex, 2 onesided complex -> real of length n,
    3 real -> all n bins (conjugate-symmetric)."""
    with torch.no_grad():
        xm, back = _rows(x.detach(), dim)
        n_in = xm.shape[-1]
        rows = torch._numel(xm.shape) // n_in if n_in else 0
        out_len = n // 2 + 1 if mode == 1 else n
        shape = tuple(xm.shape[:-1]) + (out_len,)
        if mode == 2:
            dt = x.dtype.to_real()
        elif mode == 1 or mode == 3:
            dt = x.dtype.to_complex()
        else:
            dt = x.dtype
        if n_in == 0:
            y = torch.zeros(*shape, dtype=dt)
        else:
            y = torch.Tensor(_k.fft(xm._s, rows, n_in, n, mode, inverse, _float(scale)), shape, dt)
        return y if back is None else y.permute(*back)


def _attach(out, x, backward, name):
    if torch.is_grad_enabled() and x.requires_grad:
        node = torch._Node(lambda g: (backward(g),), (x,), name)
        node.diff = True
        out.requires_grad = True
        out._node = node
    return out


# ---- the differentiable 1-D transforms ------------------------------------------------------
def _c2c(x, dim, n, inverse, scale):
    """The length-n DFT of complex x along dim (x zero-padded or truncated),
    times scale; its gradient is the opposite-direction transform."""
    n_in = x.shape[dim]
    out = _run(x, dim, n, 0, inverse, scale)
    return _attach(out, x, lambda g: _resize(_c2c(g, dim, n, not inverse, scale), dim, n_in), "FftC2C")


def _r2c(x, dim, n, inverse, scale, onesided):
    """The DFT of real x along dim: its n//2+1 onesided bins, or all n."""
    n_in = x.shape[dim]
    out = _run(x, dim, n, 1 if onesided else 3, inverse, scale)

    def backward(g):
        if onesided:
            g = _resize(g, dim, n)
        return _resize(torch.real(_c2c(g, dim, n, not inverse, scale)), dim, n_in)
    return _attach(out, x, backward, "FftR2C")


def _c2r(x, dim, n, inverse, scale):
    """The real length-n signal whose onesided spectrum is x (its first
    n//2+1 bins, Hermitian-extended), transformed and scaled."""
    n_in = x.shape[dim]
    out = _run(x, dim, n, 2, inverse, scale)

    def backward(g):
        # PyTorch's fft_c2r_backward: the onesided transform of the real
        # gradient, doubled at the bins the extension used twice.
        gi = _r2c(g, dim, n, not inverse, scale, True)
        double = n - (n // 2 + 1)
        if double > 0:
            parts = [torch.narrow(gi, dim, 0, 1), torch.narrow(gi, dim, 1, double) * 2.0]
            rest = gi.shape[dim] - 1 - double
            if rest > 0:
                parts.append(torch.narrow(gi, dim, 1 + double, rest))
            gi = torch.cat(parts, dim)
        return _resize(gi, dim, n_in)
    return _attach(out, x, backward, "FftC2R")


# ---- torch.fft -------------------------------------------------------------------------------
def _autocast(fn, args, kwargs):
    return torch._autocast_run(fn, "fp32", args, kwargs)


def fft(input, n=None, dim=-1, norm=None, *, out=None):
    if torch._autocast_cpu is not None:
        return _autocast(fft, (input, n, dim, norm), None)
    x = _float_input(input)
    d = _dim(x, dim)
    n = x.shape[d] if n is None else _int(n)
    _check_n(n)
    scale = _norm_scale(norm, n, True)
    if not x.dtype.is_complex:
        return _r2c(x, d, n, False, scale, False)
    return _c2c(x, d, n, False, scale)


def ifft(input, n=None, dim=-1, norm=None, *, out=None):
    if torch._autocast_cpu is not None:
        return _autocast(ifft, (input, n, dim, norm), None)
    x = _float_input(input)
    d = _dim(x, dim)
    n = x.shape[d] if n is None else _int(n)
    _check_n(n)
    scale = _norm_scale(norm, n, False)
    if not x.dtype.is_complex:
        return _r2c(x, d, n, True, scale, False)
    return _c2c(x, d, n, True, scale)


def rfft(input, n=None, dim=-1, norm=None, *, out=None):
    if torch._autocast_cpu is not None:
        return _autocast(rfft, (input, n, dim, norm), None)
    x = _float_input(input)
    if x.dtype.is_complex:
        raise RuntimeError("rfft expects a real input tensor, but got %s" % torch._CAST_NAME[x.dtype.name])
    d = _dim(x, dim)
    n = x.shape[d] if n is None else _int(n)
    _check_n(n)
    return _r2c(x, d, n, False, _norm_scale(norm, n, True), True)


def irfft(input, n=None, dim=-1, norm=None, *, out=None):
    if torch._autocast_cpu is not None:
        return _autocast(irfft, (input, n, dim, norm), None)
    x = _complex_input(input)
    d = _dim(x, dim)
    n = 2 * (x.shape[d] - 1) if n is None else _int(n)
    _check_n(n)
    return _c2r(x, d, n, True, _norm_scale(norm, n, False))


def hfft(input, n=None, dim=-1, norm=None, *, out=None):
    # The real spectrum of a Hermitian signal given by its first half:
    # irfft of the conjugate with the forward direction's scaling.
    if torch._autocast_cpu is not None:
        return _autocast(hfft, (input, n, dim, norm), None)
    x = _complex_input(input)
    d = _dim(x, dim)
    n = 2 * (x.shape[d] - 1) if n is None else _int(n)
    _check_n(n)
    return _c2r(torch.conj(x), d, n, True, _norm_scale(norm, n, True))


def ihfft(input, n=None, dim=-1, norm=None, *, out=None):
    if torch._autocast_cpu is not None:
        return _autocast(ihfft, (input, n, dim, norm), None)
    x = _float_input(input)
    if x.dtype.is_complex:
        raise RuntimeError("ihfft expects a real input tensor, but got %s" % torch._CAST_NAME[x.dtype.name])
    d = _dim(x, dim)
    n = x.shape[d] if n is None else _int(n)
    _check_n(n)
    return torch.conj(_r2c(x, d, n, False, _norm_scale(norm, n, False), True))


def _dims_and_sizes(x, s, dim, default_all):
    """PyTorch's canonicalization of (s, dim): the transformed dims and
    their lengths."""
    if dim is None:
        if s is None:
            dims = list(_range(x.ndim)) if default_all else [x.ndim - 2, x.ndim - 1]
        else:
            dims = list(_range(x.ndim - _len(s), x.ndim))
    elif isinstance(dim, _int):
        dims = [dim]
    else:
        dims = list(dim)
    dims = [_dim(x, d) for d in dims]
    if _len(set(dims)) != _len(dims):
        raise RuntimeError("FFT dims must be unique")
    if s is not None:
        s = list(s)
        if _len(s) != _len(dims):
            raise RuntimeError("When given, dim and shape arguments must have the same length")
        sizes = [x.shape[d] if v == -1 else _int(v) for d, v in zip(dims, s)]
    else:
        sizes = [x.shape[d] for d in dims]
    for n in sizes:
        _check_n(n)
    return dims, sizes


def _fftn(input, s, dim, norm, inverse, default_all):
    x = _float_input(input)
    dims, sizes = _dims_and_sizes(x, s, dim, default_all)
    if not dims:
        return x if x.dtype.is_complex else x.to(x.dtype.to_complex())
    out = x
    for i, (d, n) in enumerate(zip(dims, sizes)):
        scale = _norm_scale(norm, n, not inverse)
        if not out.dtype.is_complex:
            out = _r2c(out, d, n, inverse, scale, False)
        else:
            out = _c2c(out, d, n, inverse, scale)
    return out


def fftn(input, s=None, dim=None, norm=None, *, out=None):
    if torch._autocast_cpu is not None:
        return _autocast(fftn, (input, s, dim, norm), None)
    return _fftn(input, s, dim, norm, False, True)


def ifftn(input, s=None, dim=None, norm=None, *, out=None):
    if torch._autocast_cpu is not None:
        return _autocast(ifftn, (input, s, dim, norm), None)
    return _fftn(input, s, dim, norm, True, True)


def fft2(input, s=None, dim=(-2, -1), norm=None, *, out=None):
    if torch._autocast_cpu is not None:
        return _autocast(fft2, (input, s, dim, norm), None)
    return _fftn(input, s, dim, norm, False, False)


def ifft2(input, s=None, dim=(-2, -1), norm=None, *, out=None):
    if torch._autocast_cpu is not None:
        return _autocast(ifft2, (input, s, dim, norm), None)
    return _fftn(input, s, dim, norm, True, False)


def _rfftn(input, s, dim, norm, default_all):
    x = _float_input(input)
    if x.dtype.is_complex:
        raise RuntimeError("rfftn expects a real-valued input tensor, but got %s" % torch._CAST_NAME[x.dtype.name])
    dims, sizes = _dims_and_sizes(x, s, dim, default_all)
    if not dims:
        raise RuntimeError("rfftn must transform at least one axis")
    last = dims[-1]
    out = _r2c(x, last, sizes[-1], False, _norm_scale(norm, sizes[-1], True), True)
    for d, n in zip(dims[:-1], sizes[:-1]):
        out = _c2c(out, d, n, False, _norm_scale(norm, n, True))
    return out


def rfftn(input, s=None, dim=None, norm=None, *, out=None):
    if torch._autocast_cpu is not None:
        return _autocast(rfftn, (input, s, dim, norm), None)
    return _rfftn(input, s, dim, norm, True)


def rfft2(input, s=None, dim=(-2, -1), norm=None, *, out=None):
    if torch._autocast_cpu is not None:
        return _autocast(rfft2, (input, s, dim, norm), None)
    return _rfftn(input, s, dim, norm, False)


def _irfftn(input, s, dim, norm, default_all, conj_input, forward_scale):
    x = _complex_input(input)
    if dim is None and s is None and not default_all:
        dim = (-2, -1)
    dims, _sizes = _dims_and_sizes_c2r(x, s, dim, default_all)
    sizes = _sizes
    if conj_input:
        x = torch.conj(x)
    out = x
    for d, n in zip(dims[:-1], sizes[:-1]):
        out = _c2c(out, d, n, True, _norm_scale(norm, n, forward_scale))
    last = dims[-1]
    n = sizes[-1]
    return _c2r(out, last, n, True, _norm_scale(norm, n, forward_scale))


def _dims_and_sizes_c2r(x, s, dim, default_all):
    """(s, dim) for a c2r transform: the last dim's default length is
    2 * (bins - 1)."""
    if dim is None:
        if s is None:
            dims = list(_range(x.ndim)) if default_all else [x.ndim - 2, x.ndim - 1]
        else:
            dims = list(_range(x.ndim - _len(s), x.ndim))
    elif isinstance(dim, _int):
        dims = [dim]
    else:
        dims = list(dim)
    dims = [_dim(x, d) for d in dims]
    if not dims:
        raise RuntimeError("irfftn must transform at least one axis")
    if _len(set(dims)) != _len(dims):
        raise RuntimeError("FFT dims must be unique")
    if s is not None:
        s = list(s)
        if _len(s) != _len(dims):
            raise RuntimeError("When given, dim and shape arguments must have the same length")
        sizes = [_int(v) for v in s]
        for i, d in enumerate(dims):
            if sizes[i] == -1:
                sizes[i] = 2 * (x.shape[d] - 1) if i == _len(dims) - 1 else x.shape[d]
    else:
        sizes = [x.shape[d] for d in dims]
        sizes[-1] = 2 * (x.shape[dims[-1]] - 1)
    for n in sizes:
        _check_n(n)
    return dims, sizes


def irfftn(input, s=None, dim=None, norm=None, *, out=None):
    if torch._autocast_cpu is not None:
        return _autocast(irfftn, (input, s, dim, norm), None)
    return _irfftn(input, s, dim, norm, True, False, False)


def irfft2(input, s=None, dim=(-2, -1), norm=None, *, out=None):
    if torch._autocast_cpu is not None:
        return _autocast(irfft2, (input, s, dim, norm), None)
    return _irfftn(input, s, dim, norm, False, False, False)


def hfftn(input, s=None, dim=None, norm=None, *, out=None):
    if torch._autocast_cpu is not None:
        return _autocast(hfftn, (input, s, dim, norm), None)
    return _irfftn(input, s, dim, norm, True, True, True)


def hfft2(input, s=None, dim=(-2, -1), norm=None, *, out=None):
    if torch._autocast_cpu is not None:
        return _autocast(hfft2, (input, s, dim, norm), None)
    return _irfftn(input, s, dim, norm, False, True, True)


def _ihfftn(input, s, dim, norm, default_all):
    x = _float_input(input)
    if x.dtype.is_complex:
        raise RuntimeError("ihfftn expects a real-valued input tensor, but got %s" % torch._CAST_NAME[x.dtype.name])
    dims, sizes = _dims_and_sizes(x, s, dim, default_all)
    if not dims:
        raise RuntimeError("ihfftn must transform at least one axis")
    last = dims[-1]
    out = _r2c(x, last, sizes[-1], True, _norm_scale(norm, sizes[-1], False), True)
    for d, n in zip(dims[:-1], sizes[:-1]):
        out = _c2c(out, d, n, True, _norm_scale(norm, n, False))
    return out


def ihfftn(input, s=None, dim=None, norm=None, *, out=None):
    if torch._autocast_cpu is not None:
        return _autocast(ihfftn, (input, s, dim, norm), None)
    return _ihfftn(input, s, dim, norm, True)


def ihfft2(input, s=None, dim=(-2, -1), norm=None, *, out=None):
    if torch._autocast_cpu is not None:
        return _autocast(ihfft2, (input, s, dim, norm), None)
    return _ihfftn(input, s, dim, norm, False)


# ---- helpers ---------------------------------------------------------------------------------
def fftfreq(n, d=1.0, *, dtype=None, layout=None, device=None, requires_grad=False, out=None):
    n = _int(n)
    dt = torch.get_default_dtype() if dtype is None else dtype
    vals = [i for i in _range((n + 1) // 2)] + [i for i in _range(-(n // 2), 0)]
    out = torch.tensor([_float(v) for v in vals], dtype=dt) * (1.0 / (n * d))
    return out.requires_grad_(requires_grad) if requires_grad else out


def rfftfreq(n, d=1.0, *, dtype=None, layout=None, device=None, requires_grad=False, out=None):
    n = _int(n)
    dt = torch.get_default_dtype() if dtype is None else dtype
    out = torch.tensor([_float(v) for v in _range(n // 2 + 1)], dtype=dt) * (1.0 / (n * d))
    return out.requires_grad_(requires_grad) if requires_grad else out


def _shift_dims(x, dim):
    if dim is None:
        return list(_range(x.ndim))
    if isinstance(dim, _int):
        return [dim]
    return list(dim)


def fftshift(input, dim=None):
    dims = _shift_dims(input, dim)
    if not dims:
        return input
    return torch.roll(input, [input.shape[d] // 2 for d in dims], dims)


def ifftshift(input, dim=None):
    dims = _shift_dims(input, dim)
    if not dims:
        return input
    return torch.roll(input, [(input.shape[d] + 1) // 2 for d in dims], dims)
