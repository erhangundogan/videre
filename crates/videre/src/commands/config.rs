use anyhow::Result;
use clap::builder::PossibleValuesParser;
use std::path::PathBuf;
use videre_core::home;

const CONFIG_KEYS: &[&str] = &["db", "path"];

#[derive(clap::Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    action: Option<ConfigAction>,
}

#[derive(clap::Subcommand)]
enum ConfigAction {
    /// Set a config key (keys: db, path)
    Set {
        #[arg(value_parser = PossibleValuesParser::new(CONFIG_KEYS))]
        key: String,
        value: PathBuf,
    },
    /// Remove a config key (keys: db, path)
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
            "db" => home::set_default_db(&home, &value),
            "path" => home::set_default_path(&home, &value),
            _ => unreachable!("clap restricts keys to CONFIG_KEYS"),
        },
        Some(ConfigAction::Unset { key }) => match key.as_str() {
            "db" => home::unset_default_db(&home),
            "path" => home::unset_default_path(&home),
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
        if config_file.exists() { "" } else { " (absent)" }
    );
    // Display keys match the names `videre config set <key>` accepts, so the
    // output doubles as documentation for how to change each value.
    match &config.default_db {
        Some(db) => println!("db:            {} [from config.toml]", db.display()),
        None => println!("db:            (not set) [set with: videre config set db <path>]"),
    }
    println!("resolved db:   {}", home::resolve_db_in(home)?.display());
    match &config.default_path {
        Some(dir) => println!("resolved path: {} [from config.toml]", dir.display()),
        None => {
            println!("resolved path: (not set) [set with: videre config set path <path>]")
        }
    }
    println!("jsonl:         {}", home.join("hashes.jsonl").display());
    Ok(())
}
