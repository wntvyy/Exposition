import { describe, expect, it } from "vitest";

import { appEnvironment, REAL_CREDENTIAL_OPT_IN, scenarioUsesRealCredential } from "./app-env.mjs";

const fixed = { EXPOSITION_DATA_DIR: "C:/data" };

describe("e2e app environment", () => {
  it("never passes the real-credential opt-in on unless the launch asks for it", () => {
    const inherited = { PATH: "x", [REAL_CREDENTIAL_OPT_IN]: "1", exposition_use_real_credential: "1" };
    const env = appEnvironment(inherited, fixed, { env: { [REAL_CREDENTIAL_OPT_IN]: "1", OTHER: "y" } });
    expect(Object.keys(env).some((k) => k.toUpperCase() === REAL_CREDENTIAL_OPT_IN)).toBe(false);
    expect(env).toMatchObject({ PATH: "x", OTHER: "y", EXPOSITION_DATA_DIR: "C:/data" });
  });

  it("opts in only when asked", () => {
    expect(appEnvironment({}, fixed, { realCredential: true })[REAL_CREDENTIAL_OPT_IN]).toBe("1");
    expect(appEnvironment({}, fixed, { realCredential: false })[REAL_CREDENTIAL_OPT_IN]).toBeUndefined();
    expect(appEnvironment({}, fixed)[REAL_CREDENTIAL_OPT_IN]).toBeUndefined();
  });

  it("only live-* scenarios use the real credential", () => {
    expect(scenarioUsesRealCredential("live-agents")).toBe(true);
    for (const name of ["agents-harness", "review-sec-secrets", "m6-paper-project", "smoke"]) {
      expect(scenarioUsesRealCredential(name)).toBe(false);
    }
  });
});
