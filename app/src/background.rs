//! Work done on other threads whose results the window picks up on its next frame.

use std::sync::{Arc, OnceLock, mpsc};
use std::time::Duration;

/// Lets background threads wake the window as soon as a result is ready. It is shared
/// before the window exists, and learns its context on the first frame.
#[derive(Clone, Default)]
pub struct Wake(Arc<OnceLock<egui::Context>>);

impl Wake {
    pub fn set(&self, ctx: &egui::Context) {
        self.0.get_or_init(|| ctx.clone());
    }

    pub fn ctx(&self) -> Option<&egui::Context> {
        self.0.get()
    }

    pub fn repaint(&self) {
        if let Some(ctx) = self.ctx() {
            ctx.request_repaint();
        }
    }

    pub fn repaint_after(&self, wait: Duration) {
        if let Some(ctx) = self.ctx() {
            ctx.request_repaint_after(wait);
        }
    }
}

/// What a [`Task`] has to show for itself so far.
pub enum Polled<T> {
    Running,
    Done(T),
    /// The thread ended without a result.
    Lost,
}

/// One piece of work on its own thread, which wakes the window when it is done.
pub struct Task<T>(mpsc::Receiver<T>);

impl<T: Send + 'static> Task<T> {
    pub fn spawn(wake: &Wake, work: impl FnOnce() -> T + Send + 'static) -> Task<T> {
        let (tx, rx) = mpsc::channel();
        let wake = wake.clone();
        std::thread::spawn(move || {
            let _ = tx.send(work());
            wake.repaint();
        });
        Task(rx)
    }

    /// A task that has already finished, for tests.
    #[cfg(test)]
    pub fn done(value: T) -> Task<T> {
        let (tx, rx) = mpsc::channel();
        let _ = tx.send(value);
        Task(rx)
    }

    pub fn poll(&self) -> Polled<T> {
        match self.0.try_recv() {
            Ok(value) => Polled::Done(value),
            Err(mpsc::TryRecvError::Empty) => Polled::Running,
            Err(mpsc::TryRecvError::Disconnected) => Polled::Lost,
        }
    }
}

/// Takes the finished tasks out of `list`, with what each one gave, `None` for a lost one.
pub fn take_finished<K, T: Send + 'static>(list: &mut Vec<(K, Task<T>)>) -> Vec<(K, Option<T>)> {
    let mut finished = Vec::new();
    let mut i = 0;
    while i < list.len() {
        let result = match list[i].1.poll() {
            Polled::Running => {
                i += 1;
                continue;
            }
            Polled::Done(value) => Some(value),
            Polled::Lost => None,
        };
        let (key, _) = list.remove(i);
        finished.push((key, result));
    }
    finished
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finished_tasks_leave_the_list_in_order_and_running_ones_stay() {
        let (_keep, waiting) = mpsc::channel::<u8>();
        let lost = {
            let (tx, rx) = mpsc::channel::<u8>();
            drop(tx);
            Task(rx)
        };
        let mut list = vec![("a", Task::done(1)), ("b", Task(waiting)), ("c", lost), ("d", Task::done(4))];
        let finished = take_finished(&mut list);
        assert_eq!(finished, vec![("a", Some(1)), ("c", None), ("d", Some(4))]);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].0, "b");
    }

    #[test]
    fn a_spawned_task_delivers_its_result() {
        let task = Task::spawn(&Wake::default(), || 7);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            match task.poll() {
                Polled::Done(value) => break assert_eq!(value, 7),
                Polled::Lost => panic!("the thread lost its result"),
                Polled::Running => assert!(std::time::Instant::now() < deadline, "the task never finished"),
            }
            std::thread::yield_now();
        }
    }
}
