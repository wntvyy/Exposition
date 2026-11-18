// V2 review samples: Settings (Appearance and Model providers). Mock state
// only: nothing reads or writes a credential, a setting, or the app theme.
import {
  ChevronDown,
  ChevronRight,
  Cpu,
  FolderCog,
  HardDrive,
  Info,
  KeyRound,
  Palette,
  Pencil,
  RefreshCw,
  ShieldCheck,
  Bot,
  FlaskConical,
  XCircle,
  type LucideIcon,
} from "lucide-react";
import { useState, type ReactNode } from "react";

import { Btn, StatusText } from "./kit";
import { SETTINGS_NAV, type SettingsPage } from "./mock";
import { PALETTE_IDS, V2_PALETTES, type PaletteId } from "./palettes";

const NAV_ICONS: Record<SettingsPage, LucideIcon> = {
  Appearance: Palette,
  Editor: Pencil,
  Project: FolderCog,
  "Trusted folders": ShieldCheck,
  Execution: Cpu,
  "Model providers": Bot,
  Research: FlaskConical,
  "Storage & History": HardDrive,
  About: Info,
};

export interface SettingsState {
  page: SettingsPage;
  /** Expand OpenAI's Advanced details initially. */
  advanced?: boolean;
  /** Focus the OpenAI key field initially. */
  focusKey?: boolean;
}

export function SettingsView({ initial }: { initial: SettingsState }) {
  const [page, setPage] = useState<SettingsPage>(initial.page);
  return (
    <div className="v2-settings">
      <nav className="v2-settings__nav" aria-label="Settings sections">
        {SETTINGS_NAV.map((p) => {
          const Icon = NAV_ICONS[p];
          return (
            <button key={p} type="button" className="v2-settings__nav-item" aria-current={p === page ? "page" : undefined} onClick={() => setPage(p)}>
              <Icon size={14} aria-hidden />
              {p}
            </button>
          );
        })}
      </nav>
      <div className="v2-settings__scroll">
        <div className="v2-settings__page">
          {page === "Appearance" ? (
            <Appearance />
          ) : page === "Model providers" ? (
            <Providers advanced={initial.advanced ?? false} focusKey={initial.focusKey ?? false} />
          ) : (
            <>
              <h1 className="v2-page-title">{page}</h1>
              <p className="v2-meta">Not part of this sample.</p>
            </>
          )}
        </div>
      </div>
    </div>
  );
}

function Row({ label, htmlFor, help, children, testId }: { label: ReactNode; htmlFor?: string; help?: ReactNode; children: ReactNode; testId?: string }) {
  return (
    <div className="v2-setting" data-testid={testId}>
      <div className="v2-setting__label">{htmlFor ? <label htmlFor={htmlFor}>{label}</label> : label}</div>
      {help && <div className="v2-setting__help">{help}</div>}
      <div className="v2-setting__control">{children}</div>
    </div>
  );
}

function Group({ title, status, children }: { title: string; status?: ReactNode; children: ReactNode }) {
  return (
    <section className="v2-settings-group" aria-label={title}>
      <div className="v2-settings-group__head">
        <h2 className="v2-section-title">{title}</h2>
        <span className="v2-spacer" />
        {status}
      </div>
      <div className="v2-settings-group__body">{children}</div>
    </section>
  );
}

function Appearance() {
  const [theme, setTheme] = useState<PaletteId>("light");
  return (
    <>
      <h1 className="v2-page-title">Appearance</h1>
      <Group title="Theme">
        <div className="v2-theme-choice" role="radiogroup" aria-label="Color theme">
          {PALETTE_IDS.map((id) => {
            const p = V2_PALETTES[id];
            const c = p.colors;
            return (
              <label key={id} className="v2-theme-option" data-checked={theme === id ? "true" : undefined}>
                <input type="radio" name="v2-theme" value={id} checked={theme === id} onChange={() => setTheme(id)} />
                <span className="v2-theme-option__preview" aria-hidden style={{ background: c["surface-base"], borderColor: c.border }}>
                  <span style={{ background: c["surface-sunken"] }}>
                    <i style={{ background: c["text-muted"] }} />
                    <i style={{ background: c["text-subtle"], width: "60%" }} />
                    <i style={{ background: c.accent, width: "45%" }} />
                  </span>
                  <span style={{ background: c.surface }}>
                    <i style={{ background: c.text }} />
                    <i style={{ background: c["text-muted"], width: "80%" }} />
                    <i style={{ background: c["info-text"], width: "55%" }} />
                    <i style={{ background: c["success-text"], width: "35%" }} />
                  </span>
                </span>
                <span className="v2-theme-option__name">{p.name}</span>
              </label>
            );
          })}
        </div>
        <p className="v2-setting__help v2-theme-note">Applies immediately. Open documents, drafts, and graph positions stay as they are.</p>
      </Group>
    </>
  );
}

