use crate::core::{
    self, automation, credentials, decrypt, ekey, naming, tags, DecryptOptions, DecryptResult,
    Error, FileInfo, ProgressEvent,
};
use std::path::{Path, PathBuf};
use tauri::{Emitter, Manager};

pub struct BatchLock(pub tokio::sync::Mutex<()>);
#[derive(Default)]
pub struct WatchState(pub std::sync::Mutex<Option<automation::FolderWatcher>>);

fn history_path(app: &tauri::AppHandle) -> Option<PathBuf> {
    app.path()
        .app_data_dir()
        .ok()
        .map(|p| p.join("processing-history.json"))
}

#[tauri::command]
pub fn set_watch_directory(
    state: tauri::State<WatchState>,
    directory: Option<String>,
) -> std::result::Result<(), String> {
    let watcher = directory
        .map(|p| automation::FolderWatcher::new(Path::new(&p)))
        .transpose()
        .map_err(|e| e.to_string())?;
    *state.0.lock().map_err(|e| e.to_string())? = watcher;
    Ok(())
}

#[tauri::command]
pub fn poll_watch_directory(
    app: tauri::AppHandle,
    state: tauri::State<WatchState>,
    batch: tauri::State<BatchLock>,
    output_dir: Option<String>,
) -> std::result::Result<Vec<String>, String> {
    let Ok(_batch) = batch.0.try_lock() else {
        return Ok(Vec::new());
    };
    let mut state = state.0.lock().map_err(|e| e.to_string())?;
    let Some(watcher) = state.as_mut() else {
        return Ok(Vec::new());
    };
    let mut excluded = history_path(&app)
        .map(|p| automation::History::load(&p).outputs())
        .unwrap_or_default();
    if let Some(output) = output_dir.and_then(|p| std::fs::canonicalize(p).ok()) {
        if output != watcher.root() && output.starts_with(watcher.root()) {
            excluded.push(output);
        }
    }
    watcher.poll(&excluded).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn check_credentials() -> core::CredentialStatus {
    credentials::status()
}

/// 打开 macOS 的「完全磁盘访问权限」页；实际授权必须由用户在系统设置中确认。
#[tauri::command]
pub fn open_full_disk_access_settings() -> std::result::Result<(), String> {
    credentials::open_full_disk_access_settings().map_err(|error| error.to_string())
}

/// 用系统默认浏览器打开项目主页。WebView 对普通的 `target="_blank"` 链接支持不一致，
/// 尤其是 macOS 的 WKWebView 可能直接忽略它。
#[tauri::command]
pub fn open_external_url(url: String) -> std::result::Result<(), String> {
    if url != "https://github.com/mary20050520/qmunlock" {
        return Err("不允许打开未知链接".to_owned());
    }

    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = std::process::Command::new("open");
        command.arg(&url);
        command
    };

    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = std::process::Command::new("rundll32");
        command.args(["url.dll,FileProtocolHandler", &url]);
        command
    };

    #[cfg(target_os = "linux")]
    let mut command = {
        let mut command = std::process::Command::new("xdg-open");
        command.arg(&url);
        command
    };

    command
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("无法打开浏览器：{error}"))
}

/// 运行平台标识（`macos` / `windows` / `linux`），供前端门控平台专属功能。
///
/// 前端不能靠 UA 或凭据接口推断平台：凭据读取失败时 platform 会是 unknown，
/// 会把 macOS 独有的曲库功能误隐藏。这里直接取编译期目标系统，永远可靠。
#[tauri::command]
pub fn os_platform() -> String {
    std::env::consts::OS.to_owned()
}

#[tauri::command]
pub fn get_file_info(path: String) -> FileInfo {
    classify(&PathBuf::from(&path))
}

/// 单个文件该走哪条流程。按内容而不是只看扩展名判断。
enum Route {
    /// 带 musicex footer 的加密文件：解密 → (转码) → 增强
    Encrypted(core::MusicExFooter),
    /// 未加密音频：(转码) → 增强。`format` 是按内容识别的真实格式
    Plain { format: String },
    /// 无法处理
    Unsupported(String),
}

fn route(path: &Path) -> Route {
    if core::enc_path(path) {
        match decrypt::info(path) {
            Ok(footer) => Route::Encrypted(footer),
            // 扩展名是 .mgg / .mflac，但没有 footer、内容就是普通音频：说明没加密，直接处理
            Err(error) => match core::sniff_plain_format(path) {
                Some(format) => Route::Plain {
                    format: format.to_owned(),
                },
                None => Route::Unsupported(error.to_string()),
            },
        }
    } else if core::plain_audio_path(path) {
        Route::Plain {
            format: path
                .extension()
                .and_then(|x| x.to_str())
                .map(|x| x.to_ascii_lowercase())
                .unwrap_or_default(),
        }
    } else {
        Route::Unsupported("不支持的文件类型".into())
    }
}

