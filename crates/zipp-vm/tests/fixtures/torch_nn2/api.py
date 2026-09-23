# Behaviour of lazy modules, the parametrization API, spectral norm,
# state_dict hooks and the new layers' reprs and errors. Runs unchanged
# under PyTorch (whose output is api_expected.txt) and Zipp.
import torch
import torch.nn as nn
import torch.nn.functional as F
from torch.nn.utils import parametrize
from torch.nn.utils import parametrizations as P


def show(t):
    return [round(float(v), 4) + 0.0 for v in t.detach().reshape(-1).tolist()]


def fill(m, scale=1.0):
    with torch.no_grad():
        for i, (name, p) in enumerate(m.named_parameters()):
            n = p.numel()
            p.copy_((torch.arange(n, dtype=p.dtype).reshape(p.shape) % 7 - 3) * 0.1 * scale + 0.05 * i)


def close(a, b):
    return bool((a - b).abs().max().item() < 1e-4)


def err(fn):
    try:
        fn()
        print("no error")
    except Exception as e:
        print(type(e).__name__)


x = torch.arange(10.0).reshape(2, 5) / 10 - 0.3
x3 = torch.arange(6.0).reshape(2, 3) / 5 - 0.4

print("== lazy linear")
m = nn.LazyLinear(3)
print(m)
print(m.has_uninitialized_params(), type(m.weight).__name__, repr(m.weight), nn.parameter.is_lazy(m.weight))
print(isinstance(m.weight, nn.Parameter), isinstance(m.weight, nn.UninitializedParameter), isinstance(m.bias, nn.parameter.UninitializedTensorMixin))
sd = m.state_dict()
print(list(sd.keys()), [type(v).__name__ for v in sd.values()])
err(lambda: m.weight.shape)
err(lambda: m.weight.numel())
y = m(x)
print(type(m).__name__, tuple(m.weight.shape), tuple(m.bias.shape), m.in_features, type(m.weight).__name__, tuple(y.shape))
print(m)
print(hasattr(m, "_initialize_hook"), hasattr(m, "has_uninitialized_params"), len(m._forward_pre_hooks), len(m._load_state_dict_pre_hooks))

print("== lazy load")
src = nn.Linear(5, 3)
fill(src)
lazy = nn.LazyLinear(3)
print(lazy.load_state_dict(src.state_dict()))
print(type(lazy).__name__, tuple(lazy.weight.shape), lazy.in_features, lazy.has_uninitialized_params())
print(close(lazy(x), src(x)), show(lazy(x)))
print(type(lazy).__name__, lazy.in_features)
err(lambda: nn.Linear(5, 3).load_state_dict(nn.LazyLinear(3).state_dict()))
bn_src = nn.BatchNorm1d(2)
bn_src(torch.arange(8.0).reshape(4, 2))
lazy_bn = nn.LazyBatchNorm1d()
lazy_bn.load_state_dict(bn_src.state_dict())
print(type(lazy_bn).__name__, show(lazy_bn.running_mean), show(lazy_bn.running_var), lazy_bn.num_batches_tracked.item(), lazy_bn.num_features)

print("== lazy training")
net = nn.Sequential(nn.LazyLinear(4), nn.Tanh(), nn.LazyLinear(1))
print(net)
early = torch.optim.SGD(net.parameters(), lr=0.1)
net(x)
fill(net)
print([type(layer).__name__ for layer in net], [tuple(p.shape) for p in net.parameters()])
opt = torch.optim.SGD(net.parameters(), lr=0.1, momentum=0.9)
target = torch.tensor([[0.5], [-0.5]])
for step in range(4):
    opt.zero_grad()
    loss = F.mse_loss(net(x), target)
    loss.backward()
    opt.step()
    print(round(loss.item(), 5))
print(all(a is b for a, b in zip(early.param_groups[0]["params"], net.parameters())))

