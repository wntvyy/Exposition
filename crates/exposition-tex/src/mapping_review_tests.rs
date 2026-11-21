//! Review: the per-file indexes must give exactly what the scans they
//! replace give. Randomized (fixed seeds) over generated LaTeX sources, with
//! the parser's own units and with units put out of order.

use exposition_core::text::byte_to_utf16;

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

const PIECES: &[&str] = &[
    "plain words here ",
    "caf\u{e9} na\u{ef}ve ",
    "\u{4e2d}\u{6587} ",
    "\u{1d538}\u{1d539}\u{1f600} ",
    "e\u{301} ",
    "$a_1 + \\alpha^2$ ",
    "\\emph{nested \\textbf{deep \u{1d538}}} ",
    "\\footnote{A note with $x$ and \u{e9}.} ",
    "\\section{Heading \u{4e2d}}\n",
    "\\begin{equation}\n  \\sum_{k=0}^{n} k = \\frac{n (n + 1)}{2}\n\\end{equation}\n",
    "\\[ x^2 \\]\n",
    "% a comment \u{1f600}\n",
    "\\title{The \\emph{Title}}\\author{A. U. Thor\\thanks{Thanks.}}\\date{Today}\n",
    "\\maketitle\n",
    "\\begin{figure}\\caption{A caption \u{e9}.}\\end{figure}\n",
    "\\input{chapter}\n",
    "--- ``quoted\u{2019}\u{2019} \\ldots\\ ",
    "\n\n",
    "\n",
    "\r\n",
    "\r\n\r\n",
    "\\begin{itemize}\\item one \\item two\\end{itemize}\n",
    "\\begin{align}a &= b \\\\ c &= d\\end{align}\n",
];

fn generate(rng: &mut Rng) -> String {
    let mut text = String::new();
    if rng.below(4) != 0 {
        text.push_str("\\documentclass{article}\n\\begin{document}\n");
    }
    for _ in 0..rng.below(260) {
        text.push_str(PIECES[rng.below(PIECES.len())]);
    }
    if rng.below(4) != 0 {
        text.push_str("\\end{document}\n");
    }
    text
}

fn file(text: &str) -> SourceFile {
    SourceFile::new(WorkspacePath::parse("main.tex").unwrap(), Arc::from(text))
}

#[derive(Default)]
struct Seen {
    files: usize,
    multi_mark: usize,
    lines_unordered: usize,
    ranges_unordered: usize,
    deferred: usize,
    lookups: usize,
}

/// Compare every indexed lookup of `file` with the scan it replaced.
fn compare(file: &SourceFile, rng: &mut Rng, seen: &mut Seen, context: &str) {
    let text: &str = &file.text;
    seen.files += 1;
    seen.multi_mark += usize::from(file.index.marks.len() > 2);
    seen.lines_unordered += usize::from(!file.index.lines_ordered);
    seen.ranges_unordered += usize::from(!file.index.ranges_ordered);
    // Offsets: every byte offset, every UTF-16 offset, and beyond both ends.
    for byte in 0..=text.len() + 3 {
        assert_eq!(file.byte_to_utf16(byte), byte_to_utf16(text, byte).ok(), "{context}: byte {byte}");
    }
    let units16 = utf16_len(text);
    for offset in 0..=units16 + 3 {
        assert_eq!(file.utf16_to_byte(offset), utf16_to_byte(text, offset).ok(), "{context}: utf16 {offset}");
    }
    assert_eq!(file.utf16_to_byte(usize::MAX), utf16_to_byte(text, usize::MAX).ok());
    assert_eq!(file.byte_to_utf16(usize::MAX), None);
    seen.lookups += text.len() + units16;

    let units = &file.model.units;
    let last_line = units.iter().map(|u| u.line).filter(|line| *line < 1_000_000).max().unwrap_or(0);
    let mut line_queries: Vec<(u32, u32)> = vec![(0, 0), (0, u32::MAX), (1, 1), (last_line, last_line), (5, 2)];
    for line in 0..=last_line.min(40) + 1 {
        line_queries.push((line, line));
        line_queries.push((line, line + 2));
    }
    for _ in 0..40 {
        let first = rng.below(last_line as usize + 3) as u32;
        line_queries.push((first, first + rng.below(6) as u32));
        line_queries.push((first, first.saturating_sub(rng.below(3) as u32)));
    }
    for (first, last) in line_queries {
        let scanned: Vec<usize> =
            (0..units.len()).filter(|&i| units[i].line >= first && units[i].line <= last).collect();
        assert_eq!(file.units_on_lines(first, last), scanned, "{context}: lines {first}..={last}");
        seen.lookups += 1;
    }

    let mut range_queries: Vec<(usize, usize)> =
        vec![(0, 0), (0, text.len()), (0, usize::MAX), (text.len(), text.len()), (text.len() / 2, text.len() / 3)];
    // Queries at unit boundaries (spans exactly at the ends) and around them.
    if !units.is_empty() {
        for _ in 0..60 {
            let a = &units[rng.below(units.len())];
            let b = &units[rng.below(units.len())];
            range_queries.push((a.range.start, b.range.end));
            range_queries.push((a.range.start, a.range.end));
            range_queries.push((a.range.start + 1, b.range.end));
            range_queries.push((a.range.start, b.range.end.saturating_sub(1)));
            range_queries.push((a.range.end, b.range.start));
        }
    }
    for _ in 0..40 {
        let start = rng.below(text.len() + 2);
        range_queries.push((start, start + rng.below(200)));
    }
    for (start, end) in range_queries {
        let scanned: Vec<*const latex::SourceUnit> = units
            .iter()
            .filter(|unit| unit.range.start >= start && unit.range.end <= end)
            .map(std::ptr::from_ref)
            .collect();
        let indexed: Vec<*const latex::SourceUnit> =
            file.units_within(start, end).into_iter().map(std::ptr::from_ref).collect();
        assert_eq!(indexed, scanned, "{context}: bytes {start}..{end}");
        seen.lookups += 1;
    }

    // Deferred streams: the units of each, in unit order.
    for stream in &file.model.deferred_streams {
        let scanned: Vec<usize> = (0..units.len()).filter(|&i| units[i].stream == *stream).collect();
        assert_eq!(file.index.deferred.get(stream), Some(&scanned), "{context}: deferred stream {stream}");
        seen.deferred += 1;
    }
}

