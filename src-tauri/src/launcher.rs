//! Starting the user's editor and terminal on Windows — the counterpart of
//! the `open -a` launches in `commands.rs`, which stay as they are on macOS.
//!
//! Nothing here goes through a shell: the configured command is split into a
//! program and its arguments, the program is resolved on `PATH` with the
//! `which` crate, and it is spawned directly. `which` honours `PATHEXT`,
//! which `std::process::Command` doesn't (it only tries `.exe`), so `code`
//! finds VS Code's `code.cmd`. Batch files are safe to spawn: Rust ≥ 1.77.2
//! escapes their arguments or refuses with `InvalidInput`.
//!
//! The parsing is pure and tested on every platform; only the spawning is
//! Windows-only.

// Outside Windows only the tests use the parsing.
#![cfg_attr(not(windows), allow(dead_code))]

use std::path::{Path, PathBuf};

/// A program and its arguments, parsed from a command line in Settings.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct CommandLine {
    pub program: String,
    pub args: Vec<String>,
}

/// Split a configured command into program and arguments:
/// - a leading `"…"` is the program, so a path with spaces can be quoted:
///   `"C:\Program Files\Microsoft VS Code\Code.exe" --new-window`;
/// - an unquoted string that names an existing file as a whole is the
///   program, spaces and all (a pasted path without quotes);
/// - anything else is split on whitespace: `code --new-window`.
///
/// `None` for an empty or whitespace-only command. `is_file` is the
/// filesystem check, passed in to keep this testable.
pub(crate) fn parse_command_line(cmd: &str, is_file: impl Fn(&str) -> bool) -> Option<CommandLine> {
    let trimmed = cmd.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(rest) = trimmed.strip_prefix('"') {
        // An unclosed quote takes the rest of the string as the program.
        let (program, tail) = rest.split_once('"').unwrap_or((rest, ""));
        let program = program.trim();
        if program.is_empty() {
            return None;
        }
        return Some(CommandLine {
            program: program.to_owned(),
            args: tail.split_whitespace().map(str::to_owned).collect(),
        });
    }
    if trimmed.contains(char::is_whitespace) && is_file(trimmed) {
        return Some(CommandLine {
            program: trimmed.to_owned(),
            args: Vec::new(),
        });
    }
    let mut parts = trimmed.split_whitespace().map(str::to_owned);
    Some(CommandLine {
        program: parts.next()?,
        args: parts.collect(),
    })
}

/// How the configured terminal opens a folder.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum TerminalLaunch {
    /// Windows Terminal: `wt -d <folder>` opens a new window there.
    WindowsTerminal,
    /// Git for Windows' Git Bash: `git-bash.exe --cd=<folder>`.
    GitBash,
    /// A shell or any other program, started in the folder in a console of
    /// its own: `pwsh`, `powershell`, `cmd`, a full path to something else.
    InFolder(CommandLine),
}

/// Map the terminal setting to how it is launched. The presets match without
/// regard to case or a trailing `.exe`; everything else is a command line.
pub(crate) fn classify_terminal(
    cmd: &str,
    is_file: impl Fn(&str) -> bool,
) -> Option<TerminalLaunch> {
    let name = cmd.trim().to_ascii_lowercase();
    let name = name.strip_suffix(".exe").unwrap_or(&name);
    match name {
        "" => None,
        "wt" | "windows terminal" => Some(TerminalLaunch::WindowsTerminal),
        "git bash" | "git-bash" => Some(TerminalLaunch::GitBash),
        _ => parse_command_line(cmd, is_file).map(TerminalLaunch::InFolder),
    }
}

/// `path` as Windows Terminal's `-d` value. `wt` reads `;` as the separator
/// between its own subcommands, even inside one argument, so a folder name
/// containing one has to escape it.
pub(crate) fn wt_dir_arg(path: &str) -> String {
    path.replace(';', r"\;")
}

/// `git-bash.exe` of the Git for Windows install that `git_exe` belongs to.
/// `git.exe` sits in `cmd\`, `bin\` or `mingw64\bin\` below the install
/// root, which holds `git-bash.exe`.
pub(crate) fn git_bash_beside(git_exe: &Path, exists: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    git_exe
        .ancestors()
        .skip(1)
        .take(3)
        .map(|dir| dir.join("git-bash.exe"))
        .find(|candidate| exists(candidate))
}