print("== lazy conv and norm")
c = nn.LazyConv2d(4, 3, padding=1)
print(c)
c(torch.zeros(1, 2, 5, 5))
print(c, tuple(c.weight.shape), c.in_channels)
ct = nn.LazyConvTranspose3d(4, 2, groups=2)
ct(torch.zeros(1, 4, 2, 2, 2))
print(ct, tuple(ct.weight.shape))
err(lambda: nn.LazyConv2d(2, 3)(torch.zeros(5)))
err(lambda: nn.LazyConv2d(2, 3, groups=2)(torch.zeros(1, 3, 4, 4)))
b = nn.LazyBatchNorm2d()
print(b)
print(list(b.state_dict().keys()), [type(v).__name__ for v in b.state_dict().values()])
b(torch.arange(24.0).reshape(2, 3, 2, 2))
print(b, show(b.running_mean), show(b.running_var), b.num_batches_tracked.item())
i = nn.LazyInstanceNorm1d()
print(i, i.has_uninitialized_params())
i(torch.arange(24.0).reshape(2, 3, 4))
print(type(i).__name__, i, show(i.running_mean))

print("== buffer")
buf = nn.Buffer(torch.ones(2), persistent=False)
print(isinstance(buf, nn.Buffer), isinstance(torch.ones(2), nn.Buffer), isinstance(buf, torch.Tensor), buf.requires_grad)


class WithBuffers(nn.Module):
    def __init__(self):
        super().__init__()
        self.a = nn.Buffer(torch.arange(3.0))
        self.b = nn.Buffer(torch.ones(1), persistent=False)


wb = WithBuffers()
print(list(wb.state_dict().keys()), [k for k, _ in wb.named_buffers()], isinstance(wb.a, nn.Buffer))
import copy
wb2 = copy.deepcopy(wb)
print(isinstance(wb2.a, nn.Buffer), wb2.a.tolist(), wb2.a is not wb.a, list(wb2.state_dict().keys()))


print("== parametrize")


class Symmetric(nn.Module):
    def forward(self, X):
        return X.triu() + X.triu(1).transpose(-1, -2)

    def right_inverse(self, A):
        return A.triu()


class Scale(nn.Module):
    def __init__(self, s):
        super().__init__()
        self.s = s

    def forward(self, X):
        return X * self.s

    def right_inverse(self, X):
        return X / self.s


calls = [0]


class Counting(nn.Module):
    def forward(self, X):
        calls[0] += 1
        return X


class Flat(nn.Module):
    def forward(self, X):
        return X.reshape(-1)


m = nn.Linear(3, 3)
fill(m)
w0 = m.weight
parametrize.register_parametrization(m, "weight", Symmetric())
print(type(m).__name__, parametrize.is_parametrized(m), parametrize.is_parametrized(m, "weight"), parametrize.is_parametrized(m, "bias"))
print(parametrize.type_before_parametrizations(m).__name__, isinstance(m, nn.Linear))
print(m.parametrizations.weight.original is w0)
print(show(m.weight))
print(list(m.state_dict().keys()), [n for n, _ in m.named_parameters()])
print(m)
m.weight = torch.eye(3) * 2
print(show(m.parametrizations.weight.original), show(m.weight))
parametrize.register_parametrization(m, "weight", Scale(2.0))
print(show(m.weight), len(m.parametrizations.weight))
parametrize.register_parametrization(m, "bias", Counting())
with parametrize.cached():
    first = m.bias
    second = m.bias