/// 识别单个文件：区分加密格式与未加密音频，未加密的无需解密即可转换 / 增强。
fn classify(path: &Path) -> FileInfo {
    let display = path.display().to_string();
    match route(path) {
        Route::Encrypted(footer) => FileInfo {
            path: display,
            supported: true,
            kind: "enc".into(),
            format: Some("musicex/QMC2".into()),
            song_mid: Some(footer.song_mid),
            resource_filename: Some(footer.filename),
            error: None,
        },
        Route::Plain { format } => FileInfo {
            path: display,
            supported: true,
            kind: "plain".into(),
            format: Some(format),
            song_mid: None,
            resource_filename: None,
            error: None,
        },
        Route::Unsupported(error) if core::enc_path(path) => FileInfo {
            path: display,
            supported: false,
            kind: "enc".into(),
            format: None,
            song_mid: None,
            resource_filename: None,
            error: Some(error),
        },
        Route::Unsupported(error) => FileInfo {
            path: display,
            supported: false,
            kind: "unknown".into(),
            format: None,
            song_mid: None,
            resource_filename: None,
            error: Some(error),
        },
    }
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanResult {
    pub files: Vec<String>,
    pub infos: Vec<FileInfo>,
}

#[tauri::command]
pub fn scan_paths(app: tauri::AppHandle, paths: Vec<String>) -> ScanResult {
    let inputs = expand_paths(paths);
    let total = inputs.len().max(1) as u64;
    let mut infos = Vec::with_capacity(inputs.len());
    emit_progress(&app, "scan", "", 0, total, "正在扫描拖入的文件和文件夹");
    for (index, path) in inputs.iter().enumerate() {
        let info = classify(path);
        let current = (index + 1) as u64;
        emit_progress(
            &app,
            "parse",
            &info.path,
            current,
            total,
            &format!("正在解析 {}/{} 个文件", current, inputs.len()),
        );
        infos.push(info);
    }
    ScanResult {
        files: inputs
            .iter()
            .map(|path| path.display().to_string())
            .collect(),
        infos,
    }
}

#[tauri::command]
pub async fn decrypt_paths(
    app: tauri::AppHandle,
    paths: Vec<String>,
    output_dir: Option<String>,
    options: DecryptOptions,
) -> Vec<DecryptResult> {
    let batch = app.state::<BatchLock>();
    let _batch = batch.0.lock().await;
    let inputs = expand_paths(paths);
    if let Some(template) = options
        .output_template
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    {
        if let Err(error) = naming::validate(template.trim()) {
            return inputs
                .iter()
                .map(|input| DecryptResult {
                    input: input.display().to_string(),
                    output: None,
                    ok: false,
                    skipped: false,
                    format: None,
                    error: Some(error.to_string()),
                    cover: None,
                    lyrics: None,
                })
                .collect();
        }
    }
    let store = history_path(&app);
    let mut history = store
        .as_deref()
        .map(automation::History::load)
        .unwrap_or_default();
    let output = output_dir.as_deref().map(Path::new);
    let credentials = if options
        .manual_ekey
        .as_deref()
        .unwrap_or_default()
        .trim()
        .is_empty()
    {
        credentials::load().ok()
    } else {
        None
    };
    let mut results = Vec::with_capacity(inputs.len());
    let total = inputs.len().max(1) as u64;
    for (index, input) in inputs.iter().enumerate() {
        let source_version = automation::stamp(input);
        let key = automation::job_key(input, output, &options);
        let existing = options
            .skip_existing
            .then(|| {
                history
                    .lookup_output(input)
                    .or_else(|| history.lookup(&key, input))
                    .or_else(|| {
                        if history.knows_input(input) {
                            None
                        } else {
                            automation::legacy_output(input, output, &options)
                        }
                    })
            })
            .flatten();
        let result = if let Some(result) = existing {
            emit_frac(
                &app,
                "skip",
                &input.display().to_string(),
                (index + 1) as u64,
                total,
                1.0,
                "已跳过已有输出",
            );
            result
        } else {
            decrypt_one(
                &app,
                input,
                output,
                &options,
                credentials.as_ref(),
                (index + 1) as u64,
                total,
            )
            .await
        };
        let in_place = result
            .output
            .as_deref()
            .is_some_and(|p| std::fs::canonicalize(p).ok() == std::fs::canonicalize(input).ok());
        if in_place || source_version == automation::stamp(input) {
            history.record(key, input, &result);
        }
        if let Some(store) = store.as_deref() {
            if let Err(error) = history.save(store) {
                eprintln!("无法保存处理记录：{error}");
            }
        }
        results.push(result);
    }
    emit_frac(&app, "complete", "", total, total, 1.0, "任务处理完成");
    results
}

async fn decrypt_one(
    app: &tauri::AppHandle,
    input: &Path,
    output_dir: Option<&Path>,
    options: &DecryptOptions,
    credentials: Option<&core::Credentials>,
    file_index: u64,
    file_total: u64,
) -> DecryptResult {
    let input_name = input.display().to_string();
    let footer = match route(input) {
        Route::Encrypted(footer) => footer,
        Route::Plain { format } => {
            return enhance_plain(
                app, input, &format, output_dir, options, file_index, file_total,
            )
            .await;
        }
        Route::Unsupported(error) => {
            return DecryptResult {
                input: input_name,
                output: None,
                ok: false,
                skipped: false,
                format: None,
                error: Some(error),
                cover: None,
                lyrics: None,
            };
        }
    };
    emit_frac(
        app,
        "parse",
        &input_name,
        file_index,
        file_total,
        FRAC_PARSE,
        "正在读取 musicex footer",
    );
    let run = async {
        emit_frac(
            app,
            "ekey",
            &input_name,
            file_index,
            file_total,
            FRAC_EKEY,
            "正在获取 ekey",
        );
        let key = match options
            .manual_ekey
            .as_deref()
            .filter(|key| !key.trim().is_empty())
        {
            Some(key) => key.to_owned(),
            None => {
                let credentials =
                    credentials.ok_or("无法自动获取 ekey：请登录 QQ 音乐，或粘贴手动 ekey")?;
                ekey::fetch(&footer, credentials, credentials::api_platform()).await?
            }
        };
        let progress_app = app.clone();
        let progress_input = input_name.clone();
        let fi = file_index;
        let ft = file_total;
        decrypt::decrypt_file_with_progress(
            input,
            output_dir,
            &footer,
            &key,
            &options.output_mode,
            move |current, total, phase| {
                let ratio = if total == 0 {
                    1.0
                } else {
                    current as f64 / total as f64
                };
                let (frac, message) = if phase == "transcode" {
                    (FRAC_TRANSCODE, "正在转换为 MP3")
                } else {
                    (
                        FRAC_DECRYPT_BASE + FRAC_DECRYPT_SPAN * ratio,
                        "正在解密音频",
                    )
                };
                emit_frac(&progress_app, phase, &progress_input, fi, ft, frac, message);
            },
        )
    }
    .await;
    match run {
        Ok((output, format)) => {
            let embedded = if naming::enabled(options.output_template.as_deref()) {
                tags::read_embedded_tags(&output)
            } else {
                None
            };
            let meta = if naming::enabled(options.output_template.as_deref()) {
                tags::resolve_song(&footer.song_mid).await.ok()
            } else {
                None
            };
            let output = match naming::apply(
                &output,
                input,
                output_dir,
                options.output_template.as_deref(),
                meta.as_ref(),
                embedded.as_ref(),
            ) {
                Ok(output) => output,
                Err(error) => {
                    return DecryptResult {
                        input: input_name,
                        output: Some(output.display().to_string()),
                        ok: false,
                        skipped: false,
                        format: Some(format),
                        error: Some(format!("命名失败：{error}")),
                        cover: None,
                        lyrics: None,
                    }
                }
            };
            let song_mid = footer.song_mid.clone();
            let cover = if options.embed_cover {
                let notes = decorate_output(
                    app,
                    &output,
                    &format,
                    &song_mid,
                    &input_name,
                    options,
                    BatchPos {
                        index: file_index,
                        total: file_total,
                    },
                )
                .await;
                notes.cover
            } else {
                None
            };
            let lyrics = if options.fetch_lyrics {
                Some(
                    write_lyrics(
                        app,
                        &output,
                        options,
                        &song_mid,
                        &input_name,
                        file_index,
                        file_total,
                    )
                    .await,
                )
            } else {
                None
            };
            DecryptResult {
                input: input_name,
                output: Some(output.display().to_string()),
                ok: true,
                skipped: false,
                format: Some(format),
                error: None,
                cover,
                lyrics,
            }
        }
        Err(error) => DecryptResult {
            input: input_name,
            output: None,
            ok: false,
            skipped: false,
            format: None,
            error: Some(error.to_string()),
            cover: None,
            lyrics: None,
        },
    }
}

/// 「副本」模式的目标路径：既不能等于源路径，也不能覆盖目录里已有的文件。
///
/// 未选输出目录时目录就是源目录，同名会与源同路径，「副本」就变成改原文件；
/// 选了目录也可能撞同名文件。两种情况都追加「副本 / 副本 2 …」序号避开。
fn unique_copy_target(input: &Path, dir: &Path) -> PathBuf {
    let name = input
        .file_name()
        .map(|value| value.to_os_string())
        .unwrap_or_default();
    let stem = input
        .file_stem()
        .map(|value| value.to_string_lossy().to_string())
        .unwrap_or_default();
    let ext = input
        .extension()
        .map(|value| format!(".{}", value.to_string_lossy()))
        .unwrap_or_default();
    let mut dest = dir.join(name);
    let mut index = 2u32;
    while dest.as_path() == input || dest.exists() {
        let suffix = if index == 2 {
            String::new()
        } else {
            format!(" {}", index - 1)
        };
        dest = dir.join(format!("{stem} 副本{suffix}{ext}"));
        index += 1;
    }
    dest
}

/// 普通音频的写入目标。
#[derive(Debug, PartialEq, Eq)]
enum PlainTarget {
    /// 原位增强源文件
    InPlace,
    /// 先复制到此路径再增强副本
    Copy(PathBuf),
    /// 先用 FFmpeg 转码到此路径（MP3）再增强
    Transcode(PathBuf),
}

/// 打包的精简 FFmpeg 能解码后转 MP3 的普通音频格式（见 scripts/build-ffmpeg.sh：
/// 仅含 ogg / flac / mp3 / mov 解复用器与 vorbis / flac / aac / alac 等解码器）。
/// OPUS、WAV 不在其中，提前给出明确提示，而不是抛出 FFmpeg 的原始报错。
fn plain_transcodable(format: &str) -> bool {
    matches!(format, "ogg" | "flac" | "m4a")
}

/// 决定未加密音频写到哪里（`format` 为按内容识别的真实格式）。
///
/// - 输出 MP3 且源不是 MP3：转码到输出目录（未选则源目录），用 `available_path`
///   追加「 (1)」等序号，绝不覆盖源文件或同名已有文件。
/// - 扩展名与真实格式不符（如未加密的 .mgg 其实是 OGG）：另存为正确扩展名的新文件，
///   播放器才认得；原文件不动。
/// - 其余情况（含源已是 MP3）沿用原位 / 副本逻辑。
fn plain_target(
    input: &Path,
    format: &str,
    output_dir: Option<&Path>,
    options: &DecryptOptions,
) -> core::Result<PlainTarget> {
    let dir = || {
        output_dir
            .map(Path::to_path_buf)
            .or_else(|| input.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| PathBuf::from("."))
    };
    let stem = || {
        input
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("converted")
            .to_owned()
    };
    let extension = input
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase())
        .unwrap_or_default();
    if matches!(options.output_mode, core::OutputMode::Mp3) && format != "mp3" {
        if !plain_transcodable(format) {
            return Err(Error::from(format!(
                "暂不支持 {} 转 MP3，请改选「原格式」",
                format.to_uppercase()
            )));
        }
        Ok(PlainTarget::Transcode(decrypt::available_path(
            &dir(),
            &stem(),
            "mp3",
        )))
    } else if extension != format {
        Ok(PlainTarget::Copy(decrypt::available_path(
            &dir(),
            &stem(),
            format,
        )))
    } else if options.plain_copy || naming::enabled(options.output_template.as_deref()) {
        Ok(PlainTarget::Copy(unique_copy_target(input, &dir())))
    } else {
        Ok(PlainTarget::InPlace)
    }
}

