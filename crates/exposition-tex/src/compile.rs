//! pdfLaTeX / LuaLaTeX build adapter.
//!
//! # Snapshot strategy (overlay + TEXINPUTS)
//!
//! A build compiles one immutable snapshot of the project including unsaved
//! buffers, without writing into the project:
//!
//! * Every open document with a workspace path is materialized, byte for
//!   byte, into an application-owned overlay `<work>/src/<path>` that is
//!   recreated for each build (so it holds exactly the snapshot's buffers).
//! * The engine runs with its working directory in the overlay (at the entry
//!   file's directory) and `TEXINPUTS` = overlay dir, project dir, then the
//!   distribution default. Relative `\input`, `\include`, and
//!   `\includegraphics` paths therefore resolve to the buffer version when
//!   one exists and to the project file otherwise.
//! * All engine output goes to `<work>/out` (`-output-directory`), never to
//!   the project. `.aux`/`.bbl` persist between builds so a single draft pass
//!   resolves references from the previous full build.
//! * After each build the PDF, SyncTeX, and log are copied into
//!   `<work>/builds/<build-id>/`, which is never modified afterwards: page
//!   views and mappings refer to that directory.
//! * The `-recorder` file lists every input actually read; project files are
//!   hashed (and `.tex` texts retained) right after the run, which yields the
//!   snapshot's `SourceRevision`s and content-derived `SnapshotId`.
//!   Limitation: a project file that is not open in a buffer is read from
//!   disk by the engine; if it changes *during* the run the recorded hash may
//!   not match what the engine read. Copying the whole project would remove
//!   that window at the cost of per-build copying.
//!
//! # Security
//!
//! `-no-shell-escape` explicitly (also `shell_escape=f`); `openout_any=p`
//! (verified: absolute and parent-directory writes are refused by both
//! engines, including LuaLaTeX's `io.open`; `\write18`, `os.execute`, and
//! `io.popen` are disabled); a constructed environment (no inherited
//! secrets); program + argument arrays only, run through [`JobRunner`].
//!
//! Reads are **not** confined: TeX Live 2026 reads absolute and `../` paths
//! through `\input` even with `openin_any=p` (set anyway as defense in depth
//! for distributions that honour it). A project can therefore typeset any
//! file the user can read into its PDF, so building an unfamiliar project
//! requires workspace trust, or the restricted execution backend once the
//! execution service provides one. `--safer` would restrict LuaLaTeX further
//! but breaks luaotfload, so it is not used.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use exposition_core::build::{
    BuildConfiguration, BuildId, BuildResult, BuildStatus, BuildTarget, Diagnostic, DocumentFormat, Engine,
    ExportArtifact, ExportKind, MappingPrecision, PageArtifact, Severity, SnapshotId, SourceLocation, SourceRevision,
};
use exposition_core::document::DocumentId;
use exposition_core::job::{EnvironmentSpec, ExecutionPolicy, JobOutcome, JobSpec};
use exposition_core::path::WorkspacePath;
use exposition_core::text::{LineColumn, LineIndex, TextRange};
use exposition_workspace::DocumentSnapshot;
use sha2::{Digest, Sha256};

use crate::log::{LogMessageKind, parse_log, rerun_requested};
use crate::mapping::SourceSet;
use crate::pdf::PdfDocument;
use crate::runner::{JobRunner, ProcessOutput};
use crate::synctex::SyncTex;
use crate::toolchain::{TexTool, TexToolchain, guidance};

/// How much work a build does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuildMode {
    /// Engine passes, bibliography tool, and reruns until cross-references
    /// settle (at most [`MAX_ENGINE_PASSES`]).
    Full,
    /// One engine pass using the previous `.aux`/`.bbl` (typing latency).
    Draft,
}

pub const MAX_ENGINE_PASSES: usize = 5;

#[derive(Clone, Debug)]
pub struct TexBuildRequest {
    pub target: BuildTarget,
    /// Canonical workspace root.
    pub project_root: PathBuf,
    /// Open documents (unsaved content included). Documents without a
    /// workspace path are ignored.
    pub buffers: Vec<DocumentSnapshot>,
    pub mode: BuildMode,
    /// pdfLaTeX only: run with `\pdfgentounicode=1 \input glyphtounicode` so
    /// the PDF carries ToUnicode maps for Type 1 fonts.
    pub tounicode: bool,
}

/// One file of a build snapshot.
#[derive(Clone, Debug)]
pub struct SnapshotFile {
    pub content_hash: String,
    /// Text for TeX sources (UTF-8); `None` for binary inputs (images, fonts).
    pub text: Option<Arc<str>>,
    pub document: Option<DocumentId>,
    pub revision: Option<u64>,
}

