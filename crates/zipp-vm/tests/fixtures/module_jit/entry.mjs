import {
  addOne,
  bump,
  explode,
  identity,
  invoke,
  live,
  makeCounter,
  readLive,
} from "./dep.mjs";

let checksum = 0;
for (let i = 0; i < 40000; i++) {
  checksum = (checksum + addOne(i & 255)) | 0;
  if ((i & 1023) === 0) checksum = (checksum ^ bump(1)) | 0;
  checksum = (checksum + invoke(identity, i & 31)) | 0;
}

// Force an int-specialized body to deopt and resume with JS string coercion.
const deopt = addOne("40");

// The callback throw crosses an already-hot imported caller frame and must
// unwind once, without re-running the call after native code reports it.
let caught = "none";
try {
  invoke(explode, 1);
} catch (error) {
  caught = error.message;
}

// Captured mutable state and the exported live binding must remain coherent
// even though neighbouring imported functions are compiled.
const counter = makeCounter(3);
let closure = 0;
for (let i = 0; i < 12000; i++) closure = counter(i & 3);

console.log(
  checksum + "|" + deopt + "|" + caught + "|" + closure + "|" + live + "|" + readLive()
);
