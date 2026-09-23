# torch.distributions parity cases: every distribution's log_prob, entropy,
# cdf/icdf, mean/mode/variance/stddev and the gradients of log_prob, entropy
# and cdf with respect to the parameters, the transforms, and every registered
# KL pair with its gradients. `results()` returns {name: [numbers]} (nan and
# the infinities as strings). `gen.py` runs this file under PyTorch 2.11 and
# writes dist_expected.json; Zipp compares against it. Names starting with
# "f32." are float32 cases (compared loosely).
import math
import torch
import torch.distributions as D
from torch.distributions import constraints, transforms

F64 = torch.float64


def t(values, dtype=F64):
    return torch.tensor(values, dtype=dtype)


def num(v):
    v = float(v)
    if v != v:
        return "nan"
    if v == math.inf:
        return "inf"
    if v == -math.inf:
        return "-inf"
    return v


def flat(x):
    if isinstance(x, (int, float)):
        return [num(x)]
    return [num(v) for v in x.detach().reshape(-1).tolist()]


def grads(out, params):
    if not params or not out.requires_grad:
        return [0.0] * sum(p.numel() for p in params)
    gs = torch.autograd.grad(out.sum(), params, allow_unused=True, retain_graph=True)
    res = []
    for g, p in zip(gs, params):
        res.extend(flat(g) if g is not None else [0.0] * p.numel())
    return res


def leaf(values, dtype=F64):
    return t(values, dtype).requires_grad_()


def record(out, name, dist, params, value=None, cdf=None, icdf=None, entropy=True,
           stats=("mean", "variance", "stddev", "mode")):
    if value is not None:
        lp = dist.log_prob(value)
        out[name + ".log_prob"] = flat(lp)
        out[name + ".log_prob.grad"] = grads(lp, params)
    if entropy:
        h = dist.entropy()
        out[name + ".entropy"] = flat(h)
        out[name + ".entropy.grad"] = grads(h, params)
    if cdf is not None:
        c = dist.cdf(cdf)
        out[name + ".cdf"] = flat(c)
        if c.requires_grad:
            out[name + ".cdf.grad"] = grads(c, params)
    if icdf is not None:
        q = dist.icdf(icdf)
        out[name + ".icdf"] = flat(q)
        out[name + ".icdf.grad"] = grads(q, params)
    for s in stats:
        out[name + "." + s] = flat(getattr(dist, s))
    out[name + ".shape"] = list(dist.batch_shape) + [-1] + list(dist.event_shape)


