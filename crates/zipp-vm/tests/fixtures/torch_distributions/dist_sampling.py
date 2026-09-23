# Sampling checks. Streams differ from PyTorch's, so samples are checked by
# their moments (sample mean and variance within six standard errors
# estimated from the sample itself), and rsample gradients by exact
# reparameterisation identities (d(loc + scale * eps)/d scale = eps, ...) or,
# for the implicitly reparameterised Gamma family, by E[dx/da] = dE[x]/da.
# Every line printed starts with "ok" or "FAIL"; the same file runs under
# PyTorch as a sanity check of the bounds.
import math
import torch
import torch.distributions as D

torch.manual_seed(1234)
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


def close(name, got, want, tol=1e-9):
    got = torch.as_tensor(got, dtype=F64)
    want = torch.as_tensor(want, dtype=F64)
    ok = bool(((got - want).abs() <= tol * (1 + want.abs())).all())
    report(name, ok, "%s vs %s" % (got.tolist(), want.tolist()))


def leaf(values):
    return torch.tensor(values, dtype=F64, requires_grad=True)


# location-scale families: moments and exact rsample gradients
loc, scale = leaf([1.0, -2.0]), leaf([0.5, 2.0])
d = D.Normal(loc, scale)
moments("normal sample", d.sample((N,)), [1.0, -2.0], [0.25, 4.0])
x = d.rsample((N,))
moments("normal rsample", x, [1.0, -2.0], [0.25, 4.0])
x.sum().backward()
close("normal d/dloc", loc.grad, [N, N])
close("normal d/dscale", scale.grad, ((x.detach() - loc.detach()) / scale.detach()).sum(0))

low, high = leaf([0.0, -1.0]), leaf([2.0, 3.0])
x = D.Uniform(low, high).rsample((N,))
moments("uniform", x, [1.0, 1.0], [4.0 / 12, 16.0 / 12])
x.sum().backward()
u = (x.detach() - low.detach()) / (high.detach() - low.detach())
close("uniform d/dlow", low.grad, (1 - u).sum(0))
close("uniform d/dhigh", high.grad, u.sum(0))

rate = leaf([0.5, 3.0])
x = D.Exponential(rate).rsample((N,))
moments("exponential", x, [2.0, 1.0 / 3], [4.0, 1.0 / 9])
x.sum().backward()
close("exponential d/drate", rate.grad, (-x.detach() / rate.detach()).sum(0))

loc, scale = leaf([0.5, -1.0]), leaf([1.0, 0.3])
x = D.Laplace(loc, scale).rsample((N,))
moments("laplace", x, [0.5, -1.0], [2.0, 0.18])
x.sum().backward()
close("laplace d/dscale", scale.grad, ((x.detach() - loc.detach()) / scale.detach()).sum(0))

