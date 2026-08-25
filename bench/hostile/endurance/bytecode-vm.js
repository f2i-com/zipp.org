"use strict";

// A long-running application loop that is hostile to exact-loop reducers: a JS
// bytecode interpreter with switch dispatch, indirect helpers, typed state and
// branchy control flow.
(function main() {
  const targetSteps = 4800000;
  const program = new Uint8Array([
    1, 0, 0, 1, 1, 1, 2, 0, 1, 3, 0, 17,
    4, 2, 0, 5, 2, 31, 6, 2, 9, 7, 2, 0,
    8, 0, 0, 9, 0, 0, 10, 0, 0, 11, 0, 0
  ]);
  const regs = new Int32Array(16);
  const memory = new Int32Array(1024);
  const helpers = [
    (value) => Math.imul(value ^ 0x45d9f3b, 33) | 0,
    (value) => ((value << 7) | (value >>> 25)) | 0,
    (value) => (value + 0x6d2b79f5) | 0
  ];

  let ip = 0;
  let steps = 0;
  let checksum = 0;
  while (steps < targetSteps) {
    const op = program[ip];
    const a = program[ip + 1];
    const b = program[ip + 2];
    ip += 3;
    switch (op) {
      case 1: regs[a] = (regs[a] + b + steps) | 0; break;
      case 2: regs[a] = (regs[a] ^ regs[b]) | 0; break;
      case 3: regs[a] = Math.imul(regs[a], b) | 0; break;
      case 4: memory[(regs[b] + steps) & 1023] = regs[a]; break;
      case 5: regs[a] = memory[(regs[a] + b) & 1023]; break;
      case 6: regs[a] = helpers[b % helpers.length](regs[a]); break;
      case 7: checksum = (checksum + regs[a] + memory[(steps + b) & 1023]) | 0; break;
      case 8: if ((regs[a] & 15) === b) ip = 0; break;
      case 9: regs[a] = (regs[a] + checksum) | 0; break;
      case 10: regs[a] = regs[a] < 0 ? -regs[a] : regs[a]; break;
      case 11: ip = 0; break;
      default: ip = 0; break;
    }
    if (ip >= program.length) ip = 0;
    steps++;
  }

  for (let i = 0; i < regs.length; i++) checksum = (checksum ^ regs[i]) | 0;
  for (let i = 0; i < memory.length; i += 17) checksum = (checksum + memory[i]) | 0;
  console.log("bytecode-vm", checksum, steps, regs[0], memory[0]);
})();
