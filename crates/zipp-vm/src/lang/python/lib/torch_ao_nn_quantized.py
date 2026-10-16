"""torch.ao.nn.quantized for Zipp: the static quantized modules
torch.ao.quantization.convert produces (Linear, Conv1d, Conv2d, ReLU6,
Quantize, DeQuantize, Dropout, FloatFunctional/QFunctional), running the
integer kernels of torch._quant with PyTorch 2.11's requantization
arithmetic (see torch/_quant). Their state dicts use PyTorch's keys
(`_packed_params._packed_params` holding (weight, bias) for Linear;
`weight`/`bias`/`scale`/`zero_point` for convolutions), so checkpoints move
between the two."""
import torch
import torch.nn as nn
import torch._quant as _q
import torch.ao.nn.intrinsic as nni
import torch.ao.nn.intrinsic.qat as nniqat
from torch.nn.utils import fuse_conv_bn_weights, fuse_linear_bn_weights
from torch.nn.utils.parametrize import type_before_parametrizations

__all__ = ["BatchNorm2d", "BatchNorm3d", "Conv1d", "Conv2d", "Conv3d", "ConvTranspose1d", "ConvTranspose2d", "ConvTranspose3d",
           "DeQuantize", "ELU", "Embedding", "EmbeddingBag", "GroupNorm", "Hardswish", "InstanceNorm1d", "InstanceNorm2d",
           "InstanceNorm3d", "LayerNorm", "LeakyReLU", "Linear", "LSTM", "MultiheadAttention", "Quantize", "ReLU6", "Sigmoid",
           "Softmax", "Dropout", "PReLU", "FloatFunctional", "QFunctional"]


def _addindent(s, n):
    lines = s.split("\n")
    if len(lines) == 1:
        return s
    first = lines.pop(0)
    return first + "\n" + "\n".join((n * " ") + line for line in lines)


def _hide_packed_params_repr(self, params):
    extra_lines = []
    extra_repr = self.extra_repr()
    if extra_repr:
        extra_lines = extra_repr.split("\n")
    child_lines = []
    for key, module in self._modules.items():
        if isinstance(module, params):
            continue
        child_lines.append("(" + key + "): " + _addindent(repr(module), 2))
    lines = extra_lines + child_lines
    main_str = self._get_name() + "("
    if lines:
        if len(extra_lines) == 1 and not child_lines:
            main_str += extra_lines[0]
        else:
            main_str += "\n  " + "\n  ".join(lines) + "\n"
    return main_str + ")"


# ---- weight quantization (torch/ao/nn/quantized/modules/utils.py) --------------------------
def _get_weight_observer(observer):
    if hasattr(observer, "activation_post_process"):
        observer = observer.activation_post_process
    return observer


def _needs_weight_clamping(observer, dtype):
    observer = _get_weight_observer(observer)
    if dtype in (torch.qint8, torch.quint8, torch.qint32):
        lo, hi = _q._INFO[dtype.name][1:]
        return observer.quant_min > lo or observer.quant_max < hi
    return False


def _clamp_weights(qweight, observer, scale, zp):
    if not _needs_weight_clamping(observer, qweight.dtype):
        return qweight
    observer = _get_weight_observer(observer)
    q = torch.clamp(qweight.int_repr(), observer.quant_min, observer.quant_max).to(qweight._q.dtype)
    if observer.qscheme in (torch.per_tensor_symmetric, torch.per_tensor_affine):
        return torch._make_per_tensor_quantized_tensor(q, scale.item(), zp.item())
    return torch._make_per_channel_quantized_tensor(q, scale, zp, axis=observer.ch_axis)


def _quantize_weight(float_wt, observer):
    wt_scale, wt_zp = observer.calculate_qparams()
    if observer.qscheme in (torch.per_tensor_symmetric, torch.per_tensor_affine):
        qweight = torch.quantize_per_tensor(float_wt, float(wt_scale), int(wt_zp), torch.qint8)
        return _clamp_weights(qweight, observer, wt_scale, wt_zp)
    if observer.qscheme in (torch.per_channel_symmetric, torch.per_channel_affine):
        qweight = torch.quantize_per_channel(float_wt, wt_scale.to(torch.double), wt_zp.to(torch.int64), observer.ch_axis, torch.qint8)
        return _clamp_weights(qweight, observer, wt_scale, wt_zp)
    raise ValueError("Unexpected qscheme " + repr(observer.qscheme))


