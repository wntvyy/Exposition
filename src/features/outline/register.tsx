// Outline feature: the Editor's "Outline" auxiliary view (beside Lean and
// Dependencies, so the Files view stays visible) and its palette command.
// The outline reads the active document's text only while the view is shown.
import { ListTree } from "lucide-react";

import { layout } from "../../app/layout";
import { EDITOR_PAGE, pages } from "../../app/pages";
import { registry } from "../../app/registry";
import { OutlineView } from "./OutlineView";

export const OUTLINE_VIEW_ID = "editor.outline";

registry.registerView({
  id: OUTLINE_VIEW_ID,
  location: "auxiliary",
  title: "Outline",
  icon: ListTree,
  order: 25,
  render: () => <OutlineView />,
});

registry.registerCommand({
  id: "outline.show",
  title: "Show Outline",
  category: "View",
  run: () => {
    pages.show(EDITOR_PAGE);
    layout.showView("auxiliary", OUTLINE_VIEW_ID);
  },
});
