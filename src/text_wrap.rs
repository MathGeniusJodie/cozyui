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
    for word in text.split_whitespace() {
        let word_width = text_width(word, char_width);
        let gap_width = if line.is_empty() { 0 } else { char_width(' ') };

        if line_width + gap_width + word_width <= max_width {
            if !line.is_empty() {
                line.push(' ');
                line_width += gap_width;
            }
            line.push_str(word);
            line_width += word_width;
            continue;
        }

        if !line.is_empty() {
            lines.push(line);
            line = String::new();
            line_width = 0;
        }

        for ch in word.chars() {
            let ch_width = char_width(ch);
            if line_width + ch_width > max_width && !line.is_empty() {
                lines.push(line);
                line = String::new();
                line_width = 0;
            }
            line.push(ch);
            line_width += ch_width;
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
