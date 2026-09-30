//! The query language: one Lucene-style string, as in Gmail and GitHub search,
//! that says which files to work on and, optionally, what to rank them by.
//!
//! ```text
//! person:özgür "gün batımı" -tag:screenshot (tag:deniz OR tag:plaj) rating:>=4
//! ```
//!
//! Parsing is `tantivy-query-grammar`'s; only its syntax tree is used, never a
//! tantivy index. Every `key:value` term becomes a one-field
//! [`RowSelection`], so a query filters with exactly the predicates the CLI
//! flags use; AND intersects their sets, OR unions them, NOT subtracts from
//! the library. Every term without a key, bare or quoted, joins the semantic
//! text, and only at the top level: text ranks rather than filters, so "text
//! OR tag" would have no meaning.

use std::path::PathBuf;

use tantivy_query_grammar::{
    parse_query, parse_query_lenient, Delimiter, Occur, UserInputAst, UserInputBound, UserInputLeaf,
};
use videre_core::selection::{MediaKind, PresenceField, RowSelection};

/// A boolean filter over one-field selections.
#[derive(Debug, Clone)]
pub enum Expr {
    And(Vec<Expr>),
    Or(Vec<Expr>),
    Not(Box<Expr>),
    Leaf(Box<RowSelection>),
}

/// A compiled query: its filter, and its text for ranking. Either may be
/// absent; an empty query has neither.
#[derive(Debug, Clone, Default)]
pub struct Compiled {
    pub filter: Option<Expr>,
    pub text: Option<String>,
}

/// Why a query was refused, and where, as a character offset, when the
/// parser knows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryError {
    pub message: String,
    pub at: Option<usize>,
}

impl std::fmt::Display for QueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.at {
            Some(at) => write!(f, "{} (at character {})", self.message, at + 1),
            None => write!(f, "{}", self.message),
        }
    }
}

impl std::error::Error for QueryError {}

/// The keys a query understands, in the order error messages list them.
pub const KEYS: &[&str] = &[
    "person", "tag", "category", "place", "date", "after", "before", "rating", "pick", "label",
    "is", "type", "ext", "mime", "path", "has", "missing",
];

/// Compile `query` into a filter and a ranking text.
pub fn compile(query: &str) -> Result<Compiled, QueryError> {
    if query.trim().is_empty() {
        return Ok(Compiled::default());
    }
    // The lenient parser is the one that knows where a mistake is; the strict
    // one is used only once the lenient one found nothing wrong.
    let (_, errors) = parse_query_lenient(query);
    if let Some(first) = errors.first() {
        return Err(QueryError {
            message: first.message.clone(),
            at: Some(first.pos),
        });
    }
    let ast = parse_query(query).map_err(|_| refused("the query could not be parsed"))?;
    let mut text: Vec<String> = Vec::new();
    let filter = match ast {
        UserInputAst::Clause(items) => clause(items, Some(&mut text))?,
        other => {
            if let Some(words) = as_text(&other) {
                text.push(words);
                None
            } else {
                Some(node(other)?)
            }
        }
    };
    Ok(Compiled {
        filter,
        text: (!text.is_empty()).then(|| text.join(" ")),
    })
}

fn refused(message: impl Into<String>) -> QueryError {
    QueryError {
        message: message.into(),
        at: None,
    }
}

/// The words of a term without a key, bare or quoted.
fn as_text(ast: &UserInputAst) -> Option<String> {
    match ast {
        UserInputAst::Leaf(leaf) => match leaf.as_ref() {
            UserInputLeaf::Literal(lit)
                if lit.field_name.is_none() && !lit.prefix && lit.slop == 0 =>
            {
                Some(lit.phrase.clone())
            }
            _ => None,
        },
        _ => None,
    }
}

