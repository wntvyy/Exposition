//! The closed, versioned graph vocabulary: entity kinds, resource roles, and
//! relation kinds with their endpoint rules.
//!
//! An arbitrary line or free-text label never acquires executable or
//! mathematical meaning: a relation is one of these kinds, its endpoints must
//! satisfy the kind's rule, and extending the vocabulary means adding a kind
//! here with documented semantics (and bumping [`VOCABULARY_VERSION`]).
//! Execution bindings are not relations; they exist only inside validated
//! workflow definitions.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Version of this vocabulary. Stored in each graph; a graph written with a
/// newer vocabulary is refused rather than misread.
pub const VOCABULARY_VERSION: u32 = 1;

/// What an entity is. The kind determines which operations apply to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum EntityKind {
    // Structure
    /// The project root (exactly one per graph).
    Project,
    /// A relevant project folder.
    Folder,
    /// An intentional organizational collection of existing entities.
    Collection,
    /// The project's research space container (`.exposition/research/<id>/`).
    ResearchSpace,
    // Resources
    /// A project file (manuscript, note, script, dataset, retained artifact,
    /// asset, Lean source, bibliography, ...). One identity per file; its
    /// roles say how it is used.
    Resource,
    /// An explicit reference to something outside the project (a dataset
    /// path, URL, DOI). Never crawled or executed.
    ExternalResource,
    // Mathematics
    Definition,
    Assumption,
    Conjecture,
    Lemma,
    Theorem,
    Proposition,
    Corollary,
    /// A provisional or informal claim.
    Claim,
    Proof,
    Question,
    /// A named mathematical object (a space, an operator, a constant).
    MathObject,
    /// A declaration of a formal (Lean) source file.
    FormalDeclaration,
    // Citation
    /// A bibliographic reference (e.g. a BibTeX entry).
    Reference,
    // Research
    /// A saved experiment: objective, procedure, declared inputs, retained outcomes.
    Experiment,
    /// A retained finding (substantive text lives in a source/research record).
    Finding,
    /// A retained investigation (e.g. an agent investigation kept on purpose).
    Investigation,
    // Workflows
    /// A saved workflow definition.
    Workflow,
}

/// Coarse grouping used for filters, legends, and colors.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum EntityFamily {
    Structure,
    Resource,
    Mathematics,
    Citation,
    Research,
    Workflow,
}

impl EntityKind {
    pub const ALL: [EntityKind; 23] = [
        EntityKind::Project,
        EntityKind::Folder,
        EntityKind::Collection,
        EntityKind::ResearchSpace,
        EntityKind::Resource,
        EntityKind::ExternalResource,
        EntityKind::Definition,
        EntityKind::Assumption,
        EntityKind::Conjecture,
        EntityKind::Lemma,
        EntityKind::Theorem,
        EntityKind::Proposition,
        EntityKind::Corollary,
        EntityKind::Claim,
        EntityKind::Proof,
        EntityKind::Question,
        EntityKind::MathObject,
        EntityKind::FormalDeclaration,
        EntityKind::Reference,
        EntityKind::Experiment,
        EntityKind::Finding,
        EntityKind::Investigation,
        EntityKind::Workflow,
    ];

    pub fn family(self) -> EntityFamily {
        use EntityKind::*;
        match self {
            Project | Folder | Collection | ResearchSpace => EntityFamily::Structure,
            Resource | ExternalResource => EntityFamily::Resource,
            Definition | Assumption | Conjecture | Lemma | Theorem | Proposition | Corollary | Claim | Proof
            | Question | MathObject | FormalDeclaration => EntityFamily::Mathematics,
            Reference => EntityFamily::Citation,
            Experiment | Finding | Investigation => EntityFamily::Research,
            Workflow => EntityFamily::Workflow,
        }
    }

    /// Statements that a proof can target and a formal declaration can formalize.
    pub fn is_statement(self) -> bool {
        use EntityKind::*;
        matches!(self, Conjecture | Lemma | Theorem | Proposition | Corollary | Claim)
    }

    /// Informal mathematical entities (excluding formal declarations).
    pub fn is_informal_math(self) -> bool {
        self.family() == EntityFamily::Mathematics && self != EntityKind::FormalDeclaration
    }

