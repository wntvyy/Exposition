import { Command as CommandIcon, FileText, Monitor, Moon, PanelBottom, PanelLeft, PanelRight, Settings, Shapes, SlidersHorizontal, Sun } from "lucide-react";
import { Focusable, ToggleButton as AriaToggleButton } from "react-aria-components";

import { MenuTrigger, Menu, MenuItem, MenuSection, MenuSeparator, SubmenuTrigger } from "../../ui/components/Menu";
import { IconButton } from "../../ui/components/Button";
import { Toolbar } from "../../ui/components/Panel";
import { Tooltip } from "../../ui/components/Tooltip";
import { setThemePreference, useTheme, type ThemePreference } from "../../ui/theme";
import { layout, useVisibility, type Region } from "../layout";
import { EDITOR_PAGE, EDITOR_PAGE_TITLE, PAGE_KEYBINDINGS, pages, usePages } from "../pages";
import { registry } from "../registry";
import { toggleEditorRegion } from "./builtins";
import { useRegistryVersion } from "./useRegistry";
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

/**
 * A page switcher entry: a navigation button marked `aria-current="page"` when
 * its page is shown (not a toggle: choosing the shown page again keeps it).
 */
function PageButton({
  id,
  title,
  icon: Icon,
  current,
}: {
  id: string;
  title: string;
  icon: React.ComponentType<{ size?: number; "aria-hidden"?: boolean }>;
  current: boolean;
}) {
  return (
    <Tooltip label={`${title} page`} keybinding={PAGE_KEYBINDINGS[id]} placement="right">
      <Focusable>
        <button
          type="button"
          className="wb-rail__button wb-rail__page"
          aria-label={title}
          aria-current={current ? "page" : undefined}
          data-selected={current || undefined}
          data-page-button={id}
          onClick={() => pages.show(id)}
        >
          <Icon size={20} aria-hidden />
        </button>
      </Focusable>
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

  useRegistryVersion();
  const activePage = usePages((s) => s.active);
  const registeredPages = registry.pages();
  const onEditor = activePage === EDITOR_PAGE;

  return (
    <nav className="wb-rail" aria-label="Activity bar" data-region="activitybar">
      {registeredPages.length > 0 && (
        <Toolbar aria-label="Pages" orientation="vertical" className="wb-rail__group wb-rail__group--pages">
          {[{ id: EDITOR_PAGE, title: EDITOR_PAGE_TITLE, icon: FileText }, ...registeredPages].map((page) => (
            <PageButton key={page.id} id={page.id} title={page.title} icon={page.icon} current={activePage === page.id} />
          ))}
        </Toolbar>
      )}
      <Toolbar aria-label="Sidebar views" orientation="vertical" className="wb-rail__group">
        {views.map((view) => (
          <RailToggle
            key={view.id}
            label={view.title}
            icon={view.icon}
            isSelected={onEditor && visibility.sidebar && active?.id === view.id}
            onChange={() => {
              if (!onEditor) {
                pages.show(EDITOR_PAGE);
                layout.showView("sidebar", view.id);
              } else if (visibility.sidebar && active?.id === view.id) layout.setVisible("sidebar", false);
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
            isSelected={onEditor && visibility[region]}
            onChange={() => toggleEditorRegion(region)}
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
