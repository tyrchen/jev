//! Validated question types and input handling for the Jev decision model.

#![forbid(unsafe_code)]
#![warn(rust_2024_compatibility, missing_docs, missing_debug_implementations)]

use std::{collections::BTreeMap, fmt, str::FromStr};

use serde::{
    Serialize,
    de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor},
};
use serde_json::{Map, Number, Value};
use thiserror::Error;

/// Maximum state input size, in bytes, shared by every input source.
pub const MAX_STATE_BYTES: usize = 1_048_576;
/// Maximum nesting depth accepted in state JSON.
pub const MAX_STATE_DEPTH: usize = 64;
/// Maximum number of JSON values accepted in state JSON.
pub const MAX_STATE_NODES: usize = 100_000;
/// Maximum question instruction length, in UTF-8 bytes.
pub const MAX_INSTRUCTIONS_BYTES: usize = 4_096;
/// Maximum criterion or option description length, in UTF-8 bytes.
pub const MAX_CRITERION_BYTES: usize = 2_048;
/// Maximum options in a Choice question.
pub const MAX_CHOICE_OPTIONS: usize = 64;
/// Maximum levels in a Score question.
pub const MAX_SCORE_LEVELS: usize = 32;
/// Maximum ID length, in ASCII bytes.
pub const MAX_ID_BYTES: usize = 64;
/// Maximum model ID length, in ASCII bytes.
pub const MAX_MODEL_ID_BYTES: usize = 256;

/// Errors raised while validating Jev input.
#[derive(Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum InputError {
    /// A text field is empty or longer than its byte limit.
    #[error("{field} must be non-empty and at most {max_bytes} bytes (got {actual_bytes})")]
    InvalidText {
        /// Name of the invalid field.
        field: String,
        /// Maximum accepted UTF-8 byte length.
        max_bytes: usize,
        /// Actual UTF-8 byte length.
        actual_bytes: usize,
    },
    /// A question ID contains an unsupported character or length.
    #[error(
        "question ID must use 1 to {max_bytes} ASCII letters, digits, underscore, or hyphen (got \
         {actual_bytes} bytes)"
    )]
    InvalidQuestionId {
        /// Maximum accepted ID length.
        max_bytes: usize,
        /// Actual ID length.
        actual_bytes: usize,
    },
    /// A model ID contains whitespace, control characters, or unsupported syntax.
    #[error(
        "model ID must use 1 to {max_bytes} ASCII letters, digits, slash, dot, colon, underscore, \
         hyphen, or tilde (got {actual_bytes} bytes)"
    )]
    InvalidModelId {
        /// Maximum accepted model ID length.
        max_bytes: usize,
        /// Actual model ID length.
        actual_bytes: usize,
    },
    /// A Choice option does not use ID=DESCRIPTION syntax.
    #[error("choice options must use ID=DESCRIPTION syntax")]
    InvalidChoiceOption,
    /// A Choice option ID is invalid.
    #[error(
        "Choice option ID must use 1 to {max_bytes} ASCII letters, digits, underscore, or hyphen \
         (got {actual_bytes} bytes)"
    )]
    InvalidChoiceOptionId {
        /// Maximum accepted option ID length.
        max_bytes: usize,
        /// Actual option ID length.
        actual_bytes: usize,
    },
    /// Choice option IDs are repeated.
    #[error("Choice option at position {index} repeats an earlier ID")]
    DuplicateChoiceOptionId {
        /// One-based position of the duplicate option.
        index: usize,
    },
    /// A question has an invalid number of options or levels.
    #[error("{question} requires between {min} and {max} entries (got {actual})")]
    InvalidCount {
        /// Question primitive name.
        question: &'static str,
        /// Minimum accepted entry count.
        min: usize,
        /// Maximum accepted entry count.
        max: usize,
        /// Supplied entry count.
        actual: usize,
    },
    /// Input state exceeds its byte limit.
    #[error("state exceeds the {max_bytes}-byte limit (got {actual_bytes} bytes)")]
    StateTooLarge {
        /// Maximum accepted input size in bytes.
        max_bytes: usize,
        /// Observed input size in bytes.
        actual_bytes: usize,
    },
    /// Input state is empty or contains only whitespace.
    #[error("state must contain non-whitespace text")]
    EmptyState,
    /// State JSON is malformed in strict JSON mode.
    #[error("state is not valid JSON")]
    InvalidJson,
    /// State JSON contains a repeated object key.
    #[error("state JSON contains a duplicate object key")]
    DuplicateJsonKey,
    /// State JSON exceeds the configured depth or node count.
    #[error("state JSON exceeds the {limit} limit")]
    StateJsonLimit {
        /// The exceeded resource limit.
        limit: &'static str,
    },
    /// Strict JSON mode does not accept scalar booleans, numbers, or null.
    #[error("JSON state must be a string, object, or array; scalar {kind} is not accepted")]
    UnsupportedJsonScalar {
        /// Scalar JSON kind.
        kind: &'static str,
    },
}

/// Controls how state input is interpreted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateFormat {
    /// Decode valid JSON strings, objects, and arrays; preserve syntax-invalid input as text.
    Auto,
    /// Preserve the input as a JSON string.
    Text,
    /// Require JSON containing a string, object, or array.
    Json,
}

/// A validated state value accepted by the Decisions API.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct State(Value);

impl State {
    /// Parses state text with a shared byte limit and, when parsed as JSON, depth and node limits.
    ///
    /// JSON object keys must be unique in auto and strict JSON modes. Text mode does not parse
    /// the input. In auto mode malformed JSON is preserved as text; strict JSON mode rejects it.
    ///
    /// # Errors
    ///
    /// Returns an error for empty, oversized, ambiguous, or unsupported state.
    ///
    /// # Examples
    ///
    /// ```
    /// let state = jev_core::State::parse("{\"ready\":true}", jev_core::StateFormat::Json)?;
    /// assert_eq!(state.as_value(), &serde_json::json!({"ready":true}));
    /// # Ok::<(), jev_core::InputError>(())
    /// ```
    pub fn parse(input: &str, format: StateFormat) -> Result<Self, InputError> {
        if input.len() > MAX_STATE_BYTES {
            return Err(InputError::StateTooLarge {
                max_bytes: MAX_STATE_BYTES,
                actual_bytes: input.len(),
            });
        }
        if input.trim().is_empty() {
            return Err(InputError::EmptyState);
        }
        if format == StateFormat::Text {
            return Ok(Self(Value::String(input.to_owned())));
        }
        match parse_bounded_json(input) {
            Ok(value) => {
                let value = format_json_state(value, format, input)?;
                Self::from_value(value)
            }
            Err(error) if format == StateFormat::Auto && is_json_syntax_error(&error) => {
                Ok(Self(Value::String(input.to_owned())))
            }
            Err(error) => Err(map_json_error(&error)),
        }
    }

