//! Minimal async line editor for serial consoles.

use alloc::vec::Vec;

use embedded_io_async::{Read, Write};

/// How typed characters are echoed back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Echo {
    /// Echo characters as typed.
    Plain,
    /// Echo this character instead of each typed character.
    Mask(char),
    /// Do not echo anything.
    Hidden,
}

/// Result of reading a line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Line {
    /// Enter was pressed.
    Done,
    /// Ctrl-C was pressed.
    Cancelled,
    /// The IO reported end of stream (a read returned 0 bytes).
    Eof,
}

const CTRL_C: u8 = 0x03;
const BACKSPACE: u8 = 0x08;
const ESC: u8 = 0x1b;
const DEL: u8 = 0x7f;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum EscState {
    #[default]
    None,
    Esc,
    Csi,
}

/// Line reader state that must survive between lines: unread input (e.g.
/// from pasted text) and CRLF / escape sequence tracking.
#[derive(Debug, Default)]
pub(crate) struct LineReader {
    pending: [u8; 16],
    pending_start: usize,
    pending_end: usize,
    last_was_cr: bool,
    esc: EscState,
    /// Skipping the rest of a character that did not fit into the line.
    skip_continuation: bool,
}

impl LineReader {
    /// Read one line into `buf` (cleared first), at most `max` bytes.
    pub(crate) async fn read_line<IO: Read + Write>(
        &mut self,
        io: &mut IO,
        buf: &mut Vec<u8>,
        max: usize,
        echo: Echo,
    ) -> Result<Line, IO::Error> {
        buf.clear();
        self.skip_continuation = false;
        // Echo is collected per received chunk and written at once.
        let mut echoed: Vec<u8> = Vec::new();
        loop {
            if self.pending_start == self.pending_end {
                io.write_all(&echoed).await?;
                echoed.clear();
                io.flush().await?;
                self.pending_end = io.read(&mut self.pending).await?;
                self.pending_start = 0;
                if self.pending_end == 0 {
                    // `embedded-io` defines a 0 byte read as end of stream:
                    // reading again would spin forever.
                    buf.clear();
                    return Ok(Line::Eof);
                }
            }
            while self.pending_start < self.pending_end {
                let byte = self.pending[self.pending_start];
                self.pending_start += 1;
                let was_cr = core::mem::replace(&mut self.last_was_cr, false);

                // Swallow ANSI escape sequences (arrow keys etc.).
                match self.esc {
                    EscState::Esc => {
                        self.esc = if byte == b'[' {
                            EscState::Csi
                        } else {
                            EscState::None
                        };
                        continue;
                    }
                    EscState::Csi => {
                        if (0x40..=0x7e).contains(&byte) {
                            self.esc = EscState::None;
                        }
                        continue;
                    }
                    EscState::None => {}
                }

                match byte {
                    b'\n' if was_cr => {}
                    b'\r' | b'\n' => {
                        self.last_was_cr = byte == b'\r';
                        echoed.extend_from_slice(b"\r\n");
                        io.write_all(&echoed).await?;
                        io.flush().await?;
                        return Ok(Line::Done);
                    }
                    CTRL_C => {
                        echoed.extend_from_slice(b"^C\r\n");
                        io.write_all(&echoed).await?;
                        io.flush().await?;
                        buf.clear();
                        return Ok(Line::Cancelled);
                    }
                    BACKSPACE | DEL => {
                        if pop_char(buf) && echo != Echo::Hidden {
                            echoed.extend_from_slice(b"\x08 \x08");
                        }
                    }
                    ESC => self.esc = EscState::Esc,
                    b if b < 0x20 => {}
                    b => {
                        let starts_char = b & 0xc0 != 0x80;
                        if starts_char {
                            // Accept a character only if all of its bytes fit,
                            // so the line is never cut inside a character.
                            self.skip_continuation = buf.len() + utf8_width(b) > max;
                        }
                        if self.skip_continuation || buf.len() >= max {
                            continue;
                        }
                        buf.push(b);
                        // Only echo once per character (skip UTF-8 continuation bytes).
                        match echo {
                            Echo::Plain => echoed.push(b),
                            Echo::Mask(c) if starts_char => {
                                let mut tmp = [0u8; 4];
                                echoed.extend_from_slice(c.encode_utf8(&mut tmp).as_bytes());
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }
}

/// Length of the UTF-8 sequence starting with `lead` (1 for invalid lead bytes).
fn utf8_width(lead: u8) -> usize {
    match lead {
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => 1,
    }
}

/// Remove the last (possibly multi-byte) character. Returns whether anything was removed.
fn pop_char(buf: &mut Vec<u8>) -> bool {
    let Some(mut idx) = buf.len().checked_sub(1) else {
        return false;
    };
    while idx > 0 && buf[idx] & 0xc0 == 0x80 {
        idx -= 1;
    }
    buf.truncate(idx);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::tests::MockIo;
    use futures::executor::block_on;

    fn read(input: &[u8], echo: Echo) -> (Vec<Vec<u8>>, alloc::string::String) {
        block_on(async {
            let mut io = MockIo::new(input);
            let mut r = LineReader::default();
            let mut lines = Vec::new();
            let mut buf = Vec::new();
            while !io.input_exhausted() {
                match r.read_line(&mut io, &mut buf, 64, echo).await.unwrap() {
                    Line::Done => lines.push(buf.clone()),
                    Line::Cancelled => lines.push(b"<cancel>".to_vec()),
                    Line::Eof => lines.push(b"<eof>".to_vec()),
                }
            }
            (lines, io.output())
        })
    }

    #[test]
    fn crlf_is_one_line() {
        let (lines, _) = read(b"ab\r\ncd\r\n", Echo::Plain);
        assert_eq!(lines, [b"ab".to_vec(), b"cd".to_vec()]);
    }

    #[test]
    fn empty_line_after_cr_lf() {
        let (lines, _) = read(b"a\r\n\r\n", Echo::Plain);
        assert_eq!(lines, [b"a".to_vec(), b"".to_vec()]);
    }

    #[test]
    fn backspace_and_escape_sequences() {
        let (lines, _) = read(b"abx\x7f\x1b[Dc\r", Echo::Plain);
        assert_eq!(lines, [b"abc".to_vec()]);
    }

    #[test]
    fn utf8_backspace_removes_whole_char() {
        let (lines, _) = read("aé\x08b\r".as_bytes(), Echo::Plain);
        assert_eq!(lines, [b"ab".to_vec()]);
    }

    #[test]
    fn masked_echo_never_contains_secret() {
        let (lines, out) = read("pässword\r".as_bytes(), Echo::Mask('*'));
        assert_eq!(lines, ["pässword".as_bytes().to_vec()]);
        assert!(!out.contains("ss"));
        assert_eq!(out.matches('*').count(), 8);
    }

    #[test]
    fn hidden_echo_prints_nothing_but_newline() {
        let (_, out) = read(b"secret\r", Echo::Hidden);
        assert_eq!(out, "\r\n");
    }

    #[test]
    fn length_limit_never_splits_a_character() {
        block_on(async {
            // "aé" is 3 bytes; with a limit of 2 the "é" must be dropped whole.
            let mut io = MockIo::new("aé\r".as_bytes());
            let mut buf = Vec::new();
            LineReader::default()
                .read_line(&mut io, &mut buf, 2, Echo::Plain)
                .await
                .unwrap();
            assert_eq!(buf, b"a");
        });
    }

    #[test]
    fn ctrl_c_cancels() {
        let (lines, _) = read(b"ab\x03", Echo::Plain);
        assert_eq!(lines, [b"<cancel>".to_vec()]);
    }
}
