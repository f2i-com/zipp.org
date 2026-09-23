# torch.nn behaviour that is not a number: registries, state_dict rules,
# containers, repr, hooks, errors and init properties. Runs unchanged on
# CPython + PyTorch 2.11 (whose output is api_expected.txt) and on Zipp.
import copy
import math
import torch
import torch.nn as nn
import torch.nn.functional as F
import torch.nn.init as init
from collections import OrderedDict
from torch.nn.utils.rnn import pack_padded_sequence, pad_packed_sequence, pad_sequence, pack_sequence


def r(v, n=4):
    if isinstance(v, torch.Tensor):
        v = v.tolist()
    if isinstance(v, (list, tuple)):
        return [r(x, n) for x in v]
    if isinstance(v, float):
        v = round(v, n)
        return 0.0 if v == 0 else v
    return v


def attempt(label, fn):
    try:
        print(label, fn())
    except Exception as e:
        print(label, type(e).__name__, e.args[0] if e.args else "")


# ---- __setattr__ keeps the registries in step (bug 3) ----
class RunningMean(nn.Module):
    def __init__(self):
        super().__init__()
        self.register_buffer("running_mean", torch.zeros(2))
        self.register_buffer("cache", torch.zeros(2), persistent=False)

    def forward(self, x):
        self.running_mean = 0.9 * self.running_mean + 0.1 * x.mean(0)
        return x


m = RunningMean()
m(torch.tensor([[1.0, 2.0], [3.0, 4.0]]))
print("buffer reassigned", r(m.running_mean), r(m.state_dict()["running_mean"]), "running_mean" in m.__dict__)
print("non-persistent keys", list(m.state_dict().keys()), [n for n, _ in m.named_buffers()])
print("load without non-persistent", m.load_state_dict({"running_mean": torch.ones(2)}))
m.cache = None
print("buffer None", list(m._buffers.keys()), m.cache)
attempt("buffer bad type", lambda: setattr(m, "running_mean", 3))
del m.cache
print("del buffer", list(m._buffers.keys()), hasattr(m, "cache"))

w = nn.Module()
w.p = None
w.p = nn.Parameter(torch.ones(1))
print("attr then param", [n for n, _ in w.named_parameters()], "p" in w.__dict__, type(w.p).__name__)
w.sub = None
w.sub = nn.Linear(1, 1)
print("attr then module", [n for n, _ in w.named_children()], "sub" in w.__dict__)
w.p = None
print("param None", list(w._parameters.keys()), w.p, [n for n, _ in w.named_parameters()])
attempt("param bad type", lambda: setattr(w, "p", torch.ones(1)))
w.sub = None
print("module None", list(w._modules.keys()), [n for n, _ in w.named_children()])
attempt("module bad type", lambda: setattr(w, "sub", 3))
w.q = nn.Parameter(torch.zeros(2))
try:
    w.q = nn.Linear(2, 2)
except TypeError as e:
    print("param replaced by module", type(e).__name__, e.args[0].startswith("cannot assign '") and e.args[0].endswith("Linear' as parameter 'q' (torch.nn.Parameter or None expected)"))
w.lin2 = nn.Linear(2, 2)
w.lin2 = nn.Parameter(torch.zeros(1))
print("module replaced by param", list(w._parameters.keys()), list(w._modules.keys()))
seq = nn.Sequential(nn.Linear(1, 1), nn.ReLU())
seq.extra = nn.Tanh()
seq.extra = None
print("sequential None child", [n for n, _ in seq.named_children()], len(seq))

# ---- registration checks (bug 15) ----
v = nn.Module()
v.plain = 1
attempt("buffer dotted", lambda: v.register_buffer("a.b", torch.zeros(1)))
attempt("buffer empty", lambda: v.register_buffer("", torch.zeros(1)))
attempt("buffer existing attr", lambda: v.register_buffer("plain", torch.zeros(1)))
attempt("buffer non-string", lambda: v.register_buffer(3, torch.zeros(1)))
attempt("buffer non-tensor", lambda: v.register_buffer("b", [1]))
attempt("param dotted", lambda: v.register_parameter("a.b", nn.Parameter(torch.zeros(1))))
attempt("param plain tensor", lambda: v.register_parameter("c", torch.zeros(1)))
attempt("param existing attr", lambda: v.register_parameter("plain", nn.Parameter(torch.zeros(1))))
attempt("param non-leaf", lambda: v.register_parameter("d", nn.Parameter(torch.zeros(1)) * 2))
attempt("module non-module", lambda: v.add_module("e", 3))
v.register_buffer("nb", None)
print("buffer registered None", list(v._buffers.keys()), list(v.state_dict().keys()))

