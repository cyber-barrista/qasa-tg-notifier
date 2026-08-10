//! qasa-tg-notifier: every few hours, poll Qasa's public GraphQL API for
//! genuinely new Stockholm apartment listings and push them to a Telegram
//! chat. Also serves an interactive `/search` filter UI.

mod config;
mod qasa;
mod search;
mod telegram;

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result};
use frankenstein::client_reqwest::Bot;
use frankenstein::methods::GetUpdatesParams;
use frankenstein::types::{CallbackQuery, MaybeInaccessibleMessage, User};
use frankenstein::updates::UpdateContent;
use frankenstein::AsyncTelegramApi;
use time::OffsetDateTime;
use tracing::{debug, error, info, warn};

use config::Config;

/// Cap on listings a single search will post.
const RECENT_MAX_LISTINGS: usize = 40;
/// How many listings to scan (within the age window) before client-side
/// room/price filtering.
const SEARCH_SCAN_MAX: usize = 200;
/// Pause between messages. Telegram caps sends to a single group at ~20/min,
/// so ~3s spacing keeps us under it; the 429-retry in `telegram` is the backstop.
const SEND_GAP: Duration = Duration::from_secs(3);

const HELP: &str = "QASA notifier.\n\
     • /search — open the filter UI (neighborhood, age, rooms, max rent).\n\
     • I also post new Stockholm apartments automatically every few hours.";

