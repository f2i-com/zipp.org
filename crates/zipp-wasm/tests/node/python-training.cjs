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
    assert.deepEqual(e.takeOutput(), ['GPU optimizer options must be numeric or boolean scalars']);
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
        print('accepted')
    except NotImplementedError:
        print('rejected')
    optimizer.param_groups[0][option] = 0
for kind in [torch.optim.Adam, torch.optim.AdamW, torch.optim.RMSprop]:
    optimizer = kind(model.parameters())
    try:
        compiled(inputs, targets)
        print('accepted')
    except NotImplementedError:
        print('rejected')
print(all(p.grad is None for p in model.parameters()))
`;
  const e = new Engine();e.initPythonProject({main:rejectionSource},'main');
  assert.deepEqual(e.takeOutput(), [...Array(5).fill('accepted'), 'rejected', 'True']);
  assert.deepEqual(e.takeHostRequests(), []); e.dispose();
  console.log('momentum, dampening, Nesterov, Adam and AdamW are captured, RMSprop is rejected before submission; CPU gradients preserved');


  // ---- prepared sessions: compiled.prepare() through the real engine and the session protocol ----
  // Six distinct batches (fixtures/torch_prepared.py): first as chained
  // compiled calls, then, from the same initial weights, as one prepared
  // session (two single steps, the rest in one run), synced and disposed.
  // Both must track PyTorch 2.11's eager steps, and each other.
  const preparedSource = await fs.readFile(path.join(root, 'crates/zipp-vm/tests/fixtures/torch_prepared.py'), 'utf8');
  const preparedExpected = JSON.parse(await fs.readFile(path.join(root, 'crates/zipp-vm/tests/fixtures/torch_prepared_expected.json')));
  for (const backend of ['cpu-js', 'wasm']) {
    for (const kase of ['gelu_ce_adam', 'relu_mse_nesterov']) {
      const expected = preparedExpected[kase];
      const e = new Engine();
      e.initPythonProject({main: `case = ${JSON.stringify(kase)}\n` + preparedSource + `
import json
compiled = torch.compile(train_step, training=True)
initial = [p.detach().clone() for p in model.parameters()]
prepared = None
def chained(index):
    compiled(*batches[index]).submit(lambda loss: print(json.dumps(state(loss))), lambda error: print('FAILED', str(error)))
def reset():
    for p, value in zip(model.parameters(), initial):
        p.data = value.clone()
        p.grad = None
    optimizer.state.clear()
def prepare():
    global prepared
    prepared = compiled.prepare(*batches[0], on_ready=lambda p: print('ready', p.backend), on_error=lambda error: print('FAILED', str(error)))
    print('prepared', prepared.backend, len(prepared.session.outputs), len(prepared.session.resident))
def step(index):
    prepared.step(lambda loss: print('loss', json.dumps(loss.item())), *batches[index], on_error=lambda error: print('FAILED', str(error)))
def steps(start, stop):
    prepared.steps(lambda losses: print('losses', json.dumps([l.item() for l in losses])), batches[start:stop], on_error=lambda error: print('FAILED', str(error)))
def refused():
    for call in (lambda: compiled(*batches[0]), lambda: train_step(*batches[0])):
        try:
            call()
            print('NOT REFUSED')
        except RuntimeError as error:
            print('refused', str(error))
def sync():
    prepared.sync(lambda p: print('synced', json.dumps(state(torch.tensor(0.0)))), on_error=lambda error: print('FAILED', str(error)))
def dispose():
    prepared.dispose()
    print('disposed', prepared.executed, prepared.backend)
def eager(index):
    print('eager', json.dumps(state(train_step(*batches[index]))))
