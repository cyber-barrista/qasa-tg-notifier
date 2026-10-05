//! Interactive `/danang` filter builder over Chợ Tốt listings, parameterised
//! by [`City`] so another city is a table entry away.
//!
//! Same shape as `bostad_search` (main screen → one sub-menu per filter).
//! The category drives the API's `cg` parameter and selected districts go to
//! the API as `area_v2` codes; everything else is applied client-side over
//! the newest pages. Rents are VND per month — Đà Nẵng apartments run roughly
//! 5–40 million, typically 8–15.

use frankenstein::types::{InlineKeyboardButton, InlineKeyboardMarkup};

use crate::nhatot::{
    strip_place_prefix, Ad, City, CATEGORY_ALL, CATEGORY_APARTMENT, CATEGORY_HOUSE,
};
use crate::search::{button, mark, menu};

/// Minimum-room presets; `0` means "any".
const ROOMS: [u8; 5] = [0, 1, 2, 3, 4];
/// Rent presets in VND; `0` means "any".
const RENTS: [i64; 10] = [
    0, 5_000_000, 8_000_000, 10_000_000, 12_000_000, 15_000_000, 20_000_000, 25_000_000,
    30_000_000, 40_000_000,
];
/// Minimum-size presets in m²; `0` means "any".
const SIZES: [u32; 6] = [0, 30, 40, 50, 60, 80];
/// Max-age presets in hours; `0` means "any". Age is measured from
/// `list_time`, which Chợ Tốt rewrites when an ad is bumped, so this is
/// "listed or bumped within" — a bump means the place is still available.
const AGES: [i64; 5] = [0, 24, 72, 168, 336];
/// Default max age. Đà Nẵng posts ~150 apartment ads a day, so a search is
/// always capped by the 40-result limit before the age bites; the preset
/// mainly matters once districts and rent narrow things down.
const DEFAULT_AGE_HOURS: i64 = 72;

/// Property type, mapped to the API's `cg` code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    Apartment,
    House,
    Both,
}

impl Category {
    const ALL: &'static [Category] = &[Category::Apartment, Category::House, Category::Both];

    fn from_index(i: usize) -> Option<Category> {
        Category::ALL.get(i).copied()
    }

    pub fn label(self) -> &'static str {
        match self {
            Category::Apartment => "🏢 Apartment",
            Category::House => "🏡 House",
            Category::Both => "Both",
        }
    }

    /// The `cg` query parameter to fetch with.
    pub fn cg(self) -> u32 {
        match self {
            Category::Apartment => CATEGORY_APARTMENT,
            Category::House => CATEGORY_HOUSE,
            Category::Both => CATEGORY_ALL,
        }
    }

    fn matches(self, ad: &Ad) -> bool {
        match self {
            Category::Apartment => ad.category == Some(CATEGORY_APARTMENT),
            Category::House => ad.category == Some(CATEGORY_HOUSE),
            // `cg=1000` also returns offices and land; keep homes only.
            Category::Both => ad.is_home(),
        }
    }
}

/// Current selection for an in-progress city search.
#[derive(Clone, Debug)]
pub struct Filters {
    pub city: City,
    pub category: Category,
    /// Selected district labels; empty = whole city.
    pub places: Vec<&'static str>,
    /// Minimum room count; 0 = any.
    pub min_rooms: u8,
    /// Min/max monthly rent in VND; None = any.
    pub min_rent: Option<i64>,
    pub max_rent: Option<i64>,
    /// Minimum size in m²; None = any.
    pub min_size: Option<u32>,
    /// Max hours since listed/bumped; None = any.
    pub max_age_hours: Option<i64>,
}

impl Default for Filters {
    fn default() -> Self {
        Filters::for_city(City::DaNang)
    }
}

impl Filters {
    pub fn for_city(city: City) -> Self {
        Self {
            city,
            category: Category::Apartment,
            places: Vec::new(),
            min_rooms: 0,
            min_rent: None,
            max_rent: None,
            min_size: None,
            max_age_hours: Some(DEFAULT_AGE_HOURS),
        }
    }

    /// Short human-readable district summary for labels/messages.
    pub fn place_summary(&self) -> String {
        let names: Vec<&str> = self
            .city
            .places()
            .iter()
            .filter(|p| self.places.contains(&p.label))
            .map(|p| strip_place_prefix(p.label))
            .collect();
        match names.as_slice() {
            [] => "Whole city".to_string(),
            [one] => (*one).to_string(),
            [a, b] => format!("{a}, {b}"),
            [a, b, ..] => format!("{a}, {b} +{}", names.len() - 2),
        }
    }

