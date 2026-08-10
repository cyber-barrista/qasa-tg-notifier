//! Interactive `/search` filter builder.
//!
//! The main screen shows one button per filter; tapping a filter opens its own
//! sub-menu of options (like the neighborhood picker), so each dimension can
//! offer granular choices without crowding one screen. Areas are multi-select.
//! Age and areas are pushed to the API; rooms and rent are filtered client-side.

use frankenstein::types::{InlineKeyboardButton, InlineKeyboardMarkup};

use crate::qasa::Home;

/// Max-age presets, in hours.
const AGES: [i64; 7] = [3, 6, 12, 24, 48, 72, 168];
/// Minimum-room presets; `0` means "any".
const ROOMS: [u8; 6] = [0, 1, 2, 3, 4, 5];
/// Rent presets in SEK; `0` means "any". Shared by the min and max menus.
const RENTS: [i64; 11] = [
    0, 6_000, 8_000, 10_000, 12_000, 15_000, 18_000, 20_000, 25_000, 30_000, 40_000,
];

/// When no area is selected, search the whole city.
const ALL_STOCKHOLM: &str = "se/stockholm";

/// Declare the `Area` enum plus its `ALL`/`label`/`slug` from one table.
macro_rules! declare_areas {
    ($($variant:ident => $label:literal, $slug:literal;)+) => {
        /// A selectable Stockholm-region area.
        ///
        /// Each `(label, slug)` pair was verified live against the API: the slug
        /// returns a real subset of listings (an invalid slug silently falls
        /// back to a country-wide result, so this list is curated from probing).
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum Area {
            $($variant,)+
        }

        impl Area {
            /// All areas, in display order.
            pub const ALL: &'static [Area] = &[$(Area::$variant,)+];

            pub fn label(self) -> &'static str {
                match self { $(Area::$variant => $label,)+ }
            }

            /// The Qasa `areaIdentifier` for this area.
            pub fn slug(self) -> &'static str {
                match self { $(Area::$variant => $slug,)+ }
            }
        }
    };
}

declare_areas! {
    Sodermalm => "Södermalm", "se/sodermalm";
    Kungsholmen => "Kungsholmen", "se/kungsholmen";
    Vasastan => "Vasastan", "se/vasastan";
    Ostermalm => "Östermalm", "se/ostermalm";
    Norrmalm => "Norrmalm", "se/norrmalm";
    GamlaStan => "Gamla stan", "se/gamla_stan";
    Gardet => "Gärdet", "se/gardet";
    Djurgarden => "Djurgården", "se/djurgarden";
    HammarbySjostad => "Hammarby Sjöstad", "se/hammarby_sjostad";
    Liljeholmen => "Liljeholmen", "se/liljeholmen";
    Hagersten => "Hägersten", "se/hagersten";
    Aspudden => "Aspudden", "se/aspudden";
    Midsommarkransen => "Midsommarkransen", "se/midsommarkransen";
    Telefonplan => "Telefonplan", "se/telefonplan";
    Arsta => "Årsta", "se/arsta";
    Gullmarsplan => "Gullmarsplan", "se/gullmarsplan";
    Hammarbyhojden => "Hammarbyhöjden", "se/hammarbyhojden";
    Bjorkhagen => "Björkhagen", "se/björkhagen";
    Karrtorp => "Kärrtorp", "se/kärrtorp";
    Enskede => "Enskede", "se/enskede";
    Stureby => "Stureby", "se/stureby";
    Bandhagen => "Bandhagen", "se/bandhagen";
    Hogdalen => "Högdalen", "se/högdalen";
    Ragsved => "Rågsved", "se/rågsved";
    Hagsatra => "Hagsätra", "se/hagsatra";
    Farsta => "Farsta", "se/farsta";
    Skondal => "Sköndal", "se/sköndal";
    Gubbangen => "Gubbängen", "se/gubbangen";
    Alvsjo => "Älvsjö", "se/alvsjo";
    Hokarangen => "Hökarängen", "se/hokarangen";
    Skarpnack => "Skarpnäck", "se/skarpnäck";
    Bagarmossen => "Bagarmossen", "se/bagarmossen";
    Kista => "Kista", "se/kista";
    Husby => "Husby", "se/husby";
    Akalla => "Akalla", "se/akalla";
    Rinkeby => "Rinkeby", "se/rinkeby";
    Tensta => "Tensta", "se/tensta";
    Spanga => "Spånga", "se/spanga";
    Hasselby => "Hässelby", "se/hässelby";
    Vallingby => "Vällingby", "se/vallingby";
    Blackeberg => "Blackeberg", "se/blackeberg";
    Bromma => "Bromma", "se/bromma";
    Traneberg => "Traneberg", "se/traneberg";
    Mariehall => "Mariehäll", "se/mariehäll";
    Sundbyberg => "Sundbyberg", "se/sundbyberg";
    Solna => "Solna", "se/solna";
    Sollentuna => "Sollentuna", "se/sollentuna";
    Nacka => "Nacka", "se/nacka";
    Sickla => "Sickla", "se/sickla";
    Danderyd => "Danderyd", "se/danderyd";
    Lidingo => "Lidingö", "se/lidingo";
    Huddinge => "Huddinge", "se/huddinge";
    Jarfalla => "Järfälla", "se/jarfalla";
    Taby => "Täby", "se/taby";
    Skarholmen => "Skärholmen", "se/skarholmen";
    Fruangen => "Fruängen", "se/fruängen";
    Bredang => "Bredäng", "se/bredäng";
    Norsborg => "Norsborg", "se/norsborg";
    Fredhall => "Fredhäll", "se/fredhäll";
}

