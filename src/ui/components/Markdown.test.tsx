// The shared Markdown renderer as an untrusted-content boundary: hostile HTML,
// URLs, images, and KaTeX input render inert; streaming prefixes never throw;
// copy returns the exact source; links open only through app_open_external.
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const ipc = vi.hoisted(() => ({ appOpenExternal: vi.fn() }));
vi.mock("../../lib/ipc", () => ({ commands: ipc }));
// Monaco does not run under jsdom; the real highlighter is exercised in the app (e2e a1-responses).
vi.mock("./MarkdownHighlight", () => ({
  highlight: vi.fn(async (code: string) =>
    code.split("\n").map((line) => [{ text: line, kind: line.startsWith("import") ? "keyword" : null }]),
  ),
}));

const { Markdown, ResponseSurface } = await import("./Markdown");
const { StateSizeProvider } = await import("./stateSize");
const { externalUrl, isOpenFence, isOpenMath, normalizeMathDelimiters, runnableLanguage } = await import("./MarkdownPolicy");

const { contrastRatio } = await import("../color");
const { palettes, themeNames } = await import("../tokens");

const writeText = vi.fn<(text: string) => Promise<void>>();

function renderMd(source: string, props: Partial<Parameters<typeof Markdown>[0]> = {}) {
  return render(<Markdown source={source} {...props} />);
}

/** Every element and attribute that could run script, load a resource, or navigate. */
function liveHazards(root: Element): string[] {
  const found: string[] = [];
  const dangerous = ["script", "iframe", "frame", "object", "embed", "img", "image", "video", "audio", "source", "link", "style", "form", "base", "meta", "svg", "use", "foreignobject"];
  for (const el of root.querySelectorAll("*")) {
    const tag = el.tagName.toLowerCase();
    // KaTeX draws some glyphs (\sqrt, wide accents) as inline SVG paths, and controls carry lucide icons:
    // the renderer's own output, not response markup.
    if (dangerous.includes(tag) && !el.closest(".katex-html") && !el.closest("svg.lucide")) found.push(`<${tag}>`);
    for (const attr of el.getAttributeNames()) {
      if (attr.startsWith("on")) found.push(`${tag}[${attr}]`);
      if (["href", "src", "srcset", "action", "formaction", "xlink:href", "target", "srcdoc", "background", "poster", "data"].includes(attr)) {
        found.push(`${tag}[${attr}=${el.getAttribute(attr)}]`);
      }
    }
  }
  return found;
}

beforeEach(() => {
  ipc.appOpenExternal.mockReset();
  ipc.appOpenExternal.mockResolvedValue(undefined);
  writeText.mockReset();
  writeText.mockResolvedValue(undefined);
  Object.defineProperty(navigator, "clipboard", { value: { writeText }, configurable: true });
});
afterEach(() => cleanup());

// ---------------------------------------------------------------- policy

describe("external URL policy (mirrors app_open_external)", () => {
  it("accepts absolute http and https addresses with a host", () => {
    for (const url of ["https://arxiv.org/abs/2401.00001", "http://example.org/a?b=c#d", "HTTPS://Example.org/X", "https://例え.jp/パス"]) {
      expect(externalUrl(url)).toBe(url);
    }
  });

  it("refuses every other scheme, relative links, credentials, and hidden characters", () => {
    for (const url of [
      "javascript:alert(1)",
      "JaVaScRiPt:alert(1)",
      " javascript:alert(1)",
      "java\tscript:alert(1)",
      "vbscript:msgbox(1)",
      "data:text/html,<script>alert(1)</script>",
      "data:image/png;base64,AAAA",
      "file:///C:/Windows/win.ini",
      "tauri://localhost/index.html",
      "ipc://localhost/app_info",
      "mailto:someone@example.org",
      "blob:https://example.org/uuid",
      "about:blank",
      "//example.org/x",
      "www.example.org",
      "/relative/path",
      "notes.md",
      "#fragment",
      "https://user:pw@example.org/",
      "https://user@example.org/",
      "https://example.org/a b",
      "https://example.org/\u202e",
      "https://exa\u200bmple.org/",
      "https://example.org/\u0000",
      `https://example.org/${"a".repeat(2100)}`,
      "",
    ]) {
      expect(externalUrl(url), url).toBeNull();
    }
  });
});