    /// `area_v2` codes to send to the API for the selected districts.
    pub fn area_codes(&self) -> Vec<&'static str> {
        self.city
            .places()
            .iter()
            .filter(|p| self.places.contains(&p.label))
            .map(|p| p.area_v2)
            .collect()
    }

    fn toggle_place(&mut self, label: &'static str) {
        if let Some(pos) = self.places.iter().position(|p| *p == label) {
            self.places.remove(pos);
        } else {
            self.places.push(label);
        }
    }
}

/// Which screen to display.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    Main,
    Age,
    Category,
    Rooms,
    MinRent,
    MaxRent,
    Size,
    Place,
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
        "menu:age" => return Action::Show(Screen::Age),
        "menu:cat" => return Action::Show(Screen::Category),
        "menu:rooms" => return Action::Show(Screen::Rooms),
        "menu:minrent" => return Action::Show(Screen::MinRent),
        "menu:maxrent" => return Action::Show(Screen::MaxRent),
        "menu:size" => return Action::Show(Screen::Size),
        "menu:place" => return Action::Show(Screen::Place),
        "placeclear" => {
            filters.places.clear();
            return Action::Show(Screen::Place);
        }
        _ => {}
    }
    let Some((key, val)) = data.split_once(':') else {
        return Action::Ignore;
    };
    match key {
        "age" => {
            if let Ok(v) = val.parse::<i64>() {
                if AGES.contains(&v) {
                    filters.max_age_hours = (v > 0).then_some(v);
                }
            }
            Action::Show(Screen::Main)
        }
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
        "size" => {
            if let Ok(v) = val.parse::<u32>() {
                if SIZES.contains(&v) {
                    filters.min_size = (v > 0).then_some(v);
                }
            }
            Action::Show(Screen::Main)
        }
        // Places are multi-select: toggle and stay on the place screen.
        "place" => {
            if let Some(p) = val
                .parse::<usize>()
                .ok()
                .and_then(|i| filters.city.places().get(i))
            {
                filters.toggle_place(p.label);
            }
            Action::Show(Screen::Place)
        }
        _ => Action::Ignore,
    }
}

/// Does an ad pass all filters? The category and districts also drive the
/// fetch, so those checks mostly matter for `Both`; the rest is purely
/// client-side. `now_ms` is the current time in epoch milliseconds (the unit
/// `list_time` uses).
pub fn passes(filters: &Filters, ad: &Ad, now_ms: i64) -> bool {
    if !filters.category.matches(ad) {
        return false;
    }
    if let Some(max_hours) = filters.max_age_hours {
        if ad.list_time < now_ms - max_hours * 3_600_000 {
            return false;
        }
    }
    if !filters.places.is_empty() {
        let in_selected = filters
            .city
            .place_of(ad)
            .is_some_and(|p| filters.places.contains(&p));
        if !in_selected {
            return false;
        }
    }
    if filters.min_rooms > 0 {
        match ad.rooms {
            Some(r) if r >= filters.min_rooms => {}
            _ => return false,
        }
    }
    if let Some(min) = filters.min_rent {
        match ad.price {
            Some(p) if p >= min => {}
            _ => return false,
        }
    }
    if let Some(max) = filters.max_rent {
        match ad.price {
            Some(p) if p <= max => {}
            _ => return false,
        }
    }
    if let Some(min) = filters.min_size {
        match ad.size {
            Some(s) if s >= f64::from(min) => {}
            _ => return false,
        }
    }
    true
}

/// Render a screen: the message text and its inline keyboard.
pub fn render(screen: Screen, filters: &Filters) -> (String, InlineKeyboardMarkup) {
    match screen {
        Screen::Main => (main_text(filters), main_keyboard(filters)),
        Screen::Age => (
            "⏱ Listed or bumped within:".to_string(),
            age_keyboard(filters),
        ),
        Screen::Category => ("🗂 Property type:".to_string(), category_keyboard(filters)),
        Screen::Rooms => (
            "🛏 Minimum number of bedrooms:".to_string(),
            rooms_keyboard(filters),
        ),
        Screen::MinRent => (
            "💰 Minimum rent (million VND / month):".to_string(),
            rent_keyboard(filters.min_rent, "minrent"),
        ),
        Screen::MaxRent => (
            "💰 Maximum rent (million VND / month):".to_string(),
            rent_keyboard(filters.max_rent, "maxrent"),
        ),
        Screen::Size => ("📐 Minimum size (m²):".to_string(), size_keyboard(filters)),
        Screen::Place => (
            "📍 Tap districts to toggle (none selected = whole city):".to_string(),
            place_keyboard(filters),
        ),
    }
}