/// One clause: its required terms ANDed, its optional (OR) terms as one
/// alternative, its excluded terms subtracted. `text` is `Some` only at the
/// top level, the one place text may appear.
fn clause(
    items: Vec<(Option<Occur>, UserInputAst)>,
    mut text: Option<&mut Vec<String>>,
) -> Result<Option<Expr>, QueryError> {
    let (mut all, mut any, mut none) = (Vec::new(), Vec::new(), Vec::new());
    for (occur, ast) in items {
        if let Some(words) = as_text(&ast) {
            match (occur, text.as_deref_mut()) {
                (None | Some(Occur::Must), Some(text)) => text.push(words),
                _ => {
                    return Err(refused(format!(
                        "text {words:?} can only be searched for, not combined with OR or NOT; \
                         use a key such as tag:{words}"
                    )))
                }
            }
            continue;
        }
        let expr = node(ast)?;
        match occur {
            None | Some(Occur::Must) => all.push(expr),
            Some(Occur::Should) => any.push(expr),
            Some(Occur::MustNot) => none.push(Expr::Not(Box::new(expr))),
        }
    }
    if !any.is_empty() {
        all.push(if any.len() == 1 {
            any.pop().unwrap()
        } else {
            Expr::Or(any)
        });
    }
    all.extend(none);
    Ok(match all.len() {
        0 => None,
        1 => all.pop(),
        _ => Some(Expr::And(all)),
    })
}

/// A nested term: a group, or a `key:value`.
fn node(ast: UserInputAst) -> Result<Expr, QueryError> {
    match ast {
        UserInputAst::Clause(items) => {
            clause(items, None)?.ok_or_else(|| refused("an empty group `()` selects nothing"))
        }
        UserInputAst::Boost(..) => Err(refused("boosts (`^`) are not supported")),
        UserInputAst::Leaf(leaf) => match *leaf {
            UserInputLeaf::Literal(lit) => {
                let Some(field) = lit.field_name.as_deref() else {
                    return Err(refused(format!(
                        "text {:?} can only be searched for, not combined with OR or NOT",
                        lit.phrase
                    )));
                };
                // The grammar keeps an unquoted `den*` as the word itself, so
                // the star is checked here as well as the prefix flag.
                let starred = lit.delimiter == Delimiter::None && lit.phrase.contains('*');
                if lit.prefix || starred || lit.slop > 0 {
                    return Err(refused(format!(
                        "{field}: takes an exact value; wildcards and `~` are not supported"
                    )));
                }
                term(field, &lit.phrase)
            }
            UserInputLeaf::Range {
                field,
                lower,
                upper,
            } => range(field.as_deref().unwrap_or(""), &lower, &upper),
            UserInputLeaf::Set { field, elements } => {
                let field = field.unwrap_or_default();
                let parts = elements
                    .iter()
                    .map(|v| term(&field, v))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(Expr::Or(parts))
            }
            UserInputLeaf::All => Err(refused("`*` alone selects everything; leave it out")),
            UserInputLeaf::Exists { field } => Err(refused(format!(
                "{field}:* is not supported; use has:gps or has:date"
            ))),
            UserInputLeaf::Regex { field, .. } => Err(refused(format!(
                "{}: takes an exact value; patterns are not supported",
                field.unwrap_or_default()
            ))),
        },
    }
}

fn leaf(fill: impl FnOnce(&mut RowSelection)) -> Expr {
    let mut sel = RowSelection::default();
    fill(&mut sel);
    Expr::Leaf(Box::new(sel))
}

fn rating_at_least(min: i64) -> Expr {
    leaf(|s| s.min_rating = Some(min))
}

fn stars(value: &str) -> Result<i64, QueryError> {
    match value.trim().parse::<i64>() {
        Ok(n) if (0..=5).contains(&n) => Ok(n),
        _ => Err(refused(format!(
            "rating must be a whole number of stars from 0 to 5, e.g. rating:4 or rating:>=4; \
             got {value:?}"
        ))),
    }
}

