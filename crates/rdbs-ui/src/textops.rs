//! Editing commands of the SQL editor as pure functions of (text, selection), in character
//! indices like egui's cursors, so they can be tested without a UI.

use std::ops::Range;

/// Columns a tab advances, for Tabify / Untabify and Decrease Line Indent.
pub const TAB_WIDTH: usize = 4;

/// A selection in character indices; `anchor == caret` is a plain caret.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    /// Fixed end.
    pub anchor: usize,
    /// End that moves with the cursor.
    pub caret: usize,
}

impl Selection {
    /// A caret without selection.
    pub fn caret(at: usize) -> Self {
        Self {
            anchor: at,
            caret: at,
        }
    }

    /// Selection from `anchor` to `caret`.
    pub fn new(anchor: usize, caret: usize) -> Self {
        Self { anchor, caret }
    }

    /// The selected characters, start first.
    pub fn range(self) -> Range<usize> {
        self.anchor.min(self.caret)..self.anchor.max(self.caret)
    }

    /// Whether nothing is selected.
    pub fn is_empty(self) -> bool {
        self.anchor == self.caret
    }
}

/// New text and selection after a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    /// Whole editor text.
    pub text: String,
    /// Selection to set.
    pub selection: Selection,
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn join(parts: &[&[char]]) -> String {
    parts.iter().flat_map(|p| p.iter()).collect()
}

/// Character range `start..end` of the lines a selection touches, without the final line
/// break. A selection that ends at the start of a line does not include that line.
fn line_block(chars: &[char], sel: Selection) -> Range<usize> {
    let r = sel.range();
    let mut last = r.end.min(chars.len());
    if !sel.is_empty() && last > r.start && chars[last - 1] == '\n' {
        last -= 1;
    }
    let start = chars[..r.start.min(chars.len())]
        .iter()
        .rposition(|&c| c == '\n')
        .map_or(0, |i| i + 1);
    let end = chars[last..]
        .iter()
        .position(|&c| c == '\n')
        .map_or(chars.len(), |i| last + i);
    start..end
}

/// Replaces the lines of `block` with `lines`. The caret of an empty selection keeps its
/// place on its line, shifted by the change in front of it (`shift`); a selection covers the
/// rewritten lines.
fn replace_block(
    chars: &[char],
    block: Range<usize>,
    lines: &[String],
    sel: Selection,
    shift: impl Fn(usize, usize) -> isize,
) -> Edit {
    let joined = lines.join("\n");
    let new_len = joined.chars().count();
    let text = join(&[&chars[..block.start]]) + &joined + &join(&[&chars[block.end..]]);
    let selection = if sel.is_empty() {
        // Line of the caret within the block and its column.
        let before = &chars[block.start..sel.caret.min(block.end)];
        let line = before.iter().filter(|&&c| c == '\n').count();
        let column = before.iter().rev().take_while(|&&c| c != '\n').count();
        let line_start: usize = lines[..line].iter().map(|l| l.chars().count() + 1).sum();
        let line_len = lines.get(line).map_or(0, |l| l.chars().count());
        let moved = (column as isize + shift(line, column)).clamp(0, line_len as isize) as usize;
        Selection::caret(block.start + line_start + moved)
    } else {
        Selection::new(block.start, block.start + new_len)
    };
    Edit { text, selection }
}

fn block_lines(chars: &[char], block: &Range<usize>) -> Vec<String> {
    chars[block.clone()]
        .iter()
        .collect::<String>()
        .split('\n')
        .map(str::to_owned)
        .collect()
}

fn closer(c: char) -> Option<char> {
    match c {
        '(' => Some(')'),
        '[' => Some(']'),
        '\'' => Some('\''),
        '"' => Some('"'),
        _ => None,
    }
}

