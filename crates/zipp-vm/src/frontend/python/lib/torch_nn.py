"""torch.nn for Zipp: Module, Parameter and the layers the runtime supports."""
import math
import torch
from torch import Tensor
from collections import OrderedDict
import torch.nn.functional as F
import torch.nn.init as init
import torch.nn.utils as utils


class Parameter(Tensor):
    def __init__(self, data=None, requires_grad=True):
        if data is None:
            data = torch.zeros(0)
        Tensor.__init__(self, data._s, data.shape, data.dtype, requires_grad)

    def __repr__(self):
        return "Parameter containing:\n" + Tensor.__repr__(self)


class Module:
    def __init__(self):
        object.__setattr__(self, "_parameters", OrderedDict())
        object.__setattr__(self, "_modules", OrderedDict())
        object.__setattr__(self, "_buffers", OrderedDict())
        object.__setattr__(self, "training", True)

    def __setattr__(self, name, value):
        params = self.__dict__.get("_parameters")
        if params is None:
            raise AttributeError("cannot assign before Module.__init__() call")
        if isinstance(value, Parameter):
            self._modules.pop(name, None)
            params[name] = value
        elif isinstance(value, Module):
            params.pop(name, None)
            self._modules[name] = value
        else:
            if name in params:
                if value is None:
                    params[name] = None
                    return
                raise TypeError("cannot assign '%s' as parameter '%s' (torch.nn.Parameter or None expected)" % (type(value).__name__, name))
            if name in self._modules and value is not None and not isinstance(value, Module):
                raise TypeError("cannot assign '%s' as child module '%s'" % (type(value).__name__, name))
            object.__setattr__(self, name, value)

    def __getattr__(self, name):
        d = self.__dict__
        if "_parameters" in d and name in d["_parameters"]:
            return d["_parameters"][name]
        if "_modules" in d and name in d["_modules"]:
            return d["_modules"][name]
        if "_buffers" in d and name in d["_buffers"]:
            return d["_buffers"][name]
        raise AttributeError("'%s' object has no attribute '%s'" % (type(self).__name__, name))

    def __delattr__(self, name):
        if name in self._parameters:
            del self._parameters[name]
        elif name in self._modules:
            del self._modules[name]
        else:
            object.__delattr__(self, name)

    def register_parameter(self, name, param):
        self._parameters[name] = param

    def register_buffer(self, name, tensor, persistent=True):
        self._buffers[name] = tensor

    def add_module(self, name, module):
        self._modules[name] = module

    def forward(self, *args, **kwargs):
        raise NotImplementedError

    def __call__(self, *args, **kwargs):
        return self.forward(*args, **kwargs)

    def parameters(self, recurse=True):
        for _, p in self.named_parameters(recurse=recurse):
            yield p

    def named_parameters(self, prefix="", recurse=True):
        seen = set()
        for name, p in self._parameters.items():
            if p is not None and id(p) not in seen:
                seen.add(id(p))
                yield (prefix + name, p)
        if recurse:
            for mname, m in self._modules.items():
                for name, p in m.named_parameters(prefix + mname + "."):
                    if id(p) not in seen:
                        seen.add(id(p))
                        yield (name, p)

    def buffers(self):
        for _, b in self.named_buffers():
            yield b

    def named_buffers(self, prefix=""):
        for name, b in self._buffers.items():
            yield (prefix + name, b)
        for mname, m in self._modules.items():
            for name, b in m.named_buffers(prefix + mname + "."):
                yield (name, b)

    def children(self):
        return iter(list(self._modules.values()))

    def named_children(self):
        return iter(list(self._modules.items()))

    def modules(self):
        yield self
        for m in self._modules.values():
            for sub in m.modules():
                yield sub

    def named_modules(self, prefix=""):
        yield (prefix, self)
        for name, m in self._modules.items():
            for sub_name, sub in m.named_modules(prefix + name + "." if prefix else name):
                yield (sub_name if prefix else (name if sub is m else sub_name), sub)

    def train(self, mode=True):
        object.__setattr__(self, "training", bool(mode))
        for m in self._modules.values():
            m.train(mode)
        return self

    def eval(self):
        return self.train(False)

    def requires_grad_(self, requires_grad=True):
        for p in self.parameters():
            p.requires_grad_(requires_grad)
        return self

    def zero_grad(self, set_to_none=True):
        for p in self.parameters():
            if set_to_none:
                p.grad = None
            elif p.grad is not None:
                p.grad.zero_()

    def to(self, *args, **kwargs):
        torch._check_cpu_device(kwargs.get("device"))
        target = kwargs.get("dtype")
        for a in args:
            if isinstance(a, (str, torch.device)):
                torch._check_cpu_device(a)
            if isinstance(a, torch.dtype):
                target = a
        if target is not None:
            for p in self.parameters():
                p._s = p.to(target)._s
                p.dtype = target
        return self

    def cpu(self):
        return self

    def cuda(self, *a):
        raise RuntimeError("CUDA is not available on Zipp")

    def float(self):
        return self.to(torch.float32)

    def double(self):
        return self.to(torch.float64)

    def apply(self, fn):
        for m in self._modules.values():
            m.apply(fn)
        fn(self)
        return self

    def state_dict(self, destination=None, prefix="", keep_vars=False):
        out = OrderedDict() if destination is None else destination
        for name, p in self._parameters.items():
            if p is not None:
                out[prefix + name] = p if keep_vars else p.detach()
        for name, b in self._buffers.items():
            if b is not None:
                out[prefix + name] = b if keep_vars else b.detach()
        for name, m in self._modules.items():
            m.state_dict(out, prefix + name + ".")
        return out

    def load_state_dict(self, state_dict, strict=True):
        own = self.state_dict(keep_vars=True)
        missing = [k for k in own if k not in state_dict]
        unexpected = [k for k in state_dict if k not in own]
        if strict and (missing or unexpected):
            raise RuntimeError("Error(s) in loading state_dict for %s:%s%s" % (
                type(self).__name__,
                ("\n\tMissing key(s) in state_dict: " + ", ".join(repr(k) for k in missing)) if missing else "",
                ("\n\tUnexpected key(s) in state_dict: " + ", ".join(repr(k) for k in unexpected)) if unexpected else ""))
        for k, target in own.items():
            if k in state_dict:
                value = state_dict[k]
                if tuple(value.shape) != tuple(target.shape):
                    raise RuntimeError("size mismatch for %s: copying a param with shape %s from checkpoint, the shape in current model is %s" % (k, tuple(value.shape), tuple(target.shape)))
                with torch.no_grad():
                    target.copy_(value)
        return _IncompatibleKeys(missing, unexpected)

    def extra_repr(self):
        return ""

    def __repr__(self):
        lines = []
        for name, m in self._modules.items():
            body = repr(m).replace("\n", "\n  ")
            lines.append("  (%s): %s" % (name, body))
        extra = self.extra_repr()
        if not lines:
            return "%s(%s)" % (type(self).__name__, extra)
        return "%s(%s\n%s\n)" % (type(self).__name__, extra, "\n".join(lines))


