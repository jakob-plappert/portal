use crate::project::Project;
use crate::shot::Shot;

#[derive(Default)]
pub struct AppState {
    pub current_project: Option<Project>,
    pub shots: Vec<Shot>,
}
