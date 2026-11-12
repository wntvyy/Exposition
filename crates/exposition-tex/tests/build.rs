//! Build adapter tests against the real pdfLaTeX and LuaLaTeX engines.

mod common;

use std::collections::BTreeMap;
use std::path::Path;

use exposition_core::build::{BuildStatus, Engine, MappingPrecision, Severity};
use exposition_core::document::{
    ConflictPolicy, DocumentEdit, EditOrigin, TextPatch, TransactionId, TransactionRequest, UndoBehavior,
};
use exposition_core::text::utf16_len;
use exposition_tex::{BuildMode, LocalProcessRunner, TexBuilder, TexPageSession, TexToolchain};

const ENGINES: [Engine; 2] = [Engine::PdfLatex, Engine::LuaLatex];

/// Every file under `dir` with its content hash.
fn tree(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let rel = path.strip_prefix(dir).unwrap().display().to_string();
                out.insert(rel, std::fs::read(&path).unwrap());
            }
        }
    }
    out
}

fn page_text(build: &exposition_tex::TexBuild) -> String {
    let session = TexPageSession::open(build).expect("session");
    (0..session.page_count())
        .map(|i| session.pdf.extract_glyphs(i).unwrap().iter().filter_map(|g| g.text.clone()).collect::<String>())
        .collect()
}