# ---- traversal (bugs 11, 12) ----
net = nn.Sequential(nn.Linear(2, 2), nn.Sequential(nn.ReLU(), nn.Linear(2, 1)))
print("named_modules", [n for n, _ in net.named_modules()])
print("named_modules prefix", [n for n, _ in net.named_modules(prefix="net")])
print("named_parameters prefix", [n for n, _ in net.named_parameters(prefix="net")])
print("named_buffers prefix", [n for n, _ in nn.Sequential(nn.BatchNorm1d(2)).named_buffers(prefix="b")])
lin = nn.Linear(2, 2)
shared = nn.Sequential(lin, nn.ReLU(), lin)
print("shared modules", [n for n, _ in shared.named_modules()], len(list(shared.modules())), len(list(shared.children())), len(list(shared.parameters())))
print("shared no dedupe", [n for n, _ in shared.named_modules(remove_duplicate=False)], [n for n, _ in shared.named_parameters(remove_duplicate=False)])
print("shared state_dict", list(shared.state_dict().keys()))
print("non-recursive", [n for n, _ in net.named_parameters(recurse=False)], len(list(net[0].parameters(recurse=False))))

# ---- state_dict (bugs 4, 13) ----
kv = nn.Sequential(nn.Linear(2, 2))
sd = kv.state_dict(keep_vars=True)
print("keep_vars nested", sd["0.weight"] is kv[0].weight, sd["0.weight"].requires_grad, kv.state_dict()["0.weight"].requires_grad)
lin = nn.Linear(2, 3)
attempt("load missing", lambda: lin.load_state_dict({"weight": torch.zeros(3, 2)}))
attempt("load unexpected", lambda: lin.load_state_dict({"weight": torch.zeros(3, 2), "bias": torch.zeros(3), "extra": torch.zeros(1)}))
attempt("load size mismatch", lambda: lin.load_state_dict({"weight": torch.zeros(2, 2), "bias": torch.zeros(3)}))
res = lin.load_state_dict({"weight": torch.ones(3, 2), "extra": torch.zeros(1)}, strict=False)
print("load non-strict", res.missing_keys, res.unexpected_keys, r(lin.weight.sum().item()))
missing, unexpected = res
print("incompatible keys tuple", missing, unexpected)
bn = nn.BatchNorm1d(2)
sd = {k: v for k, v in bn.state_dict().items() if k != "num_batches_tracked"}
print("bn old checkpoint", bn.load_state_dict(sd))
a = nn.Linear(2, 2)
opt = torch.optim.SGD(a.parameters(), lr=0.1)
a.load_state_dict({"weight": torch.ones(2, 2), "bias": torch.zeros(2)}, assign=True)
print("assign", type(a.weight).__name__, a.weight.requires_grad, r(a.weight))
ce = nn.CrossEntropyLoss(weight=torch.ones(3))
print("loss buffers", list(ce.state_dict().keys()), list(nn.BCEWithLogitsLoss(pos_weight=torch.ones(2)).state_dict().keys()), list(nn.MSELoss().state_dict().keys()))

# ---- dtype conversion (bug 14) ----
class WithBuffer(nn.Module):
    def __init__(self):
        super().__init__()
        self.lin = nn.Linear(1, 1)
        self.register_buffer("rm", torch.zeros(1))
        self.register_buffer("count", torch.tensor(0))


