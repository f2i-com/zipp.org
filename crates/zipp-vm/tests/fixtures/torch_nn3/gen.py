"""Generate the third set of torch.nn parity fixtures with CPU PyTorch (2.11).

    python crates/zipp-vm/tests/fixtures/torch_nn3/gen.py

Same record format and runner as ../torch_nn (gen.py there builds each case
in PyTorch, check.py rebuilds it in Zipp, loads the recorded state_dict and
compares outputs, input and parameter gradients and buffers). Groups:
grid_sample/affine_grid, CTC loss, fractional max pooling with convolution
padding modes and SyncBatchNorm, second derivatives through convolutions,
and the parallel wrappers. api.py prints behaviour (wrapper
attributes and state_dict keys, lazy-parameter errors, dropout's kept
values) and runs unchanged on both; its PyTorch output is api_expected.txt.

CUDA is hidden so DataParallel behaves as on a CPU-only machine, and a
one-process gloo group lets PyTorch build DistributedDataParallel (Zipp's
needs none).
"""
import json
import os
import subprocess
import sys

os.environ["CUDA_VISIBLE_DEVICES"] = "-1"
import torch  # noqa: E402
import torch.distributed as dist  # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "torch_nn"))
import gen as base  # noqa: E402  (run_case and the input specs)

randn, const, nograd = base.randn, base.const, base.nograd


def rand01(*shape):
    return ("rand01", list(shape))


def cases_grid():
    c = []
    add = lambda name, ctor, expr, inputs, **kw: c.append(dict(name=name, expr=expr, inputs=inputs, ctor=ctor, **kw))
    img = randn(2, 3, 5, 6)
    grid = randn(2, 4, 5, 2)
    for mode in ("bilinear", "nearest", "bicubic"):
        for pad in ("zeros", "border", "reflection"):
            for ac in (False, True):
                name = "grid2d_%s_%s%s" % (mode, pad, "_ac" if ac else "")
                scale = 2.6 if pad == "reflection" else 0.9
                add(name, None, "F.grid_sample(x0, x1 * %s, mode='%s', padding_mode='%s', align_corners=%s)" % (scale, mode, pad, ac), [img, grid])
    vol = randn(1, 2, 3, 4, 5)
    grid3 = randn(1, 2, 3, 4, 3)
    for mode in ("bilinear", "nearest"):
        for pad in ("zeros", "border", "reflection"):
            for ac in (False, True):
                name = "grid3d_%s_%s%s" % (mode, pad, "_ac" if ac else "")
                scale = 2.6 if pad == "reflection" else 0.9
                add(name, None, "F.grid_sample(x0, x1 * %s, mode='%s', padding_mode='%s', align_corners=%s)" % (scale, mode, pad, ac), [vol, grid3])
    add("grid2d_float64", None, "F.grid_sample(x0.double(), x1.double(), mode='bicubic', padding_mode='reflection', align_corners=False)", [randn(1, 2, 4, 3), randn(1, 3, 3, 2)])
    add("grid2d_default_args", None, "F.grid_sample(x0, x1 * 0.7)", [randn(1, 1, 3, 3), randn(1, 2, 2, 2)])
    add("grid2d_size_one", None, "(F.grid_sample(x0, x1, align_corners=True, padding_mode='reflection'), F.grid_sample(x0, x1, mode='bicubic', padding_mode='border'))", [randn(1, 2, 1, 4), randn(1, 2, 3, 2)])
    add("affine_grid_2d", None, "F.affine_grid(x0, (2, 3, 4, 5), align_corners=False)", [randn(2, 2, 3)])
    add("affine_grid_2d_ac", None, "F.affine_grid(x0, [1, 1, 3, 2], align_corners=True)", [randn(1, 2, 3)])
    add("affine_grid_3d", None, "F.affine_grid(x0, torch.Size([2, 1, 2, 3, 4]), align_corners=False)", [randn(2, 3, 4)])
    add("affine_grid_3d_ac", None, "F.affine_grid(x0, (1, 2, 3, 2, 2), align_corners=True)", [randn(1, 3, 4)])
    add("spatial_transformer", None, "F.grid_sample(x0, F.affine_grid(x1 * 0.5 + torch.tensor([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0]]), (2, 3, 4, 4), align_corners=False), align_corners=False)", [randn(2, 3, 5, 5), randn(2, 2, 3)])
    add("spatial_transformer_3d", None, "F.grid_sample(x0, F.affine_grid(x1 * 0.4 + torch.eye(3, 4), (1, 2, 3, 3, 2), align_corners=True), padding_mode='border', align_corners=True)", [randn(1, 2, 3, 4, 3), randn(1, 3, 4)])
    return c


