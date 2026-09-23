"""Generate the second set of torch.nn parity fixtures with CPU PyTorch (2.11).

    python crates/zipp-vm/tests/fixtures/torch_nn2/gen.py

Same record format and runner as ../torch_nn (gen.py there builds each case
in PyTorch, check.py rebuilds it in Zipp, loads the recorded state_dict and
compares outputs, input and parameter gradients and buffers). Groups:
3-D convolution/pooling/resampling, bicubic and antialiased interpolation,
spectral/weight norm, parametrizations and lazy modules. api.py prints
behaviour (lazy materialization, parametrize API, state_dict hooks, errors)
and runs unchanged on both; its PyTorch output is api_expected.txt.
"""
import json
import os
import subprocess
import sys

import torch

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "torch_nn"))
import gen as base  # noqa: E402  (run_case and the input specs)

randn, const, nograd = base.randn, base.const, base.nograd


def cases_conv3d():
    c = []
    add = lambda name, ctor, expr, inputs, **kw: c.append(dict(name=name, expr=expr, inputs=inputs, ctor=ctor, **kw))
    x5 = randn(2, 4, 5, 6, 5)
    add("conv3d_basic", "nn.Conv3d(4, 3, 3)", "m(x0)", [x5])
    add("conv3d_stride_pad_dil_groups", "nn.Conv3d(4, 6, (3, 2, 3), stride=(2, 1, 2), padding=(1, 2, 0), dilation=(1, 2, 1), groups=2)", "m(x0)", [x5])
    add("conv3d_same_even", "nn.Conv3d(4, 2, (2, 3, 4), padding='same', dilation=(2, 1, 1), bias=False)", "m(x0)", [x5])
    add("conv3d_valid_unbatched", "nn.Conv3d(4, 2, 2, padding='valid')", "m(x0)", [randn(4, 3, 4, 3)])
    add("conv3d_depthwise", "nn.Conv3d(4, 8, 3, padding=1, groups=4)", "m(x0)", [randn(1, 4, 3, 4, 4)])
    add("conv3d_functional", None, "F.conv3d(x0, x1, x2, stride=2, padding=1)", [randn(1, 2, 5, 5, 4), randn(3, 2, 3, 3, 3), randn(3)])
    add("conv3d_float64", None, "F.conv3d(x0.double(), x1.double(), padding=(0, 1, 1))", [randn(1, 2, 3, 4, 4), randn(2, 2, 2, 3, 3)])
    add("convtranspose3d_basic", "nn.ConvTranspose3d(4, 3, 3)", "m(x0)", [randn(2, 4, 3, 4, 3)])
    add("convtranspose3d_stride_pad_op_groups", "nn.ConvTranspose3d(4, 6, (3, 2, 3), stride=(2, 3, 2), padding=(1, 0, 2), output_padding=(1, 2, 0), groups=2)", "m(x0)", [randn(2, 4, 3, 3, 4)])
    add("convtranspose3d_dilation_nobias", "nn.ConvTranspose3d(2, 3, 2, stride=2, padding=1, dilation=(2, 1, 3), bias=False)", "m(x0)", [randn(1, 2, 3, 2, 3)])
    add("convtranspose3d_output_size", "nn.ConvTranspose3d(2, 2, 3, stride=2)", "m(x0, output_size=[7, 8, 7])", [randn(1, 2, 3, 3, 3)])
    add("convtranspose3d_unbatched", "nn.ConvTranspose3d(3, 2, 2, stride=(1, 2, 1))", "m(x0)", [randn(3, 2, 3, 2)])
    add("conv_transpose3d_functional", None, "F.conv_transpose3d(x0, x1, x2, stride=(2, 1, 1), padding=(3, 0, 1), output_padding=(1, 0, 0))", [randn(1, 2, 4, 3, 3), randn(2, 3, 4, 2, 3), randn(3)])
    # output_padding >= stride is legal when the dilation is larger.
    add("convtranspose2d_output_padding_ge_stride", "nn.ConvTranspose2d(2, 3, (2, 3), stride=(1, 2), output_padding=(1, 0), dilation=(2, 1))", "m(x0)", [randn(1, 2, 4, 3)])
    add("convtranspose1d_output_padding_ge_stride", "nn.ConvTranspose1d(2, 2, 2, stride=1, padding=1, output_padding=2, dilation=3)", "m(x0)", [randn(2, 2, 5)])
    add("conv_transpose3d_output_padding_dilation", None, "F.conv_transpose3d(x0, x1, x2, stride=(1, 1, 2), output_padding=(1, 1, 0), dilation=(2, 2, 1))", [randn(1, 1, 4, 4, 3), randn(1, 2, 1, 1, 3), randn(2)])
    add("maxpool3d_default", "nn.MaxPool3d(2)", "m(x0)", [randn(2, 2, 4, 5, 4)])
    add("maxpool3d_pad_dil_ceil_idx", "nn.MaxPool3d((3, 2, 3), stride=(2, 1, 2), padding=(1, 1, 1), dilation=(1, 2, 1), ceil_mode=True, return_indices=True)", "m(x0)", [randn(1, 2, 6, 5, 7)])
    add("maxpool3d_unbatched_idx", None, "F.max_pool3d(x0, 2, stride=1, return_indices=True)", [randn(2, 3, 3, 4)])
    add("avgpool3d_excl_pad", "nn.AvgPool3d(3, 2, padding=1, count_include_pad=False)", "m(x0)", [randn(2, 2, 5, 6, 5)])
    add("avgpool3d_ceil", "nn.AvgPool3d((2, 3, 3), (2, 2, 2), padding=(1, 1, 0), ceil_mode=True)", "m(x0)", [randn(1, 2, 5, 6, 6)])
    add("avgpool3d_divisor", "nn.AvgPool3d(2, divisor_override=3)", "m(x0)", [randn(1, 2, 4, 4, 4)])
    add("adaptive_avg3d_nondiv", "nn.AdaptiveAvgPool3d((2, 3, 4))", "m(x0)", [randn(2, 2, 5, 7, 6)])
    add("adaptive_avg3d_none", "nn.AdaptiveAvgPool3d((None, 2, None))", "m(x0)", [randn(1, 2, 3, 6, 2)])
    add("adaptive_avg3d_div", None, "F.adaptive_avg_pool3d(x0, 2)", [randn(1, 3, 4, 4, 6)])
    add("adaptive_max3d_nondiv_idx", "nn.AdaptiveMaxPool3d((2, 3, 2), return_indices=True)", "m(x0)", [randn(2, 2, 5, 7, 3)])
    add("adaptive_max3d_div", "nn.AdaptiveMaxPool3d(2)", "m(x0)", [randn(1, 2, 4, 4, 4)])
    add("lppool3d", "nn.LPPool3d(2, 2, stride=(1, 2, 2))", "m(x0)", [("pos", [1, 2, 3, 4, 4])])
    add("max_unpool3d", None, "(lambda r: F.max_unpool3d(r[0], r[1], 2))(F.max_pool3d(x0, 2, return_indices=True))", [randn(1, 2, 4, 4, 4)])
    add("max_unpool2d_module", "nn.MaxUnpool2d(2, stride=2)", "(lambda r: m(r[0], r[1], output_size=[5, 5]))(F.max_pool2d(x0, 2, return_indices=True))", [randn(1, 2, 5, 5)])
    add("max_unpool1d", None, "(lambda r: F.max_unpool1d(r[0], r[1], 3, stride=2, padding=1))(F.max_pool1d(x0, 3, stride=2, padding=1, return_indices=True))", [randn(2, 2, 7)])
    add("interp_trilinear_ac", None, "F.interpolate(x0, size=(4, 5, 3), mode='trilinear', align_corners=True)", [randn(1, 2, 2, 3, 3)])
    add("interp_trilinear_scale", None, "F.interpolate(x0, scale_factor=(1.5, 0.5, 2.0), mode='trilinear')", [randn(1, 1, 4, 6, 2)])
    add("interp_nearest_3d_size", None, "F.interpolate(x0, size=(3, 5, 2))", [randn(1, 2, 2, 3, 4)])
    add("interp_nearest_exact_3d", None, "F.interpolate(x0, size=(5, 2, 3), mode='nearest-exact')", [randn(1, 1, 3, 4, 2)])
    add("interp_area_3d", None, "F.interpolate(x0, size=(2, 3, 2), mode='area')", [randn(1, 2, 5, 6, 3)])
    add("upsample_trilinear_module", "nn.Upsample(scale_factor=2, mode='trilinear')", "m(x0)", [randn(1, 2, 2, 2, 3)])
    add("batchnorm3d_train", "nn.BatchNorm3d(3)", "(m(x0), m(x0 * 2 + 1))", [randn(2, 3, 3, 2, 4)])
    add("batchnorm3d_eval", "nn.BatchNorm3d(3)", "m(x0)", [randn(2, 3, 3, 2, 4)], train=False)
    add("instancenorm3d_affine_stats", "nn.InstanceNorm3d(3, affine=True, track_running_stats=True)", "m(x0)", [randn(2, 3, 3, 2, 4)])
    add("instancenorm3d_unbatched", "nn.InstanceNorm3d(2)", "m(x0)", [randn(2, 3, 4, 2)])
    add("dropout3d_eval", "nn.Dropout3d(0.5)", "m(x0)", [randn(2, 3, 2, 2, 2)], train=False)
    add("reflectionpad3d", "nn.ReflectionPad3d((1, 2, 0, 1, 2, 1))", "m(x0)", [randn(1, 2, 3, 3, 4)])
    add("replicationpad3d", "nn.ReplicationPad3d(2)", "m(x0)", [randn(1, 1, 2, 3, 2)])
    add("circularpad3d", "nn.CircularPad3d((1, 0, 2, 1, 1, 1))", "m(x0)", [randn(1, 2, 2, 3, 3)])
    add("constantpad3d_zeropad3d", "nn.Sequential(nn.ConstantPad3d((1, 0, 0, 1, 1, 0), 2.5), nn.ZeroPad3d(1))", "m(x0)", [randn(1, 1, 2, 2, 2)])
    add("lazyconv3d_dry_run", "(lambda m: (m(torch.zeros(1, 3, 4, 4, 4)), m)[1])(nn.LazyConv3d(2, 3, padding=1))", "m(x0)", [randn(1, 3, 4, 4, 4)])
    add("lazyconvtranspose3d_dry_run", "(lambda m: (m(torch.zeros(3, 2, 2, 2)), m)[1])(nn.LazyConvTranspose3d(2, 2, stride=2))", "m(x0)", [randn(3, 2, 2, 2)])
    return c


