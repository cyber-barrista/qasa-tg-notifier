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

use crate::qasa::Home;

/// Marker used to locate the watermark inside the pinned message text.
const WATERMARK_KEY: &str = "watermark=";

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
