// node:buffer — Buffer, a Uint8Array with Node's encodings and methods.
(function () {
  "use strict";
  const encoder = new TextEncoder();
  const B64 = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
  const B64URL = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
  const B64INV = new Int16Array(256).fill(-1);
  for (let i = 0; i < 64; i++) {
    B64INV[B64.charCodeAt(i)] = i;
    B64INV[B64URL.charCodeAt(i)] = i;
  }
  function normEncoding(enc) {
    if (enc === undefined || enc === null) return "utf8";
    const e = String(enc).toLowerCase();
    switch (e) {
      case "utf8": case "utf-8": return "utf8";
      case "hex": return "hex";
      case "base64": return "base64";
      case "base64url": return "base64url";
      case "latin1": case "binary": return "latin1";
      case "ascii": return "ascii";
      case "ucs2": case "ucs-2": case "utf16le": case "utf-16le": return "utf16le";
      default: throw new TypeError("Unknown encoding: " + enc);
    }
  }
  function bytesOf(string, enc) {
    switch (normEncoding(enc)) {
      case "utf8": return encoder.encode(string);
      case "hex": {
        const n = string.length >>> 1;
        const out = new Uint8Array(n);
        let i = 0;
        for (; i < n; i++) {
          const v = parseInt(string.substr(i * 2, 2), 16);
          if (Number.isNaN(v)) break;
          out[i] = v;
        }
        return out.subarray(0, i);
      }
      case "base64":
      case "base64url": {
        const out = [];
        let bits = 0, acc = 0;
        for (let i = 0; i < string.length; i++) {
          const v = B64INV[string.charCodeAt(i) & 255];
          if (v < 0) continue;
          acc = (acc << 6) | v;
          bits += 6;
          if (bits >= 8) {
            bits -= 8;
            out.push((acc >> bits) & 255);
          }
        }
        return Uint8Array.from(out);
      }
      case "latin1":
      case "ascii": {
        const out = new Uint8Array(string.length);
        for (let i = 0; i < string.length; i++) out[i] = string.charCodeAt(i) & 255;
        return out;
      }
      case "utf16le": {
        const out = new Uint8Array(string.length * 2);
        for (let i = 0; i < string.length; i++) {
          const c = string.charCodeAt(i);
          out[i * 2] = c & 255;
          out[i * 2 + 1] = c >> 8;
        }
        return out;
      }
    }
  }
  function stringOf(bytes, enc) {
    switch (normEncoding(enc)) {
      case "utf8": return new TextDecoder().decode(bytes);
      case "hex": {
        let s = "";
        for (let i = 0; i < bytes.length; i++) s += (bytes[i] < 16 ? "0" : "") + bytes[i].toString(16);
        return s;
      }
      case "base64":
      case "base64url": {
        const url = normEncoding(enc) === "base64url";
        const T = url ? B64URL : B64;
        let s = "";
        let i = 0;
        for (; i + 2 < bytes.length; i += 3) {
          const n = (bytes[i] << 16) | (bytes[i + 1] << 8) | bytes[i + 2];
          s += T[n >> 18] + T[(n >> 12) & 63] + T[(n >> 6) & 63] + T[n & 63];
        }
        if (i < bytes.length) {
          const n = (bytes[i] << 16) | ((i + 1 < bytes.length ? bytes[i + 1] : 0) << 8);
          s += T[n >> 18] + T[(n >> 12) & 63];
          s += i + 1 < bytes.length ? T[(n >> 6) & 63] : url ? "" : "=";
          s += url ? "" : "=";
        }
        return s;
      }
      case "latin1": {
        let s = "";
        for (let i = 0; i < bytes.length; i += 4096) s += String.fromCharCode(...bytes.subarray(i, i + 4096));
        return s;
      }
      case "ascii": {
        let s = "";
        for (let i = 0; i < bytes.length; i++) s += String.fromCharCode(bytes[i] & 127);
        return s;
      }
      case "utf16le": {
        let s = "";
        for (let i = 0; i + 1 < bytes.length; i += 2) s += String.fromCharCode(bytes[i] | (bytes[i + 1] << 8));
        return s;
      }
    }
  }

  class Buffer extends Uint8Array {
    static from(value, encodingOrOffset, length) {
      if (typeof value === "string") return toBuffer(bytesOf(value, encodingOrOffset));
      if (value instanceof ArrayBuffer || (typeof SharedArrayBuffer !== "undefined" && value instanceof SharedArrayBuffer)) {
        const offset = encodingOrOffset === undefined ? 0 : +encodingOrOffset;
        const len = length === undefined ? value.byteLength - offset : +length;
        return new Buffer(value, offset, len);
      }
      if (ArrayBuffer.isView(value)) {
        const b = Buffer.allocUnsafe(value.byteLength);
        b.set(new Uint8Array(value.buffer, value.byteOffset, value.byteLength));
        return b;
      }
      if (value && typeof value === "object") {
        if (value.type === "Buffer" && Array.isArray(value.data)) return Buffer.from(value.data);
        if (typeof value.length === "number" || Array.isArray(value)) {
          const b = Buffer.allocUnsafe(value.length >>> 0);
          for (let i = 0; i < b.length; i++) b[i] = value[i] & 255;
          return b;
        }
        if (typeof value[Symbol.toPrimitive] === "function" || typeof value.valueOf === "function") {
          const p = value.valueOf();
          if (p !== value) return Buffer.from(p, encodingOrOffset, length);
        }
      }
      throw new TypeError("The first argument must be of type string or an instance of Buffer, ArrayBuffer, or Array or an Array-like Object. Received " + (value === null ? "null" : typeof value));
    }
    static alloc(size, fill, encoding) {
      const b = new Buffer(size);
      if (fill !== undefined && fill !== 0) b.fill(fill, 0, b.length, encoding);
      return b;
    }
    static allocUnsafe(size) {
      return new Buffer(size);
    }
    static allocUnsafeSlow(size) {
      return new Buffer(size);
    }
    static isBuffer(b) {
      return b instanceof Buffer;
    }
    static isEncoding(enc) {
      try {
        normEncoding(enc);
        return typeof enc === "string";
      } catch {
        return false;
      }
    }
    static byteLength(value, encoding) {
      if (typeof value !== "string") return value.byteLength;
      return bytesOf(value, encoding).length;
    }
    static concat(list, totalLength) {
      if (totalLength === undefined) totalLength = list.reduce((n, b) => n + b.length, 0);
      const out = Buffer.alloc(totalLength);
      let at = 0;
      for (const b of list) {
        const take = Math.min(b.length, totalLength - at);
        if (take <= 0) break;
        out.set(take === b.length ? b : b.subarray(0, take), at);
        at += take;
      }
      return out;
    }
    static compare(a, b) {
      return a.compare(b);
    }
    toString(encoding, start = 0, end = this.length) {
      start = Math.max(0, start | 0);
      end = Math.min(this.length, end === undefined ? this.length : end | 0);
      if (end <= start) return "";
      return stringOf(this.subarray(start, end), encoding);
    }
    toJSON() {
      return { type: "Buffer", data: Array.from(this) };
    }
    equals(other) {
      if (this.length !== other.length) return false;
      for (let i = 0; i < this.length; i++) if (this[i] !== other[i]) return false;
      return true;
    }
    compare(other, tStart = 0, tEnd = other.length, sStart = 0, sEnd = this.length) {
      const a = this.subarray(sStart, sEnd), b = other.subarray(tStart, tEnd);
      const n = Math.min(a.length, b.length);
      for (let i = 0; i < n; i++) if (a[i] !== b[i]) return a[i] < b[i] ? -1 : 1;
      return a.length === b.length ? 0 : a.length < b.length ? -1 : 1;
    }
    write(string, offset = 0, length, encoding) {
      if (typeof offset === "string") {
        encoding = offset;
        offset = 0;
      } else if (typeof length === "string") {
        encoding = length;
        length = undefined;
      }
      const bytes = bytesOf(string, encoding);
      const n = Math.min(bytes.length, this.length - offset, length === undefined ? Infinity : length);
      this.set(bytes.subarray(0, n), offset);
      return n;
    }
    fill(value, offset = 0, end = this.length, encoding) {
      if (typeof offset === "string") {
        encoding = offset;
        offset = 0;
        end = this.length;
      }
      if (typeof value === "string") {
        const bytes = value.length === 1 && normEncoding(encoding) === "utf8" && value.charCodeAt(0) < 128
          ? [value.charCodeAt(0)] : bytesOf(value, encoding);
        if (bytes.length === 0) return super.fill(0, offset, end);
        for (let i = offset, j = 0; i < end; i++, j = (j + 1) % bytes.length) this[i] = bytes[j];
        return this;
      }
      return super.fill(typeof value === "number" ? value & 255 : value, offset, end);
    }
    slice(start, end) {
      return this.subarray(start, end);
    }
    subarray(start, end) {
      const u = Uint8Array.prototype.subarray.call(this, start, end);
      return new Buffer(u.buffer, u.byteOffset, u.length);
    }
    indexOf(value, byteOffset = 0, encoding) {
      const needle = typeof value === "number" ? [value & 255] : typeof value === "string" ? bytesOf(value, encoding) : value;
      if (byteOffset < 0) byteOffset = Math.max(0, this.length + byteOffset);
      outer: for (let i = byteOffset; i <= this.length - needle.length; i++) {
        for (let j = 0; j < needle.length; j++) if (this[i + j] !== needle[j]) continue outer;
        return i;
      }
      return -1;
    }
    includes(value, byteOffset, encoding) {
      return this.indexOf(value, byteOffset, encoding) !== -1;
    }
    copy(target, targetStart = 0, sourceStart = 0, sourceEnd = this.length) {
      const src = this.subarray(sourceStart, Math.min(sourceEnd, sourceStart + (target.length - targetStart)));
      target.set(src, targetStart);
      return src.length;
    }
    readUInt8(o = 0) { return this[o]; }
    readInt8(o = 0) { return (this[o] << 24) >> 24; }
    readUInt16LE(o = 0) { return this[o] | (this[o + 1] << 8); }
    readUInt16BE(o = 0) { return (this[o] << 8) | this[o + 1]; }
    readInt16LE(o = 0) { return (this.readUInt16LE(o) << 16) >> 16; }
    readInt16BE(o = 0) { return (this.readUInt16BE(o) << 16) >> 16; }
    readUInt32LE(o = 0) { return (this[o] | (this[o + 1] << 8) | (this[o + 2] << 16)) + this[o + 3] * 0x1000000; }
    readUInt32BE(o = 0) { return this[o] * 0x1000000 + ((this[o + 1] << 16) | (this[o + 2] << 8) | this[o + 3]); }
    readInt32LE(o = 0) { return this[o] | (this[o + 1] << 8) | (this[o + 2] << 16) | (this[o + 3] << 24); }
    readInt32BE(o = 0) { return (this[o] << 24) | (this[o + 1] << 16) | (this[o + 2] << 8) | this[o + 3]; }
    readDoubleLE(o = 0) { return new DataView(this.buffer, this.byteOffset).getFloat64(o, true); }
    readDoubleBE(o = 0) { return new DataView(this.buffer, this.byteOffset).getFloat64(o, false); }
    readFloatLE(o = 0) { return new DataView(this.buffer, this.byteOffset).getFloat32(o, true); }
    readFloatBE(o = 0) { return new DataView(this.buffer, this.byteOffset).getFloat32(o, false); }
    readBigUInt64LE(o = 0) { return new DataView(this.buffer, this.byteOffset).getBigUint64(o, true); }
    readBigUInt64BE(o = 0) { return new DataView(this.buffer, this.byteOffset).getBigUint64(o, false); }
    writeUInt8(v, o = 0) { this[o] = v; return o + 1; }
    writeInt8(v, o = 0) { this[o] = v; return o + 1; }
    writeUInt16LE(v, o = 0) { this[o] = v; this[o + 1] = v >>> 8; return o + 2; }
    writeUInt16BE(v, o = 0) { this[o] = v >>> 8; this[o + 1] = v; return o + 2; }
    writeInt16LE(v, o = 0) { return this.writeUInt16LE(v & 0xffff, o); }
    writeInt16BE(v, o = 0) { return this.writeUInt16BE(v & 0xffff, o); }
    writeUInt32LE(v, o = 0) { this[o] = v; this[o + 1] = v >>> 8; this[o + 2] = v >>> 16; this[o + 3] = v >>> 24; return o + 4; }
    writeUInt32BE(v, o = 0) { this[o] = v >>> 24; this[o + 1] = v >>> 16; this[o + 2] = v >>> 8; this[o + 3] = v; return o + 4; }
    writeInt32LE(v, o = 0) { return this.writeUInt32LE(v >>> 0, o); }
    writeInt32BE(v, o = 0) { return this.writeUInt32BE(v >>> 0, o); }
    writeDoubleLE(v, o = 0) { new DataView(this.buffer, this.byteOffset).setFloat64(o, v, true); return o + 8; }
    writeDoubleBE(v, o = 0) { new DataView(this.buffer, this.byteOffset).setFloat64(o, v, false); return o + 8; }
    writeFloatLE(v, o = 0) { new DataView(this.buffer, this.byteOffset).setFloat32(o, v, true); return o + 4; }
    writeFloatBE(v, o = 0) { new DataView(this.buffer, this.byteOffset).setFloat32(o, v, false); return o + 4; }
  }
  for (const [alias, name] of [["readUint8", "readUInt8"], ["readUint16LE", "readUInt16LE"], ["readUint16BE", "readUInt16BE"], ["readUint32LE", "readUInt32LE"], ["readUint32BE", "readUInt32BE"], ["writeUint8", "writeUInt8"], ["writeUint16LE", "writeUInt16LE"], ["writeUint16BE", "writeUInt16BE"], ["writeUint32LE", "writeUInt32LE"], ["writeUint32BE", "writeUInt32BE"]]) {
    Buffer.prototype[alias] = Buffer.prototype[name];
  }
  Buffer.poolSize = 8192;
  function toBuffer(u8) {
    return new Buffer(u8.buffer, u8.byteOffset, u8.length);
  }
  return {
    Buffer,
    kMaxLength: 2 ** 32,
    constants: { MAX_LENGTH: 2 ** 32, MAX_STRING_LENGTH: 2 ** 29 - 24 },
    atob: (s) => stringOf(bytesOf(String(s), "base64"), "latin1"),
    btoa: (s) => stringOf(bytesOf(String(s), "latin1"), "base64"),
    isUtf8: () => true,
    _toBuffer: toBuffer,
  };
})
