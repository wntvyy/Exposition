// Keybinding parsing, display, and the workbench dispatcher for registry
// commands.
//
// Bindings are written `Mod+Shift+P`; `Mod` is Ctrl on Windows/Linux and Cmd on
// macOS. The dispatcher listens on `window` in the bubble phase, so focused
// components (Monaco, the page editor, dialogs) see keys first; an event they
// handled (`preventDefault`) is never re-dispatched. While focus is in a text
// input, textarea, contenteditable, or Monaco, only "global" bindings fire.
import { registry, type CommandContribution } from "./registry";

export type Platform = "mac" | "windows" | "linux";

export interface Chord {
  ctrl: boolean;
  meta: boolean;
  alt: boolean;
  shift: boolean;
  /** Normalized key: lowercase letter/digit/punctuation, `f1`..`f24`, or a lowercase named key. */
  key: string;
}

export function detectPlatform(): Platform {
  const nav = globalThis.navigator as (Navigator & { userAgentData?: { platform?: string } }) | undefined;
  const raw = `${nav?.userAgentData?.platform ?? ""} ${nav?.platform ?? ""} ${nav?.userAgent ?? ""}`.toLowerCase();
  if (raw.includes("mac")) return "mac";
  if (raw.includes("win")) return "windows";
  return "linux";
}

const NAMED_KEYS: Record<string, string> = {
  esc: "escape",
  escape: "escape",
  enter: "enter",
  return: "enter",
  tab: "tab",
  space: "space",
  backspace: "backspace",
  delete: "delete",
  del: "delete",
  insert: "insert",
  home: "home",
  end: "end",
  pageup: "pageup",
  pagedown: "pagedown",
  up: "arrowup",
  down: "arrowdown",
  left: "arrowleft",
  right: "arrowright",
  arrowup: "arrowup",
  arrowdown: "arrowdown",
  arrowleft: "arrowleft",
  arrowright: "arrowright",
  plus: "+",
  minus: "-",
};

function normalizeKeyName(name: string): string | null {
  const lower = name.toLowerCase();
  if (NAMED_KEYS[lower]) return NAMED_KEYS[lower];
  if (/^f([1-9]|1\d|2[0-4])$/.test(lower)) return lower;
  if ([...name].length === 1) return lower;
  return null;
}

/** Parse a binding such as `Mod+Shift+P`. Throws on malformed input. */
export function parseKeybinding(binding: string, platform: Platform = detectPlatform()): Chord {
  const parts = binding.split("+").map((p) => p.trim());
  // A trailing "+" key ("Mod++") splits into an empty last part.
  if (parts.length >= 2 && parts.at(-1) === "" && parts.at(-2) === "") parts.splice(-2, 2, "+");
  const chord: Chord = { ctrl: false, meta: false, alt: false, shift: false, key: "" };
  for (const [index, part] of parts.entries()) {
    const lower = part.toLowerCase();
    const isLast = index === parts.length - 1;
    if (!isLast) {
      if (lower === "mod") chord[platform === "mac" ? "meta" : "ctrl"] = true;
      else if (lower === "ctrl" || lower === "control") chord.ctrl = true;
      else if (lower === "cmd" || lower === "meta" || lower === "super") chord.meta = true;
      else if (lower === "alt" || lower === "option") chord.alt = true;
      else if (lower === "shift") chord.shift = true;
      else throw new Error(`invalid modifier "${part}" in keybinding "${binding}"`);
      continue;
    }
    const key = part === "" ? null : normalizeKeyName(part);
    if (!key) throw new Error(`invalid key in keybinding "${binding}"`);
    chord.key = key;
  }
  return chord;
}

/** Normalize a keyboard event. Physical letter/digit codes win when modifiers or the layout alter `key`. */
export function chordFromEvent(event: KeyboardEvent): Chord {
  let key: string;
  const letter = /^Key([A-Z])$/.exec(event.code);
  const digit = /^Digit(\d)$/.exec(event.code);
  if (/^[a-zA-Z]$/.test(event.key)) key = event.key.toLowerCase();
  else if (letter) key = letter[1]!.toLowerCase();
  else if (digit) key = digit[1]!;
  else if (event.key === " ") key = "space";
  else key = normalizeKeyName(event.key) ?? event.key.toLowerCase();
  return { ctrl: event.ctrlKey, meta: event.metaKey, alt: event.altKey, shift: event.shiftKey, key };
}

export function chordsEqual(a: Chord, b: Chord): boolean {
  return a.ctrl === b.ctrl && a.meta === b.meta && a.alt === b.alt && a.shift === b.shift && a.key === b.key;
}

const KEY_LABELS: Record<string, string> = {
  escape: "Esc",
  enter: "Enter",
  tab: "Tab",
  space: "Space",
  backspace: "Backspace",
  delete: "Delete",
  insert: "Insert",
  home: "Home",
  end: "End",
  pageup: "PageUp",
  pagedown: "PageDown",
  arrowup: "↑",
  arrowdown: "↓",
  arrowleft: "←",
  arrowright: "→",
};

