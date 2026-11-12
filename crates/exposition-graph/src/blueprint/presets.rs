//! Built-in project presets: complete starting structures. (Workflow presets
//! — reusable procedures within a project — are in `workflow::presets`.)
//!
//! Each preset produces an ordinary [`ProjectBlueprint`] that goes through the
//! same validation and materialization as agent proposals and imports. Every
//! call mints a fresh research-space id; entity and project ids are minted
//! when the blueprint is applied. Required toolchains are declared, never
//! installed or run.

use exposition_core::path::WorkspacePath;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{
    BLUEPRINT_SCHEMA, BlueprintBuildTarget, BlueprintCollection, BlueprintEngine, BlueprintEntity, BlueprintEntry,
    BlueprintFile, BlueprintRef, BlueprintRelation, BlueprintResearch, BlueprintWorkflow, CapabilityId,
    ProjectBlueprint, RequiredCapability,
};
use crate::vocabulary::{EntityKind, RelationKind};
use crate::workflow::presets::{FORMAL_AUDIT, NUMERICAL_EXPERIMENT, WorkflowPresetRequest, draft};

pub const PAPER: &str = "paper";
pub const COMPUTATIONAL: &str = "computational";
pub const LEAN: &str = "lean";
pub const RESEARCH: &str = "research";
pub const PAPER_COMPUTATION: &str = "paperComputation";
pub const PAPER_LEAN: &str = "paperLean";
/// An empty project for the objective path (an agent organizes it).
pub const EMPTY: &str = "empty";

/// Used when no Lean toolchain is installed (it is declared, not installed).
pub const DEFAULT_LEAN_TOOLCHAIN: &str = "leanprover/lean4:v4.35.0";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum ManuscriptFormat {
    Typst,
    /// LaTeX built with pdfLaTeX.
    LatexPdf,
    /// LaTeX built with LuaLaTeX.
    LatexLua,
    /// Markdown (print profile).
    Markdown,
}

impl ManuscriptFormat {
    pub fn capability(self) -> CapabilityId {
        match self {
            ManuscriptFormat::Typst => CapabilityId::Typst,
            ManuscriptFormat::LatexPdf => CapabilityId::PdfLatex,
            ManuscriptFormat::LatexLua => CapabilityId::LuaLatex,
            ManuscriptFormat::Markdown => CapabilityId::Markdown,
        }
    }

