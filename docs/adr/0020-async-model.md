# ADR-0020: `async` のモデル（ランタイムのスタックフル・コルーチン、単一スレッド、協調的）

- **状態:** Accepted
- **日付:** 2026-11-05
- **関連:** LANG-008、AI-006（M5）、NFR-006、NFR-007、MEM-005、ADR-0005、ADR-0012、ADR-0013、ADR-0014、ADR-0017、ADR-0018（論点 3・14）、`docs/design/memory-model.md`（2.2・2.5・3）、`docs/plans/m4.md`（T9・T10、分割の判断基準）、issue #55、#56

## 背景

LANG-008 は「`async/await` と、型付きの失敗表現（`Result`）」を求める。`Result` と `?` は M3（ADR-0013）で入った。残るのは `async/await` で、M5 の AI SDK（LLM の呼び出し、ツールの実行、AI-006 のタイムアウトと予算）が使う土台になる。

いまのコンパイラとランタイムの前提は次のとおり。

- 本言語の実行ファイルは単一スレッドで、Python（ADR-0018）は起動時に GIL を取ったまま同じスレッドで動く。
- ARC は IR の独立したパス（ADR-0012）で、引数は +0（借用）で渡す。借用の安全条件（memory-model 2.2）は「+0 で渡してよいのはローカルのスロットから読んだ値だけ」で、`await` を入れる前に再確認すると決めていた。
- 関数値はない（E0213）。Effect は `io` / `exit` / `python` の注釈だけ（ADR-0014）。
- 正しさの安全網は、IR の検証器・終了時の生存数（102）・AddressSanitizer の3つ。
- 対象環境は macOS arm64 と Linux x86_64（NFR-006）。Wasm は Phase 3。
- 体制は 1 人 + AI。M4b の目安は 3〜4 週。

この ADR では、`async` の実行モデル、表面の構文と型、スケジューラの意味論、ARC・Effect・`Result`・Python との関係、範囲外を決める。

## 選択肢と決定

