"""Quantized tensors for Zipp (torch.quint8, torch.qint8, torch.qint32),
following PyTorch 2.11's CPU semantics: torch.quantize_per_tensor,
quantize_per_channel and quantize_per_tensor_dynamic, dequantize, the
Tensor.q_* accessors, printing, the shape operations PyTorch runs on a
quantized tensor, fake quantization with its straight-through gradients,
and the integer kernels of the quantized Linear/Conv modules
(torch.ao.nn.quantized) with PyTorch's requantization arithmetic.

A quantized tensor is a `torch._QTensor` (a Tensor subclass named Tensor)
holding its integers in an ordinary uint8/int8/int32 tensor `_q`, its
scheme `_qs`, and either `_scale` (a Python float, PyTorch's double) and
`_zp` (int), or per-channel `_scales` (float64), `_zps` (int64) and
`_axis`. The loops are `_zipp_tensor` kernels (`q_*`) with native twins.

Numerics (checked against PyTorch 2.11 on x86):
- quantize: clamp(nearbyint(x * float(1 / float(scale))) + zero_point);
- dequantize: float(scale) * (q - zero_point);
- Linear (fbgemm, also the x86 engine's): the exact int32 accumulator,
  then nearbyint(float(float(acc) + bias / atw) * multiplier) + zp with
  atw = float(x_scale) * float(w_scale) and multiplier = atw / y_scale;
- Conv under the x86 engine (oneDNN on Linux with AVX512-VNNI for
  symmetric weights and at most 100 groups) and everything under onednn:
  the same float value, the zero point added in float before rounding;
- dynamic Linear: fma(float(acc), float(x_scale) * float(w_scale), bias)
  after quantizing the input with ChooseQuantizationParams (reduce_range).
"""
import builtins as _b
import torch
import _zipp_tensor as _k

_int, _float, _bool, _len, _range, _tuple, _list, _isinstance = _b.int, _b.float, _b.bool, _b.len, _b.range, _b.tuple, _b.list, _b.isinstance
_T = torch.Tensor
_Q = torch._QTensor
_Size = torch.Size
_onew = object.__new__
_fround = _k.fround

# quantized dtype -> (storage dtype, qmin, qmax)
_INFO = {"quint8": (torch.uint8, 0, 255), "qint8": (torch.int8, -128, 127), "qint32": (torch.int32, -2 ** 31, 2 ** 31 - 1)}
_OF_INT = {"uint8": torch.quint8, "int8": torch.qint8, "int32": torch.qint32}
_SCALAR = {"quint8": "QUInt8", "qint8": "QInt8", "qint32": "QInt32", "quint4x2": "QUInt4x2"}


def _scalar_name(dt):
    n = dt.name
    return _SCALAR.get(n) or torch._CAST_NAME.get(n, n)


def _no_kernel(op):
    return NotImplementedError("Could not run '%s' with arguments from the 'QuantizedCPU' backend. This could be because the operator doesn't exist for this backend, or was omitted during the selective/custom build process (if using custom build)." % op)


# ---- construction ------------------------------------------------------------------------
def _dense(storage, shape, dt):
    return torch._new3(storage, shape if shape.__class__ is _Size else _Size(shape), dt)


def _plain(t):
    return t if t._node is None and not t.requires_grad else _T(t._s, t.shape, t.dtype)


def _make_pt(q, dt, scale, zp, scheme=None):
    t = _onew(_Q)
    t._q = q
    t.shape = q.shape
    t.dtype = dt
    t._qs = torch.per_tensor_affine if scheme is None else scheme
    t._scale = _float(scale)
    t._zp = _int(zp)
    return t


def _make_pc(q, dt, scales, zps, axis):
    t = _onew(_Q)
    t._q = q
    t.shape = q.shape
    t.dtype = dt
    t._qs = torch.per_channel_affine
    t._scales = scales
    t._zps = zps
    t._axis = axis
    return t


def _same_params(t, q):
    """A quantized tensor with t's quantization over the integers q."""
    if t._qs is torch.per_channel_affine:
        return _make_pc(q, t.dtype, t._scales, t._zps, t._axis)
    return _make_pt(q, t.dtype, t._scale, t._zp, t._qs)


def _per_channel(t):
    return t._qs is torch.per_channel_affine


def _layout(shape, axis):
    outer = 1
    for d in shape[:axis]:
        outer *= d
    inner = 1
    for d in shape[axis + 1:]:
        inner *= d
    return outer, inner


def _check_qdtype(dt):
    if not _isinstance(dt, torch.dtype) or dt.name not in _INFO:
        if _isinstance(dt, torch.dtype) and dt.name == "quint4x2":
            raise NotImplementedError("torch.quint4x2 tensors are not supported on Zipp (quint8, qint8 and qint32 are)")
        raise RuntimeError("ScalarType %s is not supported in new_qtensor." % (_scalar_name(dt) if _isinstance(dt, torch.dtype) else dt))
    return _INFO[dt.name]


def _check_float_input(x):
    if not _isinstance(x, _T) or x.__class__ is _Q:
        raise TypeError("quantize: expected a float Tensor")
    if x.dtype is not torch.float32:
        raise RuntimeError("Quantize only works on Float Tensor, got %s" % _scalar_name(x.dtype))


def _scalar(v, conv):
    if _isinstance(v, _T):
        return conv(v.item())
    return conv(v)


def _quantize_pt(x, scale, zp, dt):
    under, qmin, qmax = _check_qdtype(dt)
    if zp > qmax:
        raise RuntimeError("quantize_tensor_per_tensor_affine zero_point %d is above upper bound." % zp)
    if zp < qmin:
        raise RuntimeError("quantize_tensor_per_tensor_affine zero_point %d is below lower bound." % zp)
    n = x.numel()
    s = _k.q_quantize(_plain(x)._s, scale, zp, 1, 1, n, qmin, qmax, under.name, 1 if dt is torch.qint32 else 0)
    return _make_pt(_dense(s, x.shape, under), dt, scale, zp)


