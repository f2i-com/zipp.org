# Behaviour of the parallel wrappers, SyncBatchNorm, fractional pooling,
# CTC, grid sampling and padding-mode errors, uninitialized (lazy)
# parameters and dropout's kept values. Runs unchanged under PyTorch (whose
# output is api_expected.txt; gen.py hides CUDA) and Zipp.
import torch
import torch.nn as nn
import torch.nn.functional as F

try:
    import os
    import torch.distributed as dist
    os.environ.setdefault("MASTER_ADDR", "127.0.0.1")
    os.environ.setdefault("MASTER_PORT", "29534")
    # PyTorch's DDP needs a process group; Zipp's accepts none.
    dist.init_process_group("gloo", rank=0, world_size=1)
except ImportError:
    pass


def err(fn):
    try:
        fn()
        print("no error")
    except Exception as e:
        print(type(e).__name__)


def err_msg(fn):
    try:
        fn()
        print("no error")
    except Exception as e:
        print(type(e).__name__, str(e).split(" at 0x")[0].split(">")[0])


print("== DataParallel")
inner = nn.Sequential(nn.Linear(3, 2), nn.BatchNorm1d(2))
dp = nn.DataParallel(inner, device_ids=[0, 1], output_device=0)
print(dp)
print(dp.module is inner, dp.device_ids, nn.DataParallel is nn.parallel.DataParallel, isinstance(dp, nn.Module))
print(list(dp.state_dict().keys()))
print([n for n, _ in dp.named_parameters()])
x = torch.arange(12.0).reshape(4, 3) / 5 - 1
print(bool(torch.equal(dp(x), inner(x))))
err(lambda: dp.load_state_dict(nn.Sequential(nn.Linear(3, 2), nn.BatchNorm1d(2)).state_dict()))
print(dp.module.load_state_dict(nn.Sequential(nn.Linear(3, 2), nn.BatchNorm1d(2)).state_dict()))
plain = nn.Sequential(nn.Linear(3, 2), nn.BatchNorm1d(2))
plain.load_state_dict({k[len("module."):]: v for k, v in dp.state_dict().items()})
print(bool(torch.equal(plain[0].weight, inner[0].weight)))
dp.eval()
print(inner.training, dp.training)


class Kw(nn.Module):
    def __init__(self):
        super().__init__()
        self.lin = nn.Linear(2, 2)

    def forward(self, a, scale=1.0, shift=None):
        return self.lin(a) * scale + (0 if shift is None else shift)


kw = nn.DataParallel(Kw())
print(bool(torch.equal(kw(torch.ones(1, 2), scale=2.0, shift=1.0), kw.module(torch.ones(1, 2)) * 2.0 + 1.0)))

print("== DistributedDataParallel")
net = nn.Sequential(nn.Linear(3, 2), nn.BatchNorm1d(2))
ddp = nn.parallel.DistributedDataParallel(net)
print(ddp)
print(ddp.module is net, ddp.device_ids, ddp.output_device, ddp.dim, ddp.broadcast_buffers, ddp.find_unused_parameters, ddp.gradient_as_bucket_view, ddp.static_graph, ddp.device, ddp.device_type)
print(list(ddp.state_dict().keys()))
print(ddp.require_backward_grad_sync, ddp.require_forward_param_sync)
with ddp.no_sync():
    print(ddp.require_backward_grad_sync)
    out = ddp(x)
    out.sum().backward()
print(ddp.require_backward_grad_sync, bool(torch.equal(out, net(x))))
print(nn.parallel.DistributedDataParallel(nn.Linear(2, 2), find_unused_parameters=True, broadcast_buffers=False, static_graph=True).find_unused_parameters)
err(lambda: nn.parallel.DistributedDataParallel(nn.Linear(2, 2), device_ids=[0]))
err(lambda: nn.parallel.DistributedDataParallel(nn.Linear(2, 2), output_device=0))
err(lambda: nn.parallel.DistributedDataParallel(nn.Linear(2, 2).requires_grad_(False)))
roundtrip = nn.DataParallel(nn.Linear(3, 2))
roundtrip.load_state_dict(nn.parallel.DistributedDataParallel(nn.Linear(3, 2)).state_dict())
print("roundtrip ok")
try:
    dist.destroy_process_group()  # SyncBatchNorm refuses CPU input under one
except NameError:
    pass

print("== SyncBatchNorm")
sbn = nn.SyncBatchNorm(3, momentum=0.2)
print(sbn, sbn.process_group, isinstance(sbn, nn.BatchNorm2d))
err(lambda: sbn(torch.ones(3)))
err(lambda: nn.SyncBatchNorm(0)(torch.ones(2, 0, 3)))
print(tuple(sbn(torch.ones(4, 3)).shape), tuple(sbn(torch.ones(2, 3, 2, 2, 2)).shape), sbn.num_batches_tracked.item())
seq = nn.Sequential(nn.Conv2d(1, 2, 1), nn.BatchNorm2d(2), nn.Sequential(nn.BatchNorm1d(4, affine=False)), nn.InstanceNorm2d(2))
seq[1].eval()
bn_weight = seq[1].weight
conv = nn.SyncBatchNorm.convert_sync_batchnorm(seq, process_group="pg")
print(conv is seq, [type(m).__name__ for m in conv.modules()])
print(conv[1].weight is bn_weight, conv[1].training, conv[1].process_group, conv[2][0].affine, conv[2][0].weight)
single = nn.SyncBatchNorm.convert_sync_batchnorm(nn.BatchNorm3d(5))
print(type(single).__name__, single.num_features, list(single.state_dict().keys()))

