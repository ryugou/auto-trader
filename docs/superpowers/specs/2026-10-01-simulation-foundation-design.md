# シミュレーション基盤 設計

| 項目    | 内容 |
| ----- | --- |
| 目的    | 過去の USD/JPY データにアルゴリズムを流して評価する仕組み（データ取得、実質上限の計算、スクリプト実行、シミュレーション、評価、パラメータ探索）の仕様を決める |
| 読者    | この基盤の実装者、進化ループの設計者 |
| 正本の範囲 | 折り返し・理論値・実質上限・捕捉率の定義、スクリプトの契約、シミュレーションの約定規則、評価指標、`sim_*` テーブル、`auto-trader-sim` のコマンド |
| 関連文書  | [`2026-10-01-self-evolving-trader-concept.md`](2026-10-01-self-evolving-trader-concept.md)（目標と全体構成）。指標の計算式は `crates/market/src/indicators.rs` を正本とする |

## 1. 範囲

この基盤が行うこと:

- GMO 外国為替 FX の公開 API から USD/JPY の 5 分足（買値・売値）を取得して保存する。
- 保存した足から折り返しを抽出し、理論値と実質上限を計算する。
- スクリプトで書かれたアルゴリズムを隔離して実行し、過去の足に流して売買結果を出す。
- 売買結果を実質上限と比べ、評価指標を計算して保存する。
- 1 つのスクリプトのパラメータを、LLM を使わずに並列で探索する。

この基盤が行わないこと: LLM の呼び出し、候補の作成と選抜、昇格条件の判定、定期実行、ペーパー運用、画面。これらは進化ループ以降の設計書で定める。

既存の `crates/backtest` と `price_candles` テーブルは変更しない。

## 2. 構成

新しい crate `crates/sim`（パッケージ名 `auto-trader-sim`）を追加し、workspace の `members` に加える。ライブラリと、同名のバイナリ `auto-trader-sim` を持つ。

| モジュール | 責務 | 依存 |
| --- | --- | --- |
| `config` | 設定の読み込みと検証 | なし |
| `data` | `sim_candles` の読み書き、GMO 公開 API からの取得 | `sqlx`、`reqwest` |
| `series` | 足の配列、上位足への集約、指標系列の計算とキャッシュ | なし |
| `benchmark` | 折り返しの抽出、理論値と実質上限の計算 | `series` |
| `script` | スクリプトのコンパイル、検証、隔離実行 | `rhai`、`series` |
| `engine` | 1 回のシミュレーション（約定、保護ストップ、売買記録） | `script`、`series` |
| `eval` | 評価指標の計算 | `benchmark`、`engine` |
| `sweep` | パラメータの列挙、並列実行 | `engine`、`eval`、`rayon` |
| `store` | `sim_scripts`、`sim_batches`、`sim_runs` の読み書き | `sqlx` |

追加する外部依存は `rhai`（1 系、`sync` feature）、`rayon`（1 系）、`rand`（0.9 系）、`rand_chacha`（0.9 系）、`clap`（4 系、`derive` feature）とする。`sha2` は workspace の既存定義（`sha2 = { workspace = true }`）を使う。

`Dockerfile` のビルド行を `cargo build --release --bin auto-trader --bin auto-trader-sim` に変更し、`auto-trader-sim` を `/usr/local/bin/auto-trader-sim` に配置する。コンテナの既定コマンドは変更しない。

## 3. 数値の表現

- 価格は、ミリ円（円 × 1000）の `i64` で保持する。`NUMERIC(10,3)` の値と 1 対 1 に対応する。
- 中値は `mid2_x = bid_x + ask_x`（`x` は open、high、low、close。0.0005 円単位の `i64`）で保持する。上位足は `mid2_x` を 6.2 の規則で集約する。
- 1 pip は 0.01 円 = 10 ミリ円 = `mid2` の 20 単位とする。`mid2` の差を pips にするときは 20 で割る。
- 折り返し幅と保護ストップ幅は整数の pips で設定し、比較の前に上記の整数単位へ変換する。折り返しの判定、約定、保護ストップ、損益の合計は、すべて整数で計算する。
- pips への変換（ミリ円 ÷ 10）は、出力と保存の時点で 1 回だけ行い、`f64` とする。
- スクリプトに渡す価格と、指標の計算は `f64`（円）とする。中値の `f64` は `mid2 / 2000.0` とする。

## 4. 設定

`config/default.toml` に次の節を追加する。

