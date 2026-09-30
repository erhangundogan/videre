use crate::command_context::CommandContext;
use anyhow::{Context, Result};
use clap::builder::PossibleValuesParser;
use videre_core::library_config::{self, ConfigKey};

const CONFIG_KEYS: &[&str] = &[
    "model",
    "read-rate",
    "io-workers",
    "xmp",
    "export-xmp-on-watch",
    "gallery-starts-watch",
    "watch-debounce-ms",
    "watch-bulk-threshold",
    "watch-bulk-quiet-ms",
    "log-level",
    "log-format",
    "log-max-size-mb",
    "log-keep",
    "log-max-age-days",
];

#[derive(clap::Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    action: Option<ConfigAction>,
}

#[derive(clap::Subcommand)]
enum ConfigAction {
    /// Set a library config key
    Set {
        #[arg(value_parser = PossibleValuesParser::new(CONFIG_KEYS))]
        key: String,
        value: String,
    },
    /// Remove a library config key
    Unset {
        #[arg(value_parser = PossibleValuesParser::new(CONFIG_KEYS))]
        key: String,
    },
}

pub fn run(args: ConfigArgs, ctx: &CommandContext) -> Result<()> {
    match args.action {
        None => show(ctx),
        Some(ConfigAction::Set { key, value }) => {
            let (key, value) = config_value(&key, value)?;
            library_config::edit(&ctx.library, key, Some(value))
        }
        Some(ConfigAction::Unset { key }) => {
            library_config::edit(&ctx.library, config_key(&key), None)
        }
    }
}

fn config_key(key: &str) -> ConfigKey {
    match key {
        "model" => ConfigKey::Model,
        "read-rate" => ConfigKey::ReadRate,
        "io-workers" => ConfigKey::IoWorkers,
        "xmp" => ConfigKey::Xmp,
        "export-xmp-on-watch" => ConfigKey::ExportXmpOnWatch,
        "gallery-starts-watch" => ConfigKey::GalleryStartsWatch,
        "watch-debounce-ms" => ConfigKey::WatchDebounceMs,
        "watch-bulk-threshold" => ConfigKey::WatchBulkThreshold,
        "watch-bulk-quiet-ms" => ConfigKey::WatchBulkQuietMs,
        "log-level" => ConfigKey::LogLevel,
        "log-format" => ConfigKey::LogFormat,
        "log-max-size-mb" => ConfigKey::LogMaxSizeMb,
        "log-keep" => ConfigKey::LogKeep,
        "log-max-age-days" => ConfigKey::LogMaxAgeDays,
        _ => unreachable!("clap restricts keys to CONFIG_KEYS"),
    }
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
        ConfigKey::WatchBulkThreshold => whole_number(name, &value, "files")?,
        ConfigKey::WatchBulkQuietMs => whole_number(name, &value, "milliseconds")?,
        ConfigKey::ExportXmpOnWatch | ConfigKey::GalleryStartsWatch => {
            let on: bool = value
                .parse()
                .map_err(|_| anyhow::anyhow!("{name} must be true or false, got {value:?}"))?;
            toml::Value::Boolean(on)
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
    Ok(())
}

fn xmp_name(precedence: videre_core::marks::XmpPrecedence) -> &'static str {
    match precedence {
        videre_core::marks::XmpPrecedence::Db => "db",
        videre_core::marks::XmpPrecedence::File => "file",
        videre_core::marks::XmpPrecedence::Newest => "newest",
    }
}
