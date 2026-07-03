//! Maps emoji characters to old-school ASCII emoticons.
//!
//! Uses `phf` for a compile-time perfect hash map — zero runtime init cost,
//! no allocation, lookups are a couple of comparisons.
//!
//! Add to Cargo.toml:
//! ```toml
//! phf = { version = "0.11", features = ["macros"] }
//! ```

use phf::phf_map;

static EMOJI_TO_ASCII: phf::Map<&'static str, &'static str> = phf_map! {
    "😀" => ":D",
    "😃" => ":D",
    "😄" => ":D",
    "😁" => ":D",
    "😆" => "XD",
    "😅" => "^^';",
    "🤣" => "XD",
    "😂" => "XD",
    "🙂" => ":)",
    "🙃" => "(:",
    "😉" => ";)",
    "😊" => "^^",
    "😇" => "O:)",
    "🥰" => "^^<3",
    "😍" => ":D<3",
    "🤩" => "*o*",
    "😘" => ":*",
    "😗" => ":*",
    "😚" => ":*",
    "😙" => ":*",
    "🥲" => ":')",
    "😋" => ":P",
    "😛" => ":P",
    "😜" => ";P",
    "🤪" => "XP",
    "😝" => "XP",
    "🤑" => "$_$",
    "🤗" => "\\o/",
    "🤔" => ":/",
    "🤐" => ":X",
    "🤫" => ":X",
    "🤨" => ">_>",
    "😐" => ":|",
    "😑" => "-_-",
    "😶" => ":x",
    "😏" => ":J",
    "😒" => "-_-",
    "🙄" => "9_9",
    "😬" => ":E",
    "😌" => "^^",
    "😔" => "3(",
    "😪" => "-_-zzz",
    "🤤" => ":P~",
    "😴" => "-_-zzz",
    "😷" => ":x",
    "🤒" => ":(",
    "🤕" => ":(",
    "🤢" => ":S",
    "🤮" => ":P~~~",
    "🤧" => ":'(",
    "🥵" => ">_<",
    "🥶" => ">_<",
    "🥴" => "%)",
    "😵" => "x_x",
    "🤯" => ":o",
    "🥳" => ":D",
    "🥸" => "B|",
    "😎" => "B)",
    "😕" => ":/",
    "😟" => ":(",
    "🙁" => ":(",
    "☹" => ":(",
    "😮" => ":o",
    "😯" => ":o",
    "😲" => ":O",
    "😦" => "D:",
    "😧" => "D:",
    "😨" => "D:",
    "😰" => "D:,",
    "😥" => ",:(",
    "😢" => ":'(",
    "😭" => "D;",
    "😱" => ":O",
    "😖" => ">_<",
    "😣" => ">_<",
    "😞" => ":(",
    "😓" => ",:(",
    "😩" => "X(",
    "😫" => "X(",
    "😤" => ">:(",
    "😡" => ">:(",
    "😠" => ">:(",
    "🤬" => ">:(",
    "😈" => ">:)",
    "👿" => ">:(",
    "💀" => "x_x",
    "☠" => "x_x",
    "💩" => ":P",
    "🤡" => ":o)",
    "👹" => ">:(",
    "👺" => ">:(",
    "👻" => "(boo)",
    "🤖" => "[o_o]",
    "😺" => ":3",
    "😸" => ":3",
    "😹" => ":')",
    "😻" => "<3",
    "😼" => ">:3",
    "😽" => ":*",
    "🙀" => "D:",
    "😿" => ":'(",
    "😾" => ">:(",
    "❤" => "<3",
    "🧡" => "<3",
    "💛" => "<3",
    "💚" => "<3",
    "💙" => "<3",
    "💜" => "<3",
    "🖤" => "<3",
    "🤍" => "<3",
    "🤎" => "<3",
    "💔" => "</3",
    "💕" => "<3<3",
    "💞" => "<3<3",
    "💓" => "<3",
    "💗" => "<3",
    "💖" => "<3",
    "💘" => "<3",
    "💝" => "<3",
    "💟" => "<3",
    "💋" => ":*",
    "👋" => "o/",
    "🙌" => "\\o/",
    "👍" => "(y)",
    "👎" => "(n)",
    "🤘" => "\\m/",
    "🫡" => "o7",
    "🐟" => "<><",
    "🐠" => "<><",
    "🐡" => "<><",
    "🐱" => "=^.^=",
    "🐈" => "=^.^=",
    "🌹" => "@}--",
    "🍻" => "\\o/",
    "🎉" => "\\o/",
    "🎊" => "\\o/",
    "⭐" => "*",
    "🌟" => "*",
    "🌞" => ":sun_with_face:",
    "✨" => "*~*",
    "💤" => "zzz",
    "🎅" => "*<|:)",
    "👼" => "O:)",
    "🍆" => "8==D",
};

