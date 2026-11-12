//! The declared LaTeX math subset and its translation to Typst math.
//!
//! Every LaTeX token becomes Typst text with its own segment: letters, digits
//! and operator characters are copied one to one ([`SegmentKind::MathText`]);
//! commands that render one symbol become that Unicode character
//! ([`SegmentKind::MathAtom`], e.g. `\alpha` → `α`); structure (`\frac`,
//! braces, `^`, separators) becomes Typst syntax mapped to its LaTeX owner.
//! Constructs outside the subset render as a visible marker and are reported.

use std::ops::Range;

use crate::emit::{Emitter, typst_string};
use crate::map::SegmentKind;

/// A problem found while translating mathematics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MathProblem {
    /// Markdown byte range of the offending construct.
    pub range: Range<usize>,
    pub message: String,
    /// True for constructs rendered as an error marker; false for notes
    /// (e.g. ignored sizing hints).
    pub marker: bool,
}

// ---------------------------------------------------------------- tables --

/// Commands that render one symbol: LaTeX name → Unicode text.
const SYMBOLS: &[(&str, &str)] = &[
    // Greek lowercase.
    ("alpha", "α"),
    ("beta", "β"),
    ("gamma", "γ"),
    ("delta", "δ"),
    ("epsilon", "ϵ"),
    ("varepsilon", "ε"),
    ("zeta", "ζ"),
    ("eta", "η"),
    ("theta", "θ"),
    ("vartheta", "ϑ"),
    ("iota", "ι"),
    ("kappa", "κ"),
    ("varkappa", "ϰ"),
    ("lambda", "λ"),
    ("mu", "μ"),
    ("nu", "ν"),
    ("xi", "ξ"),
    ("omicron", "ο"),
    ("pi", "π"),
    ("varpi", "ϖ"),
    ("rho", "ρ"),
    ("varrho", "ϱ"),
    ("sigma", "σ"),
    ("varsigma", "ς"),
    ("tau", "τ"),
    ("upsilon", "υ"),
    ("phi", "ϕ"),
    ("varphi", "φ"),
    ("chi", "χ"),
    ("psi", "ψ"),
    ("omega", "ω"),
    // Greek uppercase.
    ("Gamma", "Γ"),
    ("Delta", "Δ"),
    ("Theta", "Θ"),
    ("Lambda", "Λ"),
    ("Xi", "Ξ"),
    ("Pi", "Π"),
    ("Sigma", "Σ"),
    ("Upsilon", "Υ"),
    ("Phi", "Φ"),
    ("Psi", "Ψ"),
    ("Omega", "Ω"),
    // Relations.
    ("le", "≤"),
    ("leq", "≤"),
    ("ge", "≥"),
    ("geq", "≥"),
    ("leqslant", "⩽"),
    ("geqslant", "⩾"),
    ("ne", "≠"),
    ("neq", "≠"),
    ("approx", "≈"),
    ("equiv", "≡"),
    ("sim", "∼"),
    ("simeq", "≃"),
    ("cong", "≅"),
    ("propto", "∝"),
    ("ll", "≪"),
    ("gg", "≫"),
    ("subset", "⊂"),
    ("supset", "⊃"),
    ("subseteq", "⊆"),
    ("supseteq", "⊇"),
    ("subsetneq", "⊊"),
    ("in", "∈"),
    ("notin", "∉"),
    ("ni", "∋"),
    ("perp", "⊥"),
    ("parallel", "∥"),
    ("mid", "∣"),
    ("vdash", "⊢"),
    ("models", "⊨"),
    ("prec", "≺"),
    ("succ", "≻"),
    ("preceq", "⪯"),
    ("succeq", "⪰"),
    ("doteq", "≐"),
    ("asymp", "≍"),
    // Arrows.
    ("to", "→"),
    ("rightarrow", "→"),
    ("leftarrow", "←"),
    ("gets", "←"),
    ("leftrightarrow", "↔"),
    ("Rightarrow", "⇒"),
    ("Leftarrow", "⇐"),
    ("Leftrightarrow", "⇔"),
    ("implies", "⟹"),
    ("impliedby", "⟸"),
    ("iff", "⟺"),
    ("mapsto", "↦"),
    ("longrightarrow", "⟶"),
    ("longleftarrow", "⟵"),
    ("longleftrightarrow", "⟷"),
    ("Longrightarrow", "⟹"),
    ("Longleftarrow", "⟸"),
    ("longmapsto", "⟼"),
    ("uparrow", "↑"),
    ("downarrow", "↓"),
    ("updownarrow", "↕"),
    ("hookrightarrow", "↪"),
    ("hookleftarrow", "↩"),
    ("nearrow", "↗"),
    ("searrow", "↘"),
    // Binary operators.
    ("pm", "±"),
    ("mp", "∓"),
    ("times", "×"),
    ("div", "÷"),
    ("cdot", "⋅"),
    ("ast", "∗"),
    ("star", "⋆"),
    ("circ", "∘"),
    ("bullet", "∙"),
    ("cup", "∪"),
    ("cap", "∩"),
    ("setminus", "∖"),
    ("wedge", "∧"),
    ("land", "∧"),
    ("vee", "∨"),
    ("lor", "∨"),
    ("oplus", "⊕"),
    ("ominus", "⊖"),
    ("otimes", "⊗"),
    ("odot", "⊙"),
    ("sqcup", "⊔"),
    ("sqcap", "⊓"),
    ("uplus", "⊎"),
    ("dagger", "†"),
    ("ddagger", "‡"),
    // Large operators (limits follow Typst's rules for the character).
    ("sum", "∑"),
    ("prod", "∏"),
    ("coprod", "∐"),
    ("int", "∫"),
    ("iint", "∬"),
    ("iiint", "∭"),
    ("oint", "∮"),
    ("bigcup", "⋃"),
    ("bigcap", "⋂"),
    ("bigoplus", "⨁"),
    ("bigotimes", "⨂"),
    ("bigodot", "⨀"),
    ("bigwedge", "⋀"),
    ("bigvee", "⋁"),
    ("bigsqcup", "⨆"),
    // Miscellaneous.
    ("infty", "∞"),
    ("partial", "∂"),
    ("nabla", "∇"),
    ("forall", "∀"),
    ("exists", "∃"),
    ("nexists", "∄"),
    ("neg", "¬"),
    ("lnot", "¬"),
    ("emptyset", "∅"),
    ("varnothing", "∅"),
    ("ell", "ℓ"),
    ("hbar", "ℏ"),
    ("Re", "ℜ"),
    ("Im", "ℑ"),
    ("aleph", "ℵ"),
    ("wp", "℘"),
    ("angle", "∠"),
    ("triangle", "△"),
    ("prime", "′"),
    ("top", "⊤"),
    ("bot", "⊥"),
    ("degree", "°"),
    ("ldots", "…"),
    ("dots", "…"),
    ("cdots", "⋯"),
    ("vdots", "⋮"),
    ("ddots", "⋱"),
    ("colon", ":"),
    ("backslash", "\\"),
    // Delimiters (outside \left/\right they render at natural size).
    ("langle", "⟨"),
    ("rangle", "⟩"),
    ("lfloor", "⌊"),
    ("rfloor", "⌋"),
    ("lceil", "⌈"),
    ("rceil", "⌉"),
    ("lvert", "|"),
    ("rvert", "|"),
    ("vert", "|"),
    ("lVert", "‖"),
    ("rVert", "‖"),
    ("Vert", "‖"),
];

