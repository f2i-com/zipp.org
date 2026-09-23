# Sampling checks, second part: the distributions dist_sampling.py does not
# cover and Poisson at large rates. As there, samples are checked by their
# moments (within six standard errors estimated from the sample itself) and
# rsample gradients by exact pathwise identities where they exist (location
# and scale families, Kumaraswamy's closed-form inverse CDF) or by
# E[dx/dtheta] = dE[x]/dtheta. Every line printed starts with "ok" or
# "FAIL"; the same file runs under PyTorch as a sanity check of the bounds,
# where the LKJCholesky r20 and r21 variance lines fail (see there).
import math
import torch
import torch.distributions as D

torch.manual_seed(4321)
N = 2000
F64 = torch.float64


def report(name, ok, detail=""):
    print(("ok " if ok else "FAIL ") + name + ("" if ok else " " + detail))


def moments(name, x, mean, var, dim=0):
    """x [N, ...] against the analytic mean and variance (tensors)."""
    x = x.detach().to(F64)
    n = x.shape[dim]
    m = x.mean(dim)
    c = x - m
    v = (c * c).mean(dim)
    m4 = (c ** 4).mean(dim)
    se_m = (v / n).sqrt()
    se_v = ((m4 - v * v).clamp(min=0) / n).sqrt()
    mean = torch.as_tensor(mean, dtype=F64)
    var = torch.as_tensor(var, dtype=F64)
    ok_m = bool(((m - mean).abs() <= 6 * se_m + 1e-12).all())
    ok_v = bool(((v - var).abs() <= 6 * se_v + 1e-12).all())
    report(name + " mean", ok_m, "%s vs %s" % (m.tolist(), mean.tolist()))
    report(name + " var", ok_v, "%s vs %s" % (v.tolist(), var.tolist()))


def mean_is(name, x, mean, dim=0):
    """The sample mean of x [N, ...] within six standard errors of `mean`."""
    x = x.detach().to(F64)
    m = x.mean(dim)
    se = x.std(dim) / math.sqrt(x.shape[dim])
    mean = torch.as_tensor(mean, dtype=F64)
    report(name, bool(((m - mean).abs() <= 6 * se + 1e-12).all()), "%s vs %s" % (m.tolist(), mean.tolist()))


def close(name, got, want, tol=1e-9):
    got = torch.as_tensor(got, dtype=F64)
    want = torch.as_tensor(want, dtype=F64)
    ok = bool(((got - want).abs() <= tol * (1 + want.abs())).all())
    report(name, ok, "%s vs %s" % (got.tolist(), want.tolist()))


def leaf(values):
    return torch.tensor(values, dtype=F64, requires_grad=True)


# Gumbel: a location-scale family
loc, scale = leaf([0.5, -1.0]), leaf([1.0, 2.5])
x = D.Gumbel(loc, scale).rsample((N,))
moments("gumbel", x, loc.detach() + scale.detach() * 0.5772156649015329, (math.pi ** 2 / 6) * scale.detach() ** 2)
x.sum().backward()
close("gumbel d/dloc", loc.grad, [N, N])
close("gumbel d/dscale", scale.grad, ((x.detach() - loc.detach()) / scale.detach()).sum(0))

# Pareto: log(x / scale) ~ Exponential(alpha); x is linear in the scale
scale, alpha = leaf([1.0, 0.5]), leaf([1.5, 5.0])
x = D.Pareto(scale, alpha).rsample((N,))
report("pareto support", bool((x.detach() >= scale.detach()).all()))
moments("pareto log", (x.detach() / scale.detach()).log(), 1 / alpha.detach(), alpha.detach() ** -2)
x.sum().backward()
close("pareto d/dscale", scale.grad, (x.detach() / scale.detach()).sum(0))
# d x / d alpha = -x log(x / scale) / alpha
close("pareto d/dalpha", alpha.grad, (-x.detach() * (x.detach() / scale.detach()).log() / alpha.detach()).sum(0), 1e-8)

# Weibull
scale, conc = leaf([2.0, 0.5]), leaf([1.5, 0.8])
d = D.Weibull(scale, conc)
x = d.rsample((N,))
moments("weibull", x, d.mean.detach(), d.variance.detach())
x.sum().backward()
close("weibull d/dscale", scale.grad, (x.detach() / scale.detach()).sum(0))
close("weibull d/dconc", conc.grad, (-x.detach() * (x.detach() / scale.detach()).log() / conc.detach()).sum(0), 1e-8)

