"""torch.ao.nn.quantized.dynamic for Zipp: the dynamically quantized Linear
and LSTM torch.ao.quantization.quantize_dynamic produces. Weights are
qint8 (per tensor or per channel); each call quantizes its float input
with the range it sees (reduce_range, as fbgemm/x86) and returns float, as
PyTorch 2.11's quantized::linear_dynamic. The LSTM projects the whole
input sequence of a layer at once and the hidden state step by step, then
runs PyTorch's float LSTM cell (sigmoid/tanh gates) on the results."""
import torch
import torch.nn as nn
import torch._quant as _q
import torch.ao.nn.quantized as nnq
import torch.ao.nn.intrinsic as nni
from torch.nn.utils.parametrize import type_before_parametrizations

__all__ = ["Linear", "LSTM", "GRU", "LSTMCell", "RNNCell", "GRUCell", "Conv1d", "Conv2d", "Conv3d", "ConvTranspose1d", "ConvTranspose2d",
           "ConvTranspose3d"]


def _linear_dynamic(x, w, b, relu=False):
    if w.__class__ is torch._QTensor:
        return _q.linear_dynamic(x, w, b, True, relu)
    # float16 weights (float16_dynamic_qconfig): the weight rounded to
    # half precision, the product in float32.
    y = torch.nn.functional.linear(x.float(), w.to(torch.float16).float(), b)
    return torch.relu(y) if relu else y


class Linear(nnq.Linear):
    _version = 4

    def __init__(self, in_features, out_features, bias_=True, dtype=torch.qint8):
        super().__init__(in_features, out_features, bias_, dtype=dtype)
        self.version = 4

    def forward(self, x):
        if self._packed_params.dtype not in (torch.qint8, torch.float16):
            raise RuntimeError("Unsupported dtype on dynamic quantized linear!")
        w, b = self._weight_bias()
        return _linear_dynamic(x, w, b).to(x.dtype)

    def _get_name(self):
        return "DynamicQuantizedLinear"

    def extra_repr(self):
        s = "in_features=%d, out_features=%d, dtype=%s" % (self.in_features, self.out_features, self._packed_params.dtype)
        if self._packed_params.dtype == torch.qint8:
            s += ", qscheme=%s" % (self.weight().qscheme(),)
        return s

    def _load_from_state_dict(self, state_dict, prefix, local_metadata, strict, missing_keys, unexpected_keys, error_msgs):
        self.version = local_metadata.get("version", None)
        super()._load_from_state_dict(state_dict, prefix, local_metadata, False, missing_keys, unexpected_keys, error_msgs)

    @classmethod
    def from_float(cls, mod, use_precomputed_fake_quant=False):
        float_modules = [torch.nn.Linear, torch.nn.NonDynamicallyQuantizableLinear, nni.LinearReLU]
        if type(mod) not in float_modules:
            raise AssertionError("nn.quantized.dynamic.Linear.from_float only works for one of" + str([m.__name__ for m in float_modules]) + ", got " + str(type(mod)))
        if not hasattr(mod, "qconfig"):
            raise AssertionError("Input float module must have qconfig defined")
        if type(mod) is nni.LinearReLU:
            mod = mod[0]
        if mod.qconfig is not None and mod.qconfig.weight is not None:
            weight_observer = mod.qconfig.weight()
        else:
            from torch.ao.quantization.qconfig import default_dynamic_qconfig
            weight_observer = default_dynamic_qconfig.weight()
        dtype = weight_observer.dtype
        if dtype not in (torch.qint8, torch.float16):
            raise AssertionError("The only supported dtypes for dynamic quantized linear are qint8 and float16, got: %s" % dtype)
        weight_observer(mod.weight)
        if dtype == torch.qint8:
            qweight = nnq._quantize_weight(mod.weight.float(), weight_observer)
        else:
            qweight = mod.weight.float().detach()
        qlinear = cls(mod.in_features, mod.out_features, dtype=dtype)
        qlinear.set_weight_bias(qweight, mod.bias)
        return qlinear

    @classmethod
    def from_reference(cls, ref_qlinear):
        qlinear = cls(ref_qlinear.in_features, ref_qlinear.out_features, dtype=ref_qlinear.weight_dtype)
        qlinear.set_weight_bias(ref_qlinear.get_quantized_weight(), ref_qlinear.bias)
        return qlinear


