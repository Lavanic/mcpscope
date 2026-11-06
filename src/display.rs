//! Turning recorded bytes into something safe to print.
//!
//! Recorded text is whatever a server sent. A server that emits escape
//! sequences, deliberately or by accident, must not get to repaint, retitle
//! or reorder the terminal of the person reading the recording. Everything
//! server-supplied goes through here first.

/// Control characters and the bidirectional overrides that let text claim to
/// say something other than what it says, rendered visibly instead of acted
/// upon. Newlines are escaped too: this is for single-line output.
pub fn sanitize_line(text: &str) -> String {
    sanitize(text, false)
}

/// Same, but newlines survive, for panes that show a message in full.
pub fn sanitize_block(text: &str) -> String {
    sanitize(text, true)
}

fn sanitize(text: &str, keep_newlines: bool) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\n' if keep_newlines => out.push(ch),
            // caret notation, the convention cat -v made familiar
            c if (c as u32) < 0x20 => {
                out.push('^');
                out.push(char::from_u32(c as u32 + 0x40).unwrap_or('?'));
            }
            '\u{7f}' => out.push_str("^?"),
            // bidi overrides can make a line render in an order the bytes do
            // not have, which is how a harmless looking name hides a payload
            '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' => {
                out.push_str(&format!("<U+{:04X}>", ch as u32));
            }
            c => out.push(c),
        }
    }
    out
}

/// Local time of day for a recorded millisecond timestamp, as sqlite sees
/// the machine's timezone. Doing this in the query keeps calendar maths and
/// a tzdata dependency out of the binary.
pub const LOCAL_HMS: &str = "strftime('%H:%M:%S', {}/1000.0, 'unixepoch', 'localtime')";

/// Same, with the date, for listings that span days.
pub const LOCAL_STAMP: &str = "datetime({}/1000.0, 'unixepoch', 'localtime')";

/// Substitute the column into one of the formats above.
pub fn local_time_sql(format: &str, column: &str) -> String {
    format.replace("{}", column)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_sequences_are_shown_not_obeyed() {
        let evil = "\x1b[31mRED\x1b]0;retitled\x07";
        let safe = sanitize_line(evil);
        assert!(!safe.contains('\x1b'), "no raw escape survives: {safe:?}");
        assert!(safe.starts_with("^["), "shown in caret notation: {safe}");
        assert!(safe.contains("RED"), "the text itself is kept: {safe}");
    }

    #[test]
    fn newlines_follow_the_mode() {
        assert_eq!(sanitize_line("a\nb"), "a^Jb");
        assert_eq!(sanitize_block("a\nb"), "a\nb");
        assert_eq!(sanitize_block("a\rb"), "a^Mb");
    }

    #[test]
    fn bidi_overrides_are_made_visible() {
        let safe = sanitize_line("file\u{202e}gnp.exe");
        assert!(safe.contains("<U+202E>"), "{safe}");
    }

    #[test]
    fn ordinary_text_is_untouched() {
        let text = "tools/call id=7 {\"q\": \"café ☕\"}";
        assert_eq!(sanitize_line(text), text);
    }
}
