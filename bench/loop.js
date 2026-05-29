// Same loop as loop.zipp, for a speed comparison.
const t = performance.now();
let total = 0;
let i = 1;
while (i <= 50000000) {
  total = total + i;
  i = i + 1;
}
console.log(total);
console.error("js exec ms:", (performance.now() - t).toFixed(1));
