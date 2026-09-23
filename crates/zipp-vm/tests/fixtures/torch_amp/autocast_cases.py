"""torch.autocast("cpu") cases, run unchanged by CPython with PyTorch 2.11
(gen.py) and by Zipp (python_torch_amp.rs)."""
import torch
import torch.nn as nn
import torch.nn.functional as F


def T(*shape, dt=torch.float32, pos=False, seed=0):
    n = 1
    for s in shape:
        n *= s
    v = [((i * 37 + seed * 7) % 23 - 11) / 7.0 for i in range(n)]
    if pos:
        v = [abs(x) + 0.5 for x in v]
    return torch.tensor(v, dtype=torch.float64).reshape(*shape).to(dt)


def SPD(n, dt):
    a = T(n, n, dt=torch.float64)
    return (a @ a.T + n * torch.eye(n, dtype=torch.float64)).to(dt)


def ops(dt):
    tgt = torch.tensor([1, 0, 2, 1])
    img = lambda: T(1, 2, 6, 6, dt=dt)
    vol = lambda: T(1, 2, 4, 4, 4, dt=dt)
    return [
        ("matmul", lambda: T(4, 5, dt=dt) @ T(5, 3, dt=dt)),
        ("matmul_1d", lambda: torch.matmul(T(5, dt=dt), T(5, dt=dt))),
        ("mm", lambda: torch.mm(T(4, 5, dt=dt), T(5, 3, dt=dt))),
        ("bmm", lambda: torch.bmm(T(2, 4, 5, dt=dt), T(2, 5, 3, dt=dt))),
        ("baddbmm", lambda: torch.baddbmm(T(2, 4, 3, dt=dt), T(2, 4, 5, dt=dt), T(2, 5, 3, dt=dt))),
        ("addmm", lambda: torch.addmm(T(4, 3, dt=dt), T(4, 5, dt=dt), T(5, 3, dt=dt))),
        ("addbmm", lambda: torch.addbmm(T(4, 3, dt=dt), T(2, 4, 5, dt=dt), T(2, 5, 3, dt=dt))),
        ("Tensor.mm", lambda: T(4, 5, dt=dt).mm(T(5, 3, dt=dt))),
        ("mv", lambda: torch.mv(T(4, 5, dt=dt), T(5, dt=dt))),
        ("addmv", lambda: torch.addmv(T(4, dt=dt), T(4, 5, dt=dt), T(5, dt=dt))),
        ("dot", lambda: torch.dot(T(5, dt=dt), T(5, dt=dt))),
        ("outer", lambda: torch.outer(T(3, dt=dt), T(4, dt=dt))),
        ("einsum_contract", lambda: torch.einsum("ij,jk->ik", T(4, 5, dt=dt), T(5, 3, dt=dt))),
        ("einsum_elementwise", lambda: torch.einsum("i,i->i", T(4, dt=dt), T(4, dt=dt))),
        ("tensordot", lambda: torch.tensordot(T(4, 5, dt=dt), T(5, 3, dt=dt), dims=1)),
        ("tensordot_full", lambda: torch.tensordot(T(4, 5, dt=dt), T(4, 5, dt=dt), dims=2)),
        ("multi_dot", lambda: torch.linalg.multi_dot([T(4, 5, dt=dt), T(5, 3, dt=dt), T(3, 2, dt=dt)])),
        ("matrix_power", lambda: torch.matrix_power(T(3, 3), 3)),
        ("linalg.vecdot", lambda: torch.linalg.vecdot(T(4, 5, dt=dt), T(4, 5, dt=dt))),
        ("F.linear", lambda: F.linear(T(4, 5, dt=dt), T(3, 5, dt=dt), T(3, dt=dt))),
        ("F.bilinear", lambda: F.bilinear(T(4, 3, dt=dt), T(4, 2, dt=dt), T(5, 3, 2, dt=dt), T(5, dt=dt))),
        ("F.conv1d", lambda: F.conv1d(T(1, 2, 8, dt=dt), T(3, 2, 3, dt=dt))),
        ("F.conv2d", lambda: F.conv2d(img(), T(3, 2, 3, 3, dt=dt), T(3, dt=dt), padding=1)),
        ("F.conv3d", lambda: F.conv3d(vol(), T(3, 2, 2, 2, 2, dt=dt))),
        ("F.conv_transpose2d", lambda: F.conv_transpose2d(img(), T(2, 3, 3, 3, dt=dt))),
        ("F.prelu", lambda: F.prelu(T(4, 5, dt=dt), T(1, dt=dt))),
        ("F.sdpa", lambda: F.scaled_dot_product_attention(T(1, 3, 4, dt=dt), T(1, 3, 4, dt=dt, seed=1), T(1, 3, 4, dt=dt, seed=2))),
        ("F.relu", lambda: F.relu(T(4, 5, dt=dt))),
        ("F.softmax", lambda: F.softmax(T(4, 5, dt=dt), -1)),
        ("F.layer_norm", lambda: F.layer_norm(T(4, 5, dt=dt), (5,))),
        ("F.max_pool2d", lambda: F.max_pool2d(img(), 2)),
        ("F.max_pool3d", lambda: F.max_pool3d(vol(), 2)),
        ("F.adaptive_avg_pool3d", lambda: F.adaptive_avg_pool3d(vol(), 2)),
        ("F.pad_reflect", lambda: F.pad(img(), (1, 1, 1, 1), mode="reflect")),
        ("F.pad_replicate", lambda: F.pad(img(), (1, 1, 1, 1), mode="replicate")),
        ("F.pad_constant", lambda: F.pad(img(), (1, 1, 1, 1))),
        ("F.pad_circular", lambda: F.pad(img(), (1, 1, 1, 1), mode="circular")),
        ("F.mse_loss", lambda: F.mse_loss(T(4, 5, dt=dt), T(4, 5, dt=dt, seed=1))),
        ("F.l1_loss", lambda: F.l1_loss(T(4, 5, dt=dt), T(4, 5, dt=dt, seed=1))),
        ("F.smooth_l1_loss", lambda: F.smooth_l1_loss(T(4, 5, dt=dt), T(4, 5, dt=dt, seed=1))),
        ("F.cross_entropy", lambda: F.cross_entropy(T(4, 3, dt=dt), tgt)),
        ("F.nll_loss", lambda: F.nll_loss(T(4, 3, dt=dt), tgt)),
        ("F.kl_div", lambda: F.kl_div(T(4, 3, dt=dt), T(4, 3, dt=dt, pos=True), reduction="batchmean")),
        ("F.bce_with_logits", lambda: F.binary_cross_entropy_with_logits(T(4, 5, dt=dt), torch.sigmoid(T(4, 5, dt=dt, seed=1)))),
        ("F.gaussian_nll", lambda: F.gaussian_nll_loss(T(4, 5, dt=dt), T(4, 5, dt=dt, seed=1), T(4, 5, dt=dt, pos=True))),
        ("F.cosine_similarity", lambda: F.cosine_similarity(T(4, 5, dt=dt), T(4, 5, dt=dt, seed=1))),
        ("cdist", lambda: torch.cdist(T(4, 5, dt=dt), T(3, 5, dt=dt))),
        ("trace", lambda: torch.trace(T(4, 4, dt=dt))),
        ("prod", lambda: torch.prod(T(4, 5, dt=dt), 1)),
        ("Tensor.prod", lambda: T(4, 5, dt=dt).prod()),
        ("quantile", lambda: torch.quantile(T(4, 5, dt=dt), 0.3)),
        ("sum", lambda: torch.sum(T(4, 5, dt=dt))),
        ("cumsum", lambda: torch.cumsum(T(4, 5, dt=dt), 1)),
        ("cat_mixed", lambda: torch.cat([T(4, 5, dt=dt), T(4, 5)])),
        ("cat_same", lambda: torch.cat([T(4, 5, dt=dt), T(4, 5, dt=dt)])),
        ("stack_f64", lambda: torch.stack([T(4, 5, dt=dt), T(4, 5, dt=torch.float64)])),
        ("inverse", lambda: torch.inverse(SPD(3, dt))),
        ("linalg.inv", lambda: torch.linalg.inv(SPD(3, dt))),
        ("linalg.cholesky", lambda: torch.linalg.cholesky(SPD(3, dt))),
        ("linalg.svdvals", lambda: torch.linalg.svdvals(T(3, 3, dt=dt))),
        ("linalg.eigvalsh", lambda: torch.linalg.eigvalsh(SPD(3, dt))),
        ("linalg.solve", lambda: torch.linalg.solve(SPD(3, dt), T(3, 2, dt=dt))),
        ("linalg.pinv", lambda: torch.linalg.pinv(T(3, 2))),
        ("pinverse", lambda: torch.pinverse(T(3, 2))),
        ("nn.Linear", lambda: nn.Linear(5, 3)(T(4, 5, dt=dt)) if dt is torch.float32 else torch.zeros(1, dtype=dt)),
        ("nn.MHA", lambda: nn.MultiheadAttention(4, 2, batch_first=True)(T(1, 3, 4), T(1, 3, 4, seed=1), T(1, 3, 4, seed=2))[0]),
        ("nn.RNN", lambda: nn.RNN(5, 3, batch_first=True)(T(2, 4, 5))[0]),
        ("nn.GRUCell", lambda: nn.GRUCell(5, 3)(T(4, 5))),
        ("nn.LSTMCell", lambda: nn.LSTMCell(5, 3)(T(4, 5))[0]),
    ]


