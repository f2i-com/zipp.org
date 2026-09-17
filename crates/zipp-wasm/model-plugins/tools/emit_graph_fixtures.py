#!/usr/bin/env python3
"""Record the actual plugin's graph plans for host and optional checkout tests.

The plugin is imported the way the host installs it, so these fixtures come from
the same source ZIPP compiles. The output is expected to be byte-identical
across runs: a diff here means the architecture changed, and the stored PyTorch
oracle no longer describes it.
"""
from pathlib import Path
import json
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))
from cpython_host import load_plugin, write_json

ROOT = Path(__file__).resolve().parents[1]
plugin = load_plugin(ROOT/'plugins/tiny-causal')
model = json.loads((ROOT/'examples/tiny-char/model.json').read_text())
oracle = json.loads((ROOT/'examples/tiny-char/oracle.json').read_text())
cases = [dict(prompt=c['prompt'], tokens=c['tokens'], template=plugin.build_graph(model['config'], c['tokens']))
         for c in oracle['cases']]
write_json(ROOT/'examples/tiny-char/graph-cases.json', {'cases': cases})
print(f'Wrote {len(cases)} graph fixtures from the actual Python plugin.')