    fn file(self) -> &'static str {
        match self {
            ManuscriptFormat::Typst => "main.typ",
            ManuscriptFormat::LatexPdf | ManuscriptFormat::LatexLua => "main.tex",
            ManuscriptFormat::Markdown => "main.md",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum ResearchRuntime {
    Python,
    R,
    Julia,
}

impl ResearchRuntime {
    pub fn capability(self) -> CapabilityId {
        match self {
            ResearchRuntime::Python => CapabilityId::Python,
            ResearchRuntime::R => CapabilityId::R,
            ResearchRuntime::Julia => CapabilityId::Julia,
        }
    }

    fn title(self) -> &'static str {
        match self {
            ResearchRuntime::Python => "Python",
            ResearchRuntime::R => "R",
            ResearchRuntime::Julia => "Julia",
        }
    }
}

/// A preset as the New Project page lists it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ProjectPresetInfo {
    pub id: String,
    pub title: String,
    pub description: String,
    /// Asks for a manuscript format.
    pub manuscript: bool,
    /// Asks for a research runtime.
    pub computation: bool,
    /// Includes a Lean project.
    pub lean: bool,
    /// No manuscript: opens the Research page.
    pub research_only: bool,
    /// Whether an optional workflow draft can be included.
    pub workflow: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectPresetRequest {
    pub preset: String,
    /// Project title (defaults to the preset's).
    pub title: Option<String>,
    pub format: Option<ManuscriptFormat>,
    pub runtime: Option<ResearchRuntime>,
    /// `lean-toolchain` content (an installed toolchain, or the default).
    pub lean_toolchain: Option<String>,
    /// Include the preset's workflow draft.
    pub include_workflow: bool,
    /// What the project is for (objective path).
    pub purpose: Option<String>,
}

pub fn catalog() -> Vec<ProjectPresetInfo> {
    let p = |id: &str,
             title: &str,
             description: &str,
             manuscript,
             computation,
             lean,
             research_only,
             workflow: Option<&str>| {
        ProjectPresetInfo {
            id: id.into(),
            title: title.into(),
            description: description.into(),
            manuscript,
            computation,
            lean,
            research_only,
            workflow: workflow.map(str::to_owned),
        }
    };
    vec![
        p(
            PAPER,
            "Mathematical paper",
            "A manuscript in Typst, LaTeX (pdfLaTeX or LuaLaTeX), or Markdown with a definition, standing assumptions, and a main result.",
            true,
            false,
            false,
            false,
            None,
        ),
        p(
            COMPUTATIONAL,
            "Computational study",
            "A research space with a script for an installed Python, R, or Julia runtime, a data folder, a research question, and a baseline experiment. No manuscript.",
            false,
            true,
            false,
            true,
            Some(NUMERICAL_EXPERIMENT),
        ),
        p(
            LEAN,
            "Lean investigation",
            "A Lean project (lean-toolchain, a declarative lakefile.toml, and a source file) with the conjecture under investigation.",
            false,
            false,
            true,
            false,
            Some(FORMAL_AUDIT),
        ),
        p(
            RESEARCH,
            "Blank research project",
            "Only a research space with notes and a research question — no manuscript. Organize it as the work develops.",
            false,
            false,
            false,
            true,
            None,
        ),
        p(
            PAPER_COMPUTATION,
            "Paper with computation",
            "A manuscript plus a research space with a script and a baseline experiment.",
            true,
            true,
            false,
            false,
            Some(NUMERICAL_EXPERIMENT),
        ),
        p(
            PAPER_LEAN,
            "Paper with Lean",
            "A manuscript plus a Lean project for formalizing its results.",
            true,
            false,
            true,
            false,
            Some(FORMAL_AUDIT),
        ),
    ]
}

/// A fresh research-space id (`rs-` + 12 hex).
pub fn fresh_research_id() -> String {
    format!("rs-{}", &uuid::Uuid::new_v4().simple().to_string()[..12])
}

fn lean_toolchain_ok(t: &str) -> bool {
    !t.is_empty()
        && t.len() <= 100
        && t.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | ':' | '.' | '-' | '_' | '+'))
}

#[derive(Default)]
struct Builder {
    bp: Option<ProjectBlueprint>,
}

impl Builder {
    fn bp(&mut self) -> &mut ProjectBlueprint {
        self.bp.as_mut().expect("started")
    }

    fn file(&mut self, path: &str, content: String) {
        self.bp().files.push(BlueprintFile { path: path.into(), content });
    }

    fn entity(&mut self, temp: &str, kind: EntityKind, label: &str, source: Option<&str>, attributes: &[(&str, &str)]) {
        self.bp().entities.push(BlueprintEntity {
            temp_id: temp.into(),
            kind,
            label: label.into(),
            attributes: attributes.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect(),
            source: source.map(str::to_owned),
        });
    }

    fn relation(&mut self, kind: RelationKind, from: BlueprintRef, to: BlueprintRef) {
        self.bp().relations.push(BlueprintRelation { kind, from, to, note: None });
    }

    fn capability(&mut self, capability: CapabilityId, purpose: &str) {
        if !self.bp().capabilities.iter().any(|c| c.capability == capability) {
            self.bp().capabilities.push(RequiredCapability { capability, purpose: purpose.into() });
        }
    }
}

fn temp(t: &str) -> BlueprintRef {
    BlueprintRef::Temp { temp_id: t.into() }
}

fn file(p: &str) -> BlueprintRef {
    BlueprintRef::File { path: p.into() }
}