def run(dt, fast):
    out = []
    for name, fn in ops(dt):
        try:
            with torch.autocast("cpu", dtype=fast):
                r = fn()
            out.append(str(r.dtype).replace("torch.", ""))
        except Exception as e:
            out.append("E:" + type(e).__name__)
    return out


def policy_lines():
    """Each op's result dtype inside torch.autocast("cpu", dtype=fast) for
    float32 inputs (bfloat16, float16 regions), inputs of the region's own
    dtype, and float64 inputs; then cat's prioritize errors."""
    out = []
    names = [n for n, _ in ops(torch.float32)]
    rows = {}
    for dt in (torch.float32, torch.bfloat16, torch.float16, torch.float64):
        for fast in (torch.bfloat16, torch.float16):
            if dt in (torch.bfloat16, torch.float16) and dt is not fast:
                continue
            for n, r in zip(names, run(dt, fast)):
                rows.setdefault(n, []).append(r)
    for n in names:
        out.append(n + " " + " ".join(rows[n]))
    for fast, other in ((torch.bfloat16, torch.float16), (torch.float16, torch.bfloat16)):
        with torch.autocast("cpu", dtype=fast):
            for first, second in ((other, torch.float32), (torch.float32, other)):
                try:
                    out.append("cat %s %s %s" % (first, second, torch.cat([torch.ones(2, dtype=first), torch.ones(2, dtype=second)]).dtype))
                except RuntimeError as e:
                    out.append("cat %s %s RuntimeError %s" % (first, second, e))
    return out