| 論点 | 決定（推奨） | 主な代替案 | 理由 |
|---|---|---|---|
| 1. 実行モデル | **ランタイムのスタックフル・コルーチン。** タスクごとに自分のスタックを持ち、ランタイムがコンテキストを切り替える。コンパイラは `async def` の本体を普通の関数としてコンパイルし、中断点（`await`、`sleep_ms`）はランタイムの組み込み関数の呼び出しになる | (a) IR の状態機械（CPS。lowering が async 関数をヒープ上のフレームと再開関数に変換する）。(b) OS スレッド | (a) は lowering・ARC パス（ローカルがヒープのフレームに移ると「可変な場所」になり、+0 の借用と Move 最適化の前提が崩れる）・バックエンド・`explain` のすべてを変える XL（`m4.md` 分割の判断基準 3）。スタックフルなら、変更はランタイムの新しいモジュール 1 つと IR の命令 3 つ（`spawn` / `await` / `sleep_ms`）に収まり、`await` を「関数呼び出しと同じ地点」として扱える（論点 11）。(b) はデータ競合の保証（memory-model 3）を失い、GIL の方針（ADR-0018 論点 3）とも衝突する |
| 2. コンテキストの切り替え | **手書きのアセンブリ**（Rust の `core::arch::global_asm!`）を `wys_runtime` に置く。aarch64（macOS arm64: x19〜x28、fp、lr、d8〜d15、sp）と x86_64 SysV（Linux: rbx、rbp、r12〜r15、rsp）の 2 種類。`unsafe` はランタイムに閉じ込め、外部クレートは使わない（ADR-0018 論点 2 と同じ方針） | (a) `ucontext`（`makecontext` / `swapcontext`）。(b) `corosensei` などのクレート | `swapcontext` は macOS で非推奨で、切り替えごとに `sigprocmask` のシステムコールを伴い、ASan が「完全には対応しない」と警告する。アセンブリは 2 アーキテクチャで 80 行程度の、よく知られた手順。Windows は対象外（NFR-006） |
| 3. タスクのスタック | タスクごとに `mmap` で **1 MiB を予約**し（物理メモリは触った分だけ）、低位に **ガードページ**（`PROT_NONE`、1 ページ）を置く。あふれると SIGSEGV で異常終了（memory-model 3 の「スタックオーバーフロー」と同じ扱い）。タスクが終わったら `munmap`。`main` のタスクはメインスレッドのスタックをそのまま使う | (a) ヒープから確保（ガードなし）。(b) スタックのプール。(c) 分割スタック | ガードページがないと、あふれは静かなメモリ破壊になり、ASan でも見つからない。プールは M4 では要らない（必要になったらベンチで判断）。大きさは定数にし、見直し条件に入れる |
| 4. 表面の構文 | `async def f(...) -> T [uses ...]:`、前置の `await <後置式>`、組み込み関数 `spawn(<async 関数の呼び出し>) -> Task[T]`、組み込みの名目的な Generic 型 `Task[T]`（`Result` と同じ登録表）、組み込みの async 関数 `sleep_ms(n: int) -> None`。`async` と `await` は予約語、`spawn` / `sleep_ms` / `Task` は組み込みの名前（再定義は E0209）。文法: `func_def = [ "async" ] "def" IDENT ... ; unary = "-" unary \| "await" chain { "?" } \| postfix ; chain = primary { "." IDENT [ "(" [ args ] ")" ] \| "[" expr "]" } ;`（`await` の被演算子は `?` を含まない後置の並び `chain` で、`?` は `await` の外側に付く。これで `await f()?` が `(await f())?` になる（論点 13）。いまの `postfix` は「`chain` の後に `?` が続き得るもの」として書き直す。`?` の直後に `.` / `[` を書けない規則（E0101）は `await` の後でも同じ） | (a) `spawn f(x)` を文にする。(b) Python のコルーチンオブジェクト（`f()` が値になり、`await` や `create_task` に渡す）。(c) 後置の `.await`（Rust） | (b) は「呼び出しが値になる」ので関数値（E0213）に近い仕組みが要り、ARC で扱う値も増える。本 ADR では「`async def` の呼び出しは `await` の直下か `spawn` の引数にだけ書ける」という構文の制約（論点 6）で代える。`spawn` を関数の形にすれば `print` / `panic` と同じ組み込みで、予約語が増えない。Python 風の前置 `await` を利用者が期待する |
| 5. `await` の意味 | 2 つ。**(a)** `await f(args)`（`f` が `async def`）は、その場で `f` を呼ぶ（新しいタスクを作らない。IR は普通の `call`）。`f` の中の中断で現在のタスクが止まる。**(b)** `await t`（`t: Task[T]`）は `t` が終わるまで現在のタスクを止め、結果を +1 所有で返す。終わっていれば止まらずに続ける（切り替えない）。同じ `Task` を何度 `await` してもよく、毎回同じ値（参照型なら同じオブジェクトを retain）を返す。タスクが自分自身を `await` したら実行時エラー（101） | (a) `await f()` もタスクを作ってから待つ。(b) `await` は 1 回だけ（2 回目はパニック） | スタックフルでは (a) は単なる呼び出しで、タスクを作るコストも結果の受け渡しも要らない。「何度でも」は、`Task` が参照型で共有できる（`list[Task[int]]` に入れて配る）ことと整合する |
| 6. 呼び出しの制約と `async` の「色」 | `async def` の関数（と `sleep_ms`）の呼び出し式は、`await` の直下か `spawn(...)` の唯一の引数にだけ書ける。それ以外（式文、代入、引数、演算）は E1303。`await` と `spawn` は `async def` の本体の中にだけ書ける（E1301）。同期の関数から `async def` を呼ぶ方法はない。**`async` は Effect ではない**（`uses` には書かない。`uses pure` の `async def` も書ける） | (a) 同期の関数からも `await` を許す（Go のように、どの関数でも中断できる）。(b) `async` を Effect の 1 つにする | 技術的にはスタックフルならどこでも中断できるが、「この関数は途中で止まり得る」ことを AI と人が署名で読めることが LANG-008 の価値。Effect は外界との関わり（権限）で、M5 の Capability Runtime が強制する対象（ADR-0014 論点 11）。中断は権限ではないので、`?` と `Result` の関係（E1011）と同じ「型検査の規則」にする。M6 の推論の対象にもしない |
| 7. `async def main` / `test_*` | 認める。`async def main() -> None` なら、生成した `@main` が `wys_sched_run(entry)` を呼び、`main` をタスク 0 としてメインスレッドのスタックで動かす。`async def test_x()` も同じ経路。同期の `main` から `spawn` はできない（E1301）ので、`async` を使うプログラムは `async def main` になる。引数と戻り値の規則（E0210 / E0801）は変えない | 同期の `main` のまま `spawn` を許し、暗黙にスケジューラを動かす | 暗黙の既定を作らない（ADR-0014 論点 7 と同じ）。`async` を使わないプログラムの生成コードとリンクは変わらない |
| 8. `spawn` の意味 | `spawn(f(a, b))` は、引数をその場で評価して（**+1 で消費**し、タスクが所有する）、新しいタスクを作り、実行キューの**末尾**に入れ、`Task[T]`（`T` は `f` の戻り値の型。`None` なら `Task[None]`）を返す。タスクの本体は、作った側が中断するまで動かない。タスクは `spawn` を書いた関数が終わっても動き続ける（構造化しない）。`gather` は足さない（`for t in tasks: await t` で書ける） | (a) 構造化並行（タスクグループ。スコープを抜けるまで待つ）。(b) `gather(list[Task[T]])`。(c) 作った瞬間に最初の中断まで動かす（JS の Promise） | 構造化は「ブロックを持つ構文」か「関数値」を要し、M4 の範囲を超える。(c) は実行順が「どこまで同期に進むか」に依存して読みにくい。根（`main`）だけは論点 14 で構造化する。M5 のタスクグループ・タイムアウト（AI-006）はこの `spawn` / `await` の上に足す（論点 15） |
| 9. スケジューラ | 単一スレッド、協調的、横取りなし。実行キューは **FIFO**。中断点は `await`（未完了の `Task`）と `sleep_ms` の 2 つだけ（`spawn` も `print` も切り替えない）。中断したタスクは、待っていた `Task` の完了（待ち手を FIFO で起こす）か、タイマーで実行キューに戻る。タイマーは（期限, 登録の通し番号）の順。実行キューが空になったら、最も早い期限まで待ってからそのタスクを起こす | (a) 優先度、ランダム化。(b) `yield` 相当の組み込み | 実行順がプログラムだけで決まり、Conformance で `# expect-stdout` に順序を書ける（`m4.md` の出口判定「`sleep_ms` による交互実行の順序」）。`yield` は `await sleep_ms(0)` で代用できる |
| 10. 時計と決定性 | `sleep_ms(n)` の期限は、ランタイムの**仮想時計**で `now + n`。仮想時計は起動時 0 で、タイマーが発火したときにその期限まで進む。実行キューが空のときだけ、仮想の期限に実時間（単調時計）が追いつくまで `std::thread::sleep` で待つ。環境変数 `WYS_VIRTUAL_TIME=1` なら実際には待たない（Conformance の harness が設定する）。負の `n` は 0 として扱う | 実時間で期限を決める | 実時間だと、期限が近い 2 つのタスクの順序が CI の揺らぎで変わる。仮想時計なら順序は常に同じで、実際の待ち時間も「仮想時計で n ミリ秒以上」は保たれる。`sleep_ms` の実時間の精度は要件にない |
| 11. 借用の安全条件（memory-model 2.2）と `await` | **規則は変えない。** `await` / `sleep_ms` / `spawn` は IR では組み込みの呼び出し（`Inst::Call`）で、ARC パスは**関数呼び出しと同じ地点**として扱う: (1) 可変な場所から読んだ値は中断点をまたぐ前に retain される（`arc::keeps_places` が偽）、(2) 中断中のタスクのフレームが +0 で借りている引数は、同じタスクのやはり中断中の呼び出し元のスロットが所有していて、ほかのタスクからは到達できない、(3) `spawn` の引数は +1 で消費してタスクが所有する（作った側が先に戻っても安全）。Move 最適化（`take`、`param_never_reassigned`）は関数内の判定なので影響しない | 中断点で借用をすべて retain する保守的な規則 | 単一スレッドで、中断点が IR の明示的な呼び出しなので、既存の規則で足りる。`tests/conformance/lang/async_borrow_across_await.wys`（`f(xs[0], xs)` の `f` が中断している間に別のタスクが `xs` を空にする）を 3 つのモードで通して確かめる |
| 12. データ競合の保証範囲（memory-model 3） | 「MVP はシングルスレッドなので発生しない」を「単一スレッドの協調的なタスクなので発生しない。タスクの切り替えは `await` と `sleep_ms` でだけ起こる」に改める。共有する `list` / `dict` を別のタスクが中断点をまたいで書き換えることはできる（別名による可変性、memory-model 2.5）が、順序の決まった逐次の変更で、競合ではない | — | Phase 3 の並列化の型制約（`Send` / `Sync` 相当）はそのまま将来の課題 |
| 13. `Result` / `?` / パニック / `exit` | `async def f() -> Result[T, E]` は普通に書ける。**`await f()?` は `(await f())?`** と読む（`?` は `await` より弱い。`await (f()?)` は常に E1303 なので曖昧でない）。タスクの中のパニックは、いまと同じくプロセス全体を 101 で止める。`exit()` もプロセスを止める。生存数の検査はしない（ADR-0013 論点 11 のまま） | パニックを `Task` の失敗にして `await` が `Result` を返す | ADR-0013 論点 2「パニックは処理を止めるための仕組み」。回復できる失敗は `Result` で返す（LANG-008 の 2 要素の組み合わせ） |
| 14. `Task[T]` の値、ARC、終了時 | `Task[T]` は参照型（ADR-0012 のヘッダ + 制御ブロックへの参照 + 引数と結果の領域）で、通常の retain / release の対象。`print` / `==` / 演算 / `dict` のキーには使えない（既存の E0201 / E0207 / E0907）。スケジューラはタスクが終わるまで自分の参照（+1）を持つので、利用者がハンドルを捨てても（`await` しなくても）タスクは最後まで動く。結果は最後の release で型ごとの解放処理が release する。**`main` が戻ったら、残っているタスク（実行キュー、待ち、タイマー）がすべて終わるまでスケジューラを回してから** `wys_runtime_finish`（生存数の検査）を呼ぶ。すべてのタスクが待ちで、実行キューもタイマーも空なら `runtime error: deadlock: N task(s) are waiting but none can run`（101） | (a) `main` が戻ったら残りのタスクを捨てる。(b) 捨てる代わりにキャンセルする | スタックフルなコルーチンは巻き戻し（unwinding）なしには途中で捨てられない（中断中のフレームが所有する値を release できず、生存数の検査 102 が失敗する）。最後まで動かせば、生存数 0・出力の決定性・「定義済みの失敗」がそのまま保たれる。無限に待つプログラムは `exit()` で止める |
| 15. キャンセルとタイムアウト（AI-006、M5） | **M4 では入れない。** M5 で入れる方式を予約する: 「中断点での協調的な巻き戻し」。キャンセルされたタスクが次の中断点から戻るときに、生成コードが `wys_task_cancelled()` を見て、真なら関数の出口（ローカルをすべて release する既存の経路）へ跳び、ダミーの戻り値で呼び出し元へ戻る。呼び出し元も `await` の直後の同じ検査で伝搬する。待つ側には `with_timeout(t, ms) -> Result[T, Timeout]` のような組み込みが `Err` を返す。lowering で中断点の直後に分岐を足すだけで、スタックフルのモデルは変わらない | M4 で入れる | M4b の規模（3〜4 週）に収める。M5 の `docs/design/ai-sdk.md` で予算・タイムアウトの API と一緒に決める（`m4.md` 分割の判断基準 4） |
| 16. Python（ADR-0018）との関係 | タスクの中から Python を呼べる（同じスレッド、GIL は持ったまま）。Python の呼び出し中は切り替えが起きない（ブロックする）。`asyncio` との統合は範囲外（Phase 3）。**T10a の最初に確かめること:** CPython 3.14 は C スタックの深さをスタックポインタで検査し、その上限をスレッドの状態の初期化時（メインスレッドのスタック上）に決める。コルーチンのスタックは別の（より低い）アドレスにあるので、タスクのスタック上で Python を呼ぶと `RecursionError` か致命的エラー（`Unrecoverable stack overflow`）になり得る（greenlet が 3.14 対応で同じ問題を扱った。Stable ABI からは上限を書き換えられない）。起きたら、Python を呼ぶ組み込み（`wys_py_*`）は**メインスレッドのスタックに切り替えてから実行する**（サービス呼び出し: 要求するタスクが main のコンテキストへ切り替え、戻り際に帰る）。これは `wys_runtime_python` だけの変更で、言語の意味は変わらない | (a) タスクごとに `PyThreadState` を作って切り替える（上限の再計算は内部構造に依存する）。(b) M4 では Python をタスク 0 からしか呼べないことにする（実行時エラー） | (a) は Stable ABI の外。(b) は工数が足りないときの退避先として残す |
| 17. 標準出力 | `print` はいまのまま（Rust の `stdout`、行バッファ）。切り替えは中断点だけなので、出力の順序は実行順そのもの。ロックを持ったまま中断することはない | タスクごとの出力バッファ | 必要がない |
| 18. AddressSanitizer | 切り替えの前後で `__sanitizer_start_switch_fiber` / `__sanitizer_finish_switch_fiber` を呼ぶ。この 2 つは ASan のランタイムにしかないので、`wys_runtime` は直接参照せず、生成コードの `@main` が ASan モード（`emit_llvm_ir_with(_, sanitize=true)`）のときだけ `wys_set_fiber_hooks(begin, end, unpoison)` で関数ポインタを登録する（通常のビルドでは登録しないので、リンクも生成コードも変わらない）。`munmap` の前に `__asan_unpoison_memory_region` で shadow を消す。3 つのモードすべてで `lang/async_*` を実行する。偽陽性が消せなければ、Python と同じく理由を書いて async のケースだけ除外する | ASan を使わない | ADR-0012 の安全網を保つ。ASan の fiber API はこの目的のためにある |
| 19. IR とツール | IR: `Ty::Task(TaskId)`（`Program.tasks: Vec<Option<Ty>>` に結果の型）、`Function.is_async`、組み込み `Spawn { dst, func, args }`（引数を +1 で消費。理由は `consume_borrowed:spawn`）、`Await { dst, task }`（結果を +1 所有で返す）、`Builtin::SleepMs`。検証器: `Await` / `SleepMs` / `Spawn` は `is_async` の関数の中だけ、`Spawn` の対象は `is_async`、`Await` の被演算子は `Task`。`dump-ir`: `async fn f0 main() {`、`t3 = spawn f1(t1, t2)`、`t4 = await t3`、`call @sleep_ms(t5)`。`wys explain` の `function` に `async`（項目の追加なので版は 0 のまま）。バックエンド: `Task` は `ptr`。型ごとに `@wys_release_task{i}` / `@wys_task_await{i}`、`spawn` の対象の関数ごとに `@wys_task_entry_{f}`（引数を領域から読んで呼び、結果を書き、引数を release する） | `async` を IR に持たない | ADR-0014 論点 9 と同じく、後続のパス（M5 のキャンセル）が使える属性として運ぶ |
| 20. Wasm（Phase 3） | Wasm にはスタックの切り替えがない（stack-switching 提案 / JSPI か、Binaryen の Asyncify 変換が要る）。中断点が IR の明示的な命令で `async` が関数の属性として残るので、必要になれば IR の状態機械への変換を後から足せる（そのときは ARC の前提を再検討する） | 最初から状態機械にする | 論点 1 |
| 21. 範囲外 | OS スレッドと並列実行、非同期 I/O（I/O の組み込みが `print` / `arg` しかない）、チャネル / ロック / セレクト、キャンセル・タイムアウト・タスクグループ（M5）、`Task` のメソッド（`done()` など）、非同期のイテレータ・`async for` / `async with`、Python の awaitable、`asyncio`、Windows | — | M4b の範囲を「`spawn` / `await` / `sleep_ms` で順序の決まった協調的な実行」に絞る |

