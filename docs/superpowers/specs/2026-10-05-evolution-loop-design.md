# 進化ループ 設計

| 項目    | 内容 |
| ----- | --- |
| 目的    | シミュレーション段を無人で回す仕組み（振り返り、候補の作成、パラメータ探索、選抜、経験の蓄積、定期実行）の仕様を決める |
| 読者    | 進化ループの実装者、ペーパー昇格（部品 4）の設計者 |
| 正本の範囲 | データの区分、候補と世代の状態遷移、1 回の実行と 1 世代の処理順、ゲート、封印区間の評価、経験の記録、LLM の呼び出し規約、予算の制御、`evo_*` テーブル、`auto-trader-sim evolve`、`evolver` サービス |
| 関連文書  | [`2026-10-01-self-evolving-trader-concept.md`](2026-10-01-self-evolving-trader-concept.md)（目標と全体構成）、[`2026-10-01-simulation-foundation-design.md`](2026-10-01-simulation-foundation-design.md)（スクリプトの契約、シミュレーション、基準値、評価指標、パラメータ探索。以下「基盤設計」） |

## 1. 範囲

進化ループが行うこと:

- 取得済みの足を最新の日付まで補う。
- 現在の最良のアルゴリズム（以下「王者」）の成績を振り返り、LLM に修正案または新しいアルゴリズムを書かせる。
- 候補のパラメータを、LLM を使わずに探索する。
- 候補をゲートで選抜し、すべて通過したものを「昇格可能」として記録する。
- 各世代で試したことと結果を記録し、次の世代の LLM への入力に使う。

進化ループが行わないこと: ペーパー口座での実行と入れ替え（部品 4）、実トレードへの適用（部品 5）、新しい情報源の提案（部品 6）、画面（部品 7）。進化ループは「昇格可能」の記録までを担い、部品 4 がそれを読んでペーパーに投入する。部品 4 は、自分の状態を別のテーブルに持ち、`evo_candidates.status` を変更しない。

進化ループは `sim_batches` と `sim_runs` に書き込まない。スクリプトは `sim_scripts` に登録する。

## 2. 基準値の使い方

基準値（理論値、実質上限、確定追随）と捕捉率の定義は、基盤設計 7.2 と 10 章を正本とする。進化ループは次のように使う。

| 基準値 | 使い道 |
| --- | --- |
| 理論値 | 捕捉率の分母。成長を示す見出しの指標に使う |
| 確定追随 | 初期の王者（4 章）の考え方。振り返りの材料にも含める |
| 実質上限 | 選抜には使わない（理論値との差が実測で 0.1% しかないため）。`missed_legs` の並び順としてだけ使われる |

候補どうしの比較は、同じ区間の `total_pips` で行う。同じ区間では基準値が定数なので、どの折り返し幅の捕捉率で比べても順位は変わらない。成長を示す見出しの指標は、王者の、`anchor` 以降の区間（3 章。どの選抜にも使っていない最新の足）での、折り返し幅 `headline_theta_pips`（50）の捕捉率とし、実行ごとに記録する（10 章の `evo_runs`）。 この区間は実行ごとに伸び、区切りが進むと入れ替わるため、見出しの指標の増減には相場の違いが混ざる。相場の違いを除いて見るために、同じ区間での `seed_script`（既定のパラメータ）の `total_pips` もあわせて記録し、王者との差を見る。見出しの指標とこの差は、成長の表示にだけ使い、候補の選抜には使わない。

## 3. データの区分

区分は、`block_days`（30 日）ごとにだけ動かす。区分が同じ間は、王者の基準値とゲートの結果が変わらない。

- `data_to`: 最新の足の `open_time` の UTC の日付の 0:00。その日の足は途中までしかないため、使わない。
- `anchor`: `data_to` 以前で最も新しい、区切りの日付。区切りは `block_epoch`（2024-01-01）から `block_days` 日ごととする。
- 区分は次のとおりとし、すべて UTC の 0:00 を境界とする `[from, to)` とする。

| 区分 | 範囲 | 使い道 |
| --- | --- | --- |
| 学習区間 | `[train_from, validation_from)` | 振り返り、LLM への入力、パラメータ探索、ゲート G1〜G3 |
| 検証区間 | `[validation_from, sealed_from)`。`validation_from = sealed_from − validation_days`（180 日） | ゲート G4・G5 の合否 |
| 封印区間 | `[sealed_from, anchor)`。`sealed_from = anchor − sealed_days`（90 日） | 封印区間の評価（7 章）だけ |

- `train_from` は、基盤設計 13 章の `--from` を省略した場合の値とする。
- 学習区間が `min_train_days`（365 日）に満たない場合、進化ループは ERROR を出して終了コード 1 で終了する。
- `anchor` 以降の足 `[anchor, data_to)` は、次の区切りまで、候補の作成と選抜には使わない。見出しの指標（2 章）の計算にだけ使う。この区間が 1 日に満たない場合、見出しの指標は記録しない（NULL）。