class PackedParameter(nn.Module):
    def __init__(self, param):
        super().__init__()
        self.param = param


class RNNBase(nn.Module):
    _FLOAT_MODULE = nn.RNNBase
    _version = 2

    def __init__(self, mode, input_size, hidden_size, num_layers=1, bias=True, batch_first=False, dropout=0.0, bidirectional=False, dtype=torch.qint8):
        super().__init__()
        self.mode = mode
        self.input_size = input_size
        self.hidden_size = hidden_size
        self.num_layers = num_layers
        self.bias = bias
        self.batch_first = batch_first
        self.dropout = float(dropout)
        self.bidirectional = bidirectional
        self.dtype = dtype
        self.version = 2
        self.training = False
        num_directions = 2 if bidirectional else 1
        if isinstance(dropout, bool) or not isinstance(dropout, (int, float)) or not 0 <= dropout <= 1:
            raise ValueError("dropout should be a number in range [0, 1] representing the probability of an element being zeroed")
        if mode == "LSTM":
            gate_size = 4 * hidden_size
        elif mode == "GRU":
            gate_size = 3 * hidden_size
        else:
            raise ValueError("Unrecognized RNN mode: " + mode)
        values = []
        for layer in range(num_layers):
            for _ in range(num_directions):
                layer_input_size = input_size if layer == 0 else hidden_size * num_directions
                # PyTorch starts from random weights (these draws keep the
                # random stream where PyTorch leaves it).
                w_ih = torch.randn(gate_size, layer_input_size)
                w_hh = torch.randn(gate_size, hidden_size)
                b_ih = torch.randn(gate_size)
                b_hh = torch.randn(gate_size)
                if dtype == torch.qint8:
                    w_ih = torch.quantize_per_tensor(w_ih, scale=0.1, zero_point=0, dtype=torch.qint8)
                    w_hh = torch.quantize_per_tensor(w_hh, scale=0.1, zero_point=0, dtype=torch.qint8)
                values.append(PackedParameter((w_ih, b_ih, w_hh, b_hh)))
        self._all_weight_values = nn.ModuleList(values)

    def _get_name(self):
        return "DynamicQuantizedRNN"

    def extra_repr(self):
        s = "{input_size}, {hidden_size}"
        if self.num_layers != 1:
            s += ", num_layers={num_layers}"
        if self.bias is not True:
            s += ", bias={bias}"
        if self.batch_first is not False:
            s += ", batch_first={batch_first}"
        if self.dropout != 0:
            s += ", dropout={dropout}"
        if self.bidirectional is not False:
            s += ", bidirectional={bidirectional}"
        return s.format(**self.__dict__)

    def __repr__(self):
        extra = self.extra_repr()
        return self._get_name() + "(" + extra + ")"

    def check_input(self, input, batch_sizes):
        expected_input_dim = 2 if batch_sizes is not None else 3
        if input.dim() != expected_input_dim:
            raise RuntimeError("input must have %d dimensions, got %d" % (expected_input_dim, input.dim()))
        if self.input_size != input.size(-1):
            raise RuntimeError("input.size(-1) must be equal to input_size. Expected %d, got %d" % (self.input_size, input.size(-1)))

    def get_expected_hidden_size(self, input, batch_sizes):
        mini_batch = input.size(0) if self.batch_first else input.size(1)
        num_directions = 2 if self.bidirectional else 1
        return (self.num_layers * num_directions, mini_batch, self.hidden_size)

    def check_hidden_size(self, hx, expected_hidden_size, msg="Expected hidden size {}, got {}"):
        if tuple(hx.size()) != tuple(expected_hidden_size):
            raise RuntimeError(msg.format(expected_hidden_size, list(hx.size())))

    def _weight_bias(self):
        weight_bias_dict = {"weight": {}, "bias": {}}
        count = 0
        num_directions = 2 if self.bidirectional else 1
        for layer in range(self.num_layers):
            for direction in range(num_directions):
                suffix = "_reverse" if direction == 1 else ""
                w_ih, b_ih, w_hh, b_hh = self._all_weight_values[count].param
                weight_bias_dict["weight"]["weight_ih_l%d%s" % (layer, suffix)] = w_ih
                weight_bias_dict["weight"]["weight_hh_l%d%s" % (layer, suffix)] = w_hh
                weight_bias_dict["bias"]["bias_ih_l%d%s" % (layer, suffix)] = b_ih
                weight_bias_dict["bias"]["bias_hh_l%d%s" % (layer, suffix)] = b_hh
                count += 1
        return weight_bias_dict

    def get_weight(self):
        return self._weight_bias()["weight"]

    def get_bias(self):
        return self._weight_bias()["bias"]

    def set_weight_bias(self, weight_bias_dict):
        num_directions = 2 if self.bidirectional else 1
        values = []
        for layer in range(self.num_layers):
            for direction in range(num_directions):
                suffix = "_reverse" if direction == 1 else ""
                values.append(PackedParameter((weight_bias_dict["weight_ih_l%d%s" % (layer, suffix)], weight_bias_dict["bias_ih_l%d%s" % (layer, suffix)],
                                               weight_bias_dict["weight_hh_l%d%s" % (layer, suffix)], weight_bias_dict["bias_hh_l%d%s" % (layer, suffix)])))
        self._all_weight_values = nn.ModuleList(values)

    @classmethod
    def from_float(cls, mod, use_precomputed_fake_quant=False):
        if type(mod) not in (torch.nn.LSTM, torch.nn.GRU):
            raise AssertionError("nn.quantized.dynamic.RNNBase.from_float only works for nn.LSTM and nn.GRU")
        if not hasattr(mod, "qconfig"):
            raise AssertionError("Input float module must have qconfig defined")
        if mod.qconfig is not None and mod.qconfig.weight is not None:
            weight_observer_method = mod.qconfig.weight
        else:
            from torch.ao.quantization.qconfig import default_dynamic_qconfig
            weight_observer_method = default_dynamic_qconfig.weight
        dtype = weight_observer_method().dtype
        if dtype not in (torch.qint8, torch.float16):
            raise RuntimeError("Unsupported dtype for dynamic RNN quantization: %s" % dtype)
        if type(mod) is torch.nn.GRU:
            raise NotImplementedError("dynamically quantized GRU is not supported on Zipp (LSTM and Linear are)")
        q = LSTM(mod.input_size, mod.hidden_size, mod.num_layers, mod.bias, mod.batch_first, mod.dropout, mod.bidirectional, dtype)
        if not mod.bias:
            raise AssertionError("mod.bias must be True")
        num_directions = 2 if mod.bidirectional else 1
        values = []
        for layer in range(q.num_layers):
            for direction in range(num_directions):
                suffix = "_reverse" if direction == 1 else ""
                w_ih = getattr(mod, "weight_ih_l%d%s" % (layer, suffix))
                b_ih = getattr(mod, "bias_ih_l%d%s" % (layer, suffix))
                w_hh = getattr(mod, "weight_hh_l%d%s" % (layer, suffix))
                b_hh = getattr(mod, "bias_hh_l%d%s" % (layer, suffix))
                if dtype == torch.qint8:
                    def quantize(w):
                        observer = weight_observer_method()
                        observer(w)
                        return nnq._quantize_weight(w.float(), observer)
                    w_ih, w_hh = quantize(w_ih), quantize(w_hh)
                else:
                    w_ih, w_hh = w_ih.float().detach(), w_hh.float().detach()
                values.append(PackedParameter((w_ih, b_ih.detach().float(), w_hh, b_hh.detach().float())))
        q._all_weight_values = nn.ModuleList(values)
        return q


