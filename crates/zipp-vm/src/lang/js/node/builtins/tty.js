// node:tty — whether a stream is a terminal.
(function (host) {
  "use strict";
  class ReadStream {
    constructor(fd) { this.fd = fd; this.isTTY = host.isatty(fd); }
    setRawMode() { return this; }
  }
  class WriteStream {
    constructor(fd) { this.fd = fd; this.isTTY = host.isatty(fd); this.columns = 80; this.rows = 24; }
    getColorDepth() { return 1; }
    hasColors() { return false; }
    getWindowSize() { return [this.columns, this.rows]; }
  }
  return { isatty: (fd) => host.isatty(+fd), ReadStream, WriteStream };
})
