# torch.distributions behaviour that is not a number, second part: the
# distributions and transforms dist_api.py does not cover, the package's
# __all__, the constraints/transforms/kl submodules and the Bregman KL. Run
# under PyTorch 2.11 (gen.py writes api_expected2.txt); Zipp must print the
# same lines.
import torch
import torch.distributions as D
import torch.distributions.constraints as C
import torch.distributions.transforms as T
import torch.distributions.kl as K
from torch.distributions.transforms import AffineTransform, CorrCholeskyTransform
from torch.distributions.kl import register_kl, kl_divergence
from torch.distributions.constraints import positive, corr_cholesky


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
print("all", sorted(D.__all__))
print("missing", [n for n in D.__all__ if not hasattr(D, n)])

# the submodules re-export the package's objects
print("submodules", C.positive is D.constraints.positive, positive is D.constraints.positive,
      corr_cholesky is D.constraints.corr_cholesky, C.Constraint is D.constraints.Constraint,
      AffineTransform is D.AffineTransform, T.ExpTransform is D.ExpTransform, CorrCholeskyTransform is D.CorrCholeskyTransform,
      T.identity_transform is D.identity_transform, register_kl is D.register_kl, kl_divergence is D.kl_divergence,
      K.kl_divergence is D.kl_divergence, C is D.constraints, T is D.transforms, K is D.kl)
print("submodule names", C.__name__, T.__name__, K.__name__)
print("constraint names", [n for n in ("Constraint", "boolean", "cat", "corr_cholesky", "dependent", "dependent_property",
                                        "greater_than", "greater_than_eq", "independent", "integer_interval", "interval",
                                        "half_open_interval", "is_dependent", "less_than", "lower_cholesky", "lower_triangular",
                                        "MixtureSameFamilyConstraint", "multinomial", "nonnegative", "nonnegative_integer",
                                        "one_hot", "positive", "positive_semidefinite", "positive_definite", "positive_integer",
                                        "real", "real_vector", "simplex", "square", "stack", "symmetric", "unit_interval")
                                  if not hasattr(C, n)])
print("transform names", [n for n in ("AbsTransform", "AffineTransform", "CatTransform", "ComposeTransform", "CorrCholeskyTransform",
                                       "CumulativeDistributionTransform", "ExpTransform", "IndependentTransform",
                                       "LowerCholeskyTransform", "PositiveDefiniteTransform", "PowerTransform", "ReshapeTransform",
                                       "SigmoidTransform", "SoftplusTransform", "TanhTransform", "SoftmaxTransform", "StackTransform",
                                       "StickBreakingTransform", "Transform", "identity_transform")
                                 if not hasattr(T, n)])

cov = torch.tensor([[2.0, 0.3], [0.3, 1.0]])
dists = [
    ("Gumbel", D.Gumbel(torch.tensor([0.0, 1.0]), torch.tensor([1.0, 2.0]))),
    ("Gumbel0", D.Gumbel(0.0, 1.0)),
    ("Pareto", D.Pareto(torch.tensor([1.0, 2.0]), torch.tensor([3.0, 4.0]))),
    ("Weibull", D.Weibull(torch.tensor([1.0, 2.0]), torch.tensor([1.5, 0.5]))),
    ("Kumaraswamy", D.Kumaraswamy(torch.tensor([1.0, 2.0]), torch.tensor([3.0, 0.5]))),
    ("ContinuousBernoulli", D.ContinuousBernoulli(torch.tensor([0.3, 0.9]))),
    ("ContinuousBernoulli0", D.ContinuousBernoulli(0.3)),
    ("FisherSnedecor", D.FisherSnedecor(torch.tensor([3.0, 5.0]), torch.tensor([4.0, 6.0]))),
    ("GeneralizedPareto", D.GeneralizedPareto(torch.tensor([0.0, 1.0]), torch.tensor([1.0, 2.0]), torch.tensor([0.2, -0.3]))),
    ("InverseGamma", D.InverseGamma(torch.tensor([2.0, 3.0]), torch.tensor([1.0, 0.5]))),
    ("LogisticNormal", D.LogisticNormal(torch.zeros(2, 3), torch.ones(2, 3))),
    ("VonMises", D.VonMises(torch.tensor([0.0, 1.0]), torch.tensor([1.0, 4.0]))),
    ("NegativeBinomial", D.NegativeBinomial(torch.tensor([3.0, 5.0]), torch.tensor([0.3, 0.6]))),
    ("Wishart", D.Wishart(torch.tensor([4.0, 5.0]), cov)),
    ("LKJCholesky", D.LKJCholesky(3, torch.tensor([0.5, 2.0]))),
]
for name, d in dists:
    s = d.sample((2,))
    print(name, list(d.batch_shape), list(d.event_shape), d.has_rsample, d.has_enumerate_support,
          list(s.shape), s.dtype, repr(d.support))
    print("  ", d)
    print("  ", {k: repr(v) for k, v in d.arg_constraints.items()})
    print("   support ok", bool(d.support.check(s).all()), list(d.log_prob(s).shape))
    if d.has_rsample:
        print("   rsample", list(d.rsample((3,)).shape))

