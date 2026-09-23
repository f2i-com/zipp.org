# torch.distributions parity cases, second part: Gumbel, Pareto, Weibull,
# Kumaraswamy, ContinuousBernoulli, FisherSnedecor, GeneralizedPareto,
# InverseGamma, LogisticNormal, VonMises, NegativeBinomial, Wishart and
# LKJCholesky (log_prob, entropy, cdf/icdf, moments and their gradients), the
# CorrCholesky, LowerCholesky, PositiveDefinite, Cat, Stack and
# CumulativeDistribution transforms, and the KL pairs PyTorch registers that
# the first part does not cover. `results2()` returns {name: [numbers]} (nan
# and the infinities as strings); gen.py writes dist_expected2.json from it.
# Self-contained (the helpers repeat dist_cases.py's) so either file runs
# alone.
import math
import torch
import torch.distributions as D
from torch.distributions import transforms

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


def corr_cholesky(corrs):
    return torch.linalg.cholesky(t(corrs))


def continuous(out):
    loc, scale = leaf([0.3, -1.2, 2.0]), leaf([0.5, 1.5, 2.2])
    record(out, "gumbel", D.Gumbel(loc, scale), [loc, scale], t([0.1, -0.4, 3.0]),
           cdf=t([0.1, -0.4, 3.0]), icdf=t([0.1, 0.5, 0.9]))
    record(out, "f32.gumbel_scalar", D.Gumbel(0.5, 2.0), [], torch.tensor([0.25, 3.0]))

    scale, alpha = leaf([0.5, 1.0, 2.0]), leaf([0.8, 2.5, 5.0])
    record(out, "pareto", D.Pareto(scale, alpha), [scale, alpha], t([0.7, 1.9, 2.5]),
           cdf=t([0.7, 1.9, 2.5]), icdf=t([0.1, 0.5, 0.9]), stats=("mean", "variance", "mode"))

    scale, conc = leaf([1.0, 2.0, 0.5]), leaf([0.7, 1.5, 3.0])
    record(out, "weibull", D.Weibull(scale, conc), [scale, conc], t([0.4, 3.0, 0.6]),
           cdf=t([0.4, 3.0, 0.6]), icdf=t([0.2, 0.5, 0.95]))

    c1, c0 = leaf([0.5, 2.0, 3.0, 1.5]), leaf([0.7, 1.5, 0.8, 4.0])
    record(out, "kumaraswamy", D.Kumaraswamy(c1, c0), [c1, c0], t([0.3, 0.6, 0.45, 0.2]),
           cdf=t([0.3, 0.6, 0.45, 0.2]), icdf=t([0.1, 0.5, 0.9, 0.3]))

    probs = leaf([0.2, 0.4995, 0.7, 0.95])
    record(out, "cbernoulli", D.ContinuousBernoulli(probs), [probs], t([0.3, 0.5, 0.9, 0.1]),
           cdf=t([0.3, 0.5, 0.9, 0.1]), icdf=t([0.25, 0.5, 0.75, 0.9]), stats=("mean", "variance", "stddev"))
    logits = leaf([-1.0, 0.001, 2.0])
    record(out, "cbernoulli_logits", D.ContinuousBernoulli(logits=logits), [logits], t([0.8, 0.5, 0.3]),
           cdf=t([0.8, 0.5, 0.3]), stats=("mean", "variance"))
    out["cbernoulli.expfamily"] = flat(D.ExponentialFamily.entropy(D.ContinuousBernoulli(t([0.2, 0.7]))))
    out["cbernoulli.edges"] = flat(D.ContinuousBernoulli(t([0.3, 0.8])).cdf(t([0.0, 1.0])))

    df1, df2 = leaf([1.5, 3.0, 6.0]), leaf([1.5, 5.0, 10.0])
    record(out, "fisher", D.FisherSnedecor(df1, df2), [df1, df2], t([0.4, 1.3, 2.2]), entropy=False,
           stats=("mean", "variance", "mode"))

    # PyTorch's GeneralizedPareto compares the concentration with a default-dtype
    # tensor, so float64 parameters need float64 as the default dtype.
    torch.set_default_dtype(F64)
    loc, scale, conc = leaf([0.1, 0.2, 0.0]), leaf([1.0, 2.0, 0.5]), leaf([0.0, 0.3, -0.2])
    record(out, "genpareto", D.GeneralizedPareto(loc, scale, conc), [loc, scale, conc], t([0.5, 1.0, 1.0]),
           cdf=t([0.5, 1.0, 1.0]), icdf=t([0.1, 0.5, 0.9]))
    loc, scale, conc = leaf([0.0, 1.0]), leaf([1.5, 0.5]), leaf([0.7, -0.6])
    record(out, "genpareto2", D.GeneralizedPareto(loc, scale, conc), [loc, scale, conc], t([2.0, 1.5]),
           cdf=t([2.0, 1.5]))
    torch.set_default_dtype(torch.float32)
    record(out, "f32.genpareto", D.GeneralizedPareto(0.5, 2.0, 0.25), [], torch.tensor([0.75, 4.0]),
           cdf=torch.tensor([0.75, 4.0]))

    conc, rate = leaf([0.7, 2.5, 6.0]), leaf([1.0, 0.5, 3.0])
    record(out, "invgamma", D.InverseGamma(conc, rate), [conc, rate], t([0.4, 3.0, 2.2]))
    out["invgamma.cdf"] = flat(D.InverseGamma(t([0.7, 2.5, 6.0]), t([1.0, 0.5, 3.0])).cdf(t([0.4, 3.0, 2.2])))

    loc, scale = leaf([0.1, -0.3, 0.5]), leaf([0.5, 1.2, 0.8])
    record(out, "logisticnormal", D.LogisticNormal(loc, scale), [loc, scale], t([0.2, 0.3, 0.1, 0.4]),
           entropy=False, stats=())
    loc = leaf([[0.1, -0.3], [0.4, 0.0]])
    record(out, "logisticnormal_batch", D.LogisticNormal(loc, 0.7), [loc], t([[0.2, 0.3, 0.5], [0.6, 0.3, 0.1]]),
           entropy=False, stats=())
    out["logisticnormal_scalar"] = flat(D.LogisticNormal(0.2, 1.1).log_prob(t([0.3, 0.7], torch.float32)))

    loc, conc = leaf([0.3, -1.0, 2.5]), leaf([0.5, 2.0, 5.0])
    record(out, "vonmises", D.VonMises(loc, conc), [loc, conc], t([0.1, 2.0, -3.0]), entropy=False,
           stats=("mean", "variance", "mode"))
    out["vonmises.variance.grad"] = grads(D.VonMises(loc, conc).variance, [conc])


