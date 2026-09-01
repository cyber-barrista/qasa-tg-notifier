//! Interactive `/bostad` filter builder over the Bostadsförmedlingen feed.
//!
//! Same shape as `search` (main screen → one sub-menu per filter), but the
//! whole feed arrives as a single JSON array, so every filter is applied
//! client-side. The category filter is the headline: "Bostad snabbt" ads are
//! first-come-first-served, which is the reason this notifier exists at all.

use frankenstein::types::{InlineKeyboardButton, InlineKeyboardMarkup};

use crate::bostad::Ad;
use crate::search::{button, mark, menu};

/// Minimum-room presets; `0` means "any".
const ROOMS: [u8; 6] = [0, 1, 2, 3, 4, 5];
/// Rent presets in SEK; `0` means "any". Bostadsförmedlingen's regulated rents
/// run lower than the open market, hence the finer low end than `search::RENTS`.
const RENTS: [i64; 11] = [
    0, 5_000, 6_000, 7_000, 8_000, 9_000, 10_000, 12_000, 15_000, 18_000, 22_000,
];

/// Ad category, matched against the feed's per-ad booleans.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    /// First-come-first-served fast track (`BostadSnabbt`).
    Snabbt,
    /// Student apartments (`Student`).
    Student,
    /// Ordinary queue-time ads (`Vanlig`).
    Regular,
    All,
}

impl Category {
    const ALL: &'static [Category] = &[
        Category::Snabbt,
        Category::Student,
        Category::Regular,
        Category::All,
    ];

    fn from_index(i: usize) -> Option<Category> {
        Category::ALL.get(i).copied()
    }

    pub fn label(self) -> &'static str {
        match self {
            Category::Snabbt => "⚡ Bostad snabbt",
            Category::Student => "🎓 Student",
            Category::Regular => "🏢 Regular queue",
            Category::All => "All ads",
        }
    }

    fn matches(self, ad: &Ad) -> bool {
        match self {
            Category::Snabbt => ad.bostad_snabbt,
            Category::Student => ad.student,
            Category::Regular => ad.vanlig,
            Category::All => true,
        }
    }
}

/// Declare the `Kommun` enum plus its `ALL`/`label` from one table. The labels
/// are matched verbatim against the feed's `Kommun` field (list taken from the
/// live feed's distinct values for the Stockholm region).
macro_rules! declare_kommuner {
    ($($variant:ident => $label:literal;)+) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum Kommun {
            $($variant,)+
        }

        impl Kommun {
            /// All kommuner, in display order.
            pub const ALL: &'static [Kommun] = &[$(Kommun::$variant,)+];

            pub fn label(self) -> &'static str {
                match self { $(Kommun::$variant => $label,)+ }
            }
        }
    };
}

declare_kommuner! {
    Stockholm => "Stockholm";
    Solna => "Solna";
    Sundbyberg => "Sundbyberg";
    Nacka => "Nacka";
    Lidingo => "Lidingö";
    Danderyd => "Danderyd";
    Taby => "Täby";
    Sollentuna => "Sollentuna";
    Jarfalla => "Järfälla";
    UpplandsBro => "Upplands-Bro";
    UpplandsVasby => "Upplands Väsby";
    Sigtuna => "Sigtuna";
    Vallentuna => "Vallentuna";
    Osteraker => "Österåker";
    Norrtalje => "Norrtälje";
    Varmdo => "Värmdö";
    Tyreso => "Tyresö";
    Haninge => "Haninge";
    Huddinge => "Huddinge";
    Botkyrka => "Botkyrka";
    Salem => "Salem";
    Sodertalje => "Södertälje";
    Nykvarn => "Nykvarn";
    Nynashamn => "Nynäshamn";
    Habo => "Håbo";
}

impl Kommun {
    fn from_index(i: usize) -> Option<Kommun> {
        Kommun::ALL.get(i).copied()
    }
}

/// Current selection for an in-progress /bostad search.
#[derive(Clone, Debug)]
pub struct Filters {
    pub category: Category,
    /// Selected kommuner; empty = the whole region.
    pub kommuner: Vec<Kommun>,
    /// Minimum room count; 0 = any.
    pub min_rooms: u8,
    /// Min/max monthly rent in SEK; None = any.
    pub min_rent: Option<i64>,
    pub max_rent: Option<i64>,
}

