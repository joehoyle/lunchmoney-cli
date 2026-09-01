use std::{
    collections::HashMap,
    env, fs,
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::{Shell, generate};
use reqwest::{Client, Method, StatusCode};
use serde_json::Value;
use terminal_size::{Width, terminal_size};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const DEFAULT_BASE_URL: &str = "https://api.lunchmoney.dev/v2";

#[derive(Debug, Parser)]
#[command(name = "lunchmoney", version, about = "Agent-friendly CLI for the Lunch Money API", long_about = None)]
#[command(
    after_help = "AUTHENTICATION:\n  Run `lunchmoney auth login` to save a token for this user, set LUNCH_MONEY_TOKEN, or pass --token for one command.\n  Command-line and environment tokens take precedence over a saved token.\n\nOUTPUT:\n  Tables are used interactively and JSON when piped. Use --output json for agents and --compact for JSONL-friendly output.\n\nAPI:\n  This CLI targets Lunch Money API v2 (https://api.lunchmoney.dev/v2).\n\nEXAMPLES:\n  lunchmoney auth login\n  lunchmoney me\n  lunchmoney transactions list --start-date 2026-01-01 --end-date 2026-01-31 --all\n  lunchmoney raw get /recurring_items"
)]
struct Cli {
    /// Lunch Money developer API token. Prefer auth login or LUNCH_MONEY_TOKEN to avoid shell history.
    #[arg(long, env = "LUNCH_MONEY_TOKEN", global = true, hide_env_values = true)]
    token: Option<String>,

    /// API origin. Useful for mock servers and future compatible deployments.
    #[arg(long, env = "LUNCH_MONEY_BASE_URL", default_value = DEFAULT_BASE_URL, global = true)]
    base_url: String,

    /// Emit compact, single-line JSON (implies --output json).
    #[arg(long, global = true)]
    compact: bool,

    /// Choose output format. "auto" uses tables in a terminal and JSON when piped.
    #[arg(long, value_enum, default_value_t = OutputFormat::Auto, global = true)]
    output: OutputFormat,

    /// Print the HTTP method and URL to stderr before sending the request.
    #[arg(long, global = true)]
    verbose: bool,

    /// Show additional columns in human-readable tables.
    #[arg(long, global = true)]
    wide: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Show the authenticated user's profile.
    Me,
    /// Work with transactions.
    #[command(alias = "tx")]
    Transactions {
        #[command(subcommand)]
        command: Transactions,
    },
    /// Work with categories.
    #[command(alias = "cats")]
    Categories {
        #[command(subcommand)]
        command: ResourceCommand,
    },
    /// Work with tags.
    Tags {
        #[command(subcommand)]
        command: ResourceCommand,
    },
    /// Get a budget summary for a date range.
    Summary(SummaryArgs),
    /// Manage budget settings and period budgets.
    Budgets {
        #[command(subcommand)]
        command: Budgets,
    },
    /// List connected Plaid accounts, or trigger an import.
    Accounts {
        #[command(subcommand)]
        command: Accounts,
    },
    /// Manage manual accounts.
    #[command(alias = "manual")]
    ManualAccounts {
        #[command(subcommand)]
        command: ResourceCommand,
    },
    /// List recurring items, or fetch one by ID.
    #[command(alias = "rec")]
    Recurring(RecurringArgs),
    /// Call any Lunch Money endpoint directly; use this for newly released endpoints.
    Raw(RawArgs),
    /// Generate shell completion scripts.
    Completion { shell: Shell },
    /// Save, remove, or inspect the locally stored API token.
    Auth {
        #[command(subcommand)]
        command: Auth,
    },
}

#[derive(Debug, Subcommand)]
enum Auth {
    /// Prompt for and save a token for use by future shells.
    Login,
    /// Remove the locally stored token.
    Logout,
    /// Report whether a token is available, without displaying it.
    Status,
}

#[derive(Debug, Subcommand)]
enum Transactions {
    /// List this month's transactions. Use --month YYYY-MM for another month.
    #[command(alias = "ls")]
    List(TransactionList),
    /// Fetch one transaction by ID.
    #[command(alias = "show")]
    Get { id: i64 },
    /// Create a transaction from a JSON object.
    Create(BodyArgs),
    /// Fully replace a transaction with a JSON object.
    Update {
        id: i64,
        #[command(flatten)]
        body: BodyArgs,
    },
    /// Delete a transaction by ID.
    Delete { id: i64 },
}

#[derive(Debug, clap::Args)]
struct TransactionList {
    /// Calendar month to list (YYYY-MM). Fetches all transactions in that month.
    #[arg(long, value_name = "YYYY-MM", conflicts_with_all = ["start_date", "end_date"])]
    month: Option<String>,
    /// Inclusive start date (YYYY-MM-DD).
    #[arg(long)]
    start_date: Option<String>,
    /// Inclusive end date (YYYY-MM-DD).
    #[arg(long)]
    end_date: Option<String>,
    /// Only transactions created after this date or ISO 8601 timestamp.
    #[arg(long)]
    created_since: Option<String>,
    /// Only transactions updated after this date or ISO 8601 timestamp.
    #[arg(long)]
    updated_since: Option<String>,
    #[arg(long)]
    /// Filter by tag ID.
    tag_id: Option<i64>,
    #[arg(long)]
    /// Filter by category or category-group ID; use 0 for uncategorized.
    category_id: Option<i64>,
    #[arg(long)]
    /// Filter by manual account ID; use 0 to exclude manual accounts.
    manual_account_id: Option<i64>,
    #[arg(long)]
    /// Filter by Plaid account ID; use 0 to exclude Plaid accounts.
    plaid_account_id: Option<i64>,
    #[arg(long)]
    /// Filter by recurring item ID.
    recurring_id: Option<i64>,
    /// Filter by review status.
    #[arg(long, value_enum)]
    status: Option<TransactionStatus>,
    /// Only pending or only posted transactions.
    #[arg(long)]
    is_pending: Option<bool>,
    /// Include pending transactions (enabled by default; use --include-pending=false to disable).
    #[arg(
        long,
        default_value_t = true,
        default_missing_value = "true",
        num_args = 0..=1,
        require_equals = true,
        action = clap::ArgAction::Set
    )]
    include_pending: bool,
    /// Exclude pending transactions (convenient inverse of --include-pending).
    #[arg(long)]
    exclude_pending: bool,
    /// Include API and Plaid metadata.
    #[arg(long)]
    include_metadata: bool,
    /// Include original transactions that were split.
    #[arg(long)]
    include_split_parents: bool,
    /// Include transactions that belong to groups.
    #[arg(long)]
    include_group_children: bool,
    /// Populate child transactions for groups and split parents.
    #[arg(long)]
    include_children: bool,
    /// Include attachment metadata.
    #[arg(long)]
    include_files: bool,
    /// Return only transaction-group parents.
    #[arg(long)]
    is_group_parent: bool,
    /// Number of records per request (1-1000).
    #[arg(long, default_value_t = 25)]
    limit: usize,
    /// Zero-based page offset. Ignored when --all is used.
    #[arg(long, default_value_t = 0)]
    offset: usize,
    /// Follow pagination until the API reports there are no more results.
    #[arg(long)]
    all: bool,
}

#[derive(Clone, Debug, ValueEnum)]
enum TransactionStatus {
    Reviewed,
    Unreviewed,
    DeletePending,
}

impl TransactionStatus {
    fn api_value(&self) -> &'static str {
        match self {
            Self::Reviewed => "reviewed",
            Self::Unreviewed => "unreviewed",
            Self::DeletePending => "delete_pending",
        }
    }
}

#[derive(Debug, Subcommand)]
enum ResourceCommand {
    /// List all resources.
    #[command(alias = "ls")]
    List,
    /// Fetch one resource by ID.
    #[command(alias = "show")]
    Get { id: i64 },
    /// Create from a JSON object.
    Create(BodyArgs),
    /// Fully replace with a JSON object.
    Update {
        id: i64,
        #[command(flatten)]
        body: BodyArgs,
    },
    /// Delete by ID.
    Delete {
        id: i64,
        /// Delete even when dependencies exist (categories and tags only).
        #[arg(long)]
        force: bool,
    },
}

