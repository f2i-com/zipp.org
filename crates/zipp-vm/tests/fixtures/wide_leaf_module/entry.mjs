import { bump, live, setLive, transform } from "./kernel.mjs";

let checksum = 0x12345678;
for (let i = 0; i < 180000; i++) {
  if (i === 60000) bump();
  if (i === 120000) setLive("13");
  checksum = transform((checksum + i) | 0, i & 7);
}

console.log(checksum + "|" + live);
