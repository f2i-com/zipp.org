# torch-level aliases (grid_sampler, affine_grid_generator, ctc_loss), RNNs under
# CPU autocast and torch.* on uninitialized parameters: runs under PyTorch 2.11
# (python followups.py > followups_expected.txt) and Zipp (python_torch_complex.rs).
import math
import torch
import torch.nn as nn
import torch.nn.functional as F
x = torch.arange(16.).reshape(1, 1, 4, 4)
th = torch.tensor([[[1., 0.2, 0.1], [-0.3, 0.9, 0.05]]])
g = torch.affine_grid_generator(th, [1, 1, 3, 3], False)
print([round(v, 5) for v in g.flatten().tolist()])
print([round(v, 5) for v in torch.affine_grid_generator(th, [1, 1, 2, 3], True).flatten().tolist()])
for mode in (0, 1, 2):
    for pad in (0, 1, 2):
        for ac in (False, True):
            print(mode, pad, ac, [round(v, 3) for v in torch.grid_sampler(x, g * 1.3, mode, pad, ac).flatten().tolist()])
lp = torch.tensor([[math.sin(0.7 * i + 1.3 * j) for j in range(8)] for i in range(5)]).reshape(5, 2, 4).log_softmax(-1)
t = torch.tensor([[1, 2], [3, 3]]); il = torch.tensor([5, 4]); tl = torch.tensor([2, 1])
print([round(v, 5) for v in torch.ctc_loss(lp, t, il, tl).tolist()], round(torch.ctc_loss(lp, t, il, tl, 0, 1, False).item(), 5), round(torch.ctc_loss(lp, t, il, tl, 1, 2, True).item(), 5))
for cls in (nn.LSTM, nn.GRU, nn.RNN):
    m = cls(4, 3)
    with torch.autocast("cpu", dtype=torch.bfloat16):
        o, h = m(torch.ones(2, 1, 4))
    print(cls.__name__, o.dtype, (h[0].dtype, h[1].dtype) if isinstance(h, tuple) else h.dtype)
pz = nn.parameter.UninitializedParameter()
for f in [lambda: torch.add(pz, 1), lambda: torch.matmul(pz, pz), lambda: pz.shape, lambda: F.linear(torch.ones(2), pz), lambda: torch.exp(pz), lambda: torch.sum(pz), lambda: torch.cat([pz, pz])]:
    try:
        f(); print("ok")
    except Exception as e:
        print(type(e).__name__, str(e)[:34])
