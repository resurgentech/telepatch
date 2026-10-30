//! Incremental stdout splitter: the streaming form of
//! [`crate::trailer::split_stream`].

use crate::trailer::Trailer;

/// A piece of the stream, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece {
    /// Bytes belonging to the op in flight (the separator `\n` before a
    /// trailer is not included).
    Output(Vec<u8>),
    Trailer(Trailer),
}

/// Feed it chunks; it yields output and trailers. Only a line shaped
/// `\n__tp_<nonce> <id> <json>\n` for *this* nonce is a trailer.
pub struct Splitter {
    marker: Vec<u8>,
    buf: Vec<u8>,
}

impl Splitter {
    pub fn new(nonce: &str) -> Splitter {
        Splitter {
            marker: format!("\n__tp_{nonce} ").into_bytes(),
            buf: Vec::new(),
        }
    }

    pub fn push(&mut self, bytes: &[u8]) -> Vec<Piece> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        loop {
            let found = self
                .buf
                .windows(self.marker.len())
                .position(|w| w == self.marker.as_slice());
            match found {
                Some(i) => {
                    let line_start = i + 1;
                    let Some(len) = self.buf[line_start..].iter().position(|&b| b == b'\n') else {
                        // Trailer line not complete yet.
                        if i > 0 {
                            out.push(Piece::Output(self.buf.drain(..i).collect()));
                        }
                        break;
                    };
                    let line = &self.buf[line_start..line_start + len];
                    let parsed = std::str::from_utf8(line).ok().and_then(Trailer::parse_line);
                    match parsed {
                        Some(t) => {
                            if i > 0 {
                                out.push(Piece::Output(self.buf[..i].to_vec()));
                            }
                            out.push(Piece::Trailer(t));
                            self.buf.drain(..line_start + len + 1);
                        }
                        None => {
                            // Right nonce, malformed: plain output.
                            out.push(Piece::Output(self.buf.drain(..=i).collect()));
                        }
                    }
                }
                None => {
                    // Hold back a tail that might become the marker.
                    let keep = self
                        .buf
                        .iter()
                        .rposition(|&b| b == b'\n')
                        .filter(|&j| self.marker.starts_with(&self.buf[j..]))
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
}
