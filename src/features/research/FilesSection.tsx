// The Research subsection of the Files explorer: the research space's groups
// over actual resource identities. Items open the Research page at the item.
import { Beaker, FileCode2, FlaskConical, Image, Table2, Workflow } from "lucide-react";
import { useEffect, useState } from "react";

import { Button, Section, Tree, TreeItem } from "../../ui/components";
import { itemKey, parseItemKey, revealInResearch, startSpaceTracking, useSpace, type ResearchItem } from "./space";
import "./research.css";

const MAX_SHOWN = 50;

export function ResearchFilesSection() {
  const listing = useSpace((s) => s.listing);
  const loaded = useSpace((s) => s.loaded);
  const [expanded, setExpanded] = useState<Set<string>>(() => new Set());
  useEffect(() => startSpaceTracking(), []);
  if (!loaded) return null;
  if (!listing?.space) {
    return (
      <Section key="disabled" title="Research" collapsible defaultExpanded={false}>
        <div className="xp-files__research" data-testid="files-research" data-enabled="false">
          <p className="xp-files__research-note">Research is not enabled for this project.</p>
          <Button size="sm" variant="ghost" icon={FlaskConical} onPress={() => revealInResearch(null)}>
            Open Research…
          </Button>
        </div>
      </Section>
    );
  }
  const groups: { id: string; title: string; icon: typeof Beaker; items: { item: ResearchItem; title: string }[] }[] = [
    {
      id: "sources",
      title: "Sources",
      icon: FileCode2,
      items: listing.sources.map((f) => ({ item: { kind: "file", path: f.path }, title: f.name })),
    },
    { id: "data", title: "Data", icon: Table2, items: listing.data.map((f) => ({ item: { kind: "file", path: f.path }, title: f.name })) },
    { id: "notes", title: "Notes", icon: FileCode2, items: listing.notes.map((f) => ({ item: { kind: "file", path: f.path }, title: f.name })) },
    {
      id: "experiments",
      title: "Experiments",
      icon: Beaker,
      items: listing.experiments.map((e) => ({ item: { kind: "experiment", entity: e.entity }, title: e.label })),
    },
    {
      id: "results",
      title: "Results",
      icon: Image,
      items: listing.results
        .filter((r) => r.lifecycle === "active")
        .map((r) => ({ item: { kind: "result", entity: r.entity }, title: r.label })),
    },
    {
      id: "workflows",
      title: "Workflows",
      icon: Workflow,
      items: listing.workflows.map((w) => ({ item: { kind: "workflow", entity: w.entity }, title: w.label })),
    },
  ];
  return (
    <Section key="enabled" title="Research" collapsible defaultExpanded>
      <div className="xp-files__research" data-testid="files-research" data-enabled="true">
        <Tree
          aria-label="Research space"
          className="xp-tree"
          expandedKeys={expanded}
          onExpandedChange={(keys) => setExpanded(new Set([...keys].map(String)))}
          onAction={(key) => {
            const k = String(key);
            if (k.startsWith("rgroup:")) {
              revealInResearch(null);
              return;
            }
            const item = parseItemKey(k);
            if (item) revealInResearch(item);
          }}
        >
          {groups.map((g) => (
            <TreeItem
              key={g.id}
              id={`rgroup:${g.id}`}
              title={g.title}
              icon={g.icon}
              hasChildItems={g.items.length > 0}
              data-research-group={g.id}
              trailing={<span className="xp-tree-note">{g.items.length}</span>}
            >
              {g.items.slice(0, MAX_SHOWN).map(({ item, title }) => (
                <TreeItem key={itemKey(item)} id={itemKey(item)} title={title} data-research-item={itemKey(item)} />
              ))}
            </TreeItem>
          ))}
        </Tree>
      </div>
    </Section>
  );
}
