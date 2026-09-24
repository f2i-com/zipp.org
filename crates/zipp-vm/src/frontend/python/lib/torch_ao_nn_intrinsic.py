"""torch.ao.nn.intrinsic for Zipp: the fused float modules torch.ao.quantization
builds (fuse_modules) and later converts as one unit (ConvReLU2d,
LinearReLU, ConvBn2d, ...). Each is an nn.Sequential of its parts, as in
PyTorch 2.11."""
import torch
from torch.nn import BatchNorm1d, BatchNorm2d, BatchNorm3d, Conv1d, Conv2d, Conv3d, Linear, ReLU
from torch.nn.utils.parametrize import type_before_parametrizations

__all__ = ["ConvReLU1d", "ConvReLU2d", "ConvReLU3d", "LinearReLU", "ConvBn1d", "ConvBn2d", "ConvBnReLU1d", "ConvBnReLU2d",
           "ConvBn3d", "ConvBnReLU3d", "BNReLU2d", "BNReLU3d", "LinearBn1d", "LinearLeakyReLU", "LinearTanh", "ConvAdd2d",
           "ConvAddReLU2d"]


class _FusedModule(torch.nn.Sequential):
    pass


def _check(types, mods):
    got = [type_before_parametrizations(m) for m in mods]
    if got != list(types):
        names = [t.__name__ for t in got]
        if len(names) == 2:
            raise AssertionError("Incorrect types for input modules: %s and %s" % tuple(names))
        raise AssertionError("Incorrect types for input modules: %s, %s, and %s" % tuple(names))


class ConvReLU1d(_FusedModule):
    def __init__(self, conv, relu):
        _check((Conv1d, ReLU), (conv, relu))
        super().__init__(conv, relu)


class ConvReLU2d(_FusedModule):
    def __init__(self, conv, relu):
        _check((Conv2d, ReLU), (conv, relu))
        super().__init__(conv, relu)


class ConvReLU3d(_FusedModule):
    def __init__(self, conv, relu):
        _check((Conv3d, ReLU), (conv, relu))
        super().__init__(conv, relu)


class LinearReLU(_FusedModule):
    def __init__(self, linear, relu):
        _check((Linear, ReLU), (linear, relu))
        super().__init__(linear, relu)


class ConvBn1d(_FusedModule):
    def __init__(self, conv, bn):
        _check((Conv1d, BatchNorm1d), (conv, bn))
        super().__init__(conv, bn)


class ConvBn2d(_FusedModule):
    def __init__(self, conv, bn):
        _check((Conv2d, BatchNorm2d), (conv, bn))
        super().__init__(conv, bn)


class ConvBn3d(_FusedModule):
    def __init__(self, conv, bn):
        _check((Conv3d, BatchNorm3d), (conv, bn))
        super().__init__(conv, bn)


class ConvBnReLU1d(_FusedModule):
    def __init__(self, conv, bn, relu):
        _check((Conv1d, BatchNorm1d, ReLU), (conv, bn, relu))
        super().__init__(conv, bn, relu)


class ConvBnReLU2d(_FusedModule):
    def __init__(self, conv, bn, relu):
        _check((Conv2d, BatchNorm2d, ReLU), (conv, bn, relu))
        super().__init__(conv, bn, relu)


class ConvBnReLU3d(_FusedModule):
    def __init__(self, conv, bn, relu):
        _check((Conv3d, BatchNorm3d, ReLU), (conv, bn, relu))
        super().__init__(conv, bn, relu)


class BNReLU2d(_FusedModule):
    def __init__(self, batch_norm, relu):
        _check((BatchNorm2d, ReLU), (batch_norm, relu))
        super().__init__(batch_norm, relu)


class BNReLU3d(_FusedModule):
    def __init__(self, batch_norm, relu):
        _check((BatchNorm3d, ReLU), (batch_norm, relu))
        super().__init__(batch_norm, relu)


class LinearBn1d(_FusedModule):
    def __init__(self, linear, bn):
        _check((Linear, BatchNorm1d), (linear, bn))
        super().__init__(linear, bn)


class LinearLeakyReLU(_FusedModule):
    def __init__(self, linear, leaky_relu):
        if not (type(linear) is Linear and type(leaky_relu) is torch.nn.LeakyReLU):
            raise AssertionError("Incorrect types for input modules: %s and %s" % (type(linear).__name__, type(leaky_relu).__name__))
        super().__init__(linear, leaky_relu)


class LinearTanh(_FusedModule):
    def __init__(self, linear, tanh):
        if not (type(linear) is Linear and type(tanh) is torch.nn.Tanh):
            raise AssertionError("Incorrect types for input modules: %s and %s" % (type(linear).__name__, type(tanh).__name__))
        super().__init__(linear, tanh)


class ConvAdd2d(_FusedModule):
    def __init__(self, conv, add):
        super().__init__(conv)
        self.add = add

    def forward(self, x1, x2):
        return self.add(self[0](x1), x2)


class ConvAddReLU2d(_FusedModule):
    def __init__(self, conv, add, relu):
        super().__init__(conv)
        self.add = add
        self.relu = relu

    def forward(self, x1, x2):
        return self.relu(self.add(self[0](x1), x2))


import torch._quant as _q

quantized = _q._LazyModule("torch.ao.nn.intrinsic.quantized")
qat = _q._LazyModule("torch.ao.nn.intrinsic.qat")
