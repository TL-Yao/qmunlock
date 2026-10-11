use super::{DecryptOptions, DecryptResult, OutputMode};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
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

fn identity(path: &Path) -> String {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    let value = std::fs::canonicalize(&absolute)
        .unwrap_or(absolute)
        .to_string_lossy()
        .into_owned();
    #[cfg(windows)]
    let value = value
        .strip_prefix(r"\\?\")
        .unwrap_or(&value)
        .replace('\\', "/")
        .to_lowercase();
    value
}

// No keys or credentials are serialized. Options affecting the output invalidate a record.
pub fn job_key(input: &Path, output: Option<&Path>, options: &DecryptOptions) -> String {
    let folder = output.or_else(|| input.parent()).unwrap_or(Path::new("."));
    serde_json::json!([
        identity(input),
        identity(folder),
        matches!(options.output_mode, OutputMode::Mp3),
        options.output_template,
        options.embed_cover,
        options.fetch_lyrics,
        options.lyrics_dir,
        options.embed_synced_lyrics,
        options.plain_copy
    ])
    .to_string()
}

#[derive(Serialize, Deserialize)]
struct Record {
    source: Stamp,
    result: DecryptResult,
}

#[derive(Default, Serialize, Deserialize)]
pub struct History {
    records: BTreeMap<String, Record>,
}

impl History {
    pub fn lookup_output(&self, input: &Path) -> Option<DecryptResult> {
        let record = self.records.values().find(|r| {
            r.result.output.as_deref().is_some_and(|p| {
                identity(Path::new(p)) == identity(input)
                    && identity(Path::new(&r.result.input)) != identity(input)
            })
        })?;
        stamp(input)?;
        let mut result = record.result.clone();
        result.input = input.display().to_string();
        result.output = Some(result.input.clone());
        result.skipped = true;
        Some(result)
    }
    pub fn knows_input(&self, input: &Path) -> bool {
        self.records
            .values()
            .any(|r| identity(Path::new(&r.result.input)) == identity(input))
    }
    pub fn load(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|s| serde_json::from_slice(&s).ok())
            .unwrap_or_default()
    }
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let temporary = path.with_extension("tmp");
        std::fs::write(&temporary, serde_json::to_vec(self)?)?;
        std::fs::rename(temporary, path)
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
        if result.ok
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
                    },
                );
            }
        }
    }
    pub fn outputs(&self) -> Vec<PathBuf> {
        self.records
            .values()
            .filter_map(|r| {
                r.result
                    .output
                    .as_deref()
                    .filter(|p| identity(Path::new(p)) != identity(Path::new(&r.result.input)))
                    .and_then(|p| std::fs::canonicalize(p).ok())
            })
            .collect()
    }
}

fn collect(
    root: &Path,
    excluded: &[PathBuf],
    found: &mut BTreeMap<PathBuf, Stamp>,
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let path = entry.path();
        if kind.is_symlink() || excluded.iter().any(|p| path == *p || path.starts_with(p)) {
            continue;
        }
        if kind.is_dir() {
            collect(&path, excluded, found)?;
        } else if super::supported_path(&path) {
            if let Some(s) = stamp(&path) {
                found.insert(path, s);
            }
        }
    }
    Ok(())
}

pub struct FolderWatcher {
    root: PathBuf,
    observed: BTreeMap<PathBuf, Stamp>,
    delivered: BTreeMap<PathBuf, Stamp>,
}

impl FolderWatcher {
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn new(root: &Path) -> std::io::Result<Self> {
        let root = std::fs::canonicalize(root)?;
        let mut observed = BTreeMap::new();
        collect(&root, &[], &mut observed)?;
        Ok(Self {
            root,
            delivered: observed.clone(),
            observed,
        })
    }
    // A version must stay unchanged for two polls before it can be delivered.
    pub fn poll(&mut self, excluded: &[PathBuf]) -> std::io::Result<Vec<String>> {
        let mut now = BTreeMap::new();
        collect(&self.root, excluded, &mut now)?;
        let mut ready = Vec::new();
        self.delivered.retain(|p, _| now.contains_key(p));
        for (path, value) in &now {
            if self.observed.get(path) == Some(value) && self.delivered.get(path) != Some(value) {
                ready.push(path.display().to_string());
                self.delivered.insert(path.clone(), value.clone());
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
        std::fs::write(&input, b"a").unwrap();
        assert!(watcher.poll(&[]).unwrap().is_empty());
        std::fs::write(&input, b"longer").unwrap();
        assert!(watcher.poll(&[]).unwrap().is_empty());
        assert_eq!(
            watcher.poll(&[]).unwrap(),
            vec![input.display().to_string()]
        );
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
        std::fs::write(&file, b"new version").unwrap();
        assert!(watcher.poll(&[]).unwrap().is_empty());
        assert_eq!(watcher.poll(&[]).unwrap().len(), 1);
        std::fs::remove_file(&file).unwrap();
        assert!(watcher.poll(&[]).unwrap().is_empty());
        std::fs::write(&file, b"another version").unwrap();
        assert!(watcher.poll(&[]).unwrap().is_empty());
        assert_eq!(watcher.poll(&[]).unwrap().len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