    /// Entities that must be bound to a source region or file to exist
    /// meaningfully (their text lives in a source).
    pub fn is_file_backed(self) -> bool {
        matches!(self, EntityKind::Resource | EntityKind::Folder)
    }

    pub fn as_str(self) -> &'static str {
        use EntityKind::*;
        match self {
            Project => "project",
            Folder => "folder",
            Collection => "collection",
            ResearchSpace => "researchSpace",
            Resource => "resource",
            ExternalResource => "externalResource",
            Definition => "definition",
            Assumption => "assumption",
            Conjecture => "conjecture",
            Lemma => "lemma",
            Theorem => "theorem",
            Proposition => "proposition",
            Corollary => "corollary",
            Claim => "claim",
            Proof => "proof",
            Question => "question",
            MathObject => "mathObject",
            FormalDeclaration => "formalDeclaration",
            Reference => "reference",
            Experiment => "experiment",
            Finding => "finding",
            Investigation => "investigation",
            Workflow => "workflow",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == raw)
    }

    pub fn title(self) -> &'static str {
        use EntityKind::*;
        match self {
            Project => "Project",
            Folder => "Folder",
            Collection => "Collection",
            ResearchSpace => "Research space",
            Resource => "Resource",
            ExternalResource => "External resource",
            Definition => "Definition",
            Assumption => "Assumption",
            Conjecture => "Conjecture",
            Lemma => "Lemma",
            Theorem => "Theorem",
            Proposition => "Proposition",
            Corollary => "Corollary",
            Claim => "Claim",
            Proof => "Proof",
            Question => "Question",
            MathObject => "Mathematical object",
            FormalDeclaration => "Formal declaration",
            Reference => "Reference",
            Experiment => "Experiment",
            Finding => "Finding",
            Investigation => "Investigation",
            Workflow => "Workflow",
        }
    }
}

/// How a file-backed resource is used. File, dataset, and artifact are
/// compatible roles of one resource identity, never duplicate entities.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum ResourceRole {
    /// A typeset document (Typst, LaTeX, Markdown).
    Manuscript,
    Note,
    /// Python, R, or Julia code.
    Script,
    Dataset,
    /// A retained research output (figure, table, data file).
    Artifact,
    /// An image or other file a manuscript uses.
    Asset,
    /// A Lean source file.
    FormalSource,
    Bibliography,
    Configuration,
    Other,
}

impl ResourceRole {
    pub const ALL: [ResourceRole; 10] = [
        ResourceRole::Manuscript,
        ResourceRole::Note,
        ResourceRole::Script,
        ResourceRole::Dataset,
        ResourceRole::Artifact,
        ResourceRole::Asset,
        ResourceRole::FormalSource,
        ResourceRole::Bibliography,
        ResourceRole::Configuration,
        ResourceRole::Other,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ResourceRole::Manuscript => "manuscript",
            ResourceRole::Note => "note",
            ResourceRole::Script => "script",
            ResourceRole::Dataset => "dataset",
            ResourceRole::Artifact => "artifact",
            ResourceRole::Asset => "asset",
            ResourceRole::FormalSource => "formalSource",
            ResourceRole::Bibliography => "bibliography",
            ResourceRole::Configuration => "configuration",
            ResourceRole::Other => "other",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|r| r.as_str() == raw)
    }
}

/// The closed set of relation kinds. `A depends_on B` reads "A depends on B";
/// change-impact analysis follows relations in the reverse direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum RelationKind {
    /// Ownership: a project/folder/research space contains an entity.
    Contains,
    /// Organization: an entity is a member of a collection or research space.
    MemberOf,
    /// Source location: a declared entity is anchored in a resource.
    AnchoredIn,
    /// Observed source/build relationship: a resource imports another.
    Imports,
    /// A resource uses an asset, dataset, or retained artifact.
    UsesAsset,
    /// Mathematical assertion: A depends on B.
    DependsOn,
    /// Mathematical assertion: A assumes assumption B.
    Assumes,
    /// The proof A targets statement B (not "verified").
    ProofOf,
    /// Citation or mention, without inferred mathematical dependence.
    References,
    /// A supports B; carries an evidence kind and evidence records.
    EvidenceFor,
    /// A was derived from B (run records say how).
    DerivedFrom,
    /// The formal declaration A is associated with B (proposed or reviewed
    /// correspondence; never a machine proof of equivalence).
    Formalizes,
}

