"""torch.ao.nn.intrinsic.qat for Zipp: the fused quantization-aware training
modules (LinearReLU, ConvReLU1d/2d, ConvBn1d/2d, ConvBnReLU1d/2d) with
PyTorch 2.11's forward: a ConvBn scales the weight by the batch norm's
gamma / running std before fake-quantizing it, convolves, unscales and
applies the batch norm (the "approximate" path PyTorch uses by default)."""
import math
import torch
import torch.nn as nn
import torch.nn.functional as F
from torch.nn import init
from torch.nn.parameter import Parameter
from torch.nn.utils import fuse_conv_bn_weights
import torch.ao.nn.intrinsic as nni
import torch.ao.nn.qat as nnqat

__all__ = ["LinearReLU", "ConvBn1d", "ConvBnReLU1d", "ConvReLU1d", "ConvBn2d", "ConvBnReLU2d", "ConvReLU2d", "update_bn_stats", "freeze_bn_stats"]

_BN_CLASS_MAP = {1: nn.BatchNorm1d, 2: nn.BatchNorm2d, 3: nn.BatchNorm3d}


class LinearReLU(nnqat.Linear, nni._FusedModule):
    _FLOAT_MODULE = nni.LinearReLU

    def __init__(self, in_features, out_features, bias=True, qconfig=None):
        nnqat.Linear.__init__(self, in_features, out_features, bias, qconfig)

    def forward(self, input):
        return F.relu(F.linear(input, self.weight_fake_quant(self.weight), self.bias))

    @classmethod
    def from_float(cls, mod, use_precomputed_fake_quant=False):
        return super().from_float(mod, use_precomputed_fake_quant)

    def to_float(self):
        linear = torch.nn.Linear(self.in_features, self.out_features, self.bias is not None)
        linear.weight = torch.nn.Parameter(self.weight.detach())
        if self.bias is not None:
            linear.bias = torch.nn.Parameter(self.bias.detach())
        return nni.LinearReLU(linear, torch.nn.ReLU())


class _ConvBnMixin:
    _version = 2

    def _init_bn(self, out_channels, bias, eps, momentum, freeze_bn, qconfig, dim):
        if not qconfig:
            raise AssertionError("qconfig must be provided for QAT module")
        self.qconfig = qconfig
        self.freeze_bn = freeze_bn if self.training else True
        self.bn = _BN_CLASS_MAP[dim](out_channels, eps, momentum, True, True)
        self.weight_fake_quant = self.qconfig.weight()
        if bias:
            self.bias = Parameter(torch.empty(out_channels))
        else:
            self.register_parameter("bias", None)
        self.reset_bn_parameters()
        if self.training:
            if freeze_bn:
                self.freeze_bn_stats()
            else:
                self.update_bn_stats()
        else:
            self.freeze_bn_stats()
        self._enable_slow_path_for_better_numerical_stability = False

    def reset_running_stats(self):
        self.bn.reset_running_stats()

    def reset_bn_parameters(self):
        self.bn.reset_running_stats()
        init.uniform_(self.bn.weight)
        init.zeros_(self.bn.bias)
        if self.bias is not None:
            fan_in, _ = init._calculate_fan_in_and_fan_out(self.weight)
            bound = 1 / math.sqrt(fan_in)
            init.uniform_(self.bias, -bound, bound)

    def update_bn_stats(self):
        self.freeze_bn = False
        self.bn.training = True
        return self

    def freeze_bn_stats(self):
        self.freeze_bn = True
        self.bn.training = False
        return self

    def _forward(self, input):
        running_std = torch.sqrt(self.bn.running_var + self.bn.eps)
        scale_factor = self.bn.weight / running_std
        weight_shape = [1] * len(self.weight.shape)
        weight_shape[0] = -1
        bias_shape = [1] * len(self.weight.shape)
        bias_shape[1] = -1
        scaled_weight = self.weight_fake_quant(self.weight * scale_factor.reshape(weight_shape))
        if self.bias is not None:
            zero_bias = torch.zeros_like(self.bias, dtype=input.dtype)
        else:
            zero_bias = torch.zeros(self.out_channels, dtype=input.dtype)
        conv = self._conv_forward(input, scaled_weight, zero_bias)
        conv_orig = conv / scale_factor.reshape(bias_shape)
        if self.bias is not None:
            conv_orig = conv_orig + self.bias.reshape(bias_shape)
        return self.bn(conv_orig)

    def forward(self, input):
        return self._forward(input)

    def train(self, mode=True):
        self.training = mode
        if not self.freeze_bn:
            for module in self.children():
                module.train(mode)
        return self

    @classmethod
    def from_float(cls, mod, use_precomputed_fake_quant=False):
        if type(mod) is not cls._FLOAT_MODULE:
            raise AssertionError("qat." + cls.__name__ + ".from_float only works for " + cls._FLOAT_MODULE.__name__)
        if not hasattr(mod, "qconfig"):
            raise AssertionError("Input float module must have qconfig defined")
        if not mod.qconfig:
            raise AssertionError("Input float module must have a valid qconfig")
        qconfig = mod.qconfig
        conv, bn = mod[0], mod[1]
        qat_convbn = cls(conv.in_channels, conv.out_channels, conv.kernel_size, conv.stride, conv.padding, conv.dilation, conv.groups,
                         conv.bias is not None, conv.padding_mode, bn.eps, bn.momentum, False, qconfig)
        qat_convbn.weight = conv.weight
        qat_convbn.bias = conv.bias
        qat_convbn.bn.weight = bn.weight
        qat_convbn.bn.bias = bn.bias
        qat_convbn.bn.running_mean = bn.running_mean
        qat_convbn.bn.running_var = bn.running_var
        qat_convbn.bn.num_batches_tracked = bn.num_batches_tracked
        return qat_convbn

    def to_float(self):
        cls = type(self)
        conv = cls._FLOAT_CONV_MODULE(self.in_channels, self.out_channels, self.kernel_size, self.stride, self.padding, self.dilation,
                                      self.groups, self.bias is not None, self.padding_mode)
        conv.weight = torch.nn.Parameter(self.weight.detach())
        if self.bias is not None:
            conv.bias = torch.nn.Parameter(self.bias.detach())
        conv.weight, conv.bias = fuse_conv_bn_weights(conv.weight, conv.bias, self.bn.running_mean, self.bn.running_var, self.bn.eps,
                                                      self.bn.weight, self.bn.bias)
        if cls._FLOAT_RELU_MODULE is not None:
            conv_relu = cls._FUSED_FLOAT_MODULE(conv, cls._FLOAT_RELU_MODULE())
            conv_relu.train(self.training)
            return conv_relu
        conv.train(self.training)
        return conv