/// Typing `ch` with brackets and quotes closed automatically: `(`, `[`, `'` and `"` insert
/// their pair (or wrap the selection), and typing a closer right before the same closer steps
/// over it. Returns `None` when the character should be inserted normally.
pub fn type_char(text: &str, sel: Selection, ch: char) -> Option<Edit> {
    let chars: Vec<char> = text.chars().collect();
    let r = sel.range();
    if r.end > chars.len() {
        return None;
    }
    let next = chars.get(r.end).copied();
    let prev = r.start.checked_sub(1).map(|i| chars[i]);
    if sel.is_empty() && matches!(ch, ')' | ']' | '\'' | '"') && next == Some(ch) {
        return Some(Edit {
            text: text.to_owned(),
            selection: Selection::caret(r.end + 1),
        });
    }
    let close = closer(ch)?;
    if !sel.is_empty() {
        let inner = &chars[r.clone()];
        let text = join(&[&chars[..r.start], &[ch], inner, &[close], &chars[r.end..]]);
        return Some(Edit {
            text,
            selection: Selection::new(r.start + 1, r.end + 1),
        });
    }
    // Only before blanks, closers or separators, so typing in front of a word stays plain.
    let free_after = next.is_none_or(|c| c.is_whitespace() || matches!(c, ')' | ']' | ',' | ';'));
    if !free_after {
        return None;
    }
    if matches!(ch, '\'' | '"') && prev.is_some_and(is_word) {
        // An apostrophe inside a word stays single; N'…' (Unicode literals) still pairs.
        let word_start = chars[..r.start]
            .iter()
            .rposition(|&c| !is_word(c))
            .map_or(0, |i| i + 1);
        let word: String = chars[word_start..r.start].iter().collect();
        if !(ch == '\'' && word.eq_ignore_ascii_case("n")) {
            return None;
        }
    }
    let text = join(&[&chars[..r.start], &[ch, close], &chars[r.end..]]);
    Some(Edit {
        text,
        selection: Selection::caret(r.start + 1),
    })
}

/// Backspace between an empty pair (`(|)`, `'|'`, …) deletes both characters.
pub fn backspace(text: &str, sel: Selection) -> Option<Edit> {
    if !sel.is_empty() || sel.caret == 0 {
        return None;
    }
    let chars: Vec<char> = text.chars().collect();
    let at = sel.caret;
    let open = *chars.get(at - 1)?;
    let close = *chars.get(at)?;
    if closer(open) != Some(close) {
        return None;
    }
    Some(Edit {
        text: join(&[&chars[..at - 1], &chars[at + 1..]]),
        selection: Selection::caret(at - 1),
    })
}

/// Make Uppercase / Make Lowercase of the selection, or of the word at the caret.
pub fn change_case(text: &str, sel: Selection, upper: bool) -> Option<Edit> {
    let chars: Vec<char> = text.chars().collect();
    let range = if sel.is_empty() {
        let at = sel.caret.min(chars.len());
        let start = chars[..at]
            .iter()
            .rposition(|&c| !is_word(c))
            .map_or(0, |i| i + 1);
        let end = chars[at..]
            .iter()
            .position(|&c| !is_word(c))
            .map_or(chars.len(), |i| at + i);
        start..end
    } else {
        sel.range()
    };
    if range.is_empty() || range.end > chars.len() {
        return None;
    }
    let part: String = chars[range.clone()].iter().collect();
    let changed = if upper {
        part.to_uppercase()
    } else {
        part.to_lowercase()
    };
    // Case mapping can change the length (ß → SS); keep the selection on the new text.
    let len = changed.chars().count();
    let text = join(&[&chars[..range.start]]) + &changed + &join(&[&chars[range.end..]]);
    let selection = if sel.is_empty() {
        Selection::caret(sel.caret.min(range.start + len))
    } else if sel.anchor <= sel.caret {
        Selection::new(range.start, range.start + len)
    } else {
        Selection::new(range.start + len, range.start)
    };
    Some(Edit { text, selection })
}

/// Move Selected Lines Up / Down: swaps the touched lines with the neighbouring line.
pub fn move_lines(text: &str, sel: Selection, up: bool) -> Option<Edit> {
    let chars: Vec<char> = text.chars().collect();
    let block = line_block(&chars, sel);
    let (moved, delta) = if up {
        if block.start == 0 {
            return None;
        }
        let prev_start = chars[..block.start - 1]
            .iter()
            .rposition(|&c| c == '\n')
            .map_or(0, |i| i + 1);
        let prev = &chars[prev_start..block.start - 1];
        let text = join(&[
            &chars[..prev_start],
            &chars[block.clone()],
            &['\n'],
            prev,
            &chars[block.end..],
        ]);
        (text, -((prev.len() + 1) as isize))
    } else {
        if block.end >= chars.len() {
            return None;
        }
        let next_end = chars[block.end + 1..]
            .iter()
            .position(|&c| c == '\n')
            .map_or(chars.len(), |i| block.end + 1 + i);
        let next = &chars[block.end + 1..next_end];
        let text = join(&[
            &chars[..block.start],
            next,
            &['\n'],
            &chars[block.clone()],
            &chars[next_end..],
        ]);
        (text, (next.len() + 1) as isize)
    };
    let shift = |i: usize| (i as isize + delta) as usize;
    Some(Edit {
        text: moved,
        selection: Selection::new(shift(sel.anchor), shift(sel.caret)),
    })
}

