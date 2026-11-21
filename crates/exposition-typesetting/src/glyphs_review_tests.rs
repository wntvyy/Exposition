//! Review: the top-level index must find exactly the node a search from the
//! root finds, for every span of every document, also after incremental
//! edits. Randomized (fixed seeds) over generated Typst sources.

use super::*;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

const WORDS: &[&str] = &[
    "plain",
    "caf\u{e9}",
    "\u{4e2d}\u{6587}",
    "\u{1d538}\u{1d539}",
    "e\u{301}",
    "\u{1f600}",
    "x",
    "*strong*",
    "_emph_",
    "`raw`",
    "$a^2 + b_1$",
    "#emph[in #strong[deep \u{1d538}]]",
    "#box[a #box[b #box[c]]]",
    "#include \"chapter.typ\"",
    "#import \"lib.typ\": f",
    "#let v = [content *here*]",
    "#f(1, [arg], named: $x$)",
    "<label>",
    "@ref",
    "// comment",
    "/* block */",
    "\\#",
    "~",
    "---",
];

fn word(rng: &mut Rng) -> &'static str {
    WORDS[rng.below(WORDS.len())]
}

fn generate(rng: &mut Rng) -> String {
    let newline = if rng.below(3) == 0 { "\r\n" } else { "\n" };
    let mut text = String::new();
    for _ in 0..rng.below(60) {
        match rng.below(9) {
            0 => text.push_str(&format!("= Heading {}{newline}", word(rng))),
            1 => text.push_str(&format!("- item {}{newline}  - nested {}{newline}", word(rng), word(rng))),
            2 => text.push_str(&format!("$ sum_(k = 0)^n k = (n (n + 1)) / 2 $ {newline}{newline}")),
            3 => text.push_str(&format!("```rust{newline}fn main() {{}}{newline}```{newline}")),
            4 => text.push_str(&format!("#table(columns: 2, [a], [{}]){newline}", word(rng))),
            5 => text.push_str(newline),
            6 => text.push_str(&format!("+ one{newline}+ two{newline}/ Term: description{newline}")),
            _ => {
                for _ in 0..1 + rng.below(8) {
                    text.push_str(word(rng));
                    text.push(' ');
                }
                text.push_str(newline);
            }
        }
    }
    text
}

fn walk<'a>(node: &LinkedNode<'a>, out: &mut Vec<LinkedNode<'a>>) {
    out.push(node.clone());
    for child in node.children() {
        walk(&child, out);
    }
}

fn describe(node: Option<LinkedNode<'_>>) -> Option<(std::ops::Range<usize>, SyntaxKind, Span)> {
    node.map(|n| (n.range(), n.kind(), n.span()))
}

/// Every span of `source`, and the spans in `foreign`: the index followed by
/// the retained root search gives what the root search alone gives.
fn check(source: &Source, foreign: &[Span], context: &str) -> (usize, usize) {
    let top = TopLevel::new(source);
    assert!(top.keys.windows(2).all(|pair| pair[0] <= pair[1]), "{context}: keys are sorted");
    let root = LinkedNode::new(source.root());
    let mut nodes = Vec::new();
    walk(&root, &mut nodes);
    let (mut indexed, mut checked) = (0, 0);
    for span in nodes.iter().map(|node| node.span()).chain(foreign.iter().copied()) {
        let through_index = top.find(span);
        let searched = source.find(span);
        // A node the index returns must be the root search's node, never another one.
        if through_index.is_some() {
            indexed += 1;
            assert_eq!(
                describe(through_index.clone()),
                describe(searched.clone()),
                "{context}: the index found another node for {span:?}"
            );
        }
        assert_eq!(describe(through_index.or_else(|| source.find(span))), describe(searched), "{context}: {span:?}");
        checked += 1;
    }
    (indexed, checked)
}

fn spans(source: &Source) -> Vec<Span> {
    let mut nodes = Vec::new();
    walk(&LinkedNode::new(source.root()), &mut nodes);
    nodes.iter().map(|node| node.span()).collect()
}

#[test]
fn the_index_equals_the_root_search_on_generated_documents() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let (mut indexed, mut checked, mut documents) = (0, 0, 0);
    // Spans of the previous document (another tree with the same file id).
    let mut previous: Vec<Span> = vec![Span::detached()];
    for round in 0..300 {
        let text = if round == 0 { String::new() } else { generate(&mut rng) };
        let source = Source::detached(text);
        let (i, c) = check(&source, &previous, &format!("document {round}"));
        indexed += i;
        checked += c;
        documents += 1;
        previous = spans(&source);
        previous.push(Span::detached());
    }
    eprintln!("{documents} documents, {checked} spans checked, {indexed} found through the index");
    assert!(checked > 50_000, "{indexed} of {checked}");
}

#[test]
fn the_index_equals_the_root_search_after_incremental_edits() {
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    let (mut indexed, mut checked, mut edits) = (0, 0, 0);
    for document in 0..60 {
        let mut source = Source::detached(generate(&mut rng));
        for edit in 0..25 {
            let text = source.text().to_owned();
            // A replacement at character boundaries: insert, delete or both.
            let mut at = rng.below(text.len() + 1);
            while !text.is_char_boundary(at) {
                at -= 1;
            }
            let mut end = (at + rng.below(12)).min(text.len());
            while !text.is_char_boundary(end) {
                end += 1;
            }
            let with = match rng.below(5) {
                0 => String::new(),
                1 => "\r\n\r\n".to_owned(),
                2 => "$".to_owned(),
                3 => "#box[".to_owned(),
                _ => word(&mut rng).to_owned(),
            };
            source.edit(at..end, &with);
            let (i, c) = check(&source, &[], &format!("document {document} edit {edit}"));
            indexed += i;
            checked += c;
            edits += 1;
        }
        // A whole-text replacement reuses the tree where it can.
        let replacement = generate(&mut rng);
        source.replace(&replacement);
        check(&source, &[], &format!("document {document} replaced"));
    }
    eprintln!("{edits} edits, {checked} spans checked, {indexed} found through the index");
    assert!(checked > 100_000, "{indexed} of {checked}");
}