#[cfg(windows)]
mod spawn {
    use std::os::windows::process::CommandExt;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use super::{classify_terminal, git_bash_beside, parse_command_line, wt_dir_arg};
    use super::{CommandLine, TerminalLaunch};

    // From the Win32 process-creation flags (winbase.h).
    /// A console program gets a console window of its own.
    const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
    /// A console program runs without one — for the `cmd.exe` that runs a
    /// `.cmd` shim like VS Code's `code.cmd`, which would otherwise flash.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    fn is_file(path: &str) -> bool {
        Path::new(path).is_file()
    }

    fn resolve(program: &str, what: &str) -> Result<PathBuf, String> {
        which::which(program).map_err(|_| {
            format!("Couldn't find the {what} `{program}`. Put it on PATH or enter its full path in Settings.")
        })
    }

    fn is_batch(program: &Path) -> bool {
        program
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("cmd") || ext.eq_ignore_ascii_case("bat"))
    }

    fn start(mut command: Command, what: &str, program: &Path) -> Result<(), String> {
        // Editors and terminals outlive the call; never wait on them.
        command
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("Starting the {what} `{}` failed: {e}", program.display()))
    }

    /// Open `path` in the configured editor, with the path as the last
    /// argument.
    pub(crate) fn open_editor(cmd: &str, path: &str) -> Result<(), String> {
        let CommandLine { program, args } = parse_command_line(cmd, is_file)
            .ok_or("No editor command configured. Set one in Settings.")?;
        let program = resolve(&program, "editor")?;
        let mut command = Command::new(&program);
        command.args(&args).arg(path);
        if is_batch(&program) {
            command.creation_flags(CREATE_NO_WINDOW);
        }
        start(command, "editor", &program)
    }

    /// Open a terminal in the folder `path`.
    pub(crate) fn open_terminal(cmd: &str, path: &str) -> Result<(), String> {
        let launch = classify_terminal(cmd, is_file)
            .ok_or("No terminal configured. Set one in Settings.")?;
        match launch {
            TerminalLaunch::WindowsTerminal => {
                let program = resolve("wt", "terminal")?;
                let mut command = Command::new(&program);
                command.arg("-d").arg(wt_dir_arg(path));
                start(command, "terminal", &program)
            }
            TerminalLaunch::GitBash => {
                let program = find_git_bash()?;
                let mut command = Command::new(&program);
                command.arg(format!("--cd={path}"));
                start(command, "terminal", &program)
            }
            // The installed app has no standard handles (GUI subsystem), so a
            // shell started here attaches to its new console. A debug build
            // is a console program and passes on its own handles, the pipes
            // of `tauri dev`, and `cmd` then exits at once: try terminal
            // launches with a release build.
            TerminalLaunch::InFolder(CommandLine { program, args }) => {
                let program = resolve(&program, "terminal")?;
                let mut command = Command::new(&program);
                command
                    .args(&args)
                    .current_dir(path)
                    .creation_flags(CREATE_NEW_CONSOLE);
                start(command, "terminal", &program)
            }
        }
    }

    /// Git Bash is rarely on `PATH` itself, but `git` is: look beside it,
    /// then in the default install folder.
    fn find_git_bash() -> Result<PathBuf, String> {
        which::which("git-bash")
            .ok()
            .or_else(|| {
                which::which("git")
                    .ok()
                    .and_then(|git| git_bash_beside(&git, Path::exists))
            })
            .or_else(|| {
                std::env::var_os("ProgramFiles")
                    .map(|dir| Path::new(&dir).join("Git").join("git-bash.exe"))
                    .filter(|candidate| candidate.exists())
            })
            .ok_or_else(|| "Couldn't find Git Bash. Is Git for Windows installed?".to_owned())
    }
}

#[cfg(windows)]
pub(crate) use spawn::{open_editor, open_terminal};

#[cfg(test)]
mod tests {
    use super::*;

