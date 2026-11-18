//! Structured questions: the agent asks the user (`ask_user`) and the task
//! waits for the answer, like an approval wait.
//!
//! The backend owns everything about a request: its id, revision, the task
//! and tool call it belongs to, and its state. Answers are validated here
//! against the current revision; an answer is user content, never an
//! authorization (the central gate still decides every later action).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use ts_rs::TS;

use crate::model::TaskId;

/// Questions one request may hold.
pub const MAX_QUESTIONS: usize = 3;
pub const MAX_PROMPT_CHARS: usize = 300;
pub const MAX_EXPLANATION_CHARS: usize = 600;
pub const MIN_OPTIONS: usize = 2;
pub const MAX_OPTIONS: usize = 5;
pub const MAX_OPTION_ID_CHARS: usize = 32;
pub const MAX_LABEL_CHARS: usize = 80;
pub const MAX_CONSEQUENCE_CHARS: usize = 200;
/// A custom answer.
pub const MAX_CUSTOM_CHARS: usize = 2000;
/// A "Chat about it" message.
pub const MAX_DISCUSSION_CHARS: usize = 4000;

pub mod codes {
    /// The request was answered (differently) or superseded.
    pub const RESOLVED: &str = "agents.questionResolved";
    /// The answer refers to an older revision of the options.
    pub const STALE: &str = "agents.questionStale";
    /// A discussion turn is already running.
    pub const BUSY: &str = "agents.questionBusy";
    /// The answer does not fit the questions.
    pub const INVALID_ANSWER: &str = "agents.invalidAnswer";
    /// No such question request for this task.
    pub const NOT_FOUND: &str = "agents.noQuestion";
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct QuestionOption {
    /// Stable within the request (`[a-z0-9_-]{1,32}`).
    pub id: String,
    pub label: String,
    pub consequence: Option<String>,
    /// At most one option per question is recommended.
    #[serde(default)]
    pub recommended: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct Question {
    /// `q1` … `q3`, assigned by the application in order.
    pub id: String,
    pub prompt: String,
    pub explanation: Option<String>,
    pub options: Vec<QuestionOption>,
    pub allow_custom: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum QuestionValue {
    Option { option_id: String },
    Custom { text: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct QuestionAnswer {
    pub question_id: String,
    pub value: QuestionValue,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum QuestionRequestState {
    Pending,
    /// A "Chat about it" turn is running (or about to).
    Discussing,
    Answered {
        answers: Vec<QuestionAnswer>,
    },
    /// The request can no longer be answered ("The task was canceled",
    /// "Exposition restarted", "The task stopped: …").
    Superseded {
        reason: String,
    },
}

impl QuestionRequestState {
    /// Still waiting for the user (pending or being discussed).
    pub fn is_open(&self) -> bool {
        matches!(self, QuestionRequestState::Pending | QuestionRequestState::Discussing)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct QuestionRequest {
    pub id: String,
    pub task_id: TaskId,
    pub tool_use_id: String,
    /// Starts at 1; bumped when a discussion turn revises the questions.
    pub revision: u32,
    pub questions: Vec<Question>,
    pub state: QuestionRequestState,
    pub created_at_ms: i64,
}

impl QuestionRequest {
    pub fn new(task_id: TaskId, tool_use_id: &str, questions: Vec<Question>, created_at_ms: i64) -> Self {
        Self {
            id: format!("question-{}", uuid::Uuid::new_v4()),
            task_id,
            tool_use_id: tool_use_id.to_owned(),
            revision: 1,
            questions,
            state: QuestionRequestState::Pending,
            created_at_ms,
        }
    }
}

// ---- tool input

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct OptionInput {
    id: String,
    label: String,
    #[serde(default)]
    consequence: Option<String>,
    #[serde(default)]
    recommended: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct QuestionInput {
    prompt: String,
    #[serde(default)]
    explanation: Option<String>,
    options: Vec<OptionInput>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct AskUserInput {
    questions: Vec<QuestionInput>,
    #[serde(default, rename = "allowCustom")]
    allow_custom: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviseQuestionInput {
    request: String,
    questions: Vec<QuestionInput>,
}

fn chars(text: &str) -> usize {
    text.chars().count()
}

fn optional_text(raw: Option<String>, what: &str, limit: usize) -> Result<Option<String>, String> {
    let Some(text) = raw.map(|t| t.trim().to_owned()).filter(|t| !t.is_empty()) else { return Ok(None) };
    if chars(&text) > limit {
        return Err(format!("{what} must be at most {limit} characters"));
    }
    Ok(Some(text))
}

fn valid_option_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_OPTION_ID_CHARS
        && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

fn build_questions(inputs: Vec<QuestionInput>, allow_custom: bool) -> Result<Vec<Question>, String> {
    if inputs.is_empty() || inputs.len() > MAX_QUESTIONS {
        return Err(format!("questions must contain 1 to {MAX_QUESTIONS} questions"));
    }
    let mut out = Vec::with_capacity(inputs.len());
    for (index, input) in inputs.into_iter().enumerate() {
        let n = index + 1;
        let prompt = input.prompt.trim().to_owned();
        if prompt.is_empty() || chars(&prompt) > MAX_PROMPT_CHARS {
            return Err(format!("question {n}: prompt must be 1 to {MAX_PROMPT_CHARS} characters"));
        }
        let explanation =
            optional_text(input.explanation, &format!("question {n}: explanation"), MAX_EXPLANATION_CHARS)?;
        if input.options.len() < MIN_OPTIONS || input.options.len() > MAX_OPTIONS {
            return Err(format!("question {n}: options must contain {MIN_OPTIONS} to {MAX_OPTIONS} options"));
        }
        let mut options: Vec<QuestionOption> = Vec::with_capacity(input.options.len());
        for option in input.options {
            let id = option.id.trim().to_owned();
            if !valid_option_id(&id) {
                return Err(format!(
                    "question {n}: option id {id:?} must be 1 to {MAX_OPTION_ID_CHARS} of a-z, 0-9, '_' or '-'"
                ));
            }
            if options.iter().any(|o| o.id == id) {
                return Err(format!("question {n}: option id {id:?} is used twice"));
            }
            let label = option.label.trim().to_owned();
            if label.is_empty() || chars(&label) > MAX_LABEL_CHARS {
                return Err(format!("question {n}: option {id:?} label must be 1 to {MAX_LABEL_CHARS} characters"));
            }
            let consequence = optional_text(
                option.consequence,
                &format!("question {n}: option {id:?} consequence"),
                MAX_CONSEQUENCE_CHARS,
            )?;
            options.push(QuestionOption { id, label, consequence, recommended: option.recommended.unwrap_or(false) });
        }
        if options.iter().filter(|o| o.recommended).count() > 1 {
            return Err(format!("question {n}: at most one option may be recommended"));
        }
        out.push(Question { id: format!("q{n}"), prompt, explanation, options, allow_custom });
    }
    Ok(out)
}

/// Validate an `ask_user` input. An invalid input is an ordinary tool error.
pub fn parse_ask_user(input: &Value) -> Result<Vec<Question>, String> {
    let parsed: AskUserInput =
        serde_json::from_value(input.clone()).map_err(|e| format!("input does not match the tool schema: {e}"))?;
    build_questions(parsed.questions, parsed.allow_custom.unwrap_or(true))
}

/// Validate a `revise_question` input: the request it names and its new
/// questions (custom answers stay allowed or not, as asked originally).
pub fn parse_revise(input: &Value, allow_custom: bool) -> Result<(String, Vec<Question>), String> {
    let parsed: ReviseQuestionInput =
        serde_json::from_value(input.clone()).map_err(|e| format!("input does not match the tool schema: {e}"))?;
    Ok((parsed.request, build_questions(parsed.questions, allow_custom)?))
}

/// Check that `answers` give exactly one valid answer per question.
pub fn validate_answers(questions: &[Question], answers: &[QuestionAnswer]) -> Result<(), String> {
    if answers.len() != questions.len() {
        return Err(format!("Answer each of the {} question(s) exactly once.", questions.len()));
    }
    for question in questions {
        let mut given = answers.iter().filter(|a| a.question_id == question.id);
        let (Some(answer), None) = (given.next(), given.next()) else {
            return Err(format!("Answer question {} exactly once.", question.id));
        };
        match &answer.value {
            QuestionValue::Option { option_id } => {
                if !question.options.iter().any(|o| &o.id == option_id) {
                    return Err(format!("{option_id:?} is not an option of question {}.", question.id));
                }
            }
            QuestionValue::Custom { text } => {
                if !question.allow_custom {
                    return Err(format!("Question {} does not accept a custom answer.", question.id));
                }
                let n = chars(text.trim());
                if n == 0 || n > MAX_CUSTOM_CHARS {
                    return Err(format!("A custom answer must be 1 to {MAX_CUSTOM_CHARS} characters."));
                }
            }
        }
    }
    Ok(())
}

/// The answers in question order (custom text trimmed), for storing and comparing.
pub fn normalize(answers: &[QuestionAnswer]) -> Vec<QuestionAnswer> {
    let mut out: Vec<QuestionAnswer> = answers
        .iter()
        .map(|a| QuestionAnswer {
            question_id: a.question_id.clone(),
            value: match &a.value {
                QuestionValue::Custom { text } => QuestionValue::Custom { text: text.trim().to_owned() },
                other => other.clone(),
            },
        })
        .collect();
    out.sort_by(|a, b| a.question_id.cmp(&b.question_id));
    out
}

/// The tool result the model receives once the user answered: each question
/// id mapped to `{"optionId": …}` or `{"custom": …}`.
pub fn answer_result(answers: &[QuestionAnswer]) -> Value {
    let mut map = Map::new();
    for answer in answers {
        let value = match &answer.value {
            QuestionValue::Option { option_id } => json!({"optionId": option_id}),
            QuestionValue::Custom { text } => json!({"custom": text}),
        };
        map.insert(answer.question_id.clone(), value);
    }
    Value::Object(map)
}

// ---- tool schemas (non-strict; Rust validates the input exactly)

fn question_items() -> Value {
    json!({
        "type": "object",
        "properties": {
            "prompt": {"type": "string", "description": format!("The question, one clear sentence (at most {MAX_PROMPT_CHARS} characters).")},
            "explanation": {"type": "string", "description": format!("Brief context the user needs to decide (at most {MAX_EXPLANATION_CHARS} characters); \"\" for none.")},
            "options": {
                "type": "array",
                "minItems": MIN_OPTIONS,
                "maxItems": MAX_OPTIONS,
                "description": "The choices, most useful first.",
                "items": {
                    "type": "object",
                    "properties": {
                        "id": {"type": "string", "description": "Stable option id: 1-32 of a-z, 0-9, '_' or '-'. Keep an option's id when you revise it."},
                        "label": {"type": "string", "description": format!("Concise label (at most {MAX_LABEL_CHARS} characters).")},
                        "consequence": {"type": "string", "description": format!("What choosing it means or costs (at most {MAX_CONSEQUENCE_CHARS} characters); \"\" for none.")},
                        "recommended": {"type": "boolean", "description": "true for the one option you recommend, if any (at most one per question)."}
                    },
                    "required": ["id", "label", "consequence", "recommended"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["prompt", "explanation", "options"],
        "additionalProperties": false
    })
}

pub fn ask_user_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "questions": {"type": "array", "minItems": 1, "maxItems": MAX_QUESTIONS, "items": question_items(), "description": format!("1 to {MAX_QUESTIONS} related questions, asked together.")},
            "allowCustom": {"type": "boolean", "description": "Whether the user may type an answer of their own instead of an option (usually true)."}
        },
        "required": ["questions", "allowCustom"],
        "additionalProperties": false
    })
}

pub fn revise_question_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "request": {"type": "string", "description": "The id of the question request being discussed."},
            "questions": {"type": "array", "minItems": 1, "maxItems": MAX_QUESTIONS, "items": question_items(), "description": "The complete revised questions, in order."}
        },
        "required": ["request", "questions"],
        "additionalProperties": false
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ask(questions: Value) -> Result<Vec<Question>, String> {
        parse_ask_user(&json!({"questions": questions, "allowCustom": true}))
    }

    fn option(id: &str) -> Value {
        json!({"id": id, "label": format!("Option {id}"), "consequence": "", "recommended": false})
    }

    #[test]
    fn ask_user_input_is_validated() {
        let ok =
            ask(json!([{"prompt": "Which style?", "explanation": "", "options": [option("a"), option("b")]}])).unwrap();
        assert_eq!(ok[0].id, "q1");
        assert_eq!(ok[0].explanation, None);
        assert_eq!(ok[0].options[0].consequence, None);
        assert!(ok[0].allow_custom);
        // Optional fields may be omitted.
        let lean = parse_ask_user(&json!({"questions": [{"prompt": "P", "options": [
            {"id": "x", "label": "X"}, {"id": "y", "label": "Y", "recommended": true}]}]}))
        .unwrap();
        assert!(lean[0].allow_custom && lean[0].options[1].recommended);
        let q = |options: Value| json!([{"prompt": "P", "explanation": "", "options": options}]);
        assert!(ask(json!([])).is_err(), "no questions");
        let one = q(json!([option("a"), option("b")]))[0].clone();
        assert!(ask(Value::Array(vec![one; 4])).is_err(), "four questions");
        assert!(ask(q(json!([option("a")]))).is_err(), "one option");
        assert!(ask(q(json!([option("a"), option("b"), option("c"), option("d"), option("e"), option("f")]))).is_err());
        assert!(ask(q(json!([option("A"), option("b")]))).is_err(), "upper-case id");
        assert!(ask(q(json!([option("a"), option("a")]))).is_err(), "duplicate id");
        let long = "x".repeat(33);
        assert!(ask(q(json!([option(&long), option("b")]))).is_err());
        let two_recommended = json!([
            {"id": "a", "label": "A", "consequence": "", "recommended": true},
            {"id": "b", "label": "B", "consequence": "", "recommended": true}
        ]);
        assert!(ask(q(two_recommended)).is_err());
        assert!(
            ask(json!([{"prompt": "x".repeat(301), "explanation": "", "options": [option("a"), option("b")]}]))
                .is_err()
        );
        assert!(
            ask(json!([{"prompt": "P", "explanation": "x".repeat(601), "options": [option("a"), option("b")]}]))
                .is_err()
        );
        assert!(parse_ask_user(&json!({"questions": [], "allowCustom": true, "extra": 1})).is_err());
        let no_custom = parse_ask_user(
            &json!({"questions": [{"prompt": "P", "options": [option("a"), option("b")]}], "allowCustom": false}),
        )
        .unwrap();
        assert!(!no_custom[0].allow_custom);
    }

    #[test]
    fn answers_are_validated_and_normalized() {
        let questions = ask(json!([
            {"prompt": "One", "explanation": "", "options": [option("a"), option("b")]},
            {"prompt": "Two", "explanation": "", "options": [option("c"), option("d")]}
        ]))
        .unwrap();
        let pick = |q: &str, o: &str| QuestionAnswer {
            question_id: q.into(),
            value: QuestionValue::Option { option_id: o.into() },
        };
        let custom = |q: &str, t: &str| QuestionAnswer {
            question_id: q.into(),
            value: QuestionValue::Custom { text: t.into() },
        };
        assert!(validate_answers(&questions, &[pick("q1", "a"), pick("q2", "d")]).is_ok());
        assert!(validate_answers(&questions, &[pick("q1", "a")]).is_err(), "one missing");
        assert!(validate_answers(&questions, &[pick("q1", "a"), pick("q1", "b")]).is_err(), "twice");
        assert!(validate_answers(&questions, &[pick("q1", "c"), pick("q2", "d")]).is_err(), "other question's option");
        assert!(validate_answers(&questions, &[pick("q1", "a"), custom("q2", "  ")]).is_err(), "empty custom");
        assert!(validate_answers(&questions, &[pick("q1", "a"), custom("q2", &"x".repeat(2001))]).is_err());
        assert!(validate_answers(&questions, &[pick("q1", "a"), custom("q2", "mine")]).is_ok());
        let mut strict = questions.clone();
        strict[1].allow_custom = false;
        assert!(validate_answers(&strict, &[pick("q1", "a"), custom("q2", "mine")]).is_err());
        assert_eq!(normalize(&[custom("q2", " mine "), pick("q1", "a")]), vec![pick("q1", "a"), custom("q2", "mine")]);
        assert_eq!(
            answer_result(&[pick("q1", "a"), custom("q2", "mine")]),
            json!({"q1": {"optionId": "a"}, "q2": {"custom": "mine"}})
        );
    }

    #[test]
    fn state_serializes_as_the_contract_says() {
        let state = QuestionRequestState::Answered {
            answers: vec![QuestionAnswer {
                question_id: "q1".into(),
                value: QuestionValue::Option { option_id: "a".into() },
            }],
        };
        assert_eq!(
            serde_json::to_value(&state).unwrap(),
            json!({"kind": "answered", "answers": [{"questionId": "q1", "value": {"kind": "option", "optionId": "a"}}]})
        );
        assert_eq!(serde_json::to_value(QuestionRequestState::Pending).unwrap(), json!({"kind": "pending"}));
    }
}
