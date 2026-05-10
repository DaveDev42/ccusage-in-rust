use std::env;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

const LITELLM_URL: &str =
    "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let bundled_path = manifest_dir.join("data").join("litellm-bundled.json");

    println!("cargo:rerun-if-changed=data/litellm-bundled.json");
    println!("cargo:rerun-if-env-changed=CCUSAGE_RS_REFRESH_PRICING");
    println!("cargo:rerun-if-env-changed=CCUSAGE_RS_OFFLINE_BUILD");

    let offline = env::var("CCUSAGE_RS_OFFLINE_BUILD").is_ok();

    if env::var("CCUSAGE_RS_REFRESH_PRICING").is_ok() && !offline {
        if let Err(err) = refresh_pricing(&bundled_path) {
            println!("cargo:warning=Failed to refresh LiteLLM pricing: {err}");
        }
    }

    if !bundled_path.exists() {
        if offline {
            panic!(
                "data/litellm-bundled.json missing and CCUSAGE_RS_OFFLINE_BUILD is set; \
                 commit a snapshot or unset the variable."
            );
        }
        if let Err(err) = refresh_pricing(&bundled_path) {
            panic!(
                "data/litellm-bundled.json missing and could not fetch from LiteLLM: {err}. \
                 Run with network access or commit a snapshot."
            );
        }
    }
}

fn refresh_pricing(target: &PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;
    let body = client.get(LITELLM_URL).send()?.error_for_status()?.text()?;
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(target, body)?;
    Ok(())
}