/// Upright operator names that Typst defines with the same name.
const OPERATORS: &[&str] = &[
    "sin", "cos", "tan", "cot", "sec", "csc", "arcsin", "arccos", "arctan", "sinh", "cosh", "tanh", "coth", "exp",
    "log", "ln", "lg", "lim", "liminf", "limsup", "max", "min", "sup", "inf", "det", "dim", "ker", "deg", "gcd", "arg",
    "Pr", "hom",
];

/// Commands with one argument rendered by a Typst function (outermost first).
const UNARY: &[(&str, &[&str])] = &[
    ("sqrt", &["sqrt"]),
    ("hat", &["hat"]),
    ("widehat", &["hat"]),
    ("tilde", &["tilde"]),
    ("widetilde", &["tilde"]),
    ("bar", &["macron"]),
    ("overline", &["overline"]),
    ("underline", &["underline"]),
    ("vec", &["arrow"]),
    ("overrightarrow", &["arrow"]),
    ("dot", &["dot"]),
    ("ddot", &["dot.double"]),
    ("acute", &["acute"]),
    ("grave", &["grave"]),
    ("breve", &["breve"]),
    ("check", &["caron"]),
    ("mathring", &["circle"]),
    ("overbrace", &["overbrace"]),
    ("underbrace", &["underbrace"]),
    ("mathbb", &["bb"]),
    ("mathbf", &["upright", "bold"]),
    ("boldsymbol", &["bold"]),
    ("bm", &["bold"]),
    ("mathrm", &["upright"]),
    ("mathit", &["italic"]),
    ("mathcal", &["cal"]),
    ("mathscr", &["scr"]),
    ("mathfrak", &["frak"]),
    ("mathsf", &["sans"]),
    ("mathtt", &["mono"]),
];

/// Commands with two arguments rendered by a Typst function.
const BINARY: &[(&str, &str)] = &[
    ("frac", "frac"),
    ("dfrac", "frac"),
    ("tfrac", "frac"),
    ("cfrac", "frac"),
    ("binom", "binom"),
    ("dbinom", "binom"),
    ("tbinom", "binom"),
];

/// Spacing commands → Typst math spacing.
const SPACES: &[(&str, &str)] = &[
    (",", "thin"),
    (":", "med"),
    (">", "med"),
    (";", "thick"),
    ("quad", "quad"),
    ("qquad", "wide"),
    (" ", "thick"),
    ("!", "#h(-0.1667em)"),
];

/// `\not` combinations.
const NEGATIONS: &[(&str, &str)] = &[
    ("=", "≠"),
    ("<", "≮"),
    (">", "≯"),
    ("in", "∉"),
    ("equiv", "≢"),
    ("subset", "⊄"),
    ("supset", "⊅"),
    ("subseteq", "⊈"),
    ("supseteq", "⊉"),
    ("le", "≰"),
    ("leq", "≰"),
    ("ge", "≱"),
    ("geq", "≱"),
    ("sim", "≁"),
    ("approx", "≉"),
    ("cong", "≇"),
    ("mid", "∤"),
    ("parallel", "∦"),
];

/// Environments rendered as plain multi-line mathematics.
const LINE_ENVS: &[&str] =
    &["aligned", "align", "align*", "gathered", "gather", "gather*", "split", "equation", "equation*", "multline"];

/// Matrix environments → Typst `mat` delimiter.
const MATRIX_ENVS: &[(&str, &str)] = &[
    ("matrix", "#none"),
    ("smallmatrix", "#none"),
    ("pmatrix", "\"(\""),
    ("bmatrix", "\"[\""),
    ("Bmatrix", "\"{\""),
    ("vmatrix", "\"|\""),
    ("Vmatrix", "\"||\""),
];

/// Characters Typst math treats as delimiters; they are escaped so that
/// LaTeX's fixed-size, unmatched-allowed semantics are kept.
const DELIMITERS: &[char] = &['(', ')', '[', ']', '{', '}', '|', '⟨', '⟩', '⌊', '⌋', '⌈', '⌉', '‖'];

fn lookup<'a, T: Copy>(table: &'a [(&'a str, T)], name: &str) -> Option<T> {
    table.iter().find(|(k, _)| *k == name).map(|(_, v)| *v)
}

/// The rendered Unicode text of a symbol command (for tests and tools).
pub fn symbol_text(name: &str) -> Option<&'static str> {
    lookup(SYMBOLS, name)
}

/// Every command name of the declared subset (documentation and tests).
pub fn supported_commands() -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    out.extend(SYMBOLS.iter().map(|(k, _)| *k));
    out.extend(OPERATORS.iter().copied());
    out.extend(UNARY.iter().map(|(k, _)| *k));
    out.extend(BINARY.iter().map(|(k, _)| *k));
    out.extend(SPACES.iter().filter(|(k, _)| k.chars().all(|c| c.is_ascii_alphabetic())).map(|(k, _)| *k));
    out.extend([
        "left",
        "right",
        "middle",
        "text",
        "textrm",
        "textit",
        "textbf",
        "mbox",
        "operatorname",
        "not",
        "limits",
        "nolimits",
        "displaystyle",
        "textstyle",
        "nonumber",
        "notag",
        "overset",
        "underset",
        "stackrel",
        "pmod",
        "bmod",
        "big",
        "Big",
        "bigg",
        "Bigg",
        "begin",
        "end",
    ]);
    out
}

// ----------------------------------------------------------------- lexer --

#[derive(Clone, Debug, PartialEq)]
enum Tk {
    /// `\name` (letters only).
    Cmd,
    /// `\` followed by one non-letter.
    Sym(char),
    Open,
    Close,
    Hat,
    Under,
    Amp,
    Primes,
    Char(char),
    Space,
    /// A lone backslash at the end.
    Stray,
}

#[derive(Clone, Debug)]
struct Token {
    tk: Tk,
    /// Absolute Markdown byte range.
    range: Range<usize>,
}

fn lex(src: &str, base: usize) -> Vec<Token> {
    let mut out = Vec::new();
    let mut it = src.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        let start = base + i;
        let tk = match c {
            '\\' => match it.peek().copied() {
                Some((_, n)) if n.is_ascii_alphabetic() => {
                    let mut end = i + 1;
                    while let Some(&(j, n)) = it.peek() {
                        if n.is_ascii_alphabetic() {
                            end = j + 1;
                            it.next();
                        } else {
                            break;
                        }
                    }
                    out.push(Token { tk: Tk::Cmd, range: start..base + end });
                    continue;
                }
                Some((j, n)) => {
                    it.next();
                    out.push(Token { tk: Tk::Sym(n), range: start..base + j + n.len_utf8() });
                    continue;
                }
                None => Tk::Stray,
            },
            '{' => Tk::Open,
            '}' => Tk::Close,
            '^' => Tk::Hat,
            '_' => Tk::Under,
            '&' => Tk::Amp,
            '%' => {
                // Comment to the end of the line.
                let mut end = i + 1;
                while let Some(&(j, n)) = it.peek() {
                    end = j;
                    if n == '\n' {
                        break;
                    }
                    end = j + n.len_utf8();
                    it.next();
                }
                out.push(Token { tk: Tk::Space, range: start..base + end });
                continue;
            }
            '\'' => {
                let mut end = i + 1;
                while let Some(&(j, '\'')) = it.peek() {
                    end = j + 1;
                    it.next();
                }
                out.push(Token { tk: Tk::Primes, range: start..base + end });
                continue;
            }
            c if c.is_whitespace() => {
                let mut end = i + c.len_utf8();
                while let Some(&(j, n)) = it.peek() {
                    if !n.is_whitespace() {
                        break;
                    }
                    end = j + n.len_utf8();
                    it.next();
                }
                out.push(Token { tk: Tk::Space, range: start..base + end });
                continue;
            }
            c => Tk::Char(c),
        };
        out.push(Token { tk, range: start..start + c.len_utf8() });
    }
    out
}

