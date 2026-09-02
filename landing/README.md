# Zipp landing page

The standalone React + TypeScript landing page for Zipp, built with Vite. Its
Node/Bun/Deno performance copy is specifically the native PGO CLI evidence in
the clean four-engine canonical captures:
[`../bench/real13_c28781cf_pgo_2026-09-02.json`](../bench/real13_c28781cf_pgo_2026-09-02.json)
and
[`../bench/hostile/head_clean_c28781cf_pgo_2026-09-02.json`](../bench/hostile/head_clean_c28781cf_pgo_2026-09-02.json).
The all-30 headline is the equal-row geomean of the normal 13 and hostile 17;
development-only A/Bs do not silently change the public ratios.

The checked-in v0.0.6 browser module is the section-stripped production build:
5,480,311 bytes raw, 1,825,812 at gzip-9, and 1,233,575 at Brotli-11, with
SHA-256 `318fc5cf7ee5d55751d829419d4de5af1ab2643b8f7fd30df2e3779c16ad1691`.
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
