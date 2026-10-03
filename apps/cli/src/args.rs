use std::path::PathBuf;

use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum};
use jev_core::{ChoiceOption, ModelId, QuestionId, StateFormat};

const DEFAULT_MODEL: &str = "typesafe/jev-1.13";
const DEFAULT_QUESTION_ID: &str = "answer";

#[derive(Debug, Parser)]
#[command(
    name = "jev",
    version,
    about = "Ask Jev for typed decisions about text or structured state",
    group(ArgGroup::new("state_source").args(["state", "state_file"]).multiple(false)),
    after_help = "State is limited to 1 MiB from every source. Use --state-format to choose \
                  interpretation. Input, connect, and request deadlines are separate. JSON \
                  output goes to stdout; diagnostics go to stderr.\n\n\
                  Examples:\n  \
                  jev noul 'Is this a bug?' --state 'The save button fails'\n  \
                  jev choice 'Which team?' --option billing=Payments --option technical=Product\n  \
                  jev score 'How urgent?' --level Low --level High\n  \
                  jev --dry-run noul 'Is this safe?' --state 'Read-only operation'"
)]
pub struct Cli {
    /// State text or JSON. Shell argument limits still apply to this form.
    #[arg(long, global = true, value_name = "TEXT_OR_JSON")]
    pub state: Option<String>,

    /// Read state from a file. Use '-' to read stdin explicitly.
    #[arg(long, global = true, value_name = "PATH")]
    pub state_file: Option<PathBuf>,

    /// Interpret state as auto-detected JSON, plain text, or strict JSON.
    #[arg(long, global = true, value_enum, default_value = "auto")]
    pub state_format: StateFormatArg,

    /// `OpenRouter` model ID. Defaults to the pinned Jev model version.
    #[arg(long, global = true, default_value = DEFAULT_MODEL)]
    pub model: ModelId,

    /// Key used to find this answer in the output's answers object.
    #[arg(long, global = true, default_value = DEFAULT_QUESTION_ID)]
    pub id: QuestionId,

    /// Format JSON as compact one-line or indented output.
    #[arg(long, global = true, value_enum, default_value = "json")]
    pub output: OutputMode,

    /// Emit only the answers map, omitting request and usage metadata.
    #[arg(long, global = true)]
    pub answers_only: bool,

    /// Validate and print the exact request JSON without credentials or a network call.
    /// The preview includes the state supplied on the command line, file, or stdin.
    #[arg(long, global = true)]
    pub dry_run: bool,

    /// Maximum time to acquire file or stdin state, in seconds (1 to 300).
    #[arg(long, global = true, default_value_t = 30, value_parser = clap::value_parser!(u64).range(1..=300))]
    pub input_timeout: u64,

    /// Connection deadline, in seconds (1 to 120).
    #[arg(long, global = true, default_value_t = 15, value_parser = clap::value_parser!(u64).range(1..=120))]
    pub connect_timeout: u64,

    /// Overall HTTP request deadline, in seconds (1 to 600).
    #[arg(long, global = true, default_value_t = 90, value_parser = clap::value_parser!(u64).range(1..=600))]
    pub request_timeout: u64,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum StateFormatArg {
    /// Decode valid JSON strings, objects, and arrays; preserve malformed JSON as text.
    Auto,
    /// Preserve all non-whitespace input as a string.
    Text,
    /// Require a JSON string, object, or array.
    Json,
}

impl From<StateFormatArg> for StateFormat {
    fn from(format: StateFormatArg) -> Self {
        match format {
            StateFormatArg::Auto => Self::Auto,
            StateFormatArg::Text => Self::Text,
            StateFormatArg::Json => Self::Json,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum OutputMode {
    /// Compact JSON on one line, suitable for pipes and agent parsing.
    Json,
    /// Indented JSON for interactive inspection.
    Pretty,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Ask whether a condition holds (Noul).
    Noul(NoulArgs),
    /// Select one option from a fixed set (Choice).
    Choice(ChoiceArgs),
    /// Place the state on an ordered scale (Score).
    Score(ScoreArgs),
}

#[derive(Debug, Args)]
pub struct NoulArgs {
    /// The yes-or-no judgment Jev should make.
    #[arg(value_name = "QUESTION")]
    pub instructions: String,

    /// Description of when the answer is true. Must pair with --false-criteria.
    #[arg(long, value_name = "DESCRIPTION", requires = "false_criteria")]
    pub true_criteria: Option<String>,

    /// Description of when the answer is false. Must pair with --true-criteria.
    #[arg(long, value_name = "DESCRIPTION", requires = "true_criteria")]
    pub false_criteria: Option<String>,
}

#[derive(Debug, Args)]
pub struct ChoiceArgs {
    /// The question Jev should answer by selecting one option.
    #[arg(value_name = "QUESTION")]
    pub instructions: String,

    /// Candidate in ID=DESCRIPTION form. Repeat at least twice (2 to 64 options).
    #[arg(long = "option", value_name = "ID=DESCRIPTION", required = true)]
    pub options: Vec<ChoiceOption>,
}

#[derive(Debug, Args)]
pub struct ScoreArgs {
    /// The question Jev should answer on the ordered scale.
    #[arg(value_name = "QUESTION")]
    pub instructions: String,

    /// Ordered level description from lowest (index 0) to highest; repeat 2 to 32 times.
    #[arg(long = "level", value_name = "DESCRIPTION", required = true)]
    pub levels: Vec<String>,
}
