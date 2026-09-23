# Regenerates the expected values from CPU PyTorch 2.11 (run with the
# CPython that has PyTorch installed, from this directory):
#     python gen.py
# It writes complex_expected.json (results()) and text_expected.txt
# (text()) from complex_cases.py.
import json
import os
import sys

sys.dont_write_bytecode = True
HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import complex_cases  # noqa: E402

with open(os.path.join(HERE, "complex_expected.json"), "w", newline="\n") as f:
    json.dump(complex_cases.results(), f, indent=0, sort_keys=True)
    f.write("\n")
with open(os.path.join(HERE, "text_expected.txt"), "w", newline="\n", encoding="utf-8") as f:
    for line in complex_cases.text():
        f.write(line + "\n")
