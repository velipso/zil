mod default_colors;
mod fallback_themes;
mod font_family_cache;
mod registry;
mod scale;
mod schema;
mod styles;
mod theme_settings_provider;
mod ui_density;

use std::{collections::HashMap, sync::Arc};
use serde::Deserialize;
use serde_yaml_ng::{Value as YamlValue, Mapping as YamlMapping};

use gpui::BorrowAppContext;
use gpui::Global;
use gpui::{
    App, AssetSource, Hsla, Pixels, Rgba, SharedString, WindowAppearance,
    WindowBackgroundAppearance, px,
};

pub use crate::default_colors::*;
pub use crate::fallback_themes::{apply_status_color_defaults, apply_theme_color_defaults};
pub use crate::font_family_cache::*;
pub use crate::registry::*;
pub use crate::scale::*;
pub use crate::schema::*;
pub use crate::styles::*;
pub use crate::theme_settings_provider::*;
pub use crate::ui_density::*;

/// The name of the default dark theme.
pub const DEFAULT_DARK_THEME: &str = "One Dark";

/// Defines window border radius for platforms that use client side decorations.
pub const CLIENT_SIDE_DECORATION_ROUNDING: Pixels = px(10.0);
/// Defines window shadow size for platforms that use client side decorations.
pub const CLIENT_SIDE_DECORATION_SHADOW: Pixels = px(10.0);

/// The appearance of the theme.
#[derive(Debug, PartialEq, Clone, Copy, Deserialize)]
pub enum Appearance {
    /// A light appearance.
    Light,
    /// A dark appearance.
    Dark,
}

impl Appearance {
    /// Returns whether the appearance is light.
    pub fn is_light(&self) -> bool {
        match self {
            Self::Light => true,
            Self::Dark => false,
        }
    }
}

impl From<WindowAppearance> for Appearance {
    fn from(value: WindowAppearance) -> Self {
        match value {
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Self::Dark,
            WindowAppearance::Light | WindowAppearance::VibrantLight => Self::Light,
        }
    }
}

/// Which themes should be loaded. This is used primarily for testing.
pub enum LoadThemes {
    /// Only load the base theme.
    ///
    /// No user themes will be loaded.
    JustBase,

    /// Load all of the built-in themes.
    All(Box<dyn AssetSource>),
}

/// Initialize the theme system with default themes.
///
/// This sets up the [`ThemeRegistry`], [`FontFamilyCache`], [`SystemAppearance`],
/// and [`GlobalTheme`] with the default dark theme. It does NOT load bundled
/// themes from JSON or integrate with settings — use `theme_settings::init` for that.
pub fn init(themes_to_load: LoadThemes, cx: &mut App) {
    SystemAppearance::init(cx);
    let assets = match themes_to_load {
        LoadThemes::JustBase => Box::new(()) as Box<dyn AssetSource>,
        LoadThemes::All(assets) => assets,
    };
    ThemeRegistry::set_global(assets, cx);
    FontFamilyCache::init_global(cx);

    let themes = ThemeRegistry::default_global(cx);
    let theme = themes.get(DEFAULT_DARK_THEME).unwrap_or_else(|_| {
        themes
            .list()
            .into_iter()
            .next()
            .map(|m| themes.get(&m.name).unwrap())
            .unwrap()
    });
    cx.set_global(GlobalTheme { theme });
}

/// Implementing this trait allows accessing the active theme.
pub trait ActiveTheme {
    /// Returns the active theme.
    fn theme(&self) -> &Arc<Theme>;
}

impl ActiveTheme for App {
    fn theme(&self) -> &Arc<Theme> {
        GlobalTheme::theme(self)
    }
}

/// The appearance of the system.
#[derive(Debug, Clone, Copy)]
pub struct SystemAppearance(pub Appearance);

impl std::ops::Deref for SystemAppearance {
    type Target = Appearance;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Default for SystemAppearance {
    fn default() -> Self {
        Self(Appearance::Dark)
    }
}

#[derive(Default)]
struct GlobalSystemAppearance(SystemAppearance);

impl std::ops::DerefMut for GlobalSystemAppearance {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl std::ops::Deref for GlobalSystemAppearance {
    type Target = SystemAppearance;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Global for GlobalSystemAppearance {}

impl SystemAppearance {
    /// Initializes the [`SystemAppearance`] for the application.
    pub fn init(cx: &mut App) {
        *cx.default_global::<GlobalSystemAppearance>() =
            GlobalSystemAppearance(SystemAppearance(cx.window_appearance().into()));
    }

    /// Returns the global [`SystemAppearance`].
    pub fn global(cx: &App) -> Self {
        cx.global::<GlobalSystemAppearance>().0
    }

