//! qasa-tg-notifier: every few hours, poll Qasa's public GraphQL API for
//! genuinely new Stockholm apartment listings and push them to a Telegram
//! chat. Optionally also polls Bostadsförmedlingen's feed every few minutes
//! for new first-come-first-served "Bostad snabbt" ads (a separate chat), and
//! Chợ Tốt's listings for new Nha Trang rentals (another separate chat).
//! Serves interactive `/search`, `/bostad` and `/nhatrang` filter UIs.

mod bostad;
mod bostad_search;
mod config;
mod deepl;
mod nhatot;
mod nhatot_search;
mod qasa;
mod search;
mod telegram;

use std::collections::{BTreeSet, HashMap};
use std::time::Duration;

use anyhow::{Context, Result};
use frankenstein::client_reqwest::Bot;
use frankenstein::methods::GetUpdatesParams;
use frankenstein::types::{CallbackQuery, Chat, ChatType, MaybeInaccessibleMessage, User};
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
/// Pages (50 ads each) a `/nhatrang` or `/danang` search scans per request.
const NHATOT_SEARCH_PAGES: usize = 4;
/// Pages the notifier scans per city × category each cycle. One page is
/// plenty: Đà Nẵng's 50 newest apartment ads span ~6 h against a 30-min
/// poll, and the pinned seen-set is pruned to this window, so with two
/// cities × two categories it stays ≤ 200 ids (Telegram caps a message at
/// 4096 chars).
const NHATOT_NOTIFY_PAGES: usize = 1;
/// The notifier only posts ads *first* listed this recently. Đà Nẵng sellers
/// bump constantly; without this, every old ad bumped back into the window
/// after dropping out of it would be re-posted as new.
const NHATOT_NOTIFY_MAX_FIRST_LISTED_HOURS: i64 = 48;
/// How much of a Chợ Tốt description to send to DeepL. Longer than the
/// posted preview so the cut falls after translation, short enough to keep
/// the free plan's 500k chars/month comfortable.
const TRANSLATE_BODY_CHARS: usize = 500;

const HELP_COMMANDS: &str = "QASA notifier.\n\
     • /search — open the Qasa filter UI (neighborhood, age, rooms, max rent).\n\
     • /bostad — open the Bostadsförmedlingen filter UI (category, kommun, rooms, rent, queue years).\n\
     • /nhatrang — open the Nha Trang (Chợ Tốt) filter UI (age, type, ward, rooms, rent, size).\n\
     • /danang — the same for Da Nang (districts instead of wards).";

/// `/help` text for a given chat: the command list, plus a line about the
/// scheduled posts *this* chat receives. Chats without a notifier (DMs,
/// other groups) get the commands only.
fn help_text(cfg: &Config, chat_id: i64) -> String {
    let scheduled = if chat_id == cfg.chat_id {
        Some(format!(
            "I also post new Stockholm apartments from Qasa here every {} hours.",
            cfg.interval.as_secs() / 3600
        ))
    } else if Some(chat_id) == cfg.bostad_chat_id {
        Some(format!(
            "I also post new first-come-first-served Bostad snabbt ads here every {} minutes.",
            cfg.bostad_interval.as_secs() / 60
        ))
    } else if Some(chat_id) == cfg.nhatot_chat_id {
        let cities: Vec<&str> = cfg.nhatot_cities.iter().map(|c| c.label()).collect();
        Some(format!(
            "I also post new {} rentals from Chợ Tốt here every {} minutes.",
            cities.join(" and "),
            cfg.nhatot_interval.as_secs() / 60
        ))
    } else {
        None
    };
    match scheduled {
        Some(line) => format!("{HELP_COMMANDS}\n• {line}"),
        None => HELP_COMMANDS.to_string(),
    }
}

/// An in-progress filter-UI session: which search a config message belongs to.
#[derive(Clone)]
enum Session {
    Qasa(search::Filters),
    Bostad(bostad_search::Filters),
    Nhatot(nhatot_search::Filters),
}

