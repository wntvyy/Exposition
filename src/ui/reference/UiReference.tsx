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
  History,
  Italic,
  Network,
  Play,
  RefreshCw,
  Save,
  Settings2,
  Sigma,
  Square,
  Trash2,
  TriangleAlert,
  WrapText,
} from "lucide-react";
import { lazy, Suspense, useId, useState, type ReactNode } from "react";
import { Group, Panel as ResizablePanel } from "react-resizable-panels";

import { formatKeybinding } from "../../app/keybindings";
import { notify } from "../../app/notifications";
import { registry } from "../../app/registry";
import { useRegistryVersion } from "../../app/shell/useRegistry";
import {
  Badge,
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
  InspectorBody,
  InspectorHeader,
  Kbd,
  Keybinding,
  KeyValueList,
  ListRow,
  LoadingState,
  Markdown,
  Menu,
  MenuItem,
  MenuSection,
  MenuSeparator,
  MenuTrigger,
  NodeCard,
  PageHeader,
  Panel,
  PanelBody,
  PanelHeader,
  ProgressBar,
  ResizeHandle,
  ResponseSurface,
  SearchField,
  Section,
  SegmentedControl,
  Select,
  SimpleTable,
  Spinner,
  StateSizeProvider,
  StatusPill,
  StatusText,
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
  type CodeRunAction,
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
    <SimpleTable
      aria-label="Keyboard shortcuts"
      columns={[
        { id: "command", title: "Command", render: (c: (typeof rows)[number]) => (c.category ? `${c.category}: ${c.title}` : c.title) },
        { id: "keys", title: "Keys", render: (c: (typeof rows)[number]) => <Keybinding binding={c.keybinding!} /> },
        { id: "id", title: "Id", render: (c: (typeof rows)[number]) => <span className="ui-code">{c.id}</span> },
      ]}
      rows={rows}
      rowKey={(c) => c.id}
    />
  );
}

const statuses: Status[] = ["idle", "running", "succeeded", "failed", "canceled", "stale", "unavailable", "warning"];

interface SampleInput {
  input: string;
  path: string;
  size: string;
  version: string;
}

const sampleInputs: SampleInput[] = [
  { input: "Observations (Dataset)", path: "data/obs.csv", size: "16 B", version: "Current at review" },
  { input: "Parameters (Table)", path: "data/params.csv", size: "1.2 KiB", version: "Pinned revision 3" },
];

/** The representative response shown in every treatment (also streamed by the a1-responses e2e scenario). */
export const SAMPLE_RESPONSE = [
  "The series converges because the terms decay like $1/n^2$; see the [Basel problem](https://en.wikipedia.org/wiki/Basel_problem) for the closed form. In short, **the sum is finite** and *the partial sums increase monotonically*.",
  "",
  "$$",
  String.raw`\sum_{n=1}^{\infty} \frac{1}{n^2} = \frac{\pi^2}{6}`,
  "$$",
  "",
  "A quick numerical check in Python:",
  "",
  "```python",
  "import math",
  "",
  "partial = sum(1 / n**2 for n in range(1, 10_001))",
  "print(partial, math.pi**2 / 6)  # 1.6448340718 1.6449340668",
  "```",
  "",
  "What the check shows:",
  "",
  String.raw`- the partial sum after $10^4$ terms is within $10^{-4}$ of $\pi^2/6$;`,
  "- the error shrinks like $1/N$;",
  "- no term is negative, so the order of summation does not matter.",
  "",
  "| $N$ | Partial sum | Error |",
  "|---:|---:|---:|",
  "| 10 | 1.549768 | 0.095166 |",
  "| 100 | 1.634984 | 0.009950 |",
  "| 10 000 | 1.644834 | 0.000100 |",
].join("\n");

const sampleRun: CodeRunAction = {
  label: "Run in Research",
  run: () => notify({ tone: "info", title: "Sample only", body: "In a transcript, Run in Research hands the code to your research session." }),
};

