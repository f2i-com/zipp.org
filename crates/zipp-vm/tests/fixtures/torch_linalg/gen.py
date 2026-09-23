# Regenerates the expected values from CPU PyTorch 2.11 (run with the
# CPython that has PyTorch installed, from this directory):
#     python gen.py
# It writes linalg_expected.json (results()) and errors_expected.txt
# (errors()) from linalg_cases.py.
import json
import os
import sys

sys.dont_write_bytecode = True
HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import linalg_cases  # noqa: E402

with open(os.path.join(HERE, "linalg_expected.json"), "w", newline="\n") as f:
    json.dump(linalg_cases.results(), f, indent=0, sort_keys=True)
    f.write("\n")
with open(os.path.join(HERE, "errors_expected.txt"), "w", newline="\n") as f:
    for line in linalg_cases.errors():
        f.write(line + "\n")
