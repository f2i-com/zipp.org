"""A per-distribution submodule of torch.distributions for Zipp
(torch.distributions.normal, .gamma, .utils, .distribution, ...): PyTorch
splits the package into one module per file, and code imports from them
(`from torch.distributions.normal import Normal`,
`from torch.distributions.utils import broadcast_all`). Every such module is
this one file: it re-exports the package's objects (the same objects, so
isinstance checks and the KL registry see one class), whichever module name
it is bundled under."""
from torch.distributions import *
from torch.distributions import (constraints, transforms, kl, Distribution, ExponentialFamily,
                                 TransformedDistribution, ExpRelaxedCategorical, LogitRelaxedBernoulli,
                                 ConstraintRegistry, biject_to, transform_to, register_kl, kl_divergence,
                                 broadcast_all, lazy_property, logits_to_probs, probs_to_logits, clamp_probs,
                                 tril_matrix_to_vec, vec_to_tril_matrix, euler_constant, _standard_normal,
                                 _sum_rightmost, _Number, _batch_mahalanobis, _batch_mv,
                                 _batch_capacitance_tril, _batch_lowrank_logdet, _batch_lowrank_mahalanobis,
                                 _precision_to_scale_tril, _log_modified_bessel_fn, _rejection_sample,
                                 _mvdigamma, _clamp_above_eps, _kumaraswamy_moments, _clamp_by_zero)
import math
import torch
from math import nan, inf