impl Area {
    fn from_index(i: usize) -> Option<Area> {
        Area::ALL.get(i).copied()
    }
}

/// Current selection for an in-progress search.
#[derive(Clone, Debug)]
pub struct Filters {
    pub age_hours: i64,
    /// Minimum room count; 0 = any.
    pub min_rooms: u8,
    /// Min/max monthly rent in SEK; None = any.
    pub min_rent: Option<i64>,
    pub max_rent: Option<i64>,
    /// Selected areas; empty = whole city.
    pub areas: Vec<Area>,
}

impl Default for Filters {
    fn default() -> Self {
        Self {
            age_hours: 24,
            min_rooms: 0,
            min_rent: None,
            max_rent: None,
            areas: Vec::new(),
        }
    }
}

impl Filters {
    /// `areaIdentifier` values for the query — selected areas, or the whole city.
    pub fn area_slugs(&self) -> Vec<&'static str> {
        if self.areas.is_empty() {
            vec![ALL_STOCKHOLM]
        } else {
            Area::ALL
                .iter()
                .filter(|a| self.areas.contains(a))
                .map(|a| a.slug())
                .collect()
        }
    }

    /// Short human-readable area summary for labels/messages.
    pub fn area_summary(&self) -> String {
        let names: Vec<&str> = Area::ALL
            .iter()
            .filter(|a| self.areas.contains(a))
            .map(|a| a.label())
            .collect();
        match names.as_slice() {
            [] => "All Stockholm".to_string(),
            [one] => (*one).to_string(),
            [a, b] => format!("{a}, {b}"),
            [a, b, ..] => format!("{a}, {b} +{}", names.len() - 2),
        }
    }

    fn toggle_area(&mut self, area: Area) {
        if let Some(pos) = self.areas.iter().position(|a| *a == area) {
            self.areas.remove(pos);
        } else {
            self.areas.push(area);
        }
    }
}

