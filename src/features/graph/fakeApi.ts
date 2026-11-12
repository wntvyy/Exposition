// In-memory GraphApi for unit tests. It mimics the backend contract the UI
// relies on (proposal validation by vocabulary endpoint rules, expected-
// revision applies and view saves, events) so view logic can be tested
// without IPC. Never imported by application code.
import type { GraphApi, GraphEvent } from "./api";
import { fallbackEntityKind, fallbackRelationKind } from "./model";
import type {
  ApplyOutcome,
  Diagnostic,
  EntityDetail,
  EntitySummary,
  GraphOperation,
  GraphStatus,
  MathOverview,
  Proposal,
  RelationKindInfo,
  RelationSummary,
  SavedView,
  Snapshot,
  SnapshotQuery,
  Vocabulary,
} from "./model";

export interface FakeGraph {
  api: GraphApi;
  entities: EntitySummary[];
  relations: RelationSummary[];
  views: Map<string, SavedView>;
  revision: () => number;
  /** Calls made, by method name. */
  calls: { method: string; args: unknown[] }[];
  emit(event: GraphEvent): void;
  resync(): void;
}

export function fakeVocabulary(): Vocabulary {
  const entityKinds = new Map(
    ["folder", "collection", "manuscript", "definition", "lemma", "theorem", "proof", "reference", "formal_declaration"].map((id) => {
      const info = fallbackEntityKind(id);
      return [id, { ...info, creatable: ["collection", "lemma", "theorem", "definition"].includes(id) }];
    }),
  );
  const relation = (id: string, from: string[], to: string[], label: string, inverseLabel: string): [string, RelationKindInfo] => [
    id,
    { ...fallbackRelationKind(id), label, inverseLabel, from, to, manual: true },
  ];
  const math = ["definition", "lemma", "theorem"];
  const relationKinds = new Map([
    relation("contains", ["folder"], [], "contains", "is in"),
    relation("member_of", [], ["collection"], "is a member of", "has member"),
    relation("depends_on", math, math, "depends on", "is a dependency of"),
    relation("proof_of", ["proof"], ["lemma", "theorem"], "proves", "is proved by"),
    relation("references", [], [], "references", "is referenced by"),
    relation("formalizes", ["formal_declaration"], math, "formalizes", "is formalized by"),
  ]);
  return { version: "test-1", entityKinds, relationKinds };
}

