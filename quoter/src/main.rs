mod engine;
mod protocols;
mod reader;
mod route;
mod session;
mod writer;

use std::path::PathBuf;

use clap::{Parser, Subcommand};

const DEFAULT_TIERS: &[f64] = &[
    0.1, 1.0, 5.0, 10.0, 50.0, 100.0, 500.0, 1000.0, 5000.0, 10000.0, 100000.0,
];

#[derive(Parser)]
#[command(
    name = "quoter",
    version,
    about = "Offline quote replay for Solana AMM sessions"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Quote {
        #[arg(long)]
        session: PathBuf,
        #[arg(long, value_delimiter = ',')]
        tiers: Option<Vec<f64>>,
        #[arg(long)]
        output: Option<PathBuf>,
    },

    RouteAnalysis {
        #[arg(long)]
        session: PathBuf,

        #[arg(long)]
        output: Option<PathBuf>,
    },
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Quote {
            session,
            tiers,
            output,
        } => run_quote(&session, tiers.as_deref(), output.as_deref()),
        Commands::RouteAnalysis { session, output } => {
            run_route_analysis(&session, output.as_deref())
        }
    }
}

fn run_route_analysis(
    session_dir: &std::path::Path,
    output_path: Option<&std::path::Path>,
) -> anyhow::Result<()> {
    let metadata = session::SessionMetadata::load(session_dir)?;
    tracing::info!(session = %metadata.session_id, pools = metadata.pools.len(), "loaded session");

    let rows = route::run_route_analysis(session_dir, &metadata)?;

    let csv_path = match output_path {
        Some(p) => p.to_path_buf(),
        None => {
            let dir = session_dir.join("analysis");
            std::fs::create_dir_all(&dir)?;
            dir.join("route_analysis.csv")
        }
    };

    route::write_route_csv(&rows, &csv_path)?;
    tracing::info!(path = %csv_path.display(), rows = rows.len(), "route analysis CSV written");
    Ok(())
}

fn run_quote(
    session_dir: &std::path::Path,
    tiers: Option<&[f64]>,
    output_dir: Option<&std::path::Path>,
) -> anyhow::Result<()> {
    let metadata = session::SessionMetadata::load(session_dir)?;
    tracing::info!(
        session_id = %metadata.session_id,
        pools = metadata.pools.len(),
        "loaded session metadata"
    );

    let tiers_usd: Vec<f64> = if let Some(t) = tiers {
        t.to_vec()
    } else if let Some(ref t) = metadata.quote_tiers_usd {
        t.clone()
    } else {
        DEFAULT_TIERS.to_vec()
    };
    tracing::info!(tiers = ?tiers_usd, "using USD tiers");

    let output_base = match output_dir {
        Some(p) => p.to_path_buf(),
        None => session_dir.join("quoter_output"),
    };
    std::fs::create_dir_all(&output_base)?;

    let results = engine::process_session(session_dir, &metadata, &tiers_usd);

    let mut total_quotes = 0usize;
    let mut total_derived = 0usize;
    let mut total_updates = 0usize;
    let mut pool_errors = 0usize;

    let mut pair_quotes: std::collections::HashMap<String, Vec<engine::EngineQuoteRow>> =
        std::collections::HashMap::new();
    let mut pair_derived: std::collections::HashMap<String, Vec<engine::EngineDerivedRow>> =
        std::collections::HashMap::new();

    for result in results {
        match result {
            Ok(pool_result) => {
                let pair_key = pool_result
                    .symbol
                    .as_deref()
                    .map(|s| s.replace('/', "-"))
                    .unwrap_or_else(|| {
                        pool_result.pool_id[..12.min(pool_result.pool_id.len())].to_string()
                    });

                tracing::info!(
                    pool_id = %pool_result.pool_id,
                    amm_type = %pool_result.amm_type,
                    updates = pool_result.updates_processed,
                    quotes = pool_result.quotes_emitted,
                    derived = pool_result.derived.len(),
                    "pool processed"
                );

                total_updates += pool_result.updates_processed;
                total_quotes += pool_result.quotes_emitted;
                total_derived += pool_result.derived.len();

                pair_quotes
                    .entry(pair_key.clone())
                    .or_default()
                    .extend(pool_result.quotes);
                pair_derived
                    .entry(pair_key)
                    .or_default()
                    .extend(pool_result.derived);
            }
            Err(e) => {
                tracing::error!(error = %e, "pool processing failed");
                pool_errors += 1;
            }
        }
    }

    for (pair, quotes) in &pair_quotes {
        let pair_dir = output_base.join(pair);
        let qf = writer::write_quotes(quotes, &pair_dir)?;
        let derived = pair_derived
            .get(pair.as_str())
            .map(|v| v.as_slice())
            .unwrap_or(&[]);
        let df = writer::write_derived_state(derived, &pair_dir)?;
        tracing::info!(
            pair = %pair,
            quotes = quotes.len(),
            derived = derived.len(),
            quote_files = qf.len(),
            derived_files = df.len(),
            "pair output written"
        );
    }

    tracing::info!(
        total_updates,
        total_quotes,
        total_derived,
        pool_errors,
        "quoter run complete"
    );

    if pool_errors > 0 {
        tracing::warn!(errors = pool_errors, "some pools had errors");
    }

    Ok(())
}
