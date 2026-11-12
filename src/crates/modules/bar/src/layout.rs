//! The `[layout]` section of `bar.toml` and its normalized form.
//!
//! A zone is an ordered list of entries. An entry is a widget name
//! (`"clock"`) or a table with `widget`, an optional `id` and that widget's
//! options, flat (`{ widget = "clock", id = "clock", show_seconds = false }`). Decoding is
//! a hand-written visitor rather than an untagged enum so type errors keep
//! their exact field path (`layout.end[2].widget`) and unknown keys inside an
//! entry table are reported as warnings like everywhere else.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use schemars::JsonSchema;
use serde::de::{self, IgnoredAny, IntoDeserializer, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use specta::Type;
use stache_sdk::ConfigError;

/// A widget the Bar can place in a layout zone.
#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    JsonSchema,
    Type,
)]
#[serde(rename_all = "snake_case")]
pub enum BarWidgetKind {
    /// The Tiling workspace strip.
    Spaces,
    /// The playing media.
    Media,
    /// The current weather.
    Weather,
    /// The CPU usage.
    Cpu,
    /// The battery state.
    Battery,
    /// The Keep Awake toggle.
    KeepAwake,
    /// The Wi-Fi state.
    Wifi,
    /// The clock.
    Clock,
}

impl BarWidgetKind {
    /// The `bar.toml` spelling, also the base of generated ids.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Spaces => "spaces",
            Self::Media => "media",
            Self::Weather => "weather",
            Self::Cpu => "cpu",
            Self::Battery => "battery",
            Self::KeepAwake => "keep_awake",
            Self::Wifi => "wifi",
            Self::Clock => "clock",
        }
    }
}

/// The entry options of a widget that has none: an empty table.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Type)]
pub struct NoOptions {}

/// Whether the clock counts hours 0 to 23 or 1 to 12 with a day period.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Type)]
#[serde(rename_all = "snake_case")]
pub enum HourCycle {
    /// 24-hour clock, `00` to `23`.
    #[default]
    H23,
    /// 12-hour clock with AM/PM.
    H12,
}

/// The `clock` entry options.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Type)]
// Defaults apply while decoding an entry (see `EntryVisitor`); the published
// item always carries every field.
#[serde(rename_all = "snake_case")]
#[schemars(default)]
pub struct ClockOptions {
    /// Whether the date is shown before the time.
    pub show_date: bool,
    /// Whether the time shows seconds.
    pub show_seconds: bool,
    /// `h23` (24-hour) or `h12` (12-hour with AM/PM).
    pub hour_cycle: HourCycle,
}

impl Default for ClockOptions {
    fn default() -> Self {
        Self {
            show_date: true,
            show_seconds: true,
            hour_cycle: HourCycle::H23,
        }
    }
}

/// The lowest and highest accepted `temperature_alert`, in degrees Celsius.
pub const TEMPERATURE_ALERT_RANGE: std::ops::RangeInclusive<u8> = 1..=150;

/// The `cpu` entry options.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Type)]
#[serde(rename_all = "snake_case")]
#[schemars(default)]
pub struct CpuOptions {
    /// The temperature, in degrees Celsius, at which the CPU item shows its
    /// alert (1 to 150).
    #[schemars(range(min = 1, max = 150))]
    pub temperature_alert: u8,
}

impl Default for CpuOptions {
    fn default() -> Self { Self { temperature_alert: 85 } }
}

/// The keys of an entry table besides `widget`: an optional `id` and the
/// widget's options, flat.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, JsonSchema, Type)]
#[serde(rename_all = "snake_case")]
#[schemars(rename = "BarEntry{O}")]
pub struct BarEntryFields<O> {
    /// A stable identity for this item: ASCII letters, digits, `-` or `_`,
    /// unique across all zones. Omitted, the widget name is used (then
    /// `<name>_2`, `<name>_3`, ... for repeats, in file order).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    // TOML has no null: the schema is a plain optional string.
    #[schemars(with = "String", length(min = 1), regex(pattern = r"^[A-Za-z0-9_-]+$"))]
    pub id: Option<String>,
    /// The widget's options; omitted keys take their defaults.
    #[serde(flatten)]
    pub options: O,
}

