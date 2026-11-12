// The Graph feature's model. Records come from the generated IPC bindings
// (Rust owns identities, semantics, admission, validation); this module adds
// the frontend's display layer: a vocabulary index for legends and pickers,
// icons and color categories, and the client-side shape of a saved view.
//
// Endpoint rules here are display aids (ordering a picker); every change is
// still proposed to and validated by the backend.
import {
  Anchor,
  BookMarked,
  BookOpen,
  Braces,
  CircleHelp,
  Database,
  File,
  FileCode,
  FileText,
  FlaskConical,
  FlaskRound,
  Folder,
  FolderKanban,
  Globe,
  Image,
  Library,
  Lightbulb,
  Quote,
  ScrollText,
  Search,
  Settings2,
  Shapes,
  Sigma,
  StickyNote,
  Workflow,
} from "lucide-react";

import type { AssertionStatus } from "../../bindings/AssertionStatus";
import type { Entity } from "../../bindings/Entity";
import type { EntityFamily } from "../../bindings/EntityFamily";
import type { EntityKind } from "../../bindings/EntityKind";
import type { FormalVerdict } from "../../bindings/FormalVerdict";
import type { Freshness } from "../../bindings/Freshness";
import type { Origin } from "../../bindings/Origin";
import type { RelationFamily as BindingRelationFamily } from "../../bindings/RelationFamily";
import type { RelationKind } from "../../bindings/RelationKind";
import type { ResourceRole } from "../../bindings/ResourceRole";
import type { ViewScope } from "../../bindings/ViewScope";
import type { Vocabulary } from "../../bindings/Vocabulary";
import type { IconComponent, Status } from "../../ui/components";
import type { GraphCategory } from "../../ui/tokens";
import type { RelationFamily } from "./canvas/types";

// ----- vocabulary index ----------------------------------------------------------------

export interface EntityKindDisplay {
  kind: EntityKind;
  family: EntityFamily;
  /** Vocabulary title ("Theorem"). */
  title: string;
  category: GraphCategory;
  icon: IconComponent;
  /** Organizes other entities (project, folder, collection, research space). */
  group: boolean;
  /** Shown in the Mathematics subsection (informal math and formal declarations). */
  mathematical: boolean;
  /** A statement a proof can target. */
  statement: boolean;
  formal: boolean;
  /** People may create it through a proposal. */
  creatable: boolean;
  /** Backed by a project file or folder (deleting it is a file operation). */
  fileBacked: boolean;
}

export interface RelationKindDisplay {
  kind: RelationKind;
  family: RelationFamily;
  /** Forward reading: "A depends on B". */
  reading: string;
  /** Reverse reading for incoming relations: "B is a dependency of A". */
  inverse: string;
  /** Plain-language endpoint rule from the vocabulary. */
  rule: string;
  /** Allowed `from→to` kind pairs, as `${from}>${to}`. */
  endpoints: Set<string>;
  /** People may add or remove it. */
  manual: boolean;
}

export interface VocabularyIndex {
  version: number;
  entityKinds: Map<EntityKind, EntityKindDisplay>;
  relationKinds: Map<RelationKind, RelationKindDisplay>;
  roles: ResourceRole[];
}

const STATEMENTS = new Set<EntityKind>(["conjecture", "lemma", "theorem", "proposition", "corollary", "claim"]);
const GROUPS = new Set<EntityKind>(["project", "folder", "collection", "researchSpace"]);

const KIND_ICONS: Partial<Record<EntityKind, IconComponent>> = {
  project: FolderKanban,
  folder: Folder,
  collection: Library,
  researchSpace: FlaskConical,
  resource: File,
  externalResource: Globe,
  definition: BookOpen,
  assumption: Anchor,
  conjecture: CircleHelp,
  lemma: Sigma,
  theorem: Sigma,
  proposition: Sigma,
  corollary: Sigma,
  claim: Quote,
  proof: ScrollText,
  question: CircleHelp,
  mathObject: Shapes,
  formalDeclaration: Braces,
  reference: BookMarked,
  experiment: FlaskRound,
  finding: Lightbulb,
  investigation: Search,
  workflow: Workflow,
};

const FAMILY_CATEGORY: Record<EntityFamily, GraphCategory> = {
  structure: "structure",
  resource: "document",
  mathematics: "math",
  citation: "reference",
  research: "data",
  workflow: "workflow",
};