/// Relation families for legends and filters.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum RelationFamily {
    Structure,
    ResourceUse,
    Mathematics,
    Citation,
    Evidence,
    Formal,
}

impl RelationKind {
    pub const ALL: [RelationKind; 12] = [
        RelationKind::Contains,
        RelationKind::MemberOf,
        RelationKind::AnchoredIn,
        RelationKind::Imports,
        RelationKind::UsesAsset,
        RelationKind::DependsOn,
        RelationKind::Assumes,
        RelationKind::ProofOf,
        RelationKind::References,
        RelationKind::EvidenceFor,
        RelationKind::DerivedFrom,
        RelationKind::Formalizes,
    ];

    pub fn family(self) -> RelationFamily {
        use RelationKind::*;
        match self {
            Contains | MemberOf | AnchoredIn => RelationFamily::Structure,
            Imports | UsesAsset => RelationFamily::ResourceUse,
            DependsOn | Assumes | ProofOf => RelationFamily::Mathematics,
            References => RelationFamily::Citation,
            EvidenceFor | DerivedFrom => RelationFamily::Evidence,
            Formalizes => RelationFamily::Formal,
        }
    }

    pub fn as_str(self) -> &'static str {
        use RelationKind::*;
        match self {
            Contains => "contains",
            MemberOf => "memberOf",
            AnchoredIn => "anchoredIn",
            Imports => "imports",
            UsesAsset => "usesAsset",
            DependsOn => "dependsOn",
            Assumes => "assumes",
            ProofOf => "proofOf",
            References => "references",
            EvidenceFor => "evidenceFor",
            DerivedFrom => "derivedFrom",
            Formalizes => "formalizes",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == raw)
    }

    /// Human reading of `from <verb> to`.
    pub fn reading(self) -> &'static str {
        use RelationKind::*;
        match self {
            Contains => "contains",
            MemberOf => "is a member of",
            AnchoredIn => "is declared in",
            Imports => "imports",
            UsesAsset => "uses",
            DependsOn => "depends on",
            Assumes => "assumes",
            ProofOf => "is a proof of",
            References => "references",
            EvidenceFor => "is evidence for",
            DerivedFrom => "is derived from",
            Formalizes => "formalizes",
        }
    }

    /// Mathematical relations may form cycles (traversal terminates and
    /// explains them); structural containment may not.
    pub fn allows_cycles(self) -> bool {
        !matches!(self, RelationKind::Contains)
    }

    /// Endpoint rule: may an entity of kind `from` relate to one of kind `to`?
    pub fn allows(self, from: EntityKind, to: EntityKind) -> bool {
        use EntityKind as K;
        use RelationKind::*;
        let math = |k: EntityKind| k.family() == EntityFamily::Mathematics;
        match self {
            Contains => {
                matches!(from, K::Project | K::Folder | K::ResearchSpace)
                    && matches!(
                        to,
                        K::Folder
                            | K::Resource
                            | K::ExternalResource
                            | K::Collection
                            | K::ResearchSpace
                            | K::Experiment
                            | K::Finding
                            | K::Investigation
                            | K::Workflow
                    )
            }
            MemberOf => from != K::Project && matches!(to, K::Collection | K::ResearchSpace),
            AnchoredIn => (math(from) || matches!(from, K::Reference | K::Finding)) && to == K::Resource,
            Imports => from == K::Resource && matches!(to, K::Resource | K::ExternalResource),
            UsesAsset => from == K::Resource && matches!(to, K::Resource | K::ExternalResource),
            DependsOn => math(from) && from != K::Proof && math(to) && to != K::Question,
            Assumes => {
                (from.is_statement() || matches!(from, K::Proof | K::Definition))
                    && matches!(to, K::Assumption | K::Conjecture)
            }
            ProofOf => from == K::Proof && to.is_statement(),
            References => matches!(from, K::Resource | K::Finding | K::Investigation | K::Experiment) || math(from),
            EvidenceFor => {
                matches!(
                    from,
                    K::Resource | K::Experiment | K::Finding | K::Investigation | K::Proof | K::FormalDeclaration
                ) && (to.is_statement() || matches!(to, K::Finding | K::Question | K::Investigation | K::Experiment))
            }
            DerivedFrom => {
                matches!(from, K::Resource | K::Finding | K::Experiment)
                    && matches!(to, K::Resource | K::ExternalResource | K::Experiment | K::Workflow)
            }
            Formalizes => {
                from == K::FormalDeclaration
                    && (to.is_statement() || matches!(to, K::Definition | K::Assumption | K::MathObject))
            }
        }
    }

    /// Plain-language endpoint rule, shown when a connection is refused.
    pub fn rule(self) -> &'static str {
        use RelationKind::*;
        match self {
            Contains => {
                "A project, folder, or research space contains folders, resources, collections, experiments, findings, investigations, or workflows."
            }
            MemberOf => "Any entity except the project can be a member of a collection or the research space.",
            AnchoredIn => "Mathematical entities, references, and findings are declared in a resource.",
            Imports => "A resource imports another resource or an external resource.",
            UsesAsset => "A resource uses another resource (asset, dataset, artifact) or an external resource.",
            DependsOn => "A mathematical entity (not a proof) depends on another mathematical entity (not a question).",
            Assumes => "A statement, proof, or definition assumes an assumption or a conjecture.",
            ProofOf => "A proof targets a statement (conjecture, lemma, theorem, proposition, corollary, claim).",
            References => {
                "A resource, finding, investigation, experiment, or mathematical entity references anything citable."
            }
            EvidenceFor => {
                "A resource, experiment, finding, investigation, proof, or formal declaration supports a statement, finding, question, investigation, or experiment."
            }
            DerivedFrom => {
                "A resource, finding, or experiment is derived from a resource, external resource, experiment, or workflow."
            }
            Formalizes => {
                "A formal declaration formalizes a statement, definition, assumption, or mathematical object."
            }
        }
    }
}