b = WithBuffer()
b.lin(torch.ones(1, 1)).sum().backward()
w0 = b.lin.weight
b.double()
print("double", b.rm.dtype, b.count.dtype, b.lin.weight.dtype, b.lin.weight.grad.dtype, b.lin.weight is w0)
b.float()
print("float", b.rm.dtype, b.lin.weight.dtype, b.lin.weight.grad.dtype)
b.to(torch.float64)
print("to dtype", b.rm.dtype, b.lin.bias.dtype)
b.to("cpu")
b.to(device="cpu", dtype=torch.float32)
print("to device", b.rm.dtype)
attempt("to int", lambda: b.to(torch.int64))

# ---- containers (bugs 10, 16) ----
pl = nn.ParameterList([torch.zeros(3), nn.Parameter(torch.ones(2))])
print("parameterlist", [type(p).__name__ for p in pl], pl[0].requires_grad, len(list(pl.parameters())), [n for n, _ in pl.named_parameters()])
pl.append(torch.ones(1))
print("parameterlist append", len(pl), type(pl[2]).__name__, type(pl[0:2]).__name__, len(pl[0:2]))
pd = nn.ParameterDict({"a": torch.zeros(2), "b": nn.Parameter(torch.ones(1))})
pd["c"] = torch.ones(3)
print("parameterdict", list(pd.keys()), [type(v).__name__ for v in pd.values()], "a" in pd, [n for n, _ in pd.named_parameters()])
s = nn.Sequential(nn.Linear(1, 2), nn.ReLU(), nn.Linear(2, 3), nn.Tanh())
print("sequential slice", type(s[1:3]).__name__, len(s[1:3]), type(s[-1]).__name__, [n for n, _ in s[1:3].named_children()])
s.append(nn.Sigmoid())
s.insert(1, nn.Identity())
s.extend([nn.ReLU()])
print("sequential edit", [type(x).__name__ for x in s], [n for n, _ in s.named_children()])
del s[0]
print("sequential del", [n for n, _ in s.named_children()], type(s[0]).__name__)
print("sequential pop", type(s.pop(0)).__name__, len(s))
s[0] = nn.GELU()
print("sequential setitem", type(s[0]).__name__)
sd = nn.Sequential(OrderedDict([("fc", nn.Linear(1, 1)), ("act", nn.ReLU())]))
print("sequential ordereddict", [n for n, _ in sd.named_children()], type(sd[0:1]).__name__, [n for n, _ in sd[0:1].named_children()])
ml = nn.ModuleList([nn.Linear(1, 1), nn.ReLU()])
ml.insert(1, nn.Tanh())
ml.extend([nn.Sigmoid()])
ml.append(nn.GELU())
print("modulelist", [type(x).__name__ for x in ml], type(ml[1:3]).__name__, len(ml[1:3]))
ml[0] = nn.Identity()
del ml[1]
print("modulelist edit", [type(x).__name__ for x in ml], [n for n, _ in ml.named_children()])
ml += [nn.ReLU()]
print("modulelist iadd", len(ml), type(ml.pop(-1)).__name__, len(ml))
attempt("modulelist index", lambda: ml[10])
md = nn.ModuleDict({"a": nn.Linear(1, 1), "b": nn.ReLU()})
md["c"] = nn.Tanh()
md.update([("d", nn.Sigmoid())])
print("moduledict", list(md.keys()), "b" in md, len(md), [n for n, _ in md.named_parameters()], type(md.pop("b")).__name__, list(md))

