//! Opaque stable identifiers.
//!
//! Identities are independent of paths and hashes: paths locate resources,
//! hashes identify versions. Every id is `<prefix><32 hex>` when minted here;
//! ids arriving over IPC or from a project database are validated (prefix,
//! length, characters) before use and never interpreted further.

use std::fmt;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Why an identifier was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid {kind} identifier")]
pub struct IdError {
    pub kind: &'static str,
}

const MAX_ID_LEN: usize = 64;

fn valid(raw: &str, prefix: &str) -> bool {
    raw.len() <= MAX_ID_LEN
        && raw.len() > prefix.len()
        && raw.starts_with(prefix)
        && raw[prefix.len()..].chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

macro_rules! id_type {
    ($(#[$doc:meta])* $name:ident, $prefix:literal, $kind:literal) => {
        $(#[$doc])*
        #[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
        #[ts(export, type = "string")]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            pub const PREFIX: &'static str = $prefix;

            /// Mint a fresh random identifier.
            pub fn mint() -> Self {
                Self(format!("{}{}", $prefix, uuid::Uuid::new_v4().simple()))
            }

            /// Validate an identifier from an untrusted source.
            pub fn parse(raw: &str) -> Result<Self, IdError> {
                if valid(raw, $prefix) { Ok(Self(raw.to_owned())) } else { Err(IdError { kind: $kind }) }
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = IdError;
            fn try_from(raw: String) -> Result<Self, IdError> {
                if valid(&raw, $prefix) { Ok(Self(raw)) } else { Err(IdError { kind: $kind }) }
            }
        }

        impl From<$name> for String {
            fn from(id: $name) -> String {
                id.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

id_type!(
    /// Stable identity of a project graph (kept in the graph database).
    ProjectId, "prj-", "project"
);
id_type!(
    /// Stable identity of an entity; survives renames and edits.
    EntityId, "ent-", "entity"
);
id_type!(
    /// Stable identity of a relation.
    RelationId, "rel-", "relation"
);
id_type!(
    /// Immutable evidence record.
    EvidenceId, "evd-", "evidence"
);
id_type!(
    /// A saved canvas view (presentation only).
    ViewId, "view-", "view"
);
id_type!(
    /// A change proposal awaiting application.
    ProposalId, "prop-", "proposal"
);
id_type!(
    /// A saved workflow definition (all its revisions share the id).
    WorkflowId, "wf-", "workflow"
);
id_type!(
    /// A cross-service operation in the app-owned operation journal.
    OperationId, "op-", "operation"
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minted_ids_parse_and_round_trip_through_json() {
        let id = EntityId::mint();
        assert!(id.as_str().starts_with("ent-"));
        assert_eq!(EntityId::parse(id.as_str()).unwrap(), id);
        let json = serde_json::to_string(&id).unwrap();
        let back: EntityId = serde_json::from_str(&json).unwrap();
        assert_eq!(back, id);
    }

    #[test]
    fn hostile_ids_are_refused() {
        for raw in [
            "",
            "ent-",
            "rel-abc",
            "ent-../x",
            "ent-a b",
            "ent-x'; DROP TABLE entities;--",
            &format!("ent-{}", "a".repeat(80)),
        ] {
            assert!(EntityId::parse(raw).is_err(), "{raw:?}");
            assert!(serde_json::from_str::<EntityId>(&format!("{raw:?}")).is_err(), "{raw:?}");
        }
    }
}
