use crate::command_context::CommandContext;
use anyhow::{Context, Result};
use clap::builder::PossibleValuesParser;
use videre_core::library_config::{self, ConfigKey};

/// Every key `videre config` sets, from the one key table in
/// `library_config`.
pub(crate) fn config_keys() -> Vec<&'static str> {
    library_config::KEYS.iter().map(|k| k.cli).collect()
}

#[derive(clap::Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    action: Option<ConfigAction>,
}

#[derive(clap::Subcommand)]
enum ConfigAction {
    /// Set a library config key
    Set {
        #[arg(value_parser = PossibleValuesParser::new(config_keys()))]
        key: String,
        #[arg(add = clap_complete::engine::ArgValueCompleter::new(
            crate::completions::config_value_candidates
        ))]
        value: String,
    },
    /// Remove a library config key
    Unset {
        #[arg(value_parser = PossibleValuesParser::new(config_keys()))]
        key: String,
    },
}

pub fn run(args: ConfigArgs, ctx: &CommandContext) -> Result<()> {
    match args.action {
        None => show(ctx),
        Some(ConfigAction::Set { key, value }) => {
            let (key, value) = config_value(&key, value)?;
            let off = key == ConfigKey::StreetDetail && value == toml::Value::Boolean(false);
            library_config::edit(&ctx.library, key, Some(value))?;
            if off {
                forget_street_detail(ctx)?;
            }
            Ok(())
        }
        Some(ConfigAction::Unset { key }) => {
            let key = config_key(&key);
            library_config::edit(&ctx.library, key, None)?;
            if key == ConfigKey::StreetDetail {
                forget_street_detail(ctx)?;
            }
            Ok(())
        }
    }
}

/// Street detail off means no street map on disk: the download was the
/// consent's only product, so withdrawing the consent deletes it.
fn forget_street_detail(ctx: &CommandContext) -> Result<()> {
    if let Some(bytes) = videre_core::basemap_detail::remove(&ctx.library.paths.state)? {
        eprintln!(
            "street-detail off: deleted the downloaded street map ({:.0} MB)",
            bytes as f64 / 1e6
        );
    }
    Ok(())
}

fn config_key(key: &str) -> ConfigKey {
    library_config::spec_for(key)
        .expect("clap restricts keys to config_keys")
        .key
}

/// Parse a whole-number CLI value; `unit` names what the number counts.
fn whole_number(key: &str, value: &str, unit: &str) -> Result<toml::Value> {
    let n: u64 = value
        .parse()
        .map_err(|_| anyhow::anyhow!("{key} must be a whole number of {unit}, got {value:?}"))?;
    let n = i64::try_from(n).with_context(|| format!("{key} is too large"))?;
    Ok(toml::Value::Integer(n))
}

fn config_value(key: &str, value: String) -> Result<(ConfigKey, toml::Value)> {
    let name = key;
    let key = config_key(key);
    let value = match key {
        ConfigKey::Model | ConfigKey::Xmp | ConfigKey::LogLevel | ConfigKey::LogFormat => {
            toml::Value::String(value)
        }
        ConfigKey::LogMaxSizeMb => whole_number(name, &value, "megabytes")?,
        ConfigKey::LogKeep => whole_number(name, &value, "files")?,
        ConfigKey::LogMaxAgeDays => whole_number(name, &value, "days")?,
        ConfigKey::ReadRate => {
            let mb_s: u64 = value.parse().map_err(|_| {
                anyhow::anyhow!("read-rate must be a whole number of MB/s, got {value:?}")
            })?;
            let mb_s = i64::try_from(mb_s).context("read-rate is too large")?;
            toml::Value::Integer(mb_s)
        }
        ConfigKey::IoWorkers => whole_number(name, &value, "workers")?,
        ConfigKey::RunHistory => whole_number(name, &value, "runs")?,
        ConfigKey::WatchBulkThreshold => whole_number(name, &value, "files")?,
        ConfigKey::WatchBulkQuietMs => whole_number(name, &value, "milliseconds")?,
        ConfigKey::ExportXmpOnWatch | ConfigKey::GalleryStartsWatch | ConfigKey::StreetDetail => {
            let on: bool = value
                .parse()
                .map_err(|_| anyhow::anyhow!("{name} must be true or false, got {value:?}"))?;
            toml::Value::Boolean(on)
        }
        ConfigKey::SearchMinMatch | ConfigKey::SimilarMinScore => {
            let n: f64 = value
                .parse()
                .map_err(|_| anyhow::anyhow!("{name} must be a number, got {value:?}"))?;
            toml::Value::Float(n)
        }
        ConfigKey::WatchDebounceMs => {
            let ms: u64 = value.parse().map_err(|_| {
                anyhow::anyhow!(
                    "watch-debounce-ms must be a whole number of milliseconds, got {value:?}"
                )
            })?;
            let ms = i64::try_from(ms).context("watch-debounce-ms is too large")?;
            toml::Value::Integer(ms)
        }
    };
    Ok((key, value))
}

