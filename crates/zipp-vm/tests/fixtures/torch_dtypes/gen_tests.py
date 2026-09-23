"""Writes crates/zipp-vm/tests/python_torch_dtypes.rs: the float16/bfloat16/
int8/int16 dtypes and the special functions of the bundled `torch`, checked
against CPython 3.11 + PyTorch 2.11. Every expected value is computed here
by PyTorch and written into the Rust test as a literal; the embedded Python
program recomputes each case under Zipp.

    python -P crates/zipp-vm/tests/fixtures/torch_dtypes/gen_tests.py [--fixtures]

`--fixtures` also rewrites dtypes.pt here (a checkpoint PyTorch saves with
every new storage type), which the tests load.

Tolerances: 0 means exact (the value PyTorch's CPU kernel gives, bit for
bit). A float16 result that PyTorch's kernel accumulates in its own blocked
float order (matmul, softmax, large sums) is allowed one float16 ulp,
written as 1e-3 (relative above 1, absolute below); bfloat16 likewise 8e-3.
"""
import math
import sys
import textwrap
import torch

assert torch.__version__.startswith("2.11"), torch.__version__

NORM = r'''
import math
import torch

def norm(v):
    if isinstance(v, torch.Tensor):
        flat = v.detach().reshape(-1).tolist()
        return ["T", list(v.shape), str(v.dtype), flat]
    if isinstance(v, torch.dtype):
        return str(v)
    if isinstance(v, torch.Size):
        return list(v)
    if isinstance(v, (list, tuple)):
        return [norm(x) for x in v]
    if isinstance(v, dict):
        return [[k, norm(v[k])] for k in sorted(v)]
    return v

def close(got, want, tol):
    if isinstance(want, list):
        return isinstance(got, list) and len(got) == len(want) and all(close(g, w, tol) for g, w in zip(got, want))
    if isinstance(want, bool) or want is None or isinstance(want, str):
        return got == want
    if isinstance(want, int) and not isinstance(want, bool):
        return (isinstance(got, int) and not isinstance(got, bool) and got == want) or (isinstance(got, float) and got == want and tol > 0)
    if isinstance(want, float):
        if not isinstance(got, (int, float)) or isinstance(got, bool):
            return False
        if math.isnan(want):
            return math.isnan(got)
        if math.isinf(want):
            return got == want
        return abs(got - want) <= tol * max(1.0, abs(want))
    return got == want

def check(label, fn, want, tol=0):
    try:
        got = norm(fn())
    except Exception as e:
        got = ["E", type(e).__name__]
    ok = close(got, want, tol)
    print(label, ok if ok else "False got=%r want=%r" % (got, want))

def err(fn, *words):
    """The exception type, and whether its message holds every word."""
    try:
        fn()
    except Exception as e:
        return [type(e).__name__, all(w in str(e) for w in words)]
    return "no error"

# Deterministic inputs (Python arithmetic, the same under both runtimes).
def vals(n, scale=7.3, mod=101):
    return [((i * 37) % mod - mod // 2) / scale for i in range(n)]

def seq(n, dt=torch.float32, scale=7.3, mod=101):
    return torch.tensor(vals(n, scale, mod), dtype=torch.float64).to(dt)
'''

exec(NORM)


def lit(v):
    if isinstance(v, float):
        if math.isnan(v):
            return 'float("nan")'
        if math.isinf(v):
            return 'float("inf")' if v > 0 else '-float("inf")'
        return repr(v)
    if isinstance(v, list):
        return "[" + ", ".join(lit(x) for x in v) + "]"
    return repr(v)


TESTS = []


def test(name, doc, setup, cases):
    TESTS.append((name, doc, textwrap.dedent(setup).strip("\n"), cases))


NAMES = ["uint8", "int8", "int16", "int32", "int64", "float16", "bfloat16", "float32", "float64", "bool"]

test("dtypes_promote_and_describe_themselves_as_in_pytorch",
     "The new dtypes, their aliases and attributes, PyTorch's promotion table (uint8 with int8 is int16, float16 with bfloat16 is float32), result_type with Python scalars and 0-d tensors, finfo/iinfo, legacy type names and constructors.",
     '''
     NAMES = %r
     def table():
         return [[str(torch.promote_types(getattr(torch, a), getattr(torch, b))) for b in NAMES] for a in NAMES]
     def attrs():
         return [[str(d), d.itemsize, d.is_floating_point, d.is_signed] for d in (torch.float16, torch.bfloat16, torch.int8, torch.int16, torch.half, torch.short)]
     def finfo(d):
         f = torch.finfo(d)
         return [f.bits, f.eps, f.max, f.min, f.tiny, f.smallest_normal, f.resolution, f.dtype]
     def iinfo(d):
         i = torch.iinfo(d)
         return [i.bits, i.min, i.max, i.dtype]
     h = torch.ones(3, dtype=torch.half)
     c = torch.ones(3, dtype=torch.int8)
     ''' % NAMES,
     [("table", "table()", 0), ("attrs", "attrs()", 0),
      ("aliases", "[torch.half is torch.float16, torch.short is torch.int16, torch.float16 == torch.half, torch.bfloat16 != torch.half]", 0),
      ("scalars", "[(h + 1.5).dtype, (h * 2).dtype, (c + 1).dtype, (c + 1.5).dtype, (c * True).dtype, (h + torch.tensor(2.0)).dtype, (h + torch.ones(3)).dtype, (c + torch.tensor(300)).dtype, (h + torch.ones(3, dtype=torch.bfloat16)).dtype, (c / c).dtype, (h / 2).dtype, (c + torch.ones(3, dtype=torch.uint8)).dtype]", 0),
      ("result_type", "[torch.result_type(h, 1.0), torch.result_type(c, 2), torch.result_type(torch.tensor(1, dtype=torch.int8), torch.tensor(1, dtype=torch.uint8)), torch.result_type(c, torch.tensor(1.0, dtype=torch.float64))]", 0),
      ("can_cast", "[torch.can_cast(torch.half, torch.int8), torch.can_cast(torch.int16, torch.half), torch.can_cast(torch.bfloat16, torch.float16)]", 0),
      ("finfo_half", "finfo(torch.half)", 0), ("finfo_bf16", "finfo(torch.bfloat16)", 0), ("iinfo_int8", "iinfo(torch.int8)", 0), ("iinfo_int16", "iinfo(torch.int16)", 0),
      ("finfo_repr", "[repr(torch.finfo(torch.float16)), repr(torch.finfo(torch.bfloat16)), repr(torch.iinfo(torch.int8))]", 0),
      ("types", "[torch.ones(1, dtype=d).type() for d in (torch.half, torch.bfloat16, torch.int8, torch.int16)]", 0),
      ("legacy", "[torch.HalfTensor([1.1, 2]), torch.BFloat16Tensor([1.1]), torch.CharTensor([200, 3]), torch.ShortTensor(2), torch.ones(2).type(torch.HalfTensor), torch.ones(2).type('torch.CharTensor')]", 0),
      ("element_size", "[torch.ones(1, dtype=d).element_size() for d in (torch.half, torch.bfloat16, torch.int8, torch.int16)]", 0),
      ("is_signed", "[torch.ones(1, dtype=torch.half).is_signed(), c.is_signed(), torch.ones(1, dtype=torch.uint8).is_signed()]", 0)])

