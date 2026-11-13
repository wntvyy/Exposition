use super::presets::{FORMAL_AUDIT, NUMERICAL_EXPERIMENT, WorkflowPresetRequest, draft};
use super::*;
use crate::model::Origin;
use crate::service::{GraphConfig, GraphService};

fn path(p: &str) -> WorkspacePath {
    WorkspacePath::parse(p).unwrap()
}

fn numerical(manuscript: bool) -> WorkflowDefinition {
    draft(&WorkflowPresetRequest {
        preset: NUMERICAL_EXPERIMENT.into(),
        script: Some(path("analysis/run.py")),
        data: Some(path("data/obs.csv")),
        manuscript: manuscript.then(|| path("main.typ")),
        lean_file: None,
    })
    .unwrap()
}

fn audit() -> WorkflowDefinition {
    draft(&WorkflowPresetRequest {
        preset: FORMAL_AUDIT.into(),
        lean_file: Some(path("Proofs/Main.lean")),
        ..Default::default()
    })
    .unwrap()
}

fn validate(def: &WorkflowDefinition) -> WorkflowValidation {
    validate_workflow(def, &ValidationContext::default())
}

fn codes(v: &WorkflowValidation) -> Vec<(String, Option<u8>)> {
    v.diagnostics.iter().filter(|d| d.severity == DiagnosticSeverity::Error).map(|d| (d.code.clone(), d.rule)).collect()
}

fn has(v: &WorkflowValidation, code: &str, rule: Option<u8>) -> bool {
    codes(v).iter().any(|(c, r)| c == code && *r == rule)
}

#[test]
fn presets_are_well_formed_and_not_runnable_from_validation_alone() {
    for def in [numerical(true), numerical(false), audit()] {
        let v = validate(&def);
        assert_eq!(codes(&v), vec![], "{}: {:?}", def.title, v.diagnostics);
        assert_eq!(v.state, DraftState::WellFormed);
        assert!(!v.runnable, "validation alone never makes a draft runnable");
        assert_eq!(v.runnable_reason, NOT_RUNNABLE_REASON);
        assert!(!v.plan.is_empty());
    }
    let plan: Vec<String> = validate(&numerical(true)).plan.into_iter().map(|p| p.step).collect();
    assert_eq!(plan, vec!["run", "retain", "promote"], "dependency order");
}

#[test]
fn preset_without_files_is_an_incomplete_draft() {
    let def = draft(&WorkflowPresetRequest { preset: FORMAL_AUDIT.into(), ..Default::default() }).unwrap();
    let v = validate(&def);
    assert_eq!(v.state, DraftState::Incomplete);
    assert!(has(&v, "workflow.inputUnset", Some(1)));
    assert!(!v.runnable);
}

#[test]
fn numerical_preset_without_a_script_starts_from_an_inline_template() {
    let def = draft(&WorkflowPresetRequest { preset: NUMERICAL_EXPERIMENT.into(), ..Default::default() }).unwrap();
    let v = validate(&def);
    assert_eq!(codes(&v), vec![], "{:?}", v.diagnostics);
    let run = def.steps.iter().find(|s| s.id == "run").unwrap();
    let Some(StepPayload::Code { source, .. }) = &run.payload else { panic!("inline code") };
    assert!(source.contains("EXPOSITION_OUTPUT_DIR") && source.contains("figure.svg") && source.contains("table.csv"));
    assert!(source.contains("DATA = None"));
    assert!(!source.contains("numpy") && !source.contains("matplotlib"), "pure Python");
    let with_data = draft(&WorkflowPresetRequest {
        preset: NUMERICAL_EXPERIMENT.into(),
        data: Some(path("data/obs.csv")),
        ..Default::default()
    })
    .unwrap();
    let Some(StepPayload::Code { source, .. }) = &with_data.steps[0].payload else { panic!("inline code") };
    assert!(source.contains("DATA = \"data/obs.csv\""));
    assert_eq!(validate(&with_data).state, DraftState::WellFormed);
}

