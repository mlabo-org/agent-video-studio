[English](README.md) | 日本語

# Agent Video Studio

Agent Video Studio（AVS）は、2人の操作者が同時に使うことを前提にした、ローカルで動く動画編集ソフトです。1人は macOS のネイティブな編集アプリを使う人間、もう1人は MCP ツールを使うエージェント（Codex、Claude Code、ほかのプラグイン）です。どちらも同じプロジェクトセッションを編集するので、エージェントの編集は開いている編集画面にすぐ反映され、人間の編集はエージェントの次の呼び出しから見えます。

実装は Rust です。Node、npm、ブラウザ、開発用サーバーは使いません。

## 動かすもの

| 成果物 | インストール先 | 中身 |
|---|---|---|
| エディター | `~/Applications/Agent Video Studio.app` | 編集アプリ（eframe/egui）。`open -a "Agent Video Studio"` で開きます。 |
| MCP サーバー | `~/.local/bin/agent-video-studio-mcp` | エージェント用のツール群。stdio で 69 個のツールを提供します。`.mcp.json` が両方のホストでこれを起動します。 |

AVS には、`PATH` 上の `ffmpeg` と `ffprobe`（または `FFMPEG_PATH` / `FFPROBE_PATH`）が必要です。`drawtext` 付きでビルドした ffmpeg は要りません。字幕は AVS が自分で描きます。

## プラグインのソース（Codex と Claude Code）

このリポジトリは、両方のホストで使う `agent-video-studio` プラグインの唯一のソースです。

- `.codex-plugin/plugin.json`：Codex 用のプラグイン情報。
- `.claude-plugin/marketplace.json`：このリポジトリ自体を Claude Code のマーケットプレイスにします（エントリーのソースは `./`）。`.claude-plugin/plugin.json` はありません。
- `.mcp.json`：両方のホストで共通の起動定義。`$HOME/.local/bin/agent-video-studio-mcp` を起動します。
- `skills/agent-video-studio/SKILL.md`：エージェントが AVS をいつ、どう使うか。
- `local-capability-manifest.json`、`schemas/v1/leaves/`：ほかのプラグインとローカルの機能ブローカーが読む、機能の記述とツールごとの JSON スキーマ。生成物です（「開発」を参照）。

### インストール

必要なものは、macOS 14 以降、Rust のツールチェーン（`cargo`）、Xcode のコマンドラインツール、`ffmpeg`（例：`brew install ffmpeg`）です。リポジトリはどこに置いても動きます。置き場所に依存する箇所はありません。

```bash
AVS_SIGN_IDENTITY="<コード署名の証明書名>" scripts/install.sh   # 証明書がある場合
AVS_SIGN_IDENTITY=- scripts/install.sh                           # 証明書がない場合（アドホック署名）
```

`AVS_SIGN_IDENTITY` には、キーチェーンにあるコード署名証明書の名前を渡します（`security find-identity -v -p codesigning` で一覧が出ます）。自分のマシンで使うだけなら、キーチェーンアクセスの証明書アシスタントで作った自己署名証明書（種類は「コード署名」）で足ります。証明書で署名すると、作り直しても macOS がマイクやフォルダーへの許可を覚えています。`-` を渡すとアドホック署名になります。機能はすべて動きますが、インストールのたびに macOS が許可を聞き直します。スクリプトが勝手にアドホック署名を選ぶことはありません。

スクリプトは、一時ディレクトリでリリースビルドを作り、`macos/AppIcon.icns` をアプリに入れ、アプリと MCP サーバーに署名してから、新しいファイルを所定の場所に移します。場所は `~/Applications/Agent Video Studio.app` と `~/.local/bin/agent-video-studio-mcp` です（`AVS_APP_DIR` と `AVS_BIN_DIR` で変えられます。ただし `.mcp.json` はサーバーが `~/.local/bin` にある前提です）。ソースを変えても、このスクリプトを動かすまでは反映されません。