const ROLE_DISPLAY: Record<ResourceRole, { label: string; icon: IconComponent; category: GraphCategory }> = {
  manuscript: { label: "Manuscript", icon: FileText, category: "document" },
  note: { label: "Note", icon: StickyNote, category: "document" },
  script: { label: "Script", icon: FileCode, category: "document" },
  dataset: { label: "Dataset", icon: Database, category: "data" },
  artifact: { label: "Artifact", icon: Image, category: "data" },
  asset: { label: "Asset", icon: Image, category: "data" },
  formalSource: { label: "Formal source", icon: Braces, category: "formal" },
  bibliography: { label: "Bibliography", icon: BookMarked, category: "reference" },
  configuration: { label: "Configuration", icon: Settings2, category: "structure" },
  other: { label: "File", icon: File, category: "document" },
};

const RELATION_FAMILY: Record<BindingRelationFamily, RelationFamily> = {
  structure: "structure",
  resourceUse: "resource",
  mathematics: "math",
  citation: "citation",
  evidence: "evidence",
  formal: "formal",
};

/** The vocabulary's forward readings, used only before the vocabulary has loaded (display). */
export const READINGS: Record<RelationKind, string> = {
  contains: "contains",
  memberOf: "is a member of",
  anchoredIn: "is declared in",
  imports: "imports",
  usesAsset: "uses",
  dependsOn: "depends on",
  assumes: "assumes",
  proofOf: "is a proof of",
  references: "references",
  evidenceFor: "is evidence for",
  derivedFrom: "is derived from",
  formalizes: "formalizes",
};

/** Display-only reverse readings (the vocabulary gives the forward reading). */
const INVERSE: Record<RelationKind, string> = {
  contains: "is in",
  memberOf: "has member",
  anchoredIn: "declares",
  imports: "is imported by",
  usesAsset: "is used by",
  dependsOn: "is a dependency of",
  assumes: "is assumed by",
  proofOf: "is proved by",
  references: "is referenced by",
  evidenceFor: "is supported by",
  derivedFrom: "has derivative",
  formalizes: "is formalized by",
};

const FALLBACK_FAMILY: Partial<Record<RelationKind, RelationFamily>> = {
  imports: "resource",
  usesAsset: "resource",
  dependsOn: "math",
  assumes: "math",
  proofOf: "math",
  references: "citation",
  evidenceFor: "evidence",
  derivedFrom: "evidence",
  formalizes: "formal",
};

export function indexVocabulary(vocabulary: Vocabulary): VocabularyIndex {
  const entityKinds = new Map<EntityKind, EntityKindDisplay>();
  for (const info of vocabulary.entityKinds) {
    entityKinds.set(info.kind, {
      kind: info.kind,
      family: info.family,
      title: info.title,
      category: info.kind === "formalDeclaration" ? "formal" : FAMILY_CATEGORY[info.family],
      icon: KIND_ICONS[info.kind] ?? Shapes,
      group: GROUPS.has(info.kind),
      mathematical: info.family === "mathematics",
      statement: STATEMENTS.has(info.kind),
      formal: info.kind === "formalDeclaration",
      creatable: info.creatable,
      fileBacked: info.kind === "resource" || info.kind === "folder",
    });
  }
  const relationKinds = new Map<RelationKind, RelationKindDisplay>();
  for (const info of vocabulary.relationKinds) {
    relationKinds.set(info.kind, {
      kind: info.kind,
      family: RELATION_FAMILY[info.family],
      reading: info.reading,
      inverse: INVERSE[info.kind],
      rule: info.rule,
      endpoints: new Set(info.endpoints.map(([from, to]) => `${from}>${to}`)),
      manual: info.manual,
    });
  }
  return { version: vocabulary.version, entityKinds, relationKinds, roles: vocabulary.resourceRoles };
}

/** "researchSpace" → "Research space". */
export function humanize(id: string): string {
  const words = id.replace(/([a-z])([A-Z])/g, "$1 $2").replace(/[_-]+/g, " ").trim().toLowerCase();
  return words.charAt(0).toUpperCase() + words.slice(1);
}

export function kindDisplay(index: VocabularyIndex | null, kind: EntityKind): EntityKindDisplay {
  const known = index?.entityKinds.get(kind);
  if (known) return known;
  return {
    kind,
    family: "structure",
    title: humanize(kind),
    category: "structure",
    icon: KIND_ICONS[kind] ?? Shapes,
    group: GROUPS.has(kind),
    mathematical: false,
    statement: STATEMENTS.has(kind),
    formal: kind === "formalDeclaration",
    creatable: false,
    fileBacked: kind === "resource" || kind === "folder",
  };
}

export function relationDisplay(index: VocabularyIndex | null, kind: RelationKind): RelationKindDisplay {
  return (
    index?.relationKinds.get(kind) ?? {
      kind,
      family: FALLBACK_FAMILY[kind] ?? "structure",
      reading: READINGS[kind] ?? humanize(kind).toLowerCase(),
      inverse: INVERSE[kind] ?? `${humanize(kind).toLowerCase()} (inverse)`,
      rule: "",
      endpoints: new Set(),
      manual: false,
    }
  );
}