/// Kinds of support an `evidenceFor` relation or an evidence record carries.
/// Their types cannot be substituted for one another.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum EvidenceKind {
    /// A computation's retained output (research run).
    Computational,
    /// Service-owned formal verification evidence (a Lean audit). Only the
    /// verification service can register it.
    FormalCheck,
    /// A human or agent review report.
    Review,
    /// A typeset build.
    Build,
    /// A literature source.
    Literature,
}

impl EvidenceKind {
    pub const ALL: [EvidenceKind; 5] = [
        EvidenceKind::Computational,
        EvidenceKind::FormalCheck,
        EvidenceKind::Review,
        EvidenceKind::Build,
        EvidenceKind::Literature,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            EvidenceKind::Computational => "computational",
            EvidenceKind::FormalCheck => "formalCheck",
            EvidenceKind::Review => "review",
            EvidenceKind::Build => "build",
            EvidenceKind::Literature => "literature",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == raw)
    }

    /// Privileged kinds are registered only by their owning service, never
    /// through proposals from people, agents, or workflow steps.
    pub fn is_service_owned(self) -> bool {
        matches!(self, EvidenceKind::FormalCheck | EvidenceKind::Computational | EvidenceKind::Build)
    }
}

/// The vocabulary as the frontend and agents see it (legends, pickers,
/// refusal explanations). The backend remains the only validator.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct Vocabulary {
    pub version: u32,
    pub entity_kinds: Vec<EntityKindInfo>,
    pub resource_roles: Vec<ResourceRole>,
    pub relation_kinds: Vec<RelationKindInfo>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct EntityKindInfo {
    pub kind: EntityKind,
    pub family: EntityFamily,
    pub title: String,
    /// People and agents may create it directly (others come from indexing,
    /// services, or blueprints).
    pub creatable: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct RelationKindInfo {
    pub kind: RelationKind,
    pub family: RelationFamily,
    pub reading: String,
    pub rule: String,
    /// Allowed `[from, to]` kind pairs.
    pub endpoints: Vec<[EntityKind; 2]>,
    /// People and agents may add it (structural ones come from indexing).
    pub manual: bool,
}

impl RelationKind {
    /// Relations people and agents may add or remove by hand. Containment and
    /// anchoring follow the files; imports are observed by indexing.
    pub fn is_manual(self) -> bool {
        !matches!(self, RelationKind::Contains | RelationKind::AnchoredIn | RelationKind::Imports)
    }
}

impl EntityKind {
    /// Kinds people and agents may create directly. Resources, folders,
    /// declarations, and the project itself come from files, indexing, and
    /// services.
    pub fn is_creatable(self) -> bool {
        use EntityKind::*;
        !matches!(self, Project | Folder | Resource | ResearchSpace | FormalDeclaration)
    }
}

pub fn vocabulary() -> Vocabulary {
    let kinds: Vec<EntityKind> = EntityKind::ALL.to_vec();
    Vocabulary {
        version: VOCABULARY_VERSION,
        entity_kinds: kinds
            .iter()
            .map(|&kind| EntityKindInfo {
                kind,
                family: kind.family(),
                title: kind.title().to_owned(),
                creatable: kind.is_creatable(),
            })
            .collect(),
        resource_roles: ResourceRole::ALL.to_vec(),
        relation_kinds: RelationKind::ALL
            .iter()
            .map(|&kind| RelationKindInfo {
                kind,
                family: kind.family(),
                reading: kind.reading().to_owned(),
                rule: kind.rule().to_owned(),
                endpoints: kinds
                    .iter()
                    .flat_map(|&a| kinds.iter().map(move |&b| (a, b)))
                    .filter(|&(a, b)| kind.allows(a, b))
                    .map(|(a, b)| [a, b])
                    .collect(),
                manual: kind.is_manual(),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_round_trips_by_name() {
        for kind in EntityKind::ALL {
            assert_eq!(EntityKind::parse(kind.as_str()), Some(kind));
            let json = serde_json::to_value(kind).unwrap();
            assert_eq!(json, kind.as_str());
        }
        for kind in RelationKind::ALL {
            assert_eq!(RelationKind::parse(kind.as_str()), Some(kind));
            assert_eq!(serde_json::to_value(kind).unwrap(), kind.as_str());
        }
        for role in ResourceRole::ALL {
            assert_eq!(ResourceRole::parse(role.as_str()), Some(role));
            assert_eq!(serde_json::to_value(role).unwrap(), role.as_str());
        }
        assert_eq!(EntityKind::parse("bogus"), None);
    }

    #[test]
    fn vocabulary_lists_every_kind_once() {
        let v = vocabulary();
        assert_eq!(v.entity_kinds.len(), 23);
        assert_eq!(v.relation_kinds.len(), 12);
        assert!(v.relation_kinds.iter().all(|r| !r.endpoints.is_empty()), "every relation has allowed endpoints");
    }

    #[test]
    fn endpoint_rules_follow_the_documented_semantics() {
        use EntityKind as K;
        assert!(RelationKind::ProofOf.allows(K::Proof, K::Theorem));
        assert!(!RelationKind::ProofOf.allows(K::Theorem, K::Proof), "direction matters");
        assert!(!RelationKind::ProofOf.allows(K::Proof, K::Definition));
        assert!(RelationKind::DependsOn.allows(K::Theorem, K::Lemma));
        assert!(!RelationKind::DependsOn.allows(K::Resource, K::Lemma), "files do not depend mathematically");
        assert!(RelationKind::References.allows(K::Resource, K::Reference));
        assert!(RelationKind::Formalizes.allows(K::FormalDeclaration, K::Theorem));
        assert!(!RelationKind::Formalizes.allows(K::Theorem, K::FormalDeclaration));
        assert!(RelationKind::UsesAsset.allows(K::Resource, K::Resource));
        assert!(!RelationKind::UsesAsset.allows(K::Theorem, K::Resource));
        assert!(!RelationKind::MemberOf.allows(K::Project, K::Collection));
        assert!(RelationKind::Assumes.allows(K::Theorem, K::Assumption));
    }

    #[test]
    fn formal_check_evidence_is_service_owned() {
        assert!(EvidenceKind::FormalCheck.is_service_owned());
        assert!(!EvidenceKind::Review.is_service_owned());
    }
}