```toml
[sim]
gmo_public_base_url = "https://forex-api.coin.z.com/public"
warmup_bars = 3000
protective_stop_pips = 100
thetas_pips = [20, 50, 100]
jobs = 1
max_operations_per_bar = 200000
max_operations_per_run = 1000000000
indicator_cache_mb = 512
```

- `auto-trader-sim` は、環境変数 `CONFIG_PATH`（未設定時は `config/default.toml`）のファイルから `[database].url` と `[sim]` だけを読む。`AppConfig::load` は使わない。
- `[sim]` のキーが欠けている場合は、上記の値を既定値として使う。
- 次を満たさない場合は ERROR を出して終了コード 1 で終了する: `warmup_bars >= 1`、`protective_stop_pips >= 1`、`thetas_pips` は空でなく各値が 1 以上で重複がない、`jobs >= 1`、`max_operations_per_bar >= 1`、`max_operations_per_run >= 1`、`indicator_cache_mb >= 1`。

## 5. データ

### 5.1 テーブル

```sql
CREATE TABLE sim_candles (
    exchange   TEXT        NOT NULL,
    pair       TEXT        NOT NULL,
    timeframe  TEXT        NOT NULL,
    open_time  TIMESTAMPTZ NOT NULL,
    bid_open   NUMERIC(10,3) NOT NULL,
    bid_high   NUMERIC(10,3) NOT NULL,
    bid_low    NUMERIC(10,3) NOT NULL,
    bid_close  NUMERIC(10,3) NOT NULL,
    ask_open   NUMERIC(10,3) NOT NULL,
    ask_high   NUMERIC(10,3) NOT NULL,
    ask_low    NUMERIC(10,3) NOT NULL,
    ask_close  NUMERIC(10,3) NOT NULL,
    PRIMARY KEY (exchange, pair, timeframe, open_time)
);
```

保存する値は `exchange = 'gmo_fx'`、`pair = 'USD_JPY'`、`timeframe = 'M5'` だけとする。

### 5.2 取得

`GET {gmo_public_base_url}/v1/klines?symbol=USD_JPY&priceType={BID|ASK}&interval=5min&date={YYYYMMDD}` を、対象の各日付について `BID` と `ASK` の 2 回呼ぶ。レスポンス形式は GMO コイン 外国為替 FX API 公式ドキュメントの「KLine 情報の取得」に従う。`date` は日本時間 6:00 に切り替わる営業日で、20231028 以降を指定できる。

処理規則:

- リクエストの間隔は 1 秒以上あける。
- `BID` と `ASK` を `openTime` で結合し、両方がそろった足だけを保存する。片方しかない足は保存せず、日付ごとの件数を WARN で記録する。
- 次のいずれかに該当する足は保存せず、日付ごとの件数を WARN で記録する。
  - 買値・売値のいずれかで、価格が 0 以下である。
  - 買値・売値のいずれかで、`high < max(open, close)` または `low > min(open, close)` である。
  - open、high、low、close のいずれかで、売値が買値より小さい。
- 保存は主キーによる upsert とする。同じ日付を再取得しても結果は変わらない。
- HTTP エラー、タイムアウト、レスポンスの `status` が 0 以外の場合は、2 秒・4 秒・8 秒の間隔で 3 回まで再試行する。それでも失敗した日付と `priceType` は ERROR で記録し、次の日付へ進む。
- 失敗が 1 件でもあれば、失敗した日付の一覧を出力して終了コード 1 で終了する。同じ範囲を再実行すれば欠けた日付が埋まる。

## 6. 足の系列と指標

### 6.1 系列

シミュレーションと基準値の計算は、`sim_candles` から `open_time` の昇順で読み込んだ M5 の足の配列を使う。読み込んだ足の先頭を添字 0 とする。

### 6.2 上位足への集約

上位足は `M15`、`H1`、`H4` とする。上位足のバケットは、UTC のエポック秒を足の長さで割った商で決める。バケット内に存在する M5 の足から、最初の open、最大の high、最小の low、最後の close を取って上位足を作る。M5 の足が 1 本もないバケットの上位足は作らない。上位足の系列も、読み込んだ範囲の先頭を添字 0 とする。

上位足のバケット `B` は、`バケット B の終了時刻 <= open_time[t] + 300 秒` を満たすとき、足 `t` の時点で完成しているとする。足 `t` の時点でスクリプトに見せる上位足は、完成しているものだけとする。

### 6.3 指標

指標は `sma`、`ema`、`rsi`、`atr`、`adx`、`bb`、`donchian`、`keltner` の 8 種とする。入力は中値の系列とする。

