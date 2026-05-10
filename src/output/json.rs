//! JSON output structs that match ccusage shapes byte-for-byte.
//!
//! Field order matters: serde_json (with `preserve_order`) emits in declaration order,
//! and ccusage's `JSON.stringify` emits in insertion order — so the upstream JS literal
//! object order is replicated here verbatim.

use serde::{Serialize, Serializer};

/// Serialize an `f64` the way `JSON.stringify` does: integer-valued floats lose
/// their `.0` (e.g. `0.0` → `0`, `5.0` → `5`), but fractional values render normally.
fn js_number<S: Serializer>(v: &f64, s: S) -> Result<S::Ok, S::Error> {
    if v.is_finite() && v.fract() == 0.0 && v.abs() < 9_007_199_254_740_992.0 {
        s.serialize_i64(*v as i64)
    } else {
        s.serialize_f64(*v)
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct ModelBreakdown {
    #[serde(rename = "modelName")]
    pub model_name: String,
    #[serde(rename = "inputTokens")]
    pub input_tokens: u64,
    #[serde(rename = "outputTokens")]
    pub output_tokens: u64,
    #[serde(rename = "cacheCreationTokens")]
    pub cache_creation_tokens: u64,
    #[serde(rename = "cacheReadTokens")]
    pub cache_read_tokens: u64,
    #[serde(serialize_with = "js_number")]
    pub cost: f64,
}

#[derive(Debug, Serialize)]
pub(crate) struct DailyEntry {
    pub date: String,
    #[serde(rename = "inputTokens")]
    pub input_tokens: u64,
    #[serde(rename = "outputTokens")]
    pub output_tokens: u64,
    #[serde(rename = "cacheCreationTokens")]
    pub cache_creation_tokens: u64,
    #[serde(rename = "cacheReadTokens")]
    pub cache_read_tokens: u64,
    #[serde(rename = "totalTokens")]
    pub total_tokens: u64,
    #[serde(rename = "totalCost", serialize_with = "js_number")]
    pub total_cost: f64,
    #[serde(rename = "modelsUsed")]
    pub models_used: Vec<String>,
    #[serde(rename = "modelBreakdowns")]
    pub model_breakdowns: Vec<ModelBreakdown>,
}

#[derive(Debug, Serialize)]
pub(crate) struct MonthlyEntry {
    pub month: String,
    #[serde(rename = "inputTokens")]
    pub input_tokens: u64,
    #[serde(rename = "outputTokens")]
    pub output_tokens: u64,
    #[serde(rename = "cacheCreationTokens")]
    pub cache_creation_tokens: u64,
    #[serde(rename = "cacheReadTokens")]
    pub cache_read_tokens: u64,
    #[serde(rename = "totalTokens")]
    pub total_tokens: u64,
    #[serde(rename = "totalCost", serialize_with = "js_number")]
    pub total_cost: f64,
    #[serde(rename = "modelsUsed")]
    pub models_used: Vec<String>,
    #[serde(rename = "modelBreakdowns")]
    pub model_breakdowns: Vec<ModelBreakdown>,
}

#[derive(Debug, Serialize)]
pub(crate) struct SessionEntry {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(rename = "inputTokens")]
    pub input_tokens: u64,
    #[serde(rename = "outputTokens")]
    pub output_tokens: u64,
    #[serde(rename = "cacheCreationTokens")]
    pub cache_creation_tokens: u64,
    #[serde(rename = "cacheReadTokens")]
    pub cache_read_tokens: u64,
    #[serde(rename = "totalTokens")]
    pub total_tokens: u64,
    #[serde(rename = "totalCost", serialize_with = "js_number")]
    pub total_cost: f64,
    #[serde(rename = "lastActivity")]
    pub last_activity: String,
    #[serde(rename = "modelsUsed")]
    pub models_used: Vec<String>,
    #[serde(rename = "modelBreakdowns")]
    pub model_breakdowns: Vec<ModelBreakdown>,
    #[serde(rename = "projectPath")]
    pub project_path: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct Totals {
    #[serde(rename = "inputTokens")]
    pub input_tokens: u64,
    #[serde(rename = "outputTokens")]
    pub output_tokens: u64,
    #[serde(rename = "cacheCreationTokens")]
    pub cache_creation_tokens: u64,
    #[serde(rename = "cacheReadTokens")]
    pub cache_read_tokens: u64,
    #[serde(rename = "totalCost", serialize_with = "js_number")]
    pub total_cost: f64,
    #[serde(rename = "totalTokens")]
    pub total_tokens: u64,
}

#[derive(Debug, Serialize)]
pub(crate) struct DailyOutput {
    pub daily: Vec<DailyEntry>,
    pub totals: Totals,
}

#[derive(Debug, Serialize)]
pub(crate) struct MonthlyOutput {
    pub monthly: Vec<MonthlyEntry>,
    pub totals: Totals,
}

#[derive(Debug, Serialize)]
pub(crate) struct SessionOutput {
    pub sessions: Vec<SessionEntry>,
    pub totals: Totals,
}

#[derive(Debug, Serialize)]
pub(crate) struct TokenCounts {
    #[serde(rename = "inputTokens")]
    pub input_tokens: u64,
    #[serde(rename = "outputTokens")]
    pub output_tokens: u64,
    #[serde(rename = "cacheCreationInputTokens")]
    pub cache_creation_input_tokens: u64,
    #[serde(rename = "cacheReadInputTokens")]
    pub cache_read_input_tokens: u64,
}

#[derive(Debug, Serialize)]
pub(crate) struct BurnRate {
    #[serde(rename = "tokensPerMinute", serialize_with = "js_number")]
    pub tokens_per_minute: f64,
    #[serde(rename = "tokensPerMinuteForIndicator", serialize_with = "js_number")]
    pub tokens_per_minute_for_indicator: f64,
    #[serde(rename = "costPerHour", serialize_with = "js_number")]
    pub cost_per_hour: f64,
}

#[derive(Debug, Serialize)]
pub(crate) struct Projection {
    #[serde(rename = "totalTokens")]
    pub total_tokens: u64,
    #[serde(rename = "totalCost", serialize_with = "js_number")]
    pub total_cost: f64,
    #[serde(rename = "remainingMinutes")]
    pub remaining_minutes: u64,
}

#[derive(Debug, Serialize)]
pub(crate) struct BlockEntry {
    pub id: String,
    #[serde(rename = "startTime")]
    pub start_time: String,
    #[serde(rename = "endTime")]
    pub end_time: String,
    #[serde(rename = "actualEndTime")]
    pub actual_end_time: Option<String>,
    #[serde(rename = "isActive")]
    pub is_active: bool,
    #[serde(rename = "isGap")]
    pub is_gap: bool,
    pub entries: usize,
    #[serde(rename = "tokenCounts")]
    pub token_counts: TokenCounts,
    #[serde(rename = "totalTokens")]
    pub total_tokens: u64,
    #[serde(rename = "costUSD", serialize_with = "js_number")]
    pub cost_usd: f64,
    pub models: Vec<String>,
    #[serde(rename = "burnRate")]
    pub burn_rate: Option<BurnRate>,
    pub projection: Option<Projection>,
}

#[derive(Debug, Serialize)]
pub(crate) struct BlocksOutput {
    pub blocks: Vec<BlockEntry>,
}