/// `key:value`, as the one-field selection the matching flag would build.
fn term(key: &str, value: &str) -> Result<Expr, QueryError> {
    let bad = |e: anyhow::Error| refused(format!("{key}:{value}: {e:#}"));
    let value = value.to_string();
    Ok(match key {
        "person" => leaf(|s| s.person = Some(value)),
        "tag" => leaf(|s| s.tags = vec![value]),
        "category" => leaf(|s| s.category = Some(value)),
        "place" => leaf(|s| s.place_name = Some(value)),
        "date" => {
            let (after, before) = videre_core::query::expand_date(&value).map_err(bad)?;
            leaf(|s| {
                s.after = Some(after);
                s.before = Some(before);
            })
        }
        "after" => {
            let bound = videre_core::query::normalise_bound(&value).map_err(bad)?;
            leaf(|s| s.after = Some(bound))
        }
        "before" => {
            let bound = videre_core::query::normalise_bound(&value).map_err(bad)?;
            leaf(|s| s.before = Some(bound))
        }
        "rating" => rating_at_least(stars(&value)?),
        "pick" => {
            let pick = match value.as_str() {
                "keep" => videre_core::marks::Pick::Keep,
                "reject" => videre_core::marks::Pick::Reject,
                _ => {
                    return Err(refused(format!(
                        "pick must be keep or reject; got {value:?}"
                    )))
                }
            };
            leaf(|s| s.pick = Some(pick))
        }
        "label" => leaf(|s| s.label = Some(value)),
        "is" => match value.as_str() {
            "liked" => leaf(|s| s.liked = true),
            _ => {
                return Err(refused(format!(
                    "unknown is:{value}; the only one is is:liked"
                )))
            }
        },
        "type" => {
            let kind = MediaKind::parse(&value).map_err(bad)?;
            leaf(|s| s.kinds = vec![kind])
        }
        "ext" => leaf(|s| s.exts = vec![value]),
        "mime" => leaf(|s| s.mimes = vec![value]),
        "path" => leaf(|s| s.paths = vec![PathBuf::from(value)]),
        "has" => {
            let field = PresenceField::parse(&value).map_err(bad)?;
            leaf(|s| s.has = vec![field])
        }
        "missing" => {
            let field = PresenceField::parse(&value).map_err(bad)?;
            leaf(|s| s.missing = vec![field])
        }
        other => {
            return Err(refused(format!(
                "unknown key {other:?}; keys are {}",
                KEYS.join(", ")
            )))
        }
    })
}

/// `rating:>=4`, `rating:[2 TO 3]` and friends. Stars are whole numbers, so
/// every bound becomes an "at least" selection: `> 3` is `>= 4`, and an upper
/// bound subtracts the ratings above it.
fn range(key: &str, lower: &UserInputBound, upper: &UserInputBound) -> Result<Expr, QueryError> {
    if key != "rating" {
        return Err(refused(format!(
            "ranges are supported only for rating; for dates use date:, after: and before:, \
             got a range on {key:?}"
        )));
    }
    let min = match lower {
        UserInputBound::Inclusive(v) => Some(stars(v)?),
        UserInputBound::Exclusive(v) => Some(stars(v)? + 1),
        UserInputBound::Unbounded => None,
    };
    let above = match upper {
        UserInputBound::Inclusive(v) => Some(stars(v)? + 1),
        UserInputBound::Exclusive(v) => Some(stars(v)?),
        UserInputBound::Unbounded => None,
    };
    let mut parts = Vec::new();
    if let Some(min) = min {
        parts.push(rating_at_least(min));
    }
    if let Some(above) = above {
        parts.push(Expr::Not(Box::new(rating_at_least(above))));
    }
    Ok(match parts.len() {
        0 => return Err(refused("rating:[* TO *] selects everything; leave it out")),
        1 => parts.pop().unwrap(),
        _ => Expr::And(parts),
    })
}

/// The hashes `expr` selects in `library`. Each leaf resolves through the
/// selection layer, with its path guard and per-model categories, exactly as
/// the flag it mirrors would; NOT subtracts from every hash in the library.
pub fn resolve(
    expr: &Expr,
    conn: &rusqlite::Connection,
    ctx: &videre_core::selection::SelectionCtx,
    library: &videre_core::library::LibraryContext,
) -> anyhow::Result<std::collections::HashSet<String>> {
    let mut everything = None;
    resolve_with(expr, conn, ctx, library, &mut everything)
}