## 新しいエラーコード（E13xx 帯）

| コード | 内容 |
|---|---|
| E1301 | `await` か `spawn` を `async def` の外で使っている（`main` なら「`async def main` にする」と促す。`related` に関数の定義） |
| E1302 | `await` の被演算子が `Task` でも `async def` の呼び出しでもない（型を示す。同期関数の呼び出しなら「`await` を外す」） |
| E1303 | `async def` の関数（`sleep_ms` を含む）の呼び出しを、`await` の直下か `spawn` の引数以外に書いている（式文、代入、引数、演算。ヒント: `await f(...)` か `spawn(f(...))`） |
| E1304 | `spawn` の引数が `async def` の関数の呼び出しでない（`spawn(t)`、`spawn(g())` で `g` が同期、`spawn(await f())`、引数の数が 1 でない） |

警告は追加しない。`Task` を `print` / `==` / 演算 / `dict` のキーに使うのは既存の E0201 / E0207 / E0907。`await` の後に式がない、`async` の後に `def` がない等は E0101。実行時エラー（101）: `runtime error: deadlock: N task(s) are waiting but none can run`、`runtime error: a task cannot await itself`。タスクのスタックのあふれは SIGSEGV（メッセージなし。見直し条件）。

## 影響

- 実装は 3 つに分ける（`docs/plans/m4.md` の T10a / T10b を 3 つに読み替える）。
  - **T10a:** ランタイム（コンテキストの切り替え、スタック、スケジューラ、タスクのオブジェクト、仮想時計、C ABI、ASan の hook）、Python のスタック上限の確認と対処、memory-model 2.2 / 2.5 / 3 の注記
  - **T10b:** 構文と型検査（`async` / `await` / `spawn` / `Task` / `sleep_ms`、E1301〜E1304、`fmt`、`grammar.ebnf`、`error-codes.md`）
  - **T10c:** lowering・ARC・検証器・`dump-ir`・バックエンド・`explain`・Conformance の実行ケース・e2e・3 つのモード
