pub fn wrap_lines<F>(text: &str, max_width: usize, char_width: F) -> Vec<String>
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

fn wrap_paragraph<F>(text: &str, max_width: usize, char_width: &F, lines: &mut Vec<String>)
where
    F: Fn(char) -> usize,
{
    let mut line = String::new();
    let mut line_width = 0;
    let mut last_word_boundary = None;
    for ch in text.chars() {
        let ch_width = char_width(ch);
        line.push(ch);
        line_width += ch_width;
        if ch.is_whitespace() {
            last_word_boundary = Some((line.len(), line_width));
        }

        if line_width > max_width && !line.is_empty() {
            if let Some((break_at, break_width)) = last_word_boundary
                && break_at < line.len()
            {
                let next = line[break_at..].to_string();
                lines.push(line[..break_at].to_string());
                line = next;
                line_width -= break_width;
                last_word_boundary = last_boundary(&line, char_width);
            } else if let Some(split_at) = line.len().checked_sub(ch.len_utf8())
                && split_at > 0
            {
                lines.push(line[..split_at].to_string());
                line = ch.to_string();
                line_width = ch_width;
                last_word_boundary = ch.is_whitespace().then_some((line.len(), line_width));
            }
        }
    }

    if !line.is_empty() {
        lines.push(line);
    }
}

fn last_boundary<F>(text: &str, char_width: &F) -> Option<(usize, usize)>
where
    F: Fn(char) -> usize,
{
    let mut width = 0;
    let mut boundary = None;
    for (index, ch) in text.char_indices() {
        width += char_width(ch);
        if ch.is_whitespace() {
            boundary = Some((index + ch.len_utf8(), width));
        }
    }
    boundary
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
    fn wrap_lines_prefers_word_boundaries() {
        assert_eq!(
            wrap_lines("hello world", 8, |_| 1),
            vec!["hello ".to_string(), "world".to_string()]
        );
    }

    #[test]
    fn wrap_lines_splits_words_that_are_too_wide() {
        assert_eq!(
            wrap_lines("abcdefgh", 3, |_| 1),
            vec!["abc".to_string(), "def".to_string(), "gh".to_string()]
        );
    }
}
