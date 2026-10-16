"""torch.ao.nn.qat for Zipp: the quantization-aware training modules
prepare_qat swaps in (Linear, Conv1d, Conv2d, Conv3d). Each runs its float
op with the weight passed through `weight_fake_quant` (the qconfig's weight
fake quantizer), as in PyTorch 2.11."""
import torch
import torch.nn as nn
import torch.nn.functional as F
from torch.ao.nn.intrinsic import _FusedModule, LinearReLU as _LinearReLU
from torch.nn.utils.parametrize import type_before_parametrizations

__all__ = ["Linear", "Conv1d", "Conv2d", "Conv3d"]


class Linear(nn.Linear):
    _FLOAT_MODULE = nn.Linear

    def __init__(self, in_features, out_features, bias=True, qconfig=None, device=None, dtype=None):
        factory_kwargs = {"device": device, "dtype": dtype}
        super().__init__(in_features, out_features, bias, device=device, dtype=dtype)
        if not qconfig:
            raise AssertionError("qconfig must be provided for QAT module")
        self.qconfig = qconfig
        self.weight_fake_quant = qconfig.weight(factory_kwargs=factory_kwargs)

    def forward(self, input):
        return F.linear(input, self.weight_fake_quant(self.weight), self.bias)

    @classmethod
    def from_float(cls, mod, use_precomputed_fake_quant=False):
        if type_before_parametrizations(mod) != cls._FLOAT_MODULE:
            raise AssertionError("qat.%s.from_float only works for %s, got %s" % (cls.__name__, cls._FLOAT_MODULE.__name__, type_before_parametrizations(mod).__name__))
        if not hasattr(mod, "qconfig"):
            raise AssertionError("Input float module must have qconfig defined")
        if not mod.qconfig:
            raise AssertionError("Input float module must have a valid qconfig")
        if type_before_parametrizations(mod) == _LinearReLU:
            mod = mod[0]
        qconfig = mod.qconfig
        qat_linear = cls(mod.in_features, mod.out_features, bias=mod.bias is not None, qconfig=qconfig)
        qat_linear.weight = mod.weight
        qat_linear.bias = mod.bias
        return qat_linear

    def to_float(self):
        linear = torch.nn.Linear(self.in_features, self.out_features, self.bias is not None)
        linear.weight = torch.nn.Parameter(self.weight.detach())
        if self.bias is not None:
            linear.bias = torch.nn.Parameter(self.bias.detach())
        linear.train(self.training)
        return linear


def _conv_init(self, qconfig):
    if not qconfig:
        raise AssertionError("qconfig must be provided for QAT module")
    self.qconfig = qconfig
    self.weight_fake_quant = qconfig.weight(factory_kwargs={"device": None, "dtype": None})


def _conv_from_float(cls, mod):
    if type(mod) is not cls._FLOAT_MODULE:
        raise AssertionError("qat.%s.from_float only works for %s, got %s" % (cls.__name__, cls._FLOAT_MODULE.__name__, type(mod).__name__))
    if not hasattr(mod, "qconfig"):
        raise AssertionError("Input float module must have qconfig defined")
    if not mod.qconfig:
        raise AssertionError("Input float module must have a valid qconfig")
    if issubclass(type(mod), _FusedModule):
        mod = mod[0]
    qconfig = mod.qconfig
    qat_conv = cls(mod.in_channels, mod.out_channels, mod.kernel_size, stride=mod.stride, padding=mod.padding, dilation=mod.dilation,
                   groups=mod.groups, bias=mod.bias is not None, padding_mode=mod.padding_mode, qconfig=qconfig)
    qat_conv.weight = mod.weight
    qat_conv.bias = mod.bias
    return qat_conv


def _conv_to_float(self):
    cls = type(self)
    conv = cls._FLOAT_CONV_MODULE(self.in_channels, self.out_channels, self.kernel_size, self.stride, self.padding, self.dilation,
                                  self.groups, self.bias is not None, self.padding_mode)
    conv.weight = torch.nn.Parameter(self.weight.detach())
    if self.bias is not None:
        conv.bias = torch.nn.Parameter(self.bias.detach())
    if issubclass(cls, _FusedModule):
        fused = cls._FLOAT_MODULE(conv, cls._FLOAT_RELU_MODULE())
        fused.train(self.training)
        return fused
    return conv


class Conv1d(nn.Conv1d):
    _FLOAT_MODULE = nn.Conv1d
    _FLOAT_CONV_MODULE = nn.Conv1d

    def __init__(self, in_channels, out_channels, kernel_size, stride=1, padding=0, dilation=1, groups=1, bias=True, padding_mode="zeros", qconfig=None, device=None, dtype=None):
        nn.Conv1d.__init__(self, in_channels, out_channels, kernel_size, stride, padding, dilation, groups, bias, padding_mode, device=device, dtype=dtype)
        _conv_init(self, qconfig)

    def forward(self, input):
        return self._conv_forward(input, self.weight_fake_quant(self.weight), self.bias)

    @classmethod
    def from_float(cls, mod, use_precomputed_fake_quant=False):
        return _conv_from_float(cls, mod)

    def to_float(self):
        return _conv_to_float(self)


class Conv2d(nn.Conv2d):
    _FLOAT_MODULE = nn.Conv2d
    _FLOAT_CONV_MODULE = nn.Conv2d

    def __init__(self, in_channels, out_channels, kernel_size, stride=1, padding=0, dilation=1, groups=1, bias=True, padding_mode="zeros", qconfig=None, device=None, dtype=None):
        nn.Conv2d.__init__(self, in_channels, out_channels, kernel_size, stride, padding, dilation, groups, bias, padding_mode, device=device, dtype=dtype)
        _conv_init(self, qconfig)

    def forward(self, input):
        return self._conv_forward(input, self.weight_fake_quant(self.weight), self.bias)

    @classmethod
    def from_float(cls, mod, use_precomputed_fake_quant=False):
        return _conv_from_float(cls, mod)

    def to_float(self):
        return _conv_to_float(self)


class Conv3d(nn.Conv3d):
    _FLOAT_MODULE = nn.Conv3d
    _FLOAT_CONV_MODULE = nn.Conv3d

    def __init__(self, in_channels, out_channels, kernel_size, stride=1, padding=0, dilation=1, groups=1, bias=True, padding_mode="zeros", qconfig=None, device=None, dtype=None):
        nn.Conv3d.__init__(self, in_channels, out_channels, kernel_size, stride, padding, dilation, groups, bias, padding_mode, device=device, dtype=dtype)
        _conv_init(self, qconfig)

    def forward(self, input):
        return self._conv_forward(input, self.weight_fake_quant(self.weight), self.bias)

    @classmethod
    def from_float(cls, mod, use_precomputed_fake_quant=False):
        return _conv_from_float(cls, mod)

    def to_float(self):
        return _conv_to_float(self)