/// The immutable inputs of one build.
#[derive(Clone, Debug)]
pub struct BuildSnapshot {
    pub id: SnapshotId,
    pub files: BTreeMap<WorkspacePath, SnapshotFile>,
    overlay_root: PathBuf,
    project_root: PathBuf,
    entry_dir: Option<WorkspacePath>,
}

impl BuildSnapshot {
    /// Map a file name as written by the engine (SyncTeX input, log, recorder)
    /// to a workspace path, if it lies in the overlay or the project.
    pub fn resolve(&self, name: &str) -> Option<WorkspacePath> {
        resolve_name(name, &self.overlay_root, &self.project_root, self.entry_dir.as_ref())
    }

    /// Text of a snapshot file.
    pub fn text(&self, path: &WorkspacePath) -> Option<&Arc<str>> {
        self.files.get(path).and_then(|f| f.text.as_ref())
    }

    /// The snapshot's TeX sources bound to a SyncTeX file, for mapping.
    pub fn source_set(&self, synctex: &SyncTex) -> SourceSet {
        let files = self.files.iter().filter_map(|(path, file)| {
            let tex = matches!(path.extension(), Some("tex" | "ltx"));
            match (&file.text, tex) {
                (Some(text), true) => Some((path.clone(), text.clone())),
                _ => None,
            }
        });
        SourceSet::new(files, synctex, |name| self.resolve(name))
    }
}

/// Outputs of a build (immutable per-build directory).
#[derive(Clone, Debug)]
pub struct TexOutput {
    pub dir: PathBuf,
    pub pdf: PathBuf,
    pub synctex: Option<PathBuf>,
    pub log: PathBuf,
}

#[derive(Clone, Debug)]
pub struct PassRecord {
    pub tool: String,
    pub duration_ms: u64,
    pub exit_code: Option<i32>,
}

#[derive(Clone, Debug)]
pub struct TexBuild {
    pub result: BuildResult,
    pub output: Option<TexOutput>,
    pub snapshot: Option<BuildSnapshot>,
    pub passes: Vec<PassRecord>,
}

/// Builds LaTeX targets with pdfLaTeX or LuaLaTeX.
pub struct TexBuilder<R: JobRunner> {
    runner: R,
    toolchain: Option<TexToolchain>,
    build_root: PathBuf,
    versions: Mutex<BTreeMap<TexTool, String>>,
    lock: Mutex<()>,
    counter: std::sync::atomic::AtomicU64,
}

const ENGINE_TIMEOUT_MS: u64 = 180_000;
const KEEP_BUILDS: usize = 4;

impl<R: JobRunner> TexBuilder<R> {
    /// `build_root` must be an application-owned directory (never inside a
    /// project); it is created if missing.
    pub fn new(runner: R, toolchain: Option<TexToolchain>, build_root: PathBuf) -> std::io::Result<Self> {
        std::fs::create_dir_all(&build_root)?;
        let build_root = plain_path(&build_root.canonicalize()?);
        Ok(Self {
            runner,
            toolchain,
            build_root,
            versions: Mutex::new(BTreeMap::new()),
            lock: Mutex::new(()),
            counter: std::sync::atomic::AtomicU64::new(0),
        })
    }

    pub fn build_root(&self) -> &Path {
        &self.build_root
    }

    pub fn toolchain(&self) -> Option<&TexToolchain> {
        self.toolchain.as_ref()
    }

    pub fn build(&self, request: &TexBuildRequest) -> TexBuild {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let started = Instant::now();
        let n = self.counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let id = BuildId(format!("tex-{}-{n}", std::process::id()));
        let mut build = self.build_inner(request, &id);
        build.result.duration_ms = started.elapsed().as_millis() as u64;
        build
    }

    fn early(&self, request: &TexBuildRequest, id: &BuildId, status: BuildStatus) -> TexBuild {
        TexBuild {
            result: BuildResult {
                id: id.clone(),
                target: request.target.clone(),
                snapshot: SnapshotId(String::new()),
                sources: Vec::new(),
                configuration: None,
                status,
                diagnostics: Vec::new(),
                pages: Vec::new(),
                mapping: MappingPrecision::None,
                exports: Vec::new(),
                duration_ms: 0,
            },
            output: None,
            snapshot: None,
            passes: Vec::new(),
        }
    }

