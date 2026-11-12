//! Structured tools offered to the model.
//!
//! Every tool has a strict JSON schema (sent with `strict: true`) and a typed
//! Rust input with `deny_unknown_fields`; with eager input streaming the API
//! does not validate inputs, so each call is deserialized and validated here
//! before anything runs. Invalid input becomes an error tool result.
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

/// The tool list, in a fixed order.
pub fn definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: LIST_FILES,
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
            description: "Typeset a document (Typst .typ or LaTeX .tex) from the current editor state, unsaved \
                changes included, and report the real status, diagnostics with locations, and page count. Use it to \
                check that your changes build; report its result honestly, including failures. A Typst or Markdown \
                build reads only files in the task's scope; a LaTeX build runs TeX natively, so it needs a task \
                scoped to the whole project and the user's approval.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "entry": {"type": "string", "description": "Entry file relative to the project root, e.g. main.typ or paper.tex."},
                    "engine": {"type": "string", "enum": ["auto", "pdflatex", "lualatex"], "description": "LaTeX engine; auto uses the project's setting (ignored for Typst)."}
                },
                "required": ["entry", "engine"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: INSERT_ARTIFACT,
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
    ]
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
    /// List a folder (names and sizes) or use it as a working folder.
    Folder,
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
}

impl ToolCall {
    /// What this call does (see [`Effects`]).
    pub fn effects(&self) -> Effects<'_> {
        use services::ServiceKind;
        let effects =
            |paths, exposes, launch, changes, service, what| Effects { paths, exposes, launch, changes, service, what };
        match self {
            ToolCall::ListFiles(input) => {
                let paths =
                    if is_root(&input.path) { Vec::new() } else { vec![(input.path.as_str(), PathUse::Folder)] };
                effects(paths, Exposure::Metadata, Launch::None, false, None, "Listing files")
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
                    vec![(input.working_directory.as_str(), PathUse::Folder)]
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
    }

    #[test]
    fn schemas_are_strict_and_unique() {
        let defs = definitions();
        let mut names: Vec<_> = defs.iter().map(|d| d.name).collect();
        for def in &defs {
            object_schemas_are_strict(&def.input_schema, def.name);
        }
        names.dedup();
        assert_eq!(names.len(), 9);
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
        ];
        for (name, input) in ok {
            assert!(ToolCall::parse(name, &input).is_ok(), "{name}");
        }
    }
}
