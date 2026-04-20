//! Low-level byte cursor over source input.

/// A cursor into a byte slice with peek/advance operations.
pub struct Cursor<'src> {
    src: &'src [u8],
    pos: usize,
}

#[allow(dead_code)] // Library-style helpers; currently-unused variants round out the Cursor API.
impl<'src> Cursor<'src> {
    pub fn new(src: &'src [u8]) -> Self {
        Self { src, pos: 0 }
    }

    /// Current byte offset.
    pub fn pos(&self) -> usize {
        self.pos
    }

    /// Whether we've consumed all input.
    pub fn is_eof(&self) -> bool {
        self.pos >= self.src.len()
    }

    /// Peek at the current byte without advancing.
    pub fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }

    /// Peek at the byte `n` positions ahead.
    pub fn peek_nth(&self, n: usize) -> Option<u8> {
        self.src.get(self.pos + n).copied()
    }

    /// Advance by one byte and return it.
    pub fn advance(&mut self) -> Option<u8> {
        let b = self.src.get(self.pos).copied()?;
        self.pos += 1;
        Some(b)
    }

    /// Advance by `n` bytes.
    pub fn skip(&mut self, n: usize) {
        self.pos = (self.pos + n).min(self.src.len());
    }

    /// Consume bytes while `predicate` returns true.
    pub fn eat_while(&mut self, predicate: impl Fn(u8) -> bool) {
        while let Some(b) = self.peek() {
            if predicate(b) {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    /// Extract a slice from `start` to current position.
    pub fn slice_from(&self, start: usize) -> &'src [u8] {
        &self.src[start..self.pos]
    }

    /// Check if the remaining input starts with `needle`.
    pub fn starts_with(&self, needle: &[u8]) -> bool {
        self.src[self.pos..].starts_with(needle)
    }

    /// Remaining bytes.
    pub fn remaining(&self) -> &'src [u8] {
        &self.src[self.pos..]
    }
}
