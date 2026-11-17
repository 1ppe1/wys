# wys — AIネイティブ次世代プログラミング言語

[English](README.md)

`wys` は、人間と AI の双方が安全に開発できることを目指す、静的型付きのコンパイル言語。Python に近い書き方で、型を検査してから単独の Native 実行ファイルを作る。LLM の呼び出しとツールを使うエージェントを標準の関数とデコレータで書け、権限・承認・予算は**ランタイムが強制する**（プロンプトでの指示に頼らない）。

> **現在のステータス:** 最初の公開版 [0.1.0](https://github.com/1ppe1/wys/releases/tag/v0.1.0) を公開（Phase 2 の AI ネイティブ MVP まで）。0.y の間は構文・組み込み・CLI が予告なく変わり得る（[安定性と互換性](docs/stability.md)）。

## 特徴

- **型安全:** 型注釈と推論、`T | None` の絞り込み、レコード（構造的）、Generics、`match` の網羅性検査、`Result[T, E]` と `?`。外から来る値は `Unknown` で、`validate[T]` で検証するまで使えない。
- **Native 実行:** LLVM IR を生成し clang でリンクする。ランタイムを静的リンクした単独の実行ファイルになる。メモリは参照カウント（ARC）と Move 最適化。
- **AI SDK:** 型付きの `llm_generate[T]`、`@tool` / `@agent`、Capability・承認・予算のランタイム強制、監査ログ、トレース、記録と再生、評価ケース（`wys eval`）。
- **連携と道具:** CPython の埋め込み、`async` / `await`、`wys fmt` / `wys test` / `wys explain`、機械向けの JSON 診断、LSP（`wys lsp`）。

## 例

```wys
type Point = {x: int, y: int}

def describe(n: int | None) -> str:
    if n is None:
        return "nothing"
    return "value"

def main() -> None:
    p = Point(x=1, y=3)
    print(p.x + p.y)
    print(describe(None))
```

型の誤りはコンパイル時に見つかる（`wys check --format json` で機械向けの診断）。

### ランタイムが止める

ツールに `approval="required"` を付けると、LLM がそのツールを呼ぼうとしても、承認がなければランタイムが実行しない。[`examples/agent_demo.wys`](examples/agent_demo.wys) から:

```wys
@tool(description="Save a note to disk.", approval="required")
def save_note(title: str) -> None uses io:
    print("saved " + title)
```

既定の方針は拒否。LLM（この例ではモック）が `save_note` を呼んでも実行されず、監査イベントに残る:

```console
$ wys run examples/agent_demo.wys
notes: 25 chars, saved=false
0 llm_called
1 tool_called char_count
2 llm_called
3 approval_requested save_note
4 approval_denied save_note
5 llm_called
```

`WYS_APPROVAL=allow` で許可すると、ツールが実行される:

```console
$ WYS_APPROVAL=allow wys run examples/agent_demo.wys
saved notes
notes: 25 chars, saved=false
0 llm_called
1 tool_called char_count
2 llm_called
3 approval_requested save_note
4 approval_granted save_note
5 tool_called save_note
6 llm_called
```

保証する範囲と、しない範囲は [セキュリティ](docs/security.md) に書いてある。

## インストール

対象環境は macOS arm64 と Linux x86_64（ADR-0029）。Windows は対象外。前提は `clang`（リンクに使う）。

配布物から入れる（[Release](https://github.com/1ppe1/wys/releases/latest) の最新版を取ってきて、SHA-256 を確かめてから展開する。ADR-0030）:

```sh
curl -fsSL https://raw.githubusercontent.com/1ppe1/wys/main/scripts/install.sh | sh
```

既定では `~/.local` に入る。`sh -s -- --prefix <dir>` や `--version <版>` で変えられる。

macOS のバイナリは公証していないので、Gatekeeper に止められたら `xattr -d com.apple.quarantine "$HOME/.local/bin/wys"`。

ソースから作る（Rust のツールチェーンは `rust-toolchain.toml` で固定）:

```sh
git clone https://github.com/1ppe1/wys.git && cd wys
cargo build --release
target/release/wys --version
target/release/wys run examples/hello.wys
target/release/wys run examples/agent_demo.wys   # @tool / @agent（既定のプロバイダはモック。ネットワークに出ない）
```

## ドキュメント

日本語の文書が正。入門・AI SDK・セキュリティ・安定性には英訳がある（[docs/en/](docs/en/)、ADR-0031）。

| ドキュメント | 内容 |
|---|---|
| [入門](docs/guide/getting-started.md) | インストールから、型・`None`・`Result`・テスト・非同期まで |
| [AI SDK](docs/guide/ai-sdk.md) | 型付きの LLM 呼び出し、ツールと agent、権限・承認・予算 |
| [Python との連携](docs/guide/python.md) | CPython の埋め込み |
| [リファレンス](docs/guide/reference.md) | コマンド、組み込み、環境変数、エラーコード |
| [セキュリティ](docs/security.md) | 保証すること・しないこと |
| [ベンチマーク](docs/benchmarks.md) | 計測の方法と結果（絶対値と計測環境だけ） |
| [文書の索引](docs/README.md) | 要件定義書、設計、ADR、計画などすべての文書 |

## ロードマップ

| フェーズ | 内容 | 状態 |
|---|---|---|
| Phase 1 | 言語・Native の MVP（型検査、単独の実行ファイル、テスト） | 完了 |
| Phase 2 | AI ネイティブ MVP（型付きの LLM 出力、権限拒否、予算超過の検証） | 完了 |
| Phase 3 | 高性能・広域展開（SIMD・並列、Wasm、GPU、パッケージ管理） | 未着手 |

詳細は [docs/roadmap.md](docs/roadmap.md) と要件定義書の「7. 優先順位とフェーズ」。

## 開発

```sh
./scripts/ci.sh   # fmt・clippy・テスト・Conformance（Debug / Release / AddressSanitizer）など、CI と同じ検査
```

設計判断は [ADR](docs/adr/README.md) に記録する。AI エージェント（Claude Code）向けの指示は [CLAUDE.md](CLAUDE.md)。

## ライセンス

[MIT](LICENSE-MIT) OR [Apache-2.0](LICENSE-APACHE) の二重ライセンス（利用者が選べる。ADR-0028）。Copyright (c) 2026 1ppe1。依存クレートのライセンスは [docs/third-party-licenses.md](docs/third-party-licenses.md)。

貢献の手順は [CONTRIBUTING.md](CONTRIBUTING.md)、行動規範は [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md)、脆弱性の報告は [SECURITY.md](SECURITY.md)、変更履歴は [CHANGELOG.md](CHANGELOG.md)。
