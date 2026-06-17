use crate::LanguageName;
use collections::{HashMap, HashSet, IndexSet};
use gpui_shared_string::SharedString;
use lsp::LanguageServerName;
use regex::Regex;
use schemars::{JsonSchema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use std::{num::NonZeroU32, path::Path, sync::Arc};

/// Controls the soft-wrapping behavior in the editor.
#[derive(Copy, Clone, Debug, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SoftWrap {
    /// Prefer a single line generally, unless an overly long line is encountered.
    None,
    /// Deprecated: use None instead. Left to avoid breaking existing users' configs.
    /// Prefer a single line generally, unless an overly long line is encountered.
    PreferLine,
    /// Soft wrap lines that exceed the editor width.
    EditorWidth,
    /// Soft wrap line at the preferred line length or the editor width (whichever is smaller).
    #[serde(alias = "preferred_line_length")]
    Bounded,
}

/// Top-level configuration for a language, typically loaded from a `config.toml`
/// shipped alongside the grammar.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
pub struct LanguageConfig {
    /// Human-readable name of the language.
    pub name: LanguageName,
    /// The name of this language for a Markdown code fence block
    pub code_fence_block_name: Option<Arc<str>>,
    /// Alternative language names that Jupyter kernels may report for this language.
    /// Used when a kernel's `language` field differs from Zed's language name.
    /// For example, the Nu extension would set this to `["nushell"]`.
    #[serde(default)]
    pub kernel_language_names: Vec<Arc<str>>,
    // The name of the grammar in a WASM bundle (experimental).
    pub grammar: Option<Arc<str>>,
    /// The criteria for matching this language to a given file.
    #[serde(flatten)]
    pub matcher: LanguageMatcher,
    /// If set to true, auto indentation uses last non empty line to determine
    /// the indentation level for a new line.
    #[serde(default = "auto_indent_using_last_non_empty_line_default")]
    pub auto_indent_using_last_non_empty_line: bool,
    // Whether indentation of pasted content should be adjusted based on the context.
    #[serde(default)]
    pub auto_indent_on_paste: Option<bool>,
    /// A regex that is used to determine whether the indentation level should be
    /// increased in the following line.
    #[serde(default, deserialize_with = "deserialize_regex")]
    #[schemars(schema_with = "regex_json_schema")]
    pub increase_indent_pattern: Option<Regex>,
    /// A regex that is used to determine whether the indentation level should be
    /// decreased in the following line.
    #[serde(default, deserialize_with = "deserialize_regex")]
    #[schemars(schema_with = "regex_json_schema")]
    pub decrease_indent_pattern: Option<Regex>,
    /// A list of rules for decreasing indentation. Each rule pairs a regex with a set of valid
    /// "block-starting" tokens. When a line matches a pattern, its indentation is aligned with
    /// the most recent line that began with a corresponding token. This enables context-aware
    /// outdenting, like aligning an `else` with its `if`.
    #[serde(default)]
    pub decrease_indent_patterns: Vec<DecreaseIndentConfig>,
    /// A placeholder used internally by Semantic Index.
    #[serde(default)]
    pub collapsed_placeholder: String,
    /// A line comment string that is inserted in e.g. `toggle comments` action.
    /// A language can have multiple flavours of line comments. All of the provided line comments are
    /// used for comment continuations on the next line, but only the first one is used for Editor::ToggleComments.
    #[serde(default)]
    pub line_comments: Vec<Arc<str>>,
    /// Delimiters and configuration for recognizing and formatting block comments.
    #[serde(default)]
    pub block_comment: Option<BlockCommentConfig>,
    /// Delimiters and configuration for recognizing and formatting documentation comments.
    #[serde(default, alias = "documentation")]
    pub documentation_comment: Option<BlockCommentConfig>,
    /// List markers that are inserted unchanged on newline (e.g., `- `, `* `, `+ `).
    #[serde(default)]
    pub unordered_list: Vec<Arc<str>>,
    /// Configuration for ordered lists with auto-incrementing numbers on newline (e.g., `1. ` becomes `2. `).
    #[serde(default)]
    pub ordered_list: Vec<OrderedListConfig>,
    /// Configuration for task lists where multiple markers map to a single continuation prefix (e.g., `- [x] ` continues as `- [ ] `).
    #[serde(default)]
    pub task_list: Option<TaskListConfig>,
    /// A list of additional regex patterns that should be treated as prefixes
    /// for creating boundaries during rewrapping, ensuring content from one
    /// prefixed section doesn't merge with another (e.g., markdown list items).
    /// By default, Zed treats as paragraph and comment prefixes as boundaries.
    #[serde(default, deserialize_with = "deserialize_regex_vec")]
    #[schemars(schema_with = "regex_vec_json_schema")]
    pub rewrap_prefixes: Vec<Regex>,
    /// A list of language servers that are allowed to run on subranges of a given language.
    #[serde(default)]
    pub scope_opt_in_language_servers: Vec<LanguageServerName>,
    #[serde(default)]
    pub overrides: HashMap<String, LanguageConfigOverride>,
    /// A list of characters that Zed should treat as word characters for the
    /// purpose of features that operate on word boundaries, like 'move to next word end'
    /// or a whole-word search in buffer search.
    #[serde(default)]
    pub word_characters: HashSet<char>,
    /// Whether to indent lines using tab characters, as opposed to multiple
    /// spaces.
    #[serde(default)]
    pub hard_tabs: Option<bool>,
    /// How many columns a tab should occupy.
    #[serde(default)]
    #[schemars(range(min = 1, max = 128))]
    pub tab_size: Option<NonZeroU32>,
    /// How to soft-wrap long lines of text.
    #[serde(default)]
    pub soft_wrap: Option<SoftWrap>,
    /// When set, selections can be wrapped using prefix/suffix pairs on both sides.
    #[serde(default)]
    pub wrap_characters: Option<WrapCharactersConfig>,
    /// The name of a Prettier parser that will be used for this language when no file path is available.
    /// If there's a parser name in the language settings, that will be used instead.
    #[serde(default)]
    pub prettier_parser_name: Option<String>,
    /// If true, this language is only for syntax highlighting via an injection into other
    /// languages, but should not appear to the user as a distinct language.
    #[serde(default)]
    pub hidden: bool,
    /// A list of characters that Zed should treat as word characters for completion queries.
    #[serde(default)]
    pub completion_query_characters: HashSet<char>,
    /// A list of characters that Zed should treat as word characters for linked edit operations.
    #[serde(default)]
    pub linked_edit_characters: HashSet<char>,
    /// A list of preferred debuggers for this language.
    #[serde(default)]
    pub debuggers: IndexSet<SharedString>,
}

