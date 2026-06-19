mod commands;
mod config;
mod crawl4ai;
mod http;
mod output;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "web",
    version,
    about = "LLM-friendly web access: fetch pages, search, get sourced answers, crawl sites"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Output structured JSON instead of markdown/plain text
    #[arg(long, global = true)]
    json: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Fetch a URL and return clean, LLM-ready markdown
    /// (crawl4ai -> Jina Reader -> Exa contents -> raw fetch)
    Fetch {
        /// URL to fetch
        url: String,

        /// Return raw fetched body without provider cleaning
        #[arg(long)]
        raw: bool,
    },

    /// Search the web and return a ranked list of links (Brave -> Exa)
    Search {
        /// Search query
        query: String,

        /// Number of results
        #[arg(long, short)]
        n: Option<usize>,

        /// Restrict to a single site/domain (e.g. example.com)
        #[arg(long)]
        site: Option<String>,

        /// Recency filter: day, week, month, year
        #[arg(long)]
        recency: Option<String>,
    },

    /// Ask a question and get a sourced answer with citations (Exa -> Perplexity)
    Answer {
        /// Question or query
        query: String,

        /// Provider: exa (default) or perplexity
        #[arg(long)]
        provider: Option<String>,
    },

    /// Crawl a site to markdown (crawl4ai -> Firecrawl).
    /// --map (list URLs) and --depth are Firecrawl-only.
    Crawl {
        /// Root URL to crawl
        url: String,

        /// Only list discovered URLs, do not fetch page content
        #[arg(long)]
        map: bool,

        /// Maximum number of pages to crawl
        #[arg(long, default_value = "20")]
        limit: usize,

        /// Maximum crawl depth from the root URL
        #[arg(long)]
        depth: Option<usize>,
    },
}

fn main() {
    let cli = Cli::parse();
    let json = cli.json;

    let result = match cli.command {
        Commands::Fetch { url, raw } => commands::fetch::run(&url, raw, json),
        Commands::Search {
            query,
            n,
            site,
            recency,
        } => commands::search::run(&query, n, site.as_deref(), recency.as_deref(), json),
        Commands::Answer { query, provider } => {
            commands::answer::run(&query, provider.as_deref(), json)
        }
        Commands::Crawl {
            url,
            map,
            limit,
            depth,
        } => commands::crawl::run(&url, map, limit, depth, json),
    };

    if let Err(e) = result {
        eprintln!("Error: {e:#}");
        std::process::exit(1);
    }
}
