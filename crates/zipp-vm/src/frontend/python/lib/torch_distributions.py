"""torch.distributions for Zipp: PyTorch's probability distributions,
transforms, constraints and KL registry, written over the bundled tensor ops
so that `log_prob`, `entropy`, `rsample` and friends are differentiable with
the ordinary autograd.

The classes follow PyTorch 2.11's `torch/distributions/*.py` (argument
checking, broadcasting, shapes, `expand`, validation messages). Sampling
draws from torch's generator (`torch.rand`/`torch.randn`/`torch.poisson`
and the tensor `*_` samplers), so values follow the manual seed but are not
PyTorch's streams. `constraints`, `transforms` and `kl` are also the
submodules torch.distributions.constraints/.transforms/.kl, which re-export
the objects defined here; `utils` and `constraint_registry` are attributes.

`lgamma`, `digamma` and the regularized incomplete gamma use `torch.lgamma`
/ `torch.digamma` when torch provides them and otherwise a per-element
kernel on Python floats (`math.lgamma`, a recurrence plus asymptotic
digamma, a series/continued-fraction trigamma) with the matching backward.
Gamma's `rsample` is implicitly reparameterised: the gradient of a sample
with respect to its concentration is -dP(a, x)/da / p(x; a), evaluated with
PyTorch's own approximation (`_standard_gamma_grad`), so a given sample gets
PyTorch's gradient. Dirichlet (and Beta) samples are normalised Gamma draws
and are differentiated through that normalisation, an unbiased pathwise
gradient that differs per sample from PyTorch's `_dirichlet_grad`.
"""
import math
import torch

_Number = (int, float)
inf = math.inf
nan = math.nan
euler_constant = 0.57721566490153286060


# ---- namespaces -----------------------------------------------------------------------------
class _Namespace:
    """What PyTorch has as a submodule (`constraints`, `transforms`, ...)."""

    def __init__(self, name):
        self.__name__ = name

    def __repr__(self):
        return "<namespace '%s'>" % self.__name__


def _size(*parts):
    out = []
    for p in parts:
        out.extend(p)
    return torch.Size(tuple(out))


def _numel(shape):
    n = 1
    for s in shape:
        n *= s
    return n


def _all_true(valid):
    if isinstance(valid, bool):
        return valid
    return bool(valid.all())


def _fmt_bound(v):
    """A bound as PyTorch's f-string shows it (a 0-d tensor as its value)."""
    if isinstance(v, torch.Tensor) and v.dim() == 0:
        return format(v.item(), "")
    return str(v)


def _fmt_param(v):
    if isinstance(v, torch.Tensor):
        if v.numel() == 1:
            return format(v.item(), "") if v.dim() == 0 else str(v)
        return str(v.size())
    return str(v)


# ---- special functions ----------------------------------------------------------------------
def _elementwise(x, fn, grad, name):
    """`fn` applied per element on Python floats; backward `g * grad(x)`."""
    if not isinstance(x, torch.Tensor):
        x = torch.tensor(float(x), dtype=torch.get_default_dtype())
    if not x.dtype.is_floating_point:
        x = x.to(torch.get_default_dtype())
    vals = torch._k.to_list(x._s)
    out = torch.Tensor(torch._k.from_flat(x.dtype.name, [fn(v) for v in vals]), x.shape, x.dtype)
    if grad is not None and torch.is_grad_enabled() and x.requires_grad:
        def backward(g):
            return (g * grad(x),)
        node = torch._Node(backward, (x,), name, (x,))
        node.diff = True
        out.requires_grad = True
        out._node = node
    return out


def _py_lgamma(v):
    if v != v:
        return v
    if v <= 0 and v == math.floor(v):
        return inf
    if v == inf:
        return inf
    return math.lgamma(v)


def _py_digamma(x):
    if x != x or x == -inf:
        return nan
    if x == inf:
        return inf
    if x <= 0 and x == math.floor(x):
        # PyTorch: -inf at 0 (from above), nan at negative integers.
        return -inf if x == 0 else nan
    result = 0.0
    if x < 0:
        # Reflection: psi(1 - x) - psi(x) = pi cot(pi x).
        result = -math.pi / math.tan(math.pi * x)
        x = 1.0 - x
    while x < 10.0:
        result -= 1.0 / x
        x += 1.0
    if x == 10.0:
        return result + 2.25175258906672110764
    inv2 = 1.0 / (x * x)
    series = inv2 * (1.0 / 12 - inv2 * (1.0 / 120 - inv2 * (1.0 / 252 - inv2 * (1.0 / 240 - inv2 * (1.0 / 132 - inv2 * (691.0 / 32760 - inv2 / 12.0))))))
    return result + math.log(x) - 0.5 / x - series


def _py_trigamma(x):
    # PyTorch's calc_trigamma: reflection below 1/2, six recurrence steps
    # and a short asymptotic series (so gradients of digamma agree with
    # PyTorch's to the last bits, including its truncation).
    if x != x:
        return nan
    if x <= 0 and x == math.floor(x):
        return inf
    sign = 1.0
    result = 0.0
    if x < 0.5:
        sign = -1.0
        s = math.sin(math.pi * x)
        result -= (math.pi * math.pi) / (s * s)
        x = 1.0 - x
    for _ in range(6):
        result += 1.0 / (x * x)
        x += 1.0
    ixx = 1.0 / (x * x)
    result += (1.0 + 1.0 / (2.0 * x) + ixx * (1.0 / 6 - ixx * (1.0 / 30 - ixx * (1.0 / 42)))) / x
    return sign * result


def _trigamma(x):
    return _elementwise(x, _py_trigamma, None, "Polygamma")


def _digamma_fallback(x):
    return _elementwise(x, _py_digamma, _trigamma, "Digamma")


def _lgamma_fallback(x):
    return _elementwise(x, _py_lgamma, _digamma, "Lgamma")


def _lgamma(x):
    fn = getattr(torch, "lgamma", None)
    if fn is not None:
        return fn(x if isinstance(x, torch.Tensor) else torch.tensor(float(x)))
    return _lgamma_fallback(x)


def _digamma(x):
    fn = getattr(torch, "digamma", None)
    if fn is not None:
        return fn(x if isinstance(x, torch.Tensor) else torch.tensor(float(x)))
    return _digamma_fallback(x)


def _xlogy(x, y):
    """x * log(y), zero where x is zero, with PyTorch 2.11's gradient: log(y)
    for x and x / y for y, both masked where x == 0 and y <= 0. (Composed
    here rather than taken from torch.xlogy, so the gradient at x == 0 is
    PyTorch's whichever torch is loaded.)"""
    if not isinstance(x, torch.Tensor):
        x = torch.as_tensor(x, dtype=y.dtype if isinstance(y, torch.Tensor) else None)
    if not isinstance(y, torch.Tensor):
        y = torch.as_tensor(y, dtype=x.dtype)
    safe_y = torch.where((x == 0) & (y <= 0), torch.ones_like(y), y)
    return x * torch.log(safe_y)


def _softplus(x):
    return torch.where(x > 20, x, torch.log1p(torch.exp(x.clamp(max=20))))


def _py_gammainc(a, x):
    """The regularized lower incomplete gamma P(a, x)."""
    if x != x or a != a:
        return nan
    if x <= 0:
        return 0.0
    if x == inf:
        return 1.0
    lead = a * math.log(x) - x - math.lgamma(a)
    if x < a + 1.0:
        term = 1.0 / a
        total = term
        ap = a
        for _ in range(100000):
            ap += 1.0
            term *= x / ap
            total += term
            if abs(term) < abs(total) * 1e-17:
                break
        return total * math.exp(lead)
    # Lentz's continued fraction for Q(a, x).
    tiny = 1e-300
    b = x + 1.0 - a
    c = 1.0 / tiny
    d = 1.0 / b
    h = d
    for i in range(1, 100000):
        an = -i * (i - a)
        b += 2.0
        d = an * d + b
        if abs(d) < tiny:
            d = tiny
        c = b + an / c
        if abs(c) < tiny:
            c = tiny
        d = 1.0 / d
        delta = d * c
        h *= delta
        if abs(delta - 1.0) < 1e-17:
            break
    return 1.0 - math.exp(lead) * h


def _gammainc(a, x):
    """P(a, x) elementwise; differentiable in x (as PyTorch's gammainc)."""
    special = getattr(torch, "special", None)
    fn = getattr(special, "gammainc", None) if special is not None else None
    if fn is not None:
        return fn(a, x)
    a, x = torch.broadcast_tensors(a, x)
    av = torch._k.to_list(a.detach()._s)
    xv = torch._k.to_list(x.detach()._s)
    flat = [_py_gammainc(ai, xi) for ai, xi in zip(av, xv)]
    out = torch.Tensor(torch._k.from_flat(x.dtype.name, flat), x.shape, x.dtype)
    if torch.is_grad_enabled() and x.requires_grad:
        def backward(g):
            dens = torch.exp((a - 1) * torch.log(x) - x - _lgamma(a))
            return (None, g * dens)
        node = torch._Node(backward, (a, x), "Igamma", (a, x))
        out.requires_grad = True
        out._node = node
    return out


_SGG_COEF = (
    (0.16009398, -0.094634809, 0.025146376, -0.0030648343, 1, 0.32668115, 0.10406089, 0.0014179084),
    (0.53487893, 0.1298071, 0.065735949, -0.0015649758, 0.16639465, 0.020070113, -0.0035938915, -0.00058392623),
    (0.040121004, -0.0065914022, -0.0026286047, -0.0013441777, 0.017050642, -0.0021309326, 0.00085092367, -1.5247877e-07),
)


def _standard_gamma_grad(concentration, x):
    """d x / d alpha for x ~ Gamma(alpha, 1) at a fixed quantile, elementwise,
    as PyTorch's standard_gamma_grad_one computes it (in float64): a Taylor
    series for small x, Rice's saddle-point expansion for large alpha and a
    bivariate rational approximation otherwise."""
    a, x = torch.broadcast_tensors(concentration.detach(), x.detach())
    dtype = x.dtype
    a = a.to(torch.float64)
    x = x.to(torch.float64)
    nan_t = torch.full_like(x, nan)
    # x < 0.8: a Taylor series of the incomplete gamma function.
    numer = torch.ones_like(x)
    denom = a
    series1 = numer / denom
    series2 = numer / (denom * denom)
    for i in range(1, 6):
        numer = numer * (-x / i)
        denom = denom + 1
        series1 = series1 + numer / denom
        series2 = series2 + numer / (denom * denom)
    pow_x_alpha = x ** a
    gamma_pdf = x ** (a - 1) * torch.exp(-x)
    gamma_cdf = pow_x_alpha * series1
    gamma_cdf_alpha = torch.where(x > 0, (torch.log(x) - _digamma(a)) * gamma_cdf - pow_x_alpha * series2, nan_t)
    series = torch.where(gamma_pdf != 0, -gamma_cdf_alpha / gamma_pdf, nan_t)
    series = torch.where(series != series, torch.zeros_like(series), series)
    # alpha > 8: Rice's saddle-point expansion (a polynomial near x = alpha).
    numer_1 = 1 + 24 * a * (1 + 12 * a)
    numer_2 = 1440 * (a * a) + 6 * x * (53 - 120 * x) - 65 * x * x / a + a * (107 + 3600 * x)
    near = numer_1 * numer_2 / (1244160 * (a * a) * (a * a))
    sq = torch.sqrt(8 * a)
    term2 = sq / (a - x)
    term3 = (x - a - a * torch.log(x / a)) ** -1.5
    term23 = torch.where(x < a, term2 - term3, term2 + term3)
    term1 = torch.log(x / a) * term23 - torch.sqrt(2 / a) * (a + x) / ((a - x) * (a - x))
    stirling = 1 + 1 / (12 * a) * (1 + 1 / (24 * a))
    rice = torch.where((0.9 * a <= x) & (x <= 1.1 * a), near, -stirling * x * term1 / sq)
    # otherwise: a bivariate rational approximation in log(x / alpha), log(alpha).
    u = torch.log(x / a)
    v = torch.log(a)
    c = [_SGG_COEF[0][i] + u * (_SGG_COEF[1][i] + u * _SGG_COEF[2][i]) for i in range(8)]
    p = c[0] + v * (c[1] + v * (c[2] + v * c[3]))
    q = c[4] + v * (c[5] + v * (c[6] + v * c[7]))
    rational = torch.exp(p / q)
    out = torch.where(x < 0.8, series, torch.where(a > 8.0, rice, rational))
    return out.to(dtype)


def _uniforms(n):
    return torch._k.to_list(torch.rand(n, dtype=torch.float64)._s) if n else []


def _normals(n):
    return torch._k.to_list(torch.randn(n, dtype=torch.float64)._s) if n else []


def _gamma_scalar(a, buf_n, buf_u):
    """One Marsaglia-Tsang Gamma(a, 1) draw for a >= 1 (lists of normals and
    uniforms from torch's generator are consumed and refilled in blocks)."""
    d = a - 1.0 / 3.0
    c = 1.0 / math.sqrt(9.0 * d)
    while True:
        if not buf_n:
            buf_n.extend(_normals(64))
        z = buf_n.pop()
        v = 1.0 + c * z
        if v <= 0:
            continue
        v = v * v * v
        if not buf_u:
            buf_u.extend(_uniforms(64))
        u = buf_u.pop()
        if u < 1.0 - 0.0331 * z * z * z * z or (u > 0 and math.log(u) < 0.5 * z * z + d * (1.0 - v + math.log(v))):
            return d * v


def _patch(values, mask, fill):
    """values with the lanes where `mask` holds replaced, in order, by the
    floats fill() returns for their positions (a list of lane indices). The
    lanes are found by list.index: masked_select and nonzero are costlier."""
    count = int(mask.sum())
    if not count:
        return values
    flags = torch._k.to_list(mask._s)
    idx = []
    i = -1
    for _ in range(count):
        i = flags.index(True, i + 1)
        idx.append(i)
    return values.index_copy(0, torch.tensor(idx, dtype=torch.int64), torch.tensor(fill(idx), dtype=torch.float64))


def _gamma_draws(alphas):
    """Marsaglia-Tsang draws of Gamma(alpha, 1) for a float64 vector of
    alphas on torch's generator. The first proposal is made for every lane
    at once (it is accepted over 95% of the time); rejected lanes then draw
    one at a time. Alphas below 1 draw Gamma(alpha + 1) scaled by U^(1/alpha);
    alpha 0 gives 0 and invalid alphas nan."""
    n = alphas.numel()
    if n == 0:
        return torch.zeros(0, dtype=torch.float64)
    finite = (alphas > 0) & (alphas < inf)
    safe = torch.where(finite, alphas, torch.ones_like(alphas))
    small = safe < 1
    a = torch.where(small, safe + 1, safe)
    d = a - 1.0 / 3.0
    c = 1.0 / torch.sqrt(9.0 * d)
    z = torch.randn(n, dtype=torch.float64)
    u = torch.rand(n, dtype=torch.float64)
    v = 1.0 + c * z
    positive = v > 0
    v = torch.where(positive, v, torch.ones_like(v))
    v = v * v * v
    z2 = z * z
    accept = positive & ((u < 1.0 - 0.0331 * z2 * z2) | (torch.log(u) < 0.5 * z2 + d * (1.0 - v + torch.log(v))))

    def redraw(idx):
        a_list = torch._k.to_list(a._s)
        buf_n, buf_u = [], []
        return [_gamma_scalar(a_list[i], buf_n, buf_u) for i in idx]
    out = _patch(d * v, finite & ~accept, redraw)
    if bool(small.any()):
        out = torch.where(small, out * torch.rand(n, dtype=torch.float64).pow(1.0 / safe), out)
    out = torch.where(finite, out.clamp(min=2.2250738585072014e-308), torch.full_like(out, nan))
    # Gamma(0) is a point mass at zero (the callers clamp to their dtype's tiny).
    out = torch.where(alphas == 0, torch.zeros_like(out), out)
    return torch.where(alphas == inf, torch.full_like(out, inf), out)


def _sample_gamma_py(alphas):
    """Gamma(alpha, 1) draws for a list of alphas (a list of floats)."""
    if not alphas:
        return []
    return torch._k.to_list(_gamma_draws(torch.tensor([float(x) for x in alphas], dtype=torch.float64))._s)


def _standard_gamma(concentration):
    """Gamma(concentration, 1) samples with the implicit reparameterisation
    gradient (torch._standard_gamma)."""
    a = concentration
    with torch.no_grad():
        draws = _gamma_draws(a.detach().reshape(-1).to(torch.float64))
        out = draws.to(a.dtype).clamp(min=torch.finfo(a.dtype).tiny).reshape(tuple(a.shape))
    if torch.is_grad_enabled() and a.requires_grad:
        sample = out.detach()

        def backward(g):
            return (g * _standard_gamma_grad(a.detach(), sample),)
        node = torch._Node(backward, (a,), "StandardGamma", (a,))
        out.requires_grad = True
        out._node = node
    return out


def _sample_binomial_py(counts, probs):
    out = []
    for n, p in zip(counts, probs):
        n = int(n)
        if n <= 0 or p <= 0:
            out.append(0.0)
            continue
        if p >= 1:
            out.append(float(n))
            continue
        flip = p > 0.5
        q = 1.0 - p if flip else p
        if n * q < 30:
            # Inversion: walk the pmf from zero.
            u = _uniforms(1)[0]
            r = q / (1.0 - q)
            f = (1.0 - q) ** n
            k = 0
            while u > f and k < n:
                u -= f
                f *= r * (n - k) / (k + 1)
                k += 1
        else:
            k = sum(1 for u in _uniforms(n) if u < q)
        out.append(float(n - k if flip else k))
    return out


def _poisson_inversion(lam, u):
    """The smallest k with P(X <= k) >= u, X ~ Poisson(lam) (lam below 10)."""
    k = 0
    p = math.exp(-lam)
    F = p
    while u > F and p > 0:
        k += 1
        p *= lam / k
        F += p
    return float(k)


def _poisson_ptrs(lam, buf):
    """One draw by Hormann's transformed rejection with squeeze (lam >= 10)."""
    slam = math.sqrt(lam)
    loglam = math.log(lam)
    b = 0.931 + 2.53 * slam
    a = -0.059 + 0.02483 * b
    log_invalpha = math.log(1.1239 + 1.1328 / (b - 3.4))
    vr = 0.9277 - 3.6224 / (b - 2)
    while True:
        if len(buf) < 2:
            buf.extend(_uniforms(64))
        U = buf.pop() - 0.5
        V = buf.pop()
        us = 0.5 - abs(U)
        if us <= 0:
            continue
        k = math.floor((2 * a / us + b) * U + lam + 0.43)
        if us >= 0.07 and V <= vr:
            return float(k)
        if k < 0 or (us < 0.013 and V > us):
            continue
        if V <= 0 or math.log(V) + log_invalpha - math.log(a / (us * us) + b) <= -lam + k * loglam - math.lgamma(k + 1):
            return float(k)


def _poisson_draws(lam):
    """Poisson draws for a float64 vector of rates on torch's generator:
    inversion below rate 10, and from 10 up Hormann's transformed rejection
    with squeeze (PTRS, as NumPy's random_poisson_ptrs), whose cost does not
    grow with the rate (a product of uniforms underflows exp(-rate) near
    rate 745). Both run on all their lanes at once; lanes PTRS rejects retry
    one at a time."""
    n = lam.numel()
    out = torch.zeros(n, dtype=torch.float64)
    if n == 0:
        return out
    small = (lam > 0) & (lam < 10)
    if bool(small.any()):
        r = torch.where(small, lam, torch.zeros_like(lam))
        u = torch.rand(n, dtype=torch.float64)
        k = torch.zeros(n, dtype=torch.float64)
        p = torch.exp(-r)
        F = p
        active = small & (u > F)
        while bool(active.any()):
            k = k + active.to(torch.float64)
            p = torch.where(active, p * r / k.clamp(min=1), p)
            F = torch.where(active, F + p, F)
            active = active & (u > F) & (p > 0)
        out = torch.where(small, k, out)
    big = (lam >= 10) & (lam < inf)
    if bool(big.any()):
        L = torch.where(big, lam, torch.full_like(lam, 10.0))
        loglam = torch.log(L)
        b = 0.931 + 2.53 * torch.sqrt(L)
        a = -0.059 + 0.02483 * b
        log_invalpha = torch.log(1.1239 + 1.1328 / (b - 3.4))
        vr = 0.9277 - 3.6224 / (b - 2)
        U = torch.rand(n, dtype=torch.float64) - 0.5
        V = torch.rand(n, dtype=torch.float64)
        us = 0.5 - U.abs()
        inside = us > 0
        safe_us = torch.where(inside, us, torch.ones_like(us))
        k = torch.floor((2 * a / safe_us + b) * U + L + 0.43)
        fast = (us >= 0.07) & (V <= vr)
        reject = (k < 0) | ((us < 0.013) & (V > us))
        kk = k.clamp(min=0)
        squeeze = torch.log(V) + log_invalpha - torch.log(a / (safe_us * safe_us) + b) <= -L + kk * loglam - torch.lgamma(kk + 1)
        accept = inside & (fast | (~reject & squeeze))

        def redraw(idx):
            rates = torch._k.to_list(L._s)
            buf = []
            return [_poisson_ptrs(rates[i], buf) for i in idx]
        out = torch.where(big, _patch(k, big & ~accept, redraw), out)
    out = torch.where(lam == inf, torch.full_like(out, inf), out)
    return torch.where(lam != lam, torch.full_like(out, nan), out)


def _sample_poisson_py(rates):
    """Poisson draws for a list of rates (a list of floats)."""
    if not rates:
        return []
    return torch._k.to_list(_poisson_draws(torch.tensor([float(x) for x in rates], dtype=torch.float64))._s)


def _poisson_sample(rate):
    """torch.poisson(rate) with _sample_poisson_py's draws (any rate)."""
    with torch.no_grad():
        return _poisson_draws(rate.detach().reshape(-1).to(torch.float64)).to(rate.dtype).reshape(tuple(rate.shape))


# ---- utils ----------------------------------------------------------------------------------
def broadcast_all(*values):
    for v in values:
        if not (isinstance(v, torch.Tensor) or isinstance(v, _Number)):
            raise ValueError("Input arguments must all be instances of Number, torch.Tensor or objects implementing __torch_function__.")
    if not all(isinstance(v, torch.Tensor) for v in values):
        dtype = torch.get_default_dtype()
        for v in values:
            if isinstance(v, torch.Tensor):
                dtype = v.dtype
                break
        values = [v if isinstance(v, torch.Tensor) else torch.tensor(float(v), dtype=dtype) for v in values]
    return torch.broadcast_tensors(*values)


def _standard_normal(shape, dtype=None, device=None):
    return torch.randn(tuple(shape), dtype=dtype)


def _sum_rightmost(value, dim):
    if dim == 0:
        return value
    required_shape = tuple(value.shape[:len(value.shape) - dim]) + (-1,)
    return value.reshape(required_shape).sum(-1)


def logits_to_probs(logits, is_binary=False):
    if is_binary:
        return torch.sigmoid(logits)
    return torch.softmax(logits, dim=-1)


def clamp_probs(probs):
    eps = torch.finfo(probs.dtype).eps
    return probs.clamp(min=eps, max=1 - eps)


def probs_to_logits(probs, is_binary=False):
    ps_clamped = clamp_probs(probs)
    if is_binary:
        return torch.log(ps_clamped) - torch.log1p(-ps_clamped)
    return torch.log(ps_clamped)


class lazy_property:
    """Computed on first access (with gradients enabled) and then stored on
    the instance, as PyTorch's `lazy_property`."""

    def __init__(self, wrapped):
        self.wrapped = wrapped
        self.__name__ = getattr(wrapped, "__name__", "lazy")
        self.__doc__ = getattr(wrapped, "__doc__", None)

    def __get__(self, instance, obj_type=None):
        if instance is None:
            return self
        with torch.enable_grad():
            value = self.wrapped(instance)
        setattr(instance, self.wrapped.__name__, value)
        return value


def tril_matrix_to_vec(mat, diag=0):
    n = mat.shape[-1]
    if diag < -n or diag >= n:
        raise ValueError("diag (%d) provided is outside [%d, %d]." % (diag, -n, n - 1))
    rows = [(i, j) for i in range(n) for j in range(n) if j < i + diag + 1]
    return torch.stack([mat[..., i, j] for i, j in rows], -1)


def vec_to_tril_matrix(vec, diag=0):
    m = vec.shape[-1]
    n = (-(1 + 2 * diag) + ((1 + 2 * diag) ** 2 + 8 * m + 4 * abs(diag) * (diag + 1)) ** 0.5) / 2
    if round(n) - n > torch.finfo(vec.dtype).eps:
        raise ValueError("The size of last dimension is %d which cannot be expressed as the lower triangular part of a square D x D matrix." % m)
    n = int(round(n))
    zero = torch.zeros_like(vec[..., 0])
    entries = {}
    k = 0
    for i in range(n):
        for j in range(n):
            if j < i + diag + 1:
                entries[(i, j)] = vec[..., k]
                k += 1
    rows = [torch.stack([entries.get((i, j), zero) for j in range(n)], -1) for i in range(n)]
    return torch.stack(rows, -2)


utils = _Namespace("torch.distributions.utils")
for _name, _value in (("broadcast_all", broadcast_all), ("logits_to_probs", logits_to_probs),
                      ("clamp_probs", clamp_probs), ("probs_to_logits", probs_to_logits),
                      ("lazy_property", lazy_property), ("tril_matrix_to_vec", tril_matrix_to_vec),
                      ("vec_to_tril_matrix", vec_to_tril_matrix), ("euler_constant", euler_constant),
                      ("_standard_normal", _standard_normal), ("_sum_rightmost", _sum_rightmost)):
    setattr(utils, _name, _value)


# ---- constraints ----------------------------------------------------------------------------
class Constraint:
    __module__ = "torch.distributions.constraints"
    """A region over which a variable is valid; `check(value)` returns a
    byte tensor saying which event satisfies it."""
    is_discrete = False
    event_dim = 0

    def check(self, value):
        raise NotImplementedError

    def __repr__(self):
        return type(self).__name__[1:] + "()"


class _Dependent(Constraint):
    __module__ = "torch.distributions.constraints"
    def __init__(self, *, is_discrete=NotImplemented, event_dim=NotImplemented):
        self._is_discrete = is_discrete
        self._event_dim = event_dim

    @property
    def is_discrete(self):
        if self._is_discrete is NotImplemented:
            raise NotImplementedError(".is_discrete cannot be determined statically")
        return self._is_discrete

    @property
    def event_dim(self):
        if self._event_dim is NotImplemented:
            raise NotImplementedError(".event_dim cannot be determined statically")
        return self._event_dim

    def __call__(self, *, is_discrete=NotImplemented, event_dim=NotImplemented):
        if is_discrete is NotImplemented:
            is_discrete = self._is_discrete
        if event_dim is NotImplemented:
            event_dim = self._event_dim
        return _Dependent(is_discrete=is_discrete, event_dim=event_dim)

    def check(self, x):
        raise ValueError("Cannot determine validity of dependent constraint")


def is_dependent(constraint):
    return isinstance(constraint, _Dependent)


class _DependentProperty(_Dependent):
    """A property whose value is a constraint that depends on the instance
    (`@constraints.dependent_property`); on the class it is a dependent
    constraint, so validation skips it."""

    def __init__(self, fn=None, *, is_discrete=NotImplemented, event_dim=NotImplemented):
        self.fget = fn
        self._is_discrete = is_discrete
        self._event_dim = event_dim
        if fn is not None:
            self.__doc__ = getattr(fn, "__doc__", None)

    def __get__(self, instance, owner=None):
        if instance is None:
            return self
        if self.fget is None:
            raise AttributeError("unreadable attribute")
        return self.fget(instance)

    def __set__(self, instance, value):
        raise AttributeError("can't set attribute")

    def __call__(self, fn=None, *, is_discrete=NotImplemented, event_dim=NotImplemented):
        return _DependentProperty(fn, is_discrete=self._is_discrete, event_dim=self._event_dim)


def _dependent_property(fn=None, *, is_discrete=NotImplemented, event_dim=NotImplemented):
    return _DependentProperty(fn, is_discrete=is_discrete, event_dim=event_dim)


class _IndependentConstraint(Constraint):
    __module__ = "torch.distributions.constraints"
    def __init__(self, base_constraint, reinterpreted_batch_ndims):
        if not isinstance(base_constraint, Constraint):
            raise AssertionError("base_constraint must be a Constraint, got %s" % type(base_constraint).__name__)
        if not isinstance(reinterpreted_batch_ndims, int):
            raise AssertionError("reinterpreted_batch_ndims must be an int, got %s" % type(reinterpreted_batch_ndims).__name__)
        if reinterpreted_batch_ndims < 0:
            raise AssertionError("reinterpreted_batch_ndims must be >= 0, got %d" % reinterpreted_batch_ndims)
        self.base_constraint = base_constraint
        self.reinterpreted_batch_ndims = reinterpreted_batch_ndims

    @property
    def is_discrete(self):
        return self.base_constraint.is_discrete

    @property
    def event_dim(self):
        return self.base_constraint.event_dim + self.reinterpreted_batch_ndims

    def check(self, value):
        result = self.base_constraint.check(value)
        if result.dim() < self.reinterpreted_batch_ndims:
            expected = self.base_constraint.event_dim + self.reinterpreted_batch_ndims
            raise ValueError("Expected value.dim() >= %d but got %d" % (expected, value.dim()))
        result = result.reshape(tuple(result.shape[:result.dim() - self.reinterpreted_batch_ndims]) + (-1,))
        return result.all(-1)

    def __repr__(self):
        return "%s(%r, %d)" % (type(self).__name__[1:], self.base_constraint, self.reinterpreted_batch_ndims)


