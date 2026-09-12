//! Adapters that turn a harness's own storage into `domain` types.

pub mod claude;
pub mod codex;
pub mod herdr_refs;
pub mod jsonl;
pub mod message;
pub mod registry;

pub use message::{ExtractedMessage, Role};
