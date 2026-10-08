pub(crate) mod chat;
pub(crate) mod client;
pub(crate) mod history;
mod markdown;
mod models;
pub(crate) mod research;
pub(crate) mod terminal;

use crate::OutputFormat;
use anyhow::{Context, Result, bail};
use genai::adapter::AdapterKind;
use serde_json::{Value, json};
use std::{
    env,
    fs::{self, OpenOptions},
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
};

#[derive(Debug, clap::Subcommand)]
pub enum AiCommand {
    /// Chat about your account and use tools to perform Lunch Money API actions.
    Chat(Box<chat::ChatArgs>),
    /// List supported AI providers, credentials, and review capabilities.
    #[command(alias = "list-providers")]
    Providers,
    /// Browse public model IDs without API keys (advertised availability only).
    Models {
        /// Limit results to a provider; defaults to all supported catalog providers.
        #[arg(long, value_parser = provider_parser(), ignore_case = true)]
        provider: Option<String>,
        /// Case-insensitive match against model ID or display name.
        #[arg(long)]
        search: Option<String>,
    },
    /// Save provider, model, or API base URL defaults. Updates only supplied settings.
    #[command(alias = "configure")]
    Set {
        #[arg(long, value_parser = provider_parser(), ignore_case = true)]
        provider: Option<String>,
        #[arg(long)]
        model: Option<String>,
        #[arg(
            long = "ai-base-url",
            id = "ai_base_url",
            conflicts_with = "clear_base_url"
        )]
        base_url: Option<String>,
        /// Return to the provider's standard API URL.
        #[arg(long)]
        clear_base_url: bool,
    },
    /// Securely prompt for and save the selected provider's API key.
    #[command(alias = "token")]
    Login {
        /// Provider to configure, or the saved default provider.
        #[arg(long, value_parser = provider_parser(), ignore_case = true)]
        provider: Option<String>,
        /// Prefer the hidden prompt or LUNCH_MONEY_AI_API_KEY to shell history.
        #[arg(long, env = "LUNCH_MONEY_AI_API_KEY", hide_env_values = true)]
        api_key: Option<String>,
    },
    /// Show saved settings and token presence without displaying secrets.
    #[command(alias = "show")]
    Status,
    /// Remove a provider's saved API key, retaining its model and other settings.
    Logout {
        #[arg(long, value_parser = provider_parser(), ignore_case = true)]
        provider: Option<String>,
    },
}

const PROVIDERS: &[AdapterKind] = &[
    AdapterKind::Anthropic,
    AdapterKind::OpenAI,
    AdapterKind::OpenAIResp,
    AdapterKind::Gemini,
    AdapterKind::Ollama,
    AdapterKind::OpenRouter,
    AdapterKind::Groq,
    AdapterKind::DeepSeek,
    AdapterKind::Xai,
    AdapterKind::Fireworks,
    AdapterKind::Together,
    AdapterKind::Cohere,
    AdapterKind::Aihubmix,
    AdapterKind::Mimo,
    AdapterKind::Moonshot,
    AdapterKind::Nebius,
    AdapterKind::Zai,
    AdapterKind::BigModel,
    AdapterKind::Aliyun,
    AdapterKind::Baidu,
    AdapterKind::OllamaCloud,
    AdapterKind::Vertex,
    AdapterKind::GithubCopilot,
    AdapterKind::OpenCodeGo,
    AdapterKind::BedrockApi,
    AdapterKind::MiniMax,
];

pub fn provider_parser() -> clap::builder::PossibleValuesParser {
    let mut values: Vec<_> = PROVIDERS
        .iter()
        .map(|adapter| {
            let name = adapter.as_lower_str();
            let value = clap::builder::PossibleValue::new(name);
            // Keep canonical spellings discoverable in help and shell completion.
            match name {
                "open_router" => value.alias("openrouter").alias("open-router"),
                "openai_resp" => value.alias("openairesp").alias("openai-resp"),
                _ => value,
            }
        })
        .collect();
    values.insert(0, clap::builder::PossibleValue::new("jev"));
    clap::builder::PossibleValuesParser::new(values)
}

fn provider_catalog() -> Value {
    let mut providers = vec![json!({
        "provider": "jev", "key_env": "TYPESAFE_API_KEY", "category_suggestions": true,
        "name_suggestions": "Choose names from transaction data", "generates_names": false,
        "default_model": "jev-latest"
    })];
    providers.extend(PROVIDERS.iter().map(|adapter| json!({
        "provider": adapter.as_lower_str(), "key_env": adapter.default_key_env_name(),
        "category_suggestions": true, "name_suggestions": "Generate improved names", "generates_names": true,
        "default_model": null
    })));
    json!({"providers": providers})
}

