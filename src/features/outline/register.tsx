// Outline feature: the Editor's "Outline" sidebar view (a rail toggle) and its
// palette command. The outline reads the active document's text only when
// the view is shown.
import { ListTree } from "lucide-react";

import { layout } from "../../app/layout";
import { EDITOR_PAGE, pages } from "../../app/pages";
import { registry } from "../../app/registry";
import { OutlineView } from "./OutlineView";

export const OUTLINE_VIEW_ID = "editor.outline";

registry.registerView({
  id: OUTLINE_VIEW_ID,
  location: "sidebar",
  title: "Outline",
  icon: ListTree,
  order: 10,
  render: () => <OutlineView />,
});

registry.registerCommand({
  id: "outline.show",
  title: "Show Outline",
  category: "View",
  run: () => {
    pages.show(EDITOR_PAGE);
    layout.showView("sidebar", OUTLINE_VIEW_ID);
  },
});