def quantize_per_tensor(input, scale, zero_point, dtype):
    if _isinstance(input, (_list, _tuple)):
        # The list form: one scale and zero point (tensors) per input.
        scales = scale.tolist() if _isinstance(scale, _T) else _list(scale)
        zps = zero_point.tolist() if _isinstance(zero_point, _T) else _list(zero_point)
        return _tuple(quantize_per_tensor(t, torch.tensor(s, dtype=torch.float32) if _isinstance(scale, _T) else s, z, dtype) for t, s, z in zip(input, scales, zps))
    _check_float_input(input)
    s = _scalar(scale, _float)
    z = _scalar(zero_point, _int)
    if _isinstance(scale, _T):
        s = _float(_fround(s)) if scale.dtype is not torch.float64 else s
    return _quantize_pt(input, s, z, dtype)


def quantize_per_channel(input, scales, zero_points, axis, dtype):
    _check_float_input(input)
    under, qmin, qmax = _check_qdtype(dtype)
    rank = _len(input.shape)
    axis = _int(axis)
    if axis < 0 or axis >= rank:
        raise RuntimeError("Expected axis to be in the range [0, %d) but got %d" % (rank, axis))
    if not (_isinstance(scales, _T) and _isinstance(zero_points, _T)):
        raise TypeError("quantize_per_channel(): scales and zero_points must be tensors")
    C = input.shape[axis]
    if scales.numel() != C:
        raise RuntimeError("length of scales must equal to channel, expected %d got, %d" % (C, scales.numel()))
    if zero_points.numel() != C:
        raise RuntimeError("length of zero_points must equal to channel expected %d got, %d" % (C, zero_points.numel()))
    if zero_points.dtype.is_floating_point:
        raise NotImplementedError("per-channel quantization with floating point zero points (per_channel_affine_float_qparams) is not supported on Zipp")
    sc = _plain(scales.detach().reshape(-1).to(torch.float64)).clone()
    zp = _plain(zero_points.detach().reshape(-1).to(torch.int64)).clone()
    for z in zp.tolist():
        if z > qmax:
            raise RuntimeError("quantize_tensor_per_channel_affine zero_point %d is above upper bound." % z)
        if z < qmin:
            raise RuntimeError("quantize_tensor_per_channel_affine zero_point %d is below lower bound." % z)
    outer, inner = _layout(input.shape, axis)
    s = _k.q_quantize(_plain(input)._s, sc._s, zp._s, outer, C, inner, qmin, qmax, under.name, 2)
    return _make_pc(_dense(s, input.shape, under), dtype, sc, zp, axis)


def _choose(mn, mx, qmin, qmax, preserve_sparsity=False, reduce_range=False):
    """PyTorch's quant_utils::ChooseQuantizationParams: (scale, zero point)
    for a float range, in double arithmetic, min/max kept as floats."""
    if reduce_range:
        qmin = _int(qmin / 2)
        qmax = _int(qmax / 2)
    mn = _fround(mn)
    mx = _fround(mx)
    if mn > mx:
        raise RuntimeError("In ChooseQuantizationParams, min should be less than or equal to max")
    if mn < 0 and mx > 0 and preserve_sparsity:
        sym_qmin = -(_int((qmax - qmin) / 2) + 1)
        sym_qmax = _int((qmax - qmin) / 2)
        # float divisions (min/max are floats in PyTorch's signature)
        max_scale = max(abs(_fround(mn / sym_qmin)), abs(_fround(mx / sym_qmax)))
        mn = _fround(max_scale * sym_qmin)
        mx = _fround(max_scale * sym_qmax)
    mn = min(mn, 0.0)
    mx = max(mx, 0.0)
    scale = (mx - mn) / (qmax - qmin)
    fs = _fround(scale)
    if fs == 0.0 or 1.0 / fs == _float("inf") or _fround(1.0 / fs) == _float("inf"):
        scale = 0.1
    small = _fround(6.1e-5)
    if scale < small:
        org = scale
        scale = small
        if mn == 0.0:
            mx = _fround(small * (qmax - qmin))
        elif mx == 0.0:
            mn = _fround(-small * (qmax - qmin))
        else:
            amp = _fround(small / org)
            mn = _fround(mn * amp)
            mx = _fround(mx * amp)
    zp_min = qmin - mn / scale
    zp_max = qmax - mx / scale
    err_min = abs(qmin) - abs(mn / scale)
    err_max = abs(qmax) - abs(mx / scale)
    initial = zp_min if err_min < err_max else zp_max
    if mn < 0 and mx > 0 and preserve_sparsity:
        initial = (qmin + qmax) / 2
    if initial < qmin:
        zp = qmin
    elif initial > qmax:
        zp = qmax
    else:
        zp = _int(round(initial))
    return scale, zp


def _minmax(x):
    if x.numel() == 0:
        return 0.0, 0.0
    lo, hi = torch.aminmax(_plain(x))
    return _float(lo.item()), _float(hi.item())


def _choose_qparams_per_tensor(input, reduce_range=False):
    mn, mx = _minmax(input)
    return _choose(mn, mx, 0, 255, False, _bool(reduce_range))


def quantize_per_tensor_dynamic(input, dtype, reduce_range):
    _check_float_input(input)
    if dtype not in (torch.quint8, torch.qint8):
        raise RuntimeError("dtype %snot supported" % _scalar_name(dtype))
    under, qmin, qmax = _INFO[dtype.name]
    mn, mx = _minmax(input)
    scale, zp = _choose(mn, mx, qmin, qmax, False, _bool(reduce_range))
    return _quantize_pt(input, scale, zp, dtype)


def dequantize(tensor):
    if _isinstance(tensor, (_list, _tuple)):
        return _tuple(dequantize(t) for t in tensor)
    if tensor.__class__ is not _Q:
        return tensor
    q = tensor._q
    n = q.numel()
    if _per_channel(tensor):
        outer, inner = _layout(q.shape, tensor._axis)
        s = _k.q_dequantize(q._s, tensor._scales._s, tensor._zps._s, outer, q.shape[tensor._axis], inner, True)
    else:
        s = _k.q_dequantize(q._s, tensor._scale, tensor._zp, 1, 1, n, False)
    return _dense(s, q.shape, torch.float32)


def _int_input(t, what):
    if not _isinstance(t, _T) or t.dtype.name not in _OF_INT:
        raise RuntimeError("%s: expected a uint8, int8 or int32 tensor of integers" % what)
    return _plain(t.detach()).contiguous()