def state_lines():
    """The autocast state through nesting, enabled=False, a nested dtype
    change, the decorator form, cache_enabled, and the ops a region
    leaves alone (float64, mv) or casts to float32 (prod)."""
    st = []
    st.append((torch.is_autocast_enabled("cpu"), str(torch.get_autocast_dtype("cpu"))))
    with torch.autocast("cpu"):
        st.append((torch.is_autocast_enabled("cpu"), str(torch.get_autocast_dtype("cpu")), str((T(2, 2) @ T(2, 2)).dtype)))
        with torch.autocast("cpu", enabled=False):
            st.append((torch.is_autocast_enabled("cpu"), str((T(2, 2) @ T(2, 2)).dtype)))
            with torch.autocast("cpu", dtype=torch.float16):
                st.append((torch.is_autocast_enabled("cpu"), str(torch.get_autocast_dtype("cpu")), str((T(2, 2) @ T(2, 2)).dtype)))
        st.append((str((T(2, 2) @ T(2, 2)).dtype), str(torch.get_autocast_dtype("cpu"))))
        st.append(str(torch.mm(T(2, 2, dt=torch.float64), T(2, 2, dt=torch.float64)).dtype))
        st.append(str(torch.addmm(T(2), T(2, 2), T(2, 2)).dtype))
        st.append(str(torch.mv(T(2, 2), T(2)).dtype))
        st.append(str(torch.prod(T(3, dt=torch.bfloat16)).dtype))
    st.append((torch.is_autocast_enabled("cpu"), str((T(2, 2) @ T(2, 2)).dtype)))

    @torch.autocast("cpu")
    def f(a, b):
        return a @ b

    st.append(str(f(T(2, 2), T(2, 2)).dtype))
    with torch.amp.autocast(device_type="cpu", cache_enabled=False):
        w = T(3, 3).requires_grad_()
        st.append((str((w @ w).dtype), torch.is_autocast_cache_enabled()))
    st.append(torch.is_autocast_cache_enabled())
    with torch.autocast("cpu", dtype=torch.float32):
        st.append((torch.is_autocast_enabled("cpu"), str((T(2, 2) @ T(2, 2)).dtype)))
    return [repr(x) for x in st]


def _flat(x):
    return [float(v) for v in x.detach().double().reshape(-1).tolist()]


def train_values():
    """One forward/backward of Linear-ReLU-Linear with cross_entropy, a
    Conv2d and an mse_loss inside torch.autocast("cpu") (bfloat16 and
    float16), a weight used twice (the cast cache), and the float32
    gradients that flow back through the casts."""
    out = {}
    for fast in (torch.bfloat16, torch.float16):
        tag = str(fast).replace("torch.", "")
        lin1, lin2, conv = nn.Linear(6, 5), nn.Linear(5, 3), nn.Conv2d(2, 3, 3)
        with torch.no_grad():
            for i, p in enumerate(list(lin1.parameters()) + list(lin2.parameters()) + list(conv.parameters())):
                p.copy_(T(*p.shape, seed=i + 3) * 0.3)
        x = T(4, 6, seed=1)
        img = T(2, 2, 5, 5, seed=2)
        tgt = torch.tensor([0, 2, 1, 1])
        with torch.autocast("cpu", dtype=fast):
            h = lin1(x)
            o = lin2(F.relu(h))
            loss = F.cross_entropy(o, tgt)
            c = conv(img)
            s = c.sum()
            mse = F.mse_loss(o, torch.zeros(4, 3))
            y = (lin1.weight @ T(6, 4)) + (lin1.weight @ T(6, 4, seed=5))
        out[tag + " dtypes"] = [str(t.dtype) for t in (h, o, loss, c, s, mse, y)]
        (loss + s * 0.01 + mse + y.float().sum() * 0.001).backward()
        out[tag + " values"] = _flat(loss) + _flat(s) + _flat(mse) + _flat(h) + _flat(o)
        for name, p in (("w1", lin1.weight), ("b1", lin1.bias), ("w2", lin2.weight), ("b2", lin2.bias), ("cw", conv.weight), ("cb", conv.bias)):
            out[tag + " grad " + name] = [str(p.grad.dtype)] + _flat(p.grad)
    return out
