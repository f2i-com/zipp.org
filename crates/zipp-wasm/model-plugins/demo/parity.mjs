// Cross-backend numerical acceptance, in the browser that will actually run the
// model. It replays the graph plans the Python plugin emitted (recorded in
// examples/tiny-char/graph-cases.json) through every backend this browser
// offers, and compares complete logit tensors with the stored PyTorch outputs.
// No Python engine is involved: this isolates the compute path from the plugin.
import {createRuntime} from '../../gpu-lab/src/runtime.mjs';
import {FileMapSource, openSafetensors, WeightStore, bindGraph, resolveLimits, fetchBytes} from '../src/index.mjs';

const $ = id => document.getElementById(id);
const root = new URL('../', import.meta.url);
const BACKENDS = ['webgpu', 'webgl2', 'wasm', 'cpu-js'];

async function assets() {
  const model = new URL('examples/tiny-char/', root);
  const weights = await fetchBytes(new URL('weights.safetensors', model), {maxBytes: 4 * 1024 * 1024});
  const cases = JSON.parse(new TextDecoder().decode(await fetchBytes(new URL('graph-cases.json', model), {maxBytes: 4 * 1024 * 1024})));
  const oracle = JSON.parse(new TextDecoder().decode(await fetchBytes(new URL('oracle.json', model), {maxBytes: 4 * 1024 * 1024})));
  const source = new FileMapSource(new Map([['weights.safetensors', weights]]));
  const limits = resolveLimits();
  // Unwrap deliberately, then insist there is something to compare: a fixture
  // read as an object rather than its array would otherwise loop zero times and
  // report a perfect score for every backend.
  const plans = cases.cases, expected = oracle.cases;
  if (!Array.isArray(plans) || plans.length === 0) throw Error('graph-cases.json has no cases');
  if (!Array.isArray(expected) || expected.length !== plans.length) throw Error('oracle.json does not match the graph fixtures');
  return {cases: plans, oracle: expected, limits, store: new WeightStore([await openSafetensors(source, 'weights.safetensors', limits)], limits)};
}

function worstError(actual, expected) {
  if (actual.length !== expected.length) throw Error(`logit count ${actual.length} != ${expected.length}`);
  let worst = 0;
  for (let i = 0; i < actual.length; i++) worst = Math.max(worst, Math.abs(actual[i] - expected[i]));
  return worst;
}

$('run').addEventListener('click', async () => {
  const tolerance = Number($('tolerance').value);
  if (!Number.isFinite(tolerance) || tolerance <= 0) { $('status').textContent = 'Give a positive tolerance.'; return; }
  $('run').disabled = true; $('table').textContent = ''; $('details').textContent = '';
  const rows = [], notes = [];
  try {
    $('status').textContent = 'Reading the fixture…';
    const {cases, oracle, limits, store} = await assets();
    try {
      for (const backend of BACKENDS) {
        $('status').textContent = `Running ${backend}…`;
        let runtime = null;
        try {
          // An explicit request must fail rather than quietly land elsewhere.
          runtime = await createRuntime({backend, wasmUrl: new URL('../../gpu-lab/wasm/kernels.wasm', import.meta.url)});
          if (runtime.info().backend !== backend) throw Error(`asked for ${backend}, got ${runtime.info().backend}`);
          let worst = 0;
          const started = performance.now();
          for (let i = 0; i < cases.length; i++) {
            const graph = await bindGraph(cases[i].template, store, limits);
            const result = await runtime.execute(graph, {typedOutputs: true});
            worst = Math.max(worst, worstError(result.outputs.logits.data, oracle[i].logits.flat()));
          }
          const ms = performance.now() - started;
          rows.push({backend, worst, ms, verdict: worst <= tolerance ? 'PASS' : 'FAIL'});
          notes.push(`${backend}: ${runtime.info().description ?? ''}`.trim());
        } catch (error) {
          rows.push({backend, verdict: 'unavailable', reason: String(error.message || error)});
        } finally { runtime?.dispose(); }
      }
    } finally { store.dispose(); }
    const width = Math.max(...rows.map(r => r.backend.length));
    $('table').textContent = rows.map(r => r.verdict === 'unavailable'
      ? `${r.backend.padEnd(width)}  unavailable in this browser — ${r.reason}`
      : `${r.backend.padEnd(width)}  ${r.verdict}  max |Δlogit| ${r.worst.toExponential(3)}  ${r.ms.toFixed(0)} ms for ${cases.length} prompts`).join('\n');
    const compared = rows.filter(r => r.verdict !== 'unavailable');
    const failed = compared.filter(r => r.verdict === 'FAIL').length;
    $('status').textContent = compared.length === 0 ? 'No backend was available to compare.'
      : `${compared.length} backend(s) compared against PyTorch, ${failed} outside tolerance.`;
    $('details').textContent = [`tolerance ${tolerance}`, `prompts ${cases.length}`, `vocabulary ${oracle[0].logits.at(-1).length}`, ...notes].join('\n');
  } catch (error) {
    $('status').textContent = `${error.code || 'ERROR'}: ${error.message || error}`;
  } finally { $('run').disabled = false; }
});