show("negbin zero sample", D.NegativeBinomial(torch.tensor([0.0, 0.0]), torch.tensor([0.5, 0.9])).sample((3,)))
show("gamma zero sample", D.Gamma(torch.tensor([0.0, 0.0]), 1.0, validate_args=False).sample((2,)) == torch.finfo(torch.float32).tiny)
print("dtypes", D.VonMises(torch.zeros(2, dtype=F64), 1.0).sample().dtype, D.NegativeBinomial(3.0, torch.tensor([0.5])).sample().dtype,
      D.LKJCholesky(2, torch.tensor(1.0, dtype=F64)).sample().dtype, D.Wishart(4.0, torch.eye(2, dtype=F64)).sample().dtype)

# argument validation
error("gumbel scale", lambda: D.Gumbel(0.0, -1.0))
error("pareto alpha", lambda: D.Pareto(torch.tensor([1.0]), torch.tensor([0.0])))
error("weibull conc", lambda: D.Weibull(torch.tensor([1.0]), torch.tensor([-1.0])))
error("kumaraswamy", lambda: D.Kumaraswamy(torch.tensor([1.0]), torch.tensor([-1.0])))
error("cb probs", lambda: D.ContinuousBernoulli(torch.tensor([1.5]), validate_args=True))
error("cb probs default", lambda: D.ContinuousBernoulli(torch.tensor([1.5])))
error("cb both", lambda: D.ContinuousBernoulli(torch.tensor([0.5]), torch.tensor([0.0])))
error("fisher", lambda: D.FisherSnedecor(torch.tensor([1.0]), torch.tensor([0.0])))
error("genpareto scale", lambda: D.GeneralizedPareto(0.0, -1.0, 0.1))
error("invgamma", lambda: D.InverseGamma(torch.tensor([-1.0]), torch.tensor([1.0])))
error("logisticnormal", lambda: D.LogisticNormal(torch.zeros(2), torch.tensor([1.0, -1.0])))
error("vonmises", lambda: D.VonMises(torch.tensor([0.0]), torch.tensor([0.0])))
error("negbin probs", lambda: D.NegativeBinomial(3.0, torch.tensor([1.0])))
error("negbin count", lambda: D.NegativeBinomial(torch.tensor([-1.0]), torch.tensor([0.5])))
error("negbin both", lambda: D.NegativeBinomial(3.0))
error("wishart none", lambda: D.Wishart(4.0))
error("wishart two", lambda: D.Wishart(4.0, cov, scale_tril=torch.eye(2)))
error("wishart dim", lambda: D.Wishart(4.0, torch.tensor([1.0, 2.0])))
error("wishart df", lambda: D.Wishart(torch.tensor([0.5]), cov))
error("wishart pd", lambda: D.Wishart(4.0, torch.tensor([[1.0, 2.0], [2.0, 1.0]])))
error("lkj dim", lambda: D.LKJCholesky(1, 1.0))
error("lkj conc", lambda: D.LKJCholesky(3, torch.tensor([-1.0])))
# sample validation
error("gumbel value", lambda: D.Gumbel(0.0, 1.0).log_prob(torch.tensor(float("nan"))))
error("pareto support", lambda: D.Pareto(torch.tensor([2.0]), torch.tensor([1.0])).log_prob(torch.tensor([1.0])))
error("weibull support", lambda: D.Weibull(torch.tensor([1.0]), torch.tensor([1.0])).log_prob(torch.tensor([-1.0])))
error("kumaraswamy support", lambda: D.Kumaraswamy(torch.tensor([1.0]), torch.tensor([1.0])).log_prob(torch.tensor([1.5])))
error("cb support", lambda: D.ContinuousBernoulli(torch.tensor([0.3])).log_prob(torch.tensor([1.5])))
error("genpareto support", lambda: D.GeneralizedPareto(torch.tensor([0.0]), torch.tensor([1.0]), torch.tensor([-0.5])).log_prob(torch.tensor([3.0])))
error("invgamma support", lambda: D.InverseGamma(torch.tensor([1.0]), torch.tensor([1.0])).log_prob(torch.tensor([0.0])))
error("logisticnormal support", lambda: D.LogisticNormal(torch.zeros(2), 1.0).log_prob(torch.tensor([0.5, 0.6, 0.1])))
error("negbin support", lambda: D.NegativeBinomial(3.0, torch.tensor([0.5])).log_prob(torch.tensor([1.5])))
error("wishart support", lambda: D.Wishart(4.0, cov).log_prob(torch.tensor([[1.0, 2.0], [2.0, 1.0]])))
error("lkj support", lambda: D.LKJCholesky(2, 1.0).log_prob(torch.tensor([[1.0, 0.0], [0.5, 0.5]])))
# what PyTorch leaves unimplemented
error("fisher entropy", lambda: D.FisherSnedecor(torch.tensor([3.0]), torch.tensor([4.0])).entropy())
error("vonmises entropy", lambda: D.VonMises(torch.tensor([0.0]), torch.tensor([1.0])).entropy())
error("vonmises cdf", lambda: D.VonMises(torch.tensor([0.0]), torch.tensor([1.0])).cdf(torch.tensor([0.0])))
error("vonmises rsample", lambda: D.VonMises(torch.tensor([0.0]), torch.tensor([1.0])).rsample())
error("negbin entropy", lambda: D.NegativeBinomial(3.0, torch.tensor([0.5])).entropy())
error("negbin rsample", lambda: D.NegativeBinomial(3.0, torch.tensor([0.5])).rsample())
error("cb mode", lambda: D.ContinuousBernoulli(torch.tensor([0.5])).mode)
error("logisticnormal mean", lambda: D.LogisticNormal(torch.zeros(2), 1.0).mean)
error("lkj mean", lambda: D.LKJCholesky(3, 1.0).mean)
error("lkj rsample", lambda: D.LKJCholesky(3, 1.0).rsample())
error("pareto entropy ok", lambda: D.Pareto(torch.tensor([1.0]), torch.tensor([2.0])).entropy())
print("vonmises variance lazy", "variance" in D.VonMises(torch.tensor([0.0]), torch.tensor([1.0])).__dict__)
v = D.VonMises(torch.tensor([0.0]), torch.tensor([1.0]))
v.variance
print("vonmises variance after", "variance" in v.__dict__)

