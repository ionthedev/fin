//! Source text, bounded edit history, and atomic document persistence.

use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub const MAX_FILE_BYTES: usize = 256 * 1024;
const HISTORY_LIMIT: usize = 100;

#[derive(Debug)]
pub struct Document {
    pub source: String,
    pub path: Option<PathBuf>,
    baseline: String,
    undo: VecDeque<String>,
    redo: Vec<String>,
}

impl Document {
    pub fn new(source: String) -> Self {
        Self {
            baseline: source.clone(),
            source,
            path: None,
            undo: VecDeque::new(),
            redo: Vec::new(),
        }
    }

    pub fn is_dirty(&self) -> bool {
        self.source != self.baseline
    }

    /// Record a changed source snapshot and discard the abandoned redo branch.
    pub fn edit(&mut self, source: String) {
        if source == self.source {
            return;
        }
        let previous = std::mem::replace(&mut self.source, source);
        self.push_undo(previous);
        self.redo.clear();
    }

    /// Continue an existing editing gesture without adding another undo step.
    /// Callers start each gesture with `continue_previous = false`, and end it
    /// before saving, navigating history, or applying edits from another view.
    pub fn edit_grouped(&mut self, source: String, continue_previous: bool) {
        if continue_previous && !self.undo.is_empty() {
            if source != self.source {
                self.source = source;
                self.redo.clear();
            }
        } else {
            self.edit(source);
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo(&mut self) -> bool {
        let Some(previous) = self.undo.pop_back() else {
            return false;
        };
        self.redo
            .push(std::mem::replace(&mut self.source, previous));
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(next) = self.redo.pop() else {
            return false;
        };
        let previous = std::mem::replace(&mut self.source, next);
        self.push_undo(previous);
        true
    }

    /// Read a bounded UTF-8 source file, changing document state only on success.
    pub fn load(&mut self, path: &Path) -> Result<(), String> {
        let file = File::open(path)
            .map_err(|error| format!("Could not open {}: {error}", path.display()))?;
        let mut bytes = Vec::new();
        file.take((MAX_FILE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|error| format!("Could not read {}: {error}", path.display()))?;
        if bytes.len() > MAX_FILE_BYTES {
            return Err("This file exceeds the 256 KiB source limit.".into());
        }
        let source = String::from_utf8(bytes)
            .map_err(|_| "This file is not valid UTF-8 text.".to_string())?;
        self.reset(source);
        self.path = Some(path.to_path_buf());
        Ok(())
    }

    /// Write beside the destination, then atomically replace it on success.
    /// A failed write or replacement leaves the original file and saved baseline intact.
    pub fn save(&mut self, path: &Path) -> Result<(), String> {
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let save_error =
            |error: std::io::Error| format!("Could not save {}: {error}", path.display());
        let mut temporary = tempfile::Builder::new()
            .prefix(".fin-")
            .tempfile_in(parent)
            .map_err(save_error)?;
        match fs::metadata(path) {
            Ok(metadata) => {
                if !metadata.is_file() {
                    return Err("Choose a file as the save destination.".into());
                }
                temporary
                    .as_file()
                    .set_permissions(metadata.permissions())
                    .map_err(save_error)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(save_error(error)),
        }
        temporary
            .write_all(self.source.as_bytes())
            .map_err(save_error)?;
        temporary.as_file().sync_all().map_err(save_error)?;
        temporary
            .persist(path)
            .map_err(|error| save_error(error.error))?;
        self.baseline.clone_from(&self.source);
        self.path = Some(path.to_path_buf());
        Ok(())
    }

    /// Start a clean, untitled document with no undo or redo history.
    pub fn reset(&mut self, source: String) {
        *self = Self::new(source);
    }

    fn push_undo(&mut self, source: String) {
        if self.undo.len() == HISTORY_LIMIT {
            self.undo.pop_front();
        }
        self.undo.push_back(source);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grouped_edits_undo_together_and_preserve_saved_checkpoints() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let mut document = Document::new("start".into());
        document.edit_grouped("start one".into(), false);
        document.edit_grouped("start one two".into(), true);
        assert!(document.undo());
        assert_eq!(document.source, "start");
        assert!(document.redo());
        document
            .save(&directory.path().join("group.mmd"))
            .expect("save");
        document.edit_grouped("start one two three".into(), false);
        assert!(document.undo());
        assert!(!document.is_dirty());
        document.edit_grouped(document.source.clone(), true);
        assert!(document.can_redo());
        document.edit_grouped("new branch".into(), false);
        assert!(!document.can_redo());
    }

    #[test]
    fn undo_redo_tracks_saved_state_and_branches() {
        let mut document = Document::new("first".into());
        document.edit("second".into());
        document.edit("third".into());
        assert!(document.is_dirty());
        assert!(document.undo());
        assert_eq!(document.source, "second");
        assert!(document.undo());
        assert_eq!(document.source, "first");
        assert!(!document.is_dirty());
        assert!(!document.undo());
        assert!(document.redo());
        assert_eq!(document.source, "second");
        document.edit("new branch".into());
        assert!(!document.can_redo());
        assert!(!document.redo());
        assert!(document.undo());
        assert_eq!(document.source, "second");
    }

    #[test]
    fn unchanged_edit_preserves_redo_and_history_is_bounded() {
        let mut document = Document::new("0".into());
        for number in 1..=120 {
            document.edit(number.to_string());
        }
        assert_eq!(document.undo.len(), HISTORY_LIMIT);
        assert!(document.undo());
        document.edit(document.source.clone());
        assert!(document.can_redo());
        assert!(document.redo());
        let mut undone = 0;
        while document.undo() {
            undone += 1;
        }
        assert_eq!(undone, HISTORY_LIMIT);
        assert_eq!(document.source, "20");
    }

    #[test]
    fn saving_replaces_existing_file_and_retains_undo() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("diagram.mmd");
        fs::write(&path, "old contents").expect("existing file");
        let mut document = Document::new("flowchart TD\n  A --> B".into());
        document.edit("flowchart LR\n  A --> B".into());
        document.save(&path).expect("save document");
        assert_eq!(
            fs::read_to_string(&path).expect("saved source"),
            document.source
        );
        assert_eq!(document.path.as_deref(), Some(path.as_path()));
        assert!(!document.is_dirty());
        assert!(document.undo());
        assert!(document.is_dirty());
        assert!(document.redo());
        assert!(!document.is_dirty());
    }

    #[test]
    fn failed_save_preserves_original_and_dirty_document() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("diagram.mmd");
        let mut document = Document::new("original".into());
        document.save(&path).expect("initial save");
        document.edit("unsaved changes".into());
        let invalid_destination = directory.path().join("missing").join("diagram.mmd");
        assert!(document.save(&invalid_destination).is_err());
        assert_eq!(document.path.as_deref(), Some(path.as_path()));
        assert_eq!(document.source, "unsaved changes");
        assert!(document.is_dirty());
        assert_eq!(
            fs::read_to_string(&path).expect("original file"),
            "original"
        );
        assert!(document.save(directory.path()).is_err());
        assert_eq!(
            fs::read_to_string(&path).expect("original file"),
            "original"
        );
        assert!(document.undo());
        assert!(!document.is_dirty());
    }

    #[test]
    fn opening_resets_history_path_and_baseline() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("opened.mmd");
        fs::write(&path, "flowchart TD\n  A --> B").expect("source file");
        let mut document = Document::new("first".into());
        document.edit("second".into());
        document.edit("third".into());
        document.undo();
        document.load(&path).expect("load source");
        assert_eq!(document.source, "flowchart TD\n  A --> B");
        assert_eq!(document.path.as_deref(), Some(path.as_path()));
        assert!(!document.is_dirty());
        assert!(!document.can_undo());
        assert!(!document.can_redo());
        document.reset("new document".into());
        assert!(document.path.is_none());
        assert!(!document.is_dirty());
    }

    #[test]
    fn failed_open_preserves_edits_and_rejects_oversize_or_invalid_utf8() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("invalid.mmd");
        let mut document = Document::new("original".into());
        document.edit("unsaved".into());
        fs::write(&path, [0xff, 0xfe]).expect("invalid UTF-8 file");
        assert!(
            document
                .load(&path)
                .expect_err("reject UTF-8")
                .contains("UTF-8")
        );
        fs::write(&path, vec![b'a'; MAX_FILE_BYTES + 1]).expect("oversized file");
        assert!(
            document
                .load(&path)
                .expect_err("reject size")
                .contains("256 KiB")
        );
        assert_eq!(document.source, "unsaved");
        assert!(document.path.is_none());
        assert!(document.is_dirty());
        assert!(document.undo());
        assert_eq!(document.source, "original");
        fs::write(&path, vec![b'a'; MAX_FILE_BYTES]).expect("maximum-size file");
        document.load(&path).expect("allow exact limit");
        assert_eq!(document.source.len(), MAX_FILE_BYTES);
    }
}