指標系列の要素 `i` は、`crates/market/src/indicators.rs` の対応する関数を、その時間足の系列の `[0..=i]` に適用した値と一致しなければならない（差は `1e-6` 以下）。指標値は、読み込んだ範囲の先頭の位置（`warmup_bars` と評価期間の開始）に依存する。

| 指標 | 対応する関数 | スクリプトへの戻り値 |
| --- | --- | --- |
| `sma` | `sma(closes, period)` | 小数 |
| `ema` | `ema(closes, period)` | 小数 |
| `rsi` | `rsi(closes, period)` | 小数 |
| `atr` | `atr(highs, lows, closes, period)` | 小数 |
| `adx` | `adx(highs, lows, closes, period)` | 小数 |
| `bb` | `bollinger_bands(closes, period, mult)` | 戻り値のタプル `(lower, middle, upper)` を同名のキーに対応させる |
| `donchian` | `donchian_channel(highs, lows, period, include_current = true)` | 戻り値のタプル `(lower, upper)` を同名のキーに対応させる |
| `keltner` | `keltner_channels(highs, lows, closes, period, mult)` | 戻り値のタプル `(lower, middle, upper)` を同名のキーに対応させる |

引数の制約:

- `period` は 1 以上 1000 以下の整数とする。
- `mult` は整数または小数で受け付け、`(mult × 100)` を四捨五入した整数に丸めて使う。丸めた値は 10 以上 1000 以下（0.1〜10.0）とする。
- `shift` は 0 以上 1000 以下の整数とする。

キャッシュ:

- `(時間足, 指標, period, 丸めた mult)` をキーとして系列全体を計算し、同じデータを使う複数のシミュレーションで共有する。1 つのキーの計算は 1 度だけ行う。
- 1 回のシミュレーションが要求できるキーの種類は 64 までとする。65 種類目を要求した時点で実行時エラーとする。この判定は、そのキーが既にキャッシュにあるかどうかに依存しない。
- キャッシュの合計サイズが `indicator_cache_mb` を超えたら、最後に使われてから最も時間がたった系列から破棄する。破棄は速度にだけ影響し、結果には影響しない。

## 7. 折り返しと基準値

### 7.1 折り返しの抽出

入力は評価期間の `mid2_close` と、折り返し幅 `θ`（`thetas_pips` の値 × 20、`mid2` の単位）とする。

1. 方向未定の間は、先頭からの最高値 `hi` と最安値 `lo` を、その位置とともに保持する。`mid2_close[t] - lo >= θ` になったら、`lo` の位置を最初の折り返し点（安値）とし、方向を上昇、暫定の極値を `mid2_close[t]` とする。そうでなく `hi - mid2_close[t] >= θ` になったら、`hi` の位置を最初の折り返し点（高値）とし、方向を下降、暫定の極値を `mid2_close[t]` とする。
2. 上昇中は、`mid2_close[t]` が暫定の極値より大きければ極値を更新する。そうでなく `極値 - mid2_close[t] >= θ` になったら、極値の位置を折り返し点（高値）として確定し、方向を下降、暫定の極値を `mid2_close[t]` とする。
3. 下降中は 2 の対称とする。
4. 極値の更新は、厳密に大きい（小さい）場合だけ行う。同値の場合は先に現れた位置を保持する。

連続する 2 つの確定した折り返し点の間を **波** と呼ぶ。始点が安値なら上昇の波、高値なら下降の波である。始点の位置を `a`、終点の位置を `b` とする。

次の区間は波として扱わない。

- 最後に確定した折り返し点より後の区間
- 足 `b+1` が評価期間内に存在しない波

波の方向ラベルは、波として扱う区間の足 `a+1` から足 `b` までに付ける。それ以外の足には付けない。

### 7.2 基準値

| 基準値 | 上昇の波 | 下降の波 |
| --- | --- | --- |
| 理論値 | `bid_close[b] - ask_close[a]` | `bid_close[a] - ask_close[b]` |
| 実質上限 | `bid_open[b+1] - ask_open[a+1]` | `bid_open[a+1] - ask_open[b+1]` |

各基準値は、すべての波の値の合計とする。

## 8. スクリプト

### 8.1 契約

アルゴリズムは Rhai のスクリプト 1 ファイルで表現する。スクリプトは次の 2 つの関数を定義する。

```rhai
fn params() {
    #{
        entry: #{ min: 10, max: 60, step: 2, "default": 20 },
    }
}

fn on_bar(ctx, p) {
    let ch = ctx.donchian("M15", p.entry, 1);
    if ch == () { return 0; }
    let c = ctx.close("M15", 0);
    if c > ch.upper { return 1; }
    if c < ch.lower { return -1; }
    ctx.position
}
```

