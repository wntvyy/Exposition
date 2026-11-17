// Disposable git repositories for the impact tests (never the working repo).
import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";

const IDENTITY = ["-c", "user.name=impact-test", "-c", "user.email=impact-test@example.invalid", "-c", "commit.gpgsign=false", "-c", "core.autocrlf=false"];

export function tempRepo(files = {}, { branch = "main" } = {}) {
  const dir = mkdtempSync(join(tmpdir(), "xp-impact-"));
  const git = (...args) => execFileSync("git", [...IDENTITY, ...args], { cwd: dir, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] }).trim();
  const write = (path, text) => {
    mkdirSync(dirname(join(dir, path)), { recursive: true });
    writeFileSync(join(dir, path), text);
  };
  const remove = (path) => rmSync(join(dir, path), { force: true });
  git("init", "-q", "-b", branch);
  for (const [path, text] of Object.entries(files)) write(path, text);
  if (Object.keys(files).length) {
    git("add", "-A");
    git("commit", "-q", "-m", "base");
  }
  return { dir, git, write, remove, cleanup: () => rmSync(dir, { recursive: true, force: true }) };
}
