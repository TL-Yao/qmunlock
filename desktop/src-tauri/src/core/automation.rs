use super::{DecryptOptions, DecryptResult, OutputMode};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stamp {
    length: u64,
    modified: u128,
}

pub fn stamp(path: &Path) -> Option<Stamp> {
    let m = std::fs::metadata(path).ok()?;
    if !m.is_file() || m.len() == 0 {
        return None;
    }
    Some(Stamp {
        length: m.len(),
        modified: m
            .modified()
            .ok()?
            .duration_since(UNIX_EPOCH)
            .ok()?
            .as_nanos(),
    })
}

// Resolve the nearest existing ancestor, including symlinks such as macOS /var.
// Appending the missing suffix gives the same identity before and after creation.
pub fn canonical_path(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    let mut ancestor = absolute.as_path();
    let mut suffix = Vec::new();
    loop {
        if let Ok(mut resolved) = std::fs::canonicalize(ancestor) {
            for part in suffix.iter().rev() {
                resolved.push(part);
            }
            return resolved;
        }
        let Some(name) = ancestor.file_name() else {
            return absolute;
        };
        suffix.push(name.to_os_string());
        let Some(parent) = ancestor.parent() else {
            return absolute;
        };
        ancestor = parent;
    }
}

fn identity(path: &Path) -> String {
    let value = canonical_path(path).to_string_lossy().into_owned();
    #[cfg(windows)]
    let value = value
        .strip_prefix(r"\\?\")
        .unwrap_or(&value)
        .replace('\\', "/")
        .to_lowercase();
    value
}

pub fn job_key(input: &Path, output: Option<&Path>, options: &DecryptOptions) -> String {
    let folder = output.or_else(|| input.parent()).unwrap_or(Path::new("."));
    serde_json::json!([
        identity(input),
        identity(folder),
        matches!(options.output_mode, OutputMode::Mp3),
        options.output_template,
        options.embed_cover,
        options.fetch_lyrics,
        options
            .lyrics_dir
            .as_deref()
            .map(|p| identity(Path::new(p))),
        options.embed_synced_lyrics,
        options.plain_copy
    ])
    .to_string()
}

#[derive(Clone, Serialize, Deserialize)]
struct Record {
    source: Stamp,
    result: DecryptResult,
    #[serde(default)]
    updated: u128,
}

pub const HISTORY_LIMIT: usize = 10_000;
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct History {
    records: BTreeMap<String, Record>,
    #[serde(skip)]
    inputs: BTreeSet<String>,
    #[serde(skip)]
    output_results: BTreeMap<String, DecryptResult>,
    #[serde(skip)]
    output_paths: BTreeSet<PathBuf>,
}

impl History {
    fn index(&mut self, result: &DecryptResult) {
        let source = identity(Path::new(&result.input));
        self.inputs.insert(source.clone());
        if let Some(output) = result.output.as_deref() {
            let path = canonical_path(Path::new(output));
            let destination = identity(&path);
            if source != destination {
                self.output_results.insert(destination, result.clone());
                self.output_paths.insert(path);
            }
        }
    }
    fn reindex(&mut self) {
        self.inputs.clear();
        self.output_results.clear();
        self.output_paths.clear();
        let results: Vec<_> = self.records.values().map(|r| r.result.clone()).collect();
        for result in results {
            self.index(&result);
        }
    }
    pub fn lookup_output(&self, input: &Path) -> Option<DecryptResult> {
        let mut result = self.output_results.get(&identity(input))?.clone();
        stamp(input)?;
        result.input = input.display().to_string();
        result.output = Some(result.input.clone());
        result.skipped = true;
        Some(result)
    }
    pub fn knows_input(&self, input: &Path) -> bool {
        self.inputs.contains(&identity(input))
    }
    pub fn load(path: &Path) -> Self {
        let mut history: Self = std::fs::read(path)
            .ok()
            .and_then(|s| serde_json::from_slice(&s).ok())
            .unwrap_or_default();
        // Migrate keys written before non-existent paths were canonicalized.
        history.records = std::mem::take(&mut history.records)
            .into_iter()
            .map(|(key, record)| {
                let key = serde_json::from_str::<Vec<serde_json::Value>>(&key)
                    .ok()
                    .map(|mut parts| {
                        for index in [0, 1, 6] {
                            if let Some(value) = parts.get(index).and_then(|v| v.as_str()) {
                                parts[index] =
                                    serde_json::Value::String(identity(Path::new(value)));
                            }
                        }
                        serde_json::to_string(&parts).unwrap_or(key.clone())
                    })
                    .unwrap_or(key);
                (key, record)
            })
            .collect();
        history.prune();
        history
    }
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let temporary = path.with_extension("tmp");
        std::fs::write(&temporary, serde_json::to_vec(self)?)?;
        std::fs::rename(temporary, path)
    }
    pub fn prune(&mut self) {
        self.records.retain(|_, r| {
            r.result
                .output
                .as_deref()
                .and_then(|p| stamp(Path::new(p)))
                .is_some()
        });
        if self.records.len() > HISTORY_LIMIT {
            let mut oldest: Vec<_> = self
                .records
                .iter()
                .map(|(key, r)| (r.updated, key.clone()))
                .collect();
            oldest.sort();
            for (_, key) in oldest.into_iter().take(self.records.len() - HISTORY_LIMIT) {
                self.records.remove(&key);
            }
        }
        self.reindex();
    }
    pub fn lookup(&self, key: &str, input: &Path) -> Option<DecryptResult> {
        let record = self.records.get(key)?;
        if stamp(input).as_ref() != Some(&record.source) {
            return None;
        }
        stamp(Path::new(record.result.output.as_deref()?))?;
        let mut result = record.result.clone();
        result.input = input.display().to_string();
        result.skipped = true;
        Some(result)
    }
    pub fn record(&mut self, key: String, input: &Path, result: &DecryptResult) {
        // Repeated skips do not refresh eviction age; index legacy outputs once.
        let source_id = identity(input);
        let known =
            self.inputs.contains(&source_id) || self.output_results.contains_key(&source_id);
        if result.ok
            && (!result.skipped || !known)
            && result
                .output
                .as_deref()
                .and_then(|p| stamp(Path::new(p)))
                .is_some()
        {
            if let Some(source) = stamp(input) {
                self.records.insert(
                    key,
                    Record {
                        source,
                        result: result.clone(),
                        updated: std::time::SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_nanos(),
                    },
                );
                self.index(result);
            }
        }
    }
    // These canonical paths are indexed once on load/update, never during a poll.
    pub fn outputs(&self) -> Vec<PathBuf> {
        self.output_paths.iter().cloned().collect()
    }
}