class MixtureSameFamilyConstraint(Constraint):
    __module__ = "torch.distributions.constraints"
    def __init__(self, base_constraint):
        if not isinstance(base_constraint, Constraint):
            raise AssertionError("base_constraint must be a Constraint, got %s" % type(base_constraint).__name__)
        self.base_constraint = base_constraint

    @property
    def is_discrete(self):
        return self.base_constraint.is_discrete

    @property
    def event_dim(self):
        return self.base_constraint.event_dim

    def check(self, value):
        unsqueezed_value = value.unsqueeze(-1 - self.event_dim)
        result = self.base_constraint.check(unsqueezed_value)
        if value.dim() < self.event_dim:
            raise ValueError("Expected value.dim() >= %d but got %d" % (self.event_dim, value.dim()))
        num_dim_to_keep = value.dim() - self.event_dim
        result = result.reshape(tuple(result.shape[:num_dim_to_keep]) + (-1,))
        return result.all(-1)

    def __repr__(self):
        return "%s(%r)" % (type(self).__name__, self.base_constraint)


class _Boolean(Constraint):
    __module__ = "torch.distributions.constraints"
    is_discrete = True

    def check(self, value):
        return (value == 0) | (value == 1)


class _OneHot(Constraint):
    __module__ = "torch.distributions.constraints"
    is_discrete = True
    event_dim = 1

    def check(self, value):
        is_boolean = (value == 0) | (value == 1)
        is_normalized = value.sum(-1).eq(1)
        return is_boolean.all(-1) & is_normalized


class _IntegerInterval(Constraint):
    __module__ = "torch.distributions.constraints"
    is_discrete = True

    def __init__(self, lower_bound, upper_bound):
        self.lower_bound = lower_bound
        self.upper_bound = upper_bound

    def check(self, value):
        return (value % 1 == 0) & (value >= self.lower_bound) & (value <= self.upper_bound)

    def __repr__(self):
        return "%s(lower_bound=%s, upper_bound=%s)" % (type(self).__name__[1:], _fmt_bound(self.lower_bound), _fmt_bound(self.upper_bound))


class _IntegerLessThan(Constraint):
    __module__ = "torch.distributions.constraints"
    is_discrete = True

    def __init__(self, upper_bound):
        self.upper_bound = upper_bound

    def check(self, value):
        return (value % 1 == 0) & (value <= self.upper_bound)

    def __repr__(self):
        return "%s(upper_bound=%s)" % (type(self).__name__[1:], _fmt_bound(self.upper_bound))


class _IntegerGreaterThan(Constraint):
    __module__ = "torch.distributions.constraints"
    is_discrete = True

    def __init__(self, lower_bound):
        self.lower_bound = lower_bound

    def check(self, value):
        return (value % 1 == 0) & (value >= self.lower_bound)

    def __repr__(self):
        return "%s(lower_bound=%s)" % (type(self).__name__[1:], _fmt_bound(self.lower_bound))


class _Real(Constraint):
    __module__ = "torch.distributions.constraints"
    def check(self, value):
        return value == value


class _GreaterThan(Constraint):
    __module__ = "torch.distributions.constraints"
    def __init__(self, lower_bound):
        self.lower_bound = lower_bound

    def check(self, value):
        return value > self.lower_bound

    def __repr__(self):
        return "%s(lower_bound=%s)" % (type(self).__name__[1:], _fmt_bound(self.lower_bound))


class _GreaterThanEq(Constraint):
    __module__ = "torch.distributions.constraints"
    def __init__(self, lower_bound):
        self.lower_bound = lower_bound

    def check(self, value):
        return value >= self.lower_bound

    def __repr__(self):
        return "%s(lower_bound=%s)" % (type(self).__name__[1:], _fmt_bound(self.lower_bound))


class _LessThan(Constraint):
    __module__ = "torch.distributions.constraints"
    def __init__(self, upper_bound):
        self.upper_bound = upper_bound

    def check(self, value):
        return value < self.upper_bound

    def __repr__(self):
        return "%s(upper_bound=%s)" % (type(self).__name__[1:], _fmt_bound(self.upper_bound))


class _Interval(Constraint):
    __module__ = "torch.distributions.constraints"
    def __init__(self, lower_bound, upper_bound):
        self.lower_bound = lower_bound
        self.upper_bound = upper_bound

    def check(self, value):
        return (value >= self.lower_bound) & (value <= self.upper_bound)

    def __repr__(self):
        return "%s(lower_bound=%s, upper_bound=%s)" % (type(self).__name__[1:], _fmt_bound(self.lower_bound), _fmt_bound(self.upper_bound))


class _HalfOpenInterval(Constraint):
    __module__ = "torch.distributions.constraints"
    def __init__(self, lower_bound, upper_bound):
        self.lower_bound = lower_bound
        self.upper_bound = upper_bound

    def check(self, value):
        return (value >= self.lower_bound) & (value < self.upper_bound)

    def __repr__(self):
        return "%s(lower_bound=%s, upper_bound=%s)" % (type(self).__name__[1:], _fmt_bound(self.lower_bound), _fmt_bound(self.upper_bound))


class _Simplex(Constraint):
    __module__ = "torch.distributions.constraints"
    event_dim = 1

    def check(self, value):
        return torch.all(value >= 0, dim=-1) & ((value.sum(-1) - 1).abs() < 1e-6)


class _Multinomial(Constraint):
    __module__ = "torch.distributions.constraints"
    is_discrete = True
    event_dim = 1

    def __init__(self, upper_bound):
        self.upper_bound = upper_bound

    def check(self, x):
        return (x >= 0).all(dim=-1) & (x.sum(dim=-1) <= self.upper_bound)


def _is_lower_triangular(value):
    return (value.tril() == value).reshape(tuple(value.shape[:-2]) + (-1,)).all(-1)


class _LowerTriangular(Constraint):
    __module__ = "torch.distributions.constraints"
    event_dim = 2

    def check(self, value):
        return _is_lower_triangular(value)


class _LowerCholesky(Constraint):
    __module__ = "torch.distributions.constraints"
    event_dim = 2

    def check(self, value):
        positive_diagonal = (value.diagonal(dim1=-2, dim2=-1) > 0).all(-1)
        return _is_lower_triangular(value) & positive_diagonal


class _CorrCholesky(Constraint):
    __module__ = "torch.distributions.constraints"
    event_dim = 2

    def check(self, value):
        tol = torch.finfo(value.dtype).eps * value.size(-1) * 10
        row_norm = value.detach().pow(2).sum(-1).sqrt()
        unit_row_norm = (row_norm - 1.0).abs().le(tol).all(dim=-1)
        return _LowerCholesky().check(value) & unit_row_norm


class _Square(Constraint):
    __module__ = "torch.distributions.constraints"
    event_dim = 2

    def check(self, value):
        return torch.full(tuple(value.shape[:-2]), value.shape[-2] == value.shape[-1], dtype=torch.bool)


class _Symmetric(_Square):
    __module__ = "torch.distributions.constraints"
    def check(self, value):
        square_check = _Square.check(self, value)
        if not square_check.all():
            return square_check
        return torch.isclose(value, value.mT, atol=1e-6).all(-2).all(-1)


class _PositiveSemidefinite(_Symmetric):
    __module__ = "torch.distributions.constraints"
    def check(self, value):
        sym_check = _Symmetric.check(self, value)
        if not sym_check.all():
            return sym_check
        return _eigvalsh(value).ge(0).all(-1)


class _PositiveDefinite(_Symmetric):
    __module__ = "torch.distributions.constraints"
    def check(self, value):
        sym_check = _Symmetric.check(self, value)
        if not sym_check.all():
            return sym_check
        return _cholesky_info(value).eq(0)


class _Cat(Constraint):
    __module__ = "torch.distributions.constraints"
    def __init__(self, cseq, dim=0, lengths=None):
        if not all(isinstance(c, Constraint) for c in cseq):
            raise AssertionError("All elements of cseq must be Constraint instances")
        self.cseq = list(cseq)
        if lengths is None:
            lengths = [1] * len(self.cseq)
        self.lengths = list(lengths)
        if len(self.lengths) != len(self.cseq):
            raise AssertionError("lengths (%d) must match cseq (%d)" % (len(self.lengths), len(self.cseq)))
        self.dim = dim

    @property
    def is_discrete(self):
        return any(c.is_discrete for c in self.cseq)

    @property
    def event_dim(self):
        return max(c.event_dim for c in self.cseq)

    def check(self, value):
        checks = []
        start = 0
        for constr, length in zip(self.cseq, self.lengths):
            checks.append(constr.check(value.narrow(self.dim, start, length)))
            start = start + length
        return torch.cat(checks, self.dim)


class _Stack(Constraint):
    __module__ = "torch.distributions.constraints"
    def __init__(self, cseq, dim=0):
        if not all(isinstance(c, Constraint) for c in cseq):
            raise AssertionError("All elements of cseq must be Constraint instances")
        self.cseq = list(cseq)
        self.dim = dim

    @property
    def is_discrete(self):
        return any(c.is_discrete for c in self.cseq)

    @property
    def event_dim(self):
        dim = max(c.event_dim for c in self.cseq)
        if self.dim + dim < 0:
            dim += 1
        return dim

    def check(self, value):
        vs = [value.select(self.dim, i) for i in range(value.size(self.dim))]
        return torch.stack([constr.check(v) for v, constr in zip(vs, self.cseq)], self.dim)


constraints = _Namespace("torch.distributions.constraints")
_real = _Real()
_positive = _GreaterThan(0.0)
_nonnegative = _GreaterThanEq(0.0)
_unit_interval = _Interval(0.0, 1.0)
_real_vector = _IndependentConstraint(_real, 1)
_simplex = _Simplex()
_boolean = _Boolean()
_one_hot = _OneHot()
_nonnegative_integer = _IntegerGreaterThan(0)
_lower_cholesky = _LowerCholesky()
_positive_definite = _PositiveDefinite()
_positive_semidefinite = _PositiveSemidefinite()
_corr_cholesky = _CorrCholesky()
_dependent = _Dependent()
for _name, _value in (
        ("Constraint", Constraint), ("MixtureSameFamilyConstraint", MixtureSameFamilyConstraint),
        ("dependent", _dependent), ("dependent_property", _dependent_property), ("is_dependent", is_dependent),
        ("independent", _IndependentConstraint), ("boolean", _boolean), ("one_hot", _one_hot),
        ("nonnegative_integer", _nonnegative_integer), ("positive_integer", _IntegerGreaterThan(1)),
        ("integer_interval", _IntegerInterval), ("real", _real), ("real_vector", _real_vector),
        ("positive", _positive), ("nonnegative", _nonnegative), ("greater_than", _GreaterThan),
        ("greater_than_eq", _GreaterThanEq), ("less_than", _LessThan), ("multinomial", _Multinomial),
        ("unit_interval", _unit_interval), ("interval", _Interval), ("half_open_interval", _HalfOpenInterval),
        ("simplex", _simplex), ("lower_triangular", _LowerTriangular()), ("lower_cholesky", _lower_cholesky),
        ("corr_cholesky", _corr_cholesky), ("square", _Square()), ("symmetric", _Symmetric()),
        ("positive_semidefinite", _positive_semidefinite), ("positive_definite", _positive_definite),
        ("cat", _Cat), ("stack", _Stack)):
    setattr(constraints, _name, _value)


# ---- linear algebra helpers -------------------------------------------------------------------
def _linalg(name):
    """torch.linalg's `name` when the bundled torch.linalg provides it."""
    try:
        import torch.linalg as LA
    except ImportError:
        return None
    return getattr(LA, name, None)


def _cholesky(A):
    fn = _linalg("cholesky")
    if fn is not None:
        return fn(A)
    # Column-by-column Cholesky over tensor ops (differentiable). The input
    # is symmetrized first so the gradient is PyTorch's symmetric one.
    A = 0.5 * (A + A.mT)
    n = A.shape[-1]
    cols = {}
    diag = {}
    for j in range(n):
        s = A[..., j, j]
        for k in range(j):
            s = s - cols[(j, k)] * cols[(j, k)]
        if not bool((s > 0).all()):
            raise RuntimeError("linalg.cholesky: The factorization could not be completed because the input is not positive-definite (the leading minor of order %d is not positive-definite)." % (j + 1))
        d = s.sqrt()
        diag[j] = d
        cols[(j, j)] = d
        for i in range(j + 1, n):
            t = A[..., i, j]
            for k in range(j):
                t = t - cols[(i, k)] * cols[(j, k)]
            cols[(i, j)] = t / d
    zero = torch.zeros_like(A[..., 0, 0])
    rows = [torch.stack([cols[(i, j)] if j <= i else zero for j in range(n)], -1) for i in range(n)]
    return torch.stack(rows, -2)


def _cholesky_info(A):
    fn = _linalg("cholesky_ex")
    if fn is not None:
        return fn(A).info
    n = A.shape[-1]
    flat = A.detach().reshape(-1, n, n)
    infos = []
    for b in range(flat.shape[0]):
        m = [[float(v) for v in row] for row in flat[b].tolist()]
        info = 0
        L = [[0.0] * n for _ in range(n)]
        for j in range(n):
            s = m[j][j] - sum(L[j][k] * L[j][k] for k in range(j))
            if not s > 0:
                info = j + 1
                break
            L[j][j] = math.sqrt(s)
            for i in range(j + 1, n):
                L[i][j] = (m[i][j] - sum(L[i][k] * L[j][k] for k in range(j))) / L[j][j]
        infos.append(info)
    return torch.tensor(infos, dtype=torch.int32).reshape(tuple(A.shape[:-2]))


def _eigvalsh(A):
    fn = _linalg("eigvalsh")
    if fn is not None:
        return fn(A)
    raise NotImplementedError("constraints.positive_semidefinite needs torch.linalg.eigvalsh")


def _solve_triangular_lower(L, B):
    """L^{-1} B for lower-triangular L [..., n, n] and B [..., n, k]."""
    fn = _linalg("solve_triangular")
    if fn is not None:
        return fn(L, B, upper=False)
    n = L.shape[-1]
    shape = torch.broadcast_shapes(tuple(L.shape[:-2]), tuple(B.shape[:-2]))
    L = L.expand(tuple(shape) + (n, n))
    B = B.expand(tuple(shape) + tuple(B.shape[-2:]))
    rows = []
    for i in range(n):
        r = B[..., i, :]
        for k in range(i):
            r = r - L[..., i, k].unsqueeze(-1) * rows[k]
        rows.append(r / L[..., i, i].unsqueeze(-1))
    return torch.stack(rows, -2)


# ---- Distribution ---------------------------------------------------------------------------
class Distribution:
    __module__ = "torch.distributions.distribution"
    """The abstract base class for probability distributions."""
    has_rsample = False
    has_enumerate_support = False
    _validate_args = True

    @staticmethod
    def set_default_validate_args(value):
        if value not in [True, False]:
            raise ValueError
        Distribution._validate_args = value

    def __init__(self, batch_shape=(), event_shape=(), validate_args=None):
        self._batch_shape = torch.Size(tuple(batch_shape))
        self._event_shape = torch.Size(tuple(event_shape))
        if validate_args is not None:
            self._validate_args = validate_args
        if self._validate_args:
            try:
                arg_constraints = self.arg_constraints
            except NotImplementedError:
                arg_constraints = {}
            for param, constraint in arg_constraints.items():
                if is_dependent(constraint):
                    continue
                if param not in self.__dict__ and isinstance(getattr(type(self), param, None), lazy_property):
                    continue
                value = getattr(self, param)
                valid = constraint.check(value)
                if not _all_true(valid):
                    raise ValueError("Expected parameter %s (%s of shape %s) of distribution %r to satisfy the constraint %r, but found invalid values:\n%s" % (
                        param, type(value).__name__, tuple(value.shape), self, constraint, value))

    def expand(self, batch_shape, _instance=None):
        raise NotImplementedError

    @property
    def batch_shape(self):
        return self._batch_shape

    @property
    def event_shape(self):
        return self._event_shape

    @property
    def arg_constraints(self):
        raise NotImplementedError

    @property
    def support(self):
        raise NotImplementedError

    @property
    def mean(self):
        raise NotImplementedError

    @property
    def mode(self):
        raise NotImplementedError("%s does not implement mode" % type(self))

    @property
    def variance(self):
        raise NotImplementedError

    @property
    def stddev(self):
        return self.variance.sqrt()

    def sample(self, sample_shape=()):
        with torch.no_grad():
            return self.rsample(sample_shape)

    def rsample(self, sample_shape=()):
        raise NotImplementedError

    def sample_n(self, n):
        return self.sample(torch.Size((n,)))

    def log_prob(self, value):
        raise NotImplementedError

    def cdf(self, value):
        raise NotImplementedError

    def icdf(self, value):
        raise NotImplementedError

    def enumerate_support(self, expand=True):
        raise NotImplementedError

    def entropy(self):
        raise NotImplementedError

    def perplexity(self):
        return torch.exp(self.entropy())

    def _extended_shape(self, sample_shape=()):
        return _size(tuple(sample_shape), self._batch_shape, self._event_shape)

    def _validate_sample(self, value):
        if not isinstance(value, torch.Tensor):
            raise ValueError("The value argument to log_prob must be a Tensor")
        event_dim_start = len(value.size()) - len(self._event_shape)
        if tuple(value.size()[event_dim_start:]) != tuple(self._event_shape):
            raise ValueError("The right-most size of value must match event_shape: %s vs %s." % (value.size(), self._event_shape))
        actual_shape = value.size()
        expected_shape = _size(self._batch_shape, self._event_shape)
        for i, j in zip(reversed(tuple(actual_shape)), reversed(tuple(expected_shape))):
            if i != 1 and j != 1 and i != j:
                raise ValueError("Value is not broadcastable with batch_shape+event_shape: %s vs %s." % (actual_shape, expected_shape))
        try:
            support = self.support
        except NotImplementedError:
            return
        valid = support.check(value)
        if not _all_true(valid):
            raise ValueError("Expected value argument (%s of shape %s) to be within the support (%r) of the distribution %r, but found invalid values:\n%s" % (
                type(value).__name__, tuple(value.shape), support, self, value))

    def _get_checked_instance(self, cls, _instance=None):
        if _instance is None and type(self).__init__ != cls.__init__:
            raise NotImplementedError("Subclass %s of %s that defines a custom __init__ method must also define a custom .expand() method." % (type(self).__name__, cls.__name__))
        return self.__new__(type(self)) if _instance is None else _instance

    def __repr__(self):
        try:
            names = [k for k in self.arg_constraints if k in self.__dict__]
        except NotImplementedError:
            names = []
        return type(self).__name__ + "(" + ", ".join("%s: %s" % (p, _fmt_param(self.__dict__[p])) for p in names) + ")"


class ExponentialFamily(Distribution):
    __module__ = "torch.distributions.exp_family"
    """Distributions whose density is exp(<t(x), theta> - F(theta) + k(x));
    the default entropy is the Bregman divergence of the log normalizer."""

    @property
    def _natural_params(self):
        raise NotImplementedError

    def _log_normalizer(self, *natural_params):
        raise NotImplementedError

    @property
    def _mean_carrier_measure(self):
        raise NotImplementedError

    def entropy(self):
        result = -self._mean_carrier_measure
        nparams = [p.detach().requires_grad_() for p in self._natural_params]
        with torch.enable_grad():
            lg_normal = self._log_normalizer(*nparams)
            gradients = torch._autograd_grad(lg_normal.sum(), nparams, create_graph=True)
        result = result + lg_normal
        for np_, g in zip(nparams, gradients):
            result = result - (np_ * g).reshape(tuple(self._batch_shape) + (-1,)).sum(-1)
        return result


def _scalar_batch(*params):
    return all(isinstance(p, _Number) for p in params)


# ---- continuous distributions ---------------------------------------------------------------
class Normal(ExponentialFamily):
    __module__ = "torch.distributions.normal"
    arg_constraints = {"loc": _real, "scale": _positive}
    support = _real
    has_rsample = True
    _mean_carrier_measure = 0

    def __init__(self, loc, scale, validate_args=None):
        self.loc, self.scale = broadcast_all(loc, scale)
        batch_shape = torch.Size() if _scalar_batch(loc, scale) else self.loc.size()
        Distribution.__init__(self, batch_shape, validate_args=validate_args)

    @property
    def mean(self):
        return self.loc

    @property
    def mode(self):
        return self.loc

    @property
    def stddev(self):
        return self.scale

    @property
    def variance(self):
        return self.stddev.pow(2)

    def expand(self, batch_shape, _instance=None):
        new = self._get_checked_instance(Normal, _instance)
        batch_shape = torch.Size(tuple(batch_shape))
        new.loc = self.loc.expand(batch_shape)
        new.scale = self.scale.expand(batch_shape)
        Distribution.__init__(new, batch_shape, validate_args=False)
        new._validate_args = self._validate_args
        return new

    def sample(self, sample_shape=()):
        shape = self._extended_shape(sample_shape)
        with torch.no_grad():
            return torch.normal(self.loc.expand(shape), self.scale.expand(shape))

    def rsample(self, sample_shape=()):
        shape = self._extended_shape(sample_shape)
        eps = _standard_normal(shape, dtype=self.loc.dtype)
        return self.loc + eps * self.scale

    def log_prob(self, value):
        if self._validate_args:
            self._validate_sample(value)
        var = self.scale ** 2
        log_scale = math.log(self.scale) if isinstance(self.scale, _Number) else self.scale.log()
        return -((value - self.loc) ** 2) / (2 * var) - log_scale - math.log(math.sqrt(2 * math.pi))

    def cdf(self, value):
        if self._validate_args:
            self._validate_sample(value)
        return 0.5 * (1 + torch.erf((value - self.loc) * self.scale.reciprocal() / math.sqrt(2)))

    def icdf(self, value):
        return self.loc + self.scale * torch.erfinv(2 * value - 1) * math.sqrt(2)

    def entropy(self):
        return 0.5 + 0.5 * math.log(2 * math.pi) + torch.log(self.scale)

    @property
    def _natural_params(self):
        return (self.loc / self.scale.pow(2), -0.5 * self.scale.pow(2).reciprocal())

    def _log_normalizer(self, x, y):
        return -0.25 * x.pow(2) / y + 0.5 * torch.log(-math.pi / y)


class Uniform(Distribution):
    __module__ = "torch.distributions.uniform"
    has_rsample = True

    def __init__(self, low, high, validate_args=None):
        self.low, self.high = broadcast_all(low, high)
        batch_shape = torch.Size() if _scalar_batch(low, high) else self.low.size()
        Distribution.__init__(self, batch_shape, validate_args=validate_args)

    @property
    def arg_constraints(self):
        return {"low": _LessThan(self.high), "high": _GreaterThan(self.low)}

    @property
    def mean(self):
        return (self.high + self.low) / 2

    @property
    def mode(self):
        return nan * self.high

    @property
    def stddev(self):
        return (self.high - self.low) / 12 ** 0.5

    @property
    def variance(self):
        return (self.high - self.low).pow(2) / 12

    def expand(self, batch_shape, _instance=None):
        new = self._get_checked_instance(Uniform, _instance)
        batch_shape = torch.Size(tuple(batch_shape))
        new.low = self.low.expand(batch_shape)
        new.high = self.high.expand(batch_shape)
        Distribution.__init__(new, batch_shape, validate_args=False)
        new._validate_args = self._validate_args
        return new

    @_dependent_property(is_discrete=False, event_dim=0)
    def support(self):
        return _Interval(self.low, self.high)

    def rsample(self, sample_shape=()):
        shape = self._extended_shape(sample_shape)
        rand = torch.rand(tuple(shape), dtype=self.low.dtype)
        return self.low + rand * (self.high - self.low)

    def log_prob(self, value):
        if self._validate_args:
            self._validate_sample(value)
        lb = self.low.le(value).type_as(self.low)
        ub = self.high.gt(value).type_as(self.low)
        return torch.log(lb.mul(ub)) - torch.log(self.high - self.low)

    def cdf(self, value):
        if self._validate_args:
            self._validate_sample(value)
        result = (value - self.low) / (self.high - self.low)
        return result.clamp(min=0, max=1)

    def icdf(self, value):
        return value * (self.high - self.low) + self.low

    def entropy(self):
        return torch.log(self.high - self.low)


class Exponential(ExponentialFamily):
    __module__ = "torch.distributions.exponential"
    arg_constraints = {"rate": _positive}
    support = _nonnegative
    has_rsample = True
    _mean_carrier_measure = 0

    def __init__(self, rate, validate_args=None):
        (self.rate,) = broadcast_all(rate)
        batch_shape = torch.Size() if isinstance(rate, _Number) else self.rate.size()
        Distribution.__init__(self, batch_shape, validate_args=validate_args)

    @property
    def mean(self):
        return self.rate.reciprocal()

    @property
    def mode(self):
        return torch.zeros_like(self.rate)

    @property
    def stddev(self):
        return self.rate.reciprocal()

    @property
    def variance(self):
        return self.rate.pow(-2)

    def expand(self, batch_shape, _instance=None):
        new = self._get_checked_instance(Exponential, _instance)
        batch_shape = torch.Size(tuple(batch_shape))
        new.rate = self.rate.expand(batch_shape)
        Distribution.__init__(new, batch_shape, validate_args=False)
        new._validate_args = self._validate_args
        return new

    def rsample(self, sample_shape=()):
        shape = self._extended_shape(sample_shape)
        return torch.empty(tuple(shape), dtype=self.rate.dtype).exponential_() / self.rate

    def log_prob(self, value):
        if self._validate_args:
            self._validate_sample(value)
        return self.rate.log() - self.rate * value

    def cdf(self, value):
        if self._validate_args:
            self._validate_sample(value)
        return 1 - torch.exp(-self.rate * value)

    def icdf(self, value):
        return -torch.log1p(-value) / self.rate

    def entropy(self):
        return 1.0 - torch.log(self.rate)

    @property
    def _natural_params(self):
        return (-self.rate,)

    def _log_normalizer(self, x):
        return -torch.log(-x)


class Laplace(Distribution):
    __module__ = "torch.distributions.laplace"
    arg_constraints = {"loc": _real, "scale": _positive}
    support = _real
    has_rsample = True

    def __init__(self, loc, scale, validate_args=None):
        self.loc, self.scale = broadcast_all(loc, scale)
        batch_shape = torch.Size() if _scalar_batch(loc, scale) else self.loc.size()
        Distribution.__init__(self, batch_shape, validate_args=validate_args)

    @property
    def mean(self):
        return self.loc

    @property
    def mode(self):
        return self.loc

    @property
    def variance(self):
        return 2 * self.scale.pow(2)

    @property
    def stddev(self):
        return 2 ** 0.5 * self.scale

    def expand(self, batch_shape, _instance=None):
        new = self._get_checked_instance(Laplace, _instance)
        batch_shape = torch.Size(tuple(batch_shape))
        new.loc = self.loc.expand(batch_shape)
        new.scale = self.scale.expand(batch_shape)
        Distribution.__init__(new, batch_shape, validate_args=False)
        new._validate_args = self._validate_args
        return new

    def rsample(self, sample_shape=()):
        shape = self._extended_shape(sample_shape)
        finfo = torch.finfo(self.loc.dtype)
        u = torch.empty(tuple(shape), dtype=self.loc.dtype).uniform_(finfo.eps - 1, 1)
        return self.loc - self.scale * u.sign() * torch.log1p(-u.abs())

    def log_prob(self, value):
        if self._validate_args:
            self._validate_sample(value)
        return -torch.log(2 * self.scale) - torch.abs(value - self.loc) / self.scale

    def cdf(self, value):
        if self._validate_args:
            self._validate_sample(value)
        return 0.5 - 0.5 * (value - self.loc).sign() * torch.expm1(-(value - self.loc).abs() / self.scale)

    def icdf(self, value):
        term = value - 0.5
        return self.loc - self.scale * term.sign() * torch.log1p(-2 * term.abs())

    def entropy(self):
        return 1 + torch.log(2 * self.scale)


