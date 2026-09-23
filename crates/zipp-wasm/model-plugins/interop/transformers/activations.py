import math

import torch
import torch.nn.functional as F


def _gelu_new(x):
    # transformers' NewGELUActivation, written as it writes it (GPT-2, GPT-Neo).
    return 0.5 * x * (1.0 + torch.tanh(math.sqrt(2.0 / math.pi) * (x + 0.044715 * torch.pow(x, 3.0))))


def _gelu_fast(x):
    return 0.5 * x * (1.0 + torch.tanh(x * 0.7978845608 * (1.0 + 0.044715 * x * x)))


def _quick_gelu(x):
    return x * torch.sigmoid(1.702 * x)

ACT2FN = {
    "silu": F.silu,
    "swish": F.silu,
    "gelu": F.gelu,
    "gelu_new": _gelu_new,
    "gelu_fast": _gelu_fast,
    "gelu_pytorch_tanh": lambda x: F.gelu(x, approximate="tanh"),
    "quick_gelu": _quick_gelu,
    "relu": F.relu,
    "tanh": torch.tanh,
    "sigmoid": torch.sigmoid,
}
