//! Find and replace in the SQL editor: the bar above the text, match highlighting, and the
//! commands behind Ctrl+F, Ctrl+H, F3 and Shift+F3.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::Range;

use egui::text::{ByteIndex, CCursor, LayoutJob, LayoutSection};
use egui::{Color32, Key, Margin};
use regex::{Regex, RegexBuilder};

use super::*;

/// More matches than this are not listed (the count says "10000+").
const MAX_MATCHES: usize = 10_000;

/// One occurrence of the search text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Match {
    /// Byte range in the editor text.
    pub bytes: Range<usize>,
    /// Character range in the editor text (what the text editor's cursor counts).
    pub chars: Range<usize>,
}

/// What to look for.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub(super) struct FindOptions {
    pub match_case: bool,
    pub whole_word: bool,
    pub regex: bool,
}

/// Matches of the last search, kept until the text or the search changes.
#[derive(Default)]
struct Cache {
    key: Option<u64>,
    regex: Option<Regex>,
    matches: Vec<Match>,
    error: Option<String>,
}

/// Find bar of a query tab. Not persisted.
#[derive(Default)]
pub struct FindState {
    /// The bar is shown.
    pub(super) open: bool,
    /// The replace row is shown.
    pub(super) replace: bool,
    pub(super) query: String,
    pub(super) replacement: String,
    pub(super) options: FindOptions,
    /// Give the search field (or the replace field) the keyboard next frame.
    focus: Option<Field>,
    /// Caret position (characters) the search started from; typing finds from here.
    origin: usize,
    /// Scroll this character into view next frame.
    pub(super) scroll_to: Option<usize>,
    /// Characters of the current match. Kept here because the text editor drops its
    /// selection while the find bar has the keyboard.
    current: Option<Range<usize>>,
    /// Result of the last replace, shown until the search changes.
    notice: Option<String>,
    cache: Cache,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    Query,
    Replacement,
}

impl FindState {
    /// Whether the bar is shown.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Matches in `text`, searched again only when the text or the search changed.
    fn refresh(&mut self, text: &str) {
        let mut hasher = DefaultHasher::new();
        text.hash(&mut hasher);
        self.query.hash(&mut hasher);
        self.options.hash(&mut hasher);
        let key = hasher.finish();
        if self.cache.key == Some(key) {
            return;
        }
        self.cache = match compile(&self.query, &self.options) {
            Ok(Some(regex)) => Cache {
                key: Some(key),
                matches: find_matches(text, &regex, self.options.whole_word),
                regex: Some(regex),
                error: None,
            },
            Ok(None) => Cache {
                key: Some(key),
                ..Cache::default()
            },
            Err(error) => Cache {
                key: Some(key),
                error: Some(error),
                ..Cache::default()
            },
        };
    }

    /// Matches for highlighting, as character ranges, with the current one marked. Empty
    /// when the bar is closed or the text changed since the last search.
    pub(super) fn highlights(&self, text_len: usize) -> Highlights {
        if !self.open
            || self
                .cache
                .matches
                .last()
                .is_some_and(|m| m.bytes.end > text_len)
        {
            return Highlights::default();
        }
        Highlights(
            self.cache
                .matches
                .iter()
                .map(|m| (m.chars.clone(), self.current.as_ref() == Some(&m.chars)))
                .collect(),
        )
    }

    /// Index of the current match.
    fn current_index(&self) -> Option<usize> {
        let current = self.current.as_ref()?;
        self.cache.matches.iter().position(|m| m.chars == *current)
    }

    /// Forgets the current match once the editor, holding the keyboard, selects something
    /// else (the user clicked or typed there).
    pub(super) fn follow_editor(&mut self, ctx: &egui::Context, editor_id: Id) {
        if self.current.is_some()
            && let Some(sel) = current_selection(ctx, editor_id)
            && Some(sel.range()) != self.current
        {
            self.current = None;
        }
    }
}

/// Character ranges of the matches, and whether each one is the current match.
#[derive(Default)]
pub(super) struct Highlights(Vec<(Range<usize>, bool)>);

