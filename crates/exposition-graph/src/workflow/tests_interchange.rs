//! Workflow bundles: export with composite children, untrusted import.

use serde_json::{Value, json};

use super::interchange::*;
use super::tests_v2::{child_fit, parent, wrap};
use super::*;
use crate::model::Origin;
use crate::service::{GraphConfig, GraphService};

fn service() -> (tempfile::TempDir, tempfile::TempDir, std::sync::Arc<GraphService>) {
    let data = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let graph = GraphService::new(GraphConfig { data_dir: data.path().to_path_buf() });
    let status = graph.attach(&std::fs::canonicalize(project.path()).unwrap());
    assert!(status.is_usable(), "{status:?}");
    (data, project, graph)
}

fn save(
    graph: &GraphService,
    id: Option<&WorkflowId>,
    expected: Option<u64>,
    def: &WorkflowDefinition,
) -> SavedWorkflow {
    graph
        .save_workflow(
            &Origin::User,
            &SaveWorkflowRequest { id: id.cloned(), expected_revision: expected, definition: def.clone() },
            &ValidationContext::default(),
        )
        .unwrap()
}

/// A project with: child c (revisions 1 and 2), wrapper w@1 → c@1, and a
/// root running w@1 and c@2.
fn exporting_project() -> (tempfile::TempDir, tempfile::TempDir, std::sync::Arc<GraphService>, SavedWorkflow) {
    let (d, p, graph) = service();
    let c = save(&graph, None, None, &child_fit());
    let mut edited = child_fit();
    edited.steps[0].payload = Some(StepPayload::Code { language: CodeLanguage::Python, source: "print(2)".into() });
    let c2 = save(&graph, Some(&c.id), Some(1), &edited);
    assert_eq!(c2.revision, 2);
    let w = save(&graph, None, None, &wrap(&c.id, 1));
    let mut root = parent(&w.id, 1);
    let mut sub2 = root.steps[0].clone();
    sub2.id = "sub2".into();
    sub2.label = "Second fit".into();
    sub2.composite = Some(CompositeRef { workflow: c.id.clone(), revision: 2 });
    root.steps.insert(1, sub2);
    root.connections.push(WorkflowConnection::Data {
        from: PortSource::Input { input: "data".into() },
        to: StepPort { step: "sub2".into(), port: "data".into() },
    });
    root.connections.push(WorkflowConnection::Data {
        from: PortSource::Step { step: "sub2".into(), port: "fitted".into() },
        to: StepPort { step: "review".into(), port: "material".into() },
    });
    let saved = save(&graph, None, None, &root);
    assert_eq!(saved.validation.state, DraftState::WellFormed, "{:?}", saved.validation.diagnostics);
    (d, p, graph, saved)
}

fn bundle_json(graph: &GraphService, root: &SavedWorkflow) -> Value {
    let bundle = graph.export_workflow_bundle(&root.id, None).unwrap();
    serde_json::from_str(&bundle_text(&bundle).unwrap()).unwrap()
}

#[test]
fn export_carries_the_root_and_every_pinned_child_and_nothing_else() {
    let (_d, _p, graph, root) = exporting_project();
    let bundle = graph.export_workflow_bundle(&root.id, None).unwrap();
    assert_eq!(bundle.schema, BUNDLE_SCHEMA);
    assert_eq!((bundle.root.id.clone(), bundle.root.revision), (root.id.clone(), 1));
    assert_eq!(bundle.root.definition, root.definition);
    let carried: Vec<(String, u64)> =
        bundle.children.iter().map(|c| (c.definition.title.clone(), c.revision)).collect();
    assert_eq!(carried.len(), 3, "{carried:?}");
    assert!(carried.contains(&("Fit".into(), 1)) && carried.contains(&("Fit".into(), 2)));
    assert!(carried.contains(&("Wrapper".into(), 1)));
    // Only definitions: no validation, origin, runnability, trust, approval, or trigger.
    let json = bundle_json(&graph, &root);
    let keys: Vec<&str> = json.as_object().unwrap().keys().map(|k| k.as_str()).collect();
    assert_eq!(keys, vec!["children", "root", "schema"]);
    for entry in std::iter::once(&json["root"]).chain(json["children"].as_array().unwrap()) {
        let keys: Vec<&str> = entry.as_object().unwrap().keys().map(|k| k.as_str()).collect();
        assert_eq!(keys, vec!["definition", "id", "revision"]);
    }
    let text = json.to_string();
    for word in ["runnable", "approval", "trust", "trigger", "origin", "diagnostics"] {
        assert!(!text.contains(word), "{word} in the bundle");
    }
}

