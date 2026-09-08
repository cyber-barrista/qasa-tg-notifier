//! Telegram side: sending listings and using a pinned message as the
//! persistent watermark store.
//!
//! Instead of a database or a Fly volume, the bot keeps a single pinned
//! message in the target chat holding the newest Qasa home id it has seen. On
//! boot it reads that back via `getChat`; each cycle it edits it in place.

use std::time::Duration;

use anyhow::{Context, Result};
use frankenstein::client_reqwest::Bot;
use frankenstein::methods::{
    AnswerCallbackQueryParams, EditMessageTextParams, GetChatParams, PinChatMessageParams,
    SendMessageParams,
};
use frankenstein::types::{InlineKeyboardButton, InlineKeyboardMarkup, Message, ReplyMarkup};
use frankenstein::AsyncTelegramApi;
use frankenstein::{Error as TgError, ParseMode};

use crate::bostad::Ad;
use crate::qasa::Home;

/// Marker used to locate the watermark inside the pinned message text.
const WATERMARK_KEY: &str = "watermark=";
/// Marker for the Bostadsförmedlingen seen-ids line in its chat's pinned
/// message. A set rather than a watermark because `AnnonsId` is not monotonic
/// with publish date; it stays tiny because it's pruned to live ads each cycle.
const SEEN_KEY: &str = "seen=";

/// How many times to retry a send after a 429 before giving up.
const MAX_SEND_ATTEMPTS: usize = 5;
/// Fallback wait when Telegram sends a 429 without a `retry_after`.
const DEFAULT_RETRY_SECS: u16 = 5;

/// Send a message, honoring Telegram's 429 `retry_after` by waiting and
/// retrying instead of dropping the message.
async fn send_message_retrying(bot: &Bot, params: &SendMessageParams) -> Result<Message> {
    for attempt in 1..=MAX_SEND_ATTEMPTS {
        match bot.send_message(params).await {
            Ok(response) => return Ok(response.result),
            Err(TgError::Api(resp)) if resp.error_code == 429 && attempt < MAX_SEND_ATTEMPTS => {
                let wait = resp
                    .parameters
                    .and_then(|p| p.retry_after)
                    .unwrap_or(DEFAULT_RETRY_SECS);
                tracing::warn!(attempt, wait, "rate limited by Telegram; waiting to retry");
                tokio::time::sleep(Duration::from_secs(u64::from(wait) + 1)).await;
            }
            Err(e) => return Err(e).context("send_message"),
        }
    }
    unreachable!("loop returns on the final attempt")
}

/// Parsed contents of the pinned state message.
pub struct State {
    pub watermark: u64,
    pub message_id: i32,
}

/// Read the watermark from the chat's pinned message, if any.
pub async fn read_state(bot: &Bot, chat_id: i64) -> Result<Option<State>> {
    let params = GetChatParams::builder().chat_id(chat_id).build();
    let chat = bot
        .get_chat(&params)
        .await
        .context("getChat failed")?
        .result;

    let Some(pinned) = chat.pinned_message else {
        return Ok(None);
    };
    let Some(text) = pinned.text.as_deref() else {
        return Ok(None);
    };
    let Some(watermark) = parse_watermark(text) else {
        return Ok(None);
    };
    Ok(Some(State {
        watermark,
        message_id: pinned.message_id,
    }))
}

/// Create-and-pin (first run) or edit-in-place the state message.
pub async fn write_state(
    bot: &Bot,
    chat_id: i64,
    existing_message_id: Option<i32>,
    watermark: u64,
) -> Result<()> {
    let text = format!(
        "📌 qasa-tg-notifier state\n{WATERMARK_KEY}{watermark}\nNewest Qasa home id seen — please don't unpin or delete."
    );

    match existing_message_id {
        Some(message_id) => {
            let params = EditMessageTextParams::builder()
                .chat_id(chat_id)
                .message_id(message_id)
                .text(text)
                .build();
            bot.edit_message_text(&params)
                .await
                .context("editing pinned state message")?;
        }
        None => {
            let send = SendMessageParams::builder()
                .chat_id(chat_id)
                .text(text)
                .disable_notification(true)
                .build();
            let message = send_message_retrying(bot, &send)
                .await
                .context("sending initial state message")?;

            let pin = PinChatMessageParams::builder()
                .chat_id(chat_id)
                .message_id(message.message_id)
                .disable_notification(true)
                .build();
            bot.pin_chat_message(&pin)
                .await
                .context("pinning state message")?;
        }
    }
    Ok(())
}