class Cauchy(Distribution):
    __module__ = "torch.distributions.cauchy"
    arg_constraints = {"loc": _real, "scale": _positive}
    support = _real
    has_rsample = True

    def __init__(self, loc, scale, validate_args=None):
        self.loc, self.scale = broadcast_all(loc, scale)
        batch_shape = torch.Size() if _scalar_batch(loc, scale) else self.loc.size()
        Distribution.__init__(self, batch_shape, validate_args=validate_args)

    def expand(self, batch_shape, _instance=None):
        new = self._get_checked_instance(Cauchy, _instance)
        batch_shape = torch.Size(tuple(batch_shape))
        new.loc = self.loc.expand(batch_shape)
        new.scale = self.scale.expand(batch_shape)
        Distribution.__init__(new, batch_shape, validate_args=False)
        new._validate_args = self._validate_args
        return new

    @property
    def mean(self):
        return torch.full(tuple(self._extended_shape()), nan, dtype=self.loc.dtype)

    @property
    def mode(self):
        return self.loc

    @property
    def variance(self):
        return torch.full(tuple(self._extended_shape()), inf, dtype=self.loc.dtype)

    def rsample(self, sample_shape=()):
        shape = self._extended_shape(sample_shape)
        eps = torch.empty(tuple(shape), dtype=self.loc.dtype).cauchy_()
        return self.loc + eps * self.scale

    def log_prob(self, value):
        if self._validate_args:
            self._validate_sample(value)
        return -math.log(math.pi) - self.scale.log() - (((value - self.loc) / self.scale) ** 2).log1p()

    def cdf(self, value):
        if self._validate_args:
            self._validate_sample(value)
        return torch.atan((value - self.loc) / self.scale) / math.pi + 0.5

    def icdf(self, value):
        return torch.tan(math.pi * (value - 0.5)) * self.scale + self.loc

    def entropy(self):
        return math.log(4 * math.pi) + self.scale.log()


class Gamma(ExponentialFamily):
    __module__ = "torch.distributions.gamma"
    arg_constraints = {"concentration": _positive, "rate": _positive}
    support = _nonnegative
    has_rsample = True
    _mean_carrier_measure = 0

    def __init__(self, concentration, rate, validate_args=None):
        self.concentration, self.rate = broadcast_all(concentration, rate)
        batch_shape = torch.Size() if _scalar_batch(concentration, rate) else self.concentration.size()
        Distribution.__init__(self, batch_shape, validate_args=validate_args)

    @property
    def mean(self):
        return self.concentration / self.rate

    @property
    def mode(self):
        return ((self.concentration - 1) / self.rate).clamp(min=0)

    @property
    def variance(self):
        return self.concentration / self.rate.pow(2)

    def expand(self, batch_shape, _instance=None):
        new = self._get_checked_instance(Gamma, _instance)
        batch_shape = torch.Size(tuple(batch_shape))
        new.concentration = self.concentration.expand(batch_shape)
        new.rate = self.rate.expand(batch_shape)
        Distribution.__init__(new, batch_shape, validate_args=False)
        new._validate_args = self._validate_args
        return new

    def rsample(self, sample_shape=()):
        shape = self._extended_shape(sample_shape)
        value = _standard_gamma(self.concentration.expand(shape)) / self.rate.expand(shape)
        value.data.clamp_(min=torch.finfo(value.dtype).tiny)
        return value

    def log_prob(self, value):
        value = torch.as_tensor(value, dtype=self.rate.dtype)
        if self._validate_args:
            self._validate_sample(value)
        return _xlogy(self.concentration, self.rate) + _xlogy(self.concentration - 1, value) - self.rate * value - _lgamma(self.concentration)

    def entropy(self):
        return self.concentration - torch.log(self.rate) + _lgamma(self.concentration) + (1.0 - self.concentration) * _digamma(self.concentration)

    @property
    def _natural_params(self):
        return (self.concentration - 1, -self.rate)

    def _log_normalizer(self, x, y):
        return _lgamma(x + 1) + (x + 1) * torch.log(-y.reciprocal())

    def cdf(self, value):
        if self._validate_args:
            self._validate_sample(value)
        return _gammainc(self.concentration, self.rate * value)


class Chi2(Gamma):
    __module__ = "torch.distributions.chi2"
    arg_constraints = {"df": _positive}

    def __init__(self, df, validate_args=None):
        Gamma.__init__(self, 0.5 * df, 0.5, validate_args=validate_args)

    def expand(self, batch_shape, _instance=None):
        new = self._get_checked_instance(Chi2, _instance)
        return Gamma.expand(self, batch_shape, new)

    @property
    def df(self):
        return self.concentration * 2


class Dirichlet(ExponentialFamily):
    __module__ = "torch.distributions.dirichlet"
    arg_constraints = {"concentration": _IndependentConstraint(_positive, 1)}
    support = _simplex
    has_rsample = True

    def __init__(self, concentration, validate_args=None):
        if concentration.dim() < 1:
            raise ValueError("`concentration` parameter must be at least one-dimensional.")
        self.concentration = concentration
        Distribution.__init__(self, concentration.shape[:-1], concentration.shape[-1:], validate_args=validate_args)

    def expand(self, batch_shape, _instance=None):
        new = self._get_checked_instance(Dirichlet, _instance)
        batch_shape = torch.Size(tuple(batch_shape))
        new.concentration = self.concentration.expand(_size(batch_shape, self.event_shape))
        Distribution.__init__(new, batch_shape, self.event_shape, validate_args=False)
        new._validate_args = self._validate_args
        return new

    def rsample(self, sample_shape=()):
        shape = self._extended_shape(sample_shape)
        concentration = self.concentration.expand(shape)
        gammas = _standard_gamma(concentration)
        return gammas / gammas.sum(-1, True)

    def log_prob(self, value):
        if self._validate_args:
            self._validate_sample(value)
        return _xlogy(self.concentration - 1.0, value).sum(-1) + _lgamma(self.concentration.sum(-1)) - _lgamma(self.concentration).sum(-1)

    @property
    def mean(self):
        return self.concentration / self.concentration.sum(-1, True)

    @property
    def mode(self):
        concentrationm1 = (self.concentration - 1).clamp(min=0.0)
        mode = concentrationm1 / concentrationm1.sum(-1, True)
        mask = (self.concentration < 1).all(dim=-1)
        k = concentrationm1.shape[-1]
        hot = torch.nn.functional.one_hot(mode.argmax(dim=-1), k).to(mode.dtype)
        return torch.where(mask.unsqueeze(-1), hot, mode)

    @property
    def variance(self):
        con0 = self.concentration.sum(-1, True)
        return self.concentration * (con0 - self.concentration) / (con0.pow(2) * (con0 + 1))

    def entropy(self):
        k = self.concentration.size(-1)
        a0 = self.concentration.sum(-1)
        return _lgamma(self.concentration).sum(-1) - _lgamma(a0) - (k - a0) * _digamma(a0) - ((self.concentration - 1.0) * _digamma(self.concentration)).sum(-1)

    @property
    def _natural_params(self):
        return (self.concentration,)

    def _log_normalizer(self, x):
        return _lgamma(x).sum(-1) - _lgamma(x.sum(-1))


class Beta(ExponentialFamily):
    __module__ = "torch.distributions.beta"
    arg_constraints = {"concentration1": _positive, "concentration0": _positive}
    support = _unit_interval
    has_rsample = True

    def __init__(self, concentration1, concentration0, validate_args=None):
        if _scalar_batch(concentration1, concentration0):
            c1c0 = torch.tensor([float(concentration1), float(concentration0)])
        else:
            concentration1, concentration0 = broadcast_all(concentration1, concentration0)
            c1c0 = torch.stack([concentration1, concentration0], -1)
        self._dirichlet = Dirichlet(c1c0, validate_args=validate_args)
        Distribution.__init__(self, self._dirichlet._batch_shape, validate_args=validate_args)

    def expand(self, batch_shape, _instance=None):
        new = self._get_checked_instance(Beta, _instance)
        batch_shape = torch.Size(tuple(batch_shape))
        new._dirichlet = self._dirichlet.expand(batch_shape)
        Distribution.__init__(new, batch_shape, validate_args=False)
        new._validate_args = self._validate_args
        return new

    @property
    def mean(self):
        return self.concentration1 / (self.concentration1 + self.concentration0)

    @property
    def mode(self):
        return self._dirichlet.mode[..., 0]

    @property
    def variance(self):
        total = self.concentration1 + self.concentration0
        return self.concentration1 * self.concentration0 / (total.pow(2) * (total + 1))

    def rsample(self, sample_shape=()):
        return self._dirichlet.rsample(sample_shape).select(-1, 0)

    def log_prob(self, value):
        if self._validate_args:
            self._validate_sample(value)
        heads_tails = torch.stack([value, 1.0 - value], -1)
        return self._dirichlet.log_prob(heads_tails)

    def entropy(self):
        return self._dirichlet.entropy()

    @property
    def concentration1(self):
        return self._dirichlet.concentration[..., 0]

    @property
    def concentration0(self):
        return self._dirichlet.concentration[..., 1]

    @property
    def _natural_params(self):
        return (self.concentration1, self.concentration0)

    def _log_normalizer(self, x, y):
        return _lgamma(x) + _lgamma(y) - _lgamma(x + y)


class StudentT(Distribution):
    __module__ = "torch.distributions.studentT"
    arg_constraints = {"df": _positive, "loc": _real, "scale": _positive}
    support = _real
    has_rsample = True

    def __init__(self, df, loc=0.0, scale=1.0, validate_args=None):
        self.df, self.loc, self.scale = broadcast_all(df, loc, scale)
        self._chi2 = Chi2(self.df)
        Distribution.__init__(self, self.df.size(), validate_args=validate_args)

    @property
    def mean(self):
        return torch.where(self.df <= 1, torch.full_like(self.loc, nan), self.loc)

    @property
    def mode(self):
        return self.loc

    @property
    def variance(self):
        df = self.df
        big = df > 2
        safe = torch.where(big, df, torch.full_like(df, 3.0))
        m = self.scale.pow(2) * safe / (safe - 2)
        m = torch.where(big, m, torch.full_like(df, inf))
        return torch.where(df <= 1, torch.full_like(df, nan), m)

    def expand(self, batch_shape, _instance=None):
        new = self._get_checked_instance(StudentT, _instance)
        batch_shape = torch.Size(tuple(batch_shape))
        new.df = self.df.expand(batch_shape)
        new.loc = self.loc.expand(batch_shape)
        new.scale = self.scale.expand(batch_shape)
        new._chi2 = self._chi2.expand(batch_shape)
        Distribution.__init__(new, batch_shape, validate_args=False)
        new._validate_args = self._validate_args
        return new

    def rsample(self, sample_shape=()):
        shape = self._extended_shape(sample_shape)
        X = _standard_normal(shape, dtype=self.df.dtype)
        Z = self._chi2.rsample(sample_shape)
        Y = X * torch.rsqrt(Z / self.df)
        return self.loc + self.scale * Y

    def log_prob(self, value):
        if self._validate_args:
            self._validate_sample(value)
        y = (value - self.loc) / self.scale
        Z = self.scale.log() + 0.5 * self.df.log() + 0.5 * math.log(math.pi) + _lgamma(0.5 * self.df) - _lgamma(0.5 * (self.df + 1.0))
        return -0.5 * (self.df + 1.0) * torch.log1p(y ** 2.0 / self.df) - Z

    def entropy(self):
        lbeta = _lgamma(0.5 * self.df) + math.lgamma(0.5) - _lgamma(0.5 * (self.df + 1))
        return self.scale.log() + 0.5 * (self.df + 1) * (_digamma(0.5 * (self.df + 1)) - _digamma(0.5 * self.df)) + 0.5 * self.df.log() + lbeta


# ---- discrete distributions -------------------------------------------------------------------
class Bernoulli(ExponentialFamily):
    __module__ = "torch.distributions.bernoulli"
    arg_constraints = {"probs": _unit_interval, "logits": _real}
    support = _boolean
    has_enumerate_support = True
    _mean_carrier_measure = 0

    def __init__(self, probs=None, logits=None, validate_args=None):
        if (probs is None) == (logits is None):
            raise ValueError("Either `probs` or `logits` must be specified, but not both.")
        if probs is not None:
            is_scalar = isinstance(probs, _Number)
            (self.probs,) = broadcast_all(probs)
        else:
            is_scalar = isinstance(logits, _Number)
            (self.logits,) = broadcast_all(logits)
        self._param = self.probs if probs is not None else self.logits
        batch_shape = torch.Size() if is_scalar else self._param.size()
        Distribution.__init__(self, batch_shape, validate_args=validate_args)

    def expand(self, batch_shape, _instance=None):
        new = self._get_checked_instance(Bernoulli, _instance)
        batch_shape = torch.Size(tuple(batch_shape))
        if "probs" in self.__dict__:
            new.probs = self.probs.expand(batch_shape)
            new._param = new.probs
        if "logits" in self.__dict__:
            new.logits = self.logits.expand(batch_shape)
            new._param = new.logits
        Distribution.__init__(new, batch_shape, validate_args=False)
        new._validate_args = self._validate_args
        return new

    @property
    def mean(self):
        return self.probs

    @property
    def mode(self):
        mode = (self.probs >= 0.5).to(self.probs.dtype)
        return torch.where(self.probs == 0.5, torch.full_like(mode, nan), mode)

    @property
    def variance(self):
        return self.probs * (1 - self.probs)

    @lazy_property
    def logits(self):
        return probs_to_logits(self.probs, is_binary=True)

    @lazy_property
    def probs(self):
        return logits_to_probs(self.logits, is_binary=True)

    @property
    def param_shape(self):
        return self._param.size()

    def sample(self, sample_shape=()):
        shape = self._extended_shape(sample_shape)
        with torch.no_grad():
            return torch.bernoulli(self.probs.expand(shape))

    def log_prob(self, value):
        if self._validate_args:
            self._validate_sample(value)
        logits, value = broadcast_all(self.logits, value)
        return -torch.nn.functional.binary_cross_entropy_with_logits(logits, value, reduction="none")

    def entropy(self):
        return torch.nn.functional.binary_cross_entropy_with_logits(self.logits, self.probs, reduction="none")

    def enumerate_support(self, expand=True):
        values = torch.arange(2, dtype=self._param.dtype)
        values = values.view((-1,) + (1,) * len(self._batch_shape))
        if expand:
            values = values.expand((-1,) + tuple(self._batch_shape))
        return values

    @property
    def _natural_params(self):
        return (torch.log(self.probs) - torch.log1p(-self.probs),)

    def _log_normalizer(self, x):
        return torch.log1p(torch.exp(x))


class Geometric(Distribution):
    __module__ = "torch.distributions.geometric"
    arg_constraints = {"probs": _unit_interval, "logits": _real}
    support = _nonnegative_integer

    def __init__(self, probs=None, logits=None, validate_args=None):
        if (probs is None) == (logits is None):
            raise ValueError("Either `probs` or `logits` must be specified, but not both.")
        if probs is not None:
            (self.probs,) = broadcast_all(probs)
        else:
            (self.logits,) = broadcast_all(logits)
        probs_or_logits = probs if probs is not None else logits
        batch_shape = torch.Size() if isinstance(probs_or_logits, _Number) else probs_or_logits.size()
        Distribution.__init__(self, batch_shape, validate_args=validate_args)
        if self._validate_args and probs is not None:
            value = self.probs
            valid = value > 0
            if not valid.all():
                raise ValueError("Expected parameter probs (%s of shape %s) of distribution %r to be positive but found invalid values:\n%s" % (
                    type(value).__name__, tuple(value.shape), self, value.data[~valid]))

    def expand(self, batch_shape, _instance=None):
        new = self._get_checked_instance(Geometric, _instance)
        batch_shape = torch.Size(tuple(batch_shape))
        if "probs" in self.__dict__:
            new.probs = self.probs.expand(batch_shape)
        if "logits" in self.__dict__:
            new.logits = self.logits.expand(batch_shape)
        Distribution.__init__(new, batch_shape, validate_args=False)
        new._validate_args = self._validate_args
        return new

    @property
    def mean(self):
        return 1.0 / self.probs - 1.0

    @property
    def mode(self):
        return torch.zeros_like(self.probs)

    @property
    def variance(self):
        return (1.0 / self.probs - 1.0) / self.probs

    @lazy_property
    def logits(self):
        return probs_to_logits(self.probs, is_binary=True)

    @lazy_property
    def probs(self):
        return logits_to_probs(self.logits, is_binary=True)

    def sample(self, sample_shape=()):
        shape = self._extended_shape(sample_shape)
        tiny = torch.finfo(self.probs.dtype).tiny
        with torch.no_grad():
            u = torch.empty(tuple(shape), dtype=self.probs.dtype).uniform_(tiny, 1)
            return (u.log() / (-self.probs).log1p()).floor()

    def log_prob(self, value):
        if self._validate_args:
            self._validate_sample(value)
        value, probs = broadcast_all(value, self.probs)
        probs = torch.where((probs == 1) & (value == 0), torch.zeros_like(probs), probs)
        return value * (-probs).log1p() + self.probs.log()

    def entropy(self):
        return torch.nn.functional.binary_cross_entropy_with_logits(self.logits, self.probs, reduction="none") / self.probs


class Poisson(ExponentialFamily):
    __module__ = "torch.distributions.poisson"
    arg_constraints = {"rate": _nonnegative}
    support = _nonnegative_integer

    def __init__(self, rate, validate_args=None):
        (self.rate,) = broadcast_all(rate)
        batch_shape = torch.Size() if isinstance(rate, _Number) else self.rate.size()
        Distribution.__init__(self, batch_shape, validate_args=validate_args)

    @property
    def mean(self):
        return self.rate

    @property
    def mode(self):
        return self.rate.floor()

    @property
    def variance(self):
        return self.rate

    def expand(self, batch_shape, _instance=None):
        new = self._get_checked_instance(Poisson, _instance)
        batch_shape = torch.Size(tuple(batch_shape))
        new.rate = self.rate.expand(batch_shape)
        Distribution.__init__(new, batch_shape, validate_args=False)
        new._validate_args = self._validate_args
        return new

    def sample(self, sample_shape=()):
        shape = self._extended_shape(sample_shape)
        with torch.no_grad():
            return _poisson_sample(self.rate.expand(shape))

    def log_prob(self, value):
        if self._validate_args:
            self._validate_sample(value)
        rate, value = broadcast_all(self.rate, value)
        return _xlogy(value, rate) - rate - _lgamma(value + 1)

    @property
    def _natural_params(self):
        return (torch.log(self.rate),)

    def _log_normalizer(self, x):
        return torch.exp(x)


def _clamp_by_zero(x):
    return (x.clamp(min=0) + x - x.clamp(max=0)) / 2


class Binomial(Distribution):
    __module__ = "torch.distributions.binomial"
    arg_constraints = {"total_count": _nonnegative_integer, "probs": _unit_interval, "logits": _real}
    has_enumerate_support = True

    def __init__(self, total_count=1, probs=None, logits=None, validate_args=None):
        if (probs is None) == (logits is None):
            raise ValueError("Either `probs` or `logits` must be specified, but not both.")
        if probs is not None:
            self.total_count, self.probs = broadcast_all(total_count, probs)
            self.total_count = self.total_count.type_as(self.probs)
        else:
            self.total_count, self.logits = broadcast_all(total_count, logits)
            self.total_count = self.total_count.type_as(self.logits)
        self._param = self.probs if probs is not None else self.logits
        Distribution.__init__(self, self._param.size(), validate_args=validate_args)

    def expand(self, batch_shape, _instance=None):
        new = self._get_checked_instance(Binomial, _instance)
        batch_shape = torch.Size(tuple(batch_shape))
        new.total_count = self.total_count.expand(batch_shape)
        if "probs" in self.__dict__:
            new.probs = self.probs.expand(batch_shape)
            new._param = new.probs
        if "logits" in self.__dict__:
            new.logits = self.logits.expand(batch_shape)
            new._param = new.logits
        Distribution.__init__(new, batch_shape, validate_args=False)
        new._validate_args = self._validate_args
        return new

    @_dependent_property(is_discrete=True, event_dim=0)
    def support(self):
        return _IntegerInterval(0, self.total_count)

    @property
    def mean(self):
        return self.total_count * self.probs

    @property
    def mode(self):
        return torch.minimum(((self.total_count + 1) * self.probs).floor(), self.total_count)

    @property
    def variance(self):
        return self.total_count * self.probs * (1 - self.probs)

    @lazy_property
    def logits(self):
        return probs_to_logits(self.probs, is_binary=True)

    @lazy_property
    def probs(self):
        return logits_to_probs(self.logits, is_binary=True)

    @property
    def param_shape(self):
        return self._param.size()

    def sample(self, sample_shape=()):
        shape = self._extended_shape(sample_shape)
        with torch.no_grad():
            n = torch._k.to_list(self.total_count.expand(shape).contiguous().reshape(-1)._s) if _numel(shape) else []
            p = torch._k.to_list((self.probs.expand(shape).reshape(-1) * 1)._s) if _numel(shape) else []
            vals = _sample_binomial_py(n, p)
            return torch.tensor(vals, dtype=self.probs.dtype).reshape(tuple(shape))

    def log_prob(self, value):
        if self._validate_args:
            self._validate_sample(value)
        log_factorial_n = _lgamma(self.total_count + 1)
        log_factorial_k = _lgamma(value + 1)
        log_factorial_nmk = _lgamma(self.total_count - value + 1)
        normalize_term = self.total_count * _clamp_by_zero(self.logits) + self.total_count * torch.log1p(torch.exp(-torch.abs(self.logits))) - log_factorial_n
        return value * self.logits - log_factorial_k - log_factorial_nmk - normalize_term

    def entropy(self):
        total_count = int(self.total_count.max())
        if not self.total_count.min() == total_count:
            raise NotImplementedError("Inhomogeneous total count not supported by `entropy`.")
        log_prob = self.log_prob(self.enumerate_support(False))
        return -(torch.exp(log_prob) * log_prob).sum(0)

    def enumerate_support(self, expand=True):
        total_count = int(self.total_count.max())
        if not self.total_count.min() == total_count:
            raise NotImplementedError("Inhomogeneous total count not supported by `enumerate_support`.")
        values = torch.arange(1 + total_count, dtype=self._param.dtype)
        values = values.view((-1,) + (1,) * len(self._batch_shape))
        if expand:
            values = values.expand((-1,) + tuple(self._batch_shape))
        return values


class Categorical(Distribution):
    __module__ = "torch.distributions.categorical"
    arg_constraints = {"probs": _simplex, "logits": _real_vector}
    has_enumerate_support = True

    def __init__(self, probs=None, logits=None, validate_args=None):
        if (probs is None) == (logits is None):
            raise ValueError("Either `probs` or `logits` must be specified, but not both.")
        if probs is not None:
            if probs.dim() < 1:
                raise ValueError("`probs` parameter must be at least one-dimensional.")
            self.probs = probs / probs.sum(-1, keepdim=True)
        else:
            if logits.dim() < 1:
                raise ValueError("`logits` parameter must be at least one-dimensional.")
            self.logits = logits - logits.logsumexp(dim=-1, keepdim=True)
        self._param = self.probs if probs is not None else self.logits
        self._num_events = self._param.size()[-1]
        batch_shape = self._param.size()[:-1] if self._param.dim() > 1 else torch.Size()
        Distribution.__init__(self, batch_shape, validate_args=validate_args)

    def expand(self, batch_shape, _instance=None):
        new = self._get_checked_instance(Categorical, _instance)
        batch_shape = torch.Size(tuple(batch_shape))
        param_shape = _size(batch_shape, (self._num_events,))
        if "probs" in self.__dict__:
            new.probs = self.probs.expand(param_shape)
            new._param = new.probs
        if "logits" in self.__dict__:
            new.logits = self.logits.expand(param_shape)
            new._param = new.logits
        new._num_events = self._num_events
        Distribution.__init__(new, batch_shape, validate_args=False)
        new._validate_args = self._validate_args
        return new

    @_dependent_property(is_discrete=True, event_dim=0)
    def support(self):
        return _IntegerInterval(0, self._num_events - 1)

    @lazy_property
    def logits(self):
        return probs_to_logits(self.probs)

    @lazy_property
    def probs(self):
        return logits_to_probs(self.logits)

    @property
    def param_shape(self):
        return self._param.size()

    @property
    def mean(self):
        return torch.full(tuple(self._extended_shape()), nan, dtype=self.probs.dtype)

    @property
    def mode(self):
        return self.probs.argmax(dim=-1)

    @property
    def variance(self):
        return torch.full(tuple(self._extended_shape()), nan, dtype=self.probs.dtype)

    def sample(self, sample_shape=()):
        sample_shape = torch.Size(tuple(sample_shape))
        with torch.no_grad():
            probs_2d = self.probs.reshape(-1, self._num_events)
            n = _numel(sample_shape)
            if n == 0:
                return torch.zeros(tuple(self._extended_shape(sample_shape)), dtype=torch.int64)
            samples_2d = torch.multinomial(probs_2d, n, True).T
            return samples_2d.reshape(tuple(self._extended_shape(sample_shape)))

    def log_prob(self, value):
        if self._validate_args:
            self._validate_sample(value)
        value = value.long().unsqueeze(-1)
        value, log_pmf = torch.broadcast_tensors(value, self.logits)
        value = value[..., :1]
        return log_pmf.gather(-1, value).squeeze(-1)

    def entropy(self):
        min_real = torch.finfo(self.logits.dtype).min
        logits = torch.clamp(self.logits, min=min_real)
        p_log_p = logits * self.probs
        return -p_log_p.sum(-1)

    def enumerate_support(self, expand=True):
        values = torch.arange(self._num_events, dtype=torch.int64)
        values = values.view((-1,) + (1,) * len(self._batch_shape))
        if expand:
            values = values.expand((-1,) + tuple(self._batch_shape))
        return values


class OneHotCategorical(Distribution):
    __module__ = "torch.distributions.one_hot_categorical"
    arg_constraints = {"probs": _simplex, "logits": _real_vector}
    support = _one_hot
    has_enumerate_support = True

    def __init__(self, probs=None, logits=None, validate_args=None):
        self._categorical = Categorical(probs, logits)
        batch_shape = self._categorical.batch_shape
        event_shape = self._categorical.param_shape[-1:]
        Distribution.__init__(self, batch_shape, event_shape, validate_args=validate_args)

    def expand(self, batch_shape, _instance=None):
        new = self._get_checked_instance(OneHotCategorical, _instance)
        batch_shape = torch.Size(tuple(batch_shape))
        new._categorical = self._categorical.expand(batch_shape)
        Distribution.__init__(new, batch_shape, self.event_shape, validate_args=False)
        new._validate_args = self._validate_args
        return new

    @property
    def _param(self):
        return self._categorical._param

    @property
    def probs(self):
        return self._categorical.probs

    @property
    def logits(self):
        return self._categorical.logits

    @property
    def mean(self):
        return self._categorical.probs

    @property
    def mode(self):
        probs = self._categorical.probs
        mode = probs.argmax(dim=-1)
        return torch.nn.functional.one_hot(mode, num_classes=probs.shape[-1]).to(probs.dtype)

    @property
    def variance(self):
        return self._categorical.probs * (1 - self._categorical.probs)

    @property
    def param_shape(self):
        return self._categorical.param_shape

    def sample(self, sample_shape=()):
        probs = self._categorical.probs
        indices = self._categorical.sample(sample_shape)
        return torch.nn.functional.one_hot(indices, self._categorical._num_events).to(probs.dtype)

    def log_prob(self, value):
        if self._validate_args:
            self._validate_sample(value)
        indices = value.max(-1)[1]
        return self._categorical.log_prob(indices)

    def entropy(self):
        return self._categorical.entropy()

    def enumerate_support(self, expand=True):
        n = self.event_shape[0]
        values = torch.eye(n, dtype=self._param.dtype)
        values = values.view((n,) + (1,) * len(self.batch_shape) + (n,))
        if expand:
            values = values.expand((n,) + tuple(self.batch_shape) + (n,))
        return values


class OneHotCategoricalStraightThrough(OneHotCategorical):
    __module__ = "torch.distributions.one_hot_categorical"
    has_rsample = True

    def rsample(self, sample_shape=()):
        samples = self.sample(sample_shape)
        probs = self._categorical.probs
        return samples + (probs - probs.detach())


class Multinomial(Distribution):
    __module__ = "torch.distributions.multinomial"
    arg_constraints = {"probs": _simplex, "logits": _real_vector}

    def __init__(self, total_count=1, probs=None, logits=None, validate_args=None):
        if not isinstance(total_count, int):
            raise NotImplementedError("inhomogeneous total_count is not supported")
        self.total_count = total_count
        self._categorical = Categorical(probs=probs, logits=logits)
        self._binomial = Binomial(total_count=total_count, probs=self.probs)
        batch_shape = self._categorical.batch_shape
        event_shape = self._categorical.param_shape[-1:]
        Distribution.__init__(self, batch_shape, event_shape, validate_args=validate_args)

    @property
    def mean(self):
        return self.probs * self.total_count

    @property
    def variance(self):
        return self.total_count * self.probs * (1 - self.probs)

    def expand(self, batch_shape, _instance=None):
        new = self._get_checked_instance(Multinomial, _instance)
        batch_shape = torch.Size(tuple(batch_shape))
        new.total_count = self.total_count
        new._categorical = self._categorical.expand(batch_shape)
        Distribution.__init__(new, batch_shape, self.event_shape, validate_args=False)
        new._validate_args = self._validate_args
        return new

    @_dependent_property(is_discrete=True, event_dim=1)
    def support(self):
        return _Multinomial(self.total_count)

    @property
    def logits(self):
        return self._categorical.logits

    @property
    def probs(self):
        return self._categorical.probs

    @property
    def param_shape(self):
        return self._categorical.param_shape

    def sample(self, sample_shape=()):
        sample_shape = torch.Size(tuple(sample_shape))
        samples = self._categorical.sample(_size((self.total_count,), sample_shape))
        shifted_idx = list(range(samples.dim()))
        shifted_idx.append(shifted_idx.pop(0))
        samples = samples.permute(*shifted_idx)
        counts = torch.zeros(tuple(self._extended_shape(sample_shape)), dtype=torch.int64)
        counts.scatter_add_(-1, samples, torch.ones_like(samples))
        return counts.type_as(self.probs)

    def entropy(self):
        # PyTorch's n is an int64 tensor, whose lgamma is float32.
        n = torch.tensor(self.total_count)
        cat_entropy = self._categorical.entropy()
        term1 = n * cat_entropy - _lgamma(torch.tensor(float(self.total_count) + 1, dtype=torch.float32))
        support = self._binomial.enumerate_support(expand=False)[1:]
        binomial_probs = torch.exp(self._binomial.log_prob(support))
        weights = _lgamma(support + 1)
        term2 = (binomial_probs * weights).sum([0, -1])
        return term1 + term2

    def log_prob(self, value):
        if self._validate_args:
            self._validate_sample(value)
        logits, value = broadcast_all(self.logits, value)
        log_factorial_n = _lgamma(value.sum(-1) + 1)
        log_factorial_xs = _lgamma(value + 1).sum(-1)
        logits = torch.where((value == 0) & (logits == -inf), torch.zeros_like(logits), logits)
        log_powers = (logits * value).sum(-1)
        return log_factorial_n - log_factorial_xs + log_powers


