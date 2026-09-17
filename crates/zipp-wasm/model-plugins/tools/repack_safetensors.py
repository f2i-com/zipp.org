#!/usr/bin/env python3
"""Repack a pickled PyTorch checkpoint as safetensors, changing nothing else.

    python tools/repack_safetensors.py --source models/tinystories-1m-src

ZIPP will not unpickle a checkpoint: `pytorch_model.bin` is a pickle, and
loading one executes whatever it was built to execute. This is the one offline
step a Hugging Face folder may still need, and it is deliberately not a
conversion -- every tensor keeps its name, its shape and its dtype, so what
comes out is the same checkpoint in a container that can be read without
running code. A repo that already ships `model.safetensors` needs no step at
all; the plugin reads it as it is.

Requires torch only to read the pickle. Compare the report it prints with the
model card if the provenance of a checkpoint matters to you.
"""
import argparse
from pathlib import Path


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--source', required=True, help='Hugging Face model folder')
    parser.add_argument('--out', default=None, help='destination folder (default: alongside the source)')
    args = parser.parse_args()
    import torch
    from safetensors.torch import save_file
    source = Path(args.source)
    out = Path(args.out) if args.out else source
    out.mkdir(parents=True, exist_ok=True)
    pickled = source / 'pytorch_model.bin'
    if not pickled.exists():
        raise SystemExit('no pytorch_model.bin in %s (a safetensors checkpoint needs no repack)' % source)
    state = torch.load(str(pickled), map_location='cpu', weights_only=True)
    # contiguous() only settles memory layout; names, shapes and dtypes are the
    # checkpoint's own. Tensors that share storage are copied so safetensors can
    # store each one separately, which changes no value.
    tensors = {name: value.contiguous().clone() for name, value in state.items()}
    target = out / 'model.safetensors'
    # `format` is not decoration: transformers refuses a safetensors file whose
    # metadata does not name its framework, so a repack without it would be
    # readable by ZIPP and by nothing else.
    save_file(tensors, str(target), metadata={'format': 'pt', 'repacked_from': 'pytorch_model.bin'})
    kinds = {}
    for value in tensors.values():
        kinds[str(value.dtype)] = kinds.get(str(value.dtype), 0) + 1
    print('%d tensors repacked, unchanged: %s' % (len(tensors), ', '.join(
        '%d x %s' % (count, dtype) for dtype, count in sorted(kinds.items()))))
    print('%.1f MiB -> %s' % (target.stat().st_size / 2 ** 20, target))
    print('names, shapes and dtypes are identical to the pickle; nothing was renamed or transposed')


if __name__ == '__main__':
    main()
