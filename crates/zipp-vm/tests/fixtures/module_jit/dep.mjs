export let live = 7;

export function addOne(value) {
  return (value + 1) | 0;
}

export function bump(delta) {
  live = (live + delta) | 0;
  return live;
}

export function readLive() {
  return live;
}

export function invoke(callback, value) {
  return callback(value) + 3;
}

export function identity(value) {
  return value;
}

export function explode() {
  throw new Error("module-boom");
}

export function makeCounter(seed) {
  let captured = seed;
  return function step(delta) {
    captured = (captured + delta) | 0;
    live = (live + (captured & 3)) | 0;
    return captured;
  };
}