/// The blueprint of a preset with the chosen options.
pub fn blueprint(request: &ProjectPresetRequest) -> Result<ProjectBlueprint, String> {
    let info = catalog().into_iter().find(|p| p.id == request.preset);
    let (manuscript, computation, lean, research_only) = match (&info, request.preset.as_str()) {
        (Some(i), _) => (i.manuscript, i.computation, i.lean, i.research_only),
        // The objective path's empty project: no files, but a research space
        // so a generation agent has a managed research folder to fill.
        (None, EMPTY) => (false, false, false, true),
        (None, other) => return Err(format!("unknown project preset {:?}", crate::store::truncate(other, 40))),
    };
    let format = if manuscript { Some(request.format.ok_or("choose a manuscript format")?) } else { None };
    let runtime = if computation { Some(request.runtime.ok_or("choose a research runtime")?) } else { None };
    let title = request
        .title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
        .or_else(|| info.as_ref().map(|i| i.title.clone()))
        .unwrap_or_else(|| "New project".to_owned());
    let purpose = request
        .purpose
        .clone()
        .or_else(|| info.as_ref().map(|i| i.description.clone()))
        .unwrap_or_else(|| "A project an agent organizes for the stated objective.".to_owned());
    let mut b = Builder {
        bp: Some(ProjectBlueprint {
            schema: BLUEPRINT_SCHEMA.into(),
            source: format!("preset:{}", request.preset.to_ascii_lowercase()),
            title: title.clone(),
            purpose,
            folders: Vec::new(),
            files: Vec::new(),
            research: None,
            entities: Vec::new(),
            relations: Vec::new(),
            collections: Vec::new(),
            workflows: Vec::new(),
            capabilities: Vec::new(),
            build_targets: Vec::new(),
            entry: BlueprintEntry::Files,
        }),
    };

    let mut manuscript_path: Option<&str> = None;
    if let Some(format) = format {
        let path = format.file();
        manuscript_path = Some(path);
        b.file(path, manuscript_template(format, &title));
        b.bp().folders.push("figures".into());
        b.capability(format.capability(), "Builds the manuscript.");
        let (engine, name) = match format {
            ManuscriptFormat::Typst => (BlueprintEngine::Typst, "Paper (Typst)"),
            ManuscriptFormat::LatexPdf => (BlueprintEngine::PdfLatex, "Paper (pdfLaTeX)"),
            ManuscriptFormat::LatexLua => (BlueprintEngine::LuaLatex, "Paper (LuaLaTeX)"),
            ManuscriptFormat::Markdown => (BlueprintEngine::MarkdownPrint, "Paper (Markdown)"),
        };
        b.bp().build_targets.push(BlueprintBuildTarget { name: name.into(), entry: path.into(), engine });
        b.entity("definition", EntityKind::Definition, "Key definition", Some(path), &[]);
        b.entity("assumptions", EntityKind::Assumption, "Standing assumptions", Some(path), &[]);
        b.entity("mainResult", EntityKind::Claim, "Main result", Some(path), &[]);
        b.relation(RelationKind::DependsOn, temp("mainResult"), temp("definition"));
        b.relation(RelationKind::Assumes, temp("mainResult"), temp("assumptions"));
        b.bp().collections.push(BlueprintCollection {
            temp_id: "manuscript".into(),
            label: "Manuscript".into(),
            members: vec![file(path), temp("definition"), temp("assumptions"), temp("mainResult")],
        });
        b.bp().entry = BlueprintEntry::Editor { path: path.into() };
    }

    let research =
        (computation || research_only).then(|| BlueprintResearch { id: fresh_research_id(), label: "Research".into() });
    let mut script_path: Option<String> = None;
    if let Some(r) = &research {
        let folder = r.folder();
        let notes = format!("{folder}/notes/notes.md");
        b.bp().research = Some(r.clone());
        b.file(&notes, research_notes(&title));
        b.entity("question", EntityKind::Question, "Research question", Some(&notes), &[]);
        b.relation(RelationKind::MemberOf, temp("question"), BlueprintRef::ResearchSpace);
        if let Some(runtime) = runtime {
            let (name, content) = match runtime {
                ResearchRuntime::Python => ("analysis.py", PYTHON_SCRIPT.to_owned()),
                ResearchRuntime::R => ("analysis.R", R_SCRIPT.to_owned()),
                ResearchRuntime::Julia => ("analysis.jl", JULIA_SCRIPT.to_owned()),
            };
            let script = format!("{folder}/sources/{name}");
            b.bp().folders.push(format!("{folder}/data"));
            b.file(&script, content);
            b.capability(runtime.capability(), "Runs the study's scripts (declared; nothing is run or installed).");
            b.entity(
                "baseline",
                EntityKind::Experiment,
                "Baseline experiment",
                None,
                &[("procedure", &script), ("runtime", runtime.title())],
            );
            b.relation(RelationKind::References, temp("baseline"), file(&script));
            b.relation(RelationKind::References, temp("baseline"), temp("question"));
            b.relation(RelationKind::MemberOf, temp("baseline"), BlueprintRef::ResearchSpace);
            b.relation(RelationKind::MemberOf, file(&script), BlueprintRef::ResearchSpace);
            script_path = Some(script);
        }
        b.relation(RelationKind::MemberOf, file(&notes), BlueprintRef::ResearchSpace);
        if manuscript_path.is_none() {
            b.bp().entry = BlueprintEntry::Research;
        }
    }

    let mut lean_path: Option<&str> = None;
    if lean {
        let toolchain = request.lean_toolchain.clone().unwrap_or_else(|| DEFAULT_LEAN_TOOLCHAIN.to_owned());
        if !lean_toolchain_ok(&toolchain) {
            return Err("invalid Lean toolchain name".into());
        }
        b.file("lean-toolchain", format!("{toolchain}\n"));
        b.file("lakefile.toml", LAKEFILE.to_owned());
        b.file("Investigation.lean", LEAN_SOURCE.to_owned());
        b.capability(
            CapabilityId::Lean,
            "Checks the formal development (declared; Exposition never installs toolchains).",
        );
        b.entity("conjecture", EntityKind::Conjecture, "Main conjecture", Some("Investigation.lean"), &[]);
        let mut members = vec![file("Investigation.lean"), file("lakefile.toml"), temp("conjecture")];
        if manuscript_path.is_some() {
            members.push(temp("mainResult"));
        }
        b.bp().collections.push(BlueprintCollection {
            temp_id: "formal".into(),
            label: if manuscript_path.is_some() { "Formalization".into() } else { "Formal development".into() },
            members,
        });
        lean_path = Some("Investigation.lean");
        if manuscript_path.is_none() {
            b.bp().entry = BlueprintEntry::Editor { path: "Investigation.lean".into() };
        }
    }

    if request.include_workflow {
        let draft_request = match info.as_ref().and_then(|i| i.workflow.as_deref()) {
            Some(NUMERICAL_EXPERIMENT) if runtime == Some(ResearchRuntime::Python) => Some(WorkflowPresetRequest {
                preset: NUMERICAL_EXPERIMENT.into(),
                script: script_path.as_deref().and_then(|s| WorkspacePath::parse(s).ok()),
                manuscript: manuscript_path.and_then(|m| WorkspacePath::parse(m).ok()),
                ..Default::default()
            }),
            Some(FORMAL_AUDIT) => Some(WorkflowPresetRequest {
                preset: FORMAL_AUDIT.into(),
                lean_file: lean_path.and_then(|l| WorkspacePath::parse(l).ok()),
                ..Default::default()
            }),
            // The initial workflow adapters are Python and Lean only.
            _ => None,
        };
        if let Some(definition) = draft_request.as_ref().and_then(draft) {
            b.bp().workflows.push(BlueprintWorkflow { temp_id: "workflow".into(), definition });
            if research.is_some() {
                b.relation(RelationKind::MemberOf, temp("workflow"), BlueprintRef::ResearchSpace);
            } else if lean {
                let formal = b.bp().collections.iter_mut().find(|c| c.temp_id == "formal");
                if let Some(c) = formal {
                    c.members.push(temp("workflow"));
                }
            }
        }
    }
    Ok(b.bp.take().expect("built"))
}

