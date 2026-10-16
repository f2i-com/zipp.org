"""torch.ao.quantization for Zipp: PyTorch 2.11's eager-mode quantization
workflow. Observers (MinMax, MovingAverageMinMax, PerChannelMinMax,
MovingAveragePerChannelMinMax, Histogram, FixedQParams, Placeholder,
Recording, Noop) compute scale and zero point with PyTorch's float32
tensor arithmetic; FakeQuantize / FusedMovingAvgObsFakeQuantize drive
quantization-aware training; QConfig and the default qconfigs name them;
prepare / convert / quantize / prepare_qat / quantize_qat / quantize_dynamic
/ fuse_modules rewrite a model as PyTorch does, swapping in
torch.ao.nn.quantized (and .dynamic, .qat, .intrinsic) modules.

FX graph mode (quantize_fx) and the PT2E flow are not available: Zipp has
no graph tracing.
"""
import copy
import itertools
import math
import sys
from collections import OrderedDict, namedtuple
from functools import partial

import torch
import torch.nn as nn
import torch._quant as _q
import torch.ao.nn.quantized as nnq
import torch.ao.nn.quantized.dynamic as nnqd
import torch.ao.nn.intrinsic as nni
import torch.ao.nn.intrinsic.quantized as nniq
import torch.ao.nn.intrinsic.qat as nniqat
import torch.ao.nn.qat as nnqat
from torch.nn.utils.parametrize import type_before_parametrizations, is_parametrized

__all__ = [
    "DeQuantStub", "FakeQuantize", "FakeQuantizeBase", "FixedQParamsFakeQuantize", "FixedQParamsObserver",
    "FusedMovingAvgObsFakeQuantize", "HistogramObserver", "MinMaxObserver", "MovingAverageMinMaxObserver",
    "MovingAveragePerChannelMinMaxObserver", "NoopObserver", "ObserverBase", "PerChannelMinMaxObserver", "PlaceholderObserver",
    "QConfig", "QConfigAny", "QConfigDynamic", "QuantStub", "QuantType", "QuantWrapper", "RecordingObserver", "ReuseInputObserver",
    "UniformQuantizationObserverBase", "add_quant_dequant", "convert", "default_activation_only_qconfig", "default_affine_fixed_qparams_fake_quant",
    "default_affine_fixed_qparams_observer", "default_debug_observer", "default_debug_qconfig", "default_dynamic_fake_quant",
    "default_dynamic_qat_qconfig", "default_dynamic_qconfig", "default_dynamic_quant_observer", "default_embedding_qat_qconfig",
    "default_eval_fn", "default_fake_quant", "default_fixed_qparams_range_0to1_fake_quant", "default_fixed_qparams_range_0to1_observer",
    "default_fixed_qparams_range_neg1to1_fake_quant", "default_fixed_qparams_range_neg1to1_observer", "default_float_qparams_observer",
    "default_fused_act_fake_quant", "default_fused_per_channel_wt_fake_quant", "default_fused_wt_fake_quant", "default_histogram_fake_quant",
    "default_histogram_observer", "default_observer", "default_per_channel_qconfig", "default_per_channel_weight_fake_quant",
    "default_per_channel_weight_observer", "default_placeholder_observer", "default_qat_qconfig", "default_qat_qconfig_v2", "default_qconfig",
    "default_reuse_input_observer", "default_reuse_input_qconfig", "default_symmetric_fixed_qparams_fake_quant",
    "default_symmetric_fixed_qparams_observer", "default_weight_fake_quant", "default_weight_observer", "default_weight_only_qconfig",
    "disable_fake_quant", "disable_observer", "enable_fake_quant", "enable_observer", "float16_dynamic_qconfig", "float16_static_qconfig",
    "float_qparams_weight_only_qconfig", "fuse_conv_bn", "fuse_conv_bn_relu", "fuse_linear_bn", "fuse_modules", "fuse_modules_qat",
    "fused_per_channel_wt_fake_quant_range_neg_127_to_127", "fused_wt_fake_quant_range_neg_127_to_127", "get_default_compare_output_module_list",
    "get_default_dynamic_quant_module_mappings", "get_default_qat_module_mappings", "get_default_qat_qconfig", "get_default_qconfig",
    "get_default_qconfig_propagation_list", "get_default_static_quant_module_mappings", "get_dynamic_quant_module_class",
    "get_fuser_method", "get_observer_state_dict", "get_quantized_operator", "get_static_quant_module_class", "load_observer_state_dict",
    "no_observer_set", "per_channel_dynamic_qconfig", "per_channel_weight_observer_range_neg_127_to_127", "prepare", "prepare_qat",
    "propagate_qconfig_", "qconfig_equals", "quantize", "quantize_dynamic", "quantize_qat", "swap_module", "weight_observer_range_neg_127_to_127",
    "default_symmetric_qnnpack_qconfig", "default_per_channel_symmetric_qnnpack_qconfig", "default_symmetric_qnnpack_qat_qconfig",
    "default_per_channel_symmetric_qnnpack_qat_qconfig", "default_quint8_weight_qconfig",
]

_EPS = torch.finfo(torch.float32).eps
_warned = set()


def _warn(message, category="UserWarning", stacklevel=1):
    """A Python warning as the default filter shows it (once per message;
    this runtime has no `warnings` module): on stderr."""
    if message in _warned:
        return
    _warned.add(message)
    sys.stderr.write("%s: %s\n" % (category, message))


def _factory_kwargs(kwargs):
    if kwargs is None:
        return {}
    return {k: v for k, v in kwargs.items() if k in ("device", "dtype") and v is not None}


# ---- utils (torch/ao/quantization/utils.py) -------------------------------------------------
def get_combined_dict(default_dict, additional_dict):
    d = dict(default_dict)
    d.update(additional_dict)
    return d


def is_per_tensor(qscheme):
    return qscheme == torch.per_tensor_affine or qscheme == torch.per_tensor_symmetric


def is_per_channel(qscheme):
    return qscheme in (torch.per_channel_affine, torch.per_channel_affine_float_qparams, torch.per_channel_symmetric)


def check_min_max_valid(min_val, max_val):
    if min_val.numel() == 0 or max_val.numel() == 0:
        _warn("must run observer before calling calculate_qparams. Returning default values.", stacklevel=2)
        return False
    if min_val.dim() == 0 or max_val.dim() == 0:
        if min_val == float("inf") and max_val == float("-inf"):
            _warn("must run observer before calling calculate_qparams. Returning default values.", stacklevel=2)
            return False
        if min_val > max_val:
            raise AssertionError("min %s should be less than max %s" % (min_val, max_val))
    elif torch.any(min_val > max_val):
        raise AssertionError("min %s should be less than max %s" % (min_val, max_val))
    return True


def validate_qmin_qmax(quant_min, quant_max):
    if not quant_min <= 0 <= quant_max:
        raise AssertionError("Used-specified quantization range must include 0.")
    if quant_min >= quant_max:
        raise AssertionError("qmin must be strictly less than qmax for user-specified quantization range.")


def calculate_qmin_qmax(quant_min, quant_max, has_customized_qrange, dtype, reduce_range):
    if has_customized_qrange:
        if dtype in (torch.qint32, torch.int32):
            initial_quant_min, initial_quant_max = 0, 2 ** 32 - 1
        else:
            initial_quant_min, initial_quant_max = 0, 255
        if quant_min is not None and quant_max is not None:
            initial_quant_min, initial_quant_max = quant_min, quant_max
        qrange_len = initial_quant_max - initial_quant_min + 1
        if dtype in (torch.qint8, torch.int8):
            if not 0 < qrange_len <= 256:
                raise AssertionError("quantization range should be positive and not exceed the maximum bit range (=256).")
        elif dtype in (torch.qint32, torch.int32):
            if not 0 < qrange_len <= 2 ** 32:
                raise AssertionError("quantization range should be positive and not exceed the maximum bit range (=4294967296).")
        if reduce_range:
            quant_min, quant_max = quant_min // 2, quant_max // 2
    elif dtype in (torch.qint8, torch.int8):
        quant_min, quant_max = (-64, 63) if reduce_range else (-128, 127)
    elif dtype in (torch.quint8, torch.uint8):
        quant_min, quant_max = (0, 127) if reduce_range else (0, 255)
    elif dtype in (torch.qint32, torch.int32):
        quant_min, quant_max = -1 * 2 ** 31, 2 ** 31 - 1
    elif dtype == torch.int16:
        quant_min, quant_max = -2 ** 15, 2 ** 15 - 1
    else:
        quant_min, quant_max = 0, 15
    return quant_min, quant_max


def has_no_children_ignoring_parametrizations(module):
    if len(module._modules) == 0:
        return True
    if is_parametrized(module):
        return len(module._modules) == 1 and "parametrizations" in module._modules
    return False


def get_qparam_dict(observer_or_fake_quant):
    qscheme = getattr(observer_or_fake_quant, "qscheme", None)
    dtype = observer_or_fake_quant.dtype
    qparams = {"qscheme": qscheme, "dtype": dtype}
    if not qscheme or isinstance(observer_or_fake_quant, PlaceholderObserver):
        return {"qscheme": None, "dtype": dtype}
    if is_per_tensor(qscheme):
        qscheme = torch.per_tensor_affine
    elif is_per_channel(qscheme):
        if qscheme == torch.per_channel_symmetric:
            qscheme = torch.per_channel_affine
        qparams["axis"] = observer_or_fake_quant.ch_axis
    qparams["qscheme"] = qscheme
    scale, zero_point = observer_or_fake_quant.calculate_qparams()
    qparams["scale"] = scale
    qparams["zero_point"] = zero_point
    if hasattr(observer_or_fake_quant, "quant_min"):
        qparams["quant_min"] = observer_or_fake_quant.quant_min
    if hasattr(observer_or_fake_quant, "quant_max"):
        qparams["quant_max"] = observer_or_fake_quant.quant_max
    return qparams


def activation_dtype(qconfig):
    return qconfig.activation().dtype


def weight_dtype(qconfig):
    return qconfig.weight().dtype


def activation_is_statically_quantized(qconfig):
    return activation_dtype(qconfig) in (torch.quint8, torch.qint8, torch.qint32, torch.float16, torch.uint8, torch.int8, torch.int16, torch.int32) and not getattr(qconfig.activation(), "is_dynamic", False)


class QuantType:
    DYNAMIC = 0
    STATIC = 1
    QAT = 2
    WEIGHT_ONLY = 3


# ---- observers (torch/ao/quantization/observer.py) ------------------------------------------
class _PartialWrapper:
    def __init__(self, p):
        self.p = p
        self.callable_args = {}

    def __call__(self, *args, **keywords):
        for arg_name in self.callable_args:
            if arg_name not in keywords:
                keywords = dict(keywords)
                keywords[arg_name] = self.callable_args[arg_name]()
        return self.p(*args, **keywords)

    def __repr__(self):
        return _partial_repr(self.p) + repr(self.callable_args)

    def with_args(self, **kwargs):
        return _with_args(self, **kwargs)

    def with_callable_args(self, **kwargs):
        result = _PartialWrapper(p=self.p)
        result.callable_args = dict(self.callable_args)
        result.callable_args.update(kwargs)
        return result


def _partial_repr(p):
    parts = [repr(p.func)]
    parts.extend(repr(a) for a in p.args)
    parts.extend("%s=%r" % (k, v) for k, v in p.keywords.items())
    return "functools.partial(" + ", ".join(parts) + ")"


def _with_args(cls_or_self, **kwargs):
    return _PartialWrapper(partial(cls_or_self, **kwargs))


