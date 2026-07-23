mod commands;
mod state;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let db = state::DbState::open().expect("failed to open videre database");
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(db)
        .invoke_handler(tauri::generate_handler![
            commands::faces_list,
            commands::cluster_detail,
            commands::person_detail,
            commands::search_person,
            commands::assign,
            commands::new_person,
            commands::remove_face,
            commands::dissolve_cluster,
            commands::delete_person,
            commands::set_primary,
            commands::rename_person,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