#[test]
fn indexed_lookups_equal_the_scans_on_generated_sources() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut seen = Seen::default();
    let astral = "\u{1d538}".repeat(1024);
    let fixed = ["", "\n", "\r\n", "\u{1f600}", "a", "\\title{T}\\maketitle", astral.as_str()];
    for (round, text) in fixed.iter().enumerate() {
        compare(&file(text), &mut rng, &mut seen, &format!("fixed {round}"));
    }
    for round in 0..120 {
        let text = generate(&mut rng);
        compare(&file(&text), &mut rng, &mut seen, &format!("generated {round}"));
    }
    eprintln!(
        "{} files ({} with several marks, {} line-unordered, {} range-unordered), {} deferred streams, {} lookups",
        seen.files, seen.multi_mark, seen.lines_unordered, seen.ranges_unordered, seen.deferred, seen.lookups
    );
    assert!(seen.multi_mark > 50 && seen.deferred > 20, "the generated sources must exercise marks and streams");
}

/// The parser's units put out of order (and with overlapping ranges): the
/// index must notice and give what the scan gives.
#[test]
fn indexed_lookups_equal_the_scans_when_units_are_out_of_order() {
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    let mut seen = Seen::default();
    for round in 0..80 {
        let text: Arc<str> = Arc::from(generate(&mut rng));
        let mut model = latex::parse(&text);
        if model.units.len() < 4 {
            continue;
        }
        match round % 4 {
            // A few swaps.
            0 => {
                for _ in 0..1 + rng.below(4) {
                    let (a, b) = (rng.below(model.units.len()), rng.below(model.units.len()));
                    model.units.swap(a, b);
                }
            }
            // Reversed.
            1 => model.units.reverse(),
            // Lines ordered, ranges not: one range stretched over its successors.
            2 => {
                let at = rng.below(model.units.len() - 1);
                model.units[at].range.end = text.len();
            }
            // Ranges ordered, lines not.
            _ => {
                let at = rng.below(model.units.len() - 1);
                model.units[at].line = u32::MAX - 1;
            }
        }
        let index = SourceIndex::new(&text, &model);
        let file = SourceFile { path: WorkspacePath::parse("main.tex").unwrap(), text, model, index };
        compare(&file, &mut rng, &mut seen, &format!("shuffled {round}"));
    }
    eprintln!(
        "{} files, {} line-unordered, {} range-unordered, {} lookups",
        seen.files, seen.lines_unordered, seen.ranges_unordered, seen.lookups
    );
    assert!(seen.lines_unordered > 20 && seen.ranges_unordered > 20, "the cases must defeat the ordered paths");
}
