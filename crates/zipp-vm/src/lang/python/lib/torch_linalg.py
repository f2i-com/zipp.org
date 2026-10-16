"""torch.linalg for Zipp: decompositions, solvers, inverses and norms over
batches of matrices [..., m, n] of float32/float64.

The factorizations run in double precision and round once to the input's
dtype: LU with partial pivoting (det, slogdet, inv, solve), LAPACK-style
Householder QR (R's diagonal has geqrf's signs), Cholesky, triangular
solves, the symmetric eigenproblem, a one-sided Jacobi SVD and Hessenberg +
Francis double-shift QR for the general eigenproblem. They run as native
loops (`_zipp_tensor.linalg`, the engine's `vm::py_tensor::linalg`) when the
engine takes them, and otherwise as the Python algorithms below, which the
native ones match to the documented tolerances (the symmetric eigenproblem
runs tridiagonal QL natively and cyclic Jacobi here). Gradients are
PyTorch's formulas written with tensor operations, so they batch, broadcast
and (through `create_graph=True`) differentiate again.

`eig`/`eigvals` return complex tensors (complex64 for float32 input,
complex128 for float64), as PyTorch does, with unit eigenvectors whose
largest component is real and positive (real ones: largest entry positive).
"""
import math as _math
import torch

_float = float
_int = int
_abs = abs
_min = min
_max = max
_sum = sum
_range = range


class LinAlgError(RuntimeError):
    """What PyTorch raises for a singular or non positive-definite input."""
    pass


# ---- plumbing ----------------------------------------------------------------------------
def _returns(name, fields, items):
    return torch._ReturnTypes(tuple(items), name, fields)


def _check_float(A, fname, arg="A", allow_complex=False):
    if not isinstance(A, torch.Tensor):
        raise TypeError("linalg_%s(): argument '%s' must be Tensor, not %s" % (fname, arg, type(A).__name__))
    if A.dtype.is_complex:
        if allow_complex and A.dtype.name in ("complex64", "complex128"):
            return
        raise NotImplementedError("linalg.%s: complex input is not supported on Zipp" % fname)
    if not A.dtype.is_floating_point:
        raise RuntimeError("linalg.%s: Expected a floating point or complex tensor as input. Got %s" % (fname, torch._CAST_NAME.get(A.dtype.name, A.dtype.name)))
    if A.dtype.name not in ("float32", "float64"):
        raise RuntimeError("linalg.%s: Low precision dtypes not supported. Got %s" % (fname, {"float16": "Half", "bfloat16": "BFloat16"}.get(A.dtype.name, A.dtype.name)))


def _check_matrix(A, fname, arg="A", allow_complex=False):
    _check_float(A, fname, arg, allow_complex)
    if A.ndim < 2:
        raise RuntimeError("linalg.%s: The input tensor %s must have at least 2 dimensions." % (fname, arg))


def _check_square(A, fname, arg="A", allow_complex=False):
    _check_matrix(A, fname, arg, allow_complex)
    if A.shape[-1] != A.shape[-2]:
        raise RuntimeError("linalg.%s: %s must be batches of square matrices, but they are %d by %d matrices" % (fname, arg, A.shape[-2], A.shape[-1]))


def _batch(A):
    return tuple(A.shape[:-2])


def _count(shape):
    n = 1
    for d in shape:
        n *= d
    return n


def _mats(A):
    """The matrices of A [..., m, n] as lists of row lists of Python floats."""
    m, n = A.shape[-2], A.shape[-1]
    flat = torch._k.to_list(A._s)
    out = []
    size = m * n
    for b in _range(_count(_batch(A))):
        base = b * size
        out.append([[_float(v) for v in flat[base + i * n: base + i * n + n]] for i in _range(m)])
    return out


def _tensor(mats, batch, m, n, dt):
    flat = []
    for M in mats:
        for row in M:
            flat.extend(row)
    return torch.Tensor(torch._k.from_flat(dt.name, flat), tuple(batch) + (m, n), dt)


def _flat_tensor(flat, shape, dt):
    if dt.is_floating_point:
        flat = [_float(v) for v in flat]
    else:
        flat = [_int(v) for v in flat]
    return torch.Tensor(torch._k.from_flat(dt.name, flat), tuple(shape), dt)


# The native factorizations (`_zipp_tensor.linalg`): op codes and a caller.
_NAT_LU, _NAT_SOLVE, _NAT_TRI, _NAT_CHOL, _NAT_QR, _NAT_EIGH, _NAT_SVD, _NAT_EIG = 1, 2, 3, 4, 5, 6, 7, 8


def _native(op, ins, dims):
    """The float64 output storages of a native factorization of the
    (contiguous float32/float64) tensors `ins`, or None when the engine
    declines; the caller then runs its Python algorithm."""
    return torch._k.linalg(op, [t._s for t in ins], list(dims))


def _t64(storage, shape):
    return torch.Tensor(storage, tuple(shape), torch.float64)


def _as(storage, shape, dt):
    """A native float64 result as a tensor of `dt` (rounded once)."""
    if dt is torch.float64:
        return torch.Tensor(storage, tuple(shape), dt)
    return torch.Tensor(torch._k.astype(storage, dt.name), tuple(shape), dt)


def _floats(storage):
    return torch._k.to_list(storage)


def _needs(*ts):
    if not torch.is_grad_enabled():
        return False
    for t in ts:
        if isinstance(t, torch.Tensor) and t.requires_grad:
            return True
    return False


def _attach(out, parents, backward, name, saved=()):
    """Gives `out` a history: `backward(g)` returns one gradient per parent.
    Backward functions here are tensor operations, so they are themselves
    differentiable (create_graph=True). `saved`: the tensors backward reads;
    writing one in place before backward raises PyTorch's version error."""
    node = torch._Node(backward, tuple(parents), name, [t for t in saved if t is not None] or None)
    node.diff = True
    out.requires_grad = True
    out._node = node
    return out


def _mT(x):
    return x.transpose(-2, -1)


def _eye_like(n, batch, dt):
    I = torch.eye(n, dtype=dt)
    return I.expand(*(tuple(batch) + (n, n))) if batch else I


def _fdiv(a, b):
    """IEEE division: a/0 is +-inf (nan for 0/0), as LAPACK produces."""
    if b != 0.0:
        return a / b
    if a != a or a == 0.0:
        return _math.nan
    return _math.copysign(_math.inf, a) * _math.copysign(1.0, b)


def _identity(n):
    return [[1.0 if i == j else 0.0 for j in _range(n)] for i in _range(n)]


def _transpose(M):
    if not M:
        return []
    return [list(col) for col in zip(*M)]


def _matmul(A, B):
    Bt = _transpose(B)
    return [[_math.fsum([a * b for a, b in zip(row, col)]) for col in Bt] for row in A]


def _batch_msg(fname, b, count, msg):
    if count > 1:
        return "%s: (Batch element %d): %s" % (fname, b, msg)
    return "%s: %s" % (fname, msg)


def _broadcast_batch(A, B):
    """A [..., m, n] and B [..., p, q] with their batch dimensions broadcast."""
    batch = tuple(torch.broadcast_shapes(_batch(A), _batch(B)))
    if _batch(A) != batch:
        A = A.expand(*(batch + tuple(A.shape[-2:])))
    if _batch(B) != batch:
        B = B.expand(*(batch + tuple(B.shape[-2:])))
    return A, B, batch


# ---- kernels on Python floats ------------------------------------------------------------
def _lu(M):
    """Partial-pivoting LU of a list-of-rows matrix (LAPACK getrf's pivot
    choice: the first largest magnitude). Returns (LU, perm, pivots, sign,
    info): perm[i] is the source row of row i, pivots LAPACK's 1-based swaps,
    info the 1-based index of the first zero pivot (0 if none)."""
    m = len(M)
    n = len(M[0]) if m else 0
    A = [row[:] for row in M]
    perm = list(_range(m))
    pivots = []
    sign = 1.0
    info = 0
    for k in _range(_min(m, n)):
        p = k
        best = _abs(A[k][k])
        for i in _range(k + 1, m):
            v = _abs(A[i][k])
            if v > best:
                best = v
                p = i
        pivots.append(p + 1)
        if p != k:
            A[k], A[p] = A[p], A[k]
            perm[k], perm[p] = perm[p], perm[k]
            sign = -sign
        piv = A[k][k]
        if piv == 0.0:
            if info == 0:
                info = k + 1
            continue
        rk = A[k]
        for i in _range(k + 1, m):
            ri = A[i]
            f = ri[k] / piv
            ri[k] = f
            if f != 0.0:
                for j in _range(k + 1, n):
                    ri[j] -= f * rk[j]
    return A, perm, pivots, sign, info


def _lu_solve(LU, perm, B):
    n = len(LU)
    k = len(B[0]) if n else 0
    X = [B[perm[i]][:] for i in _range(n)]
    for i in _range(n):
        Li = LU[i]
        xi = X[i]
        for j in _range(i):
            f = Li[j]
            if f != 0.0:
                xj = X[j]
                for c in _range(k):
                    xi[c] -= f * xj[c]
    for i in _range(n - 1, -1, -1):
        Ui = LU[i]
        xi = X[i]
        for j in _range(i + 1, n):
            f = Ui[j]
            if f != 0.0:
                xj = X[j]
                for c in _range(k):
                    xi[c] -= f * xj[c]
        d = Ui[i]
        for c in _range(k):
            xi[c] = _fdiv(xi[c], d)
    return X


def _tri_solve(T, B, upper, unit):
    """Solves T X = B for triangular T (only that triangle is read)."""
    n = len(T)
    k = len(B[0]) if n else 0
    X = [row[:] for row in B]
    order = _range(n - 1, -1, -1) if upper else _range(n)
    for i in order:
        Ti = T[i]
        xi = X[i]
        js = _range(i + 1, n) if upper else _range(i)
        for j in js:
            f = Ti[j]
            if f != 0.0:
                xj = X[j]
                for c in _range(k):
                    xi[c] -= f * xj[c]
        if not unit:
            d = Ti[i]
            for c in _range(k):
                xi[c] = _fdiv(xi[c], d)
    return X


def _cholesky(M):
    """Lower Cholesky factor (reads the lower triangle) and LAPACK's info."""
    n = len(M)
    L = [[0.0] * n for _ in _range(n)]
    for j in _range(n):
        Lj = L[j]
        s = M[j][j] - _math.fsum([Lj[k] * Lj[k] for k in _range(j)])
        if not s > 0.0:
            # potrf stops here: the failing diagonal holds the non-positive
            # pivot and the columns after it keep the input's lower triangle.
            Lj[j] = s
            for i in _range(j + 1, n):
                for c in _range(j, i + 1):
                    L[i][c] = M[i][c]
            return L, j + 1
        d = _math.sqrt(s)
        Lj[j] = d
        for i in _range(j + 1, n):
            Li = L[i]
            Li[j] = (M[i][j] - _math.fsum([Li[k] * Lj[k] for k in _range(j)])) / d
    return L, 0