def cases_resample():
    c = []
    add = lambda name, ctor, expr, inputs, **kw: c.append(dict(name=name, expr=expr, inputs=inputs, ctor=ctor, **kw))
    img = randn(1, 2, 5, 6)
    for ac in (False, True):
        tag = "_ac" if ac else ""
        add("bicubic_up_size" + tag, None, "F.interpolate(x0, size=(9, 11), mode='bicubic', align_corners=%s)" % ac, [img])
        add("bicubic_down_size" + tag, None, "F.interpolate(x0, size=(3, 4), mode='bicubic', align_corners=%s)" % ac, [randn(2, 1, 7, 9)])
        add("bicubic_scale" + tag, None, "F.interpolate(x0, scale_factor=(1.7, 0.6), mode='bicubic', align_corners=%s)" % ac, [img])
        add("aa_bilinear_down" + tag, None, "F.interpolate(x0, size=(3, 2), mode='bilinear', antialias=True, align_corners=%s)" % ac, [randn(1, 2, 9, 7)])
        add("aa_bicubic_down" + tag, None, "F.interpolate(x0, size=(4, 3), mode='bicubic', antialias=True, align_corners=%s)" % ac, [randn(1, 2, 11, 8)])
        add("aa_bilinear_up_mixed" + tag, None, "F.interpolate(x0, size=(8, 3), mode='bilinear', antialias=True, align_corners=%s)" % ac, [randn(1, 1, 5, 7)])
    add("bicubic_scale_recompute", None, "F.interpolate(x0, scale_factor=1.3, mode='bicubic', recompute_scale_factor=True)", [img])
    add("bicubic_scale_int", None, "F.interpolate(x0, scale_factor=2, mode='bicubic')", [randn(1, 1, 3, 4)])
    add("bicubic_to_one", None, "F.interpolate(x0, size=(1, 4), mode='bicubic')", [randn(1, 1, 4, 5)])
    add("bicubic_float64", None, "F.interpolate(x0.double(), size=(7, 3), mode='bicubic')", [randn(1, 1, 4, 5)])
    add("aa_bilinear_scale", None, "F.interpolate(x0, scale_factor=0.4, mode='bilinear', antialias=True)", [randn(1, 2, 10, 12)])
    add("aa_bicubic_scale_recompute", None, "F.interpolate(x0, scale_factor=(0.3, 0.45), mode='bicubic', antialias=True, recompute_scale_factor=True)", [randn(1, 1, 13, 11)])
    add("aa_bicubic_scale_given", None, "F.interpolate(x0, scale_factor=(0.35, 0.5), mode='bicubic', antialias=True, recompute_scale_factor=False)", [randn(1, 1, 13, 11)])
    add("aa_bicubic_up", None, "F.interpolate(x0, size=(7, 9), mode='bicubic', antialias=True)", [randn(1, 2, 4, 5)])
    add("aa_same_height", None, "F.interpolate(x0, size=(6, 3), mode='bilinear', antialias=True)", [randn(1, 1, 6, 8)])
    add("aa_same_size_given_scale", None, "(F.interpolate(x0, scale_factor=(1.3, 0.5), mode='bicubic', antialias=True, recompute_scale_factor=False), F.interpolate(x1, scale_factor=(0.5, 1.2), mode='bilinear', antialias=True))", [randn(1, 1, 3, 6), randn(1, 2, 6, 4)])
    add("aa_width_one", None, "F.interpolate(x0, size=(4, 1), mode='bilinear', antialias=True)", [randn(1, 2, 7, 5)])
    add("aa_bicubic_width_one", None, "F.interpolate(x0, size=(3, 1), mode='bicubic', antialias=True)", [randn(2, 1, 8, 4)])
    add("upsample_bicubic_module", "nn.Upsample(size=(6, 5), mode='bicubic', align_corners=True)", "m(x0)", [randn(1, 2, 3, 3)])
    add("upsample_bicubic_scale_module", "nn.Upsample(scale_factor=1.5, mode='bicubic')", "m(x0)", [randn(1, 1, 4, 4)])
    return c


