//! Batch rename via editor (spec F20): planning engine.
//!
//! The UI flow this module supports:
//! 1. [`to_editor_text`] serialises the selected names, one per line, into text the UI writes to a
//!    temp file and opens in the configured editor. Names that cannot round trip through an editor
//!    (not valid UTF-8, contain a newline, or end with a carriage return) are reported in
//!    [`EditorText::unsupported`] so the UI can exclude them and tell the user.
//! 2. After the editor exits, the UI reads the file back and calls [`parse_edited`].
//! 3. [`plan`] turns the resulting ops into an ordered [`Plan`]: [`Plan::preview`] feeds the
//!    preview dialog, [`Plan::steps`] is handed to the job engine, which executes them in order
//!    (one `rename(2)` per step; temp steps included — no locking needed because the plan order
//!    guarantees each step's `from` exists and its `to` does not).
//! 4. On success the caller records an [`UndoLog`] (see [`UndoLog::new`]) and persists it with
//!    [`save_undo_log`]. The undo command runs [`verify_undo`] (staleness check), shows
//!    [`undo_plan`]'s preview and executes its steps.
//!
//! All names are bytes ([`OsString`]); nothing here assumes UTF-8 except the documented editor
//! round trip. The only I/O in this module is [`save_undo_log`] / [`load_undo_log`], which touch
//! exactly the path the caller gives them.

use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Prefix of the temporary names inserted to resolve swaps, cycles and case-only renames.
/// Kept stable so the undo staleness check can recognise them.
pub const TEMP_PREFIX: &str = ".ofm-rename-";

/// Why a target name (one edited line) is not a valid single file name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum InvalidNameReason {
    /// The line is empty.
    #[error("empty name")]
    Empty,
    /// The line is `.`.
    #[error("'.' is not a file name")]
    Dot,
    /// The line is `..`.
    #[error("'..' is not a file name")]
    DotDot,
    /// The name contains `/` (would address a nested path or escape the directory).
    #[error("contains '/'")]
    Slash,
    /// The name contains a NUL byte.
    #[error("contains NUL")]
    Nul,
}

/// Everything that can go wrong while planning, parsing or persisting a batch rename.
#[derive(Debug, Error)]
pub enum BatchError {
    /// Names that cannot round trip through a text editor: not valid UTF-8, contain a newline,
    /// or end with a carriage return. The UI should exclude them from the editor buffer and
    /// tell the user they were not renamed.
    #[error(
        "names cannot be edited as text (not valid UTF-8, contain a newline, or end with CR): {0:?}"
    )]
    UnsupportedNames(Vec<OsString>),

    /// The edited text does not have exactly one line per original name.
    #[error(
        "edited name list has {actual} lines, expected {expected} ({added} added, {removed} removed)"
    )]
    LineCount {
        /// Number of original names.
        expected: usize,
        /// Number of lines in the edited text.
        actual: usize,
        /// Lines added by the user (`actual - expected` when positive).
        added: usize,
        /// Lines removed by the user (`expected - actual` when positive).
        removed: usize,
    },

    /// A line was changed to something that is not a valid single file name.
    #[error("invalid target name {name:?}: {reason}")]
    InvalidTarget {
        /// The offending line.
        name: OsString,
        /// Why it is invalid.
        reason: InvalidNameReason,
    },

    /// Two renames in the batch target the same name (compared per [`PlanOptions`]).
    #[error("duplicate target name in batch: {0:?}")]
    DuplicateTarget(OsString),

    /// Two renames in the batch start from the same name.
    #[error("duplicate source name in batch: {0:?}")]
    DuplicateSource(OsString),

    /// A rename targets a name that already exists in the directory and is not itself being
    /// renamed away in this batch.
    #[error("target {target:?} already exists and is not being renamed")]
    Collision {
        /// The existing name that would be overwritten.
        target: OsString,
    },

    /// [`verify_undo`] found the undo log does not match the directory contents: some file was
    /// renamed again, deleted, or a new file now occupies a name the undo needs.
    #[error("undo is no longer applicable: {0}")]
    StaleUndo(String),

    /// Internal invariant: a dependency cycle survived temp-name resolution. Indicates a bug in
    /// this module; surfaced instead of producing an unexecutable plan.
    #[error("internal error: unresolved rename cycle")]
    InternalCycle,

    /// I/O error while saving or loading the undo log.
    #[error("I/O on undo log {path:?}: {source}")]
    Io {
        /// File being accessed.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },

    /// The undo log failed to serialize or deserialize.
    #[error("undo log serialization failed: {0}")]
    Serialize(String),
}