    fn build_inner(&self, request: &TexBuildRequest, id: &BuildId) -> TexBuild {
        let target = &request.target;
        let Some(engine_tool) = TexTool::for_engine(target.engine).filter(|_| target.format == DocumentFormat::Latex)
        else {
            return self.early(
                request,
                id,
                BuildStatus::Error { message: format!("{:?} is not a TeX engine target", target.engine) },
            );
        };
        let Some(toolchain) = self.toolchain.as_ref() else {
            return self.early(
                request,
                id,
                BuildStatus::ToolUnavailable { tool: engine_tool.name().into(), guidance: guidance(engine_tool) },
            );
        };
        let Some(engine_path) = toolchain.tool(engine_tool) else {
            return self.early(
                request,
                id,
                BuildStatus::ToolUnavailable { tool: engine_tool.name().into(), guidance: guidance(engine_tool) },
            );
        };
        let project_root = match request.project_root.canonicalize() {
            Ok(p) => plain_path(&p),
            Err(e) => {
                return self.early(request, id, BuildStatus::Error { message: format!("project root: {e}") });
            }
        };

        // Work directory for this (project, entry, engine).
        let key = short_hash(&format!("{}|{}|{:?}", project_root.display(), target.entry, target.engine));
        let work = self.build_root.join(&key);
        let overlay_root = work.join("src");
        let out_dir = work.join("out");
        if let Err(e) = prepare_dirs(&overlay_root, &out_dir, &project_root) {
            return self.early(request, id, BuildStatus::Error { message: format!("build directory: {e}") });
        }

        // Materialize buffers.
        let mut buffers: BTreeMap<WorkspacePath, &DocumentSnapshot> = BTreeMap::new();
        for doc in &request.buffers {
            if let Some(path) = &doc.path {
                buffers.insert(path.clone(), doc);
            }
        }
        for (path, doc) in &buffers {
            let file = path.components().fold(overlay_root.clone(), |p, c| p.join(c));
            if let Some(parent) = file.parent()
                && let Err(e) = std::fs::create_dir_all(parent)
            {
                return self.early(request, id, BuildStatus::Error { message: format!("overlay: {e}") });
            }
            if let Err(e) = std::fs::write(&file, doc.text.as_bytes()) {
                return self.early(request, id, BuildStatus::Error { message: format!("overlay: {e}") });
            }
        }
        let entry_dir = target.entry.parent();
        let in_dir =
            |root: &Path| entry_dir.iter().flat_map(|d| d.components()).fold(root.to_path_buf(), |p, c| p.join(c));
        let overlay_entry_dir = in_dir(&overlay_root);
        let project_entry_dir = in_dir(&project_root);
        let _ = std::fs::create_dir_all(&overlay_entry_dir);
        let entry_on_disk = target.entry.components().fold(project_root.clone(), |p, c| p.join(c));
        if !buffers.contains_key(&target.entry) && !entry_on_disk.is_file() {
            return self.early(
                request,
                id,
                BuildStatus::Error { message: format!("entry file {} does not exist", target.entry) },
            );
        }

        let entry_name = target.entry.file_name().to_owned();
        // The entry becomes the engine's first command-line argument, which
        // TeX interprets (`&format`, `\code`), and TeX cannot read names with
        // spaces or specials: accept plain names only.
        let plain_name = !entry_name.starts_with(['-', '&', '.'])
            && entry_name.chars().all(|c| c.is_alphanumeric() || matches!(c, '.' | '_' | '-' | '+'));
        if !plain_name {
            return self.early(
                request,
                id,
                BuildStatus::Error {
                    message: format!(
                        "entry file name {entry_name:?} is not supported by TeX; use letters, digits, '.', '_', '-'"
                    ),
                },
            );
        }
        let job_name = entry_name.rsplit_once('.').map(|(stem, _)| stem.to_owned()).unwrap_or(entry_name.clone());
        let env = self.environment(toolchain, &overlay_entry_dir, &project_entry_dir);
        let out_arg = slash(&out_dir);
        let mut engine_args = vec![
            "-synctex=1".to_owned(),
            "-interaction=nonstopmode".to_owned(),
            "-file-line-error".to_owned(),
            "-no-shell-escape".to_owned(),
            "-recorder".to_owned(),
            format!("-output-directory={out_arg}"),
            format!("-jobname={job_name}"),
        ];
        if request.tounicode && target.engine == Engine::PdfLatex {
            engine_args.push(format!("\\pdfgentounicode=1 \\input glyphtounicode \\input{{{entry_name}}}"));
        } else {
            engine_args.push(entry_name.clone());
        }
        let engine_spec = JobSpec {
            label: format!("{} {}", engine_tool.name(), target.entry),
            program: engine_path.display().to_string(),
            args: engine_args,
            environment: env.clone(),
            working_directory: overlay_entry_dir.display().to_string(),
            policy: ExecutionPolicy::TrustedNative,
            timeout_ms: Some(ENGINE_TIMEOUT_MS),
            log_limit_bytes: Some(1 << 20),
            stdin: None,
        };

        let mut passes = Vec::new();
        let mut diagnostics_extra: Vec<Diagnostic> = Vec::new();
        let log_path = out_dir.join(format!("{job_name}.log"));
        let aux_path = out_dir.join(format!("{job_name}.aux"));
        let pdf_path = out_dir.join(format!("{job_name}.pdf"));
        let _ = std::fs::remove_file(&pdf_path);

        let run_engine = |passes: &mut Vec<PassRecord>| -> Result<ProcessOutput, BuildStatus> {
            let output = self.runner.run(&engine_spec);
            passes.push(PassRecord {
                tool: engine_tool.name().into(),
                duration_ms: output.duration_ms,
                exit_code: output.exit_code(),
            });
            match &output.outcome {
                JobOutcome::Exited { .. } => Ok(output),
                JobOutcome::Canceled => Err(BuildStatus::Canceled),
                JobOutcome::TimedOut => Err(BuildStatus::Error { message: "the TeX engine timed out".into() }),
                JobOutcome::FailedToStart { reason } => Err(BuildStatus::Error { message: reason.clone() }),
                JobOutcome::Terminated { description } => Err(BuildStatus::Error { message: description.clone() }),
            }
        };

        let mut last = match run_engine(&mut passes) {
            Ok(o) => o,
            Err(status) => return self.early(request, id, status),
        };
        if request.mode == BuildMode::Full {
            let mut aux_hash = file_hash(&aux_path);
            for _ in 1..MAX_ENGINE_PASSES {
                let mut rerun = false;
                match self.bibliography(toolchain, &out_dir, &job_name, &overlay_entry_dir, &project_entry_dir, &env) {
                    BibOutcome::Ran(record) => {
                        passes.push(record);
                        rerun = true;
                    }
                    BibOutcome::Missing(tool) => diagnostics_extra.push(Diagnostic {
                        severity: Severity::Warning,
                        message: format!("The bibliography needs {} which is not available.", tool.name()),
                        location: None,
                        hints: vec![guidance(tool)],
                    }),
                    BibOutcome::Failed(record, message) => {
                        passes.push(record);
                        diagnostics_extra.push(Diagnostic {
                            severity: Severity::Error,
                            message,
                            location: None,
                            hints: Vec::new(),
                        });
                    }
                    BibOutcome::NotNeeded => {}
                }
                let log = read_lossy(&log_path);
                let new_aux = file_hash(&aux_path);
                if rerun_requested(&log) || new_aux != aux_hash {
                    rerun = true;
                }
                aux_hash = new_aux;
                if !rerun {
                    break;
                }
                last = match run_engine(&mut passes) {
                    Ok(o) => o,
                    Err(status) => return self.early(request, id, status),
                };
                if file_hash(&aux_path) == aux_hash && !rerun_requested(&read_lossy(&log_path)) {
                    break;
                }
                aux_hash = file_hash(&aux_path);
            }
        }

        // Snapshot: recorder inputs inside the overlay or project.
        let snapshot = self.snapshot(
            target,
            &out_dir.join(format!("{job_name}.fls")),
            &buffers,
            &overlay_root,
            &project_root,
            entry_dir.clone(),
        );
        let log = read_lossy(&log_path);
        let mut diagnostics = diagnostics_from_log(&log, &snapshot);
        diagnostics.extend(diagnostics_extra);
        let has_errors = diagnostics.iter().any(|d| d.severity == Severity::Error);

        // Freeze outputs.
        let frozen = work.join("builds").join(&id.0);
        let output = if pdf_path.is_file() {
            let _ = std::fs::create_dir_all(&frozen);
            let pdf = frozen.join(format!("{job_name}.pdf"));
            let synctex_src = out_dir.join(format!("{job_name}.synctex.gz"));
            let synctex = frozen.join(format!("{job_name}.synctex.gz"));
            let log_copy = frozen.join(format!("{job_name}.log"));
            let ok = std::fs::copy(&pdf_path, &pdf).is_ok();
            let synctex = (synctex_src.is_file() && std::fs::copy(&synctex_src, &synctex).is_ok()).then_some(synctex);
            let _ = std::fs::copy(&log_path, &log_copy);
            ok.then_some(TexOutput { dir: frozen.clone(), pdf, synctex, log: log_copy })
        } else {
            None
        };
        prune_builds(&work.join("builds"), KEEP_BUILDS);

        let mut pages = Vec::new();
        let mut exports = Vec::new();
        if let Some(out) = &output
            && let Ok(bytes) = std::fs::read(&out.pdf)
        {
            exports.push(ExportArtifact {
                kind: ExportKind::Pdf,
                path: out.pdf.display().to_string(),
                content_hash: hex_sha256(&bytes),
            });
            if let Ok(doc) = PdfDocument::open(bytes) {
                for i in 0..doc.page_count() {
                    let (w, h) = doc.page_size(i).unwrap_or((0.0, 0.0));
                    pages.push(PageArtifact {
                        index: i as u32,
                        width_pt: w,
                        height_pt: h,
                        render_ref: format!("tex-build:{}/page/{i}", id.0),
                    });
                }
            }
        }
        let exit_ok = last.exit_code() == Some(0);
        let status = if output.is_none() && !has_errors && exit_ok {
            BuildStatus::Error { message: "the engine produced no PDF".into() }
        } else if has_errors || !exit_ok || output.is_none() {
            BuildStatus::Failed
        } else {
            BuildStatus::Succeeded
        };
        let mapping = match &output {
            Some(o) if o.synctex.is_some() => MappingPrecision::Glyph,
            Some(_) => MappingPrecision::None,
            None => MappingPrecision::None,
        };
        let configuration = Some(BuildConfiguration {
            engine: target.engine,
            compiler: self.version(engine_tool, engine_path, &env, &overlay_entry_dir),
            font_fingerprint: font_fingerprint(&out_dir.join(format!("{job_name}.fls"))),
            profile: (request.tounicode && target.engine == Engine::PdfLatex).then(|| "glyphtounicode".to_owned()),
        });
        let sources = snapshot
            .files
            .iter()
            .map(|(path, f)| SourceRevision {
                path: path.clone(),
                document: f.document.clone(),
                revision: f.revision,
                content_hash: f.content_hash.clone(),
            })
            .collect();
        TexBuild {
            result: BuildResult {
                id: id.clone(),
                target: target.clone(),
                snapshot: snapshot.id.clone(),
                sources,
                configuration,
                status,
                diagnostics,
                pages,
                mapping,
                exports,
                duration_ms: 0,
            },
            output,
            snapshot: Some(snapshot),
            passes,
        }
    }

