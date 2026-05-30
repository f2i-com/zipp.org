//! Database bridge — CRUD + sync over an embedder-defined store.
//!
//! [`DbBridge`] lets a host plug any record-oriented backend (SQLite,
//! FoundationDB, a JS object, …) behind the VM's `db.query/create/update`
//! syntax. Records are opaque JSON blobs as far as the engine is concerned;
//! any shape validation is the host's responsibility.
//!
//! # Writing an implementation
//!
//! ```
//! use zipp_engine::db_bridge::{DbBridge, DbRecord, DbSyncStatus};
//!
//! /// An in-memory store that keeps a `Vec<DbRecord>` — useful for tests.
//! pub struct MemoryDb {
//!     rows: Vec<DbRecord>,
//!     next_id: u64,
//! }
//!
//! impl DbBridge for MemoryDb {
//!     fn query(&self, collection: &str) -> Result<Vec<DbRecord>, String> {
//!         Ok(self
//!             .rows
//!             .iter()
//!             .filter(|r| r.collection == collection)
//!             .cloned()
//!             .collect())
//!     }
//!     fn create(&mut self, collection: &str, data: &str) -> Result<DbRecord, String> {
//!         self.next_id += 1;
//!         let rec = DbRecord {
//!             id: self.next_id.to_string(),
//!             collection: collection.to_string(),
//!             data: data.to_string(),
//!             created_at: String::new(),
//!             updated_at: String::new(),
//!             data_parsed: None,
//!         };
//!         self.rows.push(rec.clone());
//!         Ok(rec)
//!     }
//!     # fn update(&mut self, _id: &str, _data: &str) -> Result<Option<DbRecord>, String> { unimplemented!() }
//!     # fn delete(&mut self, _id: &str) -> Result<(), String> { unimplemented!() }
//!     # fn hard_delete(&mut self, _collection: &str, _id: &str) -> Result<(), String> { unimplemented!() }
//!     # fn get(&self, _collection: &str, _id: &str) -> Result<Option<DbRecord>, String> { unimplemented!() }
//!     # fn start_sync(&mut self, _room: &str) {}
//!     # fn stop_sync(&mut self, _room: Option<&str>) {}
//!     # fn get_sync_status(&self, _room: Option<&str>) -> DbSyncStatus {
//!     #     DbSyncStatus { connected: false, peers: 0, room: String::new() }
//!     # }
//!     # fn get_saved_sync_room(&self) -> Option<String> { None }
//! }
//! ```
//!
//! Note the `data` field contract: the JSON *string* passed to `create`
//! and `update` is what the host must persist; any transformation should
//! happen inside the bridge. The optional `data_parsed` on returned
//! records is a fast-path — when it's `Some`, the VM skips the
//! `serde_json::from_str(&data)` call.

/// A single record returned from the host store.
///
/// `data` is the canonical JSON representation; `data_parsed` is an
/// optional pre-parsed copy that lets the engine skip `from_str`.
/// Implementations that already have a live `serde_json::Value` on hand
/// (e.g. a JS `JsValue` walk in the WASM bridge) should populate
/// `data_parsed` and leave `data` empty.
#[derive(Clone, Debug)]
pub struct DbRecord {
    /// Stable primary key chosen by the bridge. IDs do not need to be
    /// numeric; the engine treats them as opaque strings.
    pub id: String,
    /// Which logical table / collection the record belongs to.
    pub collection: String,
    /// JSON-encoded payload. May be empty when `data_parsed` is set.
    pub data: String,
    /// ISO-8601 creation timestamp (as a string).
    pub created_at: String,
    /// ISO-8601 last-modified timestamp (as a string).
    pub updated_at: String,
    /// Optional pre-parsed payload. When `Some`, the engine uses this
    /// directly and never reparses `data`.
    pub data_parsed: Option<serde_json::Value>,
}

/// Connection status reported by the sync subsystem.
#[derive(Clone, Debug)]
pub struct DbSyncStatus {
    /// True when a transport is live and peers could, in principle, be
    /// reached. A `true` value does not imply any peers are actually
    /// connected — see `peers`.
    pub connected: bool,
    /// Count of peers currently joined to the sync room.
    pub peers: usize,
    /// The active sync room identifier (empty when disconnected).
    pub room: String,
}

/// Host integration for database-style storage.
///
/// The VM holds a `Box<dyn DbBridge>` per [`crate::engine::ScriptState`];
/// `null`/`UNDEFINED` is returned for scripts that try to hit the DB
/// without a bridge attached. Implementations run on the VM's thread, so
/// they must be fast (or dispatch to another thread and block).
pub trait DbBridge {
    /// Return every record currently in `collection`.
    fn query(&self, collection: &str) -> Result<Vec<DbRecord>, String>;
    /// Insert a new record; returning the record with `id`,
    /// `created_at`, and `updated_at` populated.
    fn create(&mut self, collection: &str, data: &str) -> Result<DbRecord, String>;
    /// Update an existing record by ID. Returning `Ok(None)` means "no
    /// record with that ID" and surfaces to the script as `undefined`.
    fn update(&mut self, id: &str, data: &str) -> Result<Option<DbRecord>, String>;
    /// Soft-delete: typically flips a "deleted_at" flag so sync peers
    /// pick up the tombstone. Use `hard_delete` for permanent removal.
    fn delete(&mut self, id: &str) -> Result<(), String>;
    /// Permanently remove a record. No tombstone.
    fn hard_delete(&mut self, collection: &str, id: &str) -> Result<(), String>;
    /// Point lookup by primary key.
    fn get(&self, collection: &str, id: &str) -> Result<Option<DbRecord>, String>;
    /// Join the named sync room. Host-specific.
    fn start_sync(&mut self, room: &str);
    /// Leave a sync room (or all rooms when `None`).
    fn stop_sync(&mut self, room: Option<&str>);
    /// Current connection state.
    fn get_sync_status(&self, room: Option<&str>) -> DbSyncStatus;
    /// Return the last room the bridge auto-joined, if the host
    /// persists one across sessions.
    fn get_saved_sync_room(&self) -> Option<String>;
}
