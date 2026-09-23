# Regenerates the expected values from CPU PyTorch (run with the CPython
# that has PyTorch 2.11 installed, from this directory):
#     python gen_expected.py
# It writes optim_expected.json and sched_expected.json, and the PyTorch-side
# checkpoints the Zipp tests load (see make_checkpoints below).
import json
import os
import pickle
import sys
from collections import OrderedDict

import torch

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import optim_cases  # noqa: E402
import sched_cases  # noqa: E402


def write_json(name, value):
    with open(os.path.join(HERE, name), "w") as f:
        json.dump(value, f, indent=0, sort_keys=True)
        f.write("\n")


def make_checkpoints():
    # An Adam checkpoint after three steps (PyTorch keeps `step` as a float
    # tensor), and the trajectory the next three steps take from it.
    torch.manual_seed(0)
    w = torch.tensor([1.0, -2.0, 0.5], requires_grad=True)
    b = torch.tensor([0.25, -0.75], requires_grad=True)
    opt = torch.optim.Adam([w, b], lr=0.1, weight_decay=0.05)
    for _ in range(3):
        opt.zero_grad()
        optim_cases._loss(w, b).backward()
        opt.step()
    torch.save({"w": w.detach(), "b": b.detach(), "opt": opt.state_dict()}, os.path.join(HERE, "adam_resume.pt"))
    after = []
    for _ in range(3):
        opt.zero_grad()
        optim_cases._loss(w, b).backward()
        opt.step()
        after.extend(w.detach().tolist() + b.detach().tolist())
    # A Linear model's Adam checkpoint after one step, for a compiled step.
    torch.manual_seed(0)
    model = torch.nn.Linear(2, 1)
    lin_opt = torch.optim.Adam(model.parameters(), lr=0.01)
    ((model(torch.tensor([[1.0, 2.0]])) - 1.0) ** 2).mean().backward()
    lin_opt.step()
    torch.save({"model": model.state_dict(), "opt": lin_opt.state_dict()}, os.path.join(HERE, "adam_linear.pt"))
    # Plain data PyTorch writes through protocol 2's globals: bytes
    # (_codecs.encode), a set (builtins.set), a torch.Size and a torch.device.
    torch.save({"tok": b"\x00\xff\x80abc", "empty": b"", "s": {3, 1, 2}, "od": OrderedDict([("z", 1), ("a", 2)]),
                "bytearray": bytearray(b"xy")}, os.path.join(HERE, "plain_data.pt"))
    torch.save({"shape": torch.Size([2, 3]), "dev": torch.device("cpu"), "param": torch.nn.Parameter(torch.tensor([1.5, -2.0]))},
               os.path.join(HERE, "torch_globals.pt"))
    # CPython's own pickles of plain data at every protocol.
    data = {"fs": frozenset([1, 2]), "s": {3}, "b": b"\x00\xff", "ba": bytearray(b"ab"), "f": 1.5, "neg": -7, "big": 2 ** 70,
            "t": (1, "two", 3.0), "l": [None, True, False], "u": "café ☃", "nested": {"k": [1, {"x": b""}]}}
    for proto in range(6):
        with open(os.path.join(HERE, "plain_p%d.pkl" % proto), "wb") as f:
            f.write(pickle.dumps(data, protocol=proto))
    return after


def main():
    write_json("optim_expected.json", optim_cases.results())
    write_json("sched_expected.json", sched_cases.results())
    write_json("resume_expected.json", make_checkpoints())
    print("wrote expectations with torch", torch.__version__)


if __name__ == "__main__":
    main()
