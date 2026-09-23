# Regenerates the expected values from CPU PyTorch 2.11 (run with the CPython
# that has PyTorch installed, from this directory):
#     python gen.py
# It writes dist_expected.json (dist_cases.py's results) and
# api_expected.txt (what dist_api.py prints).
import json
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import dist_cases  # noqa: E402

with open(os.path.join(HERE, "dist_expected.json"), "w") as f:
    json.dump(dist_cases.results(), f, indent=0, sort_keys=True)
    f.write("\n")

api = subprocess.run([sys.executable, os.path.join(HERE, "dist_api.py")], capture_output=True, text=True, check=True)
with open(os.path.join(HERE, "api_expected.txt"), "w", newline="\n") as f:
    f.write(api.stdout)
