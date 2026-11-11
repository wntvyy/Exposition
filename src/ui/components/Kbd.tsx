import { formatKeybinding, type Platform } from "../../app/keybindings";
import { cx } from "./cx";

/** A single key cap. */
export function Kbd({ children, className }: { children: string; className?: string }) {
  return <kbd className={cx("ui-kbd", className)}>{children}</kbd>;
}

export interface KeybindingProps {
  /** Binding in `Mod+Shift+P` form. */
  binding: string;
  platform?: Platform;
  className?: string;
}

/** A platform-formatted keybinding: `Ctrl` `Shift` `P` (Windows/Linux) or `⇧⌘P` (macOS). */
export function Keybinding({ binding, platform, className }: KeybindingProps) {
  let parts: string[];
  try {
    parts = formatKeybinding(binding, platform);
  } catch {
    return null;
  }
  const label = parts.join("+");
  return (
    <span className={cx("ui-keybinding", className)} aria-label={label}>
      {parts.map((part, i) => (
        <kbd key={i} className="ui-kbd" aria-hidden>
          {part}
        </kbd>
      ))}
    </span>
  );
}

/** Plain-text keybinding label, e.g. for tooltips and aria-keyshortcuts-like hints. */
export function keybindingLabel(binding: string, platform?: Platform): string {
  try {
    return formatKeybinding(binding, platform).join("+");
  } catch {
    return binding;
  }
}
