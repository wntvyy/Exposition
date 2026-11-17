//! Model catalogs: which models a connection offers, with what Exposition
//! knows about each one.
//!
//! Discovery uses the selected connection (OpenAI `GET /v1/models`; Anthropic
//! paginated `GET /v1/models`). The provider lists say little about
//! capabilities, so a small maintained table (`openai_rules`, `anthropic_rules`)
//! decides whether Exposition's tool-using agent supports a model: a model
//! that is listed but not known stays *unverified*, never silently eligible.
//! Unknown metadata is not proof of support, and catalog visibility does not
//! guarantee that every request succeeds.
//!
//! Maintaining the table: when a provider releases a model family, add a rule
//! with what its official documentation states (tools, image and PDF input,
//! effort levels, context window), the date checked, and a test.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::model::{Effort, ProviderKind};

/// Date the maintained capability rules were last checked against provider docs.
pub const METADATA_CHECKED: &str = "2026-11-17";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum Support {
    Yes,
    No,
    /// Exposition has no reliable information.
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ModelLifecycle {
    Active,
    /// Still served; shuts down on `date` when known.
    Deprecated {
        date: Option<String>,
    },
    /// The provider no longer lists or serves it.
    Retired,
    Unknown,
}

/// Can Exposition's agent (tools, streaming, history) run on this model?
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum AgentSupport {
    Supported,
    /// Listed by the provider, but Exposition has not verified it for agents.
    Unverified {
        reason: String,
    },
    Unsupported {
        reason: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ModelCapabilities {
    pub tools: Support,
    pub images: Support,
    pub pdf: Support,
    /// Effort levels this model and route accept (empty: no effort control).
    pub efforts: Vec<Effort>,
    pub context_tokens: Option<u64>,
    pub max_output_tokens: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum DescriptorSource {
    /// Listed by the provider's catalog for this connection.
    Catalog,
    /// Not listed now (saved choice, or the catalog is unavailable).
    Saved,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ModelDescriptor {
    pub provider: ProviderKind,
    /// Exact provider model ID sent in requests.
    pub id: String,
    pub display_name: String,
    pub lifecycle: ModelLifecycle,
    pub agent: AgentSupport,
    pub capabilities: ModelCapabilities,
    pub source: DescriptorSource,
    /// Creation time reported by the provider (ms since epoch).
    pub created_at_ms: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum CatalogState {
    /// Fetched for this connection within the freshness window.
    Fresh,
    /// From the cache: older than the freshness window, or a refresh failed.
    Stale,
    /// Nothing could be fetched (no credential, network, rejected key).
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ModelCatalog {
    pub provider: ProviderKind,
    pub state: CatalogState,
    /// When the listed models were fetched (ms since epoch).
    pub fetched_at_ms: Option<u64>,
    /// Safe-to-show reason when stale or unavailable.
    pub error: Option<String>,
    /// Sorted for display: supported first, then by the provider's order.
    pub models: Vec<ModelDescriptor>,
    /// The default the picker suggests for a new choice, with its reason.
    pub suggested: Option<String>,
    pub suggested_reason: Option<String>,
    pub metadata_checked: String,
}

fn caps(tools: Support, images: Support, pdf: Support, efforts: &[Effort]) -> ModelCapabilities {
    ModelCapabilities { tools, images, pdf, efforts: efforts.to_vec(), context_tokens: None, max_output_tokens: None }
}

const ALL_EFFORTS: &[Effort] = &[Effort::Low, Effort::Medium, Effort::High, Effort::Xhigh, Effort::Max];
const OPENAI_EARLIER_EFFORTS: &[Effort] = &[Effort::Low, Effort::Medium, Effort::High];

/// What Exposition knows about an OpenAI model ID (maintained rules; checked
/// 2026-11-17 against developers.openai.com "Models" and "File inputs").
pub fn openai_rules(id: &str) -> (AgentSupport, ModelCapabilities) {
    use Support::*;
    let id = id.to_ascii_lowercase();
    let non_text = [
        "embedding",
        "tts",
        "whisper",
        "dall-e",
        "moderation",
        "transcribe",
        "realtime",
        "gpt-live",
        "audio",
        "image",
        "search",
        "davinci",
        "babbage",
        "computer-use",
        "sora",
    ];
    if non_text.iter().any(|k| id.contains(k)) {
        return (
            AgentSupport::Unsupported {
                reason: "Not a text model that can call tools through the Responses API.".into(),
            },
            caps(No, Unknown, Unknown, &[]),
        );
    }
    let specialized = ["cyber", "rosalind"];
    if specialized.iter().any(|k| id.contains(k)) {
        return (
            AgentSupport::Unverified {
                reason: "A specialized model: its access and tool behavior are not documented for general agents."
                    .into(),
            },
            caps(Unknown, Unknown, Unknown, &[]),
        );
    }
    // GPT-6 family (Astra, Sol, Terra, Luna): functions, image input, PDF via
    // file inputs ("gpt-4o and later"), effort low…max (Terra/Luna also
    // "none", which Exposition does not offer).
    if id.starts_with("gpt-6") {
        return (AgentSupport::Supported, caps(Yes, Yes, Yes, ALL_EFFORTS));
    }
    // Earlier families still served to some accounts.
    if id.starts_with("gpt-5") || id.starts_with("o3") || id.starts_with("o4-mini") {
        return (AgentSupport::Supported, caps(Yes, Yes, Yes, OPENAI_EARLIER_EFFORTS));
    }
    if id.starts_with("gpt-4.1") || id.starts_with("gpt-4o") {
        return (AgentSupport::Supported, caps(Yes, Yes, Yes, &[]));
    }
    (
        AgentSupport::Unverified {
            reason:
                "Listed by OpenAI, but Exposition has not verified tool use, attachments, or effort for this model."
                    .into(),
        },
        caps(Unknown, Unknown, Unknown, &[]),
    )
}

/// What Exposition knows about an Anthropic model (maintained rules plus the
/// catalog's own capability fields when present).
pub fn anthropic_rules(id: &str) -> (AgentSupport, ModelCapabilities) {
    use Support::*;
    let id = id.to_ascii_lowercase();
    if !id.starts_with("claude-") {
        return (
            AgentSupport::Unverified { reason: "Not a Claude model line Exposition knows.".into() },
            caps(Unknown, Unknown, Unknown, &[]),
        );
    }
    // Claude models from the 4 line on: tools, vision, PDF, adaptive thinking
    // with effort (the request builder sends `output_config.effort`).
    let legacy = ["claude-3-", "claude-2", "claude-instant"].iter().any(|p| id.starts_with(p));
    if legacy {
        return (
            AgentSupport::Unverified {
                reason:
                    "Older Claude model: Exposition's requests (adaptive thinking, effort) are not verified with it."
                        .into(),
            },
            caps(Yes, Yes, Unknown, &[]),
        );
    }
    (AgentSupport::Supported, caps(Yes, Yes, Yes, ALL_EFFORTS))
}

fn ms_from_rfc3339(s: &str) -> Option<u64> {
    // YYYY-MM-DDTHH:MM:SS(.fff)(Z|±hh:mm); seconds precision is enough.
    let b = s.as_bytes();
    if b.len() < 19 {
        return None;
    }
    let num = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d, h, mi, se) = (num(0..4)?, num(5..7)?, num(8..10)?, num(11..13)?, num(14..16)?, num(17..19)?);
    // Days from civil (Howard Hinnant).
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let doy = (153 * (mo + if mo > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + h * 3600 + mi * 60 + se;
    u64::try_from(secs).ok().map(|s| s * 1000)
}

/// Models from an OpenAI API-key catalog (`{"object":"list","data":[…]}`).
pub fn parse_openai(body: &Value) -> Result<Vec<ModelDescriptor>, String> {
    let data = body.get("data").and_then(Value::as_array).ok_or("the model list has no data array")?;
    let mut out = Vec::new();
    for item in data {
        let Some(id) = item.get("id").and_then(Value::as_str) else { continue };
        let (agent, capabilities) = openai_rules(id);
        let lifecycle = match item.get("shutdown_date").and_then(Value::as_str) {
            Some(date) => ModelLifecycle::Deprecated { date: Some(date.to_owned()) },
            None => ModelLifecycle::Active,
        };
        out.push(ModelDescriptor {
            provider: ProviderKind::OpenAi,
            id: id.to_owned(),
            display_name: id.to_owned(),
            lifecycle,
            agent,
            capabilities,
            source: DescriptorSource::Catalog,
            created_at_ms: item.get("created").and_then(Value::as_u64).map(|s| s * 1000),
        });
    }
    Ok(out)
}

/// One page of an Anthropic catalog: models, and the cursor for the next page.
pub fn parse_anthropic_page(body: &Value) -> Result<(Vec<ModelDescriptor>, Option<String>), String> {
    let data = body.get("data").and_then(Value::as_array).ok_or("the model list has no data array")?;
    let mut out = Vec::new();
    for item in data {
        let Some(id) = item.get("id").and_then(Value::as_str) else { continue };
        let (mut agent, mut capabilities) = anthropic_rules(id);
        // Token limits (nullable).
        capabilities.context_tokens = item.get("max_input_tokens").and_then(Value::as_u64).filter(|n| *n > 0);
        capabilities.max_output_tokens = item.get("max_tokens").and_then(Value::as_u64).filter(|n| *n > 0);
        // The catalog's own capability map (nullable; keys may be added):
        // primary evidence when present, the maintained rules otherwise.
        if let Some(c) = item.get("capabilities").filter(|c| c.is_object()) {
            let supported = |v: Option<&Value>| v.and_then(|v| v.get("supported")).and_then(Value::as_bool);
            let support = |v: Option<&Value>| match supported(v) {
                Some(true) => Support::Yes,
                Some(false) => Support::No,
                None => Support::Unknown,
            };
            capabilities.images = support(c.get("image_input"));
            capabilities.pdf = support(c.get("pdf_input"));
            if let Some(effort) = c.get("effort") {
                capabilities.efforts = if supported(Some(effort)) == Some(true) {
                    [
                        ("low", Effort::Low),
                        ("medium", Effort::Medium),
                        ("high", Effort::High),
                        ("xhigh", Effort::Xhigh),
                        ("max", Effort::Max),
                    ]
                    .into_iter()
                    .filter(|(k, _)| supported(effort.get(*k)) == Some(true))
                    .map(|(_, e)| e)
                    .collect()
                } else {
                    Vec::new()
                };
            }
            // Exposition's requests use adaptive thinking with an effort level.
            let adaptive = c.get("thinking").and_then(|t| t.get("types")).and_then(|t| t.get("adaptive"));
            if supported(adaptive) == Some(false) {
                agent = AgentSupport::Unverified {
                    reason: "This model does not accept the adaptive thinking Exposition's agent requests use.".into(),
                };
            } else if supported(adaptive) == Some(true) && agent != AgentSupport::Supported {
                agent = AgentSupport::Supported;
            }
        }
        let lifecycle = match item.get("lifecycle").and_then(Value::as_str) {
            Some("deprecated") => ModelLifecycle::Deprecated {
                date: item.get("retires_at").and_then(Value::as_str).map(|d| d.chars().take(10).collect()),
            },
            Some("retired") => ModelLifecycle::Retired,
            Some("active") | None => ModelLifecycle::Active,
            Some(_) => ModelLifecycle::Unknown,
        };
        if lifecycle == ModelLifecycle::Retired {
            agent = AgentSupport::Unsupported { reason: "Retired: requests naming this model fail.".into() };
        }
        out.push(ModelDescriptor {
            provider: ProviderKind::Anthropic,
            id: id.to_owned(),
            display_name: item.get("display_name").and_then(Value::as_str).unwrap_or(id).to_owned(),
            lifecycle,
            agent,
            capabilities,
            source: DescriptorSource::Catalog,
            created_at_ms: item.get("created_at").and_then(Value::as_str).and_then(ms_from_rfc3339),
        });
    }
    let more = body.get("has_more").and_then(Value::as_bool).unwrap_or(false);
    let next = if more { body.get("last_id").and_then(Value::as_str).map(str::to_owned) } else { None };
    Ok((out, next))
}

/// A descriptor for a saved model ID the catalog does not (or cannot) list:
/// kept exactly, marked so the user can correct it, never substituted.
pub fn saved_descriptor(provider: ProviderKind, id: &str) -> ModelDescriptor {
    let (agent, capabilities) = match provider {
        ProviderKind::OpenAi => openai_rules(id),
        ProviderKind::Anthropic => anthropic_rules(id),
        ProviderKind::Scripted => {
            (AgentSupport::Supported, caps(Support::Yes, Support::Yes, Support::Yes, ALL_EFFORTS))
        }
    };
    ModelDescriptor {
        provider,
        id: id.to_owned(),
        display_name: id.to_owned(),
        lifecycle: ModelLifecycle::Unknown,
        agent,
        capabilities,
        source: DescriptorSource::Saved,
        created_at_ms: None,
    }
}

/// Order for display (supported first, then newest first, then ID) and the
/// suggested default: the saved choice when it is listed and supported;
/// otherwise the newest supported, active model. Never just the first row.
pub fn finish(
    provider: ProviderKind,
    mut models: Vec<ModelDescriptor>,
    saved: &[String],
) -> (Vec<ModelDescriptor>, Option<String>, Option<String>) {
    for id in saved {
        if !id.is_empty() && !models.iter().any(|m| &m.id == id) {
            models.push(saved_descriptor(provider, id));
        }
    }
    let rank = |m: &ModelDescriptor| match m.agent {
        AgentSupport::Supported => 0,
        AgentSupport::Unverified { .. } => 1,
        AgentSupport::Unsupported { .. } => 2,
    };
    models.sort_by(|a, b| {
        rank(a)
            .cmp(&rank(b))
            .then(b.created_at_ms.unwrap_or(0).cmp(&a.created_at_ms.unwrap_or(0)))
            .then(a.id.cmp(&b.id))
    });
    let usable = |m: &&ModelDescriptor| {
        m.agent == AgentSupport::Supported
            && m.source == DescriptorSource::Catalog
            && m.lifecycle == ModelLifecycle::Active
    };
    if let Some(first) = saved.first().filter(|id| models.iter().filter(usable).any(|m| &&m.id == id)) {
        return (models, Some(first.clone()), Some("Your saved choice.".into()));
    }
    let newest = models.iter().filter(usable).max_by_key(|m| m.created_at_ms.unwrap_or(0)).map(|m| m.id.clone());
    let reason =
        newest.as_ref().map(|_| "The newest model Exposition supports for agents on this connection.".to_owned());
    (models, newest, reason)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn openai_catalog_marks_support_and_never_promotes_unknown_models() {
        let body = json!({"object":"list","data":[
            {"id":"gpt-6.2-astra","object":"model","created":1_760_000_000,"owned_by":"system"},
            {"id":"text-embedding-3-large","object":"model","created":1,"owned_by":"system"},
            {"id":"ft:custom-thing","object":"model","created":2,"owned_by":"me","shutdown_date":"2027-01-01"},
            {"id":"gpt-6-cyber","object":"model","created":3,"owned_by":"system"},
            {"id":"gpt-image-2.6-flare","object":"model","created":4,"owned_by":"system"},
            {"id":"gpt-5.4","object":"model","created":5,"owned_by":"system"}
        ]});
        let models = parse_openai(&body).unwrap();
        assert_eq!(models[0].agent, AgentSupport::Supported);
        assert_eq!(models[0].capabilities.efforts, ALL_EFFORTS);
        assert!(matches!(models[1].agent, AgentSupport::Unsupported { .. }));
        assert!(matches!(models[2].agent, AgentSupport::Unverified { .. }));
        assert_eq!(models[2].lifecycle, ModelLifecycle::Deprecated { date: Some("2027-01-01".into()) });
        assert!(matches!(models[3].agent, AgentSupport::Unverified { .. }));
        assert!(matches!(models[4].agent, AgentSupport::Unsupported { .. }));
        assert_eq!(models[5].capabilities.efforts, OPENAI_EARLIER_EFFORTS);
        assert!(parse_openai(&json!({"models": []})).is_err());
    }

    #[test]
    fn anthropic_pages_and_nullable_fields() {
        let body = json!({"data":[
            {"type":"model","id":"claude-opus-5-5","display_name":"Claude Opus 5.5","created_at":"2026-09-01T00:00:00Z",
             "max_input_tokens": 1_000_000, "capabilities": null},
            {"type":"model","id":"claude-3-haiku-20240307","display_name":"Claude Haiku 3","created_at":"2024-03-07T00:00:00Z"},
            {"type":"model","id":"mystery-1","display_name":null,"created_at":"bad"}
        ], "has_more": true, "first_id": "claude-opus-5-5", "last_id": "mystery-1"});
        let (models, next) = parse_anthropic_page(&body).unwrap();
        assert_eq!(next.as_deref(), Some("mystery-1"));
        assert_eq!(models[0].display_name, "Claude Opus 5.5");
        assert_eq!(models[0].capabilities.context_tokens, Some(1_000_000));
        assert_eq!(models[0].created_at_ms, Some(1_788_220_800_000));
        assert!(matches!(models[1].agent, AgentSupport::Unverified { .. }));
        assert_eq!(models[2].display_name, "mystery-1");
        assert!(matches!(models[2].agent, AgentSupport::Unverified { .. }));
        let (_, none) = parse_anthropic_page(&json!({"data": [], "has_more": false, "last_id": "x"})).unwrap();
        assert_eq!(none, None);
    }

    #[test]
    fn anthropic_capability_map_is_primary_evidence() {
        let s = |b: bool| json!({"supported": b});
        let model = |adaptive: bool, xhigh: Value, lifecycle: &str| {
            json!({"data":[{"type":"model","id":"claude-x-1","display_name":"X","created_at":"2026-01-01T00:00:00Z",
              "lifecycle": lifecycle, "retires_at": "2027-03-01T00:00:00Z", "line": "unknown-new-line",
              "max_input_tokens": 200000, "max_tokens": 0,
              "capabilities": {"image_input": s(true), "pdf_input": s(false),
                "effort": {"supported": true, "low": s(true), "medium": s(true), "high": s(true), "max": s(false), "xhigh": xhigh},
                "thinking": {"supported": true, "types": {"adaptive": s(adaptive)}}}}],"has_more":false})
        };
        let (m, _) = parse_anthropic_page(&model(true, Value::Null, "active")).unwrap();
        assert_eq!(m[0].agent, AgentSupport::Supported);
        assert_eq!((m[0].capabilities.images, m[0].capabilities.pdf), (Support::Yes, Support::No));
        assert_eq!(m[0].capabilities.efforts, vec![Effort::Low, Effort::Medium, Effort::High]);
        assert_eq!((m[0].capabilities.context_tokens, m[0].capabilities.max_output_tokens), (Some(200000), None));
        let (m, _) = parse_anthropic_page(&model(false, s(true), "deprecated")).unwrap();
        assert!(matches!(m[0].agent, AgentSupport::Unverified { .. }));
        assert_eq!(m[0].lifecycle, ModelLifecycle::Deprecated { date: Some("2027-03-01".into()) });
        let (m, _) = parse_anthropic_page(&model(true, s(true), "retired")).unwrap();
        assert!(matches!(m[0].agent, AgentSupport::Unsupported { .. }));
    }

    #[test]
    fn saved_ids_are_kept_and_the_suggestion_is_explainable() {
        let listed = vec![
            ModelDescriptor {
                created_at_ms: Some(2),
                ..saved_descriptor(ProviderKind::Anthropic, "claude-sonnet-5-5")
            },
            ModelDescriptor { created_at_ms: Some(3), ..saved_descriptor(ProviderKind::Anthropic, "claude-opus-5-5") },
        ]
        .into_iter()
        .map(|m| ModelDescriptor { source: DescriptorSource::Catalog, lifecycle: ModelLifecycle::Active, ..m })
        .collect::<Vec<_>>();
        // A saved ID the catalog no longer lists stays, marked as saved.
        let (models, suggested, reason) = finish(ProviderKind::Anthropic, listed.clone(), &["claude-legacy-1".into()]);
        assert!(models.iter().any(|m| m.id == "claude-legacy-1" && m.source == DescriptorSource::Saved));
        assert_eq!(suggested.as_deref(), Some("claude-opus-5-5"));
        assert!(reason.unwrap().contains("newest"));
        // A listed saved choice is suggested as itself.
        let (_, suggested, reason) = finish(ProviderKind::Anthropic, listed, &["claude-sonnet-5-5".into()]);
        assert_eq!(suggested.as_deref(), Some("claude-sonnet-5-5"));
        assert_eq!(reason.as_deref(), Some("Your saved choice."));
    }
}
