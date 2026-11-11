//! A browser key press as the bytes a terminal sends for it.

/// `key` is `KeyboardEvent.key`. `None` for keys that send nothing on
/// their own (Shift, F-keys, ...). `app_cursor`: the program asked for
/// application cursor keys, so arrows are `ESC O x` instead of `ESC [ x`.
pub fn encode(key: &str, ctrl: bool, alt: bool, app_cursor: bool) -> Option<Vec<u8>> {
    let arrow = |c: u8| vec![0x1b, if app_cursor { b'O' } else { b'[' }, c];
    let mut bytes = match key {
        "Enter" => vec![b'\r'],
        "Backspace" => vec![0x7f],
        "Tab" => vec![b'\t'],
        "Escape" => vec![0x1b],
        "ArrowUp" => arrow(b'A'),
        "ArrowDown" => arrow(b'B'),
        "ArrowRight" => arrow(b'C'),
        "ArrowLeft" => arrow(b'D'),
        "Home" => arrow(b'H'),
        "End" => arrow(b'F'),
        "Delete" => b"\x1b[3~".to_vec(),
        "PageUp" => b"\x1b[5~".to_vec(),
        "PageDown" => b"\x1b[6~".to_vec(),
        _ => {
            let mut chars = key.chars();
            let (Some(c), None) = (chars.next(), chars.next()) else {
                return None;
            };
            match c {
                // Ctrl with @ A-Z [ \ ] ^ _ (or a-z, or space) is a control code.
                ' ' if ctrl => vec![0],
                '@'..='_' | 'a'..='z' if ctrl => vec![c.to_ascii_uppercase() as u8 & 0x1f],
                _ => c.to_string().into_bytes(),
            }
        }
    };
    if alt {
        bytes.insert(0, 0x1b);
    }
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::encode;

    #[test]
    fn printable_keys_are_their_utf8() {
        assert_eq!(encode("a", false, false, false), Some(b"a".to_vec()));
        assert_eq!(encode("é", false, false, false), Some("é".as_bytes().to_vec()));
    }

    #[test]
    fn ctrl_makes_control_codes() {
        assert_eq!(encode("c", true, false, false), Some(vec![0x03]));
        assert_eq!(encode("[", true, false, false), Some(vec![0x1b]));
        assert_eq!(encode(" ", true, false, false), Some(vec![0]));
    }

    #[test]
    fn alt_prefixes_escape() {
        assert_eq!(encode("b", false, true, false), Some(b"\x1bb".to_vec()));
    }

    #[test]
    fn arrows_follow_the_cursor_key_mode() {
        assert_eq!(encode("ArrowUp", false, false, false), Some(b"\x1b[A".to_vec()));
        assert_eq!(encode("ArrowUp", false, false, true), Some(b"\x1bOA".to_vec()));
    }

    #[test]
    fn modifier_keys_alone_send_nothing() {
        assert_eq!(encode("Shift", false, false, false), None);
        assert_eq!(encode("F5", false, false, false), None);
    }
}
