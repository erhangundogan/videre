//! Apple Photos and iPhoto: the pre-flight checklist and the two warnings the
//! filesystem can honestly give.
//!
//! Deliberately reads no Apple database. `Photos.sqlite` is undocumented,
//! changes between macOS releases, and renames its own tables, so an importer
//! built on it breaks on someone else's release schedule. Everything here
//! answers the question "would this still work if Apple renamed every table
//! tomorrow?" with yes.
//!
//! Of the three ways a naive walk goes wrong, only one destroys data:
//! importing device-sized stand-ins believing they are originals is
//! unrecoverable once the Apple library is deleted, which is exactly what
//! someone doing this migration is about to do. Trashed photos and a
//! referenced library are both recoverable, so the warnings are weighted
//! accordingly rather than treating three different-sized problems as equals.

use std::path::{Path, PathBuf};

/// Median size below which a library is called optimised.
///
/// Modern phone originals run roughly 2 to 5 MB for stills and far more for
/// video; device-sized stand-ins are typically a few hundred KB.
///
/// **A starting point, not a measured value: this must be calibrated against a
/// genuinely optimised library before release.** Raising it makes false
/// positives on libraries of older, smaller photos; lowering it lets a real
/// optimised library through, which is the expensive direction.
const OPTIMISED_MEDIAN_BYTES: u64 = 300 * 1024;

/// Above this many originals, a library is too populated to be referenced, and
/// the package walk that the referenced check needs is skipped entirely. That
/// bounds its cost: a real library never triggers it.
const REFERENCED_MAX_FILES: usize = 100;

/// What the filesystem can say about a library, and nothing more.
pub(crate) struct LibraryShape {
    pub files: usize,
    pub median_bytes: u64,
    pub originals_bytes: u64,
    /// Every byte in the package, including previews and databases. Only
    /// computed when the originals count is low enough to be suspicious.
    pub package_bytes: u64,
}

impl LibraryShape {
    pub(crate) fn from_sizes(sizes: &[u64], package_bytes: u64) -> Self {
        let mut sorted = sizes.to_vec();
        sorted.sort_unstable();
        // Lower middle for an even count: no averaging, so no overflow on
        // large files and no float anywhere in a size comparison.
        let median_bytes = if sorted.is_empty() {
            0
        } else {
            sorted[(sorted.len() - 1) / 2]
        };
        Self {
            files: sizes.len(),
            median_bytes,
            // Saturating: a debug build panics on an overflowing `sum()`, and
            // a size total is a report, never a reason to abort a run.
            originals_bytes: sizes.iter().fold(0u64, |a, b| a.saturating_add(*b)),
            package_bytes,
        }
    }

    /// The safety net for a pre-flight checklist that was skipped.
    ///
    /// A statement about the library, not a per-file filter: individual small
    /// files are perfectly normal, so classifying file by file would produce
    /// constant false positives. The population statistic is the honest
    /// signal, and the user is the one who knows whether optimisation is on.
    pub(crate) fn looks_optimised(&self) -> bool {
        self.files > 0 && self.median_bytes < OPTIMISED_MEDIAN_BYTES
    }

    /// A referenced library never copies files in, so `originals/` holds
    /// almost nothing while the package is otherwise complete. Directly
    /// observable, and needs no schema.
    pub(crate) fn looks_referenced(&self) -> bool {
        // `files > 0` matters: an *empty* originals folder is ambiguous between
        // a referenced library and one whose originals iCloud has evicted, and
        // the filesystem cannot separate them, so it gets its own message
        // rather than being asserted as referenced. Found against a real
        // library with an empty originals/ and all 16 derivatives fan-out dirs.
        self.files > 0
            && self.files <= REFERENCED_MAX_FILES
            && self.package_bytes > 0
            && self.originals_bytes.saturating_mul(10) < self.package_bytes
    }
}

/// Measures the located originals, and the package only when it might matter.
pub(crate) fn survey(package: &Path, files: &[PathBuf]) -> LibraryShape {
    let sizes: Vec<u64> = files
        .iter()
        .filter_map(|f| std::fs::metadata(f).ok().map(|m| m.len()))
        .collect();
    let package_bytes = if sizes.len() <= REFERENCED_MAX_FILES {
        walkdir::WalkDir::new(package)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .filter_map(|e| e.metadata().ok().map(|m| m.len()))
            .sum()
    } else {
        0
    };
    LibraryShape::from_sizes(&sizes, package_bytes)
}

/// The mechanism that replaces schema reading.
///
/// Deliberately a checklist rather than a detection routine: whether iCloud
/// optimisation is on is something only the user can see, and asking is both
/// more reliable and far cheaper than inferring it. The ordering carries the
/// message, so item 1 is the one that loses data and the rest are
/// conveniences.
pub(crate) fn checklist(shape: &LibraryShape) -> String {
    format!(
        "\nBefore continuing, in the Photos app:\n\
         \n  \
           1. Settings > iCloud > Download Originals to this Mac\n     \
              If \"Optimise Mac Storage\" is on, the files on disk are smaller\n     \
              stand-ins, not your originals. Wait for the download to finish.\n     \
              This is the one worth getting right: once you delete the Apple\n     \
              library, whatever was left in iCloud is not coming back.\n\
         \n  \
           2. Optionally, empty Recently Deleted\n     \
              Deleted photos are still on disk and will be imported. Harmless,\n     \
              since you can delete them again from videre, but it saves a pass.\n\
         \n  \
           3. Optionally, quit Photos\n     \
              Avoids it writing while videre reads.\n\
         \n\
         Found {} file(s), {}, median size {}.\n",
        shape.files,
        human_bytes(shape.originals_bytes),
        human_bytes(shape.median_bytes),
    )
}

