//! Benchmarks for the editor's hot paths, run with:
//!
//! ```text
//! cargo run --release -p collab-server --example bench
//! ```
//!
//! No extra dependencies: each case warms up, then times a fixed number
//! of iterations and prints the mean time per op. The numbers are for
//! comparing one change against another on the same machine, not for
//! comparing against other editors: the machine, the build, and the
//! document size all move them.
//!
//! The cases follow what a typing session actually does: inserts at the
//! end (typing) and in the middle (editing), deletes, formatting a
//! range, a stale client catching up through the transform, rendering
//! spans, and the disk writes (append + snapshot) that run on every op.

use collab_ot::{transform, Document, FormatChange, Op};
use collab_server::room::Room;
use collab_server::store::Store;
use std::env;
use std::time::Instant;

/// Time `f` over `iters` iterations and print the mean per iteration.
fn time_it(label: &str, iters: usize, mut f: impl FnMut()) {
    // Warm up so the first timed run is not paying for cold caches.
    for _ in 0..3 {
        f();
    }
    let start = Instant::now();
    for _ in 0..iters {
        f();
    }
    let per_op = start.elapsed() / iters as u32;
    println!("{label:<38} {per_op:>10.2?} per op");
}

/// A document of `chars` plain characters, built one insert at a time
/// the way a user types it.
fn doc_of(chars: usize) -> Document {
    let mut doc = Document::new();
    let chunk = "abcdefghij";
    let mut pos = 0;
    while pos < chars {
        let take = chunk.len().min(chars - pos);
        doc.apply(&Op::insert(1, pos, &chunk[..take])).unwrap();
        pos += take;
    }
    doc
}

fn bench_insert_at_end() {
    let mut doc = doc_of(10_000);
    let mut pos = doc.len();
    time_it("insert 1 char at end (10k doc)", 5_000, || {
        doc.apply(&Op::insert(1, pos, "x")).unwrap();
        pos += 1;
    });
}

fn bench_insert_in_middle() {
    let mut doc = doc_of(10_000);
    time_it("insert 1 char in middle (10k doc)", 5_000, || {
        doc.apply(&Op::insert(1, 5_000, "x")).unwrap();
        doc.apply(&Op::delete(1, 5_000, 1)).unwrap();
    });
}

fn bench_delete_in_middle() {
    let mut doc = doc_of(10_000);
    time_it("delete 1 char in middle (10k doc)", 5_000, || {
        doc.apply(&Op::delete(1, 5_000, 1)).unwrap();
        doc.apply(&Op::insert(1, 5_000, "x")).unwrap();
    });
}

fn bench_format_range() {
    let mut doc = doc_of(10_000);
    let mut bold = false;
    time_it("format 1000 chars (10k doc)", 1_000, || {
        bold = !bold;
        doc.apply(&Op::format(1, 1_000, 1_000, FormatChange::bold(bold)))
            .unwrap();
    });
}

fn bench_stale_client_catch_up() {
    // A client that missed 100 ops: its op is transformed past each
    // one in turn. The list of missed ops never changes, so every
    // rep measures the same work.
    let seen: Vec<Op> = (0..100).map(|i| Op::insert(1, i, "x")).collect();
    time_it("transform 1 op past 100 missed ops", 1_000, || {
        let mut op = Op::insert(2, 0, "y");
        for s in &seen {
            op = transform(&op, s);
        }
        std::hint::black_box(op);
    });
}

fn bench_spans() {
    let mut doc = doc_of(10_000);
    doc.apply(&Op::format(1, 0, 5_000, FormatChange::bold(true)))
        .unwrap();
    time_it("spans of 10k doc, half bold", 100, || {
        let spans = doc.spans();
        std::hint::black_box(spans);
    });
}

fn bench_len() {
    let doc = doc_of(100_000);
    time_it("doc len (100k doc)", 100_000, || {
        std::hint::black_box(doc.len());
    });
}

fn bench_typing_session() {
    let mut room = Room::new();
    let mut version = 0;
    let mut pos = 0;
    time_it("room apply, typing at end", 5_000, || {
        let (_, v) = room
            .apply(&Op::insert(1, pos, "x"), version)
            .unwrap()
            .unwrap();
        version = v;
        pos += 1;
    });
}

fn bench_store_append() {
    let dir = env::temp_dir().join(format!("collab-bench-{}", std::process::id()));
    let store = Store::new(&dir, "room");
    let mut version = 0;
    time_it("store append, one op (fsync)", 200, || {
        version += 1;
        store.append(version, &Op::insert(1, 0, "x")).unwrap();
    });
    let _ = std::fs::remove_dir_all(&dir);
}

fn bench_snapshot_round_trip() {
    let dir = env::temp_dir().join(format!("collab-bench-snap-{}", std::process::id()));
    let store = Store::new(&dir, "room");
    let doc = doc_of(10_000);
    for version in 1..=100 {
        store.append(version, &Op::insert(1, 0, "x")).unwrap();
    }
    time_it("snapshot save + load (10k doc)", 20, || {
        store.save_snapshot(100, doc.text(), doc.attrs()).unwrap();
        let restored = Store::new(&dir, "room").load().unwrap().unwrap();
        std::hint::black_box(restored.doc.text().len());
    });
    let _ = std::fs::remove_dir_all(&dir);
}

fn main() {
    println!("collaborative editor benchmarks (release build)");
    println!("{:-<62}", "");
    bench_insert_at_end();
    bench_insert_in_middle();
    bench_delete_in_middle();
    bench_format_range();
    bench_stale_client_catch_up();
    bench_spans();
    bench_len();
    bench_typing_session();
    bench_store_append();
    bench_snapshot_round_trip();
    println!("{:-<62}", "");
    println!("times are mean per op; lower is faster");
}
