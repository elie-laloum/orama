//! Agent classification and trace assembly.
//!
//! What these words mean for a CLI agent harness, which is not the same as for
//! a request-scoped web service:
//!
//! * **Session** — one CLI run. Comes from the harness, not inferred.
//! * **Trace** — one user turn: from a human message until the harness goes
//!   idle. A single turn is many API calls, because each tool result costs
//!   another round trip.
//! * **Generation** — one API call. The only span kind that costs money.
//! * **Agent** — a distinct harness persona inside the session: the main loop,
//!   a spawned subagent, a background classifier, a quota probe.
//!
//! Assembly is session-scoped and order-dependent, so it runs as a pass over a
//! session's generations rather than per call. Ids are derived by hashing stable
//! inputs, so rebuilding reproduces them exactly and external links survive.

use rusqlite::{Connection, Result};

use super::extract::fingerprint_n;

/// The fields trace assembly reads back out of `generations`.
#[derive(Debug, Clone)]
struct Row {
    call_id: i64,
    started_at: String,
    ended_at: Option<String>,
    stop_reason: Option<String>,
    model: Option<String>,
    max_tokens: Option<i64>,
    tools_declared: i64,
    system_segments: i64,
    stop_sequences: Option<String>,
    billing_variant: Option<String>,
    system_hash: Option<String>,
    tools_hash: Option<String>,
    new_user_turn: bool,
    first_turn_hash: Option<String>,
}

/// What a generation is, inside its session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AgentRole {
    /// The interactive loop the human is talking to.
    Main,
    /// A nested agent the main loop spawned, with its own tools.
    Subagent,
    /// A background helper with no tools: titles, classification, summaries.
    Sidechain,
    /// A degenerate call used to test entitlement, not to do work.
    Probe,
    Unknown,
}

impl AgentRole {
    fn as_str(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::Subagent => "subagent",
            Self::Sidechain => "sidechain",
            Self::Probe => "probe",
            Self::Unknown => "unknown",
        }
    }
}

/// The classification of one generation, plus why it was reached.
#[derive(Debug, Clone)]
struct Agent {
    id: String,
    name: String,
    role: AgentRole,
}

/// What the session's main loop looks like, used as the comparison baseline.
#[derive(Debug, Clone, Copy, Default)]
struct MainShape<'a> {
    max_tools: i64,
    model: Option<&'a str>,
    /// The billing variant of the main loop. This is provider-supplied identity,
    /// so it outranks any shape heuristic.
    variant: Option<&'a str>,
}

