//! Declarative interchange: workflow bundles.
//!
//! A bundle ([`BUNDLE_SCHEMA`]) carries one root definition and every saved
//! workflow revision its composite steps run (recursively), each with the id
//! and revision it had where it was exported, so references can be rewritten.
//! A bundle holds definitions only: no validation results, origins, trust,
//! approvals, run plans, or triggers — and the strict parser refuses any of
//! them (unknown fields anywhere, including keys a tagged unit variant would
//! otherwise ignore).
//!
//! Importing treats the bundle as untrusted data: bounded sizes, strict
//! schema, every reference resolvable inside the bundle, no recursion (by
//! workflow id, any revision), nesting at most [`MAX_COMPOSITE_DEPTH`], and
//! no unreferenced extra workflows. Every structural check runs before
//! anything is saved. Children are saved first as imported drafts (fresh ids;
//! several revisions of one exported workflow become revisions of one new
//! workflow), references are rewritten to the new ids and revisions, and each
//! definition is validated like any other save. Nothing is approved, trusted,
//! triggered, or run by an import.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::persist::{SaveWorkflowRequest, SavedWorkflow};
use super::v2::MAX_COMPOSITE_DEPTH;
use super::{CompositeRef, MAX_DEFINITION_BYTES, ValidationContext, WORKFLOW_SCHEMAS, WorkflowDefinition, dropped_key};
use crate::ids::WorkflowId;
use crate::model::Origin;
use crate::service::{GraphError, GraphResult, GraphService};

/// Schema identifier of a workflow bundle.
pub const BUNDLE_SCHEMA: &str = "exposition.workflow-bundle/1";
/// Bundles larger than this are refused before parsing.
pub const MAX_BUNDLE_BYTES: usize = 8 * 1024 * 1024;
/// Composite children a bundle may carry.
pub const MAX_BUNDLE_CHILDREN: usize = 32;

/// A root workflow definition and the composite children it runs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowBundle {
    /// Must be [`BUNDLE_SCHEMA`].
    pub schema: String,
    pub root: BundledWorkflow,
    /// Every workflow revision a composite step (of the root or of another
    /// child) runs, each once.
    pub children: Vec<BundledWorkflow>,
}

/// One workflow revision in a bundle, with the identity it had where it was
/// exported (composite references in the bundle use it).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BundledWorkflow {
    pub id: WorkflowId,
    pub revision: u64,
    pub definition: WorkflowDefinition,
}

/// A child saved by an import.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ImportedChild {
    /// Its identity in the bundle.
    pub original: CompositeRef,
    /// Its new identity in this project.
    pub saved: CompositeRef,
    pub title: String,
    pub state: super::DraftState,
}

/// What an import saved (all drafts; nothing runs).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowBundleImport {
    pub root: SavedWorkflow,
    /// Children in the order they were saved (dependencies first).
    pub children: Vec<ImportedChild>,
}

/// Where an export was written.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowBundleExport {
    /// The file written (as the person chose it).
    pub path: String,
    pub bytes: u64,
    /// Composite children carried with the root.
    pub children: u32,
}

fn invalid(message: impl Into<String>) -> GraphError {
    GraphError::new("workflow.bundleInvalid", message)
}

/// The JSON text of a bundle (what an export writes).
pub fn bundle_text(bundle: &WorkflowBundle) -> Result<String, String> {
    let text = serde_json::to_string_pretty(bundle).map_err(|e| e.to_string())?;
    if text.len() > MAX_BUNDLE_BYTES {
        return Err(format!("the workflow bundle would be larger than {} MiB", MAX_BUNDLE_BYTES / (1024 * 1024)));
    }
    Ok(text)
}

/// What an interchange file holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InterchangeFile {
    Bundle(WorkflowBundle),
    /// A single definition (the M8 draft JSON).
    Definition(WorkflowDefinition),
}

