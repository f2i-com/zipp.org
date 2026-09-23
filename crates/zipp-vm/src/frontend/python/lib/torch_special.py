"""torch.special for Zipp: the special functions, each with its gradient.

The kernels (`_zipp_tensor`'s unary/binary tables) compute in double
precision with the algorithms ATen uses (Cephes' expansions, the Hurwitz
zeta, the incomplete gamma series and continued fraction) and round once to
the result dtype; integer inputs give the default float dtype, as PyTorch
promotes them. The functions PyTorch also has at the top level
(`torch.lgamma`, `torch.xlogy`, ...) live in `torch` and are named here.
"""
import torch

_unary = torch._unary
_binary = torch._binary
_float_input = torch._float_input
_float_result = torch._float_result
_unbroadcast = torch._unbroadcast

gammaln = torch.lgamma
digamma = torch.digamma
psi = torch.digamma
polygamma = torch.polygamma
multigammaln = torch.mvlgamma
i0 = torch.i0
modified_bessel_i0 = torch.i0
sinc = torch.sinc
logit = torch.logit
expit = torch.sigmoid
xlogy = torch.xlogy
gammainc = torch.igamma
gammaincc = torch.igammac
erf = torch.erf
erfc = torch.erfc
erfinv = torch.erfinv
exp2 = torch.exp2
expm1 = torch.expm1
log1p = torch.log1p
round = torch.round
logsumexp = torch.logsumexp


def softmax(input, dim, *, dtype=None):
    return torch.softmax(input, dim, dtype=dtype)


def log_softmax(input, dim, *, dtype=None):
    return torch.log_softmax(input, dim, dtype=dtype)


def erfcx(input):
    """exp(x**2) * erfc(x), without the overflow of either factor."""
    return _unary("erfcx", _float_input(input), "SpecialErfcx",
                  lambda g, x, o: torch.mul(g, torch.sub(torch.mul(torch.mul(x, 2), o), 1.1283791670955126)))


def i0e(input):
    """exp(-|x|) * i0(x)."""
    return _unary("i0e", _float_input(input), "SpecialI0E", torch._i0e_backward)


def i1(input):
    return _unary("i1", _float_input(input), "SpecialI1", torch._i1_backward)


modified_bessel_i1 = i1


def i1e(input):
    """exp(-|x|) * i1(x)."""
    return _unary("i1e", _float_input(input), "SpecialI1E", torch._i1e_backward)


def ndtr(input):
    """The standard normal CDF, as PyTorch composes it: (1 + erf(x / sqrt 2)) / 2."""
    return torch.mul(torch.add(torch.erf(torch.mul(_float_input(input), 0.7071067811865476)), 1), 0.5)


def ndtri(input):
    """The standard normal quantile function (the inverse of ndtr)."""
    return _unary("ndtri", _float_input(input), "SpecialNdtri",
                  lambda g, x, o: torch.mul(g, torch.mul(torch.exp(torch.mul(torch.square(o), 0.5)), 2.5066282746310002)), saves="o")


def log_ndtr(input):
    """log(ndtr(x)), accurate far into the lower tail."""
    return _unary("log_ndtr", _float_input(input), "SpecialLogNdtr",
                  lambda g, x, o: torch.mul(g, torch.mul(torch.exp(torch.neg(torch.add(o, torch.mul(torch.square(x), 0.5)))), 0.3989422804014327)))


def entr(input):
    """-x log x for x > 0, 0 at 0, -inf below."""
    return _unary("entr", _float_input(input), "SpecialEntr", lambda g, x, o: torch.mul(g, torch.neg(torch.add(torch.log(x), 1))))


def xlog1py(input, other):
    """x * log1p(y), 0 where x is 0."""
    def backward(g, x, y, o):
        # PyTorch's: xlog1py(g, y), zero where x is 0 and y <= -1.
        return (_unbroadcast(torch.where(torch.logical_and(x == 0, y <= -1), 0.0, xlog1py(g, y)), x.shape) if x.requires_grad else None,
                _unbroadcast(torch.mul(g, torch.div(x, torch.add(y, 1))), y.shape) if y.requires_grad else None)
    return _binary("xlog1py", input, other, "SpecialXlog1Py", backward, ("xy", "xy"), _float_result(input, other))


def zeta(input, other):
    """The Hurwitz zeta function: the sum over k >= 0 of (k + other)**-input."""
    def backward(g, x, q, o):
        if x.requires_grad:
            raise NotImplementedError("the derivative for 'self' is not implemented.")
        return (None, _unbroadcast(torch.mul(g, torch.mul(torch.neg(x), zeta(torch.add(x, 1), q))), q.shape))
    return _binary("zeta", input, other, "SpecialZeta", backward, ("xy", "xy"), _float_result(input, other))
