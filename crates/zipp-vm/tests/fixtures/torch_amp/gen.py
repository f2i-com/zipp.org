# Regenerates the expected values from CPU PyTorch 2.11 (run with the
# CPython that has PyTorch installed, from this directory):
#     python gen.py
# It writes amp_expected.json (results()) and amp_lines_expected.txt
# (lines()) from amp_cases.py, and autocast_lines_expected.txt
# (policy_lines() and state_lines()) and autocast_expected.json
# (train_values()) from autocast_cases.py.
import json
import os
import sys

sys.dont_write_bytecode = True
NL = chr(10)
HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import amp_cases  # noqa: E402
import autocast_cases  # noqa: E402

with open(os.path.join(HERE, "amp_expected.json"), "w", newline="\n") as f:
    json.dump(amp_cases.results(), f, indent=0, sort_keys=True)
    f.write("\n")
with open(os.path.join(HERE, "amp_lines_expected.txt"), "w", newline="\n") as f:
    for line in amp_cases.lines():
        f.write(line + "\n")
with open(os.path.join(HERE, "autocast_lines_expected.txt"), "w", newline=NL) as f:
    for line in autocast_cases.policy_lines() + autocast_cases.state_lines():
        f.write(line + NL)
with open(os.path.join(HERE, "autocast_expected.json"), "w", newline=NL) as f:
    json.dump(autocast_cases.train_values(), f, indent=0, sort_keys=True)
    f.write(NL)
