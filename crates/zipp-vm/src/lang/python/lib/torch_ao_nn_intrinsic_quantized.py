"""torch.ao.nn.intrinsic.quantized for Zipp: the quantized fused modules
(LinearReLU, ConvReLU1d, ConvReLU2d) convert makes from the fused float or
QAT modules: the quantized op with ReLU folded into the requantization
(outputs below the zero point clamp to it), as PyTorch's linear_relu /
conv2d_relu kernels."""
import torch
import torch.ao.nn.quantized as nnq
import torch.ao.nn.intrinsic as nni
import torch.ao.nn.intrinsic.qat as nniqat
import torch._quant as _q
from torch.nn.utils import fuse_conv_bn_weights

__all__ = ["LinearReLU", "ConvReLU1d", "ConvReLU2d", "BNReLU2d"]


class LinearReLU(nnq.Linear):
    _FLOAT_MODULE = nni.LinearReLU

    def __init__(self, in_features, out_features, bias=True, dtype=torch.qint8):
        super().__init__(in_features, out_features, bias, dtype)

    def forward(self, x):
        w, b = self._weight_bias()
        return _q.linear(x, w, b, self.scale, self.zero_point, relu=True)

    def _get_name(self):
        return "QuantizedLinearReLU"

    @classmethod
    def from_float(cls, mod, use_precomputed_fake_quant=False):
        return super().from_float(mod, use_precomputed_fake_quant)

    @classmethod
    def from_reference(cls, ref_linear_relu, output_scale, output_zero_point):
        return super().from_reference(ref_linear_relu[0], output_scale, output_zero_point)


class ConvReLU1d(nnq.Conv1d):
    _FLOAT_MODULE = nni.ConvReLU1d
    _NNIQAT_CONV_BN_MODULE = nniqat.ConvBnReLU1d
    _NNI_CONV_RELU_MODULE = nni.ConvReLU1d
    _relu = True

    def _get_name(self):
        return "QuantizedConvReLU1d"

    @classmethod
    def from_float(cls, mod, use_precomputed_fake_quant=False):
        if type(mod) is nniqat.ConvBnReLU1d:
            mod.weight, mod.bias = fuse_conv_bn_weights(mod.weight, mod.bias, mod.bn.running_mean, mod.bn.running_var, mod.bn.eps, mod.bn.weight, mod.bn.bias)
            return cls.get_qconv(mod, mod.activation_post_process, mod.weight_fake_quant)
        return super().from_float(mod, use_precomputed_fake_quant)


class ConvReLU2d(nnq.Conv2d):
    _FLOAT_MODULE = nni.ConvReLU2d
    _NNIQAT_CONV_BN_MODULE = nniqat.ConvBnReLU2d
    _NNI_CONV_RELU_MODULE = nni.ConvReLU2d
    _relu = True

    def _get_name(self):
        return "QuantizedConvReLU2d"

    @classmethod
    def from_float(cls, mod, use_precomputed_fake_quant=False):
        if type(mod) is nniqat.ConvBnReLU2d:
            mod.weight, mod.bias = fuse_conv_bn_weights(mod.weight, mod.bias, mod.bn.running_mean, mod.bn.running_var, mod.bn.eps, mod.bn.weight, mod.bn.bias)
            return cls.get_qconv(mod, mod.activation_post_process, mod.weight_fake_quant)
        return super().from_float(mod, use_precomputed_fake_quant)


BNReLU2d = nnq._unsupported("BNReLU2d", nnq._FUSE_HINT)