// ------------------------------------------------------------------ tree --

#[derive(Clone, Debug)]
enum Node {
    /// One rendered unit. `text` is the Typst spelling; `linear` when it is
    /// the LaTeX spelling too.
    Atom {
        text: String,
        range: Range<usize>,
        linear: bool,
    },
    /// `{...}` (braces are not rendered).
    Group {
        items: Vec<Node>,
        range: Range<usize>,
    },
    Script {
        base: Option<Box<Node>>,
        limits: Option<(bool, Range<usize>)>,
        primes: Option<Range<usize>>,
        sub: Option<(Range<usize>, Box<Node>)>,
        sup: Option<(Range<usize>, Box<Node>)>,
    },
    /// Typst function call(s) wrapping the arguments.
    Call {
        funcs: &'static [&'static str],
        cmd: Range<usize>,
        args: Vec<Node>,
        range: Range<usize>,
    },
    /// `\text{...}` and friends: `pieces` are rendered characters.
    Text {
        style: &'static str,
        cmd: Range<usize>,
        pieces: Vec<(char, Range<usize>)>,
        range: Range<usize>,
    },
    OpName {
        name: String,
        range: Range<usize>,
    },
    LeftRight {
        left: Delim,
        body: Vec<Node>,
        right: Delim,
        range: Range<usize>,
    },
    Middle {
        delim: Delim,
        range: Range<usize>,
    },
    Space {
        typst: &'static str,
        range: Range<usize>,
    },
    Linebreak {
        range: Range<usize>,
    },
    Align {
        range: Range<usize>,
    },
    /// cases / matrices: rows of cells.
    Grid {
        func: &'static str,
        delim: Option<&'static str>,
        rows: Vec<Vec<Vec<Node>>>,
        range: Range<usize>,
    },
    /// `\displaystyle` etc. applied to the rest of the group.
    Style {
        func: &'static str,
        cmd: Range<usize>,
        body: Vec<Node>,
    },
    Pmod {
        cmd: Range<usize>,
        arg: Box<Node>,
        range: Range<usize>,
    },
    NoOp,
    Unsupported {
        range: Range<usize>,
    },
}

#[derive(Clone, Debug)]
struct Delim {
    /// Rendered delimiter ("" for `.`).
    text: &'static str,
    range: Range<usize>,
}

impl Node {
    fn range(&self) -> Option<Range<usize>> {
        match self {
            Node::Atom { range, .. }
            | Node::Group { range, .. }
            | Node::Call { range, .. }
            | Node::Text { range, .. }
            | Node::OpName { range, .. }
            | Node::LeftRight { range, .. }
            | Node::Middle { range, .. }
            | Node::Space { range, .. }
            | Node::Linebreak { range }
            | Node::Align { range }
            | Node::Grid { range, .. }
            | Node::Pmod { range, .. }
            | Node::Unsupported { range } => Some(range.clone()),
            Node::Script { base, primes, sub, sup, .. } => {
                let start = base
                    .as_ref()
                    .and_then(|b| b.range())
                    .map(|r| r.start)
                    .or(primes.as_ref().map(|r| r.start))
                    .or(sub.as_ref().map(|s| s.0.start))
                    .or(sup.as_ref().map(|s| s.0.start))?;
                let ends = [
                    primes.as_ref().map(|r| r.end),
                    sub.as_ref().and_then(|s| s.1.range()).map(|r| r.end),
                    sup.as_ref().and_then(|s| s.1.range()).map(|r| r.end),
                    base.as_ref().and_then(|b| b.range()).map(|r| r.end),
                ];
                Some(start..ends.into_iter().flatten().max().unwrap_or(start))
            }
            Node::Style { cmd, body, .. } => {
                let end = body.iter().filter_map(Node::range).map(|r| r.end).max().unwrap_or(cmd.end);
                Some(cmd.start..end)
            }
            Node::NoOp => None,
        }
    }
}

// ---------------------------------------------------------------- parser --

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stop {
    /// Top level of the equation.
    End,
    /// Inside `{...}`.
    Brace,
    /// Inside `\left ... \right`.
    Right,
    /// Inside an environment body.
    Env,
    /// Inside an optional argument `[...]`.
    Bracket,
}

struct Parser<'s> {
    src: &'s str,
    toks: Vec<Token>,
    pos: usize,
    problems: Vec<MathProblem>,
}

