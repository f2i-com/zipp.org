# torch.sparse parity cases: sparse COO (hybrid included), CSR and CSC
# tensors, their constructors, conversions, printing, coalescing, the
# arithmetic (PyTorch's merge for add, intersection for mul), reductions,
# softmax, products, shape ops, autograd (sparse gradients where PyTorch
# gives them), nn.Embedding(sparse=True) with SGD/Adagrad/SparseAdam, and
# the errors of common misuse. Runs unchanged under PyTorch 2.11 (gen.py
# writes sparse_expected.json and text_expected.txt) and under Zipp
# (python_torch_sparse.rs).
import math

import torch

try:
    import warnings

    warnings.simplefilter("ignore")
except ImportError:
    pass

D = torch.float64
F32 = torch.float32


def sp(i, v, size=None, dtype=None, coalesced=None):
    return torch.sparse_coo_tensor(torch.tensor(i), torch.tensor(v, dtype=dtype), size, is_coalesced=coalesced)


def flat(x):
    if isinstance(x, (tuple, list)):
        out = []
        for t in x:
            out.extend(flat(t))
        return out
    if isinstance(x, torch.Tensor):
        x = x.detach()
        if x.layout != torch.strided:
            x = x.to_dense()
        return [float(v) for v in x.reshape(-1).tolist()]
    if x is None:
        return []
    return [float(x)]


def vals(n, seed):
    return [math.sin(0.9 * i + seed) + 0.3 * math.cos(2.3 * i + 2 * seed) for i in range(n)]


def rand_coo(shape, nnz, seed, dtype=D, dense=()):
    """A deterministic uncoalesced COO tensor (duplicates included)."""
    idx = []
    for d, n in enumerate(shape):
        idx.append([int(abs(math.sin(1.7 * k + 3.1 * d + seed)) * 1000) % n for k in range(nnz)])
    block = 1
    for n in dense:
        block *= n
    v = torch.tensor(vals(nnz * block, seed), dtype=D).reshape((nnz,) + tuple(dense)).to(dtype)
    return torch.sparse_coo_tensor(torch.tensor(idx), v, tuple(shape) + tuple(dense))


def dense_t(shape, seed, dtype=D):
    n = 1
    for s in shape:
        n *= s
    return torch.tensor(vals(n, seed), dtype=D).reshape(tuple(shape)).to(dtype)


def rnd(v):
    """Values to 10 significant digits (exp/log may differ in the last ulp)."""
    if isinstance(v, list):
        return [rnd(x) for x in v]
    if isinstance(v, float):
        return float("%.10g" % v)
    return v


def st(s):
    """A sparse tensor's structure: layout, indices, values, flags."""
    if s.layout == torch.sparse_coo:
        return "coo %s %s %s %s" % (s._indices().tolist(), rnd(s._values().tolist()), s.is_coalesced(), tuple(s.shape))
    if s.layout == torch.sparse_csr:
        return "csr %s %s %s %s" % (s.crow_indices().tolist(), s.col_indices().tolist(), rnd(s.values().tolist()), tuple(s.shape))
    if s.layout == torch.sparse_csc:
        return "csc %s %s %s %s" % (s.ccol_indices().tolist(), s.row_indices().tolist(), rnd(s.values().tolist()), tuple(s.shape))
    return "dense %s" % (rnd(s.tolist()),)


def err(fn):
    try:
        r = fn()
    except Exception as e:
        msg = str(e).split("\n")[0]
        if msg.startswith("Could not run"):
            # Which operator it names differs (Zipp names the ones it
            # dispatches itself); the backend refusing it must not.
            at = msg.find("' backend")
            msg = "no kernel for the " + msg[msg.rfind("'", 0, at) + 1:at] + " backend"
        return "ERR %s: %s" % (type(e).__name__, msg)
    if isinstance(r, torch.Tensor):
        return st(r) if r.layout != torch.strided else "dense %s %s" % (r.dtype, rnd(r.tolist()))
    return repr(r)


def gname(t):
    g = t.grad_fn
    return None if g is None else type(g).__name__


def grads(loss_fn, *inputs):
    xs = [x.detach().clone().requires_grad_(True) for x in inputs]
    loss = loss_fn(*xs)
    gs = torch.autograd.grad(loss, xs, allow_unused=True)
    return [float(loss.detach())] + flat([torch.zeros_like(x) if g is None else g for x, g in zip(xs, gs)])


def leaf_grad(A, fn):
    L = A.detach().clone().requires_grad_()
    loss = fn(L)
    loss.backward()
    return "%s %s" % (gname(loss), err(lambda: L.grad))