def cases_ctc():
    c = []
    add = lambda name, ctor, expr, inputs, **kw: c.append(dict(name=name, expr=expr, inputs=inputs, ctor=ctor, **kw))
    T, N, C = 7, 3, 5
    x = randn(T, N, C)
    tg = const([[1, 2, 2, 0], [3, 1, 4, 2], [4, 4, 0, 0]], "int64")
    for red in ("mean", "sum", "none"):
        add("ctc_padded_" + red, None, "F.ctc_loss(x0.log_softmax(2), x1, (7, 6, 5), (3, 4, 2), reduction='%s')" % red, [x, tg])
    add("ctc_tensor_lengths", None, "F.ctc_loss(x0.log_softmax(2), x1, torch.tensor([7, 7, 4]), torch.tensor([3, 2, 1]))", [x, tg])
    add("ctc_int32_lengths", None, "F.ctc_loss(x0.log_softmax(2), x1.int(), torch.tensor([6, 7, 3], dtype=torch.int32), torch.tensor([2, 4, 2], dtype=torch.int32), reduction='sum')", [x, tg])
    add("ctc_concatenated", None, "F.ctc_loss(x0.log_softmax(2), x1, [7, 5, 6], [3, 1, 2], reduction='none')", [x, const([1, 1, 3, 4, 2, 2], "int64")])
    add("ctc_raw_log_probs", None, "F.ctc_loss(x0, x1, (7, 6, 5), (3, 4, 2), reduction='sum')", [x, tg])
    add("ctc_blank_last", None, "F.ctc_loss(x0.log_softmax(2), x1, (7, 7, 7), (2, 3, 1), blank=4)", [x, const([[0, 0, 1], [3, 2, 3], [1, 0, 0]], "int64")])
    add("ctc_empty_target", None, "F.ctc_loss(x0.log_softmax(2), x1, (7, 4, 6), (0, 2, 1), reduction='none')", [x, const([[1, 2], [2, 2], [3, 1]], "int64")])
    add("ctc_infeasible", None, "F.ctc_loss(x0.log_softmax(2), x1, (2, 7, 3), (4, 2, 3), reduction='none')", [x, const([[1, 2, 3, 4], [1, 1, 0, 0], [2, 2, 2, 0]], "int64")])
    add("ctc_zero_infinity", None, "F.ctc_loss(x0.log_softmax(2), x1, (2, 7, 3), (4, 2, 2), zero_infinity=True)", [x, const([[1, 2, 3, 4], [1, 1, 0, 0], [2, 2, 2, 0]], "int64")])
    add("ctc_zero_infinity_sum", None, "F.ctc_loss(x0.log_softmax(2), x1, (7, 1, 5), (3, 2, 2), reduction='sum', zero_infinity=True)", [x, tg])
    add("ctc_module", "nn.CTCLoss(blank=1, reduction='sum', zero_infinity=True)", "m(x0.log_softmax(2), x1, (7, 6, 7), (2, 3, 3))", [x, const([[2, 3, 0], [4, 4, 4], [3, 2, 3]], "int64")])
    add("ctc_module_default", "nn.CTCLoss()", "m(x0.log_softmax(2), x1, torch.tensor([7, 5, 6]), torch.tensor([4, 2, 3]))", [x, tg])
    add("ctc_unbatched", None, "(F.ctc_loss(x0.log_softmax(1), x1, (6,), (3,), reduction='none'), F.ctc_loss(x0.log_softmax(1), x1, torch.tensor([5]), torch.tensor(3)))", [randn(6, 4), const([1, 3, 3], "int64")])
    add("ctc_target_uses_blank", None, "F.ctc_loss(x0.log_softmax(2), x1, (7, 7, 6), (3, 2, 3), reduction='none')", [x, const([[2, 0, 1], [0, 0, 0], [1, 2, 0]], "int64")])
    add("ctc_float64", None, "F.ctc_loss(x0.double().log_softmax(2), x1, (7, 6, 5), (3, 4, 2))", [x, tg])
    add("ctc_long", None, "F.ctc_loss(x0.log_softmax(2), x1, (30, 26, 18), (8, 5, 7), reduction='none')", [randn(30, 3, 6), const([[1, 2, 2, 3, 5, 5, 4, 1], [5, 1, 1, 1, 2, 0, 0, 0], [3, 4, 3, 4, 3, 4, 3, 0]], "int64")])
    return c