impl<'s> Parser<'s> {
    fn text(&self, range: &Range<usize>) -> &'s str {
        &self.src[range.clone()]
    }

    fn peek(&self) -> Option<&Token> {
        self.toks.get(self.pos)
    }

    fn skip_space(&mut self) {
        while self.peek().is_some_and(|t| t.tk == Tk::Space) {
            self.pos += 1;
        }
    }

    fn cmd_name(&self, t: &Token) -> &'s str {
        &self.src[t.range.start + 1..t.range.end]
    }

    fn unsupported(&mut self, mut range: Range<usize>, message: String) -> Node {
        // Trailing whitespace is not part of the construct.
        let trimmed = self.src[range.clone()].trim_end().len();
        range.end = range.start + trimmed;
        self.problems.push(MathProblem { range: range.clone(), message, marker: true });
        Node::Unsupported { range }
    }

    /// Index of the token closing the group opened at `open` (exclusive
    /// scan), or `None` when unbalanced.
    fn matching_close(&self, open: usize) -> Option<usize> {
        let mut depth = 0usize;
        for (i, t) in self.toks.iter().enumerate().skip(open) {
            match t.tk {
                Tk::Open => depth += 1,
                Tk::Close => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// Consume any brace groups / optional arguments following position
    /// (for an unsupported command) and return the covered end.
    fn swallow_args(&mut self, mut end: usize) -> usize {
        loop {
            let save = self.pos;
            self.skip_space();
            match self.peek().map(|t| t.tk.clone()) {
                Some(Tk::Open) => match self.matching_close(self.pos) {
                    Some(close) => {
                        end = self.toks[close].range.end;
                        self.pos = close + 1;
                    }
                    None => {
                        end = self.toks.last().map_or(end, |t| t.range.end);
                        self.pos = self.toks.len();
                        return end;
                    }
                },
                Some(Tk::Char('[')) if self.pos == save => {
                    // Optional argument directly after the command.
                    match self.toks[self.pos..].iter().position(|t| t.tk == Tk::Char(']')) {
                        Some(k) => {
                            end = self.toks[self.pos + k].range.end;
                            self.pos += k + 1;
                        }
                        None => {
                            self.pos = save;
                            return end;
                        }
                    }
                }
                _ => {
                    self.pos = save;
                    return end;
                }
            }
        }
    }

    fn at_stop(&self, stop: Stop) -> bool {
        let Some(t) = self.peek() else { return true };
        match (&t.tk, stop) {
            (Tk::Close, Stop::Brace) => true,
            (Tk::Char(']'), Stop::Bracket) => true,
            (Tk::Cmd, Stop::Right) => matches!(self.cmd_name(t), "right"),
            (Tk::Cmd, Stop::Env) => self.cmd_name(t) == "end",
            (Tk::Amp | Tk::Sym('\\'), Stop::Env) => false,
            _ => false,
        }
    }

    /// Parse a sequence until `stop` (not consumed).
    fn seq(&mut self, stop: Stop) -> Vec<Node> {
        let mut items: Vec<Node> = Vec::new();
        while self.pos < self.toks.len() {
            if self.at_stop(stop) {
                break;
            }
            let t = self.toks[self.pos].clone();
            match t.tk {
                Tk::Space => self.pos += 1,
                Tk::Close => {
                    // Unbalanced `}`.
                    self.pos += 1;
                    let node = self.unsupported(t.range.clone(), "unbalanced `}`".into());
                    items.push(node);
                }
                Tk::Hat | Tk::Under | Tk::Primes => {
                    let base = items.pop();
                    let node = self.scripts(base);
                    items.push(node);
                }
                Tk::Cmd if matches!(self.cmd_name(&t), "limits" | "nolimits") => {
                    let base = items.pop();
                    let node = self.scripts(base);
                    items.push(node);
                }
                Tk::Cmd if matches!(self.cmd_name(&t), "displaystyle" | "textstyle" | "scriptstyle") => {
                    self.pos += 1;
                    let func = match self.cmd_name(&t) {
                        "displaystyle" => "display",
                        "textstyle" => "inline",
                        _ => "script",
                    };
                    let body = self.seq(stop);
                    items.push(Node::Style { func, cmd: t.range.clone(), body });
                }
                _ => {
                    let node = self.atom();
                    items.push(node);
                }
            }
        }
        merge_numbers(items)
    }

    /// Scripts after `base` (current token is `^`, `_`, primes or `\limits`).
    fn scripts(&mut self, base: Option<Node>) -> Node {
        let mut limits = None;
        let mut primes = None;
        let mut sub = None;
        let mut sup = None;
        loop {
            let save = self.pos;
            self.skip_space();
            let Some(t) = self.peek().cloned() else { break };
            match t.tk {
                Tk::Cmd if limits.is_none() && sub.is_none() && sup.is_none() => match self.cmd_name(&t) {
                    "limits" => {
                        limits = Some((true, t.range.clone()));
                        self.pos += 1;
                    }
                    "nolimits" => {
                        limits = Some((false, t.range.clone()));
                        self.pos += 1;
                    }
                    _ => {
                        self.pos = save;
                        break;
                    }
                },
                Tk::Primes if primes.is_none() && sub.is_none() && sup.is_none() => {
                    primes = Some(t.range.clone());
                    self.pos += 1;
                }
                Tk::Hat | Tk::Under => {
                    self.pos += 1;
                    let slot = if t.tk == Tk::Hat { &sup } else { &sub };
                    if slot.is_some() {
                        let what = if t.tk == Tk::Hat { "superscript" } else { "subscript" };
                        let arg = self.arg();
                        let end = arg.range().map_or(t.range.end, |r| r.end);
                        let node = self.unsupported(t.range.start..end, format!("double {what}"));
                        // Keep what was parsed and report the extra script.
                        let script = Node::Script {
                            base: base.map(Box::new),
                            limits,
                            primes,
                            sub: sub.map(|(r, n)| (r, Box::new(n))),
                            sup: sup.map(|(r, n)| (r, Box::new(n))),
                        };
                        return Node::Group {
                            range: script.range().unwrap_or(t.range.clone()),
                            items: vec![script, node],
                        };
                    }
                    let arg = self.arg();
                    if t.tk == Tk::Hat {
                        sup = Some((t.range.clone(), arg));
                    } else {
                        sub = Some((t.range.clone(), arg));
                    }
                }
                _ => {
                    self.pos = save;
                    break;
                }
            }
        }
        Node::Script {
            base: base.map(Box::new),
            limits,
            primes,
            sub: sub.map(|(r, n)| (r, Box::new(n))),
            sup: sup.map(|(r, n)| (r, Box::new(n))),
        }
    }

    /// A command or script argument: a group or a single token.
    fn arg(&mut self) -> Node {
        self.skip_space();
        let Some(t) = self.peek().cloned() else {
            let at = self.toks.last().map_or(0, |t| t.range.end);
            return self.unsupported(at..at, "missing argument".into());
        };
        match t.tk {
            Tk::Open => self.group(),
            Tk::Close | Tk::Hat | Tk::Under | Tk::Amp | Tk::Primes => {
                self.unsupported(t.range.start..t.range.start, "missing argument".into())
            }
            Tk::Char(c) if c.is_ascii_digit() => {
                // A single digit (LaTeX takes one token).
                self.pos += 1;
                Node::Atom { text: c.to_string(), range: t.range.clone(), linear: true }
            }
            _ => self.atom(),
        }
    }

    /// `{...}` at the current position.
    fn group(&mut self) -> Node {
        let open = self.toks[self.pos].range.clone();
        self.pos += 1;
        let items = self.seq(Stop::Brace);
        match self.peek() {
            Some(t) if t.tk == Tk::Close => {
                let end = t.range.end;
                self.pos += 1;
                Node::Group { items, range: open.start..end }
            }
            _ => {
                let end = self.toks.last().map_or(open.end, |t| t.range.end);
                let node = self.unsupported(open.start..end, "unbalanced `{`".into());
                Node::Group { items: vec![node], range: open.start..end }
            }
        }
    }

    /// Raw text of a `{...}` group following the current position.
    fn raw_group(&mut self) -> Option<(Range<usize>, Range<usize>)> {
        self.skip_space();
        let t = self.peek()?;
        if t.tk != Tk::Open {
            return None;
        }
        let close = self.matching_close(self.pos)?;
        let open = self.toks[self.pos].range.clone();
        let end = self.toks[close].range.clone();
        self.pos = close + 1;
        Some((open.start..end.end, open.end..end.start))
    }

    fn delimiter(&mut self) -> Option<Delim> {
        self.skip_space();
        let t = self.peek()?.clone();
        let text: &'static str = match &t.tk {
            Tk::Char(c) => match c {
                '(' => "(",
                ')' => ")",
                '[' => "[",
                ']' => "]",
                '|' => "|",
                '.' => "",
                '/' => "/",
                _ => return None,
            },
            Tk::Sym(c) => match c {
                '{' => "{",
                '}' => "}",
                '|' => "‖",
                _ => return None,
            },
            Tk::Cmd => match self.cmd_name(&t) {
                "langle" => "⟨",
                "rangle" => "⟩",
                "lfloor" => "⌊",
                "rfloor" => "⌋",
                "lceil" => "⌈",
                "rceil" => "⌉",
                "vert" | "lvert" | "rvert" => "|",
                "Vert" | "lVert" | "rVert" => "‖",
                "uparrow" => "↑",
                "downarrow" => "↓",
                _ => return None,
            },
            _ => return None,
        };
        self.pos += 1;
        Some(Delim { text, range: t.range.clone() })
    }

    fn atom(&mut self) -> Node {
        let t = self.toks[self.pos].clone();
        self.pos += 1;
        match t.tk {
            Tk::Char(c) => self.char_atom(c, t.range),
            Tk::Open => {
                self.pos -= 1;
                self.group()
            }
            Tk::Amp => Node::Align { range: t.range },
            Tk::Sym(c) => self.control_symbol(c, t.range),
            Tk::Cmd => self.command(t.range),
            Tk::Stray => self.unsupported(t.range, "lone backslash".into()),
            Tk::Close | Tk::Hat | Tk::Under | Tk::Primes | Tk::Space => Node::NoOp,
        }
    }

    fn char_atom(&mut self, c: char, range: Range<usize>) -> Node {
        let escaped = |c: char| Node::Atom { text: format!("\\{c}"), range: range.clone(), linear: false };
        match c {
            '#' | '$' => self.unsupported(range, format!("`{c}` must be written `\\{c}` in mathematics")),
            '/' | '"' | '@' | '√' | '∛' | '∜' => escaped(c),
            '~' => Node::Space { typst: "thick", range },
            c if DELIMITERS.contains(&c) => escaped(c),
            c => Node::Atom { text: c.to_string(), range, linear: true },
        }
    }

    fn control_symbol(&mut self, c: char, range: Range<usize>) -> Node {
        let key = c.to_string();
        if let Some(space) = lookup(SPACES, &key) {
            return Node::Space { typst: space, range };
        }
        match c {
            '\\' => Node::Linebreak { range },
            '{' | '}' | '|' => {
                let text = if c == '|' { "\\‖".to_owned() } else { format!("\\{c}") };
                Node::Atom { text, range, linear: false }
            }
            '#' | '$' | '%' | '&' | '_' => Node::Atom { text: format!("\\{c}"), range, linear: false },
            c => self.unsupported(range, format!("unsupported control symbol `\\{c}`")),
        }
    }

    fn command(&mut self, cmd: Range<usize>) -> Node {
        let name = &self.src[cmd.start + 1..cmd.end];
        if let Some(text) = lookup(SYMBOLS, name) {
            let text = if text.chars().all(|c| DELIMITERS.contains(&c)) || text == "\\" {
                format!("\\{text}")
            } else {
                text.to_owned()
            };
            return Node::Atom { text, range: cmd, linear: false };
        }
        if OPERATORS.contains(&name) {
            return Node::Atom { text: name.to_owned(), range: cmd, linear: false };
        }
        if name == "bmod" {
            return Node::Atom { text: "mod".into(), range: cmd, linear: false };
        }
        if let Some(space) = lookup(SPACES, name) {
            return Node::Space { typst: space, range: cmd };
        }
        if name == "sqrt" {
            // Optional index.
            let save = self.pos;
            self.skip_space();
            if self.peek().is_some_and(|t| t.tk == Tk::Char('[')) {
                let open = self.toks[self.pos].range.clone();
                self.pos += 1;
                let index = self.seq(Stop::Bracket);
                if self.peek().is_some_and(|t| t.tk == Tk::Char(']')) {
                    let close = self.toks[self.pos].range.clone();
                    self.pos += 1;
                    let body = self.arg();
                    let end = body.range().map_or(close.end, |r| r.end);
                    let index = Node::Group { items: index, range: open.start..close.end };
                    return Node::Call {
                        funcs: &["root"],
                        cmd: cmd.clone(),
                        args: vec![index, body],
                        range: cmd.start..end,
                    };
                }
                self.pos = save;
                return self.unsupported(cmd.start..open.end, "unterminated optional argument of `\\sqrt`".into());
            }
            self.pos = save;
        }
        if let Some(funcs) = lookup(UNARY, name) {
            let arg = self.arg();
            let end = arg.range().map_or(cmd.end, |r| r.end);
            return Node::Call { funcs, cmd: cmd.clone(), args: vec![arg], range: cmd.start..end };
        }
        if let Some(func) = lookup(BINARY, name) {
            let a = self.arg();
            let b = self.arg();
            let end = b.range().or(a.range()).map_or(cmd.end, |r| r.end);
            let funcs: &'static [&'static str] = if func == "frac" { &["frac"] } else { &["binom"] };
            return Node::Call { funcs, cmd: cmd.clone(), args: vec![a, b], range: cmd.start..end };
        }
        match name {
            "text" | "textrm" | "mbox" | "textit" | "textbf" | "textnormal" => self.text_command(cmd, name),
            "operatorname" => {
                let star = self.peek().is_some_and(|t| t.tk == Tk::Char('*'));
                if star {
                    self.pos += 1;
                }
                match self.raw_group() {
                    Some((range, inner)) => {
                        let name = self.text(&inner).trim().to_owned();
                        if name.chars().all(|c| c.is_alphanumeric() || c == ' ' || c == '-') && !name.is_empty() {
                            Node::OpName { name, range: cmd.start..range.end }
                        } else {
                            self.unsupported(cmd.start..range.end, "unsupported `\\operatorname` content".into())
                        }
                    }
                    None => self.unsupported(cmd, "`\\operatorname` needs a `{...}` argument".into()),
                }
            }
            "left" => self.left_right(cmd),
            "right" => self.unsupported(cmd, "`\\right` without `\\left`".into()),
            "middle" => match self.delimiter() {
                Some(delim) => {
                    let range = cmd.start..delim.range.end;
                    Node::Middle { delim, range }
                }
                None => self.unsupported(cmd, "`\\middle` needs a delimiter".into()),
            },
            "big" | "Big" | "bigg" | "Bigg" | "bigl" | "bigr" | "Bigl" | "Bigr" | "biggl" | "biggr" | "Biggl"
            | "Biggr" | "bigm" | "Bigm" => match self.delimiter() {
                Some(delim) => {
                    self.problems.push(MathProblem {
                        range: cmd.clone(),
                        message: format!("`\\{name}` sizing is ignored; the delimiter is shown at its natural size"),
                        marker: false,
                    });
                    let text = if delim.text.is_empty() {
                        String::new()
                    } else if delim.text.chars().all(|c| DELIMITERS.contains(&c)) {
                        format!("\\{}", delim.text)
                    } else {
                        delim.text.to_owned()
                    };
                    Node::Atom { text, range: cmd.start..delim.range.end, linear: false }
                }
                None => self.unsupported(cmd, format!("`\\{name}` needs a delimiter")),
            },
            "not" => {
                self.skip_space();
                let Some(next) = self.peek().cloned() else {
                    return self.unsupported(cmd, "`\\not` needs a relation".into());
                };
                let key = match &next.tk {
                    Tk::Char(c) => c.to_string(),
                    Tk::Cmd => self.cmd_name(&next).to_owned(),
                    _ => String::new(),
                };
                match lookup(NEGATIONS, &key) {
                    Some(text) => {
                        self.pos += 1;
                        Node::Atom { text: text.to_owned(), range: cmd.start..next.range.end, linear: false }
                    }
                    None => {
                        self.pos += 1;
                        self.unsupported(cmd.start..next.range.end, "unsupported `\\not` combination".into())
                    }
                }
            }
            "nonumber" | "notag" => Node::NoOp,
            "overset" | "underset" | "stackrel" => {
                let over = self.arg();
                let body = self.arg();
                let end = body.range().map_or(cmd.end, |r| r.end);
                let base = Node::Call { funcs: &["limits"], cmd: cmd.clone(), args: vec![body], range: cmd.start..end };
                let annotation = Some((cmd.clone(), Box::new(over)));
                if name == "underset" {
                    Node::Script { base: Some(Box::new(base)), limits: None, primes: None, sub: annotation, sup: None }
                } else {
                    Node::Script { base: Some(Box::new(base)), limits: None, primes: None, sub: None, sup: annotation }
                }
            }
            "pmod" => {
                let arg = self.arg();
                let end = arg.range().map_or(cmd.end, |r| r.end);
                Node::Pmod { cmd: cmd.clone(), arg: Box::new(arg), range: cmd.start..end }
            }
            "begin" => self.environment(cmd),
            "end" => self.unsupported(cmd, "`\\end` without `\\begin`".into()),
            _ => {
                let end = self.swallow_args(cmd.end);
                self.unsupported(cmd.start..end, format!("unsupported command `\\{name}`"))
            }
        }
    }

    fn text_command(&mut self, cmd: Range<usize>, name: &str) -> Node {
        let style = match name {
            "textit" => "emph",
            "textbf" => "strong",
            _ => "",
        };
        let Some((range, inner)) = self.raw_group() else {
            return self.unsupported(cmd, format!("`\\{name}` needs a `{{...}}` argument"));
        };
        let content = self.text(&inner);
        let mut pieces = Vec::new();
        let mut it = content.char_indices().peekable();
        while let Some((i, c)) = it.next() {
            let at = inner.start + i;
            match c {
                '\\' => match it.next() {
                    Some((j, e)) if "{}$&%#_ ".contains(e) => pieces.push((e, at..inner.start + j + e.len_utf8())),
                    Some(_) => {
                        return self
                            .unsupported(cmd.start..range.end, format!("unsupported command inside `\\{name}`"));
                    }
                    None => return self.unsupported(cmd.start..range.end, "lone backslash".into()),
                },
                '$' | '{' | '}' | '^' | '_' | '&' | '#' | '%' => {
                    return self.unsupported(cmd.start..range.end, format!("unsupported `{c}` inside `\\{name}`"));
                }
                '~' => pieces.push(('\u{a0}', at..at + 1)),
                '\n' | '\r' => pieces.push((' ', at..at + 1)),
                c => pieces.push((c, at..at + c.len_utf8())),
            }
        }
        Node::Text { style, cmd: cmd.clone(), pieces, range: cmd.start..range.end }
    }

    fn left_right(&mut self, cmd: Range<usize>) -> Node {
        let Some(left) = self.delimiter() else {
            return self.unsupported(cmd, "`\\left` needs a delimiter".into());
        };
        let left = Delim { range: cmd.start..left.range.end, ..left };
        let body = self.seq(Stop::Right);
        match self.peek().cloned() {
            Some(t) if t.tk == Tk::Cmd && self.cmd_name(&t) == "right" => {
                self.pos += 1;
                match self.delimiter() {
                    Some(right) => {
                        let right = Delim { range: t.range.start..right.range.end, ..right };
                        let range = cmd.start..right.range.end;
                        Node::LeftRight { left, body, right, range }
                    }
                    None => self.unsupported(cmd.start..t.range.end, "`\\right` needs a delimiter".into()),
                }
            }
            _ => {
                let end = self.toks.last().map_or(cmd.end, |t| t.range.end);
                self.unsupported(cmd.start..end, "`\\left` without `\\right`".into())
            }
        }
    }

    fn environment(&mut self, cmd: Range<usize>) -> Node {
        let Some((name_range, inner)) = self.raw_group() else {
            return self.unsupported(cmd, "`\\begin` needs an environment name".into());
        };
        let env = self.text(&inner).trim().to_owned();
        let start = cmd.start;
        let line_env = LINE_ENVS.contains(&env.as_str());
        let matrix = lookup(MATRIX_ENVS, &env);
        let cases = env == "cases";
        if !line_env && matrix.is_none() && !cases {
            // Skip to the matching \end{env} and mark the whole environment.
            let end = self.find_end(&env).unwrap_or_else(|| self.toks.last().map_or(name_range.end, |t| t.range.end));
            return self.unsupported(start..end, format!("unsupported environment `{env}`"));
        }
        // Rows of cells.
        let mut rows: Vec<Vec<Vec<Node>>> = vec![vec![Vec::new()]];
        let mut flat: Vec<Node> = Vec::new();
        // `seq` stops at `\end` or the end of input; `&` and `\\` come
        // through as nodes.
        for node in self.seq(Stop::Env) {
            match node {
                Node::Align { .. } if !line_env => rows.last_mut().expect("row").push(Vec::new()),
                Node::Linebreak { .. } if !line_env => rows.push(vec![Vec::new()]),
                other if line_env => flat.push(other),
                other => rows.last_mut().expect("row").last_mut().expect("cell").push(other),
            }
        }
        // Expect \end{env}.
        let end = match self.peek().cloned() {
            Some(t) if t.tk == Tk::Cmd && self.cmd_name(&t) == "end" => {
                self.pos += 1;
                match self.raw_group() {
                    Some((r, inner)) if self.text(&inner).trim() == env => r.end,
                    Some((r, _)) => {
                        return self
                            .unsupported(start..r.end, format!("`\\begin{{{env}}}` ended by a different `\\end`"));
                    }
                    None => return self.unsupported(start..t.range.end, "`\\end` needs an environment name".into()),
                }
            }
            _ => {
                let end = self.toks.last().map_or(name_range.end, |t| t.range.end);
                return self.unsupported(start..end, format!("`\\begin{{{env}}}` without `\\end`"));
            }
        };
        let range = start..end;
        if line_env {
            return Node::Group { items: flat, range };
        }
        // Drop a trailing empty row (`\\` before `\end`).
        if rows.len() > 1 && rows.last().is_some_and(|r| r.len() == 1 && r[0].is_empty()) {
            rows.pop();
        }
        if cases {
            Node::Grid { func: "cases", delim: None, rows, range }
        } else {
            Node::Grid { func: "mat", delim: matrix, rows, range }
        }
    }

    /// Position after the `\end{env}` matching the current nesting.
    fn find_end(&mut self, env: &str) -> Option<usize> {
        let mut depth = 1usize;
        while self.pos < self.toks.len() {
            let t = self.toks[self.pos].clone();
            self.pos += 1;
            if t.tk != Tk::Cmd {
                continue;
            }
            let name = self.cmd_name(&t);
            if name == "begin" || name == "end" {
                let is_begin = name == "begin";
                if let Some((r, inner)) = self.raw_group()
                    && self.text(&inner).trim() == env
                {
                    if is_begin {
                        depth += 1;
                    } else {
                        depth -= 1;
                        if depth == 0 {
                            return Some(r.end);
                        }
                    }
                }
            }
        }
        None
    }
}

