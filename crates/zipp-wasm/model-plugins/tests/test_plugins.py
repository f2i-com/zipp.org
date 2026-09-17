"""Plugin math parity under CPython/NumPy, independently compared with PyTorch.
These are NOT ZIPP VM or GPU tests; test-integration.mjs covers the checkout gates.
"""
import json
from pathlib import Path
import sys
import unittest
import numpy as np
from safetensors.numpy import load_file
ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT/'tools'))
from cpython_host import load_plugin as _load_plugin, plugin_support
sys.path.insert(0, str(Path(__file__).resolve().parent))
from evaluator import run as evaluate

def load_plugin(name):
    """Import the plugin as the registry installs it, not as a loose file."""
    return _load_plugin(ROOT/'plugins'/name)

class PluginTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.plugin = load_plugin('tiny-causal');cls.bigram = load_plugin('bigram')
        cls.model = json.loads((ROOT/'examples/tiny-char/model.json').read_text())
        cls.oracle = json.loads((ROOT/'examples/tiny-char/oracle.json').read_text())
        cls.weights = load_file(str(ROOT/'examples/tiny-char/weights.safetensors'))
    def test_five_logit_parity_cases(self):
        errors=[]
        for case in self.oracle['cases']:
            with self.subTest(prompt=case['prompt']):
                template=self.plugin.build_graph(self.model['config'],case['tokens'])
                result=evaluate(template,self.weights); expected=np.array(case['logits'],dtype=np.float32)
                error=float(np.max(np.abs(result-expected)));errors.append(error)
                np.testing.assert_allclose(result,expected,rtol=2e-5,atol=2e-5)
        print('CPython/NumPy versus stored PyTorch logits: max_abs_error =',max(errors))
    def test_greedy_generation_matches_reference(self):
        for case in self.oracle['cases']:
            with self.subTest(prompt=case['prompt']):
                tokens=list(case['tokens']);out=[]
                for _ in range(32):
                    if len(tokens)>self.model['config']['context_length']:break
                    logits=evaluate(self.plugin.build_graph(self.model['config'],tokens),self.weights)
                    token=int(logits[-1].argmax())
                    if token==self.model['tokenizer']['eos_token_id']:break
                    tokens.append(token);out.append(token)
                self.assertEqual(out,case['greedy_tokens'])
    def test_causality(self):
        ids=self.oracle['cases'][3]['tokens']
        full=evaluate(self.plugin.build_graph(self.model['config'],ids),self.weights)
        short=evaluate(self.plugin.build_graph(self.model['config'],ids[:4]),self.weights)
        np.testing.assert_allclose(full[:4],short,rtol=1e-5,atol=1e-5)
    def test_tokenizer_roundtrip_and_unknown(self):
        t=self.model['tokenizer'];text='hello zipp!'
        self.assertEqual(self.plugin.decode(self.plugin.encode(text,t),t),text)
        self.assertEqual(self.plugin.decode(self.plugin.encode('☃',t),t),'\ufffd')
    def test_empty_prompt_is_bos(self):
        self.assertEqual(self.plugin.encode('',self.model['tokenizer']),[0])
    def test_configuration_failure(self):
        for key,value in [('hidden_size',33),('num_layers',0),('num_heads',True),('layer_norm_epsilon',float('nan'))]:
            c=dict(self.model['config']);c[key]=value
            with self.subTest(key=key),self.assertRaises(ValueError):self.plugin.describe(c)
    def test_context_and_ids_rejected(self):
        for ids in [[],[0]*49,[-1],[True],[100000]]:
            with self.subTest(ids=ids[:2]),self.assertRaises(ValueError):self.plugin.build_graph(self.model['config'],ids)
    def test_no_binary_weights_in_python_graph(self):
        template=self.plugin.build_graph(self.model['config'],[0,3,4])
        self.assertLess(len(json.dumps(template)),30000)
        self.assertFalse(any('data' in node for node in template['graph']['nodes']))
        self.assertTrue(all(node['id']==i for i,node in enumerate(template['graph']['nodes'])))
    def test_second_architecture(self):
        c={'vocab_size':4,'context_length':8};weights={'transition_logits':np.arange(16,dtype=np.float32).reshape(4,4)}
        graph=self.bigram.build_graph(c,[0,2])
        np.testing.assert_array_equal(evaluate(graph,weights),weights['transition_logits'][[2]])
    def test_tokenizer_wrong_schema_rejected(self):
        t=dict(self.model['tokenizer']);t['type']='gpt2'
        with self.assertRaises(ValueError):self.plugin.encode('hello',t)
    def test_declared_support_matches_the_manifest_and_describe(self):
        for name in ['tiny-causal','bigram']:
            with self.subTest(plugin=name):
                module=load_plugin(name);support=plugin_support(module)
                manifest=json.loads((ROOT/'plugins'/name/'plugin.json').read_text())
                self.assertEqual(manifest['checkpoint_format'],support['checkpoint_format'])
                self.assertEqual(manifest['tokenizer_formats'],support['tokenizer_formats'])
                config=self.model['config'] if name=='tiny-causal' else {'vocab_size':7,'context_length':32}
                described=module.describe(config)
                self.assertEqual(described['checkpoint_format'],support['checkpoint_format'])
                self.assertEqual(described['tokenizer_formats'],support['tokenizer_formats'])
    def test_fixture_claims_only_its_own_checkpoint_family(self):
        # The next model milestone is a separate GPT-Neo/TinyStories plugin with
        # that family's tokenizer and state-dict mapping. Emitting the same
        # transformer operations is not compatibility with those checkpoints, so
        # this fixture must never advertise them, in the manifest or the source.
        for name in ['tiny-causal','bigram']:
            with self.subTest(plugin=name):
                support=plugin_support(load_plugin(name))
                self.assertEqual(support['checkpoint_format'],'zipp.'+('tiny-causal' if name=='tiny-causal' else 'bigram')+'-v1')
                declared=' '.join([support['checkpoint_format']]+support['tokenizer_formats'])
                for foreign in ['gpt-neo','gptneo','tinystories','gpt2','llama','sentencepiece','byte-bpe']:
                    self.assertNotIn(foreign,declared)
                self.assertEqual(support['tokenizer_formats'],['character-v1'])

if __name__=='__main__':unittest.main()
