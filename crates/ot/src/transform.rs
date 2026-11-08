// Transforming concurrent operations.
//
// When two sites edit at the same time, each site has already applied its
// own op locally. To apply the other site's op afterwards, it has to be
// shifted so it still means the same edit. `transform(op, against)`
// returns the op that has the same effect after `against` was applied as
// `op` had before it.
//
// The rules keep every pair of ops convergent: applying `a` then
// `transform(b, a)` gives the same document as applying `b` then
// `transform(a, b)`. Two choices make that work:
//
// - Two inserts at the same spot are ordered by site id. The lower site
//   goes first, so both sites end up with the same order.
// - An insert that lands inside a range another site deleted is dropped:
//   the delete already removed that spot, so the text has nowhere to go.
//   The matching rule on the other side is that a delete grows to cover
//   text inserted inside its range.

use crate::op::Op;

/// Adjusts `op` so it can be applied after `against` was already applied.
pub fn transform(op: &Op, against: &Op) -> Op {
    match (op, against) {
        (
            Op::Insert { site, pos, text },
            Op::Insert {
                site: other,
                pos: other_pos,
                text: other_text,
            },
        ) => {
            // The other insert pushes this one right when it lands before
            // it, or at the same spot with the lower site id.
            if *pos < *other_pos || (*pos == *other_pos && *site < *other) {
                op.clone()
            } else {
                Op::insert(*site, pos + other_text.chars().count(), text)
            }
        }
        (
            Op::Insert { site, pos, text },
            Op::Delete {
                pos: other_pos,
                len: other_len,
                ..
            },
        ) => {
            if *pos <= *other_pos {
                op.clone()
            } else if *pos >= other_pos + other_len {
                Op::insert(*site, pos - other_len, text)
            } else {
                // Inside the deleted range: the text has nowhere to go.
                Op::insert(*site, *other_pos, "")
            }
        }
        (
            Op::Delete { site, pos, len },
            Op::Insert {
                pos: other_pos,
                text: other_text,
                ..
            },
        ) => {
            let other_len = other_text.chars().count();
            if pos + len <= *other_pos {
                op.clone()
            } else if *pos >= *other_pos {
                Op::delete(*site, pos + other_len, *len)
            } else {
                // The insert landed inside this delete, so the delete
                // grows to cover the inserted text as well.
                Op::delete(*site, *pos, len + other_len)
            }
        }
        (
            Op::Delete { site, pos, len },
            Op::Delete {
                pos: other_pos,
                len: other_len,
                ..
            },
        ) => {
            // The overlap is already gone, so it comes off this delete's
            // length; anything the other delete removed before this one
            // started shifts its position left.
            let overlap = (pos + len)
                .min(other_pos + other_len)
                .saturating_sub((*pos).max(*other_pos));
            let shift = (*other_len).min(pos.saturating_sub(*other_pos));
            Op::delete(*site, pos - shift, len - overlap)
        }
    }
}

