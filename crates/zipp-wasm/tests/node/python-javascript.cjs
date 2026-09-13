// Test each artifact's opt-in contract, rather than silently skipping it.
const assert = require('node:assert/strict');
const { Engine } = require(process.env.ZIPP_NODE_PACKAGE || './pkg/zipp_wasm.js');
const enabled = process.argv.includes('--enabled');
const source = `import javascript
from js import eval as js_eval
print(javascript.eval('[1, 2, 3].map(x => x * 2)'))
javascript.eval('globalThis.counter = 41')
print(js_eval('++globalThis.counter'))
print(javascript.engine)
`;
const engine = new Engine();
try {
  if (enabled) {
    engine.initPythonProject({ main: source }, 'main');
    assert.deepEqual(engine.takeOutput(), ['[2, 4, 6]', '42', 'zipp-same-vm']);
    const other = new Engine();
    try {
      other.initPythonProject({ main: "import javascript\nprint(javascript.eval('typeof globalThis.counter'))\n" }, 'main');
      assert.deepEqual(other.takeOutput(), ['undefined']);
    } finally { other.dispose(); }
  } else {
    assert.throws(() => engine.initPythonProject({ main: source }, 'main'), /javascript|module/i);
  }
  console.log(`PASS: Python/JavaScript WASM interop ${enabled ? 'same instance and independent engines' : 'absent in default build'}`);
} finally { engine.dispose(); }
