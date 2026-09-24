"""torch.ao.nn.quantized.functional for Zipp: functional forms of the
quantized operations on quantized tensors."""
import torch
import torch._quant as _q
import torch.nn.functional as F

__all__ = ["linear", "conv1d", "conv2d", "relu6", "max_pool1d", "max_pool2d", "adaptive_avg_pool2d", "avg_pool2d", "interpolate",
           "upsample", "upsample_nearest", "hardtanh", "clamp", "threshold"]


def _pair(v, n=2):
    return tuple(v) if isinstance(v, (tuple, list)) else (v,) * n


def linear(input, weight, bias=None, scale=None, zero_point=None):
    if scale is None:
        scale = input.q_scale()
    if zero_point is None:
        zero_point = input.q_zero_point()
    return _q.linear(input, weight, bias, scale, zero_point)


def conv2d(input, weight, bias, stride=1, padding=0, dilation=1, groups=1, padding_mode="zeros", scale=1.0, zero_point=0, dtype=torch.quint8):
    if padding_mode != "zeros":
        raise NotImplementedError("Only zero-padding is supported!")
    if input.dtype != torch.quint8:
        raise NotImplementedError("Only torch.quint8 is supported for activation tensor!")
    if weight.dtype != torch.qint8:
        raise NotImplementedError("Only torch.qint8 is supported for weight tensor!")
    if input.ndim != 4:
        raise ValueError("Input shape must be `(N, C, H, W)`!")
    return _q.conv2d(input, weight, bias, _pair(stride), _pair(padding), _pair(dilation), groups, scale, zero_point)


def conv1d(input, weight, bias, stride=1, padding=0, dilation=1, groups=1, padding_mode="zeros", scale=1.0, zero_point=0, dtype=torch.quint8):
    if padding_mode != "zeros":
        raise NotImplementedError("Only zero-padding is supported!")
    if input.ndim != 3:
        raise ValueError("Input shape must be `(N, C, L)`!")
    return _q.conv1d(input, weight, bias, _pair(stride, 1), _pair(padding, 1), _pair(dilation, 1), groups, scale, zero_point)


def relu6(input, inplace=False):
    return _q._hardtanh(input, 0.0, 6.0, inplace)


def hardtanh(input, min_val=-1.0, max_val=1.0, inplace=False):
    return _q._hardtanh(input, min_val, max_val, inplace)


def clamp(input, min_, max_):
    return _q._hardtanh(input, min_, max_)


def threshold(input, threshold, value):
    raise NotImplementedError("quantized threshold is not supported on Zipp")


def max_pool1d(input, kernel_size, stride=None, padding=0, dilation=1, ceil_mode=False, return_indices=False):
    return F.max_pool1d(input, kernel_size, stride, padding, dilation, ceil_mode, return_indices)


def max_pool2d(input, kernel_size, stride=None, padding=0, dilation=1, ceil_mode=False, return_indices=False):
    return F.max_pool2d(input, kernel_size, stride, padding, dilation, ceil_mode, return_indices)


def adaptive_avg_pool2d(input, output_size):
    return F.adaptive_avg_pool2d(input, output_size)


def avg_pool2d(input, kernel_size, stride=None, padding=0, ceil_mode=False, count_include_pad=True, divisor_override=None):
    return F.avg_pool2d(input, kernel_size, stride, padding, ceil_mode, count_include_pad, divisor_override)


def interpolate(input, size=None, scale_factor=None, mode="nearest", align_corners=None):
    return F.interpolate(input, size, scale_factor, mode, align_corners)


def upsample(input, size=None, scale_factor=None, mode="nearest", align_corners=None):
    return F.interpolate(input, size, scale_factor, mode, align_corners)


def upsample_nearest(input, size=None, scale_factor=None):
    return F.interpolate(input, size, scale_factor, "nearest")
