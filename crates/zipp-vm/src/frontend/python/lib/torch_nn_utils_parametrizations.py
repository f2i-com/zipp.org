"""torch.nn.utils.parametrizations for Zipp: spectral_norm, weight_norm and
orthogonal as parametrizations (see torch.nn.utils.parametrize), with
PyTorch's state_dict layout (`parametrizations.weight.original`,
`...0._u`/`_v`, `original0`/`original1`, `...0.base`)."""
import math
from enum import Enum, auto
import torch
import torch.nn.functional as F
from torch import Tensor
from torch.nn import Module
from torch.nn.utils import parametrize

__all__ = ["orthogonal", "spectral_norm", "weight_norm"]


# ---- small dense linear algebra, composed so autograd flows ----------------------------------
def _eye_like(x, n, k=None):
    k = n if k is None else k
    eye = torch.tensor([[1.0 if i == j else 0.0 for j in range(k)] for i in range(n)], dtype=x.dtype)
    batch = tuple(x.shape[:-2])
    return eye.expand(*(batch + (n, k))) if batch else eye


def _matrix_exp(a):
    """exp(a) by scaling and squaring a Taylor series (in float64, rounded
    back once), every step a differentiable tensor operation."""
    work = a.double()
    with torch.no_grad():
        norm = float(torch.abs(work).sum(-2).max().item()) if work.numel() else 0.0
    squarings = max(0, int(math.ceil(math.log2(norm))) + 1) if norm > 0.5 else 0
    scaled = work / (2.0 ** squarings)
    eye = _eye_like(work, work.shape[-1])
    term = eye
    total = eye
    for k in range(1, 19):
        term = (term @ scaled) / k
        total = total + term
    for _ in range(squarings):
        total = total @ total
    return total.to(a.dtype)


def _lists_inverse(m):
    n = len(m)
    a = [list(row) + [1.0 if i == j else 0.0 for j in range(n)] for i, row in enumerate(m)]
    for col in range(n):
        best = max(range(col, n), key=lambda r: abs(a[r][col]))
        if a[best][col] == 0.0:
            raise RuntimeError("linalg.solve: The solver failed because the input matrix is singular.")
        a[col], a[best] = a[best], a[col]
        piv = a[col][col]
        a[col] = [v / piv for v in a[col]]
        for r in range(n):
            if r != col and a[r][col] != 0.0:
                f = a[r][col]
                a[r] = [x - f * y for x, y in zip(a[r], a[col])]
    return [row[n:] for row in a]


def _inverse(m):
    """The inverse of a (batch of) square matrices, differentiable."""
    flat = m.detach().double().reshape(-1, m.shape[-2], m.shape[-1])
    inv = torch.tensor([_lists_inverse(x) for x in flat.tolist()], dtype=torch.float64).reshape(*m.shape).to(m.dtype)
    if torch._needs_grad(m):
        def backward(g):
            return (-(inv.transpose(-2, -1) @ g @ inv.transpose(-2, -1)),)
        inv.requires_grad = True
        inv._node = torch._Node(backward, (m,), "LinalgInvExBackward0")
    return inv


def _householder_product(a, tau):
    """torch.linalg.householder_product: the first k columns of
    H_1 H_2 ... H_k, H_i = I - tau_i v_i v_i^T with v_i the i-th column of
    `a` below the diagonal and 1 on it."""
    n, k = a.shape[-2], a.shape[-1]
    q = _eye_like(a, n, k)
    rows = torch.arange(n).reshape(n, 1)
    for i in reversed(range(k)):
        below = (rows > i).to(a.dtype)
        unit = (rows == i).to(a.dtype)
        v = a.narrow(-1, i, 1) * below + unit
        t = tau.narrow(-1, i, 1).unsqueeze(-1)
        q = q - (t * v) @ (v.transpose(-2, -1) @ q)
    return q


def _geqrf_lists(m):
    """LAPACK's geqrf on one matrix (lists of floats): R on and above the
    diagonal, the Householder vectors below it, and the tau factors."""
    a = [list(row) for row in m]
    rows, cols = len(a), len(a[0]) if a else 0
    taus = []
    for j in range(min(rows, cols)):
        alpha = a[j][j]
        xnorm = math.sqrt(sum(a[i][j] * a[i][j] for i in range(j + 1, rows)))
        if xnorm == 0.0:
            taus.append(0.0)
            continue
        beta = -math.copysign(math.sqrt(alpha * alpha + xnorm * xnorm), alpha)
        tau = (beta - alpha) / beta
        scale = 1.0 / (alpha - beta)
        for i in range(j + 1, rows):
            a[i][j] *= scale
        a[j][j] = beta
        for c in range(j + 1, cols):
            s = a[j][c] + sum(a[i][j] * a[i][c] for i in range(j + 1, rows))
            s *= tau
            a[j][c] -= s
            for i in range(j + 1, rows):
                a[i][c] -= s * a[i][j]
        taus.append(tau)
    return a, taus


