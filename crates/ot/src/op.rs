// The operations any editor in a session can make on a document.
//
// An op carries the site (client) it came from. Day 1 does not use it yet;
// day 2's transform needs it to break ties when two sites insert at the
// same position.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Op {
    Insert { site: u64, pos: usize, text: String },
    Delete { site: u64, pos: usize, len: usize },
}

impl Op {
    pub fn insert(site: u64, pos: usize, text: impl Into<String>) -> Self {
        Op::Insert {
            site,
            pos,
            text: text.into(),
        }
    }

    pub fn delete(site: u64, pos: usize, len: usize) -> Self {
        Op::Delete { site, pos, len }
    }

    pub fn site(&self) -> u64 {
        match self {
            Op::Insert { site, .. } | Op::Delete { site, .. } => *site,
        }
    }

    /// An op that changes nothing: an empty insert or a zero-length delete.
    pub fn is_noop(&self) -> bool {
        match self {
            Op::Insert { text, .. } => text.is_empty(),
            Op::Delete { len, .. } => *len == 0,
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
                text: "hi".to_string()
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
    }

    #[test]
    fn noop_detection() {
        assert!(Op::insert(1, 0, "").is_noop());
        assert!(Op::delete(1, 0, 0).is_noop());
        assert!(!Op::insert(1, 0, "x").is_noop());
        assert!(!Op::delete(1, 0, 1).is_noop());
    }
}