class WeightedQuantizedModule(nn.Module):
    @classmethod
    def from_reference(cls, ref_module, output_scale, output_zero_point):
        raise NotImplementedError


def _check_bias(b):
    if b is not None and not isinstance(b, torch.Tensor):
        raise TypeError("bias must be a Tensor or None")
    return None if b is None else b.detach().to(torch.float32)


# ---- Linear -------------------------------------------------------------------------------
class LinearPackedParams(nn.Module):
    """What PyTorch keeps as an opaque packed-weight object: here the
    quantized weight and float bias themselves."""
    _version = 3

    def __init__(self, dtype=torch.qint8):
        super().__init__()
        self.dtype = dtype
        if dtype == torch.qint8:
            wq = torch._empty_affine_quantized([1, 1], scale=1.0, zero_point=0, dtype=torch.qint8)
        elif dtype == torch.float16:
            wq = torch.zeros([1, 1], dtype=torch.float)
        else:
            raise RuntimeError("Unsupported dtype on dynamic quantized linear!")
        self.set_weight_bias(wq, None)

    def set_weight_bias(self, weight, bias):
        if self.dtype == torch.qint8:
            if weight.__class__ is not torch._QTensor or weight.dtype is not torch.qint8:
                raise RuntimeError("quantized::linear_prepack: expected a qint8 weight")
        self.__dict__["_packed_weight"] = weight
        self.__dict__["_packed_bias"] = _check_bias(bias)

    def _weight_bias(self):
        return self.__dict__["_packed_weight"], self.__dict__["_packed_bias"]

    @property
    def _packed_params(self):
        return self._weight_bias()

    def forward(self, x):
        return x

    def _save_to_state_dict(self, destination, prefix, keep_vars):
        super()._save_to_state_dict(destination, prefix, keep_vars)
        destination[prefix + "dtype"] = self.dtype
        destination[prefix + "_packed_params"] = self._weight_bias()

    def _load_from_state_dict(self, state_dict, prefix, local_metadata, strict, missing_keys, unexpected_keys, error_msgs):
        if prefix + "dtype" in state_dict:
            self.dtype = state_dict.pop(prefix + "dtype")
        if prefix + "_packed_params" in state_dict:
            weight, bias = state_dict.pop(prefix + "_packed_params")
            self.set_weight_bias(weight, bias)
        elif prefix + "weight" in state_dict:
            self.set_weight_bias(state_dict.pop(prefix + "weight"), state_dict.pop(prefix + "bias"))
        elif strict:
            missing_keys.append(prefix + "_packed_params")
        super()._load_from_state_dict(state_dict, prefix, local_metadata, False, missing_keys, unexpected_keys, error_msgs)

    def __repr__(self):
        return repr(self._weight_bias())


