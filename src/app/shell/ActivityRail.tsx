import { PanelBottom, PanelLeft, PanelRight } from "lucide-react";
import { ToggleButton as AriaToggleButton } from "react-aria-components";

import { Toolbar } from "../../ui/components/Panel";
import { Tooltip } from "../../ui/components/Tooltip";
import { layout, useVisibility, type Region } from "../layout";
import { EDITOR_PAGE, usePages } from "../pages";
import { toggleEditorRegion } from "./builtins";
import { useActiveView } from "./ViewContainer";

function RailToggle({
  label,
  keybinding,
  isSelected,
  onChange,
  icon: Icon,
}: {
  label: string;
  keybinding?: string;
  isSelected: boolean;
  onChange: () => void;
  icon: React.ComponentType<{ size?: number; "aria-hidden"?: boolean }>;
}) {
  return (
    <Tooltip label={label} keybinding={keybinding} placement="right">
      <AriaToggleButton className="wb-rail__button" aria-label={label} isSelected={isSelected} onChange={onChange}>
        <Icon size={20} aria-hidden />
      </AriaToggleButton>
    </Tooltip>
  );
}

const regionToggles: { region: Region; label: string; keybinding: string; icon: typeof PanelLeft }[] = [
  { region: "sidebar", label: "Toggle sidebar", keybinding: "Mod+B", icon: PanelLeft },
  { region: "panel", label: "Toggle panel", keybinding: "Mod+J", icon: PanelBottom },
  { region: "auxiliary", label: "Toggle auxiliary panel", keybinding: "Mod+Alt+B", icon: PanelRight },
];

/**
 * The Editor page's activity rail: one toggle per sidebar view (choosing a
 * view shows it; choosing the shown view hides the sidebar) and the layout
 * toggles. Pages are switched from the page bar; on Research, Agents, and
 * Graph the rail is not shown, so those pages use the full width.
 */
export function ActivityRail() {
  const { views, active } = useActiveView("sidebar");
  const visibility = useVisibility();
  const activePage = usePages((s) => s.active);
  if (activePage !== EDITOR_PAGE) return null;

  return (
    <nav className="wb-rail" aria-label="Activity bar" data-region="activitybar">
      <Toolbar aria-label="Sidebar views" orientation="vertical" className="wb-rail__group">
        {views.map((view) => (
          <RailToggle
            key={view.id}
            label={view.title}
            icon={view.icon}
            isSelected={visibility.sidebar && active?.id === view.id}
            onChange={() => {
              if (visibility.sidebar && active?.id === view.id) layout.setVisible("sidebar", false);
              else layout.showView("sidebar", view.id);
            }}
          />
        ))}
      </Toolbar>
      <Toolbar aria-label="Layout" orientation="vertical" className="wb-rail__group wb-rail__group--end">
        {regionToggles.map(({ region, label, keybinding, icon }) => (
          <RailToggle
            key={region}
            label={label}
            keybinding={keybinding}
            icon={icon}
            isSelected={visibility[region]}
            onChange={() => toggleEditorRegion(region)}
          />
        ))}
      </Toolbar>
    </nav>
  );
}
