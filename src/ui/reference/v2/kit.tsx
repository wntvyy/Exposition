// V2 review samples: the sample-local control kit. These are presentation
// proposals for the shared components (Button, StatusPill, StatusText, Badge,
// Banner, fields, tabs, menus), drawn only from `--color-*` roles and the
// direction's `--v2-*` geometry. Styling lives in v2.css under `.v2s`.
import { AlertTriangle, CheckCircle2, Circle, Folder, FolderOpen, Info, Loader2, XCircle, type LucideIcon } from "lucide-react";
import type { ReactNode } from "react";

import { FileIcon } from "../../components/FileIcon";
import { iconUrl, resolveFileIcon, type NodeKind, type ThemeKind } from "./fileIcons";

export { iconUrl };

export type Tone = "neutral" | "accent" | "success" | "warning" | "danger" | "info" | "stale";
/** Forced interaction state for palette sheets (otherwise real :hover/:active/:focus apply). */
export type ForcedState = "rest" | "hover" | "pressed" | "selected" | "focused" | "disabled";

const TONE_ICON: Record<Tone, LucideIcon> = {
  neutral: Circle,
  accent: Circle,
  success: CheckCircle2,
  warning: AlertTriangle,
  danger: XCircle,
  info: Info,
  stale: Loader2,
};

export interface BtnProps {
  children?: ReactNode;
  variant?: "primary" | "secondary" | "ghost" | "danger";
  size?: "md" | "sm";
  icon?: LucideIcon;
  disabled?: boolean;
  state?: ForcedState;
  onClick?: () => void;
  expanded?: boolean;
  label?: string;
  testId?: string;
}

export function Btn({ children, variant = "secondary", size = "md", icon: Icon, disabled, state, onClick, expanded, label, testId }: BtnProps) {
  return (
    <button
      type="button"
      className="v2-btn"
      data-variant={variant}
      data-size={size}
      data-state={state}
      data-icon-only={children ? undefined : "true"}
      disabled={disabled || state === "disabled"}
      aria-expanded={expanded}
      aria-label={label}
      title={children ? undefined : label}
      onClick={onClick}
      data-testid={testId}
    >
      {Icon && <Icon size={size === "sm" ? 14 : 16} aria-hidden />}
      {children}
    </button>
  );
}

export function StatusPill({ tone, children, icon }: { tone: Tone; children: ReactNode; icon?: LucideIcon }) {
  const Icon = icon ?? TONE_ICON[tone];
  return (
    <span className="v2-pill" data-tone={tone}>
      <Icon size={12} aria-hidden />
      {children}
    </span>
  );
}

export function StatusText({ tone, children, icon }: { tone: Tone; children: ReactNode; icon?: LucideIcon }) {
  const Icon = icon ?? TONE_ICON[tone];
  return (
    <span className="v2-status-text" data-tone={tone}>
      <Icon size={14} aria-hidden />
      <span>{children}</span>
    </span>
  );
}

export function Badge({ tone = "neutral", dashed, children }: { tone?: Tone; dashed?: boolean; children: ReactNode }) {
  return (
    <span className="v2-badge" data-tone={tone} data-dashed={dashed ? "true" : undefined}>
      {children}
    </span>
  );
}

export function Banner({ tone, title, children, testId }: { tone: Tone; title?: ReactNode; children?: ReactNode; testId?: string }) {
  const Icon = TONE_ICON[tone];
  return (
    <div className="v2-banner" data-tone={tone} role={tone === "danger" ? "alert" : undefined} data-testid={testId}>
      <Icon size={16} aria-hidden className="v2-banner__icon" />
      <div className="v2-banner__body">
        {title && <div className="v2-banner__title">{title}</div>}
        {children && <div className="v2-banner__text">{children}</div>}
      </div>
    </div>
  );
}

export function ChromeLabel({ children }: { children: ReactNode }) {
  return <span className="v2-chrome-label">{children}</span>;
}

export function Kbd({ children }: { children: ReactNode }) {
  return <kbd className="v2-kbd">{children}</kbd>;
}

/** A file or folder icon resolved with the sample resolver. Decorative: the name is next to it. */
export function FileGlyph({
  path,
  kind = "file",
  theme,
  size = 14,
  expanded,
}: {
  path: string;
  kind?: NodeKind;
  theme: ThemeKind;
  size?: 14 | 16;
  expanded?: boolean;
}) {
  const r = resolveFileIcon(path, kind, theme);
  if (r.rule === "folder") {
    const F = expanded ? FolderOpen : Folder;
    return <F size={size} aria-hidden className="v2-glyph v2-glyph--folder" />;
  }
  if (r.rule === "existing-glyph" || r.rule === "generic") {
    return (
      <span className="v2-glyph v2-glyph--legacy" style={{ width: size, height: size }} aria-hidden>
        <FileIcon name={path} size={size} />
      </span>
    );
  }
  return (
    <img
      className="v2-glyph"
      src={iconUrl(r.asset!)}
      width={size}
      height={size}
      alt=""
      aria-hidden
      draggable={false}
      data-icon={r.asset}
      data-rule={r.rule}
    />
  );
}