#[derive(Debug, Subcommand)]
enum Budgets {
    /// Show the account's budget period settings.
    Settings,
    /// Create or update a budget from a JSON object.
    Upsert(BodyArgs),
    /// Delete a budget for a category and period.
    Delete {
        #[arg(long)]
        category_id: i64,
        #[arg(long)]
        start_date: String,
    },
}

#[derive(Debug, Subcommand)]
enum Accounts {
    /// List all Plaid-linked accounts.
    Plaid,
    /// Queue an import from Plaid. The API rate-limits this operation.
    Fetch(FetchArgs),
}

#[derive(Debug, clap::Args)]
struct DateRange {
    #[arg(long)]
    start_date: String,
    #[arg(long)]
    end_date: String,
}

#[derive(Debug, clap::Args)]
struct SummaryArgs {
    #[command(flatten)]
    range: DateRange,
    #[arg(long)]
    include_excluded: bool,
    #[arg(long)]
    include_occurrences: bool,
    #[arg(long, requires = "include_occurrences")]
    include_past_budget_dates: bool,
    #[arg(long)]
    include_totals: bool,
    #[arg(long)]
    include_rollover_pool: bool,
}

#[derive(Debug, clap::Args)]
struct RecurringArgs {
    /// Fetch one recurring item instead of listing.
    #[arg(long)]
    id: Option<i64>,
    #[arg(long, requires = "end_date")]
    start_date: Option<String>,
    #[arg(long, requires = "start_date")]
    end_date: Option<String>,
    #[arg(long)]
    include_suggested: bool,
}

#[derive(Debug, clap::Args)]
struct FetchArgs {
    /// Fetch a single Plaid account.
    #[arg(long)]
    id: Option<i64>,
    #[arg(long, requires = "end_date")]
    start_date: Option<String>,
    #[arg(long, requires = "start_date")]
    end_date: Option<String>,
}

#[derive(Debug, clap::Args)]
struct BodyArgs {
    /// JSON object, inline or loaded from a file with @path.
    #[arg(long, value_name = "JSON|@FILE")]
    body: String,
}

#[derive(Debug, clap::Args)]
struct RawArgs {
    /// HTTP method.
    #[arg(value_enum)]
    method: HttpMethod,
    /// API path, such as /transactions. Absolute URLs are rejected.
    path: String,
    /// Query parameter, repeatable: --query key=value
    #[arg(short = 'q', long, value_parser = parse_key_value)]
    query: Vec<(String, String)>,
    /// JSON request body, inline or loaded from a file with @path.
    #[arg(long, value_name = "JSON|@FILE")]
    body: Option<String>,
}

#[derive(Clone, Debug, ValueEnum)]
enum HttpMethod {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}
impl From<HttpMethod> for Method {
    fn from(value: HttpMethod) -> Self {
        match value {
            HttpMethod::Get => Method::GET,
            HttpMethod::Post => Method::POST,
            HttpMethod::Put => Method::PUT,
            HttpMethod::Patch => Method::PATCH,
            HttpMethod::Delete => Method::DELETE,
        }
    }
}

#[derive(Clone, Debug, ValueEnum, PartialEq)]
enum OutputFormat {
    Auto,
    Json,
    Table,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum TableKind {
    Generic,
    Transactions,
    Categories,
    Tags,
    PlaidAccounts,
    ManualAccounts,
    Recurring,
    Summary,
}

#[derive(Clone, Default)]
struct RenderContext {
    categories: HashMap<i64, String>,
    accounts: HashMap<i64, String>,
    tags: HashMap<i64, String>,
    currency: Option<String>,
}

struct Api {
    client: Client,
    base_url: String,
    token: String,
    compact: bool,
    output: OutputFormat,
    verbose: bool,
    wide: bool,
}

impl Api {
    fn new(cli: &Cli) -> Result<Self> {
        let token = cli.token.clone().or(load_saved_token()?).context(
            "missing API token: run `lunchmoney auth login`, set LUNCH_MONEY_TOKEN, or pass --token",
        )?;
        let base_url = cli.base_url.trim_end_matches('/').to_owned();
        if !base_url.starts_with("https://") && !base_url.starts_with("http://") {
            bail!("--base-url must start with http:// or https://");
        }
        Ok(Self {
            client: Client::builder()
                .user_agent(concat!("lunchmoney/", env!("CARGO_PKG_VERSION")))
                .timeout(std::time::Duration::from_secs(30))
                .build()?,
            base_url,
            token,
            compact: cli.compact,
            output: cli.output.clone(),
            verbose: cli.verbose,
            wide: cli.wide,
        })
    }

    async fn request(
        &self,
        method: Method,
        path: &str,
        query: &[(String, String)],
        body: Option<Value>,
    ) -> Result<Value> {
        if !path.starts_with('/') || path.starts_with("//") {
            bail!("path must begin with one slash, e.g. /me");
        }
        let url = format!("{}{}", self.base_url, path);
        if self.verbose {
            eprintln!("{} {}", method, url);
        }
        let mut request = self
            .client
            .request(method, &url)
            .bearer_auth(&self.token)
            .query(query);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.context("request failed")?;
        let status = response.status();
        let text = response.text().await.context("could not read response")?;
        if !status.is_success() {
            bail!("{}", format_api_error(status, &text));
        }
        if text.trim().is_empty() || status == StatusCode::NO_CONTENT {
            return Ok(Value::Null);
        }
        serde_json::from_str(&text).context("API returned non-JSON data")
    }

    fn wants_table(&self) -> bool {
        !self.compact
            && (self.output == OutputFormat::Table
                || (self.output == OutputFormat::Auto && io::stdout().is_terminal()))
    }

    fn print(&self, value: &Value) -> Result<()> {
        self.print_as(value, TableKind::Generic, &RenderContext::default())
    }

