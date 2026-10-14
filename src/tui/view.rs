//! Minimal, dark-by-default terminal presentation.

use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};

use super::{
    app::{App, Focus, Modal},
    graph::{ACCENT, BACKGROUND, Camera, FOREGROUND, GraphView, MUTED, fit_camera},
};

const BORDER: Color = Color::Rgb(51, 56, 70);
const PANEL: Color = Color::Rgb(27, 30, 39);
const ERROR: Color = Color::Rgb(237, 155, 164);
const SUCCESS: Color = Color::Rgb(166, 212, 179);

pub(super) fn draw(frame: &mut Frame<'_>, app: &mut App) {
    let screen = frame.area();
    frame.render_widget(
        Block::default().style(Style::default().bg(BACKGROUND).fg(FOREGROUND)),
        screen,
    );
    if screen.is_empty() {
        return;
    }
    let sections = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(1),
        Constraint::Length(2),
    ])
    .split(screen);
    draw_header(frame, sections[0], app);
    let content = sections[1];
    let graph = if app.source_visible {
        let panes = if content.width >= 75 {
            Layout::horizontal([Constraint::Percentage(42), Constraint::Percentage(58)])
                .split(content)
        } else {
            Layout::vertical([Constraint::Percentage(45), Constraint::Percentage(55)])
                .split(content)
        };
        draw_source(frame, panes[0], app);
        panes[1]
    } else {
        app.source_area = Rect::default();
        content
    };
    draw_graph(frame, graph, app);
    draw_footer(frame, sections[2], app);
    if app.modal.is_some() {
        draw_modal(frame, app);
    }
}

fn draw_header(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let name = app
        .document
        .path
        .as_ref()
        .and_then(|path| path.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Untitled.mmd".to_owned());
    let dirty = if app.document.is_dirty() { " ●" } else { "" };
    let state = if app.rendering {
        ("rendering", MUTED)
    } else if app.error.is_some() {
        ("preview retained", ERROR)
    } else {
        ("live", SUCCESS)
    };
    let title = Line::from(vec![
        Span::styled(
            " fin ",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" / ", Style::default().fg(BORDER)),
        Span::raw(name),
        Span::styled(dirty, Style::default().fg(ACCENT)),
        Span::styled("   TUI", Style::default().fg(MUTED)),
        Span::styled(format!("   • {}", state.0), Style::default().fg(state.1)),
    ]);
    frame.render_widget(Paragraph::new(title), area);
}

fn panel(title: String, focused: bool) -> Block<'static> {
    Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(if focused { ACCENT } else { BORDER }))
        .style(Style::default().bg(BACKGROUND).fg(FOREGROUND))
}

fn draw_source(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    if area.is_empty() {
        return;
    }
    let focused = app.focus == Focus::Source && app.modal.is_none();
    let block = panel(
        format!(" Mermaid source · {} ", app.vim.mode_label()),
        focused,
    );
    app.source_area = block.inner(area);
    app.editor.set_block(block);
    app.editor
        .set_style(Style::default().fg(FOREGROUND).bg(BACKGROUND));
    app.editor.set_line_number_style(Style::default().fg(MUTED));
    app.editor
        .set_cursor_line_style(Style::default().bg(if focused { PANEL } else { BACKGROUND }));
    app.editor.set_cursor_style(if focused {
        Style::default().bg(ACCENT).fg(BACKGROUND)
    } else {
        Style::default().fg(FOREGROUND).bg(BACKGROUND)
    });
    app.editor
        .set_selection_style(Style::default().bg(Color::Rgb(67, 57, 94)));
    if area.width > 2 && area.height > 2 {
        frame.render_widget(&app.editor, area);
    } else {
        frame.render_widget(panel(" Source ".into(), focused), area);
    }
}

fn draw_graph(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    let block = panel(
        format!(" Graph · {:.0}% ", app.zoom * 100.0),
        app.focus == Focus::Graph && app.modal.is_none(),
    );
    app.graph_area = block.inner(area);
    if app.fit
        && let Some(rendered) = &app.rendered
    {
        (app.pan, app.zoom) = fit_camera(rendered, app.graph_area);
        app.fit = false;
    }
    // Build the title after fitting so displayed zoom matches the first frame.
    let block = panel(
        format!(" Graph · {:.0}% ", app.zoom * 100.0),
        app.focus == Focus::Graph && app.modal.is_none(),
    );
    frame.render_widget(block, area);
    if app.graph_area.is_empty() {
        return;
    }
    if let Some(rendered) = &app.rendered {
        frame.render_widget(
            GraphView {
                rendered,
                flow: app.rendered_flow.as_ref(),
                selected: app.selected.as_deref(),
                connect_from: app.connect_from.as_deref(),
                camera: Camera {
                    pan: app.pan,
                    zoom: app.zoom,
                },
            },
            app.graph_area,
        );
    } else {
        let message = if app.rendering {
            "Laying out your diagram…"
        } else {
            "Your diagram appears here.\n\nStart typing Mermaid, or focus this pane\nand press a to add a node."
        };
        let top = app.graph_area.height.saturating_sub(4) / 2;
        let empty = Rect {
            y: app.graph_area.y.saturating_add(top),
            height: app.graph_area.height.saturating_sub(top),
            ..app.graph_area
        };
        frame.render_widget(
            Paragraph::new(message)
                .alignment(Alignment::Center)
                .style(Style::default().fg(MUTED))
                .wrap(Wrap { trim: true }),
            empty,
        );
    }
}

