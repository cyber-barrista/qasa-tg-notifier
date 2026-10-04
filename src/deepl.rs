//! Minimal DeepL client: just `POST /v2/translate`, used to turn Chợ Tốt's
//! Vietnamese titles and descriptions into English before posting.
//!
//! Hand-rolled on the shared reqwest client rather than the `deepl` crate,
//! which would add half a dozen crates for one JSON call. DeepL's API is
//! stable and documented: `Authorization: DeepL-Auth-Key <key>`, a JSON body
//! `{"text": [...], "source_lang": "VI", "target_lang": "EN-US"}`, and a
//! response `{"translations": [{"text": "...", ...}]}` in input order. Free
//! keys end in `:fx` and must use the `api-free.deepl.com` host.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::Config;

const FREE_ENDPOINT: &str = "https://api-free.deepl.com/v2/translate";
const PRO_ENDPOINT: &str = "https://api.deepl.com/v2/translate";
/// DeepL's per-request cap on the `text` array.
const MAX_TEXTS: usize = 50;

#[derive(Clone)]
pub struct Translator {
    client: reqwest::Client,
    endpoint: String,
    key: String,
}

#[derive(Serialize)]
struct Request<'a> {
    text: &'a [&'a str],
    source_lang: &'static str,
    target_lang: &'static str,
}

#[derive(Deserialize)]
struct Response {
    #[serde(default)]
    translations: Vec<Translation>,
}

#[derive(Deserialize)]
struct Translation {
    #[serde(default)]
    text: String,
}

impl Translator {
    /// `None` when no `DEEPL_API_KEY` is configured (ads are then posted in
    /// Vietnamese).
    pub fn from_config(client: &reqwest::Client, cfg: &Config) -> Option<Translator> {
        let key = cfg.deepl_api_key.clone()?;
        let endpoint = cfg
            .deepl_endpoint
            .clone()
            .unwrap_or_else(|| default_endpoint(&key).to_string());
        Some(Translator {
            client: client.clone(),
            endpoint,
            key,
        })
    }

    /// Translate Vietnamese `texts` to English, one output per input, in order.
    pub async fn translate(&self, texts: &[&str]) -> Result<Vec<String>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        if texts.len() > MAX_TEXTS {
            bail!("DeepL accepts at most {MAX_TEXTS} texts per request");
        }
        let body = Request {
            text: texts,
            source_lang: "VI",
            target_lang: "EN-US",
        };
        let response: Response = self
            .client
            .post(&self.endpoint)
            .header("Authorization", format!("DeepL-Auth-Key {}", self.key))
            .json(&body)
            .send()
            .await
            .context("requesting DeepL translation")?
            .error_for_status()
            .context("DeepL rejected the request")?
            .json()
            .await
            .context("decoding DeepL response")?;
        if response.translations.len() != texts.len() {
            bail!(
                "DeepL returned {} translations for {} texts",
                response.translations.len(),
                texts.len()
            );
        }
        Ok(response.translations.into_iter().map(|t| t.text).collect())
    }
}

/// Free-plan keys (suffix `:fx`) live on a different host than Pro keys.
fn default_endpoint(key: &str) -> &'static str {
    if key.ends_with(":fx") {
        FREE_ENDPOINT
    } else {
        PRO_ENDPOINT
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_keys_use_the_free_host() {
        assert_eq!(default_endpoint("abc:fx"), FREE_ENDPOINT);
        assert_eq!(default_endpoint("abc"), PRO_ENDPOINT);
    }

    #[test]
    fn parses_response_in_order() {
        let raw = r#"{"translations":[
            {"text":"Luxury PH apartment for rent","detected_source_language":"VI"},
            {"text":"Near the beach","detected_source_language":"VI"}]}"#;
        let r: Response = serde_json::from_str(raw).unwrap();
        let texts: Vec<String> = r.translations.into_iter().map(|t| t.text).collect();
        assert_eq!(texts, ["Luxury PH apartment for rent", "Near the beach"]);
    }

    /// Hits the real API with the key from the environment (`make debug`'s
    /// `.env`); run with `cargo test -- --ignored`. Skips without a key.
    #[tokio::test]
    #[ignore = "network; needs DEEPL_API_KEY"]
    async fn live_translate_vietnamese() {
        let Ok(key) = std::env::var("DEEPL_API_KEY") else {
            eprintln!("DEEPL_API_KEY unset; skipping");
            return;
        };
        let t = Translator {
            client: reqwest::Client::new(),
            endpoint: default_endpoint(&key).to_string(),
            key,
        };
        let out = t
            .translate(&["Cho thuê căn hộ 2 phòng ngủ full nội thất", "Gần biển"])
            .await
            .unwrap();
        assert_eq!(out.len(), 2);
        assert!(out[0].to_lowercase().contains("rent"), "{out:?}");
        // Wording varies ("beach" / "ocean" / "sea"); just check it's English.
        assert!(out[1].is_ascii(), "{out:?}");
    }

    #[test]
    fn request_serializes_deepl_shape() {
        let req = Request {
            text: &["xin chào"],
            source_lang: "VI",
            target_lang: "EN-US",
        };
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(
            json,
            r#"{"text":["xin chào"],"source_lang":"VI","target_lang":"EN-US"}"#
        );
    }
}
