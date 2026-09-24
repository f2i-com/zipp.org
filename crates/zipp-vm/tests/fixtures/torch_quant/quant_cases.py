# torch quantization parity cases: quantized tensors (quantize_per_tensor,
# quantize_per_channel, quantize_per_tensor_dynamic, dequantize, printing,
# shape ops), fake quantization with its gradients, the observers and fake
# quantizers of torch.ao.quantization, the quantized Linear/Conv kernels
# under the x86, fbgemm and onednn engines (ties included), dynamic Linear
# and LSTM, quantized pooling and QFunctional, and the eager workflows
# (fuse_modules, prepare/convert with MinMax and Histogram observers,
# prepare_qat/convert with FakeQuantize and the fused fake quantizer,
# quantize_dynamic). Runs unchanged under PyTorch 2.11 (gen.py writes
# quant_expected.json and text_expected.txt; it needs a build with the x86
# engine, i.e. Linux/macOS x86 — Windows wheels ship only onednn) and under
# Zipp (python_torch_quant.rs).
import math

import torch
import torch.nn as nn
import torch.nn.functional as F
import torch.ao.quantization as tq
import torch.ao.nn.quantized as nnq
import torch.ao.nn.quantized.dynamic as nnqd
import torch.ao.nn.intrinsic.quantized as nniq

try:
    import warnings

    warnings.simplefilter("ignore")
except ImportError:
    pass

F32 = torch.float32


def vals(n, seed):
    return [math.sin(0.9 * i + seed) + 0.3 * math.cos(2.3 * i + 2 * seed) for i in range(n)]


def dense(shape, seed, scale=1.0):
    n = 1
    for s in shape:
        n *= s
    return (torch.tensor(vals(n, seed), dtype=torch.float64) * scale).to(F32).reshape(shape)


def ties(shape, seed, step):
    """Values on half-steps of `step` (quantization ties) mixed with others."""
    n = 1
    for s in shape:
        n *= s
    out = []
    for i, v in enumerate(vals(n, seed)):
        out.append(round(v * 40) * 0.5 * step if i % 3 else v * 7 * step)
    return torch.tensor(out, dtype=F32).reshape(shape)


def flat(x):
    if isinstance(x, (tuple, list)):
        out = []
        for t in x:
            out.extend(flat(t))
        return out
    if isinstance(x, torch.Tensor):
        if x.is_quantized:
            return flat(x.int_repr()) + flat(x.dequantize()) + qparams(x)
        return [float(v) for v in x.detach().reshape(-1).tolist()]
    if x is None:
        return []
    return [float(x)]


def qparams(q):
    if q.qscheme() in (torch.per_channel_affine, torch.per_channel_symmetric):
        return flat(q.q_per_channel_scales()) + flat(q.q_per_channel_zero_points()) + [float(q.q_per_channel_axis())]
    return [float(q.q_scale()), float(q.q_zero_point())]


def err(fn):
    try:
        return repr(fn())
    except Exception as e:
        msg = str(e).split("\n")[0]
        if msg.startswith("Could not run"):
            at = msg.find("' backend")
            msg = "no kernel for the " + msg[msg.rfind("'", 0, at) + 1:at] + " backend"
        return "ERR %s: %s" % (type(e).__name__, msg)


def gname(t):
    g = t.grad_fn
    return None if g is None else type(g).__name__


def set_params(model, seed):
    """Deterministic parameters and batch-norm statistics."""
    with torch.no_grad():
        for i, (name, p) in enumerate(sorted(model.named_parameters())):
            p.copy_(dense(tuple(p.shape), seed + 0.37 * i, 0.4))
        for i, (name, b) in enumerate(sorted(model.named_buffers())):
            if name.endswith("running_mean"):
                b.copy_(dense(tuple(b.shape), seed + 1.3 * i, 0.2))
            elif name.endswith("running_var"):
                b.copy_(dense(tuple(b.shape), seed + 1.7 * i, 0.3).abs() + 0.5)


def quantized_linear(x, w, b, scale, zp, per_channel, relu, wscale):
    xq = torch.quantize_per_tensor(x, 2.0 ** -4 if wscale else 0.037, 3 if wscale else 131, torch.quint8)
    if per_channel:
        n = w.shape[0]
        sc = torch.tensor([2.0 ** -6 if wscale else 0.004 + 0.001 * j for j in range(n)], dtype=torch.float64)
        wq = torch.quantize_per_channel(w, sc, torch.zeros(n, dtype=torch.long), 0, torch.qint8)
    else:
        wq = torch.quantize_per_tensor(w, 2.0 ** -6 if wscale else 0.0052, 0, torch.qint8)
    m = nniq.LinearReLU(w.shape[1], w.shape[0]) if relu else nnq.Linear(w.shape[1], w.shape[0], bias_=b is not None)
    m.set_weight_bias(wq, b)
    m.scale, m.zero_point = scale, zp
    return m(xq)