/// Merge adjacent digit atoms (and one decimal point between digits) into a
/// single number, as Typst lexes numbers.
fn merge_numbers(items: Vec<Node>) -> Vec<Node> {
    let digits = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
    let mut out: Vec<Node> = Vec::with_capacity(items.len());
    for node in items {
        if let Node::Atom { text, range, linear: true } = &node
            && let Some(Node::Atom { text: prev, range: prev_range, linear: true }) = out.last_mut()
            && prev_range.end == range.start
            && prev.starts_with(|c: char| c.is_ascii_digit())
            && prev.chars().all(|c| c.is_ascii_digit() || c == '.')
            && (digits(text) || (text == "." && !prev.contains('.')))
        {
            prev.push_str(text);
            prev_range.end = range.end;
            continue;
        }
        out.push(node);
    }
    split_trailing_points(out)
}

fn split_trailing_points(items: Vec<Node>) -> Vec<Node> {
    let mut out = Vec::with_capacity(items.len());
    for node in items {
        match node {
            Node::Atom { text, range, linear: true }
                if text.len() > 1 && text.ends_with('.') && text.chars().all(|c| c.is_ascii_digit() || c == '.') =>
            {
                let n = text.len() - 1;
                out.push(Node::Atom { text: text[..n].to_owned(), range: range.start..range.end - 1, linear: true });
                out.push(Node::Atom { text: ".".into(), range: range.end - 1..range.end, linear: true });
            }
            other => out.push(other),
        }
    }
    out
}

