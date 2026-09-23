"""Generate the torch.nn parity fixtures with CPU PyTorch (2.11).

    python crates/zipp-vm/tests/fixtures/torch_nn/gen.py

Writes parity_*.py (one per test group: the recorded data plus each case's
constructor and expression as functions, since Zipp has no eval) and
api_expected.txt next to this file. Each parity case names a module
constructor and/or an expression; the
generator builds it in PyTorch with fixed inputs, runs forward and a
backward of sum(out * w) with fixed random weights w, and records the
state_dict, inputs, outputs, gradients and buffers after the forward.
`check.py` rebuilds the same case in Zipp, loads the state_dict and compares.
api.py prints behaviour (registries, repr, errors, hooks) and runs unchanged
on both; its PyTorch output is api_expected.txt.
"""
import json
import math
import os
import subprocess
import sys

import torch
import torch.nn as nn
import torch.nn.functional as F
from torch.nn.utils.rnn import pack_padded_sequence, pad_packed_sequence, pad_sequence, pack_sequence

HERE = os.path.dirname(os.path.abspath(__file__))


def randn(*shape):
    return ("randn", list(shape))


def ints(low, high, *shape):
    return ("int", low, high, list(shape))


def const(data, dtype="float32", grad=True):
    return ("const", data, dtype, grad)


def nograd(spec):
    return ("nograd", spec)


def make_input(spec, gen):
    kind = spec[0]
    if kind == "randn":
        return torch.randn(*spec[1], generator=gen), True
    if kind == "int":
        return torch.randint(spec[1], spec[2], tuple(spec[3]), generator=gen), False
    if kind == "const":
        t = torch.tensor(spec[1], dtype=getattr(torch, spec[2]))
        return t, spec[3] and t.is_floating_point()
    if kind == "nograd":
        t, _ = make_input(spec[1], gen)
        return t, False
    if kind == "rand01":
        return torch.rand(*spec[1], generator=gen) * 0.98 + 0.01, True
    if kind == "pos":
        return torch.rand(*spec[1], generator=gen) + 0.5, True
    if kind == "sign":
        return (torch.randint(0, 2, tuple(spec[1]), generator=gen) * 2 - 1).float(), False
    raise ValueError(kind)


def flatten(out, acc):
    if isinstance(out, torch.Tensor):
        acc.append(out)
    elif isinstance(out, (tuple, list)):
        for o in out:
            flatten(o, acc)
    return acc


def numbers(values):
    # A string keeps each case one small literal (Zipp bounds a function's
    # registers); nine significant digits round-trip float32.
    return ",".join(("%.9g" % v) if isinstance(v, float) else str(v) for v in values)


def tensor_json(t):
    t = t.detach()
    return {"shape": list(t.shape), "dtype": str(t.dtype).replace("torch.", ""), "data": numbers(t.reshape(-1).tolist())}


def run_case(case, index):
    name, ctor, expr, inputs = case["name"], case.get("ctor"), case["expr"], case["inputs"]
    gen = torch.Generator().manual_seed(1000 + index)
    torch.manual_seed(index)
    xs, grads = [], []
    for spec in inputs:
        t, g = make_input(spec, gen)
        if g:
            t.requires_grad_(True)
        xs.append(t)
        grads.append(g)
    env = {"torch": torch, "nn": nn, "F": F, "math": math, "pack_padded_sequence": pack_padded_sequence, "pad_packed_sequence": pad_packed_sequence, "pad_sequence": pad_sequence, "pack_sequence": pack_sequence}
    m = None
    if ctor:
        m = eval(ctor, env)
        # Non-trivial parameters: affine norms start at ones/zeros otherwise.
        with torch.no_grad():
            for pname, p in m.named_parameters():
                if case.get("perturb", True):
                    p.add_(torch.randn(p.shape, generator=gen) * 0.1)
        m.train(case.get("train", True))
    state = {k: tensor_json(v) for k, v in m.state_dict().items()} if m is not None else {}
    for i, x in enumerate(xs):
        env["x%d" % i] = x
    env["m"] = m
    out = eval(expr, env)
    outs = flatten(out, [])
    ups = []
    loss = None
    for o in outs:
        if o.is_floating_point() and o.requires_grad:
            w = torch.randn(o.shape, generator=gen)
            ups.append(numbers(w.reshape(-1).tolist()))
            term = (o * w).sum()
            loss = term if loss is None else loss + term
        else:
            ups.append(None)
    if loss is not None:
        loss.backward()
    record = {
        "name": name, "ctor": ctor, "expr": expr, "train": case.get("train", True),
        "tol": case.get("tol", 2e-5),
        "state": state,
        "inputs": [tensor_json(x) | {"grad": g} for x, g in zip(xs, grads)],
        "outputs": [tensor_json(o) for o in outs],
        "upstream": ups,
        "input_grads": [tensor_json(x.grad) if g and x.grad is not None else None for x, g in zip(xs, grads)],
        "param_grads": {k: tensor_json(p.grad) for k, p in m.named_parameters() if p.grad is not None} if m is not None else {},
        "buffers": {k: tensor_json(b) for k, b in m.named_buffers()} if m is not None else {},
    }
    return record