登録時の検証:

- サイズは 32 KiB 以下とする。
- 引数 0 個の `params` と、引数 2 個の `on_bar` が定義されている。判定は `ast.iter_functions()` の関数名と引数の数で行う。
- トップレベルに書かれた関数定義以外の文は、実行しない。
- `params()` は、パラメータ名から `#{min, max, step, "default"}` へのマップを返す。パラメータがない場合は `#{}` を返す。`default` は Rhai の予約語のため、キーは引用符付きの `"default"` で書く。
- 4 つの値は、パラメータごとにすべて整数またはすべて小数とする。
- `min <= default <= max`、`step > 0`、パラメータ数は 12 以下とする。
- `default` は `min + k × step`（`k` は 0 以上の整数）と一致する。小数の場合は `1e-9` の差を許容する。
- `params()` は `max_operations_per_bar` の上限のもとで呼ぶ。

`on_bar(ctx, p)`:

- 評価期間内の M5 の足が 1 本確定するたびに 1 回呼ばれる。評価期間より前の足では呼ばれない。
- `p` は、パラメータ名から値へのマップである。
- `this` は状態を保持するマップで、1 回のシミュレーションの間は保持される。初期値は `#{}` である。
- 戻り値は整数 `1`（買い持ち）、`-1`（売り持ち）、`0`（持たない）のいずれかとする。

呼び出しは `CallFnOptions::new().eval_ast(false).bind_this_ptr(..)` で行う。

### 8.2 `ctx`

`tf` は `"M5"`、`"M15"`、`"H1"`、`"H4"` のいずれかとする。`shift` の 0 は、足 `t` の時点で完成している最新の足を指す。`period`、`mult`、`shift` の制約は 6.3 のとおりとする。

| 呼び出し | 戻り値 |
| --- | --- |
| `ctx.position` | 現在のポジション（`1`、`-1`、`0`） |
| `ctx.entry_price` | 建値（円）。ポジションがなければ `0.0` |
| `ctx.bars_held` | `t - 建てた足の添字`。建てた足では `0`。ドテンで `0` に戻る。ポジションがなければ `0` |
| `ctx.unrealized_pips` | 含み損益。買いは `bid_close[t] - 建値`、売りは `建値 - ask_close[t]` を pips にした値。ポジションがなければ `0.0` |
| `ctx.time` | 足 `t` の終了時刻（UTC のエポック秒） |
| `ctx.hour` | 足 `t` の終了時刻の時（UTC、0〜23） |
| `ctx.weekday` | 足 `t` の終了時刻の曜日（UTC、月曜 = 0） |
| `ctx.spread` | `ask_close[t] - bid_close[t]` を pips にした値 |
| `ctx.open(tf, shift)`、`high`、`low`、`close` | 中値（円） |
| `ctx.sma(tf, period, shift)`、`ema`、`rsi`、`atr`、`adx` | 小数 |
| `ctx.bb(tf, period, mult, shift)` | `#{upper, middle, lower}` |
| `ctx.donchian(tf, period, shift)` | `#{upper, lower}` |
| `ctx.keltner(tf, period, mult, shift)` | `#{upper, middle, lower}` |

- 足や指標の計算に必要な本数が足りない場合、その呼び出しは `()` を返す。
- `tf` が上記以外の場合、`period`・`mult`・`shift` が制約の範囲外の場合、整数を要求する引数に小数を渡した場合は、実行時エラーとする。
- スクリプトは、足 `t` より後の情報を取得する手段を持たない。

### 8.3 隔離

- エンジンは `Engine::new_raw()` を基点とし、`ArithmeticPackage`、`LogicPackage`、`BasicMathPackage`、`BasicIteratorPackage`、`BasicArrayPackage`、`BasicMapPackage`、`BasicStringPackage` だけを登録する。`LanguageCorePackage`、`BasicTimePackage`、`BasicFnPackage`、`BasicBlobPackage`、`DebuggingPackage` は登録しない。
- モジュールの解決には `DummyModuleResolver` を設定し、`import` を失敗させる。
- `print` と `debug` のハンドラは設定しない。
- `eval` は `Engine::disable_symbol("eval")` で無効にする。
- 上限: `on_bar` 1 回あたりの演算数は `max_operations_per_bar`、1 回のシミュレーションの演算数の合計は `max_operations_per_run`、呼び出しの深さは 16、文字列長は 4,096、配列長は 1,024、マップの要素数は 256 とする。
- 演算数の合計は、`Engine::on_progress` が渡す呼び出しごとの累計を、シミュレーションごとの合計へ加算して数える。合計が上限を超えたら、その呼び出しを打ち切る。1 回のシミュレーションは 1 つのスレッド上で最初から最後まで実行する。

