//! Small, Unicode-safe Vim command interpreter for the Mermaid source pane.
//!
//! Document history belongs to the application. This module edits the text area
//! and returns undo/redo requests without touching its private history.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui_textarea::{CursorMove, TextArea};

const MAX_COUNT: usize = 9_999;
const MAX_PASTE_BYTES: usize = 256 * 1024;
type Position = (usize, usize);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum Mode {
    #[default]
    Normal,
    Insert,
    Visual,
    VisualLine,
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum Action {
    None,
    Undo(usize),
    Redo(usize),
    Command(String),
}

#[derive(Clone, Copy)]
enum Operator {
    Delete,
    Change,
    Yank,
}

impl Operator {
    fn key(self) -> char {
        match self {
            Self::Delete => 'd',
            Self::Change => 'c',
            Self::Yank => 'y',
        }
    }
}

struct Prompt {
    prefix: char,
    text: String,
}

#[derive(Default)]
pub(super) struct Vim {
    mode: Mode,
    count: Option<usize>,
    operator: Option<(Operator, usize)>,
    pending_g: bool,
    replace: Option<usize>,
    input: Option<Prompt>,
    visual: Option<(Position, Position)>,
    register: String,
    linewise: bool,
    search: String,
    search_backwards: bool,
}

#[derive(Clone, Copy)]
enum Motion {
    Left,
    Down,
    Up,
    Right,
    Word,
    BackWord,
    EndWord,
    Head,
    First,
    End,
    Top,
    Bottom,
}

impl Vim {
    pub(super) fn mode(&self) -> Mode {
        self.mode
    }

    pub(super) fn mode_label(&self) -> &'static str {
        if let Some(input) = &self.input {
            return if input.prefix == ':' {
                "COMMAND"
            } else {
                "SEARCH"
            };
        }
        match self.mode {
            Mode::Normal => "NORMAL",
            Mode::Insert => "INSERT",
            Mode::Visual => "VISUAL",
            Mode::VisualLine => "VISUAL LINE",
        }
    }

    pub(super) fn is_prompt(&self) -> bool {
        self.input.is_some()
    }

    pub(super) fn prompt(&self) -> Option<String> {
        if let Some(input) = &self.input {
            return Some(format!("{}{}", input.prefix, input.text));
        }
        let mut pending = String::new();
        if let Some((op, count)) = self.operator {
            if count > 1 {
                pending.push_str(&count.to_string());
            }
            pending.push(op.key());
        }
        if let Some(count) = self.count {
            pending.push_str(&count.to_string());
        }
        if self.pending_g {
            pending.push('g');
        }
        if let Some(count) = self.replace {
            if count > 1 {
                pending.push_str(&count.to_string());
            }
            pending.push('r');
        }
        (!pending.is_empty()).then_some(pending)
    }

    pub(super) fn reset(&mut self, editor: &mut TextArea<'static>) {
        let position = self.visual.map_or(cursor(editor), |(_, position)| position);
        self.mode = Mode::Normal;
        self.clear_pending();
        self.input = None;
        self.visual = None;
        editor.cancel_selection();
        jump(editor, clamp(editor, position, false));
    }

    pub(super) fn clamp_cursor(&mut self, editor: &mut TextArea<'static>) {
        let cursor = clamp(editor, cursor(editor), self.mode == Mode::Insert);
        editor.cancel_selection();
        jump(editor, cursor);
        if let Some((anchor, _)) = self.visual {
            self.visual = Some((clamp(editor, anchor, false), cursor));
            self.show_selection(editor);
        }
    }

    fn clear_pending(&mut self) {
        self.count = None;
        self.operator = None;
        self.pending_g = false;
        self.replace = None;
    }

    pub(super) fn handle_paste(&mut self, text: &str, editor: &mut TextArea<'static>) {
        if let Some(input) = &mut self.input {
            input.text.extend(text.chars().filter(|c| !c.is_control()));
            return;
        }
        self.clear_pending();
        if self.visual.is_some() {
            self.visual_operator(Operator::Delete, editor);
        }
        editor.insert_str(text);
        if self.mode != Mode::Insert {
            if cursor(editor).1 > 0 {
                editor.move_cursor(CursorMove::Back);
            }
            self.clamp_cursor(editor);
        }
    }

    pub(super) fn handle_key(&mut self, key: KeyEvent, editor: &mut TextArea<'static>) -> Action {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        if key.code == KeyCode::Esc || (control && matches!(key.code, KeyCode::Char('[' | 'c'))) {
            if self.mode == Mode::Insert && cursor(editor).1 > 0 {
                editor.move_cursor(CursorMove::Back);
            }
            self.reset(editor);
            return Action::None;
        }
        if self.input.is_some() {
            return self.prompt_key(key, editor);
        }
        if control && key.code == KeyCode::Char('r') {
            let count = self.count.unwrap_or(1);
            self.reset(editor);
            return Action::Redo(count);
        }
        if self.mode == Mode::Insert {
            // Keep textarea's own undo/redo bindings out of the application history.
            if control && matches!(key.code, KeyCode::Char('z' | 'y')) {
                return if key.code == KeyCode::Char('z') {
                    Action::Undo(1)
                } else {
                    Action::Redo(1)
                };
            }
            editor.input(key);
            return Action::None;
        }
        if control || key.modifiers.contains(KeyModifiers::ALT) {
            self.clear_pending();
            return Action::None;
        }
        if let Some(count) = self.replace.take() {
            if let KeyCode::Char(ch) = key.code {
                let (row, col) = cursor(editor);
                let available = line_len(editor, row).saturating_sub(col);
                if available >= count {
                    select(editor, (row, col), (row, col + count));
                    editor.insert_str(ch.to_string().repeat(count));
                    jump(editor, (row, col + count - 1));
                }
            }
            self.clear_pending();
            return Action::None;
        }
        if let KeyCode::Char(digit @ '0'..='9') = key.code
            && (digit != '0' || self.count.is_some())
        {
            self.count = Some(
                (self.count.unwrap_or(0) * 10 + (digit as usize - '0' as usize)).min(MAX_COUNT),
            );
            return Action::None;
        }
        if self.pending_g {
            self.pending_g = false;
            if key.code == KeyCode::Char('g') {
                self.motion(Motion::Top, editor);
            } else {
                self.clear_pending();
            }
            return Action::None;
        }
        let motion = match key.code {
            KeyCode::Char('h') | KeyCode::Left => Some(Motion::Left),
            KeyCode::Char('j') | KeyCode::Down => Some(Motion::Down),
            KeyCode::Char('k') | KeyCode::Up => Some(Motion::Up),
            KeyCode::Char('l') | KeyCode::Right => Some(Motion::Right),
            KeyCode::Char('w') => Some(Motion::Word),
            KeyCode::Char('b') => Some(Motion::BackWord),
            KeyCode::Char('e') => Some(Motion::EndWord),
            KeyCode::Char('0') | KeyCode::Home => Some(Motion::Head),
            KeyCode::Char('^') => Some(Motion::First),
            KeyCode::Char('$') | KeyCode::End => Some(Motion::End),
            KeyCode::Char('G') => Some(Motion::Bottom),
            _ => None,
        };
        if let Some(motion) = motion {
            self.motion(motion, editor);
            return Action::None;
        }
        if key.code == KeyCode::Char('g') {
            self.pending_g = true;
            return Action::None;
        }
        let count = self.count.take().unwrap_or(1);
        if let KeyCode::Char(ch @ ('d' | 'c' | 'y')) = key.code {
            let op = match ch {
                'd' => Operator::Delete,
                'c' => Operator::Change,
                _ => Operator::Yank,
            };
            if self.visual.is_some() {
                self.visual_operator(op, editor);
            } else if let Some((previous, before)) = self.operator.take() {
                if previous.key() == ch {
                    let row = cursor(editor).0;
                    self.line_operator(
                        op,
                        row,
                        row.saturating_add(before.saturating_mul(count).min(MAX_COUNT) - 1),
                        editor,
                    );
                }
                self.clear_pending();
            } else {
                self.operator = Some((op, count));
            }
            return Action::None;
        }
        if self.operator.is_some() {
            self.clear_pending();
            return Action::None;
        }
        match key.code {
            KeyCode::Char('i' | 'a' | 'I' | 'A' | 'o' | 'O') if self.visual.is_none() => {
                self.enter_insert(key.code, editor);
            }
            KeyCode::Char('v' | 'V') => {
                let mode = if key.code == KeyCode::Char('v') {
                    Mode::Visual
                } else {
                    Mode::VisualLine
                };
                if self.mode == mode {
                    let cursor = self.visual.map_or(cursor(editor), |(_, cursor)| cursor);
                    self.reset(editor);
                    jump(editor, cursor);
                } else {
                    let cursor = self.visual.map_or(cursor(editor), |(_, cursor)| cursor);
                    self.visual = Some(self.visual.unwrap_or((cursor, cursor)));
                    self.mode = mode;
                    self.show_selection(editor);
                }
            }
            KeyCode::Char('x') | KeyCode::Delete => {
                if self.visual.is_some() {
                    self.visual_operator(Operator::Delete, editor);
                } else {
                    let (row, col) = cursor(editor);
                    self.char_operator(
                        Operator::Delete,
                        (row, col),
                        (row, (col + count).min(line_len(editor, row))),
                        editor,
                    );
                }
            }
            KeyCode::Char('s') => {
                if self.visual.is_some() {
                    self.visual_operator(Operator::Change, editor);
                } else {
                    let (row, col) = cursor(editor);
                    self.char_operator(
                        Operator::Change,
                        (row, col),
                        (row, (col + count).min(line_len(editor, row))),
                        editor,
                    );
                }
            }
            KeyCode::Char('D' | 'C') => {
                let start = self
                    .visual
                    .map_or(cursor(editor), |(anchor, cursor)| anchor.min(cursor));
                let end_row = (start.0 + count - 1).min(editor.lines().len() - 1);
                let op = if key.code == KeyCode::Char('D') {
                    Operator::Delete
                } else {
                    Operator::Change
                };
                self.char_operator(op, start, (end_row, line_len(editor, end_row)), editor);
            }
            KeyCode::Char('p' | 'P') => self.put(key.code == KeyCode::Char('p'), count, editor),
            KeyCode::Char('r') if self.visual.is_none() => self.replace = Some(count),
            KeyCode::Char('u') => {
                self.reset(editor);
                return Action::Undo(count);
            }
            KeyCode::Char(prefix @ (':' | '/' | '?')) => {
                self.reset(editor);
                self.input = Some(Prompt {
                    prefix,
                    text: String::new(),
                });
            }
            KeyCode::Char('n' | 'N') => {
                self.find(
                    self.search_backwards ^ (key.code == KeyCode::Char('N')),
                    count,
                    editor,
                );
            }
            _ => self.clear_pending(),
        }
        Action::None
    }

    fn prompt_key(&mut self, key: KeyEvent, editor: &mut TextArea<'static>) -> Action {
        let input = self.input.as_mut().expect("active prompt");
        match key.code {
            KeyCode::Enter => {
                let input = self.input.take().expect("active prompt");
                if input.prefix == ':' {
                    return Action::Command(input.text);
                }
                if !input.text.is_empty() {
                    self.search = input.text;
                    self.search_backwards = input.prefix == '?';
                }
                self.find(self.search_backwards, 1, editor);
            }
            KeyCode::Backspace => {
                input.text.pop();
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                input.text.clear()
            }
            KeyCode::Char(ch)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                input.text.push(ch)
            }
            _ => {}
        }
        Action::None
    }

    fn enter_insert(&mut self, key: KeyCode, editor: &mut TextArea<'static>) {
        let (row, col) = cursor(editor);
        match key {
            KeyCode::Char('a') => jump(editor, (row, (col + 1).min(line_len(editor, row)))),
            KeyCode::Char('I') => jump(editor, (row, first_nonblank(editor, row))),
            KeyCode::Char('A') => jump(editor, (row, line_len(editor, row))),
            KeyCode::Char('o') => {
                jump(editor, (row, line_len(editor, row)));
                editor.insert_newline();
            }
            KeyCode::Char('O') => {
                jump(editor, (row, 0));
                editor.insert_newline();
                jump(editor, (row, 0));
            }
            _ => {}
        }
        self.mode = Mode::Insert;
        self.clear_pending();
    }

    fn motion(&mut self, motion: Motion, editor: &mut TextArea<'static>) {
        let before = self.operator.map_or(1, |(_, count)| count);
        let explicit = self.count.is_some() || before > 1;
        let count = before
            .saturating_mul(self.count.take().unwrap_or(1))
            .min(MAX_COUNT);
        let start = self.visual.map_or(cursor(editor), |(_, cursor)| cursor);
        let mut target = destination(editor, start, motion, count, explicit);
        if let Some((op, _)) = self.operator.take() {
            if matches!(
                motion,
                Motion::Down | Motion::Up | Motion::Top | Motion::Bottom
            ) {
                self.line_operator(op, start.0.min(target.0), start.0.max(target.0), editor);
            } else {
                // Vim's cw changes the word without consuming the following space.
                if matches!((op, motion), (Operator::Change, Motion::Word))
                    && char_at(editor, start).is_some_and(|ch| !ch.is_whitespace())
                {
                    let chars: Vec<char> = editor.lines().join("\n").chars().collect();
                    let mut at = offset(editor, start);
                    let class = word_class(chars[at]);
                    while at + 1 < chars.len() && word_class(chars[at + 1]) == class {
                        at += 1;
                    }
                    target = position(editor, at);
                    if count > 1 {
                        target = destination(editor, target, Motion::EndWord, count - 1, true);
                    }
                    target.1 = (target.1 + 1).min(line_len(editor, target.0));
                } else if matches!(motion, Motion::EndWord | Motion::End) {
                    target.1 = (target.1 + 1).min(line_len(editor, target.0));
                } else if matches!(motion, Motion::Word) && target.0 > start.0 {
                    let last_start = if count > 1 {
                        destination(editor, start, Motion::Word, count - 1, true)
                    } else {
                        start
                    };
                    // A final w crossing a line boundary consumes the last word
                    // and its trailing spaces, but keeps that line's newline.
                    if target.0 > last_start.0 && line_len(editor, last_start.0) > 0 {
                        target = (last_start.0, line_len(editor, last_start.0));
                    }
                }
                self.char_operator(op, start.min(target), start.max(target), editor);
            }
        } else {
            target = clamp(editor, target, false);
            if let Some((anchor, _)) = self.visual {
                self.visual = Some((anchor, target));
                self.show_selection(editor);
            } else {
                jump(editor, target);
            }
        }
        self.clear_pending();
    }

    fn char_operator(
        &mut self,
        op: Operator,
        start: Position,
        end: Position,
        editor: &mut TextArea<'static>,
    ) {
        let original = cursor(editor);
        let chars: Vec<char> = editor.lines().join("\n").chars().collect();
        if start != end {
            self.register = chars[offset(editor, start)..offset(editor, end)]
                .iter()
                .collect();
            self.linewise = false;
        }
        self.visual = None;
        self.mode = if matches!(op, Operator::Change) {
            Mode::Insert
        } else {
            Mode::Normal
        };
        editor.cancel_selection();
        if matches!(op, Operator::Yank) {
            jump(editor, clamp(editor, original, false));
        } else {
            select(editor, start, end);
            editor.cut();
            jump(editor, clamp(editor, start, self.mode == Mode::Insert));
        }
    }

    fn line_operator(
        &mut self,
        op: Operator,
        first: usize,
        last: usize,
        editor: &mut TextArea<'static>,
    ) {
        let first = first.min(editor.lines().len() - 1);
        let last = last.min(editor.lines().len() - 1);
        let original = cursor(editor);
        self.register = editor.lines()[first..=last].join("\n") + "\n";
        self.linewise = true;
        self.visual = None;
        self.mode = if matches!(op, Operator::Change) {
            Mode::Insert
        } else {
            Mode::Normal
        };
        editor.cancel_selection();
        if matches!(op, Operator::Yank) {
            jump(editor, clamp(editor, original, false));
            return;
        }
        let (start, end) = if matches!(op, Operator::Change) {
            ((first, 0), (last, line_len(editor, last)))
        } else if last + 1 < editor.lines().len() {
            ((first, 0), (last + 1, 0))
        } else if first > 0 {
            (
                (first - 1, line_len(editor, first - 1)),
                (last, line_len(editor, last)),
            )
        } else {
            ((0, 0), (last, line_len(editor, last)))
        };
        select(editor, start, end);
        editor.cut();
        let row = first.min(editor.lines().len() - 1);
        jump(editor, (row, first_nonblank(editor, row)));
    }

    fn visual_operator(&mut self, op: Operator, editor: &mut TextArea<'static>) {
        let Some((anchor, cursor)) = self.visual else {
            return;
        };
        if self.mode == Mode::VisualLine {
            self.line_operator(op, anchor.0.min(cursor.0), anchor.0.max(cursor.0), editor);
        } else {
            let start = anchor.min(cursor);
            let mut end = anchor.max(cursor);
            end.1 = (end.1 + 1).min(line_len(editor, end.0));
            self.char_operator(op, start, end, editor);
            if matches!(op, Operator::Yank) {
                jump(editor, start);
            }
        }
        self.clear_pending();
    }

    fn show_selection(&self, editor: &mut TextArea<'static>) {
        let Some((anchor, cursor)) = self.visual else {
            return;
        };
        let (mut start, mut end) = (anchor.min(cursor), anchor.max(cursor));
        if self.mode == Mode::VisualLine {
            start.1 = 0;
            end = if end.0 + 1 < editor.lines().len() {
                (end.0 + 1, 0)
            } else {
                (end.0, line_len(editor, end.0))
            };
        } else {
            end.1 = (end.1 + 1).min(line_len(editor, end.0));
        }
        // Keep the active end on the correct side of a backwards selection.
        if cursor < anchor {
            select(editor, end, start);
        } else {
            select(editor, start, end);
        }
    }

    fn put(&mut self, after: bool, count: usize, editor: &mut TextArea<'static>) {
        if self.register.is_empty() {
            return;
        }
        let copies = count.min(MAX_PASTE_BYTES / self.register.len());
        if copies == 0 {
            return;
        }
        let text = self.register.repeat(copies);
        let linewise = self.linewise;
        if let Some((anchor, active)) = self.visual
            && self.mode == Mode::VisualLine
        {
            let first = anchor.0.min(active.0);
            let last = anchor.0.max(active.0);
            select(editor, (first, 0), (last, line_len(editor, last)));
            editor.insert_str(if linewise {
                text.strip_suffix('\n').unwrap_or(&text)
            } else {
                &text
            });
            self.visual = None;
            self.mode = Mode::Normal;
            jump(editor, (first, first_nonblank(editor, first)));
        } else if self.visual.is_some() {
            let old_register = self.register.clone();
            self.visual_operator(Operator::Delete, editor);
            self.register = old_register;
            self.linewise = linewise;
            editor.insert_str(if linewise {
                text.strip_suffix('\n').unwrap_or(&text)
            } else {
                &text
            });
        } else if linewise {
            let row = cursor(editor).0;
            let inserted_row = if after { row + 1 } else { row };
            if after && inserted_row == editor.lines().len() {
                jump(editor, (row, line_len(editor, row)));
                editor.insert_str(format!("\n{}", text.strip_suffix('\n').unwrap_or(&text)));
            } else {
                jump(editor, (inserted_row, 0));
                editor.insert_str(&text);
            }
            jump(editor, (inserted_row, first_nonblank(editor, inserted_row)));
        } else {
            let (row, col) = cursor(editor);
            jump(
                editor,
                (row, (col + usize::from(after)).min(line_len(editor, row))),
            );
            editor.insert_str(&text);
            if cursor(editor).1 > 0 {
                editor.move_cursor(CursorMove::Back);
            }
        }
        self.clamp_cursor(editor);
    }

    fn find(&mut self, backwards: bool, count: usize, editor: &mut TextArea<'static>) {
        if self.search.is_empty() {
            return;
        }
        let text = editor.lines().join("\n");
        let mut previous_byte = 0;
        let mut previous_char = 0;
        let matches: Vec<usize> = text
            .match_indices(&self.search)
            .map(|(byte, _)| {
                previous_char += text[previous_byte..byte].chars().count();
                previous_byte = byte;
                previous_char
            })
            .collect();
        if matches.is_empty() {
            return;
        }
        let current = offset(editor, cursor(editor));
        let extra = (count - 1) % matches.len();
        let index = if backwards {
            let first = (matches.partition_point(|&found| found < current) + matches.len() - 1)
                % matches.len();
            (first + matches.len() - extra) % matches.len()
        } else {
            (matches.partition_point(|&found| found <= current) + extra) % matches.len()
        };
        let target = position(editor, matches[index]);
        if let Some((anchor, _)) = self.visual {
            self.visual = Some((anchor, target));
            self.show_selection(editor);
        } else {
            jump(editor, target);
        }
    }
}

