# rally-rs
<!-- bdg:begin -->
[![crates.io](https://img.shields.io/crates/v/rally-rs.svg)](https://crates.io/crates/rally-rs)
[![license](https://img.shields.io/github/license/fujibee/rally-rs.svg)](https://github.com/fujibee/rally-rs)
[![CI](https://github.com/fujibee/rally-rs/actions/workflows/publish.yaml/badge.svg)](https://github.com/fujibee/rally-rs/actions/workflows/publish.yaml)
<!-- bdg:end -->

CLI で動く AI エージェント同士に小さなメッセージボックスを渡すためのツールです。
[`fujibee/agmsg`](https://github.com/fujibee/agmsg) を Rust に書き直したもので、
クレート名は `rally-rs`、コマンド名は `ral` です。

常駐デーモンもネットワークサービスも持ちません。SQLite のファイル 1 つと、
チームごとの JSON 設定ファイルを共有ディレクトリに置くだけで、Claude Code や
Codex のような別々のエージェントがそこを読み書きしてやり取りします。

## 何をするのか

- エージェントを「チーム」に所属させる(`ral join`)
- 同じチームの別エージェントにメッセージを送る(`ral send`)
- 自分宛ての未読を読む(`ral inbox`)
- 過去のやり取りを見る(`ral history`)

これだけです。メッセージの中身を解釈したり、自動で何かを実行したりはしません。
読むタイミングは、エージェント側のフック(後述)か、手動で `inbox` を叩く
ことで決まります。

## データの置き場所

`--home` の指定、環境変数 `RALLY_HOME`、`ral install` 済みなら
`~/.agents/skills/ral`、それ以外は `~/.config/rally-rs` の順で決まります。

中身は次の 2 つだけです。

- `db/messages.db` — メッセージ本体(SQLite)
- `teams/<team>/config.json` — チームメンバーの登録情報

## インストール

バイナリをビルドします。

```bash
cargo install --path .
```

エージェント用のスキルとラッパースクリプトを置きます。

```bash
ral install
```

これで以下が作られます。

- `~/.agents/skills/ral/SKILL.md`
- `~/.agents/skills/ral/scripts/*.sh`
- `~/.agents/skills/ral/db/messages.db`
- `~/.agents/skills/ral/teams/`
- `~/.claude` がある場合は `~/.claude/commands/ral.md`

ラッパーは `--home ~/.agents/skills/ral` を付けて Rust バイナリを呼びます。
Claude Code と Codex から同じ DB が見える、という以上の仕掛けはありません。

## 直接 CLI を使う場合

```bash
ral join myteam alice codex /path/to/project-a
ral join myteam bob   claude-code /path/to/project-b
ral send  myteam alice bob "hello"
ral inbox myteam bob
ral history myteam
ral team myteam
ral whoami /path/to/project-a codex
```

そのほかに以下があります。

- `ral identities <project> <agent_type>` — 同じプロジェクトに複数チームで
  登録している場合の一覧
- `ral reset <project> <agent_type> [agent]` — 登録の解除
- `ral leave <team> <agent>` — チームから完全に抜ける
- `ral rename <team> <old> <new>` / `ral rename-team <old> <new>` —
  メッセージを引き継いだ上で改名
- `ral config get/set/show` — 簡易設定
- `ral init-db` — DB の手動初期化(通常は不要)

## エージェントから使う場合

`ral install` 後にエージェントを起動し直すと、スキルとコマンドが読み込まれます。

Claude Code:

```text
/ral
/ral send bob README を見て
/ral mode monitor
```

Codex:

```text
$ral
$ral send alice レビューお願いします
$ral mode turn
```

OpenCode:

```text
/ral
/ral send alice レビューお願いします
/ral mode turn
```

### 配送モード

`ral delivery set <mode> <agent_type> <project>` で切り替えます。

- `monitor` — Claude Code の SessionStart フックで Monitor ツールに
  ストリームさせる。受信したら会話の途中でも届く。
- `turn` — Stop フックでターンの合間に inbox を確認する。
- `both` — monitor を主にしつつ turn でも保険を取る。
- `off` — 自動チェックなし。

OpenCode は Claude Code の JSON hook ではなく `.opencode/plugins/ral.js` を生成して
`session.idle` などのイベントで inbox を確認します。Codex には Monitor 相当の口が
無いので `turn` か `off` を使ってください。
`ral hook on/off` は旧名で、`delivery set turn/off` の別名として残してあります。

## メンテナンス

```bash
ral install --update             # スクリプトを最新に上書き
ral uninstall --yes --keep-data  # スクリプトとスキルだけ消して DB は残す
ral uninstall --yes              # 全部消す
```

## 実装状況

現時点で動くもの:

- 上に書いた CLI サブコマンド一式
- Claude Code 向けの SessionStart / Stop フックと Monitor ストリーム
  (`ral watch` / `ral session-start` / `ral session-end` / `ral check-inbox`)
- Codex 向けの turn モード
- OpenCode 向けの plugin ベース配送(`opencode` agent type)
- `ral install` でのスキル・ラッパー・コマンド配置と `--update` 更新

まだ無い、あるいは限定的なもの:

- ネットワーク越しの同期(共有ディレクトリ前提)
- 添付ファイルや構造化ペイロード(本文は単一の文字列のみ)
- 認証やアクセス制御(同じ DB ファイルが見える全員が読み書き可能)
- Claude Code、Codex、OpenCode、Gemini CLI、Antigravity 以外の
  エージェント種別への正式対応

統合テストは `tests/cli.rs` にあります。`cargo test` で実行できます。

## 元にしたもの

このツールは [`fujibee/agmsg`](https://github.com/fujibee/agmsg) の考え方と
コマンド体系を参考にしています。`agmsg` は Bash スクリプトで実装されていますが、
`rally-rs` では同じような使い勝手を Rust の単一バイナリで扱えるようにしています。

## ライセンス

MIT。
