use lrpar::Lexeme;

pub enum TopLevelOption<StorageT> {
    GitHub(
        Lexeme<StorageT>,
        Vec<ProviderOption<StorageT>>,
        Vec<Match<StorageT>>,
    ),
    Listen(Lexeme<StorageT>),
    MaxJobs(Lexeme<StorageT>),
    User(Lexeme<StorageT>),
}

pub enum ProviderOption<StorageT> {
    ReposDir(Lexeme<StorageT>),
}

pub struct Match<StorageT> {
    pub re: Lexeme<StorageT>,
    pub options: Vec<PerRepoOption<StorageT>>,
}

pub enum PerRepoOption<StorageT> {
    Email(Lexeme<StorageT>),
    Queue(Lexeme<StorageT>, QueueKind),
    Secret(Lexeme<StorageT>),
    Timeout(Lexeme<StorageT>),
}

pub enum QueueKind {
    Evict,
    Parallel,
    Sequential,
}