test("float16_and_bfloat16_round_every_value_to_the_format",
     "Creation and conversion round to nearest even, float16 with subnormals and overflow to inf, bfloat16 on the upper 16 bits of a float32; a double converts through float, as c10::Half does; `view` exposes the bits; int8/int16 wrap.",
     '''
     EDGE = [65504.0, 65519.0, 65520.0, 1e5, -1e5, 1e-8, 3e-8, 6e-8, 6.1e-5, 1.0 + 2 ** -11, 1.0 + 3 * 2 ** -11, 0.1, -0.0, 1.0 + 2 ** -11 + 2 ** -40, float("inf"), float("nan"), 2049.0, 2051.0, -2051.0, 3.3e38, 3.4e38, 3.3961e38, 1.00390625, 1.01171875, 1e-40, 1.2e-38]
     ''',
     [("half_from_list", "torch.tensor(EDGE, dtype=torch.half)", 0), ("bf16_from_list", "torch.tensor(EDGE, dtype=torch.bfloat16)", 0),
      ("double_half", "torch.tensor(EDGE, dtype=torch.float64).half()", 0), ("double_bf16", "torch.tensor(EDGE, dtype=torch.float64).bfloat16()", 0),
      ("float_half", "torch.tensor(EDGE).to(torch.float16)", 0), ("float_bf16", "torch.tensor(EDGE).to(dtype=torch.bfloat16)", 0),
      ("half_bf16", "torch.tensor(EDGE, dtype=torch.half).bfloat16()", 0), ("bf16_half", "torch.tensor(EDGE, dtype=torch.bfloat16).half()", 0),
      ("half_back", "[torch.tensor(EDGE, dtype=torch.half).float(), torch.tensor(EDGE, dtype=torch.half).double()]", 0),
      ("int8_from_float", "torch.tensor([200.0, -200.0, 1000.7, -1000.7, 127.9, -128.9, 0.5]).to(torch.int8)", 0),
      ("int16_from_float", "torch.tensor([40000.0, -40000.0, 32767.5, -32768.9]).to(torch.int16)", 0),
      ("int_from_half", "[torch.tensor([300.5, -129.0, 2.9], dtype=torch.half).char(), torch.tensor([300.5, -129.0, 2.9], dtype=torch.half).short(), torch.tensor([300, -129], dtype=torch.int16).to(torch.int8), torch.tensor([200, 255], dtype=torch.uint8).to(torch.int8)]", 0),
      ("int8_list", "torch.tensor([1, 2, 300], dtype=torch.int8)", 0),
      ("int8_from_tensor", "[torch.tensor(torch.tensor([300], dtype=torch.int16), dtype=torch.int8), err(lambda: torch.full((2,), 300, dtype=torch.int8), 'overflow'), err(lambda: torch.zeros(2, dtype=torch.int16).fill_(1e5), 'overflow')]", 0),
      ("view_bits", "[torch.tensor([1.5, 2.5, -0.0, 65504.0], dtype=torch.half).view(torch.int16), torch.tensor([1.5, -2.0], dtype=torch.bfloat16).view(torch.int16), torch.tensor([15872, -1024], dtype=torch.int16).view(torch.half), torch.tensor([16320], dtype=torch.int16).view(torch.bfloat16)]", 0),
      ("full_arange", "[torch.full((2,), 0.1, dtype=torch.half), torch.arange(2045, 2052, dtype=torch.half), torch.arange(0, 1, 0.3, dtype=torch.bfloat16), torch.linspace(0, 1, 4, dtype=torch.half), torch.ones(2, dtype=torch.int8) * 127]", 0),
      ("items", "[torch.tensor(0.1, dtype=torch.half).item(), torch.tensor(-3, dtype=torch.int8).item(), torch.tensor([0.1], dtype=torch.bfloat16).tolist()]", 0),
      ("setitem", "(lambda t: (t.__setitem__(0, 0.1), t.__setitem__(1, torch.tensor(70000.0)), t.fill_(t[0] * 3) if False else t)[-1])(torch.zeros(3, dtype=torch.half))", 0),
      ("copy_", "[torch.zeros(3, dtype=torch.half).copy_(torch.tensor([0.1, 1e5, 3e-8])), torch.zeros(2, dtype=torch.int8).copy_(torch.tensor([200, -300]))]", 0),
      ("where_cat", "[torch.where(torch.tensor([True, False]), torch.tensor([0.1, 0.2], dtype=torch.half), torch.tensor(0.3)), torch.cat([torch.tensor([0.1], dtype=torch.half), torch.tensor([2049])]), torch.cat([torch.tensor([1], dtype=torch.int8), torch.tensor([2], dtype=torch.uint8)])]", 0),
      ("set_default", "(lambda: (torch.set_default_dtype(torch.half), [torch.tensor([0.1]), torch.ones(2), torch.tensor([1, 2]) * 0.5, torch.get_default_dtype()], torch.set_default_dtype(torch.float32))[1])()", 0)])

