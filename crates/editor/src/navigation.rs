use super::*;

impl Editor {
    pub fn move_left(&mut self, _: &MoveLeft, window: &mut Window, cx: &mut Context<Self>) {
        self.change_selections(Default::default(), window, cx, |s| {
            s.move_with(&mut |map, selection| {
                let cursor = if selection.is_empty() {
                    movement::left(map, selection.start)
                } else {
                    selection.start
                };
                selection.collapse_to(cursor, SelectionGoal::None);
            });
        })
    }

    pub fn select_left(&mut self, _: &SelectLeft, window: &mut Window, cx: &mut Context<Self>) {
        self.change_selections(Default::default(), window, cx, |s| {
            s.move_heads_with(&mut |map, head, _| (movement::left(map, head), SelectionGoal::None));
        })
    }

    pub fn move_right(&mut self, _: &MoveRight, window: &mut Window, cx: &mut Context<Self>) {
        self.change_selections(Default::default(), window, cx, |s| {
            s.move_with(&mut |map, selection| {
                let cursor = if selection.is_empty() {
                    movement::right(map, selection.end)
                } else {
                    selection.end
                };
                selection.collapse_to(cursor, SelectionGoal::None)
            });
        })
    }

    pub fn select_right(&mut self, _: &SelectRight, window: &mut Window, cx: &mut Context<Self>) {
        self.change_selections(Default::default(), window, cx, |s| {
            s.move_heads_with(&mut |map, head, _| {
                (movement::right(map, head), SelectionGoal::None)
            });
        });
    }

    pub fn move_up(&mut self, _: &MoveUp, window: &mut Window, cx: &mut Context<Self>) {
        if self.take_rename(true, window, cx).is_some() {
            return;
        }

        if self.mode.is_single_line() {
            cx.propagate();
            return;
        }

        let text_layout_details = &self.text_layout_details(window, cx);
        let selection_count = self.selections.count();
        let first_selection = self.selections.first_anchor();

        self.change_selections(Default::default(), window, cx, |s| {
            s.move_with(&mut |map, selection| {
                if !selection.is_empty() {
                    selection.goal = SelectionGoal::None;
                }
                let (cursor, goal) = movement::up(
                    map,
                    selection.start,
                    selection.goal,
                    false,
                    text_layout_details,
                );
                selection.collapse_to(cursor, goal);
            });
        });

        if selection_count == 1 && first_selection.range() == self.selections.first_anchor().range()
        {
            cx.propagate();
        }
    }

    pub fn move_up_by_lines(
        &mut self,
        action: &MoveUpByLines,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.take_rename(true, window, cx).is_some() {
            return;
        }

        if self.mode.is_single_line() {
            cx.propagate();
            return;
        }

        let text_layout_details = &self.text_layout_details(window, cx);

        self.change_selections(Default::default(), window, cx, |s| {
            s.move_with(&mut |map, selection| {
                if !selection.is_empty() {
                    selection.goal = SelectionGoal::None;
                }
                let (cursor, goal) = movement::up_by_rows(
                    map,
                    selection.start,
                    action.lines,
                    selection.goal,
                    false,
                    text_layout_details,
                );
                selection.collapse_to(cursor, goal);
            });
        })
    }

    pub fn move_down_by_lines(
        &mut self,
        action: &MoveDownByLines,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.take_rename(true, window, cx).is_some() {
            return;
        }

        if self.mode.is_single_line() {
            cx.propagate();
            return;
        }

        let text_layout_details = &self.text_layout_details(window, cx);

        self.change_selections(Default::default(), window, cx, |s| {
            s.move_with(&mut |map, selection| {
                if !selection.is_empty() {
                    selection.goal = SelectionGoal::None;
                }
                let (cursor, goal) = movement::down_by_rows(
                    map,
                    selection.start,
                    action.lines,
                    selection.goal,
                    false,
                    text_layout_details,
                );
                selection.collapse_to(cursor, goal);
            });
        })
    }

    pub fn select_down_by_lines(
        &mut self,
        action: &SelectDownByLines,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text_layout_details = &self.text_layout_details(window, cx);
        self.change_selections(Default::default(), window, cx, |s| {
            s.move_heads_with(&mut |map, head, goal| {
                movement::down_by_rows(map, head, action.lines, goal, false, text_layout_details)
            })
        })
    }

