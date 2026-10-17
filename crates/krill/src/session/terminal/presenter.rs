//! Strip terminal queries before presenting raw output. krilld alone answers them.
//! State spans OUTPUT frames. Escape/control strings have a bounded buffer.

#[derive(Default)]
pub(super) struct Presenter {
    pending: Vec<u8>,
    overflow: bool,
    escaped: bool,
}

impl Presenter {
    pub(super) fn push(&mut self, bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(bytes.len());
        for &byte in bytes {
            if self.pending.is_empty() {
                if byte == 0x1b {
                    self.pending.push(byte);
                } else {
                    out.push(byte);
                }
                continue;
            }
            if !self.overflow {
                self.pending.push(byte);
                if self.pending.len() > 4096 {
                    self.overflow = true;
                }
            }
            let prefix = self.pending.get(1).copied();
            let string = matches!(prefix, Some(b']' | b'P' | b'_' | b'^'));
            let done = if string {
                (byte == 7 && prefix == Some(b']')) || (self.escaped && byte == b'\\')
            } else if prefix == Some(b'[') {
                self.pending.len() > 2 && (0x40..=0x7e).contains(&byte)
            } else {
                (0x30..=0x7e).contains(&byte)
            };
            self.escaped = byte == 0x1b;
            if done {
                let query = match prefix {
                    Some(b'[') => {
                        matches!(byte, b'c' | b'n' | b't')
                            || (byte == b'u' && self.pending.get(2) == Some(&b'?'))
                            || (byte == b'q' && self.pending.get(2) == Some(&b'>'))
                            || (byte == b'p' && self.pending.contains(&b'$'))
                    }
                    Some(b']') => self.pending.windows(2).any(|v| v == b";?"),
                    Some(b'P' | b'_' | b'^') => true,
                    _ => false,
                };
                if !query && !self.overflow {
                    out.extend_from_slice(&self.pending);
                }
                self.pending.clear();
                self.overflow = false;
                self.escaped = false;
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn queries_are_removed_across_every_frame_boundary() {
        let bytes = b"a\x1b[c\x1b[?u\x1b[>0q\x1b[6n\x1b[?25$p\x1b[14t\x1b]10;?\x07\x1bP+q544e\x1b\\b\x1b[31mc\x1b[0m\x1b]8;;https://example.com\x1b\\d";
        let expected = b"ab\x1b[31mc\x1b[0m\x1b]8;;https://example.com\x1b\\d";
        for split in 0..=bytes.len() {
            let mut p = Presenter::default();
            let mut out = p.push(&bytes[..split]);
            out.extend(p.push(&bytes[split..]));
            assert_eq!(out, expected);
        }
    }

    proptest! {
        #[test]
        fn arbitrary_bytes_are_bounded(bytes in prop::collection::vec(any::<u8>(), 0..20000)) {
            let mut p = Presenter::default();
            for chunk in bytes.chunks(17) {
                p.push(chunk);
                prop_assert!(p.pending.len() <= 4097);
            }
        }
    }
}