describe("math delimiters", () => {
  it("normalizes \\( \\) and \\[ \\] outside code", () => {
    expect(normalizeMathDelimiters("a \\(x^2\\) b")).toBe("a $x^2$ b");
    expect(normalizeMathDelimiters("\\[\nx = 1\n\\]")).toBe("\n$$\n\nx = 1\n\n$$\n");
    expect(normalizeMathDelimiters("`\\(x\\)` and \\\\(y\\\\)")).toBe("`\\(x\\)` and \\\\(y\\\\)");
    expect(normalizeMathDelimiters("```\n\\(x\\)\n```\n\\(y\\)")).toBe("```\n\\(x\\)\n```\n$y$");
    expect(normalizeMathDelimiters("unclosed \\(x")).toBe("unclosed \\(x");
  });

  it("knows when a fence or a display formula is still open while streaming", () => {
    const open = "Text\n\n```python\nprint(1)";
    expect(isOpenFence(open, 6, open.length)).toBe(true);
    const closed = "Text\n\n```python\nprint(1)\n```";
    expect(isOpenFence(closed, 6, closed.length)).toBe(false);
    const math = "$$\n\\frac{1}{2}";
    expect(isOpenMath(math, 0, math.length)).toBe(true);
    expect(isOpenMath("$$\nx\n$$", 0, 7)).toBe(false);
  });

  it("maps runnable fence languages", () => {
    expect(runnableLanguage("python")).toBe("python");
    expect(runnableLanguage("py")).toBe("python");
    expect(runnableLanguage("R")).toBe("r");
    expect(runnableLanguage("jl")).toBe("julia");
    expect(runnableLanguage("javascript")).toBeNull();
    expect(runnableLanguage(null)).toBeNull();
  });
});

// ---------------------------------------------------------------- rendering

describe("Markdown rendering", () => {
  it("renders headings, emphasis, lists, blockquotes, tables, and code", async () => {
    const { container } = renderMd(
      "# Title\n\nSome *emphasis*, **strong** and ~~struck~~ text.\n\n- one\n- two\n\n1. first\n\n> quoted\n\n| a | b |\n|---|--:|\n| 1 | 2 |\n\n- [x] done\n- [ ] open\n\n```python\nimport math\nprint(1)\n```",
    );
    expect(container.querySelector("h1")?.textContent).toBe("Title");
    expect(container.querySelector("em")?.textContent).toBe("emphasis");
    expect(container.querySelector("strong")?.textContent).toBe("strong");
    expect(container.querySelector("del")?.textContent).toBe("struck");
    expect(container.querySelectorAll("ul > li")).toHaveLength(4);
    expect(container.querySelector("ol > li")?.textContent).toBe("first");
    expect(container.querySelector("blockquote")?.textContent?.trim()).toBe("quoted");
    expect(container.querySelector(".ui-md-table table th")?.textContent).toBe("a");
    const boxes = [...container.querySelectorAll<HTMLInputElement>("input[type=checkbox]")];
    expect(boxes.map((b) => [b.checked, b.disabled])).toEqual([
      [true, true],
      [false, true],
    ]);
    expect(screen.getByTestId("md-code-text").textContent).toBe("import math\nprint(1)");
    // Highlighting arrives asynchronously as spans with token classes (never an HTML string).
    await act(async () => {});
    expect(container.querySelector(".ui-md-tok-keyword")?.textContent).toBe("import math");
    expect(screen.getByTestId("md-code-text").textContent).toBe("import math\nprint(1)");
    expect(liveHazards(container)).toEqual([]);
  });

  it("keeps plain text readable, including angle brackets", () => {
    const { container } = renderMd("Just a sentence with a < b and 3 > 2, and Vec<T> in prose.");
    expect(container.textContent).toBe("Just a sentence with a < b and 3 > 2, and Vec<T> in prose.");
  });

  it("typesets inline and display math with KaTeX and keeps MathML for screen readers", () => {
    const { container } = renderMd("Inline $x^2$ and \\(y_1\\).\n\n$$\n\\frac{a}{b}\n$$\n\n\\[\n\\sum_i i\n\\]");
    expect(container.querySelectorAll("[data-math=inline] .katex")).toHaveLength(2);
    expect(container.querySelectorAll("[data-math=display] .katex-display")).toHaveLength(2);
    expect(container.querySelectorAll(".katex math")).toHaveLength(4);
    expect(container.querySelector(".katex math annotation")?.textContent).toBe("x^2");
  });

  it("shows TeX that cannot be parsed as code", () => {
    const { container } = renderMd("$$\n\\frac{1}{\n$$\n\nand $\\frac{1}{$ inline");
    expect(container.querySelector("[data-math-source=display]")?.textContent).toContain("\\frac{1}{");
    expect(container.querySelector("[data-math-source=inline]")?.textContent).toBe("$\\frac{1}{$");
    expect(container.querySelector(".katex-error")).toBeNull();
  });

  it("lets wide tables scroll as a keyboard-focusable region", () => {
    const scrollWidth = vi.spyOn(Element.prototype, "scrollWidth", "get").mockReturnValue(2000);
    const clientWidth = vi.spyOn(Element.prototype, "clientWidth", "get").mockReturnValue(300);
    try {
      renderMd(`| ${Array.from({ length: 30 }, (_, i) => `c${i}`).join(" | ")} |\n|${"---|".repeat(30)}\n| ${"1 | ".repeat(30)}`);
      const table = screen.getByRole("region", { name: "Table" });
      expect(table.getAttribute("tabindex")).toBe("0");
      const code = renderMd("```\nwide\n```");
      expect(within(code.container).getByRole("region", { name: "Code" }).getAttribute("tabindex")).toBe("0");
    } finally {
      scrollWidth.mockRestore();
      clientWidth.mockRestore();
    }
  });
});