/// Parsed contents of the bostad chat's pinned state message.
pub struct BostadState {
    pub seen: std::collections::BTreeSet<u64>,
    pub message_id: i32,
}

/// Read the seen-ids set from the bostad chat's pinned message, if any.
pub async fn read_bostad_state(bot: &Bot, chat_id: i64) -> Result<Option<BostadState>> {
    let params = GetChatParams::builder().chat_id(chat_id).build();
    let chat = bot
        .get_chat(&params)
        .await
        .context("getChat failed")?
        .result;

    let Some(pinned) = chat.pinned_message else {
        return Ok(None);
    };
    let Some(text) = pinned.text.as_deref() else {
        return Ok(None);
    };
    let Some(seen) = parse_seen(text) else {
        return Ok(None);
    };
    Ok(Some(BostadState {
        seen,
        message_id: pinned.message_id,
    }))
}

/// Create-and-pin (first run) or edit-in-place the bostad state message.
pub async fn write_bostad_state(
    bot: &Bot,
    chat_id: i64,
    existing_message_id: Option<i32>,
    seen: &std::collections::BTreeSet<u64>,
) -> Result<()> {
    let ids: Vec<String> = seen.iter().map(u64::to_string).collect();
    let text = format!(
        "📌 bostad-snabbt notifier state\n{SEEN_KEY}{}\nLive Bostad snabbt ad ids already notified — please don't unpin or delete.",
        ids.join(",")
    );

    match existing_message_id {
        Some(message_id) => {
            let params = EditMessageTextParams::builder()
                .chat_id(chat_id)
                .message_id(message_id)
                .text(text)
                .build();
            bot.edit_message_text(&params)
                .await
                .context("editing pinned bostad state message")?;
        }
        None => {
            let send = SendMessageParams::builder()
                .chat_id(chat_id)
                .text(text)
                .disable_notification(true)
                .build();
            let message = send_message_retrying(bot, &send)
                .await
                .context("sending initial bostad state message")?;

            let pin = PinChatMessageParams::builder()
                .chat_id(chat_id)
                .message_id(message.message_id)
                .disable_notification(true)
                .build();
            bot.pin_chat_message(&pin)
                .await
                .context("pinning bostad state message")?;
        }
    }
    Ok(())
}

/// Send a single listing as an HTML message with an "Open on Qasa" URL button.
pub async fn send_listing(bot: &Bot, chat_id: i64, home: &Home) -> Result<()> {
    let open_button = InlineKeyboardButton::builder()
        .text("🔗 Open on Qasa")
        .url(listing_url(home))
        .build();
    let markup = InlineKeyboardMarkup {
        inline_keyboard: vec![vec![open_button]],
    };
    let params = SendMessageParams::builder()
        .chat_id(chat_id)
        .text(format_listing(home))
        .parse_mode(ParseMode::Html)
        .reply_markup(ReplyMarkup::InlineKeyboardMarkup(markup))
        .build();
    send_message_retrying(bot, &params)
        .await
        .context("sending listing")?;
    Ok(())
}

/// Send a single Bostadsförmedlingen ad as an HTML message with a URL button.
pub async fn send_bostad_listing(bot: &Bot, chat_id: i64, ad: &Ad) -> Result<()> {
    let open_button = InlineKeyboardButton::builder()
        .text("🔗 Open on Bostadsförmedlingen")
        .url(ad.full_url())
        .build();
    let markup = InlineKeyboardMarkup {
        inline_keyboard: vec![vec![open_button]],
    };
    let params = SendMessageParams::builder()
        .chat_id(chat_id)
        .text(format_bostad_listing(ad))
        .parse_mode(ParseMode::Html)
        .reply_markup(ReplyMarkup::InlineKeyboardMarkup(markup))
        .build();
    send_message_retrying(bot, &params)
        .await
        .context("sending bostad listing")?;
    Ok(())
}