# expand
for name, d in (("Gumbel", D.Gumbel(torch.zeros(3), 1.0)), ("Pareto", D.Pareto(torch.ones(3), 2.0)),
                ("Weibull", D.Weibull(torch.ones(3), 2.0)), ("Kumaraswamy", D.Kumaraswamy(torch.ones(3), 2.0)),
                ("ContinuousBernoulli", D.ContinuousBernoulli(logits=torch.zeros(3))),
                ("FisherSnedecor", D.FisherSnedecor(torch.ones(3) * 3, 4.0)),
                ("GeneralizedPareto", D.GeneralizedPareto(torch.zeros(3), 1.0, 0.1)),
                ("InverseGamma", D.InverseGamma(torch.ones(3) * 2, 1.0)),
                ("LogisticNormal", D.LogisticNormal(torch.zeros(3, 2), 1.0)),
                ("VonMises", D.VonMises(torch.zeros(3), 1.0)),
                ("NegativeBinomial", D.NegativeBinomial(torch.ones(3) * 2, logits=torch.zeros(3))),
                ("Wishart", D.Wishart(torch.ones(3) * 4, torch.eye(2))),
                ("LKJCholesky", D.LKJCholesky(2, torch.ones(3)))):
    e = d.expand(torch.Size([4, 3]))
    print("expand", name, type(e).__name__, list(e.batch_shape), list(e.event_shape), list(e.sample().shape),
          list(e.log_prob(e.sample()).shape), e._validate_args)