fn collect(
    root: &Path,
    excluded: &BTreeSet<PathBuf>,
    found: &mut BTreeMap<PathBuf, Stamp>,
    unreadable: &mut Vec<PathBuf>,
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(root)? {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                unreadable.push(root.to_path_buf());
                continue;
            }
        };
        let path = entry.path();
        let kind = match entry.file_type() {
            Ok(kind) => kind,
            Err(_) => {
                unreadable.push(path);
                continue;
            }
        };
        if kind.is_symlink() || path.ancestors().any(|p| excluded.contains(p)) {
            continue;
        }
        if kind.is_dir() {
            if collect(&path, excluded, found, unreadable).is_err() {
                unreadable.push(path);
            }
        } else if super::supported_path(&path) {
            if let Some(s) = stamp(&path) {
                found.insert(path, s);
            }
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WatchFile {
    pub path: String,
    pub version: String,
}
fn version(stamp: &Stamp) -> String {
    format!("{}:{}", stamp.length, stamp.modified)
}

pub struct FolderWatcher {
    root: PathBuf,
    observed: BTreeMap<PathBuf, Stamp>,
    delivered: BTreeMap<PathBuf, Stamp>,
    offered: BTreeMap<PathBuf, Stamp>,
}

impl FolderWatcher {
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn new(root: &Path) -> std::io::Result<Self> {
        let root = std::fs::canonicalize(root)?;
        let mut observed = BTreeMap::new();
        collect(&root, &BTreeSet::new(), &mut observed, &mut Vec::new())?;
        Ok(Self {
            root,
            delivered: observed.clone(),
            observed,
            offered: BTreeMap::new(),
        })
    }
    pub fn acknowledge(&mut self, files: &[WatchFile]) {
        for file in files {
            let path = PathBuf::from(&file.path);
            if let Some(offered) = self.offered.get(&path) {
                if version(offered) == file.version && stamp(&path).as_ref() == Some(offered) {
                    self.delivered.insert(path.clone(), offered.clone());
                    self.offered.remove(&path);
                }
            }
        }
    }
    pub fn poll(&mut self, excluded: &[PathBuf]) -> std::io::Result<Vec<WatchFile>> {
        let mut now = BTreeMap::new();
        let mut unreadable = Vec::new();
        let excluded = excluded.iter().cloned().collect();
        collect(&self.root, &excluded, &mut now, &mut unreadable)?;
        let scanned: BTreeSet<_> = now.keys().cloned().collect();
        // Keep versions under temporarily inaccessible subdirectories, so recovery
        // does not turn already delivered files into new downloads.
        for (path, value) in &self.observed {
            if unreadable.iter().any(|p| path.starts_with(p)) {
                now.entry(path.clone()).or_insert_with(|| value.clone());
            }
        }
        self.delivered.retain(|p, _| now.contains_key(p));
        self.offered.retain(|p, s| now.get(p) == Some(s));
        let mut ready = Vec::new();
        for (path, value) in &now {
            if !scanned.contains(path) {
                continue;
            }
            if self.observed.get(path) == Some(value) && self.delivered.get(path) != Some(value) {
                // An encrypted download paused midway must not reach the UI until
                // a valid footer exists. Plain audio under QQ extensions is allowed.
                if super::enc_path(path)
                    && super::decrypt::info(path).is_err()
                    && super::sniff_plain_format(path).is_none()
                {
                    continue;
                }
                ready.push(WatchFile {
                    path: path.display().to_string(),
                    version: version(value),
                });
                self.offered.insert(path.clone(), value.clone());
            }
        }
        self.observed = now;
        Ok(ready)
    }
}

/// Also recognize same-name outputs produced before processing history was introduced.
pub fn legacy_output(
    input: &Path,
    output: Option<&Path>,
    options: &DecryptOptions,
) -> Option<DecryptResult> {
    if options
        .output_template
        .as_deref()
        .is_some_and(|s| !s.trim().is_empty())
    {
        return None;
    }
    let extension = input.extension()?.to_str()?.to_ascii_lowercase();
    let format = if matches!(options.output_mode, OutputMode::Mp3) {
        "mp3"
    } else {
        match extension.as_str() {
            "mgg" => "ogg",
            "mflac" => "flac",
            "mmp4" => "m4a",
            other => other,
        }
    };
    let dest = output
        .or_else(|| input.parent())?
        .join(input.file_stem()?)
        .with_extension(format);
    if identity(&dest) == identity(input) || super::sniff_plain_format(&dest) != Some(format) {
        return None;
    }
    stamp(&dest)?;
    Some(DecryptResult {
        input: input.display().to_string(),
        output: Some(dest.display().to_string()),
        ok: true,
        skipped: true,
        format: Some(format.into()),
        error: None,
        cover: None,
        lyrics: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn directory(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("qm-automation-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }
    #[test]
    fn watcher_waits_for_stable_versions_and_ignores_outputs() {
        let dir = directory("watch");
        std::fs::write(dir.join("old.mgg"), b"old").unwrap();
        let mut watcher = FolderWatcher::new(&dir).unwrap();
        let input = watcher.root.join("new.mgg");
        std::fs::write(&input, b"OggSa").unwrap();
        assert!(watcher.poll(&[]).unwrap().is_empty());
        std::fs::write(&input, b"OggSlonger").unwrap();
        assert!(watcher.poll(&[]).unwrap().is_empty());
        let offered = watcher.poll(&[]).unwrap();
        assert_eq!(offered.len(), 1);
        assert_eq!(offered[0].path, input.display().to_string());
        assert_eq!(watcher.poll(&[]).unwrap().len(), 1); // Retry until accepted.
        watcher.acknowledge(&offered);
        assert!(watcher.poll(&[]).unwrap().is_empty());
        let output = watcher.root.join("new.mp3");
        std::fs::write(&output, b"ID3output").unwrap();
        assert!(watcher
            .poll(std::slice::from_ref(&output))
            .unwrap()
            .is_empty());
        assert!(watcher.poll(&[output]).unwrap().is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn history_rechecks_source_and_output_and_survives_restart() {
        let dir = directory("history");
        let input = dir.join("source.mgg");
        let output = dir.join("out.mp3");
        std::fs::write(&input, b"source").unwrap();
        std::fs::write(&output, b"output").unwrap();
        let result = DecryptResult {
            input: input.display().to_string(),
            output: Some(output.display().to_string()),
            ok: true,
            skipped: false,
            format: Some("mp3".into()),
            error: None,
            cover: None,
            lyrics: None,
        };
        let mut history = History::default();
        history.record("key".into(), &input, &result);
        let store = dir.join("history.json");
        history.save(&store).unwrap();
        let history = History::load(&store);
        assert!(history.lookup("key", &input).unwrap().skipped);
        assert!(history.lookup_output(&output).unwrap().skipped);
        assert!(history.lookup_output(&input).is_none());
        let serialized = std::fs::read_to_string(&store).unwrap();
        assert!(!serialized.contains("manual_ekey"));
        std::fs::remove_file(&output).unwrap();
        assert!(history.lookup("key", &input).is_none());
        std::fs::write(&output, b"output").unwrap();
        std::fs::write(&input, b"changed source").unwrap();
        assert!(history.lookup("key", &input).is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn options() -> DecryptOptions {
        DecryptOptions {
            output_mode: OutputMode::Original,
            manual_ekey: None,
            skip_existing: true,
            output_template: None,
            embed_cover: false,
            fetch_lyrics: false,
            lyrics_dir: None,
            embed_synced_lyrics: false,
            plain_copy: false,
        }
    }

    #[test]
    fn output_directory_creation_does_not_change_key_and_options_invalidate_it() {
        let dir = directory("keys");
        let input = dir.join("song.mgg");
        std::fs::write(&input, b"source").unwrap();
        let out = dir.join("new-output");
        let mut options = options();
        let key = job_key(&input, Some(&out), &options);
        std::fs::create_dir_all(&out).unwrap();
        assert_eq!(key, job_key(&input, Some(&out), &options));
        options.manual_ekey = Some("never-persist-this".into());
        assert_eq!(key, job_key(&input, Some(&out), &options));
        options.output_template = Some("{artist} - {title}".into());
        assert_ne!(key, job_key(&input, Some(&out), &options));
        options.output_template = None;
        options.output_mode = OutputMode::Mp3;
        assert_ne!(key, job_key(&input, Some(&out), &options));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn legacy_skip_requires_a_valid_separate_output() {
        let dir = directory("legacy");
        let input = dir.join("song.mgg");
        std::fs::write(&input, b"encrypted source").unwrap();
        let output = dir.join("song.ogg");
        std::fs::write(&output, b"invalid").unwrap();
        assert!(legacy_output(&input, None, &options()).is_none());
        std::fs::write(&output, b"OggS\0\x02 audio padding padding").unwrap();
        assert!(legacy_output(&input, None, &options()).unwrap().skipped);
        assert!(legacy_output(&output, None, &options()).is_none());
        let mut template = options();
        template.output_template = Some("{title}".into());
        assert!(legacy_output(&input, None, &template).is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn watcher_handles_updates_deletion_and_recreation() {
        let dir = directory("updates");
        let file = dir.join("song.mgg");
        std::fs::write(&file, b"old").unwrap();
        let mut watcher = FolderWatcher::new(&dir).unwrap();
        std::fs::write(&file, b"OggSnew version").unwrap();
        assert!(watcher.poll(&[]).unwrap().is_empty());
        assert_eq!(watcher.poll(&[]).unwrap().len(), 1);
        std::fs::remove_file(&file).unwrap();
        assert!(watcher.poll(&[]).unwrap().is_empty());
        std::fs::write(&file, b"OggSanother version").unwrap();
        assert!(watcher.poll(&[]).unwrap().is_empty());
        assert_eq!(watcher.poll(&[]).unwrap().len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn identity_resolves_symlink_ancestor_before_nested_output_exists() {
        let dir = directory("symlink");
        let real = dir.join("real");
        let alias = dir.join("alias");
        std::fs::create_dir_all(&real).unwrap();
        std::os::unix::fs::symlink(&real, &alias).unwrap();
        let source = alias.join("song.mgg");
        std::fs::write(&source, b"source").unwrap();
        let output = alias.join("QM Unlock 输出/nested");
        let key = job_key(&source, Some(&output), &options());
        assert_eq!(
            identity(&output),
            identity(&real.join("QM Unlock 输出/nested"))
        );
        std::fs::create_dir_all(&output).unwrap();
        assert_eq!(key, job_key(&source, Some(&output), &options()));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn watcher_requires_valid_format_and_ack_of_unchanged_version() {
        let dir = directory("receipts");
        let mut watcher = FolderWatcher::new(&dir).unwrap();
        let input = watcher.root.join("paused.mgg");
        std::fs::write(&input, b"incomplete encrypted download").unwrap();
        for _ in 0..4 {
            assert!(watcher.poll(&[]).unwrap().is_empty());
        }
        std::fs::write(&input, b"OggScompleted").unwrap();
        assert!(watcher.poll(&[]).unwrap().is_empty());
        let first = watcher.poll(&[]).unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(watcher.poll(&[]).unwrap().len(), 1); // Simulated failed frontend scan.
        std::fs::write(&input, b"OggSnext longer version").unwrap();
        watcher.acknowledge(&first); // Stale acknowledgement must not suppress an update.
        assert!(watcher.poll(&[]).unwrap().is_empty());
        let second = watcher.poll(&[]).unwrap();
        assert_eq!(second.len(), 1);
        assert_ne!(first[0].version, second[0].version);
        watcher.acknowledge(&second);
        assert!(watcher.poll(&[]).unwrap().is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_subdirectory_does_not_interrupt_sibling_delivery() {
        use std::os::unix::fs::PermissionsExt;
        let dir = directory("permission");
        let blocked = dir.join("temporary");
        std::fs::create_dir_all(&blocked).unwrap();
        std::fs::write(blocked.join("already-delivered.mgg"), b"OggSold").unwrap();
        let mut watcher = FolderWatcher::new(&dir).unwrap();
        let ready = watcher.root.join("ready.mgg");
        std::fs::write(&ready, b"OggSready").unwrap();
        std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o000)).unwrap();
        // Also runs under privileged test runners; siblings must still be scanned.
        assert!(watcher.poll(&[]).unwrap().is_empty());
        let offered = watcher.poll(&[]).unwrap();
        assert_eq!(offered.len(), 1);
        watcher.acknowledge(&offered);
        std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(watcher.poll(&[]).unwrap().is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn history_prunes_deleted_outputs_and_caps_old_records() {
        let dir = directory("prune");
        let input = dir.join("source.mgg");
        let output = dir.join("song.mp3");
        std::fs::write(&input, b"source").unwrap();
        std::fs::write(&output, b"output").unwrap();
        let result = DecryptResult {
            input: input.display().to_string(),
            output: Some(output.display().to_string()),
            ok: true,
            skipped: false,
            format: Some("mp3".into()),
            error: None,
            cover: None,
            lyrics: None,
        };
        let mut history = History::default();
        for n in 0..HISTORY_LIMIT + 2 {
            history.records.insert(
                format!("record-{n}"),
                Record {
                    source: stamp(&input).unwrap(),
                    result: result.clone(),
                    updated: n as u128,
                },
            );
        }
        history.prune();
        assert_eq!(history.records.len(), HISTORY_LIMIT);
        assert!(!history.records.contains_key("record-0"));
        assert!(!history.records.contains_key("record-1"));
        assert_eq!(history.outputs().len(), 1);
        std::fs::remove_file(output).unwrap();
        history.prune();
        assert!(history.records.is_empty());
        assert!(history.outputs().is_empty());
        assert!(!history.knows_input(&input));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
