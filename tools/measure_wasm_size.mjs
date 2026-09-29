import fs from 'node:fs';
import zlib from 'node:zlib';
import crypto from 'node:crypto';

function uleb(bytes, cursor) {
  let value = 0, shift = 0, byte;
  do { byte = bytes[cursor.i++]; value += (byte & 127) * 2 ** shift; shift += 7; } while (byte & 128);
  return value;
}
function sections(bytes) {
  const out = []; const cursor = { i: 8 };
  while (cursor.i < bytes.length) {
    const start = cursor.i, id = bytes[cursor.i++], length = uleb(bytes, cursor), payload = cursor.i;
    let name = ({1:'type',2:'import',3:'function',4:'table',5:'memory',6:'global',7:'export',8:'start',9:'element',10:'code',11:'data',12:'data_count'})[id] ?? 'custom';
    if (id === 0) { const n = uleb(bytes, cursor); name = 'custom:' + bytes.subarray(cursor.i, cursor.i+n).toString(); }
    out.push({name, bytes:payload + length - start}); cursor.i = payload + length;
  }
  return out;
}
const records = [];
for (const arg of process.argv.slice(2)) {
  const bytes = fs.readFileSync(arg);
  const record = {path:arg, bytes:bytes.length, sha256:crypto.createHash('sha256').update(bytes).digest('hex'), gzip9:zlib.gzipSync(bytes,{level:9}).length, brotli11:zlib.brotliCompressSync(bytes,{params:{[zlib.constants.BROTLI_PARAM_QUALITY]:11}}).length};
  if (arg.endsWith('.wasm')) record.sections = sections(bytes);
  records.push(record);
}
console.log(JSON.stringify(records,null,2));