    fn print_as(&self, value: &Value, kind: TableKind, context: &RenderContext) -> Result<()> {
        let stdout = io::stdout();
        let mut out = stdout.lock();
        let format = if self.compact {
            OutputFormat::Json
        } else if self.output == OutputFormat::Auto {
            if out.is_terminal() {
                OutputFormat::Table
            } else {
                OutputFormat::Json
            }
        } else {
            self.output.clone()
        };
        match format {
            OutputFormat::Json | OutputFormat::Auto => {
                if self.compact {
                    serde_json::to_writer(&mut out, value)?;
                } else {
                    serde_json::to_writer_pretty(&mut out, value)?;
                }
                writeln!(out)?;
            }
            OutputFormat::Table => {
                let color = out.is_terminal();
                render_table(&mut out, value, kind, context, self.wide, color)?
            }
        }
        Ok(())
    }
}

#[tokio::main]
async fn main() {
    if let Err(error) = run(Cli::parse()).await {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<()> {
    if let Commands::Completion { shell } = cli.command {
        generate(shell, &mut Cli::command(), "lunchmoney", &mut io::stdout());
        return Ok(());
    }
    if let Commands::Auth { command } = cli.command {
        return auth(command, cli.token.as_deref());
    }
    let api = Api::new(&cli)?;
    match cli.command {
        Commands::Me => api.print(&api.request(Method::GET, "/me", &[], None).await?),
        Commands::Transactions { command } => transactions(&api, command).await,
        Commands::Categories { command } => {
            resource(&api, "/categories", TableKind::Categories, command).await
        }
        Commands::Tags { command } => resource(&api, "/tags", TableKind::Tags, command).await,
        Commands::Summary(args) => {
            let value = api
                .request(Method::GET, "/summary", &summary_query(&args), None)
                .await?;
            let context = if api.wants_table() {
                summary_context(&api).await
            } else {
                RenderContext::default()
            };
            api.print_as(&value, TableKind::Summary, &context)
        }
        Commands::Budgets {
            command: Budgets::Settings,
        } => api.print(
            &api.request(Method::GET, "/budgets/settings", &[], None)
                .await?,
        ),
        Commands::Budgets {
            command: Budgets::Upsert(body),
        } => api.print(
            &api.request(Method::PUT, "/budgets", &[], Some(read_json(&body.body)?))
                .await?,
        ),
        Commands::Budgets {
            command:
                Budgets::Delete {
                    category_id,
                    start_date,
                },
        } => api.print(
            &api.request(
                Method::DELETE,
                "/budgets",
                &[
                    ("category_id".into(), category_id.to_string()),
                    ("start_date".into(), start_date),
                ],
                None,
            )
            .await?,
        ),
        Commands::Accounts {
            command: Accounts::Plaid,
        } => api.print_as(
            &api.request(Method::GET, "/plaid_accounts", &[], None)
                .await?,
            TableKind::PlaidAccounts,
            &RenderContext::default(),
        ),
        Commands::Accounts {
            command: Accounts::Fetch(args),
        } => api.print(
            &api.request(
                Method::POST,
                "/plaid_accounts/fetch",
                &fetch_query(&args),
                None,
            )
            .await?,
        ),
        Commands::ManualAccounts { command } => {
            resource(&api, "/manual_accounts", TableKind::ManualAccounts, command).await
        }
        Commands::Recurring(args) => {
            let path = args
                .id
                .map(|id| format!("/recurring_items/{id}"))
                .unwrap_or_else(|| "/recurring_items".into());
            let query = recurring_query(&args);
            let value = api.request(Method::GET, &path, &query, None).await?;
            let context = if args.id.is_none() && api.wants_table() {
                transaction_context(&api).await
            } else {
                RenderContext::default()
            };
            api.print_as(&value, TableKind::Recurring, &context)
        }
        Commands::Raw(args) => api.print(
            &api.request(
                args.method.into(),
                &args.path,
                &args.query,
                args.body.as_deref().map(read_json).transpose()?,
            )
            .await?,
        ),
        Commands::Completion { .. } | Commands::Auth { .. } => unreachable!(),
    }
}

fn auth(command: Auth, supplied_token: Option<&str>) -> Result<()> {
    match command {
        Auth::Login => {
            let token = match supplied_token {
                Some(token) => token.to_owned(),
                None => prompt_for_token()?,
            };
            if token.trim().is_empty() {
                bail!("API token cannot be empty");
            }
            save_token(token.trim())?;
            println!("Token saved. Future lunchmoney commands will use it automatically.");
        }
        Auth::Logout => {
            let path = token_file()?;
            match fs::remove_file(&path) {
                Ok(()) => println!("Saved token removed."),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    println!("No saved token found.")
                }
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("could not remove {}", path.display()));
                }
            }
        }
        Auth::Status => {
            if supplied_token.is_some() {
                println!("A token is available from --token or LUNCH_MONEY_TOKEN.");
            } else if load_saved_token()?.is_some() {
                println!("A saved token is available.");
            } else {
                println!("No token is configured. Run `lunchmoney auth login` to save one.");
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
fn prompt_for_token() -> Result<String> {
    eprint!("Lunch Money API token: ");
    io::stderr().flush()?;
    Command::new("stty")
        .arg("-echo")
        .status()
        .context("could not hide API token input")?
        .success()
        .then_some(())
        .context("could not hide API token input")?;

    let mut token = String::new();
    let read_result = io::stdin().read_line(&mut token);
    let restore_result = Command::new("stty").arg("echo").status();
    eprintln!();
    read_result.context("could not read API token")?;
    restore_result
        .context("could not restore terminal echo")?
        .success()
        .then_some(())
        .context("could not restore terminal echo")?;
    Ok(token)
}

#[cfg(not(unix))]
fn prompt_for_token() -> Result<String> {
    bail!(
        "interactive token entry is only supported on Unix; set LUNCH_MONEY_TOKEN and run `lunchmoney auth login`"
    )
}

fn token_file() -> Result<PathBuf> {
    let config_dir =
        if let Some(path) = env::var_os("XDG_CONFIG_HOME").filter(|path| !path.is_empty()) {
            PathBuf::from(path)
        } else if cfg!(target_os = "macos") {
            PathBuf::from(env::var_os("HOME").context("could not determine home directory")?)
                .join("Library/Application Support")
        } else if cfg!(windows) {
            PathBuf::from(env::var_os("APPDATA").context("could not determine app data directory")?)
        } else {
            PathBuf::from(env::var_os("HOME").context("could not determine home directory")?)
                .join(".config")
        };
    Ok(config_dir.join("lunchmoney").join("config.json"))
}

fn load_saved_token() -> Result<Option<String>> {
    let path = token_file()?;
    let contents = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("could not read {}", path.display()));
        }
    };
    let token = serde_json::from_str::<Value>(&contents)
        .with_context(|| format!("saved token file {} is invalid", path.display()))?
        .get("token")
        .and_then(Value::as_str)
        .filter(|token| !token.trim().is_empty())
        .map(str::to_owned);
    Ok(token)
}

fn save_token(token: &str) -> Result<()> {
    let path = token_file()?;
    let directory = path
        .parent()
        .context("token file has no parent directory")?;
    fs::create_dir_all(directory)
        .with_context(|| format!("could not create {}", directory.display()))?;
    restrict_directory_permissions(directory)?;
    let temporary = path.with_extension("json.tmp");
    fs::write(
        &temporary,
        serde_json::to_string_pretty(&serde_json::json!({ "token": token }))? + "\n",
    )
    .with_context(|| format!("could not write {}", temporary.display()))?;
    restrict_file_permissions(&temporary)?;
    fs::rename(&temporary, &path).with_context(|| format!("could not save {}", path.display()))?;
    Ok(())
}

#[cfg(unix)]
fn restrict_directory_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("could not secure {}", path.display()))
}

#[cfg(not(unix))]
fn restrict_directory_permissions(_: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn restrict_file_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("could not secure {}", path.display()))
}

#[cfg(not(unix))]
fn restrict_file_permissions(_: &Path) -> Result<()> {
    Ok(())
}

async fn transactions(api: &Api, command: Transactions) -> Result<()> {
    match command {
        Transactions::List(mut args) => {
            if args.start_date.is_some() != args.end_date.is_some() {
                bail!("--start-date and --end-date must be used together");
            }
            if args.limit == 0 || args.limit > 1000 {
                bail!("--limit must be between 1 and 1000");
            }
            let month_scope = if let Some(month) = args.month.clone() {
                Some(month)
            } else if args.start_date.is_none() && args.end_date.is_none() {
                Some(current_month()?)
            } else {
                None
            };
            if let Some(month) = &month_scope {
                apply_month_scope(&mut args, month)?;
            }
            let fetch_all = args.all || month_scope.is_some();
            let mut query = transaction_query(&args, args.offset);
            if !fetch_all {
                let value = api
                    .request(Method::GET, "/transactions", &query, None)
                    .await?;
                let context = if api.wants_table() {
                    transaction_context(api).await
                } else {
                    RenderContext::default()
                };
                return api.print_as(&value, TableKind::Transactions, &context);
            }
            let mut results = Vec::new();
            let mut offset = 0;
            loop {
                query = transaction_query(&args, offset);
                let page = api
                    .request(Method::GET, "/transactions", &query, None)
                    .await?;
                let items = page
                    .get("transactions")
                    .and_then(Value::as_array)
                    .context("unexpected transactions response")?;
                let count = items.len();
                results.extend(items.iter().cloned());
                let has_more = page
                    .get("has_more")
                    .and_then(Value::as_bool)
                    .unwrap_or(count == args.limit);
                if !has_more || count == 0 {
                    break;
                }
                offset += count;
            }
            let value = serde_json::json!({ "transactions": results, "has_more": false });
            let context = if api.wants_table() {
                transaction_context(api).await
            } else {
                RenderContext::default()
            };
            api.print_as(&value, TableKind::Transactions, &context)
        }
        Transactions::Get { id } => api.print(
            &api.request(Method::GET, &format!("/transactions/{id}"), &[], None)
                .await?,
        ),
        Transactions::Create(body) => api.print(
            &api.request(
                Method::POST,
                "/transactions",
                &[],
                Some(read_json(&body.body)?),
            )
            .await?,
        ),
        Transactions::Update { id, body } => api.print(
            &api.request(
                Method::PUT,
                &format!("/transactions/{id}"),
                &[],
                Some(read_json(&body.body)?),
            )
            .await?,
        ),
        Transactions::Delete { id } => api.print(
            &api.request(Method::DELETE, &format!("/transactions/{id}"), &[], None)
                .await?,
        ),
    }
}