/// Parse untrusted interchange text strictly: a bundle or a single definition.
pub fn parse_interchange(text: &str) -> Result<InterchangeFile, String> {
    if text.len() > MAX_BUNDLE_BYTES {
        return Err(format!("the file is larger than {} MiB", MAX_BUNDLE_BYTES / (1024 * 1024)));
    }
    let raw: serde_json::Value = serde_json::from_str(text).map_err(|e| format!("the file is not valid JSON: {e}"))?;
    let schema = raw.get("schema").and_then(|s| s.as_str()).unwrap_or_default().to_owned();
    if WORKFLOW_SCHEMAS.contains(&schema.as_str()) {
        if text.len() > MAX_DEFINITION_BYTES {
            return Err(format!("the workflow definition is larger than {} KiB", MAX_DEFINITION_BYTES / 1024));
        }
        return super::definition_from_value(raw).map(InterchangeFile::Definition);
    }
    if schema != BUNDLE_SCHEMA {
        return Err(format!(
            "unsupported file schema {:?} (this version reads {BUNDLE_SCHEMA} and single workflow definitions)",
            super::truncate(&schema, 60)
        ));
    }
    let bundle: WorkflowBundle =
        serde_json::from_value(raw.clone()).map_err(|e| format!("the workflow bundle is not valid: {e}"))?;
    let carried = serde_json::to_value(&bundle).map_err(|e| e.to_string())?;
    if let Some(at) = dropped_key(&raw, &carried, String::new()) {
        return Err(format!("the workflow bundle is not valid: unknown field at {at}"));
    }
    Ok(InterchangeFile::Bundle(bundle))
}

/// Structural checks of an untrusted bundle (before anything is saved).
/// Returns the children in save order (dependencies first; revisions of one
/// exported workflow in increasing order).
pub fn check_bundle(bundle: &WorkflowBundle) -> Result<Vec<usize>, String> {
    if bundle.schema != BUNDLE_SCHEMA {
        return Err(format!("unsupported bundle schema {:?}", super::truncate(&bundle.schema, 60)));
    }
    if bundle.children.len() > MAX_BUNDLE_CHILDREN {
        return Err(format!(
            "a bundle can carry at most {MAX_BUNDLE_CHILDREN} composite workflows (it has {})",
            bundle.children.len()
        ));
    }
    let entries: Vec<&BundledWorkflow> = std::iter::once(&bundle.root).chain(&bundle.children).collect();
    let mut index: HashMap<(WorkflowId, u64), usize> = HashMap::new();
    for (i, e) in entries.iter().enumerate() {
        if e.revision == 0 {
            return Err(format!("workflow {} has revision 0; revisions start at 1", e.id));
        }
        let size = serde_json::to_vec(&e.definition).map(|v| v.len()).unwrap_or(usize::MAX);
        if size > MAX_DEFINITION_BYTES {
            return Err(format!("workflow {} is larger than {} KiB", e.id, MAX_DEFINITION_BYTES / 1024));
        }
        if index.insert((e.id.clone(), e.revision), i).is_some() {
            return Err(format!("workflow {} revision {} appears twice", e.id, e.revision));
        }
    }
    let refs = |e: &BundledWorkflow| -> Vec<CompositeRef> {
        e.definition.steps.iter().filter_map(|s| s.composite.clone()).collect()
    };
    // Every reference is to a child in the bundle (never the root, at any revision).
    for e in &entries {
        for r in refs(e) {
            if r.workflow == bundle.root.id {
                return Err(format!(
                    "workflow {} includes the bundle's root workflow; a workflow cannot include itself",
                    e.id
                ));
            }
            match index.get(&(r.workflow.clone(), r.revision)) {
                Some(0) | None => {
                    return Err(format!(
                        "workflow {} runs workflow {} revision {}, which the bundle does not carry",
                        e.id, r.workflow, r.revision
                    ));
                }
                Some(_) => {}
            }
        }
    }
    // No recursion (a workflow id twice on one path) and bounded depth; every child is used.
    fn walk(
        entries: &[&BundledWorkflow],
        index: &HashMap<(WorkflowId, u64), usize>,
        at: usize,
        path: &mut Vec<WorkflowId>,
        used: &mut BTreeSet<usize>,
        steps: &mut usize,
    ) -> Result<(), String> {
        *steps += 1;
        if *steps > super::v2::MAX_COMPOSITE_EXPANSIONS {
            return Err("the bundle's composites expand too far".into());
        }
        let e = entries[at];
        if path.contains(&e.id) {
            return Err(format!("workflow {} includes itself (directly or through another composite)", e.id));
        }
        if path.len() > MAX_COMPOSITE_DEPTH {
            return Err(format!("composites nest more than {MAX_COMPOSITE_DEPTH} levels deep"));
        }
        used.insert(at);
        path.push(e.id.clone());
        for s in &e.definition.steps {
            if let Some(r) = &s.composite
                && let Some(&next) = index.get(&(r.workflow.clone(), r.revision))
            {
                walk(entries, index, next, path, used, steps)?;
            }
        }
        path.pop();
        Ok(())
    }
    let mut used = BTreeSet::new();
    let mut steps = 0usize;
    walk(&entries, &index, 0, &mut Vec::new(), &mut used, &mut steps)?;
    if let Some(unused) = (1..entries.len()).find(|i| !used.contains(i)) {
        return Err(format!(
            "workflow {} revision {} is not used by the root workflow; a bundle carries only what the root runs",
            entries[unused].id, entries[unused].revision
        ));
    }
    // Save order: dependencies first, and earlier revisions of one workflow first.
    let n = bundle.children.len();
    let mut before: BTreeMap<usize, BTreeSet<usize>> = (0..n).map(|i| (i, BTreeSet::new())).collect();
    for (i, child) in bundle.children.iter().enumerate() {
        for r in refs(child) {
            if let Some(&j) = index.get(&(r.workflow.clone(), r.revision)) {
                before.get_mut(&i).expect("child").insert(j - 1);
            }
        }
        for (j, other) in bundle.children.iter().enumerate() {
            if other.id == child.id && other.revision < child.revision {
                before.get_mut(&i).expect("child").insert(j);
            }
        }
    }
    let mut order = Vec::new();
    let mut done = BTreeSet::new();
    while order.len() < n {
        let next = (0..n).find(|i| !done.contains(i) && before[i].iter().all(|j| done.contains(j)));
        let Some(next) = next else {
            return Err("the bundle's workflows depend on each other in a cycle".into());
        };
        done.insert(next);
        order.push(next);
    }
    Ok(order)
}