/// The table form of a layout entry, one shape per widget.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema, Type)]
#[serde(tag = "widget", rename_all = "snake_case")]
pub enum BarLayoutEntryTable {
    /// The Tiling workspace strip.
    Spaces(BarEntryFields<NoOptions>),
    /// The playing media.
    Media(BarEntryFields<NoOptions>),
    /// The current weather.
    Weather(BarEntryFields<NoOptions>),
    /// The CPU usage.
    Cpu(BarEntryFields<CpuOptions>),
    /// The battery state.
    Battery(BarEntryFields<NoOptions>),
    /// The Keep Awake toggle.
    KeepAwake(BarEntryFields<NoOptions>),
    /// The Wi-Fi state.
    Wifi(BarEntryFields<NoOptions>),
    /// The clock.
    Clock(BarEntryFields<ClockOptions>),
}

/// The resolved options of one entry, by widget.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Options {
    clock: ClockOptions,
    cpu: CpuOptions,
}

impl BarLayoutEntryTable {
    /// A table for `widget` with an optional id and default options.
    #[must_use]
    pub fn new(widget: BarWidgetKind, id: Option<String>) -> Self {
        Self::from_parts(widget, id, Options::default())
    }

    fn from_parts(widget: BarWidgetKind, id: Option<String>, options: Options) -> Self {
        let none = |id| BarEntryFields { id, options: NoOptions {} };
        match widget {
            BarWidgetKind::Spaces => Self::Spaces(none(id)),
            BarWidgetKind::Media => Self::Media(none(id)),
            BarWidgetKind::Weather => Self::Weather(none(id)),
            BarWidgetKind::Cpu => Self::Cpu(BarEntryFields { id, options: options.cpu }),
            BarWidgetKind::Battery => Self::Battery(none(id)),
            BarWidgetKind::KeepAwake => Self::KeepAwake(none(id)),
            BarWidgetKind::Wifi => Self::Wifi(none(id)),
            BarWidgetKind::Clock => Self::Clock(BarEntryFields { id, options: options.clock }),
        }
    }

    /// The placed widget.
    #[must_use]
    pub const fn widget(&self) -> BarWidgetKind {
        match self {
            Self::Spaces(_) => BarWidgetKind::Spaces,
            Self::Media(_) => BarWidgetKind::Media,
            Self::Weather(_) => BarWidgetKind::Weather,
            Self::Cpu(_) => BarWidgetKind::Cpu,
            Self::Battery(_) => BarWidgetKind::Battery,
            Self::KeepAwake(_) => BarWidgetKind::KeepAwake,
            Self::Wifi(_) => BarWidgetKind::Wifi,
            Self::Clock(_) => BarWidgetKind::Clock,
        }
    }

    /// The explicit id, if any.
    #[must_use]
    pub fn id(&self) -> Option<&str> {
        match self {
            Self::Spaces(fields)
            | Self::Media(fields)
            | Self::Weather(fields)
            | Self::Battery(fields)
            | Self::KeepAwake(fields)
            | Self::Wifi(fields) => fields.id.as_deref(),
            Self::Cpu(fields) => fields.id.as_deref(),
            Self::Clock(fields) => fields.id.as_deref(),
        }
    }

    fn options(&self) -> Options {
        let mut options = Options::default();
        match self {
            Self::Cpu(fields) => options.cpu = fields.options,
            Self::Clock(fields) => options.clock = fields.options,
            _ => {}
        }
        options
    }
}

/// One layout entry: a widget name, or a table with `widget`, an optional
/// `id` and that widget's options.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema, Type)]
#[serde(untagged)]
pub enum BarLayoutEntry {
    /// A widget name; its id is generated and its options are the defaults.
    Widget(BarWidgetKind),
    /// A widget with an optional explicit id and options.
    Table(BarLayoutEntryTable),
}

impl BarLayoutEntry {
    /// The placed widget.
    #[must_use]
    pub const fn widget(&self) -> BarWidgetKind {
        match self {
            Self::Widget(widget) => *widget,
            Self::Table(table) => table.widget(),
        }
    }

    /// The explicit id, if any.
    #[must_use]
    pub fn explicit_id(&self) -> Option<&str> {
        match self {
            Self::Widget(_) => None,
            Self::Table(table) => table.id(),
        }
    }

