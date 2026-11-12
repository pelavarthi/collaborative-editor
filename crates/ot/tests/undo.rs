// Undo and redo, including against edits from other people.
//
// Each test drives one document plus a History the way a client would:
// record each local edit before applying it, note each remote op as it
// lands, and apply whatever undo or redo returns.

use collab_ot::{Document, History, Op};

/// Applies `op` to `doc` and records it in `history` first, as a client
/// does for its own edits.
fn edit(history: &mut History, doc: &mut Document, op: &Op) {
    history.record(op, doc);
    doc.apply(op).unwrap();
}

/// Applies a remote op to `doc` and notes it in `history`.
fn remote(history: &mut History, doc: &mut Document, op: &Op) {
    history.note_remote(op);
    doc.apply(op).unwrap();
}

#[test]
fn undo_of_an_insert_deletes_it_and_redo_puts_it_back() {
    let mut doc = Document::from_text("hello");
    let mut history = History::new();

    edit(&mut history, &mut doc, &Op::insert(1, 5, " world"));
    assert_eq!(doc.text(), "hello world");

    let undone = history.undo(&doc).unwrap();
    assert_eq!(undone, Op::delete(1, 5, 6));
    doc.apply(&undone).unwrap();
    assert_eq!(doc.text(), "hello");
    assert!(history.can_redo());

    let redone = history.redo(&doc).unwrap();
    assert_eq!(redone, Op::insert(1, 5, " world"));
    doc.apply(&redone).unwrap();
    assert_eq!(doc.text(), "hello world");
    assert!(!history.can_redo());
}

#[test]
fn undo_of_a_delete_restores_the_deleted_text() {
    let mut doc = Document::from_text("hello world");
    let mut history = History::new();

    edit(&mut history, &mut doc, &Op::delete(1, 5, 6));
    assert_eq!(doc.text(), "hello");

    let undone = history.undo(&doc).unwrap();
    assert_eq!(undone, Op::insert(1, 5, " world"));
    doc.apply(&undone).unwrap();
    assert_eq!(doc.text(), "hello world");

    let redone = history.redo(&doc).unwrap();
    assert_eq!(redone, Op::delete(1, 5, 6));
    doc.apply(&redone).unwrap();
    assert_eq!(doc.text(), "hello");
}

#[test]
fn undo_after_a_remote_insert_shifts_to_the_right_text() {
    // I type X in the middle; meanwhile you insert Y at the start.
    // Undo must delete the X, not the Y.
    let mut doc = Document::from_text("ab");
    let mut history = History::new();

    edit(&mut history, &mut doc, &Op::insert(1, 1, "X"));
    remote(&mut history, &mut doc, &Op::insert(2, 0, "Y"));
    assert_eq!(doc.text(), "YaXb");

    let undone = history.undo(&doc).unwrap();
    assert_eq!(undone, Op::delete(1, 2, 1));
    doc.apply(&undone).unwrap();
    assert_eq!(doc.text(), "Yab");
}

#[test]
fn undo_of_a_delete_after_a_remote_insert_puts_the_text_back_in_place() {
    // I delete "bcd"; you insert Z at the start in the meantime.
    let mut doc = Document::from_text("abcde");
    let mut history = History::new();

    edit(&mut history, &mut doc, &Op::delete(1, 1, 3));
    assert_eq!(doc.text(), "ae");
    remote(&mut history, &mut doc, &Op::insert(2, 0, "Z"));
    assert_eq!(doc.text(), "Zae");

    let undone = history.undo(&doc).unwrap();
    assert_eq!(undone, Op::insert(1, 2, "bcd"));
    doc.apply(&undone).unwrap();
    assert_eq!(doc.text(), "Zabcde");
}

