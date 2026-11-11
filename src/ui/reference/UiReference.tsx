// In-app UI reference: every shared component and status treatment, rendered
// with the live tokens in the current theme. Opened with the command
// "Developer: Show UI Reference". Sample content is labelled as such.
import "./reference.css";

import {
  Bold,
  Copy,
  File,
  FileCode,
  FileText,
  Folder,
  Hammer,
  Italic,
  Play,
  RefreshCw,
  Save,
  Settings2,
  Sigma,
  Square,
  Trash2,
  WrapText,
} from "lucide-react";
import { lazy, Suspense, useId, useState, type ReactNode } from "react";
import { Group, Panel as ResizablePanel } from "react-resizable-panels";

import { formatKeybinding } from "../../app/keybindings";
import { notify } from "../../app/notifications";
import { registry } from "../../app/registry";
import { useRegistryVersion } from "../../app/shell/useRegistry";
import {
  Banner,
  Button,
  Checkbox,
  confirmUnsaved,
  Dialog,
  DialogTrigger,
  EditorTabList,
  EmptyState,
  ErrorState,
  IconButton,
  Kbd,
  Keybinding,
  LoadingState,
  Menu,
  MenuItem,
  MenuSection,
  MenuSeparator,
  MenuTrigger,
  Panel,
  PanelBody,
  PanelHeader,
  ProgressBar,
  ResizeHandle,
  SearchField,
  Section,
  SegmentedControl,
  Select,
  Spinner,
  StatusPill,
  SubmenuTrigger,
  Switch,
  Tab,
  TabList,
  TabPanel,
  Tabs,
  TextField,
  ToggleButton,
  Toolbar,
  Tooltip,
  Tree,
  TreeItem,
  UnavailableState,
  type EditorTabItem,
  type Status,
} from "../components";
import { setThemePreference, useTheme, type ThemePreference } from "../theme";
import { palettes, type ColorRole } from "../tokens";

const MonacoSample = lazy(() => import("./MonacoSample"));

function RefSection({ title, description, children }: { title: string; description?: string; children: ReactNode }) {
  const id = useId();
  return (
    <section className="ref-section" aria-labelledby={id}>
      <h2 className="ref-section__title" id={id}>
        {title}
      </h2>
      {description && <p className="ref-section__description">{description}</p>}
      <div className="ref-section__body">{children}</div>
    </section>
  );
}

function Specimen({ label, children, wide }: { label: string; children: ReactNode; wide?: boolean }) {
  return (
    <figure className="ref-specimen" data-wide={wide || undefined}>
      <figcaption className="ref-specimen__label">{label}</figcaption>
      <div className="ref-specimen__content">{children}</div>
    </figure>
  );
}

const swatchGroups: { title: string; roles: ColorRole[] }[] = [
  {
    title: "Surfaces",
    roles: ["surface-base", "surface-sunken", "surface", "surface-raised", "surface-hover", "surface-selected", "field"],
  },
  { title: "Text", roles: ["text", "text-muted", "text-subtle", "text-disabled"] },
  { title: "Lines and focus", roles: ["border", "border-strong", "focus-ring", "selection"] },
  { title: "Accent", roles: ["accent", "accent-hover", "accent-text", "accent-subtle"] },
  {
    title: "Feedback",
    roles: ["info-text", "success-text", "warning-text", "danger-text", "status-stale-text", "status-neutral-text"],
  },
];

function Swatches() {
  const theme = useTheme((s) => s.resolved);
  return (
    <div className="ref-swatch-groups">
      {swatchGroups.map((group) => (
        <div key={group.title} className="ref-swatch-group">
          <h3 className="ref-subtitle">{group.title}</h3>
          <ul className="ref-swatches">
            {group.roles.map((role) => (
              <li key={role} className="ref-swatch">
                <span className="ref-swatch__chip" style={{ background: `var(--color-${role})` }} />
                <span className="ref-swatch__name">--color-{role}</span>
                <code className="ref-swatch__value">{palettes[theme][role]}</code>
              </li>
            ))}
          </ul>
        </div>
      ))}
    </div>
  );
}

const sampleTabs: EditorTabItem[] = [
  { key: "main.typ", title: "main.typ", icon: FileText, description: "paper/main.typ" },
  { key: "analysis.py", title: "analysis.py", icon: FileCode, dirty: true, description: "research/analysis.py" },
  { key: "refs.bib", title: "refs.bib", icon: File, preview: true, description: "refs.bib (preview)" },
];

