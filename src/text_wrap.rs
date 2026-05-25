pub(crate) fn wrap_lines<F>(text: &str, max_width: usize, char_width: F) -> Vec<String>
where
    F: Fn(char) -> usize,
{
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        wrap_paragraph(paragraph, max_width, &char_width, &mut lines);
    }

    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

pub(crate) fn fits_in_lines<F>(
    text: &str,
    max_width: usize,
    max_lines: usize,
    char_width: F,
) -> bool
where
    F: Fn(char) -> usize,
{
    wrap_lines(text, max_width, char_width).len() <= max_lines
}

fn wrap_paragraph<F>(text: &str, max_width: usize, char_width: &F, lines: &mut Vec<String>)
where
    F: Fn(char) -> usize,
{
    let mut line = String::new();
    let mut line_width = 0;
    for ch in text.chars() {
        let ch_width = char_width(ch);
        if line_width + ch_width > max_width && !line.is_empty() {
            lines.push(line);
            line = String::new();
            line_width = 0;
        }
        line.push(ch);
        line_width += ch_width;
    }

    if !line.is_empty() {
        lines.push(line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_lines_preserves_trailing_spaces() {
        assert_eq!(wrap_lines("a ", 10, |_| 1), vec!["a ".to_string()]);
    }

    #[test]
    fn wrap_lines_preserves_repeated_spaces() {
        assert_eq!(wrap_lines("a  b", 10, |_| 1), vec!["a  b".to_string()]);
    }

    #[test]
    fn fits_in_lines_counts_space_width() {
        assert!(!fits_in_lines("abc ", 3, 1, |_| 1));
    }
}