/// Classify a generation against the session's dominant shape.
///
/// Ordered most- to least-specific; the first match wins. Every condition is a
/// field the provider actually sends, so this is a rule table rather than a
/// similarity heuristic.
fn classify(row: &Row, main: MainShape<'_>) -> Agent {
    let session_max_tools = main.max_tools;
    let main_model = main.model;
    let id = fingerprint_n(
        &[
            row.billing_variant.as_deref().unwrap_or(""),
            row.system_hash.as_deref().unwrap_or(""),
            row.tools_hash.as_deref().unwrap_or(""),
        ],
        16,
    );
    let toolless = row.tools_declared == 0;

    // A one-token, systemless, toolless call is not work — it is an entitlement
    // check. Treating its 429 as a rate-limit incident would be a false alarm.
    if row.max_tokens == Some(1) && toolless && row.system_segments == 0 {
        return Agent {
            id,
            name: "quota-probe".to_owned(),
            role: AgentRole::Probe,
        };
    }

    // A toolless call constrained by a stop sequence is a classifier: it is
    // asked for one tagged verdict, not for a conversation.
    if toolless && row.stop_sequences.is_some() {
        let tag = row
            .stop_sequences
            .as_deref()
            .and_then(stop_sequence_tag)
            .unwrap_or_else(|| "classifier".to_owned());
        return Agent {
            id,
            name: format!("classifier:{tag}"),
            role: AgentRole::Sidechain,
        };
    }

    // Any other toolless call is a background helper: a title, a summary, a
    // one-shot judgement. It cannot act, so it is not doing the user's work.
    if toolless {
        return Agent {
            id,
            name: "sidechain".to_owned(),
            role: AgentRole::Sidechain,
        };
    }

    // The billing variant is the harness naming its own agent, so it settles the
    // question before any shape heuristic runs. Without this, a main loop whose
    // tool count wobbles by one — an MCP server connecting mid-session — splits
    // into a phantom "subagent".
    if row.billing_variant.is_some() && row.billing_variant.as_deref() == main.variant {
        return Agent {
            id,
            name: "main".to_owned(),
            role: AgentRole::Main,
        };
    }

    // Tool-bearing, a different variant, and a trimmed tool set: a spawned
    // subagent, which inherits a subset of the parent's tools.
    if session_max_tools > 0 && row.tools_declared < session_max_tools {
        let suffix = row
            .model
            .as_deref()
            .and_then(|model| model.split('-').nth(1))
            .unwrap_or("agent");
        return Agent {
            id,
            name: format!("subagent:{suffix}"),
            role: AgentRole::Subagent,
        };
    }

    if row.tools_declared == session_max_tools && session_max_tools > 0 {
        // A different model at full tool count is still a distinct agent.
        let is_main_model = main_model.is_none() || row.model.as_deref() == main_model;
        return Agent {
            id,
            name: if is_main_model {
                "main"
            } else {
                "subagent:peer"
            }
            .to_owned(),
            role: if is_main_model {
                AgentRole::Main
            } else {
                AgentRole::Subagent
            },
        };
    }

    Agent {
        id: id.clone(),
        name: format!("agent-{}", &id[..6.min(id.len())]),
        role: AgentRole::Unknown,
    }
}

/// The tag inside a stop sequence such as `["</severity>"]`, which names what
/// the classifier was asked to decide.
fn stop_sequence_tag(raw: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    let first = value.as_array()?.first()?.as_str()?;
    let tag = first
        .trim_start_matches('<')
        .trim_start_matches('/')
        .trim_end_matches('>');
    (!tag.is_empty()).then(|| tag.to_owned())
}

