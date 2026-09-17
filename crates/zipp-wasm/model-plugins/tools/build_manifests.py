#!/usr/bin/env python3
"""Regenerate source hashes and the same-origin website catalogue after editing a plugin.
Model manifests pin these identities; update their architecture.sha256 intentionally.
"""
from pathlib import Path
import hashlib, json
ROOT = Path(__file__).resolve().parents[1]
catalogue = []
for directory, identifier in [('tiny-causal', 'org.zipp.tiny-causal'), ('bigram', 'org.zipp.bigram')]:
    path = ROOT/'plugins'/directory
    sources = {p.relative_to(path).as_posix():hashlib.sha256(p.read_bytes()).hexdigest() for p in sorted(path.rglob('*.py'))}
    manifest = dict(format='zipp.python-model-plugin', version=1, id=identifier, plugin_version='0.1.0',
                    entry='architecture', capabilities=['graph-v2'], sources=sources)
    raw = (json.dumps(manifest,indent=2)+'\n').encode(); (path/'plugin.json').write_bytes(raw)
    catalogue.append(dict(id=identifier, version='0.1.0', manifest=f'plugins/{directory}/plugin.json',sha256=hashlib.sha256(raw).hexdigest()))
(ROOT/'catalog.v1.json').write_text(json.dumps(dict(version=1,plugins=catalogue),indent=2)+'\n')
print('Updated plugin manifests and catalogue.')
