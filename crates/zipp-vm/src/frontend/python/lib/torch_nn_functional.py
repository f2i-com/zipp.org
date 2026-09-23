"""torch.nn.functional for Zipp.

Operations compose from the eager tensor kernels in `torch`, so every one of
them takes part in autograd. Pooling, unfolding, padding modes and
interpolation work on strided slices and index selections; recurrent loops,
attention and normalisation are ordinary tensor expressions. Semantics
(argument order, defaults, output sizes, reductions, error types) follow
PyTorch 2.11.
"""
import math
import struct
import torch
from torch import Tensor
import _zipp_tensor as _k

_inf = float("inf")


# ---- argument helpers ----------------------------------------------------------------------
def _ntuple(value, n, name="argument"):
    if isinstance(value, (tuple, list)):
        if len(value) == 1 and n > 1:
            return tuple(int(value[0]) for _ in range(n))
        if len(value) != n:
            raise ValueError("%s must be an int or a tuple of %d ints, got %r" % (name, n, value))
        return tuple(int(v) for v in value)
    return tuple(int(value) for _ in range(n))


def _single(value):
    return _ntuple(value, 1)


def _pair(value):
    return _ntuple(value, 2)


def _reduction(size_average, reduce, reduction):
    # The legacy size_average/reduce flags override `reduction`, as in PyTorch.
    if size_average is None and reduce is None:
        return reduction
    size_average = True if size_average is None else size_average
    reduce = True if reduce is None else reduce
    return "none" if not reduce else "mean" if size_average else "sum"


def _reduce(loss, reduction):
    if reduction == "mean":
        return loss.mean()
    if reduction == "sum":
        return loss.sum()
    if reduction == "none":
        return loss
    raise ValueError("%s is not a valid value for reduction" % reduction)


def _apply_inplace(x, fn, *args):
    """x = fn(x, *args) written into x (the `inplace=True` form). The core's
    in-place protocol evaluates `fn` on the old values with their history,
    so autograd sees what PyTorch's in-place op records."""
    op = getattr(x, "_inplace_op", None)
    if op is not None:
        return op(fn, *args)
    out = fn(x, *args)
    if torch._needs_grad(x) or out.requires_grad:
        return x._inplace(out)
    x.copy_(out)
    return x


def _check_float(x, name):
    if not x.dtype.is_floating_point:
        raise RuntimeError("%s: expected a floating point input, got %s" % (name, x.dtype))


def _f32(v):
    """Round a Python float to float32, as PyTorch's CPU index arithmetic does."""
    return struct.unpack("f", struct.pack("f", v))[0]


def _unbind(x, dim=0):
    """x.unbind(dim) whose backward assembles the whole gradient once.

    A plain unbind gives every piece a slice node, and each slice's backward
    materialises a zero tensor of the full shape, so a T-step loop costs
    O(T^2). Here the pieces report their gradients to a shared scalar hub,
    and the hub's single node (run after every piece, being their common
    parent) stacks them into the gradient of `x`.
    """
    n = x.shape[dim]
    if not torch._needs_grad(x):
        return [x.select(dim, i) for i in range(n)]
    with torch.no_grad():
        pieces = [x.select(dim, i) for i in range(n)]
    slots = [None] * n
    piece_shape = tuple(pieces[0].shape) if n else ()

    def hub_backward(g):
        parts = [s if s is not None else torch.zeros(*piece_shape, dtype=x.dtype) for s in slots]
        for i in range(n):
            slots[i] = None
        return (torch.stack(parts, dim),)

    hub = torch.zeros((), dtype=x.dtype)
    hub.requires_grad = True
    hub._node = torch._Node(hub_backward, (x,), "UnbindBackward0")

    def report(i):
        def backward(g):
            slots[i] = g if slots[i] is None else slots[i] + g
            return (torch.zeros((), dtype=g.dtype),)
        return backward
    for i, p in enumerate(pieces):
        p.requires_grad = True
        p._node = torch._Node(report(i), (hub,), "SelectBackward0")
    return pieces


def _embed_slice(piece, full_shape, spec):
    """A zero tensor of `full_shape` holding `piece` at the slice `spec`; its
    backward takes the same slice of the gradient."""
    base = torch.zeros(*full_shape, dtype=piece.dtype)
    _k.setslice(base._s, base.shape, spec, piece._s, piece.shape)
    if torch._needs_grad(piece):
        def backward(g):
            storage, shape = _k.slice(g._s, g.shape, spec)
            return (Tensor(storage, shape, g.dtype),)
        base.requires_grad = True
        base._node = torch._Node(backward, (piece,), "SliceScatterBackward0")
    return base


def _strided(x, starts, count, step, first_dim):
    """x[..., s0::step0 (count0 items), s1::step1, ...] over the dims from first_dim."""
    spec = [(None, None, None)] * first_dim
    for s, c, st in zip(starts, count, step):
        spec.append((s, s + (c - 1) * st + 1, st))
    return x[tuple(slice(*t) for t in spec)]


# ---- linear / embedding --------------------------------------------------------------------
def linear(x, weight, bias=None):
    if torch._graph_recording and getattr(x, "_zipp_graph", False):
        return x.linear(weight, bias)
    # matmul(x, weight.T) + bias, without copying the weight's transpose.
    return torch._linear(x, weight, bias)


def bilinear(input1, input2, weight, bias=None):
    lead = tuple(input1.shape[:-1])
    a = input1.reshape(-1, input1.shape[-1])
    b = input2.reshape(-1, input2.shape[-1])
    # (o, n, j) = a @ W[o]; then contract j with b.
    out = (torch.matmul(a.unsqueeze(0), weight) * b.unsqueeze(0)).sum(-1).transpose(0, 1)
    out = out.reshape(*(lead + (weight.shape[0],)))
    return out if bias is None else out + bias


def _check_indices(idx, n):
    if idx.dtype.is_floating_point:
        raise RuntimeError("Expected tensor for argument #1 'indices' to have one of the following scalar types: Long, Int; but got torch.FloatTensor instead")
    if idx.numel() and (idx.min().item() < 0 or idx.max().item() >= n):
        raise IndexError("index out of range in self")


def _embedding_renorm(weight, idx, max_norm, norm_type):
    # Rows looked up with a norm above max_norm are rescaled in place, as PyTorch does.
    with torch.no_grad():
        rows = sorted(set(int(v) for v in idx.reshape(-1).tolist()))
        for r in rows:
            norm = _safe_norm(weight[r], norm_type).item()
            if norm > max_norm:
                weight[r] = weight[r] * (max_norm / (norm + 1e-7))


def embedding(input, weight, padding_idx=None, max_norm=None, norm_type=2.0, scale_grad_by_freq=False, sparse=False):
    idx = input
    _check_indices(idx, weight.shape[0])
    if padding_idx is not None:
        if padding_idx > 0 and padding_idx >= weight.shape[0]:
            raise AssertionError("Padding_idx must be within num_embeddings")
        if padding_idx < 0:
            if padding_idx < -weight.shape[0]:
                raise AssertionError("Padding_idx must be within num_embeddings")
            padding_idx += weight.shape[0]
    if max_norm is not None:
        _embedding_renorm(weight, idx, max_norm, norm_type)
    flat = idx.reshape(-1)
    rows = torch.index_select(weight, 0, flat)
    if padding_idx is not None and torch.is_grad_enabled() and weight.requires_grad:
        # The padding row is looked up but receives no gradient.
        rows = torch.where((flat != padding_idx).unsqueeze(1), rows, rows.detach())
    if scale_grad_by_freq and torch.is_grad_enabled() and weight.requires_grad:
        # Each row's gradient is divided by how often its index occurs in the batch.
        counts = {}
        for v in flat.tolist():
            counts[v] = counts.get(v, 0) + 1
        scale = torch.tensor([1.0 / counts[v] for v in flat.tolist()], dtype=weight.dtype).unsqueeze(1)
        rows = rows * scale + (rows * (1 - scale)).detach()
    return rows.reshape(*(tuple(idx.shape) + (weight.shape[1],)))


def embedding_bag(input, weight, offsets=None, max_norm=None, norm_type=2, scale_grad_by_freq=False, mode="mean", sparse=False, per_sample_weights=None, include_last_offset=False, padding_idx=None):
    if mode not in ("sum", "mean", "max"):
        raise ValueError("mode has to be one of sum, mean or max")
    if per_sample_weights is not None and mode != "sum":
        raise NotImplementedError("embedding_bag: per_sample_weights was not None. per_sample_weights is only supported for mode='sum' (got mode='%s'). Please open a feature request on GitHub." % mode)
    if padding_idx is not None and padding_idx < 0:
        padding_idx += weight.shape[0]
    if input.dim() == 2:
        if offsets is not None:
            raise ValueError("if input is 2D, then offsets has to be None, as input is treated is a mini-batch of fixed length sequences. However, found offsets of type Tensor")
        b, n = input.shape
        bags = [(i * n, n) for i in range(b)]
        flat = input.reshape(-1)
        psw = None if per_sample_weights is None else per_sample_weights.reshape(-1)
    elif input.dim() == 1:
        if offsets is None:
            raise ValueError("offsets has to be a 1D Tensor but got None")
        starts = [int(v) for v in offsets.tolist()]
        total = input.shape[0]
        if include_last_offset:
            ends = starts[1:]
            starts = starts[:-1]
        else:
            ends = starts[1:] + [total]
        bags = [(s, e - s) for s, e in zip(starts, ends)]
        flat = input
        psw = per_sample_weights
    else:
        raise ValueError("input has to be 1D or 2D Tensor, but got Tensor of dimension %d" % input.dim())
    _check_indices(flat, weight.shape[0])
    if max_norm is not None:
        _embedding_renorm(weight, flat, max_norm, norm_type)
    rows = torch.index_select(weight, 0, flat)
    if psw is not None:
        rows = rows * psw.unsqueeze(1)
    keep_all = padding_idx is None
    values = flat.tolist()
    out = []
    for start, length in bags:
        members = [i for i in range(start, start + length) if keep_all or values[i] != padding_idx]
        if not members:
            out.append(torch.zeros(weight.shape[1], dtype=weight.dtype))
            continue
        if members == list(range(start, start + length)):
            bag = rows.narrow(0, start, length)
        else:
            bag = torch.index_select(rows, 0, torch.tensor(members, dtype=torch.int64))
        if mode == "sum":
            out.append(bag.sum(0))
        elif mode == "mean":
            out.append(bag.sum(0) / len(members))
        else:
            out.append(bag.max(0)[0])
    return torch.stack(out, 0)


def one_hot(idx, num_classes=-1):
    if idx.dtype.is_floating_point:
        raise RuntimeError("one_hot is only applicable to index tensor.")
    if num_classes < 0:
        num_classes = int(idx.max().item()) + 1 if idx.numel() else 0
    return torch.one_hot_(idx, int(num_classes))


# ---- padding -------------------------------------------------------------------------------
def pad(input, pad, mode="constant", value=None):
    x = input
    padding = [int(p) for p in pad]
    if len(padding) % 2:
        raise ValueError("Padding length must be divisible by 2")
    ndim = len(x.shape)
    if len(padding) // 2 > ndim:
        raise ValueError("Padding length should be less than or equal to two times the input dimension but got padding length %d and input of dimension %d" % (len(padding), ndim))
    if mode not in ("constant", "reflect", "replicate", "circular"):
        raise NotImplementedError("Unrecognised padding mode " + str(mode))
    if mode != "constant":
        if value is not None and value != 0:
            raise ValueError("Padding mode \"%s\" doesn't take in value argument" % mode)
        if len(padding) // 2 > ndim - 1:
            raise NotImplementedError("Only 2D, 3D, 4D, 5D padding with non-constant padding are supported for now")
    fill = 0.0 if value is None else float(value)
    out = x
    for i in range(0, len(padding), 2):
        left, right = padding[i], padding[i + 1]
        dim = ndim - 1 - i // 2
        if left == 0 and right == 0:
            continue
        # Negative padding crops.
        if left < 0:
            out = out.narrow(dim, -left, out.shape[dim] + left)
            left = 0
        if right < 0:
            out = out.narrow(dim, 0, out.shape[dim] + right)
            right = 0
        if left == 0 and right == 0:
            continue
        if mode == "constant":
            out = _pad_dim(out, dim, left, right, fill)
        else:
            out = _pad_mode_dim(out, dim, left, right, mode)
    return out


def _pad_dim(x, dim, left, right, value):
    if dim == len(x.shape) - 1:
        storage, shape = _k.pad_last(x._s, x.shape, left, right, value)
        out = Tensor(storage, shape, x.dtype)
        if torch._needs_grad(x):
            out.requires_grad = True
            out._node = torch._Node(lambda g: (g.narrow(dim, left, x.shape[dim]),), (x,), "ConstantPadNd")
            out._node.diff = True
        return out
    moved = torch.movedim(x, dim, -1)
    return torch.movedim(_pad_dim(moved, len(x.shape) - 1, left, right, value), -1, dim)


def _pad_mode_dim(x, dim, left, right, mode):
    n = x.shape[dim]
    parts = []
    if mode == "reflect":
        if left >= n or right >= n:
            raise RuntimeError("Argument #4: Padding size should be less than the corresponding input dimension, but got: padding (%d, %d) at dimension %d of input %s" % (left, right, dim, tuple(x.shape)))
        if left:
            parts.append(torch.flip(x.narrow(dim, 1, left), [dim]))
        parts.append(x)
        if right:
            parts.append(torch.flip(x.narrow(dim, n - 1 - right, right), [dim]))
    elif mode == "replicate":
        if n == 0:
            raise RuntimeError("padding of an empty dimension is not supported in replicate mode")
        if left:
            parts.append(torch.index_select(x, dim, torch.zeros(left, dtype=torch.int64)))
        parts.append(x)
        if right:
            parts.append(torch.index_select(x, dim, torch.full((right,), n - 1, dtype=torch.int64)))
    else:
        if left > n or right > n:
            raise RuntimeError("Padding value causes wrapping around more than once.")
        if left:
            parts.append(x.narrow(dim, n - left, left))
        parts.append(x)
        if right:
            parts.append(x.narrow(dim, 0, right))
    return torch.cat(parts, dim)


# ---- convolution ---------------------------------------------------------------------------
def _conv_pair(value, name, minimum=1):
    pair = (value, value) if isinstance(value, int) else tuple(value) if isinstance(value, (tuple, list)) else ()
    if len(pair) != 2 or any(isinstance(v, bool) or not isinstance(v, int) or v < minimum for v in pair):
        raise ValueError(name + " must be an integer or pair of integers >= " + str(minimum))
    return pair


