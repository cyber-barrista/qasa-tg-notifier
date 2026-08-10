use std::time::Duration;

use anyhow::{Context, Result};

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

        Ok(Self {
            bot_token,
            chat_id,
            area,
            home_types,
            interval,
            max_notify,
            endpoint,
        })
    }
}

fn required(key: &str) -> Result<String> {
    std::env::var(key).with_context(|| format!("missing required env var {key}"))
}

fn optional(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}