    /// Validates and wraps a dynamic JSON value.
    ///
    /// # Errors
    ///
    /// Returns an error for unsupported root values, excessive nesting, or too many nodes.
    ///
    /// # Examples
    ///
    /// ```
    /// let state = jev_core::State::from_value(serde_json::json!(["one", "two"]))?;
    /// assert_eq!(state.as_value(), &serde_json::json!(["one", "two"]));
    /// # Ok::<(), jev_core::InputError>(())
    /// ```
    pub fn from_value(value: Value) -> Result<Self, InputError> {
        validate_value_limits(&value)?;
        match &value {
            Value::String(text) if !text.trim().is_empty() => {}
            Value::Object(_) | Value::Array(_) => {}
            Value::String(_) => return Err(InputError::EmptyState),
            Value::Null => return Err(InputError::UnsupportedJsonScalar { kind: "null" }),
            Value::Bool(_) => return Err(InputError::UnsupportedJsonScalar { kind: "boolean" }),
            Value::Number(_) => return Err(InputError::UnsupportedJsonScalar { kind: "number" }),
        }
        let encoded = serde_json::to_vec(&value).map_err(|_| InputError::InvalidJson)?;
        if encoded.len() > MAX_STATE_BYTES {
            return Err(InputError::StateTooLarge {
                max_bytes: MAX_STATE_BYTES,
                actual_bytes: encoded.len(),
            });
        }
        Ok(Self(value))
    }

    /// Returns the state value for request construction.
    #[must_use]
    ///
    /// # Examples
    ///
    /// ```
    /// let state = jev_core::State::parse("notes", jev_core::StateFormat::Text)?;
    /// assert_eq!(state.as_value(), &serde_json::json!("notes"));
    /// # Ok::<(), jev_core::InputError>(())
    /// ```
    pub const fn as_value(&self) -> &Value {
        &self.0
    }
}

fn format_json_state(value: Value, format: StateFormat, input: &str) -> Result<Value, InputError> {
    match (format, value) {
        (StateFormat::Auto, Value::Bool(_) | Value::Number(_) | Value::Null) => {
            Ok(Value::String(input.to_owned()))
        }
        (_, Value::Bool(_)) => Err(InputError::UnsupportedJsonScalar { kind: "boolean" }),
        (_, Value::Number(_)) => Err(InputError::UnsupportedJsonScalar { kind: "number" }),
        (_, Value::Null) => Err(InputError::UnsupportedJsonScalar { kind: "null" }),
        (_, value) => Ok(value),
    }
}

fn validate_value_limits(value: &Value) -> Result<(), InputError> {
    let mut pending = vec![(value, 0_usize)];
    let mut nodes = 0_usize;
    while let Some((current, depth)) = pending.pop() {
        if depth > MAX_STATE_DEPTH {
            return Err(InputError::StateJsonLimit { limit: "depth" });
        }
        nodes = nodes.checked_add(1).ok_or(InputError::StateJsonLimit {
            limit: "node count",
        })?;
        if nodes > MAX_STATE_NODES {
            return Err(InputError::StateJsonLimit {
                limit: "node count",
            });
        }
        match current {
            Value::Array(values) => pending.extend(values.iter().map(|item| (item, depth + 1))),
            Value::Object(values) => pending.extend(values.values().map(|item| (item, depth + 1))),
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        }
    }
    Ok(())
}

fn parse_bounded_json(input: &str) -> Result<Value, serde_json::Error> {
    let mut deserializer = serde_json::Deserializer::from_str(input);
    let mut node_count = 0;
    let value = BoundedValueSeed {
        depth: 0,
        nodes: &mut node_count,
    }
    .deserialize(&mut deserializer)?;
    deserializer.end()?;
    Ok(value)
}

struct BoundedValueSeed<'a> {
    depth: usize,
    nodes: &'a mut usize,
}

impl<'de> DeserializeSeed<'de> for BoundedValueSeed<'_> {
    type Value = Value;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        if self.depth > MAX_STATE_DEPTH {
            return Err(de::Error::custom("jev-policy-depth"));
        }
        *self.nodes = self
            .nodes
            .checked_add(1)
            .ok_or_else(|| de::Error::custom("jev-policy-nodes"))?;
        if *self.nodes > MAX_STATE_NODES {
            return Err(de::Error::custom("jev-policy-nodes"));
        }
        deserializer.deserialize_any(BoundedValueVisitor {
            depth: self.depth,
            nodes: self.nodes,
        })
    }
}

struct BoundedValueVisitor<'a> {
    depth: usize,
    nodes: &'a mut usize,
}

impl<'de> Visitor<'de> for BoundedValueVisitor<'_> {
    type Value = Value;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON value")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(Value::Bool(value))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
        Ok(Value::Number(Number::from(value)))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(Value::Number(Number::from(value)))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| E::custom("invalid JSON number"))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
        Ok(Value::String(value.to_owned()))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(Value::String(value))
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(Value::Null)
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(Value::Null)
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element_seed(BoundedValueSeed {
            depth: self.depth + 1,
            nodes: &mut *self.nodes,
        })? {
            values.push(value);
        }
        Ok(Value::Array(values))
    }

    fn visit_map<A>(self, mut object: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut values = Map::new();
        while let Some(key) = object.next_key::<String>()? {
            if values.contains_key(&key) {
                return Err(de::Error::custom("jev-policy-duplicate"));
            }
            let value = object.next_value_seed(BoundedValueSeed {
                depth: self.depth + 1,
                nodes: &mut *self.nodes,
            })?;
            values.insert(key, value);
        }
        Ok(Value::Object(values))
    }
}

fn is_json_syntax_error(error: &serde_json::Error) -> bool {
    matches!(
        error.classify(),
        serde_json::error::Category::Syntax | serde_json::error::Category::Eof
    )
}

fn map_json_error(error: &serde_json::Error) -> InputError {
    let message = error.to_string();
    if message.contains("jev-policy-duplicate") {
        InputError::DuplicateJsonKey
    } else if message.contains("jev-policy-depth") {
        InputError::StateJsonLimit { limit: "depth" }
    } else if message.contains("jev-policy-nodes") {
        InputError::StateJsonLimit {
            limit: "node count",
        }
    } else {
        InputError::InvalidJson
    }
}

/// Parses a JSON document with the shared byte, nesting, node, and duplicate-key limits.
///
/// The returned value remains dynamic because response envelopes and state objects are
/// intentionally extensible. Callers validate known fields after parsing.
///
/// # Errors
///
/// Returns an error for malformed JSON, duplicate object keys, or exceeded resource limits.
///
/// # Examples
///
/// ```
/// let value = jev_core::parse_bounded_json_value(r#"{"ready":true}"#)?;
/// assert_eq!(value, serde_json::json!({"ready":true}));
/// # Ok::<(), jev_core::InputError>(())
/// ```
pub fn parse_bounded_json_value(input: &str) -> Result<Value, InputError> {
    if input.len() > MAX_STATE_BYTES {
        return Err(InputError::StateTooLarge {
            max_bytes: MAX_STATE_BYTES,
            actual_bytes: input.len(),
        });
    }
    parse_bounded_json(input).map_err(|error| map_json_error(&error))
}

