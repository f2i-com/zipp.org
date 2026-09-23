# torch.distributions behaviour that is not a number: shapes, flags,
# constraints, validation errors, expand, the KL registry and reprs. Run
# under PyTorch 2.11 (`python dist_api.py > api_expected.txt`, done by gen.py);
# Zipp must print the same lines.
import torch
import torch.distributions as D
from torch.distributions import constraints, transforms, kl_divergence, register_kl


def show(label, value):
    if isinstance(value, torch.Tensor):
        value = value.tolist()
    elif isinstance(value, torch.Size):
        value = list(value)
    print(label, value)


def error(label, fn):
    try:
        fn()
    except Exception as e:
        print(label, type(e).__name__, str(e).split("\n")[0])
    else:
        print(label, "no error")


F64 = torch.float64
loc = torch.tensor([0.0, 1.0, 2.0])
scale = torch.tensor([1.0, 2.0, 0.5])

# shapes and flags
dists = [
    ("Normal", D.Normal(loc, scale)),
    ("Normal0", D.Normal(0.0, 1.0)),
    ("LogNormal", D.LogNormal(loc, scale)),
    ("Uniform", D.Uniform(0.0, torch.tensor([1.0, 2.0]))),
    ("Bernoulli", D.Bernoulli(torch.tensor([0.3, 0.6]))),
    ("Categorical", D.Categorical(torch.ones(2, 4) / 4)),
    ("OneHot", D.OneHotCategorical(torch.ones(2, 4) / 4)),
    ("Binomial", D.Binomial(5, torch.tensor([0.3, 0.6]))),
    ("Multinomial", D.Multinomial(4, torch.ones(3) / 3)),
    ("Poisson", D.Poisson(torch.tensor([1.0, 2.0]))),
    ("Geometric", D.Geometric(torch.tensor([0.3]))),
    ("Exponential", D.Exponential(torch.tensor([1.0]))),
    ("Laplace", D.Laplace(0.0, 1.0)),
    ("Cauchy", D.Cauchy(0.0, 1.0)),
    ("Gamma", D.Gamma(torch.tensor([1.0, 2.0]), 1.0)),
    ("Beta", D.Beta(torch.tensor([1.0, 2.0]), torch.tensor([3.0, 4.0]))),
    ("Dirichlet", D.Dirichlet(torch.ones(2, 3))),
    ("Chi2", D.Chi2(torch.tensor([3.0]))),
    ("StudentT", D.StudentT(torch.tensor([3.0, 4.0]))),
    ("HalfNormal", D.HalfNormal(torch.tensor([1.0]))),
    ("HalfCauchy", D.HalfCauchy(torch.tensor([1.0]))),
    ("MVN", D.MultivariateNormal(torch.zeros(2, 3), torch.eye(3))),
    ("LowRank", D.LowRankMultivariateNormal(torch.zeros(3), torch.ones(3, 1), torch.ones(3))),
    ("Independent", D.Independent(D.Normal(torch.zeros(2, 3), 1.0), 1)),
    ("Mixture", D.MixtureSameFamily(D.Categorical(torch.ones(3)), D.Normal(torch.zeros(3), 1.0))),
    ("RelaxedBernoulli", D.RelaxedBernoulli(torch.tensor(0.5), torch.tensor([0.3]))),
    ("RelaxedOneHot", D.RelaxedOneHotCategorical(torch.tensor(0.5), torch.ones(2, 3) / 3)),
    ("Transformed", D.TransformedDistribution(D.Normal(0.0, 1.0), [transforms.ExpTransform()])),
    ("Stick", D.TransformedDistribution(D.Normal(torch.zeros(3), 1.0), [transforms.StickBreakingTransform()])),
]
for name, d in dists:
    s = d.sample((2,))
    print(name, list(d.batch_shape), list(d.event_shape), d.has_rsample, d.has_enumerate_support,
          list(s.shape), s.dtype, repr(d.support))

# sample dtype follows the parameters
print(D.Normal(torch.zeros(2, dtype=F64), 1.0).sample().dtype, D.Categorical(torch.ones(3)).sample().dtype,
      D.Poisson(torch.ones(2, dtype=F64)).sample().dtype, D.Bernoulli(torch.tensor(0.5, dtype=F64)).sample().dtype)