fn manuscript_template(format: ManuscriptFormat, title: &str) -> String {
    match format {
        ManuscriptFormat::Typst => format!(
            "#set document(title: \"{ts}\")\n#set page(paper: \"a4\", margin: 2.5cm)\n#set heading(numbering: \"1.\")\n\n= {t}\n\n== Definitions\nKey definition: state the central definition here.\n\n== Assumptions\nStanding assumptions: list what every result below assumes.\n\n== Main result\nMain result: state the claim this paper establishes.\n\n== Proof\nProve the main result from the definition and the assumptions.\n",
            t = typst_escape(title),
            ts = typst_string_escape(title)
        ),
        ManuscriptFormat::LatexPdf | ManuscriptFormat::LatexLua => {
            let fonts = if format == ManuscriptFormat::LatexPdf { "\\usepackage[T1]{fontenc}\n" } else { "" };
            format!(
                "\\documentclass[11pt]{{article}}\n{fonts}\\usepackage{{amsmath,amssymb,amsthm}}\n\n\\newtheorem{{theorem}}{{Theorem}}\n\\theoremstyle{{definition}}\n\\newtheorem{{definition}}{{Definition}}\n\n\\title{{{t}}}\n\\author{{}}\n\\date{{}}\n\n\\begin{{document}}\n\\maketitle\n\n\\section{{Definitions}}\n\\begin{{definition}}\nKey definition: state the central definition here.\n\\end{{definition}}\n\n\\section{{Assumptions}}\nStanding assumptions: list what every result below assumes.\n\n\\section{{Main result}}\n\\begin{{theorem}}\nMain result: state the claim this paper establishes.\n\\end{{theorem}}\n\n\\begin{{proof}}\nProve the main result from the definition and the assumptions.\n\\end{{proof}}\n\n\\end{{document}}\n",
                t = latex_escape(title)
            )
        }
        ManuscriptFormat::Markdown => format!(
            "# {t}\n\n## Definitions\n\nKey definition: state the central definition here.\n\n## Assumptions\n\nStanding assumptions: list what every result below assumes.\n\n## Main result\n\nMain result: state the claim this paper establishes.\n\n## Proof\n\nProve the main result from the definition and the assumptions.\n",
            t = markdown_escape(title)
        ),
    }
}