def continuous(out):
    loc, scale = leaf([0.3, -1.2, 2.0]), leaf([0.5, 1.5, 2.2])
    record(out, "normal", D.Normal(loc, scale), [loc, scale], t([0.1, -0.4, 3.0]),
           cdf=t([0.1, -0.4, 3.0]), icdf=t([0.1, 0.5, 0.9]))
    record(out, "normal_scalar", D.Normal(0.5, 2.0), [], torch.tensor([0.25, 3.0]), entropy=False)

    loc, scale = leaf([0.3, -0.5]), leaf([0.4, 1.1])
    record(out, "lognormal", D.LogNormal(loc, scale), [loc, scale], t([1.2, 0.3]),
           cdf=t([1.2, 0.3]), icdf=t([0.2, 0.7]))

    low, high = leaf([0.0, -1.0]), leaf([2.0, 3.0])
    record(out, "uniform", D.Uniform(low, high), [low, high], t([0.5, 2.5]),
           cdf=t([0.5, 2.9]), icdf=t([0.25, 0.8]))

    rate = leaf([0.5, 2.0])
    record(out, "exponential", D.Exponential(rate), [rate], t([0.7, 1.9]),
           cdf=t([0.7, 1.9]), icdf=t([0.3, 0.9]))

    loc, scale = leaf([0.2, -1.0]), leaf([0.8, 2.5])
    record(out, "laplace", D.Laplace(loc, scale), [loc, scale], t([1.0, -3.0]),
           cdf=t([1.0, -3.0]), icdf=t([0.3, 0.8]))

    loc, scale = leaf([0.2, -1.0]), leaf([0.8, 2.5])
    record(out, "cauchy", D.Cauchy(loc, scale), [loc, scale], t([1.0, -3.0]),
           cdf=t([1.0, -3.0]), icdf=t([0.3, 0.8]), stats=("mean", "variance", "mode"))

    conc, rate = leaf([0.7, 2.5, 6.0]), leaf([1.0, 0.5, 3.0])
    record(out, "gamma", D.Gamma(conc, rate), [conc, rate], t([0.4, 3.0, 2.2]))
    # PyTorch's gammainc has no gradient in the concentration.
    conc_c = t([0.7, 2.5, 6.0])
    out["gamma.cdf"] = flat(D.Gamma(conc_c, rate).cdf(t([0.4, 3.0, 2.2])))
    out["gamma.cdf.grad"] = grads(D.Gamma(conc_c, rate).cdf(t([0.4, 3.0, 2.2])), [rate])

    df = leaf([1.5, 4.0])
    record(out, "chi2", D.Chi2(df), [df], t([0.8, 5.0]))

    c1, c0 = leaf([0.5, 2.0, 3.0]), leaf([0.5, 1.5, 4.0])
    record(out, "beta", D.Beta(c1, c0), [c1, c0], t([0.3, 0.6, 0.45]))
    record(out, "f32.beta_scalar", D.Beta(2.0, 3.0), [], torch.tensor(0.25), entropy=True)

    conc = leaf([[0.5, 1.0, 2.0], [3.0, 2.0, 1.5], [0.4, 0.3, 0.8]])
    record(out, "dirichlet", D.Dirichlet(conc), [conc], t([[0.2, 0.3, 0.5], [0.6, 0.3, 0.1], [0.1, 0.1, 0.8]]))

    df, loc, scale = leaf([0.8, 1.5, 3.0, 6.0]), leaf([0.0, 1.0, -1.0, 0.5]), leaf([1.0, 0.5, 2.0, 1.5])
    record(out, "studentt", D.StudentT(df, loc, scale), [df, loc, scale], t([0.3, 1.2, -4.0, 2.0]))

    scale = leaf([0.7, 2.0])
    record(out, "halfnormal", D.HalfNormal(scale), [scale], t([0.5, 3.0]),
           cdf=t([0.5, 3.0]), icdf=t([0.3, 0.9]))
    out["halfnormal.neg"] = flat(D.HalfNormal(t([1.0]), validate_args=False).log_prob(t([-0.5])))
    scale = leaf([0.7, 2.0])
    record(out, "halfcauchy", D.HalfCauchy(scale), [scale], t([0.5, 3.0]),
           cdf=t([0.5, 3.0]), icdf=t([0.3, 0.9]), stats=("mean", "variance", "mode"))