test("float16_and_bfloat16_arithmetic_matches_pytorch",
     "Elementwise ops on float16/bfloat16 round each result, with PyTorch's scalar rules: add/sub/pow/remainder round a Python scalar to the format first, mul/div keep it in float, `s / x` is `reciprocal(x) * s`, `add(alpha=)` rounds alpha and forms alpha * b in float; mixed float16/bfloat16 computes in float32.",
     '''
     def pair(dt):
         a = seq(96, dt)
         b = seq(96, dt, 3.1, 89)
         return a, b
     def ops(dt):
         a, b = pair(dt)
         p = a.abs() + 0.01
         s = 0.1234567
         return [a + b, a - b, a * b, a / b, a + s, s + a, a - s, s - a, a * s, s * a, a / s, s / a, p ** s, s ** a, a * torch.tensor(s), a + torch.tensor(s),
                 torch.add(a, b, alpha=s), torch.sub(a, b, alpha=2.3), torch.remainder(a, 0.37), torch.fmod(a, 0.37), torch.maximum(a, b), torch.clamp(a, -0.3, 0.7),
                 torch.floor(a), torch.round(a), -a, a.abs(), torch.atan2(a, b), a.neg().relu(), torch.where(a > 0, a, b), a.lerp(b, 0.3), torch.addcmul(a, a, b, value=0.5)]
     def unary(dt):
         a = seq(96, dt)
         p = a.abs() + 0.01
         f = torch.nn.functional
         return [a.exp(), p.log(), a.tanh(), a.sigmoid(), p.sqrt(), p.rsqrt(), a.sin(), a.cos(), f.gelu(a), f.silu(a), a.square(), a.reciprocal(), a.erf(), p.log1p(), a.expm1(), f.softplus(a), a.exp2(), p.log2()]
     def mixed():
         h, b = seq(8, torch.half), seq(8, torch.bfloat16)
         return [h + b, h * b, h + seq(8), h * seq(8, torch.float64), h + seq(8, torch.int8, 1.0), seq(8, torch.int16, 0.5) * 0.5]
     ''',
     [("ops_half", "ops(torch.half)", 0), ("ops_bf16", "ops(torch.bfloat16)", 0), ("unary_half", "unary(torch.half)", 1e-3), ("unary_bf16", "unary(torch.bfloat16)", 8e-3),
      ("unary_half_exact", "[unary(torch.half)[i] for i in (0, 2, 3, 4, 9, 10)]", 0), ("mixed", "mixed()", 0),
      ("inplace", "(lambda a, b: [a.add_(b), a.mul_(0.3), a.sub_(b, alpha=0.5), a.div_(3), a.clamp_(-1, 1), a.addcdiv_(b, b.abs() + 1, value=0.2)][-1])(seq(24, torch.half), seq(24, torch.half, 3.1, 89))", 0),
      ("iadd_float", "(lambda a: (a.__iadd__(torch.ones(3) * 0.1), a)[1])(torch.ones(3, dtype=torch.half))", 0),
      ("compare", "[seq(8, torch.half) > 0.1, seq(8, torch.half) == seq(8, torch.half, 7.3), torch.isnan(torch.tensor([float('nan'), 1.0], dtype=torch.half)), torch.isinf(torch.tensor([1e5], dtype=torch.half))]", 0)])

test("float16_and_bfloat16_reductions_matmul_and_softmax",
     "sum/mean/cumsum/var/std/norm accumulate in higher precision and round once; prod rounds every partial product to the format, as PyTorch does; matmul, linear, softmax and log_softmax agree within one ulp (PyTorch's kernels accumulate in float in a blocked order).",
     '''
     x = seq(96, torch.half).reshape(8, 12)
     y = seq(96, torch.bfloat16).reshape(8, 12)
     w = seq(60, torch.half, 3.1, 89).reshape(12, 5)
     wb = seq(60, torch.bfloat16, 3.1, 89).reshape(12, 5)
     big = seq(4000, torch.half, 13.1, 997)
     def reds(t):
         return [t.sum(), t.sum(0), t.sum(1), t.mean(), t.mean(1), t.cumsum(1), t.max(), t.min(1), t.argmax(1), t.amax(0), t.var(1), t.std(), t.norm(), t.abs().sum(1, keepdim=True)]
     ''',
     [("reds_half", "reds(x)", 0), ("reds_bf16", "reds(y)", 0), ("prod", "[(x * 0.1 + 1).prod(1), (x * 0.1 + 1).prod(0), (y * 0.1 + 1).prod(), (x[:3, :4] + 3).prod()]", 0),
      ("big_sum", "[big.sum(), big.mean(), big.float().sum(), (big * big).sum()]", 1e-3),
      ("matmul_half", "[x @ w, w.t() @ x.t(), x[0] @ w, torch.nn.functional.linear(x, w.t(), w[:, 0].reshape(-1)[:5] if False else w[0])]", 1e-3),
      ("matmul_bf16", "[y @ wb, y[0] @ wb]", 8e-3),
      ("matmul_dtype", "[(x @ w).dtype, (y @ wb).dtype, torch.bmm(x.reshape(2, 4, 12), w.expand(2, 12, 5)).dtype]", 0),
      ("softmax", "[x.softmax(1), x.softmax(0), x.log_softmax(1), x.log_softmax(0), torch.nn.functional.cross_entropy(x, torch.arange(8) % 12)]", 1e-3),
      ("softmax_bf16", "[y.softmax(1), y.log_softmax(0)]", 8e-3),
      ("sum_dtype", "[x.sum(dtype=torch.float32), x.to(torch.int8).sum(), x.mean(dtype=torch.float64), x.sum().dtype, x.to(torch.int16).sum().dtype]", 1e-6)])