    /// Opens real windows, so it only runs by hand (WS10 in
    /// docs/plans/2026-11-21-windows-port.md):
    ///
    /// ```text
    /// $env:GITBUDDY_SMOKE_EDITOR = 'code'
    /// $env:GITBUDDY_SMOKE_TERMINALS = 'wt|pwsh|cmd|Git Bash'
    /// cargo test --lib launcher::tests::smoke -- --ignored
    /// ```
    ///
    /// Each configured program should open on the repository root.
    #[cfg(windows)]
    #[test]
    #[ignore = "opens editor and terminal windows; run by hand"]
    fn smoke_opens_the_configured_programs() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let root = root.to_str().unwrap();
        if let Ok(editor) = std::env::var("GITBUDDY_SMOKE_EDITOR") {
            open_editor(&editor, root).unwrap();
        }
        if let Ok(terminals) = std::env::var("GITBUDDY_SMOKE_TERMINALS") {
            for terminal in terminals.split('|') {
                open_terminal(terminal, root).unwrap();
            }
        }
    }

    fn line(program: &str, args: &[&str]) -> Option<CommandLine> {
        Some(CommandLine {
            program: program.to_owned(),
            args: args.iter().map(|a| (*a).to_owned()).collect(),
        })
    }

    const NO_FILES: fn(&str) -> bool = |_| false;

    #[test]
    fn plain_commands_split_on_whitespace() {
        assert_eq!(parse_command_line("code", NO_FILES), line("code", &[]));
        assert_eq!(
            parse_command_line("  code   --new-window ", NO_FILES),
            line("code", &["--new-window"])
        );
        assert_eq!(parse_command_line("", NO_FILES), None);
        assert_eq!(parse_command_line("   ", NO_FILES), None);
    }

    #[test]
    fn a_quoted_program_keeps_its_spaces() {
        assert_eq!(
            parse_command_line(
                r#""C:\Program Files\Microsoft VS Code\Code.exe" --new-window"#,
                NO_FILES
            ),
            line(
                r"C:\Program Files\Microsoft VS Code\Code.exe",
                &["--new-window"]
            )
        );
        // Unclosed: the rest is the program.
        assert_eq!(
            parse_command_line(r#""C:\Tools\My Editor\ed.exe"#, NO_FILES),
            line(r"C:\Tools\My Editor\ed.exe", &[])
        );
        assert_eq!(parse_command_line(r#""" --flag"#, NO_FILES), None);
    }

    #[test]
    fn an_unquoted_path_to_a_file_is_one_program() {
        let path = r"C:\Program Files\Microsoft VS Code\Code.exe";
        assert_eq!(parse_command_line(path, |p| p == path), line(path, &[]));
        // Not a file: split as usual, and resolving `C:\Program` fails later
        // with a message naming it.
        assert_eq!(
            parse_command_line(path, NO_FILES),
            line(r"C:\Program", &["Files\\Microsoft", "VS", r"Code\Code.exe"])
        );
    }

    #[test]
    fn terminal_presets_ignore_case_and_exe() {
        for wt in ["wt", "WT", "wt.exe", "Windows Terminal", " wt "] {
            assert_eq!(
                classify_terminal(wt, NO_FILES),
                Some(TerminalLaunch::WindowsTerminal),
                "{wt:?}"
            );
        }
        for bash in ["Git Bash", "git-bash", "git-bash.exe"] {
            assert_eq!(
                classify_terminal(bash, NO_FILES),
                Some(TerminalLaunch::GitBash),
                "{bash:?}"
            );
        }
        assert_eq!(
            classify_terminal("pwsh", NO_FILES),
            Some(TerminalLaunch::InFolder(CommandLine {
                program: "pwsh".into(),
                args: vec![],
            }))
        );
        assert_eq!(
            classify_terminal("cmd.exe /k title repo", NO_FILES),
            Some(TerminalLaunch::InFolder(CommandLine {
                program: "cmd.exe".into(),
                args: vec!["/k".into(), "title".into(), "repo".into()],
            }))
        );
        assert_eq!(classify_terminal("  ", NO_FILES), None);
    }

    #[test]
    fn windows_terminal_gets_semicolons_escaped() {
        assert_eq!(wt_dir_arg(r"C:\src\repo"), r"C:\src\repo");
        assert_eq!(wt_dir_arg(r"C:\src\a;b"), r"C:\src\a\;b");
    }

    #[test]
    fn git_bash_is_found_at_the_install_root() {
        let root = Path::new(r"C:\Program Files\Git");
        let bash = root.join("git-bash.exe");
        let exists = |p: &Path| p == bash;
        for git in [
            root.join("cmd").join("git.exe"),
            root.join("bin").join("git.exe"),
            root.join("mingw64").join("bin").join("git.exe"),
        ] {
            assert_eq!(git_bash_beside(&git, exists), Some(bash.clone()), "{git:?}");
        }
        assert_eq!(
            git_bash_beside(Path::new(r"C:\scoop\shims\git.exe"), exists),
            None
        );
    }
}