/// 未加密音频流程：不解密，可选转 MP3，再做封面 / 歌词增强。
///
/// 曲库信息按文件名（去掉扩展名）检索；写入目标见 [`plain_target`]：转 MP3 时
/// 增强作用于新生成的 MP3（从而可以内嵌封面），`plain_copy` 为真时先复制副本再改副本，
/// 否则原位写入。macOS 独有的访达图标与 QQ 音乐属性仍由 cfg 门控，其他平台自动跳过。
async fn enhance_plain(
    app: &tauri::AppHandle,
    input: &Path,
    source_format: &str,
    output_dir: Option<&Path>,
    options: &DecryptOptions,
    file_index: u64,
    file_total: u64,
) -> DecryptResult {
    let input_name = input.display().to_string();
    emit_frac(
        app,
        "parse",
        &input_name,
        file_index,
        file_total,
        FRAC_PARSE,
        "正在识别未加密音频",
    );
    let run = async {
        let mut format = source_format.to_owned();
        // 是否产出了「格式上的新文件」（转码，或未加密 .mgg 另存为 .ogg）
        let mut converted = false;
        let target = match plain_target(input, source_format, output_dir, options)? {
            PlainTarget::InPlace => input.to_path_buf(),
            PlainTarget::Copy(dest) => {
                if let Some(dir) = dest.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                std::fs::copy(input, &dest)?;
                converted = true;
                dest
            }
            PlainTarget::Transcode(dest) => {
                if let Some(dir) = dest.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                emit_frac(
                    app,
                    "transcode",
                    &input_name,
                    file_index,
                    file_total,
                    FRAC_PLAIN_TRANSCODE,
                    "正在转换为 MP3",
                );
                let (source, output) = (input.to_path_buf(), dest.clone());
                tauri::async_runtime::spawn_blocking(move || {
                    decrypt::convert_to_mp3(&source, &output)
                })
                .await
                .map_err(|error| Error::from(format!("转码任务异常：{error}")))??;
                format = "mp3".to_owned();
                converted = true;
                dest
            }
        };

        let stem = input
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_owned();
        // 文件自带标签比文件名可靠：优先用它构造检索词，并校验检索结果是否同一首歌。
        // 读目标而非源：OGG 等格式读不到标签，转码后 FFmpeg 会把注释带进 MP3 的 ID3
        let embedded = tags::read_embedded_tags(&target);
        let query = match embedded.as_ref() {
            Some(found) if !found.title.is_empty() => {
                if found.artist.is_empty() {
                    found.title.clone()
                } else {
                    format!("{} {}", found.title, found.artist)
                }
            }
            _ => stem,
        };
        emit_frac(
            app,
            "cover",
            &input_name,
            file_index,
            file_total,
            FRAC_COVER,
            "正在检索曲库",
        );
        // 检索返回全部候选，再结合文件标签逐条校验挑出同一首歌，避免错配版本
        let matched = match tags::search_songs(&query).await {
            Ok(candidates) => tags::pick_matching_meta(&query, candidates, embedded.as_ref()),
            Err(error) => Err(error),
        };
        let meta = match matched {
            Ok(meta) => meta,
            // 转换产物本身就是用户要的结果：曲库匹配失败只记为备注，不把整个任务判为失败
            Err(error) if converted || naming::enabled(options.output_template.as_deref()) => {
                let target = naming::apply(
                    &target,
                    input,
                    output_dir,
                    options.output_template.as_deref(),
                    None,
                    embedded.as_ref(),
                )?;
                return Ok((target, format, vec![format!("未匹配曲库：{error}")], None));
            }
            Err(error) => return Err(error),
        };

        let target = naming::apply(
            &target,
            input,
            output_dir,
            options.output_template.as_deref(),
            Some(&meta),
            embedded.as_ref(),
        )?;
        let mut notes: Vec<String> = Vec::new();
        // 「封面」= FLAC / MP3 内嵌封面 + 访达自定义图标（macOS）。
        let image = if options.embed_cover {
            match tags::fetch_cover(&meta.album_mid).await {
                Ok(image) => Some(image),
                Err(error) => {
                    notes.push(format!("未获取封面：{error}"));
                    None
                }
            }
        } else {
            None
        };

        if options.embed_cover {
            if let Some(image) = image.as_ref() {
                let description = if meta.singers.is_empty() {
                    meta.title.clone()
                } else {
                    format!("{} - {}", meta.title, meta.singers)
                };
                let embedded_cover = match format.as_str() {
                    "flac" => tags::embed_cover_into_flac(&target, image, &description),
                    "mp3" => tags::embed_cover_into_mp3(&target, image, &description, &meta),
                    _ => {
                        notes.push(format!(
                            "{} 暂不支持内嵌封面（已设置访达图标）",
                            format.to_uppercase()
                        ));
                        Ok(())
                    }
                };
                match embedded_cover {
                    Ok(()) if matches!(format.as_str(), "flac" | "mp3") => {
                        notes.push(format!("内嵌封面《{}》", meta.album));
                    }
                    Ok(()) => {}
                    Err(error) => notes.push(format!("未写入封面：{error}")),
                }
                if cfg!(target_os = "macos") {
                    match tags::set_finder_icon(&target, image) {
                        Ok(()) => notes.push("访达图标".to_owned()),
                        Err(error) => notes.push(format!("访达图标未设置：{error}")),
                    }
                }
            }
        }

        let lyrics = if options.fetch_lyrics {
            Some(
                write_lyrics(
                    app,
                    &target,
                    options,
                    &meta.song_mid,
                    &input_name,
                    file_index,
                    file_total,
                )
                .await,
            )
        } else {
            None
        };

        Ok::<_, Error>((target, format, notes, lyrics))
    }
    .await;

    match run {
        Ok((target, format, notes, lyrics)) => DecryptResult {
            input: input_name,
            output: Some(target.display().to_string()),
            ok: true,
            skipped: false,
            format: Some(format),
            error: None,
            cover: (!notes.is_empty()).then(|| notes.join(" · ")),
            lyrics,
        },
        Err(error) => DecryptResult {
            input: input_name,
            output: None,
            ok: false,
            skipped: false,
            format: None,
            error: Some(error.to_string()),
            cover: None,
            lyrics: None,
        },
    }
}

