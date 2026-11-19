// A miniature of a theme for the Appearance choice: a sidebar and an editor
// drawn with that theme's own token values (a preview of a theme other than
// the active one cannot use the live custom properties). Decorative.
import { palettes, type ThemeName } from "../tokens";

function Mini({ theme }: { theme: ThemeName }) {
  const c = palettes[theme];
  return (
    <span className="ui-theme-preview__frame" style={{ background: c["surface-base"], borderColor: c.border }}>
      <span className="ui-theme-preview__pane" style={{ background: c["surface-sunken"] }}>
        <i style={{ background: c["text-muted"] }} />
        <i style={{ background: c["text-subtle"], width: "60%" }} />
        <i style={{ background: c.accent, width: "45%" }} />
      </span>
      <span className="ui-theme-preview__pane" style={{ background: c.surface }}>
        <i style={{ background: c.text }} />
        <i style={{ background: c["text-muted"], width: "80%" }} />
        <i style={{ background: c["info-text"], width: "55%" }} />
        <i style={{ background: c["success-text"], width: "35%" }} />
      </span>
    </span>
  );
}

/** One theme, or "system": Light and Dark side by side. */
export function ThemePreview({ theme }: { theme: ThemeName | "system" }) {
  if (theme !== "system") return <Mini theme={theme} />;
  return (
    <span className="ui-theme-preview__split">
      <Mini theme="light" />
      <Mini theme="dark" />
    </span>
  );
}
