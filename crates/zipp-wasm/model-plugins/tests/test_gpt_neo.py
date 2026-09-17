"""The GPT-Neo plugin against the reference implementation it claims to read.

These tests need a checkpoint, which is not in this repository: third-party
weights are not redistributed with ZIPP. Point ZIPP_GPT_NEO_MODEL at a
Hugging Face GPT-Neo folder, or leave the default and run:

    python tools/repack_safetensors.py --source models/tinystories-1m-src
    python tools/make_gpt_neo_oracle.py --source models/tinystories-1m-src \\
        --out models/tinystories-1m-src

Everything here compares against transformers, never against a plausible
reading of the output: agreeing that two things are "byte-level BPE" or "a
transformer" is exactly the kind of claim this plugin exists to avoid making.
"""
import json
import os
from pathlib import Path
import sys
import unittest

import numpy as np

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT/'tools'))
sys.path.insert(0, str(Path(__file__).resolve().parent))
from cpython_host import load_plugin, deliver_assets, plugin_support
from evaluator import run, decode as decode_run

MODEL = Path(os.environ.get('ZIPP_GPT_NEO_MODEL', ROOT/'models/tinystories-1m-src'))
HAVE_MODEL = (MODEL/'config.json').exists() and (MODEL/'oracle.json').exists()
REASON = f'no GPT-Neo checkpoint and oracle at {MODEL} (see this module docstring)'


