// The operations any editor in a session can make on a document.
//
// An op carries the site (client) it came from. Day 1 does not use it yet;
// day 2's transform needs it to break ties when two sites insert at the
// same position.
//
// Day 9 adds formatting: an insert carries the attributes of the text it
// adds, and a format op changes the attributes of a range that is already
// there. Attributes travel with the text, so a bold word stays bold when
// someone else types next to it.

use crate::attrs::{Attrs, FormatChange};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Op {
    Insert {
        site: u64,
        pos: usize,
        text: String,
        /// The formatting of the inserted text. Skipped when plain, so
        /// the JSON for ordinary typing stays exactly as it was, and
        /// an insert from an older client without this field reads as
        /// plain text.
        #[serde(default, skip_serializing_if = "Attrs::is_plain")]
        attrs: Attrs,
    },
    Delete {
        site: u64,
        pos: usize,
        len: usize,
    },
    Format {
        site: u64,
        pos: usize,
        len: usize,
        change: FormatChange,
    },
}

impl Op {
    pub fn insert(site: u64, pos: usize, text: impl Into<String>) -> Self {
        Self::insert_styled(site, pos, text, Attrs::plain())
    }

    pub fn insert_styled(
        site: u64,
        pos: usize,
        text: impl Into<String>,
        attrs: Attrs,
    ) -> Self {
        Op::Insert {
            site,
            pos,
            text: text.into(),
            attrs,
        }
    }

    pub fn delete(site: u64, pos: usize, len: usize) -> Self {
        Op::Delete { site, pos, len }
    }

    pub fn format(site: u64, pos: usize, len: usize, change: FormatChange) -> Self {
        Op::Format {
            site,
            pos,
            len,
            change,
        }
    }

    pub fn site(&self) -> u64 {
        match self {
            Op::Insert { site, .. } | Op::Delete { site, .. } | Op::Format { site, .. } => {
                *site
            }
        }
    }

    /// An op that changes nothing: an empty insert, a zero-length
    /// delete, or a format over no characters.
    pub fn is_noop(&self) -> bool {
        match self {
            Op::Insert { text, .. } => text.is_empty(),
            Op::Delete { len, .. } => *len == 0,
            Op::Format { len, .. } => *len == 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructors_and_site() {
        let ins = Op::insert(3, 0, "hi");
        assert_eq!(ins.site(), 3);
        assert_eq!(
            ins,
            Op::Insert {
                site: 3,
                pos: 0,
                text: "hi".to_string(),
                attrs: Attrs::plain(),
            }
        );

        let del = Op::delete(7, 2, 4);
        assert_eq!(del.site(), 7);
        assert_eq!(
            del,
            Op::Delete {
                site: 7,
                pos: 2,
                len: 4
            }
        );

        let fmt = Op::format(5, 1, 2, FormatChange::bold(true));
        assert_eq!(fmt.site(), 5);
        assert_eq!(
            fmt,
            Op::Format {
                site: 5,
                pos: 1,
                len: 2,
                change: FormatChange::bold(true),
            }
        );
    }

    #[test]
    fn noop_detection() {
        assert!(Op::insert(1, 0, "").is_noop());
        assert!(Op::delete(1, 0, 0).is_noop());
        assert!(Op::format(1, 0, 0, FormatChange::bold(true)).is_noop());
        assert!(!Op::insert(1, 0, "x").is_noop());
        assert!(!Op::delete(1, 0, 1).is_noop());
        assert!(!Op::format(1, 0, 1, FormatChange::bold(true)).is_noop());
    }

    #[test]
    fn plain_insert_keeps_the_old_json_shape() {
        // Older clients and logs carry no attrs field; a plain insert
        // still serializes without one, and reads back the same.
        let json = serde_json::to_string(&Op::insert(1, 0, "hi")).unwrap();
        assert_eq!(json, r#"{"Insert":{"site":1,"pos":0,"text":"hi"}}"#);
        let back: Op = serde_json::from_str(&json).unwrap();
        assert_eq!(back, Op::insert(1, 0, "hi"));

        let styled = Op::insert_styled(1, 0, "hi", Attrs::bold());
        let json = serde_json::to_string(&styled).unwrap();
        assert_eq!(
            json,
            r#"{"Insert":{"site":1,"pos":0,"text":"hi","attrs":{"bold":true,"italic":false,"link":null}}}"#
        );
        let back: Op = serde_json::from_str(&json).unwrap();
        assert_eq!(back, styled);
    }
}
