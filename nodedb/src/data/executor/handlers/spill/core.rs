//! Generic spill-to-disk file I/O shared by all GROUP BY spillers.
//!
//! `SpillCore<K, V>` handles serializing an in-memory map to temp files and
//! k-way merging them back.  All application-specific logic (governor
//! integration, feed routing, merge semantics) lives in the typed wrappers
//! in `groupby.rs` and `columnar.rs`.

use std::collections::HashMap;
use std::fs::File;
use std::hash::Hash;
use std::io::{BufWriter, Read as _, Seek as _, SeekFrom, Write as _};
use std::marker::PhantomData;
use std::path::PathBuf;

/// Maximum output cardinality as a multiple of the in-memory cap.
///
/// Grace-hash recursive partitioning is deferred to v0.2.0; a deterministic
/// error is returned rather than OOMing.
const FINALIZE_CAP_FACTOR: usize = 10;

/// Generic spill-to-disk manager for a `HashMap<K, V>`.
///
/// Spill runs are serialized as JSON (serde_json) and stored in temp files
/// inside `spill_dir`.  On `merge()`, all runs plus any remaining in-memory
/// entries are folded together using a caller-supplied merge function.
pub(super) struct SpillCore<K, V> {
    spill_dir: PathBuf,
    runs: Vec<File>,
    pub(super) spilled_runs: u64,
    _marker: PhantomData<(K, V)>,
}

impl<K, V> SpillCore<K, V>
where
    K: serde::Serialize + serde::de::DeserializeOwned + Eq + Hash,
    V: serde::Serialize + serde::de::DeserializeOwned,
{
    pub(super) fn new(spill_dir: PathBuf) -> crate::Result<Self> {
        std::fs::create_dir_all(&spill_dir).map_err(|e| crate::Error::Storage {
            engine: "groupby_spill".into(),
            detail: format!("failed to create spill dir {}: {e}", spill_dir.display()),
        })?;
        Ok(Self {
            spill_dir,
            runs: Vec::new(),
            spilled_runs: 0,
            _marker: PhantomData,
        })
    }

    /// Serialize `entries` to a temp file and append to the run list.
    ///
    /// Returns immediately without writing if `entries` is empty.
    pub(super) fn flush_run(&mut self, entries: impl Iterator<Item = (K, V)>) -> crate::Result<()> {
        let entries: Vec<(K, V)> = entries.collect();
        if entries.is_empty() {
            return Ok(());
        }

        let encoded = serde_json::to_vec(&entries).map_err(|e| crate::Error::Storage {
            engine: "groupby_spill".into(),
            detail: format!("spill serialize error: {e}"),
        })?;

        let mut file =
            tempfile::tempfile_in(&self.spill_dir).map_err(|e| crate::Error::Storage {
                engine: "groupby_spill".into(),
                detail: format!("failed to create spill temp file: {e}"),
            })?;

        {
            let mut writer = BufWriter::new(&mut file);
            writer
                .write_all(&encoded)
                .map_err(|e| crate::Error::Storage {
                    engine: "groupby_spill".into(),
                    detail: format!("spill write error: {e}"),
                })?;
            writer.flush().map_err(|e| crate::Error::Storage {
                engine: "groupby_spill".into(),
                detail: format!("spill flush error: {e}"),
            })?;
        }

        file.seek(SeekFrom::Start(0))
            .map_err(|e| crate::Error::Storage {
                engine: "groupby_spill".into(),
                detail: format!("spill seek error: {e}"),
            })?;

        self.runs.push(file);
        self.spilled_runs += 1;
        Ok(())
    }

    /// Merge all spill runs and the remaining `in_mem` entries into a single
    /// consolidated `HashMap<K, V>`.
    ///
    /// `merge_fn(dst, src)` is called when `src`'s key already exists in the
    /// output.  Returns `Err` if the final cardinality exceeds
    /// `cap × FINALIZE_CAP_FACTOR`.
    pub(super) fn merge<F>(
        mut self,
        in_mem: &mut HashMap<K, V>,
        cap: usize,
        merge_fn: F,
    ) -> crate::Result<HashMap<K, V>>
    where
        F: Fn(&mut V, V),
    {
        let output_cap = cap.saturating_mul(FINALIZE_CAP_FACTOR);
        let mut output: HashMap<K, V> = HashMap::new();

        for mut run_file in self.runs.drain(..) {
            run_file
                .seek(SeekFrom::Start(0))
                .map_err(|e| crate::Error::Storage {
                    engine: "groupby_spill".into(),
                    detail: format!("spill run seek error: {e}"),
                })?;

            let mut buf = Vec::new();
            run_file
                .read_to_end(&mut buf)
                .map_err(|e| crate::Error::Storage {
                    engine: "groupby_spill".into(),
                    detail: format!("spill run read error: {e}"),
                })?;

            let entries: Vec<(K, V)> =
                serde_json::from_slice(&buf).map_err(|e| crate::Error::Storage {
                    engine: "groupby_spill".into(),
                    detail: format!("spill run deserialize error: {e}"),
                })?;

            merge_entries(&mut output, entries, output_cap, &merge_fn)?;
        }

        let in_mem_entries: Vec<(K, V)> = in_mem.drain().collect();
        merge_entries(&mut output, in_mem_entries, output_cap, &merge_fn)?;

        Ok(output)
    }
}

fn merge_entries<K, V, F>(
    output: &mut HashMap<K, V>,
    entries: Vec<(K, V)>,
    output_cap: usize,
    merge_fn: &F,
) -> crate::Result<()>
where
    K: Eq + Hash,
    F: Fn(&mut V, V),
{
    for (key, value) in entries {
        if output.len() >= output_cap && !output.contains_key(&key) {
            return Err(crate::Error::Storage {
                engine: "groupby_spill".into(),
                detail: format!(
                    "finalized group cardinality exceeds {FINALIZE_CAP_FACTOR}x cap \
                     ({output_cap}), query result cardinality limit reached"
                ),
            });
        }
        match output.entry(key) {
            std::collections::hash_map::Entry::Occupied(mut e) => {
                merge_fn(e.get_mut(), value);
            }
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert(value);
            }
        }
    }
    Ok(())
}

impl<K, V> Drop for SpillCore<K, V> {
    fn drop(&mut self) {
        // tempfile handles auto-delete each file via OS-level unlink on close.
        self.runs.clear();
        if let Err(e) = std::fs::remove_dir(&self.spill_dir)
            && self.spill_dir.exists()
        {
            tracing::warn!(
                dir = %self.spill_dir.display(),
                error = %e,
                "groupby_spill: could not remove spill directory"
            );
        }
    }
}