/// Delete Horizontal White Space: removes the spaces and tabs around the caret, or in a
/// selection collapses runs of them to one space and drops trailing ones (indentation stays).
pub fn delete_horizontal_whitespace(text: &str, sel: Selection) -> Option<Edit> {
    let chars: Vec<char> = text.chars().collect();
    let blank = |c: char| c == ' ' || c == '\t';
    if sel.is_empty() {
        let at = sel.caret.min(chars.len());
        let start = chars[..at]
            .iter()
            .rposition(|&c| !blank(c))
            .map_or(0, |i| i + 1);
        let end = chars[at..]
            .iter()
            .position(|&c| !blank(c))
            .map_or(chars.len(), |i| at + i);
        if start == end {
            return None;
        }
        return Some(Edit {
            text: join(&[&chars[..start], &chars[end..]]),
            selection: Selection::caret(start),
        });
    }
    let r = sel.range();
    let selected: String = chars[r.clone()].iter().collect();
    let at_line_start = r.start == 0 || chars[r.start - 1] == '\n';
    let mut out = String::new();
    for (i, line) in selected.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let indent_len = if i > 0 || at_line_start {
            line.len() - line.trim_start_matches(blank).len()
        } else {
            0
        };
        let (indent, rest) = line.split_at(indent_len);
        out.push_str(indent);
        let mut pending = false;
        for c in rest.chars() {
            if blank(c) {
                pending = true;
            } else {
                if pending {
                    out.push(' ');
                    pending = false;
                }
                out.push(c);
            }
        }
    }
    if out == selected {
        return None;
    }
    let len = out.chars().count();
    Some(Edit {
        text: join(&[&chars[..r.start]]) + &out + &join(&[&chars[r.end..]]),
        selection: Selection::new(r.start, r.start + len),
    })
}

/// Lines a whole-document command works on: the touched lines, or every line without a
/// selection.
fn block_or_all(chars: &[char], sel: Selection) -> Range<usize> {
    if sel.is_empty() {
        0..chars.len()
    } else {
        line_block(chars, sel)
    }
}

/// Sort Lines: the selected lines (or all lines) in ascending order, ignoring case first.
pub fn sort_lines(text: &str, sel: Selection) -> Option<Edit> {
    let chars: Vec<char> = text.chars().collect();
    let block = block_or_all(&chars, sel);
    let mut lines = block_lines(&chars, &block);
    let before = lines.clone();
    lines.sort_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()).then(a.cmp(b)));
    (lines != before).then(|| replace_block(&chars, block, &lines, Selection::new(0, 1), |_, _| 0))
}

/// Join Lines: the selected lines (or the caret's line and the next) joined by one space.
pub fn join_lines(text: &str, sel: Selection) -> Option<Edit> {
    let chars: Vec<char> = text.chars().collect();
    let mut block = line_block(&chars, sel);
    if sel.is_empty() || !chars[block.clone()].contains(&'\n') {
        if block.end >= chars.len() {
            return None;
        }
        block.end = chars[block.end + 1..]
            .iter()
            .position(|&c| c == '\n')
            .map_or(chars.len(), |i| block.end + 1 + i);
    }
    let lines = block_lines(&chars, &block);
    let mut joined = lines[0].trim_end().to_owned();
    for line in &lines[1..] {
        let part = line.trim();
        if !part.is_empty() {
            if !joined.is_empty() {
                joined.push(' ');
            }
            joined.push_str(part);
        }
    }
    let caret = block.start + joined.chars().count();
    let text = join(&[&chars[..block.start]]) + &joined + &join(&[&chars[block.end..]]);
    Some(Edit {
        text,
        selection: Selection::caret(caret),
    })
}