/// A multi-line summary of the active filters, for the "Searching…" message.
pub fn describe(f: &Filters) -> String {
    format!(
        "🏙 City: {}\n⏱ Age: {}\n🗂 Type: {}\n📍 District: {}\n🛏 Rooms: {}\n💰 Rent: {}–{}\n📐 Min size: {}",
        f.city.label(),
        age_text(f.max_age_hours),
        f.category.label(),
        f.place_summary(),
        rooms_text(f.min_rooms),
        rent_text(f.min_rent),
        rent_text(f.max_rent),
        size_text(f.min_size),
    )
}

fn rooms_text(min_rooms: u8) -> String {
    if min_rooms == 0 {
        "any".to_string()
    } else {
        format!("{min_rooms}+")
    }
}

/// VND rent as millions, e.g. `15M` or `12.5M`.
fn rent_text(rent: Option<i64>) -> String {
    match rent {
        None => "any".to_string(),
        Some(v) => {
            let whole = v / 1_000_000;
            let tenths = (v % 1_000_000) / 100_000;
            if tenths == 0 {
                format!("{whole}M")
            } else {
                format!("{whole}.{tenths}M")
            }
        }
    }
}

/// Hours as `24h` / `3d` / `7d`.
fn age_text(hours: Option<i64>) -> String {
    match hours {
        None => "any".to_string(),
        Some(h) if h % 24 == 0 && h > 24 => format!("{}d", h / 24),
        Some(h) => format!("{h}h"),
    }
}

fn size_text(size: Option<u32>) -> String {
    match size {
        None => "any".to_string(),
        Some(v) => format!("{v}+ m²"),
    }
}

fn main_text(f: &Filters) -> String {
    format!(
        "🔍 Search {} rentals (Chợ Tốt)\n\n⏱ Age: {}\n🗂 Type: {}\n🛏 Rooms: {}\n💰 Rent: {}–{} VND/month\n📐 Min size: {}\n📍 District: {}\n\nTap a field to change it, then Search.",
        f.city.label(),
        age_text(f.max_age_hours),
        f.category.label(),
        rooms_text(f.min_rooms),
        rent_text(f.min_rent),
        rent_text(f.max_rent),
        size_text(f.min_size),
        f.place_summary(),
    )
}

/// The main screen: one button per filter, then Search.
fn main_keyboard(f: &Filters) -> InlineKeyboardMarkup {
    let inline_keyboard = vec![
        vec![button(
            format!("⏱ Age: {}", age_text(f.max_age_hours)),
            "menu:age",
        )],
        vec![button(
            format!("🗂 Type: {}", f.category.label()),
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
            format!("📐 Min size: {}", size_text(f.min_size)),
            "menu:size",
        )],
        vec![button(
            format!("📍 District: {}", f.place_summary()),
            "menu:place",
        )],
        vec![button("🔎 Search".to_string(), "go")],
    ];
    InlineKeyboardMarkup::builder()
        .inline_keyboard(inline_keyboard)
        .build()
}

fn age_keyboard(f: &Filters) -> InlineKeyboardMarkup {
    let buttons = AGES
        .iter()
        .map(|h| {
            let text = if *h == 0 {
                "Any".to_string()
            } else {
                age_text(Some(*h))
            };
            button(
                mark(f.max_age_hours.unwrap_or(0) == *h, &text),
                &format!("age:{h}"),
            )
        })
        .collect();
    menu(buttons, 3)
}

fn category_keyboard(f: &Filters) -> InlineKeyboardMarkup {
    let buttons = Category::ALL
        .iter()
        .enumerate()
        .map(|(i, c)| button(mark(f.category == *c, c.label()), &format!("cat:{i}")))
        .collect();
    menu(buttons, 3)
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
                rent_text(Some(*v))
            };
            button(
                mark(selected.unwrap_or(0) == *v, &text),
                &format!("{prefix}:{v}"),
            )
        })
        .collect();
    menu(buttons, 3)
}

fn size_keyboard(f: &Filters) -> InlineKeyboardMarkup {
    let buttons = SIZES
        .iter()
        .map(|v| {
            let text = if *v == 0 {
                "Any".to_string()
            } else {
                format!("{v}+ m²")
            };
            button(
                mark(f.min_size.unwrap_or(0) == *v, &text),
                &format!("size:{v}"),
            )
        })
        .collect();
    menu(buttons, 3)
}

