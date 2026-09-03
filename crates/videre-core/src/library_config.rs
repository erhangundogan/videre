//! Library-scoped settings: the type and its built-in defaults.
//!
//! Only the shape lives here. Reading and editing the `config.toml` under a
//! library's reserved state directory is the config layer's job; a context is
//! constructed with these defaults and the loader layers a file onto them.

use crate::embeddings::DEFAULT_MODEL_ID;
use crate::marks::XmpPrecedence;

/// Settings governing how one library is processed.
///
/// Absent settings mean the built-in default, mirroring the global config's
/// convention where a missing key falls back rather than erroring.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryConfig {
    /// Embedding model id, e.g. `google/siglip-base-patch16-224`. A plain
    /// string, not a path: it must never be absolutized, the same rule the
    /// global config's `default_model` follows.
    pub default_model: String,
    /// How a mark read from a file's XMP reconciles with the db on
    /// scan/watch/import; the default is db wins and XMP fills the gaps.
    pub xmp_precedence: XmpPrecedence,
    /// Whether `videre watch` runs the XMP export stage each cycle. Opt-in:
    /// absent means off, matching the global config.
    pub export_xmp_on_watch: bool,
    /// Assumed floor read rate in MB/s used to scale I/O timeouts to file
    /// size; `None` means the built-in default applies
    /// (`io_timeout::MIN_READ_RATE_MB_S_DEFAULT`).
    pub min_read_rate_mb_s: Option<u64>,
}

impl Default for LibraryConfig {
    /// The built-in defaults: the built-in embedding model, db-first XMP
    /// precedence, no export on watch, and the timeout floor left at its
    /// built-in value.
    fn default() -> Self {
        Self {
            default_model: DEFAULT_MODEL_ID.to_string(),
            xmp_precedence: XmpPrecedence::default(),
            export_xmp_on_watch: false,
            min_read_rate_mb_s: None,
        }
    }
}
