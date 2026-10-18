// Type surface of embed-data.mjs, which is plain JavaScript so the starter
// generator and the dependency-free Node tests can import it directly.
export interface EmbedAsset {
  /** The release file name, e.g. zipp-0.0.26-javascript.wasm. */
  file: string
  bytes: number
  /** Size of the Brotli-compressed `<file>.br` the release also publishes. */
  brBytes: number
}
export interface EmbedEngine extends EmbedAsset {
  id: 'javascript' | 'lite' | 'javascript-python'
  label: string
  summary: string
  /** The language the examples run on this engine. */
  language: 'javascript' | 'python'
}
export interface SandboxOptions {
  language?: 'javascript' | 'python'
  permissions?: Record<string, unknown>
}

export const EMBED_RELEASE: string
export const REPOSITORY_URL: string
export const RELEASE_TAG_URL: string
export function releaseAssetUrl(name: string): string
export const KIT: { file: string; folder: string; bytes: number; loader: string }
export const STARTER: { folder: string; path: string }
export const PYTHON_BASE: EmbedAsset
export const TORCH_PACKAGE: EmbedAsset
export const ENGINES: EmbedEngine[]
export const DEFAULT_ENGINE: EmbedEngine
export function engineById(id: string): EmbedEngine
export function formatMegabytes(bytes: number): string
export function defaultProgram(engine: EmbedEngine): string
export function defaultOutput(engine: EmbedEngine): string[]
export function defaultOptions(engine: EmbedEngine): SandboxOptions | undefined
export function defaultExample(engine: EmbedEngine): string
export function capabilityTitle(engine: EmbedEngine): string
export function capabilityBlurb(engine: EmbedEngine): string
export function capabilityOptions(engine: EmbedEngine): SandboxOptions
export function capabilityProgram(engine: EmbedEngine): string
export function capabilityOutput(engine: EmbedEngine): string[]
export function capabilityStorage(engine: EmbedEngine): Record<string, string> | null
export function capabilityExample(engine: EmbedEngine): string
export const EMBED_CSP: string