    fn environment(&self, toolchain: &TexToolchain, overlay_dir: &Path, project_dir: &Path) -> EnvironmentSpec {
        let sep = if cfg!(windows) { ";" } else { ":" };
        let search = format!("{}{sep}{}{sep}", slash(overlay_dir), slash(project_dir));
        let mut set = BTreeMap::new();
        let mut path = toolchain.bin_dir.display().to_string();
        if cfg!(windows)
            && let Some(root) = std::env::var_os("SystemRoot")
        {
            path.push(';');
            path.push_str(&Path::new(&root).join("System32").display().to_string());
        } else if !cfg!(windows) {
            path.push_str(":/usr/bin:/bin");
        }
        set.insert("PATH".to_owned(), path);
        set.insert("TEXINPUTS".to_owned(), search.clone());
        set.insert("BIBINPUTS".to_owned(), search.clone());
        set.insert("BSTINPUTS".to_owned(), search);
        set.insert("max_print_line".to_owned(), "10000".to_owned());
        set.insert("error_line".to_owned(), "254".to_owned());
        set.insert("half_error_line".to_owned(), "238".to_owned());
        set.insert("openout_any".to_owned(), "p".to_owned());
        set.insert("openin_any".to_owned(), "p".to_owned());
        set.insert("shell_escape".to_owned(), "f".to_owned());
        let inherit = [
            "SystemRoot",
            "windir",
            "TEMP",
            "TMP",
            "TMPDIR",
            "USERPROFILE",
            "HOMEDRIVE",
            "HOMEPATH",
            "HOME",
            "APPDATA",
            "LOCALAPPDATA",
            "LANG",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        EnvironmentSpec { inherit, set }
    }

    fn version(&self, tool: TexTool, program: &Path, env: &EnvironmentSpec, cwd: &Path) -> String {
        if let Some(v) = self.versions.lock().unwrap_or_else(|p| p.into_inner()).get(&tool) {
            return v.clone();
        }
        let spec = JobSpec {
            label: format!("{} --version", tool.name()),
            program: program.display().to_string(),
            args: vec!["--version".into()],
            environment: env.clone(),
            working_directory: cwd.display().to_string(),
            policy: ExecutionPolicy::TrustedNative,
            timeout_ms: Some(20_000),
            log_limit_bytes: Some(64 << 10),
            stdin: None,
        };
        let output = self.runner.run(&spec);
        let version = output.stdout.lines().next().unwrap_or(tool.name()).trim().to_owned();
        self.versions.lock().unwrap_or_else(|p| p.into_inner()).insert(tool, version.clone());
        version
    }

    fn bibliography(
        &self,
        toolchain: &TexToolchain,
        out_dir: &Path,
        job: &str,
        overlay_dir: &Path,
        project_dir: &Path,
        env: &EnvironmentSpec,
    ) -> BibOutcome {
        let bcf = out_dir.join(format!("{job}.bcf"));
        let aux = read_lossy(&out_dir.join(format!("{job}.aux")));
        let (tool, control) = if bcf.is_file() {
            (TexTool::Biber, read_lossy(&bcf))
        } else if aux.contains("\\bibdata{") {
            let lines: String = aux
                .lines()
                .filter(|l| l.starts_with("\\citation") || l.starts_with("\\bibdata") || l.starts_with("\\bibstyle"))
                .collect::<Vec<_>>()
                .join("\n");
            (TexTool::Bibtex, lines)
        } else {
            return BibOutcome::NotNeeded;
        };
        // Bibliography databases referenced by the control data.
        let mut hasher = Sha256::new();
        hasher.update(control.as_bytes());
        for name in bib_databases(&control, tool) {
            let file = if name.ends_with(".bib") { name.clone() } else { format!("{name}.bib") };
            for dir in [overlay_dir, project_dir] {
                if let Ok(bytes) = std::fs::read(dir.join(&file)) {
                    hasher.update(&bytes);
                    break;
                }
            }
        }
        let hash = hex::encode(hasher.finalize());
        let stamp = out_dir.join(format!("{job}.exposition-bib"));
        let bbl = out_dir.join(format!("{job}.bbl"));
        if bbl.is_file() && std::fs::read_to_string(&stamp).ok().as_deref() == Some(hash.as_str()) {
            return BibOutcome::NotNeeded;
        }
        let Some(program) = toolchain.tool(tool) else { return BibOutcome::Missing(tool) };
        let mut args = vec![job.to_owned()];
        if tool == TexTool::Biber {
            args = vec![
                "--input-directory".into(),
                slash(overlay_dir),
                "--input-directory".into(),
                slash(project_dir),
                job.to_owned(),
            ];
        }
        let spec = JobSpec {
            label: format!("{} {job}", tool.name()),
            program: program.display().to_string(),
            args,
            environment: env.clone(),
            working_directory: out_dir.display().to_string(),
            policy: ExecutionPolicy::TrustedNative,
            timeout_ms: Some(ENGINE_TIMEOUT_MS),
            log_limit_bytes: Some(1 << 20),
            stdin: None,
        };
        let output = self.runner.run(&spec);
        let record =
            PassRecord { tool: tool.name().into(), duration_ms: output.duration_ms, exit_code: output.exit_code() };
        match output.exit_code() {
            // bibtex exits 1 for warnings, 2 for errors.
            Some(0) | Some(1) if bbl.is_file() => {
                let _ = std::fs::write(&stamp, &hash);
                BibOutcome::Ran(record)
            }
            _ => {
                let detail = output.stdout.lines().rev().take(6).collect::<Vec<_>>().join(" | ");
                BibOutcome::Failed(record, format!("{} failed: {detail}", tool.name()))
            }
        }
    }

    fn snapshot(
        &self,
        target: &BuildTarget,
        fls: &Path,
        buffers: &BTreeMap<WorkspacePath, &DocumentSnapshot>,
        overlay_root: &Path,
        project_root: &Path,
        entry_dir: Option<WorkspacePath>,
    ) -> BuildSnapshot {
        let mut files: BTreeMap<WorkspacePath, SnapshotFile> = BTreeMap::new();
        for (path, doc) in buffers {
            files.insert(
                path.clone(),
                SnapshotFile {
                    content_hash: hex_sha256(doc.text.as_bytes()),
                    text: Some(doc.text.clone()),
                    document: Some(doc.id.clone()),
                    revision: Some(doc.revision),
                },
            );
        }
        let recorder = read_lossy(fls);
        let mut pwd: Option<PathBuf> = None;
        for line in recorder.lines() {
            if let Some(p) = line.strip_prefix("PWD ") {
                pwd = Some(PathBuf::from(p));
                continue;
            }
            let Some(name) = line.strip_prefix("INPUT ") else { continue };
            let absolute = match &pwd {
                Some(dir) if Path::new(name).is_relative() => slash(&dir.join(name)),
                _ => name.to_owned(),
            };
            let Some(path) = resolve_name(&absolute, overlay_root, project_root, entry_dir.as_ref()) else {
                continue;
            };
            if files.contains_key(&path) {
                continue;
            }
            let disk = path.components().fold(project_root.to_path_buf(), |p, c| p.join(c));
            let Ok(bytes) = std::fs::read(&disk) else { continue };
            let is_text = matches!(path.extension(), Some("tex" | "ltx" | "sty" | "cls" | "bib" | "bbx" | "cbx"));
            let text = if is_text { String::from_utf8(bytes.clone()).ok().map(Arc::from) } else { None };
            files.insert(path, SnapshotFile { content_hash: hex_sha256(&bytes), text, document: None, revision: None });
        }
        let mut hasher = Sha256::new();
        hasher.update(format!("{:?}|{}|", target.engine, target.entry).as_bytes());
        for (path, file) in &files {
            hasher.update(path.as_str().as_bytes());
            hasher.update(b"=");
            hasher.update(file.content_hash.as_bytes());
            hasher.update(b"\n");
        }
        BuildSnapshot {
            id: SnapshotId(hex::encode(hasher.finalize())),
            files,
            overlay_root: overlay_root.to_path_buf(),
            project_root: project_root.to_path_buf(),
            entry_dir,
        }
    }
}

enum BibOutcome {
    NotNeeded,
    Ran(PassRecord),
    Missing(TexTool),
    Failed(PassRecord, String),
}

fn bib_databases(control: &str, tool: TexTool) -> Vec<String> {
    match tool {
        TexTool::Bibtex => control
            .lines()
            .filter_map(|l| l.strip_prefix("\\bibdata{"))
            .flat_map(|rest| rest.trim_end_matches('}').split(',').map(|s| s.trim().to_owned()).collect::<Vec<_>>())
            .collect(),
        _ => {
            // <bcf:datasource type="file" datatype="bibtex" ...>refs.bib</bcf:datasource>
            let mut names = Vec::new();
            let mut rest = control;
            while let Some(start) = rest.find("<bcf:datasource") {
                let after = &rest[start..];
                let Some(open_end) = after.find('>') else { break };
                let Some(close) = after.find("</bcf:datasource>") else { break };
                if open_end < close {
                    names.push(after[open_end + 1..close].trim().to_owned());
                }
                rest = &after[close..];
            }
            names
        }
    }
}

/// Resolve a file name printed by the engine to a workspace path.
fn resolve_name(
    name: &str,
    overlay_root: &Path,
    project_root: &Path,
    entry_dir: Option<&WorkspacePath>,
) -> Option<WorkspacePath> {
    let name = name.trim().replace('\\', "/");
    let lower = |s: &str| if cfg!(windows) { s.to_ascii_lowercase() } else { s.to_owned() };
    let candidate = clean_dots(&name);
    let is_absolute = candidate.starts_with('/') || candidate.as_bytes().get(1) == Some(&b':');
    if !is_absolute {
        let joined = match entry_dir {
            Some(dir) => format!("{}/{}", dir.as_str(), candidate),
            None => candidate.clone(),
        };
        return WorkspacePath::parse(&joined).ok();
    }
    for root in [overlay_root, project_root] {
        let root = format!("{}/", slash(root).trim_end_matches('/'));
        if lower(&candidate).starts_with(&lower(&root)) {
            return WorkspacePath::parse(&candidate[root.len()..]).ok();
        }
    }
    None
}

/// Remove `./` components and `x/../` pairs lexically.
fn clean_dots(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    let absolute = path.starts_with('/');
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    let joined = parts.join("/");
    if absolute { format!("/{joined}") } else { joined }
}

fn diagnostics_from_log(log: &str, snapshot: &BuildSnapshot) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for message in parse_log(log) {
        let mut hints = message.context.clone();
        let location = match (&message.file, message.lines) {
            (Some(file), Some((l0, l1))) => match snapshot.resolve(file) {
                Some(path) => {
                    let range = snapshot.text(&path).and_then(|text| line_range(text, l0, l1));
                    if range.is_none() {
                        hints.push(format!("{path}:{l0}"));
                    }
                    range.map(|range| SourceLocation { path, range })
                }
                None => {
                    hints.push(format!("{file}:{l0}"));
                    None
                }
            },
            _ => None,
        };
        if matches!(message.kind, LogMessageKind::UndefinedReference | LogMessageKind::UndefinedCitation) {
            hints.push("Run a full build to resolve references and the bibliography.".into());
        }
        out.push(Diagnostic { severity: message.severity, message: message.message, location, hints });
    }
    out
}

