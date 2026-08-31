use anyhow::Result;
use clap::builder::PossibleValuesParser;
use videre_core::home;

const CONFIG_KEYS: &[&str] = &[
    "db",
    "path",
    "model",
    "read-rate",
    "xmp",
    "export-xmp-on-watch",
];

#[derive(clap::Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    action: Option<ConfigAction>,
}

#[derive(clap::Subcommand)]
enum ConfigAction {
    /// Set a config key (keys: db, path, model)
    Set {
        #[arg(value_parser = PossibleValuesParser::new(CONFIG_KEYS))]
        key: String,
        value: String,
    },
    /// Remove a config key (keys: db, path, model)
    Unset {
        #[arg(value_parser = PossibleValuesParser::new(CONFIG_KEYS))]
        key: String,
    },
}

pub fn run(args: ConfigArgs) -> Result<()> {
    let home = home::videre_home()?;
    match args.action {
        None => show(&home),
        Some(ConfigAction::Set { key, value }) => match key.as_str() {
            "db" => home::set_default_db(&home, std::path::Path::new(&value)),
            "path" => home::set_default_path(&home, std::path::Path::new(&value)),
            "read-rate" => {
                let mb_s: u64 = value.parse().map_err(|_| {
                    anyhow::anyhow!("read-rate must be a whole number of MB/s, got {value:?}")
                })?;
                home::set_min_read_rate(&home, mb_s)
            }
            "model" => {
                videre_core::embeddings::validate_model_id(&value)?;
                home::set_default_model(&home, &value)
            }
            "xmp" => home::set_xmp_precedence(&home, &value),
            "export-xmp-on-watch" => {
                let on: bool = value.parse().map_err(|_| {
                    anyhow::anyhow!("export-xmp-on-watch must be true or false, got {value:?}")
                })?;
                home::set_export_xmp_on_watch(&home, on)
            }
            _ => unreachable!("clap restricts keys to CONFIG_KEYS"),
        },
        Some(ConfigAction::Unset { key }) => match key.as_str() {
            "db" => home::unset_default_db(&home),
            "path" => home::unset_default_path(&home),
            "read-rate" => home::unset_min_read_rate(&home),
            "model" => home::unset_default_model(&home),
            "xmp" => home::unset_xmp_precedence(&home),
            "export-xmp-on-watch" => home::unset_export_xmp_on_watch(&home),
            _ => unreachable!("clap restricts keys to CONFIG_KEYS"),
        },
    }
}

fn show(home: &std::path::Path) -> Result<()> {
    let config_file = home::config_path(home);
    let config = home::load_config(home)?;
    println!("home:          {}", home.display());
    println!(
        "config:        {}{}",
        config_file.display(),
        if config_file.exists() {
            ""
        } else {
            " (absent)"
        }
    );
    // Display keys match the names `videre config set <key>` accepts, so the
    // output doubles as documentation for how to change each value.
    match &config.default_db {
        Some(db) => println!("db:            {} [from config.toml]", db.display()),
        None => println!("db:            (not set) [set with: videre config set db <path>]"),
    }
    // The resolved value must come from `resolve_db`, the VIDERE_HOME-aware
    // function every command opens the database through. Resolving it from
    // config alone once printed a path no command would open, so with an
    // explicit home whose config.toml named a different database, `config`
    // showed one path while commands failed with "no database found" against
    // another.
    println!("resolved db:   {}", home::resolve_db(None)?.display());
    match &config.default_path {
        Some(dir) => println!("resolved path: {} [from config.toml]", dir.display()),
        None => {
            println!("resolved path: (not set) [set with: videre config set path <path>]")
        }
    }
    // Show the resolved value even when unset: the question being asked is
    // "what will videre use", not "what did I type".
    match &config.default_model {
        Some(m) => println!("model:         {m} [from config.toml]"),
        None => println!(
            "model:         {} (default) [set with: videre config set model <id>]",
            videre_core::embeddings::DEFAULT_MODEL_ID
        ),
    }
    match &config.xmp_precedence {
        Some(p) => println!("xmp:           {p} [from config.toml]"),
        None => println!(
            "xmp:           db (default) [set with: videre config set xmp <db|file|newest>]"
        ),
    }
    match config.export_xmp_on_watch {
        Some(true) => println!("export-xmp-on-watch: on [from config.toml]"),
        _ => println!(
            "export-xmp-on-watch: off (default) [set with: videre config set export-xmp-on-watch <true|false>]"
        ),
    }
    println!("jsonl:         {}", home.join("hashes.jsonl").display());
    Ok(())
}
