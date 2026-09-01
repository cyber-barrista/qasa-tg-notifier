//! Client for Bostadsförmedlingen's public listings feed.
//!
//! `https://bostad.stockholm.se/AllaAnnonser/` returns one unauthenticated JSON
//! array of every live ad (~500), with per-ad category booleans. As with the
//! Qasa client, the schema is undocumented and drift-prone, so every field is
//! `Option` + `#[serde(default)]`; the JSON keys are Swedish and some are
//! non-ASCII (`Hyra`, `Yta`, `LägstaHyran`), hence the explicit renames.

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::config::Config;

/// A single Bostadsförmedlingen ad. `AnnonsId` is the stable identity of an
/// ad, but it is NOT monotonic with publish date (ads created early and
/// published late keep their low id), so dedup uses a seen-id set rather than
/// a watermark — see `main::run_bostad_cycle`.
#[derive(Deserialize, Debug, Clone)]
pub struct Ad {
    #[serde(rename = "AnnonsId")]
    pub annons_id: u64,
    #[serde(default, rename = "Gatuadress")]
    pub gatuadress: Option<String>,
    #[serde(default, rename = "Stadsdel")]
    pub stadsdel: Option<String>,
    #[serde(default, rename = "Kommun")]
    pub kommun: Option<String>,
    #[serde(default, rename = "AntalRum")]
    pub antal_rum: Option<f64>,
    #[serde(default, rename = "Yta")]
    pub yta: Option<f64>,
    #[serde(default, rename = "Hyra")]
    pub hyra: Option<i64>,
    /// Multi-unit ("project") ads carry a rent/size range instead of `Hyra`/`Yta`.
    #[serde(default, rename = "LägstaHyran")]
    pub lagsta_hyran: Option<i64>,
    #[serde(default, rename = "HögstaHyran")]
    pub hogsta_hyran: Option<i64>,
    #[serde(default, rename = "LägstaYtan")]
    pub lagsta_ytan: Option<f64>,
    #[serde(default, rename = "HögstaYtan")]
    pub hogsta_ytan: Option<f64>,
    #[serde(default, rename = "AnnonseradTill")]
    pub annonserad_till: Option<String>,
    /// Relative listing path, e.g. `/bostad/202614943/`.
    #[serde(default, rename = "Url")]
    pub url: Option<String>,
    #[serde(default, rename = "Lagenhetstyp")]
    pub lagenhetstyp: Option<String>,
    #[serde(default, rename = "Nyproduktion")]
    pub nyproduktion: bool,
    #[serde(default, rename = "Student")]
    pub student: bool,
    #[serde(default, rename = "Ungdom")]
    pub ungdom: bool,
    #[serde(default, rename = "Senior")]
    pub senior: bool,
    #[serde(default, rename = "Korttid")]
    pub korttid: bool,
    #[serde(default, rename = "Vanlig")]
    pub vanlig: bool,
    /// First-come-first-served fast track (ex "Bostadssnabben") — the ads the
    /// scheduled notifier watches, since speed actually wins these.
    #[serde(default, rename = "BostadSnabbt")]
    pub bostad_snabbt: bool,
    #[serde(default, rename = "KortKotid")]
    pub kort_kotid: bool,
}

impl Ad {
    /// Monthly rent: the single-unit figure, else the top of a project range.
    pub fn rent(&self) -> Option<i64> {
        self.hyra.or(self.hogsta_hyran).or(self.lagsta_hyran)
    }

    /// Living area in m², with the same single-then-range fallback as `rent`.
    pub fn sqm(&self) -> Option<f64> {
        self.yta.or(self.hogsta_ytan).or(self.lagsta_ytan)
    }

    /// Absolute listing page URL.
    pub fn full_url(&self) -> String {
        format!(
            "https://bostad.stockholm.se{}",
            self.url.as_deref().unwrap_or("/")
        )
    }
}

/// Fetch every live ad. The feed is one array with no paging.
pub async fn fetch_all(client: &reqwest::Client, cfg: &Config) -> Result<Vec<Ad>> {
    let ads: Vec<Ad> = client
        .get(&cfg.bostad_endpoint)
        .send()
        .await
        .context("requesting Bostadsförmedlingen feed")?
        .error_for_status()?
        .json()
        .await
        .context("decoding Bostadsförmedlingen feed")?;
    Ok(ads)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Captured from the live feed (trimmed to the fields we consume plus a few
    // extras); guards the non-ASCII key renames against silent drift.
    const SAMPLE: &str = r#"[
      {
        "LägenhetId": 202614943, "AnnonsId": 300740, "Stadsdel": "Handen",
        "Gatuadress": "Hebes Gränd 26", "Kommun": "Haninge", "AntalRum": 1,
        "Yta": null, "Hyra": null, "AnnonseradTill": "2026-09-06",
        "AnnonseradFran": "2026-08-18", "Url": "/bostad/202614943/",
        "Nyproduktion": true, "Ungdom": false, "Student": false,
        "Senior": false, "Korttid": false, "Vanlig": true,
        "Bostadssnabben": false, "BostadSnabbt": false,
        "Lagenhetstyp": "Hyresrätt", "KortKotid": false,
        "LägstaHyran": 8570, "HögstaHyran": 10165,
        "LägstaYtan": 26, "HögstaYtan": 35
      },
      {
        "AnnonsId": 301449, "Stadsdel": "Bollmora", "Gatuadress": "Njupkärrsvägen 5",
        "Kommun": "Tyresö", "AntalRum": 1, "Yta": 33, "Hyra": 9074,
        "AnnonseradFran": "2026-08-31", "AnnonseradTill": "2026-09-03",
        "Url": "/bostad/202615120/", "Nyproduktion": true,
        "Vanlig": false, "BostadSnabbt": true, "Lagenhetstyp": "Hyresrätt"
      }
    ]"#;

    #[test]
    fn parses_sample_feed() {
        let ads: Vec<Ad> = serde_json::from_str(SAMPLE).unwrap();
        assert_eq!(ads.len(), 2);

        // Project ad: rent/size come from the range fallbacks.
        let project = &ads[0];
        assert_eq!(project.annons_id, 300_740);
        assert_eq!(project.rent(), Some(10_165));
        assert_eq!(project.sqm(), Some(35.0));
        assert!(!project.bostad_snabbt);
        assert!(project.vanlig);

        // Fast-track ad: single-unit fields win.
        let snabbt = &ads[1];
        assert!(snabbt.bostad_snabbt);
        assert_eq!(snabbt.rent(), Some(9_074));
        assert_eq!(snabbt.sqm(), Some(33.0));
        assert_eq!(
            snabbt.full_url(),
            "https://bostad.stockholm.se/bostad/202615120/"
        );
        // Absent booleans default to false rather than failing the parse.
        assert!(!snabbt.student);
    }
}
