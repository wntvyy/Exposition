//! Markdown → Typst translation.

use std::collections::HashMap;
use std::ops::Range;

use exposition_core::build::Severity;
use exposition_core::path::WorkspacePath;
use exposition_core::text::TextRange;
use pulldown_cmark::{Alignment, CodeBlockKind, Event, LinkType, Options, Parser, Tag};

use crate::emit::{Emitter, typst_string};
use crate::front_matter::{self, FrontMatter, Scalar};
use crate::map::{SegmentKind, SourceMap};
use crate::math;
use crate::profile::{self, Layout, PROFILE_ID};
use crate::{EquationRef, ImageRef, MarkdownDiagnostic, TranslateOptions, Translation};

/// Parser options of the profile: CommonMark plus GFM tables, footnotes,
/// strikethrough and task lists, `$`/`$$` mathematics, and YAML front matter.
pub fn parser_options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_MATH
        | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS
}

/// A parsed element or event with its Markdown byte range.
#[derive(Debug)]
pub(crate) enum Node<'s> {
    El { tag: Tag<'s>, range: Range<usize>, children: Vec<Node<'s>> },
    Leaf { ev: Event<'s>, range: Range<usize> },
}

impl Node<'_> {
    pub(crate) fn range(&self) -> &Range<usize> {
        match self {
            Node::El { range, .. } | Node::Leaf { range, .. } => range,
        }
    }

    fn is_inline(&self) -> bool {
        match self {
            Node::El { tag, .. } => matches!(
                tag,
                Tag::Emphasis
                    | Tag::Strong
                    | Tag::Strikethrough
                    | Tag::Superscript
                    | Tag::Subscript
                    | Tag::Link { .. }
                    | Tag::Image { .. }
            ),
            Node::Leaf { ev, .. } => !matches!(ev, Event::Rule),
        }
    }
}

pub(crate) struct Tree<'s> {
    pub(crate) blocks: Vec<Node<'s>>,
    pub(crate) footnotes: Vec<(String, Range<usize>, Vec<Node<'s>>)>,
    /// YAML content range and the whole block range.
    pub(crate) metadata: Option<(Range<usize>, Range<usize>)>,
}

pub(crate) fn parse_tree(md: &str) -> Tree<'_> {
    let mut stack: Vec<(Tag, Range<usize>, Vec<Node>)> = Vec::new();
    let mut root: Vec<Node> = Vec::new();
    for (ev, range) in Parser::new_ext(md, parser_options()).into_offset_iter() {
        match ev {
            Event::Start(tag) => stack.push((tag, range, Vec::new())),
            Event::End(_) => {
                let (tag, range, children) = stack.pop().expect("balanced events");
                let node = Node::El { tag, range, children };
                match stack.last_mut() {
                    Some(parent) => parent.2.push(node),
                    None => root.push(node),
                }
            }
            ev => {
                let node = Node::Leaf { ev, range };
                match stack.last_mut() {
                    Some(parent) => parent.2.push(node),
                    None => root.push(node),
                }
            }
        }
    }
    let mut blocks = Vec::with_capacity(root.len());
    let mut footnotes = Vec::new();
    let mut metadata = None;
    for node in root {
        match node {
            Node::El { tag: Tag::FootnoteDefinition(label), range, children } => {
                footnotes.push((label.to_string(), range, children));
            }
            Node::El { tag: Tag::MetadataBlock(_), range, children } => {
                let content = children.iter().map(|c| c.range().clone()).reduce(|a, b| a.start..b.end);
                let content = content.unwrap_or(range.start..range.start);
                metadata = Some((content, range));
            }
            other => blocks.push(other),
        }
    }
    Tree { blocks, footnotes, metadata }
}

