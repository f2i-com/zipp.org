# Regenerates the expected values from CPU PyTorch 2.11 (run with the CPython
# that has PyTorch installed, from this directory):
#     python gen.py
# It writes dist_expected.json and dist_expected2.json (dist_cases.py's and
# dist_cases2.py's results) and api_expected.txt and api_expected2.txt (what
# dist_api.py and dist_api2.py print).
import json
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import dist_cases  # noqa: E402
import dist_cases2  # noqa: E402

for name, results in (("dist_expected.json", dist_cases.results), ("dist_expected2.json", dist_cases2.results2)):
    with open(os.path.join(HERE, name), "w", newline="\n") as f:
        json.dump(results(), f, indent=0, sort_keys=True)
        f.write("\n")

for script, name in (("dist_api.py", "api_expected.txt"), ("dist_api2.py", "api_expected2.txt")):
    api = subprocess.run([sys.executable, os.path.join(HERE, script)], capture_output=True, text=True, check=True)
    with open(os.path.join(HERE, name), "w", newline="\n") as f:
        f.write(api.stdout)
