// Name every defined function of a finished WebAssembly module by its place
// in a content order, so that `wasm-opt --reorder-functions-by-name` lays the
// code section out in that order (optimize-wasm.sh, which then drops the
// names again).
//
// The order is by signature, then by body bytes: functions of one type with
// the same locals and the same opening instructions end up side by side.
// Brotli finds more and nearer matches in a code section laid out that way
// than in the linker's order, and the module is smaller after compression:
// 9 to 12 KB in each of the four modules, measured 2026-10-14. The engine
// runs the same instructions; only the functions' places and indices change.
//
// The input must have no name section (wasm-opt writes none without -g), and
// the output path must be distinct and absent. The order is a pure function
// of the module's bytes, so the same input always gives the same output.

'use strict'
const fs = require('fs')
const path = require('path')

const [inputArg, outputArg] = process.argv.slice(2)
if (!inputArg || !outputArg) {
  throw new Error('usage: node order-functions.cjs INPUT.wasm OUTPUT.wasm')
}
const inputPath = path.resolve(inputArg)
const outputPath = path.resolve(outputArg)
if (inputPath === outputPath) throw new Error('input and output paths must differ')
if (fs.existsSync(outputPath)) throw new Error(`refusing to overwrite existing output: ${outputPath}`)

const input = fs.readFileSync(inputPath)
new WebAssembly.Module(input)
if (input.length < 8 || input.subarray(0, 8).toString('hex') !== '0061736d01000000') {
  throw new Error('not a WebAssembly 1.0 module')
}

function readU32Leb(bytes, start, limit) {
  let value = 0
  let shift = 0
  let offset = start
  for (let count = 0; count < 5; count++) {
    if (offset >= limit) throw new Error('truncated unsigned LEB128')
    const byte = bytes[offset++]
    value |= (byte & 0x7f) << shift
    if ((byte & 0x80) === 0) return { value: value >>> 0, offset }
    shift += 7
  }
  throw new Error('invalid u32 LEB128')
}

function u32Leb(value) {
  const out = []
  do {
    let byte = value & 0x7f
    value >>>= 7
    if (value !== 0) byte |= 0x80
    out.push(byte)
  } while (value !== 0)
  return Buffer.from(out)
}

function nameBytes(text) {
  const bytes = Buffer.from(text, 'utf8')
  return Buffer.concat([u32Leb(bytes.length), bytes])
}

// Skip a limits structure (table or memory) and return the offset after it.
function skipLimits(bytes, offset, limit) {
  const flags = readU32Leb(bytes, offset, limit)
  let at = readU32Leb(bytes, flags.offset, limit).offset
  if (flags.value & 1) at = readU32Leb(bytes, at, limit).offset
  return at
}

let importedFunctions = 0
let types = null
let bodies = null
let offset = 8
while (offset < input.length) {
  const id = input[offset++]
  const size = readU32Leb(input, offset, input.length)
  const start = size.offset
  const end = start + size.value
  if (end > input.length) throw new Error('section extends beyond module')
  if (id === 0) {
    const nameLength = readU32Leb(input, start, end)
    const name = input.subarray(nameLength.offset, nameLength.offset + nameLength.value).toString('utf8')
    if (name === 'name') throw new Error('the input already has a name section')
  } else if (id === 2) {
    const count = readU32Leb(input, start, end)
    let at = count.offset
    for (let i = 0; i < count.value; i++) {
      for (let field = 0; field < 2; field++) {
        const length = readU32Leb(input, at, end)
        at = length.offset + length.value
      }
      const kind = input[at++]
      if (kind === 0) {
        importedFunctions++
        at = readU32Leb(input, at, end).offset
      } else if (kind === 1) {
        at = skipLimits(input, at + 1, end)
      } else if (kind === 2) {
        at = skipLimits(input, at, end)
      } else if (kind === 3) {
        at += 2
      } else if (kind === 4) {
        at = readU32Leb(input, at + 1, end).offset
      } else {
        throw new Error(`unknown import kind ${kind}`)
      }
    }
    if (at !== end) throw new Error('import section parse did not end at its boundary')
  } else if (id === 3) {
    const count = readU32Leb(input, start, end)
    types = []
    let at = count.offset
    for (let i = 0; i < count.value; i++) {
      const type = readU32Leb(input, at, end)
      types.push(type.value)
      at = type.offset
    }
    if (at !== end) throw new Error('function section parse did not end at its boundary')
  } else if (id === 10) {
    const count = readU32Leb(input, start, end)
    bodies = []
    let at = count.offset
    for (let i = 0; i < count.value; i++) {
      const length = readU32Leb(input, at, end)
      bodies.push(input.subarray(length.offset, length.offset + length.value))
      at = length.offset + length.value
    }
    if (at !== end) throw new Error('code section parse did not end at its boundary')
  }
  offset = end
}
if (offset !== input.length) throw new Error('section parser did not consume the module')
if (!types || !bodies || types.length !== bodies.length) {
  throw new Error('function and code sections disagree')
}

const order = bodies.map((_, i) => i)
order.sort((a, b) => (types[a] - types[b]) || Buffer.compare(bodies[a], bodies[b]) || (a - b))
const rank = new Array(bodies.length)
order.forEach((fn, place) => { rank[fn] = place })

const width = String(bodies.length).length
const entries = [u32Leb(bodies.length)]
for (let fn = 0; fn < bodies.length; fn++) {
  entries.push(u32Leb(importedFunctions + fn), nameBytes(String(rank[fn]).padStart(width, '0')))
}
const functionNames = Buffer.concat(entries)
const payload = Buffer.concat([nameBytes('name'), Buffer.from([1]), u32Leb(functionNames.length), functionNames])
const output = Buffer.concat([input, Buffer.from([0]), u32Leb(payload.length), payload])
new WebAssembly.Module(output)
fs.writeFileSync(outputPath, output, { flag: 'wx' })
console.log(`ordered ${bodies.length} functions by signature and body`)
