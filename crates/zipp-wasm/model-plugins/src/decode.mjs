import {check, fields, integer, shapeSize, sameShape} from './common.mjs';

/** The block format a tensor can stay in on a device, or null. */
const residentDtype = (weights, name) =>
  typeof weights.residentDtype === 'function' ? weights.residentDtype(name) : null;

/**
 * Cached decoding: one prepared graph, run once per token.
 *
 * The eager path in `bindings.mjs` rebuilds and resubmits the whole model for
 * every token, re-uploading every weight and recomputing the whole context. It
 * is the correctness oracle and stays exactly as it was. This is the other
 * shape of the same model, and it is what makes a real checkpoint usable:
 *
 *   * `runtime.prepare` validates the plan once and uploads every static input
 *     once, so weights stop crossing the boundary per token. For a 50,257-entry
 *     vocabulary that alone is 12.9 MiB of upload removed from every step.
 *   * Key and value caches are graph inputs marked `carry`, so the device keeps
 *     them between runs and the host never sees them.
 *   * Each step attends over the cache instead of recomputing the context, so a
 *     token costs what one position costs rather than what the prompt costs.
 *
 * The protocol has no scatter, so a plugin writes into its cache with a one-hot
 * column the host feeds per step: `cache * (1 - write) + write @ new`. That is
 * ordinary arithmetic on tensors the backend already knows how to multiply.
 *
 * The host fills four kinds of per-step input, none of which require knowing
 * what the model is: a gathered embedding row, an additive mask for the
 * positions written so far, that one-hot write column, and -- for a rotary
 * model -- the cosines and sines of the current position. Those last are
 * trigonometry over a position and a dimension, never over the data, so they
 * belong on this side of the boundary rather than in a kernel.
 */
const STEP_SLOTS = new Set(['rows', 'mask', 'write', 'rope']);
const ROPE_PARTS = new Set(['cos', 'sin', 'matrix']);
const INDEXES = new Set(['token', 'position']);
/** Graph v2 wants finite float32; this is the mask sentinel, not -Infinity. */
const MASKED = -1e9;

export function validateDecodeTemplate(template, limits) {
  fields(template, ['version', 'kind', 'context', 'graph', 'bindings'],
    ['version', 'kind', 'context', 'graph', 'bindings']);
  check(template.version === 1 && template.kind === 'decode', 'VERSION', 'Unsupported decode template');
  const graph = template.graph;
  fields(graph, ['version', 'nodes', 'outputs'], ['version', 'nodes', 'outputs']);
  check(graph.version === 2, 'VERSION', 'Expected ZIPP Graph v2');
  check(Array.isArray(graph.nodes) && graph.nodes.length > 0 && graph.nodes.length <= limits.maxNodes,
    'LIMIT', 'Decode graph node budget exceeded');
  integer(template.context, 1, limits.maxContext, 'Decode context');
  check(Array.isArray(graph.outputs) && graph.outputs.length >= 1, 'FORMAT', 'Decode graph needs outputs');
  const outputs = new Map();
  for (const output of graph.outputs) {
    fields(output, ['name', 'id'], ['name', 'id']);
    check(typeof output.name === 'string' && /^[a-z][a-z0-9_]{0,63}$/.test(output.name), 'FORMAT', 'Invalid output name');
    check(!outputs.has(output.name), 'FORMAT', 'Duplicate output name');
    integer(output.id, 0, graph.nodes.length - 1, 'Output node id');
    outputs.set(output.name, output.id);
  }
  check(outputs.has('logits'), 'FORMAT', 'A decode graph must expose a logits output');
  check(Array.isArray(template.bindings) && template.bindings.length <= graph.nodes.length,
    'LIMIT', 'Invalid decode binding count');
  return {graph, outputs};
}

/**
 * Resolve a decode template into a program `runtime.prepare` accepts, plus the
 * per-step plan the caller feeds it. Weight reads happen once, here.
 */
