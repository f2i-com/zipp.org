// A loopback-only bridge for fixed NCA workloads; never executes browser source.
"use strict";
const { spawn, execFile } = require('node:child_process');
const { randomBytes } = require('node:crypto');
const path = require('node:path');
const readline = require('node:readline');

function createLab({ root, port }) {
  const python = process.env.NCA_PYTHON || 'python';
  const lab = path.resolve(process.env.NCA_LAB_DIR || path.join(root, '..', 'nca_fast_memory_language_lab'));
  const env = { ...process.env, NCA_LAB_DIR: lab, PYTHONUNBUFFERED: '1', PYTHONIOENCODING: 'utf-8' };
  const script = path.join(__dirname, 'native_lab.py');
  const token = randomBytes(32).toString('hex');
  const origins = new Set([`http://127.0.0.1:${port}`, `http://localhost:${port}`]);
  let probePromise, hardware, active = [], jobs = [], events = [], cursor = 0, generation = 0;
  let telemetryCache = { time: 0, data: [] }, telemetryPromise;
  const record = event => { events.push({ ...event, cursor: ++cursor }); if (events.length > 400) events.shift(); };
  function probe() {
    return probePromise ||= new Promise(resolve => {
      execFile(python, ['-u', script, '--probe'], { env, windowsHide: true, timeout: 60000, maxBuffer: 1024 * 1024 }, (error, stdout, stderr) => {
        try {
          const data = stdout.trim().split('\n').map(s => JSON.parse(s)).find(x => x.type === 'probe');
          if (error || !data) throw error || Error(stderr || 'No PyTorch response');
          hardware = data; resolve(data);
        } catch (err) { resolve({ error: `Cannot use ${python}: ${err.message}`, lab, devices: [] }); }
      });
    });
  }
  function telemetry() {
    if (Date.now() - telemetryCache.time < 1500) return Promise.resolve(telemetryCache.data);
    return telemetryPromise ||= new Promise(resolve => {
      execFile('nvidia-smi', ['--query-gpu=index,name,utilization.gpu,memory.used,memory.total,temperature.gpu,power.draw', '--format=csv,noheader,nounits'],
        { windowsHide: true, timeout: 4000, maxBuffer: 65536 }, (error, stdout) => {
          const data = error ? [] : stdout.trim().split('\n').filter(Boolean).map(line => {
            const [index, name, utilization, usedMiB, totalMiB, temperature, watts] = line.split(',').map(s => s.trim());
            const number = s => Number.isFinite(Number(s)) ? Number(s) : null;
            return { id: `cuda:${index}`, name, utilization: number(utilization), usedMiB: number(usedMiB), totalMiB: number(totalMiB), temperature: number(temperature), watts: number(watts) };
          });
          telemetryCache = { time: Date.now(), data }; telemetryPromise = null; resolve(data);
        });
    });
  }
  function launch(config) {
    generation++;
    jobs = []; events = [];
    const group = `${Date.now()}-${generation}`;
    for (const [index, device] of config.devices.entries()) {
      const id = `${group}-${index}`;
      const output = path.join(root, 'target', 'nca-runs', id);
      const args = ['-u', script, '--mode', config.mode, '--device', device, '--out', output,
        '--steps', String(config.steps), '--batch', String(config.batch), '--length', String(config.length),
        '--seed', String(config.seed + index), '--bytes', String(config.bytes), '--temperature', String(config.temperature),
        '--frame-ms', String(config.frameMs), '--prompt', config.prompt];
      const child = spawn(python, args, { env, windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] });
      const job = { id, device, seed: config.seed + index, mode: config.mode, output, state: 'starting' };
      jobs.push(job); active.push({ child, job });
      let stderr = '', finished = false;
      const lines = readline.createInterface({ input: child.stdout });
      lines.on('line', line => {
        try {
          const event = JSON.parse(line);
          if (event.type === 'started' && job.state !== 'stopping') job.state = 'running';
          if (event.type === 'done') finished = true;
          if (event.type === 'error') job.error = event.message;
          if (event.type === 'frame') job.latestFrame = event;
          if (event.type === 'result' || event.type === 'done') job.result = event;
          record({ ...event, job: id, device });
        } catch { record({ type: 'log', job: id, device, message: line.slice(0, 2000) }); }
      });
      child.stderr.on('data', data => { stderr = (stderr + data).slice(-8000); });
      const timeout = setTimeout(() => { job.error = 'Run exceeded the 30 minute deadline'; job.state = 'stopping'; child.kill(); }, 30 * 60 * 1000);
      child.on('error', error => { job.error = error.message; });
      child.on('close', code => {
        clearTimeout(timeout); lines.close();
        job.state = job.state === 'stopping' && !job.error ? 'stopped' : code === 0 && finished ? 'complete' : 'failed';
        if (job.state === 'failed') job.error ||= stderr || `Runner exited ${code}`;
        active = active.filter(x => x.child !== child);
        record({ type: 'exit', job: id, device, state: job.state, error: job.error });
      });
    }
  }
  function validate(body) {
    if (!body || typeof body !== 'object' || Array.isArray(body)) throw Error('Expected run options');
    if (!['memory', 'memory-train', 'language', 'language-train'].includes(body.mode)) throw Error('Unknown example');
    const allowed = new Set(['cpu', ...(hardware?.devices || []).filter(d => d.usable).map(d => d.id)]);
    if (!Array.isArray(body.devices) || !body.devices.length || body.devices.length > 8 ||
        new Set(body.devices).size !== body.devices.length || body.devices.some(d => !allowed.has(d))) throw Error('Select available devices');
    if (!hardware?.labAvailable) throw Error(`Lab source missing at ${lab}; set NCA_LAB_DIR before starting the server`);
    const int = (key, fallback, min, max) => {
      const value = body[key] ?? fallback;
      if (!Number.isInteger(value) || value < min || value > max) throw Error(`${key} must be ${min}–${max}`);
      return value;
    };
    const prompt = body.prompt ?? 'The ', temperature = body.temperature ?? .3;
    if (typeof prompt !== 'string' || !prompt.length || Buffer.byteLength(prompt) > 512 || prompt.includes('\0')) throw Error('Prompt must contain 1–512 UTF-8 bytes');
    if (typeof temperature !== 'number' || !Number.isFinite(temperature) || temperature < .05 || temperature > 2) throw Error('Temperature must be .05–2');
    return { mode: body.mode, devices: body.devices, prompt, temperature,
      steps: int('steps', 100, 1, 5000), batch: int('batch', 32, 1, 256), length: int('length', 96, 8, 256),
      seed: int('seed', 0, 0, 1000000), bytes: int('bytes', 160, 1, 512), frameMs: int('frameMs', 80, 0, 500) };
  }
  const json = (res, status, data) => { res.writeHead(status, { 'content-type': 'application/json', 'cache-control': 'no-store' }); res.end(JSON.stringify(data)); };
  async function handle(req, res, url) {
    if (!url.pathname.startsWith('/api/nca/')) return false;
    // Host validation also prevents DNS rebinding. Browser mutations require a
    // same-origin token, JSON, and (when present) an exact allowed Origin.
    if (!origins.has(`http://${req.headers.host}`) || (req.headers.origin && !origins.has(req.headers.origin)) ||
        req.headers['sec-fetch-site'] === 'cross-site') { json(res, 403, { error: 'Local same-origin requests only' }); return true; }
    try {
      if (url.pathname === '/api/nca/status' && req.method === 'GET') {
        const [capabilities, gpus] = await Promise.all([probe(), telemetry()]);
        const after = Number(url.searchParams.get('after')) || 0;
        json(res, 200, { ...capabilities, token, gpus, jobs, running: active.length > 0, cursor,
          events: events.filter(e => e.cursor > after) });
      } else if (req.method === 'POST' && ['/api/nca/run', '/api/nca/stop'].includes(url.pathname)) {
        if (req.headers['x-nca-token'] !== token || req.headers['content-type'] !== 'application/json') {
          json(res, 403, { error: 'Reload the lab page before running' }); return true;
        }
        let body = '';
        for await (const chunk of req) { body += chunk; if (Buffer.byteLength(body) > 8192) { json(res, 413, { error: 'Request too large' }); return true; } }
        if (url.pathname.endsWith('/stop')) {
          for (const { child, job } of active) { job.state = 'stopping'; child.kill(); }
          json(res, 200, { stopping: active.length });
        } else {
          if (active.length) { json(res, 409, { error: 'Stop or finish the current experiment first' }); return true; }
          await probe();
          // Another request may have launched while capability probing awaited.
          if (active.length) { json(res, 409, { error: 'An experiment is already running' }); return true; }
          const config = validate(JSON.parse(body)); launch(config); json(res, 202, { jobs });
        }
      } else json(res, 404, { error: 'Unknown lab endpoint' });
    } catch (error) { json(res, 400, { error: error.message }); }
    return true;
  }
  function close() { for (const { child } of active) child.kill(); }
  return { handle, close };
}
module.exports = { createLab };
