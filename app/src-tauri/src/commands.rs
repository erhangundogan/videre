use crate::state::DbState;
use tauri::State;
use videre_api::{ClusterDetail, FacesData, PersonDetail};

fn err(e: videre_api::Error) -> String {
    e.to_string()
}

#[tauri::command]
pub fn faces_list(db: State<DbState>) -> Result<FacesData, String> {
    let conn = db.0.lock().map_err(|_| "db lock poisoned".to_string())?;
    videre_api::faces_list(&conn).map_err(err)
}

#[tauri::command]
pub fn cluster_detail(db: State<DbState>, cluster_id: i64) -> Result<ClusterDetail, String> {
    let conn = db.0.lock().map_err(|_| "db lock poisoned".to_string())?;
    videre_api::cluster_detail(&conn, cluster_id).map_err(err)
}

#[tauri::command]
pub fn person_detail(db: State<DbState>, name: String) -> Result<PersonDetail, String> {
    let conn = db.0.lock().map_err(|_| "db lock poisoned".to_string())?;
    videre_api::person_detail(&conn, &name).map_err(err)
}

#[tauri::command]
pub fn search_person(db: State<DbState>, name: String) -> Result<Vec<String>, String> {
    let conn = db.0.lock().map_err(|_| "db lock poisoned".to_string())?;
    videre_api::search_person(&conn, &name).map_err(err)
}

#[tauri::command]
pub fn assign(db: State<DbState>, face_ids: Vec<i64>, person_label: String) -> Result<(), String> {
    let conn = db.0.lock().map_err(|_| "db lock poisoned".to_string())?;
    videre_api::assign(&conn, &face_ids, &person_label).map_err(err)
}

#[tauri::command]
pub fn new_person(db: State<DbState>, face_ids: Vec<i64>, label: String) -> Result<(), String> {
    let conn = db.0.lock().map_err(|_| "db lock poisoned".to_string())?;
    videre_api::new_person(&conn, &face_ids, &label).map_err(err)
}

#[tauri::command]
pub fn remove_face(db: State<DbState>, face_id: i64) -> Result<(), String> {
    let conn = db.0.lock().map_err(|_| "db lock poisoned".to_string())?;
    videre_api::remove_face(&conn, face_id).map_err(err)
}

#[tauri::command]
pub fn dissolve_cluster(db: State<DbState>, cluster_id: i64) -> Result<(), String> {
    let conn = db.0.lock().map_err(|_| "db lock poisoned".to_string())?;
    videre_api::dissolve_cluster(&conn, cluster_id).map_err(err)
}

#[tauri::command]
pub fn delete_person(db: State<DbState>, label: String) -> Result<(), String> {
    let conn = db.0.lock().map_err(|_| "db lock poisoned".to_string())?;
    videre_api::delete_person(&conn, &label).map_err(err)
}

#[tauri::command]
pub fn set_primary(db: State<DbState>, face_id: i64, person_label: String) -> Result<(), String> {
    let conn = db.0.lock().map_err(|_| "db lock poisoned".to_string())?;
    videre_api::set_primary(&conn, face_id, &person_label).map_err(err)
}

#[tauri::command]
pub fn rename_person(
    db: State<DbState>,
    old_label: String,
    new_label: String,
) -> Result<(), String> {
    let conn = db.0.lock().map_err(|_| "db lock poisoned".to_string())?;
    videre_api::rename_person(&conn, &old_label, &new_label).map_err(err)
}
