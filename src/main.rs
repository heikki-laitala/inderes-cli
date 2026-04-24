//! `inderes` — unofficial CLI wrapping the Inderes MCP server.
//!
//! Design goal: keep token cost small for agents. Instead of registering the
//! Inderes MCP server with an agent host (which loads every tool schema into
//! context), this binary talks MCP privately and exposes a handful of CLI
//! subcommands. Agents discover it through an OpenClaw skill.

mod auth;
mod commands;
mod mcp;
mod oauth;
mod skill;
mod storage;

use std::path::PathBuf;

use anyhow::Result;
use clap::{ArgAction, Parser, Subcommand};
use clap_complete::Shell;

pub const DEFAULT_ENDPOINT: &str = "https://mcp.inderes.com/";

#[derive(Debug, Parser)]
#[command(
    name = "inderes",
    version,
    about = "Unofficial CLI for the Inderes MCP server (https://mcp.inderes.com).",
    long_about = "Unofficial CLI for the Inderes MCP server.\n\n\
                  Not affiliated with Inderes Oyj. Requires an Inderes account with a\n\
                  Premium subscription. First run: `inderes login` to authenticate via\n\
                  your browser."
)]
pub struct Cli {
    /// Override the MCP HTTP endpoint (default: https://mcp.inderes.com/).
    #[arg(long, env = "INDERES_MCP_ENDPOINT", global = true)]
    endpoint: Option<String>,

    /// Emit raw JSON from MCP tools (for agents / scripting).
    #[arg(long, global = true)]
    json: bool,

    /// Increase logging verbosity (repeatable).
    #[arg(short, long, global = true, action = ArgAction::Count)]
    verbose: u8,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Sign in with your Inderes account (browser-based OAuth).
    Login {
        /// Print the auth URL instead of opening a browser.
        #[arg(long)]
        no_browser: bool,
    },
    /// Remove stored tokens.
    Logout,
    /// Show who you're signed in as.
    Whoami,

    /// Search for companies by name (partial matches supported).
    Search {
        /// Free-text company name or ticker fragment.
        query: String,
    },

    /// Historical financial fundamentals (income, valuation, margins).
    Fundamentals {
        /// One or more company IDs, e.g. COMPANY:200.
        #[arg(required = true)]
        company_ids: Vec<String>,
        /// Resolution: quarterly | yearly.
        #[arg(long, default_value = "yearly")]
        resolution: String,
        /// Fields to return (repeat for multiple).
        #[arg(long = "field", short = 'f')]
        fields: Vec<String>,
        /// Start year (inclusive).
        #[arg(long)]
        from_year: Option<i32>,
        /// End year (inclusive).
        #[arg(long)]
        to_year: Option<i32>,
    },

    /// Forward-looking analyst estimates + recommendations.
    Estimates {
        /// Company IDs. Omit to pull Inderes's entire covered universe.
        company_ids: Vec<String>,
        /// Fields to return (repeat for multiple).
        #[arg(long = "field", short = 'f', required = true)]
        fields: Vec<String>,
        /// Number of latest estimate transactions per company.
        #[arg(long, default_value_t = 1)]
        count: u32,
        /// Include quarterly estimates.
        #[arg(long)]
        quarters: bool,
        /// Limit to N unique years of estimates.
        #[arg(long, default_value_t = 1)]
        years: u32,
    },

    /// Reports, articles, analyst comments, releases.
    #[command(subcommand)]
    Content(ContentCmd),

    /// Annual/interim reports and other filings.
    #[command(subcommand)]
    Documents(DocumentsCmd),

    /// Low-level: call any MCP tool directly.
    Call {
        /// Tool name (e.g. list-transcripts). Omit when using --list.
        tool: Option<String>,
        /// Argument KEY=VALUE — VALUE is parsed as JSON if possible, else
        /// as a string. Repeat for multiple args.
        #[arg(long = "arg", short = 'a')]
        args: Vec<String>,
        /// Pass a raw JSON object as the tool arguments (overrides --arg).
        #[arg(long)]
        json_args: Option<String>,
        /// List all available MCP tools instead of calling one.
        #[arg(long)]
        list: bool,
    },

    /// Write SKILL.md into the chosen agent host's skills directory.
    InstallSkill {
        /// Agent host to install for (openclaw or hermes).
        host: skill::Host,
        /// Destination path (default: ~/.<host>/skills/inderes/SKILL.md).
        #[arg(long)]
        dest: Option<PathBuf>,
        /// Overwrite if it already exists.
        #[arg(long)]
        force: bool,
    },