/// Raw bytes of an OS string (exact on Unix; lossy elsewhere, which only affects the TOML undo
/// log round trip on non-Unix platforms — the plan itself always works on bytes).
#[cfg(unix)]
fn os_bytes(s: &OsStr) -> Cow<'_, [u8]> {
    use std::os::unix::ffi::OsStrExt;
    Cow::Borrowed(s.as_bytes())
}

/// Non-Unix fallback for [`os_bytes`].
#[cfg(not(unix))]
fn os_bytes(s: &OsStr) -> Cow<'_, [u8]> {
    Cow::Owned(s.to_string_lossy().into_owned().into_bytes())
}

/// Inverse of [`os_bytes`].
#[cfg(unix)]
fn os_from_bytes(bytes: Vec<u8>) -> OsString {
    use std::os::unix::ffi::OsStringExt;
    OsStringExt::from_vec(bytes)
}

/// Non-Unix fallback for [`os_from_bytes`].
#[cfg(not(unix))]
fn os_from_bytes(bytes: Vec<u8>) -> OsString {
    OsString::from(String::from_utf8_lossy(&bytes).into_owned())
}

/// Comparison key for a name; ASCII-lowercased when `ci` (case-insensitive target file system).
fn key(name: &OsStr, ci: bool) -> Vec<u8> {
    let bytes = os_bytes(name);
    if ci {
        bytes.iter().map(u8::to_ascii_lowercase).collect()
    } else {
        bytes.into_owned()
    }
}

/// The UTF-8 view of `name` if it can survive a round trip through a text editor.
fn editor_safe_str(name: &OsStr) -> Option<&str> {
    let s = name.to_str()?;
    if s.contains('\n') || s.ends_with('\r') {
        None
    } else {
        Some(s)
    }
}

/// Result of [`to_editor_text`]: the text for the editor buffer plus the names that could not be
/// represented in it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EditorText {
    /// One name per line, trailing newline included (editors expect it; [`parse_edited`] ignores
    /// it). Empty when every name is unsupported.
    pub text: String,
    /// Names the UI should leave out of the buffer (and not rename): not valid UTF-8, contain a
    /// newline, or end with a carriage return.
    pub unsupported: Vec<OsString>,
}

/// Serialises names into editor text, one per line.
///
/// Names that cannot round trip through an editor are not written to the buffer; they are
/// returned in [`EditorText::unsupported`] so the UI can exclude them (or offer another rename
/// path) instead of corrupting them.
pub fn to_editor_text(names: &[OsString]) -> EditorText {
    let mut out = EditorText::default();
    for name in names {
        if let Some(s) = editor_safe_str(name) {
            out.text.push_str(s);
            out.text.push('\n');
        } else {
            out.unsupported.push(name.clone());
        }
    }
    out
}

/// A single user-visible rename.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenameOp {
    /// Current name.
    #[serde(with = "serde_osstring")]
    pub from: OsString,
    /// Desired new name.
    #[serde(with = "serde_osstring")]
    pub to: OsString,
}

/// Parses the edited text back into rename ops.
///
/// `old` must be the same name list that was passed to [`to_editor_text`] (order matters: the
/// n-th line is the new name of the n-th file). The text must have exactly one line per name;
/// a single trailing newline is ignored and CRLF line endings are tolerated. Lines equal to the
/// original name produce no op. Changed lines are validated as single file names (non-empty, not
/// `.`/`..`, no `/`, no NUL).
///
/// Duplicate targets and collisions with existing directory entries are *not* reported here;
/// they depend on the directory contents and are detected by [`plan`].
pub fn parse_edited(old: &[OsString], text: &str) -> Result<Vec<RenameOp>, BatchError> {
    let unsafe_names: Vec<OsString> = old
        .iter()
        .filter(|n| editor_safe_str(n).is_none())
        .cloned()
        .collect();
    if !unsafe_names.is_empty() {
        return Err(BatchError::UnsupportedNames(unsafe_names));
    }

    let mut lines: Vec<&str> = if text.is_empty() {
        Vec::new()
    } else {
        let mut lines: Vec<&str> = text.split('\n').collect();
        if text.ends_with('\n') {
            lines.pop();
        }
        lines
    };
    for line in &mut lines {
        if let Some(stripped) = line.strip_suffix('\r') {
            *line = stripped;
        }
    }

    if lines.len() != old.len() {
        let (added, removed) = if lines.len() > old.len() {
            (lines.len() - old.len(), 0)
        } else {
            (0, old.len() - lines.len())
        };
        return Err(BatchError::LineCount {
            expected: old.len(),
            actual: lines.len(),
            added,
            removed,
        });
    }

    let mut ops = Vec::new();
    for (from, line) in old.iter().zip(lines) {
        // Old names were validated as editor-safe above.
        if editor_safe_str(from) == Some(line) {
            continue;
        }
        check_target(line)?;
        ops.push(RenameOp {
            from: from.clone(),
            to: OsString::from(line),
        });
    }
    Ok(ops)
}

