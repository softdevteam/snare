use lrpar::Span;

pub enum TopLevelOption {
    GitHub(Span, Vec<ProviderOption>, Vec<Match>),
    Listen(Span),
    MaxJobs(Span),
    User(Span),
}

pub enum ProviderOption {
    ReposDir(Span),
}

pub struct Match {
    pub re: Span,
    pub options: Vec<PerRepoOption>,
}

pub enum PerRepoOption {
    Cmd(Span),
    Email(Span),
    ErrorCmd(Span),
    Queue(Span, QueueKind),
    Secret(Span),
    Timeout(Span),
}

pub enum QueueKind {
    Evict,
    Parallel,
    Sequential,
}
