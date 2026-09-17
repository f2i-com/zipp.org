#!/usr/bin/env python3
"""Stage a transformers modelling file, and what it should produce, for the shim.

    python tools/stage_transformers_model.py --model qwen3

Copies `modeling_<model>.py` and `configuration_<model>.py` out of the
transformers you have installed -- unmodified, which is the whole point -- and
records a small model's weights and logits from that same package. Nothing is
downloaded and nothing is checked in: the files belong to Hugging Face, and the
output directory is gitignored.

tests/transformers-shim.test.mjs then runs those files inside the engine against
interop/transformers and compares.
"""
import argparse
import hashlib
import importlib
import inspect
import json
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))
from cpython_host import write_json

ROOT = Path(__file__).resolve().parents[1]
# Small enough to run in an interpreter, wide enough to exercise the pieces that
# differ between architectures: grouped-query heads, a head dimension that is
# not hidden/heads, and more than one layer.
SHAPES = dict(vocab_size=48, hidden_size=32, intermediate_size=64, num_hidden_layers=2,
              num_attention_heads=4, num_key_value_heads=2, head_dim=8,
              max_position_embeddings=64, rms_norm_eps=1e-6, rope_theta=10000.0,
              tie_word_embeddings=True, attention_bias=False, attention_dropout=0.0,
              use_cache=False)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--model', default='qwen3', help='a transformers model directory name')
    parser.add_argument('--out', default=None, help='where to stage (default models/transformers-<model>)')
    parser.add_argument('--tokens', default='3,14,7,2,9')
    args = parser.parse_args()
    import torch
    out = Path(args.out) if args.out else ROOT/('models/transformers-' + args.model)
    out.mkdir(parents=True, exist_ok=True)

    package = 'transformers.models.' + args.model
    modeling = importlib.import_module(package + '.modeling_' + args.model)
    configuration = importlib.import_module(package + '.configuration_' + args.model)
    staged = {}
    for module in (modeling, configuration):
        name = module.__name__.rsplit('.', 1)[1] + '.py'
        source = Path(inspect.getfile(module)).read_bytes()
        (out/name).write_bytes(source)
        staged[name] = hashlib.sha256(source).hexdigest()
        print('%-30s %6d bytes, copied unmodified' % (name, len(source)))

    def defined_in(module, suffix):
        # Only what the module defines: it also imports PretrainedConfig and
        # friends, and picking one of those builds the wrong thing.
        for key, value in vars(module).items():
            if key.endswith(suffix) and isinstance(value, type) and value.__module__ == module.__name__:
                return value
        raise SystemExit('%s defines no %s' % (module.__name__, suffix))

    config_class = defined_in(configuration, 'Config')
    model_class = defined_in(modeling, 'ForCausalLM')
    print('building %s with %s' % (model_class.__name__, config_class.__name__))
    torch.manual_seed(5)
    config = config_class(**SHAPES)
    config._attn_implementation = 'eager'
    model = model_class(config).eval()
    ids = torch.tensor([[int(t) for t in args.tokens.split(',')]])
    with torch.no_grad():
        logits = model(input_ids=ids).logits
    state = model.state_dict()
    write_json(out/'case.json', {
        'source': 'transformers %s on CPU float32' % args.model,
        'model': args.model,
        'config_class': config_class.__name__,
        'model_class': model_class.__name__,
        'sources': staged,
        'config': SHAPES,
        'input_ids': ids[0].tolist(),
        'weights': {name: value.reshape(-1).tolist() for name, value in state.items()},
        'shapes': {name: list(value.shape) for name, value in state.items()},
        'expected': logits.reshape(-1).tolist(),
    })
    print('%d tensors, %d logits -> %s' % (len(state), logits.numel(), out/'case.json'))


if __name__ == '__main__':
    main()
