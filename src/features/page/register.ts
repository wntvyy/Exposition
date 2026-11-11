// Page editing feature: the exact-page editor for Typst files.
import { registry } from "../../app/registry";
import { installDevHarness } from "./devHarness";
import { PageEditor } from "./PageEditor";

registry.registerEditor({
  id: "typst-page",
  priority: 10,
  matches: (resource) => resource.path?.toLowerCase().endsWith(".typ") ?? false,
  // Source and Split are composed with the workspace SourceEditor at integration.
  modes: ["page"],
  component: PageEditor,
});

installDevHarness();
