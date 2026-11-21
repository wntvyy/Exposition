//! Review of perf WS-2 (documents: patches located once, incremental
//! line-ending convention, merged undo group computed from the touched
//! region).
//!
//! Two independent oracles:
//! 1. The document service as it was at the product base (`f9f7a39`), kept
//!    verbatim in `review_support/documents_base.rs`. Random operation
//!    sequences run against both services; every result, error, event,
//!    info, text, and the retained undo/redo/history sizes must be equal.
//! 2. A naive model (UTF-16 vectors, full-text undo snapshots, a full scan
//!    for the line-ending convention) that shares no code with either.

use exposition_core::document::{
    ChangeKind, ConflictPolicy, DocumentEdit, DocumentEvent, DocumentId, EditOrigin, LineEnding, SelectionHint,
    TextEncoding, TextPatch, TransactionError, TransactionId, TransactionOutcome, TransactionRequest, UndoBehavior,
};
use exposition_core::path::WorkspacePath;
use exposition_core::text::TextRange;
use exposition_workspace::diff::diff_patches;
use exposition_workspace::documents as new;
use tokio::sync::broadcast::Receiver;

#[allow(dead_code, unused_imports, clippy::all)]
#[path = "review_support/documents_base.rs"]
mod base;

// ----- deterministic random numbers ---------------------------------------------

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xD1B5_4A32_D192_ED03)
    }

    fn next(&mut self) -> u64 {
        // xorshift64*
        let mut x = self.0.max(1);
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        if n == 0 { 0 } else { (self.next() >> 11) as usize % n }
    }

    fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

const PIECES: &[&str] = &[
    "a", "b", "the", " ", "\t", "é", "ß", "中", "😀", "𝒳", "\u{301}", "\u{200d}", "\r", "\n", "\r\n", "\n\n", "x\r",
    "\ny", "",
];

fn random_text(rng: &mut Rng, max_pieces: usize) -> String {
    let n = rng.below(max_pieces + 1);
    (0..n).map(|_| *rng.pick(PIECES)).collect()
}

/// Text without any line terminator (so documents stay editable), with
/// characters of every UTF-8 width so 4096-byte blocks end inside characters.
fn large_text(rng: &mut Rng, bytes: usize, ending: &str) -> String {
    let pieces = ["a", "bc", "é", "中", "😀", "𝒳", " ", "e\u{301}", "the "];
    let mut out = String::new();
    // A random lead so block boundaries fall at different places per seed.
    for _ in 0..rng.below(7) {
        out.push('q');
    }
    while out.len() < bytes {
        out.push_str(rng.pick(&pieces));
        if !ending.is_empty() && rng.chance(2) {
            out.push_str(ending);
        }
    }
    out
}

// ----- naive model helpers --------------------------------------------------------

fn naive_line_ending(text: &str) -> LineEnding {
    let chars: Vec<char> = text.chars().collect();
    let (mut lf, mut crlf, mut cr) = (false, false, false);
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\r' && chars.get(i + 1) == Some(&'\n') {
            crlf = true;
            i += 2;
            continue;
        }
        if chars[i] == '\r' {
            cr = true;
        }
        if chars[i] == '\n' {
            lf = true;
        }
        i += 1;
    }
    match (lf, crlf, cr) {
        (false, false, false) => LineEnding::None,
        (true, false, false) => LineEnding::Lf,
        (false, true, false) => LineEnding::Crlf,
        _ => LineEnding::Mixed,
    }
}

fn is_boundary(units: &[u16], at: usize) -> bool {
    if at == 0 || at >= units.len() {
        return at <= units.len();
    }
    let low = (0xDC00..0xE000).contains(&units[at]);
    let high_before = (0xD800..0xDC00).contains(&units[at - 1]);
    !(low && high_before)
}

/// Apply patches the slow, obvious way. `None` when a patch is unordered,
/// overlaps the previous one, is out of range, or cuts a surrogate pair.
fn naive_apply(text: &str, patches: &[TextPatch]) -> Option<String> {
    let units: Vec<u16> = text.encode_utf16().collect();
    let mut out: Vec<u16> = Vec::new();
    let mut copied = 0usize;
    for patch in patches {
        let (start, end) = (patch.range.start as usize, patch.range.end as usize);
        if start > end || end > units.len() || start < copied {
            return None;
        }
        if !is_boundary(&units, start) || !is_boundary(&units, end) {
            return None;
        }
        out.extend_from_slice(&units[copied..start]);
        out.extend(patch.text.encode_utf16());
        copied = end;
    }
    out.extend_from_slice(&units[copied..]);
    String::from_utf16(&out).ok()
}

