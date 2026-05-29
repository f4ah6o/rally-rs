use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::io::Write;
use std::process::{Command as StdCommand, Stdio};
use std::thread;
use std::time::Duration;
use tempfile::TempDir;

fn ral(home: &TempDir) -> Command {
    let mut cmd = Command::cargo_bin("ral").unwrap();
    cmd.env("RALLY_HOME", home.path());
    cmd
}

fn ral_with_home(home: &TempDir) -> Command {
    let mut cmd = Command::cargo_bin("ral").unwrap();
    cmd.env("HOME", home.path());
    cmd.env_remove("RALLY_HOME");
    cmd
}

#[test]
fn send_inbox_and_history() {
    let home = TempDir::new().unwrap();

    ral(&home)
        .args(["join", "testteam", "alice", "claude-code", "/tmp/project-a"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Joined team testteam as alice"));

    ral(&home)
        .args(["join", "testteam", "bob", "codex", "/tmp/project-b"])
        .assert()
        .success();

    ral(&home)
        .args(["send", "testteam", "alice", "bob", "hello bob"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Sent to bob"));

    ral(&home)
        .args(["inbox", "testteam", "bob"])
        .assert()
        .success()
        .stdout(predicate::str::contains("hello bob"))
        .stdout(predicate::str::contains("alice"));

    ral(&home)
        .args(["inbox", "testteam", "bob"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No new messages"));

    ral(&home)
        .args(["history", "testteam"])
        .assert()
        .success()
        .stdout(predicate::str::contains("alice"))
        .stdout(predicate::str::contains("bob"));
}

#[test]
fn team_identity_and_reset() {
    let home = TempDir::new().unwrap();

    ral(&home)
        .args(["join", "myteam", "alice", "claude-code", "/tmp/proj-a"])
        .assert()
        .success();
    ral(&home)
        .args(["join", "myteam", "alice", "claude-code", "/tmp/proj-b"])
        .assert()
        .success();

    ral(&home)
        .args(["team", "myteam"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 member"))
        .stdout(predicate::str::contains("+1 more"));

    ral(&home)
        .args(["whoami", "/tmp/proj-b", "claude-code"])
        .assert()
        .success()
        .stdout(predicate::str::contains("agent=alice"))
        .stdout(predicate::str::contains("teams=myteam"));

    ral(&home)
        .args(["reset", "/tmp/proj-a", "claude-code", "alice"])
        .assert()
        .success()
        .stdout(predicate::str::contains("removed 1 registration"));

    ral(&home)
        .args(["whoami", "/tmp/proj-b", "claude-code"])
        .assert()
        .success()
        .stdout(predicate::str::contains("agent=alice"));
}

#[test]
fn rename_team_migrates_messages() {
    let home = TempDir::new().unwrap();

    ral(&home)
        .args(["join", "oldteam", "alice", "codex", "/tmp/a"])
        .assert()
        .success();
    ral(&home)
        .args(["join", "oldteam", "bob", "codex", "/tmp/b"])
        .assert()
        .success();
    ral(&home)
        .args(["send", "oldteam", "alice", "bob", "hello"])
        .assert()
        .success();
    ral(&home)
        .args(["rename-team", "oldteam", "newteam"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Renamed team oldteam"));
    ral(&home)
        .args(["inbox", "newteam", "bob"])
        .assert()
        .success()
        .stdout(predicate::str::contains("hello"));
}

#[test]
fn rename_agent_updates_team_and_messages() {
    let home = TempDir::new().unwrap();
    ral(&home)
        .args(["join", "team", "alice", "codex", "/tmp/a"])
        .assert()
        .success();
    ral(&home)
        .args(["join", "team", "bob", "codex", "/tmp/b"])
        .assert()
        .success();
    ral(&home)
        .args(["send", "team", "alice", "bob", "hello"])
        .assert()
        .success();
    ral(&home)
        .args(["rename", "team", "bob", "robert"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Renamed bob"));
    ral(&home)
        .args(["team", "team"])
        .assert()
        .success()
        .stdout(predicate::str::contains("robert"))
        .stdout(predicate::str::contains("bob").not());
    ral(&home)
        .args(["inbox", "team", "robert"])
        .assert()
        .success()
        .stdout(predicate::str::contains("hello"));
}

#[test]
fn delivery_modes_are_idempotent_and_status_derives_mode() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let project_path = project.path().to_str().unwrap();

    ral(&home)
        .args(["delivery", "set", "both", "claude-code", project_path])
        .assert()
        .success()
        .stdout(predicate::str::contains("Delivery mode set to 'both'"))
        .stdout(predicate::str::contains("AGMSG-DIRECTIVE"));
    ral(&home)
        .args(["delivery", "set", "both", "claude-code", project_path])
        .assert()
        .success();

    let settings = fs::read_to_string(project.path().join(".claude/settings.local.json")).unwrap();
    assert_eq!(settings.matches("session-start.sh").count(), 1);
    assert_eq!(settings.matches("check-inbox.sh").count(), 1);

    ral(&home)
        .args(["delivery", "status", "claude-code", project_path])
        .assert()
        .success()
        .stdout(predicate::str::contains("mode: both"));

    ral(&home)
        .args(["delivery", "set", "turn", "claude-code", project_path])
        .assert()
        .success()
        .stdout(predicate::str::contains("TaskStop"));
    let settings = fs::read_to_string(project.path().join(".claude/settings.local.json")).unwrap();
    assert!(!settings.contains("session-start.sh"));
    assert!(settings.contains("check-inbox.sh"));
}

#[test]
fn hook_aliases_delivery_turn_and_off() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let project_path = project.path().to_str().unwrap();

    ral(&home)
        .args(["hook", "on", "codex", project_path])
        .assert()
        .success()
        .stdout(predicate::str::contains("Delivery mode set to 'turn'"));
    assert!(project.path().join(".codex/hooks.json").exists());

    ral(&home)
        .args(["hook", "off", "codex", project_path])
        .assert()
        .success()
        .stdout(predicate::str::contains("Delivery mode set to 'off'"));
    let hooks = fs::read_to_string(project.path().join(".codex/hooks.json")).unwrap();
    assert!(!hooks.contains("check-inbox.sh"));
}

#[test]
fn check_inbox_handles_codex_json_cooldown_and_unread() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let project_path = project.path().to_str().unwrap();

    ral(&home)
        .args(["join", "team", "alice", "codex", project_path])
        .assert()
        .success();
    ral(&home)
        .args(["send", "team", "bob", "alice", "hello"])
        .assert()
        .success();

    let mut cmd = ral(&home);
    cmd.args(["check-inbox", "codex", project_path])
        .write_stdin("{}")
        .assert()
        .success()
        .stdout(predicate::str::contains("\"decision\":\"block\""))
        .stdout(predicate::str::contains("hello"));

    let mut cmd = ral(&home);
    cmd.args(["check-inbox", "codex", project_path])
        .write_stdin("{}")
        .assert()
        .success()
        .stdout(predicate::str::contains("cooldown"));

    let mut cmd = ral(&home);
    cmd.args(["check-inbox", "codex", project_path])
        .write_stdin(r#"{"stop_hook_active":true}"#)
        .assert()
        .success()
        .stdout("");
}

#[test]
fn session_start_only_emits_directive_when_joined() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let project_path = project.path().to_str().unwrap();

    let mut cmd = ral(&home);
    cmd.args(["session-start", "claude-code", project_path])
        .write_stdin(r#"{"session_id":"s1"}"#)
        .assert()
        .success()
        .stdout("");

    ral(&home)
        .args(["join", "team", "alice", "claude-code", project_path])
        .assert()
        .success();
    let mut cmd = ral(&home);
    cmd.args(["session-start", "claude-code", project_path])
        .write_stdin(r#"{"session_id":"s1"}"#)
        .assert()
        .success()
        .stdout(predicate::str::contains("AGMSG-DIRECTIVE"))
        .stdout(predicate::str::contains("watch.sh s1"));
}

#[test]
fn install_and_uninstall_write_skill_assets() {
    let fake_home = TempDir::new().unwrap();
    ral_with_home(&fake_home)
        .args(["install", "--cmd", "ral"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Installed"));

    let skill = fake_home.path().join(".agents/skills/ral");
    assert!(skill.join(".rally-rs").exists());
    assert!(skill.join("SKILL.md").exists());
    assert!(skill.join("scripts/send.sh").exists());
    assert!(skill.join("db/messages.db").exists());

    ral_with_home(&fake_home)
        .args(["uninstall", "--yes", "--keep-data"])
        .assert()
        .success()
        .stdout(predicate::str::contains("kept data"));
    assert!(skill.join("db/messages.db").exists());
    assert!(!skill.join("scripts/send.sh").exists());

    ral_with_home(&fake_home)
        .args(["install", "--cmd", "ral", "--update"])
        .assert()
        .success();
    ral_with_home(&fake_home)
        .args(["uninstall", "--yes"])
        .assert()
        .success();
    assert!(!skill.exists());
}

#[test]
fn skill_command_prints_bundled_skill() {
    let home = TempDir::new().unwrap();

    ral(&home)
        .arg("skill")
        .assert()
        .success()
        .stdout(predicate::str::contains("name: ral"))
        .stdout(predicate::str::contains("## Delivery Modes"))
        .stdout(predicate::str::contains(
            "~/.agents/skills/ral/scripts/whoami.sh",
        ))
        .stderr("");
}

#[test]
fn skills_alias_matches_skill_output() {
    let home = TempDir::new().unwrap();

    let skill = ral(&home).arg("skill").output().unwrap();
    let skills = ral(&home).arg("skills").output().unwrap();

    assert!(skill.status.success());
    assert!(skills.status.success());
    assert_eq!(skill.stdout, skills.stdout);
    assert!(skill.stderr.is_empty());
    assert!(skills.stderr.is_empty());
}

#[test]
fn skill_command_renders_custom_command_name() {
    let home = TempDir::new().unwrap();

    ral(&home)
        .args(["skill", "--cmd", "teamchat"])
        .assert()
        .success()
        .stdout(predicate::str::contains("name: teamchat"))
        .stdout(predicate::str::contains(
            "~/.agents/skills/teamchat/scripts/whoami.sh",
        ))
        .stderr("");
}

#[test]
fn watch_streams_new_messages_and_cleans_pidfile() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let project_path = project.path().to_str().unwrap();
    let bin = assert_cmd::cargo::cargo_bin("ral");

    ral(&home)
        .args(["join", "team", "alice", "claude-code", project_path])
        .assert()
        .success();
    ral(&home)
        .args(["send", "team", "bob", "alice", "old"])
        .assert()
        .success();

    let child = StdCommand::new(&bin)
        .env("RALLY_HOME", home.path())
        .env("RAL_WATCH_INTERVAL", "1")
        .args(["watch", "watch-test", project_path, "claude-code"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    thread::sleep(Duration::from_millis(500));
    assert!(home.path().join("run/watch.watch-test.pid").exists());

    ral(&home)
        .args(["send", "team", "bob", "alice", "new"])
        .assert()
        .success();
    thread::sleep(Duration::from_millis(1500));

    #[cfg(unix)]
    unsafe {
        libc::kill(child.id() as i32, libc::SIGTERM);
    }
    let output = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains("old"));
    assert!(stdout.contains("new"));
    assert!(!home.path().join("run/watch.watch-test.pid").exists());
}

#[test]
fn session_end_stops_matching_watcher() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let project_path = project.path().to_str().unwrap();
    let bin = assert_cmd::cargo::cargo_bin("ral");

    ral(&home)
        .args(["join", "team", "alice", "claude-code", project_path])
        .assert()
        .success();
    let mut child = StdCommand::new(&bin)
        .env("RALLY_HOME", home.path())
        .env("RAL_WATCH_INTERVAL", "10")
        .args(["watch", "end-test", project_path, "claude-code"])
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    thread::sleep(Duration::from_millis(500));
    assert!(home.path().join("run/watch.end-test.pid").exists());

    let mut proc = StdCommand::new(&bin)
        .env("RALLY_HOME", home.path())
        .args(["session-end", "claude-code", project_path])
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    proc.stdin
        .as_mut()
        .unwrap()
        .write_all(br#"{"session_id":"end-test"}"#)
        .unwrap();
    assert!(proc.wait().unwrap().success());
    thread::sleep(Duration::from_millis(500));
    let _ = child.try_wait();
    assert!(!home.path().join("run/watch.end-test.pid").exists());
}