/// A validated Choice option with an immutable ID and description.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ChoiceOption {
    id: String,
    description: String,
}

impl ChoiceOption {
    /// Creates an option from its stable ID and description.
    ///
    /// # Errors
    ///
    /// Returns an error if the ID or description is invalid.
    ///
    /// # Examples
    ///
    /// ```
    /// let option = jev_core::ChoiceOption::new("billing", "Billing issues")?;
    /// assert_eq!(option.id(), "billing");
    /// # Ok::<(), jev_core::InputError>(())
    /// ```
    pub fn new(id: impl Into<String>, description: impl Into<String>) -> Result<Self, InputError> {
        let id = id.into();
        let description = description.into();
        validate_question_id(&id).map_err(|_| InputError::InvalidChoiceOptionId {
            max_bytes: MAX_ID_BYTES,
            actual_bytes: id.len(),
        })?;
        validate_text(
            &description,
            MAX_CRITERION_BYTES,
            "Choice option description",
        )?;
        Ok(Self { id, description })
    }

    /// Returns the option's stable ID.
    #[must_use]
    ///
    /// # Examples
    ///
    /// ```
    /// let option = jev_core::ChoiceOption::new("billing", "Payments")?;
    /// assert_eq!(option.id(), "billing");
    /// # Ok::<(), jev_core::InputError>(())
    /// ```
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the option's description.
    #[must_use]
    ///
    /// # Examples
    ///
    /// ```
    /// let option = jev_core::ChoiceOption::new("billing", "Payments")?;
    /// assert_eq!(option.description(), "Payments");
    /// # Ok::<(), jev_core::InputError>(())
    /// ```
    pub fn description(&self) -> &str {
        &self.description
    }
}

impl FromStr for ChoiceOption {
    type Err = InputError;

    /// Parses an option in ID=DESCRIPTION form.
    ///
    /// # Errors
    ///
    /// Returns an error when the separator or either valid field is missing.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::str::FromStr;
    ///
    /// let option = jev_core::ChoiceOption::from_str("billing=Payments=and invoices")?;
    /// assert_eq!(option.description(), "Payments=and invoices");
    /// # Ok::<(), jev_core::InputError>(())
    /// ```
    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let (id, description) = input
            .split_once('=')
            .ok_or(InputError::InvalidChoiceOption)?;
        Self::new(id, description)
    }
}

/// A validated pair of positive and negative Noul criteria.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct NoulCriteria {
    true_description: String,
    false_description: String,
}

impl NoulCriteria {
    /// Creates both Noul criterion descriptions.
    ///
    /// # Errors
    ///
    /// Returns an error if either description is empty or too long.
    ///
    /// # Examples
    ///
    /// ```
    /// let criteria = jev_core::NoulCriteria::new("Read-only", "Changes state")?;
    /// assert_eq!(criteria.true_description(), "Read-only");
    /// # Ok::<(), jev_core::InputError>(())
    /// ```
    pub fn new(
        true_description: impl Into<String>,
        false_description: impl Into<String>,
    ) -> Result<Self, InputError> {
        let true_description = true_description.into();
        let false_description = false_description.into();
        validate_text(&true_description, MAX_CRITERION_BYTES, "true criterion")?;
        validate_text(&false_description, MAX_CRITERION_BYTES, "false criterion")?;
        Ok(Self {
            true_description,
            false_description,
        })
    }

    /// Returns the positive-answer criterion.
    #[must_use]
    ///
    /// # Examples
    ///
    /// ```
    /// let criteria = jev_core::NoulCriteria::new("Read-only", "Changes state")?;
    /// assert_eq!(criteria.true_description(), "Read-only");
    /// # Ok::<(), jev_core::InputError>(())
    /// ```
    pub fn true_description(&self) -> &str {
        &self.true_description
    }

    /// Returns the negative-answer criterion.
    #[must_use]
    ///
    /// # Examples
    ///
    /// ```
    /// let criteria = jev_core::NoulCriteria::new("Read-only", "Changes state")?;
    /// assert_eq!(criteria.false_description(), "Changes state");
    /// # Ok::<(), jev_core::InputError>(())
    /// ```
    pub fn false_description(&self) -> &str {
        &self.false_description
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum QuestionKind {
    Noul(Option<NoulCriteria>),
    Choice(Vec<ChoiceOption>),
    Score(Vec<String>),
}

/// The primitive used by a decision question.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuestionPrimitive {
    /// Yes-or-no decision.
    Noul,
    /// Select one option from a fixed set.
    Choice,
    /// Place state on an ordered scale.
    Score,
}

/// A question whose fields can only be created through validated constructors.
///
/// ```compile_fail
/// let question = jev_core::DecisionQuestion::Noul {
///     instructions: String::new(),
///     true_criteria: Some("yes".to_owned()),
///     false_criteria: None,
/// };
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct DecisionQuestion {
    instructions: String,
    kind: QuestionKind,
}

impl DecisionQuestion {
    /// Creates a Noul question with optional paired criteria.
    ///
    /// # Errors
    ///
    /// Returns an error when instructions or either criterion is invalid.
    ///
    /// # Examples
    ///
    /// ```
    /// let question = jev_core::DecisionQuestion::noul("Is this safe?", None)?;
    /// assert_eq!(question.primitive(), jev_core::QuestionPrimitive::Noul);
    /// # Ok::<(), jev_core::InputError>(())
    /// ```
    pub fn noul(
        instructions: impl Into<String>,
        criteria: Option<NoulCriteria>,
    ) -> Result<Self, InputError> {
        Ok(Self {
            instructions: validate_instructions(instructions.into())?,
            kind: QuestionKind::Noul(criteria),
        })
    }

    /// Creates a Choice question with at least two uniquely identified options.
    ///
    /// # Errors
    ///
    /// Returns an error when instructions, option count, or IDs are invalid.
    ///
    /// # Examples
    ///
    /// ```
    /// let question = jev_core::DecisionQuestion::choice(
    ///     "Which team owns this?",
    ///     vec![
    ///         jev_core::ChoiceOption::new("billing", "Payments")?,
    ///         jev_core::ChoiceOption::new("technical", "Product issues")?,
    ///     ],
    /// )?;
    /// assert_eq!(question.choice_options().len(), 2);
    /// # Ok::<(), jev_core::InputError>(())
    /// ```
    pub fn choice(
        instructions: impl Into<String>,
        options: Vec<ChoiceOption>,
    ) -> Result<Self, InputError> {
        let instructions = validate_instructions(instructions.into())?;
        validate_count("Choice", options.len(), 2, MAX_CHOICE_OPTIONS)?;
        let mut ids = BTreeMap::new();
        for (index, option) in options.iter().enumerate() {
            if ids.insert(option.id.as_str(), index).is_some() {
                return Err(InputError::DuplicateChoiceOptionId { index: index + 1 });
            }
        }
        Ok(Self {
            instructions,
            kind: QuestionKind::Choice(options),
        })
    }

