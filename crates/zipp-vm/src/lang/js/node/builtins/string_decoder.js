// node:string_decoder — bytes to text across chunk boundaries.
(function (host, builtin) {
  "use strict";
  const { Buffer } = builtin("buffer");
  class StringDecoder {
    constructor(encoding = "utf8") {
      this.encoding = String(encoding).toLowerCase().replace("-", "");
      this._decoder = this.encoding === "utf8" ? new TextDecoder("utf-8") : null;
    }
    write(buf) {
      if (typeof buf === "string") return buf;
      return this._decoder ? this._decoder.decode(buf, { stream: true }) : Buffer.from(buf).toString(this.encoding);
    }
    end(buf) {
      let out = buf ? this.write(buf) : "";
      if (this._decoder) out += this._decoder.decode();
      return out;
    }
  }
  return { StringDecoder };
})