def discrete(out):
    count, probs = leaf([3.0, 5.5, 1.0]), leaf([0.3, 0.6, 0.0])
    record(out, "negbinomial", D.NegativeBinomial(count, probs), [count, probs], t([2.0, 7.0, 0.0]),
           entropy=False, stats=("mean", "variance", "mode"))
    count, logits = leaf([2.0, 4.0]), leaf([0.3, -1.5])
    record(out, "negbinomial_logits", D.NegativeBinomial(count, logits=logits), [count, logits], t([0.0, 3.0]),
           entropy=False, stats=("mean", "variance", "mode"))
    out["negbinomial.zero"] = flat(D.NegativeBinomial(t([0.0, 0.0, 2.0]), t([0.5, 0.5, 0.5])).log_prob(t([0.0, 1.0, 1.0])))
    out["negbinomial.probs"] = flat(D.NegativeBinomial(3.0, logits=t([0.5, -0.5])).probs)


def matrices(out):
    df = leaf([4.0, 6.5])
    a = t([[2.0, 0.3, 0.1], [0.3, 1.5, -0.2], [0.1, -0.2, 1.0]])
    cov = a.clone().requires_grad_()
    x = t([[1.5, 0.2, -0.3], [0.2, 2.0, 0.4], [-0.3, 0.4, 0.9]])
    record(out, "wishart_cov", D.Wishart(df, covariance_matrix=cov), [df, cov], x)
    d = D.Wishart(t(5.0), covariance_matrix=a)
    out["wishart.scale_tril"] = flat(d.scale_tril)
    out["wishart.precision"] = flat(d.precision_matrix)
    prec = a.clone().requires_grad_()
    record(out, "wishart_prec", D.Wishart(df, precision_matrix=prec), [df, prec], x, stats=("mean", "variance"))
    out["wishart_prec.cov"] = flat(D.Wishart(t(5.0), precision_matrix=a).covariance_matrix)
    tril = leaf([[1.2, 0.0, 0.0], [0.4, 0.9, 0.0], [-0.3, 0.2, 0.7]])
    record(out, "wishart_tril", D.Wishart(df, scale_tril=tril), [df, tril], x, stats=("mean",))
    out["wishart.number_df"] = flat(D.Wishart(3.5, covariance_matrix=t([[1.0, 0.2], [0.2, 0.5]])).log_prob(t([[0.8, 0.1], [0.1, 0.6]])))
    out["wishart.expfamily"] = flat(D.ExponentialFamily.entropy(D.Wishart(t([4.0, 5.5]), covariance_matrix=a)))
    out["wishart.expand"] = flat(D.Wishart(t(4.5), covariance_matrix=a).expand([2]).log_prob(x))

    conc = leaf([0.5, 1.0, 2.5])
    L = corr_cholesky([[[1.0, 0.3, -0.2], [0.3, 1.0, 0.5], [-0.2, 0.5, 1.0]],
                       [[1.0, -0.6, 0.1], [-0.6, 1.0, 0.0], [0.1, 0.0, 1.0]],
                       [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]])
    record(out, "lkj", D.LKJCholesky(3, conc), [conc], L, entropy=False, stats=())
    L2 = corr_cholesky([[1.0, 0.4], [0.4, 1.0]])
    conc2 = leaf(1.7)
    record(out, "lkj2", D.LKJCholesky(2, conc2), [conc2], L2, entropy=False, stats=())
    out["lkj4"] = flat(D.LKJCholesky(4, 3.0).log_prob(corr_cholesky(
        [[1.0, 0.2, 0.1, 0.0], [0.2, 1.0, 0.3, -0.1], [0.1, 0.3, 1.0, 0.2], [0.0, -0.1, 0.2, 1.0]])))