fn resolve_with(
    expr: &Expr,
    conn: &rusqlite::Connection,
    ctx: &videre_core::selection::SelectionCtx,
    library: &videre_core::library::LibraryContext,
    everything: &mut Option<std::collections::HashSet<String>>,
) -> anyhow::Result<std::collections::HashSet<String>> {
    Ok(match expr {
        Expr::Leaf(sel) => sel
            .resolve_in(conn, ctx, library)?
            .hashes
            .expect("a leaf always holds a predicate"),
        Expr::And(parts) => {
            let mut acc: Option<std::collections::HashSet<String>> = None;
            for part in parts {
                // An empty intersection stays empty: skip the rest.
                if acc.as_ref().is_some_and(|a| a.is_empty()) {
                    break;
                }
                let set = resolve_with(part, conn, ctx, library, everything)?;
                acc = Some(match acc {
                    Some(a) => a.intersection(&set).cloned().collect(),
                    None => set,
                });
            }
            acc.unwrap_or_default()
        }
        Expr::Or(parts) => {
            let mut acc = std::collections::HashSet::new();
            for part in parts {
                acc.extend(resolve_with(part, conn, ctx, library, everything)?);
            }
            acc
        }
        Expr::Not(inner) => {
            let excluded = resolve_with(inner, conn, ctx, library, everything)?;
            if everything.is_none() {
                let mut stmt = conn.prepare("SELECT DISTINCT hash FROM file_hashes")?;
                let all = stmt
                    .query_map([], |r| r.get::<_, String>(0))?
                    .collect::<rusqlite::Result<_>>()?;
                *everything = Some(all);
            }
            everything
                .as_ref()
                .unwrap()
                .difference(&excluded)
                .cloned()
                .collect()
        }
    })
}

/// A compact, stable rendering for tests and error messages:
/// `and(person:"özgür", or(tag:deniz, tag:plaj), not(tag:ekran))`.
pub fn render(expr: &Expr) -> String {
    let join = |name: &str, parts: &[Expr]| {
        format!(
            "{name}({})",
            parts.iter().map(render).collect::<Vec<_>>().join(", ")
        )
    };
    match expr {
        Expr::And(parts) => join("and", parts),
        Expr::Or(parts) => join("or", parts),
        Expr::Not(inner) => format!("not({})", render(inner)),
        Expr::Leaf(sel) => sel.describe(),
    }
}

#[cfg(test)]
mod resolve_tests {
    use super::*;
    use videre_core::library::LibraryContext;
    use videre_core::selection::SelectionCtx;