// ---------------------------------------------------------------- hostile input

describe("untrusted content renders inert", () => {
  it("shows raw HTML as literal text and never creates its elements", () => {
    const hostile = [
      "<script>window.__xsr = 1</script>",
      "",
      '<img src="https://evil.example/x.png" onerror="window.__xsr = 2">',
      "",
      '<iframe src="https://evil.example/"></iframe>',
      "",
      "<style>body { display: none }</style>",
      "",
      '<svg onload="alert(1)"><use href="#x"/></svg>',
      "",
      'inline <a href="javascript:alert(1)">anchor</a> and <b onmouseover="alert(1)">bold</b> and <!-- comment -->',
      "",
      '<details open><summary>s</summary><form action="https://evil.example"><input formaction="x"></form></details>',
    ].join("\n");
    const { container } = renderMd(hostile);
    expect(liveHazards(container)).toEqual([]);
    expect(container.querySelector("a, b, details, input, form")).toBeNull();
    expect(container.textContent).toContain("<script>window.__xsr = 1</script>");
    expect(container.textContent).toContain('<a href="javascript:alert(1)">anchor</a>');
    expect((window as unknown as { __xsr?: unknown }).__xsr).toBeUndefined();
  });

  it("renders HTML entities as text", () => {
    const { container } = renderMd("&lt;script&gt;alert(1)&lt;/script&gt; and &#x3C;img src=x onerror=alert(1)&#x3E; &amp; &copy;");
    expect(container.textContent).toBe("<script>alert(1)</script> and <img src=x onerror=alert(1)> & ©");
    expect(liveHazards(container)).toEqual([]);
  });

  it("renders javascript: and every other non-http link inert, in all spellings", () => {
    const links = [
      "[a](javascript:alert(1))",
      "[b](JAVASCRIPT:alert(1))",
      "[c](&#106;avascript:alert(1))",
      "[d](&#x6A;avascript&#x3A;alert(1))",
      "[e](java&#x09;script:alert(1))",
      "[f](<javascript:alert(1)>)",
      "[g](%6Aavascript:alert(1))",
      "[h](vbscript:msgbox(1))",
      "[i](data:text/html;base64,PHNjcmlwdD5hbGVydCgxKTwvc2NyaXB0Pg==)",
      "[j](file:///C:/Windows/win.ini)",
      "[k](tauri://localhost/index.html)",
      "[l](ipc://localhost/app_info)",
      "[m](notes/a.md)",
      "[n](#heading)",
      "[o](mailto:a@b.c)",
      "[p](https://user:pw@example.org/)",
      "<javascript:alert(1)>",
      "<a@b.c>",
    ];
    const { container } = renderMd(links.join("\n\n"));
    expect(container.querySelectorAll("[data-external-link]")).toHaveLength(0);
    expect(container.querySelectorAll("[data-inert-link]")).toHaveLength(links.length);
    expect(container.querySelector("a")).toBeNull();
    expect(liveHazards(container)).toEqual([]);
    for (const el of container.querySelectorAll("[data-inert-link]")) fireEvent.click(el);
    expect(ipc.appOpenExternal).not.toHaveBeenCalled();
  });

  it("resolves reference-style links through the same policy", () => {
    const { container } = renderMd("[bad][r] and [good][s]\n\n[r]: javascript:alert(1)\n[s]: https://example.org/doc \"Title\"");
    expect(container.querySelectorAll("[data-inert-link]")).toHaveLength(1);
    const link = screen.getByRole("link", { name: "good" });
    expect(link.hasAttribute("href")).toBe(false);
    expect(link.hasAttribute("title")).toBe(false);
    expect(link.nextElementSibling?.textContent).toBe("https://example.org/doc");
  });

  it("renders footnotes without live links or stray addresses", () => {
    const { container } = renderMd("A claim[^1].\n\n[^1]: The source.");
    expect(container.querySelector("section[data-footnotes]")?.textContent).toContain("The source.");
    expect(container.querySelectorAll("[data-external-link]")).toHaveLength(0);
    expect(container.querySelectorAll(".ui-md-link__address")).toHaveLength(0);
    expect(liveHazards(container)).toEqual([]);
  });

  it("shows autolinks once and offers only http(s) as links", () => {
    const { container } = renderMd("See <https://example.org/a> and www.example.org and https://example.org/b.");
    const links = screen.getAllByRole("link");
    expect(links.map((l) => l.textContent)).toEqual(["https://example.org/a", "www.example.org", "https://example.org/b"]);
    // An autolink's text already is its address.
    expect(container.querySelectorAll(".ui-md-link__address")).toHaveLength(0);
    expect(liveHazards(container)).toEqual([]);
  });

  it("opens an http(s) link only through app_open_external, on click or Enter", () => {
    renderMd("[Basel problem](https://en.wikipedia.org/wiki/Basel_problem)");
    const link = screen.getByRole("link", { name: "Basel problem" });
    expect(link.hasAttribute("href")).toBe(false);
    expect(link.hasAttribute("target")).toBe(false);
    expect(link.getAttribute("tabindex")).toBe("0");
    const click = new MouseEvent("click", { bubbles: true, cancelable: true });
    link.dispatchEvent(click);
    expect(click.defaultPrevented).toBe(true);
    expect(ipc.appOpenExternal).toHaveBeenCalledTimes(1);
    expect(ipc.appOpenExternal).toHaveBeenCalledWith("https://en.wikipedia.org/wiki/Basel_problem");
    fireEvent.keyDown(link, { key: "Enter" });
    expect(ipc.appOpenExternal).toHaveBeenCalledTimes(2);
    fireEvent.keyDown(link, { key: " " });
    expect(ipc.appOpenExternal).toHaveBeenCalledTimes(2);
  });

  it("never loads images: placeholders with alt text and address, Open for http(s) only", () => {
    const { container } = renderMd(
      [
        "![a chart](https://example.org/chart.png)",
        "![pixel](data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==)",
        "![x](javascript:alert(1))",
        "![](file:///C:/secret.png)",
      ].join("\n\n"),
    );
    expect(container.querySelector("img")).toBeNull();
    const images = screen.getAllByTestId("md-image");
    expect(images).toHaveLength(4);
    expect(images[0]!.textContent).toContain("a chart");
    expect(images[0]!.textContent).toContain("https://example.org/chart.png");
    expect(images[3]!.textContent).toContain("Image");
    const open = screen.getAllByRole("button", { name: /Open/ });
    expect(open).toHaveLength(1);
    expect(open[0]!.getAttribute("aria-label")).toBe("Open a chart in the browser");
    fireEvent.click(open[0]!);
    expect(ipc.appOpenExternal).toHaveBeenCalledWith("https://example.org/chart.png");
    expect(liveHazards(container)).toEqual([]);
  });

  it("refuses KaTeX commands that link, load, or style, and bounds sizes and expansion", () => {
    const tex = [
      "$\\href{javascript:alert(1)}{x}$",
      "$\\url{https://evil.example}$",
      "$\\includegraphics{https://evil.example/x.png}$",
      "$\\htmlClass{evil}{x}$",
      "$\\htmlId{evil}{x}$",
      "$\\htmlStyle{position:fixed;inset:0}{x}$",
      "$\\htmlData{foo=bar}{x}$",
      "$\\rule{100000em}{100000em}$",
      "$\\kern{100000em}x$",
      "$\\def\\a{\\b\\b}\\def\\b{\\c\\c}\\def\\c{\\d\\d}\\def\\d{\\e\\e}\\def\\e{\\f\\f}\\def\\f{\\g\\g}\\def\\g{\\h\\h}\\def\\h{\\i\\i}\\def\\i{xx}\\a\\a\\a$",
      "$\\gdef\\leak{LEAK}x$ then $\\leak$",
    ].join("\n\n");
    const { container } = renderMd(tex);
    expect(liveHazards(container)).toEqual([]);
    expect(container.querySelector("a, img, .evil, #evil, [data-foo]")).toBeNull();
    for (const el of container.querySelectorAll<HTMLElement>("[style]")) {
      expect(el.getAttribute("style")).not.toMatch(/position|inset/);
      for (const match of el.getAttribute("style")!.matchAll(/(-?[\d.]+)em/g)) {
        expect(Math.abs(Number(match[1]))).toBeLessThanOrEqual(10.5);
      }
    }
    // The macro bomb hits maxExpand and is shown as source; \gdef does not leak into the next formula.
    const sources = [...container.querySelectorAll("[data-math-source=inline]")].map((el) => el.textContent);
    expect(sources.some((s) => s?.startsWith("$\\def\\a"))).toBe(true);
    const leakUse = [...container.querySelectorAll("[data-math=inline], [data-math-source=inline]")].find((el) =>
      (el.querySelector("annotation")?.textContent ?? el.textContent ?? "").replace(/\$/g, "") === String.raw`\leak`,
    );
    expect(leakUse).toBeTruthy();
    expect(leakUse!.querySelector(".katex-html")?.textContent ?? leakUse!.textContent).not.toContain("LEAK");
  });
});

