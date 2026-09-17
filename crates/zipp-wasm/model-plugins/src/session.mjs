import {check, fields, integer, checkHash, sha256, resolveLimits, abortCheck} from './common.mjs';
import {parseJSON} from './json.mjs';
import {readJSON, readAll} from './sources.mjs';
import {openSafetensors, WeightStore} from './safetensors.mjs';
import {bindGraph} from './bindings.mjs';

function validateModel(manifest, plugin, limits) {
  fields(manifest, ['format', 'version', 'architecture', 'config', 'tokenizer', 'weights', 'license'],
    ['format', 'version', 'architecture', 'config', 'tokenizer', 'weights']);
  check(manifest.format === 'zipp.local-model' && manifest.version === 1, 'VERSION', 'Unsupported local model manifest');
  fields(manifest.architecture, ['id', 'version', 'sha256'], ['id', 'version', 'sha256']);
  checkHash(manifest.architecture.sha256);
  const a = manifest.architecture, p = plugin.identity;
  check(a.id === p.id && a.version === p.version && a.sha256 === p.sha256, 'PLUGIN', 'Model requires a different exact plugin identity; no automatic download');
  check(manifest.config && typeof manifest.config === 'object' && !Array.isArray(manifest.config), 'FORMAT', 'Expected model configuration');
  check(manifest.tokenizer && typeof manifest.tokenizer === 'object' && !Array.isArray(manifest.tokenizer), 'FORMAT', 'Expected tokenizer configuration');
  check(Array.isArray(manifest.weights), 'FORMAT', 'Expected weight shard list');
  integer(manifest.weights.length, 1, limits.maxWeightFiles, 'Weight file count');
  const paths = new Set();
  for (const item of manifest.weights) {
    fields(item, ['path', 'sha256'], ['path']);
    check(typeof item.path === 'string' && item.path.endsWith('.safetensors') && !paths.has(item.path), 'FORMAT', 'Expected unique Safetensors paths'); paths.add(item.path);
    if (item.sha256 !== undefined) checkHash(item.sha256);
  }
  return manifest;
}
function parseReply(reply, limit) {
  check(typeof reply === 'string', 'PLUGIN', 'Plugin hook must return a JSON string');
  return parseJSON(reply, {maxChars: limit});
}
export function seededRandom(seed = 1) {
  integer(seed, 0, 0xffffffff, 'Sampling seed'); let state = seed >>> 0;
  return () => { state = (Math.imul(state, 1664525) + 1013904223) >>> 0; return state / 4294967296; };
}
export function sampleLogits(logits, {temperature = 0, topK = 0, random = Math.random} = {}) {
  check(logits.length > 0, 'SHAPE', 'Empty logits');
  check(Number.isFinite(temperature) && temperature >= 0 && temperature <= 100, 'SAMPLING', 'Invalid temperature');
  integer(topK, 0, logits.length, 'topK');
  let best = 0;
  for (let i = 0; i < logits.length; i++) { check(Number.isFinite(logits[i]), 'NUMBER', 'Non-finite logits'); if (logits[i] > logits[best]) best = i; }
  if (temperature === 0) return best;
  let indices = Array.from({length: logits.length}, (_, i) => i);
  if (topK > 0 && topK < logits.length) indices = indices.sort((a, b) => logits[b] - logits[a] || a - b).slice(0, topK);
  // Subtract before dividing: even a tiny positive temperature cannot overflow
  // the maximum logit to +Infinity. Negative differences may underflow to -Inf.
  const probs = indices.map(i => Math.exp((logits[i] - logits[best]) / temperature));
  const sum = probs.reduce((a, b) => a + b, 0), r = random();
  check(Number.isFinite(r) && r >= 0 && r < 1, 'SAMPLING', 'Random source must return [0,1)'); let pick = r * sum;
  for (let i = 0; i < indices.length; i++) { pick -= probs[i]; if (pick < 0) return indices[i]; }
  return indices[indices.length - 1];
}

/** One approved Python plugin and one local model per engine/session.
 * Owns engine + model cache, borrows compute runtime. A Worker must own this
 * instance: a Promise timeout cannot interrupt synchronous guest execution.
 */