def cases_bugs():
    c = []
    add = lambda name, expr, inputs, ctor=None, **kw: c.append(dict(name=name, expr=expr, inputs=inputs, ctor=ctor, **kw))
    zeros4 = const([0.0, 0.0, 0.0, 0.0])
    y4 = const([1.0, 0.0, 1.0, 0.5], grad=False)
    add("bce_logits_zero", "F.binary_cross_entropy_with_logits(x0, x1)", [zeros4, y4])
    add("bce_logits_zero_posweight", "F.binary_cross_entropy_with_logits(x0, x1, pos_weight=torch.tensor([2.0, 2.0, 2.0, 2.0]), reduction='none')", [zeros4, y4])
    add("bce_logits_module", "m(x0, x1)", [const([0.0, 1.5, -2.0, 0.0]), y4], ctor="nn.BCEWithLogitsLoss(weight=torch.tensor([1.0, 2.0, 0.5, 1.0]), pos_weight=torch.tensor([3.0, 1.0, 1.0, 0.5]), reduction='sum')")
    add("bce_logits_random", "F.binary_cross_entropy_with_logits(x0, x1)", [randn(3, 5), ("rand01", [3, 5])])
    add("ce_ignore_index", "F.cross_entropy(x0, x1)", [randn(4, 5), const([1, -100, 4, 0], "int64")])
    add("ce_ignore_weight_sum", "F.cross_entropy(x0, x1, weight=torch.tensor([1.0, 2.0, 0.5, 1.5, 3.0]), ignore_index=2, reduction='sum')", [randn(4, 5), const([1, 2, 4, 0], "int64")])
    add("ce_ignore_weight_mean", "F.cross_entropy(x0, x1, weight=torch.tensor([1.0, 2.0, 0.5, 1.5, 3.0]), ignore_index=2)", [randn(4, 5), const([1, 2, 4, 0], "int64")])
    add("ce_ignore_smoothing", "F.cross_entropy(x0, x1, label_smoothing=0.2)", [randn(4, 5), const([1, -100, 4, 0], "int64")])
    add("ce_ignore_smoothing_weight", "F.cross_entropy(x0, x1, weight=torch.tensor([1.0, 2.0, 0.5, 1.5, 3.0]), label_smoothing=0.1)", [randn(4, 5), const([1, -100, 4, 0], "int64")])
    add("ce_ignore_none", "F.cross_entropy(x0, x1, reduction='none')", [randn(4, 5), const([1, -100, 4, 0], "int64")])
    add("ce_nd_ignore", "F.cross_entropy(x0, x1)", [randn(2, 3, 4), const([[0, 1, -100, 2], [2, -100, 1, 0]], "int64")])
    add("ce_module_args", "m(x0, x1)", [randn(4, 5), const([1, 3, 4, 0], "int64")], ctor="nn.CrossEntropyLoss(weight=torch.tensor([1.0, 2.0, 0.5, 1.5, 3.0]), ignore_index=3, label_smoothing=0.1)")
    add("ce_all_ignored_sum", "F.cross_entropy(x0, x1, reduction='sum')", [randn(2, 3), const([-100, -100], "int64")])
    add("nll_weight_ignore", "F.nll_loss(F.log_softmax(x0, 1), x1, weight=torch.tensor([0.5, 1.0, 2.0]), ignore_index=0)", [randn(5, 3), const([0, 1, 2, 2, 1], "int64")])
    add("nll_nd", "F.nll_loss(F.log_softmax(x0, 1), x1, reduction='none')", [randn(2, 3, 2, 2), ints(0, 3, 2, 2, 2)])
    add("nll_module", "m(x0, x1)", [randn(4, 3), const([0, -100, 2, 1], "int64")], ctor="nn.NLLLoss()")
    add("normalize_zero_row", "F.normalize(x0)", [const([[0.0, 0.0, 0.0], [1.0, 2.0, 2.0]])])
    add("normalize_p1_dim0", "F.normalize(x0, p=1.0, dim=0)", [const([[0.0, 1.0], [0.0, -3.0]])])
    add("cosine_zero_row", "F.cosine_similarity(x0, x1)", [const([[0.0, 0.0], [3.0, 4.0]]), const([[1.0, 1.0], [1.0, -1.0]])])
    add("cosine_eps_separate", "F.cosine_similarity(x0, x1)", [const([[1e-5, 0.0], [1e-3, 0.0]], grad=False), const([[2e-5, 0.0], [1e-6, 0.0]], grad=False)])
    add("cosine_eps_clamped_grad", "F.cosine_similarity(x0, x1)", [const([[1e-9, 2e-9], [0.3, 0.4]]), const([[1.0, -1.0], [2.0, 0.5]])])
    add("cosine_broadcast_dim", "F.cosine_similarity(x0, x1, dim=-1)", [randn(3, 4), randn(1, 4)])
    add("dropout_p1", "F.dropout(x0, 1.0)", [randn(3, 2)])
    add("dropout_module_p1", "m(x0)", [randn(3, 2)], ctor="nn.Dropout(p=1.0)")
    add("softmax_module_default_3d", "m(x0)", [randn(2, 3, 2)], ctor="nn.Softmax()")
    add("logsoftmax_module_default_3d", "m(x0)", [randn(2, 3, 2)], ctor="nn.LogSoftmax()")
    add("softmax_module_default_4d", "m(x0)", [randn(2, 3, 2, 2)], ctor="nn.Softmax()")
    add("softmin_module_default_1d", "m(x0)", [randn(4)], ctor="nn.Softmin()")
    add("relu_inplace_value", "F.relu(x0.clone(), inplace=True)", [randn(5)])
    add("gru_cell_unbatched", "m(x0, x1)", [randn(3), randn(4)], ctor="nn.GRUCell(3, 4)")
    add("lstm_cell_unbatched", "m(x0, (x1, x2))", [randn(3), randn(4), randn(4)], ctor="nn.LSTMCell(3, 4)")
    add("rnn_cell_relu", "m(x0)", [randn(2, 3)], ctor="nn.RNNCell(3, 4, nonlinearity='relu')")
    add("conv1d_stride_tuple", "m(x0)", [randn(2, 4, 9)], ctor="nn.Conv1d(4, 6, 3, stride=(2,), padding=(1,), dilation=2, groups=2)")
    add("conv1d_unbatched", "m(x0)", [randn(4, 7)], ctor="nn.Conv1d(4, 2, 3)")
    add("conv1d_same", "m(x0)", [randn(2, 3, 8)], ctor="nn.Conv1d(3, 2, 4, padding='same')")
    add("conv2d_same_even", "m(x0)", [randn(1, 2, 5, 6)], ctor="nn.Conv2d(2, 3, (2, 4), padding='same', dilation=(2, 1))")
    add("conv2d_valid", "m(x0)", [randn(1, 2, 5, 6)], ctor="nn.Conv2d(2, 3, 3, padding='valid')")
    add("layernorm_no_bias", "m(x0)", [randn(2, 3, 4)], ctor="nn.LayerNorm(4, bias=False)")
    add("linear_zero_in", "m(x0)", [randn(3, 0)], ctor="nn.Linear(0, 2)")
    add("embedding_padding_neg", "m(x0)", [const([[0, 3], [2, 3]], "int64")], ctor="nn.Embedding(4, 3, padding_idx=-1)")
    return c


