"""An independent NumPy reading of a Graph v2 plan, for differential testing.

This is deliberately NOT ZIPP: it is a second implementation of the same
protocol, written from the operation definitions, so that a plugin's graph can
be checked against a reference implementation without an engine. When it and
ZIPP agree with PyTorch, three implementations agree. When only the plugin and
this evaluator agree, nothing has been proven about the engine -- that is what
the checkout gates in tests/integration.test.mjs are for.
"""
import math

import numpy as np


def evaluate(template, weights, feeds=None):
    """Return every node's value; the caller picks the graph's output.

    `feeds` supplies the per-step inputs of a prepared decode graph by node id:
    the gathered rows, the mask, the write column and the carried caches. The
    engine keeps those on the device between runs; here they are ordinary
    arrays handed back in, which is what makes a cached decode checkable
    against the full-context path that has no state at all.
    """
    bindings = {b['node']: b for b in template['bindings']}
    feeds = feeds or {}
    values = []
    for node in template['graph']['nodes']:
        op = node['op']
        a = values[node['a']] if 'a' in node else None
        b = values[node['b']] if 'b' in node else None
        if op == 'input':
            binding = bindings.get(node['id'])
            if node['id'] in feeds:
                out = np.asarray(feeds[node['id']], dtype=np.float32).reshape(node['shape'])
            elif binding is None:
                out = np.array(node['data'], dtype=np.float32).reshape(node['shape'])
            elif binding['kind'] == 'zeros':
                out = np.zeros(node['shape'], dtype=np.float32)
            elif binding['kind'] == 'step':
                raise AssertionError('step input %d was not fed' % node['id'])
            elif binding['kind'] == 'tensor':
                out = weights[binding['tensor']]
                if binding.get('transpose'):
                    out = out.T
            elif binding['kind'] == 'rows':
                out = weights[binding['tensor']][binding['indices']]
            elif binding['kind'] == 'causal':
                length = binding['length']
                out = np.zeros((length, length), dtype=np.float32)
                for row in range(length):
                    for col in range(length):
                        if col > row or ('window' in binding and col <= row - binding['window']):
                            out[row, col] = -1e9
            else:
                raise AssertionError('unsupported binding ' + binding['kind'])
        elif op == 'full':
            out = np.full(node['shape'], node['value'], dtype=np.float32)
        elif op == 'add':
            out = a + b
        elif op == 'sub':
            out = a - b
        elif op == 'mul':
            out = a * b
        elif op == 'div':
            out = a / b
        elif op == 'matmul':
            out = a @ b
        elif op == 'mean':
            out = a.mean(axis=node.get('axis'), keepdims=node.get('keepdim', False), dtype=np.float32)
        elif op == 'sum':
            out = a.sum(axis=node.get('axis'), keepdims=node.get('keepdim', False), dtype=np.float32)
        elif op == 'sqrt':
            out = np.sqrt(a)
        elif op == 'exp':
            out = np.exp(a)
        elif op == 'tanh':
            out = np.tanh(a.astype(np.float64))
        elif op == 'sigmoid':
            out = 1.0 / (1.0 + np.exp(-a.astype(np.float64)))
        elif op == 'neg':
            out = -a
        elif op == 'relu':
            out = np.maximum(a, 0)
        elif op == 'reshape':
            out = a.reshape(node['shape'])
        elif op == 'permute':
            out = a.transpose(node['dims'])
        elif op == 'transpose':
            out = a.T
        elif op == 'gelu':
            # ZIPP's kernel is the erf form; `gelu_new` is a different function
            # and plugins that need it compose it from tanh.
            erf = np.vectorize(math.erf, otypes=[float])(a.astype(np.float64) / math.sqrt(2))
            out = 0.5 * a.astype(np.float64) * (1 + erf)
        elif op == 'softmax':
            exps = np.exp(a - a.max(axis=-1, keepdims=True))
            out = exps / exps.sum(axis=-1, keepdims=True)
        elif op == 'log_softmax':
            shifted = a - a.max(axis=-1, keepdims=True)
            out = shifted - np.log(np.exp(shifted).sum(axis=-1, keepdims=True))
        else:
            raise AssertionError('Unsupported test op ' + op)
        values.append(np.asarray(out, dtype=np.float32))
    return values


def run(template, weights):
    """The graph's single logits output."""
    return evaluate(template, weights)[template['graph']['outputs'][0]['id']]


def decode(template, weights, tokens):
    """Run a prepared decode graph token by token, carrying the caches by hand.

    Returns the logits after the last token, which must equal what the
    full-context graph produces for the same prompt.
    """
    context = template['context']
    nodes = template['graph']['nodes']
    outputs = {o['name']: o['id'] for o in template['graph']['outputs']}
    carried = [(node['id'], outputs[node['carry']]) for node in nodes
               if node['op'] == 'input' and 'carry' in node]
    state, logits = {}, None
    for position, token in enumerate(tokens):
        feeds = dict(state)
        for binding in template['bindings']:
            if binding['kind'] != 'step':
                continue
            node = nodes[binding['node']]
            if binding['slot'] == 'rows':
                index = token if binding['index'] == 'token' else position
                feeds[binding['node']] = weights[binding['tensor']][index]
            elif binding['slot'] == 'mask':
                mask = np.zeros(context, dtype=np.float32)
                for column in range(context):
                    window = binding.get('window')
                    if column > position or (window is not None and column <= position - window):
                        mask[column] = -1e9
                feeds[binding['node']] = mask
            else:
                write = np.zeros(context, dtype=np.float32)
                write[position] = 1.0
                feeds[binding['node']] = write
        values = evaluate(template, weights, feeds)
        state = {input_id: values[output_id] for input_id, output_id in carried}
        logits = values[outputs['logits']]
    return logits