fn cursor(editor: &TextArea<'_>) -> Position {
    let position = editor.cursor();
    (position.0, position.1)
}

fn line_len(editor: &TextArea<'_>, row: usize) -> usize {
    editor.lines()[row].chars().count()
}

fn first_nonblank(editor: &TextArea<'_>, row: usize) -> usize {
    editor.lines()[row]
        .chars()
        .position(|ch| !ch.is_whitespace())
        .unwrap_or(0)
}

fn char_at(editor: &TextArea<'_>, (row, col): Position) -> Option<char> {
    editor.lines()[row].chars().nth(col)
}

fn clamp(editor: &TextArea<'_>, (row, col): Position, insertion: bool) -> Position {
    let row = row.min(editor.lines().len() - 1);
    let len = line_len(editor, row);
    (
        row,
        col.min(if insertion {
            len
        } else {
            len.saturating_sub(1)
        }),
    )
}

fn jump(editor: &mut TextArea<'_>, (row, col): Position) {
    // Jump's API uses u16, while textarea character coordinates use usize.
    let limit = usize::from(u16::MAX);
    if row <= limit && col <= limit {
        editor.move_cursor(CursorMove::Jump(row as u16, col as u16));
        return;
    }
    if cursor(editor).0 != row {
        if row <= limit {
            editor.move_cursor(CursorMove::Jump(row as u16, 0));
        } else {
            editor.move_cursor(CursorMove::Bottom);
            for _ in row..editor.lines().len() - 1 {
                editor.move_cursor(CursorMove::Up);
            }
        }
    }
    let len = line_len(editor, row);
    let col = col.min(len);
    let distance = cursor(editor).1.abs_diff(col);
    if len.saturating_sub(col) < distance {
        if cursor(editor).1 != len {
            editor.move_cursor(CursorMove::End);
        }
    } else if row <= limit && col.saturating_sub(limit) < distance {
        editor.move_cursor(CursorMove::Jump(row as u16, col.min(limit) as u16));
    }
    let current = cursor(editor).1;
    if current < col {
        for _ in current..col {
            editor.move_cursor(CursorMove::Forward);
        }
    } else {
        for _ in col..current {
            editor.move_cursor(CursorMove::Back);
        }
    }
}

