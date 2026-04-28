#[derive(Debug, Clone)]
pub enum LiveUpdateEvent {
    BackfillEpochProcessed,
    LiveHeadProcessed,
}
