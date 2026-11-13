//! Structured tools offered to the model.
//!
//! Every tool has a JSON schema and a typed Rust input with
//! `deny_unknown_fields`; with eager input streaming the API does not validate
//! inputs, so each call is deserialized and validated here before anything
//! runs. Invalid input becomes an error tool result. The original file,
//! build, research, and Lean tools are sent `strict`; the larger graph and
//! workflow schemas are not (the provider compiles all strict schemas of a
//! request into one grammar with a size limit — see the test below).
//!
//! The tool list is fixed and deterministic (prompt-cache friendly). Whether a
//! tool may act is decided per call by the task's scope, permission profile,
//! execution mode, consent, and budgets — never by model or project text.

pub mod exec;
pub mod execution;
pub mod scope;
pub mod services;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::graph::propose::{AGENT_CREATABLE, GraphProposeInput, manual_relations};
use crate::graph::query::GraphQueryInput;
use crate::graph::workflow::WorkflowProposeInput;
use crate::provider::ToolDefinition;

pub const LIST_FILES: &str = "list_files";
pub const READ_FILE: &str = "read_file";
pub const EDIT_DOCUMENT: &str = "edit_document";
pub const CREATE_FILE: &str = "create_file";
pub const BUILD_DOCUMENT: &str = "build_document";
pub const RUN_PYTHON: &str = "run_python";
pub const CHECK_LEAN: &str = "check_lean";
pub const RUN_COMMAND: &str = "run_command";
pub const INSERT_ARTIFACT: &str = "insert_artifact";
pub const GRAPH_QUERY: &str = "graph_query";
pub const GRAPH_PROPOSE: &str = "graph_propose";
pub const WORKFLOW_PROPOSE: &str = "workflow_propose";

