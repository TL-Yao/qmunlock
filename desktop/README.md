# QM Unlock Desktop

跨平台图形界面，面向当前用户本地的 QQ 音乐 `musicex` 下载文件。

## 开发

```bash
cd desktop
npm install
npm run tauri dev
```

需要 Rust stable、Node.js 20+，以及对应平台的 Tauri 系统依赖。构建并放入两个平台
的 LGPL FFmpeg：

```bash
./scripts/build-ffmpeg.sh
```

该脚本在 macOS 构建机上生成 macOS universal 和 Windows x64 两个二进制；Windows
交叉编译需要先安装 `brew install mingw-w64`。源码版本和 SHA-256 固定在脚本中，构建
完成后可用 `file src-tauri/resources/ffmpeg/*/ffmpeg*` 检查产物。

开发时如果资源不存在，也会回退到 `PATH` 中的 `ffmpeg`。

`npm run tauri -- build --debug` 会构建本机调试安装包。CI 会运行前端构建、Rust
格式/Clippy 和单元测试；`desktop-v*` 标签触发 macOS universal 与 Windows x64 的
安装包工作流。MP3 编码使用随包提供的 LGPL `libmp3lame`，不依赖 Homebrew 或系统中的
GPL FFmpeg。

## 行为

- 支持新版 `musicex` `.mgg` / `.mflac` / `.mmp4`，以及未加密的 `.flac` / `.mp3` / `.m4a` /
  `.ogg` / `.opus` / `.wav`。两类文件共用一个队列，按内容选择流程（`commands.rs` 的
  `route()`）：有 musicex footer 的先解密；没有 footer 的按文件头识别真实格式，直接转换 /
  增强。扩展名是 `.mgg` / `.mflac` 但内容是普通音频的文件按未加密处理，并以真实扩展名另存。
- 选择 MP3 时，未加密的 `.ogg` / `.flac` / `.m4a` 用随包 FFmpeg 转码为新文件，不覆盖源文件；
  随包 FFmpeg 不含 Opus 与 WAV 解码器，这两种格式只能按原格式增强。
- macOS 从当前用户 QQ 音乐 plist 读取凭据；Windows 从已登录、运行中的 QQMusic.exe
  读取凭据，权限不足时才提示管理员启动。
- ekey 和登录令牌仅用于当前进程，不写入日志、磁盘或任务历史。
- 自动获取失败时可以在界面手动粘贴 ekey。

- 目录监听、成功处理记录和安全命名模板见根目录 README 的「目录监听、跳过与命名」。处理记录保存到 Tauri app_data_dir 的 processing-history.json；偏好保存在 WebView localStorage，重启不会自动恢复监听。
- 新模块 core/automation.rs 负责文件稳定检测与历史复用，core/naming.rs 负责模板校验、元数据回退和输出副本命名；批次共用异步互斥锁，监听轮询在处理期间暂停。
- 扫描、监听设置/轮询及确认命令使用 Tauri 异步分发，文件系统操作不在 UI 主线程执行。历史索引按会话缓存，批次结束时在阻塞工作线程统一清理并写入一次，最多保留 10,000 条；设置中的「清除处理记录」同步清除磁盘文件与缓存。
- 监听结果包含路径和不透明版本字符串。前端扫描并入队成功后调用 acknowledge_watch_files；未确认版本会再次交付，过期确认不会吞掉源文件更新。加密下载需先存在有效 footer，或可识别的普通音频头。