def cases_pool():
    c = []
    add = lambda name, ctor, expr, inputs, **kw: c.append(dict(name=name, expr=expr, inputs=inputs, ctor=ctor, **kw))
    add("fmp2d_output_size", None, "F.fractional_max_pool2d(x0, 2, output_size=(3, 4), return_indices=True, _random_samples=x1)", [randn(2, 3, 7, 9), nograd(rand01(2, 3, 2))])
    add("fmp2d_kernel_rect", None, "F.fractional_max_pool2d(x0, (3, 2), output_size=5, _random_samples=x1)", [randn(1, 2, 8, 7), nograd(rand01(1, 2, 2))])
    add("fmp2d_ratio_tuple", None, "F.fractional_max_pool2d(x0, 2, output_ratio=(0.5, 0.7), return_indices=True, _random_samples=x1)", [randn(1, 2, 9, 10), nograd(rand01(1, 2, 2))])
    add("fmp2d_unbatched", None, "F.fractional_max_pool2d(x0, 2, output_size=(4, 3), return_indices=True, _random_samples=x1)", [randn(2, 6, 5), nograd(rand01(1, 2, 2))])
    add("fmp2d_full_and_one", None, "(F.fractional_max_pool2d(x0, 2, output_size=(5, 4), _random_samples=x1), F.fractional_max_pool2d(x0, 3, output_size=1, _random_samples=x1))", [randn(1, 1, 6, 5), nograd(rand01(1, 1, 2))])
    add("fmp2d_float64", None, "F.fractional_max_pool2d(x0.double(), 3, output_size=(4, 5), return_indices=True, _random_samples=x1.double())", [randn(2, 2, 9, 11), nograd(rand01(2, 2, 2))])
    add("fmp2d_module", "nn.FractionalMaxPool2d(3, output_ratio=0.5, return_indices=True, _random_samples=torch.rand(2, 2, 2))", "m(x0)", [randn(2, 2, 10, 9)])
    add("fmp2d_module_size", "nn.FractionalMaxPool2d((2, 3), output_size=(4, 4), _random_samples=torch.rand(1, 3, 2))", "m(x0)", [randn(1, 3, 8, 8)])
    add("fmp3d_output_size", None, "F.fractional_max_pool3d(x0, 2, output_size=(2, 3, 3), return_indices=True, _random_samples=x1)", [randn(2, 2, 5, 6, 7), nograd(rand01(2, 2, 3))])
    add("fmp3d_ratio", None, "F.fractional_max_pool3d(x0, (2, 1, 3), output_ratio=0.6, _random_samples=x1)", [randn(1, 2, 6, 5, 8), nograd(rand01(1, 2, 3))])
    add("fmp3d_unbatched", None, "F.fractional_max_pool3d(x0, 2, output_size=3, return_indices=True, _random_samples=x1)", [randn(2, 5, 5, 6), nograd(rand01(1, 2, 3))])
    add("fmp3d_module", "nn.FractionalMaxPool3d(2, output_size=(2, 2, 3), return_indices=True, _random_samples=torch.rand(1, 2, 3))", "m(x0)", [randn(1, 2, 4, 5, 6)])
    for mode in ("reflect", "replicate", "circular"):
        add("conv1d_" + mode, "nn.Conv1d(3, 4, 3, padding=2, dilation=2, padding_mode='%s')" % mode, "m(x0)", [randn(2, 3, 7)])
        add("conv2d_" + mode, "nn.Conv2d(4, 6, (3, 2), stride=(2, 1), padding=(1, 1), groups=2, padding_mode='%s')" % mode, "m(x0)", [randn(2, 4, 5, 6)])
        add("conv3d_" + mode, "nn.Conv3d(2, 3, 3, padding=(1, 2, 1), stride=(1, 2, 1), padding_mode='%s')" % mode, "m(x0)", [randn(1, 2, 4, 5, 4)])
    add("conv2d_same_reflect", "nn.Conv2d(2, 3, (2, 4), padding='same', dilation=(2, 1), padding_mode='reflect')", "m(x0)", [randn(1, 2, 5, 6)])
    add("conv1d_same_circular_unbatched", "nn.Conv1d(2, 2, 4, padding='same', padding_mode='circular')", "m(x0)", [randn(2, 6)])
    add("conv2d_replicate_unbatched", "nn.Conv2d(2, 2, 3, padding=1, padding_mode='replicate', bias=False)", "m(x0)", [randn(2, 4, 4)])
    add("conv3d_circular_unbatched", "nn.Conv3d(1, 2, 2, padding=1, padding_mode='circular')", "m(x0)", [randn(1, 3, 3, 3)])
    add("lazyconv2d_reflect", "(lambda m: (m(torch.zeros(1, 2, 5, 5)), m)[1])(nn.LazyConv2d(3, 3, padding=1, padding_mode='reflect'))", "m(x0)", [randn(1, 2, 5, 5)])
    add("syncbn_train", "nn.SyncBatchNorm(3)", "(m(x0), m(x0 * 2 + 1))", [randn(2, 3, 4, 5)])
    add("syncbn_eval", "nn.SyncBatchNorm(3)", "m(x0)", [randn(2, 3, 4, 5)], train=False)
    add("syncbn_1d_cumulative", "nn.SyncBatchNorm(4, momentum=None, affine=False)", "(m(x0), m(x0 - 1))", [randn(5, 4)])
    add("syncbn_5d_no_stats", "nn.SyncBatchNorm(2, track_running_stats=False)", "m(x0)", [randn(2, 2, 2, 3, 2)])
    add("convert_sync_batchnorm", "nn.SyncBatchNorm.convert_sync_batchnorm(nn.Sequential(nn.Conv2d(2, 3, 3), nn.BatchNorm2d(3), nn.ReLU(), nn.Sequential(nn.BatchNorm2d(3, momentum=0.3))))", "m(x0)", [randn(2, 2, 5, 5)])
    return c