/// The tool list, in a fixed order.
pub fn definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: LIST_FILES,
            strict: true,
            description: "List files and folders of the user's project (only those inside this task's scope). \
                Call it first to discover the project's structure, and whenever you need a path you have not seen. \
                Paths are relative to the project root and use forward slashes.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Folder to list, relative to the project root. Use \"\" for the root."},
                    "recursive": {"type": "boolean", "description": "Include subfolders (up to 500 entries)."}
                },
                "required": ["path", "recursive"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: READ_FILE,
            strict: true,
            description: "Read a text file of the project. Returns the current text, including unsaved changes in \
                the user's open editor, and its revision number. Always read a file (again) before editing it: \
                edit_document needs the revision you read, and the user may have changed the file since.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "File path relative to the project root."}
                },
                "required": ["path"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: EDIT_DOCUMENT,
            strict: true,
            description: "Change a text file by replacing exact snippets of the revision you read. Each old_text \
                must occur exactly once in that revision (copy it exactly, with enough surrounding text to be unique) \
                and edits must not overlap; to insert, include neighbouring text in old_text and new_text. All edits \
                apply together as one transaction to the user's open document (not saved to disk); the user reviews \
                and can revert them. If the user changed the same text meanwhile, the call fails with the current \
                revision: read the file again and redo the edit on the new text.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "File path relative to the project root."},
                    "base_revision": {"type": "integer", "description": "The revision returned by read_file (or by your previous edit) that old_text refers to."},
                    "edits": {
                        "type": "array",
                        "description": "Replacements, applied together.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "old_text": {"type": "string", "description": "Exact text to replace; must occur exactly once. Empty only to fill an empty file."},
                                "new_text": {"type": "string", "description": "Replacement text."}
                            },
                            "required": ["old_text", "new_text"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["path", "base_revision", "edits"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: CREATE_FILE,
            strict: true,
            description: "Create a new text file with the given content. Fails if the file already exists (use \
                edit_document to change existing files). Missing folders are created. The user can review and \
                revert the creation.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "New file path relative to the project root."},
                    "content": {"type": "string", "description": "Complete file content."}
                },
                "required": ["path", "content"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: BUILD_DOCUMENT,
            strict: true,
            description: "Typeset a document (Typst .typ, Markdown .md or .markdown through its print profile, or \
                LaTeX .tex) from the current editor state, unsaved changes included, and report the real status, \
                diagnostics with locations, and page count. Use it to check that your changes build; report its \
                result honestly, including failures. A Typst or Markdown build reads only files in the task's scope; \
                a LaTeX build runs TeX natively, so it needs a task scoped to the whole project and the user's \
                approval.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "entry": {"type": "string", "description": "Entry file relative to the project root, e.g. main.typ, notes.md, or paper.tex."},
                    "engine": {"type": "string", "enum": ["auto", "pdflatex", "lualatex"], "description": "LaTeX engine; auto uses the project's setting (ignored for Typst)."}
                },
                "required": ["entry", "engine"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: INSERT_ARTIFACT,
            strict: true,
            description: "Insert a figure or table produced by a research run (an artifact listed in a run_python \
                result) into a Typst, LaTeX, or Markdown document using the format's own figure/table markup and a \
                stable attachment key, or update an earlier insertion by its key. The change is applied to the open \
                document like an edit and is reviewable.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Document relative to the project root."},
                    "run_id": {"type": "string", "description": "Run id from run_python."},
                    "artifact_id": {"type": "string", "description": "Artifact id from that run."},
                    "after_text": {"type": "string", "description": "Exact text, unique in the current document, after which to insert; empty inserts at the end. Ignored when updating."},
                    "caption": {"type": "string", "description": "Caption text; empty for none (an update keeps the existing caption)."},
                    "update_key": {"type": "string", "description": "Attachment key of an earlier insertion to update; empty to insert."}
                },
                "required": ["path", "run_id", "artifact_id", "after_text", "caption", "update_key"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: RUN_PYTHON,
            strict: true,
            description: "Run Python code in a separate process for this task (never the user's own sessions) and \
                return its real exit code and output. Use it for calculations and data analysis the user asked for. \
                In the restricted sandbox the project is read-only at /workspace and files can be written to /out. \
                Available only for tasks scoped to the whole project.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "code": {"type": "string", "description": "Python source to execute."}
                },
                "required": ["code"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: CHECK_LEAN,
            strict: true,
            description: "Check a Lean file with the project's Lean toolchain and return the real diagnostics and \
                proof status (incomplete proofs and `sorry` are reported as such). Lean runs natively, so this needs a \
                task scoped to the whole project and the user's approval.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Lean file relative to the project root."}
                },
                "required": ["path"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: RUN_COMMAND,
            strict: true,
            description: "Run one program with an argument list (no shell, no pipes or redirection) in the project \
                folder and return its exit code and bounded output. Use only when no dedicated tool fits. In the \
                restricted sandbox the project is read-only at /workspace and files can be written to /out. \
                Available only for tasks scoped to the whole project.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "program": {"type": "string", "description": "Program name or path, e.g. latexmk or python."},
                    "args": {"type": "array", "items": {"type": "string"}, "description": "Arguments, one per item."},
                    "working_directory": {"type": "string", "description": "Folder relative to the project root; \"\" for the root."}
                },
                "required": ["program", "args", "working_directory"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: GRAPH_QUERY,
            // Non-strict: its schema is large (typed operations / a workflow
            // definition); with it, the request's strict grammar exceeded the
            // provider's limit and every task was rejected. Rust validates
            // the input exactly (`deny_unknown_fields`).
            strict: false,
            description: "Read the project graph: Exposition's structural model of this project (stable identities of \
                files and research entities such as claims, lemmas, proofs, references, and experiments, and the \
                typed relations among them). Results are bounded and filtered to this task's scope and content \
                consent. query: search (by kinds and text), entity (one entity with its relations and evidence), \
                neighborhood (entities within depth relations), inspect (the entities bound in one file), impact \
                (what must be reviewed if an entity changes). Entity ids (ent-…) come from the graph context or \
                earlier results.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "enum": ["search", "entity", "neighborhood", "inspect", "impact"], "description": "What to read."},
                    "text": {"type": "string", "description": "search: case-insensitive text matched against labels and paths; \"\" for none. Without content consent only paths are matched."},
                    "kinds": {"type": "array", "items": {"type": "string", "enum": entity_kind_names()}, "description": "search: only these entity kinds; [] for all."},
                    "entity": {"type": "string", "description": "entity, neighborhood, impact: an entity id (ent-…); \"\" otherwise."},
                    "path": {"type": "string", "description": "inspect: a project file relative to the project root; \"\" otherwise."},
                    "depth": {"type": "integer", "description": "neighborhood and impact: relations to follow, 1 to 3 (use 1 otherwise)."},
                    "limit": {"type": "integer", "description": "Maximum entities to return, 1 to 100."}
                },
                "required": ["query", "text", "kinds", "entity", "path", "depth", "limit"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: GRAPH_PROPOSE,
            // Non-strict: its schema is large (typed operations / a workflow
            // definition); with it, the request's strict grammar exceeded the
            // provider's limit and every task was rejected. Rust validates
            // the input exactly (`deny_unknown_fields`).
            strict: false,
            description: "Propose typed changes to the project graph: create meaningful entities (claims, \
                definitions, lemmas, proofs, questions, references, experiments, findings, collections), add or \
                withdraw typed relations (dependsOn, assumes, proofOf, references, evidenceFor, memberOf, ...), \
                bind entities to files or regions, archive or discard entities. The application validates every \
                operation (closed vocabulary and endpoint rules) and applies them as one graph change: after the \
                user's approval in tasks that review each change, directly in tasks that apply changes. Relations \
                you add are recorded as suggestions until a person accepts them. Use \"\", [] and -1 for fields an \
                operation does not use; refer to entities created earlier in the same call by their temp_id.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "summary": {"type": "string", "description": "One-line summary of the change for the user; \"\" for none."},
                    "base_revision": {"type": "integer", "description": "The graph revision your view of the graph comes from (graph context or a query result)."},
                    "operations": {"type": "array", "description": "Operations, applied together or not at all.", "items": {"anyOf": operation_schemas()}}
                },
                "required": ["summary", "base_revision", "operations"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: WORKFLOW_PROPOSE,
            // Non-strict: its schema is large (typed operations / a workflow
            // definition); with it, the request's strict grammar exceeded the
            // provider's limit and every task was rejected. Rust validates
            // the input exactly (`deny_unknown_fields`).
            strict: false,
            description: "Save a workflow draft in the project graph: a declarative procedure of registered \
                operations (steps with typed input/output ports), explicit project-file inputs, typed data \
                connections or prerequisites, declared outcomes, and an authority ceiling (at most this task's own \
                permissions). The application validates it against the workflow contract and saves it with its \
                diagnostics (incomplete drafts are fine); after the user's approval in tasks that review each \
                change, directly in tasks that apply changes. Drafts are not runnable in this version and nothing \
                runs. Steps cannot stand in for service-owned results (formal evidence, retained artifacts). Use \
                \"\", [] and -1 for unused fields.",
            input_schema: workflow_schema(),
        },
    ]
}