#[test]
fn undo_of_an_insert_already_deleted_remotely_is_a_harmless_noop() {
    // I insert X; you delete that same X before I hit undo. There is
    // nothing left to undo, so the undo op must change nothing.
    let mut doc = Document::from_text("ab");
    let mut history = History::new();

    edit(&mut history, &mut doc, &Op::insert(1, 1, "X"));
    remote(&mut history, &mut doc, &Op::delete(2, 1, 1));
    assert_eq!(doc.text(), "ab");

    let undone = history.undo(&doc).unwrap();
    assert!(undone.is_noop());
    doc.apply(&undone).unwrap();
    assert_eq!(doc.text(), "ab");
}

#[test]
fn redo_after_a_remote_edit_lands_in_the_right_spot() {
    // I insert X, undo it, then you insert Y at the start. Redo must
    // put the X back after the Y, not where it was before the undo.
    let mut doc = Document::from_text("ab");
    let mut history = History::new();

    edit(&mut history, &mut doc, &Op::insert(1, 1, "X"));
    let undone = history.undo(&doc).unwrap();
    doc.apply(&undone).unwrap();
    assert_eq!(doc.text(), "ab");

    remote(&mut history, &mut doc, &Op::insert(2, 0, "Y"));
    assert_eq!(doc.text(), "Yab");

    let redone = history.redo(&doc).unwrap();
    assert_eq!(redone, Op::insert(1, 2, "X"));
    doc.apply(&redone).unwrap();
    assert_eq!(doc.text(), "YaXb");

    // And undo still works after the redo.
    let undone = history.undo(&doc).unwrap();
    doc.apply(&undone).unwrap();
    assert_eq!(doc.text(), "Yab");
}

#[test]
fn two_undos_then_redos_walk_back_and_forward_in_order() {
    let mut doc = Document::from_text("");
    let mut history = History::new();

    edit(&mut history, &mut doc, &Op::insert(1, 0, "one "));
    edit(&mut history, &mut doc, &Op::insert(1, 4, "two"));
    assert_eq!(doc.text(), "one two");

    let undone = history.undo(&doc).unwrap();
    doc.apply(&undone).unwrap();
    assert_eq!(doc.text(), "one ");
    let undone = history.undo(&doc).unwrap();
    doc.apply(&undone).unwrap();
    assert_eq!(doc.text(), "");
    assert!(!history.can_undo());

    let redone = history.redo(&doc).unwrap();
    doc.apply(&redone).unwrap();
    assert_eq!(doc.text(), "one ");
    let redone = history.redo(&doc).unwrap();
    doc.apply(&redone).unwrap();
    assert_eq!(doc.text(), "one two");
    assert!(!history.can_redo());
}

#[test]
fn a_new_edit_after_undo_clears_redo() {
    let mut doc = Document::from_text("abc");
    let mut history = History::new();

    edit(&mut history, &mut doc, &Op::insert(1, 3, "d"));
    let undone = history.undo(&doc).unwrap();
    doc.apply(&undone).unwrap();
    assert!(history.can_redo());

    edit(&mut history, &mut doc, &Op::insert(1, 3, "e"));
    assert_eq!(doc.text(), "abce");
    assert!(!history.can_redo());
    assert!(history.redo(&doc).is_none());
}

#[test]
fn undo_with_nothing_to_undo_returns_none() {
    let mut history = History::new();
    let doc = Document::new();
    assert!(!history.can_undo());
    assert!(history.undo(&doc).is_none());
    assert!(history.redo(&doc).is_none());
}

#[test]
fn undo_is_capped_so_old_edits_fall_off() {
    let mut doc = Document::new();
    let mut history = History::new();

    for _ in 0..105 {
        let op = Op::insert(1, doc.len(), "x");
        edit(&mut history, &mut doc, &op);
    }
    assert_eq!(history.undo_len(), 100);

    // The 100 kept edits still undo cleanly, newest first.
    for _ in 0..100 {
        let undone = history.undo(&doc).unwrap();
        doc.apply(&undone).unwrap();
    }
    assert_eq!(doc.text(), "xxxxx");
    assert!(!history.can_undo());
}