def transformed(out):
    x = t([[-1.0, 0.5, 2.0], [0.3, 0.0, -0.7]])
    tr = transforms.CorrCholeskyTransform()
    xr = x.clone().requires_grad_()
    y = tr(xr)
    out["tr.corrchol.y"] = flat(y)
    out["tr.corrchol.inv"] = flat(tr.inv(y.detach()))
    ladj = tr.log_abs_det_jacobian(xr, y)
    out["tr.corrchol.ladj"] = flat(ladj)
    out["tr.corrchol.ladj.grad"] = grads(ladj, [xr])
    out["tr.corrchol.inv_ladj"] = flat(tr.inv.log_abs_det_jacobian(y.detach(), x))
    out["tr.corrchol.shape"] = list(tr.forward_shape(torch.Size([2, 6]))) + list(tr.inverse_shape(torch.Size([5, 4, 4])))

    m = t([[[0.5, 0.3], [-0.2, -0.4]], [[1.0, 2.0], [0.1, 0.0]]])
    lc = transforms.LowerCholeskyTransform()
    out["tr.lowerchol.y"] = flat(lc(m))
    out["tr.lowerchol.inv"] = flat(lc.inv(lc(m)))
    pd = transforms.PositiveDefiniteTransform()
    out["tr.posdef.y"] = flat(pd(m))
    out["tr.posdef.inv"] = flat(pd.inv(pd(m)))

    z = t([[-1.0, 0.5, 2.0, 0.1], [0.3, 0.0, -0.7, 1.2]])
    cat = transforms.CatTransform([transforms.ExpTransform(), transforms.AffineTransform(1.0, 3.0)], dim=1, lengths=[3, 1])
    yc = cat(z)
    out["tr.cat.y"] = flat(yc)
    out["tr.cat.inv"] = flat(cat.inv(yc))
    out["tr.cat.ladj"] = flat(cat.log_abs_det_jacobian(z, yc))
    cat0 = transforms.CatTransform([transforms.ExpTransform(), transforms.SigmoidTransform()], dim=0, lengths=[1, 1])
    out["tr.cat0.ladj"] = flat(cat0.log_abs_det_jacobian(z, cat0(z)))
    st = transforms.StackTransform([transforms.ExpTransform(), transforms.TanhTransform()], dim=0)
    ys = st(z)
    out["tr.stack.y"] = flat(ys)
    out["tr.stack.inv"] = flat(st.inv(ys))
    out["tr.stack.ladj"] = flat(st.log_abs_det_jacobian(z, ys))
    cdt = transforms.CumulativeDistributionTransform(D.Normal(t([0.5]), t([2.0])))
    yd = cdt(z)
    out["tr.cdf.y"] = flat(yd)
    out["tr.cdf.inv"] = flat(cdt.inv(yd))
    out["tr.cdf.ladj"] = flat(cdt.log_abs_det_jacobian(z, yd))
    # a copula-style distribution through the CDF transform
    td = D.TransformedDistribution(D.Normal(t([0.2, -0.1]), t([1.0, 0.5])), [transforms.CumulativeDistributionTransform(D.Normal(t([0.0, 0.0]), t([1.5, 1.5])))])
    out["tr.cdf.dist"] = flat(td.log_prob(t([0.3, 0.6])))
    for name, c in (("lower_cholesky", D.constraints.lower_cholesky), ("positive_definite", D.constraints.positive_definite),
                    ("corr_cholesky", D.constraints.corr_cholesky)):
        tr_ = D.transform_to(c)
        v = tr_(z[..., :3] if name == "corr_cholesky" else m)
        out["transform_to." + name] = flat(v)