/// Expand Selection to Line: selects the touched lines including their line break.
pub fn expand_to_line(text: &str, sel: Selection) -> Selection {
    let chars: Vec<char> = text.chars().collect();
    let block = line_block(&chars, sel);
    let end = if block.end < chars.len() {
        block.end + 1
    } else {
        block.end
    };
    Selection::new(block.start, end)
}

fn indent_of(line: &str) -> &str {
    &line[..line.len() - line.trim_start_matches([' ', '\t']).len()]
}

fn indent_width(indent: &str) -> usize {
    indent.chars().fold(0, |w, c| {
        if c == '\t' {
            (w / TAB_WIDTH + 1) * TAB_WIDTH
        } else {
            w + 1
        }
    })
}

/// Comment Selection (`comment`) or Uncomment Selection on the touched lines. Comments go in
/// at the smallest indentation of the non-blank lines; uncommenting removes the first `--`
/// after the indentation.
pub fn toggle_comment(text: &str, sel: Selection, comment: bool) -> Option<Edit> {
    let chars: Vec<char> = text.chars().collect();
    let block = line_block(&chars, sel);
    let lines = block_lines(&chars, &block);
    let column = lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| indent_of(l).chars().count())
        .min()?;
    let mut shifts = Vec::with_capacity(lines.len());
    let new: Vec<String> = lines
        .iter()
        .map(|line| {
            let indent = indent_of(line);
            if comment {
                if line.trim().is_empty() {
                    shifts.push((usize::MAX, 0));
                    return line.clone();
                }
                let at: usize = line.chars().take(column).map(char::len_utf8).sum();
                shifts.push((column, 2));
                format!("{}--{}", &line[..at], &line[at..])
            } else if line[indent.len()..].starts_with("--") {
                shifts.push((indent.chars().count(), -2));
                format!("{indent}{}", &line[indent.len() + 2..])
            } else {
                shifts.push((usize::MAX, 0));
                line.clone()
            }
        })
        .collect();
    if new == lines {
        return None;
    }
    Some(replace_block(&chars, block, &new, sel, |line, col| {
        let (at, delta) = shifts[line];
        if col >= at { delta } else { 0 }
    }))
}

/// Increase (`increase`) or Decrease Line Indent of the touched non-blank lines by one tab
/// (decreasing also removes up to [`TAB_WIDTH`] spaces).
pub fn indent_lines(text: &str, sel: Selection, increase: bool) -> Option<Edit> {
    let chars: Vec<char> = text.chars().collect();
    let block = line_block(&chars, sel);
    let lines = block_lines(&chars, &block);
    let mut shifts = Vec::with_capacity(lines.len());
    let new: Vec<String> = lines
        .iter()
        .map(|line| {
            if increase {
                if line.trim().is_empty() {
                    shifts.push(0);
                    return line.clone();
                }
                shifts.push(1);
                format!("\t{line}")
            } else {
                let removed = if line.starts_with('\t') {
                    1
                } else {
                    line.chars()
                        .take(TAB_WIDTH)
                        .take_while(|&c| c == ' ')
                        .count()
                };
                shifts.push(-(removed as isize));
                line[removed..].to_owned()
            }
        })
        .collect();
    if new == lines {
        return None;
    }
    Some(replace_block(&chars, block, &new, sel, |line, _| {
        shifts[line]
    }))
}

/// Tabify (`to_tabs`) or Untabify the indentation of the selected lines (or all lines).
pub fn retab(text: &str, sel: Selection, to_tabs: bool) -> Option<Edit> {
    let chars: Vec<char> = text.chars().collect();
    let block = block_or_all(&chars, sel);
    let lines = block_lines(&chars, &block);
    let mut shifts = Vec::with_capacity(lines.len());
    let new: Vec<String> = lines
        .iter()
        .map(|line| {
            let indent = indent_of(line);
            let width = indent_width(indent);
            let replaced = if to_tabs {
                "\t".repeat(width / TAB_WIDTH) + &" ".repeat(width % TAB_WIDTH)
            } else {
                " ".repeat(width)
            };
            shifts.push(replaced.chars().count() as isize - indent.chars().count() as isize);
            format!("{replaced}{}", &line[indent.len()..])
        })
        .collect();
    if new == lines {
        return None;
    }
    Some(replace_block(&chars, block, &new, sel, |line, _| {
        shifts[line]
    }))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Space,
    Lower,
    Upper,
    Digit,
    Underscore,
    Other,
}

