// The Graph feature's normalized model of backend graph data. `api.ts` maps the
// generated IPC bindings into these shapes; every view consumes only these.
//
// Semantics (which relations exist, endpoint rules, admission, validation)
// come from the backend vocabulary and proposals. The tables at the end of
// this file are display only: icon, color category, and a fallback label for
// a kind id the vocabulary did not describe.
import {
  Anchor,
  BookMarked,
  BookOpen,
  Braces,
  CircleHelp,
  Database,
  FileCode,
  FileText,
  FlaskConical,
  FlaskRound,
  Folder,
  FolderKanban,
  Image,
  Library,
  Lightbulb,
  Quote,
  ScrollText,
  Search,
  Shapes,
  Sigma,
  StickyNote,
  Table2,
  Workflow,
} from "lucide-react";

import type { IconComponent } from "../../ui/components";
import type { GraphCategory } from "../../ui/tokens";
import type { AssertionStatus, RelationFamily } from "./canvas/types";

export type { AssertionStatus, RelationFamily };

// ----- status ------------------------------------------------------------------

export type GraphFreshness =
  | { kind: "synchronized" }
  | { kind: "reconciling" }
  | { kind: "pending"; count: number }
  | { kind: "unavailable"; reason?: string };

export interface RecoveryOption {
  id: string;
  label: string;
  description?: string;
}

export type GraphAvailability =
  | { kind: "attached" }
  | { kind: "temporaryReadOnly"; reason: string }
  | { kind: "unavailable"; reason: string; recovery: RecoveryOption[] }
  | { kind: "detached" };

export interface GraphStatus {
  projectId: string | null;
  availability: GraphAvailability;
  revision: number;
  counts: { entities: number; relations: number };
  freshness: GraphFreshness;
}

// ----- vocabulary ----------------------------------------------------------------

export interface EntityKindInfo {
  id: string;
  label: string;
  /** Plural label for lists ("Theorems"). */
  plural: string;
  description?: string;
  category: GraphCategory;
  icon: IconComponent;
  /** Can organize other entities (folder, collection, research space). */
  group: boolean;
  /** Shown in the Mathematics subsection. */
  mathematical: boolean;
  /** A formal declaration (e.g. a Lean declaration). */
  formal: boolean;
  /** Users may create it manually through a proposal. */
  creatable: boolean;
  /** Backed by a project file (its deletion is a file operation, not a graph edit). */
  fileBacked: boolean;
}

export interface RelationKindInfo {
  id: string;
  family: RelationFamily;
  /** Forward reading: "A depends on B". */
  label: string;
  /** Reverse reading: "B is a dependency of A". */
  inverseLabel: string;
  description?: string;
  /** Allowed source and target entity kinds (empty = any). */
  from: string[];
  to: string[];
  /** Users may add it manually. */
  manual: boolean;
}

export interface Vocabulary {
  version: string;
  entityKinds: Map<string, EntityKindInfo>;
  relationKinds: Map<string, RelationKindInfo>;
}

// ----- entities, relations, snapshots ---------------------------------------------

export type Lifecycle = "active" | "archived";
export type BindingState = "resolved" | "missing" | "ambiguous";

export interface TextSpan {
  start: number;
  end: number;
}

export interface SourceBinding {
  path: string;
  range: TextSpan | null;
  /** 0-based line of the range start, when known. */
  line: number | null;
  state: BindingState;
  sourceVersion: string | null;
  /** Inspected from an unsaved buffer (revision-labelled overlay), not the saved file. */
  unsaved?: boolean;
}

export interface EntitySummary {
  id: string;
  kind: string;
  label: string;
  lifecycle: Lifecycle;
  /** Primary binding state (worst of its bindings), null when unbound. */
  binding: BindingState | null;
  /** Primary path, when file-backed or bound. */
  path: string | null;
  /** Evidence is stale for current inputs. */
  stale?: boolean;
  /** Origin of the entity: indexed, user, agent, preset. */
  origin?: string;
}

