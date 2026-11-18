// V2 UI standard, phase 1: isolated review samples. Three directions (Compact
// Calm, Balanced Workspace, Reading Ease) render the same Editor, Settings, and
// compact Agent content in one base palette (the proposed Light); four palette
// sheets and the file-icon sample are separate. Everything is scoped under
// `.v2s` with local custom properties: no production view, token, or user
// setting changes, and all data is mock (no IPC, credentials, writes, or graph).
import "./v2.css";

import { useState } from "react";

import { AgentPanel } from "./AgentPanel";
import { DIRECTION_IDS, DIRECTIONS, geometryStyle, type DirectionId } from "./directions";
import { IconSample } from "./IconSample";
import type { SettingsPage } from "./mock";
import { PALETTE_IDS, paletteStyle, V2_PALETTES, type PaletteId } from "./palettes";
import { PaletteSheet } from "./PaletteSheet";
import { SettingsView } from "./SettingsView";
import { Workbench } from "./Workbench";

export type SampleId = DirectionId | "palette" | "icons";
export type SampleView = "editor" | "settings" | "agent";

export interface SampleParams {
  sample: SampleId;
  view: SampleView;
  palette: PaletteId;
  /** Agent panel width in the Agent view. */
  width: 400 | 360;
  /** Open the Files context menu on a row. */
  menu: boolean;
  tab: string;
  settingsPage: "providers" | "appearance";
  advanced: boolean;
  focusKey: boolean;
  scroll: "question" | null;
  /** Show the review bar (the harness hides it for screenshots). */
  chrome: boolean;
}

export const DEFAULT_PARAMS: SampleParams = {
  sample: "balanced",
  view: "editor",
  palette: "light",
  width: 400,
  menu: false,
  tab: "results",
  settingsPage: "providers",
  advanced: false,
  focusKey: false,
  scroll: null,
  chrome: true,
};

const SAMPLES: SampleId[] = [...DIRECTION_IDS, "palette", "icons"];
const VIEWS: SampleView[] = ["editor", "settings", "agent"];

export function parseSampleParams(search: string): SampleParams {
  const q = new URLSearchParams(search);
  const pick = <T extends string>(key: string, allowed: readonly T[], fallback: T): T => {
    const v = q.get(key);
    return (allowed as readonly string[]).includes(v ?? "") ? (v as T) : fallback;
  };
  return {
    sample: pick("s", SAMPLES, DEFAULT_PARAMS.sample),
    view: pick("view", VIEWS, DEFAULT_PARAMS.view),
    palette: pick("palette", PALETTE_IDS, DEFAULT_PARAMS.palette),
    width: q.get("w") === "360" ? 360 : 400,
    menu: q.get("menu") === "files",
    tab: pick("tab", ["results", "appendix", "fit", "lean"], "results"),
    settingsPage: q.get("page") === "appearance" ? "appearance" : "providers",
    advanced: q.get("advanced") === "1",
    focusKey: q.get("focus") === "key",
    scroll: q.get("scroll") === "question" ? "question" : null,
    chrome: q.get("chrome") !== "0",
  };
}

export function sampleSearch(p: SampleParams): string {
  const q = new URLSearchParams({ s: p.sample });
  if (p.sample === "palette" || p.sample === "icons" || p.palette !== "light") q.set("palette", p.palette);
  if (p.sample !== "palette" && p.sample !== "icons") q.set("view", p.view);
  if (p.width === 360) q.set("w", "360");
  if (p.menu) q.set("menu", "files");
  if (p.tab !== "results") q.set("tab", p.tab);
  if (p.settingsPage === "appearance") q.set("page", "appearance");
  if (p.advanced) q.set("advanced", "1");
  if (p.focusKey) q.set("focus", "key");
  if (p.scroll) q.set("scroll", p.scroll);
  if (!p.chrome) q.set("chrome", "0");
  return `?${q.toString()}`;
}

const SAMPLE_LABEL: Record<SampleId, string> = {
  compact: "Compact Calm",
  balanced: "Balanced Workspace",
  reading: "Reading Ease",
  palette: "Palette sheets",
  icons: "File icons",
};

function Segmented<T extends string | number>({ label, value, options, onChange }: { label: string; value: T; options: [T, string][]; onChange: (v: T) => void }) {
  return (
    <div className="v2s-bar__group" role="radiogroup" aria-label={label}>
      <span className="v2s-bar__label">{label}</span>
      {options.map(([id, text]) => (
        <button key={String(id)} type="button" role="radio" aria-checked={id === value} className="v2s-bar__opt" onClick={() => onChange(id)}>
          {text}
        </button>
      ))}
    </div>
  );
}