class _IncompatibleKeys:
    def __init__(self, missing, unexpected):
        self.missing_keys = missing
        self.unexpected_keys = unexpected

    def __repr__(self):
        return "<All keys matched successfully>" if not self.missing_keys and not self.unexpected_keys else "_IncompatibleKeys(missing_keys=%r, unexpected_keys=%r)" % (self.missing_keys, self.unexpected_keys)


class Sequential(Module):
    def __init__(self, *layers):
        super().__init__()
        if len(layers) == 1 and isinstance(layers[0], OrderedDict):
            for name, layer in layers[0].items():
                self.add_module(name, layer)
        else:
            for i, layer in enumerate(layers):
                self.add_module(str(i), layer)

    def forward(self, x):
        for layer in self._modules.values():
            x = layer(x)
        return x

    def __len__(self):
        return len(self._modules)

    def __getitem__(self, i):
        return list(self._modules.values())[i]

    def __iter__(self):
        return iter(list(self._modules.values()))

    def append(self, layer):
        self.add_module(str(len(self._modules)), layer)
        return self


class ModuleList(Module):
    def __init__(self, modules=None):
        super().__init__()
        for m in modules or []:
            self.append(m)

    def append(self, m):
        self.add_module(str(len(self._modules)), m)
        return self

    def __len__(self):
        return len(self._modules)

    def __getitem__(self, i):
        return list(self._modules.values())[i]

    def __iter__(self):
        return iter(list(self._modules.values()))


