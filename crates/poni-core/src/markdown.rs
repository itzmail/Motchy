//! Markdown styling for the Notes editor: which stretches of a note's text
//! are a heading, a bullet, bold and so on. The text itself stays as typed;
//! the editor only draws these stretches differently. Pure, so the rules are
//! tested here and the Mac only applies them.
//!
//! Offsets and lengths count UTF-16 units, which is what the Mac's text
//! system measures in.

/// How a stretch of text is drawn. Later spans are applied over earlier ones
/// (a `Marker` over the line's `Heading`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    /// A whole heading line: 1 (largest) to 6.
    Heading(u8),
    /// Syntax that is only there to be typed (`####`, `**`, `` ` ``, `>`): dimmed.
    Marker,
    /// A list item's whole line: its text hangs in from the bullet.
    Item,
    /// The bullet itself (`-`, `*`, `+`).
    Bullet,
    /// A numbered item's number and its dot.
    Number,
    Bold,
    Italic,
    /// Code in backticks.
    Code,
    /// A quote's whole line.
    Quote,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub len: usize,
    pub style: Style,
}

/// The styled stretches of `text`, line by line, in the order to apply them.
pub fn spans(text: &str) -> Vec<Span> {
    let mut out = Vec::new();
    let mut at = 0;
    for line in text.split('\n') {
        let len16 = line.encode_utf16().count();
        line_spans(line, at, len16, &mut out);
        at += len16 + 1;
    }
    out
}

fn line_spans(line: &str, base: usize, len16: usize, out: &mut Vec<Span>) {
    let chars: Vec<char> = line.chars().collect();
    // UTF-16 offset of each char, and one past the end.
    let mut offs = Vec::with_capacity(chars.len() + 1);
    let mut n = 0;
    for c in &chars {
        offs.push(n);
        n += c.len_utf16();
    }
    offs.push(n);
    let put = |out: &mut Vec<Span>, from: usize, to: usize, style| {
        if to > from {
            out.push(Span { start: base + offs[from], len: offs[to] - offs[from], style });
        }
    };

    // The line's own kind, and where its text starts.
    let hashes = chars.iter().take_while(|c| **c == '#').count();
    let indent = chars.iter().take_while(|c| **c == ' ').count();
    let rest = &chars[indent.min(chars.len())..];
    let digits = rest.iter().take_while(|c| c.is_ascii_digit()).count();
    let text_from = if (1..=6).contains(&hashes) && chars.get(hashes) == Some(&' ') {
        out.push(Span { start: base, len: len16, style: Style::Heading(hashes as u8) });
        put(out, 0, hashes + 1, Style::Marker);
        hashes + 1
    } else if matches!(rest.first(), Some('-' | '*' | '+')) && rest.get(1) == Some(&' ') {
        out.push(Span { start: base, len: len16, style: Style::Item });
        put(out, indent, indent + 1, Style::Bullet);
        indent + 2
    } else if (1..=9).contains(&digits) && matches!(rest.get(digits), Some('.' | ')')) && rest.get(digits + 1) == Some(&' ') {
        out.push(Span { start: base, len: len16, style: Style::Item });
        put(out, indent, indent + digits + 1, Style::Number);
        indent + digits + 2
    } else if chars.first() == Some(&'>') {
        out.push(Span { start: base, len: len16, style: Style::Quote });
        let marker = if chars.get(1) == Some(&' ') { 2 } else { 1 };
        put(out, 0, marker, Style::Marker);
        marker
    } else {
        0
    };
    inline(&chars, text_from, &put, out);
}

/// How long the start of `line` is up to where its text begins, when the
/// editor hides the typed mark there: a heading's `#### `, a quote's `> `,
/// a bullet's `- ` with its indent (UTF-16 units; 0 for other lines). A
/// number is shown as typed, so a numbered line has none.
pub fn hidden_prefix(line: &str) -> usize {
    match spans(line).as_slice() {
        [Span { style: Style::Heading(_) | Style::Quote, .. }, Span { style: Style::Marker, start: 0, len }, ..] => *len,
        [Span { style: Style::Item, .. }, Span { style: Style::Bullet, start, len }, ..] => start + len + 1,
        _ => 0,
    }
}

/// How long the mark is that Backspace at the start of the line's text takes
/// off whole, so the line goes back to plain text: a heading's or quote's
/// mark, a bullet or a number with its indent (0 for other lines).
pub fn line_prefix(line: &str) -> usize {
    match hidden_prefix(line) {
        0 => match spans(line).as_slice() {
            [Span { style: Style::Item, .. }, Span { style: Style::Bullet | Style::Number, start, len }, ..] => start + len + 1,
            _ => 0,
        },
        n => n,
    }
}