/// Assign agent, trace and span identity across one session.
pub fn assemble_session(conn: &Connection, session_id: &str) -> Result<()> {
    let rows = load(conn, session_id)?;
    if rows.is_empty() {
        return Ok(());
    }

    let max_tools = rows.iter().map(|row| row.tools_declared).max().unwrap_or(0);
    let baseline = rows.iter().find(|row| row.tools_declared == max_tools);
    let main = MainShape {
        max_tools,
        model: baseline.and_then(|row| row.model.as_deref()),
        variant: baseline.and_then(|row| row.billing_variant.as_deref()),
    };

    let agents: Vec<Agent> = rows.iter().map(|row| classify(row, main)).collect();

    // Pass 1: main-loop calls define the traces. A new trace opens when the
    // human speaks again, when the harness previously went idle, or when the
    // conversation root changes (a branch or a restart).
    let mut trace_of: Vec<Option<String>> = vec![None; rows.len()];
    let mut current: Option<String> = None;
    let mut previous: Option<&Row> = None;
    let mut roots: Vec<(String, String, Option<String>)> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        if agents[index].role != AgentRole::Main {
            continue;
        }
        let boundary = current.is_none()
            || row.new_user_turn
            || previous.and_then(|p| p.stop_reason.as_deref()) == Some("end_turn")
            || previous.is_some_and(|p| p.first_turn_hash != row.first_turn_hash);
        if boundary {
            let trace = fingerprint_n(&["trace", session_id, &row.call_id.to_string()], 32);
            roots.push((trace.clone(), row.started_at.clone(), None));
            current = Some(trace);
        }
        if let Some(last) = roots.last_mut() {
            last.2 = row.ended_at.clone().or(Some(row.started_at.clone()));
        }
        trace_of[index] = current.clone();
        previous = Some(row);
    }

    // Pass 2: everything else attaches to the trace whose window contains it.
    // A helper that ran while the main loop was working belongs to that turn.
    for (index, row) in rows.iter().enumerate() {
        if trace_of[index].is_some() {
            continue;
        }
        let containing = roots
            .iter()
            .find(|(_, start, end)| {
                row.started_at.as_str() >= start.as_str()
                    && end
                        .as_deref()
                        .is_some_and(|end| row.started_at.as_str() <= end)
            })
            .map(|(trace, _, _)| trace.clone());
        // No overlap means we genuinely do not know which turn it served, so it
        // gets its own trace rather than being attached to a plausible guess.
        trace_of[index] = Some(containing.unwrap_or_else(|| {
            fingerprint_n(&["trace", session_id, &row.call_id.to_string()], 32)
        }));
    }

    // Pass 3: parents. Generations of the main loop sit directly under the
    // trace; helper and subagent calls nest under the main call they ran during,
    // which is what produces a readable tree rather than a 20-deep chain.
    let span_of: Vec<String> = rows
        .iter()
        .map(|row| fingerprint_n(&["gen", session_id, &row.call_id.to_string()], 16))
        .collect();

    let mut updates = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        let trace = trace_of[index].clone();
        let (parent, depth) = if agents[index].role == AgentRole::Main {
            (None, 0_i64)
        } else {
            let host = rows
                .iter()
                .enumerate()
                .filter(|(other, _)| agents[*other].role == AgentRole::Main)
                .filter(|(other, candidate)| {
                    trace_of[*other] == trace
                        && candidate.started_at <= row.started_at
                        && candidate
                            .ended_at
                            .as_deref()
                            .is_some_and(|end| row.started_at.as_str() <= end)
                })
                .map(|(other, _)| span_of[other].clone())
                .next_back();
            match host {
                Some(parent) => (Some(parent), 1),
                None => (None, 0),
            }
        };
        updates.push((
            row.call_id,
            trace,
            span_of[index].clone(),
            parent,
            depth,
            agents[index].clone(),
        ));
    }

    let mut stmt = conn.prepare(
        r#"
        UPDATE generations
           SET trace_id = ?2, span_id = ?3, parent_span_id = ?4, depth = ?5,
               agent_id = ?6, agent_name = ?7, agent_role = ?8
         WHERE call_id = ?1
        "#,
    )?;
    for (call_id, trace, span, parent, depth, agent) in &updates {
        stmt.execute(rusqlite::params![
            call_id,
            trace,
            span,
            parent,
            depth,
            agent.id,
            agent.name,
            agent.role.as_str(),
        ])?;
    }
    drop(stmt);

    // Tool spans inherit their generation's trace so a trace query returns the
    // whole tree in one pass.
    conn.execute(
        r#"
        UPDATE tool_calls SET trace_id = (
            SELECT g.trace_id FROM generations g WHERE g.call_id = tool_calls.call_id
        ) WHERE session_id = ?1
        "#,
        [session_id],
    )?;
    Ok(())
}

/// Assemble every session that has derived generations.
pub fn assemble_all(conn: &Connection) -> Result<usize> {
    let sessions: Vec<String> = {
        let mut stmt = conn
            .prepare("SELECT DISTINCT session_id FROM generations WHERE session_id IS NOT NULL")?;
        let rows = stmt.query_map([], |row| row.get(0))?;
        rows.collect::<Result<_>>()?
    };
    for session in &sessions {
        assemble_session(conn, session)?;
    }
    Ok(sessions.len())
}