/// Whether the backslash before byte `p` escapes the character at `p`.
pub(crate) fn escaped_at(md: &str, p: usize) -> bool {
    if p == 0 || !md[p..].starts_with(|c: char| c.is_ascii_punctuation()) {
        return false;
    }
    let backslashes = md[..p].bytes().rev().take_while(|&b| b == b'\\').count();
    backslashes % 2 == 1
}

struct Tx<'s> {
    e: Emitter<'s>,
    footnotes: HashMap<String, usize>,
    defs: Vec<(String, Range<usize>, Vec<Node<'s>>)>,
    /// Label index of footnotes already printed.
    printed: HashMap<String, usize>,
    in_progress: Vec<String>,
    referenced: Vec<bool>,
    diagnostics: Vec<(Severity, String, Range<usize>, Vec<String>)>,
    equations: Vec<(Range<usize>, TextRange, bool)>,
    images: Vec<(Range<usize>, Option<WorkspacePath>, String)>,
    document: Option<WorkspacePath>,
}

pub(crate) fn translate(md: &str, options: &TranslateOptions) -> Translation {
    let tree = parse_tree(md);
    let mut tx = Tx {
        e: Emitter::new(md),
        footnotes: HashMap::new(),
        defs: Vec::new(),
        printed: HashMap::new(),
        in_progress: Vec::new(),
        referenced: Vec::new(),
        diagnostics: Vec::new(),
        equations: Vec::new(),
        images: Vec::new(),
        document: options.document.clone(),
    };
    let Tree { blocks, footnotes, metadata } = tree;
    for (i, (label, range, children)) in footnotes.into_iter().enumerate() {
        tx.footnotes.entry(label.clone()).or_insert(i);
        tx.defs.push((label, range, children));
        tx.referenced.push(false);
    }

    // Front matter and the template.
    let fm = metadata.map(|(content, block)| front_matter::parse(&md[content.clone()], content.start, block));
    let mut layout = Layout::default();
    if let Some(fm) = &fm {
        for p in &fm.problems {
            tx.diag(Severity::Warning, p.message.clone(), p.range.clone());
        }
        let (l, problems) = profile::layout(fm);
        layout = l;
        for p in problems {
            tx.diag(Severity::Warning, p.message, p.range);
        }
        for entry in &fm.entries {
            if !profile::KEYS.contains(&entry.key.as_str()) {
                tx.diagnostics.push((
                    Severity::Info,
                    format!("front matter key `{}` is not used by the print profile", entry.key),
                    entry.key_range.clone(),
                    vec![format!("Recognised keys: {}.", profile::KEYS.join(", "))],
                ));
            }
        }
    }
    let document_args = fm.as_ref().map(document_args).unwrap_or_default();
    tx.e.template(&profile::preamble(&layout, &document_args));
    tx.e.template("\n");
    tx.e.line_start();
    if let Some(fm) = &fm {
        tx.title_block(fm);
    }

    tx.flow(&blocks, None);
    tx.e.template("\n");

    for (i, (label, range, _)) in tx.defs.iter().enumerate() {
        if !tx.referenced[i] {
            tx.diagnostics.push((
                Severity::Warning,
                format!("footnote `{label}` is defined but never referenced; it is not printed"),
                range.clone(),
                Vec::new(),
            ));
        }
    }

    let md16 = crate::emit::Utf16Index::new(md);
    let Tx { e, diagnostics, equations, images, .. } = tx;
    let markdown_len = e.md16.len();
    let (typst, segments, generated_len) = e.finish();
    let diagnostics = diagnostics
        .into_iter()
        .map(|(severity, message, range, hints)| MarkdownDiagnostic {
            severity,
            message,
            range: md16.range(&range),
            hints,
        })
        .collect();
    let equations = equations
        .into_iter()
        .map(|(range, generated, block)| EquationRef { markdown: md16.range(&range), generated, block })
        .collect();
    let images = images
        .into_iter()
        .map(|(range, path, source)| ImageRef { markdown: md16.range(&range), path, source })
        .collect();
    Translation {
        typst,
        map: SourceMap::new(segments, generated_len, markdown_len),
        diagnostics,
        equations,
        images,
        front_matter: fm,
        layout,
        profile: PROFILE_ID,
    }
}

