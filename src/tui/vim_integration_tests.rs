//! Exercise Vim commands through the same events used by the terminal frontend.

use std::fs;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

use super::app::{App, Focus, Modal, PendingAction};
use crate::document::Document;

const SOURCE: &str = "flowchart TD\nA --> B";

fn app() -> App {
    App::new(Document::new(SOURCE.into()))
}

fn key(app: &mut App, code: KeyCode) {
    app.handle_event(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
}

fn text(app: &mut App, text: &str) {
    for ch in text.chars() {
        key(
            app,
            if ch == '\n' {
                KeyCode::Enter
            } else {
                KeyCode::Char(ch)
            },
        );
    }
}

fn ctrl(app: &mut App, ch: char) {
    app.handle_event(Event::Key(KeyEvent::new(
        KeyCode::Char(ch),
        KeyModifiers::CONTROL,
    )));
}

fn ex(app: &mut App, command: &str) {
    text(app, ":");
    text(app, command);
    key(app, KeyCode::Enter);
}

fn insert_comment(app: &mut App) {
    text(app, "i%% edited in Vim\n");
    key(app, KeyCode::Esc);
}

#[test]
fn source_starts_in_normal_mode_and_motions_do_not_edit() {
    let mut app = app();
    let revision = app.revision;
    text(&mut app, "ljhlj0$wbe2hggG");
    assert_eq!(app.document.source, SOURCE);
    assert_eq!(app.editor.lines().join("\n"), SOURCE);
    assert!(!app.document.is_dirty());
    assert!(!app.document.can_undo());
    assert_eq!(app.revision, revision);
    assert_eq!(app.focus, Focus::Source);
}

#[test]
fn insert_session_and_visual_edits_share_one_ordered_undo_history() {
    let mut app = app();
    insert_comment(&mut app);
    let inserted = app.document.source.clone();
    assert_eq!(inserted, format!("%% edited in Vim\n{SOURCE}"));

    key(&mut app, KeyCode::Tab);
    assert_eq!(app.focus, Focus::Graph);
    text(&mut app, "a");
    let with_node = app.document.source.clone();
    assert_eq!(app.flow.as_ref().unwrap().nodes.len(), 3);
    assert_ne!(with_node, inserted);

    key(&mut app, KeyCode::Tab);
    text(&mut app, "u");
    assert_eq!(app.document.source, inserted);
    text(&mut app, "u");
    assert_eq!(app.document.source, SOURCE);
    assert!(!app.document.is_dirty());
    ctrl(&mut app, 'r');
    assert_eq!(app.document.source, inserted);
    ctrl(&mut app, 'r');
    assert_eq!(app.document.source, with_node);
    assert_eq!(app.editor.lines().join("\n"), with_node);
}

#[test]
fn leaving_insert_for_graph_finishes_its_undo_group() {
    let mut app = app();
    text(&mut app, "i%% edited before changing panels\n");
    let inserted = app.document.source.clone();
    key(&mut app, KeyCode::Tab);
    assert_eq!(app.focus, Focus::Graph);
    text(&mut app, "a");
    assert_eq!(app.flow.as_ref().unwrap().nodes.len(), 3);

    ctrl(&mut app, 'z');
    assert_eq!(app.document.source, inserted);
    ctrl(&mut app, 'z');
    assert_eq!(app.document.source, SOURCE);
    assert!(!app.document.is_dirty());
}

#[test]
fn function_key_modals_cancel_pending_vim_operators() {
    for modal_key in [KeyCode::F(1), KeyCode::F(4)] {
        let mut app = app();
        text(&mut app, "d");
        key(&mut app, modal_key);
        assert!(app.modal.is_some());
        key(&mut app, KeyCode::Esc);
        text(&mut app, "w");

        assert!(app.modal.is_none());
        assert_eq!(app.document.source, SOURCE);
        assert!(!app.document.can_undo());
        assert!(!app.document.is_dirty());
    }
}

#[test]
fn write_existing_file_saves_source_and_marks_document_clean() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("diagram.mmd");
    fs::write(&path, SOURCE).unwrap();
    let mut document = Document::new(String::new());
    document.load(&path).unwrap();
    let mut app = App::new(document);
    insert_comment(&mut app);
    ex(&mut app, "w");

    assert_eq!(fs::read_to_string(&path).unwrap(), app.document.source);
    assert_eq!(app.document.path.as_deref(), Some(path.as_path()));
    assert!(!app.document.is_dirty());
    assert!(!app.should_quit);
    assert!(app.modal.is_none());
}