print(calls[0], first is second)
first = m.bias
second = m.bias
print(calls[0])
m(torch.ones(1, 3)).sum().backward()
print(show(m.parametrizations.weight.original.grad), show(m.parametrizations.bias.original.grad))
print(list(m.state_dict().keys()))
m2 = nn.Linear(3, 3)
parametrize.register_parametrization(m2, "weight", Symmetric())
parametrize.register_parametrization(m2, "weight", Scale(2.0))
parametrize.register_parametrization(m2, "bias", Counting())
print(m2.load_state_dict(m.state_dict()), show(m2.weight) == show(m.weight))
parametrize.remove_parametrizations(m, "bias", leave_parametrized=False)
parametrize.remove_parametrizations(m, "weight")
print(type(m).__name__, [n for n, _ in m.named_parameters()], show(m.weight), hasattr(m, "parametrizations"), m.weight is w0)
err(lambda: parametrize.remove_parametrizations(m, "weight"))
err(lambda: parametrize.register_parametrization(nn.Linear(2, 3), "weight", Flat()))
err(lambda: parametrize.register_parametrization(nn.Linear(2, 3), "nothing", Flat()))
m3 = nn.Linear(2, 3)
parametrize.register_parametrization(m3, "weight", Flat(), unsafe=True)
print(tuple(m3.weight.shape))
bn = nn.BatchNorm1d(2)
parametrize.register_parametrization(bn, "running_mean", Scale(3.0))
print(list(bn.state_dict().keys()), type(bn).__name__)

print("== parametrizations")
sn = P.spectral_norm(nn.Linear(3, 2))
print(list(sn.state_dict().keys()), [n for n, _ in sn.named_parameters()])
print(sn.parametrizations.weight[0].n_power_iterations, sn.parametrizations.weight[0].dim, sn.parametrizations.weight[0].eps)
sn1 = P.spectral_norm(nn.Linear(4, 1))
print(round(float(sn1.weight.norm()), 4))
snt = P.spectral_norm(nn.ConvTranspose2d(2, 3, 2))
print(snt.parametrizations.weight[0].dim, tuple(snt.parametrizations.weight[0]._u.shape), tuple(snt.parametrizations.weight[0]._v.shape))
sn_bias = P.spectral_norm(nn.Linear(3, 2), name="bias")
print(show(sn_bias.bias.norm()))
wn = P.weight_norm(nn.Linear(3, 2))
fill(wn)
print(list(wn.state_dict().keys()), [tuple(v.shape) for v in wn.state_dict().values()])
old = nn.utils.weight_norm(nn.Linear(3, 2))
fill(old)
new = P.weight_norm(nn.Linear(3, 2))
print(new.load_state_dict(old.state_dict()))
print(close(new(x3), old(x3)), show(new(x3)))
orth = nn.Linear(3, 3)
fill(orth)
P.orthogonal(orth)
q = orth.weight
print(list(orth.state_dict().keys()), show(q @ q.t()), show(q))
orth.weight = torch.tensor([[0.0, 1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, -1.0]])
print(show(orth.weight))
tall = P.orthogonal(nn.Linear(2, 4))
q = tall.weight
print(tuple(q.shape), show(q.t() @ q), tall.parametrizations.weight[0].orthogonal_map)
wide = P.orthogonal(nn.Linear(4, 2), orthogonal_map="householder", use_trivialization=False)
fill(wide)
q = wide.weight
print(list(wide.state_dict().keys()), show(q @ q.t()))
err(lambda: P.orthogonal(nn.Linear(3, 3), orthogonal_map="nope"))
err(lambda: P.orthogonal(nn.Linear(3, 3), orthogonal_map="matrix_exp", use_trivialization=False))

print("== legacy spectral norm")
sn = nn.utils.spectral_norm(nn.Linear(3, 2))
print([n for n, _ in sn.named_parameters()], [n for n, _ in sn.named_buffers()], list(sn.state_dict().keys()))
print(type(sn.weight).__name__, isinstance(sn.weight, nn.Parameter))
err(lambda: nn.utils.spectral_norm(sn))
err(lambda: nn.utils.remove_spectral_norm(nn.Linear(2, 2)))
fill(sn)
with torch.no_grad():
    sn.weight_u.copy_(torch.tensor([0.6, 0.8]))
    sn.weight_v.copy_(torch.tensor([1.0, 0.0, 0.0]))