def _make_per_tensor_quantized_tensor(input, scale, zero_point):
    q = _int_input(input, "_make_per_tensor_quantized_tensor").clone()
    return _make_pt(q, _OF_INT[q.dtype.name], _scalar(scale, _float), _scalar(zero_point, _int))


def _make_per_channel_quantized_tensor(input, scale, zero_point, axis):
    q = _int_input(input, "_make_per_channel_quantized_tensor").clone()
    C = q.shape[axis]
    if scale.numel() != C:
        raise RuntimeError("length of scales must equal to channel, expected %d got, %d" % (C, scale.numel()))
    sc = _plain(scale.detach().reshape(-1).to(torch.float64)).clone()
    zp = _plain(zero_point.detach().reshape(-1).to(torch.int64)).clone()
    return _make_pc(q, _OF_INT[q.dtype.name], sc, zp, _int(axis))


def _empty_affine_quantized(size, *, scale=1.0, zero_point=0, dtype=None, layout=None, device=None, pin_memory=False, memory_format=None):
    dt = torch.quint8 if dtype is None else dtype
    under, qmin, qmax = _check_qdtype(dt)
    shape = torch._shape_args((size,))
    # PyTorch leaves the memory uninitialized; here it is zero.
    return _make_pt(torch.zeros(shape, dtype=under), dt, _float(scale), _int(zero_point))


def _empty_per_channel_affine_quantized(size, *, scales, zero_points, axis, dtype=None, layout=None, device=None, pin_memory=False, memory_format=None):
    dt = torch.quint8 if dtype is None else dtype
    under, qmin, qmax = _check_qdtype(dt)
    shape = torch._shape_args((size,))
    return _make_per_channel_quantized_tensor(torch.zeros(shape, dtype=under), scales, zero_points, axis)


# ---- fake quantization --------------------------------------------------------------------
def _fq_check(qmin, qmax, zps):
    if qmin > qmax:
        raise RuntimeError("`quant_min` should be less than or         equal to `quant_max`.")
    for z in zps:
        if z < qmin or z > qmax:
            raise RuntimeError("`zero_point` must be between `quant_min` and `quant_max`.")


def _fake_quant(x, scales, zps, outer, C, inner, qmin, qmax, name):
    if not x.dtype.is_floating_point:
        raise RuntimeError("fake_quantize: expected a floating point input, got %s" % _scalar_name(x.dtype))
    src = _plain(x)
    dt = x.dtype
    work = src if dt in (torch.float32, torch.float64) else src.to(torch.float32)
    out_s, mask_s = _k.q_fake_quant(work._s, scales, zps, outer, C, inner, qmin, qmax)
    out = _dense(out_s, x.shape, work.dtype)
    if out.dtype is not dt:
        out = out.to(dt)
    if torch._grad_enabled and x.requires_grad:
        mask = _dense(mask_s, x.shape, torch.bool)
        out.requires_grad = True
        out._node = torch._Node(lambda g: (torch.where(mask, g, torch.zeros_like(g)),), (x,), name)
    return out


def fake_quantize_per_tensor_affine(input, scale, zero_point, quant_min, quant_max):
    tensor_qparams = _isinstance(scale, _T) or _isinstance(zero_point, _T)
    s = _scalar(scale, _float)
    z = _scalar(zero_point, _int) if not (_isinstance(zero_point, _T) and zero_point.dtype.is_floating_point) else _int(round(zero_point.item()))
    qmin, qmax = _int(quant_min), _int(quant_max)
    _fq_check(qmin, qmax, [z])
    name = "FakeQuantizePerTensorAffineCachemaskTensorQparams" if tensor_qparams else "FakeQuantizePerTensorAffineCachemask"
    return _fake_quant(input, s, z, 1, 1, input.numel(), qmin, qmax, name)


def fake_quantize_per_channel_affine(input, scale, zero_point, axis, quant_min, quant_max):
    if zero_point.dtype not in (torch.int32, torch.float32, torch.float16):
        raise RuntimeError("Zero-point must be Int32, Float or Half, found %s" % _scalar_name(zero_point.dtype))
    if zero_point.dtype.is_floating_point:
        raise NotImplementedError("fake_quantize_per_channel_affine with floating point zero points is not supported on Zipp")
    rank = _len(input.shape)
    axis = _int(axis)
    if axis < 0:
        axis += rank
    if axis < 0 or axis >= rank:
        raise RuntimeError("`axis` must be between 0 and number of dimensions of input")
    C = input.shape[axis]
    if scale.numel() != C or zero_point.numel() != C:
        raise RuntimeError("dimensions of scale and zero-point are not consistent with input tensor")
    qmin, qmax = _int(quant_min), _int(quant_max)
    zl = zero_point.reshape(-1).tolist()
    _fq_check(qmin, qmax, zl)
    sc = _plain(scale.detach().reshape(-1).to(torch.float32))
    zp = _plain(zero_point.detach().reshape(-1).to(torch.int64))
    outer, inner = _layout(input.shape, axis)
    return _fake_quant(input, sc._s, zp._s, outer, C, inner, qmin, qmax, "FakeQuantizePerChannelAffineCachemask")


