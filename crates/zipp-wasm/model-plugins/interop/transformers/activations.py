import torch
import torch.nn.functional as F

ACT2FN = {
    "silu": F.silu,
    "swish": F.silu,
    "gelu": F.gelu,
    "relu": F.relu,
    "tanh": torch.tanh,
    "sigmoid": torch.sigmoid,
}
