//! Client for Qasa's public, unauthenticated GraphQL API.
//!
//! We issue the site's own `HomeSearch` operation and deserialize only the
//! fields we display. The schema is undocumented and drift-prone, so every
//! node field is `Option` + `#[serde(default)]`: added or removed sibling
//! fields never break parsing, and a shape change surfaces as empty data
//! rather than a hard error.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

use crate::config::Config;

/// The exact `HomeSearch` operation the qasa.com frontend fires, trimmed to
/// the fields we consume.
const HOME_SEARCH_QUERY: &str = r#"query HomeSearch($order: HomeIndexSearchOrderInput, $offset: Int, $limit: Int, $params: HomeSearchParamsInput) {
  homeIndexSearch(order: $order, params: $params) {
    documents(offset: $offset, limit: $limit) {
      totalCount
      nodes {
        id
        title
        rent
        currency
        monthlyCost
        roomCount
        squareMeters
        homeType
        firstHand
        platform
        publishedAt
        publishedOrBumpedAt
        location {
          locality
          route
          streetNumber
        }
      }
    }
  }
}"#;

const PAGE_LIMIT: i64 = 50;
/// Safety cap on pages fetched per cycle. Listings are ordered by
/// `published_or_bumped_at` descending, so anything genuinely new clusters at
/// the top; six pages (300 listings) is far more than a 3-hour window yields.
const MAX_PAGES: i64 = 6;

// ---- request types ---------------------------------------------------------

#[derive(Serialize)]
struct GqlRequest<'a> {
    query: &'a str,
    variables: Variables,
    #[serde(rename = "operationName")]
    operation_name: &'a str,
}

#[derive(Serialize)]
struct Variables {
    offset: i64,
    limit: i64,
    order: Order,
    params: Params,
}

#[derive(Serialize)]
struct Order {
    direction: &'static str,
    #[serde(rename = "orderBy")]
    order_by: &'static str,
}

#[derive(Serialize)]
struct Params {
    currency: &'static str,
    #[serde(rename = "areaIdentifier")]
    area_identifier: Vec<String>,
    markets: Vec<&'static str>,
    #[serde(rename = "homeType")]
    home_type: Vec<String>,
    #[serde(rename = "rentalType")]
    rental_type: Vec<&'static str>,
    /// Exclude single rooms in shared flats (which Qasa still tags
    /// `homeType: apartment` but marks `shared: true`).
    shared: bool,
}

// ---- response types --------------------------------------------------------

#[derive(Deserialize)]
struct GqlResponse<T> {
    data: Option<T>,
    #[serde(default)]
    errors: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
struct SearchData {
    #[serde(rename = "homeIndexSearch")]
    home_index_search: HomeIndexSearch,
}

#[derive(Deserialize)]
struct HomeIndexSearch {
    documents: Documents,
}

#[derive(Deserialize)]
struct Documents {
    #[serde(default)]
    nodes: Vec<Home>,
}

/// A single listing. Every field is optional to tolerate schema drift.
#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Home {
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub rent: Option<i64>,
    #[serde(default)]
    pub currency: Option<String>,
    #[serde(default)]
    pub monthly_cost: Option<i64>,
    #[serde(default)]
    pub room_count: Option<f64>,
    #[serde(default)]
    pub square_meters: Option<f64>,
    #[serde(default)]
    pub home_type: Option<String>,
    #[serde(default)]
    pub first_hand: Option<bool>,
    #[serde(default)]
    pub platform: Option<String>,
    #[serde(default)]
    pub published_at: Option<String>,
    #[serde(default)]
    pub published_or_bumped_at: Option<String>,
    #[serde(default)]
    pub location: Option<Location>,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Location {
    #[serde(default)]
    pub locality: Option<String>,
    #[serde(default)]
    pub route: Option<String>,
    #[serde(default)]
    pub street_number: Option<String>,
}

impl Home {
    /// Qasa home ids are monotonic integers; we use them as the watermark.
    pub fn id_num(&self) -> Option<u64> {
        self.id.parse().ok()
    }