impl Default for Filters {
    fn default() -> Self {
        Self {
            category: Category::Snabbt,
            kommuner: Vec::new(),
            min_rooms: 0,
            min_rent: None,
            max_rent: None,
        }
    }
}

impl Filters {
    /// Short human-readable kommun summary for labels/messages.
    pub fn kommun_summary(&self) -> String {
        let names: Vec<&str> = Kommun::ALL
            .iter()
            .filter(|k| self.kommuner.contains(k))
            .map(|k| k.label())
            .collect();
        match names.as_slice() {
            [] => "Whole region".to_string(),
            [one] => (*one).to_string(),
            [a, b] => format!("{a}, {b}"),
            [a, b, ..] => format!("{a}, {b} +{}", names.len() - 2),
        }
    }

    fn toggle_kommun(&mut self, kommun: Kommun) {
        if let Some(pos) = self.kommuner.iter().position(|k| *k == kommun) {
            self.kommuner.remove(pos);
        } else {
            self.kommuner.push(kommun);
        }
    }
}

/// Which screen to display.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    Main,
    Category,
    Rooms,
    MinRent,
    MaxRent,
    Kommun,
}

/// What the caller should do after a button press.
pub enum Action {
    /// Redraw the given screen.
    Show(Screen),
    /// Run the search with the current filters.
    Search,
    /// Unrecognized/no-op.
    Ignore,
}

/// Apply a callback-data action to the filters, returning the next step.
pub fn apply(filters: &mut Filters, data: &str) -> Action {
    match data {
        "go" => return Action::Search,
        "back" => return Action::Show(Screen::Main),
        "menu:cat" => return Action::Show(Screen::Category),
        "menu:rooms" => return Action::Show(Screen::Rooms),
        "menu:minrent" => return Action::Show(Screen::MinRent),
        "menu:maxrent" => return Action::Show(Screen::MaxRent),
        "menu:kommun" => return Action::Show(Screen::Kommun),
        "kommunclear" => {
            filters.kommuner.clear();
            return Action::Show(Screen::Kommun);
        }
        _ => {}
    }
    let Some((key, val)) = data.split_once(':') else {
        return Action::Ignore;
    };
    match key {
        "cat" => {
            if let Some(c) = val.parse::<usize>().ok().and_then(Category::from_index) {
                filters.category = c;
            }
            Action::Show(Screen::Main)
        }
        "rooms" => {
            if let Ok(r) = val.parse::<u8>() {
                if ROOMS.contains(&r) {
                    filters.min_rooms = r;
                }
            }
            Action::Show(Screen::Main)
        }
        "minrent" => {
            if let Ok(v) = val.parse::<i64>() {
                filters.min_rent = (v > 0).then_some(v);
            }
            Action::Show(Screen::Main)
        }
        "maxrent" => {
            if let Ok(v) = val.parse::<i64>() {
                filters.max_rent = (v > 0).then_some(v);
            }
            Action::Show(Screen::Main)
        }
        // Kommuner are multi-select: toggle and stay on the kommun screen.
        "kommun" => {
            if let Some(k) = val.parse::<usize>().ok().and_then(Kommun::from_index) {
                filters.toggle_kommun(k);
            }
            Action::Show(Screen::Kommun)
        }
        _ => Action::Ignore,
    }
}

/// Does an ad pass all filters? Everything is client-side here.
pub fn passes(filters: &Filters, ad: &Ad) -> bool {
    if !filters.category.matches(ad) {
        return false;
    }
    if !filters.kommuner.is_empty() {
        let in_selected = ad
            .kommun
            .as_deref()
            .is_some_and(|k| filters.kommuner.iter().any(|sel| sel.label() == k));
        if !in_selected {
            return false;
        }
    }
    if filters.min_rooms > 0 {
        match ad.antal_rum {
            Some(r) if r >= f64::from(filters.min_rooms) => {}
            _ => return false,
        }
    }
    if let Some(min) = filters.min_rent {
        match ad.rent() {
            Some(rent) if rent >= min => {}
            _ => return false,
        }
    }
    if let Some(max) = filters.max_rent {
        match ad.rent() {
            Some(rent) if rent <= max => {}
            _ => return false,
        }
    }
    true
}