class ConvBn1d(_ConvBnMixin, nn.Conv1d, nni._FusedModule):
    _FLOAT_BN_MODULE = nn.BatchNorm1d
    _FLOAT_RELU_MODULE = None
    _FLOAT_MODULE = nni.ConvBn1d
    _FLOAT_CONV_MODULE = nn.Conv1d

    def __init__(self, in_channels, out_channels, kernel_size, stride=1, padding=0, dilation=1, groups=1, bias=None, padding_mode="zeros", eps=1e-05, momentum=0.1, freeze_bn=False, qconfig=None):
        nn.Conv1d.__init__(self, in_channels, out_channels, kernel_size, stride, padding, dilation, groups, False, padding_mode)
        self._init_bn(out_channels, bias, eps, momentum, freeze_bn, qconfig, 1)


class ConvBnReLU1d(ConvBn1d):
    _FLOAT_MODULE = nni.ConvBnReLU1d
    _FLOAT_RELU_MODULE = nn.ReLU
    _FUSED_FLOAT_MODULE = nni.ConvReLU1d

    def forward(self, input):
        return F.relu(self._forward(input))


class ConvBn2d(_ConvBnMixin, nn.Conv2d, nni._FusedModule):
    _FLOAT_BN_MODULE = nn.BatchNorm2d
    _FLOAT_RELU_MODULE = None
    _FLOAT_MODULE = nni.ConvBn2d
    _FLOAT_CONV_MODULE = nn.Conv2d

    def __init__(self, in_channels, out_channels, kernel_size, stride=1, padding=0, dilation=1, groups=1, bias=None, padding_mode="zeros", eps=1e-05, momentum=0.1, freeze_bn=False, qconfig=None):
        nn.Conv2d.__init__(self, in_channels, out_channels, kernel_size, stride, padding, dilation, groups, False, padding_mode)
        self._init_bn(out_channels, bias, eps, momentum, freeze_bn, qconfig, 2)


class ConvBnReLU2d(ConvBn2d):
    _FLOAT_MODULE = nni.ConvBnReLU2d
    _FLOAT_RELU_MODULE = nn.ReLU
    _FUSED_FLOAT_MODULE = nni.ConvReLU2d

    def forward(self, input):
        return F.relu(self._forward(input))


class ConvReLU1d(nnqat.Conv1d, nni._FusedModule):
    _FLOAT_MODULE = nni.ConvReLU1d
    _FLOAT_CONV_MODULE = nn.Conv1d
    _FLOAT_BN_MODULE = None
    _FLOAT_RELU_MODULE = nn.ReLU

    def forward(self, input):
        return F.relu(self._conv_forward(input, self.weight_fake_quant(self.weight), self.bias))


class ConvReLU2d(nnqat.Conv2d, nni._FusedModule):
    _FLOAT_MODULE = nni.ConvReLU2d
    _FLOAT_CONV_MODULE = nn.Conv2d
    _FLOAT_BN_MODULE = None
    _FLOAT_RELU_MODULE = nn.ReLU

    def forward(self, input):
        return F.relu(self._conv_forward(input, self.weight_fake_quant(self.weight), self.bias))


def update_bn_stats(mod):
    if type(mod) in (ConvBnReLU1d, ConvBnReLU2d, ConvBn1d, ConvBn2d):
        mod.update_bn_stats()


def freeze_bn_stats(mod):
    if type(mod) in (ConvBnReLU1d, ConvBnReLU2d, ConvBn1d, ConvBn2d):
        mod.freeze_bn_stats()
