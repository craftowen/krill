<h1 align="center">
  <a href="https://krill.page/en"><img src="https://krill.page/icon-192.png" alt="Krill" width="56" valign="middle" /></a> Krill
</h1>

<p align="center">
  <a href="https://github.com/craftowen/krill/stargazers"><img src="https://img.shields.io/github/stars/craftowen/krill?style=flat&amp;label=%E2%98%85&amp;color=ff5238" alt="GitHub スター数" /></a>
  <a href="https://github.com/craftowen/krill/releases/latest"><img src="https://img.shields.io/github/v/release/craftowen/krill?style=flat&amp;color=2a3040" alt="最新リリース" /></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-3b82f6?style=flat" alt="ライセンス: Apache-2.0" /></a>
  <img src="https://img.shields.io/badge/macOS%20Apple%20Silicon%20%7C%20Windows%20x64%20%7C%20Mobile%20PWA-10b981?style=flat" alt="対応プラットフォーム: macOS Apple Silicon、Windows x64 プレビュー、モバイル PWA" />
</p>

<p align="center">
  <sub><a href="README.md">English</a> · <a href="README.ko.md">한국어</a> · <a href="README.zh-CN.md">简体中文</a> · <a href="https://krill.page/en">Web サイト</a> · <a href="https://krill.page/en/docs">CLI リファレンス</a> · <a href="GETTING_STARTED.md">はじめに</a> · <a href="https://github.com/craftowen/krill/releases/latest">リリース</a></sub>
</p>

<p align="center">
  <strong>複数の CLI コーディングエージェントを、ネイティブアプリで並行実行。Electron も Web ビューも使いません。</strong><br/>
  Claude Code、Codex、Antigravity、Cursor、OpenCode にそれぞれ Git worktree とターミナルを割り当て、あなたの対応を待っているエージェントのキューと diff レビューを備えています。最新の 10 ターミナルベンチマーク（2026-10-28、Krill 0.2.20 対 Orca 1.4.242、画面オン）では、Krill のメモリ使用量は Orca の <strong>1,153.6 MiB</strong> に対して <strong>153.3 MiB</strong> で、約 1/7 でした。
</p>

<p align="center">
  <a href="https://krill.page/en"><img src="https://krill.page/app-screenshot-en.png" alt="使い捨てリポジトリ上の通常のターミナルと実際の Git 変更を表示した、Krill のネイティブ英語 UI プレビュー" width="960" /></a>
  <br/>
  <sub>使い捨ての Git リポジトリで通常のターミナルを表示した、ネイティブ英語 UI のプレビュー（インストール済みの macOS 0.1.37）。<a href="https://krill.page/app-screenshot.png">韓国語 UI での実際の Claude Code セッション</a>もご覧ください。</sub>
</p>

---

## Orca（Electron）や `tmux` ではなく Krill を選ぶ理由

