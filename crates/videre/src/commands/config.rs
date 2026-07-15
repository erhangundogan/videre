use anyhow::Result;
use clap::builder::PossibleValuesParser;
use std::path::PathBuf;
use videre_core::home;

const CONFIG_KEYS: &[&str] = &["db"];

#[derive(clap::Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    action: Option<ConfigAction>,
}

#[derive(clap::Subcommand)]
enum ConfigAction {
    /// Set a config key (keys: db)
    Set {
        #[arg(value_parser = PossibleValuesParser::new(CONFIG_KEYS))]
        key: String,
        value: PathBuf,
    },
    /// Remove a config key (keys: db)
    Unset {
        #[arg(value_parser = PossibleValuesParser::new(CONFIG_KEYS))]
        key: String,
    },
}

pub fn run(args: ConfigArgs) -> Result<()> {
    let home = home::videre_home()?;
    match args.action {
        None => show(&home),
        Some(ConfigAction::Set { value, .. }) => home::set_default_db(&home, &value),
        Some(ConfigAction::Unset { .. }) => home::unset_default_db(&home),
    }
}

fn show(home: &std::path::Path) -> Result<()> {
    let config_file = home::config_path(home);
    let config = home::load_config(home)?;
    println!("home:        {}", home.display());
    println!(
        "config:      {}{}",
        config_file.display(),
        if config_file.exists() { "" } else { " (absent)" }
    );
    match &config.default_db {
        Some(db) => println!("default_db:  {}", db.display()),
        None => println!("default_db:  (not set)"),
    }
    println!("resolved db: {}", home::resolve_db_in(home)?.display());
    println!("jsonl:       {}", home.join("hashes.jsonl").display());
    Ok(())
}