/// 输出后的增强处理结果。
struct EnhanceNotes {
    cover: Option<String>,
}

/// 批次位置：当前是第几个文件、共几个文件。
/// 把两个计数器合成一个参数，避免 decorate_output 触发
/// clippy::too_many_arguments（上限 7 个）。
#[derive(Debug, Clone, Copy)]
struct BatchPos {
    index: u64,
    total: u64,
}

/// 输出后的增强处理。
///
/// 职责划分：
/// - 「封面」= FLAC / MP3 内嵌封面 + 访达自定义图标（仅 macOS）。Apple Music 与
///   Quick Look 读取前者；访达图标则让不支持内嵌封面的格式也能在文件夹中识别。
///
/// 每一步都是尽力而为：失败只记录到任务报告，不影响已经完成的解密结果。
async fn decorate_output(
    app: &tauri::AppHandle,
    output: &Path,
    format: &str,
    song_mid: &str,
    input_name: &str,
    options: &DecryptOptions,
    pos: BatchPos,
) -> EnhanceNotes {
    let mut notes: Vec<String> = Vec::new();
    emit_frac(
        app,
        "cover",
        input_name,
        pos.index,
        pos.total,
        FRAC_COVER,
        "正在获取曲库信息与封面",
    );
    let meta = match tags::resolve_song(song_mid).await {
        Ok(meta) => meta,
        Err(error) => {
            return EnhanceNotes {
                cover: options.embed_cover.then(|| format!("未写入封面：{error}")),
            }
        }
    };

    // 「封面」= FLAC / MP3 内嵌封面 + 访达自定义图标（macOS）。Apple Music 与
    // Quick Look 读取前者；访达图标则让不支持内嵌封面的格式也能在文件夹中识别。
    let image = if options.embed_cover {
        match tags::fetch_cover(&meta.album_mid).await {
            Ok(image) => Some(image),
            Err(error) => {
                notes.push(format!("未获取封面：{error}"));
                None
            }
        }
    } else {
        None
    };

    if options.embed_cover {
        if let Some(image) = image.as_ref() {
            let description = if meta.singers.is_empty() {
                meta.title.clone()
            } else {
                format!("{} - {}", meta.title, meta.singers)
            };
            let embedded_cover = match format {
                "flac" => tags::embed_cover_into_flac(output, image, &description),
                "mp3" => tags::embed_cover_into_mp3(output, image, &description, &meta),
                _ => {
                    notes.push(format!(
                        "{} 暂不支持内嵌封面（已设置访达图标）",
                        format.to_uppercase()
                    ));
                    Ok(())
                }
            };
            match embedded_cover {
                Ok(()) if matches!(format, "flac" | "mp3") => {
                    notes.push(format!("内嵌封面《{}》", meta.album));
                }
                Ok(()) => {}
                Err(error) => notes.push(format!("未写入封面：{error}")),
            }
            if cfg!(target_os = "macos") {
                match tags::set_finder_icon(output, image) {
                    Ok(()) => notes.push("访达图标".to_owned()),
                    Err(error) => notes.push(format!("访达图标未设置：{error}")),
                }
            }
        }
    }

    EnhanceNotes {
        cover: (!notes.is_empty()).then(|| notes.join(" · ")),
    }
}

