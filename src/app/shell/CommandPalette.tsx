import { Search } from "lucide-react";
import { Fragment, useMemo, useState, type KeyboardEvent } from "react";
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
  /** Observe opening and closing (e.g. to remember which surface had focus before). */
  subscribe: (listener: (open: boolean) => void): (() => void) =>
    usePalette.subscribe((state, previous) => {
      if (state.open !== previous.open) listener(state.open);
    }),
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

/** Enabled commands ranked for `query` (recently run first when it is empty). */
function rankCommands(query: string) {
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
}

function PaletteContent() {
  const version = useRegistryVersion();
  const [query, setQuery] = useState("");
  const ranked = useMemo(() => {
    void version;
    return rankCommands(query);
  }, [query, version]);

  const run = (id: string) => {
    commandPalette.close();
    recent.splice(0, recent.length, id, ...recent.filter((r) => r !== id).slice(0, 19));
    // Let the dialog close and restore focus before the command moves focus itself.
    setTimeout(() => {
      void registry.runCommand(id).catch((error: unknown) => console.error(`command ${id} failed`, error));
    }, 0);
  };

  // Enter runs the highlighted command, else the top match for the text in the
  // field now. react-aria's Menu runs only an item holding its virtual focus,
  // which it moves to the first match only after forward typing: after
  // Backspace, paste, undo, or an edit it saw no typing event for, nothing is
  // highlighted and Enter did nothing, leaving the palette open.
  const onKeyDownCapture = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key !== "Enter" || event.nativeEvent.isComposing) return;
    if (event.altKey || event.ctrlKey || event.metaKey || event.shiftKey) return;
    if (!(event.target instanceof HTMLInputElement)) return;
    const text = event.target.value;
    // The list (and its highlight) may not show `text` yet if rendering lags.
    const current = text === query ? ranked : rankCommands(text);
    const highlighted =
      text === query
        ? event.currentTarget.querySelector<HTMLElement>(".wb-palette__item[data-focused]")?.dataset.command
        : undefined;
    const id = highlighted ?? (text.trim() ? current[0]?.item.id : undefined);
    if (!id) return;
    event.preventDefault();
    event.stopPropagation();
    run(id);
  };

  return (
    <Dialog className="wb-palette__dialog" aria-label="Command palette">
      <div className="wb-palette__body" onKeyDownCapture={onKeyDownCapture}>
        <Autocomplete inputValue={query} onInputChange={setQuery}>
          <TextField aria-label="Command" className="wb-palette__field" autoFocus>
            <Search size={16} aria-hidden className="wb-palette__icon" />
            <Input className="wb-palette__input" placeholder="Type the name of a command to run" spellCheck={false} />
          </TextField>
          <Menu
            className="wb-palette__list"
            aria-label="Commands"
            data-query={query.trim() ? "" : undefined}
            items={ranked.map((r) => ({ ...r, id: r.item.id }))}
            onAction={(key) => run(String(key))}
            renderEmptyState={() => <div className="wb-palette__empty">No matching commands</div>}
          >
            {(entry) => {
              const label = commandLabel(entry.item);
              const split = entry.item.category ? entry.item.category.length + 2 : 0;
              return (
                <MenuItem
                  id={entry.item.id}
                  textValue={label}
                  className="wb-palette__item"
                  data-command={entry.item.id}
                >
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
      </div>
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
