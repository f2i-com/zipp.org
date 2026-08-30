# Zipp landing page

The standalone React + TypeScript landing page for Zipp, built with Vite. Its
performance copy mirrors the clean four-engine canonical captures in
[`../bench/real13_21288c1_pgo_2026-08-30.json`](../bench/real13_21288c1_pgo_2026-08-30.json)
and
[`../bench/hostile/head_clean_21288c1_pgo_2026-08-30.json`](../bench/hostile/head_clean_21288c1_pgo_2026-08-30.json).
The all-30 headline is the equal-row geomean of the normal 13 and hostile 17;
development-only A/Bs do not silently change the public ratios.

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
