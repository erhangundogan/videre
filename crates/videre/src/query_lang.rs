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
    "person", "people", "tag", "category", "place", "date", "after", "before", "rating", "pick",
    "label", "is", "type", "ext", "mime", "path", "has", "missing",
];

/// Compile `query` into a filter and a ranking text.
pub fn compile(query: &str) -> Result<Compiled, QueryError> {
    if query.trim().is_empty() {
        return Ok(Compiled::default());
    }
    // The lenient parser is the one that knows where a mistake is; the strict
    // one is used only once the lenient one found nothing wrong.
    let (escaped, inserted) = escape_word_apostrophes(query);
    let (_, errors) = parse_query_lenient(&escaped);
    if let Some(first) = errors.first() {
        // Report the position in the text as typed, not the escaped copy,
        // and in characters, which is what the messages and the gallery say:
        // the grammar counts bytes, and every Turkish letter is two.
        let shift = inserted.iter().take_while(|&&at| at < first.pos).count();
        let byte = (first.pos - shift).min(query.len());
        let at = query.char_indices().take_while(|&(i, _)| i < byte).count();
        return Err(QueryError {
            message: first.message.clone(),
            at: Some(at),
        });
    }
    let ast = parse_query(&escaped).map_err(|_| refused("the query could not be parsed"))?;
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

/// Escape every apostrophe that follows a letter or digit outside double
/// quotes, so the grammar reads it as part of the word, not as the start of
/// a single-quoted phrase.
///
/// :warning: Turkish puts an apostrophe before every suffix on a proper noun
/// (`İstanbul'da`, `Ayşe'nin`) and English before a possessive, so without
/// this an ordinary search failed with "missing delimiter". A quote that
/// opens a word (`'gün batımı'`) still quotes, up to its closing quote. Returns the escaped text and
/// the byte offsets, in it, of each inserted backslash, so an error position
/// can be mapped back to what was typed.
fn escape_word_apostrophes(query: &str) -> (String, Vec<usize>) {
    let mut out = String::with_capacity(query.len());
    let mut inserted = Vec::new();
    let (mut in_double, mut in_single) = (false, false);
    let mut prev: Option<char> = None;
    for c in query.chars() {
        let escaped = prev == Some('\\');
        if c == '"' && !escaped && !in_single {
            in_double = !in_double;
        } else if c == '\'' && !escaped && !in_double {
            if in_single {
                in_single = false;
            } else if prev.is_some_and(char::is_alphanumeric) {
                inserted.push(out.len());
                out.push('\\');
            } else {
                in_single = true;
            }
        }
        out.push(c);
        prev = Some(c);
    }
    (out, inserted)
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
        "people" => leaf(|s| s.people = Some(value)),
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

/// True when any leaf of `expr` satisfies `test`: for a command to refuse a
/// key it cannot answer, as the flags refuse it by not existing.
pub fn any_leaf(expr: &Expr, test: &dyn Fn(&RowSelection) -> bool) -> bool {
    match expr {
        Expr::And(parts) | Expr::Or(parts) => parts.iter().any(|p| any_leaf(p, test)),
        Expr::Not(inner) => any_leaf(inner, test),
        Expr::Leaf(sel) => test(sel),
    }
}

/// What a suggestion completes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SuggestionKind {
    Key,
    Value,
    Operator,
}

/// One suggestion: `insert` replaces the query from [`Suggestions::start`] to
/// the cursor. `label` is what a person reads (a display name, for a person
/// inserted by identity), `count` how many files have the value, and
/// `face_id` a face to show beside a person.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Suggestion {
    pub insert: String,
    pub label: String,
    pub kind: SuggestionKind,
    pub count: Option<i64>,
    pub face_id: Option<i64>,
}

/// The suggestions for the term at the cursor, and where that term starts,
/// as a character offset, as [`QueryError::at`] is.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Suggestions {
    pub start: usize,
    pub items: Vec<Suggestion>,
}

