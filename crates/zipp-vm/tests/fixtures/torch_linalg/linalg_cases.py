# torch.linalg parity cases: every function's forward values and, where
# PyTorch differentiates it, the gradient of a scalar loss with respect to
# each input. Runs unchanged under PyTorch 2.11 (gen.py writes
# linalg_expected.json from it) and under Zipp (python_torch_linalg.rs).
#
# Inputs come from fixed formulas, not a random stream. Eigenvector and
# singular-vector signs are not unique, so those cases compare
# sign-invariant quantities (|V|, V**2-weighted losses, reconstructions);
# QR, Cholesky and the LU family are unique and compare directly.
import math

import torch
import torch.linalg as LA

D = torch.float64


def mat(m, n, seed, dtype=D, batch=()):
    count = 1
    for b in batch:
        count *= b
    vals = []
    for b in range(count):
        for i in range(m):
            for j in range(n):
                vals.append(math.sin(1.3 * i + 0.7 * j + seed + 2.1 * b) + (0.9 if i == j else 0.0))
    return torch.tensor(vals, dtype=dtype).reshape(*(tuple(batch) + (m, n)))


def spd(n, seed, dtype=D, batch=()):
    a = mat(n, n, seed, dtype, batch)
    return a @ a.transpose(-2, -1) + n * torch.eye(n, dtype=dtype)


def sym(n, seed, dtype=D, batch=()):
    a = mat(n, n, seed, dtype, batch)
    return a + a.transpose(-2, -1)


def weights(shape, seed, dtype=D):
    n = 1
    for s in shape:
        n *= s
    return torch.tensor([math.cos(0.37 * k + seed) for k in range(n)], dtype=dtype).reshape(*tuple(shape))


def flat(x):
    if isinstance(x, (tuple, list)):
        out = []
        for t in x:
            out.extend(flat(t))
        return out
    if isinstance(x, torch.Tensor):
        vals = x.detach().reshape(-1).tolist()
    else:
        vals = [x]
    out = []
    for v in vals:
        v = float(v)
        if v != v:
            v = 1234.5
        elif v == math.inf:
            v = 9999.0
        elif v == -math.inf:
            v = -9999.0
        out.append(v)
    return out


def real(x):
    return x.real if x.is_complex() else x


def grads(loss_fn, *inputs):
    xs = [x.detach().clone().requires_grad_(True) for x in inputs]
    loss = loss_fn(*xs)
    gs = torch.autograd.grad(loss, xs)
    return [float(loss.detach())] + flat(list(gs))


def wsum(t, seed):
    return (t * weights(t.shape, seed, t.dtype)).sum()


