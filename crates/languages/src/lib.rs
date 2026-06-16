use gpui::{App, UpdateGlobal};
use node_runtime::NodeRuntime;
use project::Fs;
use settings::{SemanticTokenRules, SettingsStore};
use smol::stream::StreamExt;
use std::sync::Arc;
use util::ResultExt;

pub use language::*;

/// A shared grammar for plain text, exposed for reuse by downstream crates.
#[cfg(feature = "tree-sitter-gitcommit")]
pub static LANGUAGE_GIT_COMMIT: std::sync::LazyLock<Arc<Language>> =
    std::sync::LazyLock::new(|| {
        Arc::new(Language::new(
            LanguageConfig {
                name: "Git Commit".into(),
                soft_wrap: Some(language::SoftWrap::EditorWidth),
                matcher: LanguageMatcher {
                    path_suffixes: vec!["COMMIT_EDITMSG".to_owned()],
                    first_line_pattern: None,
                    ..LanguageMatcher::default()
                },
                line_comments: vec![Arc::from("#")],
                ..LanguageConfig::default()
            },
            Some(tree_sitter_gitcommit::LANGUAGE.into()),
        ))
    });

pub fn semantic_token_rules(lang: &str) -> SemanticTokenRules {
    let path = format!("{lang}/semantic_token_rules.json");
    let content = grammars::get_file(&path)
        .unwrap_or_else(|| panic!("missing {path}"));
    let json = std::str::from_utf8(&content.data)
        .unwrap_or_else(|_| panic!("invalid utf-8 in {path}"));
    settings::parse_json_with_comments::<SemanticTokenRules>(json)
        .unwrap_or_else(|_| panic!("failed to parse {path}"))
}

pub fn init(languages: Arc<LanguageRegistry>, _fs: Arc<dyn Fs>, _node: NodeRuntime, cx: &mut App) {
    #[cfg(feature = "load-grammars")]
    languages.register_native_grammars(grammars::native_grammars());

    let built_in_languages = [
        LanguageInfo {
            name: "bash",
            ..Default::default()
        },
        LanguageInfo {
            name: "c",
            ..Default::default()
        },
        LanguageInfo {
            name: "cpp",
            semantic_token_rules: Some(semantic_token_rules("cpp")),
            ..Default::default()
        },
        LanguageInfo {
            name: "css",
            ..Default::default()
        },
        LanguageInfo {
            name: "diff",
            ..Default::default()
        },
        LanguageInfo {
            name: "go",
            semantic_token_rules: Some(semantic_token_rules("go")),
            ..Default::default()
        },
        LanguageInfo {
            name: "gomod",
            ..Default::default()
        },
        LanguageInfo {
            name: "gowork",
            ..Default::default()
        },
        LanguageInfo {
            name: "json",
            ..Default::default()
        },
        LanguageInfo {
            name: "jsonc",
            ..Default::default()
        },
        LanguageInfo {
            name: "markdown",
            ..Default::default()
        },
        LanguageInfo {
            name: "markdown-inline",
            ..Default::default()
        },
        LanguageInfo {
            name: "python",
            semantic_token_rules: Some(semantic_token_rules("python")),
            ..Default::default()
        },
        LanguageInfo {
            name: "rust",
            semantic_token_rules: Some(semantic_token_rules("rust")),
            ..Default::default()
        },
        LanguageInfo {
            name: "tsx",
            ..Default::default()
        },
        LanguageInfo {
            name: "typescript",
            ..Default::default()
        },
        LanguageInfo {
            name: "javascript",
            ..Default::default()
        },
        LanguageInfo {
            name: "jsdoc",
            ..Default::default()
        },
        LanguageInfo {
            name: "regex",
            ..Default::default()
        },
        LanguageInfo {
            name: "yaml",
            ..Default::default()
        },
        LanguageInfo {
            name: "gitcommit",
            ..Default::default()
        },
        LanguageInfo {
            name: "zed-keybind-context",
            ..Default::default()
        },
    ];

    for registration in built_in_languages {
        register_language(
            &languages,
            registration.name,
            registration.adapters,
            registration.context,
            registration.toolchain,
            registration.manifest_name,
            registration.semantic_token_rules,
            cx,
        );
    }

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

#[derive(Default)]
struct LanguageInfo {
    name: &'static str,
    adapters: Vec<Arc<dyn LspAdapter>>,
    context: Option<Arc<dyn ContextProvider>>,
    toolchain: Option<Arc<dyn ToolchainLister>>,
    manifest_name: Option<ManifestName>,
    semantic_token_rules: Option<SemanticTokenRules>,
}

fn register_language(
    languages: &LanguageRegistry,
    name: &'static str,
    adapters: Vec<Arc<dyn LspAdapter>>,
    context: Option<Arc<dyn ContextProvider>>,
    toolchain: Option<Arc<dyn ToolchainLister>>,
    manifest_name: Option<ManifestName>,
    semantic_token_rules: Option<SemanticTokenRules>,
    cx: &mut App,
) {
    let config = load_config(name);
    if let Some(rules) = &semantic_token_rules {
        SettingsStore::update_global(cx, |store, cx| {
            store.set_language_semantic_token_rules(config.name.0.clone(), rules.clone(), cx);
        });
    }
    for adapter in adapters {
        languages.register_lsp_adapter(config.name.clone(), adapter);
    }
    languages.register_language(
        config.name.clone(),
        config.grammar.clone(),
        config.matcher.clone(),
        config.hidden,
        manifest_name.clone(),
        Arc::new(move || {
            Ok(LoadedLanguage {
                config: config.clone(),
                queries: grammars::load_queries(name),
                context_provider: context.clone(),
                toolchain_provider: toolchain.clone(),
                manifest_name: manifest_name.clone(),
            })
        }),
    );
}

#[cfg(any(test, feature = "test-support"))]
pub fn language(name: &str, grammar: tree_sitter::Language) -> Arc<Language> {
    Arc::new(
        Language::new(grammars::load_config(name), Some(grammar))
            .with_queries(grammars::load_queries(name))
            .unwrap(),
    )
}

fn load_config(name: &str) -> LanguageConfig {
    let grammars_loaded = cfg!(any(feature = "load-grammars", test));
    grammars::load_config_for_feature(name, grammars_loaded)
}
