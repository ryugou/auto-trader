-- 週次進化バッチ (crates/app/src/weekly_batch.rs) が LLM から受け取ったパラメータ提案の記録。
-- 提案は strategy_params へ自動反映せず、この表へ追記だけ行う (承認・版管理は別途)。
-- UPDATE / DELETE はトリガで拒否し、DB レベルで追記専用にする。

CREATE TABLE strategy_param_proposals (
    id                UUID PRIMARY KEY,
    strategy_name     TEXT        NOT NULL,
    base_params       JSONB       NOT NULL,
    proposed_params   JSONB       NOT NULL,
    normalized_params JSONB,
    validation_error  TEXT,
    rationale         TEXT        NOT NULL,
    expected_effect   TEXT        NOT NULL,
    evidence          JSONB       NOT NULL,
    llm_model         TEXT        NOT NULL,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK ((normalized_params IS NULL) <> (validation_error IS NULL))
);

CREATE INDEX strategy_param_proposals_strategy_created_idx
    ON strategy_param_proposals (strategy_name, created_at DESC);

CREATE FUNCTION reject_strategy_param_proposals_mutation() RETURNS trigger AS $$
BEGIN
    RAISE EXCEPTION 'strategy_param_proposals is append-only';
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER strategy_param_proposals_append_only
    BEFORE UPDATE OR DELETE ON strategy_param_proposals
    FOR EACH ROW EXECUTE FUNCTION reject_strategy_param_proposals_mutation();
