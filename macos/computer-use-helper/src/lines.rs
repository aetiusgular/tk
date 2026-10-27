//! Newline-delimited JSON with a cap on each line, for the socket and for the
//! driver's MCP proxy.

use std::io::{self, BufRead};

/// One line, without its newline. `Ok(None)` at the end of the stream. A line
/// longer than `max` is read to its end and refused, so the stream stays in
/// step.
pub fn read_line_capped(reader: &mut dyn BufRead, max: usize) -> io::Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    let mut too_long = false;
    loop {
        let available = match reader.fill_buf() {
            Ok(buffer) => buffer,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        if available.is_empty() {
            if line.is_empty() && !too_long {
                return Ok(None);
            }
            break;
        }
        let (chunk, done) = match available.iter().position(|b| *b == b'\n') {
            Some(at) => (&available[..at], at + 1),
            None => (available, available.len()),
        };
        if !too_long {
            if line.len() + chunk.len() > max {
                too_long = true;
                line.clear();
            } else {
                line.extend_from_slice(chunk);
            }
        }
        let found = done <= available.len() && available.get(done - 1) == Some(&b'\n');
        reader.consume(done);
        if found {
            break;
        }
    }
    if too_long {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "line too long"));
    }
    if line.last() == Some(&b'\r') {
        line.pop();
    }
    Ok(Some(line))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn reads_lines_and_refuses_long_ones_in_step() {
        let mut input = Cursor::new(b"one\r\n0123456789\nthree".to_vec());
        assert_eq!(read_line_capped(&mut input, 5).unwrap().unwrap(), b"one");
        assert!(read_line_capped(&mut input, 5).is_err());
        assert_eq!(read_line_capped(&mut input, 5).unwrap().unwrap(), b"three");
        assert!(read_line_capped(&mut input, 5).unwrap().is_none());
    }
}
