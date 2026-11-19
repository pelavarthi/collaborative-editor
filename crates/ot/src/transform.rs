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
//
// Formatting follows the same shape as deleting, because a format op
// covers a range without changing the text length:
//
// - A format shifts and shrinks past inserts and deletes exactly like
//   a delete does, and grows to cover text inserted inside it.
// - An insert that lands inside a formatted range picks up that
//   formatting, so both orders end with the inserted text formatted.
// - Two formats do not move each other: the one applied later wins on
//   the overlap, because it is applied after, and the earlier one is
//   already in place.
//
// That last rule has a known limit: two formats that set the same
// attribute differently where their ranges overlap cannot converge
// with single-range ops. Restricting the losing op to the part of its
// range outside the overlap would need two ranges when it extends
// past the overlap on both sides, and one op can only carry one. In
// that case the two orders disagree on the overlap, so the server's
// order decides and the loser is dropped. Fixing it properly means
// splitting a format op into pieces when it transforms, which is a
// bigger change than one chunk: simultaneous conflicting formats on
// the same characters are the one case this transform does not make
// convergent, and the random convergence tests skip exactly that case.

use crate::op::Op;

/// Adjusts `op` so it can be applied after `against` was already applied.
pub fn transform(op: &Op, against: &Op) -> Op {
    match (op, against) {
        (
            Op::Insert {
                site,
                pos,
                text,
                attrs,
            },
            Op::Insert {
                site: other,
                pos: other_pos,
                text: other_text,
                ..
            },
        ) => {
            // The other insert pushes this one right when it lands before
            // it, or at the same spot with the lower site id.
            if *pos < *other_pos || (*pos == *other_pos && *site < *other) {
                op.clone()
            } else {
                Op::insert_styled(*site, pos + other_text.chars().count(), text, attrs.clone())
            }
        }
        (
            Op::Insert {
                site,
                pos,
                text,
                attrs,
            },
            Op::Delete {
                pos: other_pos,
                len: other_len,
                ..
            },
        ) => {
            if *pos <= *other_pos {
                op.clone()
            } else if *pos >= other_pos + other_len {
                Op::insert_styled(*site, pos - other_len, text, attrs.clone())
            } else {
                // Inside the deleted range: the text has nowhere to go.
                Op::insert_styled(*site, *other_pos, "", attrs.clone())
            }
        }
        (
            Op::Insert {
                site,
                pos,
                text,
                attrs,
            },
            Op::Format {
                pos: other_pos,
                len: other_len,
                change,
                ..
            },
        ) => {
            // Strictly inside the formatted range, the inserted text
            // takes on the formatting: on the other side the format
            // grows to cover it, so both orders end formatted.
            if *pos > *other_pos && *pos < other_pos + other_len {
                Op::insert_styled(*site, *pos, text, change.apply_to(attrs))
            } else {
                op.clone()
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
        (Op::Delete { .. }, Op::Format { .. }) => {
            // Formatting never moves text, so a delete is untouched.
            op.clone()
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
        (
            Op::Format {
                site,
                pos,
                len,
                change,
            },
            Op::Insert {
                pos: other_pos,
                text: other_text,
                ..
            },
        ) => {
            // Same shape as a delete against an insert: an insert
            // before the range pushes it right, one inside it grows
            // the range to cover the new text.
            let other_len = other_text.chars().count();
            if pos + len <= *other_pos {
                op.clone()
            } else if *pos >= *other_pos {
                Op::format(*site, pos + other_len, *len, change.clone())
            } else {
                Op::format(*site, *pos, len + other_len, change.clone())
            }
        }
        (
            Op::Format {
                site,
                pos,
                len,
                change,
            },
            Op::Delete {
                pos: other_pos,
                len: other_len,
                ..
            },
        ) => {
            // Same shape as a delete against a delete: the overlap is
            // already gone, and anything removed before the range
            // started shifts it left.
            let overlap = (pos + len)
                .min(other_pos + other_len)
                .saturating_sub((*pos).max(*other_pos));
            let shift = (*other_len).min(pos.saturating_sub(*other_pos));
            Op::format(*site, pos - shift, len - overlap, change.clone())
        }
        (Op::Format { .. }, Op::Format { .. }) => {
            // Formatting never moves text, and the op applied later
            // wins on any overlap, so a format is untouched by another.
            op.clone()
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
    use crate::attrs::FormatChange;
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

    /// Like `converges`, but also compares the formatting: two orders
    /// must agree on which characters are bold, not just on the text.
    fn converges_rich(doc: &str, a: &Op, b: &Op) {
        let mut first = Document::from_text(doc);
        first.apply(a).unwrap();
        first.apply(&transform(b, a)).unwrap();

        let mut second = Document::from_text(doc);
        second.apply(b).unwrap();
        second.apply(&transform(a, b)).unwrap();

        assert_eq!(first.text(), second.text(), "a={a:?} b={b:?} on {doc:?}");
        assert_eq!(
            first.attrs(),
            second.attrs(),
            "formatting diverged on {doc:?} with a={a:?} b={b:?}"
        );
    }

    #[test]
    fn format_after_insert_shifts_right_or_grows() {
        let fmt = Op::format(1, 4, 2, FormatChange::bold(true));
        // Insert before the range pushes it right.
        assert_eq!(
            transform(&fmt, &Op::insert(2, 1, "XX")),
            Op::format(1, 6, 2, FormatChange::bold(true))
        );
        // Insert inside the range grows it to cover the new text.
        assert_eq!(
            transform(&fmt, &Op::insert(2, 5, "XX")),
            Op::format(1, 4, 4, FormatChange::bold(true))
        );
        // Insert at the end leaves the range alone.
        assert_eq!(transform(&fmt, &Op::insert(2, 6, "XX")), fmt);
    }

    #[test]
    fn format_after_delete_shrinks_like_a_delete() {
        let fmt = Op::format(1, 4, 4, FormatChange::bold(true));
        // The delete takes the first two chars of the range.
        assert_eq!(
            transform(&fmt, &Op::delete(2, 4, 2)),
            Op::format(1, 4, 2, FormatChange::bold(true))
        );
        // A delete before the range pulls it left.
        assert_eq!(
            transform(&fmt, &Op::delete(2, 1, 2)),
            Op::format(1, 2, 4, FormatChange::bold(true))
        );
        // A delete covering the whole range leaves an empty format.
        assert_eq!(
            transform(&fmt, &Op::delete(2, 0, 10)),
            Op::format(1, 0, 0, FormatChange::bold(true))
        );
    }

    #[test]
    fn format_against_format_is_unchanged() {
        let a = Op::format(1, 0, 5, FormatChange::bold(true));
        let b = Op::format(2, 2, 5, FormatChange::italic(true));
        assert_eq!(transform(&a, &b), a);
        assert_eq!(transform(&b, &a), b);
    }

    #[test]
    fn insert_inside_a_format_takes_its_attributes() {
        let ins = Op::insert(1, 3, "X");
        let fmt = Op::format(2, 0, 5, FormatChange::bold(true));
        assert_eq!(
            transform(&ins, &fmt),
            Op::insert_styled(1, 3, "X", crate::attrs::Attrs::bold())
        );
        // At the edges the insert keeps its own (plain) attributes:
        // on the other side the format shifts or stays put instead of
        // growing, so the text stays plain there too.
        assert_eq!(
            transform(&Op::insert(1, 0, "X"), &fmt),
            Op::insert(1, 0, "X")
        );
        assert_eq!(
            transform(&Op::insert(1, 5, "X"), &fmt),
            Op::insert(1, 5, "X")
        );
    }

    #[test]
    fn delete_is_untouched_by_a_format() {
        let del = Op::delete(1, 2, 3);
        let fmt = Op::format(2, 0, 5, FormatChange::bold(true));
        assert_eq!(transform(&del, &fmt), del);
    }

    #[test]
    fn format_and_insert_inside_it_converge() {
        converges_rich(
            "hello",
            &Op::insert(1, 2, "X"),
            &Op::format(2, 0, 5, FormatChange::bold(true)),
        );
        // The inserted text ends up bold in both orders.
        let mut doc = Document::from_text("hello");
        doc.apply(&Op::format(2, 0, 5, FormatChange::bold(true)))
            .unwrap();
        doc.apply(&transform(
            &Op::insert(1, 2, "X"),
            &Op::format(2, 0, 5, FormatChange::bold(true)),
        ))
        .unwrap();
        assert_eq!(doc.text(), "heXllo");
        assert!(doc.attrs().iter().all(|a| a.bold));
    }

    #[test]
    fn format_and_delete_converge() {
        converges_rich(
            "hello world",
            &Op::format(1, 0, 8, FormatChange::bold(true)),
            &Op::delete(2, 3, 4),
        );
        converges_rich(
            "hello world",
            &Op::format(1, 0, 5, FormatChange::italic(true)),
            &Op::delete(2, 0, 11),
        );
    }

    #[test]
    fn overlapping_formats_converge() {
        converges_rich(
            "hello world",
            &Op::format(1, 0, 6, FormatChange::bold(true)),
            &Op::format(2, 4, 7, FormatChange::italic(true)),
        );
        // The overlap ends up bold and italic in both orders.
        let mut doc = Document::from_text("hello world");
        doc.apply(&Op::format(1, 0, 6, FormatChange::bold(true)))
            .unwrap();
        doc.apply(&Op::format(2, 4, 7, FormatChange::italic(true)))
            .unwrap();
        assert!(doc.attrs()[4].bold);
        assert!(doc.attrs()[4].italic);
        assert!(!doc.attrs()[0].italic);
        assert!(!doc.attrs()[10].bold);
    }
}
