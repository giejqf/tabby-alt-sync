//! A single-user [tabby-web](https://github.com/Eugeny/tabby-web) replacement:
//! just the `/api/1/configs` and `/api/1/user` endpoints Tabby's
//! *Settings → Config sync* talks to, one static bearer token, SQLite storage.
//!
//! Wire compatibility with DRF is the contract; see `AGENTS.md` and
//! `tests/compat.rs`.

pub mod api;
pub mod auth;
pub mod config;
pub mod db;
pub mod time;