### 8.4 エラー

| 事象 | 扱い |
| --- | --- |
| 8.1 の登録時の検証の違反、コンパイルエラー、`params()` の実行時エラー（上限超過を含む） | `invalid_script` として登録を拒否する。エラーメッセージを呼び出し元へ返す |
| `on_bar` の実行時エラー（上限超過を含む）、戻り値が `1`・`-1`・`0` 以外 | そのシミュレーションを中断し、状態 `script_error` として記録する |

エラーを既定値に置き換えて続行することはしない。

## 9. シミュレーション

### 9.1 期間

評価期間を `[from, to)` とし、境界は UTC の 0:00 とする。`from` より前の `warmup_bars` 本を、指標の計算用に読み込む。`from` より前の足が `warmup_bars` 本に満たない場合は、指定可能な最も早い `from` を示して終了コード 1 で終了する。売買、`on_bar` の呼び出し、評価は、評価期間内の足だけで行う。

評価期間内で足が 1 本もない平日（UTC の月曜〜金曜）がある場合は、その日付を WARN で列挙する。

### 9.2 約定規則

ポジションの大きさは常に 1 単位とし、損益は pips で表す。スワップ、レバレッジ、ロスカットは扱わない。ポジションなしから始める。

評価期間の各足 `t` について、次の順で処理する。

1. **約定**: 足 `t` が評価期間の先頭でなく、直前の足の `on_bar` の戻り値が現在のポジションと異なる場合、足 `t` の始値で約定する。買いポジションの決済は `bid_open[t]`、売りポジションの決済は `ask_open[t]`、買いの新規は `ask_open[t]`、売りの新規は `bid_open[t]` とする。ドテンは決済と新規を同じ足の始値で行う。
2. **保護ストップ**: ポジションがある場合、ストップ価格を、買いは `建値 - protective_stop_pips`、売りは `建値 + protective_stop_pips` とする。買いで `bid_low[t] <= ストップ価格` なら `min(ストップ価格, bid_open[t])` で決済する。売りで `ask_high[t] >= ストップ価格` なら `max(ストップ価格, ask_open[t])` で決済する。決済後はポジションなしとする。
3. **判断**: 足 `t` の終値時点で `on_bar` を呼び、戻り値を保持する。`ctx` のポジション、建値、保有足数は、1 と 2 を処理した後の状態を返す。

評価期間の最後の足の `on_bar` の戻り値は約定しない。最後の足の処理後にポジションが残っている場合は、買いは `bid_close`、売りは `ask_close` で決済する。

足と足の間に時間の空きがあっても、約定は次に存在する足の始値で行う。

売買 1 件の損益は、買いが `決済価格 - 建値`、売りが `建値 - 決済価格` とする。決済理由は `signal`、`protective_stop`、`end_of_data` のいずれかとする。

同じスクリプト、パラメータ、データ、設定に対する結果は、`jobs` の値によらず常に同一でなければならない。

## 10. 評価指標

全体の指標:

| 指標 | 定義 |
| --- | --- |
| `total_pips` | 全売買の損益の合計 |
| `trade_count` | 売買の件数 |
| `win_rate` | 損益が正の売買の割合。売買がなければ 0 |
| `max_drawdown_pips` | 決済ごとの累積損益の、最高値からの最大下落幅。最高値の初期値は 0 とする |
| `time_in_market` | 終値時点でポジションを持っていた足の割合 |
| `protective_stop_count` | 保護ストップによる決済の件数 |
| `segments` | 評価期間を足数で 6 等分した各区間の損益合計（余りは最後の区間に含める。売買は決済した足の区間に計上する） |

`thetas_pips` の各値について計算する指標:

| 指標 | 定義 |
| --- | --- |
| `leg_count` | 波の数 |
| `ideal_pips`、`realizable_pips` | 7.2 の理論値と実質上限 |
| `capture_rate` | `total_pips / realizable_pips`。`realizable_pips <= 0` なら null |
| `correct_side_ratio` | 方向ラベルが付いた足のうち、終値時点のポジションが波の方向と一致していた足の割合。方向ラベルが付いた足がなければ null |
| `missed_legs` | 方向が一致していた足の割合が 0.5 未満の波を、実質上限の値の大きい順に最大 20 件。同値の場合は開始時刻の早い順とする |
| `mean_lag_bars` | 方向が一致した足を 1 本以上含む波について、`最初に一致した足の添字 - (a+1)` の平均。対象の波がなければ null |
| `mean_lag_pips` | 同じ波について、`mid2_close[最初に一致した足] - mid2_close[a]` の絶対値を pips にした値の平均。対象の波がなければ null |