def discrete(out):
    probs = leaf([0.2, 0.7, 0.5])
    record(out, "bernoulli", D.Bernoulli(probs), [probs], t([0.0, 1.0, 1.0]))
    logits = leaf([-1.0, 0.3, 2.0])
    record(out, "bernoulli_logits", D.Bernoulli(logits=logits), [logits], t([1.0, 0.0, 1.0]))
    out["bernoulli.support"] = flat(D.Bernoulli(t([0.3, 0.6])).enumerate_support())

    probs = leaf([[0.1, 0.2, 0.7], [0.3, 0.3, 0.4]])
    record(out, "categorical", D.Categorical(probs), [probs], t([2.0, 0.0]),
           stats=("mode",))
    logits = leaf([[0.5, -1.0, 2.0, 0.1], [1.0, 1.0, 0.0, -2.0]])
    record(out, "categorical_logits", D.Categorical(logits=logits), [logits], torch.tensor([3, 1]),
           stats=("mode",))
    out["categorical.mean"] = flat(D.Categorical(t([0.2, 0.8])).mean)
    out["categorical.support"] = flat(D.Categorical(t([[0.2, 0.8], [0.5, 0.5]])).enumerate_support())
    out["categorical.lp_broadcast"] = flat(D.Categorical(logits=t([[0.5, -1.0, 2.0], [1.0, 0.0, 0.2]])).log_prob(torch.tensor([[0], [2]])))

    probs = leaf([[0.1, 0.2, 0.7], [0.3, 0.3, 0.4]])
    record(out, "onehot", D.OneHotCategorical(probs), [probs], t([[0.0, 0.0, 1.0], [1.0, 0.0, 0.0]]))
    out["onehot.support"] = flat(D.OneHotCategorical(t([0.2, 0.3, 0.5])).enumerate_support())

    probs = leaf([0.3, 0.6])
    record(out, "binomial", D.Binomial(t([5.0, 5.0]), probs), [probs], t([2.0, 4.0]))
    logits = leaf([0.3, -0.8])
    record(out, "binomial_logits", D.Binomial(t([5.0, 10.0]), logits=logits), [logits], t([2.0, 7.0]),
           entropy=False)

    probs = leaf([0.2, 0.3, 0.5])
    record(out, "multinomial", D.Multinomial(6, probs), [probs], t([1.0, 2.0, 3.0]),
           stats=("mean", "variance"))
    logits = leaf([[0.2, -0.3, 1.5], [0.0, 0.1, 0.2]])
    record(out, "multinomial_logits", D.Multinomial(4, logits=logits), [logits], t([[0.0, 1.0, 3.0], [2.0, 2.0, 0.0]]),
           stats=("mean", "variance"))

    rate = leaf([0.5, 3.0, 7.5])
    record(out, "poisson", D.Poisson(rate), [rate], t([0.0, 2.0, 9.0]), entropy=False)

    probs = leaf([0.2, 0.6])
    record(out, "geometric", D.Geometric(probs), [probs], t([0.0, 3.0]))
    logits = leaf([-0.5, 1.0])
    record(out, "geometric_logits", D.Geometric(logits=logits), [logits], t([2.0, 1.0]))


def multivariate(out):
    loc = leaf([[0.5, -1.0, 0.2], [1.0, 0.0, -0.5]])
    a = t([[2.0, 0.3, 0.1], [0.3, 1.5, -0.2], [0.1, -0.2, 1.0]])
    cov = a.clone().requires_grad_()
    x = t([[0.1, 0.2, 0.3], [1.5, -0.5, 0.0]])
    record(out, "mvn_cov", D.MultivariateNormal(loc, covariance_matrix=cov), [loc, cov], x,
           stats=("mean", "variance", "stddev", "mode"))
    d = D.MultivariateNormal(loc.detach(), covariance_matrix=a)
    out["mvn_cov.scale_tril"] = flat(d.scale_tril)
    out["mvn_cov.precision"] = flat(d.precision_matrix)
    prec = a.clone().requires_grad_()
    record(out, "mvn_prec", D.MultivariateNormal(loc, precision_matrix=prec), [loc, prec], x,
           stats=("variance",))
    out["mvn_prec.cov"] = flat(D.MultivariateNormal(loc.detach(), precision_matrix=a).covariance_matrix)
    tril = leaf([[1.2, 0.0, 0.0], [0.4, 0.9, 0.0], [-0.3, 0.2, 0.7]])
    record(out, "mvn_tril", D.MultivariateNormal(loc, scale_tril=tril), [loc, tril], x,
           stats=("variance",))
    # A batch of covariance matrices against one location.
    covs = torch.stack([a, a * 0.5 + torch.eye(3, dtype=F64) * 0.2])
    out["mvn_batch.log_prob"] = flat(D.MultivariateNormal(t([0.0, 0.1, 0.2]), covs).log_prob(x))

    loc = leaf([0.5, -1.0, 0.2])
    factor = leaf([[0.5, 0.1], [-0.3, 0.8], [0.2, 0.2]])
    diag = leaf([0.7, 1.2, 0.4])
    record(out, "lowrank", D.LowRankMultivariateNormal(loc, factor, diag), [loc, factor, diag], x,
           stats=("mean", "variance"))
    out["lowrank.cov"] = flat(D.LowRankMultivariateNormal(loc, factor, diag).covariance_matrix)

    loc, scale = leaf([[0.1, 0.2, 0.3], [-0.5, 0.0, 1.0]]), leaf([[1.0, 0.5, 2.0], [0.3, 0.8, 1.1]])
    record(out, "independent", D.Independent(D.Normal(loc, scale), 1), [loc, scale], x)

    mix = leaf([0.2, 0.5, 0.3])
    loc, scale = leaf([-1.0, 0.5, 2.0]), leaf([0.5, 1.0, 0.8])
    gmm = D.MixtureSameFamily(D.Categorical(mix), D.Normal(loc, scale))
    record(out, "mixture", gmm, [mix, loc, scale], t([0.0, 1.5, -2.0]), cdf=t([0.0, 1.5]), entropy=False,
           stats=("mean", "variance"))
    mloc = leaf([[0.0, 0.0], [1.0, -1.0]])
    comp = D.Independent(D.Normal(mloc, t([[1.0, 0.5], [0.4, 0.7]])), 1)
    gmm2 = D.MixtureSameFamily(D.Categorical(t([0.3, 0.7])), comp)
    record(out, "mixture_mv", gmm2, [mloc], t([[0.5, -0.2], [1.0, 1.0]]), entropy=False, stats=("mean", "variance"))