async fn resource(api: &Api, base: &str, kind: TableKind, command: ResourceCommand) -> Result<()> {
    match command {
        ResourceCommand::List => api.print_as(
            &api.request(Method::GET, base, &[], None).await?,
            kind,
            &RenderContext::default(),
        ),
        ResourceCommand::Get { id } => api.print(
            &api.request(Method::GET, &format!("{base}/{id}"), &[], None)
                .await?,
        ),
        ResourceCommand::Create(body) => api.print(
            &api.request(Method::POST, base, &[], Some(read_json(&body.body)?))
                .await?,
        ),
        ResourceCommand::Update { id, body } => api.print(
            &api.request(
                Method::PUT,
                &format!("{base}/{id}"),
                &[],
                Some(read_json(&body.body)?),
            )
            .await?,
        ),
        ResourceCommand::Delete { id, force } => {
            if force && base == "/manual_accounts" {
                bail!("--force is only supported for categories and tags");
            }
            let query = if force {
                vec![("force".into(), "true".into())]
            } else {
                Vec::new()
            };
            api.print(
                &api.request(Method::DELETE, &format!("{base}/{id}"), &query, None)
                    .await?,
            )
        }
    }
}

fn transaction_query(args: &TransactionList, offset: usize) -> Vec<(String, String)> {
    let mut q = vec![
        ("limit".into(), args.limit.to_string()),
        ("offset".into(), offset.to_string()),
    ];
    for (key, value) in [
        ("start_date", &args.start_date),
        ("end_date", &args.end_date),
        ("created_since", &args.created_since),
        ("updated_since", &args.updated_since),
    ] {
        if let Some(value) = value {
            q.push((key.into(), value.clone()));
        }
    }
    for (key, value) in [
        ("tag_id", args.tag_id),
        ("category_id", args.category_id),
        ("manual_account_id", args.manual_account_id),
        ("plaid_account_id", args.plaid_account_id),
        ("recurring_id", args.recurring_id),
    ] {
        if let Some(value) = value {
            q.push((key.into(), value.to_string()));
        }
    }
    if let Some(status) = &args.status {
        q.push(("status".into(), status.api_value().into()));
    }
    if let Some(is_pending) = args.is_pending {
        q.push(("is_pending".into(), is_pending.to_string()));
    }
    let include_pending =
        args.include_pending && !args.exclude_pending && args.is_pending.is_none();
    for (key, enabled) in [
        ("include_pending", include_pending),
        ("include_metadata", args.include_metadata),
        ("include_split_parents", args.include_split_parents),
        ("include_group_children", args.include_group_children),
        ("include_children", args.include_children),
        ("include_files", args.include_files),
        ("is_group_parent", args.is_group_parent),
    ] {
        if enabled {
            q.push((key.into(), "true".into()));
        }
    }
    q
}

fn apply_month_scope(args: &mut TransactionList, month: &str) -> Result<()> {
    let (start_date, end_date) = month_range(month)?;
    args.start_date = Some(start_date);
    args.end_date = Some(end_date);
    Ok(())
}

fn month_range(month: &str) -> Result<(String, String)> {
    let (year, month_number) = month
        .split_once('-')
        .context("--month must use YYYY-MM, for example 2026-08")?;
    if year.len() != 4 || month_number.len() != 2 {
        bail!("--month must use YYYY-MM, for example 2026-08");
    }
    let year: u32 = year
        .parse()
        .context("--month must use YYYY-MM, for example 2026-08")?;
    let month_number: u32 = month_number
        .parse()
        .context("--month must use YYYY-MM, for example 2026-08")?;
    if year == 0 || !(1..=12).contains(&month_number) {
        bail!("--month must use a valid calendar month, for example 2026-08");
    }
    let last_day = days_in_month(year, month_number);
    Ok((
        format!("{year:04}-{month_number:02}-01"),
        format!("{year:04}-{month_number:02}-{last_day:02}"),
    ))
}

fn days_in_month(year: u32, month: u32) -> u32 {
    match month {
        4 | 6 | 9 | 11 => 30,
        2 if year % 400 == 0 || (year % 4 == 0 && year % 100 != 0) => 29,
        2 => 28,
        _ => 31,
    }
}

fn current_month() -> Result<String> {
    let output = Command::new("date")
        .arg("+%Y-%m")
        .output()
        .context("could not determine the current month")?;
    if !output.status.success() {
        bail!("could not determine the current month");
    }
    let month = String::from_utf8(output.stdout)
        .context("system date returned invalid text")?
        .trim()
        .to_owned();
    month_range(&month)?;
    Ok(month)
}

fn summary_query(args: &SummaryArgs) -> Vec<(String, String)> {
    let mut query = vec![
        ("start_date".into(), args.range.start_date.clone()),
        ("end_date".into(), args.range.end_date.clone()),
    ];
    for (key, enabled) in [
        ("include_exclude_from_budgets", args.include_excluded),
        ("include_occurrences", args.include_occurrences),
        ("include_past_budget_dates", args.include_past_budget_dates),
        ("include_totals", args.include_totals),
        ("include_rollover_pool", args.include_rollover_pool),
    ] {
        if enabled {
            query.push((key.into(), "true".into()));
        }
    }
    query
}

fn recurring_query(args: &RecurringArgs) -> Vec<(String, String)> {
    let mut query = Vec::new();
    for (key, value) in [
        ("start_date", &args.start_date),
        ("end_date", &args.end_date),
    ] {
        if let Some(value) = value {
            query.push((key.into(), value.clone()));
        }
    }
    if args.include_suggested {
        query.push(("include_suggested".into(), "true".into()));
    }
    query
}

fn fetch_query(args: &FetchArgs) -> Vec<(String, String)> {
    let mut query = Vec::new();
    if let Some(id) = args.id {
        query.push(("id".into(), id.to_string()));
    }
    for (key, value) in [
        ("start_date", &args.start_date),
        ("end_date", &args.end_date),
    ] {
        if let Some(value) = value {
            query.push((key.into(), value.clone()));
        }
    }
    query
}

async fn transaction_context(api: &Api) -> RenderContext {
    let (categories, plaid, manual, tags, me) = tokio::join!(
        api.request(Method::GET, "/categories", &[], None),
        api.request(Method::GET, "/plaid_accounts", &[], None),
        api.request(Method::GET, "/manual_accounts", &[], None),
        api.request(Method::GET, "/tags", &[], None),
        api.request(Method::GET, "/me", &[], None),
    );
    let mut context = RenderContext::default();
    if let Ok(value) = categories {
        collect_names(&mut context.categories, &value, "categories");
    }
    if let Ok(value) = plaid {
        collect_names(&mut context.accounts, &value, "plaid_accounts");
    }
    if let Ok(value) = manual {
        collect_names(&mut context.accounts, &value, "manual_accounts");
    }
    if let Ok(value) = tags {
        collect_names(&mut context.tags, &value, "tags");
    }
    if let Ok(value) = me {
        context.currency = value
            .get("primary_currency")
            .and_then(Value::as_str)
            .map(str::to_owned);
    }
    context
}

async fn summary_context(api: &Api) -> RenderContext {
    let (categories, me) = tokio::join!(
        api.request(Method::GET, "/categories", &[], None),
        api.request(Method::GET, "/me", &[], None),
    );
    let mut context = RenderContext::default();
    if let Ok(value) = categories {
        collect_names(&mut context.categories, &value, "categories");
    }
    if let Ok(value) = me {
        context.currency = value
            .get("primary_currency")
            .and_then(Value::as_str)
            .map(str::to_owned);
    }
    context
}

fn collect_names(target: &mut HashMap<i64, String>, value: &Value, array_key: &str) {
    let Some(items) = value.get(array_key).and_then(Value::as_array) else {
        return;
    };
    collect_item_names(target, items);
}