fn draw_footer(frame: &mut Frame<'_>, area: Rect, app: &App) {
    if area.is_empty() {
        return;
    }
    let selected = app
        .selected
        .as_ref()
        .and_then(|id| {
            app.flow
                .as_ref()
                .ok()
                .and_then(|flow| flow.nodes.iter().find(|node| &node.id == id))
        })
        .map(|node| format!("{} · {}", node.id, node.label));
    let source_prompt = (app.focus == Focus::Source && app.modal.is_none())
        .then(|| app.vim.prompt())
        .flatten();
    let (status, color) = if let Some(prompt) = source_prompt {
        (format!(" {prompt}"), ACCENT)
    } else if let Some(from) = &app.connect_from {
        (
            format!(" Connect from {from} → select a target, then press Enter · Esc cancels"),
            ACCENT,
        )
    } else if !app.status.is_empty() {
        let status = if app.focus == Focus::Graph {
            selected.as_ref().map_or_else(
                || app.status.clone(),
                |selected| format!("{selected}  ·  {}", app.status),
            )
        } else {
            app.status.clone()
        };
        (format!(" {status}"), MUTED)
    } else if let Some(error) = &app.error {
        (format!(" {}", error.replace(['\n', '\r'], " ")), ERROR)
    } else if let Some(selected) = selected {
        (format!(" {selected}"), MUTED)
    } else if app.rendered_flow.is_none() && app.rendered.is_some() {
        (
            " Portable raster preview · edit Mermaid source or export SVG for full detail".into(),
            MUTED,
        )
    } else {
        (" Mermaid ↔ flowchart · changes stay in sync".into(), MUTED)
    };
    frame.render_widget(
        Paragraph::new(status).style(Style::default().fg(color)),
        Rect { height: 1, ..area },
    );
    if area.height > 1 {
        let help = if app.focus == Focus::Source {
            " i insert   Esc normal   h/j/k/l move   :w save   :q quit   Tab graph   F1 help"
        } else {
            " Tab source   a node   c connect   r rename   e edges   +/- zoom   0 fit   F1 help"
        };
        frame.render_widget(
            Paragraph::new(help).style(Style::default().fg(MUTED).bg(PANEL)),
            Rect {
                y: area.y + 1,
                height: 1,
                ..area
            },
        );
    }
}

fn centered(screen: Rect, width: u16, height: u16) -> Rect {
    let width = width
        .min(screen.width.saturating_sub(2))
        .max(screen.width.min(1));
    let height = height
        .min(screen.height.saturating_sub(2))
        .max(screen.height.min(1));
    Rect::new(
        screen.x + (screen.width - width) / 2,
        screen.y + (screen.height - height) / 2,
        width,
        height,
    )
}

fn popup(frame: &mut Frame<'_>, width: u16, height: u16, title: &str) -> Rect {
    let area = centered(frame.area(), width, height);
    frame.render_widget(Clear, area);
    let block = panel(format!(" {title} "), true).style(Style::default().bg(PANEL).fg(FOREGROUND));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    inner
}

