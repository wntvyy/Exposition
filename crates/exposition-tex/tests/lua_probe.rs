//! Evaluation of a LuaLaTeX callback approach to exact glyph → source maps.
//!
//! A `pre_shipout_filter` walks every shipped node list and reports, per
//! node type, whether LuaTeX attached SyncTeX (tag, line) fields. It also
//! probes the Lua API for any input-column facility. The result (asserted
//! below) is the evidence for the mapping design: LuaTeX glyph nodes carry no
//! source line (tag 0, line 0) and no column API exists, so a Lua callback
//! cannot produce an exact glyph → (line, column) map without instrumenting
//! the source; glue/kern/math nodes carry lines exactly as SyncTeX records.

mod common;

use exposition_core::build::Engine;
use exposition_core::job::{EnvironmentSpec, ExecutionPolicy, JobSpec};
use exposition_tex::{JobRunner, LocalProcessRunner, TexTool};

const PROBE: &str = r#"
local D = node.direct
local GLYPH, HLIST, VLIST, DISC, GLUE, KERN = node.id('glyph'), node.id('hlist'), node.id('vlist'),
  node.id('disc'), node.id('glue'), node.id('kern')
local counts = { glyph = 0, glyph_line = 0, glue = 0, glue_line = 0, ligature = 0, disc = 0 }
local function walk(head)
  local n = head
  while n do
    local id = D.getid(n)
    if id == GLYPH then
      counts.glyph = counts.glyph + 1
      local tag, line = D.get_synctex_fields(n)
      if (line or 0) > 0 then counts.glyph_line = counts.glyph_line + 1 end
      if D.getcomponents(n) then counts.ligature = counts.ligature + 1 end
    elseif id == GLUE or id == KERN then
      counts.glue = counts.glue + 1
      local tag, line = D.get_synctex_fields(n)
      if (line or 0) > 0 then counts.glue_line = counts.glue_line + 1 end
    elseif id == DISC then
      counts.disc = counts.disc + 1
      local pre, post, replace = D.getdisc(n)
      walk(replace)
    elseif id == HLIST or id == VLIST then
      walk(D.getlist(n))
    end
    n = D.getnext(n)
  end
end
local column_api = 0
for _, lib in ipairs({ tex, status, token, node, D }) do
  for k in pairs(lib) do
    local s = tostring(k)
    if s:find('column') or s:find('inputloc') or s:find('charpos') then column_api = column_api + 1 end
  end
end
luatexbase.add_to_callback('pre_shipout_filter', function(head)
  walk(D.todirect(head))
  texio.write_nl('log', string.format('EXPOSITION-PROBE glyph=%d glyph_line=%d glue=%d glue_line=%d ligature=%d disc=%d column_api=%d',
    counts.glyph, counts.glyph_line, counts.glue, counts.glue_line, counts.ligature, counts.disc, column_api))
  return true
end, 'probe')
"#;

#[test]
fn lua_callbacks_see_no_per_glyph_source_positions() {
    let Some(toolchain) = common::toolchain(Engine::LuaLatex) else { return };
    let project = common::project_from_fixture("latex-basic");
    std::fs::write(project.path().join("probe.lua"), PROBE).unwrap();
    let runner = LocalProcessRunner::new(vec![project.path().to_path_buf()]);
    let mut environment = EnvironmentSpec {
        inherit: ["SystemRoot", "TEMP", "TMP", "USERPROFILE", "HOME", "APPDATA", "LOCALAPPDATA"]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        ..Default::default()
    };
    environment.set.insert("PATH".into(), toolchain.bin_dir.display().to_string());
    environment.set.insert("max_print_line".into(), "10000".into());
    let spec = JobSpec {
        label: "lualatex probe".into(),
        program: toolchain.tool(TexTool::LuaLatex).unwrap().display().to_string(),
        args: vec![
            "-synctex=1".into(),
            "-interaction=nonstopmode".into(),
            "-no-shell-escape".into(),
            "-jobname=main".into(),
            "\\directlua{dofile('probe.lua')}\\input{main.tex}".into(),
        ],
        environment,
        working_directory: project.path().canonicalize().unwrap().display().to_string(),
        policy: ExecutionPolicy::TrustedNative,
        timeout_ms: Some(120_000),
        log_limit_bytes: None,
        stdin: None,
    };
    let output = runner.run(&spec);
    assert_eq!(output.exit_code(), Some(0), "{}", output.stdout);
    let log = std::fs::read_to_string(project.path().join("main.log")).unwrap();
    let line = log.lines().rfind(|l| l.starts_with("EXPOSITION-PROBE")).expect("probe report");
    eprintln!("{line}");
    let field = |name: &str| -> u32 {
        line.split_whitespace()
            .find_map(|kv| kv.strip_prefix(&format!("{name}=")))
            .and_then(|v| v.parse().ok())
            .unwrap()
    };
    assert!(field("glyph") > 700, "glyph nodes were walked");
    assert_eq!(field("glyph_line"), 0, "LuaTeX glyph nodes carry no SyncTeX line");
    assert!(field("glue_line") * 2 > field("glue"), "glue/kern nodes do carry lines");
    assert!(field("ligature") > 5, "ligature glyphs (with components) are visible to Lua");
    assert_eq!(field("column_api"), 0, "no input-column API in tex/status/token/node");
}