/// Send a plain informational note (e.g. the "…and N more" summary).
pub async fn send_note(bot: &Bot, chat_id: i64, text: &str) -> Result<()> {
    let params = SendMessageParams::builder()
        .chat_id(chat_id)
        .text(text.to_string())
        .disable_notification(true)
        .build();
    send_message_retrying(bot, &params)
        .await
        .context("sending note")?;
    Ok(())
}

/// Send a message carrying an inline keyboard; returns the sent message so the
/// caller can key session state on its id.
pub async fn send_keyboard(
    bot: &Bot,
    chat_id: i64,
    text: &str,
    markup: InlineKeyboardMarkup,
) -> Result<Message> {
    let params = SendMessageParams::builder()
        .chat_id(chat_id)
        .text(text.to_string())
        .reply_markup(ReplyMarkup::InlineKeyboardMarkup(markup))
        .disable_notification(true)
        .build();
    send_message_retrying(bot, &params)
        .await
        .context("sending keyboard message")
}

/// Replace a message's text and inline keyboard in place.
pub async fn edit_keyboard(
    bot: &Bot,
    chat_id: i64,
    message_id: i32,
    text: &str,
    markup: InlineKeyboardMarkup,
) -> Result<()> {
    let params = EditMessageTextParams::builder()
        .chat_id(chat_id)
        .message_id(message_id)
        .text(text.to_string())
        .reply_markup(markup)
        .build();
    bot.edit_message_text(&params)
        .await
        .context("editing keyboard message")?;
    Ok(())
}

/// Replace a message's text and drop its inline keyboard.
pub async fn edit_plain(bot: &Bot, chat_id: i64, message_id: i32, text: &str) -> Result<()> {
    let params = EditMessageTextParams::builder()
        .chat_id(chat_id)
        .message_id(message_id)
        .text(text.to_string())
        .build();
    bot.edit_message_text(&params)
        .await
        .context("editing message")?;
    Ok(())
}

/// Acknowledge a callback query so the client stops showing a spinner.
pub async fn answer_callback(bot: &Bot, callback_query_id: &str) -> Result<()> {
    let params = AnswerCallbackQueryParams::builder()
        .callback_query_id(callback_query_id.to_string())
        .build();
    bot.answer_callback_query(&params)
        .await
        .context("answering callback query")?;
    Ok(())
}

