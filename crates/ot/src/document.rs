// A document is its text plus one set of attributes per character.
// Positions count characters, not bytes, so one emoji is one position
// even though it takes four bytes in UTF-8. That is what an editor
// cursor sees, so ops use the same counting.
//
// The attributes live in a vector parallel to the text: `attrs[i]` is
// the formatting of the i-th character. An insert splices attributes
// in with the text, a delete takes both out, and a format op rewrites
// the attributes of a range without touching the text.
//
// The text is a plain String for now. A rope would make big edits faster,
// but it is not needed until documents get large.

use crate::attrs::Attrs;
use crate::op::Op;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Document {
    text: String,
    attrs: Vec<Attrs>,
    /// Character count, kept in step with the text. `len()` and the
    /// end-of-document check in `char_to_byte` are O(1) because of
    /// it: without it every keystroke re-counted every character in
    /// the document, so typing got slower as the document grew.
    len: usize,
}

/// One run of equally formatted text, for rendering: the text and the
/// attributes every character in it shares.
pub type Span = (String, Attrs);

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
    FormatOutOfBounds {
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
            OpError::FormatOutOfBounds { pos, len, doc_len } => {
                write!(
                    f,
                    "format of {len} chars at {pos} past end of document ({doc_len} chars)"
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
        let len = text.chars().count();
        let attrs = vec![Attrs::plain(); len];
        Self {
            text: text.to_string(),
            attrs,
            len,
        }
    }

    /// Rebuilds a document from text and per-character attributes, as
    /// a snapshot stores them. A short attribute list is padded with
    /// plain attributes; a long one is cut to the text.
    pub fn from_parts(text: &str, attrs: Vec<Attrs>) -> Self {
        let mut attrs = attrs;
        let len = text.chars().count();
        attrs.truncate(len);
        attrs.resize_with(len, Attrs::plain);
        Self {
            text: text.to_string(),
            attrs,
            len,
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// The formatting of each character, in order.
    pub fn attrs(&self) -> &[Attrs] {
        &self.attrs
    }

    /// The text as runs of equally formatted characters, for rendering.
    /// An empty document has no spans; plain text is one span.
    pub fn spans(&self) -> Vec<Span> {
        let mut spans: Vec<Span> = Vec::new();
        for (ch, attrs) in self.text.chars().zip(self.attrs.iter()) {
            match spans.last_mut() {
                Some((text, last)) if *last == *attrs => text.push(ch),
                _ => spans.push((ch.to_string(), attrs.clone())),
            }
        }
        spans
    }

    /// Length in characters. O(1): the count is cached, not re-counted.
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Applies one op, or rejects it when its position is out of bounds.
    /// A failed apply leaves the document unchanged.
    pub fn apply(&mut self, op: &Op) -> Result<(), OpError> {
        match op {
            Op::Insert {
                pos, text, attrs, ..
            } => {
                let byte = self.char_to_byte(*pos).ok_or(OpError::InsertOutOfBounds {
                    pos: *pos,
                    doc_len: self.len(),
                })?;
                let added = text.chars().count();
                self.text.insert_str(byte, text);
                let inserted: Vec<Attrs> = vec![attrs.clone(); added];
                self.attrs.splice(*pos..*pos, inserted);
                self.len += added;
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
                self.attrs.drain(*pos..*pos + *len);
                self.len -= *len;
                Ok(())
            }
            Op::Format {
                pos, len, change, ..
            } => {
                if pos + len > self.len() {
                    return Err(OpError::FormatOutOfBounds {
                        pos: *pos,
                        len: *len,
                        doc_len: self.len(),
                    });
                }
                for attrs in &mut self.attrs[*pos..*pos + *len] {
                    *attrs = change.apply_to(attrs);
                }
                Ok(())
            }
        }
    }

    /// Byte offset of the pos-th character, or None when pos is past the end.
    /// The two positions typing hits most, the start and the end, are
    /// answered without scanning: appending to a long document used to
    /// walk the whole string just to find its end. Anywhere else still
    /// scans from the start, so a mid-document edit in a long document
    /// is still O(pos); a rope would fix that, this does not.
    fn char_to_byte(&self, pos: usize) -> Option<usize> {
        if pos == 0 {
            return Some(0);
        }
        if pos == self.len {
            return Some(self.text.len());
        }
        if pos > self.len {
            return None;
        }
        self.text.char_indices().nth(pos).map(|(byte, _)| byte)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attrs::FormatChange;

    #[test]
    fn cached_length_stays_right_across_edits() {
        let mut doc = Document::from_text("a\u{1F600}b");
        assert_eq!(doc.len(), 3);
        doc.apply(&Op::insert(1, 3, "xy")).unwrap();
        assert_eq!(doc.len(), 5);
        doc.apply(&Op::delete(1, 1, 2)).unwrap();
        assert_eq!(doc.len(), 3);
        assert_eq!(doc.text(), "axy");
        doc.apply(&Op::format(1, 0, 2, FormatChange::bold(true)))
            .unwrap();
        assert_eq!(doc.len(), 3);
        // A failed apply leaves the length alone too.
        assert!(doc.apply(&Op::insert(1, 9, "z")).is_err());
        assert_eq!(doc.len(), 3);
    }

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

    #[test]
    fn format_bolds_a_range_and_leaves_the_rest_plain() {
        let mut doc = Document::from_text("hello world");
        doc.apply(&Op::format(1, 0, 5, FormatChange::bold(true)))
            .unwrap();
        assert_eq!(doc.text(), "hello world");
        assert_eq!(
            doc.spans(),
            vec![
                ("hello".to_string(), Attrs::bold()),
                (" world".to_string(), Attrs::plain()),
            ]
        );
    }

    #[test]
    fn format_changes_only_what_it_names() {
        let mut doc = Document::from_text("hello");
        doc.apply(&Op::format(1, 0, 5, FormatChange::bold(true)))
            .unwrap();
        doc.apply(&Op::format(1, 1, 3, FormatChange::italic(true)))
            .unwrap();
        // The middle is bold and italic; the ends are only bold.
        let mut italic = Attrs::bold();
        italic.italic = true;
        assert_eq!(
            doc.spans(),
            vec![
                ("h".to_string(), Attrs::bold()),
                ("ell".to_string(), italic),
                ("o".to_string(), Attrs::bold()),
            ]
        );
    }

    #[test]
    fn format_can_add_and_remove_a_link() {
        let mut doc = Document::from_text("click here");
        doc.apply(&Op::format(
            1,
            0,
            5,
            FormatChange::link(Some("https://example.com".into())),
        ))
        .unwrap();
        assert_eq!(doc.attrs()[0].link.as_deref(), Some("https://example.com"));
        assert_eq!(doc.attrs()[6].link, None);
        doc.apply(&Op::format(1, 0, 5, FormatChange::link(None)))
            .unwrap();
        assert!(doc.attrs().iter().all(|a| a.link.is_none()));
    }

    #[test]
    fn format_past_the_end_fails_and_changes_nothing() {
        let mut doc = Document::from_text("hi");
        let err = doc
            .apply(&Op::format(1, 1, 2, FormatChange::bold(true)))
            .unwrap_err();
        assert_eq!(
            err,
            OpError::FormatOutOfBounds {
                pos: 1,
                len: 2,
                doc_len: 2
            }
        );
        assert!(doc.attrs().iter().all(|a| a.is_plain()));
    }

    #[test]
    fn insert_carries_its_attributes_and_delete_drops_them() {
        let mut doc = Document::from_text("ab");
        doc.apply(&Op::insert_styled(1, 1, "X", Attrs::bold()))
            .unwrap();
        assert_eq!(doc.text(), "aXb");
        assert_eq!(doc.attrs()[1], Attrs::bold());
        assert_eq!(doc.attrs()[2], Attrs::plain());
        doc.apply(&Op::delete(1, 1, 1)).unwrap();
        assert_eq!(doc.text(), "ab");
        assert!(doc.attrs().iter().all(|a| a.is_plain()));
    }

    #[test]
    fn spans_merge_equal_runs_and_skip_nothing() {
        let doc = Document::from_parts("aab", vec![Attrs::bold(), Attrs::bold(), Attrs::plain()]);
        assert_eq!(
            doc.spans(),
            vec![
                ("aa".to_string(), Attrs::bold()),
                ("b".to_string(), Attrs::plain()),
            ]
        );
        assert_eq!(Document::new().spans(), Vec::<Span>::new());
    }
}