fn print_providers(compact: bool, output: &OutputFormat) -> Result<()> {
    let value = provider_catalog();
    if compact {
        println!("{}", serde_json::to_string(&value)?);
    } else if *output == OutputFormat::Json
        || (*output == OutputFormat::Auto && !io::stdout().is_terminal())
    {
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        println!(
            "{:<15} │ {:<26} │ Name suggestions",
            "Provider", "API key environment"
        );
        println!(
            "{}─┼─{}─┼─{}",
            "─".repeat(15),
            "─".repeat(26),
            "─".repeat(23)
        );
        for provider in value["providers"].as_array().unwrap() {
            println!(
                "{:<15} │ {:<26} │ {}",
                provider["provider"].as_str().unwrap(),
                provider["key_env"]
                    .as_str()
                    .unwrap_or("None (local server)"),
                if provider["generates_names"] == true {
                    "Generate improved names"
                } else {
                    "Choose existing names"
                }
            );
        }
        println!(
            "\nAll providers support category suggestions. Model capabilities and access vary."
        );
        println!("Configure: lunchmoney ai set --provider PROVIDER --model MODEL");
        println!("Save a token: lunchmoney ai login (hidden input; local Ollama needs no token)");
    }
    Ok(())
}

pub struct Settings {
    pub provider: String,
    pub model: String,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
}

pub fn provider_name(name: &str) -> Result<String> {
    let name = name.trim().to_ascii_lowercase().replace('-', "_");
    let name = match name.as_str() {
        "openrouter" => "open_router",
        "openairesp" => "openai_resp",
        other => other,
    };
    if name == "jev" || AdapterKind::from_lower_str(name).is_some() {
        return Ok(name.into());
    }
    bail!("unknown AI provider {name:?}; run `lunchmoney ai providers` to see supported providers")
}

pub fn validate_base_url(base_url: &str) -> Result<()> {
    let parsed = url::Url::parse(base_url).context("invalid AI base URL")?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        bail!("AI base URL must be HTTP(S), without credentials, query, or fragment");
    }
    Ok(())
}

fn path() -> Result<PathBuf> {
    Ok(crate::token_file()?.with_file_name("ai.json"))
}