sn.eval()
sn(x3)
print(show(sn.weight_u), show(sn.weight_v), show(sn.weight))
sn.train()
sn(x3)
print(show(sn.weight_u), show(sn.weight_v), show(sn.weight))
nn.utils.remove_spectral_norm(sn)
print(list(sn.state_dict().keys()), len(sn._forward_pre_hooks), len(sn._state_dict_hooks), len(sn._load_state_dict_pre_hooks), show(sn.weight))
conv = nn.utils.spectral_norm(nn.ConvTranspose1d(2, 3, 2))
print(tuple(conv.weight_u.shape), tuple(conv.weight_v.shape))

print("== state dict hooks")
h = nn.Linear(2, 2)
h.register_state_dict_pre_hook(lambda mod, prefix, keep_vars: print("pre", repr(prefix), keep_vars))


def post(mod, sd, prefix, meta):
    sd[prefix + "extra"] = torch.zeros(1)


h.register_state_dict_post_hook(post)
print(list(h.state_dict().keys()))
seq = nn.Sequential(h)
print(list(seq.state_dict(keep_vars=True).keys()))
h2 = nn.Linear(2, 2)


def load_pre(mod, sd, prefix, meta, strict, missing, unexpected, errors):
    sd.pop(prefix + "extra", None)
    print("load pre", repr(prefix), sorted(sd.keys()))


h2.register_load_state_dict_pre_hook(load_pre)
h2.register_load_state_dict_post_hook(lambda mod, inc: print("load post", inc.missing_keys, inc.unexpected_keys))
print(h2.load_state_dict(h.state_dict()))
handle = h2.register_load_state_dict_post_hook(lambda mod, inc: inc.missing_keys.append("fake"))
err(lambda: h2.load_state_dict(h.state_dict()))
handle.remove()
print(h2.load_state_dict(h.state_dict(), strict=False))

print("== 3-D layers")
print(nn.Conv3d(2, 4, 3, stride=2, padding=1, groups=2))
print(nn.Conv3d(2, 4, (1, 2, 3), padding="same", bias=False))
print(nn.ConvTranspose3d(2, 4, (1, 2, 3), stride=(1, 2, 1), output_padding=(0, 1, 0)))
print(nn.MaxPool3d(2))
print(nn.MaxPool3d((1, 2, 3), stride=1, padding=(0, 1, 1), dilation=2, ceil_mode=True))
print(nn.AvgPool3d(3, 2, 1))
print(nn.AdaptiveAvgPool3d((1, None, 3)))
print(nn.AdaptiveMaxPool3d(2))
print(nn.MaxUnpool3d(2))
print(nn.LPPool3d(2, 3))
print(nn.ReflectionPad3d(1), nn.ReplicationPad3d((1, 0, 1, 0, 1, 0)), nn.CircularPad3d(2), nn.ZeroPad3d(1), nn.ConstantPad3d(1, 2.0))
print(nn.MultiLabelMarginLoss(), nn.Dropout3d(0.2), nn.Upsample(scale_factor=2, mode="trilinear"))
err(lambda: F.conv3d(torch.zeros(1, 2, 3, 3), torch.zeros(1, 2, 1, 1, 1)))
err(lambda: F.conv3d(torch.zeros(1, 2, 3), torch.zeros(1, 2, 1, 1, 1)))
err(lambda: nn.Conv3d(2, 2, 3, stride=2, padding="same"))
err(lambda: nn.ConvTranspose3d(2, 2, 2, output_padding=1)(torch.zeros(1, 2, 2, 2, 2)))
err(lambda: F.max_pool3d(torch.zeros(1, 1, 2, 2), 2))
err(lambda: nn.MaxPool3d(2, padding=2)(torch.zeros(1, 1, 4, 4, 4)))
err(lambda: F.interpolate(torch.zeros(1, 1, 4, 4), size=2, mode="nearest", antialias=True))
err(lambda: F.interpolate(torch.zeros(1, 1, 2, 4, 4), size=2, mode="bicubic"))
err(lambda: F.interpolate(torch.zeros(1, 1, 4), size=2, mode="bilinear", antialias=True))
v, idx = F.max_pool3d(torch.arange(64.0).reshape(1, 1, 4, 4, 4), 2, return_indices=True)
print(show(v), idx.reshape(-1).tolist())
print(show(F.max_unpool3d(v, idx, 2).sum((2, 3))))
print(tuple(F.interpolate(torch.zeros(1, 1, 5, 7), scale_factor=0.5, mode="bicubic", antialias=True).shape), tuple(F.interpolate(torch.zeros(1, 1, 5, 7), scale_factor=1.5, mode="bicubic").shape))