def results():
    R = {}
    A3 = mat(3, 3, 0.3)
    B3 = mat(3, 3, 1.1, batch=(2,))
    A4 = mat(4, 4, 2.0)
    tall = mat(5, 3, 0.8)
    wide = mat(3, 5, 1.7)

    # det / slogdet / logdet
    R["det"] = flat([LA.det(A3), LA.det(B3), LA.det(A4)])
    R["det_grad"] = grads(lambda a: wsum(LA.det(a), 1), B3)
    sing = torch.tensor([[1.0, 2.0], [2.0, 4.0]], dtype=D)
    R["det_singular_grad"] = grads(lambda a: LA.det(a), sing)
    s, la = LA.slogdet(B3)
    R["slogdet"] = flat([s, la, LA.slogdet(sing)])
    R["slogdet_grad"] = grads(lambda a: wsum(LA.slogdet(a).logabsdet, 2), B3)
    neg = torch.tensor([[0.0, 1.0], [1.0, 0.0]], dtype=D)
    R["logdet"] = flat([torch.logdet(spd(3, 0.5)), torch.logdet(neg), torch.logdet(sing)])
    R["logdet_grad"] = grads(lambda a: torch.logdet(a), spd(3, 0.5))

    # inv / inv_ex
    R["inv"] = flat([LA.inv(A3), LA.inv(B3), torch.inverse(A4), A3.inverse()])
    R["inv_grad"] = grads(lambda a: wsum(LA.inv(a), 3), B3)
    r = LA.inv_ex(B3)
    R["inv_ex"] = flat([r.inverse, r.info]) + [float(r.info.dtype == torch.int32), float(LA.inv_ex(sing).info)]

    # solve
    Bm = mat(3, 2, 0.4)
    Bb = mat(3, 2, 0.9, batch=(2,))
    v3 = torch.tensor([1.0, -2.0, 0.5], dtype=D)
    R["solve"] = flat([LA.solve(A3, Bm), LA.solve(B3, Bm), LA.solve(A3, Bb), LA.solve(A3, v3), LA.solve(B3, mat(2, 3, 0.2).reshape(2, 3))])
    R["solve_left_false"] = flat([LA.solve(A3, mat(2, 3, 0.6), left=False), LA.solve(B3, mat(4, 3, 0.1), left=False)])
    R["solve_grad"] = grads(lambda a, b: wsum(LA.solve(a, b), 4), B3, Bm)
    R["solve_grad_vec"] = grads(lambda a, b: wsum(LA.solve(a, b), 5), A3, v3)
    R["solve_grad_right"] = grads(lambda a, b: wsum(LA.solve(a, b, left=False), 6), B3, mat(4, 3, 0.6))
    se = LA.solve_ex(B3, Bm)
    R["solve_ex"] = flat([se.result, se.info])

    # solve_triangular
    U = torch.triu(A4) + 2 * torch.eye(4, dtype=D)
    L = torch.tril(A4) + 2 * torch.eye(4, dtype=D)
    B42 = mat(4, 2, 0.5)
    B24 = mat(2, 4, 0.5)
    R["solve_triangular"] = flat([
        LA.solve_triangular(U, B42, upper=True),
        LA.solve_triangular(L, B42, upper=False),
        LA.solve_triangular(L, B42, upper=False, unitriangular=True),
        LA.solve_triangular(U, B24, upper=True, left=False),
        LA.solve_triangular(A4, B42, upper=True),
    ])
    R["solve_triangular_grad"] = grads(lambda a, b: wsum(LA.solve_triangular(a, b, upper=True), 7), U, B42)
    R["solve_triangular_grad_lower_unit"] = grads(lambda a, b: wsum(LA.solve_triangular(a, b, upper=False, unitriangular=True), 8), L, B42)
    R["solve_triangular_grad_right"] = grads(lambda a, b: wsum(LA.solve_triangular(a, b, upper=False, left=False), 9), L, B24)

    # cholesky
    P3 = spd(3, 0.2)
    Pb = spd(3, 0.7, batch=(2,))
    R["cholesky"] = flat([LA.cholesky(P3), LA.cholesky(Pb), LA.cholesky(P3, upper=True), torch.cholesky(P3), torch.cholesky(P3, upper=True)])
    R["cholesky_grad"] = grads(lambda a: wsum(LA.cholesky(a), 10), Pb)
    R["cholesky_grad_upper"] = grads(lambda a: wsum(LA.cholesky(a, upper=True), 11), P3)
    notpd = torch.tensor([[1.0, 2.0], [2.0, 1.0]], dtype=D)
    ce = LA.cholesky_ex(torch.stack([torch.eye(2, dtype=D), notpd]))
    R["cholesky_ex"] = flat([ce.info, ce.L[0]]) + [float(ce.info.dtype == torch.int32)]
    R["cholesky_solve"] = flat([torch.cholesky_solve(Bm, LA.cholesky(P3)), torch.cholesky_solve(Bm, LA.cholesky(P3, upper=True), upper=True), torch.cholesky_inverse(LA.cholesky(P3))])
    R["cholesky_solve_grad"] = grads(lambda l, b: wsum(torch.cholesky_solve(b, l), 12), LA.cholesky(P3), Bm)
    R["cholesky_solve_grad_upper"] = grads(lambda l, b: wsum(torch.cholesky_solve(b, l, upper=True), 54) + wsum(torch.cholesky_inverse(l, upper=True), 55), LA.cholesky(P3, upper=True), mat(3, 2, 0.8, batch=(2,)))

    # qr
    for name, M in (("tall", tall), ("wide", wide), ("square", A4)):
        q, r = LA.qr(M)
        qc, rc = LA.qr(M, mode="complete")
        qr_ = LA.qr(M, mode="r")
        R["qr_" + name] = flat([q, r, qc, rc, qr_.R]) + [float(qr_.Q.numel())]
    R["qr_batch"] = flat(list(LA.qr(B3)))
    R["qr_grad_tall"] = grads(lambda a: wsum(LA.qr(a).Q, 13) + wsum(LA.qr(a).R, 14), tall)
    R["qr_grad_wide"] = grads(lambda a: wsum(LA.qr(a).Q, 15) + wsum(LA.qr(a).R, 16), wide)
    R["qr_grad_square_complete"] = grads(lambda a: wsum(LA.qr(a, mode="complete").Q, 17) + wsum(LA.qr(a, mode="complete").R, 18), A4)
    R["torch_qr"] = flat(list(torch.qr(tall))) + flat(list(torch.qr(tall, some=False)))

    # eigh / eigvalsh
    S4 = sym(4, 0.3)
    Sb = sym(3, 1.4, batch=(2,))
    w, V = LA.eigh(S4)
    wb, Vb = LA.eigh(Sb)
    R["eigh"] = flat([w, V.abs(), wb, Vb.abs(), V @ torch.diag(w) @ V.T, LA.eigh(S4, UPLO="U").eigenvalues, LA.eigvalsh(Sb)])
    R["eigh_uplo"] = flat([LA.eigvalsh(torch.triu(S4) + 5 * torch.tril(S4, -1), UPLO="U"), LA.eigvalsh(torch.triu(S4) + 5 * torch.tril(S4, -1))])
    R["eigh_grad"] = grads(lambda a: wsum(LA.eigh(a).eigenvalues, 19) + wsum(LA.eigh(a).eigenvectors ** 2, 20), S4)
    R["eigh_grad_batch"] = grads(lambda a: wsum(LA.eigh(a).eigenvectors ** 2, 21), Sb)
    R["eigvalsh_grad"] = grads(lambda a: wsum(LA.eigvalsh(a), 22), Sb)

    # svd / svdvals
    for name, M in (("tall", tall), ("wide", wide), ("square", A4)):
        U_, S_, Vh_ = LA.svd(M, full_matrices=False)
        Uf, Sf, Vhf = LA.svd(M)
        k = S_.shape[-1]
        R["svd_" + name] = flat([S_, U_.abs(), Vh_.abs(), U_ @ torch.diag(S_) @ Vh_, Sf, Uf[:, :k].abs(), Vhf[:k].abs(),
                                 Uf.T @ Uf, Vhf @ Vhf.T, LA.svdvals(M)]) + [float(Uf.shape[0]), float(Uf.shape[1]), float(Vhf.shape[0])]
        R["svd_grad_" + name] = grads(lambda a: wsum(LA.svd(a, full_matrices=False).S, 23) + wsum(LA.svd(a, full_matrices=False).U ** 2, 24) + wsum(LA.svd(a, full_matrices=False).Vh ** 2, 25), M)
        R["svdvals_grad_" + name] = grads(lambda a: wsum(LA.svdvals(a), 26), M)
    # The completing columns of a full U are any orthonormal basis; the
    # squared row norms over them are not, and their gradient is dropped
    # (PyTorch narrows gU to the first min(m, n) columns).
    R["svd_grad_full"] = grads(lambda a: wsum(LA.svd(a).U[:, :3] ** 2, 27) + wsum((LA.svd(a).U[:, 3:] ** 2).sum(1), 53) + wsum(LA.svd(a).S, 28), tall)
    R["svd_batch"] = flat([LA.svdvals(B3), LA.svd(B3).S])
    U_, S_, V_ = torch.svd(tall)
    R["torch_svd"] = flat([S_, U_.abs(), V_.abs(), U_ @ torch.diag(S_) @ V_.T]) + [float(V_.shape[0]), float(V_.shape[1])]
    U_, S_, V_ = torch.svd(tall, some=False)
    R["torch_svd_full"] = flat([S_]) + [float(U_.shape[0]), float(U_.shape[1]), float(V_.shape[0])]
    U_, S_, V_ = torch.svd(tall, compute_uv=False)
    R["torch_svd_nouv"] = flat([S_, U_, V_])

    # pinv / matrix_rank
    rankdef = torch.tensor([[1.0, 2.0, 3.0], [2.0, 4.0, 6.0], [1.0, 0.0, 1.0], [0.0, 1.0, 1.0]], dtype=D)
    R["pinv"] = flat([LA.pinv(tall), LA.pinv(wide), LA.pinv(A4), LA.pinv(rankdef), LA.pinv(rankdef, rtol=0.3), LA.pinv(rankdef, atol=2.0),
                      LA.pinv(S4, hermitian=True), LA.pinv(B3), torch.pinverse(tall)])
    R["pinv_grad_tall"] = grads(lambda a: wsum(LA.pinv(a), 29), tall)
    R["pinv_grad_wide"] = grads(lambda a: wsum(LA.pinv(a), 30), wide)
    R["pinv_grad_hermitian"] = grads(lambda a: wsum(LA.pinv(a, hermitian=True), 31), S4)
    R["matrix_rank"] = flat([LA.matrix_rank(tall), LA.matrix_rank(rankdef), LA.matrix_rank(rankdef, atol=1.0), LA.matrix_rank(rankdef, rtol=0.2),
                             LA.matrix_rank(torch.ones(3, 3, dtype=D)), LA.matrix_rank(torch.ones(3, 3, dtype=D), hermitian=True),
                             LA.matrix_rank(B3), LA.matrix_rank(torch.zeros(2, 2, dtype=D)), LA.matrix_rank(S4, hermitian=True, atol=3.0)])

    # lstsq
    Bt = mat(5, 2, 0.35)
    for drv in (None, "gels", "gelsy", "gelsd", "gelss"):
        out = LA.lstsq(tall, Bt, driver=drv)
        R["lstsq_%s" % drv] = flat([out.solution, out.residuals, out.rank, out.singular_values]) + [float(out.residuals.numel()), float(out.rank.numel()), float(out.singular_values.numel())]
    out = LA.lstsq(wide, mat(3, 2, 0.1), driver="gelsd")
    R["lstsq_wide"] = flat([out.solution, out.rank, out.singular_values]) + [float(out.residuals.numel())]
    out = LA.lstsq(rankdef, mat(4, 1, 0.9), driver="gelsd")
    R["lstsq_rankdef"] = flat([out.solution, out.rank, out.singular_values]) + [float(out.residuals.numel())]
    out = LA.lstsq(mat(4, 3, 0.2, batch=(2,)), mat(4, 2, 0.3, batch=(2,)), driver="gelss")
    R["lstsq_batch"] = flat([out.solution, out.residuals, out.rank, out.singular_values])
    out = LA.lstsq(tall, mat(5, 1, 0.3).reshape(5))
    R["lstsq_vector"] = flat([out.solution]) + [float(out.solution.ndim)]
    R["lstsq_grad"] = grads(lambda a, b: wsum(LA.lstsq(a, b).solution, 32), tall, Bt)

    # norms
    x = torch.tensor([[0.5, -2.0, 0.0], [3.0, 1.0, -1.5]], dtype=D)
    vn = []
    for o in (2, 1, math.inf, -math.inf, 0, 3, -1, 0.5):
        vn += [LA.vector_norm(x, o), LA.vector_norm(x, o, dim=1), LA.vector_norm(x, o, dim=0, keepdim=True)]
    vn += [LA.vector_norm(x, dim=(0, 1)), LA.vector_norm(x, keepdim=True), LA.vector_norm(x.float(), dtype=D), LA.vector_norm(torch.tensor(-3.0, dtype=D))]
    R["vector_norm"] = flat(vn) + [float(LA.vector_norm(x, keepdim=True).ndim)]
    for o in (2, 1, math.inf, -math.inf, 3, -1):
        R["vector_norm_grad_%s" % o] = grads(lambda a: wsum(LA.vector_norm(a, o, dim=1), 33), x + 0.25)
    mn = []
    T3 = mat(3, 4, 0.6, batch=(2,))
    for o in ("fro", "nuc", 2, -2, 1, -1, math.inf, -math.inf):
        mn += [LA.matrix_norm(A4, o), LA.matrix_norm(T3, o), LA.matrix_norm(T3, o, dim=(0, 2)), LA.matrix_norm(T3, o, dim=(2, 1), keepdim=True)]
        R["matrix_norm_grad_%s" % o] = grads(lambda a: wsum(LA.matrix_norm(a, o), 34) + wsum(LA.matrix_norm(a, o, dim=(0, 2)), 35), T3)
    R["matrix_norm"] = flat(mn) + [float(LA.matrix_norm(T3, "nuc", dim=(2, 1), keepdim=True).ndim)]
    R["norm"] = flat([LA.norm(x), LA.norm(x, 1), LA.norm(x, "fro"), LA.norm(x, "nuc"), LA.norm(x, math.inf), LA.norm(x[0], math.inf),
                      LA.norm(T3), LA.norm(T3, dim=1), LA.norm(T3, 2, dim=(1, 2)), LA.norm(T3, 3, dim=(2,), keepdim=True)])
    R["cond"] = flat([LA.cond(A4), LA.cond(tall), LA.cond(A4, "fro"), LA.cond(A4, 1), LA.cond(A4, -2)])

    # matrix_power / matrix_exp
    R["matrix_power"] = flat([LA.matrix_power(A3, 0), LA.matrix_power(A3, 1), LA.matrix_power(A3, 3), LA.matrix_power(A3, -2), LA.matrix_power(B3, 5), torch.matrix_power(A3, 2), A3.matrix_power(4)])
    R["matrix_power_grad"] = grads(lambda a: wsum(LA.matrix_power(a, 3), 36) + wsum(LA.matrix_power(a, -2), 37), B3)
    E1 = mat(3, 3, 0.1) * 0.3
    E2 = mat(4, 4, 2.2) * 2.5
    R["matrix_exp"] = flat([LA.matrix_exp(E1), LA.matrix_exp(E2), LA.matrix_exp(B3), torch.matrix_exp(E1), LA.matrix_exp(torch.zeros(2, 2, dtype=D))])
    R["matrix_exp_grad"] = grads(lambda a: wsum(LA.matrix_exp(a), 38), E2)
    R["matrix_exp_grad_batch"] = grads(lambda a: wsum(LA.matrix_exp(a), 39), B3)

    # cross / multi_dot / vecdot / diagonal / vander / householder_product
    a = mat(2, 3, 0.1)
    b = mat(2, 3, 0.9)
    R["cross"] = flat([LA.cross(a, b), LA.cross(a, b[0:1]), LA.cross(a.T, b.T, dim=0)])
    R["cross_grad"] = grads(lambda p, q: wsum(LA.cross(p, q), 40), a, b)
    m1, m2, m3 = mat(2, 4, 0.1), mat(4, 3, 0.2), mat(3, 5, 0.3)
    v4 = torch.tensor([1.0, 0.5, -1.0, 2.0], dtype=D)
    v5 = torch.tensor([0.2, -0.3, 1.0, 0.0, 2.0], dtype=D)
    R["multi_dot"] = flat([LA.multi_dot([m1, m2, m3]), LA.multi_dot([v4, m2, m3, v5]), LA.multi_dot([m1, m2]), LA.multi_dot((v4, m2, m3))])
    R["multi_dot_grad"] = grads(lambda p, q, r: wsum(LA.multi_dot([p, q, r]), 41), m1, m2, m3)
    R["vecdot"] = flat([LA.vecdot(a, b), LA.vecdot(a, b, dim=0), LA.vecdot(a, b[1])])
    R["vecdot_grad"] = grads(lambda p, q: wsum(LA.vecdot(p, q), 42), a, b)
    R["diagonal"] = flat([LA.diagonal(A4), LA.diagonal(B3, offset=1), LA.diagonal(T3, offset=-1, dim1=1, dim2=2), LA.diagonal(T3, dim1=0, dim2=2)])
    R["diagonal_grad"] = grads(lambda t: wsum(LA.diagonal(t, offset=1), 43), T3)
    R["vander"] = flat([LA.vander(torch.tensor([1.0, 2.0, -0.5], dtype=D)), LA.vander(torch.tensor([[1.0, 2.0], [3.0, 0.5]], dtype=D), N=4)])
    H = mat(5, 3, 0.45)
    tau = torch.tensor([1.2, 0.4, 1.7], dtype=D)
    R["householder_product"] = flat([LA.householder_product(H, tau), LA.householder_product(H, tau[:2])])
    R["householder_product_grad"] = grads(lambda h, t: wsum(LA.householder_product(h, t), 44), H, tau)
    g_ = torch.geqrf(tall) if hasattr(torch, "geqrf") else None
    if g_ is not None:
        R["householder_geqrf"] = flat([LA.householder_product(g_[0], g_[1]).abs()])
    else:
        R["householder_geqrf"] = flat([LA.qr(tall).Q.abs()])

    # eig / eigvals on real spectra (sorted; vectors compared up to sign)
    Rm = torch.tensor([[4.0, 1.0, 2.0], [0.5, 3.0, 1.0], [0.2, 0.4, 1.0]], dtype=D)
    ev, evec = LA.eig(Rm)
    ev, evec = real(ev), real(evec)
    order = torch.argsort(ev)
    R["eig"] = flat([ev[order], evec[:, order].abs(), torch.sort(real(LA.eigvals(Rm))).values, (Rm @ evec - evec * ev).abs().amax() < 1e-10])

    def eig_loss(a):
        lam, vec = LA.eig(a)
        lam, vec = real(lam), real(vec)
        return (lam ** 2).sum() + ((vec ** 2) * lam.unsqueeze(-2)).sum()
    R["eig_grad"] = grads(eig_loss, Rm)
    R["eigvals_grad"] = grads(lambda a: (real(LA.eigvals(a)) ** 3).sum(), Rm)

    # LU (forward only on Zipp): partial pivoting makes P, L, U unique
    P_, L_, U_ = LA.lu(A4)
    Pw, Lw, Uw = LA.lu(wide)
    Pt, Lt, Ut = LA.lu(tall)
    LUf, piv = LA.lu_factor(B3)
    lfe = LA.lu_factor_ex(sing)
    R["lu"] = flat([P_, L_, U_, Pw, Lw, Uw, Pt, Lt, Ut, LUf, piv, LA.lu_solve(LUf, piv, Bb), LA.lu_solve(LUf, piv, mat(2, 3, 0.1), left=False),
                    LA.lu_solve(LUf, piv, Bb, adjoint=True), lfe.info, LA.solve_ex(sing, torch.ones(2, 1, dtype=D)).info])
    # LU gradients (PyTorch's linalg_lu_backward, lu_factor_ex_backward and
    # lu_solve's)
    R["lu_grad"] = grads(lambda a: wsum(LA.lu(a).L, 60) + wsum(LA.lu(a).U, 61), A4)
    R["lu_grad_wide"] = grads(lambda a: wsum(LA.lu(a).L, 62) + wsum(LA.lu(a).U, 63), wide)
    R["lu_grad_tall"] = grads(lambda a: wsum(LA.lu(a).L, 64) + wsum(LA.lu(a).U, 65), tall)
    R["lu_grad_batch"] = grads(lambda a: wsum(LA.lu(a).U, 66), B3)
    R["lu_factor_grad"] = grads(lambda a: wsum(LA.lu_factor(a).LU, 67), B3)
    R["lu_factor_grad_rect"] = grads(lambda a, b: wsum(LA.lu_factor(a).LU, 68) + wsum(LA.lu_factor(b).LU, 69), wide, tall)
    R["lu_solve_grad"] = grads(lambda f, b: wsum(LA.lu_solve(f, piv, b), 70) + wsum(LA.lu_solve(f, piv, b, adjoint=True), 71), LUf, Bb)
    R["lu_solve_grad_right"] = grads(lambda f, b: wsum(LA.lu_solve(f, piv, b, left=False), 72), LUf, mat(2, 3, 0.1))

    # eig / eigvals with complex spectra (PyTorch returns complex tensors):
    # eigenvalues sorted by (real, imag) and |V| in that order (the order
    # and each eigenvector's phase are not unique), reconstructions, and
    # the gradients of losses that depend on neither.
    def eig_sorted(a):
        lam, vec = LA.eig(a)
        lr, li = lam.real.reshape(-1).tolist(), lam.imag.reshape(-1).tolist()
        order = sorted(range(len(lr)), key=lambda i: (round(lr[i], 9), round(li[i], 9)))
        idx = torch.tensor(order)
        return [torch.view_as_real(lam)[idx], vec.abs()[:, idx], (a.to(lam.dtype) @ vec - vec * lam).abs().amax() < 1e-9]
    Cm = mat(4, 4, 0.5) - mat(4, 4, 1.3).T
    rot = torch.tensor([[0.0, -1.0], [1.0, 0.0]], dtype=D)
    C5 = mat(5, 5, 2.4) - 1.5 * mat(5, 5, 0.1).T
    M4 = mat(4, 4, 0.3) + 0.5 * mat(4, 4, 1.9).T
    R["eig_complex"] = flat(eig_sorted(Cm) + eig_sorted(rot) + eig_sorted(C5) + eig_sorted(M4))
    R["eig_complex_dtypes"] = [float(LA.eig(Cm).eigenvalues.dtype == torch.complex128), float(LA.eig(Cm.float()).eigenvectors.dtype == torch.complex64),
                               float(LA.eigvals(Rm).dtype == torch.complex128), float(LA.eigvals(Cm.float()).dtype == torch.complex64)]
    R["f32_eig_complex"] = flat(eig_sorted(Cm.float())[:2])

    def eig_inv_loss(a, seed):
        lam, vec = LA.eig(a)
        rows = weights((a.shape[-1], 1), seed)
        return ((lam * lam).real.sum() + (lam.abs() ** 3).sum() + (vec.abs() ** 2 * rows * lam.real.unsqueeze(-2)).sum()
                + (vec.abs() ** 2 * rows * lam.imag.abs().unsqueeze(-2)).sum())
    R["eig_complex_grad"] = grads(lambda a: eig_inv_loss(a, 73), Cm)
    R["eig_complex_grad5"] = grads(lambda a: eig_inv_loss(a, 74), C5)
    R["eig_complex_grad_real"] = grads(lambda a: eig_inv_loss(a, 75), C5)
    R["eig_complex_grad4"] = grads(lambda a: eig_inv_loss(a, 77), M4)
    R["eigvals_complex_grad"] = grads(lambda a: (LA.eigvals(a) ** 3).real.sum() + LA.eigvals(a).abs().sum(), Cm)
    R["eig_complex_grad_batch"] = grads(lambda a: eig_inv_loss(a, 76), torch.stack([Cm, M4]))

    # tensorinv / tensorsolve / matmul
    T4 = mat(4, 6, 0.3).reshape(4, 6)[:, :4].reshape(2, 2, 4) + torch.eye(4, dtype=D).reshape(2, 2, 4)
    R["tensorinv"] = flat([LA.tensorinv(T4.reshape(4, 2, 2), ind=1)])
    R["tensorsolve"] = flat([LA.tensorsolve(T4.reshape(2, 2, 2, 2), mat(2, 2, 0.5))])
    R["linalg_matmul"] = flat([LA.matmul(A3, Bm)])

    # top-level aliases and tensor methods
    R["aliases"] = flat([torch.det(B3), B3.det(), torch.slogdet(A3), A3.slogdet(), A3.logdet(), torch.inverse(B3), P3.cholesky(),
                         torch.pinverse(A4), A4.pinverse(), A3.matrix_exp()])
    R["alias_grad"] = grads(lambda a: torch.det(a) + wsum(torch.inverse(a), 45) + torch.slogdet(a)[1], A3)

    # float32 (Zipp rounds a double-precision result once)
    F3 = mat(3, 3, 0.3, dtype=torch.float32)
    FP = spd(3, 0.2, dtype=torch.float32)
    FS = sym(3, 0.3, dtype=torch.float32)
    Ft = mat(4, 3, 0.8, dtype=torch.float32)
    R["f32_forward"] = flat([LA.det(F3), LA.inv(F3), LA.solve(F3, Ft[:3]), LA.cholesky(FP), LA.qr(Ft).R, LA.eigvalsh(FS), LA.svdvals(Ft),
                             LA.pinv(Ft), LA.matrix_norm(F3, "nuc"), LA.matrix_exp(F3 * 0.3), LA.slogdet(F3).logabsdet])
    R["f32_dtypes"] = [float(t.dtype == torch.float32) for t in (LA.det(F3), LA.inv(F3), LA.eigh(FS).eigenvectors, LA.svd(Ft).U, LA.qr(Ft).Q, LA.pinv(Ft))]
    R["f32_grad"] = grads(lambda a: wsum(LA.inv(a), 46) + LA.det(a) + wsum(LA.svdvals(a), 47), F3)

    # second order (create_graph) through inv, det, solve and cholesky
    def second(fn, x):
        x = x.detach().clone().requires_grad_(True)
        g, = torch.autograd.grad(fn(x), x, create_graph=True)
        h, = torch.autograd.grad(wsum(g, 48), x)
        return flat([g, h])
    R["second_inv"] = second(lambda a: wsum(LA.inv(a), 49), A3)
    R["second_det"] = second(lambda a: LA.det(a), A3)
    R["second_solve"] = second(lambda a: wsum(LA.solve(a, Bm), 50), A3)
    R["second_cholesky"] = second(lambda a: wsum(LA.cholesky(a), 51), P3)
    R["second_eigh"] = second(lambda a: wsum(LA.eigvalsh(a), 52), S4)
    return R


