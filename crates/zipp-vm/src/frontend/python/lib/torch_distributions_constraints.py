"""torch.distributions.constraints for Zipp: re-exports the constraint
objects torch.distributions defines (the same objects, so
`torch.distributions.constraints.positive is constraints.positive`), and the
constraint classes PyTorch's module also holds."""
from torch.distributions import constraints as _ns
from torch.distributions import (_Boolean, _Cat, _CorrCholesky, _Dependent, _DependentProperty,
                                 _GreaterThan, _GreaterThanEq, _HalfOpenInterval, _IndependentConstraint,
                                 _IntegerGreaterThan, _IntegerInterval, _IntegerLessThan, _Interval,
                                 _LessThan, _LowerCholesky, _LowerTriangular, _Multinomial, _OneHot,
                                 _PositiveDefinite, _PositiveSemidefinite, _Real, _Simplex, _Square,
                                 _Stack, _Symmetric)
import torch

__all__ = [
    "Constraint", "boolean", "cat", "corr_cholesky", "dependent", "dependent_property",
    "greater_than", "greater_than_eq", "independent", "integer_interval", "interval",
    "half_open_interval", "is_dependent", "less_than", "lower_cholesky", "lower_triangular",
    "MixtureSameFamilyConstraint", "multinomial", "nonnegative", "nonnegative_integer", "one_hot",
    "positive", "positive_semidefinite", "positive_definite", "positive_integer", "real",
    "real_vector", "simplex", "square", "stack", "symmetric", "unit_interval",
]

Constraint = _ns.Constraint
MixtureSameFamilyConstraint = _ns.MixtureSameFamilyConstraint
boolean = _ns.boolean
cat = _ns.cat
corr_cholesky = _ns.corr_cholesky
dependent = _ns.dependent
dependent_property = _ns.dependent_property
greater_than = _ns.greater_than
greater_than_eq = _ns.greater_than_eq
half_open_interval = _ns.half_open_interval
independent = _ns.independent
integer_interval = _ns.integer_interval
interval = _ns.interval
is_dependent = _ns.is_dependent
less_than = _ns.less_than
lower_cholesky = _ns.lower_cholesky
lower_triangular = _ns.lower_triangular
multinomial = _ns.multinomial
nonnegative = _ns.nonnegative
nonnegative_integer = _ns.nonnegative_integer
one_hot = _ns.one_hot
positive = _ns.positive
positive_definite = _ns.positive_definite
positive_integer = _ns.positive_integer
positive_semidefinite = _ns.positive_semidefinite
real = _ns.real
real_vector = _ns.real_vector
simplex = _ns.simplex
square = _ns.square
stack = _ns.stack
symmetric = _ns.symmetric
unit_interval = _ns.unit_interval