class ParameterList(Module):
    def __init__(self, params=None):
        super().__init__()
        for p in params or []:
            self.append(p)

    def append(self, p):
        self.register_parameter(str(len(self._parameters)), p)
        return self

    def __len__(self):
        return len(self._parameters)

    def __getitem__(self, i):
        return list(self._parameters.values())[i]

    def __iter__(self):
        return iter(list(self._parameters.values()))


class Identity(Module):
    def forward(self, x):
        return x


class Linear(Module):
    def __init__(self, in_features, out_features, bias=True, device=None, dtype=None):
        torch._check_cpu_device(device)
        super().__init__()
        self.in_features = in_features
        self.out_features = out_features
        self.weight = Parameter(torch.empty(out_features, in_features, dtype=dtype))
        self.bias = Parameter(torch.empty(out_features, dtype=dtype)) if bias else None
        self.reset_parameters()

    def reset_parameters(self):
        init.kaiming_uniform_(self.weight, a=math.sqrt(5))
        if self.bias is not None:
            bound = 1 / math.sqrt(self.in_features) if self.in_features > 0 else 0
            init.uniform_(self.bias, -bound, bound)

    def forward(self, x):
        return F.linear(x, self.weight, self.bias)

    def extra_repr(self):
        return "in_features=%d, out_features=%d, bias=%s" % (self.in_features, self.out_features, self.bias is not None)


class Embedding(Module):
    def __init__(self, num_embeddings, embedding_dim, padding_idx=None, device=None, dtype=None):
        torch._check_cpu_device(device)
        super().__init__()
        self.num_embeddings = num_embeddings
        self.embedding_dim = embedding_dim
        self.padding_idx = padding_idx
        self.weight = Parameter(torch.empty(num_embeddings, embedding_dim, dtype=dtype))
        init.normal_(self.weight)
        if padding_idx is not None:
            with torch.no_grad():
                self.weight[padding_idx] = 0

    def forward(self, idx):
        return F.embedding(idx, self.weight)

    def extra_repr(self):
        return "%d, %d" % (self.num_embeddings, self.embedding_dim)


class Conv1d(Module):
    def __init__(self, in_channels, out_channels, kernel_size, stride=1, padding=0, dilation=1, groups=1, bias=True, device=None, dtype=None):
        torch._check_cpu_device(device)
        super().__init__()
        if stride != 1 or dilation != 1 or groups != 1:
            raise NotImplementedError("Conv1d on Zipp supports stride=1, dilation=1, groups=1")
        self.in_channels = in_channels
        self.out_channels = out_channels
        self.kernel_size = (kernel_size,) if isinstance(kernel_size, int) else tuple(kernel_size)
        self.padding = (padding,) if isinstance(padding, int) else tuple(padding)
        self.stride = (1,)
        self.weight = Parameter(torch.empty(out_channels, in_channels, self.kernel_size[0], dtype=dtype))
        self.bias = Parameter(torch.empty(out_channels, dtype=dtype)) if bias else None
        self.reset_parameters()

    def reset_parameters(self):
        init.kaiming_uniform_(self.weight, a=math.sqrt(5))
        if self.bias is not None:
            fan_in = self.in_channels * self.kernel_size[0]
            bound = 1 / math.sqrt(fan_in) if fan_in > 0 else 0
            init.uniform_(self.bias, -bound, bound)

    def forward(self, x):
        return F.conv1d(x, self.weight, self.bias, padding=self.padding[0])

    def extra_repr(self):
        return "%d, %d, kernel_size=%s, stride=%s" % (self.in_channels, self.out_channels, self.kernel_size, self.stride)