def kls(out):
    def kl(name, p, q, params):
        k = D.kl_divergence(p, q)
        out["kl." + name] = flat(k)
        if params:
            out["kl." + name + ".grad"] = grads(k, params)

    a, b, c, d = leaf([0.1, -0.5]), leaf([0.8, 1.2]), leaf([0.4, 0.0]), leaf([1.1, 0.6])
    r1, r2 = leaf([0.5, 2.0]), leaf([1.5, 0.7])
    g1, gr1 = leaf([0.7, 3.0]), leaf([1.0, 0.5])
    b1, b0 = leaf([0.5, 2.0]), leaf([1.5, 3.0])
    lo, hi = leaf([0.1, 0.3]), leaf([0.6, 0.9])
    pa_s, pa_a = leaf([0.5, 1.2]), leaf([2.5, 4.0])
    pb_s, pb_a = leaf([0.4, 1.0]), leaf([1.5, 3.0])
    cb_p, cb_q = leaf([0.3, 0.8]), leaf([0.6, 0.4995])

    kl("gumbel", D.Gumbel(a, b), D.Gumbel(c, d), [a, b, c, d])
    kl("gumbel_normal", D.Gumbel(a, b), D.Normal(c, d), [a, b, c, d])
    kl("normal_gumbel", D.Normal(a, b), D.Gumbel(c, d), [a, b, c, d])
    kl("exponential_gumbel", D.Exponential(r1), D.Gumbel(c, d), [r1, c, d])
    kl("gamma_gumbel", D.Gamma(g1, gr1), D.Gumbel(c, d), [g1, gr1, c, d])
    kl("uniform_gumbel", D.Uniform(lo, hi), D.Gumbel(c, d), [lo, hi, c, d])
    kl("pareto", D.Pareto(pa_s, pa_a), D.Pareto(pb_s, pb_a), [pa_s, pa_a, pb_s, pb_a])
    kl("pareto_inf", D.Pareto(pb_s, pb_a), D.Pareto(pa_s, pa_a), [])
    kl("pareto_exponential", D.Pareto(pa_s, t([2.5, 0.8])), D.Exponential(r1), [pa_s, r1])
    kl("pareto_gamma", D.Pareto(pa_s, pa_a), D.Gamma(g1, gr1), [pa_s, pa_a, g1, gr1])
    kl("pareto_normal", D.Pareto(pa_s, t([2.5, 1.5])), D.Normal(c, d), [pa_s, c, d])
    kl("uniform_pareto", D.Uniform(t([0.6, 0.2]), t([1.5, 0.9])), D.Pareto(t([0.5, 0.5]), pa_a), [pa_a])
    kl("cbernoulli", D.ContinuousBernoulli(cb_p), D.ContinuousBernoulli(cb_q), [cb_p, cb_q])
    kl("cbernoulli_exponential", D.ContinuousBernoulli(cb_p), D.Exponential(r1), [cb_p, r1])
    kl("cbernoulli_normal", D.ContinuousBernoulli(cb_p), D.Normal(c, d), [cb_p, c, d])
    kl("cbernoulli_uniform", D.ContinuousBernoulli(cb_p), D.Uniform(t([-0.5, 0.0]), t([1.5, 1.0])), [cb_p])
    kl("beta_cbernoulli", D.Beta(b1, b0), D.ContinuousBernoulli(cb_q), [b1, b0, cb_q])
    kl("uniform_cbernoulli", D.Uniform(lo, hi), D.ContinuousBernoulli(cb_q), [lo, hi, cb_q])
    kl("beta_exponential", D.Beta(b1, b0), D.Exponential(r1), [b1, b0, r1])
    kl("beta_gamma", D.Beta(b1, b0), D.Gamma(g1, gr1), [b1, b0, g1, gr1])
    kl("beta_normal", D.Beta(b1, b0), D.Normal(c, d), [b1, b0, c, d])
    kl("beta_uniform", D.Beta(b1, b0), D.Uniform(t([-0.5, 0.2]), t([1.5, 1.0])), [b1, b0])
    kl("exponential_normal", D.Exponential(r1), D.Normal(c, d), [r1, c, d])
    kl("gamma_exponential", D.Gamma(g1, gr1), D.Exponential(r1), [g1, gr1, r1])
    kl("gamma_normal", D.Gamma(g1, gr1), D.Normal(c, d), [g1, gr1, c, d])
    kl("laplace_normal", D.Laplace(a, b), D.Normal(c, d), [a, b, c, d])
    kl("uniform_beta", D.Uniform(lo, hi), D.Beta(b1, b0), [lo, hi, b1, b0])
    kl("uniform_beta_inf", D.Uniform(t([-0.1, 0.3]), t([0.5, 0.9])), D.Beta(b1, b0), [])
    kl("uniform_exponential", D.Uniform(t([0.5, -1.0]), t([1.5, 2.0])), D.Exponential(r1), [r1])
    kl("uniform_gamma", D.Uniform(t([0.5, -1.0]), t([1.5, 2.0])), D.Gamma(g1, gr1), [g1, gr1])
    kl("bernoulli_poisson", D.Bernoulli(t([0.2, 0.7])), D.Poisson(r1), [r1])
    lowr = D.LowRankMultivariateNormal(t([0.5, -1.0, 0.2]), leaf([[0.5, 0.1], [-0.3, 0.8], [0.2, 0.2]]), leaf([0.7, 1.2, 0.4]))
    mvn = D.MultivariateNormal(t([0.0, 0.3, -0.1]), scale_tril=leaf([[1.2, 0.0, 0.0], [0.4, 0.9, 0.0], [-0.3, 0.2, 0.7]]))
    kl("lowrank_mvn", lowr, mvn, [lowr._unbroadcasted_cov_factor, lowr._unbroadcasted_cov_diag, mvn._unbroadcasted_scale_tril])
    kl("mvn_lowrank", mvn, lowr, [lowr._unbroadcasted_cov_factor, lowr._unbroadcasted_cov_diag, mvn._unbroadcasted_scale_tril])
    for name, p, q in (("inf.beta_pareto", D.Beta(b1, b0), D.Pareto(pa_s, pa_a)),
                       ("inf.cbernoulli_pareto", D.ContinuousBernoulli(cb_p), D.Pareto(pa_s, pa_a)),
                       ("inf.exponential_beta", D.Exponential(r1), D.Beta(b1, b0)),
                       ("inf.exponential_uniform", D.Exponential(r1), D.Uniform(lo, hi)),
                       ("inf.gamma_cbernoulli", D.Gamma(g1, gr1), D.ContinuousBernoulli(cb_p)),
                       ("inf.gumbel_gamma", D.Gumbel(a, b), D.Gamma(g1, gr1)),
                       ("inf.laplace_uniform", D.Laplace(a, b), D.Uniform(lo, hi)),
                       ("inf.normal_exponential", D.Normal(a, b), D.Exponential(r1)),
                       ("inf.pareto_uniform", D.Pareto(pa_s, pa_a), D.Uniform(lo, hi)),
                       ("inf.poisson_bernoulli", D.Poisson(r1), D.Bernoulli(t([0.2, 0.7]))),
                       ("inf.poisson_binomial", D.Poisson(r1), D.Binomial(3, t([0.2, 0.7])))):
        out["kl." + name] = flat(D.kl_divergence(p, q))
    # the exponential-family Bregman divergence (Wishart has no closed form registered)
    df_p, df_q = leaf([4.0, 5.5]), leaf([3.5, 6.0])
    cov_q = t([[1.0, 0.2, 0.0], [0.2, 2.0, 0.3], [0.0, 0.3, 0.5]])
    kl("wishart", D.Wishart(df_p, covariance_matrix=t([[2.0, 0.3, 0.1], [0.3, 1.5, -0.2], [0.1, -0.2, 1.0]])),
       D.Wishart(df_q, covariance_matrix=cov_q), [df_p, df_q])


def results2():
    out = {}
    continuous(out)
    discrete(out)
    matrices(out)
    transformed(out)
    kls(out)
    return out