loc, scale = leaf([0.5]), leaf([2.0])
x = D.Cauchy(loc, scale).rsample((N,))
q = x.detach().reshape(-1).sort()[0]
median = float(q[N // 2])
iqr = float(q[3 * N // 4] - q[N // 4])
report("cauchy median", abs(median - 0.5) < 0.25, str(median))
report("cauchy iqr", abs(iqr - 4.0) < 0.5, str(iqr))
x.sum().backward()
close("cauchy d/dscale", scale.grad, ((x.detach() - loc.detach()) / scale.detach()).sum(0))

x = D.LogNormal(torch.tensor([0.2], dtype=F64), torch.tensor([0.4], dtype=F64)).sample((N,))
moments("lognormal", x, [math.exp(0.2 + 0.08)], [(math.exp(0.16) - 1) * math.exp(0.4 + 0.16)])
x = D.HalfNormal(torch.tensor([1.5], dtype=F64)).sample((N,))
moments("halfnormal", x, [1.5 * math.sqrt(2 / math.pi)], [2.25 * (1 - 2 / math.pi)])
x = D.HalfCauchy(torch.tensor([1.5], dtype=F64)).sample((N,))
report("halfcauchy median", abs(float(x.reshape(-1).sort()[0][N // 2]) - 1.5) < 0.2)
report("halfcauchy positive", bool((x >= 0).all()))

# the Gamma family
conc, rate = leaf([0.7, 3.0]), leaf([1.0, 2.0])
d = D.Gamma(conc, rate)
x = d.rsample((N,))
moments("gamma", x, [0.7, 1.5], [0.7, 0.75])
report("gamma positive", bool((x > 0).all()))
x.sum().backward()
close("gamma d/drate", rate.grad, (-x.detach() / rate.detach()).sum(0))
# E[dx/dconc] = d E[x] / d conc = 1 / rate
per_sample = (torch._standard_gamma_grad if hasattr(torch, "_standard_gamma_grad") else D._standard_gamma_grad)(
    conc.detach().expand(N, 2), x.detach() * rate.detach()) / rate.detach()
close("gamma d/dconc is the implicit gradient", conc.grad, per_sample.sum(0), 1e-6)
se = per_sample.std(0) / math.sqrt(N)
report("gamma E[dx/dconc]", bool(((per_sample.mean(0) - 1 / rate.detach()).abs() < 6 * se).all()),
       "%s" % per_sample.mean(0).tolist())

df = leaf([3.0])
x = D.Chi2(df).rsample((N,))
moments("chi2", x, [3.0], [6.0])
x.sum().backward()
report("chi2 E[dx/ddf]", abs(float(df.grad) / N - 1.0) < 0.1, str(float(df.grad) / N))

c1, c0 = leaf([2.0, 0.5]), leaf([3.0, 0.5])
x = D.Beta(c1, c0).rsample((N,))
moments("beta", x, [0.4, 0.5], [0.04, 0.125])
x.sum().backward()
# d E[x] / d c1 = c0 / (c1 + c0)^2
want = c0.detach() / (c1.detach() + c0.detach()) ** 2
report("beta E[dx/dc1]", bool(((c1.grad / N - want).abs() < 0.1 * want + 0.01).all()), "%s vs %s" % ((c1.grad / N).tolist(), want.tolist()))

conc = leaf([1.0, 2.0, 3.0])
x = D.Dirichlet(conc).rsample((N,))
close("dirichlet simplex", x.detach().sum(-1), torch.ones(N, dtype=F64), 1e-9)
moments("dirichlet", x, [1 / 6, 2 / 6, 3 / 6], [(1 * 5) / (36 * 7), (2 * 4) / (36 * 7), (3 * 3) / (36 * 7)])
x[:, 0].sum().backward()
# d E[x0] / d a0 = (a_sum - a0) / a_sum^2 = 5 / 36, and / d a1 = -a0 / a_sum^2
report("dirichlet E[dx0/da]", abs(float(conc.grad[0]) / N - 5 / 36) < 0.02 and abs(float(conc.grad[1]) / N + 1 / 36) < 0.02,
       str((conc.grad / N).tolist()))

df, loc = leaf([5.0]), leaf([1.0])
x = D.StudentT(df, loc, torch.tensor([2.0], dtype=F64)).rsample((N,))
moments("studentt", x, [1.0], [4.0 * 5 / 3])
x.sum().backward()
close("studentt d/dloc", loc.grad, [N])

# discrete distributions
x = D.Bernoulli(torch.tensor([0.2, 0.7], dtype=F64)).sample((N,))
moments("bernoulli", x, [0.2, 0.7], [0.16, 0.21])
p = torch.tensor([0.1, 0.2, 0.3, 0.4], dtype=F64)
x = D.Categorical(p).sample((N,))
report("categorical dtype", x.dtype == torch.int64)
moments("categorical", torch.nn.functional.one_hot(x, 4).to(F64), p, p * (1 - p))
x = D.OneHotCategorical(p).sample((N,))
moments("onehot", x, p, p * (1 - p))
x = D.Binomial(torch.tensor([10.0, 60.0], dtype=F64), torch.tensor([0.3, 0.55], dtype=F64)).sample((N,))
moments("binomial", x, [3.0, 33.0], [2.1, 14.85])
x = D.Multinomial(8, p).sample((N,))
close("multinomial count", x.sum(-1), torch.full((N,), 8.0, dtype=F64))
moments("multinomial", x, 8 * p, 8 * p * (1 - p))
x = D.Poisson(torch.tensor([0.5, 4.0], dtype=F64)).sample((N,))
moments("poisson", x, [0.5, 4.0], [0.5, 4.0])
x = D.Geometric(torch.tensor([0.3, 0.8], dtype=F64)).sample((N,))
moments("geometric", x, [0.7 / 0.3, 0.2 / 0.8], [0.7 / 0.09, 0.2 / 0.64])

# multivariate
loc = leaf([1.0, -1.0, 0.5])
cov = torch.tensor([[2.0, 0.3, 0.1], [0.3, 1.5, -0.2], [0.1, -0.2, 1.0]], dtype=F64)
x = D.MultivariateNormal(loc, cov).rsample((N,))
moments("mvn", x, [1.0, -1.0, 0.5], torch.diagonal(cov))
xc = x.detach() - x.detach().mean(0)
emp = xc.T @ xc / N
report("mvn covariance", bool(((emp - cov).abs() < 0.15).all()), str(emp.tolist()))
x.sum().backward()
close("mvn d/dloc", loc.grad, [N, N, N])
W = torch.tensor([[0.5, 0.1], [-0.3, 0.8], [0.2, 0.2]], dtype=F64)
diag = torch.tensor([0.7, 1.2, 0.4], dtype=F64)
x = D.LowRankMultivariateNormal(torch.zeros(3, dtype=F64), W, diag).sample((N,))
lr_cov = W @ W.T + torch.diag(diag)
moments("lowrank", x, [0.0, 0.0, 0.0], torch.diagonal(lr_cov))
emp = x.T @ x / N
report("lowrank covariance", bool(((emp - lr_cov).abs() < 0.15).all()), str(emp.tolist()))

gmm = D.MixtureSameFamily(D.Categorical(torch.tensor([0.3, 0.7], dtype=F64)),
                          D.Normal(torch.tensor([-2.0, 1.0], dtype=F64), torch.tensor([0.5, 1.0], dtype=F64)))
moments("mixture", gmm.sample((N,)), gmm.mean, gmm.variance)
ind = D.Independent(D.Normal(torch.zeros(2, 3, dtype=F64), 1.0), 1)
report("independent shape", list(ind.sample((5,)).shape) == [5, 2, 3])

# relaxed and straight-through
probs = leaf([0.3, 0.8])
x = D.RelaxedBernoulli(torch.tensor(0.5, dtype=F64), probs).rsample((N,))
report("relaxed bernoulli range", bool(((x > 0) & (x < 1)).all()))
x.sum().backward()
report("relaxed bernoulli grad", bool((probs.grad > 0).all()), str(probs.grad.tolist()))
probs = leaf([0.2, 0.3, 0.5])
x = D.RelaxedOneHotCategorical(torch.tensor(0.3, dtype=F64), probs).rsample((N,))
close("relaxed onehot simplex", x.detach().sum(-1), torch.ones(N, dtype=F64), 1e-9)
report("relaxed onehot argmax", bool(((torch.nn.functional.one_hot(x.detach().argmax(-1), 3).to(F64).mean(0) - probs.detach()).abs() < 0.05).all()))
probs = leaf([0.2, 0.3, 0.5])
w = torch.tensor([1.0, 2.0, 3.0], dtype=F64)
x = D.OneHotCategoricalStraightThrough(probs).rsample((N,))
(x * w).sum().backward()
# d/dp_j of sum_i w_i p_i / sum(p) with sum(p) = 1
close("straight-through grad", probs.grad, N * (w - (w * probs.detach()).sum()))
td = D.TransformedDistribution(D.Normal(torch.zeros(2, dtype=F64), 1.0), [D.ExpTransform()])
moments("transformed", td.sample((N,)), [math.exp(0.5)] * 2, [(math.e - 1) * math.e] * 2)

# the seed fixes the stream
torch.manual_seed(7)
a = D.Gamma(torch.tensor([0.5, 2.0]), 1.0).sample((3,))
b = D.Binomial(10, torch.tensor([0.3])).sample((3,))
torch.manual_seed(7)
report("reproducible", bool((a == D.Gamma(torch.tensor([0.5, 2.0]), 1.0).sample((3,))).all()) and
       bool((b == D.Binomial(10, torch.tensor([0.3])).sample((3,))).all()))