## 4. 王者

**王者** は、状態が `promotable` の候補のうち、その実行の区分で `script_error` にならないものの中で、`promoted_at` が最も新しいものとする。該当する候補が 1 つもない場合は、`seed_script` を既定のパラメータで実行するものを王者とする（この場合、王者に対応する候補の行はない）。

王者は、1 回の実行の最初に 1 度だけ決め、その実行の中では変えない。

`seed_script` の既定は、確定追随の考え方をそのまま書いた次のスクリプト（`crates/sim/scripts/zigzag_follow.rhai`）とする。

```rhai
fn params() {
    #{
        theta: #{ min: 10, max: 100, step: 5, "default": 50 },
    }
}

fn on_bar(ctx, p) {
    let c = ctx.close("M5", 0);
    if c == () { return 0; }
    if !("dir" in this) {
        this.dir = 0;
        this.hi = c;
        this.lo = c;
    }
    let th = p.theta * 0.01 - 0.000000001;
    if c > this.hi { this.hi = c; }
    if c < this.lo { this.lo = c; }
    if this.dir <= 0 && c - this.lo >= th {
        this.dir = 1;
        this.hi = c;
    } else if this.dir >= 0 && this.hi - c >= th {
        this.dir = -1;
        this.lo = c;
    }
    this.dir
}
```

## 5. 状態

### 5.1 世代

世代は、候補を作る 1 回の試みである。

| 種類 | 内容 | LLM |
| --- | --- | --- |
| `retune` | 王者のスクリプトのパラメータを探索し直す | 使わない |
| `modify` | 王者のスクリプトの条件を、LLM に追加・削除・入れ替えさせる | 使う |
| `novel` | 王者と異なる考え方のスクリプトを、LLM に書かせる | 使う |

| 状態 | 意味 |
| --- | --- |
| `running` | 処理中 |
| `completed` | パラメータ探索とゲート G1〜G5 の判定まで終わった（候補が 1 つも残らなかった場合を含む） |
| `llm_error` | LLM の呼び出しが失敗した、または応答を読めなかった |
| `invalid_script` | LLM が返したスクリプトが、修復の上限まで登録時の検証を通らなかった |
| `duplicate` | LLM が返したスクリプトが、登録済みのスクリプトと同じ内容だった |
| `too_slow` | スクリプトの実行が遅く、パラメータ探索を行わなかった（6 章 手順 4） |
| `failed` | DB のエラーなどで中断した |

### 5.2 候補

候補は「スクリプト 1 本とパラメータ 1 組」である。候補の行は、ゲート G1〜G5 の判定が終わった時点で、判定の結果とともに作る。

| 状態 | 意味 | 次の状態 |
| --- | --- | --- |
| `rejected` | いずれかのゲート、または封印区間の評価で不合格になった | なし |
| `finalist` | G1〜G5 を通過し、封印区間の評価を待っている | `promotable`、`rejected`、`expired` |
| `promotable` | 封印区間の評価を通過した。部品 4 がペーパーに投入できる | なし |
| `expired` | 封印区間の評価を受けないまま、区分が次の区切りへ進んだ | なし |

進化ループは、`promotable` の候補の状態を変更しない。

## 6. 1 回の実行

`auto-trader-sim evolve` の 1 回の実行は、次の順で進む。