# ---- repr (bug 15) ----
for mod in [nn.Linear(3, 2), nn.Linear(3, 2, bias=False), nn.LayerNorm(4), nn.LayerNorm((2, 3), eps=1e-6, elementwise_affine=False), nn.Dropout(0.2), nn.Dropout(0.1, inplace=True), nn.GELU(), nn.GELU("tanh"), nn.Softmax(), nn.Softmax(dim=1), nn.LogSoftmax(1),
            nn.Embedding(4, 2), nn.Embedding(4, 2, padding_idx=-1, max_norm=1.0), nn.ReLU(), nn.ReLU(inplace=True), nn.LeakyReLU(0.2), nn.ELU(alpha=0.5, inplace=True), nn.Hardtanh(-2.0, 2.0), nn.ReLU6(), nn.Softplus(2, 10), nn.Threshold(0.1, 0.0), nn.PReLU(3), nn.GLU(), nn.SiLU(), nn.Mish(), nn.Hardswish(), nn.Tanh(), nn.Sigmoid(), nn.Identity(),
            nn.GRUCell(2, 3), nn.LSTMCell(2, 3, bias=False), nn.RNNCell(2, 3, nonlinearity="relu"), nn.LSTM(2, 3, num_layers=2, batch_first=True, bidirectional=True), nn.GRU(2, 3, dropout=0.5, num_layers=2), nn.RNN(2, 3), nn.LSTM(4, 3, proj_size=2),
            nn.Conv1d(2, 3, 3), nn.Conv2d(2, 4, (3, 2), stride=2, padding=1, groups=2, bias=False), nn.Conv2d(1, 1, 3, padding="same"), nn.ConvTranspose2d(2, 3, 3, stride=2, output_padding=1),
            nn.BatchNorm2d(3), nn.BatchNorm1d(3, momentum=None, affine=False), nn.InstanceNorm2d(3), nn.GroupNorm(2, 4), nn.RMSNorm(4), nn.LocalResponseNorm(2),
            nn.MaxPool2d(2), nn.MaxPool1d(3, 2, 1), nn.AvgPool2d(2, 1), nn.AvgPool1d(3), nn.AdaptiveAvgPool2d((1, 1)), nn.AdaptiveMaxPool1d(2), nn.Flatten(), nn.Unflatten(1, (2, 3)),
            nn.Upsample(scale_factor=2), nn.Upsample(size=(3, 4), mode="bilinear"), nn.PixelShuffle(2), nn.Unfold(2), nn.Fold((4, 4), 2), nn.ZeroPad2d(1), nn.ConstantPad1d(2, 1.5), nn.ReflectionPad2d((1, 0, 1, 0)),
            nn.CrossEntropyLoss(), nn.MSELoss(reduction="sum"), nn.CosineSimilarity(), nn.MultiheadAttention(4, 2), nn.EmbeddingBag(5, 2, mode="sum"), nn.Bilinear(2, 3, 4),
            nn.Sequential(nn.Linear(2, 2), nn.Sequential(nn.ReLU(), nn.Dropout())), nn.ModuleList([nn.Linear(2, 2), nn.Linear(2, 2), nn.Linear(2, 2), nn.ReLU()]), nn.ModuleList(), nn.ParameterList([nn.Parameter(torch.zeros(2, 3))]),
            nn.TransformerEncoderLayer(4, 2, 8)]:
    print(repr(mod))

