// The environment of an app launched by the e2e harness.
//
// Credential isolation: a development build reads the user's real provider key
// only when `EXPOSITION_USE_REAL_CREDENTIAL=1` (otherwise it uses a namespaced
// development entry). The harness never passes that opt-in on, from its own
// environment or from a scenario's `env`, unless the launch explicitly asks
// for the real credential, which only `live-*` scenarios do (see run.mjs).
// Release builds always use the real entry; the harness cannot change that.

export const REAL_CREDENTIAL_OPT_IN = "EXPOSITION_USE_REAL_CREDENTIAL";

/**
 * Model-provider endpoint overrides (honored by debug builds only, loopback
 * addresses only). Unless a scenario points them at its own mock, or the
 * launch uses the real credential (live scenarios), they point at a closed
 * loopback port: a development key is never sent to a real provider (for
 * example when a dialog loads the model catalog), and nothing depends on the
 * network.
 */
export const PROVIDER_ENDPOINTS = ["EXPOSITION_DEV_ANTHROPIC_BASE_URL", "EXPOSITION_DEV_OPENAI_BASE_URL"];
export const CLOSED_LOOPBACK = "http://127.0.0.1:9";

/**
 * @param {Record<string, string | undefined>} baseEnv the harness's environment
 * @param {Record<string, string>} fixed per-launch variables (data dir, WebView2)
 * @param {{ env?: Record<string, string>, realCredential?: boolean }} options
 */
export function appEnvironment(baseEnv, fixed, options = {}) {
  const env = { ...baseEnv, ...fixed, ...options.env };
  for (const name of Object.keys(env)) {
    if (name.toUpperCase() === REAL_CREDENTIAL_OPT_IN) delete env[name];
  }
  if (options.realCredential === true) {
    env[REAL_CREDENTIAL_OPT_IN] = "1";
    // Live runs talk to the real providers unless the scenario says otherwise.
    for (const name of PROVIDER_ENDPOINTS) if (!options.env?.[name]) delete env[name];
  } else {
    for (const name of PROVIDER_ENDPOINTS) env[name] = options.env?.[name] ?? CLOSED_LOOPBACK;
  }
  return env;
}

/** Whether a scenario may use the real credential entry. */
export function scenarioUsesRealCredential(name) {
  return name.startsWith("live-");
}
