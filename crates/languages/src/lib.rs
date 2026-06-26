use gpui::{App, UpdateGlobal};
use project::Fs;
use settings::SettingsStore;
use smol::stream::StreamExt;
use std::sync::Arc;
use util::ResultExt;

pub use language::*;

pub fn init(languages: Arc<LanguageRegistry>, _fs: Arc<dyn Fs>, cx: &mut App) {
    let mut subscription = languages.subscribe();
    let mut prev_language_settings = languages.language_settings();

    cx.spawn(async move |cx| {
        while subscription.next().await.is_some() {
            let language_settings = languages.language_settings();
            if language_settings != prev_language_settings {
                cx.update(|cx| {
                    SettingsStore::update_global(cx, |settings, cx| {
                        settings
                            .set_extension_settings(
                                settings::ExtensionsSettingsContent {
                                    all_languages: language_settings.clone(),
                                },
                                cx,
                            )
                            .log_err();
                    });
                });
                prev_language_settings = language_settings;
            }
        }
        anyhow::Ok(())
    })
    .detach();
}