// ---------------------------------------------------------------- streaming

const LONG_DOCUMENT = [
  "# Convergence",
  "",
  "The series converges because $\\sum 1/n^2$ is bounded; see [notes](https://example.org/notes).",
  "",
  "$$",
  "\\sum_{n=1}^{\\infty} \\frac{1}{n^2} = \\frac{\\pi^2}{6}",
  "$$",
  "",
  "```python",
  "import math",
  "print(sum(1 / n**2 for n in range(1, 10_001)))",
  "```",
  "",
  "| N | Sum |",
  "|---:|---:|",
  "| 10 | 1.5498 |",
  "| 100 | 1.6350 |",
  "",
  "> A quote with **bold *nested* text** and `code`.",
  "",
  "- a list item with \\(x\\)",
  "- another with ![img](https://example.org/i.png)",
  "",
  "\\[",
  "\\int_0^1 x^2 \\, dx = \\frac{1}{3}",
  "\\]",
  "",
  "Footnote reference[^1].",
  "",
  "[^1]: The footnote.",
].join("\n");

describe("streaming", () => {
  it("never throws on any prefix of a long document", () => {
    for (let end = 0; end <= LONG_DOCUMENT.length; end += 3) {
      const { container, unmount } = renderMd(LONG_DOCUMENT.slice(0, end), { streaming: true });
      expect(liveHazards(container)).toEqual([]);
      unmount();
    }
  });

  it("follows a growing source and keeps unclosed fences and math as text until they close", async () => {
    const { container, rerender } = render(<Markdown source="Start" streaming />);
    const grow = async (source: string) => {
      rerender(<Markdown source={source} streaming />);
      await act(async () => {
        await new Promise((resolve) => setTimeout(resolve, 40));
      });
    };
    await grow("Start\n\n```python\nprint(1)");
    const open = container.querySelector("[data-testid=md-code-block]");
    expect(open?.hasAttribute("data-open")).toBe(true);
    expect(within(container).queryByRole("button", { name: /Copy/ })).toBeNull();
    await grow("Start\n\n```python\nprint(1)\n```");
    expect(container.querySelector("[data-testid=md-code-block]")?.hasAttribute("data-open")).toBe(false);
    expect(within(container).getByRole("button", { name: "Copy code" })).toBeTruthy();
    await grow("Start\n\n```python\nprint(1)\n```\n\n$$\n\\frac{1}{2}");
    expect(container.querySelector("[data-math-source=display]")).not.toBeNull();
    await grow("Start\n\n```python\nprint(1)\n```\n\n$$\n\\frac{1}{2}\n$$");
    expect(container.querySelector("[data-math=display] .katex")).not.toBeNull();
  });

  it("re-parses at most once per animation frame while streaming", async () => {
    const frames: FrameRequestCallback[] = [];
    const raf = vi.spyOn(window, "requestAnimationFrame").mockImplementation((cb) => frames.push(cb));
    try {
      const { container, rerender } = render(<Markdown source="a" streaming />);
      for (const text of ["a b", "a b c", "a b c d"]) rerender(<Markdown source={text} streaming />);
      expect(frames).toHaveLength(1);
      expect(container.textContent).toBe("a");
      act(() => frames.shift()!(0));
      expect(container.textContent).toBe("a b c d");
    } finally {
      raf.mockRestore();
    }
  });
});