    pub fn select_up_by_lines(
        &mut self,
        action: &SelectUpByLines,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text_layout_details = &self.text_layout_details(window, cx);
        self.change_selections(Default::default(), window, cx, |s| {
            s.move_heads_with(&mut |map, head, goal| {
                movement::up_by_rows(map, head, action.lines, goal, false, text_layout_details)
            })
        })
    }

    pub fn select_page_up(
        &mut self,
        _: &SelectPageUp,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(row_count) = self.visible_row_count() else {
            return;
        };

        let text_layout_details = &self.text_layout_details(window, cx);

        self.change_selections(Default::default(), window, cx, |s| {
            s.move_heads_with(&mut |map, head, goal| {
                movement::up_by_rows(map, head, row_count, goal, false, text_layout_details)
            })
        })
    }

    pub fn move_page_up(
        &mut self,
        action: &MovePageUp,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.take_rename(true, window, cx).is_some() {
            return;
        }

        if matches!(self.mode, EditorMode::SingleLine) {
            cx.propagate();
            return;
        }

        let Some(row_count) = self.visible_row_count() else {
            return;
        };

        let effects = if action.center_cursor {
            SelectionEffects::scroll(Autoscroll::center())
        } else {
            SelectionEffects::default()
        };

        let text_layout_details = &self.text_layout_details(window, cx);

        self.change_selections(effects, window, cx, |s| {
            s.move_with(&mut |map, selection| {
                if !selection.is_empty() {
                    selection.goal = SelectionGoal::None;
                }
                let (cursor, goal) = movement::up_by_rows(
                    map,
                    selection.end,
                    row_count,
                    selection.goal,
                    false,
                    text_layout_details,
                );
                selection.collapse_to(cursor, goal);
            });
        });
    }

    pub fn select_up(&mut self, _: &SelectUp, window: &mut Window, cx: &mut Context<Self>) {
        let text_layout_details = &self.text_layout_details(window, cx);
        self.change_selections(Default::default(), window, cx, |s| {
            s.move_heads_with(&mut |map, head, goal| {
                movement::up(map, head, goal, false, text_layout_details)
            })
        })
    }

    pub fn move_down(&mut self, _: &MoveDown, window: &mut Window, cx: &mut Context<Self>) {
        if self.take_rename(true, window, cx).is_some() {
            return;
        }

        if self.mode.is_single_line() {
            cx.propagate();
            return;
        }

        let text_layout_details = &self.text_layout_details(window, cx);
        let selection_count = self.selections.count();
        let first_selection = self.selections.first_anchor();

        self.change_selections(Default::default(), window, cx, |s| {
            s.move_with(&mut |map, selection| {
                if !selection.is_empty() {
                    selection.goal = SelectionGoal::None;
                }
                let (cursor, goal) = movement::down(
                    map,
                    selection.end,
                    selection.goal,
                    false,
                    text_layout_details,
                );
                selection.collapse_to(cursor, goal);
            });
        });

        if selection_count == 1 && first_selection.range() == self.selections.first_anchor().range()
        {
            cx.propagate();
        }
    }

    pub fn select_page_down(
        &mut self,
        _: &SelectPageDown,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(row_count) = self.visible_row_count() else {
            return;
        };

        let text_layout_details = &self.text_layout_details(window, cx);

        self.change_selections(Default::default(), window, cx, |s| {
            s.move_heads_with(&mut |map, head, goal| {
                movement::down_by_rows(map, head, row_count, goal, false, text_layout_details)
            })
        })
    }

    pub fn move_page_down(
        &mut self,
        action: &MovePageDown,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.take_rename(true, window, cx).is_some() {
            return;
        }

        if matches!(self.mode, EditorMode::SingleLine) {
            cx.propagate();
            return;
        }

        let Some(row_count) = self.visible_row_count() else {
            return;
        };

        let effects = if action.center_cursor {
            SelectionEffects::scroll(Autoscroll::center())
        } else {
            SelectionEffects::default()
        };

        let text_layout_details = &self.text_layout_details(window, cx);
        self.change_selections(effects, window, cx, |s| {
            s.move_with(&mut |map, selection| {
                if !selection.is_empty() {
                    selection.goal = SelectionGoal::None;
                }
                let (cursor, goal) = movement::down_by_rows(
                    map,
                    selection.end,
                    row_count,
                    selection.goal,
                    false,
                    text_layout_details,
                );
                selection.collapse_to(cursor, goal);
            });
        });
    }