class Conv2d(Module):
    def __init__(self, in_channels, out_channels, kernel_size, stride=1, padding=0, dilation=1, groups=1, bias=True, padding_mode="zeros", device=None, dtype=None):
        torch._check_cpu_device(device)
        super().__init__()
        if any(isinstance(v, bool) or not isinstance(v, int) or v < 1 for v in (in_channels, out_channels, groups)):
            raise ValueError("channels and groups must be positive integers")
        if in_channels % groups or out_channels % groups:
            raise ValueError("in_channels and out_channels must be divisible by groups")
        if padding_mode != "zeros":
            raise NotImplementedError("Conv2d supports zero padding only")
        if dtype is not None and dtype not in (torch.float32, torch.float64):
            raise TypeError("Conv2d requires float32 or float64")
        self.in_channels, self.out_channels, self.groups = in_channels, out_channels, groups
        self.kernel_size = F._conv_pair(kernel_size, "kernel_size")
        self.stride = F._conv_pair(stride, "stride")
        self.padding = F._conv_pair(padding, "padding", 0)
        self.dilation = F._conv_pair(dilation, "dilation")
        self.padding_mode = padding_mode
        self.weight = Parameter(torch.empty(out_channels, in_channels // groups, *self.kernel_size, dtype=dtype))
        self.bias = Parameter(torch.empty(out_channels, dtype=dtype)) if bias else None
        self.reset_parameters()

    def reset_parameters(self):
        init.kaiming_uniform_(self.weight, a=math.sqrt(5))
        if self.bias is not None:
            fan_in = self.in_channels // self.groups * self.kernel_size[0] * self.kernel_size[1]
            bound = 1 / math.sqrt(fan_in)
            init.uniform_(self.bias, -bound, bound)

    def forward(self, x):
        return F.conv2d(x, self.weight, self.bias, self.stride, self.padding, self.dilation, self.groups)

    def extra_repr(self):
        return "%d, %d, kernel_size=%s, stride=%s, padding=%s, dilation=%s, groups=%d" % (self.in_channels, self.out_channels, self.kernel_size, self.stride, self.padding, self.dilation, self.groups)


class GRUCell(Module):
    def __init__(self, input_size, hidden_size, bias=True, device=None, dtype=None):
        torch._check_cpu_device(device)
        super().__init__()
        self.input_size = input_size
        self.hidden_size = hidden_size
        self.weight_ih = Parameter(torch.empty(3 * hidden_size, input_size, dtype=dtype))
        self.weight_hh = Parameter(torch.empty(3 * hidden_size, hidden_size, dtype=dtype))
        self.bias_ih = Parameter(torch.empty(3 * hidden_size, dtype=dtype)) if bias else None
        self.bias_hh = Parameter(torch.empty(3 * hidden_size, dtype=dtype)) if bias else None
        self.reset_parameters()

    def reset_parameters(self):
        stdv = 1.0 / math.sqrt(self.hidden_size) if self.hidden_size > 0 else 0
        for p in self.parameters():
            init.uniform_(p, -stdv, stdv)

    def forward(self, x, h=None):
        if h is None:
            h = x.new_zeros(x.shape[0], self.hidden_size)
        gi = F.linear(x, self.weight_ih, self.bias_ih)
        gh = F.linear(h, self.weight_hh, self.bias_hh)
        i_r, i_z, i_n = gi.chunk(3, 1)
        h_r, h_z, h_n = gh.chunk(3, 1)
        r = torch.sigmoid(i_r + h_r)
        z = torch.sigmoid(i_z + h_z)
        n = torch.tanh(i_n + r * h_n)
        return (1 - z) * n + z * h


class LSTMCell(Module):
    def __init__(self, input_size, hidden_size, bias=True):
        super().__init__()
        self.input_size = input_size
        self.hidden_size = hidden_size
        self.weight_ih = Parameter(torch.empty(4 * hidden_size, input_size))
        self.weight_hh = Parameter(torch.empty(4 * hidden_size, hidden_size))
        self.bias_ih = Parameter(torch.empty(4 * hidden_size)) if bias else None
        self.bias_hh = Parameter(torch.empty(4 * hidden_size)) if bias else None
        stdv = 1.0 / math.sqrt(hidden_size)
        for p in self.parameters():
            init.uniform_(p, -stdv, stdv)

    def forward(self, x, state=None):
        if state is None:
            h = x.new_zeros(x.shape[0], self.hidden_size)
            c = x.new_zeros(x.shape[0], self.hidden_size)
        else:
            h, c = state
        gates = F.linear(x, self.weight_ih, self.bias_ih) + F.linear(h, self.weight_hh, self.bias_hh)
        i, f, g, o = gates.chunk(4, 1)
        c2 = torch.sigmoid(f) * c + torch.sigmoid(i) * torch.tanh(g)
        h2 = torch.sigmoid(o) * torch.tanh(c2)
        return h2, c2


class LayerNorm(Module):
    def __init__(self, normalized_shape, eps=1e-5, elementwise_affine=True):
        super().__init__()
        shape = (normalized_shape,) if isinstance(normalized_shape, int) else tuple(normalized_shape)
        self.normalized_shape = shape
        self.eps = eps
        self.weight = Parameter(torch.ones(*shape)) if elementwise_affine else None
        self.bias = Parameter(torch.zeros(*shape)) if elementwise_affine else None

    def forward(self, x):
        return F.layer_norm(x, self.normalized_shape, self.weight, self.bias, self.eps)


class Dropout(Module):
    def __init__(self, p=0.5):
        super().__init__()
        self.p = p

    def forward(self, x):
        return F.dropout(x, self.p, self.training)


class _Activation(Module):
    _fn = None

    def forward(self, x):
        return type(self)._fn(x)


class SiLU(_Activation):
    _fn = staticmethod(F.silu)


class ReLU(_Activation):
    _fn = staticmethod(F.relu)


class GELU(Module):
    def __init__(self, approximate="none"):
        super().__init__()
        self.approximate = approximate

    def forward(self, x):
        return F.gelu(x, self.approximate)


class Tanh(_Activation):
    _fn = staticmethod(torch.tanh)


class Sigmoid(_Activation):
    _fn = staticmethod(torch.sigmoid)


class Softmax(Module):
    def __init__(self, dim=-1):
        super().__init__()
        self.dim = dim

    def forward(self, x):
        return F.softmax(x, self.dim)


class LogSoftmax(Softmax):
    def forward(self, x):
        return F.log_softmax(x, self.dim)


class Flatten(Module):
    def __init__(self, start_dim=1, end_dim=-1):
        super().__init__()
        self.start_dim = start_dim
        self.end_dim = end_dim

    def forward(self, x):
        return x.flatten(self.start_dim, self.end_dim)


class MSELoss(Module):
    def forward(self, a, b):
        return F.mse_loss(a, b)


class CrossEntropyLoss(Module):
    def forward(self, logits, target):
        return F.cross_entropy(logits, target)


class BCEWithLogitsLoss(Module):
    def forward(self, logits, target):
        return F.binary_cross_entropy_with_logits(logits, target)


class L1Loss(Module):
    def forward(self, a, b):
        return (a - b).abs().mean()


functional = F