fn rewrite(definition: &WorkflowDefinition, mapping: &HashMap<(WorkflowId, u64), CompositeRef>) -> WorkflowDefinition {
    let mut out = definition.clone();
    for step in &mut out.steps {
        if let Some(r) = &mut step.composite
            && let Some(new) = mapping.get(&(r.workflow.clone(), r.revision))
        {
            *r = new.clone();
        }
    }
    out
}

impl GraphService {
    /// Export a saved workflow revision (the latest when `revision` is none)
    /// with every composite child it runs.
    pub fn export_workflow_bundle(&self, id: &WorkflowId, revision: Option<u64>) -> GraphResult<WorkflowBundle> {
        let root = self.workflow(id, revision)?;
        let mut children: Vec<BundledWorkflow> = Vec::new();
        let mut seen: BTreeSet<(WorkflowId, u64)> = BTreeSet::new();
        // (definition, path of workflow ids above it)
        let mut queue: Vec<(WorkflowDefinition, Vec<WorkflowId>)> =
            vec![(root.definition.clone(), vec![root.id.clone()])];
        while let Some((definition, path)) = queue.pop() {
            for step in &definition.steps {
                let Some(r) = &step.composite else { continue };
                if path.contains(&r.workflow) {
                    return Err(GraphError::new(
                        "workflow.exportInvalid",
                        format!("step “{}” includes its own workflow; fix the draft before exporting it", step.label),
                    ));
                }
                if path.len() > MAX_COMPOSITE_DEPTH {
                    return Err(GraphError::new(
                        "workflow.exportInvalid",
                        format!("composites nest more than {MAX_COMPOSITE_DEPTH} levels deep"),
                    ));
                }
                if !seen.insert((r.workflow.clone(), r.revision)) {
                    continue;
                }
                let child = self.workflow_definition(&r.workflow, r.revision).ok_or_else(|| {
                    GraphError::new(
                        "workflow.exportInvalid",
                        format!(
                            "step “{}” runs workflow {} revision {}, which does not exist in this project; fix the draft before exporting it",
                            step.label, r.workflow, r.revision
                        ),
                    )
                })?;
                if children.len() >= MAX_BUNDLE_CHILDREN {
                    return Err(GraphError::new(
                        "workflow.exportInvalid",
                        format!("a bundle can carry at most {MAX_BUNDLE_CHILDREN} composite workflows"),
                    ));
                }
                let mut below = path.clone();
                below.push(r.workflow.clone());
                queue.push((child.clone(), below));
                children.push(BundledWorkflow { id: r.workflow.clone(), revision: r.revision, definition: child });
            }
        }
        children.sort_by(|a, b| (a.id.as_str(), a.revision).cmp(&(b.id.as_str(), b.revision)));
        let bundle = WorkflowBundle {
            schema: BUNDLE_SCHEMA.into(),
            root: BundledWorkflow { id: root.id, revision: root.revision, definition: root.definition },
            children,
        };
        bundle_text(&bundle).map_err(|m| GraphError::new("workflow.exportInvalid", m))?;
        Ok(bundle)
    }