    pub fn select_down(&mut self, _: &SelectDown, window: &mut Window, cx: &mut Context<Self>) {
        let text_layout_details = &self.text_layout_details(window, cx);
        self.change_selections(Default::default(), window, cx, |s| {
            s.move_heads_with(&mut |map, head, goal| {
                movement::down(map, head, goal, false, text_layout_details)
            })
        });
    }

    pub fn move_to_previous_word_start(
        &mut self,
        _: &MoveToPreviousWordStart,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.change_selections(Default::default(), window, cx, |s| {
            s.move_cursors_with(&mut |map, head, _| {
                (
                    movement::previous_word_start(map, head),
                    SelectionGoal::None,
                )
            });
        })
    }

    pub fn move_to_previous_subword_start(
        &mut self,
        _: &MoveToPreviousSubwordStart,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.change_selections(Default::default(), window, cx, |s| {
            s.move_cursors_with(&mut |map, head, _| {
                (
                    movement::previous_subword_start(map, head),
                    SelectionGoal::None,
                )
            });
        })
    }

    pub fn select_to_previous_word_start(
        &mut self,
        _: &SelectToPreviousWordStart,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.change_selections(Default::default(), window, cx, |s| {
            s.move_heads_with(&mut |map, head, _| {
                (
                    movement::previous_word_start(map, head),
                    SelectionGoal::None,
                )
            });
        })
    }

    pub fn select_to_previous_subword_start(
        &mut self,
        _: &SelectToPreviousSubwordStart,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.change_selections(Default::default(), window, cx, |s| {
            s.move_heads_with(&mut |map, head, _| {
                (
                    movement::previous_subword_start(map, head),
                    SelectionGoal::None,
                )
            });
        })
    }

    pub fn move_to_next_word_end(
        &mut self,
        _: &MoveToNextWordEnd,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.change_selections(Default::default(), window, cx, |s| {
            s.move_cursors_with(&mut |map, head, _| {
                (movement::next_word_end(map, head), SelectionGoal::None)
            });
        })
    }

    pub fn move_to_next_subword_end(
        &mut self,
        _: &MoveToNextSubwordEnd,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.change_selections(Default::default(), window, cx, |s| {
            s.move_cursors_with(&mut |map, head, _| {
                (movement::next_subword_end(map, head), SelectionGoal::None)
            });
        })
    }

    pub fn select_to_next_word_end(
        &mut self,
        _: &SelectToNextWordEnd,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.change_selections(Default::default(), window, cx, |s| {
            s.move_heads_with(&mut |map, head, _| {
                (movement::next_word_end(map, head), SelectionGoal::None)
            });
        })
    }

    pub fn select_to_next_subword_end(
        &mut self,
        _: &SelectToNextSubwordEnd,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.change_selections(Default::default(), window, cx, |s| {
            s.move_heads_with(&mut |map, head, _| {
                (movement::next_subword_end(map, head), SelectionGoal::None)
            });
        })
    }

    pub fn move_to_beginning_of_line(
        &mut self,
        action: &MoveToBeginningOfLine,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let stop_at_indent = action.stop_at_indent && !self.mode.is_single_line();
        self.change_selections(Default::default(), window, cx, |s| {
            s.move_cursors_with(&mut |map, head, _| {
                (
                    movement::indented_line_beginning(
                        map,
                        head,
                        action.stop_at_soft_wraps,
                        stop_at_indent,
                    ),
                    SelectionGoal::None,
                )
            });
        })
    }