`capture_rate` の分子は評価期間全体の損益、分母は波として扱う区間だけの実質上限であり、対象の区間が一致しない。値は負にも 1 超にもなる。`capture_rate` は、同じ評価期間・同じ折り返し幅の結果どうしの比較にだけ使う。

`metrics` 列の JSON は次の形とする。`by_theta` のキーは折り返し幅（pips）の 10 進表記とする。

```json
{
  "segments": [0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
  "by_theta": {
    "20": {
      "leg_count": 0,
      "ideal_pips": 0.0,
      "realizable_pips": 0.0,
      "capture_rate": null,
      "correct_side_ratio": null,
      "mean_lag_bars": null,
      "mean_lag_pips": null,
      "missed_legs": [
        {
          "start_time": "2024-01-02T03:05:00Z",
          "end_time": "2024-01-02T07:40:00Z",
          "direction": 1,
          "realizable_pips": 0.0,
          "flat_bars": 0,
          "opposite_bars": 0
        }
      ]
    }
  }
}
```

`start_time` は足 `a+1` の `open_time`、`end_time` は足 `b` の `open_time`、`direction` は上昇が `1`、下降が `-1` とする。`flat_bars` は、足 `a+1` から足 `b` までのうち、終値時点でポジションなしだった足の数とする。`opposite_bars` は、同じ範囲で波の方向と逆のポジションだった足の数とする。

## 11. パラメータ探索

1 つのスクリプトについて、`params()` の範囲から組み合わせを作り、それぞれをシミュレーションする。

- 各パラメータの候補値は `min + k × step`（`k` は 0 以上の整数）のうち `max` 以下のものとする。小数の場合は `max + 1e-9` 以下と判定する。
- 組み合わせには、パラメータ名の辞書順に並べた混合基数の添字を付ける。辞書順で最後のパラメータを最下位の桁とする。全組み合わせ数は `u128` の飽和演算で求め、2^63 を上限とする。
- 全組み合わせ数が `max_runs` 以下なら全件を実行する。
- 全組み合わせ数が `max_runs` を超える場合は、`default` の組み合わせを必ず含める。残りは、`default` の添字を除いた添字の列から、`rand::seq::index::sample(&mut ChaCha8Rng::seed_from_u64(seed), 全組み合わせ数 - 1, max_runs - 1)` で抽出する。
- 実行は `jobs` 本のスレッドで並列に行う。

## 12. 保存

```sql
CREATE TABLE sim_scripts (
    id            UUID PRIMARY KEY,
    name          TEXT NOT NULL,
    source        TEXT NOT NULL,
    source_sha256 TEXT NOT NULL UNIQUE,
    parent_id     UUID REFERENCES sim_scripts(id),
    origin        TEXT NOT NULL CHECK (origin IN ('human', 'llm')),
    note          TEXT NOT NULL DEFAULT '',
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE sim_batches (
    id          UUID PRIMARY KEY,
    script_id   UUID NOT NULL REFERENCES sim_scripts(id),
    status      TEXT NOT NULL CHECK (status IN ('running', 'completed', 'failed')),
    period_from TIMESTAMPTZ NOT NULL,
    period_to   TIMESTAMPTZ NOT NULL,
    bar_count   INTEGER NOT NULL,
    total_runs  INTEGER NOT NULL,
    config      JSONB NOT NULL,
    started_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    finished_at TIMESTAMPTZ
);

CREATE TABLE sim_runs (
    id                    UUID PRIMARY KEY,
    batch_id              UUID NOT NULL REFERENCES sim_batches(id),
    params                JSONB NOT NULL,
    status                TEXT NOT NULL CHECK (status IN ('ok', 'script_error')),
    error                 TEXT,
    total_pips            DOUBLE PRECISION,
    trade_count           INTEGER,
    win_rate              DOUBLE PRECISION,
    max_drawdown_pips     DOUBLE PRECISION,
    time_in_market        DOUBLE PRECISION,
    protective_stop_count INTEGER,
    metrics               JSONB,
    created_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (batch_id, params)
);

CREATE INDEX sim_batches_script_id_idx ON sim_batches (script_id);
```

スクリプト:

- `source_sha256` は、ソースの UTF-8 バイト列の SHA-256 を小文字の 16 進で表す。
- 同じ `source_sha256` のスクリプトは 1 行だけ保存する。登録済みのスクリプトを再登録した場合は、既存の行を変更せず、その `id` を使う。
- `name` は、スクリプトのファイル名から拡張子を除いたものとする。
- `origin = 'human'` の場合、`parent_id` は NULL とする。