// --------------------------------------------------------------- emitter --

/// Whether `nodes` produce no Typst content (empty groups, no-ops).
fn renders_nothing(nodes: &[Node]) -> bool {
    nodes.iter().all(|n| match n {
        Node::NoOp => true,
        Node::Group { items, .. } => renders_nothing(items),
        _ => false,
    })
}

/// Location of an emitted equation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EmittedEquation {
    pub problems: Vec<MathProblem>,
}

struct MathEmitter<'e, 's> {
    e: &'e mut Emitter<'s>,
    /// The whole equation (owner of separators).
    owner: Range<usize>,
    need_space: bool,
    /// Inside a Typst function argument list (`,` `;` `:` `.` are syntax).
    in_args: bool,
    problems: &'e [MathProblem],
}

/// Translate the mathematics `content` (at Markdown byte `base`) of an
/// equation whose full range (with delimiters) is `owner`.
pub(crate) fn emit_equation(
    e: &mut Emitter<'_>,
    content: Range<usize>,
    owner: Range<usize>,
    block: bool,
) -> EmittedEquation {
    let src = e.md;
    let toks = lex(&src[content.clone()], content.start);
    let mut parser = Parser { src, toks, pos: 0, problems: Vec::new() };
    let mut nodes = Vec::new();
    while parser.pos < parser.toks.len() {
        nodes.extend(parser.seq(Stop::End));
        // Stray closers at the top level.
        if parser.pos < parser.toks.len() {
            let t = parser.toks[parser.pos].clone();
            parser.pos += 1;
            let node = match t.tk {
                Tk::Cmd if parser.cmd_name(&t) == "right" => {
                    parser.unsupported(t.range.clone(), "`\\right` without `\\left`".into())
                }
                Tk::Cmd if parser.cmd_name(&t) == "end" => {
                    let end = parser.swallow_args(t.range.end);
                    parser.unsupported(t.range.start..end, "`\\end` without `\\begin`".into())
                }
                _ => parser.unsupported(t.range.clone(), "unexpected token".into()),
            };
            nodes.push(node);
        }
    }
    let problems = parser.problems;
    let open = owner.start..content.start;
    let close = content.end..owner.end;
    e.mapped(if block { "$ " } else { "$" }, &open, SegmentKind::Syntax);
    {
        let mut m = MathEmitter { e, owner: owner.clone(), need_space: false, in_args: false, problems: &problems };
        m.seq(&nodes);
    }
    e.mapped(if block { " $" } else { "$" }, &close, SegmentKind::Syntax);
    EmittedEquation { problems }
}