test("int8_and_int16_wrap_like_pytorch",
     "int8/int16 arithmetic wraps modulo 2**8/2**16 (uint8 already did); division gives float32, integer sums accumulate in int64, mixed with uint8 promotes to int16.",
     '''
     a = torch.tensor([100, 120, -128, 127, -7, 3], dtype=torch.int8)
     b = torch.tensor([30, 10, 1, 1, 2, -5], dtype=torch.int8)
     s = torch.tensor([30000, -32768, 200, 7], dtype=torch.int16)
     ''',
     [("int8_ops", "[a + b, a - b, a * b, -a, a.abs(), a // b, a % b, a / b, a * 2, a + 200, a ** 2, a & b, a | b, a ^ b, ~a, a << 1, a >> 1]", 0),
      ("int16_ops", "[s + s, s * 3, -s, s // 7, s.abs(), s - 1, s * torch.tensor([2], dtype=torch.int8)]", 0),
      ("int_red", "[a.sum(), a.prod(), a.cumsum(0), a.max(), a.min(), a.argmax(), a.float().mean(), s.sum(), s.cumsum(0), (a > 0).sum(), a.sort()[0], a.clamp(-5, 5), a.to(torch.float16), a.bincount if False else a.max(0)[1]]", 0),
      ("int_promote", "[(a + torch.tensor([1], dtype=torch.uint8)).dtype, (a + s[:1]).dtype, (a * 1.5).dtype, (s + True).dtype, torch.tensor([250], dtype=torch.uint8) + torch.tensor([10], dtype=torch.int8)]", 0),
      ("int_index", "[torch.arange(10)[torch.tensor([1, -1], dtype=torch.int8)], torch.arange(10)[torch.tensor([2, 3], dtype=torch.int16)], torch.zeros(3, dtype=torch.int8).index_fill(0, torch.tensor([1]), 300)]", 0),
      ("int_matmul", "[a.reshape(2, 3) @ b.reshape(3, 2), s.reshape(2, 2) @ s.reshape(2, 2)]", 0),
      ("int_errors", "[err(lambda: a.mean(), 'mean'), err(lambda: a // torch.zeros(6, dtype=torch.int8), 'ZeroDivisionError')]", 0)])

test("float16_autograd_modules_and_optimizers",
     "Gradients flow through float16/bfloat16 tensors in their dtype (and back through `.to` to a float32 leaf); Module.half()/bfloat16()/to(dtype) convert parameters and buffers; an SGD step keeps half parameters in float16.",
     '''
     def grads(dt):
         x = seq(12, dt).requires_grad_()
         y = (x * x * 0.3 + x.sin()).sum()
         y.backward()
         return [y, x.grad]
     def through_to():
         x = seq(6).requires_grad_()
         y = (x.half() * 3).float().exp().sum()
         y.backward()
         return [y, x.grad]
     def module(dt):
         torch.manual_seed(0)
         m = torch.nn.Sequential(torch.nn.Linear(12, 5), torch.nn.LayerNorm(5), torch.nn.ReLU(), torch.nn.Linear(5, 2))
         for p in m.parameters():
             with torch.no_grad():
                 p.copy_(seq(p.numel(), torch.float32, 11.0, 31).reshape(p.shape))
         m = m.half() if dt is torch.half else m.to(dt)
         x = seq(24, dt).reshape(2, 12)
         out = m(x)
         out.sum().backward()
         return [out, [p.dtype for p in m.parameters()], m[0].weight.grad]
     def sgd():
         # One plain step (torch.optim's update is p - lr * grad, which can
         # round once more than PyTorch's p.add_(grad, alpha=-lr): 1 ulp).
         p = seq(10, torch.half).requires_grad_()
         opt = torch.optim.SGD([p], lr=0.1)
         (p * p).sum().backward()
         opt.step()
         return [p, p.grad]
     def bn():
         m = torch.nn.BatchNorm1d(3).half()
         return [m.running_mean.dtype, m.weight.dtype, m.num_batches_tracked.dtype]
     ''',
     [("grads_half", "grads(torch.half)", 0), ("grads_bf16", "grads(torch.bfloat16)", 0), ("through_to", "through_to()", 1e-6),
      ("module_half", "module(torch.half)", 1e-3), ("module_bf16", "module(torch.bfloat16)", 8e-3), ("sgd", "sgd()", 1e-3), ("bn_buffers", "bn()", 0)])

test("reduced_precision_random_numbers_follow_the_cpu_stream",
     "rand draws 11 bits per float16 and 8 per bfloat16 element from PyTorch's MT19937 stream; random_() spans [0, 2**11] / [0, 2**8] for the floats and [0, max] for int8/int16; randint casts.",
     '''
     def draws(fn):
         torch.manual_seed(0)
         return fn()
     ''',
     [("rand_half", "draws(lambda: torch.rand(8, dtype=torch.half))", 0), ("rand_bf16", "draws(lambda: torch.rand(8, dtype=torch.bfloat16))", 0),
      ("random_", "[draws(lambda: torch.empty(6, dtype=d).random_()) for d in (torch.half, torch.bfloat16, torch.int8, torch.int16)]", 0),
      ("randint", "draws(lambda: torch.randint(-5, 5, (6,), dtype=torch.int8))", 0),
      ("uniform_", "draws(lambda: torch.empty(5, dtype=torch.half).uniform_(-1, 1)).dtype", 0),
      ("randn_dtype", "draws(lambda: [torch.randn(3, dtype=torch.half).dtype, torch.randn(3, dtype=torch.bfloat16).dtype])", 0)])

