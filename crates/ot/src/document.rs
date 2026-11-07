// A document is just its text. Positions count characters, not bytes,
// so one emoji is one position even though it takes four bytes in UTF-8.
// That is what an editor cursor sees, so ops use the same counting.
//
// The text is a plain String for now. A rope would make big edits faster,
// but it is not needed until documents get large.

use crate::op::Op;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Document {
    text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpError {
    InsertOutOfBounds {
        pos: usize,
        doc_len: usize,
    },
    DeleteOutOfBounds {
        pos: usize,
        len: usize,
        doc_len: usize,
    },
}

impl std::fmt::Display for OpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpError::InsertOutOfBounds { pos, doc_len } => {
                write!(f, "insert at {pos} past end of document ({doc_len} chars)")
            }
            OpError::DeleteOutOfBounds { pos, len, doc_len } => {
                write!(
                    f,
                    "delete of {len} chars at {pos} past end of document ({doc_len} chars)"
                )
            }
        }
    }
}

impl std::error::Error for OpError {}

impl Document {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_text(text: &str) -> Self {
        Self {
            text: text.to_string(),
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// Length in characters.
    pub fn len(&self) -> usize {
        self.text.chars().count()
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Applies one op, or rejects it when its position is out of bounds.
    /// A failed apply leaves the document unchanged.
    pub fn apply(&mut self, op: &Op) -> Result<(), OpError> {
        match op {
            Op::Insert { pos, text, .. } => {
                let byte = self.char_to_byte(*pos).ok_or(OpError::InsertOutOfBounds {
                    pos: *pos,
                    doc_len: self.len(),
                })?;
                self.text.insert_str(byte, text);
                Ok(())
            }
            Op::Delete { pos, len, .. } => {
                let start = self.char_to_byte(*pos).ok_or(OpError::DeleteOutOfBounds {
                    pos: *pos,
                    len: *len,
                    doc_len: self.len(),
                })?;
                let end = self
                    .char_to_byte(pos + len)
                    .ok_or(OpError::DeleteOutOfBounds {
                        pos: *pos,
                        len: *len,
                        doc_len: self.len(),
                    })?;
                self.text.replace_range(start..end, "");
                Ok(())
            }
        }
    }

    /// Byte offset of the pos-th character, or None when pos is past the end.
    fn char_to_byte(&self, pos: usize) -> Option<usize> {
        match self.text.char_indices().nth(pos) {
            Some((byte, _)) => Some(byte),
            None if pos == self.text.chars().count() => Some(self.text.len()),
            None => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_at_start_middle_and_end() {
        let mut doc = Document::from_text("bcd");
        doc.apply(&Op::insert(1, 0, "a")).unwrap();
        assert_eq!(doc.text(), "abcd");
        doc.apply(&Op::insert(1, 2, "X")).unwrap();
        assert_eq!(doc.text(), "abXcd");
        doc.apply(&Op::insert(1, doc.len(), "!")).unwrap();
        assert_eq!(doc.text(), "abXcd!");
    }

    #[test]
    fn insert_past_the_end_fails_and_changes_nothing() {
        let mut doc = Document::from_text("hi");
        let err = doc.apply(&Op::insert(1, 3, "x")).unwrap_err();
        assert_eq!(err, OpError::InsertOutOfBounds { pos: 3, doc_len: 2 });
        assert_eq!(doc.text(), "hi");
    }

    #[test]
    fn delete_middle_end_and_all() {
        let mut doc = Document::from_text("hello world");
        doc.apply(&Op::delete(1, 5, 1)).unwrap();
        assert_eq!(doc.text(), "helloworld");
        doc.apply(&Op::delete(1, 9, 1)).unwrap();
        assert_eq!(doc.text(), "helloworl");
        doc.apply(&Op::delete(1, 0, doc.len())).unwrap();
        assert_eq!(doc.text(), "");
        assert!(doc.is_empty());
    }

    #[test]
    fn delete_past_the_end_fails_and_changes_nothing() {
        let mut doc = Document::from_text("hi");
        let err = doc.apply(&Op::delete(1, 1, 2)).unwrap_err();
        assert_eq!(
            err,
            OpError::DeleteOutOfBounds {
                pos: 1,
                len: 2,
                doc_len: 2
            }
        );
        assert_eq!(doc.text(), "hi");
    }

    #[test]
    fn positions_count_characters_not_bytes() {
        let mut doc = Document::from_text("a\u{1F600}b");
        assert_eq!(doc.len(), 3);
        doc.apply(&Op::insert(1, 2, "X")).unwrap();
        assert_eq!(doc.text(), "a\u{1F600}Xb");
        doc.apply(&Op::delete(1, 1, 1)).unwrap();
        assert_eq!(doc.text(), "aXb");
    }

    #[test]
    fn a_sequence_of_ops_builds_a_document() {
        let mut doc = Document::new();
        for word in ["hello", " ", "world"] {
            doc.apply(&Op::insert(1, doc.len(), word)).unwrap();
        }
        assert_eq!(doc.text(), "hello world");
        doc.apply(&Op::delete(1, 5, 6)).unwrap();
        doc.apply(&Op::insert(1, 5, " rust")).unwrap();
        assert_eq!(doc.text(), "hello rust");
    }

    #[test]
    fn empty_ops_are_noops() {
        let mut doc = Document::from_text("abc");
        doc.apply(&Op::insert(1, 1, "")).unwrap();
        doc.apply(&Op::delete(1, 1, 0)).unwrap();
        assert_eq!(doc.text(), "abc");
    }
}