    /// Import untrusted interchange text: a bundle (children first, references
    /// rewritten) or a single definition. Everything is saved as imported
    /// drafts with fresh ids; nothing is trusted, approved, triggered, or run.
    pub fn import_workflow_bundle(
        &self,
        origin: &Origin,
        text: &str,
        context: &ValidationContext<'_>,
    ) -> GraphResult<WorkflowBundleImport> {
        let bundle = match parse_interchange(text).map_err(invalid)? {
            InterchangeFile::Definition(definition) => {
                let request = SaveWorkflowRequest { id: None, expected_revision: None, definition };
                let root = self.save_definition(origin, &request, context, true)?;
                return Ok(WorkflowBundleImport { root, children: Vec::new() });
            }
            InterchangeFile::Bundle(bundle) => bundle,
        };
        let order = check_bundle(&bundle).map_err(invalid)?;
        // Callers' contexts never resolve against another project: references
        // resolve against this store once rewritten.
        let context = ValidationContext {
            ceiling: context.ceiling,
            file_exists: context.file_exists,
            resolve_workflow: None,
            workflow_id: None,
        };
        let mut mapping: HashMap<(WorkflowId, u64), CompositeRef> = HashMap::new();
        let mut new_ids: HashMap<WorkflowId, (WorkflowId, u64)> = HashMap::new();
        let mut children = Vec::new();
        for i in order {
            let child = &bundle.children[i];
            let definition = rewrite(&child.definition, &mapping);
            let (id, expected) = match new_ids.get(&child.id) {
                Some((id, latest)) => (Some(id.clone()), Some(*latest)),
                None => (None, None),
            };
            let request = SaveWorkflowRequest { id, expected_revision: expected, definition };
            let saved = self.save_definition(origin, &request, &context, true)?;
            new_ids.insert(child.id.clone(), (saved.id.clone(), saved.revision));
            let new = CompositeRef { workflow: saved.id.clone(), revision: saved.revision };
            mapping.insert((child.id.clone(), child.revision), new.clone());
            children.push(ImportedChild {
                original: CompositeRef { workflow: child.id.clone(), revision: child.revision },
                saved: new,
                title: saved.definition.title.clone(),
                state: saved.validation.state,
            });
        }
        let request = SaveWorkflowRequest {
            id: None,
            expected_revision: None,
            definition: rewrite(&bundle.root.definition, &mapping),
        };
        let root = self.save_definition(origin, &request, &context, true)?;
        Ok(WorkflowBundleImport { root, children })
    }
}