    fn options(&self) -> Options {
        match self {
            Self::Widget(_) => Options::default(),
            Self::Table(table) => table.options(),
        }
    }
}

/// The widget that owns an option key; option names are unique across widgets.
fn option_owner(key: &str) -> Option<BarWidgetKind> {
    match key {
        "show_date" | "show_seconds" | "hour_cycle" => Some(BarWidgetKind::Clock),
        "temperature_alert" => Some(BarWidgetKind::Cpu),
        _ => None,
    }
}

/// Whether `id` is a valid explicit layout id.
fn check_id(id: &str) -> Result<(), String> {
    if id.is_empty() {
        return Err("an id must not be empty".to_owned());
    }
    if !id
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(format!(
            "id `{id}` may contain only ASCII letters, digits, `-` or `_`"
        ));
    }
    Ok(())
}

fn check_temperature_alert(value: u8) -> Result<(), String> {
    if TEMPERATURE_ALERT_RANGE.contains(&value) {
        Ok(())
    } else {
        Err(format!(
            "temperature_alert must be between {} and {} degrees Celsius, got {value}",
            TEMPERATURE_ALERT_RANGE.start(),
            TEMPERATURE_ALERT_RANGE.end()
        ))
    }
}

/// An id decoded and checked in place, so a bad id reports `...id` as its path.
struct EntryId(String);

impl<'de> Deserialize<'de> for EntryId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let id = String::deserialize(deserializer)?;
        check_id(&id).map_err(de::Error::custom)?;
        Ok(Self(id))
    }
}

/// A `temperature_alert` decoded and checked in place.
struct TemperatureAlert(u8);

impl<'de> Deserialize<'de> for TemperatureAlert {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = u8::deserialize(deserializer)?;
        check_temperature_alert(value).map_err(de::Error::custom)?;
        Ok(Self(value))
    }
}

struct EntryVisitor;

impl EntryVisitor {
    /// Decodes one option value in place, so its errors keep their path.
    fn option<'de, A: MapAccess<'de>>(
        map: &mut A,
        key: &str,
        options: &mut Options,
    ) -> Result<(), A::Error> {
        match key {
            "show_date" => options.clock.show_date = map.next_value()?,
            "show_seconds" => options.clock.show_seconds = map.next_value()?,
            "hour_cycle" => options.clock.hour_cycle = map.next_value()?,
            "temperature_alert" => {
                options.cpu.temperature_alert = map.next_value::<TemperatureAlert>()?.0;
            }
            _ => {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(())
    }
}

impl<'de> Visitor<'de> for EntryVisitor {
    type Value = BarLayoutEntry;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a widget name or a table with `widget`, an optional `id` and options")
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
        BarWidgetKind::deserialize(value.into_deserializer()).map(BarLayoutEntry::Widget)
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut widget = None;
        let mut id = None;
        let mut options = Options::default();
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "widget" => {
                    if widget.is_some() {
                        return Err(de::Error::duplicate_field("widget"));
                    }
                    widget = Some(map.next_value::<BarWidgetKind>()?);
                }
                "id" => {
                    if id.is_some() {
                        return Err(de::Error::duplicate_field("id"));
                    }
                    id = Some(map.next_value::<EntryId>()?.0);
                }
                // An option read before `widget` is decoded for its owner; the
                // catalog has already removed (and warned about) another
                // widget's options, see `BarLayoutConfig::prune_foreign_options`.
                key if option_owner(key)
                    .is_some_and(|owner| widget.is_none_or(|widget| widget == owner)) =>
                {
                    Self::option(&mut map, key, &mut options)?;
                }
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        let widget = widget.ok_or_else(|| de::Error::missing_field("widget"))?;
        Ok(BarLayoutEntry::Table(BarLayoutEntryTable::from_parts(
            widget, id, options,
        )))
    }
}

impl<'de> Deserialize<'de> for BarLayoutEntry {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(EntryVisitor)
    }
}

/// The three ordered Bar zones. A missing `[layout]` is the default
/// arrangement; a zone missing from a present `[layout]` is empty.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Type)]
#[serde(rename_all = "snake_case")]
pub struct BarLayoutConfig {
    /// Items aligned to the leading edge, in order.
    #[serde(default)]
    pub start: Vec<BarLayoutEntry>,
    /// Items centered on the Bar, in order.
    #[serde(default)]
    pub center: Vec<BarLayoutEntry>,
    /// Items aligned to the trailing edge, in order.
    #[serde(default)]
    pub end: Vec<BarLayoutEntry>,
}

