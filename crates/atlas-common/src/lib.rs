//! Wire types shared between `atlas-server` and the web app.
//!
//! Every type here derives `ts_rs::TS` with `#[ts(export)]`. Run
//! `cargo test -p atlas-common` to write the TypeScript bindings to
//! `app/src/generated/` (set by `TS_RS_EXPORT_DIR` in `.cargo/config.toml`).
//! That directory is gitignored, so a fresh clone has none until you do.
//!
//! ## `#[ts(type = "number")]` on 64-bit integers
//!
//! ts-rs maps `u64`/`i64` to `bigint`, but serde writes plain JSON numbers and
//! `JSON.parse` yields `number`. Annotate every 64-bit field (byte counts,
//! unix timestamps, counters), all of which sit far below
//! `Number.MAX_SAFE_INTEGER`.

pub mod auth;
pub mod connections;
pub mod error;
pub mod meta;
pub mod search;

pub use auth::*;
pub use connections::*;
pub use error::*;
pub use meta::*;
pub use search::*;