fn boundaries(text: &str) -> Vec<u32> {
    let mut at = 0u32;
    let mut out = vec![0];
    for ch in text.chars() {
        at += ch.len_utf16() as u32;
        out.push(at);
    }
    out
}

fn utf16_len(text: &str) -> u32 {
    text.encode_utf16().count() as u32
}

// ----- the two services side by side ---------------------------------------------

struct Pair {
    new: new::DocumentService,
    base: base::DocumentService,
    new_rx: Receiver<DocumentEvent>,
    base_rx: Receiver<DocumentEvent>,
    ids: Vec<DocumentId>,
    counter: u64,
    log: Vec<String>,
    seed: u64,
}

fn drain(rx: &mut Receiver<DocumentEvent>) -> Vec<DocumentEvent> {
    let mut out = Vec::new();
    while let Ok(event) = rx.try_recv() {
        out.push(event);
    }
    out
}

impl Pair {
    fn open(seed: u64, texts: &[String]) -> Self {
        let new = new::DocumentService::new();
        let base = base::DocumentService::new();
        let new_rx = new.subscribe();
        let base_rx = base.subscribe();
        let mut pair = Self { new, base, new_rx, base_rx, ids: Vec::new(), counter: 0, log: Vec::new(), seed };
        for (index, text) in texts.iter().enumerate() {
            let path = Some(WorkspacePath::parse(&format!("doc{index}.typ")).unwrap());
            let a = pair.new.open(new::OpenDocument {
                path: path.clone(),
                text: text.clone(),
                language: "typst".into(),
                encoding: TextEncoding::Utf8,
                read_only: None,
            });
            let b = pair.base.open(base::OpenDocument {
                path,
                text: text.clone(),
                language: "typst".into(),
                encoding: TextEncoding::Utf8,
                read_only: None,
            });
            assert_eq!(a, b);
            pair.ids.push(a.id);
        }
        pair.compare("open");
        pair
    }

    fn fail(&self, what: &str) -> String {
        let tail: Vec<&String> = self.log.iter().rev().take(12).rev().collect();
        format!("seed {} after {} operations: {what}\nlast operations:\n{tail:#?}", self.seed, self.log.len())
    }

    fn text(&self, id: &DocumentId) -> String {
        self.base.state(id).unwrap().text
    }

    fn revision(&self, id: &DocumentId) -> u64 {
        self.base.info(id).unwrap().revision
    }

    /// Everything observable must be equal, and the convention reported for
    /// the text must be the one a full scan finds.
    fn compare(&mut self, what: &str) {
        let new_events = drain(&mut self.new_rx);
        let base_events = drain(&mut self.base_rx);
        assert_eq!(new_events, base_events, "{}", self.fail(&format!("events differ ({what})")));
        assert_eq!(self.new.list(), self.base.list(), "{}", self.fail(&format!("list differs ({what})")));
        for id in &self.ids {
            let a = self.new.state(id).unwrap();
            let b = self.base.state(id).unwrap();
            assert_eq!(a.text, b.text, "{}", self.fail(&format!("text differs ({what})")));
            assert_eq!(a.info, b.info, "{}", self.fail(&format!("info differs ({what})")));
            assert_eq!(
                a.info.line_ending,
                naive_line_ending(&a.text),
                "{}",
                self.fail(&format!("line ending is not that of the text {:?} ({what})", a.text))
            );
            let (shared_info, shared_text) = self.new.shared_state(id).unwrap();
            assert_eq!((&shared_info, &*shared_text), (&a.info, a.text.as_str()), "{}", self.fail("shared_state"));
            let ra = self.new.retained(id).unwrap();
            let rb = self.base.retained(id).unwrap();
            assert_eq!(
                (ra.undo_groups, ra.undo_bytes, ra.redo_groups, ra.redo_bytes, ra.history_entries, ra.history_bytes),
                (rb.undo_groups, rb.undo_bytes, rb.redo_groups, rb.redo_bytes, rb.history_entries, rb.history_bytes),
                "{}",
                self.fail(&format!("retained histories differ ({what})"))
            );
            let since = a.info.revision.saturating_sub(3);
            assert_eq!(
                self.new.origins_since(id, since),
                self.base.origins_since(id, since),
                "{}",
                self.fail("origins_since")
            );
        }
    }

