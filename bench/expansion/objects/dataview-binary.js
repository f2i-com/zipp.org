// dataview-binary: a binary record codec on DataView and Uint8Array.
//
// What it measures: the byte-level work of a wire-format encoder/decoder.
//   1. encode 192 records per round into an ArrayBuffer through DataView:
//      setUint32/setInt8/setUint16/setInt16/setInt32/setFloat32/setFloat64/
//      setBigInt64/setBigUint64, alternating little- and big-endian records
//      (a header byte selects the endianness);
//   2. LEB128 varints (unsigned) and zigzag varints (signed) for lengths and
//      deltas, and short ASCII names written byte by byte;
//   3. CRC-32 (table-driven, 256-entry Uint32Array) over the whole encoded
//      buffer and over every 64-byte frame;
//   4. framing with Uint8Array.set (copy into an output buffer at an
//      offset), subarray (zero-copy views into frames) and slice (copies);
//   5. decode every record back with the matching getters and compare all
//      fields with the source (mismatch count must be 0).
// No NaN is ever encoded (its bit pattern is implementation-defined), and
// Float32 values are compared through Math.fround.
// Why it is a likely weak spot: each DataView access is a builtin call that
// must check the view bounds, convert the value, and byte-swap for the
// requested endianness; BigInt64 accessors allocate BigInts; CRC and varint
// loops are tight Uint8Array/Uint32Array element loops where any per-access
// overhead in typed-array indexing shows directly.
// Dominant operations: DataView get/set calls, Uint8Array element loads and
// stores, Uint32Array table lookups, BigInt64 conversion.
//
// SIZE is the number of rounds; every round encodes and decodes the same
// number of records.

const SIZE = 500;

let seed = 424242;
function rnd() { seed = (Math.imul(seed, 1664525) + 1013904223) | 0; return (seed >>> 0) / 4294967296; }
function mix(h, v) { return Math.imul(h ^ (v | 0), 16777619); }

const NREC = 192;
const CRC_TABLE = (function () {
  const t = new Uint32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    t[n] = c >>> 0;
  }
  return t;
})();

