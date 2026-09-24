// The WebGPU API subset gpu-lab's WebGPU backend uses, over wgpu.
//
// Every object here is a handle: the real adapter, device, buffers, shaders,
// layouts, pipelines, bind groups and command buffers live in the native host
// (crates/zipp-gpu/src/device.rs), reached through `__zippHostCall(kind, ...)`
// with string arguments. A command encoder records its compute passes and
// copies here and hands them to the host in ONE call at `finish()`, where
// they are replayed onto a wgpu command encoder, so recording a dispatch
// costs no host round trip.
//
// Bytes cross through two fixed typed arrays, `__zgpuUp` (script to device)
// and `__zgpuDown` (device to script), which the host reaches as memory
// regions. They are top-level `var`s because the host resolves them by global
// name, and they are reused (grown geometrically) because a region the host
// has resolved stays pinned for the engine's lifetime.
//
// Every promise the API returns is settled before it is returned: the host
// blocks on the device where WebGPU would suspend (a map, a queue drain, an
// error scope, an asynchronous pipeline), so the whole runtime's `await`
// chain finishes inside one engine call's microtask drain.
var __zgpuUp = new Uint8Array(1 << 16);
var __zgpuDown = new Uint8Array(1 << 16);
var __zgpuShim = (function () {
  "use strict";
  const host = __zippHostCall;
  const BUFFER_USAGE = Object.freeze({MAP_READ: 1, MAP_WRITE: 2, COPY_SRC: 4, COPY_DST: 8, INDEX: 16, VERTEX: 32,
    UNIFORM: 64, STORAGE: 128, INDIRECT: 256, QUERY_RESOLVE: 512});
  const MAP_MODE = Object.freeze({READ: 1, WRITE: 2});
  const SHADER_STAGE = Object.freeze({VERTEX: 1, FRAGMENT: 2, COMPUTE: 4});
  // A failed host call (a refused descriptor, an unknown object) is a thrown
  // JavaScript error, as the browser's own synchronous validation is.
  const settle = (fn) => { try { return Promise.resolve(fn()); } catch (e) { return Promise.reject(e); } };
  const operationError = (message) => { const e = new Error(message); e.name = 'OperationError'; return e; };
  // "LOST:<message>" from a blocking call: the device is gone.
  function checkLost(device, reply) {
    if (typeof reply === 'string' && reply.startsWith('LOST:')) {
      const message = reply.slice(5);
      if (device._lostResolve) { const r = device._lostResolve; device._lostResolve = null; r({reason: 'unknown', message}); }
      return message;
    }
    return null;
  }
  function upload(view) {
    if (view.byteLength > __zgpuUp.length) {
      let size = __zgpuUp.length;
      while (size < view.byteLength) size *= 2;
      __zgpuUp = new Uint8Array(size);
    }
    __zgpuUp.set(view);
  }
  function download(size) {
    if (size > __zgpuDown.length) {
      let next = __zgpuDown.length;
      while (next < size) next *= 2;
      __zgpuDown = new Uint8Array(next);
    }
  }
  function bytesOf(data, offset, size, what) {
    // An ArrayBuffer is addressed in bytes, a typed array in elements.
    if (data instanceof ArrayBuffer) {
      const length = size === undefined ? data.byteLength - offset : size;
      return new Uint8Array(data, offset, length);
    }
    if (ArrayBuffer.isView(data)) {
      const unit = data.BYTES_PER_ELEMENT || 1, count = data.byteLength / unit;
      const length = size === undefined ? count - offset : size;
      if (offset + length > count) throw new RangeError(what + ': data range out of bounds');
      return new Uint8Array(data.buffer, data.byteOffset + offset * unit, length * unit);
    }
    throw new TypeError(what + ': data must be an ArrayBuffer or a typed array');
  }
  // Bind groups are cheap and gpu-lab drops its cache of them now and then;
  // the native objects follow the script's when the engine collects them.
  const dropped = typeof FinalizationRegistry === 'function'
    ? new FinalizationRegistry((held) => { try { host('gpu.drop', held[0], held[1], held[2]); } catch (e) { /* device gone */ } })
    : null;
  const tracked = (device, kind, id) => {
    const object = {_id: id};
    if (dropped !== null) dropped.register(object, [device._id, kind, id]);
    return object;
  };
  const idOf = (object, what) => {
    if (object === null || typeof object !== 'object' || typeof object._id !== 'number') throw new TypeError(what + ' is not a WebGPU object');
    return object._id;
  };

  class GPUBuffer {
    constructor(device, id, size, usage, mapped) {
      this._device = device; this._id = id; this.size = size; this.usage = usage; this.label = '';
      this.mapState = mapped ? 'mapped' : 'unmapped';
      // mappedAtCreation: the script writes this shadow, unmap uploads it.
      this._shadow = mapped ? new ArrayBuffer(size) : null;
      this._mapped = null; this._mappedOffset = 0;
    }
    mapAsync(mode, offset = 0, size) {
      return settle(() => {
        if (this.mapState !== 'unmapped') throw operationError('Buffer is already mapped or pending');
        const length = size === undefined ? Math.max(0, this.size - offset) : size;
        if (mode !== MAP_MODE.READ) throw operationError('Only MAP_READ mapping is supported');
        download(length);
        const reply = host('gpu.bufferMap', this._device._id, this._id, offset, length);
        const lost = checkLost(this._device, reply);
        if (lost !== null) throw operationError('Device lost: ' + lost);
        if (reply !== 'ok') throw operationError(String(reply).replace(/^ERR:/, ''));
        this._mapped = __zgpuDown.slice(0, length).buffer; this._mappedOffset = offset;
        this.mapState = 'mapped';
        return undefined;
      });
    }
    getMappedRange(offset, size) {
      if (this.mapState !== 'mapped') throw operationError('Buffer is not mapped');
      if (this._shadow !== null) {
        const start = offset === undefined ? 0 : offset, length = size === undefined ? this.size - start : size;
        if (start === 0 && length === this.size) return this._shadow;
        // A partial range of a buffer mapped at creation: a view the unmap
        // still sees would need a shared buffer; gpu-lab maps whole buffers.
        throw operationError('Partial ranges of a buffer mapped at creation are not supported');
      }
      const base = this._mappedOffset, start = offset === undefined ? base : offset;
      const length = size === undefined ? this._mapped.byteLength - (start - base) : size;
      if (start < base || start - base + length > this._mapped.byteLength) throw operationError('Range is outside the mapped range');
      if (start === base && length === this._mapped.byteLength) return this._mapped;
      return this._mapped.slice(start - base, start - base + length);
    }
    unmap() {
      if (this.mapState === 'unmapped') return;
      if (this._shadow !== null) {
        upload(new Uint8Array(this._shadow));
        host('gpu.bufferUnmap', this._device._id, this._id, this.size);
        this._shadow = null;
      } else {
        host('gpu.bufferUnmap', this._device._id, this._id, -1);
        this._mapped = null;
      }
      this.mapState = 'unmapped';
    }
    destroy() {
      if (this._destroyed) return;
      this._destroyed = true; this._shadow = null; this._mapped = null; this.mapState = 'unmapped';
      host('gpu.bufferDestroy', this._device._id, this._id);
    }
  }

  class GPUComputePassEncoder {
    constructor(encoder) { this._encoder = encoder; this._ops = encoder._ops; this._ended = false; }
    setPipeline(pipeline) { this._ops.push('S' + idOf(pipeline, 'pipeline')); }
    setBindGroup(index, group, offsets) {
      let op = 'B' + index + ',' + idOf(group, 'bind group');
      // Recorded by id: keep the object reachable until finish() has
      // replayed it, or a collection (gpu-lab drops its group cache
      // mid-execution) could release the native group first.
      this._encoder._held.push(group);
      if (offsets) for (let i = 0; i < offsets.length; i++) op += ',' + (offsets[i] >>> 0);
      this._ops.push(op);
    }
    dispatchWorkgroups(x, y = 1, z = 1) { this._ops.push('D' + (x >>> 0) + ',' + (y >>> 0) + ',' + (z >>> 0)); }
    end() { if (!this._ended) { this._ended = true; this._ops.push('E'); } }
  }
  class GPUCommandEncoder {
    constructor(device) { this._device = device; this._ops = []; this._held = []; this._pass = null; this._finished = false; }
    beginComputePass() {
      if (this._pass && !this._pass._ended) throw operationError('A compute pass is already open');
      this._ops.push('P');
      this._pass = new GPUComputePassEncoder(this);
      return this._pass;
    }
    copyBufferToBuffer(src, a, b, c, d) {
      // (src, srcOffset, dst, dstOffset, size) or the newer (src, dst, size).
      let srcOffset = a, dst = b, dstOffset = c, size = d;
      if (typeof a === 'object') { dst = a; srcOffset = 0; dstOffset = 0; size = b === undefined ? src.size : b; }
      this._ops.push('C' + idOf(src, 'source') + ',' + srcOffset + ',' + idOf(dst, 'destination') + ',' + dstOffset + ',' + size);
    }
    finish() {
      if (this._finished) throw operationError('Encoder already finished');
      if (this._pass && !this._pass._ended) throw operationError('A compute pass is still open');
      this._finished = true;
      const commands = {_id: Number(host('gpu.finish', this._device._id, this._ops.join(';')))};
      this._held = [];
      return commands;
    }
  }
  class GPUQueue {
    constructor(device) { this._device = device; }
    writeBuffer(buffer, bufferOffset, data, dataOffset = 0, size) {
      const bytes = bytesOf(data, dataOffset, size, 'writeBuffer');
      if (bytes.byteLength === 0) return;
      upload(bytes);
      host('gpu.writeBuffer', this._device._id, idOf(buffer, 'buffer'), bufferOffset, bytes.byteLength);
    }
    submit(commandBuffers) {
      const ids = [];
      for (const c of commandBuffers) ids.push(idOf(c, 'command buffer'));
      host('gpu.submit', this._device._id, ids.join(','));
    }
    onSubmittedWorkDone() {
      return settle(() => {
        const lost = checkLost(this._device, host('gpu.done', this._device._id));
        if (lost !== null) throw operationError('Device lost: ' + lost);
        return undefined;
      });
    }
  }
  class GPUDevice {
    constructor(id, limits) {
      this._id = id; this.limits = Object.freeze(limits); this.features = new Set(); this.queue = new GPUQueue(this); this.label = '';
      this._lostResolve = null;
      this.lost = new Promise(resolve => { this._lostResolve = resolve; });
      this._destroyed = false;
    }
    createBuffer({size, usage, mappedAtCreation = false}) {
      const id = Number(host('gpu.buffer', this._id, size, usage, mappedAtCreation ? 1 : 0));
      return new GPUBuffer(this, id, size, usage, !!mappedAtCreation);
    }
    createShaderModule({code}) { return {_id: Number(host('gpu.shader', this._id, String(code)))}; }
    createBindGroupLayout({entries}) {
      const list = entries.map(e => {
        const b = e.buffer;
        if (!b) throw new TypeError('Only buffer bindings are supported');
        return [e.binding, e.visibility, b.type || 'uniform', b.hasDynamicOffset ? 1 : 0, b.minBindingSize || 0];
      });
      return {_id: Number(host('gpu.bindGroupLayout', this._id, JSON.stringify(list)))};
    }
    createPipelineLayout({bindGroupLayouts}) {
      return {_id: Number(host('gpu.pipelineLayout', this._id, bindGroupLayouts.map(l => idOf(l, 'layout')).join(',')))};
    }
    _pipeline(descriptor) {
      const layout = descriptor.layout, compute = descriptor.compute;
      if (!layout || layout === 'auto') throw new TypeError('An explicit pipeline layout is required');
      return host('gpu.pipeline', this._id, idOf(layout, 'layout'), idOf(compute.module, 'module'), compute.entryPoint || 'main', String(descriptor.label || ''));
    }
    createComputePipeline(descriptor) {
      const reply = this._pipeline(descriptor);
      if (String(reply).startsWith('ERR:')) throw operationError(String(reply).slice(4));
      return {_id: Number(reply)};
    }
    createComputePipelineAsync(descriptor) {
      return settle(() => {
        const reply = this._pipeline(descriptor);
        if (String(reply).startsWith('ERR:')) { const e = new Error(String(reply).slice(4)); e.name = 'GPUPipelineError'; throw e; }
        return {_id: Number(reply)};
      });
    }
    createBindGroup({layout, entries}) {
      const list = entries.map(e => {
        const r = e.resource, buffer = r && r.buffer ? r.buffer : null;
        if (!buffer) throw new TypeError('Only buffer bindings are supported');
        return [e.binding, idOf(buffer, 'buffer'), r.offset || 0, r.size === undefined ? -1 : r.size];
      });
      return tracked(this, 'group', Number(host('gpu.bindGroup', this._id, idOf(layout, 'layout'), JSON.stringify(list))));
    }
    createCommandEncoder() { return new GPUCommandEncoder(this); }
    pushErrorScope(filter) { host('gpu.pushErrorScope', this._id, String(filter)); }
    popErrorScope() {
      return settle(() => {
        const reply = String(host('gpu.popErrorScope', this._id));
        if (reply === '') return null;
        if (reply.startsWith('ERR:')) throw operationError(reply.slice(4));
        const at = reply.indexOf(':'), kind = reply.slice(0, at), message = reply.slice(at + 1);
        // A GPUError: gpu-lab reads its message.
        return {message, kind};
      });
    }
    destroy() {
      if (this._destroyed) return;
      this._destroyed = true;
      host('gpu.destroyDevice', this._id);
      if (this._lostResolve) { const r = this._lostResolve; this._lostResolve = null; r({reason: 'destroyed', message: 'Device destroyed'}); }
    }
  }
  class GPUAdapter {
    constructor(summary) {
      this.info = Object.freeze(summary.info);
      this.isFallbackAdapter = !!summary.info.isFallbackAdapter;
      this.limits = Object.freeze(summary.limits);
      this.features = new Set(summary.features);
    }
    requestDevice(descriptor = {}) {
      return settle(() => {
        const required = descriptor.requiredLimits || {};
        const created = JSON.parse(host('gpu.requestDevice', JSON.stringify(required)));
        return new GPUDevice(created.id, created.limits);
      });
    }
  }
  const gpu = Object.freeze({
    requestAdapter(options = {}) {
      return settle(() => {
        const reply = host('gpu.requestAdapter', String(options.powerPreference || ''));
        return reply === 'null' ? null : new GPUAdapter(JSON.parse(reply));
      });
    },
    getPreferredCanvasFormat() { return 'bgra8unorm'; },
  });
  globalThis.GPUBufferUsage = BUFFER_USAGE;
  globalThis.GPUMapMode = MAP_MODE;
  globalThis.GPUShaderStage = SHADER_STAGE;
  if (typeof globalThis.navigator !== 'object' || globalThis.navigator === null) globalThis.navigator = {};
  globalThis.navigator.gpu = gpu;
  // gpu-lab's timings read performance.now(); this engine's plain scripts have
  // no `performance`, and Date.now() is too coarse for a millisecond kernel.
  if (typeof globalThis.performance !== 'object' || globalThis.performance === null)
    globalThis.performance = {now: () => Number(host('gpu.now'))};
  return {gpu, upload};
})();