エディターだけを使うなら、これで終わりです（`open -a "Agent Video Studio"`）。エージェントにツールを使わせるには、ホストにプラグインを追加します。

- Claude Code：`claude plugin marketplace add <このクローンのパス>`（または GitHub のリポジトリ）のあと、`claude plugin install agent-video-studio@agent-video-studio`。ローカルのクローンから入れた場合は、その場所のまま読み込まれ、プラグイン情報の変更は新しいセッションか `/reload-plugins` で反映されます。GitHub から追加したマーケットプレイスは、`claude plugin marketplace update agent-video-studio` と `claude plugin update agent-video-studio@agent-video-studio` で更新します。実行ファイルは、どちらの場合も `scripts/install.sh` が入れたものです。
- Codex：Codex のプラグインマーケットプレイス経由で、このチェックアウトからインストールします。Codex はプラグイン情報のキャッシュを使うので、プラグイン情報を変えたらキャッシュを更新してください。
- そのほかの MCP クライアント：`~/.local/bin/agent-video-studio-mcp` を stdio サーバーとして起動します。

すでに動いている MCP サーバーのプロセスは、そのセッションが読み込み直すまで古い実行ファイルのままです。

## ワークスペース

| クレート | 役割 |
|---|---|
| `crates/avs-core` | タイムラインの文書と、すべての編集。入出力はしません。編集は必ず `ops::Operation` の形を取り、`ops::apply` がコピーの上で実行して、不変条件を破る結果を拒否します。読み取り専用の問い合わせ、`avs.auto-edit-plan.v1`、メディアの配置もここです。 |
| `crates/avs-session` | ディスク上のすべて：プロジェクトセッションのファイル（リビジョン、取り消し・やり直し、活動ログ）、ライブラリの設定、ワークスペース、メディアの取り込み、書き出しジョブの状態。 |
| `crates/avs-render` | 書き出しとライブプレビューの両方が使う、ひとつの ffmpeg 合成グラフ。プレビューで見た絵が、そのまま書き出されます。 |
| `crates/avs-handoff` | CodexVideo の `avs.codexvideo-handoff.v1` マニフェストの取り込み。 |
| `crates/avs-mcp` | MCP サーバー。`tools.json` がツールの一覧です。 |
| `crates/avs-app` | 編集アプリ。 |

### 編集の通り道はひとつ

エディター、MCP ツール、自動編集プラン、ハンドオフの取り込みは、どれも `Operation` の値を作って `Session::apply` / `apply_many` に渡します。そこでライブラリのロックを取り、呼び出し側のリビジョンを確かめ、`avs_core::apply` を実行し、取り消し用のスナップショットを積み、一時ファイルを本来の場所に移します。プロジェクト全体を保存する API は存在しないので、一方の操作者がもう一方の編集を上書きすることはありません。リビジョンが古い呼び出しは拒否され、何も変わりません。エディターは読み込み直して、同じ操作をやり直します。

どこでも成り立つ設計上の決まり：

- 時間は整数のマイクロ秒（`Time`）で持ち、JSON では秒で渡します。誤差を許す比較はありません。
- `apply` は自分の結果を検証します。重なり、メディアの範囲、フェードの長さ、速度、音量、クロップ、リンクの整合性を1か所で調べ、コード（`CLIP_OVERLAP`、`MEDIA_RANGE_EXCEEDED` など）で報告します。
- ロックしたトラックは、クリップへの編集をすべて拒否します（`TRACK_LOCKED`）。
- 映像クリップと、その音を持つクリップは、リンクされた組です。移動、トリム、分割、削除、速度変更は一緒に行われます。片方だけを切った場合は、切れた部分のリンクだけが外れます。
- 分割したあとの後半は `sourceStart + offset × speed` から続き、切り口のフェードは取り除かれます。トリムで収まらなくなったフェードは短くなります。
- `overwrite` は、置いたクリップが覆う範囲だけを空けます。`insert` は、そのトラックに場所を開けます。