    /// When the listing was first published, parsed from RFC 3339.
    pub fn published_at_dt(&self) -> Option<OffsetDateTime> {
        parse_dt(self.published_at.as_deref())
    }

    /// Most recent publish-or-bump time; used to stop time-window paging.
    pub fn published_or_bumped_dt(&self) -> Option<OffsetDateTime> {
        parse_dt(self.published_or_bumped_at.as_deref())
    }
}

fn parse_dt(s: Option<&str>) -> Option<OffsetDateTime> {
    s.and_then(|s| OffsetDateTime::parse(s, &Rfc3339).ok())
}

/// Result of a fetch: the listings newer than the watermark, and the highest
/// id observed (the new watermark).
pub struct Fetched {
    pub new: Vec<Home>,
    pub max_id: u64,
}

/// Fetch listings, returning those with an id greater than `watermark`.
///
/// When `watermark` is `None` (first run) we fetch a single page purely to
/// establish the watermark and return no listings — this avoids notifying on
/// the entire existing board.
pub async fn fetch_new(
    client: &reqwest::Client,
    cfg: &Config,
    watermark: Option<u64>,
) -> Result<Fetched> {
    let seeding = watermark.is_none();
    let mut new = Vec::new();
    let mut max_id = watermark.unwrap_or(0);

    let pages = if seeding { 1 } else { MAX_PAGES };
    for page in 0..pages {
        let offset = page * PAGE_LIMIT;
        let area_ids = [cfg.area.clone()];
        let data = fetch_page(client, cfg, &area_ids, offset, PAGE_LIMIT)
            .await
            .with_context(|| format!("fetching page at offset {offset}"))?;

        let nodes = data.home_index_search.documents.nodes;
        if nodes.is_empty() {
            break;
        }

        let mut any_new = false;
        for home in nodes {
            let Some(id) = home.id_num() else {
                tracing::warn!(id = %home.id, "skipping listing with non-numeric id");
                continue;
            };
            max_id = max_id.max(id);
            if let Some(wm) = watermark {
                if id > wm {
                    new.push(home);
                    any_new = true;
                }
            }
        }

        // Sorted by recency, so once a full page yields nothing new, later
        // pages won't either.
        if !seeding && !any_new {
            break;
        }
    }

    Ok(Fetched { new, max_id })
}

/// Safety cap on pages scanned for a `/recent` command (wider than the
/// periodic cycle, since the window can be large).
const COMMAND_MAX_PAGES: i64 = 20;

/// Fetch listings first published at or after `cutoff`, newest last.
///
/// Listings are ordered by `published_or_bumped_at` descending, so we page
/// until an entire page is older than `cutoff`, then stop. We filter on
/// `publishedAt` (not bump time) so a recently *bumped* old listing is not
/// reported as new. At most `max` listings are returned (the newest ones).
pub async fn fetch_recent(
    client: &reqwest::Client,
    cfg: &Config,
    areas: &[&str],
    cutoff: OffsetDateTime,
    max: usize,
) -> Result<Vec<Home>> {
    let area_ids: Vec<String> = areas.iter().map(|s| (*s).to_string()).collect();
    let mut out: Vec<Home> = Vec::new();

    for page in 0..COMMAND_MAX_PAGES {
        let offset = page * PAGE_LIMIT;
        let data = fetch_page(client, cfg, &area_ids, offset, PAGE_LIMIT)
            .await
            .with_context(|| format!("fetching page at offset {offset}"))?;

        let nodes = data.home_index_search.documents.nodes;
        if nodes.is_empty() {
            break;
        }

        // A page is exhausted only when every entry's bump time is older than
        // the cutoff. Unparseable timestamps keep us scanning (conservative).
        let mut page_has_fresh = false;
        for home in nodes {
            if home.published_or_bumped_dt().is_none_or(|b| b >= cutoff) {
                page_has_fresh = true;
            }
            if home.published_at_dt().is_some_and(|p| p >= cutoff) {
                out.push(home);
            }
        }

        if !page_has_fresh || out.len() >= max {
            break;
        }
    }

    // Keep the newest `max`, then present oldest-first for chronological posting.
    out.sort_by_key(|h| std::cmp::Reverse(h.published_at_dt()));
    out.truncate(max);
    out.reverse();
    Ok(out)
}

async fn fetch_page(
    client: &reqwest::Client,
    cfg: &Config,
    area_ids: &[String],
    offset: i64,
    limit: i64,
) -> Result<SearchData> {
    let body = GqlRequest {
        query: HOME_SEARCH_QUERY,
        operation_name: "HomeSearch",
        variables: Variables {
            offset,
            limit,
            order: Order {
                direction: "descending",
                order_by: "published_or_bumped_at",
            },
            params: Params {
                currency: "SEK",
                area_identifier: area_ids.to_vec(),
                markets: vec!["sweden"],
                home_type: cfg.home_types.clone(),
                rental_type: vec!["long_term"],
                shared: false,
            },
        },
    };

    let resp: GqlResponse<SearchData> = client
        .post(&cfg.endpoint)
        .json(&body)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await
        .context("decoding GraphQL response")?;

    if !resp.errors.is_empty() {
        bail!("Qasa GraphQL returned errors: {:?}", resp.errors);
    }
    resp.data.context("GraphQL response had no data")
}

#[cfg(test)]
mod tests {
    use super::*;