    /// Creates a Score question with at least two ordered levels.
    ///
    /// # Errors
    ///
    /// Returns an error when instructions, level count, or a level is invalid.
    ///
    /// # Examples
    ///
    /// ```
    /// let question = jev_core::DecisionQuestion::score(
    ///     "How urgent?",
    ///     vec!["Low".to_owned(), "High".to_owned()],
    /// )?;
    /// assert_eq!(question.score_levels()[0], "Low");
    /// # Ok::<(), jev_core::InputError>(())
    /// ```
    pub fn score(instructions: impl Into<String>, levels: Vec<String>) -> Result<Self, InputError> {
        let instructions = validate_instructions(instructions.into())?;
        validate_count("Score", levels.len(), 2, MAX_SCORE_LEVELS)?;
        for (index, level) in levels.iter().enumerate() {
            validate_text(
                level,
                MAX_CRITERION_BYTES,
                &format!("Score level {}", index + 1),
            )?;
        }
        Ok(Self {
            instructions,
            kind: QuestionKind::Score(levels),
        })
    }

    /// Returns the question primitive.
    #[must_use]
    ///
    /// # Examples
    ///
    /// ```
    /// let question = jev_core::DecisionQuestion::noul("Is this safe?", None)?;
    /// assert_eq!(question.primitive(), jev_core::QuestionPrimitive::Noul);
    /// # Ok::<(), jev_core::InputError>(())
    /// ```
    pub const fn primitive(&self) -> QuestionPrimitive {
        match &self.kind {
            QuestionKind::Noul(_) => QuestionPrimitive::Noul,
            QuestionKind::Choice(_) => QuestionPrimitive::Choice,
            QuestionKind::Score(_) => QuestionPrimitive::Score,
        }
    }

    /// Returns the validated instructions.
    #[must_use]
    ///
    /// # Examples
    ///
    /// ```
    /// let question = jev_core::DecisionQuestion::noul("Is this safe?", None)?;
    /// assert_eq!(question.instructions(), "Is this safe?");
    /// # Ok::<(), jev_core::InputError>(())
    /// ```
    pub fn instructions(&self) -> &str {
        &self.instructions
    }

    /// Returns the optional paired Noul criteria.
    #[must_use]
    ///
    /// # Examples
    ///
    /// ```
    /// let criteria = jev_core::NoulCriteria::new("Read-only", "Changes state")?;
    /// let question = jev_core::DecisionQuestion::noul("Is this safe?", Some(criteria))?;
    /// assert_eq!(question.noul_criteria().map(jev_core::NoulCriteria::true_description), Some("Read-only"));
    /// # Ok::<(), jev_core::InputError>(())
    /// ```
    pub fn noul_criteria(&self) -> Option<&NoulCriteria> {
        match &self.kind {
            QuestionKind::Noul(criteria) => criteria.as_ref(),
            QuestionKind::Choice(_) | QuestionKind::Score(_) => None,
        }
    }

    /// Returns Choice options, or an empty slice for other primitives.
    #[must_use]
    ///
    /// # Examples
    ///
    /// ```
    /// let options = vec![
    ///     jev_core::ChoiceOption::new("yes", "Yes")?,
    ///     jev_core::ChoiceOption::new("no", "No")?,
    /// ];
    /// let question = jev_core::DecisionQuestion::choice("Proceed?", options)?;
    /// assert_eq!(question.choice_options().len(), 2);
    /// # Ok::<(), jev_core::InputError>(())
    /// ```
    pub fn choice_options(&self) -> &[ChoiceOption] {
        match &self.kind {
            QuestionKind::Choice(options) => options,
            QuestionKind::Noul(_) | QuestionKind::Score(_) => &[],
        }
    }

    /// Returns ordered Score levels, or an empty slice for other primitives.
    #[must_use]
    ///
    /// # Examples
    ///
    /// ```
    /// let question = jev_core::DecisionQuestion::score(
    ///     "How urgent?",
    ///     vec!["Low".to_owned(), "High".to_owned()],
    /// )?;
    /// assert_eq!(question.score_levels(), &["Low", "High"]);
    /// # Ok::<(), jev_core::InputError>(())
    /// ```
    pub fn score_levels(&self) -> &[String] {
        match &self.kind {
            QuestionKind::Score(levels) => levels,
            QuestionKind::Noul(_) | QuestionKind::Choice(_) => &[],
        }
    }

    /// Converts the question to its Decisions API JSON shape.
    ///
    /// # Errors
    ///
    /// Returns a serialization error if JSON serialization fails.
    ///
    /// # Examples
    ///
    /// ```
    /// let question = jev_core::DecisionQuestion::noul("Is this safe?", None)?;
    /// assert_eq!(
    ///     question.to_value()?,
    ///     serde_json::json!({"type":"noul","instructions":"Is this safe?"})
    /// );
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn to_value(&self) -> Result<Value, serde_json::Error> {
        serde_json::to_value(self.as_wire())
    }

    fn as_wire(&self) -> QuestionWire<'_> {
        match &self.kind {
            QuestionKind::Noul(criteria) => QuestionWire::Noul {
                instructions: &self.instructions,
                criteria: criteria.as_ref().map(|criteria| NoulCriteriaWire {
                    true_description: criteria.true_description(),
                    false_description: criteria.false_description(),
                }),
            },
            QuestionKind::Choice(options) => QuestionWire::Choice {
                instructions: &self.instructions,
                criteria: options
                    .iter()
                    .map(|option| (option.id(), option.description()))
                    .collect(),
            },
            QuestionKind::Score(levels) => QuestionWire::Score {
                instructions: &self.instructions,
                criteria: levels,
            },
        }
    }
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum QuestionWire<'a> {
    Noul {
        instructions: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        criteria: Option<NoulCriteriaWire<'a>>,
    },
    Choice {
        instructions: &'a str,
        criteria: BTreeMap<&'a str, &'a str>,
    },
    Score {
        instructions: &'a str,
        criteria: &'a [String],
    },
}

#[derive(Serialize)]
struct NoulCriteriaWire<'a> {
    #[serde(rename = "true")]
    true_description: &'a str,
    #[serde(rename = "false")]
    false_description: &'a str,
}

/// A validated question ID accepted by the Decisions API.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub struct QuestionId(String);

