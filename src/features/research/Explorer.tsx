// Research explorer: the research space's groups over actual resource
// identities — Sources, Data, Notes (managed folders), Experiments, Results
// (retained outcomes), Workflows (saved definitions, read-only here), and
// external References.
import {
  Archive,
  Beaker,
  FileCode2,
  FileInput,
  FilePlus2,
  FileText,
  FlaskConical,
  Image,
  Link2,
  NotebookText,
  Plus,
  Table2,
  Workflow,
} from "lucide-react";
import { useState } from "react";

import type { ResearchListing } from "../../bindings/ResearchListing";
import type { SpaceGroup } from "../../bindings/SpaceGroup";
import {
  Button,
  Dialog,
  IconButton,
  MenuItem,
  Menu,
  MenuTrigger,
  Select,
  StatusPill,
  TextField,
  Toolbar,
  Tree,
  TreeItem,
} from "../../ui/components";
import { createSourceFile, importInto, itemKey, parseItemKey, selectItem, useSpace } from "./space";

const GROUPS: { id: SpaceGroup; title: string }[] = [
  { id: "sources", title: "Sources" },
  { id: "data", title: "Data" },
  { id: "notes", title: "Notes" },
];

function fileIcon(name: string) {
  const ext = name.split(".").pop()?.toLowerCase() ?? "";
  if (["py", "r", "jl", "ipy"].includes(ext)) return FileCode2;
  if (["csv", "tsv", "json", "parquet", "xlsx"].includes(ext)) return Table2;
  if (["md", "txt", "org", "rst"].includes(ext)) return NotebookText;
  return FileText;
}

function NewFileDialog({ onClose }: { onClose: () => void }) {
  const [group, setGroup] = useState<SpaceGroup>("sources");
  const [name, setName] = useState("analysis.py");
  const [busy, setBusy] = useState(false);
  const create = async () => {
    setBusy(true);
    const path = await createSourceFile(group, name.trim());
    setBusy(false);
    if (path) onClose();
  };
  return (
    <Dialog
      title="New research file"
      isOpen
      onOpenChange={(open) => !open && onClose()}
      footer={
        <>
          <Button variant="secondary" onPress={onClose}>
            Cancel
          </Button>
          <Button variant="primary" isPending={busy} isDisabled={!name.trim()} onPress={() => void create()}>
            Create
          </Button>
        </>
      }
    >
      <Select<SpaceGroup> label="Group" options={GROUPS.map((g) => ({ id: g.id, label: g.title }))} value={group} onChange={setGroup} />
      <TextField
        label="File name"
        code
        value={name}
        onChange={setName}
        description="Python, R, or Julia code in Sources; CSV/JSON/… in Data; Markdown or text in Notes."
      />
    </Dialog>
  );
}

function groupFiles(listing: ResearchListing, group: SpaceGroup) {
  return group === "sources" ? listing.sources : group === "data" ? listing.data : listing.notes;
}

