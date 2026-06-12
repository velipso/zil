use collections::HashMap;
use http_client::CustomHeaders;
use http_client::http::{HeaderName, HeaderValue};

pub mod anthropic;
pub mod bedrock;
pub mod cloud;
pub mod deepseek;
pub mod google;
pub mod lmstudio;
pub mod mistral;
pub mod ollama;
pub mod open_ai;
pub mod open_ai_compatible;
pub mod open_router;
pub mod openai_subscribed;
pub mod opencode;

pub mod vercel_ai_gateway;
pub mod x_ai;

const COMMON_RESERVED_HEADER_NAMES: &[&str] = &["Authorization", "Content-Type", "Accept"];

/// Validate the user-supplied custom-headers map once at settings load time,
/// dropping reserved or malformed entries (each with a `log::warn!`) and
/// returning a typed `CustomHeaders` ready to be appended to outgoing requests.
pub(crate) fn resolve_custom_headers(
    provider_name: &str,
    settings: &HashMap<String, String>,
    reserved_header_names: &[&str],
) -> CustomHeaders {
    let headers = settings
        .iter()
        .filter_map(|(name, value)| {
            if COMMON_RESERVED_HEADER_NAMES
                .iter()
                .chain(reserved_header_names)
                .any(|reserved| reserved.eq_ignore_ascii_case(name))
            {
                log::warn!(
                    "ignoring custom {provider_name} header `{name}`: managed by Zed and cannot be overridden"
                );
                return None;
            }
            let header_name = match name.parse::<HeaderName>() {
                Ok(header_name) => header_name,
                Err(err) => {
                    log::warn!("ignoring custom {provider_name} header `{name}`: invalid header name ({err})");
                    return None;
                }
            };
            let header_value = match HeaderValue::from_str(value) {
                Ok(header_value) => header_value,
                Err(err) => {
                    log::warn!(
                        "ignoring custom {provider_name} header `{name}`: invalid header value ({err})"
                    );
                    return None;
                }
            };
            Some((header_name, header_value))
        })
        .collect();
    CustomHeaders::new(headers)
}