    pub fn select_to_beginning_of_line(
        &mut self,
        action: &SelectToBeginningOfLine,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let stop_at_indent = action.stop_at_indent && !self.mode.is_single_line();
        self.change_selections(Default::default(), window, cx, |s| {
            s.move_heads_with(&mut |map, head, _| {
                (
                    movement::indented_line_beginning(
                        map,
                        head,
                        action.stop_at_soft_wraps,
                        stop_at_indent,
                    ),
                    SelectionGoal::None,
                )
            });
        });
    }

    pub fn move_to_end_of_line(
        &mut self,
        action: &MoveToEndOfLine,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.change_selections(Default::default(), window, cx, |s| {
            s.move_cursors_with(&mut |map, head, _| {
                (
                    movement::line_end(map, head, action.stop_at_soft_wraps),
                    SelectionGoal::None,
                )
            });
        })
    }

    pub fn select_to_end_of_line(
        &mut self,
        action: &SelectToEndOfLine,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.change_selections(Default::default(), window, cx, |s| {
            s.move_heads_with(&mut |map, head, _| {
                (
                    movement::line_end(map, head, action.stop_at_soft_wraps),
                    SelectionGoal::None,
                )
            });
        })
    }

    pub fn move_to_start_of_paragraph(
        &mut self,
        _: &MoveToStartOfParagraph,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(self.mode, EditorMode::SingleLine) {
            cx.propagate();
            return;
        }
        self.change_selections(Default::default(), window, cx, |s| {
            s.move_with(&mut |map, selection| {
                selection.collapse_to(
                    movement::start_of_paragraph(map, selection.head(), 1),
                    SelectionGoal::None,
                )
            });
        })
    }

    pub fn move_to_end_of_paragraph(
        &mut self,
        _: &MoveToEndOfParagraph,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(self.mode, EditorMode::SingleLine) {
            cx.propagate();
            return;
        }
        self.change_selections(Default::default(), window, cx, |s| {
            s.move_with(&mut |map, selection| {
                selection.collapse_to(
                    movement::end_of_paragraph(map, selection.head(), 1),
                    SelectionGoal::None,
                )
            });
        })
    }

    pub fn select_to_start_of_paragraph(
        &mut self,
        _: &SelectToStartOfParagraph,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(self.mode, EditorMode::SingleLine) {
            cx.propagate();
            return;
        }
        self.change_selections(Default::default(), window, cx, |s| {
            s.move_heads_with(&mut |map, head, _| {
                (
                    movement::start_of_paragraph(map, head, 1),
                    SelectionGoal::None,
                )
            });
        })
    }

    pub fn select_to_end_of_paragraph(
        &mut self,
        _: &SelectToEndOfParagraph,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(self.mode, EditorMode::SingleLine) {
            cx.propagate();
            return;
        }
        self.change_selections(Default::default(), window, cx, |s| {
            s.move_heads_with(&mut |map, head, _| {
                (
                    movement::end_of_paragraph(map, head, 1),
                    SelectionGoal::None,
                )
            });
        })
    }

    pub fn move_to_start_of_excerpt(
        &mut self,
        _: &MoveToStartOfExcerpt,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(self.mode, EditorMode::SingleLine) {
            cx.propagate();
            return;
        }
        self.change_selections(Default::default(), window, cx, |s| {
            s.move_with(&mut |map, selection| {
                selection.collapse_to(
                    movement::start_of_excerpt(
                        map,
                        selection.head(),
                        workspace::searchable::Direction::Prev,
                    ),
                    SelectionGoal::None,
                )
            });
        })
    }

    pub fn move_to_start_of_next_excerpt(
        &mut self,
        _: &MoveToStartOfNextExcerpt,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(self.mode, EditorMode::SingleLine) {
            cx.propagate();
            return;
        }

        self.change_selections(Default::default(), window, cx, |s| {
            s.move_with(&mut |map, selection| {
                selection.collapse_to(
                    movement::start_of_excerpt(
                        map,
                        selection.head(),
                        workspace::searchable::Direction::Next,
                    ),
                    SelectionGoal::None,
                )
            });
        })
    }