    /// A library of four photos:
    /// - `deniz.jpg`: tagged deniz, 5 stars, 2023, Özgür in it;
    /// - `plaj.jpg`: tagged plaj, 2 stars, 2024, Ayşe in it;
    /// - `klip.mov`: untagged, unrated, undated;
    /// - `ekran.png`: tagged ekran, 2023.
    fn library() -> (tempfile::TempDir, LibraryContext) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("kütüphane");
        std::fs::create_dir(&root).unwrap();
        let ctx = LibraryContext::new(&root, &temp.path().join("cache")).unwrap();
        let conn = videre_core::library_db::initialize(&ctx).unwrap();
        let root = ctx.paths.root.display().to_string();
        conn.execute_batch(&format!(
            "INSERT INTO file_hashes (path, hash, ext, mime, exif_date) VALUES
               ('{root}/deniz.jpg', 'h_deniz', 'jpg', 'image/jpeg', '2023-07-01T10:00:00'),
               ('{root}/plaj.jpg',  'h_plaj',  'jpg', 'image/jpeg', '2024-08-01T10:00:00'),
               ('{root}/klip.mov',  'h_klip',  'mov', 'video/quicktime', NULL),
               ('{root}/ekran.png', 'h_ekran', 'png', 'image/png', '2023-02-01T10:00:00');
             INSERT INTO photo_tags VALUES
               ('h_deniz', 'deniz'), ('h_plaj', 'plaj'), ('h_ekran', 'ekran');
             INSERT INTO marks (hash, rating, liked, updated_at) VALUES
               ('h_deniz', 5, 1, 'now'), ('h_plaj', 2, 0, 'now');
             INSERT INTO people (name, full_name) VALUES ('ozgur', 'Özgür'), ('ayse', 'Ayşe');
             INSERT INTO faces (hash, bbox, embedding, person_label, confirmed) VALUES
               ('h_deniz', '0,0,9,9', X'00', 'ozgur', 1),
               ('h_plaj',  '0,0,9,9', X'00', 'ayse', 1);"
        ))
        .unwrap();
        (temp, ctx)
    }

    fn hashes(ctx: &LibraryContext, query: &str) -> Vec<String> {
        let conn = videre_core::library_db::open_existing(ctx).unwrap();
        let filter = compile(query).unwrap().filter.expect("a filter");
        let mut got: Vec<String> = resolve(&filter, &conn, &SelectionCtx::default(), ctx)
            .unwrap()
            .into_iter()
            .collect();
        got.sort();
        got
    }

    #[test]
    fn and_or_and_not_are_set_operations() {
        let (_t, ctx) = library();
        assert_eq!(hashes(&ctx, "tag:deniz OR tag:plaj"), ["h_deniz", "h_plaj"]);
        assert_eq!(hashes(&ctx, "tag:deniz is:liked"), ["h_deniz"]);
        assert_eq!(hashes(&ctx, "tag:deniz tag:plaj"), Vec::<String>::new());
        assert_eq!(hashes(&ctx, "type:image -tag:ekran"), ["h_deniz", "h_plaj"]);
    }

    #[test]
    fn not_keeps_files_that_have_nothing_to_exclude() {
        let (_t, ctx) = library();
        // The untagged clip is not tagged ekran, so it stays.
        assert_eq!(hashes(&ctx, "-tag:ekran"), ["h_deniz", "h_klip", "h_plaj"]);
    }

    #[test]
    fn missing_data_excludes() {
        let (_t, ctx) = library();
        assert_eq!(hashes(&ctx, "date:2023"), ["h_deniz", "h_ekran"]);
        assert_eq!(hashes(&ctx, "missing:date"), ["h_klip"]);
    }

    #[test]
    fn people_or_gives_their_union_and_a_band_is_exact() {
        let (_t, ctx) = library();
        assert_eq!(
            hashes(&ctx, "person:Özgür OR person:ayşe"),
            ["h_deniz", "h_plaj"]
        );
        assert_eq!(hashes(&ctx, "rating:[2 TO 4]"), ["h_plaj"]);
        assert_eq!(hashes(&ctx, "rating:>=3"), ["h_deniz"]);
    }

    #[test]
    fn nothing_matching_is_an_empty_set() {
        let (_t, ctx) = library();
        assert!(hashes(&ctx, "tag:yok").is_empty());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (filter rendering, text) for a query that must compile.
    fn ok(query: &str) -> (Option<String>, Option<String>) {
        let c = compile(query).unwrap_or_else(|e| panic!("{query:?}: {e}"));
        (c.filter.as_ref().map(render), c.text)
    }

    fn err(query: &str) -> QueryError {
        match compile(query) {
            Ok(c) => panic!(
                "{query:?} compiled: {:?} {:?}",
                c.filter.as_ref().map(render),
                c.text
            ),
            Err(e) => e,
        }
    }

    #[test]
    fn words_without_a_key_are_the_text() {
        assert_eq!(ok("gün batımı"), (None, Some("gün batımı".into())));
        assert_eq!(ok(r#""gün batımı""#), (None, Some("gün batımı".into())));
        assert_eq!(ok(""), (None, None));
    }

    #[test]
    fn text_and_filters_combine() {
        let (filter, text) = ok(r#""gün batımı" person:özgür"#);
        assert_eq!(filter.as_deref(), Some(r#"--person "özgür""#));
        assert_eq!(text.as_deref(), Some("gün batımı"));
        let (_, text) = ok("kumsalda tag:deniz gün batımı");
        assert_eq!(text.as_deref(), Some("kumsalda gün batımı"));
    }

    #[test]
    fn a_quoted_value_keeps_its_spaces() {
        let (filter, _) = ok(r#"person:"Erhan Gündoğan""#);
        assert_eq!(filter.as_deref(), Some(r#"--person "Erhan Gündoğan""#));
        let (filter, _) = ok(r#"place:"Kadıköy, İstanbul""#);
        assert_eq!(filter.as_deref(), Some(r#"place:"Kadıköy, İstanbul""#));
    }

    #[test]
    fn or_not_and_nesting() {
        assert_eq!(
            ok("tag:deniz OR tag:plaj").0.as_deref(),
            Some("or(--tag deniz, --tag plaj)")
        );
        assert_eq!(ok("-tag:ekran").0.as_deref(), Some("not(--tag ekran)"));
        assert_eq!(ok("NOT tag:ekran").0.as_deref(), Some("not(--tag ekran)"));
        assert_eq!(
            ok("tag:deniz AND (person:özgür OR person:ayşe)")
                .0
                .as_deref(),
            Some(r#"and(--tag deniz, or(--person "özgür", --person "ayşe"))"#)
        );
        assert_eq!(
            ok("tag:deniz -tag:ekran is:liked").0.as_deref(),
            Some("and(--tag deniz, --like, not(--tag ekran))")
        );
    }

    #[test]
    fn ratings_are_at_least_or_a_band() {
        assert_eq!(ok("rating:4").0.as_deref(), Some("--rating 4"));
        assert_eq!(ok("rating:>=4").0.as_deref(), Some("--rating 4"));
        assert_eq!(ok("rating:>3").0.as_deref(), Some("--rating 4"));
        assert_eq!(
            ok("rating:[2 TO 3]").0.as_deref(),
            Some("and(--rating 2, not(--rating 4))")
        );
        assert_eq!(ok("rating:<3").0.as_deref(), Some("not(--rating 3)"));
    }

    #[test]
    fn dates_expand_like_the_flags() {
        assert_eq!(
            ok("date:2023").0.as_deref(),
            Some("--after 2023-01-01T00:00:00 --before 2024-01-01T00:00:00")
        );
        assert_eq!(
            ok("after:2023-05-01").0.as_deref(),
            Some("--after 2023-05-01T00:00:00")
        );
    }

    #[test]
    fn every_other_key() {
        for (query, want) in [
            ("category:document", "--category document"),
            ("pick:reject", "--pick reject"),
            ("label:Red", "--label Red"),
            ("type:video", "--type video"),
            ("ext:heic", "--ext heic"),
            ("mime:image/heic", "--mime image/heic"),
            ("path:Tatil/2023", "--path Tatil/2023"),
            ("has:gps", "--has gps"),
            ("missing:date", "--missing date"),
        ] {
            assert_eq!(ok(query).0.as_deref(), Some(want), "{query}");
        }
    }

    #[test]
    fn mistakes_are_named() {
        let e = err("kişi:özgür");
        assert!(e.message.contains("unknown key"), "{e}");
        assert!(e.message.contains("person"), "lists the keys: {e}");
        assert!(err("is:favourite").message.contains("is:liked"));
        assert!(err("rating:beş").message.contains("rating"));
        assert!(err("type:audio").message.contains("image, video"));
        assert!(err("pick:maybe").message.contains("keep"));
        assert!(err("date:dün").message.contains("date"));
    }

    #[test]
    fn text_is_refused_inside_or_and_not() {
        assert!(err("deniz OR tag:plaj").message.contains("text"));
        assert!(err("-deniz").message.contains("text"));
        assert!(err("tag:x (deniz OR plaj)").message.contains("text"));
    }

    #[test]
    fn a_syntax_error_says_where() {
        let e = err("(tag:deniz");
        assert_eq!(e.at, Some(10), "{e}");
    }

    #[test]
    fn unsupported_syntax_is_refused_not_ignored() {
        for q in ["tag:den*", "tag:deniz^2", "tag:/de.*/", "person:\"a b\"~2"] {
            assert!(compile(q).is_err(), "{q} must be refused");
        }
    }
}