## プロジェクトのデータ

- 設定：`~/Library/Application Support/Agent Video Studio/project-library-settings.json` が、いま使うプロジェクトライブラリを指します。`AVS_PROJECT_LIBRARY_ROOT` は、そのプロセスに限って上書きします。`AVS_PROJECT_SESSION_PATH` は、セッションファイルを1つ直接指定します。
- プロジェクトライブラリの中身は、`project-session.json`、`media/`（取り込んだファイル。クリップは `/project-library/media/...` で参照します）、`exports/`、`previews/`、それに隠し状態（`.project-session.lock`、`.export-jobs/`、`.cache/`）です。
- セッションファイルは封筒の版 2 です。ライブラリの ID、リビジョン、プロジェクト、最大 30 個の取り消し用スナップショット、最大 200 件の活動ログが入ります。以前の TypeScript 実装が書いた版 1 のファイルはそのまま読み込め、最初の書き込みで版 2 になります。壊れた取り消し用スナップショットは警告を出して捨てます。読み込みが止まるのは、現在のプロジェクト自体が不正なときだけです。
- ワークスペースの削除は、隣に `.deleted` という名前へ変える操作で、あとから復元できます。

## エディター

1つのウィンドウに5つの領域があります。メディアライブラリ、プレビュー、複数トラックのタイムライン、インスペクター、エージェントの活動（セッションの活動ログと現在のリビジョン。外からの編集は目立つように表示します）です。メニュー（ファイル、編集、マーク、メディア、ツール、表示、再生、ウィンドウ、ヘルプ）はウィンドウの中ではなく、macOS のメニューバーにあります。テーマ、言語（日本語・英語）、文字サイズは、アプリケーションメニューの設定ウィンドウ（App Shell Foundation）で変えます。

キーと編集の振る舞いは Final Cut Pro の流儀に従います。全部の一覧は、アプリのヘルプにあります。テキスト欄を編集している間は、キーはその欄のものです。その間、メニューにはショートカットが表示されません。

| | キー |
|---|---|
| 再生 | Space、J / K / L、←/→ で1フレーム、Shift+←/→ で10フレーム、↑/↓ で前後の編集点、Home/End |
| ツール | A 選択、T トリム、B ブレード、R 範囲、N スナップ |
| 編集 | Cmd+Z / Shift+Cmd+Z、Cmd+B 再生ヘッドで分割、Delete 詰めて削除、Shift+Delete または Forward Delete 隙間を残して削除、Option+[ / Option+] 先頭・末尾を再生ヘッドまでトリム、V ミュート、Cmd+X/C/V、Cmd+D 複製、Cmd+A |
| 範囲 | I / O、X 再生ヘッドの下のクリップを範囲に、Option+X 解除 |
| メディアをタイムラインへ | E 末尾に追加、W 挿入、D 上書き、Q 別のトラックに配置、Option+C 字幕を追加 |
| 表示 | Cmd+プラス / Cmd+マイナス、Shift+Z 全体を表示、ピンチ |
| アプリ | Cmd+I 取り込み、Cmd+E 書き出し、Shift+Option+A ナレーション録音、Cmd+, 設定 |

Final Cut Pro と違う点があります。タイムラインがマグネティック方式ではなくトラック方式なので、あえてこうしています。選択ツールでクリップの端をドラッグすると、ただのトリムになります（詰めるには Option を押すか、トリムツールを使います）。クリップ本体を Option ドラッグするとスライドします。V は音だけをミュートします（絵を隠すにはトラックの M を使います）。範囲はすべてのトラックにまたがります。