/// UTF-16 range covering 1-based lines `l0..=l1`.
fn line_range(text: &str, l0: u32, l1: u32) -> Option<TextRange> {
    let index = LineIndex::new(text);
    let first = l0.checked_sub(1)? as usize;
    let last = (l1.max(l0) - 1) as usize;
    let start = index.line_start(first)?;
    let last = last.min(index.line_count() - 1);
    let end_line_start = index.line_start(last)?;
    // Find the end of the last line's content.
    let mut end = end_line_start;
    while index.offset(LineColumn { line: last as u32, character: (end - end_line_start + 1) as u32 }).is_ok() {
        end += 1;
    }
    Some(TextRange::new(start as u32, end as u32))
}

fn prepare_dirs(overlay_root: &Path, out_dir: &Path, project_root: &Path) -> std::io::Result<()> {
    if overlay_root.exists() {
        std::fs::remove_dir_all(overlay_root)?;
    }
    std::fs::create_dir_all(overlay_root)?;
    std::fs::create_dir_all(out_dir)?;
    // `\include{dir/file}` writes `dir/file.aux` into the output directory.
    mirror_dirs(project_root, out_dir, 0)?;
    Ok(())
}

fn mirror_dirs(from: &Path, to: &Path, depth: usize) -> std::io::Result<()> {
    if depth > 3 {
        return Ok(());
    }
    let Ok(entries) = std::fs::read_dir(from) else { return Ok(()) };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with('.') || matches!(name_str.as_ref(), "node_modules" | "target") {
            continue;
        }
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            let dest = to.join(&name);
            std::fs::create_dir_all(&dest)?;
            mirror_dirs(&entry.path(), &dest, depth + 1)?;
        }
    }
    Ok(())
}