/// One value a key can take in this library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Value {
    /// What goes after `key:`, unquoted.
    pub value: String,
    pub label: String,
    pub count: Option<i64>,
    pub face_id: Option<i64>,
}

/// What can complete the term at `cursor` (a character offset) in `text`: a
/// key while one is typed, the library's values after `key:`, and operators
/// after a complete term. One function for the gallery and the shell
/// completer, so both offer the same things. Anything the library cannot
/// answer (a missing table, an unknown key) suggests nothing rather than
/// failing: a suggestion is a convenience, never an error.
pub fn suggest(
    conn: &rusqlite::Connection,
    text: &str,
    cursor: usize,
    limit: usize,
) -> Suggestions {
    let before: Vec<char> = text.chars().take(cursor).collect();
    let Some(mut start) = term_start(&before) else {
        // Inside a quoted phrase: that is text, not a term to complete.
        return Suggestions::default();
    };
    let negated = before.get(start) == Some(&'-');
    if negated {
        start += 1;
    }
    let term: String = before[start..].iter().collect();
    let mut items = Vec::new();
    if let Some((key, typed)) = term.split_once(':') {
        let typed = typed.strip_prefix('"').unwrap_or(typed);
        for v in values(conn, key, typed) {
            let insert = format!("{key}:{}", quoted(&v.value));
            items.push(Suggestion {
                insert,
                label: v.label,
                kind: SuggestionKind::Value,
                count: v.count,
                face_id: v.face_id,
            });
        }
    } else {
        let follows_a_term = !negated
            && before[..start]
                .iter()
                .rev()
                .find(|c| !c.is_whitespace())
                .is_some_and(|c| *c != '(');
        if follows_a_term {
            for op in ["OR", "-", "("] {
                if op.starts_with(term.as_str()) {
                    items.push(operator(op));
                }
            }
        }
        let typed = term.to_lowercase();
        for key in KEYS {
            if key.starts_with(typed.as_str()) {
                items.push(Suggestion {
                    insert: format!("{key}:"),
                    label: format!("{key}:"),
                    kind: SuggestionKind::Key,
                    count: None,
                    face_id: None,
                });
            }
        }
    }
    items.truncate(limit);
    Suggestions { start, items }
}

fn operator(op: &str) -> Suggestion {
    Suggestion {
        insert: op.to_string(),
        label: op.to_string(),
        kind: SuggestionKind::Operator,
        count: None,
        face_id: None,
    }
}

/// Where the term ending at the cursor begins: after the last whitespace or
/// `(` outside double quotes. `None` inside a quoted phrase, where the quote
/// did not follow a `key:`.
fn term_start(before: &[char]) -> Option<usize> {
    let (mut start, mut quote_at) = (0, None);
    for (i, c) in before.iter().enumerate() {
        match (quote_at, c) {
            (None, '"') => quote_at = Some(i),
            (Some(_), '"') => quote_at = None,
            (None, c) if c.is_whitespace() || *c == '(' => start = i + 1,
            _ => {}
        }
    }
    match quote_at {
        // An open quote is fine as a value's (`place:"Kad`), not as text.
        Some(q) if q == 0 || before[q - 1] != ':' => None,
        _ => Some(start),
    }
}

/// `value` as a query term's value: quoted when it has a space or anything
/// the grammar reads as syntax.
fn quoted(value: &str) -> String {
    let plain = !value.is_empty()
        && !value.starts_with('-')
        && !["OR", "AND", "NOT", "TO"].contains(&value)
        && !value
            .chars()
            .any(|c| c.is_whitespace() || "():\"'[]{}^~*\\".contains(c));
    if plain {
        value.to_string()
    } else {
        format!("\"{value}\"")
    }
}