#[test]
fn full_build_with_bibliography_and_reruns_leaves_the_project_untouched() {
    for engine in ENGINES {
        let Some(h) = common::Harness::new("latex-basic", engine) else { continue };
        let before = tree(h.project.path());
        let build = h.builder.build(&h.request(BuildMode::Full));
        let result = &build.result;
        assert_eq!(result.status, BuildStatus::Succeeded, "{engine:?}: {:?}", result.diagnostics);
        assert_eq!(result.mapping, MappingPrecision::Glyph);
        assert_eq!(result.pages.len(), 2);
        assert!((result.pages[0].width_pt - 612.0).abs() < 1.0, "letter paper");
        assert_eq!(result.exports.len(), 1);
        assert!(
            Path::new(&result.exports[0].path)
                .starts_with(h.build_root.path().canonicalize().unwrap().to_string_lossy().trim_start_matches(r"\\?\"))
        );
        let config = result.configuration.as_ref().unwrap();
        let expected_compiler = if engine == Engine::PdfLatex { "pdfTeX" } else { "LuaHBTeX" };
        assert!(config.compiler.contains(expected_compiler), "{}", config.compiler);
        assert_eq!(config.font_fingerprint.len(), 64);
        let tools: Vec<&str> = build.passes.iter().map(|p| p.tool.as_str()).collect();
        assert!(tools.contains(&"bibtex"), "{tools:?}");
        assert!(tools.iter().filter(|t| t.ends_with("latex")).count() >= 2, "{tools:?}");
        // Inputs actually read are recorded with hashes.
        let paths: Vec<&str> = result.sources.iter().map(|s| s.path.as_str()).collect();
        for expected in ["main.tex", "sections/results.tex", "figures/box.png"] {
            assert!(paths.contains(&expected), "{engine:?} missing {expected} in {paths:?}");
        }
        assert!(result.sources.iter().all(|s| s.content_hash.len() == 64));
        assert_eq!(result.snapshot.0.len(), 64);
        // References and the citation resolved after reruns.
        let text = page_text(&build);
        assert!(text.contains("[1]"), "citation label");
        assert!(text.contains("Section2"), "cross-reference");
        assert!(!result.diagnostics.iter().any(|d| d.message.contains("undefined")), "{:?}", result.diagnostics);
        // Nothing was written into the project.
        assert_eq!(tree(h.project.path()), before, "{engine:?} modified the project directory");
        eprintln!("{engine:?} full build: {} ms, passes {:?}", result.duration_ms, build.passes);
    }
}

#[test]
fn unsaved_buffers_are_compiled_from_the_snapshot() {
    for engine in ENGINES {
        let Some(h) = common::Harness::new("latex-basic", engine) else { continue };
        let id = h.open("sections/results.tex");
        let text = h.text(&id);
        let at = text.find("The result").unwrap() + "The ".len();
        let at16 = utf16_len(&text[..at]) as u32;
        h.documents
            .apply(TransactionRequest {
                id: TransactionId("t1".into()),
                origin: EditOrigin::Source,
                edits: vec![DocumentEdit {
                    document: id.clone(),
                    base_revision: 1,
                    patches: vec![TextPatch::insert(at16, "UNSAVED ")],
                }],
                undo: UndoBehavior::NewGroup,
                conflict: ConflictPolicy::Reject,
                label: None,
                selection: None,
            })
            .unwrap();
        let on_disk = std::fs::read(h.project.path().join("sections/results.tex")).unwrap();
        let build = h.builder.build(&h.request(BuildMode::Full));
        assert_eq!(build.result.status, BuildStatus::Succeeded);
        assert!(page_text(&build).contains("TheUNSAVEDresult"), "{engine:?}");
        let source = build.result.sources.iter().find(|s| s.path.as_str() == "sections/results.tex").unwrap();
        assert_eq!(source.document.as_ref(), Some(&id));
        assert_eq!(source.revision, Some(2));
        assert_eq!(std::fs::read(h.project.path().join("sections/results.tex")).unwrap(), on_disk);
        // Snapshot ids are content-derived: the same inputs give the same id.
        let again = h.builder.build(&h.request(BuildMode::Draft));
        assert_eq!(again.result.snapshot, build.result.snapshot);
        assert_eq!(again.passes.len(), 1, "draft builds run one engine pass");
    }
}

#[test]
fn errors_warnings_and_bad_boxes_become_located_diagnostics() {
    for engine in ENGINES {
        let Some(h) = common::Harness::new("latex-errors", engine) else { continue };
        let build = h.builder.build(&h.request(BuildMode::Draft));
        let result = &build.result;
        assert_eq!(result.status, BuildStatus::Failed, "{engine:?}");
        let main = std::fs::read_to_string(h.project.path().join("main.tex")).unwrap();
        let line_range = |text: &str, line: usize| {
            let start: usize = text.split('\n').take(line - 1).map(|l| utf16_len(l) + 1).sum();
            let len = utf16_len(text.split('\n').nth(line - 1).unwrap());
            (start as u32, (start + len) as u32)
        };
        let find = |severity: Severity, needle: &str| {
            result
                .diagnostics
                .iter()
                .find(|d| d.severity == severity && d.message.contains(needle))
                .unwrap_or_else(|| panic!("{engine:?}: no {severity:?} '{needle}' in {:#?}", result.diagnostics))
        };
        let undefined: Vec<_> = result
            .diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Error && d.message.contains("Undefined control sequence"))
            .collect();
        assert_eq!(undefined.len(), 2, "{engine:?}");
        let first = undefined[0].location.as_ref().unwrap();
        assert_eq!(first.path.as_str(), "main.tex");
        assert_eq!((first.range.start, first.range.end), line_range(&main, 8));
        assert!(undefined[0].hints.iter().any(|h| h.contains("\\undefinedmacro")));
        let second = undefined[1].location.as_ref().unwrap();
        assert_eq!(second.path.as_str(), "sections/part.tex");
        assert_eq!(second.range.start, 0);
        let reference = find(Severity::Warning, "Reference `sec:nowhere'");
        assert_eq!(reference.location.as_ref().unwrap().range.start, line_range(&main, 9).0);
        find(Severity::Warning, "Citation `nokey'");
        let overfull = find(Severity::Info, "Overfull \\hbox");
        assert_eq!(overfull.location.as_ref().unwrap().range.start, line_range(&main, 12).0);
        // A PDF is still produced (TeX recovers in nonstopmode): the last output.
        assert!(build.output.is_some());
    }
}

#[test]
fn missing_engines_report_tool_unavailable() {
    let build_root = tempfile::tempdir().unwrap();
    let project = common::project_from_fixture("latex-basic");
    let runner = LocalProcessRunner::new(vec![build_root.path().to_path_buf()]);
    let builder = TexBuilder::new(runner, None, build_root.path().to_path_buf()).unwrap();
    let request = exposition_tex::TexBuildRequest {
        target: exposition_core::build::BuildTarget {
            format: exposition_core::build::DocumentFormat::Latex,
            engine: Engine::PdfLatex,
            entry: exposition_core::path::WorkspacePath::parse("main.tex").unwrap(),
        },
        project_root: project.path().to_path_buf(),
        buffers: Vec::new(),
        mode: BuildMode::Full,
        tounicode: false,
        workspace: None,
    };
    let build = builder.build(&request);
    match &build.result.status {
        BuildStatus::ToolUnavailable { tool, guidance } => {
            assert_eq!(tool, "pdflatex");
            assert!(guidance.contains("TeX Live") && guidance.contains("PATH"));
        }
        other => panic!("unexpected {other:?}"),
    }
    // A distribution that has pdfLaTeX but no LuaLaTeX.
    let fake_bin = tempfile::tempdir().unwrap();
    let exe = if cfg!(windows) { "pdflatex.exe" } else { "pdflatex" };
    std::fs::write(fake_bin.path().join(exe), b"").unwrap();
    let partial = TexToolchain::in_dir(fake_bin.path()).unwrap();
    let runner = LocalProcessRunner::new(vec![build_root.path().to_path_buf()]);
    let builder = TexBuilder::new(runner, Some(partial), build_root.path().to_path_buf()).unwrap();
    let mut lua = request.clone();
    lua.target.engine = Engine::LuaLatex;
    assert!(matches!(
        builder.build(&lua).result.status,
        BuildStatus::ToolUnavailable { ref tool, .. } if tool == "lualatex"
    ));
}

/// Writes outside the build output and process execution are refused.
/// (Reads are not confined by TeX; see the `compile` module docs.)
#[test]
fn writes_outside_the_build_and_process_execution_are_refused() {
    for engine in ENGINES {
        let Some(h) = common::Harness::new("latex-basic", engine) else { continue };
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().canonicalize().unwrap().join("escape.tex");
        let abs = target.display().to_string().trim_start_matches(r"\\?\").replace('\\', "/");
        let attempts = [
            format!("\\immediate\\openout\\w={abs}\\immediate\\write\\w{{x}}\\immediate\\closeout\\w"),
            "\\immediate\\openout\\w=../escape.tex\\immediate\\write\\w{x}\\immediate\\closeout\\w".to_owned(),
            "\\immediate\\write18{cmd /c echo pwned > pwned.txt}\\immediate\\write18{touch pwned.txt}".to_owned(),
            concat!(
                "\\ifdefined\\directlua\\directlua{",
                "if os.execute then os.execute('cmd /c echo pwned > pwned.txt') end ",
                "if io.popen then io.popen('cmd /c echo pwned > pwned.txt') end ",
                "local f = io.open('../pwned.txt', 'w') if f then f:write('x') f:close() end}\\fi"
            )
            .to_owned(),
        ];
        for attempt in attempts {
            let main = format!(
                "\\documentclass{{article}}\n\\newwrite\\w\n\\begin{{document}}\nVisible.\n{attempt}\n\\end{{document}}\n"
            );
            std::fs::write(h.project.path().join("main.tex"), &main).unwrap();
            let before = tree(h.project.path());
            let build = h.builder.build(&h.request(BuildMode::Draft));
            assert!(!target.exists(), "{engine:?} wrote outside: {attempt}");
            assert_eq!(tree(h.project.path()), before, "{engine:?} wrote into the project: {attempt}");
            let escaped: Vec<_> = tree(h.build_root.path())
                .into_keys()
                .filter(|k| k.contains("pwned") || k.ends_with("escape.tex"))
                .collect();
            assert!(escaped.is_empty(), "{engine:?}: {escaped:?} after {attempt}");
            let _ = build;
        }
    }
}

/// Explicitly relative names (`./x`, `../x`) are opened relative to the
/// engine's working directory (kpathsea does not search `TEXINPUTS` for them),
/// so they must resolve from the entry's directory exactly as in the project,
/// while open buffers still override the files on disk and nothing is written
/// into the project.
#[test]
fn explicitly_relative_paths_resolve_from_the_entry_directory_as_in_the_project() {
    for engine in ENGINES {
        let Some(h) = common::Harness::new("latex-basic", engine) else { continue };
        let root = h.project.path();
        let write = |path: &str, text: &str| {
            let file = root.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, text).unwrap();
        };
        std::fs::create_dir_all(root.join("figs")).unwrap();
        std::fs::copy(root.join("figures/box.png"), root.join("figs/x.png")).unwrap();
        std::fs::create_dir_all(root.join(".exposition/artifacts/run-1")).unwrap();
        std::fs::copy(root.join("figures/box.png"), root.join(".exposition/artifacts/run-1/output-1.png")).unwrap();
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::copy(root.join("figures/box.png"), root.join("sub/local.png")).unwrap();
        write("shared/macros.tex", "\\newcommand{\\shared}{SHAREDDISK}\n");
        write(
            "sub/report.tex",
            concat!(
                "\\documentclass{article}\n\\usepackage{graphicx}\n\\input{../shared/macros}\n",
                "\\begin{document}\nReport \\shared.\n",
                "\\includegraphics[width=1cm]{../figs/x.png}\n",
                "\\includegraphics[width=1cm]{../.exposition/artifacts/run-1/output-1.png}\n",
                "\\includegraphics[width=1cm]{./local.png}\n\\end{document}\n"
            ),
        );
        write(
            "book.tex",
            "\\documentclass{article}\n\\usepackage{graphicx}\n\\begin{document}\nBook.\n\\input{chapters/ch1}\n\\end{document}\n",
        );
        write(
            "chapters/ch1.tex",
            concat!(
                "Chapter one.\n\\input{./chapters/part}\n",
                "\\includegraphics[width=1cm]{./figs/x.png}\n\\includegraphics[width=1cm]{figs/x.png}\n"
            ),
        );
        write("chapters/part.tex", "PARTDISK.\n");
        let before = tree(root);
        let request = |entry: &str, mode| {
            let mut request = h.request(mode);
            request.target.entry = exposition_core::path::WorkspacePath::parse(entry).unwrap();
            request.buffers = h.documents.snapshots();
            request
        };
        let sources = |build: &exposition_tex::TexBuild| -> Vec<String> {
            build.result.sources.iter().map(|s| s.path.as_str().to_owned()).collect()
        };

        // A document built as its own entry from a subdirectory.
        let report = h.builder.build(&request("sub/report.tex", BuildMode::Draft));
        assert_eq!(report.result.status, BuildStatus::Succeeded, "{engine:?}: {:#?}", report.result.diagnostics);
        assert!(page_text(&report).contains("ReportSHAREDDISK."), "{engine:?}: {}", page_text(&report));
        let paths = sources(&report);
        for expected in
            ["sub/report.tex", "shared/macros.tex", "figs/x.png", ".exposition/artifacts/run-1/output-1.png"]
        {
            assert!(paths.iter().any(|p| p == expected), "{engine:?} missing {expected} in {paths:?}");
        }
        assert!(paths.iter().any(|p| p == "sub/local.png"), "{engine:?}: {paths:?}");

        // An open buffer overrides the disk file reached through `../`.
        let macros = h.open("shared/macros.tex");
        let disk = h.text(&macros).find("DISK").unwrap() as u32;
        h.documents
            .apply(TransactionRequest {
                id: TransactionId("m1".into()),
                origin: EditOrigin::Source,
                edits: vec![DocumentEdit {
                    document: macros.clone(),
                    base_revision: 1,
                    patches: vec![TextPatch::replace(disk, disk + 4, "BUFFER")],
                }],
                undo: UndoBehavior::NewGroup,
                conflict: ConflictPolicy::Reject,
                label: None,
                selection: None,
            })
            .unwrap();
        assert!(h.text(&macros).contains("SHAREDBUFFER"), "{}", h.text(&macros));
        let buffered = h.builder.build(&request("sub/report.tex", BuildMode::Draft));
        assert!(page_text(&buffered).contains("ReportSHAREDBUFFER."), "{engine:?}: {}", page_text(&buffered));
        let source = buffered.result.sources.iter().find(|s| s.path.as_str() == "shared/macros.tex").unwrap();
        assert_eq!(source.document.as_ref(), Some(&macros));
        // Closing the buffer returns to the disk file; a disk change is seen.
        h.documents.close(&macros).unwrap();
        let closed = h.builder.build(&request("sub/report.tex", BuildMode::Draft));
        assert!(page_text(&closed).contains("ReportSHAREDDISK."), "{engine:?}: {}", page_text(&closed));
        write("shared/macros.tex", "\\newcommand{\\shared}{SHAREDCHANGEDONDISK}\n");
        let changed = h.builder.build(&request("sub/report.tex", BuildMode::Draft));
        assert!(page_text(&changed).contains("ReportSHAREDCHANGEDONDISK."), "{engine:?}: {}", page_text(&changed));
        write("shared/macros.tex", "\\newcommand{\\shared}{SHAREDDISK}\n");

        // The root entry's included chapter uses root-relative names, as TeX
        // resolves them from the working directory, not the including file.
        let book = h.builder.build(&request("book.tex", BuildMode::Draft));
        assert_eq!(book.result.status, BuildStatus::Succeeded, "{engine:?}: {:#?}", book.result.diagnostics);
        assert!(page_text(&book).contains("PARTDISK."), "{engine:?}: {}", page_text(&book));
        let paths = sources(&book);
        for expected in ["book.tex", "chapters/ch1.tex", "chapters/part.tex", "figs/x.png"] {
            assert!(paths.iter().any(|p| p == expected), "{engine:?} missing {expected} in {paths:?}");
        }
        let part = h.open("chapters/part.tex");
        let disk = h.text(&part).find("DISK").unwrap() as u32;
        h.documents
            .apply(TransactionRequest {
                id: TransactionId("p1".into()),
                origin: EditOrigin::Source,
                edits: vec![DocumentEdit {
                    document: part.clone(),
                    base_revision: 1,
                    patches: vec![TextPatch::replace(disk, disk + 4, "BUFFER")],
                }],
                undo: UndoBehavior::NewGroup,
                conflict: ConflictPolicy::Reject,
                label: None,
                selection: None,
            })
            .unwrap();
        let book = h.builder.build(&request("book.tex", BuildMode::Draft));
        assert!(page_text(&book).contains("PARTBUFFER."), "{engine:?}: {}", page_text(&book));
        h.documents.close(&part).unwrap();

        // Nothing was written into the project.
        assert_eq!(tree(root), before, "{engine:?} modified the project directory");
    }
}

#[test]
fn biblatex_projects_run_biber() {
    for engine in ENGINES {
        let Some(h) = common::Harness::new("latex-biblatex", engine) else { continue };
        if h.builder.toolchain().and_then(|t| t.tool(exposition_tex::TexTool::Biber).map(|_| ())).is_none() {
            eprintln!("SKIPPED: biber not found");
            return;
        }
        let build = h.builder.build(&h.request(BuildMode::Full));
        assert_eq!(build.result.status, BuildStatus::Succeeded, "{engine:?}: {:#?}", build.result.diagnostics);
        let tools: Vec<&str> = build.passes.iter().map(|p| p.tool.as_str()).collect();
        assert!(tools.contains(&"biber"), "{tools:?}");
        let text = page_text(&build);
        assert!(text.contains("detail[1]") && text.contains("TheTEXbook"), "{engine:?}: {text}");
        eprintln!("{engine:?} biblatex: {} ms, passes {:?}", build.result.duration_ms, build.passes);
        // Unchanged citations and databases: biber is not rerun.
        let again = h.builder.build(&h.request(BuildMode::Full));
        assert!(!again.passes.iter().any(|p| p.tool == "biber"), "{:?}", again.passes);
    }
}

#[test]
fn entry_names_tex_would_interpret_are_rejected() {
    let Some(h) = common::Harness::new("latex-basic", Engine::PdfLatex) else { return };
    for name in ["&pdflatex.tex", "-shell-escape.tex", "my paper.tex"] {
        std::fs::copy(h.project.path().join("main.tex"), h.project.path().join(name)).unwrap();
        let mut request = h.request(BuildMode::Draft);
        request.target.entry = exposition_core::path::WorkspacePath::parse(name).unwrap();
        let build = h.builder.build(&request);
        assert!(
            matches!(&build.result.status, BuildStatus::Error { message } if message.contains("not supported")),
            "{name}: {:?}",
            build.result.status
        );
        assert!(build.passes.is_empty(), "{name}: no engine may run");
    }
}

/// R-m23 F7: a program configured as the engine that is not a TeX engine
/// fails with an explanation (not "failed, 0 errors").
#[cfg(windows)]
#[test]
fn a_wrong_engine_executable_is_explained() {
    let Some(h) = common::Harness::new("latex-basic", Engine::LuaLatex) else { return };
    let dir = tempfile::tempdir().unwrap();
    let fake = dir.path().join("lualatex.exe");
    let system = std::env::var_os("SystemRoot").map(std::path::PathBuf::from).unwrap_or("C:\\Windows".into());
    std::fs::copy(system.join("System32").join("where.exe"), &fake).unwrap();
    let mut tools = std::collections::BTreeMap::new();
    tools.insert(exposition_tex::TexTool::LuaLatex, fake.clone());
    // A successful build first leaves a log behind in the build directory.
    let good = h.builder.build(&h.request(BuildMode::Draft));
    assert_eq!(good.result.status, BuildStatus::Succeeded);
    h.builder.set_toolchain(TexToolchain::from_tools(tools));
    let build = h.builder.build(&h.request(BuildMode::Draft));
    assert_eq!(build.result.status, BuildStatus::Failed);
    let explained = build
        .result
        .diagnostics
        .iter()
        .find(|d| d.severity == Severity::Error && d.message.contains("did not run as a TeX engine"))
        .unwrap_or_else(|| panic!("no explanation: {:#?}", build.result.diagnostics));
    assert!(explained.message.contains("lualatex"), "{}", explained.message);
    assert!(explained.hints.iter().any(|h| h.contains("Settings")), "{:?}", explained.hints);
}
