// In-memory GraphApi for unit tests. It mimics the parts of the backend
// contract the UI relies on (proposals validated by endpoint rules, manual
// kinds and duplicates; applies by proposal id; expected-revision view saves
// with `graph.staleView`; events) so view logic can be tested without IPC.
// Never imported by application code.
import type { ApplyOutcome } from "../../bindings/ApplyOutcome";
import type { ChangeProposal } from "../../bindings/ChangeProposal";
import type { Entity } from "../../bindings/Entity";
import type { EntityKind } from "../../bindings/EntityKind";
import type { GraphDiagnostic } from "../../bindings/GraphDiagnostic";
import type { GraphEvent } from "../../bindings/GraphEvent";
import type { GraphFreshness } from "../../bindings/GraphFreshness";
import type { GraphOperation } from "../../bindings/GraphOperation";
import type { GraphStatus } from "../../bindings/GraphStatus";
import type { GraphView } from "../../bindings/GraphView";
import type { MathOverview } from "../../bindings/MathOverview";
import type { MathRow } from "../../bindings/MathRow";
import type { Relation } from "../../bindings/Relation";
import type { RelationKind } from "../../bindings/RelationKind";
import type { Vocabulary } from "../../bindings/Vocabulary";
import type { GraphApi } from "./api";
import { indexVocabulary } from "./model";

const MATH: EntityKind[] = ["definition", "assumption", "conjecture", "lemma", "theorem", "proposition", "corollary", "claim", "proof", "question", "mathObject"];
const STATEMENT: EntityKind[] = ["conjecture", "lemma", "theorem", "proposition", "corollary", "claim"];
const ALL: EntityKind[] = ["project", "folder", "collection", "researchSpace", "resource", "externalResource", ...MATH, "formalDeclaration", "reference", "experiment", "finding", "investigation", "workflow"];

const pairs = (from: EntityKind[], to: EntityKind[]): [EntityKind, EntityKind][] => from.flatMap((a) => to.map((b): [EntityKind, EntityKind] => [a, b]));

export function fakeVocabulary(): Vocabulary {
  const titles: Partial<Record<EntityKind, string>> = { researchSpace: "Research space", mathObject: "Mathematical object", formalDeclaration: "Formal declaration", externalResource: "External resource" };
  return {
    version: 1,
    entityKinds: ALL.map((kind) => ({
      kind,
      family: ["project", "folder", "collection", "researchSpace"].includes(kind)
        ? "structure"
        : kind === "resource" || kind === "externalResource"
          ? "resource"
          : MATH.includes(kind) || kind === "formalDeclaration"
            ? "mathematics"
            : kind === "reference"
              ? "citation"
              : kind === "workflow"
                ? "workflow"
                : "research",
      title: titles[kind] ?? kind.charAt(0).toUpperCase() + kind.slice(1),
      creatable: !["project", "folder", "resource", "researchSpace", "formalDeclaration"].includes(kind),
    })),
    resourceRoles: ["manuscript", "note", "script", "dataset", "artifact", "asset", "formalSource", "bibliography", "configuration", "other"],
    relationKinds: [
      { kind: "contains", family: "structure", reading: "contains", rule: "A folder contains things.", endpoints: pairs(["project", "folder"], ["folder", "resource", "collection"]), manual: false },
      { kind: "memberOf", family: "structure", reading: "is a member of", rule: "Anything joins a collection.", endpoints: pairs(ALL.filter((k) => k !== "project"), ["collection"]), manual: true },
      { kind: "dependsOn", family: "mathematics", reading: "depends on", rule: "A mathematical entity (not a proof) depends on another (not a question).", endpoints: pairs(MATH.filter((k) => k !== "proof"), MATH.filter((k) => k !== "question")), manual: true },
      { kind: "assumes", family: "mathematics", reading: "assumes", rule: "A statement assumes an assumption.", endpoints: pairs(STATEMENT, ["assumption", "conjecture"]), manual: true },
      { kind: "proofOf", family: "mathematics", reading: "is a proof of", rule: "A proof targets a statement.", endpoints: pairs(["proof"], STATEMENT), manual: true },
      { kind: "references", family: "citation", reading: "references", rule: "Anything citable.", endpoints: pairs([...MATH, "resource"], ALL), manual: true },
      { kind: "formalizes", family: "formal", reading: "formalizes", rule: "A formal declaration formalizes a statement.", endpoints: pairs(["formalDeclaration"], STATEMENT), manual: true },
    ],
  };
}

