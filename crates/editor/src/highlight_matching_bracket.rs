use crate::{Editor, HighlightKey, RangeToAnchorExt, display_map::DisplaySnapshot};
use gpui::{AppContext, Context, HighlightStyle, FontWeight};
use language::CursorShape;
use multi_buffer::{MultiBufferOffset, MultiBufferSnapshot};
use theme::ActiveTheme;
use std::ops::Range;

fn dumb_innermost_enclosing_bracket_ranges(
    buffer_snapshot: &MultiBufferSnapshot,
    range: Range<MultiBufferOffset>,
) -> Option<(Range<MultiBufferOffset>, Range<MultiBufferOffset>)> {
    // VELIPSO: check language settings somehow for which brackets to enable
    let curly_enable = true;
    let square_enable = true;
    let paren_enable = true;

    if !curly_enable && !square_enable && !paren_enable {
        return None;
    }

    fn result(
        start: usize,
        end: usize
    ) -> Option<(Range<MultiBufferOffset>, Range<MultiBufferOffset>)> {
        Some((
            MultiBufferOffset(start)..MultiBufferOffset(start + 1),
            MultiBufferOffset(end)..MultiBufferOffset(end + 1),
        ))
    }

    #[derive(PartialEq, Debug)]
    enum NearChar {
        None,
        Start,
        End(usize),
    }

    let mut curly_count = 0;
    let mut square_count = 0;
    let mut paren_count = 0;

    let mut offset = range.start.0;
    let mut curly_start: Option<usize> = None;
    let mut square_start: Option<usize> = None;
    let mut paren_start: Option<usize> = None;

    let mut curly_left: NearChar = NearChar::None;
    let mut square_left: NearChar = NearChar::None;
    let mut paren_left: NearChar = NearChar::None;

    for c in buffer_snapshot.reversed_chars_at(range.start) {
        offset = offset.saturating_sub(c.len_utf8());

        if curly_enable && curly_start.is_none() {
            if c == '{' {
                if curly_count == 0 {
                    curly_start = Some(offset);
                } else {
                    curly_count -= 1;
                    if curly_left == NearChar::Start && curly_count == 0 {
                        curly_left = NearChar::End(offset);
                    }
                }
            } else if c == '}' {
                if offset == range.start.0 - 1 && curly_left == NearChar::None {
                    curly_left = NearChar::Start;
                }
                curly_count += 1;
            }
        }

        if square_enable && square_start.is_none() {
            if c == '[' {
                if square_count == 0 {
                    square_start = Some(offset);
                } else {
                    square_count -= 1;
                    if square_left == NearChar::Start && square_count == 0 {
                        square_left = NearChar::End(offset);
                    }
                }
            } else if c == ']' {
                if offset == range.start.0 - 1 && square_left == NearChar::None {
                    square_left = NearChar::Start;
                }
                square_count += 1;
            }
        }

        if paren_enable && paren_start.is_none() {
            if c == '(' {
                if paren_count == 0 {
                    paren_start = Some(offset);
                } else {
                    paren_count -= 1;
                    if paren_left == NearChar::Start && paren_count == 0 {
                        paren_left = NearChar::End(offset);
                    }
                }
            } else if c == ')' {
                if offset == range.start.0 - 1 && paren_left == NearChar::None {
                    paren_left = NearChar::Start;
                }
                paren_count += 1;
            }
        }

        if (!curly_enable || curly_start.is_some())
            && (!square_enable || square_start.is_some())
            && (!paren_enable || paren_start.is_some())
        {
            break;
        }
    }

    let mut curly_count = 0;
    let mut square_count = 0;
    let mut paren_count = 0;

    let mut offset = range.end.0;
    let mut curly_end: Option<usize> = None;
    let mut square_end: Option<usize> = None;
    let mut paren_end: Option<usize> = None;

    let mut curly_right: NearChar = NearChar::None;
    let mut square_right: NearChar = NearChar::None;
    let mut paren_right: NearChar = NearChar::None;

    for c in buffer_snapshot.chars_at(range.end) {
        if curly_enable && curly_end.is_none() {
            if c == '}' {
                if curly_count == 0 {
                    curly_end = Some(offset);
                } else {
                    curly_count -= 1;
                    if curly_right == NearChar::Start && curly_count == 0 {
                        curly_right = NearChar::End(offset);
                    }
                }
            } else if c == '{' {
                if offset == range.end.0 && curly_right == NearChar::None {
                    curly_right = NearChar::Start;
                }
                curly_count += 1;
            }
        }

        if square_enable && square_end.is_none() {
            if c == ']' {
                if square_count == 0 {
                    square_end = Some(offset);
                } else {
                    square_count -= 1;
                    if square_right == NearChar::Start && square_count == 0 {
                        square_right = NearChar::End(offset);
                    }
                }
            } else if c == '[' {
                if offset == range.end.0 && square_right == NearChar::None {
                    square_right = NearChar::Start;
                }
                square_count += 1;
            }
        }

        if paren_enable && paren_end.is_none() {
            if c == ')' {
                if paren_count == 0 {
                    paren_end = Some(offset);
                } else {
                    paren_count -= 1;
                    if paren_right == NearChar::Start && paren_count == 0 {
                        paren_right = NearChar::End(offset);
                    }
                }
            } else if c == '(' {
                if offset == range.end.0 && paren_right == NearChar::None {
                    paren_right = NearChar::Start;
                }
                paren_count += 1;
            }
        }

        if (!curly_enable || curly_start.is_some())
            && (!square_enable || square_start.is_some())
            && (!paren_enable || paren_start.is_some())
        {
            break;
        }
        offset += c.len_utf8();
    }

    // score each entry based on how far they are from the cursor
    // math cannot panic because range.start.0 >= start and range.end.0 <= end
    let curly = if let Some(start) = curly_start && let Some(end) = curly_end {
        Some((start, end, (range.start.0 - start) + (end - range.end.0)))
    } else {
        None
    };
    let square = if let Some(start) = square_start && let Some(end) = square_end {
        Some((start, end, (range.start.0 - start) + (end - range.end.0)))
    } else {
        None
    };
    let paren = if let Some(start) = paren_start && let Some(end) = paren_end {
        Some((start, end, (range.start.0 - start) + (end - range.end.0)))
    } else {
        None
    };

    // find the best match (lowest score)
    if let Some(curly) = curly {
        if let Some(square) = square {
            if let Some(paren) = paren {
                // three-way sort
                if curly.2 <= square.2 && curly.2 <= paren.2 {
                    result(curly.0, curly.1)
                } else if square.2 <= paren.2 {
                    result(square.0, square.1)
                } else {
                    result(paren.0, paren.1)
                }
            } else {
                // two-way sort
                if curly.2 <= square.2 {
                    result(curly.0, curly.1)
                } else {
                    result(square.0, square.1)
                }
            }
        } else if let Some(paren) = paren {
            // two-way sort
            if curly.2 <= paren.2 {
                result(curly.0, curly.1)
            } else {
                result(paren.0, paren.1)
            }
        } else {
            result(curly.0, curly.1)
        }
    } else if let Some(square) = square {
        if let Some(paren) = paren {
            // two-way sort
            if square.2 <= paren.2 {
                result(square.0, square.1)
            } else {
                result(paren.0, paren.1)
            }
        } else {
            result(square.0, square.1)
        }
    } else if let Some(paren) = paren {
        result(paren.0, paren.1)
    } else {
        // not inside a bracket, so check for brackets immediately outside
        if let NearChar::End(curly_left) = curly_left {
            result(curly_left, range.start.0 - 1)
        } else if let NearChar::End(square_left) = square_left {
            result(square_left, range.start.0 - 1)
        } else if let NearChar::End(paren_left) = paren_left {
            result(paren_left, range.start.0 - 1)
        } else if let NearChar::End(curly_right) = curly_right {
            result(range.end.0, curly_right)
        } else if let NearChar::End(square_right) = square_right {
            result(range.end.0, square_right)
        } else if let NearChar::End(paren_right) = paren_right {
            result(range.end.0, paren_right)
        } else {
            None
        }
    }
}