def fused_moving_avg_obs_fake_quant(input, observer_on, fake_quant_on, running_min, running_max, scale, zero_point, averaging_const, quant_min, quant_max, ch_axis, per_row_fake_quant=False, symmetric_quant=False):
    """PyTorch's fused observer + fake quantization (FusedMovingAvgObsFakeQuantize):
    the moving-average min/max update in float, qparams from
    ChooseQuantizationParams (preserve_sparsity for symmetric schemes),
    then the cachemask fake quantization. running_min/max, scale and
    zero_point are updated in place."""
    qmin, qmax = _int(quant_min), _int(quant_max)
    c = _fround(_float(averaging_const))
    if _int(observer_on.reshape(-1)[0].item()) == 1:
        x = _plain(input.detach())
        if per_row_fake_quant:
            axis = _int(ch_axis)
            order = _list(_range(_len(x.shape)))
            order[axis], order[0] = 0, axis
            y = torch.flatten(x.permute(*order), 1)
            cur_min, cur_max = torch.aminmax(y, dim=1)
        else:
            cur_min, cur_max = torch.aminmax(x)
            cur_min, cur_max = cur_min.reshape(1), cur_max.reshape(1)
        with torch.no_grad():
            if running_min.numel() != cur_min.numel():
                running_min.resize_(cur_min.shape)
                running_max.resize_(cur_max.shape)
                running_min.fill_(_float("inf"))
                running_max.fill_(_float("-inf"))
            rmin = running_min.reshape(-1).tolist()
            rmax = running_max.reshape(-1).tolist()
            mins = cur_min.reshape(-1).tolist()
            maxs = cur_max.reshape(-1).tolist()
            new_min, new_max, scales, zps = [], [], [], []
            for i in _range(_len(mins)):
                a = mins[i] if rmin[i] in (_float("inf"), _float("-inf")) else _fround(rmin[i] + _fround(c * _fround(mins[i] - rmin[i])))
                b = maxs[i] if rmax[i] in (_float("inf"), _float("-inf")) else _fround(rmax[i] + _fround(c * _fround(maxs[i] - rmax[i])))
                new_min.append(a)
                new_max.append(b)
                s, z = _choose(a, b, qmin, qmax, _bool(symmetric_quant), False)
                scales.append(s)
                zps.append(z)
            running_min.copy_(torch.tensor(new_min, dtype=running_min.dtype).reshape(running_min.shape))
            running_max.copy_(torch.tensor(new_max, dtype=running_max.dtype).reshape(running_max.shape))
            if scale.numel() != _len(scales):
                scale.resize_((_len(scales),))
                zero_point.resize_((_len(scales),))
            scale.copy_(torch.tensor(scales, dtype=torch.float64).to(scale.dtype).reshape(scale.shape))
            zero_point.copy_(torch.tensor(zps, dtype=torch.int64).to(zero_point.dtype).reshape(zero_point.shape))
    if _int(fake_quant_on.reshape(-1)[0].item()) == 1:
        if per_row_fake_quant:
            axis = _int(ch_axis)
            outer, inner = _layout(input.shape, axis)
            sc = _plain(scale.detach().reshape(-1).to(torch.float32))
            zp = _plain(zero_point.detach().reshape(-1).to(torch.int64))
            _fq_check(qmin, qmax, zp.tolist())
            return _fake_quant(input, sc._s, zp._s, outer, input.shape[axis], inner, qmin, qmax, "_FusedMovingAvgObsFqHelper")
        z = _int(zero_point.reshape(-1)[0].item())
        _fq_check(qmin, qmax, [z])
        return _fake_quant(input, _float(scale.reshape(-1)[0].item()), z, 1, 1, input.numel(), qmin, qmax, "_FusedMovingAvgObsFqHelper")
    return input.clone() if not input.requires_grad else input * 1


# ---- quantized kernels -------------------------------------------------------------------
def _engine():
    return torch.backends.quantized.engine


def _weight_params(w, n_out):
    """(zero points, float32 scales) of a quantized weight, per output
    channel: per-tensor weights repeat theirs."""
    if _per_channel(w):
        if w._axis != 0:
            raise RuntimeError("quantized kernels support per-channel weights along axis 0 only")
        return w._zps._s, _plain(w._scales.to(torch.float32))
    return w._zp, torch.full((n_out,), _fround(w._scale), dtype=torch.float32)


def _symmetric(w):
    if _per_channel(w):
        return not bool((w._zps != 0).any())
    return w._zp == 0


def _requant_params(x_scale, w_scales, y_scale):
    atw = w_scales * _fround(x_scale)
    mult = atw / _fround(y_scale)
    return atw, mult


def _check_act(x, what):
    if x.__class__ is not _Q:
        raise NotImplementedError("Could not run '%s' with arguments from the 'CPU' backend. This could be because the operator doesn't exist for this backend, or was omitted during the selective/custom build process (if using custom build). '%s' is only available for these backends: [QuantizedCPU]." % (what, what))
    if x.dtype is not torch.quint8 or _per_channel(x):
        raise RuntimeError("%s: expected a per-tensor quint8 input on Zipp (as fbgemm/x86)" % what)


def _bias_storage(b):
    return None if b is None else _plain(b.detach().to(torch.float32))._s


def linear(x, w, b, scale, zero_point, relu=False):
    """quantized::linear (and linear_relu): quint8 x [..., K] times the
    qint8 weight [N, K] plus a float bias, requantized to (scale, zp)."""
    _check_act(x, "quantized::linear")
    N, K = w.shape[0], w.shape[1]
    lead = _tuple(x.shape[:-1])
    if x.shape[-1] != K:
        raise RuntimeError("The number of rows in the packB should be equal to K: %d" % K)
    M = 1
    for d in lead:
        M *= d
    wz, ws = _weight_params(w, N)
    acc = _k.q_matmul(x._q._s, x._zp, M, K, w._q._s, wz, N)
    atw, mult = _requant_params(x._scale, ws, scale)
    zp = _int(zero_point)
    mode = 1 if _engine() == "onednn" else 0
    out = _k.q_requant(acc, M, N, 1, _bias_storage(b), atw._s, mult._s, zp, zp if relu else 0, 255, mode)
    return _make_pt(_dense(out, lead + (N,), torch.uint8), torch.quint8, _float(scale), zp)


def linear_dynamic(x, w, b, reduce_range=True, relu=False):
    """quantized::linear_dynamic: the float input quantized per call
    (ChooseQuantizationParams over its range, reduce_range as fbgemm),
    the integer product, and fbgemm's float output."""
    if x.__class__ is _Q or not x.dtype.is_floating_point:
        raise RuntimeError("quantized::linear_dynamic: expected a float input")
    xf = _plain(x.detach()).to(torch.float32).contiguous()
    N, K = w.shape[0], w.shape[1]
    lead = _tuple(xf.shape[:-1])
    if xf.shape[-1] != K:
        raise RuntimeError("The number of rows in the packB should be equal to K: %d" % K)
    M = 1
    for d in lead:
        M *= d
    mn, mx = _minmax(xf)
    xs, xz = _choose(mn, mx, 0, 255, False, _bool(reduce_range))
    xq = _k.q_quantize(xf._s, xs, xz, 1, 1, xf.numel(), 0, 255, "uint8", 0)
    wz, ws = _weight_params(w, N)
    acc = _k.q_matmul(xq, xz, M, K, w._q._s, wz, N)
    atw = ws * _fround(xs)
    out = _dense(_k.q_requant(acc, M, N, 1, _bias_storage(b), atw._s, atw._s, 0, 0, 0, 2), lead + (N,), torch.float32)
    return torch.relu(out) if relu else out