test("reduced_precision_tensors_print_as_pytorch",
     "repr of float16/bfloat16/int8/int16 tensors: PyTorch's formatter, with the dtype suffix.",
     "",
     [("repr", "[repr(torch.tensor([1.5, 2.25], dtype=torch.half)), repr(torch.tensor([0.1], dtype=torch.half)), repr(torch.tensor([[1e-5, 1000.]], dtype=torch.half)), repr(torch.tensor(3.25, dtype=torch.bfloat16)), repr(torch.tensor([1, -2], dtype=torch.int8)), repr(torch.tensor([300], dtype=torch.int16)), repr(torch.ones(2, dtype=torch.half, requires_grad=True)), repr(torch.zeros(0, dtype=torch.bfloat16))]", 0)])

test("checkpoints_carry_half_bfloat16_char_and_short_storages",
     "torch.load reads PyTorch's HalfStorage/BFloat16Storage/CharStorage/ShortStorage records (2 or 1 bytes per element, bf16 as the upper float32 half); torch.save writes the same bytes and class names, and a Zipp checkpoint loads back.",
     '''
     def loaded():
         d = torch.load("dtypes.pt")
         return [[k, d[k]] for k in sorted(d)]
     def raw(t):
         # The storage record's bytes torch.save writes for `t`.
         import zipfile
         torch.save(t, "one.pt")
         with zipfile.ZipFile("one.pt") as z:
             return list(z.read([n for n in z.namelist() if n.endswith("/data/0")][0]))
     def roundtrip():
         d = {"h": torch.tensor([1.5, -2.0, 65504.0, 6e-8, float("inf")], dtype=torch.half), "b": torch.tensor([1.5, -3.4e38], dtype=torch.bfloat16),
              "c": torch.tensor([-3, 4, 127], dtype=torch.int8), "s": torch.tensor([-300, 32767], dtype=torch.int16), "dt": [torch.half, torch.bfloat16, torch.int8, torch.int16],
              "p": torch.nn.Parameter(torch.ones(2, dtype=torch.half))}
         torch.save(d, "roundtrip.pt")
         back = torch.load("roundtrip.pt")
         return [[k, back[k]] for k in sorted(back)] + [type(back["p"]).__name__]
     def names():
         import zipfile
         torch.save({"h": torch.ones(1, dtype=torch.half), "b": torch.ones(1, dtype=torch.bfloat16), "c": torch.ones(1, dtype=torch.int8), "s": torch.ones(1, dtype=torch.int16)}, "names.pt")
         with zipfile.ZipFile("names.pt") as z:
             pkl = [z.read(n) for n in z.namelist() if n.endswith("data.pkl")][0]
         return [w in pkl for w in (b"HalfStorage", b"BFloat16Storage", b"CharStorage", b"ShortStorage")]
     ''',
     [("load_pytorch", "loaded()", 0),
      ("bytes", "[raw(torch.tensor([1.5, -2.0, 65504.0, 6e-8, float('nan')], dtype=torch.half)), raw(torch.tensor([1.5, -3.4e38, float('nan')], dtype=torch.bfloat16)), raw(torch.tensor([-3, 4], dtype=torch.int8)), raw(torch.tensor([-300, 32767], dtype=torch.int16))]", 0),
      ("roundtrip", "roundtrip()", 0), ("names", "names()", 0)])

SPECIAL_X = [-50.5, -10.7, -3.3, -2.5, -1.5, -0.9, -0.3, -1e-3, 0.0, 1e-6, 0.01, 0.3, 0.5, 0.99, 1.0, 1.5, 2.0, 2.5, 3.5, 7.5, 8.5, 10.0, 12.0, 30.0, 100.0, 1e4]
UNARY_SPECIAL = ["lgamma", "digamma", "erfcx", "i0", "i0e", "i1", "i1e", "ndtr", "log_ndtr", "entr", "sinc", "expit", "gammaln", "psi"]