#[test]
fn import_saves_children_first_and_rewrites_references() {
    let (_d, _p, source, root) = exporting_project();
    let text = bundle_text(&source.export_workflow_bundle(&root.id, None).unwrap()).unwrap();
    let (_d2, _p2, target) = service();
    let out = target.import_workflow_bundle(&Origin::User, &text, &ValidationContext::default()).unwrap();
    // c@1 and c@2 become revisions 1 and 2 of one new workflow; w@1 a new workflow.
    assert_eq!(out.children.len(), 3);
    let fit: Vec<&ImportedChild> = out.children.iter().filter(|c| c.title == "Fit").collect();
    assert_eq!(fit.len(), 2);
    assert_eq!(fit[0].saved.workflow, fit[1].saved.workflow, "one new workflow, two revisions");
    assert_eq!((fit[0].original.revision, fit[0].saved.revision), (1, 1));
    assert_eq!((fit[1].original.revision, fit[1].saved.revision), (2, 2));
    let wrapper = out.children.iter().find(|c| c.title == "Wrapper").unwrap();
    for c in &out.children {
        assert_ne!(c.saved.workflow, c.original.workflow, "fresh ids");
    }
    assert!(
        out.children.iter().position(|c| c.title == "Wrapper").unwrap()
            > out.children.iter().position(|c| c.original.revision == 1 && c.title == "Fit").unwrap(),
        "dependencies first"
    );
    // References point at the new identities.
    let w = target.workflow(&wrapper.saved.workflow, Some(1)).unwrap();
    assert_eq!(w.definition.steps[0].composite, Some(fit[0].saved.clone()));
    let refs: Vec<CompositeRef> = out.root.definition.steps.iter().filter_map(|s| s.composite.clone()).collect();
    assert_eq!(refs, vec![wrapper.saved.clone(), fit[1].saved.clone()]);
    // Drafts, imported, validated against the new project, never runnable by import.
    assert_ne!(out.root.id, root.id);
    assert!(out.root.imported);
    assert_eq!(out.root.validation.state, DraftState::WellFormed, "{:?}", out.root.validation.diagnostics);
    assert!(!out.root.validation.runnable);
    let plan: Vec<&str> = out.root.validation.plan.iter().map(|p| p.step.as_str()).collect();
    assert_eq!(plan, vec!["sub/inner/fit", "sub2/fit", "review"]);
    let entity = target.entity_detail(out.root.entity.as_ref().unwrap()).unwrap().entity;
    assert_eq!(entity.attributes["workflow.imported"], "true");
    assert_eq!(entity.attributes["workflow.runnable"], "false");
    for c in &out.children {
        assert!(target.workflow(&c.saved.workflow, Some(c.saved.revision)).unwrap().imported);
    }
    assert_eq!(target.workflows().unwrap().len(), 3);
    // A second import makes another independent copy.
    let again = target.import_workflow_bundle(&Origin::User, &text, &ValidationContext::default()).unwrap();
    assert_ne!(again.root.id, out.root.id);
    assert_eq!(target.workflows().unwrap().len(), 6);
}

#[test]
fn a_single_definition_imports_as_one_draft() {
    let (_d, _p, graph) = service();
    let text = serde_json::to_string(&child_fit()).unwrap();
    let out = graph.import_workflow_bundle(&Origin::User, &text, &ValidationContext::default()).unwrap();
    assert!(out.children.is_empty());
    assert!(out.root.imported);
    assert!(!out.root.validation.runnable);
    assert_eq!(graph.workflows().unwrap().len(), 1);
}