`}, 'main');
      const runtime = await createRuntime({backend, wasmBytes});
      const seen = [];
      const adapter = createPythonGPUAdapter(e, runtime, {allowExecute: true, onDelivered: ev => seen.push(ev.reply.ok ? 'ok' : ev.reply.error.code)});
      // A delivery can raise the next request (a run queued behind the
      // session's creation is sent when the create reply arrives), so the
      // host drains until the guest has nothing pending.
      const settle = async () => {
        for (let i = 0; i < 16; i++) {
          adapter.drain(); await adapter.idle();
          if (adapter.pending === 0 && e.pythonCall('__zipp_py_pending_host', []) === 0) return;
        }
        throw new Error('host requests did not settle');
      };
      const near = (actual, wanted, what, tolerance = 2e-6) => {
        assert.equal(actual.length, wanted.length, what);
        let worst = 0;
        actual.forEach((value, i) => { worst = Math.max(worst, Math.abs(value - wanted[i])); });
        assert.ok(worst < tolerance, `${backend} ${kase} ${what}: deviates by ${worst}`);
        return worst;
      };
      const chainedStates = [];
      for (let index = 0; index < expected.length; index++) {
        e.pythonCall('chained', [index]); await settle();
        const [line] = e.takeOutput();
        chainedStates.push(JSON.parse(line));
        near(chainedStates[index], expected[index], `chained step ${index}`);
      }
      seen.length = 0;
      e.pythonCall('reset', []);
      e.pythonCall('prepare', []);
      e.pythonCall('step', [0]);   // queued behind the session's creation
      e.pythonCall('refused', []);
      assert.deepEqual(e.takeOutput(), [`prepared None ${kase.endsWith('adam') ? '17 16' : '13 12'}`, 'refused Parameter is resident in a prepared GPU session; sync() and dispose() it before recording another step', 'refused Parameter is resident in a prepared GPU session; sync() and dispose() it before an eager optimizer step'],
        'the session is requested, not created, before the host drains; its parameters are refused meanwhile');
      await settle();
      assert.equal(adapter.sessions, 1);
      const lines = e.takeOutput();
      assert.equal(lines[0], `ready ${backend}`);
      const losses = [JSON.parse(lines[1].slice(5))];
      e.pythonCall('step', [1]); await settle();
      losses.push(JSON.parse(e.takeOutput()[0].slice(5)));
      e.pythonCall('steps', [2, expected.length]); await settle();
      losses.push(...JSON.parse(e.takeOutput()[0].slice(7)));
      near(losses, expected.map(s => s[0]), 'resident losses vs PyTorch');
      near(losses, chainedStates.map(s => s[0]), 'resident losses vs chained compiled calls', 1e-6);
      e.pythonCall('sync', []); await settle();
      const synced = JSON.parse(e.takeOutput()[0].slice(7));
      const worstPyTorch = near(synced.slice(1), expected[expected.length - 1].slice(1), 'synced weights, gradients and optimizer state vs PyTorch');
      const worstChained = near(synced.slice(1), chainedStates[chainedStates.length - 1].slice(1), 'synced state vs chained compiled calls', 1e-6);
      e.pythonCall('dispose', []);
      await settle();
      assert.deepEqual(e.takeOutput(), [`disposed ${expected.length} ${backend}`]);
      assert.equal(adapter.sessions, 0, 'the adapter holds no session after the guest disposed it');
      assert.deepEqual(seen, ['ok', 'ok', 'ok', 'ok', 'ok', 'ok'], 'create, three runs, one download and the dispose were delivered');
      // After dispose the eager optimizer continues from the synced state.
      e.pythonCall('eager', [0]);
      assert.equal(e.takeOutput()[0].slice(0, 5), 'eager');
      adapter.invalidate(); runtime.dispose(); e.dispose();
      console.log(`${backend} ${kase}: prepared session over ${expected.length} batches tracks PyTorch (max abs error ${worstPyTorch.toExponential(2)}) and chained compiled calls (${worstChained.toExponential(2)}); refusals, sync and dispose checked`);
    }
  }

  // ---- a batch buffer refilled in place between steps queued in one call ----
  // The host takes queued steps after the guest's call returns, so each
  // step's feed must be posted as a copy: refilling one buffer in place
  // before the next step() must not change what an earlier step trains on.
  {
    const kase = 'gelu_ce_adam';
    const e = new Engine();
    e.initPythonProject({main: `case = ${JSON.stringify(kase)}\n` + preparedSource + `
import json
compiled = torch.compile(train_step, training=True)
initial = [p.detach().clone() for p in model.parameters()]
xbuf, ybuf = batches[0][0].clone(), batches[0][1].clone()
prepared = None
def prepare():
    global prepared
    for p, value in zip(model.parameters(), initial):
        p.data = value.clone()
        p.grad = None
    optimizer.state.clear()
    prepared = compiled.prepare(xbuf, ybuf)
def run(in_place):
    for x, y in batches[:4]:
        if in_place:
            xbuf.copy_(x)
            ybuf.copy_(y)
            prepared.step(lambda loss: print(json.dumps(loss.item())), xbuf, ybuf)
        else:
            prepared.step(lambda loss: print(json.dumps(loss.item())), x.clone(), y.clone())
def dispose():
    prepared.dispose()
`}, 'main');
    const runtime = await createRuntime({backend: 'cpu-js', wasmBytes});
    const adapter = createPythonGPUAdapter(e, runtime, {allowExecute: true});
    const settle = async () => {
      for (let i = 0; i < 16; i++) {
        adapter.drain(); await adapter.idle();
        if (adapter.pending === 0 && e.pythonCall('__zipp_py_pending_host', []) === 0) return;
      }
      throw new Error('host requests did not settle');
    };
    const losses = {};
    for (const inPlace of [false, true]) {
      e.pythonCall('prepare', []); await settle();
      e.pythonCall('run', [inPlace]); await settle();
      losses[inPlace] = e.takeOutput().map(line => JSON.parse(line));
      e.pythonCall('dispose', []); await settle();
    }
    const expected = preparedExpected[kase].slice(0, 4).map(s => s[0]);
    assert.equal(losses[true].length, 4);
    losses[false].forEach((loss, i) => assert.ok(Math.abs(loss - expected[i]) < 2e-6, `fresh batch ${i}: ${loss} vs ${expected[i]}`));
    assert.deepEqual(losses[true], losses[false], 'steps queued in one call must each train on the batch passed to them');
    adapter.invalidate(); runtime.dispose(); e.dispose();
    console.log('cpu-js: steps queued in one call over a buffer refilled in place train on their own batches');
  }

  // ---- protocol version 3: masks, where, clamp and dropout through the host ----
  // fixtures/torch_gpu2/masks.py against PyTorch 2.11 as chained compiled
  // calls (the "edges" case starts on every clamp/hardtanh/relu6/tie
  // boundary), then a prepared dropout session whose device masks must be
  // fresh every step and equal the ones zipp_gpu's reference draws from the
  // recorded seed, with the synced gradient masked like the last step.
  const masksSource = await fs.readFile(path.join(root, 'crates/zipp-vm/tests/fixtures/torch_gpu2/masks.py'), 'utf8');
  const masksExpected = JSON.parse(await fs.readFile(path.join(root, 'crates/zipp-vm/tests/fixtures/torch_gpu2/masks_expected.json')));
  for (const backend of ['cpu-js', 'wasm']) {
    for (const kase of ['edges', 'tensorclamp', 'where', 'logical', 'activations']) {
      const expected = masksExpected[kase];
      const e = new Engine();
      e.initPythonProject({main: `case = ${JSON.stringify(kase)}\n` + masksSource + `
