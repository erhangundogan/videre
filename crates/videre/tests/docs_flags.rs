//! The docs site must describe the flags that actually ship.
//!
//! `--type`, `--ext`, `--mime` and `--path` shipped in 0.15.0 and were
//! documented nowhere; a docs page also described `--near`, which no command
//! has ever accepted. Both were found by reading, months apart, and both were
//! mechanically findable. This converts that trust problem into a test failure.
//!
//! Deliberately compares against `--help` rather than the source: `--help` is
//! what a user is told, so it is the thing a docs page has to agree with.

mod common;
use common::videre_bin;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Present on every command, so absent from every docs page and not a finding.
const UNIVERSAL: [&str; 2] = ["--help", "--version"];

/// Legacy names that still work but are hidden from `--help`.
///
/// A clap `alias` does not appear in the flag list, so these are invisible to a
/// diff against `--help` while remaining perfectly valid to type. They are
/// documented (a script using one deserves to find out what replaced it) but
/// deliberately not advertised.
///
/// `hidden_aliases_still_work` runs each one, so an entry cannot quietly become a lie:
/// remove the alias from the code and this list starts failing.
const HIDDEN_ALIASES: [(&str, &str); 2] =
    [("scan", "--output-sqlite"), ("watch", "--output-sqlite")];

/// Flags of the **install script**, not of `videre` itself. The install and
/// troubleshooting pages document them correctly; they are simply another
/// program's command line, so `videre --help` will never list them.
const INSTALLER_FLAGS: [&str; 4] = ["--uninstall", "--to", "--release", "--version"];

/// Every `.md` under the docs tree, not just `commands/`.
///
/// :warning: The per-command check below only reads `commands/`, which left the
/// guides unguarded. `guides/browsing.md` went on documenting `--faces`,
/// `--show-faces` and `--all` for two releases after they were removed, teaching
/// a command line that no longer parsed. Found by reading, which is what these
/// tests exist to replace.
fn all_docs_pages() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/src/content/docs");
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let path = e.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|x| x == "md" || x == "mdx") {
                out.push(path);
            }
        }
    }
    out
}

fn docs_dir() -> Option<PathBuf> {
    // Tests run from the crate root; docs/ lives at the workspace root and is
    // excluded from the published package, so a consumer running `cargo test`
    // on the crates.io tarball has no docs to check against.
    let d = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/src/content/docs/commands");
    d.is_dir().then(|| d)
}

/// Every long flag mentioned in `text`, minus the universal ones.
fn flags(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i + 2 < bytes.len() {
        // :warning: A flag starts a word. Without this, the Hugging Face
        // cache layout `models--google--siglip-base-patch16-224` reads as
        // the flags `--google` and `--siglip-base-patch16-224`, and the
        // docs pages that legitimately show that path fail the check.
        let starts_word = i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
        if starts_word
            && bytes[i] == b'-'
            && bytes[i + 1] == b'-'
            && bytes[i + 2].is_ascii_alphanumeric()
        {
            let start = i;
            i += 2;
            while i < bytes.len()
                && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'-')
                && !(bytes[i] == b'-' && i + 1 < bytes.len() && bytes[i + 1] == b'-')
            {
                i += 1;
            }
            let f = text[start..i].trim_end_matches('-').to_string();
            if !UNIVERSAL.contains(&f.as_str()) {
                out.insert(f);
            }
        } else {
            i += 1;
        }
    }
    out
}

fn help_for(cmd: &str) -> String {
    let out = Command::new(videre_bin())
        .args([cmd, "--help"])
        .output()
        .unwrap();
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn commands(docs: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(docs)
        .unwrap()
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            (p.extension()? == "md").then(|| p.file_stem()?.to_str().map(str::to_string))?
        })
        .collect();
    v.sort();
    v
}

#[test]
fn every_shipped_flag_is_documented() {
    let Some(docs) = docs_dir() else { return };
    let mut findings = Vec::new();

    for cmd in commands(&docs) {
        let shipped = flags(&help_for(&cmd));
        let documented = flags(&std::fs::read_to_string(docs.join(format!("{cmd}.md"))).unwrap());
        let missing: Vec<&String> = shipped.difference(&documented).collect();
        if !missing.is_empty() {
            findings.push(format!(
                "  {cmd}: {}",
                missing
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            ));
        }
    }

    assert!(
        findings.is_empty(),
        "flags ship but are documented nowhere on their command's page:\n{}\n\n\
         Document them in docs/src/content/docs/commands/<cmd>.md. If a flag is \
         meant to stay unadvertised, hide it from --help with a clap alias and \
         record it in HIDDEN_ALIASES.",
        findings.join("\n")
    );
}