# ---- constructors and small forwards (bug 16) ----
print("identity args", nn.Identity(54, unused="x")(torch.ones(2)).tolist())
print("linear zero in", list(nn.Linear(0, 3)(torch.zeros(2, 0)).shape), r(nn.Linear(0, 3).bias))
print("gru cell unbatched", list(nn.GRUCell(2, 3)(torch.ones(2)).shape), list(nn.GRUCell(2, 3, dtype=torch.float64)(torch.ones(1, 2, dtype=torch.float64)).dtype.__repr__().split(".")))
h, c = nn.LSTMCell(2, 3)(torch.ones(2))
print("lstm cell unbatched", list(h.shape), list(c.shape), nn.LSTMCell(2, 3, dtype=torch.float64).weight_ih.dtype)
print("conv1d unbatched", list(nn.Conv1d(2, 3, 2, stride=(2,), padding=(1,))(torch.ones(2, 5)).shape))
print("layernorm no bias", [n for n, _ in nn.LayerNorm(3, bias=False).named_parameters()])
attempt("conv same strided", lambda: nn.Conv2d(1, 1, 3, stride=2, padding="same"))
attempt("conv bad padding string", lambda: nn.Conv2d(1, 1, 3, padding="full"))
print("calculate_gain", [r(init.calculate_gain(n)) for n in ["linear", "conv1d", "conv2d", "conv_transpose1d", "conv_transpose2d", "sigmoid", "tanh", "relu", "leaky_relu", "selu"]], r(init.calculate_gain("leaky_relu", 0.2)))
attempt("calculate_gain bad", lambda: init.calculate_gain("swish"))
attempt("calculate_gain bad slope", lambda: init.calculate_gain("leaky_relu", "x"))
print("fan in/out", init._calculate_fan_in_and_fan_out(torch.empty(4, 3, 2, 5)))
attempt("fan 1d", lambda: init._calculate_fan_in_and_fan_out(torch.empty(3)))
emb = nn.Embedding(4, 2, padding_idx=-1)
print("embedding padding", emb.padding_idx, r(emb.weight[3]))
attempt("embedding negative index", lambda: nn.Embedding(3, 2)(torch.tensor([-1])))
attempt("embedding index too large", lambda: nn.Embedding(3, 2)(torch.tensor([3])))
attempt("embedding bad padding", lambda: nn.Embedding(3, 2, padding_idx=3))
pre = nn.Embedding.from_pretrained(torch.ones(3, 2))
print("from_pretrained", pre.weight.requires_grad, pre.num_embeddings, pre.embedding_dim)
attempt("dropout p negative", lambda: F.dropout(torch.ones(2), -0.5))
attempt("dropout p > 1", lambda: nn.Dropout(1.5))
x = torch.ones(3, requires_grad=True)
y = nn.Dropout(p=1.0)(x)
y.sum().backward()
print("dropout p=1", y.tolist(), x.grad.tolist(), nn.Dropout(0.5).eval()(x) is x)
t = torch.tensor([-1.0, 2.0])
out = F.relu(t, inplace=True)
print("relu inplace", t.tolist(), out is t)
t = torch.tensor([-1.0, 2.0])
nn.LeakyReLU(0.5, inplace=True)(t)
print("leaky inplace", t.tolist())
attempt("relu inplace leaf", lambda: F.relu(torch.ones(2, requires_grad=True), inplace=True))
attempt("ce target out of range", lambda: F.cross_entropy(torch.zeros(1, 5), torch.tensor([-1])))
attempt("ce target too large", lambda: F.cross_entropy(torch.zeros(2, 5), torch.tensor([0, 5])))
attempt("nll float target", lambda: F.nll_loss(torch.zeros(2, 3), torch.tensor([0.0, 1.0])))
attempt("ce batch mismatch", lambda: F.cross_entropy(torch.zeros(3, 5), torch.tensor([0, 1])))
print("ce all ignored mean", F.cross_entropy(torch.zeros(2, 3), torch.tensor([-100, -100])).item() != F.cross_entropy(torch.zeros(2, 3), torch.tensor([-100, -100])).item())
attempt("bce out of range", lambda: F.binary_cross_entropy(torch.tensor([1.5]), torch.tensor([1.0])))
attempt("bce shape", lambda: F.binary_cross_entropy_with_logits(torch.zeros(2), torch.zeros(3)))
mm = nn.Linear(2, 2)
mm(torch.ones(1, 2)).sum().backward()
mm.weight.grad[0, 0] = float("nan")
attempt("clip nonfinite", lambda: nn.utils.clip_grad_norm_(mm.parameters(), 1.0, error_if_nonfinite=True))
mm.zero_grad()
mm(torch.ones(1, 2)).sum().backward()
print("clip foreach", r(nn.utils.clip_grad_norm_(mm.parameters(), 0.5, foreach=True).item()), r(nn.utils.get_total_norm([p.grad for p in mm.parameters()]).item()))
nn.utils.clip_grad_value_(mm.parameters(), 0.1, foreach=None)
print("clip value", r(mm.weight.grad))
nn.utils.clip_grads_with_norm_(mm.parameters(), 0.05, torch.tensor(0.2))
print("clip with norm", r(mm.weight.grad))
wn = nn.utils.weight_norm(nn.Linear(3, 2))
print("weight_norm", [n for n, _ in wn.named_parameters()], list(wn.state_dict().keys()))
nn.utils.remove_weight_norm(wn)
print("remove_weight_norm", [n for n, _ in wn.named_parameters()], type(wn.weight).__name__)
print("softmax implicit dims", [F.softmax(torch.zeros(*s)).shape == torch.Size(s) for s in [(3,), (2, 3), (2, 3, 4), (2, 3, 4, 5)]], r(F.softmax(torch.tensor([[1.0, 2.0], [3.0, 5.0]]))), r(nn.Softmax()(torch.tensor([1.0, 2.0]))))