#[test]
fn saving_during_insert_starts_a_new_undo_group_at_the_saved_baseline() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("checkpoint.mmd");
    fs::write(&path, SOURCE).unwrap();
    let mut document = Document::new(String::new());
    document.load(&path).unwrap();
    let mut app = App::new(document);
    text(&mut app, "i%% first ");
    ctrl(&mut app, 's');
    let saved = app.document.source.clone();
    assert_eq!(fs::read_to_string(&path).unwrap(), saved);
    assert!(!app.document.is_dirty());

    text(&mut app, "second\n");
    key(&mut app, KeyCode::Esc);
    assert_ne!(app.document.source, saved);
    assert!(app.document.is_dirty());
    text(&mut app, "u");
    assert_eq!(app.document.source, saved);
    assert!(!app.document.is_dirty());
    assert_eq!(fs::read_to_string(&path).unwrap(), saved);
    text(&mut app, "u");
    assert_eq!(app.document.source, SOURCE);
}

#[test]
fn write_command_accepts_paths_with_spaces() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("my mermaid diagram.mmd");
    let mut app = app();
    insert_comment(&mut app);
    ex(&mut app, &format!("w {}", path.display()));

    assert_eq!(fs::read_to_string(&path).unwrap(), app.document.source);
    assert_eq!(app.document.path.as_deref(), Some(path.as_path()));
    assert!(!app.document.is_dirty());
    assert!(!app.should_quit);
}

#[test]
fn command_bracketed_paste_edits_the_command_without_touching_source() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("pasted file name.mmd");
    let mut app = app();
    text(&mut app, ":w ");
    app.handle_event(Event::Paste(path.to_string_lossy().into_owned()));
    assert_eq!(app.document.source, SOURCE);
    key(&mut app, KeyCode::Enter);

    assert_eq!(fs::read_to_string(&path).unwrap(), SOURCE);
    assert_eq!(app.document.path.as_deref(), Some(path.as_path()));
    assert!(!app.document.is_dirty());
    assert!(!app.document.can_undo());
}

#[test]
fn write_quit_closes_only_after_successful_save() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("saved before exit.mmd");
    let mut app = app();
    insert_comment(&mut app);
    ex(&mut app, &format!("wq {}", path.display()));

    assert_eq!(fs::read_to_string(&path).unwrap(), app.document.source);
    assert!(!app.document.is_dirty());
    assert!(app.should_quit);
}

#[test]
fn failed_write_quit_retains_dirty_source_and_keeps_session_open() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("missing/diagram.mmd");
    let mut app = app();
    insert_comment(&mut app);
    let source = app.document.source.clone();
    ex(&mut app, &format!("wq {}", path.display()));

    assert!(!app.should_quit);
    assert!(app.document.is_dirty());
    assert_eq!(app.document.source, source);
    assert!(!path.exists());
    assert!(app.status.to_lowercase().contains("save"));
}

#[test]
fn quit_protects_unsaved_edits_and_force_quit_is_explicit() {
    let mut app = app();
    insert_comment(&mut app);
    let source = app.document.source.clone();
    ex(&mut app, "q");
    assert!(!app.should_quit);
    assert!(matches!(
        app.modal,
        Some(Modal::Unsaved {
            action: PendingAction::Quit
        })
    ));

    key(&mut app, KeyCode::Esc);
    assert!(app.modal.is_none());
    assert_eq!(app.document.source, source);
    ex(&mut app, "q!");
    assert!(app.should_quit);
    assert_eq!(app.document.source, source);
}

