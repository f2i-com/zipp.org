# Zipp landing page

The standalone React + TypeScript landing page for Zipp, built with Vite. Its
Node/Bun/Deno performance copy is specifically the native PGO CLI evidence in
the clean four-engine canonical captures:
[`../bench/real13_c28781cf_pgo_2026-09-02.json`](../bench/real13_c28781cf_pgo_2026-09-02.json)
and
[`../bench/hostile/head_clean_c28781cf_pgo_2026-09-02.json`](../bench/hostile/head_clean_c28781cf_pgo_2026-09-02.json).
The all-30 headline is the equal-row geomean of the normal 13 and hostile 17;
development-only A/Bs do not silently change the public ratios. At runtime the
page also asks `api/stats.php` (a small PHP endpoint in `public/api/`) for the
live repository facts -- version, commit count, stars, latest release and the
README's own figures -- and fills them in when it answers; a static host keeps
the built-in figures.

The checked-in browser module is the section-stripped production build of the
engine at v0.0.12 (`crates/zipp-wasm/README.md` has the exact recipe): 5,558,860
bytes raw, 1,812,458 at gzip-9, and 1,248,649 at Brotli-11, with SHA-256
`bd8614fe5f3a3b8ef67f4b917cdefebb3fe69afa39a9804a0d3f6b0b6b267126`.
The separate pinned QuickJS-NG and Boa comparison documents both the incomplete
exact-suite WASM attempt and the specialization-sensitive micro diagnostic in
[`../bench/comparison/README.md`](../bench/comparison/README.md).

```sh
npm install
npm run dev
```

Before publishing, run:

```sh
npm run typecheck
npm run build
```

The production bundle is written to `dist/`. The bolt favicon lives in
`public/zipp-bolt.svg`, and the 1200×630 social card lives in
`public/zipp-og-card.png`.

The OpenAI Sites and Cloudflare Vite adapters emit the deployment metadata,
static assets, and Worker-compatible bundle used by the private hosted preview.