export interface RelationSummary {
  id: string;
  kind: string;
  from: string;
  to: string;
  status: AssertionStatus;
  origin: string;
}

export type CoverageState = "complete" | "partial" | "unknown" | "unsupported";

export interface Coverage {
  state: CoverageState;
  /** What was or was not extracted, e.g. "LaTeX labels and theorem environments". */
  detail?: string;
  /** Files whose extraction is partial/unknown. */
  unresolved: number;
}

export interface Snapshot {
  revision: number;
  entities: EntitySummary[];
  relations: RelationSummary[];
  /** The result was cut to the query limit. */
  truncated: boolean;
  /** Total matching entities, when known (> entities.length when truncated). */
  total: number | null;
  coverage: Coverage;
  freshness: GraphFreshness;
}

export type SnapshotDirection = "outgoing" | "incoming" | "both";

export interface SnapshotQuery {
  kinds?: string[];
  text?: string;
  around?: { entity: string; depth: number; relationKinds?: string[]; direction?: SnapshotDirection };
  limit?: number;
  includeArchived?: boolean;
}

export interface EvidenceRecord {
  id: string;
  kind: string;
  /** Human summary: "Lean audit of RunCheck at rev 4". */
  summary: string;
  outcome: string | null;
  recordedAt: string | null;
  /** Current / stale / unknown applicability for current inputs. */
  freshness: "current" | "stale" | "unknown";
  /** Lean evidence: the audited declaration, its printed statement and axioms. */
  formal?: {
    declaration: string;
    statement: string | null;
    axioms: string[];
    verdict: VerifierVerdict | null;
    toolchain: string | null;
    /** Dependency edges the checker exposed; null = not exposed by the evidence. */
    dependencies: string[] | null;
  };
}

export interface EntityDetail extends EntitySummary {
  description?: string;
  bindings: SourceBinding[];
  relations: RelationSummary[];
  /** Other endpoints of `relations`, for labels. */
  neighbors: EntitySummary[];
  evidence: EvidenceRecord[];
  provenance: { origin: string; createdAt: string | null; detail?: string }[];
  revision: number;
}

// ----- source inspection ---------------------------------------------------------------

export interface SourceInspection {
  path: string;
  offset: number;
  revision: number | null;
  /** Answered from the unsaved buffer overlay rather than the saved file. */
  unsaved: boolean;
  entities: EntitySummary[];
  relations: RelationSummary[];
  neighbors: EntitySummary[];
  unresolved: SourceBinding[];
  coverage: Coverage;
  freshness: GraphFreshness;
  graphRevision: number;
}

// ----- impact -------------------------------------------------------------------------

export type ImpactAction = "needsReview" | "stale" | "citationReview";

export interface ImpactItem {
  entity: EntitySummary;
  path: string[];
  reason: string;
  action: ImpactAction;
}

export interface ImpactResult {
  items: ImpactItem[];
  cycles: string[];
  truncated: boolean;
  workLimited: boolean;
}

// ----- changes ---------------------------------------------------------------------------

export type GraphOperation =
  | { op: "createEntity"; kind: string; label: string; metadata?: Record<string, unknown> }
  | { op: "updateLabel"; entity: string; label: string }
  | { op: "setLifecycle"; entity: string; lifecycle: Lifecycle }
  | { op: "addRelation"; kind: string; from: string; to: string }
  | { op: "setRelationStatus"; relation: string; status: "accepted" | "rejected" }
  | { op: "removeRelation"; relation: string }
  | { op: "addMember"; collection: string; entity: string }
  | { op: "removeMember"; collection: string; entity: string };

export interface Diagnostic {
  severity: "error" | "warning" | "info";
  code: string;
  message: string;
  /** Index of the operation it concerns. */
  operation: number | null;
}

export interface Proposal {
  id: string | null;
  baseRevision: number;
  operations: GraphOperation[];
  valid: boolean;
  diagnostics: Diagnostic[];
  /** Human summary of the effect, when the backend provides one. */
  summary?: string;
}