1. **準備**: 設定を読み込んで検証する。DB に接続し、`sim_*` と `evo_*` のテーブルの存在を確認する。`pg_try_advisory_lock` で固定のキーのロックを取る。取れなければ、INFO を出して終了コード 0 で終了する（別の実行が動いている）。ロックを取った接続は、プールに返さず、実行の終了まで保持する。
2. **後始末**: `running` のまま残っている実行と世代を、すべて `failed` にする。
3. **データの補充**: `sim_candles` に足が 1 本もない場合は、先に `backfill` を実行するよう求める ERROR を出して、終了コード 1 で終了する。補充する前の足から求めた `anchor`（3 章）を含む GMO の営業日から、実行時点の日本時間の日付の前日までを取得する（基盤設計 5.2）。取得済みの日付も取り直す（保存は upsert のため結果は変わらず、途中の欠けが埋まる）。取得した足のうち、`open_time` が `anchor` より前のものは保存しない（`anchor` より前の足は、区切りが進むまで変えない。保存済みの検証区間・封印区間の値と比べられなくなるため）。取得に失敗した日付があれば、WARN を出して続行する。
4. **区分の決定**: 3 章の区分を決める。`anchor_date` が現在の `anchor` と異なる `finalist` の候補を、すべて `expired` にする。
5. **王者の評価**: 王者を決め（4 章）、学習・検証・封印の各区間と `[anchor, data_to)` で実行して、`total_pips` と取引数を得る。結果は、この実行の中で比較の基準として使う。`promotable` の候補が、学習・検証・封印のいずれかの区間で `script_error` になった場合は、WARN を出してその候補を除き、王者を決め直す（除いた候補の ID は `evo_runs.error` に記録する）。`[anchor, data_to)` で `script_error` になった場合は、王者を除かず、見出しの指標を記録しない（NULL）で、WARN を出す。`seed_script` が、学習・検証・封印のいずれかの区間で `script_error` になった場合は、ERROR を出して終了コード 1 で終了する。王者が `seed_script` の場合は、`sim_scripts` に `origin = 'human'`、`name` = ファイル名から拡張子を除いたものとして登録する（登録済みなら、その行を使う）。実行の行（`evo_runs`）を、王者の値と見出しの指標とともに作る。 あわせて、`seed_script` を既定のパラメータで `[anchor, data_to)` で実行し、`total_pips` を記録する（王者が `seed_script` で既定のパラメータの場合は、王者の値をそのまま使う）。この実行が `script_error` になった場合は、記録しない（NULL）で、WARN を出す。
6. **合わせ直し**: 王者のスクリプトについて、現在の `anchor` の区分で開始した `kind = 'retune'` の世代のうち、状態が `completed` または `too_slow` のものがない場合に、`retune` の世代を 1 回実行する。区切りの中では学習区間が変わらず、探索の結果も変わらないため、同じ区切りで繰り返さない。
7. **世代の実行**: 8 章の上限に達するか、`max_generations_per_run` に達するまで、`modify` または `novel` の世代を 1 つずつ順に実行する。世代は並列に実行しない。
8. **封印区間の評価**: 7 章の条件を満たす場合に、1 回だけ行う。
9. **終了**: 実行の行を `completed` にし、実行した世代数、作った候補数、`finalist` と `promotable` になった候補、LLM の呼び出し回数と消費トークン数を出力する。

`--dry-run` を指定した場合は、ロックを取らず、手順 2・3・6・8 と、手順 4 の `expired` への更新、手順 5 の `evo_runs` の作成と `seed_script` の登録を行わない。DB には一切書き込まない。王者の評価と、最初の世代の振り返りの材料の作成までをメモリ上で行い、LLM に送る入力を標準出力に出して終了する。LLM は呼ばない。`--retune-only` を指定した場合は、手順 7 を行わない（手順 6 の条件は変えない）。`--dry-run` と `--retune-only` を同時に指定した場合は、引数の誤りとする。

### 6.1 世代の種類の決定（手順 7）

王者を親とする `modify` の世代のうち、状態が `completed` の直近 `stagnation_generations`（5）件を見る。5 件すべてで、G5 まで通過した候補（状態が `finalist`・`promotable`・`expired` のもの、または `failed_gate` が `G6`・`G4_superseded` のもの）が 1 つもない場合を **停滞** とする。該当する世代が 5 件に満たない場合は、停滞としない。

- 停滞していない場合は `modify`。
- 停滞している場合は、王者を親とする直近の `completed` の世代（`modify` と `novel`）が `modify` なら `novel`、`novel` なら `modify` とする。

### 6.2 1 世代の処理

**手順 1: 振り返りの材料の作成**（`modify` と `novel`）

王者を学習区間でメモリ上で実行し、次をまとめる。検証区間と封印区間の結果は含めない。

- 全体の指標（基盤設計 10 章の `total_pips`、`trade_count`、`win_rate`、`max_drawdown_pips`、`time_in_market`、`protective_stop_count`、`segments`）
- `headline_theta_pips` の `leg_count`、`ideal_pips`、`confirm_pips`、`capture_rate`、`correct_side_ratio`、`mean_lag_bars`、`mean_lag_pips`
- `headline_theta_pips` の `missed_legs` の上位 `evidence_missed_legs`（10）件。各件に、波の方向、`realizable_pips`、`flat_bars`、`opposite_bars` と、足 `a+1` の終値時点の次の値を付ける。
  - 時刻（UTC の時）と曜日
  - M15 の ATR(14)、H1 の ADX(14)（その時点で完成している足の、shift 0 の値）
  - M5 の終値と H1 の SMA(50) の差（pips）
  - スプレッド（基盤設計 8.2 の `ctx.spread`）
  - 王者のポジション（`1`、`-1`、`0`）
- 経験の要約（9 章）

絶対的な日時（年月日）は、材料に含めない。

**手順 2: LLM の呼び出し**（`modify` と `novel`）

8 章の規約で LLM を呼び、スクリプトを得る。