export async function prepareDecode(template, weights, limits) {
  const {graph, outputs} = validateDecodeTemplate(template, limits);
  const byNode = new Map();
  for (const binding of template.bindings) {
    check(binding && typeof binding === 'object', 'FORMAT', 'Invalid binding');
    integer(binding.node, 0, graph.nodes.length - 1, 'Binding node');
    check(!byNode.has(binding.node), 'FORMAT', 'Duplicate input binding');
    byNode.set(binding.node, binding);
  }
  const steps = [], carries = new Map();
  let residentElements = 0;
  const nodes = graph.nodes.map((raw, id) => {
    check(raw && typeof raw === 'object' && raw.id === id, 'GRAPH', 'Nonconsecutive graph id');
    const node = {...raw};
    if (node.op !== 'input') {
      check(!byNode.has(id), 'FORMAT', 'Only input nodes may bind assets or step data');
      return node;
    }
    fields(node, ['id', 'op', 'shape', 'carry', 'data'], ['id', 'op', 'shape']);
    const size = shapeSize(node.shape, limits);
    const binding = byNode.get(id);
    // A literal is a constant the graph carries itself -- a rotation matrix, a
    // selector -- and is the one input that needs no binding. Exactly one
    // source, as in the eager path.
    if (Object.hasOwn(node, 'data')) {
      check(!binding, 'FORMAT', 'An input has one data source: a literal or a binding, not both');
      check(Array.isArray(node.data) && node.data.length === size, 'SHAPE', 'Literal input size mismatch');
      check(!Object.hasOwn(node, 'carry'), 'FORMAT', 'A literal is constant, not carried');
      return node;
    }
    check(binding, 'FORMAT', 'Every decode input needs a binding: a weight, a zeroed cache, or step data');
    if (Object.hasOwn(node, 'carry')) {
      check(typeof node.carry === 'string' && outputs.has(node.carry), 'FORMAT',
        `carry names no output: ${String(node.carry)}`);
      check(!carries.has(node.carry), 'FORMAT', 'Two inputs carry the same output');
      carries.set(node.carry, id);
      residentElements += size;
      integer(residentElements, 0, limits.maxTensorElements, 'Resident cache elements');
    }
    return node;
  });
  for (const [id, binding] of byNode) {
    const node = nodes[id];
    check(node.op === 'input', 'FORMAT', 'Binding target must be an input');
    if (binding.kind === 'matrix' || binding.kind === 'blocks') {
      // The checkpoint's own [out, in] matrix, kept in its block format where a
      // backend can read it. Without this a cached decode would hold the model
      // expanded, which is the arithmetic the whole path exists to avoid.
      fields(binding, ['node', 'kind', 'tensor'], ['node', 'kind', 'tensor']);
      const info = weights.info(binding.tensor);
      check(info.shape.length === 2, 'SHAPE', `A weight matrix is rank two: ${binding.tensor}`);
      check(sameShape(info.shape, node.shape), 'SHAPE',
        `${binding.tensor} is ${JSON.stringify(info.shape)}, not ${JSON.stringify(node.shape)}`);
      check(!Object.hasOwn(node, 'carry'), 'FORMAT', 'A weight is static, not carried');
      if (binding.kind === 'blocks') {
        check(residentDtype(weights, binding.tensor) !== null, 'DTYPE',
          `${binding.tensor} is ${info.dtype}; no backend holds that format, so bind it as a matrix`);
      }
    } else if (binding.kind === 'tensor') {
      fields(binding, ['node', 'kind', 'tensor', 'transpose'], ['node', 'kind', 'tensor']);
      const shape = weights.info(binding.tensor).shape;
      if (Object.hasOwn(binding, 'transpose')) {
        check(binding.transpose === true, 'FORMAT', 'transpose is true when present');
        check(shape.length === 2, 'SHAPE', `Only a matrix can be transposed: ${binding.tensor}`);
      }
      check(sameShape(binding.transpose ? [shape[1], shape[0]] : shape, node.shape), 'SHAPE',
        `Weight shape mismatch: ${binding.tensor}`);
      check(!Object.hasOwn(node, 'carry'), 'FORMAT', 'A weight is static, not carried');
    } else if (binding.kind === 'zeros') {
      fields(binding, ['node', 'kind'], ['node', 'kind']);
      check(Object.hasOwn(node, 'carry'), 'FORMAT', 'A zeroed input is only useful as a carried cache');
    } else if (binding.kind === 'step') {
      fields(binding, ['node', 'kind', 'slot', 'tensor', 'index', 'window', 'part', 'dim', 'base'],
        ['node', 'kind', 'slot']);
      check(STEP_SLOTS.has(binding.slot), 'FORMAT', `Unknown per-step slot: ${String(binding.slot)}`);
      check(!Object.hasOwn(node, 'carry'), 'FORMAT', 'Step data is fed, not carried');
      if (binding.slot === 'rope') {
        fields(binding, ['node', 'kind', 'slot', 'part', 'dim', 'base'],
          ['node', 'kind', 'slot', 'part', 'dim', 'base']);
        check(ROPE_PARTS.has(binding.part), 'FORMAT', 'A rotary table is cos or sin');
        integer(binding.dim, 2, 4096, 'Rotary dimension');
        check(binding.dim % 2 === 0, 'SHAPE', 'Rotary embeddings need an even dimension');
        check(typeof binding.base === 'number' && Number.isFinite(binding.base) && binding.base > 1,
          'FORMAT', 'A rotary base is a finite number above one');
        check(sameShape(node.shape, binding.part === 'matrix' ? [binding.dim, binding.dim] : [1, 1, binding.dim]),
          'SHAPE', binding.part === 'matrix' ? 'A rotary matrix is [dim, dim]' : 'A rotary table is [1, 1, dim]');
      } else if (binding.slot === 'rows') {
        check(INDEXES.has(binding.index), 'FORMAT', 'A gathered row is indexed by token or position');
        const shape = weights.info(binding.tensor).shape;
        check(shape.length === 2 && sameShape(node.shape, [1, shape[1]]), 'SHAPE',
          `Gathered row shape mismatch: ${binding.tensor}`);
      } else if (binding.slot === 'mask') {
        if (binding.window !== undefined) integer(binding.window, 1, template.context, 'Local attention window');
        check(sameShape(node.shape, [1, template.context]), 'SHAPE', 'A decode mask is [1, context]');
      } else {
        check(sameShape(node.shape, [template.context, 1]), 'SHAPE', 'A write column is [context, 1]');
      }
      steps.push({node: id, ...binding});
    } else check(false, 'FORMAT', `Unknown decode binding: ${String(binding.kind)}`);
  }
  check(steps.some(step => step.slot === 'write') === carries.size > 0, 'FORMAT',
    'A cache that is carried must be written, and a write needs a cache');
  // Weights and zeroed caches become the plan's static data, uploaded once.
  for (const [id, binding] of byNode) {
    if (binding.kind === 'matrix' || binding.kind === 'blocks') {
      if (residentDtype(weights, binding.tensor) !== null) {
        const {dtype, bytes} = await weights.blocks(binding.tensor);
        nodes[id].dtype = dtype;
        nodes[id].data = bytes;
      } else {
        nodes[id].data = await weights.tensor(binding.tensor);
      }
    } else if (binding.kind === 'tensor') {
      const data = await weights.tensor(binding.tensor);
      if (!binding.transpose) nodes[id].data = data;
      else {
        const [rows, columns] = weights.info(binding.tensor).shape, out = new Float32Array(data.length);
        for (let row = 0; row < rows; row++) {
          for (let column = 0; column < columns; column++) out[column * rows + row] = data[row * columns + column];
        }
        nodes[id].data = out;
      }
    } else if (binding.kind === 'zeros') {
      nodes[id].data = new Float32Array(shapeSize(nodes[id].shape, limits));
    }
  }
  return {
    program: {version: 2, nodes, outputs: graph.outputs.map(o => ({...o}))},
    steps, context: template.context,
    resident: [...carries.keys()],
  };
}