def quantized_conv(x, w, b, stride, padding, dilation, groups, scale, zp, per_channel, relu, wscale):
    xq = torch.quantize_per_tensor(x, 2.0 ** -4 if wscale else 0.043, 5 if wscale else 120, torch.quint8)
    n = w.shape[0]
    if per_channel:
        sc = torch.tensor([2.0 ** -6 if wscale else 0.003 + 0.0007 * j for j in range(n)], dtype=torch.float64)
        wq = torch.quantize_per_channel(w, sc, torch.zeros(n, dtype=torch.long), 0, torch.qint8)
    else:
        wq = torch.quantize_per_tensor(w, 2.0 ** -6 if wscale else 0.0061, 0, torch.qint8)
    cls = nniq.ConvReLU2d if relu else nnq.Conv2d
    m = cls(x.shape[1], n, tuple(w.shape[2:]), stride=stride, padding=padding, dilation=dilation, groups=groups, bias=b is not None)
    m.set_weight_bias(wq, b)
    m.scale, m.zero_point = scale, zp
    return m(xq)


class Net(nn.Module):
    """A small CNN in the shape eager quantization expects."""

    def __init__(self):
        super().__init__()
        self.quant = tq.QuantStub()
        self.conv1 = nn.Conv2d(2, 4, 3, padding=1)
        self.bn1 = nn.BatchNorm2d(4)
        self.relu1 = nn.ReLU()
        self.pool = nn.MaxPool2d(2)
        self.conv2 = nn.Conv2d(4, 6, 3, stride=1, groups=2)
        self.relu2 = nn.ReLU()
        self.gap = nn.AdaptiveAvgPool2d(1)
        self.fc1 = nn.Linear(6, 5)
        self.relu3 = nn.ReLU()
        self.fc2 = nn.Linear(5, 3)
        self.dequant = tq.DeQuantStub()

    def forward(self, x):
        x = self.quant(x)
        x = self.pool(self.relu1(self.bn1(self.conv1(x))))
        x = self.relu2(self.conv2(x))
        x = torch.flatten(self.gap(x), 1)
        x = self.relu3(self.fc1(x))
        return self.dequant(self.fc2(x))


FUSE = [["conv1", "bn1", "relu1"], ["conv2", "relu2"], ["fc1", "relu3"]]


def ptq(qconfig, seed, batches=3):
    m = Net()
    set_params(m, seed)
    m.eval()
    m.qconfig = qconfig
    m = tq.fuse_modules(m, FUSE)
    p = tq.prepare(m)
    for i in range(batches):
        p(dense((2, 2, 8, 8), seed + i, 1.5 + 0.5 * i))
    q = tq.convert(p)
    return p, q


def qat(qconfig, seed, steps=3):
    m = Net()
    set_params(m, seed)
    m.train()
    m.qconfig = qconfig
    m = tq.fuse_modules_qat(m, FUSE)
    p = tq.prepare_qat(m)
    opt = torch.optim.SGD(p.parameters(), lr=0.05, momentum=0.9)
    losses = []
    for i in range(steps):
        x = dense((3, 2, 8, 8), seed + 0.3 * i, 1.2)
        target = dense((3, 3), seed + 5.0 + i, 0.5)
        opt.zero_grad()
        loss = ((p(x) - target) ** 2).mean()
        loss.backward()
        opt.step()
        losses.append(loss)
    p.eval()
    q = tq.convert(p)
    return p, q, losses


