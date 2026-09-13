// Local bridge contract tests. No Python, network listener, or GPU is started.
const test = require('node:test');
const assert = require('node:assert/strict');
const { Readable, PassThrough } = require('node:stream');
const { EventEmitter } = require('node:events');
const { createLab } = require('./native-lab.cjs');

const hardware = { type: 'probe', labAvailable: true, devices: [
  { id: 'cuda:0', usable: true, uuid: 'second-card' },
  { id: 'cuda:1', usable: true, uuid: 'first-card' }
] };
function fakeExecute(file, args, options, callback) {
  queueMicrotask(() => callback(null, args.includes('--probe') ? JSON.stringify(hardware) :
    '0, GPU-first-card, NVIDIA card A, 20, 100, 1000, 40, 60\n1, GPU-second-card, NVIDIA card B, 30, 200, 1000, 45, 80\n', ''));
}
function child() {
  const c = new EventEmitter(); c.stdout = new PassThrough(); c.stderr = new PassThrough();
  c.kill = () => { c.stdout.end(); c.stderr.end(); c.emit('close', null); return true; };
  return c;
}
function fixture(options = {}) {
  const launches = [];
  const lab = createLab({ root: process.cwd(), port: 18765, executeFile: fakeExecute,
    spawnProcess(file, args) { const c = child(); launches.push({ args, child: c }); return c; }, ...options });
  async function request(endpoint, body, token, chunks) {
    const req = Readable.from(chunks || (body === undefined ? [] : [Buffer.from(JSON.stringify(body))]));
    req.method = body === undefined ? 'GET' : 'POST';
    req.headers = { host: '127.0.0.1:18765', 'content-type': 'application/json', 'x-nca-token': token };
    let status, data;
    const res = { writeHead(code) { status = code; }, end(text) { data = JSON.parse(text); } };
    await lab.handle(req, res, new URL(`http://127.0.0.1:18765/api/nca/${endpoint}`));
    return { status, data };
  }
  return { lab, request, launches };
}

test('Unicode request chunks and option-like prompts reach the runner intact', async t => {
  const f = fixture(); t.after(() => f.lab.close());
  const { data: status } = await f.request('status');
  const prompt = '--été 🧪';
  const body = { mode: 'language', devices: ['cuda:0'], prompt };
  const chunks = [...Buffer.from(JSON.stringify(body))].map(byte => Buffer.from([byte]));
  const result = await f.request('run', body, status.token, chunks);
  assert.equal(result.status, 202);
  assert.ok(f.launches[0].args.includes(`--prompt=${prompt}`));
  assert.equal((await f.request('status')).data.running, true);
  await f.request('stop', {}, status.token);
  const stopped = (await f.request('status')).data;
  assert.equal(stopped.running, false);
  assert.equal(stopped.jobs[0].state, 'stopped');
});

test('NVIDIA telemetry follows UUIDs when CUDA device numbering is reversed', async () => {
  const f = fixture(); const { data } = await f.request('status');
  assert.deepEqual(data.gpus.map(g => [g.id, g.cudaDevices]), [
    ['nvidia:0', ['cuda:1']], ['nvidia:1', ['cuda:0']]
  ]);
});

test('Unavailable telemetry never hides a healthy Python runner', async () => {
  const f = fixture({ executeFile(file, args, options, callback) {
    if (file === 'nvidia-smi') throw Error('Telemetry launch denied');
    fakeExecute(file, args, options, callback);
  } });
  const result = await f.request('status');
  assert.equal(result.status, 200); assert.equal(result.data.labAvailable, true);
  assert.deepEqual(result.data.gpus, []);
});

test('A synchronous Python launch error is reported as capability information', async () => {
  const f = fixture({ executeFile() { throw Error('Python launch denied'); } });
  const result = await f.request('status');
  assert.equal(result.status, 200); assert.match(result.data.error, /Python launch denied/);
  assert.deepEqual(result.data.devices, []);
});

test('A failed second worker remains visible and does not strand the first worker', async t => {
  let count = 0;
  const f = fixture({ spawnProcess() { if (++count === 2) throw Error('Worker launch failed'); return child(); } });
  t.after(() => f.lab.close());
  const { data } = await f.request('status');
  const run = await f.request('run', { mode: 'memory', devices: ['cuda:0', 'cuda:1'] }, data.token);
  assert.equal(run.status, 202); assert.equal(run.data.jobs[1].state, 'failed');
  assert.match(run.data.jobs[1].error, /Worker launch failed/);
  await f.request('stop', {}, data.token);
  const status = (await f.request('status')).data;
  assert.equal(status.running, false);
  assert.deepEqual(status.jobs.map(job => job.state), ['stopped', 'failed']);
});

test('Invalid run options and oversized requests launch no workers', async t => {
  const f = fixture(); t.after(() => f.lab.close());
  const { data } = await f.request('status');
  const body = { mode: 'memory', devices: ['cuda:0'] };
  assert.equal((await f.request('run', body, 'wrong')).status, 403);
  assert.equal((await f.request('run', { ...body, steps: 0 }, data.token)).status, 400);
  assert.equal((await f.request('run', body, data.token, [Buffer.alloc(8193)])).status, 413);
  assert.equal(f.launches.length, 0);
});