    /// Returns a mutable reference to the global [`SystemAppearance`].
    pub fn global_mut(cx: &mut App) -> &mut Self {
        cx.global_mut::<GlobalSystemAppearance>()
    }
}

/// A theme family is a grouping of themes under a single name.
///
/// For example, the "One" theme family contains the "One Light" and "One Dark" themes.
///
/// It can also be used to package themes with many variants.
///
/// For example, the "Atelier" theme family contains "Cave", "Dune", "Estuary", "Forest", "Heath", etc.
pub struct ThemeFamily {
    /// The unique identifier for the theme family.
    pub id: String,
    /// The name of the theme family. This will be displayed in the UI, such as when adding or removing a theme family.
    pub name: SharedString,
    /// The author of the theme family.
    pub author: SharedString,
    /// The [Theme]s in the family.
    pub themes: Vec<Theme>,
    /// The color scales used by the themes in the family.
    /// Note: This will be removed in the future.
    pub scales: ColorScales,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Tinted8Scheme {
    name: String,
    slug: String,
    author: String,
    theme_author: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Tinted8Palette {
    pub black_dim: Rgba,
    pub black: Rgba,
    pub black_bright: Rgba,

    pub red_dim: Rgba,
    pub red: Rgba,
    pub red_bright: Rgba,

    pub green_dim: Rgba,
    pub green: Rgba,
    pub green_bright: Rgba,

    pub yellow_dim: Rgba,
    pub yellow: Rgba,
    pub yellow_bright: Rgba,

    pub blue_dim: Rgba,
    pub blue: Rgba,
    pub blue_bright: Rgba,

    pub magenta_dim: Rgba,
    pub magenta: Rgba,
    pub magenta_bright: Rgba,

    pub cyan_dim: Rgba,
    pub cyan: Rgba,
    pub cyan_bright: Rgba,

    pub white_dim: Rgba,
    pub white: Rgba,
    pub white_bright: Rgba,

    pub gray_dim: Rgba,
    pub gray: Rgba,
    pub gray_bright: Rgba,

    pub orange_dim: Rgba,
    pub orange: Rgba,
    pub orange_bright: Rgba,

    pub brown_dim: Rgba,
    pub brown: Rgba,
    pub brown_bright: Rgba,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Tinted8Syntax {
    pub comment: Rgba,
    pub comment_block: Rgba,
    pub comment_documentation: Rgba,
    pub comment_line: Rgba,

    pub constant: Rgba,
    pub constant_character: Rgba,
    pub constant_character_entity: Rgba,
    pub constant_character_escape: Rgba,
    pub constant_language: Rgba,
    pub constant_numeric: Rgba,
    pub constant_numeric_float: Rgba,
    pub constant_numeric_hex: Rgba,
    pub constant_numeric_integer: Rgba,
    pub constant_other: Rgba,

    pub entity: Rgba,
    pub entity_name: Rgba,
    pub entity_name_class: Rgba,
    pub entity_name_function: Rgba,
    pub entity_name_function_constructor: Rgba,
    pub entity_name_label: Rgba,
    pub entity_name_namespace: Rgba,
    pub entity_name_section: Rgba,
    pub entity_name_tag: Rgba,
    pub entity_name_type: Rgba,
    pub entity_name_type_class: Rgba,
    pub entity_name_type_enum: Rgba,
    pub entity_other: Rgba,
    pub entity_other_attributename: Rgba,
    pub entity_other_inheritedclass: Rgba,

    pub invalid: Rgba,
    pub invalid_deprecated: Rgba,
    pub invalid_illegal: Rgba,

    pub keyword: Rgba,
    pub keyword_control: Rgba,
    pub keyword_control_flow: Rgba,
    pub keyword_control_import: Rgba,
    pub keyword_declaration: Rgba,
    pub keyword_operator: Rgba,
    pub keyword_other: Rgba,

    pub markup: Rgba,
    pub markup_bold: Rgba,
    pub markup_changed: Rgba,
    pub markup_deleted: Rgba,
    pub markup_heading: Rgba,
    pub markup_inserted: Rgba,
    pub markup_italic: Rgba,
    pub markup_link: Rgba,
    pub markup_list: Rgba,
    pub markup_list_numbered: Rgba,
    pub markup_list_unnumbered: Rgba,
    pub markup_quote: Rgba,
    pub markup_raw: Rgba,
    pub markup_underline: Rgba,

    pub meta: Rgba,
    pub meta_annotation: Rgba,
    pub meta_block: Rgba,
    pub meta_class: Rgba,
    pub meta_embedded: Rgba,
    pub meta_function: Rgba,
    pub meta_import: Rgba,
    pub meta_object: Rgba,
    pub meta_preprocessor: Rgba,
    pub meta_tag: Rgba,
    pub meta_type: Rgba,

    pub punctuation: Rgba,
    pub punctuation_definition: Rgba,
    pub punctuation_definition_comment: Rgba,
    pub punctuation_definition_string: Rgba,
    pub punctuation_section: Rgba,
    pub punctuation_separator: Rgba,

    pub source: Rgba,

    pub storage: Rgba,
    pub storage_modifier: Rgba,
    pub storage_type: Rgba,

    pub string: Rgba,
    pub string_interpolated: Rgba,
    pub string_other: Rgba,
    pub string_quoted: Rgba,
    pub string_quoted_double: Rgba,
    pub string_quoted_single: Rgba,
    pub string_regexp: Rgba,
    pub string_template: Rgba,
    pub string_unquoted: Rgba,

    pub support: Rgba,
    pub support_class: Rgba,
    pub support_constant: Rgba,
    pub support_function: Rgba,
    pub support_function_builtin: Rgba,
    pub support_other: Rgba,
    pub support_type: Rgba,
    pub support_variable: Rgba,

    pub text: Rgba,

    pub variable: Rgba,
    pub variable_language: Rgba,
    pub variable_other: Rgba,
    pub variable_other_constant: Rgba,
    pub variable_other_object: Rgba,
    pub variable_other_object_property: Rgba,
    pub variable_parameter: Rgba,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Tinted8Ui {
    pub accent_normal: Rgba,

    pub border_normal: Rgba,

    pub chrome_background_dark: Rgba,
    pub chrome_background_light: Rgba,
    pub chrome_background_normal: Rgba,
    pub chrome_foreground_dark: Rgba,
    pub chrome_foreground_light: Rgba,

    pub cursor_muted_background: Rgba,
    pub cursor_muted_foreground: Rgba,
    pub cursor_normal_background: Rgba,
    pub cursor_normal_foreground: Rgba,

    pub deprecated: Rgba,

    pub global_background_dark: Rgba,
    pub global_background_light: Rgba,
    pub global_background_normal: Rgba,
    pub global_foreground_dark: Rgba,
    pub global_foreground_light: Rgba,
    pub global_foreground_normal: Rgba,

    pub gutter_background: Rgba,
    pub gutter_foreground: Rgba,

    pub highlight_button_background: Rgba,
    pub highlight_button_foreground: Rgba,
    pub highlight_line_background: Rgba,
    pub highlight_line_foreground: Rgba,
    pub highlight_search_background: Rgba,
    pub highlight_search_foreground: Rgba,
    pub highlight_text_activebackground: Rgba,
    pub highlight_text_activeforeground: Rgba,
    pub highlight_text_background: Rgba,
    pub highlight_text_foreground: Rgba,

    pub indentguide_activebackground: Rgba,
    pub indentguide_background: Rgba,

    pub link_normal: Rgba,

    pub selection_background: Rgba,
    pub selection_foreground: Rgba,
    pub selection_inactivebackground: Rgba,

    pub status_error: Rgba,
    pub status_info: Rgba,
    pub status_success: Rgba,
    pub status_warning: Rgba,

    pub tooltip_background: Rgba,
    pub tooltip_foreground: Rgba,

    pub whitespace_foreground: Rgba,
}

/// A theme is the primary mechanism for defining the appearance of the UI.
#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    /// The unique identifier for the theme.
    pub id: String,
    /// The name of the theme.
    pub name: SharedString,
    /// The appearance of the theme (light or dark).
    pub appearance: Appearance,
    /// The colors and other styles for the theme.
    pub styles: ThemeStyles,

    pub scheme: Tinted8Scheme,
    pub palette: Tinted8Palette,
    pub syntax: Tinted8Syntax,
    pub ui: Tinted8Ui,
}

impl Theme {
    pub fn parse_yaml(
        data: &str
    ) -> Result<(Tinted8Scheme, Tinted8Palette, Tinted8Syntax, Tinted8Ui), String> {
        let partial: YamlValue = serde_yaml_ng::from_str(data).map_err(|err| {
            format!("Failed to parse YAML: {err}")
        })?;

        fn join_prefix(prefix: &String, key: &String) -> String {
            if prefix.as_str() == "" {
                format!("{key}")
            } else {
                format!("{prefix}.{key}")
            }
        }

        fn flatten(prefix: &String, map: &YamlMapping, out: &mut HashMap<String, String>) {
            for (key, val) in map {
                if let YamlValue::String(key) = key {
                    insert_value(prefix, key, val, out);
                }
            }
        }

        fn value_to_string(val: &YamlValue) -> Option<String> {
            match val {
                YamlValue::Null => Some("null".to_string()),
                YamlValue::Bool(b) => Some(format!("{b}")),
                YamlValue::Number(n) => Some(format!("{n}")),
                YamlValue::String(s) => Some(s.clone()),
                YamlValue::Tagged(t) => value_to_string(&t.value),
                _ => None,
            }
        }

        fn insert_value(prefix: &String, key: &String, val: &YamlValue, out: &mut HashMap<String, String>) {
            if let Some(val) = match val {
                    YamlValue::Mapping(map) => {
                        flatten(&join_prefix(prefix, key), &map, out);
                        None
                    },
                    _ => value_to_string(&val)
                }
            {
                out.insert(join_prefix(prefix, key), val);
            }
        }

        fn get_map(val: &YamlValue) -> Option<&YamlMapping> {
            match val {
                YamlValue::Tagged(t) => get_map(&t.value),
                YamlValue::Mapping(m) => Some(&m),
                _ => None,
            }
        }

        let mut scheme = HashMap::<String, String>::new();
        let mut variant: Option<Appearance> = None;
        let mut palette = HashMap::<String, String>::new();
        let mut syntax = HashMap::<String, String>::new();
        let mut ui = HashMap::<String, String>::new();

        match partial {
            YamlValue::Mapping(map) => {
                for (key, val) in map {
                    match key {
                        YamlValue::String(s) => {
                            if s.as_str() == "variant" {
                                if let Some(variant_str) = value_to_string(&val) {
                                    variant = match variant_str.as_str() {
                                        "dark" => Ok(Some(Appearance::Dark)),
                                        "light" => Ok(Some(Appearance::Light)),
                                        _ => Err(format!("Invalid 'variant'; expecting \"dark\" or \"light\", but found: {variant_str}")),
                                    }?;
                                }
                            } else if let Some(map) = get_map(&val) {
                                match s.as_str() {
                                    "scheme" => { flatten(&"".to_string(), &map, &mut scheme); },
                                    "palette" => { flatten(&"".to_string(), &map, &mut palette); },
                                    "syntax" => { flatten(&"".to_string(), &map, &mut syntax); },
                                    "ui" => { flatten(&"".to_string(), &map, &mut ui); },
                                    _ => {}, // ignore
                                }
                            }
                        }
                        _ => {}, // ignore
                    }
                }
            },
            _ => return Err("Invalid YAML structure".to_string()),
        }

        let name = scheme
            .get("name")
            .cloned()
            .or_else(|| scheme.get("slug").cloned())
            .or_else(|| {
                scheme.get("family").as_ref().map(|family| {
                    if let Some(style) = &scheme.get("style") {
                        format!("{family} {style}")
                    } else {
                        family.to_string()
                    }
                })
            })
            .ok_or_else(||
                "Missing one of 'scheme.name', 'scheme.slug', or 'scheme.family'".to_string()
            )?;

        let slug = deunicode::deunicode(scheme.get("slug").unwrap_or(&name))
            .to_lowercase()
            .chars()
            .filter_map(|c| {
                if c.is_ascii_alphanumeric() {
                    Some(c)
                } else if c.is_ascii_whitespace() || c == '-' {
                    Some('-')
                } else {
                    None
                }
            })
            .collect::<String>();

        let author = scheme.get("author").cloned().unwrap_or_else(|| "Unknown".to_string());
        let theme_author = scheme.get("theme-author").cloned();
        let variant = variant.ok_or_else(|| "Missing 'variant' (expecting \"dark\" or \"light\")")?;

        let scheme = Tinted8Scheme {
            name,
            slug,
            author,
            theme_author,
        };

        fn rgba_from_hex(hex: &String) -> Option<Rgba> {
            let hex = hex.strip_prefix('#').unwrap_or(&hex);

            fn parse_pair(s: &str) -> Option<u8> {
                u8::from_str_radix(s, 16).ok()
            }

            fn expand(c: char) -> Option<u8> {
                c.to_digit(16).map(|n| (n as u8) * 17)
            }

            let (r, g, b, a) = match hex.len() {
                3 => {
                    let mut chars = hex.chars();
                    (
                        expand(chars.next()?)?,
                        expand(chars.next()?)?,
                        expand(chars.next()?)?,
                        255,
                    )
                }
                4 => {
                    let mut chars = hex.chars();
                    (
                        expand(chars.next()?)?,
                        expand(chars.next()?)?,
                        expand(chars.next()?)?,
                        expand(chars.next()?)?,
                    )
                }
                6 => (
                    parse_pair(&hex[0..2])?,
                    parse_pair(&hex[2..4])?,
                    parse_pair(&hex[4..6])?,
                    255,
                ),
                8 => (
                    parse_pair(&hex[0..2])?,
                    parse_pair(&hex[2..4])?,
                    parse_pair(&hex[4..6])?,
                    parse_pair(&hex[6..8])?,
                ),
                _ => return None,
            };

            Some(Rgba {
                r: r as f32 / 255.,
                g: g as f32 / 255.,
                b: b as f32 / 255.,
                a: a as f32 / 255.,
            })
        }

        fn palette_hex(palette: &HashMap<String, String>, field: &str) -> Result<Rgba, String> {
            let hex = palette.get(field)
                .ok_or_else(|| format!("Missing 'palette.{field}'"))?;
            Ok(rgba_from_hex(hex)
                .ok_or_else(|| format!("Invalid color for '{field}': \"{hex}\""))?
            )
        }

        fn gray_from_black_white(black: Rgba, white: Rgba) -> Rgba {
            let Hsla { h: h_b, s: s_b, l: l_b, a: a_b } = Hsla::from(black);
            let Hsla { h: h_w, s: s_w, l: l_w, a: a_w } = Hsla::from(white);
            let h_b = if h_b.is_nan() { h_w } else { h_b };
            let h_w = if h_w.is_nan() { h_b } else { h_w };

            let s = 0.5 * (s_b + s_w);
            let d = (h_b - h_w + 0.5).rem_euclid(1.0) - 0.5;
            let h = (h_w + 0.5 * d).rem_euclid(1.0);
            let l = 0.5 * (l_b + l_w);
            let a = 0.5 * (a_b + a_w);

            Rgba::from(Hsla { h, s, l, a })
        }

        fn orange_from_yellow(yellow: Rgba) -> Rgba {
            let Hsla { h, s, l, a } = Hsla::from(yellow);
            let h = (h + 350. / 360.).rem_euclid(1.0);
            Rgba::from(Hsla { h, s, l, a })
        }

        fn brown_from_yellow(yellow: Rgba) -> Rgba {
            let Hsla { h, s, l, a } = Hsla::from(yellow);
            let h = (h + 345. / 360.).rem_euclid(1.0);
            let s = (s * 0.65).clamp(0., 1.);
            let l = (l - 0.3).clamp(0., 1.);
            Rgba::from(Hsla { h, s, l, a })
        }

        let black = palette_hex(&palette, "black")?;
        let red = palette_hex(&palette, "red")?;
        let green = palette_hex(&palette, "green")?;
        let yellow = palette_hex(&palette, "yellow")?;
        let blue = palette_hex(&palette, "blue")?;
        let magenta = palette_hex(&palette, "magenta")?;
        let cyan = palette_hex(&palette, "cyan")?;
        let white = palette_hex(&palette, "white")?;
        let gray = palette_hex(&palette, "gray")
            .unwrap_or_else(|_| gray_from_black_white(black, white));
        let orange = palette_hex(&palette, "orange")
            .unwrap_or_else(|_| orange_from_yellow(yellow));
        let brown = palette_hex(&palette, "brown")
            .unwrap_or_else(|_| brown_from_yellow(yellow));

        fn make_bright(color: Rgba) -> Rgba {
            let dl = 0.12 as f32;
            let Hsla { h, s, l, a } = Hsla::from(color);

            let k = if l < 0.5 { 1.08 } else if l < 0.8 { 1. } else { 0.9 };
            let s = (s * k).clamp(0., 1.);
            let l = (l + dl.min(1. - l)).clamp(0., 1.);

            Rgba::from(Hsla { h, s, l, a })
        }

        fn make_dim(color: Rgba) -> Rgba {
            let dl = 0.12 as f32;
            let Hsla { h, s, l, a } = Hsla::from(color);

            let k = if l < 0.4 { 1.04 } else if l < 0.7 { 1.07 } else { 1.1 };
            let s = (s * k).clamp(0., 1.);
            let l = (l - dl.min(l)).clamp(0., 1.);

            Rgba::from(Hsla { h, s, l, a })
        }

        let black_dim = palette_hex(&palette, "black-dim")
            .unwrap_or_else(|_| make_dim(black));
        let black_bright = palette_hex(&palette, "black-bright")
            .unwrap_or_else(|_| make_bright(black));
        let red_dim = palette_hex(&palette, "red-dim")
            .unwrap_or_else(|_| make_dim(red));
        let red_bright = palette_hex(&palette, "red-bright")
            .unwrap_or_else(|_| make_bright(red));
        let green_dim = palette_hex(&palette, "green-dim")
            .unwrap_or_else(|_| make_dim(green));
        let green_bright = palette_hex(&palette, "green-bright")
            .unwrap_or_else(|_| make_bright(green));
        let yellow_dim = palette_hex(&palette, "yellow-dim")
            .unwrap_or_else(|_| make_dim(yellow));
        let yellow_bright = palette_hex(&palette, "yellow-bright")
            .unwrap_or_else(|_| make_bright(yellow));
        let blue_dim = palette_hex(&palette, "blue-dim")
            .unwrap_or_else(|_| make_dim(blue));
        let blue_bright = palette_hex(&palette, "blue-bright")
            .unwrap_or_else(|_| make_bright(blue));
        let magenta_dim = palette_hex(&palette, "magenta-dim")
            .unwrap_or_else(|_| make_dim(magenta));
        let magenta_bright = palette_hex(&palette, "magenta-bright")
            .unwrap_or_else(|_| make_bright(magenta));
        let cyan_dim = palette_hex(&palette, "cyan-dim")
            .unwrap_or_else(|_| make_dim(cyan));
        let cyan_bright = palette_hex(&palette, "cyan-bright")
            .unwrap_or_else(|_| make_bright(cyan));
        let white_dim = palette_hex(&palette, "white-dim")
            .unwrap_or_else(|_| make_dim(white));
        let white_bright = palette_hex(&palette, "white-bright")
            .unwrap_or_else(|_| make_bright(white));
        let gray_dim = palette_hex(&palette, "gray-dim")
            .unwrap_or_else(|_| make_dim(gray));
        let gray_bright = palette_hex(&palette, "gray-bright")
            .unwrap_or_else(|_| make_bright(gray));
        let orange_dim = palette_hex(&palette, "orange-dim")
            .unwrap_or_else(|_| make_dim(orange));
        let orange_bright = palette_hex(&palette, "orange-bright")
            .unwrap_or_else(|_| make_bright(orange));
        let brown_dim = palette_hex(&palette, "brown-dim")
            .unwrap_or_else(|_| make_dim(brown));
        let brown_bright = palette_hex(&palette, "brown-bright")
            .unwrap_or_else(|_| make_bright(brown));

        let palette = Tinted8Palette {
            black_dim,
            black,
            black_bright,

            red_dim,
            red,
            red_bright,

            green_dim,
            green,
            green_bright,

            yellow_dim,
            yellow,
            yellow_bright,

            blue_dim,
            blue,
            blue_bright,

            magenta_dim,
            magenta,
            magenta_bright,

            cyan_dim,
            cyan,
            cyan_bright,

            white_dim,
            white,
            white_bright,

            gray_dim,
            gray,
            gray_bright,

            orange_dim,
            orange,
            orange_bright,

            brown_dim,
            brown,
            brown_bright,
        };

        let (
            shade1,
            shade2,
            shade3,
            shade4,
            shade5,
            shade6,
            shade7,
            shade8,
            shade9,
        ) = match variant {
            Appearance::Dark => (
                /* 1 */ black_dim,
                /* 2 */ black,
                /* 3 */ black_bright,
                /* 4 */ gray_dim,
                /* 5 */ gray,
                /* 6 */ gray_bright,
                /* 7 */ white_dim,
                /* 8 */ white,
                /* 9 */ white_bright,
            ),
            Appearance::Light => (
                /* 1 */ white_bright,
                /* 2 */ white,
                /* 3 */ white_dim,
                /* 4 */ gray_bright,
                /* 5 */ gray,
                /* 6 */ gray_dim,
                /* 7 */ black_bright,
                /* 8 */ black,
                /* 9 */ black_dim,
            ),
        };

        fn sy_col(
            syntax: &HashMap<String, String>,
            key: &str,
            default_color: Rgba
        ) -> Result<Rgba, String> {
            let mut key = key.to_string();

            loop {
                if let Some(hex) = syntax.get(&key) {
                    return rgba_from_hex(hex)
                        .ok_or_else(|| format!("Invalid color for 'syntax.{key}': \"{hex}\""));
                } else {
                    if let Some((parent, _)) = key.rsplit_once('.') {
                        key = parent.to_string();
                    } else {
                        return Ok(default_color);
                    }
                }
            }
        }

        let syntax = Tinted8Syntax {
            comment: sy_col(&syntax, "comment", shade4)?,
            comment_block: sy_col(&syntax, "comment.block", shade4)?,
            comment_documentation: sy_col(&syntax, "comment.documentation", shade4)?,
            comment_line: sy_col(&syntax, "comment.line", shade4)?,

            constant: sy_col(&syntax, "constant", orange)?,
            constant_character: sy_col(&syntax, "constant.character", orange)?,
            constant_character_entity: sy_col(&syntax, "constant.character.entity", orange)?,
            constant_character_escape: sy_col(&syntax, "constant.character.escape", orange)?,
            constant_language: sy_col(&syntax, "constant.language", orange)?,
            constant_numeric: sy_col(&syntax, "constant.numeric", orange)?,
            constant_numeric_float: sy_col(&syntax, "constant.numeric.float", orange)?,
            constant_numeric_hex: sy_col(&syntax, "constant.numeric.hex", orange)?,
            constant_numeric_integer: sy_col(&syntax, "constant.numeric.integer", orange)?,
            constant_other: sy_col(&syntax, "constant.other", orange)?,

            entity: sy_col(&syntax, "entity", shade8)?,
            entity_name: sy_col(&syntax, "entity.name", shade8)?,
            entity_name_class: sy_col(&syntax, "entity.name.class", yellow)?,
            entity_name_function: sy_col(&syntax, "entity.name.function", blue)?,
            entity_name_function_constructor: sy_col(&syntax, "entity.name.function.constructor", blue)?,
            entity_name_label: sy_col(&syntax, "entity.name.label", shade8)?,
            entity_name_namespace: sy_col(&syntax, "entity.name.namespace", yellow)?,
            entity_name_section: sy_col(&syntax, "entity.name.section", cyan)?,
            entity_name_tag: sy_col(&syntax, "entity.name.tag", shade8)?,
            entity_name_type: sy_col(&syntax, "entity.name.type", cyan)?,
            entity_name_type_class: sy_col(&syntax, "entity.name.type.class", cyan)?,
            entity_name_type_enum: sy_col(&syntax, "entity.name.type.enum", cyan)?,
            entity_other: sy_col(&syntax, "entity.other", shade8)?,
            entity_other_attributename: sy_col(&syntax, "entity.other.attribute-name", magenta)?,
            entity_other_inheritedclass: sy_col(&syntax, "entity.other.inherited-class", shade8)?,

            invalid: sy_col(&syntax, "invalid", red_bright)?,
            invalid_deprecated: sy_col(&syntax, "invalid.deprecated", yellow_bright)?,
            invalid_illegal: sy_col(&syntax, "invalid.illegal", red_bright)?,

            keyword: sy_col(&syntax, "keyword", magenta)?,
            keyword_control: sy_col(&syntax, "keyword.control", magenta)?,
            keyword_control_flow: sy_col(&syntax, "keyword.control.flow", magenta)?,
            keyword_control_import: sy_col(&syntax, "keyword.control.import", magenta)?,
            keyword_declaration: sy_col(&syntax, "keyword.declaration", magenta)?,
            keyword_operator: sy_col(&syntax, "keyword.operator", magenta)?,
            keyword_other: sy_col(&syntax, "keyword.other", magenta)?,

            markup: sy_col(&syntax, "markup", orange)?,
            markup_bold: sy_col(&syntax, "markup.bold", orange)?,
            markup_changed: sy_col(&syntax, "markup.changed", yellow_bright)?,
            markup_deleted: sy_col(&syntax, "markup.deleted", red_bright)?,
            markup_heading: sy_col(&syntax, "markup.heading", magenta)?,
            markup_inserted: sy_col(&syntax, "markup.inserted", green_bright)?,
            markup_italic: sy_col(&syntax, "markup.italic", orange)?,
            markup_link: sy_col(&syntax, "markup.link", yellow)?,
            markup_list: sy_col(&syntax, "markup.list", orange)?,
            markup_list_numbered: sy_col(&syntax, "markup.list.numbered", cyan)?,
            markup_list_unnumbered: sy_col(&syntax, "markup.list.unnumbered", cyan)?,
            markup_quote: sy_col(&syntax, "markup.quote", orange)?,
            markup_raw: sy_col(&syntax, "markup.raw", orange)?,
            markup_underline: sy_col(&syntax, "markup.underline", orange)?,

            meta: sy_col(&syntax, "meta", shade8)?,
            meta_annotation: sy_col(&syntax, "meta.annotation", orange)?,
            meta_block: sy_col(&syntax, "meta.block", shade8)?,
            meta_class: sy_col(&syntax, "meta.class", shade8)?,
            meta_embedded: sy_col(&syntax, "meta.embedded", shade8)?,
            meta_function: sy_col(&syntax, "meta.function", shade8)?,
            meta_import: sy_col(&syntax, "meta.import", shade8)?,
            meta_object: sy_col(&syntax, "meta.object", orange)?,
            meta_preprocessor: sy_col(&syntax, "meta.preprocessor", shade8)?,
            meta_tag: sy_col(&syntax, "meta.tag", shade8)?,
            meta_type: sy_col(&syntax, "meta.type", shade8)?,

            punctuation: sy_col(&syntax, "punctuation", shade8)?,
            punctuation_definition: sy_col(&syntax, "punctuation.definition", shade8)?,
            punctuation_definition_comment: sy_col(&syntax, "punctuation.definition.comment", shade4)?,
            punctuation_definition_string: sy_col(&syntax, "punctuation.definition.string", green)?,
            punctuation_section: sy_col(&syntax, "punctuation.section", orange)?,
            punctuation_separator: sy_col(&syntax, "punctuation.separator", shade8)?,

            source: sy_col(&syntax, "source", shade8)?,

            storage: sy_col(&syntax, "storage", magenta)?,
            storage_modifier: sy_col(&syntax, "storage.modifier", magenta)?,
            storage_type: sy_col(&syntax, "storage.type", magenta)?,

            string: sy_col(&syntax, "string", green)?,
            string_interpolated: sy_col(&syntax, "string.interpolated", green)?,
            string_other: sy_col(&syntax, "string.other", green)?,
            string_quoted: sy_col(&syntax, "string.quoted", green)?,
            string_quoted_double: sy_col(&syntax, "string.quoted.double", green)?,
            string_quoted_single: sy_col(&syntax, "string.quoted.single", green)?,
            string_regexp: sy_col(&syntax, "string.regexp", red)?,
            string_template: sy_col(&syntax, "string.template", green)?,
            string_unquoted: sy_col(&syntax, "string.unquoted", green)?,

            support: sy_col(&syntax, "support", blue)?,
            support_class: sy_col(&syntax, "support.class", blue)?,
            support_constant: sy_col(&syntax, "support.constant", magenta)?,
            support_function: sy_col(&syntax, "support.function", blue)?,
            support_function_builtin: sy_col(&syntax, "support.function.builtin", blue_bright)?,
            support_other: sy_col(&syntax, "support.other", blue)?,
            support_type: sy_col(&syntax, "support.type", blue)?,
            support_variable: sy_col(&syntax, "support.variable", cyan)?,

            text: sy_col(&syntax, "text", shade8)?,

            variable: sy_col(&syntax, "variable", shade8)?,
            variable_language: sy_col(&syntax, "variable.language", magenta)?,
            variable_other: sy_col(&syntax, "variable.other", shade8)?,
            variable_other_constant: sy_col(&syntax, "variable.other.constant", shade8)?,
            variable_other_object: sy_col(&syntax, "variable.other.object", shade8)?,
            variable_other_object_property: sy_col(&syntax, "variable.other.object.property", shade8)?,
            variable_parameter: sy_col(&syntax, "variable.parameter", cyan_bright)?,
        };

        fn ui_col(
            ui: &HashMap<String, String>,
            key: &str,
            default_color: Rgba
        ) -> Result<Rgba, String> {
            let key = key.to_string();

            if let Some(hex) = ui.get(&key) {
                return rgba_from_hex(hex)
                    .ok_or_else(|| format!("Invalid color for 'ui.{key}': \"{hex}\""));
            } else {
                return Ok(default_color);
            }
        }

        let ui = Tinted8Ui {
            accent_normal: ui_col(&ui, "accent.normal", cyan)?,

            border_normal: ui_col(&ui, "border.normal", shade4)?,

            chrome_background_dark: ui_col(&ui, "chrome.background.dark", match variant {
                Appearance::Dark => black_dim,
                Appearance::Light => gray_bright,
            })?,
            chrome_background_light: ui_col(&ui, "chrome.background.light", match variant {
                Appearance::Dark => gray_dim,
                Appearance::Light => white,
            })?,
            chrome_background_normal: ui_col(&ui, "chrome.background.normal", shade3)?,
            chrome_foreground_dark: ui_col(&ui, "chrome.foreground.dark", match variant {
                Appearance::Dark => white_dim,
                Appearance::Light => black_dim,
            })?,
            chrome_foreground_light: ui_col(&ui, "chrome.foreground.light", match variant {
                Appearance::Dark => white_bright,
                Appearance::Light => black_bright,
            })?,

            cursor_muted_background: ui_col(&ui, "cursor.muted.background", shade6)?,
            cursor_muted_foreground: ui_col(&ui, "cursor.muted.foreground", shade4)?,
            cursor_normal_background: ui_col(&ui, "cursor.normal.background", shade8)?,
            cursor_normal_foreground: ui_col(&ui, "cursor.normal.foreground", shade2)?,

            deprecated: ui_col(&ui, "deprecated", brown)?,

            global_background_dark: ui_col(&ui, "global.background.dark", shade1)?,
            global_background_light: ui_col(&ui, "global.background.light", shade3)?,
            global_background_normal: ui_col(&ui, "global.background.normal", shade2)?,
            global_foreground_dark: ui_col(&ui, "global.foreground.dark", shade7)?,
            global_foreground_light: ui_col(&ui, "global.foreground.light", shade9)?,
            global_foreground_normal: ui_col(&ui, "global.foreground.normal", shade8)?,

            gutter_background: ui_col(&ui, "gutter.background", shade2)?,
            gutter_foreground: ui_col(&ui, "gutter.foreground", shade7)?,

            highlight_button_background: ui_col(&ui, "highlight.button.background", shade3)?,
            highlight_button_foreground: ui_col(&ui, "highlight.button.foreground", shade8)?,
            highlight_line_background: ui_col(&ui, "highlight.line.background", shade4)?,
            highlight_line_foreground: ui_col(&ui, "highlight.line.foreground", shade7)?,
            highlight_search_background: ui_col(&ui, "highlight.search.background", shade3)?,
            highlight_search_foreground: ui_col(&ui, "highlight.search.foreground", yellow)?,
            highlight_text_activebackground: ui_col(&ui, "highlight.text.active-background", shade5)?,
            highlight_text_activeforeground: ui_col(&ui, "highlight.text.active-foreground", shade8)?,
            highlight_text_background: ui_col(&ui, "highlight.text.background", shade4)?,
            highlight_text_foreground: ui_col(&ui, "highlight.text.foreground", shade8)?,

            indentguide_activebackground: ui_col(&ui, "indent-guide.active-background", shade4)?,
            indentguide_background: ui_col(&ui, "indent-guide.background", shade3)?,

            link_normal: ui_col(&ui, "link.normal", cyan)?,

            selection_background: ui_col(&ui, "selection.background", shade3)?,
            selection_foreground: ui_col(&ui, "selection.foreground", shade8)?,
            selection_inactivebackground: ui_col(&ui, "selection.inactive-background", shade3)?,

            status_error: ui_col(&ui, "status.error", red)?,
            status_info: ui_col(&ui, "status.info", orange)?,
            status_success: ui_col(&ui, "status.success", green)?,
            status_warning: ui_col(&ui, "status.warning", yellow)?,

            tooltip_background: ui_col(&ui, "tooltip.background", shade1)?,
            tooltip_foreground: ui_col(&ui, "tooltip.foreground", shade8)?,

            whitespace_foreground: ui_col(&ui, "whitespace.foreground", shade5)?,
        };

        Ok((scheme, palette, syntax, ui))
    }

    pub fn default_tinted8_yaml() -> &'static str {
        r##"
scheme:
  system: "tinted8"
  supports:
    styling-spec: "0.2.0"
  author: "Tinted Theming (https://github.com/tinted-theming)"
  theme-author: "morhetz (https://github.com/morhetz/gruvbox)"
  family: "Gruvbox"
  style: "Dark"
variant: "dark"
palette:
  black: "#282828"
  white: "#ebdbb2"
  red: "#cc241d"
  yellow: "#d79921"
  green: "#98971a"
  cyan: "#689d6a"
  blue: "#458588"
  magenta: "#b16286"
  gray: "#928374"
  orange: "#d65d0e"
  black-bright: "#3c3836"
  white-bright: "#fbf1c7"
  red-bright: "#fb4934"
  yellow-bright: "#fabd2f"
  green-bright: "#b8bb26"
  cyan-bright: "#8ec07c"
  blue-bright: "#83a598"
  magenta-bright: "#d3869b"
  gray-bright: "#a89984"
  orange-bright: "#fe8019"
  black-dim: "#1d2021"
  gray-dim: "#928374"
syntax:
  entity.name: "#98971a"
  keyword: "#d79921"
  storage: "#d65d0e"
  support.constant: "#689d6a"
  entity.name.function: "#689d6a"
        "##
    }

    /// Returns the [`SystemColors`] for the theme.
    #[inline(always)]
    pub fn system(&self) -> &SystemColors {
        &self.styles.system
    }

    /// Returns the [`AccentColors`] for the theme.
    #[inline(always)]
    pub fn accents(&self) -> &AccentColors {
        &self.styles.accents
    }

    /// Returns the [`PlayerColors`] for the theme.
    #[inline(always)]
    pub fn players(&self) -> &PlayerColors {
        &self.styles.player
    }

    /// Returns the [`ThemeColors`] for the theme.
    #[inline(always)]
    pub fn colors(&self) -> &ThemeColors {
        &self.styles.colors
    }

    /// Returns the [`SyntaxTheme`] for the theme.
    #[inline(always)]
    pub fn syntax(&self) -> &Arc<SyntaxTheme> {
        &self.styles.syntax
    }

    /// Returns the [`StatusColors`] for the theme.
    #[inline(always)]
    pub fn status(&self) -> &StatusColors {
        &self.styles.status
    }

    /// Returns the [`Appearance`] for the theme.
    #[inline(always)]
    pub fn appearance(&self) -> Appearance {
        self.appearance
    }

    /// Returns the [`WindowBackgroundAppearance`] for the theme.
    #[inline(always)]
    pub fn window_background_appearance(&self) -> WindowBackgroundAppearance {
        self.styles.window_background_appearance
    }

    /// Darkens the color by reducing its lightness.
    /// The resulting lightness is clamped to ensure it doesn't go below 0.0.
    ///
    /// The first value darkens light appearance mode, the second darkens appearance dark mode.
    ///
    /// Note: This is a tentative solution and may be replaced with a more robust color system.
    pub fn darken(&self, color: Hsla, light_amount: f32, dark_amount: f32) -> Hsla {
        let amount = match self.appearance {
            Appearance::Light => light_amount,
            Appearance::Dark => dark_amount,
        };
        let mut hsla = color;
        hsla.l = (hsla.l - amount).max(0.0);
        hsla
    }
}

/// The active theme.
pub struct GlobalTheme {
    theme: Arc<Theme>,
}
impl Global for GlobalTheme {}

impl GlobalTheme {
    /// Creates a new [`GlobalTheme`] with the given theme.
    pub fn new(theme: Arc<Theme>) -> Self {
        Self { theme }
    }

    /// Updates the active theme.
    pub fn update_theme(cx: &mut App, theme: Arc<Theme>) {
        cx.update_global::<Self, _>(|this, _| this.theme = theme);
    }

    /// Returns the active theme.
    pub fn theme(cx: &App) -> &Arc<Theme> {
        &cx.global::<Self>().theme
    }
}