export type ApplyOutcome =
  | { kind: "applied"; revision: number }
  | { kind: "conflict"; currentRevision: number }
  | { kind: "invalid"; diagnostics: Diagnostic[] };

// ----- saved views -----------------------------------------------------------------------

export interface ViewFilters {
  /** Entity kinds to show (null = all). */
  entityKinds: string[] | null;
  /** Relation kinds to show (null = all). */
  relationKinds: string[] | null;
  lifecycle: "active" | "all";
  /** Show rejected suggestions. */
  showRejected: boolean;
  text: string;
}

export interface ViewFocus {
  entity: string;
  depth: number;
  direction: SnapshotDirection;
}

export interface NodeLayout {
  x: number;
  y: number;
  width?: number;
  height?: number;
}

export interface SavedView {
  id: string;
  name: string;
  /** Expected-revision token for saves. */
  revision: number;
  positions: Record<string, NodeLayout>;
  viewport: { x: number; y: number; zoom: number } | null;
  filters: ViewFilters;
  hidden: string[];
  collapsed: string[];
  focus: ViewFocus | null;
}

export type ViewSaveOutcome = { kind: "saved"; view: SavedView } | { kind: "conflict"; current: SavedView };

export const defaultFilters: ViewFilters = {
  entityKinds: null,
  relationKinds: null,
  lifecycle: "active",
  showRejected: false,
  text: "",
};

// ----- Mathematics ---------------------------------------------------------------------------

/** Exact verifier verdict vocabulary (Lean audit), or none. */
export type VerifierVerdict =
  | "checked"
  | "checkedWithAxioms"
  | "incomplete"
  | "reliesOnCompiledCode"
  | "partiallyChecked"
  | "notVerified"
  | "errors";

export type InformalState = "none" | "draft" | "proofLinked" | "underReview" | "reviewed" | "disputed";
export type FormalAssociationState = "none" | "suggested" | "proposed" | "reviewed" | "rejected";
export type EvidenceFreshness = "current" | "stale" | "unknown";

export interface MathRow {
  entity: EntitySummary;
  binding: SourceBinding | null;
  informal: InformalState;
  /** Entities whose `proof_of` targets this statement. */
  proofs: string[];
  formal: FormalAssociationState;
  /** Formal declarations associated (`formalizes`). */
  declarations: string[];
  verdict: VerifierVerdict | null;
  freshness: EvidenceFreshness;
  coverage: Coverage;
}

export interface MathOverview {
  revision: number;
  rows: MathRow[];
  relations: RelationSummary[];
  coverage: Coverage;
  freshness: GraphFreshness;
  /** Formal dependency edges from checker evidence are available. */
  formalDependencies: "available" | "unavailable";
  formalDependenciesReason?: string;
}

// ----- display tables (fallbacks; the vocabulary wins) ---------------------------------------

interface KindDisplay {
  category: GraphCategory;
  icon: IconComponent;
}

const KIND_DISPLAY: Record<string, KindDisplay> = {
  project: { category: "structure", icon: FolderKanban },
  folder: { category: "structure", icon: Folder },
  collection: { category: "structure", icon: Library },
  research_space: { category: "structure", icon: FlaskConical },
  manuscript: { category: "document", icon: FileText },
  document: { category: "document", icon: FileText },
  note: { category: "document", icon: StickyNote },
  source: { category: "document", icon: FileCode },
  script: { category: "document", icon: FileCode },
  file: { category: "document", icon: FileText },
  asset: { category: "data", icon: Image },
  dataset: { category: "data", icon: Database },
  artifact: { category: "data", icon: Image },
  figure: { category: "data", icon: Image },
  table: { category: "data", icon: Table2 },
  result: { category: "data", icon: Lightbulb },
  finding: { category: "data", icon: Lightbulb },
  experiment: { category: "data", icon: FlaskRound },
  reference: { category: "reference", icon: BookMarked },
  citation: { category: "reference", icon: BookMarked },
  external_resource: { category: "reference", icon: BookMarked },
  definition: { category: "math", icon: BookOpen },
  assumption: { category: "math", icon: Anchor },
  conjecture: { category: "math", icon: CircleHelp },
  lemma: { category: "math", icon: Sigma },
  theorem: { category: "math", icon: Sigma },
  proposition: { category: "math", icon: Sigma },
  corollary: { category: "math", icon: Sigma },
  claim: { category: "math", icon: Quote },
  proof: { category: "math", icon: ScrollText },
  question: { category: "math", icon: CircleHelp },
  math_object: { category: "math", icon: Shapes },
  formal_declaration: { category: "formal", icon: Braces },
  lean_declaration: { category: "formal", icon: Braces },
  investigation: { category: "workflow", icon: Search },
  workflow: { category: "workflow", icon: Workflow },
};