def transformed(out):
    loc, scale = leaf([0.1, -0.3]), leaf([0.5, 1.2])
    td = D.TransformedDistribution(D.Normal(loc, scale), [transforms.ExpTransform(), transforms.AffineTransform(1.0, 2.0)])
    record(out, "transformed", td, [loc, scale], t([2.5, 4.0]), cdf=t([2.5, 4.0]), icdf=t([0.2, 0.6]),
           entropy=False, stats=())
    td2 = D.TransformedDistribution(D.Normal(loc, scale), [transforms.TanhTransform()])
    record(out, "transformed_tanh", td2, [loc, scale], t([0.3, -0.7]), entropy=False, stats=())
    td3 = D.TransformedDistribution(D.Normal(loc, scale), [transforms.SigmoidTransform(), transforms.AffineTransform(0.0, -1.0)])
    record(out, "transformed_neg", td3, [loc, scale], t([-0.3, -0.7]), cdf=t([-0.3, -0.7]), entropy=False, stats=())

    x = t([[-1.0, 0.5, 2.0], [0.3, 0.0, -0.7]])
    for name, tr in (("exp", transforms.ExpTransform()), ("affine", transforms.AffineTransform(t([1.0, -2.0, 0.5]), t([2.0, 0.5, -3.0]))),
                     ("sigmoid", transforms.SigmoidTransform()), ("tanh", transforms.TanhTransform()),
                     ("softplus", transforms.SoftplusTransform()), ("power", transforms.PowerTransform(t(2.5))),
                     ("compose", transforms.ComposeTransform([transforms.ExpTransform(), transforms.AffineTransform(0.5, 3.0)])),
                     ("affine_event", transforms.AffineTransform(0.0, t([2.0, 0.5, 3.0]), event_dim=1)),
                     ("stickbreaking", transforms.StickBreakingTransform())):
        xin = x.exp() if name == "power" else x
        y = tr(xin)
        out["tr." + name + ".y"] = flat(y)
        out["tr." + name + ".inv"] = flat(tr.inv(y))
        out["tr." + name + ".ladj"] = flat(tr.log_abs_det_jacobian(xin, y))
        out["tr." + name + ".inv_ladj"] = flat(tr.inv.log_abs_det_jacobian(y, xin))
    sm = transforms.SoftmaxTransform()
    out["tr.softmax.y"] = flat(sm(x))
    out["tr.softmax.inv"] = flat(sm.inv(sm(x)))

    temp = t(0.5)
    probs = leaf([0.3, 0.8])
    record(out, "relaxed_bernoulli", D.RelaxedBernoulli(temp, probs), [probs], t([0.2, 0.9]), entropy=False, stats=())
    logits = leaf([0.3, -0.8])
    record(out, "logit_relaxed_bernoulli", D.RelaxedBernoulli(temp, logits=logits).base_dist, [logits], t([0.4, -1.5]), entropy=False, stats=())
    probs = leaf([[0.2, 0.3, 0.5], [0.6, 0.3, 0.1]])
    record(out, "relaxed_onehot", D.RelaxedOneHotCategorical(t(0.7), probs), [probs], t([[0.1, 0.3, 0.6], [0.5, 0.25, 0.25]]),
           entropy=False, stats=())
    record(out, "exp_relaxed", D.RelaxedOneHotCategorical(t(0.7), probs).base_dist, [probs], t([[0.1, 0.3, 0.6], [0.5, 0.25, 0.25]]).log(),
           entropy=False, stats=())