function Providers({ advanced, focusKey }: { advanced: boolean; focusKey: boolean }) {
  const [open, setOpen] = useState(advanced);
  const [key, setKey] = useState("sk-proj-7Hq2 3f9Q");
  const [error, setError] = useState(true);
  return (
    <>
      <h1 className="v2-page-title">Model providers</h1>
      <p className="v2-page-lead">
        API keys are kept in Windows Credential Manager. Exposition never shows them again, writes them to project files or logs, or passes them to commands it runs.
      </p>

      <Group title="Anthropic" status={<StatusText tone="success">Connected</StatusText>}>
        <Row label="API key">
          <div className="v2-row">
            <span className="v2-value">Configured — stored in Windows Credential Manager</span>
            <span className="v2-spacer" />
            <Btn size="sm" icon={KeyRound}>
              Replace key…
            </Btn>
            <Btn size="sm" variant="ghost">
              Remove key…
            </Btn>
          </div>
        </Row>
        <Row label="Model list">
          <div className="v2-row">
            <span className="v2-value">12 models · refreshed today at 09:14</span>
            <span className="v2-spacer" />
            <Btn size="sm" variant="ghost" icon={RefreshCw}>
              Refresh
            </Btn>
          </div>
        </Row>
        <Row label="Billing">
          <span className="v2-value">Requests use this API key and are billed to its Anthropic account.</span>
        </Row>
      </Group>

      <Group title="OpenAI" status={<StatusText tone="neutral">Not connected</StatusText>}>
        <Row label="API key" htmlFor="v2-openai-key" testId="v2-openai-key-row">
          <div className="v2-row v2-row--field">
            <input
              id="v2-openai-key"
              className="v2-input v2-input--code"
              type="password"
              value={key}
              onChange={(e) => {
                setKey(e.target.value);
                setError(false);
              }}
              aria-invalid={error}
              aria-describedby={error ? "v2-openai-key-error" : undefined}
              autoFocus={focusKey}
              placeholder="Paste your OpenAI API key"
            />
            <Btn size="md" icon={KeyRound} onClick={() => setError(true)} disabled={!key}>
              Save key
            </Btn>
          </div>
          {error && (
            <p className="v2-field-error" id="v2-openai-key-error" role="alert" data-testid="v2-key-error">
              <XCircle size={14} aria-hidden />
              <span>
                Not saved: OpenAI rejected the key (401 invalid_api_key). Check that you copied the whole key, then save again.
              </span>
            </p>
          )}
        </Row>
        <Row label="Billing">
          <span className="v2-value">Requests use this API key and are billed to its OpenAI API account (organization and project, when set).</span>
        </Row>
        <div className="v2-details" data-open={open ? "true" : undefined}>
          <button type="button" className="v2-details__summary" aria-expanded={open} aria-controls="v2-advanced-body" onClick={() => setOpen((v) => !v)} data-testid="v2-advanced">
            {open ? <ChevronDown size={14} aria-hidden /> : <ChevronRight size={14} aria-hidden />}
            <span>Advanced</span>
            <span className="v2-meta">Organization and project: not set</span>
          </button>
          {/* Stays mounted while collapsed, so entered values and validation are kept. */}
          <div className="v2-details__body" hidden={!open} id="v2-advanced-body">
            <Row label="Organization ID" htmlFor="v2-org" help="Optional. Requests are billed to this organization.">
              <input id="v2-org" className="v2-input v2-input--code" placeholder="org-…" />
            </Row>
            <Row label="Project ID" htmlFor="v2-proj" help="Optional. Requests are billed to this project.">
              <input id="v2-proj" className="v2-input v2-input--code" placeholder="proj_…" />
            </Row>
            <div className="v2-setting__actions">
              <Btn size="sm" disabled>
                Save organization and project
              </Btn>
            </div>
          </div>
        </div>
      </Group>

      <Group title="Defaults for new tasks">
        <Row label="Model and effort" help="A task or a message can choose another model without changing this.">
          <Btn icon={Bot}>
            claude-opus-5-5 · High
            <ChevronDown size={14} aria-hidden />
          </Btn>
        </Row>
      </Group>
    </>
  );
}