class LSTM(RNNBase):
    _FLOAT_MODULE = nn.LSTM

    def __init__(self, *args, **kwargs):
        super().__init__("LSTM", *args, **kwargs)

    def _get_name(self):
        return "DynamicQuantizedLSTM"

    def forward(self, input, hx=None):
        if isinstance(input, torch.nn.utils.rnn.PackedSequence):
            raise NotImplementedError("a PackedSequence input to a dynamically quantized LSTM is not supported on Zipp")
        self.check_input(input, None)
        num_directions = 2 if self.bidirectional else 1
        max_batch_size = input.size(0) if self.batch_first else input.size(1)
        if hx is None:
            zeros = torch.zeros(self.num_layers * num_directions, max_batch_size, self.hidden_size, dtype=input.dtype)
            hx = (zeros, zeros)
        expected = self.get_expected_hidden_size(input, None)
        self.check_hidden_size(hx[0], expected, "Expected hidden[0] size {}, got {}")
        self.check_hidden_size(hx[1], expected, "Expected hidden[1] size {}, got {}")
        x = input.transpose(0, 1) if self.batch_first else input
        T = x.shape[0]
        H = self.hidden_size
        h_out, c_out = [], []
        layer_in = x
        idx = 0
        for layer in range(self.num_layers):
            outs = []
            for d in range(num_directions):
                w_ih, b_ih, w_hh, b_hh = self._all_weight_values[idx].param
                # the input projection of the whole sequence, one call
                pre = _linear_dynamic(layer_in, w_ih, b_ih)
                h = hx[0][idx]
                c = hx[1][idx]
                steps = range(T - 1, -1, -1) if d == 1 else range(T)
                seq = [None] * T
                for t in steps:
                    gates = _linear_dynamic(h, w_hh, b_hh) + pre[t]
                    i, f, g, o = gates.chunk(4, 1)
                    i = i.sigmoid()
                    f = f.sigmoid()
                    g = g.tanh()
                    o = o.sigmoid()
                    c = f * c + i * g
                    h = o * c.tanh()
                    seq[t] = h
                outs.append(torch.stack(seq) if T else layer_in.new_zeros((0, layer_in.shape[1], H)))
                h_out.append(h)
                c_out.append(c)
                idx += 1
            layer_in = torch.cat(outs, 2) if num_directions == 2 else outs[0]
        output = layer_in.transpose(0, 1) if self.batch_first else layer_in
        return output, (torch.stack(h_out), torch.stack(c_out))

    @classmethod
    def from_float(cls, mod, use_precomputed_fake_quant=False):
        return super().from_float(mod, use_precomputed_fake_quant)


