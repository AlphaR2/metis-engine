//! A provider that replays a fixed timeline, for tests and for dry runs against a local
//! validator. Each call to `observe` returns the current step; `advance` moves to the next.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;

use super::{ScoreSource, SourceError};
use crate::model::Observation;

#[derive(Clone)]
pub struct Scripted {
    name: &'static str,
    steps: Arc<Vec<Observation>>,
    cursor: Arc<AtomicUsize>,
}

impl Scripted {
    pub fn new(name: &'static str, steps: Vec<Observation>) -> Self {
        assert!(!steps.is_empty());
        Self { name, steps: Arc::new(steps), cursor: Arc::new(AtomicUsize::new(0)) }
    }

    /// Moves to the next step, stopping at the last.
    pub fn advance(&self) {
        let last = self.steps.len() - 1;
        let next = (self.cursor.load(Ordering::SeqCst) + 1).min(last);
        self.cursor.store(next, Ordering::SeqCst);
    }

    pub fn at_end(&self) -> bool {
        self.cursor.load(Ordering::SeqCst) == self.steps.len() - 1
    }
}

#[async_trait]
impl ScoreSource for Scripted {
    fn id(&self) -> &'static str {
        self.name
    }

    async fn observe(&self, _external_id: &str) -> Result<Observation, SourceError> {
        Ok(self.steps[self.cursor.load(Ordering::SeqCst)].clone())
    }
}
