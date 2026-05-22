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
/// Walks grapheme clusters when the `unicode-segmentation` feature is
/// available; falls back to `char` boundaries otherwise. Unknown emoji
/// pass through unchanged.
pub fn replace_emoji(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for g in split_graphemes(s) {
        match EMOJI_TO_ASCII.get(g) {
            Some(&ascii) => out.push_str(ascii),
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
        assert_eq!(replace_emoji("plain text"), "plain text");
    }

    #[test]
    fn all_values_are_ascii() {
        for (_, v) in EMOJI_TO_ASCII.entries() {
            assert!(v.is_ascii(), "non-ASCII value: {:?}", v);
        }
    }
}