impl QuestionId {
    /// Returns the ID string.
    #[must_use]
    ///
    /// # Examples
    ///
    /// ```
    /// use std::str::FromStr;
    ///
    /// let id = jev_core::QuestionId::from_str("ticket")?;
    /// assert_eq!(id.as_str(), "ticket");
    /// # Ok::<(), jev_core::InputError>(())
    /// ```
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for QuestionId {
    type Err = InputError;

    /// Parses and validates a question ID.
    ///
    /// # Errors
    ///
    /// Returns an error unless the ID uses 1 to 64 ASCII letters, digits, underscore, or hyphen.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::str::FromStr;
    ///
    /// let id = jev_core::QuestionId::from_str("ticket_check")?;
    /// assert_eq!(id.as_str(), "ticket_check");
    /// # Ok::<(), jev_core::InputError>(())
    /// ```
    fn from_str(id: &str) -> Result<Self, Self::Err> {
        validate_question_id(id)?;
        Ok(Self(id.to_owned()))
    }
}

impl fmt::Display for QuestionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A syntactically valid model identifier. Availability is checked by the provider.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub struct ModelId(String);

impl ModelId {
    /// Returns the model ID string.
    #[must_use]
    ///
    /// # Examples
    ///
    /// ```
    /// use std::str::FromStr;
    ///
    /// let model = jev_core::ModelId::from_str("typesafe/jev-1.13")?;
    /// assert_eq!(model.as_str(), "typesafe/jev-1.13");
    /// # Ok::<(), jev_core::InputError>(())
    /// ```
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for ModelId {
    type Err = InputError;

    /// Parses a provider-qualified model ID, version suffix, or Jev alias.
    ///
    /// # Errors
    ///
    /// Returns an error for empty IDs, whitespace, controls, unsupported punctuation, or excess
    /// length.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::str::FromStr;
    ///
    /// let model = jev_core::ModelId::from_str("typesafe/jev-1.13")?;
    /// assert_eq!(model.as_str(), "typesafe/jev-1.13");
    /// # Ok::<(), jev_core::InputError>(())
    /// ```
    fn from_str(model: &str) -> Result<Self, Self::Err> {
        let valid_chars = model.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b':' | b'_' | b'-' | b'~')
        });
        if model.is_empty()
            || model.len() > MAX_MODEL_ID_BYTES
            || !valid_chars
            || model.starts_with('/')
            || model.ends_with('/')
            || model.contains("//")
        {
            return Err(InputError::InvalidModelId {
                max_bytes: MAX_MODEL_ID_BYTES,
                actual_bytes: model.len(),
            });
        }
        Ok(Self(model.to_owned()))
    }
}

impl fmt::Display for ModelId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Validates a question ID without retaining it.
///
/// # Errors
///
/// Returns an error unless the ID uses 1 to 64 ASCII letters, digits, underscore, or hyphen.
///
/// # Examples
///
/// ```
/// jev_core::validate_question_id("ticket_check")?;
/// # Ok::<(), jev_core::InputError>(())
/// ```
pub fn validate_question_id(id: &str) -> Result<(), InputError> {
    if id.is_empty()
        || id.len() > MAX_ID_BYTES
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    {
        return Err(InputError::InvalidQuestionId {
            max_bytes: MAX_ID_BYTES,
            actual_bytes: id.len(),
        });
    }
    Ok(())
}

fn validate_instructions(instructions: String) -> Result<String, InputError> {
    validate_text(&instructions, MAX_INSTRUCTIONS_BYTES, "instructions")?;
    Ok(instructions)
}

fn validate_count(
    question: &'static str,
    actual: usize,
    min: usize,
    max: usize,
) -> Result<(), InputError> {
    if !(min..=max).contains(&actual) {
        return Err(InputError::InvalidCount {
            question,
            min,
            max,
            actual,
        });
    }
    Ok(())
}

fn validate_text(value: &str, max_bytes: usize, field: &str) -> Result<(), InputError> {
    if value.trim().is_empty() || value.len() > max_bytes {
        return Err(InputError::InvalidText {
            field: field.to_owned(),
            max_bytes,
            actual_bytes: value.len(),
        });
    }
    Ok(())
}

/// Errors raised when an API answer does not match its submitted question.
#[derive(Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum AnswerError {
    /// The answer is not a JSON object.
    #[error("answer must be a JSON object")]
    NotObject,
    /// A required response field is missing or has the wrong type.
    #[error("answer field {field} is missing or has the wrong type")]
    MissingField {
        /// Name of the invalid response field.
        field: &'static str,
    },
    /// The answer primitive does not match the submitted question.
    #[error("answer type does not match the submitted question")]
    WrongPrimitive,
    /// A probability or confidence value is outside the inclusive 0 to 1 range.
    #[error("answer field {field} must be a finite number from 0 to 1")]
    InvalidProbability {
        /// Name of the invalid response field.
        field: &'static str,
    },
    /// A Choice answer selects an ID absent from the submitted options.
    #[error("Choice answer selected an unknown option ID")]
    UnknownChoice,
    /// Score is outside the submitted level range.
    #[error("Score answer is outside the submitted level range")]
    ScoreOutOfRange,
    /// Probability keys do not match the submitted options or levels.
    #[error("probability keys do not match the submitted criteria")]
    InvalidProbabilityKeys,
}

#[derive(Debug, Clone, PartialEq)]
enum AnswerKind {
    Noul {
        probability: f64,
    },
    Choice {
        selected: String,
        confidence: f64,
        probabilities: BTreeMap<String, f64>,
    },
    Score {
        score: f64,
        confidence: f64,
        probabilities: BTreeMap<String, f64>,
    },
}

/// A typed answer validated against the question that produced it.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct DecisionAnswer {
    kind: AnswerKind,
}

