export let live = 7;
export const ROUNDS = 180000;

export function bump() {
  live = (live + 1) | 0;
}