fn document_args(fm: &FrontMatter) -> String {
    let mut args = Vec::new();
    if let Some(title) = fm.scalar("title") {
        args.push(format!("title: {}", typst_string(&title.value)));
    }
    if let Some(author) = fm.get("author") {
        let names: Vec<String> = author.values.iter().map(|s| typst_string(&s.value)).collect();
        if !names.is_empty() {
            args.push(format!("author: ({},)", names.join(", ")));
        }
    }
    args.join(", ")
}

impl<'s> Tx<'s> {
    fn diag(&mut self, severity: Severity, message: String, range: Range<usize>) {
        self.diagnostics.push((severity, message, range, Vec::new()));
    }

    fn syntax(&mut self, text: &str, owner: &Range<usize>) {
        self.e.syntax(text, owner);
    }

    /// Open a content block (`[`): markup line-start rules apply inside.
    fn open_content(&mut self, text: &str, owner: &Range<usize>) {
        self.e.syntax(text, owner);
        self.e.line_start();
    }

    /// Close an embedded code expression (`]` / `)`).
    fn close_code(&mut self, text: &str, owner: &Range<usize>) {
        self.e.syntax(text, owner);
        self.e.end_code();
    }

    fn parbreak(&mut self, owner: &Range<usize>) {
        self.e.syntax("\n\n", owner);
        self.e.line_start();
    }

    // ------------------------------------------------------------ title --

    fn scalar_content(&mut self, s: &Scalar) {
        if s.linear {
            let chars: Vec<(usize, char)> = s.value.char_indices().collect();
            for (k, &(i, c)) in chars.iter().enumerate() {
                let at = s.range.start + i;
                let next = chars.get(k + 1).map(|x| x.1);
                self.e.markup_char(c, next, &(at..at + c.len_utf8()), true);
            }
        } else {
            self.e.markup_atom(&s.value, &s.range);
        }
    }

    fn title_block(&mut self, fm: &FrontMatter) {
        let title = fm.scalar("title");
        let authors = fm.get("author").map(|e| e.values.as_slice()).unwrap_or(&[]);
        let date = fm.scalar("date");
        if title.is_none() && authors.is_empty() && date.is_none() {
            return;
        }
        let owner = fm.block.clone();
        self.syntax("#exmd-title(", &owner);
        let mut first = true;
        if let Some(t) = title {
            self.open_content("title: [", &owner);
            self.scalar_content(t);
            self.syntax("]", &owner);
            first = false;
        }
        if !authors.is_empty() {
            if !first {
                self.syntax(", ", &owner);
            }
            self.syntax("authors: (", &owner);
            for a in authors {
                self.open_content("[", &owner);
                self.scalar_content(a);
                self.syntax("], ", &owner);
            }
            self.syntax(")", &owner);
            first = false;
        }
        if let Some(d) = date {
            if !first {
                self.syntax(", ", &owner);
            }
            self.open_content("date: [", &owner);
            self.scalar_content(d);
            self.syntax("]", &owner);
        }
        self.close_code(")", &owner);
        self.parbreak(&owner);
    }

    // ----------------------------------------------------------- blocks --