def _householder(M):
    """LAPACK geqrf on a list-of-rows matrix: returns the working matrix (R on
    and above the diagonal, Householder vectors below) and the taus."""
    m = len(M)
    n = len(M[0]) if m else 0
    A = [row[:] for row in M]
    taus = []
    for j in _range(_min(m, n)):
        alpha = A[j][j]
        xnorm = _math.sqrt(_math.fsum([A[i][j] * A[i][j] for i in _range(j + 1, m)]))
        if xnorm == 0.0:
            taus.append(0.0)
            continue
        beta = -_math.copysign(_math.hypot(alpha, xnorm), alpha)
        tau = (beta - alpha) / beta
        scale = 1.0 / (alpha - beta)
        for i in _range(j + 1, m):
            A[i][j] *= scale
        A[j][j] = beta
        for c in _range(j + 1, n):
            w = A[j][c] + _math.fsum([A[i][j] * A[i][c] for i in _range(j + 1, m)])
            w *= tau
            A[j][c] -= w
            for i in _range(j + 1, m):
                A[i][c] -= w * A[i][j]
        taus.append(tau)
    return A, taus


def _form_q(A, taus, m, cols):
    """The first `cols` columns of H_0 H_1 ... H_{k-1} (LAPACK orgqr)."""
    Q = [[1.0 if i == c else 0.0 for c in _range(cols)] for i in _range(m)]
    for j in _range(len(taus) - 1, -1, -1):
        tau = taus[j]
        if tau == 0.0:
            continue
        for c in _range(cols):
            w = Q[j][c] + _math.fsum([A[i][j] * Q[i][c] for i in _range(j + 1, m)])
            if w == 0.0:
                continue
            w *= tau
            Q[j][c] -= w
            for i in _range(j + 1, m):
                Q[i][c] -= w * A[i][j]
    return Q


def _jacobi_eigh(S):
    """Eigenvalues (ascending) and eigenvectors (columns) of a symmetric
    matrix by cyclic Jacobi rotations."""
    n = len(S)
    A = [row[:] for row in S]
    V = _identity(n)
    for sweep in _range(100):
        rotated = False
        for p in _range(n - 1):
            for q in _range(p + 1, n):
                apq = A[p][q]
                if apq == 0.0:
                    continue
                app = A[p][p]
                aqq = A[q][q]
                if _abs(apq) <= 1e-18 * _math.sqrt(_abs(app * aqq)):
                    A[p][q] = A[q][p] = 0.0
                    continue
                rotated = True
                theta = (aqq - app) / (2.0 * apq)
                if _abs(theta) > 1e150:
                    t = 0.5 / theta
                else:
                    t = _math.copysign(1.0, theta) / (_abs(theta) + _math.sqrt(theta * theta + 1.0))
                c = 1.0 / _math.sqrt(t * t + 1.0)
                s = t * c
                Ap = A[p]
                Aq = A[q]
                for k in _range(n):
                    if k == p or k == q:
                        continue
                    Ak = A[k]
                    akp = Ak[p]
                    akq = Ak[q]
                    x = c * akp - s * akq
                    y = s * akp + c * akq
                    Ak[p] = x
                    Ak[q] = y
                    Ap[k] = x
                    Aq[k] = y
                Ap[p] = app - t * apq
                Aq[q] = aqq + t * apq
                Ap[q] = Aq[p] = 0.0
                for k in _range(n):
                    Vk = V[k]
                    vkp = Vk[p]
                    vkq = Vk[q]
                    Vk[p] = c * vkp - s * vkq
                    Vk[q] = s * vkp + c * vkq
        if not rotated:
            break
    w = [A[i][i] for i in _range(n)]
    order = sorted(_range(n), key=lambda i: w[i])
    vals = [w[i] for i in order]
    cols = [[V[k][i] for k in _range(n)] for i in order]
    for col in cols:
        _fix_sign(col)
    return vals, _transpose(cols) if cols else []


def _fix_sign(v, *others):
    """A deterministic sign: the largest-magnitude entry of v positive."""
    best = 0.0
    sign = 1.0
    for x in v:
        if _abs(x) > best + 1e-12 * best:
            best = _abs(x)
            sign = -1.0 if x < 0 else 1.0
    if sign < 0:
        for i in _range(len(v)):
            v[i] = -v[i]
        for o in others:
            for i in _range(len(o)):
                o[i] = -o[i]


def _dot(a, b):
    s = 0.0
    for i in _range(len(a)):
        s += a[i] * b[i]
    return s


def _complete_basis(cols, m):
    """Extends orthonormal columns (lists of length m) to m of them, each
    time with the unit vector whose residual against the basis so far is
    largest (at least 1/sqrt(m)), orthogonalized twice."""
    cols = [c[:] for c in cols]
    while len(cols) < m:
        best = None
        best_norm = -1.0
        for e in _range(m):
            v = [1.0 if i == e else 0.0 for i in _range(m)]
            for _ in _range(2):
                for c in cols:
                    d = _dot(c, v)
                    for i in _range(m):
                        v[i] -= d * c[i]
            nrm = _math.sqrt(_dot(v, v))
            if nrm > best_norm:
                best_norm = nrm
                best = v
        cols.append([x / best_norm for x in best])
    return cols


def _svd_tall(M, m, n):
    """One-sided Jacobi on the columns of an m x n matrix, m >= n. Returns
    (S descending, U columns (n of them, length m), V columns). Column norms
    are recomputed each sweep and updated exactly by each rotation."""
    cols = [[M[i][j] for i in _range(m)] for j in _range(n)]
    V = [[1.0 if i == j else 0.0 for i in _range(n)] for j in _range(n)]
    for sweep in _range(80):
        rotated = False
        nrm = [_dot(c, c) for c in cols]
        for p in _range(n - 1):
            cp = cols[p]
            vp = V[p]
            for q in _range(p + 1, n):
                cq = cols[q]
                c = 0.0
                for i in _range(m):
                    c += cp[i] * cq[i]
                if c == 0.0:
                    continue
                a = nrm[p]
                b = nrm[q]
                if _abs(c) <= 1e-16 * _math.sqrt(_abs(a * b)):
                    continue
                rotated = True
                zeta = (b - a) / (2.0 * c)
                if _abs(zeta) > 1e150:
                    t = 0.5 / zeta
                else:
                    t = _math.copysign(1.0, zeta) / (_abs(zeta) + _math.sqrt(1.0 + zeta * zeta))
                cs = 1.0 / _math.sqrt(1.0 + t * t)
                sn = cs * t
                nrm[p] = a - t * c
                nrm[q] = b + t * c
                for i in _range(m):
                    x = cp[i]
                    y = cq[i]
                    cp[i] = cs * x - sn * y
                    cq[i] = sn * x + cs * y
                vq = V[q]
                for i in _range(n):
                    x = vp[i]
                    y = vq[i]
                    vp[i] = cs * x - sn * y
                    vq[i] = sn * x + cs * y
                # An updated norm that shrank a lot has lost its relative
                # accuracy (it can even go negative): measure it again.
                if nrm[p] < 0.01 * a:
                    nrm[p] = _dot(cp, cp)
                if nrm[q] < 0.01 * b:
                    nrm[q] = _dot(cq, cq)
        if not rotated:
            break
    sig = [_math.sqrt(_dot(c, c)) for c in cols]
    order = sorted(_range(n), key=lambda j: -sig[j])
    S = [sig[j] for j in order]
    Vc = [V[j][:] for j in order]
    smax = S[0] if S else 0.0
    tiny = smax * _max(m, n) * 2.220446049250313e-16
    U = []
    for idx, j in enumerate(order):
        if S[idx] > tiny and S[idx] > 1e-300:
            U.append([x / S[idx] for x in cols[j]])
        else:
            U.append(None)
    # Columns for (numerically) zero singular values: complete the basis.
    good = [u for u in U if u is not None]
    if len(good) < n:
        full = _complete_basis(good, m)
        extra = full[len(good):]
        k = 0
        for idx in _range(n):
            if U[idx] is None:
                U[idx] = extra[k]
                k += 1
    for idx in _range(n):
        _fix_sign(Vc[idx], U[idx])
    return S, U, Vc


def _svd(M, m, n, full):
    """SVD of an m x n list-of-rows matrix: (U m x (m|k), S, Vh (n|k) x n)."""
    k = _min(m, n)
    if m >= n:
        S, Ucols, Vcols = _svd_tall(M, m, n)
    else:
        S, Vcols, Ucols = _svd_tall(_transpose(M), n, m)
    if full:
        Ucols = _complete_basis(Ucols, m)
        Vcols = _complete_basis(Vcols, n)
    U = _transpose(Ucols) if Ucols else [[] for _ in _range(m)]
    Vh = [c[:] for c in Vcols]
    return U, S, Vh


def _hessenberg(M):
    n = len(M)
    A = [row[:] for row in M]
    for k in _range(n - 2):
        x = [A[i][k] for i in _range(k + 1, n)]
        xnorm = _math.sqrt(_math.fsum([v * v for v in x[1:]]))
        if xnorm == 0.0:
            continue
        alpha = x[0]
        beta = -_math.copysign(_math.hypot(alpha, xnorm), alpha)
        v = [alpha - beta] + x[1:]
        vv = _math.fsum([t * t for t in v])
        f = 2.0 / vv
        r = len(v)
        for c in _range(n):
            w = f * _math.fsum([v[i] * A[k + 1 + i][c] for i in _range(r)])
            if w != 0.0:
                for i in _range(r):
                    A[k + 1 + i][c] -= w * v[i]
        for row in A:
            w = f * _math.fsum([row[k + 1 + i] * v[i] for i in _range(r)])
            if w != 0.0:
                for i in _range(r):
                    row[k + 1 + i] -= w * v[i]
        for i in _range(k + 2, n):
            A[i][k] = 0.0
    return A


def _sgn(a, b):
    return _abs(a) if b >= 0 else -_abs(a)


