//! `forgebox-helper.exe`: the elevated helper of Forgebox. Everything is in the library
//! (`forge_helper`); see `build.rs` for the `requireAdministrator` manifest.

fn main() -> std::process::ExitCode {
    forge_helper::main()
}