/// Validates one edited line as a single file name.
fn check_target(name: &str) -> Result<(), BatchError> {
    let reason = if name.is_empty() {
        Some(InvalidNameReason::Empty)
    } else if name == "." {
        Some(InvalidNameReason::Dot)
    } else if name == ".." {
        Some(InvalidNameReason::DotDot)
    } else if name.contains('/') {
        Some(InvalidNameReason::Slash)
    } else if name.contains('\0') {
        Some(InvalidNameReason::Nul)
    } else {
        None
    };
    match reason {
        Some(reason) => Err(BatchError::InvalidTarget {
            name: OsString::from(name),
            reason,
        }),
        None => Ok(()),
    }
}

/// How a [`Step`] renames its `from`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepKind {
    /// Plain rename of a user-visible name.
    Direct,
    /// First half of a split rename: `from` goes to a temporary name.
    ToTemp,
    /// Second half of a split rename: a temporary name reaches its final target.
    FromTemp,
}

/// One executable rename. Steps are ordered so that at execution time `from` exists and `to`
/// does not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Step {
    /// Name at execution time (a temporary name for [`StepKind::FromTemp`]).
    #[serde(with = "serde_osstring")]
    pub from: OsString,
    /// Name after this step (a temporary name for [`StepKind::ToTemp`]).
    #[serde(with = "serde_osstring")]
    pub to: OsString,
    /// What kind of rename this is.
    pub kind: StepKind,
}

/// An executable rename plan.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Plan {
    /// Steps in execution order. Executing them sequentially with plain renames always works:
    /// swaps and cycles are split into temp steps and chains are ordered so no step overwrites a
    /// name that is still needed.
    pub steps: Vec<Step>,
    /// User-visible renames `(from, to)` in input order, for the preview dialog. Temp steps are
    /// not listed here.
    pub preview: Vec<(OsString, OsString)>,
}

/// Options for [`plan_with_options`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PlanOptions {
    /// Treat the target file system as case-insensitive (e.g. FAT/ExFOL, NTFS, macOS): renames
    /// that differ only in case (`a.TXT` -> `a.txt`) are routed through a temporary name, and
    /// name comparisons for duplicates/collisions ignore ASCII case. Default `false` (the Linux
    /// norm, where a case-only rename works directly).
    pub assume_case_insensitive: bool,
}

/// Plans a batch rename on a case-sensitive file system. See [`plan_with_options`].
pub fn plan(
    ops: &[RenameOp],
    existing_names_in_dir: &HashSet<OsString>,
) -> Result<Plan, BatchError> {
    plan_with_options(ops, existing_names_in_dir, &PlanOptions::default())
}

