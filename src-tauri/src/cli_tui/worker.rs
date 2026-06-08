#[derive(Debug, Clone, Eq, PartialEq)]
pub enum WorkerEvent {
    Idle,
}

impl WorkerEvent {
    pub fn label(&self) -> &'static str {
        match self {
            WorkerEvent::Idle => "idle",
        }
    }
}
