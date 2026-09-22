"""torch.nn.functional for Zipp."""
import math
import torch
from torch import Tensor
import _zipp_tensor as _k


def linear(x, weight, bias=None):
    if torch._graph_recording and getattr(x, "_zipp_graph", False):
        return x.linear(weight, bias)
    out = torch.matmul(x, weight.transpose(0, 1))
    return out if bias is None else out + bias


def embedding(idx, weight, padding_idx=None):
    if idx.dtype.is_floating_point:
        raise RuntimeError("Expected tensor for argument #1 'indices' to have one of the following scalar types: Long, Int; but got torch.FloatTensor instead")
    flat = idx.reshape(-1)
    rows = torch.index_select(weight, 0, flat)
    if padding_idx is not None and torch.is_grad_enabled() and weight.requires_grad:
        # The padding row is looked up but receives no gradient.
        if padding_idx < 0:
            padding_idx += weight.shape[0]
        rows = torch.where((flat != padding_idx).unsqueeze(1), rows, rows.detach())
    return rows.reshape(*(tuple(idx.shape) + (weight.shape[1],)))


def pad(x, padding, mode="constant", value=0.0):
    if mode != "constant":
        raise NotImplementedError("only constant padding is supported")
    padding = list(padding)
    if len(padding) % 2:
        raise ValueError("Padding length must be divisible by 2")
    out = x
    ndim = len(x.shape)
    for i in range(0, len(padding), 2):
        left, right = int(padding[i]), int(padding[i + 1])
        dim = ndim - 1 - i // 2
        if left == 0 and right == 0:
            continue
        if left < 0 or right < 0:
            raise NotImplementedError("negative padding is not supported")
        out = _pad_dim(out, dim, left, right, float(value))
    return out


def _pad_dim(x, dim, left, right, value):
    if dim == len(x.shape) - 1:
        storage, shape = _k.pad_last(x._s, x.shape, left, right, value)
        out = Tensor(storage, shape, x.dtype)
        if torch._needs_grad(x):
            out.requires_grad = True
            out._node = torch._Node(lambda g: (g.narrow(dim, left, x.shape[dim]),), (x,), "ConstantPadNd")
        return out
    moved = torch.movedim(x, dim, -1)
    return torch.movedim(_pad_dim(moved, len(x.shape) - 1, left, right, value), -1, dim)


def conv1d(x, weight, bias=None, stride=1, padding=0, dilation=1, groups=1):
    if stride != 1 or dilation != 1 or groups != 1:
        raise NotImplementedError("conv1d on Zipp supports stride=1, dilation=1, groups=1")
    if padding:
        x = pad(x, (padding, padding))
    storage, shape = _k.conv1d(x._s, x.shape, weight._s, weight.shape, None if bias is None else bias._s)
    out = Tensor(storage, shape, x.dtype)
    if torch._needs_grad(x, weight, bias):
        def backward(g):
            gx, gw, gb = _k.conv1d_backward(x._s, x.shape, weight._s, weight.shape, g._s)
            return (Tensor(gx, x.shape, x.dtype), Tensor(gw, weight.shape, weight.dtype), Tensor(gb, (weight.shape[0],), weight.dtype))
        out.requires_grad = True
        out._node = torch._Node(backward, (x, weight, bias), "Conv1d")
    return out


def _conv_pair(value, name, minimum=1):
    pair = (value, value) if isinstance(value, int) else tuple(value) if isinstance(value, (tuple, list)) else ()
    if len(pair) != 2 or any(isinstance(v, bool) or not isinstance(v, int) or v < minimum for v in pair):
        raise ValueError(name + " must be an integer or pair of integers >= " + str(minimum))
    return pair


def conv2d(x, weight, bias=None, stride=1, padding=0, dilation=1, groups=1):
    if torch._graph_recording and getattr(x, "_zipp_graph", False):
        raise NotImplementedError("conv2d currently supports eager CPU tensors only")
    stride = _conv_pair(stride, "stride")
    padding = _conv_pair(padding, "padding", 0)
    dilation = _conv_pair(dilation, "dilation")
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
            gx, gw, gb = _k.conv2d_backward(x._s, x.shape, weight._s, weight.shape, g._s, stride, padding, dilation, groups)
            return (Tensor(gx, x.shape, x.dtype), Tensor(gw, weight.shape, weight.dtype), None if bias is None else Tensor(gb, bias.shape, bias.dtype))
        out.requires_grad = True
        out._node = torch._Node(backward, (x, weight, bias), "Conv2d")
    return out


def silu(x, inplace=False):
    return torch.silu(x)


def relu(x, inplace=False):
    return torch.relu(x)


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


def leaky_relu(x, negative_slope=0.01):
    return torch.where(x > 0, x, x * negative_slope)


def elu(x, alpha=1.0):
    # As in softplus: clamp the unused exp branch so a large input's gradient is not nan.
    return torch.where(x > 0, x, alpha * (torch.exp(x.clamp(max=0)) - 1))


def softmax(x, dim=None, dtype=None):
    if dim is None:
        dim = 0 if len(x.shape) in (0, 1, 3) else 1
    return torch.softmax(x, dim, dtype=dtype)


def log_softmax(x, dim=None, dtype=None):
    if dim is None:
        dim = 0 if len(x.shape) in (0, 1, 3) else 1
    return torch.log_softmax(x, dim, dtype=dtype)


def normalize(x, p=2.0, dim=1, eps=1e-12):
    n = x.norm(p, dim, keepdim=True).clamp(min=eps)
    return x / n.expand_as(x)