    fn apply(&mut self, request: TransactionRequest) -> Result<TransactionOutcome, TransactionError> {
        let before: Vec<(DocumentId, String)> = self.ids.iter().map(|id| (id.clone(), self.text(id))).collect();
        self.log.push(format!("apply {request:?}"));
        let a = self.new.apply(request.clone());
        let b = self.base.apply(request.clone());
        assert_eq!(a, b, "{}", self.fail("apply result differs"));
        self.compare("apply");
        // Independent of the base: an accepted edit is the naive application
        // of the patches it reports; a refused one changes nothing.
        match &a {
            Ok(outcome) => {
                for applied in &outcome.applied {
                    let old = &before.iter().find(|(id, _)| *id == applied.document).unwrap().1;
                    let expected = naive_apply(old, &applied.patches);
                    assert_eq!(
                        Some(self.text(&applied.document)),
                        expected,
                        "{}",
                        self.fail("accepted edit is not the naive application of its patches")
                    );
                    assert_eq!(applied.revision, applied.previous_revision + 1, "{}", self.fail("revision step"));
                }
            }
            Err(_) => {
                for (id, old) in &before {
                    assert_eq!(&self.text(id), old, "{}", self.fail("a refused transaction changed text"));
                }
            }
        }
        a
    }

    fn step(&mut self, id: &DocumentId, kind: ChangeKind, origin: EditOrigin) -> bool {
        let before = self.text(id);
        self.log.push(format!("{kind:?} {id:?} by {origin:?}"));
        let (a, b) = if kind == ChangeKind::Undo {
            (self.new.undo(id, origin.clone()), self.base.undo(id, origin))
        } else {
            (self.new.redo(id, origin.clone()), self.base.redo(id, origin))
        };
        assert_eq!(a, b, "{}", self.fail("undo/redo result differs"));
        self.compare("undo/redo");
        match a {
            Ok(event) => {
                assert_eq!(
                    Some(self.text(id)),
                    naive_apply(&before, &event.patches),
                    "{}",
                    self.fail("undo/redo is not the naive application of its patches")
                );
                true
            }
            Err(_) => {
                assert_eq!(self.text(id), before, "{}", self.fail("a refused undo/redo changed text"));
                false
            }
        }
    }

    fn request(
        &mut self,
        origin: EditOrigin,
        edits: Vec<DocumentEdit>,
        undo: UndoBehavior,
        conflict: ConflictPolicy,
        selection: Option<SelectionHint>,
    ) -> TransactionRequest {
        self.counter += 1;
        TransactionRequest {
            id: TransactionId(format!("review-{}", self.counter)),
            origin,
            edits,
            undo,
            conflict,
            label: None,
            selection,
        }
    }
}

fn random_origin(rng: &mut Rng) -> EditOrigin {
    match rng.below(12) {
        0..=5 => EditOrigin::Source,
        6 => EditOrigin::Page,
        7 => EditOrigin::Agent { task_id: "t".into() },
        8 => EditOrigin::External,
        9 => EditOrigin::Recovery,
        10 => EditOrigin::System,
        _ => EditOrigin::Research { run_id: "r".into() },
    }
}

fn random_undo(rng: &mut Rng) -> UndoBehavior {
    match rng.below(10) {
        0..=3 => UndoBehavior::NewGroup,
        4..=6 => UndoBehavior::Merge { key: "typing".into() },
        7 => UndoBehavior::Merge { key: "other".into() },
        _ => UndoBehavior::Skip,
    }
}

/// Sorted patches over `text`: mostly valid, sometimes touching each other,
/// sometimes inside a surrogate pair, out of range, or overlapping.
fn random_patches(rng: &mut Rng, text: &str) -> Vec<TextPatch> {
    let bounds = boundaries(text);
    let len = *bounds.last().unwrap();
    let count = 1 + rng.below(4);
    let mut offsets: Vec<u32> = (0..count * 2)
        .map(|_| {
            if rng.chance(85) {
                // Near the start, near the end, or anywhere.
                match rng.below(6) {
                    0 => bounds[rng.below(bounds.len().min(3))],
                    1 => bounds[bounds.len() - 1 - rng.below(bounds.len().min(3))],
                    _ => *rng.pick(&bounds),
                }
            } else {
                rng.below(len as usize + 2) as u32
            }
        })
        .collect();
    offsets.sort_unstable();
    if rng.chance(3) {
        offsets.reverse();
    }
    if rng.chance(2) {
        let last = offsets.len() - 1;
        offsets[last] = len + 1 + rng.below(3) as u32;
    }
    offsets
        .chunks(2)
        .map(|pair| {
            // Keep most ranges short so deletions do not empty the text at once.
            let (start, mut end) = (pair[0], pair[1]);
            if end > start && rng.chance(70) {
                end = start + (end - start).min(1 + rng.below(4) as u32);
            }
            TextPatch { range: TextRange::new(start, end), text: random_text(rng, 3) }
        })
        .collect()
}

