/** Pass one bounded data value into the guest, without exposing host APIs. */
export function injectStorySeed(source: string, seed: unknown): string {
  if (typeof seed !== 'number' || !Number.isInteger(seed) || seed < 0 || seed > 0xffffffff) return source
  return `const STORY_SEED = ${seed};\n${source}`
}