export class ModelSession {
  #engine; #runtime; #weights; #model; #plugin; #limits; #description; #busy = false; #closed = false;
  static async open({source, plugin, engineFactory, runtime, limits: overrides = {}}) {
    const limits = resolveLimits(overrides);
    check(plugin?.identity && typeof engineFactory === 'function' && typeof runtime?.execute === 'function', 'HOST', 'Supply installed plugin, engine factory and ZIPP compute runtime');
    const model = validateModel(await readJSON(source, 'model.json', limits.maxManifestBytes), plugin, limits);
    let total = 0;
    for (const item of model.weights) { const bytes = source.size(item.path); integer(bytes, 8, limits.maxModelFileBytes, 'Weight file bytes'); total += bytes; integer(total, 8, limits.maxModelBytes, 'Aggregate weight bytes'); }
    const indexes = [];
    for (const item of model.weights) {
      // Optional content integrity costs a whole-file read in this first version.
      if (item.sha256 !== undefined) check(await sha256(await readAll(source, item.path, limits.maxModelFileBytes)) === item.sha256, 'HASH', 'Weight digest mismatch');
      indexes.push(await openSafetensors(source, item.path, limits));
    }
    const weights = new WeightStore(indexes, limits); let engine;
    try {
      engine = engineFactory();
      engine.setSyncHostCapabilities([]); engine.setInstructionBudget(limits.instructionBudget);
      engine.initPythonProject({...plugin.files}, plugin.entry, []);
      const session = new ModelSession(engine, runtime, weights, model, plugin, limits);
      const d = session.#jsonCall('zipp_model_describe', [JSON.stringify(model.config)]);
      fields(d, ['task', 'vocab_size', 'max_context'], ['task', 'vocab_size', 'max_context']);
      check(d.task === 'causal-lm', 'TASK', 'This initial session driver supports causal language model plugins');
      integer(d.vocab_size, 2, limits.maxDimension, 'Vocabulary size'); integer(d.max_context, 1, limits.maxContext, 'Context size');
      integer(model.tokenizer.eos_token_id, 0, d.vocab_size - 1, 'EOS token');
      session.#description = Object.freeze(d); return session;
    } catch (error) { weights.dispose(); try { engine?.dispose(); } catch {} throw error; }
  }
  constructor(engine, runtime, weights, model, plugin, limits) {
    this.#engine = engine; this.#runtime = runtime; this.#weights = weights; this.#model = model; this.#plugin = plugin; this.#limits = limits;
  }
  get info() { return {plugin: {...this.#plugin.identity}, ...this.#description, decodedWeightBytes: this.#weights.decodedBytes, backend: this.#runtime.info?.().backend ?? 'host-supplied'}; }
  #live() { check(!this.#closed && !this.#engine.disposed, 'DISPOSED', 'Model session is closed'); }
  #call(name, args) { this.#live(); this.#engine.renewInstructionBudget(); return this.#engine.pythonCall(name, args); }
  #jsonCall(name, args) { return parseReply(this.#call(name, args), this.#limits.maxGraphBytes); }
  #tokens(tokens) {
    check(Array.isArray(tokens) && tokens.length > 0 && tokens.length <= this.#description.max_context, 'CONTEXT', 'Token context is empty or exceeds the model context');
    for (const t of tokens) integer(t, 0, this.#description.vocab_size - 1, 'Token id');
  }
  async #infer(tokens, signal) {
    this.#tokens(tokens); abortCheck(signal);
    const template = this.#jsonCall('zipp_model_graph', [JSON.stringify(this.#model.config), JSON.stringify(tokens)]);
    const graph = await bindGraph(template, this.#weights, this.#limits); abortCheck(signal); this.#live();
    const result = await this.#runtime.execute(graph, {typedOutputs: true}); abortCheck(signal); this.#live();
    const output = result.outputs?.logits;
    check(output && Array.isArray(output.shape) && output.shape.length === 2 &&
      (output.shape[0] === tokens.length || output.shape[0] === 1) && output.shape[1] === this.#description.vocab_size,
      'SHAPE', 'Plugin must return logits shaped [context or 1, vocab_size]');
    check(output.data instanceof Float32Array && output.data.length === output.shape[0] * output.shape[1], 'SHAPE', 'Expected typed logits data');
    const logits = output.data.slice(-this.#description.vocab_size);
    for (const v of logits) check(Number.isFinite(v), 'NUMBER', 'Non-finite model logits');
    return {logits, stats: result.stats, backend: result.backend};
  }
  async infer(tokens, {signal} = {}) {
    this.#live(); check(!this.#busy, 'BUSY', 'Await the previous model operation'); this.#busy = true;
    try { return await this.#infer([...tokens], signal); } finally { this.#busy = false; }
  }
  async generate(prompt, {maxNewTokens = 32, temperature = 0, topK = 0, seed = 1, signal, onToken} = {}) {
    this.#live(); check(!this.#busy, 'BUSY', 'Await the previous model operation');
    check(typeof prompt === 'string' && prompt.length <= this.#limits.maxPromptChars, 'LIMIT', 'Prompt exceeds text limit');
    integer(maxNewTokens, 0, this.#limits.maxNewTokens, 'maxNewTokens');
    const random = seededRandom(seed); this.#busy = true;
    try {
      abortCheck(signal);
      const tokens = this.#jsonCall('zipp_model_encode', [prompt, JSON.stringify(this.#model.tokenizer)]); this.#tokens(tokens);
      const generated = []; let text = '', finishReason = 'length', lastStats = null, backend = this.info.backend;
      for (let i = 0; i < maxNewTokens; i++) {
        if (tokens.length > this.#description.max_context) { finishReason = 'context'; break; }
        const out = await this.#infer(tokens, signal); lastStats = out.stats; backend = out.backend;
        const token = sampleLogits(out.logits, {temperature, topK, random});
        if (token === this.#model.tokenizer.eos_token_id) { finishReason = 'eos'; break; }
        generated.push(token); tokens.push(token);
        // Decode the complete generated prefix. This also supports tokenizers
        // whose byte pieces cannot be decoded correctly one token at a time.
        text = this.#call('zipp_model_decode', [JSON.stringify(generated), JSON.stringify(this.#model.tokenizer)]);
        check(typeof text === 'string' && text.length <= this.#limits.maxPromptChars * 4, 'LIMIT', 'Decoded text exceeds output limit');
        if (onToken) await onToken({token, text, count: generated.length, backend});
        abortCheck(signal);
      }
      return {text, tokens: generated, finishReason, backend, stats: lastStats};
    } finally { this.#busy = false; }
  }
  dispose() {
    check(!this.#busy, 'BUSY', 'Await work before disposing; terminate the owning Worker to hard-cancel');
    if (this.#closed) return;
    this.#closed = true; this.#weights.dispose(); this.#engine.dispose();
  }
}