- 既存のプログラムは変わらない（`async` を使わなければ生成コードもリンクも同じ）。`async` / `await` が予約語になる（既存のケースにこの名前の識別子がないことを T10b で確かめる）。
- 更新する文書: `grammar.ebnf`、`error-codes.md`（E13xx、実行時エラーの表）、`explain-schema.json`（`async`）、`docs/design/ir.md`（`Task`、`spawn` / `await`、glue）、`memory-model.md`（2.2 の `await` の段落、2.5、3 の「データ競合」と「スタックオーバーフロー」）、用語集（タスク、スタックフル・コルーチン、中断点、協調的スケジューラ、仮想時計）、`docs/test-plan.md`（LANG-008 の行）、`docs/roadmap.md`、ADR 一覧、CLAUDE.md（`WYS_VIRTUAL_TIME`）。
- 初版の制限:
  - キャンセル・タイムアウトはない。無限に待つタスクはプロセスを終わらせない（`exit()` で止める）。
  - `spawn` した関数の引数はコピー（参照型は共有）で、戻り値は 1 つ。
  - タスクのスタックは 1 MiB 固定。深い再帰はタスクの中では早くあふれる。
  - Python の呼び出しはタスクを止める（ほかのタスクは進まない）。
  - Linux の ASan で偽陽性が消せなければ、async のケースは ASan から除外する。
- 見直し条件:
  - M5 で AI-006（タイムアウト・予算）とタスクグループを入れるとき（論点 15 の方式、`Capability` と `spawn` の関係）
  - Phase 3 で Wasm を入れるとき（論点 20）、スレッドを入れるとき（論点 12、原子的な参照カウント）
  - T10a の確認で CPython 3.14 のスタック検査が問題にならなかった、または対処が想定より重かったとき（論点 16 の代替 (b)）
  - ベンチでタスクの生成・切り替えのコストが問題になったとき（スタックのプール、大きさ）
  - 関数値を入れるとき（`spawn(f)`、構造化並行、`gather`）
  - Windows を対象に加えるとき（x86_64 の Windows の呼び出し規約、`VirtualAlloc`）
