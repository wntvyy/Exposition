// Drive the app's native file dialogs (Windows IFileDialog) through UI
// Automation (native-dialog.ps1): start the driver, then do whatever opens the
// dialog in the app; the driver waits for the dialog of the app's process,
// types the path into it and presses its accept button (or Cancel).
import { execFile } from "node:child_process";
import { join } from "node:path";

const SCRIPT = join(import.meta.dirname, "native-dialog.ps1");

/**
 * @param {{ title: string, path?: string, cancel?: boolean, pid?: number, timeoutMs?: number }} options
 * @returns {Promise<Record<string, unknown>>} what the driver found and did
 */
export function driveNativeDialog({ title, path = "", cancel = false, pid = 0, timeoutMs = 20_000 }) {
  if (process.platform !== "win32") return Promise.reject(new Error("native dialogs are driven on Windows only"));
  const args = ["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File", SCRIPT, "-Title", title, "-TimeoutMs", String(timeoutMs)];
  if (pid) args.push("-ProcessId", String(pid));
  if (cancel) args.push("-Cancel");
  else args.push("-Path", path);
  return new Promise((resolve, reject) => {
    execFile("powershell.exe", args, { timeout: timeoutMs + 20_000, windowsHide: true }, (error, stdout, stderr) => {
      const line = String(stdout).trim().split(/\r?\n/).filter(Boolean).at(-1) ?? "";
      let result = null;
      try {
        result = JSON.parse(line);
      } catch {
        // Reported below with the raw output.
      }
      if (error || !result) {
        reject(new Error(`native dialog driver failed (${error?.message ?? "no result"}): ${stdout}${stderr}`));
        return;
      }
      resolve(result);
    });
  });
}

/**
 * Open a native dialog with `open()` (e.g. a click) while the driver waits for
 * it, and resolve with the driver's report.
 */
export async function withNativeDialog(options, open) {
  const driven = driveNativeDialog(options);
  // Let PowerShell load UI Automation before the dialog appears (it polls anyway).
  await new Promise((r) => setTimeout(r, 300));
  await open();
  return driven;
}