# ---- multivariate normal ----------------------------------------------------------------------
def _batch_mv(bmat, bvec):
    return torch.matmul(bmat, bvec.unsqueeze(-1)).squeeze(-1)


def _batch_mahalanobis(bL, bx):
    """x^T (L L^T)^{-1} x for x [..., n] with L [..., n, n] broadcast."""
    return _solve_triangular_lower(bL, bx.unsqueeze(-1)).squeeze(-1).pow(2).sum(-1)


def _precision_to_scale_tril(P):
    Lf = _cholesky(torch.flip(P, (-2, -1)))
    L_inv = torch.transpose(torch.flip(Lf, (-2, -1)), -2, -1)
    Id = torch.eye(P.shape[-1], dtype=P.dtype)
    return _solve_triangular_lower(L_inv, Id)


def _batch_trace_XXT(bmat):
    n = bmat.size(-1)
    m = bmat.size(-2)
    flat_trace = bmat.reshape(-1, m * n).pow(2).sum(-1)
    return flat_trace.reshape(tuple(bmat.shape[:-2]))


class MultivariateNormal(Distribution):
    __module__ = "torch.distributions.multivariate_normal"
    arg_constraints = {"loc": _real_vector, "covariance_matrix": _positive_definite,
                       "precision_matrix": _positive_definite, "scale_tril": _lower_cholesky}
    support = _real_vector
    has_rsample = True

    def __init__(self, loc, covariance_matrix=None, precision_matrix=None, scale_tril=None, validate_args=None):
        if loc.dim() < 1:
            raise ValueError("loc must be at least one-dimensional.")
        if (covariance_matrix is not None) + (scale_tril is not None) + (precision_matrix is not None) != 1:
            raise ValueError("Exactly one of covariance_matrix or precision_matrix or scale_tril may be specified.")
        if scale_tril is not None:
            if scale_tril.dim() < 2:
                raise ValueError("scale_tril matrix must be at least two-dimensional, with optional leading batch dimensions")
            batch_shape = torch.broadcast_shapes(tuple(scale_tril.shape[:-2]), tuple(loc.shape[:-1]))
            self.scale_tril = scale_tril.expand(tuple(batch_shape) + (-1, -1))
        elif covariance_matrix is not None:
            if covariance_matrix.dim() < 2:
                raise ValueError("covariance_matrix must be at least two-dimensional, with optional leading batch dimensions")
            batch_shape = torch.broadcast_shapes(tuple(covariance_matrix.shape[:-2]), tuple(loc.shape[:-1]))
            self.covariance_matrix = covariance_matrix.expand(tuple(batch_shape) + (-1, -1))
        else:
            if precision_matrix.dim() < 2:
                raise ValueError("precision_matrix must be at least two-dimensional, with optional leading batch dimensions")
            batch_shape = torch.broadcast_shapes(tuple(precision_matrix.shape[:-2]), tuple(loc.shape[:-1]))
            self.precision_matrix = precision_matrix.expand(tuple(batch_shape) + (-1, -1))
        self.loc = loc.expand(tuple(batch_shape) + (-1,))
        Distribution.__init__(self, batch_shape, self.loc.shape[-1:], validate_args=validate_args)
        if scale_tril is not None:
            self._unbroadcasted_scale_tril = scale_tril
        elif covariance_matrix is not None:
            self._unbroadcasted_scale_tril = _cholesky(covariance_matrix)
        else:
            self._unbroadcasted_scale_tril = _precision_to_scale_tril(precision_matrix)

    def expand(self, batch_shape, _instance=None):
        new = self._get_checked_instance(MultivariateNormal, _instance)
        batch_shape = torch.Size(tuple(batch_shape))
        loc_shape = _size(batch_shape, self.event_shape)
        cov_shape = _size(batch_shape, self.event_shape, self.event_shape)
        new.loc = self.loc.expand(loc_shape)
        new._unbroadcasted_scale_tril = self._unbroadcasted_scale_tril
        if "covariance_matrix" in self.__dict__:
            new.covariance_matrix = self.covariance_matrix.expand(cov_shape)
        if "scale_tril" in self.__dict__:
            new.scale_tril = self.scale_tril.expand(cov_shape)
        if "precision_matrix" in self.__dict__:
            new.precision_matrix = self.precision_matrix.expand(cov_shape)
        Distribution.__init__(new, batch_shape, self.event_shape, validate_args=False)
        new._validate_args = self._validate_args
        return new

    @lazy_property
    def scale_tril(self):
        return self._unbroadcasted_scale_tril.expand(_size(self._batch_shape, self._event_shape, self._event_shape))

    @lazy_property
    def covariance_matrix(self):
        L = self._unbroadcasted_scale_tril
        return torch.matmul(L, L.mT).expand(_size(self._batch_shape, self._event_shape, self._event_shape))

    @lazy_property
    def precision_matrix(self):
        L = self._unbroadcasted_scale_tril
        Linv = _solve_triangular_lower(L, torch.eye(L.shape[-1], dtype=L.dtype))
        return torch.matmul(Linv.mT, Linv).expand(_size(self._batch_shape, self._event_shape, self._event_shape))

    @property
    def mean(self):
        return self.loc

    @property
    def mode(self):
        return self.loc

    @property
    def variance(self):
        return self._unbroadcasted_scale_tril.pow(2).sum(-1).expand(_size(self._batch_shape, self._event_shape))

    def rsample(self, sample_shape=()):
        shape = self._extended_shape(sample_shape)
        eps = _standard_normal(shape, dtype=self.loc.dtype)
        return self.loc + _batch_mv(self._unbroadcasted_scale_tril, eps)

    def log_prob(self, value):
        if self._validate_args:
            self._validate_sample(value)
        diff = value - self.loc
        M = _batch_mahalanobis(self._unbroadcasted_scale_tril, diff)
        half_log_det = self._unbroadcasted_scale_tril.diagonal(dim1=-2, dim2=-1).log().sum(-1)
        return -0.5 * (self._event_shape[0] * math.log(2 * math.pi) + M) - half_log_det

    def entropy(self):
        half_log_det = self._unbroadcasted_scale_tril.diagonal(dim1=-2, dim2=-1).log().sum(-1)
        H = 0.5 * self._event_shape[0] * (1.0 + math.log(2 * math.pi)) + half_log_det
        if len(self._batch_shape) == 0:
            return H
        return H.expand(self._batch_shape)


class LowRankMultivariateNormal(Distribution):
    __module__ = "torch.distributions.lowrank_multivariate_normal"
    """N(loc, W W^T + diag(D)) with W = cov_factor [..., n, r], D = cov_diag."""
    arg_constraints = {"loc": _real_vector, "cov_factor": _IndependentConstraint(_real, 2),
                       "cov_diag": _IndependentConstraint(_positive, 1)}
    support = _real_vector
    has_rsample = True

    def __init__(self, loc, cov_factor, cov_diag, validate_args=None):
        if loc.dim() < 1:
            raise ValueError("loc must be at least one-dimensional.")
        event_shape = loc.shape[-1:]
        if cov_factor.dim() < 2:
            raise ValueError("cov_factor must be at least two-dimensional, with optional leading batch dimensions")
        if tuple(cov_factor.shape[-2:-1]) != tuple(event_shape):
            raise ValueError("cov_factor must be a batch of matrices with shape %d x m" % event_shape[0])
        if tuple(cov_diag.shape[-1:]) != tuple(event_shape):
            raise ValueError("cov_diag must be a batch of vectors with shape %s" % (event_shape,))
        loc_ = loc.unsqueeze(-1)
        cov_diag_ = cov_diag.unsqueeze(-1)
        loc_, self.cov_factor, cov_diag_ = torch.broadcast_tensors(loc_, cov_factor, cov_diag_)
        self.loc = loc_[..., 0]
        self.cov_diag = cov_diag_[..., 0]
        batch_shape = self.loc.shape[:-1]
        self._unbroadcasted_cov_factor = cov_factor
        self._unbroadcasted_cov_diag = cov_diag
        self._capacitance_tril = _batch_capacitance_tril(cov_factor, cov_diag)
        Distribution.__init__(self, batch_shape, event_shape, validate_args=validate_args)

    def expand(self, batch_shape, _instance=None):
        new = self._get_checked_instance(LowRankMultivariateNormal, _instance)
        batch_shape = torch.Size(tuple(batch_shape))
        loc_shape = _size(batch_shape, self.event_shape)
        new.loc = self.loc.expand(loc_shape)
        new.cov_diag = self.cov_diag.expand(loc_shape)
        new.cov_factor = self.cov_factor.expand(_size(loc_shape, self.cov_factor.shape[-1:]))
        new._unbroadcasted_cov_factor = self._unbroadcasted_cov_factor
        new._unbroadcasted_cov_diag = self._unbroadcasted_cov_diag
        new._capacitance_tril = self._capacitance_tril
        Distribution.__init__(new, batch_shape, self.event_shape, validate_args=False)
        new._validate_args = self._validate_args
        return new

    @property
    def mean(self):
        return self.loc

    @property
    def mode(self):
        return self.loc

    @lazy_property
    def variance(self):
        return (self._unbroadcasted_cov_factor.pow(2).sum(-1) + self._unbroadcasted_cov_diag).expand(_size(self._batch_shape, self._event_shape))

    @lazy_property
    def covariance_matrix(self):
        W = self._unbroadcasted_cov_factor
        cov = torch.matmul(W, W.mT) + torch.diag_embed(self._unbroadcasted_cov_diag)
        return cov.expand(_size(self._batch_shape, self._event_shape, self._event_shape))

    @lazy_property
    def scale_tril(self):
        return _cholesky(self.covariance_matrix)

    @lazy_property
    def precision_matrix(self):
        L = _cholesky(self.covariance_matrix)
        Linv = _solve_triangular_lower(L, torch.eye(L.shape[-1], dtype=L.dtype))
        return torch.matmul(Linv.mT, Linv)

    def rsample(self, sample_shape=()):
        shape = self._extended_shape(sample_shape)
        W_shape = tuple(shape[:-1]) + tuple(self.cov_factor.shape[-1:])
        eps_W = _standard_normal(W_shape, dtype=self.loc.dtype)
        eps_D = _standard_normal(shape, dtype=self.loc.dtype)
        return self.loc + _batch_mv(self._unbroadcasted_cov_factor, eps_W) + self._unbroadcasted_cov_diag.sqrt() * eps_D

    def log_prob(self, value):
        if self._validate_args:
            self._validate_sample(value)
        diff = value - self.loc
        M = _batch_lowrank_mahalanobis(self._unbroadcasted_cov_factor, self._unbroadcasted_cov_diag, diff, self._capacitance_tril)
        log_det = _batch_lowrank_logdet(self._unbroadcasted_cov_factor, self._unbroadcasted_cov_diag, self._capacitance_tril)
        return -0.5 * (self._event_shape[0] * math.log(2 * math.pi) + log_det + M)

    def entropy(self):
        log_det = _batch_lowrank_logdet(self._unbroadcasted_cov_factor, self._unbroadcasted_cov_diag, self._capacitance_tril)
        H = 0.5 * (self._event_shape[0] * (1.0 + math.log(2 * math.pi)) + log_det)
        if len(self._batch_shape) == 0:
            return H
        return H.expand(self._batch_shape)


def _batch_capacitance_tril(W, D):
    m = W.size(-1)
    Wt_Dinv = W.mT / D.unsqueeze(-2)
    K = torch.matmul(Wt_Dinv, W)
    K = K + torch.eye(m, dtype=W.dtype)
    return _cholesky(K)


def _batch_lowrank_logdet(W, D, capacitance_tril):
    return 2 * capacitance_tril.diagonal(dim1=-2, dim2=-1).log().sum(-1) + D.log().sum(-1)


def _batch_lowrank_mahalanobis(W, D, x, capacitance_tril):
    Wt_Dinv = W.mT / D.unsqueeze(-2)
    Wt_Dinv_x = _batch_mv(Wt_Dinv, x)
    mahalanobis_term1 = (x.pow(2) / D).sum(-1)
    mahalanobis_term2 = _batch_mahalanobis(capacitance_tril, Wt_Dinv_x)
    return mahalanobis_term1 - mahalanobis_term2


# ---- Independent, mixtures ------------------------------------------------------------------
class Independent(Distribution):
    __module__ = "torch.distributions.independent"
    arg_constraints = {}

    def __init__(self, base_distribution, reinterpreted_batch_ndims, validate_args=None):
        if reinterpreted_batch_ndims > len(base_distribution.batch_shape):
            raise ValueError("Expected reinterpreted_batch_ndims <= len(base_distribution.batch_shape), actual %d vs %d" % (reinterpreted_batch_ndims, len(base_distribution.batch_shape)))
        shape = _size(base_distribution.batch_shape, base_distribution.event_shape)
        event_dim = reinterpreted_batch_ndims + len(base_distribution.event_shape)
        batch_shape = shape[:len(shape) - event_dim]
        event_shape = shape[len(shape) - event_dim:]
        self.base_dist = base_distribution
        self.reinterpreted_batch_ndims = reinterpreted_batch_ndims
        Distribution.__init__(self, batch_shape, event_shape, validate_args=validate_args)

    def expand(self, batch_shape, _instance=None):
        new = self._get_checked_instance(Independent, _instance)
        batch_shape = torch.Size(tuple(batch_shape))
        new.base_dist = self.base_dist.expand(_size(batch_shape, self.event_shape[:self.reinterpreted_batch_ndims]))
        new.reinterpreted_batch_ndims = self.reinterpreted_batch_ndims
        Distribution.__init__(new, batch_shape, self.event_shape, validate_args=False)
        new._validate_args = self._validate_args
        return new

    @property
    def has_rsample(self):
        return self.base_dist.has_rsample

    @property
    def has_enumerate_support(self):
        if self.reinterpreted_batch_ndims > 0:
            return False
        return self.base_dist.has_enumerate_support

    @_dependent_property
    def support(self):
        result = self.base_dist.support
        if self.reinterpreted_batch_ndims:
            result = _IndependentConstraint(result, self.reinterpreted_batch_ndims)
        return result

    @property
    def mean(self):
        return self.base_dist.mean

    @property
    def mode(self):
        return self.base_dist.mode

    @property
    def variance(self):
        return self.base_dist.variance

    def sample(self, sample_shape=()):
        return self.base_dist.sample(sample_shape)

    def rsample(self, sample_shape=()):
        return self.base_dist.rsample(sample_shape)

    def log_prob(self, value):
        return _sum_rightmost(self.base_dist.log_prob(value), self.reinterpreted_batch_ndims)

    def entropy(self):
        return _sum_rightmost(self.base_dist.entropy(), self.reinterpreted_batch_ndims)

    def enumerate_support(self, expand=True):
        if self.reinterpreted_batch_ndims > 0:
            raise NotImplementedError("Enumeration over cartesian product is not implemented")
        return self.base_dist.enumerate_support(expand=expand)

    def __repr__(self):
        return "%s(%r, %d)" % (type(self).__name__, self.base_dist, self.reinterpreted_batch_ndims)


class MixtureSameFamily(Distribution):
    __module__ = "torch.distributions.mixture_same_family"
    arg_constraints = {}
    has_rsample = False

    def __init__(self, mixture_distribution, component_distribution, validate_args=None):
        self._mixture_distribution = mixture_distribution
        self._component_distribution = component_distribution
        if not isinstance(self._mixture_distribution, Categorical):
            raise ValueError(" The Mixture distribution needs to be an  instance of torch.distributions.Categorical")
        if not isinstance(self._component_distribution, Distribution):
            raise ValueError("The Component distribution need to be an instance of torch.distributions.Distribution")
        mdbs = self._mixture_distribution.batch_shape
        cdbs = self._component_distribution.batch_shape[:-1]
        for size1, size2 in zip(reversed(tuple(mdbs)), reversed(tuple(cdbs))):
            if size1 != 1 and size2 != 1 and size1 != size2:
                raise ValueError("`mixture_distribution.batch_shape` (%s) is not compatible with `component_distribution.batch_shape`(%s)" % (mdbs, cdbs))
        km = self._mixture_distribution.logits.shape[-1]
        kc = self._component_distribution.batch_shape[-1]
        if km != kc:
            raise ValueError("`mixture_distribution component` (%d) does not equal `component_distribution.batch_shape[-1]` (%d)" % (km, kc))
        self._num_component = km
        event_shape = self._component_distribution.event_shape
        self._event_ndims = len(event_shape)
        Distribution.__init__(self, cdbs, event_shape, validate_args=validate_args)

    def expand(self, batch_shape, _instance=None):
        batch_shape = torch.Size(tuple(batch_shape))
        batch_shape_comp = _size(batch_shape, (self._num_component,))
        new = self._get_checked_instance(MixtureSameFamily, _instance)
        new._component_distribution = self._component_distribution.expand(batch_shape_comp)
        new._mixture_distribution = self._mixture_distribution.expand(batch_shape)
        new._num_component = self._num_component
        new._event_ndims = self._event_ndims
        Distribution.__init__(new, batch_shape, new._component_distribution.event_shape, validate_args=False)
        new._validate_args = self._validate_args
        return new

    @_dependent_property
    def support(self):
        return MixtureSameFamilyConstraint(self._component_distribution.support)

    @property
    def mixture_distribution(self):
        return self._mixture_distribution

    @property
    def component_distribution(self):
        return self._component_distribution

    @property
    def mean(self):
        probs = self._pad_mixture_dimensions(self.mixture_distribution.probs)
        return torch.sum(probs * self.component_distribution.mean, dim=-1 - self._event_ndims)

    @property
    def variance(self):
        probs = self._pad_mixture_dimensions(self.mixture_distribution.probs)
        mean_cond_var = torch.sum(probs * self.component_distribution.variance, dim=-1 - self._event_ndims)
        var_cond_mean = torch.sum(probs * (self.component_distribution.mean - self._pad(self.mean)).pow(2.0), dim=-1 - self._event_ndims)
        return mean_cond_var + var_cond_mean

    def cdf(self, x):
        x = self._pad(x)
        cdf_x = self.component_distribution.cdf(x)
        mix_prob = self.mixture_distribution.probs
        return torch.sum(cdf_x * mix_prob, dim=-1)

    def log_prob(self, x):
        if self._validate_args:
            self._validate_sample(x)
        x = self._pad(x)
        log_prob_x = self.component_distribution.log_prob(x)
        log_mix_prob = torch.log_softmax(self.mixture_distribution.logits, dim=-1)
        return torch.logsumexp(log_prob_x + log_mix_prob, dim=-1)

    def sample(self, sample_shape=()):
        with torch.no_grad():
            sample_len = len(sample_shape)
            batch_len = len(self.batch_shape)
            gather_dim = sample_len + batch_len
            es = tuple(self.event_shape)
            mix_sample = self.mixture_distribution.sample(sample_shape)
            mix_shape = tuple(mix_sample.shape)
            comp_samples = self.component_distribution.sample(sample_shape)
            mix_sample_r = mix_sample.reshape(mix_shape + (1,) * (len(es) + 1))
            mix_sample_r = mix_sample_r.repeat(*((1,) * len(mix_shape) + (1,) + es))
            samples = torch.gather(comp_samples, gather_dim, mix_sample_r)
            return samples.squeeze(gather_dim)

    def _pad(self, x):
        return x.unsqueeze(-1 - self._event_ndims)

    def _pad_mixture_dimensions(self, x):
        dist_batch_ndims = len(self.batch_shape)
        cat_batch_ndims = len(self.mixture_distribution.batch_shape)
        pad_ndims = 0 if cat_batch_ndims == 1 else dist_batch_ndims - cat_batch_ndims
        xs = tuple(x.shape)
        return x.reshape(xs[:-1] + (1,) * pad_ndims + xs[-1:] + (1,) * self._event_ndims)

    def __repr__(self):
        return "MixtureSameFamily(\n  %r,\n  %r)" % (self.mixture_distribution, self.component_distribution)


# ---- transforms -----------------------------------------------------------------------------
class Transform:
    __module__ = "torch.distributions.transforms"
    """An invertible map with a computable log|det J|; `inv` is its inverse."""
    bijective = False

    def __init__(self, cache_size=0):
        self._cache_size = cache_size
        self._inv = None
        if cache_size == 0:
            pass
        elif cache_size == 1:
            self._cached_x_y = (None, None)
        else:
            raise ValueError("cache_size must be 0 or 1")

    @property
    def event_dim(self):
        if self.domain.event_dim == self.codomain.event_dim:
            return self.domain.event_dim
        raise ValueError("Please use either .domain.event_dim or .codomain.event_dim")

    @property
    def inv(self):
        inv = self._inv
        if inv is None:
            inv = _InverseTransform(self)
            self._inv = inv
        return inv

    @property
    def sign(self):
        raise NotImplementedError

    def with_cache(self, cache_size=1):
        if self._cache_size == cache_size:
            return self
        if type(self).__init__ is Transform.__init__:
            return type(self)(cache_size=cache_size)
        raise NotImplementedError("%s.with_cache is not implemented" % type(self))

    def __eq__(self, other):
        return self is other

    def __ne__(self, other):
        return not self.__eq__(other)

    def __hash__(self):
        return id(self)

    def __call__(self, x):
        if self._cache_size == 0:
            return self._call(x)
        x_old, y_old = self._cached_x_y
        if x is x_old:
            return y_old
        y = self._call(x)
        self._cached_x_y = (x, y)
        return y

    def _inv_call(self, y):
        if self._cache_size == 0:
            return self._inverse(y)
        x_old, y_old = self._cached_x_y
        if y is y_old:
            return x_old
        x = self._inverse(y)
        self._cached_x_y = (x, y)
        return x

    def _call(self, x):
        raise NotImplementedError

    def _inverse(self, y):
        raise NotImplementedError

    def log_abs_det_jacobian(self, x, y):
        raise NotImplementedError

    def __repr__(self):
        return type(self).__name__ + "()"

    def forward_shape(self, shape):
        return shape

    def inverse_shape(self, shape):
        return shape


class _InverseTransform(Transform):
    __module__ = "torch.distributions.transforms"
    def __init__(self, transform):
        Transform.__init__(self, cache_size=transform._cache_size)
        self._inv = transform

    @_dependent_property(is_discrete=False)
    def domain(self):
        return self._inv.codomain

    @_dependent_property(is_discrete=False)
    def codomain(self):
        return self._inv.domain

    @property
    def bijective(self):
        return self._inv.bijective

    @property
    def sign(self):
        return self._inv.sign

    @property
    def inv(self):
        return self._inv

    def with_cache(self, cache_size=1):
        return self.inv.with_cache(cache_size).inv

    def __eq__(self, other):
        if not isinstance(other, _InverseTransform):
            return False
        return self._inv == other._inv

    def __hash__(self):
        return id(self)

    def __repr__(self):
        return "%s(%r)" % (type(self).__name__, self._inv)

    def __call__(self, x):
        return self._inv._inv_call(x)

    def log_abs_det_jacobian(self, x, y):
        return -self._inv.log_abs_det_jacobian(y, x)

    def forward_shape(self, shape):
        return self._inv.inverse_shape(shape)

    def inverse_shape(self, shape):
        return self._inv.forward_shape(shape)


class ComposeTransform(Transform):
    __module__ = "torch.distributions.transforms"
    def __init__(self, parts, cache_size=0):
        if cache_size:
            parts = [part.with_cache(cache_size) for part in parts]
        Transform.__init__(self, cache_size=cache_size)
        self.parts = parts

    def __eq__(self, other):
        if not isinstance(other, ComposeTransform):
            return False
        return self.parts == other.parts

    def __hash__(self):
        return id(self)

    @_dependent_property(is_discrete=False)
    def domain(self):
        if not self.parts:
            return _real
        domain = self.parts[0].domain
        event_dim = self.parts[-1].codomain.event_dim
        for part in reversed(self.parts):
            event_dim += part.domain.event_dim - part.codomain.event_dim
            event_dim = max(event_dim, part.domain.event_dim)
        if event_dim > domain.event_dim:
            domain = _IndependentConstraint(domain, event_dim - domain.event_dim)
        return domain

    @_dependent_property(is_discrete=False)
    def codomain(self):
        if not self.parts:
            return _real
        codomain = self.parts[-1].codomain
        event_dim = self.parts[0].domain.event_dim
        for part in self.parts:
            event_dim += part.codomain.event_dim - part.domain.event_dim
            event_dim = max(event_dim, part.codomain.event_dim)
        if event_dim > codomain.event_dim:
            codomain = _IndependentConstraint(codomain, event_dim - codomain.event_dim)
        return codomain

    @property
    def bijective(self):
        return all(p.bijective for p in self.parts)

    @property
    def sign(self):
        sign = 1
        for p in self.parts:
            sign = sign * p.sign
        return sign

    @property
    def inv(self):
        inv = self._inv
        if inv is None:
            inv = ComposeTransform([p.inv for p in reversed(self.parts)])
            self._inv = inv
            inv._inv = self
        return inv

    def with_cache(self, cache_size=1):
        if self._cache_size == cache_size:
            return self
        return ComposeTransform(self.parts, cache_size=cache_size)

    def __call__(self, x):
        for part in self.parts:
            x = part(x)
        return x

    def log_abs_det_jacobian(self, x, y):
        if not self.parts:
            return torch.zeros_like(x)
        xs = [x]
        for part in self.parts[:-1]:
            xs.append(part(xs[-1]))
        xs.append(y)
        terms = []
        event_dim = self.domain.event_dim
        for part, x_, y_ in zip(self.parts, xs[:-1], xs[1:]):
            terms.append(_sum_rightmost(part.log_abs_det_jacobian(x_, y_), event_dim - part.domain.event_dim))
            event_dim += part.codomain.event_dim - part.domain.event_dim
        total = terms[0]
        for t in terms[1:]:
            total = total + t
        return total

    def forward_shape(self, shape):
        for part in self.parts:
            shape = part.forward_shape(shape)
        return shape

    def inverse_shape(self, shape):
        for part in reversed(self.parts):
            shape = part.inverse_shape(shape)
        return shape

    def __repr__(self):
        return type(self).__name__ + "(\n    " + ",\n    ".join([repr(p) for p in self.parts]) + "\n)"


identity_transform = ComposeTransform([])


class IndependentTransform(Transform):
    __module__ = "torch.distributions.transforms"
    def __init__(self, base_transform, reinterpreted_batch_ndims, cache_size=0):
        Transform.__init__(self, cache_size=cache_size)
        self.base_transform = base_transform.with_cache(cache_size)
        self.reinterpreted_batch_ndims = reinterpreted_batch_ndims

    def with_cache(self, cache_size=1):
        if self._cache_size == cache_size:
            return self
        return IndependentTransform(self.base_transform, self.reinterpreted_batch_ndims, cache_size=cache_size)

    @_dependent_property(is_discrete=False)
    def domain(self):
        return _IndependentConstraint(self.base_transform.domain, self.reinterpreted_batch_ndims)

    @_dependent_property(is_discrete=False)
    def codomain(self):
        return _IndependentConstraint(self.base_transform.codomain, self.reinterpreted_batch_ndims)

    @property
    def bijective(self):
        return self.base_transform.bijective

    @property
    def sign(self):
        return self.base_transform.sign

    def _call(self, x):
        if x.dim() < self.domain.event_dim:
            raise ValueError("Too few dimensions on input")
        return self.base_transform(x)

    def _inverse(self, y):
        if y.dim() < self.codomain.event_dim:
            raise ValueError("Too few dimensions on input")
        return self.base_transform.inv(y)

    def log_abs_det_jacobian(self, x, y):
        return _sum_rightmost(self.base_transform.log_abs_det_jacobian(x, y), self.reinterpreted_batch_ndims)

    def __repr__(self):
        return "%s(%r, %d)" % (type(self).__name__, self.base_transform, self.reinterpreted_batch_ndims)

    def forward_shape(self, shape):
        return self.base_transform.forward_shape(shape)

    def inverse_shape(self, shape):
        return self.base_transform.inverse_shape(shape)