impl DecisionAnswer {
    /// Parses and validates one answer against its submitted question.
    ///
    /// # Errors
    ///
    /// Returns an error for missing fields, wrong primitives, invalid ranges, or unknown criteria.
    ///
    /// # Examples
    ///
    /// ```
    /// let question = jev_core::DecisionQuestion::noul("Is this a bug?", None)?;
    /// let answer = jev_core::DecisionAnswer::parse(
    ///     &question,
    ///     &serde_json::json!({"type":"noul","noul":0.96}),
    /// )?;
    /// assert_eq!(answer.noul_probability(), Some(0.96));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn parse(question: &DecisionQuestion, value: &Value) -> Result<Self, AnswerError> {
        let object = value.as_object().ok_or(AnswerError::NotObject)?;
        let answer_type = object
            .get("type")
            .and_then(Value::as_str)
            .ok_or(AnswerError::MissingField { field: "type" })?;
        let kind = match &question.kind {
            QuestionKind::Noul(_) => {
                if answer_type != "noul" {
                    return Err(AnswerError::WrongPrimitive);
                }
                AnswerKind::Noul {
                    probability: probability_field(object, "noul")?,
                }
            }
            QuestionKind::Choice(options) => {
                if answer_type != "choice" {
                    return Err(AnswerError::WrongPrimitive);
                }
                let selected = object
                    .get("choice")
                    .and_then(Value::as_str)
                    .ok_or(AnswerError::MissingField { field: "choice" })?;
                if !options.iter().any(|option| option.id() == selected) {
                    return Err(AnswerError::UnknownChoice);
                }
                let confidence = probability_field(object, "confidence")?;
                let criteria = options.iter().map(|option| option.id().to_owned());
                let probabilities = probability_map(object, criteria)?;
                AnswerKind::Choice {
                    selected: selected.to_owned(),
                    confidence,
                    probabilities,
                }
            }
            QuestionKind::Score(levels) => {
                if answer_type != "score" {
                    return Err(AnswerError::WrongPrimitive);
                }
                let score = object
                    .get("score")
                    .and_then(Value::as_f64)
                    .filter(|score| score.is_finite())
                    .ok_or(AnswerError::MissingField { field: "score" })?;
                let max_score = levels
                    .len()
                    .checked_sub(1)
                    .and_then(|level| u8::try_from(level).ok())
                    .map(f64::from)
                    .ok_or(AnswerError::ScoreOutOfRange)?;
                if score < 0.0 || score > max_score {
                    return Err(AnswerError::ScoreOutOfRange);
                }
                let confidence = probability_field(object, "confidence")?;
                let criteria = (0..levels.len()).map(|index| index.to_string());
                let probabilities = probability_map(object, criteria)?;
                AnswerKind::Score {
                    score,
                    confidence,
                    probabilities,
                }
            }
        };
        Ok(Self { kind })
    }

    /// Returns the primitive of this validated answer.
    #[must_use]
    ///
    /// # Examples
    ///
    /// ```
    /// let question = jev_core::DecisionQuestion::noul("Is this a bug?", None)?;
    /// let answer = jev_core::DecisionAnswer::parse(
    ///     &question,
    ///     &serde_json::json!({"type":"noul","noul":0.8}),
    /// )?;
    /// assert_eq!(answer.primitive(), jev_core::QuestionPrimitive::Noul);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub const fn primitive(&self) -> QuestionPrimitive {
        match &self.kind {
            AnswerKind::Noul { .. } => QuestionPrimitive::Noul,
            AnswerKind::Choice { .. } => QuestionPrimitive::Choice,
            AnswerKind::Score { .. } => QuestionPrimitive::Score,
        }
    }

    /// Returns the Noul probability, when this is a Noul answer.
    #[must_use]
    ///
    /// # Examples
    ///
    /// ```
    /// let question = jev_core::DecisionQuestion::noul("Is this a bug?", None)?;
    /// let answer = jev_core::DecisionAnswer::parse(
    ///     &question,
    ///     &serde_json::json!({"type":"noul","noul":0.8}),
    /// )?;
    /// assert_eq!(answer.noul_probability(), Some(0.8));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub const fn noul_probability(&self) -> Option<f64> {
        match self.kind {
            AnswerKind::Noul { probability } => Some(probability),
            AnswerKind::Choice { .. } | AnswerKind::Score { .. } => None,
        }
    }

    /// Returns the selected Choice ID, when this is a Choice answer.
    #[must_use]
    ///
    /// # Examples
    ///
    /// ```
    /// let question = jev_core::DecisionQuestion::choice(
    ///     "Which team?",
    ///     vec![
    ///         jev_core::ChoiceOption::new("billing", "Payments")?,
    ///         jev_core::ChoiceOption::new("technical", "Product")?,
    ///     ],
    /// )?;
    /// let answer = jev_core::DecisionAnswer::parse(
    ///     &question,
    ///     &serde_json::json!({"type":"choice","choice":"billing","confidence":0.9,"probabilities":{"billing":0.8,"technical":0.2}}),
    /// )?;
    /// assert_eq!(answer.selected_choice(), Some("billing"));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn selected_choice(&self) -> Option<&str> {
        match &self.kind {
            AnswerKind::Choice { selected, .. } => Some(selected),
            AnswerKind::Noul { .. } | AnswerKind::Score { .. } => None,
        }
    }

    /// Returns confidence for Choice or Score, when present.
    #[must_use]
    ///
    /// # Examples
    ///
    /// ```
    /// let question = jev_core::DecisionQuestion::choice(
    ///     "Which team?",
    ///     vec![
    ///         jev_core::ChoiceOption::new("billing", "Payments")?,
    ///         jev_core::ChoiceOption::new("technical", "Product")?,
    ///     ],
    /// )?;
    /// let answer = jev_core::DecisionAnswer::parse(
    ///     &question,
    ///     &serde_json::json!({"type":"choice","choice":"billing","confidence":0.9,"probabilities":{"billing":0.8,"technical":0.2}}),
    /// )?;
    /// assert_eq!(answer.confidence(), Some(0.9));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub const fn confidence(&self) -> Option<f64> {
        match self.kind {
            AnswerKind::Choice { confidence, .. } | AnswerKind::Score { confidence, .. } => {
                Some(confidence)
            }
            AnswerKind::Noul { .. } => None,
        }
    }

    /// Returns the numeric Score, when this is a Score answer.
    #[must_use]
    ///
    /// # Examples
    ///
    /// ```
    /// let question = jev_core::DecisionQuestion::score(
    ///     "How urgent?",
    ///     vec!["Low".to_owned(), "High".to_owned()],
    /// )?;
    /// let answer = jev_core::DecisionAnswer::parse(
    ///     &question,
    ///     &serde_json::json!({"type":"score","score":0.7,"confidence":0.9,"probabilities":{"0":0.3,"1":0.7}}),
    /// )?;
    /// assert_eq!(answer.score(), Some(0.7));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub const fn score(&self) -> Option<f64> {
        match self.kind {
            AnswerKind::Score { score, .. } => Some(score),
            AnswerKind::Noul { .. } | AnswerKind::Choice { .. } => None,
        }
    }

    /// Returns the validated Choice or Score probabilities.
    ///
    /// Choice keys are option IDs. Score keys are decimal level indexes.
    #[must_use]
    ///
    /// # Examples
    ///
    /// ```
    /// let question = jev_core::DecisionQuestion::choice(
    ///     "Which team?",
    ///     vec![
    ///         jev_core::ChoiceOption::new("billing", "Payments")?,
    ///         jev_core::ChoiceOption::new("technical", "Product")?,
    ///     ],
    /// )?;
    /// let answer = jev_core::DecisionAnswer::parse(
    ///     &question,
    ///     &serde_json::json!({"type":"choice","choice":"billing","confidence":0.9,"probabilities":{"billing":0.8,"technical":0.2}}),
    /// )?;
    /// assert_eq!(answer.probabilities().and_then(|map| map.get("billing")), Some(&0.8));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn probabilities(&self) -> Option<&BTreeMap<String, f64>> {
        match &self.kind {
            AnswerKind::Choice { probabilities, .. } | AnswerKind::Score { probabilities, .. } => {
                Some(probabilities)
            }
            AnswerKind::Noul { .. } => None,
        }
    }
}

fn probability_field(object: &Map<String, Value>, field: &'static str) -> Result<f64, AnswerError> {
    object
        .get(field)
        .and_then(Value::as_f64)
        .filter(|number| number.is_finite() && (0.0..=1.0).contains(number))
        .ok_or(AnswerError::InvalidProbability { field })
}