def _hqr(M):
    """Eigenvalues (real and imaginary parts) of a real matrix: Hessenberg
    reduction, then the Francis double-shift QR iteration (EISPACK hqr)."""
    n = len(M)
    H = _hessenberg(M)
    a = [[0.0] * (n + 1)] + [[0.0] + row[:] for row in H]
    wr = [0.0] * (n + 1)
    wi = [0.0] * (n + 1)
    anorm = 0.0
    for i in _range(1, n + 1):
        for j in _range(_max(i - 1, 1), n + 1):
            anorm += _abs(a[i][j])
    nn = n
    t = 0.0
    x = y = z = w = p = q = r = 0.0
    while nn >= 1:
        its = 0
        while True:
            l = nn
            while l >= 2:
                s = _abs(a[l - 1][l - 1]) + _abs(a[l][l])
                if s == 0.0:
                    s = anorm
                if _abs(a[l][l - 1]) + s == s:
                    a[l][l - 1] = 0.0
                    break
                l -= 1
            if l < 1:
                l = 1
            x = a[nn][nn]
            if l == nn:
                wr[nn] = x + t
                wi[nn] = 0.0
                nn -= 1
            else:
                y = a[nn - 1][nn - 1]
                w = a[nn][nn - 1] * a[nn - 1][nn]
                if l == nn - 1:
                    p = 0.5 * (y - x)
                    q = p * p + w
                    z = _math.sqrt(_abs(q))
                    x += t
                    if q >= 0.0:
                        z = p + _sgn(z, p)
                        wr[nn - 1] = wr[nn] = x + z
                        if z != 0.0:
                            wr[nn] = x - w / z
                        wi[nn - 1] = wi[nn] = 0.0
                    else:
                        wr[nn - 1] = wr[nn] = x + p
                        wi[nn - 1] = z
                        wi[nn] = -z
                    nn -= 2
                else:
                    if its == 60:
                        raise LinAlgError("linalg.eig: The algorithm failed to converge")
                    if its == 10 or its == 20:
                        t += x
                        for i in _range(1, nn + 1):
                            a[i][i] -= x
                        s = _abs(a[nn][nn - 1]) + _abs(a[nn - 1][nn - 2])
                        y = x = 0.75 * s
                        w = -0.4375 * s * s
                    its += 1
                    m = nn - 2
                    while m >= l:
                        z = a[m][m]
                        r = x - z
                        s = y - z
                        p = (r * s - w) / a[m + 1][m] + a[m][m + 1]
                        q = a[m + 1][m + 1] - z - r - s
                        r = a[m + 2][m + 1]
                        s = _abs(p) + _abs(q) + _abs(r)
                        p /= s
                        q /= s
                        r /= s
                        if m == l:
                            break
                        u = _abs(a[m][m - 1]) * (_abs(q) + _abs(r))
                        v = _abs(p) * (_abs(a[m - 1][m - 1]) + _abs(z) + _abs(a[m + 1][m + 1]))
                        if u + v == v:
                            break
                        m -= 1
                    for i in _range(m + 2, nn + 1):
                        a[i][i - 2] = 0.0
                        if i != m + 2:
                            a[i][i - 3] = 0.0
                    k = m
                    while k <= nn - 1:
                        if k != m:
                            p = a[k][k - 1]
                            q = a[k + 1][k - 1]
                            r = 0.0
                            if k != nn - 1:
                                r = a[k + 2][k - 1]
                            x = _abs(p) + _abs(q) + _abs(r)
                            if x != 0.0:
                                p /= x
                                q /= x
                                r /= x
                        s = _sgn(_math.sqrt(p * p + q * q + r * r), p)
                        if s != 0.0:
                            if k == m:
                                if l != m:
                                    a[k][k - 1] = -a[k][k - 1]
                            else:
                                a[k][k - 1] = -s * x
                            p += s
                            x = p / s
                            y = q / s
                            z = r / s
                            q /= p
                            r /= p
                            for j in _range(k, nn + 1):
                                p = a[k][j] + q * a[k + 1][j]
                                if k != nn - 1:
                                    p += r * a[k + 2][j]
                                    a[k + 2][j] -= p * z
                                a[k + 1][j] -= p * y
                                a[k][j] -= p * x
                            mmin = nn if nn < k + 3 else k + 3
                            for i in _range(l, mmin + 1):
                                p = x * a[i][k] + y * a[i][k + 1]
                                if k != nn - 1:
                                    p += z * a[i][k + 2]
                                    a[i][k + 2] -= p * r
                                a[i][k + 1] -= p * q
                                a[i][k] -= p
                        k += 1
            if nn < 1 or l >= nn - 1:
                break
    return wr[1:], wi[1:]


def _unit_complex(x, y):
    """(x + i y) scaled to unit norm and rotated so that its largest
    component (the first within a relative 1e-12) is real and positive."""
    n = len(x)
    nrm = _math.sqrt(_math.fsum([x[i] * x[i] + y[i] * y[i] for i in _range(n)]))
    if nrm > 0.0:
        x = [v / nrm for v in x]
        y = [v / nrm for v in y]
    best = 0.0
    at = 0
    for i in _range(n):
        r = _math.hypot(x[i], y[i])
        if r > best + 1e-12 * best:
            best = r
            at = i
    if best > 0.0:
        c = x[at] / best
        s_ = -y[at] / best
        x, y = [x[i] * c - y[i] * s_ for i in _range(n)], [x[i] * s_ + y[i] * c for i in _range(n)]
        y[at] = 0.0
    return x, y


def _eig_lists(M):
    """Eigenvalues (wr, wi) of a real matrix in Schur-diagonal order (a
    conjugate pair with its positive imaginary part first) and its unit
    eigenvectors (vr, vi as column lists): the null vectors of A - lambda I
    from an SVD (of the real 2n x 2n form of A - lambda I for a complex
    lambda), one group per (numerically) repeated real eigenvalue."""
    n = len(M)
    wr, wi = _hqr(M) if n else ([], [])
    scale = _max([_abs(v) for row in M for v in row] + [1e-300])
    tol = 1e-9 * _max(scale, 1.0)
    vr = [None] * n
    vi = [None] * n
    done = [False] * n
    for i in _range(n):
        if done[i]:
            continue
        if wi[i] == 0.0:
            group = [j for j in _range(n) if not done[j] and wi[j] == 0.0 and _abs(wr[j] - wr[i]) <= tol]
            lam = _math.fsum([wr[j] for j in group]) / len(group)
            shifted = [[M[r][c] - (lam if r == c else 0.0) for c in _range(n)] for r in _range(n)]
            U, S, Vh = _svd(shifted, n, n, False)
            for idx, j in enumerate(group):
                v = Vh[n - 1 - idx][:]
                _fix_sign(v)
                vr[j] = v
                vi[j] = [0.0] * n
                done[j] = True
            continue
        a, b = wr[i], wi[i]
        big = [[0.0] * (2 * n) for _ in _range(2 * n)]
        for r in _range(n):
            for c in _range(n):
                v = M[r][c] - (a if r == c else 0.0)
                big[r][c] = v
                big[n + r][n + c] = v
            big[r][n + r] = b
            big[n + r][r] = -b
        U, S, Vh = _svd(big, 2 * n, 2 * n, False)
        v = Vh[2 * n - 1]
        x, y = _unit_complex(v[:n], v[n:])
        vr[i], vi[i], done[i] = x, y, True
        for j in _range(i + 1, n):
            if not done[j] and wi[j] == -b and wr[j] == a:
                vr[j], vi[j], done[j] = x[:], [-t for t in y], True
                break
    return wr, wi, vr, vi


# ---- LU family: det, slogdet, inv, solve -------------------------------------------------
def _cofactor_backward(A, out, name):
    """det's gradient g * det * A^{-T}; zero for an exactly singular matrix,
    as PyTorch 2.11 returns."""
    def backward(g):
        batch = _batch(A)
        dets = torch._k.to_list(out.detach()._s)
        if all(d != 0.0 for d in dets):
            Ainv = _inv_core(A, "linalg.det")
            return (_mT(Ainv) * (g * out).unsqueeze(-1).unsqueeze(-1),)
        mats = _mats(A.detach())
        gs = torch._k.to_list(g.detach()._s)
        res = []
        for M, d, gv in zip(mats, dets, gs):
            n = len(M)
            if d == 0.0:
                res.append([[0.0] * n for _ in _range(n)])
            else:
                LU, perm, piv, sign, info = _lu(M)
                inv = _lu_solve(LU, perm, _identity(n))
                res.append([[gv * d * inv[j][i] for j in _range(n)] for i in _range(n)])
        return (_tensor(res, batch, A.shape[-1], A.shape[-1], A.dtype),)
    return _attach(out, (A,), backward, name, (A, out))


def _lu_diagonals(A):
    """Per matrix of A: (sign of the pivoting permutation, U's diagonal)."""
    batch = _batch(A)
    n = A.shape[-1]
    count = _count(batch)
    nat = _native(_NAT_LU, [A.detach()], (count, n, n))
    if nat is not None:
        diag = _floats(torch.diagonal(_t64(nat[0], (count, n, n)), 0, -2, -1)._s)
        signs = _floats(nat[3])
        return [(signs[b], diag[b * n:(b + 1) * n]) for b in _range(count)]
    out = []
    for M in _mats(A):
        LU, perm, piv, sign, info = _lu(M)
        out.append((sign, [LU[i][i] for i in _range(len(M))]))
    return out


def det(A, *, out=None):
    _check_square(A, "det")
    batch = _batch(A)
    vals = []
    for sign, diag in _lu_diagonals(A):
        d = sign
        for v in diag:
            d *= v
        vals.append(d)
    res = _flat_tensor(vals, batch, A.dtype)
    if _needs(A):
        _cofactor_backward(A, res, "LinalgDet")
    return res


def slogdet(A, *, out=None):
    _check_square(A, "slogdet")
    batch = _batch(A)
    signs, logs = [], []
    for sign, diag in _lu_diagonals(A):
        s = sign
        la = 0.0
        for d in diag:
            if d == 0.0:
                s = 0.0
                la = -_math.inf
                break
            if d != d:
                s = _math.nan
                la = _math.nan
                break
            if d < 0:
                s = -s
            la += _math.log(_abs(d))
        signs.append(s)
        logs.append(la)
    sign_t = _flat_tensor(signs, batch, A.dtype)
    log_t = _flat_tensor(logs, batch, A.dtype)
    if _needs(A):
        def backward(g):
            Ainv = _inv_core(A, "linalg.slogdet")
            return (_mT(Ainv) * g.unsqueeze(-1).unsqueeze(-1),)
        _attach(log_t, (A,), backward, "LinalgSlogdet", (A,))
    return _returns("linalg_slogdet", ("sign", "logabsdet"), (sign_t, log_t))


