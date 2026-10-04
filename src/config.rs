use std::time::Duration;

use anyhow::{Context, Result};

use crate::nhatot::City;

/// Runtime configuration, read entirely from environment variables.
///
/// `BOT_TOKEN` and `CHAT_ID` are required (set as Fly secrets in production);
/// everything else has a sensible default so a bare `BOT_TOKEN`/`CHAT_ID` pair
/// is enough to run.
#[derive(Debug, Clone)]
pub struct Config {
    pub bot_token: String,
    pub chat_id: i64,
    /// Qasa area identifier, e.g. `se/stockholm`.
    pub area: String,
    /// `homeType` filter values, e.g. `["apartment"]`.
    pub home_types: Vec<String>,
    /// How long to wait between polls.
    pub interval: Duration,
    /// Cap on listings sent in a single cycle; the rest are summarised.
    pub max_notify: usize,
    pub endpoint: String,
    /// Chat for Bostadsförmedlingen "Bostad snabbt" notifications; unset
    /// disables that notifier entirely.
    pub bostad_chat_id: Option<i64>,
    /// How long to wait between Bostadsförmedlingen polls. Much shorter than
    /// the Qasa interval: Bostad snabbt ads are first-come-first-served.
    pub bostad_interval: Duration,
    pub bostad_endpoint: String,
    /// Chat for Nha Trang (Chợ Tốt / nhatot.com) rental notifications; unset
    /// disables that notifier entirely.
    pub nhatot_chat_id: Option<i64>,
    pub nhatot_interval: Duration,
    pub nhatot_endpoint: String,
    /// Cities the notifier polls (the `/nhatrang` and `/danang` commands work
    /// regardless). Default: both.
    pub nhatot_cities: Vec<City>,
    /// Chợ Tốt `cg` category code the notifier polls (default: all real
    /// estate, filtered to apartments + houses client-side).
    pub nhatot_category: u32,
    /// DeepL key for translating Chợ Tốt titles/descriptions to English;
    /// unset posts them in Vietnamese.
    pub deepl_api_key: Option<String>,
    /// Override of the DeepL endpoint (defaults by key type, see `deepl`).
    pub deepl_endpoint: Option<String>,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let bot_token = required("BOT_TOKEN")?;
        let chat_id = required("CHAT_ID")?
            .parse()
            .context("CHAT_ID must be an integer (a Telegram chat id)")?;

        let area = optional("QASA_AREA", "se/stockholm");
        let home_types = optional("HOME_TYPES", "apartment")
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>();

        let hours: u64 = optional("POLL_INTERVAL_HOURS", "3")
            .parse()
            .context("POLL_INTERVAL_HOURS must be a positive integer")?;
        let interval = Duration::from_secs(hours.max(1) * 3600);

        let max_notify = optional("MAX_NOTIFY_PER_CYCLE", "40")
            .parse()
            .context("MAX_NOTIFY_PER_CYCLE must be an integer")?;

        let endpoint = optional("QASA_ENDPOINT", "https://api.qasa.com/graphql");

        let bostad_chat_id = optional_chat_id("BOSTAD_CHAT_ID")?;
        let nhatot_chat_id = optional_chat_id("NHATOT_CHAT_ID")?;
        // Each notifier persists its state in *the* pinned message of its chat
        // (getChat exposes only the most recently pinned one), so sharing a
        // chat would have two notifiers overwrite each other's state.
        ensure_distinct_chats(&[
            ("CHAT_ID", Some(chat_id)),
            ("BOSTAD_CHAT_ID", bostad_chat_id),
            ("NHATOT_CHAT_ID", nhatot_chat_id),
        ])?;

        let bostad_interval = minutes("BOSTAD_POLL_INTERVAL_MINS", "10")?;
        let bostad_endpoint = optional(
            "BOSTAD_ENDPOINT",
            "https://bostad.stockholm.se/AllaAnnonser/",
        );

        let nhatot_interval = minutes("NHATOT_POLL_INTERVAL_MINS", "30")?;
        let nhatot_endpoint = optional(
            "NHATOT_ENDPOINT",
            "https://gateway.chotot.com/v1/public/ad-listing",
        );
        let nhatot_cities = optional("NHATOT_CITIES", "nhatrang,danang")
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|slug| {
                City::from_slug(slug).with_context(|| {
                    format!("NHATOT_CITIES: unknown city {slug:?} (known: nhatrang, danang)")
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let nhatot_category = optional("NHATOT_CATEGORY", "1000")
            .parse()
            .context("NHATOT_CATEGORY must be an integer (a Chợ Tốt cg code)")?;

        let deepl_api_key = std::env::var("DEEPL_API_KEY")
            .ok()
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty());
        let deepl_endpoint = std::env::var("DEEPL_ENDPOINT").ok();

        Ok(Self {
            bot_token,
            chat_id,
            area,
            home_types,
            interval,
            max_notify,
            endpoint,
            bostad_chat_id,
            bostad_interval,
            bostad_endpoint,
            nhatot_chat_id,
            nhatot_interval,
            nhatot_endpoint,
            nhatot_cities,
            nhatot_category,
            deepl_api_key,
            deepl_endpoint,
        })
    }
}

fn required(key: &str) -> Result<String> {
    std::env::var(key).with_context(|| format!("missing required env var {key}"))
}

fn optional(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

/// An optional chat id: unset is `None`, set-but-invalid is a hard error.
fn optional_chat_id(key: &str) -> Result<Option<i64>> {
    match std::env::var(key) {
        Ok(v) => Ok(Some(v.parse().with_context(|| {
            format!("{key} must be an integer (a Telegram chat id)")
        })?)),
        Err(_) => Ok(None),
    }
}

/// A poll interval given in minutes, floored at one minute.
fn minutes(key: &str, default: &str) -> Result<Duration> {
    let mins: u64 = optional(key, default)
        .parse()
        .with_context(|| format!("{key} must be a positive integer"))?;
    Ok(Duration::from_secs(mins.max(1) * 60))
}

/// Fail if any two configured chats are the same chat.
fn ensure_distinct_chats(chats: &[(&str, Option<i64>)]) -> Result<()> {
    for (i, (a_name, a)) in chats.iter().enumerate() {
        let Some(a_id) = a else { continue };
        for (b_name, b) in &chats[i + 1..] {
            if *b == Some(*a_id) {
                anyhow::bail!(
                    "{b_name} must differ from {a_name}: each notifier stores its state in its chat's pinned message"
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_distinct_chats_rejects_equal_pairs() {
        assert!(ensure_distinct_chats(&[("A", Some(1)), ("B", None), ("C", None)]).is_ok());
        assert!(ensure_distinct_chats(&[("A", Some(1)), ("B", Some(2)), ("C", Some(3))]).is_ok());
        let err = ensure_distinct_chats(&[("A", Some(1)), ("B", Some(2)), ("C", Some(1))])
            .unwrap_err()
            .to_string();
        assert!(err.starts_with("C must differ from A"), "{err}");
        assert!(ensure_distinct_chats(&[("A", Some(1)), ("B", Some(1))]).is_err());
    }
}