impl Default for BarLayoutConfig {
    fn default() -> Self {
        use BarWidgetKind::{Battery, Clock, Cpu, KeepAwake, Media, Spaces, Weather, Wifi};
        let zone =
            |kinds: &[BarWidgetKind]| kinds.iter().copied().map(BarLayoutEntry::Widget).collect();
        Self {
            start: zone(&[Spaces]),
            center: zone(&[Media]),
            end: zone(&[Weather, Cpu, Battery, KeepAwake, Wifi, Clock]),
        }
    }
}

/// One placed item as the UI receives it, tagged by `widget`, with its
/// resolved options (defaults applied; an empty object for a widget without
/// options).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(tag = "widget", rename_all = "snake_case")]
pub enum BarLayoutItem {
    /// The Tiling workspace strip.
    Spaces {
        /// The unique, stable id of the item.
        id: String,
        /// No options.
        options: NoOptions,
    },
    /// The playing media.
    Media {
        /// The unique, stable id of the item.
        id: String,
        /// No options.
        options: NoOptions,
    },
    /// The current weather.
    Weather {
        /// The unique, stable id of the item.
        id: String,
        /// No options.
        options: NoOptions,
    },
    /// The CPU usage.
    Cpu {
        /// The unique, stable id of the item.
        id: String,
        /// The resolved options.
        options: CpuOptions,
    },
    /// The battery state.
    Battery {
        /// The unique, stable id of the item.
        id: String,
        /// No options.
        options: NoOptions,
    },
    /// The Keep Awake toggle.
    KeepAwake {
        /// The unique, stable id of the item.
        id: String,
        /// No options.
        options: NoOptions,
    },
    /// The Wi-Fi state.
    Wifi {
        /// The unique, stable id of the item.
        id: String,
        /// No options.
        options: NoOptions,
    },
    /// The clock.
    Clock {
        /// The unique, stable id of the item.
        id: String,
        /// The resolved options.
        options: ClockOptions,
    },
}

impl BarLayoutItem {
    fn new(id: String, entry: &BarLayoutEntry) -> Self {
        let options = entry.options();
        let none = NoOptions {};
        match entry.widget() {
            BarWidgetKind::Spaces => Self::Spaces { id, options: none },
            BarWidgetKind::Media => Self::Media { id, options: none },
            BarWidgetKind::Weather => Self::Weather { id, options: none },
            BarWidgetKind::Cpu => Self::Cpu { id, options: options.cpu },
            BarWidgetKind::Battery => Self::Battery { id, options: none },
            BarWidgetKind::KeepAwake => Self::KeepAwake { id, options: none },
            BarWidgetKind::Wifi => Self::Wifi { id, options: none },
            BarWidgetKind::Clock => Self::Clock { id, options: options.clock },
        }
    }

    /// The unique, stable id of the item.
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            Self::Spaces { id, .. }
            | Self::Media { id, .. }
            | Self::Weather { id, .. }
            | Self::Cpu { id, .. }
            | Self::Battery { id, .. }
            | Self::KeepAwake { id, .. }
            | Self::Wifi { id, .. }
            | Self::Clock { id, .. } => id,
        }
    }

    /// The widget to render.
    #[must_use]
    pub const fn widget(&self) -> BarWidgetKind {
        match self {
            Self::Spaces { .. } => BarWidgetKind::Spaces,
            Self::Media { .. } => BarWidgetKind::Media,
            Self::Weather { .. } => BarWidgetKind::Weather,
            Self::Cpu { .. } => BarWidgetKind::Cpu,
            Self::Battery { .. } => BarWidgetKind::Battery,
            Self::KeepAwake { .. } => BarWidgetKind::KeepAwake,
            Self::Wifi { .. } => BarWidgetKind::Wifi,
            Self::Clock { .. } => BarWidgetKind::Clock,
        }
    }
}

