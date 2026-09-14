// Type surface of render.mjs for vite.config.ts / vite-plugin.ts. The renderer is
// plain JavaScript so the dependency-free Node tests can import it directly.
export interface SiteAssets {
  css: string
  journalJs: string
}
export interface SiteContent {
  site: { origin: string; name: string; [key: string]: unknown }
  pages: Array<{ slug: string; [key: string]: unknown }>
  journal: { entries: Array<{ slug: string; [key: string]: unknown }>; [key: string]: unknown }
}
export function renderSite(content: SiteContent, assets: SiteAssets): Map<string, string>
export function siteRoutes(content: SiteContent): string[]
export function validateContent(content: SiteContent): string[]
