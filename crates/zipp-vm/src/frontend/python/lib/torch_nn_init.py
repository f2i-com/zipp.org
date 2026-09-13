"""torch.nn.init for Zipp: in-place initializers on the shared generator."""
import math
import torch


def _fans(t):
    if len(t.shape) < 2:
        raise ValueError("Fan in and fan out can not be computed for tensor with fewer than 2 dimensions")
    receptive = 1
    for d in t.shape[2:]:
        receptive *= d
    return t.shape[1] * receptive, t.shape[0] * receptive


def calculate_gain(nonlinearity, param=None):
    if nonlinearity in ("linear", "conv1d", "conv2d", "conv3d", "sigmoid"):
        return 1
    if nonlinearity == "tanh":
        return 5.0 / 3
    if nonlinearity == "relu":
        return math.sqrt(2.0)
    if nonlinearity == "leaky_relu":
        slope = 0.01 if param is None else param
        return math.sqrt(2.0 / (1 + slope ** 2))
    if nonlinearity == "selu":
        return 3.0 / 4
    raise ValueError("Unsupported nonlinearity %s" % nonlinearity)


def constant_(t, val):
    with torch.no_grad():
        t.fill_(val)
    return t


def zeros_(t):
    return constant_(t, 0.0)


def ones_(t):
    return constant_(t, 1.0)


def uniform_(t, a=0.0, b=1.0, generator=None):
    with torch.no_grad():
        t.uniform_(a, b, generator=generator)
    return t


def normal_(t, mean=0.0, std=1.0, generator=None):
    with torch.no_grad():
        t.normal_(mean, std, generator=generator)
    return t


def kaiming_uniform_(t, a=0, mode="fan_in", nonlinearity="leaky_relu", generator=None):
    fan_in, fan_out = _fans(t)
    fan = fan_in if mode == "fan_in" else fan_out
    gain = calculate_gain(nonlinearity, a)
    bound = math.sqrt(3.0) * gain / math.sqrt(fan)
    return uniform_(t, -bound, bound, generator)


def kaiming_normal_(t, a=0, mode="fan_in", nonlinearity="leaky_relu", generator=None):
    fan_in, fan_out = _fans(t)
    fan = fan_in if mode == "fan_in" else fan_out
    std = calculate_gain(nonlinearity, a) / math.sqrt(fan)
    return normal_(t, 0.0, std, generator)


def xavier_uniform_(t, gain=1.0, generator=None):
    fan_in, fan_out = _fans(t)
    bound = gain * math.sqrt(6.0 / (fan_in + fan_out))
    return uniform_(t, -bound, bound, generator)


def xavier_normal_(t, gain=1.0, generator=None):
    fan_in, fan_out = _fans(t)
    return normal_(t, 0.0, gain * math.sqrt(2.0 / (fan_in + fan_out)), generator)


def eye_(t):
    with torch.no_grad():
        t.copy_(torch.eye(t.shape[0], t.shape[1], dtype=t.dtype))
    return t


def orthogonal_(t, gain=1.0):
    return xavier_uniform_(t, gain)