fn class(c: char) -> Class {
    if c.is_whitespace() {
        Class::Space
    } else if c == '_' {
        Class::Underscore
    } else if c.is_uppercase() {
        Class::Upper
    } else if c.is_alphabetic() {
        Class::Lower
    } else if c.is_numeric() {
        Class::Digit
    } else {
        Class::Other
    }
}

/// Next subword (`forward`) or Previous subword: the caret position after moving over one
/// part of an identifier (`order_line`, `OrderLine`, `HTTPServer` have two parts each).
pub fn subword(text: &str, caret: usize, forward: bool) -> usize {
    let chars: Vec<char> = text.chars().collect();
    let classes: Vec<Class> = chars.iter().map(|&c| class(c)).collect();
    let n = chars.len();
    let mut i = caret.min(n);
    if forward {
        while i < n && matches!(classes[i], Class::Space | Class::Underscore) {
            i += 1;
        }
        if i >= n {
            return n;
        }
        let start = classes[i];
        i += 1;
        match start {
            Class::Upper => {
                if i < n && classes[i] == Class::Lower {
                    while i < n && classes[i] == Class::Lower {
                        i += 1;
                    }
                } else {
                    // An acronym ends before the capital that starts the next part.
                    while i < n
                        && classes[i] == Class::Upper
                        && classes.get(i + 1) != Some(&Class::Lower)
                    {
                        i += 1;
                    }
                }
            }
            c => {
                while i < n && classes[i] == c {
                    i += 1;
                }
            }
        }
        i
    } else {
        while i > 0 && matches!(classes[i - 1], Class::Space | Class::Underscore) {
            i -= 1;
        }
        if i == 0 {
            return 0;
        }
        let end = classes[i - 1];
        i -= 1;
        match end {
            Class::Lower => {
                while i > 0 && classes[i - 1] == Class::Lower {
                    i -= 1;
                }
                if i > 0 && classes[i - 1] == Class::Upper {
                    i -= 1;
                }
            }
            c => {
                while i > 0 && classes[i - 1] == c {
                    i -= 1;
                }
            }
        }
        i
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caret(at: usize) -> Selection {
        Selection::caret(at)
    }

    #[test]
    fn pairs_brackets_and_quotes() {
        let e = type_char("select ", caret(7), '(').unwrap();
        assert_eq!((e.text.as_str(), e.selection), ("select ()", caret(8)));
        let e = type_char("x = ", caret(4), '\'').unwrap();
        assert_eq!((e.text.as_str(), e.selection), ("x = ''", caret(5)));
        // Stepping over the closer that is already there.
        let e = type_char("('a')", caret(3), '\'').unwrap();
        assert_eq!((e.text.as_str(), e.selection), ("('a')", caret(4)));
        let e = type_char("(a)", caret(2), ')').unwrap();
        assert_eq!(e.selection, caret(3));
        // N'…' pairs; an apostrophe inside a word does not; neither does typing before a word.
        assert_eq!(type_char("N", caret(1), '\'').unwrap().text, "N''");
        assert!(type_char("don", caret(3), '\'').is_none());
        assert!(type_char("abc", caret(0), '(').is_none());
        assert!(type_char("a", caret(1), 'b').is_none());
    }

    #[test]
    fn wraps_selection() {
        let e = type_char("select a", Selection::new(7, 8), '(').unwrap();
        assert_eq!(e.text, "select (a)");
        assert_eq!(e.selection, Selection::new(8, 9));
        assert_eq!(
            type_char("abc", Selection::new(0, 3), '\'').unwrap().text,
            "'abc'"
        );
    }

    #[test]
    fn backspace_removes_empty_pair() {
        let e = backspace("f()", caret(2)).unwrap();
        assert_eq!((e.text.as_str(), e.selection), ("f", caret(1)));
        assert!(backspace("f(x)", caret(2)).is_none());
        assert!(backspace("()", Selection::new(0, 2)).is_none());
    }

    #[test]
    fn changes_case_of_selection_or_word() {
        let e = change_case("select a", Selection::new(0, 6), true).unwrap();
        assert_eq!(e.text, "SELECT a");
        assert_eq!(e.selection, Selection::new(0, 6));
        assert_eq!(
            change_case("FROM x", caret(2), false).unwrap().text,
            "from x"
        );
        assert!(change_case("a  b", caret(2), true).is_none());
    }

    #[test]
    fn moves_lines() {
        let e = move_lines("a\nbb\nc", caret(3), true).unwrap();
        assert_eq!((e.text.as_str(), e.selection), ("bb\na\nc", caret(1)));
        let e = move_lines("a\nbb\nc", caret(3), false).unwrap();
        assert_eq!((e.text.as_str(), e.selection), ("a\nc\nbb", caret(5)));
        assert!(move_lines("a\nb", caret(0), true).is_none());
        assert!(move_lines("a\nb", caret(3), false).is_none());
        // A selection ending at the start of a line moves only the lines above it.
        let e = move_lines("a\nb\nc", Selection::new(0, 2), false).unwrap();
        assert_eq!(e.text, "b\na\nc");
    }

    #[test]
    fn deletes_horizontal_whitespace() {
        let e = delete_horizontal_whitespace("a   b", caret(2)).unwrap();
        assert_eq!((e.text.as_str(), e.selection), ("ab", caret(1)));
        let e = delete_horizontal_whitespace("  a  =\t b  \n  c", Selection::new(0, 15)).unwrap();
        assert_eq!(e.text, "  a = b\n  c");
    }

    #[test]
    fn sorts_and_joins_lines() {
        assert_eq!(sort_lines("b\nA\nc", caret(0)).unwrap().text, "A\nb\nc");
        let e = sort_lines("z\nb\na\ny", Selection::new(2, 5)).unwrap();
        assert_eq!(e.text, "z\na\nb\ny");
        let e = join_lines("select a,\n   b\nfrom t", caret(0)).unwrap();
        assert_eq!(e.text, "select a, b\nfrom t");
        let e = join_lines("a\n b\n c\nd", Selection::new(0, 6)).unwrap();
        assert_eq!(e.text, "a b c\nd");
    }

    #[test]
    fn expands_to_line() {
        assert_eq!(expand_to_line("ab\ncd\nef", caret(4)), Selection::new(3, 6));
        assert_eq!(expand_to_line("ab\ncd", caret(4)), Selection::new(3, 5));
    }

    #[test]
    fn comments_and_uncomments_lines() {
        let e = toggle_comment("  a\n    b\n\n  c", Selection::new(0, 13), true).unwrap();
        assert_eq!(e.text, "  --a\n  --  b\n\n  --c");
        let back = toggle_comment(&e.text, Selection::new(0, 19), false).unwrap();
        assert_eq!(back.text, "  a\n    b\n\n  c");
        let e = toggle_comment("select 1", caret(3), true).unwrap();
        assert_eq!((e.text.as_str(), e.selection), ("--select 1", caret(5)));
        assert!(toggle_comment("select 1", caret(0), false).is_none());
    }

    #[test]
    fn indents_and_retabs() {
        let e = indent_lines("a\n\nb", Selection::new(0, 4), true).unwrap();
        assert_eq!(e.text, "\ta\n\n\tb");
        assert_eq!(
            indent_lines("\ta\n      b", Selection::new(0, 10), false)
                .unwrap()
                .text,
            "a\n  b"
        );
        assert_eq!(
            retab("      a\n\tb", caret(0), true).unwrap().text,
            "\t  a\n\tb"
        );
        assert_eq!(retab("\t  a", caret(0), false).unwrap().text, "      a");
    }

    #[test]
    fn moves_by_subword() {
        let text = "order_line OrderLine HTTPServer";
        assert_eq!(subword(text, 0, true), 5);
        assert_eq!(subword(text, 5, true), 10);
        assert_eq!(subword(text, 10, true), 16);
        assert_eq!(subword(text, 16, true), 20);
        assert_eq!(subword(text, 20, true), 25);
        assert_eq!(subword(text, 25, true), 31);
        assert_eq!(subword(text, 31, false), 25);
        assert_eq!(subword(text, 25, false), 21);
        assert_eq!(subword(text, 20, false), 16);
        assert_eq!(subword(text, 10, false), 6);
    }
}
