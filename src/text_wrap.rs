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
    for word in text.split_whitespace() {
        let candidate = if line.is_empty() {
            word.to_string()
        } else {
            format!("{line} {word}")
        };

        if text_width(&candidate, char_width) <= max_width {
            line = candidate;
            continue;
        }

        if !line.is_empty() {
            lines.push(line);
            line = String::new();
        }

        for ch in word.chars() {
            if text_width(&line, char_width) + char_width(ch) > max_width && !line.is_empty() {
                lines.push(line);
                line = String::new();
            }
            line.push(ch);
        }
    }

    if !line.is_empty() {
        lines.push(line);
    }
}

fn text_width<F>(text: &str, char_width: &F) -> usize
where
    F: Fn(char) -> usize,
{
    text.chars().map(char_width).sum()
}