    pub fn move_to_end_of_excerpt(
        &mut self,
        _: &MoveToEndOfExcerpt,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(self.mode, EditorMode::SingleLine) {
            cx.propagate();
            return;
        }
        self.change_selections(Default::default(), window, cx, |s| {
            s.move_with(&mut |map, selection| {
                selection.collapse_to(
                    movement::end_of_excerpt(
                        map,
                        selection.head(),
                        workspace::searchable::Direction::Next,
                    ),
                    SelectionGoal::None,
                )
            });
        })
    }

    pub fn move_to_end_of_previous_excerpt(
        &mut self,
        _: &MoveToEndOfPreviousExcerpt,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(self.mode, EditorMode::SingleLine) {
            cx.propagate();
            return;
        }
        self.change_selections(Default::default(), window, cx, |s| {
            s.move_with(&mut |map, selection| {
                selection.collapse_to(
                    movement::end_of_excerpt(
                        map,
                        selection.head(),
                        workspace::searchable::Direction::Prev,
                    ),
                    SelectionGoal::None,
                )
            });
        })
    }

    pub fn select_to_start_of_excerpt(
        &mut self,
        _: &SelectToStartOfExcerpt,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(self.mode, EditorMode::SingleLine) {
            cx.propagate();
            return;
        }
        self.change_selections(Default::default(), window, cx, |s| {
            s.move_heads_with(&mut |map, head, _| {
                (
                    movement::start_of_excerpt(map, head, workspace::searchable::Direction::Prev),
                    SelectionGoal::None,
                )
            });
        })
    }

    pub fn select_to_start_of_next_excerpt(
        &mut self,
        _: &SelectToStartOfNextExcerpt,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(self.mode, EditorMode::SingleLine) {
            cx.propagate();
            return;
        }
        self.change_selections(Default::default(), window, cx, |s| {
            s.move_heads_with(&mut |map, head, _| {
                (
                    movement::start_of_excerpt(map, head, workspace::searchable::Direction::Next),
                    SelectionGoal::None,
                )
            });
        })
    }

    pub fn select_to_end_of_excerpt(
        &mut self,
        _: &SelectToEndOfExcerpt,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(self.mode, EditorMode::SingleLine) {
            cx.propagate();
            return;
        }
        self.change_selections(Default::default(), window, cx, |s| {
            s.move_heads_with(&mut |map, head, _| {
                (
                    movement::end_of_excerpt(map, head, workspace::searchable::Direction::Next),
                    SelectionGoal::None,
                )
            });
        })
    }

    pub fn select_to_end_of_previous_excerpt(
        &mut self,
        _: &SelectToEndOfPreviousExcerpt,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(self.mode, EditorMode::SingleLine) {
            cx.propagate();
            return;
        }
        self.change_selections(Default::default(), window, cx, |s| {
            s.move_heads_with(&mut |map, head, _| {
                (
                    movement::end_of_excerpt(map, head, workspace::searchable::Direction::Prev),
                    SelectionGoal::None,
                )
            });
        })
    }

    pub fn move_to_beginning(
        &mut self,
        _: &MoveToBeginning,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(self.mode, EditorMode::SingleLine) {
            cx.propagate();
            return;
        }
        self.change_selections(Default::default(), window, cx, |s| {
            s.select_ranges(vec![Anchor::Min..Anchor::Min]);
        });
    }

    pub fn select_to_beginning(
        &mut self,
        _: &SelectToBeginning,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut selection = self.selections.last::<Point>(&self.display_snapshot(cx));
        selection.set_head(Point::zero(), SelectionGoal::None);
        self.change_selections(Default::default(), window, cx, |s| {
            s.select(vec![selection]);
        });
    }