test("special_functions_match_pytorch",
     "torch.special and the top-level special functions: values within 1e-9 (float64) and 1e-6 (float32) of PyTorch, PyTorch's edge values (poles, infinities, NaN), integer inputs promoted to float, and the error cases.",
     '''
     X = %r
     x64 = torch.tensor(X, dtype=torch.float64)
     x32 = torch.tensor(X)
     P = [0.0, 1e-300, 1e-20, 1e-5, 0.01, 0.1, 0.13, 0.14, 0.3, 0.5, 0.86, 0.87, 0.99, 0.99999, 1.0, 1.5, -0.1]
     A = [0.1, 0.5, 1.0, 2.5, 5.0, 30.0, 100.0, 1000.0, 0.0, -1.0, float("inf")]
     Y = [0.01, 0.5, 1.0, 4.9, 5.1, 28.0, 33.0, 95.0, 105.0, 0.0, float("inf")]
     aa = torch.tensor([a for a in A for y in Y], dtype=torch.float64)
     yy = torch.tensor([y for a in A for y in Y], dtype=torch.float64)
     S = torch.special
     def uni(dt):
         t = x64 if dt is torch.float64 else x32
         return [getattr(S, n)(t) for n in %r]
     def edges():
         e = torch.tensor([0.0, -0.0, -1.0, -2.0, float("inf"), -float("inf"), float("nan")], dtype=torch.float64)
         return [torch.lgamma(e), torch.digamma(e), torch.polygamma(1, e), torch.polygamma(2, e), torch.polygamma(3, e), S.erfcx(e), S.i0(e), S.i1e(e), S.ndtr(e), S.log_ndtr(e), S.entr(e), S.sinc(e), S.logit(e)]
     ''' % (SPECIAL_X, UNARY_SPECIAL),
     [("unary_f64", "uni(torch.float64)", 1e-9), ("unary_f32", "uni(torch.float32)", 1e-6),
      ("polygamma", "[torch.polygamma(n, x64) for n in (0, 1, 2, 3, 5)]", 1e-9), ("polygamma_f32", "[x32.polygamma(n) for n in (1, 2)]", 1e-5),
      ("ndtri", "S.ndtri(torch.tensor(P, dtype=torch.float64))", 1e-9), ("ndtri_f32", "S.ndtri(torch.tensor(P))", 1e-6),
      ("logit", "[S.logit(torch.tensor(P, dtype=torch.float64)), S.logit(torch.tensor(P)), S.logit(torch.tensor(P), eps=1e-6), torch.logit(torch.tensor(P, dtype=torch.float64), 0.25)]", 1e-9),
      ("gammainc", "[S.gammainc(aa, yy), S.gammaincc(aa, yy), aa.igammac(yy)]", 1e-9), ("gammainc_f32", "torch.igamma(aa.float(), yy.float())", 1e-5),
      ("xlogy", "[torch.xlogy(torch.tensor([0., 0., 0., 2., 2., -1.]), torch.tensor([0., float('nan'), -1., 0., 3., 5.])), S.xlog1py(torch.tensor([0., 0., 0., 2., 2.]), torch.tensor([-1., float('nan'), -2., 0., 3.])), torch.xlogy(2, torch.tensor([3.])), torch.xlogy(torch.tensor([3.]), 2.0), S.xlogy(torch.tensor([1, 2]), 3)]", 1e-6),
      ("zeta", "[S.zeta(torch.tensor([2., 3., 1., 0.5, 4.5, 7.0], dtype=torch.float64), torch.tensor([1., 2., 1., 1., 0.5, 10.0], dtype=torch.float64)), S.zeta(torch.tensor([2.0]), 3.0)]", 1e-9),
      ("mvlgamma", "[torch.mvlgamma(torch.tensor([2.5, 3.0], dtype=torch.float64), 3), S.multigammaln(torch.tensor([2.5]), 2), torch.tensor([4.0]).mvlgamma(4)]", 1e-6),
      ("edges", "edges()", 1e-9),
      ("int_inputs", "[torch.lgamma(torch.tensor([1, 2, 5])), S.i0(torch.tensor([1, 2])), S.gammainc(torch.tensor([1., 2.]), torch.tensor([1, 2])), torch.sinc(torch.tensor([0, 1]))]", 1e-6),
      ("half_inputs", "[torch.lgamma(seq(8, torch.half).abs() + 0.5), S.ndtr(seq(8, torch.bfloat16)), torch.digamma(seq(8, torch.half).abs() + 0.5)]", 0),
      ("methods", "[x64.lgamma(), x64.digamma(), x64.i0(), x64.sinc(), x64.abs().logit(0.1), x64.xlogy(2.0), (x64.abs() + 1).mvlgamma(2), x64.clone().lgamma_(), x64.clone().polygamma_(1)]", 1e-9),
      ("errors", "[err(lambda: torch.mvlgamma(torch.tensor([0.5, 3.]), 2), 'All elements must be greater than (p-1)/2'), err(lambda: torch.polygamma(-1, x64), 'negative'), (lambda a: err(lambda: torch.autograd.grad(S.gammainc(a, torch.tensor([1.5])).sum(), [a]), 'igamma: input'))(torch.tensor([2.5], requires_grad=True))]", 0),
      ("names", "[hasattr(S, n) for n in ('gammaln', 'psi', 'expit', 'erfcx', 'i0e', 'i1', 'i1e', 'ndtr', 'ndtri', 'log_ndtr', 'entr', 'xlog1py', 'zeta', 'multigammaln', 'polygamma', 'digamma', 'softmax', 'log_softmax', 'logsumexp', 'round', 'exp2', 'expm1', 'log1p', 'erf', 'erfc', 'erfinv', 'gammainc', 'gammaincc', 'logit', 'sinc', 'xlogy', 'i0')]", 0)])

GRAD_FNS = ["lgamma", "digamma", "erfcx", "i0", "i0e", "i1", "i1e", "ndtr", "log_ndtr", "sinc", "entr", "expit"]

