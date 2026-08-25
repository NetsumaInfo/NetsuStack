use std::collections::VecDeque;

/// A terminal byte chunk tagged with its unique stream position.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnsiChunk {
    pub sequence: u64,
    pub data: Vec<u8>,
}

/// A consistent transcript snapshot. Live delivery starts at `next_sequence`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnsiReplay {
    pub first_sequence: Option<u64>,
    pub next_sequence: u64,
    pub chunks: Vec<AnsiChunk>,
}

/// The requested continuation predates retained transcript data.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplayGap {
    pub requested_sequence: u64,
    pub available_from: u64,
    pub next_sequence: u64,
}

/// Bounded raw VT data used to rebuild an xterm instance after attachment.
#[derive(Debug)]
pub struct AnsiTranscript {
    chunks: VecDeque<AnsiChunk>,
    max_bytes: usize,
    max_lines: usize,
    byte_len: usize,
    line_count: usize,
    next_sequence: u64,
}

impl AnsiTranscript {
    pub const DEFAULT_MAX_BYTES: usize = 2 * 1024 * 1024;
    pub const DEFAULT_MAX_LINES: usize = 20_000;

    #[must_use]
    pub fn new(max_bytes: usize, max_lines: usize) -> Self {
        Self {
            chunks: VecDeque::new(),
            max_bytes: max_bytes.max(1),
            max_lines: max_lines.max(1),
            byte_len: 0,
            line_count: 0,
            next_sequence: 1,
        }
    }

    /// Returns the full live chunk. Replay retains it only when the whole chunk fits.
    pub fn push(&mut self, data: &[u8]) -> Option<AnsiChunk> {
        self.push_owned(data.to_vec())
    }

    /// Accepts an owned live chunk, avoiding another full allocation for live-only oversize data.
    pub fn push_owned(&mut self, data: Vec<u8>) -> Option<AnsiChunk> {
        if data.is_empty() || self.next_sequence == u64::MAX {
            return None;
        }

        let chunk = AnsiChunk {
            sequence: self.next_sequence,
            data,
        };
        self.next_sequence += 1;
        let incoming_lines = count_lines(&chunk.data);
        if chunk.data.len() > self.max_bytes || incoming_lines > self.max_lines {
            self.clear();
            return Some(chunk);
        }
        self.byte_len = self.byte_len.saturating_add(chunk.data.len());
        self.line_count = self.line_count.saturating_add(incoming_lines);
        self.chunks.push_back(chunk.clone());
        self.enforce_limits();
        Some(chunk)
    }

    #[must_use]
    pub const fn byte_len(&self) -> usize {
        self.byte_len
    }

    #[must_use]
    pub const fn line_count(&self) -> usize {
        self.line_count
    }

    #[must_use]
    pub fn replay(&self) -> AnsiReplay {
        self.replay_chunks(self.chunks.iter().cloned().collect())
    }

    pub fn replay_from(&self, requested_sequence: u64) -> Result<AnsiReplay, ReplayGap> {
        let available_from = self
            .chunks
            .front()
            .map_or(self.next_sequence, |chunk| chunk.sequence);
        if requested_sequence < available_from || requested_sequence > self.next_sequence {
            return Err(ReplayGap {
                requested_sequence,
                available_from,
                next_sequence: self.next_sequence,
            });
        }

        Ok(self.replay_chunks(
            self.chunks
                .iter()
                .filter(|chunk| chunk.sequence >= requested_sequence)
                .cloned()
                .collect(),
        ))
    }

    pub fn clear(&mut self) {
        self.chunks.clear();
        self.byte_len = 0;
        self.line_count = 0;
    }

    fn replay_chunks(&self, chunks: Vec<AnsiChunk>) -> AnsiReplay {
        AnsiReplay {
            first_sequence: chunks.first().map(|chunk| chunk.sequence),
            next_sequence: self.next_sequence,
            chunks,
        }
    }

    fn enforce_limits(&mut self) {
        while self.byte_len > self.max_bytes || self.line_count > self.max_lines {
            if let Some(removed) = self.chunks.pop_front() {
                self.byte_len = self.byte_len.saturating_sub(removed.data.len());
                self.line_count = self.line_count.saturating_sub(count_lines(&removed.data));
            } else {
                break;
            }
        }
    }
}

impl Default for AnsiTranscript {
    fn default() -> Self {
        Self::new(Self::DEFAULT_MAX_BYTES, Self::DEFAULT_MAX_LINES)
    }
}

fn count_lines(bytes: &[u8]) -> usize {
    bytes.iter().filter(|byte| **byte == b'\n').count()
}