/// 抓取 LRC 歌词并写入指定目录（留空则与音频同目录）。
async fn write_lyrics(
    app: &tauri::AppHandle,
    output: &Path,
    options: &DecryptOptions,
    song_mid: &str,
    input_name: &str,
    file_index: u64,
    file_total: u64,
) -> String {
    let directory = match options
        .lyrics_dir
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(value) => PathBuf::from(value),
        None => output.parent().unwrap_or(Path::new(".")).to_path_buf(),
    };
    emit_frac(
        app,
        "lyrics",
        input_name,
        file_index,
        file_total,
        FRAC_LYRICS,
        "正在获取歌词",
    );
    let lyric = match tags::fetch_lyric(song_mid).await {
        Ok(lyric) => lyric,
        Err(error) => return format!("未获取歌词：{error}"),
    };
    match tags::write_lyric(output, &directory, &lyric) {
        Ok(path) => {
            let name = path
                .file_name()
                .map(|value| value.to_string_lossy().into_owned())
                .unwrap_or_default();
            if output
                .extension()
                .and_then(|value| value.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("mp3"))
            {
                match tags::embed_lyrics_into_mp3(output, &lyric) {
                    Ok(()) if options.embed_synced_lyrics => {
                        match tags::embed_synced_lyrics_into_mp3(output, &lyric) {
                            Ok(()) => {
                                format!("歌词 {name} · 已嵌入 MP3 · 已嵌入同步歌词（实验性）")
                            }
                            Err(error) => {
                                format!("歌词 {name} · 已嵌入 MP3 · 同步歌词失败：{error}")
                            }
                        }
                    }
                    Ok(()) => format!("歌词 {name} · 已嵌入 MP3"),
                    Err(error) => format!("歌词 {name} · MP3 内嵌失败：{error}"),
                }
            } else {
                format!("歌词 {name}")
            }
        }
        Err(error) => format!("未写入歌词：{error}"),
    }
}