/// Strict schema of `workflow_propose` (the workflow definition, flattened).
fn workflow_schema() -> Value {
    use exposition_graph::workflow::{OutcomeKind, PortType};
    let names = |values: Vec<Value>| -> Vec<String> {
        values.into_iter().filter_map(|v| v.as_str().map(str::to_owned)).collect()
    };
    let port_types = names(PortType::ALL.iter().map(|t| serde_json::to_value(t).unwrap_or(Value::Null)).collect());
    let outcome_kinds = names(
        [
            OutcomeKind::TransformedDataset,
            OutcomeKind::RetainedFigure,
            OutcomeKind::RetainedTable,
            OutcomeKind::InvestigationReport,
            OutcomeKind::AuditEvidence,
            OutcomeKind::ReviewableProposal,
            OutcomeKind::ManuscriptPromotion,
        ]
        .iter()
        .map(|k| serde_json::to_value(k).unwrap_or(Value::Null))
        .collect(),
    );
    let operations: Vec<String> = exposition_graph::workflow::operations().into_iter().map(|o| o.id).collect();
    let mut relations: Vec<&str> = vec![""];
    relations.extend(exposition_graph::RelationKind::ALL.iter().map(|k| k.as_str()));
    let strict = |props: Value| {
        let required: Vec<String> = props.as_object().map(|o| o.keys().cloned().collect()).unwrap_or_default();
        json!({"type": "object", "properties": props, "required": required, "additionalProperties": false})
    };
    let id =
        |what: &str| json!({"type": "string", "description": format!("{what} id: 1–32 letters, digits, '-' or '_'.")});
    json!({
        "type": "object",
        "properties": {
            "workflow_id": {"type": "string", "description": "\"\" for a new workflow, or the id (wf-…) of a saved one to save a new revision of."},
            "expected_revision": {"type": "integer", "description": "The saved revision you are revising, or -1."},
            "title": {"type": "string"},
            "description": {"type": "string"},
            "preset": {"type": "string", "description": "The workflow preset it starts from, or \"\"."},
            "inputs": {"type": "array", "items": strict(json!({
                "id": id("Input"),
                "label": {"type": "string"},
                "port_type": {"type": "string", "enum": port_types},
                "path": {"type": "string", "description": "Project file relative to the project root, or \"\" when not chosen yet."},
                "pinned_hash": {"type": "string", "description": "SHA-256 (hex) to pin the input to, or \"\" for the version at run start."}
            }))},
            "steps": {"type": "array", "items": strict(json!({
                "id": id("Step"),
                "label": {"type": "string"},
                "operation": {"type": "string", "enum": operations, "description": "A registered operation."},
                "version": {"type": "integer", "description": "Operation version (1)."},
                "parameters": {"type": "array", "items": strict(json!({
                    "name": {"type": "string"},
                    "kind": {"type": "string", "enum": ["text", "integer", "boolean"]},
                    "text": {"type": "string"},
                    "integer": {"type": "integer"},
                    "boolean": {"type": "boolean"}
                }))},
                "payload": {"anyOf": [
                    strict(json!({"kind": {"type": "string", "enum": ["none"]}})),
                    strict(json!({"kind": {"type": "string", "enum": ["code"]}, "language": {"type": "string", "enum": ["python"]}, "source": {"type": "string"}})),
                    strict(json!({"kind": {"type": "string", "enum": ["prompt"]}, "text": {"type": "string"}}))
                ]}
            }))},
            "connections": {"type": "array", "items": {"anyOf": [
                strict(json!({
                    "kind": {"type": "string", "enum": ["data"]},
                    "from_step": {"type": "string", "description": "Producing step (\"\" when from_input is set)."},
                    "from_port": {"type": "string"},
                    "from_input": {"type": "string", "description": "External input id, or \"\"."},
                    "to_step": {"type": "string"},
                    "to_port": {"type": "string"}
                })),
                strict(json!({"kind": {"type": "string", "enum": ["prerequisite"]}, "before": {"type": "string"}, "after": {"type": "string"}})),
                strict(json!({
                    "kind": {"type": "string", "enum": ["annotation"]},
                    "from": {"type": "string"},
                    "to": {"type": "string"},
                    "label": {"type": "string"},
                    "relation": {"type": "string", "enum": relations}
                }))
            ]}},
            "outcomes": {"type": "array", "items": strict(json!({
                "id": id("Outcome"),
                "kind": {"type": "string", "enum": outcome_kinds},
                "label": {"type": "string"},
                "step": {"type": "string"},
                "port": {"type": "string"},
                "optional": {"type": "boolean"}
            }))},
            "ceiling": strict(json!({
                "execution": {"type": "string", "enum": ["none", "restricted", "trustedNative"]},
                "model_requests": {"type": "boolean"},
                "retain_results": {"type": "boolean"},
                "propose_changes": {"type": "boolean"}
            }))
        },
        "required": ["workflow_id", "expected_revision", "title", "description", "preset", "inputs", "steps", "connections", "outcomes", "ceiling"],
        "additionalProperties": false
    })
}