def kls(out):
    def kl(name, p, q, params):
        k = D.kl_divergence(p, q)
        out["kl." + name] = flat(k)
        if params:
            out["kl." + name + ".grad"] = grads(k, params)

    a, b, c, d = leaf([0.1, -0.5]), leaf([0.8, 1.2]), leaf([0.4, 0.0]), leaf([1.1, 0.6])
    kl("normal", D.Normal(a, b), D.Normal(c, d), [a, b, c, d])
    p, q = leaf([[0.2, 0.3, 0.5], [0.1, 0.1, 0.8]]), leaf([[0.4, 0.4, 0.2], [0.3, 0.3, 0.4]])
    kl("categorical", D.Categorical(p), D.Categorical(q), [p, q])
    kl("onehot", D.OneHotCategorical(p), D.OneHotCategorical(q), [p, q])
    kl("categorical_zero", D.Categorical(t([0.0, 0.5, 0.5])), D.Categorical(t([0.2, 0.0, 0.8])), [])
    p, q = leaf([0.2, 0.7]), leaf([0.5, 0.4])
    kl("bernoulli", D.Bernoulli(p), D.Bernoulli(q), [p, q])
    loc1 = leaf([0.5, -1.0, 0.2])
    loc2 = leaf([0.0, 0.3, -0.1])
    cov1 = t([[2.0, 0.3, 0.1], [0.3, 1.5, -0.2], [0.1, -0.2, 1.0]]).requires_grad_()
    tril2 = leaf([[1.2, 0.0, 0.0], [0.4, 0.9, 0.0], [-0.3, 0.2, 0.7]])
    kl("mvn", D.MultivariateNormal(loc1, cov1), D.MultivariateNormal(loc2, scale_tril=tril2), [loc1, loc2, cov1, tril2])
    lo1, hi1, lo2, hi2 = leaf([0.0, 1.0]), leaf([1.0, 2.0]), leaf([-1.0, 1.5]), leaf([2.0, 3.0])
    kl("uniform", D.Uniform(lo1, hi1), D.Uniform(lo2, hi2), [lo1, hi1, lo2, hi2])
    r1, r2 = leaf([0.5, 2.0]), leaf([1.5, 0.7])
    kl("exponential", D.Exponential(r1), D.Exponential(r2), [r1, r2])
    c1, rr1, c2, rr2 = leaf([0.7, 3.0]), leaf([1.0, 0.5]), leaf([2.0, 1.2]), leaf([0.3, 2.0])
    kl("gamma", D.Gamma(c1, rr1), D.Gamma(c2, rr2), [c1, rr1, c2, rr2])
    kl("exponential_gamma", D.Exponential(r1), D.Gamma(c2, rr2), [r1, c2, rr2])
    b1, b0, e1, e0 = leaf([0.5, 2.0]), leaf([1.5, 3.0]), leaf([2.0, 0.7]), leaf([1.0, 4.0])
    kl("beta", D.Beta(b1, b0), D.Beta(e1, e0), [b1, b0, e1, e0])
    dp, dq = leaf([[0.5, 1.0, 2.0], [3.0, 2.0, 1.5]]), leaf([[1.0, 1.0, 1.0], [0.4, 0.3, 0.8]])
    kl("dirichlet", D.Dirichlet(dp), D.Dirichlet(dq), [dp, dq])
    kl("laplace", D.Laplace(a, b), D.Laplace(c, d), [a, b, c, d])
    kl("normal_laplace", D.Normal(a, b), D.Laplace(c, d), [a, b, c, d])
    kl("uniform_normal", D.Uniform(lo1, hi1), D.Normal(c, d), [lo1, hi1, c, d])
    kl("cauchy", D.Cauchy(a, b), D.Cauchy(c, d), [a, b, c, d])
    kl("halfnormal", D.HalfNormal(b), D.HalfNormal(d), [b, d])
    kl("lognormal", D.LogNormal(a, b), D.LogNormal(c, d), [a, b, c, d])
    la, lb = leaf([[0.1, 0.2], [0.3, -0.4]]), leaf([[1.0, 0.5], [2.0, 0.7]])
    kl("independent", D.Independent(D.Normal(la, lb), 1), D.Independent(D.Normal(lb, la.exp()), 1), [la, lb])
    kl("poisson", D.Poisson(r1), D.Poisson(r2), [r1, r2])
    kl("geometric", D.Geometric(t([0.2, 0.6])), D.Geometric(t([0.5, 0.3])), [])
    kl("binomial", D.Binomial(t([5.0, 5.0]), t([0.3, 0.6])), D.Binomial(t([5.0, 3.0]), t([0.5, 0.2])), [])
    f1, dg1 = leaf([[0.5, 0.1], [-0.3, 0.8], [0.2, 0.2]]), leaf([0.7, 1.2, 0.4])
    f2, dg2 = leaf([[0.2], [0.4], [-0.6]]), leaf([1.0, 0.5, 0.9])
    kl("lowrank", D.LowRankMultivariateNormal(loc1, f1, dg1), D.LowRankMultivariateNormal(loc2, f2, dg2), [loc1, f1, dg1, f2, dg2])