def results():
    R = {}
    for dt, tag in ((D, "f64"), (F32, "f32")):
        S = rand_coo((7, 5), 24, 0.3, dt)
        H = rand_coo((6, 4), 15, 1.1, dt, (3,))
        M = dense_t((5, 4), 0.7, dt)
        R[tag + "_coalesce"] = flat([S.coalesce(), S.to_dense(), H.coalesce(), H.to_dense()])
        R[tag + "_to_sparse"] = flat([S.to_dense().to_sparse(), H.to_dense().to_sparse(1), S.to_dense().to_sparse_csr(), S.to_dense().to_sparse_csc()])
        R[tag + "_spmm"] = flat([torch.sparse.mm(S, M), torch.mm(S, M), S @ M, torch.matmul(S, M[:, 0]), S.to_sparse_csr() @ M,
                                 dense_t((4, 7), 0.5, dt) @ S, torch.sparse.mm(dense_t((4, 7), 0.5, dt), S), torch.sparse.addmm(dense_t((7, 4), 1.9, dt), S, M, beta=0.5, alpha=-2.0)])
        R[tag + "_spspmm"] = flat([torch.sparse.mm(S, rand_coo((5, 6), 11, 2.2, dt)), torch.mm(S.t(), S)])
        R[tag + "_sum"] = flat([torch.sparse.sum(S), torch.sparse.sum(S, 0), torch.sparse.sum(S, 1), torch.sparse.sum(H, (0, 2)),
                                torch.sparse.sum(H, 2), torch.sparse.sum(H, (0, 1)), S.sum(), torch.sum(S, 1, keepdim=True)])
        R[tag + "_softmax"] = flat([torch.sparse.softmax(S, 0), torch.sparse.softmax(S, 1), torch.sparse.log_softmax(S, 1),
                                    torch.sparse.softmax(H, 0), torch.sparse.softmax(H, 2), torch.sparse.log_softmax(H, 1)])
        S2 = rand_coo((7, 5), 19, 4.4, dt)
        X = dense_t((7, 5), 2.8, dt)
        R[tag + "_arith"] = flat([S + S2, S - S2, torch.add(S, S2, alpha=2.5), X + S, X - S, torch.add(X, S, alpha=-0.5), S * S2,
                                  S.coalesce() * S2.coalesce(), S * X, X * S, S * 2.5, S / 3.0, -S, S.pow(2), torch.sin(S), torch.sqrt(S.abs()),
                                  torch.tanh(S), torch.relu(S), torch.expm1(S), torch.log1p(S.abs())])
        R[tag + "_shape"] = flat([S.t(), S.transpose(0, 1), S.index_select(0, torch.tensor([6, 0, 3, 3])), S.index_select(1, torch.tensor([4, 1])),
                                  H.index_select(2, torch.tensor([2, 0])), S[3], S[2, 1], H[1], torch.cat([S, S2], 0), torch.cat([S, S2], 1),
                                  torch.stack([S, S2]), S.to_sparse_csr().t().to_dense()])
        # gradients of real losses
        W = dense_t((7, 5), 3.3, dt)
        R[tag + "_grad_spmm"] = grads(lambda v, m: (torch.sparse.mm(torch.sparse_coo_tensor(S._indices(), v, S.shape), m) * dense_t((7, 4), 0.1, dt)).sum(), S._values(), M)
        R[tag + "_grad_mm"] = grads(lambda v, m: (torch.mm(torch.sparse_coo_tensor(S._indices(), v, S.shape), m) ** 2).sum(), S._values(), M)
        R[tag + "_grad_dsmm"] = grads(lambda v, m: (m @ torch.sparse_coo_tensor(S._indices(), v, S.shape)).sin().sum(), S._values(), dense_t((4, 7), 0.5, dt))
        R[tag + "_grad_todense"] = grads(lambda v: (torch.sparse_coo_tensor(S._indices(), v, S.shape).to_dense() * W).sum(), S._values())
        R[tag + "_grad_values"] = grads(lambda v: (torch.sparse_coo_tensor(S._indices(), v, S.shape).coalesce().values() ** 3).sum(), S._values())
        R[tag + "_grad_sum"] = grads(lambda v: (torch.sparse.sum(torch.sparse_coo_tensor(H._indices(), v, H.shape), (0, 2)).to_dense() * torch.tensor([1.0, -2.0, 3.0, 0.5], dtype=dt)).sum(), H._values())
        R[tag + "_grad_softmax"] = grads(lambda v: (torch.sparse.softmax(torch.sparse_coo_tensor(S._indices(), v, S.shape), 1).to_dense() * W).sum(), S._values())
        R[tag + "_grad_logsoftmax"] = grads(lambda v: (torch.sparse.log_softmax(torch.sparse_coo_tensor(H._indices(), v, H.shape), 0).to_dense() * dense_t((6, 4, 3), 0.9, dt)).sum(), H._values())
        R[tag + "_grad_arith"] = grads(lambda v, x: ((torch.sparse_coo_tensor(S._indices(), v, S.shape) * 3.0 + torch.sparse_coo_tensor(S._indices(), v, S.shape) * x).to_dense() ** 2).sum()
                                       + (x + torch.sparse_coo_tensor(S._indices(), v, S.shape)).sum(), S._values(), X)
        R[tag + "_grad_unary"] = grads(lambda v: torch.sparse.sum(torch.sparse_coo_tensor(S._indices(), v, S.shape).pow(3)), S._values())
        R[tag + "_grad_addmm"] = grads(lambda b, v, m: (torch.sparse.addmm(b, torch.sparse_coo_tensor(S._indices(), v, S.shape), m, beta=0.25, alpha=3.0) ** 2).sum(),
                                       dense_t((7, 4), 1.9, dt), S._values(), M)
        R[tag + "_grad_csr"] = grads(lambda v, m: (torch.sparse_csr_tensor(torch.tensor([0, 2, 3, 5]), torch.tensor([0, 2, 1, 0, 2]), v, (3, 3)) @ m).pow(2).sum(),
                                     dense_t((5,), 0.4, dt), dense_t((3, 2), 1.4, dt))
        # sparse leaves: gradients with PyTorch's sparse layouts
        L = S.detach().clone().requires_grad_()
        (torch.sparse.mm(L, M) * dense_t((7, 4), 0.1, dt)).sum().backward()
        R[tag + "_leaf_spmm"] = flat(L.grad)
        L = S.detach().clone().requires_grad_()
        torch.mm(L, M).sum().backward()
        R[tag + "_leaf_mm"] = flat(L.grad)
        L = H.detach().clone().requires_grad_()
        (torch.sparse.softmax(L, 1).to_dense() * dense_t((6, 4, 3), 1.2, dt)).sum().backward()
        R[tag + "_leaf_softmax"] = flat(L.grad)
        # nn.Embedding(sparse=True) and the optimizers that take sparse gradients
        for opt_name in ("SGD", "SGDm", "Adagrad", "SparseAdam"):
            emb = torch.nn.Embedding(10, 4, sparse=True).to(dt)
            with torch.no_grad():
                emb.weight.copy_(dense_t((10, 4), 5.5, dt))
            if opt_name == "SGD":
                opt = torch.optim.SGD(emb.parameters(), lr=0.1)
            elif opt_name == "SGDm":
                opt = torch.optim.SGD(emb.parameters(), lr=0.1, momentum=0.9, nesterov=True)
            elif opt_name == "Adagrad":
                opt = torch.optim.Adagrad(emb.parameters(), lr=0.1, lr_decay=0.01)
            else:
                opt = torch.optim.SparseAdam(emb.parameters(), lr=0.05)
            head = dense_t((4, 3), 6.1, dt)
            losses = []
            for step in range(5):
                idx = torch.tensor([[step % 10, (3 * step + 1) % 10, 7], [2, (step * step) % 10, 2]])
                opt.zero_grad()
                loss = (emb(idx) @ head).tanh().sum()
                loss.backward()
                opt.step()
                losses.append(loss.item())
            R["%s_opt_%s" % (tag, opt_name)] = losses + flat(emb.weight)
        # a two-layer GCN step on a normalised sparse adjacency
        torch.manual_seed(0)
        n = 9
        edges = [(i, (i * 4 + 1) % n) for i in range(n)] + [(i, (i + 2) % n) for i in range(n)]
        ind = torch.tensor([[a for a, b in edges] + [b for a, b in edges] + list(range(n)), [b for a, b in edges] + [a for a, b in edges] + list(range(n))])
        A = torch.sparse_coo_tensor(ind, torch.ones(ind.shape[1], dtype=dt), (n, n)).coalesce()
        deg = torch.sparse.sum(A, 1).to_dense()
        norm = deg.pow(-0.5)
        A = torch.sparse_coo_tensor(A.indices(), A.values() * norm[A.indices()[0]] * norm[A.indices()[1]], (n, n))
        Xf = dense_t((n, 5), 0.6, dt)
        W1 = dense_t((5, 6), 1.6, dt).requires_grad_()
        W2 = dense_t((6, 3), 2.6, dt).requires_grad_()
        target = torch.tensor([i % 3 for i in range(n)])
        opt = torch.optim.Adam([W1, W2], lr=0.01)
        losses = []
        for step in range(4):
            opt.zero_grad()
            h = torch.relu(torch.sparse.mm(A, Xf @ W1))
            out = torch.sparse.mm(A, h @ W2)
            loss = torch.nn.functional.cross_entropy(out, target)
            loss.backward()
            opt.step()
            losses.append(loss.item())
        R[tag + "_gcn"] = losses + flat([W1, W2])
    return R


