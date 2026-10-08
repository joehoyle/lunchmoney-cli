use anyhow::{Context, Result, bail};
use genai::adapter::AdapterKind;
use genai::resolver::{AuthData, Endpoint};
use std::time::Duration;

pub(crate) fn chat_error(error: genai::Error) -> anyhow::Error {
    // The SDK's error wrappers do not expose their nested sources through Error::source.
    // Unwrap only transport errors, avoiding debug output containing transaction payloads.
    if let genai::Error::WebModelCall {
        webc_error: genai::webc::Error::Reqwest(cause),
        ..
    } = &error
    {
        let mut message = format!("AI connection failed: {cause}");
        let mut source = std::error::Error::source(cause);
        while let Some(cause) = source {
            message.push_str(&format!("; {cause}"));
            source = cause.source();
        }
        return anyhow::anyhow!(message);
    }
    error.into()
}

pub(crate) fn ai_client(args: &crate::ai::Settings) -> Result<genai::Client> {
    let mut builder = genai::Client::builder()
        .with_web_config(genai::WebConfig::default().with_timeout(Duration::from_secs(90)));
    if let Some(key) = &args.api_key {
        if key.trim().is_empty() {
            bail!("AI API key must not be empty");
        }
        let key = key.clone();
        builder =
            builder.with_auth_resolver_fn(move |_| Ok(Some(AuthData::from_single(key.clone()))));
    }
    let endpoint = if let Some(base_url) = &args.base_url {
        let parsed = url::Url::parse(base_url).context("invalid --ai-base-url")?;
        if !matches!(parsed.scheme(), "http" | "https")
            || parsed.host_str().is_none()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
        {
            bail!(
                "--ai-base-url must be an HTTP(S) base URL without credentials, query, or fragment"
            );
        }
        Some(Endpoint::from_owned(format!(
            "{}/",
            base_url.trim_end_matches('/')
        )))
    } else {
        None
    };
    builder = builder.with_service_target_resolver_fn(move |mut target: genai::ServiceTarget| {
        // GPT-6 reasoning with function tools requires Responses. Resolve the
        // protocol independently of authentication and preserve custom endpoints.
        let name = target
            .model
            .model_name
            .rsplit("::")
            .next()
            .unwrap_or_default();
        if target.model.adapter_kind == AdapterKind::OpenAI && name.starts_with("gpt-6") {
            target.model.adapter_kind = AdapterKind::OpenAIResp;
        }
        if let Some(endpoint) = &endpoint {
            target.endpoint = endpoint.clone();
        }
        Ok(target)
    });
    Ok(builder.build())
}
