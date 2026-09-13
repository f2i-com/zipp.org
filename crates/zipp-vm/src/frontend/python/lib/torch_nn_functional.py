"""torch.nn.functional for Zipp."""
import math
import torch
from torch import Tensor
import _zipp_tensor as _k


def linear(x, weight, bias=None):
    out = torch.matmul(x, weight.transpose(0, 1))
    return out if bias is None else out + bias


def embedding(idx, weight, padding_idx=None):
    if idx.dtype.is_floating_point:
        raise RuntimeError("Expected tensor for argument #1 'indices' to have one of the following scalar types: Long, Int; but got torch.FloatTensor instead")
    flat = idx.reshape(-1)
    rows = torch.index_select(weight, 0, flat)
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


def silu(x, inplace=False):
    return torch.silu(x)


def relu(x, inplace=False):
    return torch.relu(x)


def gelu(x, approximate="none"):
    return 0.5 * x * (1 + torch.tanh(math.sqrt(2 / math.pi) * (x + 0.044715 * x * x * x)))


def sigmoid(x):
    return torch.sigmoid(x)


def tanh(x):
    return torch.tanh(x)


def softplus(x, beta=1.0, threshold=20.0):
    return torch.where(x * beta > threshold, x, torch.log(1 + torch.exp(x * beta)) / beta)


def leaky_relu(x, negative_slope=0.01):
    return torch.where(x > 0, x, x * negative_slope)


def elu(x, alpha=1.0):
    return torch.where(x > 0, x, alpha * (torch.exp(x) - 1))


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
    if len(log_probs.shape) != 2:
        raise ValueError("nll_loss expects [N, C] log-probabilities")
    picked = log_probs[torch.arange(log_probs.shape[0]), target]
    loss = -picked
    return loss.mean() if reduction == "mean" else loss.sum() if reduction == "sum" else loss


def cross_entropy(logits, target, weight=None, reduction="mean", label_smoothing=0.0):
    if target.dtype.is_floating_point:
        lp = log_softmax(logits, -1)
        loss = -(target * lp).sum(-1)
        return loss.mean() if reduction == "mean" else loss.sum() if reduction == "sum" else loss
    if len(logits.shape) == 1:
        logits = logits.unsqueeze(0)
        target = target.reshape(1)
    if len(logits.shape) > 2:
        # [N, C, d1, ...]: put classes last and flatten.
        moved = torch.movedim(logits, 1, -1)
        logits = moved.reshape(-1, moved.shape[-1])
        target = target.reshape(-1)
    return nll_loss(log_softmax(logits, -1), target, reduction)


def binary_cross_entropy_with_logits(logits, target, weight=None, reduction="mean", pos_weight=None):
    if tuple(logits.shape) != tuple(target.shape):
        raise ValueError("Target size (%s) must be the same as input size (%s)" % (tuple(target.shape), tuple(logits.shape)))
    # max(x, 0) - x * y + log(1 + exp(-|x|)), the numerically stable form.
    loss = torch.clamp(logits, min=0) - logits * target + torch.log(1 + torch.exp(-torch.abs(logits)))
    if weight is not None:
        loss = loss * weight
    return loss.mean() if reduction == "mean" else loss.sum() if reduction == "sum" else loss


def binary_cross_entropy(probs, target, weight=None, reduction="mean"):
    p = probs.clamp(1e-12, 1 - 1e-12)
    loss = -(target * torch.log(p) + (1 - target) * torch.log(1 - p))
    return loss.mean() if reduction == "mean" else loss.sum() if reduction == "sum" else loss


def cosine_similarity(a, b, dim=1, eps=1e-8):
    return (a * b).sum(dim) / (a.norm(2, dim) * b.norm(2, dim)).clamp(min=eps)
