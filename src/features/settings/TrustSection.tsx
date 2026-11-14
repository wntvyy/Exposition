// Trusted folders: list (with grant time) and revoke. Granting is deliberately
// not offered here: it only happens in the trust prompt an action raises.
import { useEffect } from "react";
import { FolderLock, RefreshCw, ShieldOff, TriangleAlert } from "lucide-react";

import type { TrustRuleLocation } from "../../bindings/TrustRuleLocation";
import { events } from "../../lib/ipc";
import { Badge, Button, EmptyState, ErrorState, ListRow, LoadingState, StatusText } from "../../ui/components";
import { formatGrantedAt } from "./model";
import { SettingsGroup, SettingsPage } from "./parts";
import { loadTrust, revokeTrust, useSettings } from "./store";

function LocationList({ locations }: { locations: TrustRuleLocation[] }) {
  return (
    <ul className="exp-settings__locations">
      {locations.map((l) => (
        <li key={l.path}>
          <code className="ui-code exp-settings__path">{l.path}</code> <span className="ui-meta">{l.description}</span>
        </li>
      ))}
    </ul>
  );
}

export function TrustSection() {
  const trust = useSettings((s) => s.trust);
  const error = useSettings((s) => s.trustError);
  useEffect(() => {
    void loadTrust();
    // A folder trusted (or revoked) from any other view shows up here at once.
    let unlisten: (() => void) | null = null;
    let live = true;
    void events
      .trustChanged(() => void loadTrust())
      .then((un) => (live ? (unlisten = un) : un()))
      .catch(() => {});
    return () => {
      live = false;
      unlisten?.();
    };
  }, []);

  return (
    <SettingsPage
      title="Trusted folders"
      testId="settings-trust"
      description={
        <>
          Builds, kernels, and commands in a trusted folder run on this computer with your account&rsquo;s privileges.
          Exposition asks before trusting a folder, when an action needs it; opening a project never trusts it.
        </>
      }
      actions={
        <Button size="sm" variant="ghost" icon={RefreshCw} onPress={() => void loadTrust()}>
          Refresh
        </Button>
      }
    >
      {trust === null ? (
        error ? (
          <ErrorState
            title="Could not read trusted folders"
            details={error}
            actions={<Button onPress={() => void loadTrust()}>Retry</Button>}
          />
        ) : (
          <LoadingState title="Loading trusted folders…" />
        )
      ) : (
        <>
          <SettingsGroup title="Trusted">
            {trust.roots.length === 0 ? (
              <EmptyState size="compact" title="No trusted folders" />
            ) : (
              <div className="exp-settings__list" role="list" data-testid="trusted-folders">
                {trust.roots.map((root) => (
                  <ListRow
                    key={root.path}
                    role="listitem"
                    data-testid="trusted-folder"
                    data-path={root.path}
                    icon={<FolderLock size={14} aria-hidden />}
                    title={<span className="exp-settings__path" title={root.path}>
                        {root.path}
                      </span>}
                    codeTitle
                    meta={
                      <>
                        <span>{root.grantedAtMs === null ? "Trusted (date not recorded)" : `Trusted ${formatGrantedAt(root.grantedAtMs)}`}</span>
                        {!root.exists && <StatusText status="unavailable" label="Folder no longer exists" />}
                        {root.ignoredReason && (
                          <>
                            <Badge tone="warning" icon={TriangleAlert}>
                              Ignored: too broad
                            </Badge>
                            <span>{root.ignoredReason}</span>
                          </>
                        )}
                      </>
                    }
                    trailing={
                      <Button size="sm" icon={ShieldOff} aria-label={`Revoke trust for ${root.path}`} onPress={() => void revokeTrust(root)}>
                        Revoke…
                      </Button>
                    }
                  />
                ))}
              </div>
            )}
          </SettingsGroup>
          <SettingsGroup title="What can be trusted" testId="trust-rules" description={trust.rules.summary}>
            <div className="exp-settings__rules">
              <div>
                <h4 className="ui-group-label">Personal folders (and anything containing them)</h4>
                <LocationList locations={trust.rules.personal} />
              </div>
              <div>
                <h4 className="ui-group-label">System folders (and anything inside or containing them)</h4>
                <LocationList locations={trust.rules.system} />
              </div>
            </div>
          </SettingsGroup>
        </>
      )}
    </SettingsPage>
  );
}
