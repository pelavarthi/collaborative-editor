//! Operational transformation core for the collaborative editor.
//!
//! Day 1: the document model and the two operations every editor can make,
//! insert and delete. Transforming concurrent operations from different
//! sites lands on day 2.

pub mod document;
pub mod op;

pub use document::{Document, OpError};
pub use op::Op;