- 応答を読めない場合は、世代を `llm_error` にして終える。
- スクリプトを基盤設計 8.1 で検証する。あわせて、全組み合わせ数（基盤設計 11 章）が `max_combinations`（1,000,000）以下であることを確認する。通らない場合は、8 章の修復を `repair_attempts`（2）回まで行う。それでも通らなければ、世代を `invalid_script` にし、最後のエラーメッセージを `error` に記録して終える。
- `source_sha256` が登録済みのスクリプトと一致する場合は、世代を `duplicate` にして終える。
- スクリプトを `sim_scripts` に、`origin = 'llm'`、`parent_id` = 王者のスクリプト、`name` = 8 章で整えた名前、`note` = `change_summary` として登録する。

**手順 3: 対象のスクリプト**

`retune` では王者のスクリプト、`modify` と `novel` では手順 2 のスクリプトを対象とする。

**手順 4: パラメータ探索**

まず、対象のスクリプトを既定のパラメータで、学習区間で 1 回実行し、所要時間を測る。`所要時間 × 実行数 ÷ 並列数` が `max_sweep_secs`（1,800）を超える場合は、世代を `too_slow` にして終える。実行数は、全組み合わせ数と `sweep_max_runs`（1,000）の小さいほう（基盤設計 11 章で実際に実行される数）とする。並列数は、基盤設計 11 章の、実際に使うスレッド数とする。

次に、学習区間で探索する（基盤設計 11 章）。実行数は上と同じ、乱数の種は世代ごとに決めて記録する。結果は保存せず、メモリ上で、`status = 'ok'` のものを `total_pips` の大きい順に `top_k`（5）組まで残す（同値は、パラメータの JSON 文字列の昇順）。残す前に、次の組を除く: `retune` で王者と同じパラメータの組。同じ `anchor_date` で、同じスクリプトと同じパラメータの候補がすでにある組。同じスクリプトと同じパラメータの `promotable` の候補がすでにある組。

残った組が 0 の場合は、候補を作らずに世代を `completed` にする。

**手順 5: ゲート G1〜G5**

残った各組について、G1 から順に判定する。1 つでも不合格なら、その組は `rejected` として、不合格のゲートを記録する。G5 まで通過した組は `finalist` とする。判定の結果と使った数値は、候補の行の `gate_results` に保存する。

| ゲート | 区間 | 合格条件 |
| --- | --- | --- |
| G1 取引の量と質 | 学習 | 1 日あたりの取引数が `min_trades_per_day`（0.2）以上。保護ストップによる決済が取引数の `max_protective_stop_ratio`（0.05）以下。`total_pips` が正 |
| G2 パラメータの頑健さ | 学習 | 各パラメータを 1 つずつ、1 刻みだけ上と下にずらした組（範囲内のものだけ。最大でパラメータ数 × 2 組）の `total_pips` の中央値が、その組の `total_pips` の `robust_ratio`（0.6）倍以上。ずらした組が 1 つもない場合は合格 |
| G3 期間の安定 | 学習 | `segments` の 6 区間のうち、`min_positive_segments`（4）区間以上で損益が正 |
| G4 検証 | 検証 | `total_pips` が正で、王者の検証区間の `total_pips` を、**必要な差**（下記。最小の差は `min_margin_pips_validation`（60））を超えて上回る。1 日あたりの取引数が G1 の基準を満たす。1 日あたりの損益が、学習区間の 1 日あたりの損益の `decay_ratio`（0.4）倍以上 |
| G5 コストの耐性 | 検証 | `total_pips − 取引数 × stress_cost_pips`（0.5）が正で、王者について同じ式で計算した値を上回る |

- **必要な差** は、`max(promote_margin × |王者の total_pips|, 最小の差)` とする（`promote_margin` は 0.10）。「必要な差を超えて上回る」は、`total_pips > 王者の total_pips + 必要な差` を指す。割合だけで決めると、王者の値が小さいときに、わずかな差で入れ替わってしまうためである。
- 「1 日あたり」は、区間の暦日数（`to − from`）で割る。
- `gate_results` には、検証区間の `total_pips` と取引数を必ず保存する（7 章の判定し直しに使う）。
- G2 でずらした組が `script_error` になった場合、その組の `total_pips` は 0 として中央値に含める。
- G4・G5 の実行が `script_error` になった場合、そのゲートは不合格とする。

## 7. 封印区間の評価

封印区間は、同じデータで何度も試すと、偶然通過する候補が出る。評価は、次の条件をすべて満たす場合に、1 回の実行につき 1 候補だけ行う。

- 現在の `anchor` の `finalist` が 1 つ以上ある。
- 直近の封印区間の評価（`evo_sealed_evals` の `evaluated_at` の最大値）から `sealed_eval_interval_days`（3 日）以上たっている、または評価が 1 件もない。

手順:

1. `finalist` のそれぞれについて、`gate_results` に保存した検証区間の値で、この実行の王者に対する G4 と G5 の「上回る」条件を判定し直す。満たさなくなったものを、`rejected`（不合格のゲートは `G4_superseded`）にする。
2. 残った `finalist` のうち、`evo_sealed_evals` に行がないものの中から、**学習区間** の `total_pips` が最大の 1 つを選ぶ（同値は `created_at` の早い順）。検証区間の値は、選ぶ基準に使わない。
3. 選んだ候補を封印区間で実行する。結果の `evo_sealed_evals` への記録（`script_error` の場合も 1 件として記録する）と、手順 4 の候補の状態の更新は、1 つのトランザクションで行う。
4. 次をすべて満たせば合格とし、候補を `promotable` にして `promoted_at` を設定する。満たさなければ `rejected`（不合格のゲートは `G6`）にする。
   - `total_pips` が正で、王者の封印区間の `total_pips` を、必要な差（6.2 手順 5。最小の差は `min_margin_pips_sealed`（30））を超えて上回る。
   - 1 日あたりの取引数が G1 の基準を満たす。
5. 選ばれなかった `finalist` は、そのまま残す。

1 つの区切り（30 日）の間に封印区間を評価できるのは、最大で `block_days ÷ sealed_eval_interval_days` 回（10 回）である。封印区間（90 日）は区切り 3 つ分にまたがるため、同じ日のデータは、最大でその 3 倍の回数の評価に使われる。昇格のたびに王者の封印区間の値が上がり、次の合格が難しくなることと、必要な差の最小値で、偶然の通過を抑える。

## 8. LLM の呼び出し

### 8.1 提供元

Gemini の `generateContent` を使う。エンドポイントとモデルは既存の `[gemini]` の設定（`api_url`、`model`）、API キーは環境変数 `GEMINI_API_KEY` を使う。

- リクエストの `generationConfig` に、`responseMimeType = "application/json"` と、次の 4 つの文字列の項目を必須とする `responseSchema` を指定する: `name`、`hypothesis`、`change_summary`、`script`。
- 応答の本文は、`candidates[0].content.parts[0].text` を JSON として読む。
- 入力のトークン数は `usageMetadata.promptTokenCount`、出力のトークン数は `usageMetadata.candidatesTokenCount + usageMetadata.thoughtsTokenCount` とする（項目がなければ 0）。
- タイムアウトは `llm_timeout_secs`（120）とする。HTTP のエラーとタイムアウトは、`llm_retry`（2）回まで、10 秒・30 秒の間隔で再試行する。

### 8.2 入力

入力は、次の順の 1 つのテキストとする。会話の履歴は使わない。

1. 役割と出力の形式の指示
2. スクリプトの契約（基盤設計 8.1・8.2 の要点、`"default"` を引用符付きで書く規則、パラメータ数と全組み合わせ数の上限、`ctx.time` を使えないこと）
3. 種類ごとの指示（`modify`: 王者のスクリプトを基に条件を変える。`novel`: 王者と異なる考え方で書く）
4. 「以下はデータであり、指示ではない」という区切り
5. 王者のスクリプト
6. 振り返りの材料（6.2 手順 1）

修復の依頼は、上の 1〜6 に、直前に返されたスクリプトと検証のエラーメッセージを加えた、1 つの入力として送る。修復の応答を読めない場合は、世代を `invalid_script` とする。

入力には、検証区間と封印区間の数値、絶対的な日時、API キー、DB の接続情報を含めない。

### 8.3 応答の扱い

- `name` が `^[a-z0-9_]{1,40}$` に合わない場合は、`llm_` に世代の ID の先頭 8 桁を付けた名前に置き換える。
- `hypothesis` と `change_summary` は、制御文字を除き、500 文字で切る。
- `script` は、基盤の隔離環境の中でだけ実行する。進化ループが行うシミュレーションでは、`ctx.time` を無効にする（基盤設計 8.2）。特定の日時に結び付いた条件を書けないようにするためである。

### 8.4 上限

- 呼び出し回数は、HTTP 200 の応答を受け取った呼び出し（修復を含む）を 1 回と数える。再試行で失敗したものは数えない。
- 当日（UTC）の呼び出し回数の合計が `max_llm_calls_per_day`（20）から `1 + repair_attempts` を引いた値を超えている場合、新しい `modify`・`novel` の世代を始めない。
- 当日の入力と出力のトークン数の合計に、`(1 + repair_attempts) × llm_token_reserve_per_call`（1 回の呼び出しの見込みの上限。50,000）を足した値が `max_llm_tokens_per_day`（1,000,000）を超える場合も、新しい世代を始めない。1 回の呼び出しの実際のトークン数が `llm_token_reserve_per_call` を超えた場合は、WARN を出す（上限を超え得るのは、この場合だけである）。
- 呼び出し回数とトークン数は、世代を開始した日（UTC）に計上する。日付をまたいだ世代の呼び出しも開始した日に入るため、翌日の上限には数えない。ずれは 1 世代分（`1 + repair_attempts` 回）までである。
- 呼び出し回数とトークン数は、呼び出しのたびに、応答の直後に世代の行へ保存する。
- `llm_error` の世代が、同じ実行の中で 3 回続いた場合は、ERROR を出して実行を `failed` にし、終了コード 1 で終了する。