export interface FakeGraph {
  api: GraphApi;
  entities: Entity[];
  relations: Relation[];
  views: Map<string, GraphView>;
  mathRows: MathRow[];
  revision: () => number;
  calls: { method: string; args: unknown[] }[];
  emit(event: GraphEvent): void;
  resync(): void;
}

const freshness: GraphFreshness = { state: "synchronized", pending: 0, checkpointMs: null, overlay: [] };
const coverage = { resources: 1, complete: 0, partial: 1, unsupported: 0, pending: 0, failed: 0, unresolvedBindings: 0 };

export function entity(id: string, kind: EntityKind, label: string, extra: Partial<Entity> = {}): Entity {
  return {
    id,
    kind,
    roles: [],
    label,
    key: null,
    lifecycle: "active",
    tombstoneReason: null,
    revision: 1,
    origin: { kind: "user" },
    attributes: {},
    createdAtMs: 0,
    updatedAtMs: 0,
    ...extra,
  };
}

export function relation(id: string, kind: RelationKind, from: string, to: string, status: Relation["status"] = "accepted"): Relation {
  return {
    id,
    kind,
    from,
    to,
    origin: status === "observed" ? { kind: "extracted", extractor: "test" } : { kind: "user" },
    status,
    evidenceKind: null,
    evidence: [],
    fromVersion: null,
    toVersion: null,
    note: null,
    revision: 1,
    createdAtMs: 0,
    updatedAtMs: 0,
  };
}

