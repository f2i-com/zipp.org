# Graph IR v2 operations from the bundled zipp_gpu builder, evaluated by its
# float32 reference (no GPU host in the CLI or CPython). Transcendental results
# are printed as scaled integers so libm last-bit differences cannot show.
from zipp_gpu import Graph, GraphError, ComputeError, execute_locally


def q(values):
    return [round(v * 100000) for v in values]


def show(result):
    for name in sorted(result["outputs"]):
        out = result["outputs"][name]
        print(name, out["shape"], out["data"])


g = Graph()
col = g.tensor([[1], [2], [3]])
row = g.tensor([10, 20])
cube = g.tensor(list(range(24)), shape=(2, 3, 4))
print((col + row).shape, (col / 2).shape, (cube + g.tensor([1, 2, 3, 4])).shape, (cube * g.tensor([[1], [2], [3]])).shape)
g.submit(show, grid=col * row - row, ratio=row / col, flip=1 / row, bias=cube - g.tensor([1, 2, 3, 4]))

u = Graph()
x = u.tensor([-2.5, -1, 0.5, 3])
res = execute_locally(u.program(exp=x.exp(), tanh=x.tanh(), sig=x.sigmoid(), gelu=x.gelu(), gg=x.gelu_grad(),
                                relu=x.relu(), sq=(x * x).sqrt(), lg=(x * x + 1).log(), neg=-x))
for name in ["exp", "tanh", "sig", "gelu", "gg", "lg"]:
    print(name, q(res["outputs"][name]["data"]))
print("relu", res["outputs"]["relu"]["data"], "sqrt", res["outputs"]["sq"]["data"], "neg", res["outputs"]["neg"]["data"])

r = Graph()
m = r.tensor([[1, 2, 3], [4, 5, 6]])
print(m.sum().shape, m.sum(0).shape, m.sum(-1, keepdim=True).shape, m.mean(keepdim=True).shape, m.T.shape, m.reshape(3, -1).shape)
res = execute_locally(r.program(total=m.sum(), cols=m.sum(0), rows=m.mean(1), keep=m.sum(axis=-1, keepdim=True),
                                sm=m.softmax(), ls=m.log_softmax(), perm=m.permute(1, 0), flat=m.reshape(6)))
for name in ["total", "cols", "rows", "keep", "perm", "flat"]:
    print(name, res["outputs"][name]["shape"], res["outputs"][name]["data"])
print("softmax", q(res["outputs"]["sm"]["data"]), "log_softmax", q(res["outputs"]["ls"]["data"]))

b = Graph()
a3 = b.tensor(list(range(12)), shape=(2, 2, 3))
w = b.tensor([[1, 0], [0, 1], [1, 1]])
b3 = b.tensor(list(range(12)), shape=(2, 3, 2))
t = b.tensor(list(range(24)), shape=(2, 3, 4)).permute(2, 0, 1)
b.submit(show, bw=a3 @ w, bb=a3 @ b3, swapped=a3.transpose(1, 2), perm=t.sum(0))

c = Graph()
logits = c.tensor([[0, 0, 0], [1, 2, 3], [3, -1, 0.5]])
res = execute_locally(c.program(loss=logits.cross_entropy([2, 0, 0]), grad=logits.cross_entropy_grad([2, 0, 0])))
print("ce", q(res["outputs"]["loss"]["data"]), q(res["outputs"]["grad"]["data"]))

o = Graph()
p = o.tensor([1.0, -2.0, 0.5])
grad = o.tensor([0.5, 0.25, -1.0])
p1, m1, v1 = o.adam(p, grad, o.zeros((3,)), o.zeros((3,)), lr=0.1, step=1)
p2, m2, v2 = o.adam(p1, grad, m1, v1, lr=0.1, step=2)
buf = o.momentum_update(o.tensor([1.0, 1.0, 1.0]), grad, 0.9, dampening=0.1)
res = execute_locally(o.program(p1=p1, p2=p2, m2=m2, v2=v2, buf=buf, sgd=o.sgd_update(p, buf, 0.5)))
for name in ["p1", "p2", "m2", "v2", "buf", "sgd"]:
    print(name, res["outputs"][name]["data"])

for bad in [lambda: g.tensor([1, 2]) + g.tensor([1, 2, 3]), lambda: m.sum(2), lambda: m.softmax(0),
            lambda: m.permute(0, 0), lambda: m.reshape(4, 2), lambda: logits.cross_entropy([0, 3, 1]),
            lambda: logits.cross_entropy([0.5, 1, 1]), lambda: a3 @ b.tensor(list(range(18)), shape=(3, 3, 2)),
            lambda: o.adam(p, grad, p, p, betas=(1.0, 0.5)), lambda: o.sgd_update(p, o.zeros((2,)), 0.1),
            lambda: g.zeros((1, 1, 1, 1, 1)), lambda: m.transpose(0)]:
    try:
        bad()
        print("no error")
    except GraphError as e:
        print("GraphError", e)
nan = Graph()
big = nan.tensor([3e38, 1.0]) * 10
for op in ["relu", "tanh", "sum"]:
    try:
        execute_locally(nan.program(r=getattr(big - big, op)()))
        print(op, "finite")
    except ComputeError as e:
        print(op, e.code)
print(execute_locally(nan.program(mask=(big - big).positive()))["outputs"]["mask"]["data"])