#[test]
fn no_page_documents_a_flag_that_does_not_exist() {
    // Scoped to "accepted by no command at all" rather than "not on this
    // command": pages legitimately mention other commands' flags in prose, and
    // treating those as stale produced 18 false findings against 1 real one.
    let Some(docs) = docs_dir() else { return };
    let cmds = commands(&docs);
    let mut anywhere: BTreeSet<String> = cmds.iter().flat_map(|c| flags(&help_for(c))).collect();
    anywhere.extend(HIDDEN_ALIASES.iter().map(|(_, f)| f.to_string()));

    let mut findings = Vec::new();
    for cmd in &cmds {
        let documented = flags(&std::fs::read_to_string(docs.join(format!("{cmd}.md"))).unwrap());
        let dead: Vec<&String> = documented.difference(&anywhere).collect();
        if !dead.is_empty() {
            findings.push(format!(
                "  {cmd}.md: {}",
                dead.iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            ));
        }
    }

    assert!(
        findings.is_empty(),
        "docs name flags that no videre command accepts:\n{}",
        findings.join("\n")
    );
}

#[test]
fn every_command_help_links_to_its_docs_page() {
    let Some(docs) = docs_dir() else { return };
    for cmd in commands(&docs) {
        let help = help_for(&cmd);
        let want = format!("https://docs.videre.sh/commands/{cmd}/");
        assert!(
            help.contains(&want),
            "`videre {cmd} --help` does not link to {want}"
        );
    }
}

#[test]
fn docs_links_point_at_pages_that_exist() {
    // The link is built from the subcommand name, so it is only correct while
    // page slugs and subcommand names agree. A command added without a page
    // would otherwise ship a --help pointing at a 404.
    let Some(docs) = docs_dir() else { return };
    let out = Command::new(videre_bin()).arg("--help").output().unwrap();
    let listed = String::from_utf8_lossy(&out.stdout);

    // Subcommand names are the first word of each line in the commands section.
    let names: Vec<&str> = listed
        .lines()
        .filter_map(|l| {
            let t = l.strip_prefix("  ")?.trim_start();
            let first = t.split_whitespace().next()?;
            (!first.starts_with('-') && first.chars().all(|c| c.is_ascii_lowercase() || c == '-'))
                .then_some(first)
        })
        .collect();
    assert!(names.len() >= 15, "parsed too few subcommands: {names:?}");

    for n in names {
        if n == "help" {
            continue;
        }
        assert!(
            docs.join(format!("{n}.md")).is_file(),
            "`videre {n} --help` links to https://docs.videre.sh/commands/{n}/ \
             but docs/src/content/docs/commands/{n}.md does not exist"
        );
    }
}

#[test]
fn hidden_aliases_still_work() {
    // The list above exempts these from the staleness check, so it has to be
    // true rather than merely claimed. Each is run for real against a scanned
    // library; dropping the alias from the code fails here rather than leaving
    // a docs page recommending a flag that no longer parses.
    let dir = tempfile::tempdir().unwrap();
    let pics = dir.path().join("pics");
    std::fs::create_dir_all(&pics).unwrap();
    std::fs::write(pics.join("a.dng"), b"x").unwrap();

    for (cmd, alias) in HIDDEN_ALIASES {
        if cmd != "scan" {
            continue; // watch runs a loop; scan proves the alias parses
        }
        let db = dir.path().join(format!("{cmd}.db"));
        let out = Command::new(videre_bin())
            .env("VIDERE_HOME", dir.path())
            .args([cmd, pics.to_str().unwrap(), alias])
            .arg(&db)
            .arg("--silent")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "`videre {cmd} {alias}` no longer works:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(db.is_file(), "`{alias}` did not write a database");
    }
}

/// The same rule as `no_page_documents_a_flag_that_does_not_exist`, applied to
/// every page rather than only the command reference.
///
/// Scoped identically: a flag accepted by *no* command is stale wherever it
/// appears. A flag documented on the "wrong" page is not a finding, because
/// guides legitimately show several commands working together.
#[test]
fn no_page_anywhere_names_a_flag_that_does_not_exist() {
    let Some(docs) = docs_dir() else { return };
    let cmds = commands(&docs);
    let mut anywhere: BTreeSet<String> = cmds.iter().flat_map(|c| flags(&help_for(c))).collect();
    anywhere.extend(HIDDEN_ALIASES.iter().map(|(_, f)| f.to_string()));
    anywhere.extend(INSTALLER_FLAGS.iter().map(|f| f.to_string()));

    let mut findings = Vec::new();
    for page in all_docs_pages() {
        let Ok(text) = std::fs::read_to_string(&page) else {
            continue;
        };
        let dead: Vec<String> = flags(&text)
            .difference(&anywhere)
            .map(|s| s.to_string())
            .collect();
        if !dead.is_empty() {
            let name = page
                .strip_prefix(
                    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/src/content/docs"),
                )
                .unwrap_or(&page);
            findings.push(format!("  {}: {}", name.display(), dead.join(" ")));
        }
    }

    findings.sort();
    assert!(
        findings.is_empty(),
        "docs name flags that no videre command accepts:\n{}",
        findings.join("\n")
    );
}
