"""torch.nn.init for Zipp: in-place initializers on the shared generator.

Bounds and standard deviations are computed with PyTorch's formulas, in the
same order, so a given generator state draws the same values.
"""
import math
import torch


def calculate_gain(nonlinearity, param=None):
    linear_fns = ["linear", "conv1d", "conv2d", "conv3d", "conv_transpose1d", "conv_transpose2d", "conv_transpose3d"]
    if nonlinearity in linear_fns or nonlinearity == "sigmoid":
        return 1
    if nonlinearity == "tanh":
        return 5.0 / 3
    if nonlinearity == "relu":
        return math.sqrt(2.0)
    if nonlinearity == "leaky_relu":
        if param is None:
            negative_slope = 0.01
        elif not isinstance(param, bool) and isinstance(param, (int, float)):
            negative_slope = param
        else:
            raise ValueError("negative_slope %s not a valid number" % (param,))
        return math.sqrt(2.0 / (1 + negative_slope ** 2))
    if nonlinearity == "selu":
        return 3.0 / 4
    raise ValueError("Unsupported nonlinearity %s" % nonlinearity)


def _calculate_fan_in_and_fan_out(tensor):
    if tensor.dim() < 2:
        raise ValueError("Fan in and fan out can not be computed for tensor with fewer than 2 dimensions")
    receptive = 1
    for d in tensor.shape[2:]:
        receptive *= d
    return tensor.shape[1] * receptive, tensor.shape[0] * receptive


_fans = _calculate_fan_in_and_fan_out


def _calculate_correct_fan(tensor, mode):
    mode = mode.lower()
    if mode not in ("fan_in", "fan_out"):
        raise ValueError("Mode %s not supported, please use one of fan_in, fan_out" % mode)
    fan_in, fan_out = _calculate_fan_in_and_fan_out(tensor)
    return fan_in if mode == "fan_in" else fan_out


def constant_(t, val):
    with torch.no_grad():
        t.fill_(val)
    return t


def zeros_(t):
    with torch.no_grad():
        t.zero_()
    return t


def ones_(t):
    with torch.no_grad():
        t.fill_(1.0)
    return t


def uniform_(t, a=0.0, b=1.0, generator=None):
    with torch.no_grad():
        t.uniform_(a, b, generator=generator)
    return t


def normal_(t, mean=0.0, std=1.0, generator=None):
    with torch.no_grad():
        t.normal_(mean, std, generator=generator)
    return t


def kaiming_uniform_(t, a=0, mode="fan_in", nonlinearity="leaky_relu", generator=None):
    if 0 in tuple(t.shape):
        return t
    fan = _calculate_correct_fan(t, mode)
    gain = calculate_gain(nonlinearity, a)
    std = gain / math.sqrt(fan)
    bound = math.sqrt(3.0) * std
    return uniform_(t, -bound, bound, generator)


def kaiming_normal_(t, a=0, mode="fan_in", nonlinearity="leaky_relu", generator=None):
    if 0 in tuple(t.shape):
        return t
    fan = _calculate_correct_fan(t, mode)
    gain = calculate_gain(nonlinearity, a)
    std = gain / math.sqrt(fan)
    return normal_(t, 0.0, std, generator)


def xavier_uniform_(t, gain=1.0, generator=None):
    fan_in, fan_out = _calculate_fan_in_and_fan_out(t)
    std = gain * math.sqrt(2.0 / float(fan_in + fan_out))
    a = math.sqrt(3.0) * std
    return uniform_(t, -a, a, generator)


def xavier_normal_(t, gain=1.0, generator=None):
    fan_in, fan_out = _calculate_fan_in_and_fan_out(t)
    std = gain * math.sqrt(2.0 / float(fan_in + fan_out))
    return normal_(t, 0.0, std, generator)


def _erfinv(x):
    """Elementwise inverse error function of a tensor in (-1, 1): M. Giles'
    single-precision polynomials (float32-accurate, as trunc_normal_ needs)."""
    w = -torch.log((1 - x) * (1 + x))
    small = w < 5
    ws = torch.where(small, w, torch.full_like(w, 2.5)) - 2.5
    wl = torch.sqrt(torch.where(small, torch.full_like(w, 9.0), w)) - 3
    p_small = 2.81022636e-08
    for c in (3.43273939e-07, -3.5233877e-06, -4.39150654e-06, 0.00021858087, -0.00125372503, -0.00417768164, 0.246640727, 1.50140941):
        p_small = c + p_small * ws
    p_large = -0.000200214257
    for c in (0.000100950558, 0.00134934322, -0.00367342844, 0.00573950773, -0.0076224613, 0.00943887047, 1.00167406, 2.83297682):
        p_large = c + p_large * wl
    return torch.where(small, p_small, p_large) * x