def text():
    T = []
    p = T.append
    A = sp([[0, 1, 1, 0, 2], [2, 0, 2, 2, 1]], [1.0, 2.0, 3.0, 4.0, 5.0], (3, 3))
    B = sp([[1, 0, 2, 0, 0, 2, 1], [2, 2, 2, 0, 1, 0, 1]], [10.0, 20.0, 30.0, 40.0, 50.0, 60.0, 70.0], (3, 3))
    C = sp([[0, 2], [2, 1]], [7.0, 8.0], (3, 3))
    H = sp([[0, 2, 0]], [[1.0, 2.0], [3.0, 4.0], [5.0, 6.0]], (3, 2))
    X = torch.tensor([[0.0, 1.0, 0.0], [2.0, 0.0, 3.0], [4.0, 5.0, 0.0]])
    Dm = torch.arange(6.0).reshape(3, 2)
    R = X.to_sparse_csr()
    # printing
    for t in (A, A.coalesce(), A.double(), A.long(), A.int(), A.bool(), A.half(), H, H.coalesce(), R, X.to_sparse_csc(), X.to_sparse(),
              X.to_sparse(1), torch.sparse_coo_tensor(torch.zeros(2, 0, dtype=torch.long), torch.zeros(0), (2, 3)), torch.zeros(3, 3).to_sparse(),
              torch.sparse_coo_tensor(size=(2, 3)), torch.sparse_csr_tensor(torch.tensor([0, 2, 3]), torch.tensor([0, 2, 1]), torch.tensor([1.0, 2.0, 3.0])),
              torch.sparse_csr_tensor(torch.tensor([0, 1, 2], dtype=torch.int32), torch.tensor([0, 1], dtype=torch.int32), torch.tensor([1.0, 2.0]), (2, 2)),
              torch.sparse_csc_tensor(torch.tensor([0, 1, 1, 3]), torch.tensor([0, 0, 1]), torch.tensor([1.0, 2.0, 3.0]), (2, 3)),
              A.requires_grad_(False).detach().clone().requires_grad_(), torch.tensor([[1, 0], [0, 2]]).to_sparse_csr(),
              sp([[0, 1]], [[[1.0, 2.0]], [[3.0, 4.0]]], (2, 1, 2)), torch.sparse_coo_tensor([[0, 1], [1, 0]], [1, 2], (2, 2)),
              torch.sparse_coo_tensor([[0, 1], [1, 0]], [1.0, 2.0]), sp([[0], [1]], [1.5], (2, 2)), torch.eye(40).to_sparse(), torch.eye(5).to_sparse().t()):
        for line in repr(t).split("\n"):
            p(line)
    p(str(A.layout) + " " + str(R.layout) + " " + str(torch.strided) + " " + str(X.to_sparse_csc().layout))
    for t in (A, A.coalesce(), H, R, X, X.to_sparse_csc()):
        p("%s %s %s %s %s %s %s" % (t.is_sparse, t.is_sparse_csr, t.layout, t.dtype, tuple(t.shape), t.sparse_dim(), t.dense_dim()))
    p("%s %s %s %s %s" % (type(A).__name__, isinstance(A, torch.Tensor), A.device, A.is_cuda, A.requires_grad))
    p("%s %s %s %s %s" % (A._nnz(), R._nnz(), A.dim(), A.numel(), A.element_size()))
    for f in (lambda: A.indices(), lambda: A.values(), lambda: A._indices(), lambda: A._values(), lambda: A.coalesce().indices(),
              lambda: A.coalesce().values(), lambda: R.crow_indices(), lambda: R.col_indices(), lambda: R.values(), lambda: A.crow_indices(),
              lambda: R.indices(), lambda: R.is_coalesced(), lambda: R.coalesce(), lambda: X.coalesce(), lambda: X.is_coalesced(),
              lambda: X.indices(), lambda: X.values(), lambda: X._nnz(), lambda: X.to_dense() is X, lambda: A.nnz(), lambda: A.is_coalesced(),
              lambda: A.stride(), lambda: A.is_contiguous(), lambda: A.type(), lambda: A.long().type(), lambda: A.type(torch.float64),
              lambda: R.type(), lambda: A.storage(), lambda: A.data_ptr(), lambda: A.item(), lambda: A.tolist(), lambda: A.numpy(),
              lambda: A.view(9), lambda: A.reshape(9), lambda: A.contiguous(), lambda: X.to_sparse_csc().ccol_indices(),
              lambda: X.to_sparse_csc().row_indices(), lambda: R.ccol_indices(), lambda: A.sparse_mask(A), lambda: len(A), lambda: [st(r) for r in A],
              lambda: A.to_sparse() is A, lambda: A.to_sparse(1), lambda: R.to_sparse_csr() is R, lambda: A.new(torch.tensor([[0], [1]]), torch.tensor([9.0]), (3, 3)),
              lambda: A.coalesce().is_coalesced(), lambda: A.coalesce().coalesce() is A.coalesce(), lambda: sp([[0, 1], [1, 0]], [1.0, 2.0], (2, 2)).is_coalesced(),
              lambda: sp([[0], [1]], [1.0], (2, 2)).is_coalesced(), lambda: A._coalesced_(True).is_coalesced(), lambda: A._coalesced_(False).is_coalesced()):
        p(err(f))
    # legacy constructors, spdiags, sampled_addmm
    ii = torch.tensor([[0, 1], [1, 0]])
    diags = torch.tensor([[1.0, 2.0, 3.0, 4.0], [5.0, 6.0, 7.0, 8.0], [9.0, 10.0, 11.0, 12.0]])
    for f in (lambda: torch.sparse.FloatTensor(ii, torch.tensor([1.0, 2.0]), torch.Size([2, 2])), lambda: torch.sparse.FloatTensor(2, 3),
              lambda: torch.sparse.DoubleTensor(ii, torch.tensor([1.0, 2.0], dtype=torch.float64), torch.Size([2, 2])),
              lambda: torch.sparse.LongTensor(ii, torch.tensor([1, 2]), torch.Size([2, 2])),
              lambda: torch.sparse.spdiags(diags[:2, :3], torch.tensor([0, -1]), (3, 3)), lambda: torch.sparse.spdiags(diags, torch.tensor([1, 0, -2]), (4, 3)),
              lambda: torch.sparse.spdiags(diags, torch.tensor([2, -1, 0]), (3, 5)), lambda: torch.sparse.spdiags(diags[:1], torch.tensor([0]), (3, 3), layout=torch.sparse_csr),
              lambda: torch.sparse.sampled_addmm(X.to_sparse_csr(), torch.arange(6.0).reshape(3, 2), torch.arange(6.0).reshape(2, 3), beta=0.5, alpha=2.0)):
        p(err(f))
    # constructor errors and checks
    i = torch.tensor([[0, 1, 1, 0], [2, 0, 2, 2]])
    v = torch.tensor([3.0, 4.0, 5.0, 6.0])
    for f in (lambda: torch.sparse_coo_tensor(i, v, (2, 2)), lambda: torch.sparse_coo_tensor(i, v, (2, 2), check_invariants=True),
              lambda: torch.sparse_coo_tensor(i, v[:3], (2, 3)), lambda: torch.sparse_coo_tensor(i.float(), v, (2, 3)),
              lambda: torch.sparse_coo_tensor(i[0], v, (2, 3)), lambda: torch.sparse_coo_tensor(-i, v, (2, 3), check_invariants=True),
              lambda: torch.sparse_coo_tensor(-i, v, (2, 3)), lambda: torch.sparse_coo_tensor(i, v, (2, 3), is_coalesced=True),
              lambda: torch.sparse_coo_tensor(i, v), lambda: torch.sparse_coo_tensor(i, v, (2, 3), dtype=torch.float64),
              lambda: torch.sparse_coo_tensor(torch.tensor([[0]]), torch.ones(1, 3), (2, 2)), lambda: torch.sparse_coo_tensor(torch.zeros(1, 1, 1, dtype=torch.long), torch.ones(1), (2,)),
              lambda: torch.sparse_coo_tensor(i, v.long(), (2, 3), requires_grad=True), lambda: torch.sparse_coo_tensor(i, v, (2, 3), requires_grad=True).requires_grad,
              lambda: torch.sparse_coo_tensor(i, v, (2, 3), check_invariants=True)):
        p(err(f))
    with torch.sparse.check_sparse_tensor_invariants():
        p(err(lambda: torch.sparse_coo_tensor(i, v, (2, 2))))
        p(str(torch.sparse.check_sparse_tensor_invariants.is_enabled()))
    p(str(torch.sparse.check_sparse_tensor_invariants.is_enabled()))
    # coalescing and the arithmetic's structure
    Ac, Bc = A.coalesce(), B.coalesce()
    for f in (lambda: A + B, lambda: B + A, lambda: Ac + Bc, lambda: Ac + B, lambda: A - B, lambda: torch.add(A, B, alpha=2), lambda: A + A,
              lambda: A * B, lambda: B * A, lambda: A * Bc, lambda: Ac * B, lambda: Ac * Bc, lambda: Bc * Ac, lambda: A * C, lambda: C * A,
              lambda: A * C.coalesce(), lambda: A * A, lambda: X + A, lambda: A + X, lambda: torch.add(X, A, alpha=2), lambda: X - A, lambda: A - X,
              lambda: A + 1, lambda: A * X, lambda: X * A, lambda: A * 2, lambda: 2 * A, lambda: A * torch.tensor(2.0), lambda: A * torch.tensor([2.0]),
              lambda: A * torch.tensor([1.0, 2.0, 3.0]), lambda: A.long() * 2.5, lambda: A.long() * 2, lambda: A / 2, lambda: A.long() / 2,
              lambda: A / B, lambda: A / X, lambda: 2 / A, lambda: -A, lambda: -Ac, lambda: A.pow(2), lambda: A ** 2, lambda: torch.sin(A), lambda: torch.sqrt(Ac),
              lambda: torch.abs(-A), lambda: torch.relu(-A), lambda: torch.exp(A), lambda: A == A, lambda: A.double() + A, lambda: A + A.long(),
              lambda: A + sp([[0], [0]], [1.0], (3, 4)), lambda: A * sp([[0], [0]], [1.0], (3, 4)), lambda: A * torch.ones(3, 4), lambda: torch.ones(3) + A,
              lambda: torch.square(A), lambda: torch.floor(A * 0.5), lambda: torch.sign(-A), lambda: torch.isnan(A), lambda: torch.softmax(A, 1),
              lambda: torch.sparse_coo_tensor(torch.zeros(2, 0, dtype=torch.long), torch.zeros(0), (3, 3)) + A,
              lambda: A + torch.sparse_coo_tensor(torch.zeros(2, 0, dtype=torch.long), torch.zeros(0), (3, 3))):
        p(err(f))
    # reductions, softmax
    for f in (lambda: torch.sparse.sum(A), lambda: torch.sparse.sum(A, 0), lambda: torch.sparse.sum(A, 1), lambda: torch.sparse.sum(A, (0, 1)),
              lambda: A.sum(), lambda: A.sum(0), lambda: torch.sum(A, 0, keepdim=True), lambda: torch.sparse.sum(A.long(), dtype=torch.float64),
              lambda: torch.sparse.sum(H, 1), lambda: torch.sparse.sum(H, (0, 1)), lambda: torch.sparse.sum(H, 0), lambda: torch.sparse.sum(A.long()),
              lambda: torch.sparse.sum(A.bool()), lambda: R.sum(), lambda: R.sum(0), lambda: torch.sparse.softmax(A.double(), 1),
              lambda: torch.sparse.softmax(A.double(), 0), lambda: torch.sparse.log_softmax(A.double(), 1), lambda: torch.sparse.softmax(A.long(), 1),
              lambda: torch.sparse.softmax(H.double(), 1), lambda: torch.sparse.softmax(H.double(), 0), lambda: torch.sparse.softmax(A, 1, dtype=torch.float64),
              lambda: A.norm(), lambda: A.norm(1), lambda: A.mean(), lambda: A.max()):
        p(err(f))
    # products
    S1 = sp([[0, 2], [0, 1]], [1.0, 2.0], (3, 3))
    for f in (lambda: torch.sparse.mm(A, Dm), lambda: A @ Dm, lambda: torch.mm(A, Dm), lambda: torch.matmul(A, torch.ones(3)),
              lambda: torch.ones(4, 3) @ A, lambda: torch.mm(torch.ones(4, 3), A), lambda: torch.sparse.mm(torch.ones(2, 3), A), lambda: A @ A.t(),
              lambda: torch.sparse.mm(A, A.t()), lambda: torch.mm(A, S1), lambda: torch.sparse.addmm(torch.ones(3, 2), A, Dm, beta=0.5, alpha=2),
              lambda: torch.addmm(torch.ones(3, 2), A, Dm), lambda: torch.smm(A, Dm), lambda: torch.hspmm(A, Dm), lambda: R @ Dm, lambda: Dm.t() @ R,
              lambda: R @ R, lambda: A @ torch.ones(4, 2), lambda: A.double() @ Dm, lambda: torch.mm(A, torch.ones(3, 2, 2)),
              lambda: torch.mm(A.bool(), torch.ones(3, 2, dtype=torch.bool)), lambda: torch.mm(A.long(), torch.ones(3, 2, dtype=torch.long)),
              lambda: torch.sparse.mm(H, torch.ones(2, 2)), lambda: torch.mm(torch.sparse_coo_tensor(torch.zeros(2, 0, dtype=torch.long), torch.zeros(0), (3, 3)), Dm),
              lambda: torch.mm(A.to(torch.complex64), torch.ones(3, 2, dtype=torch.complex64))):
        p(err(f))
    # shape ops and conversions
    for f in (lambda: A.t(), lambda: A.transpose(0, 1), lambda: Ac.t(), lambda: H.t(), lambda: H.transpose(0, 1), lambda: sp([[0], [0], [0]], [1.0], (2, 2, 2)).t(),
              lambda: A.index_select(1, torch.tensor([2, 0])), lambda: A.index_select(0, torch.tensor([1, 1])), lambda: Ac.index_select(0, torch.tensor([0, 2])),
              lambda: A.index_select(0, torch.tensor([-1])), lambda: A.index_select(0, torch.tensor([3])), lambda: H.index_select(1, torch.tensor([1, 1, 0])),
              lambda: A[0], lambda: A[-1], lambda: A[0, 2], lambda: H[0], lambda: H[1], lambda: sp([[0, 2, 0]], [1.0, 2.0, 3.0], (3,))[0], lambda: A[0:2],
              lambda: torch.cat([A, A], 0), lambda: torch.cat([A, Ac], 1), lambda: torch.stack([A, A]), lambda: torch.zeros_like(A), lambda: A.clone(),
              lambda: Ac.clone(), lambda: Ac.detach(), lambda: Ac.double(), lambda: A.to(torch.float64), lambda: A.to(torch.float32) is A,
              lambda: A.to_dense(), lambda: H.to_dense(), lambda: R.to_dense(), lambda: A.to_sparse_csr(), lambda: A.to_sparse_csc(), lambda: R.to_sparse(),
              lambda: R.to_sparse_coo(), lambda: X.to_sparse_csc().to_sparse(), lambda: R.t(), lambda: R.transpose(0, 1), lambda: R.t().t(), lambda: H.to_sparse_csr(),
              lambda: torch.eye(3).to_sparse(layout=torch.sparse_csr), lambda: R + R, lambda: R * 2, lambda: R * R, lambda: -R, lambda: torch.sin(R),
              lambda: R.to(torch.float64), lambda: torch.ones(3, 3) + R, lambda: R / 2, lambda: X.sparse_mask(A), lambda: X.sparse_mask(Ac),
              lambda: X.to_sparse().sparse_dim(), lambda: A.clone().zero_(), lambda: A.clone().mul_(3), lambda: A.clone().add_(B), lambda: A.clone().add_(X),
              lambda: Ac.clone().add_(Ac), lambda: A.clone().add_(B, alpha=2), lambda: torch.zeros(3, 3).add_(A, alpha=-0.5), lambda: A.clone().neg_(),
              lambda: A.clone().div_(2)):
        p(err(f))
    # autograd: grad_fn names and sparse gradients
    vv = torch.tensor([1.0, 2.0, 3.0, 4.0, 5.0], requires_grad=True)
    S = torch.sparse_coo_tensor(A._indices(), vv, (3, 3))
    Dg = Dm.clone().requires_grad_()
    out = torch.sparse.mm(S, Dg)
    p("%s %s" % (gname(S), gname(out)))
    out.sum().backward()
    p("%s %s" % (vv.grad.tolist(), Dg.grad.tolist()))
    wts = torch.tensor([[1.0, 2.0], [3.0, 4.0], [5.0, 6.0]])
    for label, fn in (("sparse.mm", lambda L: (torch.sparse.mm(L, Dm) * wts).sum()), ("mm", lambda L: (torch.mm(L, Dm) * wts).sum()),
                      ("to_dense", lambda L: (L.to_dense() * torch.arange(9.0).reshape(3, 3)).sum()),
                      ("values", lambda L: (L.coalesce().values() * torch.tensor([1.0, 2.0, 3.0, 4.0])).sum()),
                      ("sum_dim", lambda L: torch.sparse.sum(L, 1).to_dense().sum()), ("sum", lambda L: torch.sparse.sum(L)),
                      ("softmax", lambda L: (torch.sparse.softmax(L, 1).to_dense() * torch.arange(9.0).reshape(3, 3)).sum()),
                      ("addmm", lambda L: torch.sparse.addmm(torch.ones(3, 2), L, Dm, beta=0.5, alpha=2).sum()),
                      ("scaled", lambda L: torch.sparse.sum(L * 3 + L)), ("mixed", lambda L: torch.sparse.sum(L.double() + L)), ("three", lambda L: torch.sparse.sum(L * 2) + torch.sparse.sum(L * 5) + torch.sparse.sum(L * 7)),
                      ("t", lambda L: (L.t().to_dense() * torch.arange(9.0).reshape(3, 3)).sum()), ("neg", lambda L: torch.sparse.sum(-L)),
                      ("div", lambda L: (torch.sparse.mm(L / 4, Dm) * wts).sum()), ("pow", lambda L: (L.pow(2).to_dense() * torch.arange(9.0).reshape(3, 3)).sum()),
                      ("mul_dense", lambda L: (L * X).to_dense().sum()), ("mm_sparse", lambda L: (torch.mm(L, S1).to_dense() * torch.arange(9.0).reshape(3, 3)).sum()),
                      ("sparse_mm_sparse", lambda L: (torch.sparse.mm(L, S1).to_dense() * torch.arange(9.0).reshape(3, 3)).sum())):
        p("%s %s" % (label, err(lambda: leaf_grad(A, fn))))
    L = A.detach().clone().requires_grad_()
    torch.sparse.sum(L * 2).backward()
    torch.sparse.sum(L * 3).backward()
    p("accumulate " + err(lambda: L.grad))
    L = A.detach().clone().requires_grad_()
    torch.sparse.sum(L * 2).backward()
    torch.mm(L, torch.ones(3, 1)).sum().backward()
    p("sparse+dense " + err(lambda: L.grad))
    L = A.detach().clone().requires_grad_()
    m = L * 2
    m.retain_grad()
    (torch.sparse.sum(m * 3) + torch.sparse.sum(m)).backward()
    p("retain %s %s" % (err(lambda: m.grad), err(lambda: L.grad)))
    Lc = A.detach().clone().requires_grad_()
    for name, t in (("coalesce", Lc.coalesce()), ("values", Lc.coalesce().values()), ("_values", Lc._values()), ("to_dense", Lc.to_dense()),
                    ("t", Lc.t()), ("clone", Lc.clone()), ("double", Lc.double()), ("neg", -Lc), ("mul", Lc * 2), ("add", Lc + Lc),
                    ("sum", torch.sparse.sum(Lc)), ("softmax", torch.sparse.softmax(Lc, 0)), ("log_softmax", torch.sparse.log_softmax(Lc, 0)),
                    ("to_sparse", X.clone().requires_grad_().to_sparse()), ("mm", torch.mm(Lc, Dm)), ("sparse.mm", torch.sparse.mm(Lc, Dm))):
        p("%s %s %s" % (name, gname(t), t.requires_grad))
    for line in repr(torch.sparse.softmax(Lc, 1)).split("\n"):
        p(line)
    # copies and a module holding a sparse buffer
    import copy
    L = A.detach().clone().requires_grad_()
    torch.sparse.sum(L * 2).backward()
    Lc = copy.deepcopy(L)
    p("deepcopy %s %s %s %s" % (st(Lc), Lc.requires_grad, err(lambda: Lc.grad), Lc._values() is L._values()))
    p("copy %s %s" % (st(copy.copy(A)), err(lambda: copy.deepcopy(L * 2))))

    class Gcn(torch.nn.Module):
        def __init__(self, adj):
            super().__init__()
            self.register_buffer("adj", adj)
            self.lin = torch.nn.Linear(2, 2)

        def forward(self, x):
            return torch.sparse.mm(self.adj, self.lin(x))

    net = Gcn(A.coalesce())
    with torch.no_grad():
        net.lin.weight.copy_(torch.tensor([[1.0, -1.0], [0.5, 2.0]]))
        net.lin.bias.copy_(torch.tensor([0.25, -0.5]))
    p("module %s %s" % (sorted(net.state_dict()), st(net.state_dict()["adj"])))
    p("forward %s" % err(lambda: net(Dm)))
    net2 = Gcn(torch.sparse_coo_tensor(torch.zeros(2, 0, dtype=torch.long), torch.zeros(0), (3, 3)))
    net2.load_state_dict(net.state_dict())
    p("loaded %s %s" % (st(net2.adj), err(lambda: net2(Dm))))
    p("double %s" % st(net.double().adj))
    # nn.Embedding(sparse=True)
    emb = torch.nn.Embedding(5, 3, sparse=True)
    with torch.no_grad():
        emb.weight.copy_(torch.arange(15.0).reshape(5, 3))
    o = emb(torch.tensor([[1, 3], [1, 0]]))
    p("%s %s" % (gname(o), emb))
    (o * torch.arange(12.0).reshape(2, 2, 3)).sum().backward()
    for line in repr(emb.weight.grad).split("\n"):
        p(line)
    emb = torch.nn.Embedding(5, 3, sparse=True, padding_idx=0)
    (emb(torch.tensor([[1, 3], [1, 0]])) * 2).sum().backward()
    p("padding " + err(lambda: emb.weight.grad))
    emb = torch.nn.Embedding(4, 2, sparse=True)
    w = torch.tensor([[1.0, -1.0], [2.0, 0.5]])
    ((emb(torch.tensor([1, 2])) * w).sum() + (emb(torch.tensor([2, 3])) * w).sum() * 2).backward()
    p("two lookups " + err(lambda: emb.weight.grad))
    (emb(torch.tensor([0])) * w[0]).sum().backward()
    p("second backward " + err(lambda: emb.weight.grad))
    bag = torch.nn.EmbeddingBag(6, 2, mode="sum", sparse=True)
    with torch.no_grad():
        bag.weight.copy_(torch.arange(12.0).reshape(6, 2))
    (bag(torch.tensor([1, 2, 4, 1]), torch.tensor([0, 2])) * torch.tensor([[1.0, 2.0], [3.0, 4.0]])).sum().backward()
    p("bag " + err(lambda: bag.weight.grad))
    bag = torch.nn.EmbeddingBag(6, 2, mode="mean", sparse=True)
    (bag(torch.tensor([[1, 2], [4, 1]])) * torch.tensor([[1.0, 2.0], [3.0, 4.0]])).sum().backward()
    p("bag mean " + err(lambda: bag.weight.grad))
    # optimizers with sparse gradients
    for name in ("SGD", "Adam", "AdamW", "RMSprop", "Adagrad", "Adamax", "NAdam", "RAdam", "Adadelta", "ASGD", "Rprop", "SparseAdam"):
        emb = torch.nn.Embedding(4, 2, sparse=True)
        with torch.no_grad():
            emb.weight.copy_(torch.arange(8.0).reshape(4, 2))
        opt = getattr(torch.optim, name)(emb.parameters(), lr=0.5)
        (emb(torch.tensor([1, 2, 1])) * torch.tensor([1.0, 2.0])).sum().backward()
        p("%s %s %s" % (name, err(lambda: opt.step()), emb.weight.tolist()))
    emb = torch.nn.Embedding(4, 2, sparse=True)
    (emb(torch.tensor([1, 2, 1])) * torch.tensor([1.0, 2.0])).sum().backward()
    for f in (lambda: torch.optim.SGD(emb.parameters(), lr=0.1, weight_decay=0.1).step(), lambda: torch.optim.Adagrad(emb.parameters(), lr=0.1, weight_decay=0.1).step(),
              lambda: torch.optim.SparseAdam([torch.zeros(2, 2).to_sparse()]), lambda: torch.nn.utils.clip_grad_norm_(emb.parameters(), 1.0) is None and None):
        p(err(f))
    emb2 = torch.nn.Embedding(4, 2)
    emb2(torch.tensor([1])).sum().backward()
    p(err(lambda: torch.optim.SparseAdam(emb2.parameters()).step()))
    emb = torch.nn.Embedding(4, 2, sparse=True)
    with torch.no_grad():
        emb.weight.copy_(torch.arange(8.0).reshape(4, 2))
    opt = torch.optim.SGD(emb.parameters(), lr=0.5, momentum=0.9)
    for idx in ([1, 2, 1], [3]):
        opt.zero_grad()
        (emb(torch.tensor(idx)) * torch.tensor([1.0, 2.0])).sum().backward()
        opt.step()
        p("momentum %s %s" % (err(lambda: opt.state[emb.weight]["momentum_buffer"]), emb.weight.tolist()))
    return T
