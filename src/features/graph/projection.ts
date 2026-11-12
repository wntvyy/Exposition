// Projection of a graph snapshot through a saved view into canvas nodes and
// edges: lifecycle/kind/relation filters, hidden entities (view-only),
// collapsed groups (members fold into the group; their relations re-route to
// it and aggregate), search (non-matches dim, never disappear), and badges.
// Presentation only: nothing here changes or infers semantics.
import type { Entity } from "../../bindings/Entity";
import type { Relation } from "../../bindings/Relation";
import type { CanvasBadge, CanvasEdge, CanvasNode } from "./canvas/types";
import { entityDisplay, relationDisplay, STATUS_LABELS, type SavedView, type VocabularyIndex } from "./model";

/** Structure relations that make one entity a member of a group. */
const MEMBERSHIP: Partial<Record<Relation["kind"], "groupToMember" | "memberToGroup">> = {
  contains: "groupToMember",
  memberOf: "memberToGroup",
};

export interface ProjectionInput {
  entities: Entity[];
  relations: Relation[];
}

export interface Projection {
  nodes: Omit<CanvasNode, "position">[];
  edges: CanvasEdge[];
  /** Entities left out by view filters (kinds, lifecycle). */
  filteredOut: number;
  /** Entities hidden from this view by the user. */
  hidden: number;
  /** Entities folded into collapsed groups. */
  folded: number;
  /** Entities matching the search text (when searching). */
  matches: number;
}

const LIFECYCLE_BADGE: Record<Exclude<Entity["lifecycle"], "active">, CanvasBadge> = {
  archived: { label: "Archived", tone: "neutral", dashed: true },
  discarded: { label: "Discarded", tone: "neutral", dashed: true },
  tombstoned: { label: "Removed from source", tone: "warning", dashed: true, title: "Its file or declaration no longer exists" },
};

/** Extra per-entity badges supplied by the caller (binding state, verdicts, freshness). */
export type BadgeSource = (entity: Entity) => CanvasBadge[];

export function lifecycleBadges(entity: Entity): CanvasBadge[] {
  return entity.lifecycle === "active" ? [] : [LIFECYCLE_BADGE[entity.lifecycle]];
}

export function matchesText(entity: Entity, typeLabel: string, text: string): boolean {
  const query = text.trim().toLowerCase();
  if (!query) return true;
  return [entity.label, typeLabel, entity.key ?? "", entity.kind].some((field) => field.toLowerCase().includes(query));
}

/** Member → group map (direct membership) from structure relations among `ids`. */
export function membership(relations: Relation[], ids: Set<string>, isGroup: (id: string) => boolean): Map<string, string> {
  const parent = new Map<string, string>();
  for (const r of relations) {
    const how = MEMBERSHIP[r.kind];
    if (!how || r.status === "rejected") continue;
    const [group, member] = how === "groupToMember" ? [r.from, r.to] : [r.to, r.from];
    if (!ids.has(group) || !ids.has(member) || group === member || !isGroup(group)) continue;
    if (!parent.has(member)) parent.set(member, group);
  }
  return parent;
}

/** The outermost collapsed ancestor of `id` (itself when not folded). Cycle-safe. */
export function visibleRepresentative(id: string, parent: Map<string, string>, collapsed: Set<string>): string {
  let representative = id;
  const seen = new Set([id]);
  let current = id;
  for (;;) {
    const up = parent.get(current);
    if (!up || seen.has(up)) break;
    seen.add(up);
    if (collapsed.has(up)) representative = up;
    current = up;
  }
  return representative;
}

export function project(
  input: ProjectionInput,
  vocabulary: VocabularyIndex | null,
  view: SavedView,
  extraBadges?: BadgeSource,
): Projection {
  const { filters } = view;
  const hiddenSet = new Set(view.hidden);
  const collapsed = new Set(view.collapsed);
  let filteredOut = 0;
  let hidden = 0;

  const kept = new Map<string, Entity>();
  for (const entity of input.entities) {
    if (!filters.showInactive && entity.lifecycle !== "active") {
      filteredOut++;
      continue;
    }
    if (filters.kinds.length && !filters.kinds.includes(entity.kind)) {
      filteredOut++;
      continue;
    }
    if (hiddenSet.has(entity.id)) {
      hidden++;
      continue;
    }
    kept.set(entity.id, entity);
  }

  const ids = new Set(kept.keys());
  const isGroup = (id: string) => entityDisplay(vocabulary, kept.get(id)!).kind.group;
  const parent = membership(input.relations, ids, isGroup);
  const representative = new Map<string, string>();
  for (const id of ids) representative.set(id, visibleRepresentative(id, parent, collapsed));
  const memberCount = new Map<string, number>();
  for (const [id, rep] of representative) if (rep !== id) memberCount.set(rep, (memberCount.get(rep) ?? 0) + 1);
  const childCount = new Map<string, number>();
  for (const group of parent.values()) childCount.set(group, (childCount.get(group) ?? 0) + 1);

  let matches = 0;
  const nodes: Omit<CanvasNode, "position">[] = [];
  for (const entity of kept.values()) {
    if (representative.get(entity.id) !== entity.id) continue;
    const display = entityDisplay(vocabulary, entity);
    const match = matchesText(entity, display.typeLabel, filters.text);
    if (filters.text.trim() && match) matches++;
    const isCollapsed = collapsed.has(entity.id);
    nodes.push({
      id: entity.id,
      title: entity.label,
      typeLabel: display.typeLabel,
      typeIcon: display.icon,
      category: display.category,
      badges: [...lifecycleBadges(entity), ...(extraBadges?.(entity) ?? [])],
      inactive: entity.lifecycle !== "active",
      dimmed: !match,
      ...(display.kind.group && (childCount.get(entity.id) || isCollapsed)
        ? { group: { collapsed: isCollapsed, memberCount: memberCount.get(entity.id) ?? 0 } }
        : {}),
    });
  }

  const edges = new Map<string, CanvasEdge>();
  for (const r of input.relations) {
    if (r.status === "rejected" && !filters.showInactive) continue;
    if (filters.relationKinds.length && !filters.relationKinds.includes(r.kind)) continue;
    const from = representative.get(r.from);
    const to = representative.get(r.to);
    if (!from || !to || from === to) continue;
    const info = relationDisplay(vocabulary, r.kind);
    const rerouted = from !== r.from || to !== r.to;
    const id = rerouted ? `folded:${from}:${to}:${r.kind}:${r.status}` : r.id;
    const existing = edges.get(id);
    if (existing) {
      existing.count = (existing.count ?? 1) + 1;
      existing.description = describe(kept, from, info.reading, to, r.status, existing.count);
      continue;
    }
    edges.set(id, {
      id,
      source: from,
      target: to,
      kind: r.kind,
      label: info.reading,
      family: info.family,
      status: r.status,
      ...(rerouted ? { count: 1 } : {}),
      description: describe(kept, from, info.reading, to, r.status, 1),
    });
  }

  let folded = 0;
  for (const count of memberCount.values()) folded += count;
  return { nodes, edges: [...edges.values()], filteredOut, hidden, folded, matches };
}

function describe(
  entities: Map<string, Entity>,
  from: string,
  reading: string,
  to: string,
  status: Relation["status"],
  count: number,
): string {
  const name = (id: string) => entities.get(id)?.label ?? id;
  const suffix = status === "accepted" ? "" : `, ${STATUS_LABELS[status].toLowerCase()}`;
  const many = count > 1 ? ` (${count} relationships)` : "";
  return `${name(from)} ${reading} ${name(to)}${suffix}${many}`;
}