def _unsupported(name):
    class _Unsupported(nn.Module):
        def __init__(self, *args, **kwargs):
            raise NotImplementedError("torch.ao.nn.quantized.dynamic.%s is not supported on Zipp (Linear and LSTM are)" % name)

        @classmethod
        def from_float(cls, mod, use_precomputed_fake_quant=False):
            raise NotImplementedError("dynamically quantized %s is not supported on Zipp (Linear and LSTM are)" % name)
    _Unsupported.__name__ = name
    _Unsupported.__qualname__ = name
    return _Unsupported


GRU = _unsupported("GRU")
LSTMCell = _unsupported("LSTMCell")
RNNCell = _unsupported("RNNCell")
GRUCell = _unsupported("GRUCell")
Conv1d = _unsupported("Conv1d")
Conv2d = _unsupported("Conv2d")
Conv3d = _unsupported("Conv3d")
ConvTranspose1d = _unsupported("ConvTranspose1d")
ConvTranspose2d = _unsupported("ConvTranspose2d")
ConvTranspose3d = _unsupported("ConvTranspose3d")


class _LinearReLU(Linear):
    """torch.ao.nn.intrinsic.quantized.dynamic.LinearReLU."""
    _FLOAT_MODULE = nni.LinearReLU

    def forward(self, x):
        w, b = self._weight_bias()
        return _linear_dynamic(x, w, b, relu=True).to(x.dtype)

    def _get_name(self):
        return "DynamicQuantizedLinearReLU"

    @classmethod
    def from_float(cls, mod, use_precomputed_fake_quant=False):
        return super().from_float(mod, use_precomputed_fake_quant)


_LinearReLU.__name__ = "LinearReLU"
_LinearReLU.__qualname__ = "LinearReLU"
