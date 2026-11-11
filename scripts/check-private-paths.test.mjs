import { describe, expect, it } from "vitest";

import { findPrivatePaths } from "./check-private-paths.mjs";

describe("findPrivatePaths", () => {
  it("flags agent instructions, planning documents, and credentials at any depth", () => {
    const paths = [
      "CLAUDE.md",
      "src/CLAUDE.md",
      "AGENTS.local.md",
      ".claude/settings.json",
      "nested/.agents/x.md",
      ".exposition-dev/MASTER.md",
      ".env",
      ".env.production",
      "config\\.claude\\x.json",
    ];
    expect(findPrivatePaths(paths)).toEqual(paths);
  });

  it("allows ordinary source and the example environment file", () => {
    const paths = ["src/App.tsx", ".env.example", "docs/claude-notes.md", "crates/exposition-core/src/lib.rs"];
    expect(findPrivatePaths(paths)).toEqual([]);
  });
});
