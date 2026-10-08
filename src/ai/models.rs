use super::{PROVIDERS, provider_name};
use crate::OutputFormat;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{
    io::{self, IsTerminal},
    time::Duration,
};

const CATALOG_URL: &str = "https://models.dev/api.json";
const JEV_SOURCE: &str = "https://docs.typesafe.ai/models";

fn catalog_provider(provider: &str) -> Option<&str> {
    Some(match provider {
        "jev" | "ollama" | "baidu" => return None,
        "openai_resp" => "openai",
        "gemini" => "google",
        "open_router" => "openrouter",
        "fireworks" => "fireworks-ai",
        "together" => "togetherai",
        "mimo" => "xiaomi",
        "moonshot" => "moonshotai",
        "bigmodel" => "zhipuai",
        "aliyun" => "alibaba",
        "ollama_cloud" => "ollama-cloud",
        "vertex" => "google-vertex",
        "github_copilot" => "github-copilot",
        "opencode_go" => "opencode-go",
        "bedrock_api" => "amazon-bedrock",
        other => other,
    })
}

fn collect(catalog: &Value, provider: Option<&str>, search: Option<&str>) -> Result<Value> {
    let catalog = catalog
        .as_object()
        .context("invalid public model catalog: expected providers object")?;
    let mut providers = vec!["jev"];
    providers.extend(PROVIDERS.iter().map(|adapter| adapter.as_lower_str()));
    let mut models = Vec::new();
    let mut unavailable = Vec::new();
    for provider_id in providers {
        if provider.is_some_and(|selected| selected != provider_id) {
            continue;
        }
        if provider_id == "jev" {
            // Publicly documented aliases; no authenticated TypeSafe API call.
            for (id, name) in [
                ("jev-latest", "Jev latest stable"),
                ("jev-preview", "Jev preview"),
            ] {
                models.push(json!({"provider": provider_id, "id": id, "name": name,
                    "context": null, "structured_output": true, "source": JEV_SOURCE}));
            }
            continue;
        }
        let entries = catalog_provider(provider_id)
            .and_then(|key| catalog.get(key))
            .and_then(|entry| entry["models"].as_object());
        let Some(entries) = entries else {
            unavailable.push(provider_id);
            continue;
        };
        for (key, model) in entries {
            // Embedding, audio, and image-only models cannot review transactions.
            let Some(outputs) = model["modalities"]["output"].as_array() else {
                continue;
            };
            if !outputs.iter().any(|output| output == "text") {
                continue;
            }
            models.push(json!({"provider": provider_id,
                "id": model["id"].as_str().unwrap_or(key),
                "name": model["name"].as_str().unwrap_or(key),
                "context": model["limit"]["context"],
                "structured_output": model["structured_output"], "source": CATALOG_URL}));
        }
    }
    if let Some(search) = search {
        let search = search.to_lowercase();
        models.retain(|model| {
            ["id", "name"].iter().any(|field| {
                model[field]
                    .as_str()
                    .unwrap_or_default()
                    .to_lowercase()
                    .contains(&search)
            })
        });
    }
    models.sort_by(|a, b| {
        (a["provider"].as_str(), a["id"].as_str()).cmp(&(b["provider"].as_str(), b["id"].as_str()))
    });
    Ok(
        json!({"models": models, "unavailable_providers": unavailable,
        "notice": "Public catalog of text-output models; account access and review compatibility are not verified. Jev aliases are built in from its public docs."}),
    )
}

pub async fn run(
    provider: Option<String>,
    search: Option<String>,
    compact: bool,
    output: &OutputFormat,
) -> Result<()> {
    let provider = provider.as_deref().map(provider_name).transpose()?;
    if provider
        .as_deref()
        .is_some_and(|id| id != "jev" && catalog_provider(id).is_none())
    {
        if provider.as_deref() == Some("ollama") {
            bail!(
                "local Ollama models depend on what you have installed; there is no public catalog for this provider"
            );
        }
        bail!(
            "no public model catalog for {}",
            provider.as_deref().unwrap()
        );
    }
    let catalog = if provider.as_deref() == Some("jev") {
        json!({})
    } else {
        // Fresh client with no auth headers or saved settings, even if keys are configured.
        reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()?
            .get(CATALOG_URL)
            .send()
            .await
            .context("could not fetch public model catalog from models.dev")?
            .error_for_status()
            .context("public model catalog request failed")?
            .json::<Value>()
            .await
            .context("invalid JSON in public model catalog")?
    };
    let value = collect(&catalog, provider.as_deref(), search.as_deref())?;
    if let Some(provider) = provider
        && !value["unavailable_providers"]
            .as_array()
            .unwrap()
            .is_empty()
    {
        bail!("the public catalog has no model data for {provider}");
    }
    if compact {
        println!("{}", serde_json::to_string(&value)?);
    } else if *output == OutputFormat::Json
        || (*output == OutputFormat::Auto && !io::stdout().is_terminal())
    {
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        println!("{:<15} │ Model ID", "Provider");
        for model in value["models"].as_array().unwrap() {
            println!(
                "{:<15} │ {}",
                model["provider"].as_str().unwrap(),
                model["id"].as_str().unwrap()
            );
        }
        if value["models"].as_array().unwrap().is_empty() {
            println!("No matching models.");
        }
        println!("\n{}", value["notice"].as_str().unwrap());
        let unavailable = value["unavailable_providers"].as_array().unwrap();
        if !unavailable.is_empty() {
            println!(
                "No public catalog data: {}",
                unavailable
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        println!("Configure: lunchmoney ai set --provider PROVIDER --model MODEL_ID");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_provider_filters_text_and_preserves_model_ids() {
        let catalog = json!({"google": {"models": {
            "fallback": {"id": "gemini/path", "name": "Gemini Fast", "modalities": {"output": ["text"]}},
            "embedding": {"modalities": {"output": ["embedding"]}},
            "unknown": {}
        }}, "unsupported": {"models": {"other": {}}}});
        let value = collect(&catalog, Some("gemini"), Some("FAST")).unwrap();
        assert_eq!(value["models"].as_array().unwrap().len(), 1);
        assert_eq!(value["models"][0]["provider"], "gemini");
        assert_eq!(value["models"][0]["id"], "gemini/path");
        assert_eq!(value["models"][0]["structured_output"], Value::Null);
    }

    #[test]
    fn reports_missing_catalog_and_has_offline_jev_aliases() {
        assert_eq!(
            collect(&json!({}), Some("baidu"), None).unwrap()["unavailable_providers"],
            json!(["baidu"])
        );
        assert_eq!(
            collect(&json!({}), Some("jev"), None).unwrap()["models"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert!(collect(&json!([]), None, None).is_err());
    }
}