def conv2d(x, w, b, stride, padding, dilation, groups, scale, zero_point, relu=False):
    """quantized::conv2d (and conv2d_relu) on a quint8 NCHW input."""
    _check_act(x, "quantized::conv2d")
    if _len(x.shape) != 4:
        raise ValueError("Input shape must be `(N, C, H, W)`!")
    O = w.shape[0]
    wz, ws = _weight_params(w, O)
    res = _k.q_conv2d(x._q._s, _tuple(x.shape), x._zp, w._q._s, _tuple(w.shape), wz, _tuple(stride), _tuple(padding), _tuple(dilation), _int(groups))
    acc, shape = res
    atw, mult = _requant_params(x._scale, ws, scale)
    zp = _int(zero_point)
    engine = _engine()
    onednn = engine == "onednn" or (engine == "x86" and _symmetric(w) and groups <= 100)
    inner = shape[2] * shape[3]
    out = _k.q_requant(acc, shape[0], O, inner, _bias_storage(b), atw._s, mult._s, zp, zp if relu else 0, 255, 1 if onednn else 0)
    return _make_pt(_dense(out, shape, torch.uint8), torch.quint8, _float(scale), zp)


def conv1d(x, w, b, stride, padding, dilation, groups, scale, zero_point, relu=False):
    """quantized::conv1d: conv2d over a height of one."""
    if _len(x.shape) != 3:
        raise ValueError("Input shape must be `(N, C, L)`!")
    x4 = _same_params(x, x._q.unsqueeze(2))
    w4 = _same_params(w, w._q.unsqueeze(2))
    y = conv2d(x4, w4, b, (1, stride[0]), (0, padding[0]), (1, dilation[0]), groups, scale, zero_point, relu)
    return _same_params(y, y._q.squeeze(2))


# ---- operations on quantized tensors ------------------------------------------------------
def _relu(x, inplace=False):
    if _per_channel(x):
        raise RuntimeError("Expected quantizer->qscheme() == kPerTensorAffine to be true, but got false.  (Could this error message be improved?  If so, please report an enhancement request to PyTorch.)")
    q = torch.clamp_min(x._q, x._zp).to(x._q.dtype)
    if inplace:
        x._q = q
        return x
    return _same_params(x, q)


def _hardtanh(x, min_val, max_val, inplace=False):
    """quantized hardtanh/relu6: the bounds quantized with x's parameters."""
    if _per_channel(x):
        raise RuntimeError("Expected quantizer->qscheme() == kPerTensorAffine to be true, but got false.  (Could this error message be improved?  If so, please report an enhancement request to PyTorch.)")
    under, qmin, qmax = _INFO[x.dtype.name]
    lo = _k.to_list(_k.q_quantize(torch.tensor([_float(min_val), _float(max_val)])._s, x._scale, x._zp, 1, 1, 2, qmin, qmax, under.name, 1))
    q = torch.clamp(x._q, lo[0], lo[1]).to(x._q.dtype)
    if inplace:
        x._q = q
        return x
    return _same_params(x, q)


def _max_pool(x, fn, *args):
    """A max pool of a quantized tensor: the pool of its integers (the
    order is the same), with its parameters."""
    q = x._q
    out = fn(q.to(torch.float32), *args)
    if _isinstance(out, _tuple):
        return (_same_params(x, out[0].to(q.dtype)), out[1])
    return _same_params(x, out.to(q.dtype))


def _check_same(ts, what):
    first = ts[0]
    for t in ts:
        if t.__class__ is not _Q:
            raise NotImplementedError("Could not run 'aten::qscheme' with arguments from the 'CPU' backend. This could be because the operator doesn't exist for this backend, or was omitted during the selective/custom build process (if using custom build).")
        if t.dtype is not first.dtype or _per_channel(t):
            raise RuntimeError("%s: all inputs must be per-tensor quantized tensors of one dtype on Zipp" % what)


def _cat(tensors, dim=0):
    """torch.cat of quantized tensors: the result takes the first input's
    parameters; the others are requantized into them."""
    _check_same(tensors, "cat")
    first = tensors[0]
    parts = []
    for t in tensors:
        if t._scale == first._scale and t._zp == first._zp:
            parts.append(t._q)
        else:
            parts.append(_quantize_pt(dequantize(t), first._scale, first._zp, first.dtype)._q)
    return _same_params(first, torch.cat(parts, dim))


def _stack(tensors, dim=0):
    rank = _len(tensors[0].shape) + 1
    d = dim + rank if dim < 0 else dim
    return _cat([_same_params(t, t._q.unsqueeze(d)) for t in tensors], d)


def _equal(a, b):
    if a.__class__ is not _Q or b.__class__ is not _Q:
        return False
    if a.dtype is not b.dtype or a._qs is not b._qs or _tuple(a.shape) != _tuple(b.shape):
        return False
    if _per_channel(a):
        if a._axis != b._axis or not torch.equal(a._scales, b._scales) or not torch.equal(a._zps, b._zps):
            return False
    elif a._scale != b._scale or a._zp != b._zp:
        return False
    return torch.equal(a._q, b._q)


def _reshape_q(x, q):
    if _per_channel(x):
        C = x._scales.numel()
        axis = x._axis
        if axis >= _len(q.shape) or q.shape[axis] != C:
            raise RuntimeError("length of scales must equal to channel, expected %d got, %d" % (q.shape[axis] if axis < _len(q.shape) else 1, C))
    return _same_params(x, q)


def _no_strides(x):
    if _per_channel(x):
        raise RuntimeError("Setting strides is possible only on uniformly quantized tensor")


# ---- Tensor methods ----------------------------------------------------------------------
def _m_repr(self, *, tensor_contents=None):
    indent = _len("tensor(")
    suffixes = ["size=" + str(_tuple(self.shape))]
    suffixes.append("dtype=" + repr(self.dtype))
    suffixes.append("quantization_scheme=" + repr(self._qs))
    if _per_channel(self):
        suffixes.append("scale=" + repr(self._scales))
        suffixes.append("zero_point=" + repr(self._zps))
        suffixes.append("axis=" + str(self._axis))
    else:
        suffixes.append("scale=" + repr(self._scale))
        suffixes.append("zero_point=" + str(self._zp))
    d = dequantize(self)
    body = "[]" if d.numel() == 0 else torch._tensor_str(d, indent)
    return torch._add_suffixes("tensor(" + body, suffixes, indent)


