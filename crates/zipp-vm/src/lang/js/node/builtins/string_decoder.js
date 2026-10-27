// node:string_decoder — bytes to text across chunk boundaries: a character
// split between two writes (UTF-8, UTF-16LE) or a base64 group cut short is
// held back until the rest arrives. Node's decoder state is a 7-byte buffer
// (the held bytes, then how many are missing, buffered, and the encoding),
// kept here as it is, since `lastChar`, `lastNeed` and `lastTotal` read it.
//
// Derived from Node.js (lib/string_decoder.js, src/string_decoder.cc), which
// carries this notice:
//
// Copyright Node.js contributors. All rights reserved.
// Copyright Joyent, Inc. and other Node contributors.
//
// Permission is hereby granted, free of charge, to any person obtaining a
// copy of this software and associated documentation files (the
// "Software"), to deal in the Software without restriction, including
// without limitation the rights to use, copy, modify, merge, publish,
// distribute, sublicense, and/or sell copies of the Software, and to permit
// persons to whom the Software is furnished to do so, subject to the
// following conditions:
//
// The above copyright notice and this permission notice shall be included
// in all copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS
// OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
// MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN
// NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM,
// DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR
// OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE
// USE OR OTHER DEALINGS IN THE SOFTWARE.
(function (host, builtin) {
  "use strict";
  const { Buffer } = builtin("buffer");
  const kNativeDecoder = Symbol("kNativeDecoder");
  const kIncompleteCharactersStart = 0;
  const kIncompleteCharactersEnd = 4;
  const kMissingBytes = 4;
  const kBufferedBytes = 5;
  const kEncodingField = 6;
  const kSize = 7;
  // Node's encoding enum.
  const UTF8 = 1, BASE64 = 2, UCS2 = 3, BASE64URL = 7;
  const encodingNames = ["ascii", "utf8", "base64", "utf16le", "latin1", "hex", "buffer", "base64url"];
  const encodingsMap = { ascii: 0, utf8: 1, base64: 2, utf16le: 3, latin1: 4, hex: 5, base64url: 7 };
  const aliases = {
    hex: "hex", utf8: "utf8", ucs2: "utf16le", "utf-8": "utf8", ascii: "ascii", "ucs-2": "utf16le",
    base64: "base64", latin1: "latin1", binary: "latin1", utf16le: "utf16le", "utf-16le": "utf16le", base64url: "base64url",
  };
  // V8's UTF-8 decoding, which keeps a byte order mark.
  const utf8 = new TextDecoder("utf-8", { ignoreBOM: true });

  function normalizeEncoding(enc) {
    if (enc == null || enc === "utf8" || enc === "utf-8") return "utf8";
    const n = enc.length;
    if (n >= 3 && n <= 9) {
      const lower = `${enc}`.toLowerCase();
      if (Object.prototype.hasOwnProperty.call(aliases, lower) && lower.length === n) return aliases[lower];
    } else if (enc === "") {
      return "utf8";
    }
    return undefined;
  }
  function nodeError(Base, code, message) {
    const err = new Base(message);
    Object.defineProperty(err, "toString", {
      value() { return this.name + " [" + code + "]: " + this.message; },
      writable: true,
      configurable: true,
    });
    err.code = code;
    return err;
  }
  function received(v) {
    if (v == null) return "Received " + v;
    if (typeof v === "function") return "Received function " + v.name;
    if (typeof v === "object") {
      const name = v.constructor && v.constructor.name;
      return name ? "Received an instance of " + name : "Received " + builtin("util").inspect(v, { depth: -1 });
    }
    const shown = typeof v === "bigint" ? v + "n" : Object.is(v, -0) ? "-0" : String(v);
    return "Received type " + typeof v + " (" + (shown.length > 28 ? shown.slice(0, 25) + "..." : shown) + ")";
  }

  function makeString(bytes, start, end, encoding) {
    if (encoding === UTF8) return utf8.decode(bytes.subarray(start, end));
    return Buffer.from(bytes.buffer, bytes.byteOffset + start, end - start).toString(encodingNames[encoding]);
  }
  function decode(state, buf) {
    const data = new Uint8Array(buf.buffer, buf.byteOffset, buf.byteLength);
    const encoding = state[kEncodingField];
    let pos = 0, nread = data.length;
    if (encoding !== UTF8 && encoding !== UCS2 && encoding !== BASE64 && encoding !== BASE64URL) {
      return makeString(data, 0, nread, encoding);
    }
    let prepend = "";
    // Bytes that finish a character held back from the last chunk.
    if (state[kMissingBytes] > 0) {
      if (encoding === UTF8) {
        // A byte that cannot continue the character ends it there; the
        // decoder replaces what was held as V8's does.
        for (let i = 0; i < nread && i < state[kMissingBytes]; ++i) {
          if ((data[i] & 0xc0) !== 0x80) {
            state[kMissingBytes] = 0;
            state.set(data.subarray(0, i), kIncompleteCharactersStart + state[kBufferedBytes]);
            state[kBufferedBytes] += i;
            pos += i;
            nread -= i;
            break;
          }
        }
      }
      const found = Math.min(nread, state[kMissingBytes]);
      state.set(data.subarray(pos, pos + found), kIncompleteCharactersStart + state[kBufferedBytes]);
      pos += found;
      nread -= found;
      state[kMissingBytes] -= found;
      state[kBufferedBytes] += found;
      if (state[kMissingBytes] === 0) {
        prepend = makeString(state, kIncompleteCharactersStart, state[kBufferedBytes], encoding);
        state[kBufferedBytes] = 0;
      }
    }
    if (nread === 0) return prepend;
    // A character this chunk ends partway through, held back for the next.
    if (encoding === UTF8 && data[pos + nread - 1] & 0x80) {
      for (let i = nread - 1; ; --i) {
        state[kBufferedBytes]++;
        const byte = data[pos + i];
        if ((byte & 0xc0) === 0x80) {
          // Four trailing bytes, or no lead byte at all: invalid, left to the
          // decoder.
          if (state[kBufferedBytes] >= 4 || i === 0) {
            state[kBufferedBytes] = 0;
            break;
          }
        } else {
          if ((byte & 0xe0) === 0xc0) state[kMissingBytes] = 2;
          else if ((byte & 0xf0) === 0xe0) state[kMissingBytes] = 3;
          else if ((byte & 0xf8) === 0xf0) state[kMissingBytes] = 4;
          else {
            state[kBufferedBytes] = 0;
            break;
          }
          if (state[kBufferedBytes] >= state[kMissingBytes]) {
            state[kMissingBytes] = 0;
            state[kBufferedBytes] = 0;
          }
          state[kMissingBytes] -= state[kBufferedBytes];
          break;
        }
      }
    } else if (encoding === UCS2) {
      if (nread % 2 === 1) {
        state[kBufferedBytes] = 1;
        state[kMissingBytes] = 1;
      } else if ((data[pos + nread - 1] & 0xfc) === 0xd8) {
        // The first half of a surrogate pair.
        state[kBufferedBytes] = 2;
        state[kMissingBytes] = 2;
      }
    } else if (encoding === BASE64 || encoding === BASE64URL) {
      state[kBufferedBytes] = nread % 3;
      if (state[kBufferedBytes] > 0) state[kMissingBytes] = 3 - state[kBufferedBytes];
    }
    const held = state[kBufferedBytes];
    if (held > 0) {
      nread -= held;
      state.set(data.subarray(pos + nread, pos + nread + held), kIncompleteCharactersStart);
    }
    return nread > 0 ? prepend + makeString(data, pos, pos + nread, encoding) : prepend;
  }
  function flush(state) {
    const encoding = state[kEncodingField];
    // A lone trailing byte of UTF-16 is dropped.
    if (encoding === UCS2 && state[kBufferedBytes] % 2 === 1) {
      state[kMissingBytes]--;
      state[kBufferedBytes]--;
    }
    if (state[kBufferedBytes] === 0) return "";
    const ret = makeString(state, kIncompleteCharactersStart, state[kBufferedBytes], encoding);
    state[kMissingBytes] = 0;
    state[kBufferedBytes] = 0;
    return ret;
  }

  function StringDecoder(encoding) {
    this.encoding = normalizeEncoding(encoding);
    if (this.encoding === undefined) {
      throw nodeError(TypeError, "ERR_UNKNOWN_ENCODING", builtin("util").format("Unknown encoding: %s", encoding));
    }
    this[kNativeDecoder] = Buffer.alloc(kSize);
    this[kNativeDecoder][kEncodingField] = encodingsMap[this.encoding];
  }
  StringDecoder.prototype.write = function write(buf) {
    if (typeof buf === "string") return buf;
    if (!ArrayBuffer.isView(buf)) {
      throw nodeError(TypeError, "ERR_INVALID_ARG_TYPE",
        'The "buf" argument must be an instance of Buffer, TypedArray, or DataView. ' + received(buf));
    }
    if (!this[kNativeDecoder]) {
      throw nodeError(TypeError, "ERR_INVALID_THIS", 'Value of "this" must be of type StringDecoder');
    }
    return decode(this[kNativeDecoder], buf);
  };
  StringDecoder.prototype.end = function end(buf) {
    const ret = buf === undefined ? "" : this.write(buf);
    if (this[kNativeDecoder][kBufferedBytes] > 0) return ret + flush(this[kNativeDecoder]);
    return ret;
  };
  // Undocumented, kept for the packages that use it.
  StringDecoder.prototype.text = function text(buf, offset) {
    this[kNativeDecoder][kMissingBytes] = 0;
    this[kNativeDecoder][kBufferedBytes] = 0;
    return this.write(buf.slice(offset));
  };
  Object.defineProperties(StringDecoder.prototype, {
    lastChar: {
      configurable: true,
      enumerable: true,
      get() { return this[kNativeDecoder].subarray(kIncompleteCharactersStart, kIncompleteCharactersEnd); },
    },
    lastNeed: {
      configurable: true,
      enumerable: true,
      get() { return this[kNativeDecoder][kMissingBytes]; },
    },
    lastTotal: {
      configurable: true,
      enumerable: true,
      get() { return this[kNativeDecoder][kBufferedBytes] + this[kNativeDecoder][kMissingBytes]; },
    },
  });
  return { StringDecoder };
})
