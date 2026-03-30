//! Typed bodies for Claude prompts. Serialize with `serde_json` — user-derived text lives in
//! fields and is escaped by the serializer (no manual `format!` interpolation).
#![allow(dead_code)]
// Types are used once callers (Slack, scheduler) wire Claude; kept as the public prompt API.

use chrono::{DateTime, Utc};
use chrono_tz::Europe::London;
use serde::{Deserialize, Serialize};

/// Bumped when any payload shape changes incompatibly.
pub const PROMPT_API_VERSION: u32 = 1;

pub const TASK_INTENT_CLASSIFICATION: &str = "intent_classification";
pub const TASK_MORNING_BRIEFING: &str = "morning_briefing";
pub const TASK_FREEFORM_QUERY: &str = "freeform_query";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct SystemContextV1 {
    pub api_version: u32,
    pub assistant_name: &'static str,
    pub user: UserProfileV1,
    pub clock: ClockV1,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct UserProfileV1 {
    pub name: &'static str,
    pub occupation: &'static str,
    pub location: &'static str,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct ClockV1 {
    pub date: String,
    pub time: String,
    pub timezone: &'static str,
}

impl SystemContextV1 {
    pub fn for_wolf(now: DateTime<Utc>) -> Self {
        let london = now.with_timezone(&London);
        Self {
            api_version: PROMPT_API_VERSION,
            assistant_name: "Mervyn",
            user: UserProfileV1 {
                name: "Wolf",
                occupation: "karaoke jockey and software developer",
                location: "Portsmouth, UK",
            },
            clock: ClockV1 {
                date: london.format("%Y-%m-%d").to_string(),
                time: london.format("%H:%M").to_string(),
                timezone: "Europe/London",
            },
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct IntentClassificationV1 {
    pub api_version: u32,
    pub task: &'static str,
    pub message: String,
}

impl IntentClassificationV1 {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            api_version: PROMPT_API_VERSION,
            task: TASK_INTENT_CLASSIFICATION,
            message: message.into(),
        }
    }
}

/// Expected model reply for intent routing (deserialize). `api_version` mirrors the request when present.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct IntentClassificationReplyV1 {
    #[serde(default)]
    pub api_version: u32,
    pub intent: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct MorningBriefingV1 {
    pub api_version: u32,
    pub task: &'static str,
    pub events: String,
    pub reminders: String,
    pub worklog: String,
}

impl MorningBriefingV1 {
    pub fn new(events: impl Into<String>, reminders: impl Into<String>, worklog: impl Into<String>) -> Self {
        Self {
            api_version: PROMPT_API_VERSION,
            task: TASK_MORNING_BRIEFING,
            events: events.into(),
            reminders: reminders.into(),
            worklog: worklog.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct FreeformQueryV1 {
    pub api_version: u32,
    pub task: &'static str,
    pub context: String,
    pub question: String,
}

impl FreeformQueryV1 {
    pub fn new(context: impl Into<String>, question: impl Into<String>) -> Self {
        Self {
            api_version: PROMPT_API_VERSION,
            task: TASK_FREEFORM_QUERY,
            context: context.into(),
            question: question.into(),
        }
    }
}