def _inv_core(A, fname="linalg.inv", check=True):
    batch = _batch(A)
    n = A.shape[-1]
    count = _count(batch)
    nat = _native(_NAT_SOLVE, [A.detach(), _eye_like(n, (count,), torch.float64).contiguous()], (count, n, n))
    if nat is not None:
        infos = [_int(v) for v in _floats(nat[1])]
        if check:
            for b, info in enumerate(infos):
                if info:
                    raise LinAlgError(_batch_msg(fname, b, count, "The diagonal element %d is zero, the inversion could not be completed because the input matrix is singular." % info))
        out = _as(nat[0], batch + (n, n), A.dtype)
    else:
        mats = _mats(A)
        res, infos = [], []
        for b, M in enumerate(mats):
            LU, perm, piv, sign, info = _lu(M)
            if info and check:
                raise LinAlgError(_batch_msg(fname, b, len(mats), "The diagonal element %d is zero, the inversion could not be completed because the input matrix is singular." % info))
            res.append(_lu_solve(LU, perm, _identity(n)))
            infos.append(info)
        out = _tensor(res, batch, n, n, A.dtype)
    if _needs(A):
        def backward(g):
            t = _mT(out)
            return (-torch.matmul(t, torch.matmul(g, t)),)
        _attach(out, (A,), backward, "LinalgInvEx", (out,))
    return out if check else (out, infos)


def inv(A, *, out=None):
    _check_square(A, "inv", allow_complex=True)
    if A.dtype.is_complex:
        n = A.shape[-1]
        return _csolve(A, _eye_like(n, _batch(A), A.dtype))
    return _inv_core(A)


def inv_ex(A, *, check_errors=False, out=None):
    _check_square(A, "inv_ex")
    res, infos = _inv_core(A, "linalg.inv", check=False)
    if check_errors and any(infos):
        _inv_core(A)
    info = _flat_tensor(infos, _batch(A), torch.int32)
    return _returns("linalg_inv_ex", ("inverse", "info"), (res, info))


def _is_vector_rhs(A, B):
    return B.ndim == 1 or (B.ndim == A.ndim - 1 and tuple(B.shape) == tuple(A.shape[:-1]))


def _solve_core(A, B, check=True, fname="torch.linalg.solve"):
    """X = A^{-1} B for A [..., n, n], B [..., n, k] with equal batches."""
    batch = _batch(A)
    n = A.shape[-1]
    k = B.shape[-1]
    count = _count(batch)
    dt = torch.promote_types(A.dtype, B.dtype)
    nat = _native(_NAT_SOLVE, [A.detach(), B.detach()], (count, n, k))
    if nat is not None:
        infos = [_int(v) for v in _floats(nat[1])]
        if check:
            for b, info in enumerate(infos):
                if info:
                    raise LinAlgError(_batch_msg(fname, b, count, "The solver failed because the input matrix is singular."))
        out = _as(nat[0], batch + (n, k), dt)
    else:
        res, infos = [], []
        Ams = _mats(A)
        Bms = _mats(B)
        for b, (M, R) in enumerate(zip(Ams, Bms)):
            LU, perm, piv, sign, info = _lu(M)
            if info and check:
                raise LinAlgError(_batch_msg(fname, b, len(Ams), "The solver failed because the input matrix is singular."))
            res.append(_lu_solve(LU, perm, R))
            infos.append(info)
        out = _tensor(res, batch, n, k, dt)
    if _needs(A, B):
        def backward(g):
            gB = _solve_core(_mT(A), g)
            gA = -torch.matmul(gB, _mT(out)) if A.requires_grad else None
            return (gA, gB if B.requires_grad else None)
        _attach(out, (A, B), backward, "LinalgSolveEx", (A, out))
    return out if check else (out, infos)


def _solve(A, B, left, check, fname, allow_vector=True):
    _check_square(A, fname, allow_complex=True)
    _check_float(B, fname, "B", allow_complex=True)
    if A.dtype != B.dtype:
        raise RuntimeError("%s: Expected A and B to have the same dtype, but found A of type %s and B of type %s instead" % (fname, torch._CAST_NAME[A.dtype.name], torch._CAST_NAME[B.dtype.name]))
    vector = allow_vector and _is_vector_rhs(A, B)
    Bm = B.unsqueeze(-1) if vector else B
    if not left:
        A = _mT(A)
        Bm = _mT(Bm)
        if vector:
            Bm = _mT(Bm)
    n = A.shape[-1]
    if Bm.shape[-2] != n:
        raise RuntimeError("linalg.solve: Incompatible shapes of A and B for the equation %s (%dx%d and %dx%d)" % ("AX = B" if left else "XA = B", A.shape[-2], A.shape[-1], Bm.shape[-2], Bm.shape[-1]))
    A2, B2, batch = _broadcast_batch(A, Bm)
    if A2.dtype.is_complex:
        got = _csolve(A2, B2)
        if not check:
            got = (got, [0] * _count(batch))
    else:
        got = _solve_core(A2, B2, check, "torch.linalg.solve")
    X, infos = (got, None) if check else got
    if not left and not vector:
        X = _mT(X)
    if vector:
        X = X.squeeze(-1)
    return X, infos, batch


def solve(A, B, *, left=True, out=None):
    return _solve(A, B, left, True, "linalg.solve")[0]


def solve_ex(A, B, *, left=True, check_errors=False, out=None):
    X, infos, batch = _solve(A, B, left, check_errors, "linalg.solve_ex")
    if infos is None:
        infos = [0] * _count(batch)
    return _returns("linalg_solve_ex", ("result", "info"), (X, _flat_tensor(infos, batch, torch.int32)))


def _tri_core(A, B, upper, unit):
    """Left triangular solve with equal batches."""
    batch = _batch(A)
    n = A.shape[-1]
    k = B.shape[-1]
    nat = _native(_NAT_TRI, [A.detach(), B.detach()], (_count(batch), n, k, 1 if upper else 0, 1 if unit else 0))
    if nat is not None:
        out = _as(nat[0], batch + (n, k), A.dtype)
    else:
        res = [_tri_solve(M, R, upper, unit) for M, R in zip(_mats(A), _mats(B))]
        out = _tensor(res, batch, n, k, A.dtype)
    if _needs(A, B):
        def backward(g):
            gB = _tri_core(_mT(A), g, not upper, unit)
            gA = None
            if A.requires_grad:
                gA = -torch.matmul(gB, _mT(out))
                if upper:
                    gA = torch.triu(gA, 1 if unit else 0)
                else:
                    gA = torch.tril(gA, -1 if unit else 0)
            return (gA, gB if B.requires_grad else None)
        _attach(out, (A, B), backward, "LinalgSolveTriangular", (A, out))
    return out


def solve_triangular(A, B, *, upper, left=True, unitriangular=False, out=None):
    _check_matrix(A, "solve_triangular")
    _check_matrix(B, "solve_triangular", "B")
    if A.shape[-1] != A.shape[-2]:
        raise RuntimeError("linalg.solve_triangular: A must be batches of square matrices, but they are %d by %d matrices" % (A.shape[-2], A.shape[-1]))
    if A.dtype != B.dtype:
        raise RuntimeError("linalg.solve_triangular: Expected A and B to have the same dtype, but found A of type %s and B of type %s instead" % (torch._CAST_NAME[A.dtype.name], torch._CAST_NAME[B.dtype.name]))
    n = A.shape[-1]
    if left:
        if B.shape[-2] != n:
            raise RuntimeError("linalg.solve_triangular: Incompatible shapes of A and B for the equation AX = B (%dx%d and %dx%d)" % (n, n, B.shape[-2], B.shape[-1]))
        A2, B2, batch = _broadcast_batch(A, B)
        return _tri_core(A2, B2, bool(upper), bool(unitriangular))
    if B.shape[-1] != n:
        raise RuntimeError("linalg.solve_triangular: Incompatible shapes of A and B for the equation XA = B (%dx%d and %dx%d)" % (n, n, B.shape[-2], B.shape[-1]))
    A2, B2, batch = _broadcast_batch(_mT(A), _mT(B))
    return _mT(_tri_core(A2, B2, not upper, bool(unitriangular)))


def _lu_factors(A):
    """The packed LU factors (A's dtype), the 1-based pivots and the infos
    of every matrix of A, and each row permutation (perm[i] the source row
    of row i), without history."""
    batch = _batch(A)
    m, n = A.shape[-2], A.shape[-1]
    k = _min(m, n)
    count = _count(batch)
    nat = _native(_NAT_LU, [A.detach()], (count, m, n))
    if nat is not None:
        LU = _as(nat[0], batch + (m, n), A.dtype)
        perms = [_int(v) for v in _floats(nat[1])]
        pivs = [_int(v) for v in _floats(nat[2])]
        infos = [_int(v) for v in _floats(nat[4])]
        return LU, pivs, infos, [perms[b * m:(b + 1) * m] for b in _range(count)]
    mats, pivs, infos, perms = [], [], [], []
    for M in _mats(A):
        LU, perm, piv, sign, info = _lu(M)
        mats.append(LU)
        pivs.extend(piv)
        infos.append(info)
        perms.append(perm)
    return _tensor(mats, batch, m, n, A.dtype), pivs, infos, perms


def _perm_matrices(perms, batch, m, dt):
    """P with A = P L U: P[perm[i]][i] = 1."""
    flat = []
    for perm in perms:
        P = [[0.0] * m for _ in _range(m)]
        for i in _range(m):
            P[perm[i]][i] = 1.0
        for row in P:
            flat.extend(row)
    return _flat_tensor(flat, batch + (m, m), dt)


def _unpack(LU):
    """L (unit lower, m x k) and U (upper, k x n) of packed factors, as
    tensor operations (so differentiable)."""
    m, n = LU.shape[-2], LU.shape[-1]
    k = _min(m, n)
    batch = _batch(LU)
    L = torch.tril(LU[..., :, :k], -1) + torch.eye(m, k, dtype=LU.dtype)
    U = torch.triu(LU[..., :k, :])
    return L, U


def _lu_backward(gL, gU, P, L, U):
    """PyTorch's linalg_lu_backward: A's gradient from those of L and U
    (either may be None), for A = P L U."""
    m, n = L.shape[-2], U.shape[-1]
    k = _min(m, n)
    if m == n:
        gA = None
        if gL is not None:
            gA = torch.tril(torch.matmul(_mT(L), gL), -1)
        if gU is not None:
            t = torch.triu(torch.matmul(gU, _mT(U)))
            gA = t if gA is None else gA + t
        gA = solve_triangular(_mT(U), gA, upper=False, left=False)
        gA = solve_triangular(_mT(L), gA, upper=True, unitriangular=True)
        return torch.matmul(P, gA)
    if m < n:
        U1 = U[..., :, :k]
        gA = None
        if gL is not None:
            gA = torch.matmul(_mT(L), gL)
        if gU is not None:
            t = torch.matmul(torch.triu(gU), _mT(U))
            gA = -t if gA is None else gA - t
        gA = solve_triangular(_mT(U1), torch.tril(gA, -1), upper=False, left=False)
        if gU is not None:
            gA = torch.cat([gA + torch.triu(gU[..., :, :k]), gU[..., :, k:]], -1)
        gA = solve_triangular(_mT(L), gA, upper=True, unitriangular=True)
        if gU is None:
            gA = torch.cat([gA, torch.zeros_like(U[..., :, k:])], -1)
        return torch.matmul(P, gA)
    L1 = L[..., :k, :]
    gA = None
    if gU is not None:
        gA = torch.matmul(gU, _mT(U))
    if gL is not None:
        t = torch.matmul(_mT(L), torch.tril(gL, -1))
        gA = -t if gA is None else gA - t
    gA = solve_triangular(_mT(L1), torch.triu(gA), upper=True, unitriangular=True)
    if gL is not None:
        gA = torch.cat([gA + torch.tril(gL[..., :k, :], -1), gL[..., k:, :]], -2)
    gA = solve_triangular(_mT(U), gA, upper=False, left=False)
    if gL is None:
        gA = torch.cat([gA, torch.zeros_like(L[..., k:, :])], -2)
    return torch.matmul(P, gA)