# reprs
print(D.Normal(0.0, 1.0))
print(D.Normal(loc, scale))
print(D.Categorical(torch.ones(3)))
print(D.Independent(D.Normal(loc, scale), 1))
print(D.Uniform(0.0, 2.0))
print(constraints.positive, constraints.real, constraints.simplex, constraints.unit_interval, constraints.real_vector)
print(constraints.interval(0.0, 2.0), constraints.integer_interval(0, 3), constraints.greater_than_eq(1.0), constraints.less_than(3.0))
print(constraints.nonnegative_integer, constraints.positive_definite, constraints.lower_cholesky, constraints.independent(constraints.positive, 2))
print(transforms.ExpTransform(), transforms.ExpTransform().inv, transforms.ComposeTransform([transforms.ExpTransform(), transforms.SigmoidTransform()]))
print(D.biject_to(constraints.positive), D.transform_to(constraints.simplex), D.biject_to(constraints.simplex))

# constraints
x = torch.tensor([-1.0, 0.0, 0.5, 1.0, 2.0])
for name in ("real", "positive", "nonnegative", "unit_interval", "boolean", "nonnegative_integer", "positive_integer"):
    show(name, getattr(constraints, name).check(x))
show("interval", constraints.interval(-0.5, 1.0).check(x))
show("half_open", constraints.half_open_interval(0.0, 1.0).check(x))
show("greater_than", constraints.greater_than(0.5).check(x))
show("simplex", constraints.simplex.check(torch.tensor([[0.2, 0.8], [0.5, 0.6], [-0.1, 1.1]])))
show("one_hot", constraints.one_hot.check(torch.tensor([[0.0, 1.0], [1.0, 1.0]])))
show("real_vector", constraints.real_vector.check(torch.tensor([[0.0, float("nan")], [1.0, 2.0]])))
show("lower_cholesky", constraints.lower_cholesky.check(torch.tensor([[[1.0, 0.0], [0.5, 2.0]], [[1.0, 0.1], [0.5, 2.0]], [[-1.0, 0.0], [0.5, 2.0]]])))
show("positive_definite", constraints.positive_definite.check(torch.tensor([[[2.0, 0.5], [0.5, 1.0]], [[1.0, 2.0], [2.0, 1.0]]])))
show("symmetric", constraints.symmetric.check(torch.tensor([[[2.0, 0.5], [0.5, 1.0]], [[1.0, 2.0], [0.0, 1.0]]])))
show("multinomial", constraints.multinomial(3).check(torch.tensor([[1.0, 2.0], [2.0, 2.0]])))
print(constraints.is_dependent(constraints.dependent), constraints.is_dependent(constraints.real), constraints.real.event_dim,
      constraints.simplex.event_dim, constraints.positive_definite.event_dim, constraints.boolean.is_discrete)
print(D.Uniform(0.0, 2.0).support.check(torch.tensor([1.0, 3.0])).tolist(), D.Binomial(3, torch.tensor(0.5)).support.check(torch.tensor([1.0, 4.0])).tolist())
print(D.Categorical(torch.ones(4)).support.check(torch.tensor([0, 3, 4])).tolist(), D.Independent(D.Normal(torch.zeros(2), 1.0), 1).support.event_dim)

