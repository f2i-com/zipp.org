"""Plugin math parity under CPython/NumPy, independently compared with PyTorch.
These are NOT ZIPP VM or GPU tests; test-integration.mjs covers the checkout gates.
"""
import importlib.util
import json
import math
from pathlib import Path
import sys
import unittest
import numpy as np
from safetensors.numpy import load_file
ROOT = Path(__file__).resolve().parents[1]

def load_plugin(name):
    folder = ROOT/'plugins'/name
    spec = importlib.util.spec_from_file_location('test_'+name.replace('-','_'), folder/'architecture.py', submodule_search_locations=[str(folder)])
    module = importlib.util.module_from_spec(spec); sys.modules[spec.name] = module; spec.loader.exec_module(module)
    return module

def evaluate(template, weights):
    bindings = {b['node']:b for b in template['bindings']}; values = []
    for n in template['graph']['nodes']:
        op = n['op']; a = values[n['a']] if 'a' in n else None; b = values[n['b']] if 'b' in n else None
        if op == 'input':
            binding = bindings.get(n['id'])
            if binding is None: out = np.array(n['data'],dtype=np.float32).reshape(n['shape'])
            elif binding['kind'] == 'tensor': out = weights[binding['tensor']]
            elif binding['kind'] == 'rows': out = weights[binding['tensor']][binding['indices']]
            else:
                t = binding['length']; out = np.zeros((t,t),dtype=np.float32)
                for row in range(t):
                    for col in range(t):
                        if col>row or ('window' in binding and col<=row-binding['window']): out[row,col] = -1e9
        elif op == 'full': out = np.full(n['shape'],n['value'],dtype=np.float32)
        elif op == 'add': out = a+b
        elif op == 'sub': out = a-b
        elif op == 'mul': out = a*b
        elif op == 'div': out = a/b
        elif op == 'matmul': out = a@b
        elif op == 'mean': out = a.mean(axis=n.get('axis'),keepdims=n.get('keepdim',False),dtype=np.float32)
        elif op == 'sqrt': out = np.sqrt(a)
        elif op == 'reshape': out = a.reshape(n['shape'])
        elif op == 'permute': out = a.transpose(n['dims'])
        elif op == 'transpose': out = a.T
        elif op == 'gelu':
            erf = np.vectorize(math.erf,otypes=[float])(a.astype(np.float64)/math.sqrt(2))
            out = 0.5*a.astype(np.float64)*(1+erf)
        elif op == 'softmax':
            exps = np.exp(a-a.max(axis=-1,keepdims=True)); out = exps/exps.sum(axis=-1,keepdims=True)
        else: raise AssertionError('Unsupported test op '+op)
        values.append(np.asarray(out,dtype=np.float32))
    return values[template['graph']['outputs'][0]['id']]

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

if __name__=='__main__':unittest.main()
