// Keyboard-accessible alternative to the canvas: the same projected entities
// and relationships as sortable tables. Selecting a row selects the item in
// the inspector, which offers every canvas action except dragging.
import type { Entity } from "../../bindings/Entity";
import type { Relation } from "../../bindings/Relation";
import { Badge, DataTable, type DataTableColumn } from "../../ui/components";
import type { CanvasBadge, CanvasNode } from "./canvas/types";
import { RelationStatusBadge } from "./Inspector";
import { relationDisplay, type VocabularyIndex } from "./model";
import type { Projection } from "./projection";

type Row = Omit<CanvasNode, "position"> & { entity: Entity | undefined; relationCount: number };

function Badges({ badges }: { badges: CanvasBadge[] }) {
  if (!badges.length) return <span className="xp-muted">—</span>;
  return (
    <span className="xp-graph-badges">
      {badges.map((b) => (
        <Badge key={b.label} tone={b.tone} dashed={b.dashed} title={b.title}>
          {b.label}
        </Badge>
      ))}
    </span>
  );
}

export function EntityTable({
  projection,
  entities,
  selected,
  onSelect,
  onActivate,
}: {
  projection: Projection;
  entities: Map<string, Entity>;
  selected: string | null;
  onSelect: (id: string | null) => void;
  onActivate?: (id: string) => void;
}) {
  const counts = new Map<string, number>();
  for (const e of projection.edges) {
    counts.set(e.source, (counts.get(e.source) ?? 0) + (e.count ?? 1));
    counts.set(e.target, (counts.get(e.target) ?? 0) + (e.count ?? 1));
  }
  const rows: Row[] = projection.nodes.map((n) => ({ ...n, entity: entities.get(n.id), relationCount: counts.get(n.id) ?? 0 }));
  const columns: DataTableColumn<Row>[] = [
    {
      id: "name",
      title: "Name",
      isRowHeader: true,
      sortValue: (r) => r.title,
      render: (r) => (
        <span className="xp-graph-table__name">
          {r.typeIcon && <r.typeIcon size={14} aria-hidden />}
          {r.title}
          {r.group?.collapsed ? <span className="xp-muted"> ({r.group.memberCount} inside, collapsed)</span> : null}
        </span>
      ),
    },
    { id: "kind", title: "Kind", sortValue: (r) => r.typeLabel, render: (r) => r.typeLabel, width: "16%" },
    { id: "status", title: "Status", sortValue: (r) => r.badges.map((b) => b.label).join(), render: (r) => <Badges badges={r.badges} />, width: "22%" },
    { id: "key", title: "Source or key", sortValue: (r) => r.entity?.key ?? "", render: (r) => (r.entity?.key ? <code className="xp-graph-path">{r.entity.key}</code> : <span className="xp-muted">—</span>), width: "24%" },
    { id: "relations", title: "Relations", sortValue: (r) => r.relationCount, render: (r) => String(r.relationCount), width: "10%" },
  ];
  return (
    <DataTable
      aria-label="Entities"
      className="xp-graph-table"
      columns={columns}
      rows={rows}
      rowKey={(r) => r.id}
      selectedKey={selected}
      onSelectionChange={onSelect}
      onRowAction={onActivate}
      defaultSort={{ column: "name", direction: "ascending" }}
      rowProps={(r) => ({ "data-entity-id": r.id })}
      empty="No items match this view. Change the filters or search."
    />
  );
}

interface RelationRow {
  id: string;
  relation: Relation | undefined;
  from: string;
  to: string;
  reading: string;
  status: Relation["status"];
  count: number;
}

export function RelationTable({
  projection,
  entities,
  relations,
  vocabulary,
  selected,
  onSelect,
}: {
  projection: Projection;
  entities: Map<string, Entity>;
  relations: Map<string, Relation>;
  vocabulary: VocabularyIndex | null;
  selected: string | null;
  onSelect: (id: string | null) => void;
}) {
  const label = (id: string) => entities.get(id)?.label ?? id;
  const rows: RelationRow[] = projection.edges.map((e) => ({
    id: e.id,
    relation: relations.get(e.id),
    from: label(e.source),
    to: label(e.target),
    reading: relationDisplay(vocabulary, e.kind as Relation["kind"]).reading,
    status: e.status,
    count: e.count ?? 1,
  }));
  const columns: DataTableColumn<RelationRow>[] = [
    { id: "from", title: "From", isRowHeader: true, sortValue: (r) => r.from, render: (r) => r.from },
    { id: "relation", title: "Relationship", sortValue: (r) => r.reading, render: (r) => (r.count > 1 ? `${r.reading} (×${r.count}, collapsed)` : r.reading) },
    { id: "to", title: "To", sortValue: (r) => r.to, render: (r) => r.to },
    { id: "status", title: "Status", sortValue: (r) => r.status, render: (r) => <RelationStatusBadge status={r.status} />, width: "14%" },
  ];
  return (
    <DataTable
      aria-label="Relationships"
      className="xp-graph-table"
      columns={columns}
      rows={rows}
      rowKey={(r) => r.id}
      selectedKey={selected}
      onSelectionChange={onSelect}
      defaultSort={{ column: "from", direction: "ascending" }}
      rowProps={(r) => ({ "data-relation-id": r.id })}
      empty="No relationships in this view."
    />
  );
}