# argument validation
error("normal scale", lambda: D.Normal(0.0, -1.0))
error("normal scale tensor", lambda: D.Normal(torch.zeros(2), torch.tensor([1.0, 0.0])))
error("bernoulli both", lambda: D.Bernoulli(probs=torch.tensor(0.5), logits=torch.tensor(0.0)))
error("bernoulli probs", lambda: D.Bernoulli(torch.tensor([1.5])))
error("categorical 0d", lambda: D.Categorical(torch.tensor(0.5)))
error("categorical simplex", lambda: D.Categorical(torch.tensor([-0.5, 1.5])))
error("uniform order", lambda: D.Uniform(1.0, 0.0))
error("dirichlet 0d", lambda: D.Dirichlet(torch.tensor(1.0)))
error("dirichlet positive", lambda: D.Dirichlet(torch.tensor([1.0, -1.0])))
error("mvn none", lambda: D.MultivariateNormal(torch.zeros(2)))
error("mvn not pd", lambda: D.MultivariateNormal(torch.zeros(2), torch.tensor([[1.0, 2.0], [2.0, 1.0]])))
error("mvn tril", lambda: D.MultivariateNormal(torch.zeros(2), scale_tril=torch.tensor([[1.0, 1.0], [0.0, 1.0]])))
error("multinomial count", lambda: D.Multinomial(torch.tensor(3), torch.ones(2)))
error("independent ndims", lambda: D.Independent(D.Normal(torch.zeros(2), 1.0), 2))
error("mixture type", lambda: D.MixtureSameFamily(D.Normal(0.0, 1.0), D.Normal(torch.zeros(2), 1.0)))
error("mixture count", lambda: D.MixtureSameFamily(D.Categorical(torch.ones(3)), D.Normal(torch.zeros(2), 1.0)))
error("geometric zero", lambda: D.Geometric(torch.tensor([0.0, 0.5])))
error("gamma rate", lambda: D.Gamma(1.0, 0.0))
error("poisson negative", lambda: D.Poisson(torch.tensor([-1.0])))
error("transforms type", lambda: D.TransformedDistribution(D.Normal(0.0, 1.0), [3]))
# sample validation
error("exp support", lambda: D.Exponential(torch.tensor(1.0)).log_prob(torch.tensor(-1.0)))
error("beta support", lambda: D.Beta(2.0, 2.0).log_prob(torch.tensor(1.5)))
error("value type", lambda: D.Normal(0.0, 1.0).log_prob(0.5))
error("event shape", lambda: D.MultivariateNormal(torch.zeros(3), torch.eye(3)).log_prob(torch.zeros(2)))
error("broadcast", lambda: D.Normal(torch.zeros(3), 1.0).log_prob(torch.zeros(2)))
error("categorical support", lambda: D.Categorical(torch.ones(3)).log_prob(torch.tensor(3)))
error("bernoulli support", lambda: D.Bernoulli(torch.tensor(0.5)).log_prob(torch.tensor(0.5)))
error("no validation", lambda: D.Exponential(torch.tensor(1.0), validate_args=False).log_prob(torch.tensor(-1.0)))
error("no validation param", lambda: D.Normal(0.0, -1.0, validate_args=False))
D.Distribution.set_default_validate_args(False)
error("default off", lambda: D.Normal(0.0, -1.0).log_prob(torch.tensor(1.0)))
D.Distribution.set_default_validate_args(True)
error("default on", lambda: D.Normal(0.0, -1.0))
error("set default", lambda: D.Distribution.set_default_validate_args(3))
error("mode", lambda: D.Distribution().mode)
error("entropy", lambda: D.Poisson(torch.tensor(1.0)).entropy())
error("enumerate", lambda: D.Independent(D.Bernoulli(torch.ones(2) / 2), 1).enumerate_support())
error("inhomogeneous", lambda: D.Binomial(torch.tensor([2.0, 3.0]), torch.tensor([0.5, 0.5])).enumerate_support())
print("validate flag", D.Normal(0.0, 1.0)._validate_args, D.Normal(0.0, 1.0, validate_args=False)._validate_args)

# expand
for name, d in (("Normal", D.Normal(torch.zeros(3), 1.0)), ("Bernoulli", D.Bernoulli(logits=torch.zeros(3))),
                ("Categorical", D.Categorical(torch.ones(3, 2))), ("Dirichlet", D.Dirichlet(torch.ones(3, 2))),
                ("MVN", D.MultivariateNormal(torch.zeros(3, 2), torch.eye(2))), ("Gamma", D.Gamma(torch.ones(3), 1.0)),
                ("LogNormal", D.LogNormal(torch.zeros(3), 1.0)), ("StudentT", D.StudentT(torch.ones(3) * 3)),
                ("Independent", D.Independent(D.Normal(torch.zeros(3, 2), 1.0), 1)), ("Beta", D.Beta(torch.ones(3), 2.0)),
                ("Mixture", D.MixtureSameFamily(D.Categorical(torch.ones(3, 2)), D.Normal(torch.zeros(3, 2), 1.0)))):
    e = d.expand(torch.Size([4, 3]))
    print("expand", name, type(e).__name__, list(e.batch_shape), list(e.event_shape), list(e.sample().shape),
          list(e.log_prob(e.sample()).shape), e._validate_args)