def _m_format(self, spec):
    if spec:
        raise TypeError("unsupported format string passed to Tensor.__format__")
    return _m_repr(self)


def _m_int_repr(self):
    return self._q.clone()


def _m_q_scale(self):
    if _per_channel(self):
        raise RuntimeError("Expected quantizer->qscheme() == kPerTensorAffine to be true, but got false.  (Could this error message be improved?  If so, please report an enhancement request to PyTorch.)")
    return self._scale


def _m_q_zero_point(self):
    if _per_channel(self):
        raise RuntimeError("Expected quantizer->qscheme() == kPerTensorAffine to be true, but got false.  (Could this error message be improved?  If so, please report an enhancement request to PyTorch.)")
    return self._zp


def _need_pc(self):
    if not _per_channel(self):
        raise RuntimeError("Expected quantizer->qscheme() == kPerChannelAffine || quantizer->qscheme() == kPerChannelAffineFloatQParams to be true, but got false.  (Could this error message be improved?  If so, please report an enhancement request to PyTorch.)")


def _m_q_per_channel_scales(self):
    _need_pc(self)
    return self._scales.clone()


def _m_q_per_channel_zero_points(self):
    _need_pc(self)
    return self._zps.clone()


def _m_q_per_channel_axis(self):
    _need_pc(self)
    return self._axis


def _m_qscheme(self):
    return self._qs


def _m_clone(self, memory_format=None):
    out = _same_params(self, self._q.clone())
    if _per_channel(self):
        out._scales = self._scales.clone()
        out._zps = self._zps.clone()
    return out


def _m_detach(self):
    return _same_params(self, self._q)


def _m_view(self, *shape):
    return _reshape_q(self, self._q.view(*shape))


def _m_reshape(self, *shape):
    return _reshape_q(self, self._q.reshape(*shape))


def _m_flatten(self, start_dim=0, end_dim=-1):
    return _reshape_q(self, self._q.flatten(start_dim, end_dim))


def _m_squeeze(self, *dim):
    return _reshape_q(self, self._q.squeeze(*dim))


def _m_unsqueeze(self, dim):
    if _per_channel(self):
        rank = _len(self.shape) + 1
        d = dim + rank if dim < 0 else dim
        out = _same_params(self, self._q.unsqueeze(dim))
        if d <= self._axis:
            out._axis = self._axis + 1
        return out
    return _same_params(self, self._q.unsqueeze(dim))


def _m_permute(self, *dims):
    _no_strides(self)
    return _same_params(self, self._q.permute(*dims))


def _m_transpose(self, dim0, dim1):
    _no_strides(self)
    return _same_params(self, self._q.transpose(dim0, dim1))


def _m_t(self):
    _no_strides(self)
    return _same_params(self, self._q.t())


def _m_expand(self, *sizes):
    _no_strides(self)
    return _same_params(self, self._q.expand(*sizes).contiguous())


def _m_getitem(self, key):
    if not _per_channel(self):
        return _same_params(self, self._q[key])
    keys = key if _isinstance(key, _tuple) else (key,)
    for k in keys:
        if not (_isinstance(k, (_int, slice)) and not _isinstance(k, _bool)):
            raise NotImplementedError("indexing a per-channel quantized tensor supports ints and slices on Zipp")
    axis = self._axis
    if axis < _len(keys) and _isinstance(keys[axis], _int):
        # Selecting one channel leaves a per-tensor tensor with its parameters.
        c = keys[axis]
        return _make_pt(self._q[key], self.dtype, self._scales[c].item(), _int(self._zps[c].item()))
    removed = 0
    for k in keys[:axis]:
        if _isinstance(k, _int):
            removed += 1
    q = self._q[key]
    if axis < _len(keys):
        sl = keys[axis]
        return _make_pc(q, self.dtype, self._scales[sl].clone(), self._zps[sl].clone(), axis - removed)
    return _make_pc(q, self.dtype, self._scales, self._zps, axis - removed)


def _m_item(self):
    return dequantize(self).item()


def _m_tolist(self):
    raise RuntimeError("load_scalar: invalid type")


def _m_numpy(self, force=False):
    raise TypeError("Got unsupported ScalarType %s" % _scalar_name(self.dtype))


def _m_to(self, *args, **kwargs):
    for a in _list(args) + _list(kwargs.values()):
        if _isinstance(a, torch.dtype) and a is not self.dtype:
            raise RuntimeError("Creation of quantized tensor requires quantized dtype like torch.quint8")
    return self


def _m_float_cast(self, *args, **kwargs):
    raise RuntimeError("Creation of quantized tensor requires quantized dtype like torch.quint8")


def _m_relu(self):
    return _relu(self)


def _m_relu_(self):
    return _relu(self, True)


def _m_max(self, dim=None, keepdim=False):
    if dim is not None:
        raise _no_kernel("aten::max.dim")
    return _same_params(self, self._q.max())


def _m_min(self, dim=None, keepdim=False):
    if dim is not None:
        raise _no_kernel("aten::min.dim")
    return _same_params(self, self._q.min())


def _m_eq(self, other):
    return dequantize(self) == dequantize(other)


def _m_ne(self, other):
    return dequantize(self) != dequantize(other)


def _m_deepcopy(self, memo):
    out = _m_clone(self)
    memo[id(self)] = out
    return out


def _m_requires_grad_(self, requires_grad=True):
    if requires_grad:
        raise RuntimeError("only Tensors of floating point and complex dtype can require gradients")
    return self


def _m_stride(self, dim=None):
    s = _T.stride(self._q)
    return s if dim is None else s[torch._norm_dim(dim, _len(self.shape))]


def _m_data(self):
    return _m_detach(self)


def _m_storage(self):
    raise NotImplementedError("Tensor.storage() of a quantized tensor is not supported on Zipp; use int_repr()")


def _m_dense_int_repr(self):
    raise _no_kernel_cpu("aten::int_repr")


def _no_kernel_cpu(op):
    return NotImplementedError("Could not run '%s' with arguments from the 'CPU' backend. This could be because the operator doesn't exist for this backend, or was omitted during the selective/custom build process (if using custom build)." % op)


