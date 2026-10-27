//! Reading newline-delimited input with a hard cap on line length, so no
//! input can make the daemon allocate without bound.

use std::io::{self, BufRead};

/// The result of one capped line read.
#[derive(Debug, PartialEq, Eq)]
pub enum Line {
    /// End of input with nothing pending.
    Eof,
    /// A line ended by `\n` (the newline is not in the buffer).
    Complete,
    /// Input ended without a final `\n`; the buffer holds the partial line.
    Cut,
    /// The line was longer than the cap. It was read to its end (or to end
    /// of input) and discarded; the buffer is empty.
    TooLong,
}

/// Reads one line into `buf` (cleared first), keeping at most `cap` bytes.
/// An over-long line is consumed to its newline and reported as
/// [`Line::TooLong`].
pub fn read_capped<R: BufRead>(reader: &mut R, buf: &mut Vec<u8>, cap: usize) -> io::Result<Line> {
    buf.clear();
    let mut too_long = false;
    let mut any = false;
    loop {
        let available = match reader.fill_buf() {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if available.is_empty() {
            return Ok(if too_long {
                Line::TooLong
            } else if any {
                Line::Cut
            } else {
                Line::Eof
            });
        }
        any = true;
        let (chunk, found, used) = match available.iter().position(|&b| b == b'\n') {
            Some(i) => (&available[..i], true, i + 1),
            None => (available, false, available.len()),
        };
        if !too_long {
            if buf.len().saturating_add(chunk.len()) > cap {
                too_long = true;
                buf.clear();
            } else {
                buf.extend_from_slice(chunk);
            }
        }
        reader.consume(used);
        if found {
            return Ok(if too_long {
                Line::TooLong
            } else {
                Line::Complete
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufReader, Cursor};

    #[test]
    fn reads_lines_and_cut() {
        let mut r = BufReader::with_capacity(4, Cursor::new(b"ab\ncdefgh\nij".to_vec()));
        let mut buf = Vec::new();
        assert_eq!(read_capped(&mut r, &mut buf, 100).unwrap(), Line::Complete);
        assert_eq!(buf, b"ab");
        assert_eq!(read_capped(&mut r, &mut buf, 100).unwrap(), Line::Complete);
        assert_eq!(buf, b"cdefgh");
        assert_eq!(read_capped(&mut r, &mut buf, 100).unwrap(), Line::Cut);
        assert_eq!(buf, b"ij");
        assert_eq!(read_capped(&mut r, &mut buf, 100).unwrap(), Line::Eof);
    }

    #[test]
    fn over_long_lines_are_skipped() {
        let mut r = BufReader::with_capacity(3, Cursor::new(b"12345678\nok\n123456789".to_vec()));
        let mut buf = Vec::new();
        assert_eq!(read_capped(&mut r, &mut buf, 5).unwrap(), Line::TooLong);
        assert!(buf.is_empty());
        assert_eq!(read_capped(&mut r, &mut buf, 5).unwrap(), Line::Complete);
        assert_eq!(buf, b"ok");
        assert_eq!(read_capped(&mut r, &mut buf, 5).unwrap(), Line::TooLong);
        assert_eq!(read_capped(&mut r, &mut buf, 5).unwrap(), Line::Eof);
    }

    #[test]
    fn exact_cap_is_allowed() {
        let mut r = Cursor::new(b"12345\n".to_vec());
        let mut buf = Vec::new();
        assert_eq!(read_capped(&mut r, &mut buf, 5).unwrap(), Line::Complete);
    }
}
