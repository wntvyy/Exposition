# Markdown print profile (`exposition-markdown-print/1`)

Exposition keeps Markdown as the native source. To produce pages, a Markdown
document is translated to Typst with one fixed print template and laid out by
the Typst compiler. A source map links every rendered character back to the
Markdown it came from, so prose and supported mathematics can be edited
directly on the page; each edit is written back as a minimal Markdown patch.
The profile identifier changes whenever the template or the translation rules
change the layout of existing documents.

## Syntax

- CommonMark, plus GitHub-flavoured tables, strikethrough (`~~text~~`),
  footnotes (`[^id]` with `[^id]: text`) and task lists (`- [ ]`, `- [x]`).
- Optional YAML front matter between `---` lines at the start.
- Typographic punctuation: `--` prints an en dash, `---` an em dash, `...` an
  ellipsis, and straight quotes print as curly quotes. Every other character
  prints literally.
- Raw HTML is not interpreted. It prints verbatim in a subdued style and is
  reported as a warning. HTML comments (`<!-- ... -->`, including the
  markers research attachments use) are not printed.

## Front matter

| Key | Value | Effect |
|---|---|---|
| `title` | text | Title block and PDF title |
| `author` | text, or a list | Title block and PDF authors |
| `date` | text | Title block |
| `papersize` | `a3`, `a4`, `a5`, `b5`, `letter`, `legal` | Page size (default `a4`) |
| `fontsize` | `6pt` to `24pt` | Body text size (default `11pt`) |
| `margin` | a length in `cm`, `mm`, `in` or `pt` | Page margins (default `2.5cm`) |
| `lang` | ISO 639 code | Hyphenation and quotes (default `en`) |

Values may be plain, single-quoted or double-quoted scalars; `author` may also
be a `[a, b]` list or a `- name` list. Other keys are reported and ignored.
Title, author and date values can be edited on the page.

## Blocks and inline content

- Headings are unnumbered.
- Paragraphs are justified.
- Block quotes have a rule on the left.
- Bullet and numbered lists keep their tightness, start number and `.`/`)`
  style. Task items show a check box.
- Code blocks and inline code print in DejaVu Sans Mono, highlighted when a
  language is given.
- Tables keep column alignment, and the header row is bold.
- A horizontal rule prints as a thin line.
- A footnote prints at the bottom of the page. Repeated references share one
  note. Definitions that are never referenced are reported and not printed.
- Emphasis, strong, strikethrough, links and autolinks, and hard and soft
  line breaks are supported.
- Images must be files in the workspace, given relative to the document or
  with `/` for the workspace root. Other forms are refused with a warning and
  a visible marker: URLs, data URIs, absolute paths, and paths that leave the
  workspace. A paragraph that holds only an image becomes a figure, captioned
  with the alt text. Other images print inline at text height.

## Mathematics

`$...$` is inline and `$$...$$` is display. Both use pulldown-cmark/GitHub
rules: the content needs balanced braces, and an inline formula may not start
or end with a space. Inside, the following LaTeX subset is translated token
by token:

- **Characters**: letters, digits, decimal numbers, `+ - * / = < > ( ) [ ] | , ; : . ! '` and primes.
- **Scripts**: `^` and `_` with single tokens or `{...}` groups, `\limits`, `\nolimits`.
- **Greek**: `\alpha` to `\omega`, the variants (`\varepsilon`, `\vartheta`, `\varphi`, ...), and the capital forms.
- **Relations**: `\le \ge \ne \approx \equiv \sim \simeq \cong \propto \ll \gg \subset \subseteq \in \notin \perp \parallel \mid ...`, and `\not` combinations.
- **Arrows**: `\to \rightarrow \leftarrow \Rightarrow \Leftrightarrow \implies \iff \mapsto \longrightarrow ...`
- **Operators**: `\pm \mp \times \div \cdot \circ \cup \cap \setminus \wedge \vee \oplus \otimes ...`
- **Large operators**: `\sum \prod \int \iint \oint \bigcup \bigcap ...` (limits follow the usual display rules).
- **Functions**: `\sin \cos \tan \log \ln \exp \lim \max \min \sup \inf \det \gcd ...`, `\operatorname{...}`, `\bmod`, `\pmod{...}`.
- **Structures**: `\frac` (and `\dfrac`, `\tfrac`, `\cfrac`), `\binom`, `\sqrt`, `\sqrt[n]`, `\left ... \middle ... \right`, `\overset`, `\underset`, `\stackrel`, `\overbrace`, `\underbrace`.
- **Accents**: `\hat \widehat \tilde \bar \overline \underline \vec \dot \ddot \acute \grave \breve \check \mathring`.
- **Fonts**: `\mathbb \mathbf \boldsymbol \mathrm \mathit \mathcal \mathscr \mathfrak \mathsf \mathtt`.
- **Text**: `\text`, `\textrm`, `\textit`, `\textbf`, `\mbox`.
- **Spacing**: `\, \: \; \! \quad \qquad \ ` and `~`.
- **Environments**: `aligned`, `align`, `gathered`, `split`, `equation` (with `&` and `\\`), `cases`, and `matrix`, `pmatrix`, `bmatrix`, `Bmatrix`, `vmatrix`, `Vmatrix`.
- **Styles**: `\displaystyle`, `\textstyle`. `\nonumber` and `\notag` are accepted and have no effect, because equations are unnumbered.

`\big` and its relatives are accepted, but the delimiter keeps its natural
size; this is reported as a note.

Anything else renders as a red `unsupported: <source>` marker and is reported
as a warning. It is never dropped. Examples are unknown commands, `\color`,
`\label`, `\tag`, the `array` environment, and a `\left` without `\right`.

## Page editing

| Context | Behaviour |
|---|---|
| Prose | Typed text is escaped only where it would change meaning. `\`, `` ` `` and `$` are always escaped. `#`, `>`, `-`, `+`, `*`, `=` and `1.` are escaped at the start of a line, `\|` in table cells, and `<` and `&` before tag or entity text. Emphasis characters are escaped when they would pair up. |
| Backspace and Delete | Removes one rendered character, together with its escape or entity. At the start of a paragraph it joins the paragraph to the one before. A hard line break becomes a soft one. |
| Enter | Splits a paragraph. In a quote it keeps the `>` prefix. In a list item it starts a new item, numbered next for ordered lists and unchecked for task lists. In a heading it moves the rest into a new paragraph. |
| Mathematics | Typing inserts LaTeX: `{ } $ # % &` are escaped, `\` becomes `\backslash`, and `^` or `_` opens `^{}` with the caret inside. A space is added after a control word when needed. Backspace removes a whole command, or a one-token script together with its `^` or `_`. Deleting the last token removes the equation. Structural commands such as `\frac` and `\left` are edited in source. |
| Front matter | Values are edited as YAML. A plain value that would need quoting is quoted. |

Code, raw HTML, generated text (list markers, footnote numbers, page numbers)
and marker text cannot be edited on the page. Every edit is checked by
re-parsing: the rendered result must equal the intended change, and
mathematics must not gain errors. Otherwise the edit is refused and the source
is left unchanged.

## Template

- Fonts: New Computer Modern for text and mathematics, DejaVu Sans Mono for
  code. All are embedded.
- Page: A4 with 2.5 cm margins and centred page numbers, unless the front
  matter changes them.
- Page view and PDF export come from the same compiled document.
