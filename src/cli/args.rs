use clap::{Args, Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "mimori",
    author = "mimori team",
    version = "2.0.0",
    about = "High-performance AST code-intelligence and symbol-graph CLI"
)]
pub struct Cli {
    #[arg(long, global = true, help = "Output structured JSON")]
    pub json: bool,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    #[command(about = "Initialize .mimori memory and cache directory")]
    Init,

    #[command(about = "Extract token-dense code slice for a symbol or line range")]
    Slice(SliceArgs),

    #[command(about = "Search symbols or files across the repository")]
    Find(FindArgs),

    #[command(about = "Display upstream callers of a target symbol")]
    Up(TargetArgs),

    #[command(about = "Display downstream callees invoked by a target symbol")]
    Down(TargetArgs),

    #[command(about = "Evaluate transitive blast radius of a symbol across entry points")]
    Blast(BlastArgs),

    #[command(about = "Generate hierarchical, centrality-ranked architectural map")]
    Map(MapArgs),

    #[command(about = "Purge cached index data in .mimori/")]
    Clean(CleanArgs),

    #[command(about = "Append high-signal action record to activity journal")]
    Log(LogArgs),

    #[command(about = "Dump full context snapshot")]
    Dump(DumpArgs),
}

#[derive(Args, Debug)]
pub struct SliceArgs {
    #[arg(help = "Target coordinate (e.g. 'path/file.rs:symbol' or 'path/file.rs:#L10-50')")]
    pub target: String,

    #[arg(short = 'f', long, help = "Inline private local callee bodies")]
    pub follow_local: bool,

    #[arg(short = 'i', long, help = "Include top-of-file import statements")]
    pub with_imports: bool,
}

#[derive(Args, Debug)]
pub struct FindArgs {
    #[arg(help = "Search pattern")]
    pub pattern: String,

    #[arg(short = 's', long, help = "Symbols only")]
    pub symbols_only: bool,

    #[arg(short = 'f', long, help = "Files only")]
    pub files_only: bool,
}

#[derive(Args, Debug)]
pub struct TargetArgs {
    #[arg(help = "Target symbol or coordinate")]
    pub target: String,
}

#[derive(Args, Debug)]
pub struct BlastArgs {
    #[arg(help = "Target symbol or coordinate")]
    pub target: String,

    #[arg(short = 'd', long, default_value_t = 3, help = "Maximum traversal depth")]
    pub depth: usize,
}

#[derive(Args, Debug)]
pub struct MapArgs {
    #[arg(long, help = "Scope directory or module")]
    pub scope: Option<String>,

    #[arg(long, help = "Focus target symbol")]
    pub focus: Option<String>,

    #[arg(long, help = "Seed term for personalized ranking")]
    pub seed: Option<String>,

    #[arg(long, help = "Keep only the top N symbols by centrality")]
    pub limit: Option<usize>,
}

#[derive(Args, Debug)]
pub struct CleanArgs {
    #[arg(long, help = "Purge all caches and memory")]
    pub all: bool,
}

#[derive(Args, Debug)]
pub struct DumpArgs {
    #[arg(long, help = "Write context snapshot to .mimori/.cache/context.md")]
    pub file: bool,

    #[arg(long, help = "Scope directory or module")]
    pub scope: Option<String>,

    #[arg(long, help = "Seed term for personalized ranking")]
    pub seed: Option<String>,

    #[arg(long, help = "Keep only the top N symbols by centrality")]
    pub limit: Option<usize>,
}

#[derive(Args, Debug)]
pub struct LogArgs {
    #[arg(short = 'a', long, help = "Action name or slug (e.g. 'jwt-rotation')")]
    pub action: String,

    #[arg(short = 's', long, help = "Concise summary (<160 chars)")]
    pub summary: String,

    #[arg(short = 'f', long, help = "Comma-separated list of affected files")]
    pub files: Option<String>,
}
