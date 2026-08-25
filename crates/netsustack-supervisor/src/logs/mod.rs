//! Independent stores for readable logs and terminal replay data.

mod ansi;
mod plain;

pub use ansi::{AnsiChunk, AnsiReplay, AnsiTranscript, ReplayGap};
pub use plain::{PlainLogError, PlainLogStore};
