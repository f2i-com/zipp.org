import { bump } from "./dep.mjs";
// The namespace maps these re-exports to dep.mjs's live slots rather than to
// snapshot values owned by this barrel.
import * as ns from "./barrel.mjs";

let checksum = 0;
for (let i = 0; i < ns.ROUNDS; i++) {
  if (i === 90000) bump();
  checksum = (checksum + ns.live + (i & 3)) | 0;
}

console.log(checksum + "|" + ns.live + "|" + ns.missing);
