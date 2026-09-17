#!/usr/bin/env python3
"""Record the actual plugin's graph plans for host and optional checkout tests."""
from pathlib import Path
import importlib.util,json,sys
ROOT=Path(__file__).resolve().parents[1]
folder=ROOT/'plugins/tiny-causal'
spec=importlib.util.spec_from_file_location('fixture_plugin',folder/'architecture.py',submodule_search_locations=[str(folder)])
plugin=importlib.util.module_from_spec(spec);sys.modules[spec.name]=plugin;spec.loader.exec_module(plugin)
model=json.loads((ROOT/'examples/tiny-char/model.json').read_text())
oracle=json.loads((ROOT/'examples/tiny-char/oracle.json').read_text())
cases=[dict(prompt=c['prompt'],tokens=c['tokens'],template=plugin.build_graph(model['config'],c['tokens'])) for c in oracle['cases']]
(ROOT/'examples/tiny-char/graph-cases.json').write_text(json.dumps({'cases':cases},indent=2)+'\n')
print('Wrote five graph fixtures from the actual Python plugin.')