class ReshapeTransform(Transform):
    __module__ = "torch.distributions.transforms"
    bijective = True

    def __init__(self, in_shape, out_shape, cache_size=0):
        self.in_shape = torch.Size(tuple(in_shape))
        self.out_shape = torch.Size(tuple(out_shape))
        if _numel(self.in_shape) != _numel(self.out_shape):
            raise ValueError("in_shape, out_shape have different numbers of elements")
        Transform.__init__(self, cache_size=cache_size)

    @_dependent_property
    def domain(self):
        return _IndependentConstraint(_real, len(self.in_shape))

    @_dependent_property
    def codomain(self):
        return _IndependentConstraint(_real, len(self.out_shape))

    def with_cache(self, cache_size=1):
        if self._cache_size == cache_size:
            return self
        return ReshapeTransform(self.in_shape, self.out_shape, cache_size=cache_size)

    def _call(self, x):
        batch_shape = tuple(x.shape[:x.dim() - len(self.in_shape)])
        return x.reshape(batch_shape + tuple(self.out_shape))

    def _inverse(self, y):
        batch_shape = tuple(y.shape[:y.dim() - len(self.out_shape)])
        return y.reshape(batch_shape + tuple(self.in_shape))

    def log_abs_det_jacobian(self, x, y):
        batch_shape = tuple(x.shape[:x.dim() - len(self.in_shape)])
        return torch.zeros(batch_shape, dtype=x.dtype)

    def forward_shape(self, shape):
        if len(shape) < len(self.in_shape):
            raise ValueError("Too few dimensions on input")
        cut = len(shape) - len(self.in_shape)
        if tuple(shape[cut:]) != tuple(self.in_shape):
            raise ValueError("Shape mismatch: expected %s but got %s" % (shape[cut:], self.in_shape))
        return _size(shape[:cut], self.out_shape)

    def inverse_shape(self, shape):
        if len(shape) < len(self.out_shape):
            raise ValueError("Too few dimensions on input")
        cut = len(shape) - len(self.out_shape)
        if tuple(shape[cut:]) != tuple(self.out_shape):
            raise ValueError("Shape mismatch: expected %s but got %s" % (shape[cut:], self.out_shape))
        return _size(shape[:cut], self.in_shape)


class ExpTransform(Transform):
    __module__ = "torch.distributions.transforms"
    domain = _real
    codomain = _positive
    bijective = True
    sign = +1

    def __eq__(self, other):
        return isinstance(other, ExpTransform)

    def __hash__(self):
        return id(self)

    def _call(self, x):
        return x.exp()

    def _inverse(self, y):
        return y.log()

    def log_abs_det_jacobian(self, x, y):
        return x


class PowerTransform(Transform):
    __module__ = "torch.distributions.transforms"
    domain = _positive
    codomain = _positive
    bijective = True

    def __init__(self, exponent, cache_size=0):
        Transform.__init__(self, cache_size=cache_size)
        (self.exponent,) = broadcast_all(exponent)

    def with_cache(self, cache_size=1):
        if self._cache_size == cache_size:
            return self
        return PowerTransform(self.exponent, cache_size=cache_size)

    @property
    def sign(self):
        return self.exponent.sign()

    def __eq__(self, other):
        if not isinstance(other, PowerTransform):
            return False
        return bool(self.exponent.eq(other.exponent).all().item())

    def __hash__(self):
        return id(self)

    def _call(self, x):
        return x.pow(self.exponent)

    def _inverse(self, y):
        return y.pow(1 / self.exponent)

    def log_abs_det_jacobian(self, x, y):
        return (self.exponent * y / x).abs().log()

    def forward_shape(self, shape):
        return torch.broadcast_shapes(tuple(shape), tuple(getattr(self.exponent, "shape", ())))

    def inverse_shape(self, shape):
        return torch.broadcast_shapes(tuple(shape), tuple(getattr(self.exponent, "shape", ())))


def _clipped_sigmoid(x):
    finfo = torch.finfo(x.dtype)
    return torch.clamp(torch.sigmoid(x), min=finfo.tiny, max=1.0 - finfo.eps)


class SigmoidTransform(Transform):
    __module__ = "torch.distributions.transforms"
    domain = _real
    codomain = _unit_interval
    bijective = True
    sign = +1

    def __eq__(self, other):
        return isinstance(other, SigmoidTransform)

    def __hash__(self):
        return id(self)

    def _call(self, x):
        return _clipped_sigmoid(x)

    def _inverse(self, y):
        finfo = torch.finfo(y.dtype)
        y = y.clamp(min=finfo.tiny, max=1.0 - finfo.eps)
        return y.log() - (-y).log1p()

    def log_abs_det_jacobian(self, x, y):
        return -_softplus(-x) - _softplus(x)


class SoftplusTransform(Transform):
    __module__ = "torch.distributions.transforms"
    domain = _real
    codomain = _positive
    bijective = True
    sign = +1

    def __eq__(self, other):
        return isinstance(other, SoftplusTransform)

    def __hash__(self):
        return id(self)

    def _call(self, x):
        return _softplus(x)

    def _inverse(self, y):
        return (-y).expm1().neg().log() + y

    def log_abs_det_jacobian(self, x, y):
        return -_softplus(-x)


class TanhTransform(Transform):
    __module__ = "torch.distributions.transforms"
    domain = _real
    codomain = _Interval(-1.0, 1.0)
    bijective = True
    sign = +1

    def __eq__(self, other):
        return isinstance(other, TanhTransform)

    def __hash__(self):
        return id(self)

    def _call(self, x):
        return x.tanh()

    def _inverse(self, y):
        return torch.atanh(y)

    def log_abs_det_jacobian(self, x, y):
        return 2.0 * (math.log(2.0) - x - _softplus(-2.0 * x))


class AbsTransform(Transform):
    __module__ = "torch.distributions.transforms"
    domain = _real
    codomain = _positive

    def __eq__(self, other):
        return isinstance(other, AbsTransform)

    def __hash__(self):
        return id(self)

    def _call(self, x):
        return x.abs()

    def _inverse(self, y):
        return y


class AffineTransform(Transform):
    __module__ = "torch.distributions.transforms"
    bijective = True

    def __init__(self, loc, scale, event_dim=0, cache_size=0):
        Transform.__init__(self, cache_size=cache_size)
        self.loc = loc
        self.scale = scale
        self._event_dim = event_dim

    @property
    def event_dim(self):
        return self._event_dim

    @_dependent_property(is_discrete=False)
    def domain(self):
        if self.event_dim == 0:
            return _real
        return _IndependentConstraint(_real, self.event_dim)

    @_dependent_property(is_discrete=False)
    def codomain(self):
        if self.event_dim == 0:
            return _real
        return _IndependentConstraint(_real, self.event_dim)

    def with_cache(self, cache_size=1):
        if self._cache_size == cache_size:
            return self
        return AffineTransform(self.loc, self.scale, self.event_dim, cache_size=cache_size)

    def __eq__(self, other):
        if not isinstance(other, AffineTransform):
            return False
        if isinstance(self.loc, _Number) and isinstance(other.loc, _Number):
            if self.loc != other.loc:
                return False
        elif not bool((self.loc == other.loc).all()):
            return False
        if isinstance(self.scale, _Number) and isinstance(other.scale, _Number):
            if self.scale != other.scale:
                return False
        elif not bool((self.scale == other.scale).all()):
            return False
        return True

    def __hash__(self):
        return id(self)

    @property
    def sign(self):
        if isinstance(self.scale, _Number):
            return 1 if float(self.scale) > 0 else -1 if float(self.scale) < 0 else 0
        return self.scale.sign()

    def _call(self, x):
        return self.loc + self.scale * x

    def _inverse(self, y):
        return (y - self.loc) / self.scale

    def log_abs_det_jacobian(self, x, y):
        shape = tuple(x.shape)
        scale = self.scale
        if isinstance(scale, _Number):
            result = torch.full_like(x, math.log(abs(scale)))
        else:
            result = torch.abs(scale).log()
        if self.event_dim:
            result_size = tuple(result.size()[:result.dim() - self.event_dim]) + (-1,)
            result = result.view(result_size).sum(-1)
            shape = shape[:len(shape) - self.event_dim]
        return result.expand(shape)

    def forward_shape(self, shape):
        return torch.broadcast_shapes(tuple(shape), tuple(getattr(self.loc, "shape", ())), tuple(getattr(self.scale, "shape", ())))

    def inverse_shape(self, shape):
        return torch.broadcast_shapes(tuple(shape), tuple(getattr(self.loc, "shape", ())), tuple(getattr(self.scale, "shape", ())))


class SoftmaxTransform(Transform):
    __module__ = "torch.distributions.transforms"
    domain = _real_vector
    codomain = _simplex

    def __eq__(self, other):
        return isinstance(other, SoftmaxTransform)

    def __hash__(self):
        return id(self)

    def _call(self, x):
        probs = (x - x.max(-1, True)[0]).exp()
        return probs / probs.sum(-1, True)

    def _inverse(self, y):
        return y.log()

    def forward_shape(self, shape):
        if len(shape) < 1:
            raise ValueError("Too few dimensions on input")
        return shape

    def inverse_shape(self, shape):
        if len(shape) < 1:
            raise ValueError("Too few dimensions on input")
        return shape


class StickBreakingTransform(Transform):
    __module__ = "torch.distributions.transforms"
    domain = _real_vector
    codomain = _simplex
    bijective = True

    def __eq__(self, other):
        return isinstance(other, StickBreakingTransform)

    def __hash__(self):
        return id(self)

    def _call(self, x):
        offset = x.shape[-1] + 1 - torch.ones(x.shape[-1], dtype=x.dtype).cumsum(-1)
        z = _clipped_sigmoid(x - offset.log())
        z_cumprod = (1 - z).cumprod(-1)
        pad_z = torch.cat([z, torch.ones_like(z[..., :1])], -1)
        pad_c = torch.cat([torch.ones_like(z_cumprod[..., :1]), z_cumprod], -1)
        return pad_z * pad_c

    def _inverse(self, y):
        y_crop = y[..., :-1]
        offset = y.shape[-1] - torch.ones(y_crop.shape[-1], dtype=y.dtype).cumsum(-1)
        sf = 1 - y_crop.cumsum(-1)
        finfo = torch.finfo(y.dtype)
        sf = torch.clamp(sf, min=finfo.tiny)
        return y_crop.log() - sf.log() + offset.log()

    def log_abs_det_jacobian(self, x, y):
        offset = x.shape[-1] + 1 - torch.ones(x.shape[-1], dtype=x.dtype).cumsum(-1)
        x = x - offset.log()
        detJ = (-x - _softplus(-x) + y[..., :-1].log()).sum(-1)
        return detJ

    def forward_shape(self, shape):
        if len(shape) < 1:
            raise ValueError("Too few dimensions on input")
        return _size(shape[:-1], (shape[-1] + 1,))

    def inverse_shape(self, shape):
        if len(shape) < 1:
            raise ValueError("Too few dimensions on input")
        return _size(shape[:-1], (shape[-1] - 1,))


def _pad_left_one(x):
    """F.pad(x, [1, 0], value=1): a column of ones before the last dim."""
    return torch.cat([torch.ones_like(x[..., :1]), x], -1)