/// One random operation on the pair.
fn random_step(rng: &mut Rng, pair: &mut Pair, cursors: &mut [u32]) {
    let which = rng.below(pair.ids.len());
    let id = pair.ids[which].clone();
    let text = pair.text(&id);
    let revision = pair.revision(&id);
    let bounds = boundaries(&text);
    let len = *bounds.last().unwrap();
    cursors[which] = cursors[which].min(len);
    match rng.below(100) {
        // Typing: one character at the cursor, merged.
        0..=34 => {
            if rng.chance(10) {
                cursors[which] = *rng.pick(&bounds);
            }
            let piece = if rng.chance(70) {
                *rng.pick(&["a", "t", "h", "e", " ", "é", "😀", "\u{301}"])
            } else {
                *rng.pick(PIECES)
            };
            let at = cursors[which];
            let selection = rng.chance(50).then(|| SelectionHint {
                before: vec![TextRange::empty(at)],
                after: vec![TextRange::empty(at + utf16_len(piece))],
            });
            let request = pair.request(
                EditOrigin::Source,
                vec![DocumentEdit {
                    document: id,
                    base_revision: revision,
                    patches: vec![TextPatch::insert(at, piece)],
                }],
                UndoBehavior::Merge { key: "typing".into() },
                ConflictPolicy::Reject,
                selection,
            );
            if pair.apply(request).is_ok() {
                cursors[which] = at + utf16_len(piece);
            }
        }
        // Backspace / delete forward, merged with the typing.
        35..=44 => {
            let at = cursors[which];
            let index = bounds.iter().position(|b| *b >= at).unwrap_or(bounds.len() - 1);
            let (start, end) = if rng.chance(70) {
                (bounds[index.saturating_sub(1)], bounds[index])
            } else {
                (bounds[index], bounds[(index + 1).min(bounds.len() - 1)])
            };
            if start == end {
                return;
            }
            let request = pair.request(
                EditOrigin::Source,
                vec![DocumentEdit {
                    document: id,
                    base_revision: revision,
                    patches: vec![TextPatch::delete(start, end)],
                }],
                UndoBehavior::Merge { key: "typing".into() },
                ConflictPolicy::Reject,
                None,
            );
            if pair.apply(request).is_ok() {
                cursors[which] = start;
            }
        }
        // Any patches, any undo behaviour, any origin, sometimes a stale base.
        45..=69 => {
            let patches = random_patches(rng, &text);
            let base_revision = match rng.below(12) {
                0 => revision.saturating_sub(1 + rng.below(3) as u64),
                1 => revision + 1,
                _ => revision,
            };
            let conflict = if rng.chance(50) { ConflictPolicy::RebaseDisjoint } else { ConflictPolicy::Reject };
            let selection = rng.chance(30).then(|| SelectionHint {
                before: vec![TextRange::new(0, 1.min(len))],
                after: vec![TextRange::empty(rng.below(len as usize + 1) as u32)],
            });
            let request = pair.request(
                random_origin(rng),
                vec![DocumentEdit { document: id, base_revision, patches }],
                random_undo(rng),
                conflict,
                selection,
            );
            let _ = pair.apply(request);
        }
        // Several documents in one transaction (all or nothing).
        70..=74 => {
            let edits: Vec<DocumentEdit> = pair
                .ids
                .clone()
                .into_iter()
                .map(|doc| {
                    let text = pair.text(&doc);
                    DocumentEdit {
                        base_revision: pair.revision(&doc),
                        patches: random_patches(rng, &text),
                        document: doc,
                    }
                })
                .collect();
            let request = pair.request(random_origin(rng), edits, random_undo(rng), ConflictPolicy::Reject, None);
            let _ = pair.apply(request);
        }
        // Undo / redo by any origin.
        75..=86 => {
            pair.step(&id, ChangeKind::Undo, random_origin(rng));
        }
        87..=92 => {
            pair.step(&id, ChangeKind::Redo, random_origin(rng));
        }
        // The file changed on disk: the buffer is replaced by the diff to the new text.
        93..=96 => {
            let mut next = text.clone();
            if rng.chance(15) {
                next = random_text(rng, 12);
            } else {
                for _ in 0..1 + rng.below(3) {
                    let b = boundaries(&next);
                    let at = *rng.pick(&b) as usize;
                    let units: Vec<u16> = next.encode_utf16().collect();
                    let cut = (at + rng.below(3)).min(units.len());
                    let cut = (cut..=units.len()).find(|c| is_boundary(&units, *c)).unwrap_or(units.len());
                    let mut out = units[..at].to_vec();
                    out.extend(random_text(rng, 3).encode_utf16());
                    out.extend_from_slice(&units[cut..]);
                    next = String::from_utf16(&out).unwrap();
                }
            }
            let patches = diff_patches(&text, &next);
            if patches.is_empty() {
                return;
            }
            let request = pair.request(
                EditOrigin::External,
                vec![DocumentEdit { document: id.clone(), base_revision: revision, patches }],
                UndoBehavior::NewGroup,
                ConflictPolicy::Reject,
                None,
            );
            if pair.apply(request).is_ok() {
                assert_eq!(pair.text(&id), next, "{}", pair.fail("reload did not produce the disk text"));
                let rev = pair.revision(&id);
                assert_eq!(pair.new.mark_saved(&id, rev), pair.base.mark_saved(&id, rev));
                pair.compare("mark_saved after reload");
            }
        }
        // Saved in between.
        _ => {
            pair.log.push(format!("mark_saved {id:?} {revision}"));
            assert_eq!(pair.new.mark_saved(&id, revision), pair.base.mark_saved(&id, revision));
            pair.compare("mark_saved");
        }
    }
}