#[test]
fn runnable_only_when_well_formed_and_every_runner_is_available() {
    let def = numerical(true);
    let mut v = validate(&def);
    assert!(!v.runnable && v.runnable_reason == NOT_RUNNABLE_REASON, "validation alone never decides");
    apply_runnability(&mut v, &def, &|_| Ok(()));
    assert!(v.runnable, "{}", v.runnable_reason);
    let mut v = validate(&def);
    apply_runnability(&mut v, &def, &|id| {
        if id == "restrictedPython" { Err("the restricted image is not configured".into()) } else { Ok(()) }
    });
    assert!(!v.runnable);
    assert!(v.runnable_reason.contains("the restricted image is not configured"), "{}", v.runnable_reason);
    assert!(v.runnable_reason.contains("Run the experiment"), "{}", v.runnable_reason);
    // An incomplete draft is never runnable, whatever the runners say.
    let incomplete = draft(&WorkflowPresetRequest { preset: FORMAL_AUDIT.into(), ..Default::default() }).unwrap();
    let mut v = validate(&incomplete);
    apply_runnability(&mut v, &incomplete, &|_| Ok(()));
    assert!(!v.runnable);
    assert!(v.runnable_reason.contains("error"), "{}", v.runnable_reason);
    // The registry reports availability per runner.
    let ops = with_availability(&|id| if id == "leanAudit" { Err("untrusted".into()) } else { Ok(()) });
    for op in ops {
        assert_eq!(op.runner.available, op.runner.id != "leanAudit", "{}", op.id);
    }
}

#[test]
fn rule1_required_input_must_be_bound() {
    let mut def = audit();
    def.connections.retain(|c| !matches!(c, WorkflowConnection::Data { to, .. } if to.step == "associate"));
    let v = validate(&def);
    assert!(has(&v, "workflow.unboundInput", Some(1)), "{:?}", v.diagnostics);
    assert_eq!(v.state, DraftState::Incomplete);
}

#[test]
fn rule1_external_inputs_are_versioned_project_files() {
    let mut def = audit();
    def.inputs[0].source = InputSource::Resource {
        path: path("Proofs/Main.lean"),
        version: InputVersion::Pinned { content_hash: "abc".into() },
    };
    assert!(has(&validate(&def), "workflow.inputVersion", Some(1)));
    // A file the project does not have.
    let mut def = audit();
    let exists = |_: &WorkspacePath| false;
    let v = validate_workflow(&def, &ValidationContext { file_exists: Some(&exists), ..Default::default() });
    assert!(has(&v, "workflow.inputMissing", Some(1)));
    // Graph storage and run records are never inputs.
    def.inputs[0].source =
        InputSource::Resource { path: path(".exposition/graph.sqlite3"), version: InputVersion::AtRunStart };
    let v = validate(&def);
    assert!(has(&v, "workflow.inputNotResource", Some(4)), "{:?}", v.diagnostics);
}

#[test]
fn rule2_orphan_steps_and_disconnected_components_are_rejected() {
    // An extra review step with its own input but no outcome.
    let mut def = numerical(false);
    def.ceiling.model_requests = true;
    def.steps.push(WorkflowStep {
        id: "review".into(),
        label: "Review the data".into(),
        operation: StepOperation { id: "agent.review".into(), version: 1 },
        parameters: BTreeMap::new(),
        payload: Some(StepPayload::Prompt { text: "Summarize the data.".into() }),
    });
    def.connections.push(WorkflowConnection::Data {
        from: PortSource::Input { input: "data".into() },
        to: StepPort { step: "review".into(), port: "material".into() },
    });
    let v = validate(&def);
    assert!(has(&v, "workflow.orphanStep", Some(2)), "{:?}", v.diagnostics);
    // Declaring its report as an outcome makes it contribute.
    def.outcomes.push(WorkflowOutcome {
        id: "report".into(),
        kind: OutcomeKind::InvestigationReport,
        label: "Data review".into(),
        from: StepPort { step: "review".into(), port: "report".into() },
        optional: false,
    });
    assert_eq!(codes(&validate(&def)), vec![]);
    // Unused optional outputs (run.dataset, run.table) are fine: no error above.
}

#[test]
fn rule2_needs_a_required_outcome() {
    let mut def = audit();
    def.outcomes.clear();
    assert!(has(&validate(&def), "workflow.noOutcome", Some(2)));
    let mut def = numerical(true);
    for o in &mut def.outcomes {
        o.optional = true;
    }
    assert!(has(&validate(&def), "workflow.noOutcome", Some(2)));
}