fn collect_item_names(target: &mut HashMap<i64, String>, items: &[Value]) {
    for item in items {
        if let (Some(id), Some(name)) = (
            item.get("id").and_then(Value::as_i64),
            item.get("display_name")
                .or_else(|| item.get("name"))
                .and_then(Value::as_str),
        ) {
            target.insert(id, name.to_owned());
        }
        if let Some(children) = item.get("children").and_then(Value::as_array) {
            collect_item_names(target, children);
        }
    }
}
fn read_json(input: &str) -> Result<Value> {
    let source = if let Some(path) = input.strip_prefix('@') {
        fs::read_to_string(PathBuf::from(path)).with_context(|| format!("could not read {path}"))?
    } else {
        input.to_owned()
    };
    let value: Value = serde_json::from_str(&source).context("body must be valid JSON")?;
    if !value.is_object() {
        bail!("body must be a JSON object");
    }
    Ok(value)
}
fn parse_key_value(input: &str) -> Result<(String, String), String> {
    input
        .split_once('=')
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .filter(|(k, _)| !k.is_empty())
        .ok_or_else(|| "use key=value".to_owned())
}

fn format_api_error(status: StatusCode, text: &str) -> String {
    let parsed = serde_json::from_str::<Value>(text).ok();
    let message = parsed
        .as_ref()
        .and_then(|value| value.get("message"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| text.trim());
    let details = parsed
        .as_ref()
        .and_then(|value| value.get("errors"))
        .and_then(Value::as_array)
        .map(|errors| {
            errors
                .iter()
                .filter_map(|error| error.get("errMsg").and_then(Value::as_str))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut output = format!("Lunch Money API returned {status}: {message}");
    if !details.is_empty() {
        output.push_str(" — ");
        output.push_str(&details.join("; "));
    }
    match status {
        StatusCode::UNAUTHORIZED => output.push_str(" (check LUNCH_MONEY_TOKEN)"),
        StatusCode::TOO_MANY_REQUESTS => output.push_str(" (wait, then retry)"),
        _ => {}
    }
    output
}

#[derive(Clone, Copy)]
enum Align {
    Left,
    Right,
}

#[derive(Clone)]
struct ColumnSpec {
    label: &'static str,
    key: &'static str,
    min: usize,
    max: usize,
    align: Align,
}

impl ColumnSpec {
    fn new(label: &'static str, key: &'static str, min: usize, max: usize) -> Self {
        Self {
            label,
            key,
            min,
            max,
            align: Align::Left,
        }
    }

    fn right(mut self) -> Self {
        self.align = Align::Right;
        self
    }
}

fn render_table(
    out: &mut impl Write,
    value: &Value,
    kind: TableKind,
    context: &RenderContext,
    wide: bool,
    color: bool,
) -> Result<()> {
    if kind == TableKind::Generic {
        return render_generic(out, value, color);
    }

    let mut effective_context = context.clone();
    if kind == TableKind::Categories {
        collect_names(&mut effective_context.categories, value, "categories");
    }
    let rows = table_rows(value, kind);
    if rows.is_empty() {
        writeln!(out, "No {} found.", resource_name(kind))?;
        return Ok(());
    }

    let columns = columns_for(kind, wide);
    let cells: Vec<Vec<String>> = rows
        .iter()
        .map(|row| {
            columns
                .iter()
                .map(|column| table_cell(row, kind, column.key, &effective_context))
                .collect()
        })
        .collect();
    let mut widths: Vec<usize> = columns
        .iter()
        .enumerate()
        .map(|(index, column)| {
            cells
                .iter()
                .map(|row| display_width(&row[index]))
                .max()
                .unwrap_or(0)
                .max(display_width(column.label))
                .clamp(column.min, column.max)
        })
        .collect();
    fit_widths(&mut widths, &columns, available_width());

    let headers: Vec<String> = columns
        .iter()
        .map(|column| column.label.to_owned())
        .collect();
    write_table_row(out, &headers, &columns, &widths, true, color, kind)?;
    write_table_rule(out, &widths)?;
    for row in &cells {
        write_table_row(out, row, &columns, &widths, false, color, kind)?;
    }

    let count = rows.len();
    let noun = if count == 1 {
        resource_name_singular(kind)
    } else {
        resource_name(kind)
    };
    write!(out, "\n{count} {noun}")?;
    if value.get("has_more").and_then(Value::as_bool) == Some(true) {
        write!(out, " • more available; use --all")?;
    }
    writeln!(out)?;
    Ok(())
}

fn table_rows(value: &Value, kind: TableKind) -> Vec<&Value> {
    let key = match kind {
        TableKind::Transactions => "transactions",
        TableKind::Categories | TableKind::Summary => "categories",
        TableKind::Tags => "tags",
        TableKind::PlaidAccounts => "plaid_accounts",
        TableKind::ManualAccounts => "manual_accounts",
        TableKind::Recurring => "recurring_items",
        TableKind::Generic => return Vec::new(),
    };
    match value {
        Value::Array(items) => items.iter().collect(),
        Value::Object(object) => object
            .get(key)
            .and_then(Value::as_array)
            .map(|items| items.iter().collect())
            .unwrap_or_else(|| vec![value]),
        _ => Vec::new(),
    }
}

fn columns_for(kind: TableKind, wide: bool) -> Vec<ColumnSpec> {
    let mut columns = match kind {
        TableKind::Transactions => vec![
            ColumnSpec::new("Date", "date", 10, 10),
            ColumnSpec::new("Payee", "payee", 8, 28),
            ColumnSpec::new("Category", "category", 8, 20),
            ColumnSpec::new("Account", "account", 8, 20),
            ColumnSpec::new("Amount", "amount", 10, 18).right(),
        ],
        TableKind::Categories => vec![
            ColumnSpec::new("ID", "id", 4, 10).right(),
            ColumnSpec::new("Name", "name", 12, 30),
            ColumnSpec::new("Kind", "kind", 7, 12),
            ColumnSpec::new("Group", "group", 8, 24),
            ColumnSpec::new("Flags", "flags", 8, 28),
        ],
        TableKind::Tags => vec![
            ColumnSpec::new("ID", "id", 4, 10).right(),
            ColumnSpec::new("Name", "name", 12, 28),
            ColumnSpec::new("Description", "description", 12, 40),
            ColumnSpec::new("Status", "status", 8, 10),
        ],
        TableKind::PlaidAccounts | TableKind::ManualAccounts => vec![
            ColumnSpec::new("ID", "id", 4, 10).right(),
            ColumnSpec::new("Account", "account", 12, 26),
            ColumnSpec::new("Institution", "institution", 10, 24),
            ColumnSpec::new("Type", "type", 8, 18),
            ColumnSpec::new("Balance", "balance", 10, 18).right(),
            ColumnSpec::new("Status", "status", 7, 12),
        ],
        TableKind::Recurring => vec![
            ColumnSpec::new("ID", "id", 4, 10).right(),
            ColumnSpec::new("Payee", "payee", 10, 26),
            ColumnSpec::new("Schedule", "schedule", 10, 18),
            ColumnSpec::new("Amount", "amount", 10, 18).right(),
            ColumnSpec::new("Category", "category", 8, 20),
            ColumnSpec::new("Status", "status", 8, 12),
        ],
        TableKind::Summary => vec![
            ColumnSpec::new("Category", "category", 12, 28),
            ColumnSpec::new("Budgeted", "budgeted", 10, 16).right(),
            ColumnSpec::new("Activity", "activity", 10, 16).right(),
            ColumnSpec::new("Available", "available", 10, 16).right(),
            ColumnSpec::new("Expected", "expected", 10, 16).right(),
            ColumnSpec::new("Remaining", "remaining", 10, 16).right(),
        ],
        TableKind::Generic => Vec::new(),
    };
    if wide {
        match kind {
            TableKind::Transactions => {
                columns.insert(0, ColumnSpec::new("ID", "id", 4, 10).right());
                columns.extend([
                    ColumnSpec::new("Status", "status", 8, 14),
                    ColumnSpec::new("Tags", "tags", 6, 20),
                    ColumnSpec::new("Notes", "notes", 8, 32),
                    ColumnSpec::new("Source", "source", 6, 12),
                ]);
            }
            TableKind::Categories => {
                columns.push(ColumnSpec::new("Description", "description", 10, 32))
            }
            TableKind::Tags => columns.push(ColumnSpec::new("Colors", "colors", 8, 22)),
            TableKind::PlaidAccounts => columns.extend([
                ColumnSpec::new("Last Import", "last_import", 12, 19),
                ColumnSpec::new("Mask", "mask", 4, 8),
            ]),
            TableKind::ManualAccounts => {
                columns.push(ColumnSpec::new("Balance As Of", "balance_as_of", 12, 19))
            }
            TableKind::Recurring => columns.extend([
                ColumnSpec::new("Missing", "missing", 7, 9).right(),
                ColumnSpec::new("Description", "description", 10, 32),
            ]),
            TableKind::Summary | TableKind::Generic => {}
        }
    }
    columns
}

fn table_cell(row: &Value, kind: TableKind, key: &str, context: &RenderContext) -> String {
    match (kind, key) {
        (_, "id") => value_at(row, "id"),
        (TableKind::Transactions, "date") => value_at(row, "date"),
        (TableKind::Transactions, "payee") => first_value(row, &["payee", "original_name"]),
        (TableKind::Transactions, "category") => {
            named_id(row, "category_id", &context.categories, "Uncategorized")
        }
        (TableKind::Transactions, "account") => transaction_account(row, context),
        (TableKind::Transactions, "amount") => money_at(row, "amount", "currency", context),
        (TableKind::Transactions, "status") => transaction_status(row),
        (TableKind::Transactions, "tags") => tag_names(row, context),
        (TableKind::Transactions, "notes") => value_at(row, "notes"),
        (TableKind::Transactions, "source") => title_case(&value_at(row, "source")),
        (TableKind::Categories, "name") => value_at(row, "name"),
        (TableKind::Categories, "kind") => category_kind(row),
        (TableKind::Categories, "group") => named_id(row, "group_id", &context.categories, "—"),
        (TableKind::Categories, "flags") => category_flags(row),
        (TableKind::Categories, "description") => value_at(row, "description"),
        (TableKind::Tags, "name") => value_at(row, "name"),
        (TableKind::Tags, "description") => value_at(row, "description"),
        (TableKind::Tags, "status") => archived_status(row),
        (TableKind::Tags, "colors") => format!(
            "{} / {}",
            value_at(row, "text_color"),
            value_at(row, "background_color")
        ),
        (TableKind::PlaidAccounts | TableKind::ManualAccounts, "account") => {
            first_value(row, &["display_name", "name"])
        }
        (TableKind::PlaidAccounts | TableKind::ManualAccounts, "institution") => {
            value_at(row, "institution_name")
        }
        (TableKind::PlaidAccounts | TableKind::ManualAccounts, "type") => account_type(row),
        (TableKind::PlaidAccounts | TableKind::ManualAccounts, "balance") => {
            money_at(row, "balance", "currency", context)
        }
        (TableKind::PlaidAccounts | TableKind::ManualAccounts, "status") => {
            title_case(&value_at(row, "status"))
        }
        (TableKind::PlaidAccounts, "last_import") => short_datetime(&value_at(row, "last_import")),
        (TableKind::PlaidAccounts, "mask") => value_at(row, "mask"),
        (TableKind::ManualAccounts, "balance_as_of") => {
            short_datetime(&value_at(row, "balance_as_of"))
        }
        (TableKind::Recurring, "payee") => recurring_payee(row),
        (TableKind::Recurring, "schedule") => recurring_schedule(row),
        (TableKind::Recurring, "amount") => money_at(
            row,
            "transaction_criteria.amount",
            "transaction_criteria.currency",
            context,
        ),
        (TableKind::Recurring, "category") => named_id(
            row,
            "overrides.category_id",
            &context.categories,
            "Uncategorized",
        ),
        (TableKind::Recurring, "status") => title_case(&value_at(row, "status")),
        (TableKind::Recurring, "missing") => array_len_at(row, "matches.missing_transaction_dates"),
        (TableKind::Recurring, "description") => value_at(row, "description"),
        (TableKind::Summary, "category") => {
            named_id(row, "category_id", &context.categories, "Unknown")
        }
        (TableKind::Summary, "budgeted") => money_at(row, "totals.budgeted", "", context),
        (TableKind::Summary, "activity") => summary_activity(row, context),
        (TableKind::Summary, "available") => money_at(row, "totals.available", "", context),
        (TableKind::Summary, "expected") => money_at(row, "totals.recurring_expected", "", context),
        (TableKind::Summary, "remaining") => {
            money_at(row, "totals.recurring_remaining", "", context)
        }
        _ => value_at(row, key),
    }
}

fn render_generic(out: &mut impl Write, value: &Value, color: bool) -> Result<()> {
    match value {
        Value::Object(object) => {
            let labels: Vec<String> = object.keys().map(|key| human_label(key)).collect();
            let width = labels
                .iter()
                .map(|label| display_width(label))
                .max()
                .unwrap_or(0)
                .min(28);
            for ((_, value), label) in object.iter().zip(labels) {
                let label = fit_cell(&label, width);
                if color {
                    write!(out, "\x1b[1m{}\x1b[0m", pad(&label, width, Align::Left))?;
                } else {
                    write!(out, "{}", pad(&label, width, Align::Left))?;
                }
                writeln!(out, "  {}", human_value(value))?;
            }
        }
        Value::Array(items) if items.is_empty() => writeln!(out, "No results found.")?,
        Value::Array(items) => {
            for item in items {
                writeln!(out, "{}", human_value(item))?;
            }
        }
        Value::Null => writeln!(out, "Done.")?,
        _ => writeln!(out, "{}", human_value(value))?,
    }
    Ok(())
}

fn value_at<'a>(value: &'a Value, path: &str) -> String {
    get_path(value, path)
        .map(human_value)
        .unwrap_or_else(|| "—".into())
}

fn get_path<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    if path.is_empty() {
        return None;
    }
    path.split('.')
        .try_fold(value, |current, key| current.get(key))
}

fn first_value(value: &Value, paths: &[&str]) -> String {
    paths
        .iter()
        .filter_map(|path| get_path(value, path))
        .find(|value| !value.is_null() && value.as_str() != Some(""))
        .map(human_value)
        .unwrap_or_else(|| "—".into())
}

fn named_id(value: &Value, path: &str, names: &HashMap<i64, String>, fallback: &str) -> String {
    let Some(id) = get_path(value, path).and_then(Value::as_i64) else {
        return fallback.into();
    };
    names
        .get(&id)
        .cloned()
        .unwrap_or_else(|| format!("Unknown (#{id})"))
}

fn transaction_account(value: &Value, context: &RenderContext) -> String {
    for key in ["plaid_account_id", "manual_account_id"] {
        if let Some(id) = value.get(key).and_then(Value::as_i64) {
            return context
                .accounts
                .get(&id)
                .cloned()
                .unwrap_or_else(|| format!("#{id}"));
        }
    }
    if value.get("is_group_parent").and_then(Value::as_bool) == Some(true) {
        "Transaction group".into()
    } else {
        "Cash".into()
    }
}

fn transaction_status(value: &Value) -> String {
    if value.get("is_pending").and_then(Value::as_bool) == Some(true) {
        "Pending".into()
    } else {
        title_case(&value_at(value, "status"))
    }
}

fn tag_names(value: &Value, context: &RenderContext) -> String {
    let Some(ids) = value.get("tag_ids").and_then(Value::as_array) else {
        return "—".into();
    };
    if ids.is_empty() {
        return "—".into();
    }
    ids.iter()
        .filter_map(Value::as_i64)
        .map(|id| {
            context
                .tags
                .get(&id)
                .cloned()
                .unwrap_or_else(|| format!("#{id}"))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn category_kind(value: &Value) -> String {
    if value.get("is_group").and_then(Value::as_bool) == Some(true) {
        "Group".into()
    } else if value.get("is_income").and_then(Value::as_bool) == Some(true) {
        "Income".into()
    } else {
        "Expense".into()
    }
}

fn category_flags(value: &Value) -> String {
    let mut flags = Vec::new();
    if value.get("exclude_from_budget").and_then(Value::as_bool) == Some(true) {
        flags.push("No budget");
    }
    if value.get("exclude_from_totals").and_then(Value::as_bool) == Some(true) {
        flags.push("No totals");
    }
    if value.get("archived").and_then(Value::as_bool) == Some(true) {
        flags.push("Archived");
    }
    if flags.is_empty() {
        "—".into()
    } else {
        flags.join(", ")
    }
}

fn archived_status(value: &Value) -> String {
    if value.get("archived").and_then(Value::as_bool) == Some(true) {
        "Archived".into()
    } else {
        "Active".into()
    }
}

fn account_type(value: &Value) -> String {
    let account_type = value_at(value, "type");
    let subtype = value_at(value, "subtype");
    if subtype == "—" || subtype.eq_ignore_ascii_case(&account_type) {
        title_case(&account_type)
    } else {
        format!("{} / {}", title_case(&account_type), title_case(&subtype))
    }
}

fn recurring_payee(value: &Value) -> String {
    first_value(
        value,
        &[
            "overrides.payee",
            "transaction_criteria.payee",
            "description",
        ],
    )
}

fn recurring_schedule(value: &Value) -> String {
    let quantity = get_path(value, "transaction_criteria.quantity")
        .and_then(Value::as_i64)
        .unwrap_or(1);
    let unit = get_path(value, "transaction_criteria.granularity")
        .and_then(Value::as_str)
        .unwrap_or("period");
    if quantity == 1 {
        format!("Every {unit}")
    } else {
        format!("Every {quantity} {unit}s")
    }
}

fn array_len_at(value: &Value, path: &str) -> String {
    get_path(value, path)
        .and_then(Value::as_array)
        .map(|items| items.len().to_string())
        .unwrap_or_else(|| "0".into())
}

fn summary_activity(value: &Value, context: &RenderContext) -> String {
    let other = get_path(value, "totals.other_activity")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    let recurring = get_path(value, "totals.recurring_activity")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    format_money(other + recurring, context.currency.as_deref())
}

fn money_at(
    value: &Value,
    amount_path: &str,
    currency_path: &str,
    context: &RenderContext,
) -> String {
    let Some(amount) = get_path(value, amount_path).and_then(number_value) else {
        return "—".into();
    };
    let currency = get_path(value, currency_path)
        .and_then(Value::as_str)
        .or(context.currency.as_deref());
    format_money(amount, currency)
}

fn number_value(value: &Value) -> Option<f64> {
    value.as_f64().or_else(|| value.as_str()?.parse().ok())
}

fn format_money(amount: f64, currency: Option<&str>) -> String {
    let absolute = format!("{:.2}", amount.abs());
    let (whole, decimal) = absolute.split_once('.').unwrap_or((&absolute, "00"));
    let mut grouped = String::new();
    for (index, character) in whole.chars().rev().enumerate() {
        if index > 0 && index % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(character);
    }
    let whole: String = grouped.chars().rev().collect();
    let sign = if amount < 0.0 { "−" } else { "" };
    match currency {
        Some(currency) => format!("{sign}{whole}.{decimal} {}", currency.to_uppercase()),
        None => format!("{sign}{whole}.{decimal}"),
    }
}

fn human_value(value: &Value) -> String {
    match value {
        Value::Null => "—".into(),
        Value::String(text) if text.is_empty() => "—".into(),
        Value::String(text) => text.replace(['\n', '\r', '\t'], " "),
        Value::Bool(true) => "Yes".into(),
        Value::Bool(false) => "No".into(),
        Value::Number(value) => value.to_string(),
        Value::Array(items) if items.is_empty() => "—".into(),
        Value::Array(items) if items.iter().all(Value::is_string) => items
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(", "),
        _ => serde_json::to_string(value).unwrap_or_else(|_| "<unprintable>".into()),
    }
}

fn human_label(key: &str) -> String {
    key.split('_')
        .map(|word| match word.to_ascii_lowercase().as_str() {
            "id" => "ID".into(),
            "api" => "API".into(),
            other => {
                let mut chars = other.chars();
                chars
                    .next()
                    .map(|first| first.to_uppercase().chain(chars).collect())
                    .unwrap_or_default()
            }
        })
        .collect::<Vec<String>>()
        .join(" ")
}

fn title_case(value: &str) -> String {
    value
        .split('_')
        .map(|word| {
            let mut chars = word.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().chain(chars).collect())
                .unwrap_or_default()
        })
        .collect::<Vec<String>>()
        .join(" ")
}

fn short_datetime(value: &str) -> String {
    value.get(..19).unwrap_or(value).replace('T', " ")
}

fn available_width() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|value| value.parse().ok())
        .or_else(|| terminal_size().map(|(Width(width), _)| usize::from(width)))
        .unwrap_or(100)
        .max(40)
}

fn fit_widths(widths: &mut [usize], columns: &[ColumnSpec], available: usize) {
    let separators = widths.len().saturating_sub(1) * 3;
    while widths.iter().sum::<usize>() + separators > available {
        let Some((index, _)) = widths
            .iter()
            .enumerate()
            .filter(|(index, width)| **width > columns[*index].min)
            .max_by_key(|(_, width)| **width)
        else {
            break;
        };
        widths[index] -= 1;
    }
}

fn write_table_row(
    out: &mut impl Write,
    row: &[String],
    columns: &[ColumnSpec],
    widths: &[usize],
    header: bool,
    color: bool,
    kind: TableKind,
) -> Result<()> {
    let muted_transaction = !header
        && color
        && matches!(kind, TableKind::Transactions)
        && transaction_is_payment_transfer(row, columns);
    if muted_transaction {
        write!(out, "\x1b[2m")?;
    }
    for (index, value) in row.iter().enumerate() {
        if index > 0 {
            write!(out, " │ ")?;
        }
        let cell_color = if !muted_transaction && color && matches!(kind, TableKind::Transactions) {
            transaction_color(columns[index].key, value)
        } else {
            None
        };
        let value = fit_cell(value, widths[index]);
        let value = pad(&value, widths[index], columns[index].align);
        if header && color {
            write!(out, "\x1b[1m{value}\x1b[0m")?;
        } else if color && matches!(kind, TableKind::Transactions) {
            if let Some(color) = cell_color {
                write!(out, "{color}{value}\x1b[0m")?;
            } else {
                write!(out, "{value}")?;
            }
        } else {
            write!(out, "{value}")?;
        }
    }
    if muted_transaction {
        write!(out, "\x1b[0m")?;
    }
    writeln!(out)?;
    Ok(())
}

fn transaction_is_payment_transfer(row: &[String], columns: &[ColumnSpec]) -> bool {
    columns
        .iter()
        .zip(row)
        .any(|(column, value)| column.key == "category" && value == "Payment, Transfer")
}

fn transaction_color(key: &str, value: &str) -> Option<&'static str> {
    let value = value.trim();
    match key {
        "date" => Some("\x1b[2m"),
        "category" if value == "Payment, Transfer" => Some("\x1b[2m"),
        "category" => Some("\x1b[35m"),
        "account" => Some("\x1b[36m"),
        "amount" if value.starts_with(['−', '-']) => Some("\x1b[32m"),
        "amount" if value != "—" => Some("\x1b[31m"),
        "status" if value.trim() == "Pending" => Some("\x1b[33m"),
        "status" => Some("\x1b[2m"),
        _ => None,
    }
}