fn run(seed: u64, texts: Vec<String>, steps: usize) {
    let mut rng = Rng::new(seed);
    let mut cursors: Vec<u32> = texts.iter().map(|t| utf16_len(t)).collect();
    let mut pair = Pair::open(seed, &texts);
    for _ in 0..steps {
        random_step(&mut rng, &mut pair, &mut cursors);
    }
}

const SEEDS_TEXTS: &[&str] = &[
    "",
    "hello world",
    "then",
    "a\nb\n",
    "a\r\nb\r\nc",
    "line\r\n",
    "x\r",
    "a\rb",
    "a\r\nb\nc\r",
    "😀😀😀",
    "e\u{301}e\u{301}\n",
    "中文\n𝒳𝒳\n",
];

#[test]
fn random_sequences_on_small_texts_match_the_base_implementation_and_the_naive_model() {
    for seed in 0..600u64 {
        let mut rng = Rng::new(seed ^ 0xABCD);
        let first = if seed % 3 == 0 {
            random_text(&mut rng, 30)
        } else {
            SEEDS_TEXTS[seed as usize % SEEDS_TEXTS.len()].to_owned()
        };
        let second = random_text(&mut rng, 10);
        run(seed, vec![first, second], 150);
    }
}

#[test]
fn random_sequences_on_texts_of_several_seek_blocks_match_the_base_implementation() {
    for seed in 0..60u64 {
        let mut rng = Rng::new(seed ^ 0x51DE);
        let ending = ["", "\n", "\r\n"][seed as usize % 3];
        let size = [4090, 4096, 4100, 8190, 8200, 12_300, 20_000][seed as usize % 7];
        run(1_000_000 + seed, vec![large_text(&mut rng, size, ending)], 120);
    }
}

// ----- scripted cases named in the review brief ---------------------------------

fn typed(pair: &mut Pair, id: &DocumentId, at: u32, text: &str) -> Result<TransactionOutcome, TransactionError> {
    let revision = pair.revision(id);
    let request = pair.request(
        EditOrigin::Source,
        vec![DocumentEdit {
            document: id.clone(),
            base_revision: revision,
            patches: vec![TextPatch::insert(at, text)],
        }],
        UndoBehavior::Merge { key: "typing".into() },
        ConflictPolicy::Reject,
        None,
    );
    pair.apply(request)
}

fn patched(pair: &mut Pair, id: &DocumentId, patches: Vec<TextPatch>, undo: UndoBehavior) -> bool {
    let revision = pair.revision(id);
    let request = pair.request(
        EditOrigin::Source,
        vec![DocumentEdit { document: id.clone(), base_revision: revision, patches }],
        undo,
        ConflictPolicy::Reject,
        None,
    );
    pair.apply(request).is_ok()
}