impl Editor {
    pub fn refresh_matching_bracket_highlights(
        &mut self,
        snapshot: &DisplaySnapshot,
        cx: &mut Context<Editor>,
    ) {
        let newest_selection = self.selections.newest::<MultiBufferOffset>(&snapshot);
        // Don't highlight brackets if the selection isn't empty
        if !newest_selection.is_empty() {
            self.clear_highlights(HighlightKey::MatchingBracket, cx);
            return;
        }

        let buffer_snapshot = snapshot.buffer_snapshot();
        let head = newest_selection.head();
        if head > buffer_snapshot.len() {
            log::error!("bug: cursor offset is out of range while refreshing bracket highlights");
            return;
        }

        let mut tail = head;
        if (self.cursor_shape == CursorShape::Block || self.cursor_shape == CursorShape::Hollow)
            && head < buffer_snapshot.len()
        {
            if let Some(tail_ch) = buffer_snapshot.chars_at(tail).next() {
                tail += tail_ch.len_utf8();
            }
        }
        let task = cx.background_spawn({
            let buffer_snapshot = buffer_snapshot.clone();
            async move { dumb_innermost_enclosing_bracket_ranges(&buffer_snapshot, head..tail) }
        });
        self.refresh_matching_bracket_highlights_task = cx.spawn({
            let buffer_snapshot = buffer_snapshot.clone();
            async move |this, cx| {
                let bracket_ranges = task.await;
                let current_ranges = this
                    .read_with(cx, |editor, cx| {
                        editor
                            .display_map
                            .read(cx)
                            .text_highlights(HighlightKey::MatchingBracket)
                            .map(|(_, ranges)| ranges.to_vec())
                    })
                    .ok()
                    .flatten();
                let new_ranges = bracket_ranges.map(|(opening_range, closing_range)| {
                    vec![
                        opening_range.to_anchors(&buffer_snapshot),
                        closing_range.to_anchors(&buffer_snapshot),
                    ]
                });

                if current_ranges != new_ranges {
                    this.update(cx, |editor, cx| {
                        editor.clear_highlights(HighlightKey::MatchingBracket, cx);
                        if let Some(new_ranges) = new_ranges {
                            editor.highlight_text(
                                HighlightKey::MatchingBracket,
                                new_ranges,
                                HighlightStyle {
                                    background_color: Some(
                                        cx.theme()
                                            .colors()
                                            .editor_document_highlight_bracket_background,
                                    ),
                                    font_weight: Some(FontWeight::from(700.)),
                                    ..Default::default()
                                },
                                cx,
                            )
                        }
                    })
                    .ok();
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{editor_tests::init_test, test::editor_lsp_test_context::EditorLspTestContext};
    use indoc::indoc;
    use language::{BracketPair, BracketPairConfig, Language, LanguageConfig, LanguageMatcher};

    #[gpui::test]
    async fn test_matching_bracket_highlights(cx: &mut gpui::TestAppContext) {
        init_test(cx, |_| {});

        let mut cx = EditorLspTestContext::new(
            Language::new(
                LanguageConfig {
                    name: "Rust".into(),
                    matcher: LanguageMatcher {
                        path_suffixes: vec!["rs".to_string()],
                        ..Default::default()
                    },
                    brackets: BracketPairConfig {
                        pairs: vec![
                            BracketPair {
                                start: "{".to_string(),
                                end: "}".to_string(),
                                close: false,
                                surround: false,
                                newline: true,
                            },
                            BracketPair {
                                start: "(".to_string(),
                                end: ")".to_string(),
                                close: false,
                                surround: false,
                                newline: true,
                            },
                        ],
                        ..Default::default()
                    },
                    ..Default::default()
                },
                Some(tree_sitter_rust::LANGUAGE.into()),
            )
            .with_brackets_query(indoc! {r#"
                ("{" @open "}" @close)
                ("(" @open ")" @close)
                "#})
            .unwrap(),
            Default::default(),
            cx,
        )
        .await;

        // positioning cursor inside bracket highlights both
        cx.set_state(indoc! {r#"
            pub fn test("Test ˇargument") {
                another_test(1, 2, 3);
            }
        "#});
        cx.run_until_parked();
        cx.assert_editor_text_highlights(
            HighlightKey::MatchingBracket,
            indoc! {r#"
            pub fn test«(»"Test argument"«)» {
                another_test(1, 2, 3);
            }
        "#},
        );

        cx.set_state(indoc! {r#"
            pub fn test("Test argument") {
                another_test(1, ˇ2, 3);
            }
        "#});
        cx.run_until_parked();
        cx.assert_editor_text_highlights(
            HighlightKey::MatchingBracket,
            indoc! {r#"
            pub fn test("Test argument") {
                another_test«(»1, 2, 3«)»;
            }
        "#},
        );

        cx.set_state(indoc! {r#"
            pub fn test("Test argument") {
                anotherˇ_test(1, 2, 3);
            }
        "#});
        cx.run_until_parked();
        cx.assert_editor_text_highlights(
            HighlightKey::MatchingBracket,
            indoc! {r#"
            pub fn test("Test argument") «{»
                another_test(1, 2, 3);
            «}»
        "#},
        );

        // positioning outside of brackets removes highlight
        cx.set_state(indoc! {r#"
            pub fˇn test("Test argument") {
                another_test(1, 2, 3);
            }
        "#});
        cx.run_until_parked();
        cx.assert_editor_text_highlights(
            HighlightKey::MatchingBracket,
            indoc! {r#"
            pub fn test("Test argument") {
                another_test(1, 2, 3);
            }
        "#},
        );

        // non empty selection dismisses highlight
        cx.set_state(indoc! {r#"
            pub fn test("Te«st argˇ»ument") {
                another_test(1, 2, 3);
            }
        "#});
        cx.run_until_parked();
        cx.assert_editor_text_highlights(
            HighlightKey::MatchingBracket,
            indoc! {r#"
            pub fn test«("Test argument") {
                another_test(1, 2, 3);
            }
        "#},
        );
    }
}