fn load(conn: &Connection, session_id: &str) -> Result<Vec<Row>> {
    let mut stmt = conn.prepare(
        r#"
        SELECT call_id, started_at, ended_at, stop_reason, model, max_tokens,
               COALESCE(tools_declared_count, 0), COALESCE(system_segments_count, 0),
               stop_sequences, billing_variant, system_hash, tools_hash,
               new_user_turn, first_turn_hash
          FROM generations
         WHERE session_id = ?1
         ORDER BY started_at, call_id
        "#,
    )?;
    let rows = stmt.query_map([session_id], |row| {
        Ok(Row {
            call_id: row.get(0)?,
            started_at: row.get(1)?,
            ended_at: row.get(2)?,
            stop_reason: row.get(3)?,
            model: row.get(4)?,
            max_tokens: row.get(5)?,
            tools_declared: row.get(6)?,
            system_segments: row.get(7)?,
            stop_sequences: row.get(8)?,
            billing_variant: row.get(9)?,
            system_hash: row.get(10)?,
            tools_hash: row.get(11)?,
            new_user_turn: row.get::<_, i64>(12)? != 0,
            first_turn_hash: row.get(13)?,
        })
    })?;
    rows.collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A session whose main loop is opus at 121 tools, variant `.85f`.
    fn main_shape() -> MainShape<'static> {
        MainShape {
            max_tools: 121,
            model: Some("claude-opus-5"),
            variant: Some("2.1.220.85f"),
        }
    }

    fn row(tools: i64, max_tokens: Option<i64>, system_segments: i64) -> Row {
        Row {
            call_id: 1,
            started_at: "2026-07-25T10:00:00Z".into(),
            ended_at: None,
            stop_reason: None,
            model: Some("claude-opus-5".into()),
            max_tokens,
            tools_declared: tools,
            system_segments,
            stop_sequences: None,
            billing_variant: None,
            system_hash: None,
            tools_hash: None,
            new_user_turn: false,
            first_turn_hash: None,
        }
    }

    #[test]
    fn a_one_token_toolless_call_is_a_probe_not_work() {
        // Every 429 in the real capture is one of these. Classifying it as a
        // rate-limit incident would be a false alarm on 100% of them.
        let agent = classify(&row(0, Some(1), 0), main_shape());
        assert_eq!(agent.role, AgentRole::Probe);
        assert_eq!(agent.name, "quota-probe");
    }

    #[test]
    fn a_stop_sequence_bounded_toolless_call_is_a_named_classifier() {
        let mut input = row(0, Some(256), 1);
        input.stop_sequences = Some(r#"["</severity>"]"#.into());
        let agent = classify(&input, main_shape());
        assert_eq!(agent.role, AgentRole::Sidechain);
        assert_eq!(agent.name, "classifier:severity");
    }

    #[test]
    fn full_tool_count_on_the_session_model_is_the_main_loop() {
        let agent = classify(&row(121, Some(32000), 3), main_shape());
        assert_eq!(agent.role, AgentRole::Main);
        assert_eq!(agent.name, "main");
    }

    #[test]
    fn a_trimmed_tool_set_is_a_subagent() {
        let mut input = row(119, Some(32000), 3);
        input.model = Some("claude-haiku-4-5-20251001".into());
        let agent = classify(&input, main_shape());
        assert_eq!(agent.role, AgentRole::Subagent);
        assert_eq!(agent.name, "subagent:haiku");
    }

    #[test]
    fn a_tool_count_wobble_within_one_variant_stays_the_main_loop() {
        // An MCP server connecting mid-session changes the declared tool count.
        // The harness still calls it the same agent, and so must we.
        let mut input = row(120, Some(32000), 3);
        input.billing_variant = Some("2.1.220.85f".into());
        let agent = classify(&input, main_shape());
        assert_eq!(agent.role, AgentRole::Main);
    }

    #[test]
    fn agent_id_is_stable_and_separates_variants() {
        let mut a = row(121, Some(32000), 3);
        a.billing_variant = Some("2.1.220.85f".into());
        let mut b = a.clone();
        b.billing_variant = Some("2.1.220.ea8".into());
        let first = classify(&a, main_shape());
        assert_eq!(first.id, classify(&a, main_shape()).id);
        assert_ne!(first.id, classify(&b, main_shape()).id);
    }
}