/// Validates `ops` against the directory contents and builds an executable [`Plan`].
///
/// `existing_names_in_dir` must contain **all** entries of the target directory (not only the
/// selected ones). Detected errors: duplicate sources, duplicate targets, and targets that
/// collide with an existing entry which is not itself being renamed away. Swaps (`a->b, b->a`),
/// cycles (`a->b, b->c, c->a`) and, with [`PlanOptions::assume_case_insensitive`], case-only
/// renames are split into unique temporary names ([`TEMP_PREFIX`]) so sequential execution never
/// loses data. Chains (`a->b, b->c`) are ordered so no step overwrites a name that is still
/// needed. No-op renames (`from == to`) are silently dropped.
pub fn plan_with_options(
    ops: &[RenameOp],
    existing_names_in_dir: &HashSet<OsString>,
    options: &PlanOptions,
) -> Result<Plan, BatchError> {
    let ci = options.assume_case_insensitive;

    // --- Validation (sources, targets, collisions) -------------------------
    let mut src_keys: HashSet<Vec<u8>> = HashSet::with_capacity(ops.len());
    let mut dst_keys: HashSet<Vec<u8>> = HashSet::with_capacity(ops.len());
    for op in ops {
        if !src_keys.insert(key(&op.from, ci)) {
            return Err(BatchError::DuplicateSource(op.from.clone()));
        }
        if !dst_keys.insert(key(&op.to, ci)) {
            return Err(BatchError::DuplicateTarget(op.to.clone()));
        }
    }
    let existing_keys: HashSet<Vec<u8>> =
        existing_names_in_dir.iter().map(|n| key(n, ci)).collect();
    for op in ops {
        let dk = key(&op.to, ci);
        if existing_keys.contains(&dk) && !src_keys.contains(&dk) {
            return Err(BatchError::Collision {
                target: op.to.clone(),
            });
        }
    }

    // --- Step construction -------------------------------------------------
    // `taken` seeds temp-name uniqueness with everything that must not be clobbered.
    let mut taken: HashSet<Vec<u8>> = existing_keys
        .iter()
        .chain(src_keys.iter())
        .chain(dst_keys.iter())
        .cloned()
        .collect();
    let mut temp_counter: u64 = 0;
    let mut steps: Vec<Step> = Vec::new();
    let mut temp_keys: HashSet<Vec<u8>> = HashSet::new();
    let mut preview: Vec<(OsString, OsString)> = Vec::new();
    for op in ops {
        if op.from == op.to {
            continue;
        }
        preview.push((op.from.clone(), op.to.clone()));
        if ci && key(&op.from, ci) == key(&op.to, ci) {
            let temp = make_temp(&mut taken, &mut temp_counter, ci);
            temp_keys.insert(key(&temp, ci));
            steps.push(Step {
                from: op.from.clone(),
                to: temp.clone(),
                kind: StepKind::ToTemp,
            });
            steps.push(Step {
                from: temp,
                to: op.to.clone(),
                kind: StepKind::FromTemp,
            });
        } else {
            steps.push(Step {
                from: op.from.clone(),
                to: op.to.clone(),
                kind: StepKind::Direct,
            });
        }
    }

    // --- Ordering ----------------------------------------------------------
    // Dependency rule between steps i (creates `to_i`) and j (renames `from_j == to_i` away):
    //   * regular name: j must run before i (rename away before recreating);
    //   * temp name:    i must run before j (create before consuming).
    // Cycles of the first kind are rename swaps/cycles; break them (and any residual cycle, as a
    // safety net) by temp-ing one direct step, then re-order.
    loop {
        match order_steps(&steps, &temp_keys, ci) {
            Some(order) => {
                steps = order.into_iter().map(|i| steps[i].clone()).collect();
                break;
            }
            None => {
                let leftover = leftover_steps(&steps, &temp_keys, ci);
                let victim = leftover
                    .iter()
                    .copied()
                    .find(|&i| steps[i].kind == StepKind::Direct)
                    .ok_or(BatchError::InternalCycle)?;
                let old = steps[victim].clone();
                let temp = make_temp(&mut taken, &mut temp_counter, ci);
                temp_keys.insert(key(&temp, ci));
                steps[victim] = Step {
                    from: old.from,
                    to: temp.clone(),
                    kind: StepKind::ToTemp,
                };
                steps.push(Step {
                    from: temp,
                    to: old.to,
                    kind: StepKind::FromTemp,
                });
            }
        }
    }

    Ok(Plan { steps, preview })
}

/// Generates a unique temporary name not colliding with `taken` (which grows with each temp).
fn make_temp(taken: &mut HashSet<Vec<u8>>, counter: &mut u64, ci: bool) -> OsString {
    loop {
        *counter += 1;
        let name = format!("{TEMP_PREFIX}{}-{counter}", std::process::id());
        if taken.insert(key(OsStr::new(&name), ci)) {
            return OsString::from(name);
        }
    }
}

/// Topological order of `steps` per the dependency rule in [`plan_with_options`], or `None` when
/// a cycle remains. Deterministic: ready steps are taken in index order.
fn order_steps(steps: &[Step], temp_keys: &HashSet<Vec<u8>>, ci: bool) -> Option<Vec<usize>> {
    let by_src: HashMap<Vec<u8>, usize> = steps
        .iter()
        .enumerate()
        .map(|(i, s)| (key(&s.from, ci), i))
        .collect();
    let mut succ: Vec<Vec<usize>> = vec![Vec::new(); steps.len()];
    let mut indeg = vec![0usize; steps.len()];
    for (i, s) in steps.iter().enumerate() {
        let tk = key(&s.to, ci);
        let Some(&j) = by_src.get(&tk) else { continue };
        if j == i {
            continue;
        }
        if temp_keys.contains(&tk) {
            succ[i].push(j);
            indeg[j] += 1;
        } else {
            succ[j].push(i);
            indeg[i] += 1;
        }
    }
    let mut ready: BTreeSet<usize> = (0..steps.len()).filter(|&i| indeg[i] == 0).collect();
    let mut order = Vec::with_capacity(steps.len());
    while let Some(&i) = ready.iter().next() {
        ready.remove(&i);
        order.push(i);
        for &k in &succ[i] {
            indeg[k] -= 1;
            if indeg[k] == 0 {
                ready.insert(k);
            }
        }
    }
    (order.len() == steps.len()).then_some(order)
}

