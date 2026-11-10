// On-disk persistence for one room: a snapshot plus an op log.
//
// The store keeps two files per room under the data directory:
//
// - `snapshot.json`: the document text at some version, written every
//   so often so the log does not grow forever.
// - `oplog.jsonl`: one JSON line per op applied after that snapshot,
//   each line carrying the version the op produced.
//
// Loading is snapshot first, then replay the log lines in order. The
// log is append-only while the server runs; each append is flushed to
// disk before the op is broadcast, so a crash loses nothing the clients
// already saw. When a snapshot is saved, log lines at or below the
// snapshot version are dropped, since the snapshot already holds them.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use collab_ot::{Document, Op};
use serde::{Deserialize, Serialize};

/// One line of the op log: the op and the version it produced.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct LogEntry {
    version: usize,
    op: Op,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Snapshot {
    version: usize,
    text: String,
}

/// What loading a room from disk gives back: the document, the version
/// the snapshot was at (the start of the returned history), and the ops
/// applied since that snapshot.
#[derive(Debug)]
pub struct Restored {
    pub doc: Document,
    pub base: usize,
    pub history: Vec<Op>,
}

#[derive(Debug, Clone)]
pub struct Store {
    dir: PathBuf,
}

impl Store {
    /// A store for `room_id` under `data_dir`. The room id is sanitised
    /// into a directory name, so a client cannot escape the data dir
    /// with a name like `../elsewhere`.
    pub fn new(data_dir: impl AsRef<Path>, room_id: &str) -> Self {
        let safe: String = room_id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        Self {
            dir: data_dir.as_ref().join(safe),
        }
    }

    fn snapshot_path(&self) -> PathBuf {
        self.dir.join("snapshot.json")
    }

    fn log_path(&self) -> PathBuf {
        self.dir.join("oplog.jsonl")
    }