def extras(out):
    # float32 parameters
    loc, scale = leaf([0.3, -1.2], torch.float32), leaf([0.5, 1.5], torch.float32)
    record(out, "f32.normal", D.Normal(loc, scale), [loc, scale], t([0.1, -0.4], torch.float32))
    conc, rate = leaf([0.7, 4.0], torch.float32), leaf([1.0, 2.5], torch.float32)
    record(out, "f32.gamma", D.Gamma(conc, rate), [conc, rate], t([0.4, 1.3], torch.float32))
    probs = leaf([0.1, 0.2, 0.7], torch.float32)
    record(out, "f32.categorical", D.Categorical(probs), [probs], torch.tensor(2), stats=())
    # expand keeps the parameters' values
    base = D.Normal(t([0.1, 0.2]), t([1.0, 2.0]))
    ex = base.expand([3, 2])
    out["expand.normal"] = flat(ex.log_prob(t([[0.0, 1.0], [1.0, 2.0], [-1.0, 0.0]])))
    out["expand.lognormal"] = flat(D.LogNormal(t([0.1, 0.2]), t([1.0, 2.0])).expand([2, 2]).log_prob(t([1.5, 0.5])))
    out["expand.dirichlet"] = flat(D.Dirichlet(t([1.0, 2.0, 3.0])).expand([2]).log_prob(t([[0.2, 0.3, 0.5], [0.1, 0.1, 0.8]])))
    out["expand.mvn"] = flat(D.MultivariateNormal(t([0.0, 1.0]), t([[1.0, 0.2], [0.2, 2.0]])).expand([3]).log_prob(t([0.5, 0.5])))
    out["perplexity"] = flat(D.Categorical(t([0.25, 0.25, 0.5])).perplexity())
    # Exponential-family entropy through the Bregman divergence of the log normalizer
    out["expfamily.normal"] = flat(D.ExponentialFamily.entropy(D.Normal(t([0.3]), t([1.7]))))
    out["expfamily.gamma"] = flat(D.ExponentialFamily.entropy(D.Gamma(t([2.5]), t([0.7]))))
    out["expfamily.exponential"] = flat(D.ExponentialFamily.entropy(D.Exponential(t([0.7]))))
    # the implicit reparameterisation gradient of standard gamma samples (PyTorch's
    # approximation, reproduced exactly)
    a = t([0.3, 0.8, 1.0, 2.5, 7.0, 30.0, 12.0, 3.0])
    x = t([0.05, 0.6, 1.3, 2.0, 9.0, 25.0, 12.5, 0.2])
    if hasattr(torch, "_standard_gamma_grad"):
        g = torch._standard_gamma_grad(a, x)
    else:
        g = D._standard_gamma_grad(a, x)
    out["standard_gamma_grad"] = flat(g)


def results():
    out = {}
    continuous(out)
    discrete(out)
    multivariate(out)
    transformed(out)
    kls(out)
    extras(out)
    return out