def errors():
    """(label, exception class name family, message) for the error paths."""
    lines = []

    def attempt(label, fn):
        try:
            fn()
            lines.append("%s: no error" % label)
        except Exception as e:
            kind = "LinAlgError" if isinstance(e, LA.LinAlgError) else type(e).__name__
            lines.append("%s: %s %s %s" % (label, kind, isinstance(e, RuntimeError), str(e).split("\n")[0]))
    sing = torch.tensor([[1.0, 2.0], [2.0, 4.0]])
    notpd = torch.tensor([[1.0, 2.0], [2.0, 1.0]])
    attempt("inv singular", lambda: LA.inv(sing))
    attempt("inv batch", lambda: LA.inv(torch.stack([torch.eye(2), sing])))
    attempt("cholesky", lambda: LA.cholesky(notpd))
    attempt("cholesky batch", lambda: LA.cholesky(torch.stack([torch.eye(2), notpd])))
    attempt("cholesky_ex check", lambda: LA.cholesky_ex(notpd, check_errors=True))
    attempt("solve singular", lambda: LA.solve(sing, torch.ones(2)))
    attempt("solve shapes", lambda: LA.solve(torch.eye(2), torch.ones(3)))
    attempt("inv rect", lambda: LA.inv(torch.ones(2, 3)))
    attempt("inv 1d", lambda: LA.inv(torch.ones(3)))
    attempt("inv long", lambda: LA.inv(torch.ones(2, 2, dtype=torch.long)))
    attempt("det rect", lambda: LA.det(torch.ones(2, 3)))
    attempt("eigh rect", lambda: LA.eigh(torch.ones(2, 3)))
    attempt("norm 3d ord", lambda: LA.norm(torch.ones(2, 2, 2), ord=2))
    attempt("matrix_norm ord", lambda: LA.matrix_norm(torch.ones(2, 2), ord=3))
    attempt("vector_norm str", lambda: LA.vector_norm(torch.ones(2, 2), ord="fro"))
    attempt("cross", lambda: LA.cross(torch.ones(3), torch.ones(4)))
    attempt("multi_dot", lambda: LA.multi_dot([torch.ones(2)]))
    attempt("lstsq driver", lambda: LA.lstsq(torch.ones(3, 2), torch.ones(3, 1), driver="bad"))
    attempt("qr mode", lambda: LA.qr(torch.ones(2, 2), mode="bad"))
    attempt("matrix_power singular", lambda: LA.matrix_power(sing, -1))
    attempt("cross ndim", lambda: LA.cross(torch.ones(2, 3), torch.ones(3)))

    def inplace(fn, write):
        base = torch.tensor([[2.0, 1.0], [0.5, 3.0]], requires_grad=True)
        x = base * 1.0
        y = fn(x)
        write(x, y)
        try:
            y.sum().backward()
            return "no error"
        except RuntimeError as e:
            return "inplace error" if "modified by an inplace operation" in str(e) else str(e)
    lines.append("inv output write: " + inplace(LA.inv, lambda x, y: y.add_(1.0)))
    lines.append("det input write: " + inplace(LA.det, lambda x, y: x.mul_(2.0)))
    lines.append("solve input write: " + inplace(lambda a: LA.solve(a, torch.ones(2, 1)), lambda x, y: x.mul_(2.0)))
    lines.append("cholesky output write: " + inplace(lambda a: LA.cholesky(a @ a.T), lambda x, y: y.mul_(2.0)))
    x = torch.ones(3, 2, requires_grad=True)
    attempt("qr r backward", lambda: LA.qr(x, mode="r").R.sum().backward())
    attempt("qr complete backward", lambda: LA.qr(x, mode="complete").Q.sum().backward())
    return lines

