//! Incremental stdout splitter: the streaming form of
//! [`crate::trailer::split_stream`].

use crate::trailer::{START_TAG, Trailer};

/// A piece of the stream, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece {
    /// Bytes belonging to the op in flight (the separator `\n` before a
    /// trailer or a start line is not included).
    Output(Vec<u8>),
    Trailer(Trailer),
    /// An op's start line: `\n__tpS_<nonce> <id> <pid> <shell_pid>\n`.
    /// Never output.
    Started {
        id: usize,
        pid: i32,
        shell_pid: i32,
    },
}

/// Feed it chunks; it yields output, start lines and trailers. Only a line
/// shaped `\n__tp_<nonce> <id> <json>\n` for *this* nonce is a trailer, and
/// only `\n__tpS_<nonce> <id> <pid> <shell_pid>\n` is a start line.
pub struct Splitter {
    marker: Vec<u8>,
    start_marker: Vec<u8>,
    buf: Vec<u8>,
}

fn find(buf: &[u8], marker: &[u8]) -> Option<usize> {
    buf.windows(marker.len()).position(|w| w == marker)
}

fn parse_start(line: &[u8]) -> Option<(usize, i32, i32)> {
    let line = std::str::from_utf8(line).ok()?;
    let mut f = line.split(' ');
    f.next()?; // __tpS_<nonce>
    let id = f.next()?.parse().ok()?;
    let pid = f.next()?.parse().ok()?;
    let shell_pid = f.next()?.parse().ok()?;
    f.next().is_none().then_some((id, pid, shell_pid))
}

impl Splitter {
    pub fn new(nonce: &str) -> Splitter {
        Splitter {
            marker: format!("\n__tp_{nonce} ").into_bytes(),
            start_marker: format!("\n{START_TAG}{nonce} ").into_bytes(),
            buf: Vec::new(),
        }
    }

    pub fn push(&mut self, bytes: &[u8]) -> Vec<Piece> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        loop {
            let t = find(&self.buf, &self.marker);
            let s = find(&self.buf, &self.start_marker);
            // The earlier marker wins; true is a start line.
            let found = match (t, s) {
                (Some(t), Some(s)) if s < t => Some((s, true)),
                (Some(t), _) => Some((t, false)),
                (None, Some(s)) => Some((s, true)),
                (None, None) => None,
            };
            match found {
                Some((i, is_start)) => {
                    let line_start = i + 1;
                    let Some(len) = self.buf[line_start..].iter().position(|&b| b == b'\n') else {
                        // The line is not complete yet.
                        if i > 0 {
                            out.push(Piece::Output(self.buf.drain(..i).collect()));
                        }
                        break;
                    };
                    let line = &self.buf[line_start..line_start + len];
                    let piece = if is_start {
                        parse_start(line).map(|(id, pid, shell_pid)| Piece::Started {
                            id,
                            pid,
                            shell_pid,
                        })
                    } else {
                        std::str::from_utf8(line)
                            .ok()
                            .and_then(Trailer::parse_line)
                            .map(Piece::Trailer)
                    };
                    match piece {
                        Some(p) => {
                            if i > 0 {
                                out.push(Piece::Output(self.buf[..i].to_vec()));
                            }
                            out.push(p);
                            self.buf.drain(..line_start + len + 1);
                        }
                        None => {
                            // Right nonce, malformed: plain output.
                            out.push(Piece::Output(self.buf.drain(..=i).collect()));
                        }
                    }
                }
                None => {
                    // Hold back a tail that might become a marker.
                    let keep = self
                        .buf
                        .iter()
                        .rposition(|&b| b == b'\n')
                        .filter(|&j| {
                            self.marker.starts_with(&self.buf[j..])
                                || self.start_marker.starts_with(&self.buf[j..])
                        })
                        .unwrap_or(self.buf.len());
                    if keep > 0 {
                        out.push(Piece::Output(self.buf.drain(..keep).collect()));
                    }
                    break;
                }
            }
        }
        out
    }

    /// End of stream: whatever was held back is output.
    pub fn finish(&mut self) -> Option<Vec<u8>> {
        (!self.buf.is_empty()).then(|| std::mem::take(&mut self.buf))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(chunks: &[&str]) -> (Vec<u8>, Vec<usize>) {
        let mut s = Splitter::new("n");
        let mut o = vec![];
        let mut ids = vec![];
        for c in chunks {
            for p in s.push(c.as_bytes()) {
                match p {
                    Piece::Output(b) => o.extend(b),
                    Piece::Trailer(t) => ids.push(t.id),
                    Piece::Started { .. } => {}
                }
            }
        }
        o.extend(s.finish().unwrap_or_default());
        (o, ids)
    }

    #[test]
    fn chunking_does_not_matter() {
        let whole = "a\nb\n__tp_n 1 {}\nc\n__tp_x 2 {}\n\n__tp_n 2 {}\n";
        let (o, ids) = run(&[whole]);
        assert_eq!(o, b"a\nbc\n__tp_x 2 {}\n");
        assert_eq!(ids, vec![1, 2]);
        for cut in 1..whole.len() {
            let (a, b) = whole.split_at(cut);
            assert_eq!(run(&[a, b]), (o.clone(), ids.clone()), "cut {cut}");
        }
    }

    #[test]
    fn a_start_line_is_not_a_trailer_and_not_output() {
        let whole = "\n__tpS_n 3 41 7\nhi\n\n__tp_n 3 {}\n\n__tpS_x 3 1 2\n";
        let (o, ids) = run(&[whole]);
        assert_eq!(o, b"hi\n\n__tpS_x 3 1 2\n");
        assert_eq!(ids, vec![3]);
        let mut s = Splitter::new("n");
        let p = s.push(whole.as_bytes());
        assert_eq!(
            p[0],
            Piece::Started {
                id: 3,
                pid: 41,
                shell_pid: 7
            }
        );
        for cut in 1..whole.len() {
            let (a, b) = whole.split_at(cut);
            assert_eq!(run(&[a, b]), (o.clone(), ids.clone()), "cut {cut}");
        }
    }
}