import json
compiled = torch.compile(train_step, training=True)
def chained(index):
    compiled(*batches[index]).submit(lambda loss: print(json.dumps(state(loss))), lambda error: print('FAILED', str(error)))
`}, 'main');
      const runtime = await createRuntime({backend, wasmBytes});
      const adapter = createPythonGPUAdapter(e, runtime, {allowExecute: true});
      let worst = 0;
      for (let index = 0; index < expected.length; index++) {
        e.pythonCall('chained', [index]); adapter.drain(); await adapter.idle();
        const [line] = e.takeOutput();
        const actual = JSON.parse(line);
        assert.equal(actual.length, expected[index].length, `${backend} ${kase} step ${index}`);
        actual.forEach((value, i) => { worst = Math.max(worst, Math.abs(value - expected[index][i]) / (1 + Math.abs(expected[index][i]))); });
      }
      assert.ok(worst < 1e-6, `${backend} ${kase}: deviates from PyTorch by ${worst}`);
      adapter.invalidate(); runtime.dispose(); e.dispose();
      console.log(`${backend} ${kase}: ${expected.length} compiled version-3 steps track PyTorch (max relative error ${worst.toExponential(2)})`);
    }
    const e = new Engine();
    e.initPythonProject({main: `import torch
import zipp_gpu
from torch import nn
import torch.nn.functional as F
w = nn.Parameter(torch.ones(32))
optimizer = torch.optim.SGD([w], lr=0.0)
def step(x):
    optimizer.zero_grad()
    out = F.dropout(x * w, 0.5)
    out.sum().backward()
    optimizer.step()
    return out
compiled = torch.compile(step, training=True)
x = torch.ones(16, 32)
prepared = None
last = []
def prepare():
    global prepared
    prepared = compiled.prepare(x, on_ready=lambda p: print('ready', p.backend), on_error=lambda error: print('FAILED', str(error)))
def report(outs):
    node = [n for n in prepared.session._program["nodes"] if n["op"] == "uniform"][0]
    same = []
    for i, out in enumerate(outs):
        g = zipp_gpu.Graph()
        u = zipp_gpu.execute_locally(g.program(u=g.uniform(tuple(node["shape"]), node["seed"], node["step"] + i)))["outputs"]["u"]["data"]
        same.append([1.0 if v >= 0.5 else 0.0 for v in u] == [1.0 if v != 0 else 0.0 for v in out.flatten().tolist()])
    last[:] = [outs[-1]]
    print('masks', all(same), len(set(tuple(o.flatten().tolist()) for o in outs)), sorted(set(outs[0].flatten().tolist())))
def run():
    prepared.steps(report, [(x,)] * 4, on_error=lambda error: print('FAILED', str(error)))
def sync():
    prepared.sync(lambda p: print('grad', torch.equal(w.grad, (last[0] != 0).float().sum(0) * 2.0)), on_error=lambda error: print('FAILED', str(error)))
def dispose():
    prepared.dispose()
`}, 'main');
    const runtime = await createRuntime({backend, wasmBytes});
    const adapter = createPythonGPUAdapter(e, runtime, {allowExecute: true});
    const settle = async () => {
      for (let i = 0; i < 16; i++) {
        adapter.drain(); await adapter.idle();
        if (adapter.pending === 0 && e.pythonCall('__zipp_py_pending_host', []) === 0) return;
      }
      throw new Error('host requests did not settle');
    };
    e.pythonCall('prepare', []); await settle();
    e.pythonCall('run', []); await settle();
    e.pythonCall('sync', []); await settle();
    e.pythonCall('dispose', []); await settle();
    assert.deepEqual(e.takeOutput(), [`ready ${backend}`, 'masks True 4 [0.0, 2.0]', 'grad True']);
    adapter.invalidate(); runtime.dispose(); e.dispose();
    console.log(`${backend}: a prepared dropout session draws a fresh device mask per step, equal to zipp_gpu's reference, and its gradient carries it`);
  }
}
main().catch(error=>{console.error(error);process.exitCode=1;});