def cases_param():
    c = []
    add = lambda name, ctor, expr, inputs, **kw: c.append(dict(name=name, expr=expr, inputs=inputs, ctor=ctor, **kw))
    SN = "torch.nn.utils.spectral_norm"
    P = "torch.nn.utils.parametrizations"
    add("spectral_norm_legacy_linear", "%s(nn.Linear(4, 3))" % SN, "m(x0)", [randn(5, 4)])
    add("spectral_norm_legacy_twice", "%s(nn.Linear(4, 3), n_power_iterations=3)" % SN, "(m(x0), m(x0 * 2))", [randn(2, 4)])
    add("spectral_norm_legacy_eval", "%s(nn.Linear(4, 3))" % SN, "m(x0)", [randn(5, 4)], train=False)
    add("spectral_norm_legacy_conv2d", "%s(nn.Conv2d(2, 3, 3), eps=1e-4)" % SN, "m(x0)", [randn(1, 2, 5, 5)])
    add("spectral_norm_legacy_convT_dim1", "%s(nn.ConvTranspose2d(2, 3, 2))" % SN, "m(x0)", [randn(1, 2, 3, 3)])
    add("spectral_norm_legacy_remove", "%s(nn.Linear(3, 3))" % SN, "(m(x0), torch.nn.utils.remove_spectral_norm(m).weight * 1)", [randn(2, 3)])
    add("spectral_norm_param_linear", "%s.spectral_norm(nn.Linear(4, 3))" % P, "m(x0)", [randn(5, 4)])
    add("spectral_norm_param_twice", "%s.spectral_norm(nn.Linear(4, 3), n_power_iterations=2)" % P, "(m(x0), m(x0 + 1))", [randn(2, 4)])
    add("spectral_norm_param_eval", "%s.spectral_norm(nn.Linear(4, 3))" % P, "m(x0)", [randn(5, 4)], train=False)
    add("spectral_norm_param_conv", "%s.spectral_norm(nn.Conv1d(3, 2, 3), dim=1)" % P, "m(x0)", [randn(2, 3, 6)])
    add("spectral_norm_param_convT", "%s.spectral_norm(nn.ConvTranspose1d(2, 3, 3))" % P, "m(x0)", [randn(2, 2, 4)])
    add("spectral_norm_param_remove", "%s.spectral_norm(nn.Linear(3, 4))" % P, "torch.nn.utils.parametrize.remove_parametrizations(m, 'weight')(x0)", [randn(2, 3)])
    add("weight_norm_param", "%s.weight_norm(nn.Linear(4, 3))" % P, "m(x0)", [randn(5, 4)])
    add("weight_norm_param_dim_none", "%s.weight_norm(nn.Linear(4, 3), dim=None)" % P, "m(x0)", [randn(5, 4)])
    add("weight_norm_param_conv_dim1", "%s.weight_norm(nn.Conv2d(2, 3, 2), dim=1)" % P, "m(x0)", [randn(1, 2, 3, 3)])
    add("orthogonal_square", "%s.orthogonal(nn.Linear(4, 4))" % P, "m(x0)", [randn(3, 4)])
    add("orthogonal_tall", "%s.orthogonal(nn.Linear(3, 5))" % P, "m(x0)", [randn(2, 3)])
    add("orthogonal_wide", "%s.orthogonal(nn.Linear(5, 3))" % P, "m(x0)", [randn(2, 5)])
    add("orthogonal_cayley", "%s.orthogonal(nn.Linear(4, 4), orthogonal_map='cayley')" % P, "m(x0)", [randn(2, 4)])
    add("orthogonal_householder_square", "%s.orthogonal(nn.Linear(3, 3), orthogonal_map='householder')" % P, "m(x0)", [randn(2, 3)])
    add("orthogonal_matrix_exp_tall", "%s.orthogonal(nn.Linear(2, 4), orthogonal_map='matrix_exp')" % P, "m(x0)", [randn(3, 2)])
    add("orthogonal_no_trivialization", "%s.orthogonal(nn.Linear(3, 4), use_trivialization=False)" % P, "m(x0)", [randn(2, 3)])
    add("parametrizations_stacked", "%s.spectral_norm(%s.weight_norm(nn.Linear(4, 3)))" % (P, P), "m(x0)", [randn(2, 4)])
    add("lazylinear_dry_run", "(lambda m: (m(torch.zeros(2, 5)), m)[1])(nn.LazyLinear(3))", "m(x0)", [randn(4, 5)])
    add("lazy_sequential", "(lambda m: (m(torch.zeros(1, 6)), m)[1])(nn.Sequential(nn.LazyLinear(4), nn.ReLU(), nn.LazyLinear(2, bias=False)))", "m(x0)", [randn(3, 6)])
    add("lazyconv1d_unbatched", "(lambda m: (m(torch.zeros(3, 7)), m)[1])(nn.LazyConv1d(4, 3, stride=2))", "m(x0)", [randn(3, 7)])
    add("lazyconv2d_groups", "(lambda m: (m(torch.zeros(1, 4, 5, 5)), m)[1])(nn.LazyConv2d(6, 3, groups=2, padding=1))", "m(x0)", [randn(2, 4, 5, 5)])
    add("lazyconvtranspose1d", "(lambda m: (m(torch.zeros(1, 2, 4)), m)[1])(nn.LazyConvTranspose1d(3, 2, stride=2))", "m(x0)", [randn(1, 2, 4)])
    add("lazyconvtranspose2d", "(lambda m: (m(torch.zeros(1, 2, 3, 3)), m)[1])(nn.LazyConvTranspose2d(3, 2, stride=2, output_padding=1))", "m(x0)", [randn(2, 2, 3, 3)])
    add("lazybatchnorm1d", "(lambda m: (m(torch.zeros(4, 3)), m)[1])(nn.LazyBatchNorm1d())", "m(x0)", [randn(5, 3)])
    add("lazybatchnorm2d", "(lambda m: (m(torch.ones(2, 3, 2, 2)), m)[1])(nn.LazyBatchNorm2d(momentum=0.3))", "m(x0)", [randn(2, 3, 2, 2)])
    add("lazybatchnorm3d", "(lambda m: (m(torch.ones(2, 2, 2, 2, 2)), m)[1])(nn.LazyBatchNorm3d())", "m(x0)", [randn(2, 2, 2, 2, 2)])
    add("lazyinstancenorm2d_affine", "(lambda m: (m(torch.zeros(1, 3, 2, 2)), m)[1])(nn.LazyInstanceNorm2d(affine=True, track_running_stats=True))", "m(x0)", [randn(2, 3, 3, 2)])
    add("lazyinstancenorm1d_default", "(lambda m: (m(torch.zeros(2, 3, 4)), m)[1])(nn.LazyInstanceNorm1d())", "m(x0)", [randn(2, 3, 4)])
    add("lazyinstancenorm3d_affine", "(lambda m: (m(torch.zeros(1, 2, 2, 2, 2)), m)[1])(nn.LazyInstanceNorm3d(affine=True, track_running_stats=False))", "m(x0)", [randn(2, 2, 3, 2, 2)])
    add("fuse_conv_bn_eval", "(lambda m: (m(torch.arange(150.0).reshape(2, 3, 5, 5) / 20 - 3), m)[1])(nn.Sequential(nn.Conv2d(3, 4, 3), nn.BatchNorm2d(4)))", "(torch.nn.utils.fuse_conv_bn_eval(m[0], m[1])(x0), m(x0))", [randn(1, 3, 5, 5)], train=False)
    add("fuse_convtranspose_bn_eval", "(lambda m: (m(torch.arange(24.0).reshape(1, 2, 3, 4) / 10 - 1), m)[1])(nn.Sequential(nn.ConvTranspose2d(2, 3, 2, stride=2), nn.BatchNorm2d(3)))", "(torch.nn.utils.fuse_conv_bn_eval(m[0], m[1], transpose=True)(x0), m(x0))", [randn(1, 2, 3, 3)], train=False)
    add("fuse_linear_bn_eval", "(lambda m: (m(torch.arange(20.0).reshape(5, 4) / 7 - 1), m)[1])(nn.Sequential(nn.Linear(4, 3), nn.BatchNorm1d(3)))", "(torch.nn.utils.fuse_linear_bn_eval(m[0], m[1])(x0), m(x0))", [randn(2, 4)], train=False)
    add("multilabel_margin", "nn.MultiLabelMarginLoss()", "m(x0, x1)", [randn(3, 4), const([[3, 0, -1, 1], [1, 2, 3, -1], [-1, 2, 0, 0]], "int64")])
    add("multilabel_margin_none_1d", None, "(F.multilabel_margin_loss(x0, x1, reduction='none'), F.multilabel_margin_loss(x2, x3, reduction='sum'))", [randn(2, 5), const([[4, 4, 1, -1, 0], [0, 1, 2, 3, 4]], "int64"), randn(4), const([2, -1, 0, 1], "int64")])
    add("inplace_aliases", None, "(F.elu_(x0.clone(), 0.5), F.leaky_relu_(x0.clone(), 0.2), F.hardtanh_(x0.clone(), -0.5, 0.5), F.threshold_(x0.clone(), 0.1, 2.0), F.selu_(x0.clone()), F.celu_(x0.clone(), 1.5))", [randn(2, 5)])
    add("normalize_out", None, "F.normalize(x0, dim=0, out=torch.empty(3, 2))", [nograd(randn(3, 2))])
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
    dump("conv3d", cases_conv3d())
    dump("resample", cases_resample())
    dump("param", cases_param())
    out = subprocess.run([sys.executable, "-W", "ignore", os.path.join(HERE, "api.py")], capture_output=True, text=True, check=True).stdout
    with open(os.path.join(HERE, "api_expected.txt"), "w", encoding="utf-8", newline="\n") as f:
        f.write(out)
    print("api", len(out.splitlines()), "lines")