/// Transforms both ops against each other, so either order of applying
/// them lands on the same document.
pub fn transform_pair(a: &Op, b: &Op) -> (Op, Op) {
    (transform(a, b), transform(b, a))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;

    fn converges(doc: &str, a: &Op, b: &Op) {
        let mut first = Document::from_text(doc);
        first.apply(a).unwrap();
        first.apply(&transform(b, a)).unwrap();

        let mut second = Document::from_text(doc);
        second.apply(b).unwrap();
        second.apply(&transform(a, b)).unwrap();

        assert_eq!(first.text(), second.text(), "a={a:?} b={b:?} on {doc:?}");
    }

    #[test]
    fn insert_before_insert_is_unchanged() {
        let a = Op::insert(1, 0, "A");
        let b = Op::insert(2, 3, "B");
        assert_eq!(transform(&a, &b), a);
    }

    #[test]
    fn insert_after_insert_shifts_right() {
        let a = Op::insert(1, 5, "A");
        let b = Op::insert(2, 2, "BBB");
        assert_eq!(transform(&a, &b), Op::insert(1, 8, "A"));
    }

    #[test]
    fn inserts_at_the_same_spot_order_by_site() {
        let low = Op::insert(1, 2, "A");
        let high = Op::insert(2, 2, "B");
        // Lower site goes first, so it stays put.
        assert_eq!(transform(&low, &high), low);
        // Higher site goes second, so it shifts past the lower site's text.
        assert_eq!(transform(&high, &low), Op::insert(2, 3, "B"));
    }

    #[test]
    fn insert_before_delete_is_unchanged() {
        let a = Op::insert(1, 0, "A");
        let b = Op::delete(2, 2, 3);
        assert_eq!(transform(&a, &b), a);
    }

    #[test]
    fn insert_after_delete_shifts_left() {
        let a = Op::insert(1, 6, "A");
        let b = Op::delete(2, 1, 3);
        assert_eq!(transform(&a, &b), Op::insert(1, 3, "A"));
    }

    #[test]
    fn insert_inside_a_delete_is_dropped() {
        let a = Op::insert(1, 2, "A");
        let b = Op::delete(2, 1, 3);
        assert_eq!(transform(&a, &b), Op::insert(1, 1, ""));
    }

    #[test]
    fn insert_at_the_edges_of_a_delete_survives() {
        let at_start = Op::insert(1, 1, "A");
        let at_end = Op::insert(1, 4, "B");
        let b = Op::delete(2, 1, 3);
        assert_eq!(transform(&at_start, &b), at_start);
        assert_eq!(transform(&at_end, &b), Op::insert(1, 1, "B"));
    }

    #[test]
    fn delete_before_insert_is_unchanged() {
        let a = Op::delete(1, 0, 2);
        let b = Op::insert(2, 5, "B");
        assert_eq!(transform(&a, &b), a);
    }

    #[test]
    fn delete_after_insert_shifts_right() {
        let a = Op::delete(1, 4, 2);
        let b = Op::insert(2, 1, "BBB");
        assert_eq!(transform(&a, &b), Op::delete(1, 7, 2));
    }

    #[test]
    fn delete_grows_around_an_insert_inside_it() {
        let a = Op::delete(1, 1, 3);
        let b = Op::insert(2, 2, "BB");
        assert_eq!(transform(&a, &b), Op::delete(1, 1, 5));
    }

    #[test]
    fn disjoint_deletes_shift_but_keep_length() {
        let a = Op::delete(1, 5, 2);
        let b = Op::delete(2, 1, 3);
        assert_eq!(transform(&a, &b), Op::delete(1, 2, 2));
        assert_eq!(transform(&b, &a), b);
    }

    #[test]
    fn overlapping_deletes_lose_only_the_overlap() {
        // a removes 1..4, b removes 3..6: one char overlaps.
        let a = Op::delete(1, 1, 3);
        let b = Op::delete(2, 3, 3);
        assert_eq!(transform(&a, &b), Op::delete(1, 1, 2));
        // After a ran, b's remaining "ef" sits at 1..3.
        assert_eq!(transform(&b, &a), Op::delete(2, 1, 2));
    }

    #[test]
    fn delete_fully_inside_another_delete_becomes_a_noop() {
        let a = Op::delete(1, 2, 2);
        let b = Op::delete(2, 0, 6);
        assert_eq!(transform(&a, &b), Op::delete(1, 0, 0));
    }

    #[test]
    fn delete_covering_another_delete_shrinks_by_it() {
        let a = Op::delete(1, 0, 6);
        let b = Op::delete(2, 2, 2);
        assert_eq!(transform(&a, &b), Op::delete(1, 0, 4));
    }

    #[test]
    fn same_spot_inserts_converge() {
        converges("", &Op::insert(1, 0, "hello"), &Op::insert(2, 0, "world"));
    }

    #[test]
    fn insert_against_delete_converges() {
        converges("abcdef", &Op::insert(1, 2, "X"), &Op::delete(2, 1, 3));
        // Insert lands inside the deleted range: it is dropped on one side
        // and swallowed by the delete on the other, both ending at "aef".
        let mut doc = Document::from_text("abcdef");
        doc.apply(&Op::delete(2, 1, 3)).unwrap();
        doc.apply(&transform(&Op::insert(1, 2, "X"), &Op::delete(2, 1, 3)))
            .unwrap();
        assert_eq!(doc.text(), "aef");
    }

    #[test]
    fn overlapping_deletes_converge() {
        converges("abcdefgh", &Op::delete(1, 1, 4), &Op::delete(2, 3, 4));
    }

    #[test]
    fn transform_pair_matches_transform() {
        let a = Op::insert(1, 3, "A");
        let b = Op::delete(2, 1, 2);
        assert_eq!(
            transform_pair(&a, &b),
            (transform(&a, &b), transform(&b, &a))
        );
    }
}