def one_hot(idx, num_classes=-1):
    if idx.dtype.is_floating_point:
        raise RuntimeError("one_hot is only applicable to index tensor.")
    if num_classes < 0:
        num_classes = int(idx.max().item()) + 1 if idx.numel() else 0
    return torch.one_hot_(idx, int(num_classes))


def dropout(x, p=0.5, training=True, inplace=False):
    if not training or p == 0:
        return x
    mask = (torch.rand(*x.shape) >= p).to(x.dtype)
    return x * mask / (1 - p)


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


def mse_loss(a, b, reduction="mean"):
    d = (a - b) ** 2
    return d.mean() if reduction == "mean" else d.sum() if reduction == "sum" else d


def l1_loss(a, b, reduction="mean"):
    d = (a - b).abs()
    return d.mean() if reduction == "mean" else d.sum() if reduction == "sum" else d


def nll_loss(log_probs, target, reduction="mean"):
    if torch._graph_recording and getattr(log_probs, "_zipp_graph", False):
        raise NotImplementedError("GPU nll_loss is not captured; use F.cross_entropy on the logits with integer class targets")
    if len(log_probs.shape) != 2:
        raise ValueError("nll_loss expects [N, C] log-probabilities")
    picked = log_probs[torch.arange(log_probs.shape[0]), target]
    loss = -picked
    return loss.mean() if reduction == "mean" else loss.sum() if reduction == "sum" else loss


def cross_entropy(logits, target, weight=None, reduction="mean", label_smoothing=0.0):
    if torch._graph_recording and getattr(logits, "_zipp_graph", False) and not target.dtype.is_floating_point:
        # Integer class targets take the fused graph operation; probability
        # targets compose from log_softmax below.
        return logits.cross_entropy(target, weight, reduction, label_smoothing)
    # Classes are dim 1 of [N, C, d1, ...] (the last dim of [C] or [N, C]).
    class_dim = 1 if len(logits.shape) > 2 else -1
    n_classes = logits.shape[class_dim]
    if weight is not None:
        shape = [1] * len(logits.shape)
        shape[class_dim] = n_classes
        class_weight = weight.reshape(*shape)
    if target.dtype.is_floating_point:
        # Probability targets; the mean is over the N * d1 * ... positions (not the weights).
        if label_smoothing > 0:
            target = target * (1 - label_smoothing) + label_smoothing / n_classes
        terms = target * log_softmax(logits, class_dim)
        if weight is not None:
            terms = terms * class_weight
        loss = -terms.sum(class_dim)
        return loss.mean() if reduction == "mean" else loss.sum() if reduction == "sum" else loss
    target_shape = tuple(target.shape)
    if len(logits.shape) == 1:
        logits = logits.unsqueeze(0)
    elif len(logits.shape) > 2:
        # [N, C, d1, ...]: put classes last and flatten.
        moved = torch.movedim(logits, 1, -1)
        logits = moved.reshape(-1, moved.shape[-1])
    target = target.reshape(-1)
    lp = log_softmax(logits, -1)
    if weight is None and label_smoothing == 0:
        loss = nll_loss(lp, target, reduction)
        return loss.reshape(*target_shape) if reduction == "none" else loss
    # PyTorch's weighted / label-smoothed form: each position's loss is
    # (1 - eps) * w[t] * -lp[t] + eps / C * sum_c w[c] * -lp[c], and the mean
    # divides by the summed target weights (the count without a weight).
    loss = -lp[torch.arange(lp.shape[0]), target]
    if weight is not None:
        target_weight = weight[target]
        loss = loss * target_weight
    if label_smoothing > 0:
        smooth = -(lp * weight).sum(-1) if weight is not None else -lp.sum(-1)
        loss = loss * (1 - label_smoothing) + smooth * (label_smoothing / n_classes)
    if reduction == "none":
        return loss.reshape(*target_shape)
    if reduction == "sum":
        return loss.sum()
    return loss.sum() / (target_weight.sum() if weight is not None else target.shape[0])


def binary_cross_entropy_with_logits(logits, target, weight=None, reduction="mean", pos_weight=None):
    if tuple(logits.shape) != tuple(target.shape):
        raise ValueError("Target size (%s) must be the same as input size (%s)" % (tuple(target.shape), tuple(logits.shape)))
    if pos_weight is not None:
        # (1 - y) x + (1 + (pw - 1) y) softplus(-x), with softplus(-x) = max(-x, 0) + log(1 + exp(-|x|)).
        softplus_neg = torch.clamp(-logits, min=0) + torch.log(1 + torch.exp(-torch.abs(logits)))
        loss = (1 - target) * logits + (1 + (pos_weight - 1) * target) * softplus_neg
    else:
        # max(x, 0) - x * y + log(1 + exp(-|x|)), the numerically stable form.
        loss = torch.clamp(logits, min=0) - logits * target + torch.log(1 + torch.exp(-torch.abs(logits)))
    if weight is not None:
        loss = loss * weight
    return loss.mean() if reduction == "mean" else loss.sum() if reduction == "sum" else loss


def binary_cross_entropy(probs, target, weight=None, reduction="mean"):
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
    return loss.mean() if reduction == "mean" else loss.sum() if reduction == "sum" else loss


def cosine_similarity(a, b, dim=1, eps=1e-8):
    return (a * b).sum(dim) / (a.norm(2, dim) * b.norm(2, dim)).clamp(min=eps)
