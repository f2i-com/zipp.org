// Actual Zipp WASM training against CPU PyTorch reference values.
const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const path = require('node:path');
const {pathToFileURL} = require('node:url');
const {Engine, zippProfile} = require('./pkg/zipp_wasm.js');
const root = path.resolve(__dirname, '../../../..');
const lab = path.join(root, 'crates/zipp-wasm/gpu-lab');
const load = file => import(pathToFileURL(path.join(lab, file)));
async function main() {
  assert.ok(JSON.parse(zippProfile()).languages.includes('python'));
  const source = await fs.readFile(path.join(root, 'crates/zipp-vm/tests/fixtures/torch_training.py'), 'utf8');
  const expected = JSON.parse(await fs.readFile(path.join(root, 'crates/zipp-vm/tests/fixtures/torch_training_expected.json')));
  const {createRuntime} = await load('src/runtime.mjs');
  const {createPythonGPUAdapter} = await load('src/zipp-python-adapter.mjs');
  const wasmBytes = await fs.readFile(path.join(lab, 'wasm/kernels.wasm'));
  const program = source + `
import json
compiled = torch.compile(train_step, training=True)
pending = None
def done(loss):
    print(json.dumps(state(loss)))
def failed(error):
    print('FAILED', str(error))
def request():
    global pending
    pending = compiled(inputs, targets)
    pending.submit(done, failed)
def weights():
    return [p.detach().tolist() for p in model.parameters()]
def gradients():
    return [None if p.grad is None else p.grad.tolist() for p in model.parameters()]
def change():
    with torch.no_grad():
        model[0].weight.fill_(0.25)
def change_grad():
    model[0].weight.grad.zero_()
def freeze():
    model[0].weight.requires_grad_(False)
def duplicate():
    try:
        pending.submit(done)
    except RuntimeError:
        print('duplicate rejected')
`;
  for (const backend of ['cpu-js', 'wasm']) {
    const e = new Engine(); e.initPythonProject({main:program}, 'main');
    const runtime = await createRuntime({backend, wasmBytes});
    const events = [];
    const adapter = createPythonGPUAdapter(e, runtime, {allowExecute:true, onDelivered:ev=>events.push(ev)});
    for (let step=0; step<5; step++) {
      const before = e.pythonCall('weights', []), grads = e.pythonCall('gradients', []);
      e.pythonCall('request', []);
      assert.deepEqual(e.pythonCall('weights', []), before, 'recording must not change weights');
      assert.deepEqual(e.pythonCall('gradients', []), grads, 'recording must not clear or change gradients');
      adapter.drain(); await adapter.idle();
      const lines = e.takeOutput(); assert.equal(lines.length, 1);
      const actual = JSON.parse(lines[0]);
      assert.equal(actual.length, expected[step].length);
      actual.forEach((value,i)=>assert.ok(Math.abs(value-expected[step][i])<2e-6, `${backend} step ${step} value ${i}: ${value} vs ${expected[step][i]}`));
    }
    assert.ok(events.every(ev=>ev.delivered && ev.reply.ok && !ev.error));
    e.pythonCall('duplicate', []); assert.deepEqual(e.takeOutput(), ['duplicate rejected']);
    // An explicit backend error leaves prior gradients and weights intact.
    const before = e.pythonCall('weights', []), grads = e.pythonCall('gradients', []);
    e.pythonCall('request', []); const [denied] = e.takeHostRequests();
    e.pythonCall('__zipp_py_deliver', [denied.id, {ok:false,error:{code:'DEVICE_LOST',message:'test loss'}}]);
    assert.match(e.takeOutput()[0], /FAILED.*DEVICE_LOST/);
    assert.deepEqual(e.pythonCall('weights', []), before);
    assert.deepEqual(e.pythonCall('gradients', []), grads);
    // Out-of-order completions: only the first matching snapshot can commit.
    e.pythonCall('request', []); e.pythonCall('request', []);
    adapter.drain(); await adapter.idle();
    const overlapping = e.takeOutput(); assert.equal(overlapping.length,2);
    assert.ok(Array.isArray(JSON.parse(overlapping[0])));
    assert.match(overlapping[1], /FAILED.*stale/);
    // An external edit is preserved when the stale result arrives.
    e.pythonCall('request', []); e.pythonCall('change', []);
    const edited = e.pythonCall('weights', []);
    adapter.drain(); await adapter.idle();
    assert.match(e.takeOutput()[0], /FAILED.*stale/);
    assert.deepEqual(e.pythonCall('weights', []), edited);
    // Edits to gradients or requires_grad metadata also make a result stale.
    e.pythonCall('request', []); e.pythonCall('change_grad', []);
    const beforeGradEdit = e.pythonCall('weights', []);
    adapter.drain(); await adapter.idle();
    assert.match(e.takeOutput()[0], /FAILED.*stale/);
    assert.deepEqual(e.pythonCall('weights', []), beforeGradEdit);
    e.pythonCall('request', []); e.pythonCall('freeze', []);
    adapter.drain(); await adapter.idle();
    assert.match(e.takeOutput()[0], /FAILED.*stale/);
    assert.deepEqual(e.pythonCall('weights', []), beforeGradEdit);
    adapter.invalidate(); runtime.dispose(); e.dispose();
    console.log(`${backend}: 5-step PyTorch loss/gradient/weight parity; failure, duplicate and stale-result checks passed`);
  }
  const edgeSource = await fs.readFile(path.join(root, 'crates/zipp-vm/tests/fixtures/torch_training_edges.py'), 'utf8');
  const edgeProgram = edgeSource + `
compiled = torch.compile(step, training=True)
pending = None
def record():
    global pending
    pending = compiled(x)
def submit():
    pending.submit(lambda loss: print('loss', loss.item()), lambda error: print('FAILED', str(error)))
`;
  for (const option of ['lr','weight_decay','maximize','momentum','dampening','nesterov']) {
    const e = new Engine();
    e.initPythonProject({main:edgeProgram + `
optimizer.param_groups[0]["${option}"] = []
try:
    record()
    print('NOT REJECTED')
except NotImplementedError as error:
    print(str(error))
`}, 'main');
    assert.deepEqual(e.takeOutput(), ['GPU SGD options must be numeric or boolean scalars']);
    assert.deepEqual(e.pythonCall('values', []), [2, 3, 11, 7]);
    assert.deepEqual(e.takeHostRequests(), []); e.dispose();
  }
  for (const backend of ['cpu-js', 'wasm']) {
    const runtime = await createRuntime({backend, wasmBytes});
    // A leaf first seen under no_grad still contributes to later recorded ops.
    // The detached branch contributes to loss, but must not contribute gradients.
    const e = new Engine(); e.initPythonProject({main:edgeProgram}, 'main');
    const adapter = createPythonGPUAdapter(e, runtime, {allowExecute:true});
    e.pythonCall('record', []); e.pythonCall('submit', []);
    adapter.drain(); await adapter.idle();
    assert.deepEqual(e.takeOutput(), ['loss 202.0']);
    const result = e.pythonCall('values', []);
    assert.ok(Math.abs(result[0] - 1.9) < 1e-6);
    assert.deepEqual(result.slice(1), [3, 1, 7]);
    adapter.invalidate(); e.dispose();
    // Check both changes before submit and changes while work is pending.
    for (const timing of ['before-submit', 'pending']) {
      for (const option of ['lr','weight_decay','maximize','momentum','dampening','nesterov','append','replace','remove','add_group','reorder_groups','remove_group','remove_option']) {
        const e = new Engine(); e.initPythonProject({main:edgeProgram}, 'main');
        const adapter = createPythonGPUAdapter(e, runtime, {allowExecute:true});
        const before = e.pythonCall('values', []);
        e.pythonCall('record', []);
        if (timing === 'before-submit') e.pythonCall('change_optimizer', [option]);
        e.pythonCall('submit', []);
        if (timing === 'pending') e.pythonCall('change_optimizer', [option]);
        adapter.drain(); await adapter.idle();
        const lines = e.takeOutput();
        assert.equal(lines.length, 1);
        assert.match(lines[0], /FAILED.*stale.*optimizer changed/, `${backend} ${timing} ${option}`);
        assert.deepEqual(e.pythonCall('values', []), before, `${option}: all weights/gradients, including q.grad, must survive`);
        adapter.invalidate(); e.dispose();
      }
    }
    runtime.dispose();
    console.log(`${backend}: no_grad leaf/operation semantics and 26 optimizer mutation cases passed`);
  }
  const rejectionSource = source + `
compiled = torch.compile(train_step, training=True)
for option in ['momentum', 'dampening', 'nesterov']:
    optimizer.param_groups[0][option] = 1
    try:
        compiled(inputs, targets)
        print('NOT REJECTED')
    except NotImplementedError:
        print('rejected')
    optimizer.param_groups[0][option] = 0
for kind in [torch.optim.Adam, torch.optim.AdamW, torch.optim.RMSprop]:
    optimizer = kind(model.parameters())
    try:
        compiled(inputs, targets)
        print('NOT REJECTED')
    except NotImplementedError:
        print('rejected')
print(all(p.grad is None for p in model.parameters()))
`;
  const e = new Engine();e.initPythonProject({main:rejectionSource},'main');
  assert.deepEqual(e.takeOutput(), [...Array(6).fill('rejected'), 'True']);
  assert.deepEqual(e.takeHostRequests(), []); e.dispose();
  console.log('Unsupported training optimizers/options rejected before submission; CPU gradients preserved');
}
main().catch(error=>{console.error(error);process.exitCode=1;});