/// Which screen to display.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    Main,
    Age,
    Rooms,
    MinRent,
    MaxRent,
    Area,
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
        "menu:rooms" => return Action::Show(Screen::Rooms),
        "menu:minrent" => return Action::Show(Screen::MinRent),
        "menu:maxrent" => return Action::Show(Screen::MaxRent),
        "menu:area" => return Action::Show(Screen::Area),
        "areaclear" => {
            filters.areas.clear();
            return Action::Show(Screen::Area);
        }
        _ => {}
    }
    let Some((key, val)) = data.split_once(':') else {
        return Action::Ignore;
    };
    match key {
        "age" => {
            if let Ok(h) = val.parse::<i64>() {
                if AGES.contains(&h) {
                    filters.age_hours = h;
                }
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
        // Areas are multi-select: toggle and stay on the area screen.
        "area" => {
            if let Some(area) = val.parse::<usize>().ok().and_then(Area::from_index) {
                filters.toggle_area(area);
            }
            Action::Show(Screen::Area)
        }
        _ => Action::Ignore,
    }
}

/// Does a fetched listing pass the client-side (room/rent) filters?
/// Age and areas are already applied by the API query.
pub fn passes(filters: &Filters, home: &Home) -> bool {
    if filters.min_rooms > 0 {
        match home.room_count {
            Some(r) if r >= f64::from(filters.min_rooms) => {}
            _ => return false,
        }
    }
    if let Some(min) = filters.min_rent {
        match home.rent {
            Some(rent) if rent >= min => {}
            _ => return false,
        }
    }
    if let Some(max) = filters.max_rent {
        match home.rent {
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
        Screen::Age => (
            "⏱ Published within — pick a max age:".to_string(),
            age_keyboard(filters),
        ),
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
        Screen::Area => (
            "📍 Tap areas to toggle (none selected = all Stockholm):".to_string(),
            area_keyboard(filters),
        ),
    }
}

/// A multi-line summary of the active filters, for the "Searching…" message.
pub fn describe(f: &Filters) -> String {
    format!(
        "📍 Area: {}\n⏱ Published: last {}h\n🛏 Rooms: {}\n💰 Rent: {}–{}",
        f.area_summary(),
        f.age_hours,
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
        "🔍 Search Stockholm rentals\n\n⏱ Age: last {}h\n🛏 Rooms: {}\n💰 Rent: {}–{}\n📍 Area: {}\n\nTap a field to change it, then Search.",
        f.age_hours,
        rooms_text(f.min_rooms),
        rent_text(f.min_rent),
        rent_text(f.max_rent),
        f.area_summary(),
    )
}

fn button(text: String, data: &str) -> InlineKeyboardButton {
    InlineKeyboardButton::builder()
        .text(text)
        .callback_data(data.to_string())
        .build()
}

/// Mark the selected option with a leading dot.
fn mark(selected: bool, text: &str) -> String {
    if selected {
        format!("• {text}")
    } else {
        text.to_string()
    }
}

/// Lay buttons out `per_row` wide and append a Back row.
fn menu(buttons: Vec<InlineKeyboardButton>, per_row: usize) -> InlineKeyboardMarkup {
    let mut inline_keyboard: Vec<Vec<InlineKeyboardButton>> = buttons
        .chunks(per_row)
        .map(<[InlineKeyboardButton]>::to_vec)
        .collect();
    inline_keyboard.push(vec![button("⬅ Back".to_string(), "back")]);
    InlineKeyboardMarkup { inline_keyboard }
}

/// The main screen: one button per filter, then Search.
fn main_keyboard(f: &Filters) -> InlineKeyboardMarkup {
    let inline_keyboard = vec![
        vec![button(format!("⏱ Age: {}h", f.age_hours), "menu:age")],
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
            format!("📍 Areas: {}", f.area_summary()),
            "menu:area",
        )],
        vec![button("🔎 Search".to_string(), "go")],
    ];
    InlineKeyboardMarkup { inline_keyboard }
}