## 9. 経験の要約

LLM への入力に含める要約は、DB の記録から毎回作る。要約そのものは保存しない。

**系譜** は、王者の候補から、`世代 → parent_candidate_id` を NULL までたどって得られる候補の集合とする。王者が `seed_script` の場合、系譜は空とする。

要約の内容:

- 親（`parent_candidate_id`）が系譜に含まれる世代と、親が NULL の世代（`seed_script` が王者だったときの試行）の、直近 `experience_recent`（10）件。各世代について、種類、状態、`change_summary`、最も先のゲートまで進んだ候補の、学習区間の数値（`total_pips`、取引数、G1〜G3 の判定に使った値）と、G4・G5 の合否。**検証区間と封印区間の数値、および封印区間の評価の合否は含めない。** 封印区間の合否を渡すと、後の世代が封印区間に合わせ込めるためである。
- 全世代を通じて、不合格の理由になった回数が多いゲートの上位 3 つと、その回数（`G6` は数えない）
- `invalid_script` になった世代の、直近 5 件のエラーメッセージ（各 300 文字まで）

## 10. 保存

```sql
CREATE TABLE evo_runs (
    id                        UUID PRIMARY KEY,
    status                    TEXT NOT NULL CHECK (status IN ('running', 'completed', 'failed')),
    anchor_date               DATE NOT NULL,
    train_from                TIMESTAMPTZ NOT NULL,
    validation_from           TIMESTAMPTZ NOT NULL,
    sealed_from               TIMESTAMPTZ NOT NULL,
    champion_candidate_id     UUID,
    champion_script_id        UUID NOT NULL REFERENCES sim_scripts(id),
    champion_params           JSONB NOT NULL,
    champion_train_pips       DOUBLE PRECISION NOT NULL,
    champion_validation_pips  DOUBLE PRECISION NOT NULL,
    champion_sealed_pips      DOUBLE PRECISION NOT NULL,
    data_to                   TIMESTAMPTZ NOT NULL,
    champion_fresh_pips       DOUBLE PRECISION,
    headline_capture_rate     DOUBLE PRECISION,
    seed_fresh_pips           DOUBLE PRECISION,
    config                    JSONB NOT NULL,
    error                     TEXT,
    started_at                TIMESTAMPTZ NOT NULL DEFAULT now(),
    finished_at               TIMESTAMPTZ
);

CREATE TABLE evo_generations (
    id                   UUID PRIMARY KEY,
    run_id               UUID NOT NULL REFERENCES evo_runs(id),
    kind                 TEXT NOT NULL CHECK (kind IN ('retune', 'modify', 'novel')),
    status               TEXT NOT NULL CHECK (status IN ('running', 'completed', 'llm_error', 'invalid_script', 'duplicate', 'too_slow', 'failed')),
    parent_candidate_id  UUID,
    script_id            UUID REFERENCES sim_scripts(id),
    hypothesis           TEXT,
    change_summary       TEXT,
    sweep_seed           BIGINT,
    sweep_runs           INTEGER,
    sweep_script_errors  INTEGER,
    llm_calls            INTEGER NOT NULL DEFAULT 0,
    llm_input_tokens     BIGINT NOT NULL DEFAULT 0,
    llm_output_tokens    BIGINT NOT NULL DEFAULT 0,
    error                TEXT,
    started_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    finished_at          TIMESTAMPTZ
);

CREATE TABLE evo_candidates (
    id               UUID PRIMARY KEY,
    generation_id    UUID NOT NULL REFERENCES evo_generations(id),
    script_id        UUID NOT NULL REFERENCES sim_scripts(id),
    params           JSONB NOT NULL,
    status           TEXT NOT NULL CHECK (status IN ('rejected', 'finalist', 'promotable', 'expired')),
    failed_gate      TEXT,
    gate_results     JSONB NOT NULL,
    anchor_date      DATE NOT NULL,
    train_pips       DOUBLE PRECISION NOT NULL,
    validation_pips  DOUBLE PRECISION,
    train_metrics    JSONB NOT NULL,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    promoted_at      TIMESTAMPTZ,
    UNIQUE (generation_id, script_id, params)
);

CREATE TABLE evo_sealed_evals (
    id            UUID PRIMARY KEY,
    candidate_id  UUID NOT NULL REFERENCES evo_candidates(id),
    run_id        UUID NOT NULL REFERENCES evo_runs(id),
    anchor_date   DATE NOT NULL,
    status        TEXT NOT NULL CHECK (status IN ('ok', 'script_error')),
    total_pips    DOUBLE PRECISION,
    trade_count   INTEGER,
    passed        BOOLEAN NOT NULL,
    evaluated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX evo_candidates_status_idx ON evo_candidates (status);
CREATE INDEX evo_generations_started_at_idx ON evo_generations (started_at);

ALTER TABLE evo_runs
    ADD CONSTRAINT evo_runs_champion_candidate_fk
    FOREIGN KEY (champion_candidate_id) REFERENCES evo_candidates(id);
ALTER TABLE evo_generations
    ADD CONSTRAINT evo_generations_parent_candidate_fk
    FOREIGN KEY (parent_candidate_id) REFERENCES evo_candidates(id);
```

