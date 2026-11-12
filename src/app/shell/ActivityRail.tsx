import { Command as CommandIcon, Monitor, Moon, PanelBottom, PanelLeft, PanelRight, Settings, Shapes, SlidersHorizontal, Sun } from "lucide-react";
import { ToggleButton as AriaToggleButton } from "react-aria-components";

import { MenuTrigger, Menu, MenuItem, MenuSection, MenuSeparator, SubmenuTrigger } from "../../ui/components/Menu";
import { IconButton } from "../../ui/components/Button";
import { Toolbar } from "../../ui/components/Panel";
import { Tooltip } from "../../ui/components/Tooltip";
import { setThemePreference, useTheme, type ThemePreference } from "../../ui/theme";
import { layout, useVisibility, type Region } from "../layout";
import { registry } from "../registry";
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

const themeOptions: { id: ThemePreference; label: string; icon: typeof Sun }[] = [
  { id: "light", label: "Light", icon: Sun },
  { id: "dark", label: "Dark", icon: Moon },
  { id: "system", label: "Follow system", icon: Monitor },
];

/**
 * Left activity rail: one toggle per sidebar view (choosing a view shows it;
 * choosing the shown view hides the sidebar), region toggles, and the manage
 * menu (command palette, theme, UI reference).
 */
export function ActivityRail() {
  const { views, active } = useActiveView("sidebar");
  const visibility = useVisibility();
  const preference = useTheme((s) => s.preference);

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
            onChange={() => layout.toggle(region)}
          />
        ))}
        <MenuTrigger>
          <IconButton icon={Settings} aria-label="Manage" size="lg" className="wb-rail__button" tooltip={false} />
          <Menu
            placement="end bottom"
            aria-label="Manage"
            onAction={(key) => {
              if (key === "palette") void registry.runCommand("workbench.action.showCommands");
              if (key === "settings" && registry.command("workbench.action.openSettings")) {
                void registry.runCommand("workbench.action.openSettings");
              }
              if (key === "reference") void registry.runCommand("workbench.action.showUiReference");
            }}
          >
            <MenuItem id="palette" icon={CommandIcon} keybinding="Mod+Shift+P">
              Command Palette…
            </MenuItem>
            <MenuItem id="settings" icon={SlidersHorizontal} keybinding="Mod+,">
              Settings…
            </MenuItem>
            <SubmenuTrigger>
              <MenuItem id="theme" icon={preference === "dark" ? Moon : preference === "light" ? Sun : Monitor} hasSubmenu>
                Color Theme
              </MenuItem>
              <Menu
                isSubmenu
                aria-label="Color theme"
                selectionMode="single"
                selectedKeys={[preference]}
                onAction={(key) => setThemePreference(key as ThemePreference)}
              >
                <MenuSection title="Color theme">
                  {themeOptions.map((option) => (
                    <MenuItem key={option.id} id={option.id}>
                      {option.label}
                    </MenuItem>
                  ))}
                </MenuSection>
              </Menu>
            </SubmenuTrigger>
            <MenuSeparator />
            <MenuItem id="reference" icon={Shapes}>
              UI Reference
            </MenuItem>
          </Menu>
        </MenuTrigger>
      </Toolbar>
    </nav>
  );
}