fn show(ctx: &CommandContext) -> Result<()> {
    let paths = &ctx.library.paths;
    let config = &ctx.library.settings;
    println!("library:       {}", paths.root.display());
    println!("selected by:   {}", ctx.source.label());
    println!("state:         {}", paths.state.display());
    println!(
        "config:        {}{}",
        paths.config.display(),
        if library_config::exists(paths)? {
            ""
        } else {
            " (absent)"
        }
    );
    println!("db:            {}", paths.db.display());
    println!("jsonl:         {}", paths.jsonl.display());
    println!("model:         {}", config.default_model);
    match config.min_read_rate_mb_s {
        Some(rate) => println!("read-rate:     {rate} MB/s"),
        None => println!(
            "read-rate:     {} MB/s (default)",
            videre_core::io_timeout::MIN_READ_RATE_MB_S_DEFAULT
        ),
    }
    match config.max_io_workers {
        Some(max) => println!("io-workers:    {max}"),
        None => println!(
            "io-workers:    {} (default)",
            videre_core::io_timeout::worker_stats().maximum
        ),
    }
    println!("xmp:           {}", xmp_name(config.xmp_precedence));
    println!(
        "export-xmp-on-watch: {}",
        if config.export_xmp_on_watch {
            "on"
        } else {
            "off"
        }
    );
    println!(
        "gallery-starts-watch: {}",
        if config.gallery_starts_watch {
            "on"
        } else {
            "off"
        }
    );
    println!(
        "street-detail: {}",
        if config.street_detail { "on" } else { "off" }
    );
    match config.watch_debounce_ms {
        Some(ms) => println!("watch-debounce-ms: {ms} ms"),
        None => println!(
            "watch-debounce-ms: {} ms (default)",
            videre_core::library_config::WATCH_DEBOUNCE_MS_DEFAULT
        ),
    }
    println!(
        "watch-bulk-threshold: {} files{}",
        config
            .watch_bulk_threshold
            .unwrap_or(library_config::WATCH_BULK_THRESHOLD_DEFAULT),
        if config.watch_bulk_threshold.is_some() {
            ""
        } else {
            " (default)"
        }
    );
    println!(
        "watch-bulk-quiet-ms: {} ms{}",
        config
            .watch_bulk_quiet_ms
            .unwrap_or(library_config::WATCH_BULK_QUIET_MS_DEFAULT),
        if config.watch_bulk_quiet_ms.is_some() {
            ""
        } else {
            " (default)"
        }
    );
    println!("log-level:     {}", config.log_level.as_str());
    println!("log-format:    {}", config.log_format.as_str());
    println!("log-max-size-mb: {} MB", config.log_max_size_mb);
    println!("log-keep:      {}", config.log_keep);
    println!("log-max-age-days: {} days", config.log_max_age_days);
    println!(
        "search-min-match: {}{}",
        config.search_min_match,
        if config.search_min_match == library_config::SEARCH_MIN_MATCH_DEFAULT {
            " (default)"
        } else {
            ""
        }
    );
    match config.similar_min_score {
        Some(score) => println!("similar-min-score: {score}"),
        None => println!("similar-min-score: none (default)"),
    }
    println!(
        "run-history:   {} run(s) per command{}",
        config.run_history,
        if config.run_history == library_config::RUN_HISTORY_DEFAULT {
            " (default)"
        } else {
            ""
        }
    );
    Ok(())
}

fn xmp_name(precedence: videre_core::marks::XmpPrecedence) -> &'static str {
    precedence.as_str()
}
