//! `axon-bus`: the same commands as `axon bus`, kept for installed hooks and scripts.

fn main() -> std::process::ExitCode {
    axon_bus::cli_main(std::env::args_os())
}
