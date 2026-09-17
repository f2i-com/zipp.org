import torch


def _default_rope(config, device=None, seq_len=None, **kwargs):
    base = config.rope_theta
    dim = getattr(config, "head_dim", None) or config.hidden_size // config.num_attention_heads
    inverse = 1.0 / (base ** (torch.arange(0, dim, 2).float() / dim))
    return inverse, 1.0


ROPE_INIT_FUNCTIONS = {"default": _default_rope}


def dynamic_rope_update(function):
    """Only the dynamic rope types rescale between calls; default never does."""
    return function


def rope_config_validation(config, ignore_keys=None):
    return None