- `evo_runs.champion_candidate_id` と `evo_generations.parent_candidate_id` は、王者が `seed_script` の場合に NULL とする。この 2 列の外部キーは、テーブルの作成順が循環するため、全テーブルを作った後に `ALTER TABLE` で付ける。
- `failed_gate` は、`G1`〜`G5`、`G4_superseded`、`G6` のいずれかとする。
- `validation_pips` は、G4 まで進まなかった候補では NULL とする。
- `train_metrics` は、その組の学習区間の実行の、基盤設計 10 章の全体の指標と `metrics` の内容を持つ。
- `evo_runs.config` は、その実行で使った `[evolve]` と `[evolve.gates]` の設定値を持つ。
- `champion_fresh_pips` と `headline_capture_rate` は、`[anchor, data_to)` での王者の値である。この区間が 1 日に満たない場合は NULL とする。 `seed_fresh_pips` は、同じ区間での `seed_script`（既定のパラメータ）の `total_pips` で、同じ場合に NULL とする。
- 当日の LLM の呼び出し回数とトークン数は、`started_at` が当日（UTC）の `evo_generations` の合計で判定する。

既存のテーブルは変更しない。マイグレーションは `20261006000001_evolution_loop.sql` の 1 ファイルとする。

## 11. 設定

```toml
[evolve]
seed_script = "crates/sim/scripts/zigzag_follow.rhai"
headline_theta_pips = 50
block_epoch = "2024-01-01"
block_days = 30
sealed_days = 90
validation_days = 180
min_train_days = 365
sealed_eval_interval_days = 3
stagnation_generations = 5
max_generations_per_run = 5
max_llm_calls_per_day = 20
max_llm_tokens_per_day = 1000000
llm_token_reserve_per_call = 50000
repair_attempts = 2
max_combinations = 1000000
sweep_max_runs = 1000
max_sweep_secs = 1800
top_k = 5
evidence_missed_legs = 10
experience_recent = 10
llm_timeout_secs = 120
llm_retry = 2

[evolve.gates]
min_trades_per_day = 0.2
max_protective_stop_ratio = 0.05
robust_ratio = 0.6
min_positive_segments = 4
promote_margin = 0.10
min_margin_pips_validation = 60
min_margin_pips_sealed = 30
decay_ratio = 0.4
stress_cost_pips = 0.5
```

検証（違反は終了コード 1）:

- `headline_theta_pips` は `[sim].thetas_pips` に含まれる。
- 日数と回数は 1 以上。`top_k <= sweep_max_runs <= 1,000,000`。`max_llm_calls_per_day > repair_attempts`。`1 <= llm_token_reserve_per_call`、かつ `(1 + repair_attempts) × llm_token_reserve_per_call <= max_llm_tokens_per_day`。
- `min_trades_per_day > 0`。`1 <= min_positive_segments <= 6`。`max_protective_stop_ratio`、`robust_ratio`、`decay_ratio` は 0 以上 1 以下。`promote_margin >= 0`。`min_margin_pips_validation >= 0`。`min_margin_pips_sealed >= 0`。`stress_cost_pips >= 0`。
- `seed_script` のファイルが存在し、基盤設計 8.1 の検証を通る。

## 12. コマンドと実行形態

`auto-trader-sim` に、サブコマンド `evolve` を追加する。引数は `--dry-run` と `--retune-only`（6 章）とする。

| 条件 | 終了コード |
| --- | --- |
| 実行が `completed` で終わった（LLM の上限に達して世代を実行しなかった場合を含む） | 0 |
| 別の実行がロックを持っていた | 0 |
| 設定の検証違反、引数の誤り、テーブルの不足、学習区間の不足、`seed_script` の `script_error`、`llm_error` の 3 連続、DB のエラー | 1 |

定期実行は、`docker-compose.yml` に次の `evolver` サービスを追加して行う。

