// Bridge fixture for the browser Worker smoke test: an in-memory db and
// localStorage built INSIDE the Worker by zipp-host.worker.mjs. Frozen,
// null-prototype adapters with own data-property methods, as the README
// asks of trusted host code.
export default function makeBridges() {
  const rows = Object.create(null);
  let nextId = 0;
  const store = Object.create(null);
  const db = Object.freeze(Object.assign(Object.create(null), {
    query: (c) => (rows[c] || []).slice(),
    get: (c, id) => (rows[c] || []).find((r) => r.id === id) || null,
    create: (c, d) => { const r = { ...d, id: "id" + ++nextId }; (rows[c] ||= []).push(r); return r; },
    update: (id, d) => ({ id, ...d }),
    delete: () => {},
    hardDelete: () => {},
    startSync: () => {},
    stopSync: () => {},
    getSyncStatus: () => ({ connected: false }),
    getSavedSyncRoom: () => null,
  }));
  const localStorage = Object.freeze(Object.assign(Object.create(null), {
    getItem: (k) => (k in store ? store[k] : null),
    setItem: (k, v) => { store[k] = String(v); },
    removeItem: (k) => { delete store[k]; },
    clear: () => { for (const k of Object.keys(store)) delete store[k]; },
  }));
  return { db, localStorage };
}