/// Render a screen: the message text and its inline keyboard.
pub fn render(screen: Screen, filters: &Filters) -> (String, InlineKeyboardMarkup) {
    match screen {
        Screen::Main => (main_text(filters), main_keyboard(filters)),
        Screen::Category => ("🗂 Ad category:".to_string(), category_keyboard(filters)),
        Screen::Rooms => (
            "🛏 Minimum number of rooms:".to_string(),
            rooms_keyboard(filters),
        ),
        Screen::MinRent => (
            "💰 Minimum rent (SEK / month):".to_string(),
            rent_keyboard(filters.min_rent, "minrent"),
        ),
        Screen::MaxRent => (
            "💰 Maximum rent (SEK / month):".to_string(),
            rent_keyboard(filters.max_rent, "maxrent"),
        ),
        Screen::Kommun => (
            "📍 Tap kommuner to toggle (none selected = whole region):".to_string(),
            kommun_keyboard(filters),
        ),
    }
}

/// A multi-line summary of the active filters, for the "Searching…" message.
pub fn describe(f: &Filters) -> String {
    format!(
        "🗂 Category: {}\n📍 Kommun: {}\n🛏 Rooms: {}\n💰 Rent: {}–{}",
        f.category.label(),
        f.kommun_summary(),
        rooms_text(f.min_rooms),
        rent_text(f.min_rent),
        rent_text(f.max_rent),
    )
}

fn rooms_text(min_rooms: u8) -> String {
    if min_rooms == 0 {
        "any".to_string()
    } else {
        format!("{min_rooms}+")
    }
}

fn rent_text(rent: Option<i64>) -> String {
    match rent {
        None => "any".to_string(),
        Some(v) => format!("{}k", v / 1000),
    }
}

fn main_text(f: &Filters) -> String {
    format!(
        "🔍 Search Bostadsförmedlingen ads\n\n🗂 Category: {}\n🛏 Rooms: {}\n💰 Rent: {}–{}\n📍 Kommun: {}\n\nTap a field to change it, then Search.",
        f.category.label(),
        rooms_text(f.min_rooms),
        rent_text(f.min_rent),
        rent_text(f.max_rent),
        f.kommun_summary(),
    )
}

/// The main screen: one button per filter, then Search.
fn main_keyboard(f: &Filters) -> InlineKeyboardMarkup {
    let inline_keyboard = vec![
        vec![button(
            format!("🗂 Category: {}", f.category.label()),
            "menu:cat",
        )],
        vec![button(
            format!("🛏 Rooms: {}", rooms_text(f.min_rooms)),
            "menu:rooms",
        )],
        vec![button(
            format!("💰 Min rent: {}", rent_text(f.min_rent)),
            "menu:minrent",
        )],
        vec![button(
            format!("💰 Max rent: {}", rent_text(f.max_rent)),
            "menu:maxrent",
        )],
        vec![button(
            format!("📍 Kommun: {}", f.kommun_summary()),
            "menu:kommun",
        )],
        vec![button("🔎 Search".to_string(), "go")],
    ];
    InlineKeyboardMarkup { inline_keyboard }
}

fn category_keyboard(f: &Filters) -> InlineKeyboardMarkup {
    let buttons = Category::ALL
        .iter()
        .enumerate()
        .map(|(i, c)| button(mark(f.category == *c, c.label()), &format!("cat:{i}")))
        .collect();
    menu(buttons, 2)
}

fn rooms_keyboard(f: &Filters) -> InlineKeyboardMarkup {
    let buttons = ROOMS
        .iter()
        .map(|r| {
            let text = if *r == 0 {
                "Any".to_string()
            } else {
                format!("{r}+")
            };
            button(mark(f.min_rooms == *r, &text), &format!("rooms:{r}"))
        })
        .collect();
    menu(buttons, 3)
}

/// Shared renderer for the min/max rent menus. `prefix` is `minrent`/`maxrent`.
fn rent_keyboard(selected: Option<i64>, prefix: &str) -> InlineKeyboardMarkup {
    let buttons = RENTS
        .iter()
        .map(|v| {
            let text = if *v == 0 {
                "Any".to_string()
            } else {
                format!("{}k", v / 1000)
            };
            button(
                mark(selected.unwrap_or(0) == *v, &text),
                &format!("{prefix}:{v}"),
            )
        })
        .collect();
    menu(buttons, 3)
}