/** Display labels for a binding, e.g. `["Ctrl", "Shift", "P"]` or `["⇧", "⌘", "P"]`. */
export function formatKeybinding(binding: string, platform: Platform = detectPlatform()): string[] {
  const chord = parseKeybinding(binding, platform);
  const key = KEY_LABELS[chord.key] ?? chord.key.toUpperCase();
  if (platform === "mac") {
    return [chord.ctrl && "⌃", chord.alt && "⌥", chord.shift && "⇧", chord.meta && "⌘", key].filter(
      (p): p is string => Boolean(p),
    );
  }
  return [chord.ctrl && "Ctrl", chord.meta && "Win", chord.alt && "Alt", chord.shift && "Shift", key].filter(
    (p): p is string => Boolean(p),
  );
}

const TEXT_INPUT_TYPES = new Set(["text", "search", "email", "url", "tel", "password", "number", ""]);

/** True when keyboard input at `target` edits text (inputs, textareas, contenteditable, Monaco). */
export function isEditableTarget(target: EventTarget | null): boolean {
  if (!(target instanceof Element)) return false;
  if (target.closest(".monaco-editor")) return true;
  if (target instanceof HTMLTextAreaElement) return !target.readOnly;
  if (target instanceof HTMLInputElement) return TEXT_INPUT_TYPES.has(target.type) && !target.readOnly;
  if (target instanceof HTMLElement && target.isContentEditable) return true;
  const editable = target.closest("[contenteditable]");
  return editable !== null && editable.getAttribute("contenteditable") !== "false";
}

const TEXT_EDITING_KEYS = new Set([
  "a",
  "c",
  "v",
  "x",
  "z",
  "y",
  "arrowleft",
  "arrowright",
  "arrowup",
  "arrowdown",
  "home",
  "end",
  "backspace",
  "delete",
]);

/** Chords that may fire while text has focus: Ctrl/Cmd/Alt or function keys, minus text-editing chords. */
export function isGlobalChord(chord: Chord): boolean {
  if (/^f\d+$/.test(chord.key)) return true;
  if (!(chord.ctrl || chord.meta || chord.alt)) return false;
  if ((chord.ctrl || chord.meta) && !chord.alt && TEXT_EDITING_KEYS.has(chord.key)) return false;
  return true;
}

function scopeOf(command: CommandContribution, chord: Chord): "global" | "workbench" {
  return command.keybindingScope ?? (isGlobalChord(chord) ? "global" : "workbench");
}

const parsed = new Map<string, Chord | null>();

function cachedChord(binding: string, platform: Platform): Chord | null {
  const cacheKey = `${platform}:${binding}`;
  if (!parsed.has(cacheKey)) {
    try {
      parsed.set(cacheKey, parseKeybinding(binding, platform));
    } catch (error) {
      console.warn(String(error));
      parsed.set(cacheKey, null);
    }
  }
  return parsed.get(cacheKey) ?? null;
}

/**
 * True when Ctrl+Alt (AltGr on Windows/Linux layouts) produced a character,
 * e.g. AltGr+B → "{" on a Hungarian layout. That is text input, not a chord.
 */
export function isAltGraphCharacter(event: KeyboardEvent): boolean {
  if (event.getModifierState?.("AltGraph")) return [...event.key].length === 1;
  if (!(event.ctrlKey && event.altKey) || event.metaKey || [...event.key].length !== 1) return false;
  const letter = /^Key([A-Z])$/.exec(event.code)?.[1]?.toLowerCase();
  const digit = /^Digit(\d)$/.exec(event.code)?.[1];
  return event.key.toLowerCase() !== (letter ?? digit ?? "");
}

/** The enabled command bound to this event, honoring editing scope; undefined when none applies. */
export function commandForEvent(event: KeyboardEvent, platform: Platform = detectPlatform()): CommandContribution | undefined {
  if (event.defaultPrevented || event.isComposing || event.keyCode === 229) return undefined;
  if (["Control", "Shift", "Alt", "Meta", "AltGraph"].includes(event.key)) return undefined;
  const editing = isEditableTarget(event.target);
  if (editing && isAltGraphCharacter(event)) return undefined;
  const chord = chordFromEvent(event);
  return registry.allCommands().find((command) => {
    if (!command.keybinding) return false;
    const bound = cachedChord(command.keybinding, platform);
    if (!bound || !chordsEqual(bound, chord)) return false;
    if (editing && scopeOf(command, bound) !== "global") return false;
    if (event.repeat && command.ignoreRepeat) return false;
    return command.enabled ? command.enabled() : true;
  });
}

/** Install the keydown dispatcher. Returns an uninstaller. */
export function installKeybindings(target: Window = window, platform: Platform = detectPlatform()): () => void {
  const onKeyDown = (event: KeyboardEvent) => {
    const command = commandForEvent(event, platform);
    if (!command) return;
    event.preventDefault();
    event.stopPropagation();
    void registry.runCommand(command.id).catch((error: unknown) => {
      console.error(`command ${command.id} failed`, error);
    });
  };
  target.addEventListener("keydown", onKeyDown);
  return () => target.removeEventListener("keydown", onKeyDown);
}