@unittest.skipUnless(HAVE_MODEL, REASON)
class GPTNeoTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        from safetensors.numpy import load_file
        cls.plugin = load_plugin(ROOT/'plugins/gpt-neo')
        cls.assets = deliver_assets(cls.plugin, cls.plugin.NATIVE_LAYOUT, MODEL)
        cls.native = json.loads((MODEL/'config.json').read_text())
        cls.manifest = cls.plugin.native_manifest(cls.native, cls.assets, {'max_context': 512})
        cls.oracle = json.loads((MODEL/'oracle.json').read_text())
        shards = sorted(MODEL.glob('*.safetensors'))
        cls.weights = {}
        for shard in shards:
            cls.weights.update(load_file(str(shard)))

    def test_reads_the_checkpoints_own_config(self):
        config = self.manifest['config']
        self.assertEqual(config['vocab_size'], self.native['vocab_size'])
        self.assertEqual(config['num_layers'], self.native['num_layers'])
        # The HF shorthand [[["global", "local"], 4]] expands to one per layer.
        self.assertEqual(len(config['attention_types']), config['num_layers'])
        self.assertEqual(set(config['attention_types']) - {'global', 'local'}, set())
        self.assertEqual(config['activation'], self.native['activation_function'])

    def test_logit_parity_with_transformers(self):
        errors = []
        for case in self.oracle['cases']:
            with self.subTest(prompt=case['prompt']):
                template = self.plugin.build_graph(self.manifest['config'], case['tokens'])
                logits = run(template, self.weights)[-1]
                expected = np.array(case['logits'], dtype=np.float32)
                self.assertEqual(int(logits.argmax()), int(expected.argmax()))
                errors.append(float(np.max(np.abs(logits - expected))))
                np.testing.assert_allclose(logits, expected, rtol=2e-4, atol=2e-4)
        print('GPT-Neo vs transformers logits: max_abs_error =', max(errors))

    def test_greedy_generation_matches_transformers(self):
        config = self.manifest['config']
        for case in self.oracle['cases'][:2]:
            with self.subTest(prompt=case['prompt']):
                tokens, produced = list(case['tokens']), []
                for _ in range(8):
                    logits = run(self.plugin.build_graph(config, tokens), self.weights)[-1]
                    token = int(logits.argmax())
                    produced.append(token)
                    tokens.append(token)
                self.assertEqual(produced, case['greedy_tokens'][:len(produced)])

    def test_reads_checkpoint_order_tensors_without_a_rewrite(self):
        template = self.plugin.build_graph(self.manifest['config'], [1, 2, 3])
        bound = {b['tensor'] for b in template['bindings'] if 'tensor' in b}
        # Every name is one the checkpoint itself uses.
        missing = bound - set(self.weights)
        self.assertEqual(missing, set(), 'plugin asked for tensors the checkpoint does not have')
        transposed = {b['tensor'] for b in template['bindings'] if b.get('transpose')}
        self.assertIn('transformer.wte.weight', transposed, 'the tied output projection is the embedding')
        for name in transposed:
            self.assertEqual(len(self.weights[name].shape), 2)

    def test_local_attention_window_matches_the_reference_mask(self):
        config = dict(self.manifest['config'])
        window = config['window_size']
        template = self.plugin.build_graph(config, list(range(1, 6)))
        windows = [b.get('window') for b in template['bindings'] if b.get('kind') == 'causal']
        if 'local' in config['attention_types']:
            self.assertIn(window, windows, 'a local layer must bind a windowed mask')
        self.assertIn(None, windows, 'a global layer must bind an unwindowed mask')

    def test_the_activation_is_the_tanh_form_not_the_erf_kernel(self):
        template = self.plugin.build_graph(self.manifest['config'], [1, 2])
        ops = {node['op'] for node in template['graph']['nodes']}
        self.assertIn('tanh', ops, 'gelu_new is composed from tanh')
        self.assertNotIn('gelu', ops, "ZIPP's gelu kernel is the erf form and is a different function")

    def test_attention_is_not_scaled(self):
        # GPT-Neo multiplies q by k and masks, with no 1/sqrt(head_dim) factor.
        # Checked structurally rather than by hunting for the constant: this
        # checkpoint's head_dim is 4, so its scale would be 0.5, which is also
        # one of the GELU coefficients. The scores that reach the mask must come
        # straight out of a matmul.
        template = self.plugin.build_graph(self.manifest['config'], [1, 2])
        nodes = template['graph']['nodes']
        masks = {b['node'] for b in template['bindings'] if b.get('kind') == 'causal'}
        masked = [node for node in nodes if node['op'] == 'add' and node.get('b') in masks]
        self.assertEqual(len(masked), self.manifest['config']['num_layers'])
        for node in masked:
            self.assertEqual(nodes[node['a']]['op'], 'matmul',
                             'attention scores reach the mask unscaled')

    def test_cached_decode_matches_the_full_context_path(self):
        # The cached graph carries key and value caches across steps; the eager
        # graph keeps no state at all. They must agree token for token, or the
        # cache is quietly wrong in a way that still reads like English.
        template = self.plugin.build_decode_graph(self.manifest['config'])
        for case in self.oracle['cases']:
            with self.subTest(prompt=case['prompt']):
                cached = decode_run(template, self.weights, case['tokens'])[-1]
                expected = np.array(case['logits'], dtype=np.float32)
                self.assertEqual(int(cached.argmax()), int(expected.argmax()))
                np.testing.assert_allclose(cached, expected, rtol=2e-4, atol=2e-4)

    def test_the_cache_is_carried_not_returned(self):
        template = self.plugin.build_decode_graph(self.manifest['config'])
        nodes = template['graph']['nodes']
        outputs = {output['name'] for output in template['graph']['outputs']}
        carried = [node for node in nodes if node['op'] == 'input' and 'carry' in node]
        layers = self.manifest['config']['num_layers']
        self.assertEqual(len(carried), 2*layers, 'one key and one value cache per layer')
        for node in carried:
            self.assertIn(node['carry'], outputs, 'a carried input names an output')
        # Every cache starts zeroed and is written before it is read, so a second
        # generation needs no reset.
        zeroed = {b['node'] for b in template['bindings'] if b['kind'] == 'zeros'}
        self.assertEqual(zeroed, {node['id'] for node in carried})

    def test_a_decode_step_feeds_only_small_things(self):
        config = self.manifest['config']
        template = self.plugin.build_decode_graph(config)
        nodes = template['graph']['nodes']
        fed = [nodes[b['node']] for b in template['bindings'] if b['kind'] == 'step']
        elements = sum(int(np.prod(node['shape'])) for node in fed)
        # Two embedding rows, the masks and the write column: kilobytes, against
        # a vocabulary-sized projection that now stays on the device.
        self.assertLess(elements, 4*config['context_length'] + 4*config['hidden_size'])
        self.assertLess(elements, config['vocab_size'],
                        'a decode step must not upload anything vocabulary-sized')

    def test_declares_its_own_checkpoint_family(self):
        support = plugin_support(self.plugin)
        self.assertEqual(support['checkpoint_format'], 'hf.gpt-neo-v1')
        self.assertEqual(support['tokenizer_formats'], ['gpt2-byte-bpe-v1'])
        described = self.plugin.describe(self.manifest['config'])
        self.assertEqual(described['checkpoint_format'], 'hf.gpt-neo-v1')
        self.assertIs(described['decode'], True, 'this plugin offers a cached decode graph')

    def test_refuses_a_config_from_another_family(self):
        for change in [{'model_type': 'gpt2'}, {'model_type': 'qwen3'},
                       {'activation_function': 'gelu'}, {'attention_dropout': 0.1}]:
            with self.subTest(change=change), self.assertRaises(ValueError):
                self.plugin.native_manifest({**self.native, **change}, self.assets, {'max_context': 512})

    def test_only_the_last_position_reaches_the_vocabulary(self):
        config = self.manifest['config']
        template = self.plugin.build_graph(config, [1, 2, 3, 4, 5])
        output = template['graph']['outputs'][0]
        logits = run(template, self.weights)
        self.assertEqual(list(logits.shape), [1, config['vocab_size']],
                         'a real vocabulary times a full context would exceed the element limit')
        self.assertEqual(output['name'], 'logits')


