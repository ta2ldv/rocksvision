//! Format facts shared by more than one parser: log-record framing (WAL + MANIFEST),
//! internal keys / value types (WAL + SST + MANIFEST) and compression codecs (SST + WAL).
//! Parsers (`wal`, `sst`, `manifest`) depend on this module, never on each other.

pub mod codec;
pub mod ikey;
pub mod log;