#[test]
fn cancelled_and_unknown_commands_never_become_mermaid_text() {
    let mut app = app();
    text(&mut app, ":w abandoned-path.mmd");
    key(&mut app, KeyCode::Esc);
    assert_eq!(app.document.source, SOURCE);
    ex(&mut app, "not-a-fin-command");

    assert_eq!(app.document.source, SOURCE);
    assert!(!app.document.can_undo());
    assert!(!app.should_quit);
    let status = app.status.to_lowercase();
    assert!(
        status.contains("not-a-fin-command")
            || status.contains("unknown")
            || status.contains("unsupported"),
        "Unknown commands need an explanatory status: {}",
        app.status
    );
}

#[test]
fn graph_shortcuts_are_unchanged_by_source_vim_commands() {
    let mut app = app();
    key(&mut app, KeyCode::Tab);
    text(&mut app, "ad");
    assert_eq!(app.flow.as_ref().unwrap().nodes.len(), 4);
    assert!(app.document.is_dirty());
    let selected = app.selected.clone();
    text(&mut app, "k");
    assert_ne!(app.selected, selected);
    text(&mut app, "j");
    assert_eq!(app.selected, selected);
    text(&mut app, "r");
    assert!(matches!(app.modal, Some(Modal::Prompt { .. })));
    key(&mut app, KeyCode::Esc);
    assert_eq!(app.focus, Focus::Graph);
}

#[test]
fn normal_bracketed_paste_is_literal_and_one_undoable_edit() {
    let mut app = app();
    app.handle_event(Event::Paste("%% 你好 🌍\r\n".into()));
    let pasted = app.document.source.clone();
    assert_eq!(pasted, format!("%% 你好 🌍\n{SOURCE}"));
    text(&mut app, "h");
    assert_eq!(app.document.source, pasted);
    text(&mut app, "u");
    assert_eq!(app.document.source, SOURCE);
    assert!(!app.document.can_undo());
    ctrl(&mut app, 'r');
    assert_eq!(app.document.source, pasted);
}

#[test]
fn insert_bracketed_paste_belongs_to_the_surrounding_insert_session() {
    let mut app = app();
    text(&mut app, "i%% ");
    app.handle_event(Event::Paste("Vim 你好\r\n".into()));
    key(&mut app, KeyCode::Esc);
    assert_eq!(app.document.source, format!("%% Vim 你好\n{SOURCE}"));
    text(&mut app, "u");
    assert_eq!(app.document.source, SOURCE);
    assert!(!app.document.can_undo());
}

#[test]
fn control_c_returns_source_to_normal_and_graph_still_protects_quit() {
    let mut app = app();
    ctrl(&mut app, 'c');
    assert!(!app.should_quit);
    assert!(app.modal.is_none());

    text(&mut app, "i%% unfinished\n");
    ctrl(&mut app, 'c');
    let source = app.document.source.clone();
    text(&mut app, "h");
    assert_eq!(app.document.source, source);
    assert!(!app.should_quit);
    assert!(app.modal.is_none());

    key(&mut app, KeyCode::Tab);
    ctrl(&mut app, 'c');
    assert!(!app.should_quit);
    assert!(matches!(
        app.modal,
        Some(Modal::Unsaved {
            action: PendingAction::Quit
        })
    ));
}

#[test]
fn edit_command_uses_unsaved_protection_before_loading_another_file() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("another diagram.mmd");
    let next_source = "flowchart LR\nC --> D";
    fs::write(&path, next_source).unwrap();
    let mut app = app();
    insert_comment(&mut app);
    let edited = app.document.source.clone();
    ex(&mut app, &format!("e {}", path.display()));
    assert!(matches!(app.modal, Some(Modal::Unsaved { .. })));
    assert_eq!(app.document.source, edited);
    key(&mut app, KeyCode::Esc);
    assert_eq!(app.document.source, edited);

    ex(&mut app, &format!("e {}", path.display()));
    text(&mut app, "d");
    assert_eq!(app.document.source, next_source);
    assert_eq!(app.document.path.as_deref(), Some(path.as_path()));
    assert!(!app.document.is_dirty());
    assert!(!app.document.can_undo());
    assert!(!app.should_quit);
}