def lu_factor_ex(A, *, pivot=True, check_errors=False, out=None):
    _check_matrix(A, "lu_factor_ex")
    if not pivot:
        raise NotImplementedError("linalg.lu_factor: LU without pivoting is not supported on Zipp")
    batch = _batch(A)
    m, n = A.shape[-2], A.shape[-1]
    LU, pivs, infos, perms = _lu_factors(A)
    if check_errors and any(infos):
        raise LinAlgError("torch.linalg.lu_factor_ex: U[%d,%d] is zero and using it on lu_solve would result in a division by zero. If you still want to perform the factorization, consider calling linalg.lu(A, pivot) or linalg.lu_factor_ex(A, pivot)" % (max(infos), max(infos)))
    if _needs(A):
        k = _min(m, n)
        P = _perm_matrices(perms, batch, m, A.dtype)

        def backward(g):
            # PyTorch's lu_factor_ex_backward: L's part of the packed
            # gradient is its first k columns, U's its first k rows.
            L, U = _unpack(LU)
            return (_lu_backward(g[..., :, :k], g[..., :k, :], P, L, U),)
        _attach(LU, (A,), backward, "LinalgLuFactorEx", (LU,))
    return _returns("linalg_lu_factor_ex", ("LU", "pivots", "info"), (LU, _flat_tensor(pivs, batch + (_min(m, n),), torch.int32), _flat_tensor(infos, batch, torch.int32)))


def lu_factor(A, *, pivot=True, out=None):
    LU, pivots, info = lu_factor_ex(A, pivot=pivot)
    return _returns("linalg_lu_factor", ("LU", "pivots"), (LU, pivots))


def lu(A, *, pivot=True, out=None):
    """P, L, U with A = P L U (L unit lower m x k, U upper k x n)."""
    _check_matrix(A, "lu")
    if not pivot:
        raise NotImplementedError("linalg.lu: LU without pivoting is not supported on Zipp")
    batch = _batch(A)
    m = A.shape[-2]
    LU, pivs, infos, perms = _lu_factors(A)
    P = _perm_matrices(perms, batch, m, A.dtype)
    with torch.no_grad():
        L, U = _unpack(LU)
    if _needs(A):
        _attach(L, (A,), lambda g: (_lu_backward(g, None, P, L, U),), "LinalgLuBackward", (L, U))
        _attach(U, (A,), lambda g: (_lu_backward(None, g, P, L, U),), "LinalgLuBackward", (L, U))
    return _returns("linalg_lu", ("P", "L", "U"), (P, L, U))


def lu_solve(LU, pivots, B, *, left=True, adjoint=False, out=None):
    """Solves with A = P L U rebuilt from the packed factors by tensor
    operations, so the gradients reach LU (through tril/triu) and B."""
    _check_square(LU, "lu_solve", "LU")
    n = LU.shape[-1]
    batch = _batch(LU)
    piv = torch._k.to_list(pivots._s)
    perms = []
    for b in _range(_count(batch)):
        perm = list(_range(n))
        for i in _range(n):
            p = _int(piv[b * n + i]) - 1
            perm[i], perm[p] = perm[p], perm[i]
        perms.append(perm)
    P = _perm_matrices(perms, batch, n, LU.dtype)
    L, U = _unpack(LU)
    A = torch.matmul(P, torch.matmul(L, U))
    if adjoint:
        A = _mT(A)
    return _solve(A, B, left, True, "linalg.lu_solve", False)[0]


# ---- Cholesky ----------------------------------------------------------------------------
def _cholesky_core(A, upper, check, fname):
    batch = _batch(A)
    n = A.shape[-1]
    count = _count(batch)
    nat = _native(_NAT_CHOL, [A.detach()], (count, n))
    if nat is not None:
        infos = [_int(v) for v in _floats(nat[1])]
        if check:
            for b, info in enumerate(infos):
                if info:
                    raise LinAlgError(_batch_msg(fname, b, count, "The factorization could not be completed because the input is not positive-definite (the leading minor of order %d is not positive-definite)." % info))
        out = _as(nat[0], batch + (n, n), A.dtype)
        if upper:
            out = _mT(out).contiguous()
    else:
        mats = _mats(A)
        res, infos = [], []
        for b, M in enumerate(mats):
            L, info = _cholesky(M)
            if info and check:
                raise LinAlgError(_batch_msg(fname, b, len(mats), "The factorization could not be completed because the input is not positive-definite (the leading minor of order %d is not positive-definite)." % info))
            res.append(_transpose(L) if upper else L)
            infos.append(info)
        out = _tensor(res, batch, n, n, A.dtype)
    if _needs(A):
        def backward(g):
            L = _mT(out) if upper else out
            gL = _mT(g) if upper else g
            P = torch.tril(torch.matmul(_mT(L), gL))
            P = 0.5 * (P + _mT(torch.tril(P, -1)))
            Linv = _tri_core(L, _eye_like(n, batch, L.dtype), False, False)
            return (torch.matmul(_mT(Linv), torch.matmul(P, Linv)),)
        _attach(out, (A,), backward, "LinalgCholeskyEx", (out,))
    return out, infos


def cholesky(A, *, upper=False, out=None):
    _check_square(A, "cholesky")
    return _cholesky_core(A, upper, True, "linalg.cholesky")[0]


def cholesky_ex(A, *, upper=False, check_errors=False, out=None):
    _check_square(A, "cholesky_ex")
    L, infos = _cholesky_core(A, upper, check_errors, "linalg.cholesky_ex")
    return _returns("linalg_cholesky_ex", ("L", "info"), (L, _flat_tensor(infos, _batch(A), torch.int32)))


# ---- QR ----------------------------------------------------------------------------------
def _qr_backward_square(gQ, gR, Q, R):
    """m >= n (reduced): gA = (gQ + Q copyltu(M)) R^{-T}, M = R gR^T - gQ^T Q."""
    M = None
    if gR is not None:
        M = torch.matmul(R, _mT(gR))
    if gQ is not None:
        t = torch.matmul(_mT(gQ), Q)
        M = -t if M is None else M - t
    b = torch.tril(M, -1)
    sym = b + _mT(b) + torch.diag_embed(torch.diagonal(M, 0, -2, -1))
    X = torch.matmul(Q, sym)
    if gQ is not None:
        X = X + gQ
    # X R^{-T}: solve R Y^T = X^T
    return _mT(_tri_core(R, _mT(X), True, False))


def qr(A, mode="reduced", *, out=None):
    _check_matrix(A, "qr")
    if mode not in ("reduced", "complete", "r"):
        raise RuntimeError("qr received unrecognized mode '%s' but expected one of 'reduced' (default), 'r', or 'complete'" % mode)
    batch = _batch(A)
    m, n = A.shape[-2], A.shape[-1]
    k = _min(m, n)
    qcols = m if mode == "complete" else k
    rrows = m if mode == "complete" else k
    nat = _native(_NAT_QR, [A.detach()], (_count(batch), m, n, qcols if mode != "r" else 0, rrows))
    if nat is not None:
        R = _as(nat[1], batch + (rrows, n), A.dtype)
        Q = _as(nat[0], batch + (m, qcols), A.dtype) if mode != "r" else torch.zeros(0, dtype=A.dtype)
    else:
        Qs, Rs = [], []
        for M in _mats(A):
            W, taus = _householder(M)
            Rs.append([[W[i][j] if j >= i else 0.0 for j in _range(n)] for i in _range(rrows)])
            if mode != "r":
                Qs.append(_form_q(W, taus, m, qcols))
        R = _tensor(Rs, batch, rrows, n, A.dtype)
        Q = _tensor(Qs, batch, m, qcols, A.dtype) if mode != "r" else torch.zeros(0, dtype=A.dtype)
    if _needs(A):
        def check():
            if mode == "r":
                raise RuntimeError("The derivative of linalg.qr depends on Q, which is not computed when mode='r'. Please use linalg.qr(A, mode='reduced') if you are going to differentiate through linalg.qr.")
            if mode == "complete" and m > n:
                raise RuntimeError("The QR decomposition is not differentiable when mode='complete' and nrows > ncols.")

        def backward_parts(gQ, gR):
            check()
            if m >= n:
                return _qr_backward_square(gQ, gR, Q, R)
            # A = [X | Y], X = Q R1 square, Y = Q R2.
            R1 = R[..., :, :m]
            gR1 = None if gR is None else gR[..., :, :m]
            gR2 = None if gR is None else gR[..., :, m:]
            Y = A[..., :, m:]
            gQ2 = gQ
            if gR2 is not None:
                t = torch.matmul(Y, _mT(gR2))
                gQ2 = t if gQ2 is None else gQ2 + t
            gX = _qr_backward_square(gQ2, gR1, Q, R1)
            gY = torch.matmul(Q, gR2) if gR2 is not None else torch.zeros(*(tuple(batch) + (m, n - m)), dtype=A.dtype)
            return torch.cat([gX, gY], -1)

        if mode != "r":
            _attach(Q, (A,), lambda g: (backward_parts(g, None),), "LinalgQr", (A, Q, R))
        _attach(R, (A,), lambda g: (backward_parts(None, g),), "LinalgQr", (A, Q if mode != "r" else None, R))
    return _returns("linalg_qr", ("Q", "R"), (Q, R))