print("expand validate", D.VonMises(torch.zeros(3), 1.0, validate_args=False).expand([2, 3])._validate_args,
      D.Gumbel(torch.zeros(3), 1.0, validate_args=False).expand([2, 3])._validate_args)
w = D.Wishart(torch.tensor(4.0), scale_tril=torch.eye(2))
print("wishart lazy", "scale_tril" in w.__dict__, "covariance_matrix" in w.__dict__)
w.covariance_matrix
print("wishart lazy after", "covariance_matrix" in w.__dict__)

# transforms
cc = T.CorrCholeskyTransform()
print("corrchol", cc.domain, cc.codomain, cc.bijective, cc.event_dim if False else cc.domain.event_dim, cc.codomain.event_dim)
lc = T.LowerCholeskyTransform()
print("lowerchol", lc.domain, lc.codomain, lc.bijective, lc == T.LowerCholeskyTransform(), T.PositiveDefiniteTransform().codomain)
error("lowerchol ladj", lambda: lc.log_abs_det_jacobian(torch.eye(2), torch.eye(2)))
cat = T.CatTransform([T.ExpTransform(), T.SigmoidTransform()], dim=-1, lengths=[2, 1])
print("cat", cat.domain, cat.codomain, cat.bijective, cat.event_dim, cat.length)
st = T.StackTransform([T.ExpTransform(), T.SigmoidTransform()], dim=0)
print("stack", st.domain, st.codomain, st.bijective)
cdt = T.CumulativeDistributionTransform(D.Normal(0.0, 1.0))
print("cdt", cdt.domain, cdt.codomain, cdt.bijective, cdt.sign, cdt.with_cache().distribution is cdt.distribution)
print("cat check", C.cat([C.positive, C.real], dim=-1, lengths=[2, 1]).check(torch.tensor([[1.0, 2.0, -3.0], [1.0, -2.0, 3.0]])).tolist())
print("registry", D.transform_to(C.lower_cholesky), D.transform_to(C.positive_definite), D.transform_to(C.positive_semidefinite),
      D.biject_to(C.corr_cholesky), D.transform_to(C.corr_cholesky))
print("registry cat", D.biject_to(C.cat([C.positive, C.real], 0, [1, 1])).transforms,
      D.transform_to(C.stack([C.unit_interval, C.real], -1)).transforms)
error("biject lower_cholesky", lambda: D.biject_to(C.lower_cholesky))
error("biject positive_definite", lambda: D.biject_to(C.positive_definite))
error("cat transform", lambda: T.CatTransform([T.ExpTransform()], dim=0, lengths=[1, 2]))
error("cat length", lambda: T.CatTransform([T.ExpTransform()], dim=0, lengths=[2])(torch.zeros(3)))

# KL registry
error("kl expfamily cross", lambda: kl_divergence(D.Poisson(torch.tensor([1.0])), D.Gamma(torch.tensor([1.0]), torch.tensor([1.0]))))
error("kl wishart", lambda: kl_divergence(D.Wishart(torch.tensor(4.0), cov), D.Wishart(torch.tensor(5.0), torch.eye(2))))
error("kl vonmises", lambda: kl_divergence(D.VonMises(torch.tensor(0.0), torch.tensor(1.0)), D.VonMises(torch.tensor(0.0), torch.tensor(1.0))))
show("kl pareto inf", kl_divergence(D.Pareto(torch.tensor([1.0, 2.0]), torch.tensor([2.0, 2.0])), D.Pareto(torch.tensor([1.5, 1.5]), torch.tensor([2.0, 2.0]))) == float("inf"))
show("kl normal gamma", kl_divergence(D.Normal(torch.tensor([0.0]), 1.0), D.Gamma(torch.tensor([1.0]), 1.0)))
show("kl uniform gumbel", kl_divergence(D.Uniform(0.0, 1.0), D.Gumbel(0.0, 1.0)).shape)
show("kl registry size", len([k for k in K._KL_REGISTRY]))