/// Indices of steps that no topological order can place (i.e. cyclic or downstream of a cycle).
fn leftover_steps(steps: &[Step], temp_keys: &HashSet<Vec<u8>>, ci: bool) -> Vec<usize> {
    let by_src: HashMap<Vec<u8>, usize> = steps
        .iter()
        .enumerate()
        .map(|(i, s)| (key(&s.from, ci), i))
        .collect();
    let mut succ: Vec<Vec<usize>> = vec![Vec::new(); steps.len()];
    let mut indeg = vec![0usize; steps.len()];
    for (i, s) in steps.iter().enumerate() {
        let tk = key(&s.to, ci);
        let Some(&j) = by_src.get(&tk) else { continue };
        if j == i {
            continue;
        }
        if temp_keys.contains(&tk) {
            succ[i].push(j);
            indeg[j] += 1;
        } else {
            succ[j].push(i);
            indeg[i] += 1;
        }
    }
    // Kahn's algorithm; whatever keeps indeg > 0 is leftover.
    let mut ready: BTreeSet<usize> = (0..steps.len()).filter(|&i| indeg[i] == 0).collect();
    let mut placed = vec![false; steps.len()];
    while let Some(&i) = ready.iter().next() {
        ready.remove(&i);
        placed[i] = true;
        for &k in &succ[i] {
            indeg[k] -= 1;
            if indeg[k] == 0 {
                ready.insert(k);
            }
        }
    }
    (0..steps.len()).filter(|&i| !placed[i]).collect()
}

/// Record of a successfully executed batch rename, for undo (spec F20).
///
/// Serialize with [`save_undo_log`] / read back with [`load_undo_log`]; both take an explicit
/// path so the caller picks the state directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UndoLog {
    /// Directory the batch ran in; step names are relative to it.
    #[serde(with = "serde_path")]
    pub dir: PathBuf,
    /// Steps exactly as executed, in execution order.
    pub steps: Vec<Step>,
    /// The user-visible renames (`from` -> `to`) that `steps` implement, in input order.
    pub renames: Vec<RenameOp>,
}

impl UndoLog {
    /// Records a successfully executed `plan` in `dir` for later reversal via [`undo_plan`].
    pub fn new(dir: PathBuf, plan: &Plan) -> UndoLog {
        UndoLog {
            dir,
            steps: plan.steps.clone(),
            renames: plan
                .preview
                .iter()
                .map(|(from, to)| RenameOp {
                    from: from.clone(),
                    to: to.clone(),
                })
                .collect(),
        }
    }
}

/// Builds the plan that reverses an [`UndoLog`]: every step swapped and executed in reverse
/// order, which is always safe because it replays the forward run backwards. The preview shows
/// the user-visible renames in reverse.
pub fn undo_plan(log: &UndoLog) -> Plan {
    Plan {
        steps: log
            .steps
            .iter()
            .rev()
            .map(|s| Step {
                from: s.to.clone(),
                to: s.from.clone(),
                kind: s.kind,
            })
            .collect(),
        preview: log
            .renames
            .iter()
            .rev()
            .map(|r| (r.to.clone(), r.from.clone()))
            .collect(),
    }
}

/// Checks an [`UndoLog`] against the current directory contents before undoing.
///
/// Simulates the undo (every step's `to` renamed back to its `from`, in reverse order) against
/// `names_in_dir`: each undo step requires its `to` to be present and its `from` to be absent at
/// that point. If any check fails the undo is stale — something was renamed again, deleted, or a
/// new file now occupies a name the undo needs — and must not be attempted.
pub fn verify_undo(log: &UndoLog, names_in_dir: &HashSet<OsString>) -> Result<(), BatchError> {
    let mut sim: HashSet<Vec<u8>> = names_in_dir.iter().map(|n| key(n, false)).collect();
    for (i, step) in log.steps.iter().enumerate().rev() {
        let to = key(&step.to, false);
        let from = key(&step.from, false);
        if !sim.contains(&to) || sim.contains(&from) {
            return Err(BatchError::StaleUndo(format!(
                "step {i} ({:?} -> {:?}) does not match the directory contents",
                step.from, step.to
            )));
        }
        sim.remove(&to);
        sim.insert(from);
    }
    Ok(())
}

/// Serializes `log` to TOML and writes it to `path` atomically (temp file in the same directory,
/// fsync, rename; mode `0600`). Creates parent directories as needed. A failure leaves any
/// previous file at `path` untouched.
pub fn save_undo_log(log: &UndoLog, path: &Path) -> Result<(), BatchError> {
    let text = toml::to_string_pretty(log).map_err(|e| BatchError::Serialize(e.to_string()))?;
    atomic_write_private(path, &text)
}