def householder_product(A, tau, *, out=None):
    """Q = H_1 ... H_k from geqrf's reflectors: the first n columns (A is
    [..., m, n], tau [..., k]). Composed from tensor operations."""
    _check_matrix(A, "householder_product")
    m, n = A.shape[-2], A.shape[-1]
    k = tau.shape[-1]
    if n > m:
        raise RuntimeError("torch.linalg.householder_product: input.shape[-2] must be greater than or equal to input.shape[-1]")
    if k > n:
        raise RuntimeError("torch.linalg.householder_product: input.shape[-1] must be greater than or equal to tau.shape[-1]")
    batch = _batch(A)
    Q = torch.eye(m, n, dtype=A.dtype)
    if batch:
        Q = Q.expand(*(batch + (m, n)))
    for i in _range(k - 1, -1, -1):
        parts = []
        if i > 0:
            parts.append(torch.zeros(*(batch + (i,)), dtype=A.dtype))
        parts.append(torch.ones(*(batch + (1,)), dtype=A.dtype))
        if i + 1 < m:
            parts.append(A[..., i + 1:, i])
        v = torch.cat(parts, -1).unsqueeze(-1)
        t = tau[..., i].unsqueeze(-1).unsqueeze(-1)
        Q = Q - t * torch.matmul(v, torch.matmul(_mT(v), Q))
    return Q


# ---- symmetric eigenproblem --------------------------------------------------------------
def _sym_from(M, uplo):
    n = len(M)
    if uplo == "L":
        return [[M[i][j] if j <= i else M[j][i] for j in _range(n)] for i in _range(n)]
    return [[M[i][j] if j >= i else M[j][i] for j in _range(n)] for i in _range(n)]


def _eigh_core(A, uplo, want_vectors, fname):
    _check_square(A, fname)
    uplo = str(uplo).upper()
    if uplo not in ("L", "U"):
        raise RuntimeError("Expected UPLO argument to be 'L' or 'U', but got %s" % uplo)
    batch = _batch(A)
    n = A.shape[-1]
    L, V = _eigh_values(A, uplo)
    if _needs(A):
        def gl_backward(g):
            return (torch.matmul(V * g.unsqueeze(-2), _mT(V)),)

        def gv_backward(g):
            VhgV = torch.matmul(_mT(V), g)
            VhgV = 0.5 * (VhgV - _mT(VhgV))
            E = L.unsqueeze(-2) - L.unsqueeze(-1)
            E = E + torch.eye(n, dtype=A.dtype)
            inner = VhgV / E
            inner = inner - torch.diag_embed(torch.diagonal(inner, 0, -2, -1))
            return (torch.matmul(V, torch.matmul(inner, _mT(V))),)
        _attach(L, (A,), gl_backward, "LinalgEigh", (V,))
        # V gets a history even for eigvalsh: its gradient reads V, and a
        # second derivative (create_graph) must reach A through it.
        _attach(V, (A,), gv_backward, "LinalgEigh", (L, V))
    return L, V


def _eigh_values(A, uplo):
    """Eigenvalues (ascending) and eigenvectors of the symmetric matrices
    A's `uplo` triangles name, without history."""
    batch = _batch(A)
    n = A.shape[-1]
    nat = _native(_NAT_EIGH, [A.detach()], (_count(batch), n, 1 if uplo == "L" else 0))
    if nat is not None:
        return _as(nat[0], batch + (n,), A.dtype), _as(nat[1], batch + (n, n), A.dtype)
    vals, vecs = [], []
    for M in _mats(A):
        w, V = _jacobi_eigh(_sym_from(M, uplo))
        vals.extend(w)
        vecs.append(V if V else [[] for _ in _range(n)])
    return _flat_tensor(vals, batch + (n,), A.dtype), _tensor(vecs, batch, n, n, A.dtype)


def eigh(A, UPLO="L", *, out=None):
    L, V = _eigh_core(A, UPLO, True, "eigh")
    return _returns("linalg_eigh", ("eigenvalues", "eigenvectors"), (L, V))


def eigvalsh(A, UPLO="L", *, out=None):
    return _eigh_core(A, UPLO, False, "eigvalsh")[0]


# ---- general eigenproblem (real spectra only) --------------------------------------------
def _csolve(A, B):
    """X = A^{-1} B for complex A [..., n, n] and B [..., n, k], through the
    real system [[Re A, -Im A], [Im A, Re A]] [Re X; Im X] = [Re B; Im B]
    (differentiable through the real solve)."""
    Ar, Ai = torch.real(A), torch.imag(A)
    Br, Bi = torch.real(B), torch.imag(B)
    M = torch.cat([torch.cat([Ar, -Ai], -1), torch.cat([Ai, Ar], -1)], -2)
    R = torch.cat([Br, Bi], -2)
    M2, R2, batch = _broadcast_batch(M, R)
    X = _solve_core(M2, R2)
    n = A.shape[-1]
    return torch.complex(X[..., :n, :], X[..., n:, :])


def _eig_core(A, want_vectors, fname):
    """Eigenvalues and eigenvectors of real matrices as complex tensors
    (complex64 for float32, complex128 for float64), with PyTorch's
    gradients (linalg_eig_backward; a real input takes the real part)."""
    _check_square(A, fname)
    batch = _batch(A)
    n = A.shape[-1]
    count = _count(batch)
    cdt = A.dtype.to_complex()
    nat = _native(_NAT_EIG, [A.detach()], (count, n))
    if nat is not None:
        if any(ok == 0.0 for ok in _floats(nat[4])):
            raise LinAlgError("linalg.eig: The algorithm failed to converge")
        wr, wi = _t64(nat[0], batch + (n,)), _t64(nat[1], batch + (n,))
        vr, vi = _t64(nat[2], batch + (n, n)), _t64(nat[3], batch + (n, n))
    else:
        fr, fi, gr, gi = [], [], [], []
        for M in _mats(A):
            wr_, wi_, vr_, vi_ = _eig_lists(M)
            fr.extend(wr_)
            fi.extend(wi_)
            for r in _range(n):
                gr.extend([vr_[c][r] for c in _range(n)])
                gi.extend([vi_[c][r] for c in _range(n)])
        wr, wi = _flat_tensor(fr, batch + (n,), torch.float64), _flat_tensor(fi, batch + (n,), torch.float64)
        vr, vi = _flat_tensor(gr, batch + (n, n), torch.float64), _flat_tensor(gi, batch + (n, n), torch.float64)
    with torch.no_grad():
        L = torch.complex(wr, wi).to(cdt)
        V = torch.complex(vr, vi).to(cdt)
    if _needs(A):
        Vh = V.mH

        def conj_by(inner):
            # V^{-H} inner V^H, whose real part is a real input's gradient
            return torch.real(_csolve(Vh, torch.matmul(inner, Vh)))

        def gl_backward(g):
            return (conj_by(torch.diag_embed(g.to(cdt))),)

        def gv_backward(g):
            VhgV = torch.matmul(Vh, g.to(cdt))
            d = torch.diagonal(VhgV, 0, -2, -1)
            im = torch.imag(d)
            if not torch.allclose(im, torch.zeros_like(im), rtol=1e-2, atol=1e-2):
                raise RuntimeError("linalg_eig_backward: The eigenvectors in the complex case are specified up to multiplication by e^{i phi}. The specified loss function depends on this quantity, so it is ill-defined.")
            VhgV = VhgV - torch.matmul(Vh, V * torch.real(d).unsqueeze(-2))
            Lc = torch.conj(L)
            E = Lc.unsqueeze(-2) - Lc.unsqueeze(-1)
            E = E + torch.eye(n, dtype=cdt)
            inner = VhgV / E
            inner = inner - torch.diag_embed(torch.diagonal(inner, 0, -2, -1))
            return (conj_by(inner),)
        _attach(L, (A,), gl_backward, "LinalgEig", (V,))
        if want_vectors:
            _attach(V, (A,), gv_backward, "LinalgEig", (L, V))
    return L, V


def eig(A, *, out=None):
    """Eigenvalues and unit eigenvectors of real matrices, as complex
    tensors like PyTorch's. The eigenvalues come in the Schur form's
    diagonal order (a conjugate pair with its positive imaginary part
    first), which is LAPACK's for most matrices but not guaranteed to be;
    each eigenvector's largest component is real and positive."""
    L, V = _eig_core(A, True, "eig")
    return _returns("linalg_eig", ("eigenvalues", "eigenvectors"), (L, V))


def eigvals(A, *, out=None):
    return _eig_core(A, False, "eigvals")[0]


# ---- SVD ---------------------------------------------------------------------------------
def _svd_core(A, full_matrices, want_uv, fname):
    _check_matrix(A, fname)
    batch = _batch(A)
    m, n = A.shape[-2], A.shape[-1]
    k = _min(m, n)
    ucols = m if (full_matrices and want_uv) else k
    vrows = n if (full_matrices and want_uv) else k
    U, S, Vh = _svd_values(A, full_matrices and want_uv)
    if _needs(A):
        def Uk():
            return U if ucols == k else U[..., :, :k]

        def Vhk():
            return Vh if vrows == k else Vh[..., :k, :]

        def gs_backward(g):
            Uk_, Vhk_ = Uk(), Vhk()
            if m >= n:
                return (torch.matmul(Uk_, g.unsqueeze(-1) * Vhk_),)
            return (torch.matmul(Uk_ * g.unsqueeze(-2), Vhk_),)

        def uv_backward(gU, gVh):
            Uk_, Vhk_ = Uk(), Vhk()
            if gU is not None and ucols != k:
                gU = gU[..., :, :k]
            if gVh is not None and vrows != k:
                gVh = gVh[..., :k, :]
            S2 = S * S
            E = S2.unsqueeze(-2) - S2.unsqueeze(-1)
            E = E + torch.eye(k, dtype=A.dtype)
            if gU is not None:
                UhgU = torch.matmul(_mT(Uk_), gU)
                UhgU = UhgU - _mT(UhgU)
                gA = (UhgU / E) * S.unsqueeze(-2)
            else:
                VhgV = torch.matmul(Vhk_, _mT(gVh))
                VhgV = VhgV - _mT(VhgV)
                gA = S.unsqueeze(-1) * (VhgV / E)
            gA = gA - torch.diag_embed(torch.diagonal(gA, 0, -2, -1))
            if m > n and gU is not None:
                gA = torch.matmul(Uk_, gA)
                gUSinv = gU / S.unsqueeze(-2)
                gA = gA + gUSinv - torch.matmul(Uk_, torch.matmul(_mT(Uk_), gUSinv))
                return (torch.matmul(gA, Vhk_),)
            if m < n and gVh is not None:
                gA = torch.matmul(gA, Vhk_)
                SinvgVh = gVh / S.unsqueeze(-1)
                gA = gA + SinvgVh - torch.matmul(torch.matmul(SinvgVh, _mT(Vhk_)), Vhk_)
                return (torch.matmul(Uk_, gA),)
            return (torch.matmul(Uk_, torch.matmul(gA, Vhk_)),)
        _attach(S, (A,), gs_backward, "LinalgSvd", (U, Vh))
        _attach(U, (A,), lambda g: uv_backward(g, None), "LinalgSvd", (U, S, Vh))
        _attach(Vh, (A,), lambda g: uv_backward(None, g), "LinalgSvd", (U, S, Vh))
    return U, S, Vh