fn select(editor: &mut TextArea<'_>, start: Position, end: Position) {
    editor.cancel_selection();
    jump(editor, start);
    editor.start_selection();
    jump(editor, end);
}

fn offset(editor: &TextArea<'_>, (row, col): Position) -> usize {
    editor.lines()[..row]
        .iter()
        .map(|line| line.chars().count() + 1)
        .sum::<usize>()
        + col
}

fn position(editor: &TextArea<'_>, mut offset: usize) -> Position {
    for (row, line) in editor.lines().iter().enumerate() {
        let len = line.chars().count();
        if offset <= len {
            return (row, offset);
        }
        offset -= len + 1;
    }
    let row = editor.lines().len() - 1;
    (row, line_len(editor, row))
}

fn destination(
    editor: &TextArea<'_>,
    start: Position,
    motion: Motion,
    count: usize,
    explicit: bool,
) -> Position {
    let (row, col) = start;
    let last = editor.lines().len() - 1;
    match motion {
        Motion::Left => (row, col.saturating_sub(count)),
        Motion::Right => (row, (col + count).min(line_len(editor, row))),
        Motion::Down => clamp(editor, ((row + count).min(last), col), false),
        Motion::Up => clamp(editor, (row.saturating_sub(count), col), false),
        Motion::Head => (row, 0),
        Motion::First => (row, first_nonblank(editor, row)),
        Motion::End => {
            let row = (row + count - 1).min(last);
            (row, line_len(editor, row).saturating_sub(1))
        }
        Motion::Top | Motion::Bottom => {
            let row = if explicit {
                (count - 1).min(last)
            } else if matches!(motion, Motion::Top) {
                0
            } else {
                last
            };
            (row, first_nonblank(editor, row))
        }
        Motion::Word | Motion::BackWord | Motion::EndWord => {
            let chars: Vec<char> = editor.lines().join("\n").chars().collect();
            let mut at = offset(editor, start).min(chars.len());
            for _ in 0..count {
                match motion {
                    Motion::Word => {
                        if at < chars.len() {
                            let class = word_class(chars[at]);
                            while at < chars.len() && word_class(chars[at]) == class {
                                at += 1;
                            }
                            while at < chars.len() && chars[at].is_whitespace() {
                                at += 1;
                            }
                        }
                    }
                    Motion::BackWord => {
                        at = at.saturating_sub(1);
                        while at > 0 && chars[at].is_whitespace() {
                            at -= 1;
                        }
                        if !chars.is_empty() {
                            let class = word_class(chars[at]);
                            while at > 0 && word_class(chars[at - 1]) == class {
                                at -= 1;
                            }
                        }
                    }
                    Motion::EndWord => {
                        if at + 1 < chars.len() {
                            at += 1;
                            while at < chars.len() && chars[at].is_whitespace() {
                                at += 1;
                            }
                            if at < chars.len() {
                                let class = word_class(chars[at]);
                                while at + 1 < chars.len() && word_class(chars[at + 1]) == class {
                                    at += 1;
                                }
                            }
                        }
                    }
                    _ => unreachable!(),
                }
            }
            position(editor, at)
        }
    }
}

