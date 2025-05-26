use clap::Parser;

#[derive(Debug, Parser)]
#[command(name = "z71200")]
#[command(
    about = "Launches the z71200 UI runtime with required context injected into your target programme."
)]
pub struct Cli {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
    pub command: Vec<String>,
}