/// What Enter does at the end of a list line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Enter {
    /// Not a list line: a plain new line.
    Plain,
    /// A new line that starts with this (the next bullet or number).
    Next(String),
    /// The item is empty: the list ends, and the line is emptied.
    End,
}

/// What Enter does at the end of `line`.
pub fn on_enter(line: &str) -> Enter {
    let indent: String = line.chars().take_while(|c| *c == ' ').collect();
    let rest = &line[indent.len()..];
    let mut it = rest.chars();
    let (prefix, next) = match it.next() {
        Some(m @ ('-' | '*' | '+')) if it.next() == Some(' ') => (format!("{m} "), format!("{indent}{m} ")),
        Some(d) if d.is_ascii_digit() => {
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            let after = &rest[digits.len()..];
            let n: u64 = match digits.parse() {
                Ok(n) if digits.len() <= 9 => n,
                _ => return Enter::Plain,
            };
            match (after.chars().next(), after.chars().nth(1)) {
                (Some(d @ ('.' | ')')), Some(' ')) => (format!("{digits}{d} "), format!("{indent}{}{d} ", n + 1)),
                _ => return Enter::Plain,
            }
        }
        _ => return Enter::Plain,
    };
    if rest[prefix.len()..].trim().is_empty() {
        Enter::End
    } else {
        Enter::Next(next)
    }
}

/// Code, bold and italic inside a line, from `from` on.
/// Adds the span of chars `from..to` (the line's own offsets) to the list.
type Put<'a> = dyn Fn(&mut Vec<Span>, usize, usize, Style) + 'a;

