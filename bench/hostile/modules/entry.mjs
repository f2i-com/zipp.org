"use strict";

import transform, {
  bump,
  consume,
  stateChecksum,
  transitions,
  version,
} from "./barrel.mjs";
import * as constants from "./constants.mjs";

const initialVersion = version;
bump(3);
if (version !== initialVersion + 3 || transitions !== 1) {
  throw new Error("live module binding did not update");
}

let checksum = consume(constants.SALT, 64);
for (let i = 0; i < constants.ROUNDS; i++) {
  if ((i & 0xffff) === 0) bump((i >>> 16) & 3);
  checksum = transform((checksum + i) | 0, i & (constants.LANES - 1));
}
checksum = (checksum ^ stateChecksum() ^ version ^ transitions) >>> 0;

console.log(
  "module checksum=" + checksum +
    " version=" + version +
    " transitions=" + transitions
);
