use clap::{Args, Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "mimori",
    author = "mimori team",
    version,
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
    #[command(about = "Initialize .mimori cache directory and .agents memory substrate")]
    Init(InitArgs),

    #[command(about = "Extract token-dense code slice for a symbol or line range")]
    Slice(SliceArgs),

    #[command(about = "Search symbols or files across the repository")]
    Find(FindArgs),

    #[command(about = "Display upstream callers of a target symbol")]
    Up(TargetArgs),

    #[command(about = "Display downstream callees invoked by a target symbol")]
    Down(TargetArgs),

    #[command(about = "List files under a scope whose content lacks a pattern (absence query)")]
    Missing(MissingArgs),

    #[command(
        about = "Display non-call mentioners of a target symbol (property reads, type refs, arg/template mentions)"
    )]
    Uses(TargetArgs),
    #[command(about = "Evaluate transitive blast radius of a symbol across entry points")]
    Blast(BlastArgs),

    #[command(about = "Generate hierarchical, centrality-ranked architectural map")]
    Map(MapArgs),

    #[command(about = "Purge cached index data in .mimori/")]
    Clean(CleanArgs),

    #[command(about = "Show repository health: stats, hubs, dead-weight candidates")]
    Doctor(DoctorArgs),

    #[command(about = "Run mimori as a Model Context Protocol (MCP) JSON-RPC server over stdio")]
    Mcp(McpArgs),

    #[command(about = "Read, lint, or resolve project memory (.agents/memory.md)")]
    Memory(MemoryArgs),

    #[command(about = "Scan, check, or sync in-code ponytail technical debt")]
    Debt(DebtArgs),

    #[command(about = "Generate Turn-0 budget-aware prompt snapshot")]
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

    #[arg(
        long,
        help = "Token budget for markdown output (heuristic chars/token calibrated per language: 3 for Rust/Go, 4 for TS/JS, 5 for Python; core slice is never cut)"
    )]
    pub budget: Option<usize>,

    #[arg(
        short = 'n',
        long,
        help = "Prefix code block lines with line numbers (L{line}: ...)"
    )]
    pub numbered: bool,
}

#[derive(Args, Debug)]
pub struct FindArgs {
    #[arg(help = "Search pattern")]
    pub pattern: String,

    #[arg(
        short = 's',
        long,
        conflicts_with = "files_only",
        help = "Symbols only"
    )]
    pub symbols_only: bool,

    #[arg(short = 'f', long, help = "Files only")]
    pub files_only: bool,

    #[arg(short, long, help = "Maximum number of results to display")]
    pub limit: Option<usize>,
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

    #[arg(
        short = 'd',
        long,
        default_value_t = 3,
        help = "Maximum traversal depth"
    )]
    pub depth: usize,

    #[arg(long, help = "Traverse downstream callees instead of upstream callers")]
    pub down: bool,

    #[arg(
        long = "with-sinks",
        value_name = "NAMES",
        help = "Comma-separated literal sink substrings to sweep (e.g. 'console.,logPath,sendNotification'). Graph traversal can't see literals; this runs the rg-equivalent sweep in the same call and appends per-line hits."
    )]
    pub with_sinks: Option<String>,
}

#[derive(Args, Debug)]
pub struct MapArgs {
    #[arg(long, help = "Scope directory or module")]
    pub scope: Option<String>,

    #[arg(long, help = "Focus target symbol")]
    pub focus: Option<String>,

    #[arg(long, help = "Seed term for personalized ranking")]
    pub seed: Option<String>,

    #[arg(short = 'l', long, help = "Keep only the top N symbols by centrality")]
    pub limit: Option<usize>,
}

#[derive(Args, Debug)]
pub struct MissingArgs {
    #[arg(
        help = "Literal substring to look for; `|` separates alternatives (e.g. 'Remote|serverId')"
    )]
    pub pattern: String,

    #[arg(long, help = "Workspace-relative directory to sweep")]
    pub scope: Option<String>,

    #[arg(
        long,
        help = "Only consider files whose content contains this literal (e.g. a lane-defining marker)"
    )]
    pub defines: Option<String>,
}

#[derive(Args, Debug)]
pub struct CleanArgs {
    #[arg(long, help = "Purge all caches and memory")]
    pub all: bool,
}

#[derive(Args, Debug)]
pub struct DoctorArgs {
    #[arg(short = 'l', long, help = "Keep only the top N candidates per tier")]
    pub limit: Option<usize>,
}

#[derive(Args, Debug, Default)]
pub struct McpArgs {
    #[arg(
        short,
        long,
        help = "Workspace root directory (defaults to current directory)"
    )]
    pub workspace: Option<std::path::PathBuf>,
}

#[derive(Args, Debug, Default)]
pub struct InitArgs {
    #[arg(long, help = "Force overwrite of existing .agents template files")]
    pub force: bool,
}

#[derive(Args, Debug, Default)]
pub struct MemoryArgs {
    #[command(subcommand)]
    pub command: Option<MemoryCommand>,

    #[arg(long, help = "Section to display (epics, debt, vocab, gotchas)")]
    pub section: Option<String>,

    #[arg(long, help = "Token budget ceiling")]
    pub budget: Option<usize>,
}

#[derive(Subcommand, Debug)]
pub enum MemoryCommand {
    #[command(about = "Display project memory sections")]
    Show(MemoryShowArgs),

    #[command(about = "Lint .agents/memory.md format and 30-line debt ceiling")]
    Lint,

    #[command(about = "Alias for lint")]
    Check,

    #[command(about = "Surgically resolve debt item matching pattern by deleting it")]
    Resolve(MemoryResolveArgs),
}

#[derive(Args, Debug, Default)]
pub struct MemoryShowArgs {
    #[arg(long, help = "Section to display (epics, debt, vocab, gotchas)")]
    pub section: Option<String>,

    #[arg(long, help = "Token budget ceiling")]
    pub budget: Option<usize>,
}

#[derive(Args, Debug)]
pub struct MemoryResolveArgs {
    #[arg(help = "Substring or regex pattern matching debt line to resolve and delete")]
    pub pattern: String,
}

#[derive(Args, Debug, Default)]
pub struct DebtArgs {
    #[command(subcommand)]
    pub command: Option<DebtCommand>,

    #[arg(long, help = "Scope subdirectory")]
    pub scope: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum DebtCommand {
    #[command(about = "Scan source files for in-code ponytail debt markers")]
    List(DebtListArgs),

    #[command(about = "Synchronize in-code markers into .agents/memory.md")]
    Sync,

    #[command(about = "CI validation gate checking markers and 30-line ceiling")]
    Check,
}

#[derive(Args, Debug, Default)]
pub struct DebtListArgs {
    #[arg(long, help = "Scope subdirectory")]
    pub scope: Option<String>,
}

#[derive(Args, Debug)]
pub struct DumpArgs {
    #[arg(long, default_value_t = 1500, help = "Token budget for Turn-0 context")]
    pub budget: usize,

    #[arg(long, help = "Focus target symbol for personalized PageRank")]
    pub focus: Option<String>,
}