print("== fractional max pooling")
print(nn.FractionalMaxPool2d(3, output_ratio=0.5), nn.FractionalMaxPool3d(2, output_size=4).output_size)
fm = nn.FractionalMaxPool2d(2, output_size=(3, 3))
print(list(fm.state_dict().keys()), list(nn.FractionalMaxPool2d(2, output_size=3, _random_samples=torch.rand(1, 1, 2)).state_dict().keys()))
out = fm(torch.randn(2, 3, 7, 7))
print(tuple(out.shape))
out, idx = F.fractional_max_pool3d(torch.randn(3, 5, 6, 7), 2, output_ratio=(0.5, 0.5, 0.5), return_indices=True)
print(tuple(out.shape), idx.dtype)
err(lambda: nn.FractionalMaxPool2d(2))
err(lambda: nn.FractionalMaxPool2d(2, output_size=3, output_ratio=0.5))
err(lambda: nn.FractionalMaxPool2d(2, output_ratio=1.5))
err(lambda: nn.FractionalMaxPool3d(0, output_size=2))
err(lambda: F.fractional_max_pool2d(torch.ones(1, 1, 6, 6), 2, output_ratio=0.5))
err(lambda: F.fractional_max_pool2d(torch.ones(1, 1, 6, 6), 2))
err(lambda: F.fractional_max_pool2d(torch.ones(1, 1, 6, 6), 4, output_size=4))
err(lambda: F.fractional_max_pool2d(torch.ones(6, 6), 2, output_size=2))
x5 = torch.arange(36.0).reshape(1, 1, 6, 6)
o, i = F.fractional_max_pool2d(x5, 2, output_size=(3, 3), return_indices=True, _random_samples=torch.tensor([[[0.25, 0.75]]]))
print(o.reshape(-1).tolist(), i.reshape(-1).tolist())

print("== CTC")
print(nn.CTCLoss(), nn.CTCLoss(blank=2, zero_infinity=True).zero_infinity)
lp = torch.randn(5, 2, 4).log_softmax(2)
tg = torch.tensor([[1, 2], [3, 3]])
err(lambda: F.ctc_loss(lp, tg, (6, 5), (2, 2)))
err(lambda: F.ctc_loss(lp, tg, torch.tensor([5.0, 5.0]), torch.tensor([2, 2])))
err(lambda: F.ctc_loss(lp, tg, (5, 5), (2, 2), blank=4))
err(lambda: F.ctc_loss(lp, tg, (5, 5, 5), (2, 2, 2)))
err(lambda: F.ctc_loss(lp, tg, (5, 5), (3, 2)))
err(lambda: F.ctc_loss(lp, torch.tensor([1, 2, 3]), (5, 5), (2, 2)))
err(lambda: F.ctc_loss(lp, tg, (5, 5), (2, 2), reduction="avg"))
print(F.ctc_loss(lp, tg, (5, 5), (2, 2), reduction="none").shape, F.ctc_loss(lp, tg, (5, 5), (2, 2)).shape)

print("== grid_sample and affine_grid")
img = torch.randn(1, 2, 3, 3)
err(lambda: F.grid_sample(img, torch.zeros(1, 2, 2, 2), mode="area", align_corners=False))
err(lambda: F.grid_sample(img, torch.zeros(1, 2, 2, 2), padding_mode="wrap", align_corners=False))
err(lambda: F.grid_sample(torch.randn(1, 1, 2, 2, 2), torch.zeros(1, 1, 1, 1, 3), mode="bicubic", align_corners=False))
err(lambda: F.grid_sample(img, torch.zeros(1, 2, 2, 3), align_corners=False))
err(lambda: F.grid_sample(img, torch.zeros(2, 2, 2, 2), align_corners=False))
err(lambda: F.grid_sample(img, torch.zeros(1, 2, 2), align_corners=False))
err(lambda: F.grid_sample(img, torch.zeros(1, 2, 2, 2, dtype=torch.float64), align_corners=False))
err(lambda: F.affine_grid(torch.zeros(1, 2, 3, dtype=torch.int64), (1, 1, 2, 2), align_corners=False))
err(lambda: F.affine_grid(torch.zeros(1, 3, 3), (1, 1, 2, 2), align_corners=False))
err(lambda: F.affine_grid(torch.zeros(1, 2, 3), (1, 1, 2), align_corners=False))
err(lambda: F.affine_grid(torch.zeros(1, 2, 3), (1, 1, 0, 2), align_corners=False))
g = F.affine_grid(torch.tensor([[[1.0, 0.0, 0.0], [0.0, 1.0, 0.0]]]), (1, 1, 2, 3), align_corners=True)
print(g.reshape(-1).tolist())
print(F.grid_sample(torch.arange(4.0).reshape(1, 1, 2, 2), g, align_corners=True).reshape(-1).tolist())