#[test]
fn rule3_incompatible_types_are_refused() {
    let mut def = numerical(false);
    // A dataset cannot be read as code.
    def.connections.push(WorkflowConnection::Data {
        from: PortSource::Input { input: "data".into() },
        to: StepPort { step: "retain".into(), port: "artifact".into() },
    });
    def.connections.retain(
        |c| !matches!(c, WorkflowConnection::Data { from: PortSource::Step { step, .. }, .. } if step == "run"),
    );
    def.connections.push(WorkflowConnection::Data {
        from: PortSource::Step { step: "run".into(), port: "table".into() },
        to: StepPort { step: "run".into(), port: "script".into() },
    });
    let v = validate(&def);
    assert!(has(&v, "workflow.cycle", None), "self-feeding is refused: {:?}", v.diagnostics);
    // Figure into a code port.
    let mut def = numerical(false);
    def.inputs.push(WorkflowInput {
        id: "fig".into(),
        label: "A figure".into(),
        port_type: PortType::Figure,
        source: InputSource::Resource { path: path("figs/a.png"), version: InputVersion::AtRunStart },
    });
    def.connections.retain(|c| !matches!(c, WorkflowConnection::Data { to, .. } if to.port == "script"));
    def.connections.push(WorkflowConnection::Data {
        from: PortSource::Input { input: "fig".into() },
        to: StepPort { step: "run".into(), port: "script".into() },
    });
    assert!(has(&validate(&def), "workflow.incompatibleTypes", Some(3)));
}

#[test]
fn rule3_annotations_and_semantic_edges_never_satisfy_execution() {
    let mut def = audit();
    // Replace the data connection into `associate` by a decorative line
    // labelled with a semantic relation.
    def.connections.retain(|c| !matches!(c, WorkflowConnection::Data { to, .. } if to.step == "associate"));
    def.connections.push(WorkflowConnection::Annotation {
        from: "audit".into(),
        to: "associate".into(),
        label: "uses the audit".into(),
        relation: Some(RelationKind::DependsOn),
    });
    let v = validate(&def);
    let unbound = v.diagnostics.iter().find(|d| d.code == "workflow.unboundInput").expect("still unbound");
    assert_eq!(unbound.rule, Some(1));
    assert!(unbound.message.contains("annotation line does not deliver data"), "{}", unbound.message);
    // Nor does the annotation make `associate` contribute or order it.
    assert!(!v.plan.is_empty());
    // A prerequisite orders execution but delivers no data.
    def.connections.push(WorkflowConnection::Prerequisite { before: "audit".into(), after: "associate".into() });
    let v = validate(&def);
    assert!(has(&v, "workflow.unboundInput", Some(1)), "a prerequisite is not data");
}

#[test]
fn rule3_promotion_cannot_skip_retention() {
    // Connecting the raw figure straight into promotion would promote an
    // unretained, unvalidated output implicitly.
    let mut def = numerical(true);
    def.connections.retain(|c| !matches!(c, WorkflowConnection::Data { to, .. } if to.step == "promote"));
    def.connections.push(WorkflowConnection::Data {
        from: PortSource::Step { step: "run".into(), port: "figure".into() },
        to: StepPort { step: "promote".into(), port: "artifact".into() },
    });
    let v = validate(&def);
    assert!(has(&v, "workflow.serviceOwnedType", Some(6)), "{:?}", v.diagnostics);
}

#[test]
fn rule4_effects_must_fit_the_ceiling() {
    // A formal audit runs Lean natively: refused under a restricted ceiling.
    let mut def = audit();
    def.ceiling.execution = ExecutionCeiling::Restricted;
    let v = validate(&def);
    assert!(has(&v, "workflow.effectNotAllowed", Some(4)), "{:?}", v.diagnostics);
    // A workflow cannot claim more than its requester (e.g. an agent task) has.
    let def = audit();
    let ctx = ValidationContext { ceiling: Some(AuthorityCeiling::NONE), ..Default::default() };
    assert!(has(&validate_workflow(&def, &ctx), "workflow.ceilingExceeded", Some(4)));
    // Model requests need explicit permission.
    let mut def = numerical(false);
    def.steps.push(WorkflowStep {
        id: "review".into(),
        label: "Review".into(),
        operation: StepOperation { id: "agent.review".into(), version: 1 },
        parameters: BTreeMap::new(),
        payload: Some(StepPayload::Prompt { text: "Review the figure".into() }),
    });
    assert!(has(&validate(&def), "workflow.effectNotAllowed", Some(4)));
}

