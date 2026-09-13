import { createStateRenderer } from './nca-renderer.mjs';
const $ = id => document.getElementById(id);
const explanations = {
  memory: 'Load the learned checkpoint, write fresh associations, then follow a query around eight cells. Shared weights remain frozen.',
  'memory-train': 'Train the actual cellular-memory model on new association episodes, then replay its learned writes and ring query.',
  language: 'Load the supplied byte-language checkpoint and watch recurrent cache activations as it generates template-style text.',
  'language-train': 'Train the actual causal model from scratch on the lab’s separate toy training and validation text, then generate a sample.'
};
let token, cursor = 0, renderer, ready = false, running = false, submitting = false, selectedJob = '', jobs = [], mutation = 0;
const frames = new Map(), losses = new Map(), results = new Map();
let logs = [];
function error(message) { $('error').textContent = message || ''; $('error').hidden = !message; }
try {
  renderer = createStateRenderer($('state'));
  $('renderer').textContent = `Visualization: hardware WebGL2 · ${renderer.description}. Browser chooses one adapter; native compute uses the selected devices.`;
} catch (e) { $('renderer').textContent = e.message; error(e.message); }
function updateMode() {
  const mode = $('mode').value;
  $('explanation').textContent = explanations[mode];
  $('training-options').hidden = !mode.endsWith('-train');
  $('language-options').hidden = !mode.startsWith('language');
  // Hidden controls must not block form validation.
  for (const panel of ['training-options', 'language-options']) for (const input of $(panel).querySelectorAll('input')) input.disabled = $(panel).hidden || running;
}
$('mode').addEventListener('change', updateMode); updateMode();
function buttons() {
  $('run').disabled = !ready || running || submitting;
  $('stop').disabled = !running || submitting;
  for (const input of $('controls').querySelectorAll('input, select')) input.disabled = running || submitting;
  if (!running && !submitting) updateMode();
  for (const input of $('devices').querySelectorAll('[data-unusable]')) input.disabled = true;
}
function renderDevices(data) {
  const list = $('devices'); list.replaceChildren();
  let chosen = false;
  for (const device of [...data.devices, { id: 'cpu', name: 'CPU', usable: true }]) {
    const label = document.createElement('label'); label.className = 'device';
    const input = document.createElement('input'); input.type = 'checkbox'; input.value = device.id;
    input.checked = device.usable && !chosen; if (input.checked) chosen = true;
    input.disabled = !device.usable; if (!device.usable) input.dataset.unusable = 'true';
    const title = document.createElement('span'); title.textContent = `${device.id} · ${device.name}`;
    const detail = document.createElement('small');
    detail.textContent = device.id === 'cpu' ? 'Explicit CPU option' : device.usable ? `${(device.totalMiB / 1024).toFixed(1)} GiB · CUDA kernel verified` : device.error;
    title.append(detail); label.append(input, title); list.append(label);
  }
}
function telemetry(gpus) {
  $('telemetry').replaceChildren();
  if (!gpus.length) { $('telemetry').textContent = 'NVIDIA telemetry unavailable.'; return; }
  for (const gpu of gpus) {
    const card = document.createElement('div'); card.className = 'gpu-card';
    const title = document.createElement('strong'); title.textContent = `${gpu.id} · ${gpu.name}`;
    const meter = document.createElement('div'); meter.className = 'meter'; const bar = document.createElement('div');
    bar.style.width = `${Math.max(0, Math.min(100, gpu.utilization || 0))}%`; meter.append(bar);
    const detail = document.createElement('p'); detail.textContent = `${gpu.utilization ?? '—'}% GPU · ${gpu.usedMiB ?? '—'} / ${gpu.totalMiB ?? '—'} MiB\n${gpu.temperature ?? '—'} °C · ${gpu.watts ?? '—'} W`;
    card.append(title, meter, detail); $('telemetry').append(card);
  }
}
function renderFrame() {
  const job = jobs.find(j => j.id === selectedJob), frame = frames.get(selectedJob);
  if (!job) return;
  const memory = job.mode.startsWith('memory');
  $('view-title').textContent = memory ? 'Private memory · cell × channel' : frame?.phase === 'training' ? 'Recurrent activations · channel × byte position' : 'Incremental cache · stage × channel';
  if (frame) {
    if (renderer) {
      try { renderer.draw(frame, memory); $('empty').hidden = true; }
      catch (e) { error(e.message); renderer = null; $('empty').hidden = false; $('empty').textContent = 'Visualization unavailable. Native compute continues.'; }
    }
    $('frame-status').textContent = `${job.device} · ${frame.phase} ${frame.step}${frame.cell != null ? ` · cell ${frame.cell} · key ${frame.key}` : ''}`;
    if (frame.text != null) $('output').textContent = frame.text;
  }
  const points = losses.get(selectedJob) || [];
  if (points.length) {
    const low = Math.min(...points.map(p => p.loss)), high = Math.max(...points.map(p => p.loss));
    const maxStep = Math.max(2, points.at(-1).step);
    $('loss-line').setAttribute('points', points.map(p => `${5 + (p.step - 1) / (maxStep - 1) * 490},${110 - (p.loss - low) / Math.max(.001, high - low) * 100}`).join(' '));
    $('loss-value').textContent = points.at(-1).loss.toFixed(4);
    $('loss-caption').textContent = `Steps 1–${points.at(-1).step} · loss ${low.toFixed(3)}–${high.toFixed(3)} · ${memory ? 'binary cross-entropy' : 'nats per byte'}`;
  } else {
    $('loss-line').setAttribute('points', ''); $('loss-value').textContent = '—'; $('loss-caption').textContent = 'A curve appears during training.';
  }
  const result = results.get(selectedJob);
  if (result) $('output').textContent = result.text ?? `Observed: ${result.observed}\nAnswer:   ${result.answer}\nWriter ${result.writer} → reader ${result.reader}\nFrozen shared weights: ${result.sharedWeightsUnchanged ? 'verified' : 'NO'}\nBit accuracy: ${(result.bit_accuracy * 100).toFixed(1)}%`;
  else if (!frame?.text) $('output').textContent = `${job.device} · ${job.state}${job.error ? `\n${job.error}` : ''}`;
}
function updateJobs(next) {
  const changed = next.map(j => j.id).join() !== jobs.map(j => j.id).join(); jobs = next;
  if (changed) {
    const ids = new Set(jobs.map(j => j.id));
    for (const map of [frames, losses, results]) for (const key of map.keys()) if (!ids.has(key)) map.delete(key);
    $('view-job').replaceChildren();
    for (const job of jobs) { const option = document.createElement('option'); option.value = job.id; option.textContent = `${job.device} · seed ${job.seed}`; $('view-job').append(option); }
    if (!jobs.some(j => j.id === selectedJob)) selectedJob = jobs[0]?.id || '';
    $('view-job').value = selectedJob;
  }
  for (const job of jobs) {
    if (job.latestFrame) frames.set(job.id, job.latestFrame);
    if (job.result) results.set(job.id, job.result);
  }
}
$('view-job').addEventListener('change', () => { selectedJob = $('view-job').value; renderFrame(); });
async function api(endpoint, body) {
  const response = await fetch(`/api/nca/${endpoint}`, { cache: 'no-store', signal: AbortSignal.timeout(75000),
    ...(body === undefined ? {} : { method: 'POST', headers: { 'content-type': 'application/json', 'x-nca-token': token }, body: JSON.stringify(body) }) });
  if (response.status === 404) throw Error('Start the local bridge with node crates/zipp-wasm/playground/serve.cjs, then open this page from that server.');
  const data = await response.json(); if (!response.ok) throw Error(data.error || response.statusText); return data;
}
async function poll() {
  const stamp = mutation;
  try {
    const data = await api(`status?after=${cursor}`);
    if (stamp !== mutation) { setTimeout(poll, 180); return; }
    // A restarted server has a new token and cursor sequence.
    if (token && data.token !== token) { cursor = 0; ready = false; frames.clear(); losses.clear(); results.clear(); }
    token = data.token; cursor = data.cursor;
    if (!ready) {
      if (data.error) throw Error(data.error);
      renderDevices(data); ready = !!data.labAvailable;
      $('lab-path').textContent = data.lab; $('torch-version').textContent = `PyTorch ${data.torch} · CUDA ${data.cuda || 'unavailable'}`;
      if (!ready) error('Lab source is missing. Set NCA_LAB_DIR and restart the server.');
    }
    running = data.running; updateJobs(data.jobs); telemetry(data.gpus);
    for (const event of data.events) {
      if (event.type === 'frame') {
        frames.set(event.job, event);
        if (event.loss != null) {
          if (!losses.has(event.job)) losses.set(event.job, []);
          losses.get(event.job).push({ step: event.step, loss: event.loss });
        }
      } else {
        if (event.type === 'result' || event.type === 'done') results.set(event.job, event);
        logs.push(`${event.device} ${event.type}: ${JSON.stringify(event)}`);
        if (event.type === 'error' || event.error) error(event.message || event.error);
      }
    }
    logs = logs.slice(-60); $('events').textContent = logs.join('\n');
    $('connection').textContent = running ? `● ${jobs.filter(j => ['starting', 'running', 'stopping'].includes(j.state)).length} active device(s)` : jobs.length ? jobs.map(j => `${j.device}: ${j.state}`).join(' · ') : '● Local runner connected';
    buttons(); renderFrame();
  } catch (e) { ready = false; $('connection').textContent = 'Runner unavailable'; error(e.message); buttons(); }
  setTimeout(poll, running ? 180 : 1800);
}
$('controls').addEventListener('submit', async event => {
  event.preventDefault(); mutation++; error(''); submitting = true; buttons();
  try {
    const data = await api('run', {
      mode: $('mode').value, devices: [...$('devices').querySelectorAll('input:checked')].map(i => i.value),
      steps: Number($('steps').value), batch: Number($('batch').value),
      prompt: $('prompt').value, bytes: Number($('bytes').value), temperature: Number($('temperature').value), frameMs: 100
    });
    frames.clear(); losses.clear(); results.clear(); logs = [];
    $('output').textContent = 'Starting Python and loading the model…'; $('empty').hidden = false;
    if (renderer) renderer.draw(null);
    updateJobs(data.jobs); running = true;
  } catch (e) { error(e.message); }
  finally { mutation++; submitting = false; buttons(); }
});
$('stop').addEventListener('click', async () => {
  mutation++; submitting = true; buttons();
  try { await api('stop', {}); } catch (e) { error(e.message); }
  finally { mutation++; submitting = false; buttons(); }
});
poll();