const MATH_KINDS = new Set([
  "definition",
  "assumption",
  "conjecture",
  "lemma",
  "theorem",
  "proposition",
  "corollary",
  "claim",
  "proof",
  "question",
  "math_object",
]);
const FORMAL_KINDS = new Set(["formal_declaration", "lean_declaration"]);
const GROUP_KINDS = new Set(["project", "folder", "collection", "research_space"]);
const FILE_KINDS = new Set(["manuscript", "document", "note", "source", "script", "file", "asset", "dataset", "artifact", "figure", "table"]);

const RELATION_FAMILY: Record<string, RelationFamily> = {
  contains: "structure",
  member_of: "structure",
  anchored_in: "structure",
  imports: "resource",
  uses_asset: "resource",
  depends_on: "math",
  assumes: "math",
  proof_of: "math",
  references: "citation",
  evidence_for: "evidence",
  derived_from: "evidence",
  formalizes: "formal",
};

/** "research_space" → "Research space". */
export function humanize(id: string): string {
  const words = id.replace(/([a-z])([A-Z])/g, "$1 $2").replace(/[_-]+/g, " ").trim().toLowerCase();
  return words.charAt(0).toUpperCase() + words.slice(1);
}

export function displayForKind(kind: string): KindDisplay {
  return KIND_DISPLAY[kind] ?? { category: "structure", icon: Shapes };
}

/** Fallback description of an entity kind the vocabulary did not list. */
export function fallbackEntityKind(kind: string): EntityKindInfo {
  const display = displayForKind(kind);
  const label = humanize(kind);
  return {
    id: kind,
    label,
    plural: `${label}s`,
    category: display.category,
    icon: display.icon,
    group: GROUP_KINDS.has(kind),
    mathematical: MATH_KINDS.has(kind) || FORMAL_KINDS.has(kind),
    formal: FORMAL_KINDS.has(kind),
    creatable: false,
    fileBacked: FILE_KINDS.has(kind),
  };
}

/** Fallback description of a relation kind the vocabulary did not list. */
export function fallbackRelationKind(kind: string): RelationKindInfo {
  const label = humanize(kind).toLowerCase();
  return {
    id: kind,
    family: RELATION_FAMILY[kind] ?? "structure",
    label,
    inverseLabel: `${label} (inverse)`,
    from: [],
    to: [],
    manual: false,
  };
}

export const familyFallback = (kind: string): RelationFamily => RELATION_FAMILY[kind] ?? "structure";
export const isMathKindFallback = (kind: string) => MATH_KINDS.has(kind) || FORMAL_KINDS.has(kind);

export function entityKind(vocabulary: Vocabulary | null, kind: string): EntityKindInfo {
  return vocabulary?.entityKinds.get(kind) ?? fallbackEntityKind(kind);
}

export function relationKind(vocabulary: Vocabulary | null, kind: string): RelationKindInfo {
  return vocabulary?.relationKinds.get(kind) ?? fallbackRelationKind(kind);
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

/** Does the vocabulary allow `kind` between entities of these kinds? (Display ordering only; the backend validates.) */
export function endpointAllowed(info: RelationKindInfo, fromKind: string, toKind: string): boolean {
  return (info.from.length === 0 || info.from.includes(fromKind)) && (info.to.length === 0 || info.to.includes(toKind));
}
