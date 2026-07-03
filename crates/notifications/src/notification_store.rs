use gpui::{App, AppContext as _, Context, Entity, EventEmitter, Global};
use rpc::Notification;
use std::ops::Range;
use sum_tree::{Bias, SumTree};
use time::OffsetDateTime;

pub fn init(cx: &mut App) {
    let notification_store = cx.new(|cx| NotificationStore::new(cx));
    cx.set_global(GlobalNotificationStore(notification_store));
}

struct GlobalNotificationStore(Entity<NotificationStore>);

impl Global for GlobalNotificationStore {}

pub struct NotificationStore {
    notifications: SumTree<NotificationEntry>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum NotificationEvent {
    NotificationsUpdated {
        old_range: Range<usize>,
        new_count: usize,
    },
    NewNotification {
        entry: NotificationEntry,
    },
    NotificationRemoved {
        entry: NotificationEntry,
    },
    NotificationRead {
        entry: NotificationEntry,
    },
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct NotificationEntry {
    pub id: u64,
    pub notification: Notification,
    pub timestamp: OffsetDateTime,
    pub is_read: bool,
    pub response: Option<bool>,
}

#[derive(Clone, Debug, Default)]
pub struct NotificationSummary {
    max_id: u64,
    count: usize,
    unread_count: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
struct Count(usize);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
struct NotificationId(u64);

impl NotificationStore {
    pub fn global(cx: &App) -> Entity<Self> {
        cx.global::<GlobalNotificationStore>().0.clone()
    }

    pub fn new(_cx: &mut Context<Self>) -> Self {
        Self {
            notifications: Default::default(),
        }
    }

    pub fn notification_count(&self) -> usize {
        self.notifications.summary().count
    }

    pub fn unread_notification_count(&self) -> usize {
        self.notifications.summary().unread_count
    }

    // Get the nth newest notification.
    pub fn notification_at(&self, ix: usize) -> Option<&NotificationEntry> {
        let count = self.notifications.summary().count;
        if ix >= count {
            return None;
        }
        let ix = count - 1 - ix;
        let (.., item) = self
            .notifications
            .find::<Count, _>((), &Count(ix), Bias::Right);
        item
    }
    pub fn notification_for_id(&self, id: u64) -> Option<&NotificationEntry> {
        let (.., item) =
            self.notifications
                .find::<NotificationId, _>((), &NotificationId(id), Bias::Left);
        if let Some(item) = item
            && item.id == id
        {
            return Some(item);
        }
        None
    }
}

impl EventEmitter<NotificationEvent> for NotificationStore {}

impl sum_tree::Item for NotificationEntry {
    type Summary = NotificationSummary;

    fn summary(&self, _cx: ()) -> Self::Summary {
        NotificationSummary {
            max_id: self.id,
            count: 1,
            unread_count: if self.is_read { 0 } else { 1 },
        }
    }
}

impl sum_tree::ContextLessSummary for NotificationSummary {
    fn zero() -> Self {
        Default::default()
    }

    fn add_summary(&mut self, summary: &Self) {
        self.max_id = self.max_id.max(summary.max_id);
        self.count += summary.count;
        self.unread_count += summary.unread_count;
    }
}

impl sum_tree::Dimension<'_, NotificationSummary> for NotificationId {
    fn zero(_cx: ()) -> Self {
        Default::default()
    }

    fn add_summary(&mut self, summary: &NotificationSummary, _: ()) {
        debug_assert!(summary.max_id > self.0);
        self.0 = summary.max_id;
    }
}

impl sum_tree::Dimension<'_, NotificationSummary> for Count {
    fn zero(_cx: ()) -> Self {
        Default::default()
    }

    fn add_summary(&mut self, summary: &NotificationSummary, _: ()) {
        self.0 += summary.count;
    }
}