    /// Emit block content: runs of inline nodes form paragraphs; blocks are
    /// separated by paragraph breaks.
    fn flow(&mut self, nodes: &[Node<'s>], _container: Option<&Range<usize>>) {
        let mut first = true;
        let mut i = 0;
        while i < nodes.len() {
            if nodes[i].is_inline() {
                let mut j = i;
                while j < nodes.len() && nodes[j].is_inline() {
                    j += 1;
                }
                if !first {
                    let owner = nodes[i].range().clone();
                    self.parbreak(&owner);
                }
                self.inlines(&nodes[i..j]);
                i = j;
            } else {
                if !first {
                    let owner = nodes[i].range().clone();
                    self.parbreak(&owner);
                }
                self.block(&nodes[i]);
                i += 1;
            }
            first = false;
        }
    }

    fn block(&mut self, node: &Node<'s>) {
        let Node::El { tag, range, children } = node else {
            if let Node::Leaf { ev: Event::Rule, range } = node {
                self.syntax("#exmd-rule()", range);
                self.e.end_code();
            } else {
                self.inlines(std::slice::from_ref(node));
            }
            return;
        };
        match tag {
            Tag::Paragraph => {
                if let Some(image) = lone_image(children) {
                    self.figure(image);
                } else {
                    self.inlines(children);
                }
            }
            Tag::Heading { level, .. } => {
                let level = *level as u8;
                self.open_content(&format!("#heading(level: {level})["), range);
                self.inlines(children);
                self.close_code("]", range);
            }
            Tag::BlockQuote(_) => {
                self.open_content("#exmd-quote[", range);
                self.flow(children, Some(range));
                self.close_code("]", range);
            }
            Tag::CodeBlock(kind) => self.code_block(kind, range, children),
            Tag::HtmlBlock => {
                self.diag(
                    Severity::Warning,
                    "raw HTML is not interpreted by the print profile; it is printed verbatim".into(),
                    range.clone(),
                );
                let pieces = self.leaf_text_pieces(children);
                self.syntax("#exmd-html(block: true, ", range);
                self.e.string_literal(&pieces, range);
                self.close_code(")", range);
            }
            Tag::List(start) => self.list(*start, range, children),
            Tag::Table(aligns) => self.table(aligns, range, children),
            Tag::Item | Tag::FootnoteDefinition(_) | Tag::MetadataBlock(_) => self.flow(children, Some(range)),
            Tag::TableHead | Tag::TableRow | Tag::TableCell => self.inlines(children),
            Tag::DefinitionList | Tag::DefinitionListTitle | Tag::DefinitionListDefinition => {
                self.flow(children, Some(range))
            }
            _ => self.inlines(children),
        }
    }

    /// (char, Markdown byte range) pairs for the text leaves of a code or
    /// HTML block; `\r` before `\n` is dropped.
    fn leaf_text_pieces(&self, children: &[Node<'s>]) -> Vec<(char, Range<usize>)> {
        let md = self.e.md;
        let mut pieces = Vec::new();
        for child in children {
            let Node::Leaf { ev: Event::Text(t) | Event::Html(t) | Event::InlineHtml(t), range } = child else {
                continue;
            };
            let src = &md[range.clone()];
            if src == t.as_ref() {
                let mut it = src.char_indices().peekable();
                while let Some((i, c)) = it.next() {
                    if c == '\r' && it.peek().is_some_and(|&(_, n)| n == '\n') {
                        continue;
                    }
                    let at = range.start + i;
                    pieces.push((c, at..at + c.len_utf8()));
                }
            } else {
                for c in t.chars() {
                    if c != '\r' {
                        pieces.push((c, range.clone()));
                    }
                }
            }
        }
        pieces
    }

    fn code_block(&mut self, kind: &CodeBlockKind<'s>, range: &Range<usize>, children: &[Node<'s>]) {
        let lang = match kind {
            CodeBlockKind::Fenced(info) => info
                .split_whitespace()
                .next()
                .map(|l| l.trim_matches(|c| c == '{' || c == '}' || c == '.'))
                .filter(|l| !l.is_empty() && l.chars().all(|c| c.is_alphanumeric() || "+-#_.".contains(c)))
                .map(str::to_owned),
            CodeBlockKind::Indented => None,
        };
        let mut pieces = self.leaf_text_pieces(children);
        // The final newline ends the block; it is not part of the code.
        if pieces.last().is_some_and(|p| p.0 == '\n') {
            pieces.pop();
        }
        match &lang {
            Some(lang) => self.syntax(&format!("#raw(block: true, lang: {}, ", typst_string(lang)), range),
            None => self.syntax("#raw(block: true, ", range),
        }
        self.e.string_literal(&pieces, range);
        self.close_code(")", range);
    }

    fn list(&mut self, start: Option<u64>, range: &Range<usize>, items: &[Node<'s>]) {
        let tight = !items.iter().any(|item| match item {
            Node::El { children, .. } => children.iter().any(|c| matches!(c, Node::El { tag: Tag::Paragraph, .. })),
            _ => false,
        });
        match start {
            Some(n) => {
                let md = self.e.md;
                let marker_delim = items
                    .first()
                    .map(|i| md[i.range().start..].trim_start_matches(|c: char| c.is_ascii_digit()))
                    .and_then(|rest| rest.chars().next())
                    .filter(|&c| c == ')')
                    .map_or("1.", |_| "1)");
                self.syntax(&format!("#enum(start: {n}, tight: {tight}, numbering: \"{marker_delim}\""), range);
            }
            None => self.syntax(&format!("#list(tight: {tight}"), range),
        }
        for item in items {
            let Node::El { range: item_range, children, .. } = item else { continue };
            self.open_content(", [", item_range);
            self.flow(children, Some(item_range));
            self.syntax("]", item_range);
        }
        self.close_code(")", range);
    }

    fn table(&mut self, aligns: &[Alignment], range: &Range<usize>, rows: &[Node<'s>]) {
        let columns = aligns.len().max(1);
        let align: Vec<&str> = aligns
            .iter()
            .map(|a| match a {
                Alignment::Center => "center",
                Alignment::Right => "right",
                Alignment::Left | Alignment::None => "left",
            })
            .collect();
        self.syntax(&format!("#table(columns: {columns}, align: ({},)", align.join(", ")), range);
        for row in rows {
            let Node::El { tag, range: row_range, children } = row else { continue };
            let header = matches!(tag, Tag::TableHead);
            if header {
                self.syntax(", table.header(", row_range);
            }
            let mut count = 0;
            for (k, cell) in children.iter().enumerate() {
                let Node::El { range: cell_range, children: inline, .. } = cell else { continue };
                if count >= columns {
                    break;
                }
                let sep = if header && k == 0 { "[" } else { ", [" };
                self.open_content(sep, cell_range);
                self.inlines(inline);
                self.syntax("]", cell_range);
                count += 1;
            }
            while count < columns {
                let sep = if header && count == 0 { "[]" } else { ", []" };
                self.syntax(sep, row_range);
                count += 1;
            }
            if header {
                self.syntax(")", row_range);
            }
        }
        self.close_code(")", range);
    }

    // ---------------------------------------------------------- inlines --

    fn inlines(&mut self, nodes: &[Node<'s>]) {
        for node in nodes {
            self.inline(node);
        }
    }

    fn inline(&mut self, node: &Node<'s>) {
        match node {
            Node::Leaf { ev, range } => self.leaf(ev, range),
            Node::El { tag, range, children } => match tag {
                Tag::Emphasis => self.wrap("#emph[", range, children),
                Tag::Strong => self.wrap("#strong[", range, children),
                Tag::Strikethrough => self.wrap("#strike[", range, children),
                Tag::Superscript => self.wrap("#super[", range, children),
                Tag::Subscript => self.wrap("#sub[", range, children),
                Tag::Link { link_type, dest_url, .. } => {
                    let dest = match link_type {
                        LinkType::Email => format!("mailto:{dest_url}"),
                        _ => dest_url.to_string(),
                    };
                    if dest.is_empty() {
                        self.inlines(children);
                    } else {
                        let open = format!("#link({})[", typst_string(&dest));
                        self.wrap(&open, range, children);
                    }
                }
                Tag::Image { dest_url, .. } => self.inline_image(dest_url, range, children),
                _ => self.block(node),
            },
        }
    }

    fn wrap(&mut self, open: &str, range: &Range<usize>, children: &[Node<'s>]) {
        self.open_content(open, range);
        self.inlines(children);
        self.close_code("]", range);
    }

    fn leaf(&mut self, ev: &Event<'s>, range: &Range<usize>) {
        match ev {
            Event::Text(t) => self.text(t, range),
            Event::Code(t) => self.code_span(t, range),
            Event::InlineMath(t) => self.math(t, range, false),
            Event::DisplayMath(t) => self.math(t, range, true),
            Event::Html(t) | Event::InlineHtml(t) => {
                self.diag(
                    Severity::Warning,
                    "raw HTML is not interpreted by the print profile; it is printed verbatim".into(),
                    range.clone(),
                );
                let _ = t;
                let pieces =
                    self.leaf_text_pieces(std::slice::from_ref(&Node::Leaf { ev: ev.clone(), range: range.clone() }));
                self.syntax("#exmd-html(", range);
                self.e.string_literal(&pieces, range);
                self.close_code(")", range);
            }
            Event::FootnoteReference(label) => self.footnote(label, range),
            Event::SoftBreak => self.e.mapped(" ", range, SegmentKind::Atom),
            Event::HardBreak => self.close_code("#linebreak()", range),
            Event::TaskListMarker(done) => {
                self.close_code(&format!("#exmd-task({done})"), range);
                self.syntax(" ", range);
            }
            Event::Rule => {
                self.syntax("#exmd-rule()", range);
                self.e.end_code();
            }
            Event::Start(_) | Event::End(_) => {}
        }
    }

    fn text(&mut self, text: &str, range: &Range<usize>) {
        let md = self.e.md;
        let src = &md[range.clone()];
        if src != text {
            // An entity or other decoded spelling: one rendered unit.
            self.e.markup_atom(text, range);
            return;
        }
        let chars: Vec<(usize, char)> = text.char_indices().collect();
        for (k, &(i, c)) in chars.iter().enumerate() {
            let at = range.start + i;
            let next = chars.get(k + 1).map(|x| x.1);
            if k == 0 && escaped_at(md, at) {
                self.e.markup_char(c, next, &(at - 1..at + c.len_utf8()), false);
            } else {
                self.e.markup_char(c, next, &(at..at + c.len_utf8()), true);
            }
        }
    }

    fn code_span(&mut self, content: &str, range: &Range<usize>) {
        let md = self.e.md;
        let src = &md[range.clone()];
        let ticks = src.bytes().take_while(|&b| b == b'`').count();
        let inner = range.start + ticks..range.end - ticks;
        // CommonMark strips one space on each side when both are present.
        let candidates = [inner.clone(), inner.start + 1..inner.end.saturating_sub(1)];
        let exact = candidates.into_iter().find(|r| r.start <= r.end && md.get(r.clone()) == Some(content));
        let pieces: Vec<(char, Range<usize>)> = match exact {
            Some(r) => content.char_indices().map(|(i, c)| (c, r.start + i..r.start + i + c.len_utf8())).collect(),
            None => content.chars().map(|c| (c, range.clone())).collect(),
        };
        self.syntax("#raw(", range);
        self.e.string_literal(&pieces, range);
        self.close_code(")", range);
    }

    fn math(&mut self, content: &str, range: &Range<usize>, block: bool) {
        let md = self.e.md;
        let delim = if block { 2 } else { 1 };
        let mut inner = range.start + delim..range.end.saturating_sub(delim);
        if md.get(inner.clone()) != Some(content) {
            // Locate the content (defensive; the parser does not rewrite math).
            match md[range.clone()].find(content) {
                Some(i) => inner = range.start + i..range.start + i + content.len(),
                None => {
                    self.diag(Severity::Error, "could not locate the mathematics source".into(), range.clone());
                    self.e.unsupported(&format!("#exmd-unsupported({})", typst_string(content)), range);
                    return;
                }
            }
        }
        let start = self.e.generated_len();
        let out = math::emit_equation(&mut self.e, inner, range.clone(), block);
        let end = self.e.generated_len();
        self.equations.push((range.clone(), TextRange::new(start, end), block));
        for p in out.problems {
            let severity = if p.marker { Severity::Warning } else { Severity::Info };
            let hints = if p.marker {
                vec![
                    "The Markdown print profile supports a declared LaTeX math subset; see docs/markdown-profile.md."
                        .to_owned(),
                ]
            } else {
                Vec::new()
            };
            self.diagnostics.push((severity, p.message, p.range, hints));
        }
    }

    fn footnote(&mut self, label: &str, range: &Range<usize>) {
        let Some(&index) = self.footnotes.get(label) else {
            self.diag(Severity::Warning, format!("footnote `{label}` is not defined"), range.clone());
            self.e.unsupported(&format!("#exmd-unsupported({})", typst_string(&format!("[^{label}]"))), range);
            return;
        };
        self.referenced[index] = true;
        if let Some(&n) = self.printed.get(label) {
            self.close_code(&format!("#footnote(<exmd-fn-{n}>)"), range);
            return;
        }
        if self.in_progress.iter().any(|l| l == label) {
            self.diag(Severity::Warning, format!("footnote `{label}` refers to itself"), range.clone());
            self.e.unsupported(&format!("#exmd-unsupported({})", typst_string(&format!("[^{label}]"))), range);
            return;
        }
        let n = self.printed.len() + 1;
        self.in_progress.push(label.to_owned());
        let children = std::mem::take(&mut self.defs[index].2);
        let def_range = self.defs[index].1.clone();
        self.open_content("#footnote[", range);
        self.flow(&children, Some(&def_range));
        self.syntax("]", range);
        self.syntax(&format!("<exmd-fn-{n}>"), range);
        self.defs[index].2 = children;
        self.in_progress.pop();
        self.printed.insert(label.to_owned(), n);
    }

    // ----------------------------------------------------------- images --

    fn resolve_image(&mut self, dest: &str, range: &Range<usize>) -> Option<WorkspacePath> {
        match resolve_image_path(dest, self.document.as_ref()) {
            Ok(path) => {
                self.images.push((range.clone(), Some(path.clone()), dest.to_owned()));
                Some(path)
            }
            Err(reason) => {
                self.images.push((range.clone(), None, dest.to_owned()));
                self.diagnostics.push((
                    Severity::Error,
                    format!("image `{dest}` cannot be used: {reason}"),
                    range.clone(),
                    vec!["Images must be files inside the workspace, given relative to the document.".into()],
                ));
                None
            }
        }
    }

    fn figure(&mut self, image: &Node<'s>) {
        let Node::El { tag: Tag::Image { dest_url, .. }, range, children } = image else { return };
        let Some(path) = self.resolve_image(dest_url, range) else {
            self.missing_image(dest_url, range);
            return;
        };
        let alt = plain_text(children);
        self.syntax(
            &format!("#figure(exmd-image({}, alt: {})", typst_string(&format!("/{path}")), typst_string(&alt)),
            range,
        );
        if !children.is_empty() {
            self.open_content(", caption: [", range);
            self.inlines(children);
            self.syntax("]", range);
        }
        self.close_code(")", range);
    }

    fn inline_image(&mut self, dest: &str, range: &Range<usize>, children: &[Node<'s>]) {
        let Some(path) = self.resolve_image(dest, range) else {
            self.missing_image(dest, range);
            return;
        };
        let alt = plain_text(children);
        self.close_code(
            &format!("#exmd-inline-image({}, alt: {})", typst_string(&format!("/{path}")), typst_string(&alt)),
            range,
        );
    }

    fn missing_image(&mut self, dest: &str, range: &Range<usize>) {
        let text = format!("#exmd-missing-image({})", typst_string(dest));
        self.e.unsupported(&text, range);
        self.e.end_code();
    }
}

/// The paragraph's only content is one image (surrounding whitespace aside).
fn lone_image<'a, 's>(children: &'a [Node<'s>]) -> Option<&'a Node<'s>> {
    let mut image = None;
    for c in children {
        match c {
            Node::El { tag: Tag::Image { .. }, .. } if image.is_none() => image = Some(c),
            Node::Leaf { ev: Event::Text(t), .. } if t.trim().is_empty() => {}
            Node::Leaf { ev: Event::SoftBreak, .. } => {}
            _ => return None,
        }
    }
    image
}

fn plain_text(nodes: &[Node]) -> String {
    let mut s = String::new();
    for n in nodes {
        match n {
            Node::Leaf { ev: Event::Text(t) | Event::Code(t) | Event::InlineMath(t), .. } => s.push_str(t),
            Node::Leaf { ev: Event::SoftBreak | Event::HardBreak, .. } => s.push(' '),
            Node::El { children, .. } => s.push_str(&plain_text(children)),
            _ => {}
        }
    }
    s
}

/// Resolve an image destination against the document's directory, confined
/// to the workspace.
pub fn resolve_image_path(dest: &str, document: Option<&WorkspacePath>) -> Result<WorkspacePath, String> {
    let dest = dest.trim();
    if dest.is_empty() {
        return Err("empty path".into());
    }
    let lower = dest.to_ascii_lowercase();
    if lower.contains("://") || lower.starts_with("data:") || lower.starts_with("mailto:") || lower.starts_with("file:")
    {
        return Err("remote and data URLs are not loaded".into());
    }
    let decoded = percent_decode(dest).ok_or_else(|| "invalid percent-encoding".to_owned())?;
    if decoded.contains('\\') || decoded.contains('\0') {
        return Err("backslashes are not allowed in image paths".into());
    }
    let bytes = decoded.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        return Err("absolute paths are not allowed".into());
    }
    let (root_relative, rest) = match decoded.strip_prefix('/') {
        Some(rest) => (true, rest),
        None => (false, decoded.as_str()),
    };
    let mut parts: Vec<String> = Vec::new();
    if !root_relative && let Some(parent) = document.and_then(WorkspacePath::parent) {
        parts.extend(parent.components().map(str::to_owned));
    }
    for component in rest.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return Err("the path leaves the workspace".into());
                }
            }
            c => parts.push(c.to_owned()),
        }
    }
    WorkspacePath::parse(&parts.join("/")).map_err(|e| e.to_string())
}

fn percent_decode(s: &str) -> Option<String> {
    if !s.contains('%') {
        return Some(s.to_owned());
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = s.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_paths_are_confined() {
        let doc = WorkspacePath::parse("docs/paper/main.md").unwrap();
        let ok = |d: &str| resolve_image_path(d, Some(&doc)).unwrap().to_string();
        assert_eq!(ok("fig.png"), "docs/paper/fig.png");
        assert_eq!(ok("./img/a%20b.png"), "docs/paper/img/a b.png");
        assert_eq!(ok("../shared/x.png"), "docs/shared/x.png");
        assert_eq!(ok("/assets/logo.png"), "assets/logo.png");
        for bad in ["../../../x.png", "https://example.com/a.png", "C:/x.png", "a\\b.png", "data:image/png;base64,AA"] {
            assert!(resolve_image_path(bad, Some(&doc)).is_err(), "{bad}");
        }
    }

    #[test]
    fn escape_detection() {
        assert!(escaped_at("a\\*b", 2));
        assert!(!escaped_at("a\\\\*b", 3));
        assert!(!escaped_at("a\\b", 2));
    }
}
