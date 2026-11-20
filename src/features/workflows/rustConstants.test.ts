// PG-10: the editor mirrors a few Rust limits and ids as display hints (Rust
// validates every draft again). This test pins each mirror to the Rust source
// it copies, so a change on either side fails here instead of misleading the
// form. Vitest runs from the repository root; the Rust files are read as text
// (the same approach as the KNOWN_VERDICTS and tokens tests).
import { describe, expect, it } from "vitest";

import { COMPOSITE_OPERATION } from "./contract";
import { FILE_INPUT_TYPES, OUTCOME_ACCEPTS } from "./model";
import { DEFAULT_LIMITS, MAX_FOR_EACH_VALUES, MAX_PARALLEL, MAX_UNTIL_ITERATIONS } from "./orchestration";
import { MAX_COMPOSITE_DEPTH } from "./useComposites";

async function rust(path: string): Promise<string> {
  const nodeFs = "node:fs";
  const { readFileSync } = (await import(/* @vite-ignore */ nodeFs)) as { readFileSync: (path: string, encoding: "utf8") => string };
  return readFileSync(path, "utf8");
}

function numberConst(source: string, name: string): number {
  const found = new RegExp(`pub const ${name}: \\w+ = (\\d+);`).exec(source);
  if (!found) throw new Error(`${name} was not found in the Rust source`);
  return Number(found[1]);
}

function textConst(source: string, name: string): string {
  const found = new RegExp(`pub const ${name}: &str = "([^"]+)";`).exec(source);
  if (!found) throw new Error(`${name} was not found in the Rust source`);
  return found[1]!;
}

/** `PascalCase` (a Rust variant) as its serde `camelCase` name. */
const camel = (variant: string) => variant.charAt(0).toLowerCase() + variant.slice(1);

describe("frontend mirrors of Rust workflow constants", () => {
  it("uses the limits validation enforces", async () => {
    const workflow = await rust("crates/exposition-graph/src/workflow.rs");
    const v2 = await rust("crates/exposition-graph/src/workflow/v2.rs");
    expect(MAX_PARALLEL).toBe(numberConst(workflow, "MAX_PARALLEL"));
    expect(MAX_FOR_EACH_VALUES).toBe(numberConst(v2, "MAX_FOR_EACH_VALUES"));
    expect(MAX_UNTIL_ITERATIONS).toBe(numberConst(v2, "MAX_UNTIL_ITERATIONS"));
    expect(MAX_COMPOSITE_DEPTH).toBe(numberConst(v2, "MAX_COMPOSITE_DEPTH"));
    expect(COMPOSITE_OPERATION).toBe(textConst(workflow, "COMPOSITE_OPERATION"));
  });

  it("describes the application-wide run limits the run service applies", async () => {
    const model = await rust("crates/exposition-workflow/src/model.rs");
    expect(numberConst(model, "MAX_PARALLEL")).toBe(MAX_PARALLEL);
    const block = /impl Default for WorkflowConcurrencyLimits \{[\s\S]*?Self \{([\s\S]*?)\}/.exec(model)?.[1] ?? "";
    const field = (name: string) => /(\d+)/.exec(new RegExp(`${name}: ([^,]+),`).exec(block)?.[1] ?? "")?.[1];
    expect(DEFAULT_LIMITS).toEqual({
      maxActiveRuns: numberConst(model, "MAX_ACTIVE_RUNS"),
      maxInFlightSteps: Number(field("max_in_flight_steps")),
      restricted: Number(field("restricted")),
      native: Number(field("native")),
      agent: Number(field("agent")),
      lean: Number(field("lean")),
    });
  });

  it("offers for each outcome kind exactly the port types validation accepts", async () => {
    const workflow = await rust("crates/exposition-graph/src/workflow.rs");
    const body = /pub fn accepts\(self\) -> &'static \[PortType\] \{([\s\S]*?)\n    \}/.exec(workflow)?.[1] ?? "";
    const fromRust: Record<string, string[]> = {};
    for (const arm of body.matchAll(/((?:OutcomeKind::\w+\s*\|?\s*)+)=> &\[([^\]]*)\]/g)) {
      const types = [...arm[2]!.matchAll(/PortType::(\w+)/g)].map((m) => camel(m[1]!));
      for (const kind of arm[1]!.matchAll(/OutcomeKind::(\w+)/g)) fromRust[camel(kind[1]!)] = types;
    }
    expect(Object.keys(fromRust).length).toBeGreaterThan(0);
    expect(OUTCOME_ACCEPTS).toEqual(fromRust);
  });

  it("offers as input types exactly the port types an external input may have", async () => {
    const registry = await rust("crates/exposition-graph/src/workflow/registry.rs");
    // `PortType::is_file_input`: what validation accepts (`workflow.inputType` otherwise).
    const body = /pub fn is_file_input\(self\) -> bool \{([\s\S]*?)\n    \}/.exec(registry)?.[1] ?? "";
    const types = [...body.matchAll(/PortType::(\w+)/g)].map((m) => camel(m[1]!));
    expect(types.length).toBeGreaterThan(0);
    expect([...FILE_INPUT_TYPES].sort()).toEqual([...types].sort());
  });

  it("names the restricted batch operations by their registered ids", async () => {
    const registry = await rust("crates/exposition-graph/src/workflow/registry.rs");
    const label = await rust("src/features/research/ExecutionLabel.tsx");
    for (const name of ["RESEARCH_PYTHON_BATCH", "RESEARCH_R_BATCH", "RESEARCH_JULIA_BATCH"]) {
      expect(label).toContain(`operation: "${textConst(registry, name)}"`);
    }
  });
});