test("special_function_gradients_match_pytorch",
     "The gradients of the special functions (first and, where PyTorch's backward is differentiable, second order) and their grad_fn names.",
     '''
     def g1(name, vals=(0.3, 1.7, -0.4, 2.5, 7.0)):
         x = torch.tensor(vals, dtype=torch.float64, requires_grad=True)
         f = getattr(torch.special, name)
         (g,) = torch.autograd.grad(f(x).sum(), [x])
         return g
     def g2(name, vals=(0.3, 1.7, 2.5, 7.0)):
         x = torch.tensor(vals, dtype=torch.float64, requires_grad=True)
         f = getattr(torch.special, name)
         (g,) = torch.autograd.grad(f(x).sum(), [x], create_graph=True)
         (h,) = torch.autograd.grad(g.sum(), [x])
         return [g, h]
     def binary(fn, a, b):
         x = torch.tensor(a, dtype=torch.float64, requires_grad=True)
         y = torch.tensor(b, dtype=torch.float64, requires_grad=True)
         return list(torch.autograd.grad(fn(x, y).sum(), [x, y]))
     def only_second(fn, a, b):
         x = torch.tensor(a, dtype=torch.float64)
         y = torch.tensor(b, dtype=torch.float64, requires_grad=True)
         return torch.autograd.grad(fn(x, y).sum(), [y])[0]
     def other():
         out = []
         x = torch.tensor([0.3, 1.7, 2.5], dtype=torch.float64, requires_grad=True)
         out.append(torch.autograd.grad(torch.polygamma(2, x).sum(), [x])[0])
         out.append(torch.autograd.grad(torch.special.ndtri(torch.tensor([0.1, 0.5, 0.97], dtype=torch.float64, requires_grad=True)).sum(), [])[0] if False else None)
         p = torch.tensor([0.1, 0.5, 0.97], dtype=torch.float64, requires_grad=True)
         out.append(torch.autograd.grad(torch.special.ndtri(p).sum(), [p])[0])
         q = torch.tensor([0.1, 0.5, 0.97, 1e-8, 1.5], dtype=torch.float64, requires_grad=True)
         out.append(torch.autograd.grad(torch.special.logit(q).sum(), [q])[0])
         out.append(torch.autograd.grad(torch.special.logit(q, eps=0.05).sum(), [q])[0])
         m = torch.tensor([2.5, 3.0], dtype=torch.float64, requires_grad=True)
         out.append(torch.autograd.grad(torch.mvlgamma(m, 3).sum(), [m])[0])
         z = torch.tensor([0.0, 0.5], dtype=torch.float64, requires_grad=True)
         out.append(torch.autograd.grad(torch.special.i1(z).sum(), [z])[0])
         out.append(torch.autograd.grad(torch.special.i1e(z).sum(), [z])[0])
         out.append(torch.autograd.grad(torch.special.sinc(z).sum(), [z])[0])
         return out
     def names():
         x = torch.tensor([0.5], requires_grad=True)
         S = torch.special
         fs = [torch.lgamma(x), torch.digamma(x), torch.polygamma(1, x), S.erfcx(x), S.i0(x), S.i0e(x), S.i1(x), S.i1e(x), S.ndtri(x), S.log_ndtr(x), S.entr(x), S.sinc(x), S.logit(x), torch.xlogy(x, x), S.xlog1py(x, x), S.gammainc(x, x), S.gammaincc(x, x), S.zeta(x + 1, x)]
         return [type(f.grad_fn).__name__ for f in fs]
     ''',
     [("first", "[g1(n) for n in %r]" % GRAD_FNS, 1e-9),
      ("second", "[g2(n) for n in ('lgamma', 'digamma', 'erfcx', 'i0', 'ndtr', 'log_ndtr', 'expit')]", 1e-8),
      ("binary", "[binary(torch.xlogy, [0.0, 2.0, 3.0, 0.0, 0.0, 0.0, 0.0], [1.0, 0.5, 4.0, 2.0, 0.0, -1.0, float('nan')]), binary(torch.special.xlog1py, [0.0, 2.0, 0.0, 0.0, 0.0, 0.0], [1.0, 0.5, 2.0, -1.0, -2.0, float('nan')])]", 1e-9),
      ("gammainc_x", "[only_second(torch.special.gammainc, [2.5, 0.5, 30.0], [1.5, 2.0, 28.0]), only_second(torch.special.gammaincc, [2.5, 0.5], [1.5, 2.0]), only_second(torch.special.zeta, [2.0, 3.5], [1.5, 2.0])]", 1e-9),
      ("other", "other()", 1e-9), ("grad_fn_names", "names()", 0)])

test("torch_exposes_its_submodules_after_import",
     "After `import torch`, torch.special, torch.linalg, torch.distributions and torch.amp are attributes (the last three import on first use).",
     "",
     [("special", "[torch.special.expit(torch.tensor([0.0])), torch.special.gammaln(torch.tensor([3.0]))]", 1e-6),
      ("present", "[hasattr(torch, n) for n in ('special', 'linalg', 'distributions', 'amp')]", 0),
      ("nuc_norm", "[torch.norm(torch.arange(6.).reshape(2, 3), p='nuc'), torch.arange(12.).reshape(2, 2, 3).norm(p='nuc', dim=(1, 2), keepdim=True)]", 1e-5),
      ("import_forms", "(lambda: (__import__('torch.linalg'), __import__('torch.distributions'), __import__('torch.amp'), [torch.linalg.__name__, torch.distributions.__name__, torch.amp.__name__]))()[-1]", 0)])