def _svd_values(A, full):
    """U, S (descending), Vh of every matrix of A, without history, in
    A's dtype, or float64 with `f64`."""
    batch = _batch(A)
    m, n = A.shape[-2], A.shape[-1]
    k = _min(m, n)
    ucols = m if full else k
    vrows = n if full else k
    nat = _native(_NAT_SVD, [A.detach()], (_count(batch), m, n, 1 if full else 0))
    if nat is not None:
        return _as(nat[0], batch + (m, ucols), A.dtype), _as(nat[1], batch + (k,), A.dtype), _as(nat[2], batch + (vrows, n), A.dtype)
    Us, Ss, Vhs = [], [], []
    for M in _mats(A):
        U, S, Vh = _svd(M, m, n, full)
        Us.append(U)
        Ss.extend(S)
        Vhs.append(Vh)
    return _tensor(Us, batch, m, ucols, A.dtype), _flat_tensor(Ss, batch + (k,), A.dtype), _tensor(Vhs, batch, vrows, n, A.dtype)


def svd(A, full_matrices=True, *, driver=None, out=None):
    U, S, Vh = _svd_core(A, full_matrices, True, "svd")
    return _returns("linalg_svd", ("U", "S", "Vh"), (U, S, Vh))


def svdvals(A, *, driver=None, out=None):
    return _svd_core(A, False, False, "svdvals")[1]


# ---- pseudo-inverse, rank, least squares -------------------------------------------------
def _tol_values(v, count):
    """A tolerance argument (None, a number or a tensor broadcasting over the
    batch) as one float (or None) per matrix."""
    if v is None:
        return [None] * count
    if isinstance(v, torch.Tensor):
        vals = torch._k.to_list(v.detach()._s)
        if len(vals) == 1:
            return [_float(vals[0])] * count
        if len(vals) == count:
            return [_float(x) for x in vals]
        raise RuntimeError("linalg: atol/rtol must broadcast over the batch of matrices")
    return [_float(v)] * count


def _thresholds(A, atol, rtol, count):
    m, n = A.shape[-2], A.shape[-1]
    eps = torch.finfo(A.dtype).eps
    atols = _tol_values(atol, count)
    rtols = _tol_values(rtol, count)
    out = []
    for a, r in zip(atols, rtols):
        if r is None:
            r = 0.0 if (a is not None and a > 0) else eps * _max(m, n)
        out.append((0.0 if a is None else a, r))
    return out


def _pinv_backward(g, P, A):
    m, n = A.shape[-2], A.shape[-1]
    Ph = _mT(P)
    gh = _mT(g)
    if m <= n:
        K = torch.matmul(gh, P)
        KPh = torch.matmul(K, Ph)
        return -_mT(torch.matmul(P, K)) + KPh - torch.matmul(torch.matmul(A, P), KPh) + torch.matmul(torch.matmul(Ph, P), gh - torch.matmul(K, A))
    K = torch.matmul(P, gh)
    PhK = torch.matmul(Ph, K)
    return -_mT(torch.matmul(K, P)) + torch.matmul(torch.matmul(gh - torch.matmul(A, K), P), Ph) + PhK - torch.matmul(torch.matmul(PhK, P), A)


def pinv(A, rcond=None, hermitian=False, *, atol=None, rtol=None, out=None):
    _check_matrix(A, "pinv")
    if rcond is not None:
        if rtol is not None:
            raise RuntimeError("linalg.pinv: rcond and rtol cannot both be given")
        rtol = rcond
    batch = _batch(A)
    m, n = A.shape[-2], A.shape[-1]
    count = _count(batch)
    tols = _thresholds(A, atol, rtol, count)
    A64 = A.detach().to(torch.float64)
    with torch.no_grad():
        if hermitian:
            w, V = _eigh_values(A64, "L")
            ws = _floats(w._s)
            inv = []
            for b, (a, r) in enumerate(tols):
                row = ws[b * n:(b + 1) * n]
                big = _max([_abs(x) for x in row] + [0.0])
                thr = _max(a, r * big)
                inv.extend([1.0 / x if _abs(x) > thr else 0.0 for x in row])
            P = torch.matmul(V * _flat_tensor(inv, batch + (1, n), torch.float64), _mT(V))
        else:
            U, S, Vh = _svd_values(A64, False)
            k = _min(m, n)
            ss = _floats(S._s)
            inv = []
            for b, (a, r) in enumerate(tols):
                row = ss[b * k:(b + 1) * k]
                big = row[0] if row else 0.0
                thr = _max(a, r * big)
                inv.extend([1.0 / x if x > thr else 0.0 for x in row])
            P = torch.matmul(_mT(Vh) * _flat_tensor(inv, batch + (1, k), torch.float64), _mT(U))
    P = P.to(A.dtype) if A.dtype is not torch.float64 else P
    if _needs(A):
        _attach(P, (A,), lambda g: (_pinv_backward(g, P, A),), "LinalgPinv", (A, P))
    return P


def matrix_rank(A, tol=None, hermitian=False, *, atol=None, rtol=None, out=None):
    _check_matrix(A, "matrix_rank")
    if tol is not None:
        atol = tol
    batch = _batch(A)
    m, n = A.shape[-2], A.shape[-1]
    count = _count(batch)
    tols = _thresholds(A, atol, rtol, count)
    ranks = []
    with torch.no_grad():
        if hermitian:
            vals = _floats(_eigh_values(A.detach().to(torch.float64), "L")[0]._s)
            k = n
        else:
            vals = _floats(_svd_values(A.detach().to(torch.float64), False)[1]._s)
            k = _min(m, n)
    for b, (a, r) in enumerate(tols):
        S = vals[b * k:(b + 1) * k]
        if hermitian:
            S = sorted([_abs(x) for x in S], reverse=True)
        big = S[0] if S else 0.0
        thr = _max(a, r * big)
        ranks.append(len([s for s in S if s > thr]))
    return _flat_tensor(ranks, batch, torch.int64)


def lstsq(A, B, rcond=None, *, driver=None):
    _check_matrix(A, "lstsq")
    _check_float(B, "lstsq", "B")
    if A.dtype != B.dtype:
        raise RuntimeError("torch.linalg.lstsq: Expected input and other to have the same dtype, but got input's dtype %s and other's dtype %s" % (torch._CAST_NAME[A.dtype.name], torch._CAST_NAME[B.dtype.name]))
    drv = "gelsy" if driver is None else driver
    if drv not in ("gels", "gelsy", "gelsd", "gelss"):
        raise RuntimeError("torch.linalg.lstsq: parameter `driver` should be one of (gels, gelsy, gelsd, gelss)")
    vector = _is_vector_rhs(A, B)
    Bm = B.unsqueeze(-1) if vector else B
    m, n = A.shape[-2], A.shape[-1]
    if Bm.shape[-2] != m:
        raise RuntimeError("torch.linalg.lstsq: input.size(-2) should match other.size(-2)")
    A2, B2, batch = _broadcast_batch(A, Bm)
    eps = torch.finfo(A.dtype).eps
    r = eps * _max(m, n) if rcond is None or rcond < 0 else _float(rcond)
    if drv == "gels":
        r = 0.0
    X = torch.matmul(pinv(A2, rtol=r) if r > 0 else pinv(A2, atol=0.0, rtol=0.0), B2)
    with torch.no_grad():
        S = _svd_core(A2.detach(), False, False, "lstsq")[1]
        svals = torch._k.to_list(S._s)
        k = _min(m, n)
        ranks = []
        for b in _range(_count(batch)):
            row = svals[b * k: b * k + k]
            big = row[0] if row else 0.0
            ranks.append(len([s for s in row if s > r * big]))
        if drv != "gelsy" and m > n and all(rk == n for rk in ranks):
            resid = torch.sum((torch.matmul(A2.detach(), X.detach()) - B2.detach()) ** 2, -2)
        else:
            resid = torch.zeros(0, dtype=A.dtype)
        rank_t = torch.zeros(0, dtype=torch.int64) if drv == "gels" else _flat_tensor(ranks, batch, torch.int64)
        sv = S if drv in ("gelsd", "gelss") else torch.zeros(0, dtype=A.dtype)
    if vector:
        X = X.squeeze(-1)
    return _returns("linalg_lstsq", ("solution", "residuals", "rank", "singular_values"), (X, resid, rank_t, sv))


# ---- norms -------------------------------------------------------------------------------
def vector_norm(x, ord=2, dim=None, keepdim=False, *, dtype=None, out=None):
    if isinstance(ord, str):
        raise TypeError("linalg_vector_norm(): argument 'ord' must be Number, not str")
    if dtype is not None:
        x = x.to(dtype)
    if isinstance(x, torch.Tensor) and x.dtype.is_complex:
        # A complex vector's norms are its moduli's.
        x = torch.abs(x)
    _check_float(x, "vector_norm")
    if dim is None:
        dims = tuple(_range(x.ndim))
    elif isinstance(dim, int):
        dims = (dim,)
    else:
        dims = tuple(dim)
    if x.ndim == 0:
        x = x.reshape(1)
        dims = (0,)
        if keepdim:
            return torch.norm(x, ord, dims, False)
    p = _float(ord)
    if p == -_math.inf:
        return torch.amin(torch.abs(x), dims, keepdim)
    if p < 0:
        return torch.pow(torch.sum(torch.pow(torch.abs(x), p), dims, keepdim), 1.0 / p)
    return torch.norm(x, p, dims, keepdim)


def matrix_norm(A, ord="fro", dim=(-2, -1), keepdim=False, *, dtype=None, out=None):
    if dtype is not None:
        A = A.to(dtype)
    if isinstance(A, torch.Tensor) and A.dtype.is_complex and ord in ("fro", 1, -1, _math.inf, -_math.inf):
        # These norms of a complex matrix are its moduli's.
        A = torch.abs(A)
    _check_float(A, "matrix_norm")
    if A.ndim < 2:
        raise RuntimeError("linalg.matrix_norm: The input tensor A must have at least 2 dimensions.")
    dim = tuple(dim)
    if len(dim) != 2:
        raise RuntimeError("linalg.matrix_norm: dim must be a 2-tuple. Got %s" % (list(dim),))
    r = A.ndim
    d0 = dim[0] + r if dim[0] < 0 else dim[0]
    d1 = dim[1] + r if dim[1] < 0 else dim[1]
    if d0 == d1:
        raise RuntimeError("linalg.matrix_norm: dims must be different. Got (%d, %d)" % (dim[0], dim[1]))
    if isinstance(ord, str):
        if ord not in ("fro", "nuc"):
            raise RuntimeError("linalg.matrix_norm: Order %s not supported." % ord)
        if ord == "fro":
            return vector_norm(A, 2, (d0, d1), keepdim)
    elif ord not in (1, -1, 2, -2, _math.inf, -_math.inf):
        raise RuntimeError("linalg.matrix_norm: Order %s not supported." % (ord,))
    if ord == "nuc" or ord in (2, -2):
        moved = torch.movedim(A, (d0, d1), (-2, -1))
        S = svdvals(moved)
        if ord == "nuc":
            res = torch.sum(S, -1)
        elif ord == 2:
            res = torch.amax(S, -1)
        else:
            res = torch.amin(S, -1)
        if keepdim:
            for d in sorted((d0, d1)):
                res = res.unsqueeze(d)
        return res
    if ord in (1, -1):
        sum_dim, red_dim = d0, d1
    else:
        sum_dim, red_dim = d1, d0
    s = torch.sum(torch.abs(A), sum_dim, True)
    res = torch.amax(s, red_dim, True) if ord > 0 else torch.amin(s, red_dim, True)
    if not keepdim:
        shape = [A.shape[i] if i not in (d0, d1) else 1 for i in _range(r)]
        res = res.reshape(*[shape[i] for i in _range(r) if i not in (d0, d1)])
    return res


