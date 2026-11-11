import { Search } from "lucide-react";
import { Fragment, useMemo, useState } from "react";
import {
  Autocomplete,
  Dialog,
  Input,
  Keyboard,
  Menu,
  MenuItem,
  Modal,
  ModalOverlay,
  Text,
  TextField,
} from "react-aria-components";
import { create } from "zustand";

import { Keybinding } from "../../ui/components/Kbd";
import { rankByFuzzy } from "../fuzzy";
import { registry, type CommandContribution } from "../registry";
import { useRegistryVersion } from "./useRegistry";

export const SHOW_COMMANDS_ID = "workbench.action.showCommands";

const usePalette = create<{ open: boolean }>(() => ({ open: false }));
/** Most recently run commands first (session only). */
const recent: string[] = [];

export const commandPalette = {
  open: () => usePalette.setState({ open: true }),
  close: () => usePalette.setState({ open: false }),
  toggle: () => usePalette.setState((s) => ({ open: !s.open })),
  isOpen: () => usePalette.getState().open,
};

export function commandLabel(command: CommandContribution): string {
  return command.category ? `${command.category}: ${command.title}` : command.title;
}

function Highlighted({ text, positions }: { text: string; positions: number[] }) {
  if (!positions.length) return <>{text}</>;
  const set = new Set(positions);
  const parts: { text: string; hit: boolean }[] = [];
  for (let i = 0; i < text.length; i++) {
    const hit = set.has(i);
    const last = parts.at(-1);
    if (last && last.hit === hit) last.text += text[i];
    else parts.push({ text: text[i]!, hit });
  }
  return (
    <>
      {parts.map((p, i) => (p.hit ? <mark key={i}>{p.text}</mark> : <Fragment key={i}>{p.text}</Fragment>))}
    </>
  );
}

function PaletteContent() {
  const version = useRegistryVersion();
  const [query, setQuery] = useState("");
  const ranked = useMemo(() => {
    void version;
    const available = registry
      .allCommands()
      .filter((c) => c.id !== SHOW_COMMANDS_ID && (c.enabled ? c.enabled() : true));
    const ordered = query
      ? available
      : [...available].sort((a, b) => {
          const ra = recent.indexOf(a.id);
          const rb = recent.indexOf(b.id);
          return (ra < 0 ? Infinity : ra) - (rb < 0 ? Infinity : rb);
        });
    return rankByFuzzy(query, ordered, commandLabel);
  }, [query, version]);

  const run = (id: string) => {
    commandPalette.close();
    recent.splice(0, recent.length, id, ...recent.filter((r) => r !== id).slice(0, 19));
    // Let the dialog close and restore focus before the command moves focus itself.
    setTimeout(() => {
      void registry.runCommand(id).catch((error: unknown) => console.error(`command ${id} failed`, error));
    }, 0);
  };

  return (
    <Dialog className="wb-palette__dialog" aria-label="Command palette">
      <Autocomplete inputValue={query} onInputChange={setQuery}>
        <TextField aria-label="Command" className="wb-palette__field" autoFocus>
          <Search size={16} aria-hidden className="wb-palette__icon" />
          <Input className="wb-palette__input" placeholder="Type the name of a command to run" spellCheck={false} />
        </TextField>
        <Menu
          className="wb-palette__list"
          aria-label="Commands"
          items={ranked.map((r) => ({ ...r, id: r.item.id }))}
          onAction={(key) => run(String(key))}
          renderEmptyState={() => <div className="wb-palette__empty">No matching commands</div>}
        >
          {(entry) => {
            const label = commandLabel(entry.item);
            const split = entry.item.category ? entry.item.category.length + 2 : 0;
            return (
              <MenuItem id={entry.item.id} textValue={label} className="wb-palette__item">
                <Text slot="label" className="wb-palette__label">
                  {split > 0 && (
                    <>
                      <span className="wb-palette__category">
                        <Highlighted
                          text={label.slice(0, split - 2)}
                          positions={entry.positions.filter((p) => p < split - 2)}
                        />
                        :
                      </span>{" "}
                    </>
                  )}
                  <Highlighted
                    text={label.slice(split)}
                    positions={entry.positions.filter((p) => p >= split).map((p) => p - split)}
                  />
                </Text>
                {entry.item.keybinding && (
                  <Keyboard className="wb-palette__keys">
                    <Keybinding binding={entry.item.keybinding} />
                  </Keyboard>
                )}
              </MenuItem>
            );
          }}
        </Menu>
      </Autocomplete>
    </Dialog>
  );
}

/** Command palette (Mod+Shift+P): fuzzy-filter enabled registry commands, Enter runs, Escape closes. */
export function CommandPalette() {
  const open = usePalette((s) => s.open);
  return (
    <ModalOverlay
      className="ui-modal-overlay wb-palette-overlay"
      isOpen={open}
      isDismissable
      onOpenChange={(next) => usePalette.setState({ open: next })}
    >
      <Modal className="ui-modal wb-palette">
        <PaletteContent />
      </Modal>
    </ModalOverlay>
  );
}
