// Reduced from a Game Boy emulator's sprite scanline renderer. Nothing here is
// nested for its own sake: the priority mode and the x-flip are constant for a
// whole sprite, so the tests are lifted out of the pixel loop rather than
// re-decided eight times per sprite. That hoisting is the entire optimisation,
// and it is what puts a `for` inside an `if` inside an `if` inside a `for`
// inside a function -- ordinary emulator code, six blocks deep.
//
// This file parsed under v0.0.1 with single-digit headroom. It is here as the
// near miss: a limit set just above the corpus is a limit that rejects the next
// sprite mode someone adds.

function renderSpriteLine(line, sprites, pmode) {
  for (var idx = 0; idx < sprites.length; idx = idx + 1) {
    var attr = OAM[sprites[idx] + 3];
    var sx = OAM[sprites[idx] + 1] - 8;
    var xs = sx < 0 ? 0 : sx;
    var xe = sx + 8 > SCREEN_W ? SCREEN_W : sx + 8;
    var owner = idx + 1;
    var fbase = line * SCREEN_W;
    var pbase = (attr & 0x10) !== 0 ? 8 : 4;
    var pshift = PALETTE[(attr & 7) << 1];

    var cbase = CACHE_BASE - sx;
    var step = 1;
    if ((attr & 0x20) !== 0) {
      cbase = CACHE_BASE + 7 + sx;
      step = -1;
    }

    if (pmode === 0) {
      if (step === 1) {
        for (var x = xs; x < xe; x = x + 1) {
          if (TILECACHE[cbase + x] !== 0 && LINE_OWNER[x] === 0) {
            LINE_OWNER[x] = owner;
            FB[fbase + x] = pbase | ((pshift >>> (TILECACHE[cbase + x] << 1)) & 3);
          }
        }
      } else {
        for (var xr = xs; xr < xe; xr = xr + 1) {
          if (TILECACHE[cbase - xr] !== 0 && LINE_OWNER[xr] === 0) {
            LINE_OWNER[xr] = owner;
            FB[fbase + xr] = pbase | ((pshift >>> (TILECACHE[cbase - xr] << 1)) & 3);
          }
        }
      }
    } else if (step === 1) {
      for (var xp = xs; xp < xe; xp = xp + 1) {
        if (TILECACHE[cbase + xp] !== 0 && LINE_OWNER[xp] === 0) {
          if (spriteWins(pmode, fbase + xp, xp)) {
            LINE_OWNER[xp] = owner;
            FB[fbase + xp] = pbase | ((pshift >>> (TILECACHE[cbase + xp] << 1)) & 3);
          }
        }
      }
    } else {
      for (var xq = xs; xq < xe; xq = xq + 1) {
        if (TILECACHE[cbase - xq] !== 0 && LINE_OWNER[xq] === 0) {
          if (spriteWins(pmode, fbase + xq, xq)) {
            LINE_OWNER[xq] = owner;
            FB[fbase + xq] = pbase | ((pshift >>> (TILECACHE[cbase - xq] << 1)) & 3);
          }
        }
      }
    }
  }
}

// The instruction decoder from the same emulator, reduced to one column. A
// switch over an opcode byte is flat, but every arm carries its own expression
// tree and the whole thing sits inside the step function's own nesting.
function stepOnce(cpu) {
  switch (MEM[cpu.pc]) {
    case 0x00: cpu.pc = (cpu.pc + 1) & 0xffff; return 4;
    case 0x01: {
      cpu.bc = MEM[cpu.pc + 1] | (MEM[cpu.pc + 2] << 8);
      cpu.pc = (cpu.pc + 3) & 0xffff;
      return 12;
    }
    case 0x20: {
      if ((cpu.f & 0x80) === 0) {
        cpu.pc = (cpu.pc + 2 + ((MEM[cpu.pc + 1] << 24) >> 24)) & 0xffff;
        return 12;
      }
      cpu.pc = (cpu.pc + 2) & 0xffff;
      return 8;
    }
    default:
      throw new Error("unimplemented opcode " + MEM[cpu.pc].toString(16));
  }
}
