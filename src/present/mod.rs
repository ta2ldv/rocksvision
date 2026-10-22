//! Presenters: turn parser state, typed structs and `Fact`s into `View`s (rows, columns with
//! semantic roles, explanations, navigation links, derived-space titles) and lazy tree
//! children. Presenters may read every parser; parsers never import presenters.

pub mod common;
pub mod manifest;
pub mod sst;
pub mod wal;

#[cfg(test)]
mod kind_tests;
