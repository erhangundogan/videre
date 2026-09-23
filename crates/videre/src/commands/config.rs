use crate::command_context::CommandContext;
use anyhow::{Context, Result};
use clap::builder::PossibleValuesParser;
use videre_core::library_config::{self, ConfigKey};

const CONFIG_KEYS: &[&str] = &[
    "model",
    "read-rate",
    "xmp",
    "export-xmp-on-watch",
    "watch-debounce-ms",
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
        "xmp" => ConfigKey::Xmp,
        "export-xmp-on-watch" => ConfigKey::ExportXmpOnWatch,
        "watch-debounce-ms" => ConfigKey::WatchDebounceMs,
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
        ConfigKey::ExportXmpOnWatch => {
            let on: bool = value.parse().map_err(|_| {
                anyhow::anyhow!("export-xmp-on-watch must be true or false, got {value:?}")
            })?;
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
    println!("xmp:           {}", xmp_name(config.xmp_precedence));
    println!(
        "export-xmp-on-watch: {}",
        if config.export_xmp_on_watch {
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
