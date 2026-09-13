// The identical PyTorch-reference fixture runs in the native and WASM VM.
const { readFileSync } = require('node:fs');
const path = require('node:path');
const assert = require('node:assert/strict');
const { Engine } = require('./pkg/zipp_wasm.js');
const source = readFileSync(path.join(__dirname, '../../../zipp-vm/tests/fixtures/torch_conv2d.py'), 'utf8');
const e = new Engine();
try {
  e.initPythonProject({ main: source }, 'main');
  assert.deepEqual(e.takeOutput(), [
    'conv2d case 0 passed', 'conv2d case 1 passed',
    'conv2d case 2 passed', 'conv2d case 3 passed', 'conv2d optimizer passed',
  ]);
  console.log('PASS: actual WASM CPU Conv2d forward, gradients and optimizer match PyTorch reference');
} finally { e.dispose(); }