/// The values `key` can take in this library that match `typed`, most used
/// first. Matching folds case and accents, and a value matches when any of
/// its words starts with what was typed, so "gündo" finds "Erhan Gündoğan".
/// A person is offered by identity, with the display name as the label.
pub fn values(conn: &rusqlite::Connection, key: &str, typed: &str) -> Vec<Value> {
    let fixed = |vals: &[&str]| -> Vec<Value> {
        vals.iter()
            .map(|v| Value {
                value: v.to_string(),
                label: v.to_string(),
                count: None,
                face_id: None,
            })
            .collect()
    };
    let sql = |q: &str| -> Vec<Value> { counted(conn, q).unwrap_or_default() };
    let mut all = match key {
        "person" | "people" => {
            if !table_exists(conn, "faces") {
                return Vec::new();
            }
            let people = table_exists(conn, "people");
            // Every named person, even one in no file yet, as `--person`
            // always offered; and any label that has no people row.
            let q = if people {
                "SELECT p.name, p.full_name,
                        (SELECT COUNT(DISTINCT hash) FROM faces
                         WHERE person_label = p.name AND confirmed = 1),
                        (SELECT MIN(id) FROM faces
                         WHERE person_label = p.name AND confirmed = 1)
                 FROM people p
                 UNION ALL
                 SELECT person_label, person_label, COUNT(DISTINCT hash), MIN(id)
                 FROM faces
                 WHERE confirmed = 1 AND person_label IS NOT NULL
                   AND person_label NOT IN (SELECT name FROM people)
                 GROUP BY person_label"
            } else {
                "SELECT person_label, person_label, COUNT(DISTINCT hash), MIN(id)
                 FROM faces WHERE confirmed = 1 AND person_label IS NOT NULL
                 GROUP BY person_label"
            };
            sql(q)
        }
        "tag" if table_exists(conn, "photo_tags") => {
            sql("SELECT tag, tag, COUNT(*), NULL FROM photo_tags GROUP BY tag")
        }
        "category" if table_exists(conn, "classifications") => {
            sql("SELECT category, category, COUNT(DISTINCT hash), NULL
             FROM classifications GROUP BY category")
        }
        "label" if table_exists(conn, "marks") => {
            sql("SELECT label, label, COUNT(*), NULL FROM marks
             WHERE label IS NOT NULL AND label != '' GROUP BY label")
        }
        "place" => place_values(conn),
        "ext" => sql("SELECT ext, ext, COUNT(*), NULL FROM file_hashes
             WHERE ext IS NOT NULL AND ext != '' GROUP BY ext"),
        "mime" => sql("SELECT mime, mime, COUNT(*), NULL FROM file_hashes
             WHERE mime IS NOT NULL AND mime != '' GROUP BY mime"),
        "is" => fixed(&["liked"]),
        "type" => fixed(&["image", "video"]),
        "has" | "missing" => fixed(&["gps", "date"]),
        "pick" => fixed(&["keep", "reject"]),
        "rating" => fixed(&["1", "2", "3", "4", "5"]),
        _ => Vec::new(),
    };
    // Counted values, most used first; fixed ones keep their order.
    if all.iter().any(|v| v.count.is_some()) {
        all.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.value.cmp(&b.value)));
    }
    all.retain(|v| !v.value.contains('"') && matches_typed(v, typed));
    all
}

fn table_exists(conn: &rusqlite::Connection, name: &str) -> bool {
    videre_core::db::table_exists(conn, name).unwrap_or(false)
}

/// Rows of (value, label, count, face id).
fn counted(conn: &rusqlite::Connection, sql: &str) -> rusqlite::Result<Vec<Value>> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map([], |r| {
        Ok(Value {
            value: r.get(0)?,
            label: r.get(1)?,
            count: r.get(2)?,
            face_id: r.get(3)?,
        })
    })?;
    rows.collect()
}

/// Place names from geocoding and from location clusters, as `place:`
/// matches them (`query::by_place_name`).
fn place_values(conn: &rusqlite::Connection) -> Vec<Value> {
    let mut by_name: std::collections::BTreeMap<String, i64> = Default::default();
    let named = counted(
        conn,
        "SELECT location_name, location_name, COUNT(*), NULL FROM file_hashes
         WHERE location_name IS NOT NULL AND location_name != '' GROUP BY location_name",
    )
    .unwrap_or_default();
    let clustered = if table_exists(conn, "location_clusters") {
        counted(
            conn,
            "SELECT c.name, c.name, COUNT(f.hash), NULL FROM location_clusters c
             JOIN file_hashes f ON f.location_cluster_id = c.id
             WHERE c.name IS NOT NULL AND c.name != '' GROUP BY c.name",
        )
        .unwrap_or_default()
    } else {
        Vec::new()
    };
    // The same name from both sources is one place; its larger count is the
    // better guess, since the two overlap rather than add up.
    for v in named.into_iter().chain(clustered) {
        let n = by_name.entry(v.value).or_default();
        *n = (*n).max(v.count.unwrap_or(0));
    }
    by_name
        .into_iter()
        .map(|(name, n)| Value {
            label: name.clone(),
            value: name,
            count: Some(n),
            face_id: None,
        })
        .collect()
}

fn matches_typed(v: &Value, typed: &str) -> bool {
    let Some(want) = videre_core::person::normalize(typed) else {
        return typed.trim().is_empty();
    };
    [v.value.as_str(), v.label.as_str()].iter().any(|s| {
        videre_core::person::normalize(s).is_some_and(|folded| {
            folded.starts_with(&want)
                || folded
                    .match_indices('_')
                    .any(|(i, _)| folded[i + 1..].starts_with(&want))
        })
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
mod test_library {
    use videre_core::library::LibraryContext;

    /// A library of four photos:
    /// - `deniz.jpg`: tagged deniz, 5 stars, 2023, Özgür in it;
    /// - `plaj.jpg`: tagged plaj, 2 stars, 2024, Ayşe in it;
    /// - `klip.mov`: untagged, unrated, undated;
    /// - `ekran.png`: tagged ekran, 2023.
    ///
    /// For suggestions: `plaj.jpg` is in "Kadıköy, İstanbul", `deniz.jpg` has
    /// the label red, and `ekran.png` is classified as a screenshot.
    pub(super) fn library() -> (tempfile::TempDir, LibraryContext) {
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
             UPDATE marks SET label = 'red' WHERE hash = 'h_deniz';
             UPDATE file_hashes SET location_name = 'Kadıköy, İstanbul' WHERE hash = 'h_plaj';
             INSERT INTO classifications (model_id, hash, category, confidence, classified_at)
               VALUES ('m', 'h_ekran', 'screenshot', 0.9, 'now');
             INSERT INTO people (name, full_name) VALUES
               ('ozgur', 'Özgür'), ('ayse', 'Ayşe'), ('erhan_gundogan', 'Erhan Gündoğan'),
               ('zeynep', 'Zeynep');
             INSERT INTO faces (hash, bbox, embedding, person_label, confirmed) VALUES
               ('h_deniz', '0,0,9,9', X'00', 'ozgur', 1),
               ('h_plaj',  '0,0,9,9', X'00', 'ayse', 1),
               ('h_plaj',  '1,1,9,9', X'00', 'erhan_gundogan', 1),
               ('h_ekran', '1,1,9,9', X'00', 'erhan_gundogan', 1);"
        ))
        .unwrap();
        (temp, ctx)
    }
}

#[cfg(test)]
mod suggest_tests {
    use super::test_library::library;
    use super::*;

    /// What `text` suggests with the cursor at its end: the term's start and
    /// each suggestion's insert text.
    fn at_end(text: &str) -> (usize, Vec<String>) {
        let (_t, ctx) = library();
        let conn = videre_core::library_db::open_existing(&ctx).unwrap();
        let s = suggest(&conn, text, text.chars().count(), 50);
        (s.start, s.items.into_iter().map(|i| i.insert).collect())
    }

    fn inserts(text: &str) -> Vec<String> {
        at_end(text).1
    }

    #[test]
    fn a_key_being_typed_offers_keys() {
        assert_eq!(inserts("pe"), ["person:", "people:"]);
        assert_eq!(inserts("ta"), ["tag:"]);
        assert_eq!(at_end("deniz pl"), (6, vec!["place:".to_string()]));
        assert_eq!(
            inserts("").len(),
            KEYS.len(),
            "an empty query offers every key"
        );
    }

    #[test]
    fn a_key_offers_the_library_s_values_most_used_first() {
        assert_eq!(inserts("tag:"), ["tag:deniz", "tag:ekran", "tag:plaj"]);
        assert_eq!(inserts("tag:de"), ["tag:deniz"]);
        assert_eq!(inserts("label:"), ["label:red"]);
        assert_eq!(inserts("category:"), ["category:screenshot"]);
        assert_eq!(inserts("ext:"), ["ext:jpg", "ext:mov", "ext:png"]);
        assert!(inserts("tag:yok").is_empty());
        assert!(inserts("unknown:").is_empty());
    }

    #[test]
    fn people_are_matched_folded_on_any_word_and_inserted_by_identity() {
        // Erhan is in two files, so first.
        assert_eq!(
            inserts("person:"),
            [
                "person:erhan_gundogan",
                "person:ayse",
                "person:ozgur",
                "person:zeynep"
            ]
        );
        // Named, but in no file yet: still offered, last, with no files.
        let (_t, ctx) = library();
        let conn = videre_core::library_db::open_existing(&ctx).unwrap();
        let zeynep = &values(&conn, "person", "zey")[0];
        assert_eq!((zeynep.value.as_str(), zeynep.count), ("zeynep", Some(0)));
        assert_eq!(inserts("person:Öz"), ["person:ozgur"]);
        assert_eq!(inserts("person:oz"), ["person:ozgur"]);
        assert_eq!(inserts("person:gündo"), ["person:erhan_gundogan"]);
        assert_eq!(inserts("person:\"Erh"), ["person:erhan_gundogan"]);

        let (_t, ctx) = library();
        let conn = videre_core::library_db::open_existing(&ctx).unwrap();
        let s = suggest(&conn, "person:erh", 10, 50);
        let erhan = &s.items[0];
        assert_eq!(erhan.label, "Erhan Gündoğan");
        assert_eq!(erhan.count, Some(2));
        assert!(erhan.face_id.is_some(), "a face to show beside the name");
    }

    #[test]
    fn a_value_with_spaces_is_inserted_quoted() {
        assert_eq!(inserts("place:"), ["place:\"Kadıköy, İstanbul\""]);
        assert_eq!(inserts("place:kadi"), ["place:\"Kadıköy, İstanbul\""]);
        assert_eq!(inserts("place:\"Kad"), ["place:\"Kadıköy, İstanbul\""]);
        assert_eq!(inserts("place:ist"), ["place:\"Kadıköy, İstanbul\""]);
    }

    #[test]
    fn fixed_values_are_offered_for_their_keys() {
        assert_eq!(inserts("is:"), ["is:liked"]);
        assert_eq!(inserts("type:v"), ["type:video"]);
        assert_eq!(inserts("has:"), ["has:gps", "has:date"]);
        assert_eq!(inserts("pick:"), ["pick:keep", "pick:reject"]);
        assert_eq!(inserts("rating:4").len(), 1);
    }

    #[test]
    fn a_negated_term_keeps_its_minus() {
        assert_eq!(at_end("-tag:ek"), (1, vec!["tag:ekran".to_string()]));
        assert_eq!(at_end("deniz (tag:pl"), (7, vec!["tag:plaj".to_string()]));
    }

    #[test]
    fn a_complete_term_is_followed_by_operators_and_keys() {
        let got = inserts("tag:deniz ");
        assert_eq!(&got[..3], ["OR", "-", "("]);
        assert!(got.contains(&"person:".to_string()));
        assert_eq!(inserts("tag:deniz O"), ["OR"]);
        // A minus waits for a term; an operator cannot follow it.
        let negated = inserts("tag:deniz -");
        assert!(!negated.contains(&"OR".to_string()), "{negated:?}");
        assert_eq!(negated.len(), KEYS.len());
    }

    #[test]
    fn inside_a_quoted_phrase_there_is_nothing_to_suggest() {
        assert!(inserts("\"gün bat").is_empty());
        assert!(inserts("tag:deniz \"ak").is_empty());
    }

    #[test]
    fn the_cursor_not_the_end_decides_the_term() {
        let (_t, ctx) = library();
        let conn = videre_core::library_db::open_existing(&ctx).unwrap();
        // Cursor after "tag:de", before " person:oz".
        let s = suggest(&conn, "tag:de person:oz", 6, 50);
        assert_eq!(s.start, 0);
        let got: Vec<String> = s.items.into_iter().map(|i| i.insert).collect();
        assert_eq!(got, ["tag:deniz"]);
    }

    #[test]
    fn values_are_what_the_flag_completers_offer_too() {
        let (_t, ctx) = library();
        let conn = videre_core::library_db::open_existing(&ctx).unwrap();
        let people: Vec<String> = values(&conn, "person", "ay")
            .into_iter()
            .map(|v| v.value)
            .collect();
        assert_eq!(people, ["ayse"]);
    }
}

#[cfg(test)]
mod resolve_tests {
    use super::test_library::library;
    use super::*;
    use videre_core::library::LibraryContext;
    use videre_core::selection::SelectionCtx;

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
    fn people_gives_everyone_with_the_word_in_their_name() {
        let (_t, ctx) = library();
        // Erhan Gündoğan is in plaj and ekran; a surname finds him.
        assert_eq!(hashes(&ctx, "people:gündoğan"), ["h_ekran", "h_plaj"]);
        assert_eq!(
            hashes(&ctx, "people:özgür OR people:ayşe"),
            ["h_deniz", "h_plaj"]
        );
        // person: names one identity exactly; people: never takes a prefix.
        assert!(hashes(&ctx, "people:gün").is_empty());
        assert!(hashes(&ctx, "person:gündoğan").is_empty());
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
    fn or_binds_its_neighbours_and_the_rest_must_match() {
        assert_eq!(
            ok("person:özgür tag:deniz OR tag:plaj").0.as_deref(),
            Some(r#"and(--person "özgür", or(--tag deniz, --tag plaj))"#)
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
    fn an_apostrophe_inside_a_word_is_part_of_it() {
        // Turkish puts one before every suffix on a proper noun, English
        // before every possessive. Neither is a quote.
        assert_eq!(
            ok("İstanbul'da deniz"),
            (None, Some("İstanbul'da deniz".into()))
        );
        assert_eq!(ok("the dog's toy"), (None, Some("the dog's toy".into())));
        assert_eq!(
            ok("çocukların' top"),
            (None, Some("çocukların' top".into()))
        );
        assert_eq!(
            ok("tag:ayşe'nin").0,
            ok("tag:ayşe_nin")
                .0
                .map(|f| f.replace("ayşe_nin", "ayşe'nin"))
        );
        assert_eq!(
            ok(r#""Ayşe'nin evi" tag:deniz"#).1,
            Some("Ayşe'nin evi".into())
        );
        // A quote that opens a word still quotes.
        assert_eq!(ok("'gün batımı'"), (None, Some("gün batımı".into())));
    }

    #[test]
    fn an_error_after_an_apostrophe_is_placed_in_the_typed_text() {
        let with = err("İstanbul'da ) deniz");
        let without = err("İstanbulxda ) deniz");
        assert_eq!(with.at, without.at, "{with}");
    }

    #[test]
    fn an_error_position_counts_characters_not_bytes() {
        // "İ" and "ü" are two bytes each; the `)` is character 12.
        assert_eq!(err("İstanbul'da ) deniz").at, Some(12));
        assert_eq!(err("gün ) deniz").at, Some(4));
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