print("== convolution padding modes")
print(nn.Conv2d(2, 3, 3, padding=1, padding_mode="reflect"))
print(nn.Conv1d(2, 3, 3, padding=1, padding_mode="circular").padding_mode, nn.Conv3d(1, 1, 3, padding=1, padding_mode="replicate")._reversed_padding_repeated_twice)
err(lambda: nn.Conv2d(2, 3, 3, padding_mode="mirror"))
err(lambda: nn.ConvTranspose2d(2, 3, 3, padding_mode="reflect"))
err(lambda: nn.Conv2d(1, 1, 3, padding=3, padding_mode="reflect")(torch.ones(1, 1, 3, 3)))
print(nn.Conv2d(1, 1, 3, padding=1, padding_mode="circular", bias=False)(torch.ones(1, 1, 3, 3)).shape)

print("== second derivatives through convolutions")
c2 = nn.Conv2d(1, 1, 2, bias=False)
with torch.no_grad():
    c2.weight.copy_(torch.tensor([[[[1.0, -2.0], [0.5, 3.0]]]]))
xin = torch.arange(9.0).reshape(1, 1, 3, 3).requires_grad_()
gx, = torch.autograd.grad((c2(xin) ** 2).sum(), xin, create_graph=True)
print(gx.requires_grad, gx.reshape(-1).tolist())
(gx ** 2).sum().backward()
print(xin.grad.reshape(-1).tolist(), c2.weight.grad.reshape(-1).tolist())

print("== uninitialized parameters")
lazy = nn.LazyLinear(3)
w = lazy.weight
err_msg(lambda: w + 1)
err_msg(lambda: 2 * w)
err_msg(lambda: w.sum())
err_msg(lambda: w.view(-1))
err_msg(lambda: w.numel())
err_msg(lambda: w.detach())
err_msg(lambda: -w)
err_msg(lambda: w.clone())
err_msg(lambda: w.dim())
err_msg(lambda: w[0])
err_msg(lambda: w.tolist())
err_msg(lambda: w.requires_grad_(False))
err_msg(lambda: nn.init.uniform_(w))
err_msg(lambda: nn.init.kaiming_uniform_(w))
for name in ("constant_", "zeros_", "ones_", "normal_", "kaiming_normal_", "xavier_uniform_", "xavier_normal_", "trunc_normal_", "eye_", "dirac_", "orthogonal_", "sparse_"):
    err_msg(lambda: getattr(nn.init, name)(w, *{"constant_": (1.0,), "sparse_": (0.5,)}.get(name, ())))
err_msg(lambda: nn.parameter.UninitializedBuffer() * 3)
err_msg(lambda: w.shape)
err(lambda: nn.utils.parameters_to_vector(lazy.parameters()))
print(w.size(), w.dtype, w.requires_grad, w.is_floating_point(), type(w.float()).__name__)
lazy.double()
print(type(lazy.weight).__name__, lazy.weight.dtype, lazy.weight is w)
print(lazy(torch.ones(2, 4, dtype=torch.float64)).dtype, type(lazy.weight).__name__, lazy.weight is w, tuple(w.shape))
half = nn.LazyBatchNorm1d().to(torch.float64)
print(type(half.running_mean).__name__, half.running_mean.dtype, half(torch.ones(3, 2, dtype=torch.float64)).dtype)

print("== dropout keeps x * (1 / (1 - p)) rounded as PyTorch's CPU kernel")
xs = (torch.arange(2000) * 7919 % 10007).float() / 1024 - 4.0


def kept_values(fn, x):
    """Each element's value where some run keeps it (all runs must agree)."""
    vals = [None] * x.numel()
    flat_x = x.reshape(-1).tolist()
    for _ in range(120):
        y = fn(x).reshape(-1).tolist()
        for i, v in enumerate(y):
            if v != 0 or flat_x[i] == 0:
                if vals[i] is not None and vals[i] != v:
                    return "inconsistent"
                vals[i] = v
        if all(v is not None for v in vals):
            break
    if any(v is None for v in vals):
        return "unkept"
    digest = 0.0
    for i, v in enumerate(vals):
        digest += v * (i % 97 + 1)
    return repr(digest) + " " + " ".join(repr(v) for v in vals[:4])


for p in (0.1, 0.3, 0.7):
    print(p, kept_values(lambda t: F.dropout(t, p), xs))
print("float64", kept_values(lambda t: F.dropout(t, 0.3), xs.double()))
print("module", kept_values(nn.Dropout(0.2 + 0.1), xs))
print("inplace", kept_values(lambda t: F.dropout(t.clone(), 0.1, inplace=True), xs))
print("dropout1d", kept_values(lambda t: F.dropout1d(t, 0.3), xs.reshape(40, 50, 1)))
print("dropout2d", kept_values(lambda t: F.dropout2d(t, 0.7), xs.reshape(4, 50, 10, 1)))
print("eval", bool(torch.equal(F.dropout(xs, 0.3, training=False), xs)))