def build(target):
    import os
    import shutil
    import tempfile
    fixtures = os.path.join(os.path.dirname(os.path.abspath(target)), "fixtures")
    work = tempfile.mkdtemp()
    shutil.copy(os.path.join(fixtures, "torch_dtypes", "dtypes.pt"), work)
    os.chdir(work)
    rust = ['''//! The float16/bfloat16/int8/int16 dtypes and the special functions of the
//! bundled `torch` subset against PyTorch 2.11: promotion, rounding to the
//! format on every result, PyTorch's scalar rules, reductions, autograd,
//! modules, random streams, printing, checkpoints and torch.special. Every
//! expected value was computed by CPython 3.11 with PyTorch 2.11 and written
//! here as a literal by `fixtures/torch_dtypes/gen_tests.py` (edit the cases
//! there and rerun it); each Python program checks Zipp's result against it
//! and prints `label True`.
#![cfg(feature = "python")]
use zipp_vm::frontend::compile_python_program;

const NORM: &str = r#"''' + NORM.strip("\n") + '''
"#;

fn run_program(source: String) -> Vec<String> {
    let files = vec![(
        "dtypes.pt".to_owned(),
        include_bytes!("fixtures/torch_dtypes/dtypes.pt").to_vec(),
    )];
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(move || {
            let modules = vec![("main".to_owned(), source)];
            let mut compiled = compile_python_program("main", &modules, &files, &[], false)?;
            let state = compiled.state_mut();
            state.set_limits(4_000_000_000, None);
            state.run_init()?;
            Ok::<_, String>(state.take_output())
        })
        .expect("spawn")
        .join()
        .expect("test thread")
        .unwrap()
}

fn run(body: &str) -> Vec<String> {
    run_program(format!("{NORM}\\n{body}"))
}

fn assert_all_true(out: &[String], count: usize) {
    let failed: Vec<&String> = out.iter().filter(|line| !line.ends_with(" True")).collect();
    assert!(failed.is_empty(), "{failed:#?}");
    assert_eq!(out.len(), count, "{out:#?}");
}
''']
    for name, doc, setup, cases in TESTS:
        ns = {}
        exec(NORM, ns)
        exec(setup, ns)
        lines = []
        for label, expr, tol in cases:
            try:
                want = ns["norm"](eval(expr, ns))
            except Exception as e:
                want = ["E", type(e).__name__]
            lines.append("check(%r, lambda: %s, %s, %r)" % (label, expr, lit(want), tol))
        body = (setup + "\n" if setup else "") + "\n".join(lines) + "\n"
        assert '"#' not in body, name
        rust.append('''
/// %s
#[test]
fn %s() {
    let out = run(r#"
%s"#);
    assert_all_true(&out, %d);
}
''' % (doc, name, body, len(cases)))
    rust.append(NATIVE_TEST)
    open(target, "w", encoding="utf-8", newline="\n").write("".join(rust))


# Zipp-only: the native loops (`vm::py_tensor`) and the JavaScript loops give
# the same bytes for the new dtypes (the rounding to float16/bfloat16 runs
# after either).
NATIVE_TEST = r'''
/// The native loops and the JavaScript loops give the same bytes for float16, bfloat16, int8 and int16 storages.
#[test]
fn native_and_javascript_loops_agree_on_the_new_dtypes() {
    let out = run_program(
        r#"
import _zipp_tensor as _k

def values(n, seed, dtype):
    out = []
    for i in range(n):
        v = ((i * 37 + seed * 11) % 23 - 11) / 4.0 + (seed % 5) * 0.137
        if i % 17 == 3:
            v = [70000.0, -1e-7, 3e-8, float("inf"), float("nan"), 0.1][i % 6]
        if dtype in ("int8", "int16"):
            v = int(v * 40) if v == v and abs(v) < 1e9 else i
        out.append(v)
    return out

BAD = []
CASES = [0]

def both(label, fn):
    CASES[0] += 1
    got = []
    for on in (False, True):
        _k._native(on)
        r = fn()
        if isinstance(r, tuple):
            r = r[0]
        got.append((_k.dtype(r), _k.tobytes(r)))
    if got[0] != got[1]:
        BAD.append(label)

for dt in ["float16", "bfloat16", "int8", "int16"]:
    A = _k.from_flat(dt, values(96, 1, dt))
    B = _k.from_flat(dt, values(96, 2, dt))
    F = _k.from_flat("float32", values(96, 3, "float32"))
    for op in ["add", "sub", "mul", "div", "max", "pow", "atan2"]:
        both("binary %s %s" % (op, dt), lambda: _k.binary(op, A, (96,), B, (96,)))
        both("binary %s %s f32" % (op, dt), lambda: _k.binary(op, A, (8, 12), F, (8, 12), dt))
    for op in ["exp", "tanh", "sigmoid", "gelu", "neg", "abs", "sqrt", "log1p", "lgamma", "ndtr"]:
        both("unary %s %s" % (op, dt), lambda: _k.unary(op, A))
    for op in ["sum", "mean", "prod", "max", "argmin"]:
        both("reduce %s %s" % (op, dt), lambda: _k.reduce(op, A, (8, 12), (1,), False, True))
    both("matmul %s" % dt, lambda: _k.matmul(A, (8, 12), B, (12, 8)))
    both("softmax %s" % dt, lambda: _k.softmax(A, (8, 12), 1, False))
    both("permute %s" % dt, lambda: _k.permute(A, (8, 12), (1, 0)))
    both("where %s" % dt, lambda: _k.where(_k.from_flat("bool", [i % 3 for i in range(96)]), (96,), A, (96,), F, (96,), dt))
print("cases", CASES[0], "mismatches", len(BAD), BAD[:5])
"#
        .to_owned(),
    );
    assert_eq!(out.len(), 1, "{out:#?}");
    assert!(out[0].contains("mismatches 0 []"), "{}", out[0]);
}
'''


def make_fixture(path):
    """A checkpoint with every new storage type, saved by PyTorch."""
    torch.save({"h": torch.tensor([1.5, -2.0, 65504.0, 6e-8, float("inf"), 0.1], dtype=torch.half),
                "b": torch.tensor([[1.5, -3.4e38], [0.1, 1e-40]], dtype=torch.bfloat16),
                "c": torch.tensor([-3, 4, 127, -128], dtype=torch.int8),
                "s": torch.tensor([-300, 32767], dtype=torch.int16),
                "t": torch.arange(12, dtype=torch.half).reshape(3, 4).t(),
                "dtypes": [torch.half, torch.bfloat16, torch.int8, torch.int16],
                "p": torch.nn.Parameter(torch.tensor([0.5, 0.25], dtype=torch.bfloat16))}, path)


if __name__ == "__main__":
    import os
    here = os.path.dirname(os.path.abspath(__file__))
    if "--fixtures" in sys.argv:
        sys.argv.remove("--fixtures")
        make_fixture(os.path.join(here, "dtypes.pt"))
    build(sys.argv[1] if len(sys.argv) > 1 else os.path.join(here, "..", "..", "python_torch_dtypes.rs"))
