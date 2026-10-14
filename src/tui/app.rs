//! Terminal editor commands and loss-aware, asynchronous source synchronization.

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind};
use ratatui::layout::Rect;
use ratatui_textarea::{CursorMove, TextArea};

use super::{
    graph,
    vim::{Action as VimAction, Mode, Vim},
};
use crate::document::{Document, MAX_FILE_BYTES};
use crate::flow::{Edge, EdgeKind, Flow, Shape};
use crate::render::{Rendered, Renderer};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Focus {
    Source,
    Graph,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum PendingAction {
    New,
    Open,
    OpenPath(PathBuf),
    Quit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WriteKind {
    Source,
    Svg,
}

#[derive(Clone, Debug)]
pub(super) enum PromptKind {
    Save { after: Option<PendingAction> },
    Open,
    Export,
    Rename { node: String },
    EdgeLabel { index: usize },
}

pub(super) enum Modal {
    Help,
    Prompt {
        title: String,
        input: Box<TextArea<'static>>,
        kind: PromptKind,
    },
    Unsaved {
        action: PendingAction,
    },
    Overwrite {
        path: PathBuf,
        kind: WriteKind,
        after: Option<PendingAction>,
    },
    Edges {
        indices: Vec<usize>,
        selected: usize,
    },
}

struct RenderRequest {
    revision: u64,
    source: String,
}
struct RenderReply {
    revision: u64,
    source: String,
    result: Result<Rendered, String>,
}
struct Worker {
    requests: Sender<RenderRequest>,
    replies: Receiver<RenderReply>,
}

pub(super) struct App {
    pub document: Document,
    pub editor: TextArea<'static>,
    pub vim: Vim,
    pub focus: Focus,
    pub source_visible: bool,
    pub rendered: Option<Rendered>,
    pub rendered_flow: Option<Flow>,
    pub rendered_revision: u64,
    pub revision: u64,
    pub flow: Result<Flow, String>,
    pub selected: Option<String>,
    pub connect_from: Option<String>,
    pub pan: (f64, f64),
    pub zoom: f64,
    pub fit: bool,
    pub status: String,
    pub error: Option<String>,
    pub rendering: bool,
    pub modal: Option<Modal>,
    pub should_quit: bool,
    pub graph_area: Rect,
    pub source_area: Rect,
    worker: Option<Worker>,
    insert_group: bool,
}

impl App {
    pub fn new(document: Document) -> Self {
        let flow = Flow::parse(&document.source);
        let selected = flow
            .as_ref()
            .ok()
            .and_then(|flow| flow.nodes.first())
            .map(|node| node.id.clone());
        let editor = textarea(&document.source);
        Self {
            document,
            editor,
            vim: Vim::default(),
            flow,
            selected,
            focus: Focus::Source,
            source_visible: true,
            rendered: None,
            rendered_flow: None,
            rendered_revision: 0,
            revision: 1,
            connect_from: None,
            pan: (0.0, 0.0),
            zoom: 1.0,
            fit: true,
            status: "Tab switches panels · F1 shows all shortcuts".into(),
            error: None,
            rendering: false,
            modal: None,
            should_quit: false,
            graph_area: Rect::default(),
            source_area: Rect::default(),
            worker: None,
            insert_group: false,
        }
    }

    pub fn start_worker(&mut self) {
        let (requests, incoming) = mpsc::channel::<RenderRequest>();
        let (outgoing, replies) = mpsc::channel::<RenderReply>();
        let spawn = thread::Builder::new()
            .name("fin-tui-renderer".into())
            .spawn(move || {
                let mut renderer = Renderer::new();
                while let Ok(mut request) = incoming.recv() {
                    // Wait until typing pauses and replace pending jobs with the latest source.
                    loop {
                        match incoming.recv_timeout(Duration::from_millis(90)) {
                            Ok(latest) => request = latest,
                            Err(mpsc::RecvTimeoutError::Timeout) => break,
                            Err(mpsc::RecvTimeoutError::Disconnected) => return,
                        }
                    }
                    let result = renderer.render(&request.source);
                    if outgoing
                        .send(RenderReply {
                            revision: request.revision,
                            source: request.source,
                            result,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            });
        if let Err(error) = spawn {
            self.error = Some(format!("Could not start the rendering worker: {error}"));
            return;
        }
        self.worker = Some(Worker { requests, replies });
        self.schedule_render();
    }

    pub fn render_sync(&mut self) {
        let result = Renderer::new().render(&self.document.source);
        self.accept_reply(RenderReply {
            revision: self.revision,
            source: self.document.source.clone(),
            result,
        });
    }

    pub fn poll_render(&mut self) {
        let replies: Vec<_> = self
            .worker
            .as_ref()
            .map(|worker| worker.replies.try_iter().collect())
            .unwrap_or_default();
        for reply in replies {
            self.accept_reply(reply);
        }
    }

    fn accept_reply(&mut self, reply: RenderReply) {
        if reply.revision != self.revision {
            return;
        }
        self.rendering = false;
        match reply.result {
            Ok(rendered) => {
                self.rendered_flow = Flow::parse(&reply.source).ok();
                self.rendered = Some(rendered);
                self.rendered_revision = reply.revision;
                self.error = None;
            }
            Err(error) => {
                self.error = Some(error);
                self.status.clear();
            }
        }
    }

    fn schedule_render(&mut self) {
        if let Some(worker) = &self.worker {
            self.rendering = true;
            if worker
                .requests
                .send(RenderRequest {
                    revision: self.revision,
                    source: self.document.source.clone(),
                })
                .is_err()
            {
                self.rendering = false;
                self.error =
                    Some("The rendering worker stopped. Save your source and restart Fin.".into());
            }
        }
    }

    fn source_changed(&mut self, replace_editor: bool) {
        self.revision += 1;
        self.status.clear();
        self.flow = Flow::parse(&self.document.source);
        self.connect_from = None;
        if let Ok(flow) = &self.flow
            && !self
                .selected
                .as_ref()
                .is_some_and(|id| flow.node(id).is_some())
        {
            self.selected = flow.nodes.first().map(|node| node.id.clone());
        }
        if replace_editor {
            let cursor = self.editor.cursor();
            self.editor = textarea(&self.document.source);
            self.editor.move_cursor(CursorMove::Jump(
                cursor.0.min(u16::MAX as usize) as u16,
                cursor.1.min(u16::MAX as usize) as u16,
            ));
            self.vim.reset(&mut self.editor);
            self.insert_group = false;
        }
        self.schedule_render();
    }

    fn edit_source(&mut self, source: String, replace_editor: bool) {
        self.insert_group = false;
        self.record_source(source, replace_editor, false);
    }

    fn record_source(
        &mut self,
        source: String,
        replace_editor: bool,
        continue_group: bool,
    ) -> bool {
        if source == self.document.source {
            return false;
        }
        if source.len() > MAX_FILE_BYTES {
            self.editor = textarea(&self.document.source);
            self.vim.reset(&mut self.editor);
            self.insert_group = false;
            self.status = "Source is limited to 256 KiB; this edit was not applied.".into();
            return false;
        }
        self.document.edit_grouped(source, continue_group);
        self.source_changed(replace_editor);
        true
    }

    fn sync_vim_source(&mut self) {
        let continued = self.insert_group;
        let changed = self.record_source(self.editor.lines().join("\n"), false, continued);
        self.insert_group = (continued || changed) && self.vim.mode() == Mode::Insert;
    }

    fn leave_source(&mut self) {
        self.insert_group = false;
        self.vim.reset(&mut self.editor);
    }

    fn history(&mut self, redo: bool, count: usize) {
        self.leave_source();
        let mut changed = false;
        for _ in 0..count {
            let applied = if redo {
                self.document.redo()
            } else {
                self.document.undo()
            };
            if !applied {
                break;
            }
            changed = true;
        }
        if changed {
            self.source_changed(true);
            self.status = if redo { "Redone" } else { "Undone" }.into();
        }
    }

    fn source_key(&mut self, key: KeyEvent) {
        match self.vim.handle_key(key, &mut self.editor) {
            VimAction::None => self.sync_vim_source(),
            VimAction::Undo(count) => self.history(false, count),
            VimAction::Redo(count) => self.history(true, count),
            VimAction::Command(command) => {
                self.insert_group = false;
                self.ex_command(&command);
            }
        }
    }

    fn ex_command(&mut self, command: &str) {
        let command = command.trim();
        let (name, argument) = command
            .find(char::is_whitespace)
            .map(|index| (&command[..index], command[index..].trim()))
            .unwrap_or((command, ""));
        match name {
            "w" | "write" | "wq" => {
                let after = (name == "wq").then_some(PendingAction::Quit);
                if argument.is_empty() {
                    self.save(after);
                } else {
                    self.confirm_write(PathBuf::from(argument), WriteKind::Source, after);
                }
            }
            "x" if argument.is_empty() => {
                if self.document.is_dirty() {
                    self.save(Some(PendingAction::Quit));
                } else {
                    self.should_quit = true;
                }
            }
            "q" | "quit" if argument.is_empty() => self.request_action(PendingAction::Quit),
            "q!" | "quit!" if argument.is_empty() => self.should_quit = true,
            "e" | "edit" => {
                let path = if argument.is_empty() {
                    self.document.path.clone()
                } else {
                    Some(PathBuf::from(argument))
                };
                self.request_action(path.map_or(PendingAction::Open, PendingAction::OpenPath));
            }
            "new" | "enew" if argument.is_empty() => self.request_action(PendingAction::New),
            "h" | "help" if argument.is_empty() => self.modal = Some(Modal::Help),
            "" => {}
            _ => {
                self.status = format!(
                    "Unknown command: {command}. Use :w, :q, :q!, :wq, :x, :e <path>, or :help."
                );
            }
        }
    }

    pub fn handle_event(&mut self, event: Event) {
        match event {
            Event::Key(key) if key.kind != KeyEventKind::Release => self.handle_key(key),
            Event::Paste(text) => {
                if let Some(Modal::Prompt { input, .. }) = &mut self.modal {
                    input.insert_str(text.replace(['\r', '\n'], ""));
                } else if self.modal.is_none() && self.focus == Focus::Source {
                    let text = text.replace("\r\n", "\n").replace('\r', "\n");
                    self.vim.handle_paste(&text, &mut self.editor);
                    self.sync_vim_source();
                }
            }
            Event::Mouse(mouse) if self.modal.is_none() => {
                let on_graph = self.graph_area.contains((mouse.column, mouse.row).into());
                if on_graph {
                    match mouse.kind {
                        MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
                            self.leave_source();
                            self.focus = Focus::Graph;
                            if self.rendered_revision == self.revision
                                && let Some(rendered) = &self.rendered
                            {
                                self.selected = graph::hit_node(
                                    rendered,
                                    self.graph_area,
                                    self.pan,
                                    self.zoom,
                                    mouse.column,
                                    mouse.row,
                                );
                            }
                        }
                        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                            let amount = if mouse.kind == MouseEventKind::ScrollUp {
                                -3.0
                            } else {
                                3.0
                            };
                            self.fit = false;
                            if mouse.modifiers.contains(KeyModifiers::CONTROL) {
                                self.change_zoom(if amount < 0.0 { 1.2 } else { 1.0 / 1.2 });
                            } else {
                                graph::pan_by_cells(&mut self.pan, self.zoom, 0.0, amount);
                            }
                        }
                        _ => {}
                    }
                } else if self.source_visible
                    && self.source_area.contains((mouse.column, mouse.row).into())
                {
                    match mouse.kind {
                        MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
                            self.insert_group = false;
                            if self.vim.mode() != Mode::Insert {
                                self.vim.reset(&mut self.editor);
                            }
                            self.focus = Focus::Source;
                            let offset = self.editor.scroll_offset();
                            let row =
                                usize::from(mouse.row - self.source_area.y) + usize::from(offset.0);
                            let col = usize::from(
                                (mouse.column - self.source_area.x)
                                    .saturating_sub(self.editor.line_number_width()),
                            ) + usize::from(offset.1);
                            let clicked = self.editor.screen_to_data(row, col);
                            self.editor.move_cursor(CursorMove::Jump(
                                clicked.0.min(u16::MAX as usize) as u16,
                                clicked.1.min(u16::MAX as usize) as u16,
                            ));
                            self.vim.clamp_cursor(&mut self.editor);
                        }
                        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                            self.editor.input(mouse);
                        }
                        _ => {}
                    }
                }
            }
            Event::Resize(_, _) => {}
            _ => {}
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        if self.modal.is_some() {
            self.handle_modal(key);
            return;
        }
        if self.focus == Focus::Source && self.vim.is_prompt() {
            self.source_key(key);
            return;
        }
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        if control {
            if self.focus == Focus::Source && matches!(key.code, KeyCode::Char('r' | 'c' | '[')) {
                self.source_key(key);
                return;
            }
            self.insert_group = false;
            if self.focus == Focus::Source && self.vim.mode() != Mode::Insert {
                self.vim.reset(&mut self.editor);
            }
            match key.code {
                KeyCode::Char('q' | 'c') => self.request_action(PendingAction::Quit),
                KeyCode::Char('n') => self.request_action(PendingAction::New),
                KeyCode::Char('o') => self.request_action(PendingAction::Open),
                KeyCode::Char('s' | 'S') => {
                    if key.modifiers.contains(KeyModifiers::SHIFT) || key.code == KeyCode::Char('S')
                    {
                        self.save_prompt(None);
                    } else {
                        self.save(None);
                    }
                }
                KeyCode::Char('e') => self.export_prompt(),
                KeyCode::Char('b') => {
                    self.leave_source();
                    self.source_visible = !self.source_visible;
                    self.focus = if self.source_visible {
                        Focus::Source
                    } else {
                        Focus::Graph
                    };
                }
                KeyCode::Char('z' | 'Z') => {
                    self.history(
                        key.modifiers.contains(KeyModifiers::SHIFT)
                            || key.code == KeyCode::Char('Z'),
                        1,
                    );
                }
                KeyCode::Char('y') => self.history(true, 1),
                _ => {
                    if self.focus == Focus::Source {
                        self.source_key(key);
                    }
                }
            }
            return;
        }
        match key.code {
            KeyCode::F(1) => {
                self.insert_group = false;
                if self.vim.mode() != Mode::Insert {
                    self.vim.reset(&mut self.editor);
                }
                self.modal = Some(Modal::Help);
            }
            KeyCode::F(4) => {
                self.insert_group = false;
                if self.vim.mode() != Mode::Insert {
                    self.vim.reset(&mut self.editor);
                }
                self.save_prompt(None);
            }
            KeyCode::Tab | KeyCode::BackTab => {
                self.leave_source();
                self.focus = if self.source_visible && self.focus == Focus::Graph {
                    Focus::Source
                } else {
                    Focus::Graph
                };
            }
            _ if self.focus == Focus::Source => self.source_key(key),
            _ => self.graph_key(key),
        }
    }

    fn graph_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('a') => self.add_node(Shape::Rectangle),
            KeyCode::Char('d') => self.add_node(Shape::Diamond),
            KeyCode::Char('j' | ']') => self.cycle_node(1),
            KeyCode::Char('k' | '[') => self.cycle_node(-1),
            KeyCode::Char('r') => {
                if let Some(node) = self
                    .flow
                    .as_ref()
                    .ok()
                    .and_then(|flow| self.selected.as_ref().and_then(|id| flow.node(id)))
                {
                    self.prompt(
                        "Rename node",
                        &node.label.clone(),
                        PromptKind::Rename {
                            node: node.id.clone(),
                        },
                    );
                } else {
                    self.visual_unavailable();
                }
            }
            KeyCode::Char('s') => {
                let selected = self.selected.clone();
                self.edit_flow(move |flow| {
                    if let Some(node) = selected.and_then(|id| flow.node_mut(&id)) {
                        node.shape = Shape::from_index((node.shape.index() + 1) % 5);
                    }
                });
            }
            KeyCode::Delete | KeyCode::Backspace => {
                let selected = self.selected.clone();
                self.edit_flow(move |flow| {
                    if let Some(id) = selected {
                        flow.remove_node(&id);
                    }
                });
            }
            KeyCode::Char('c') => {
                if self.flow.is_ok() && self.selected.is_some() {
                    self.connect_from = self.selected.clone();
                    self.status =
                        "Connect: choose a destination with j/k or click, then Enter; Esc cancels"
                            .into();
                } else {
                    self.visual_unavailable();
                }
            }
            KeyCode::Enter => self.finish_connection(),
            KeyCode::Esc => {
                self.connect_from = None;
                self.status = "Connection cancelled".into();
            }
            KeyCode::Char('e') => self.edge_picker(),
            KeyCode::Left => self.pan_by(-4.0, 0.0),
            KeyCode::Right => self.pan_by(4.0, 0.0),
            KeyCode::Up => self.pan_by(0.0, -2.0),
            KeyCode::Down => self.pan_by(0.0, 2.0),
            KeyCode::Char('+' | '=') => self.change_zoom(1.2),
            KeyCode::Char('-' | '_') => self.change_zoom(1.0 / 1.2),
            KeyCode::Char('0') => self.fit = true,
            _ => {}
        }
    }

    fn pan_by(&mut self, x: f64, y: f64) {
        self.fit = false;
        graph::pan_by_cells(&mut self.pan, self.zoom, x, y);
    }
    fn change_zoom(&mut self, factor: f64) {
        self.fit = false;
        graph::zoom_at(&mut self.pan, &mut self.zoom, self.graph_area, factor);
    }

    fn visual_unavailable(&mut self) {
        self.status = self
            .flow
            .as_ref()
            .err()
            .cloned()
            .unwrap_or_else(|| "Select a node first (j/k), or add a step with a.".into());
    }

    fn edit_flow(&mut self, edit: impl FnOnce(&mut Flow)) {
        let Ok(mut flow) = self.flow.clone() else {
            self.visual_unavailable();
            return;
        };
        edit(&mut flow);
        self.edit_source(flow.to_mermaid(), true);
    }

    fn add_node(&mut self, shape: Shape) {
        let Ok(mut flow) = self.flow.clone() else {
            self.visual_unavailable();
            return;
        };
        let id = flow.add_node(shape);
        self.edit_source(flow.to_mermaid(), true);
        self.selected = Some(id);
        self.fit = true;
        self.status = "Node added · r renames · c connects".into();
    }

    fn cycle_node(&mut self, delta: isize) {
        let Ok(flow) = &self.flow else {
            self.visual_unavailable();
            return;
        };
        if flow.nodes.is_empty() {
            return;
        }
        let index = self
            .selected
            .as_ref()
            .and_then(|id| flow.nodes.iter().position(|node| &node.id == id))
            .unwrap_or(0);
        let next = (index as isize + delta).rem_euclid(flow.nodes.len() as isize) as usize;
        self.selected = Some(flow.nodes[next].id.clone());
        self.status.clear();
        if self.rendered_revision == self.revision
            && let Some(rendered) = &self.rendered
        {
            self.fit = false;
            graph::reveal_node(
                rendered,
                self.graph_area,
                &mut self.pan,
                self.zoom,
                &flow.nodes[next].id,
            );
        }
    }

    fn finish_connection(&mut self) {
        let (Some(from), Some(to)) = (self.connect_from.clone(), self.selected.clone()) else {
            return;
        };
        let Ok(mut flow) = self.flow.clone() else {
            return;
        };
        if flow.node(&from).is_none() || flow.node(&to).is_none() {
            return;
        }
        flow.edges.push(Edge {
            from,
            to,
            label: String::new(),
            kind: EdgeKind::Arrow,
        });
        let index = flow.edges.len() - 1;
        self.edit_source(flow.to_mermaid(), true);
        self.prompt(
            "Connection label (optional)",
            "",
            PromptKind::EdgeLabel { index },
        );
    }

    fn edge_picker(&mut self) {
        let Ok(flow) = &self.flow else {
            self.visual_unavailable();
            return;
        };
        let indices = flow
            .edges
            .iter()
            .enumerate()
            .filter(|(_, edge)| {
                self.selected
                    .as_ref()
                    .is_none_or(|id| &edge.from == id || &edge.to == id)
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        if indices.is_empty() {
            self.status = "No connections on this node. Use c to create one.".into();
        } else {
            self.modal = Some(Modal::Edges {
                indices,
                selected: 0,
            });
        }
    }

    fn request_action(&mut self, action: PendingAction) {
        self.leave_source();
        if self.document.is_dirty() {
            self.modal = Some(Modal::Unsaved { action });
        } else {
            self.perform_action(action);
        }
    }

    fn perform_action(&mut self, action: PendingAction) {
        match action {
            PendingAction::Quit => self.should_quit = true,
            PendingAction::Open => self.prompt("Open Mermaid file", "", PromptKind::Open),
            PendingAction::OpenPath(path) => self.open_path(path),
            PendingAction::New => {
                self.document.reset(crate::EMPTY.to_owned());
                self.reset_preview();
                self.source_changed(true);
                self.focus = Focus::Graph;
                self.status = "New flowchart · a adds a step · d adds a decision".into();
            }
        }
    }

    fn reset_preview(&mut self) {
        self.rendered = None;
        self.rendered_flow = None;
        self.error = None;
        self.fit = true;
        self.selected = None;
    }

    fn prompt(&mut self, title: &str, value: &str, kind: PromptKind) {
        let mut input = textarea(value);
        input.move_cursor(CursorMove::End);
        self.modal = Some(Modal::Prompt {
            title: title.into(),
            input: Box::new(input),
            kind,
        });
    }

    fn save_prompt(&mut self, after: Option<PendingAction>) {
        let value = self
            .document
            .path
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|| "diagram.mmd".into());
        self.prompt("Save Mermaid as", &value, PromptKind::Save { after });
    }

    fn save(&mut self, after: Option<PendingAction>) {
        if let Some(path) = self.document.path.clone() {
            self.write(path, WriteKind::Source, after);
        } else {
            self.save_prompt(after);
        }
    }

    fn export_prompt(&mut self) {
        if self.rendered_revision != self.revision || self.rendered.is_none() {
            self.status = "Wait for a valid preview before exporting SVG.".into();
            return;
        }
        let value = self
            .document
            .path
            .as_ref()
            .map(|path| path.with_extension("svg").to_string_lossy().into_owned())
            .unwrap_or_else(|| "diagram.svg".into());
        self.prompt("Export SVG as", &value, PromptKind::Export);
    }

    fn confirm_write(&mut self, path: PathBuf, kind: WriteKind, after: Option<PendingAction>) {
        if path.exists()
            && !(kind == WriteKind::Source && self.document.path.as_ref() == Some(&path))
        {
            self.modal = Some(Modal::Overwrite { path, kind, after });
        } else {
            self.write(path, kind, after);
        }
    }

    fn write(&mut self, path: PathBuf, kind: WriteKind, after: Option<PendingAction>) {
        self.insert_group = false;
        let result = match kind {
            WriteKind::Source => self.document.save(&path),
            WriteKind::Svg => match &self.rendered {
                Some(rendered) if self.rendered_revision == self.revision => {
                    atomic_export(&path, &rendered.svg)
                }
                _ => Err("The current source does not have a valid preview to export.".into()),
            },
        };
        match result {
            Ok(()) => {
                self.status = format!(
                    "{} {}",
                    if kind == WriteKind::Source {
                        "Saved"
                    } else {
                        "Exported"
                    },
                    path.display()
                );
                if let Some(action) = after {
                    self.perform_action(action);
                }
            }
            Err(error) => {
                self.status = error;
                // Failed writes do not execute the destructive continuation.
                if let Some(action) = after {
                    self.modal = Some(Modal::Unsaved { action });
                }
            }
        }
    }

    fn handle_modal(&mut self, key: KeyEvent) {
        let Some(modal) = self.modal.take() else {
            return;
        };
        if key.code == KeyCode::Esc
            || (key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c'))
        {
            self.status = "Cancelled".into();
            return;
        }
        match modal {
            Modal::Help => {
                if key.code != KeyCode::F(1) && key.code != KeyCode::Enter {
                    self.modal = Some(Modal::Help);
                }
            }
            Modal::Unsaved { action } => match key.code {
                KeyCode::Char('s' | 'S') => self.save(Some(action)),
                KeyCode::Char('d' | 'D') => self.perform_action(action),
                _ => self.modal = Some(Modal::Unsaved { action }),
            },
            Modal::Overwrite { path, kind, after } => {
                if matches!(key.code, KeyCode::Char('y' | 'Y')) {
                    self.write(path, kind, after);
                } else if matches!(key.code, KeyCode::Char('n' | 'N')) {
                    self.status = "Cancelled".into();
                } else {
                    self.modal = Some(Modal::Overwrite { path, kind, after });
                }
            }
            Modal::Prompt {
                title,
                mut input,
                kind,
            } => {
                if key.code == KeyCode::Enter {
                    self.submit_prompt(input.lines().join(""), kind);
                } else {
                    input.input(key);
                    self.modal = Some(Modal::Prompt { title, input, kind });
                }
            }
            Modal::Edges {
                indices,
                mut selected,
            } => {
                let Some(&index) = indices.get(selected) else {
                    return;
                };
                match key.code {
                    KeyCode::Char('j') | KeyCode::Down => selected = (selected + 1) % indices.len(),
                    KeyCode::Char('k') | KeyCode::Up => {
                        selected = (selected + indices.len() - 1) % indices.len()
                    }
                    KeyCode::Char('r') | KeyCode::Enter => {
                        if let Some(edge) = self
                            .flow
                            .as_ref()
                            .ok()
                            .and_then(|flow| flow.edges.get(index))
                        {
                            self.prompt(
                                "Connection label",
                                &edge.label.clone(),
                                PromptKind::EdgeLabel { index },
                            );
                        }
                        return;
                    }
                    KeyCode::Char('s') => self.edit_flow(|flow| {
                        if let Some(edge) = flow.edges.get_mut(index) {
                            edge.kind = EdgeKind::from_index((edge.kind.index() + 1) % 4);
                        }
                    }),
                    KeyCode::Delete | KeyCode::Backspace => {
                        self.edit_flow(|flow| {
                            if index < flow.edges.len() {
                                flow.edges.remove(index);
                            }
                        });
                        self.edge_picker();
                        return;
                    }
                    _ => {}
                }
                self.modal = Some(Modal::Edges { indices, selected });
            }
        }
    }

    fn submit_prompt(&mut self, text: String, kind: PromptKind) {
        match kind {
            PromptKind::Rename { node } => self.edit_flow(|flow| {
                if let Some(node) = flow.node_mut(&node) {
                    node.label = text;
                }
            }),
            PromptKind::EdgeLabel { index } => self.edit_flow(|flow| {
                if let Some(edge) = flow.edges.get_mut(index) {
                    edge.label = text;
                }
            }),
            PromptKind::Save { after } => {
                if text.trim().is_empty() {
                    self.status = "Choose a file name.".into();
                    self.save_prompt(after);
                } else {
                    self.confirm_write(PathBuf::from(text), WriteKind::Source, after);
                }
            }
            PromptKind::Export => {
                if text.trim().is_empty() {
                    self.status = "Choose an SVG file name.".into();
                    self.export_prompt();
                } else {
                    self.confirm_write(PathBuf::from(text), WriteKind::Svg, None);
                }
            }
            PromptKind::Open => self.open_path(PathBuf::from(text)),
        }
    }

    fn open_path(&mut self, path: PathBuf) {
        match self.document.load(&path) {
            Ok(()) => {
                self.reset_preview();
                self.source_changed(true);
                self.status = "Opened Mermaid source · i inserts · Esc returns to Normal".into();
            }
            Err(error) => self.status = error,
        }
    }
}

fn textarea(source: &str) -> TextArea<'static> {
    let mut editor = TextArea::new(source.split('\n').map(String::from).collect());
    editor.set_max_histories(0);
    editor.set_tab_length(4);
    editor
}

fn atomic_export(path: &std::path::Path, svg: &str) -> Result<(), String> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    let export_error =
        |error: std::io::Error| format!("Could not export {}: {error}", path.display());
    let mut temporary = tempfile::Builder::new()
        .prefix(".fin-svg-")
        .tempfile_in(parent)
        .map_err(export_error)?;
    if let Ok(metadata) = fs::metadata(path) {
        if !metadata.is_file() {
            return Err("Choose a file as the export destination.".into());
        }
        temporary
            .as_file()
            .set_permissions(metadata.permissions())
            .map_err(export_error)?;
    }
    temporary.write_all(svg.as_bytes()).map_err(export_error)?;
    temporary.as_file().sync_all().map_err(export_error)?;
    temporary
        .persist(path)
        .map_err(|error| export_error(error.error))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(app: &mut App, code: KeyCode) {
        app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
    }
    fn ctrl(app: &mut App, letter: char) {
        app.handle_key(KeyEvent::new(KeyCode::Char(letter), KeyModifiers::CONTROL));
    }
    fn empty_app() -> App {
        let mut app = App::new(Document::new(crate::EMPTY.into()));
        app.focus = Focus::Graph;
        app
    }

    #[test]
    fn visual_add_connect_rename_and_document_undo_are_bidirectional() {
        let mut app = empty_app();
        key(&mut app, KeyCode::Char('a'));
        let source_id = app.selected.clone().unwrap();
        key(&mut app, KeyCode::Char('c'));
        key(&mut app, KeyCode::Char('d'));
        // Adding a node cancels a pending connection; choose the source again.
        key(&mut app, KeyCode::Char('k'));
        key(&mut app, KeyCode::Char('c'));
        key(&mut app, KeyCode::Char('j'));
        let target_id = app.selected.clone().unwrap();
        key(&mut app, KeyCode::Enter);
        app.submit_prompt("yes ✅".into(), PromptKind::EdgeLabel { index: 0 });
        app.modal = None;
        let flow = Flow::parse(&app.document.source).unwrap();
        assert_eq!(flow.edges[0].from, source_id);
        assert_eq!(flow.edges[0].to, target_id);
        assert_eq!(flow.edges[0].label, "yes ✅");
        app.submit_prompt(
            "Résumé 🚀".into(),
            PromptKind::Rename {
                node: target_id.clone(),
            },
        );
        assert!(app.editor.lines().join("\n").contains("Résumé 🚀"));
        ctrl(&mut app, 'z');
        assert_eq!(
            app.flow.as_ref().unwrap().node(&target_id).unwrap().label,
            "Decision"
        );
        ctrl(&mut app, 'y');
        assert_eq!(
            app.flow.as_ref().unwrap().node(&target_id).unwrap().label,
            "Résumé 🚀"
        );
    }

    #[test]
    fn paste_and_source_edits_use_shared_undo_without_widget_history() {
        let mut app = empty_app();
        app.focus = Focus::Source;
        app.editor.move_cursor(CursorMove::Bottom);
        app.handle_event(Event::Paste("  A[\"你好 🌍\"]\n  A --> B".into()));
        assert_eq!(app.flow.as_ref().unwrap().nodes.len(), 2);
        ctrl(&mut app, 'z');
        assert_eq!(app.document.source, crate::EMPTY);
        ctrl(&mut app, 'y');
        assert!(app.document.source.contains("你好 🌍"));
    }

    #[test]
    fn unsaved_save_as_continuation_occurs_only_after_success() {
        let directory = tempfile::tempdir().unwrap();
        let mut app = empty_app();
        key(&mut app, KeyCode::Char('a'));
        ctrl(&mut app, 'q');
        assert!(matches!(
            app.modal,
            Some(Modal::Unsaved {
                action: PendingAction::Quit
            })
        ));
        key(&mut app, KeyCode::Char('s'));
        assert!(matches!(
            app.modal,
            Some(Modal::Prompt {
                kind: PromptKind::Save {
                    after: Some(PendingAction::Quit)
                },
                ..
            })
        ));
        app.modal = None;
        app.submit_prompt(
            directory
                .path()
                .join("missing/file.mmd")
                .to_string_lossy()
                .into_owned(),
            PromptKind::Save {
                after: Some(PendingAction::Quit),
            },
        );
        assert!(!app.should_quit);
        assert!(app.document.is_dirty());
        assert!(matches!(app.modal, Some(Modal::Unsaved { .. })));
        app.modal = None;
        let path = directory.path().join("saved.mmd");
        app.submit_prompt(
            path.to_string_lossy().into_owned(),
            PromptKind::Save {
                after: Some(PendingAction::Quit),
            },
        );
        assert!(app.should_quit);
        assert!(!app.document.is_dirty());
        assert_eq!(fs::read_to_string(path).unwrap(), app.document.source);
    }

    #[test]
    fn overwrite_cancel_keeps_existing_file_and_dirty_source() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("existing.mmd");
        fs::write(&path, "existing").unwrap();
        let mut app = empty_app();
        key(&mut app, KeyCode::Char('a'));
        app.confirm_write(path.clone(), WriteKind::Source, Some(PendingAction::New));
        assert!(matches!(app.modal, Some(Modal::Overwrite { .. })));
        key(&mut app, KeyCode::Esc);
        assert!(app.document.is_dirty());
        assert_eq!(fs::read_to_string(path).unwrap(), "existing");
        assert_eq!(app.flow.as_ref().unwrap().nodes.len(), 1);
    }

    #[test]
    fn invalid_source_cannot_be_replaced_by_visual_actions_or_stale_reply() {
        let mut app = empty_app();
        app.edit_source("flowchart TD\nsubgraph group\nA --> B\nend".into(), true);
        let before = app.document.source.clone();
        key(&mut app, KeyCode::Char('a'));
        assert_eq!(app.document.source, before);
        app.error = Some("current error".into());
        app.rendering = true;
        app.accept_reply(RenderReply {
            revision: app.revision - 1,
            source: crate::EMPTY.into(),
            result: Err("stale error".into()),
        });
        assert_eq!(app.error.as_deref(), Some("current error"));
        assert!(app.rendering);
    }

    #[test]
    fn edge_remove_and_shape_edits_are_undoable() {
        let mut app = App::new(Document::new("flowchart TD\nA --> B".into()));
        app.focus = Focus::Graph;
        key(&mut app, KeyCode::Char('s'));
        assert_eq!(app.flow.as_ref().unwrap().nodes[0].shape, Shape::Rounded);
        key(&mut app, KeyCode::Char('e'));
        key(&mut app, KeyCode::Delete);
        assert!(app.flow.as_ref().unwrap().edges.is_empty());
        ctrl(&mut app, 'z');
        assert_eq!(app.flow.as_ref().unwrap().edges.len(), 1);
    }

    #[test]
    fn invalid_source_retains_last_valid_graph_and_cannot_export_stale_svg() {
        let mut app = App::new(Document::new("flowchart TD\nA --> B".into()));
        app.render_sync();
        let good_revision = app.rendered_revision;
        let good_svg = app.rendered.as_ref().unwrap().svg.clone();
        app.edit_source("flowchart TD\n A[broken".into(), true);
        app.render_sync();
        assert!(app.error.is_some());
        assert_eq!(app.rendered_revision, good_revision);
        assert_eq!(app.rendered.as_ref().unwrap().svg, good_svg);
        assert_eq!(app.rendered_flow.as_ref().unwrap().nodes.len(), 2);
        app.export_prompt();
        assert!(app.modal.is_none());
        assert!(app.status.contains("valid preview"));
    }

    #[test]
    fn clicking_source_uses_display_columns_for_wide_unicode() {
        use crossterm::event::{MouseButton, MouseEvent};
        use ratatui::{Terminal, backend::TestBackend};

        let mut app = App::new(Document::new("flowchart TD\n A[你好]".into()));
        app.focus = Focus::Graph;
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal
            .draw(|frame| super::super::view::draw(frame, &mut app))
            .unwrap();
        // The first three columns are " A[", then the two-column character 你.
        app.handle_event(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: app.source_area.x + app.editor.line_number_width() + 5,
            row: app.source_area.y + 1,
            modifiers: KeyModifiers::NONE,
        }));
        assert_eq!(app.focus, Focus::Source);
        assert_eq!(app.editor.cursor(), (1, 4));
        key(&mut app, KeyCode::Char('i'));
        key(&mut app, KeyCode::Char('!'));
        assert_eq!(app.document.source, "flowchart TD\n A[你!好]");
    }
}