impl LanguageConfig {
    pub const FILE_NAME: &str = "config.toml";

    pub fn load(config_path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let config = std::fs::read_to_string(config_path.as_ref())?;
        toml::from_str(&config).map_err(Into::into)
    }
}

impl Default for LanguageConfig {
    fn default() -> Self {
        Self {
            name: LanguageName::new_static(""),
            code_fence_block_name: None,
            kernel_language_names: Default::default(),
            grammar: None,
            matcher: LanguageMatcher::default(),
            auto_indent_using_last_non_empty_line: auto_indent_using_last_non_empty_line_default(),
            auto_indent_on_paste: None,
            increase_indent_pattern: Default::default(),
            decrease_indent_pattern: Default::default(),
            decrease_indent_patterns: Default::default(),
            line_comments: Default::default(),
            block_comment: Default::default(),
            documentation_comment: Default::default(),
            unordered_list: Default::default(),
            ordered_list: Default::default(),
            task_list: Default::default(),
            rewrap_prefixes: Default::default(),
            scope_opt_in_language_servers: Default::default(),
            overrides: Default::default(),
            word_characters: Default::default(),
            collapsed_placeholder: Default::default(),
            hard_tabs: None,
            tab_size: None,
            soft_wrap: None,
            wrap_characters: None,
            prettier_parser_name: None,
            hidden: false,
            completion_query_characters: Default::default(),
            linked_edit_characters: Default::default(),
            debuggers: Default::default(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Default, JsonSchema)]
pub struct DecreaseIndentConfig {
    #[serde(default, deserialize_with = "deserialize_regex")]
    #[schemars(schema_with = "regex_json_schema")]
    pub pattern: Option<Regex>,
    #[serde(default)]
    pub valid_after: Vec<String>,
}

/// Configuration for continuing ordered lists with auto-incrementing numbers.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
pub struct OrderedListConfig {
    /// A regex pattern with a capture group for the number portion (e.g., `(\\d+)\\. `).
    pub pattern: String,
    /// A format string where `{1}` is replaced with the incremented number (e.g., `{1}. `).
    pub format: String,
}

/// Configuration for continuing task lists on newline.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
pub struct TaskListConfig {
    /// The list markers to match (e.g., `- [ ] `, `- [x] `).
    pub prefixes: Vec<Arc<str>>,
    /// The marker to insert when continuing the list on a new line (e.g., `- [ ] `).
    pub continuation: Arc<str>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default, JsonSchema)]
pub struct LanguageMatcher {
    /// Given a list of `LanguageConfig`'s, the language of a file can be determined based on the path extension matching any of the `path_suffixes`.
    #[serde(default)]
    pub path_suffixes: Vec<String>,
    /// A regex pattern that determines whether the language should be assigned to a file or not.
    #[serde(
        default,
        serialize_with = "serialize_regex",
        deserialize_with = "deserialize_regex"
    )]
    #[schemars(schema_with = "regex_json_schema")]
    pub first_line_pattern: Option<Regex>,
    /// Alternative names for this language used in vim/emacs modelines.
    /// These are matched case-insensitively against the `mode` (emacs) or
    /// `filetype`/`ft` (vim) specified in the modeline.
    #[serde(default)]
    pub modeline_aliases: Vec<String>,
}

