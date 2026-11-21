//! Review of perf WS-3 (`RecoveryStore::has_entry_for`) and WS-5 (the
//! relative-path ignore filter of the watcher callback): each must give the
//! answer of the code it stands in for, for inputs of every shape.

use std::fs;
use std::path::{MAIN_SEPARATOR_STR, Path};

use exposition_core::document::TextEncoding;
use exposition_core::path::WorkspacePath;
use exposition_workspace::recovery::{JOURNAL_VERSION, Journal, RecoveryStore, StoredEntry};
use exposition_workspace::tree::{is_ignored, is_ignored_relative};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }

    fn below(&mut self, n: usize) -> usize {
        self.next() as usize % n.max(1)
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

const PATHS: &[&str] = &[
    "a.txt",
    "b.txt",
    "dir/sub/deep ü.tex",
    "decoy.txt",
    "x",
    "chapters/one.typ",
    "A.txt",
    "dir/a.txt",
    "back/slash.txt",
];

fn valid(path: &str, version: u32, text: &str) -> Vec<u8> {
    serde_json::to_vec(&Journal {
        version,
        path: WorkspacePath::parse(path).unwrap(),
        base_hash: Some("abc".into()),
        revision: 7,
        encoding: TextEncoding::Utf8,
        saved_at: 1_700_000_000_000,
        text: text.to_owned(),
    })
    .unwrap()
}

/// A journal file of some shape: as written, damaged, foreign, or hostile.
fn journal_bytes(rng: &mut Rng) -> Vec<u8> {
    let path = *rng.pick(PATHS);
    let other = *rng.pick(PATHS);
    let text = match rng.below(5) {
        0 => String::new(),
        1 => format!("\"path\":\"{other}\" inside the text"),
        2 => "é😀".repeat(3_000 + rng.below(2_000)),
        3 => "x".repeat(8 * 1024 + rng.below(64)),
        _ => "plain text\n".to_owned(),
    };
    let good = valid(path, JOURNAL_VERSION, &text);
    match rng.below(22) {
        0 | 1 => good,
        // Truncated anywhere (within the head, at its end, inside a character).
        2 | 3 => good[..rng.below(good.len() + 1)].to_vec(),
        4 => good[..good.len().min(8 * 1024 - 2 + rng.below(5))].to_vec(),
        // One byte changed.
        5 | 6 => {
            let mut bytes = good;
            let window = if rng.below(2) == 0 { 60 } else { usize::MAX };
            let at = rng.below(bytes.len().min(window));
            bytes[at] = *rng.pick(&[b'"', b'\\', b' ', 0xFF, b'{', b',', b'0', b':']);
            bytes
        }
        // Other versions, other spellings of the version.
        7 => valid(path, *rng.pick(&[0, 2, 99, u32::MAX]), &text),
        8 => {
            let spelled = *rng.pick(&["1.0", "\"1\"", "-1", "01", "1e0", " 1", "1 ", "null", "true", "18446744073709551616"]);
            String::from_utf8(good).unwrap().replacen("\"version\":1", &format!("\"version\":{spelled}"), 1).into_bytes()
        }
        // Valid JSON in another layout.
        9 => serde_json::to_vec_pretty(&serde_json::from_slice::<serde_json::Value>(&good).unwrap()).unwrap(),
        10 => format!(
            r#"{{"text":{},"revision":2,"version":1,"encoding":"utf8","savedAt":1,"baseHash":null,"path":"{path}"}}"#,
            serde_json::to_string(&text).unwrap()
        )
        .into_bytes(),
        11 => format!(r#"{{"version":1, "path":"{path}","baseHash":null,"revision":2,"encoding":"utf8","savedAt":1,"text":"x"}}"#)
            .into_bytes(),
        12 => format!(r#"{{"version":1,"path" :"{path}","baseHash":null,"revision":2,"encoding":"utf8","savedAt":1,"text":"x"}}"#)
            .into_bytes(),
        // The path twice, or another field named path first.
        13 => format!(
            r#"{{"version":1,"path":"{path}","baseHash":null,"revision":2,"encoding":"utf8","savedAt":1,"text":"x","path":"{other}"}}"#
        )
        .into_bytes(),
        14 => format!(
            r#"{{"version":1,"path":"{path}","version":1,"baseHash":null,"revision":2,"encoding":"utf8","savedAt":1,"text":"x"}}"#
        )
        .into_bytes(),
        // Paths that are escaped, normalized on parsing, invalid, or not strings.
        15 => {
            let spelled = *rng.pick(&[
                r#""a\u002etxt""#,
                r#""dir\\a.txt""#,
                r#""dir//a.txt""#,
                r#""./a.txt""#,
                r#""../a.txt""#,
                r#""C:/a.txt""#,
                r#""/a.txt""#,
                r#""""#,
                r#""a.txt/""#,
                r#""con.txt""#,
                "null",
                "17",
                r#"["a.txt"]"#,
                r#""a.txt"#,
                "\"a\u{0}.txt\"",
                r#""a.txt\"#,
            ]);
            format!(r#"{{"version":1,"path":{spelled},"baseHash":null,"revision":2,"encoding":"utf8","savedAt":1,"text":"x"}}"#)
                .into_bytes()
        }
        // Bytes that are not UTF-8 inside the path.
        16 => {
            let mut bytes = br#"{"version":1,"path":"a"#.to_vec();
            bytes.extend_from_slice(&[0xFF, 0xC3]);
            bytes.extend_from_slice(br#".txt","baseHash":null,"revision":2,"encoding":"utf8","savedAt":1,"text":"x"}"#);
            bytes
        }
        // A path that ends near or beyond the end of the head.
        17 => {
            let long = format!("{}/a.txt", "p".repeat(8 * 1024 - 40 + rng.below(40)));
            format!(r#"{{"version":1,"path":"{long}","baseHash":null,"revision":2,"encoding":"utf8","savedAt":1,"text":"x"}}"#)
                .into_bytes()
        }
        18 => {
            let lead = "é".repeat(4 * 1024 - 20 + rng.below(12));
            format!(r#"{{"version":1,"path":"{lead}/a.txt","baseHash":null,"revision":2,"encoding":"utf8","savedAt":1,"text":"x"}}"#)
                .into_bytes()
        }
        // A byte-order mark, leading space, nothing, or no JSON.
        19 => [&[0xEF, 0xBB, 0xBF][..], &good].concat(),
        20 => [&b" \n"[..], &good].concat(),
        _ => rng.pick(&[&b""[..], b"{", b"not json", b"{\"version\":", b"{\"version\":1,\"path\":", b"[]", b"null"]).to_vec(),
    }
}

#[test]
fn review_pending_entry_lookup_never_disagrees_with_the_full_listing() {
    let mut rng = Rng(0x5EED);
    let candidates: Vec<WorkspacePath> = PATHS
        .iter()
        .copied()
        .chain(["dir/a.txt", "a/txt", "con.txt", "p/a.txt", "a\u{FFFD}.txt", "a\u{FFFD}\u{FFFD}.txt", "a.txt/x"])
        .filter_map(|p| WorkspacePath::parse(p).ok())
        .collect();
    for round in 0..400 {
        let dir = tempfile::tempdir().unwrap();
        let store = RecoveryStore::new(dir.path(), "k");
        fs::create_dir_all(store.dir()).unwrap();
        let mut written = Vec::new();
        for file in 0..1 + rng.below(4) {
            let bytes = journal_bytes(&mut rng);
            let prefix = if rng.below(6) == 0 { "bad" } else { "rec" };
            let name = format!("{prefix}-{:016x}-{}.json", rng.next(), 5 + file);
            fs::write(store.dir().join(&name), &bytes).unwrap();
            written.push((name, bytes));
        }
        // Files that are not entries: a live journal, a temporary file, a reason note.
        fs::write(store.dir().join("live-0123456789abcdef.json"), valid("decoy.txt", JOURNAL_VERSION, "live")).unwrap();
        fs::write(store.dir().join(".live-0123456789abcdef.json.exposition-save-1.tmp"), valid("decoy.txt", 1, "t"))
            .unwrap();
        let describe = || -> Vec<(String, String)> {
            written
                .iter()
                .map(|(n, b)| (n.clone(), String::from_utf8_lossy(&b[..b.len().min(160)]).into_owned()))
                .collect()
        };
        let quick_before: Vec<bool> = candidates.iter().map(|p| store.has_entry_for(p)).collect();
        let listed = store.entries();
        let full: Vec<bool> = candidates
            .iter()
            .map(|p| {
                listed.iter().any(|entry| match entry {
                    StoredEntry::Valid { journal, .. } => journal.path == *p,
                    StoredEntry::Unreadable { path, .. } => path.as_ref() == Some(p),
                })
            })
            .collect();
        // The listing quarantined what it could not read; the answer stays.
        let quick_after: Vec<bool> = candidates.iter().map(|p| store.has_entry_for(p)).collect();
        for (index, path) in candidates.iter().enumerate() {
            assert_eq!(
                quick_before[index],
                full[index],
                "round {round}: {path} — lookup {} but the listing {} (a `false` lookup with a `true` listing lets an exact save through)\n{:#?}",
                quick_before[index],
                full[index],
                describe()
            );
            assert_eq!(quick_after[index], full[index], "round {round}: {path} after quarantine\n{:#?}", describe());
        }
    }
}

const COMPONENTS: &[&str] = &[
    ".git",
    ".lake",
    "node_modules",
    "target",
    ".exposition",
    ".Exposition",
    ".EXPOSITION",
    "graph.sqlite3",
    "graph.sqlite3-wal",
    "Graph.SQLite3",
    "graph-backups",
    "Graph-Backups",
    "graph-backups2",
    "graph.damaged-1",
    "graph.damaged",
    "graph",
    "research",
    "a",
    "main.typ",
    ".main.typ.exposition-save-1a2b.tmp",
    ".exposition-save-",
    "x.exposition-save",
    "targets",
    ".gitignore",
    "TARGET",
    ".GIT",
    "é",
    "a b",
    "project.json",
];

fn compare(path: &str) {
    let Ok(parsed) = WorkspacePath::parse(path) else { return };
    let expected = is_ignored(&parsed);
    assert_eq!(is_ignored_relative(Path::new(path)), expected, "{path}");
    let native = path.replace('/', MAIN_SEPARATOR_STR);
    assert_eq!(is_ignored_relative(Path::new(&native)), expected, "{native}");
}

#[test]
fn review_relative_ignore_filter_agrees_with_the_workspace_filter_for_every_short_path() {
    for a in COMPONENTS {
        compare(a);
        for b in COMPONENTS {
            compare(&format!("{a}/{b}"));
            for c in COMPONENTS {
                compare(&format!("{a}/{b}/{c}"));
            }
        }
    }
    let mut rng = Rng(42);
    for _ in 0..40_000 {
        let depth = 4 + rng.below(4);
        let path: Vec<&str> = (0..depth).map(|_| *rng.pick(COMPONENTS)).collect();
        compare(&path.join("/"));
    }
    // What is not a plain relative path is never dropped by the callback.
    for odd in ["", "..", "../.git/x", "/.git/x", "./.git/x"] {
        if odd == "./.git/x" {
            // `.` is not a component of a path: this is `.git/x`.
            continue;
        }
        assert!(!is_ignored_relative(Path::new(odd)), "{odd:?}");
    }
}
