//! Client for Chợ Tốt's public ad-listing API — the JSON backend of
//! nhatot.com (the property section of Chợ Tốt, Vietnam's Blocket).
//!
//! `GET https://gateway.chotot.com/v1/public/ad-listing` is unauthenticated
//! and, unlike the nhatot.com HTML (Cloudflare managed challenge), serves a
//! plain HTTP client. Verified against the live API on 2026-10-04:
//!
//! - `region_v2=7044` is Khánh Hòa and `area_v2=704401` Thành phố Nha Trang
//!   (codes from `GET https://gateway.chotot.com/v2/public/chapy-pro/regions`).
//! - `cg` is the category: 1010 Căn hộ/Chung cư (apartments), 1020 Nhà ở
//!   (houses), 1050 Phòng trọ (rooms), 1000 all real estate (also offices and
//!   land). `st=u` means for rent, `s` for sale.
//! - `limit` is capped at 50 server-side (asking for more returns 50); `o` is
//!   the offset. The response is `{"ads": [...], "total": N}` ordered by
//!   `list_time` descending.
//! - Ads get *bumped*: `list_time` is rewritten and `orig_list_time` keeps the
//!   original. Neither `ad_id` nor `list_id` is monotonic with `list_time`,
//!   so dedup is a seen-id set, not a watermark — see `main::run_nhatot_cycle`.
//!
//! As with the other clients the schema is undocumented, so every field but
//! `ad_id` is `Option` + `#[serde(default)]`.

use anyhow::{Context, Result};
use serde::Deserialize;
use time::OffsetDateTime;

use crate::config::Config;

/// Server-side page cap.
pub const PAGE_LIMIT: usize = 50;

/// `cg` codes we care about.
pub const CATEGORY_APARTMENT: u32 = 1010;
pub const CATEGORY_HOUSE: u32 = 1020;
/// All real estate; filter client-side with [`Ad::is_home`].
pub const CATEGORY_ALL: u32 = 1000;

/// A single Chợ Tốt ad.
#[derive(Deserialize, Debug, Clone)]
pub struct Ad {
    pub ad_id: u64,
    /// Id used in the public URL (`https://www.nhatot.com/{list_id}.htm`).
    #[serde(default)]
    pub list_id: u64,
    /// Epoch milliseconds; rewritten when the ad is bumped.
    #[serde(default)]
    pub list_time: i64,
    /// Original `list_time`, present only once an ad has been bumped.
    #[serde(default)]
    pub orig_list_time: Option<i64>,
    #[serde(default)]
    pub subject: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
    /// Monthly rent in VND.
    #[serde(default)]
    pub price: Option<i64>,
    /// Human form, e.g. `15 triệu/tháng`.
    #[serde(default)]
    pub price_string: Option<String>,
    #[serde(default)]
    pub rooms: Option<u8>,
    /// Living area in m² (the API emits integers and floats interchangeably).
    #[serde(default)]
    pub size: Option<f64>,
    #[serde(default)]
    pub ward_name: Option<String>,
    /// Street; sellers sometimes paste the whole address here, so the
    /// formatter dedups it against ward/area/region.
    #[serde(default)]
    pub street_name: Option<String>,
    /// City, e.g. `Thành phố Nha Trang`.
    #[serde(default)]
    pub area_name: Option<String>,
    /// Province, e.g. `Khánh Hòa`.
    #[serde(default)]
    pub region_name: Option<String>,
    /// Building/project name; often an empty string.
    #[serde(default)]
    pub pty_project_name: Option<String>,
    #[serde(default)]
    pub category: Option<u32>,
    #[serde(default)]
    pub category_name: Option<String>,
    /// Set once `subject`/`body` have been machine-translated to English.
    #[serde(skip)]
    pub translated: bool,
}

#[derive(Deserialize, Debug)]
struct Listing {
    #[serde(default)]
    ads: Vec<Ad>,
    #[serde(default)]
    total: u64,
}

impl Ad {
    /// Public listing page (redirects to the canonical slug URL).
    pub fn url(&self) -> String {
        format!("https://www.nhatot.com/{}.htm", self.list_id)
    }

    /// Has the owner re-promoted this ad since it was first listed?
    pub fn is_bumped(&self) -> bool {
        self.orig_list_time.is_some_and(|o| o != self.list_time)
    }

    /// When the ad was first listed (falls back to the current `list_time`).
    pub fn first_listed_at(&self) -> Option<OffsetDateTime> {
        let ms = self.orig_list_time.unwrap_or(self.list_time);
        OffsetDateTime::from_unix_timestamp(ms / 1000).ok()
    }

    /// Apartment or whole house — what the notifier pushes. `cg=1000` also
    /// returns offices, shop fronts and land, which this drops.
    pub fn is_home(&self) -> bool {
        matches!(self.category, Some(CATEGORY_APARTMENT | CATEGORY_HOUSE))
    }

    /// Project/building name, `None` when absent or blank.
    pub fn project(&self) -> Option<&str> {
        self.pty_project_name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }
}