def results():
    R = {}
    # ---- quantize / dequantize ----
    for dt, zps in ((torch.quint8, (0, 7, 128, 255)), (torch.qint8, (-128, -3, 0, 127)), (torch.qint32, (-5, 0, 11))):
        for scale in (0.1, 0.0371, 2.0 ** -5, 1.7):
            for zp in zps:
                x = torch.cat([ties((21,), scale * 10, scale), dense((20,), zp * 0.1 + scale, 90 * scale)])
                q = torch.quantize_per_tensor(x, scale, zp, dt)
                R["q_%s_%g_%d" % (dt, scale, zp)] = flat(q)
    special = torch.tensor([float("nan"), float("inf"), -float("inf"), 1e30, -1e30, 0.0, -0.0] * 12)
    for dt, zp in ((torch.quint8, 9), (torch.qint8, -2), (torch.qint32, 4)):
        R["q_special_%s" % dt] = flat(torch.quantize_per_tensor(special, 0.25, zp, dt).int_repr())
    R["q_tensor_params"] = flat(torch.quantize_per_tensor(dense((4, 5), 0.3, 3.0), torch.tensor(0.0457), torch.tensor(17), torch.quint8))
    R["q_list"] = flat(torch.quantize_per_tensor([dense((3,), 1.0), dense((4,), 2.0, 4.0)], torch.tensor([0.1, 0.2]), torch.tensor([10, 5]), torch.quint8))
    for axis in (0, 1, 2):
        x = torch.cat([ties((3, 4, 5), axis + 0.5, 0.05).reshape(-1), dense((60,), axis, 3.0)]).reshape(6, 4, 5).transpose(0, axis).contiguous()
        C = x.shape[axis]
        sc = torch.tensor([0.013 + 0.011 * c for c in range(C)], dtype=torch.float64)
        for dt, base in ((torch.qint8, -3), (torch.quint8, 120), (torch.qint32, 1)):
            zp = torch.tensor([base + c for c in range(C)])
            R["qc_%d_%s" % (axis, dt)] = flat(torch.quantize_per_channel(x, sc, zp, axis, dt))
    R["qc_float_scales"] = flat(torch.quantize_per_channel(dense((3, 4), 7.0, 2.0), torch.tensor([0.1, 0.02, 0.3]), torch.tensor([0, 1, 2]), 0, torch.qint8))
    for i, (lo, hi) in enumerate(((-1.0, 2.0), (0.5, 3.0), (-4.0, -0.25), (-1e-6, 2e-6), (0.0, 0.0), (-3e-9, 1e-9), (-100.0, 0.001), (1e-5, 3e-5))):
        x = torch.linspace(lo, hi, 7)
        out = []
        for rr in (False, True):
            s, z = torch._choose_qparams_per_tensor(x, rr)
            out += [s, z]
            for dt in (torch.quint8, torch.qint8):
                out += flat(torch.quantize_per_tensor_dynamic(x, dt, rr))
        R["q_dynamic_%d" % i] = out
    # ---- fake quantization ----
    for i, (s, z, lo, hi) in enumerate(((0.1, 3, 0, 255), (0.037, 0, -128, 127), (2.0 ** -5, 17, 0, 127), (0.25, -4, -8, 7))):
        x = torch.cat([ties((30,), s, s), dense((30,), i, 10 * s * (hi - lo) / 30)])
        xs = x.clone().requires_grad_(True)
        y = torch.fake_quantize_per_tensor_affine(xs, s, z, lo, hi)
        (y * dense((60,), 3.0 + i)).sum().backward()
        xt = x.clone().requires_grad_(True)
        yt = torch.fake_quantize_per_tensor_affine(xt, torch.tensor(s), torch.tensor(z, dtype=torch.int32), lo, hi)
        (yt * yt).sum().backward()
        R["fq_%d" % i] = flat([y, xs.grad, yt, xt.grad])
    x = torch.cat([ties((3, 4), 0.2, 0.05).reshape(-1), dense((12,), 0.4, 2.0)]).reshape(3, 8).requires_grad_(True)
    y = torch.fake_quantize_per_channel_affine(x, torch.tensor([0.05, 0.02, 0.013]), torch.tensor([0, 3, -2], dtype=torch.int32), 0, -128, 127)
    (y * dense((3, 8), 1.5)).sum().backward()
    x2 = x.detach().t().contiguous().requires_grad_(True)
    y2 = torch.fake_quantize_per_channel_affine(x2, torch.tensor([0.05, 0.02, 0.013]), torch.tensor([0, 3, 250], dtype=torch.int32), 1, 0, 255)
    (y2 ** 3).sum().backward()
    R["fq_channel"] = flat([y, x.grad, y2, x2.grad])
    # ---- observers ----
    batches = [dense((4, 6), 0.1, 2.0), dense((4, 6), 1.1, 3.5) + 0.7, dense((4, 6), 2.1, 0.5) - 1.0, dense((4, 6), 3.3, 5.0)]
    obs = [("minmax", tq.MinMaxObserver()), ("minmax_sym", tq.MinMaxObserver(dtype=torch.qint8, qscheme=torch.per_tensor_symmetric)),
           ("minmax_rr", tq.MinMaxObserver(reduce_range=True)), ("minmax_q", tq.MinMaxObserver(quant_min=0, quant_max=127)),
           ("minmax_qint8_aff", tq.MinMaxObserver(dtype=torch.qint8)), ("minmax_sym_u8", tq.MinMaxObserver(qscheme=torch.per_tensor_symmetric)),
           ("minmax_qint32", tq.MinMaxObserver(dtype=torch.qint32)), ("moving", tq.MovingAverageMinMaxObserver()),
           ("moving_sym", tq.MovingAverageMinMaxObserver(averaging_constant=0.3, dtype=torch.qint8, qscheme=torch.per_tensor_symmetric)),
           ("perchannel0", tq.PerChannelMinMaxObserver()), ("perchannel1", tq.PerChannelMinMaxObserver(ch_axis=1, dtype=torch.qint8, qscheme=torch.per_channel_symmetric)),
           ("moving_pc", tq.MovingAveragePerChannelMinMaxObserver(averaging_constant=0.2, ch_axis=1)),
           ("fixed", tq.FixedQParamsObserver(scale=1.0 / 256, zero_point=0)), ("weight_127", tq.weight_observer_range_neg_127_to_127()),
           ("pc_weight_127", tq.per_channel_weight_observer_range_neg_127_to_127())]
    for name, o in obs:
        out = []
        for b in batches:
            o(b)
            s, z = o.calculate_qparams()
            out += flat([s, z]) + [float(len(s.shape)), 1.0 if z.dtype == torch.int64 else 2.0 if z.dtype == torch.int32 else 3.0]
        R["obs_" + name] = out
    for name, h in (("hist", tq.HistogramObserver()), ("hist_rr", tq.HistogramObserver(reduce_range=True)),
                    ("hist_sym", tq.HistogramObserver(dtype=torch.qint8, qscheme=torch.per_tensor_symmetric)), ("hist_bins", tq.HistogramObserver(bins=64))):
        out = []
        for i, b in enumerate(batches + [dense((50,), 9.0, 1.0), dense((40,), 2.2, 0.3)]):
            h(b)
            out += flat(h.calculate_qparams()) + [float(h.min_val), float(h.max_val)]
        out += flat(h.histogram)
        R["obs_" + name] = out
    # ---- fake quantize modules ----
    for name, fq in (("fq_default", tq.default_fake_quant()), ("fq_weight", tq.default_weight_fake_quant()),
                     ("fq_pc", tq.default_per_channel_weight_fake_quant()), ("fq_fused_act", tq.default_fused_act_fake_quant()),
                     ("fq_fused_wt", tq.default_fused_wt_fake_quant()), ("fq_fused_pc", tq.default_fused_per_channel_wt_fake_quant()),
                     ("fq_fixed", tq.default_fixed_qparams_range_neg1to1_fake_quant()), ("fq_hist", tq.default_histogram_fake_quant())):
        out = []
        for i, b in enumerate(batches):
            x = b.clone().requires_grad_(True)
            if i == 2:
                fq.disable_observer()
            if i == 3:
                fq.enable_observer()
            y = fq(x)
            (y * dense((4, 6), 7.0 + i)).sum().backward()
            out += flat([y, x.grad, fq.scale, fq.zero_point])
        R[name] = out
    # ---- quantized Linear / Conv under each engine ----
    for engine in ("x86", "fbgemm", "onednn"):
        torch.backends.quantized.engine = engine
        out = []
        for k, (M, K, N) in enumerate(((3, 7, 5), (1, 16, 9), (4, 33, 3))):
            for wscale in (True, False):
                x = dense((M, K), k + 0.2, 2.0)
                w = dense((N, K), k + 1.4, 0.5)
                b = (torch.tensor([round(v * 20) * 2.0 ** -11 for v in vals(N, k)]) if wscale else dense((N,), k + 2.2, 0.3))
                for pc in (False, True):
                    for relu in (False, True):
                        for bias in (b, None) if not relu else (b,):
                            sy = 2.0 ** -9 * (1 + k % 3) if wscale else 0.021 + 0.005 * k
                            out += flat(quantized_linear(x, w, bias, sy, 37 * k + 11, pc, relu, wscale))
        R["linear_" + engine] = out
        out = []
        for k, (shape, wshape, stride, padding, dilation, groups) in enumerate((((1, 2, 5, 6), (3, 2, 3, 3), (1, 1), (1, 1), (1, 1), 1),
                                                                                  ((2, 4, 7, 5), (6, 2, 2, 3), (2, 1), (0, 1), (1, 2), 2),
                                                                                  ((1, 3, 6, 6), (3, 1, 3, 3), (1, 2), (1, 0), (2, 1), 3))):
            for wscale in (True, False):
                x = dense(shape, k + 0.6, 2.0)
                w = dense(wshape, k + 1.9, 0.5)
                n = wshape[0]
                b = (torch.tensor([round(v * 20) * 2.0 ** -11 for v in vals(n, k)]) if wscale else dense((n,), k + 3.1, 0.3))
                for pc in (False, True):
                    for relu in (False, True):
                        sy = 2.0 ** -9 * (1 + k % 3) if wscale else 0.017 + 0.004 * k
                        out += flat(quantized_conv(x, w, b if relu or k != 1 else None, stride, padding, dilation, groups, sy, 29 * k + 7, pc, relu, wscale))
        R["conv_" + engine] = out
    torch.backends.quantized.engine = "x86"
    c1 = nnq.Conv1d(2, 3, 3, stride=2, padding=1)
    c1.set_weight_bias(torch.quantize_per_tensor(dense((3, 2, 3), 4.0, 0.5), 0.006, 0, torch.qint8), dense((3,), 4.4, 0.2))
    c1.scale, c1.zero_point = 0.02, 100
    R["conv1d"] = flat(c1(torch.quantize_per_tensor(dense((2, 2, 9), 4.8, 2.0), 0.03, 90, torch.quint8)))
    # ---- dynamic Linear ----
    out = []
    for k, (lead, K, N) in enumerate((((3,), 7, 5), ((2, 3), 16, 4), ((1,), 40, 3))):
        x = dense(lead + (K,), k + 0.9, 2.0 + k)
        w = dense((N, K), k + 3.4, 0.5)
        for pc in (False, True):
            for bias in (dense((N,), k + 5.2, 0.3), None):
                m = nnqd.Linear(K, N, bias_=bias is not None)
                if pc:
                    wq = torch.quantize_per_channel(w, torch.tensor([0.004 + 0.001 * j for j in range(N)], dtype=torch.float64), torch.zeros(N, dtype=torch.long), 0, torch.qint8)
                else:
                    wq = torch.quantize_per_tensor(w, 0.0047, 0, torch.qint8)
                m.set_weight_bias(wq, bias)
                out += flat(m(x))
    R["dynamic_linear"] = out
    # ---- quantized tensor ops ----
    q = torch.quantize_per_tensor(dense((2, 3, 6, 5), 1.2, 3.0), 0.05, 70, torch.quint8)
    q1 = torch.quantize_per_tensor(dense((3, 1, 6, 7), 2.2, 3.0), 0.04, 71, torch.quint8)
    R["qops"] = flat([torch.relu(q), F.relu(q), q.clone().relu_(), F.relu6(q), F.hardtanh(q, -0.5, 1.25), nnq.ReLU6()(q),
                      F.max_pool2d(q, 2), F.max_pool2d(q, 3, stride=2, padding=1), F.avg_pool2d(q, 2), F.avg_pool2d(q, 3, stride=2, padding=1),
                      F.avg_pool2d(q, 3, stride=2, padding=1, count_include_pad=False), F.avg_pool2d(q, 2, ceil_mode=True), F.avg_pool2d(q1, 2),
                      F.adaptive_avg_pool2d(q, 1), F.adaptive_avg_pool2d(q, (4, 3)), F.adaptive_avg_pool2d(q1, (4, 3)), F.adaptive_avg_pool2d(q1, 1),
                      F.interpolate(q, scale_factor=2), q.reshape(6, 30), q.view(-1), torch.flatten(q, 1), q.permute(0, 2, 3, 1), q.transpose(1, 2),
                      q[1], q[:, 1:, ::2], q.unsqueeze(1).squeeze(1), torch.cat([q, q], 1), torch.stack([q, q]),
                      torch.cat([q, torch.quantize_per_tensor(dense((2, 3, 6, 5), 4.4), 0.03, 10, torch.quint8)]), q.max(), q.min()])
    qa = torch.quantize_per_tensor(dense((4, 5), 0.5, 2.0), 0.02, 120, torch.quint8)
    qb = torch.quantize_per_tensor(dense((4, 5), 1.5, 3.0), 0.03, 100, torch.quint8)
    ff = nnq.QFunctional()
    ff.scale, ff.zero_point = 0.045, 60
    R["qfunctional"] = flat([ff.add(qa, qb), ff.mul(qa, qb), ff.cat([qa, qb], 1), ff.add_relu(qa, qb)])
    qc = torch.quantize_per_channel(dense((3, 4, 2), 6.0, 2.0), torch.tensor([0.02, 0.03, 0.05], dtype=torch.float64), torch.tensor([1, -2, 0]), 0, torch.qint8)
    R["qc_ops"] = flat([qc[1], qc[:, 1:], qc.reshape(3, 8), qc.clone(), qc.unsqueeze(0), qc[0, 2]])
    # ---- workflows ----
    for name, qconfig in (("minmax", tq.default_qconfig), ("x86", tq.get_default_qconfig("x86")), ("qnnpack", tq.get_default_qconfig("qnnpack")),
                          ("perchannel", tq.default_per_channel_qconfig)):
        p, q = ptq(qconfig, 0.5 + len(name))
        x = dense((2, 2, 8, 8), 11.0, 1.7)
        out = flat(q(x))
        for k, v in q.state_dict().items():
            if isinstance(v, tuple):
                out += flat(list(v))
            elif isinstance(v, torch.Tensor):
                out += flat(v)
        R["ptq_" + name] = out
        R["ptq_float_" + name] = flat(p(x))
    for name, qconfig in (("v1", tq.get_default_qat_qconfig("x86")), ("v0", tq.get_default_qat_qconfig("x86", 0)), ("default", tq.default_qat_qconfig),
                          ("qnnpack", tq.get_default_qat_qconfig("qnnpack"))):
        p, q, losses = qat(qconfig, 1.5 + len(name))
        x = dense((2, 2, 8, 8), 12.0, 1.4)
        out = flat(losses) + flat(p(x))
        for k, v in sorted(p.state_dict().items()):
            out += flat(v)
        R["qat_float_" + name] = out
        out = flat(q(x))
        for k, v in q.state_dict().items():
            if isinstance(v, tuple):
                out += flat(list(v))
            elif isinstance(v, torch.Tensor):
                out += flat(v)
        R["qat_" + name] = out
    torch.manual_seed(0)
    lin = nn.Sequential(nn.Linear(6, 5), nn.ReLU(), nn.Linear(5, 4))
    set_params(lin, 3.0)
    ql = tq.quantize_dynamic(lin)
    qpc = tq.quantize_dynamic(lin, {nn.Linear: tq.per_channel_dynamic_qconfig})
    x = dense((3, 6), 4.0, 2.0)
    R["quantize_dynamic"] = flat([ql(x), qpc(x)])
    # the model gen_checkpoint saves, run on a fixed input
    _, m = ptq(tq.get_default_qconfig("x86"), 4.0)
    R["ckpt_model"] = flat(m(dense((2, 2, 8, 8), 13.0, 1.5)))
    return R