export function ResearchExplorer() {
  const listing = useSpace((s) => s.listing);
  const [expanded, setExpanded] = useState<Set<string>>(
    () => new Set(["group:sources", "group:data", "group:notes", "group:experiments", "group:results", "group:workflows"]),
  );
  const [creating, setCreating] = useState(false);
  if (!listing?.space) return null;
  const onAction = (key: string) => {
    const item = parseItemKey(key);
    if (item) selectItem(item);
  };
  return (
    <div className="exp-rexplorer" data-testid="research-explorer">
      <Toolbar aria-label="Research space actions" className="exp-rexplorer__toolbar">
        <IconButton icon={FilePlus2} size="sm" aria-label="New research file…" onPress={() => setCreating(true)} />
        <MenuTrigger>
          <IconButton icon={FileInput} size="sm" aria-label="Import a file…" />
          <Menu aria-label="Import a file">
            <MenuItem id="copy-data" onAction={() => void importInto("data", "copy")} description="The copy is a project file; its origin is recorded">
              Copy into Data…
            </MenuItem>
            <MenuItem id="copy-sources" onAction={() => void importInto("sources", "copy")}>
              Copy into Sources…
            </MenuItem>
            <MenuItem id="reference" onAction={() => void importInto("data", "reference")} description="Read-only; never copied, crawled, or run">
              Reference a file (read-only)…
            </MenuItem>
          </Menu>
        </MenuTrigger>
        <IconButton icon={Plus} size="sm" aria-label="New experiment" onPress={() => selectItem({ kind: "experiment", entity: null })} />
      </Toolbar>
      {creating && <NewFileDialog onClose={() => setCreating(false)} />}
      <Tree
        aria-label="Research space"
        className="xp-tree exp-rexplorer__tree"
        data-testid="research-tree"
        expandedKeys={expanded}
        onExpandedChange={(keys) => setExpanded(new Set([...keys].map(String)))}
        onAction={(key) => onAction(String(key))}
      >
        {GROUPS.map((g) => {
          const files = groupFiles(listing, g.id);
          return (
            <TreeItem key={g.id} id={`group:${g.id}`} title={g.title} icon={FlaskConical} hasChildItems trailing={<span className="xp-tree-note">{files.length}</span>} data-group={g.id}>
              {files.length === 0 ? (
                <TreeItem id={`empty:${g.id}`} title={g.id === "data" ? "No data yet — import or create a file" : "Empty"} isDisabled />
              ) : (
                files.map((f) => (
                  <TreeItem
                    key={f.path}
                    id={itemKey({ kind: "file", path: f.path })}
                    title={f.name}
                    icon={fileIcon(f.name)}
                    data-path={f.path}
                    data-entity={f.entity ?? undefined}
                    trailing={f.origin ? <span className="xp-tree-note" title={`Imported from ${f.origin.source}`}>imported</span> : undefined}
                  />
                ))
              )}
            </TreeItem>
          );
        })}
        {listing.references.length > 0 && (
          <TreeItem id="group:references" title="References" icon={Link2} hasChildItems trailing={<span className="xp-tree-note">{listing.references.length}</span>}>
            {listing.references.map((r) => (
              <TreeItem key={r.entity} id={itemKey({ kind: "reference", entity: r.entity })} title={r.label} icon={Link2} trailing={<span className="xp-tree-note">read-only</span>} />
            ))}
          </TreeItem>
        )}
        <TreeItem id="group:experiments" title="Experiments" icon={Beaker} hasChildItems trailing={<span className="xp-tree-note">{listing.experiments.length}</span>}>
          {listing.experiments.length === 0 ? (
            <TreeItem id="empty:experiments" title="No experiments — use + to describe one" isDisabled />
          ) : (
            listing.experiments.map((e) => (
              <TreeItem key={e.entity} id={itemKey({ kind: "experiment", entity: e.entity })} title={e.label} icon={Beaker} data-entity={e.entity} />
            ))
          )}
        </TreeItem>
        <TreeItem id="group:results" title="Results" icon={Archive} hasChildItems trailing={<span className="xp-tree-note">{listing.results.length}</span>}>
          {listing.results.length === 0 ? (
            <TreeItem id="empty:results" title="Keep a figure or table from a run" isDisabled />
          ) : (
            listing.results.map((r) => (
              <TreeItem
                key={r.entity}
                id={itemKey({ kind: "result", entity: r.entity })}
                title={r.label}
                icon={r.kind === "table" ? Table2 : Image}
                data-entity={r.entity}
                data-lifecycle={r.lifecycle}
                trailing={
                  r.lifecycle === "active" ? (
                    r.usedBy.length > 0 ? <span className="xp-tree-note">in {r.usedBy.length} doc</span> : undefined
                  ) : (
                    <StatusPill size="sm" status="canceled" label={r.lifecycle === "archived" ? "Archived" : "Discarded"} />
                  )
                }
              />
            ))
          )}
        </TreeItem>
        <TreeItem id="group:workflows" title="Workflows" icon={Workflow} hasChildItems trailing={<span className="xp-tree-note">{listing.workflows.length}</span>}>
          {listing.workflows.length === 0 ? (
            <TreeItem id="empty:workflows" title="No saved workflows" isDisabled />
          ) : (
            listing.workflows.map((w) => (
              <TreeItem key={w.entity} id={itemKey({ kind: "workflow", entity: w.entity })} title={w.label} icon={Workflow} />
            ))
          )}
        </TreeItem>
      </Tree>
      {listing.truncated && <p className="exp-muted">Some files are not listed (too many).</p>}
    </div>
  );
}