/** A pane as the transcript sees it: its backdrop (page white or region sunken) and the pane inset. */
function ResponsePane({ region }: { region: boolean }) {
  return (
    <div
      data-testid={`ref-response-${region ? "panel" : "page"}`}
      style={{
        padding: "var(--space-3) var(--pane-inset)",
        border: "var(--border-width) solid var(--color-border)",
        borderRadius: "var(--radius-md)",
        background: region ? "var(--color-surface-sunken)" : "var(--color-surface)",
      }}
    >
      <ResponseSurface label="Agent" copyText={SAMPLE_RESPONSE}>
        <Markdown source={SAMPLE_RESPONSE} runAction={sampleRun} />
      </ResponseSurface>
    </div>
  );
}

function AgentResponseSpecimens() {
  return (
    <>
      <Specimen label="Page (Agents page, comfortable)" wide>
        <div style={{ width: "100%" }}>
          <ResponsePane region={false} />
        </div>
      </Specimen>
      <StateSizeProvider value="compact">
        <Specimen label="Panel (Agent panel, compact)">
          <div style={{ width: "100%", maxWidth: 400 }}>
            <ResponsePane region />
          </div>
        </Specimen>
      </StateSizeProvider>
    </>
  );
}

export function UiReference() {
  const [section, setSection] = useState<"project" | "math">("project");
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

      <RefSection
        title="Page header"
        description="One header for Research, Agents, Graph, the workflow editor, and Settings pages: title, view switch, actions, headline status. Optional toolbar row with the same inset."
      >
        <div className="ref-panel-frame ref-wide-frame">
          <PageHeader
            title="Graph"
            views={
              <SegmentedControl
                aria-label="Sample graph section"
                value={section}
                onChange={setSection}
                options={[
                  { id: "project", label: "Project" },
                  { id: "math", label: "Mathematics" },
                ]}
              />
            }
            status={
              <>
                <StatusPill status="succeeded" label="Synchronized" />
                <span>15 items · 17 relationships</span>
              </>
            }
            toolbar={
              <>
                <SearchField aria-label="Sample search" placeholder="Search items" />
                <Button variant="ghost" size="sm" icon={RefreshCw}>
                  Arrange
                </Button>
              </>
            }
          />
        </div>
      </RefSection>

      <RefSection
        title="Inspector"
        description="Pane header (uppercase chrome label), item header, then content sections in sentence case with a count; key/value facts; quiet inline status; provisional facts as dashed outline badges. Region states inside are compact."
      >
        <div className="ref-panel-frame ref-inspector-frame">
          <Panel aria-label="Sample inspector">
            <PanelHeader title="Inspector" actions={<IconButton icon={Square} aria-label="Close inspector (sample)" size="sm" />} />
            <InspectorHeader
              type={
                <span className="ref-type-math">
                  <Sigma size={12} aria-hidden /> Theorem
                </span>
              }
              title="Image of a compact set"
              meta={<span>paper.tex · characters 389–522</span>}
              actions={
                <>
                  <Button size="sm" icon={FileText}>
                    Open source
                  </Button>
                  <Button size="sm" variant="ghost" icon={Network}>
                    Relate to…
                  </Button>
                </>
              }
            />
            <InspectorBody>
              <Section title="Relationships" count={3}>
                <ul className="ref-plain-list">
                  <li>
                    assumes <a className="ref-link">Hypothesis H</a> <span className="ref-muted">· accepted</span>
                  </li>
                  <li>
                    depends on <a className="ref-link">Closed subsets of compacts</a> <span className="ref-muted">· observed</span>
                  </li>
                  <li>
                    supports <a className="ref-link">Lemma 2</a> <Badge dashed tone="accent">Suggested</Badge>
                  </li>
                </ul>
              </Section>
              <Section title="Evidence" count={1}>
                <KeyValueList
                  items={[
                    { key: "kind", label: "Kind", value: "Computational" },
                    { key: "run", label: "Run", value: "run-5ca362b0e9c5", code: true },
                    { key: "outcome", label: "Outcome", value: <StatusText status="succeeded" label="Run succeeded" /> },
                    { key: "fresh", label: "Freshness", value: <StatusText status="idle" label="Current" /> },
                  ]}
                />
              </Section>
              <Section title="Formal verification" collapsible>
                <EmptyState title="No formal declaration is associated." description="Associate a Lean declaration to record a reviewed correspondence." />
              </Section>
            </InspectorBody>
          </Panel>
        </div>
      </RefSection>

      <RefSection
        title="Lists and tables"
        description="List rows (tasks, runs, workflows, storage items) are flat rows with dividers, not cards. Static tables share DataTable's header, row height, and numeric alignment."
      >
        <Specimen label="List rows (one selected)" wide>
          <div className="ref-list-frame" role="list" aria-label="Sample runs">
            <ListRow
              role="listitem"
              icon={<Play size={14} aria-hidden />}
              title="# Least-squares fit of T(n)"
              codeTitle
              meta={
                <>
                  <StatusText status="succeeded" label="Executed" detail="0.7 s" />
                  <span>03:02 PM · figure · table</span>
                </>
              }
              selected
            />
            <ListRow
              role="listitem"
              icon={<Play size={14} aria-hidden />}
              title="df.assign(y2=df.y * 2)"
              codeTitle
              meta={
                <>
                  <StatusText status="failed" />
                  <span>03:05 PM · NameError</span>
                </>
              }
            />
            <ListRow
              role="listitem"
              icon={<Play size={14} aria-hidden />}
              title="Parameter sweep"
              meta={<StatusText status="running" label="Running" detail="12 s" />}
              trailing={<IconButton icon={Square} aria-label="Stop (sample)" size="sm" />}
            />
          </div>
        </Specimen>
        <Specimen label="Static table" wide>
          <SimpleTable
            aria-label="Sample inputs"
            columns={[
              { id: "input", title: "Input", render: (r: SampleInput) => <strong>{r.input}</strong> },
              { id: "path", title: "Path", render: (r: SampleInput) => <span className="ui-code">{r.path}</span> },
              { id: "size", title: "Size", numeric: true, render: (r: SampleInput) => r.size },
              { id: "version", title: "Version", render: (r: SampleInput) => <StatusText status="idle" label={r.version} /> },
            ]}
            rows={sampleInputs}
            rowKey={(r) => r.input}
          />
        </Specimen>
      </RefSection>

      <RefSection
        title="Canvas nodes"
        description="Neutral card with the type color only in the header band (icon + type label as text). Status as badges with icons; selection is an accent outline with lift, never a type color."
      >
        <div className="ref-canvas-frame">
          <div className="ref-node">
            <NodeCard title="Image of a compact set" typeLabel="Theorem" typeIcon={Sigma} category="math" selected />
          </div>
          <div className="ref-node">
            <NodeCard
              title="Closed subsets of compacts"
              typeLabel="Lemma"
              typeIcon={Sigma}
              category="math"
              badges={
                <>
                  <Badge tone="warning" icon={TriangleAlert}>
                    No proof linked
                  </Badge>
                  <Badge tone="stale" icon={History}>
                    Stale evidence
                  </Badge>
                </>
              }
            />
          </div>
          <div className="ref-node">
            <NodeCard title="paper.tex" typeLabel="Manuscript" typeIcon={FileText} category="document" />
          </div>
          <div className="ref-node">
            <NodeCard title="obs.csv" typeLabel="Dataset" typeIcon={File} category="data" inactive badges={<Badge dashed>Archived</Badge>} />
          </div>
        </div>
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
        <Specimen label="Quiet inline status (rows, tables, inspector lines)" wide>
          {statuses.map((s) => (
            <StatusText key={s} status={s} />
          ))}
          <StatusText status="succeeded" label="Executed" detail="0.7 s" />
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
        title="Agent responses"
        description="ResponseSurface + Markdown: an inset block with a subtle fill on the pane inset, comfortable on pages and compact in panels. Prose, math, a code fence, a list, and a table. Sample content."
      >
        <AgentResponseSpecimens />
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
        <p className="ref-muted">Compact (panels, sidebars, inspector sections):</p>
        <StateSizeProvider value="compact">
          <div className="ref-states">
            <div className="ref-state-frame ref-state-frame--compact">
              <EmptyState icon={FileText} title="No problems." description="Build and proof-checking diagnostics appear here." />
            </div>
            <div className="ref-state-frame ref-state-frame--compact">
              <LoadingState title="Loading runs…" />
            </div>
            <div className="ref-state-frame ref-state-frame--compact">
              <UnavailableState title="Graph unavailable." description="The graph database is locked by another window." actions={<Button size="sm">Retry</Button>} />
            </div>
          </div>
        </StateSizeProvider>
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