def tol_results():
    """Cases whose float math may round differently (sigmoid/tanh): compared
    with a tolerance."""
    R = {}
    for i, (layers, bidir, bf) in enumerate(((1, False, False), (2, False, True), (2, True, False))):
        lstm = nn.LSTM(5, 4, num_layers=layers, bidirectional=bidir, batch_first=bf)
        set_params(lstm, 0.3 * i)
        q = tq.quantize_dynamic(nn.Sequential(lstm), {nn.LSTM})
        x = dense((6, 3, 5), 1.0 + i, 2.0)
        out, (h, c) = q[0](x)
        h0 = dense((layers * (2 if bidir else 1), 3 if not bf else 6, 4), 2.0 + i, 0.5)
        out2, (h2, c2) = q[0](x, (h0, h0 * 0.5))
        R["lstm_%d" % i] = flat([out, h, c, out2, h2, c2])
    return R


def text():
    T = []
    p = T.append
    x = torch.tensor([[-1.0, 0.5, 2.0], [3.25, -0.1, 0.0]])
    q = torch.quantize_per_tensor(x, 0.1, 10, torch.quint8)
    qc = torch.quantize_per_channel(x, torch.tensor([0.1, 0.2], dtype=torch.float64), torch.tensor([0, 3]), 0, torch.qint8)
    for t in (q, qc, torch.quantize_per_tensor(x, 0.037, -3, torch.qint8), torch.quantize_per_tensor(x, 0.5, 7, torch.qint32),
              torch.quantize_per_tensor(torch.tensor(1.5), 0.1, 10, torch.quint8), torch.quantize_per_tensor(torch.zeros(0), 0.1, 10, torch.quint8),
              torch.quantize_per_tensor(dense((50, 50), 1.0, 3.0), 0.1, 10, torch.quint8), qc[0], qc[:, 1:],
              torch.quantize_per_channel(dense((3, 2, 2), 2.0, 2.0), torch.tensor([0.1, 0.25, 0.3333], dtype=torch.float64), torch.tensor([0, 1, -2]), 0, torch.qint8),
              torch.quantize_per_tensor(x, torch.tensor(0.1), torch.tensor(10), torch.quint8), q[0, 1], q.t(), torch.quantize_per_tensor_dynamic(x, torch.qint8, True)):
        for line in repr(t).split("\n"):
            p(line)
    p("%s %s %s %s %s %s" % (q.dtype, q.qscheme(), q.is_quantized, x.is_quantized, q.q_scale(), q.q_zero_point()))
    p("%s %s %s %s %s %s %s" % (q.element_size(), q.stride(), q.is_contiguous(), q.requires_grad, q.layout, q.device, q.size()))
    p("%s %s %s %s" % (qc.q_per_channel_scales(), qc.q_per_channel_zero_points(), qc.q_per_channel_axis(), qc.qscheme()))
    p("%s %s %s" % (q.int_repr(), qc.int_repr(), q.int_repr().dtype))
    p("%s %s %s %s" % (torch.quint8, torch.qint8, torch.qint32, torch.quint4x2))
    p("%s %s %s %s %s" % (torch.per_tensor_affine, torch.per_channel_affine, torch.per_tensor_symmetric, torch.per_channel_symmetric, torch.per_channel_affine_float_qparams))
    p("%s %s %s" % (torch.quint8.is_floating_point, torch.qint8.is_floating_point, torch.qint32.itemsize))
    p("%s %s %s %s %s" % (torch.equal(q, q.clone()), torch.equal(q, torch.quantize_per_tensor(x, 0.1, 11, torch.quint8)), q.item() if q.numel() == 1 else q[0, 1].item(), len(q), q.dim()))
    p("%s" % (torch.dequantize([q, qc]),))
    p("%s %s" % (x.dequantize() is x, torch._make_per_tensor_quantized_tensor(torch.tensor([1, 2], dtype=torch.uint8), 0.5, 3)))
    p("%s" % (torch._make_per_channel_quantized_tensor(torch.tensor([[1, 2]], dtype=torch.int8), torch.tensor([0.5], dtype=torch.double), torch.tensor([1]), 0),))
    p("%s" % (torch.backends.quantized.engine,))
    for f in (lambda: torch.quantize_per_tensor(x.double(), 0.1, 10, torch.quint8), lambda: torch.quantize_per_tensor(x.half(), 0.1, 10, torch.quint8),
              lambda: torch.quantize_per_tensor(torch.tensor([1, 2]), 0.1, 10, torch.quint8), lambda: torch.quantize_per_tensor(x, 0.1, 10, torch.float32),
              lambda: torch.quantize_per_tensor(x, 0.1, 300, torch.quint8), lambda: torch.quantize_per_tensor(x, 0.1, -1, torch.quint8),
              lambda: torch.quantize_per_tensor(x, 0.1, 200, torch.qint8), lambda: torch.quantize_per_channel(x, torch.tensor([0.1, 0.2], dtype=torch.double), torch.tensor([0, 3]), 1, torch.qint8),
              lambda: torch.quantize_per_tensor_dynamic(x, torch.qint32, True), lambda: qc.q_scale(), lambda: qc.q_zero_point(),
              lambda: q.q_per_channel_axis(), lambda: q.q_per_channel_scales(), lambda: torch.relu(qc), lambda: qc.view(3, 2), lambda: qc.permute(1, 0),
              lambda: qc.t(), lambda: q.tolist(), lambda: q.numpy(), lambda: q.float(), lambda: q.to(torch.float32), lambda: q + q, lambda: q.sum(),
              lambda: x.int_repr(), lambda: x.q_scale(), lambda: torch.fake_quantize_per_tensor_affine(x, 0.1, 300, 0, 255),
              lambda: torch.fake_quantize_per_tensor_affine(x, 0.1, 3, 5, 2),
              lambda: torch.fake_quantize_per_channel_affine(x, torch.tensor([0.1, 0.2]), torch.tensor([0, 3]), 0, -128, 127),
              lambda: tq.get_default_qconfig("mkl"), lambda: tq.QConfig(activation=tq.MinMaxObserver(), weight=tq.default_weight_observer),
              lambda: tq.MinMaxObserver(qscheme=torch.per_channel_affine), lambda: tq.PerChannelMinMaxObserver(qscheme=torch.per_tensor_affine),
              lambda: tq.MinMaxObserver(quant_min=3, quant_max=100), lambda: tq.prepare_qat(nn.Linear(2, 2).eval()),
              lambda: nnq.Linear(3, 2)(torch.randn(2, 3))):
        p(err(f))
    # gradients, flags, reprs
    xg = x.clone().requires_grad_(True)
    p("%s %s" % (gname(torch.fake_quantize_per_tensor_affine(xg, 0.1, 3, 0, 255)), gname(torch.fake_quantize_per_tensor_affine(xg, torch.tensor(0.1), torch.tensor(3, dtype=torch.int32), 0, 255))))
    p("%s" % gname(torch.fake_quantize_per_channel_affine(xg, torch.tensor([0.1, 0.2]), torch.tensor([0, 3], dtype=torch.int32), 0, -128, 127)))
    p(repr(tq.MinMaxObserver()))
    o = tq.MinMaxObserver()
    o(x)
    p(repr(o))
    p(repr(tq.PerChannelMinMaxObserver(ch_axis=1)))
    p(repr(tq.default_qconfig))
    p(repr(tq.get_default_qconfig("x86")))
    p(repr(tq.default_dynamic_qconfig))
    p(repr(tq.QConfig(activation=tq.MinMaxObserver, weight=tq.default_weight_observer)))
    for line in repr(tq.default_fake_quant()).split("\n"):
        p(line)
    for line in repr(tq.default_fused_act_fake_quant()).split("\n"):
        p(line)
    lin = nnq.Linear(3, 2)
    for line in repr(lin).split("\n"):
        p(line)
    p("%s %s %s %s %s" % (lin.weight().dtype, tuple(lin.weight().shape), lin.weight().qscheme(), lin.weight().q_scale(), lin.bias()))
    for line in repr(nniq.LinearReLU(3, 2)).split("\n"):
        p(line)
    for line in repr(nnq.Conv2d(2, 4, 3, stride=2, padding=1, groups=2)).split("\n"):
        p(line)
    for line in repr(nnqd.Linear(3, 2)).split("\n"):
        p(line)
    p(repr(nnq.Quantize(0.5, 3, torch.quint8)))
    p(repr(nnq.DeQuantize()))
    p(str(list(lin.state_dict().keys())))
    c = nnq.Conv2d(2, 4, 3)
    p(str(list(c.state_dict().keys())))
    # workflows: the converted models
    # (their calibrated scales come from float activations: compared as
    # values, not as text)
    pp, qq = ptq(tq.get_default_qconfig("x86"), 2.0)
    for m in (pp, qq):
        for name, mod in m.named_modules():
            p("%s %s %s %d" % (name, type(mod).__name__, mod._get_name(), len(mod._forward_hooks) + len(mod._forward_pre_hooks)))
    p(str(list(qq.state_dict().keys())))
    pq, qq2, losses = qat(tq.get_default_qat_qconfig("x86"), 3.0, steps=1)
    for m in (pq, qq2):
        for name, mod in m.named_modules():
            p("%s %s %s %s" % (name, type(mod).__name__, mod._get_name(), mod.training))
    p(str(sorted(pq.state_dict().keys())))
    lin = nn.Sequential(nn.Linear(6, 5), nn.ReLU(), nn.LSTM(5, 3))
    for line in repr(tq.quantize_dynamic(lin)).split("\n"):
        p(line)
    for line in repr(tq.fuse_modules(Net().eval(), FUSE)).split("\n"):
        p(line)
    return T