export function createFakeGraph(seed: { entities?: Entity[]; relations?: Relation[]; mathRows?: MathRow[] } = {}): FakeGraph {
  const vocabulary = fakeVocabulary();
  const index = indexVocabulary(vocabulary);
  let revision = 1;
  let nextId = 100;
  const entities: Entity[] = [...(seed.entities ?? [])];
  const relations: Relation[] = [...(seed.relations ?? [])];
  const mathRows: MathRow[] = [...(seed.mathRows ?? [])];
  const views = new Map<string, GraphView>();
  const proposals = new Map<string, ChangeProposal>();
  const calls: FakeGraph["calls"] = [];
  const listeners = new Set<{ onEvent: (e: GraphEvent) => void; onResync: () => void }>();
  const record = (method: string, ...args: unknown[]) => calls.push({ method, args });
  const status = (): GraphStatus => ({
    state: { kind: "attached" },
    project: "p1",
    root: "C:/project",
    schemaVersion: 1,
    revision,
    entities: entities.length,
    relations: relations.length,
    freshness,
    coverage,
  });
  const resolve = (ref: { kind: "id"; id: string } | { kind: "temp"; tempId: string }, temps: Map<string, EntityKind>) =>
    ref.kind === "id" ? entities.find((e) => e.id === ref.id)?.kind : temps.get(ref.tempId);

  const validate = (operations: GraphOperation[]): GraphDiagnostic[] => {
    const diagnostics: GraphDiagnostic[] = [];
    const temps = new Map<string, EntityKind>();
    const err = (operation: number, code: string, message: string) => diagnostics.push({ operation, code, message, severity: "error" });
    operations.forEach((op, i) => {
      if (op.op === "createEntity") {
        if (!index.entityKinds.get(op.kind)?.creatable) err(i, "graph.notCreatable", `a ${op.kind} cannot be created directly`);
        if (!op.label.trim()) err(i, "graph.invalidLabel", "the label is empty");
        temps.set(op.tempId, op.kind);
      }
      if (op.op === "addRelation") {
        const info = index.relationKinds.get(op.kind)!;
        const from = resolve(op.from, temps);
        const to = resolve(op.to, temps);
        if (!info.manual) return err(i, "graph.notManual", `${op.kind} relationships come from the sources`);
        if (!from || !to) return err(i, "graph.unknownEntity", "unknown entity");
        if (op.from.kind === "id" && op.to.kind === "id" && op.from.id === op.to.id) return err(i, "graph.selfRelation", "an entity cannot relate to itself");
        if (!info.endpoints.has(`${from}>${to}`)) return err(i, "graph.endpointRule", `${info.reading} cannot go from a ${from} to a ${to}: ${info.rule}`);
        if (op.from.kind === "id" && op.to.kind === "id") {
          const [a, b] = [op.from.id, op.to.id];
          if (relations.some((r) => r.kind === op.kind && r.from === a && r.to === b && r.status !== "rejected")) {
            err(i, "graph.duplicateRelation", "this relationship already exists");
          }
        }
      }
      if (op.op === "removeRelation" || op.op === "setRelationStatus") {
        const r = relations.find((x) => x.id === op.relation);
        if (!r) return err(i, "graph.unknownRelation", "relation does not exist");
        if (r.status === "observed") err(i, "graph.observed", "this relationship was observed in the sources; change the source instead");
      }
    });
    if (operations.length === 0) err(0, "graph.empty", "nothing to change");
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
      return index;
    },
    async snapshot(query) {
      record("snapshot", query);
      let list = query.includeInactive ? entities : entities.filter((e) => e.lifecycle === "active");
      if (query.kinds) list = list.filter((e) => query.kinds!.includes(e.kind));
      if (query.text) list = list.filter((e) => e.label.toLowerCase().includes(query.text!.toLowerCase()));
      if (query.around) {
        const { entity: start, depth, relationKinds, direction } = query.around;
        const keep = new Set([start]);
        let frontier = [start];
        for (let d = 0; d < depth; d++) {
          const next: string[] = [];
          for (const r of relations) {
            if (relationKinds && !relationKinds.includes(r.kind)) continue;
            if (direction !== "incoming" && frontier.includes(r.from) && !keep.has(r.to)) next.push(r.to);
            if (direction !== "outgoing" && frontier.includes(r.to) && !keep.has(r.from)) next.push(r.from);
          }
          next.forEach((id) => keep.add(id));
          frontier = next;
        }
        list = list.filter((e) => keep.has(e.id));
      }
      const limit = query.limit ?? 300;
      const shown = list.slice(0, limit);
      const ids = new Set(shown.map((e) => e.id));
      return {
        project: "p1",
        revision,
        entities: shown,
        relations: relations.filter((r) => ids.has(r.from) && ids.has(r.to)),
        truncated: list.length > limit,
        total: list.length,
        coverage,
        freshness,
      };
    },
    async entity(id) {
      record("entity", id);
      const e = entities.find((x) => x.id === id);
      if (!e) throw { code: "graph.unknownEntity", message: `entity ${id} does not exist` };
      const outgoing = relations.filter((r) => r.from === id);
      const incoming = relations.filter((r) => r.to === id);
      const neighborIds = new Set([...outgoing.map((r) => r.to), ...incoming.map((r) => r.from)]);
      return {
        entity: e,
        bindings: e.key
          ? [{ entity: id, path: e.key, selector: { kind: "wholeFile" }, range: null, version: null, fingerprint: null, state: "resolved", detail: null }]
          : [],
        outgoing,
        incoming,
        neighbors: entities.filter((x) => neighborIds.has(x.id)),
        evidence: [],
        evidenceFreshness: [],
        importedEvidence: [],
        coverage: null,
        graphRevision: revision,
      };
    },
    async inspectAtSource(request) {
      record("inspectAtSource", request);
      const resource = entities.find((e) => e.kind === "resource" && e.key === request.path) ?? null;
      const bound = entities.filter((e) => e.attributes.path === request.path);
      return {
        path: request.path,
        revision: request.revision,
        unsaved: false,
        resource,
        entities: bound.map((e) => {
          const rels = relations.filter((r) => r.from === e.id || r.to === e.id);
          const relatedIds = new Set(rels.flatMap((r) => [r.from, r.to]));
          return {
            entity: e,
            binding: { entity: e.id, path: request.path, selector: { kind: "region" }, range: { start: 0, end: 10 }, version: null, fingerprint: null, state: "resolved", detail: null },
            relations: rels,
            related: entities.filter((x) => relatedIds.has(x.id) && x.id !== e.id),
          };
        }),
        coverage: { path: request.path, extractor: "latex/1", state: "partial", unresolved: 0, detail: "Labels and theorem environments only", contentHash: null },
        freshness,
        graphRevision: revision,
      };
    },
    async impact(request) {
      record("impact", request);
      const items = relations
        .filter((r) => request.entities.includes(r.to) && r.kind === "dependsOn")
        .map((r) => ({
          entity: entities.find((e) => e.id === r.from)!,
          depth: 1,
          path: [{ relation: r.id, kind: r.kind, from: r.from, to: r.to }],
          action: "needsReview" as const,
          reason: "depends on a changed entity",
        }));
      return { roots: request.entities, items, cycles: [], truncated: false, workLimitReached: false, unknownCoverage: [], evidence: [], graphRevision: revision };
    },
    async propose(request) {
      record("propose", request);
      const diagnostics = validate(request.operations);
      const proposal: ChangeProposal = {
        id: `p${nextId++}`,
        baseRevision: request.baseRevision,
        origin: { kind: "user" },
        operations: request.operations,
        diagnostics,
        valid: diagnostics.every((d) => d.severity !== "error"),
        effects: request.operations.map((op) => `Effect: ${op.op}`),
        createdAtMs: 0,
      };
      proposals.set(proposal.id, proposal);
      return proposal;
    },
    async apply(request): Promise<ApplyOutcome> {
      record("apply", request);
      const proposal = proposals.get(request.proposal);
      if (!proposal) return { kind: "invalid", currentRevision: revision, diagnostics: [{ operation: null, code: "graph.unknownProposal", message: "unknown proposal", severity: "error" }] };
      const diagnostics = validate(proposal.operations);
      if (diagnostics.some((d) => d.severity === "error")) return { kind: "invalid", currentRevision: revision, diagnostics };
      const temps = new Map<string, string>();
      const created: { tempId: string; id: string }[] = [];
      const added: string[] = [];
      for (const op of proposal.operations) {
        if (op.op === "createEntity") {
          const id = `e${nextId++}`;
          temps.set(op.tempId, id);
          created.push({ tempId: op.tempId, id });
          entities.push(entity(id, op.kind, op.label));
        }
        if (op.op === "addRelation") {
          const from = op.from.kind === "id" ? op.from.id : temps.get(op.from.tempId)!;
          const to = op.to.kind === "id" ? op.to.id : temps.get(op.to.tempId)!;
          const id = `r${nextId++}`;
          added.push(id);
          relations.push(relation(id, op.kind, from, to));
        }
        if (op.op === "removeRelation") relations.splice(relations.findIndex((r) => r.id === op.relation), 1);
        if (op.op === "setRelationStatus") relations.find((r) => r.id === op.relation)!.status = op.status;
        if (op.op === "setLifecycle") entities.find((e) => e.id === op.entity)!.lifecycle = op.lifecycle;
      }
      revision += 1;
      const delta = { revision, entitiesAdded: created.map((c) => c.id), entitiesUpdated: [], entitiesRemoved: [], relationsAdded: added, relationsUpdated: [], relationsRemoved: [], truncated: false };
      emit({ kind: "changed", delta, origin: { kind: "user" } });
      return { kind: "applied", revision, rebased: false, created, delta };
    },
    async views() {
      record("views");
      return [...views.values()].map((v) => ({ id: v.id, name: v.name, scope: v.scope, revision: v.revision, updatedAtMs: 0 }));
    },
    async viewGet(id) {
      record("viewGet", id);
      const view = views.get(id);
      if (!view) throw { code: "graph.unknownView", message: "no such view" };
      return structuredClone(view);
    },
    async viewSave(input) {
      record("viewSave", input);
      const current = input.id ? views.get(input.id) : undefined;
      if (input.id && !current) throw { code: "graph.unknownView", message: "no such view" };
      if (current && current.revision !== input.expectedRevision) throw { code: "graph.staleView", message: "the view changed" };
      const id = input.id ?? `v${nextId++}`;
      const saved: GraphView = {
        id,
        name: input.name,
        scope: input.scope,
        nodes: structuredClone(input.nodes),
        viewport: { ...input.viewport },
        filters: structuredClone(input.filters),
        hidden: [...input.hidden],
        collapsed: [...input.collapsed],
        revision: (current?.revision ?? 0) + 1,
        updatedAtMs: 0,
      };
      views.set(id, saved);
      return structuredClone(saved);
    },
    async mathOverview(): Promise<MathOverview> {
      record("mathOverview");
      const ids = new Set(mathRows.map((r) => r.entity.id));
      return {
        rows: mathRows,
        relations: relations.filter((r) => ids.has(r.from) && ids.has(r.to)),
        truncated: false,
        coverage,
        graphRevision: revision,
      };
    },
    async recover() {
      record("recover");
      return status();
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
    mathRows,
    revision: () => revision,
    calls,
    emit,
    resync: () => listeners.forEach((l) => l.onResync()),
  };
}