/// Parse mathematics only (no emission) and return its problems; used by
/// input rules to verify edits.
pub(crate) fn check(src: &str, content: Range<usize>) -> Vec<MathProblem> {
    let toks = lex(&src[content.clone()], content.start);
    let mut parser = Parser { src, toks, pos: 0, problems: Vec::new() };
    while parser.pos < parser.toks.len() {
        let _ = parser.seq(Stop::End);
        if parser.pos < parser.toks.len() {
            let t = parser.toks[parser.pos].clone();
            parser.pos += 1;
            parser.problems.push(MathProblem { range: t.range, message: "unexpected token".into(), marker: true });
        }
    }
    parser.problems
}

/// LaTeX tokens of mathematics content as (byte range, kind) for input
/// rules: `true` marks a rendered unit (character, symbol command).
pub(crate) fn units(src: &str, content: Range<usize>) -> Vec<MathUnit> {
    lex(&src[content.clone()], content.start)
        .into_iter()
        .map(|t| {
            let kind = match t.tk {
                Tk::Char(_) => UnitKind::Char,
                Tk::Cmd => {
                    let name = &src[t.range.start + 1..t.range.end];
                    if lookup(SYMBOLS, name).is_some() || OPERATORS.contains(&name) {
                        UnitKind::Symbol
                    } else if lookup(SPACES, name).is_some() || matches!(name, "nonumber" | "notag") {
                        UnitKind::Space
                    } else {
                        UnitKind::Structure
                    }
                }
                Tk::Sym(c) => {
                    if lookup(SPACES, &c.to_string()).is_some() {
                        UnitKind::Space
                    } else if c == '\\' {
                        UnitKind::Structure
                    } else {
                        UnitKind::Symbol
                    }
                }
                Tk::Open | Tk::Close => UnitKind::Brace,
                Tk::Hat | Tk::Under => UnitKind::Script,
                Tk::Primes => UnitKind::Symbol,
                Tk::Amp | Tk::Stray => UnitKind::Structure,
                Tk::Space => UnitKind::Space,
            };
            MathUnit { range: t.range, kind }
        })
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UnitKind {
    /// A character (letter, digit, operator).
    Char,
    /// A command or control symbol rendering one symbol.
    Symbol,
    /// Whitespace, comments and spacing commands.
    Space,
    Brace,
    /// `^` / `_`.
    Script,
    /// Structural commands (`\frac`, `\left`, `\\`, `&`, ...).
    Structure,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MathUnit {
    pub range: Range<usize>,
    pub kind: UnitKind,
}

impl MathEmitter<'_, '_> {
    fn sep(&mut self) {
        if self.need_space {
            let owner = self.owner.clone();
            self.e.syntax(" ", &owner);
        }
        self.need_space = false;
    }

    fn syntax(&mut self, text: &str, owner: &Range<usize>) {
        self.e.syntax(text, owner);
    }

    fn seq(&mut self, nodes: &[Node]) {
        for node in nodes {
            self.node(node);
        }
    }

    /// Emit nodes as one function argument.
    fn arg(&mut self, nodes: &[Node]) {
        let (saved_args, saved_space) = (self.in_args, self.need_space);
        self.in_args = true;
        self.need_space = false;
        if renders_nothing(nodes) {
            // An empty argument still needs content.
            let owner = self.owner.clone();
            self.syntax("\"\"", &owner);
        }
        self.seq(nodes);
        self.in_args = saved_args;
        self.need_space = saved_space;
    }

    fn atom(&mut self, text: &str, range: &Range<usize>, linear: bool) {
        if text.is_empty() {
            self.syntax("", range);
            return;
        }
        if self.in_args && matches!(text, "," | ";" | ":" | ".") {
            self.e.mapped(&format!("\\{text}"), range, SegmentKind::MathAtom);
        } else if linear {
            self.e.mapped(text, range, SegmentKind::MathText);
        } else {
            self.e.mapped(text, range, SegmentKind::MathAtom);
        }
    }

    /// Emit a node as a script/argument operand: simple atoms directly,
    /// anything else in (removable) parentheses.
    fn operand(&mut self, node: &Node, owner: &Range<usize>) {
        match node {
            Node::Atom { text, range, linear } if !text.is_empty() => self.atom(text, range, *linear),
            Node::Group { items, range } => {
                let open = range.start..(range.start + 1).min(range.end);
                let close = range.end.saturating_sub(1).max(range.start)..range.end;
                self.syntax("(", &open);
                let saved = (self.need_space, self.in_args);
                self.need_space = false;
                self.in_args = false;
                self.seq(items);
                (self.need_space, self.in_args) = saved;
                self.syntax(")", &close);
            }
            other => {
                self.syntax("(", owner);
                let saved = (self.need_space, self.in_args);
                self.need_space = false;
                self.in_args = false;
                self.node(other);
                (self.need_space, self.in_args) = saved;
                self.syntax(")", owner);
            }
        }
    }

    fn node(&mut self, node: &Node) {
        match node {
            Node::NoOp => {}
            Node::Atom { text, range, linear } => {
                // A factorial stays attached to its operand.
                if text == "!" {
                    self.need_space = false;
                }
                self.sep();
                self.atom(text, range, *linear);
                self.need_space = true;
            }
            Node::Group { items, .. } => self.seq(items),
            Node::Script { base, limits, primes, sub, sup } => self.script(base.as_deref(), limits, primes, sub, sup),
            Node::Call { funcs, cmd, args, .. } => {
                self.sep();
                for f in funcs.iter() {
                    self.syntax(f, cmd);
                    self.syntax("(", cmd);
                }
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        self.syntax(", ", cmd);
                    }
                    match a {
                        Node::Group { items, .. } => self.arg(items),
                        other => self.arg(std::slice::from_ref(other)),
                    }
                }
                for _ in funcs.iter() {
                    self.syntax(")", cmd);
                }
                self.need_space = true;
            }
            Node::Text { style, cmd, pieces, range } => {
                self.sep();
                let open = match *style {
                    "emph" => "#emph[",
                    "strong" => "#strong[",
                    _ => "#[",
                };
                self.syntax(open, cmd);
                self.e.line_start();
                for (i, (c, r)) in pieces.iter().enumerate() {
                    let next = pieces.get(i + 1).map(|p| p.0);
                    let mut buf = [0u8; 4];
                    let linear = self.e.md[r.clone()] == *c.encode_utf8(&mut buf);
                    self.e.markup_char_as(*c, next, r, linear, true);
                }
                let close = range.end.saturating_sub(1)..range.end;
                self.syntax("]", &close);
                self.need_space = true;
            }
            Node::OpName { name, range } => {
                self.sep();
                let text = format!("op({})", typst_string(name));
                self.e.mapped(&text, range, SegmentKind::MathAtom);
                self.need_space = true;
            }
            Node::LeftRight { left, body, right, .. } => {
                self.sep();
                let cmd = left.range.clone();
                self.syntax("lr(", &cmd);
                // Matching bracket pairs stay Typst delimiters (scaled by `lr`);
                // anything else is escaped so it cannot pair with the call's
                // own parentheses, and the body's commas are then escaped.
                let pair = matches!(
                    (left.text, right.text),
                    ("(", ")") | ("[", "]") | ("{", "}") | ("⟨", "⟩") | ("⌊", "⌋") | ("⌈", "⌉")
                );
                let delim = |d: &Delim| -> String {
                    if d.text.is_empty() || pair { d.text.to_owned() } else { format!("\\{}", d.text) }
                };
                let l = delim(left);
                if l.is_empty() {
                    self.syntax("", &left.range);
                } else {
                    self.e.mapped(&l, &left.range, SegmentKind::MathAtom);
                }
                let saved = (self.need_space, self.in_args);
                self.need_space = true;
                self.in_args = !pair;
                self.seq(body);
                self.sep();
                self.in_args = saved.1;
                let r = delim(right);
                if r.is_empty() {
                    self.syntax("", &right.range);
                } else {
                    self.e.mapped(&r, &right.range, SegmentKind::MathAtom);
                }
                self.syntax(")", &right.range);
                self.need_space = true;
            }
            Node::Middle { delim, range } => {
                self.sep();
                let text = if delim.text.is_empty() { "\"\"".to_owned() } else { format!("\\{}", delim.text) };
                self.syntax("mid(", range);
                self.e.mapped(&text, &delim.range, SegmentKind::MathAtom);
                self.syntax(")", range);
                self.need_space = true;
            }
            Node::Space { typst, range } => {
                self.sep();
                self.syntax(typst, range);
                self.need_space = true;
            }
            Node::Linebreak { range } => {
                self.sep();
                self.syntax("\\", range);
                self.need_space = true;
            }
            Node::Align { range } => {
                self.sep();
                self.syntax("&", range);
                self.need_space = true;
            }
            Node::Grid { func, delim, rows, range } => {
                self.sep();
                self.syntax(func, range);
                self.syntax("(", range);
                if let Some(d) = delim {
                    self.syntax("delim: ", range);
                    self.syntax(d, range);
                    self.syntax(", ", range);
                }
                for (ri, row) in rows.iter().enumerate() {
                    if ri > 0 {
                        self.syntax(if *func == "mat" { "; " } else { ", " }, range);
                    }
                    if *func == "mat" {
                        for (ci, cell) in row.iter().enumerate() {
                            if ci > 0 {
                                self.syntax(", ", range);
                            }
                            self.arg(cell);
                        }
                    } else {
                        // cases: one argument per row, cells joined by `&`.
                        let saved = (self.in_args, self.need_space);
                        self.in_args = true;
                        self.need_space = false;
                        if row.iter().all(|c| c.is_empty()) {
                            self.syntax("\"\"", range);
                        }
                        for (ci, cell) in row.iter().enumerate() {
                            if ci > 0 {
                                self.sep();
                                self.syntax("&", range);
                                self.need_space = true;
                            }
                            self.seq(cell);
                        }
                        (self.in_args, self.need_space) = saved;
                    }
                }
                self.syntax(")", range);
                self.need_space = true;
            }
            Node::Style { func, cmd, body } => {
                self.sep();
                self.syntax(func, cmd);
                self.syntax("(", cmd);
                self.arg(body);
                self.syntax(")", cmd);
                self.need_space = true;
            }
            Node::Pmod { cmd, arg, .. } => {
                self.sep();
                self.syntax("quad \\(mod ", cmd);
                self.need_space = false;
                self.node(arg);
                self.need_space = false;
                self.syntax("\\)", cmd);
                self.need_space = true;
            }
            Node::Unsupported { range } => {
                self.sep();
                let source = &self.e.md[range.clone()];
                let label = if source.is_empty() {
                    self.problems.iter().find(|p| p.range == *range).map_or("?".to_owned(), |p| p.message.clone())
                } else {
                    source.to_owned()
                };
                let text = format!("#exmd-unsupported({})", typst_string(&label));
                self.e.unsupported(&text, range);
                self.need_space = true;
            }
        }
    }

    fn script(
        &mut self,
        base: Option<&Node>,
        limits: &Option<(bool, Range<usize>)>,
        primes: &Option<Range<usize>>,
        sub: &Option<(Range<usize>, Box<Node>)>,
        sup: &Option<(Range<usize>, Box<Node>)>,
    ) {
        // `\underbrace{x}_{a}` / `\overbrace{x}^{a}`: annotation argument.
        if let Some(Node::Call { funcs, cmd, args, .. }) = base
            && let [f @ ("underbrace" | "overbrace")] = funcs
            && primes.is_none()
            && limits.is_none()
        {
            let annotation = if *f == "underbrace" { sub } else { sup };
            let other = if *f == "underbrace" { sup } else { sub };
            if let (Some((_, note)), None) = (annotation, other) {
                self.sep();
                self.syntax(f, cmd);
                self.syntax("(", cmd);
                match &args[0] {
                    Node::Group { items, .. } => self.arg(items),
                    other => self.arg(std::slice::from_ref(other)),
                }
                self.syntax(", ", cmd);
                match note.as_ref() {
                    Node::Group { items, .. } => self.arg(items),
                    other => self.arg(std::slice::from_ref(other)),
                }
                self.syntax(")", cmd);
                self.need_space = true;
                return;
            }
        }
        self.sep();
        let owner = base.and_then(Node::range).unwrap_or_else(|| self.owner.clone());
        let wrap = limits.as_ref().map(|(on, range)| (if *on { "limits(" } else { "scripts(" }, range.clone()));
        if let Some((open, range)) = &wrap {
            self.syntax(open, range);
        }
        match base {
            None => self.syntax("\"\"", &owner),
            Some(Node::Atom { text, range, linear }) if !text.is_empty() => self.atom(text, range, *linear),
            Some(Node::Call { .. } | Node::OpName { .. } | Node::LeftRight { .. } | Node::Text { .. }) => {
                let saved = self.need_space;
                self.need_space = false;
                self.node(base.expect("base"));
                self.need_space = saved;
            }
            Some(other) => {
                // A braced or complex base: `scripts(..)` groups it without
                // visible parentheses and keeps the default script placement.
                if wrap.is_none() {
                    self.syntax("scripts(", &owner);
                    self.arg(std::slice::from_ref(other));
                    self.syntax(")", &owner);
                } else {
                    self.arg(std::slice::from_ref(other));
                }
            }
        }
        if let Some((_, range)) = &wrap {
            self.syntax(")", range);
        }
        if let Some(p) = primes {
            let text = &self.e.md[p.clone()];
            let text = text.to_owned();
            self.e.mapped(&text, p, SegmentKind::MathText);
        }
        if let Some((op, arg)) = sub {
            self.syntax("_", op);
            self.operand(arg, op);
        }
        if let Some((op, arg)) = sup {
            self.syntax("^", op);
            self.operand(arg, op);
        }
        self.need_space = true;
    }
}