@unittest.skipUnless(HAVE_MODEL, REASON)
class TokenizerDifferentialTests(unittest.TestCase):
    """The tokenizer against the one the checkpoint was trained with."""

    CASES = [
        'Once upon a time, there was a little girl.', 'Hello world', 'hello  world',
        '  leading spaces', 'trailing   ', "She's happy. He'd said they've gone; I'll wait.",
        "don't DON'T Don't", '123 4567 0 007 3.14 -5', 'tab\there\nnewline',
        'emoji \U0001f642\U0001f44d and accents café naïve',
        '中文字符 и русский',
        'MiXeD CaSe WoRdS', 'punctuation!!! ??? ...---___ (parens) [brackets] {braces}',
        'a'*200, '', ' ', '\n', '\t\t', 'word nbsp',
        'https://example.com/path?q=1&x=2', 'snake_case camelCase kebab-case',
        '$100 50% #hash @at ^caret ~tilde `tick', 'ellipsis… endash– emdash—',
        'ñ ü ö ß Ω π ∑ ∞', '​zero width',
        '1st 2nd 3rd 10th', 'quote "inside" and ' + chr(92) + 'backslash' + chr(92),
    ]

    @classmethod
    def setUpClass(cls):
        try:
            from transformers import AutoTokenizer
        except ImportError:  # pragma: no cover - environment without transformers
            raise unittest.SkipTest('transformers is not installed')
        cls.reference = AutoTokenizer.from_pretrained(str(MODEL))
        cls.plugin = load_plugin(ROOT/'plugins/gpt-neo')
        names = deliver_assets(cls.plugin, cls.plugin.NATIVE_LAYOUT, MODEL)
        cls.config = {'type': 'gpt2-byte-bpe-v1', 'eos_token_id': 50256,
                      'bos_token_id': 50256, 'assets': names}

    def test_encoding_matches_the_reference_exactly(self):
        for text in self.CASES:
            with self.subTest(text=text[:40]):
                self.assertEqual(self.plugin.encode(text, self.config),
                                 self.reference(text)['input_ids'])

    def test_decoding_round_trips(self):
        for text in self.CASES:
            with self.subTest(text=text[:40]):
                self.assertEqual(self.plugin.decode(self.plugin.encode(text, self.config), self.config), text)

    def test_the_corpus_the_checkpoint_was_trained_on(self):
        oracle = json.loads((MODEL/'oracle.json').read_text())
        for case in oracle['cases']:
            with self.subTest(prompt=case['prompt']):
                self.assertEqual(self.plugin.encode(case['prompt'], self.config), case['tokens'])

    def test_a_partial_byte_sequence_decodes_to_a_replacement(self):
        # Generation decodes a growing prefix, which can end mid-character.
        pieces = self.plugin.encode('\U0001f642', self.config)
        self.assertGreater(len(pieces), 1, 'an emoji is several byte tokens')
        self.assertEqual(self.plugin.decode(pieces[:1], self.config).count('�'), 1)

    def test_a_foreign_tokenizer_configuration_is_refused(self):
        for change in [{'type': 'character-v1'}, {'assets': {'vocab': 'vocab'}}]:
            with self.subTest(change=change), self.assertRaises(ValueError):
                self.plugin.encode('hello', {**self.config, **change})


if __name__ == '__main__':
    unittest.main()