fn entity_kind_names() -> Vec<&'static str> {
    exposition_graph::EntityKind::ALL.iter().map(|k| k.as_str()).collect()
}

/// One strict object schema per operation kind.
fn operation_schemas() -> Value {
    let creatable: Vec<&str> = AGENT_CREATABLE.iter().map(|k| k.as_str()).collect();
    let relations: Vec<&str> = manual_relations().iter().map(|k| k.as_str()).collect();
    let mut evidence_kinds: Vec<&str> = vec![""];
    evidence_kinds.extend(exposition_graph::EvidenceKind::ALL.iter().map(|k| k.as_str()));
    let attributes = json!({
        "type": "array",
        "description": "Metadata entries (names: letters, digits, '.', '_', '-'); [] for none.",
        "items": {
            "type": "object",
            "properties": {"name": {"type": "string"}, "value": {"type": "string"}},
            "required": ["name", "value"],
            "additionalProperties": false
        }
    });
    let object = |op: &str, description: &str, props: Value| {
        let mut properties = props.as_object().cloned().unwrap_or_default();
        properties.insert("op".into(), json!({"type": "string", "enum": [op], "description": description}));
        let required: Vec<String> = properties.keys().cloned().collect();
        json!({"type": "object", "properties": properties, "required": required, "additionalProperties": false})
    };
    let bind = |props: &mut serde_json::Map<String, Value>, required: bool| {
        let what = if required { "File to bind to" } else { "File to bind the new entity to; \"\" for none" };
        props.insert(
            "bind_path".into(),
            json!({"type": "string", "description": format!("{what} (relative to the project root).")}),
        );
        props.insert(
            "bind_start".into(),
            json!({"type": "integer", "description": "UTF-16 start offset of a region, or -1 for the whole file."}),
        );
        props.insert(
            "bind_end".into(),
            json!({"type": "integer", "description": "UTF-16 end offset of a region, or -1 for the whole file."}),
        );
    };
    let mut create = json!({
        "temp_id": {"type": "string", "description": "Id for referring to the new entity later in this call (letters, digits, '-', '_'; at most 32)."},
        "kind": {"type": "string", "enum": creatable},
        "label": {"type": "string", "description": "Concise one-line title."},
        "attributes": attributes.clone()
    });
    if let Some(props) = create.as_object_mut() {
        bind(props, false);
    }
    let mut bind_source = json!({
        "entity": {"type": "string", "description": "Entity id, or a temp_id from this call."}
    });
    if let Some(props) = bind_source.as_object_mut() {
        bind(props, true);
    }
    json!([
        object("create_entity", "Admit a meaningful entity.", create),
        object(
            "add_relation",
            "Add a typed relation (recorded as a suggestion).",
            json!({
                "relation": {"type": "string", "enum": relations},
                "from": {"type": "string", "description": "Entity id, or a temp_id from this call. Reads: from <relation> to."},
                "to": {"type": "string", "description": "Entity id, or a temp_id from this call."},
                "evidence_kind": {"type": "string", "enum": evidence_kinds, "description": "evidenceFor only: the kind of support; \"\" otherwise."},
                "evidence": {"type": "array", "items": {"type": "string"}, "description": "Existing evidence record ids to cite; [] for none."},
                "note": {"type": "string", "description": "Short note; \"\" for none."}
            })
        ),
        object(
            "update_entity",
            "Change an entity's label or attributes.",
            json!({
                "entity": {"type": "string", "description": "Entity id."},
                "expected_revision": {"type": "integer", "description": "The entity revision you read."},
                "label": {"type": "string", "description": "New label; \"\" keeps it."},
                "attributes": attributes,
                "replace_attributes": {"type": "boolean", "description": "true replaces all attributes with the list given."}
            })
        ),
        object(
            "set_lifecycle",
            "Archive, discard, or reactivate an entity (nothing is deleted).",
            json!({
                "entity": {"type": "string", "description": "Entity id."},
                "expected_revision": {"type": "integer", "description": "The entity revision you read."},
                "lifecycle": {"type": "string", "enum": ["active", "archived", "discarded"]}
            })
        ),
        object(
            "remove_relation",
            "Withdraw one of your own suggested relations.",
            json!({"relation": {"type": "string", "description": "Relation id."}})
        ),
        object("bind_source", "Bind an entity to a file or a region of it.", bind_source),
    ])
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListFilesInput {
    pub path: String,
    pub recursive: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadFileInput {
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditInput {
    pub old_text: String,
    pub new_text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditDocumentInput {
    pub path: String,
    pub base_revision: u64,
    pub edits: Vec<EditInput>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateFileInput {
    pub path: String,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildDocumentInput {
    pub entry: String,
    pub engine: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InsertArtifactInput {
    pub path: String,
    pub run_id: String,
    pub artifact_id: String,
    pub after_text: String,
    pub caption: String,
    pub update_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunPythonInput {
    pub code: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckLeanInput {
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunCommandInput {
    pub program: String,
    pub args: Vec<String>,
    pub working_directory: String,
}

/// A validated tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolCall {
    GraphQuery(GraphQueryInput),
    GraphPropose(GraphProposeInput),
    WorkflowPropose(WorkflowProposeInput),
    ListFiles(ListFilesInput),
    ReadFile(ReadFileInput),
    EditDocument(EditDocumentInput),
    CreateFile(CreateFileInput),
    BuildDocument(BuildDocumentInput),
    InsertArtifact(InsertArtifactInput),
    RunPython(RunPythonInput),
    CheckLean(CheckLeanInput),
    RunCommand(RunCommandInput),
}

/// How a call uses a project path it names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathUse {
    /// Read the file's content.
    Read,
    /// List a folder (names and sizes).
    Folder,
    /// Run a process in a folder (it may write there).
    WorkingFolder,
    /// Change an existing (or open) document.
    Write,
    /// Create a new file.
    Create,
}

/// What a result can carry back to the model provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exposure {
    /// File names and metadata only (listings, revisions, sizes, keys).
    Metadata,
    /// Content derived from files in the task's scope only (reads of other
    /// files are refused by the application, including reads a compiler
    /// discovers while building).
    ScopedContent,
    /// Content of any file the tool can reach: the whole project (restricted
    /// container) or any file the user's account can read (native processes,
    /// TeX).
    Unconfined,
    /// Graph records filtered by the tool itself to the task's scope and
    /// consent: paths, kinds, and counts always; content-derived labels,
    /// notes, and reasons only with content consent.
    ConsentFiltered,
}

/// Whether and how a call starts a process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Launch {
    /// No process (in-process work through the application's services).
    None,
    /// A native process only: there is no restricted backend for it.
    NativeOnly,
    /// Native after the user's approval, or unattended in the restricted backend.
    NativeOrRestricted,
}

/// What a call does. Every rule of the application's gate is applied to these
/// effects, never to a tool by name; [`ToolCall::effects`] has no default arm,
/// so a new tool cannot run before it declares them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Effects<'a> {
    /// Project paths the call names, as requested.
    pub paths: Vec<(&'a str, PathUse)>,
    pub exposes: Exposure,
    pub launch: Launch,
    /// Whether the call changes documents or files.
    pub changes: bool,
    /// The application service the call needs.
    pub service: Option<services::ServiceKind>,
    /// What the call is, for messages ("Python code", "a LaTeX build").
    pub what: &'static str,
    /// The named paths (other than folders) are project files the call acts
    /// on: they are resolved to graph resource identities and must not be
    /// pending reconciliation or ambiguous in the graph.
    pub targets: bool,
}

impl ToolCall {
    /// What this call does (see [`Effects`]).
    pub fn effects<'a>(&'a self) -> Effects<'a> {
        use services::ServiceKind;
        let effects = |paths: Vec<(&'a str, PathUse)>, exposes, launch, changes, service, what| {
            let targets = !paths.is_empty();
            Effects { paths, exposes, launch, changes, service, what, targets }
        };
        match self {
            ToolCall::GraphQuery(input) => {
                let paths = input.named_path().map(|p| vec![(p.trim(), PathUse::Read)]).unwrap_or_default();
                Effects {
                    targets: false,
                    ..effects(paths, Exposure::ConsentFiltered, Launch::None, false, None, "A graph query")
                }
            }
            ToolCall::WorkflowPropose(input) => {
                // Named input files are scope-checked; nothing is read or run.
                let paths = input.named_paths().into_iter().map(|p| (p, PathUse::Read)).collect();
                Effects {
                    targets: false,
                    ..effects(paths, Exposure::ConsentFiltered, Launch::None, true, None, "A workflow draft")
                }
            }
            ToolCall::GraphPropose(input) => {
                let paths =
                    crate::graph::propose::bound_paths(input).into_iter().map(|p| (p, PathUse::Write)).collect();
                Effects {
                    targets: false,
                    ..effects(paths, Exposure::ConsentFiltered, Launch::None, true, None, "A graph change")
                }
            }
            ToolCall::ListFiles(input) => {
                let paths =
                    if is_root(&input.path) { Vec::new() } else { vec![(input.path.as_str(), PathUse::Folder)] };
                Effects {
                    targets: false,
                    ..effects(paths, Exposure::Metadata, Launch::None, false, None, "Listing files")
                }
            }
            ToolCall::ReadFile(input) => effects(
                vec![(input.path.as_str(), PathUse::Read)],
                Exposure::ScopedContent,
                Launch::None,
                false,
                None,
                "Reading a file",
            ),
            ToolCall::EditDocument(input) => effects(
                vec![(input.path.as_str(), PathUse::Write)],
                Exposure::Metadata,
                Launch::None,
                true,
                None,
                "Editing a document",
            ),
            ToolCall::CreateFile(input) => effects(
                vec![(input.path.as_str(), PathUse::Create)],
                Exposure::Metadata,
                Launch::None,
                true,
                None,
                "Creating a file",
            ),
            ToolCall::BuildDocument(input) if is_latex(&input.entry) => effects(
                vec![(input.entry.as_str(), PathUse::Read)],
                // TeX can read any file the user can (an absolute \input path)
                // and its log comes back as diagnostics.
                Exposure::Unconfined,
                Launch::NativeOnly,
                false,
                Some(ServiceKind::Typesetting),
                "A LaTeX build (TeX)",
            ),
            ToolCall::BuildDocument(input) => effects(
                vec![(input.entry.as_str(), PathUse::Read)],
                // In-process build confined to the scope; status, pages, and
                // diagnostics are derived from file content.
                Exposure::ScopedContent,
                Launch::None,
                false,
                Some(ServiceKind::Typesetting),
                "A document build",
            ),
            ToolCall::InsertArtifact(input) => effects(
                vec![(input.path.as_str(), PathUse::Write)],
                Exposure::Metadata,
                Launch::None,
                true,
                Some(ServiceKind::Research),
                "Inserting a research artifact",
            ),
            ToolCall::RunPython(_) => effects(
                Vec::new(),
                Exposure::Unconfined,
                Launch::NativeOrRestricted,
                false,
                Some(ServiceKind::Research),
                "Python code",
            ),
            ToolCall::CheckLean(input) => effects(
                vec![(input.path.as_str(), PathUse::Read)],
                // Lean elaborates imports and runs project code (#eval can read
                // any file); its messages come back as diagnostics.
                Exposure::Unconfined,
                Launch::NativeOnly,
                false,
                Some(ServiceKind::Lean),
                "Lean checking (the project's own toolchain)",
            ),
            ToolCall::RunCommand(input) => {
                let paths = if is_root(&input.working_directory) {
                    Vec::new()
                } else {
                    vec![(input.working_directory.as_str(), PathUse::WorkingFolder)]
                };
                effects(
                    paths,
                    Exposure::Unconfined,
                    Launch::NativeOrRestricted,
                    false,
                    Some(ServiceKind::Execution),
                    "A command",
                )
            }
        }
    }

    /// Validate `input` against the tool's schema.
    pub fn parse(name: &str, input: &Value) -> Result<Self, String> {
        fn typed<T: serde::de::DeserializeOwned>(input: &Value) -> Result<T, String> {
            serde_json::from_value(input.clone()).map_err(|e| format!("input does not match the tool schema: {e}"))
        }
        Ok(match name {
            LIST_FILES => ToolCall::ListFiles(typed(input)?),
            READ_FILE => ToolCall::ReadFile(typed(input)?),
            EDIT_DOCUMENT => {
                let parsed: EditDocumentInput = typed(input)?;
                if parsed.edits.is_empty() {
                    return Err("edits must contain at least one replacement".into());
                }
                ToolCall::EditDocument(parsed)
            }
            CREATE_FILE => ToolCall::CreateFile(typed(input)?),
            BUILD_DOCUMENT => {
                let parsed: BuildDocumentInput = typed(input)?;
                if !matches!(parsed.engine.as_str(), "auto" | "pdflatex" | "lualatex") {
                    return Err("engine must be auto, pdflatex, or lualatex".into());
                }
                ToolCall::BuildDocument(parsed)
            }
            INSERT_ARTIFACT => ToolCall::InsertArtifact(typed(input)?),
            RUN_PYTHON => ToolCall::RunPython(typed(input)?),
            CHECK_LEAN => ToolCall::CheckLean(typed(input)?),
            RUN_COMMAND => ToolCall::RunCommand(typed(input)?),
            GRAPH_QUERY => {
                let parsed: GraphQueryInput = typed(input)?;
                parsed.validate()?;
                ToolCall::GraphQuery(parsed)
            }
            GRAPH_PROPOSE => {
                let parsed: GraphProposeInput = typed(input)?;
                if parsed.operations.is_empty() {
                    return Err("operations must contain at least one operation".into());
                }
                ToolCall::GraphPropose(parsed)
            }
            WORKFLOW_PROPOSE => {
                let parsed: WorkflowProposeInput = typed(input)?;
                parsed.check_bounds()?;
                ToolCall::WorkflowPropose(parsed)
            }
            other => return Err(format!("unknown tool {other:?}")),
        })
    }

    /// Whether the call launches a process (counted against the command
    /// budget). Typst builds run in-process; LaTeX builds run TeX tools.
    pub fn is_process(&self) -> bool {
        self.effects().launch != Launch::None
    }

    pub fn changes_documents(&self) -> bool {
        self.effects().changes
    }
}

/// `""`, `"."`, or `"./"`: the project root.
pub(crate) fn is_root(raw: &str) -> bool {
    matches!(raw.trim(), "" | "." | "./")
}

fn is_latex(entry: &str) -> bool {
    let entry = entry.to_ascii_lowercase();
    entry.ends_with(".tex") || entry.ends_with(".ltx")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object_schemas_are_strict(schema: &Value, at: &str) {
        if schema.get("type") == Some(&json!("object")) {
            assert_eq!(schema["additionalProperties"], json!(false), "{at}");
            let props: Vec<&String> = schema["properties"].as_object().unwrap().keys().collect();
            let required: Vec<&str> =
                schema["required"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
            for p in &props {
                assert!(required.contains(&p.as_str()), "{at}.{p} must be required");
            }
            for (name, prop) in schema["properties"].as_object().unwrap() {
                object_schemas_are_strict(prop, &format!("{at}.{name}"));
            }
        }
        if let Some(items) = schema.get("items") {
            object_schemas_are_strict(items, &format!("{at}[]"));
        }
        if let Some(variants) = schema.get("anyOf").and_then(Value::as_array) {
            for (i, variant) in variants.iter().enumerate() {
                object_schemas_are_strict(variant, &format!("{at}|{i}"));
            }
        }
    }

    #[test]
    fn schemas_are_strict_and_unique() {
        let defs = definitions();
        let mut names: Vec<_> = defs.iter().map(|d| d.name).collect();
        for def in &defs {
            object_schemas_are_strict(&def.input_schema, def.name);
        }
        names.sort();
        names.dedup();
        assert_eq!(names.len(), 12);
    }

    #[test]
    fn rejects_inputs_that_do_not_match_the_schema() {
        assert!(ToolCall::parse(READ_FILE, &json!({"path": "a.typ"})).is_ok());
        assert!(ToolCall::parse(READ_FILE, &json!({})).is_err());
        assert!(ToolCall::parse(READ_FILE, &json!({"path": "a", "extra": 1})).is_err());
        assert!(ToolCall::parse(READ_FILE, &json!({"path": 3})).is_err());
        assert!(ToolCall::parse(EDIT_DOCUMENT, &json!({"path": "a", "base_revision": -1, "edits": []})).is_err());
        assert!(ToolCall::parse(EDIT_DOCUMENT, &json!({"path": "a", "base_revision": 2, "edits": []})).is_err());
        assert!(ToolCall::parse("delete_everything", &json!({})).is_err());
        // Every documented example input validates.
        let ok = [
            (LIST_FILES, json!({"path": "", "recursive": true})),
            (EDIT_DOCUMENT, json!({"path": "a", "base_revision": 2, "edits": [{"old_text": "x", "new_text": "y"}]})),
            (CREATE_FILE, json!({"path": "a", "content": ""})),
            (BUILD_DOCUMENT, json!({"entry": "main.typ", "engine": "auto"})),
            (
                INSERT_ARTIFACT,
                json!({"path": "main.typ", "run_id": "r", "artifact_id": "a", "after_text": "", "caption": "", "update_key": ""}),
            ),
            (RUN_PYTHON, json!({"code": "print(1)"})),
            (CHECK_LEAN, json!({"path": "Main.lean"})),
            (RUN_COMMAND, json!({"program": "latexmk", "args": ["-pdf"], "working_directory": ""})),
            (
                GRAPH_QUERY,
                json!({"query": "search", "text": "", "kinds": ["claim"], "entity": "", "path": "", "depth": 1, "limit": 20}),
            ),
            (
                WORKFLOW_PROPOSE,
                json!({"workflow_id": "", "expected_revision": -1, "title": "Census", "description": "", "preset": "",
                    "inputs": [{"id": "code", "label": "Code", "port_type": "codeSource", "path": "", "pinned_hash": ""}],
                    "steps": [{"id": "run", "label": "Run", "operation": "research.pythonBatch", "version": 1, "parameters": [], "payload": {"kind": "none"}}],
                    "connections": [{"kind": "data", "from_step": "", "from_port": "", "from_input": "code", "to_step": "run", "to_port": "script"}],
                    "outcomes": [], "ceiling": {"execution": "none", "model_requests": false, "retain_results": false, "propose_changes": false}}),
            ),
            (
                GRAPH_PROPOSE,
                json!({"summary": "", "base_revision": 3, "operations": [
                    {"op": "create_entity", "temp_id": "c1", "kind": "claim", "label": "Squares grow", "attributes": [], "bind_path": "main.typ", "bind_start": -1, "bind_end": -1},
                    {"op": "add_relation", "relation": "dependsOn", "from": "c1", "to": "ent-0123456789abcdef0123456789abcdef", "evidence_kind": "", "evidence": [], "note": ""}
                ]}),
            ),
        ];
        for (name, input) in ok {
            assert!(ToolCall::parse(name, &input).is_ok(), "{name}");
        }
    }
}

#[cfg(test)]
mod strict_tests {
    use super::*;

    /// The provider compiles every strict schema of a request into one
    /// grammar and rejects the request ("The compiled grammar is too large")
    /// past a limit, which failed every live agent task once the graph tools
    /// were strict. Keep the strict set to the tools proven within the limit
    /// in live runs, and bound its size.
    #[test]
    fn strict_tool_grammar_stays_within_the_provider_limit() {
        let tools = definitions();
        let strict: Vec<&str> = tools.iter().filter(|t| t.strict).map(|t| t.name).collect();
        assert_eq!(
            strict,
            vec![
                LIST_FILES,
                READ_FILE,
                EDIT_DOCUMENT,
                CREATE_FILE,
                BUILD_DOCUMENT,
                INSERT_ARTIFACT,
                RUN_PYTHON,
                CHECK_LEAN,
                RUN_COMMAND
            ],
        );
        let strict_size: usize = tools.iter().filter(|t| t.strict).map(|t| t.input_schema.to_string().len()).sum();
        assert!(strict_size <= 8_000, "strict schemas total {strict_size} bytes");
        for t in tools.iter().filter(|t| !t.strict) {
            assert_eq!(t.input_schema["additionalProperties"], false, "{} still refuses unknown fields", t.name);
        }
    }
}