fn emit_progress(
    app: &tauri::AppHandle,
    phase: &str,
    input: &str,
    current: u64,
    total: u64,
    message: &str,
) {
    let percent = if total == 0 {
        0
    } else {
        ((current.min(total) as f64 / total as f64) * 100.0).round() as u8
    };
    let _ = app.emit(
        "decrypt-progress",
        ProgressEvent {
            phase: phase.into(),
            input: input.into(),
            current,
            total,
            percent,
            // 这个入口只用于扫描等无具体文件的阶段，行内进度条不会激活
            file_percent: percent,
            message: message.into(),
        },
    );
}

/// 以「整批任务」为口径发射进度：`percent = (已完成文件数 + 当前文件内占比) / 总文件数`。
///
/// 旧实现让每个阶段各自从 0/1 开始发射（ekey 0%、封面 0%……），而解密本身是瞬时的、
/// 耗时全在网络等待上，于是界面长时间停在 0% 再一次性跳 100%。这里把阶段占比折算进
/// 当前文件的那一份，进度才会平滑地从 0 走到 100。
fn emit_frac(
    app: &tauri::AppHandle,
    phase: &str,
    input: &str,
    file_index: u64,
    file_total: u64,
    frac: f64,
    message: &str,
) {
    let done = file_index.saturating_sub(1) as f64;
    let clamped = frac.clamp(0.0, 1.0);
    let percent = if file_total == 0 {
        100
    } else {
        (((done + clamped) / file_total as f64) * 100.0).round() as u8
    };
    let _ = app.emit(
        "decrypt-progress",
        ProgressEvent {
            phase: phase.into(),
            input: input.into(),
            current: file_index.min(file_total.max(1)),
            total: file_total,
            percent,
            // 行内进度条只看当前文件自己走到哪，不受批次影响
            file_percent: (clamped * 100.0).round() as u8,
            message: message.into(),
        },
    );
}