impl Ord for LanguageMatcher {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.path_suffixes
            .cmp(&other.path_suffixes)
            .then_with(|| {
                self.first_line_pattern
                    .as_ref()
                    .map(Regex::as_str)
                    .cmp(&other.first_line_pattern.as_ref().map(Regex::as_str))
            })
            .then_with(|| self.modeline_aliases.cmp(&other.modeline_aliases))
    }
}

impl PartialOrd for LanguageMatcher {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Eq for LanguageMatcher {}

impl PartialEq for LanguageMatcher {
    fn eq(&self, other: &Self) -> bool {
        self.path_suffixes == other.path_suffixes
            && self.first_line_pattern.as_ref().map(Regex::as_str)
                == other.first_line_pattern.as_ref().map(Regex::as_str)
            && self.modeline_aliases == other.modeline_aliases
    }
}

/// The configuration for block comments for this language.
#[derive(Clone, Debug, JsonSchema, PartialEq)]
pub struct BlockCommentConfig {
    /// A start tag of block comment.
    pub start: Arc<str>,
    /// A end tag of block comment.
    pub end: Arc<str>,
    /// A character to add as a prefix when a new line is added to a block comment.
    pub prefix: Arc<str>,
    /// A indent to add for prefix and end line upon new line.
    #[schemars(range(min = 1, max = 128))]
    pub tab_size: u32,
}

impl<'de> Deserialize<'de> for BlockCommentConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum BlockCommentConfigHelper {
            New {
                start: Arc<str>,
                end: Arc<str>,
                prefix: Arc<str>,
                tab_size: u32,
            },
            Old([Arc<str>; 2]),
        }

        match BlockCommentConfigHelper::deserialize(deserializer)? {
            BlockCommentConfigHelper::New {
                start,
                end,
                prefix,
                tab_size,
            } => Ok(BlockCommentConfig {
                start,
                end,
                prefix,
                tab_size,
            }),
            BlockCommentConfigHelper::Old([start, end]) => Ok(BlockCommentConfig {
                start,
                end,
                prefix: "".into(),
                tab_size: 0,
            }),
        }
    }
}

#[derive(Clone, Deserialize, Default, Debug, JsonSchema)]
pub struct LanguageConfigOverride {
    #[serde(default)]
    pub line_comments: Override<Vec<Arc<str>>>,
    #[serde(default)]
    pub block_comment: Override<BlockCommentConfig>,
    #[serde(skip)]
    pub disabled_bracket_ixs: Vec<u16>,
    #[serde(default)]
    pub word_characters: Override<HashSet<char>>,
    #[serde(default)]
    pub completion_query_characters: Override<HashSet<char>>,
    #[serde(default)]
    pub linked_edit_characters: Override<HashSet<char>>,
    #[serde(default)]
    pub opt_into_language_servers: Vec<LanguageServerName>,
    #[serde(default)]
    pub prefer_label_for_snippet: Option<bool>,
}

#[derive(Clone, Deserialize, Debug, Serialize, JsonSchema)]
#[serde(untagged)]
pub enum Override<T> {
    Remove { remove: bool },
    Set(T),
}

impl<T> Default for Override<T> {
    fn default() -> Self {
        Override::Remove { remove: false }
    }
}

impl<T> Override<T> {
    pub fn as_option<'a>(this: Option<&'a Self>, original: Option<&'a T>) -> Option<&'a T> {
        match this {
            Some(Self::Set(value)) => Some(value),
            Some(Self::Remove { remove: true }) => None,
            Some(Self::Remove { remove: false }) | None => original,
        }
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
pub struct WrapCharactersConfig {
    /// Opening token split into a prefix and suffix. The first caret goes
    /// after the prefix (i.e., between prefix and suffix).
    pub start_prefix: String,
    pub start_suffix: String,
    /// Closing token split into a prefix and suffix. The second caret goes
    /// after the prefix (i.e., between prefix and suffix).
    pub end_prefix: String,
    pub end_suffix: String,
}

pub fn auto_indent_using_last_non_empty_line_default() -> bool {
    true
}

pub fn deserialize_regex<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Regex>, D::Error> {
    let source = Option::<String>::deserialize(d)?;
    if let Some(source) = source {
        Ok(Some(regex::Regex::new(&source).map_err(de::Error::custom)?))
    } else {
        Ok(None)
    }
}

pub fn regex_json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    json_schema!({
        "type": "string"
    })
}

pub fn serialize_regex<S>(regex: &Option<Regex>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match regex {
        Some(regex) => serializer.serialize_str(regex.as_str()),
        None => serializer.serialize_none(),
    }
}

pub fn deserialize_regex_vec<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<Regex>, D::Error> {
    let sources = Vec::<String>::deserialize(d)?;
    sources
        .into_iter()
        .map(|source| regex::Regex::new(&source))
        .collect::<Result<_, _>>()
        .map_err(de::Error::custom)
}

pub fn regex_vec_json_schema(_: &mut SchemaGenerator) -> schemars::Schema {
    json_schema!({
        "type": "array",
        "items": { "type": "string" }
    })
}