fn prune_builds(dir: &Path, keep: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut builds: Vec<(std::time::SystemTime, PathBuf)> =
        entries.flatten().filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path()))).collect();
    builds.sort();
    let excess = builds.len().saturating_sub(keep);
    for (_, path) in builds.into_iter().take(excess) {
        let _ = std::fs::remove_dir_all(path);
    }
}

fn font_fingerprint(fls: &Path) -> String {
    let recorder = read_lossy(fls);
    let mut fonts: Vec<&str> = recorder
        .lines()
        .filter_map(|l| l.strip_prefix("INPUT "))
        .filter(|n| {
            let n = n.to_ascii_lowercase();
            [".pfb", ".otf", ".ttf", ".tfm", ".vf", ".enc", ".map"].iter().any(|e| n.ends_with(e))
        })
        .collect();
    fonts.sort_unstable();
    fonts.dedup();
    hex_sha256(fonts.join("\n").as_bytes())
}

fn read_lossy(path: &Path) -> String {
    std::fs::read(path).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default()
}

fn file_hash(path: &Path) -> Option<String> {
    std::fs::read(path).ok().map(|b| hex_sha256(&b))
}

pub(crate) fn hex_sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn short_hash(text: &str) -> String {
    hex_sha256(text.as_bytes())[..16].to_owned()
}

