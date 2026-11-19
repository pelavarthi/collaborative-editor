// Convergence tests: two sites editing the same document at the same
// time must end up with the same text, no matter which op is applied
// first. These tests throw random pairs of ops at random documents and
// check both orders land on the same result.
//
// The random numbers come from a tiny xorshift generator seeded by hand,
// so the tests are deterministic: same seed, same ops, every run.

use collab_ot::attrs::FormatChange;
use collab_ot::transform::transform;
use collab_ot::{Document, Op};

/// A small deterministic generator: no extra dependency just for tests.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

const PIECES: [&str; 6] = ["a", "bc", "xyz", " ", "\u{1F600}", "ab"];

fn random_doc(rng: &mut Rng) -> String {
    let mut doc = String::new();
    for _ in 0..rng.below(8) {
        doc.push_str(PIECES[rng.below(PIECES.len())]);
    }
    doc
}

fn random_op(rng: &mut Rng, site: u64, doc_len: usize) -> Op {
    if doc_len == 0 || rng.below(2) == 0 {
        let pos = rng.below(doc_len + 1);
        Op::insert(site, pos, PIECES[rng.below(PIECES.len())])
    } else if rng.below(3) == 0 {
        // A format op: bold or italic on or off, or a link set or
        // removed, over a random range.
        let pos = rng.below(doc_len);
        let len = rng.below(doc_len - pos) + 1;
        let change = match rng.below(6) {
            0 => FormatChange::bold(true),
            1 => FormatChange::bold(false),
            2 => FormatChange::italic(true),
            3 => FormatChange::italic(false),
            4 => FormatChange::link(Some("https://example.com".into())),
            _ => FormatChange::link(None),
        };
        Op::format(site, pos, len, change)
    } else {
        let pos = rng.below(doc_len);
        let len = rng.below(doc_len - pos) + 1;
        Op::delete(site, pos, len)
    }
}

fn both_orders_converge(doc: &str, a: &Op, b: &Op) {
    let mut first = Document::from_text(doc);
    first.apply(a).expect("first op applies");
    first
        .apply(&transform(b, a))
        .expect("transformed second op applies");

    let mut second = Document::from_text(doc);
    second.apply(b).expect("second op applies");
    second
        .apply(&transform(a, b))
        .expect("transformed first op applies");

    assert_eq!(
        first.text(),
        second.text(),
        "orders diverged on {doc:?} with a={a:?} b={b:?}"
    );
    assert_eq!(
        first.attrs(),
        second.attrs(),
        "formatting diverged on {doc:?} with a={a:?} b={b:?}"
    );
}

/// Two format ops that set the same attribute differently where their
/// ranges overlap cannot converge with single-range ops: restricting
/// the losing op to the part outside the overlap would need two ranges
/// when it extends past the overlap on both sides. The transform
/// leaves formats untouched, so the later server op wins and the two
/// orders disagree. Those pairs are skipped here; everything else the
/// transform claims must converge.
fn conflicting_formats(a: &Op, b: &Op) -> bool {
    let (
        Op::Format {
            pos: ap,
            len: al,
            change: ac,
            ..
        },
        Op::Format {
            pos: bp,
            len: bl,
            change: bc,
            ..
        },
    ) = (a, b)
    else {
        return false;
    };
    let overlap = (*ap + *al).min(*bp + *bl) > (*ap).max(*bp);
    overlap && changes_conflict(ac, bc)
}

/// Both changes set the same attribute to different values: bold on
/// against bold off, a link set against the same link removed.
fn changes_conflict(a: &FormatChange, b: &FormatChange) -> bool {
    matches!((a.bold, b.bold), (Some(x), Some(y)) if x != y)
        || matches!((a.italic, b.italic), (Some(x), Some(y)) if x != y)
        || matches!((&a.link, &b.link), (Some(x), Some(y)) if x != y)
}

#[test]
fn random_pairs_of_ops_always_converge() {
    let mut rng = Rng(0xC011AB);
    for _ in 0..2000 {
        let doc = random_doc(&mut rng);
        let len = doc.chars().count();
        let a = random_op(&mut rng, 1, len);
        let b = random_op(&mut rng, 2, len);
        if conflicting_formats(&a, &b) {
            continue;
        }
        both_orders_converge(&doc, &a, &b);
    }
}

#[test]
fn three_sites_converge_through_a_server_order() {
    // A central server serializes the three concurrent ops in site
    // order, transforming each against the ones it already applied.
    // Each client applies its own op straight away, then folds the
    // server's broadcast in, transforming each broadcast op against its
    // own op while the server has not seen it yet. All three clients
    // must end up with the server's document.
    let mut rng = Rng(0xE1D70B);
    for _ in 0..500 {
        let doc = random_doc(&mut rng);
        let len = doc.chars().count();
        let ops = [
            random_op(&mut rng, 1, len),
            random_op(&mut rng, 2, len),
            random_op(&mut rng, 3, len),
        ];
        if conflicting_formats(&ops[0], &ops[1])
            || conflicting_formats(&ops[0], &ops[2])
            || conflicting_formats(&ops[1], &ops[2])
        {
            continue;
        }

        let mut server = Document::from_text(&doc);
        let mut broadcast: Vec<Op> = Vec::new();
        for op in &ops {
            let mut incoming = op.clone();
            for prev in &broadcast {
                incoming = transform(&incoming, prev);
            }
            server.apply(&incoming).unwrap();
            broadcast.push(incoming);
        }

        for (i, own) in ops.iter().enumerate() {
            let mut client = Document::from_text(&doc);
            client.apply(own).unwrap();
            // The client's own op, not yet acknowledged by the server.
            let mut unacked = Some(own.clone());
            for (k, remote) in broadcast.iter().enumerate() {
                if k == i {
                    // Its own op echoed back: already applied, just ack it.
                    unacked = None;
                    continue;
                }
                let mut incoming = remote.clone();
                if let Some(local) = &unacked {
                    incoming = transform(&incoming, local);
                }
                client.apply(&incoming).unwrap();
                // The local op now sits after the incoming one, so it is
                // transformed against the broadcast op as the server sent
                // it, not the version shifted for this client.
                if let Some(local) = unacked.take() {
                    unacked = Some(transform(&local, remote));
                }
            }
            assert_eq!(
                client.text(),
                server.text(),
                "site {} diverged on {doc:?} with ops {ops:?}",
                i + 1
            );
            assert_eq!(
                client.attrs(),
                server.attrs(),
                "site {} formatting diverged on {doc:?} with ops {ops:?}",
                i + 1
            );
        }
    }
}

#[test]
fn two_typists_writing_one_document() {
    // The classic case: two people typing at the same cursor.
    let mut alice = Document::from_text("");
    let mut bob = Document::from_text("");

    let a1 = Op::insert(1, 0, "hello");
    let b1 = Op::insert(2, 0, "world");
    alice.apply(&a1).unwrap();
    alice.apply(&transform(&b1, &a1)).unwrap();
    bob.apply(&b1).unwrap();
    bob.apply(&transform(&a1, &b1)).unwrap();

    assert_eq!(alice.text(), bob.text());
    assert_eq!(alice.text(), "helloworld");
}
