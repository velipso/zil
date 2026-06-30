use std::sync::Arc;
use std::fmt::Debug;

use anyhow::Result;
use collections::HashMap;
use gpui::{App, AssetSource, Global, SharedString};
use parking_lot::RwLock;
use thiserror::Error;

use crate::{Appearance, Theme, ThemeFamily};

/// The metadata for a theme.
#[derive(Debug, Clone)]
pub struct ThemeMeta {
    /// The name of the theme.
    pub name: SharedString,
    /// The appearance of the theme.
    pub appearance: Appearance,
}

/// An error indicating that the theme with the given name was not found.
#[derive(Debug, Error, Clone)]
#[error("theme not found: {0}")]
pub struct ThemeNotFoundError(pub SharedString);

/// The global [`ThemeRegistry`].
///
/// This newtype exists for obtaining a unique [`TypeId`](std::any::TypeId) when
/// inserting the [`ThemeRegistry`] into the context as a global.
///
/// This should not be exposed outside of this module.
#[derive(Default)]
struct GlobalThemeRegistry(Arc<ThemeRegistry>);

impl std::ops::DerefMut for GlobalThemeRegistry {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl std::ops::Deref for GlobalThemeRegistry {
    type Target = Arc<ThemeRegistry>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Global for GlobalThemeRegistry {}

struct ThemeRegistryState {
    themes: HashMap<SharedString, Arc<Theme>>,
    /// Whether the extensions have been loaded yet.
    extensions_loaded: bool,
}

/// The registry for themes.
pub struct ThemeRegistry {
    state: RwLock<ThemeRegistryState>,
    assets: Box<dyn AssetSource>,
}

impl ThemeRegistry {
    /// Returns the global [`ThemeRegistry`].
    pub fn global(cx: &App) -> Arc<Self> {
        cx.global::<GlobalThemeRegistry>().0.clone()
    }

    /// Returns the global [`ThemeRegistry`].
    ///
    /// Inserts a default [`ThemeRegistry`] if one does not yet exist.
    pub fn default_global(cx: &mut App) -> Arc<Self> {
        cx.default_global::<GlobalThemeRegistry>().0.clone()
    }

    /// Returns the global [`ThemeRegistry`] if it exists.
    pub fn try_global(cx: &mut App) -> Option<Arc<Self>> {
        cx.try_global::<GlobalThemeRegistry>().map(|t| t.0.clone())
    }

    /// Sets the global [`ThemeRegistry`].
    pub(crate) fn set_global(assets: Box<dyn AssetSource>, cx: &mut App) {
        cx.set_global(GlobalThemeRegistry(Arc::new(ThemeRegistry::new(assets))));
    }

    /// Returns the asset source used by this registry.
    pub fn assets(&self) -> &dyn AssetSource {
        self.assets.as_ref()
    }

    /// Creates a new [`ThemeRegistry`] with the given [`AssetSource`].
    pub fn new(assets: Box<dyn AssetSource>) -> Self {
        let registry = Self {
            state: RwLock::new(ThemeRegistryState {
                themes: HashMap::default(),
                extensions_loaded: false,
            }),
            assets,
        };

        // We're loading the Zed default theme, as we need a theme to be loaded
        // for tests.
        registry.insert_theme_families([crate::fallback_themes::zed_default_themes()]);

        registry
    }

    /// Returns whether the extensions have been loaded.
    pub fn extensions_loaded(&self) -> bool {
        self.state.read().extensions_loaded
    }

    /// Sets the flag indicating that the extensions have loaded.
    pub fn set_extensions_loaded(&self) {
        self.state.write().extensions_loaded = true;
    }

    /// Inserts the given theme families into the registry.
    pub fn insert_theme_families(&self, families: impl IntoIterator<Item = ThemeFamily>) {
        for family in families.into_iter() {
            self.insert_themes(family.themes);
        }
    }

    /// Registers theme families for use in tests.
    #[cfg(any(test, feature = "test-support"))]
    pub fn register_test_themes(&self, families: impl IntoIterator<Item = ThemeFamily>) {
        self.insert_theme_families(families);
    }

    /// Inserts the given themes into the registry.
    pub fn insert_themes(&self, themes: impl IntoIterator<Item = Theme>) {
        let mut state = self.state.write();
        for theme in themes.into_iter() {
            state.themes.insert(theme.name.clone(), Arc::new(theme));
        }
    }

    /// Removes the themes with the given names from the registry.
    pub fn remove_user_themes(&self, themes_to_remove: &[SharedString]) {
        self.state
            .write()
            .themes
            .retain(|name, _| !themes_to_remove.contains(name))
    }

    /// Removes all themes from the registry.
    pub fn clear(&self) {
        self.state.write().themes.clear();
    }

    /// Returns the names of all themes in the registry.
    pub fn list_names(&self) -> Vec<SharedString> {
        let mut names = self.state.read().themes.keys().cloned().collect::<Vec<_>>();
        names.sort();
        names
    }

    /// Returns the metadata of all themes in the registry.
    pub fn list(&self) -> Vec<ThemeMeta> {
        self.state
            .read()
            .themes
            .values()
            .map(|theme| ThemeMeta {
                name: theme.name.clone(),
                appearance: theme.appearance(),
            })
            .collect()
    }

    /// Returns the theme with the given name.
    pub fn get(&self, name: &str) -> Result<Arc<Theme>, ThemeNotFoundError> {
        self.state
            .read()
            .themes
            .get(name)
            .ok_or_else(|| ThemeNotFoundError(name.to_string().into()))
            .cloned()
    }
}

impl Default for ThemeRegistry {
    fn default() -> Self {
        Self::new(Box::new(()))
    }
}