fn age_keyboard(f: &Filters) -> InlineKeyboardMarkup {
    let buttons = AGES
        .iter()
        .map(|h| {
            button(
                mark(f.age_hours == *h, &format!("{h}h")),
                &format!("age:{h}"),
            )
        })
        .collect();
    menu(buttons, 4)
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

/// Multi-select area picker: a toggle grid plus Clear / Done controls.
fn area_keyboard(f: &Filters) -> InlineKeyboardMarkup {
    let mut inline_keyboard: Vec<Vec<InlineKeyboardButton>> = Area::ALL
        .iter()
        .enumerate()
        .map(|(i, area)| {
            button(
                mark(f.areas.contains(area), area.label()),
                &format!("area:{i}"),
            )
        })
        .collect::<Vec<_>>()
        .chunks(3)
        .map(<[InlineKeyboardButton]>::to_vec)
        .collect();
    inline_keyboard.push(vec![
        button("🧹 Clear".to_string(), "areaclear"),
        button("✅ Done".to_string(), "back"),
    ]);
    InlineKeyboardMarkup { inline_keyboard }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home(rooms: Option<f64>, rent: Option<i64>) -> Home {
        Home {
            id: "1".into(),
            title: None,
            rent,
            currency: None,
            monthly_cost: None,
            room_count: rooms,
            square_meters: None,
            home_type: None,
            first_hand: None,
            platform: None,
            published_at: None,
            published_or_bumped_at: None,
            location: None,
        }
    }

    #[test]
    fn passes_room_and_rent_range() {
        let f = Filters {
            min_rooms: 2,
            min_rent: Some(10_000),
            max_rent: Some(15_000),
            ..Filters::default()
        };
        assert!(passes(&f, &home(Some(2.0), Some(12_000))));
        assert!(!passes(&f, &home(Some(1.0), Some(12_000)))); // too few rooms
        assert!(!passes(&f, &home(Some(2.0), Some(9_000)))); // below min rent
        assert!(!passes(&f, &home(Some(2.0), Some(16_000)))); // above max rent
        assert!(!passes(&f, &home(Some(2.0), None))); // unknown rent excluded
    }

    #[test]
    fn area_multiselect_toggles_and_maps_to_slugs() {
        let mut f = Filters::default();
        assert_eq!(f.area_slugs(), vec!["se/stockholm"]);
        assert_eq!(f.area_summary(), "All Stockholm");

        // Toggle two areas on; each keeps us on the area screen.
        assert!(matches!(
            apply(&mut f, "area:0"),
            Action::Show(Screen::Area)
        ));
        assert!(matches!(
            apply(&mut f, "area:3"),
            Action::Show(Screen::Area)
        ));
        assert_eq!(f.areas.len(), 2);
        let slugs = f.area_slugs();
        assert!(slugs.contains(&Area::ALL[0].slug()));
        assert!(slugs.contains(&Area::ALL[3].slug()));

        // Toggle the first back off.
        apply(&mut f, "area:0");
        assert_eq!(f.areas.len(), 1);

        // Clear returns to "all".
        assert!(matches!(
            apply(&mut f, "areaclear"),
            Action::Show(Screen::Area)
        ));
        assert_eq!(f.area_slugs(), vec!["se/stockholm"]);
    }

    #[test]
    fn apply_updates_and_routes() {
        let mut f = Filters::default();
        assert!(matches!(
            apply(&mut f, "menu:age"),
            Action::Show(Screen::Age)
        ));
        assert!(matches!(
            apply(&mut f, "age:48"),
            Action::Show(Screen::Main)
        ));
        assert_eq!(f.age_hours, 48);
        assert!(matches!(
            apply(&mut f, "minrent:10000"),
            Action::Show(Screen::Main)
        ));
        assert_eq!(f.min_rent, Some(10_000));
        assert!(matches!(
            apply(&mut f, "maxrent:0"),
            Action::Show(Screen::Main)
        ));
        assert_eq!(f.max_rent, None);
        assert!(matches!(apply(&mut f, "go"), Action::Search));
        assert!(matches!(apply(&mut f, "garbage"), Action::Ignore));
    }

    #[test]
    fn all_area_slugs_are_prefixed_and_unique() {
        let mut seen = std::collections::HashSet::new();
        for area in Area::ALL {
            assert!(area.slug().starts_with("se/"), "{}", area.slug());
            assert!(seen.insert(area.slug()), "duplicate slug {}", area.slug());
        }
        assert_eq!(Area::from_index(Area::ALL.len()), None);
    }
}
