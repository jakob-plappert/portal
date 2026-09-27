mod app_state;
mod project;
mod shot;

use std::cell::RefCell;
use std::rc::Rc;

use app_state::AppState;
use project::Project;
use shot::Shot;

use slint::{ComponentHandle, ModelRc, VecModel};

slint::include_modules!();

fn main() -> Result<(), slint::PlatformError> {
    let window = MainWindow::new()?;

    let state = Rc::new(RefCell::new(AppState::default()));

    setup_create_project(&window, Rc::clone(&state));

    setup_open_project(&window, Rc::clone(&state));

    setup_refresh_projects(&window);

    setup_create_shot(&window, Rc::clone(&state));

    if let Err(error) = refresh_project_list(&window) {
        eprintln!("Could not load project list: {error}");
    }

    window.run()
}

fn setup_create_project(window: &MainWindow, state: Rc<RefCell<AppState>>) {
    let weak_window = window.as_weak();

    window.on_create_project(move |name| {
        let name = name.to_string();

        match Project::create(&name) {
            Ok(project) => {
                println!(
                    "Created project '{}' at {}",
                    project.name,
                    project.path.display(),
                );

                let shots = Vec::new();

                {
                    let mut state = state.borrow_mut();

                    state.current_project = Some(project.clone());

                    state.shots = shots.clone();
                }

                if let Some(window) = weak_window.upgrade() {
                    show_project(&window, &project, &shots);

                    if let Err(error) = refresh_project_list(&window) {
                        eprintln!("Could not refresh project list: {error}");
                    }
                }
            }

            Err(error) => {
                eprintln!("Could not create project: {error}");

                if let Some(window) = weak_window.upgrade() {
                    window.set_project_error(error.into());
                }
            }
        }
    });
}

fn setup_open_project(window: &MainWindow, state: Rc<RefCell<AppState>>) {
    let weak_window = window.as_weak();

    window.on_open_project(move |folder_name| {
        let folder_name = folder_name.to_string();

        let result = Project::open(&folder_name)
            .and_then(|project| Shot::load_all(&project).map(|shots| (project, shots)));

        match result {
            Ok((project, shots)) => {
                println!(
                    "Opened project '{}' with {} shot(s)",
                    project.name,
                    shots.len(),
                );

                {
                    let mut state = state.borrow_mut();

                    state.current_project = Some(project.clone());

                    state.shots = shots.clone();
                }

                if let Some(window) = weak_window.upgrade() {
                    show_project(&window, &project, &shots);
                }
            }

            Err(error) => {
                eprintln!("Could not open project: {error}");

                if let Some(window) = weak_window.upgrade() {
                    window.set_project_error(error.into());
                }
            }
        }
    });
}

fn setup_refresh_projects(window: &MainWindow) {
    let weak_window = window.as_weak();

    window.on_refresh_projects(move || {
        if let Some(window) = weak_window.upgrade() {
            match refresh_project_list(&window) {
                Ok(()) => {
                    window.set_project_error(String::new().into());
                }

                Err(error) => {
                    eprintln!("Could not refresh projects: {error}");

                    window.set_project_error(error.into());
                }
            }
        }
    });
}

fn setup_create_shot(window: &MainWindow, state: Rc<RefCell<AppState>>) {
    let weak_window = window.as_weak();

    window.on_create_shot(move |title, brief| {
        let title = title.to_string();

        let brief = brief.to_string();

        let project = {
            let state = state.borrow();

            state.current_project.clone()
        };

        let Some(project) = project else {
            if let Some(window) = weak_window.upgrade() {
                window.set_shot_error("No project is currently open.".into());
            }

            return;
        };

        match Shot::create(&project, &title, &brief) {
            Ok(shot) => {
                println!("Created shot {:04}: {}", shot.number, shot.title,);

                let shots = {
                    let mut state = state.borrow_mut();

                    state.shots.push(shot.clone());

                    state.shots.sort_by_key(|shot| shot.number);

                    state.shots.clone()
                };

                if let Some(window) = weak_window.upgrade() {
                    set_shot_model(&window, &shots);

                    select_shot(&window, &shot);

                    window.set_new_shot_dialog_open(false);

                    window.set_shot_error(String::new().into());
                }
            }

            Err(error) => {
                eprintln!("Could not create shot: {error}");

                if let Some(window) = weak_window.upgrade() {
                    window.set_shot_error(error.into());
                }
            }
        }
    });
}

fn refresh_project_list(window: &MainWindow) -> Result<(), String> {
    let projects = Project::list()?;

    let items = projects
        .into_iter()
        .map(|project| ProjectListItem {
            name: project.name.into(),

            folder_name: project.folder_name.into(),
        })
        .collect::<Vec<_>>();

    let model = ModelRc::new(VecModel::from(items));

    window.set_available_projects(model);

    Ok(())
}

fn show_project(window: &MainWindow, project: &Project, shots: &[Shot]) {
    window.set_current_project_name(project.name.clone().into());

    window.set_project_open(true);

    window.set_new_project_dialog_open(false);

    window.set_open_project_dialog_open(false);

    window.set_project_error(String::new().into());

    set_shot_model(window, shots);

    if let Some(first_shot) = shots.first() {
        select_shot(window, first_shot);
    } else {
        clear_selected_shot(window);
    }
}

fn set_shot_model(window: &MainWindow, shots: &[Shot]) {
    let items = shots
        .iter()
        .map(|shot| ShotListItem {
            number: shot.number as i32,

            title: shot.title.clone().into(),

            brief: shot.brief.clone().into(),

            label: format!("{:04}  {}", shot.number, shot.title,).into(),
        })
        .collect::<Vec<_>>();

    let model = ModelRc::new(VecModel::from(items));

    window.set_shots(model);
}

fn select_shot(window: &MainWindow, shot: &Shot) {
    window.set_current_shot_number(shot.number as i32);

    window.set_current_shot_title(shot.title.clone().into());

    window.set_current_shot_brief(shot.brief.clone().into());
}

fn clear_selected_shot(window: &MainWindow) {
    window.set_current_shot_number(0);

    window.set_current_shot_title(String::new().into());

    window.set_current_shot_brief(String::new().into());
}