def _dense_q_accessor(op):
    def method(self, *args):
        raise _no_kernel_cpu(op)
    return method


def _dense_dequantize(self):
    return self


def _q_reduce_args(t):
    """(storage, offset, size, stride, quantizer_params) as PyTorch pickles
    a quantized tensor (torch._utils._rebuild_qtensor)."""
    st = torch._STORAGE_TYPES[t.dtype.name](t._q._s)
    if _per_channel(t):
        params = (t._qs, t._scales, t._zps, t._axis)
    else:
        params = (t._qs, t._scale, t._zp)
    return (st, 0, _tuple(t.shape), _T.stride(t._q), params)


def _rebuild_qtensor(storage, storage_offset, size, stride, quantizer_params, requires_grad=False, backward_hooks=None):
    dt = storage.dtype
    under, qmin, qmax = _check_qdtype(dt)
    q = torch._rebuild_tensor_v2(torch._STORAGE_TYPES[under.name](storage._s), storage_offset, size, stride)
    scheme = quantizer_params[0]
    if scheme in (torch.per_tensor_affine, torch.per_tensor_symmetric):
        return _make_pt(q, dt, _float(quantizer_params[1]), _int(quantizer_params[2]))
    if scheme in (torch.per_channel_affine, torch.per_channel_symmetric):
        return _make_pc(q, dt, _plain(quantizer_params[1]).to(torch.float64), _plain(quantizer_params[2]).to(torch.int64), _int(quantizer_params[3]))
    raise RuntimeError("Unsupported qscheme %s in a checkpoint on Zipp" % (scheme,))


def _m_self(self, *args, **kwargs):
    return self


def _m_true(self, *args, **kwargs):
    return True


def _raiser(op):
    def method(self, *args, **kwargs):
        raise _no_kernel(op)
    return method


def _install():
    q_methods = {
        "__repr__": _m_repr, "__str__": _m_repr, "__format__": _m_format, "__getitem__": _m_getitem, "__deepcopy__": _m_deepcopy,
        "__eq__": _m_eq, "__ne__": _m_ne, "dequantize": dequantize, "int_repr": _m_int_repr, "q_scale": _m_q_scale,
        "q_zero_point": _m_q_zero_point, "q_per_channel_scales": _m_q_per_channel_scales,
        "q_per_channel_zero_points": _m_q_per_channel_zero_points, "q_per_channel_axis": _m_q_per_channel_axis,
        "qscheme": _m_qscheme, "clone": _m_clone, "detach": _m_detach, "detach_": _m_self, "view": _m_view,
        "reshape": _m_reshape, "flatten": _m_flatten, "squeeze": _m_squeeze, "unsqueeze": _m_unsqueeze, "permute": _m_permute,
        "transpose": _m_transpose, "t": _m_t, "expand": _m_expand, "item": _m_item, "tolist": _m_tolist, "numpy": _m_numpy,
        "to": _m_to, "float": _m_float_cast, "double": _m_float_cast, "half": _m_float_cast, "long": _m_float_cast,
        "int": _m_float_cast, "relu": _m_relu, "relu_": _m_relu_, "max": _m_max, "min": _m_min,
        "requires_grad_": _m_requires_grad_, "stride": _m_stride, "storage": _m_storage, "untyped_storage": _m_storage,
        "contiguous": _m_self, "is_contiguous": _m_true, "cpu": _m_self,
    }
    for name in q_methods:
        setattr(_Q, name, q_methods[name])
    # Arithmetic and reductions have no quantized kernel (PyTorch's error;
    # the QFunctional / quantized modules are the quantized arithmetic).
    for name, op in (("__add__", "add"), ("__radd__", "add"), ("__iadd__", "add_"), ("__sub__", "sub"), ("__rsub__", "sub"),
                     ("__isub__", "sub_"), ("__mul__", "mul"), ("__rmul__", "mul"), ("__imul__", "mul_"), ("__truediv__", "div"),
                     ("__rtruediv__", "div"), ("__itruediv__", "div_"), ("__matmul__", "matmul"), ("__rmatmul__", "matmul"),
                     ("__neg__", "neg"), ("__pow__", "pow"), ("add", "add"), ("sub", "sub"), ("mul", "mul"), ("div", "div"),
                     ("matmul", "matmul"), ("mm", "mm"), ("sum", "sum.IntList_out"), ("mean", "mean.out"), ("abs", "abs"), ("exp", "exp"),
                     ("log", "log"), ("sqrt", "sqrt"), ("sigmoid", "sigmoid"), ("tanh", "tanh"), ("softmax", "_softmax"),
                     ("log_softmax", "_log_softmax"), ("add_", "add_"), ("mul_", "mul_"), ("sub_", "sub_"), ("div_", "div_"),
                     ("copy_", "copy_"), ("fill_", "fill_"), ("zero_", "zero_"), ("backward", "backward")):
        setattr(_Q, name, _raiser("aten::" + op))
    _Q.data = property(_m_data)
    _Q.T = property(_m_t)
    dense = {"dequantize": _dense_dequantize, "int_repr": _dense_q_accessor("aten::int_repr"),
             "q_scale": _dense_q_accessor("aten::q_scale"), "q_zero_point": _dense_q_accessor("aten::q_zero_point"),
             "q_per_channel_scales": _dense_q_accessor("aten::q_per_channel_scales"),
             "q_per_channel_zero_points": _dense_q_accessor("aten::q_per_channel_zero_points"),
             "q_per_channel_axis": _dense_q_accessor("aten::q_per_channel_axis"), "qscheme": _dense_q_accessor("aten::qscheme")}
    for name in dense:
        if not hasattr(_T, name):
            setattr(_T, name, dense[name])


_install()


# ---- quantized elementwise ops (QFunctional) ---------------------------------------------
def _check_pair(x, y, what):
    for t in (x, y):
        if t.__class__ is not _Q or _per_channel(t):
            raise RuntimeError("%s: expected per-tensor quantized inputs" % what)
    if x.dtype is not y.dtype:
        raise RuntimeError("%s: the inputs must have the same dtype" % what)


