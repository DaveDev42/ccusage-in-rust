use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use anyhow::Result;
use serde::{Deserialize, Serialize};

const LITELLM_URL: &str =
    "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";
const CACHE_TTL: Duration = Duration::from_secs(24 * 3600);
const TIERED_THRESHOLD: u64 = 200_000;

const PROVIDER_PREFIXES: &[&str] = &[
    "anthropic/",
    "claude-3-5-",
    "claude-3-",
    "claude-",
    "openrouter/openai/",
];

const BUNDLED: &str = include_str!("../data/litellm-bundled.json");

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub(crate) struct ModelPricing {
    #[serde(default)]
    pub input_cost_per_token: Option<f64>,
    #[serde(default)]
    pub output_cost_per_token: Option<f64>,
    #[serde(default)]
    pub cache_creation_input_token_cost: Option<f64>,
    #[serde(default)]
    pub cache_read_input_token_cost: Option<f64>,
    #[serde(default)]
    pub input_cost_per_token_above_200k_tokens: Option<f64>,
    #[serde(default)]
    pub output_cost_per_token_above_200k_tokens: Option<f64>,
    #[serde(default)]
    pub cache_creation_input_token_cost_above_200k_tokens: Option<f64>,
    #[serde(default)]
    pub cache_read_input_token_cost_above_200k_tokens: Option<f64>,
    #[serde(default)]
    pub provider_specific_entry: Option<ProviderSpecific>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub(crate) struct ProviderSpecific {
    #[serde(default)]
    pub fast: Option<f64>,
}

pub(crate) struct PricingFetcher {
    map: HashMap<String, ModelPricing>,
}

impl PricingFetcher {
    pub(crate) fn load(offline: bool) -> Result<Self> {
        // Tier 1: live fetch (skipped if offline).
        if !offline {
            if let Ok(map) = fetch_live() {
                let _ = write_disk_cache(&map);
                return Ok(Self { map });
            }
        }

        // Tier 2: disk cache (24h TTL).
        if let Some(map) = read_disk_cache() {
            return Ok(Self { map });
        }

        // Tier 3: bundled fallback.
        Ok(Self {
            map: parse_pricing_json(BUNDLED).unwrap_or_default(),
        })
    }

    pub(crate) fn get_pricing(&self, model_name: &str) -> Option<&ModelPricing> {
        // Direct candidates: raw + prefixed.
        if let Some(p) = self.map.get(model_name) {
            return Some(p);
        }
        for prefix in PROVIDER_PREFIXES {
            let candidate = format!("{prefix}{model_name}");
            if let Some(p) = self.map.get(&candidate) {
                return Some(p);
            }
        }
        // Substring fallback (lowercase, both directions).
        let lower = model_name.to_lowercase();
        for (key, value) in &self.map {
            let comparison = key.to_lowercase();
            if comparison.contains(&lower) || lower.contains(&comparison) {
                return Some(value);
            }
        }
        None
    }

    pub(crate) fn calculate_cost(
        &self,
        model_name: &str,
        input_tokens: u64,
        output_tokens: u64,
        cache_creation_input_tokens: u64,
        cache_read_input_tokens: u64,
        speed_fast: bool,
    ) -> f64 {
        let Some(pricing) = self.get_pricing(model_name) else {
            return 0.0;
        };
        let base = calculate_from_pricing(
            pricing,
            input_tokens,
            output_tokens,
            cache_creation_input_tokens,
            cache_read_input_tokens,
        );
        if speed_fast {
            let multiplier = pricing
                .provider_specific_entry
                .as_ref()
                .and_then(|e| e.fast)
                .unwrap_or(1.0);
            base * multiplier
        } else {
            base
        }
    }
}

fn calculate_from_pricing(
    p: &ModelPricing,
    input: u64,
    output: u64,
    cache_creation: u64,
    cache_read: u64,
) -> f64 {
    tiered(
        input,
        p.input_cost_per_token,
        p.input_cost_per_token_above_200k_tokens,
    ) + tiered(
        output,
        p.output_cost_per_token,
        p.output_cost_per_token_above_200k_tokens,
    ) + tiered(
        cache_creation,
        p.cache_creation_input_token_cost,
        p.cache_creation_input_token_cost_above_200k_tokens,
    ) + tiered(
        cache_read,
        p.cache_read_input_token_cost,
        p.cache_read_input_token_cost_above_200k_tokens,
    )
}

fn tiered(total: u64, base: Option<f64>, above: Option<f64>) -> f64 {
    if total == 0 {
        return 0.0;
    }
    if total > TIERED_THRESHOLD {
        if let Some(tiered_price) = above {
            let below = TIERED_THRESHOLD;
            let over = total - TIERED_THRESHOLD;
            let mut cost = (over as f64) * tiered_price;
            if let Some(base_price) = base {
                cost += (below as f64) * base_price;
            }
            return cost;
        }
    }
    if let Some(base_price) = base {
        (total as f64) * base_price
    } else {
        0.0
    }
}

fn fetch_live() -> Result<HashMap<String, ModelPricing>> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;
    let body = client.get(LITELLM_URL).send()?.error_for_status()?.text()?;
    parse_pricing_json(&body)
}

fn parse_pricing_json(text: &str) -> Result<HashMap<String, ModelPricing>> {
    let raw: HashMap<String, serde_json::Value> = serde_json::from_str(text)?;
    let mut map = HashMap::with_capacity(raw.len());
    for (k, v) in raw {
        if let Ok(p) = serde_json::from_value::<ModelPricing>(v) {
            map.insert(k, p);
        }
    }
    Ok(map)
}

fn cache_path() -> Option<PathBuf> {
    let dir = dirs::cache_dir()?.join("ccusage-rs");
    let _ = fs::create_dir_all(&dir);
    Some(dir.join("litellm.json"))
}

fn read_disk_cache() -> Option<HashMap<String, ModelPricing>> {
    let path = cache_path()?;
    let meta = fs::metadata(&path).ok()?;
    let modified = meta.modified().ok()?;
    if SystemTime::now().duration_since(modified).ok()? > CACHE_TTL {
        return None;
    }
    let body = fs::read_to_string(&path).ok()?;
    parse_pricing_json(&body).ok()
}

fn write_disk_cache(map: &HashMap<String, ModelPricing>) -> Result<()> {
    let Some(path) = cache_path() else {
        return Ok(());
    };
    let body = serde_json::to_string(map)?;
    fs::write(path, body)?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CostMode {
    Auto,
    Calculate,
    Display,
}