def trunc_normal_(tensor, mean=0.0, std=1.0, a=-2.0, b=2.0, generator=None):
    def norm_cdf(x):
        return (1.0 + math.erf(x / math.sqrt(2.0))) / 2.0
    lo = norm_cdf((a - mean) / std)
    hi = norm_cdf((b - mean) / std)
    with torch.no_grad():
        tensor.uniform_(2 * lo - 1, 2 * hi - 1, generator=generator)
        u = tensor.double().clamp(min=-1 + 1e-15, max=1 - 1e-15)
        values = (_erfinv(u) * (std * math.sqrt(2.0)) + mean).clamp(min=a, max=b)
        tensor.copy_(values.to(tensor.dtype))
    return tensor


def eye_(t):
    if t.dim() != 2:
        raise ValueError("Only tensors with 2 dimensions are supported")
    with torch.no_grad():
        t.copy_(torch.eye(t.shape[0], t.shape[1], dtype=t.dtype))
    return t


def dirac_(tensor, groups=1):
    dims = tensor.dim()
    if dims not in (3, 4, 5):
        raise ValueError("Only tensors with 3, 4, or 5 dimensions are supported")
    sizes = tuple(tensor.shape)
    if sizes[0] % groups != 0:
        raise ValueError("dim 0 must be divisible by groups")
    out_per_group = sizes[0] // groups
    min_dim = min(out_per_group, sizes[1])
    with torch.no_grad():
        tensor.zero_()
        for g in range(groups):
            for d in range(min_dim):
                index = (g * out_per_group + d, d) + tuple(s // 2 for s in sizes[2:])
                tensor[index] = 1
    return tensor


def _orthonormal_columns(a):
    """Q of the QR decomposition of the m x n (m >= n) tensor `a` with a
    positive diagonal R, which is what PyTorch's `q * sign(diag(r))`
    normalises any QR to: Gram-Schmidt with reorthogonalisation, in float64.
    Columns not yet filled are zero, so `q @ (q.T @ v)` projects onto the
    ones done so far."""
    m, n = a.shape
    q = torch.zeros(m, n, dtype=torch.float64)
    for j in range(n):
        v = a[:, j]
        for _ in range(2):
            v = v - torch.matmul(q, torch.matmul(v, q))
        norm = math.sqrt((v * v).sum().item())
        q[:, j] = v / norm if norm > 0 else v
    return q


def orthogonal_(tensor, gain=1, generator=None):
    if tensor.dim() < 2:
        raise ValueError("Only tensors with 2 or more dimensions are supported")
    if tensor.numel() == 0:
        return tensor
    rows = tensor.shape[0]
    cols = tensor.numel() // rows
    with torch.no_grad():
        flat = torch.empty(rows, cols, dtype=tensor.dtype).normal_(0, 1, generator=generator).double()
        if rows < cols:
            flat = flat.transpose(0, 1)
        q = _orthonormal_columns(flat)
        if rows < cols:
            q = q.transpose(0, 1)
        tensor.copy_((q * gain).to(tensor.dtype).reshape(*tensor.shape))
    return tensor


def sparse_(tensor, sparsity, std=0.01, generator=None):
    if tensor.dim() != 2:
        raise ValueError("Only tensors with 2 dimensions are supported")
    rows, cols = tensor.shape
    num_zeros = int(math.ceil(sparsity * rows))
    with torch.no_grad():
        tensor.normal_(0, std, generator=generator)
        for col in range(cols):
            order = torch.randperm(rows, generator=generator) if generator is not None else torch.randperm(rows)
            for r in order.tolist()[:num_zeros]:
                tensor[r, col] = 0
    return tensor


def _guarded(fn):
    """fn, refusing an uninitialized (lazy) parameter with PyTorch's error."""
    def init(tensor, *args, **kwargs):
        if getattr(tensor, "_uninitialized", False):
            raise ValueError("Attempted to use an uninitialized parameter in <function %s>. This error happens when you are using a `LazyModule` or explicitly manipulating `torch.nn.parameter.%s` objects. When using LazyModules Call `forward` with a dummy batch to initialize the parameters before calling torch functions" % (fn.__name__, type(tensor).__name__))
        return fn(tensor, *args, **kwargs)
    init.__name__ = fn.__name__
    init.__doc__ = fn.__doc__
    return init


# PyTorch routes these four through __torch_function__; the others reach
# an uninitialized tensor's own methods (or its shape) first.
constant_ = _guarded(constant_)
uniform_ = _guarded(uniform_)
normal_ = _guarded(normal_)
kaiming_uniform_ = _guarded(kaiming_uniform_)

# The deprecated names without the trailing underscore.
uniform = uniform_
normal = normal_
constant = constant_
eye = eye_
dirac = dirac_
xavier_uniform = xavier_uniform_
xavier_normal = xavier_normal_
kaiming_uniform = kaiming_uniform_
kaiming_normal = kaiming_normal_
orthogonal = orthogonal_
sparse = sparse_
