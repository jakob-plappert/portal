use std::path::Path;
use std::process::Command;

/// Ask the operating system's normal file manager to show a local directory.
/// The language model is not involved and receives no filesystem access; this
/// runs only after the human presses the corresponding UI button.
pub fn open_folder(path: &Path) -> Result<(), String> {
    if !path.is_dir() {
        return Err(format!("Data folder '{}' does not exist.", path.display()));
    }
    let mut command = folder_command(path);
    command
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("Could not open '{}': {error}", path.display()))
}

#[cfg(target_os = "linux")]
fn folder_command(path: &Path) -> Command {
    let mut command = Command::new("xdg-open");
    command.arg(path);
    command
}

#[cfg(target_os = "macos")]
fn folder_command(path: &Path) -> Command {
    let mut command = Command::new("open");
    command.arg(path);
    command
}

#[cfg(target_os = "windows")]
fn folder_command(path: &Path) -> Command {
    let mut command = Command::new("explorer");
    command.arg(path);
    command
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn folder_command(_path: &Path) -> Command {
    Command::new("xdg-open")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_folder_is_rejected_before_launching_a_program() {
        let directory = tempfile::tempdir().expect("temporary directory should exist");
        let error = open_folder(&directory.path().join("missing"))
            .expect_err("missing directory must not launch the file manager");
        assert!(error.contains("does not exist"));
    }
}