/// Reads and parses the [`UndoLog`] written by [`save_undo_log`].
pub fn load_undo_log(path: &Path) -> Result<UndoLog, BatchError> {
    let text = std::fs::read_to_string(path).map_err(|source| BatchError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    toml::from_str(&text).map_err(|e| BatchError::Serialize(e.to_string()))
}

/// Atomic private write; mirrors `ofm-config`'s helper (ofm-core cannot depend on ofm-config).
fn atomic_write_private(path: &Path, contents: &str) -> Result<(), BatchError> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir).map_err(|source| BatchError::Io {
        path: dir.to_path_buf(),
        source,
    })?;
    let mut tmp_name = OsString::from(".");
    tmp_name.push(path.file_name().unwrap_or_default());
    tmp_name.push(format!(
        ".tmp.{}.{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let tmp = dir.join(tmp_name);
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, path)
    })();
    match result {
        Ok(()) => {
            if let Ok(d) = std::fs::File::open(dir) {
                let _ = d.sync_all();
            }
            Ok(())
        }
        Err(source) => {
            let _ = std::fs::remove_file(&tmp);
            Err(BatchError::Io {
                path: path.to_path_buf(),
                source,
            })
        }
    }
}

/// (De)serializes `OsString` as raw bytes so non-UTF-8 names survive the TOML undo log.
mod serde_osstring {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::ffi::OsString;

    use super::{os_bytes, os_from_bytes};

    pub fn serialize<S: Serializer>(value: &OsString, serializer: S) -> Result<S::Ok, S::Error> {
        os_bytes(value).into_owned().serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<OsString, D::Error> {
        let bytes: Vec<u8> = Vec::deserialize(deserializer)?;
        Ok(os_from_bytes(bytes))
    }
}

/// (De)serializes `PathBuf` as raw bytes (see [`serde_osstring`]).
mod serde_path {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::path::{Path, PathBuf};

    use super::{os_bytes, os_from_bytes};

