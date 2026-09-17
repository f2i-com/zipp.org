#!/usr/bin/env python3
"""Write a model.json beside a checkpoint folder, changing nothing in it.

    python tools/pin_checkpoint.py --plugin gpt-neo --source <hf folder>

A checkpoint folder can be opened directly, without any of this, through
`ModelSession.openNative`: the plugin reads the project's own config.json and
the host asks you to approve the digests of everything it will read. That is
the weaker claim, because a folder from someone else carries no pin.

This writes the stronger one. It runs the same `native_manifest` the plugin
would run, then records the result as a manifest that pins the exact plugin
identity and the SHA-256 of every file the session will read, so a later load
fails if any of them changed. The weights are not touched, renamed or
rewritten; only model.json is added.
"""
import argparse
import hashlib
import json
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))
from cpython_host import load_plugin, write_json

ROOT = Path(__file__).resolve().parents[1]


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--plugin', default='gpt-neo', help='plugin folder under plugins/')
    parser.add_argument('--source', required=True, help='checkpoint folder to pin')
    parser.add_argument('--max-context', type=int, default=512,
                        help='the context the pin declares; every token is recomputed from the '
                             'whole context, so a checkpoint maximum is rarely usable')
    args = parser.parse_args()
    source = Path(args.source)
    plugin_dir = ROOT / 'plugins' / args.plugin
    module = load_plugin(plugin_dir)
    layout = module.NATIVE_LAYOUT
    manifest_path = plugin_dir / 'plugin.json'
    manifest = json.loads(manifest_path.read_text())

    native = json.loads((source / layout['config']).read_text())
    assets = {name: name for name in layout['assets']}
    described = module.native_manifest(native, assets, {'max_context': args.max_context})

    shards = sorted(p.name for p in source.glob('*.safetensors'))
    if not shards:
        raise SystemExit('no safetensors in %s; run tools/repack_safetensors.py first' % source)
    index = source / 'model.safetensors.index.json'
    if index.exists():
        weight_map = json.loads(index.read_text())['weight_map']
        shards = sorted(set(weight_map.values()))

    tokenizer = dict(described['tokenizer'])
    tokenizer['assets'] = {
        name: {'path': entry['path'], 'form': entry['form'], 'sha256': digest(source / entry['path'])}
        for name, entry in layout['assets'].items()
    }
    model = {
        'format': 'zipp.local-model', 'version': 1,
        'architecture': {'id': manifest['id'], 'version': manifest['plugin_version'],
                         'sha256': digest(manifest_path)},
        'checkpoint_format': manifest['checkpoint_format'],
        'config': described['config'],
        'tokenizer': tokenizer,
        'weights': [{'path': shard, 'sha256': digest(source / shard)} for shard in shards],
        'license': 'Pinned from %s; the checkpoint keeps its own licence and is not '
                   'redistributed with ZIPP' % source.name,
    }
    write_json(source / 'model.json', model)
    print('pinned %s@%s over %d shard(s) and %d tokenizer asset(s)'
          % (manifest['id'], manifest['plugin_version'], len(shards), len(tokenizer['assets'])))
    for note in described.get('notes', []):
        print('  note:', note)
    print('wrote %s; no other file was touched' % (source / 'model.json'))


if __name__ == '__main__':
    main()