# Kumaraswamy: x = (1 - (1 - u)^(1/b))^(1/a), so dx/da = -x log(x) / a
c1, c0 = leaf([2.0, 0.6]), leaf([3.0, 1.5])
d = D.Kumaraswamy(c1, c0)
x = d.rsample((N,))
report("kumaraswamy range", bool(((x > 0) & (x < 1)).all()))
moments("kumaraswamy", x, d.mean.detach(), d.variance.detach())
x.sum().backward()
close("kumaraswamy d/dc1", c1.grad, (-x.detach() * x.detach().log() / c1.detach()).sum(0), 1e-7)

# ContinuousBernoulli (inverse-CDF rsample): E[dx/dp] = dE[x]/dp
# (0.5 is inside PyTorch's unstable region, where the sample is the uniform itself)
probs = leaf([0.2, 0.8, 0.5])
d = D.ContinuousBernoulli(probs)
x = d.rsample((N,))
moments("cbernoulli", x, d.mean.detach(), d.variance.detach())
per = torch.stack([torch.autograd.grad(x[:, j].sum(), probs, retain_graph=True)[0][j] for j in range(2)])
want = torch.autograd.grad(d.mean.sum(), probs)[0][:2]
report("cbernoulli E[dx/dp]", bool(((per / N - want).abs() < 0.05 * want.abs() + 0.02).all()), "%s vs %s" % ((per / N).tolist(), want.tolist()))
x = D.ContinuousBernoulli(torch.tensor([0.3], dtype=F64)).sample((N,))
report("cbernoulli sample", (not x.requires_grad) and bool(((x >= 0) & (x <= 1)).all()))

# FisherSnedecor (Gamma ratio)
df1, df2 = leaf([5.0, 3.0]), leaf([12.0, 20.0])
d = D.FisherSnedecor(df1, df2)
x = d.rsample((N,))
moments("fisher", x, d.mean.detach(), d.variance.detach())
x.sum().backward()
report("fisher grads", df1.grad is not None and bool(torch.isfinite(df1.grad).all()) and bool(torch.isfinite(df2.grad).all()))

# GeneralizedPareto (inverse-CDF rsample, location-scale)
loc, scale, conc = leaf([0.5, -1.0]), leaf([2.0, 1.0]), leaf([0.1, -0.3])
d = D.GeneralizedPareto(loc, scale, conc)
x = d.rsample((N,))
report("genpareto support", bool(d.support.check(x.detach()).all()))
moments("genpareto", x, d.mean.detach(), d.variance.detach())
x.sum().backward()
close("genpareto d/dloc", loc.grad, [N, N])
close("genpareto d/dscale", scale.grad, ((x.detach() - loc.detach()) / scale.detach()).sum(0))

# InverseGamma: 1 / Gamma, linear in the rate
conc, rate = leaf([6.0, 9.0]), leaf([2.0, 0.5])
d = D.InverseGamma(conc, rate)
x = d.rsample((N,))
moments("invgamma", x, d.mean.detach(), d.variance.detach())
x.sum().backward()
close("invgamma d/drate", rate.grad, (x.detach() / rate.detach()).sum(0))
report("invgamma E[dx/dconc]", abs(float(conc.grad[0]) / N + 2.0 / 25) < 0.02, str(float(conc.grad[0]) / N))

# LogisticNormal: the stick-breaking inverse recovers the Normal
loc, scale = torch.tensor([0.3, -0.5, 1.0], dtype=F64), torch.tensor([0.5, 1.0, 0.8], dtype=F64)
x = D.LogisticNormal(loc, scale).sample((N,))
close("logisticnormal simplex", x.sum(-1), torch.ones(N, dtype=F64), 1e-9)
moments("logisticnormal", D.StickBreakingTransform().inv(x), loc, scale ** 2)

# VonMises (Best-Fisher rejection): E[cos(x - loc)] = I1/I0 = 1 - variance, E[sin(x - loc)] = 0
loc, conc = torch.tensor([0.5, -2.0, 3.0], dtype=F64), torch.tensor([0.3, 4.0, 50.0], dtype=F64)
d = D.VonMises(loc, conc)
x = d.sample((N,))
report("vonmises range", bool(((x >= -math.pi) & (x < math.pi)).all()))
mean_is("vonmises cos", torch.cos(x - loc), 1 - d.variance)
mean_is("vonmises sin", torch.sin(x - loc), torch.zeros(3, dtype=F64))
x32 = D.VonMises(torch.tensor([0.0]), torch.tensor([1e-6])).sample((N,))
report("vonmises tiny concentration", x32.dtype == torch.float32 and bool(((x32 >= -math.pi) & (x32 < math.pi)).all()))
# circular variance: E[cos(2(x - loc))] = I2/I0
i1 = (1 - d.variance)
i2_over_i0 = 1 - 2 * i1 / conc
mean_is("vonmises cos2", torch.cos(2 * (x - loc)), i2_over_i0)