/// In-progress search sessions, keyed by (chat_id, config-message_id).
type Sessions = HashMap<(i64, i32), Session>;

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

    // Independent loops: the periodic notifiers and the command listener.
    let notifier = tokio::spawn(notifier_loop(http.clone(), bot.clone(), cfg.clone()));
    let bostad_notifier = match cfg.bostad_chat_id {
        Some(chat_id) => {
            info!(
                chat_id,
                interval_secs = cfg.bostad_interval.as_secs(),
                "bostad snabbt notifier enabled"
            );
            tokio::spawn(bostad_notifier_loop(
                http.clone(),
                bot.clone(),
                cfg.clone(),
                chat_id,
            ))
        }
        // Disabled: park a task that never resolves so the select! below is uniform.
        None => tokio::spawn(std::future::pending()),
    };
    let nhatot_notifier = match cfg.nhatot_chat_id {
        Some(chat_id) => {
            info!(
                chat_id,
                interval_secs = cfg.nhatot_interval.as_secs(),
                cities = ?cfg.nhatot_cities,
                category = cfg.nhatot_category,
                translate = cfg.deepl_api_key.is_some(),
                "nhatot (Vietnam) notifier enabled"
            );
            tokio::spawn(nhatot_notifier_loop(
                http.clone(),
                bot.clone(),
                cfg.clone(),
                chat_id,
            ))
        }
        None => tokio::spawn(std::future::pending()),
    };
    let commands = tokio::spawn(command_loop(http, bot, cfg));

    // No loop returns in normal operation; if one dies, exit so the
    // container restarts.
    tokio::select! {
        r = notifier => error!("notifier task exited: {r:?}"),
        r = bostad_notifier => error!("bostad notifier task exited: {r:?}"),
        r = nhatot_notifier => error!("nhatot notifier task exited: {r:?}"),
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

/// Poll the Bostadsförmedlingen feed every `cfg.bostad_interval` and push new
/// "Bostad snabbt" (first-come-first-served) ads.
async fn bostad_notifier_loop(http: reqwest::Client, bot: Bot, cfg: Config, chat_id: i64) {
    let mut ticker = tokio::time::interval(cfg.bostad_interval);
    loop {
        ticker.tick().await;
        if let Err(e) = run_bostad_cycle(&http, &bot, &cfg, chat_id).await {
            error!("bostad cycle failed: {e:#}");
        }
    }
}

/// One Bostad snabbt cycle. Dedup is a seen-id *set* (not a watermark, since
/// `AnnonsId` is not monotonic with publish date) persisted in the bostad
/// chat's pinned message; pruning it to live ads keeps it a handful of ids.
async fn run_bostad_cycle(
    http: &reqwest::Client,
    bot: &Bot,
    cfg: &Config,
    chat_id: i64,
) -> Result<()> {
    let snabbt: Vec<bostad::Ad> = bostad::fetch_all(http, cfg)
        .await
        .context("fetching bostad feed")?
        .into_iter()
        .filter(|ad| ad.bostad_snabbt)
        .collect();
    let live_ids: BTreeSet<u64> = snabbt.iter().map(|ad| ad.annons_id).collect();

    let state = telegram::read_seen_state(bot, chat_id)
        .await
        .context("reading pinned bostad state")?;

    let Some(state) = state else {
        // First run: record every live ad, notify nothing.
        telegram::write_seen_state(bot, chat_id, None, &telegram::BOSTAD_STATE, &live_ids).await?;
        info!(
            live = live_ids.len(),
            "seeded bostad state on first run; no notifications sent"
        );
        return Ok(());
    };

    let mut new: Vec<&bostad::Ad> = snabbt
        .iter()
        .filter(|ad| !state.seen.contains(&ad.annons_id))
        .collect();
    // Oldest-new first, so the chat reads chronologically.
    new.sort_by_key(|ad| ad.annons_id);
    let total = new.len();
    let send_n = total.min(cfg.max_notify);

    for ad in &new[..send_n] {
        if let Err(e) = telegram::send_bostad_listing(bot, chat_id, ad).await {
            warn!(id = ad.annons_id, "failed to send bostad listing: {e:#}");
        }
        tokio::time::sleep(SEND_GAP).await;
    }
    if total > send_n {
        let more = total - send_n;
        let _ = telegram::send_note(
            bot,
            chat_id,
            &format!("…and {more} more new ad(s) — sending next cycle."),
        )
        .await;
    }

    // Seen = still-live ads we'd already seen, plus what we just sent. Ads
    // capped out of this cycle stay unseen and go out next cycle.
    let mut seen: BTreeSet<u64> = state.seen.intersection(&live_ids).copied().collect();
    seen.extend(new[..send_n].iter().map(|ad| ad.annons_id));
    // Skip the write when nothing changed — Telegram rejects a no-op edit.
    if seen != state.seen {
        telegram::write_seen_state(
            bot,
            chat_id,
            Some(state.message_id),
            &telegram::BOSTAD_STATE,
            &seen,
        )
        .await?;
    }

    if total > 0 {
        info!(sent = send_n, total_new = total, "bostad cycle complete");
    } else {
        info!("no new bostad snabbt ads");
    }
    Ok(())
}

/// Poll Chợ Tốt every `cfg.nhatot_interval` and push new Nha Trang rentals.
async fn nhatot_notifier_loop(http: reqwest::Client, bot: Bot, cfg: Config, chat_id: i64) {
    let mut ticker = tokio::time::interval(cfg.nhatot_interval);
    loop {
        ticker.tick().await;
        if let Err(e) = run_nhatot_cycle(&http, &bot, &cfg, chat_id).await {
            error!("nhatot cycle failed: {e:#}");
        }
    }
}

/// One Nha Trang cycle. Same seen-set scheme as bostad: Chợ Tốt ids are not
/// monotonic with listing time and `list_time` is rewritten on bump, so a
/// watermark would miss ads. The set is pruned to the ids in the scanned
/// window; an ad bumped back after dropping out of it re-notifies, tagged.
async fn run_nhatot_cycle(
    http: &reqwest::Client,
    bot: &Bot,
    cfg: &Config,
    chat_id: i64,
) -> Result<()> {
    // One request per city × category: `cg=1000` would share one page
    // between apartments, houses, offices and land, so split it.
    let categories: Vec<u32> = if cfg.nhatot_category == nhatot::CATEGORY_ALL {
        vec![nhatot::CATEGORY_APARTMENT, nhatot::CATEGORY_HOUSE]
    } else {
        vec![cfg.nhatot_category]
    };
    let now_ms = OffsetDateTime::now_utc().unix_timestamp() * 1000;
    let first_listed_floor = now_ms - NHATOT_NOTIFY_MAX_FIRST_LISTED_HOURS * 3_600_000;
    let mut ads: Vec<nhatot::Ad> = Vec::new();
    for city in &cfg.nhatot_cities {
        for &category in &categories {
            let page = nhatot::fetch_rent(http, cfg, *city, &[], category, NHATOT_NOTIFY_PAGES)
                .await
                .with_context(|| format!("fetching Chợ Tốt {} listings", city.label()))?;
            ads.extend(page.into_iter().filter(|ad| {
                ad.is_home() && ad.orig_list_time.unwrap_or(ad.list_time) >= first_listed_floor
            }));
        }
    }
    if ads.is_empty() {
        // An empty window would prune the whole seen-set and re-notify
        // everything next cycle; treat it as a feed hiccup instead.
        warn!("nhatot feed returned no ads; leaving state untouched");
        return Ok(());
    }
    let window: BTreeSet<u64> = ads.iter().map(|ad| ad.ad_id).collect();

    let state = telegram::read_seen_state(bot, chat_id)
        .await
        .context("reading pinned nhatot state")?;

    let Some(state) = state else {
        // First run: record the whole window, notify nothing.
        telegram::write_seen_state(bot, chat_id, None, &telegram::NHATOT_STATE, &window).await?;
        info!(
            window = window.len(),
            "seeded nhatot state on first run; no notifications sent"
        );
        return Ok(());
    };

    let mut new: Vec<&nhatot::Ad> = ads
        .iter()
        .filter(|ad| !state.seen.contains(&ad.ad_id))
        .collect();
    // Oldest-new first, so the chat reads chronologically (by listing time,
    // since ids don't order by time here).
    new.sort_by_key(|ad| ad.list_time);
    let total = new.len();
    let send_n = total.min(cfg.max_notify);

    let translator = deepl::Translator::from_config(http, cfg);
    for ad in &new[..send_n] {
        let ad = translate_ad(ad, translator.as_ref()).await;
        if let Err(e) = telegram::send_nhatot_listing(bot, chat_id, &ad).await {
            warn!(id = ad.ad_id, "failed to send nhatot listing: {e:#}");
        }
        tokio::time::sleep(SEND_GAP).await;
    }
    if total > send_n {
        let more = total - send_n;
        let _ = telegram::send_note(
            bot,
            chat_id,
            &format!("…and {more} more new ad(s) — sending next cycle."),
        )
        .await;
    }

    // Seen = ads still in the window we'd already seen, plus what we just
    // sent. Ads capped out of this cycle stay unseen and go out next cycle.
    let mut seen: BTreeSet<u64> = state.seen.intersection(&window).copied().collect();
    seen.extend(new[..send_n].iter().map(|ad| ad.ad_id));
    // Skip the write when nothing changed — Telegram rejects a no-op edit.
    if seen != state.seen {
        telegram::write_seen_state(
            bot,
            chat_id,
            Some(state.message_id),
            &telegram::NHATOT_STATE,
            &seen,
        )
        .await?;
    }

    if total > 0 {
        info!(sent = send_n, total_new = total, "nhatot cycle complete");
    } else {
        info!("no new nhatot ads");
    }
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
                                    &message.chat,
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

/// Whether the bot serves commands from this chat: the configured chats plus
/// any one-on-one (private) chat. Other groups it may get added to are
/// ignored. Scheduled notifications still go only to their configured chat.
fn chat_allowed(cfg: &Config, chat: &Chat) -> bool {
    chat.id == cfg.chat_id
        || Some(chat.id) == cfg.bostad_chat_id
        || Some(chat.id) == cfg.nhatot_chat_id
        || chat.type_field == ChatType::Private
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
    chat: &Chat,
    user: Option<&User>,
    text: &str,
) {
    if !chat_allowed(cfg, chat) {
        debug!(chat_id = chat.id, "ignoring message from non-target chat");
        return;
    }
    let chat_id = chat.id;
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
                    sessions.insert((chat_id, message.message_id), Session::Qasa(filters));
                }
                Err(e) => error!(user = %who, "failed to open search: {e:#}"),
            }
        }
        "/bostad" => {
            info!(user = %who, command = cmd, "opening bostad search UI");
            let filters = bostad_search::Filters::default();
            let (text, keyboard) = bostad_search::render(bostad_search::Screen::Main, &filters);
            match telegram::send_keyboard(bot, chat_id, &text, keyboard).await {
                Ok(message) => {
                    sessions.insert((chat_id, message.message_id), Session::Bostad(filters));
                }
                Err(e) => error!(user = %who, "failed to open bostad search: {e:#}"),
            }
        }
        "/nhatrang" | "/nhatot" | "/danang" => {
            let city = if cmd == "/danang" {
                nhatot::City::DaNang
            } else {
                nhatot::City::NhaTrang
            };
            info!(user = %who, command = cmd, city = city.label(), "opening nhatot search UI");
            let filters = nhatot_search::Filters::for_city(city);
            let (text, keyboard) = nhatot_search::render(nhatot_search::Screen::Main, &filters);
            match telegram::send_keyboard(bot, chat_id, &text, keyboard).await {
                Ok(message) => {
                    sessions.insert((chat_id, message.message_id), Session::Nhatot(filters));
                }
                Err(e) => error!(user = %who, "failed to open nhatot search: {e:#}"),
            }
        }
        "/start" | "/help" => {
            info!(user = %who, command = cmd, "help requested");
            let _ = telegram::send_note(bot, chat_id, &help_text(cfg, chat_id)).await;
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
    let (chat, message_id) = match message {
        MaybeInaccessibleMessage::Message(m) => (&*m.chat, m.message_id),
        MaybeInaccessibleMessage::InaccessibleMessage(m) => (&m.chat, m.message_id),
    };
    if !chat_allowed(cfg, chat) {
        debug!(user = %who, chat_id = chat.id, "ignoring callback from non-target chat");
        return;
    }
    let chat_id = chat.id;
    debug!(user = %who, button = data, "button pressed");

    let key = (chat_id, message_id);
    let Some(session) = sessions.get(&key).cloned() else {
        debug!(user = %who, "callback for expired search session");
        let _ = telegram::edit_plain(
            bot,
            chat_id,
            message_id,
            "This search expired — send /search, /bostad, /nhatrang or /danang to start a new one.",
        )
        .await;
        return;
    };

    match session {
        Session::Qasa(mut filters) => match search::apply(&mut filters, data) {
            search::Action::Show(screen) => {
                let (text, keyboard) = search::render(screen, &filters);
                let _ = telegram::edit_keyboard(bot, chat_id, message_id, &text, keyboard).await;
                sessions.insert(key, Session::Qasa(filters));
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
        },
        Session::Bostad(mut filters) => match bostad_search::apply(&mut filters, data) {
            bostad_search::Action::Show(screen) => {
                let (text, keyboard) = bostad_search::render(screen, &filters);
                let _ = telegram::edit_keyboard(bot, chat_id, message_id, &text, keyboard).await;
                sessions.insert(key, Session::Bostad(filters));
            }
            bostad_search::Action::Search => {
                info!(
                    user = %who,
                    category = ?filters.category,
                    min_rooms = filters.min_rooms,
                    min_rent = ?filters.min_rent,
                    max_rent = ?filters.max_rent,
                    max_queue_years = ?filters.max_queue_years,
                    kommuner = %filters.kommun_summary(),
                    "bostad search triggered"
                );
                sessions.remove(&key);
                let _ = telegram::edit_plain(
                    bot,
                    chat_id,
                    message_id,
                    &format!("🔎 Searching…\n\n{}", bostad_search::describe(&filters)),
                )
                .await;
                tokio::spawn(run_bostad_search(
                    http.clone(),
                    bot.clone(),
                    cfg.clone(),
                    chat_id,
                    filters,
                ));
            }
            bostad_search::Action::Ignore => {}
        },
        Session::Nhatot(mut filters) => match nhatot_search::apply(&mut filters, data) {
            nhatot_search::Action::Show(screen) => {
                let (text, keyboard) = nhatot_search::render(screen, &filters);
                let _ = telegram::edit_keyboard(bot, chat_id, message_id, &text, keyboard).await;
                sessions.insert(key, Session::Nhatot(filters));
            }
            nhatot_search::Action::Search => {
                info!(
                    user = %who,
                    max_age_hours = ?filters.max_age_hours,
                    category = ?filters.category,
                    min_rooms = filters.min_rooms,
                    min_rent = ?filters.min_rent,
                    max_rent = ?filters.max_rent,
                    min_size = ?filters.min_size,
                    city = filters.city.label(),
                    places = %filters.place_summary(),
                    "nhatot search triggered"
                );
                sessions.remove(&key);
                let _ = telegram::edit_plain(
                    bot,
                    chat_id,
                    message_id,
                    &format!("🔎 Searching…\n\n{}", nhatot_search::describe(&filters)),
                )
                .await;
                tokio::spawn(run_nhatot_search(
                    http.clone(),
                    bot.clone(),
                    cfg.clone(),
                    chat_id,
                    filters,
                ));
            }
            nhatot_search::Action::Ignore => {}
        },
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

/// Fetch, filter, and post the results of a completed /bostad search.
async fn run_bostad_search(
    http: reqwest::Client,
    bot: Bot,
    cfg: Config,
    chat_id: i64,
    filters: bostad_search::Filters,
) {
    let ads = match bostad::fetch_all(&http, &cfg).await {
        Ok(v) => v,
        Err(e) => {
            error!("bostad search fetch failed: {e:#}");
            let _ = telegram::send_note(&bot, chat_id, "⚠️ Search failed, please try again.").await;
            return;
        }
    };

    let mut matches: Vec<bostad::Ad> = ads
        .into_iter()
        .filter(|ad| bostad_search::passes(&filters, ad))
        .collect();
    let total = matches.len();

    if total == 0 {
        info!(kommuner = %filters.kommun_summary(), "bostad search returned no matches");
        let _ = telegram::send_note(&bot, chat_id, "No matches for those filters.").await;
        return;
    }

    // Newest ads have the highest ids; keep the newest, post oldest-first.
    matches.sort_by_key(|ad| ad.annons_id);
    if matches.len() > RECENT_MAX_LISTINGS {
        matches = matches.split_off(matches.len() - RECENT_MAX_LISTINGS);
    }

    info!(
        kommuner = %filters.kommun_summary(),
        total_matches = total,
        posting = matches.len(),
        "bostad search complete"
    );

    for ad in &matches {
        if let Err(e) = telegram::send_bostad_listing(&bot, chat_id, ad).await {
            warn!(id = ad.annons_id, "failed to send bostad listing: {e:#}");
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

/// Fetch, filter, and post the results of a completed /nhatrang search.
async fn run_nhatot_search(
    http: reqwest::Client,
    bot: Bot,
    cfg: Config,
    chat_id: i64,
    filters: nhatot_search::Filters,
) {
    let ads = match nhatot::fetch_rent(
        &http,
        &cfg,
        filters.city,
        &filters.area_codes(),
        filters.category.cg(),
        NHATOT_SEARCH_PAGES,
    )
    .await
    {
        Ok(v) => v,
        Err(e) => {
            error!("nhatot search fetch failed: {e:#}");
            let _ = telegram::send_note(&bot, chat_id, "⚠️ Search failed, please try again.").await;
            return;
        }
    };

    let now_ms = OffsetDateTime::now_utc().unix_timestamp() * 1000;
    let mut matches: Vec<nhatot::Ad> = ads
        .into_iter()
        .filter(|ad| nhatot_search::passes(&filters, ad, now_ms))
        .collect();
    let total = matches.len();

    if total == 0 {
        info!(
            city = filters.city.label(),
            places = %filters.place_summary(),
            "nhatot search returned no matches"
        );
        let _ = telegram::send_note(&bot, chat_id, "No matches for those filters.").await;
        return;
    }

    // Keep the newest by listing time, post oldest-first.
    matches.sort_by_key(|ad| ad.list_time);
    if matches.len() > RECENT_MAX_LISTINGS {
        matches = matches.split_off(matches.len() - RECENT_MAX_LISTINGS);
    }

    info!(
        city = filters.city.label(),
        places = %filters.place_summary(),
        total_matches = total,
        posting = matches.len(),
        "nhatot search complete"
    );

    let translator = deepl::Translator::from_config(&http, &cfg);
    for ad in &matches {
        let ad = translate_ad(ad, translator.as_ref()).await;
        if let Err(e) = telegram::send_nhatot_listing(&bot, chat_id, &ad).await {
            warn!(id = ad.ad_id, "failed to send nhatot listing: {e:#}");
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

/// Copy of `ad` with its Vietnamese title and (pre-cut) description
/// translated to English when DeepL is configured. Any failure logs and
/// falls back to the original text, so a translation outage never drops an
/// ad.
async fn translate_ad(ad: &nhatot::Ad, translator: Option<&deepl::Translator>) -> nhatot::Ad {
    let mut out = ad.clone();
    let Some(translator) = translator else {
        return out;
    };
    let subject = ad.subject.as_deref().map(str::trim).unwrap_or("");
    let body = ad
        .body
        .as_deref()
        .map(|b| telegram::preview(b, TRANSLATE_BODY_CHARS))
        .unwrap_or_default();
    // DeepL rejects empty strings, so only send what exists.
    let mut texts: Vec<&str> = Vec::new();
    if !subject.is_empty() {
        texts.push(subject);
    }
    if !body.is_empty() {
        texts.push(&body);
    }
    if texts.is_empty() {
        return out;
    }
    match translator.translate(&texts).await {
        Ok(translated) => {
            let mut it = translated.into_iter();
            if !subject.is_empty() {
                out.subject = it.next();
            }
            if !body.is_empty() {
                out.body = it.next();
            }
            out.translated = true;
        }
        Err(e) => warn!(id = ad.ad_id, "translation failed, posting original: {e:#}"),
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        Config {
            bot_token: "t".to_string(),
            chat_id: 1,
            area: "se/stockholm".to_string(),
            home_types: vec!["apartment".to_string()],
            interval: Duration::from_secs(3 * 3600),
            max_notify: 40,
            endpoint: String::new(),
            bostad_chat_id: Some(2),
            bostad_interval: Duration::from_secs(10 * 60),
            bostad_endpoint: String::new(),
            nhatot_chat_id: Some(3),
            nhatot_interval: Duration::from_secs(30 * 60),
            nhatot_endpoint: String::new(),
            nhatot_cities: vec![nhatot::City::NhaTrang, nhatot::City::DaNang],
            nhatot_category: 1000,
            deepl_api_key: None,
            deepl_endpoint: None,
        }
    }

    #[test]
    fn help_mentions_only_this_chats_schedule() {
        let cfg = cfg();
        let qasa = help_text(&cfg, 1);
        assert!(qasa.contains("/nhatrang"));
        assert!(qasa.contains("Stockholm apartments from Qasa here every 3 hours"));
        assert!(!qasa.contains("Bostad snabbt ads here"));
        assert!(!qasa.contains("Nha Trang rentals from"));

        let bostad = help_text(&cfg, 2);
        assert!(bostad.contains("Bostad snabbt ads here every 10 minutes"));
        assert!(!bostad.contains("Qasa here"));

        let nhatot = help_text(&cfg, 3);
        assert!(nhatot.contains("Nha Trang and Da Nang rentals from Chợ Tốt here every 30 minutes"));
        assert!(nhatot.contains("/danang"));

        // A DM (or any other chat) gets the command list only.
        let dm = help_text(&cfg, 99);
        assert_eq!(dm, HELP_COMMANDS);
        assert!(!dm.contains("I also post"));
    }
}
