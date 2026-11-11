// The desktop app starts, the webview loads the bundled frontend, and a typed
// command round-trips through Rust.
import { join } from "node:path";

export default async function smoke({ launchApp, artifactsDir, assert }) {
  const app = await launchApp();
  try {
    const { page } = app;
    const info = await page.evaluate(() => window.__TAURI_INTERNALS__.invoke("app_info"));
    assert.equal(info.name, "Exposition");
    assert.equal(info.dataDir, app.dataDir, "app must use the isolated EXPOSITION_DATA_DIR");
    // A command that is not registered/permitted must be refused.
    const refused = await page
      .evaluate(() => window.__TAURI_INTERNALS__.invoke("definitely_not_a_command"))
      .then(() => false, () => true);
    assert.ok(refused, "unknown commands must be rejected");
    await page.screenshot({ path: join(artifactsDir, "smoke.png") });
  } finally {
    await app.close();
  }
}