#[test]
fn rule6_forged_formal_evidence_is_refused() {
    // Python output wired into the evidence input of the association step.
    let mut def = audit();
    def.ceiling.execution = ExecutionCeiling::TrustedNative;
    def.inputs.push(WorkflowInput {
        id: "script".into(),
        label: "Forger".into(),
        port_type: PortType::CodeSource,
        source: InputSource::Resource { path: path("forge.py"), version: InputVersion::AtRunStart },
    });
    def.steps.push(WorkflowStep {
        id: "forge".into(),
        label: "Print matching JSON".into(),
        operation: StepOperation { id: "research.nativePython".into(), version: 1 },
        parameters: BTreeMap::new(),
        payload: None,
    });
    def.connections.retain(|c| !matches!(c, WorkflowConnection::Data { to, .. } if to.step == "associate"));
    def.connections.push(WorkflowConnection::Data {
        from: PortSource::Input { input: "script".into() },
        to: StepPort { step: "forge".into(), port: "script".into() },
    });
    def.connections.push(WorkflowConnection::Data {
        from: PortSource::Step { step: "forge".into(), port: "dataset".into() },
        to: StepPort { step: "associate".into(), port: "evidence".into() },
    });
    let v = validate(&def);
    assert!(has(&v, "workflow.serviceOwnedType", Some(6)), "{:?}", v.diagnostics);
    // Declaring a Python output as audit evidence.
    def.outcomes.push(WorkflowOutcome {
        id: "fake".into(),
        kind: OutcomeKind::AuditEvidence,
        label: "Fake audit".into(),
        from: StepPort { step: "forge".into(), port: "table".into() },
        optional: false,
    });
    let v = validate(&def);
    assert!(
        v.diagnostics.iter().any(|d| d.code == "workflow.serviceOwnedType" && d.outcome.as_deref() == Some("fake")),
        "{:?}",
        v.diagnostics
    );
    // An agent report cannot be audit evidence either.
    let mut def = audit();
    def.outcomes[0].from = StepPort { step: "associate".into(), port: "proposal".into() };
    assert!(has(&validate(&def), "workflow.serviceOwnedType", Some(6)));
    // An external input cannot claim to be formal evidence.
    let mut def = audit();
    def.inputs.push(WorkflowInput {
        id: "ev".into(),
        label: "Evidence file".into(),
        port_type: PortType::FormalEvidence,
        source: InputSource::Resource { path: path("evidence.json"), version: InputVersion::AtRunStart },
    });
    assert!(has(&validate(&def), "workflow.inputType", Some(6)));
}

#[test]
fn forged_output_declarations_are_schema_errors() {
    // A step cannot declare its own outputs (or anything else unknown).
    let mut json = serde_json::to_value(audit()).unwrap();
    json["steps"][0]["outputs"] = serde_json::json!([{ "port": "evidence", "type": "formalEvidence" }]);
    let err = parse_definition(&json.to_string()).unwrap_err();
    assert!(err.contains("unknown field `outputs`"), "{err}");
}

#[test]
fn loops_conditionals_nesting_and_hooks_are_not_expressible() {
    for (field, value) in [
        ("condition", serde_json::json!("x > 1")),
        ("loop", serde_json::json!({ "times": 3 })),
        ("workflow", serde_json::json!({ "schema": WORKFLOW_SCHEMA })),
        ("retries", serde_json::json!(5)),
    ] {
        let mut json = serde_json::to_value(audit()).unwrap();
        json["steps"][0][field] = value;
        assert!(parse_definition(&json.to_string()).is_err(), "{field} accepted");
    }
    for field in ["trust", "autoRun", "onOpen", "approvals"] {
        let mut json = serde_json::to_value(audit()).unwrap();
        json[field] = serde_json::json!(true);
        assert!(parse_definition(&json.to_string()).is_err(), "{field} accepted");
    }
    // Cycles through prerequisites.
    let mut def = audit();
    def.connections.push(WorkflowConnection::Prerequisite { before: "associate".into(), after: "audit".into() });
    let v = validate(&def);
    assert!(has(&v, "workflow.cycle", None));
    assert!(v.plan.is_empty());
}