#[test]
fn scripted_line_ending_and_boundary_cases_match_the_base_implementation() {
    // A lone CR at the end, then LF typed after it: the pair becomes one CRLF.
    let mut pair = Pair::open(1, &["a\r\nb".to_owned()]);
    let id = pair.ids[0].clone();
    assert!(typed(&mut pair, &id, 4, "\r").is_ok(), "CRLF text + lone CR is accepted or refused like the base");
    let _ = typed(&mut pair, &id, 5, "\n");
    pair.step(&id, ChangeKind::Undo, EditOrigin::Source);
    pair.step(&id, ChangeKind::Redo, EditOrigin::Source);

    // Typing into the middle of a CRLF (splits it), then deleting what was typed (joins it).
    let mut pair = Pair::open(2, &["one\r\ntwo\r\n".to_owned()]);
    let id = pair.ids[0].clone();
    let _ = typed(&mut pair, &id, 4, "x");
    let text = pair.text(&id);
    if text.contains("\rx\n") {
        assert!(patched(&mut pair, &id, vec![TextPatch::delete(4, 5)], UndoBehavior::Merge { key: "typing".into() }));
    }
    pair.step(&id, ChangeKind::Undo, EditOrigin::Source);

    // Deleting the text between a CR and an LF joins them.
    let mut pair = Pair::open(3, &["a\r\nb\rxx\nc".to_owned()]);
    let id = pair.ids[0].clone();
    let _ = patched(&mut pair, &id, vec![TextPatch::delete(5, 7)], UndoBehavior::NewGroup);
    pair.step(&id, ChangeKind::Undo, EditOrigin::Source);

    // Two touching patches that form a terminator between them.
    let mut pair = Pair::open(4, &["ab\ncd".to_owned()]);
    let id = pair.ids[0].clone();
    let _ = patched(
        &mut pair,
        &id,
        vec![TextPatch::replace(0, 1, "x\r"), TextPatch::replace(1, 2, "\ny")],
        UndoBehavior::NewGroup,
    );
    // An insertion that ends in CR right before an existing LF, and one that starts with LF after an existing CR.
    let mut pair = Pair::open(5, &["ab\ncd".to_owned()]);
    let id = pair.ids[0].clone();
    let _ = patched(&mut pair, &id, vec![TextPatch::insert(2, "q\r")], UndoBehavior::NewGroup);
    pair.step(&id, ChangeKind::Undo, EditOrigin::Source);
    // Removing the only terminator, and the last LF of several.
    let _ = patched(&mut pair, &id, vec![TextPatch::delete(2, 3)], UndoBehavior::NewGroup);
    pair.step(&id, ChangeKind::Undo, EditOrigin::Source);

    // "the" typed right before "then": the common prefix runs on into unchanged text.
    let mut pair = Pair::open(6, &["x then y then".to_owned()]);
    let id = pair.ids[0].clone();
    for (at, ch) in [(2, "t"), (3, "h"), (4, "e"), (5, "n"), (6, " ")] {
        typed(&mut pair, &id, at, ch).unwrap();
    }
    pair.step(&id, ChangeKind::Undo, EditOrigin::Source);
    assert_eq!(pair.text(&id), "x then y then");
    pair.step(&id, ChangeKind::Redo, EditOrigin::Source);
    assert_eq!(pair.text(&id), "x then then y then");

    // Typing then deleting everything typed (the merged group becomes empty), then typing again.
    let mut pair = Pair::open(7, &["😀é".to_owned()]);
    let id = pair.ids[0].clone();
    typed(&mut pair, &id, 2, "😀").unwrap();
    assert!(patched(&mut pair, &id, vec![TextPatch::delete(2, 4)], UndoBehavior::Merge { key: "typing".into() }));
    typed(&mut pair, &id, 0, "é").unwrap();
    typed(&mut pair, &id, 4, "z").unwrap();
    pair.step(&id, ChangeKind::Undo, EditOrigin::Source);
    assert_eq!(pair.text(&id), "😀é");

    // Offsets inside a surrogate pair and past the end are refused alike.
    let mut pair = Pair::open(8, &["a😀b".to_owned()]);
    let id = pair.ids[0].clone();
    assert!(typed(&mut pair, &id, 2, "x").is_err());
    assert!(typed(&mut pair, &id, 5, "x").is_err());
    assert!(typed(&mut pair, &id, 4, "x").is_ok());
}

