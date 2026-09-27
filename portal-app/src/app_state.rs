use crate::project::Project;
use crate::shot::Shot;

#[derive(Default)]
pub struct AppState {
    // `None` represents the normal startup state in which no project has been
    // opened yet. Once present, the owned `Project` remains Portal's source of
    // truth; the Slint properties are only a view of this data.
    pub current_project: Option<Project>,

    // The application owns its shots. Slint receives a separate model built
    // from this vector rather than becoming responsible for project data.
    pub shots: Vec<Shot>,
}
