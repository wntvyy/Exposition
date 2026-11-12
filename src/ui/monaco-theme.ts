// Monaco themes and editor defaults derived from the design tokens.
//
// `src/lib/monaco.ts` defines both themes and keeps Monaco on the app theme
// (Monaco's theme is global), so feature editors never call `setTheme`
// themselves; they spread `editorDefaults` into their construction options.
import type * as Monaco from "monaco-editor/editor";

import { parseColor } from "./color";
import { useTheme } from "./theme";
import { fonts, palettes, type ColorRole, type ThemeName } from "./tokens";

export const MONACO_THEMES: Record<ThemeName, string> = {
  light: "exposition-light",
  dark: "exposition-dark",
};

/** Monaco accepts `#rrggbb` / `#rrggbbaa` only. */
function hex(theme: ThemeName, role: ColorRole, alpha?: number): string {
  const { r, g, b, a } = parseColor(palettes[theme][role]);
  const to2 = (n: number) => Math.round(n).toString(16).padStart(2, "0");
  const finalAlpha = alpha ?? a;
  return `#${to2(r)}${to2(g)}${to2(b)}${finalAlpha < 1 ? to2(finalAlpha * 255) : ""}`;
}

/** Token color without the leading `#` (Monaco rule format). */
function fg(theme: ThemeName, role: ColorRole): string {
  return hex(theme, role).slice(1, 7);
}

export function monacoThemeData(theme: ThemeName): Monaco.editor.IStandaloneThemeData {
  const c = (role: ColorRole, alpha?: number) => hex(theme, role, alpha);
  return {
    base: theme === "dark" ? "vs-dark" : "vs",
    inherit: true,
    rules: [
      { token: "", foreground: fg(theme, "text") },
      { token: "comment", foreground: fg(theme, "text-subtle"), fontStyle: "italic" },
      { token: "keyword", foreground: fg(theme, "accent-text") },
      { token: "string", foreground: fg(theme, "success-text") },
      { token: "number", foreground: fg(theme, "warning-text") },
      { token: "type", foreground: fg(theme, "status-stale-text") },
      { token: "tag", foreground: fg(theme, "accent-text") },
      { token: "attribute.name", foreground: fg(theme, "status-stale-text") },
      { token: "delimiter", foreground: fg(theme, "text-muted") },
      { token: "operator", foreground: fg(theme, "text-muted") },
      { token: "invalid", foreground: fg(theme, "danger-text") },
    ],
    colors: {
      foreground: c("text"),
      focusBorder: c("focus-ring"),
      "editor.background": c("surface"),
      "editor.foreground": c("text"),
      "editorGutter.background": c("surface"),
      "editorLineNumber.foreground": c("text-subtle"),
      "editorLineNumber.activeForeground": c("text"),
      "editor.lineHighlightBackground": c("surface-sunken"),
      "editor.lineHighlightBorder": c("surface-sunken"),
      "editor.selectionBackground": c("selection"),
      "editor.inactiveSelectionBackground": c("selection-inactive"),
      "editor.selectionHighlightBackground": c("selection", 0.5),
      "editor.wordHighlightBackground": c("selection", 0.45),
      "editor.findMatchBackground": c("warning-border"),
      "editor.findMatchHighlightBackground": c("warning-bg"),
      "editorCursor.foreground": c("text"),
      "editorWhitespace.foreground": c("border"),
      "editorIndentGuide.background1": c("border-subtle"),
      "editorIndentGuide.activeBackground1": c("border"),
      "editorBracketMatch.background": c("accent-subtle"),
      "editorBracketMatch.border": c("border-strong"),
      "editorError.foreground": c("danger-text"),
      "editorWarning.foreground": c("warning-text"),
      "editorInfo.foreground": c("info-text"),
      "editorWidget.background": c("surface-raised"),
      "editorWidget.foreground": c("text"),
      "editorWidget.border": c("border"),
      "editorHoverWidget.background": c("surface-raised"),
      "editorHoverWidget.border": c("border"),
      "editorSuggestWidget.background": c("surface-raised"),
      "editorSuggestWidget.border": c("border"),
      "editorSuggestWidget.selectedBackground": c("accent-subtle"),
      "list.activeSelectionBackground": c("accent-subtle"),
      "list.activeSelectionForeground": c("text"),
      "list.hoverBackground": c("surface-hover"),
      "list.focusOutline": c("focus-ring"),
      "input.background": c("field"),
      "input.foreground": c("text"),
      "input.border": c("border-strong"),
      "inputOption.activeBorder": c("accent"),
      "scrollbarSlider.background": c("scrollbar"),
      "scrollbarSlider.hoverBackground": c("scrollbar-hover"),
      "scrollbarSlider.activeBackground": c("scrollbar-hover"),
      "widget.shadow": c("shadow"),
      "diffEditor.insertedTextBackground": c("success-bg"),
      "diffEditor.removedTextBackground": c("danger-bg"),
      "diffEditor.insertedLineBackground": c("success-bg", 0.6),
      "diffEditor.removedLineBackground": c("danger-bg", 0.6),
    },
  };
}

/** Construction options every Monaco editor in Exposition starts from. */
export const editorDefaults: Monaco.editor.IStandaloneEditorConstructionOptions = {
  fontFamily: fonts.code,
  fontSize: fonts.codeSize,
  lineHeight: fonts.codeLineHeight,
  fontLigatures: true,
  automaticLayout: true,
  minimap: { enabled: false },
  scrollBeyondLastLine: false,
  renderLineHighlight: "line",
  renderLineHighlightOnlyWhenFocus: false,
  smoothScrolling: false,
  cursorBlinking: "smooth",
  padding: { top: 8, bottom: 8 },
  scrollbar: { verticalScrollbarSize: 10, horizontalScrollbarSize: 10, useShadows: false },
  fixedOverflowWidgets: true,
  accessibilitySupport: "auto",
  wordWrap: "off",
  // Completions only from a language's trigger characters (Lean's `\`
  // abbreviations) or Ctrl+Space: no word-based list popping up while typing
  // prose, where Enter would accept a suggestion instead of a new line.
  quickSuggestions: false,
  wordBasedSuggestions: "off",
};

type MonacoApi = typeof Monaco;

/** Define both themes. Idempotent. */
export function defineMonacoThemes(monaco: MonacoApi): void {
  monaco.editor.defineTheme(MONACO_THEMES.light, monacoThemeData("light"));
  monaco.editor.defineTheme(MONACO_THEMES.dark, monacoThemeData("dark"));
}

/** Set Monaco's (global) theme to the app's current theme. */
export function applyMonacoTheme(monaco: MonacoApi): void {
  monaco.editor.setTheme(MONACO_THEMES[useTheme.getState().resolved]);
}

/** Keep Monaco on the app theme. Returns an unsubscribe function. */
export function followAppTheme(monaco: MonacoApi): () => void {
  applyMonacoTheme(monaco);
  return useTheme.subscribe((state, previous) => {
    if (state.resolved !== previous.resolved) applyMonacoTheme(monaco);
  });
}
