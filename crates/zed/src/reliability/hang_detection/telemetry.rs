use std::thread::ThreadId;
use std::time::{Duration, Instant};

use collections::HashMap;
use hdrhistogram::Histogram;

struct Item<T: Hang> {
    total_hanged: Duration,
    slowest_poll: T,
    /// saturates if more then 256 measurements end up in the same bin
    histogram: Histogram<u8>,
}

struct Hangs<T: Hang> {
    hangs: HashMap<T::Descriptor, Item<T>>,
}

trait Hang: Clone {
    type Descriptor: std::hash::Hash + PartialEq + Eq;
    fn poll_duration(&self) -> Duration;
    fn descriptor(&self) -> Self::Descriptor;
}

impl Hang for gpui::TaskTiming {
    type Descriptor = std::panic::Location<'static>;

    fn poll_duration(&self) -> Duration {
        gpui::TaskTiming::poll_duration(self)
    }
    fn descriptor(&self) -> Self::Descriptor {
        *self.location
    }
}

impl Hang for gpui::ActionTiming {
    type Descriptor = &'static str;

    fn poll_duration(&self) -> Duration {
        self.duration()
    }
    fn descriptor(&self) -> Self::Descriptor {
        self.name
    }
}

impl<T: Hang> Hangs<T> {
    fn new() -> Self {
        Self {
            hangs: HashMap::default(),
        }
    }
    fn add(&mut self, new: T, min_recorded_us: u64) {
        const MICROSECONDS_MINUTE: u64 = 60 * 1000 * 1000;

        if self.hangs.len() > 1000 {
            log::warn!("Too many hanging tasks to track, can not add new");
            return;
        }

        self.hangs
            .entry(new.descriptor())
            .and_modify(|item| {
                item.total_hanged += new.poll_duration();
                item.histogram
                    .saturating_record(new.poll_duration().as_micros() as u64);
                if new.poll_duration() > item.slowest_poll.poll_duration() {
                    item.slowest_poll = new.clone();
                }
            })
            .or_insert({
                Item {
                    total_hanged: new.poll_duration(),
                    slowest_poll: new,
                    histogram: Histogram::new_with_bounds(min_recorded_us, MICROSECONDS_MINUTE, 3)
                        .expect("function parameters are constants and correct"),
                }
            });
    }
}

pub struct Reporter {
    record_slower_then: Duration,
    foreground_thread: ThreadId,
    last_send: Instant,

    foreground: Hangs<gpui::TaskTiming>,
    background: Hangs<gpui::TaskTiming>,
    actions: Hangs<gpui::ActionTiming>,
}

impl Reporter {
    pub fn new(foreground_thread: ThreadId) -> Self {
        Self {
            record_slower_then: Duration::from_millis(1),
            foreground_thread,
            last_send: Instant::now(),
            foreground: Hangs::new(),
            background: Hangs::new(),
            actions: Hangs::new(),
        }
    }

    pub fn update(
        &mut self,
        task_stats: &[gpui::ThreadTaskStatistics],
        action_stats: &gpui::ActionStatistics,
    ) {
        self.process_foreground(task_stats);
        self.process_background(task_stats);
        self.process_actions(action_stats);
    }

    pub fn send_periodically(&mut self) {
        // this should be a long period otherwise things like
        // hang density get
        if self.last_send.elapsed() > Duration::from_mins(30) {
            self.send()
        }
    }

    pub fn send(&mut self) {
    }

    fn process_foreground(&mut self, task_stats: &[gpui::ThreadTaskStatistics]) {
        let foreground_thread = self.foreground_thread;
        let Some(foreground) = task_stats.iter().find(|t| t.thread_id == foreground_thread) else {
            // during startup foreground thread might not have statistics yet
            return;
        };

        for hang in foreground
            .stats
            .longest_poll_times
            .into_iter()
            .filter(|task| task.poll_duration() > self.record_slower_then)
        {
            self.foreground
                .add(hang, self.record_slower_then.as_micros() as u64);
        }
    }
    fn process_background(&mut self, task_stats: &[gpui::ThreadTaskStatistics]) {
        let foreground_thread = self.foreground_thread;
        let background = task_stats
            .iter()
            .filter(|t| t.thread_id != foreground_thread);

        for worker in background {
            for hang in worker
                .stats
                .longest_poll_times
                .into_iter()
                .filter(|task| task.poll_duration() > self.record_slower_then)
            {
                self.background
                    .add(hang, self.record_slower_then.as_micros() as u64);
            }
        }
    }

    fn process_actions(&mut self, action_stats: &gpui::ActionStatistics) {
        for hang in action_stats
            .longest_runtimes(false)
            .filter(|action| action.runtime() > self.record_slower_then)
        {
            self.actions
                .add(hang, self.record_slower_then.as_micros() as u64);
        }
    }
}