/// Look up the ASCII emoticon for a single emoji grapheme.
///
/// Returns `None` if the emoji isn't in the table. Input should be the
/// emoji as a `&str` (one grapheme cluster); skin-tone and ZWJ-joined
/// sequences will miss unless you've stripped modifiers first.
#[cfg(test)]
fn emoji_to_ascii(emoji: &str) -> Option<&'static str> {
    EMOJI_TO_ASCII.get(emoji).copied()
}

/// Replace every known emoji in `s` with its ASCII equivalent.
///
/// Walks `char` boundaries (see `split_graphemes` below), so ZWJ-joined
/// sequences won't merge into a single lookup key. When a char is replaced,
/// any variation selector (U+FE0E/U+FE0F) immediately following it is
/// dropped too, since it's just a presentation hint for the emoji we already
/// consumed. Unknown emoji pass through unchanged.
pub fn replace_emoji(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut graphemes = split_graphemes(s).peekable();
    while let Some(g) = graphemes.next() {
        match EMOJI_TO_ASCII.get(g) {
            Some(&ascii) => {
                out.push_str(ascii);
                while matches!(graphemes.peek(), Some(&"\u{FE0E}") | Some(&"\u{FE0F}")) {
                    graphemes.next();
                }
            }
            None => out.push_str(g),
        }
    }
    out
}

// Minimal grapheme splitter: each `char` is one segment. This handles
// the BMP and astral-plane emoji in the table above, but won't merge
// ZWJ sequences (e.g. 👨‍👩‍👧) into a single lookup key. If you need
// that, depend on `unicode-segmentation` and use `s.graphemes(true)`.
fn split_graphemes(s: &str) -> impl Iterator<Item = &str> {
    s.char_indices().map(move |(i, c)| &s[i..i + c.len_utf8()])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_lookups() {
        assert_eq!(emoji_to_ascii("🙂"), Some(":)"));
        assert_eq!(emoji_to_ascii("😆"), Some("XD"));
        assert_eq!(emoji_to_ascii("❤"), Some("<3"));
        assert_eq!(emoji_to_ascii("💔"), Some("</3"));
        assert_eq!(emoji_to_ascii("foo"), None);
    }

    #[test]
    fn replace_inline() {
        assert_eq!(replace_emoji("hi 🙂"), "hi :)");
        assert_eq!(replace_emoji("😭😭😭"), "D;D;D;");
        assert_eq!(replace_emoji("hi 🌞"), "hi :sun_with_face:");
        assert_eq!(replace_emoji("plain text"), "plain text");
    }

    #[test]
    fn replace_drops_variation_selector() {
        // "❤️" is U+2764 (mapped) followed by U+FE0F (variation selector);
        // the selector must not leak into the output.
        assert_eq!(replace_emoji("❤\u{FE0F}"), "<3");
    }

    #[test]
    fn all_values_are_ascii() {
        for (_, v) in EMOJI_TO_ASCII.entries() {
            assert!(v.is_ascii(), "non-ASCII value: {:?}", v);
        }
    }
}