#[test]
fn schema_version_operations_and_sizes() {
    let mut def = audit();
    def.schema = "exposition.workflow/2".into();
    assert!(has(&validate(&def), "workflow.schema", None));
    let mut def = audit();
    def.steps[0].operation.version = 9;
    assert!(has(&validate(&def), "workflow.unsupportedVersion", None));
    let mut def = audit();
    def.steps[0].operation.id = "shell.run".into();
    assert!(has(&validate(&def), "workflow.unknownOperation", None));
    let mut def = audit();
    for i in 0..(MAX_STEPS + 1) {
        def.steps.push(WorkflowStep {
            id: format!("s{i}"),
            label: "x".into(),
            operation: StepOperation { id: "lean.audit".into(), version: 1 },
            parameters: BTreeMap::new(),
            payload: None,
        });
    }
    assert!(has(&validate(&def), "workflow.tooLarge", None));
    // Oversized code payload.
    let mut def = numerical(false);
    def.connections.retain(|c| !matches!(c, WorkflowConnection::Data { to, .. } if to.port == "script"));
    def.inputs.retain(|i| i.id != "script");
    def.steps[0].payload =
        Some(StepPayload::Code { language: CodeLanguage::Python, source: "x".repeat(MAX_CODE_BYTES + 1) });
    assert!(has(&validate(&def), "workflow.tooLarge", None));
    assert!(parse_definition(&" ".repeat(MAX_DEFINITION_BYTES + 1)).is_err());
    // Unknown and mistyped parameters.
    let mut def = audit();
    def.steps[1].parameters.insert("shell".into(), ParamValue::Text("rm -rf".into()));
    assert!(has(&validate(&def), "workflow.unknownParameter", None));
    let mut def = numerical(true);
    def.steps[2].parameters.insert("document".into(), ParamValue::Text("../outside.typ".into()));
    assert!(has(&validate(&def), "workflow.invalidParameter", None));
}

#[test]
fn code_payload_or_script_input() {
    let mut def = numerical(false);
    def.connections.retain(|c| !matches!(c, WorkflowConnection::Data { to, .. } if to.port == "script"));
    def.inputs.retain(|i| i.id != "script");
    assert!(has(&validate(&def), "workflow.missingCode", Some(1)));
    def.steps[0].payload = Some(StepPayload::Code { language: CodeLanguage::Python, source: "print(1)".into() });
    assert_eq!(codes(&validate(&def)), vec![]);
}

fn service() -> (tempfile::TempDir, tempfile::TempDir, std::sync::Arc<GraphService>) {
    let data = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let graph = GraphService::new(GraphConfig { data_dir: data.path().to_path_buf() });
    let status = graph.attach(&std::fs::canonicalize(project.path()).unwrap());
    assert!(status.is_usable(), "{status:?}");
    (data, project, graph)
}

