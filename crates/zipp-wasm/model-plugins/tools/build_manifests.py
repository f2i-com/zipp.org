#!/usr/bin/env python3
"""Regenerate plugin manifests, the website catalogue and the example model pins.

Run this after editing any plugin source. It rehashes the Python files, rebuilds
`plugin.json` and `catalog.v1.json`, and rewrites each example model's
`architecture` pin and `checkpoint_format` so the checked-in fixtures stay
loadable. Weights, configuration, tokenizer and licence fields are left alone:
this tool re-pins an existing checkpoint, it never converts one.

A plugin's declared support is read from its source (`CHECKPOINT_FORMAT`,
`TOKENIZER_FORMATS`) rather than repeated here, so the manifest cannot claim a
checkpoint family the code does not implement. Raising `plugin_version` is a
deliberate edit below; every model pinned to the old identity must be re-pinned
(this tool does that for the bundled examples) or it will be refused.
"""
from pathlib import Path
import hashlib
import json
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))
from cpython_host import load_plugin, plugin_support, write_json

ROOT = Path(__file__).resolve().parents[1]
# directory, plugin id, plugin version, example model directory
PLUGINS = [
    ('tiny-causal', 'org.zipp.tiny-causal', '0.2.0', 'tiny-char'),
    ('bigram', 'org.zipp.bigram', '0.2.0', 'bigram'),
]

def main():
    catalogue = []
    for directory, identifier, version, example in PLUGINS:
        path = ROOT/'plugins'/directory
        support = plugin_support(load_plugin(path))
        sources = {p.relative_to(path).as_posix(): hashlib.sha256(p.read_bytes()).hexdigest()
                   for p in sorted(path.rglob('*.py'))}
        manifest = dict(format='zipp.python-model-plugin', version=1, id=identifier,
                        plugin_version=version, entry='architecture', capabilities=['graph-v2'],
                        checkpoint_format=support['checkpoint_format'],
                        tokenizer_formats=support['tokenizer_formats'], sources=sources)
        raw = (json.dumps(manifest, indent=2) + '\n').encode()
        (path/'plugin.json').write_bytes(raw)
        digest = hashlib.sha256(raw).hexdigest()
        catalogue.append(dict(id=identifier, version=version,
                              manifest=f'plugins/{directory}/plugin.json', sha256=digest,
                              checkpoint_format=support['checkpoint_format'],
                              tokenizer_formats=support['tokenizer_formats']))
        model_path = ROOT/'examples'/example/'model.json'
        model = json.loads(model_path.read_text())
        model['architecture'] = dict(id=identifier, version=version, sha256=digest)
        # Placed next to `architecture` so a reader sees identity and checkpoint
        # family together; the host compares it with the plugin's declaration.
        rebuilt = {}
        for key, value in model.items():
            rebuilt[key] = value
            if key == 'architecture':
                rebuilt['checkpoint_format'] = support['checkpoint_format']
        if model['tokenizer'].get('type') not in support['tokenizer_formats']:
            raise SystemExit(f"{model_path}: tokenizer {model['tokenizer'].get('type')!r} "
                             f"is not implemented by {identifier}")
        write_json(model_path, rebuilt)
        print(f'{identifier}@{version}: {len(sources)} source file(s), '
              f'{support["checkpoint_format"]} checkpoints, re-pinned {example}/model.json')
    write_json(ROOT/'catalog.v1.json', dict(version=1, plugins=catalogue))
    print('Updated plugin manifests, the catalogue and the example model pins.')

if __name__ == '__main__':
    main()