fn load_at(path: &Path) -> Result<Value> {
    match fs::read_to_string(path) {
        Ok(contents) => {
            let value: Value =
                serde_json::from_str(&contents).context("invalid saved AI configuration")?;
            if !value.is_object() || value.get("providers").is_some_and(|v| !v.is_object()) {
                bail!("invalid saved AI configuration structure");
            }
            Ok(value)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(json!({"providers": {}})),
        Err(error) => Err(error).context("could not read saved AI configuration"),
    }
}

fn save_at(path: &Path, value: &Value) -> Result<()> {
    let directory = path
        .parent()
        .context("AI configuration path has no parent")?;
    fs::create_dir_all(directory)?;
    crate::restrict_directory_permissions(directory)?;
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary)
        .context("could not create private AI configuration file")?;
    let result = (|| -> Result<()> {
        writeln!(file, "{}", serde_json::to_string_pretty(value)?)?;
        file.sync_all()?;
        crate::restrict_file_permissions(&temporary)?;
        fs::rename(&temporary, path).context("could not save AI configuration")?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

fn selected_provider(config: &Value, provider: Option<&str>) -> Result<String> {
    provider_name(provider.or_else(|| config["provider"].as_str()).context(
        "no AI provider configured; run `lunchmoney ai set --provider PROVIDER --model MODEL`",
    )?)
}

fn model_for(provider: &str, model: &str) -> Result<String> {
    let model = model.trim();
    if model.is_empty() {
        bail!("AI model must not be empty");
    }
    if let Some((namespace, name)) = model.split_once("::") {
        if provider_name(namespace)? != provider {
            bail!("model provider does not match selected AI provider {provider}");
        }
        if name.trim().is_empty() {
            bail!("AI model must not be empty");
        }
        Ok(format!("{provider}::{name}"))
    } else {
        Ok(format!("{provider}::{model}"))
    }
}

fn resolve_from(
    config: &Value,
    provider: Option<&str>,
    model: Option<&str>,
    api_key: Option<&str>,
    base_url: Option<&str>,
) -> Result<Settings> {
    let namespace = model.and_then(|m| m.split_once("::").map(|(p, _)| p));
    let inferred;
    let provider = if provider.is_some() || namespace.is_some() || config["provider"].is_string() {
        selected_provider(config, provider.or(namespace))?
    } else if let Some(model) = model {
        inferred = genai::Client::default()
            .default_model(model)?
            .adapter_kind
            .as_lower_str()
            .to_owned();
        inferred
    } else {
        bail!("no AI configuration; run `lunchmoney ai set --provider PROVIDER --model MODEL`");
    };
    let profile = &config["providers"][&provider];
    let model = model.or_else(|| profile["model"].as_str()).or(if provider == "jev" { Some("jev-latest") } else { None })
        .context("no model configured for this provider; run `lunchmoney ai set --model MODEL` or pass --model")?;
    let model = model_for(&provider, model)?;
    let api_key = api_key
        .or_else(|| profile["api_key"].as_str())
        .map(str::to_owned);
    if api_key.as_deref().is_some_and(|key| key.trim().is_empty()) {
        bail!("AI API key must not be empty");
    }
    let base_url = base_url
        .or_else(|| profile["base_url"].as_str())
        .map(str::to_owned);
    if let Some(base_url) = &base_url {
        validate_base_url(base_url)?;
    }
    Ok(Settings {
        provider,
        model,
        api_key,
        base_url,
    })
}

pub fn resolve(
    provider: Option<&str>,
    model: Option<&str>,
    api_key: Option<&str>,
    base_url: Option<&str>,
) -> Result<Settings> {
    let config = load_at(&path()?)?;
    let mut settings = resolve_from(&config, provider, model, api_key, base_url)?;
    // Native provider environment keys override saved keys, like command-line/env overrides.
    if api_key.is_none() {
        let native_env = if settings.provider == "jev" {
            Some("TYPESAFE_API_KEY")
        } else {
            AdapterKind::from_lower_str(&settings.provider)
                .and_then(|adapter| adapter.default_key_env_name())
        };
        if let Some(key) = native_env.and_then(|name| env::var(name).ok()) {
            if key.trim().is_empty() {
                bail!("AI API key environment variable is empty");
            }
            settings.api_key = Some(key);
        }
    }
    Ok(settings)
}

pub async fn run(command: AiCommand, compact: bool, output: OutputFormat) -> Result<()> {
    if matches!(command, AiCommand::Providers) {
        return print_providers(compact, &output);
    }
    if let AiCommand::Models { provider, search } = command {
        return models::run(provider, search, compact, &output).await;
    }
    let path = path()?;
    let mut config = load_at(&path)?;
    match command {
        AiCommand::Providers | AiCommand::Models { .. } | AiCommand::Chat(_) => unreachable!(),
        AiCommand::Set {
            provider,
            model,
            base_url,
            clear_base_url,
        } => {
            if provider.is_none() && model.is_none() && base_url.is_none() && !clear_base_url {
                bail!("provide --provider, --model, --ai-base-url, or --clear-base-url");
            }
            let namespace = model
                .as_deref()
                .and_then(|m| m.split_once("::").map(|(p, _)| p));
            let provider = selected_provider(&config, provider.as_deref().or(namespace))?;
            if let Some(model) = &model {
                model_for(&provider, model)?;
            }
            if let Some(base_url) = &base_url {
                validate_base_url(base_url)?;
            }
            if config["providers"].is_null() {
                config["providers"] = json!({});
            }
            if !config["providers"][&provider].is_object() {
                config["providers"][&provider] = json!({});
            }
            let profile = &mut config["providers"][&provider];
            if let Some(model) = model {
                profile["model"] = model.into();
            }
            if let Some(base_url) = base_url {
                profile["base_url"] = base_url.into();
            }
            if clear_base_url {
                profile.as_object_mut().unwrap().remove("base_url");
            }
            config["provider"] = provider.clone().into();
            save_at(&path, &config)?;
            println!("AI settings saved for {provider}.");
        }
        AiCommand::Login { provider, api_key } => {
            let provider = selected_provider(&config, provider.as_deref())?;
            let key = if let Some(key) = api_key {
                key
            } else {
                if !io::stdin().is_terminal() {
                    bail!(
                        "AI login requires a terminal; set LUNCH_MONEY_AI_API_KEY for non-interactive login"
                    );
                }
                crate::prompt_for_secret("AI API key")?
            };
            if key.trim().is_empty() {
                bail!("AI API key must not be empty");
            }
            if config["providers"].is_null() {
                config["providers"] = json!({});
            }
            if !config["providers"][&provider].is_object() {
                config["providers"][&provider] = json!({});
            }
            config["providers"][&provider]["api_key"] = key.trim().into();
            if config["provider"].is_null() {
                config["provider"] = provider.clone().into();
            }
            save_at(&path, &config)?;
            println!("AI token saved for {provider}.");
        }
        AiCommand::Logout { provider } => {
            let provider = selected_provider(&config, provider.as_deref())?;
            let removed = config["providers"][&provider]
                .as_object_mut()
                .and_then(|profile| profile.remove("api_key"))
                .is_some();
            if removed {
                save_at(&path, &config)?;
            }
            println!(
                "{}",
                if removed {
                    format!("Saved AI token removed for {provider}.")
                } else {
                    format!("No saved AI token for {provider}.")
                }
            );
        }
        AiCommand::Status => {
            let mut profiles = serde_json::Map::new();
            if let Some(providers) = config["providers"].as_object() {
                for (provider, profile) in providers {
                    profiles.insert(provider.clone(), json!({"model": profile["model"], "base_url": profile["base_url"], "token_saved": profile["api_key"].as_str().is_some_and(|key| !key.is_empty())}));
                }
            }
            let status = json!({"provider": config["provider"], "providers": profiles});
            if compact {
                println!("{}", serde_json::to_string(&status)?);
            } else if output == OutputFormat::Json
                || (output == OutputFormat::Auto && !io::stdout().is_terminal())
            {
                println!("{}", serde_json::to_string_pretty(&status)?);
            } else {
                println!(
                    "Saved provider: {}",
                    config["provider"].as_str().unwrap_or("Not configured")
                );
                for (provider, profile) in profiles {
                    println!(
                        "{provider}: model={} · token={} · URL={}",
                        profile["model"].as_str().unwrap_or("Not configured"),
                        if profile["token_saved"] == true {
                            "Saved"
                        } else {
                            "Not saved"
                        },
                        profile["base_url"].as_str().unwrap_or("Provider default")
                    );
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    fn config() -> Value {
        json!({"provider": "anthropic", "providers": {
            "anthropic": {"model": "claude-test", "api_key": "anthropic-secret", "base_url": "https://example.com/v1"},
            "openai": {"model": "gpt-test", "api_key": "openai-secret"}
        }})
    }
    #[test]
    fn saved_settings_and_overrides_are_scoped_to_the_selected_provider() {
        let saved = resolve_from(&config(), None, None, None, None).unwrap();
        assert_eq!(saved.model, "anthropic::claude-test");
        assert_eq!(saved.api_key.as_deref(), Some("anthropic-secret"));
        let switched = resolve_from(&config(), Some("openai"), None, None, None).unwrap();
        assert_eq!(switched.model, "openai::gpt-test");
        assert_eq!(switched.api_key.as_deref(), Some("openai-secret"));
        assert!(switched.base_url.is_none());
        let override_settings = resolve_from(
            &config(),
            None,
            Some("openai::other"),
            Some("override"),
            Some("http://localhost:1234/v1"),
        )
        .unwrap();
        assert_eq!(override_settings.model, "openai::other");
        assert_eq!(override_settings.api_key.as_deref(), Some("override"));
        assert!(
            resolve_from(
                &config(),
                Some("anthropic"),
                Some("openai::test"),
                None,
                None
            )
            .is_err()
        );
    }
    #[test]
    fn invalid_configuration_fails_without_using_another_providers_key() {
        assert!(resolve_from(&config(), Some("gemini"), None, None, None).is_err());
        assert!(resolve_from(&config(), None, Some(" "), None, None).is_err());
        assert!(provider_name("made-up").is_err());
        assert!(validate_base_url("https://secret@example.com/v1").is_err());
        assert!(validate_base_url("https://example.com?token=secret").is_err());
        assert!(validate_base_url("file:///tmp/mock").is_err());
        let jev = resolve_from(&config(), Some("jev"), None, None, None).unwrap();
        assert_eq!(jev.model, "jev::jev-latest");
        assert!(jev.api_key.is_none());
    }
    #[test]
    fn ai_url_setting_does_not_inherit_the_lunchmoney_api_url() {
        let cli =
            crate::Cli::try_parse_from(["lunchmoney", "ai", "set", "--provider", "anthropic"])
                .unwrap();
        let crate::Commands::Ai {
            command: AiCommand::Set { base_url, .. },
        } = cli.command
        else {
            panic!("expected AI settings");
        };
        assert!(base_url.is_none());
    }

    #[test]
    fn private_ai_file_round_trips_and_keeps_lunchmoney_auth_separate() {
        let directory = env::temp_dir().join(format!("lunchmoney-ai-test-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("ai.json");
        let auth_path = directory.join("config.json");
        fs::write(&auth_path, "unchanged-lunchmoney-token").unwrap();
        save_at(&path, &config()).unwrap();
        assert_eq!(load_at(&path).unwrap(), config());
        assert_eq!(
            fs::read_to_string(auth_path).unwrap(),
            "unchanged-lunchmoney-token"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        fs::remove_dir_all(directory).unwrap();
    }
}