def cases_layers():
    c = []
    add = lambda name, ctor, expr, inputs, **kw: c.append(dict(name=name, expr=expr, inputs=inputs, ctor=ctor, **kw))
    x4 = randn(2, 3, 6, 5)
    add("batchnorm2d_train", "nn.BatchNorm2d(3)", "(m(x0), m(x0 * 2 + 1))", [x4])
    add("batchnorm2d_cumulative", "nn.BatchNorm2d(3, momentum=None)", "(m(x0), m(x0 * 0.5 - 1))", [x4])
    add("batchnorm2d_eval", "nn.BatchNorm2d(3)", "m(x0)", [x4], train=False)
    add("batchnorm1d_2d", "nn.BatchNorm1d(4, affine=False)", "m(x0)", [randn(5, 4)])
    add("batchnorm1d_3d", "nn.BatchNorm1d(3)", "m(x0)", [randn(4, 3, 5)])
    add("batchnorm1d_no_stats", "nn.BatchNorm1d(3, track_running_stats=False)", "m(x0)", [randn(4, 3)], train=False)
    add("instancenorm2d_affine_stats", "nn.InstanceNorm2d(3, affine=True, track_running_stats=True)", "m(x0)", [x4])
    add("instancenorm1d_unbatched", "nn.InstanceNorm1d(3)", "m(x0)", [randn(3, 6)])
    add("groupnorm", "nn.GroupNorm(2, 4)", "m(x0)", [randn(3, 4, 5)])
    add("rmsnorm", "nn.RMSNorm([3, 4], eps=1e-6)", "m(x0)", [randn(2, 3, 4)])
    add("rmsnorm_default_eps", "nn.RMSNorm(4)", "m(x0)", [randn(3, 4)])
    add("localresponsenorm", "nn.LocalResponseNorm(3)", "m(x0)", [randn(2, 5, 3, 3)])
    add("maxpool2d_pad_dil_ceil", "nn.MaxPool2d(3, 2, padding=1, dilation=2, ceil_mode=True, return_indices=True)", "m(x0)", [randn(2, 2, 9, 8)])
    add("maxpool2d_default", "nn.MaxPool2d(2)", "m(x0)", [randn(1, 2, 5, 4)])
    add("maxpool2d_unbatched", "nn.MaxPool2d((2, 3), stride=(1, 2))", "m(x0)", [randn(2, 5, 7)])
    add("maxpool1d_idx", "nn.MaxPool1d(3, stride=2, padding=1, return_indices=True, ceil_mode=True)", "m(x0)", [randn(2, 3, 8)])
    add("avgpool2d_excl_pad", "nn.AvgPool2d(3, 2, padding=1, count_include_pad=False)", "m(x0)", [randn(2, 2, 7, 6)])
    add("avgpool2d_ceil", "nn.AvgPool2d(3, 2, padding=1, ceil_mode=True)", "m(x0)", [randn(1, 2, 6, 6)])
    add("avgpool2d_divisor", "nn.AvgPool2d(2, divisor_override=3)", "m(x0)", [randn(1, 2, 4, 4)])
    add("avgpool1d", "nn.AvgPool1d(3, 2, padding=1)", "m(x0)", [randn(2, 3, 7)])
    add("adaptive_avg2d_nondiv", "nn.AdaptiveAvgPool2d((3, 4))", "m(x0)", [randn(2, 2, 7, 9)])
    add("adaptive_avg2d_none", "nn.AdaptiveAvgPool2d((None, 2))", "m(x0)", [randn(1, 2, 3, 6)])
    add("adaptive_avg1d", "nn.AdaptiveAvgPool1d(3)", "m(x0)", [randn(2, 2, 7)])
    add("adaptive_max2d_nondiv", "nn.AdaptiveMaxPool2d((3, 2), return_indices=True)", "m(x0)", [randn(2, 2, 7, 5)])
    add("adaptive_max2d_div", "nn.AdaptiveMaxPool2d(2)", "m(x0)", [randn(1, 3, 4, 4)])
    add("adaptive_max1d", "nn.AdaptiveMaxPool1d(3, return_indices=True)", "m(x0)", [randn(2, 2, 8)])
    add("convtranspose2d", "nn.ConvTranspose2d(4, 6, 3, stride=2, padding=1, output_padding=1, groups=2)", "m(x0)", [randn(2, 4, 3, 4)])
    add("convtranspose2d_dil", "nn.ConvTranspose2d(2, 3, (2, 3), stride=(1, 2), padding=(0, 1), dilation=(2, 1), bias=False)", "m(x0)", [randn(1, 2, 4, 3)])
    add("convtranspose2d_output_size", "nn.ConvTranspose2d(2, 2, 3, stride=2)", "m(x0, output_size=[8, 8])", [randn(1, 2, 3, 3)])
    add("convtranspose1d", "nn.ConvTranspose1d(3, 2, 4, stride=3, padding=1)", "m(x0)", [randn(2, 3, 5)])
    add("unfold", "nn.Unfold((2, 3), dilation=(1, 2), padding=1, stride=2)", "m(x0)", [randn(2, 2, 5, 6)])
    add("fold", "nn.Fold((5, 6), (2, 3), dilation=(1, 2), padding=1, stride=2)", "m(x0)", [randn(2, 12, 6)])
    add("pixel_shuffle", "nn.PixelShuffle(2)", "m(x0)", [randn(1, 8, 2, 3)])
    add("pixel_unshuffle", "nn.PixelUnshuffle(2)", "m(x0)", [randn(1, 2, 4, 6)])
    add("upsample_nearest_scale", "nn.Upsample(scale_factor=2)", "m(x0)", [randn(1, 2, 3, 3)])
    add("interp_nearest_size", None, "F.interpolate(x0, size=(5, 7))", [randn(1, 2, 3, 4)])
    add("interp_nearest_frac", None, "F.interpolate(x0, scale_factor=1.7)", [randn(1, 1, 4, 5)])
    add("interp_nearest_exact", None, "F.interpolate(x0, size=(3, 7), mode='nearest-exact')", [randn(1, 1, 5, 4)])
    add("interp_bilinear", None, "F.interpolate(x0, size=(5, 7), mode='bilinear')", [randn(1, 2, 3, 4)])
    add("interp_bilinear_ac", None, "F.interpolate(x0, size=(5, 7), mode='bilinear', align_corners=True)", [randn(1, 2, 3, 4)])
    add("interp_bilinear_scale", None, "F.interpolate(x0, scale_factor=(1.5, 0.75), mode='bilinear')", [randn(1, 1, 4, 8)])
    add("interp_linear", None, "F.interpolate(x0, size=9, mode='linear')", [randn(2, 2, 4)])
    add("interp_area", None, "F.interpolate(x0, size=(2, 3), mode='area')", [randn(1, 2, 5, 7)])
    add("upsampling_bilinear2d", "nn.UpsamplingBilinear2d(scale_factor=2)", "m(x0)", [randn(1, 1, 3, 2)])
    add("pad_reflect", None, "F.pad(x0, (2, 1, 1, 2), mode='reflect')", [randn(1, 2, 4, 5)])
    add("pad_replicate", None, "F.pad(x0, (2, 1, 1, 3), mode='replicate')", [randn(1, 2, 4, 5)])
    add("pad_circular", None, "F.pad(x0, (1, 2), mode='circular')", [randn(2, 3, 4)])
    add("pad_negative", None, "F.pad(x0, (-1, 2, 1, -2), value=0.5)", [randn(2, 4, 5)])
    add("reflectionpad2d", "nn.ReflectionPad2d((1, 0, 2, 1))", "m(x0)", [randn(1, 1, 3, 3)])
    add("zeropad2d", "nn.ZeroPad2d(1)", "m(x0)", [randn(1, 1, 2, 2)])
    add("embedding_from_pretrained", "nn.Embedding.from_pretrained(torch.arange(12.0).reshape(4, 3), freeze=False)", "m(x0)", [const([[3, 1], [0, 3]], "int64")], perturb=False)
    add("embedding_max_norm", "nn.Embedding(5, 3, max_norm=1.0)", "(m(x0), m.weight * 1)", [const([1, 3, 3], "int64")])
    add("embeddingbag_mean_offsets", "nn.EmbeddingBag(6, 3)", "m(x0, x1)", [const([1, 2, 4, 5, 4, 3], "int64"), const([0, 2, 2, 5], "int64")])
    add("embeddingbag_sum_psw", "nn.EmbeddingBag(6, 3, mode='sum')", "m(x0, x1, per_sample_weights=x2)", [const([1, 2, 4, 5], "int64"), const([0, 1], "int64"), randn(4)])
    add("embeddingbag_max_2d", "nn.EmbeddingBag(6, 3, mode='max')", "m(x0)", [const([[1, 2], [4, 5], [0, 0]], "int64")])
    add("embeddingbag_padding", "nn.EmbeddingBag(6, 3, padding_idx=0)", "m(x0)", [const([[1, 0], [0, 0], [3, 2]], "int64")])
    add("bilinear", "nn.Bilinear(3, 2, 4)", "m(x0, x1)", [randn(5, 3), randn(5, 2)])
    add("prelu_channels", "nn.PReLU(3)", "m(x0)", [randn(2, 3, 4)])
    add("flatten_unflatten", "nn.Sequential(nn.Flatten(0, 1), nn.Unflatten(1, (2, 2)))", "m(x0)", [randn(2, 3, 4)])
    add("mha_basic", "nn.MultiheadAttention(8, 2)", "m(x0, x1, x1)", [randn(4, 2, 8), randn(5, 2, 8)], tol=5e-5)
    add("mha_masks_batch_first", "nn.MultiheadAttention(8, 4, batch_first=True)", "m(x0, x0, x0, key_padding_mask=torch.tensor([[False, False, True, False], [False, True, True, False]]), attn_mask=torch.tensor([[False, True, False, False], [False, False, False, True], [True, False, False, False], [False, False, False, False]]), average_attn_weights=False)", [randn(2, 4, 8)], tol=5e-5)
    add("mha_float_mask_no_weights", "nn.MultiheadAttention(6, 3, bias=False)", "m(x0, x0, x0, attn_mask=x1, need_weights=False)", [randn(3, 2, 6), nograd(randn(3, 3))], tol=5e-5)
    add("mha_kdim_vdim_bias_kv", "nn.MultiheadAttention(4, 2, kdim=3, vdim=5, add_bias_kv=True, add_zero_attn=True)", "m(x0, x1, x2)", [randn(3, 2, 4), randn(4, 2, 3), randn(4, 2, 5)], tol=5e-5)
    add("mha_unbatched", "nn.MultiheadAttention(4, 2)", "m(x0, x0, x0)", [randn(3, 4)], tol=5e-5)
    add("sdpa_causal", None, "F.scaled_dot_product_attention(x0, x1, x2, is_causal=True)", [randn(2, 3, 4, 5), randn(2, 3, 4, 5), randn(2, 3, 4, 5)], tol=5e-5)
    add("sdpa_bool_mask_scale", None, "F.scaled_dot_product_attention(x0, x1, x2, attn_mask=torch.tensor([[True, False, True], [True, True, False]]), scale=0.3)", [randn(2, 2, 4), randn(2, 3, 4), randn(2, 3, 3)], tol=5e-5)
    add("sdpa_float_mask", None, "F.scaled_dot_product_attention(x0, x1, x2, attn_mask=x3)", [randn(1, 2, 3, 4), randn(1, 2, 5, 4), randn(1, 2, 5, 2), randn(2, 3, 5)], tol=5e-5)
    add("transformer_encoder_layer", "nn.TransformerEncoderLayer(8, 2, 16, dropout=0.0)", "m(x0, src_key_padding_mask=torch.tensor([[False, False, True], [False, False, False]]))", [randn(3, 2, 8)], tol=5e-5)
    add("transformer_encoder_layer_nf_gelu", "nn.TransformerEncoderLayer(8, 2, 16, dropout=0.0, activation='gelu', norm_first=True, batch_first=True)", "m(x0, src_mask=nn.Transformer.generate_square_subsequent_mask(3))", [randn(2, 3, 8)], tol=5e-5)
    add("transformer_encoder", "nn.TransformerEncoder(nn.TransformerEncoderLayer(8, 2, 16, dropout=0.0), 2, norm=nn.LayerNorm(8), enable_nested_tensor=False)", "m(x0)", [randn(4, 2, 8)], tol=5e-5)
    add("transformer_decoder_layer", "nn.TransformerDecoderLayer(8, 2, 16, dropout=0.0, batch_first=True)", "m(x0, x1, tgt_mask=nn.Transformer.generate_square_subsequent_mask(3))", [randn(2, 3, 8), randn(2, 4, 8)], tol=5e-5)
    add("lstm_bidir_2layer", "nn.LSTM(3, 4, num_layers=2, bidirectional=True)", "m(x0)", [randn(5, 2, 3)], tol=5e-5)
    add("lstm_batch_first_hx", "nn.LSTM(3, 4, batch_first=True)", "m(x0, (x1, x2))", [randn(2, 4, 3), randn(1, 2, 4), randn(1, 2, 4)], tol=5e-5)
    add("lstm_proj", "nn.LSTM(3, 5, proj_size=2, num_layers=2)", "m(x0)", [randn(4, 2, 3)], tol=5e-5)
    add("lstm_unbatched", "nn.LSTM(3, 4)", "m(x0)", [randn(5, 3)], tol=5e-5)
    add("gru_bidir", "nn.GRU(3, 4, bidirectional=True, bias=False)", "m(x0, x1)", [randn(4, 2, 3), randn(2, 2, 4)], tol=5e-5)
    add("rnn_relu_2layer", "nn.RNN(3, 4, 2, nonlinearity='relu', batch_first=True)", "m(x0)", [randn(2, 5, 3)], tol=5e-5)
    add("rnn_tanh", "nn.RNN(3, 4)", "m(x0)", [randn(5, 2, 3)], tol=5e-5)
    add("lstm_packed_unsorted", "nn.LSTM(3, 4, bidirectional=True)", "(lambda out: (pad_packed_sequence(out[0]), out[1]))(m(pack_padded_sequence(x0, [3, 5, 2], enforce_sorted=False)))", [randn(5, 3, 3)], tol=5e-5)
    add("gru_packed_batch_first", "nn.GRU(3, 4, batch_first=True, num_layers=2)", "(lambda out: (pad_packed_sequence(out[0], batch_first=True), out[1]))(m(pack_padded_sequence(x0, [4, 4, 1], batch_first=True)))", [randn(3, 4, 3)], tol=5e-5)
    add("pad_sequence_pack_sequence", None, "(pad_sequence([x0, x1], batch_first=True, padding_value=-1.0), pack_sequence([x1, x0], enforce_sorted=False))", [randn(3, 2), randn(5, 2)])
    for act, extra in [("LeakyReLU", "0.2"), ("ELU", "0.7"), ("Softplus", "2.0, 1.5"), ("ReLU6", ""), ("Hardtanh", "-0.5, 0.8"), ("Mish", ""), ("SELU", ""), ("CELU", "1.3"), ("GLU", ""), ("Hardswish", ""), ("Hardsigmoid", ""), ("LogSigmoid", ""), ("Softmin", "1"), ("Softsign", ""), ("Tanhshrink", ""), ("Threshold", "0.1, -2.0"), ("SiLU", ""), ("Hardshrink", "0.3"), ("Softshrink", "0.3"), ("GELU", "approximate='tanh'"), ("Softmax2d", "")]:
        # Include the kinks exactly: 0, +-3 and the thresholds.
        add("act_" + act.lower(), "nn.%s(%s)" % (act, extra), "m(x0)", [("const", [[-4.0, -3.0, -1.0, -0.5, -0.3, 0.0, 0.1, 0.3, 0.5, 0.8, 3.0, 25.0], [0.2, -0.2, 1.5, -1.5, 2.5, -2.5, 4.0, -6.0, 6.0, 0.05, -0.05, 1.0]], "float32", True)] if act != "Softmax2d" else [randn(2, 2, 3)])
    add("act_prelu_scalar_kink", "nn.PReLU()", "m(x0)", [const([-1.0, 0.0, 2.0])])
    add("act_relu6_kinks", "nn.ReLU6()", "m(x0)", [const([-1.0, 0.0, 3.0, 6.0, 7.0])])
    add("act_logsigmoid_zero", "nn.LogSigmoid()", "m(x0)", [const([0.0, -40.0, 40.0])])
    add("cosine_similarity_module", "nn.CosineSimilarity(dim=0, eps=1e-6)", "m(x0, x1)", [randn(4, 3), randn(4, 3)])
    add("pairwise_distance_module", "nn.PairwiseDistance(p=1.5, keepdim=True)", "m(x0, x1)", [randn(3, 4), randn(3, 4)])
    add("pairwise_distance_equal", None, "F.pairwise_distance(x0, x0.detach() + 1e-6 * 0)", [randn(2, 3)])
    add("dropout1d_eval", "nn.Dropout1d(0.4)", "m(x0)", [randn(2, 3, 4)], train=False)
    add("identity_args", "nn.Identity(3, foo=4)", "m(x0)", [randn(2)])
    add("weight_norm_linear", "nn.utils.weight_norm(nn.Linear(3, 2))", "m(x0)", [randn(4, 3)])
    add("transformer_full", "nn.Transformer(8, 2, 1, 1, 16, dropout=0.0)", "m(x0, x1, tgt_mask=nn.Transformer.generate_square_subsequent_mask(2))", [randn(3, 2, 8), randn(2, 2, 8)], tol=5e-5)
    add("transformer_decoder", "nn.TransformerDecoder(nn.TransformerDecoderLayer(8, 2, 16, dropout=0.0, norm_first=True), 2, norm=nn.LayerNorm(8))", "m(x0, x1, memory_key_padding_mask=torch.tensor([[False, True, False], [False, False, False]]))", [randn(2, 2, 8), randn(3, 2, 8)], tol=5e-5)
    add("pdist", None, "F.pdist(x0)", [randn(4, 3)])
    add("lp_pool2d", None, "F.lp_pool2d(x0, 2, 2)", [("pos", [1, 2, 4, 4])])
    add("channel_shuffle", None, "F.channel_shuffle(x0, 2)", [randn(1, 4, 2, 2)])
    add("rrelu_eval", "nn.RReLU()", "m(x0)", [randn(2, 5)], train=False)
    add("rms_norm_float64", None, "F.rms_norm(x0.double(), [4])", [randn(3, 4)])
    add("embedding_scale_grad_by_freq", None, "F.embedding(x0, x1, scale_grad_by_freq=True)", [const([[0, 2, 2], [2, 1, 0]], "int64"), randn(4, 3)])
    add("interp_trilinear", None, "F.interpolate(x0, size=(3, 4, 2), mode='trilinear')", [randn(1, 1, 2, 3, 3)])
    add("interp_nearest_3d", None, "F.interpolate(x0, scale_factor=2)", [randn(1, 1, 2, 2, 2)])
    add("circularpad1d", "nn.CircularPad1d((2, 1))", "m(x0)", [randn(2, 3, 4)])
    add("constantpad2d_value", "nn.ConstantPad2d((1, 0, 2, 1), 1.5)", "m(x0)", [randn(1, 2, 2)])
    return c