ドラッグとスライダーは、コピーの上でプレビューし、放したときに1回だけ確定します。だから、取り消し1回で操作1回ぶんが戻ります。編集に失敗すると、ステータスバーにエラーコードが出ます。ペーストのあと、再生ヘッドは貼り付けたクリップの後ろに移ります。コマンドで再生ヘッドが画面の外に出たときは、タイムラインがそこまでスクロールします。

エディターは、いま使われているライブラリを表示します。エージェントがライブラリを切り替えると（`create_project_session`、`switch_project_library`）、開いているウィンドウも付いていき、ステータスバーでそれを知らせます。`--library` や環境変数で固定したライブラリは切り替わりません。

ナレーション録音は、既定の入力機器の入力チャンネルを1つ選んで、モノラルのファイルに書きます（48 kHz を超えるレートは下げます）。入力の多いオーディオインターフェイスでも使えます。録音ウィンドウには入力ごとのメーターが出て、選んだチャンネルを覚えています。

使い捨てのライブラリでエディターを起動するには `--library <フォルダー>` を付けます。このとき設定はそのフォルダーの中に保存され、本物の設定は読みません。

## MCP ツール

どのツールも `{ok: true, tool, ...}` を返すか、`isError: true` 付きで `{ok: false, tool, error: {code, message, details}}` を返します。変更するツールは、新しい `revision`、結果、検証の要約を返します。プロジェクトを読むには `get_timeline` を使います。

- 読み取り：`get_timeline`、`list_tracks`、`get_media`、`search_media`、`inspect_timeline`、`validate_timeline`、`collect_clips_in_range`、`get_project_session_status`。
- タイムラインの編集（どれも `Operation` 1つ、取り消しできるリビジョン1つ。`expectedRevision` は省略できます）：`add_track`、`reorder_track`、`delete_track`、`set_track_muted`、`set_track_properties`、`register_media`、`delete_media`、`add_clips`、`move_clip`、`move_clips`、`trim_clip`、`ripple_trim_clip`、`roll_edit`、`slip_clip`、`slide_clip`、`trim_clip_to_time`、`trim_clips_to_time`、`delete_left_at_time`、`delete_right_at_time`、`delete_clips_left_at_time`、`delete_clips_right_at_time`、`split_clip`、`blade_at_time`、`delete_clip`、`delete_clips`、`ripple_delete_clip`、`close_gap_at_time`、`lift_range`、`extract_range`、`set_clip_properties`、`set_clip_audio`、`set_caption_text`、`add_captions`、`set_project_settings`。
- `undo_project`、`redo_project`。
- メディア：`import_media_file`（ローカルの絶対パスのファイルをライブラリにコピーして登録します。長さ、ストリーム、寸法は `ffprobe` で調べ、時間切れの上限があります）と、`import_media_file_to_timeline`（同じ処理に加えて、1つのリビジョンで配置まで行います。音のある動画には、空いている音声トラックか新しいトラックに、リンクした音声クリップが付きます）。
- 自動編集プラン：`plan_silence_removal`、`inspect_auto_edit_plan`、`apply_auto_edit_plan`。
- CodexVideo：`import_handoff_manifest`、`import_codexvideo_job`（別名）。
- レンダリング：`render_preview`、`render_final_export`、`start_render_final_export`、`get_render_final_export_status`、`cancel_render_final_export`。
- プロジェクトライブラリ：`get_project_library_settings`、`set_project_library_settings`、`reset_project_library_settings`、`get_project_io_status`、`list_recent_project_workspaces`、`forget_recent_project_workspace`、`inspect_project_library_target`、`create_project_session`、`switch_project_library`、`inspect_project_workspace_delete`、`delete_project_workspace`、`inspect_project_workspace_restore`、`restore_project_workspace`。

`delete_clips`、`lift_range`、`extract_range`、`delete_clips_left_at_time`、`delete_clips_right_at_time`、`apply_auto_edit_plan` には `confirm: true` が必要です。`delete_project_workspace` と `restore_project_workspace` には、それぞれの inspect ツールが返す `confirmDataDirectory` が必要です。

