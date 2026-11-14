import { Command as CommandIcon, FileText, Monitor, Moon, Settings, Shapes, SlidersHorizontal, Sun } from "lucide-react";
import { Focusable } from "react-aria-components";

import { IconButton } from "../../ui/components/Button";
import { Menu, MenuItem, MenuSection, MenuSeparator, MenuTrigger, SubmenuTrigger } from "../../ui/components/Menu";
import { Toolbar } from "../../ui/components/Panel";
import { Tooltip } from "../../ui/components/Tooltip";
import { setThemePreference, useTheme, type ThemePreference } from "../../ui/theme";
import { EDITOR_PAGE, EDITOR_PAGE_TITLE, PAGE_KEYBINDINGS, pages, usePages } from "../pages";
import { registry } from "../registry";
import { useRegistryVersion } from "./useRegistry";

const themeOptions: { id: ThemePreference; label: string; icon: typeof Sun }[] = [
  { id: "light", label: "Light", icon: Sun },
  { id: "dark", label: "Dark", icon: Moon },
  { id: "system", label: "Follow system", icon: Monitor },
];

/**
 * A page tab: a navigation button marked `aria-current="page"` when its page
 * is shown (not a toggle: choosing the shown page again keeps it).
 */
function PageTab({
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
    <Tooltip label={`${title} page`} keybinding={PAGE_KEYBINDINGS[id]} placement="bottom">
      <Focusable>
        <button
          type="button"
          className="wb-pagebar__tab"
          aria-label={title}
          aria-current={current ? "page" : undefined}
          data-selected={current || undefined}
          data-page-button={id}
          onClick={() => pages.show(id)}
        >
          <Icon size={14} aria-hidden />
          <span>{title}</span>
        </button>
      </Focusable>
    </Tooltip>
  );
}

/** The Manage menu: command palette, settings, color theme, UI reference. */
function ManageMenu() {
  const preference = useTheme((s) => s.preference);
  return (
    <MenuTrigger>
      <IconButton icon={Settings} aria-label="Manage" className="wb-pagebar__manage" />
      <Menu
        placement="bottom end"
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
  );
}

/**
 * The page bar across the top of the window: the project (contributed by the
 * workspace feature as a `pagebar` status item), the page tabs (Editor,
 * Research, Agents, Graph; Mod+1…4), and the Manage menu. It is an F6 region
 * (`pagebar`); the tabs are one toolbar (arrow keys move, Tab leaves).
 */
export function PageBar() {
  useRegistryVersion();
  const activePage = usePages((s) => s.active);
  const registeredPages = registry.pages();
  const leading = registry.statusItemsAt("pagebar");
  return (
    <header className="wb-pagebar" data-region="pagebar" data-testid="page-bar">
      {leading.length > 0 && (
        <div className="wb-pagebar__project">
          {leading.map((item) => (
            <div key={item.id} className="wb-pagebar__item">
              {item.render()}
            </div>
          ))}
        </div>
      )}
      <nav className="wb-pagebar__nav" aria-label="Pages">
        <Toolbar aria-label="Pages" className="wb-pagebar__tabs">
          {[{ id: EDITOR_PAGE, title: EDITOR_PAGE_TITLE, icon: FileText }, ...registeredPages].map((page) => (
            <PageTab key={page.id} id={page.id} title={page.title} icon={page.icon} current={activePage === page.id} />
          ))}
        </Toolbar>
      </nav>
      <div className="wb-pagebar__end">
        <ManageMenu />
      </div>
    </header>
  );
}