pub(crate) fn optimised_warning(shape: &LibraryShape) -> String {
    format!(
        "warning: median file size is {} across {} files.\n  \
           That is far below what camera originals normally are, which usually means\n  \
           \"Optimise Mac Storage\" is on and these are device-sized stand-ins.\n  \
           In Photos: Settings > iCloud > Download Originals to this Mac.",
        human_bytes(shape.median_bytes),
        shape.files,
    )
}

/// `originals/` held nothing at all.
///
/// Deliberately offers both explanations instead of picking one. On disk they
/// are indistinguishable, and the one that loses data (iCloud eviction) is the
/// one worth naming first.
pub(crate) fn empty_originals_warning() -> String {
    format!(
        "warning: the originals folder is empty, so there is nothing to import.\n  \
           Two things cause this, and they look identical on disk:\n\
         \n  \
           1. The originals are not on this Mac, only in iCloud.\n     \
              Photos > Settings > iCloud: if \"Optimise Mac Storage\" is on,\n     \
              choose \"Download Originals to this Mac\" and let it finish.\n     \
              This also happens with iCloud switched off, if it was turned off\n     \
              with \"Remove from Mac\" rather than \"Download Originals\".\n     \
              Photos still shows every picture, because it displays the\n     \
              previews it kept, so nothing looks wrong until the originals\n     \
              are actually needed. Check with File > Export > Export\n     \
              Unmodified Original. Do not delete the library first: what is\n     \
              only in iCloud is not coming back.\n\
         \n  \
           2. It is a referenced library, where Photos links to files kept\n     \
              elsewhere. Photos > Settings > General: if \"Copy items to the\n     \
              Photos library\" is off, then those files are ordinary photos\n     \
              already, with nothing to import. Use them directly:\n     \
              videre scan <path/to/photos>\n",
    )
}

pub(crate) fn referenced_warning(shape: &LibraryShape) -> String {
    format!(
        "warning: originals/ contains {} file(s), but the library is {}.\n  \
           This looks like a referenced library, where Photos links to files\n  \
           that live elsewhere rather than copying them in.\n  \
           Point videre at the folders holding your actual photos instead.",
        shape.files,
        human_bytes(shape.package_bytes),
    )
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape(sizes: &[u64], package_bytes: u64) -> LibraryShape {
        LibraryShape::from_sizes(sizes, package_bytes)
    }

    #[test]
    fn the_optimised_signal_is_the_median_not_any_single_file() {
        // Screenshots, downloads and old photos are genuinely small, so
        // classifying file by file would produce constant false positives.
        // Three tiny files among camera originals must not flip the verdict.
        let mut sizes = vec![3_000_000u64; 20];
        sizes.extend([12_000, 40_000, 8_000]);
        let library = shape(&sizes, 60_000_000);
        assert!(
            !library.looks_optimised(),
            "small individual files are normal, median {} bytes",
            library.median_bytes
        );

        let optimised = shape(&[180_000; 20], 3_600_000);
        assert!(
            optimised.looks_optimised(),
            "a whole library of device-sized stand-ins is the real signal"
        );
    }

    #[test]
    fn the_median_of_an_even_count_does_not_panic_or_overflow() {
        assert_eq!(shape(&[10, 20, 30, 40], 100).median_bytes, 20);
        assert_eq!(shape(&[u64::MAX, u64::MAX], 0).median_bytes, u64::MAX);
        assert_eq!(shape(&[], 0).median_bytes, 0);
        assert!(
            !shape(&[], 0).looks_optimised(),
            "no files is not optimised"
        );
    }

    #[test]
    fn a_completely_empty_originals_folder_is_not_asserted_to_be_referenced() {
        // Real library, 2026-08-12: empty originals/ with all 16 derivative
        // fan-out dirs. That shape is produced both by a referenced library
        // and by iCloud having evicted the originals, so claiming either one
        // as fact would be a guess, and the wrong guess is the one that ends
        // with the user deleting a library whose photos are only in iCloud.
        let s = shape(&[], 4_000_000_000);
        assert!(!s.looks_referenced());
        assert!(!s.looks_optimised());
        let msg = empty_originals_warning();
        assert!(msg.contains("iCloud"), "must name the data-losing cause");
        assert!(msg.contains("referenced"), "must name the other cause");
        assert!(
            msg.contains("videre scan"),
            "referenced files need scanning, not importing"
        );
    }

    #[test]
    fn a_nearly_empty_originals_folder_in_a_large_package_looks_referenced() {
        // A referenced library never copies files in, so originals/ holds
        // almost nothing while the package itself is otherwise complete.
        let referenced = shape(&[2_000; 12], 1_200_000_000);
        assert!(referenced.looks_referenced());

        let ordinary = shape(&[3_000_000; 40], 130_000_000);
        assert!(
            !ordinary.looks_referenced(),
            "a library whose originals are most of its bytes is not referenced"
        );
    }

    #[test]
    fn the_checklist_leads_with_the_only_step_that_loses_data() {
        let text = checklist(&shape(&[1], 1));
        let download = text.find("Download Originals").expect("must be present");
        let deleted = text.find("Recently Deleted").expect("must be present");
        assert!(
            download < deleted,
            "the irreversible step comes first: {text}"
        );
    }
}