def qadd(x, y, scale, zero_point, relu=False):
    """quantized::add(_relu): the dequantized sum, requantized to (scale,
    zero_point)."""
    _check_pair(x, y, "quantized::add")
    c = dequantize(x) + dequantize(y)
    if relu:
        c = torch.relu(c)
    return _quantize_pt(c, _float(scale), _int(zero_point), x.dtype)


def qmul(x, y, scale, zero_point, relu=False):
    _check_pair(x, y, "quantized::mul")
    c = dequantize(x) * dequantize(y)
    if relu:
        c = torch.relu(c)
    return _quantize_pt(c, _float(scale), _int(zero_point), x.dtype)


def qcat(xs, dim, scale, zero_point):
    _check_same(xs, "quantized::cat")
    c = torch.cat([dequantize(t) for t in xs], dim)
    return _quantize_pt(c, _float(scale), _int(zero_point), xs[0].dtype)


def qadd_scalar(x, y):
    raise NotImplementedError("QFunctional.add_scalar (quantized::add_scalar) is not supported on Zipp")


def qmul_scalar(x, y):
    raise NotImplementedError("QFunctional.mul_scalar (quantized::mul_scalar) is not supported on Zipp")


# ---- quantized pooling -----------------------------------------------------------------------
def _pool_requant(x, acc, size_t, channels_last):
    """PyTorch's quantized average pools (input and output parameters
    equal): multiplier = float(1 / size); an NCHW input rounds
    float(acc * multiplier) + zero_point, a channels-last one (one channel,
    or a 1x1 image) rounds float(acc * multiplier) and adds the zero point."""
    mult = torch.tensor([1.0], dtype=torch.float32) / size_t.to(torch.float32)
    v = acc.to(torch.float32) * mult
    under, qmin, qmax = _INFO[x.dtype.name]
    if channels_last:
        r = torch.round(v) + x._zp
    else:
        r = torch.round(v + _float(x._zp))
    return _same_params(x, torch.clamp(r, qmin, qmax).to(under))


def _windows(n, out):
    return [((i * n) // out, -((-(i + 1) * n) // out)) for i in _range(out)]


def _adaptive_avg_pool(x, output_size, nd):
    if _per_channel(x):
        raise RuntimeError("quantized adaptive average pooling expects a per-tensor quantized input")
    q = x._q
    unbatched = _len(q.shape) == nd + 1
    if unbatched:
        q = q.unsqueeze(0)
    if nd == 1:
        q = q.unsqueeze(2)
        output_size = (1, output_size[0] if _isinstance(output_size, (_tuple, _list)) else output_size)
    B, C, H, W = q.shape
    if not _isinstance(output_size, (_tuple, _list)):
        output_size = (output_size, output_size)
    oh = H if output_size[0] is None else output_size[0]
    ow = W if output_size[1] is None else output_size[1]
    qi = q.to(torch.int64) - x._zp
    rows = []
    sizes = []
    for (h0, h1) in _windows(H, oh):
        cols = []
        srow = []
        for (w0, w1) in _windows(W, ow):
            cols.append(qi[:, :, h0:h1, w0:w1].sum((2, 3)))
            srow.append((h1 - h0) * (w1 - w0))
        rows.append(torch.stack(cols, 2))
        sizes.append(srow)
    acc = torch.stack(rows, 2)
    size_t = torch.tensor(sizes, dtype=torch.int64)
    out = _pool_requant(_same_params(x, q), acc, size_t, C == 1 or H * W == 1)
    if nd == 1:
        out = _same_params(out, out._q.squeeze(2))
    if unbatched:
        out = _same_params(out, out._q.squeeze(0))
    return out


def _avg_pool2d(x, kernel_size, stride, padding, ceil_mode, count_include_pad, divisor_override):
    if _per_channel(x):
        raise RuntimeError("quantized average pooling expects a per-tensor quantized input")
    q = x._q
    unbatched = _len(q.shape) == 3
    if unbatched:
        q = q.unsqueeze(0)
    B, C, H, W = q.shape
    kh, kw = kernel_size
    sh, sw = stride
    ph, pw = padding

    def out_len(n, k, s, p):
        num = n + 2 * p - k
        o = (num + (s - 1 if ceil_mode else 0)) // s + 1
        if ceil_mode and (o - 1) * s >= n + p:
            o -= 1
        return o
    oh, ow = out_len(H, kh, sh, ph), out_len(W, kw, sw, pw)
    qi = q.to(torch.int64) - x._zp
    rows, sizes = [], []
    for i in _range(oh):
        hs = i * sh - ph
        he = min(hs + kh, H + ph)
        full_h = he - hs
        h0, h1 = max(hs, 0), min(he, H)
        cols, srow = [], []
        for j in _range(ow):
            ws = j * sw - pw
            we = min(ws + kw, W + pw)
            full = full_h * (we - ws)
            w0, w1 = max(ws, 0), min(we, W)
            cols.append(qi[:, :, h0:h1, w0:w1].sum((2, 3)))
            if divisor_override:
                srow.append(_int(divisor_override))
            elif count_include_pad:
                srow.append(full)
            else:
                srow.append((h1 - h0) * (w1 - w0))
        rows.append(torch.stack(cols, 2))
        sizes.append(srow)
    acc = torch.stack(rows, 2)
    size_t = torch.tensor(sizes, dtype=torch.int64)
    out = _pool_requant(_same_params(x, q), acc, size_t, C == 1 or H * W == 1)
    if unbatched:
        out = _same_params(out, out._q.squeeze(0))
    return out


def _interpolate_nearest(x, fn, *args, **kwargs):
    q = x._q
    out = fn(q.to(torch.float32), *args, **kwargs)
    return _same_params(x, out.to(q.dtype))


class _LazyModule:
    """A subpackage attribute (torch.ao.quantization, torch.ao.nn.quantized,
    ...) that imports the module on first use and forwards to it: this
    runtime has no module-level __getattr__, and importing every
    subpackage eagerly would import them in a cycle."""

    def __init__(self, name):
        self._lazy_name = name

    def _lazy_load(self):
        import importlib
        return importlib.import_module(self._lazy_name)

    def __getattr__(self, attr):
        if attr.startswith("_lazy_"):
            raise AttributeError(attr)
        return getattr(self._lazy_load(), attr)

    def __dir__(self):
        return dir(self._lazy_load())

    def __repr__(self):
        return repr(self._lazy_load())
