use gpui::{App, Context, Entity};
use language::{self, Buffer, BufferEditSource, TransactionId};
use std::{
    collections::HashMap,
    ops::Range,
    time::Instant,
};
use sum_tree::Bias;
use text::BufferId;

use crate::{Anchor, MultiBufferOffset};

use super::{Event, MultiBuffer};

#[derive(Clone)]
pub(super) struct History {
    next_transaction_id: TransactionId,
    undo_stack: Vec<Transaction>,
    redo_stack: Vec<Transaction>,
    transaction_depth: usize,
}

impl Default for History {
    fn default() -> Self {
        History {
            next_transaction_id: clock::Lamport::MIN,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            transaction_depth: 0,
        }
    }
}

#[derive(Clone)]
struct Transaction {
    id: TransactionId,
    buffer_transactions: HashMap<BufferId, text::TransactionId>,
    suppress_grouping: bool,
}

impl History {
    fn push_transaction<'a, T>(
        &mut self,
        buffer_transactions: T,
        cx: &Context<MultiBuffer>,
    ) where
        T: IntoIterator<Item = (&'a Entity<Buffer>, &'a language::Transaction)>,
    {
        assert_eq!(self.transaction_depth, 0);
        let transaction = Transaction {
            id: self.next_transaction_id.tick(),
            buffer_transactions: buffer_transactions
                .into_iter()
                .map(|(buffer, transaction)| (buffer.read(cx).remote_id(), transaction.id))
                .collect(),
            suppress_grouping: false,
        };
        if !transaction.buffer_transactions.is_empty() {
            self.undo_stack.push(transaction);
            self.redo_stack.clear();
        }
    }

    fn finalize_last_transaction(&mut self) {
        if let Some(transaction) = self.undo_stack.last_mut() {
            transaction.suppress_grouping = true;
        }
    }

    fn transaction(&self, transaction_id: TransactionId) -> Option<&Transaction> {
        self.undo_stack
            .iter()
            .find(|transaction| transaction.id == transaction_id)
            .or_else(|| {
                self.redo_stack
                    .iter()
                    .find(|transaction| transaction.id == transaction_id)
            })
    }

    pub(super) fn transaction_depth(&self) -> usize {
        self.transaction_depth
    }
}

impl MultiBuffer {
    pub fn start_transaction(&mut self, cx: &mut Context<Self>) -> Option<TransactionId> {
        self.start_transaction_at(Instant::now(), cx)
    }

    pub fn start_transaction_at(
        &mut self,
        now: Instant,
        cx: &mut Context<Self>,
    ) -> Option<TransactionId> {
        let buffer = self.as_singleton();
        buffer.update(cx, |buffer, _| buffer.start_transaction_at(now))
    }

    pub fn last_transaction_id(&self, cx: &App) -> Option<TransactionId> {
        let buffer = self.as_singleton();
        buffer
            .read(cx)
            .peek_undo_stack()
            .map(|history_entry| history_entry.transaction_id())
    }

    pub fn end_transaction(&mut self, cx: &mut Context<Self>) -> Option<TransactionId> {
        self.end_transaction_at(Instant::now(), cx)
    }

    pub fn end_transaction_with_source(
        &mut self,
        source: BufferEditSource,
        cx: &mut Context<Self>,
    ) -> Option<TransactionId> {
        let buffer = self.as_singleton();
        buffer.update(cx, |buffer, cx| {
            buffer.end_transaction_with_source(source, cx)
        })
    }

    pub fn end_transaction_at(
        &mut self,
        now: Instant,
        cx: &mut Context<Self>,
    ) -> Option<TransactionId> {
        let buffer = self.as_singleton();
        buffer.update(cx, |buffer, cx| buffer.end_transaction_at(now, cx))
    }

    pub fn edited_ranges_for_transaction(
        &self,
        transaction_id: TransactionId,
        cx: &App,
    ) -> Vec<Range<MultiBufferOffset>> {
        let Some(transaction) = self.history.transaction(transaction_id) else {
            return Vec::new();
        };

        let snapshot = self.read(cx);
        let mut buffer_anchors = Vec::new();

        for (buffer_id, buffer_transaction) in &transaction.buffer_transactions {
            let Some(buffer) = self.buffer(*buffer_id) else {
                continue;
            };
            let Some(excerpt) = snapshot.first_excerpt_for_buffer(*buffer_id) else {
                continue;
            };
            let buffer_snapshot = buffer.read(cx).snapshot();

            for range in buffer
                .read(cx)
                .edited_ranges_for_transaction_id::<usize>(*buffer_transaction)
            {
                buffer_anchors.push(Anchor::in_buffer(
                    excerpt.path_key_index,
                    buffer_snapshot.anchor_at(range.start, Bias::Left),
                ));
                buffer_anchors.push(Anchor::in_buffer(
                    excerpt.path_key_index,
                    buffer_snapshot.anchor_at(range.end, Bias::Right),
                ));
            }
        }
        buffer_anchors.sort_unstable_by(|a, b| a.cmp(b, &snapshot));

        snapshot
            .summaries_for_anchors(buffer_anchors.iter())
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&[s, e]| s..e)
            .collect::<Vec<_>>()
    }

    pub fn merge_transactions(
        &mut self,
        transaction: TransactionId,
        destination: TransactionId,
        cx: &mut Context<Self>,
    ) {
        let buffer = self.as_singleton();
        buffer.update(cx, |buffer, _| {
            buffer.merge_transactions(transaction, destination)
        });
    }

    pub fn finalize_last_transaction(&mut self, cx: &mut Context<Self>) {
        self.history.finalize_last_transaction();
        if let Some(state) = &self.state {
            state.buffer.update(cx, |buffer, _| {
                buffer.finalize_last_transaction();
            });
        }
    }

    pub fn push_transaction<'a, T>(&mut self, buffer_transactions: T, cx: &Context<Self>)
    where
        T: IntoIterator<Item = (&'a Entity<Buffer>, &'a language::Transaction)>,
    {
        self.history
            .push_transaction(buffer_transactions, cx);
        self.history.finalize_last_transaction();
    }

    pub fn group_until_transaction(
        &mut self,
        transaction_id: TransactionId,
        cx: &mut Context<Self>,
    ) {
        let buffer = self.as_singleton();
        buffer.update(cx, |buffer, _| {
            buffer.group_until_transaction(transaction_id)
        });
    }

    pub fn undo(&mut self, cx: &mut Context<Self>) -> Option<TransactionId> {
        let buffer = self.as_singleton();
        let transaction_id = buffer.update(cx, |buffer, cx| buffer.undo(cx));

        if let Some(transaction_id) = transaction_id {
            cx.emit(Event::TransactionUndone { transaction_id });
        }

        transaction_id
    }

    pub fn redo(&mut self, cx: &mut Context<Self>) -> Option<TransactionId> {
        let buffer = self.as_singleton();
        buffer.update(cx, |buffer, cx| buffer.redo(cx))
    }

    pub fn undo_transaction(&mut self, transaction_id: TransactionId, cx: &mut Context<Self>) {
        let buffer = self.as_singleton();
        buffer.update(cx, |buffer, cx| buffer.undo_transaction(transaction_id, cx));
    }

    pub fn forget_transaction(&mut self, transaction_id: TransactionId, cx: &mut Context<Self>) {
        let buffer = self.as_singleton();
        buffer.update(cx, |buffer, _| {
            buffer.forget_transaction(transaction_id);
        });
    }
}
