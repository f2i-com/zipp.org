"""Hardware acceptance: serve the repo root, then pass its URL to this script.
Unavailable WebGL2/WebGPU is a failure, not a passing GPU check.
"""
import json
import sys
from playwright.sync_api import sync_playwright
base = (sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:8766").rstrip("/")
with sync_playwright() as p:
    browser = p.chromium.launch(channel="chrome", headless=True)
    page = browser.new_page()
    page.goto(base + "/", wait_until="domcontentloaded")
    result = page.evaluate("""async () => {
      const root = '/crates/zipp-wasm/';
      const wasm = await import(root + 'dist/all/zipp_wasm.js');
      await wasm.default();
      const {createRuntime} = await import(root + 'gpu-lab/src/runtime.mjs');
      const {createPythonGPUAdapter} = await import(root + 'gpu-lab/src/zipp-python-adapter.mjs');
      const {checkBackend} = await import(root + 'gpu-lab/tests/browser-cases.mjs');
      const source = await (await fetch('/crates/zipp-vm/tests/fixtures/torch_training.py')).text();
      const expected = await (await fetch('/crates/zipp-vm/tests/fixtures/torch_training_expected.json')).json();
      const reports = [];
      for (const backend of ['webgl2', 'webgpu']) {
        const cases = await checkBackend(backend);
        if (cases.status !== 'passed' || cases.passed !== 17) throw Error(JSON.stringify(cases));
        const e = new wasm.Engine();
        const rt = await createRuntime({backend});
        const events = [];
        const adapter = createPythonGPUAdapter(e, rt, {allowExecute:true, onDelivered:ev=>events.push(ev)});
        try {
          e.initPythonProject({main:source + `
import json
compiled = torch.compile(train_step, training=True)
def request():
    compiled(inputs, targets).submit(lambda loss: print(json.dumps(state(loss))))
`}, 'main');
          let maxError = 0;
          for (let step=0; step<5; step++) {
            e.pythonCall('request', []); adapter.drain(); await adapter.idle();
            const lines = e.takeOutput();
            if (lines.length !== 1) throw Error(JSON.stringify({lines, events}));
            const actual = JSON.parse(lines[0]);
            if (actual.length !== expected[step].length) throw Error('Wrong output length');
            actual.forEach((x,i)=>{
              const delta = Math.abs(x-expected[step][i]); maxError = Math.max(maxError, delta);
              if (!Number.isFinite(x) || delta > 2e-6) throw Error(`${backend} step ${step}, value ${i}: ${x} versus ${expected[step][i]}`);
            });
          }
          if (events.some(e=>!e.delivered || !e.reply.ok || e.error)) throw Error('GPU callback failed');
          reports.push({backend, info:rt.info(), steps:5, kernelCases:cases.passed, maxError});
        } finally { adapter.invalidate(); rt.dispose(); e.dispose(); }
      }
      return reports;
    }""")
    print(json.dumps(result, indent=2))
    browser.close()
