//! Compare accepted models of reliable adjacent native turns in the reader's
//! snapshot, before display filters and pagination. Never infer native identity.
pub(super) fn cte(runs_parameter: u8, watermark: &str) -> String {
    format!(
        r#"WITH turn_input AS (
      SELECT r.request_id,r.session_id,r.started_at_ms,l.run_id,l.conflicted,
        json_extract(l.body_json,'$.native_session_id') AS native_session,
        json_extract(l.body_json,'$.native_turn_id') AS native_turn,
        json_extract(l.body_json,'$.harness_id') AS harness,
        v.terminal,v.partial,a.model_id,
        ROW_NUMBER() OVER(PARTITION BY r.session_id,l.run_id,json_extract(l.body_json,'$.native_session_id'),json_extract(l.body_json,'$.native_turn_id') ORDER BY r.started_at_ms DESC,r.request_id DESC) AS latest
      FROM logical_requests r
      LEFT JOIN observation_run_links l ON l.workspace_id=r.workspace_id AND l.request_id=r.request_id
      LEFT JOIN valuation_requests_v2 v ON v.workspace_id=r.workspace_id AND v.request_id=r.request_id
      LEFT JOIN observation_attempt_models_v2 a ON a.workspace_id=r.workspace_id AND a.request_id=r.request_id AND a.ordinal=v.accepted_ordinal
      WHERE r.workspace_id=?1 AND r.started_at_ms>=?2 AND r.started_at_ms<?3 AND r.rowid<={watermark}
        AND (?{runs_parameter} IS NULL OR (l.conflicted=0 AND l.run_id IN(SELECT value FROM json_each(?{runs_parameter}))))
    ), turn_groups AS (
      SELECT session_id,run_id,native_session,native_turn,harness,
        MIN(started_at_ms) AS first_at,MAX(started_at_ms) AS last_at,
        MAX(CASE WHEN latest=1 THEN model_id END) AS final_model,
        COUNT(model_id)=COUNT(*) AND COUNT(DISTINCT started_at_ms)=COUNT(*) AND MIN(COALESCE(terminal,0))=1 AND MAX(COALESCE(partial,1))=0 AND MAX(COALESCE(conflicted,1))=0 AND native_session IS NOT NULL AND native_turn IS NOT NULL AS confirmed
      FROM turn_input GROUP BY session_id,run_id,native_session,native_turn,harness
    ), turn_neighbors AS (
      SELECT *,LAG(final_model) OVER w AS previous_model,LAG(native_turn) OVER w AS previous_turn,
        LAG(last_at) OVER w AS previous_last,LAG(confirmed) OVER w AS previous_confirmed
      FROM turn_groups WINDOW w AS(PARTITION BY session_id,run_id,native_session,harness ORDER BY first_at,native_turn)
    ), turn_changes AS (
      SELECT i.request_id,n.final_model,n.previous_model,n.previous_turn,
        CASE WHEN n.confirmed=1 AND n.previous_confirmed=1 AND n.previous_last<n.first_at
          AND NOT EXISTS(SELECT 1 FROM turn_input gap WHERE gap.session_id=n.session_id AND gap.started_at_ms>n.previous_last AND gap.started_at_ms<n.first_at AND (gap.native_turn IS NULL OR gap.native_session IS NULL OR gap.conflicted!=0))
        THEN n.final_model!=n.previous_model END AS changed
      FROM turn_input i JOIN turn_neighbors n ON n.session_id=i.session_id AND n.run_id=i.run_id AND n.native_session=i.native_session AND n.native_turn=i.native_turn AND n.harness=i.harness
    ) "#
    )
}