impl Highlights {
    /// Gives the matches in `job` a background. `job.text` may differ from the editor text
    /// in bytes (visible white space), never in characters.
    pub(super) fn paint(&self, job: &mut LayoutJob) {
        if self.0.is_empty() {
            return;
        }
        let ranges = chars_to_bytes(&job.text, &self.0);
        let other = color::warning().gamma_multiply(0.3);
        let current = color::warning().gamma_multiply(0.65);
        paint_ranges(job, &ranges, other, current);
    }
}

/// The search as a regular expression; `None` for an empty search.
fn compile(query: &str, options: &FindOptions) -> Result<Option<Regex>, String> {
    if query.is_empty() {
        return Ok(None);
    }
    let pattern = if options.regex {
        query.to_owned()
    } else {
        regex::escape(query)
    };
    RegexBuilder::new(&pattern)
        .case_insensitive(!options.match_case)
        .multi_line(true)
        .build()
        .map(Some)
        .map_err(|e| match e {
            regex::Error::Syntax(text) => text.lines().last().unwrap_or("").trim().to_owned(),
            other => other.to_string(),
        })
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Non-empty matches of `regex` in `text`; with `whole_word`, only those not touching a
/// letter, digit or underscore on either side.
fn find_matches(text: &str, regex: &Regex, whole_word: bool) -> Vec<Match> {
    let byte_ranges = regex
        .find_iter(text)
        .map(|m| m.range())
        .filter(|r| !r.is_empty())
        .filter(|r| {
            !whole_word
                || (text[..r.start]
                    .chars()
                    .next_back()
                    .is_none_or(|c| !is_word(c))
                    && text[r.end..].chars().next().is_none_or(|c| !is_word(c)))
        })
        .take(MAX_MATCHES + 1);
    // Byte to character offsets in one pass over the text.
    let mut out = Vec::new();
    let mut chars = 0;
    let mut at = 0;
    for bytes in byte_ranges {
        chars += text[at..bytes.start].chars().count();
        let start = chars;
        chars += text[bytes.clone()].chars().count();
        at = bytes.end;
        out.push(Match {
            chars: start..chars,
            bytes,
        });
    }
    out
}

/// Character ranges (sorted, not overlapping) as byte ranges of `text`. Ranges past the end
/// are dropped.
fn chars_to_bytes(text: &str, ranges: &[(Range<usize>, bool)]) -> Vec<(Range<usize>, bool)> {
    let mut offsets = text
        .char_indices()
        .map(|(i, _)| i)
        .chain(std::iter::once(text.len()));
    let mut out = Vec::with_capacity(ranges.len());
    let mut char_at = 0;
    let mut next = offsets.next();
    let mut byte_of = |target: usize| -> Option<usize> {
        while char_at < target {
            next = offsets.next();
            char_at += 1;
        }
        next
    };
    for (range, current) in ranges {
        let (Some(start), Some(end)) = (byte_of(range.start), byte_of(range.end)) else {
            break;
        };
        out.push((start..end, *current));
    }
    out
}

/// Splits the sections of `job` at the match boundaries and fills the matches.
fn paint_ranges(
    job: &mut LayoutJob,
    ranges: &[(Range<usize>, bool)],
    other: Color32,
    current: Color32,
) {
    let mut out = Vec::with_capacity(job.sections.len() + ranges.len() * 2);
    let mut m = 0;
    for section in std::mem::take(&mut job.sections) {
        let (first, end) = (section.byte_range.start.0, section.byte_range.end.0);
        let mut start = first;
        while start < end {
            while ranges.get(m).is_some_and(|(r, _)| r.end <= start) {
                m += 1;
            }
            let (piece_end, fill) = match ranges.get(m) {
                Some((r, cur)) if r.start <= start => (r.end.min(end), Some(*cur)),
                Some((r, _)) if r.start < end => (r.start, None),
                _ => (end, None),
            };
            let mut format = section.format.clone();
            if let Some(cur) = fill {
                format.background = if cur { current } else { other };
            }
            out.push(LayoutSection {
                leading_space: if start == first {
                    section.leading_space
                } else {
                    0.0
                },
                byte_range: ByteIndex(start)..ByteIndex(piece_end),
                format,
            });
            start = piece_end;
        }
    }
    job.sections = out;
}

fn field_id(tab: u64, field: Field) -> Id {
    Id::new(("find-field", tab, field == Field::Query))
}

/// Opens the bar (Ctrl+F, or Ctrl+H with `replace`). A selection within one line becomes
/// the search text.
pub(super) fn open(ctx: &egui::Context, editor_id: Id, q: &mut QueryTab, replace: bool) {
    let sel = current_selection(ctx, editor_id).unwrap_or(Selection::caret(0));
    let f = &mut q.run.find;
    let range = sel.range();
    let selected: String = q
        .sql
        .chars()
        .skip(range.start)
        .take(range.end - range.start)
        .collect();
    if !selected.is_empty() && !selected.contains('\n') {
        f.query = if f.options.regex {
            regex::escape(&selected)
        } else {
            selected
        };
    }
    f.open = true;
    f.replace = replace;
    f.origin = range.start;
    f.notice = None;
    f.focus = Some(Field::Query);
    // A selected match stays the current one.
    f.refresh(&q.sql);
    f.current = f
        .cache
        .matches
        .iter()
        .find(|m| m.chars == range)
        .map(|m| m.chars.clone());
    // The search text is selected, so typing replaces it.
    let id = field_id(q.id, Field::Query);
    let mut state = egui::text_edit::TextEditState::load(ctx, id).unwrap_or_default();
    state
        .cursor
        .set_char_range(Some(egui::text::CCursorRange::two(
            CCursor::new(0),
            CCursor::new(f.query.chars().count()),
        )));
    state.store(ctx, id);
}

/// Selects the next (or previous) match after the editor's selection, wrapping around.
/// Opens the bar first when there is nothing to look for.
pub(super) fn step(ctx: &egui::Context, editor_id: Id, q: &mut QueryTab, forward: bool) {
    if q.run.find.query.is_empty() {
        open(ctx, editor_id, q, q.run.find.replace);
        return;
    }
    q.run.find.open = true;
    q.run.find.notice = None;
    q.run.find.refresh(&q.sql);
    // From the current match, else from the editor's caret or selection.
    let sel = q.run.find.current.clone().unwrap_or_else(|| {
        current_selection(ctx, editor_id)
            .unwrap_or(Selection::caret(0))
            .range()
    });
    let matches = &q.run.find.cache.matches;
    let found = if forward {
        matches
            .iter()
            .find(|m| m.chars.start >= sel.end && m.chars != sel)
            .or(matches.first())
    } else {
        matches
            .iter()
            .rev()
            .find(|m| m.chars.end <= sel.start)
            .or(matches.last())
    };
    if let Some(m) = found.cloned() {
        select(ctx, editor_id, q, &m);
    }
}

fn select(ctx: &egui::Context, editor_id: Id, q: &mut QueryTab, m: &Match) {
    set_selection(ctx, editor_id, Selection::new(m.chars.start, m.chars.end));
    q.run.find.scroll_to = Some(m.chars.start);
    q.run.find.current = Some(m.chars.clone());
}

/// Text that replaces `m`: the replacement as typed, or with `$1` / `${name}` filled in from
/// the match when searching with a regular expression.
fn replacement_for(f: &FindState, text: &str, m: &Match) -> String {
    if f.options.regex
        && let Some(regex) = &f.cache.regex
        && let Some(caps) = regex.captures_at(text, m.bytes.start)
        && caps.get(0).is_some_and(|c| c.range() == m.bytes)
    {
        let mut out = String::new();
        caps.expand(&f.replacement, &mut out);
        return out;
    }
    f.replacement.clone()
}

/// Replaces the selected match and selects the next one. When no match is selected, selects
/// the next one first (as Find Next would).
fn replace_one(ctx: &egui::Context, editor_id: Id, q: &mut QueryTab) {
    q.run.find.refresh(&q.sql);
    let Some(i) = q.run.find.current_index() else {
        step(ctx, editor_id, q, true);
        return;
    };
    let m = q.run.find.cache.matches[i].clone();
    let with = replacement_for(&q.run.find, &q.sql, &m);
    let mut text = q.sql.clone();
    text.replace_range(m.bytes.clone(), &with);
    let caret = m.chars.start + with.chars().count();
    apply_edit(
        ctx,
        editor_id,
        q,
        textops::Edit {
            text,
            selection: Selection::caret(caret),
        },
    );
    q.run.find.refresh(&q.sql);
    let next = q
        .run
        .find
        .cache
        .matches
        .iter()
        .find(|m| m.chars.start >= caret);
    match next.or(q.run.find.cache.matches.first()).cloned() {
        Some(m) => select(ctx, editor_id, q, &m),
        None => q.run.find.current = None,
    }
}

/// Replaces every match, as one undo step.
fn replace_all(ctx: &egui::Context, editor_id: Id, q: &mut QueryTab) {
    q.run.find.refresh(&q.sql);
    let f = &q.run.find;
    let count = f.cache.matches.len();
    if count == 0 {
        return;
    }
    let mut text = String::with_capacity(q.sql.len());
    let mut at = 0;
    let mut first = 0;
    for (i, m) in f.cache.matches.iter().enumerate() {
        text.push_str(&q.sql[at..m.bytes.start]);
        if i == 0 {
            first = text.chars().count();
        }
        text.push_str(&replacement_for(f, &q.sql, m));
        at = m.bytes.end;
    }
    text.push_str(&q.sql[at..]);
    apply_edit(
        ctx,
        editor_id,
        q,
        textops::Edit {
            text,
            selection: Selection::caret(first),
        },
    );
    let count = if count > MAX_MATCHES {
        format!("{MAX_MATCHES}+")
    } else {
        format_count(count as u64)
    };
    q.run.find.notice = Some(format!("{count} replaced"));
    q.run.find.current = None;
}

/// "3 of 12", "12 matches", "No results", or why the regular expression is not valid.
fn status(f: &FindState, current: Option<usize>) -> (String, Color32) {
    if let Some(error) = &f.cache.error {
        return (error.clone(), color::danger());
    }
    if let Some(notice) = &f.notice {
        return (notice.clone(), color::text_dim());
    }
    let n = f.cache.matches.len();
    let text = match (n, current) {
        (_, _) if f.query.is_empty() => String::new(),
        (0, _) => "No results".to_owned(),
        (n, _) if n > MAX_MATCHES => format!("{MAX_MATCHES}+ matches"),
        (n, Some(i)) => format!("{} of {}", i + 1, format_count(n as u64)),
        (1, None) => "1 match".to_owned(),
        (n, None) => format!("{} matches", format_count(n as u64)),
    };
    let paint = if n == 0 && !f.query.is_empty() {
        color::warning()
    } else {
        color::text_dim()
    };
    (text, paint)
}

/// The find bar, at the top of the editor area.
pub(super) fn bar(ui: &mut Ui, q: &mut QueryTab, editor_id: Id) {
    let ctx = ui.ctx().clone();
    let query_id = field_id(q.id, Field::Query);
    let replacement_id = field_id(q.id, Field::Replacement);
    if let Some(field) = q.run.find.focus.take() {
        let id = if field == Field::Query {
            query_id
        } else {
            replacement_id
        };
        ctx.memory_mut(|m| m.request_focus(id));
    }
    let before = (q.run.find.query.clone(), q.run.find.options.clone());
    let mut step_dir: Option<bool> = None;
    let (mut close, mut one, mut all) = (false, false, false);
    let toggle_width = ui.spacing().interact_size.y + 6.0;

    Frame::new()
        .fill(color::panel())
        .inner_margin(Margin::symmetric(6, 4))
        .stroke(Stroke::new(1.0, color::border()))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let f = &mut q.run.find;
                let caret = if f.replace {
                    icon::CARET_DOWN
                } else {
                    icon::CARET_RIGHT
                };
                if ui
                    .add_sized([toggle_width, 0.0], Button::new(caret).frame(false))
                    .on_hover_text("Show replace (Ctrl+H)")
                    .clicked()
                {
                    f.replace = !f.replace;
                }
                let field = ui.add(
                    TextEdit::singleline(&mut f.query)
                        .id(query_id)
                        .hint_text("Find")
                        .desired_width(260.0),
                );
                if field.lost_focus() {
                    let (enter, shift, escape) = ui.input(|i| {
                        (
                            i.key_pressed(Key::Enter),
                            i.modifiers.shift,
                            i.key_pressed(Key::Escape),
                        )
                    });
                    if enter {
                        step_dir = Some(!shift);
                        f.focus = Some(Field::Query);
                    } else if escape {
                        close = true;
                    }
                }
                ui.toggle_value(&mut f.options.match_case, "Aa")
                    .on_hover_text("Match case");
                ui.toggle_value(&mut f.options.whole_word, "ab")
                    .on_hover_text("Match whole word");
                ui.toggle_value(&mut f.options.regex, ".*")
                    .on_hover_text("Use regular expressions");
                if ui
                    .button(icon::ARROW_UP)
                    .on_hover_text("Find previous (Shift+F3)")
                    .clicked()
                {
                    step_dir = Some(false);
                }
                if ui
                    .button(icon::ARROW_DOWN)
                    .on_hover_text("Find next (F3)")
                    .clicked()
                {
                    step_dir = Some(true);
                }
                f.refresh(&q.sql);
                let current = f.current_index();
                let (text, paint) = status(f, current);
                ui.label(RichText::new(text).color(paint));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .add(Button::new(icon::X).frame(false))
                        .on_hover_text("Close (Esc)")
                        .clicked()
                    {
                        close = true;
                    }
                });
            });
            if q.run.find.replace {
                ui.horizontal(|ui| {
                    let f = &mut q.run.find;
                    ui.add_space(toggle_width + ui.spacing().item_spacing.x);
                    let field = ui.add(
                        TextEdit::singleline(&mut f.replacement)
                            .id(replacement_id)
                            .hint_text("Replace")
                            .desired_width(260.0),
                    );
                    if field.lost_focus() {
                        let (enter, escape) =
                            ui.input(|i| (i.key_pressed(Key::Enter), i.key_pressed(Key::Escape)));
                        if enter {
                            one = true;
                            f.focus = Some(Field::Replacement);
                        } else if escape {
                            close = true;
                        }
                    }
                    if ui
                        .button(format!("{}  Replace", icon::SWAP))
                        .on_hover_text("Replace the selected match and find the next one")
                        .clicked()
                    {
                        one = true;
                    }
                    if ui
                        .button("Replace All")
                        .on_hover_text("Replace every match (one undo step)")
                        .clicked()
                    {
                        all = true;
                    }
                });
            }
        });

    // Typing finds the first match from where the search started.
    if (q.run.find.query.clone(), q.run.find.options.clone()) != before {
        q.run.find.notice = None;
        q.run.find.refresh(&q.sql);
        let origin = q.run.find.origin;
        let matches = &q.run.find.cache.matches;
        let found = matches
            .iter()
            .find(|m| m.chars.start >= origin)
            .or(matches.first())
            .cloned();
        match found {
            Some(m) => select(&ctx, editor_id, q, &m),
            None => q.run.find.current = None,
        }
    }
    if let Some(forward) = step_dir {
        step(&ctx, editor_id, q, forward);
    }
    if one {
        replace_one(&ctx, editor_id, q);
    }
    if all {
        replace_all(&ctx, editor_id, q);
    }
    if close {
        q.run.find.open = false;
        ctx.memory_mut(|m| m.request_focus(editor_id));
        if let Some(current) = q.run.find.current.take() {
            set_selection(&ctx, editor_id, Selection::new(current.start, current.end));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn search(text: &str, query: &str, options: FindOptions) -> Vec<Range<usize>> {
        let regex = compile(query, &options).unwrap().unwrap();
        find_matches(text, &regex, options.whole_word)
            .into_iter()
            .map(|m| m.chars)
            .collect()
    }

    #[test]
    #[allow(clippy::single_range_in_vec_init)]
    fn finds_text_with_case_word_and_regex_options() {
        let text = "SELECT id FROM t; select idx from t2;";
        assert_eq!(
            search(text, "select", FindOptions::default()),
            [0..6, 18..24]
        );
        let case = FindOptions {
            match_case: true,
            ..FindOptions::default()
        };
        assert_eq!(search(text, "select", case), [18..24]);
        let word = FindOptions {
            whole_word: true,
            ..FindOptions::default()
        };
        assert_eq!(search(text, "id", word.clone()), [7..9]);
        assert_eq!(search(text, "t", word), [15..16]);
        let regex = FindOptions {
            regex: true,
            ..FindOptions::default()
        };
        assert_eq!(search(text, r"t\d?;", regex.clone()), [15..17, 34..37]);
        // Special characters are literal without the regex option.
        assert_eq!(search("a.b axb", "a.b", FindOptions::default()), [0..3]);
        assert_eq!(search("a.b axb", "a.b", regex.clone()), [0..3, 4..7]);
        // Empty matches are skipped.
        assert!(search("abc", "x*", regex.clone()).is_empty());
        assert!(compile("(", &regex).is_err());
        assert!(compile("", &regex).unwrap().is_none());
    }

    #[test]
    fn match_offsets_count_characters() {
        let text = "naïve 'café' café";
        assert_eq!(
            search(text, "café", FindOptions::default()),
            [7..11, 13..17]
        );
        let regex = compile("café", &FindOptions::default()).unwrap().unwrap();
        let bytes: Vec<_> = find_matches(text, &regex, false)
            .into_iter()
            .map(|m| m.bytes)
            .collect();
        assert_eq!(&text[bytes[0].clone()], "café");
        assert_eq!(&text[bytes[1].clone()], "café");
    }

    #[test]
    fn highlights_split_sections_at_match_edges() {
        let font = egui::FontId::monospace(12.0);
        let mut job = LayoutJob::default();
        job.append(
            "select ",
            0.0,
            egui::TextFormat::simple(font.clone(), Color32::RED),
        );
        job.append("café·x", 0.0, egui::TextFormat::simple(font, Color32::BLUE));
        // Characters 5..9 ("t ca") and 12..13 ("x"), the second one current.
        let ranges = chars_to_bytes(&job.text, &[(5..9, false), (12..13, true), (40..41, false)]);
        assert_eq!(ranges.len(), 2);
        paint_ranges(&mut job, &ranges, Color32::GRAY, Color32::YELLOW);
        let pieces: Vec<(&str, Color32)> = job
            .sections
            .iter()
            .map(|s| {
                (
                    &job.text[s.byte_range.start.0..s.byte_range.end.0],
                    s.format.background,
                )
            })
            .collect();
        assert_eq!(
            pieces,
            [
                ("selec", Color32::TRANSPARENT),
                ("t ", Color32::GRAY),
                ("ca", Color32::GRAY),
                ("fé·", Color32::TRANSPARENT),
                ("x", Color32::YELLOW),
            ]
        );
    }

    #[test]
    fn regex_replacement_fills_in_groups() {
        let mut f = FindState {
            query: r"(\w+)_id".into(),
            replacement: "${1}Id".into(),
            options: FindOptions {
                regex: true,
                ..FindOptions::default()
            },
            ..FindState::default()
        };
        let text = "select user_id, order_id";
        f.refresh(text);
        let with: Vec<String> = f
            .cache
            .matches
            .iter()
            .map(|m| replacement_for(&f, text, m))
            .collect();
        assert_eq!(with, ["userId", "orderId"]);
        f.options.regex = false;
        f.query = "user_id".into();
        f.refresh(text);
        assert_eq!(replacement_for(&f, text, &f.cache.matches[0]), "${1}Id");
    }
}
