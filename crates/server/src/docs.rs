//! The documents list: one row per room, built from what is on disk.
//!
//! A document's title is its first line, so a room needs no separate
//! name to show up in the list, and its preview is the start of the
//! text with the line breaks flattened. The list is sorted newest
//! first by when the room was last saved; a room that has never been
//! written to disk sorts last.

use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::store::Store;

/// One row of the documents list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocInfo {
    pub id: String,
    pub title: String,
    pub preview: String,
    pub version: usize,
    /// Seconds since the Unix epoch when the room was last saved,
    /// or null when it has never been written to disk.
    pub updated_at: Option<u64>,
}

/// How many characters of the first line the title keeps.
const TITLE_LEN: usize = 60;

/// How many characters of the text the preview keeps.
const PREVIEW_LEN: usize = 120;

fn truncate(text: &str, max: usize) -> String {
    let mut chars = text.chars();
    let kept: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() {
        format!("{kept}…")
    } else {
        kept
    }
}

/// The first line of the text, trimmed; the room id when the text is
/// empty, so an empty document still has something to show.
pub fn title_of(text: &str, fallback: &str) -> String {
    let first = text.lines().next().unwrap_or("").trim();
    if first.is_empty() {
        fallback.to_string()
    } else {
        truncate(first, TITLE_LEN)
    }
}

/// The start of the text with line breaks flattened to spaces.
pub fn preview_of(text: &str) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect();
    truncate(flat.trim(), PREVIEW_LEN)
}

fn unix_secs(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// One row from a room's current text, version, and last save time.
pub fn from_room(
    id: &str,
    text: &str,
    version: usize,
    updated_at: Option<SystemTime>,
) -> DocInfo {
    DocInfo {
        id: id.to_string(),
        title: title_of(text, id),
        preview: preview_of(text),
        version,
        updated_at: updated_at.map(unix_secs),
    }
}

/// Newest first by last save; rooms never saved sort last, then by id
/// so the order is stable when two rooms share a save time.
pub fn sort_docs(docs: &mut [DocInfo]) {
    docs.sort_by(|a, b| {
        // Option orders None below every Some, so comparing b to a
        // puts saved rooms first, newest first, and unsaved ones last.
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| a.id.cmp(&b.id))
    });
}

/// Every room with files under `data_dir`, newest first. A directory
/// that does not load — a corrupt log, say — is skipped rather than
/// failing the whole list.
pub fn scan(data_dir: &Path) -> Vec<DocInfo> {
    let mut docs = Vec::new();
    let Ok(entries) = fs::read_dir(data_dir) else {
        return docs;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if !kind.is_dir() {
            continue;
        }
        let id = entry.file_name().to_string_lossy().into_owned();
        let store = Store::new(data_dir, &id);
        let Ok(Some(restored)) = store.load() else {
            continue;
        };
        docs.push(from_room(
            &id,
            restored.doc.text(),
            restored.base + restored.history.len(),
            store.updated_at(),
        ));
    }
    sort_docs(&mut docs);
    docs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
    use collab_ot::Op;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn temp_dir() -> std::path::PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir().join(format!("collab-docs-test-{}-{n}", std::process::id()))
    }

    #[test]
    fn title_is_the_first_line() {
        assert_eq!(title_of("Shopping list\nmilk\neggs", "room1"), "Shopping list");
    }

    #[test]
    fn empty_text_falls_back_to_the_room_id() {
        assert_eq!(title_of("", "room1"), "room1");
        assert_eq!(title_of("\n\n", "room1"), "room1");
    }

    #[test]
    fn long_titles_are_cut_off() {
        let title = title_of(&"a".repeat(100), "room1");
        assert_eq!(title.chars().count(), TITLE_LEN + 1);
        assert!(title.ends_with('…'));
    }

    #[test]
    fn preview_flattens_line_breaks_and_truncates() {
        assert_eq!(preview_of("one\ntwo\nthree"), "one two three");
        let preview = preview_of(&"word ".repeat(50));
        assert_eq!(preview.chars().count(), PREVIEW_LEN + 1);
        assert!(preview.ends_with('…'));
    }

    #[test]
    fn scan_finds_rooms_on_disk_newest_first() {
        let dir = temp_dir();
        // "zeta" is created first, "alpha" second. The names are picked
        // so the id tie-break agrees with the creation order: whether
        // the two saves land in the same second or not, "alpha" — the
        // newer room — sorts first either way.
        let a = Store::new(&dir, "zeta");
        a.append(1, &Op::insert(1, 0, "Zeta doc\nfirst")).unwrap();
        let b = Store::new(&dir, "alpha");
        b.append(1, &Op::insert(1, 0, "Alpha doc")).unwrap();
        b.append(2, &Op::insert(1, 9, "!")).unwrap();

        let docs = scan(&dir);
        assert_eq!(docs.len(), 2);
        assert_eq!(docs[0].id, "alpha");
        // The "!" landed on the first line, so it is in the title too.
        assert_eq!(docs[0].title, "Alpha doc!");
        assert_eq!(docs[0].preview, "Alpha doc!");
        assert_eq!(docs[0].version, 2);
        assert!(docs[0].updated_at.is_some());
        assert_eq!(docs[1].id, "zeta");
        assert_eq!(docs[1].title, "Zeta doc");
        assert_eq!(docs[1].preview, "Zeta doc first");
        assert_eq!(docs[1].version, 1);
    }

    #[test]
    fn scan_skips_rooms_that_do_not_load() {
        let dir = temp_dir();
        let good = Store::new(&dir, "good");
        good.append(1, &Op::insert(1, 0, "ok")).unwrap();
        // A directory with a log that jumps versions, so loading fails.
        let bad = Store::new(&dir, "bad");
        bad.append(1, &Op::insert(1, 0, "a")).unwrap();
        bad.append(3, &Op::insert(1, 1, "b")).unwrap();

        let docs = scan(&dir);
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].id, "good");
    }

    #[test]
    fn scan_of_a_missing_directory_is_empty() {
        assert!(scan(&temp_dir().join("no-such-dir")).is_empty());
    }

    #[test]
    fn unsaved_rooms_sort_last() {
        let mut docs = vec![
            from_room("a", "x", 1, None),
            from_room("b", "y", 1, Some(UNIX_EPOCH)),
            from_room("c", "z", 1, None),
        ];
        sort_docs(&mut docs);
        assert_eq!(docs[0].id, "b");
        assert_eq!(docs[1].id, "a");
        assert_eq!(docs[2].id, "c");
    }
}