/// Fetch the newest `pages` × [`PAGE_LIMIT`] for-rent ads in the configured
/// region/area and `category` (a `cg` code), newest first as the API returns
/// them. Stops early once the listing is exhausted.
pub async fn fetch_rent(
    client: &reqwest::Client,
    cfg: &Config,
    category: u32,
    pages: usize,
) -> Result<Vec<Ad>> {
    let mut ads: Vec<Ad> = Vec::new();
    for page in 0..pages {
        // Hand-built query string: every value is a numeric code, and reqwest
        // is compiled without its `query` feature.
        let url = format!(
            "{}?region_v2={}&area_v2={}&cg={category}&st=u&limit={PAGE_LIMIT}&o={}",
            cfg.nhatot_endpoint,
            cfg.nhatot_region,
            cfg.nhatot_area,
            page * PAGE_LIMIT
        );
        let listing: Listing = client
            .get(&url)
            .send()
            .await
            .context("requesting Chợ Tốt ad listing")?
            .error_for_status()?
            .json()
            .await
            .context("decoding Chợ Tốt ad listing")?;
        let page_len = listing.ads.len();
        ads.extend(listing.ads);
        if !more_pages(ads.len(), page_len, listing.total) {
            break;
        }
    }
    Ok(ads)
}

/// Is there another page worth fetching after this one?
fn more_pages(so_far: usize, page_len: usize, total: u64) -> bool {
    page_len == PAGE_LIMIT && (so_far as u64) < total
}

#[cfg(test)]
mod tests {
    use super::*;

    // Captured from the live API (trimmed to the fields we consume plus a few
    // extras). The first ad has been bumped; the second is minimal.
    const SAMPLE: &str = r#"{
      "ads": [
        {
          "ad_id": 179067567, "list_id": 134936730,
          "list_time": 1791136000000, "orig_list_time": 1790570223000,
          "subject": "cho thuê căn hộ 2 phòng ngủ Bắc Nha Trang",
          "body": "Căn hộ 2 phòng ngủ, full nội thất.\nGần biển.",
          "price": 14000000, "price_string": "14 triệu/tháng",
          "rooms": 2, "size": 63,
          "ward_name": "Phường Vĩnh Hòa", "street_name": "Trịnh hoài đức",
          "pty_project_name": "", "area_name": "Thành phố Nha Trang",
          "region_name": "Khánh Hòa", "category": 1010,
          "category_name": "Căn hộ/Chung cư", "type": "u",
          "image": "https://cdn.chotot.com/x.jpg", "is_sticky": false,
          "params": [], "seller_info": {"full_name": "A"}
        },
        {
          "ad_id": 179213542, "list_id": 135058712, "list_time": 1791143788000,
          "subject": "CHO THUÊ CĂN HỘ 2 PHÒNG NGỦ", "price": 20000000,
          "size": 74.5, "category": 1020, "type": "u"
        }
      ],
      "total": 61
    }"#;

    #[test]
    fn parses_sample_listing() {
        let listing: Listing = serde_json::from_str(SAMPLE).unwrap();
        assert_eq!(listing.total, 61);
        assert_eq!(listing.ads.len(), 2);

        let bumped = &listing.ads[0];
        assert_eq!(bumped.ad_id, 179_067_567);
        assert_eq!(bumped.url(), "https://www.nhatot.com/134936730.htm");
        assert!(bumped.is_bumped());
        assert_eq!(bumped.size, Some(63.0)); // integer JSON into f64
        assert_eq!(bumped.rooms, Some(2));
        assert_eq!(bumped.ward_name.as_deref(), Some("Phường Vĩnh Hòa"));
        assert_eq!(bumped.project(), None); // blank project name is None
        assert_eq!(bumped.region_name.as_deref(), Some("Khánh Hòa"));
        assert!(!bumped.translated);
        assert!(bumped.is_home());
        let first = bumped.first_listed_at().unwrap();
        assert_eq!(first.unix_timestamp(), 1_790_570_223);

        let minimal = &listing.ads[1];
        assert!(!minimal.is_bumped());
        assert_eq!(minimal.rooms, None);
        assert_eq!(minimal.size, Some(74.5));
        assert_eq!(minimal.price_string, None);
        assert!(minimal.is_home()); // a house
    }

    /// Hits the real API; run with `cargo test -- --ignored`.
    #[tokio::test]
    #[ignore = "network"]
    async fn live_fetch_rent_returns_nha_trang_homes() {
        std::env::set_var("BOT_TOKEN", "test");
        std::env::set_var("CHAT_ID", "1");
        let cfg = Config::from_env().unwrap();
        let client = reqwest::Client::new();
        let ads = fetch_rent(&client, &cfg, CATEGORY_ALL, 2).await.unwrap();
        assert!(
            ads.len() > PAGE_LIMIT,
            "expected two pages, got {}",
            ads.len()
        );
        let homes: Vec<&Ad> = ads.iter().filter(|a| a.is_home()).collect();
        assert!(!homes.is_empty());
        assert!(homes.iter().all(|a| a.list_id > 0 && a.list_time > 0));
        assert!(homes.iter().all(|a| a.price.is_some()));
        // Newest first, as documented.
        assert!(ads.windows(2).all(|w| w[0].list_time >= w[1].list_time));
    }

    #[test]
    fn non_home_categories_are_dropped() {
        let listing: Listing = serde_json::from_str(SAMPLE).unwrap();
        let mut office = listing.ads[0].clone();
        office.category = Some(1030);
        assert!(!office.is_home());
        office.category = None;
        assert!(!office.is_home());
    }

    #[test]
    fn more_pages_stops_on_short_page_and_total() {
        assert!(more_pages(50, 50, 61));
        assert!(!more_pages(61, 11, 61)); // short page
        assert!(!more_pages(50, 50, 50)); // total reached
        assert!(!more_pages(0, 0, 0));
    }
}