/// In-progress search sessions, keyed by (chat_id, config-message_id).
type Sessions = HashMap<(i64, i32), search::Filters>;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cfg = Config::from_env()?;

    let http = reqwest::Client::builder()
        .user_agent(concat!("qasa-tg-notifier/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("building HTTP client")?;
    let bot = Bot::new(&cfg.bot_token);

    info!(
        area = %cfg.area,
        home_types = ?cfg.home_types,
        interval_secs = cfg.interval.as_secs(),
        "starting qasa-tg-notifier"
    );

    // Two independent loops: the periodic notifier and the command listener.
    let notifier = tokio::spawn(notifier_loop(http.clone(), bot.clone(), cfg.clone()));
    let commands = tokio::spawn(command_loop(http, bot, cfg));

    // Neither loop returns in normal operation; if one dies, exit so the
    // container restarts.
    tokio::select! {
        r = notifier => error!("notifier task exited: {r:?}"),
        r = commands => error!("command task exited: {r:?}"),
    }
    Ok(())
}

/// Poll every `cfg.interval` and push genuinely-new listings.
async fn notifier_loop(http: reqwest::Client, bot: Bot, cfg: Config) {
    let mut ticker = tokio::time::interval(cfg.interval);
    loop {
        // `interval`'s first tick fires immediately, so we poll on startup.
        ticker.tick().await;
        if let Err(e) = run_cycle(&http, &bot, &cfg).await {
            error!("cycle failed: {e:#}");
        }
    }
}

async fn run_cycle(http: &reqwest::Client, bot: &Bot, cfg: &Config) -> Result<()> {
    let state = telegram::read_state(bot, cfg.chat_id)
        .await
        .context("reading pinned state")?;
    let watermark = state.as_ref().map(|s| s.watermark);

    let fetched = qasa::fetch_new(http, cfg, watermark)
        .await
        .context("fetching listings")?;

    let Some(state) = state else {
        // First run: record the watermark, notify nothing.
        telegram::write_state(bot, cfg.chat_id, None, fetched.max_id).await?;
        info!(
            watermark = fetched.max_id,
            "seeded state on first run; no notifications sent"
        );
        return Ok(());
    };

    let mut new = fetched.new;
    if new.is_empty() {
        info!("no new listings");
        return Ok(());
    }

    // Oldest-new first, so the chat reads chronologically.
    new.sort_by_key(|h| h.id_num().unwrap_or(0));
    let total = new.len();
    let send_n = total.min(cfg.max_notify);

    for home in &new[..send_n] {
        if let Err(e) = telegram::send_listing(bot, cfg.chat_id, home).await {
            warn!(id = %home.id, "failed to send listing: {e:#}");
        }
        tokio::time::sleep(SEND_GAP).await;
    }

    if total > send_n {
        let more = total - send_n;
        let _ = telegram::send_note(
            bot,
            cfg.chat_id,
            &format!("…and {more} more new listing(s) — sending next cycle."),
        )
        .await;
    }

    // Advance the watermark only past what we actually sent, so a capped burst
    // is delivered over subsequent cycles rather than silently dropped.
    let sent_max = new[..send_n]
        .iter()
        .filter_map(qasa::Home::id_num)
        .max()
        .unwrap_or(state.watermark);
    let new_watermark = state.watermark.max(sent_max);
    telegram::write_state(bot, cfg.chat_id, Some(state.message_id), new_watermark).await?;

    info!(
        sent = send_n,
        total_new = total,
        watermark = new_watermark,
        "cycle complete"
    );
    Ok(())
}

/// Long-poll `getUpdates` and dispatch commands and button presses.
async fn command_loop(http: reqwest::Client, bot: Bot, cfg: Config) {
    let mut params = GetUpdatesParams::builder().timeout(30).build();
    let mut sessions: Sessions = HashMap::new();
    info!("command listener started");
    loop {
        match bot.get_updates(&params).await {
            Ok(response) => {
                for update in response.result {
                    params.offset = Some(i64::from(update.update_id) + 1);
                    match update.content {
                        UpdateContent::Message(message) => {
                            if let Some(text) = message.text.as_deref() {
                                let user = message.from.as_deref();
                                handle_message(
                                    &bot,
                                    &cfg,
                                    &mut sessions,
                                    message.chat.id,
                                    user,
                                    text,
                                )
                                .await;
                            }
                        }
                        UpdateContent::CallbackQuery(query) => {
                            handle_callback(&http, &bot, &cfg, &mut sessions, &query).await;
                        }
                        _ => {}
                    }
                }
            }
            Err(e) => {
                warn!("get_updates failed: {e:#}");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
}

/// Render a Telegram user for logs, e.g. `Anna (id=123, @anna)`.
fn describe_user(user: Option<&User>) -> String {
    match user {
        Some(u) => {
            let handle = u
                .username
                .as_deref()
                .map_or_else(|| "no-username".to_string(), |h| format!("@{h}"));
            format!("{} (id={}, {})", u.first_name, u.id, handle)
        }
        None => "unknown user".to_string(),
    }
}

async fn handle_message(
    bot: &Bot,
    cfg: &Config,
    sessions: &mut Sessions,
    chat_id: i64,
    user: Option<&User>,
    text: &str,
) {
    if chat_id != cfg.chat_id {
        debug!(chat_id, "ignoring message from non-target chat");
        return;
    }
    let mut parts = text.split_whitespace();
    let Some(raw) = parts.next() else {
        return;
    };
    // Strip a `@botname` suffix (present when addressed in groups).
    let cmd = raw.split('@').next().unwrap_or(raw);
    let who = describe_user(user);

    match cmd {
        "/search" | "/recent" => {
            info!(user = %who, command = cmd, "opening search UI");
            let filters = search::Filters::default();
            let (text, keyboard) = search::render(search::Screen::Main, &filters);
            match telegram::send_keyboard(bot, chat_id, &text, keyboard).await {
                Ok(message) => {
                    sessions.insert((chat_id, message.message_id), filters);
                }
                Err(e) => error!(user = %who, "failed to open search: {e:#}"),
            }
        }
        "/start" | "/help" => {
            info!(user = %who, command = cmd, "help requested");
            let _ = telegram::send_note(bot, cfg.chat_id, HELP).await;
        }
        other => {
            debug!(user = %who, text = other, "ignoring non-command message");
        }
    }
}

async fn handle_callback(
    http: &reqwest::Client,
    bot: &Bot,
    cfg: &Config,
    sessions: &mut Sessions,
    query: &CallbackQuery,
) {
    // Always ack, so the client's spinner stops.
    let _ = telegram::answer_callback(bot, &query.id).await;

    let who = describe_user(Some(&query.from));

    let Some(data) = query.data.as_deref() else {
        return;
    };
    let Some(message) = query.message.as_ref() else {
        return;
    };
    let (chat_id, message_id) = match message {
        MaybeInaccessibleMessage::Message(m) => (m.chat.id, m.message_id),
        MaybeInaccessibleMessage::InaccessibleMessage(m) => (m.chat.id, m.message_id),
    };
    if chat_id != cfg.chat_id {
        debug!(user = %who, chat_id, "ignoring callback from non-target chat");
        return;
    }
    debug!(user = %who, button = data, "button pressed");

    let key = (chat_id, message_id);
    let Some(mut filters) = sessions.get(&key).cloned() else {
        debug!(user = %who, "callback for expired search session");
        let _ = telegram::edit_plain(
            bot,
            chat_id,
            message_id,
            "This search expired — send /search to start a new one.",
        )
        .await;
        return;
    };

    match search::apply(&mut filters, data) {
        search::Action::Show(screen) => {
            let (text, keyboard) = search::render(screen, &filters);
            let _ = telegram::edit_keyboard(bot, chat_id, message_id, &text, keyboard).await;
            sessions.insert(key, filters);
        }
        search::Action::Search => {
            info!(
                user = %who,
                age_hours = filters.age_hours,
                min_rooms = filters.min_rooms,
                min_rent = ?filters.min_rent,
                max_rent = ?filters.max_rent,
                areas = %filters.area_summary(),
                "search triggered"
            );
            sessions.remove(&key);
            let _ = telegram::edit_plain(
                bot,
                chat_id,
                message_id,
                &format!("🔎 Searching…\n\n{}", search::describe(&filters)),
            )
            .await;
            tokio::spawn(run_search(
                http.clone(),
                bot.clone(),
                cfg.clone(),
                chat_id,
                filters,
            ));
        }
        search::Action::Ignore => {}
    }
}

/// Fetch, filter, and post the results of a completed search.
async fn run_search(
    http: reqwest::Client,
    bot: Bot,
    cfg: Config,
    chat_id: i64,
    filters: search::Filters,
) {
    let cutoff = OffsetDateTime::now_utc() - time::Duration::hours(filters.age_hours);
    let slugs = filters.area_slugs();

    let fetched = match qasa::fetch_recent(&http, &cfg, &slugs, cutoff, SEARCH_SCAN_MAX).await {
        Ok(v) => v,
        Err(e) => {
            error!("search fetch failed: {e:#}");
            let _ = telegram::send_note(&bot, chat_id, "⚠️ Search failed, please try again.").await;
            return;
        }
    };

    let mut matches: Vec<qasa::Home> = fetched
        .into_iter()
        .filter(|h| search::passes(&filters, h))
        .collect();
    let total = matches.len();

    if total == 0 {
        info!(areas = %filters.area_summary(), "search returned no matches");
        let _ = telegram::send_note(&bot, chat_id, "No matches for those filters.").await;
        return;
    }

    // Keep the newest `RECENT_MAX_LISTINGS` (list is oldest-first).
    if matches.len() > RECENT_MAX_LISTINGS {
        matches = matches.split_off(matches.len() - RECENT_MAX_LISTINGS);
    }

    info!(
        areas = %filters.area_summary(),
        total_matches = total,
        posting = matches.len(),
        "search complete"
    );

    for home in &matches {
        if let Err(e) = telegram::send_listing(&bot, chat_id, home).await {
            warn!(id = %home.id, "failed to send listing: {e:#}");
        }
        tokio::time::sleep(SEND_GAP).await;
    }

    let note = if total > matches.len() {
        format!(
            "✅ {} matches — showing the newest {}.",
            total,
            matches.len()
        )
    } else {
        format!("✅ {total} match(es).")
    };
    let _ = telegram::send_note(&bot, chat_id, &note).await;
}