function EditorTabsSample() {
  const [tabs, setTabs] = useState(sampleTabs);
  const [selected, setSelected] = useState<string>("main.typ");
  return (
    <div className="ref-editor-tabs">
      <Tabs selectedKey={selected} onSelectionChange={(k) => setSelected(String(k))}>
        <div className="ref-editor-tabs__strip" data-group-active>
          <EditorTabList
            aria-label="Sample editor tabs"
            items={tabs}
            onClose={(key) => setTabs((t) => t.filter((x) => x.key !== key))}
          />
        </div>
        {tabs.map((t) => (
          <TabPanel key={t.key} id={t.key} className="ref-editor-tabs__panel">
            Sample tab panel for {t.title}
          </TabPanel>
        ))}
      </Tabs>
      {tabs.length < sampleTabs.length && (
        <Button size="sm" variant="ghost" icon={RefreshCw} onPress={() => setTabs(sampleTabs)}>
          Restore sample tabs
        </Button>
      )}
    </div>
  );
}

function SplitSample() {
  return (
    <div className="ref-split">
      <Group id="ref-split" orientation="horizontal">
        <ResizablePanel id="ref-split-a" minSize="20%" defaultSize="50%">
          <div className="ref-split__pane">Left pane</div>
        </ResizablePanel>
        <ResizeHandle aria-label="Resize sample panes" />
        <ResizablePanel id="ref-split-b" minSize="20%">
          <div className="ref-split__pane">Right pane</div>
        </ResizablePanel>
      </Group>
    </div>
  );
}