def cases_losses():
    c = []
    add = lambda name, expr, inputs, ctor=None, **kw: c.append(dict(name=name, expr=expr, inputs=inputs, ctor=ctor, **kw))
    for red in ("mean", "sum", "none"):
        add("mse_" + red, "m(x0, x1)", [randn(3, 4), randn(3, 4)], ctor="nn.MSELoss(reduction='%s')" % red)
        add("l1_" + red, "m(x0, x1)", [randn(3, 4), randn(3, 4)], ctor="nn.L1Loss(reduction='%s')" % red)
        add("smoothl1_" + red, "m(x0, x1)", [randn(3, 4), randn(3, 4)], ctor="nn.SmoothL1Loss(reduction='%s', beta=0.7)" % red)
        add("huber_" + red, "m(x0, x1)", [randn(3, 4), randn(3, 4)], ctor="nn.HuberLoss(reduction='%s', delta=0.6)" % red)
        add("bce_" + red, "m(x0, x1)", [("rand01", [3, 4]), ("rand01", [3, 4])], ctor="nn.BCELoss(weight=torch.linspace(0.5, 1.5, 4), reduction='%s')" % red)
        add("soft_margin_" + red, "m(x0, x1)", [randn(3, 4), ("sign", [3, 4])], ctor="nn.SoftMarginLoss(reduction='%s')" % red)
        add("multilabel_soft_margin_" + red, "m(x0, x1)", [randn(3, 4), ("const", [[1.0, 0.0, 1.0, 0.0], [0.0, 0.0, 1.0, 1.0], [1.0, 1.0, 1.0, 0.0]], "float32", False)], ctor="nn.MultiLabelSoftMarginLoss(weight=torch.tensor([1.0, 0.5, 2.0, 1.5]), reduction='%s')" % red)
        add("hinge_" + red, "m(x0, x1)", [randn(3, 4), ("sign", [3, 4])], ctor="nn.HingeEmbeddingLoss(margin=0.5, reduction='%s')" % red)
        add("margin_ranking_" + red, "m(x0, x1, x2)", [randn(5), randn(5), ("sign", [5])], ctor="nn.MarginRankingLoss(margin=0.2, reduction='%s')" % red)
        add("cosine_embedding_" + red, "m(x0, x1, x2)", [randn(4, 3), randn(4, 3), ("const", [1.0, -1.0, -1.0, 1.0], "float32", False)], ctor="nn.CosineEmbeddingLoss(margin=0.1, reduction='%s')" % red)
        add("triplet_" + red, "m(x0, x1, x2)", [randn(4, 3), randn(4, 3), randn(4, 3)], ctor="nn.TripletMarginLoss(margin=1.5, swap=True, reduction='%s')" % red)
        add("poisson_" + red, "m(x0, x1)", [randn(3, 4), ("pos", [3, 4])], ctor="nn.PoissonNLLLoss(reduction='%s')" % red)
        add("gaussian_" + red, "m(x0, x1, x2)", [randn(3, 4), randn(3, 4), ("pos", [3, 4])], ctor="nn.GaussianNLLLoss(full=True, reduction='%s')" % red)
        add("multi_margin_" + red, "m(x0, x1)", [randn(3, 4), const([0, 3, 1], "int64")], ctor="nn.MultiMarginLoss(p=2, margin=0.8, weight=torch.tensor([1.0, 2.0, 0.5, 1.5]), reduction='%s')" % red)
        add("nll_" + red, "m(F.log_softmax(x0, 1), x1)", [randn(4, 3), const([0, 2, 1, 2], "int64")], ctor="nn.NLLLoss(weight=torch.tensor([1.0, 0.5, 2.0]), reduction='%s')" % red)
        add("bce_logits_" + red, "m(x0, x1)", [randn(3, 4), ("rand01", [3, 4])], ctor="nn.BCEWithLogitsLoss(pos_weight=torch.linspace(0.5, 2.0, 4), reduction='%s')" % red)
        add("ce_prob_" + red, "m(x0, F.softmax(x1, 1))", [randn(3, 4), nograd(randn(3, 4))], ctor="nn.CrossEntropyLoss(label_smoothing=0.1, reduction='%s')" % red)
    for red in ("batchmean", "sum", "mean", "none"):
        add("kldiv_" + red, "m(F.log_softmax(x0, 1), F.softmax(x1, 1))", [randn(3, 4), randn(3, 4)], ctor="nn.KLDivLoss(reduction='%s')" % red)
    add("kldiv_log_target", "m(F.log_softmax(x0, 1), F.log_softmax(x1, 1))", [randn(3, 4), randn(3, 4)], ctor="nn.KLDivLoss(reduction='batchmean', log_target=True)")
    add("kldiv_zero_target", "F.kl_div(x0, x1, reduction='sum')", [randn(4), const([0.0, 0.5, 0.0, 0.5], grad=False)])
    add("poisson_no_log", "F.poisson_nll_loss(x0, x1, log_input=False, full=True)", [("pos", [5]), const([0.0, 1.0, 2.0, 3.5, 0.5], grad=False)])
    add("gaussian_var_shapes", "F.gaussian_nll_loss(x0, x1, x2)", [randn(3, 4), randn(3, 4), ("pos", [3])])
    add("gaussian_var_small", "F.gaussian_nll_loss(x0, x1, x2, eps=0.1)", [randn(3, 2), randn(3, 2), const([[0.01, 0.5], [0.2, 0.05], [1.0, 0.0]])])
    add("smooth_l1_beta0", "F.smooth_l1_loss(x0, x1, beta=0.0)", [randn(5), randn(5)])
    add("mse_legacy_flags", "F.mse_loss(x0, x1, size_average=False)", [randn(5), randn(5)])
    add("l1_weight", "F.l1_loss(x0, x1, weight=torch.tensor([[1.0, 2.0, 3.0], [0.5, 1.0, 0.0]]))", [randn(2, 3), randn(2, 3)])
    add("triplet_p1", "F.triplet_margin_loss(x0, x1, x2, p=1)", [randn(4, 3), randn(4, 3), randn(4, 3)])
    return c