[Orca](https://github.com/stablyai/orca) のようなエージェントワークスペースは、すでにアプリを再起動してもターミナルセッションを維持でき、Git worktree も管理できます。**アプリ再起動後のセッション維持は当然の前提であり、差別化要因ではありません。**

違いは**リソース効率とプロセスアーキテクチャ**にあります。Orca は Electron（Chromium + Node.js）上に構築されており、メイン、GPU、レンダラー、デーモンの各プロセスを別々に起動し、開いているタブごとにレンダラー内にターミナルインスタンスを保持します。Krill は同じ並行エージェントワークフローを、**Electron も Web ビューも使わない単一の Rust（`gpui`）UI プロセス**で提供します。UI の描画状態は表示中のペインにだけ接続し、ターミナルの状態は 1 つの軽量デーモン（`krilld`）に保持します。その土台となるのが、panic しない PTY 親プロセス（`krill-holder`）です。

### 再測定: Krill 0.2.20 対 Orca 1.4.242（2026-10-28、画面オン）

測定環境とマシンは下記の 2026-10-25 の測定と同じですが、ディスプレイは**オン**（ウィンドウ表示あり）で、マシンは高負荷の状態でした（実行中のロードアベレージは 11–52）。Krill は 1 回目の実行後に 0.2.19 から 0.2.20 へ自動更新され、10 ターミナルと 20 ターミナルの Krill の実行はすべて 0.2.20 を使用しました。値は 3 回実行の中央値です。

| 指標（10 ターミナル） | Krill 0.2.20 | Orca 1.4.242 | Krill と Orca の比 |
|---|---:|---:|---|
| メモリ（`phys_footprint`、アプリとデーモンの全プロセス） | **153.3 MiB** | 1,153.6 MiB | 約 1/7 |
| アイドル時 CPU（30 s 平均、画面オン） | **1.9%** | 12.9% | 約 1/7 |
| インストールサイズ（`.app` バンドル） | **95.6 MiB** | 704.7 MiB | 約 1/7 |

- 20 ターミナル: メモリ 309.4 vs 1,347.6 MiB。1 ターミナル: 125.4 vs 853.7 MiB。
- 画面オンでは `krill-app` 自体が約 111 MiB を使用しました（以前の画面ロック時の測定では 30 MiB）。そのためメモリの差は 1/15 に届いていません。

### 以前の測定: 実測ベンチマーク Krill 0.2.2 対 Orca 1.4.237（2026-10-25）

同じ **Mac mini（Apple M6、32 GB、macOS 27.0.1）** で、未改変の `/Applications` リリースビルド、新規の分離プロファイル、同一の `1200×760pt` ウィンドウ（実寸 `1176×746`）を使い、18/18 回の有効な実行から 3 回実行の中央値を取りました（各ターミナルは韓国語、ANSI カラー、>120 桁のテキストを 20,000 行、~2.8 MB 出力。画面ロック時）。

| 指標（10 ターミナル、3 回実行の中央値） | Krill 0.2.2 (Rust · GPUI) | Orca 1.4.237 (Electron) | Krill と Orca の比 |
|---|---:|---:|---|
| **メモリ（`phys_footprint`、アプリとデーモンの全プロセス）** | **85.9 MiB** | 1,274.9 MiB | **1/15**（`1 / 14.8`） |
| **アイドル時 CPU（出力後 30s 平均、画面ロック時）** | **1.13%** | 31.0% | **1/27**（`1 / 27.4`） |
| **出力処理の CPU 時間（10 × 2.8 MB のバースト）** | **1.22 s** | 6.44 s | **1/5**（`1 / 5.3`） |
| **インストールサイズ（ディスク上の `.app` バンドル）** | **92.1 MiB** | 689.1 MiB | **1/7**（`1 / 7.5`） |

#### メモリの内訳（10 ターミナル、`phys_footprint` の中央値）

| プロセスグループ | Krill 0.2.2 | Orca 1.4.237 | 差が出る理由 |
|---|---:|---:|---|
| **アプリと UI 描画** | **30.2 MiB** (`krill-app`) | **994.3 MiB** (`GPU 522.9` + `Main 259.3` + `Renderer 212.1`) | 単一のネイティブ Rust GPUI プロセス vs. Chromium のメイン + GPU + レンダラープロセス |
| **ターミナルデーモン** | **46.5 MiB** (`krilld 44.9` + `holder 1.6`) | **256.3 MiB** (`orca-terminal-host`) | 1,000 行のホット行 + `zstd` コールドスクロールバック vs. Node/JS ターミナルホスト |
| **その他のヘルパー** | **9.2 MiB** (`krill-link serve`) | **24.1 MiB** (`parcel-watcher 14.0` + `Network 7.6` + `crashpad 2.5`) | 内蔵のモバイル/トンネルゲートウェイ vs. Electron のユーティリティヘルパー |
| **合計（シェルを除く）** | **85.9 MiB** | **1,274.9 MiB** | **Krill のメモリ使用量は 1/15（`1 / 14.8`）** |

#### ターミナル数によるスケーリング（1 · 10 · 20 ターミナル、3 回実行の中央値）

| 指標 | ターミナル数 | Krill 0.2.2 | Orca 1.4.237 | Krill / Orca |
|---|---:|---:|---:|---|
| **メモリ（`phys_footprint`）** | 1 | **56.6 MiB** | 844.9 MiB | `1 / 14.9`（1/15） |
| | 10 | **85.9 MiB** | 1,274.9 MiB | `1 / 14.8`（**1/15**） |
| | 20 | **115.0 MiB** | 1,382.7 MiB | `1 / 12.0`（**1/12**） |
| **アイドル時 CPU（30s 平均、画面ロック時）** | 1 | **0.97%** | 32.6% | `1 / 33.6`（1/34） |
| | 10 | **1.13%** | 31.0% | `1 / 27.4`（**1/27**） |
| | 20 | **1.17%** | 29.0% | `1 / 24.8`（**1/25**） |
| **出力処理の CPU 時間（2.8 MB × N）** | 1 | **0.05 s** | 1.07 s | `1 / 21.4` |
| | 10 | **1.22 s** | 6.44 s | `1 / 5.3`（**1/5**） |
| | 20 | **2.91 s** | 7.69 s | `1 / 2.6` |

<details>
<summary><strong>測定条件と既知の制限（数値と併せてお読みください）</strong></summary>

- **同一のセットアップ**: 未改変の `/Applications` バンドル（`Krill 0.2.2`、`Orca 1.4.237`）を Mac mini（Apple M6、32 GB、macOS 27.0.1、2026-10-25）で実行。実行ごとに新規の分離プロファイルと `1200×760pt` ウィンドウ（実寸 `1176×746`）を使い、1 コミットの同じ Git リポジトリで `N` 個のログインシェル（`/bin/zsh -l`）タブを開き、1 タブのみ表示しました（全 18 回の実行で `orca_orphaned_terminals = 0`）。
- **同一の負荷と集計方法**: 各タブが 20,000 行（~2.8 MB の韓国語、ANSI カラー、>120 桁の行）を出力します。30s の待機後、`phys_footprint` を 5s 間隔で 3 回サンプリングし、プロセスごとの中央値を、アプリ、ヘルパー、GPU、レンダラー、デーモンの全プロセス（`krill-link serve` を含む）について合計しました。ログインシェルと `perl` ジェネレーターは両者とも除外しています。
- **画面ロック状態**: 全 18 回の実行は、ディスプレイをロックした状態（ウィンドウが画面に合成されない状態）で記録しました。ディスプレイをロックしていても、Orca の GPU プロセスは~21–24%、レンダラーは~7.5% の CPU を使用しました。ロック解除時やウィンドウ表示時の CPU は別の測定であり、ここでは何も主張していません。
- **共有マシンの負荷**: 測定はバックグラウンドタスクが同時に動く共有ワークステーションで行いました（1 分間のロードアベレージ `15.0–184.8`、繰り返しごとにアプリの順序を交互に変更）。メモリ（`phys_footprint`）はバックグラウンド負荷の影響をほとんど受けませんが、CPU とバースト処理の実経過時間は変動することがあります。
- **アーキテクチャの違い**: Krill は表示中のペインだけをデーモンに接続し、ターミナルの状態を 1 つのデーモン（`krilld`）に保持します。一方 Orca は、タブごとにレンダラー内にターミナルインスタンスを保持し、さらに `orca-terminal-host` にも状態を保持します。これはユーザーが `N` 個のタブを開いたときの実際のリソース使用量を測定したものです。エージェントのプロセス（`claude`、`codex` など）は、どのホストアプリでも同じため除外しています。

</details>

---

## 機能

<table>
<tr>
<td width="50%" valign="top">

### 1. アプリより長生きするセッション
`krill-app` を終了、クラッシュ、更新しても、エージェントはバックグラウンドで動き続けます。アプリを開き直す（または `krill attach <id>` を実行する）と、保持されたキーフレームと `zstd` スクロールバックセグメントを再生します。保持量を超えた場合は明示的な `Gap` マーカーが表示されます。

[アーキテクチャ §4.1 →](docs/ARCHITECTURE.md#41-장애별-보장-범위-규범)

</td>
<td width="50%" valign="top">

### 2. 並行 Git Worktree と Diff レビュー
サイドバーまたは `krill worktree new` からタスクごとに分離された Git worktree を作成し、リポジトリのセットアップスクリプトを実行前に確認し、5 つのレビュータブ（`Changes`、`Files`、`Search`、`Checks`、`History`）で変更を調べ、ターミナルのすぐ横でインライン diff コメントを付けられます。

[はじめに →](GETTING_STARTED.md)

</td>
</tr>
<tr>
<td width="50%" valign="top">

### 3. 待機中のエージェントを優先（`Needs You`）
組み込みのライフサイクルフック（`krill hook install`）が `working`、`waiting`、`done`、`blocked`、`idle`、`exited` の 6 つの状態を追跡し、入力が必要なエージェントを **Needs You** キューの先頭、macOS 通知、Dock バッジで知らせます。

[CLI フックリファレンス →](https://krill.page/en/docs#c-agents)

</td>
<td width="50%" valign="top">

### 4. マルチエージェントのタスク DAG と判断ゲート
コーディネーターエージェントに依存関係のあるタスクを分解させ（`krill orchestration run-create`、`task-create`）、分離された worktree でワーカーエージェントを起動し（`worker-start`）、ブロッキングの `ask`/`reply` メッセージをやり取りし、人の判断ゲートで一時停止できます（`gate-create`）。

[オーケストレーションのドキュメント →](https://krill.page/en/docs#c-orch)

</td>
</tr>
<tr>
<td width="50%" valign="top">

### 5. モバイル E2EE コンパニオン（PWA）
**Settings → Mobile** で QR コードをスキャンすると、スマートフォンのブラウザまたはホーム画面の Web アプリ（PWA）からエージェントの状態をリアルタイムで確認し、ターミナルを開けます。Relay または Tailscale 経由で接続し、エンドツーエンドの **ChaCha20-Poly1305**（`HKDF-SHA256`）で暗号化します。

ホスト型 Relay を選ぶ場合は、デスクトップで Google にサインインします。スマートフォンではサインインせずに PWA を開き、QR ペアリングまたは保存済みのデバイスキーで認証します。

[モバイルのセットアップ →](https://krill.page/en/docs)

</td>
<td width="50%" valign="top">

### 6. ワンクリックのサンドボックス Artifact（`share.krill.page`）
エージェントが生成した HTML レポート、アーキテクチャ図、Markdown ドキュメントを、サンドボックス化された 1 つの公開リンクとして公開できます（有効期限は設定可能で、無期限も選択可。上限 10 MiB）。同じ URL は `krill artifacts share` でいつでも更新、取り消しできます。

[Artifact ガイド →](docs/ARTIFACTS.md)

</td>
</tr>
</table>

**その他の機能:**

- **ヘッドレス `krill` CLI とリモート SSH ホスト**: GUI なしですべてのセッション、worktree、オーケストレーション実行を操作でき（`krill ls`、`new`、`send`、`read`、`wait`、`attach`、`handoff`）、対応するリモート Mac ホストにもまたがって操作できます（`krill --host user@host`）。
- **アカウント切り替えと自動切り替え（`krill account`）**: エージェントのアカウントを切り替えたり、使用量のレート制限に達したときに自動でローテーションしたりできます。
- **エージェントのスケジュール実行（`krill schedule`）**: UTC の cron スケジュールで定期的なエージェントタスクを実行します（[スケジュールのドキュメント](docs/SCHEDULING.md)）。
- **45 種類のターミナルテーマと本格的な韓国語 IME 対応**: **Krill Deep** を含む 15 種類の Krill テーマと、30 種類の Orca 互換組み込みテーマを同梱し（Ghostty/Warp テーマのインポートも可能）、ハングル IME 専用の変換ステートマシンを備えています。
- **クイックオープンとワークスペースコンテキスト**: worktree、セッション、ファイル、検出されたローカルサーバーのポート（`krill ports`）、添付した参照 URL（`krill reference`）を横断して検索できます。

---

## 対応エージェント

**あらゆる CLI エージェントや TUI** で動作します。ターミナルで動くものなら、Krill の永続的な PTY 保護を受けられます。以下には組み込みのプロファイルとステータスフック（`krill hook install`）を用意しています。

<p>
  <kbd>Claude Code</kbd> &nbsp;
  <kbd>OpenAI Codex</kbd> &nbsp;
  <kbd>Antigravity (agy)</kbd> &nbsp;
  <kbd>Cursor CLI</kbd> &nbsp;
  <kbd>OpenCode</kbd> &nbsp;
  <kbd>+ 任意のシェル、開発サーバー、TUI</kbd>
</p>

---

## リリース版のインストール

バイナリのインストールに Rust やソースビルドは**不要**です。使用する AI エージェントは別途インストールして認証してください。Git worktree には Git が必要です。Krill にエージェントのサブスクリプションや認証情報は含まれません。

使用状況メトリクスは同意ダイアログなしでデフォルトで有効です。収集するのは、ランダムなインストール ID、初回起動、アプリ起動、日次の操作、バージョン、OS、アーキテクチャです。プロンプト、ターミナル出力、コマンド、プロジェクトパス、アカウント情報は含まれません。収集を止めるには `krill telemetry disable` を実行します。`DO_NOT_TRACK=1` でも無効になります。送信状況と運用者向けの設定は[使用状況メトリクス](services/telemetry/CLIENT.md)を参照してください。エラー報告は別の設定で、デフォルトでオフです。

### macOS（Apple Silicon `arm64`）

インストーラーには Bash、`curl`、Python 3、macOS システムのアーカイブおよび署名ツールが必要です。`/Applications/Krill.app` をインストールし、CLI を `~/.local/bin/krill` にリンクして、アプリを開きます。

```sh
curl -fsSL https://krill.page/install.sh -o /tmp/krill-install.sh
# Review the installer before running it.
bash /tmp/krill-install.sh
export PATH="$HOME/.local/bin:$PATH"
krill --help
```

macOS リリースは**公証されていません**。インストーラーはアーカイブのチェックサムとアプリの署名を検証し、ダウンロードしたアプリの隔離属性を削除します。有効なローカル/ad-hoc 署名は Apple Developer ID の承認を意味しません。詳しくは[パッケージングの詳細](packaging/README.md)（韓国語）を参照してください。

### Windows（`x64` プレビュー）

PowerShell でインストーラーをダウンロードして内容を確認してから、実行します。

```powershell
Invoke-WebRequest https://krill.page/install.ps1 -OutFile "$env:TEMP\krill-install.ps1"
Get-Content "$env:TEMP\krill-install.ps1"
powershell -ExecutionPolicy Bypass -File "$env:TEMP\krill-install.ps1"
```

インストーラーは ZIP の SHA-256 を検証し、`%LOCALAPPDATA%\Programs\Krill` にインストールして、スタートメニューのショートカットとユーザーの `PATH` エントリを追加し、アプリを開きます。更新された `PATH` を使うために新しいターミナルを開き、`krill --help` を実行してください。Windows Authenticode 署名はまだ完了していません。チェックサムや更新署名の検証は、アプリに信頼された発行元の署名があることを意味しません。

[リリースアセット](https://github.com/craftowen/krill/releases/latest) · [Windows パッケージング](packaging/windows/README.md)（韓国語）。選択したリリースに添付されたアセットを確認してください。ソースディレクトリからダウンロード対象を推測しないでください。Homebrew ツールは現在個人用 tap を組み立てるもので、公式の Homebrew インストールコマンドを提供するものではありません。

### プラットフォーム対応表

| プラットフォーム | 現在の範囲 | インストール |
|---|---|---|
| **macOS、Apple Silicon（`arm64`）** | 主要なデスクトッププラットフォーム | リリース版のアプリと CLI、またはソースビルド |
| **Windows、`x64`** | ネイティブデスクトップのプレビュー。デスクトップ GUI、対話型ログオン、スリープ、再起動の動作は、記録済みの Windows Server VM テスト以外の検証がまだ必要 | Windows リリース ZIP と PowerShell インストーラー |
| **モバイル（iOS / Android）** | QR コードでペアリングするコンパニオン Web アプリ（PWA）、Relay または Tailscale 経由（ChaCha20-Poly1305 E2EE） | Mac アプリの **Settings → Mobile** から QR をスキャン（アプリストア版なし） |
| **macOS、Intel（`x86_64`）** | このパブリックプレビューでは非対応 | 対応するリリースバンドルもソースインストールの保証もなし |
| **Linux** | このパブリックプレビューでは非対応 | Linux デスクトップやダウンロード可能なリモートホストパッケージの提供は保証なし |
| **Windows 11、ARM64** | Windows 11 の x64 エミュレーションで `x64` パッケージを実行。ネイティブ ARM64 ビルドなし | 同じ Windows リリース ZIP と PowerShell インストーラー |
| **Windows 10 on ARM / 32 ビット Windows** | 非対応 | リリースパッケージなし |

---

## 最初のセッションを成功させる

1. Krill を開き、**Open folder…** でローカルの Git リポジトリを選択します。UI が韓国語の場合は、**설정 → 모양 → 언어 → English**（Settings → Appearance → Language）を選択します。
2. **New session**（`+`）メニューを開いて **Terminal** を選ぶか、Claude Code や Codex などのインストールおよび認証済みのエージェントを選びます。エージェントにリポジトリの調査を依頼します。
3. サイドバーの **New worktree** を使い、一意の名前を入力して **Create** を選びます。承認する前にセットアップスクリプトを確認するか、**Skip setup** を選びます。そこで 2 つ目のエージェントを起動し、独立して作業させます。
4. `krill ls` でセッション ID を記録します。セッションタブをそのままにしてアプリを終了し、開き直します。実行中のセッションが同じ ID で再接続されることを確認してください。セッションのタブやペインを明示的に閉じると、そのセッションは終了します。

コピーして使える GUI、CLI、ソースビルドの手順は[はじめに](GETTING_STARTED.md)を参照してください。

---

## 障害時に何が残るか

| イベント | 実行中のエージェントプロセス | 出力と復旧 |
|---|---|---|
| **アプリの終了、クラッシュ、再起動** | セッションサービスが動いている間は継続 | 同じセッションに再接続。保持される履歴には上限があり、超過すると `Gap` が発生することがある |
| **`krilld` のクラッシュ** | holder が PTY とハンドルを保持しているため継続可能 | 読み取り済みで未記録のバイトは失われることがある。復旧時に `Gap` を記録 |
| **両方のセッションサービスの停止、またはマシンの再起動** | 存続しない | 保存された履歴は実行中のプロセスではない。エージェントの会話の再開は別の操作 |

これはアプリの再起動をまたぐ継続性であり、あらゆる形態のデータ損失に対する保証ではありません。プラットフォーム別の復旧と検証の制限は[アーキテクチャ §4.1](docs/ARCHITECTURE.md#41-장애별-보장-범위-규범)に定義されています。

---

## ビルドとコントリビュート

ソースビルドには Rust **1.98 以降**（MSRV は [Cargo.toml](Cargo.toml) を参照。Homebrew の `rustc` を推奨、`rustup` は不要）、Git、Apple Silicon 上の macOS Command Line Tools が必要です。リリースの組み立てスクリプトには Python 3.9+ が必要です。ローカルでソースから実行するだけなら、完全な Xcode と Apple Developer メンバーシップは不要です。[ソースからの手順](GETTING_STARTED.md#build-from-source-on-macos)に従ってください。

[コントリビュートガイド](CONTRIBUTING.md)では変更範囲ごとのチェックを説明しています。Markdown のみの変更は `git diff --check` を使い、完全な `scripts/check.sh` はリリース候補のゲートです。[セキュリティポリシーと非公開の報告](SECURITY.md) · [行動規範](CODE_OF_CONDUCT.md)。

設計ドキュメントは現在韓国語のみです: [アーキテクチャ](docs/ARCHITECTURE.md)、[ロードマップ](docs/ROADMAP.md)、[コード規約](docs/CONVENTIONS.md)、[UI スタイルガイド](docs/STYLEGUIDE.md)。

---

## ライセンス

Apache-2.0: [LICENSE](LICENSE)、Copyright 2026 Krill contributors。[第三者に関する通知](THIRD_PARTY_NOTICES.md)と[ライセンスの記録](docs/licensing/README.md)を参照してください。コントリビュートの際は、Zed の GPL-3.0 `terminal` および `terminal_view` のコードを読んだりコピーしたりしないでください。許可された Orca ソースの再利用については、[コントリビュートガイド](CONTRIBUTING.md)の説明に従ってクレジットを記載してください。

使用状況メトリクスは同意ダイアログなしでデフォルトで有効です。収集するのは、ランダムなインストール ID、初回起動、アプリ起動、日次の操作、バージョン、OS、アーキテクチャです。プロンプト、ターミナル出力、コマンド、プロジェクトパス、アカウント情報は含まれません。収集を止めるには `krill telemetry disable` を実行します。`DO_NOT_TRACK=1` でも無効になります。送信状況と運用者向けの設定は[使用状況メトリクス](services/telemetry/CLIENT.md)を参照してください。エラー報告は別の設定で、デフォルトでオフです。
