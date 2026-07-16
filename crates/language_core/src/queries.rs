use std::borrow::Cow;

pub type QueryFieldAccessor = fn(&mut LanguageQueries) -> &mut Option<Cow<'static, str>>;

pub const QUERY_FILENAME_PREFIXES: &[(&str, QueryFieldAccessor)] = &[
    ("highlights", |q| &mut q.highlights),
    ("outline", |q| &mut q.outline),
    ("injections", |q| &mut q.injections),
    ("overrides", |q| &mut q.overrides),
    ("debugger", |q| &mut q.debugger),
    ("textobjects", |q| &mut q.text_objects),
];

/// Tree-sitter language queries for a given language.
#[derive(Debug, Default)]
pub struct LanguageQueries {
    pub highlights: Option<Cow<'static, str>>,
    pub outline: Option<Cow<'static, str>>,
    pub injections: Option<Cow<'static, str>>,
    pub overrides: Option<Cow<'static, str>>,
    pub text_objects: Option<Cow<'static, str>>,
    pub debugger: Option<Cow<'static, str>>,
}