class Linear(WeightedQuantizedModule):
    _version = 3
    _FLOAT_MODULE = (nn.Linear, nn.NonDynamicallyQuantizableLinear)

    def __init__(self, in_features, out_features, bias_=True, dtype=torch.qint8):
        super().__init__()
        self.in_features = in_features
        self.out_features = out_features
        bias = torch.zeros(out_features, dtype=torch.float) if bias_ else None
        if dtype == torch.qint8:
            qweight = torch._empty_affine_quantized([out_features, in_features], scale=1, zero_point=0, dtype=torch.qint8)
        elif dtype == torch.float16:
            qweight = torch.zeros([out_features, in_features], dtype=torch.float)
        else:
            raise RuntimeError("Unsupported dtype specified for quantized Linear!")
        self._packed_params = LinearPackedParams(dtype)
        self._packed_params.set_weight_bias(qweight, bias)
        self.scale = 1.0
        self.zero_point = 0

    def _get_name(self):
        return "QuantizedLinear"

    def extra_repr(self):
        return "in_features=%d, out_features=%d, scale=%s, zero_point=%s, qscheme=%s" % (self.in_features, self.out_features, self.scale, self.zero_point, self.weight().qscheme())

    def __repr__(self):
        return _hide_packed_params_repr(self, LinearPackedParams)

    def forward(self, x):
        w, b = self._weight_bias()
        return _q.linear(x, w, b, self.scale, self.zero_point)

    def _save_to_state_dict(self, destination, prefix, keep_vars):
        super()._save_to_state_dict(destination, prefix, keep_vars)
        destination[prefix + "scale"] = torch.tensor(self.scale)
        destination[prefix + "zero_point"] = torch.tensor(self.zero_point)

    def _load_from_state_dict(self, state_dict, prefix, local_metadata, strict, missing_keys, unexpected_keys, error_msgs):
        self.scale = float(state_dict[prefix + "scale"])
        state_dict.pop(prefix + "scale")
        self.zero_point = int(state_dict[prefix + "zero_point"])
        state_dict.pop(prefix + "zero_point")
        if prefix + "weight" in state_dict:
            state_dict[prefix + "_packed_params.weight"] = state_dict.pop(prefix + "weight")
            state_dict[prefix + "_packed_params.bias"] = state_dict.pop(prefix + "bias")
        super()._load_from_state_dict(state_dict, prefix, local_metadata, False, missing_keys, unexpected_keys, error_msgs)

    def _weight_bias(self):
        return self._packed_params._weight_bias()

    def weight(self):
        return self._weight_bias()[0]

    def bias(self):
        return self._weight_bias()[1]

    def set_weight_bias(self, w, b):
        self._packed_params.set_weight_bias(w, b)

    @classmethod
    def from_float(cls, mod, use_precomputed_fake_quant=False):
        if hasattr(mod, "weight_fake_quant"):
            if type_before_parametrizations(mod) == getattr(nniqat, "LinearBn1d", None):
                mod.weight, mod.bias = fuse_linear_bn_weights(mod.weight, mod.bias, mod.bn.running_mean, mod.bn.running_var, mod.bn.eps, mod.bn.weight, mod.bn.bias)
            weight_post_process = mod.weight_fake_quant
            activation_post_process = mod.activation_post_process
        else:
            floats = cls._FLOAT_MODULE if isinstance(cls._FLOAT_MODULE, tuple) else (cls._FLOAT_MODULE,)
            if type_before_parametrizations(mod) not in floats:
                raise AssertionError("nnq.%s.from_float only works for %s, but got: %s" % (cls.__name__, ", ".join(f.__name__ for f in floats), type(mod)))
            if not hasattr(mod, "qconfig"):
                raise AssertionError("Input float module must have qconfig defined")
            activation_post_process = mod.activation_post_process
            if type_before_parametrizations(mod) == nni.LinearReLU:
                mod = mod[0]
            weight_post_process = mod.qconfig.weight() if not hasattr(mod, "weight_fake_quant") else mod.weight_fake_quant
        if not use_precomputed_fake_quant:
            weight_post_process(mod.weight)
        dtype = weight_post_process.dtype
        act_scale, act_zp = activation_post_process.calculate_qparams()
        if dtype != torch.qint8:
            raise AssertionError("Weight observer must have dtype torch.qint8, got %s" % dtype)
        qweight = _quantize_weight(mod.weight.float(), weight_post_process)
        qlinear = cls(mod.in_features, mod.out_features, dtype=dtype)
        qlinear.set_weight_bias(qweight, mod.bias)
        qlinear.scale = float(act_scale)
        qlinear.zero_point = int(act_zp)
        return qlinear

    @classmethod
    def from_reference(cls, ref_qlinear, output_scale, output_zero_point):
        qlinear = cls(ref_qlinear.in_features, ref_qlinear.out_features)
        qlinear.set_weight_bias(ref_qlinear.get_quantized_weight(), ref_qlinear.bias)
        qlinear.scale = float(output_scale)
        qlinear.zero_point = int(output_zero_point)
        return qlinear


# ---- convolutions -------------------------------------------------------------------------
def _ntuple(v, n):
    if isinstance(v, (tuple, list)):
        return tuple(v)
    return (v,) * n