// ---------------------------------------------------------------- copy, run, surface

describe("copy and run actions", () => {
  it("copies a fence's exact text and the message's original Markdown", async () => {
    const fence = "def f(x):\n\treturn x  # tab, trailing spaces   \n\nprint(f('π'))";
    const source = `Intro with \\(x\\) math.\n\n\`\`\`python\n${fence}\n\`\`\`\n`;
    render(
      <ResponseSurface label="Agent" copyText={source}>
        <Markdown source={source} />
      </ResponseSurface>,
    );
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Copy code" }));
    });
    expect(writeText).toHaveBeenLastCalledWith(fence);
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Copy response" }));
    });
    expect(writeText).toHaveBeenLastCalledWith(source);
    expect(screen.getByRole("button", { name: "Copied response" })).toBeTruthy();
  });

  it("says so when the clipboard refuses", async () => {
    writeText.mockRejectedValue(new Error("denied"));
    renderMd("```\nx\n```");
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Copy code" }));
    });
    expect(screen.getByRole("button", { name: "Could not copy code" })).toBeTruthy();
  });

  it("offers the host's Run action on Python, R, and Julia fences only, and runs nothing on render", () => {
    const run = vi.fn();
    renderMd("```py\nprint(1)\n```\n\n```r\nx <- 1\n```\n\n```julia\nx = 1\n```\n\n```javascript\nalert(1)\n```\n\n```\nplain\n```", {
      runAction: { label: "Run in Research", run },
    });
    const buttons = screen.getAllByRole("button", { name: "Run in Research" });
    expect(buttons).toHaveLength(3);
    expect(run).not.toHaveBeenCalled();
    fireEvent.click(buttons[0]!);
    expect(run).toHaveBeenCalledWith("print(1)", "python");
    fireEvent.click(buttons[1]!);
    expect(run).toHaveBeenLastCalledWith("x <- 1", "r");
  });

  it("frames a response: label, Copy, compact density inside regions", () => {
    const { container } = render(
      <StateSizeProvider value="compact">
        <ResponseSurface label="Agent" copyText="x">
          <Markdown source="x" />
        </ResponseSurface>
      </StateSizeProvider>,
    );
    const surface = container.querySelector(".ui-response")!;
    expect(surface.getAttribute("data-density")).toBe("compact");
    expect(surface.getAttribute("data-context")).toBe("region");
    expect(screen.getByText("Agent")).toBeTruthy();
    cleanup();
    const page = render(
      <ResponseSurface label="Agent">
        <Markdown source="x" />
      </ResponseSurface>,
    );
    expect(page.container.querySelector(".ui-response")?.getAttribute("data-density")).toBe("comfortable");
    expect(page.container.querySelector(".ui-response")?.getAttribute("data-context")).toBe("page");
    expect(within(page.container).queryByRole("button")).toBeNull();
  });
});

// ---------------------------------------------------------------- contrast

describe("contrast", () => {
  // Every text role the renderer uses (prose, muted addresses and labels, comments,
  // links, syntax colors, TeX errors) on both response fills, in both themes.
  const roles = ["text", "text-muted", "text-subtle", "accent-text", "info-text", "success-text", "warning-text", "status-stale-text", "danger-text"] as const;
  const fills = ["surface", "surface-sunken"] as const;
  it.each(themeNames)("text on response and code fills meets 4.5:1 (%s)", (theme) => {
    const failures = fills.flatMap((bg) =>
      roles
        .map((fg) => ({ fg, bg, ratio: contrastRatio(palettes[theme][fg], palettes[theme][bg]) }))
        .filter((r) => r.ratio < 4.5)
        .map((r) => `${r.fg} on ${r.bg}: ${r.ratio.toFixed(2)}`),
    );
    expect(failures).toEqual([]);
  });
});