#[test]
fn export_refuses_a_root_whose_children_are_missing() {
    let (_d, _p, graph) = service();
    let saved = save(&graph, None, None, &parent(&WorkflowId::mint(), 1));
    let err = graph.export_workflow_bundle(&saved.id, None).unwrap_err();
    assert_eq!(err.code, "workflow.exportInvalid");
}

/// Import `bundle` into a fresh project: it must be refused and nothing saved.
fn refused(bundle: &Value, why: &str) -> String {
    let (_d, _p, graph) = service();
    let text = bundle.to_string();
    let err = graph.import_workflow_bundle(&Origin::User, &text, &ValidationContext::default()).expect_err(why);
    assert_eq!(graph.workflows().unwrap().len(), 0, "{why}: nothing saved");
    err.message
}

fn child_index(bundle: &Value, title: &str, revision: u64) -> usize {
    bundle["children"]
        .as_array()
        .unwrap()
        .iter()
        .position(|c| c["definition"]["title"] == title && c["revision"] == revision)
        .unwrap()
}

#[test]
fn hostile_bundles_claiming_trust_approval_or_triggers_are_refused() {
    let (_d, _p, graph, root) = exporting_project();
    let base = bundle_json(&graph, &root);
    let fields = [
        "trust",
        "trusted",
        "approval",
        "approvals",
        "approvalHash",
        "trigger",
        "triggers",
        "standingHash",
        "runnable",
        "validation",
        "autoRun",
    ];
    for field in fields {
        let mut b = base.clone();
        b[field] = json!(true);
        let m = refused(&b, field);
        assert!(m.contains("unknown field") || m.contains(field), "{field}: {m}");
        let mut b = base.clone();
        b["root"][field] = json!({ "granted": true });
        refused(&b, field);
        let mut b = base.clone();
        b["children"][0][field] = json!("yes");
        refused(&b, field);
        let mut b = base.clone();
        b["root"]["definition"][field] = json!(true);
        refused(&b, field);
        let mut b = base.clone();
        b["children"][0]["definition"]["steps"][0][field] = json!({ "kind": "always" });
        refused(&b, field);
    }
    // A key a tagged unit variant would silently ignore.
    let mut b = base.clone();
    b["root"]["definition"]["inputs"][0]["source"]["version"] = json!({ "kind": "atRunStart", "approved": true });
    let m = refused(&b, "smuggled key");
    assert!(m.contains("unknown field"), "{m}");
    let mut b = base.clone();
    b["children"][0]["definition"]["inputs"][0]["source"] = json!({ "kind": "unset", "trust": true });
    refused(&b, "smuggled key in a child");
}