function ShortcutTable() {
  useRegistryVersion();
  const rows = registry.allCommands().filter((c) => c.keybinding);
  return (
    <table className="ref-table">
      <thead>
        <tr>
          <th scope="col">Command</th>
          <th scope="col">Keys</th>
          <th scope="col">Id</th>
        </tr>
      </thead>
      <tbody>
        {rows.map((c) => (
          <tr key={c.id}>
            <td>{c.category ? `${c.category}: ${c.title}` : c.title}</td>
            <td>
              <Keybinding binding={c.keybinding!} />
            </td>
            <td>
              <code>{c.id}</code>
            </td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

const statuses: Status[] = ["idle", "running", "succeeded", "failed", "canceled", "stale", "unavailable", "warning"];

export function UiReference() {
  const preference = useTheme((s) => s.preference);
  const [unsavedResult, setUnsavedResult] = useState<string | null>(null);
  const [wrap, setWrap] = useState(true);
  const [mode, setMode] = useState<"source" | "page" | "split">("split");
  const [engine, setEngine] = useState<string>("lualatex");

  return (
    <div className="ref" data-testid="ui-reference">
      <header className="ref-header">
        <div>
          <h1 className="ref-header__title">UI Reference</h1>
          <p className="ref-header__lead">
            Shared tokens, components, and status treatments as implemented. Sample content is illustrative; it is not
            connected to project data.
          </p>
        </div>
        <SegmentedControl<ThemePreference>
          aria-label="Color theme"
          size="md"
          value={preference}
          onChange={setThemePreference}
          options={[
            { id: "light", label: "Light" },
            { id: "dark", label: "Dark" },
            { id: "system", label: "System" },
          ]}
        />
      </header>

      <RefSection title="Typography" description="System font stacks only (no remote fonts). UI 13px base; code 13px/20px.">
        <div className="ref-type">
          <p style={{ fontSize: "var(--font-size-2xl)", fontWeight: 600 }}>Heading 20 / semibold</p>
          <p style={{ fontSize: "var(--font-size-xl)", fontWeight: 600 }}>Title 16 / semibold — dialogs, empty states</p>
          <p style={{ fontSize: "var(--font-size-lg)" }}>Body 14 — dialog text, emphasized content</p>
          <p style={{ fontSize: "var(--font-size-md)" }}>UI 13 — default controls, lists, menus</p>
          <p style={{ fontSize: "var(--font-size-sm)", color: "var(--color-text-muted)" }}>Small 12 — tabs, status bar, hints</p>
          <p className="ref-label-sample">Label 11 / uppercase — pane headers</p>
          <pre className="ref-code-sample">{"code 13/20 — Cascadia Code, Cascadia Mono, Consolas\n$ integral_0^1 x^2 dif x = 1/3 $ != => <= ->"}</pre>
        </div>
      </RefSection>

      <RefSection title="Color roles" description="Semantic tokens; values shown for the current theme. Contrast is verified in tokens.test.ts.">
        <Swatches />
      </RefSection>

      <RefSection title="Buttons">
        <Specimen label="Variants (md)">
          <Button variant="primary" icon={Hammer}>
            Build
          </Button>
          <Button variant="secondary">Export…</Button>
          <Button variant="ghost">Details</Button>
          <Button variant="danger" icon={Trash2}>
            Delete
          </Button>
        </Specimen>
        <Specimen label="Sizes">
          <Button size="sm">Small</Button>
          <Button size="md">Medium</Button>
          <Button size="lg">Large</Button>
        </Specimen>
        <Specimen label="Disabled and pending">
          <Button isDisabled>Disabled</Button>
          <Button variant="primary" isPending>
            Building
          </Button>
        </Specimen>
        <Specimen label="Icon buttons (tooltip + shortcut)">
          <IconButton icon={Play} aria-label="Run cell" keybinding="Mod+Enter" />
          <IconButton icon={Square} aria-label="Stop" />
          <IconButton icon={Copy} aria-label="Copy" variant="secondary" />
          <IconButton icon={Settings2} aria-label="Settings" isDisabled />
        </Specimen>
        <Specimen label="Toggle buttons">
          <ToggleButton icon={WrapText} variant="secondary" isSelected={wrap} onChange={setWrap}>
            Wrap lines
          </ToggleButton>
          <ToggleButton icon={Bold} aria-label="Bold" />
          <ToggleButton icon={Italic} aria-label="Italic" defaultSelected />
        </Specimen>
        <Specimen label="Segmented control (document mode)">
          <SegmentedControl
            aria-label="Document mode"
            value={mode}
            onChange={setMode}
            options={[
              { id: "source", label: "Source" },
              { id: "page", label: "Page" },
              { id: "split", label: "Split" },
            ]}
          />
        </Specimen>
      </RefSection>

      <RefSection title="Menus">
        <Specimen label="Menu with shortcuts, sections, submenu">
          <MenuTrigger>
            <Button>Open menu</Button>
            <Menu aria-label="Sample menu">
              <MenuSection title="Document">
                <MenuItem id="save" icon={Save} keybinding="Mod+S">
                  Save
                </MenuItem>
                <MenuItem id="export" description="PDF, SVG, or PNG">
                  Export…
                </MenuItem>
              </MenuSection>
              <MenuSeparator />
              <SubmenuTrigger>
                <MenuItem id="build" icon={Hammer} hasSubmenu>
                  Build target
                </MenuItem>
                <Menu isSubmenu aria-label="Build target" selectionMode="single" defaultSelectedKeys={["pdflatex"]}>
                  <MenuItem id="pdflatex">pdfLaTeX</MenuItem>
                  <MenuItem id="lualatex">LuaLaTeX</MenuItem>
                  <MenuItem id="typst">Typst</MenuItem>
                </Menu>
              </SubmenuTrigger>
              <MenuItem id="delete" tone="danger" icon={Trash2}>
                Delete build output
              </MenuItem>
              <MenuItem id="disabled" isDisabled>
                Unavailable action
              </MenuItem>
            </Menu>
          </MenuTrigger>
        </Specimen>
      </RefSection>

      <RefSection title="Tabs">
        <Specimen label="View tabs (panel and auxiliary regions)" wide>
          <Tabs>
            <TabList aria-label="Sample panel views">
              <Tab id="output">Output</Tab>
              <Tab id="problems">Problems</Tab>
              <Tab id="terminal">Terminal</Tab>
            </TabList>
            <TabPanel id="output" className="ref-tab-panel">
              Output panel content
            </TabPanel>
            <TabPanel id="problems" className="ref-tab-panel">
              Problems panel content
            </TabPanel>
            <TabPanel id="terminal" className="ref-tab-panel">
              Terminal panel content
            </TabPanel>
          </Tabs>
        </Specimen>
        <Specimen label="Editor tabs: active, dirty (dot), preview (italic); close button, middle click, Delete" wide>
          <EditorTabsSample />
        </Specimen>
      </RefSection>

      <RefSection title="Dialogs">
        <Specimen label="Dialog">
          <DialogTrigger>
            <Button>Open dialog</Button>
            <Dialog
              title="Export document"
              footer={
                <>
                  <Button slot="close">Cancel</Button>
                  <Button slot="close" variant="primary">
                    Export
                  </Button>
                </>
              }
            >
              <div className="ref-dialog-form">
                <TextField label="File name" defaultValue="main.pdf" autoFocus />
                <Checkbox defaultSelected>Embed fonts</Checkbox>
              </div>
            </Dialog>
          </DialogTrigger>
        </Specimen>
        <Specimen label="Confirm: unsaved changes">
          <Button onPress={async () => setUnsavedResult(await confirmUnsaved("analysis.py"))}>Close dirty file…</Button>
          {unsavedResult && <span className="ref-result">Result: {unsavedResult}</span>}
        </Specimen>
      </RefSection>

      <RefSection title="Inputs">
        <Specimen label="Text field">
          <TextField label="Project name" description="Used as the folder name." defaultValue="lattice-paper" />
        </Specimen>
        <Specimen label="Invalid and code fields">
          <TextField label="Interpreter" code defaultValue="/usr/bin/python4" isInvalid errorMessage="File not found." />
        </Specimen>
        <Specimen label="Search field">
          <SearchField aria-label="Filter files" placeholder="Filter files" defaultValue="lemma" />
        </Specimen>
        <Specimen label="Select">
          <Select
            label="Engine"
            value={engine}
            onChange={setEngine}
            options={[
              { id: "pdflatex", label: "pdfLaTeX" },
              { id: "lualatex", label: "LuaLaTeX", description: "Unicode fonts, Lua" },
              { id: "xelatex", label: "XeLaTeX" },
            ]}
          />
        </Specimen>
        <Specimen label="Checkbox">
          <Checkbox defaultSelected>Run on save</Checkbox>
          <Checkbox isIndeterminate>Some files</Checkbox>
          <Checkbox isDisabled>Disabled</Checkbox>
        </Specimen>
        <Specimen label="Switch">
          <Switch defaultSelected>Auto-build</Switch>
          <Switch>Show whitespace</Switch>
        </Specimen>
      </RefSection>

      <RefSection title="Tooltip, toolbar, panel, section">
        <Specimen label="Tooltip (hover or focus)">
          <Tooltip label="Rebuild the document" keybinding="Mod+Shift+B">
            <Button icon={Hammer}>Rebuild</Button>
          </Tooltip>
        </Specimen>
        <Specimen label="Toolbar (arrow keys move between controls)">
          <Toolbar aria-label="Sample formatting">
            <ToggleButton icon={Bold} aria-label="Bold" size="sm" />
            <ToggleButton icon={Italic} aria-label="Italic" size="sm" />
            <IconButton icon={Sigma} aria-label="Insert equation" size="sm" />
          </Toolbar>
        </Specimen>
        <Specimen label="Panel with header actions and sections" wide>
          <div className="ref-panel-frame">
            <Panel aria-label="Sample panel">
              <PanelHeader
                title="Outline"
                actions={
                  <Toolbar aria-label="Outline actions">
                    <IconButton icon={RefreshCw} aria-label="Refresh outline" size="sm" />
                  </Toolbar>
                }
              />
              <PanelBody>
                <Section title="Sections" collapsible>
                  <p className="ref-section-text">1 Introduction · 2 Method · 3 Results</p>
                </Section>
                <Section title="Figures" collapsible defaultExpanded={false}>
                  <p className="ref-section-text">Figure 1 · Figure 2</p>
                </Section>
              </PanelBody>
            </Panel>
          </div>
        </Specimen>
      </RefSection>

      <RefSection title="Tree" description="react-aria Tree: arrows navigate, Right/Left expand/collapse, type-ahead. Sample data.">
        <div className="ref-tree-frame">
          <Tree aria-label="Sample project files" defaultExpandedKeys={["paper", "figures"]} selectionMode="single" defaultSelectedKeys={["main"]}>
            <TreeItem id="paper" title="paper" icon={Folder}>
              <TreeItem id="main" title="main.typ" icon={FileText} trailing={<StatusPill status="succeeded" size="sm" label="Built" />} />
              <TreeItem id="refs" title="refs.bib" icon={File} />
              <TreeItem id="figures" title="figures" icon={Folder}>
                <TreeItem id="fig1" title="convergence.svg" icon={File} trailing={<StatusPill status="stale" size="sm" />} />
              </TreeItem>
            </TreeItem>
            <TreeItem id="research" title="research" icon={Folder}>
              <TreeItem id="analysis" title="analysis.py" icon={FileCode} />
            </TreeItem>
          </Tree>
        </div>
      </RefSection>

      <RefSection title="Status" description="Icon + label, never color alone. Running shows a spinner.">
        <Specimen label="Status pills" wide>
          {statuses.map((s) => (
            <StatusPill key={s} status={s} />
          ))}
          <StatusPill status="running" label="Building main.typ" detail="4s" />
          <StatusPill status="failed" label="Build failed" detail="3 errors" />
        </Specimen>
        <Specimen label="Spinners">
          <Spinner size="sm" />
          <Spinner size="md" />
          <Spinner size="lg" />
        </Specimen>
        <Specimen label="Progress">
          <div className="ref-stack">
            <ProgressBar label="Rendering pages" value={40} valueLabel="4 of 10 pages" />
            <ProgressBar label="Starting kernel" />
          </div>
        </Specimen>
        <Specimen label="Notifications (live region)">
          <Button size="sm" onPress={() => notify({ tone: "info", title: "Kernel started", body: "Python 3.14 (research/.venv)" })}>
            Info
          </Button>
          <Button size="sm" onPress={() => notify({ tone: "success", title: "Build succeeded", body: "main.typ · 12 pages · 1.4s" })}>
            Success
          </Button>
          <Button
            size="sm"
            onPress={() => notify({ tone: "warning", title: "Output is stale", body: "Source changed after the last build." })}
          >
            Warning
          </Button>
          <Button
            size="sm"
            onPress={() =>
              notify({
                tone: "danger",
                title: "Build failed",
                body: "main.typ:14:3 unknown variable: totl",
                actions: [{ label: "Show problem", run: () => {} }],
              })
            }
          >
            Danger
          </Button>
        </Specimen>
      </RefSection>

      <RefSection
        title="Status treatments"
        description="Loading, empty, error, canceled, stale output, and unavailable tool. Examples only."
      >
        <div className="ref-states">
          <div className="ref-state-frame">
            <LoadingState title="Loading pages…" />
          </div>
          <div className="ref-state-frame">
            <EmptyState icon={FileText} title="No output yet" description="Build the document to see its pages here." actions={<Button icon={Hammer}>Build</Button>} />
          </div>
          <div className="ref-state-frame">
            <ErrorState
              live={false}
              title="Could not render pages"
              description="The compiler exited before producing output."
              details={"error: unknown variable: totl\n  ┌─ main.typ:14:3"}
              actions={<Button>Show log</Button>}
            />
          </div>
          <div className="ref-state-frame">
            <UnavailableState
              title="LaTeX toolchain not found"
              description="latexmk was not found on PATH. Install a TeX distribution or set its location."
              actions={<Button>Set location…</Button>}
            />
          </div>
        </div>
        <div className="ref-stack">
          <Banner tone="stale" title="Showing output from an earlier revision" actions={<Button size="sm">Rebuild</Button>}>
            The source changed after this build. Page edits are disabled until the pages are current.
          </Banner>
          <Banner tone="canceled" title="Run canceled" actions={<Button size="sm">Run again</Button>}>
            analysis.py was stopped after 12s. Partial output is shown below.
          </Banner>
          <Banner tone="warning" title="3 warnings in the last build" />
          <Banner tone="info" title="Recovered unsaved changes" actions={<Button size="sm" variant="ghost">Compare</Button>} />
        </div>
      </RefSection>

      <RefSection title="Focus states" description="Keyboard focus shows a 2px focus-ring outline; these specimens force the ring.">
        <Specimen label="Forced focus ring">
          <button type="button" className="ui-button ui-focus-demo" data-variant="secondary" data-size="md" tabIndex={-1}>
            Button
          </button>
          <button type="button" className="ui-button ui-focus-demo" data-variant="primary" data-size="md" tabIndex={-1}>
            Primary
          </button>
          <input className="ui-input ref-focus-input" data-focused readOnly tabIndex={-1} value="Input" aria-label="Focused input sample" />
        </Specimen>
      </RefSection>

      <RefSection title="Keyboard" description="Registered shortcuts (live from the command registry).">
        <Specimen label="Key caps">
          <Kbd>Esc</Kbd>
          <Keybinding binding="Mod+Shift+P" />
          <span className="ref-muted">macOS:</span>
          <span className="ref-mac-keys">{formatKeybinding("Mod+Shift+P", "mac").join("")}</span>
        </Specimen>
        <ShortcutTable />
      </RefSection>

      <RefSection title="Split handle" description="Focus the handle and use Left/Right arrows; Home/End jump to limits.">
        <SplitSample />
      </RefSection>

      <RefSection title="Monaco theme" description="exposition-light / exposition-dark, generated from tokens.">
        <Suspense fallback={<LoadingState title="Loading editor…" fill={false} />}>
          <MonacoSample />
        </Suspense>
      </RefSection>
    </div>
  );
}