/** Type label, icon, and color of one entity (resources by their first role). */
export function entityDisplay(index: VocabularyIndex | null, entity: Pick<Entity, "kind" | "roles">) {
  const kind = kindDisplay(index, entity.kind);
  const role = entity.kind === "resource" ? entity.roles[0] : undefined;
  if (role) {
    const r = ROLE_DISPLAY[role];
    return { typeLabel: r.label, icon: r.icon, category: r.category, kind };
  }
  return { typeLabel: kind.title, icon: kind.icon, category: kind.category, kind };
}

export function roleLabel(role: ResourceRole): string {
  return ROLE_DISPLAY[role].label;
}

/** Display ordering only: does the vocabulary list this endpoint pair? */
export function endpointAllowed(relation: RelationKindDisplay, from: EntityKind, to: EntityKind): boolean {
  return relation.endpoints.has(`${from}>${to}`);
}

export const FAMILY_LABELS: Record<RelationFamily, string> = {
  structure: "Structure",
  resource: "Resource use",
  math: "Mathematical dependencies",
  citation: "References",
  evidence: "Evidence and provenance",
  formal: "Formal association",
  execution: "Execution",
};

export const STATUS_LABELS: Record<AssertionStatus, string> = {
  accepted: "Accepted",
  suggested: "Suggested",
  observed: "Observed",
  rejected: "Rejected",
};

export function originLabel(origin: Origin): string {
  switch (origin.kind) {
    case "extracted":
      return `Extracted (${origin.extractor})`;
    case "user":
      return "You";
    case "agent":
      return "Agent task";
    case "preset":
      return `Preset ${origin.blueprint}`;
    case "service":
      return `Service (${origin.service})`;
  }
}

// ----- verification vocabulary ------------------------------------------------------------

/**
 * Exact verifier verdict labels (the Lean status vocabulary of
 * `src/features/lean/status.ts`); only a clean result is ever "Checked".
 */
export const VERDICT_VIEW: Record<FormalVerdict, { label: string; status: Status; sentence: string }> = {
  checked: { label: "Checked", status: "succeeded", sentence: "Checked by Lean's kernel with only the standard axioms." },
  checkedWithAxioms: {
    label: "Checked with axioms",
    status: "warning",
    sentence: "Checked, but it depends on axioms beyond Lean's standard ones; it holds only if they do.",
  },
  incomplete: { label: "Incomplete (sorry)", status: "warning", sentence: "The proof is incomplete: it depends on sorry." },
  reliesOnCompiledCode: {
    label: "Relies on compiled code",
    status: "warning",
    sentence: "Checked by trusting compiled code (native_decide or similar) instead of the kernel.",
  },
  partiallyChecked: { label: "Partially checked", status: "warning", sentence: "Only part of the file could be checked." },
  notVerified: { label: "Not verified", status: "warning", sentence: "The audit could not verify this result." },
  errors: { label: "Errors", status: "failed", sentence: "Lean reported errors." },
};

/** Freshness as a status: stale and unknown are never shown as success. */
export function freshnessView(freshness: Freshness | null): { label: string; status: Status; detail: string } {
  if (!freshness) return { label: "No evidence", status: "idle", detail: "There is no evidence to be current or stale." };
  switch (freshness.kind) {
    case "current":
      return { label: "Current", status: "idle", detail: "The evidence matches the current sources." };
    case "stale":
      return { label: "Stale", status: "stale", detail: freshness.reasons.join("; ") || "A relevant source changed." };
    case "unknown":
      return { label: "Unknown", status: "unavailable", detail: freshness.reason };
  }
}

// ----- saved views (client shape) ------------------------------------------------------------

export interface ClientFilters {
  /** Entity kinds to show (empty = all). */
  kinds: EntityKind[];
  /** Relation kinds to show (empty = all). */
  relationKinds: RelationKind[];
  text: string;
  /** Show archived/discarded/tombstoned entities and rejected suggestions. */
  showInactive: boolean;
}

export interface NodeLayout {
  x: number;
  y: number;
  width?: number;
  height?: number;
}

export interface SavedView {
  /** Backend id; null until first saved. */
  id: string | null;
  name: string;
  scope: ViewScope;
  /** Expected revision for the next save; null = create. */
  revision: number | null;
  positions: Record<string, NodeLayout>;
  viewport: { x: number; y: number; zoom: number } | null;
  filters: ClientFilters;
  hidden: string[];
  collapsed: string[];
}

export const defaultFilters: ClientFilters = { kinds: [], relationKinds: [], text: "", showInactive: false };

/** Local dependency focus (transient, not saved): show `depth` relations around an entity. */
export interface ViewFocus {
  entity: string;
  depth: number;
  direction: "outgoing" | "incoming" | "both";
}