export function V2Samples({ initial = DEFAULT_PARAMS, onChange }: { initial?: SampleParams; onChange?: (p: SampleParams) => void }) {
  const [params, setParams] = useState<SampleParams>(initial);
  const update = (next: Partial<SampleParams>) => {
    // Changing what is shown resets the one-shot initial states (menu, focus, scroll).
    const merged: SampleParams = { ...params, menu: false, focusKey: false, scroll: null, advanced: false, ...next };
    setParams(merged);
    onChange?.(merged);
  };
  const isDirection = params.sample !== "palette" && params.sample !== "icons";
  const palette = V2_PALETTES[params.palette];
  const key = JSON.stringify(params);

  return (
    <div className="v2s" data-testid="v2-samples">
      {params.chrome && (
        <div className="v2s-bar" role="region" aria-label="Sample controls">
          <div className="v2s-bar__row">
            <span className="v2s-bar__title">V2 samples (review)</span>
            <Segmented label="Sample" value={params.sample} options={SAMPLES.map((s) => [s, SAMPLE_LABEL[s]])} onChange={(sample) => update({ sample })} />
          </div>
          <div className="v2s-bar__row">
            {isDirection && (
              <>
                <Segmented
                  label="View"
                  value={params.view}
                  options={[
                    ["editor", "Editor"],
                    ["settings", "Settings"],
                    ["agent", "Agent"],
                  ]}
                  onChange={(view) => update({ view })}
                />
                {params.view === "agent" && (
                  <Segmented
                    label="Panel"
                    value={params.width}
                    options={[
                      [400, "400 px"],
                      [360, "360 px"],
                    ]}
                    onChange={(width) => update({ width })}
                  />
                )}
                {params.view === "settings" && (
                  <Segmented
                    label="Page"
                    value={params.settingsPage}
                    options={[
                      ["providers", "Model providers"],
                      ["appearance", "Appearance"],
                    ]}
                    onChange={(settingsPage) => update({ settingsPage })}
                  />
                )}
              </>
            )}
            <Segmented
              label={isDirection ? "Palette (preview only; samples use Light)" : "Palette"}
              value={params.palette}
              options={PALETTE_IDS.map((id) => [id, V2_PALETTES[id].name])}
              onChange={(p) => update({ palette: p })}
            />
          </div>
          {isDirection && (
            <p className="v2s-bar__note">
              <strong>{DIRECTIONS[params.sample as DirectionId].name}:</strong> {DIRECTIONS[params.sample as DirectionId].summary} {DIRECTIONS[params.sample as DirectionId].helper}{" "}
              Sample data · nothing is sent or saved.
            </p>
          )}
        </div>
      )}
      <div className="v2s-stage">
        {isDirection ? (
          <div
            key={key}
            className="v2s-frame"
            data-direction={params.sample}
            data-palette={palette.id}
            data-theme-kind={palette.kind}
            style={{ ...paletteStyle(palette), ...geometryStyle(DIRECTIONS[params.sample as DirectionId].geometry) }}
          >
            <DirectionView params={params} />
          </div>
        ) : (
          <div
            key={key}
            className="v2s-frame v2s-frame--sheet"
            data-direction="balanced"
            data-palette={palette.id}
            data-theme-kind={palette.kind}
            style={{ ...paletteStyle(palette), ...geometryStyle(DIRECTIONS.balanced.geometry) }}
          >
            {params.sample === "palette" ? <PaletteSheet palette={palette} /> : <IconSample palette={palette} />}
          </div>
        )}
      </div>
    </div>
  );
}

function DirectionView({ params }: { params: SampleParams }) {
  const theme = V2_PALETTES[params.palette].kind;
  if (params.view === "settings") {
    const page: SettingsPage = params.settingsPage === "appearance" ? "Appearance" : "Model providers";
    return <Workbench theme={theme} settings={<SettingsView initial={{ page, advanced: params.advanced, focusKey: params.focusKey }} />} />;
  }
  if (params.view === "agent") {
    return <Workbench theme={theme} initialTab={params.tab} auxiliary={<AgentPanel scrollTo={params.scroll} />} auxiliaryWidth={params.width} />;
  }
  return <Workbench theme={theme} initialTab={params.tab} initialMenu={params.menu ? "analysis/arrhenius_fit.py" : null} />;
}
