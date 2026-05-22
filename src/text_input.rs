pub(crate) enum EditKey {
    Insert(char),
    Backspace,
    Enter,
    Escape,
    Tab,
    Left,
    Right,
    None,
}

pub(crate) fn edit_key(keycode: u8, state: u16) -> EditKey {
    match keycode {
        9 => EditKey::Escape,
        22 => EditKey::Backspace,
        23 => EditKey::Tab,
        36 => EditKey::Enter,
        113 => EditKey::Left,
        114 => EditKey::Right,
        _ => printable_key(keycode, state)
            .map(EditKey::Insert)
            .unwrap_or(EditKey::None),
    }
}

pub(crate) fn printable_key(keycode: u8, state: u16) -> Option<char> {
    let shift = state & 1 != 0;
    match (keycode, shift) {
        (65, _) => Some(' '),
        (10, false) => Some('1'),
        (10, true) => Some('!'),
        (11, false) => Some('2'),
        (11, true) => Some('@'),
        (12, false) => Some('3'),
        (12, true) => Some('#'),
        (13, false) => Some('4'),
        (13, true) => Some('$'),
        (14, false) => Some('5'),
        (14, true) => Some('%'),
        (15, false) => Some('6'),
        (15, true) => Some('^'),
        (16, false) => Some('7'),
        (16, true) => Some('&'),
        (17, false) => Some('8'),
        (17, true) => Some('*'),
        (18, false) => Some('9'),
        (18, true) => Some('('),
        (19, false) => Some('0'),
        (19, true) => Some(')'),
        (20, false) => Some('-'),
        (20, true) => Some('_'),
        (21, false) => Some('='),
        (21, true) => Some('+'),
        (24, false) => Some('q'),
        (24, true) => Some('Q'),
        (25, false) => Some('w'),
        (25, true) => Some('W'),
        (26, false) => Some('e'),
        (26, true) => Some('E'),
        (27, false) => Some('r'),
        (27, true) => Some('R'),
        (28, false) => Some('t'),
        (28, true) => Some('T'),
        (29, false) => Some('y'),
        (29, true) => Some('Y'),
        (30, false) => Some('u'),
        (30, true) => Some('U'),
        (31, false) => Some('i'),
        (31, true) => Some('I'),
        (32, false) => Some('o'),
        (32, true) => Some('O'),
        (33, false) => Some('p'),
        (33, true) => Some('P'),
        (34, false) => Some('['),
        (34, true) => Some('{'),
        (35, false) => Some(']'),
        (35, true) => Some('}'),
        (38, false) => Some('a'),
        (38, true) => Some('A'),
        (39, false) => Some('s'),
        (39, true) => Some('S'),
        (40, false) => Some('d'),
        (40, true) => Some('D'),
        (41, false) => Some('f'),
        (41, true) => Some('F'),
        (42, false) => Some('g'),
        (42, true) => Some('G'),
        (43, false) => Some('h'),
        (43, true) => Some('H'),
        (44, false) => Some('j'),
        (44, true) => Some('J'),
        (45, false) => Some('k'),
        (45, true) => Some('K'),
        (46, false) => Some('l'),
        (46, true) => Some('L'),
        (47, false) => Some(';'),
        (47, true) => Some(':'),
        (48, false) => Some('\''),
        (48, true) => Some('"'),
        (49, false) => Some('`'),
        (49, true) => Some('~'),
        (51, false) => Some('\\'),
        (51, true) => Some('|'),
        (52, false) => Some('z'),
        (52, true) => Some('Z'),
        (53, false) => Some('x'),
        (53, true) => Some('X'),
        (54, false) => Some('c'),
        (54, true) => Some('C'),
        (55, false) => Some('v'),
        (55, true) => Some('V'),
        (56, false) => Some('b'),
        (56, true) => Some('B'),
        (57, false) => Some('n'),
        (57, true) => Some('N'),
        (58, false) => Some('m'),
        (58, true) => Some('M'),
        (59, false) => Some(','),
        (59, true) => Some('<'),
        (60, false) => Some('.'),
        (60, true) => Some('>'),
        (61, false) => Some('/'),
        (61, true) => Some('?'),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn printable_key_is_shared_us_x11_mapping() {
        assert_eq!(printable_key(24, 0), Some('q'));
        assert_eq!(printable_key(24, 1), Some('Q'));
        assert_eq!(printable_key(10, 1), Some('!'));
    }

    #[test]
    fn edit_key_keeps_controls_separate() {
        assert!(matches!(edit_key(22, 0), EditKey::Backspace));
        assert!(matches!(edit_key(24, 0), EditKey::Insert('q')));
    }
}
