"use strict";
function countdown(n) {
  while (n > 0) n--;
  return n;
}
function carried(n, acc) {
  while (n > 0) {
    acc = (acc * 3 + n) % 16381;
    n--;
  }
  return acc;
}
function twoBackedges(n, acc) {
  while (n > 0) {
    if ((n & 1) === 0) {
      acc += 11;
      n--;
      continue;
    }
    acc += 3;
    n--;
  }
  return acc;
}
function nested(n, acc) {
  while (n > 0) {
    for (let j = 0; j < 6; j++) acc += (n * j) % 7;
    n--;
  }
  return acc;
}
function mixed(n, acc) {
  while (n > 0) {
    acc += n;
    n--;
  }
  return acc;
}
function unknownLocal(n) {
  var value;
  while (n > 0) {
    value = value === undefined ? 1 : value + 2;
    n--;
  }
  return value === undefined ? -1 : value;
}
let counters = [0, 0, 0, 0, 0, 0, 0];
for (let i = 0; i < 1800; i++) {
  counters[0] += countdown(i % 13);
  counters[1] += carried(i % 19, i % 23);
  counters[2] += twoBackedges(i % 21, i % 10);
  counters[3] += nested(i % 13, i);
  counters[4] += mixed(i % 14, 2);
  counters[5] += unknownLocal(i % 15);
  counters[6] += mixed(i % 4, 0.25);
}
console.log(counters.join(" "));
console.log(mixed(3, "x"));
console.log(countdown(-2));