fn word_class(ch: char) -> u8 {
    if ch.is_whitespace() {
        0
    } else if ch.is_alphanumeric() || ch == '_' {
        1
    } else {
        2
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editor(text: &str) -> TextArea<'static> {
        let mut editor = TextArea::new(text.split('\n').map(str::to_owned).collect());
        editor.set_max_histories(0);
        editor
    }

    fn keys(vim: &mut Vim, editor: &mut TextArea<'static>, keys: &str) {
        for ch in keys.chars() {
            vim.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE), editor);
        }
    }

    fn special(vim: &mut Vim, editor: &mut TextArea<'static>, key: KeyCode) -> Action {
        vim.handle_key(KeyEvent::new(key, KeyModifiers::NONE), editor)
    }

    #[test]
    fn normal_commands_never_insert_and_counts_are_bounded() {
        let mut vim = Vim::default();
        let mut editor = editor("abc\ndef\nghi\njkl");
        keys(&mut vim, &mut editor, "3j99l");
        assert_eq!(cursor(&editor), (3, 2));
        keys(&mut vim, &mut editor, "9999999999999999999999999k");
        assert_eq!(cursor(&editor), (0, 2));
        assert_eq!(editor.lines()[0], "abc");
    }

    #[test]
    fn words_handle_mermaid_punctuation_and_unicode() {
        let mut vim = Vim::default();
        let mut editor = editor("猫猫 --> node_name[你好]");
        keys(&mut vim, &mut editor, "w");
        assert_eq!(cursor(&editor), (0, 3));
        keys(&mut vim, &mut editor, "w");
        assert_eq!(cursor(&editor), (0, 7));
        keys(&mut vim, &mut editor, "e");
        assert_eq!(cursor(&editor), (0, 15));
        keys(&mut vim, &mut editor, "b");
        assert_eq!(cursor(&editor), (0, 7));
        keys(&mut vim, &mut editor, "0x");
        assert_eq!(editor.lines()[0], "猫 --> node_name[你好]");
    }

    #[test]
    fn multiplied_counts_delete_words_and_lines() {
        let mut vim = Vim::default();
        let mut editor = editor("one two three four five\nnext\nlast");
        keys(&mut vim, &mut editor, "2d2w");
        assert_eq!(editor.lines()[0], "five");
        keys(&mut vim, &mut editor, "2dd");
        assert_eq!(editor.lines(), ["last"]);
        keys(&mut vim, &mut editor, "P");
        assert_eq!(editor.lines(), ["five", "next", "last"]);
    }

    #[test]
    fn linewise_registers_work_at_both_document_boundaries() {
        let mut vim = Vim::default();
        let mut editor = editor("first\nlast");
        keys(&mut vim, &mut editor, "Gdd");
        assert_eq!(editor.lines(), ["first"]);
        keys(&mut vim, &mut editor, "p");
        assert_eq!(editor.lines(), ["first", "last"]);
        keys(&mut vim, &mut editor, "ggyy2P");
        assert_eq!(editor.lines(), ["first", "first", "first", "last"]);
        keys(&mut vim, &mut editor, "gg4dd");
        assert_eq!(editor.lines(), [""]);
    }

    #[test]
    fn change_and_insert_commands_preserve_expected_ranges() {
        let mut vim = Vim::default();
        let mut editor = editor("hello world\nsecond");
        keys(&mut vim, &mut editor, "cwbye");
        assert_eq!(editor.lines()[0], "bye world");
        assert_eq!(vim.mode(), Mode::Insert);
        special(&mut vim, &mut editor, KeyCode::Esc);
        keys(&mut vim, &mut editor, "Gccchanged");
        assert_eq!(editor.lines(), ["bye world", "changed"]);
        special(&mut vim, &mut editor, KeyCode::Esc);
        keys(&mut vim, &mut editor, "Oabove");
        assert_eq!(editor.lines(), ["bye world", "above", "changed"]);
    }

    #[test]
    fn visual_ranges_are_inclusive_in_both_directions() {
        let mut vim = Vim::default();
        let mut editor = editor("αβγδε");
        keys(&mut vim, &mut editor, "v2ld");
        assert_eq!(editor.lines(), ["δε"]);
        keys(&mut vim, &mut editor, "P");
        assert_eq!(editor.lines(), ["αβγδε"]);
        keys(&mut vim, &mut editor, "$v2hd");
        assert_eq!(editor.lines(), ["αβ"]);
    }

    #[test]
    fn visual_line_yank_and_change_cover_entire_lines() {
        let mut vim = Vim::default();
        let mut editor = editor("one\ntwo\nthree");
        keys(&mut vim, &mut editor, "VjyGp");
        assert_eq!(editor.lines(), ["one", "two", "three", "one", "two"]);
        keys(&mut vim, &mut editor, "ggVjcnew");
        assert_eq!(editor.lines(), ["new", "three", "one", "two"]);
    }

    #[test]
    fn replace_and_short_deletes_do_not_cross_lines() {
        let mut vim = Vim::default();
        let mut editor = editor("你好ab\nnext");
        keys(&mut vim, &mut editor, "2r界");
        assert_eq!(editor.lines(), ["界界ab", "next"]);
        keys(&mut vim, &mut editor, "99x");
        assert_eq!(editor.lines(), ["界", "next"]);
        keys(&mut vim, &mut editor, "D");
        assert_eq!(editor.lines(), ["", "next"]);
    }

    #[test]
    fn prompt_commands_and_literal_search_do_not_modify_source() {
        let mut vim = Vim::default();
        let mut editor = editor("猫 [x] β [x]");
        keys(&mut vim, &mut editor, "/[x]");
        assert_eq!(vim.prompt().as_deref(), Some("/[x]"));
        special(&mut vim, &mut editor, KeyCode::Enter);
        assert_eq!(cursor(&editor), (0, 2));
        keys(&mut vim, &mut editor, "n");
        assert_eq!(cursor(&editor), (0, 8));
        keys(&mut vim, &mut editor, "N");
        assert_eq!(cursor(&editor), (0, 2));
        keys(&mut vim, &mut editor, ":w path with spaces.mmd");
        assert_eq!(
            special(&mut vim, &mut editor, KeyCode::Enter),
            Action::Command("w path with spaces.mmd".into())
        );
        assert_eq!(editor.lines(), ["猫 [x] β [x]"]);
    }

    #[test]
    fn undo_redo_are_application_actions_and_reset_retains_register() {
        let mut vim = Vim::default();
        let mut editor = editor("hello");
        keys(&mut vim, &mut editor, "yy2");
        assert_eq!(
            special(&mut vim, &mut editor, KeyCode::Char('u')),
            Action::Undo(2)
        );
        assert_eq!(
            vim.handle_key(
                KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL),
                &mut editor
            ),
            Action::Redo(1)
        );
        vim.reset(&mut editor);
        keys(&mut vim, &mut editor, "p");
        assert_eq!(editor.lines(), ["hello", "hello"]);
    }

    #[test]
    fn paste_goes_to_active_prompt_or_replaces_visual_selection() {
        let mut vim = Vim::default();
        let mut editor = editor("hello");
        keys(&mut vim, &mut editor, ":");
        vim.handle_paste("w some path.mmd\n", &mut editor);
        assert_eq!(vim.prompt().as_deref(), Some(":w some path.mmd"));
        special(&mut vim, &mut editor, KeyCode::Esc);
        keys(&mut vim, &mut editor, "v3l");
        vim.handle_paste("界", &mut editor);
        assert_eq!(editor.lines(), ["界o"]);
        assert_eq!(vim.mode(), Mode::Normal);
    }

    #[test]
    fn empty_documents_and_blank_lines_are_safe() {
        for command in [
            "b", "w", "e", "dd", "cc", "x", "D", "vdy", "G", "gg", "0", "$",
        ] {
            let mut vim = Vim::default();
            let mut editor = editor("");
            keys(&mut vim, &mut editor, command);
            assert_eq!(cursor(&editor), (0, 0), "{command}");
        }
    }

    #[test]
    fn last_word_deletion_keeps_newlines_and_change_keeps_spaces() {
        let mut vim = Vim::default();
        let mut editor = editor("one x\nnext line");
        keys(&mut vim, &mut editor, "wcwy");
        assert_eq!(editor.lines(), ["one y", "next line"]);
        special(&mut vim, &mut editor, KeyCode::Esc);
        keys(&mut vim, &mut editor, "dw");
        assert_eq!(editor.lines(), ["one ", "next line"]);
        keys(&mut vim, &mut editor, "gg02dw");
        assert_eq!(editor.lines(), ["line"]);
    }

    #[test]
    fn insert_escape_returns_to_previous_character_and_visual_line_put_replaces() {
        let mut vim = Vim::default();
        let mut editor = editor("one\ntwo\nthree");
        keys(&mut vim, &mut editor, "lix");
        special(&mut vim, &mut editor, KeyCode::Esc);
        assert_eq!(cursor(&editor), (0, 1));
        keys(&mut vim, &mut editor, "yyjVp");
        assert_eq!(editor.lines(), ["oxne", "oxne", "three"]);
    }

    #[test]
    fn long_line_end_is_reachable_without_truncating_cursor_coordinates() {
        let mut vim = Vim::default();
        let mut editor = editor(&"界".repeat(70_000));
        keys(&mut vim, &mut editor, "$");
        assert_eq!(cursor(&editor), (0, 69_999));
        keys(&mut vim, &mut editor, "x");
        assert_eq!(line_len(&editor, 0), 69_999);
    }
}