e = D.Normal(torch.zeros(3), 1.0, validate_args=False).expand([2, 3])
print("expand keeps validate", e._validate_args)

# enumerate_support
show("bern support", D.Bernoulli(torch.tensor([0.3, 0.6])).enumerate_support())
show("bern support noexpand", D.Bernoulli(torch.tensor([0.3, 0.6])).enumerate_support(expand=False))
show("cat support", D.Categorical(torch.ones(2, 3)).enumerate_support(expand=False))
show("binom support", D.Binomial(3, torch.tensor([0.3, 0.6])).enumerate_support())

# stddev, perplexity, sample_n, param_shape
show("stddev", D.Gamma(torch.tensor([4.0]), torch.tensor([2.0])).stddev)
show("param shape", D.Categorical(logits=torch.zeros(2, 5)).param_shape)
print("sample_n", list(D.Normal(torch.zeros(2), 1.0).sample(torch.Size([5])).shape))
print("rsample grad", D.Normal(torch.zeros(2, requires_grad=True), 1.0).rsample().requires_grad,
      D.Normal(torch.zeros(2, requires_grad=True), 1.0).sample().requires_grad)
error("rsample", lambda: D.Bernoulli(torch.tensor(0.5)).rsample())
print("lazy probs", "probs" in D.Bernoulli(logits=torch.zeros(2)).__dict__)
b = D.Bernoulli(logits=torch.zeros(2))
b.probs
print("lazy probs after", "probs" in b.__dict__)

# KL registry
class MyNormal(D.Normal):
    pass


print("kl subclass", kl_divergence(MyNormal(0.0, 1.0), D.Normal(1.0, 2.0)).item())


@register_kl(MyNormal, MyNormal)
def _kl_my(p, q):
    return torch.tensor(42.0)


print("kl registered", kl_divergence(MyNormal(0.0, 1.0), MyNormal(1.0, 2.0)).item(),
      kl_divergence(MyNormal(0.0, 1.0), D.Normal(1.0, 2.0)).item())
error("kl missing", lambda: kl_divergence(D.Normal(0.0, 1.0), D.Categorical(torch.ones(2))))
error("kl type", lambda: register_kl(int, D.Normal))
error("kl binomial", lambda: kl_divergence(D.Binomial(2, torch.tensor(0.5)), D.Binomial(3, torch.tensor(0.5))))
show("kl uniform inf", kl_divergence(D.Uniform(torch.tensor([0.0, 0.0]), torch.tensor([1.0, 3.0])), D.Uniform(torch.tensor([0.0, 0.0]), torch.tensor([2.0, 2.0]))))
show("kl bernoulli edge", kl_divergence(D.Bernoulli(torch.tensor([0.0, 1.0, 0.5])), D.Bernoulli(torch.tensor([0.5, 0.5, 0.0]))))

# transforms
t_ = transforms.AffineTransform(1.0, 2.0)
print("transform", t_.sign, t_.bijective, t_.event_dim, t_.inv.inv is t_, t_ == transforms.AffineTransform(1.0, 2.0),
      transforms.ExpTransform() == transforms.ExpTransform(), transforms.ExpTransform().inv == transforms.ExpTransform().inv)
print("domains", transforms.ExpTransform().domain, transforms.ExpTransform().codomain, transforms.SigmoidTransform().codomain,
      transforms.StickBreakingTransform().codomain, transforms.ComposeTransform([transforms.ExpTransform()]).codomain)
c = transforms.ExpTransform().with_cache(1)
xx = torch.tensor([0.5])
yy = c(xx)
print("cache", c(xx) is yy, c.inv(yy) is xx, transforms.identity_transform(xx) is xx)
print("shapes", list(transforms.StickBreakingTransform().forward_shape(torch.Size([2, 3]))),
      list(transforms.ReshapeTransform((2, 3), (6,)).forward_shape(torch.Size([4, 2, 3]))))
error("cache size", lambda: transforms.ExpTransform(cache_size=2))
