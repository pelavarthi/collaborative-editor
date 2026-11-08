//! Operational transformation core for the collaborative editor.
//!
//! Day 1: the document model and the two operations every editor can make,
//! insert and delete. Day 2: transforming concurrent operations so two
//! sites that edit at the same time still land on the same document.

pub mod document;
pub mod op;
pub mod transform;

pub use document::{Document, OpError};
pub use op::Op;
pub use transform::{transform, transform_pair};