### 自動編集プラン

`avs.auto-edit-plan.v1` は、ほかのエージェントやプラグインが編集を用意して、AVS に適用させるための形式です。

- プランは `planId`、`producer`、`target: {projectId, baseRevision}`、`operations` を持ちます。
- 操作は `{operationId, type, ...引数}` の形です。`type` はタイムライン編集ツールの名前で、引数はそのツールのものです。
- 操作は順番に実行され、それぞれが前の操作のあとのタイムラインを見ます。`plan_silence_removal` は、見つけた無音の位置がずれないように、カットを後ろから前の順で出します。
- `inspect_auto_edit_plan` はプランを試しに実行して `planHash` を返します。`apply_auto_edit_plan` は、まさにそのプランを（`expectedPlanHash`、`expectedRevision`、`confirm: true`）、リビジョン1つ・取り消し1回ぶんとして適用するか、何も変えないかのどちらかです。

AVS は、無音の検出、文字起こし、音声合成、外部サービスの呼び出しをしません。それは上流の機能の仕事で、結果を AVS に渡します。

### CodexVideo のハンドオフ

`import_handoff_manifest` は、`avs.codexvideo-handoff.v1` のマニフェストを読み、素材をライブラリにコピーして、トラック、クリップ、字幕、プロジェクト設定を、リビジョン1つ・取り消し1回ぶんとして作ります。途中で何かが失敗したら、何も保存せず、コピーも消します。戻り値は、マニフェストの ID から AVS のメディア、トラック、クリップ、字幕の ID への対応表です。`job.jobRoot` を記録するのは、マニフェストのファイルがそのフォルダーの中にあるときだけです。ジョブのルートは、レンダリングの出力先になりうるフォルダーだからです。ファイルの長さを 100 ミリ秒未満だけ超えるクリップ（マニフェストの長さがファイルより少し長い場合）は、警告を出してファイルの長さに切り詰めます。

### レンダリング

- 書き出しとプレビューは同じグラフを使います。一覧で先にあるトラックほど上に描かれ、字幕はすべての映像の上に載ります。音は、音声トラックのクリップから出ます。
- 出力は `.mp4` で、いま使っているライブラリの `exports/` か `previews/`、または取り込んだ CodexVideo ジョブの `avs/exports/` か `avs/previews/` に書きます。エディターの「書き出し」は通常の保存ダイアログを使うので、好きな場所に書けます。
- 書き出しは、出力先の隣の `.partial` ファイルに書き、成功したら名前を変えます。失敗や中止のときは、前からある出力に手を付けません。
- `start_render_final_export` は裏で動き、ライブラリごとに1つです。状態はライブラリの中のファイルなので、状態の確認と中止は、どの AVS のプロセスからでも、エディターからでもできます。

## 開発

```bash
cargo test --workspace
cargo clippy --workspace --no-deps --all-targets -- -D warnings
cargo build -p avs-mcp && target/debug/agent-video-studio-mcp --write-contract .   # マニフェストとスキーマを作り直す
crates/avs-app/selftest/run.sh edit1            # ビルドしたエディターを動かし、スクリーンショットを保存する
scripts/make-icon.sh                            # macos/AppIcon.svg を変えたあと、macos/AppIcon.icns を作り直す
scripts/third-party-licenses.sh                 # 依存を変えたあと、THIRD_PARTY_LICENSES.md を書き直す
```