/// Multi-select district picker: a toggle grid plus Clear / Done controls.
fn place_keyboard(f: &Filters) -> InlineKeyboardMarkup {
    let mut inline_keyboard: Vec<Vec<InlineKeyboardButton>> = f
        .city
        .places()
        .iter()
        .enumerate()
        .map(|(i, place)| {
            button(
                mark(
                    f.places.contains(&place.label),
                    strip_place_prefix(place.label),
                ),
                &format!("place:{i}"),
            )
        })
        .collect::<Vec<_>>()
        .chunks(2)
        .map(<[InlineKeyboardButton]>::to_vec)
        .collect();
    inline_keyboard.push(vec![
        button("🧹 Clear".to_string(), "placeclear"),
        button("✅ Done".to_string(), "back"),
    ]);
    InlineKeyboardMarkup::builder()
        .inline_keyboard(inline_keyboard)
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// "Now" for the tests; the fixture lists one hour before it.
    const NOW_MS: i64 = 1_791_200_000_000;

    fn pass(f: &Filters, ad: &Ad) -> bool {
        passes(f, ad, NOW_MS)
    }

    fn ad(
        category: u32,
        district: &str,
        rooms: Option<u8>,
        price: Option<i64>,
        size: Option<f64>,
    ) -> Ad {
        Ad {
            ad_id: 1,
            list_id: 1,
            list_time: NOW_MS - 3_600_000,
            orig_list_time: None,
            subject: None,
            body: None,
            price,
            price_string: None,
            rooms,
            size,
            ward_name: Some("Phường Mỹ An".to_string()),
            street_name: None,
            area_name: Some(district.to_string()),
            region_name: Some("Đà Nẵng".to_string()),
            pty_project_name: None,
            category: Some(category),
            category_name: None,
            translated: false,
            city: Some(City::DaNang),
        }
    }

    #[test]
    fn category_filters_by_code() {
        let apt = Filters::default();
        assert!(pass(&apt, &ad(1010, "Quận Hải Châu", None, None, None)));
        assert!(!pass(&apt, &ad(1020, "Quận Hải Châu", None, None, None)));

        let house = Filters {
            category: Category::House,
            ..Filters::default()
        };
        assert!(pass(&house, &ad(1020, "Quận Hải Châu", None, None, None)));
        assert!(!pass(&house, &ad(1010, "Quận Hải Châu", None, None, None)));

        let both = Filters {
            category: Category::Both,
            ..Filters::default()
        };
        assert!(pass(&both, &ad(1010, "Quận Hải Châu", None, None, None)));
        assert!(pass(&both, &ad(1020, "Quận Hải Châu", None, None, None)));
        assert!(!pass(&both, &ad(1030, "Quận Hải Châu", None, None, None))); // office

        assert_eq!(Category::Apartment.cg(), 1010);
        assert_eq!(Category::House.cg(), 1020);
        assert_eq!(Category::Both.cg(), 1000);
    }

    #[test]
    fn range_filters() {
        let f = Filters {
            min_rooms: 2,
            min_rent: Some(10_000_000),
            max_rent: Some(15_000_000),
            min_size: Some(50),
            ..Filters::default()
        };
        let w = "Quận Sơn Trà";
        assert!(pass(
            &f,
            &ad(1010, w, Some(2), Some(14_000_000), Some(63.0))
        ));
        assert!(!pass(
            &f,
            &ad(1010, w, Some(1), Some(14_000_000), Some(63.0))
        )); // rooms
        assert!(!pass(&f, &ad(1010, w, None, Some(14_000_000), Some(63.0)))); // unknown rooms
        assert!(!pass(
            &f,
            &ad(1010, w, Some(2), Some(8_000_000), Some(63.0))
        )); // under min
        assert!(!pass(
            &f,
            &ad(1010, w, Some(2), Some(16_000_000), Some(63.0))
        )); // over max
        assert!(!pass(
            &f,
            &ad(1010, w, Some(2), Some(14_000_000), Some(45.0))
        )); // small
        assert!(!pass(&f, &ad(1010, w, Some(2), Some(14_000_000), None))); // unknown size
    }

    #[test]
    fn district_multiselect_matches_area_name_and_maps_to_codes() {
        let mut f = Filters::default();
        assert_eq!(f.city, City::DaNang);
        assert_eq!(f.place_summary(), "Whole city");
        let son_tra = City::DaNang
            .places()
            .iter()
            .position(|p| p.label == "Quận Sơn Trà")
            .unwrap();
        assert!(matches!(
            apply(&mut f, "place:0"),
            Action::Show(Screen::Place)
        )); // Hải Châu
        apply(&mut f, &format!("place:{son_tra}"));
        assert_eq!(f.place_summary(), "Hải Châu, Sơn Trà");
        assert_eq!(f.area_codes(), ["301703", "301704"]);

        assert!(pass(&f, &ad(1010, "Quận Sơn Trà", None, None, None)));
        assert!(!pass(&f, &ad(1010, "Quận Ngũ Hành Sơn", None, None, None)));

        // Toggling again removes it; Clear returns to "whole city".
        apply(&mut f, &format!("place:{son_tra}"));
        assert!(!pass(&f, &ad(1010, "Quận Sơn Trà", None, None, None)));
        assert!(matches!(
            apply(&mut f, "placeclear"),
            Action::Show(Screen::Place)
        ));
        assert!(pass(&f, &ad(1010, "Quận Ngũ Hành Sơn", None, None, None)));
        assert!(f.area_codes().is_empty());
        // Out-of-range index is ignored.
        assert!(matches!(
            apply(&mut f, "place:99"),
            Action::Show(Screen::Place)
        ));
        assert!(f.places.is_empty());

        let (text, _) = render(Screen::Place, &f);
        assert!(text.contains("Tap districts"));
        assert!(main_text(&f).contains("Search Da Nang rentals"));
        assert!(main_text(&f).contains("📍 District:"));
        assert!(describe(&f).contains("🏙 City: Da Nang"));
    }

    #[test]
    fn age_filter_uses_list_time() {
        let mut f = Filters::default();
        assert_eq!(f.max_age_hours, Some(DEFAULT_AGE_HOURS));
        let mut old = ad(1010, "Quận Hải Châu", None, None, None);
        old.list_time = NOW_MS - 8 * 24 * 3_600_000; // 8 days ago
        assert!(!pass(&f, &old));
        assert!(pass(&f, &ad(1010, "Quận Hải Châu", None, None, None)));

        assert!(matches!(
            apply(&mut f, "menu:age"),
            Action::Show(Screen::Age)
        ));
        assert!(matches!(
            apply(&mut f, "age:336"),
            Action::Show(Screen::Main)
        ));
        assert_eq!(f.max_age_hours, Some(336));
        assert!(pass(&f, &old));
        // A bumped ad counts from its bump (list_time), by design.
        old.orig_list_time = Some(NOW_MS - 60 * 24 * 3_600_000);
        assert!(pass(&f, &old));

        assert!(matches!(apply(&mut f, "age:0"), Action::Show(Screen::Main)));
        assert_eq!(f.max_age_hours, None);
        assert!(matches!(apply(&mut f, "age:5"), Action::Show(Screen::Main))); // not a preset
        assert_eq!(f.max_age_hours, None);

        assert_eq!(age_text(Some(24)), "24h");
        assert_eq!(age_text(Some(72)), "3d");
        assert_eq!(age_text(Some(168)), "7d");
        assert_eq!(age_text(None), "any");
    }

    #[test]
    fn apply_updates_and_routes() {
        let mut f = Filters::default();
        assert!(matches!(
            apply(&mut f, "menu:size"),
            Action::Show(Screen::Size)
        ));
        assert!(matches!(
            apply(&mut f, "size:40"),
            Action::Show(Screen::Main)
        ));
        assert_eq!(f.min_size, Some(40));
        assert!(matches!(
            apply(&mut f, "size:0"),
            Action::Show(Screen::Main)
        ));
        assert_eq!(f.min_size, None);
        assert!(matches!(
            apply(&mut f, "size:33"),
            Action::Show(Screen::Main)
        )); // not a preset
        assert_eq!(f.min_size, None);
        assert!(matches!(apply(&mut f, "cat:1"), Action::Show(Screen::Main)));
        assert_eq!(f.category, Category::House);
        assert!(matches!(
            apply(&mut f, "maxrent:15000000"),
            Action::Show(Screen::Main)
        ));
        assert_eq!(f.max_rent, Some(15_000_000));
        assert!(matches!(apply(&mut f, "go"), Action::Search));
        assert!(matches!(apply(&mut f, "garbage"), Action::Ignore));
    }

    #[test]
    fn rent_text_formats_millions() {
        assert_eq!(rent_text(None), "any");
        assert_eq!(rent_text(Some(15_000_000)), "15M");
        assert_eq!(rent_text(Some(12_500_000)), "12.5M");
    }
}