/// Every UTF-16 offset of a text that spans several 4096-byte blocks, with
/// characters of every width across the block ends: an insertion at each
/// boundary lands where the naive model puts it, and each offset inside a
/// surrogate pair is refused.
#[test]
fn every_offset_across_seek_block_ends_is_located_like_the_naive_model() {
    for lead in 0..4usize {
        let mut text = "q".repeat(lead);
        let mut rng = Rng::new(77 + lead as u64);
        while text.len() < 4096 * 2 + 64 {
            text.push_str(rng.pick(&["é", "中", "😀", "𝒳", "a", "e\u{301}"]));
        }
        let service = new::DocumentService::new();
        let id = service
            .open(new::OpenDocument {
                path: None,
                text: text.clone(),
                language: "typst".into(),
                encoding: TextEncoding::Utf8,
                read_only: None,
            })
            .id;
        let units: Vec<u16> = text.encode_utf16().collect();
        let mut current = text.clone();
        let mut revision = 1;
        // Offsets around each block end (in UTF-16 units the block ends lie
        // somewhere before 4096 and 8192; cover a wide window) and the end.
        let mut targets: Vec<usize> = (0..units.len() + 2).filter(|o| o % 97 == 0).collect();
        for block_end in [4096usize, 8192] {
            let unit_at =
                text[..(0..=block_end).rev().find(|b| text.is_char_boundary(*b)).unwrap()].encode_utf16().count();
            targets.extend(unit_at.saturating_sub(6)..unit_at + 6);
        }
        targets.extend(units.len().saturating_sub(4)..units.len() + 2);
        for target in targets {
            let probe = TextPatch::replace(target as u32, target as u32, "#");
            let expected = naive_apply(&current, std::slice::from_ref(&probe));
            let result = service.apply(TransactionRequest {
                id: TransactionId(format!("probe-{target}")),
                origin: EditOrigin::Source,
                edits: vec![DocumentEdit { document: id.clone(), base_revision: revision, patches: vec![probe] }],
                undo: UndoBehavior::NewGroup,
                conflict: ConflictPolicy::Reject,
                label: None,
                selection: None,
            });
            match expected {
                Some(expected) => {
                    assert!(result.is_ok(), "lead {lead} offset {target}: {result:?}");
                    assert_eq!(service.state(&id).unwrap().text, expected, "lead {lead} offset {target}");
                    // Take the probe out again so offsets stay those of the original text.
                    service.undo(&id, EditOrigin::Source).unwrap();
                    revision += 2;
                    current = service.state(&id).unwrap().text;
                    assert_eq!(current, text, "lead {lead} offset {target}: undo restores the text");
                }
                None => assert!(result.is_err(), "lead {lead} offset {target} must be refused"),
            }
        }
    }
}

// ----- undo/redo against full-text snapshots (no base code involved) --------------

struct Group {
    key: Option<String>,
    origin: std::mem::Discriminant<EditOrigin>,
    start: String,
    end: String,
    end_revision: u64,
}