    // A captured-shape sample; guards against silent schema-drift breakage.
    const SAMPLE: &str = r#"{
      "data": {
        "homeIndexSearch": {
          "documents": {
            "totalCount": 2,
            "nodes": [
              {
                "id": "1433991", "title": null, "rent": 18200, "currency": "SEK",
                "monthlyCost": 18200, "roomCount": 3, "squareMeters": 54,
                "homeType": "apartment", "firstHand": true, "platform": "blocket",
                "publishedAt": "2026-08-08T10:00:00Z",
                "location": { "locality": "Årsta", "route": "Svärdlångsvägen", "streetNumber": "12" }
              },
              {
                "id": "1415497", "rent": 15500, "currency": "SEK", "roomCount": 1.5,
                "squareMeters": 26, "homeType": "apartment", "platform": "dotcom",
                "location": { "locality": "Stockholm" }
              }
            ]
          }
        }
      }
    }"#;

    #[test]
    fn parses_sample_response() {
        let resp: GqlResponse<SearchData> = serde_json::from_str(SAMPLE).unwrap();
        assert!(resp.errors.is_empty());
        let nodes = resp.data.unwrap().home_index_search.documents.nodes;
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0].id_num(), Some(1_433_991));
        assert_eq!(nodes[0].room_count, Some(3.0));
        assert_eq!(nodes[1].room_count, Some(1.5));
        // Missing fields tolerated.
        assert_eq!(nodes[1].title, None);
        assert_eq!(nodes[1].first_hand, None);
    }

    #[test]
    fn ignores_unknown_errors_field_default() {
        let resp: GqlResponse<serde_json::Value> =
            serde_json::from_str(r#"{"data": null}"#).unwrap();
        assert!(resp.errors.is_empty());
        assert!(resp.data.is_none());
    }

    #[test]
    fn parses_published_at_timestamp() {
        let resp: GqlResponse<SearchData> = serde_json::from_str(SAMPLE).unwrap();
        let nodes = resp.data.unwrap().home_index_search.documents.nodes;
        let dt = nodes[0].published_at_dt().expect("node 0 has publishedAt");
        assert_eq!(dt.year(), 2026);
        // node 1 has no publishedAt → None, not an error.
        assert_eq!(nodes[1].published_at_dt(), None);
    }
}