export function createFakeGraph(seed: { entities?: EntitySummary[]; relations?: RelationSummary[] } = {}): FakeGraph {
  const vocabulary = fakeVocabulary();
  let revision = 1;
  let nextId = 100;
  const entities: EntitySummary[] = [...(seed.entities ?? [])];
  const relations: RelationSummary[] = [...(seed.relations ?? [])];
  const views = new Map<string, SavedView>();
  const calls: FakeGraph["calls"] = [];
  const listeners = new Set<{ onEvent: (e: GraphEvent) => void; onResync: () => void }>();
  const record = (method: string, ...args: unknown[]) => calls.push({ method, args });
  const status = (): GraphStatus => ({
    projectId: "p1",
    availability: { kind: "attached" },
    revision,
    counts: { entities: entities.length, relations: relations.length },
    freshness: { kind: "synchronized" },
  });

  const validate = (operations: GraphOperation[]): Diagnostic[] => {
    const diagnostics: Diagnostic[] = [];
    operations.forEach((op, index) => {
      if (op.op === "addRelation") {
        const info = vocabulary.relationKinds.get(op.kind);
        const from = entities.find((e) => e.id === op.from);
        const to = entities.find((e) => e.id === op.to);
        if (!info || !from || !to) {
          diagnostics.push({ severity: "error", code: "graph.unknown", message: "Unknown relation or entity.", operation: index });
          return;
        }
        if (op.from === op.to) {
          diagnostics.push({ severity: "error", code: "graph.selfRelation", message: "An item cannot relate to itself.", operation: index });
        }
        if ((info.from.length && !info.from.includes(from.kind)) || (info.to.length && !info.to.includes(to.kind))) {
          diagnostics.push({
            severity: "error",
            code: "graph.endpointRule",
            message: `“${info.label}” cannot go from a ${from.kind} to a ${to.kind}.`,
            operation: index,
          });
        }
        if (relations.some((r) => r.kind === op.kind && r.from === op.from && r.to === op.to && r.status !== "rejected")) {
          diagnostics.push({ severity: "error", code: "graph.duplicate", message: "This relationship already exists.", operation: index });
        }
      }
      if (op.op === "createEntity") {
        const info = vocabulary.entityKinds.get(op.kind);
        if (!info?.creatable) {
          diagnostics.push({ severity: "error", code: "graph.notCreatable", message: `A ${op.kind} cannot be created manually.`, operation: index });
        }
        if (!op.label.trim()) diagnostics.push({ severity: "error", code: "graph.emptyLabel", message: "Give it a name.", operation: index });
      }
    });
    return diagnostics;
  };

  const emit = (event: GraphEvent) => listeners.forEach((l) => l.onEvent(event));

  const api: GraphApi = {
    async status() {
      record("status");
      return status();
    },
    async vocabulary() {
      record("vocabulary");
      return vocabulary;
    },
    async snapshot(query: SnapshotQuery): Promise<Snapshot> {
      record("snapshot", query);
      let list = entities;
      if (query.kinds) list = list.filter((e) => query.kinds!.includes(e.kind));
      if (query.around) {
        const keep = new Set([query.around.entity]);
        let frontier = [query.around.entity];
        for (let d = 0; d < query.around.depth; d++) {
          const next: string[] = [];
          for (const r of relations) {
            if (query.around.relationKinds && !query.around.relationKinds.includes(r.kind)) continue;
            const dir = query.around.direction ?? "both";
            if (dir !== "incoming" && frontier.includes(r.from) && !keep.has(r.to)) next.push(r.to);
            if (dir !== "outgoing" && frontier.includes(r.to) && !keep.has(r.from)) next.push(r.from);
          }
          next.forEach((id) => keep.add(id));
          frontier = next;
        }
        list = list.filter((e) => keep.has(e.id));
      }
      const limit = query.limit ?? Infinity;
      const truncated = list.length > limit;
      const shown = list.slice(0, Math.min(list.length, limit));
      const ids = new Set(shown.map((e) => e.id));
      return {
        revision,
        entities: shown,
        relations: relations.filter((r) => ids.has(r.from) && ids.has(r.to)),
        truncated,
        total: list.length,
        coverage: { state: "partial", detail: "Test coverage", unresolved: 0 },
        freshness: { kind: "synchronized" },
      };
    },
    async entity(id: string): Promise<EntityDetail> {
      record("entity", id);
      const entity = entities.find((e) => e.id === id);
      if (!entity) throw new Error(`unknown entity ${id}`);
      const rels = relations.filter((r) => r.from === id || r.to === id);
      const neighborIds = new Set(rels.flatMap((r) => [r.from, r.to]));
      return {
        ...entity,
        bindings: entity.path ? [{ path: entity.path, range: { start: 0, end: 5 }, line: 0, state: entity.binding ?? "resolved", sourceVersion: "v1" }] : [],
        relations: rels,
        neighbors: entities.filter((e) => neighborIds.has(e.id) && e.id !== id),
        evidence: [],
        provenance: [{ origin: entity.origin ?? "indexed", createdAt: null }],
        revision: 1,
      };
    },
    async inspectAtSource(path, offset, rev) {
      record("inspectAtSource", path, offset, rev);
      const found = entities.filter((e) => e.path === path);
      return {
        path,
        offset,
        revision: rev,
        unsaved: false,
        entities: found,
        relations: relations.filter((r) => found.some((e) => e.id === r.from || e.id === r.to)),
        neighbors: entities,
        unresolved: [],
        coverage: { state: "partial", unresolved: 0 },
        freshness: { kind: "synchronized" },
        graphRevision: revision,
      };
    },
    async impact(ids) {
      record("impact", ids);
      const items = relations
        .filter((r) => ids.includes(r.to) && r.kind === "depends_on")
        .map((r) => ({ entity: entities.find((e) => e.id === r.from)!, path: [r.to, r.from], reason: "depends on it", action: "needsReview" as const }));
      return { items, cycles: [], truncated: false, workLimited: false };
    },
    async propose(baseRevision, operations): Promise<Proposal> {
      record("propose", baseRevision, operations);
      const diagnostics = validate(operations);
      return { id: `p${nextId++}`, baseRevision, operations, valid: diagnostics.every((d) => d.severity !== "error"), diagnostics };
    },
    async apply(proposal, expectedRevision): Promise<ApplyOutcome> {
      record("apply", proposal, expectedRevision);
      if (expectedRevision !== revision) return { kind: "conflict", currentRevision: revision };
      const diagnostics = validate(proposal.operations);
      if (diagnostics.some((d) => d.severity === "error")) return { kind: "invalid", diagnostics };
      for (const op of proposal.operations) {
        if (op.op === "addRelation") relations.push({ id: `r${nextId++}`, kind: op.kind, from: op.from, to: op.to, status: "accepted", origin: "user" });
        if (op.op === "removeRelation") relations.splice(relations.findIndex((r) => r.id === op.relation), 1);
        if (op.op === "setRelationStatus") {
          const r = relations.find((x) => x.id === op.relation);
          if (r) r.status = op.status;
        }
        if (op.op === "createEntity") entities.push({ id: `e${nextId++}`, kind: op.kind, label: op.label, lifecycle: "active", binding: null, path: null, origin: "user" });
        if (op.op === "addMember") relations.push({ id: `r${nextId++}`, kind: "member_of", from: op.entity, to: op.collection, status: "accepted", origin: "user" });
      }
      revision += 1;
      emit({ kind: "changed", revision, entities: [], relations: [] });
      return { kind: "applied", revision };
    },
    async views() {
      record("views");
      return [...views.values()].map((v) => ({ id: v.id, name: v.name }));
    },
    async viewGet(id) {
      record("viewGet", id);
      const view = views.get(id);
      return view ? structuredClone(view) : null;
    },
    async viewSave(view) {
      record("viewSave", view);
      const current = views.get(view.id);
      if (current && current.revision !== view.revision) return { kind: "conflict", current: structuredClone(current) };
      const saved = { ...structuredClone(view), revision: (current?.revision ?? 0) + 1 };
      views.set(view.id, saved);
      return { kind: "saved", view: structuredClone(saved) };
    },
    async mathOverview(): Promise<MathOverview> {
      record("mathOverview");
      return {
        revision,
        rows: [],
        relations: [],
        coverage: { state: "partial", unresolved: 0 },
        freshness: { kind: "synchronized" },
        formalDependencies: "unavailable",
      };
    },
    subscribe(onEvent, onResync) {
      const entry = { onEvent, onResync };
      listeners.add(entry);
      return () => listeners.delete(entry);
    },
  };

  return {
    api,
    entities,
    relations,
    views,
    revision: () => revision,
    calls,
    emit,
    resync: () => listeners.forEach((l) => l.onResync()),
  };
}

export function entity(id: string, kind: string, label: string, extra: Partial<EntitySummary> = {}): EntitySummary {
  return { id, kind, label, lifecycle: "active", binding: null, path: null, ...extra };
}

export function relation(id: string, kind: string, from: string, to: string, status: RelationSummary["status"] = "accepted"): RelationSummary {
  return { id, kind, from, to, status, origin: "user" };
}