class CorrCholeskyTransform(Transform):
    __module__ = "torch.distributions.transforms"
    """Maps a vector of D(D-1)/2 reals to the Cholesky factor of a D x D
    correlation matrix: tanh to (-1, 1), then stick-breaking on the rows."""
    domain = _real_vector
    codomain = _corr_cholesky
    bijective = True

    def _call(self, x):
        x = torch.tanh(x)
        eps = torch.finfo(x.dtype).eps
        x = x.clamp(min=-1 + eps, max=1 - eps)
        r = vec_to_tril_matrix(x, diag=-1)
        z = r ** 2
        z1m_cumprod_sqrt = (1 - z).sqrt().cumprod(-1)
        r = r + torch.eye(r.shape[-1], dtype=r.dtype)
        return r * _pad_left_one(z1m_cumprod_sqrt[..., :-1])

    def _inverse(self, y):
        y_cumsum = 1 - torch.cumsum(y * y, dim=-1)
        y_cumsum_shifted = _pad_left_one(y_cumsum[..., :-1])
        y_vec = tril_matrix_to_vec(y, diag=-1)
        y_cumsum_vec = tril_matrix_to_vec(y_cumsum_shifted, diag=-1)
        t = y_vec / y_cumsum_vec.sqrt()
        return (t.log1p() - t.neg().log1p()) / 2

    def log_abs_det_jacobian(self, x, y, intermediates=None):
        # Of x and the flattened strict lower triangle of y.
        y1m_cumsum = 1 - (y * y).cumsum(dim=-1)
        y1m_cumsum_tril = tril_matrix_to_vec(y1m_cumsum, diag=-2)
        stick_breaking_logdet = 0.5 * y1m_cumsum_tril.log().sum(-1)
        tanh_logdet = -2 * (x + _softplus(-2 * x) - math.log(2.0)).sum(dim=-1)
        return stick_breaking_logdet + tanh_logdet

    def forward_shape(self, shape):
        if len(shape) < 1:
            raise ValueError("Too few dimensions on input")
        N = shape[-1]
        D = round((0.25 + 2 * N) ** 0.5 + 0.5)
        if D * (D - 1) // 2 != N:
            raise ValueError("Input is not a flattened lower-diagonal number")
        return _size(shape[:-1], (D, D))

    def inverse_shape(self, shape):
        if len(shape) < 2:
            raise ValueError("Too few dimensions on input")
        if shape[-2] != shape[-1]:
            raise ValueError("Input is not square")
        D = shape[-1]
        return _size(shape[:-2], (D * (D - 1) // 2,))


class LowerCholeskyTransform(Transform):
    __module__ = "torch.distributions.transforms"
    """Unconstrained square matrices to lower-triangular ones with a positive
    diagonal (the strict lower triangle kept, the diagonal exponentiated)."""
    domain = _IndependentConstraint(_real, 2)
    codomain = _lower_cholesky

    def __eq__(self, other):
        return isinstance(other, LowerCholeskyTransform)

    def __hash__(self):
        return id(self)

    def _call(self, x):
        return x.tril(-1) + x.diagonal(dim1=-2, dim2=-1).exp().diag_embed()

    def _inverse(self, y):
        return y.tril(-1) + y.diagonal(dim1=-2, dim2=-1).log().diag_embed()


class PositiveDefiniteTransform(Transform):
    __module__ = "torch.distributions.transforms"
    """Unconstrained square matrices to positive-definite ones (L L^T of
    LowerCholeskyTransform's L)."""
    domain = _IndependentConstraint(_real, 2)
    codomain = _positive_definite

    def __eq__(self, other):
        return isinstance(other, PositiveDefiniteTransform)

    def __hash__(self):
        return id(self)

    def _call(self, x):
        x = LowerCholeskyTransform()(x)
        return x @ x.mT

    def _inverse(self, y):
        return LowerCholeskyTransform().inv(_cholesky(y))


class CatTransform(Transform):
    __module__ = "torch.distributions.transforms"
    """Applies each transform of `tseq` to its slice (of `lengths[i]`) along
    `dim`, like torch.cat."""

    def __init__(self, tseq, dim=0, lengths=None, cache_size=0):
        if not all(isinstance(t, Transform) for t in tseq):
            raise AssertionError("All elements of tseq must be Transform instances")
        if cache_size:
            tseq = [t.with_cache(cache_size) for t in tseq]
        Transform.__init__(self, cache_size=cache_size)
        self.transforms = list(tseq)
        if lengths is None:
            lengths = [1] * len(self.transforms)
        self.lengths = list(lengths)
        if len(self.lengths) != len(self.transforms):
            raise AssertionError("lengths (%d) must match transforms (%d)" % (len(self.lengths), len(self.transforms)))
        self.dim = dim

    @property
    def event_dim(self):
        return max(t.event_dim for t in self.transforms)

    @property
    def length(self):
        return sum(self.lengths)

    def with_cache(self, cache_size=1):
        if self._cache_size == cache_size:
            return self
        return CatTransform(self.transforms, self.dim, self.lengths, cache_size)

    def _check(self, x, kind, name):
        if not (-x.dim() <= self.dim < x.dim()):
            raise AssertionError("dim %d out of range for %s with %d dimensions" % (self.dim, kind, x.dim()))
        if x.size(self.dim) != self.length:
            raise AssertionError("%s.size(%d) = %d must equal length %d" % (name, self.dim, x.size(self.dim), self.length))

    def _call(self, x):
        self._check(x, "tensor", "x")
        yslices = []
        start = 0
        for trans, length in zip(self.transforms, self.lengths):
            yslices.append(trans(x.narrow(self.dim, start, length)))
            start = start + length
        return torch.cat(yslices, dim=self.dim)

    def _inverse(self, y):
        self._check(y, "tensor", "y")
        xslices = []
        start = 0
        for trans, length in zip(self.transforms, self.lengths):
            xslices.append(trans.inv(y.narrow(self.dim, start, length)))
            start = start + length
        return torch.cat(xslices, dim=self.dim)

    def log_abs_det_jacobian(self, x, y):
        self._check(x, "x", "x")
        self._check(y, "y", "y")
        logdetjacs = []
        start = 0
        for trans, length in zip(self.transforms, self.lengths):
            logdetjac = trans.log_abs_det_jacobian(x.narrow(self.dim, start, length), y.narrow(self.dim, start, length))
            if trans.event_dim < self.event_dim:
                logdetjac = _sum_rightmost(logdetjac, self.event_dim - trans.event_dim)
            logdetjacs.append(logdetjac)
            start = start + length
        dim = self.dim
        if dim >= 0:
            dim = dim - x.dim()
        dim = dim + self.event_dim
        if dim < 0:
            return torch.cat(logdetjacs, dim=dim)
        total = logdetjacs[0]
        for j in logdetjacs[1:]:
            total = total + j
        return total

    @property
    def bijective(self):
        return all(t.bijective for t in self.transforms)

    @_dependent_property
    def domain(self):
        return _Cat([t.domain for t in self.transforms], self.dim, self.lengths)

    @_dependent_property
    def codomain(self):
        return _Cat([t.codomain for t in self.transforms], self.dim, self.lengths)


class StackTransform(Transform):
    __module__ = "torch.distributions.transforms"
    """Applies each transform of `tseq` to its slice along `dim`, like
    torch.stack."""

    def __init__(self, tseq, dim=0, cache_size=0):
        if not all(isinstance(t, Transform) for t in tseq):
            raise AssertionError("All elements of tseq must be Transform instances")
        if cache_size:
            tseq = [t.with_cache(cache_size) for t in tseq]
        Transform.__init__(self, cache_size=cache_size)
        self.transforms = list(tseq)
        self.dim = dim

    def with_cache(self, cache_size=1):
        if self._cache_size == cache_size:
            return self
        return StackTransform(self.transforms, self.dim, cache_size)

    def _slice(self, z):
        return [z.select(self.dim, i) for i in range(z.size(self.dim))]

    def _check(self, x, kind, name):
        if not (-x.dim() <= self.dim < x.dim()):
            raise AssertionError("dim %d out of range for %s with %d dimensions" % (self.dim, kind, x.dim()))
        if x.size(self.dim) != len(self.transforms):
            raise AssertionError("%s.size(%d) = %d must equal len(transforms) %d" % (name, self.dim, x.size(self.dim), len(self.transforms)))

    def _call(self, x):
        self._check(x, "tensor", "x")
        return torch.stack([trans(xs) for xs, trans in zip(self._slice(x), self.transforms)], dim=self.dim)

    def _inverse(self, y):
        self._check(y, "tensor", "y")
        return torch.stack([trans.inv(ys) for ys, trans in zip(self._slice(y), self.transforms)], dim=self.dim)

    def log_abs_det_jacobian(self, x, y):
        self._check(x, "x", "x")
        self._check(y, "y", "y")
        return torch.stack([trans.log_abs_det_jacobian(xs, ys) for xs, ys, trans in zip(self._slice(x), self._slice(y), self.transforms)], dim=self.dim)

    @property
    def bijective(self):
        return all(t.bijective for t in self.transforms)

    @_dependent_property
    def domain(self):
        return _Stack([t.domain for t in self.transforms], self.dim)

    @_dependent_property
    def codomain(self):
        return _Stack([t.codomain for t in self.transforms], self.dim)


class CumulativeDistributionTransform(Transform):
    __module__ = "torch.distributions.transforms"
    """A univariate distribution's CDF: its support to the unit interval,
    with log|dy/dx| = log_prob(x)."""
    bijective = True
    codomain = _unit_interval
    sign = +1

    def __init__(self, distribution, cache_size=0):
        Transform.__init__(self, cache_size=cache_size)
        self.distribution = distribution

    @property
    def domain(self):
        return self.distribution.support

    def _call(self, x):
        return self.distribution.cdf(x)

    def _inverse(self, y):
        return self.distribution.icdf(y)

    def log_abs_det_jacobian(self, x, y):
        return self.distribution.log_prob(x)

    def with_cache(self, cache_size=1):
        if self._cache_size == cache_size:
            return self
        return CumulativeDistributionTransform(self.distribution, cache_size=cache_size)


transforms = _Namespace("torch.distributions.transforms")
for _cls in (Transform, ComposeTransform, IndependentTransform, ReshapeTransform, ExpTransform,
             PowerTransform, SigmoidTransform, SoftplusTransform, TanhTransform, AbsTransform,
             AffineTransform, SoftmaxTransform, StickBreakingTransform, CorrCholeskyTransform,
             LowerCholeskyTransform, PositiveDefiniteTransform, CatTransform, StackTransform,
             CumulativeDistributionTransform):
    setattr(transforms, _cls.__name__, _cls)
transforms.identity_transform = identity_transform
transforms._InverseTransform = _InverseTransform


# ---- transformed distributions ----------------------------------------------------------------
class TransformedDistribution(Distribution):
    __module__ = "torch.distributions.transformed_distribution"
    arg_constraints = {}

    def __init__(self, base_distribution, transforms, validate_args=None):
        if isinstance(transforms, Transform):
            self.transforms = [transforms]
        elif isinstance(transforms, list):
            if not all(isinstance(t, Transform) for t in transforms):
                raise ValueError("transforms must be a Transform or a list of Transforms")
            self.transforms = transforms
        else:
            raise ValueError("transforms must be a Transform or list, but was %s" % (transforms,))
        base_shape = _size(base_distribution.batch_shape, base_distribution.event_shape)
        base_event_dim = len(base_distribution.event_shape)
        transform = ComposeTransform(self.transforms)
        if len(base_shape) < transform.domain.event_dim:
            raise ValueError("base_distribution needs to have shape with size at least %d, but got %s." % (transform.domain.event_dim, base_shape))
        forward_shape = torch.Size(tuple(transform.forward_shape(base_shape)))
        expanded_base_shape = torch.Size(tuple(transform.inverse_shape(forward_shape)))
        if tuple(base_shape) != tuple(expanded_base_shape):
            base_batch_shape = expanded_base_shape[:len(expanded_base_shape) - base_event_dim]
            base_distribution = base_distribution.expand(base_batch_shape)
        reinterpreted_batch_ndims = transform.domain.event_dim - base_event_dim
        if reinterpreted_batch_ndims > 0:
            base_distribution = Independent(base_distribution, reinterpreted_batch_ndims)
        self.base_dist = base_distribution
        transform_change_in_event_dim = transform.codomain.event_dim - transform.domain.event_dim
        event_dim = max(transform.codomain.event_dim, base_event_dim + transform_change_in_event_dim)
        cut = len(forward_shape) - event_dim
        Distribution.__init__(self, forward_shape[:cut], forward_shape[cut:], validate_args=validate_args)

    def expand(self, batch_shape, _instance=None):
        new = self._get_checked_instance(TransformedDistribution, _instance)
        batch_shape = torch.Size(tuple(batch_shape))
        shape = _size(batch_shape, self.event_shape)
        for t in reversed(self.transforms):
            shape = t.inverse_shape(shape)
        base_batch_shape = shape[:len(shape) - len(self.base_dist.event_shape)]
        new.base_dist = self.base_dist.expand(base_batch_shape)
        new.transforms = self.transforms
        Distribution.__init__(new, batch_shape, self.event_shape, validate_args=False)
        new._validate_args = self._validate_args
        return new

    @_dependent_property(is_discrete=False)
    def support(self):
        if not self.transforms:
            return self.base_dist.support
        support = self.transforms[-1].codomain
        if len(self.event_shape) > support.event_dim:
            support = _IndependentConstraint(support, len(self.event_shape) - support.event_dim)
        return support

    @property
    def has_rsample(self):
        return self.base_dist.has_rsample

    def sample(self, sample_shape=()):
        with torch.no_grad():
            x = self.base_dist.sample(sample_shape)
            for transform in self.transforms:
                x = transform(x)
            return x

    def rsample(self, sample_shape=()):
        x = self.base_dist.rsample(sample_shape)
        for transform in self.transforms:
            x = transform(x)
        return x

    def log_prob(self, value):
        if self._validate_args:
            self._validate_sample(value)
        event_dim = len(self.event_shape)
        log_prob = 0.0
        y = value
        for transform in reversed(self.transforms):
            x = transform.inv(y)
            event_dim += transform.domain.event_dim - transform.codomain.event_dim
            log_prob = log_prob - _sum_rightmost(transform.log_abs_det_jacobian(x, y), event_dim - transform.domain.event_dim)
            y = x
        return log_prob + _sum_rightmost(self.base_dist.log_prob(y), event_dim - len(self.base_dist.event_shape))

    def _monotonize_cdf(self, value):
        sign = 1
        for transform in self.transforms:
            sign = sign * transform.sign
        if isinstance(sign, int) and sign == 1:
            return value
        return sign * (value - 0.5) + 0.5

    def cdf(self, value):
        for transform in self.transforms[::-1]:
            value = transform.inv(value)
        if self._validate_args:
            self.base_dist._validate_sample(value)
        value = self.base_dist.cdf(value)
        return self._monotonize_cdf(value)

    def icdf(self, value):
        value = self._monotonize_cdf(value)
        value = self.base_dist.icdf(value)
        for transform in self.transforms:
            value = transform(value)
        return value


class LogNormal(TransformedDistribution):
    __module__ = "torch.distributions.log_normal"
    arg_constraints = {"loc": _real, "scale": _positive}
    support = _positive
    has_rsample = True

    def __init__(self, loc, scale, validate_args=None):
        base_dist = Normal(loc, scale, validate_args=validate_args)
        TransformedDistribution.__init__(self, base_dist, ExpTransform(), validate_args=validate_args)

    def expand(self, batch_shape, _instance=None):
        new = self._get_checked_instance(LogNormal, _instance)
        return TransformedDistribution.expand(self, batch_shape, _instance=new)

    @property
    def loc(self):
        return self.base_dist.loc

    @property
    def scale(self):
        return self.base_dist.scale

    @property
    def mean(self):
        return (self.loc + self.scale.pow(2) / 2).exp()

    @property
    def mode(self):
        return (self.loc - self.scale.square()).exp()

    @property
    def variance(self):
        scale_sq = self.scale.pow(2)
        return scale_sq.expm1() * (2 * self.loc + scale_sq).exp()

    def entropy(self):
        return self.base_dist.entropy() + self.loc


class HalfNormal(TransformedDistribution):
    __module__ = "torch.distributions.half_normal"
    arg_constraints = {"scale": _positive}
    support = _nonnegative
    has_rsample = True

    def __init__(self, scale, validate_args=None):
        base_dist = Normal(0, scale, validate_args=False)
        TransformedDistribution.__init__(self, base_dist, AbsTransform(), validate_args=validate_args)

    def expand(self, batch_shape, _instance=None):
        new = self._get_checked_instance(HalfNormal, _instance)
        return TransformedDistribution.expand(self, batch_shape, _instance=new)

    @property
    def scale(self):
        return self.base_dist.scale

    @property
    def mean(self):
        return self.scale * math.sqrt(2 / math.pi)

    @property
    def mode(self):
        return torch.zeros_like(self.scale)

    @property
    def variance(self):
        return self.scale.pow(2) * (1 - 2 / math.pi)

    def log_prob(self, value):
        if self._validate_args:
            self._validate_sample(value)
        log_prob = self.base_dist.log_prob(value) + math.log(2)
        return torch.where(value >= 0, log_prob, torch.full_like(log_prob, -inf))

    def cdf(self, value):
        if self._validate_args:
            self._validate_sample(value)
        return 2 * self.base_dist.cdf(value) - 1

    def icdf(self, prob):
        return self.base_dist.icdf((prob + 1) / 2)

    def entropy(self):
        return self.base_dist.entropy() - math.log(2)


class HalfCauchy(TransformedDistribution):
    __module__ = "torch.distributions.half_cauchy"
    arg_constraints = {"scale": _positive}
    support = _nonnegative
    has_rsample = True

    def __init__(self, scale, validate_args=None):
        base_dist = Cauchy(0, scale, validate_args=False)
        TransformedDistribution.__init__(self, base_dist, AbsTransform(), validate_args=validate_args)

    def expand(self, batch_shape, _instance=None):
        new = self._get_checked_instance(HalfCauchy, _instance)
        return TransformedDistribution.expand(self, batch_shape, _instance=new)

    @property
    def scale(self):
        return self.base_dist.scale

    @property
    def mean(self):
        return torch.full(tuple(self._extended_shape()), math.inf, dtype=self.scale.dtype)

    @property
    def mode(self):
        return torch.zeros_like(self.scale)

    @property
    def variance(self):
        return self.base_dist.variance

    def log_prob(self, value):
        if self._validate_args:
            self._validate_sample(value)
        value = torch.as_tensor(value, dtype=self.base_dist.scale.dtype)
        log_prob = self.base_dist.log_prob(value) + math.log(2)
        return torch.where(value >= 0, log_prob, torch.full_like(log_prob, -inf))

    def cdf(self, value):
        if self._validate_args:
            self._validate_sample(value)
        return 2 * self.base_dist.cdf(value) - 1

    def icdf(self, prob):
        return self.base_dist.icdf((prob + 1) / 2)

    def entropy(self):
        return self.base_dist.entropy() - math.log(2)


class LogitRelaxedBernoulli(Distribution):
    __module__ = "torch.distributions.relaxed_bernoulli"
    arg_constraints = {"probs": _unit_interval, "logits": _real}
    support = _real

    def __init__(self, temperature, probs=None, logits=None, validate_args=None):
        self.temperature = temperature
        if (probs is None) == (logits is None):
            raise ValueError("Either `probs` or `logits` must be specified, but not both.")
        if probs is not None:
            is_scalar = isinstance(probs, _Number)
            (self.probs,) = broadcast_all(probs)
        else:
            is_scalar = isinstance(logits, _Number)
            (self.logits,) = broadcast_all(logits)
        self._param = self.probs if probs is not None else self.logits
        batch_shape = torch.Size() if is_scalar else self._param.size()
        Distribution.__init__(self, batch_shape, validate_args=validate_args)

    def expand(self, batch_shape, _instance=None):
        new = self._get_checked_instance(LogitRelaxedBernoulli, _instance)
        batch_shape = torch.Size(tuple(batch_shape))
        new.temperature = self.temperature
        if "probs" in self.__dict__:
            new.probs = self.probs.expand(batch_shape)
            new._param = new.probs
        if "logits" in self.__dict__:
            new.logits = self.logits.expand(batch_shape)
            new._param = new.logits
        Distribution.__init__(new, batch_shape, validate_args=False)
        new._validate_args = self._validate_args
        return new

    @lazy_property
    def logits(self):
        return probs_to_logits(self.probs, is_binary=True)

    @lazy_property
    def probs(self):
        return logits_to_probs(self.logits, is_binary=True)

    @property
    def param_shape(self):
        return self._param.size()

    def rsample(self, sample_shape=()):
        shape = self._extended_shape(sample_shape)
        probs = clamp_probs(self.probs.expand(shape))
        uniforms = clamp_probs(torch.rand(tuple(shape), dtype=probs.dtype))
        return (uniforms.log() - (-uniforms).log1p() + probs.log() - (-probs).log1p()) / self.temperature

    def log_prob(self, value):
        if self._validate_args:
            self._validate_sample(value)
        logits, value = broadcast_all(self.logits, value)
        diff = logits - value.mul(self.temperature)
        return torch.as_tensor(self.temperature, dtype=diff.dtype).log() + diff - 2 * diff.exp().log1p()


class RelaxedBernoulli(TransformedDistribution):
    __module__ = "torch.distributions.relaxed_bernoulli"
    arg_constraints = {"probs": _unit_interval, "logits": _real}
    support = _unit_interval
    has_rsample = True

    def __init__(self, temperature, probs=None, logits=None, validate_args=None):
        base_dist = LogitRelaxedBernoulli(temperature, probs, logits)
        TransformedDistribution.__init__(self, base_dist, SigmoidTransform(), validate_args=validate_args)

    def expand(self, batch_shape, _instance=None):
        new = self._get_checked_instance(RelaxedBernoulli, _instance)
        return TransformedDistribution.expand(self, batch_shape, _instance=new)

    @property
    def temperature(self):
        return self.base_dist.temperature

    @property
    def logits(self):
        return self.base_dist.logits

    @property
    def probs(self):
        return self.base_dist.probs


class ExpRelaxedCategorical(Distribution):
    __module__ = "torch.distributions.relaxed_categorical"
    arg_constraints = {"probs": _simplex, "logits": _real_vector}
    support = _real_vector
    has_rsample = True

    def __init__(self, temperature, probs=None, logits=None, validate_args=None):
        self._categorical = Categorical(probs, logits)
        self.temperature = temperature
        batch_shape = self._categorical.batch_shape
        event_shape = self._categorical.param_shape[-1:]
        Distribution.__init__(self, batch_shape, event_shape, validate_args=validate_args)

    def expand(self, batch_shape, _instance=None):
        new = self._get_checked_instance(ExpRelaxedCategorical, _instance)
        batch_shape = torch.Size(tuple(batch_shape))
        new.temperature = self.temperature
        new._categorical = self._categorical.expand(batch_shape)
        Distribution.__init__(new, batch_shape, self.event_shape, validate_args=False)
        new._validate_args = self._validate_args
        return new

    @property
    def param_shape(self):
        return self._categorical.param_shape

    @property
    def logits(self):
        return self._categorical.logits

    @property
    def probs(self):
        return self._categorical.probs

    def rsample(self, sample_shape=()):
        shape = self._extended_shape(sample_shape)
        uniforms = clamp_probs(torch.rand(tuple(shape), dtype=self.logits.dtype))
        gumbels = -((-(uniforms.log())).log())
        scores = (self.logits + gumbels) / self.temperature
        return scores - scores.logsumexp(dim=-1, keepdim=True)

    def log_prob(self, value):
        K = self._categorical._num_events
        if self._validate_args:
            self._validate_sample(value)
        logits, value = broadcast_all(self.logits, value)
        temperature = torch.as_tensor(self.temperature, dtype=logits.dtype)
        log_scale = _lgamma(torch.full_like(temperature, float(K))) - temperature.log().mul(-(K - 1))
        score = logits - value.mul(temperature)
        score = (score - score.logsumexp(dim=-1, keepdim=True)).sum(-1)
        return score + log_scale


class RelaxedOneHotCategorical(TransformedDistribution):
    __module__ = "torch.distributions.relaxed_categorical"
    arg_constraints = {"probs": _simplex, "logits": _real_vector}
    support = _simplex
    has_rsample = True

    def __init__(self, temperature, probs=None, logits=None, validate_args=None):
        base_dist = ExpRelaxedCategorical(temperature, probs, logits, validate_args=validate_args)
        TransformedDistribution.__init__(self, base_dist, ExpTransform(), validate_args=validate_args)

    def expand(self, batch_shape, _instance=None):
        new = self._get_checked_instance(RelaxedOneHotCategorical, _instance)
        return TransformedDistribution.expand(self, batch_shape, _instance=new)

    @property
    def temperature(self):
        return self.base_dist.temperature

    @property
    def logits(self):
        return self.base_dist.logits

    @property
    def probs(self):
        return self.base_dist.probs


# ---- more transformed distributions -----------------------------------------------------------
class Gumbel(TransformedDistribution):
    __module__ = "torch.distributions.gumbel"
    """loc - scale * log(-log(U)), U ~ Uniform(tiny, 1 - eps)."""
    arg_constraints = {"loc": _real, "scale": _positive}
    support = _real

    def __init__(self, loc, scale, validate_args=None):
        self.loc, self.scale = broadcast_all(loc, scale)
        finfo = torch.finfo(self.loc.dtype)
        if isinstance(loc, _Number) and isinstance(scale, _Number):
            base_dist = Uniform(finfo.tiny, 1 - finfo.eps, validate_args=validate_args)
        else:
            base_dist = Uniform(torch.full_like(self.loc, finfo.tiny), torch.full_like(self.loc, 1 - finfo.eps), validate_args=validate_args)
        parts = [ExpTransform().inv, AffineTransform(loc=0, scale=-torch.ones_like(self.scale)),
                 ExpTransform().inv, AffineTransform(loc=loc, scale=-self.scale)]
        TransformedDistribution.__init__(self, base_dist, parts, validate_args=validate_args)

    def expand(self, batch_shape, _instance=None):
        new = self._get_checked_instance(Gumbel, _instance)
        batch_shape = torch.Size(tuple(batch_shape))
        new.loc = self.loc.expand(batch_shape)
        new.scale = self.scale.expand(batch_shape)
        return TransformedDistribution.expand(self, batch_shape, _instance=new)

    def log_prob(self, value):
        if self._validate_args:
            self._validate_sample(value)
        y = (self.loc - value) / self.scale
        return (y - y.exp()) - self.scale.log()

    @property
    def mean(self):
        return self.loc + self.scale * euler_constant

    @property
    def mode(self):
        return self.loc

    @property
    def stddev(self):
        return (math.pi / math.sqrt(6)) * self.scale

    @property
    def variance(self):
        return self.stddev.pow(2)

    def entropy(self):
        return self.scale.log() + (1 + euler_constant)


class Pareto(TransformedDistribution):
    __module__ = "torch.distributions.pareto"
    """Pareto type I: scale * exp(E), E ~ Exponential(alpha)."""
    arg_constraints = {"alpha": _positive, "scale": _positive}

    def __init__(self, scale, alpha, validate_args=None):
        self.scale, self.alpha = broadcast_all(scale, alpha)
        base_dist = Exponential(self.alpha, validate_args=validate_args)
        TransformedDistribution.__init__(self, base_dist, [ExpTransform(), AffineTransform(loc=0, scale=self.scale)], validate_args=validate_args)

    def expand(self, batch_shape, _instance=None):
        new = self._get_checked_instance(Pareto, _instance)
        batch_shape = torch.Size(tuple(batch_shape))
        new.scale = self.scale.expand(batch_shape)
        new.alpha = self.alpha.expand(batch_shape)
        return TransformedDistribution.expand(self, batch_shape, _instance=new)

    @property
    def mean(self):
        a = self.alpha.clamp(min=1)
        return a * self.scale / (a - 1)

    @property
    def mode(self):
        return self.scale

    @property
    def variance(self):
        a = self.alpha.clamp(min=2)
        return self.scale.pow(2) * a / ((a - 1).pow(2) * (a - 2))

    @_dependent_property(is_discrete=False, event_dim=0)
    def support(self):
        return _GreaterThanEq(self.scale)

    def entropy(self):
        return (self.scale / self.alpha).log() + (1 + self.alpha.reciprocal())


class Weibull(TransformedDistribution):
    __module__ = "torch.distributions.weibull"
    """scale * E^(1 / concentration), E ~ Exponential(1)."""
    arg_constraints = {"scale": _positive, "concentration": _positive}
    support = _positive

    def __init__(self, scale, concentration, validate_args=None):
        self.scale, self.concentration = broadcast_all(scale, concentration)
        self.concentration_reciprocal = self.concentration.reciprocal()
        base_dist = Exponential(torch.ones_like(self.scale), validate_args=validate_args)
        parts = [PowerTransform(exponent=self.concentration_reciprocal), AffineTransform(loc=0, scale=self.scale)]
        TransformedDistribution.__init__(self, base_dist, parts, validate_args=validate_args)

    def expand(self, batch_shape, _instance=None):
        new = self._get_checked_instance(Weibull, _instance)
        batch_shape = torch.Size(tuple(batch_shape))
        new.scale = self.scale.expand(batch_shape)
        new.concentration = self.concentration.expand(batch_shape)
        new.concentration_reciprocal = new.concentration.reciprocal()
        base_dist = self.base_dist.expand(batch_shape)
        parts = [PowerTransform(exponent=new.concentration_reciprocal), AffineTransform(loc=0, scale=new.scale)]
        TransformedDistribution.__init__(new, base_dist, parts, validate_args=False)
        new._validate_args = self._validate_args
        return new

    @property
    def mean(self):
        return self.scale * torch.exp(_lgamma(1 + self.concentration_reciprocal))

    @property
    def mode(self):
        return self.scale * ((self.concentration - 1) / self.concentration) ** self.concentration.reciprocal()

    @property
    def variance(self):
        return self.scale.pow(2) * (torch.exp(_lgamma(1 + 2 * self.concentration_reciprocal))
                                    - torch.exp(2 * _lgamma(1 + self.concentration_reciprocal)))

    def entropy(self):
        return euler_constant * (1 - self.concentration_reciprocal) + torch.log(self.scale * self.concentration_reciprocal) + 1


def _kumaraswamy_moments(a, b, n):
    arg1 = 1 + n / a
    log_value = _lgamma(arg1) + _lgamma(b) - _lgamma(arg1 + b)
    return b * torch.exp(log_value)


class Kumaraswamy(TransformedDistribution):
    __module__ = "torch.distributions.kumaraswamy"
    """(1 - (1 - U)^(1 / concentration0))^(1 / concentration1), U ~ Uniform(0, 1)."""
    arg_constraints = {"concentration1": _positive, "concentration0": _positive}
    support = _unit_interval
    has_rsample = True

    def __init__(self, concentration1, concentration0, validate_args=None):
        self.concentration1, self.concentration0 = broadcast_all(concentration1, concentration0)
        base_dist = Uniform(torch.full_like(self.concentration0, 0), torch.full_like(self.concentration0, 1), validate_args=validate_args)
        parts = [PowerTransform(exponent=self.concentration0.reciprocal()), AffineTransform(loc=1.0, scale=-1.0),
                 PowerTransform(exponent=self.concentration1.reciprocal())]
        TransformedDistribution.__init__(self, base_dist, parts, validate_args=validate_args)

    def expand(self, batch_shape, _instance=None):
        new = self._get_checked_instance(Kumaraswamy, _instance)
        batch_shape = torch.Size(tuple(batch_shape))
        new.concentration1 = self.concentration1.expand(batch_shape)
        new.concentration0 = self.concentration0.expand(batch_shape)
        return TransformedDistribution.expand(self, batch_shape, _instance=new)

    @property
    def mean(self):
        return _kumaraswamy_moments(self.concentration1, self.concentration0, 1)

    @property
    def mode(self):
        # PyTorch's log-space expression, nan where either concentration is below 1.
        log_mode = self.concentration0.reciprocal() * (-self.concentration0).log1p() - (-self.concentration0 * self.concentration1).log1p()
        bad = (self.concentration0 < 1) | (self.concentration1 < 1)
        return torch.where(bad, torch.full_like(log_mode, nan), log_mode).exp()

    @property
    def variance(self):
        return _kumaraswamy_moments(self.concentration1, self.concentration0, 2) - torch.pow(self.mean, 2)

    def entropy(self):
        t1 = 1 - self.concentration1.reciprocal()
        t0 = 1 - self.concentration0.reciprocal()
        H0 = _digamma(self.concentration0 + 1) + euler_constant
        return t0 + t1 * H0 - torch.log(self.concentration1) - torch.log(self.concentration0)


class InverseGamma(TransformedDistribution):
    __module__ = "torch.distributions.inverse_gamma"
    """1 / X for X ~ Gamma(concentration, rate)."""
    arg_constraints = {"concentration": _positive, "rate": _positive}
    support = _positive
    has_rsample = True

    def __init__(self, concentration, rate, validate_args=None):
        base_dist = Gamma(concentration, rate, validate_args=validate_args)
        neg_one = -torch.ones((), dtype=base_dist.rate.dtype)
        TransformedDistribution.__init__(self, base_dist, PowerTransform(neg_one), validate_args=validate_args)

    def expand(self, batch_shape, _instance=None):
        new = self._get_checked_instance(InverseGamma, _instance)
        return TransformedDistribution.expand(self, batch_shape, _instance=new)

    @property
    def concentration(self):
        return self.base_dist.concentration

    @property
    def rate(self):
        return self.base_dist.rate

    @property
    def mean(self):
        result = self.rate / (self.concentration - 1)
        return torch.where(self.concentration > 1, result, torch.full_like(result, inf))

    @property
    def mode(self):
        return self.rate / (self.concentration + 1)

    @property
    def variance(self):
        result = self.rate.square() / ((self.concentration - 1).square() * (self.concentration - 2))
        return torch.where(self.concentration > 2, result, torch.full_like(result, inf))

    def entropy(self):
        return self.concentration + self.rate.log() + _lgamma(self.concentration) - (1 + self.concentration) * _digamma(self.concentration)


class LogisticNormal(TransformedDistribution):
    __module__ = "torch.distributions.logistic_normal"
    """StickBreakingTransform of Normal(loc, scale): a distribution on the
    simplex one longer than loc."""
    arg_constraints = {"loc": _real, "scale": _positive}
    support = _simplex
    has_rsample = True

    def __init__(self, loc, scale, validate_args=None):
        base_dist = Normal(loc, scale, validate_args=validate_args)
        if not base_dist.batch_shape:
            base_dist = base_dist.expand([1])
        TransformedDistribution.__init__(self, base_dist, StickBreakingTransform(), validate_args=validate_args)

    def expand(self, batch_shape, _instance=None):
        new = self._get_checked_instance(LogisticNormal, _instance)
        return TransformedDistribution.expand(self, batch_shape, _instance=new)

    @property
    def loc(self):
        return self.base_dist.base_dist.loc

    @property
    def scale(self):
        return self.base_dist.base_dist.scale


# ---- more distributions -----------------------------------------------------------------------
class ContinuousBernoulli(ExponentialFamily):
    __module__ = "torch.distributions.continuous_bernoulli"
    """The continuous Bernoulli on [0, 1] (Loaiza-Ganem and Cunningham, 2019):
    density proportional to probs^x (1 - probs)^(1 - x). Probabilities in
    `lims` use Taylor expansions around 1/2."""
    arg_constraints = {"probs": _unit_interval, "logits": _real}
    support = _unit_interval
    _mean_carrier_measure = 0
    has_rsample = True

    def __init__(self, probs=None, logits=None, lims=(0.499, 0.501), validate_args=None):
        if (probs is None) == (logits is None):
            raise ValueError("Either `probs` or `logits` must be specified, but not both.")
        if probs is not None:
            is_scalar = isinstance(probs, _Number)
            (self.probs,) = broadcast_all(probs)
            # Checked here when asked, as the clamped probs always pass.
            if validate_args is not None:
                if not _all_true(self.arg_constraints["probs"].check(self.probs)):
                    raise ValueError("The parameter probs has invalid values")
            self.probs = clamp_probs(self.probs)
        else:
            is_scalar = isinstance(logits, _Number)
            (self.logits,) = broadcast_all(logits)
        self._param = self.probs if probs is not None else self.logits
        batch_shape = torch.Size() if is_scalar else self._param.size()
        self._lims = lims
        Distribution.__init__(self, batch_shape, validate_args=validate_args)

    def expand(self, batch_shape, _instance=None):
        new = self._get_checked_instance(ContinuousBernoulli, _instance)
        new._lims = self._lims
        batch_shape = torch.Size(tuple(batch_shape))
        if "probs" in self.__dict__:
            new.probs = self.probs.expand(batch_shape)
            new._param = new.probs
        if "logits" in self.__dict__:
            new.logits = self.logits.expand(batch_shape)
            new._param = new.logits
        Distribution.__init__(new, batch_shape, validate_args=False)
        new._validate_args = self._validate_args
        return new

    def _outside_unstable_region(self):
        return torch.le(self.probs, self._lims[0]) | torch.gt(self.probs, self._lims[1])

    def _cut_probs(self):
        return torch.where(self._outside_unstable_region(), self.probs, self._lims[0] * torch.ones_like(self.probs))

    def _cont_bern_log_norm(self):
        """The log normalizing constant as a function of probs."""
        cut_probs = self._cut_probs()
        cut_probs_below_half = torch.where(torch.le(cut_probs, 0.5), cut_probs, torch.zeros_like(cut_probs))
        cut_probs_above_half = torch.where(torch.ge(cut_probs, 0.5), cut_probs, torch.ones_like(cut_probs))
        log_norm = torch.log(torch.abs(torch.log1p(-cut_probs) - torch.log(cut_probs))) - torch.where(
            torch.le(cut_probs, 0.5), torch.log1p(-2.0 * cut_probs_below_half), torch.log(2.0 * cut_probs_above_half - 1.0))
        x = torch.pow(self.probs - 0.5, 2)
        taylor = math.log(2.0) + (4.0 / 3.0 + 104.0 / 45.0 * x) * x
        return torch.where(self._outside_unstable_region(), log_norm, taylor)

    @property
    def mean(self):
        cut_probs = self._cut_probs()
        mus = cut_probs / (2.0 * cut_probs - 1.0) + 1.0 / (torch.log1p(-cut_probs) - torch.log(cut_probs))
        x = self.probs - 0.5
        taylor = 0.5 + (1.0 / 3.0 + 16.0 / 45.0 * torch.pow(x, 2)) * x
        return torch.where(self._outside_unstable_region(), mus, taylor)

    @property
    def stddev(self):
        return torch.sqrt(self.variance)

    @property
    def variance(self):
        cut_probs = self._cut_probs()
        vars_ = cut_probs * (cut_probs - 1.0) / torch.pow(1.0 - 2.0 * cut_probs, 2) + 1.0 / torch.pow(torch.log1p(-cut_probs) - torch.log(cut_probs), 2)
        x = torch.pow(self.probs - 0.5, 2)
        taylor = 1.0 / 12.0 - (1.0 / 15.0 - 128.0 / 945.0 * x) * x
        return torch.where(self._outside_unstable_region(), vars_, taylor)

    @lazy_property
    def logits(self):
        return probs_to_logits(self.probs, is_binary=True)

    @lazy_property
    def probs(self):
        return clamp_probs(logits_to_probs(self.logits, is_binary=True))

    @property
    def param_shape(self):
        return self._param.size()

    def sample(self, sample_shape=()):
        shape = self._extended_shape(sample_shape)
        u = torch.rand(tuple(shape), dtype=self.probs.dtype)
        with torch.no_grad():
            return self.icdf(u)

    def rsample(self, sample_shape=()):
        shape = self._extended_shape(sample_shape)
        u = torch.rand(tuple(shape), dtype=self.probs.dtype)
        return self.icdf(u)

    def log_prob(self, value):
        if self._validate_args:
            self._validate_sample(value)
        logits, value = broadcast_all(self.logits, value)
        return -torch.nn.functional.binary_cross_entropy_with_logits(logits, value, reduction="none") + self._cont_bern_log_norm()

    def cdf(self, value):
        if self._validate_args:
            self._validate_sample(value)
        cut_probs = self._cut_probs()
        cdfs = (torch.pow(cut_probs, value) * torch.pow(1.0 - cut_probs, 1.0 - value) + cut_probs - 1.0) / (2.0 * cut_probs - 1.0)
        unbounded_cdfs = torch.where(self._outside_unstable_region(), cdfs, value)
        return torch.where(torch.le(value, 0.0), torch.zeros_like(value),
                           torch.where(torch.ge(value, 1.0), torch.ones_like(value), unbounded_cdfs))

    def icdf(self, value):
        cut_probs = self._cut_probs()
        return torch.where(
            self._outside_unstable_region(),
            (torch.log1p(-cut_probs + value * (2.0 * cut_probs - 1.0)) - torch.log1p(-cut_probs)) / (torch.log(cut_probs) - torch.log1p(-cut_probs)),
            value)

    def entropy(self):
        log_probs0 = torch.log1p(-self.probs)
        log_probs1 = torch.log(self.probs)
        return self.mean * (log_probs0 - log_probs1) - self._cont_bern_log_norm() - log_probs0

    @property
    def _natural_params(self):
        return (self.logits,)

    def _log_normalizer(self, x):
        out_unst_reg = torch.le(x, self._lims[0] - 0.5) | torch.gt(x, self._lims[1] - 0.5)
        cut_nat_params = torch.where(out_unst_reg, x, (self._lims[0] - 0.5) * torch.ones_like(x))
        log_norm = torch.log(torch.abs(torch.expm1(cut_nat_params))) - torch.log(torch.abs(cut_nat_params))
        taylor = 0.5 * x + torch.pow(x, 2) / 24.0 - torch.pow(x, 4) / 2880.0
        return torch.where(out_unst_reg, log_norm, taylor)


class FisherSnedecor(Distribution):
    __module__ = "torch.distributions.fishersnedecor"
    """F(df1, df2): the ratio of Gamma(df1 / 2, df1) and Gamma(df2 / 2, df2)
    draws (so rsample is reparameterised through the implicit Gamma gradient)."""
    arg_constraints = {"df1": _positive, "df2": _positive}
    support = _positive
    has_rsample = True

    def __init__(self, df1, df2, validate_args=None):
        self.df1, self.df2 = broadcast_all(df1, df2)
        self._gamma1 = Gamma(self.df1 * 0.5, self.df1)
        self._gamma2 = Gamma(self.df2 * 0.5, self.df2)
        batch_shape = torch.Size() if _scalar_batch(df1, df2) else self.df1.size()
        Distribution.__init__(self, batch_shape, validate_args=validate_args)

    def expand(self, batch_shape, _instance=None):
        new = self._get_checked_instance(FisherSnedecor, _instance)
        batch_shape = torch.Size(tuple(batch_shape))
        new.df1 = self.df1.expand(batch_shape)
        new.df2 = self.df2.expand(batch_shape)
        new._gamma1 = self._gamma1.expand(batch_shape)
        new._gamma2 = self._gamma2.expand(batch_shape)
        Distribution.__init__(new, batch_shape, validate_args=False)
        new._validate_args = self._validate_args
        return new

    @property
    def mean(self):
        df2 = torch.where(self.df2 <= 2, torch.full_like(self.df2, nan), self.df2)
        return df2 / (df2 - 2)

    @property
    def mode(self):
        mode = (self.df1 - 2) / self.df1 * self.df2 / (self.df2 + 2)
        return torch.where(self.df1 <= 2, torch.full_like(mode, nan), mode)

    @property
    def variance(self):
        df2 = torch.where(self.df2 <= 4, torch.full_like(self.df2, nan), self.df2)
        return 2 * df2.pow(2) * (self.df1 + df2 - 2) / (self.df1 * (df2 - 2).pow(2) * (df2 - 4))

    def rsample(self, sample_shape=()):
        shape = tuple(self._extended_shape(sample_shape))
        X1 = self._gamma1.rsample(sample_shape).view(shape)
        X2 = self._gamma2.rsample(sample_shape).view(shape)
        tiny = torch.finfo(X2.dtype).tiny
        X2 = X2.clamp(min=tiny)
        Y = X1 / X2
        return Y.clamp(min=tiny)

    def log_prob(self, value):
        if self._validate_args:
            self._validate_sample(value)
        ct1 = self.df1 * 0.5
        ct2 = self.df2 * 0.5
        ct3 = self.df1 / self.df2
        t1 = _lgamma(ct1 + ct2) - _lgamma(ct1) - _lgamma(ct2)
        t2 = ct1 * ct3.log() + (ct1 - 1) * torch.log(value)
        t3 = (ct1 + ct2) * torch.log1p(ct3 * value)
        return t1 + t2 - t3


class GeneralizedPareto(Distribution):
    __module__ = "torch.distributions.generalized_pareto"
    """The generalized Pareto distribution (TensorFlow Probability's
    parameterisation): Exponential at concentration 0, Pareto above, bounded
    below."""
    arg_constraints = {"loc": _real, "scale": _positive, "concentration": _real}
    has_rsample = True

    def __init__(self, loc, scale, concentration, validate_args=None):
        self.loc, self.scale, self.concentration = broadcast_all(loc, scale, concentration)
        batch_shape = torch.Size() if _scalar_batch(loc, scale, concentration) else self.loc.size()
        Distribution.__init__(self, batch_shape, validate_args=validate_args)

    def expand(self, batch_shape, _instance=None):
        new = self._get_checked_instance(GeneralizedPareto, _instance)
        batch_shape = torch.Size(tuple(batch_shape))
        new.loc = self.loc.expand(batch_shape)
        new.scale = self.scale.expand(batch_shape)
        new.concentration = self.concentration.expand(batch_shape)
        Distribution.__init__(new, batch_shape, validate_args=False)
        new._validate_args = self._validate_args
        return new

    def rsample(self, sample_shape=()):
        shape = self._extended_shape(sample_shape)
        u = torch.rand(tuple(shape), dtype=self.loc.dtype)
        return self.icdf(u)

    def _eq_zero(self):
        # PyTorch compares with a default-dtype zero, which fails for float64
        # parameters under a float32 default; the comparison itself is this.
        return torch.isclose(self.concentration, torch.zeros_like(self.concentration))

    def log_prob(self, value):
        if self._validate_args:
            self._validate_sample(value)
        z = self._z(value)
        eq_zero = self._eq_zero()
        safe_conc = torch.where(eq_zero, torch.ones_like(self.concentration), self.concentration)
        y = 1 / safe_conc + torch.ones_like(z)
        where_nonzero = torch.where(y == 0, y, y * torch.log1p(safe_conc * z))
        return -self.scale.log() - torch.where(eq_zero, z, where_nonzero)

    def log_survival_function(self, value):
        if self._validate_args:
            self._validate_sample(value)
        z = self._z(value)
        eq_zero = self._eq_zero()
        safe_conc = torch.where(eq_zero, torch.ones_like(self.concentration), self.concentration)
        where_nonzero = -torch.log1p(safe_conc * z) / safe_conc
        return torch.where(eq_zero, -z, where_nonzero)

    def log_cdf(self, value):
        return torch.log1p(-torch.exp(self.log_survival_function(value)))

    def cdf(self, value):
        return torch.exp(self.log_cdf(value))

    def icdf(self, value):
        loc = self.loc
        scale = self.scale
        concentration = self.concentration
        eq_zero = torch.isclose(concentration, torch.zeros_like(concentration))
        safe_conc = torch.where(eq_zero, torch.ones_like(concentration), concentration)
        logu = torch.log1p(-value)
        where_nonzero = loc + scale / safe_conc * torch.expm1(-safe_conc * logu)
        where_zero = loc - scale * logu
        return torch.where(eq_zero, where_zero, where_nonzero)

    def _z(self, x):
        return (x - self.loc) / self.scale

    @property
    def mean(self):
        concentration = self.concentration
        valid = concentration < 1
        safe_conc = torch.where(valid, concentration, torch.full_like(concentration, 0.5))
        result = self.loc + self.scale / (1 - safe_conc)
        return torch.where(valid, result, torch.full_like(result, nan))

    @property
    def variance(self):
        concentration = self.concentration
        valid = concentration < 0.5
        safe_conc = torch.where(valid, concentration, torch.full_like(concentration, 0.25))
        result = self.scale ** 2 / ((1 - safe_conc) ** 2 * (1 - 2 * safe_conc))
        return torch.where(valid, result, torch.full_like(result, nan))

    def entropy(self):
        ans = torch.log(self.scale) + self.concentration + 1
        return ans.expand(tuple(self._batch_shape))

    @property
    def mode(self):
        return self.loc

    @_dependent_property(is_discrete=False, event_dim=0)
    def support(self):
        lower = self.loc
        upper = torch.where(self.concentration < 0, lower - self.scale / self.concentration, torch.full_like(lower, inf))
        return _Interval(lower, upper)


def _eval_poly(y, coef):
    coef = list(coef)
    result = coef.pop()
    while coef:
        result = coef.pop() + y * result
    return result


_I0_COEF_SMALL = [1.0, 3.5156229, 3.0899424, 1.2067492, 0.2659732, 0.360768e-1, 0.45813e-2]
_I0_COEF_LARGE = [0.39894228, 0.1328592e-1, 0.225319e-2, -0.157565e-2, 0.916281e-2, -0.2057706e-1, 0.2635537e-1, -0.1647633e-1, 0.392377e-2]
_I1_COEF_SMALL = [0.5, 0.87890594, 0.51498869, 0.15084934, 0.2658733e-1, 0.301532e-2, 0.32411e-3]
_I1_COEF_LARGE = [0.39894228, -0.3988024e-1, -0.362018e-2, 0.163801e-2, -0.1031555e-1, 0.2282967e-1, -0.2895312e-1, 0.1787654e-1, -0.420059e-2]
_COEF_SMALL = [_I0_COEF_SMALL, _I1_COEF_SMALL]
_COEF_LARGE = [_I0_COEF_LARGE, _I1_COEF_LARGE]


def _log_modified_bessel_fn(x, order=0):
    """log(I_order(x)) for x > 0 and order 0 or 1, with PyTorch's (Abramowitz
    and Stegun's) polynomial approximations."""
    if order != 0 and order != 1:
        raise AssertionError("order must be 0 or 1, got %s" % (order,))
    y = x / 3.75
    y = y * y
    small = _eval_poly(y, _COEF_SMALL[order])
    if order == 1:
        small = x.abs() * small
    small = small.log()
    y = 3.75 / x
    large = x - 0.5 * x.log() + _eval_poly(y, _COEF_LARGE[order]).log()
    return torch.where(x < 3.75, small, large)


def _rejection_sample(loc, concentration, proposal_r, x):
    """Best and Fisher's (1979) rejection sampler for the von Mises."""
    done = torch.zeros(tuple(x.shape), dtype=torch.bool)
    while not bool(done.all()):
        u = torch.rand((3,) + tuple(x.shape), dtype=loc.dtype)
        u1, u2, u3 = u.unbind()
        z = torch.cos(math.pi * u1)
        f = (1 + proposal_r * z) / (proposal_r + z)
        c = concentration * (proposal_r - f)
        accept = ((c * (2 - c) - u2) > 0) | ((c / u2).log() + 1 - c >= 0)
        if bool(accept.any()):
            x = torch.where(accept, (u3 - 0.5).sign() * f.acos(), x)
            done = done | accept
    return (x + math.pi + loc) % (2 * math.pi) - math.pi


class VonMises(Distribution):
    __module__ = "torch.distributions.von_mises"
    """The circular von Mises distribution; `loc` and values are angles
    (any real, read modulo 2 pi). Sampling is Best-Fisher rejection in
    double precision."""
    arg_constraints = {"loc": _real, "concentration": _positive}
    support = _real
    has_rsample = False

    def __init__(self, loc, concentration, validate_args=None):
        self.loc, self.concentration = broadcast_all(loc, concentration)
        batch_shape = self.loc.shape
        Distribution.__init__(self, batch_shape, torch.Size(), validate_args)

    def log_prob(self, value):
        if self._validate_args:
            self._validate_sample(value)
        log_prob = self.concentration * torch.cos(value - self.loc)
        return log_prob - math.log(2 * math.pi) - _log_modified_bessel_fn(self.concentration, order=0)

    @lazy_property
    def _loc(self):
        return self.loc.to(torch.float64)

    @lazy_property
    def _concentration(self):
        return self.concentration.to(torch.float64)

    @lazy_property
    def _proposal_r(self):
        kappa = self._concentration
        tau = 1 + (1 + 4 * kappa ** 2).sqrt()
        rho = (tau - (2 * tau).sqrt()) / (2 * kappa)
        _proposal_r = (1 + rho ** 2) / (2 * rho)
        _proposal_r_taylor = 1 / kappa + kappa
        return torch.where(kappa < 1e-5, _proposal_r_taylor, _proposal_r)

    def sample(self, sample_shape=()):
        with torch.no_grad():
            shape = self._extended_shape(sample_shape)
            x = torch.zeros(tuple(shape), dtype=torch.float64)
            return _rejection_sample(self._loc, self._concentration, self._proposal_r, x).to(self.loc.dtype)

    def expand(self, batch_shape, _instance=None):
        try:
            return Distribution.expand(self, batch_shape)
        except NotImplementedError:
            validate_args = self.__dict__.get("_validate_args")
            batch_shape = torch.Size(tuple(batch_shape))
            loc = self.loc.expand(batch_shape)
            concentration = self.concentration.expand(batch_shape)
            return type(self)(loc, concentration, validate_args=validate_args)

    @property
    def mean(self):
        return self.loc

    @property
    def mode(self):
        return self.loc

    @lazy_property
    def variance(self):
        return 1 - (_log_modified_bessel_fn(self.concentration, order=1) - _log_modified_bessel_fn(self.concentration, order=0)).exp()


class NegativeBinomial(Distribution):
    __module__ = "torch.distributions.negative_binomial"
    """The number of successes before `total_count` failures, each trial a
    success with probability `probs`; sampled as a Gamma-Poisson mixture."""
    arg_constraints = {"total_count": _GreaterThanEq(0), "probs": _HalfOpenInterval(0.0, 1.0), "logits": _real}
    support = _nonnegative_integer

    def __init__(self, total_count, probs=None, logits=None, validate_args=None):
        if (probs is None) == (logits is None):
            raise ValueError("Either `probs` or `logits` must be specified, but not both.")
        if probs is not None:
            self.total_count, self.probs = broadcast_all(total_count, probs)
            self.total_count = self.total_count.type_as(self.probs)
        else:
            self.total_count, self.logits = broadcast_all(total_count, logits)
            self.total_count = self.total_count.type_as(self.logits)
        self._param = self.probs if probs is not None else self.logits
        Distribution.__init__(self, self._param.size(), validate_args=validate_args)

    def expand(self, batch_shape, _instance=None):
        new = self._get_checked_instance(NegativeBinomial, _instance)
        batch_shape = torch.Size(tuple(batch_shape))
        new.total_count = self.total_count.expand(batch_shape)
        if "probs" in self.__dict__:
            new.probs = self.probs.expand(batch_shape)
            new._param = new.probs
        if "logits" in self.__dict__:
            new.logits = self.logits.expand(batch_shape)
            new._param = new.logits
        Distribution.__init__(new, batch_shape, validate_args=False)
        new._validate_args = self._validate_args
        return new

    @property
    def mean(self):
        return self.total_count * torch.exp(self.logits)

    @property
    def mode(self):
        return ((self.total_count - 1) * self.logits.exp()).floor().clamp(min=0.0)

    @property
    def variance(self):
        return self.mean / torch.sigmoid(-self.logits)

    @lazy_property
    def logits(self):
        return probs_to_logits(self.probs, is_binary=True)

    @lazy_property
    def probs(self):
        return logits_to_probs(self.logits, is_binary=True)

    @property
    def param_shape(self):
        return self._param.size()

    @lazy_property
    def _gamma(self):
        # Not validated: total_count may be zero.
        return Gamma(concentration=self.total_count, rate=torch.exp(-self.logits), validate_args=False)

    def sample(self, sample_shape=()):
        with torch.no_grad():
            rate = self._gamma.sample(sample_shape=sample_shape)
            return _poisson_sample(rate)

    def log_prob(self, value):
        if self._validate_args:
            self._validate_sample(value)
        F = torch.nn.functional
        log_unnormalized_prob = self.total_count * F.logsigmoid(-self.logits) + value * F.logsigmoid(self.logits)
        log_normalization = -_lgamma(self.total_count + value) + _lgamma(1.0 + value) + _lgamma(self.total_count)
        # total_count == 0 and value == 0 has probability 1 (lgamma(0) is infinite).
        log_normalization = torch.where(self.total_count + value == 0.0, torch.zeros_like(log_normalization), log_normalization)
        return log_unnormalized_prob - log_normalization


_log_2 = math.log(2)


def _mvdigamma(x, p):
    if not bool(x.gt((p - 1) / 2).all()):
        raise AssertionError("Wrong domain for multivariate digamma function.")
    return _digamma(x.unsqueeze(-1) - torch.arange(p, dtype=x.dtype).div(2).expand(tuple(x.shape) + (-1,))).sum(-1)


def _clamp_above_eps(x):
    return x.clamp(min=torch.finfo(x.dtype).eps)


def _torch_linalg():
    import torch.linalg as LA
    return LA


class Wishart(ExponentialFamily):
    __module__ = "torch.distributions.wishart"
    """The Wishart distribution over positive-definite matrices, given df and
    one of covariance_matrix, precision_matrix or scale_tril. rsample is the
    Bartlett decomposition (chi-square diagonal, normal lower triangle),
    reparameterised through the implicit Gamma gradient."""
    support = _positive_definite
    has_rsample = True
    _mean_carrier_measure = 0

    @property
    def arg_constraints(self):
        return {"covariance_matrix": _positive_definite, "precision_matrix": _positive_definite,
                "scale_tril": _lower_cholesky, "df": _GreaterThan(self.event_shape[-1] - 1)}

    def __init__(self, df, covariance_matrix=None, precision_matrix=None, scale_tril=None, validate_args=None):
        if (covariance_matrix is not None) + (scale_tril is not None) + (precision_matrix is not None) != 1:
            raise AssertionError("Exactly one of covariance_matrix or precision_matrix or scale_tril may be specified.")
        param = next(p for p in (covariance_matrix, precision_matrix, scale_tril) if p is not None)
        if param.dim() < 2:
            raise ValueError("scale_tril must be at least two-dimensional, with optional leading batch dimensions")
        if isinstance(df, _Number):
            batch_shape = torch.Size(tuple(param.shape[:-2]))
            self.df = torch.tensor(df, dtype=param.dtype)
        else:
            batch_shape = torch.Size(tuple(torch.broadcast_shapes(tuple(param.shape[:-2]), tuple(df.shape))))
            self.df = df.expand(batch_shape)
        event_shape = param.shape[-2:]
        if bool(self.df.le(event_shape[-1] - 1).any()):
            raise ValueError("Value of df=%s expected to be greater than ndim - 1 = %d." % (df, event_shape[-1] - 1))
        if scale_tril is not None:
            self.scale_tril = param.expand(tuple(batch_shape) + (-1, -1))
        elif covariance_matrix is not None:
            self.covariance_matrix = param.expand(tuple(batch_shape) + (-1, -1))
        else:
            self.precision_matrix = param.expand(tuple(batch_shape) + (-1, -1))
        Distribution.__init__(self, batch_shape, event_shape, validate_args=validate_args)
        self._batch_dims = [-(x + 1) for x in range(len(self._batch_shape))]
        if scale_tril is not None:
            self._unbroadcasted_scale_tril = scale_tril
        elif covariance_matrix is not None:
            self._unbroadcasted_scale_tril = _cholesky(covariance_matrix)
        else:
            self._unbroadcasted_scale_tril = _precision_to_scale_tril(precision_matrix)
        self._dist_chi2 = self._make_chi2(batch_shape)

    def _make_chi2(self, batch_shape):
        # The Bartlett factor's squared diagonal: chi-square with df - i degrees.
        p = self._event_shape[-1] if self._event_shape else self._unbroadcasted_scale_tril.shape[-1]
        offsets = torch.arange(p, dtype=self._unbroadcasted_scale_tril.dtype).expand(tuple(batch_shape) + (-1,))
        return Chi2(df=self.df.unsqueeze(-1) - offsets)

    def expand(self, batch_shape, _instance=None):
        new = self._get_checked_instance(Wishart, _instance)
        batch_shape = torch.Size(tuple(batch_shape))
        cov_shape = _size(batch_shape, self.event_shape)
        new._unbroadcasted_scale_tril = self._unbroadcasted_scale_tril.expand(cov_shape)
        new.df = self.df.expand(batch_shape)
        new._batch_dims = [-(x + 1) for x in range(len(batch_shape))]
        if "covariance_matrix" in self.__dict__:
            new.covariance_matrix = self.covariance_matrix.expand(cov_shape)
        if "scale_tril" in self.__dict__:
            new.scale_tril = self.scale_tril.expand(cov_shape)
        if "precision_matrix" in self.__dict__:
            new.precision_matrix = self.precision_matrix.expand(cov_shape)
        Distribution.__init__(new, batch_shape, self.event_shape, validate_args=False)
        new._dist_chi2 = new._make_chi2(batch_shape)
        new._validate_args = self._validate_args
        return new

    @lazy_property
    def scale_tril(self):
        return self._unbroadcasted_scale_tril.expand(_size(self._batch_shape, self._event_shape))

    @lazy_property
    def covariance_matrix(self):
        L = self._unbroadcasted_scale_tril
        return (L @ L.transpose(-2, -1)).expand(_size(self._batch_shape, self._event_shape))

    @lazy_property
    def precision_matrix(self):
        _torch_linalg()
        L = self._unbroadcasted_scale_tril
        identity = torch.eye(self._event_shape[-1], dtype=L.dtype)
        return torch.cholesky_solve(identity, L).expand(_size(self._batch_shape, self._event_shape))

    def _batch_view(self, x):
        return x.view(tuple(self._batch_shape) + (1, 1))

    @property
    def mean(self):
        return self._batch_view(self.df) * self.covariance_matrix

    @property
    def mode(self):
        factor = self.df - self.covariance_matrix.shape[-1] - 1
        factor = torch.where(factor <= 0, torch.full_like(factor, nan), factor)
        return self._batch_view(factor) * self.covariance_matrix

    @property
    def variance(self):
        V = self.covariance_matrix
        diag_V = V.diagonal(dim1=-2, dim2=-1)
        return self._batch_view(self.df) * (V.pow(2) + diag_V.unsqueeze(-1) * diag_V.unsqueeze(-2))

    def _bartlett_factor(self, sample_shape=()):
        p = self._event_shape[-1]
        noise = _clamp_above_eps(self._dist_chi2.rsample(sample_shape).sqrt()).diag_embed(dim1=-2, dim2=-1)
        lower = torch.randn(tuple(sample_shape) + tuple(self._batch_shape) + (p, p), dtype=noise.dtype).tril(-1)
        return self._unbroadcasted_scale_tril @ (noise + lower)

    def _bartlett_sampling(self, sample_shape=()):
        chol = self._bartlett_factor(sample_shape)
        return chol @ chol.transpose(-2, -1)

    def _maybe_singular(self, chol):
        # chol is lower triangular with a nonzero diagonal, so chol chol^T is
        # positive definite unless a pivot is lost to rounding; only then is
        # the (costly) Cholesky check worth running.
        c = chol.detach()
        pivots = c.diagonal(dim1=-2, dim2=-1).pow(2).amin(-1)
        size = (c * c).sum(-1).amax(-1)
        return bool((pivots <= 1e3 * torch.finfo(c.dtype).eps * size).any())

    def _singular(self, sample, sample_shape):
        bad = ~self.support.check(sample)
        if self._batch_shape:
            bad = bad.reshape(tuple(sample_shape) + (-1,)).any(-1)
        return bad

    def rsample(self, sample_shape=(), max_try_correction=None):
        """Bartlett draws; singular ones (possible for ndim - 1 < df < ndim)
        are redrawn up to `max_try_correction` (10) times, as PyTorch
        intends (its own check is inverted, so it redraws every sample)."""
        if max_try_correction is None:
            max_try_correction = 10
        sample_shape = torch.Size(tuple(sample_shape))
        chol = self._bartlett_factor(sample_shape)
        sample = chol @ chol.transpose(-2, -1)
        if not self._maybe_singular(chol):
            return sample
        is_singular = self._singular(sample.detach(), sample_shape)
        for _ in range(max_try_correction):
            if not bool(is_singular.any()):
                break
            sample_new = self._bartlett_sampling(sample_shape)
            mask = is_singular.reshape(tuple(is_singular.shape) + (1,) * (len(self._batch_shape) + 2))
            sample = torch.where(mask, sample_new, sample)
            is_singular = self._singular(sample.detach(), sample_shape)
        return sample

    def log_prob(self, value):
        if self._validate_args:
            self._validate_sample(value)
        LA = _torch_linalg()
        nu = self.df
        p = self._event_shape[-1]
        L = self._unbroadcasted_scale_tril
        return (-nu * (p * _log_2 / 2 + L.diagonal(dim1=-2, dim2=-1).log().sum(-1))
                - torch.mvlgamma(nu / 2, p=p)
                + (nu - p - 1) / 2 * LA.slogdet(value).logabsdet
                - torch.cholesky_solve(value, L).diagonal(dim1=-2, dim2=-1).sum(dim=-1) / 2)

    def entropy(self):
        nu = self.df
        p = self._event_shape[-1]
        L = self._unbroadcasted_scale_tril
        return ((p + 1) * (p * _log_2 / 2 + L.diagonal(dim1=-2, dim2=-1).log().sum(-1))
                + torch.mvlgamma(nu / 2, p=p)
                - (nu - p - 1) / 2 * _mvdigamma(nu / 2, p=p)
                + nu * p / 2)

    @property
    def _natural_params(self):
        nu = self.df
        p = self._event_shape[-1]
        return -self.precision_matrix / 2, (nu - p - 1) / 2

    def _log_normalizer(self, x, y):
        LA = _torch_linalg()
        p = self._event_shape[-1]
        return (y + (p + 1) / 2) * (-LA.slogdet(-2 * x).logabsdet + _log_2 * p) + torch.mvlgamma(y + (p + 1) / 2, p=p)


class LKJCholesky(Distribution):
    __module__ = "torch.distributions.lkj_cholesky"
    """The LKJ distribution over Cholesky factors of correlation matrices:
    L L^T has density proportional to det^(concentration - 1). Sampling is
    the onion method (Lewandowski, Kurowicka and Joe, 2009, section 3.2)."""
    arg_constraints = {"concentration": _positive}
    support = _corr_cholesky

    def __init__(self, dim, concentration=1.0, validate_args=None):
        if dim < 2:
            raise ValueError("Expected dim to be an integer greater than or equal to 2. Found dim=%s." % (dim,))
        self.dim = dim
        (self.concentration,) = broadcast_all(concentration)
        batch_shape = self.concentration.size()
        event_shape = torch.Size((dim, dim))
        # Row k's squared off-diagonal norm is Beta(k / 2, eta + (dim - 2) / 2 -
        # (k - 1) / 2). (PyTorch 2.11 draws Beta(k - 1/2, ...), which is not the
        # onion method: its rows below the second come out too spread.)
        marginal_conc = self.concentration + 0.5 * (self.dim - 2)
        offset = torch.arange(self.dim - 1, dtype=self.concentration.dtype)
        offset = torch.cat([torch.zeros(1, dtype=offset.dtype), offset])
        beta_conc1 = 0.5 * (offset + 1)
        beta_conc0 = marginal_conc.unsqueeze(-1) - 0.5 * offset
        self._beta = Beta(beta_conc1, beta_conc0)
        Distribution.__init__(self, batch_shape, event_shape, validate_args)

    def expand(self, batch_shape, _instance=None):
        new = self._get_checked_instance(LKJCholesky, _instance)
        batch_shape = torch.Size(tuple(batch_shape))
        new.dim = self.dim
        new.concentration = self.concentration.expand(batch_shape)
        new._beta = self._beta.expand(_size(batch_shape, (self.dim,)))
        Distribution.__init__(new, batch_shape, self.event_shape, validate_args=False)
        new._validate_args = self._validate_args
        return new

    def sample(self, sample_shape=()):
        with torch.no_grad():
            y = self._beta.sample(sample_shape).unsqueeze(-1)
            u_normal = torch.randn(tuple(self._extended_shape(sample_shape)), dtype=y.dtype).tril(-1)
            # The first row is all zeros; clamping its norm leaves it zero.
            norm = u_normal.norm(dim=-1, keepdim=True).clamp(min=torch.finfo(y.dtype).tiny)
            w = torch.sqrt(y) * (u_normal / norm)
            eps = torch.finfo(w.dtype).tiny
            diag_elems = torch.clamp(1 - torch.sum(w ** 2, dim=-1), min=eps).sqrt()
            return w + torch.diag_embed(diag_elems)

    def log_prob(self, value):
        # Stan's LKJ Cholesky density: prod L_ii^(2 (eta - 1) + dim - i).
        if self._validate_args:
            self._validate_sample(value)
        diag_elems = value.diagonal(dim1=-1, dim2=-2)[..., 1:]
        order = torch.arange(2, self.dim + 1, dtype=self.concentration.dtype)
        order = 2 * (self.concentration - 1).unsqueeze(-1) + self.dim - order
        unnormalized_log_pdf = torch.sum(order * diag_elems.log(), dim=-1)
        dm1 = self.dim - 1
        alpha = self.concentration + 0.5 * dm1
        denominator = _lgamma(alpha) * dm1
        numerator = torch.mvlgamma(alpha - 0.5, dm1)
        pi_constant = 0.5 * dm1 * math.log(math.pi)
        normalize_term = pi_constant + numerator - denominator
        return unnormalized_log_pdf - normalize_term


# ---- constraint registry (biject_to / transform_to) ------------------------------------------
class ConstraintRegistry:
    __module__ = "torch.distributions.constraint_registry"
    """Maps constraints to transforms whose codomain they are."""

    def __init__(self):
        self._registry = {}

    def register(self, constraint, factory=None):
        if factory is None:
            return lambda factory: self.register(constraint, factory)
        if isinstance(constraint, Constraint):
            constraint = type(constraint)
        self._registry[constraint] = factory
        return factory

    def __call__(self, constraint):
        try:
            factory = self._registry[type(constraint)]
        except KeyError:
            raise NotImplementedError("Cannot transform %s constraints" % type(constraint).__name__)
        return factory(constraint)


biject_to = ConstraintRegistry()
transform_to = ConstraintRegistry()


def _transform_to_real(constraint):
    return identity_transform


def _transform_to_independent(constraint):
    return IndependentTransform(transform_to(constraint.base_constraint), constraint.reinterpreted_batch_ndims)


def _biject_to_independent(constraint):
    return IndependentTransform(biject_to(constraint.base_constraint), constraint.reinterpreted_batch_ndims)


def _transform_to_positive(constraint):
    # PyTorch registers `positive` and `greater_than` under the same class,
    # the latter last, so both give exp followed by a shift.
    return ComposeTransform([ExpTransform(), AffineTransform(constraint.lower_bound, 1)])


def _transform_to_less_than(constraint):
    return ComposeTransform([ExpTransform(), AffineTransform(constraint.upper_bound, -1)])


def _transform_to_interval(constraint):
    lower_is_0 = isinstance(constraint.lower_bound, _Number) and constraint.lower_bound == 0
    upper_is_1 = isinstance(constraint.upper_bound, _Number) and constraint.upper_bound == 1
    if lower_is_0 and upper_is_1:
        return SigmoidTransform()
    loc = constraint.lower_bound
    scale = constraint.upper_bound - constraint.lower_bound
    return ComposeTransform([SigmoidTransform(), AffineTransform(loc, scale)])


for _registry in (biject_to, transform_to):
    _registry.register(_Real, _transform_to_real)
    _registry.register(_GreaterThan, _transform_to_positive)
    _registry.register(_GreaterThanEq, _transform_to_positive)
    _registry.register(_LessThan, _transform_to_less_than)
    _registry.register(_Interval, _transform_to_interval)
    _registry.register(_HalfOpenInterval, _transform_to_interval)
biject_to.register(_IndependentConstraint, _biject_to_independent)
transform_to.register(_IndependentConstraint, _transform_to_independent)
biject_to.register(_Simplex, lambda c: StickBreakingTransform())
transform_to.register(_Simplex, lambda c: SoftmaxTransform())
transform_to.register(_LowerCholesky, lambda c: LowerCholeskyTransform())
transform_to.register(_PositiveDefinite, lambda c: PositiveDefiniteTransform())
transform_to.register(_PositiveSemidefinite, lambda c: PositiveDefiniteTransform())
biject_to.register(_CorrCholesky, lambda c: CorrCholeskyTransform())
transform_to.register(_CorrCholesky, lambda c: CorrCholeskyTransform())
biject_to.register(_Cat, lambda c: CatTransform([biject_to(part) for part in c.cseq], c.dim, c.lengths))
transform_to.register(_Cat, lambda c: CatTransform([transform_to(part) for part in c.cseq], c.dim, c.lengths))
biject_to.register(_Stack, lambda c: StackTransform([biject_to(part) for part in c.cseq], c.dim))
transform_to.register(_Stack, lambda c: StackTransform([transform_to(part) for part in c.cseq], c.dim))


# ---- KL divergence --------------------------------------------------------------------------
_KL_REGISTRY = {}
_KL_MEMOIZE = {}


def register_kl(type_p, type_q):
    """Decorator registering a pairwise KL(p || q) implementation; the most
    specific registered pair (by class hierarchy) is dispatched."""
    # PyTorch's own check, which lets any class through.
    if not isinstance(type_p, type) and issubclass(type_p, Distribution):
        raise TypeError("Expected type_p to be a Distribution subclass but got %s" % (type_p,))
    if not isinstance(type_q, type) and issubclass(type_q, Distribution):
        raise TypeError("Expected type_q to be a Distribution subclass but got %s" % (type_q,))

    def decorator(fun):
        _KL_REGISTRY[(type_p, type_q)] = fun
        _KL_MEMOIZE.clear()
        return fun
    return decorator


def _match_le(a, b):
    for x, y in zip(a, b):
        if not issubclass(x, y):
            return False
        if x is not y:
            break
    return True


def _match_min(items):
    best = items[0]
    for item in items[1:]:
        # min() replaces the current best when item < best: <= and not ==.
        if _match_le(item, best) and item != best:
            best = item
    return best


def _dispatch_kl(type_p, type_q):
    matches = [(sp, sq) for (sp, sq) in _KL_REGISTRY if issubclass(type_p, sp) and issubclass(type_q, sq)]
    if not matches:
        return NotImplemented
    left_p, left_q = _match_min(matches)
    right_q, right_p = _match_min([(m[1], m[0]) for m in matches])
    left_fun = _KL_REGISTRY[(left_p, left_q)]
    return left_fun


def kl_divergence(p, q):
    """KL(p || q), batched over the distributions' batch shape."""
    key = (type(p), type(q))
    if key in _KL_MEMOIZE:
        fun = _KL_MEMOIZE[key]
    else:
        fun = _dispatch_kl(type(p), type(q))
        _KL_MEMOIZE[key] = fun
    if fun is NotImplemented:
        raise NotImplementedError("No KL(p || q) is implemented for p type %s and q type %s" % (type(p).__name__, type(q).__name__))
    return fun(p, q)


def _infinite_like(tensor):
    return torch.full_like(tensor, inf)


@register_kl(Bernoulli, Bernoulli)
def _kl_bernoulli_bernoulli(p, q):
    t1 = p.probs * (_softplus(-q.logits) - _softplus(-p.logits))
    t1 = torch.where(q.probs == 0, torch.full_like(t1, inf), t1)
    t1 = torch.where(p.probs == 0, torch.zeros_like(t1), t1)
    t2 = (1 - p.probs) * (_softplus(q.logits) - _softplus(p.logits))
    t2 = torch.where(q.probs == 1, torch.full_like(t2, inf), t2)
    t2 = torch.where(p.probs == 1, torch.zeros_like(t2), t2)
    return t1 + t2


@register_kl(Beta, Beta)
def _kl_beta_beta(p, q):
    sum_params_p = p.concentration1 + p.concentration0
    sum_params_q = q.concentration1 + q.concentration0
    t1 = _lgamma(q.concentration1) + _lgamma(q.concentration0) + _lgamma(sum_params_p)
    t2 = _lgamma(p.concentration1) + _lgamma(p.concentration0) + _lgamma(sum_params_q)
    t3 = (p.concentration1 - q.concentration1) * _digamma(p.concentration1)
    t4 = (p.concentration0 - q.concentration0) * _digamma(p.concentration0)
    t5 = (sum_params_q - sum_params_p) * _digamma(sum_params_p)
    return t1 - t2 + t3 + t4 + t5


@register_kl(Binomial, Binomial)
def _kl_binomial_binomial(p, q):
    if (p.total_count < q.total_count).any():
        raise NotImplementedError("KL between Binomials where q.total_count > p.total_count is not implemented")
    kl = p.total_count * (p.probs * (p.logits - q.logits) + (-p.probs).log1p() - (-q.probs).log1p())
    return torch.where(p.total_count > q.total_count, torch.full_like(kl, inf), kl)


@register_kl(Categorical, Categorical)
def _kl_categorical_categorical(p, q):
    t = p.probs * (p.logits - q.logits)
    t = torch.where((q.probs == 0).expand_as(t), torch.full_like(t, inf), t)
    t = torch.where((p.probs == 0).expand_as(t), torch.zeros_like(t), t)
    return t.sum(-1)


@register_kl(Dirichlet, Dirichlet)
def _kl_dirichlet_dirichlet(p, q):
    sum_p_concentration = p.concentration.sum(-1)
    sum_q_concentration = q.concentration.sum(-1)
    t1 = _lgamma(sum_p_concentration) - _lgamma(sum_q_concentration)
    t2 = (_lgamma(p.concentration) - _lgamma(q.concentration)).sum(-1)
    t3 = p.concentration - q.concentration
    t4 = _digamma(p.concentration) - _digamma(sum_p_concentration).unsqueeze(-1)
    return t1 - t2 + (t3 * t4).sum(-1)


@register_kl(Exponential, Exponential)
def _kl_exponential_exponential(p, q):
    rate_ratio = q.rate / p.rate
    return -rate_ratio.log() + rate_ratio - 1


@register_kl(Gamma, Gamma)
def _kl_gamma_gamma(p, q):
    t1 = q.concentration * (p.rate / q.rate).log()
    t2 = _lgamma(q.concentration) - _lgamma(p.concentration)
    t3 = (p.concentration - q.concentration) * _digamma(p.concentration)
    t4 = (q.rate - p.rate) * (p.concentration / p.rate)
    return t1 + t2 + t3 + t4


@register_kl(Geometric, Geometric)
def _kl_geometric_geometric(p, q):
    return -p.entropy() - torch.log1p(-q.probs) / p.probs - q.logits


@register_kl(Laplace, Laplace)
def _kl_laplace_laplace(p, q):
    scale_ratio = p.scale / q.scale
    loc_abs_diff = (p.loc - q.loc).abs()
    t1 = -scale_ratio.log()
    t2 = loc_abs_diff / q.scale
    t3 = scale_ratio * torch.exp(-loc_abs_diff / p.scale)
    return t1 + t2 + t3 - 1


@register_kl(MultivariateNormal, MultivariateNormal)
def _kl_multivariatenormal_multivariatenormal(p, q):
    if tuple(p.event_shape) != tuple(q.event_shape):
        raise ValueError("KL-divergence between two Multivariate Normals with different event shapes cannot be computed")
    half_term1 = q._unbroadcasted_scale_tril.diagonal(dim1=-2, dim2=-1).log().sum(-1) - p._unbroadcasted_scale_tril.diagonal(dim1=-2, dim2=-1).log().sum(-1)
    combined_batch_shape = torch.broadcast_shapes(tuple(q._unbroadcasted_scale_tril.shape[:-2]), tuple(p._unbroadcasted_scale_tril.shape[:-2]))
    n = p.event_shape[0]
    q_scale_tril = q._unbroadcasted_scale_tril.expand(tuple(combined_batch_shape) + (n, n))
    p_scale_tril = p._unbroadcasted_scale_tril.expand(tuple(combined_batch_shape) + (n, n))
    term2 = _batch_trace_XXT(_solve_triangular_lower(q_scale_tril, p_scale_tril))
    term3 = _batch_mahalanobis(q._unbroadcasted_scale_tril, q.loc - p.loc)
    return half_term1 + 0.5 * (term2 + term3 - n)


@register_kl(LowRankMultivariateNormal, LowRankMultivariateNormal)
def _kl_lowrankmultivariatenormal_lowrankmultivariatenormal(p, q):
    if tuple(p.event_shape) != tuple(q.event_shape):
        raise ValueError("KL-divergence between two Low Rank Multivariate Normals with different event shapes cannot be computed")
    term1 = _batch_lowrank_logdet(q._unbroadcasted_cov_factor, q._unbroadcasted_cov_diag, q._capacitance_tril) - _batch_lowrank_logdet(p._unbroadcasted_cov_factor, p._unbroadcasted_cov_diag, p._capacitance_tril)
    term3 = _batch_lowrank_mahalanobis(q._unbroadcasted_cov_factor, q._unbroadcasted_cov_diag, q.loc - p.loc, q._capacitance_tril)
    qWt_qDinv = q._unbroadcasted_cov_factor.mT / q._unbroadcasted_cov_diag.unsqueeze(-2)
    A = _solve_triangular_lower(q._capacitance_tril, qWt_qDinv)
    term21 = (p._unbroadcasted_cov_diag / q._unbroadcasted_cov_diag).sum(-1)
    term22 = _batch_trace_XXT(p._unbroadcasted_cov_factor * q._unbroadcasted_cov_diag.rsqrt().unsqueeze(-1))
    term23 = _batch_trace_XXT(A * p._unbroadcasted_cov_diag.sqrt().unsqueeze(-2))
    term24 = _batch_trace_XXT(A.matmul(p._unbroadcasted_cov_factor))
    term2 = term21 + term22 - term23 - term24
    return 0.5 * (term1 + term2 + term3 - p.event_shape[0])


@register_kl(Normal, Normal)
def _kl_normal_normal(p, q):
    var_ratio = (p.scale / q.scale).pow(2)
    t1 = ((p.loc - q.loc) / q.scale).pow(2)
    return 0.5 * (var_ratio + t1 - 1 - var_ratio.log())


@register_kl(OneHotCategorical, OneHotCategorical)
def _kl_onehotcategorical_onehotcategorical(p, q):
    return _kl_categorical_categorical(p._categorical, q._categorical)


@register_kl(Poisson, Poisson)
def _kl_poisson_poisson(p, q):
    return p.rate * (p.rate.log() - q.rate.log()) - (p.rate - q.rate)


@register_kl(TransformedDistribution, TransformedDistribution)
def _kl_transformed_transformed(p, q):
    if p.transforms != q.transforms:
        raise NotImplementedError
    if tuple(p.event_shape) != tuple(q.event_shape):
        raise NotImplementedError
    return kl_divergence(p.base_dist, q.base_dist)


@register_kl(Uniform, Uniform)
def _kl_uniform_uniform(p, q):
    result = ((q.high - q.low) / (p.high - p.low)).log()
    return torch.where((q.low > p.low) | (q.high < p.high), torch.full_like(result, inf), result)


@register_kl(HalfNormal, HalfNormal)
def _kl_halfnormal_halfnormal(p, q):
    return _kl_normal_normal(p.base_dist, q.base_dist)


@register_kl(Cauchy, Cauchy)
def _kl_cauchy_cauchy(p, q):
    t1 = ((p.scale + q.scale).pow(2) + (p.loc - q.loc).pow(2)).log()
    t2 = (4 * p.scale * q.scale).log()
    return t1 - t2


@register_kl(Exponential, Gamma)
def _kl_exponential_gamma(p, q):
    ratio = q.rate / p.rate
    t1 = -q.concentration * torch.log(ratio)
    return t1 + ratio + _lgamma(q.concentration) + q.concentration * euler_constant - (1 + euler_constant)


@register_kl(Normal, Laplace)
def _kl_normal_laplace(p, q):
    loc_diff = p.loc - q.loc
    scale_ratio = p.scale / q.scale
    loc_diff_scale_ratio = loc_diff / p.scale
    t1 = torch.log(scale_ratio)
    t2 = math.sqrt(2 / math.pi) * p.scale * torch.exp(-0.5 * loc_diff_scale_ratio.pow(2))
    t3 = loc_diff * torch.erf(math.sqrt(0.5) * loc_diff_scale_ratio)
    return -t1 + (t2 + t3) / q.scale - 0.5 * (1 + math.log(0.5 * math.pi))


@register_kl(Uniform, Normal)
def _kl_uniform_normal(p, q):
    common_term = p.high - p.low
    t1 = (math.sqrt(math.pi * 2) * q.scale / common_term).log()
    t2 = common_term.pow(2) / 12
    t3 = ((p.high + p.low - 2 * q.loc) / 2).pow(2)
    return t1 + 0.5 * (t2 + t3) / q.scale.pow(2)


@register_kl(Independent, Independent)
def _kl_independent_independent(p, q):
    if p.reinterpreted_batch_ndims != q.reinterpreted_batch_ndims:
        raise NotImplementedError
    result = kl_divergence(p.base_dist, q.base_dist)
    return _sum_rightmost(result, p.reinterpreted_batch_ndims)


def _x_log_x(tensor):
    fn = getattr(torch, "xlogy", None)
    return fn(tensor, tensor) if fn is not None else _xlogy(tensor, tensor)


def _set_inf(result, mask):
    """PyTorch's `result[mask] = inf` (zero gradient where masked)."""
    return torch.where(mask, torch.full_like(result, inf), result)


@register_kl(ExponentialFamily, ExponentialFamily)
def _kl_expfamily_expfamily(p, q):
    # The Bregman divergence of the log normalizer (same family only).
    if type(p) is not type(q):
        raise NotImplementedError("The cross KL-divergence between different exponential families cannot                             be computed using Bregman divergences")
    p_nparams = [np_.detach().requires_grad_() for np_ in p._natural_params]
    q_nparams = q._natural_params
    lg_normal = p._log_normalizer(*p_nparams)
    gradients = torch._autograd_grad(lg_normal.sum(), p_nparams, create_graph=True)
    result = q._log_normalizer(*q_nparams) - lg_normal
    for pnp, qnp, g in zip(p_nparams, q_nparams, gradients):
        term = (qnp - pnp) * g
        result = result - _sum_rightmost(term, len(q.event_shape))
    return result


@register_kl(Gumbel, Gumbel)
def _kl_gumbel_gumbel(p, q):
    ct1 = p.scale / q.scale
    ct2 = q.loc / q.scale
    ct3 = p.loc / q.scale
    t1 = -ct1.log() - ct2 + ct3
    t2 = ct1 * euler_constant
    t3 = torch.exp(ct2 + _lgamma(1 + ct1) - ct3)
    return t1 + t2 + t3 - (1 + euler_constant)


@register_kl(Pareto, Pareto)
def _kl_pareto_pareto(p, q):
    scale_ratio = p.scale / q.scale
    alpha_ratio = q.alpha / p.alpha
    t1 = q.alpha * scale_ratio.log()
    t2 = -alpha_ratio.log()
    result = t1 + t2 + alpha_ratio - 1
    return _set_inf(result, p.support.lower_bound < q.support.lower_bound)


@register_kl(ContinuousBernoulli, ContinuousBernoulli)
def _kl_continuous_bernoulli_continuous_bernoulli(p, q):
    t1 = p.mean * (p.logits - q.logits)
    t2 = p._cont_bern_log_norm() + torch.log1p(-p.probs)
    t3 = -q._cont_bern_log_norm() - torch.log1p(-q.probs)
    return t1 + t2 + t3


@register_kl(Bernoulli, Poisson)
def _kl_bernoulli_poisson(p, q):
    return -p.entropy() - (p.probs * q.rate.log() - q.rate)


@register_kl(Beta, ContinuousBernoulli)
def _kl_beta_continuous_bernoulli(p, q):
    return -p.entropy() - p.mean * q.logits - torch.log1p(-q.probs) - q._cont_bern_log_norm()


@register_kl(Beta, Pareto)
def _kl_beta_infinity(p, q):
    return _infinite_like(p.concentration1)


@register_kl(Beta, Exponential)
def _kl_beta_exponential(p, q):
    return -p.entropy() - q.rate.log() + q.rate * (p.concentration1 / (p.concentration1 + p.concentration0))


@register_kl(Beta, Gamma)
def _kl_beta_gamma(p, q):
    t1 = -p.entropy()
    t2 = _lgamma(q.concentration) - q.concentration * q.rate.log()
    t3 = (q.concentration - 1) * (_digamma(p.concentration1) - _digamma(p.concentration1 + p.concentration0))
    t4 = q.rate * p.concentration1 / (p.concentration1 + p.concentration0)
    return t1 + t2 - t3 + t4


@register_kl(Beta, Normal)
def _kl_beta_normal(p, q):
    E_beta = p.concentration1 / (p.concentration1 + p.concentration0)
    var_normal = q.scale.pow(2)
    t1 = -p.entropy()
    t2 = 0.5 * (var_normal * 2 * math.pi).log()
    t3 = (E_beta * (1 - E_beta) / (p.concentration1 + p.concentration0 + 1) + E_beta.pow(2)) * 0.5
    t4 = q.loc * E_beta
    t5 = q.loc.pow(2) * 0.5
    return t1 + t2 + (t3 - t4 + t5) / var_normal


@register_kl(Beta, Uniform)
def _kl_beta_uniform(p, q):
    result = -p.entropy() + (q.high - q.low).log()
    return _set_inf(result, (q.low > p.support.lower_bound) | (q.high < p.support.upper_bound))


@register_kl(ContinuousBernoulli, Pareto)
def _kl_continuous_bernoulli_infinity(p, q):
    return _infinite_like(p.probs)


@register_kl(ContinuousBernoulli, Exponential)
def _kl_continuous_bernoulli_exponential(p, q):
    return -p.entropy() - torch.log(q.rate) + q.rate * p.mean


@register_kl(ContinuousBernoulli, Normal)
def _kl_continuous_bernoulli_normal(p, q):
    t1 = -p.entropy()
    t2 = 0.5 * (math.log(2.0 * math.pi) + torch.square(q.loc / q.scale)) + torch.log(q.scale)
    t3 = (p.variance + torch.square(p.mean) - 2.0 * q.loc * p.mean) / (2.0 * torch.square(q.scale))
    return t1 + t2 + t3


@register_kl(ContinuousBernoulli, Uniform)
def _kl_continuous_bernoulli_uniform(p, q):
    result = -p.entropy() + (q.high - q.low).log()
    outside = torch.ge(q.low, p.support.lower_bound) | torch.le(q.high, p.support.upper_bound)
    return torch.where(outside, torch.ones_like(result) * inf, result)


@register_kl(Exponential, Beta)
@register_kl(Exponential, ContinuousBernoulli)
@register_kl(Exponential, Pareto)
@register_kl(Exponential, Uniform)
def _kl_exponential_infinity(p, q):
    return _infinite_like(p.rate)


@register_kl(Exponential, Gumbel)
def _kl_exponential_gumbel(p, q):
    scale_rate_prod = p.rate * q.scale
    loc_scale_ratio = q.loc / q.scale
    t1 = scale_rate_prod.log() - 1
    t2 = torch.exp(loc_scale_ratio) * scale_rate_prod / (scale_rate_prod + 1)
    t3 = scale_rate_prod.reciprocal()
    return t1 - loc_scale_ratio + t2 + t3


@register_kl(Exponential, Normal)
def _kl_exponential_normal(p, q):
    var_normal = q.scale.pow(2)
    rate_sqr = p.rate.pow(2)
    t1 = 0.5 * torch.log(rate_sqr * var_normal * 2 * math.pi)
    t2 = rate_sqr.reciprocal()
    t3 = q.loc / p.rate
    t4 = q.loc.pow(2) * 0.5
    return t1 - 1 + (t2 - t3 + t4) / var_normal


@register_kl(Gamma, Beta)
@register_kl(Gamma, ContinuousBernoulli)
@register_kl(Gamma, Pareto)
@register_kl(Gamma, Uniform)
def _kl_gamma_infinity(p, q):
    return _infinite_like(p.concentration)


@register_kl(Gamma, Exponential)
def _kl_gamma_exponential(p, q):
    return -p.entropy() - q.rate.log() + q.rate * p.concentration / p.rate


@register_kl(Gamma, Gumbel)
def _kl_gamma_gumbel(p, q):
    beta_scale_prod = p.rate * q.scale
    loc_scale_ratio = q.loc / q.scale
    t1 = (p.concentration - 1) * _digamma(p.concentration) - _lgamma(p.concentration) - p.concentration
    t2 = beta_scale_prod.log() + p.concentration / beta_scale_prod
    t3 = torch.exp(loc_scale_ratio) * (1 + beta_scale_prod.reciprocal()).pow(-p.concentration) - loc_scale_ratio
    return t1 + t2 + t3


@register_kl(Gamma, Normal)
def _kl_gamma_normal(p, q):
    var_normal = q.scale.pow(2)
    beta_sqr = p.rate.pow(2)
    t1 = 0.5 * torch.log(beta_sqr * var_normal * 2 * math.pi) - p.concentration - _lgamma(p.concentration)
    t2 = 0.5 * (p.concentration.pow(2) + p.concentration) / beta_sqr
    t3 = q.loc * p.concentration / p.rate
    t4 = 0.5 * q.loc.pow(2)
    return t1 + (p.concentration - 1) * _digamma(p.concentration) + (t2 - t3 + t4) / var_normal


@register_kl(Gumbel, Beta)
@register_kl(Gumbel, ContinuousBernoulli)
@register_kl(Gumbel, Exponential)
@register_kl(Gumbel, Gamma)
@register_kl(Gumbel, Pareto)
@register_kl(Gumbel, Uniform)
def _kl_gumbel_infinity(p, q):
    return _infinite_like(p.loc)


@register_kl(Gumbel, Normal)
def _kl_gumbel_normal(p, q):
    param_ratio = p.scale / q.scale
    t1 = (param_ratio / math.sqrt(2 * math.pi)).log()
    t2 = (math.pi * param_ratio * 0.5).pow(2) / 3
    t3 = ((p.loc + p.scale * euler_constant - q.loc) / q.scale).pow(2) * 0.5
    return -t1 + t2 + t3 - (euler_constant + 1)


@register_kl(Laplace, Beta)
@register_kl(Laplace, ContinuousBernoulli)
@register_kl(Laplace, Exponential)
@register_kl(Laplace, Gamma)
@register_kl(Laplace, Pareto)
@register_kl(Laplace, Uniform)
def _kl_laplace_infinity(p, q):
    return _infinite_like(p.loc)


@register_kl(Laplace, Normal)
def _kl_laplace_normal(p, q):
    var_normal = q.scale.pow(2)
    scale_sqr_var_ratio = p.scale.pow(2) / var_normal
    t1 = 0.5 * torch.log(2 * scale_sqr_var_ratio / math.pi)
    t2 = 0.5 * p.loc.pow(2)
    t3 = p.loc * q.loc
    t4 = 0.5 * q.loc.pow(2)
    return -t1 + scale_sqr_var_ratio + (t2 - t3 + t4) / var_normal - 1


@register_kl(LowRankMultivariateNormal, MultivariateNormal)
def _kl_lowrankmultivariatenormal_multivariatenormal(p, q):
    if tuple(p.event_shape) != tuple(q.event_shape):
        raise ValueError("KL-divergence between two (Low Rank) Multivariate Normals with                          different event shapes cannot be computed")
    term1 = 2 * q._unbroadcasted_scale_tril.diagonal(dim1=-2, dim2=-1).log().sum(-1) - _batch_lowrank_logdet(
        p._unbroadcasted_cov_factor, p._unbroadcasted_cov_diag, p._capacitance_tril)
    term3 = _batch_mahalanobis(q._unbroadcasted_scale_tril, q.loc - p.loc)
    combined_batch_shape = tuple(torch.broadcast_shapes(tuple(q._unbroadcasted_scale_tril.shape[:-2]), tuple(p._unbroadcasted_cov_factor.shape[:-2])))
    n = p.event_shape[0]
    q_scale_tril = q._unbroadcasted_scale_tril.expand(combined_batch_shape + (n, n))
    p_cov_factor = p._unbroadcasted_cov_factor.expand(combined_batch_shape + (n, p.cov_factor.size(-1)))
    p_cov_diag = torch.diag_embed(p._unbroadcasted_cov_diag.sqrt()).expand(combined_batch_shape + (n, n))
    term21 = _batch_trace_XXT(_solve_triangular_lower(q_scale_tril, p_cov_factor))
    term22 = _batch_trace_XXT(_solve_triangular_lower(q_scale_tril, p_cov_diag))
    return 0.5 * (term1 + term21 + term22 + term3 - p.event_shape[0])


@register_kl(MultivariateNormal, LowRankMultivariateNormal)
def _kl_multivariatenormal_lowrankmultivariatenormal(p, q):
    if tuple(p.event_shape) != tuple(q.event_shape):
        raise ValueError("KL-divergence between two (Low Rank) Multivariate Normals with                          different event shapes cannot be computed")
    term1 = _batch_lowrank_logdet(q._unbroadcasted_cov_factor, q._unbroadcasted_cov_diag, q._capacitance_tril) - 2 * p._unbroadcasted_scale_tril.diagonal(dim1=-2, dim2=-1).log().sum(-1)
    term3 = _batch_lowrank_mahalanobis(q._unbroadcasted_cov_factor, q._unbroadcasted_cov_diag, q.loc - p.loc, q._capacitance_tril)
    qWt_qDinv = q._unbroadcasted_cov_factor.mT / q._unbroadcasted_cov_diag.unsqueeze(-2)
    A = _solve_triangular_lower(q._capacitance_tril, qWt_qDinv)
    term21 = _batch_trace_XXT(p._unbroadcasted_scale_tril * q._unbroadcasted_cov_diag.rsqrt().unsqueeze(-1))
    term22 = _batch_trace_XXT(A.matmul(p._unbroadcasted_scale_tril))
    return 0.5 * (term1 + (term21 - term22) + term3 - p.event_shape[0])


@register_kl(Normal, Beta)
@register_kl(Normal, ContinuousBernoulli)
@register_kl(Normal, Exponential)
@register_kl(Normal, Gamma)
@register_kl(Normal, Pareto)
@register_kl(Normal, Uniform)
def _kl_normal_infinity(p, q):
    return _infinite_like(p.loc)


@register_kl(Normal, Gumbel)
def _kl_normal_gumbel(p, q):
    mean_scale_ratio = p.loc / q.scale
    var_scale_sqr_ratio = (p.scale / q.scale).pow(2)
    loc_scale_ratio = q.loc / q.scale
    t1 = var_scale_sqr_ratio.log() * 0.5
    t2 = mean_scale_ratio - loc_scale_ratio
    t3 = torch.exp(-mean_scale_ratio + 0.5 * var_scale_sqr_ratio + loc_scale_ratio)
    return -t1 + t2 + t3 - (0.5 * (1 + math.log(2 * math.pi)))


@register_kl(Pareto, Beta)
@register_kl(Pareto, ContinuousBernoulli)
@register_kl(Pareto, Uniform)
def _kl_pareto_infinity(p, q):
    return _infinite_like(p.scale)


@register_kl(Pareto, Exponential)
def _kl_pareto_exponential(p, q):
    scale_rate_prod = p.scale * q.rate
    t1 = (p.alpha / scale_rate_prod).log()
    t2 = p.alpha.reciprocal()
    t3 = p.alpha * scale_rate_prod / (p.alpha - 1)
    result = t1 - t2 + t3 - 1
    return _set_inf(result, p.alpha <= 1)


@register_kl(Pareto, Gamma)
def _kl_pareto_gamma(p, q):
    common_term = p.scale.log() + p.alpha.reciprocal()
    t1 = p.alpha.log() - common_term
    t2 = _lgamma(q.concentration) - q.concentration * q.rate.log()
    t3 = (1 - q.concentration) * common_term
    t4 = q.rate * p.alpha * p.scale / (p.alpha - 1)
    result = t1 + t2 + t3 + t4 - 1
    return _set_inf(result, p.alpha <= 1)


@register_kl(Pareto, Normal)
def _kl_pareto_normal(p, q):
    var_normal = 2 * q.scale.pow(2)
    common_term = p.scale / (p.alpha - 1)
    t1 = (math.sqrt(2 * math.pi) * q.scale * p.alpha / p.scale).log()
    t2 = p.alpha.reciprocal()
    t3 = p.alpha * common_term.pow(2) / (p.alpha - 2)
    t4 = (p.alpha * common_term - q.loc).pow(2)
    result = t1 - t2 + (t3 + t4) / var_normal - 1
    return _set_inf(result, p.alpha <= 2)


@register_kl(Poisson, Bernoulli)
@register_kl(Poisson, Binomial)
def _kl_poisson_infinity(p, q):
    return _infinite_like(p.rate)


@register_kl(Uniform, Beta)
def _kl_uniform_beta(p, q):
    common_term = p.high - p.low
    t1 = torch.log(common_term)
    t2 = (q.concentration1 - 1) * (_x_log_x(p.high) - _x_log_x(p.low) - common_term) / common_term
    t3 = (q.concentration0 - 1) * (_x_log_x(1 - p.high) - _x_log_x(1 - p.low) + common_term) / common_term
    t4 = _lgamma(q.concentration1) + _lgamma(q.concentration0) - _lgamma(q.concentration1 + q.concentration0)
    result = t3 + t4 - t1 - t2
    return _set_inf(result, (p.high > q.support.upper_bound) | (p.low < q.support.lower_bound))


@register_kl(Uniform, ContinuousBernoulli)
def _kl_uniform_continuous_bernoulli(p, q):
    result = -p.entropy() - p.mean * q.logits - torch.log1p(-q.probs) - q._cont_bern_log_norm()
    outside = torch.ge(p.high, q.support.upper_bound) | torch.le(p.low, q.support.lower_bound)
    return torch.where(outside, torch.ones_like(result) * inf, result)


@register_kl(Uniform, Exponential)
def _kl_uniform_exponential(p, q):
    result = q.rate * (p.high + p.low) / 2 - ((p.high - p.low) * q.rate).log()
    return _set_inf(result, p.low < q.support.lower_bound)


@register_kl(Uniform, Gamma)
def _kl_uniform_gamma(p, q):
    common_term = p.high - p.low
    t1 = common_term.log()
    t2 = _lgamma(q.concentration) - q.concentration * q.rate.log()
    t3 = (1 - q.concentration) * (_x_log_x(p.high) - _x_log_x(p.low) - common_term) / common_term
    t4 = q.rate * (p.high + p.low) / 2
    result = -t1 + t2 + t3 + t4
    return _set_inf(result, p.low < q.support.lower_bound)


@register_kl(Uniform, Gumbel)
def _kl_uniform_gumbel(p, q):
    common_term = q.scale / (p.high - p.low)
    high_loc_diff = (p.high - q.loc) / q.scale
    low_loc_diff = (p.low - q.loc) / q.scale
    t1 = common_term.log() + 0.5 * (high_loc_diff + low_loc_diff)
    t2 = common_term * (torch.exp(-high_loc_diff) - torch.exp(-low_loc_diff))
    return t1 - t2


@register_kl(Uniform, Pareto)
def _kl_uniform_pareto(p, q):
    support_uniform = p.high - p.low
    t1 = (q.alpha * q.scale.pow(q.alpha) * support_uniform).log()
    t2 = (_x_log_x(p.high) - _x_log_x(p.low) - support_uniform) / support_uniform
    result = t2 * (q.alpha + 1) - t1
    return _set_inf(result, p.low < q.support.lower_bound)


constraint_registry = _Namespace("torch.distributions.constraint_registry")
constraint_registry.biject_to = biject_to
constraint_registry.transform_to = transform_to
constraint_registry.ConstraintRegistry = ConstraintRegistry

__all__ = [
    "AbsTransform", "AffineTransform", "Bernoulli", "Beta", "Binomial", "CatTransform", "Categorical",
    "Cauchy", "Chi2", "ComposeTransform", "ContinuousBernoulli", "CorrCholeskyTransform",
    "CumulativeDistributionTransform", "Dirichlet", "Distribution", "ExpTransform", "Exponential",
    "ExponentialFamily", "FisherSnedecor", "Gamma", "GeneralizedPareto", "Geometric", "Gumbel",
    "HalfCauchy", "HalfNormal", "Independent", "IndependentTransform", "InverseGamma", "Kumaraswamy",
    "LKJCholesky", "Laplace", "LogNormal", "LogisticNormal", "LowRankMultivariateNormal",
    "LowerCholeskyTransform", "MixtureSameFamily", "Multinomial", "MultivariateNormal",
    "NegativeBinomial", "Normal", "OneHotCategorical", "OneHotCategoricalStraightThrough", "Pareto",
    "Poisson", "PositiveDefiniteTransform", "PowerTransform", "RelaxedBernoulli",
    "RelaxedOneHotCategorical", "ReshapeTransform", "SigmoidTransform", "SoftmaxTransform",
    "SoftplusTransform", "StackTransform", "StickBreakingTransform", "StudentT", "TanhTransform",
    "Transform", "TransformedDistribution", "Uniform", "VonMises", "Weibull", "Wishart", "biject_to",
    "identity_transform", "kl_divergence", "register_kl", "transform_to",
]

# The constraints, transforms and kl submodules re-export what this module
# defines; importing them here makes `torch.distributions.constraints` (and
# the others) those modules, as in PyTorch, whichever way a program reaches
# them.
import torch.distributions.constraints as constraints
import torch.distributions.transforms as transforms
import torch.distributions.kl as kl