/** The inputs one token needs: gathered rows, the mask so far, and the write column. */
export async function stepInputs(plan, weights, {token, position}) {
  integer(position, 0, plan.context - 1, 'Decode position');
  const inputs = {};
  for (const step of plan.steps) {
    if (step.slot === 'rows') {
      const info = weights.info(step.tensor), width = info.shape[1];
      const index = step.index === 'token' ? token : position;
      integer(index, 0, info.shape[0] - 1, step.index === 'token' ? 'Token id' : 'Position');
      // One row, read as one row. A quantized embedding table is 155 million
      // values and decoding all of it to reach one of them would cost more per
      // token than the rest of the model put together.
      const gathered = typeof weights.rows === 'function' ? await weights.rows(step.tensor, [index]) : null;
      if (gathered) inputs[step.node] = gathered;
      else {
        const data = await weights.tensor(step.tensor);
        inputs[step.node] = data.slice(index * width, (index + 1) * width);
      }
    } else if (step.slot === 'rope') {
      // The angles depend on the position and the index, never on the data,
      // so they are computed here rather than being an operation.
      const half = step.dim / 2;
      const angle = i => position / step.base ** ((2 * (i % half)) / step.dim);
      if (step.part === 'matrix') {
        // The whole rotation as one matrix, so a graph applies it with one
        // multiply rather than a rotate-half built out of six operations. Two
        // non-zeros a column: x[j] keeps cos, and the other half of its pair
        // arrives with sin, negated for the first half -- which is what
        // rotate-half means, written as the linear map it is.
        const rotation = new Float32Array(step.dim * step.dim);
        for (let j = 0; j < step.dim; j++) {
          const a = angle(j);
          rotation[j * step.dim + j] = Math.cos(a);
          const partner = j < half ? j + half : j - half;
          rotation[partner * step.dim + j] = j < half ? -Math.sin(a) : Math.sin(a);
        }
        inputs[step.node] = rotation;
      } else {
        const table = new Float32Array(step.dim);
        for (let i = 0; i < step.dim; i++) table[i] = step.part === 'cos' ? Math.cos(angle(i)) : Math.sin(angle(i));
        inputs[step.node] = table;
      }
    } else if (step.slot === 'mask') {
      const mask = new Float32Array(plan.context);
      for (let column = 0; column < plan.context; column++) {
        // Positions not written yet are masked, and a local layer also forgets
        // what fell out of its window -- the same rule the eager mask applies.
        if (column > position || (step.window !== undefined && column <= position - step.window)) mask[column] = MASKED;
      }
      inputs[step.node] = mask;
    } else {
      const write = new Float32Array(plan.context);
      write[position] = 1;
      inputs[step.node] = write;
    }
  }
  return {inputs};
}