fn parse_watermark(text: &str) -> Option<u64> {
    let idx = text.find(WATERMARK_KEY)?;
    let rest = &text[idx + WATERMARK_KEY.len()..];
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// Parse the comma-separated seen-id set. An empty list (`seen=` alone, the
/// state after every live ad expires) is valid and distinct from "no state".
fn parse_seen(text: &str) -> Option<std::collections::BTreeSet<u64>> {
    let idx = text.find(SEEN_KEY)?;
    let rest = &text[idx + SEEN_KEY.len()..];
    let line = rest.lines().next().unwrap_or("");
    Some(
        line.split(',')
            .filter_map(|s| s.trim().parse().ok())
            .collect(),
    )
}

/// Escape the five characters that matter for Telegram's HTML parse mode.
fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn format_listing(home: &Home) -> String {
    let loc = home.location.as_ref();
    let locality = loc.and_then(|l| l.locality.clone()).unwrap_or_default();

    let street = loc
        .map(|l| match (&l.route, &l.street_number) {
            (Some(route), Some(number)) => format!("{route} {number}"),
            (Some(route), None) => route.clone(),
            _ => String::new(),
        })
        .unwrap_or_default();

    let headline = if !street.is_empty() {
        street
    } else if let Some(title) = &home.title {
        title.clone()
    } else if !locality.is_empty() {
        locality.clone()
    } else {
        "Home".to_string()
    };

    let mut lines = Vec::new();

    // Headline, with locality appended when it adds information.
    if !locality.is_empty() && locality != headline {
        lines.push(format!("🏠 <b>{}</b>, {}", esc(&headline), esc(&locality)));
    } else {
        lines.push(format!("🏠 <b>{}</b>", esc(&headline)));
    }

    // Price.
    let currency = home.currency.as_deref().unwrap_or("SEK");
    if let Some(rent) = home.rent {
        let mut price = format!("💰 {rent} {currency}/mo");
        if let Some(total) = home.monthly_cost {
            if total != rent {
                price.push_str(&format!(" (total {total})"));
            }
        }
        lines.push(price);
    }

    // Size + rooms.
    let mut size = String::new();
    if let Some(sqm) = home.square_meters {
        size.push_str(&format!("📐 {} m²", fmt_num(sqm)));
    }
    if let Some(rooms) = home.room_count {
        if !size.is_empty() {
            size.push_str(" · ");
        } else {
            size.push_str("📐 ");
        }
        size.push_str(&format!("{} rooms", fmt_num(rooms)));
    }
    if !size.is_empty() {
        lines.push(size);
    }

    // Tags: home type / first-hand / source platform.
    let mut tags = Vec::new();
    if let Some(home_type) = &home.home_type {
        tags.push(esc(home_type));
    }
    if home.first_hand == Some(true) {
        tags.push("first-hand".to_string());
    }
    if let Some(platform) = &home.platform {
        tags.push(format!("via {}", esc(platform)));
    }
    if !tags.is_empty() {
        lines.push(format!("🏷 {}", tags.join(" · ")));
    }

    lines.join("\n")
}

/// Public listing page for a home.
fn listing_url(home: &Home) -> String {
    format!("https://qasa.com/se/en/home/{}", home.id)
}

fn format_bostad_listing(ad: &Ad) -> String {
    let headline = ad
        .gatuadress
        .clone()
        .or_else(|| ad.stadsdel.clone())
        .unwrap_or_else(|| "Apartment".to_string());
    // Append the district/kommun where they add information.
    let mut place = Vec::new();
    if let Some(stadsdel) = &ad.stadsdel {
        if *stadsdel != headline {
            place.push(stadsdel.clone());
        }
    }
    if let Some(kommun) = &ad.kommun {
        if !place.contains(kommun) {
            place.push(kommun.clone());
        }
    }

    let mut lines = Vec::new();
    if place.is_empty() {
        lines.push(format!("🏠 <b>{}</b>", esc(&headline)));
    } else {
        lines.push(format!(
            "🏠 <b>{}</b>, {}",
            esc(&headline),
            esc(&place.join(", "))
        ));
    }

    if let Some(rent) = ad.rent() {
        lines.push(format!("💰 {rent} SEK/mo"));
    }

    let mut size = String::new();
    if let Some(sqm) = ad.sqm() {
        size.push_str(&format!("📐 {} m²", fmt_num(sqm)));
    }
    if let Some(rooms) = ad.antal_rum {
        if !size.is_empty() {
            size.push_str(" · ");
        } else {
            size.push_str("📐 ");
        }
        size.push_str(&format!("{} rooms", fmt_num(rooms)));
    }
    if !size.is_empty() {
        lines.push(size);
    }

    let mut tags = Vec::new();
    if ad.bostad_snabbt {
        tags.push("⚡ Bostad snabbt — first come, first served".to_string());
    }
    if ad.student {
        tags.push("student".to_string());
    }
    if ad.ungdom {
        tags.push("ungdom".to_string());
    }
    if ad.senior {
        tags.push("senior".to_string());
    }
    if ad.korttid {
        tags.push("short-term".to_string());
    }
    // "Short queue time" only means something for queue-allocated ads.
    if ad.kort_kotid && !ad.bostad_snabbt {
        tags.push("short queue".to_string());
    }
    if ad.nyproduktion {
        tags.push("new build".to_string());
    }
    if let Some(t) = &ad.lagenhetstyp {
        tags.push(esc(t));
    }
    if !tags.is_empty() {
        lines.push(format!("🏷 {}", tags.join(" · ")));
    }

    // Queue-years range of recent comparable lettings; meaningless for the
    // first-come-first-served snabbt ads.
    if !ad.bostad_snabbt {
        match (ad.queue_q1(), ad.queue_q3()) {
            (Some(q1), Some(q3)) if q1 != q3 => {
                lines.push(format!("⏳ queue ~{q1}–{q3} yrs"));
            }
            (Some(q1), _) => lines.push(format!("⏳ queue ~{q1} yrs")),
            _ => {}
        }
    }

    // These ads close quickly, so the deadline matters.
    if let Some(till) = &ad.annonserad_till {
        lines.push(format!("⏰ apply by {}", esc(till)));
    }

    lines.join("\n")
}

/// Render a possibly-fractional number without a trailing `.0`.
fn fmt_num(n: f64) -> String {
    if (n.fract()).abs() < f64::EPSILON {
        format!("{}", n as i64)
    } else {
        // Trim to one decimal; room counts like 1.5 are the realistic case.
        format!("{n:.1}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_watermark_from_state_text() {
        let text = "📌 qasa-tg-notifier state\nwatermark=1433991\nfoo";
        assert_eq!(parse_watermark(text), Some(1_433_991));
    }

    #[test]
    fn missing_watermark_is_none() {
        assert_eq!(parse_watermark("no marker here"), None);
    }

    #[test]
    fn parses_seen_set_from_state_text() {
        let text = "📌 bostad-snabbt notifier state\nseen=301449,301581,301744\nfoo";
        let seen = parse_seen(text).unwrap();
        assert_eq!(seen.len(), 3);
        assert!(seen.contains(&301_449));
        // An empty list is valid state (all live ads expired), not "no state".
        assert_eq!(parse_seen("seen=\nrest"), Some(Default::default()));
        assert_eq!(parse_seen("no marker here"), None);
    }

    #[test]
    fn formats_bostad_listing() {
        let ad = crate::bostad::Ad {
            annons_id: 301_449,
            gatuadress: Some("Njupkärrsvägen 5".to_string()),
            stadsdel: Some("Bollmora".to_string()),
            kommun: Some("Tyresö".to_string()),
            antal_rum: Some(1.0),
            yta: Some(33.0),
            hyra: Some(9_074),
            lagsta_hyran: None,
            hogsta_hyran: None,
            lagsta_ytan: None,
            hogsta_ytan: None,
            annonserad_till: Some("2026-09-03".to_string()),
            url: Some("/bostad/202615120/".to_string()),
            lagenhetstyp: Some("Hyresrätt".to_string()),
            nyproduktion: true,
            student: false,
            ungdom: false,
            senior: false,
            korttid: false,
            vanlig: false,
            bostad_snabbt: true,
            kort_kotid: false,
            liknade_lagenhet_statistik: Some(crate::bostad::KotidStatistik {
                kotid_fordelning_q1: Some(2),
                kotid_fordelning_q3: Some(4),
            }),
        };
        let out = format_bostad_listing(&ad);
        assert!(out.contains("<b>Njupkärrsvägen 5</b>, Bollmora, Tyresö"));
        assert!(out.contains("9074 SEK/mo"));
        assert!(out.contains("33 m² · 1 rooms"));
        // Snabbt ads bypass the queue, so their stats are not shown…
        assert!(!out.contains("queue ~"));
        // …but queue-allocated ads show the range.
        let mut regular = ad.clone();
        regular.bostad_snabbt = false;
        assert!(format_bostad_listing(&regular).contains("⏳ queue ~2–4 yrs"));
        assert!(out.contains("Bostad snabbt"));
        assert!(out.contains("apply by 2026-09-03"));
        assert_eq!(
            ad.full_url(),
            "https://bostad.stockholm.se/bostad/202615120/"
        );
    }

    #[test]
    fn escapes_html_in_listing() {
        let home = Home {
            id: "42".to_string(),
            title: Some("A & B <loft>".to_string()),
            rent: Some(12000),
            currency: Some("SEK".to_string()),
            monthly_cost: None,
            room_count: Some(2.0),
            square_meters: Some(45.0),
            home_type: Some("apartment".to_string()),
            first_hand: Some(true),
            platform: Some("dotcom".to_string()),
            published_at: None,
            published_or_bumped_at: None,
            location: None,
        };
        let out = format_listing(&home);
        assert!(out.contains("A &amp; B &lt;loft&gt;"));
        assert!(out.contains("2 rooms"));
        assert!(out.contains("45 m²"));
        assert_eq!(listing_url(&home), "https://qasa.com/se/en/home/42");
    }
}