def _with_callable_args(cls_or_self, **kwargs):
    r = _PartialWrapper(partial(cls_or_self))
    return r.with_callable_args(**kwargs)


class ObserverBase(nn.Module):
    __module__ = "torch.ao.quantization.observer"
    def __init__(self, dtype, is_dynamic=False):
        super().__init__()
        self.dtype = dtype
        self.is_dynamic = is_dynamic

    def forward(self, x):
        raise NotImplementedError

    def calculate_qparams(self, **kwargs):
        raise NotImplementedError

    with_args = classmethod(_with_args)
    with_callable_args = classmethod(_with_callable_args)


class UniformQuantizationObserverBase(ObserverBase):
    __module__ = "torch.ao.quantization.observer"
    _version = 3

    def __init__(self, dtype=torch.quint8, qscheme=torch.per_tensor_affine, reduce_range=False, quant_min=None, quant_max=None, factory_kwargs=None, eps=_EPS, is_dynamic=False, **kwargs):
        factory_kwargs = _factory_kwargs(factory_kwargs)
        super().__init__(dtype=dtype, is_dynamic=is_dynamic)
        self.qscheme = qscheme
        if reduce_range:
            _warn("Please use quant_min and quant_max to specify the range for observers.                     reduce_range will be deprecated in a future release of PyTorch.", stacklevel=2)
        self.reduce_range = reduce_range
        self.register_buffer("eps", torch.tensor([eps], **factory_kwargs))
        if self.qscheme not in (torch.per_tensor_affine, torch.per_tensor_symmetric, torch.per_channel_affine, torch.per_channel_symmetric, torch.per_channel_affine_float_qparams):
            raise AssertionError("Default Observer only works for per_tensor_affine, per_tensor_symmetric, per_channel_affine, per_channel_symmetric and per_channel_float_qparams quantization scheme")
        allowed = (torch.qint8, torch.quint8, torch.quint4x2, torch.qint32, torch.int8, torch.uint8, torch.int16, torch.int32)
        if self.dtype not in allowed:
            raise AssertionError("Default Observer only works for %s data type" % (allowed,))
        self.has_customized_qrange = quant_min is not None and quant_max is not None
        if self.has_customized_qrange:
            validate_qmin_qmax(quant_min, quant_max)
        self.quant_min, self.quant_max = calculate_qmin_qmax(quant_min, quant_max, self.has_customized_qrange, self.dtype, self.reduce_range)

    def _validate_qmin_qmax(self, quant_min, quant_max):
        validate_qmin_qmax(quant_min, quant_max)

    def _calculate_qparams(self, min_val, max_val):
        if not check_min_max_valid(min_val, max_val):
            return torch.tensor([1.0]), torch.tensor([0])
        quant_min, quant_max = self.quant_min, self.quant_max
        min_val_neg = torch.min(min_val, torch.zeros_like(min_val))
        max_val_pos = torch.max(max_val, torch.zeros_like(max_val))
        scale = torch.ones(min_val_neg.size(), dtype=torch.float32)
        zero_point = torch.zeros(min_val_neg.size(), dtype=torch.int64)
        if self.qscheme == torch.per_tensor_symmetric or self.qscheme == torch.per_channel_symmetric:
            max_val_pos = torch.max(-min_val_neg, max_val_pos)
            scale = max_val_pos / (float(quant_max - quant_min) / 2)
            scale = torch.max(scale, self.eps)
            if self.dtype in (torch.quint8, torch.uint8):
                if self.has_customized_qrange:
                    zero_point = zero_point.new_full(zero_point.size(), (quant_min + quant_max) // 2)
                else:
                    zero_point = zero_point.new_full(zero_point.size(), 128)
        elif self.qscheme == torch.per_channel_affine_float_qparams:
            scale = (max_val - min_val) / float(quant_max - quant_min)
            scale = torch.where(scale > self.eps, scale, torch.ones_like(scale))
            zero_point = -1 * min_val / scale
        else:
            scale = (max_val_pos - min_val_neg) / float(quant_max - quant_min)
            scale = torch.max(scale, self.eps)
            zero_point = quant_min - torch.round(min_val_neg / scale).to(torch.int)
            zero_point = torch.clamp(zero_point, quant_min, quant_max)
        if len(scale.shape) == 0:
            scale = torch.tensor([float(scale)], dtype=scale.dtype)
        if len(zero_point.shape) == 0:
            zero_point = torch.tensor([int(zero_point)], dtype=zero_point.dtype)
            if self.qscheme == torch.per_channel_affine_float_qparams:
                zero_point = torch.tensor([float(zero_point)], dtype=zero_point.dtype)
        return scale, zero_point

    def reset_min_max_vals(self):
        raise NotImplementedError("Cannot reset min/max values in the given observer.")


_ObserverBase = UniformQuantizationObserverBase


class MinMaxObserver(UniformQuantizationObserverBase):
    __module__ = "torch.ao.quantization.observer"
    def __init__(self, dtype=torch.quint8, qscheme=torch.per_tensor_affine, reduce_range=False, quant_min=None, quant_max=None, factory_kwargs=None, eps=_EPS, is_dynamic=False, **kwargs):
        if not is_per_tensor(qscheme):
            raise NotImplementedError("MinMaxObserver's qscheme only support torch.per_tensor_symmetric                     and torch.per_tensor_affine.")
        super().__init__(dtype=dtype, qscheme=qscheme, reduce_range=reduce_range, quant_min=quant_min, quant_max=quant_max, factory_kwargs=factory_kwargs, eps=eps, is_dynamic=is_dynamic, **kwargs)
        fk = _factory_kwargs(factory_kwargs)
        self.register_buffer("min_val", torch.tensor(float("inf"), **fk))
        self.register_buffer("max_val", torch.tensor(float("-inf"), **fk))
        if self.qscheme == torch.per_tensor_symmetric and self.reduce_range and self.dtype == torch.quint8:
            raise NotImplementedError("Cannot reduce range for symmetric                                        quantization for quint8")

    def forward(self, x_orig):
        if x_orig.numel() == 0:
            return x_orig
        x = x_orig.detach()
        x = x.to(self.min_val.dtype)
        min_val_cur, max_val_cur = torch.aminmax(x)
        min_val = torch.min(min_val_cur, self.min_val)
        max_val = torch.max(max_val_cur, self.max_val)
        self.min_val.copy_(min_val)
        self.max_val.copy_(max_val)
        return x_orig

    def calculate_qparams(self):
        return self._calculate_qparams(self.min_val, self.max_val)

    def extra_repr(self):
        return "min_val={}, max_val={}".format(self.min_val, self.max_val)

    def reset_min_max_vals(self):
        self.min_val.copy_(torch.tensor(float("inf")))
        self.max_val.copy_(torch.tensor(float("-inf")))


class MovingAverageMinMaxObserver(MinMaxObserver):
    __module__ = "torch.ao.quantization.observer"
    def __init__(self, averaging_constant=0.01, dtype=torch.quint8, qscheme=torch.per_tensor_affine, reduce_range=False, quant_min=None, quant_max=None, eps=_EPS, is_dynamic=False, **kwargs):
        if not is_per_tensor(qscheme):
            raise NotImplementedError("MovingAverageMinMaxObserver's qscheme only support                 torch.per_tensor_symmetric and torch.per_tensor_affine.                 but got: %s" % (qscheme,))
        self.averaging_constant = averaging_constant
        if is_dynamic and self.averaging_constant != 1:
            raise NotImplementedError("MovingAverageMinMaxObserver doesn't support dynamic quantization for averaging constant of %s" % self.averaging_constant)
        super().__init__(dtype=dtype, qscheme=qscheme, reduce_range=reduce_range, quant_min=quant_min, quant_max=quant_max, eps=eps, is_dynamic=is_dynamic, **kwargs)

    def forward(self, x_orig):
        if x_orig.numel() == 0:
            return x_orig
        x = x_orig.detach()
        x = x.to(self.min_val.dtype)
        min_val = self.min_val
        max_val = self.max_val
        if min_val == float("inf") and max_val == float("-inf"):
            min_val, max_val = torch.aminmax(x)
        else:
            min_val_cur, max_val_cur = torch.aminmax(x)
            min_val = min_val + self.averaging_constant * (min_val_cur - min_val)
            max_val = max_val + self.averaging_constant * (max_val_cur - max_val)
        self.min_val.copy_(min_val)
        self.max_val.copy_(max_val)
        return x_orig


class PerChannelMinMaxObserver(UniformQuantizationObserverBase):
    __module__ = "torch.ao.quantization.observer"
    def __init__(self, ch_axis=0, dtype=torch.quint8, qscheme=torch.per_channel_affine, reduce_range=False, quant_min=None, quant_max=None, factory_kwargs=None, eps=_EPS, is_dynamic=False, **kwargs):
        if not is_per_channel(qscheme):
            raise NotImplementedError("PerChannelMinMaxObserver's qscheme only support                     torch.per_channel_symmetric, torch.per_channel_affine and torch.per_channel_affine_float_qparams.")
        if is_dynamic:
            raise NotImplementedError("PerChannelMinMaxObserver doesn't support dynamic quantization")
        super().__init__(dtype=dtype, qscheme=qscheme, reduce_range=reduce_range, quant_min=quant_min, quant_max=quant_max, factory_kwargs=factory_kwargs, eps=eps, is_dynamic=is_dynamic, **kwargs)
        fk = _factory_kwargs(factory_kwargs)
        self.ch_axis = ch_axis
        self.register_buffer("min_val", torch.tensor([], **fk))
        self.register_buffer("max_val", torch.tensor([], **fk))
        if self.qscheme == torch.per_channel_symmetric and self.reduce_range and self.dtype == torch.quint8:
            raise NotImplementedError("Cannot reduce range for symmetric quantization for quint8")

    def forward(self, x_orig):
        return self._forward(x_orig)

    def _forward(self, x_orig):
        if x_orig.numel() == 0:
            return x_orig
        x = x_orig.detach()
        min_val = self.min_val
        max_val = self.max_val
        new_axis_list = list(range(len(x.size())))
        new_axis_list[self.ch_axis] = 0
        new_axis_list[0] = self.ch_axis
        y = x.permute(new_axis_list)
        y = y.to(self.min_val.dtype)
        y = torch.flatten(y, start_dim=1)
        if min_val.numel() == 0 or max_val.numel() == 0:
            min_val, max_val = torch.aminmax(y, dim=1)
        else:
            min_val_cur, max_val_cur = torch.aminmax(y, dim=1)
            min_val = torch.min(min_val_cur, min_val)
            max_val = torch.max(max_val_cur, max_val)
        self.min_val.resize_(min_val.shape)
        self.max_val.resize_(max_val.shape)
        self.min_val.copy_(min_val)
        self.max_val.copy_(max_val)
        return x_orig

    def calculate_qparams(self):
        return self._calculate_qparams(self.min_val, self.max_val)

    def extra_repr(self):
        return "min_val={}, max_val={}".format(self.min_val, self.max_val)

    def _load_from_state_dict(self, state_dict, prefix, local_metadata, strict, missing_keys, unexpected_keys, error_msgs):
        for name in ("min_val", "max_val"):
            key = prefix + name
            if key in state_dict:
                getattr(self, name).resize_(state_dict[key].shape)
            elif strict:
                missing_keys.append(key)
        super()._load_from_state_dict(state_dict, prefix, local_metadata, False, missing_keys, unexpected_keys, error_msgs)

    def reset_min_max_vals(self):
        self.min_val = torch.rand(0)
        self.max_val = torch.rand(0)


class MovingAveragePerChannelMinMaxObserver(PerChannelMinMaxObserver):
    __module__ = "torch.ao.quantization.observer"
    def __init__(self, averaging_constant=0.01, ch_axis=0, dtype=torch.quint8, qscheme=torch.per_channel_affine, reduce_range=False, quant_min=None, quant_max=None, eps=_EPS, is_dynamic=False, **kwargs):
        if not is_per_channel(qscheme):
            raise NotImplementedError("MovingAveragePerChannelMinMaxObserver's qscheme only support                     torch.per_channel_symmetric, torch.per_channel_affine and torch.per_channel_affine_float_qparams.")
        if is_dynamic:
            raise NotImplementedError("MovingAveragePerChannelMinMaxObserver doesn't support dynamic quantization")
        super().__init__(ch_axis=ch_axis, dtype=dtype, qscheme=qscheme, reduce_range=reduce_range, quant_min=quant_min, quant_max=quant_max, eps=eps, is_dynamic=is_dynamic, **kwargs)
        self.averaging_constant = averaging_constant

    def forward(self, x_orig):
        if x_orig.numel() == 0:
            return x_orig
        x = x_orig.detach()
        x = x.to(self.min_val.dtype)
        min_val = self.min_val
        max_val = self.max_val
        new_axis_list = list(range(len(x.size())))
        new_axis_list[self.ch_axis] = 0
        new_axis_list[0] = self.ch_axis
        y = x.permute(new_axis_list)
        y = torch.flatten(y, start_dim=1)
        if min_val.numel() == 0 or max_val.numel() == 0:
            min_val, max_val = torch.aminmax(y, dim=1)
        else:
            min_val_cur, max_val_cur = torch.aminmax(y, dim=1)
            min_val = min_val + self.averaging_constant * (min_val_cur - min_val)
            max_val = max_val + self.averaging_constant * (max_val_cur - max_val)
        self.min_val.resize_(min_val.shape)
        self.max_val.resize_(max_val.shape)
        self.min_val.copy_(min_val)
        self.max_val.copy_(max_val)
        return x_orig


class HistogramObserver(UniformQuantizationObserverBase):
    __module__ = "torch.ao.quantization.observer"
    """PyTorch's HistogramObserver: a running histogram of the values
    (bins over [min, max], upsampled and re-binned when the range grows),
    and scale/zero point from the range minimizing the L2 quantization
    error (_non_linear_param_search)."""

    def __init__(self, bins=2048, dtype=torch.quint8, qscheme=torch.per_tensor_affine, reduce_range=False, quant_min=None, quant_max=None, factory_kwargs=None, eps=_EPS, is_dynamic=False, **kwargs):
        if not is_per_tensor(qscheme):
            raise NotImplementedError("HistogramObserver's qscheme only support torch.per_tensor_symmetric                     and torch.per_tensor_affine.")
        if is_dynamic:
            raise NotImplementedError("HistogramObserver doesn't support dynamic quantization")
        super().__init__(dtype=dtype, qscheme=qscheme, reduce_range=reduce_range, quant_min=quant_min, quant_max=quant_max, factory_kwargs=factory_kwargs, eps=eps, is_dynamic=is_dynamic, **kwargs)
        fk = _factory_kwargs(factory_kwargs)
        self.bins = bins
        self.register_buffer("histogram", torch.zeros(self.bins, **fk))
        self.register_buffer("min_val", torch.tensor(float("inf"), **fk))
        self.register_buffer("max_val", torch.tensor(float("-inf"), **fk))
        self.dst_nbins = 2 ** torch.iinfo(self.dtype).bits
        self.upsample_rate = 16

    def _get_norm(self, delta_begin, delta_end, density):
        norm = (delta_end * delta_end * delta_end - delta_begin * delta_begin * delta_begin) / 3
        return density * norm

    def _compute_quantization_error(self, next_start_bin, next_end_bin):
        bin_width = (self.max_val.item() - self.min_val.item()) / self.bins
        dst_bin_width = bin_width * (next_end_bin - next_start_bin + 1) / self.dst_nbins
        if dst_bin_width == 0.0:
            return 0.0
        src_bin = torch.arange(self.bins)
        src_bin_begin = (src_bin - next_start_bin) * bin_width
        src_bin_end = src_bin_begin + bin_width
        dst_bin_of_begin = torch.clamp(torch.div(src_bin_begin, dst_bin_width, rounding_mode="floor"), 0, self.dst_nbins - 1)
        dst_bin_of_begin_center = (dst_bin_of_begin + 0.5) * dst_bin_width
        dst_bin_of_end = torch.clamp(torch.div(src_bin_end, dst_bin_width, rounding_mode="floor"), 0, self.dst_nbins - 1)
        density = self.histogram / bin_width
        norm = torch.zeros(self.bins)
        delta_begin = src_bin_begin - dst_bin_of_begin_center
        delta_end = dst_bin_width / 2
        norm += self._get_norm(delta_begin, torch.ones(self.bins) * delta_end, density)
        norm += (dst_bin_of_end - dst_bin_of_begin - 1) * self._get_norm(torch.tensor(-dst_bin_width / 2), torch.tensor(dst_bin_width / 2), density)
        dst_bin_of_end_center = dst_bin_of_end * dst_bin_width + dst_bin_width / 2
        delta_begin = -dst_bin_width / 2
        delta_end = src_bin_end - dst_bin_of_end_center
        norm += self._get_norm(torch.tensor(delta_begin), delta_end, density)
        return norm.sum().item()

    def _non_linear_param_search(self):
        if self.histogram.size()[0] != self.bins:
            raise AssertionError("bins mismatch")
        bin_width = (self.max_val - self.min_val) / self.bins
        total = torch.sum(self.histogram).item()
        csum = torch.cumsum(self.histogram, dim=0).tolist()
        stepsize = 1e-05
        alpha = 0.0
        beta = 1.0
        start_bin = 0
        end_bin = self.bins - 1
        norm_min = float("inf")
        while alpha < beta:
            next_alpha = alpha + stepsize
            next_beta = beta - stepsize
            l = start_bin
            r = end_bin
            while l < end_bin and csum[l] < next_alpha * total:
                l = l + 1
            while r > start_bin and csum[r] > next_beta * total:
                r = r - 1
            next_start_bin = start_bin
            next_end_bin = end_bin
            if (l - start_bin) > (end_bin - r):
                next_start_bin = l
                alpha = next_alpha
            else:
                next_end_bin = r
                beta = next_beta
            if next_start_bin == start_bin and next_end_bin == end_bin:
                continue
            norm = self._compute_quantization_error(next_start_bin, next_end_bin)
            if norm > norm_min:
                break
            norm_min = norm
            start_bin = next_start_bin
            end_bin = next_end_bin
        new_min = self.min_val + bin_width * start_bin
        new_max = self.min_val + bin_width * (end_bin + 1)
        return new_min, new_max

    def _upscale_histogram(self, histogram, orig_min, orig_max, update_min, update_max):
        histogram = histogram.repeat_interleave(self.upsample_rate) / self.upsample_rate
        bin_size = (orig_max - orig_min) / (self.bins * self.upsample_rate)
        mid_points_histogram = torch.linspace(orig_min, orig_max, self.bins * self.upsample_rate + 1)[:-1] + 0.5 * bin_size
        boundaries_new_histogram = torch.linspace(update_min, update_max, self.bins + 1)
        bucket_assignments = torch.bucketize(mid_points_histogram, boundaries_new_histogram, right=True) - 1
        bucket_assignments[bucket_assignments >= self.bins] = self.bins - 1
        bucket_assignments[bucket_assignments < 0] = 0
        return torch.bincount(bucket_assignments, weights=histogram, minlength=self.bins)

    def _combine_histograms(self, orig_hist, orig_min, orig_max, update_hist, update_min, update_max):
        if update_min == orig_min and update_max == orig_max:
            return orig_hist + update_hist
        if orig_min == orig_max:
            bin_value = torch.sum(orig_hist)
            transformed_orig_hist = torch.histc(orig_min, bins=self.bins, min=float(update_min), max=float(update_max)) * bin_value
            return transformed_orig_hist + update_hist
        if update_min > orig_min:
            raise AssertionError("update_min must be <= orig_min")
        if update_max < orig_max:
            raise AssertionError("update_max must be >= orig_max")
        transformed_orig_hist = self._upscale_histogram(orig_hist, orig_min, orig_max, update_min, update_max)
        return update_hist + transformed_orig_hist

    def reset_histogram(self, x, min_val, max_val):
        self.min_val.resize_(min_val.shape)
        self.min_val.copy_(min_val)
        self.max_val.resize_(max_val.shape)
        self.max_val.copy_(max_val)
        new_histogram = torch.histc(x, self.bins, min=float(min_val), max=float(max_val))
        self.histogram.detach_().resize_(new_histogram.shape)
        self.histogram.copy_(new_histogram)

    def forward(self, x_orig):
        if x_orig.numel() == 0:
            return x_orig
        x = x_orig.detach()
        x_min, x_max = torch.aminmax(x)
        if x_min == -math.inf or x_max == math.inf:
            _warn("torch.inf detected in input tensor, ignoring input", stacklevel=2)
            x = x[x.abs() != math.inf]
            if x.numel() == 0:
                return x_orig
            x_min, x_max = torch.aminmax(x)
        current_min = self.min_val
        current_max = self.max_val
        is_uninitialized = self.min_val == float("inf") or self.max_val == float("-inf")
        if is_uninitialized:
            self.reset_histogram(x, x_min, x_max)
        else:
            update_min, update_max = x_min, x_max
            new_min = torch.min(current_min, update_min)
            new_max = torch.max(current_max, update_max)
            update_histogram = torch.histc(x, self.bins, min=float(new_min), max=float(new_max))
            if new_min == current_min and new_max == current_max:
                combined = self.histogram + update_histogram
                self.histogram.detach_().resize_(combined.shape)
                self.histogram.copy_(combined)
            else:
                combined = self._combine_histograms(self.histogram, current_min.clone(), current_max.clone(), update_histogram, new_min, new_max)
                self.histogram.detach_().resize_(combined.shape)
                self.histogram.copy_(combined)
                self.min_val.copy_(new_min)
                self.max_val.copy_(new_max)
        return x_orig

    def calculate_qparams(self):
        is_uninitialized = self.min_val == float("inf") and self.max_val == float("-inf")
        if is_uninitialized:
            _warn("must run observer before calling calculate_qparams.                                    Returning default scale and zero point ", stacklevel=2)
            return torch.tensor([1.0]), torch.tensor([0])
        if self.bins != len(self.histogram):
            raise AssertionError("The number of bins in histogram should be equal to the number of bins supplied while making this observer")
        new_min, new_max = self._non_linear_param_search()
        return self._calculate_qparams(new_min, new_max)

    def extra_repr(self):
        return "min_val={}, max_val={}".format(self.min_val, self.max_val)


class FixedQParamsObserver(ObserverBase):
    __module__ = "torch.ao.quantization.observer"
    def __init__(self, scale, zero_point, dtype=torch.quint8, qscheme=torch.per_tensor_affine, quant_min=0, quant_max=255, is_dynamic=False, **kwargs):
        if is_dynamic:
            raise NotImplementedError("FixedQParamsObserver doesn't support dynamic quantization")
        super().__init__(dtype=dtype, is_dynamic=is_dynamic)
        self.quant_min = quant_min
        self.quant_max = quant_max
        self.register_buffer("scale", torch.tensor([scale], dtype=torch.float))
        self.register_buffer("zero_point", torch.tensor([zero_point], dtype=torch.int))
        self.dtype = dtype
        self.qscheme = qscheme

    def forward(self, X):
        return X

    def calculate_qparams(self):
        return self.scale, self.zero_point


class PlaceholderObserver(ObserverBase):
    __module__ = "torch.ao.quantization.observer"
    def __init__(self, dtype=torch.float32, custom_op_name="", compute_dtype=None, quant_min=None, quant_max=None, qscheme=None, eps=None, is_dynamic=False):
        super().__init__(dtype=dtype, is_dynamic=is_dynamic)
        if qscheme is None:
            qscheme = torch.per_tensor_affine
        if eps is None:
            eps = _EPS
        self.dtype = dtype
        self.qscheme = qscheme
        self.quant_min = quant_min
        self.quant_max = quant_max
        self.eps = eps
        self.custom_op = custom_op_name
        if compute_dtype:
            is_dynamic = True
            _warn("Please use `is_dynamic` instead of `compute_dtype`.                     `compute_dtype` will be deprecated in a future release                     of PyTorch.", stacklevel=2)

    def forward(self, x):
        return x

    def extra_repr(self):
        return "dtype=%s, is_dynamic=%s" % (self.dtype, self.is_dynamic)

    def calculate_qparams(self):
        raise Exception("calculate_qparams should not be called for PlaceholderObserver")


class RecordingObserver(ObserverBase):
    __module__ = "torch.ao.quantization.observer"
    def __init__(self, dtype=torch.quint8):
        super().__init__(dtype=dtype, is_dynamic=False)
        self.tensor_val = []

    def forward(self, x):
        self.tensor_val.append(x.clone())
        return x

    def calculate_qparams(self):
        raise Exception("calculate_qparams should not be called for RecordingObserver")

    def get_tensor_value(self):
        return self.tensor_val


class NoopObserver(ObserverBase):
    __module__ = "torch.ao.quantization.observer"
    def __init__(self, dtype=torch.float16, custom_op_name=""):
        super().__init__(dtype=dtype, is_dynamic=False)
        self.dtype = dtype
        self.custom_op = custom_op_name

    def forward(self, x):
        return x

    def calculate_qparams(self):
        raise Exception("calculate_qparams should not be called for NoopObserver")


class ReuseInputObserver(ObserverBase):
    __module__ = "torch.ao.quantization.observer"
    def __init__(self):
        super().__init__(torch.quint8, is_dynamic=False)

    def forward(self, x):
        return x

    def calculate_qparams(self):
        raise Exception("calculate_qparams should not be called for ReuseInputObserver")


def _is_activation_post_process(module):
    return isinstance(module, (ObserverBase, FakeQuantizeBase))


def get_observer_state_dict(mod):
    od = OrderedDict()
    for k, v in mod.state_dict().items():
        if "activation_post_process" in k:
            od[k] = v
    return od


def load_observer_state_dict(mod, obs_dict):
    missing_keys = []
    unexpected_keys = []
    for name, module in mod.named_modules():
        prefix = name + "."
        if _is_activation_post_process(module):
            module._load_from_state_dict(obs_dict, prefix, {}, False, missing_keys, unexpected_keys, [])
    for k in missing_keys:
        if "observer" in k or "activation_post_process" in k:
            raise Exception("Missing keys for observer %s in state_dict" % k)
    for k in unexpected_keys:
        if "observer" in k or "activation_post_process" in k:
            raise Exception("Unexpected keys for observer %s in state_dict" % k)


default_observer = MinMaxObserver.with_args(quant_min=0, quant_max=127)
default_placeholder_observer = PlaceholderObserver
default_debug_observer = RecordingObserver
default_weight_observer = MinMaxObserver.with_args(dtype=torch.qint8, qscheme=torch.per_tensor_symmetric)
weight_observer_range_neg_127_to_127 = MinMaxObserver.with_args(dtype=torch.qint8, qscheme=torch.per_tensor_symmetric, quant_min=-127, quant_max=127, eps=2 ** (-12))
default_histogram_observer = HistogramObserver.with_args(quant_min=0, quant_max=127)
default_per_channel_weight_observer = PerChannelMinMaxObserver.with_args(dtype=torch.qint8, qscheme=torch.per_channel_symmetric)
per_channel_weight_observer_range_neg_127_to_127 = PerChannelMinMaxObserver.with_args(dtype=torch.qint8, qscheme=torch.per_channel_symmetric, quant_min=-127, quant_max=127, eps=2 ** (-12))
default_dynamic_quant_observer = PlaceholderObserver.with_args(dtype=torch.quint8, quant_min=0, quant_max=255, is_dynamic=True)
default_float_qparams_observer = PerChannelMinMaxObserver.with_args(dtype=torch.quint8, qscheme=torch.per_channel_affine_float_qparams, ch_axis=0)
default_fixed_qparams_range_neg1to1_observer = FixedQParamsObserver.with_args(scale=2.0 / 256.0, zero_point=128, dtype=torch.quint8, quant_min=0, quant_max=255)
default_fixed_qparams_range_0to1_observer = FixedQParamsObserver.with_args(scale=1.0 / 256.0, zero_point=0, dtype=torch.quint8, quant_min=0, quant_max=255)
default_symmetric_fixed_qparams_observer = default_fixed_qparams_range_neg1to1_observer
default_affine_fixed_qparams_observer = default_fixed_qparams_range_0to1_observer
default_reuse_input_observer = ReuseInputObserver


# ---- fake quantization (torch/ao/quantization/fake_quantize.py) ------------------------------
def _is_float_qparams(qscheme):
    return qscheme == torch.per_channel_affine_float_qparams


def _is_symmetric_quant(qscheme):
    return qscheme in (torch.per_tensor_symmetric, torch.per_channel_symmetric)


class FakeQuantizeBase(nn.Module):
    __module__ = "torch.ao.quantization.fake_quantize"
    def __init__(self):
        super().__init__()
        self.register_buffer("fake_quant_enabled", torch.tensor([1], dtype=torch.uint8))
        self.register_buffer("observer_enabled", torch.tensor([1], dtype=torch.uint8))

    def forward(self, x):
        raise NotImplementedError

    def calculate_qparams(self, **kwargs):
        raise NotImplementedError

    def enable_fake_quant(self, enabled=True):
        self.fake_quant_enabled[0] = 1 if enabled else 0

    def disable_fake_quant(self):
        self.enable_fake_quant(False)

    def enable_observer(self, enabled=True):
        self.observer_enabled[0] = 1 if enabled else 0

    def disable_observer(self):
        self.enable_observer(False)

    @classmethod
    def with_args(cls, **kwargs):
        return _with_args(cls, **kwargs)


def _iinfo(dtype):
    return torch.iinfo(dtype)


class FakeQuantize(FakeQuantizeBase):
    __module__ = "torch.ao.quantization.fake_quantize"
    def __init__(self, observer=MovingAverageMinMaxObserver, quant_min=None, quant_max=None, is_dynamic=False, **observer_kwargs):
        super().__init__()
        if quant_min is not None and quant_max is not None:
            if quant_min > quant_max:
                raise AssertionError("quant_min must be less than or equal to quant_max")
            dtype = observer_kwargs.get("dtype", torch.quint8)
            if hasattr(observer, "p"):
                dtype = getattr(getattr(observer, "p", {}), "keywords", {}).get("dtype", dtype)
            if _iinfo(dtype).min > quant_min:
                raise AssertionError("quant_min out of bound")
            if quant_max > _iinfo(dtype).max:
                raise AssertionError("quant_max out of bound")
            observer_kwargs.update({"quant_min": quant_min, "quant_max": quant_max})
        observer_kwargs["is_dynamic"] = is_dynamic
        self.activation_post_process = observer(**observer_kwargs)
        self.quant_min = self.activation_post_process.quant_min
        self.quant_max = self.activation_post_process.quant_max
        self.is_dynamic = self.activation_post_process.is_dynamic
        zero_point_dtype = torch.float if _is_float_qparams(self.activation_post_process.qscheme) else torch.int
        self.register_buffer("scale", torch.tensor([1.0], dtype=torch.float))
        self.register_buffer("zero_point", torch.tensor([0], dtype=zero_point_dtype))
        self.dtype = self.activation_post_process.dtype
        self.qscheme = self.activation_post_process.qscheme
        self.ch_axis = self.activation_post_process.ch_axis if hasattr(self.activation_post_process, "ch_axis") else -1
        if not (is_per_channel(self.qscheme) or is_per_tensor(self.qscheme)):
            raise AssertionError("Only per channel and per tensor quantization are supported in fake quantize got qscheme: " + str(self.qscheme))
        self.is_per_channel = is_per_channel(self.qscheme)

    def calculate_qparams(self):
        return self.activation_post_process.calculate_qparams()

    def forward(self, X):
        if self.observer_enabled[0] == 1:
            self.activation_post_process(X.detach())
            _scale, _zero_point = self.calculate_qparams()
            if self.scale.shape != _scale.shape:
                self.scale.resize_(_scale.shape)
                self.zero_point.resize_(_zero_point.shape)
            self.scale.copy_(_scale)
            self.zero_point.copy_(_zero_point)
        if self.fake_quant_enabled[0] == 1:
            if self.is_per_channel:
                X = torch.fake_quantize_per_channel_affine(X, self.scale, self.zero_point, self.ch_axis, self.activation_post_process.quant_min, self.activation_post_process.quant_max)
            else:
                X = torch.fake_quantize_per_tensor_affine(X, self.scale, self.zero_point, self.activation_post_process.quant_min, self.activation_post_process.quant_max)
        return X

    def extra_repr(self):
        return "fake_quant_enabled=%s, observer_enabled=%s, quant_min=%s, quant_max=%s, dtype=%s, qscheme=%s, ch_axis=%s, scale=%s, zero_point=%s" % (
            self.fake_quant_enabled, self.observer_enabled, self.activation_post_process.quant_min, self.activation_post_process.quant_max,
            self.dtype, self.qscheme, self.ch_axis, self.scale, self.zero_point)

    def _save_to_state_dict(self, destination, prefix, keep_vars):
        super()._save_to_state_dict(destination, prefix, keep_vars)
        destination[prefix + "scale"] = self.scale
        destination[prefix + "zero_point"] = self.zero_point

    def _load_from_state_dict(self, state_dict, prefix, local_metadata, strict, missing_keys, unexpected_keys, error_msgs):
        for name in ("scale", "zero_point"):
            key = prefix + name
            if key in state_dict:
                getattr(self, name).resize_(state_dict[key].shape)
            elif strict:
                missing_keys.append(key)
        super()._load_from_state_dict(state_dict, prefix, local_metadata, strict, missing_keys, unexpected_keys, error_msgs)


class FixedQParamsFakeQuantize(FakeQuantize):
    __module__ = "torch.ao.quantization.fake_quantize"
    def __init__(self, observer):
        super().__init__(observer=observer)
        if type(self.activation_post_process) is not FixedQParamsObserver:
            raise AssertionError("%s's observer must be a %s" % (self.__class__.__name__, FixedQParamsObserver.__name__))
        self._observer_ctr = observer
        self.scale = self.activation_post_process.scale
        self.zero_point = self.activation_post_process.zero_point
        if not is_per_tensor(self.qscheme):
            raise AssertionError("Only per tensor quantization is supported FixedQParamsFakeQuantize module, got qscheme:" + str(self.qscheme))

    def calculate_qparams(self):
        return self.scale, self.zero_point

    def extra_repr(self):
        return "fake_quant_enabled=%s, observer_enabled=%s, scale=%s, zero_point=%s, dtype=%s, quant_min=%s, quant_max=%s, qscheme=%s" % (
            self.fake_quant_enabled, self.observer_enabled, self.scale, self.zero_point, self.dtype,
            self.activation_post_process.quant_min, self.activation_post_process.quant_max, self.qscheme)


class FusedMovingAvgObsFakeQuantize(FakeQuantize):
    __module__ = "torch.ao.quantization.fake_quantize"
    def __init__(self, observer=MovingAverageMinMaxObserver, quant_min=0, quant_max=255, **observer_kwargs):
        super().__init__(observer, quant_min, quant_max, **observer_kwargs)
        if not isinstance(self.activation_post_process, (MovingAverageMinMaxObserver, MovingAveragePerChannelMinMaxObserver)):
            raise AssertionError("Fused observer+fake_quant module only works with MovingAverageMinMaxObserver")
        self.register_buffer("fake_quant_enabled", torch.tensor([1], dtype=torch.long))
        self.register_buffer("observer_enabled", torch.tensor([1], dtype=torch.long))
        self.is_symmetric_quant = _is_symmetric_quant(self.activation_post_process.qscheme)

    def calculate_qparams(self):
        return self.activation_post_process.calculate_qparams()

    def extra_repr(self):
        return "fake_quant_enabled=%s, observer_enabled=%s, scale=%s, zero_point=%s, dtype=%s, quant_min=%s, quant_max=%s, qscheme=%s, reduce_range=%s" % (
            self.fake_quant_enabled, self.observer_enabled, self.scale, self.zero_point, self.dtype, self.activation_post_process.quant_min,
            self.activation_post_process.quant_max, self.qscheme, self.activation_post_process.reduce_range)

    def forward(self, X):
        return torch.fused_moving_avg_obs_fake_quant(X, self.observer_enabled, self.fake_quant_enabled, self.activation_post_process.min_val,
                                                     self.activation_post_process.max_val, self.scale, self.zero_point,
                                                     self.activation_post_process.averaging_constant, self.activation_post_process.quant_min,
                                                     self.activation_post_process.quant_max, self.ch_axis, self.is_per_channel, self.is_symmetric_quant)


default_fake_quant = FakeQuantize.with_args(observer=MovingAverageMinMaxObserver, quant_min=0, quant_max=255, dtype=torch.quint8, qscheme=torch.per_tensor_affine, reduce_range=True)
default_weight_fake_quant = FakeQuantize.with_args(observer=MovingAverageMinMaxObserver, quant_min=-128, quant_max=127, dtype=torch.qint8, qscheme=torch.per_tensor_symmetric, reduce_range=False)
default_dynamic_fake_quant = FakeQuantize.with_args(observer=MovingAverageMinMaxObserver, quant_min=0, quant_max=255, is_dynamic=True, dtype=torch.quint8, averaging_constant=1)
default_fixed_qparams_range_neg1to1_fake_quant = FixedQParamsFakeQuantize.with_args(observer=default_fixed_qparams_range_neg1to1_observer)
default_fixed_qparams_range_0to1_fake_quant = FixedQParamsFakeQuantize.with_args(observer=default_fixed_qparams_range_0to1_observer)
default_symmetric_fixed_qparams_fake_quant = default_fixed_qparams_range_neg1to1_fake_quant
default_affine_fixed_qparams_fake_quant = default_fixed_qparams_range_0to1_fake_quant
default_per_channel_weight_fake_quant = FakeQuantize.with_args(observer=MovingAveragePerChannelMinMaxObserver, quant_min=-128, quant_max=127, dtype=torch.qint8, qscheme=torch.per_channel_symmetric, reduce_range=False, ch_axis=0)
default_embedding_fake_quant = FakeQuantize.with_args(observer=MovingAveragePerChannelMinMaxObserver, qscheme=torch.per_channel_affine_float_qparams, dtype=torch.quint8, quant_min=0, quant_max=255, ch_axis=0, averaging_constant=1)
default_histogram_fake_quant = FakeQuantize.with_args(observer=HistogramObserver, quant_min=0, quant_max=255, dtype=torch.quint8, qscheme=torch.per_tensor_affine, reduce_range=True)
default_fused_act_fake_quant = FusedMovingAvgObsFakeQuantize.with_args(observer=MovingAverageMinMaxObserver, quant_min=0, quant_max=255, dtype=torch.quint8)
default_fused_wt_fake_quant = FusedMovingAvgObsFakeQuantize.with_args(observer=MovingAverageMinMaxObserver, quant_min=-128, quant_max=127, dtype=torch.qint8, qscheme=torch.per_tensor_symmetric)
default_fused_per_channel_wt_fake_quant = FusedMovingAvgObsFakeQuantize.with_args(observer=MovingAveragePerChannelMinMaxObserver, quant_min=-128, quant_max=127, dtype=torch.qint8, qscheme=torch.per_channel_symmetric)
fused_wt_fake_quant_range_neg_127_to_127 = FusedMovingAvgObsFakeQuantize.with_args(observer=MovingAverageMinMaxObserver, quant_min=-127, quant_max=127, dtype=torch.qint8, qscheme=torch.per_tensor_symmetric, eps=2 ** (-12))
fused_per_channel_wt_fake_quant_range_neg_127_to_127 = FusedMovingAvgObsFakeQuantize.with_args(observer=MovingAveragePerChannelMinMaxObserver, quant_min=-127, quant_max=127, dtype=torch.qint8, qscheme=torch.per_channel_symmetric, eps=2 ** (-12))


def disable_fake_quant(mod):
    if isinstance(mod, FakeQuantizeBase):
        mod.disable_fake_quant()


def enable_fake_quant(mod):
    if isinstance(mod, FakeQuantizeBase):
        mod.enable_fake_quant()


def disable_observer(mod):
    if isinstance(mod, FakeQuantizeBase):
        mod.disable_observer()


def enable_observer(mod):
    if isinstance(mod, FakeQuantizeBase):
        mod.enable_observer()


# ---- qconfig (torch/ao/quantization/qconfig.py) ----------------------------------------------
class QConfig(namedtuple("QConfig", ["activation", "weight"])):
    __module__ = "torch.ao.quantization.qconfig"
    __slots__ = ()

    def __new__(cls, activation, weight):
        if isinstance(activation, nn.Module) or isinstance(weight, nn.Module):
            raise ValueError("QConfig received observer instance, please pass observer class instead. Use MyObserver.with_args(x=1) to override arguments to constructor if needed")
        return super().__new__(cls, activation, weight)


class QConfigDynamic(namedtuple("QConfigDynamic", ["activation", "weight"])):
    __module__ = "torch.ao.quantization.qconfig"
    __slots__ = ()

    def __new__(cls, activation=torch.nn.Identity, weight=torch.nn.Identity):
        if isinstance(weight, nn.Module):
            raise ValueError("QConfigDynamic received observer instance, please pass observer class instead. Use MyObserver.with_args(x=1) to override arguments to constructor if needed")
        return super().__new__(cls, activation, weight)


QConfigAny = QConfig

default_qconfig = QConfig(activation=default_observer, weight=default_weight_observer)
default_debug_qconfig = QConfig(weight=default_weight_observer, activation=default_debug_observer)
default_per_channel_qconfig = QConfig(activation=default_observer, weight=default_per_channel_weight_observer)
default_dynamic_qconfig = QConfig(activation=default_dynamic_quant_observer, weight=default_weight_observer)
float16_dynamic_qconfig = QConfig(activation=PlaceholderObserver.with_args(dtype=torch.float16, is_dynamic=True), weight=PlaceholderObserver.with_args(dtype=torch.float16))
float16_static_qconfig = QConfig(activation=PlaceholderObserver.with_args(dtype=torch.float16), weight=PlaceholderObserver.with_args(dtype=torch.float16))
per_channel_dynamic_qconfig = QConfig(activation=default_dynamic_quant_observer, weight=default_per_channel_weight_observer)
float_qparams_weight_only_qconfig = QConfig(activation=default_placeholder_observer, weight=default_float_qparams_observer)
default_qat_qconfig = QConfig(activation=default_fake_quant, weight=default_weight_fake_quant)
default_dynamic_qat_qconfig = QConfig(activation=default_dynamic_fake_quant, weight=default_weight_fake_quant)
default_weight_only_qconfig = QConfig(activation=torch.nn.Identity, weight=default_weight_fake_quant)
default_activation_only_qconfig = QConfig(activation=default_fake_quant, weight=torch.nn.Identity)
default_qat_qconfig_v2 = QConfig(activation=default_fused_act_fake_quant, weight=default_fused_wt_fake_quant)
default_reuse_input_qconfig = QConfig(activation=default_reuse_input_observer, weight=NoopObserver)
default_symmetric_qnnpack_qconfig = QConfig(activation=HistogramObserver.with_args(dtype=torch.qint8, reduce_range=False, eps=2 ** (-12)), weight=weight_observer_range_neg_127_to_127)
default_per_channel_symmetric_qnnpack_qconfig = QConfig(activation=HistogramObserver.with_args(dtype=torch.qint8, reduce_range=False, eps=2 ** (-12)), weight=per_channel_weight_observer_range_neg_127_to_127)
default_embedding_qat_qconfig = QConfig(activation=NoopObserver.with_args(dtype=torch.float32), weight=default_embedding_fake_quant)
default_quint8_weight_qconfig = QConfig(activation=HistogramObserver, weight=MinMaxObserver)
default_symmetric_qnnpack_qat_qconfig = QConfig(activation=FusedMovingAvgObsFakeQuantize.with_args(observer=MovingAverageMinMaxObserver, quant_min=-128, quant_max=127, dtype=torch.qint8, reduce_range=False, eps=2 ** (-12)), weight=fused_wt_fake_quant_range_neg_127_to_127)
default_per_channel_symmetric_qnnpack_qat_qconfig = QConfig(activation=FusedMovingAvgObsFakeQuantize.with_args(observer=MovingAverageMinMaxObserver, quant_min=-128, quant_max=127, dtype=torch.qint8, reduce_range=False, eps=2 ** (-12)), weight=fused_per_channel_wt_fake_quant_range_neg_127_to_127)


def get_default_qconfig(backend="x86", version=0):
    supported_backends = ["fbgemm", "x86", "qnnpack", "onednn"]
    if backend not in supported_backends:
        raise AssertionError("backend: " + str(backend) + " not supported. backend must be one of %s" % supported_backends)
    if version != 0:
        raise AssertionError("Version number: " + str(version) + " in get_default_qconfig is not supported. Version number must be 0")
    if backend == "fbgemm":
        return QConfig(activation=HistogramObserver.with_args(reduce_range=True), weight=default_per_channel_weight_observer)
    if backend == "qnnpack":
        return QConfig(activation=HistogramObserver.with_args(reduce_range=False), weight=default_weight_observer)
    if backend == "onednn":
        return QConfig(activation=HistogramObserver.with_args(reduce_range=False), weight=default_per_channel_weight_observer)
    return QConfig(activation=HistogramObserver.with_args(reduce_range=True), weight=default_per_channel_weight_observer)


def get_default_qat_qconfig(backend="x86", version=1):
    supported_backends = ["fbgemm", "x86", "qnnpack", "onednn"]
    if backend not in supported_backends:
        raise AssertionError("backend: " + str(backend) + " not supported. backend must be one of %s" % supported_backends)
    if version == 0:
        if backend == "qnnpack":
            return QConfig(activation=FakeQuantize.with_args(observer=MovingAverageMinMaxObserver, quant_min=0, quant_max=255, reduce_range=False), weight=default_weight_fake_quant)
        if backend == "onednn":
            return QConfig(activation=FakeQuantize.with_args(observer=MovingAverageMinMaxObserver, quant_min=0, quant_max=255), weight=default_per_channel_weight_fake_quant)
        return QConfig(activation=FakeQuantize.with_args(observer=MovingAverageMinMaxObserver, quant_min=0, quant_max=255, reduce_range=True), weight=default_per_channel_weight_fake_quant)
    if version == 1:
        if backend == "qnnpack":
            return QConfig(activation=FusedMovingAvgObsFakeQuantize.with_args(observer=MovingAverageMinMaxObserver, quant_min=0, quant_max=255, reduce_range=False), weight=default_fused_wt_fake_quant)
        if backend == "onednn":
            return QConfig(activation=FusedMovingAvgObsFakeQuantize.with_args(observer=MovingAverageMinMaxObserver, quant_min=0, quant_max=255), weight=default_fused_per_channel_wt_fake_quant)
        return QConfig(activation=FusedMovingAvgObsFakeQuantize.with_args(observer=MovingAverageMinMaxObserver, quant_min=0, quant_max=255, reduce_range=True), weight=default_fused_per_channel_wt_fake_quant)
    raise AssertionError("Version number: " + str(version) + "in get_default_qat_qconfig is not supported. Version number must be 0 or 1")


def _assert_valid_qconfig(qconfig, mod):
    if qconfig is None:
        return
    if isinstance(mod, (torch.nn.ConvTranspose1d, torch.nn.ConvTranspose2d, torch.nn.ConvTranspose3d)):
        if qconfig.weight is None:
            return
        example_observer = qconfig.weight()
        if isinstance(example_observer, (PerChannelMinMaxObserver, MovingAveragePerChannelMinMaxObserver)):
            raise AssertionError("Per channel weight observer is not supported yet for ConvTranspose{n}d.")


def _add_module_to_qconfig_obs_ctr(qconfig, module):
    # One CPU device: the observers need no device placement.
    return qconfig


def _obs_or_fq_ctr_equals(a, b):
    if isinstance(a, _PartialWrapper) and isinstance(b, _PartialWrapper):
        ka, kb = dict(a.p.keywords), dict(b.p.keywords)
        same = True
        if "observer" in ka and "observer" in kb:
            same = _obs_or_fq_ctr_equals(ka.pop("observer"), kb.pop("observer"))
        return same and ka == kb and a.p.func == b.p.func and a.p.args == b.p.args
    return a == b


def qconfig_equals(q1, q2):
    if q1 is None or q2 is None:
        return q1 == q2
    try:
        return _obs_or_fq_ctr_equals(q1.activation, q2.activation) and _obs_or_fq_ctr_equals(q1.weight, q2.weight)
    except AttributeError:
        return q1 == q2


def _activation_is_memoryless(qconfig):
    act = qconfig.activation()
    if isinstance(act, FakeQuantizeBase) and hasattr(act, "activation_post_process"):
        act = act.activation_post_process
    return hasattr(act, "averaging_constant") and act.averaging_constant == 1


# ---- stubs (torch/ao/quantization/stubs.py) --------------------------------------------------
class QuantStub(nn.Module):
    __module__ = "torch.ao.quantization.stubs"
    def __init__(self, qconfig=None):
        super().__init__()
        if qconfig:
            self.qconfig = qconfig

    def forward(self, x):
        return x


class DeQuantStub(nn.Module):
    __module__ = "torch.ao.quantization.stubs"
    def __init__(self, qconfig=None):
        super().__init__()
        if qconfig:
            self.qconfig = qconfig

    def forward(self, x):
        return x


class QuantWrapper(nn.Module):
    __module__ = "torch.ao.quantization.stubs"
    def __init__(self, module):
        super().__init__()
        qconfig = getattr(module, "qconfig", None)
        self.add_module("quant", QuantStub(qconfig))
        self.add_module("dequant", DeQuantStub(qconfig))
        self.add_module("module", module)
        self.train(module.training)

    def forward(self, X):
        X = self.quant(X)
        X = self.module(X)
        return self.dequant(X)


# ---- module mappings (torch/ao/quantization/quantization_mappings.py) ------------------------
DEFAULT_STATIC_QUANT_MODULE_MAPPINGS = {
    QuantStub: nnq.Quantize, DeQuantStub: nnq.DeQuantize, nn.BatchNorm2d: nnq.BatchNorm2d, nn.BatchNorm3d: nnq.BatchNorm3d,
    nn.Dropout: nnq.Dropout, nn.Conv1d: nnq.Conv1d, nn.Conv2d: nnq.Conv2d, nn.Conv3d: nnq.Conv3d, nn.ConvTranspose1d: nnq.ConvTranspose1d,
    nn.ConvTranspose2d: nnq.ConvTranspose2d, nn.ConvTranspose3d: nnq.ConvTranspose3d, nn.ELU: nnq.ELU, nn.Embedding: nnq.Embedding,
    nn.EmbeddingBag: nnq.EmbeddingBag, nn.GroupNorm: nnq.GroupNorm, nn.Hardswish: nnq.Hardswish, nn.InstanceNorm1d: nnq.InstanceNorm1d,
    nn.InstanceNorm2d: nnq.InstanceNorm2d, nn.InstanceNorm3d: nnq.InstanceNorm3d, nn.LayerNorm: nnq.LayerNorm, nn.LeakyReLU: nnq.LeakyReLU,
    nn.NonDynamicallyQuantizableLinear: nnq.Linear, nn.Linear: nnq.Linear, nn.ReLU6: nnq.ReLU6, nn.PReLU: nnq.PReLU,
    nnq.FloatFunctional: nnq.QFunctional, nni.BNReLU2d: nniq.BNReLU2d, nni.ConvReLU1d: nniq.ConvReLU1d, nni.ConvReLU2d: nniq.ConvReLU2d,
    nni.LinearReLU: nniq.LinearReLU, nniqat.ConvBn1d: nnq.Conv1d, nniqat.ConvBn2d: nnq.Conv2d, nniqat.ConvBnReLU1d: nniq.ConvReLU1d,
    nniqat.ConvBnReLU2d: nniq.ConvReLU2d, nniqat.ConvReLU2d: nniq.ConvReLU2d, nniqat.LinearReLU: nniq.LinearReLU, nnqat.Linear: nnq.Linear,
    nnqat.Conv2d: nnq.Conv2d,
}
DEFAULT_QAT_MODULE_MAPPINGS = {
    nn.Conv2d: nnqat.Conv2d, nn.Conv3d: nnqat.Conv3d, nn.Linear: nnqat.Linear, nn.NonDynamicallyQuantizableLinear: nnqat.Linear,
    nni.ConvBn1d: nniqat.ConvBn1d, nni.ConvBn2d: nniqat.ConvBn2d, nni.ConvBnReLU1d: nniqat.ConvBnReLU1d, nni.ConvBnReLU2d: nniqat.ConvBnReLU2d,
    nni.ConvReLU2d: nniqat.ConvReLU2d, nni.LinearReLU: nniqat.LinearReLU,
}
DEFAULT_DYNAMIC_QUANT_MODULE_MAPPINGS = {
    nn.GRUCell: nnqd.GRUCell, nn.Linear: nnqd.Linear, nn.NonDynamicallyQuantizableLinear: nnqd.Linear, nn.LSTM: nnqd.LSTM, nn.GRU: nnqd.GRU,
    nn.LSTMCell: nnqd.LSTMCell, nn.RNNCell: nnqd.RNNCell, nni.LinearReLU: nnqd._LinearReLU, nn.EmbeddingBag: nnq.EmbeddingBag,
    nn.Embedding: nnq.Embedding,
}
_INCLUDE_QCONFIG_PROPAGATE_LIST = {nn.Sequential}
DEFAULT_MODULE_TO_ACT_POST_PROCESS = {
    nn.Hardsigmoid: default_fixed_qparams_range_0to1_fake_quant, nn.Sigmoid: default_fixed_qparams_range_0to1_fake_quant,
    nn.Softmax: default_fixed_qparams_range_0to1_fake_quant, nn.Tanh: default_fixed_qparams_range_neg1to1_fake_quant,
}
DEFAULT_FLOAT_TO_QUANTIZED_OPERATOR_MAPPINGS = {}


def no_observer_set():
    return set()


def get_default_static_quant_module_mappings():
    return dict(DEFAULT_STATIC_QUANT_MODULE_MAPPINGS)


def get_default_static_quant_reference_module_mappings():
    raise NotImplementedError("reference quantized modules (is_reference=True) are not supported on Zipp")


def get_static_quant_module_class(float_module_class, additional_static_quant_mapping=None, is_reference=False):
    all_mappings = get_combined_dict(DEFAULT_STATIC_QUANT_MODULE_MAPPINGS, additional_static_quant_mapping or {})
    cls = all_mappings.get(float_module_class, None)
    if cls is None:
        raise AssertionError("Floating point module class %s does not have a corresponding quantized module class" % str(float_module_class))
    return cls


def get_dynamic_quant_module_class(float_module_class, additional_dynamic_quant_mapping=None):
    all_mappings = get_combined_dict(DEFAULT_DYNAMIC_QUANT_MODULE_MAPPINGS, additional_dynamic_quant_mapping or {})
    cls = all_mappings.get(float_module_class, None)
    if cls is None:
        raise AssertionError("Floating point module class %s does not have a corresponding quantized module class" % str(float_module_class))
    return cls


def get_default_qat_module_mappings():
    return dict(DEFAULT_QAT_MODULE_MAPPINGS)


def get_default_dynamic_quant_module_mappings():
    return DEFAULT_DYNAMIC_QUANT_MODULE_MAPPINGS


def get_default_qconfig_propagation_list():
    return set(DEFAULT_STATIC_QUANT_MODULE_MAPPINGS.keys()) | set(DEFAULT_QAT_MODULE_MAPPINGS.keys()) | set(DEFAULT_DYNAMIC_QUANT_MODULE_MAPPINGS.keys()) | _INCLUDE_QCONFIG_PROPAGATE_LIST


def get_default_compare_output_module_list():
    return (set(DEFAULT_STATIC_QUANT_MODULE_MAPPINGS.values()) | set(DEFAULT_QAT_MODULE_MAPPINGS.values()) | set(DEFAULT_DYNAMIC_QUANT_MODULE_MAPPINGS.values())
            | set(DEFAULT_STATIC_QUANT_MODULE_MAPPINGS.keys()) | set(DEFAULT_QAT_MODULE_MAPPINGS.keys()) | set(DEFAULT_DYNAMIC_QUANT_MODULE_MAPPINGS.keys())
            | _INCLUDE_QCONFIG_PROPAGATE_LIST)


def get_default_float_to_quantized_operator_mappings():
    return dict(DEFAULT_FLOAT_TO_QUANTIZED_OPERATOR_MAPPINGS)


def get_quantized_operator(float_op):
    quantized_op = DEFAULT_FLOAT_TO_QUANTIZED_OPERATOR_MAPPINGS.get(float_op)
    if quantized_op is None:
        raise AssertionError("Operator %s does not have corresponding quantized op" % str(float_op))
    return quantized_op


def _get_special_act_post_process(module):
    return DEFAULT_MODULE_TO_ACT_POST_PROCESS.get(type_before_parametrizations(module))


def _has_special_act_post_process(module):
    return module.training and type(module) in DEFAULT_MODULE_TO_ACT_POST_PROCESS


# ---- quantize (torch/ao/quantization/quantize.py) ----------------------------------------------
_DEPRECATION_WARNING = "torch.ao.quantization is deprecated and will be removed in 2.10. \nFor migrations of users: \n1. Eager mode quantization (torch.ao.quantization.quantize, torch.ao.quantization.quantize_dynamic), please migrate to use torchao eager mode quantize_ API instead \n2. FX graph mode quantization (torch.ao.quantization.quantize_fx.prepare_fx,torch.ao.quantization.quantize_fx.convert_fx, please migrate to use torchao pt2e quantization API instead (prepare_pt2e, convert_pt2e) \n3. pt2e quantization has been migrated to torchao (https://github.com/pytorch/ao/tree/main/torchao/quantization/pt2e) \nsee https://github.com/pytorch/ao/issues/2259 for more details"


def _deprecated():
    _warn(_DEPRECATION_WARNING, "DeprecationWarning")


is_activation_post_process = _is_activation_post_process


def get_default_custom_config_dict():
    return {"float_to_observed_custom_module_class": {}, "observed_to_quantized_custom_module_class": {}}


def _propagate_qconfig_helper(module, qconfig_dict, qconfig_parent=None, prefix="", prepare_custom_config_dict=None):
    module_qconfig = qconfig_dict.get(type_before_parametrizations(module), qconfig_parent)
    module_qconfig = qconfig_dict.get(prefix, module_qconfig)
    module_qconfig = getattr(module, "qconfig", module_qconfig)
    _assert_valid_qconfig(module_qconfig, module)
    qconfig_with_device_check = _add_module_to_qconfig_obs_ctr(module_qconfig, module)
    module.qconfig = qconfig_with_device_check
    for name, child in module.named_children():
        module_prefix = prefix + "." + name if prefix else name
        if prepare_custom_config_dict is None or not (name in prepare_custom_config_dict.get("non_traceable_module_name", []) or type(child) in prepare_custom_config_dict.get("non_traceable_module_class", [])):
            _propagate_qconfig_helper(child, qconfig_dict, qconfig_with_device_check, module_prefix)


def propagate_qconfig_(module, qconfig_dict=None, prepare_custom_config_dict=None):
    if qconfig_dict is None:
        qconfig_dict = {}
    if prepare_custom_config_dict is None:
        prepare_custom_config_dict = {}
    _propagate_qconfig_helper(module, qconfig_dict, prepare_custom_config_dict=prepare_custom_config_dict)


def _observer_forward_hook(self, input, output):
    return self.activation_post_process(output)


def _observer_forward_pre_hook(self, input):
    return self.activation_post_process(input[0])


def _register_activation_post_process_hook(module, pre_hook=False):
    if not hasattr(module, "activation_post_process"):
        raise AssertionError("Expect activation_post_process attribute already attached to the module")
    if pre_hook:
        module.register_forward_pre_hook(_observer_forward_pre_hook, prepend=True)
    else:
        module.register_forward_hook(_observer_forward_hook, prepend=True)


def _add_observer_(module, qconfig_propagation_list=None, non_leaf_module_list=None, device=None, custom_module_class_mapping=None):
    if qconfig_propagation_list is None:
        qconfig_propagation_list = get_default_qconfig_propagation_list()
    if custom_module_class_mapping is None:
        custom_module_class_mapping = {}

    def get_activation_post_process(qconfig, special_act_post_process=None):
        return qconfig.activation() if special_act_post_process is None else special_act_post_process()

    def needs_observation(m):
        return hasattr(m, "qconfig") and m.qconfig is not None

    def insert_activation_post_process(m, special_act_post_process=None):
        if needs_observation(m) and not isinstance(m, DeQuantStub):
            m.add_module("activation_post_process", get_activation_post_process(m.qconfig, special_act_post_process))
            _register_activation_post_process_hook(m, pre_hook=_activation_is_memoryless(m.qconfig))

    for name, child in module.named_children():
        if type_before_parametrizations(child) is nn.Dropout:
            continue
        elif issubclass(type_before_parametrizations(child), (nnq.FloatFunctional, nnq.QFunctional)):
            if needs_observation(child):
                if not hasattr(child, "activation_post_process"):
                    raise AssertionError("functional class %s has no pre-defined `activation_post_process`" % type_before_parametrizations(child))
                child.activation_post_process = get_activation_post_process(child.qconfig)
        elif isinstance(child, nni._FusedModule):
            if needs_observation(child):
                insert_activation_post_process(child)
        elif non_leaf_module_list is not None and type_before_parametrizations(child) in non_leaf_module_list:
            if needs_observation(child):
                insert_activation_post_process(child)
        elif _has_special_act_post_process(child):
            insert_activation_post_process(child, _get_special_act_post_process(child))
        elif needs_observation(child) and type_before_parametrizations(child) in custom_module_class_mapping:
            observed_class = custom_module_class_mapping[type_before_parametrizations(child)]
            observed_child = observed_class.from_float(child)
            setattr(module, name, observed_child)
            insert_activation_post_process(observed_child)
        else:
            _add_observer_(child, qconfig_propagation_list, non_leaf_module_list, device, custom_module_class_mapping)
    if has_no_children_ignoring_parametrizations(module) and not isinstance(module, torch.nn.Sequential) and type_before_parametrizations(module) in qconfig_propagation_list:
        insert_activation_post_process(module)
    if hasattr(module, "weight_fake_quant") and not isinstance(module, torch.nn.Sequential) and type_before_parametrizations(module) in qconfig_propagation_list:
        insert_activation_post_process(module)


def add_quant_dequant(module):
    if has_no_children_ignoring_parametrizations(module) and hasattr(module, "qconfig") and module.qconfig:
        return QuantWrapper(module)
    for name, child in module.named_children():
        module._modules[name] = add_quant_dequant(child)
    return module


def _prepare(model, inplace=False, allow_list=None, observer_non_leaf_module_list=None, prepare_custom_config_dict=None):
    if prepare_custom_config_dict is None:
        prepare_custom_config_dict = get_default_custom_config_dict()
    custom_module_class_mapping = prepare_custom_config_dict.get("float_to_observed_custom_module_class", {})
    if not inplace:
        model = copy.deepcopy(model)
    qconfig_propagation_list = allow_list
    if allow_list is None:
        qconfig_propagation_list = get_default_qconfig_propagation_list()
    propagate_qconfig_(model, qconfig_dict=None)
    if not any(hasattr(m, "qconfig") and m.qconfig for m in model.modules()):
        _warn("None of the submodule got qconfig applied. Make sure you passed correct configuration through `qconfig_dict` or by assigning the `.qconfig` attribute directly on submodules", stacklevel=2)
    _add_observer_(model, qconfig_propagation_list, observer_non_leaf_module_list, custom_module_class_mapping=custom_module_class_mapping)
    return model


def prepare(model, inplace=False, allow_list=None, observer_non_leaf_module_list=None, prepare_custom_config_dict=None):
    _deprecated()
    return _prepare(model, inplace, allow_list, observer_non_leaf_module_list, prepare_custom_config_dict)


def _remove_activation_post_process(module):
    if hasattr(module, "activation_post_process") and _is_activation_post_process(module.activation_post_process):
        delattr(module, "activation_post_process")

    def remove_hooks(pre_hook=False):
        hook_map = module._forward_pre_hooks if pre_hook else module._forward_hooks
        observer_hook = _observer_forward_pre_hook if pre_hook else _observer_forward_hook
        for handle_id in [h for h, fn in hook_map.items() if fn is observer_hook]:
            hook_map.pop(handle_id)
    remove_hooks(pre_hook=True)
    remove_hooks(pre_hook=False)


def _remove_qconfig(module):
    for child in module.children():
        _remove_qconfig(child)
    if hasattr(module, "qconfig"):
        del module.qconfig
    _remove_activation_post_process(module)


def quantize(model, run_fn, run_args, mapping=None, inplace=False):
    _deprecated()
    if mapping is None:
        mapping = get_default_static_quant_module_mappings()
    if not inplace:
        model = copy.deepcopy(model)
    model.eval()
    _prepare(model, inplace=True)
    run_fn(model, *run_args)
    _convert_top(model, mapping, inplace=True)
    return model


def quantize_dynamic(model, qconfig_spec=None, dtype=torch.qint8, mapping=None, inplace=False):
    _deprecated()
    if qconfig_spec is None:
        if dtype == torch.qint8:
            qconfig_spec = {nn.Linear: default_dynamic_qconfig, nn.LSTM: default_dynamic_qconfig, nn.GRU: default_dynamic_qconfig,
                            nn.LSTMCell: default_dynamic_qconfig, nn.RNNCell: default_dynamic_qconfig, nn.GRUCell: default_dynamic_qconfig}
        elif dtype == torch.float16:
            qconfig_spec = {nn.Linear: float16_dynamic_qconfig, nn.LSTM: float16_dynamic_qconfig, nn.GRU: float16_dynamic_qconfig,
                            nn.LSTMCell: float16_dynamic_qconfig, nn.RNNCell: float16_dynamic_qconfig, nn.GRUCell: float16_dynamic_qconfig}
        elif dtype == torch.quint8:
            qconfig_spec = {nn.EmbeddingBag: float_qparams_weight_only_qconfig, nn.Embedding: float_qparams_weight_only_qconfig}
        else:
            raise ValueError("Don't know how to quantize with default settings for %s. Provide full qconfig please" % dtype)
    elif isinstance(qconfig_spec, set):
        if dtype is torch.qint8:
            default = default_dynamic_qconfig
        elif dtype is torch.float16:
            default = float16_dynamic_qconfig
        elif dtype is torch.quint8:
            default = float_qparams_weight_only_qconfig
        else:
            raise RuntimeError("Unknown dtype specified for quantize_dynamic: ", str(dtype))
        qconfig_spec = dict(zip(qconfig_spec, itertools.repeat(default)))
    if mapping is None:
        mapping = get_default_dynamic_quant_module_mappings()
    if not inplace:
        model = copy.deepcopy(model)
    model.eval()
    propagate_qconfig_(model, qconfig_spec)
    _convert_top(model, mapping, inplace=True)
    return model


def _prepare_qat(model, mapping=None, inplace=False):
    if not model.training:
        raise AssertionError("prepare_qat only works on models in training mode")
    if mapping is None:
        mapping = get_default_qat_module_mappings()
    if not inplace:
        model = copy.deepcopy(model)
    propagate_qconfig_(model, qconfig_dict=None)
    _convert_top(model, mapping=mapping, inplace=True, remove_qconfig=False)
    _prepare(model, observer_non_leaf_module_list=set(mapping.values()), inplace=True)
    return model


def prepare_qat(model, mapping=None, inplace=False):
    _deprecated()
    return _prepare_qat(model, mapping, inplace)


def quantize_qat(model, run_fn, run_args, inplace=False):
    _deprecated()
    if not inplace:
        model = copy.deepcopy(model)
    model.train()
    _prepare_qat(model, inplace=True)
    run_fn(model, *run_args)
    _convert_top(model, inplace=True)
    return model


def _convert_top(module, mapping=None, inplace=False, remove_qconfig=True, is_reference=False, convert_custom_config_dict=None, use_precomputed_fake_quant=False):
    if not inplace:
        module = copy.deepcopy(module)
    _convert(module, mapping, inplace=True, is_reference=is_reference, convert_custom_config_dict=convert_custom_config_dict, use_precomputed_fake_quant=use_precomputed_fake_quant)
    if remove_qconfig:
        _remove_qconfig(module)
    return module


def convert(module, mapping=None, inplace=False, remove_qconfig=True, is_reference=False, convert_custom_config_dict=None, use_precomputed_fake_quant=False):
    _deprecated()
    return _convert_top(module, mapping, inplace, remove_qconfig, is_reference, convert_custom_config_dict, use_precomputed_fake_quant)


def _convert(module, mapping=None, inplace=False, is_reference=False, convert_custom_config_dict=None, use_precomputed_fake_quant=False):
    if mapping is None:
        mapping = get_default_static_quant_reference_module_mappings() if is_reference else get_default_static_quant_module_mappings()
    if convert_custom_config_dict is None:
        convert_custom_config_dict = get_default_custom_config_dict()
    custom_module_class_mapping = convert_custom_config_dict.get("observed_to_quantized_custom_module_class", {})
    if not inplace:
        module = copy.deepcopy(module)
    reassign = {}
    for name, mod in module.named_children():
        if not isinstance(mod, nni._FusedModule) and type_before_parametrizations(mod) not in custom_module_class_mapping:
            _convert(mod, mapping, True, is_reference, convert_custom_config_dict, use_precomputed_fake_quant=use_precomputed_fake_quant)
        reassign[name] = swap_module(mod, mapping, custom_module_class_mapping, use_precomputed_fake_quant)
    for key, value in reassign.items():
        module._modules[key] = value
    return module


def swap_module(mod, mapping, custom_module_class_mapping, use_precomputed_fake_quant=False):
    new_mod = mod
    if hasattr(mod, "qconfig") and mod.qconfig is not None:
        swapped = False
        if type_before_parametrizations(mod) in custom_module_class_mapping:
            new_mod = custom_module_class_mapping[type_before_parametrizations(mod)].from_observed(mod)
            swapped = True
        elif type_before_parametrizations(mod) in mapping:
            qmod = mapping[type_before_parametrizations(mod)]
            new_mod = qmod.from_float(mod, use_precomputed_fake_quant=use_precomputed_fake_quant)
            swapped = True
        if swapped:
            for pre_hook_fn in mod._forward_pre_hooks.values():
                new_mod.register_forward_pre_hook(pre_hook_fn)
            for hook_fn in mod._forward_hooks.values():
                if hook_fn is not _observer_forward_hook:
                    new_mod.register_forward_hook(hook_fn)
    return new_mod


def _get_observer_dict(mod, target_dict, prefix=""):
    def get_prefix(prefix):
        return prefix if prefix == "" else prefix + "."
    if hasattr(mod, "activation_post_process"):
        target_dict[get_prefix(prefix) + "activation_post_process"] = mod.activation_post_process
    for name, child in mod.named_children():
        module_prefix = get_prefix(prefix) + name if prefix else name
        _get_observer_dict(child, target_dict, module_prefix)


def default_eval_fn(model, calib_data):
    for data, target in calib_data:
        model(data)


# ---- fusion (torch/ao/quantization/fuse_modules.py, fuser_method_mappings.py) ----------------
def fuse_conv_bn(is_qat, conv, bn):
    if conv.training != bn.training:
        raise AssertionError("Conv and BN both must be in the same mode (train or eval).")
    fused_module_class_map = {nn.Conv1d: nni.ConvBn1d, nn.Conv2d: nni.ConvBn2d, nn.Conv3d: nni.ConvBn3d}
    if is_qat:
        if bn.num_features != conv.out_channels:
            raise AssertionError("Output channel of Conv2d must match num_features of BatchNorm2d.")
        if not bn.affine:
            raise AssertionError("Only support fusing BatchNorm2d with affine set to True")
        if not bn.track_running_stats:
            raise AssertionError("Only support fusing BatchNorm2d with tracking_running_stats set to True")
        fused_module_class = fused_module_class_map.get(type(conv))
        if fused_module_class is not None:
            return fused_module_class(conv, bn)
        raise NotImplementedError("Cannot fuse train modules: %s" % ((conv, bn),))
    return nn.utils.fuse_conv_bn_eval(conv, bn)


def fuse_conv_bn_relu(is_qat, conv, bn, relu):
    if not conv.training == bn.training == relu.training:
        raise AssertionError("Conv and BN both must be in the same mode (train or eval).")
    if is_qat:
        map_to_fused_module_train = {nn.Conv1d: nni.ConvBnReLU1d, nn.Conv2d: nni.ConvBnReLU2d, nn.Conv3d: nni.ConvBnReLU3d}
        if bn.num_features != conv.out_channels:
            raise AssertionError("Output channel of Conv2d must match num_features of BatchNorm2d")
        if not bn.affine:
            raise AssertionError("Only support fusing BatchNorm2d with affine set to True")
        if not bn.track_running_stats:
            raise AssertionError("Only support fusing BatchNorm2d with tracking_running_stats set to True")
        fused_module = map_to_fused_module_train.get(type(conv))
        if fused_module is not None:
            return fused_module(conv, bn, relu)
        raise NotImplementedError("Cannot fuse train modules: %s" % ((conv, bn, relu),))
    map_to_fused_module_eval = {nn.Conv1d: nni.ConvReLU1d, nn.Conv2d: nni.ConvReLU2d, nn.Conv3d: nni.ConvReLU3d}
    fused_module = map_to_fused_module_eval.get(type(conv))
    if fused_module is not None:
        return fused_module(nn.utils.fuse_conv_bn_eval(conv, bn), relu)
    raise NotImplementedError("Cannot fuse eval modules: %s" % ((conv, bn, relu),))


def fuse_linear_bn(is_qat, linear, bn):
    if linear.training != bn.training:
        raise AssertionError("Linear and BN both must be in the same mode (train or eval).")
    if is_qat:
        if bn.num_features != linear.out_features:
            raise AssertionError("Output features of Linear must match num_features of BatchNorm1d")
        if not bn.affine:
            raise AssertionError("Only support fusing BatchNorm1d with affine set to True")
        if not bn.track_running_stats:
            raise AssertionError("Only support fusing BatchNorm1d with tracking_running_stats set to True")
        return nni.LinearBn1d(linear, bn)
    return nn.utils.fuse_linear_bn_eval(linear, bn)


def fuse_convtranspose_bn(is_qat, convt, bn):
    if convt.training != bn.training:
        raise AssertionError("ConvTranspose and BN both must be in the same mode (train or eval).")
    if is_qat:
        raise Exception("Fusing ConvTranspose+BatchNorm not yet supported in QAT.")
    return nn.utils.fuse_conv_bn_eval(convt, bn, transpose=True)


def _sequential_wrapper2(sequential):
    def fuser_method(is_qat, m1, m2):
        return sequential(m1, m2)
    return fuser_method


_DEFAULT_OP_LIST_TO_FUSER_METHOD = {
    (nn.Conv1d, nn.BatchNorm1d): fuse_conv_bn, (nn.Conv1d, nn.BatchNorm1d, nn.ReLU): fuse_conv_bn_relu,
    (nn.Conv2d, nn.BatchNorm2d): fuse_conv_bn, (nn.Conv2d, nn.BatchNorm2d, nn.ReLU): fuse_conv_bn_relu,
    (nn.Conv3d, nn.BatchNorm3d): fuse_conv_bn, (nn.Conv3d, nn.BatchNorm3d, nn.ReLU): fuse_conv_bn_relu,
    (nn.Conv1d, nn.ReLU): _sequential_wrapper2(nni.ConvReLU1d), (nn.Conv2d, nn.ReLU): _sequential_wrapper2(nni.ConvReLU2d),
    (nn.Conv3d, nn.ReLU): _sequential_wrapper2(nni.ConvReLU3d), (nn.Linear, nn.BatchNorm1d): fuse_linear_bn,
    (nn.Linear, nn.ReLU): _sequential_wrapper2(nni.LinearReLU), (nn.BatchNorm2d, nn.ReLU): _sequential_wrapper2(nni.BNReLU2d),
    (nn.BatchNorm3d, nn.ReLU): _sequential_wrapper2(nni.BNReLU3d), (nn.ConvTranspose1d, nn.BatchNorm1d): fuse_convtranspose_bn,
    (nn.ConvTranspose2d, nn.BatchNorm2d): fuse_convtranspose_bn, (nn.ConvTranspose3d, nn.BatchNorm3d): fuse_convtranspose_bn,
}


def get_fuser_method(op_list, additional_fuser_method_mapping=None):
    all_mappings = get_combined_dict(_DEFAULT_OP_LIST_TO_FUSER_METHOD, additional_fuser_method_mapping or {})
    fuser_method = all_mappings.get(op_list, None)
    if fuser_method is None:
        raise AssertionError("did not find fuser method for: %s " % (op_list,))
    return fuser_method


def _get_module(model, submodule_key):
    cur = model
    for s in submodule_key.split("."):
        cur = getattr(cur, s)
    return cur


def _set_module(model, submodule_key, module):
    tokens = submodule_key.split(".")
    cur = model
    for s in tokens[:-1]:
        cur = getattr(cur, s)
    setattr(cur, tokens[-1], module)


def fuse_known_modules(mod_list, is_qat, additional_fuser_method_mapping=None):
    types = tuple(type_before_parametrizations(m) for m in mod_list)
    fuser_method = get_fuser_method(types, additional_fuser_method_mapping)
    new_mod = [None] * len(mod_list)
    fused = fuser_method(is_qat, *mod_list)
    for pre_hook_fn in mod_list[0]._forward_pre_hooks.values():
        fused.register_forward_pre_hook(pre_hook_fn)
    mod_list[0]._forward_pre_hooks.clear()
    for hook_fn in mod_list[-1]._forward_hooks.values():
        fused.register_forward_hook(hook_fn)
    mod_list[-1]._forward_hooks.clear()
    new_mod[0] = fused
    for i in range(1, len(mod_list)):
        identity = nn.Identity()
        identity.training = mod_list[0].training
        new_mod[i] = identity
    return new_mod


def _fuse_modules_helper(model, modules_to_fuse, is_qat, fuser_func=fuse_known_modules, fuse_custom_config_dict=None):
    if fuse_custom_config_dict is None:
        fuse_custom_config_dict = {}
    additional = fuse_custom_config_dict.get("additional_fuser_method_mapping", {})
    mod_list = [_get_module(model, item) for item in modules_to_fuse]
    new_mod_list = fuser_func(mod_list, is_qat, additional)
    for i, item in enumerate(modules_to_fuse):
        _set_module(model, item, new_mod_list[i])


def _fuse_modules(model, modules_to_fuse, is_qat, inplace=False, fuser_func=fuse_known_modules, fuse_custom_config_dict=None):
    if not inplace:
        model = copy.deepcopy(model)
    if all(isinstance(m, str) for m in modules_to_fuse):
        _fuse_modules_helper(model, modules_to_fuse, is_qat, fuser_func, fuse_custom_config_dict)
    else:
        for module_list in modules_to_fuse:
            _fuse_modules_helper(model, module_list, is_qat, fuser_func, fuse_custom_config_dict)
    return model


def fuse_modules(model, modules_to_fuse, inplace=False, fuser_func=fuse_known_modules, fuse_custom_config_dict=None):
    return _fuse_modules(model, modules_to_fuse, is_qat=False, inplace=inplace, fuser_func=fuser_func, fuse_custom_config_dict=fuse_custom_config_dict)


def fuse_modules_qat(model, modules_to_fuse, inplace=False, fuser_func=fuse_known_modules, fuse_custom_config_dict=None):
    return _fuse_modules(model, modules_to_fuse, is_qat=True, inplace=inplace, fuser_func=fuser_func, fuse_custom_config_dict=fuse_custom_config_dict)


def _no_fx(*args, **kwargs):
    raise NotImplementedError("FX graph mode quantization (torch.ao.quantization.quantize_fx) is not available on Zipp, which has no graph tracing; use eager mode (QuantStub/DeQuantStub with prepare/convert, prepare_qat or quantize_dynamic)")


prepare_fx = convert_fx = prepare_qat_fx = fuse_fx = get_default_qconfig_mapping = get_default_qat_qconfig_mapping = _no_fx