    pub fn serialize<S: Serializer>(value: &Path, serializer: S) -> Result<S::Ok, S::Error> {
        os_bytes(value.as_os_str())
            .into_owned()
            .serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<PathBuf, D::Error> {
        let bytes: Vec<u8> = Vec::deserialize(deserializer)?;
        Ok(PathBuf::from(os_from_bytes(bytes)))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStringExt;

    fn os(s: &str) -> OsString {
        OsString::from(s)
    }

    fn bytes(v: &[u8]) -> OsString {
        OsString::from_vec(v.to_vec())
    }

    fn existing(names: &[&str]) -> HashSet<OsString> {
        names.iter().map(|n| os(n)).collect()
    }

    fn op(from: &str, to: &str) -> RenameOp {
        RenameOp {
            from: os(from),
            to: os(to),
        }
    }

    // --- to_editor_text / parse_edited -------------------------------------

    #[test]
    fn to_editor_text_one_name_per_line() {
        let text = to_editor_text(&[os("alpha"), os("beta.txt")]);
        assert_eq!(text.text, "alpha\nbeta.txt\n");
        assert!(text.unsupported.is_empty());
    }

    #[test]
    fn to_editor_text_reports_unsupported_names() {
        let text = to_editor_text(&[
            os("ok"),
            bytes(b"bad\nname"),
            bytes(b"\xff\xfe"),
            os("ends-cr\r"),
        ]);
        assert_eq!(text.text, "ok\n");
        assert_eq!(text.unsupported.len(), 3);
    }

    #[test]
    fn parse_roundtrip_unchanged_gives_no_ops() {
        let old = [os("a"), os("b")];
        let text = to_editor_text(&old);
        assert_eq!(parse_edited(&old, &text.text).unwrap(), Vec::new());
    }

    #[test]
    fn parse_tolerates_crlf_and_missing_trailing_newline() {
        let old = [os("a"), os("b")];
        assert_eq!(
            parse_edited(&old, "x\r\nb\r\n").unwrap(),
            vec![op("a", "x")]
        );
        assert_eq!(parse_edited(&old, "x\nb").unwrap(), vec![op("a", "x")]);
    }

    #[test]
    fn parse_reports_added_and_removed_lines() {
        let old = [os("a"), os("b")];
        match parse_edited(&old, "a\nb\nc\n").unwrap_err() {
            BatchError::LineCount {
                expected,
                actual,
                added,
                removed,
            } => assert_eq!((expected, actual, added, removed), (2, 3, 1, 0)),
            other => panic!("unexpected error: {other:?}"),
        }
        match parse_edited(&old, "a\n").unwrap_err() {
            BatchError::LineCount {
                expected,
                actual,
                added,
                removed,
            } => assert_eq!((expected, actual, added, removed), (2, 1, 0, 1)),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn parse_rejects_invalid_targets() {
        let old = [os("a")];
        for (line, reason) in [
            ("", InvalidNameReason::Empty),
            (".", InvalidNameReason::Dot),
            ("..", InvalidNameReason::DotDot),
            ("x/y", InvalidNameReason::Slash),
            ("x\u{0}y", InvalidNameReason::Nul),
        ] {
            match parse_edited(&old, &format!("{line}\n")).unwrap_err() {
                BatchError::InvalidTarget { name, reason: r } => {
                    assert_eq!(name, os(line));
                    assert_eq!(r, reason);
                }
                other => panic!("unexpected error for {line:?}: {other:?}"),
            }
        }
    }

    #[test]
    fn parse_rejects_unsupported_old_names() {
        let old = [os("a"), bytes(b"\xff")];
        assert!(matches!(
            parse_edited(&old, "a\nb\n"),
            Err(BatchError::UnsupportedNames(_))
        ));
    }

    #[test]
    fn parse_empty_text_matches_empty_selection() {
        assert_eq!(parse_edited(&[], "").unwrap(), Vec::new());
    }

    // --- plan ---------------------------------------------------------------

    #[test]
    fn plan_simple_rename_is_direct() {
        let plan = plan(&[op("a", "x")], &existing(&["a", "keep"])).unwrap();
        assert_eq!(
            plan.steps,
            vec![Step {
                from: os("a"),
                to: os("x"),
                kind: StepKind::Direct
            }]
        );
        assert_eq!(plan.preview, vec![(os("a"), os("x"))]);
    }

    #[test]
    fn plan_drops_noops_from_steps_and_preview() {
        let plan = plan(&[op("a", "a"), op("b", "x")], &existing(&["a", "b"])).unwrap();
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.preview, vec![(os("b"), os("x"))]);
    }

    #[test]
    fn plan_orders_chain_inner_first() {
        // a->b must run after b->c, or the rename of b to c would clobber... the reverse: b->c
        // frees the name b, then a->b can take it.
        let plan = plan(&[op("a", "b"), op("b", "c")], &existing(&["a", "b"])).unwrap();
        assert_eq!(plan.steps.len(), 2);
        assert_eq!(plan.steps[0].from, os("b"));
        assert_eq!(plan.steps[0].to, os("c"));
        assert_eq!(plan.steps[1].from, os("a"));
        assert_eq!(plan.steps[1].to, os("b"));
    }

    #[test]
    fn plan_swap_uses_temp_steps() {
        let plan = plan(&[op("a", "b"), op("b", "a")], &existing(&["a", "b"])).unwrap();
        // One temp split is enough: a->t, b->a, t->b.
        assert_eq!(plan.steps.len(), 3);
        assert_eq!(plan.steps[0].kind, StepKind::ToTemp);
        assert_eq!(plan.steps[1].kind, StepKind::Direct);
        assert_eq!(plan.steps[2].kind, StepKind::FromTemp);
        assert_eq!(plan.preview, vec![(os("a"), os("b")), (os("b"), os("a"))]);
        // Temp name is unique and prefixed.
        let temp = plan
            .steps
            .iter()
            .find(|s| s.kind == StepKind::ToTemp)
            .unwrap();
        assert!(temp.to.to_str().unwrap().starts_with(TEMP_PREFIX));
    }

    #[test]
    fn plan_three_cycle_executes_in_order() {
        let plan = plan(
            &[op("a", "b"), op("b", "c"), op("c", "a")],
            &existing(&["a", "b", "c"]),
        )
        .unwrap();
        assert_eq!(plan.steps.len(), 4, "{plan:?}");
        // Simulate execution on an in-memory name set.
        let mut fs: HashSet<OsString> = existing(&["a", "b", "c"]);
        for step in &plan.steps {
            assert!(fs.remove(&step.from), "step {:?} source missing", step);
            assert!(fs.insert(step.to.clone()), "step {:?} target taken", step);
        }
        assert_eq!(fs, existing(&["a", "b", "c"]));
    }

    #[test]
    fn plan_rejects_duplicate_targets() {
        assert!(matches!(
            plan(&[op("a", "x"), op("b", "x")], &existing(&["a", "b"])),
            Err(BatchError::DuplicateTarget(t)) if t == os("x")
        ));
    }

    #[test]
    fn plan_rejects_duplicate_sources() {
        assert!(matches!(
            plan(&[op("a", "x"), op("a", "y")], &existing(&["a"])),
            Err(BatchError::DuplicateSource(t)) if t == os("a")
        ));
    }

    #[test]
    fn plan_rejects_collision_with_unrelated_existing() {
        assert!(matches!(
            plan(&[op("a", "keep")], &existing(&["a", "keep"])),
            Err(BatchError::Collision { target }) if target == os("keep")
        ));
    }

    #[test]
    fn plan_allows_target_renamed_away_in_same_batch() {
        // b exists but is itself renamed, so a->b is legal.
        let plan = plan(&[op("a", "b"), op("b", "c")], &existing(&["a", "b"])).unwrap();
        assert_eq!(plan.steps.len(), 2);
    }

    #[test]
    fn plan_is_deterministic() {
        let ops = [op("a", "b"), op("b", "c"), op("c", "a"), op("d", "e")];
        let existing = existing(&["a", "b", "c", "d"]);
        let p1 = plan(&ops, &existing).unwrap();
        let p2 = plan(&ops, &existing).unwrap();
        assert_eq!(p1, p2);
    }

    #[test]
    fn case_only_rename_direct_by_default() {
        let plan = plan(&[op("a.TXT", "a.txt")], &existing(&["a.TXT"])).unwrap();
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.steps[0].kind, StepKind::Direct);
    }

    #[test]
    fn case_only_rename_via_temp_on_case_insensitive() {
        let opts = PlanOptions {
            assume_case_insensitive: true,
        };
        let plan =
            plan_with_options(&[op("a.TXT", "a.txt")], &existing(&["a.TXT"]), &opts).unwrap();
        assert_eq!(plan.steps.len(), 2);
        assert_eq!(plan.steps[0].kind, StepKind::ToTemp);
        assert_eq!(plan.steps[1].kind, StepKind::FromTemp);
    }

    #[test]
    fn case_insensitive_collision_detected() {
        let opts = PlanOptions {
            assume_case_insensitive: true,
        };
        assert!(matches!(
            plan_with_options(&[op("x", "README")], &existing(&["x", "readme"]), &opts),
            Err(BatchError::Collision { .. })
        ));
    }

    #[test]
    fn case_insensitive_swap_of_case_variants() {
        let opts = PlanOptions {
            assume_case_insensitive: true,
        };
        // Case-variant twins cannot coexist on a case-insensitive file system, so this is
        // reported as a duplicate source rather than planned.
        assert!(matches!(
            plan_with_options(&[op("a", "A"), op("A", "a")], &existing(&["a", "A"]), &opts),
            Err(BatchError::DuplicateSource(_))
        ));
        // A regular swap must still execute correctly with the CI option.
        let plan = plan_with_options(&[op("a", "b"), op("b", "a")], &existing(&["a", "b"]), &opts)
            .unwrap();
        let mut fs = existing(&["a", "b"]);
        for step in &plan.steps {
            assert!(fs.remove(&step.from));
            assert!(fs.insert(step.to.clone()));
        }
        assert_eq!(fs, existing(&["a", "b"]));
    }

    // --- undo ---------------------------------------------------------------

    #[test]
    fn undo_plan_reverses_steps_and_preview() {
        let plan = plan(&[op("a", "b"), op("b", "a")], &existing(&["a", "b"])).unwrap();
        let log = UndoLog::new(PathBuf::from("/tmp/dir"), &plan);
        let undo = undo_plan(&log);
        assert_eq!(undo.steps.len(), plan.steps.len());
        // Forward then undo on an in-memory set must restore the original state.
        let mut fs = existing(&["a", "b"]);
        for step in &plan.steps {
            assert!(fs.remove(&step.from));
            assert!(fs.insert(step.to.clone()));
        }
        verify_undo(&log, &fs).unwrap();
        for step in &undo.steps {
            assert!(fs.remove(&step.from), "undo step {:?} source missing", step);
            assert!(
                fs.insert(step.to.clone()),
                "undo step {:?} target taken",
                step
            );
        }
        assert_eq!(fs, existing(&["a", "b"]));
    }

    #[test]
    fn verify_undo_detects_stale_log() {
        let plan = plan(&[op("a", "b")], &existing(&["a"])).unwrap();
        let log = UndoLog::new(PathBuf::from("/tmp/dir"), &plan);
        // Someone renamed b to something else.
        assert!(matches!(
            verify_undo(&log, &existing(&["c"])),
            Err(BatchError::StaleUndo(_))
        ));
        // Someone recreated a.
        assert!(matches!(
            verify_undo(&log, &existing(&["a", "b"])),
            Err(BatchError::StaleUndo(_))
        ));
    }

    #[test]
    fn undo_log_survives_non_utf8_names_through_toml() {
        let plan = plan(&[op("a", "b")], &existing(&["a"])).unwrap();
        let mut log = UndoLog::new(PathBuf::from("/tmp/dir"), &plan);
        log.renames[0].from = bytes(b"\xff\xfenm");
        let text = toml::to_string_pretty(&log).unwrap();
        let back: UndoLog = toml::from_str(&text).unwrap();
        assert_eq!(back, log);
    }

    #[test]
    fn save_and_load_undo_log_roundtrip() {
        let plan = plan(&[op("a", "b"), op("b", "a")], &existing(&["a", "b"])).unwrap();
        let log = UndoLog::new(PathBuf::from("/data/dir"), &plan);
        let dir = std::env::temp_dir().join(format!("ofm-batch-rename-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state/undo.toml");
        save_undo_log(&log, &path).unwrap();
        assert_eq!(load_undo_log(&path).unwrap(), log);
        // Atomic write: no temp files left behind, mode 0600.
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