/// 单个文件内部的阶段占比，供 emit_frac 折算成整批进度。
const FRAC_PARSE: f64 = 0.05;
const FRAC_EKEY: f64 = 0.15;
const FRAC_DECRYPT_BASE: f64 = 0.20;
const FRAC_DECRYPT_SPAN: f64 = 0.60;
const FRAC_TRANSCODE: f64 = 0.82;
/// 普通音频没有解密阶段，转码是主要耗时，紧接识别之后显示
const FRAC_PLAIN_TRANSCODE: f64 = 0.20;
const FRAC_COVER: f64 = 0.88;
const FRAC_LYRICS: f64 = 0.93;

fn expand_paths(paths: Vec<String>) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for path in paths.into_iter().map(PathBuf::from) {
        if path.is_file() {
            if core::supported_path(&path) {
                files.push(path);
            }
        } else if path.is_dir() {
            collect(&path, &mut files);
        }
    }
    files.sort();
    files.dedup();
    files
}

fn collect(path: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, files);
        } else if core::supported_path(&path) {
            files.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_target_avoids_source_and_existing_files() {
        let dir = std::env::temp_dir().join(format!("qmunlock-copy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("song.flac");
        std::fs::write(&source, b"x").unwrap();

        // 未选输出目录时目录即源目录，必须避开源路径，否则「副本」变成改原文件
        let first = unique_copy_target(&source, &dir);
        assert_ne!(first, source);
        assert_eq!(first.file_name().unwrap(), "song 副本.flac");

        std::fs::write(&first, b"x").unwrap();
        let second = unique_copy_target(&source, &dir);
        assert_eq!(second.file_name().unwrap(), "song 副本 2.flac");

        // 选了空目录：直接用原名，不加后缀，也不覆盖任何东西
        let other = dir.join("out");
        std::fs::create_dir_all(&other).unwrap();
        assert_eq!(unique_copy_target(&source, &other), other.join("song.flac"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn plain_options(plain_copy: bool, mp3: bool) -> DecryptOptions {
        DecryptOptions {
            output_mode: if mp3 {
                core::OutputMode::Mp3
            } else {
                core::OutputMode::Original
            },
            manual_ekey: None,
            embed_cover: false,
            fetch_lyrics: false,
            lyrics_dir: None,
            embed_synced_lyrics: false,
            plain_copy,
            skip_existing: true,
            output_template: None,
        }
    }

    #[test]
    fn unencrypted_mgg_is_routed_as_plain_ogg() {
        let dir = std::env::temp_dir().join(format!("qmunlock-route-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // 扩展名 .mgg，但没有 musicex footer、内容是 OggS：按未加密 OGG 处理
        let fake = dir.join("song.mgg");
        std::fs::write(&fake, b"OggS\0\x02 plain vorbis page padding padding").unwrap();
        assert!(matches!(route(&fake), Route::Plain { ref format } if format == "ogg"));
        assert_eq!(classify(&fake).kind, "plain");
        // 原格式输出：另存为正确扩展名的 .ogg，不动原文件
        assert_eq!(
            plain_target(&fake, "ogg", None, &plain_options(false, false)).unwrap(),
            PlainTarget::Copy(dir.join("song.ogg"))
        );
        // 转 MP3：直接转码
        assert_eq!(
            plain_target(&fake, "ogg", None, &plain_options(false, true)).unwrap(),
            PlainTarget::Transcode(dir.join("song.mp3"))
        );

        // 既无 footer 也认不出格式：仍报不可用
        let junk = dir.join("junk.mgg");
        std::fs::write(&junk, vec![0x5Au8; 64]).unwrap();
        assert!(matches!(route(&junk), Route::Unsupported(_)));
        assert!(!classify(&junk).supported);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn plain_target_transcodes_non_mp3_without_overwriting() {
        let dir = std::env::temp_dir().join(format!("qmunlock-plain-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("song.ogg");
        std::fs::write(&source, b"x").unwrap();
        let mp3 = plain_options(false, true);

        // 未选输出目录：转到源目录，扩展名换成 mp3
        assert_eq!(
            plain_target(&source, "ogg", None, &mp3).unwrap(),
            PlainTarget::Transcode(dir.join("song.mp3"))
        );
        // 同名 mp3 已存在时追加序号，不覆盖
        std::fs::write(dir.join("song.mp3"), b"x").unwrap();
        assert_eq!(
            plain_target(&source, "ogg", None, &mp3).unwrap(),
            PlainTarget::Transcode(dir.join("song (1).mp3"))
        );
        // 选了输出目录：转到该目录；「原位」开关对转码无效
        let out = dir.join("out");
        assert_eq!(
            plain_target(&source, "ogg", Some(&out), &mp3).unwrap(),
            PlainTarget::Transcode(out.join("song.mp3"))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn plain_target_rejects_formats_bundled_ffmpeg_cannot_decode() {
        let mp3 = plain_options(false, true);
        for format in ["opus", "wav"] {
            let source = PathBuf::from(format!("/music/song.{format}"));
            assert!(plain_target(&source, format, None, &mp3).is_err());
            // 保留原格式时不受影响
            assert_eq!(
                plain_target(&source, format, None, &plain_options(false, false)).unwrap(),
                PlainTarget::InPlace
            );
        }
    }

    #[test]
    fn plain_target_keeps_mp3_and_original_modes() {
        let source = Path::new("/music/song.mp3");
        // 源已是 MP3：转 MP3 选项不生效，沿用原位 / 副本
        assert_eq!(
            plain_target(source, "mp3", None, &plain_options(false, true)).unwrap(),
            PlainTarget::InPlace
        );
        assert!(matches!(
            plain_target(source, "mp3", None, &plain_options(true, true)).unwrap(),
            PlainTarget::Copy(_)
        ));
        // 保留原格式：与之前一致
        assert_eq!(
            plain_target(
                Path::new("/music/song.ogg"),
                "ogg",
                None,
                &plain_options(false, false)
            )
            .unwrap(),
            PlainTarget::InPlace
        );
    }
}
