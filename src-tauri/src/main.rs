// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // `gitbuddy credential …` is git asking for a token — answer and exit
    // before any of the GUI starts.
    let args: Vec<String> = std::env::args().collect();
    if let Some(code) = gitbuddy_lib::run_cli(&args) {
        std::process::exit(code);
    }
    gitbuddy_lib::run()
}
