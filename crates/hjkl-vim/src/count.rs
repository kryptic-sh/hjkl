//! Digit-prefix count accumulator for the vim grammar.
//!
//! Vim's count prefix: typing `5j` means "move down 5 lines". Digits
//! accumulate until a non-digit key arrives, then the accumulated
//! count is consumed by that key's action.
//!
//! Vim quirk: `0` is a digit only when the buffer is non-empty
//! (so `10j` works but `0` alone is the LineStart motion). The host
//! detects this case via [`CountAccumulator::try_accumulate`] returning
//! `false` for a `0` with empty buffer, and routes the `0` through the
//! keymap path as a motion key.

/// Digit-prefix count accumulator for the vim grammar.
///
/// Tracks a running count as digits are typed. Resets when consumed.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CountAccumulator {
    /// Accumulated count. `0` means "no count specified".
    buffer: u32,
}

impl CountAccumulator {
    /// Create a new, empty accumulator.
    pub const fn new() -> Self {
        Self { buffer: 0 }
    }

    /// True iff no digits have been accumulated.
    pub const fn is_empty(&self) -> bool {
        self.buffer == 0
    }

    /// Peek at the current count without resetting. Returns 0 when empty.
    pub const fn peek(&self) -> u32 {
        self.buffer
    }

    /// Try to accumulate a digit character.
    ///
    /// Returns `true` if the digit was consumed; `false` otherwise —
    /// either because `ch` is not an ASCII digit, OR because it's `0`
    /// with an empty buffer (vim's LineStart-vs-digit-0 split). The
    /// caller routes `false` results through the keymap.
    ///
    /// Saturates at `u32::MAX` to guard pathological input.
    pub fn try_accumulate(&mut self, ch: char) -> bool {
        if !ch.is_ascii_digit() {
            return false;
        }
        if ch == '0' && self.buffer == 0 {
            return false;
        }
        let d = (ch as u8 - b'0') as u32;
        self.buffer = self.buffer.saturating_mul(10).saturating_add(d);
        true
    }

    /// Drain the buffer, returning the count or `default` if empty.
    /// Resets state.
    pub fn take_or(&mut self, default: u32) -> u32 {
        let c = if self.buffer == 0 {
            default
        } else {
            self.buffer
        };
        self.buffer = 0;
        c
    }

    /// Reset the buffer without taking. Used when a non-chord-starter
    /// key arrives and the digits need to be replayed elsewhere — call
    /// [`drain_as_digits`] first if you need the chars.
    pub fn reset(&mut self) {
        self.buffer = 0;
    }

    /// Drain the buffer as the digit characters that were typed,
    /// preserving order. Used by the host to replay digits into the
    /// engine FSM when the next key is not a hjkl-vim binding (e.g.
    /// engine still owns `p` / `u` / etc. and needs count via FSM).
    ///
    /// Resets state. Returns empty string when buffer is empty.
    pub fn drain_as_digits(&mut self) -> String {
        let s = if self.buffer == 0 {
            String::new()
        } else {
            self.buffer.to_string()
        };
        self.buffer = 0;
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_is_empty() {
        let acc = CountAccumulator::new();
        assert!(acc.is_empty());
        assert_eq!(acc.peek(), 0);
    }

    #[test]
    fn try_accumulate_digit_increments() {
        let mut acc = CountAccumulator::new();
        assert!(acc.try_accumulate('5'));
        assert_eq!(acc.peek(), 5);
        assert!(!acc.is_empty());
    }

    #[test]
    fn try_accumulate_zero_with_empty_buffer_returns_false() {
        // vim quirk: `0` with empty buffer is LineStart, not a digit
        let mut acc = CountAccumulator::new();
        assert!(!acc.try_accumulate('0'));
        assert!(acc.is_empty());
    }

    #[test]
    fn try_accumulate_zero_with_non_empty_buffer_appends() {
        // `10j` must work: '1' then '0' → buffer = 10
        let mut acc = CountAccumulator::new();
        assert!(acc.try_accumulate('1'));
        assert!(acc.try_accumulate('0'));
        assert_eq!(acc.peek(), 10);
    }

    #[test]
    fn try_accumulate_non_digit_returns_false() {
        let mut acc = CountAccumulator::new();
        assert!(!acc.try_accumulate('j'));
        assert!(!acc.try_accumulate(' '));
        assert!(!acc.try_accumulate('g'));
        assert!(acc.is_empty());
    }

    #[test]
    fn take_or_drains_and_returns_count() {
        let mut acc = CountAccumulator::new();
        acc.try_accumulate('5');
        assert_eq!(acc.take_or(1), 5);
        // Buffer must be cleared after take.
        assert!(acc.is_empty());
        assert_eq!(acc.take_or(1), 1);
    }

    #[test]
    fn take_or_returns_default_when_empty() {
        let mut acc = CountAccumulator::new();
        assert_eq!(acc.take_or(1), 1);
        assert_eq!(acc.take_or(42), 42);
    }

    #[test]
    fn drain_as_digits_returns_typed_chars_in_order() {
        let mut acc = CountAccumulator::new();
        acc.try_accumulate('1');
        acc.try_accumulate('2');
        acc.try_accumulate('3');
        let s = acc.drain_as_digits();
        assert_eq!(s, "123");
        assert!(acc.is_empty());
    }

    #[test]
    fn drain_as_digits_empty_returns_empty_string() {
        let mut acc = CountAccumulator::new();
        let s = acc.drain_as_digits();
        assert_eq!(s, "");
    }

    #[test]
    fn try_accumulate_saturates_on_overflow() {
        // Push many '9's — should saturate at u32::MAX without panicking.
        let mut acc = CountAccumulator::new();
        for _ in 0..20 {
            acc.try_accumulate('9');
        }
        assert_eq!(acc.peek(), u32::MAX);
    }

    #[test]
    fn reset_clears_without_returning() {
        let mut acc = CountAccumulator::new();
        acc.try_accumulate('7');
        assert!(!acc.is_empty());
        acc.reset();
        assert!(acc.is_empty());
        assert_eq!(acc.peek(), 0);
    }
}
