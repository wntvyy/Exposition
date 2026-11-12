// Accurate execution labels for research sessions.
//
// Python/R/Julia kernels run natively with the user's account privileges.
// The research space and per-owner sessions organize work; they do not
// contain it. The restricted, research-scoped Python batch adapter (container
// with declared inputs only) is not part of this release: it is shown as not
// available, and nothing ever falls back to it or from it silently.
import { ShieldAlert } from "lucide-react";

import { Banner, StatusPill } from "../../ui/components";

export const TRUSTED_NATIVE_LABEL = "Trusted native — runs with your account's privileges";
export const RESTRICTED_UNAVAILABLE =
  "Restricted research execution (a container that sees only declared inputs) is not available yet; research sessions run natively.";

/** Compact label for session toolbars (the full sentence is its accessible name). */
export function ExecutionLabel() {
  const full = `${TRUSTED_NATIVE_LABEL}. The research folder and session separate your work but are not a sandbox. ${RESTRICTED_UNAVAILABLE}`;
  return (
    <span
      className="exp-research__mode"
      data-testid="research-execution-mode"
      data-mode="trustedNative"
      title={full}
      aria-label={full}
      role="note"
    >
      <ShieldAlert size={14} aria-hidden />
      Trusted native — your account&rsquo;s privileges
    </span>
  );
}

/** Fuller explanation for the Research page. */
export function ExecutionModes() {
  return (
    <div className="exp-research__modes" data-testid="research-execution-modes">
      <div className="exp-research__mode-row" data-mode="trustedNative">
        <StatusPill size="sm" status="warning" label="Trusted native" />
        <span>
          Python, R, and Julia kernels run with your account&rsquo;s privileges. The research space and session keep work
          organized; they do not contain it.
        </span>
      </div>
      <div className="exp-research__mode-row" data-mode="restricted" data-available="false">
        <StatusPill size="sm" status="unavailable" label="Restricted research" />
        <span>Not available yet. Runs are never moved to or from a sandbox silently.</span>
      </div>
    </div>
  );
}

export function TrustedNativeBanner() {
  return (
    <Banner tone="warning" title={TRUSTED_NATIVE_LABEL}>
      The session and research folder organize work; they are not a sandbox. {RESTRICTED_UNAVAILABLE}
    </Banner>
  );
}
