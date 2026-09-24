# Regenerates the expected text from a CPU-only PyTorch 2.11 build (Linux,
# e.g. in WSL: a CUDA build without NCCL refuses the default backend) run
# from this directory; it opens local
# gloo process groups on ports 29562 and up):
#     python gen.py
# It writes text_expected.txt from dist_cases.py's text().
import os
import sys
import tempfile

sys.dont_write_bytecode = True
HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import dist_cases  # noqa: E402

with tempfile.TemporaryDirectory() as tmp:
    lines = dist_cases.text(os.path.join(tmp, "pg_store").replace("\\", "/"))
with open(os.path.join(HERE, "text_expected.txt"), "w", newline="\n", encoding="utf-8") as f:
    for line in lines:
        f.write(line + "\n")