#[test]
fn saving_admits_a_workflow_entity_and_edits_create_revisions() {
    let (_d, _p, graph) = service();
    let ctx = ValidationContext::default();
    let mut def = draft(&WorkflowPresetRequest { preset: FORMAL_AUDIT.into(), ..Default::default() }).unwrap();
    let saved = graph
        .save_workflow(
            &Origin::User,
            &SaveWorkflowRequest { id: None, expected_revision: None, definition: def.clone() },
            &ctx,
        )
        .unwrap();
    assert_eq!(saved.revision, 1);
    assert_eq!(saved.validation.state, DraftState::Incomplete, "saved with diagnostics");
    assert!(saved.validation.errors > 0);
    assert!(!saved.validation.runnable);
    let entity = graph.entity_detail(saved.entity.as_ref().unwrap()).unwrap().entity;
    assert_eq!(entity.kind, crate::vocabulary::EntityKind::Workflow);
    assert_eq!(entity.key.as_deref(), Some(saved.id.as_str()));
    assert_eq!(entity.attributes["workflow.revision"], "1");
    assert_eq!(entity.attributes["workflow.runnable"], "false");

    // Same content: no new revision.
    let again = graph
        .save_workflow(
            &Origin::User,
            &SaveWorkflowRequest { id: Some(saved.id.clone()), expected_revision: Some(1), definition: def.clone() },
            &ctx,
        )
        .unwrap();
    assert!(again.unchanged);
    assert_eq!(again.revision, 1);

    // An edit creates revision 2; revision 1 stays as it was.
    def.inputs[0].source = InputSource::Resource { path: path("Main.lean"), version: InputVersion::AtRunStart };
    let edited = graph
        .save_workflow(
            &Origin::User,
            &SaveWorkflowRequest { id: Some(saved.id.clone()), expected_revision: Some(1), definition: def.clone() },
            &ctx,
        )
        .unwrap();
    assert_eq!(edited.revision, 2);
    assert_eq!(edited.validation.state, DraftState::WellFormed);
    assert!(!edited.validation.runnable);
    assert_ne!(edited.hash, saved.hash);
    assert_eq!(edited.entity, saved.entity, "one entity per workflow");
    let first = graph.workflow(&saved.id, Some(1)).unwrap();
    assert_eq!(first.hash, saved.hash);
    assert!(matches!(first.definition.inputs[0].source, InputSource::Unset));
    assert_eq!(edited.revisions.len(), 2);

    // A stale editor is refused.
    let stale = graph.save_workflow(
        &Origin::User,
        &SaveWorkflowRequest { id: Some(saved.id.clone()), expected_revision: Some(1), definition: numerical(false) },
        &ctx,
    );
    assert_eq!(stale.unwrap_err().code, "workflow.stale");
    let list = graph.workflows().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].revision, 2);
    assert!(!list[0].runnable);
}

#[test]
fn imported_drafts_get_fresh_ids_and_never_run() {
    let (_d, _p, graph) = service();
    let text = serde_json::to_string(&audit()).unwrap();
    let a = graph.import_workflow(&Origin::User, &text, &ValidationContext::default()).unwrap();
    let b = graph.import_workflow(&Origin::User, &text, &ValidationContext::default()).unwrap();
    assert_ne!(a.id, b.id);
    assert!(a.imported);
    assert_eq!(a.validation.state, DraftState::WellFormed);
    assert!(!a.validation.runnable, "an imported draft is never runnable");
    let entity = graph.entity_detail(a.entity.as_ref().unwrap()).unwrap().entity;
    assert_eq!(entity.attributes["workflow.imported"], "true");
    // Hostile imports.
    let mut hostile = serde_json::to_value(audit()).unwrap();
    hostile["trust"] = serde_json::json!({ "grant": true });
    let err = graph.import_workflow(&Origin::User, &hostile.to_string(), &ValidationContext::default()).unwrap_err();
    assert_eq!(err.code, "workflow.invalid");
    assert!(graph.import_workflow(&Origin::User, "not json", &ValidationContext::default()).is_err());
    assert_eq!(graph.workflows().unwrap().len(), 2, "nothing saved for refused imports");
}

#[test]
fn a_stored_runnable_claim_is_ignored() {
    let (_d, project, graph) = service();
    let saved = graph
        .import_workflow(&Origin::User, &serde_json::to_string(&audit()).unwrap(), &ValidationContext::default())
        .unwrap();
    graph.detach();
    // Tamper with the database: claim the draft is runnable.
    let db = project.path().join(".exposition").join("graph.sqlite3");
    let conn = rusqlite::Connection::open(&db).unwrap();
    let body: String = conn.query_row("SELECT body FROM workflows", [], |r| r.get(0)).unwrap();
    let tampered = body.replace("\"runnable\":false", "\"runnable\":true");
    assert_ne!(body, tampered);
    conn.execute("UPDATE workflows SET body = ?1", [tampered]).unwrap();
    drop(conn);
    graph.attach(&std::fs::canonicalize(project.path()).unwrap());
    let read = graph.workflow(&saved.id, None).unwrap();
    assert!(!read.validation.runnable);
}
