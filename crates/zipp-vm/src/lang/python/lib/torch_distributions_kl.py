"""torch.distributions.kl for Zipp: re-exports `kl_divergence`,
`register_kl` and the registry torch.distributions defines (the same
objects, so a pair registered through either is seen by both)."""
from torch.distributions import kl_divergence, register_kl
from torch.distributions import _KL_REGISTRY, _KL_MEMOIZE, _dispatch_kl, _infinite_like, _x_log_x
from torch.distributions import _batch_mahalanobis, _batch_trace_XXT, _batch_lowrank_logdet, _batch_lowrank_mahalanobis
from torch.distributions import euler_constant as _euler_gamma
import math
import torch

__all__ = ["register_kl", "kl_divergence"]