def _geqrf(x):
    flat = x.detach().double().reshape(-1, x.shape[-2], x.shape[-1]).tolist()
    outs, taus = [], []
    for m in flat:
        a, t = _geqrf_lists(m)
        outs.append(a)
        taus.append(t)
    a = torch.tensor(outs, dtype=torch.float64).reshape(*x.shape).to(x.dtype)
    tau = torch.tensor(taus, dtype=torch.float64).reshape(*(tuple(x.shape[:-2]) + (min(x.shape[-2], x.shape[-1]),))).to(x.dtype)
    return a, tau


def _diagonal(x):
    return x.diagonal(0, -2, -1)


def _is_orthogonal(q):
    n, k = q.shape[-2], q.shape[-1]
    eye = _eye_like(q, k)
    eps = 10.0 * n * torch.finfo(q.dtype).eps
    return torch.allclose(q.transpose(-2, -1) @ q, eye, atol=eps)


def _make_orthogonal(a):
    x, tau = _geqrf(a)
    q = _householder_product(x, tau)
    return q * torch.sign(_diagonal(x)).unsqueeze(-2)


class _OrthMaps(Enum):
    matrix_exp = auto()
    cayley = auto()
    householder = auto()


class _Orthogonal(Module):
    def __init__(self, weight, orthogonal_map, *, use_trivialization=True):
        super().__init__()
        self.shape = weight.shape
        self.orthogonal_map = orthogonal_map
        if use_trivialization:
            self.register_buffer("base", None)

    def forward(self, X):
        n, k = X.shape[-2], X.shape[-1]
        transposed = n < k
        if transposed:
            X = X.transpose(-2, -1)
            n, k = k, n
        if self.orthogonal_map == _OrthMaps.matrix_exp or self.orthogonal_map == _OrthMaps.cayley:
            X = X.tril()
            if n != k:
                X = torch.cat([X, torch.zeros(*(tuple(X.shape[:-2]) + (n, n - k)), dtype=X.dtype)], -1)
            A = X - X.transpose(-2, -1)
            if self.orthogonal_map == _OrthMaps.matrix_exp:
                Q = _matrix_exp(A)
            else:
                eye = _eye_like(A, n)
                Q = _inverse(eye - 0.5 * A) @ (eye + 0.5 * A)
            if n != k:
                Q = Q.narrow(-1, 0, k)
        else:
            A = X.tril(-1)
            tau = 2.0 / (1.0 + (A * A).sum(-2))
            Q = _householder_product(A, tau)
            Q = Q * _diagonal(X).int().unsqueeze(-2)
        if "base" in self._buffers:
            Q = self.base @ Q
        if transposed:
            Q = Q.transpose(-2, -1)
        return Q

    def right_inverse(self, Q):
        with torch.no_grad():
            if tuple(Q.shape) != tuple(self.shape):
                raise ValueError("Expected a matrix or batch of matrices of shape %s. Got a tensor of shape %s." % (self.shape, Q.shape))
            Q_init = Q
            n, k = Q.shape[-2], Q.shape[-1]
            transpose = n < k
            if transpose:
                Q = Q.transpose(-2, -1)
                n, k = k, n
            if "base" not in self._buffers:
                if self.orthogonal_map == _OrthMaps.cayley or self.orthogonal_map == _OrthMaps.matrix_exp:
                    raise NotImplementedError("It is not possible to assign to the matrix exponential or the Cayley parametrizations when use_trivialization=False.")
                A, tau = _geqrf(Q)
                rows = A.reshape(-1, n, k).tolist()
                taus = tau.reshape(len(rows), -1).tolist()
                for m, t in zip(rows, taus):
                    for i in range(len(t)):
                        d = m[i][i]
                        m[i][i] = (1.0 if d > 0 else -1.0 if d < 0 else 0.0) * (-1.0 if t[i] == 0.0 else 1.0)
                A = torch.tensor(rows, dtype=Q.dtype).reshape(*Q.shape)
                return A.transpose(-2, -1) if transpose else A
            if n == k:
                Q = _make_orthogonal(Q) if not _is_orthogonal(Q) else Q.clone()
            else:
                N = torch.randn(*(tuple(Q.shape[:-2]) + (n, n - k)), dtype=Q.dtype)
                Q = _make_orthogonal(torch.cat([Q, N], -1))
            self.base = Q
            neg_id = -_eye_like(Q_init, Q_init.shape[-2], Q_init.shape[-1])
            return neg_id.clone()


def orthogonal(module, name="weight", orthogonal_map=None, *, use_trivialization=True):
    weight = getattr(module, name, None)
    if not isinstance(weight, Tensor):
        raise ValueError("Module '%s' has no parameter or buffer with name '%s'" % (module, name))
    if weight.ndim < 2:
        raise ValueError("Expected a matrix or batch of matrices. Got a tensor of %d dimensions." % weight.ndim)
    if orthogonal_map is None:
        orthogonal_map = "matrix_exp" if weight.shape[-2] == weight.shape[-1] else "householder"
    orth_enum = getattr(_OrthMaps, orthogonal_map, None) if isinstance(orthogonal_map, str) else None
    if orth_enum is None:
        raise ValueError('orthogonal_map has to be one of "matrix_exp", "cayley", "householder". Got: %s' % orthogonal_map)
    orth = _Orthogonal(weight, orth_enum, use_trivialization=use_trivialization)
    parametrize.register_parametrization(module, name, orth, unsafe=True)
    return module