class _ConvNd(WeightedQuantizedModule):
    _nd = 2

    def _init(self, in_channels, out_channels, kernel_size, stride, padding, dilation, transposed, output_padding, groups, bias, padding_mode="zeros"):
        nn.Module.__init__(self)
        if out_channels <= 0:
            raise ValueError("out_channels must be greater than 0, got %d" % out_channels)
        if in_channels % groups != 0:
            raise ValueError("in_channels must be divisible by groups")
        if out_channels % groups != 0:
            raise ValueError("out_channels must be divisible by groups")
        self.in_channels = in_channels
        self.out_channels = out_channels
        self.kernel_size = kernel_size
        self.stride = stride
        self.padding = padding
        self.dilation = dilation
        self.transposed = transposed
        self.output_padding = output_padding
        self.groups = groups
        if padding_mode not in ("zeros", "reflect"):
            raise ValueError("'padding_mode' %s is not supported by quantized convolution" % padding_mode)
        self.padding_mode = padding_mode
        weight_shape = [out_channels, in_channels // groups]
        qweight = torch._empty_affine_quantized(weight_shape + list(kernel_size), scale=1, zero_point=0, dtype=torch.qint8)
        bias_float = torch.zeros(out_channels, dtype=torch.float) if bias else None
        self.set_weight_bias(qweight, bias_float)
        self.scale = 1.0
        self.zero_point = 0

    def set_weight_bias(self, w, b):
        if w.__class__ is not torch._QTensor or w.dtype is not torch.qint8:
            raise RuntimeError("quantized::conv_prepack: expected a qint8 weight")
        self.__dict__["_packed_weight"] = w
        self.__dict__["_packed_bias"] = _check_bias(b)

    def _weight_bias(self):
        return self.__dict__["_packed_weight"], self.__dict__["_packed_bias"]

    def weight(self):
        return self._weight_bias()[0]

    def bias(self):
        return self._weight_bias()[1]

    def extra_repr(self):
        s = "{in_channels}, {out_channels}, kernel_size={kernel_size}, stride={stride}, scale={scale}, zero_point={zero_point}"
        if self.padding != (0,) * len(self.padding):
            s += ", padding={padding}"
        if self.dilation != (1,) * len(self.dilation):
            s += ", dilation={dilation}"
        if self.output_padding != (0,) * len(self.output_padding):
            s += ", output_padding={output_padding}"
        if self.groups != 1:
            s += ", groups={groups}"
        if self.bias() is None:
            s += ", bias=False"
        return s.format(**self.__dict__)

    def _save_to_state_dict(self, destination, prefix, keep_vars):
        super()._save_to_state_dict(destination, prefix, keep_vars)
        w, b = self._weight_bias()
        destination[prefix + "weight"] = w
        destination[prefix + "bias"] = b
        destination[prefix + "scale"] = torch.tensor(self.scale)
        destination[prefix + "zero_point"] = torch.tensor(self.zero_point)

    def _load_from_state_dict(self, state_dict, prefix, local_metadata, strict, missing_keys, unexpected_keys, error_msgs):
        self.set_weight_bias(state_dict[prefix + "weight"], state_dict[prefix + "bias"])
        state_dict.pop(prefix + "weight")
        state_dict.pop(prefix + "bias")
        self.scale = float(state_dict[prefix + "scale"])
        state_dict.pop(prefix + "scale")
        self.zero_point = int(state_dict[prefix + "zero_point"])
        state_dict.pop(prefix + "zero_point")
        super()._load_from_state_dict(state_dict, prefix, local_metadata, False, missing_keys, unexpected_keys, error_msgs)

    def __deepcopy__(self, memo):
        new = type(self).__new__(type(self))
        nn.Module.__init__(new)
        memo[id(self)] = new
        for k, v in self.__dict__.items():
            if k in ("_parameters", "_buffers", "_modules"):
                continue
            new.__dict__[k] = v
        w, b = self._weight_bias()
        new.__dict__["_packed_weight"] = w.clone()
        new.__dict__["_packed_bias"] = None if b is None else b.clone()
        new.training = self.training
        return new

    def __copy__(self):
        return self.__deepcopy__({})

    def _padded(self, input):
        if self.padding_mode != "zeros":
            rev = []
            for p in reversed(self.padding):
                rev += [p, p]
            return torch.nn.functional.pad(input, rev, mode=self.padding_mode), (0,) * self._nd
        return input, self.padding

    @classmethod
    def get_qconv(cls, mod, activation_post_process, weight_post_process=None):
        if weight_post_process is None:
            weight_post_process = mod.qconfig.weight()
        weight_post_process(mod.weight)
        if weight_post_process.dtype != torch.qint8:
            raise AssertionError("Weight observer must have a dtype of qint8, got %s" % weight_post_process.dtype)
        qweight = _quantize_weight(mod.weight.float(), weight_post_process)
        qconv = cls(mod.in_channels, mod.out_channels, mod.kernel_size, mod.stride, mod.padding, mod.dilation, mod.groups, mod.bias is not None, mod.padding_mode)
        qconv.set_weight_bias(qweight, mod.bias)
        if activation_post_process is None or activation_post_process.dtype == torch.float:
            return qconv
        act_scale, act_zp = activation_post_process.calculate_qparams()
        qconv.scale = float(act_scale)
        qconv.zero_point = int(act_zp)
        return qconv

    @classmethod
    def from_float(cls, mod, use_precomputed_fake_quant=False):
        if hasattr(mod, "weight_fake_quant"):
            if type(mod) is cls._NNIQAT_CONV_BN_MODULE or type(mod) is getattr(cls, "_NNIQAT_CONV_BN_RELU_MODULE", None):
                mod.weight, mod.bias = fuse_conv_bn_weights(mod.weight, mod.bias, mod.bn.running_mean, mod.bn.running_var, mod.bn.eps, mod.bn.weight, mod.bn.bias)
            if not hasattr(mod, "activation_post_process"):
                raise AssertionError("Input QAT module must have observer attached")
            weight_post_process = mod.weight_fake_quant
            activation_post_process = mod.activation_post_process
        else:
            if type(mod) is not cls._FLOAT_MODULE:
                raise AssertionError("nnq.%s.from_float only works for %s but got: %s" % (cls.__name__, cls._FLOAT_MODULE.__name__, type(mod).__name__))
            if not hasattr(mod, "qconfig"):
                raise AssertionError("Input float module must have qconfig defined.")
            activation_post_process = getattr(mod, "activation_post_process", None)
            if type(mod) is cls._NNI_CONV_RELU_MODULE:
                mod = mod[0]
            weight_post_process = mod.qconfig.weight()
        return cls.get_qconv(mod, activation_post_process, weight_post_process)

    @classmethod
    def from_reference(cls, ref_qconv, output_scale, output_zero_point):
        qconv = cls(ref_qconv.in_channels, ref_qconv.out_channels, ref_qconv.kernel_size, ref_qconv.stride, ref_qconv.padding, ref_qconv.dilation,
                    ref_qconv.groups, ref_qconv.bias is not None, ref_qconv.padding_mode)
        qconv.set_weight_bias(ref_qconv.get_quantized_weight(), ref_qconv.bias)
        qconv.scale = float(output_scale)
        qconv.zero_point = int(output_zero_point)
        return qconv


class Conv1d(_ConvNd):
    _nd = 1
    _FLOAT_MODULE = nn.Conv1d
    _NNIQAT_CONV_BN_MODULE = nniqat.ConvBn1d
    _NNIQAT_CONV_BN_RELU_MODULE = None
    _NNI_CONV_RELU_MODULE = nni.ConvReLU1d

    def __init__(self, in_channels, out_channels, kernel_size, stride=1, padding=0, dilation=1, groups=1, bias=True, padding_mode="zeros", device=None, dtype=None):
        padding = padding if isinstance(padding, str) else _ntuple(padding, 1)
        self._init(in_channels, out_channels, _ntuple(kernel_size, 1), _ntuple(stride, 1), padding, _ntuple(dilation, 1), False, (0,), groups, bias, padding_mode)

    def _get_name(self):
        return "QuantizedConv1d"

    def forward(self, input):
        if len(input.shape) != 3:
            raise ValueError("Input shape must be `(N, C, L)`!")
        x, padding = self._padded(input)
        w, b = self._weight_bias()
        return _q.conv1d(x, w, b, self.stride, padding, self.dilation, self.groups, self.scale, self.zero_point, relu=self._relu)

    _relu = False


class Conv2d(_ConvNd):
    _nd = 2
    _FLOAT_MODULE = nn.Conv2d
    _NNIQAT_CONV_BN_MODULE = nniqat.ConvBn2d
    _NNIQAT_CONV_BN_RELU_MODULE = None
    _NNI_CONV_RELU_MODULE = nni.ConvReLU2d

    def __init__(self, in_channels, out_channels, kernel_size, stride=1, padding=0, dilation=1, groups=1, bias=True, padding_mode="zeros", device=None, dtype=None):
        self._init(in_channels, out_channels, _ntuple(kernel_size, 2), _ntuple(stride, 2), _ntuple(padding, 2), _ntuple(dilation, 2), False, (0, 0), groups, bias, padding_mode)

    def _get_name(self):
        return "QuantizedConv2d"

    def forward(self, input):
        if len(input.shape) != 4:
            raise ValueError("Input shape must be `(N, C, H, W)`!")
        x, padding = self._padded(input)
        w, b = self._weight_bias()
        return _q.conv2d(x, w, b, self.stride, padding, self.dilation, self.groups, self.scale, self.zero_point, relu=self._relu)

    _relu = False


def _unsupported(name, why=""):
    class _Unsupported(nn.Module):
        def __init__(self, *args, **kwargs):
            raise NotImplementedError("torch.ao.nn.quantized.%s is not supported on Zipp%s" % (name, why))

        @classmethod
        def from_float(cls, mod, use_precomputed_fake_quant=False):
            raise NotImplementedError("quantized %s is not supported on Zipp%s" % (name, why))
    _Unsupported.__name__ = name
    _Unsupported.__qualname__ = name
    return _Unsupported


_FUSE_HINT = "; fuse it into the preceding convolution with torch.ao.quantization.fuse_modules, or keep it in float between a DeQuantStub and a QuantStub"
Conv3d = _unsupported("Conv3d")
ConvTranspose1d = _unsupported("ConvTranspose1d")
ConvTranspose2d = _unsupported("ConvTranspose2d")
ConvTranspose3d = _unsupported("ConvTranspose3d")
BatchNorm2d = _unsupported("BatchNorm2d", _FUSE_HINT)
BatchNorm3d = _unsupported("BatchNorm3d", _FUSE_HINT)
ELU = _unsupported("ELU")
Embedding = _unsupported("Embedding")
EmbeddingBag = _unsupported("EmbeddingBag")
GroupNorm = _unsupported("GroupNorm")
Hardswish = _unsupported("Hardswish")
InstanceNorm1d = _unsupported("InstanceNorm1d")
InstanceNorm2d = _unsupported("InstanceNorm2d")
InstanceNorm3d = _unsupported("InstanceNorm3d")
LayerNorm = _unsupported("LayerNorm")
LeakyReLU = _unsupported("LeakyReLU")
LSTM = _unsupported("LSTM", " (use torch.ao.quantization.quantize_dynamic for a dynamically quantized LSTM)")
MultiheadAttention = _unsupported("MultiheadAttention")
PReLU = _unsupported("PReLU")
Sigmoid = _unsupported("Sigmoid")
Softmax = _unsupported("Softmax")


# ---- activations, stubs, dropout ------------------------------------------------------------
class ReLU6(nn.ReLU):
    def __init__(self, inplace=False):
        super().__init__(inplace)
        self.inplace = inplace

    def forward(self, input):
        return _q._hardtanh(input, 0.0, 6.0, self.inplace)

    def _get_name(self):
        return "QuantizedReLU6"

    @staticmethod
    def from_float(mod, use_precomputed_fake_quant=False):
        return ReLU6(mod.inplace)


class Dropout(nn.Dropout):
    def forward(self, input):
        return input

    def _get_name(self):
        return "QuantizedDropout"

    @classmethod
    def from_float(cls, mod, use_precomputed_fake_quant=False):
        return cls(mod.p, mod.inplace)

    @classmethod
    def from_reference(cls, mod, scale, zero_point):
        return cls(mod.p, mod.inplace)


class Quantize(nn.Module):
    def __init__(self, scale, zero_point, dtype, factory_kwargs=None):
        super().__init__()
        self.register_buffer("scale", torch.tensor([scale]))
        self.register_buffer("zero_point", torch.tensor([zero_point], dtype=torch.long))
        self.dtype = dtype

    def forward(self, X):
        return torch.quantize_per_tensor(X, float(self.scale), int(self.zero_point), self.dtype)

    @staticmethod
    def from_float(mod, use_precomputed_fake_quant=False):
        if not hasattr(mod, "activation_post_process"):
            raise AssertionError("Module %s must have activation_post_process attribute" % type(mod).__name__)
        scale, zero_point = mod.activation_post_process.calculate_qparams()
        return Quantize(scale.float().item(), zero_point.long().item(), mod.activation_post_process.dtype)

    def extra_repr(self):
        return "scale=%s, zero_point=%s, dtype=%s" % (self.scale, self.zero_point, self.dtype)


class DeQuantize(nn.Module):
    def forward(self, Xq):
        return Xq.dequantize()

    @staticmethod
    def from_float(mod, use_precomputed_fake_quant=False):
        return DeQuantize()


# ---- functional modules -----------------------------------------------------------------
class FloatFunctional(nn.Module):
    """Float add/mul/cat whose output an observer watches, so that convert
    can swap in QFunctional with the observed scale and zero point."""

    def __init__(self):
        super().__init__()
        self.activation_post_process = nn.Identity()

    def forward(self, x):
        raise RuntimeError("FloatFunctional is not intended to use the 'forward'. Please use the underlying operation")

    def add(self, x, y):
        return self.activation_post_process(torch.add(x, y))

    def add_scalar(self, x, y):
        return torch.add(x, y)

    def mul(self, x, y):
        return self.activation_post_process(torch.mul(x, y))

    def mul_scalar(self, x, y):
        return torch.mul(x, y)

    def cat(self, x, dim=0):
        return self.activation_post_process(torch.cat(x, dim=dim))

    def add_relu(self, x, y):
        return self.activation_post_process(torch.nn.functional.relu(torch.add(x, y)))

    def matmul(self, x, y):
        return self.activation_post_process(torch.matmul(x, y))


class QFunctional(nn.Module):
    """Quantized add/mul/cat with an output scale and zero point
    (torch.ops.quantized.add/mul/cat)."""

    def __init__(self):
        super().__init__()
        self.scale = 1.0
        self.zero_point = 0
        self.activation_post_process = nn.Identity()

    def _save_to_state_dict(self, destination, prefix, keep_vars):
        super()._save_to_state_dict(destination, prefix, keep_vars)
        destination[prefix + "scale"] = torch.tensor(self.scale)
        destination[prefix + "zero_point"] = torch.tensor(self.zero_point)

    def _load_from_state_dict(self, state_dict, prefix, local_metadata, strict, missing_keys, unexpected_keys, error_msgs):
        self.scale = float(state_dict.pop(prefix + "scale"))
        self.zero_point = int(state_dict.pop(prefix + "zero_point"))
        super()._load_from_state_dict(state_dict, prefix, local_metadata, False, missing_keys, unexpected_keys, error_msgs)

    def _get_name(self):
        return "QFunctional"

    def extra_repr(self):
        return "scale=%s, zero_point=%s" % (self.scale, self.zero_point)

    def forward(self, x):
        raise RuntimeError("Functional is not intended to use the 'forward'. Please use the underlying operation")

    def add(self, x, y):
        return _q.qadd(x, y, self.scale, self.zero_point)

    def add_scalar(self, x, y):
        return _q.qadd_scalar(x, y)

    def mul(self, x, y):
        return _q.qmul(x, y, self.scale, self.zero_point)

    def mul_scalar(self, x, y):
        return _q.qmul_scalar(x, y)

    def cat(self, x, dim=0):
        return _q.qcat(x, dim, self.scale, self.zero_point)

    def add_relu(self, x, y):
        return _q.qadd(x, y, self.scale, self.zero_point, relu=True)

    def matmul(self, x, y):
        raise NotImplementedError("QFunctional.matmul is not supported on Zipp")

    @classmethod
    def from_float(cls, mod, use_precomputed_fake_quant=False):
        if type(mod) is not FloatFunctional:
            raise AssertionError("QFunctional.from_float expects an instance of FloatFunctional")
        scale, zero_point = mod.activation_post_process.calculate_qparams()
        new_mod = QFunctional()
        new_mod.scale = float(scale)
        new_mod.zero_point = int(zero_point)
        return new_mod


dynamic = _q._LazyModule("torch.ao.nn.quantized.dynamic")
functional = _q._LazyModule("torch.ao.nn.quantized.functional")
