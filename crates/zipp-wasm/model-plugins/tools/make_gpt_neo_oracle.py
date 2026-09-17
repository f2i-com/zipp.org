#!/usr/bin/env python3
"""Record what the reference implementation does, so the plugin can be compared.

    python tools/make_gpt_neo_oracle.py --source <hf folder> --out models/tinystories-1m

Runs Hugging Face transformers on the original checkpoint and stores, per prompt,
the token ids its own tokenizer produced, the complete final-position logits, and
the greedy continuation. Everything downstream -- the NumPy evaluator, the ZIPP
engine, the browser -- is compared against this, never against a plausible
reading of the generated text.

The oracle lives beside the converted model, outside the repository: it is
derived from third-party weights and is not redistributed with ZIPP.
"""
import argparse
import json
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))
from cpython_host import write_json

PROMPTS = [
    "Once upon a time, there was a little",
    "Lily went to the park and saw a",
    "The dog was very happy because",
    "Tom said,",
    "One day, a boy named Ben found a shiny",
]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--source', required=True, help='original Hugging Face model folder')
    parser.add_argument('--out', required=True, help='converted ZIPP model folder to write into')
    parser.add_argument('--new-tokens', type=int, default=24)
    args = parser.parse_args()
    import torch
    from transformers import AutoModelForCausalLM, AutoTokenizer
    source, out = Path(args.source), Path(args.out)
    tokenizer = AutoTokenizer.from_pretrained(str(source))
    model = AutoModelForCausalLM.from_pretrained(str(source), dtype=torch.float32)
    model.eval()
    cases = []
    with torch.no_grad():
        for prompt in PROMPTS:
            ids = tokenizer(prompt)['input_ids']
            logits = model(torch.tensor([ids])).logits[0, -1].tolist()
            generated, current = [], list(ids)
            for _ in range(args.new_tokens):
                token = int(model(torch.tensor([current])).logits[0, -1].argmax())
                if token == model.config.eos_token_id:
                    break
                generated.append(token)
                current.append(token)
            cases.append(dict(prompt=prompt, tokens=ids, logits=logits, greedy_tokens=generated,
                              greedy_text=tokenizer.decode(generated)))
            print('%-42s -> %s' % (repr(prompt), repr(cases[-1]['greedy_text'][:60])))
    write_json(out/'oracle.json', dict(
        source='Hugging Face transformers CPU reference; not a ZIPP execution result',
        model=str(source), transformers_dtype='float32', cases=cases))
    print('wrote %s/oracle.json with %d cases' % (out, len(cases)))


if __name__ == '__main__':
    main()
