# Zipp landing page

The standalone React + TypeScript landing page for Zipp, built with Vite. Its
performance copy mirrors the clean canonical capture in
[`../bench/real13_0bff482_pgo_2026-08-30.json`](../bench/real13_0bff482_pgo_2026-08-30.json);
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