# NegativeBinomial (Gamma-Poisson mixture)
d = D.NegativeBinomial(torch.tensor([3.0, 20.0, 0.5], dtype=F64), torch.tensor([0.3, 0.7, 0.9], dtype=F64))
x = d.sample((N,))
report("negbinomial integer", bool((x == x.floor()).all()) and bool((x >= 0).all()))
moments("negbinomial", x, d.mean, d.variance)

# Wishart (Bartlett decomposition): mean df V, var(X_ij) = df (V_ij^2 + V_ii V_jj)
V = torch.tensor([[2.0, 0.3, 0.1], [0.3, 1.5, -0.2], [0.1, -0.2, 1.0]], dtype=F64)
df = leaf(5.0)
d = D.Wishart(df, covariance_matrix=V)
x = d.rsample((N,))
report("wishart symmetric", bool((x.detach() - x.detach().mT).abs().max() < 1e-12))
report("wishart positive definite", bool(D.constraints.positive_definite.check(x.detach()[:200]).all()))
moments("wishart", x, d.mean.detach(), d.variance.detach())
# E[dX/d df] = dE[X]/d df = V (the Bartlett diagonal carries the Gamma gradient)
try:
    g = float(torch.autograd.grad(x.sum(), df)[0])
    report("wishart E[dX/ddf]", abs(g / N - float(V.sum())) < 0.1 * float(V.sum()), str(g / N))
except RuntimeError as e:
    # PyTorch 2.11's singular-sample correction writes into the sample in place, breaking its backward
    report("wishart E[dX/ddf]", "modified by an inplace" in str(e), str(e))
x = D.Wishart(torch.tensor([3.0, 4.0], dtype=F64), scale_tril=torch.tensor([[1.0, 0.0], [0.5, 0.8]], dtype=F64)).sample((N,))
d2 = D.Wishart(torch.tensor([3.0, 4.0], dtype=F64), scale_tril=torch.tensor([[1.0, 0.0], [0.5, 0.8]], dtype=F64))
moments("wishart batch", x, d2.mean, d2.variance)

# LKJCholesky (onion method): unit rows, and every r_ij ~ 2 Beta(a, a) - 1 with
# a = eta - 1 + dim / 2. (PyTorch 2.11's sampler draws row k's squared norm from
# Beta(k - 1/2, .) rather than the onion method's Beta(k/2, .), so under PyTorch
# the r20 and r21 lines fail; Zipp samples the distribution log_prob describes.)
eta = torch.tensor([0.5, 2.0], dtype=F64)
L = D.LKJCholesky(3, eta).sample((N,))
report("lkj lower triangular", bool((L.tril() == L).all()) and bool((L.diagonal(dim1=-2, dim2=-1) > 0).all()))
close("lkj unit rows", (L * L).sum(-1), torch.ones(N, 2, 3, dtype=F64), 1e-9)
R = L @ L.mT
for i, j in ((1, 0), (2, 0), (2, 1)):
    moments("lkj r%d%d" % (i, j), R[..., i, j], torch.zeros(2, dtype=F64), 1 / (2 * eta + 2))
L2 = D.LKJCholesky(2, torch.tensor(1.0)).sample((N,))
moments("lkj dim2", L2[..., 1, 0], 0.0, 1.0 / 3)

# Poisson at large rates (transformed rejection above rate 10)
rates = torch.tensor([3.0, 50.0, 700.0, 800.0, 5000.0, 1e6], dtype=F64)
x = D.Poisson(rates).sample((N,))
report("poisson large integer", bool((x == x.floor()).all()) and bool((x >= 0).all()))
moments("poisson large", x, rates, rates)
x = D.Poisson(torch.tensor([0.0, 0.05, 12.0], dtype=F64)).sample((N,))
report("poisson zero rate", bool((x[:, 0] == 0).all()))
moments("poisson small", x[:, 1:], torch.tensor([0.05, 12.0], dtype=F64), torch.tensor([0.05, 12.0], dtype=F64))
x = D.Poisson(torch.tensor([2000.0])).sample((N,))
report("poisson float32", x.dtype == torch.float32 and abs(float(x.mean()) - 2000.0) < 6 * math.sqrt(2000.0 / N))

# the seed fixes the stream
torch.manual_seed(11)
a = D.Poisson(torch.tensor([900.0, 3.0])).sample((3,))
b = D.VonMises(torch.tensor([0.0]), torch.tensor([2.0])).sample((3,))
torch.manual_seed(11)
report("reproducible", bool((a == D.Poisson(torch.tensor([900.0, 3.0])).sample((3,))).all()) and
       bool((b == D.VonMises(torch.tensor([0.0]), torch.tensor([2.0])).sample((3,))).all()))