fn draw_modal(frame: &mut Frame<'_>, app: &mut App) {
    let Some(modal) = &mut app.modal else {
        return;
    };
    match modal {
        Modal::Help => {
            let area = popup(frame, 88, 32, "Keyboard shortcuts");
            let lines = vec![
                Line::styled(
                    " WORKSPACE",
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                ),
                Line::raw(" Tab switch pane    Ctrl-B toggle source    F1 help    Esc cancel"),
                Line::raw(" Ctrl-N new    Ctrl-O open    Ctrl-S save    F4 save as"),
                Line::raw(" Ctrl-Z / Ctrl-Y undo / redo    Ctrl-E export SVG    Ctrl-Q quit"),
                Line::styled(
                    " SOURCE · VIM",
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                ),
                Line::raw(" i / a           Insert before / after cursor    Esc / Ctrl-C normal"),
                Line::raw(
                    " I / A / o / O   Insert at first text / line end / new line below / above",
                ),
                Line::raw(
                    " h / j / k / l   Move    w / b / e word    0 / ^ / $ line    gg / G file",
                ),
                Line::raw(
                    " d / c / y       Delete / change / yank + motion; dd / cc / yy whole line",
                ),
                Line::raw(
                    " x / D / C       Delete character / delete to line end / change to line end",
                ),
                Line::raw(" r<char> / s     Replace character / replace character in Insert mode"),
                Line::raw(" v / V           Visual / visual line    d / c / y act on selection"),
                Line::raw(" u / Ctrl-R      Undo / redo    p / P paste after / before"),
                Line::raw(" /text Enter     Find literal text    n / N next / previous match"),
                Line::raw(
                    " :w [path]       Save    :q quit    :q! discard and quit    :wq / :x save, quit",
                ),
                Line::raw(
                    " :e path         Open    Counts repeat motions and operators, e.g. 3j / 2dd",
                ),
                Line::styled(
                    " Workspace Ctrl shortcuts stay active; this is a focused Vim subset.",
                    Style::default().fg(MUTED),
                ),
                Line::styled(
                    " GRAPH",
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                ),
                Line::raw(" a / d           Add rectangle / diamond"),
                Line::raw(" j / k           Select next / previous node (or click)"),
                Line::raw(" r / s           Rename selected node / change shape"),
                Line::raw(" c               Choose source, select target, then Enter"),
                Line::raw(" e               Edit connections and their labels"),
                Line::raw(" Delete          Delete selected node and its connections"),
                Line::raw(" Arrow keys      Pan    + / - zoom    0 fit"),
                Line::raw(" Mouse wheel     Pan    Ctrl-wheel zoom"),
                Line::styled(
                    " Flowcharts use terminal labels; other diagrams use a half-block preview.",
                    Style::default().fg(MUTED),
                ),
                Line::styled(" SVG export keeps full detail.", Style::default().fg(MUTED)),
                Line::styled(" Esc / F1 close", Style::default().fg(ACCENT)),
            ];
            frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
        }
        Modal::Prompt { title, input, .. } => {
            let area = popup(frame, 76, 7, title);
            if !area.is_empty() {
                input.set_style(Style::default().fg(FOREGROUND).bg(PANEL));
                input.set_cursor_line_style(Style::default().bg(PANEL));
                input.set_cursor_style(Style::default().bg(ACCENT).fg(BACKGROUND));
                frame.render_widget(
                    &**input,
                    Rect {
                        height: area.height.min(2),
                        ..area
                    },
                );
                if area.height > 2 {
                    frame.render_widget(
                        Paragraph::new("Enter confirm · Esc cancel")
                            .style(Style::default().fg(MUTED)),
                        Rect {
                            y: area.y + area.height - 1,
                            height: 1,
                            ..area
                        },
                    );
                }
            }
        }
        Modal::Unsaved { .. } => {
            let area = popup(frame, 62, 8, "Unsaved changes");
            frame.render_widget(
                Paragraph::new(vec![
                    Line::raw("Your Mermaid source has unsaved changes."),
                    Line::raw(""),
                    Line::styled("s  Save and continue", Style::default().fg(ACCENT)),
                    Line::raw("d  Discard changes"),
                    Line::styled("Esc  Keep editing", Style::default().fg(MUTED)),
                ])
                .wrap(Wrap { trim: true }),
                area,
            );
        }
        Modal::Overwrite { path, .. } => {
            let area = popup(frame, 70, 8, "Replace existing file?");
            frame.render_widget(
                Paragraph::new(vec![
                    Line::raw(path.display().to_string()),
                    Line::raw(""),
                    Line::raw("This file already exists."),
                    Line::styled("y  Replace    Esc  Cancel", Style::default().fg(ACCENT)),
                ])
                .wrap(Wrap { trim: true }),
                area,
            );
        }
        Modal::Edges { indices, selected } => {
            let area = popup(frame, 82, 18, "Connections");
            let chunks = Layout::vertical([Constraint::Min(1), Constraint::Length(2)]).split(area);
            let items: Vec<ListItem<'_>> = indices
                .iter()
                .filter_map(|index| app.flow.as_ref().ok()?.edges.get(*index))
                .map(|edge| {
                    let kind = match edge.kind {
                        crate::flow::EdgeKind::Arrow => "→",
                        crate::flow::EdgeKind::Line => "—",
                        crate::flow::EdgeKind::Dotted => "⇢",
                        crate::flow::EdgeKind::Thick => "⇒",
                    };
                    ListItem::new(format!(
                        " {} {kind} {}{}",
                        edge.from,
                        edge.to,
                        if edge.label.is_empty() {
                            String::new()
                        } else {
                            format!("  · {}", edge.label)
                        }
                    ))
                })
                .collect();
            if items.is_empty() {
                frame.render_widget(
                    Paragraph::new("No connections. Use c in the graph to connect two nodes.")
                        .wrap(Wrap { trim: true })
                        .style(Style::default().fg(MUTED)),
                    chunks[0],
                );
            } else {
                let mut state = ListState::default().with_selected(Some(*selected));
                frame.render_stateful_widget(
                    List::new(items)
                        .highlight_style(Style::default().bg(Color::Rgb(59, 49, 84)).fg(ACCENT))
                        .highlight_symbol("› "),
                    chunks[0],
                    &mut state,
                );
            }
            frame.render_widget(
                Paragraph::new("j/k select · r label · s style · Delete remove\nEsc close")
                    .style(Style::default().fg(MUTED)),
                chunks[1],
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EXAMPLE, document::Document};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{Terminal, backend::TestBackend};

    fn screen_text(terminal: &mut Terminal<TestBackend>, app: &mut App) -> String {
        terminal.draw(|frame| draw(frame, app)).expect("draw");
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    fn press(app: &mut App, code: KeyCode) {
        app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    #[test]
    fn dark_workspace_shows_source_graph_and_document_state() {
        let mut app = App::new(Document::new(EXAMPLE.to_owned()));
        app.render_sync();
        let mut terminal = Terminal::new(TestBackend::new(140, 40)).expect("terminal");
        terminal.draw(|frame| draw(frame, &mut app)).expect("draw");
        let buffer = terminal.backend().buffer();
        let output: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
        assert!(output.contains("Mermaid source"));
        assert!(output.contains("Graph"));
        assert!(output.contains("Untitled.mmd"));
        assert!(buffer.content.iter().all(|cell| cell.bg != Color::White));
        assert!(app.graph_area.width > 0);
    }

    #[test]
    fn source_modes_and_command_prompts_are_visible() {
        let mut app = App::new(Document::new(EXAMPLE.to_owned()));
        let mut terminal = Terminal::new(TestBackend::new(120, 36)).expect("terminal");
        assert!(screen_text(&mut terminal, &mut app).contains("Mermaid source · NORMAL"));
        for (key, label) in [('i', "INSERT"), ('v', "VISUAL"), ('V', "VISUAL LINE")] {
            press(&mut app, KeyCode::Char(key));
            assert!(
                screen_text(&mut terminal, &mut app).contains(&format!("Mermaid source · {label}"))
            );
            press(&mut app, KeyCode::Esc);
        }
        for command in [":w graph.mmd", "/Mermaid"] {
            for key in command.chars() {
                press(&mut app, KeyCode::Char(key));
            }
            assert!(screen_text(&mut terminal, &mut app).contains(command));
            press(&mut app, KeyCode::Esc);
        }
        press(&mut app, KeyCode::Char('d'));
        assert!(app.vim.prompt().is_some());
        let output = screen_text(&mut terminal, &mut app);
        assert!(output.contains("i insert"));
        assert!(output.contains(":w save"));
    }

    #[test]
    fn help_fits_a_standard_terminal_with_source_and_graph_controls() {
        let mut app = App::new(Document::new(EXAMPLE.to_owned()));
        app.modal = Some(Modal::Help);
        let mut terminal = Terminal::new(TestBackend::new(120, 36)).expect("terminal");
        let output = screen_text(&mut terminal, &mut app);
        assert!(output.contains("SOURCE · VIM"));
        assert!(output.contains("GRAPH"));
        assert!(output.contains(":w [path]"));
        assert!(output.contains("Esc / F1 close"));
    }

    #[test]
    fn collapsed_source_and_modals_survive_tiny_terminals() {
        for (width, height) in [(1, 1), (2, 3), (12, 5), (60, 20)] {
            let mut app = App::new(Document::new("flowchart TD\n A[Hello]".to_owned()));
            let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
            screen_text(&mut terminal, &mut app);
            press(&mut app, KeyCode::Char('i'));
            screen_text(&mut terminal, &mut app);
            press(&mut app, KeyCode::Esc);
            press(&mut app, KeyCode::Char(':'));
            screen_text(&mut terminal, &mut app);
            app.source_visible = false;
            app.modal = Some(Modal::Help);
            terminal
                .draw(|frame| draw(frame, &mut app))
                .expect("tiny draw");
            app.modal = None;
            terminal
                .draw(|frame| draw(frame, &mut app))
                .expect("collapsed draw");
        }
    }
}