def cases_parallel():
    c = []
    add = lambda name, ctor, expr, inputs, **kw: c.append(dict(name=name, expr=expr, inputs=inputs, ctor=ctor, **kw))
    add("data_parallel", "nn.DataParallel(nn.Sequential(nn.Linear(4, 3), nn.BatchNorm1d(3)), device_ids=[0], output_device=0)", "m(x0)", [randn(5, 4)])
    add("data_parallel_kwargs", "nn.DataParallel(nn.MultiheadAttention(4, 2, batch_first=True))", "m(x0, x0, x0, need_weights=False)[0]", [randn(2, 3, 4)])
    add("ddp", "nn.parallel.DistributedDataParallel(nn.Sequential(nn.Conv1d(2, 3, 3), nn.BatchNorm1d(3)))", "m(x0)", [randn(2, 2, 6)])
    add("ddp_options", "nn.parallel.DistributedDataParallel(nn.Linear(3, 2), broadcast_buffers=False, find_unused_parameters=True, gradient_as_bucket_view=True)", "m(x0)", [randn(4, 3)])
    return c


GRAD2 = "(lambda g: (g ** 2).sum())(torch.autograd.grad((m(x0) ** 2).sum(), %s, create_graph=True)[0])"


def cases_dbl():
    c = []
    add = lambda name, ctor, expr, inputs, **kw: c.append(dict(name=name, expr=expr, inputs=inputs, ctor=ctor, **kw))
    specs = [
        ("conv1d_fast", "nn.Conv1d(3, 2, 3, padding=1)", [randn(2, 3, 6)]),
        ("conv1d_general", "nn.Conv1d(4, 6, 3, stride=2, dilation=2, groups=2)", [randn(2, 4, 9)]),
        ("conv2d", "nn.Conv2d(2, 3, 3, stride=2, padding=1)", [randn(2, 2, 6, 5)]),
        ("conv2d_groups_dil", "nn.Conv2d(4, 4, (2, 3), padding=(1, 2), dilation=(2, 1), groups=2, bias=False)", [randn(1, 4, 5, 5)]),
        ("conv2d_same", "nn.Conv2d(2, 2, 4, padding='same')", [randn(1, 2, 5, 5)]),
        ("conv2d_reflect", "nn.Conv2d(2, 3, 3, padding=1, padding_mode='reflect')", [randn(1, 2, 4, 5)]),
        ("conv3d", "nn.Conv3d(2, 2, (2, 3, 2), stride=(2, 1, 1), padding=1)", [randn(1, 2, 4, 4, 3)]),
        ("convtranspose1d", "nn.ConvTranspose1d(2, 3, 3, stride=2, padding=1, output_padding=1)", [randn(2, 2, 4)]),
        ("convtranspose2d", "nn.ConvTranspose2d(4, 2, 3, stride=2, padding=1, groups=2)", [randn(1, 4, 3, 4)]),
        ("convtranspose2d_op_ge_stride", "nn.ConvTranspose2d(2, 3, (2, 3), stride=(1, 2), output_padding=(1, 0), dilation=(2, 1))", [randn(1, 2, 4, 3)]),
        ("convtranspose3d", "nn.ConvTranspose3d(2, 2, 2, stride=(2, 1, 2))", [randn(1, 2, 2, 3, 2)]),
    ]
    for name, ctor, inputs in specs:
        add("gradpen_x_" + name, ctor, GRAD2 % "x0", inputs)
        add("gradpen_w_" + name, ctor, GRAD2 % "m.weight", inputs)
    add("gradpen_functional", None, "(lambda g: (g[0] ** 2).sum() + (g[1] * x1).sum())(torch.autograd.grad(F.conv2d(torch.tanh(x0), x1, None, 1, 1).pow(2).sum(), (x0, x1), create_graph=True))", [randn(1, 2, 4, 4), randn(3, 2, 3, 3)])
    add("hessian_vector", "nn.Sequential(nn.Conv2d(1, 2, 3), nn.Tanh(), nn.Conv2d(2, 1, 2))", "(lambda g: (g * x1).sum())(torch.autograd.grad(m(x0).sum(), x0, create_graph=True)[0])", [randn(1, 1, 5, 5), nograd(randn(1, 1, 5, 5))])
    add("third_order", "nn.Conv1d(1, 1, 2, bias=False)", "(lambda g1: torch.autograd.grad((g1 ** 2).sum(), x0, create_graph=True)[0].pow(2).sum())(torch.autograd.grad((m(x0) ** 3).sum(), x0, create_graph=True)[0])", [randn(1, 1, 4)])
    return c


