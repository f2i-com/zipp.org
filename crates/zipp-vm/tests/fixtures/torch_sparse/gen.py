# Regenerates the expected values from CPU PyTorch 2.11 (run with the
# CPython that has PyTorch installed, from this directory):
#     python gen.py
# It writes sparse_expected.json (results()) and text_expected.txt
# (text()) from sparse_cases.py, and sparse.pt (gen_checkpoint()).
import json
import os
import sys

sys.dont_write_bytecode = True
HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import sparse_cases  # noqa: E402
import torch  # noqa: E402

with open(os.path.join(HERE, "sparse_expected.json"), "w", newline="\n") as f:
    json.dump(sparse_cases.results(), f, indent=0, sort_keys=True)
    f.write("\n")
with open(os.path.join(HERE, "text_expected.txt"), "w", newline="\n", encoding="utf-8") as f:
    for line in sparse_cases.text():
        f.write(line + "\n")
# A checkpoint PyTorch writes: an uncoalesced COO tensor, a coalesced
# hybrid float64 one, a CSR and a CSC matrix (their compressed and plain
# indices share one storage) and a dense tensor.
s = torch.sparse_coo_tensor(torch.tensor([[0, 1, 1, 0], [2, 0, 2, 2]]), torch.tensor([3.0, 4.0, 5.0, 6.0]), (2, 3))
h = torch.sparse_coo_tensor(torch.tensor([[0, 2]]), torch.tensor([[1.0, 2.0], [3.0, 4.0]], dtype=torch.float64), (3, 2)).coalesce()
x = torch.tensor([[1.0, 0.0, 2.0], [0.0, 0.0, 3.0]])
torch.save({"s": s, "h": h, "c": x.to_sparse_csr(), "d": x.to_sparse_csc(), "x": torch.arange(3.0)}, os.path.join(HERE, "sparse.pt"))