#[test]
fn hostile_bundles_with_bad_structure_are_refused() {
    let (_d, _p, graph, root) = exporting_project();
    let base = bundle_json(&graph, &root);
    let wrapper = child_index(&base, "Wrapper", 1);
    let fit1 = child_index(&base, "Fit", 1);
    let fit2 = child_index(&base, "Fit", 2);
    let root_id = base["root"]["id"].clone();
    let fit_id = base["children"][fit1]["id"].clone();
    let wrapper_id = base["children"][wrapper]["id"].clone();

    // Oversized, not JSON, wrong or missing schema.
    let (_d2, _p2, empty) = service();
    let huge = format!("{{\"schema\":\"{BUNDLE_SCHEMA}\",\"pad\":\"{}\"}}", "x".repeat(MAX_BUNDLE_BYTES));
    assert!(empty.import_workflow_bundle(&Origin::User, &huge, &ValidationContext::default()).is_err());
    assert!(empty.import_workflow_bundle(&Origin::User, "{not json", &ValidationContext::default()).is_err());
    let mut b = base.clone();
    b["schema"] = json!("exposition.workflow-bundle/2");
    refused(&b, "wrong schema");
    let mut b = base.clone();
    b.as_object_mut().unwrap().remove("schema");
    refused(&b, "missing schema");
    assert_eq!(empty.workflows().unwrap().len(), 0);

    // Recursion through ids: a child runs the root (any revision).
    let mut b = base.clone();
    b["children"][fit1]["definition"] =
        serde_json::to_value(wrap(&WorkflowId::parse(root_id.as_str().unwrap()).unwrap(), 9)).unwrap();
    let m = refused(&b, "child runs the root");
    assert!(m.contains("itself"), "{m}");
    // A child runs itself.
    let mut b = base.clone();
    b["children"][wrapper]["definition"]["steps"][0]["composite"] = json!({ "workflow": wrapper_id, "revision": 1 });
    let m = refused(&b, "self reference");
    assert!(m.contains("itself"), "{m}");
    // Two children run each other: wrapper → fit@1 → wrapper.
    let mut b = base.clone();
    b["children"][fit1]["definition"] =
        serde_json::to_value(wrap(&WorkflowId::parse(wrapper_id.as_str().unwrap()).unwrap(), 1)).unwrap();
    let m = refused(&b, "cycle between children");
    assert!(m.contains("itself"), "{m}");
    // One workflow id twice on a path (fit@2 runs fit@1).
    let mut b = base.clone();
    b["children"][fit2]["definition"] =
        serde_json::to_value(wrap(&WorkflowId::parse(fit_id.as_str().unwrap()).unwrap(), 1)).unwrap();
    refused(&b, "a revision running another revision of itself");

    // A reference the bundle does not carry.
    let mut b = base.clone();
    b["children"].as_array_mut().unwrap().remove(fit1);
    let m = refused(&b, "missing child");
    assert!(m.contains("does not carry"), "{m}");
    let mut b = base.clone();
    b["root"]["definition"]["steps"][0]["composite"]["revision"] = json!(7);
    refused(&b, "wrong revision");
    // An extra child nothing uses.
    let mut b = base.clone();
    let mut extra = b["children"][fit1].clone();
    extra["id"] = json!(WorkflowId::mint().as_str());
    b["children"].as_array_mut().unwrap().push(extra);
    let m = refused(&b, "unused child");
    assert!(m.contains("not used"), "{m}");
    // The same revision twice.
    let mut b = base.clone();
    let dup = b["children"][fit1].clone();
    b["children"].as_array_mut().unwrap().push(dup);
    refused(&b, "duplicate entry");
    // Revision 0, malformed ids.
    let mut b = base.clone();
    b["root"]["revision"] = json!(0);
    refused(&b, "revision 0");
    let mut b = base.clone();
    b["children"][fit1]["id"] = json!("../../evil");
    refused(&b, "malformed id");
    let mut b = base.clone();
    b["children"][fit1]["revision"] = json!(-1);
    refused(&b, "negative revision");
    // Too many children.
    let mut b = base.clone();
    let template = b["children"][fit1].clone();
    let list = b["children"].as_array_mut().unwrap();
    for _ in 0..MAX_BUNDLE_CHILDREN {
        let mut c = template.clone();
        c["id"] = json!(WorkflowId::mint().as_str());
        list.push(c);
    }
    let m = refused(&b, "too many children");
    assert!(m.contains("at most"), "{m}");
    // A definition over the size limit.
    let mut b = base.clone();
    b["children"][fit1]["definition"]["steps"][0]["payload"]["source"] = json!("#".repeat(MAX_DEFINITION_BYTES + 1));
    refused(&b, "oversized definition");
}

#[test]
fn bundles_nest_at_most_three_levels() {
    // root → l1 → l2 → l3 → leaf: four levels below the root.
    let ids: Vec<WorkflowId> = (0..5).map(|_| WorkflowId::mint()).collect();
    let entry = |i: usize, def: WorkflowDefinition| json!({ "id": ids[i].as_str(), "revision": 1, "definition": def });
    let bundle = |depth: usize| {
        let mut children = vec![entry(1, child_fit())];
        for i in 2..=depth {
            children.push(entry(i, wrap(&ids[i - 1], 1)));
        }
        json!({
            "schema": BUNDLE_SCHEMA,
            "root": entry(0, parent(&ids[depth], 1)),
            "children": children,
        })
    };
    let (_d, _p, graph) = service();
    let ok = graph.import_workflow_bundle(&Origin::User, &bundle(3).to_string(), &ValidationContext::default());
    let ok = ok.unwrap();
    assert_eq!(ok.root.validation.state, DraftState::WellFormed, "{:?}", ok.root.validation.diagnostics);
    let m = refused(&bundle(4), "four levels");
    assert!(m.contains("levels"), "{m}");
}
