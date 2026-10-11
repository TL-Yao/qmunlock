mod commands;
mod core;

pub fn run() {
    tauri::Builder::default()
        .manage(commands::BatchLock(tokio::sync::Mutex::new(())))
        .manage(commands::WatchState::default())
        .manage(commands::HistoryCache::default())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            commands::check_credentials,
            commands::open_full_disk_access_settings,
            commands::open_external_url,
            commands::os_platform,
            commands::get_file_info,
            commands::scan_paths,
            commands::decrypt_paths,
            commands::set_watch_directory,
            commands::poll_watch_directory,
            commands::acknowledge_watch_files,
            commands::clear_processing_history,
        ])
        .run(tauri::generate_context!())
        .expect("error while running QM Unlock");
}
