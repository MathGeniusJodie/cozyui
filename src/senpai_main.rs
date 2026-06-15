// Standalone terminal chat entry point for the senpai/student pipeline.
mod openrouter;
mod senpai;

fn main() {
    senpai::cli_main();
}
