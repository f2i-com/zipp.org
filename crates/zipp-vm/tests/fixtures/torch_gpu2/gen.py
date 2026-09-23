"""Regenerate masks_expected.json with CPU PyTorch (2.11 was used):

    python crates/zipp-vm/tests/fixtures/torch_gpu2/gen.py

Each case of masks.py runs its STEPS eager steps and records one state()
vector per step (loss, gradients, weights, optimizer buffers)."""
import json
from pathlib import Path

HERE = Path(__file__).resolve().parent
SOURCE = (HERE / "masks.py").read_text(encoding="utf-8")
CASES = ["masks", "where", "clamp", "activations", "maxmin", "edges", "tensorclamp", "logical", "evaldropout"]

expected = {}
for case in CASES:
    scope = {"case": case}
    exec(compile(SOURCE, "masks.py", "exec"), scope)
    expected[case] = [scope["state"](scope["train_step"](x, y)) for x, y in scope["batches"]]
(HERE / "masks_expected.json").write_text(json.dumps(expected, separators=(",", ":")) + "\n", encoding="utf-8", newline="\n")
print("wrote", len(expected), "cases")
