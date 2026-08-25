export let version = 11;
export let transitions = 0;

export function bump(delta) {
  version = (version + delta) | 0;
  transitions = (transitions + 1) | 0;
  return version;
}

export function stateChecksum() {
  return (Math.imul(version, 131) ^ transitions) | 0;
}
