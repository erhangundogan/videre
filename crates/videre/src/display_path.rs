//! Paths printed to a terminal.

/// A path as the user should read it: every printable character as-is,
/// including combining marks, so a decomposed Turkish name stays readable,
/// but control characters and invisible direction or width characters
/// escaped, so a filename can neither drive the terminal nor display
/// reordered (a right-to-left override can make `gnp.jpg` read as `jpg.png`).
pub(crate) fn escape_controls(path: &str) -> String {
    let mut escaped = String::with_capacity(path.len());
    for ch in path.chars() {
        if ch.is_control() || is_invisible_format(ch) {
            escaped.extend(ch.escape_default());
        } else {
            escaped.push(ch);
        }
    }
    escaped
}

/// Bidirectional overrides, isolates and marks, and zero-width characters.
fn is_invisible_format(ch: char) -> bool {
    matches!(
        ch,
        '\u{061c}' | '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2069}' | '\u{feff}'
    )
}