fn write_table_rule(out: &mut impl Write, widths: &[usize]) -> Result<()> {
    for (index, width) in widths.iter().enumerate() {
        if index > 0 {
            write!(out, "─┼─")?;
        }
        write!(out, "{}", "─".repeat(*width))?;
    }
    writeln!(out)?;
    Ok(())
}

fn display_width(value: &str) -> usize {
    UnicodeWidthStr::width(value)
}

fn fit_cell(value: &str, width: usize) -> String {
    if display_width(value) <= width {
        return value.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    let target = width.saturating_sub(1);
    let mut result = String::new();
    let mut used = 0;
    for character in value.chars() {
        let character_width = character.width().unwrap_or(0);
        if used + character_width > target {
            break;
        }
        result.push(character);
        used += character_width;
    }
    result.push('…');
    result
}

fn pad(value: &str, width: usize, align: Align) -> String {
    let padding = width.saturating_sub(display_width(value));
    match align {
        Align::Left => format!("{value}{}", " ".repeat(padding)),
        Align::Right => format!("{}{value}", " ".repeat(padding)),
    }
}

fn resource_name(kind: TableKind) -> &'static str {
    match kind {
        TableKind::Transactions => "transactions",
        TableKind::Categories => "categories",
        TableKind::Tags => "tags",
        TableKind::PlaidAccounts => "Plaid accounts",
        TableKind::ManualAccounts => "manual accounts",
        TableKind::Recurring => "recurring items",
        TableKind::Summary => "budget categories",
        TableKind::Generic => "results",
    }
}

