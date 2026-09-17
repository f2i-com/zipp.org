#!/usr/bin/env python3
"""Record what PyTorch's Qwen3DecoderLayer does, to check ZIPP's torch against.

    python tools/make_torch_block_case.py --out models/qwen3-block-case.json

The layer is small and randomly initialised: the point is not that it says
anything, but that ZIPP's torch subset computes exactly what the reference
computes, operation for operation. tests/torch-subset.test.mjs runs the same
layer inside the engine from tests/fixtures/qwen3_block.py.
"""
import argparse
import json
from pathlib import Path


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--out', default='models/qwen3-block-case.json')
    parser.add_argument('--length', type=int, default=5)
    args = parser.parse_args()
    import torch
    from transformers import Qwen3Config
    from transformers.models.qwen3.modeling_qwen3 import Qwen3DecoderLayer, Qwen3RotaryEmbedding
    torch.manual_seed(7)
    config = Qwen3Config(vocab_size=64, hidden_size=32, intermediate_size=64, num_hidden_layers=1,
                         num_attention_heads=4, num_key_value_heads=2, head_dim=8,
                         max_position_embeddings=64, rms_norm_eps=1e-6, rope_theta=10000.0,
                         attention_bias=False, attention_dropout=0.0)
    config._attn_implementation = 'eager'
    layer = Qwen3DecoderLayer(config, 0).eval()
    rotary = Qwen3RotaryEmbedding(config)
    length = args.length
    x = torch.randn(1, length, config.hidden_size)
    with torch.no_grad():
        cos, sin = rotary(x, torch.arange(length).unsqueeze(0))
        mask = torch.full((length, length), float('-inf')).triu(1).reshape(1, 1, length, length)
        out = layer(x, attention_mask=mask, position_embeddings=(cos, sin))
    out = out[0] if isinstance(out, tuple) else out
    case = {
        'source': 'transformers Qwen3DecoderLayer on CPU float32',
        'config': {'hidden_size': config.hidden_size, 'num_attention_heads': config.num_attention_heads,
                   'num_key_value_heads': config.num_key_value_heads, 'head_dim': config.head_dim,
                   'intermediate_size': config.intermediate_size, 'rms_norm_eps': config.rms_norm_eps,
                   'rope_theta': config.rope_theta, 'length': length},
        'input': x.reshape(-1).tolist(),
        'weights': {name: value.reshape(-1).tolist() for name, value in layer.state_dict().items()},
        'expected': out.reshape(-1).tolist(),
    }
    target = Path(args.out)
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_bytes((json.dumps(case) + '\n').encode())
    print('wrote %s: %d tensors, %d output values' % (target, len(case['weights']), len(case['expected'])))


if __name__ == '__main__':
    main()