fn probability_map(
    object: &Map<String, Value>,
    expected_keys: impl IntoIterator<Item = String>,
) -> Result<BTreeMap<String, f64>, AnswerError> {
    let raw = object
        .get("probabilities")
        .and_then(Value::as_object)
        .ok_or(AnswerError::MissingField {
            field: "probabilities",
        })?;
    let expected = expected_keys
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
    if raw.len() != expected.len() || raw.keys().any(|key| !expected.contains(key)) {
        return Err(AnswerError::InvalidProbabilityKeys);
    }
    let mut probabilities = BTreeMap::new();
    for key in expected {
        let probability = raw
            .get(&key)
            .and_then(Value::as_f64)
            .filter(|number| number.is_finite() && (0.0..=1.0).contains(number))
            .ok_or(AnswerError::InvalidProbability {
                field: "probabilities",
            })?;
        probabilities.insert(key, probability);
    }
    Ok(probabilities)
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use serde_json::{Value, json};

    use super::{
        AnswerError, ChoiceOption, DecisionAnswer, DecisionQuestion, InputError,
        MAX_CHOICE_OPTIONS, MAX_CRITERION_BYTES, MAX_ID_BYTES, MAX_INSTRUCTIONS_BYTES,
        MAX_MODEL_ID_BYTES, MAX_SCORE_LEVELS, MAX_STATE_BYTES, MAX_STATE_DEPTH, MAX_STATE_NODES,
        ModelId, NoulCriteria, QuestionId, QuestionPrimitive, State, StateFormat,
    };

    #[test]
    fn test_should_serialize_every_question_primitive_exactly() {
        let noul = DecisionQuestion::noul(
            "Is the request safe?",
            Some(NoulCriteria::new("Read-only", "Changes state").unwrap()),
        )
        .unwrap();
        assert_eq!(
            noul.to_value().unwrap(),
            json!({"type":"noul","instructions":"Is the request safe?","criteria":{"true":"Read-only","false":"Changes state"}})
        );
        assert_eq!(
            DecisionQuestion::noul("Is this a bug?", None)
                .unwrap()
                .to_value()
                .unwrap(),
            json!({"type":"noul","instructions":"Is this a bug?"})
        );
        assert_eq!(
            DecisionQuestion::choice(
                "Which team?",
                vec![
                    ChoiceOption::new("billing", "Payments").unwrap(),
                    ChoiceOption::new("technical", "Product issues").unwrap(),
                ],
            )
            .unwrap()
            .to_value()
            .unwrap(),
            json!({"type":"choice","instructions":"Which team?","criteria":{"billing":"Payments","technical":"Product issues"}})
        );
        assert_eq!(
            DecisionQuestion::score("How urgent?", vec!["Low".into(), "High".into()])
                .unwrap()
                .to_value()
                .unwrap(),
            json!({"type":"score","instructions":"How urgent?","criteria":["Low","High"]})
        );
    }

    #[test]
    fn test_should_reject_invalid_question_text_and_utf8_byte_overflow() {
        assert!(DecisionQuestion::noul(" \n", None).is_err());
        assert!(DecisionQuestion::noul("é".repeat(MAX_INSTRUCTIONS_BYTES / 2), None).is_ok());
        assert!(
            DecisionQuestion::noul(format!("{}é", "x".repeat(MAX_INSTRUCTIONS_BYTES - 1)), None)
                .is_err()
        );
        assert!(NoulCriteria::new(" ", "No").is_err());
        assert!(ChoiceOption::new("option", "é".repeat(MAX_CRITERION_BYTES / 2)).is_ok());
        assert!(
            ChoiceOption::new(
                "option",
                format!("{}é", "x".repeat(MAX_CRITERION_BYTES - 1))
            )
            .is_err()
        );
    }

    #[test]
    fn test_should_enforce_choice_ids_and_count_boundaries() {
        let option = |id: &str| ChoiceOption::new(id, "Description").unwrap();
        assert!(DecisionQuestion::choice("Question", vec![option("a")]).is_err());
        assert_eq!(
            DecisionQuestion::choice("Question", vec![option("a"), option("a")]),
            Err(InputError::DuplicateChoiceOptionId { index: 2 })
        );
        let options = (0..MAX_CHOICE_OPTIONS)
            .map(|index| option(&format!("id_{index}")))
            .collect();
        assert!(DecisionQuestion::choice("Question", options).is_ok());
        let options = (0..=MAX_CHOICE_OPTIONS)
            .map(|index| option(&format!("id_{index}")))
            .collect();
        assert!(DecisionQuestion::choice("Question", options).is_err());
    }

    #[test]
    fn test_should_enforce_score_count_and_preserve_order() {
        assert!(DecisionQuestion::score("Question", vec!["one".into()]).is_err());
        let levels = (0..MAX_SCORE_LEVELS)
            .map(|index| format!("level {index}"))
            .collect();
        let question = DecisionQuestion::score("Question", levels).unwrap();
        assert_eq!(
            question.score_levels().first().map(String::as_str),
            Some("level 0")
        );
        let levels = (0..=MAX_SCORE_LEVELS)
            .map(|index| format!("level {index}"))
            .collect();
        assert!(DecisionQuestion::score("Question", levels).is_err());
    }

    #[test]
    fn test_should_parse_state_formats_and_all_json_kinds() {
        assert_eq!(
            State::parse("plain text", StateFormat::Auto)
                .unwrap()
                .as_value(),
            &json!("plain text")
        );
        assert_eq!(
            State::parse(r#""hello""#, StateFormat::Auto)
                .unwrap()
                .as_value(),
            &json!("hello")
        );
        assert_eq!(
            State::parse(r#"{"a":1}"#, StateFormat::Json)
                .unwrap()
                .as_value(),
            &json!({"a":1})
        );
        assert_eq!(
            State::parse("[1,2]", StateFormat::Json).unwrap().as_value(),
            &json!([1, 2])
        );
        assert!(matches!(
            State::parse("true", StateFormat::Json),
            Err(InputError::UnsupportedJsonScalar { kind: "boolean" })
        ));
        assert!(matches!(
            State::parse("12", StateFormat::Json),
            Err(InputError::UnsupportedJsonScalar { kind: "number" })
        ));
        assert!(matches!(
            State::parse("null", StateFormat::Json),
            Err(InputError::UnsupportedJsonScalar { kind: "null" })
        ));
        assert_eq!(
            State::parse("true", StateFormat::Auto).unwrap().as_value(),
            &json!("true")
        );
        assert_eq!(
            State::parse("{truncated", StateFormat::Auto)
                .unwrap()
                .as_value(),
            &json!("{truncated")
        );
        assert_eq!(
            State::parse("{truncated", StateFormat::Json),
            Err(InputError::InvalidJson)
        );
        assert_eq!(
            State::parse(r#"""#, StateFormat::Json),
            Err(InputError::InvalidJson)
        );
        assert_eq!(
            State::parse(r#""  ""#, StateFormat::Json),
            Err(InputError::EmptyState)
        );
        assert_eq!(
            State::parse("  \t\n", StateFormat::Text),
            Err(InputError::EmptyState)
        );
        assert_eq!(
            State::parse("  text  ", StateFormat::Text)
                .unwrap()
                .as_value(),
            &json!("  text  ")
        );
    }

    #[test]
    fn test_should_reject_duplicate_keys_and_excessive_json_resources() {
        assert_eq!(
            State::parse(r#"{"a":1,"a":2}"#, StateFormat::Auto),
            Err(InputError::DuplicateJsonKey)
        );
        let deep = format!(
            "{}0{}",
            "[".repeat(MAX_STATE_DEPTH + 1),
            "]".repeat(MAX_STATE_DEPTH + 1)
        );
        assert_eq!(
            State::parse(&deep, StateFormat::Auto),
            Err(InputError::StateJsonLimit { limit: "depth" })
        );
        let nodes = format!(
            "[{}]",
            std::iter::repeat_n("0", MAX_STATE_NODES)
                .collect::<Vec<_>>()
                .join(",")
        );
        assert_eq!(
            State::parse(&nodes, StateFormat::Json),
            Err(InputError::StateJsonLimit {
                limit: "node count"
            })
        );
    }

    #[test]
    fn test_should_accept_exact_state_byte_limit_and_reject_one_more() {
        let exact = "x".repeat(MAX_STATE_BYTES);
        assert_eq!(
            State::parse(&exact, StateFormat::Text)
                .unwrap()
                .as_value()
                .as_str()
                .map(str::len),
            Some(MAX_STATE_BYTES)
        );
        let oversized = format!("{exact}x");
        assert!(matches!(
            State::parse(&oversized, StateFormat::Text),
            Err(InputError::StateTooLarge { actual_bytes, .. }) if actual_bytes == MAX_STATE_BYTES + 1
        ));
    }

    #[test]
    fn test_should_validate_question_and_model_identifier_syntax() {
        assert_eq!(
            QuestionId::from_str("custom_1").unwrap().as_str(),
            "custom_1"
        );
        assert!(QuestionId::from_str("bad\nid").is_err());
        assert!(QuestionId::from_str(&"q".repeat(MAX_ID_BYTES)).is_ok());
        assert!(matches!(
            QuestionId::from_str(&"q".repeat(MAX_ID_BYTES + 1)),
            Err(InputError::InvalidQuestionId { actual_bytes, max_bytes })
                if actual_bytes == MAX_ID_BYTES + 1 && max_bytes == MAX_ID_BYTES
        ));
        for model in [
            "typesafe/jev-1.13",
            "~typesafe/jev-latest",
            "vendor/model-v2.1:free",
        ] {
            assert_eq!(ModelId::from_str(model).unwrap().as_str(), model);
        }
        for model in [" model", "model ", "model\nname", "/model", "vendor//model"] {
            assert!(ModelId::from_str(model).is_err());
        }
        assert!(ModelId::from_str(&"m".repeat(MAX_MODEL_ID_BYTES)).is_ok());
        assert!(matches!(
            ModelId::from_str(&"m".repeat(MAX_MODEL_ID_BYTES + 1)),
            Err(InputError::InvalidModelId { actual_bytes, max_bytes })
                if actual_bytes == MAX_MODEL_ID_BYTES + 1 && max_bytes == MAX_MODEL_ID_BYTES
        ));
    }

    #[test]
    fn test_should_reject_non_state_values_from_library_callers() {
        for value in [Value::Null, json!(true), json!(1)] {
            assert!(State::from_value(value).is_err());
        }
        assert!(State::from_value(json!("   ")).is_err());
        assert_eq!(
            DecisionQuestion::noul("Safe?", None).unwrap().primitive(),
            QuestionPrimitive::Noul
        );
    }

    #[test]
    fn test_should_validate_noul_answer_probability() {
        let question = DecisionQuestion::noul("Is this a bug?", None).unwrap();
        let answer = DecisionAnswer::parse(&question, &json!({"type":"noul","noul":0.96})).unwrap();
        assert_eq!(answer.noul_probability(), Some(0.96));
        for invalid in [
            json!({}),
            json!({"type":"choice","noul":0.4}),
            json!({"type":"noul","noul":"0.4"}),
            json!({"type":"noul","noul":1.01}),
            json!({"type":"noul","noul":-0.01}),
        ] {
            assert!(DecisionAnswer::parse(&question, &invalid).is_err());
        }
    }

    #[test]
    fn test_should_validate_choice_answer_against_submitted_options() {
        let question = DecisionQuestion::choice(
            "Which team?",
            vec![
                ChoiceOption::new("billing", "Payment issues").unwrap(),
                ChoiceOption::new("technical", "Product issues").unwrap(),
            ],
        )
        .unwrap();
        let valid = json!({
            "type":"choice",
            "choice":"billing",
            "confidence":0.75,
            "probabilities":{"billing":0.84,"technical":0.16},
            "provider_metadata":{"future_field":true}
        });
        let answer = DecisionAnswer::parse(&question, &valid).unwrap();
        assert_eq!(answer.selected_choice(), Some("billing"));
        assert_eq!(answer.confidence(), Some(0.75));
        assert_eq!(answer.probabilities().unwrap().get("billing"), Some(&0.84));
        assert!(DecisionAnswer::parse(
            &question,
            &json!({"type":"choice","choice":"unknown","confidence":0.9,"probabilities":{"billing":0.5,"technical":0.5}})
        )
        .is_err());
        assert_eq!(
            DecisionAnswer::parse(
                &question,
                &json!({"type":"choice","choice":"billing","confidence":0.9,"probabilities":{"billing":0.5,"other":0.5}})
            ),
            Err(AnswerError::InvalidProbabilityKeys)
        );
        assert!(DecisionAnswer::parse(
            &question,
            &json!({"type":"choice","choice":"billing","confidence":1.2,"probabilities":{"billing":0.5,"technical":0.5}})
        )
        .is_err());
        assert!(DecisionAnswer::parse(
            &question,
            &json!({"type":"choice","choice":"billing","confidence":0.9,"probabilities":{"billing":0.5}})
        )
        .is_err());
    }

    #[test]
    fn test_should_validate_score_range_and_rounded_probabilities() {
        let question = DecisionQuestion::score(
            "How urgent?",
            vec!["Low".to_owned(), "Medium".to_owned(), "High".to_owned()],
        )
        .unwrap();
        let valid = json!({
            "type":"score",
            "score":1.99,
            "confidence":0.99,
            "probabilities":{"0":0.0,"1":0.01,"2":0.99},
            "legend":{"0":"Low","1":"Medium","2":"High"}
        });
        let answer = DecisionAnswer::parse(&question, &valid).unwrap();
        assert_eq!(answer.score(), Some(1.99));
        assert_eq!(answer.confidence(), Some(0.99));
        assert!(DecisionAnswer::parse(
            &question,
            &json!({"type":"score","score":3.0,"confidence":0.8,"probabilities":{"0":0.2,"1":0.3,"2":0.5}})
        )
        .is_err());
        assert!(DecisionAnswer::parse(
            &question,
            &json!({"type":"score","score":1,"confidence":0.8,"probabilities":{"0":0.2,"1":"0.3","2":0.5}})
        )
        .is_err());
    }
}