/// Typing, deleting and replacing (recorded edits only) interleaved with
/// undo and redo: undo restores exactly the text at the start of the group
/// the model says is on top, redo the text at its end.
#[test]
fn undo_and_redo_restore_the_snapshots_of_a_simple_group_model() {
    for seed in 0..400u64 {
        let mut rng = Rng::new(seed ^ 0x77);
        let initial = match seed % 4 {
            0 => random_text(&mut rng, 25),
            1 => large_text(&mut rng, 4200, ["", "\n", "\r\n"][seed as usize % 3]),
            _ => SEEDS_TEXTS[seed as usize % SEEDS_TEXTS.len()].to_owned(),
        };
        let service = new::DocumentService::new();
        let id = service
            .open(new::OpenDocument {
                path: None,
                text: initial.clone(),
                language: "typst".into(),
                encoding: TextEncoding::Utf8,
                read_only: None,
            })
            .id;
        let mut text = initial;
        let mut revision = 1u64;
        let mut undo: Vec<Group> = Vec::new();
        let mut redo: Vec<Group> = Vec::new();
        let mut cursor = utf16_len(&text);
        for step in 0..120 {
            let context = |what: &str| format!("seed {seed} step {step}: {what}");
            match rng.below(10) {
                0..=6 => {
                    let bounds = boundaries(&text);
                    cursor = cursor.min(*bounds.last().unwrap());
                    if !bounds.contains(&cursor) || rng.chance(8) {
                        cursor = *rng.pick(&bounds);
                    }
                    let (patches, behavior) = match rng.below(10) {
                        0..=5 => (
                            vec![TextPatch::insert(
                                cursor,
                                *rng.pick(&["a", "é", "😀", "\u{301}", "\n", "\r\n", "\r", " "]),
                            )],
                            UndoBehavior::Merge { key: "typing".into() },
                        ),
                        6..=7 => {
                            let index = bounds.iter().position(|b| *b == cursor).unwrap();
                            if index == 0 {
                                continue;
                            }
                            (
                                vec![TextPatch::delete(bounds[index - 1], cursor)],
                                UndoBehavior::Merge { key: "typing".into() },
                            )
                        }
                        8 => {
                            let mut picks =
                                [*rng.pick(&bounds), *rng.pick(&bounds), *rng.pick(&bounds), *rng.pick(&bounds)];
                            picks.sort_unstable();
                            (
                                vec![
                                    TextPatch::replace(picks[0], picks[1], random_text(&mut rng, 2)),
                                    TextPatch::replace(picks[2], picks[3], random_text(&mut rng, 2)),
                                ],
                                if rng.chance(50) {
                                    UndoBehavior::NewGroup
                                } else {
                                    UndoBehavior::Merge { key: "typing".into() }
                                },
                            )
                        }
                        _ => (vec![TextPatch::insert(cursor, "z")], UndoBehavior::Merge { key: "other".into() }),
                    };
                    let origin = if rng.chance(85) { EditOrigin::Source } else { EditOrigin::Page };
                    let expected = naive_apply(&text, &patches).expect("generated patches are valid");
                    let was_mixed = naive_line_ending(&text) == LineEnding::Mixed;
                    let result = service.apply(TransactionRequest {
                        id: TransactionId(format!("m-{seed}-{step}")),
                        origin: origin.clone(),
                        edits: vec![DocumentEdit {
                            document: id.clone(),
                            base_revision: revision,
                            patches: patches.clone(),
                        }],
                        undo: behavior.clone(),
                        conflict: ConflictPolicy::Reject,
                        label: None,
                        selection: None,
                    });
                    let refused = was_mixed && naive_line_ending(&expected) == LineEnding::Mixed;
                    if refused {
                        assert!(
                            matches!(result, Err(TransactionError::MixedLineEndings { .. })),
                            "{}",
                            context(&format!("mixed text stays read-only: {result:?}"))
                        );
                        continue;
                    }
                    let outcome =
                        result.unwrap_or_else(|e| panic!("{}", context(&format!("valid edit refused: {e:?}"))));
                    let previous = revision;
                    revision = outcome.applied[0].revision;
                    let key = match &behavior {
                        UndoBehavior::Merge { key } => Some(key.clone()),
                        _ => None,
                    };
                    let disc = std::mem::discriminant(&origin);
                    match undo.last_mut() {
                        Some(top)
                            if key.is_some()
                                && top.key == key
                                && top.origin == disc
                                && top.end_revision == previous =>
                        {
                            top.end = expected.clone();
                            top.end_revision = revision;
                        }
                        _ => undo.push(Group {
                            key,
                            origin: disc,
                            start: text.clone(),
                            end: expected.clone(),
                            end_revision: revision,
                        }),
                    }
                    redo.clear();
                    text = expected;
                    if let Some(last) = patches.last() {
                        cursor = (last.range.start + utf16_len(&last.text)).min(utf16_len(&text));
                    }
                }
                7..=8 => {
                    let result = service.undo(&id, EditOrigin::Source);
                    let Some(group) = undo.pop() else {
                        assert!(
                            matches!(result, Err(TransactionError::NothingToUndo { .. })),
                            "{}",
                            context("nothing to undo")
                        );
                        continue;
                    };
                    if naive_line_ending(&text) == LineEnding::Mixed
                        && naive_line_ending(&group.start) == LineEnding::Mixed
                    {
                        assert!(result.is_err(), "{}", context("undo between two mixed texts is refused"));
                        undo.push(group);
                        continue;
                    }
                    let event = result.unwrap_or_else(|e| panic!("{}", context(&format!("undo refused: {e:?}"))));
                    revision = event.revision;
                    text = group.start.clone();
                    redo.push(Group { key: None, end_revision: revision, ..group });
                }
                _ => {
                    let result = service.redo(&id, EditOrigin::Source);
                    let Some(group) = redo.pop() else {
                        assert!(
                            matches!(result, Err(TransactionError::NothingToUndo { .. })),
                            "{}",
                            context("nothing to redo")
                        );
                        continue;
                    };
                    if naive_line_ending(&text) == LineEnding::Mixed
                        && naive_line_ending(&group.end) == LineEnding::Mixed
                    {
                        assert!(result.is_err(), "{}", context("redo between two mixed texts is refused"));
                        redo.push(group);
                        continue;
                    }
                    let event = result.unwrap_or_else(|e| panic!("{}", context(&format!("redo refused: {e:?}"))));
                    revision = event.revision;
                    text = group.end.clone();
                    undo.push(Group { key: None, end_revision: revision, ..group });
                }
            }
            let state = service.state(&id).unwrap();
            assert_eq!(state.text, text, "{}", context("text"));
            assert_eq!(state.info.revision, revision, "{}", context("revision"));
            assert_eq!(state.info.line_ending, naive_line_ending(&text), "{}", context("line ending"));
            assert_eq!(
                (state.info.can_undo, state.info.can_redo),
                (!undo.is_empty(), !redo.is_empty()),
                "{}",
                context("can undo/redo")
            );
        }
    }
}
