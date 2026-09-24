# Regenerates the expected values from CPU PyTorch 2.11 (run from this
# directory with a PyTorch whose quantized engines include x86 — a Linux or
# macOS x86 build; the Windows wheels ship only onednn — e.g. in WSL):
#     python gen.py
# It writes quant_expected.json (results() exactly, tol_results() compared
# with a tolerance), text_expected.txt (text()) and quant.pt
# (gen_checkpoint()) from quant_cases.py, and ckpt_expected.txt
# (ckpt_text(), what loading quant.pt and a save/load round trip show).
import json
import os
import sys

sys.dont_write_bytecode = True
HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import torch  # noqa: E402

if "x86" not in torch.backends.quantized.supported_engines:
    raise SystemExit("gen.py needs the x86 quantized engine (supported here: %s)" % torch.backends.quantized.supported_engines)
torch.backends.quantized.engine = "x86"
import quant_cases  # noqa: E402

expected = quant_cases.results()
expected.update({"tol_" + k: v for k, v in quant_cases.tol_results().items()})
with open(os.path.join(HERE, "quant_expected.json"), "w", newline="\n") as f:
    json.dump(expected, f, indent=0, sort_keys=True)
    f.write("\n")
with open(os.path.join(HERE, "text_expected.txt"), "w", newline="\n", encoding="utf-8") as f:
    for line in quant_cases.text():
        f.write(line + "\n")
quant_cases.gen_checkpoint(os.path.join(HERE, "quant.pt"))
with open(os.path.join(HERE, "ckpt_expected.txt"), "w", newline="\n", encoding="utf-8") as f:
    import tempfile
    with tempfile.TemporaryDirectory() as tmp:
        for line in quant_cases.ckpt_text(os.path.join(HERE, "quant.pt"), os.path.join(tmp, "rt.pt")):
            f.write(line + "\n")
