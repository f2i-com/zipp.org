// Remove only the optional `target_features` custom section from a finished
// WebAssembly module. Engines ignore custom sections; keeping this compiler
// metadata in a browser artifact costs transfer bytes without changing
// validation or execution.
//
// The output path must be distinct and absent. Callers can atomically replace
// the generated input only after this script has parsed and revalidated the
// byte-preserving result.

const fs = require('fs')
const path = require('path')

const [inputArg, outputArg] = process.argv.slice(2)
if (!inputArg || !outputArg) {
  throw new Error('usage: node strip-target-features.cjs INPUT.wasm OUTPUT.wasm')
}

const inputPath = path.resolve(inputArg)
const outputPath = path.resolve(outputArg)
if (inputPath === outputPath) {
  throw new Error('input and output paths must differ')
}
if (fs.existsSync(outputPath)) {
  throw new Error(`refusing to overwrite existing output: ${outputPath}`)
}

const input = fs.readFileSync(inputPath)
// Validate before parsing so malformed section lengths cannot be mistaken for
// an optimization opportunity.
new WebAssembly.Module(input)

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

if (input.length < 8 || input.subarray(0, 8).toString('hex') !== '0061736d01000000') {
  throw new Error('not a WebAssembly 1.0 module')
}

const kept = [input.subarray(0, 8)]
let offset = 8
let removedBytes = 0
let removedCount = 0
while (offset < input.length) {
  const sectionStart = offset
  const id = input[offset++]
  const size = readU32Leb(input, offset, input.length)
  const payloadStart = size.offset
  const sectionEnd = payloadStart + size.value
  if (sectionEnd > input.length) throw new Error('section extends beyond module')

  let remove = false
  if (id === 0) {
    const nameLength = readU32Leb(input, payloadStart, sectionEnd)
    const nameEnd = nameLength.offset + nameLength.value
    if (nameEnd > sectionEnd) throw new Error('custom-section name extends beyond payload')
    const name = new TextDecoder('utf-8', { fatal: true })
      .decode(input.subarray(nameLength.offset, nameEnd))
    remove = name === 'target_features'
  }

  if (remove) {
    removedCount++
    removedBytes += sectionEnd - sectionStart
  } else {
    kept.push(input.subarray(sectionStart, sectionEnd))
  }
  offset = sectionEnd
}

if (offset !== input.length) throw new Error('section parser did not consume the module')
if (removedCount !== 1) {
  throw new Error(`expected exactly one target_features custom section, found ${removedCount}`)
}

const output = Buffer.concat(kept)
if (output.length !== input.length - removedBytes) {
  throw new Error('output changed bytes outside the selected section')
}
const wasmModule = new WebAssembly.Module(output)
if (WebAssembly.Module.customSections(wasmModule, 'target_features').length !== 0) {
  throw new Error('target_features section survived stripping')
}

fs.writeFileSync(outputPath, output, { flag: 'wx' })
console.log(`removed target_features custom section: ${removedBytes} bytes`)