def dump(name, cases):
    records = [base.run_case(case, i) for i, case in enumerate(cases)]
    lines = ["# Generated by gen.py from PyTorch %s; do not edit." % torch.__version__, "CASES = ["]
    for rec in records:
        lines.append("    %s," % json.dumps(rec, separators=(",", ":")))
    lines.append("]")
    for i, case in enumerate(cases):
        args = ", ".join(["m"] + ["x%d" % k for k in range(len(case["inputs"]))])
        lines += ["", "", "def ctor_%d():" % i, "    return %s" % (case["ctor"] or "None")]
        lines += ["", "", "def expr_%d(%s):" % (i, args), "    return %s" % case["expr"]]
    lines += ["", "", "BUILDERS = [%s]" % ", ".join("(ctor_%d, expr_%d)" % (i, i) for i in range(len(cases)))]
    with open(os.path.join(HERE, "parity_%s.py" % name), "w", encoding="utf-8", newline="\n") as f:
        f.write("\n".join(lines) + "\n")
    print(name, len(records), "cases")


if __name__ == "__main__":
    import warnings
    warnings.filterwarnings("ignore")
    torch.set_default_dtype(torch.float32)
    os.environ.setdefault("MASTER_ADDR", "127.0.0.1")
    os.environ.setdefault("MASTER_PORT", "29533")
    dump("grid", cases_grid())
    dump("ctc", cases_ctc())
    # SyncBatchNorm refuses CPU input once a process group exists.
    dump("pool", cases_pool())
    dump("dbl", cases_dbl())
    dist.init_process_group("gloo", rank=0, world_size=1)
    dump("parallel", cases_parallel())
    dist.destroy_process_group()
    out = subprocess.run([sys.executable, "-W", "ignore", os.path.join(HERE, "api.py")], capture_output=True, text=True, check=True, env=dict(os.environ, CUDA_VISIBLE_DEVICES="-1", MASTER_PORT="29534")).stdout
    with open(os.path.join(HERE, "api_expected.txt"), "w", encoding="utf-8", newline="\n") as f:
        f.write(out)
    print("api", len(out.splitlines()), "lines")