def _norm_except_dim(v, dim):
    """torch.norm_except_dim(v, 2, dim): the 2-norm over every dimension
    but `dim` (keeping it), or of the whole tensor when dim == -1."""
    if dim == -1:
        return torch.sqrt((v * v).sum())
    dims = [d for d in range(v.dim()) if d != dim % v.dim()]
    return torch.sqrt((v * v).sum(dims, keepdim=True))


class _WeightNorm(Module):
    def __init__(self, dim=0):
        super().__init__()
        if dim is None:
            dim = -1
        self.dim = dim

    def forward(self, weight_g, weight_v):
        return weight_v * (weight_g / _norm_except_dim(weight_v, self.dim))

    def right_inverse(self, weight):
        return _norm_except_dim(weight, self.dim), weight


def weight_norm(module, name="weight", dim=0):
    _weight_norm = _WeightNorm(dim)
    parametrize.register_parametrization(module, name, _weight_norm, unsafe=True)

    def _weight_norm_compat_hook(state_dict, prefix, local_metadata, strict, missing_keys, unexpected_keys, error_msgs):
        g_key = prefix + name + "_g"
        v_key = prefix + name + "_v"
        if g_key in state_dict and v_key in state_dict:
            original0 = state_dict.pop(g_key)
            original1 = state_dict.pop(v_key)
            state_dict[prefix + "parametrizations." + name + ".original0"] = original0
            state_dict[prefix + "parametrizations." + name + ".original1"] = original1
    module._register_load_state_dict_pre_hook(_weight_norm_compat_hook)
    return module


class _SpectralNorm(Module):
    def __init__(self, weight, n_power_iterations=1, dim=0, eps=1e-12):
        super().__init__()
        ndim = weight.ndim
        if dim >= ndim or dim < -ndim:
            raise IndexError("Dimension out of range (expected to be in range of [-%d, %d] but got %d)" % (ndim, ndim - 1, dim))
        if n_power_iterations <= 0:
            raise ValueError("Expected n_power_iterations to be positive, but got n_power_iterations=%s" % n_power_iterations)
        self.dim = dim if dim >= 0 else dim + ndim
        self.eps = eps
        if ndim > 1:
            self.n_power_iterations = n_power_iterations
            with torch.no_grad():
                weight_mat = self._reshape_weight_to_matrix(weight)
                h, w = weight_mat.shape
                u = torch.empty(h, dtype=weight.dtype).normal_(0, 1)
                v = torch.empty(w, dtype=weight.dtype).normal_(0, 1)
                self.register_buffer("_u", F.normalize(u, dim=0, eps=self.eps))
                self.register_buffer("_v", F.normalize(v, dim=0, eps=self.eps))
            self._power_method(weight_mat, 15)

    def _reshape_weight_to_matrix(self, weight):
        if weight.ndim <= 1:
            raise AssertionError("Expected weight to have more than 1 dimension, got %d" % weight.ndim)
        if self.dim != 0:
            weight = weight.permute(self.dim, *[d for d in range(weight.dim()) if d != self.dim])
        return weight.flatten(1)

    def _power_method(self, weight_mat, n_power_iterations):
        with torch.no_grad():
            weight_mat = weight_mat.detach()
            for _ in range(n_power_iterations):
                self._u = F.normalize(torch.mv(weight_mat, self._v), dim=0, eps=self.eps, out=self._u)
                self._v = F.normalize(torch.mv(weight_mat.t(), self._u), dim=0, eps=self.eps, out=self._v)

    def forward(self, weight):
        if weight.ndim == 1:
            return F.normalize(weight, dim=0, eps=self.eps)
        weight_mat = self._reshape_weight_to_matrix(weight)
        if self.training:
            self._power_method(weight_mat, self.n_power_iterations)
        u = self._u.clone()
        v = self._v.clone()
        sigma = torch.dot(u, torch.mv(weight_mat, v))
        return weight / sigma

    def right_inverse(self, value):
        return value


def spectral_norm(module, name="weight", n_power_iterations=1, eps=1e-12, dim=None):
    import torch.nn as nn
    weight = getattr(module, name, None)
    if not isinstance(weight, Tensor):
        raise ValueError("Module '%s' has no parameter or buffer with name '%s'" % (module, name))
    if dim is None:
        dim = 1 if isinstance(module, (nn.ConvTranspose1d, nn.ConvTranspose2d, nn.ConvTranspose3d)) else 0
    parametrize.register_parametrization(module, name, _SpectralNorm(weight, n_power_iterations, dim, eps))
    return module
