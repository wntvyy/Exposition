// Page editing feature: typeset documents in Source, Page, and Split modes.
import { registry } from "../../app/registry";
import { TypstEditor } from "./TypstEditor";

registry.registerEditor({
  id: "typst.document",
  // Above the generic source editor for workspace .typ files.
  priority: 10,
  matches: (resource) =>
    resource.key.startsWith("file:") && (resource.path?.toLowerCase().endsWith(".typ") ?? false),
  modes: ["source", "page", "split"],
  component: TypstEditor,
});

// LaTeX (.tex) documents: the same Source/Page/Split composition; pages come
// from pdfLaTeX/LuaLaTeX builds (exposition-tex) through the shared flow.
registry.registerEditor({
  id: "latex.document",
  priority: 10,
  matches: (resource) =>
    resource.key.startsWith("file:") && (resource.path?.toLowerCase().endsWith(".tex") ?? false),
  modes: ["source", "page", "split"],
  component: TypstEditor,
});

// Markdown: the same composition (one document, Monaco source, exact page)
// over the Markdown print profile's builds.
registry.registerEditor({
  id: "markdown.document",
  priority: 10,
  matches: (resource) =>
    resource.key.startsWith("file:") && /\.(md|markdown)$/i.test(resource.path ?? ""),
  modes: ["source", "page", "split"],
  component: TypstEditor,
});