# ---- module API ----
class Tiny(nn.Module):
    def __init__(self):
        super().__init__()
        self.a = nn.Linear(2, 2)
        self.b = nn.Sequential(nn.Linear(2, 1))
        self.register_buffer("buf", torch.ones(1))

    def forward(self, x, scale=1.0):
        return self.b(self.a(x)) * scale


t = Tiny()
print("get_submodule", type(t.get_submodule("b.0")).__name__, t.get_submodule("") is t)
attempt("get_submodule missing", lambda: t.get_submodule("b.5"))
attempt("get_submodule not module", lambda: t.get_submodule("a.weight"))
print("get_parameter", list(t.get_parameter("b.0.weight").shape))
attempt("get_parameter not param", lambda: t.get_parameter("buf"))
print("get_buffer", t.get_buffer("buf").tolist())
attempt("get_buffer not buffer", lambda: t.get_buffer("a.weight"))
print("train flags", t.training, t.eval().training, t.b[0].training, t.train().b[0].training, t.train(False).a.training)
attempt("train non-bool", lambda: t.train("yes"))
t.train()
t.requires_grad_(False)
print("requires_grad_", [p.requires_grad for p in t.parameters()])
t.requires_grad_()
t(torch.ones(1, 2)).sum().backward()
t.zero_grad(set_to_none=False)
print("zero_grad keep", [r(p.grad.abs().sum().item()) for p in t.parameters()])
t.zero_grad()
print("zero_grad none", [p.grad for p in t.parameters()])
seen = []
h1 = t.register_forward_pre_hook(lambda mod, args: (args[0] * 2,))
h2 = t.register_forward_hook(lambda mod, args, out: out + 100)
h3 = t.a.register_forward_hook(lambda mod, args, out: seen.append(list(out.shape)))
torch.manual_seed(0)
with torch.no_grad():
    for p in t.parameters():
        p.fill_(0.5)
base = t(torch.ones(1, 2)).item()
print("hooks", r(base), seen)
h1.remove()
h2.remove()
h3.remove()
print("hooks removed", r(t(torch.ones(1, 2)).item()), len(seen))
h4 = t.register_forward_pre_hook(lambda mod, args, kwargs: (args, {"scale": 3.0}), with_kwargs=True)
h5 = t.register_forward_hook(lambda mod, args, kwargs, out: out * kwargs["scale"], with_kwargs=True)
print("kwargs hooks", r(t(torch.ones(1, 2)).item()))
h4.remove()
h5.remove()
order = []
t.register_forward_hook(lambda *a: order.append("first"))
t.register_forward_hook(lambda *a: order.append("prepended"), prepend=True)
t(torch.ones(1, 2))
print("hook order", order)
lin = nn.Linear(3, 2)
with torch.no_grad():
    lin.weight.copy_(torch.tensor([[1.0, 2.0, 3.0], [-1.0, 0.5, 0.0]]))
    lin.bias.fill_(0.1)
got = []
bh = lin.register_full_backward_hook(lambda mod, gin, gout: got.append((r(gin[0]), r(gout[0]))))
xin = torch.tensor([[1.0, 2.0, 3.0]], requires_grad=True)
(lin(xin) * torch.tensor([[2.0, 3.0]])).sum().backward()
print("backward hook", got, r(xin.grad))
bh.remove()
lin.register_full_backward_hook(lambda mod, gin, gout: (gin[0] * 0,))
xin = torch.tensor([[1.0, 2.0, 3.0]], requires_grad=True)
lin(xin).sum().backward()
print("backward hook replaces", r(xin.grad), r(lin.weight.grad))
nolin = nn.Linear(2, 1)
calls = []
nolin.register_full_backward_hook(lambda mod, gin, gout: calls.append((gin, r(gout[0]))))
nolin(torch.ones(1, 2)).sum().backward()
print("backward hook no input grad", calls)
with nn.Linear(1, 1).register_forward_hook(lambda *a: None) as handle:
    pass