/// The normalized layout: every item has a unique id.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct BarLayoutView {
    /// Leading-edge items, in order.
    pub start: Vec<BarLayoutItem>,
    /// Centered items, in order.
    pub center: Vec<BarLayoutItem>,
    /// Trailing-edge items, in order.
    pub end: Vec<BarLayoutItem>,
}

impl BarLayoutConfig {
    fn zones(&self) -> [(&'static str, &[BarLayoutEntry]); 3] {
        [
            ("start", &self.start),
            ("center", &self.center),
            ("end", &self.end),
        ]
    }

    /// Rejects an invalid or duplicate explicit id, naming its field path.
    ///
    /// # Errors
    /// Returns the first offending entry's path and reason.
    pub fn validate(&self) -> Result<(), ConfigError> {
        let mut seen: BTreeMap<&str, String> = BTreeMap::new();
        for (zone, entries) in self.zones() {
            for (index, entry) in entries.iter().enumerate() {
                let alert = entry.options().cpu.temperature_alert;
                check_temperature_alert(alert).map_err(|reason| {
                    ConfigError::new(format!("layout.{zone}[{index}].temperature_alert: {reason}"))
                })?;
                let Some(id) = entry.explicit_id() else {
                    continue;
                };
                let path = format!("layout.{zone}[{index}].id");
                check_id(id).map_err(|reason| ConfigError::new(format!("{path}: {reason}")))?;
                if let Some(first) = seen.get(id) {
                    return Err(ConfigError::new(format!(
                        "{path}: duplicate id `{id}` (already used at {first})"
                    )));
                }
                seen.insert(id, path);
            }
        }
        Ok(())
    }

    /// Gives every entry an id: an explicit id is kept; otherwise the widget
    /// name, then `<name>_2`, `<name>_3`, ... in file order across all zones,
    /// skipping every explicit id.
    #[must_use]
    pub fn normalized(&self) -> BarLayoutView {
        let mut used: BTreeSet<String> = self
            .zones()
            .iter()
            .flat_map(|(_, entries)| entries.iter().filter_map(BarLayoutEntry::explicit_id))
            .map(str::to_owned)
            .collect();
        let mut place = |entries: &[BarLayoutEntry]| -> Vec<BarLayoutItem> {
            entries
                .iter()
                .map(|entry| {
                    let widget = entry.widget();
                    let id = entry.explicit_id().map_or_else(
                        || {
                            let name = widget.as_str();
                            let mut candidate = name.to_owned();
                            let mut counter = 1_u32;
                            while used.contains(&candidate) {
                                counter += 1;
                                candidate = format!("{name}_{counter}");
                            }
                            used.insert(candidate.clone());
                            candidate
                        },
                        str::to_owned,
                    );
                    BarLayoutItem::new(id, entry)
                })
                .collect()
        };
        let start = place(&self.start);
        let center = place(&self.center);
        let end = place(&self.end);
        BarLayoutView { start, center, end }
    }
}

/// Removes from each layout entry table the options of another widget and
/// returns their paths (`layout.end.1.show_seconds`) as unknown-key warnings.
///
/// The decoder reads an entry's keys alphabetically, so an option arrives
/// before `widget` and cannot be told apart from the entry's own options in
/// place; this runs before decoding instead. An entry whose `widget` is
/// missing or unknown is left for the decoder to reject.
#[must_use]
pub fn prune_foreign_options(section: &mut serde_json::Value) -> Vec<String> {
    let mut warnings = Vec::new();
    let Some(layout) = section.get_mut("layout").and_then(serde_json::Value::as_object_mut) else {
        return warnings;
    };
    for zone in ["start", "center", "end"] {
        let Some(entries) = layout.get_mut(zone).and_then(serde_json::Value::as_array_mut) else {
            continue;
        };
        for (index, entry) in entries.iter_mut().enumerate() {
            let Some(table) = entry.as_object_mut() else {
                continue;
            };
            let Some(widget) =
                table.get("widget").and_then(|widget| BarWidgetKind::deserialize(widget).ok())
            else {
                continue;
            };
            let foreign: Vec<String> = table
                .keys()
                .filter(|key| option_owner(key).is_some_and(|owner| owner != widget))
                .cloned()
                .collect();
            for key in foreign {
                table.remove(&key);
                warnings.push(format!("layout.{zone}.{index}.{key}"));
            }
        }
    }
    warnings
}