- テストは本物のライブラリに触りません。一時ディレクトリの上に `avs_session::Env` を作ります。レンダリング、取り込み、MCP のテストは、ffmpeg の `lavfi` で小さな素材を生成します。
- `local-capability-manifest.json` と `schemas/v1/leaves/` は、`crates/avs-mcp/tools.json`（名前、説明、入力スキーマ）と `contract.json`（権限の要求、成果物のポート）から生成します。どちらもサーバーに組み込まれるので、作り直すときは、インストール済みのものではなく、変更後のソースからビルドした実行ファイルを使います。コミットしたファイルがずれていると、テストが失敗します。別のテストが、公開している編集スキーマのすべてのプロパティを、それを実行する Rust の型と照らし合わせます。
- エディターの自己テスト（`--self-test <script.json> --shots <フォルダー>`）は、キーとポインターのイベントを入れ、操作を適用し、状態を確かめ、実際のウィンドウのスクリーンショットを保存します。画面がスリープもロックもしていないことが必要で、`run.sh` には GNU の `timeout` が要ります。先にビルドしてください：`cargo build -p avs-app --bins --examples --target-dir target`。
- 本物の CodexVideo ジョブは、`AVS_REAL_HANDOFF_MANIFEST=<パス> cargo test -p avs-handoff --test real_manifest -- --ignored --nocapture` で一時ライブラリに取り込めます。

## 現在の制限

- 実質的に macOS 専用です。エディターのフォント、アプリのバンドル、インストール用スクリプトが macOS を前提にしています。
- `sequence` メディアはレンダリングしません。そのクリップは `SEQUENCE_MEDIA_UNSUPPORTED` の警告を出して飛ばします。
- 字幕：字幕用フォントにない文字（絵文字など）は、警告を出して空白にします。色は 16 進数か基本的な色名です。
- 逆再生（J）は1フレームずつ進み、早送りは音なしで再生します。
- プロパティを「未設定」に戻すことはできません（例：クリップを自動フィットに戻す）。
- 取り込みを取り消すと、メディアはプロジェクトから消えますが、コピーしたファイルはライブラリに残ります。
- レンダリングの出力は、このパッケージの外にあるプロジェクトライブラリに書かれます。そのため、パッケージからの相対パスで成果物の置き場を表すローカルの機能ブローカーでは、出力を記述できません。
- 自己テストはエディターの中でキーを入れるので、macOS のメニューバー、システムのファイルパネル、日本語入力、Finder からのドロップは確かめられません。これらは 2026年11月に、実際のマウスとキーボードで手で確かめました。

## ライセンス

MIT です。`LICENSE` を見てください。エディターと MCP サーバーを構成するクレートと、エディターに埋め込まれるフォントは、`THIRD_PARTY_LICENSES.md` に一覧があります。`ffmpeg` と `ffprobe` は含みません。

`AGENTS.md` と `CLAUDE.md` は、このリポジトリで作業するコーディングエージェント向けの決まりで、`creator-contract.json` はプラグイン作成用の契約です。AVS をビルドしたり使ったりするのに、どれも必要ありません。

## 経緯

2026年11月まで、AVS は TypeScript のプロジェクトでした。Vite の開発用サーバーの上でしか動かない React のエディター、`node --import tsx` で起動する MCP サーバー、ブラウザのプレビューとは別の ffmpeg レンダラーという構成です。監査で見つかった一群の不具合を根から無くすために、Rust で作り直しました。見つかった不具合は次のとおりです。エディターがプロジェクト全体を保存していて、エージェントの編集を上書きできた。分割がクリップの速度を無視していた。複数クリップの上書きが無関係なクリップを消していた。N 本の音声トラックを混ぜると、それぞれが 1/N に下がった。ffmpeg に `drawtext` がないと、書き出しから字幕が落ちた。中止した書き出しが、前の出力を消すことがあった。機能スキーマに生成の仕組みがなかった。ツールの名前と入力スキーマは引き継ぎ、次の点を変えました。`migrate_legacy_project_session` を削除、`set_project_settings` と `set_track_properties` を追加、プランの操作はどの編集でも可、結果は `{ok, tool, ...}` の形、レンダリングの出力先はプラグインのフォルダーではなくプロジェクトライブラリ。TypeScript のソースは、このリポジトリの公開履歴には含まれていません。公開履歴は Rust 版から始まります。
