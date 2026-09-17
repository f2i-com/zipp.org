/** Host-side data validation. No guest string is evaluated as JavaScript. */
export class ModelError extends Error {
  constructor(code, message) { super(message); this.name = 'ModelError'; this.code = code; }
}
export function check(ok, code, message) { if (!ok) throw new ModelError(code, message); }
export function plain(value) {
  return value !== null && typeof value === 'object' &&
    [Object.prototype, null].includes(Object.getPrototypeOf(value));
}
export function fields(value, allowed, required = []) {
  check(plain(value), 'FORMAT', 'Expected a plain object');
  for (const key of Reflect.ownKeys(value)) {
    check(typeof key === 'string' && allowed.includes(key), 'FORMAT', `Unknown field: ${String(key)}`);
    check('value' in Object.getOwnPropertyDescriptor(value, key), 'FORMAT', 'Accessors are not data');
  }
  for (const key of required) check(Object.hasOwn(value, key), 'FORMAT', `Missing field: ${key}`);
}
export function integer(value, min, max, what) {
  check(Number.isSafeInteger(value) && value >= min && value <= max, 'LIMIT', `${what} must be an integer in [${min}, ${max}]`);
  return value;
}
export function safePath(path) {
  check(typeof path === 'string' && path.length > 0 && path.length <= 512, 'PATH', 'Invalid asset path');
  const parts = path.split('/');
  check(parts.every(p => /^[A-Za-z0-9_][A-Za-z0-9_.-]*$/.test(p) && p !== '.' && p !== '..'),
    'PATH', 'Use a relative, canonical ASCII asset path without dot segments');
  return path;
}
export function shapeSize(shape, limits, allowEmpty = false) {
  check(Array.isArray(shape) && shape.length <= 4, 'SHAPE', 'Only rank 0 through 4 is supported');
  let size = 1;
  for (const d of shape) {
    integer(d, allowEmpty ? 0 : 1, limits.maxDimension, 'Tensor dimension');
    size *= d;
    integer(size, 0, limits.maxTensorElements, 'Tensor elements');
  }
  return size;
}
export const DEFAULT_LIMITS = Object.freeze({
  maxManifestBytes: 256 * 1024, maxHeaderBytes: 4 * 1024 * 1024,
  maxSourceFileBytes: 1024 * 1024, maxSourceBytes: 4 * 1024 * 1024,
  maxSourceFiles: 64, maxWeightFiles: 16, maxTensors: 4096,
  maxModelFileBytes: 128 * 1024 * 1024, maxModelBytes: 256 * 1024 * 1024,
  maxDecodedBytes: 64 * 1024 * 1024, maxBoundInputBytes: 64 * 1024 * 1024,
  maxTensorElements: 4194304, maxDimension: 65536,
  maxGraphBytes: 4 * 1024 * 1024, maxNodes: 512,
  maxContext: 512, maxNewTokens: 128, maxPromptChars: 16384,
  instructionBudget: 50000000,
});
export function resolveLimits(overrides = {}) {
  fields(overrides, Object.keys(DEFAULT_LIMITS));
  for (const [k, v] of Object.entries(overrides)) integer(v, 1, Number.MAX_SAFE_INTEGER, k);
  return Object.freeze({...DEFAULT_LIMITS, ...overrides});
}
export function sameShape(a, b) { return a.length === b.length && a.every((d, i) => d === b[i]); }
export function checkHash(hash) { check(typeof hash === 'string' && /^[a-f0-9]{64}$/.test(hash), 'HASH', 'Expected lowercase SHA-256'); }
export async function sha256(bytes) {
  check(globalThis.crypto?.subtle, 'HOST', 'Web Crypto is required (HTTPS or localhost)');
  const digest = await crypto.subtle.digest('SHA-256', bytes);
  return [...new Uint8Array(digest)].map(n => n.toString(16).padStart(2, '0')).join('');
}
export function abortCheck(signal) {
  if (signal?.aborted) throw new ModelError('ABORTED', 'Generation cancelled');
}