/// Multi-select kommun picker: a toggle grid plus Clear / Done controls.
fn kommun_keyboard(f: &Filters) -> InlineKeyboardMarkup {
    let mut inline_keyboard: Vec<Vec<InlineKeyboardButton>> = Kommun::ALL
        .iter()
        .enumerate()
        .map(|(i, kommun)| {
            button(
                mark(f.kommuner.contains(kommun), kommun.label()),
                &format!("kommun:{i}"),
            )
        })
        .collect::<Vec<_>>()
        .chunks(3)
        .map(<[InlineKeyboardButton]>::to_vec)
        .collect();
    inline_keyboard.push(vec![
        button("🧹 Clear".to_string(), "kommunclear"),
        button("✅ Done".to_string(), "back"),
    ]);
    InlineKeyboardMarkup { inline_keyboard }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ad(snabbt: bool, kommun: &str, rooms: Option<f64>, rent: Option<i64>) -> Ad {
        Ad {
            annons_id: 1,
            gatuadress: None,
            stadsdel: None,
            kommun: Some(kommun.to_string()),
            antal_rum: rooms,
            yta: None,
            hyra: rent,
            lagsta_hyran: None,
            hogsta_hyran: None,
            lagsta_ytan: None,
            hogsta_ytan: None,
            annonserad_till: None,
            url: None,
            lagenhetstyp: None,
            nyproduktion: false,
            student: false,
            ungdom: false,
            senior: false,
            korttid: false,
            vanlig: !snabbt,
            bostad_snabbt: snabbt,
            kort_kotid: false,
        }
    }

    #[test]
    fn category_and_range_filters() {
        let f = Filters {
            min_rooms: 1,
            max_rent: Some(10_000),
            ..Filters::default() // category: Snabbt
        };
        assert!(passes(&f, &ad(true, "Tyresö", Some(1.0), Some(9_074))));
        assert!(!passes(&f, &ad(false, "Tyresö", Some(1.0), Some(9_074)))); // not snabbt
        assert!(!passes(&f, &ad(true, "Tyresö", Some(1.0), Some(12_000)))); // over max rent
        assert!(!passes(&f, &ad(true, "Tyresö", None, Some(9_074)))); // unknown rooms excluded

        let all = Filters {
            category: Category::All,
            ..Filters::default()
        };
        assert!(passes(&all, &ad(false, "Tyresö", None, None)));
    }

    #[test]
    fn kommun_multiselect_matches_feed_labels() {
        let mut f = Filters::default();
        assert_eq!(f.kommun_summary(), "Whole region");

        // Toggle Stockholm (index 0) and Tyresö on.
        let tyreso = Kommun::ALL
            .iter()
            .position(|k| k.label() == "Tyresö")
            .unwrap();
        assert!(matches!(
            apply(&mut f, "kommun:0"),
            Action::Show(Screen::Kommun)
        ));
        assert!(matches!(
            apply(&mut f, &format!("kommun:{tyreso}")),
            Action::Show(Screen::Kommun)
        ));
        assert!(passes(&f, &ad(true, "Tyresö", None, None)));
        assert!(!passes(&f, &ad(true, "Sigtuna", None, None)));

        // Clear returns to "whole region".
        assert!(matches!(
            apply(&mut f, "kommunclear"),
            Action::Show(Screen::Kommun)
        ));
        assert!(passes(&f, &ad(true, "Sigtuna", None, None)));
    }

    #[test]
    fn apply_updates_and_routes() {
        let mut f = Filters::default();
        assert!(matches!(
            apply(&mut f, "menu:cat"),
            Action::Show(Screen::Category)
        ));
        assert!(matches!(apply(&mut f, "cat:1"), Action::Show(Screen::Main)));
        assert_eq!(f.category, Category::Student);
        assert!(matches!(
            apply(&mut f, "minrent:5000"),
            Action::Show(Screen::Main)
        ));
        assert_eq!(f.min_rent, Some(5_000));
        assert!(matches!(apply(&mut f, "go"), Action::Search));
        assert!(matches!(apply(&mut f, "garbage"), Action::Ignore));
    }

    #[test]
    fn kommun_labels_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for k in Kommun::ALL {
            assert!(seen.insert(k.label()), "duplicate kommun {}", k.label());
        }
        assert_eq!(Kommun::from_index(Kommun::ALL.len()), None);
    }
}
