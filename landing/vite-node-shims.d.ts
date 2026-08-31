// Minimal ambient declarations for the Node built-ins vite.config.ts uses.
//
// The config is typechecked by tsconfig.node.json, which has no `@types/node`
// and does not need one — the config touches exactly two functions. Declaring
// them here keeps the dependency out of the project for the sake of a hash.
declare module 'node:crypto' {
  export function createHash(algorithm: string): {
    update(data: Uint8Array | string): { digest(encoding: string): string } & {
      update(data: Uint8Array | string): unknown
    }
    digest(encoding: string): string
  }
}

declare module 'node:fs' {
  export function readFileSync(path: string): Uint8Array
}
