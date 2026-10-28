// node:crypto — random values and the common digests (md5, sha1, sha256,
// and their HMACs), in JavaScript. The randomness is Math.random's:
// unpredictable enough for ids, not for keys.
(function (host, builtin) {
  "use strict";
  const { Buffer } = builtin("buffer");
  function fillRandom(view) {
    const u8 = new Uint8Array(view.buffer, view.byteOffset, view.byteLength);
    for (let i = 0; i < u8.length; i++) u8[i] = (Math.random() * 256) | 0;
    return view;
  }
  function randomUUID() {
    const b = fillRandom(new Uint8Array(16));
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    const h = Array.from(b, (x) => (x < 16 ? "0" : "") + x.toString(16)).join("");
    return h.slice(0, 8) + "-" + h.slice(8, 12) + "-" + h.slice(12, 16) + "-" + h.slice(16, 20) + "-" + h.slice(20);
  }
  const rotl = (x, n) => (x << n) | (x >>> (32 - n));
  const rotr = (x, n) => (x >>> n) | (x << (32 - n));
  const K256 = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
  ];
  function pad(bytes, littleEndian) {
    const bitLen = bytes.length * 8;
    const total = (((bytes.length + 8) >> 6) + 1) * 64;
    const out = new Uint8Array(total);
    out.set(bytes);
    out[bytes.length] = 0x80;
    const dv = new DataView(out.buffer);
    if (littleEndian) {
      dv.setUint32(total - 8, bitLen >>> 0, true);
      dv.setUint32(total - 4, Math.floor(bitLen / 2 ** 32), true);
    } else {
      dv.setUint32(total - 8, Math.floor(bitLen / 2 ** 32), false);
      dv.setUint32(total - 4, bitLen >>> 0, false);
    }
    return out;
  }
  function words(H, littleEndian) {
    const out = new Uint8Array(H.length * 4);
    const dv = new DataView(out.buffer);
    H.forEach((w, i) => dv.setUint32(i * 4, w >>> 0, littleEndian));
    return out;
  }
  function sha256(bytes) {
    const H = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
    const msg = pad(bytes, false);
    const W = new Int32Array(64);
    for (let o = 0; o < msg.length; o += 64) {
      for (let i = 0; i < 16; i++) {
        W[i] = (msg[o + 4 * i] << 24) | (msg[o + 4 * i + 1] << 16) | (msg[o + 4 * i + 2] << 8) | msg[o + 4 * i + 3];
      }
      for (let i = 16; i < 64; i++) {
        const s0 = rotr(W[i - 15], 7) ^ rotr(W[i - 15], 18) ^ (W[i - 15] >>> 3);
        const s1 = rotr(W[i - 2], 17) ^ rotr(W[i - 2], 19) ^ (W[i - 2] >>> 10);
        W[i] = (W[i - 16] + s0 + W[i - 7] + s1) | 0;
      }
      let [a, b, c, d, e, f, g, h] = H;
      for (let i = 0; i < 64; i++) {
        const t1 = (h + (rotr(e, 6) ^ rotr(e, 11) ^ rotr(e, 25)) + ((e & f) ^ (~e & g)) + K256[i] + W[i]) | 0;
        const t2 = ((rotr(a, 2) ^ rotr(a, 13) ^ rotr(a, 22)) + ((a & b) ^ (a & c) ^ (b & c))) | 0;
        h = g;
        g = f;
        f = e;
        e = (d + t1) | 0;
        d = c;
        c = b;
        b = a;
        a = (t1 + t2) | 0;
      }
      const v = [a, b, c, d, e, f, g, h];
      for (let i = 0; i < 8; i++) H[i] = (H[i] + v[i]) | 0;
    }
    return words(H, false);
  }
  function sha1(bytes) {
    const H = [0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476, 0xc3d2e1f0];
    const msg = pad(bytes, false);
    const W = new Int32Array(80);
    for (let o = 0; o < msg.length; o += 64) {
      for (let i = 0; i < 16; i++) {
        W[i] = (msg[o + 4 * i] << 24) | (msg[o + 4 * i + 1] << 16) | (msg[o + 4 * i + 2] << 8) | msg[o + 4 * i + 3];
      }
      for (let i = 16; i < 80; i++) W[i] = rotl(W[i - 3] ^ W[i - 8] ^ W[i - 14] ^ W[i - 16], 1);
      let [a, b, c, d, e] = H;
      for (let i = 0; i < 80; i++) {
        let f, k;
        if (i < 20) { f = (b & c) | (~b & d); k = 0x5a827999; }
        else if (i < 40) { f = b ^ c ^ d; k = 0x6ed9eba1; }
        else if (i < 60) { f = (b & c) | (b & d) | (c & d); k = 0x8f1bbcdc; }
        else { f = b ^ c ^ d; k = 0xca62c1d6; }
        const t = (rotl(a, 5) + f + e + k + W[i]) | 0;
        e = d;
        d = c;
        c = rotl(b, 30);
        b = a;
        a = t;
      }
      const v = [a, b, c, d, e];
      for (let i = 0; i < 5; i++) H[i] = (H[i] + v[i]) | 0;
    }
    return words(H, false);
  }
  const S = [7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20,
    4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21];
  const KMD5 = Array.from({ length: 64 }, (_, i) => Math.floor(Math.abs(Math.sin(i + 1)) * 2 ** 32) | 0);
  function md5(bytes) {
    const H = [0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476];
    const msg = pad(bytes, true);
    const M = new Int32Array(16);
    for (let o = 0; o < msg.length; o += 64) {
      for (let i = 0; i < 16; i++) {
        M[i] = msg[o + 4 * i] | (msg[o + 4 * i + 1] << 8) | (msg[o + 4 * i + 2] << 16) | (msg[o + 4 * i + 3] << 24);
      }
      let [a, b, c, d] = H;
      for (let i = 0; i < 64; i++) {
        let f, g;
        if (i < 16) { f = (b & c) | (~b & d); g = i; }
        else if (i < 32) { f = (d & b) | (~d & c); g = (5 * i + 1) % 16; }
        else if (i < 48) { f = b ^ c ^ d; g = (3 * i + 5) % 16; }
        else { f = c ^ (b | ~d); g = (7 * i) % 16; }
        const t = d;
        d = c;
        c = b;
        b = (b + rotl((a + f + KMD5[i] + M[g]) | 0, S[i])) | 0;
        a = t;
      }
      const v = [a, b, c, d];
      for (let i = 0; i < 4; i++) H[i] = (H[i] + v[i]) | 0;
    }
    return words(H, true);
  }
  const digests = { sha256, sha1, md5 };
  function toBytes(data, enc) {
    return typeof data === "string" ? Buffer.from(data, enc) : new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
  }
  class Hash {
    constructor(algorithm, key) {
      const a = String(algorithm).toLowerCase().replace("-", "");
      if (!digests[a]) {
        const e = new Error("Digest method not supported");
        e.code = "ERR_CRYPTO_INVALID_DIGEST";
        throw e;
      }
      this._fn = digests[a];
      this._chunks = [];
      this._key = key;
    }
    update(data, enc) {
      this._chunks.push(Buffer.from(toBytes(data, enc)));
      return this;
    }
    digest(enc) {
      const data = Buffer.concat(this._chunks);
      let out;
      if (this._key === undefined) {
        out = this._fn(data);
      } else {
        let key = toBytes(this._key);
        if (key.length > 64) key = this._fn(key);
        const k = new Uint8Array(64);
        k.set(key);
        const inner = this._fn(Buffer.concat([Buffer.from(k.map((x) => x ^ 0x36)), data]));
        out = this._fn(Buffer.concat([Buffer.from(k.map((x) => x ^ 0x5c)), Buffer.from(inner)]));
      }
      const b = Buffer.from(out);
      return enc ? b.toString(enc) : b;
    }
  }
  return {
    randomUUID,
    randomBytes(n, cb) {
      const b = fillRandom(Buffer.alloc(n));
      if (typeof cb === "function") {
        process.nextTick(cb, null, b);
        return undefined;
      }
      return b;
    },
    randomFillSync: fillRandom,
    randomFill(buf, cb) {
      fillRandom(buf);
      process.nextTick(cb, null, buf);
    },
    randomInt(min, max) {
      if (max === undefined) {
        max = min;
        min = 0;
      }
      return min + Math.floor(Math.random() * (max - min));
    },
    getRandomValues: fillRandom,
    webcrypto: { getRandomValues: fillRandom, randomUUID, subtle: {} },
    createHash: (algorithm) => new Hash(algorithm),
    createHmac: (algorithm, key) => new Hash(algorithm, key),
    getHashes: () => Object.keys(digests),
    timingSafeEqual: (a, b) => a.length === b.length && a.every((x, i) => x === b[i]),
  };
})