fn resource_name_singular(kind: TableKind) -> &'static str {
    match kind {
        TableKind::Transactions => "transaction",
        TableKind::Categories => "category",
        TableKind::Tags => "tag",
        TableKind::PlaidAccounts => "Plaid account",
        TableKind::ManualAccounts => "manual account",
        TableKind::Recurring => "recurring item",
        TableKind::Summary => "budget category",
        TableKind::Generic => "result",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parse_pairs() {
        assert_eq!(
            parse_key_value("a=b=c").unwrap(),
            ("a".into(), "b=c".into())
        );
        assert!(parse_key_value("nope").is_err());
    }
    #[test]
    fn json_must_be_object() {
        assert!(read_json("[]").is_err());
        assert!(read_json("{\"a\":1}").is_ok());
    }
    #[test]
    fn transaction_args_parse() {
        let cli = Cli::try_parse_from([
            "lunchmoney",
            "transactions",
            "list",
            "--limit",
            "20",
            "--all",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Commands::Transactions {
                command: Transactions::List(TransactionList {
                    all: true,
                    limit: 20,
                    ..
                })
            }
        ));
    }
    #[test]
    fn month_scope_uses_the_full_calendar_month() {
        assert_eq!(
            month_range("2024-02").unwrap(),
            ("2024-02-01".into(), "2024-02-29".into())
        );
        assert_eq!(
            month_range("2026-08").unwrap(),
            ("2026-08-01".into(), "2026-08-31".into())
        );
        assert!(month_range("2026-13").is_err());
    }
    #[test]
    fn month_scope_populates_transaction_dates() {
        let cli = Cli::try_parse_from(["lunchmoney", "transactions", "list", "--month", "2026-08"])
            .unwrap();
        let Commands::Transactions {
            command: Transactions::List(mut args),
        } = cli.command
        else {
            panic!("expected transaction list command");
        };
        let month = args.month.clone().unwrap();
        apply_month_scope(&mut args, &month).unwrap();
        let query = transaction_query(&args, 0);
        assert!(query.contains(&("start_date".into(), "2026-08-01".into())));
        assert!(query.contains(&("end_date".into(), "2026-08-31".into())));
    }
    #[test]
    fn pending_transactions_are_included_by_default() {
        let cli = Cli::try_parse_from(["lunchmoney", "transactions", "list"]).unwrap();
        let Commands::Transactions {
            command: Transactions::List(args),
        } = cli.command
        else {
            panic!("expected transaction list command");
        };
        assert!(transaction_query(&args, 0).contains(&("include_pending".into(), "true".into())));

        let cli = Cli::try_parse_from(["lunchmoney", "transactions", "list", "--exclude-pending"])
            .unwrap();
        let Commands::Transactions {
            command: Transactions::List(args),
        } = cli.command
        else {
            panic!("expected transaction list command");
        };
        assert!(
            !transaction_query(&args, 0)
                .iter()
                .any(|(key, _)| key == "include_pending")
        );
    }
    #[test]
    fn tables_include_headers() {
        let mut bytes = Vec::new();
        render_table(
            &mut bytes,
            &serde_json::json!({"items":[{"id": 1, "name": "Coffee"}]}),
            TableKind::Generic,
            &RenderContext::default(),
            false,
            false,
        )
        .unwrap();
        assert!(String::from_utf8(bytes).unwrap().contains("Items"));
    }

    #[test]
    fn collect_names_includes_nested_categories() {
        let categories = serde_json::json!({
            "categories": [{
                "id": 10,
                "name": "Lifestyle",
                "children": [{ "id": 14033, "name": "Shopping" }]
            }]
        });
        let mut names = HashMap::new();
        collect_names(&mut names, &categories, "categories");
        assert_eq!(names.get(&10), Some(&"Lifestyle".to_owned()));
        assert_eq!(names.get(&14033), Some(&"Shopping".to_owned()));
    }

    #[test]
    fn transaction_table_uses_readable_columns_and_truncates() {
        let mut bytes = Vec::new();
        render_table(
            &mut bytes,
            &serde_json::json!({"transactions":[{
                "id": 123,
                "date": "2026-08-31",
                "payee": "A very long merchant name that should be shortened",
                "category_id": 9,
                "plaid_account_id": 8,
                "amount": "1234.5",
                "currency": "cad"
            }]}),
            TableKind::Transactions,
            &RenderContext {
                categories: HashMap::from([(9, "Dining".into())]),
                accounts: HashMap::from([(8, "Visa".into())]),
                ..RenderContext::default()
            },
            false,
            false,
        )
        .unwrap();
        let table = String::from_utf8(bytes).unwrap();
        assert!(table.contains("Category"));
        assert!(table.contains("1,234.50 CAD"));
        assert!(!table.contains("category_id"));
    }

    #[test]
    fn transaction_tables_hide_ids_by_default_and_colorize_terminal_output() {
        assert!(
            !columns_for(TableKind::Transactions, false)
                .iter()
                .any(|column| column.key == "id")
        );
        assert_eq!(columns_for(TableKind::Transactions, true)[0].key, "id");

        let mut bytes = Vec::new();
        render_table(
            &mut bytes,
            &serde_json::json!({"transactions":[{
                "id": 123,
                "date": "2026-09-01",
                "payee": "Coffee Shop",
                "category_id": 9,
                "plaid_account_id": 8,
                "amount": "-4.50",
                "currency": "cad"
            }]}),
            TableKind::Transactions,
            &RenderContext {
                categories: HashMap::from([(9, "Coffee".into())]),
                accounts: HashMap::from([(8, "Visa".into())]),
                ..RenderContext::default()
            },
            false,
            true,
        )
        .unwrap();
        let table = String::from_utf8(bytes).unwrap();
        assert!(table.contains("\x1b[32m"));
        assert!(table.contains("\x1b[35m"));
        assert!(table.contains("\x1b[36m"));
    }

    #[test]
    fn payment_transfer_category_is_muted() {
        assert_eq!(
            transaction_color("category", "Payment, Transfer"),
            Some("\x1b[2m")
        );
    }

    #[test]
    fn payment_transfer_transaction_mutes_the_entire_row() {
        let columns = columns_for(TableKind::Transactions, false);
        let row = vec![
            "2026-09-01".into(),
            "Transfer".into(),
            "Payment, Transfer".into(),
            "Chequing".into(),
            "100.00 CAD".into(),
        ];
        assert!(transaction_is_payment_transfer(&row, &columns));
    }

    #[test]
    fn unicode_truncation_respects_display_width() {
        assert_eq!(display_width(&fit_cell("Coffee ☕ shop", 8)), 8);
    }

    #[test]
    fn clap_command_tree_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn api_errors_are_readable() {
        let message = format_api_error(
            StatusCode::BAD_REQUEST,
            r#"{"message":"Invalid Request","errors":[{"errMsg":"start_date is required"}]}"#,
        );
        assert_eq!(
            message,
            "Lunch Money API returned 400 Bad Request: Invalid Request — start_date is required"
        );
    }
}