    /// Emit shell completions to stdout.
    Completions {
        /// Shell to generate completions for.
        shell: Shell,
    },
}

#[derive(Debug, Subcommand)]
enum ContentCmd {
    /// List content items for a company or the full feed.
    List {
        /// Restrict to a single company ID.
        #[arg(long)]
        company_id: Option<String>,
        /// Content types (ANALYST_COMMENT, ARTICLE, COMPANY_REPORT, …).
        #[arg(long = "type", short = 't')]
        types: Vec<String>,
        /// Page size (1–100).
        #[arg(long, default_value_t = 20)]
        first: u32,
        /// Pagination cursor from a previous `pageInfo.endCursor`.
        #[arg(long)]
        after: Option<String>,
    },
    /// Fetch a single content item by ID or URL.
    Get {
        /// A contentId (e.g. ANALYST_COMMENT:directus-1234) or a full inderes.fi URL.
        id_or_url: String,
        /// Preferred language: fi | en | sv | da.
        #[arg(long)]
        lang: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
enum DocumentsCmd {
    /// List a company's own filings.
    List {
        /// Company ID, e.g. COMPANY:200.
        company_id: String,
        /// Page size (1–100).
        #[arg(long, default_value_t = 20)]
        first: u32,
        /// Pagination cursor.
        #[arg(long)]
        after: Option<String>,
    },
    /// Get document metadata + section table of contents.
    Get {
        /// Document ID.
        document_id: String,
    },
    /// Read specific sections of a document.
    Read {
        /// Document ID.
        document_id: String,
        /// Comma-separated section numbers, e.g. 1,2,5.
        #[arg(long, short = 's', value_delimiter = ',', required = true)]
        sections: Vec<u32>,
    },
}

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let cli = Cli::parse();
    init_tracing(cli.verbose);

    let http = reqwest::Client::builder()
        .user_agent(concat!("inderes-cli/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let endpoint = cli
        .endpoint
        .clone()
        .unwrap_or_else(|| DEFAULT_ENDPOINT.to_string());
    let ctx = commands::ToolCtx {
        http: &http,
        endpoint: &endpoint,
        json_output: cli.json,
    };

    match cli.command {
        Command::Login { no_browser } => commands::login(&http, no_browser).await,
        Command::Logout => commands::logout(),
        Command::Whoami => commands::whoami(&http, cli.verbose > 0).await,

        Command::Search { query } => commands::search(&ctx, &query).await,
        Command::Fundamentals {
            company_ids,
            resolution,
            fields,
            from_year,
            to_year,
        } => {
            commands::fundamentals(&ctx, company_ids, &resolution, fields, from_year, to_year).await
        }
        Command::Estimates {
            company_ids,
            fields,
            count,
            quarters,
            years,
        } => commands::estimates(&ctx, company_ids, fields, count, quarters, years).await,

        Command::Content(ContentCmd::List {
            company_id,
            types,
            first,
            after,
        }) => commands::content_list(&ctx, company_id, types, first, after).await,
        Command::Content(ContentCmd::Get { id_or_url, lang }) => {
            commands::content_get(&ctx, &id_or_url, lang).await
        }

        Command::Documents(DocumentsCmd::List {
            company_id,
            first,
            after,
        }) => commands::documents_list(&ctx, &company_id, first, after).await,
        Command::Documents(DocumentsCmd::Get { document_id }) => {
            commands::documents_get(&ctx, &document_id).await
        }
        Command::Documents(DocumentsCmd::Read {
            document_id,
            sections,
        }) => commands::documents_read(&ctx, &document_id, sections).await,

        Command::Call {
            tool,
            args,
            json_args,
            list,
        } => {
            if list {
                commands::call_list(&ctx).await
            } else {
                let tool = tool.ok_or_else(|| {
                    anyhow::anyhow!("missing tool name; pass --list to see all tools")
                })?;
                commands::call(&ctx, &tool, args, json_args).await
            }
        }

        Command::InstallSkill { host, dest, force } => {
            let path = commands::install_skill(host, dest, force)?;
            println!("Skill written to {}", path.display());
            Ok(())
        }

        Command::Completions { shell } => commands::completions(shell),
    }
}

fn init_tracing(verbose: u8) {
    use tracing_subscriber::EnvFilter;
    let default = match verbose {
        0 => "warn",
        1 => "info",
        _ => "debug",
    };
    let filter = EnvFilter::try_from_env("INDERES_LOG").unwrap_or_else(|_| EnvFilter::new(default));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}
