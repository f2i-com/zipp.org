// Reduced from a Game Boy emulator's save-state sizer, which derives the exact
// byte count of a state from the arrays themselves so a change to the register
// file cannot leave a stale constant behind.
//
// One expression, twenty-two operands, twenty-one additive links. Written this
// way on purpose: a running total across twenty-two statements would let the
// list and the sum drift apart. The 16-link limit shipped in v0.0.1 rejected
// it, which is what a chain limit set below ordinary arithmetic looks like.

var STATE_HEADER = 64;

function stateBytes() {
  return STATE_HEADER +
    REG.length +
    REG16.byteLength +
    CPUF.length +
    CTR.byteLength +
    CART.byteLength +
    RTC.byteLength +
    RTC_EPOCH.byteLength +
    PAD.length +
    IO.length +
    HRAM.length +
    OAM.length +
    WRAM.length +
    VRAM.length +
    BGPAL.length +
    OBJPAL.length +
    APU.byteLength +
    APU_G.byteLength +
    WAVERAM.length +
    FB.length +
    FB_PRESENT.length +
    ramBytes();
}

// The same shape as a member chain rather than an operator chain: the parser
// builds both iteratively and both leave a left spine for the compiler to walk.
function presentBuffer(machine) {
  return machine.ppu.frame.present.planes.rgba.buffer.view.bytes.data.slice(0);
}