    pub fn move_to_end(&mut self, _: &MoveToEnd, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.mode, EditorMode::SingleLine) {
            cx.propagate();
            return;
        }
        let cursor = self.buffer.read(cx).read(cx).len();
        self.change_selections(Default::default(), window, cx, |s| {
            s.select_ranges(vec![cursor..cursor])
        });
    }

    pub fn set_nav_history(&mut self, nav_history: Option<ItemNavHistory>) {
        self.nav_history = nav_history;
    }

    pub fn save_location(
        &mut self,
        _: &SaveLocation,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.create_nav_history_entry(cx);
    }

    pub fn create_nav_history_entry(&mut self, cx: &mut Context<Self>) {
        self.push_to_nav_history(
            self.selections.newest_anchor().head(),
            None,
            false,
            true,
            cx,
        );
    }

    pub fn go_to_singleton_buffer_point(
        &mut self,
        point: Point,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.go_to_singleton_buffer_range(point..point, window, cx);
    }

    pub fn go_to_singleton_buffer_range(
        &mut self,
        range: Range<Point>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.go_to_singleton_buffer_range_impl(range, true, window, cx);
    }

    /// Like `go_to_singleton_buffer_point`, but does not push a navigation
    /// history entry. Useful when the caller already recorded one (e.g. when
    /// a file was just opened and we only need to move the cursor).
    pub fn go_to_singleton_buffer_point_silently(
        &mut self,
        point: Point,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.go_to_singleton_buffer_range_impl(point..point, false, window, cx);
    }

    pub fn go_to_next_document_highlight(
        &mut self,
        _: &GoToNextDocumentHighlight,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.go_to_document_highlight_before_or_after_position(Direction::Next, window, cx);
    }

    pub fn go_to_prev_document_highlight(
        &mut self,
        _: &GoToPreviousDocumentHighlight,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.go_to_document_highlight_before_or_after_position(Direction::Prev, window, cx);
    }

    pub fn open_url(&mut self, _: &OpenUrl, window: &mut Window, cx: &mut Context<Self>) {
        let selection = self.selections.newest_anchor();
        let head = selection.head();
        let tail = selection.tail();

        let Some((buffer, start_position)) =
            self.buffer.read(cx).text_anchor_for_position(head, cx)
        else {
            return;
        };

        let end_position = if head != tail {
            let Some((_, pos)) = self.buffer.read(cx).text_anchor_for_position(tail, cx) else {
                return;
            };
            Some(pos)
        } else {
            None
        };

        let url_finder = cx.spawn_in(window, async move |_editor, cx| {
            let url = if let Some(end_pos) = end_position {
                find_url_from_range(&buffer, start_position..end_pos, cx)
            } else {
                find_url(&buffer, start_position, cx).map(|(_, url)| url)
            };

            if let Some(url) = url {
                cx.update(|window, cx| {
                    if parse_zed_link(&url, cx).is_some() {
                        window.dispatch_action(Box::new(zed_actions::OpenZedUrl { url }), cx);
                    } else {
                        cx.open_url(&url);
                    }
                })?;
            }

            anyhow::Ok(())
        });

        url_finder.detach();
    }

    pub fn open_selected_filename(
        &mut self,
        _: &OpenSelectedFilename,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = self.workspace() else {
            return;
        };

        let position = self.selections.newest_anchor().head();

        let Some((buffer, buffer_position)) =
            self.buffer.read(cx).text_anchor_for_position(position, cx)
        else {
            return;
        };

        let project = self.project.clone();

        cx.spawn_in(window, async move |_, cx| {
            let result = find_file(&buffer, project, buffer_position, cx).await;

            if let Some((_, file_target)) = result {
                let item = workspace
                    .update_in(cx, |workspace, window, cx| {
                        workspace.open_resolved_path(file_target.resolved_path.clone(), window, cx)
                    })?
                    .await?;

                file_target.navigate_item_to_position(item, cx);
            }
            anyhow::Ok(())
        })
        .detach();
    }

    pub(super) fn push_to_nav_history(
        &mut self,
        cursor_anchor: Anchor,
        new_position: Option<Point>,
        is_deactivate: bool,
        always: bool,
        cx: &mut Context<Self>,
    ) {
        let data = self.navigation_data(cursor_anchor, cx);
        if let Some(nav_history) = self.nav_history.as_mut() {
            if let Some(new_position) = new_position {
                let row_delta = (new_position.row as i64 - data.cursor_position.row as i64).abs();
                if row_delta == 0 || (row_delta < MIN_NAVIGATION_HISTORY_ROW_DELTA && !always) {
                    return;
                }
            }

            let cursor_row = data.cursor_position.row;
            nav_history.push(Some(data), Some(cursor_row), cx);
            cx.emit(EditorEvent::PushedToNavHistory {
                anchor: cursor_anchor,
                is_deactivate,
            })
        }
    }

    pub(super) fn go_to_next_change(
        &mut self,
        _: &GoToNextChange,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(selections) = self
            .change_list
            .next_change(1, Direction::Next)
            .map(|s| s.to_vec())
        {
            self.change_selections(Default::default(), window, cx, |s| {
                let map = s.display_snapshot();
                s.select_display_ranges(selections.iter().map(|a| {
                    let point = a.to_display_point(&map);
                    point..point
                }))
            })
        }
    }

    pub(super) fn go_to_previous_change(
        &mut self,
        _: &GoToPreviousChange,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(selections) = self
            .change_list
            .next_change(1, Direction::Prev)
            .map(|s| s.to_vec())
        {
            self.change_selections(Default::default(), window, cx, |s| {
                let map = s.display_snapshot();
                s.select_display_ranges(selections.iter().map(|a| {
                    let point = a.to_display_point(&map);
                    point..point
                }))
            })
        }
    }

    pub fn navigate_to_hover_links(
        &mut self,
        definitions: Vec<HoverLink>,
        window: &mut Window,
        cx: &mut Context<Editor>,
    ) -> Task<Result<Navigated>> {
        for def in definitions.into_iter() {
            match def {
                HoverLink::Url(url) => {
                    return cx.spawn_in(window, async move |_editor, cx| {
                        cx.update(|_window, cx| {
                            cx.open_url(&url);
                        })?;
                        Ok(Navigated::Yes)
                    });
                },
                HoverLink::File(file_target) => {
                    let workspace = self.workspace();
                    return cx.spawn_in(window, async move |_editor, cx| {
                        let Some(workspace) = workspace else {
                            return Ok(Navigated::No);
                        };
                        let item = workspace
                            .update_in(cx, |workspace, window, cx| {
                                workspace.open_resolved_path(
                                    file_target.resolved_path.clone(),
                                    window,
                                    cx,
                                )
                            })?
                            .await?;
                        file_target.navigate_item_to_position(item, cx);
                        Ok(Navigated::Yes)
                    });
                },
                _ => {}
            }
        }
        Task::ready(anyhow::Ok(Navigated::No))
    }

    pub(super) fn go_to_symbol_by_offset(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        offset: i8,
    ) -> Task<Result<()>> {
        let editor_snapshot = self.snapshot(window, cx);

        let cursor_offset = self
            .selections
            .newest::<MultiBufferOffset>(&editor_snapshot.display_snapshot)
            .head();

        cx.spawn_in(window, async move |editor, wcx| -> Result<()> {
            let Ok(Some(remote_id)) = editor.update(wcx, |ed, cx| {
                let buffer = ed.buffer.read(cx).as_singleton();
                Some(buffer.read(cx).remote_id())
            }) else {
                return Ok(());
            };

            let task = editor.update(wcx, |ed, cx| ed.buffer_outline_items(remote_id, cx))?;
            let outline_items: Vec<OutlineItem<text::Anchor>> = task.await;

            let multi_snapshot = editor_snapshot.buffer();
            let buffer_range = |range: &Range<_>| {
                Some(
                    multi_snapshot
                        .buffer_anchor_range_to_anchor_range(range.clone())?
                        .to_offset(multi_snapshot),
                )
            };

            wcx.update_window(wcx.window_handle(), |_, window, acx| {
                let current_idx = outline_items
                    .iter()
                    .enumerate()
                    .filter_map(|(idx, item)| {
                        // Find the closest outline item by distance between outline text and cursor location
                        let source_range = buffer_range(&item.source_range_for_text)?;
                        let distance_to_closest_endpoint = cmp::min(
                            (source_range.start.0 as isize - cursor_offset.0 as isize).abs(),
                            (source_range.end.0 as isize - cursor_offset.0 as isize).abs(),
                        );

                        let item_towards_offset =
                            (source_range.start.0 as isize - cursor_offset.0 as isize).signum()
                                == (offset as isize).signum();

                        let source_range_contains_cursor = source_range.contains(&cursor_offset);

                        // To pick the next outline to jump to, we should jump in the direction of the offset, and
                        // we should not already be within the outline's source range. We then pick the closest outline
                        // item.
                        (item_towards_offset && !source_range_contains_cursor)
                            .then_some((distance_to_closest_endpoint, idx))
                    })
                    .min()
                    .map(|(_, idx)| idx);

                let Some(idx) = current_idx else {
                    return;
                };

                let Some(range) = buffer_range(&outline_items[idx].source_range_for_text) else {
                    return;
                };
                let selection = [range.start..range.start];

                editor
                    .update(acx, |editor, ecx| {
                        editor.change_selections(
                            SelectionEffects::scroll(Autoscroll::newest()),
                            window,
                            ecx,
                            |s| s.select_ranges(selection),
                        );
                    })
                    .log_err();
            })?;

            Ok(())
        })
    }

    pub(super) fn go_to_next_symbol(
        &mut self,
        _: &GoToNextSymbol,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.go_to_symbol_by_offset(window, cx, 1).detach();
    }

    pub(super) fn go_to_previous_symbol(
        &mut self,
        _: &GoToPreviousSymbol,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.go_to_symbol_by_offset(window, cx, -1).detach();
    }

    fn navigation_data(&self, cursor_anchor: Anchor, cx: &mut Context<Self>) -> NavigationData {
        let display_snapshot = self.display_map.update(cx, |map, cx| map.snapshot(cx));
        let buffer = self.buffer.read(cx).read(cx);
        let cursor_position = cursor_anchor.to_point(&buffer);
        let scroll_anchor = self.scroll_manager.native_anchor(&display_snapshot, cx);
        let scroll_top_row = scroll_anchor.top_row(&buffer);
        drop(buffer);

        NavigationData {
            cursor_anchor,
            cursor_position,
            scroll_anchor,
            scroll_top_row,
        }
    }
    
    fn go_to_singleton_buffer_range_impl(
        &mut self,
        range: Range<Point>,
        record_nav_history: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let multibuffer = self.buffer().read(cx);
        let anchor_range = range.to_anchors(&multibuffer.snapshot(cx));
        self.change_selections(
            SelectionEffects::scroll(Autoscroll::for_go_to_definition(
                self.cursor_top_offset(cx),
                cx,
            ))
            .nav_history(record_nav_history),
            window,
            cx,
            |s| s.select_anchor_ranges([anchor_range]),
        );
    }

    fn go_to_document_highlight_before_or_after_position(
        &mut self,
        direction: Direction,
        window: &mut Window,
        cx: &mut Context<Editor>,
    ) {
        let snapshot = self.snapshot(window, cx);
        let buffer = &snapshot.buffer_snapshot();
        let position = self
            .selections
            .newest::<Point>(&snapshot.display_snapshot)
            .head();
        let anchor_position = buffer.anchor_after(position);

        // Get all document highlights (both read and write)
        let mut all_highlights = Vec::new();

        if let Some((_, read_highlights)) = self
            .background_highlights
            .get(&HighlightKey::DocumentHighlightRead)
        {
            all_highlights.extend(read_highlights.iter());
        }

        if let Some((_, write_highlights)) = self
            .background_highlights
            .get(&HighlightKey::DocumentHighlightWrite)
        {
            all_highlights.extend(write_highlights.iter());
        }

        if all_highlights.is_empty() {
            return;
        }

        // Sort highlights by position
        all_highlights.sort_by(|a, b| a.start.cmp(&b.start, buffer));

        let target_highlight = match direction {
            Direction::Next => {
                // Find the first highlight after the current position
                all_highlights
                    .iter()
                    .find(|highlight| highlight.start.cmp(&anchor_position, buffer).is_gt())
            }
            Direction::Prev => {
                // Find the last highlight before the current position
                all_highlights
                    .iter()
                    .rev()
                    .find(|highlight| highlight.end.cmp(&anchor_position, buffer).is_lt())
            }
        };

        if let Some(highlight) = target_highlight {
            let destination = highlight.start.to_point(buffer);
            let autoscroll = Autoscroll::center();

            self.unfold_ranges(&[destination..destination], false, false, cx);
            self.change_selections(SelectionEffects::scroll(autoscroll), window, cx, |s| {
                s.select_ranges([destination..destination]);
            });
        }
    }
}