/// Forward-slash form of a path (what TeX expects on every platform).
fn slash(path: &Path) -> String {
    plain_path(path).display().to_string().replace('\\', "/")
}

/// Strip the Windows verbatim prefix (`\\?\C:\...`) that TeX cannot read.
fn plain_path(path: &Path) -> PathBuf {
    let s = path.display().to_string();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC\\") => PathBuf::from(rest),
        _ => path.to_path_buf(),
    }
}

mod hex {
    pub fn encode(bytes: impl AsRef<[u8]>) -> String {
        bytes.as_ref().iter().map(|b| format!("{b:02x}")).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_engine_names_to_workspace_paths() {
        let overlay = Path::new("C:/app/builds/abc/src");
        let project = Path::new("C:/Users/me/paper");
        let dir = WorkspacePath::parse("doc").ok();
        assert_eq!(
            resolve_name("c:/app/builds/abc/src/./doc/main.tex", overlay, project, dir.as_ref()).unwrap().as_str(),
            "doc/main.tex"
        );
        assert_eq!(
            resolve_name("C:/Users/me/paper/doc/sections/a.tex", overlay, project, dir.as_ref()).unwrap().as_str(),
            "doc/sections/a.tex"
        );
        assert_eq!(
            resolve_name("./sections/b.tex", overlay, project, dir.as_ref()).unwrap().as_str(),
            "doc/sections/b.tex"
        );
        assert_eq!(resolve_name("c:/texlive/texmf-dist/tex/latex/base/article.cls", overlay, project, None), None);
    }

    #[test]
    fn line_ranges_cover_whole_lines() {
        let text = "ab\r\ncdé\nlast";
        assert_eq!(line_range(text, 1, 1), Some(TextRange::new(0, 2)));
        assert_eq!(line_range(text, 2, 2), Some(TextRange::new(4, 7)));
        assert_eq!(line_range(text, 2, 3), Some(TextRange::new(4, 12)));
        assert_eq!(line_range(text, 9, 9), None);
    }

    #[test]
    fn bib_database_names() {
        assert_eq!(bib_databases("\\bibdata{refs, more}\n", TexTool::Bibtex), vec!["refs", "more"]);
        let bcf = r#"<bcf:datasource type="file" datatype="bibtex" glob="false">refs.bib</bcf:datasource>"#;
        assert_eq!(bib_databases(bcf, TexTool::Biber), vec!["refs.bib"]);
    }
}
