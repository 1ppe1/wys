# 安定性と互換性

`wys` の版の付け方、何をどこまで変えずに保つか、対象環境、サポート期間、失敗したときの挙動をまとめる（NFR-006 / NFR-007 / NFR-008。決定は ADR-0029）。

## 1. 版

- SemVer の `0.y.z`。最初の版は `0.1.0`。`wys --version`（`wys version --format json`）で版・コミット・target を確かめられる。
- `0.y` の間は、`y` を上げるときに破壊的変更を入れてよい。`z` は破壊的変更を含まない修正だけ。
- 変更は [CHANGELOG.md](../CHANGELOG.md) に書く（Keep a Changelog の形式）。
- `1.0` の条件はまだ決めていない。

## 2. 安定性の段階

| 面 | 段階 | 約束 |
|---|---|---|
| 言語の構文と意味、組み込みの関数と型 | 不安定 | `0.y` を上げるときに変わり得る。変えたら CHANGELOG に列挙する |
| エラーコード（`E0207` など） | 安定 | 意味を変えず、再利用しない。廃止するときは「廃止」と明記して残す（[一覧](spec/error-codes.md)） |
| 診断の JSON（`wys check --format json`）、`wys explain` の JSON | 版で管理 | `schema_version` を上げずに、項目を消したり意味を変えたりしない。項目の追加では版を上げない |
| proxy の JSON 行のプロトコル、記録（`WYS_LLM_RECORD`）、トレース（`WYS_TRACE`） | 版で管理 | 各行の `v` で管理する（ADR-0023 / ADR-0025） |
| CLI の副コマンドとフラグ、環境変数 | 不安定（文書化する） | `0.y` を上げるときに変わり得る。[リファレンス](guide/reference.md)に書く |
| ランタイムの ABI、IR、Rust のクレートの API、ランタイムのシンボル名 | 内部 | 何も約束しない。生成した実行ファイルとランタイム（`lib/`）は同じ版の組み合わせで使う |

## 3. 対象環境

| Tier | 環境 | 検査 |
|---|---|---|
| 1 | macOS arm64 | 版ごとにすべての検査（Debug / Release / AddressSanitizer の Conformance を含む）を回す |
| 2 | Linux x86_64 | 版ごとに Docker で検査を回す |
| — | Windows | 対象外 |

- Python との連携は **CPython 3.14 だけ**（ADR-0018）。
- 書き方は Python に近いが、**Python との完全な互換は目指さない**。Python のコードはそのままでは動かない。

## 4. サポート期間

- 対象は最新の `0.y` だけ。修正は最新の `0.y.z` に出し、古い `0.y` には持ち帰らない。
- 脆弱性の報告は [SECURITY.md](../SECURITY.md)。
- 性能の目標（SLO）はまだ決めていない。計測の結果は [ベンチマーク](benchmarks.md)にあるが、約束ではない。

## 5. 失敗したときの挙動（NFR-007）

ここに書く挙動は、それぞれの ADR で決めたものを集めたもの。

### 予期できる失敗は `Result`

- 失敗し得る処理は `Result[T, E]` を返し、`?` で呼び出し元に伝える（ADR-0013）。`unwrap` はない。失敗を握りつぶすところは `case Err(e): panic(...)` と書いて目立たせる。
- 外から来る値（`parse_json` など）は `Unknown` で、`validate[T]` が `Err` で「どこが・何が違うか」を返す。

### パニック（実行時エラー）

- 想定外の状態（添字の範囲外、`panic(msg)` など）では、stdout を書き出してから stderr に `runtime error: <内容>` を出し、**終了コード 101** で終わる。場所が分かるときは ` at ファイル:行:列` を付ける。スタックトレースは出さない（ADR-0013）。
- 機械的にはこの接頭辞と終了コードで判別できる。
- 終了時に解放されていないオブジェクトが残っていたら、`runtime error: N object(s) were not freed (this is a compiler bug)` を出して**終了コード 102** で終わる（ARC の誤りの検出。正常終了したときだけ検査する）。

### Python との境界（FFI）

- Python を呼ぶ操作はすべて `Result[..., PyError]` を返す。Python の例外はパニックにしない（AC-010）。`PyError` は `{kind, message, traceback}`（ADR-0018）。
- Python を初期化できなければ `runtime error: cannot initialize Python`（101）。ビルドのときに Python 3.14 が見つからなければツールのエラー（終了コード 2）。
- Python はサンドボックスではない。Python のコードは任意の副作用を起こせる（[セキュリティ](security.md)）。

### LLM とツール

- `llm_generate` と agent の失敗は、すべて `Err(LlmError)` で返し、パニックにしない（ADR-0023）。`kind` は次のとおり。
  - `denied`: 一覧にないツール、または Capability が足りない。`llm_generate` を中止する。
  - `budget`: 回数・トークン・費用の予算を超えた。
  - `timeout`: agent の期限を過ぎた。
  - `schema`: ツールの引数が型に合わない。
  - `approval`: 承認を求めたが、入力が EOF だった、または方針の値が不正。
  - `provider`: プロバイダとの通信の失敗、不正な応答、応答の打ち切り（`max_tokens`）など。
  - `mock_exhausted` / `replay`: テスト用のモックの応答が尽きた、記録の再生が要求と食い違った。
- テスト用の `llm_mock_push` を、実プロバイダや再生の設定で呼ぶと実行時エラー（101）。テストの設定が本番の呼び出しに化けないようにするため。
- 承認者が拒んだときは中止せず、`denied by approver` をツールの結果として LLM に返して続ける。
- **タイムアウトは呼び出しの境界で判定する**: LLM 呼び出しの前、応答を受け取った直後、ツールの実行の前。実行中のツールや HTTP の通信は中断しない。期限を過ぎて届いた応答は受け取らず、要求されたツールも呼ばない。
- ツールが返した `Err` は、エラーとして LLM に返す（プログラムは止まらない）。

### ツール（CLI）の失敗

- `wys` のコマンド自体の失敗（ファイルがない、clang や Python が見つからないなど）は、終了コード 2。
- 型の誤りなどの診断は `wys check --format json` で機械向けに読める。