def _same_padding(x, weight, dilation, stride, nd):
    """PyTorch's padding='same': symmetric padding, plus one extra on the
    right of a dimension whose total padding is odd."""
    if any(s != 1 for s in stride):
        raise RuntimeError("padding='same' is not supported for strided convolutions")
    ks = weight.shape[2:]
    pads, extra = [], []
    for i in range(nd):
        total = dilation[i] * (ks[i] - 1)
        pads.append(total // 2)
        extra.append(total - 2 * (total // 2))
    if any(extra):
        spec = []
        for i in reversed(range(nd)):
            spec += [0, extra[i]]
        x = pad(x, spec)
    return x, tuple(pads)


def conv1d(x, weight, bias=None, stride=1, padding=0, dilation=1, groups=1):
    stride1 = _ntuple(stride, 1, "stride")
    dilation1 = _ntuple(dilation, 1, "dilation")
    if isinstance(padding, str):
        if padding == "valid":
            padding1 = (0,)
        elif padding == "same":
            x, padding1 = _same_padding(x, weight, dilation1, stride1, 1)
        else:
            raise ValueError("Invalid padding string '%s'" % padding)
    else:
        padding1 = _ntuple(padding, 1, "padding")
    if len(x.shape) == 2:
        return conv1d(x.unsqueeze(0), weight, bias, stride1, padding1, dilation1, groups).squeeze(0)
    if len(x.shape) != 3 or len(weight.shape) != 3:
        raise RuntimeError("conv1d expects CL or NCL input and OIL weights")
    if stride1 != (1,) or dilation1 != (1,) or groups != 1:
        # The general case runs as a height-1 conv2d.
        out = conv2d(x.unsqueeze(2), weight.unsqueeze(2), bias, (1, stride1[0]), (0, padding1[0]), (1, dilation1[0]), groups)
        return out.squeeze(2)
    if padding1[0]:
        x = pad(x, (padding1[0], padding1[0]))
    storage, shape = _k.conv1d(x._s, x.shape, weight._s, weight.shape, None if bias is None else bias._s)
    out = Tensor(storage, shape, x.dtype)
    if torch._needs_grad(x, weight, bias):
        def backward(g):
            if torch._grad_enabled:
                # create_graph: the same gradients as differentiable ops.
                gx, gw, gb = _conv2d_grads(x.unsqueeze(2), weight.unsqueeze(2), bias, g.unsqueeze(2), (1, 1), (0, 0), (1, 1), 1, x.requires_grad, weight.requires_grad)
                return (None if gx is None else gx.squeeze(2), None if gw is None else gw.squeeze(2), gb)
            gx, gw, gb = _k.conv1d_backward(x._s, x.shape, weight._s, weight.shape, g._s)
            return (Tensor(gx, x.shape, x.dtype), Tensor(gw, weight.shape, weight.dtype), Tensor(gb, (weight.shape[0],), weight.dtype))
        out.requires_grad = True
        out._node = torch._Node(backward, (x, weight, bias), "Conv1d")
        out._node.diff = True
    return out


def conv2d(x, weight, bias=None, stride=1, padding=0, dilation=1, groups=1):
    if torch._graph_recording and getattr(x, "_zipp_graph", False):
        raise NotImplementedError("conv2d currently supports eager CPU tensors only")
    stride = _conv_pair(stride, "stride")
    dilation = _conv_pair(dilation, "dilation")
    if isinstance(padding, str):
        if padding == "valid":
            padding = (0, 0)
        elif padding == "same":
            if len(x.shape) == 3:
                return conv2d(x.unsqueeze(0), weight, bias, stride, padding, dilation, groups).squeeze(0)
            x, padding = _same_padding(x, weight, dilation, stride, 2)
        else:
            raise ValueError("Invalid padding string '%s'" % padding)
    padding = _conv_pair(padding, "padding", 0)
    if isinstance(groups, bool) or not isinstance(groups, int) or groups < 1:
        raise ValueError("groups must be a positive integer")
    if len(x.shape) == 3:
        return conv2d(x.unsqueeze(0), weight, bias, stride, padding, dilation, groups).squeeze(0)
    if len(x.shape) != 4 or len(weight.shape) != 4:
        raise RuntimeError("conv2d expects CHW or NCHW input and OIHW weights")
    if bias is not None and tuple(bias.shape) != (weight.shape[0],):
        raise RuntimeError("conv2d bias must match the output channels")
    storage, shape = _k.conv2d(x._s, x.shape, weight._s, weight.shape, None if bias is None else bias._s, stride, padding, dilation, groups)
    out = Tensor(storage, shape, x.dtype)
    if torch._needs_grad(x, weight, bias):
        def backward(g):
            if torch._grad_enabled:
                # create_graph: the same gradients as differentiable ops.
                return _conv2d_grads(x, weight, bias, g, stride, padding, dilation, groups, x.requires_grad, weight.requires_grad)
            gx, gw, gb = _k.conv2d_backward(x._s, x.shape, weight._s, weight.shape, g._s, stride, padding, dilation, groups)
            return (Tensor(gx, x.shape, x.dtype), Tensor(gw, weight.shape, weight.dtype), None if bias is None else Tensor(gb, bias.shape, bias.dtype))
        out.requires_grad = True
        out._node = torch._Node(backward, (x, weight, bias), "Conv2d")
        out._node.diff = True
    return out


def _conv2d_weight_grad(x, g, kernel, stride, padding, dilation, groups):
    """The weight gradient of conv2d(x, w) for the output gradient g, as a
    batched matmul of g with the unfolded input (differentiable in both)."""
    n, cin = x.shape[0], x.shape[1]
    cout = g.shape[1]
    views, outs = _patches(x, kernel, dilation, padding, stride, 0.0, "conv2d")
    cg, og, taps = cin // groups, cout // groups, len(views)
    length = outs[0] * outs[1]
    cols = torch.stack(views, 2).reshape(n, groups, cg * taps, length)
    gw = torch.matmul(g.reshape(n, groups, og, length), cols.transpose(-1, -2)).sum(0)
    return gw.reshape(cout, cg, kernel[0], kernel[1])


def _conv2d_grads(x, weight, bias, g, stride, padding, dilation, groups, need_x, need_w):
    """conv2d's input, weight and bias gradients built from differentiable
    operations, for backward(create_graph=True): the input gradient is the
    transposed convolution of g (output_padding restores the rows a stride
    skipped), the weight gradient `_conv2d_weight_grad`. Without
    create_graph the native backward kernel runs instead."""
    gx = gw = gb = None
    kh, kw = weight.shape[2], weight.shape[3]
    if need_x:
        oph = x.shape[2] + 2 * padding[0] - dilation[0] * (kh - 1) - 1 - (g.shape[2] - 1) * stride[0]
        opw = x.shape[3] + 2 * padding[1] - dilation[1] * (kw - 1) - 1 - (g.shape[3] - 1) * stride[1]
        gx = conv_transpose2d(g, weight, None, stride, padding, (oph, opw), groups, dilation)
    if need_w:
        gw = _conv2d_weight_grad(x, g, (kh, kw), stride, padding, dilation, groups)
    if bias is not None and bias.requires_grad:
        gb = g.sum((0, 2, 3))
    return (gx, gw, gb)


def conv_transpose2d(input, weight, bias=None, stride=1, padding=0, output_padding=0, groups=1, dilation=1):
    """The adjoint of conv2d: the input gradient of a conv2d whose weight is
    `weight` (in_channels, out_channels / groups, kH, kW)."""
    x = input
    if len(x.shape) == 3:
        return conv_transpose2d(x.unsqueeze(0), weight, bias, stride, padding, output_padding, groups, dilation).squeeze(0)
    if len(x.shape) != 4 or len(weight.shape) != 4:
        raise RuntimeError("conv_transpose2d expects CHW or NCHW input and (in, out/groups, kH, kW) weights")
    stride = _conv_pair(stride, "stride")
    padding = _conv_pair(padding, "padding", 0)
    output_padding = _conv_pair(output_padding, "output_padding", 0)
    dilation = _conv_pair(dilation, "dilation")
    if isinstance(groups, bool) or not isinstance(groups, int) or groups < 1:
        raise ValueError("groups must be a positive integer")
    for op, s, d in zip(output_padding, stride, dilation):
        if op >= s and op >= d:
            raise RuntimeError("output padding must be smaller than either stride or dilation, but got output_padding_height: %d output_padding_width: %d stride_height: %d stride_width: %d dilation_height: %d dilation_width: %d" % (output_padding[0], output_padding[1], stride[0], stride[1], dilation[0], dilation[1]))
    n, cin, h, w = x.shape
    if weight.shape[0] != cin:
        raise RuntimeError("Given transposed=1, weight of size %s, expected input%s to have %d channels, but got %d channels instead" % (tuple(weight.shape), tuple(x.shape), weight.shape[0], cin))
    cout = weight.shape[1] * groups
    kh, kw = weight.shape[2], weight.shape[3]
    ho = (h - 1) * stride[0] - 2 * padding[0] + dilation[0] * (kh - 1) + output_padding[0] + 1
    wo = (w - 1) * stride[1] - 2 * padding[1] + dilation[1] * (kw - 1) + output_padding[1] + 1
    if ho <= 0 or wo <= 0:
        raise RuntimeError("Given input size per channel: (%d x %d). Calculated output size per channel: (%d x %d). Output size is too small" % (h, w, ho, wo))
    out_shape = (n, cout, ho, wo)
    # The conv2d this is the adjoint of, run over an output-sized input, has
    # one position more per dimension where output_padding >= stride (legal
    # when the dilation is larger): x is extended by zeros there, and the
    # input gradient cropped back.
    hc = (ho + 2 * padding[0] - dilation[0] * (kh - 1) - 1) // stride[0] + 1
    wc = (wo + 2 * padding[1] - dilation[1] * (kw - 1) - 1) // stride[1] + 1
    xk = x.detach() if (hc, wc) == (h, w) else pad(x.detach(), (0, wc - w, 0, hc - h))
    zeros = torch.zeros(*out_shape, dtype=x.dtype)
    gx, _, _ = _k.conv2d_backward(zeros._s, zeros.shape, weight._s, weight.shape, xk._s, stride, padding, dilation, groups)
    out = Tensor(gx, out_shape, x.dtype)
    if bias is not None:
        with torch.no_grad():
            out = out + bias.detach().reshape(1, cout, 1, 1)
    if torch._needs_grad(x, weight, bias):
        def backward(g):
            if torch._grad_enabled:
                # create_graph: differentiable in g, x and weight.
                gin = gw = gb = None
                if x.requires_grad:
                    gin = conv2d(g, weight, None, stride, padding, dilation, groups)
                    if (hc, wc) != (h, w):
                        gin = gin[:, :, :h, :w]
                if weight.requires_grad:
                    xg = x if (hc, wc) == (h, w) else pad(x, (0, wc - w, 0, hc - h))
                    gw = _conv2d_weight_grad(g, xg, (kh, kw), stride, padding, dilation, groups)
                if bias is not None and bias.requires_grad:
                    gb = g.sum((0, 2, 3))
                return (gin, gw, gb)
            gin = conv2d(g, weight.detach(), None, stride, padding, dilation, groups)
            if (hc, wc) != (h, w):
                gin = gin[:, :, :h, :w]
            _, gw, _ = _k.conv2d_backward(g._s, g.shape, weight._s, weight.shape, xk._s, stride, padding, dilation, groups)
            gb = None if bias is None else g.sum((0, 2, 3))
            return (gin, Tensor(gw, weight.shape, weight.dtype), gb)
        out.requires_grad = True
        out._node = torch._Node(backward, (x, weight, bias), "ConvolutionBackward0")
        out._node.diff = True
    return out


def conv_transpose1d(input, weight, bias=None, stride=1, padding=0, output_padding=0, groups=1, dilation=1):
    x = input
    if len(x.shape) == 2:
        return conv_transpose1d(x.unsqueeze(0), weight, bias, stride, padding, output_padding, groups, dilation).squeeze(0)
    s, p, op, d = _single(stride), _single(padding), _single(output_padding), _single(dilation)
    out = conv_transpose2d(x.unsqueeze(2), weight.unsqueeze(2), bias, (1, s[0]), (0, p[0]), (0, op[0]), groups, (1, d[0]))
    return out.squeeze(2)


def _groups_arg(groups):
    if isinstance(groups, bool) or not isinstance(groups, int) or groups < 1:
        raise ValueError("groups must be a positive integer")
    return groups


def _depth_taps(x, count, start, step, spacing, taps, groups):
    """The `taps` depth slices x[:, :, start + k * spacing :: step] (each
    `count` long) of an N, C, D, H, W tensor, stacked into the channels of
    one N * count, groups * taps * C / groups, H, W batch (group-major, so a
    grouped conv2d sees each group's taps together)."""
    n, c, _, h, w = x.shape
    views = [_strided(x, [start + k * spacing], [count], [step], 2) for k in range(taps)]
    stacked = torch.stack(views, 1).reshape(n, taps, groups, c // groups, count, h, w)
    return stacked.permute(0, 4, 2, 1, 3, 5, 6).reshape(n * count, groups * taps * (c // groups), h, w)


def conv3d(input, weight, bias=None, stride=1, padding=0, dilation=1, groups=1):
    """conv3d as one conv2d call: the kernel's depth taps of every output
    plane are stacked into the input channels (and the weight's depth folded
    into its input channels to match), so the native 2-D kernel computes the
    whole sum, and autograd flows through the stacking."""
    x = input
    if len(x.shape) == 4:
        return conv3d(x.unsqueeze(0), weight, bias, stride, padding, dilation, groups).squeeze(0)
    if len(x.shape) != 5:
        raise RuntimeError("Expected 4D (unbatched) or 5D (batched) input to conv3d, but got input of size: %s" % list(x.shape))
    if len(weight.shape) != 5:
        raise RuntimeError("conv3d expects a 5-D (out, in / groups, kD, kH, kW) weight, got %s" % list(weight.shape))
    groups = _groups_arg(groups)
    s = _ntuple(stride, 3, "stride")
    d = _ntuple(dilation, 3, "dilation")
    if isinstance(padding, str):
        if padding == "valid":
            p = (0, 0, 0)
        elif padding == "same":
            x, p = _same_padding(x, weight, d, s, 3)
        else:
            raise ValueError("Invalid padding string '%s'" % padding)
    else:
        p = _ntuple(padding, 3, "padding")
    n, cin = x.shape[0], x.shape[1]
    cout, cg, kd = weight.shape[0], weight.shape[1], weight.shape[2]
    if cin != cg * groups:
        raise RuntimeError("Given groups=%d, weight of size %s, expected input%s to have %d channels, but got %d channels instead" % (groups, list(weight.shape), list(input.shape), cg * groups, cin))
    if bias is not None and tuple(bias.shape) != (cout,):
        raise RuntimeError("conv3d bias must match the output channels")
    depth = x.shape[2] + 2 * p[0]
    do = (depth - d[0] * (kd - 1) - 1) // s[0] + 1
    if do <= 0:
        raise RuntimeError("Calculated padded input size per channel: (%d x %d x %d). Kernel size: (%d x %d x %d). Kernel size can't be greater than actual input size" % (depth, x.shape[3] + 2 * p[1], x.shape[4] + 2 * p[2], kd, weight.shape[3], weight.shape[4]))
    if p[0]:
        x = pad(x, (0, 0, 0, 0, p[0], p[0]))
    planes = _depth_taps(x, do, 0, s[0], d[0], kd, groups)
    w2 = weight.permute(0, 2, 1, 3, 4).reshape(cout, kd * cg, weight.shape[3], weight.shape[4])
    out = conv2d(planes, w2, bias, s[1:], p[1:], d[1:], groups)
    return out.reshape(n, do, cout, out.shape[2], out.shape[3]).permute(0, 2, 1, 3, 4)


def conv_transpose3d(input, weight, bias=None, stride=1, padding=0, output_padding=0, groups=1, dilation=1):
    """The adjoint of conv3d, composed from one conv_transpose2d call: along
    depth it is an ordinary convolution of the stride-dilated (zero-inserted)
    input with the depth-flipped kernel, whose taps are stacked into the
    channels as in conv3d."""
    x = input
    if len(x.shape) == 4:
        return conv_transpose3d(x.unsqueeze(0), weight, bias, stride, padding, output_padding, groups, dilation).squeeze(0)
    if len(x.shape) != 5 or len(weight.shape) != 5:
        raise RuntimeError("Expected 4D (unbatched) or 5D (batched) input to conv_transpose3d, but got input of size: %s" % list(x.shape))
    groups = _groups_arg(groups)
    s = _ntuple(stride, 3, "stride")
    p = _ntuple(padding, 3, "padding")
    op = _ntuple(output_padding, 3, "output_padding")
    d = _ntuple(dilation, 3, "dilation")
    for i in range(3):
        if op[i] >= s[i] and op[i] >= d[i]:
            raise RuntimeError("output padding must be smaller than either stride or dilation, but got output_padding_depth: %d output_padding_height: %d output_padding_width: %d stride_depth: %d stride_height: %d stride_width: %d dilation_depth: %d dilation_height: %d dilation_width: %d" % (op + s + d))
    n, cin, depth = x.shape[0], x.shape[1], x.shape[2]
    if weight.shape[0] != cin:
        raise RuntimeError("Given transposed=1, weight of size %s, expected input%s to have %d channels, but got %d channels instead" % (list(weight.shape), list(x.shape), weight.shape[0], cin))
    cog, kd = weight.shape[1], weight.shape[2]
    cout = cog * groups
    do = (depth - 1) * s[0] - 2 * p[0] + d[0] * (kd - 1) + op[0] + 1
    if do <= 0:
        raise RuntimeError("Given input size per channel: (%d x %d x %d). Calculated output size per channel is too small" % tuple(x.shape[2:]))
    up = x
    if s[0] > 1:
        h, w = x.shape[3], x.shape[4]
        zeros = torch.zeros(n, cin, depth, s[0] - 1, h, w, dtype=x.dtype)
        up = torch.cat([x.unsqueeze(3), zeros], 3).reshape(n, cin, depth * s[0], h, w).narrow(2, 0, (depth - 1) * s[0] + 1)
    edge = d[0] * (kd - 1)
    if edge or op[0]:
        up = pad(up, (0, 0, 0, 0, edge, edge + op[0]))
    planes = _depth_taps(up, do, p[0], 1, d[0], kd, groups)
    cig = cin // groups
    kh, kw = weight.shape[3], weight.shape[4]
    wf = weight.flip(2).reshape(groups, cig, cog, kd, kh, kw).permute(0, 3, 1, 2, 4, 5).reshape(groups * kd * cig, cog, kh, kw)
    out = conv_transpose2d(planes, wf, bias, s[1:], p[1:], op[1:], groups, d[1:])
    return out.reshape(n, do, cout, out.shape[2], out.shape[3]).permute(0, 2, 1, 3, 4)


def _patches(x, kernel, dilation, padding, stride, fill, name):
    """The kernel offsets' strided views of the padded N, C, *spatial input:
    a list (row-major over the kernel) of N, C, *out tensors, and the output size."""
    nd = len(kernel)
    spatial = x.shape[2:]
    outs = []
    for i in range(nd):
        o = (spatial[i] + 2 * padding[i] - dilation[i] * (kernel[i] - 1) - 1) // stride[i] + 1
        if o <= 0:
            raise RuntimeError("%s: calculated output size %d is too small for input size %s" % (name, o, tuple(spatial)))
        outs.append(o)
    spec = []
    for i in reversed(range(nd)):
        spec += [padding[i], padding[i]]
    xp = pad(x, spec, value=fill) if any(padding) else x
    views = []
    for flat in range(_prod(kernel)):
        offs, rem = [], flat
        for k in reversed(kernel):
            offs.append(rem % k)
            rem //= k
        offs.reverse()
        views.append(_strided(xp, [o * d for o, d in zip(offs, dilation)], outs, stride, 2))
    return views, outs


def _prod(values):
    out = 1
    for v in values:
        out *= v
    return out


def unfold(input, kernel_size, dilation=1, padding=0, stride=1):
    x = input
    unbatched = len(x.shape) == 3
    if unbatched:
        x = x.unsqueeze(0)
    if len(x.shape) != 4:
        raise RuntimeError("Input Error: Only 3D or 4D input Tensors are supported (got %dD)" % len(input.shape))
    k, d, p, s = _pair(kernel_size), _pair(dilation), _pair(padding), _pair(stride)
    views, outs = _patches(x, k, d, p, s, 0.0, "unfold")
    n, c = x.shape[0], x.shape[1]
    cols = torch.stack(views, 2).reshape(n, c * len(views), outs[0] * outs[1])
    return cols.squeeze(0) if unbatched else cols


def fold(input, output_size, kernel_size, dilation=1, padding=0, stride=1):
    x = input
    unbatched = len(x.shape) == 2
    if unbatched:
        x = x.unsqueeze(0)
    if len(x.shape) != 3:
        raise RuntimeError("Input Error: Only unbatched (2D) or batched (3D) input Tensors are supported (got %dD)" % len(input.shape))
    size, k, d, p, s = _pair(output_size), _pair(kernel_size), _pair(dilation), _pair(padding), _pair(stride)
    kk = k[0] * k[1]
    n, ckk, length = x.shape
    if ckk % kk:
        raise RuntimeError("Expected size of input's dimension 1 to be divisible by the product of kernel_size, but got input.size(1)=%d and kernel_size=%s" % (ckk, k))
    c = ckk // kk
    outs = [(size[i] + 2 * p[i] - d[i] * (k[i] - 1) - 1) // s[i] + 1 for i in range(2)]
    if outs[0] * outs[1] != length:
        raise RuntimeError("Given output_size=%s, kernel_size=%s, dilation=%s, padding=%s, stride=%s, expected size of input's dimension 2 to match the calculated number of sliding blocks %d * %d = %d, but got input.size(2)=%d." % (size, k, d, p, s, outs[0], outs[1], outs[0] * outs[1], length))
    cols = x.reshape(n, c, kk, outs[0], outs[1])
    full = (n, c, size[0] + 2 * p[0], size[1] + 2 * p[1])
    total = None
    for flat, piece in enumerate(_unbind(cols, 2)):
        oi, oj = flat // k[1], flat % k[1]
        spec = [(None, None, None), (None, None, None)]
        spec.append((oi * d[0], oi * d[0] + (outs[0] - 1) * s[0] + 1, s[0]))
        spec.append((oj * d[1], oj * d[1] + (outs[1] - 1) * s[1] + 1, s[1]))
        placed = _embed_slice(piece, full, spec)
        total = placed if total is None else total + placed
    out = total[:, :, p[0]:p[0] + size[0], p[1]:p[1] + size[1]]
    return out.squeeze(0) if unbatched else out


# ---- pooling -------------------------------------------------------------------------------
def _pool_out(size, k, s, p, d, ceil_mode):
    num = size + 2 * p - d * (k - 1) - 1 + (s - 1 if ceil_mode else 0)
    out = num // s + 1
    if ceil_mode and (out - 1) * s >= size + p:
        out -= 1
    return out


def _pool_input(x, nd, name):
    if len(x.shape) == nd + 1:
        return x.unsqueeze(0), True
    if len(x.shape) != nd + 2:
        raise RuntimeError("%s: Expected %dD or %dD input tensor, but got %s" % (name, nd + 1, nd + 2, tuple(x.shape)))
    return x, False


def _max_pool(x, nd, kernel_size, stride, padding, dilation, ceil_mode, return_indices, name):
    x, unbatched = _pool_input(x, nd, name)
    k = _ntuple(kernel_size, nd, "kernel_size")
    s = k if stride is None or (isinstance(stride, (list, tuple)) and len(stride) == 0) else _ntuple(stride, nd, "stride")
    p = _ntuple(padding, nd, "padding")
    d = _ntuple(dilation, nd, "dilation")
    for i in range(nd):
        if p[i] > (d[i] * (k[i] - 1) + 1) // 2:
            raise RuntimeError("pad should be at most half of effective kernel size, but got pad=%d, kernel_size=%d and dilation=%d" % (p[i], k[i], d[i]))
    spatial = x.shape[2:]
    outs = [_pool_out(spatial[i], k[i], s[i], p[i], d[i], ceil_mode) for i in range(nd)]
    if any(o <= 0 for o in outs):
        raise RuntimeError("Given input size: %s. Calculated output size: %s. Output size is too small" % (tuple(x.shape[1:]), tuple([x.shape[1]] + outs)))
    def stacked(x):
        # The -inf padded input's kh * kw strided views, stacked: max(-1).
        spec = []
        for i in reversed(range(nd)):
            right = max(0, (outs[i] - 1) * s[i] + d[i] * (k[i] - 1) + 1 - spatial[i] - p[i])
            spec += [p[i], right]
        xp = pad(x, spec, value=-_inf) if any(spec) else x
        views = []
        for flat in range(_prod(k)):
            offs, rem = [], flat
            for kk in reversed(k):
                offs.append(rem % kk)
                rem //= kk
            offs.reverse()
            views.append(_strided(xp, [o * dd for o, dd in zip(offs, d)], outs, s, 2))
        if len(views) == 1:
            return views[0], torch.zeros(*views[0].shape, dtype=torch.int64)
        return torch.stack(views, -1).max(-1)
    # 2-d windows that cannot overlap run as one kernel with the same values
    # and gradients (see torch._max_pool2d).
    native = torch._max_pool2d(x, k, s, p, d, outs, lambda xd: stacked(xd)[0]) if nd == 2 else None
    values, which = native if native is not None else stacked(x)
    if unbatched:
        values = values.squeeze(0)
    if not return_indices:
        return values
    # The flat index of each maximum within the unpadded input plane.
    rank = len(which.shape)
    index = None
    rem = which
    for i in reversed(range(nd)):
        ki = rem % k[i]
        rem = rem // k[i]
        view = [1] * rank
        view[rank - nd + i] = outs[i]
        base = torch.arange(outs[i]).reshape(*view) * s[i] - p[i]
        pos = base + ki * d[i]
        stride_i = _prod(spatial[i + 1:])
        index = pos * stride_i if index is None else index + pos * stride_i
    if unbatched:
        index = index.squeeze(0)
    return values, index


def max_pool1d(input, kernel_size, stride=None, padding=0, dilation=1, ceil_mode=False, return_indices=False):
    return _max_pool(input, 1, kernel_size, stride, padding, dilation, ceil_mode, return_indices, "max_pool1d")


def max_pool2d(input, kernel_size, stride=None, padding=0, dilation=1, ceil_mode=False, return_indices=False):
    return _max_pool(input, 2, kernel_size, stride, padding, dilation, ceil_mode, return_indices, "max_pool2d")


def max_pool3d(input, kernel_size, stride=None, padding=0, dilation=1, ceil_mode=False, return_indices=False):
    return _max_pool(input, 3, kernel_size, stride, padding, dilation, ceil_mode, return_indices, "max_pool3d")


def max_pool3d_with_indices(input, kernel_size, stride=None, padding=0, dilation=1, ceil_mode=False, return_indices=True):
    return _max_pool(input, 3, kernel_size, stride, padding, dilation, ceil_mode, True, "max_pool3d")


def max_pool1d_with_indices(input, kernel_size, stride=None, padding=0, dilation=1, ceil_mode=False, return_indices=True):
    return _max_pool(input, 1, kernel_size, stride, padding, dilation, ceil_mode, True, "max_pool1d")


def max_pool2d_with_indices(input, kernel_size, stride=None, padding=0, dilation=1, ceil_mode=False, return_indices=True):
    return _max_pool(input, 2, kernel_size, stride, padding, dilation, ceil_mode, True, "max_pool2d")


def _avg_pool(x, nd, kernel_size, stride, padding, ceil_mode, count_include_pad, divisor_override, name):
    x, unbatched = _pool_input(x, nd, name)
    k = _ntuple(kernel_size, nd, "kernel_size")
    s = k if stride is None or (isinstance(stride, (list, tuple)) and len(stride) == 0) else _ntuple(stride, nd, "stride")
    p = _ntuple(padding, nd, "padding")
    for i in range(nd):
        if p[i] > k[i] // 2:
            raise RuntimeError("pad should be at most half of effective kernel size, but got pad=%d, kernel_size=%d and dilation=1" % (p[i], k[i]))
    spatial = x.shape[2:]
    outs = [_pool_out(spatial[i], k[i], s[i], p[i], 1, ceil_mode) for i in range(nd)]
    if any(o <= 0 for o in outs):
        raise RuntimeError("Given input size: %s. Calculated output size: %s. Output size is too small" % (tuple(x.shape[1:]), tuple([x.shape[1]] + outs)))
    spec = []
    for i in reversed(range(nd)):
        right = max(0, (outs[i] - 1) * s[i] + k[i] - spatial[i] - p[i])
        spec += [p[i], right]
    xp = pad(x, spec) if any(spec) else x
    total = None
    for flat in range(_prod(k)):
        offs, rem = [], flat
        for kk in reversed(k):
            offs.append(rem % kk)
            rem //= kk
        offs.reverse()
        view = _strided(xp, offs, outs, s, 2)
        total = view if total is None else total + view
    if divisor_override:
        out = total / divisor_override
    else:
        # PyTorch's divisor: the window clipped to the padded input (with
        # count_include_pad) or to the input itself (without).
        counts = []
        for i in range(nd):
            c = []
            for o in range(outs[i]):
                start = o * s[i] - p[i]
                end = min(start + k[i], spatial[i] + p[i])
                size = end - start
                if not count_include_pad:
                    size = min(end, spatial[i]) - max(start, 0)
                c.append(size)
            counts.append(c)
        if all(len(set(c)) == 1 for c in counts):
            out = total / _prod([c[0] for c in counts])
        else:
            div = None
            for i in range(nd):
                view = [1] * nd
                view[i] = outs[i]
                t = torch.tensor(counts[i], dtype=x.dtype).reshape(*view)
                div = t if div is None else div * t
            out = total / div
    return out.squeeze(0) if unbatched else out


def avg_pool1d(input, kernel_size, stride=None, padding=0, ceil_mode=False, count_include_pad=True):
    return _avg_pool(input, 1, kernel_size, stride, padding, ceil_mode, count_include_pad, None, "avg_pool1d")


def avg_pool2d(input, kernel_size, stride=None, padding=0, ceil_mode=False, count_include_pad=True, divisor_override=None):
    return _avg_pool(input, 2, kernel_size, stride, padding, ceil_mode, count_include_pad, divisor_override, "avg_pool2d")


def avg_pool3d(input, kernel_size, stride=None, padding=0, ceil_mode=False, count_include_pad=True, divisor_override=None):
    return _avg_pool(input, 3, kernel_size, stride, padding, ceil_mode, count_include_pad, divisor_override, "avg_pool3d")


def lp_pool1d(input, norm_type, kernel_size, stride=None, ceil_mode=False):
    k = _single(kernel_size)[0]
    out = avg_pool1d(input ** norm_type, k, stride, 0, ceil_mode)
    return (torch.sign(out) * relu(torch.abs(out))) ** (1.0 / norm_type) * (k ** (1.0 / norm_type))


def lp_pool2d(input, norm_type, kernel_size, stride=None, ceil_mode=False):
    kh, kw = _pair(kernel_size)
    out = avg_pool2d(input ** norm_type, (kh, kw), stride, 0, ceil_mode)
    return (torch.sign(out) * relu(torch.abs(out))) ** (1.0 / norm_type) * ((kh * kw) ** (1.0 / norm_type))


def lp_pool3d(input, norm_type, kernel_size, stride=None, ceil_mode=False):
    kd, kh, kw = _ntuple(kernel_size, 3, "kernel_size")
    out = avg_pool3d(input ** norm_type, (kd, kh, kw), stride, 0, ceil_mode)
    return ((torch.sign(out) * relu(torch.abs(out))) * (kd * kh * kw)) ** (1.0 / norm_type)


def _adaptive_sizes(x, nd, output_size):
    out = _ntuple(output_size, nd, "output_size") if not isinstance(output_size, (tuple, list)) or None not in output_size else tuple(x.shape[-nd + i] if v is None else int(v) for i, v in enumerate(output_size))
    return out


def _bins(size, out):
    return [((i * size) // out, -((-(i + 1) * size) // out)) for i in range(out)]


def _adaptive_avg(x, nd, output_size, name):
    x, unbatched = _pool_input(x, nd, name)
    outs = _adaptive_sizes(x, nd, output_size)
    spatial = x.shape[2:]
    if all(spatial[i] % outs[i] == 0 for i in range(nd)):
        k = tuple(spatial[i] // outs[i] for i in range(nd))
        out = _avg_pool(x, nd, k, k, 0, False, True, None, name)
    else:
        # Separable: average each dimension's bins in turn.
        out = x
        for i in range(nd):
            dim = 2 + i
            parts = [out.narrow(dim, a, b - a).sum(dim, keepdim=True) / (b - a) for a, b in _bins(spatial[i], outs[i])]
            out = torch.cat(parts, dim)
    return out.squeeze(0) if unbatched else out


def adaptive_avg_pool1d(input, output_size):
    return _adaptive_avg(input, 1, output_size, "adaptive_avg_pool1d")


def adaptive_avg_pool2d(input, output_size):
    return _adaptive_avg(input, 2, output_size, "adaptive_avg_pool2d")


def _adaptive_max(x, nd, output_size, return_indices, name):
    x, unbatched = _pool_input(x, nd, name)
    outs = _adaptive_sizes(x, nd, output_size)
    spatial = x.shape[2:]
    if all(spatial[i] % outs[i] == 0 for i in range(nd)):
        k = tuple(spatial[i] // outs[i] for i in range(nd))
        res = _max_pool(x, nd, k, k, 0, 1, False, return_indices, name)
    else:
        # Separable, last dimension first; ties keep the first maximum in
        # row-major order, as PyTorch's scan does.
        values, index = x, None
        for i in reversed(range(nd)):
            dim = 2 + i
            vparts, iparts = [], []
            for a, b in _bins(spatial[i], outs[i]):
                v, j = values.narrow(dim, a, b - a).max(dim, keepdim=True)
                vparts.append(v)
                if index is None:
                    iparts.append(j + a)
                else:
                    sub = index.narrow(dim, a, b - a)
                    iparts.append(torch.gather(sub, dim, j) + (j + a) * _prod(spatial[i + 1:]))
            values = torch.cat(vparts, dim)
            index = torch.cat(iparts, dim)
        res = (values, index) if return_indices else values
    if unbatched:
        return tuple(r.squeeze(0) for r in res) if return_indices else res.squeeze(0)
    return res


def adaptive_avg_pool3d(input, output_size):
    return _adaptive_avg(input, 3, output_size, "adaptive_avg_pool3d")


def adaptive_max_pool1d(input, output_size, return_indices=False):
    return _adaptive_max(input, 1, output_size, return_indices, "adaptive_max_pool1d")


def adaptive_max_pool2d(input, output_size, return_indices=False):
    return _adaptive_max(input, 2, output_size, return_indices, "adaptive_max_pool2d")


def adaptive_max_pool1d_with_indices(input, output_size, return_indices=True):
    return _adaptive_max(input, 1, output_size, True, "adaptive_max_pool1d")


def adaptive_max_pool2d_with_indices(input, output_size, return_indices=True):
    return _adaptive_max(input, 2, output_size, True, "adaptive_max_pool2d")


def adaptive_max_pool3d(input, output_size, return_indices=False):
    return _adaptive_max(input, 3, output_size, return_indices, "adaptive_max_pool3d")


def adaptive_max_pool3d_with_indices(input, output_size, return_indices=True):
    return _adaptive_max(input, 3, output_size, True, "adaptive_max_pool3d")


def _max_unpool(input, indices, nd, kernel_size, stride, padding, output_size, name):
    """Scatters each value to its flat index in a zero output plane; the
    gradient gathers from the same indices."""
    k = _ntuple(kernel_size, nd, "kernel_size")
    s = k if stride is None or (isinstance(stride, (list, tuple)) and len(stride) == 0) else _ntuple(stride, nd, "stride")
    p = _ntuple(padding, nd, "padding")
    unbatched = len(input.shape) == nd + 1
    x = input.unsqueeze(0) if unbatched else input
    idx = indices.unsqueeze(0) if unbatched else indices
    spatial = x.shape[2:]
    if output_size is None:
        outs = [(spatial[i] - 1) * s[i] - 2 * p[i] + k[i] for i in range(nd)]
    else:
        outs = [int(v) for v in output_size][-nd:]
        for i in range(nd):
            low = (spatial[i] - 1) * s[i] - 2 * p[i] + k[i]
            if not low - s[i] < outs[i] < low + s[i]:
                raise ValueError("invalid output_size %s (dim %d must be between %d and %d)" % (list(output_size), i, low - s[i], low + s[i]))
    n, c = x.shape[0], x.shape[1]
    plane = _prod(outs)
    flat_idx = idx.reshape(n, c, -1).long()
    if flat_idx.numel() and (int(flat_idx.min().item()) < 0 or int(flat_idx.max().item()) >= plane):
        raise RuntimeError("Found an invalid max index: %d (output volumes are of size %s" % (int(flat_idx.max().item()), "x".join(str(v) for v in outs)))
    flat_x = x.reshape(n, c, -1)
    out = torch.zeros(n, c, plane, dtype=x.dtype).scatter(2, flat_idx, flat_x.detach())
    if torch._needs_grad(flat_x):
        def backward(g):
            return (torch.gather(g, 2, flat_idx),)
        out.requires_grad = True
        out._node = torch._Node(backward, (flat_x,), "MaxUnpool%dDBackward0" % nd)
    out = out.reshape(n, c, *outs)
    return out.squeeze(0) if unbatched else out


def max_unpool1d(input, indices, kernel_size, stride=None, padding=0, output_size=None):
    return _max_unpool(input, indices, 1, kernel_size, stride, padding, output_size, "max_unpool1d")


def max_unpool2d(input, indices, kernel_size, stride=None, padding=0, output_size=None):
    return _max_unpool(input, indices, 2, kernel_size, stride, padding, output_size, "max_unpool2d")


def max_unpool3d(input, indices, kernel_size, stride=None, padding=0, output_size=None):
    return _max_unpool(input, indices, 3, kernel_size, stride, padding, output_size, "max_unpool3d")


# ---- resampling ----------------------------------------------------------------------------
def pixel_shuffle(input, upscale_factor):
    r = int(upscale_factor)
    *lead, c, h, w = input.shape
    if c % (r * r):
        raise RuntimeError("pixel_shuffle expects its input's 'channel' dimension to be divisible by the square of upscale_factor, but input.size(-3)=%d is not divisible by %d" % (c, r * r))
    oc = c // (r * r)
    n = len(lead)
    x = input.reshape(*lead, oc, r, r, h, w)
    x = x.permute(*range(n), n, n + 3, n + 1, n + 4, n + 2)
    return x.reshape(*lead, oc, h * r, w * r)


def pixel_unshuffle(input, downscale_factor):
    r = int(downscale_factor)
    *lead, c, h, w = input.shape
    if h % r or w % r:
        raise RuntimeError("pixel_unshuffle expects height and width to be divisible by downscale_factor, but got %d x %d with %d" % (h, w, r))
    n = len(lead)
    x = input.reshape(*lead, c, h // r, r, w // r, r)
    x = x.permute(*range(n), n, n + 2, n + 4, n + 1, n + 3)
    return x.reshape(*lead, c * r * r, h // r, w // r)


def channel_shuffle(input, groups):
    n, c = input.shape[0], input.shape[1]
    rest = tuple(input.shape[2:])
    return input.reshape(n, groups, c // groups, *rest).transpose(1, 2).reshape(n, c, *rest)


native_channel_shuffle = channel_shuffle


def interpolate(input, size=None, scale_factor=None, mode="nearest", align_corners=None, recompute_scale_factor=None, antialias=False):
    x = input
    nd = len(x.shape) - 2
    if nd < 1:
        raise ValueError("Input Error: Only 3D, 4D and 5D input Tensors supported (got %dD) for the modes: nearest | linear | bilinear | bicubic | trilinear | area | nearest-exact (got %s)" % (len(x.shape), mode))
    if mode in ("nearest", "area", "nearest-exact"):
        if align_corners is not None:
            raise ValueError("align_corners option can only be set with the interpolating modes: linear | bilinear | bicubic | trilinear")
    elif align_corners is None:
        align_corners = False
    if size is not None and scale_factor is not None:
        raise ValueError("only one of size or scale_factor should be defined")
    if size is not None:
        out_sizes = list(size) if isinstance(size, (list, tuple)) else [size] * nd
        if len(out_sizes) != nd:
            raise ValueError("Input and output must have the same number of spatial dimensions, but got input with spatial dimensions of %s and output size of %s. Please provide input tensor in (N, C, d1, d2, ...,dK) format and output size in (o1, o2, ...,oK) format." % (list(x.shape[2:]), out_sizes))
        out_sizes = [int(v) for v in out_sizes]
        scales = [None] * nd
    elif scale_factor is not None:
        scales = [float(v) for v in scale_factor] if isinstance(scale_factor, (list, tuple)) else [float(scale_factor)] * nd
        if len(scales) != nd:
            raise ValueError("Input and scale_factor must have the same number of spatial dimensions")
        out_sizes = [int(math.floor(float(x.shape[2 + i]) * scales[i])) for i in range(nd)]
        if recompute_scale_factor:
            scales = [None] * nd
    else:
        raise ValueError("either size or scale_factor should be defined")
    if antialias and not (mode in ("bilinear", "bicubic") and nd == 2):
        raise ValueError("Anti-alias option is restricted to bilinear and bicubic modes and requires a 4-D tensor as input")
    if mode == "area":
        return _adaptive_avg(x, nd, tuple(out_sizes), "adaptive_avg_pool%dd" % nd)
    if mode == "bicubic" or antialias:
        if nd != 2:
            raise NotImplementedError("Got %dD input, but bicubic mode needs 4D input" % (nd + 2))
        return _separable_resample(x, out_sizes, scales, align_corners, mode == "bicubic", antialias)
    linear_modes = {"linear": 1, "bilinear": 2, "trilinear": 3}
    if mode in linear_modes:
        if linear_modes[mode] != nd:
            raise NotImplementedError("Got %dD input, but %s mode needs %dD input" % (nd + 2, mode, linear_modes[mode] + 2))
        out = x
        for i in range(nd):
            out = _linear_resample(out, 2 + i, out_sizes[i], scales[i], align_corners)
        return out
    if mode in ("nearest", "nearest-exact"):
        out = x
        for i in range(nd):
            n_in, n_out = x.shape[2 + i], out_sizes[i]
            if n_in == n_out:
                continue
            idx = [_nearest_index(o, n_in, n_out, scales[i], mode == "nearest-exact") for o in range(n_out)]
            out = torch.index_select(out, 2 + i, torch.tensor(idx, dtype=torch.int64))
        return out
    raise NotImplementedError("Input Error: interpolate mode %r is not supported on Zipp" % mode)


def upsample(input, size=None, scale_factor=None, mode="nearest", align_corners=None):
    return interpolate(input, size, scale_factor, mode, align_corners)


def upsample_nearest(input, size=None, scale_factor=None):
    return interpolate(input, size, scale_factor, mode="nearest")


def upsample_bilinear(input, size=None, scale_factor=None):
    return interpolate(input, size, scale_factor, mode="bilinear", align_corners=True)


def _scale(n_in, n_out, scale, align_corners):
    if align_corners:
        return _f32((n_in - 1) / (n_out - 1)) if n_out > 1 else 0.0
    if scale is not None and scale > 0:
        return _f32(1.0 / scale)
    return _f32(n_in / n_out)


def _nearest_index(o, n_in, n_out, scale, exact):
    if not exact and n_out == 2 * n_in:
        return o >> 1
    s = _scale(n_in, n_out, scale, False)
    src = math.floor(_f32((o + 0.5) * s) if exact else _f32(o * s))
    return min(int(src), n_in - 1)


def _linear_resample(x, dim, n_out, scale, align_corners):
    n_in = x.shape[dim]
    s = _scale(n_in, n_out, scale, align_corners)
    i0, i1, l0, l1 = [], [], [], []
    for o in range(n_out):
        if align_corners:
            src = _f32(s * o)
        else:
            src = _f32(_f32(s * _f32(o + 0.5)) - 0.5)
            if src < 0:
                src = 0.0
        a = min(int(math.floor(src)), n_in - 1)
        b = a + (1 if a < n_in - 1 else 0)
        lam = min(max(_f32(src - a), 0.0), 1.0)
        i0.append(a)
        i1.append(b)
        l1.append(lam)
        l0.append(_f32(1.0 - lam))
    view = [1] * len(x.shape)
    view[dim] = n_out
    w0 = torch.tensor(l0, dtype=x.dtype).reshape(*view)
    w1 = torch.tensor(l1, dtype=x.dtype).reshape(*view)
    a = torch.index_select(x, dim, torch.tensor(i0, dtype=torch.int64))
    b = torch.index_select(x, dim, torch.tensor(i1, dtype=torch.int64))
    return a * w0 + b * w1


def _cubic1(x, a):
    return ((x * (a + 2) - (a + 3)) * x) * x + 1.0


def _cubic2(x, a):
    return ((x * a - 5 * a) * x + 8 * a) * x - 4 * a


def _scale64(n_in, n_out, scale, align_corners):
    if align_corners:
        return (n_in - 1) / (n_out - 1) if n_out > 1 else 0.0
    if scale is not None and scale > 0:
        return 1.0 / scale
    return n_in / n_out


def _cubic_taps(n_in, n_out, s, align_corners, dtype):
    """PyTorch's upsample_bicubic2d taps along one dimension: per output,
    four source indices (clamped to the border) and cubic-convolution
    weights (A=-0.75). Computed with `dtype` tensor arithmetic, which rounds
    each step as the kernel's own float arithmetic does."""
    o = torch.arange(n_out, dtype=dtype)
    real = o * s if align_corners else (o + 0.5) * s - 0.5
    base = torch.floor(real).clamp(max=n_in - 1)
    t = (real - base).clamp(0.0, 1.0)
    u = 1.0 - t
    a = -0.75
    w = torch.stack([_cubic2(t + 1.0, a), _cubic1(t, a), _cubic1(u, a), _cubic2(u + 1.0, a)], 1)
    idx = (base.long().unsqueeze(1) + (torch.arange(4) - 1).unsqueeze(0)).clamp(0, n_in - 1)
    return idx, w


def _aa_taps(n_in, n_out, s, cubic, dtype, r):
    """PyTorch's antialiased (PIL-style) taps along one dimension: a filter
    (triangle, or Keys cubic with a=-0.5) stretched by the downscale factor
    and normalised to sum to one within the input."""
    size = 4 if cubic else 2
    support = r(size * 0.5 * s) if s >= 1.0 else size * 0.5
    invscale = r(1.0 / s) if s >= 1.0 else 1.0
    o = torch.arange(n_out, dtype=dtype)
    center = (o + 0.5) * s
    xmin = ((center - support) + 0.5).trunc().clamp(min=0)
    xsize = ((center + support) + 0.5).trunc().clamp(max=n_in) - xmin
    taps = max(1, int(xsize.max().item()))
    j = torch.arange(taps, dtype=dtype).unsqueeze(0)
    pos = xmin.unsqueeze(1) + j
    x = torch.abs(((pos - center.unsqueeze(1)) + 0.5) * invscale)
    zero = torch.zeros_like(x)
    if cubic:
        w = torch.where(x < 1.0, _cubic1(x, -0.5), torch.where(x < 2.0, _cubic2(x, -0.5), zero))
    else:
        w = torch.where(x < 1.0, 1.0 - x, zero)
    w = torch.where(j < xsize.unsqueeze(1), w, zero)
    total = w.sum(1, keepdim=True)
    w = w / torch.where(total != 0, total, torch.ones_like(total))
    return pos.long().clamp(max=n_in - 1), w


def _resample_matrix(n_in, n_out, scale, align_corners, cubic, antialias, dtype):
    """The n_out x n_in interpolation matrix of one dimension."""
    if dtype == torch.float64:
        s, r = _scale64(n_in, n_out, scale, align_corners), _identity
    else:
        s, r = _scale(n_in, n_out, scale, align_corners), _f32
    if antialias:
        idx, w = _aa_taps(n_in, n_out, s, cubic, dtype, r)
    else:
        idx, w = _cubic_taps(n_in, n_out, s, align_corners, dtype)
    return torch.zeros(n_out, n_in, dtype=dtype).scatter_add(1, idx, w)


def _identity(v):
    return v


def _separable_resample(x, out_sizes, scales, align_corners, cubic, antialias):
    """Bicubic and antialiased bilinear/bicubic resampling of N, C, H, W:
    width first, then height, each a product with the dimension's
    interpolation matrix (so the gradient is the exact adjoint, as PyTorch's
    backward kernels compute it)."""
    dtype = torch.float64 if x.dtype == torch.float64 else torch.float32
    h_in, w_in = x.shape[2], x.shape[3]
    h_out, w_out = out_sizes
    # With antialias, PyTorch's forward skips a dimension whose size is
    # unchanged, but its backward still applies that dimension's weights
    # (not the identity when an explicit scale factor was given): the value
    # passes through and the gradient takes the weights.
    out = x
    if not antialias or w_out != w_in or (scales[1] is not None and torch._needs_grad(out)):
        m = _resample_matrix(w_in, w_out, scales[1], align_corners, cubic, antialias, dtype).to(x.dtype)
        cols = torch.matmul(out, m.t())
        out = cols if not antialias or w_out != w_in else _value_with_grad_of(out, cols)
    if not antialias or h_out != h_in or (scales[0] is not None and torch._needs_grad(out)):
        m = _resample_matrix(h_in, h_out, scales[0], align_corners, cubic, antialias, dtype).to(x.dtype)
        rows = torch.matmul(m, out)
        if antialias and h_out == h_in:
            rows = _value_with_grad_of(out, rows)
        elif antialias and w_out == 1 and h_out > 1:
            # PyTorch's antialiased kernel applies the first output row's
            # vertical weights to every row when the output is one pixel
            # wide; its backward is the true adjoint. Match both.
            quirk = torch.matmul(m.narrow(0, 0, 1).expand(h_out, h_in), out)
            rows = _value_with_grad_of(quirk, rows)
        out = rows
    return out


def _value_with_grad_of(value, proxy):
    """Exactly `value`, differentiating as `proxy` (of the same shape): the
    added proxy - proxy.detach() is an exact zero."""
    if not torch._needs_grad(proxy):
        return value.detach()
    return value.detach() + (proxy - proxy.detach())


# ---- activations ---------------------------------------------------------------------------
def silu(x, inplace=False):
    if inplace:
        return _apply_inplace(x, silu)
    out = torch.silu(x)
    return out


def relu(x, inplace=False):
    if inplace:
        return _apply_inplace(x, relu)
    out = torch.relu(x)
    return out


def relu_(x):
    return relu(x, True)


def _threshold(input, threshold, value):
    return torch.where(input > threshold, input, torch.full_like(input, value))


def threshold(input, threshold, value, inplace=False):
    if inplace:
        return _apply_inplace(input, _threshold, threshold, value)
    return _threshold(input, threshold, value)


def hardtanh(input, min_val=-1.0, max_val=1.0, inplace=False):
    if inplace:
        return _apply_inplace(input, hardtanh, min_val, max_val)
    if min_val > max_val:
        raise ValueError("min_val cannot be greater than max_val")
    x = input
    # The gradient is zero at and beyond either bound, as in PyTorch.
    out = torch.where(x <= min_val, torch.full_like(x, min_val), torch.where(x >= max_val, torch.full_like(x, max_val), x))
    return out


def relu6(input, inplace=False):
    return hardtanh(input, 0.0, 6.0, inplace)


def gelu(x, approximate="none"):
    if approximate == "none":
        # PyTorch's default: the exact-erf form (eager kernel or graph op).
        return torch._gelu(x)
    if approximate != "tanh":
        raise RuntimeError("approximate argument must be either none or tanh.")
    # The tanh form; a graph tensor composes it from recorded operations.
    return 0.5 * x * (1 + torch.tanh(math.sqrt(2 / math.pi) * (x + 0.044715 * x * x * x)))


def sigmoid(x):
    return torch.sigmoid(x)


def tanh(x):
    return torch.tanh(x)


def softplus(x, beta=1.0, threshold=20.0):
    # The exp branch is clamped where it is unused, so its gradient stays finite (no 0 * inf).
    return torch.where(x * beta > threshold, x, torch.log(1 + torch.exp((x * beta).clamp(max=threshold))) / beta)


def _softplus_neg(x):
    # softplus(-x) the way PyTorch's BCE-with-logits kernel writes it:
    # m + log(exp(-m) + exp(-x - m)) with m = max(-x, 0). The value does not
    # depend on m, so the gradient is right even where the clamp has a kink.
    m = torch.clamp(-x, min=0)
    return m + torch.log(torch.exp(-m) + torch.exp(-x - m))


def leaky_relu(x, negative_slope=0.01, inplace=False):
    if inplace:
        return _apply_inplace(x, leaky_relu, negative_slope)
    out = torch.where(x > 0, x, x * negative_slope)
    return out


def elu(x, alpha=1.0, inplace=False):
    if inplace:
        return _apply_inplace(x, elu, alpha)
    # As in softplus: clamp the unused exp branch so a large input's gradient is not nan.
    out = torch.where(x > 0, x, alpha * (torch.exp(x.clamp(max=0)) - 1))
    return out


_SELU_ALPHA = 1.6732632423543772848170429916717
_SELU_SCALE = 1.0507009873554804934193349852946


def selu(input, inplace=False):
    if inplace:
        return _apply_inplace(input, selu)
    x = input
    out = _SELU_SCALE * torch.where(x > 0, x, _SELU_ALPHA * (torch.exp(x.clamp(max=0)) - 1))
    return out


def celu(input, alpha=1.0, inplace=False):
    if inplace:
        return _apply_inplace(input, celu, alpha)
    if alpha == 0:
        raise RuntimeError("ZeroDivisionError")
    x = input
    out = torch.where(x > 0, x, alpha * (torch.exp((x / alpha).clamp(max=0)) - 1))
    return out


def rrelu(input, lower=1.0 / 8, upper=1.0 / 3, training=False, inplace=False):
    if inplace:
        return _apply_inplace(input, rrelu, lower, upper, training)
    x = input
    if training:
        slope = torch.rand(*x.shape, dtype=x.dtype) * (upper - lower) + lower
        out = torch.where(x >= 0, x, x * slope)
    else:
        out = leaky_relu(x, (lower + upper) / 2)
    return out


def prelu(input, weight):
    x = input
    if weight.numel() != 1:
        if len(x.shape) < 2:
            if weight.numel() != (x.shape[0] if len(x.shape) == 1 else 1):
                raise RuntimeError("Mismatch of parameter numbers and input channel size.")
            w = weight
        else:
            if weight.numel() != x.shape[1]:
                raise RuntimeError("Mismatch of parameter numbers and input channel size. Found parameter numbers = %d and channel size = %d." % (weight.numel(), x.shape[1]))
            w = weight.reshape(*([1, x.shape[1]] + [1] * (len(x.shape) - 2)))
    else:
        w = weight.reshape(())
    return torch.where(x > 0, x, w * x)


def glu(input, dim=-1):
    if len(input.shape) == 0:
        raise RuntimeError("glu does not support scalars because halving size must be even")
    d = dim % len(input.shape)
    if input.shape[d] % 2:
        raise RuntimeError("Halving dimension must be even, but dimension %d is size %d" % (d, input.shape[d]))
    a, b = input.chunk(2, d)
    return a * torch.sigmoid(b)


def logsigmoid(input):
    x = input
    # Both branches are stable and their unused sides stay finite.
    neg = x - torch.log(1 + torch.exp(x.clamp(max=0)))
    pos = -torch.log(1 + torch.exp(-x.clamp(min=0)))
    return torch.where(x < 0, neg, pos)


def hardshrink(input, lambd=0.5):
    x = input
    return torch.where((x > lambd) | (x < -lambd), x, torch.zeros_like(x))


def softshrink(input, lambd=0.5):
    if lambd < 0:
        raise RuntimeError("lambda must be greater or equal to 0, but found to be %s." % lambd)
    x = input
    return torch.where(x > lambd, x - lambd, torch.where(x < -lambd, x + lambd, torch.zeros_like(x)))


def tanhshrink(input):
    return input - torch.tanh(input)


def softsign(input):
    return input / (1 + torch.abs(input))


def mish(input, inplace=False):
    if inplace:
        return _apply_inplace(input, mish)
    out = input * torch.tanh(softplus(input))
    return out


def hardsigmoid(input, inplace=False):
    if inplace:
        return _apply_inplace(input, hardsigmoid)
    x = input
    # The gradient is 1/6 strictly inside (-3, 3), zero elsewhere.
    out = torch.where((x > -3) & (x < 3), x / 6 + 0.5, (x >= 3).to(x.dtype))
    return out


def hardswish(input, inplace=False):
    if inplace:
        return _apply_inplace(input, hardswish)
    x = input
    # PyTorch's gradient: 0 up to -3 inclusive, x / 3 + 1/2 inside, 1 from 3 on.
    out = torch.where(x <= -3, torch.zeros_like(x), torch.where(x < 3, x * (x + 3) / 6, x))
    return out


_threshold_fn = threshold


def threshold_(input, threshold, value):
    return _threshold_fn(input, threshold, value, True)


def hardtanh_(input, min_val=-1.0, max_val=1.0):
    return hardtanh(input, min_val, max_val, True)


def elu_(input, alpha=1.0):
    return elu(input, alpha, True)


def selu_(input):
    return selu(input, True)


def celu_(input, alpha=1.0):
    return celu(input, alpha, True)


def leaky_relu_(input, negative_slope=0.01):
    return leaky_relu(input, negative_slope, True)


def rrelu_(input, lower=1.0 / 8, upper=1.0 / 3, training=False):
    return rrelu(input, lower, upper, training, True)


def _implicit_dim(ndim):
    return 0 if ndim in (0, 1, 3) else 1


def softmax(x, dim=None, _stacklevel=3, dtype=None):
    if dim is None:
        dim = _implicit_dim(len(x.shape))
    return torch.softmax(x, dim, dtype=dtype)


def log_softmax(x, dim=None, _stacklevel=3, dtype=None):
    if dim is None:
        dim = _implicit_dim(len(x.shape))
    return torch.log_softmax(x, dim, dtype=dtype)


def softmin(input, dim=None, _stacklevel=3, dtype=None):
    if dim is None:
        dim = _implicit_dim(len(input.shape))
    return torch.softmax(-input if dtype is None else -input.to(dtype), dim)


def gumbel_softmax(logits, tau=1.0, hard=False, eps=1e-10, dim=-1):
    gumbels = -torch.log(-torch.log(torch.rand(*logits.shape, dtype=logits.dtype).clamp(min=eps)).clamp(min=eps))
    y_soft = torch.softmax((logits + gumbels) / tau, dim)
    if not hard:
        return y_soft
    index = y_soft.argmax(dim, keepdim=True)
    y_hard = torch.zeros_like(y_soft)
    y_hard = torch.where(torch.arange(y_soft.shape[dim]).reshape(*[-1 if i == dim % len(y_soft.shape) else 1 for i in range(len(y_soft.shape))]) == index, torch.ones_like(y_soft), y_hard)
    return y_hard - y_soft.detach() + y_soft


# ---- normalisation -------------------------------------------------------------------------
def _safe_norm(x, p=2.0, dim=None, keepdim=False):
    """A vector norm whose gradient at a zero vector is zero (PyTorch's
    convention) instead of 0/0."""
    if p == 1:
        return torch.abs(x).sum(dim, keepdim=keepdim)
    if p == _inf:
        a = torch.abs(x)
        return a.max() if dim is None else a.max(dim, keepdim=keepdim)[0]
    s = (x * x).sum(dim, keepdim=keepdim) if p == 2 else (torch.abs(x) ** p).sum(dim, keepdim=keepdim)
    positive = s > 0
    safe = torch.where(positive, s, torch.ones_like(s))
    root = torch.sqrt(safe) if p == 2 else safe ** (1.0 / p)
    return torch.where(positive, root, torch.zeros_like(s))


def _clamp_min_value(t, eps):
    """max(t, eps) in value, with the gradient of t itself (PyTorch clamps
    these norms in place under no_grad)."""
    return t + (torch.clamp(t.detach(), min=eps) - t.detach())


def normalize(input, p=2.0, dim=1, eps=1e-12, out=None):
    n = _safe_norm(input, p, dim, keepdim=True).clamp(min=eps)
    result = input / n.expand_as(input)
    if out is None:
        return result
    if torch._needs_grad(result):
        raise RuntimeError("normalize(): functions with out=... arguments don't support automatic differentiation, but one of the arguments requires grad.")
    out.copy_(result)
    return out


def cosine_similarity(x1, x2, dim=1, eps=1e-8):
    # Each norm is clamped separately: (x1 / max(|x1|, eps)) . (x2 / max(|x2|, eps)).
    shape = torch._broadcast_shapes(tuple(x1.shape), tuple(x2.shape))
    a = x1 if tuple(x1.shape) == shape else x1.expand(*shape)
    b = x2 if tuple(x2.shape) == shape else x2.expand(*shape)
    na = _clamp_min_value(_safe_norm(a, 2, dim, keepdim=True), eps)
    nb = _clamp_min_value(_safe_norm(b, 2, dim, keepdim=True), eps)
    return ((a / na) * (b / nb)).sum(dim)


def pairwise_distance(x1, x2, p=2.0, eps=1e-6, keepdim=False):
    d = x1 - x2 + eps
    return _safe_norm(d, p, -1 if len(d.shape) else None, keepdim=keepdim)


def pdist(input, p=2):
    n = input.shape[0]
    rows = []
    for i in range(n - 1):
        rows.append(_safe_norm(input[i:i + 1] - input[i + 1:], p, -1))
    return torch.cat(rows) if rows else torch.zeros(0, dtype=input.dtype)


def layer_norm(x, normalized_shape, weight=None, bias=None, eps=1e-5):
    dims = list(range(len(x.shape) - len(normalized_shape), len(x.shape)))
    mean = x.mean(dims, keepdim=True)
    var = ((x - mean) ** 2).mean(dims, keepdim=True)
    out = (x - mean) / torch.sqrt(var + eps)
    if weight is not None:
        out = out * weight
    if bias is not None:
        out = out + bias
    return out


def _eps_of(dtype):
    return 2.220446049250313e-16 if dtype == torch.float64 else 1.1920928955078125e-07


def rms_norm(input, normalized_shape, weight=None, eps=None):
    shape = (normalized_shape,) if isinstance(normalized_shape, int) else tuple(normalized_shape)
    if tuple(input.shape[len(input.shape) - len(shape):]) != shape:
        raise RuntimeError("Given normalized_shape=%s, expected input with shape [*, %s], but got input of size%s" % (list(shape), ", ".join(str(s) for s in shape), tuple(input.shape)))
    if eps is None:
        eps = _eps_of(input.dtype)
    dims = list(range(len(input.shape) - len(shape), len(input.shape)))
    out = input * torch.rsqrt((input * input).mean(dims, keepdim=True) + eps)
    return out if weight is None else out * weight


def _channel_view(t, ndim):
    return t.reshape(*([1, -1] + [1] * (ndim - 2)))


def batch_norm(input, running_mean, running_var, weight=None, bias=None, training=False, momentum=0.1, eps=1e-5):
    x = input
    ndim = len(x.shape)
    if ndim < 2:
        raise ValueError("expected at least 2D input (got %dD input)" % ndim)
    if training:
        size = x.numel() // x.shape[1] if x.shape[1] else 0
        if size == 1:
            raise ValueError("Expected more than 1 value per channel when training, got input size %s" % (x.shape,))
    dims = [0] + list(range(2, ndim))
    if training or running_mean is None or running_var is None:
        mean = x.mean(dims)
        var = ((x - _channel_view(mean, ndim)) ** 2).mean(dims)
        if training and running_mean is not None and running_var is not None:
            n = x.numel() // x.shape[1]
            with torch.no_grad():
                running_mean.copy_(running_mean * (1 - momentum) + mean.detach() * momentum)
                unbiased = var.detach() * (n / (n - 1)) if n > 1 else var.detach()
                running_var.copy_(running_var * (1 - momentum) + unbiased * momentum)
    else:
        mean, var = running_mean, running_var
    out = (x - _channel_view(mean, ndim)) * _channel_view(torch.rsqrt(var + eps), ndim)
    if weight is not None:
        out = out * _channel_view(weight, ndim)
    if bias is not None:
        out = out + _channel_view(bias, ndim)
    return out


def instance_norm(input, running_mean=None, running_var=None, weight=None, bias=None, use_input_stats=True, momentum=0.1, eps=1e-5):
    x = input
    ndim = len(x.shape)
    if ndim < 3:
        raise ValueError("instance_norm expects an input of at least 3 dimensions (N, C, *)")
    dims = list(range(2, ndim))
    if use_input_stats:
        if _prod(x.shape[2:]) == 1:
            raise ValueError("Expected more than 1 spatial element when training, got input size %s" % (x.shape,))
        mean = x.mean(dims, keepdim=True)
        var = ((x - mean) ** 2).mean(dims, keepdim=True)
        if running_mean is not None and running_var is not None:
            n = _prod(x.shape[2:])
            with torch.no_grad():
                m = mean.detach().reshape(x.shape[0], x.shape[1]).mean(0)
                v = (var.detach() * (n / (n - 1))).reshape(x.shape[0], x.shape[1]).mean(0)
                running_mean.copy_(running_mean * (1 - momentum) + m * momentum)
                running_var.copy_(running_var * (1 - momentum) + v * momentum)
    else:
        mean = _channel_view(running_mean, ndim)
        var = _channel_view(running_var, ndim)
    out = (x - mean) * torch.rsqrt(var + eps)
    if weight is not None:
        out = out * _channel_view(weight, ndim)
    if bias is not None:
        out = out + _channel_view(bias, ndim)
    return out


def group_norm(input, num_groups, weight=None, bias=None, eps=1e-5):
    x = input
    if len(x.shape) < 2:
        raise RuntimeError("Expected at least 2 dimensions for input tensor but received %d" % len(x.shape))
    n, c = x.shape[0], x.shape[1]
    if c % num_groups:
        raise RuntimeError("Expected number of channels in input to be divisible by num_groups, but got input of shape %s and num_groups=%d" % (x.shape, num_groups))
    g = x.reshape(n, num_groups, -1)
    mean = g.mean(-1, keepdim=True)
    var = ((g - mean) ** 2).mean(-1, keepdim=True)
    out = ((g - mean) * torch.rsqrt(var + eps)).reshape(*x.shape)
    if weight is not None:
        out = out * _channel_view(weight, len(x.shape))
    if bias is not None:
        out = out + _channel_view(bias, len(x.shape))
    return out


def local_response_norm(input, size, alpha=1e-4, beta=0.75, k=1.0):
    x = input
    if len(x.shape) < 3:
        raise ValueError("Expected 3D or higher dimensionality input (got %d dimensions)" % len(x.shape))
    if x.numel() == 0:
        return x
    sq = x * x
    c = x.shape[1]
    padded = _pad_dim(sq, 1, size // 2, (size - 1) // 2, 0.0)
    total = None
    for i in range(size):
        part = padded.narrow(1, i, c)
        total = part if total is None else total + part
    div = (total / size * alpha + k) ** beta
    return x / div


# ---- dropout -------------------------------------------------------------------------------
def _check_p(p):
    if p < 0.0 or p > 1.0:
        raise ValueError("dropout probability has to be between 0 and 1, but got %s" % p)


def dropout(x, p=0.5, training=True, inplace=False):
    _check_p(p)
    if not training or p == 0:
        return x
    if inplace:
        return _apply_inplace(x, dropout, p, training)
    if p == 1:
        out = x * torch.zeros_like(x)
    else:
        mask = (torch.rand(*x.shape) >= p).to(x.dtype)
        out = x * (mask * _dropout_scale(p, x.dtype))
    return out


def _dropout_scale(p, dtype):
    """The kept-value factor of PyTorch's CPU dropout: its noise is divided
    by 1 - p as a scalar, which the kernel does as a multiply by the
    reciprocal in the op math type (float32 below float64)."""
    if dtype is torch.float64:
        return 1.0 / (1.0 - p)
    return _f32(1.0 / _f32(1.0 - p))


def _feature_dropout(x, p, training, inplace, batched_rank, name):
    _check_p(p)
    if not training or p == 0:
        return x
    if inplace:
        return _apply_inplace(x, _feature_dropout, p, training, False, batched_rank, name)
    rank = len(x.shape)
    if rank not in (batched_rank - 1, batched_rank):
        raise RuntimeError("%s: Expected %dD or %dD input, but received a %dD input." % (name, batched_rank - 1, batched_rank, rank))
    lead = 2 if rank == batched_rank else 1
    mask_shape = tuple(x.shape[:lead]) + (1,) * (rank - lead)
    if p == 1:
        out = x * torch.zeros_like(x)
    else:
        mask = (torch.rand(*mask_shape) >= p).to(x.dtype)
        out = x * (mask * _dropout_scale(p, x.dtype))
    return out


def dropout1d(input, p=0.5, training=True, inplace=False):
    return _feature_dropout(input, p, training, inplace, 3, "dropout1d")


def dropout2d(input, p=0.5, training=True, inplace=False):
    return _feature_dropout(input, p, training, inplace, 4, "dropout2d")


def dropout3d(input, p=0.5, training=True, inplace=False):
    return _feature_dropout(input, p, training, inplace, 5, "dropout3d")


def alpha_dropout(input, p=0.5, training=False, inplace=False):
    _check_p(p)
    x = input
    if not training or p == 0:
        return x
    if inplace:
        return _apply_inplace(input, alpha_dropout, p, training)
    if p == 1:
        return x * torch.zeros_like(x)
    alpha = 1.7580993408473766
    a = 1.0 / math.sqrt((alpha * alpha * p + 1) * (1 - p))
    keep = (torch.rand(*x.shape) >= p).to(x.dtype)
    b = (keep - 1) * (alpha * a) + alpha * a * p
    out = x * (keep * a) + b
    return out


def feature_alpha_dropout(input, p=0.5, training=False, inplace=False):
    return alpha_dropout(input, p, training, inplace)


# ---- losses --------------------------------------------------------------------------------
def _weighted(d, weight, input, reduction):
    # PyTorch's weighted mse/l1: the mean divides by the summed weights.
    if tuple(weight.shape) != tuple(input.shape):
        raise ValueError("Weights and input must have the same size.")
    d = d * weight
    if reduction == "none":
        return d
    if reduction == "sum":
        return d.sum()
    if reduction == "mean":
        return d.sum() / weight.sum()
    raise ValueError("Invalid reduction mode: %s. Expected one of 'none', 'mean', 'sum'." % reduction)


def mse_loss(a, b, size_average=None, reduce=None, reduction="mean", weight=None):
    reduction = _reduction(size_average, reduce, reduction)
    d = (a - b) ** 2
    if weight is not None:
        return _weighted(d, weight, a, reduction)
    return d.mean() if reduction == "mean" else d.sum() if reduction == "sum" else d


def l1_loss(a, b, size_average=None, reduce=None, reduction="mean", weight=None):
    reduction = _reduction(size_average, reduce, reduction)
    d = (a - b).abs()
    if weight is not None:
        return _weighted(d, weight, a, reduction)
    return d.mean() if reduction == "mean" else d.sum() if reduction == "sum" else d


def smooth_l1_loss(input, target, size_average=None, reduce=None, reduction="mean", beta=1.0):
    reduction = _reduction(size_average, reduce, reduction)
    if beta < 0:
        raise RuntimeError("smooth_l1_loss does not support negative values for beta.")
    d = input - target
    a = torch.abs(d)
    if beta == 0:
        return _reduce(a, reduction)
    loss = torch.where(a < beta, 0.5 * d * d / beta, a - 0.5 * beta)
    return _reduce(loss, reduction)


def huber_loss(input, target, reduction="mean", delta=1.0, weight=None):
    if delta <= 0:
        raise RuntimeError("huber_loss does not support non-positive values for delta.")
    d = input - target
    a = torch.abs(d)
    loss = torch.where(a < delta, 0.5 * d * d, delta * (a - 0.5 * delta))
    if weight is not None:
        if tuple(weight.shape) != tuple(input.shape):
            raise ValueError("Weights and input must have the same size.")
        loss = loss * weight
    return _reduce(loss, reduction)


def _xlogy(x, y):
    # x * log(y), zero where x is zero.
    safe = torch.where(x == 0, torch.ones_like(y), y)
    return torch.where(x == 0, torch.zeros_like(x), x * torch.log(safe))


def kl_div(input, target, size_average=None, reduce=None, reduction="mean", log_target=False):
    reduction = _reduction(size_average, reduce, reduction)
    if log_target:
        loss = torch.exp(target) * (target - input)
    else:
        loss = _xlogy(target, target) - target * input
    if reduction == "batchmean":
        if len(input.shape) == 0:
            return loss.sum()
        return loss.sum() / input.shape[0]
    return _reduce(loss, reduction)


def _flatten_classes(input, target):
    """[N, C, d1, ...] / [N, C] / [C] log-probabilities (or logits) and their
    targets as [M, C] rows and [M] targets."""
    if len(input.shape) == 1:
        return input.unsqueeze(0), target.reshape(-1)
    if len(input.shape) > 2:
        moved = torch.movedim(input, 1, -1)
        return moved.reshape(-1, moved.shape[-1]), target.reshape(-1)
    return input, target.reshape(-1)


def _check_targets(input, target):
    if target.dtype.is_floating_point:
        raise RuntimeError("expected scalar type Long but found %s" % ("Float" if target.dtype == torch.float32 else "Double"))
    if len(input.shape) == 1:
        if len(target.shape) != 0:
            raise RuntimeError("0D or 1D target tensor expected, multi-target not supported")
    elif len(input.shape) == 2:
        if len(target.shape) != 1:
            raise RuntimeError("0D or 1D target tensor expected, multi-target not supported")
        if target.shape[0] != input.shape[0]:
            raise ValueError("Expected input batch_size (%d) to match target batch_size (%d)." % (input.shape[0], target.shape[0]))
    elif len(input.shape) > 2:
        if tuple(target.shape) != (input.shape[0],) + tuple(input.shape[2:]):
            raise RuntimeError("Expected target size %s, got %s" % ((input.shape[0],) + tuple(input.shape[2:]), tuple(target.shape)))
    else:
        raise ValueError("Expected 1 or more dimensions (got 0)")


def _pick(lp, target, weight, ignore_index):
    """-lp[i, target[i]] per row, the per-row target weights and the valid
    mask (None when every row counts). Targets outside [0, C) raise."""
    m, c = lp.shape[0], lp.shape[1]
    valid = None
    safe = target
    if m:
        keep = target != ignore_index
        if not bool(keep.all().item()):
            valid = keep
            safe = torch.where(keep, target, torch.zeros_like(target))
        lo, hi = safe.min().item(), safe.max().item()
        if lo < 0 or hi >= c:
            bad = lo if lo < 0 else hi
            raise IndexError("Target %d is out of bounds." % bad)
    loss = -lp[torch.arange(m), safe]
    w = None
    if weight is not None:
        w = weight[safe]
    if valid is not None:
        mask = valid.to(lp.dtype)
        w = mask if w is None else w * mask
    return loss, w, valid, safe


def nll_loss(input, target, weight=None, size_average=None, ignore_index=-100, reduce=None, reduction="mean"):
    if torch._graph_recording and getattr(input, "_zipp_graph", False):
        raise NotImplementedError("GPU nll_loss is not captured; use F.cross_entropy on the logits with integer class targets")
    reduction = _reduction(size_average, reduce, reduction)
    _check_targets(input, target)
    target_shape = tuple(target.shape)
    lp, flat = _flatten_classes(input, target)
    if weight is not None and tuple(weight.shape) != (lp.shape[1],):
        raise RuntimeError("weight tensor should be defined either for all %d classes or no classes but got weight tensor of shape: %s" % (lp.shape[1], tuple(weight.shape)))
    loss, w, valid, _ = _pick(lp, flat, weight, ignore_index)
    if w is None:
        if reduction == "none":
            return loss.reshape(*target_shape)
        return _reduce(loss, reduction)
    loss = loss * w
    if reduction == "none":
        return loss.reshape(*target_shape)
    if reduction == "sum":
        return loss.sum()
    if reduction != "mean":
        raise ValueError("%s is not a valid value for reduction" % reduction)
    return loss.sum() / w.sum()


def cross_entropy(input, target, weight=None, size_average=None, ignore_index=-100, reduce=None, reduction="mean", label_smoothing=0.0):
    logits = input
    reduction = _reduction(size_average, reduce, reduction)
    if torch._graph_recording and getattr(logits, "_zipp_graph", False) and not target.dtype.is_floating_point:
        # Integer class targets take the fused graph operation; probability
        # targets compose from log_softmax below.
        return logits.cross_entropy(target, weight, reduction, label_smoothing)
    if label_smoothing < 0 or label_smoothing > 1:
        raise RuntimeError("label_smoothing must be between 0.0 and 1.0. Got: %s" % label_smoothing)
    # Classes are dim 1 of [N, C, d1, ...] (the last dim of [C] or [N, C]).
    class_dim = 1 if len(logits.shape) > 2 else -1
    n_classes = logits.shape[class_dim]
    if weight is not None:
        shape = [1] * len(logits.shape)
        shape[class_dim] = n_classes
        class_weight = weight.reshape(*shape)
    if target.dtype.is_floating_point:
        # Probability targets; the mean is over the N * d1 * ... positions (not the weights).
        if tuple(target.shape) != tuple(logits.shape):
            raise RuntimeError("Expected input batch_size (%d) to match target batch_size (%d)." % (logits.shape[0], target.shape[0]))
        if label_smoothing > 0:
            target = target * (1 - label_smoothing) + label_smoothing / n_classes
        terms = target * log_softmax(logits, class_dim)
        if weight is not None:
            terms = terms * class_weight
        loss = -terms.sum(class_dim)
        return loss.mean() if reduction == "mean" else loss.sum() if reduction == "sum" else loss
    _check_targets(logits, target)
    target_shape = tuple(target.shape)
    rows, flat = _flatten_classes(logits, target)
    lp = log_softmax(rows, -1)
    loss, w, valid, _ = _pick(lp, flat, weight, ignore_index)
    if w is None and label_smoothing == 0:
        if reduction == "none":
            return loss.reshape(*target_shape)
        return _reduce(loss, reduction)
    # PyTorch's form: (1 - eps) * nll + eps / C * smooth, where smooth is
    # -sum_c w[c] lp[c] at each counted position, and the mean divides both
    # by the summed target weights (the count of counted positions).
    nll = loss if w is None else loss * w
    if label_smoothing > 0:
        smooth = -(lp * weight).sum(-1) if weight is not None else -lp.sum(-1)
        if valid is not None:
            smooth = smooth * valid.to(lp.dtype)
    if reduction == "none":
        out = nll if label_smoothing == 0 else nll * (1 - label_smoothing) + smooth * (label_smoothing / n_classes)
        return out.reshape(*target_shape)
    if reduction == "sum":
        total = nll.sum()
        return total if label_smoothing == 0 else total * (1 - label_smoothing) + smooth.sum() * (label_smoothing / n_classes)
    if reduction != "mean":
        raise ValueError("%s is not a valid value for reduction" % reduction)
    denom = w.sum() if w is not None else flat.shape[0]
    total = nll.sum() / denom
    return total if label_smoothing == 0 else total * (1 - label_smoothing) + (smooth.sum() / denom) * (label_smoothing / n_classes)


def binary_cross_entropy_with_logits(input, target, weight=None, size_average=None, reduce=None, reduction="mean", pos_weight=None):
    logits = input
    reduction = _reduction(size_average, reduce, reduction)
    if tuple(logits.shape) != tuple(target.shape):
        raise ValueError("Target size (%s) must be the same as input size (%s)" % (target.shape, logits.shape))
    sp = _softplus_neg(logits)
    if pos_weight is not None:
        loss = (1 - target) * logits + (1 + (pos_weight - 1) * target) * sp
    else:
        loss = (1 - target) * logits + sp
    if weight is not None:
        loss = loss * weight
    return _reduce(loss, reduction)


def binary_cross_entropy(input, target, weight=None, size_average=None, reduce=None, reduction="mean"):
    probs = input
    reduction = _reduction(size_average, reduce, reduction)
    if tuple(target.shape) != tuple(probs.shape):
        raise ValueError("Using a target size (%s) that is different to the input size (%s) is deprecated. Please ensure they have the same size." % (target.shape, probs.shape))
    if probs.numel() and (probs.min().item() < 0 or probs.max().item() > 1):
        raise RuntimeError("all elements of input should be between 0 and 1")
    # PyTorch's values: each log is clamped to >= -100, so an exact 0 or 1
    # gives a finite loss, and the gradient is (p - y) / max(p (1 - p), 1e-12),
    # applied through a detached coefficient.
    p = probs.detach()
    loss = -(target * torch.log(p).clamp(min=-100) + (1 - target) * torch.log(1 - p).clamp(min=-100))
    grad = (p - target) / torch.clamp(p * (1 - p), min=1e-12)
    if weight is not None:
        loss = loss * weight
        grad = grad * weight
    if probs.requires_grad:
        loss = loss + (probs - p) * grad.detach()
    return _reduce(loss, reduction)


def poisson_nll_loss(input, target, log_input=True, full=False, size_average=None, eps=1e-8, reduce=None, reduction="mean"):
    reduction = _reduction(size_average, reduce, reduction)
    if log_input:
        loss = torch.exp(input) - target * input
    else:
        loss = input - target * torch.log(input + eps)
    if full:
        safe = torch.where(target > 1, target, torch.ones_like(target))
        stirling = safe * torch.log(safe) - safe + 0.5 * torch.log(2 * math.pi * safe)
        loss = loss + torch.where(target > 1, stirling, torch.zeros_like(target))
    return _reduce(loss, reduction)


def gaussian_nll_loss(input, target, var, full=False, eps=1e-6, reduction="mean"):
    if not isinstance(var, Tensor):
        var = torch.full_like(input, float(var))
    if tuple(var.shape) != tuple(input.shape):
        if tuple(input.shape[:-1]) == tuple(var.shape):
            var = var.unsqueeze(-1)
        elif tuple(input.shape[:-1]) == tuple(var.shape[:-1]) and var.shape[-1] == 1:
            pass
        else:
            raise ValueError("var is of incorrect size")
    if reduction not in ("none", "mean", "sum"):
        raise ValueError(reduction + " is not a valid value for reduction")
    if var.numel() and var.min().item() < 0:
        raise ValueError("var has negative entry/entries")
    # PyTorch clamps a clone of var in place under no_grad: the value is
    # clamped, the gradient passes as if it were not.
    var = _clamp_min_value(var, eps)
    loss = 0.5 * (torch.log(var) + (input - target) ** 2 / var)
    if full:
        loss = loss + 0.5 * math.log(2 * math.pi)
    return _reduce(loss, reduction)


def cosine_embedding_loss(input1, input2, target, margin=0, size_average=None, reduce=None, reduction="mean"):
    reduction = _reduction(size_average, reduce, reduction)
    dim = len(input1.shape) - 1
    eps = 1e-12
    prod = (input1 * input2).sum(dim)
    mag1 = (input1 * input1).sum(dim) + eps
    mag2 = (input2 * input2).sum(dim) + eps
    cos = prod / torch.sqrt(mag1 * mag2)
    zeros = torch.zeros_like(cos)
    pos = 1 - cos
    neg = torch.clamp(cos - margin, min=0)
    loss = torch.where(target == 1, pos, zeros) + torch.where(target == -1, neg, zeros)
    return _reduce(loss, reduction)


def margin_ranking_loss(input1, input2, target, margin=0, size_average=None, reduce=None, reduction="mean"):
    reduction = _reduction(size_average, reduce, reduction)
    if len(input1.shape) != len(input2.shape) or len(input1.shape) != len(target.shape):
        raise RuntimeError("margin_ranking_loss : All input tensors should have same dimension but got sizes: input1: %s, input2: %s, target: %s " % (tuple(input1.shape), tuple(input2.shape), tuple(target.shape)))
    loss = torch.clamp(-target * (input1 - input2) + margin, min=0)
    return _reduce(loss, reduction)


def hinge_embedding_loss(input, target, margin=1.0, size_average=None, reduce=None, reduction="mean"):
    reduction = _reduction(size_average, reduce, reduction)
    zeros = torch.zeros_like(input)
    margin_clamp = torch.clamp(margin - input, min=0)
    loss = torch.where(target != 1, margin_clamp, zeros) + torch.where(target != -1, input, zeros)
    return _reduce(loss, reduction)


def soft_margin_loss(input, target, size_average=None, reduce=None, reduction="mean"):
    reduction = _reduction(size_average, reduce, reduction)
    loss = torch.log(1 + torch.exp(-input * target))
    return _reduce(loss, reduction)


def multilabel_soft_margin_loss(input, target, weight=None, size_average=None, reduce=None, reduction="mean"):
    reduction = _reduction(size_average, reduce, reduction)
    loss = -(target * logsigmoid(input) + (1 - target) * logsigmoid(-input))
    if weight is not None:
        loss = loss * weight
    class_dim = len(input.shape) - 1
    loss = loss.sum(class_dim) / input.shape[class_dim]
    return _reduce(loss, reduction)


def multi_margin_loss(input, target, p=1, margin=1.0, weight=None, size_average=None, reduce=None, reduction="mean"):
    reduction = _reduction(size_average, reduce, reduction)
    if p not in (1, 2):
        raise ValueError("only p == 1 and p == 2 supported")
    x = input.unsqueeze(0) if len(input.shape) == 1 else input
    t = target.reshape(-1)
    n, c = x.shape
    picked = x[torch.arange(n), t].unsqueeze(1)
    z = torch.clamp(margin - picked + x, min=0)
    if p == 2:
        z = z * z
    if weight is not None:
        z = z * weight[t].unsqueeze(1)
    others = torch.arange(c).unsqueeze(0) != t.unsqueeze(1)
    loss = (z * others.to(x.dtype)).sum(1) / c
    if len(input.shape) == 1 and reduction == "none":
        loss = loss.reshape(())
    return _reduce(loss, reduction)


def multilabel_margin_loss(input, target, size_average=None, reduce=None, reduction="mean"):
    """sum over target classes j (the leading entries of each target row,
    up to the first -1) and non-target classes i of max(0, 1 - (x[j] - x[i])),
    divided by the number of classes."""
    reduction = _reduction(size_average, reduce, reduction)
    unbatched = len(input.shape) <= 1
    x = input.reshape(1, -1) if unbatched else input
    t = target.reshape(1, -1) if unbatched else target
    if len(x.shape) != 2 or tuple(t.shape) != tuple(x.shape):
        raise RuntimeError("inconsistent target size: %s for input of size: %s" % (list(target.shape), list(input.shape)))
    n, c = x.shape
    counts = [[0.0] * c for _ in range(n)]
    member = [[0.0] * c for _ in range(n)]
    for row, labels in enumerate(t.tolist()):
        for j in labels:
            j = int(j)
            if j < 0:
                break
            if j >= c:
                raise RuntimeError("multilabel_margin_loss: target index %d is out of bounds" % j)
            counts[row][j] += 1.0
            member[row][j] = 1.0
    cnt = torch.tensor(counts, dtype=x.dtype)
    other = 1.0 - torch.tensor(member, dtype=x.dtype)
    margins = relu(1.0 - x.unsqueeze(2) + x.unsqueeze(1))
    loss = (margins * (cnt.unsqueeze(2) * other.unsqueeze(1))).sum((1, 2)) / c
    if unbatched:
        loss = loss.reshape(()) if len(input.shape) == 1 or input.numel() == 1 else loss
    return _reduce(loss, reduction)


def triplet_margin_loss(anchor, positive, negative, margin=1.0, p=2, eps=1e-6, swap=False, size_average=None, reduce=None, reduction="mean"):
    reduction = _reduction(size_average, reduce, reduction)
    if margin <= 0:
        raise ValueError("TripletMarginLoss: expected margin to be greater than 0, got %s instead" % margin)
    d_pos = pairwise_distance(anchor, positive, p, eps)
    d_neg = pairwise_distance(anchor, negative, p, eps)
    if swap:
        d_swap = pairwise_distance(positive, negative, p, eps)
        d_neg = torch.minimum(d_neg, d_swap)
    loss = torch.clamp(margin + d_pos - d_neg, min=0)
    return _reduce(loss, reduction)


def triplet_margin_with_distance_loss(anchor, positive, negative, distance_function=None, margin=1.0, swap=False, reduction="mean"):
    dist = pairwise_distance if distance_function is None else distance_function
    d_pos = dist(anchor, positive)
    d_neg = dist(anchor, negative)
    if swap:
        d_neg = torch.minimum(d_neg, dist(positive, negative))
    loss = torch.clamp(margin + d_pos - d_neg, min=0)
    return _reduce(loss, reduction)


# ---- attention -----------------------------------------------------------------------------
def scaled_dot_product_attention(query, key, value, attn_mask=None, dropout_p=0.0, is_causal=False, scale=None, enable_gqa=False):
    L, S = query.shape[-2], key.shape[-2]
    scale_factor = 1.0 / math.sqrt(query.shape[-1]) if scale is None else scale
    if enable_gqa and len(query.shape) >= 3:
        hq, hk = query.shape[-3], key.shape[-3]
        if hq != hk:
            key = key.repeat_interleave(hq // hk, dim=-3)
            value = value.repeat_interleave(hq // value.shape[-3], dim=-3)
    scores = torch.matmul(query, key.transpose(-2, -1)) * scale_factor
    if is_causal:
        if attn_mask is not None:
            raise RuntimeError("_scaled_dot_product_attention: Explicit attn_mask should not be set when is_causal=True")
        causal = torch.tril(torch.ones(L, S, dtype=torch.bool))
        scores = scores.masked_fill(causal.logical_not(), -_inf)
    if attn_mask is not None:
        if attn_mask.dtype == torch.bool:
            scores = scores.masked_fill(attn_mask.logical_not(), -_inf)
        else:
            scores = scores + attn_mask
    weights = torch.softmax(scores, -1)
    if dropout_p > 0.0:
        weights = dropout(weights, dropout_p, True)
    return torch.matmul(weights, value)


def _canonical_mask(mask, target_type):
    if mask is None:
        return None
    if mask.dtype == torch.bool:
        return torch.zeros(*mask.shape, dtype=target_type).masked_fill(mask, -_inf)
    if not mask.dtype.is_floating_point:
        raise AssertionError("only bool and floating types of masks are supported")
    return mask


def multi_head_attention_forward(query, key, value, embed_dim_to_check, num_heads, in_proj_weight, in_proj_bias, bias_k, bias_v, add_zero_attn, dropout_p, out_proj_weight, out_proj_bias, training=True, key_padding_mask=None, need_weights=True, attn_mask=None, use_separate_proj_weight=False, q_proj_weight=None, k_proj_weight=None, v_proj_weight=None, static_k=None, static_v=None, average_attn_weights=True, is_causal=False):
    """PyTorch's multi-head attention on (L, N, E) inputs (or unbatched (L, E))."""
    is_batched = len(query.shape) == 3
    if not is_batched:
        query, key, value = query.unsqueeze(1), key.unsqueeze(1), value.unsqueeze(1)
        if key_padding_mask is not None:
            key_padding_mask = key_padding_mask.unsqueeze(0)
    tgt_len, bsz, embed_dim = query.shape
    key_padding_mask = _canonical_mask(key_padding_mask, query.dtype)
    if is_causal and attn_mask is None:
        raise RuntimeError("Need attn_mask if specifying the is_causal hint. You may use the Transformer module method `generate_square_subsequent_mask` to create this mask.")
    attn_mask = _canonical_mask(attn_mask, query.dtype)
    if embed_dim != embed_dim_to_check:
        raise AssertionError("was expecting embedding dimension of %d, but got %d" % (embed_dim_to_check, embed_dim))
    head_dim = embed_dim // num_heads
    if head_dim * num_heads != embed_dim:
        raise AssertionError("embed_dim %d not divisible by num_heads %d" % (embed_dim, num_heads))
    if not use_separate_proj_weight:
        w_q, w_k, w_v = in_proj_weight.chunk(3)
    else:
        w_q, w_k, w_v = q_proj_weight, k_proj_weight, v_proj_weight
    b_q = b_k = b_v = None
    if in_proj_bias is not None:
        b_q, b_k, b_v = in_proj_bias.chunk(3)
    q = linear(query, w_q, b_q)
    k = linear(key, w_k, b_k)
    v = linear(value, w_v, b_v)
    if attn_mask is not None:
        if len(attn_mask.shape) == 2:
            if tuple(attn_mask.shape) != (tgt_len, key.shape[0]):
                raise RuntimeError("The shape of the 2D attn_mask is %s, but should be %s." % (tuple(attn_mask.shape), (tgt_len, key.shape[0])))
            attn_mask = attn_mask.unsqueeze(0)
        elif len(attn_mask.shape) == 3:
            if tuple(attn_mask.shape) != (bsz * num_heads, tgt_len, key.shape[0]):
                raise RuntimeError("The shape of the 3D attn_mask is %s, but should be %s." % (tuple(attn_mask.shape), (bsz * num_heads, tgt_len, key.shape[0])))
        else:
            raise RuntimeError("attn_mask's dimension %d is not supported" % len(attn_mask.shape))
    if bias_k is not None and bias_v is not None:
        k = torch.cat([k, bias_k.repeat(1, bsz, 1)])
        v = torch.cat([v, bias_v.repeat(1, bsz, 1)])
        if attn_mask is not None:
            attn_mask = pad(attn_mask, (0, 1))
        if key_padding_mask is not None:
            key_padding_mask = pad(key_padding_mask, (0, 1))
    q = q.reshape(tgt_len, bsz * num_heads, head_dim).transpose(0, 1)
    k = k.reshape(k.shape[0], bsz * num_heads, head_dim).transpose(0, 1) if static_k is None else static_k
    v = v.reshape(v.shape[0], bsz * num_heads, head_dim).transpose(0, 1) if static_v is None else static_v
    if add_zero_attn:
        k = torch.cat([k, torch.zeros(bsz * num_heads, 1, head_dim, dtype=k.dtype)], 1)
        v = torch.cat([v, torch.zeros(bsz * num_heads, 1, head_dim, dtype=v.dtype)], 1)
        if attn_mask is not None:
            attn_mask = pad(attn_mask, (0, 1))
        if key_padding_mask is not None:
            key_padding_mask = pad(key_padding_mask, (0, 1))
    src_len = k.shape[1]
    if key_padding_mask is not None:
        if tuple(key_padding_mask.shape) != (bsz, src_len):
            raise RuntimeError("Expected key_padded_mask.shape equal to %s, but got %s" % ((bsz, src_len), tuple(key_padding_mask.shape)))
        kpm = key_padding_mask.reshape(bsz, 1, 1, src_len).expand(bsz, num_heads, 1, src_len).reshape(bsz * num_heads, 1, src_len)
        attn_mask = kpm if attn_mask is None else attn_mask + kpm
    if not training:
        dropout_p = 0.0
    q_scaled = q * math.sqrt(1.0 / float(head_dim))
    scores = torch.matmul(q_scaled, k.transpose(-2, -1))
    if attn_mask is not None:
        scores = scores + attn_mask
    weights = torch.softmax(scores, -1)
    if dropout_p > 0.0:
        weights = dropout(weights, dropout_p)
    out = torch.matmul(weights, v)
    out = out.transpose(0, 1).reshape(tgt_len * bsz, embed_dim)
    out = linear(out, out_proj_weight, out_proj_bias)
    out = out.reshape(tgt_len, bsz, out.shape[1])
    if not is_batched:
        out = out.squeeze(1)
    if not need_weights:
        return out, None
    weights = weights.reshape(bsz, num_heads, tgt_len, src_len)
    if average_attn_weights:
        weights = weights.mean(1)
    if not is_batched:
        weights = weights.squeeze(0)
    return out, weights


# ---- grid sampling -------------------------------------------------------------------------
# grid_sample follows PyTorch's CPU kernels: 4-D input runs its vectorized
# kernel (GridSamplerKernel.cpp), 5-D input the scalar one (GridSampler.h),
# whose unnormalization and reflection round differently. Sampling is
# composed from gathers and elementwise ops, so the input and grid
# gradients come from autograd; integer tap positions are detached.
def _grid_unnormalize(coord, size, align_corners, vectorized):
    if vectorized:
        if align_corners:
            return (coord + 1) * ((size - 1) / 2)
        return (coord + 1) * (size / 2) - 0.5
    if align_corners:
        return ((coord + 1) / 2) * (size - 1)
    return ((coord + 1) * size - 1) / 2


def _grid_reflect(x, size, align_corners, vectorized):
    low, span = (0.0, size - 1) if align_corners else (-0.5, size)
    if span <= 0:
        return torch.zeros_like(x)
    a = (x - low).abs()
    if vectorized:
        twice = 2 * span
        extra = a - torch.trunc(a.detach() / twice) * twice
        return torch.minimum(extra, twice - extra) + low
    extra = torch.fmod(a, span)
    odd = torch.remainder(torch.floor(a.detach() / span), 2) == 1
    return torch.where(odd, (span - extra) + low, extra + low)


def _grid_pad(x, size, padding_mode, align_corners, vectorized):
    if padding_mode == "border":
        return torch.clamp(x, 0, size - 1)
    if padding_mode == "reflection":
        return torch.clamp(_grid_reflect(x, size, align_corners, vectorized), 0, size - 1)
    return x


class _GridTaps:
    """Gathers input values at integer (detached, floating) tap positions
    of an (N, *out) grid, zero where a checked tap falls outside the input."""

    def __init__(self, input, out_shape):
        self.n, self.c = input.shape[0], input.shape[1]
        self.sizes = tuple(input.shape[2:])
        self.out_shape = tuple(out_shape)
        self.count = _prod(self.out_shape)
        self.flat = input.reshape(self.n, self.c, _prod(self.sizes))

    def __call__(self, coords, check=True):
        # coords: a (N, *out) tensor per spatial dim, outermost first.
        valid = None
        index = None
        for pos, size in zip(coords, self.sizes):
            if check:
                ok = (pos > -1) & (pos < size)
                valid = ok if valid is None else valid & ok
            pos = pos.clamp(0, size - 1)
            index = pos if index is None else index * size + pos
        index = index.to(torch.int64).reshape(self.n, 1, self.count).expand(self.n, self.c, self.count)
        v = torch.gather(self.flat, 2, index).reshape((self.n, self.c) + self.out_shape)
        if check:
            v = torch.where(valid.unsqueeze(1), v, 0.0)
        return v


def _with_zero_grad(out, t):
    """`out`, reporting a zero gradient to `t` as well (PyTorch's grid
    gradient for nearest sampling is zeros, not absent)."""
    if not torch._needs_grad(t):
        return out
    res = Tensor(_k.copy(out._s), out.shape, out.dtype)
    res.requires_grad = True
    res._node = torch._Node(lambda g: (g, torch.zeros_like(t)), (out, t), "GridSampler2DBackward0")
    res._node.diff = True
    return res


def _cubic_coeffs(t):
    a = -0.75
    x = t + 1
    c0 = ((x * a - 5 * a) * x + 8 * a) * x - 4 * a
    c1 = ((t * (a + 2) - (a + 3)) * t) * t + 1
    x = 1 - t
    c2 = ((x * (a + 2) - (a + 3)) * x) * x + 1
    x = 2 - t
    c3 = ((x * a - 5 * a) * x + 8 * a) * x - 4 * a
    return (c0, c1, c2, c3)


def _grid_sample_2d(input, grid, mode, padding_mode, align_corners):
    n, c, h, w = input.shape
    taps = _GridTaps(input, grid.shape[1:3])
    gx, gy = grid[..., 0], grid[..., 1]
    zeros = padding_mode == "zeros"
    if mode == "bicubic":
        x = _grid_unnormalize(gx, w, align_corners, True)
        y = _grid_unnormalize(gy, h, align_corners, True)
        x0, y0 = torch.floor(x.detach()), torch.floor(y.detach())
        cx = [k.unsqueeze(1) for k in _cubic_coeffs(x - x0)]
        cy = [k.unsqueeze(1) for k in _cubic_coeffs(y - y0)]
        with torch.no_grad():
            xs = [_grid_pad(x0 + (i - 1), w, padding_mode, align_corners, True) for i in range(4)]
            ys = [_grid_pad(y0 + (i - 1), h, padding_mode, align_corners, True) for i in range(4)]
        out = None
        for i in range(4):
            row = None
            for j in range(4):
                term = cx[j] * taps((ys[i], xs[j]), zeros)
                row = term if row is None else row + term
            term = cy[i] * row
            out = term if out is None else out + term
        return out
    x = _grid_pad(_grid_unnormalize(gx, w, align_corners, True), w, padding_mode, align_corners, True)
    y = _grid_pad(_grid_unnormalize(gy, h, align_corners, True), h, padding_mode, align_corners, True)
    if mode == "nearest":
        with torch.no_grad():
            xr, yr = torch.round(x), torch.round(y)
        return _with_zero_grad(taps((yr, xr), zeros), grid)
    x0, y0 = torch.floor(x.detach()), torch.floor(y.detach())
    we = x - x0
    ea = 1 - we
    ns = y - y0
    so = 1 - ns
    x1, y1 = x0 + 1, y0 + 1
    # The east and south taps can lie past the edge even when clamped.
    out = taps((y0, x0), zeros) * (so * ea).unsqueeze(1)
    out = out + taps((y0, x1)) * (so * we).unsqueeze(1)
    out = out + taps((y1, x0)) * (ns * ea).unsqueeze(1)
    return out + taps((y1, x1)) * (ns * we).unsqueeze(1)


def _grid_sample_3d(input, grid, mode, padding_mode, align_corners):
    n, c, d, h, w = input.shape
    taps = _GridTaps(input, grid.shape[1:4])
    x = _grid_pad(_grid_unnormalize(grid[..., 0], w, align_corners, False), w, padding_mode, align_corners, False)
    y = _grid_pad(_grid_unnormalize(grid[..., 1], h, align_corners, False), h, padding_mode, align_corners, False)
    z = _grid_pad(_grid_unnormalize(grid[..., 2], d, align_corners, False), d, padding_mode, align_corners, False)
    if mode == "nearest":
        with torch.no_grad():
            xr, yr, zr = torch.round(x), torch.round(y), torch.round(z)
        return _with_zero_grad(taps((zr, yr, xr)), grid)
    x0, y0, z0 = torch.floor(x.detach()), torch.floor(y.detach()), torch.floor(z.detach())
    x1, y1, z1 = x0 + 1, y0 + 1, z0 + 1
    wx0, wx1 = x1 - x, x - x0
    wy0, wy1 = y1 - y, y - y0
    wz0, wz1 = z1 - z, z - z0
    out = None
    # PyTorch's order: tnw, tne, tsw, tse, bnw, bne, bsw, bse.
    for zc, wz in ((z0, wz0), (z1, wz1)):
        for yc, wy in ((y0, wy0), (y1, wy1)):
            for xc, wx in ((x0, wx0), (x1, wx1)):
                term = taps((zc, yc, xc)) * (wx * wy * wz).unsqueeze(1)
                out = term if out is None else out + term
    return out


def grid_sample(input, grid, mode="bilinear", padding_mode="zeros", align_corners=None):
    if mode not in ("bilinear", "nearest", "bicubic"):
        raise ValueError("nn.functional.grid_sample(): expected mode to be 'bilinear', 'nearest' or 'bicubic', but got: '%s'" % mode)
    if padding_mode not in ("zeros", "border", "reflection"):
        raise ValueError("nn.functional.grid_sample(): expected padding_mode to be 'zeros', 'border', or 'reflection', but got: '%s'" % padding_mode)
    align_corners = bool(align_corners)
    if input.dtype != grid.dtype:
        raise RuntimeError("grid_sampler(): expected input and grid to have same dtype, but input has %s and grid has %s" % (input.dtype, grid.dtype))
    nd = input.dim()
    if nd not in (4, 5) or grid.dim() != nd:
        raise RuntimeError("grid_sampler(): expected 4D or 5D input and grid with same number of dimensions, but got input with sizes %s and grid with sizes %s" % (list(input.shape), list(grid.shape)))
    if input.shape[0] != grid.shape[0]:
        raise RuntimeError("grid_sampler(): expected grid and input to have same batch size, but got input with sizes %s and grid with sizes %s" % (list(input.shape), list(grid.shape)))
    if grid.shape[-1] != nd - 2:
        raise RuntimeError("grid_sampler(): expected grid to have size %d in last dimension, but got grid with sizes %s" % (nd - 2, list(grid.shape)))
    for i in range(2, nd):
        if input.shape[i] <= 0:
            raise RuntimeError("grid_sampler(): expected input to have non-empty spatial dimensions, but input has sizes %s with dimension %d being empty" % (list(input.shape), i))
    if not input.dtype.is_floating_point:
        raise RuntimeError("\"grid_sampler_2d_cpu\" not implemented for '%s'" % input.dtype)
    if nd == 4:
        return _grid_sample_2d(input, grid, mode, padding_mode, align_corners)
    if mode == "bicubic":
        raise RuntimeError("grid_sampler(): bicubic interpolation only supports 4D input")
    return _grid_sample_3d(input, grid, mode, padding_mode, align_corners)


def _linspace_from_neg_one(steps, align_corners, dtype):
    if steps <= 1:
        return torch.zeros(1, dtype=dtype)
    r = torch.linspace(-1, 1, steps, dtype=dtype)
    if not align_corners:
        r = r * (steps - 1) / steps
    return r


def affine_grid(theta, size, align_corners=None):
    align_corners = bool(align_corners)
    if not theta.dtype.is_floating_point:
        raise ValueError("Expected theta to have floating point type, but got %s" % theta.dtype)
    size = [int(s) for s in size]
    if len(size) == 4:
        if theta.dim() != 3 or theta.shape[-2] != 2 or theta.shape[-1] != 3:
            raise ValueError("Expected a batch of 2D affine matrices of shape Nx2x3 for size %s. Got %s." % (torch.Size(size), theta.shape))
        spatial = size[-2:]
    elif len(size) == 5:
        if theta.dim() != 3 or theta.shape[-2] != 3 or theta.shape[-1] != 4:
            raise ValueError("Expected a batch of 3D affine matrices of shape Nx3x4 for size %s. Got %s." % (torch.Size(size), theta.shape))
        spatial = size[-3:]
    else:
        raise NotImplementedError("affine_grid only supports 4D and 5D sizes, for 2D and 3D affine transforms, respectively. Got size %s." % (torch.Size(size),))
    if not (align_corners and min(spatial) == 1) and min(size) <= 0:
        raise ValueError("Expected non-zero, positive output size. Got %s" % (torch.Size(size),))
    n = size[0]
    if theta.shape[0] != n:
        raise RuntimeError("Expected size[0] (%d) to match the batch of theta (%d)" % (n, theta.shape[0]))
    dt = theta.dtype
    # The base grid (x, y[, z], 1) of every output position, x fastest.
    lins = [_linspace_from_neg_one(s, align_corners, dt) for s in spatial]
    k = len(spatial)
    shape = tuple(spatial)
    cols = []
    for axis in range(k - 1, -1, -1):
        view = [1] * k
        view[axis] = lins[axis].shape[0]
        cols.append(lins[axis].reshape(*view).expand(*shape))
    cols.append(torch.ones(*shape, dtype=dt))
    base = torch.stack(cols, -1).reshape(_prod(shape), k + 1)
    grid = torch.matmul(base, theta.transpose(1, 2))
    return grid.reshape(*([n] + list(shape) + [k]))


# ---- CTC loss ------------------------------------------------------------------------------
def _lengths(value, name):
    if isinstance(value, Tensor):
        if value.dtype.is_floating_point or value.dtype == torch.bool:
            raise RuntimeError("%s must be integral" % name)
        return [int(v) for v in value.reshape(-1).tolist()]
    if isinstance(value, int):
        return [value]
    return [int(v) for v in value]


def _logsumexp3(a, b, c):
    """log(exp(a) + exp(b) + exp(c)) the way PyTorch's CTC kernel adds:
    about the largest, which counts as 0 when all are -inf."""
    m = torch.maximum(torch.maximum(a, b), c)
    m = torch.where(m == -_inf, 0.0, m)
    return torch.log(torch.exp(a - m) + torch.exp(b - m) + torch.exp(c - m)) + m


def ctc_loss(log_probs, targets, input_lengths, target_lengths, blank=0, reduction="mean", zero_infinity=False):
    """PyTorch's CPU CTC: the forward (alpha) recursion in log space, one
    tensor step per time step for the whole batch; backward runs the beta
    recursion and forms PyTorch's gradient, exp(log_probs) minus each
    label's posterior (which presumes log_softmax inputs, as PyTorch's
    does)."""
    if reduction not in ("none", "mean", "sum"):
        raise ValueError("%s is not a valid value for reduction" % reduction)
    lp = log_probs
    batched = lp.dim() == 3
    if not batched:
        if lp.dim() != 2:
            raise RuntimeError("ctc_loss expects 2-D (unbatched) or 3-D log_probs, got %s" % list(lp.shape))
        lp = lp.unsqueeze(1)
    il = _lengths(input_lengths, "input_lengths")
    tl = _lengths(target_lengths, "target_lengths")
    t_max, n, c = lp.shape
    if not (0 <= blank < c):
        raise RuntimeError("blank must be in label range")
    if len(il) != n:
        raise RuntimeError("input_lengths must be of size batch_size")
    if len(tl) != n:
        raise RuntimeError("target_lengths must be of size batch_size")
    if targets.dtype.is_floating_point:
        raise RuntimeError("Expected tensor for argument #2 'targets' to have one of the following scalar types: Long, Int; but got %s instead (while checking arguments for ctc_loss_cpu)" % targets.dtype)
    for v in tl:
        if v < 0:
            raise RuntimeError("Expected target_lengths to have value at least 0, but got value %d (while checking arguments for ctc_loss_cpu)" % v)
    l_max = max(tl) if tl else 0
    flat = [int(v) for v in targets.reshape(-1).tolist()]
    rows = []
    if targets.dim() == 1:
        # Concatenated targets.
        if len(flat) != sum(tl):
            raise RuntimeError("Expected tensor to have size %d at dimension 0, but got size %d for argument #2 'targets' (while checking arguments for ctc_loss_cpu)" % (sum(tl), len(flat)))
        pos = 0
        for v in tl:
            rows.append(flat[pos:pos + v])
            pos += v
    else:
        if targets.dim() != 2 or targets.shape[0] != n:
            raise RuntimeError("Expected tensor to have size %d at dimension 0, but got size %d for argument #2 'targets' (while checking arguments for ctc_loss_cpu)" % (n, targets.shape[0]))
        width = targets.shape[1]
        if width < l_max:
            raise RuntimeError("Expected tensor to have size at least %d at dimension 1, but got size %d for argument #2 'targets' (while checking arguments for ctc_loss_cpu)" % (l_max, width))
        for b in range(n):
            rows.append(flat[b * width:b * width + tl[b]])
    for v in il:
        if v < 0:
            raise RuntimeError("Expected input_lengths to have value at least 0, but got value %d (while checking arguments for ctc_loss_cpu)" % v)
        if v > t_max:
            raise RuntimeError("Expected input_lengths to have value at most %d, but got value %d (while checking arguments for ctc_loss_cpu)" % (t_max, v))
    s_len = 2 * l_max + 1
    # Extended labels: blanks between (and around) the targets; `skip`
    # marks the positions alpha may also reach from two back.
    ext, skip = [], []
    for b in range(n):
        lab = [blank] * s_len
        for i, v in enumerate(rows[b]):
            if not (0 <= v < c):
                raise IndexError("index %d is out of bounds for dimension 1 with size %d" % (v, c))
            lab[2 * i + 1] = v
        ext.append(lab)
        skip.append([s >= 2 and lab[s] != lab[s - 2] for s in range(s_len)])
    dt = lp.dtype
    lab_t = torch.tensor(ext, dtype=torch.int64).reshape(n, s_len)
    skip_t = torch.tensor(skip, dtype=torch.bool).reshape(n, s_len)
    il_t = torch.tensor(il, dtype=torch.int64)
    tl_t = torch.tensor(tl, dtype=torch.int64)
    lpd = lp.detach()
    alphas = []
    with torch.no_grad():
        lpe = torch.gather(lpd, 2, lab_t.reshape(1, n, s_len).expand(t_max, n, s_len))
        if t_max:
            neg = torch.full((n, 1), -_inf, dtype=dt)
            neg2 = torch.full((n, 2), -_inf, dtype=dt)
            positions = torch.arange(s_len).reshape(1, s_len)
            alpha = torch.where((positions == 0) | ((positions == 1) & (tl_t.reshape(n, 1) > 0)), lpe[0], -_inf)
            alphas.append(alpha)
            for t in range(1, t_max):
                a1 = torch.cat([neg, alpha[:, :-1]], 1)
                a2 = torch.where(skip_t, torch.cat([neg2, alpha[:, :-2]], 1), -_inf) if s_len > 2 else torch.full((n, s_len), -_inf, dtype=dt)
                new = _logsumexp3(alpha, a1, a2) + lpe[t]
                # Past its input length a sequence's alpha stays put.
                alpha = torch.where((il_t > t).reshape(n, 1), new, alpha)
                alphas.append(alpha)
            ends = torch.stack([(2 * tl_t).clamp(0, s_len - 1), (2 * tl_t - 1).clamp(0, s_len - 1)], 1)
            last = torch.gather(alpha, 1, ends)
            l1, l2 = last[:, 0], last[:, 1]
            top = torch.maximum(l1, l2)
            m = torch.where(top == -_inf, 0.0, top)
            ll = torch.where(tl_t > 0, torch.log(torch.exp(l1 - m) + torch.exp(l2 - m)) + m, l1)
        else:
            ll = torch.zeros(n, dtype=dt)
        # An empty input: likelihood 1 for an empty target, else 0.
        ll = torch.where(il_t == 0, torch.where(tl_t == 0, 0.0, -_inf), ll)
        nll = -ll
    raw = Tensor(_k.copy(nll._s), nll.shape, dt)
    if torch._needs_grad(lp):
        def backward(g):
            return (_ctc_grad(lpd, lpe, alphas, nll, g, lab_t, skip_t, il_t, tl_t, zero_infinity),)
        raw.requires_grad = True
        raw._node = torch._Node(backward, (lp,), "CtcLossBackward0")
    res = raw
    if zero_infinity:
        res = torch.where(res == _inf, torch.zeros((), dtype=dt), res)
    if reduction == "mean":
        return (res / tl_t.to(dt).clamp(min=1)).mean()
    if reduction == "sum":
        return res.sum()
    return res if batched else res.squeeze(0)


def _ctc_grad(lp, lpe, alphas, nll, g, lab_t, skip_t, il_t, tl_t, zero_infinity):
    t_max, n, c = lp.shape
    s_len = lpe.shape[2]
    dt = lp.dtype
    with torch.no_grad():
        if not t_max:
            return torch.zeros(t_max, n, c, dtype=dt)
        neg = torch.full((n, 1), -_inf, dtype=dt)
        neg2 = torch.full((n, 2), -_inf, dtype=dt)
        positions = torch.arange(s_len).reshape(1, s_len)
        end = 2 * tl_t.reshape(n, 1)
        # Beta starts at each sequence's last step t = il - 1, on the final
        # blank and the last label.
        init_mask = (positions == end) | ((positions == end - 1) & (end > 0))
        skip_next = torch.cat([skip_t[:, 2:], torch.zeros(n, 2, dtype=torch.bool)], 1) if s_len > 2 else torch.zeros(n, s_len, dtype=torch.bool)
        beta = torch.full((n, s_len), -_inf, dtype=dt)
        betas = [None] * t_max
        for t in range(t_max - 1, -1, -1):
            b1 = torch.cat([beta[:, 1:], neg], 1)
            b2 = torch.where(skip_next, torch.cat([beta[:, 2:], neg2], 1), -_inf) if s_len > 2 else torch.full((n, s_len), -_inf, dtype=dt)
            new = _logsumexp3(beta, b1, b2) + lpe[t]
            init = torch.where(init_mask, lpe[t], -_inf)
            beta = torch.where((il_t == t + 1).reshape(n, 1), init, torch.where((il_t > t + 1).reshape(n, 1), new, -_inf))
            betas[t] = beta
        ab = torch.stack(alphas, 0) + torch.stack(betas, 0)
        # PyTorch's kernel assigns (rather than adds) the last label's term
        # at the final step, so a target that uses the blank's index there
        # drops the final blank's term.
        last_lab = torch.gather(lab_t, 1, (end - 1).clamp(min=0)).reshape(n)
        drop = ((tl_t > 0) & (last_lab == lab_t[:, 0])).reshape(1, n, 1) & (positions == end).reshape(1, n, s_len) & (torch.arange(t_max).reshape(t_max, 1, 1) == (il_t - 1).reshape(1, n, 1))
        ab = torch.where(drop, -_inf, ab)
        # log of the summed exp(alpha + beta) over the positions carrying
        # each label, per (t, b, label).
        m = ab.amax(2, keepdim=True)
        m = torch.where(m == -_inf, 0.0, m)
        lab = lab_t.reshape(1, n, s_len).expand(t_max, n, s_len)
        acc = torch.zeros(t_max, n, c, dtype=dt).scatter_add(2, lab, torch.exp(ab - m))
        lcab = torch.log(acc) + m
        grad = (torch.exp(lp) - torch.exp(lcab + nll.reshape(1, n, 1) - lp)) * g.reshape(1, n, 1)
        live = torch.arange(t_max).reshape(t_max, 1) < il_t.reshape(1, n)
        if zero_infinity:
            live = live & (nll != _inf).reshape(1, n)
        return torch.where(live.unsqueeze(2), grad, torch.zeros((), dtype=dt))


# ---- fractional max pooling ----------------------------------------------------------------
def _fractional_starts(samples, in_size, out_size, pool):
    """PyTorch's generate_intervals for every plane at once: `samples` is
    (planes,) in the input dtype, whose arithmetic the kernel uses."""
    planes = samples.shape[0]
    last = torch.full((planes, 1), in_size - pool, dtype=torch.int64)
    if out_size <= 1:
        return last if out_size == 1 else last[:, :0]
    alpha = (in_size - pool) / (out_size - 1)
    if samples.dtype != torch.float64:
        alpha = _f32(alpha)
    i = torch.arange(out_size - 1, dtype=samples.dtype).reshape(1, out_size - 1)
    s = samples.reshape(planes, 1)
    seq = torch.trunc((i + s) * alpha).to(torch.int64) - torch.trunc(s * alpha).to(torch.int64)
    return torch.cat([seq, last], 1)


def _fractional_max_pool(input, nd, kernel_size, output_size, output_ratio, return_indices, _random_samples, name):
    x = input
    if output_size is None and output_ratio is None:
        raise ValueError("%s requires specifying either an output_size or an output_ratio" % name)
    if output_size is None:
        if nd == 2 and not isinstance(output_ratio, (tuple, list)):
            # PyTorch 2.11's F.fractional_max_pool2d takes len() of the ratio.
            raise TypeError("object of type '%s' has no len()" % type(output_ratio).__name__)
        ratio = tuple(output_ratio) if isinstance(output_ratio, (tuple, list)) else (output_ratio,)
        if len(ratio) == 1:
            ratio = ratio * nd
        if len(ratio) != nd:
            raise ValueError("%s requires output_ratio to either be a single Int or tuple of Ints." % name)
        output_size = [int(x.shape[-nd + i] * ratio[i]) for i in range(nd)]
    samples = _random_samples
    if samples is None:
        samples = torch.rand(1 if x.dim() == nd + 1 else x.size(0), x.size(-nd - 1), nd, dtype=x.dtype)
    kernel = _ntuple(kernel_size, nd, "kernel_size")
    out = _ntuple(output_size, nd, "output_size")
    if x.dim() not in (nd + 1, nd + 2) or x.numel() == 0:
        raise RuntimeError("%s(): Expected %dD or %dD tensor, but got: %s" % (name, nd + 1, nd + 2, list(x.shape)))
    unbatched = x.dim() == nd + 1
    if unbatched:
        x = x.unsqueeze(0)
    n, c = x.shape[0], x.shape[1]
    sizes = tuple(x.shape[2:])
    dims = ("time", "height", "width")[3 - nd:]
    for i in range(nd):
        if out[i] + kernel[i] - 1 > sizes[i]:
            raise RuntimeError("%s(): pool %s %d too large relative to input %s %d" % (name, dims[i], kernel[i], dims[i], sizes[i]))
    if samples.dim() != 3 or samples.shape[0] != n or samples.shape[1] != c or samples.shape[2] != nd:
        raise RuntimeError("%s(): expected _random_samples of shape (%d, %d, %d), but got %s" % (name, n, c, nd, list(samples.shape)))
    planes = n * c
    samples = samples.detach().to(x.dtype).reshape(planes, nd)
    # A 2-D sample is (width, height); a 3-D one (time, height, width).
    order = [1, 0] if nd == 2 else [0, 1, 2]
    index = None
    for i in range(nd):
        starts = _fractional_starts(samples[:, order[i]], sizes[i], out[i], kernel[i])
        view = [planes] + [1] * (2 * nd)
        view[1 + i] = out[i]
        kview = [1] * (1 + 2 * nd)
        kview[1 + nd + i] = kernel[i]
        pos = starts.reshape(*view) + torch.arange(kernel[i]).reshape(*kview)
        index = pos if index is None else index * sizes[i] + pos
    total = _prod(out)
    window = _prod(kernel)
    index = index.expand(*([planes] + list(out) + list(kernel))).reshape(planes, total, window)
    values = torch.gather(x.reshape(planes, _prod(sizes)), 1, index.reshape(planes, total * window)).reshape(planes, total, window)
    best, arg = values.max(2)
    shape = [n, c] + list(out)
    result = best.reshape(*shape)
    if not return_indices:
        return result.squeeze(0) if unbatched else result
    idx = torch.gather(index, 2, arg.unsqueeze(2)).reshape(*shape)
    if unbatched:
        return result.squeeze(0), idx.squeeze(0)
    return result, idx


def fractional_max_pool2d(input, kernel_size, output_size=None, output_ratio=None, return_indices=False, _random_samples=None):
    return _fractional_max_pool(input, 2, kernel_size, output_size, output_ratio, return_indices, _random_samples, "fractional_max_pool2d")


def fractional_max_pool3d(input, kernel_size, output_size=None, output_ratio=None, return_indices=False, _random_samples=None):
    return _fractional_max_pool(input, 3, kernel_size, output_size, output_ratio, return_indices, _random_samples, "fractional_max_pool3d")


def fractional_max_pool2d_with_indices(input, kernel_size, output_size=None, output_ratio=None, return_indices=False, _random_samples=None):
    return _fractional_max_pool(input, 2, kernel_size, output_size, output_ratio, True, _random_samples, "fractional_max_pool2d")


def fractional_max_pool3d_with_indices(input, kernel_size, output_size=None, output_ratio=None, return_indices=False, _random_samples=None):
    return _fractional_max_pool(input, 3, kernel_size, output_size, output_ratio, True, _random_samples, "fractional_max_pool3d")