バッチ:

- `run` と `sweep` は、実行のたびに `sim_batches` の行を 1 つ作る。`run` の `total_runs` は 1 とする。
- `bar_count` は評価期間内の足数とする。`config` は、実行時の `warmup_bars`、`protective_stop_pips`、`thetas_pips`、`max_operations_per_bar`、`max_operations_per_run` と、`sweep` の場合の `max_runs`、`seed` を持つ。
- 状態は次のとおり遷移する。

| 契機 | 遷移 |
| --- | --- |
| 実行の開始 | 行を `running` で作成する |
| すべてのシミュレーションの保存が成功 | `completed` にし、`finished_at` を設定する |
| シミュレーションの保存に失敗 | 残りを中止して `failed` にし、`finished_at` を設定して、終了コード 1 で終了する。`failed` への更新にも失敗した場合は ERROR で記録する |

- バッチの行は、設定と引数の検証、スクリプトの登録、足の読み込みがすべて成功した後に作る。
- 行の作成後に、保存の失敗以外の理由（スレッドの panic を含む）で中断した場合も、`failed` にして終了コード 1 で終了する。
- プロセスが強制終了した場合、行は `running` のまま残る。`completed` 以外のバッチは、結果の出力と後続の評価の対象にしない。

シミュレーション:

- シミュレーションの結果は、完了するたびに 1 件ずつ保存する。
- `status = 'script_error'` の行は、`error` に `<エラーが起きた足の open_time（RFC 3339）> <メッセージ>` を持ち、指標の列は NULL とする。
- 個々の売買の一覧は保存しない。

マイグレーションのファイル名は `20261001000001_simulation_foundation.sql` とし、`sim_candles` と上記 3 テーブルの作成だけを含める。既存のテーブルは変更しない。

`auto-trader-sim` は、`migrate` 以外のサブコマンドではマイグレーションを実行しない接続（`PgPoolOptions` を直接使う）を使う。起動時に `sim_candles`、`sim_scripts`、`sim_batches`、`sim_runs` のいずれかが存在しない場合は、不足しているテーブル名と対処を ERROR で出して、終了コード 1 で終了する。

## 13. コマンド

`auto-trader-sim` は次のサブコマンドを持つ。日付は `YYYY-MM-DD` で指定する。`migrate` 以外のサブコマンドは、`--json` を付けると結果を JSON で出力する。

| サブコマンド | 引数 | 動作 |
| --- | --- | --- |
| `migrate` | なし | `migrations/` のマイグレーションを適用する |
| `backfill` | `--from`、`--to` | 5.2 の取得を、`from` から `to` までの各日付（両端を含む）について行う。日付は API の `date` としてそのまま渡す |
| `benchmark` | `--from`、`--to` | `thetas_pips` の各値について、波の数、理論値、実質上限を出力する |
| `run` | `--script <path>`、`--params <json>`、`--from`、`--to` | スクリプトを登録し、シミュレーションを 1 回実行して保存し、指標と所要時間を出力する |
| `sweep` | `--script <path>`、`--from`、`--to`、`--max-runs <n>`、`--seed <n>`、`--jobs <n>` | 11 章の探索を実行して保存し、`total_pips` の上位 10 件と所要時間を出力する |

引数の規則:

- `benchmark`、`run`、`sweep` の `--from` と `--to` は、9.1 の評価期間 `[from, to)` とする。`--from` を省略した場合は、`warmup_bars` を満たす最初の UTC の日付とする。`--to` を省略した場合は、最後の足の UTC の日付の翌日とする。
- `run` の `--params` に指定しなかったパラメータは `default` の値を使う。`--params` を省略した場合は、すべて `default` とする。未知の名前、型の不一致、範囲外の値、`min + k × step` と一致しない値（小数は `1e-9` の差を許容）は、引数の誤りとする。小数のパラメータに JSON の整数を指定した場合は、小数として受け付ける。整数のパラメータに JSON の小数を指定した場合は、型の不一致とする。
- `sweep` の `--max-runs` は必須で、1 以上 1,000,000 以下とする。`--seed` を省略した場合は 42、`--jobs` を省略した場合は設定値とする。
- `sweep` の上位 10 件は `total_pips` の降順とし、同値の場合は `params` の JSON 文字列の昇順とする。
- `run` と `sweep` が登録するスクリプトの `origin` は `human` とする。

終了コード:

| 条件 | 終了コード |
| --- | --- |
| 成功 | 0 |
| 設定の検証違反、引数や期間の誤り、テーブルの不足、`invalid_script`、取得の一部失敗、保存の失敗 | 1 |
| `run` のシミュレーションが `script_error`（結果は保存する） | 1 |
| `sweep` の個々のシミュレーションが `script_error`（続行する） | 0 |

## 14. テスト

| 対象 | 確認する内容 |
| --- | --- |
| 折り返し | 手作りの系列で、波の数、位置、理論値、実質上限が期待値と一致する。逆行がちょうど折り返し幅の場合に折り返しとなる。同値の極値は先の位置を保持する。最後の未確定区間と、足 `b+1` がない波は除外される |
| 集約 | 足 `t` より後の足を書き換えても、足 `t` の時点でスクリプトに見える上位足と指標の値が変わらない |
| 指標 | 8 種すべてで、6.3 の一致条件を満たし、マップのキーと値の対応が 6.3 の表のとおりである。`period`・`mult`・`shift` の範囲外が実行時エラーになる。65 種類目のキーの要求が実行時エラーになる |
| 約定 | 評価期間の先頭の足では約定しない。次の足の始値で、買値・売値を正しく使って約定する。ドテンは同じ足で決済と新規を行う。保護ストップは、足の途中の到達と始値での飛び越えの両方で正しい価格になる。期間末の決済が行われる |
| スクリプト | 無限ループが `script_error` になる。演算数の合計の上限超過が `script_error` になる。戻り値の不正が `script_error` になる。8.1 の検証違反がそれぞれ `invalid_script` になる。トップレベルの文が実行されない。`import`、`sleep`、時刻・乱数の関数が失敗する。`this` の状態が足をまたいで保持される。`mult` に整数と小数の両方を渡せる |
| 評価 | 手作りの売買と波で、10 章の各指標が期待値と一致する。分母が 0 の指標が null になる |
| 探索 | 同じ `seed` で同じ組み合わせになる。`jobs = 1` と `jobs = 4` で、`params`、`status`、`error`、指標の列、`metrics` の集合が一致する。`default` の組み合わせが必ず含まれる。小数の刻みで `max` の値が候補に含まれる |
| 取得 | モックサーバーで、買値・売値の結合、片方だけの足の除外、不正な足の除外、再試行、失敗時の終了コードを確認する |
| 保存 | `#[sqlx::test]` で、スクリプトの重複登録が 1 行になること、`script_error` の行の形、バッチの状態遷移（`completed`、`failed`）を確認する |
| 設定 | 4 章の検証違反がそれぞれ終了コード 1 になる |

## 15. 受け入れ条件

2〜5 は、開発機で、ローカルの DB コンテナに対して `cargo run --release -p auto-trader-sim --` で実行する。事前に `migrate` を実行する。

1. `./scripts/test-all.sh` が `ALL GREEN` で終了する。14 章のテストはこのスクリプトの実行対象に含める。
2. `backfill --from 2023-10-28 --to <実行日の前日>` が終了コード 0 で完了し、`sim_candles` に 200,000 本以上の足が保存される。
3. `benchmark`（`--from`、`--to` を省略）が、`thetas_pips` の各値について波の数、理論値、実質上限を出力する。
4. `crates/sim/scripts/donchian_sar.rhai`（8.1 のスクリプト）を `run`（`--from`、`--to` を省略）し、結果が保存される。所要時間の目標は 5 秒以内とする。超えた場合、実装者は仕様を変えずに測定値を報告する。
5. 同じスクリプトを `sweep --max-runs 26` で、`--jobs 1` と `--jobs 4` のそれぞれで実行し、14 章「探索」の一致条件を満たす。

## 16. 既存システムへの影響

- 本番 DB のマイグレーションは、売買プロセス（`auto-trader`）の起動時にだけ適用する。本番 DB に対して `auto-trader-sim migrate` を実行してはならない。新しいマイグレーションを先に適用すると、古いイメージの売買プロセスが再起動時に「適用済みだがファイルがないマイグレーション」を検出して起動に失敗するためである。
- 本番 DB には `migrations/20260806000001_fx_new_account.sql` が適用済みである。この基盤の本番へのデプロイは、PR #94 と PR #96 が `main` にマージされた後に行う。
- `auto-trader-sim` は本番の売買プロセスと別のプロセスとして動く。売買プロセスのコードと、既存テーブルの内容は変更しない。
- `jobs` の既定値は 1 とする。
- シミュレーションはスワップを扱わない。ポジションを日をまたいで持つアルゴリズムでは、ペーパーの損益とスワップの分だけ差が出る。
