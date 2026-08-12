use anyhow::Result;
use clap::builder::PossibleValuesParser;
use videre_core::home;

const CONFIG_KEYS: &[&str] = &["db", "path", "model", "read-rate"];

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

/// Reject anything that is not `owner/name`.
///
/// Not cosmetic: `videre_ml::model::Embedder::load` does
/// `split_once('/').expect("model id is owner/name")`, so a bare model name
/// with the owner omitted panics at load time. Validating here means that
/// panic is unreachable from configuration.
///
/// Validation stops at shape. A well-formed id for a model that has never been
/// embedded is legitimate: setting the default before running
/// `videre embed --model` on it is a reasonable order of operations, and the
/// readers already error clearly, naming the models that do exist.
fn validate_model_id(id: &str) -> Result<()> {
    match id.split_once('/') {
        Some((owner, name)) if !owner.is_empty() && !name.is_empty() && !name.contains('/') => {
            Ok(())
        }
        _ => anyhow::bail!(
            "invalid model id {id:?}: expected owner/name, \
             e.g. google/siglip-base-patch16-224"
        ),
    }
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
                validate_model_id(&value)?;
                home::set_default_model(&home, &value)
            }
            _ => unreachable!("clap restricts keys to CONFIG_KEYS"),
        },
        Some(ConfigAction::Unset { key }) => match key.as_str() {
            "db" => home::unset_default_db(&home),
            "path" => home::unset_default_path(&home),
            "read-rate" => home::unset_min_read_rate(&home),
            "model" => home::unset_default_model(&home),
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
    println!("resolved db:   {}", home::resolve_db_in(home)?.display());
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
    println!("jsonl:         {}", home.join("hashes.jsonl").display());
    Ok(())
}