fn inline(chars: &[char], from: usize, put: &Put, out: &mut Vec<Span>) {
    let n = chars.len();
    // Characters already part of a marker or a code stretch: not read again as syntax.
    let mut taken = vec![false; n];

    // `code`
    let mut i = from;
    while i < n {
        if chars[i] == '`' {
            if let Some(j) = (i + 2..n).find(|j| chars[*j] == '`') {
                put(out, i, j + 1, Style::Code);
                put(out, i, i + 1, Style::Marker);
                put(out, j, j + 1, Style::Marker);
                taken[i..=j].iter_mut().for_each(|t| *t = true);
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }

    // **bold**
    let mut i = from;
    while i + 1 < n {
        if chars[i] == '*' && chars[i + 1] == '*' && !taken[i] && !taken[i + 1] {
            let close = (i + 3..n.saturating_sub(1)).find(|j| chars[*j] == '*' && chars[*j + 1] == '*' && !taken[*j] && !taken[*j + 1]);
            if let Some(j) = close.filter(|j| !chars[i + 2].is_whitespace() && !chars[j - 1].is_whitespace()) {
                put(out, i, j + 2, Style::Bold);
                put(out, i, i + 2, Style::Marker);
                put(out, j, j + 2, Style::Marker);
                taken[i] = true;
                taken[i + 1] = true;
                taken[j] = true;
                taken[j + 1] = true;
                i = j + 2;
                continue;
            }
        }
        i += 1;
    }

    // *italic* and _italic_ (an underscore inside a word, as in snake_case, is plain)
    let mut i = from;
    while i < n {
        let c = chars[i];
        let opens = (c == '*' || c == '_')
            && !taken[i]
            && i + 1 < n
            && !chars[i + 1].is_whitespace()
            && chars[i + 1] != c
            && (i == from || (chars[i - 1] != c && (c == '*' || !chars[i - 1].is_alphanumeric())));
        if opens {
            let close = (i + 2..n).find(|j| {
                chars[*j] == c && !taken[*j] && !chars[*j - 1].is_whitespace() && chars.get(*j + 1) != Some(&c) && (c == '*' || !chars.get(*j + 1).is_some_and(|x| x.is_alphanumeric()))
            });
            if let Some(j) = close {
                put(out, i, j + 1, Style::Italic);
                put(out, i, i + 1, Style::Marker);
                put(out, j, j + 1, Style::Marker);
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sp(start: usize, len: usize, style: Style) -> Span {
        Span { start, len, style }
    }

    #[test]
    fn headings_have_a_dim_marker() {
        let s = spans("#### Title");
        assert_eq!(s, [sp(0, 10, Style::Heading(4)), sp(0, 5, Style::Marker)]);
        // Needs the space, and no more than six hashes.
        assert!(spans("####Title").is_empty());
        assert!(spans("####### seven").is_empty());
        assert_eq!(spans("# a")[0].style, Style::Heading(1));
    }

    #[test]
    fn bullets_and_numbers() {
        assert_eq!(spans("- point"), [sp(0, 7, Style::Item), sp(0, 1, Style::Bullet)]);
        assert_eq!(spans("  * nested"), [sp(0, 10, Style::Item), sp(2, 1, Style::Bullet)]);
        assert_eq!(spans("12. step"), [sp(0, 8, Style::Item), sp(0, 3, Style::Number)]);
        assert_eq!(spans("2) step")[1], sp(0, 2, Style::Number));
        // A dash without a space is just text; so is a number without its dot.
        assert!(spans("-point").is_empty());
        assert!(spans("12 apples").is_empty());
    }

    #[test]
    fn quotes() {
        assert_eq!(spans("> said"), [sp(0, 6, Style::Quote), sp(0, 2, Style::Marker)]);
    }

    #[test]
    fn inline_styles() {
        let s = spans("a **bold** b");
        assert!(s.contains(&sp(2, 8, Style::Bold)));
        assert!(s.contains(&sp(2, 2, Style::Marker)) && s.contains(&sp(8, 2, Style::Marker)));
        assert!(spans("an *it* x").contains(&sp(3, 4, Style::Italic)));
        assert!(spans("an _it_ x").contains(&sp(3, 4, Style::Italic)));
        let s = spans("use `cargo test` now");
        assert!(s.contains(&sp(4, 12, Style::Code)));
    }

    #[test]
    fn syntax_that_is_not_closed_or_is_inside_a_word_stays_plain() {
        assert!(spans("a **bold").is_empty());
        assert!(spans("a * b * c").is_empty());
        assert!(spans("snake_case_name").is_empty());
        assert!(spans("2 * 3 * 4").is_empty());
        assert!(spans("** **").is_empty());
    }

    #[test]
    fn code_hides_its_content_from_other_syntax() {
        let s = spans("`**x**`");
        assert!(!s.iter().any(|x| x.style == Style::Bold));
        assert!(s.contains(&sp(0, 7, Style::Code)));
    }

    #[test]
    fn a_star_bullet_is_not_italic() {
        let s = spans("* *it*");
        assert!(s.contains(&sp(0, 1, Style::Bullet)));
        assert!(s.contains(&sp(2, 4, Style::Italic)));
    }

    #[test]
    fn line_marks_have_a_length() {
        assert_eq!((hidden_prefix("#### Title"), line_prefix("#### Title")), (5, 5));
        assert_eq!((hidden_prefix("> said"), line_prefix("> said")), (2, 2));
        // A bullet is hidden (the editor draws a dot); a number stays on screen.
        // Backspace takes either off.
        assert_eq!((hidden_prefix("- milk"), line_prefix("- milk")), (2, 2));
        assert_eq!(hidden_prefix("  * nested"), 4);
        assert_eq!((hidden_prefix("12. step"), line_prefix("  12. step")), (0, 6));
        assert_eq!((hidden_prefix("plain"), line_prefix("plain")), (0, 0));
        assert_eq!(line_prefix("# "), 2);
    }

    #[test]
    fn enter_continues_a_list_and_ends_it_when_the_item_is_empty() {
        assert_eq!(on_enter("- milk"), Enter::Next("- ".into()));
        assert_eq!(on_enter("  * milk"), Enter::Next("  * ".into()));
        assert_eq!(on_enter("9. milk"), Enter::Next("10. ".into()));
        assert_eq!(on_enter("2) milk"), Enter::Next("3) ".into()));
        assert_eq!(on_enter("- "), Enter::End);
        assert_eq!(on_enter("  3. "), Enter::End);
        assert_eq!(on_enter("plain"), Enter::Plain);
        assert_eq!(on_enter("-dash"), Enter::Plain);
        assert_eq!(on_enter("12 apples"), Enter::Plain);
        assert_eq!(on_enter(""), Enter::Plain);
    }

    #[test]
    fn offsets_follow_lines_and_count_utf16() {
        // "😀" is two UTF-16 units; the second line starts after the newline.
        let s = spans("😀 a\n- **b**");
        let line2 = "😀 a\n".encode_utf16().count();
        assert_eq!(line2, 5);
        assert!(s.contains(&sp(line2, 7, Style::Item)));
        assert!(s.contains(&sp(line2 + 2, 5, Style::Bold)));
        assert_eq!(spans("")[..], []);
    }
}