    /// Loads the room, if anything was ever saved for it. Returns `None`
    /// when the room has no files on disk yet.
    pub fn load(&self) -> io::Result<Option<Restored>> {
        let snapshot = match fs::read_to_string(self.snapshot_path()) {
            Ok(text) => serde_json::from_str::<Snapshot>(&text)
                .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?,
            Err(err) if err.kind() == io::ErrorKind::NotFound => Snapshot {
                version: 0,
                text: String::new(),
            },
            Err(err) => return Err(err),
        };

        let mut doc = Document::from_text(&snapshot.text);
        let mut history = Vec::new();
        let mut version = snapshot.version;

        if let Ok(file) = File::open(self.log_path()) {
            for line in BufReader::new(file).lines() {
                let line = line?;
                if line.trim().is_empty() {
                    continue;
                }
                let entry: LogEntry = serde_json::from_str(&line)
                    .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
                if entry.version != version + 1 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "op log for {} jumps from version {version} to {}",
                            self.dir.display(),
                            entry.version
                        ),
                    ));
                }
                doc.apply(&entry.op)
                    .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))?;
                version = entry.version;
                history.push(entry.op);
            }
        }

        if snapshot.version == 0 && history.is_empty() {
            return Ok(None);
        }
        Ok(Some(Restored {
            doc,
            base: snapshot.version,
            history,
        }))
    }

    /// Appends one op to the log and flushes it to disk.
    pub fn append(&self, version: usize, op: &Op) -> io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.log_path())?;
        let line = serde_json::to_string(&LogEntry {
            version,
            op: op.clone(),
        })?;
        writeln!(file, "{line}")?;
        file.sync_all()
    }

    /// Writes the snapshot for `version` and drops the log lines it
    /// covers, so the log only ever holds ops after the snapshot.
    pub fn save_snapshot(&self, version: usize, text: &str) -> io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        let snapshot = Snapshot {
            version,
            text: text.to_string(),
        };
        let tmp = self.dir.join("snapshot.json.tmp");
        fs::write(&tmp, serde_json::to_string(&snapshot)?)?;
        fs::rename(&tmp, self.snapshot_path())?;
        self.truncate_log(version)
    }

    /// Rewrites the log keeping only entries after `version`.
    fn truncate_log(&self, version: usize) -> io::Result<()> {
        let path = self.log_path();
        let kept: Vec<String> = match File::open(&path) {
            Ok(file) => BufReader::new(file)
                .lines()
                .collect::<io::Result<Vec<_>>>()?
                .into_iter()
                .filter(|line| {
                    serde_json::from_str::<LogEntry>(line)
                        .map(|entry| entry.version > version)
                        .unwrap_or(true)
                })
                .collect(),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(err) => return Err(err),
        };
        let mut file = File::create(&path)?;
        for line in kept {
            writeln!(file, "{line}")?;
        }
        file.sync_all()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn temp_dir() -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir().join(format!("collab-store-test-{}-{n}", std::process::id()))
    }

    #[test]
    fn empty_store_loads_nothing() {
        let store = Store::new(temp_dir(), "room1");
        assert!(store.load().unwrap().is_none());
    }

    #[test]
    fn append_and_load_replays_the_log() {
        let dir = temp_dir();
        let store = Store::new(&dir, "room1");
        store.append(1, &Op::insert(1, 0, "hello")).unwrap();
        store.append(2, &Op::insert(1, 5, " world")).unwrap();

        let restored = Store::new(&dir, "room1").load().unwrap().unwrap();
        assert_eq!(restored.doc.text(), "hello world");
        assert_eq!(restored.base, 0);
        assert_eq!(restored.history.len(), 2);
        assert_eq!(restored.history[0], Op::insert(1, 0, "hello"));
    }

    #[test]
    fn snapshot_then_log_replays_only_what_follows() {
        let dir = temp_dir();
        let store = Store::new(&dir, "room1");
        store.append(1, &Op::insert(1, 0, "abc")).unwrap();
        store.append(2, &Op::insert(1, 3, "def")).unwrap();
        store.save_snapshot(2, "abcdef").unwrap();
        store.append(3, &Op::insert(1, 6, "!")).unwrap();

        let restored = Store::new(&dir, "room1").load().unwrap().unwrap();
        assert_eq!(restored.doc.text(), "abcdef!");
        assert_eq!(restored.base, 2);
        assert_eq!(restored.history, vec![Op::insert(1, 6, "!")]);
    }

    #[test]
    fn snapshot_drops_covered_log_lines() {
        let dir = temp_dir();
        let store = Store::new(&dir, "room1");
        store.append(1, &Op::insert(1, 0, "abc")).unwrap();
        store.save_snapshot(1, "abc").unwrap();

        let log = fs::read_to_string(store.log_path()).unwrap();
        assert_eq!(log, "");
        assert!(Store::new(&dir, "room1").load().unwrap().is_some());
    }

    #[test]
    fn room_id_cannot_escape_the_data_dir() {
        let dir = temp_dir();
        let store = Store::new(&dir, "../escape");
        store.append(1, &Op::insert(1, 0, "x")).unwrap();
        assert!(dir.join(".._escape").exists());
        assert!(!dir.join("escape").exists());
    }

    #[test]
    fn log_with_a_gap_is_an_error() {
        let dir = temp_dir();
        let store = Store::new(&dir, "room1");
        store.append(1, &Op::insert(1, 0, "a")).unwrap();
        // Hand-edit the log so the next version jumps from 1 to 3.
        let mut file = OpenOptions::new()
            .append(true)
            .open(store.log_path())
            .unwrap();
        writeln!(
            file,
            "{}",
            serde_json::to_string(&LogEntry {
                version: 3,
                op: Op::insert(1, 1, "b"),
            })
            .unwrap()
        )
        .unwrap();
        drop(file);
        assert!(Store::new(&dir, "room1").load().is_err());
    }
}
