// Page editing feature: Typst documents in Source, Page, and Split modes.
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