function crc32(bytes, start, end) {
  let c = 0xffffffff;
  const t = CRC_TABLE;
  for (let i = start; i < end; i++) c = t[(c ^ bytes[i]) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

function writeVarint(u8, pos, v) {
  v = v >>> 0;
  while (v >= 0x80) { u8[pos++] = (v & 0x7f) | 0x80; v >>>= 7; }
  u8[pos++] = v;
  return pos;
}
// Decoders return the value and advance a shared cursor.
const cur = { pos: 0 };
function readVarint(u8) {
  let pos = cur.pos, shift = 0, v = 0, b;
  do {
    b = u8[pos++];
    v += (b & 0x7f) * Math.pow(2, shift);
    shift += 7;
  } while (b & 0x80);
  cur.pos = pos;
  return v;
}
function zigzag(n) { return ((n << 1) ^ (n >> 31)) >>> 0; }
function unzigzag(u) { return (u >>> 1) ^ -(u & 1); }

function makeRecords(n) {
  const recs = [];
  let prev = 0;
  for (let i = 0; i < n; i++) {
    const id = (rnd() * 4294967296) >>> 0;
    const len = 2 + ((rnd() * 10) | 0);
    let name = "";
    for (let j = 0; j < len; j++) name += String.fromCharCode(97 + ((rnd() * 26) | 0));
    const val = ((rnd() * 2000000) | 0) - 1000000;
    recs.push({
      le: (i & 1) === 1,
      id: id,
      kind: ((rnd() * 256) | 0) - 128,
      flags: (rnd() * 65536) | 0,
      small: ((rnd() * 65536) | 0) - 32768,
      val: val,
      delta: val - prev,
      temp: (rnd() - 0.5) * 300,          // stored as float32
      ratio: rnd() * 1e9 - 5e8,           // stored as float64
      big: BigInt(id) * 1000003n - BigInt(i) * 77777777n,   // signed 64-bit range
      ubig: (BigInt(id) << 24n) + BigInt(i),
      name: name,
    });
    prev = val;
  }
  return recs;
}

function encode(recs, buf, r) {
  const dv = new DataView(buf);
  const u8 = new Uint8Array(buf);
  let pos = 0;
  for (let i = 0; i < recs.length; i++) {
    const o = recs[i];
    const le = o.le;
    dv.setUint8(pos, le ? 1 : 0); pos += 1;
    dv.setUint32(pos, o.id, le); pos += 4;
    dv.setInt8(pos, o.kind); pos += 1;
    dv.setUint16(pos, o.flags ^ (r & 0xff), le); pos += 2;
    dv.setInt16(pos, o.small, le); pos += 2;
    dv.setInt32(pos, o.val, le); pos += 4;
    dv.setFloat32(pos, o.temp, le); pos += 4;
    dv.setFloat64(pos, o.ratio, le); pos += 8;
    dv.setBigInt64(pos, o.big, le); pos += 8;
    dv.setBigUint64(pos, o.ubig, !le); pos += 8;
    pos = writeVarint(u8, pos, zigzag(o.delta));
    pos = writeVarint(u8, pos, o.name.length);
    for (let j = 0; j < o.name.length; j++) u8[pos++] = o.name.charCodeAt(j);
  }
  return pos;
}

function decode(recs, buf, end, r) {
  const dv = new DataView(buf);
  const u8 = new Uint8Array(buf);
  let pos = 0, bad = 0, h = 0, i = 0;
  while (pos < end) {
    const o = recs[i++];
    const le = dv.getUint8(pos) === 1; pos += 1;
    if (le !== o.le) bad++;
    const id = dv.getUint32(pos, le); pos += 4;
    const kind = dv.getInt8(pos); pos += 1;
    const flags = dv.getUint16(pos, le) ^ (r & 0xff); pos += 2;
    const small = dv.getInt16(pos, le); pos += 2;
    const val = dv.getInt32(pos, le); pos += 4;
    const temp = dv.getFloat32(pos, le); pos += 4;
    const ratio = dv.getFloat64(pos, le); pos += 8;
    const big = dv.getBigInt64(pos, le); pos += 8;
    const ubig = dv.getBigUint64(pos, !le); pos += 8;
    cur.pos = pos;
    const delta = unzigzag(readVarint(u8));
    const nlen = readVarint(u8);
    pos = cur.pos;
    let nameOk = nlen === o.name.length;
    for (let j = 0; j < nlen; j++) if (u8[pos + j] !== o.name.charCodeAt(j)) nameOk = false;
    pos += nlen;
    if (id !== o.id || kind !== o.kind || flags !== o.flags || small !== o.small || val !== o.val) bad++;
    if (temp !== Math.fround(o.temp) || ratio !== o.ratio || big !== o.big || ubig !== o.ubig) bad++;
    if (delta !== o.delta || !nameOk) bad++;
    h = mix(h, id ^ val);
    h = mix(h, (temp * 64) | 0);
    h = mix(h, Number(big & 0xffffn) + Number(ubig >> 48n));
  }
  return [h, bad, i];
}

function frame(src, end, out) {
  // copy the encoded stream into the output in 64-byte frames, each prefixed
  // by a 4-byte big-endian CRC of the frame and a 1-byte length
  const odv = new DataView(out.buffer);
  let opos = 0, h = 0, frames = 0;
  for (let p = 0; p < end; p += 64) {
    const q = p + 64 < end ? p + 64 : end;
    const view = src.subarray(p, q);
    odv.setUint32(opos, crc32(view, 0, view.length), false);
    out[opos + 4] = view.length;
    out.set(view, opos + 5);
    opos += 5 + view.length;
    frames++;
  }
  // verify frames from the output, using slice copies for odd frames
  let ok = 0;
  for (let p = 0; p < opos;) {
    const len = out[p + 4];
    const body = (frames & 1) ? out.slice(p + 5, p + 5 + len) : out.subarray(p + 5, p + 5 + len);
    if (crc32(body, 0, body.length) === odv.getUint32(p, false)) ok++;
    h = mix(h, body[0] | (body[len - 1] << 8));
    p += 5 + len;
    frames--;
  }
  return [h, ok, opos];
}

function main() {
  const rounds = SIZE;
  const recs = makeRecords(NREC);
  const buf = new ArrayBuffer(NREC * 80);
  const out = new Uint8Array(NREC * 96);
  let hEnc = 0, hDec = 0, hFrame = 0, bad = 0, decoded = 0, framesOk = 0, bytes = 0;
  for (let r = 0; r < rounds; r++) {
    const end = encode(recs, buf, r);
    bytes += end;
    const u8 = new Uint8Array(buf, 0, end);
    hEnc = mix(hEnc, crc32(u8, 0, end));
    const d = decode(recs, buf, end, r);
    hDec = mix(hDec, d[0]); bad += d[1]; decoded += d[2];
    const f = frame(u8, end, out);
    hFrame = mix(hFrame, f[0] ^ f[2]); framesOk += f[1];
    // mutate one record per round so the encoded bytes change over time
    const o = recs[r % NREC];
    o.val = (o.val + 1013904223) | 0;
    o.delta = (o.delta + 1013904223) | 0;
    if (r % NREC + 1 < NREC) recs[r % NREC + 1].delta = (recs[r % NREC + 1].delta - 12345) | 0;
  }
  console.log("encode", hEnc >>> 0, bytes);
  console.log("decode", hDec >>> 0, decoded, "bad", bad);
  console.log("frames", hFrame >>> 0, framesOk);
}

main();
