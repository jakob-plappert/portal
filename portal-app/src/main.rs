mod app_state;
mod project;
mod shot;

use std::cell::RefCell;
use std::rc::Rc;

use app_state::AppState;
use project::Project;
use shot::Shot;

use slint::{ComponentHandle, ModelRc, VecModel};

// `build.rs` compiles `ui/main.slint` into Rust code. This macro includes that
// generated code, which is where `MainWindow`, `ProjectListItem`, and
// `ShotListItem` come from even though no handwritten Rust declares them.
slint::include_modules!();

fn main() -> Result<(), slint::PlatformError> {
    // `?` unwraps the successful `MainWindow` or returns its `PlatformError`
    // from `main` immediately. That keeps startup failure handling explicit
    // without a panic.
    let window = MainWindow::new()?;

    // Slint callbacks are retained by the window and all need access to the
    // same state. `Rc` provides shared ownership on this single UI thread,
    // while `RefCell` provides interior mutability: immutable `Rc` handles can
    // request checked mutable borrows at runtime.
    let state = Rc::new(RefCell::new(AppState::default()));

    // `Rc::clone` clones only the inexpensive reference-counted handle, not
    // the `AppState`. Each `move` callback below receives one owning handle to
    // the same allocation.
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
    // The window owns its callbacks. Capturing a strong window handle inside
    // one of those callbacks could form a reference cycle, so Slint supplies a
    // weak handle. `upgrade()` later returns `None` if the window was dropped.
    let weak_window = window.as_weak();

    // `move` transfers the captured `state` and weak handle into the closure,
    // allowing the callback to outlive this setup function.
    window.on_create_project(move |name| {
        // Slint passes its own shared string type across the UI/Rust boundary.
        // Converting to an owned Rust `String` makes the value independent of
        // the callback argument and suitable for persistence code.
        let name = name.to_string();

        // A `Result` forces both filesystem success and failure to be handled.
        match Project::create(&name) {
            Ok(project) => {
                println!(
                    "Created project '{}' at {}",
                    project.name,
                    project.path.display(),
                );

                let shots = Vec::new();

                {
                    // `borrow_mut()` checks RefCell's borrowing rules at
                    // runtime. This narrow scope drops the mutable borrow
                    // before any UI work or another state borrow can occur.
                    let mut state = state.borrow_mut();

                    // This ordinary `clone()` duplicates the Project's owned
                    // strings and path. Unlike `Rc::clone`, it creates an
                    // independent value because both state and this callback
                    // need to keep using the project.
                    state.current_project = Some(project.clone());

                    state.shots = shots.clone();
                }

                // Upgrading a weak handle yields `Option`: the callback only
                // updates the UI while its window still exists.
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

        // `and_then` opens the project first and loads shots only on success.
        // The inner `map` preserves any shot-loading error while pairing both
        // successful owned values for the match below.
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
                    // Keeping the `RefMut` inside this block is important:
                    // holding a RefCell borrow across unrelated callback/UI
                    // work would make later nested borrows panic at runtime.
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

    // The generated `on_refresh_projects` method registers Rust code for the
    // callback declared in Slint. Calling `root.refresh-projects()` in the UI
    // crosses this boundary and invokes the closure synchronously.
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
            // `borrow()` grants temporary read-only access through `RefCell`.
            // Cloning the `Option<Project>` lets the borrow end at this block,
            // before shot creation touches the filesystem.
            let state = state.borrow();

            state.current_project.clone()
        };

        // `let-else` handles the `None` case up front. In the remaining code,
        // `project` is the owned `Project` formerly wrapped by `Some`.
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

                    // The state needs to own the new shot, while `shot` is
                    // still used below to select it in the UI, so this clone
                    // deliberately duplicates its strings.
                    state.shots.push(shot.clone());

                    state.shots.sort_by_key(|shot| shot.number);

                    // Take an owned snapshot so the RefCell borrow ends before
                    // constructing and installing the Slint model.
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
    // Here `?` returns the user-facing error `String` to the caller if listing
    // fails; otherwise `projects` receives the successful vector.
    let projects = Project::list()?;

    // `into_iter()` consumes the vector and moves each Project into this
    // closure. The generated `ProjectListItem` is the small UI-facing shape;
    // paths and other application data stay in Rust.
    let items = projects
        .into_iter()
        .map(|project| ProjectListItem {
            name: project.name.into(),

            folder_name: project.folder_name.into(),
        })
        .collect::<Vec<_>>();

    // Repeated Slint elements consume a model rather than a Rust `Vec`
    // directly. `VecModel` adapts vector storage, and `ModelRc` is Slint's
    // reference-counted model handle used across the Rust/Slint boundary.
    let model = ModelRc::new(VecModel::from(items));

    window.set_available_projects(model);

    Ok(())
}

fn show_project(window: &MainWindow, project: &Project, shots: &[Shot]) {
    // `&Project` and `&[Shot]` are read-only borrows: this function can render
    // the data without taking ownership from AppState or allocating a new
    // collection. Individual strings are cloned only when Slint must own them.
    window.set_current_project_name(project.name.clone().into());

    window.set_project_open(true);

    window.set_new_project_dialog_open(false);

    window.set_open_project_dialog_open(false);

    window.set_project_error(String::new().into());

    set_shot_model(window, shots);

    // `first()` returns `Option<&Shot>` because an empty slice has no item.
    // Pattern matching makes both the populated and empty UI states explicit.
    if let Some(first_shot) = shots.first() {
        select_shot(window, first_shot);
    } else {
        clear_selected_shot(window);
    }
}

fn set_shot_model(window: &MainWindow, shots: &[Shot]) {
    // `iter()` borrows each Shot. The application keeps ownership while this
    // projection copies only the fields the UI needs into generated Slint
    // structs.
    let items = shots
        .iter()
        .map(|shot| ShotListItem {
            number: shot.number as i32,

            title: shot.title.clone().into(),

            brief: shot.brief.clone().into(),

            label: format!("{:04}  {}", shot.number, shot.title,).into(),
        })
        .collect::<Vec<_>>();

    // Replacing this model is intentionally simple: project data remains in
    // AppState, and the UI receives a fresh read-only snapshot after changes.
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