fn research_notes(title: &str) -> String {
    format!(
        "# {}\n\n## Research question\n\nWhat is this project trying to find out?\n\n## Log\n\nRecord findings worth keeping; retain results from the Research page.\n",
        markdown_escape(title)
    )
}

fn typst_escape(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if matches!(c, '\\' | '#' | '*' | '_' | '$' | '@' | '<' | '>' | '[' | ']' | '`' | '~') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn typst_string_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn latex_escape(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\textbackslash{}"),
            '{' | '}' | '$' | '&' | '#' | '_' | '%' => {
                out.push('\\');
                out.push(c);
            }
            '^' => out.push_str("\\^{}"),
            '~' => out.push_str("\\~{}"),
            c => out.push(c),
        }
    }
    out
}

fn markdown_escape(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if matches!(c, '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '>' | '#' | '|' | '$') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

const PYTHON_SCRIPT: &str = "\"\"\"Baseline experiment for this study.\n\nRun it from the Research page; keep the outputs worth keeping as retained\nresults. Exposition does not run it on its own.\n\"\"\"\nimport math\n\nvalues = [math.sin(k / 10) for k in range(100)]\nprint(f\"mean = {sum(values) / len(values):.6f}\")\n";

const R_SCRIPT: &str = "# Baseline experiment for this study.\n# Run it from the Research page; Exposition does not run it on its own.\nvalues <- sin((0:99) / 10)\ncat(sprintf(\"mean = %.6f\\n\", mean(values)))\n";

const JULIA_SCRIPT: &str = "# Baseline experiment for this study.\n# Run it from the Research page; Exposition does not run it on its own.\nvalues = [sin(k / 10) for k in 0:99]\nprintln(\"mean = \", sum(values) / length(values))\n";

const LAKEFILE: &str =
    "name = \"investigation\"\ndefaultTargets = [\"Investigation\"]\n\n[[lean_lib]]\nname = \"Investigation\"\n";

const LEAN_SOURCE: &str = "/-!\n# Investigation\n\nState the main conjecture and develop its proof here. Check the file from the\nLean panel once the folder is trusted.\n-/\n\n/-- A warm-up lemma the conjecture's proof can build on. -/\ntheorem warmup (n : Nat) : n + 0 = n := rfl\n";