def gen_checkpoint(path):
    """A checkpoint PyTorch writes: quantized tensors of each dtype and
    scheme, and the state dict of a converted model."""
    q = torch.quantize_per_tensor(dense((2, 3), 0.1, 2.0), 0.05, 12, torch.quint8)
    qi = torch.quantize_per_tensor(dense((4,), 0.2, 2.0), 0.03, -5, torch.qint8)
    q32 = torch.quantize_per_tensor(dense((3,), 0.3, 2.0), 0.001, 7, torch.qint32)
    qc = torch.quantize_per_channel(dense((3, 2, 2), 0.4, 2.0), torch.tensor([0.02, 0.04, 0.03], dtype=torch.float64), torch.tensor([0, 1, -1]), 0, torch.qint8)
    qct = torch.quantize_per_tensor(dense((2, 5), 0.6, 2.0), 0.02, 3, torch.quint8).t()
    _, model = ptq(tq.get_default_qconfig("x86"), 4.0)
    torch.save({"q": q, "qi": qi, "q32": q32, "qc": qc, "qct": qct, "model": model.state_dict(), "dtype": torch.quint8, "scheme": torch.per_channel_affine}, path)


def ckpt_text(path, rt_path):
    """What loading gen_checkpoint's file shows, and a save/load round trip
    of quantized tensors and a quantized model's state dict."""
    T = []
    d = torch.load(path)
    for k in ("q", "qi", "q32", "qc", "qct"):
        T.extend(repr(d[k]).split("\n"))
        T.append("%s %s %s %s" % (k, d[k].dtype, tuple(d[k].shape), d[k].int_repr().tolist()))
    T.append("%s %s" % (d["dtype"], d["scheme"]))
    T.append(str(list(d["model"].keys())))
    # a model calibrated differently, then given the checkpoint's state
    _, m = ptq(tq.default_qconfig, 9.0)
    m.load_state_dict(d["model"])
    x = dense((2, 2, 8, 8), 13.0, 1.5)
    T.extend(repr(m(x)).split("\n"))
    T.extend(repr(m.fc2.weight()).split("\n"))
    torch.save({"model": m.state_dict(), "q": d["q"], "qc": d["qc"], "qct": d["qct"]}, rt_path)
    r = torch.load(rt_path)
    T.append("%s %s %s" % (torch.equal(r["q"], d["q"]), torch.equal(r["qc"], d["qc"]), torch.equal(r["qct"], d["qct"])))
    _, m2 = ptq(tq.default_qconfig, 7.0)
    m2.load_state_dict(r["model"])
    T.extend(repr(m2(x)).split("\n"))
    return T