def dump(name, cases):
    # Zipp has no eval(): each case's constructor and expression become functions.
    records = [run_case(case, i) for i, case in enumerate(cases)]
    lines = ["# Generated by gen.py from PyTorch %s; do not edit." % torch.__version__, "CASES = ["]
    for rec in records:
        lines.append("    %s," % json.dumps(rec, separators=(",", ":")))
    lines.append("]")
    for i, case in enumerate(cases):
        args = ", ".join(["m"] + ["x%d" % k for k in range(len(case["inputs"]))])
        lines += ["", "", "def ctor_%d():" % i, "    return %s" % (case["ctor"] or "None")]
        lines += ["", "", "def expr_%d(%s):" % (i, args), "    return %s" % case["expr"]]
    lines += ["", "", "BUILDERS = [%s]" % ", ".join("(ctor_%d, expr_%d)" % (i, i) for i in range(len(cases)))]
    text = "\n".join(lines) + "\n"
    with open(os.path.join(HERE, "parity_%s.py" % name), "w", encoding="utf-8", newline="\n") as f:
        f.write(text)
    print(name, len(records), "cases")


if __name__ == "__main__":
    torch.set_default_dtype(torch.float32)
    dump("bugs", cases_bugs())
    dump("layers", cases_layers())
    dump("losses", cases_losses())
    out = subprocess.run([sys.executable, "-W", "ignore", os.path.join(HERE, "api.py")], capture_output=True, text=True, check=True).stdout
    with open(os.path.join(HERE, "api_expected.txt"), "w", encoding="utf-8", newline="\n") as f:
        f.write(out)
    print("api", len(out.splitlines()), "lines")