print("handle context", type(handle).__name__)
nb = nn.Module()
print("named_buffers empty", list(nb.named_buffers()), nn.Module().extra_repr() == "")

# ---- deepcopy ----
src = nn.Sequential(nn.Linear(2, 2), nn.BatchNorm1d(2))
src[1].register_buffer("scratch", torch.zeros(1), persistent=False)
dup = copy.deepcopy(src)
with torch.no_grad():
    dup[0].weight.fill_(7.0)
    dup[1].running_mean.fill_(3.0)
print("deepcopy independent", r(src[0].weight.sum().item()) != 14.0, r(src[1].running_mean), type(dup[0].weight).__name__, dup[0].weight.requires_grad, list(dup.state_dict().keys()) == list(src.state_dict().keys()), sorted(dup[1]._non_persistent_buffers_set))
enc = nn.TransformerEncoder(nn.TransformerEncoderLayer(4, 2, 8), 2, enable_nested_tensor=False)
print("encoder clones", len(enc.layers), enc.layers[0].self_attn.in_proj_weight is enc.layers[1].self_attn.in_proj_weight, len(list(enc.parameters())))

# ---- init ----
torch.manual_seed(0)
for shape in [(4, 4), (3, 5), (5, 3), (2, 3, 2)]:
    q = init.orthogonal_(torch.empty(*shape), gain=2.0)
    flat = q.reshape(shape[0], -1)
    g = flat @ flat.T if shape[0] <= flat.shape[1] else flat.T @ flat
    print("orthogonal", shape, r((g - 4.0 * torch.eye(g.shape[0])).abs().max().item(), 3))
tn = init.trunc_normal_(torch.empty(1000), mean=0.5, std=2.0, a=-1.0, b=1.5)
print("trunc_normal", tn.min().item() >= -1.0, tn.max().item() <= 1.5, abs(tn.mean().item() - 0.3) < 0.15)
print("dirac", init.dirac_(torch.empty(2, 3, 3)).sum().item(), init.dirac_(torch.empty(4, 2, 3, 3), groups=2).sum().item(), init.dirac_(torch.empty(1, 1, 3))[0, 0].tolist())
print("eye", init.eye_(torch.empty(2, 3)).tolist())
attempt("eye 3d", lambda: init.eye_(torch.empty(2, 2, 2)))
sp = init.sparse_(torch.empty(10, 4), 0.3)
print("sparse zeros per column", [(sp[:, j] == 0).sum().item() for j in range(4)])
print("kaiming zero-size", list(init.kaiming_uniform_(torch.empty(0, 3)).shape))

# ---- rnn utils ----
seqs = [torch.ones(3, 2), torch.ones(1, 2) * 2, torch.ones(2, 2) * 3]
padded = pad_sequence(seqs)
print("pad_sequence", list(padded.shape), r(padded[:, 1, 0]), list(pad_sequence(seqs, batch_first=True, padding_side="left").shape), r(pad_sequence(seqs, batch_first=True, padding_side="left")[1, :, 0]))
packed = pack_padded_sequence(padded, [3, 1, 2], enforce_sorted=False)
print("packed", packed.batch_sizes.tolist(), packed.sorted_indices.tolist(), packed.unsorted_indices.tolist(), r(packed.data[:, 0]))
unpacked, lens = pad_packed_sequence(packed, batch_first=True, total_length=4)
print("unpacked", list(unpacked.shape), lens.tolist(), r(unpacked[1, :, 0]))
attempt("pack unsorted enforce", lambda: pack_padded_sequence(padded, [1, 3, 2]))
attempt("pack zero length", lambda: pack_padded_sequence(padded, [3, 0, 2], enforce_sorted=False))
ps = pack_sequence([torch.ones(2, 1), torch.ones(3, 1)], enforce_sorted=False)
print("pack_sequence", ps.batch_sizes.tolist(), type(ps).__name__, len(ps))
lstm = nn.LSTM(2, 3)
out, (hn, cn) = lstm(packed)
print("lstm packed", type(out).__name__, out.data.shape[0], list(hn.shape), out.batch_sizes.tolist())
