use super::{decrypt, tags, Error, Result};
use std::path::{Path, PathBuf};

pub fn enabled(template: Option<&str>) -> bool {
    template.is_some_and(|s| !s.trim().is_empty())
}

pub fn validate(template: &str) -> Result<()> {
    if template.is_empty() || template.starts_with('/') || template.contains(['\\', ':']) {
        return Err(Error::from("命名模板必须是相对路径，子目录使用 / 分隔"));
    }
    for part in template.split('/') {
        if part.trim().is_empty() || matches!(part.trim(), "." | "..") {
            return Err(Error::from("命名模板不能包含空目录、. 或 .."));
        }
    }
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        if rest[..start].contains('}') {
            return Err(Error::from("命名模板括号不匹配"));
        }
        let end = rest[start..].find('}').ok_or("命名模板括号不匹配")? + start;
        if !matches!(
            &rest[start + 1..end],
            "filename" | "artist" | "title" | "album" | "track"
        ) {
            return Err(Error::from(
                "支持的占位符：{filename}、{artist}、{title}、{album}、{track}",
            ));
        }
        rest = &rest[end + 1..];
    }
    if rest.contains('}') {
        return Err(Error::from("命名模板括号不匹配"));
    }
    Ok(())
}

fn safe(value: &str) -> String {
    let mut value: String = value
        .chars()
        .map(|c| {
            if c.is_control() || "<>:\"/\\|?*{}".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    // Reserve room for the extension, collision suffix and Windows-name prefix.
    // Bound UTF-8 bytes, while truncating only at a Unicode character boundary.
    let mut end = value.len().min(200);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
    let value = value.trim().trim_end_matches(['.', ' ']);
    if value.is_empty() || value == "." || value == ".." {
        return "未知".into();
    }
    let base = value
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    if matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (base.len() == 4
            && (base.starts_with("COM") || base.starts_with("LPT"))
            && matches!(base.as_bytes()[3], b'1'..=b'9'))
    {
        format!("_{value}")
    } else {
        value.into()
    }
}

pub fn relative(
    template: &str,
    input: &Path,
    meta: Option<&tags::SongMeta>,
    embedded: Option<&tags::EmbeddedTags>,
) -> Result<PathBuf> {
    validate(template)?;
    let filename = input
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("converted");
    let title = meta
        .map(|m| m.title.as_str())
        .filter(|s| !s.is_empty())
        .or_else(|| embedded.map(|m| m.title.as_str()).filter(|s| !s.is_empty()))
        .unwrap_or(filename);
    let artist = meta
        .map(|m| m.singers.as_str())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            embedded
                .map(|m| m.artist.as_str())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or("未知歌手");
    let album = meta
        .map(|m| m.album.as_str())
        .filter(|s| !s.is_empty())
        .or_else(|| embedded.map(|m| m.album.as_str()).filter(|s| !s.is_empty()))
        .unwrap_or("未知专辑");
    let track = format!("{:02}", meta.and_then(|m| m.track_number).unwrap_or(0));
    let replacements = [
        ("{filename}", safe(filename)),
        ("{title}", safe(title)),
        ("{artist}", safe(artist)),
        ("{album}", safe(album)),
        ("{track}", track),
    ];
    let mut path = PathBuf::new();
    for part in template.split('/') {
        let mut name = part.to_owned();
        for (token, value) in &replacements {
            name = name.replace(token, value);
        }
        path.push(safe(&name));
    }
    Ok(path)
}

// Only move a generated copy. Never rename or overwrite the user's source.
pub fn apply(
    output: &Path,
    input: &Path,
    directory: Option<&Path>,
    template: Option<&str>,
    meta: Option<&tags::SongMeta>,
    embedded: Option<&tags::EmbeddedTags>,
) -> Result<PathBuf> {
    let Some(template) = template.filter(|s| !s.trim().is_empty()) else {
        return Ok(output.to_path_buf());
    };
    if std::fs::canonicalize(output)? == std::fs::canonicalize(input)? {
        return Err(Error::from("命名模板只能用于输出副本"));
    }
    let relative = relative(template.trim(), input, meta, embedded)?;
    let root = directory
        .or_else(|| input.parent())
        .unwrap_or(Path::new("."));
    let parent = root.join(relative.parent().unwrap_or(Path::new("")));
    std::fs::create_dir_all(&parent)?;
    let stem = relative
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("converted");
    let extension = output.extension().and_then(|s| s.to_str()).unwrap_or("bin");
    let desired = parent.join(format!("{stem}.{extension}"));
    if desired == output {
        return Ok(output.to_path_buf());
    }
    let target = decrypt::available_path(&parent, stem, extension);
    // Reserve using create_new: a collision must never replace an existing file.
    let mut destination = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&target)?;
    let copied = std::fs::File::open(output)
        .and_then(|mut source| std::io::copy(&mut source, &mut destination));
    drop(destination);
    if let Err(error) = copied {
        let _ = std::fs::remove_file(&target);
        return Err(error.into());
    }
    std::fs::remove_file(output)?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn templates_are_relative_and_metadata_cannot_create_directories() {
        for invalid in [
            "../{title}",
            "/{title}",
            "a//b",
            "C:/{title}",
            "{wrong}",
            "{title",
            "a\\b",
        ] {
            assert!(validate(invalid).is_err(), "{invalid}");
        }
        let embedded = tags::EmbeddedTags {
            title: "CON".into(),
            artist: "a/b".into(),
            album: "../x".into(),
        };
        assert_eq!(
            relative(
                "{album}/{track} {artist} - {title}",
                Path::new("song.mp3"),
                None,
                Some(&embedded)
            )
            .unwrap(),
            PathBuf::from(".._x/00 a_b - _CON")
        );
        assert_eq!(
            relative("{artist} - {title}", Path::new("song.mp3"), None, None).unwrap(),
            PathBuf::from("未知歌手 - song")
        );
    }
    #[test]
    fn naming_preserves_source_and_existing_outputs() {
        let dir = std::env::temp_dir().join(format!("qm-naming-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("source.mp3");
        let output = dir.join("copy.mp3");
        std::fs::write(&source, b"source").unwrap();
        std::fs::write(&output, b"copy").unwrap();
        let target = apply(&output, &source, Some(&dir), Some("{filename}"), None, None).unwrap();
        assert_ne!(target, source);
        assert_eq!(std::fs::read(&source).unwrap(), b"source");
        assert_eq!(std::fs::read(target).unwrap(), b"copy");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unicode_names_fit_byte_limits_and_leave_room_for_collision_suffixes() {
        let value = safe(&"中文歌名🎵".repeat(100));
        assert!(value.len() <= 200);
        assert!(!value.is_empty());
        assert!(format!("{value} (123456789).flac").len() <= 255);
        let dir = std::env::temp_dir().join(format!("qm-unicode-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("input.mp3");
        let output = dir.join("copy.mp3");
        std::fs::write(&input, b"source").unwrap();
        std::fs::write(&output, b"copy").unwrap();
        let meta = tags::EmbeddedTags {
            title: "歌".repeat(200),
            artist: String::new(),
            album: String::new(),
        };
        let target = apply(
            &output,
            &input,
            Some(&dir),
            Some("{title}"),
            None,
            Some(&meta),
        )
        .unwrap();
        assert!(target.file_name().unwrap().to_str().unwrap().len() <= 255);
        assert_eq!(std::fs::read(&target).unwrap(), b"copy");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