def norm(A, ord=None, dim=None, keepdim=False, *, dtype=None, out=None):
    if dim is not None and not isinstance(dim, int):
        dim = tuple(dim)
        if len(dim) == 1:
            dim = dim[0]
    if dim is not None:
        if isinstance(dim, int):
            return vector_norm(A, 2 if ord is None else ord, dim, keepdim, dtype=dtype)
        return matrix_norm(A, "fro" if ord is None else ord, dim, keepdim, dtype=dtype)
    if ord is None:
        return vector_norm(A, 2, None, keepdim, dtype=dtype)
    if A.ndim == 2:
        return matrix_norm(A, ord, (-2, -1), keepdim, dtype=dtype)
    if A.ndim == 1:
        return vector_norm(A, ord, None, keepdim, dtype=dtype)
    raise RuntimeError("linalg.norm: If dim is not specified but ord is, the input must be 1D or 2D. Got %dD." % A.ndim)


def cond(A, p=None, *, out=None):
    _check_matrix(A, "cond")
    if p is None or p in (2, -2):
        S = svdvals(A)
        if p == -2:
            return S[..., -1] / S[..., 0]
        return S[..., 0] / S[..., -1]
    return matrix_norm(A, p) * matrix_norm(inv(A), p)


# ---- products, powers, exponential -------------------------------------------------------
def matrix_power(A, n, *, out=None):
    _check_square(A, "matrix_power")
    n = _int(n)
    if n == 0:
        return _eye_like(A.shape[-1], _batch(A), A.dtype).clone()
    if n < 0:
        A = inv(A)
        n = -n
    if n == 1:
        return A.clone()
    result = None
    base = A
    while n:
        if n & 1:
            result = base if result is None else torch.matmul(result, base)
        n >>= 1
        if n:
            base = torch.matmul(base, base)
    return result


def matrix_exp(A):
    """Scaling and squaring with a degree-18 Taylor polynomial (Horner),
    composed from matmuls: the gradient is autograd's through the same
    arithmetic."""
    _check_square(A, "matrix_exp")
    n = A.shape[-1]
    batch = _batch(A)
    norm1 = 0.0
    for M in _mats(A.detach()):
        for j in _range(n):
            norm1 = _max(norm1, _math.fsum([_abs(M[i][j]) for i in _range(n)]))
    s = 0
    while norm1 > 0.5:
        norm1 /= 2.0
        s += 1
    X = A * (0.5 ** s) if s else A
    I = _eye_like(n, batch, A.dtype)
    E = I
    for k in _range(18, 0, -1):
        E = I + torch.matmul(X, E) * (1.0 / k)
    for _ in _range(s):
        E = torch.matmul(E, E)
    return E


def multi_dot(tensors, *, out=None):
    ts = list(tensors)
    if len(ts) < 2:
        raise RuntimeError("multi_dot(): expected at least 2 tensors but got %d" % len(ts))
    first_vec = ts[0].ndim == 1
    last_vec = ts[-1].ndim == 1
    if first_vec:
        ts[0] = ts[0].unsqueeze(0)
    if last_vec:
        ts[-1] = ts[-1].unsqueeze(-1)
    for i, t in enumerate(ts):
        if t.ndim != 2:
            raise RuntimeError("multi_dot(): tensor %d must be 2D but got %dD" % (i, t.ndim))
    for i in _range(len(ts) - 1):
        if ts[i].shape[1] != ts[i + 1].shape[0]:
            raise RuntimeError("multi_dot(): tensors %d and %d with shapes %s and %s cannot be multiplied" % (i, i + 1, list(ts[i].shape), list(ts[i + 1].shape)))
    # The cheapest parenthesization (matrix chain order).
    dims = [ts[0].shape[0]] + [t.shape[1] for t in ts]
    k = len(ts)
    cost = [[0] * k for _ in _range(k)]
    split = [[0] * k for _ in _range(k)]
    for length in _range(1, k):
        for i in _range(k - length):
            j = i + length
            best = None
            for s in _range(i, j):
                c = cost[i][s] + cost[s + 1][j] + dims[i] * dims[s + 1] * dims[j + 1]
                if best is None or c < best:
                    best = c
                    split[i][j] = s
            cost[i][j] = best

    def chain(i, j):
        if i == j:
            return ts[i]
        s = split[i][j]
        return torch.matmul(chain(i, s), chain(s + 1, j))
    res = chain(0, k - 1)
    if first_vec:
        res = res.squeeze(0)
    if last_vec:
        res = res.squeeze(-1)
    return res


def vecdot(x, y, *, dim=-1, out=None):
    return torch.sum(x * y, dim)


def cross(input, other, *, dim=-1, out=None):
    if input.ndim != other.ndim:
        raise RuntimeError("linalg.cross: inputs must have the same number of dimensions.")
    r = _max(input.ndim, other.ndim)
    d = dim + r if dim < 0 else dim
    a = input.shape[d - (r - input.ndim)] if d - (r - input.ndim) >= 0 else 1
    b = other.shape[d - (r - other.ndim)] if d - (r - other.ndim) >= 0 else 1
    if a != 3 or b != 3:
        raise RuntimeError("linalg.cross: inputs dimension %d must have length 3. Got %d and %d" % (dim, a, b))
    return torch.cross(input, other, dim)


def diagonal(A, *, offset=0, dim1=-2, dim2=-1):
    return torch.diagonal(A, offset, dim1, dim2)


def vander(x, *, N=None):
    n = x.shape[-1] if N is None else _int(N)
    if n <= 1 and N is not None and n < 1:
        raise RuntimeError("N must be greater than 1.")
    cols = [torch.ones_like(x)]
    for _ in _range(1, n):
        cols.append(cols[-1] * x)
    return torch.stack(cols, -1)


def tensorinv(A, ind=2, *, out=None):
    shape = tuple(A.shape)
    left = _count(shape[:ind])
    right = _count(shape[ind:])
    if left != right:
        raise RuntimeError("Expected self to satisfy the requirement prod(self.shape[ind:]) == prod(self.shape[:ind]), but got %d != %d" % (right, left))
    return inv(A.reshape(left, right)).reshape(*(shape[ind:] + shape[:ind]))


def tensorsolve(A, B, dims=None, *, out=None):
    if dims is not None:
        dims = [d + A.ndim if d < 0 else d for d in dims]
        rest = [d for d in _range(A.ndim) if d not in dims]
        A = A.permute(*(rest + dims))
    rshape = tuple(A.shape[B.ndim:])
    n = _count(rshape)
    X = solve(A.reshape(-1, n), B.reshape(-1))
    return X.reshape(*rshape)


def matmul(input, other, *, out=None):
    return torch.matmul(input, other)


# ---- top-level torch aliases -------------------------------------------------------------
def _logdet(A):
    s, la = slogdet(A)
    return torch.where(s < 0, torch.full_like(la, _math.nan), la)


def _torch_svd(A, some=True, compute_uv=True):
    if not compute_uv:
        S = svdvals(A)
        m, n = A.shape[-2], A.shape[-1]
        U = torch.zeros(*(_batch(A) + (m, m)), dtype=A.dtype)
        V = torch.zeros(*(_batch(A) + (n, n)), dtype=A.dtype)
        return _returns("svd", ("U", "S", "V"), (U, S, V))
    U, S, Vh = svd(A, full_matrices=not some)
    return _returns("svd", ("U", "S", "V"), (U, S, _mT(Vh)))


def _torch_qr(A, some=True):
    Q, R = qr(A, "reduced" if some else "complete")
    return _returns("qr", ("Q", "R"), (Q, R))


def _torch_slogdet(A):
    s, la = slogdet(A)
    return _returns("slogdet", ("sign", "logabsdet"), (s, la))


def _torch_cholesky(A, upper=False):
    return cholesky(A, upper=upper)


def _cholesky_solve(B, L, upper=False):
    """X = (L L^T)^{-1} B, with PyTorch's gradient for L (not restricted to
    its triangle): -(G X^T + X G^T) L, where G = (L L^T)^{-1} g."""
    _check_matrix(L, "cholesky_solve", "L")
    _check_matrix(B, "cholesky_solve", "B")
    Lm, Bm, batch = _broadcast_batch(L, B)
    with torch.no_grad():
        low = _mT(Lm) if upper else Lm
        Y = solve_triangular(low, Bm, upper=False)
        X = solve_triangular(_mT(low), Y, upper=True)
    if _needs(Lm, Bm):
        def backward(g):
            G = _cholesky_solve(g, Lm, upper)
            gL = None
            if Lm.requires_grad:
                common = torch.matmul(G, _mT(X))
                common = common + _mT(common)
                gL = -torch.matmul(Lm, common) if upper else -torch.matmul(common, Lm)
            return (gL, G if Bm.requires_grad else None)
        _attach(X, (Lm, Bm), backward, "CholeskySolve", (Lm, X))
    return X


def _cholesky_inverse(L, upper=False):
    n = L.shape[-1]
    return _cholesky_solve(_eye_like(n, _batch(L), L.dtype), L, upper)


def _pinverse(A, rcond=1e-15):
    return pinv(A, rtol=rcond)


def _install():
    aliases = {
        "det": det, "logdet": _logdet, "slogdet": _torch_slogdet, "inverse": inv,
        "cholesky": _torch_cholesky, "cholesky_solve": _cholesky_solve,
        "cholesky_inverse": _cholesky_inverse, "qr": _torch_qr, "svd": _torch_svd,
        "pinverse": _pinverse, "matrix_power": matrix_power, "matrix_exp": matrix_exp,
    }
    for name, fn in aliases.items():
        if not hasattr(torch, name):
            setattr(torch, name, fn)
        if not hasattr(torch.Tensor, name):
            setattr(torch.Tensor, name, fn)


_install()