print("== nested tensor fast path")
layer = nn.TransformerEncoderLayer(8, 2, 16, dropout=0.0, batch_first=True)
enc = nn.TransformerEncoder(layer, 2, norm=nn.LayerNorm(8))
fill(enc, 0.3)
xx = (torch.arange(64.0).reshape(2, 4, 8) % 5 - 2) / 3
pad = torch.tensor([[False, False, True, True], [False, False, False, False]])
enc.eval()
with torch.no_grad():
    fast = enc(xx, src_key_padding_mask=pad)
slow = enc(xx, src_key_padding_mask=pad)
print(enc.use_nested_tensor, layer.activation_relu_or_gelu, show(fast[0, 2]), show(fast[0, 3]))
print(close(fast[0, :2], slow[0, :2]), close(fast[1], slow[1]), close(slow[0, 2], fast[0, 2]))
with torch.no_grad():
    unaligned = enc(xx, src_key_padding_mask=torch.tensor([[True, False, False, False], [False, False, False, False]]))
    unmasked = enc(xx)
print(close(unaligned[0, 0], fast[0, 2]), close(unmasked[1], fast[1]))
print(nn.TransformerEncoder(nn.TransformerEncoderLayer(8, 2, 16), 1).use_nested_tensor, nn.TransformerEncoder(nn.TransformerEncoderLayer(8, 2, 16, batch_first=True, norm_first=True), 1).use_nested_tensor, nn.TransformerEncoder(nn.TransformerEncoderLayer(8, 2, 16, batch_first=True, activation="gelu"), 1, enable_nested_tensor=False).use_nested_tensor)

print("== deepcopy")
import copy
m = nn.LazyLinear(3)
m2 = copy.deepcopy(m)
m2(torch.ones(2, 4))
print(type(m).__name__, type(m2).__name__, m.has_uninitialized_params())
m3 = copy.deepcopy(m)
m3.load_state_dict(nn.Linear(5, 3).state_dict())
print(type(m).__name__, m.has_uninitialized_params(), tuple(m3.weight.shape), type(m3.weight).__name__)
p = P.spectral_norm(nn.Linear(3, 2))
p2 = copy.deepcopy(p)
print(type(p2).__name__, list(p2.state_dict().keys()), p2.parametrizations.weight.original is p.parametrizations.weight.original)
s = nn.utils.spectral_norm(nn.Linear(3, 2))
s2 = copy.deepcopy(s)
s2(torch.ones(1, 3))
print(torch.equal(s.weight_u, s2.weight_u), close(s2.weight_orig, s.weight_orig))

print("== legacy spectral norm, pre-v1 checkpoint")
sn = nn.utils.spectral_norm(nn.Linear(2, 3))
fill(sn)
w_orig = torch.tensor([[1.0, 2.0], [0.5, -1.0], [3.0, 0.25]])
old_state = {"bias": torch.tensor([0.1, -0.2, 0.3]), "weight_orig": w_orig, "weight_u": torch.tensor([0.6, 0.8, 0.0]), "weight": w_orig / 2.5}
print(sn.load_state_dict(old_state))
print(show(sn.weight_v), show(sn.weight_u))
sn.eval()
print(show(sn(x3[:, :2])))