- 売買プロセスと同じイメージを使う。既存の `auto-trader` サービスに `image: auto-trader:latest` を加え（`build: .` は残す）、`evolver` は `image: auto-trader:latest` だけを指定する（イメージを 2 つ作らないため）。
- `network_mode: host`、`volumes: ./config:/app/config:ro`、`environment` に `CONFIG_PATH`、`GEMINI_API_KEY`、`RUST_LOG`、`depends_on: db（service_healthy）`、`restart: unless-stopped`、`cpus: 1.0` とする。
- `command` は `["sh", "-c", "while true; do if auto-trader-sim evolve; then sleep 21600; else sleep 600; fi; done"]` とする。終了コード 0 なら 6 時間後、0 以外なら 10 分後に次を実行する。初回の起動でマイグレーションの適用より先に走った場合や、DB の一時的な障害の場合に、6 時間止まらないようにするためである。同じエラーが続く場合は、10 分ごとに ERROR が出る。

`Dockerfile` に、`COPY crates/sim/scripts/ /app/crates/sim/scripts/` を追加する。

マイグレーションは売買プロセスの起動時に適用される。`evolver` は、テーブルが存在しない場合、エラーで終了する。

## 13. テスト

- 区分の計算: 区切りの日付、3 区間の境界、区切りをまたぐ前後、学習区間の不足のエラー。
- 王者の決定: `promotable` がない場合、複数ある場合、最新の `promotable` が `script_error` になる場合（次の候補、または `seed_script` に切り替わる）。`seed_script` の登録が 1 回だけ行われること。
- 種類の決定: 停滞の判定、停滞中の `modify` と `novel` の交互。
- 振り返りの材料: 手作りの系列で、取り逃した波の各値が期待値と一致する。検証区間・封印区間の数値と、絶対的な日時が含まれない。
- ゲート: 各ゲートの境界値。王者の値が 0 以下の場合の G4 と G5。G2 のずらした組が 0 の場合と `script_error` の場合。G4・G5 の `script_error`。
- 封印区間の評価: 間隔の条件、G4 と G5 の判定し直し（`G4_superseded`）、学習区間の値での選択、すでに評価の記録がある候補を選ばないこと、必要な差（割合と最小の差のそれぞれが効く場合）、合否、記録と状態の更新が同じトランザクションであること、区切りが進んだときの `expired`。
- 合わせ直し: 同じ区切りで 2 回実行しないこと。同じ区切りに同じ組の候補がある場合に作り直さないこと。
- 見出しの指標: `[anchor, data_to)` での値が記録されること。この区間が 1 日に満たない場合に NULL になること。 同じ区間の `seed_script` の値が記録されること。
- LLM の呼び出し（モックサーバー）: 正常な応答、JSON でない応答、検証を通らないスクリプトと修復、修復の応答を読めない場合、重複、HTTP エラーと再試行、3 世代連続の失敗による終了、当日の呼び出し回数とトークン数の上限、名前と自由文の整形。
- 経験の要約: 系譜のたどり方、件数の上限、検証区間と封印区間の数値が含まれないこと。
- `ctx.time` を参照するスクリプトが、進化ループの実行では実行時エラーになること。
- `seed_script`（`zigzag_follow.rhai`）: 登録でき、手作りの系列で、折り返し幅だけ逆行した足の次の足でドテンすること。
- 排他: ロックを取れない場合に、何も書き込まずに終了コード 0 で終わること。残っていた `running` の実行と世代が `failed` になること。
- `--dry-run` が DB に書き込まず、LLM を呼ばないこと。
- 一連の流れ（モックの LLM）: 王者より良いスクリプトが返った場合に `finalist` ができ、封印区間の評価を経て `promotable` になる。悪いスクリプトが返った場合に `rejected` になる。

## 14. 受け入れ条件

1. `./scripts/test-all.sh` が `ALL GREEN` で終了する。
2. 開発機で、実データに対して `evolve --dry-run` を実行し、LLM への入力が出力される。入力に、検証区間と封印区間の数値と、絶対的な日時が含まれていない。
3. 開発機で、実データと実際の LLM に対して `evolve` を、`max_generations_per_run = 2` で 1 回実行し、実行・世代・候補が記録される。所要時間、LLM の呼び出し回数、消費トークン数を記録する。
4. 3 の結果から、1 日あたりの LLM の費用の見積もりを報告する。
5. `seed_script` を、`theta` を `headline_theta_pips` にして全期間で実行し、`total_pips` と、同じ折り返し幅の `confirm_pips` を記録する。両者の差が、期間の最後に残ったポジションの決済と、保護ストップによる決済で説明できる範囲であることを確認する。

## 15. 既存システムへの影響

- 基盤（`crates/sim`）に、確定追随の計算、捕捉率の分母の変更、`ctx.time` を無効にする設定を実装する（基盤設計 7.2、8.2、10 章）。
- 売買プロセスのコードは変更しない。
- 本番 DB に `evo_*` のテーブルが追加される。マイグレーションの適用と撤回の注意は、基盤設計 16 章と同じである。
- `evolver` サービスは、本番ホストの CPU を 1 コアまで使う。
- LLM の費用が発生する。上限は `max_llm_calls_per_day` と `max_llm_tokens_per_day` で決まる。
